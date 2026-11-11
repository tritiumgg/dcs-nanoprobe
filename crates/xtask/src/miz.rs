//! The mission generator: write a loadable shell `.miz` from a template.
//!
//! A `.miz` is a zip of Lua source files (ED-CODE, `me_mission.lua`). The
//! template is an empty mission saved from the Mission Editor, one per map.
//! Version 0 copies the template's members and can set the mission's start
//! time. It writes the members in the order the Mission Editor does, all
//! deflated (ED-CODE): `mission`, `l10n/DEFAULT/dictionary`,
//! `l10n/DEFAULT/mapResource`, `warehouses`, `options`, any resource files,
//! then `theatre`.

use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// The members a shell needs, in the Mission Editor's order. `theatre` goes
/// last, after any resource files.
const LEADING: [&str; 5] = [
    "mission",
    "l10n/DEFAULT/dictionary",
    "l10n/DEFAULT/mapResource",
    "warehouses",
    "options",
];
const THEATRE: &str = "theatre";

/// What to change in the template.
#[derive(Debug, Default)]
pub struct Changes {
    /// Mission start time, seconds after midnight.
    pub start_time: Option<u32>,
}

/// The members of a `.miz`, in file order, with their bytes.
pub fn read(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a .miz: {e}"))?;
    let mut members = Vec::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| e.to_string())?;
        if file.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|e| e.to_string())?;
        let name = file.name().map_err(|e| e.to_string())?.into_owned();
        members.push((name, data));
    }
    Ok(members)
}

/// Write a shell from `template`'s bytes.
pub fn generate(template: &[u8], changes: &Changes) -> Result<Vec<u8>, String> {
    let mut members = read(template)?;
    for name in LEADING.iter().chain([&THEATRE]) {
        if !members.iter().any(|(n, _)| n == name) {
            return Err(format!("the template has no `{name}` member"));
        }
    }

    if let Some(start) = changes.start_time {
        if start >= 86_400 {
            return Err(format!("start time {start} is not within one day"));
        }
        let mission = members
            .iter_mut()
            .find(|(n, _)| n == "mission")
            .expect("checked above");
        mission.1 = set_start_time(&mission.1, start)?;
    }

    let rank = |name: &str| {
        LEADING
            .iter()
            .position(|n| *n == name)
            .unwrap_or(if name == THEATRE {
                usize::MAX
            } else {
                LEADING.len()
            })
    };
    // A stable sort keeps resource files in the template's order.
    members.sort_by_key(|(name, _)| rank(name));

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in &members {
        writer
            .start_file(name.as_str(), options)
            .map_err(|e| e.to_string())?;
        writer.write_all(data).map_err(|e| e.to_string())?;
    }
    Ok(writer.finish().map_err(|e| e.to_string())?.into_inner())
}

