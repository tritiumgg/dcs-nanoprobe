//! Briefing text, briefing images and files written into a shell
//! (spec/persistence.md, "Mission content"). The layout is the Mission
//! Editor's (ED-CODE, DCS 2.9.31.29434, `Scripts/dictionary.lua`,
//! `MissionEditor/modules/me_mission.lua`):
//!
//! - `mission.sortie`, `descriptionText`, `descriptionBlueTask`,
//!   `descriptionRedTask` and `descriptionNeutralsTask` hold a key
//!   `DictKey_<field>_<N>`; the text is in `l10n/<locale>/dictionary`.
//! - `mission.pictureFileNameB`, `R` and `N` are lists of resource keys;
//!   `l10n/DEFAULT/mapResource` maps a key to a file name, and the file is the
//!   member `l10n/DEFAULT/<name>`.
//! - `mission.maxDictId` counts the keys of both kinds.
//! - Kneeboard pages are the members `KNEEBOARD/IMAGES/<name>` and
//!   `KNEEBOARD/<type>/IMAGES/<name>`, outside `mapResource`.

use nanoprobe_transport::codec::Lv;

use super::lua::{self, Doc, Edit, Key};
use super::media::Media;

/// A file's place in the mission.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileKind {
    /// `l10n/DEFAULT/<name>`, keyed in `mapResource`.
    Mission,
    /// `KNEEBOARD/IMAGES/<name>`.
    Kneeboard,
    /// `KNEEBOARD/<type>/IMAGES/<name>`.
    KneeboardType(String),
}

impl FileKind {
    /// The folder of the member, with its trailing `/`.
    pub fn folder(&self) -> String {
        match self {
            FileKind::Mission => "l10n/DEFAULT/".into(),
            FileKind::Kneeboard => "KNEEBOARD/IMAGES/".into(),
            FileKind::KneeboardType(t) => format!("KNEEBOARD/{t}/IMAGES/"),
        }
    }
}

/// A file a campaign carries.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    /// Where it goes.
    pub kind: FileKind,
    /// Its name in the mission.
    pub name: String,
    /// Its bytes, checked when it was set.
    pub body: Vec<u8>,
}

/// Briefing text, each `None` to keep the template's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Briefing {
    /// `sortie`.
    pub sortie: Option<Vec<u8>>,
    /// `descriptionText`.
    pub description: Option<Vec<u8>>,
    /// `descriptionBlueTask`.
    pub blue_task: Option<Vec<u8>>,
    /// `descriptionRedTask`.
    pub red_task: Option<Vec<u8>>,
    /// `descriptionNeutralsTask`.
    pub neutral_task: Option<Vec<u8>>,
}

impl Briefing {
    /// The mission field of each text, with the text.
    pub fn fields(&self) -> [(&'static str, Option<&[u8]>); 5] {
        [
            ("sortie", self.sortie.as_deref()),
            ("descriptionText", self.description.as_deref()),
            ("descriptionBlueTask", self.blue_task.as_deref()),
            ("descriptionRedTask", self.red_task.as_deref()),
            ("descriptionNeutralsTask", self.neutral_task.as_deref()),
        ]
    }
}

/// The largest briefing text, in bytes.
pub const TEXT_MAX: usize = 65_536;

/// The most images per coalition.
pub const IMAGES_MAX: usize = 16;

/// The image lists of a mission: blue, red, neutral, and the coalition
/// selection screen.
pub type Images = [Option<Vec<String>>; 4];

/// The image-list fields, in the order of [`Images`].
pub const IMAGE_FIELDS: [&str; 4] = [
    "pictureFileNameB",
    "pictureFileNameR",
    "pictureFileNameN",
    "pictureFileNameServer",
];

/// What a shell's content changes: the edits to `mission`, and the members
/// replaced or added, by name.
pub(crate) struct Plan {
    pub mission: Vec<Edit>,
    pub members: Vec<(String, Vec<u8>)>,
}

/// A name a file may have in a mission without being the template's own:
/// 1 to 64 characters of `A-Z a-z 0-9 _ - .` with a known extension.
pub fn is_file_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && Media::of_name(name).is_some()
        && !name.contains("..")
}

