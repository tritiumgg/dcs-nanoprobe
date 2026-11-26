//! `cargo xtask real-files`: run the sidecar's static-template and
//! payload-preset readers over real files, in memory.
//!
//! `shipped` streams every static template and payload preset file that a
//! DCS install ships through one SSH session and reads each one here. It
//! writes nothing on the target and keeps no file: the bytes live in memory
//! only for the length of the run. `profile` reads files a user saved in the
//! target's Saved Games profile the same way.
//!
//! Each file gets the reader the sidecar uses for its kind. A static template
//! (`.stm`) is read by `static_template::read` and converted in place by
//! `static_template::specs`, as `PutFromStaticTemplate` without an anchor
//! converts it. A payload preset file (`.lua`) is read by
//! `payload_preset::read`, and every preset name in it is matched with
//! `File::pylons` against the file's own type, as `PutGroup` matches a
//! `payload_preset`. The output is one Markdown table row per file: names and
//! counts, or the reader's refusal. No file content is printed.

use std::process::ExitCode;

use nanoprobe_api::{payload_preset, static_template};

use crate::check;
use crate::profile::Target;
use crate::ssh::{self, quote};

/// The folders of a DCS install that hold the files, as `find` names them
/// from the install folder: the editor's own and every module's
/// `UnitPayloads/` under `CoreMods/` and, when there is one, `Mods/`. A
/// missing folder of the first three fails the run: a later layout would
/// otherwise read fewer files and still pass.
const SHIPPED: &str = r#"set -u
cd "$1" || exit 3
for d in MissionEditor/data/scripts/StaticTemplate MissionEditor/data/scripts/UnitPayloads CoreMods; do
    [ -d "$d" ] || { echo "no folder $d in the install" >&2; exit 4; }
done
{
    find MissionEditor/data/scripts/StaticTemplate -maxdepth 1 -type f -name '*.stm'
    find MissionEditor/data/scripts/UnitPayloads -maxdepth 1 -type f -name '*.lua'
    find CoreMods -type f -path '*/UnitPayloads/*.lua'
    if [ -d Mods ]; then find Mods -type f -path '*/UnitPayloads/*.lua'; fi
} | LC_ALL=C sort | while IFS= read -r f; do
    printf '%s\n%s\n' "$f" "$(wc -c < "$f" | tr -d ' ')"
    cat -- "$f"
done
"#;

/// What one file is, by its name.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Template,
    Preset,
}

fn kind(path: &str) -> Option<Kind> {
    if path.ends_with(".stm") {
        Some(Kind::Template)
    } else if path.ends_with(".lua") {
        Some(Kind::Preset)
    } else {
        None
    }
}

/// Split the stream `SHIPPED` writes into its files: a path line, a size
/// line, then that many bytes, repeated.
fn frames(mut s: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    fn line<'a>(s: &mut &'a [u8]) -> Result<&'a str, String> {
        let end = s
            .iter()
            .position(|&b| b == b'\n')
            .ok_or("the stream ends inside a header line")?;
        let l = std::str::from_utf8(&s[..end]).map_err(|_| "a header line is not UTF-8")?;
        *s = &s[end + 1..];
        Ok(l)
    }
    let mut out = Vec::new();
    while !s.is_empty() {
        let path = line(&mut s)?.to_owned();
        let size: usize = line(&mut s)?
            .parse()
            .map_err(|_| format!("{path}: the size line is not a number"))?;
        if size > s.len() {
            return Err(format!("{path}: the stream ends inside the file"));
        }
        let (body, rest) = s.split_at(size);
        out.push((path, body));
        s = rest;
    }
    Ok(out)
}

/// One file's result.
#[derive(Debug, PartialEq)]
struct Row {
    path: String,
    bytes: usize,
    outcome: Outcome,
}

