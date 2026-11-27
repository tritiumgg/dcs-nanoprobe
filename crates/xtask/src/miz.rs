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
    /// The airport (by its airbase ID) whose warehouse is made limited.
    pub limited_airport: Option<u32>,
    /// The FARP or other static (by its unit ID) whose warehouse is made
    /// limited.
    pub limited_warehouse: Option<u32>,
    /// A static group of `FARP` units and its warehouse entries
    /// ([`crate::miz_heliport`]).
    pub heliport_group: Option<crate::miz_heliport::Group>,
    /// Resource files to pack: `l10n/DEFAULT/<file>` members, each with a
    /// `mapResource` entry from its key to the file name.
    pub resources: Vec<Resource>,
}

/// One packed resource file (ED-CODE: `Scripts/dictionary.lua`,
/// `setValueToResource`: `mapResource[key] = file name`, the bytes at
/// `l10n/DEFAULT/<file name>`).
#[derive(Debug, Clone)]
pub struct Resource {
    pub key: String,
    pub file: String,
    pub bytes: Vec<u8>,
}

/// The test resources `--sound-picture` packs: a 0.5 s 440 Hz tone and an
/// 8 by 8 picture, both generated here.
pub fn test_resources() -> Vec<Resource> {
    vec![
        Resource {
            key: "ResKey_Action_901".into(),
            file: "np-tone.wav".into(),
            bytes: tone_wav(440.0, 0.5),
        },
        Resource {
            key: "ResKey_Action_902".into(),
            file: "np-picture.png".into(),
            bytes: picture_png(),
        },
    ]
}