/// A name that is safe as a member name: no separator, `..` or NUL, at
/// most 255 bytes, with a known extension. A template's own file may have
/// one outside [`is_file_name`].
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains(['/', '\\', '\0'])
        && !name.contains("..")
        && Media::of_name(name).is_some()
}

/// An aircraft type a kneeboard folder may name.
pub fn is_type_name(t: &str) -> bool {
    (1..=64).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && !t.contains("..")
}

fn quoted(b: &[u8]) -> Vec<u8> {
    let mut o = String::new();
    lua::quote(&mut o, b);
    o.into_bytes()
}

/// A parsed `<name> = { ... }` member and its source.
struct Table {
    src: Vec<u8>,
    doc: Doc,
}

impl Table {
    fn parse(src: &[u8], name: &str) -> Result<Table, String> {
        let doc = lua::parse(src).map_err(|e| format!("`{name}`: {e}"))?;
        if !doc.root.table {
            return Err(format!("`{name}` does not assign a table"));
        }
        Ok(Table {
            src: src.to_vec(),
            doc,
        })
    }

    fn empty(name: &str) -> Table {
        let src = format!("{name} = \n{{\n}} -- end of {name}\n").into_bytes();
        Table::parse(&src, name).expect("an empty table parses")
    }

    #[cfg(test)]
    fn string_at(&self, key: &str) -> Option<Vec<u8>> {
        match self
            .doc
            .root
            .get(&Key::s(key))
            .and_then(|n| lua::scalar_value(&self.src, n))
        {
            Some(Lv::String(b)) => Some(b),
            _ => None,
        }
    }

    /// The string-keyed fields with string values.
    fn strings(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.doc
            .root
            .fields
            .iter()
            .filter_map(|(k, n)| match (k, lua::scalar_value(&self.src, n)) {
                (Key::Str(k), Some(Lv::String(v))) => Some((k.clone(), v)),
                _ => None,
            })
            .collect()
    }
}

/// Edits that set string-keyed fields of a table: a field it has is
/// replaced, the others are added before its closing brace.
fn set_fields(src: &[u8], root: &lua::Node, fields: &[(Vec<u8>, Vec<u8>)]) -> Vec<Edit> {
    let mut edits = Vec::new();
    let mut added = Vec::new();
    for (k, text) in fields {
        match root.get(&Key::Str(k.clone())) {
            Some(n) => edits.push(Edit {
                span: n.span.clone(),
                text: text.clone(),
            }),
            None => {
                added.extend_from_slice(b"\t[");
                added.extend_from_slice(&quoted(k));
                added.extend_from_slice(b"] = ");
                added.extend_from_slice(text);
                added.extend_from_slice(b",\n");
            }
        }
    }
    if !added.is_empty() {
        let close = root.span.end - 1;
        // The last field must end with a separator before a new one.
        let after_last = root
            .fields
            .last()
            .map_or(root.span.start + 1, |(_, n)| n.span.end);
        let between = strip_comments(&src[after_last..close]);
        let mut text = Vec::new();
        if !root.fields.is_empty() && !matches!(between.first(), Some(b',' | b';')) {
            text.push(b',');
        }
        if !src[..close].ends_with(b"\n") {
            text.push(b'\n');
        }
        text.extend_from_slice(&added);
        edits.push(Edit {
            span: close..close,
            text,
        });
    }
    edits
}

/// The bytes with whitespace and `--` line comments dropped.
fn strip_comments(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"--") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else {
            if !b[i].is_ascii_whitespace() {
                out.push(b[i]);
            }
            i += 1;
        }
    }
    out
}

fn member_index(m: &[(String, Vec<u8>)], name: &str) -> Option<usize> {
    m.iter().position(|(n, _)| n == name)
}