#[derive(Debug, PartialEq)]
enum Outcome {
    /// A static template read and converted.
    Template {
        groups: usize,
        units: usize,
        statics: usize,
        theatre: String,
    },
    /// A preset file read; `matched` of its `names` give pylons, with
    /// `empty` pylons left empty by `CLSID = ""` among them, and `refused`
    /// holds each other name with its refusal.
    Preset {
        unit_type: String,
        entries: usize,
        names: usize,
        matched: usize,
        empty: usize,
        refused: Vec<(String, String)>,
    },
    /// The reader or the conversion refused the file.
    Refused(String),
}

impl Row {
    /// Whether the reader took the file: a template converted, or a preset
    /// file read, whatever its presets gave.
    fn loaded(&self) -> bool {
        !matches!(self.outcome, Outcome::Refused(_))
    }

    /// Whether the reader took the file and every preset in it.
    fn passed(&self) -> bool {
        match &self.outcome {
            Outcome::Template { .. } => true,
            Outcome::Preset { refused, .. } => refused.is_empty(),
            Outcome::Refused(_) => false,
        }
    }
}

fn evaluate(path: &str, bytes: &[u8]) -> Row {
    let outcome = match kind(path) {
        Some(Kind::Template) => template(bytes),
        Some(Kind::Preset) => preset(bytes),
        None => Outcome::Refused("not a `.stm` or `.lua` file".into()),
    };
    Row {
        path: path.to_owned(),
        bytes: bytes.len(),
        outcome,
    }
}

fn template(bytes: &[u8]) -> Outcome {
    let l = match static_template::read(bytes) {
        Ok(l) => l,
        Err(r) => return Outcome::Refused(format!("read, {}: {}", r.field, r.text)),
    };
    match static_template::specs(&l, None, 0.0, None, "real-files") {
        Ok(s) => Outcome::Template {
            groups: s.groups.len(),
            units: s.groups.iter().map(|g| g.units.len()).sum(),
            statics: s.statics.len(),
            theatre: s.theater,
        },
        Err(r) => Outcome::Refused(format!("convert, {}: {}", r.field, r.text)),
    }
}

fn preset(bytes: &[u8]) -> Outcome {
    let f = match payload_preset::read(bytes.to_vec()) {
        Ok(f) => f,
        Err(r) => return Outcome::Refused(format!("read, {}: {}", r.field, r.text)),
    };
    let mut names = 0;
    let mut matched = 0;
    let mut empty = 0;
    let mut refused = Vec::new();
    for (name, _) in f.names() {
        names += 1;
        match f.pylons(&f.unit_type, name) {
            Ok(p) => {
                matched += 1;
                empty += p.empty.len();
            }
            Err(r) => refused.push((name.to_owned(), format!("{}: {}", r.field, r.text))),
        }
    }
    Outcome::Preset {
        unit_type: f.unit_type.clone(),
        entries: f.entries(),
        names,
        matched,
        empty,
        refused,
    }
}

/// A table cell: no `|` or line break, which would end the cell.
fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn print(rows: &[Row]) {
    let (t, p): (Vec<&Row>, Vec<&Row>) = rows
        .iter()
        .partition(|r| kind(&r.path) == Some(Kind::Template));
    if !t.is_empty() {
        println!("| Static template | Bytes | Result | Groups | Units | Statics | Theater |");
        println!("|---|---:|---|---:|---:|---:|---|");
        for r in t {
            match &r.outcome {
                Outcome::Template {
                    groups,
                    units,
                    statics,
                    theatre,
                } => println!(
                    "| `{}` | {} | loaded | {groups} | {units} | {statics} | {} |",
                    cell(&r.path),
                    r.bytes,
                    cell(theatre)
                ),
                other => println!(
                    "| `{}` | {} | refused: {} | | | | |",
                    cell(&r.path),
                    r.bytes,
                    cell(&refusal(other))
                ),
            }
        }
        println!();
    }
    if !p.is_empty() {
        println!(
            "| Preset file | Bytes | Result | Type | Entries | Names | Matched | Empty pylons |"
        );
        println!("|---|---:|---|---|---:|---:|---:|---:|");
        for r in p {
            match &r.outcome {
                Outcome::Preset {
                    unit_type,
                    entries,
                    names,
                    matched,
                    empty,
                    refused,
                } => {
                    let result = if refused.is_empty() {
                        "loaded".to_owned()
                    } else {
                        let list: Vec<String> = refused
                            .iter()
                            .map(|(n, why)| format!("{n:?}: {why}"))
                            .collect();
                        format!("loaded; refused presets: {}", list.join("; "))
                    };
                    println!(
                        "| `{}` | {} | {} | `{}` | {entries} | {names} | {matched} | {empty} |",
                        cell(&r.path),
                        r.bytes,
                        cell(&result),
                        cell(unit_type)
                    );
                }
                other => println!(
                    "| `{}` | {} | refused: {} | | | | | |",
                    cell(&r.path),
                    r.bytes,
                    cell(&refusal(other))
                ),
            }
        }
        println!();
    }
    println!("{}", footer(rows));
}