/// A 16-bit mono 22,050 Hz WAV of a sine at `freq_hz`, `seconds` long.
pub fn tone_wav(freq_hz: f64, seconds: f64) -> Vec<u8> {
    let rate = 22_050u32;
    let samples = (f64::from(rate) * seconds).round() as u32;
    let data_len = samples * 2;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..samples {
        let t = i as f64 / rate as f64;
        let v = (t * freq_hz * std::f64::consts::TAU).sin() * 12_000.0;
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// An 8 by 8 RGB PNG, red and blue squares, with stored (uncompressed)
/// deflate blocks.
fn picture_png() -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..8u8 {
        raw.push(0);
        for x in 0..8u8 {
            let red = (x / 4 + y / 4) % 2 == 0;
            raw.extend_from_slice(if red { &[220, 30, 30] } else { &[30, 30, 220] });
        }
    }
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&8u32.to_be_bytes());
    ihdr.extend_from_slice(&8u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// A `width` by `height` RGB PNG of one color, with stored (uncompressed)
/// deflate blocks of at most 65,535 bytes each.
pub fn solid_png(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((height * (1 + 3 * width)) as usize);
    let mut row = vec![0u8];
    for _ in 0..width {
        row.extend_from_slice(&rgb);
    }
    for _ in 0..height {
        raw.extend_from_slice(&row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(u8::from(i + 1 == blocks.len()));
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

/// `mapResource` as the Mission Editor writes it.
fn map_resource(resources: &[Resource]) -> Vec<u8> {
    let mut text = String::from("mapResource = \n{\n");
    for r in resources {
        text.push_str(&format!("\t[\"{}\"] = \"{}\",\n", r.key, r.file));
    }
    text.push_str("} -- end of mapResource\n");
    text.into_bytes()
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

    if let Some(id) = changes.limited_airport {
        let warehouses = members
            .iter_mut()
            .find(|(n, _)| n == "warehouses")
            .expect("checked above");
        warehouses.1 = limit_warehouse(&warehouses.1, id)?;
    }
    if let Some(id) = changes.limited_warehouse {
        let warehouses = members
            .iter_mut()
            .find(|(n, _)| n == "warehouses")
            .expect("checked above");
        warehouses.1 = limit_section(&warehouses.1, "warehouses", id)?;
    }

    if let Some(group) = &changes.heliport_group {
        let take = |members: &mut Vec<(String, Vec<u8>)>, name: &str| {
            std::mem::take(
                &mut members
                    .iter_mut()
                    .find(|(n, _)| n == name)
                    .expect("checked above")
                    .1,
            )
        };
        let mission = take(&mut members, "mission");
        let warehouses = take(&mut members, "warehouses");
        let (mission, warehouses, _) = crate::miz_heliport::add(&mission, &warehouses, group)?;
        for (name, data) in [("mission", mission), ("warehouses", warehouses)] {
            members
                .iter_mut()
                .find(|(n, _)| n == name)
                .expect("checked above")
                .1 = data;
        }
    }

    if !changes.resources.is_empty() {
        let map = members
            .iter_mut()
            .find(|(n, _)| n == "l10n/DEFAULT/mapResource")
            .expect("checked above");
        map.1 = map_resource(&changes.resources);
        for r in &changes.resources {
            members.push((format!("l10n/DEFAULT/{}", r.file), r.bytes.clone()));
        }
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

/// Starting counts of a limited warehouse: 10 F-15C, 100 Mk-82 bombs (wsType
/// 4.5.9.31, UNVERIFIED), and 100 of each fuel.
const LIMITED_AIRCRAFTS: &str = "{ [\"plane\"] = { [\"F-15C\"] = { [\"initialAmount\"] = 10 } } },";
const LIMITED_WEAPONS: &str =
    "{ [1] = { [\"initialAmount\"] = 100, [\"wsType\"] = { 4, 5, 9, 31 } } },";

/// Make the warehouse of airport `id` limited: see [`limit_section`].
fn limit_warehouse(warehouses: &[u8], id: u32) -> Result<Vec<u8>, String> {
    limit_section(warehouses, "airports", id)
}

/// Make warehouse `id` of `section` (`airports`, by airbase ID, or
/// `warehouses`, by the unit ID of a FARP or other static that has one)
/// limited: the three `unlimited*` flags false, and an empty aircraft or
/// weapon table holds the counts above (ED-CODE: the `warehouses` member of a
/// `.miz`). A table that already lists items keeps them. Fuel keeps its
/// `InitFuel` values. Only a direct entry of `section` matches: an entry
/// with the same ID in the other section is never changed.
fn limit_section(warehouses: &[u8], section: &str, id: u32) -> Result<Vec<u8>, String> {
    let text =
        std::str::from_utf8(warehouses).map_err(|_| "`warehouses` is not UTF-8".to_owned())?;
    let opening = format!("[{id}] =");
    let heading = format!("[\"{section}\"]");
    let mut out = String::with_capacity(text.len());
    let (mut inside, mut depth, mut found) = (false, 0i32, false);
    // The section's brace depth while the scan is in it: 1 between its own
    // braces, where its entries open. None outside it.
    let mut section_depth: Option<i32> = None;
    let (mut changed, mut flags, mut tables) = (0, 0, 0);
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        let mut replacement: Option<String> = None;
        let braces = line.matches('{').count() as i32 - line.matches('}').count() as i32;
        if !inside {
            if section_depth.is_none() && t.starts_with(&heading) {
                section_depth = Some(0);
            } else if section_depth == Some(1) && t.starts_with(&opening) {
                inside = true;
                found = true;
                depth = 0;
            }
        } else {
            // `t` is trimmed, so a table opened on the next line ends in `=`.
            let key = |k: &str| t.starts_with(&format!("[\"{k}\"] ="));
            let indent = &line[..line.len() - line.trim_start().len()];
            let eol = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
            if depth == 1 {
                for flag in ["unlimitedMunitions", "unlimitedAircrafts", "unlimitedFuel"] {
                    if key(flag) {
                        replacement = Some(format!("{indent}[\"{flag}\"] = false,{eol}"));
                        flags += 1;
                    }
                }
                if key("aircrafts") {
                    tables += 1;
                    if t.ends_with("{},") {
                        replacement = Some(format!(
                            "{indent}[\"aircrafts\"] = {LIMITED_AIRCRAFTS}{eol}"
                        ));
                    }
                }
                if key("weapons") {
                    tables += 1;
                    if t.ends_with("{},") {
                        replacement =
                            Some(format!("{indent}[\"weapons\"] = {LIMITED_WEAPONS}{eol}"));
                    }
                }
            }
        }
        if inside {
            depth += braces;
            if replacement.is_some() {
                changed += 1;
            }
            if depth <= 0 && line.contains('}') {
                inside = false;
            }
        }
        if let Some(d) = section_depth.as_mut() {
            *d += braces;
            if *d <= 0 && line.contains('}') {
                section_depth = None;
            }
        }
        out.push_str(replacement.as_deref().unwrap_or(line));
    }
    if !found {
        return Err(format!("`warehouses` has no {section} entry {id}"));
    }
    // An entry without both tables would leave a limited warehouse with no
    // aircraft or no weapons at all.
    if flags != 3 || tables != 2 || changed > 5 {
        return Err(format!(
            "{section} {id}: changed {flags} of 3 flags, found {tables} of 2 tables (aircrafts, weapons), {changed} fields changed"
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
                ..Changes::default()
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

    const WAREHOUSES: &str = "warehouses = \n{\n\t[\"airports\"] = \n\t{\n\t\t[12] = \n\t\t{\n\t\t\t[\"unlimitedMunitions\"] = true,\n\t\t\t[\"jet_fuel\"] = \n\t\t\t{\n\t\t\t\t[\"InitFuel\"] = 100,\n\t\t\t}, -- end of [\"jet_fuel\"]\n\t\t\t[\"unlimitedAircrafts\"] = true,\n\t\t\t[\"unlimitedFuel\"] = true,\n\t\t\t[\"aircrafts\"] = {},\n\t\t\t[\"weapons\"] = {},\n\t\t}, -- end of [12]\n\t\t[13] = \n\t\t{\n\t\t\t[\"unlimitedMunitions\"] = true,\n\t\t\t[\"unlimitedAircrafts\"] = true,\n\t\t\t[\"unlimitedFuel\"] = true,\n\t\t\t[\"aircrafts\"] = {},\n\t\t\t[\"weapons\"] = {},\n\t\t}, -- end of [13]\n\t}, -- end of [\"airports\"]\n}\n";

    #[test]
    fn limits_one_airport_only() {
        let out = String::from_utf8(limit_warehouse(WAREHOUSES.as_bytes(), 13).unwrap()).unwrap();
        let (first, second) = out.split_once("[13] = ").unwrap();
        assert!(first.contains("[\"unlimitedMunitions\"] = true"), "{first}");
        assert!(first.contains("[\"aircrafts\"] = {},"));
        assert!(!second.contains("= true"), "{second}");
        assert!(second.contains("[\"initialAmount\"] = 10 "), "{second}");
        assert!(second.contains("[\"initialAmount\"] = 100,"), "{second}");
        let out = String::from_utf8(limit_warehouse(WAREHOUSES.as_bytes(), 12).unwrap()).unwrap();
        let (first, second) = out.split_once("[13] = ").unwrap();
        assert!(!first.contains("unlimitedFuel\"] = true"), "{first}");
        assert!(first.contains("InitFuel"));
        assert!(second.contains("[\"unlimitedFuel\"] = true"));
    }

    #[test]
    fn packs_the_test_resources_with_a_map_entry_each() {
        let shell = generate(
            &template(),
            &Changes {
                resources: test_resources(),
                ..Changes::default()
            },
        )
        .unwrap();
        let members = read(&shell).unwrap();
        let get = |n: &str| members.iter().find(|(m, _)| m == n).map(|(_, b)| b.clone());
        let map = String::from_utf8(get("l10n/DEFAULT/mapResource").unwrap()).unwrap();
        assert!(
            map.contains("[\"ResKey_Action_901\"] = \"np-tone.wav\","),
            "{map}"
        );
        assert!(
            map.contains("[\"ResKey_Action_902\"] = \"np-picture.png\","),
            "{map}"
        );
        let wav = get("l10n/DEFAULT/np-tone.wav").unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(wav.len(), 44 + 22_050);
        let png = get("l10n/DEFAULT/np-picture.png").unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(members.last().unwrap().0, "theatre");
    }

    #[test]
    fn limits_a_farp_and_keeps_listed_items() {
        let text = "warehouses = \n{\n\t[\"warehouses\"] = \n\t{\n\t\t[8624] = \n\t\t{\n\t\t\t[\"unlimitedMunitions\"] = true,\n\t\t\t[\"unlimitedAircrafts\"] = true,\n\t\t\t[\"unlimitedFuel\"] = true,\n\t\t\t[\"aircrafts\"] = \n\t\t\t{\n\t\t\t\t[\"helicopters\"] = {},\n\t\t\t}, -- end of [\"aircrafts\"]\n\t\t\t[\"weapons\"] = {},\n\t\t}, -- end of [8624]\n\t}, -- end of [\"warehouses\"]\n\t[\"airports\"] = \n\t{\n\t\t[12] = \n\t\t{\n\t\t\t[\"unlimitedFuel\"] = true,\n\t\t}, -- end of [12]\n\t}, -- end of [\"airports\"]\n}\n";
        let out =
            String::from_utf8(limit_section(text.as_bytes(), "warehouses", 8624).unwrap()).unwrap();
        let (farp, airports) = out.split_once("[\"airports\"]").unwrap();
        assert_eq!(farp.matches("= false,").count(), 3, "{farp}");
        assert!(farp.contains("[\"helicopters\"] = {},"), "{farp}");
        assert!(farp.contains("[\"initialAmount\"] = 100,"), "{farp}");
        assert!(
            airports.contains("[\"unlimitedFuel\"] = true"),
            "{airports}"
        );
    }

    /// An entry that is complete but sits in the other section, in either
    /// order of the two sections.
    #[test]
    fn matches_only_its_own_section() {
        let entry = |id: u32| {
            format!(
                "\t\t[{id}] = \n\t\t{{\n\t\t\t[\"unlimitedMunitions\"] = true,\n\t\t\t[\"unlimitedAircrafts\"] = true,\n\t\t\t[\"unlimitedFuel\"] = true,\n\t\t\t[\"aircrafts\"] = {{}},\n\t\t\t[\"weapons\"] = {{}},\n\t\t}}, -- end of [{id}]\n"
            )
        };
        let section = |name: &str, id: u32| {
            format!(
                "\t[\"{name}\"] = \n\t{{\n{}\t}}, -- end of [\"{name}\"]\n",
                entry(id)
            )
        };
        for (first, second) in [("airports", "warehouses"), ("warehouses", "airports")] {
            let ids = |name: &str| if name == "airports" { 12 } else { 8624 };
            let text = format!(
                "warehouses = \n{{\n{}{}}}\n",
                section(first, ids(first)),
                section(second, ids(second))
            );
            let error = limit_section(text.as_bytes(), "warehouses", 12).unwrap_err();
            assert!(error.contains("no warehouses entry 12"), "{error}");
            let error = limit_section(text.as_bytes(), "airports", 8624).unwrap_err();
            assert!(error.contains("no airports entry 8624"), "{error}");
            let out =
                String::from_utf8(limit_section(text.as_bytes(), "airports", 12).unwrap()).unwrap();
            assert_eq!(out.matches("= false,").count(), 3, "{out}");
            let farp = out.split_once("[8624] =").unwrap().1;
            let farp = farp.split_once("end of [8624]").unwrap().0;
            assert!(!farp.contains("= false,"), "{out}");
        }
    }

    #[test]
    fn refuses_an_unknown_airport() {
        assert!(limit_warehouse(WAREHOUSES.as_bytes(), 99).is_err());
    }

    #[test]
    fn limits_the_committed_caucasus_template() {
        let bytes =
            std::fs::read(crate::repository().join("tests/fixtures/templates/caucasus.miz"))
                .unwrap();
        let shell = generate(
            &bytes,
            &Changes {
                limited_airport: Some(12),
                ..Changes::default()
            },
        )
        .unwrap();
        let members = read(&shell).unwrap();
        let text = String::from_utf8(
            members
                .iter()
                .find(|(n, _)| n == "warehouses")
                .unwrap()
                .1
                .clone(),
        )
        .unwrap();
        assert_eq!(text.matches("[\"unlimitedFuel\"] = false").count(), 1);
        assert!(text.contains("\"F-15C\""));
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
    /// tests/fixtures/templates/README.md allows, so no player name or machine setting.
    #[test]
    fn the_committed_templates_generate_and_hold_nothing_personal() {
        let folder = crate::repository().join("tests/fixtures/templates");
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
                    ..Changes::default()
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