/// The locales the template holds a dictionary for: `l10n/<locale>/dictionary`.
fn dictionaries(m: &[(String, Vec<u8>)]) -> Vec<String> {
    m.iter()
        .filter_map(|(n, _)| {
            let rest = n.strip_prefix("l10n/")?;
            let (loc, file) = rest.split_once('/')?;
            (file == "dictionary" && !loc.is_empty()).then(|| n.clone())
        })
        .collect()
}

/// Plan the content of a shell: `mission` is the template's parsed
/// `mission` member and `m` its members.
pub(crate) fn plan(
    m: &[(String, Vec<u8>)],
    mission_src: &[u8],
    mission: &Doc,
    briefing: &Briefing,
    images: &Images,
    files: &[File],
) -> Result<Plan, String> {
    let mut plan = Plan {
        mission: Vec::new(),
        members: Vec::new(),
    };
    let texts: Vec<(&str, &[u8])> = briefing
        .fields()
        .into_iter()
        .filter_map(|(f, t)| t.map(|t| (f, t)))
        .collect();
    let any_image = images.iter().any(Option::is_some);
    let mission_files: Vec<&File> = files
        .iter()
        .filter(|f| f.kind == FileKind::Mission)
        .collect();
    if texts.is_empty() && !any_image && files.is_empty() {
        return Ok(plan);
    }
    let root = &mission.root;
    let max0 = match root
        .get(&Key::s("maxDictId"))
        .and_then(|n| lua::scalar_value(mission_src, n))
    {
        Some(Lv::Number(n)) if n.fract() == 0.0 && n >= 0.0 => n as u64,
        Some(_) => return Err("`mission` has a maxDictId that is not a whole number".into()),
        None => 0,
    };
    let mut next = max0;

    // Briefing text: one key per field, written in every dictionary.
    let mut dict_fields: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut mission_fields: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for (field, text) in &texts {
        if text.len() > TEXT_MAX || text.contains(&0) {
            return Err(format!(
                "the text of `{field}` is over {TEXT_MAX} bytes or holds NUL"
            ));
        }
        let current = match root
            .get(&Key::s(field))
            .and_then(|n| lua::scalar_value(mission_src, n))
        {
            Some(Lv::String(b)) => Some(b),
            _ => None,
        };
        let key = match current {
            Some(k) if k.starts_with(b"DictKey_") => k,
            _ => {
                next += 1;
                let k = format!("DictKey_{field}_{next}").into_bytes();
                mission_fields.push((field.as_bytes().to_vec(), quoted(&k)));
                k
            }
        };
        dict_fields.push((key, quoted(text)));
    }
    if !dict_fields.is_empty() {
        let mut names = dictionaries(m);
        if !names.iter().any(|n| n == "l10n/DEFAULT/dictionary") {
            names.insert(0, "l10n/DEFAULT/dictionary".into());
        }
        for name in names {
            let t = match member_index(m, &name) {
                Some(i) => Table::parse(&m[i].1, "dictionary")?,
                None => Table::empty("dictionary"),
            };
            let bytes = lua::apply(&t.src, set_fields(&t.src, &t.doc.root, &dict_fields))?;
            plan.members.push((name, bytes));
        }
    }

    // Files and their resource keys.
    let map_name = "l10n/DEFAULT/mapResource";
    let map = match member_index(m, map_name) {
        Some(i) => Table::parse(&m[i].1, "mapResource")?,
        None => Table::empty("mapResource"),
    };
    let keyed: Vec<(Vec<u8>, Vec<u8>)> = map.strings();
    let mut new_keys: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let key_for = |name: &str, new_keys: &mut Vec<(Vec<u8>, Vec<u8>)>, next: &mut u64| -> Vec<u8> {
        if let Some((k, _)) = keyed
            .iter()
            .chain(new_keys.iter())
            .find(|(_, v)| v == name.as_bytes())
        {
            return k.clone();
        }
        *next += 1;
        let k = format!("ResKey_np_{next}").into_bytes();
        new_keys.push((k.clone(), name.as_bytes().to_vec()));
        k
    };
    for f in files {
        let folder = f.kind.folder();
        let path = format!("{folder}{}", f.name);
        let media = Media::of_name(&f.name).ok_or_else(|| format!("{}: no known type", f.name))?;
        if f.kind != FileKind::Mission && !media.is_image() {
            return Err(format!("{}: a kneeboard page is an image", f.name));
        }
        // A template member of the same name in another case is refused.
        if let Some((other, _)) = m
            .iter()
            .find(|(n, _)| n != &path && n.len() == path.len() && n.eq_ignore_ascii_case(&path))
        {
            return Err(format!(
                "{} differs only in case from the template's {other}",
                f.name
            ));
        }
        let held = member_index(m, &path).is_some();
        if !held && !is_file_name(&f.name) {
            return Err(format!(
                "{} is not a file name a new file may have, and the template has no such file",
                f.name
            ));
        }
        plan.members.push((path, f.body.clone()));
        if f.kind == FileKind::Mission {
            // The same name in a locale folder is replaced too.
            for (n, _) in m {
                if let Some(rest) = n.strip_prefix("l10n/")
                    && let Some((loc, file)) = rest.split_once('/')
                    && loc != "DEFAULT"
                    && file == f.name
                {
                    plan.members.push((n.clone(), f.body.clone()));
                }
            }
            key_for(&f.name, &mut new_keys, &mut next);
        }
    }

    // Briefing images.
    for (list, field) in images.iter().zip(IMAGE_FIELDS) {
        let Some(list) = list else {
            continue;
        };
        if list.len() > IMAGES_MAX {
            return Err(format!(
                "a coalition has at most {IMAGES_MAX} briefing images"
            ));
        }
        let mut keys = Vec::new();
        for name in list {
            if !Media::of_name(name).is_some_and(Media::is_image) {
                return Err(format!("briefing image {name:?} is not an image"));
            }
            let ours = mission_files.iter().any(|f| f.name == *name);
            let theirs = member_index(m, &format!("l10n/DEFAULT/{name}")).is_some();
            if !ours && !theirs {
                return Err(format!(
                    "briefing image {name:?} is neither a campaign file nor in the template"
                ));
            }
            keys.push(key_for(name, &mut new_keys, &mut next));
        }
        let lv = Lv::Table(
            keys.into_iter()
                .enumerate()
                .map(|(i, k)| (Lv::Number((i + 1) as f64), Lv::String(k)))
                .collect(),
        );
        let text = lua::write(&lv, 1)?;
        mission_fields.push((
            field.as_bytes().to_vec(),
            text.trim_start_matches(['\n', '\t']).as_bytes().to_vec(),
        ));
    }
    if !new_keys.is_empty() {
        let values: Vec<(Vec<u8>, Vec<u8>)> = new_keys
            .iter()
            .map(|(k, v)| (k.clone(), quoted(v)))
            .collect();
        let bytes = lua::apply(&map.src, set_fields(&map.src, &map.doc.root, &values))?;
        plan.members.push((map_name.into(), bytes));
    }
    if next != max0 {
        mission_fields.push((b"maxDictId".to_vec(), next.to_string().into_bytes()));
    }
    plan.mission = set_fields(mission_src, &mission.root, &mission_fields);
    Ok(plan)
}