/// The summary line: how many files of each kind, how many the reader took,
/// and of those how many refuse a preset.
fn footer(rows: &[Row]) -> String {
    let count = |k: Kind, f: &dyn Fn(&Row) -> bool| {
        rows.iter()
            .filter(|r| kind(&r.path) == Some(k) && f(r))
            .count()
    };
    let all = |_: &Row| true;
    format!(
        "files {}: static templates {} loaded of {}; preset files {} loaded of {}, {} of them with a refused preset",
        rows.len(),
        count(Kind::Template, &Row::loaded),
        count(Kind::Template, &all),
        count(Kind::Preset, &Row::loaded),
        count(Kind::Preset, &all),
        count(Kind::Preset, &|r: &Row| r.loaded() && !r.passed()),
    )
}

fn refusal(o: &Outcome) -> String {
    match o {
        Outcome::Refused(why) => why.clone(),
        _ => String::new(),
    }
}

const USAGE: &str = "\
usage: cargo xtask real-files <name> shipped
       cargo xtask real-files <name> profile <path>...";

/// Where the Mission Editor saves a user's files under the profile
/// (ED-CODE, docs/results/2026-11-24-real-files.md), with the extension of
/// each kind.
const SAVED: [(&str, &str); 2] = [
    ("StaticTemplate/", ".stm"),
    ("MissionEditor/UnitPayloads/", ".lua"),
];

/// Check a path under the profile that a user names: a file directly in
/// one of the two folders the editor saves to, with that folder's
/// extension, and no other component. Nothing else in the profile, such as
/// its server settings, is read, and no folder outside the two is listed.
pub fn saved_path(p: &str) -> Result<(), String> {
    let ok = SAVED.iter().any(|(dir, ext)| {
        p.strip_prefix(dir).is_some_and(|f| {
            f.ends_with(ext)
                && f.len() > ext.len()
                && !f.contains(['/', '\\', ':'])
                && f != "."
                && f != ".."
        })
    });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{p:?}: name a file directly in the profile's StaticTemplate/ (.stm) or \
             MissionEditor/UnitPayloads/ (.lua), with `/`"
        ))
    }
}