/// Replace the value on the one `["start_time"] = <n>,` line in `mission`.
fn set_start_time(mission: &[u8], start: u32) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(mission).map_err(|_| "`mission` is not UTF-8".to_owned())?;
    const KEY: &str = "[\"start_time\"] = ";
    let mut found = 0;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(KEY) {
            let digits = rest.trim_end().trim_end_matches(',');
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                found += 1;
                let indent = &line[..line.len() - trimmed.len()];
                let ending = if line.ends_with("\r\n") {
                    "\r\n"
                } else if line.ends_with('\n') {
                    "\n"
                } else {
                    ""
                };
                out.push_str(&format!("{indent}{KEY}{start},{ending}"));
                continue;
            }
        }
        out.push_str(line);
    }
    if found != 1 {
        return Err(format!(
            "`mission` has {found} start_time lines; version 0 changes exactly one"
        ));
    }
    Ok(out.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A template laid out as the Mission Editor writes one, but with members
    /// in a different order, a resource file, and one member stored.
    fn template() -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let members: [(&str, &str); 7] = [
            ("theatre", "Caucasus"),
            ("options", "options = {}\n"),
            ("l10n/DEFAULT/briefing.png", "png bytes"),
            (
                "mission",
                "mission = \n{\n    [\"start_time\"] = 28800,\n    [\"theatre\"] = \"Caucasus\",\n}\n",
            ),
            ("warehouses", "warehouses = {}\n"),
            ("l10n/DEFAULT/mapResource", "mapResource = {}\n"),
            ("l10n/DEFAULT/dictionary", "dictionary = {}\n"),
        ];
        for (name, text) in members {
            writer.start_file(name, stored).unwrap();
            writer.write_all(text.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn matches_the_template_layout() {
        let template = template();
        let shell = generate(&template, &Changes::default()).unwrap();

        let names: Vec<String> = read(&shell).unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            names,
            [
                "mission",
                "l10n/DEFAULT/dictionary",
                "l10n/DEFAULT/mapResource",
                "warehouses",
                "options",
                "l10n/DEFAULT/briefing.png",
                "theatre",
            ]
        );

        let mut original = read(&template).unwrap();
        let mut copied = read(&shell).unwrap();
        original.sort();
        copied.sort();
        assert_eq!(original, copied, "a member's contents changed");

        let mut archive = ZipArchive::new(Cursor::new(&shell)).unwrap();
        for i in 0..archive.len() {
            let file = archive.by_index(i).unwrap();
            assert_eq!(
                file.compression(),
                CompressionMethod::Deflated,
                "{:?}",
                file.name()
            );
        }
    }

    #[test]
    fn sets_the_start_time() {
        let shell = generate(
            &template(),
            &Changes {
                start_time: Some(3600),
            },
        )
        .unwrap();
        let members = read(&shell).unwrap();
        let mission = String::from_utf8(members[0].1.clone()).unwrap();
        assert!(
            mission.contains("\n    [\"start_time\"] = 3600,\n"),
            "{mission}"
        );
        assert!(mission.contains("[\"theatre\"] = \"Caucasus\""));
    }

    #[test]
    fn refuses_an_incomplete_template() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("mission", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"mission = {}").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let error = generate(&bytes, &Changes::default()).unwrap_err();
        assert!(error.contains("l10n/DEFAULT/dictionary"), "{error}");
    }

    #[test]
    fn refuses_an_ambiguous_start_time() {
        let error =
            set_start_time(b"[\"start_time\"] = 1,\n  [\"start_time\"] = 2,\n", 5).unwrap_err();
        assert!(error.contains("2 start_time lines"), "{error}");
        assert!(set_start_time(b"x = 1\n", 5).is_err());
        assert!(generate(b"not a zip", &Changes::default()).is_err());
    }

    /// Every committed template yields a shell, and carries only the options
    /// templates/README.md allows, so no player name or machine setting.
    #[test]
    fn the_committed_templates_generate_and_hold_nothing_personal() {
        let folder = crate::repository().join("templates");
        let mut seen = 0;
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "miz") {
                continue;
            }
            seen += 1;
            let bytes = std::fs::read(&path).unwrap();
            let shell = generate(
                &bytes,
                &Changes {
                    start_time: Some(43_200),
                },
            )
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let names: Vec<String> = read(&shell).unwrap().into_iter().map(|(n, _)| n).collect();
            assert_eq!(
                names,
                [&LEADING[..], &[THEATRE]].concat(),
                "{}",
                path.display()
            );

            let members = read(&bytes).unwrap();
            let options = &members.iter().find(|(n, _)| n == "options").unwrap().1;
            let options = String::from_utf8_lossy(options);
            for key in [
                "playerName",
                "graphics",
                "VR",
                "sound",
                "plugins",
                "views",
                "miscellaneous",
            ] {
                assert!(
                    !options.contains(&format!("[\"{key}\"]")),
                    "{} keeps options.{key}",
                    path.display()
                );
            }
        }
        assert!(seen > 0, "no templates in {}", folder.display());
    }
}