/// The `mapResource` key of the file `name` in `l10n/DEFAULT/` of the shell
/// at `path`; `None` when the shell, its map or such a file is absent. Only
/// the map is read. A picture DCS is given by its bare file name does not
/// show; given by its key, it does (LIVE,
/// docs/results/2026-11-26-mission-content.md).
pub fn resource_key(path: &std::path::Path, name: &str) -> Option<String> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(f).ok()?;
    let member = zip.by_name("l10n/DEFAULT/mapResource").ok()?;
    let mut src = Vec::new();
    member
        .take(super::TEMPLATE_MAX as u64)
        .read_to_end(&mut src)
        .ok()?;
    let t = Table::parse(&src, "mapResource").ok()?;
    t.strings()
        .into_iter()
        .find(|(_, v)| v == name.as_bytes())
        .and_then(|(k, _)| String::from_utf8(k).ok())
}

/// The path in the mission, `l10n/DEFAULT/<name>`, of the file `name` the
/// shell at `path` holds there; `None` when the shell or such a member is
/// absent, or `name` holds a separator. `trigger.action.radioTransmission`
/// plays a file given by this path, and not one given by its bare name or
/// its key (LIVE, docs/results/2026-11-26-mission-content.md).
pub fn resource_path(path: &std::path::Path, name: &str) -> Option<String> {
    if !is_safe_name(name) {
        return None;
    }
    let f = std::fs::File::open(path).ok()?;
    let zip = zip::ZipArchive::new(f).ok()?;
    let member = format!("l10n/DEFAULT/{name}");
    zip.index_for_name(&member).map(|_| member)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_name_resolves_to_its_key_in_a_written_shell() {
        let d = tempfile::tempdir().expect("dir");
        let map = b"mapResource = \n{\n\t[\"ResKey_np_6\"] = \"blue.png\",\n} -- end\n";
        let path = d.path().join("s.miz");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).expect("file"));
        w.start_file(
            "l10n/DEFAULT/mapResource",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("start");
        std::io::Write::write_all(&mut w, map).expect("write");
        w.finish().expect("zip");
        assert_eq!(
            resource_key(&path, "blue.png").as_deref(),
            Some("ResKey_np_6")
        );
        assert_eq!(resource_key(&path, "red.png"), None);
        assert_eq!(resource_key(&d.path().join("none.miz"), "blue.png"), None);
    }

    #[test]
    fn a_file_name_resolves_to_its_path_in_a_written_shell() {
        let d = tempfile::tempdir().expect("dir");
        let path = d.path().join("s.miz");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).expect("file"));
        w.start_file(
            "l10n/DEFAULT/call.ogg",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("start");
        std::io::Write::write_all(&mut w, b"OggS").expect("write");
        w.finish().expect("zip");
        assert_eq!(
            resource_path(&path, "call.ogg").as_deref(),
            Some("l10n/DEFAULT/call.ogg")
        );
        assert_eq!(resource_path(&path, "other.ogg"), None);
        assert_eq!(resource_path(&path, "l10n/DEFAULT/call.ogg"), None);
        assert_eq!(resource_path(&d.path().join("none.miz"), "call.ogg"), None);
    }

    #[test]
    fn names_follow_the_rules() {
        assert!(is_file_name("brief-1.png"));
        assert!(is_file_name("A_b.c.JPEG"));
        assert!(!is_file_name("a b.png"));
        assert!(!is_file_name("x.mp3"));
        assert!(!is_file_name("..png"));
        assert!(!is_file_name(&format!("{}.ogg", "a".repeat(61))));
        assert!(is_safe_name("Radio call (1).ogg"));
        assert!(!is_safe_name("a/b.ogg"));
        assert!(!is_safe_name("a\\b.ogg"));
        assert!(!is_safe_name("a\0.ogg"));
        assert!(!is_safe_name("a..b.ogg"));
        assert!(is_type_name("UH-1H"));
        assert!(!is_type_name("a/b"));
    }

    #[test]
    fn fields_are_replaced_or_added_after_a_separator() {
        for src in [
            "t = \n{\n\t[\"a\"] = 1,\n} -- end of t\n",
            "t = { a = 1 }",
            "t = { a = 1; -- c\n}",
            "t = {}",
        ] {
            let tb = Table::parse(src.as_bytes(), "t").expect("parse");
            let edits = set_fields(
                &tb.src,
                &tb.doc.root,
                &[
                    (b"a".to_vec(), b"2".to_vec()),
                    (b"b".to_vec(), b"\"x\"".to_vec()),
                ],
            );
            let out = lua::apply(src.as_bytes(), edits).expect("apply");
            let back = Table::parse(&out, "t").expect("reparse");
            assert_eq!(back.string_at("b"), Some(b"x".to_vec()), "{src}");
            let a = back
                .doc
                .root
                .get(&Key::s("a"))
                .and_then(|n| lua::scalar_value(&out, n));
            if src != "t = {}" {
                assert_eq!(a, Some(Lv::Number(2.0)), "{src}");
            }
        }
    }
}