pub fn command(name: &str, rest: &[&str]) -> Result<ExitCode, String> {
    let rows = match rest {
        ["shipped"] => shipped(&crate::target(name)?)?,
        ["profile", paths @ ..] if !paths.is_empty() => {
            for p in paths {
                saved_path(p)?;
            }
            let profile = crate::open(name)?;
            let mut rows = Vec::new();
            for p in paths {
                let bytes = profile
                    .read(p)?
                    .ok_or_else(|| format!("no file {p} in the profile"))?;
                rows.push(evaluate(p, &bytes));
            }
            rows
        }
        _ => return Err(USAGE.into()),
    };
    print(&rows);
    Ok(if rows.iter().all(Row::passed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn shipped(target: &Target) -> Result<Vec<Row>, String> {
    let Target::DockerWine { ssh, container, .. } = target else {
        return Err("`shipped` reads a docker-wine target's install".into());
    };
    let report = check::run(target)?;
    let install = match report.installs.as_slice() {
        [one] if one.version.is_some() => one,
        _ => return Err(format!("not exactly one DCS install found:\n{report}")),
    };
    eprintln!(
        "dcs {} ({}), {}",
        install.version.as_deref().unwrap_or("?"),
        install.branch.as_deref().unwrap_or("unknown branch"),
        report.wine.as_deref().unwrap_or("wine unknown")
    );
    let remote = format!(
        "docker exec -i {} sh -s -- {}",
        quote(container),
        quote(&install.path)
    );
    let out = ssh::run(ssh, &remote, SHIPPED.as_bytes())?;
    if out.code != Some(0) || !out.stderr.is_empty() {
        return Err(format!(
            "reading the install failed ({:?}): {}",
            out.code, out.stderr
        ));
    }
    let rows: Vec<Row> = frames(&out.stdout)?
        .into_iter()
        .map(|(p, b)| evaluate(&p, b))
        .collect();
    both_kinds(&rows)?;
    Ok(rows)
}

/// An install's stream holds at least one file of each kind; an empty or
/// one-sided stream says the search missed, not that every file passed.
fn both_kinds(rows: &[Row]) -> Result<(), String> {
    for (k, what) in [
        (Kind::Template, "static template"),
        (Kind::Preset, "preset file"),
    ] {
        if !rows.iter().any(|r| kind(&r.path) == Some(k)) {
            return Err(format!("the install gave no {what}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STM: &[u8] = br#"staticTemplate =
{
    ["theatre"] = "Caucasus",
    ["coalition"] =
    {
        ["blue"] =
        {
            ["name"] = "blue",
            ["country"] =
            {
                [1] =
                {
                    ["id"] = 2,
                    ["name"] = "USA",
                    ["vehicle"] =
                    {
                        ["group"] =
                        {
                            [1] =
                            {
                                ["name"] = "G",
                                ["x"] = 10,
                                ["y"] = 20,
                                ["units"] =
                                {
                                    [1] = {["type"] = "M-1 Abrams", ["name"] = "U1", ["x"] = 10, ["y"] = 20, ["heading"] = 0, ["skill"] = "Average"},
                                    [2] = {["type"] = "M-1 Abrams", ["name"] = "U2", ["x"] = 15, ["y"] = 20, ["heading"] = 0, ["skill"] = "Average"},
                                },
                            },
                        },
                    },
                },
            },
        },
    },
} -- end of staticTemplate
"#;

    const PRESET: &[u8] = br#"local unitPayloads = {
    ["name"] = "F-16C_50",
    ["payloads"] = {
        [1] = {
            ["name"] = "A",
            ["pylons"] = {
                [1] = {["CLSID"] = "{X}", ["num"] = 1},
            },
            ["tasks"] = {[1] = 11},
        },
        [2] = {
            ["name"] = "B",
            ["pylons"] = {
                [1] = {["CLSID"] = "{Y}"},
            },
        },
    },
    ["unitType"] = "F-16C_50",
}
return unitPayloads
"#;

    #[test]
    fn frames_split_the_stream() {
        let s = b"a.stm\n3\nabcb c.lua\n0\n";
        let f = frames(s).unwrap();
        assert_eq!(
            f,
            vec![("a.stm".into(), &b"abc"[..]), ("b c.lua".into(), &b""[..])]
        );
    }

    #[test]
    fn a_short_stream_is_an_error() {
        assert!(frames(b"a.stm\n9\nabc").is_err());
        assert!(frames(b"a.stm\n").is_err());
        assert!(frames(b"a.stm\nx\n").is_err());
    }

    #[test]
    fn a_template_is_read_and_counted() {
        let r = evaluate("T/a.stm", STM);
        assert_eq!(
            r.outcome,
            Outcome::Template {
                groups: 1,
                units: 2,
                statics: 0,
                theatre: "Caucasus".into()
            }
        );
        assert!(r.passed());
    }

    #[test]
    fn a_refused_template_keeps_the_readers_text() {
        let r = evaluate("T/b.stm", b"other = {}");
        match &r.outcome {
            Outcome::Refused(why) => assert!(why.starts_with("read, path: "), "{why}"),
            o => panic!("{o:?}"),
        }
        assert!(!r.passed());
    }

    #[test]
    fn every_preset_name_is_matched() {
        let r = evaluate("U/F-16C_50.lua", PRESET);
        match &r.outcome {
            Outcome::Preset {
                unit_type,
                entries,
                names,
                matched,
                empty,
                refused,
            } => {
                assert_eq!(unit_type, "F-16C_50");
                assert_eq!((*entries, *names, *matched, *empty), (2, 2, 1, 0));
                assert_eq!(refused.len(), 1);
                assert_eq!(refused[0].0, "B");
                assert!(refused[0].1.contains("`num`"), "{}", refused[0].1);
            }
            o => panic!("{o:?}"),
        }
        assert!(!r.passed());
    }

    #[test]
    fn a_code_preset_file_is_refused() {
        let r = evaluate("U/x.lua", b"local a = 1\nlocal b = {}\nreturn b\n");
        assert!(matches!(r.outcome, Outcome::Refused(_)));
    }

    #[test]
    fn an_empty_or_one_sided_stream_fails() {
        assert!(both_kinds(&[]).is_err());
        let rows: Vec<Row> = frames(b"")
            .unwrap()
            .into_iter()
            .map(|(p, b)| evaluate(&p, b))
            .collect();
        assert!(both_kinds(&rows).is_err());
        let t = evaluate("T/a.stm", STM);
        assert!(both_kinds(std::slice::from_ref(&t)).is_err());
        let p = evaluate("U/F-16C_50.lua", PRESET);
        assert!(both_kinds(&[t, p]).is_ok());
    }

    #[test]
    fn only_files_the_editor_saves_are_named() {
        for ok in [
            "StaticTemplate/test 1.stm",
            "MissionEditor/UnitPayloads/UH-1H.lua",
        ] {
            assert!(saved_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "Config/serverSettings.lua",
            "Config/network.vault",
            "StaticTemplate/x.lua",
            "MissionEditor/UnitPayloads/x.stm",
            "StaticTemplate/.stm",
            "StaticTemplate/../Config/x.stm",
            "StaticTemplate/sub/x.stm",
            "StaticTemplate\\x.stm",
            "/StaticTemplate/x.stm",
            "C:/StaticTemplate/x.stm",
            "StaticTemplate/C:x.stm",
            "x.stm",
            "",
        ] {
            assert!(saved_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_footer_counts_loaded_files_and_refused_presets() {
        let rows = [
            evaluate("T/a.stm", STM),
            evaluate("T/b.stm", b"other = {}"),
            evaluate("U/F-16C_50.lua", PRESET),
            evaluate("U/x.lua", b"local a = 1\nreturn a\n"),
        ];
        assert_eq!(
            footer(&rows),
            "files 4: static templates 1 loaded of 2; preset files 1 loaded of 2, 1 of them with a refused preset"
        );
    }

    #[test]
    fn empty_pylons_are_counted() {
        let src = String::from_utf8(PRESET.to_vec()).unwrap().replace(
            "[\"CLSID\"] = \"{X}\", [\"num\"] = 1",
            "[\"CLSID\"] = \"\", [\"num\"] = 1",
        );
        match evaluate("U/F-16C_50.lua", src.as_bytes()).outcome {
            Outcome::Preset { matched, empty, .. } => assert_eq!((matched, empty), (1, 1)),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn cells_hold_no_bar_or_break() {
        assert_eq!(cell("a|b\nc"), "a\\|b c");
    }
}
