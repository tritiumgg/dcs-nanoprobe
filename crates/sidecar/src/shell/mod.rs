//! The mission shell writer (plan 4.4): a copy of a template `.miz` with
//! only the runtime fields changed: the date and start time, the weather,
//! and the dynamic-spawn flags of airports in the `warehouses` member. Every
//! other member, and every other byte of the two members it edits, is the
//! template's.
//!
//! A `.miz` is a zip of Lua source files (ED-CODE: `me_mission.lua`). The
//! keys it edits are the ones the Mission Editor saves (ED-CODE: the
//! committed templates): `mission.date` (`Day`, `Month`, `Year`),
//! `mission.start_time` (seconds after midnight), `mission.weather`, and
//! `warehouses.airports[<id>]` `dynamicSpawn` and `allowHotStart`, and
//! `unlimitedAircrafts` false for an airport whose dynamic spawn is on.

pub mod lua;

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use nanoprobe_transport::codec::Lv;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use lua::{Edit, Key};

/// The largest template, and the largest member read from one, in bytes.
pub const TEMPLATE_MAX: usize = 16 * 1024 * 1024;

/// The members every template must hold.
pub const REQUIRED: [&str; 3] = ["mission", "warehouses", "theatre"];

/// The mission's date and start time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Start {
    /// Year, such as 2016.
    pub year: i32,
    /// 1 to 12.
    pub month: u32,
    /// 1 to 31.
    pub day: u32,
    /// Seconds after midnight, below 86,400.
    pub seconds: u32,
}

impl Start {
    /// The start at `ms` milliseconds since the Unix epoch, in UTC.
    pub fn from_unix_ms(ms: i64) -> Start {
        let secs = ms.div_euclid(1000);
        let days = secs.div_euclid(86_400);
        let seconds = u32::try_from(secs.rem_euclid(86_400)).expect("below a day");
        let (year, month, day) = civil(days);
        Start {
            year,
            month,
            day,
            seconds,
        }
    }

    /// Milliseconds since the Unix epoch, in UTC; saturates at the ends of
    /// `i64`.
    pub fn unix_ms(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
            .saturating_mul(86_400)
            .saturating_add(i64::from(self.seconds))
            .saturating_mul(1000)
    }
}

/// Days since 1970-01-01 to a civil date (H. Hinnant's algorithm).
fn civil(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (
        i32::try_from(y).unwrap_or(i32::MAX),
        u32::try_from(m).expect("1 to 12"),
        u32::try_from(d).expect("1 to 31"),
    )
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The dynamic-spawn flags of one airport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicSpawn {
    /// The airport's key in the `warehouses` member's `airports` (that it
    /// equals DCS `Airbase:getID()` is UNVERIFIED).
    pub airbase: u64,
    /// `dynamicSpawn`. When true the shell also writes `unlimitedAircrafts`
    /// false.
    pub enabled: bool,
    /// `allowHotStart`.
    pub hot_start: bool,
}

/// What a shell changes in its template.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Runtime {
    /// The date and start time; `None` keeps the template's.
    pub start: Option<Start>,
    /// The DCS weather table; `None` keeps the template's.
    pub weather: Option<Lv>,
    /// Airports whose flags are set; the others keep the template's.
    pub dynamic: Vec<DynamicSpawn>,
}

/// The members of a `.miz`, in file order, with their bytes.
pub fn members(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    if bytes.len() > TEMPLATE_MAX {
        return Err(format!("a template is at most {TEMPLATE_MAX} bytes"));
    }
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a .miz: {e}"))?;
    let mut out = Vec::new();
    let mut total = 0usize;
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| format!("not a .miz: {e}"))?;
        if file.is_dir() {
            continue;
        }
        let name = file
            .name()
            .map_err(|e| format!("not a .miz: {e}"))?
            .into_owned();
        let mut data = Vec::new();
        // Bound what a small zip may inflate to.
        file.take(TEMPLATE_MAX as u64 + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("not a .miz: {name}: {e}"))?;
        total += data.len();
        if data.len() > TEMPLATE_MAX || total > 4 * TEMPLATE_MAX {
            return Err(format!("{name}: inflates past the template limit"));
        }
        if out.iter().any(|(n, _): &(String, Vec<u8>)| *n == name) {
            return Err(format!("not a .miz: two members named {name}"));
        }
        out.push((name, data));
    }
    Ok(out)
}

fn member<'a>(m: &'a [(String, Vec<u8>)], name: &str) -> Result<&'a [u8], String> {
    m.iter()
        .find(|(n, _)| n == name)
        .map(|(_, d)| d.as_slice())
        .ok_or_else(|| format!("the template has no `{name}` member"))
}

/// The theater a `.miz` names in its `theatre` member.
pub fn theatre(bytes: &[u8]) -> Result<String, String> {
    let m = members(bytes)?;
    theatre_of(&m)
}

fn theatre_of(m: &[(String, Vec<u8>)]) -> Result<String, String> {
    let t = member(m, "theatre")?;
    let t = std::str::from_utf8(t).map_err(|_| "`theatre` is not UTF-8".to_owned())?;
    Ok(t.trim().to_owned())
}

/// Check that `bytes` is a template the writer can use for `theater`: the
/// required members, a `theatre` that names `theater`, and `mission` and
/// `warehouses` that parse with the fields the writer edits.
pub fn check(bytes: &[u8], theater: &str) -> Result<(), String> {
    let m = members(bytes)?;
    for name in REQUIRED {
        member(&m, name)?;
    }
    let t = theatre_of(&m)?;
    if t != theater {
        return Err(format!(
            "the template is for theater {t:?}, not {theater:?}"
        ));
    }
    edit_mission(member(&m, "mission")?, &Runtime::default())?;
    edit_warehouses(member(&m, "warehouses")?, &[])?;
    Ok(())
}

/// The date and start time a `.miz` holds.
pub fn start_of(bytes: &[u8]) -> Result<Start, String> {
    let m = members(bytes)?;
    let src = member(&m, "mission")?;
    let d = parse_named(src, "mission")?;
    let num = |path: &[Key]| -> Result<i64, String> {
        match d.root.at(path).and_then(|n| lua::scalar_value(src, n)) {
            Some(Lv::Number(n)) if n.fract() == 0.0 => Ok(n as i64),
            _ => Err(format!("`mission` has no whole number at {}", show(path))),
        }
    };
    let date = |k: &str| [Key::s("date"), Key::s(k)];
    let start = Start {
        year: i32::try_from(num(&date("Year"))?).map_err(|e| e.to_string())?,
        month: u32::try_from(num(&date("Month"))?).map_err(|e| e.to_string())?,
        day: u32::try_from(num(&date("Day"))?).map_err(|e| e.to_string())?,
        seconds: u32::try_from(num(&[Key::s("start_time")])?).map_err(|e| e.to_string())?,
    };
    if start.seconds >= 86_400 || !(1..=12).contains(&start.month) || !(1..=31).contains(&start.day)
    {
        return Err(format!("not a mission start: {start:?}"));
    }
    Ok(start)
}

/// Write a shell from `template`.
pub fn write(template: &[u8], rt: &Runtime) -> Result<Vec<u8>, String> {
    let mut m = members(template)?;
    for name in REQUIRED {
        member(&m, name)?;
    }
    let mission = edit_mission(member(&m, "mission")?, rt)?;
    let warehouses = edit_warehouses(member(&m, "warehouses")?, &rt.dynamic)?;
    for (name, data) in &mut m {
        match name.as_str() {
            "mission" => *data = mission.clone(),
            "warehouses" => *data = warehouses.clone(),
            _ => {}
        }
    }
    let mut w = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in &m {
        w.start_file(name.as_str(), options)
            .map_err(|e| e.to_string())?;
        w.write_all(data).map_err(|e| e.to_string())?;
    }
    Ok(w.finish().map_err(|e| e.to_string())?.into_inner())
}

fn parse_named(src: &[u8], name: &str) -> Result<lua::Doc, String> {
    let d = lua::parse(src).map_err(|e| format!("`{name}`: {e}"))?;
    if d.name != name || !d.root.table {
        return Err(format!("`{name}` does not assign a table to `{name}`"));
    }
    Ok(d)
}

fn edit_mission(src: &[u8], rt: &Runtime) -> Result<Vec<u8>, String> {
    let d = parse_named(src, "mission")?;
    let scalar = |path: &[Key]| -> Result<std::ops::Range<usize>, String> {
        match d.root.at(path) {
            Some(n) if !n.table => Ok(n.span.clone()),
            _ => Err(format!("`mission` has no value at {}", show(path))),
        }
    };
    let date = |k: &str| [Key::s("date"), Key::s(k)];
    let spans = [
        scalar(&date("Day"))?,
        scalar(&date("Month"))?,
        scalar(&date("Year"))?,
        scalar(&[Key::s("start_time")])?,
    ];
    let weather = match d.root.get(&Key::s("weather")) {
        Some(n) if n.table => n.span.clone(),
        _ => return Err("`mission` has no weather table".into()),
    };
    let mut edits = Vec::new();
    if let Some(s) = rt.start {
        if s.seconds >= 86_400 || !(1..=12).contains(&s.month) || !(1..=31).contains(&s.day) {
            return Err(format!("not a mission start: {s:?}"));
        }
        let values = [
            i64::from(s.day),
            i64::from(s.month),
            i64::from(s.year),
            i64::from(s.seconds),
        ];
        for (span, v) in spans.into_iter().zip(values) {
            edits.push(Edit {
                span,
                text: v.to_string().into_bytes(),
            });
        }
    }
    if let Some(w) = &rt.weather {
        if !matches!(w, Lv::Table(_)) {
            return Err("the weather is not a table".into());
        }
        let text = lua::write(w, 1)?;
        edits.push(Edit {
            span: weather,
            text: text.trim_start_matches(['\n', '\t']).as_bytes().to_vec(),
        });
    }
    lua::apply(src, edits)
}

fn edit_warehouses(src: &[u8], dynamic: &[DynamicSpawn]) -> Result<Vec<u8>, String> {
    let d = parse_named(src, "warehouses")?;
    let Some(airports) = d.root.get(&Key::s("airports")).filter(|n| n.table) else {
        return Err("`warehouses` has no airports table".into());
    };
    let mut edits = Vec::new();
    let mut seen = Vec::new();
    for f in dynamic {
        if seen.contains(&f.airbase) {
            return Err(format!("airport {} is named twice", f.airbase));
        }
        seen.push(f.airbase);
        let Some(a) = airports
            .get(&Key::Num(f.airbase as f64))
            .filter(|n| n.table)
        else {
            return Err(format!("`warehouses` has no airport {}", f.airbase));
        };
        // Inserted fields go on their own line, one tab deeper than the
        // line that opens the airport's table.
        let line_start = src[..a.span.start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1);
        let indent: String = src[line_start..a.span.start]
            .iter()
            .take_while(|&&b| b == b'\t' || b == b' ')
            .map(|&b| b as char)
            .collect();
        // An enabled airport's aircraft are limited, so the stock the
        // controller sets is what dynamic spawn offers: an unlimited airport
        // lists no aircraft in the file (ED-CODE, me_mission.lua
        // `unloadAirportsEquipment`) and ignores a set (LIVE, E6). A disabled
        // airport keeps the template's flag.
        let mut keys = vec![("dynamicSpawn", f.enabled), ("allowHotStart", f.hot_start)];
        if f.enabled {
            keys.push(("unlimitedAircrafts", false));
        }
        for (key, v) in keys {
            let text = if v { "true" } else { "false" };
            match a.get(&Key::s(key)) {
                Some(n) if !n.table => edits.push(Edit {
                    span: n.span.clone(),
                    text: text.as_bytes().to_vec(),
                }),
                Some(_) => return Err(format!("airport {}: `{key}` is a table", f.airbase)),
                None => edits.push(Edit {
                    span: a.span.start + 1..a.span.start + 1,
                    text: format!("\n{indent}\t[\"{key}\"] = {text},").into_bytes(),
                }),
            }
        }
        // A limited airport needs an aircraft table to hold the stock; the
        // editor leaves it out for an unlimited one. An existing table is
        // kept as it is.
        if f.enabled && a.get(&Key::s("aircrafts")).is_none() {
            edits.push(Edit {
                span: a.span.start + 1..a.span.start + 1,
                text: format!("\n{indent}\t[\"aircrafts\"] = {{}},").into_bytes(),
            });
        }
    }
    lua::apply(src, edits)
}

fn show(path: &[Key]) -> String {
    path.iter()
        .map(|k| match k {
            Key::Str(s) => String::from_utf8_lossy(s).into_owned(),
            Key::Num(n) => n.to_string(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The folder under `[missions] dir` that holds the shells.
pub const SHELLS: &str = "shells";

/// A campaign's shell file name in slot `slot` (`a` or `b`).
pub fn shell_name(campaign: u64, slot: u8) -> String {
    format!("{campaign}.{}.miz", char::from(slot))
}

/// The slot a shell file name names, if it is one of `campaign`'s.
pub fn slot_of(campaign: u64, path: &str) -> Option<u8> {
    b"ab".iter().copied().find(|&s| {
        let name = shell_name(campaign, s);
        path.strip_suffix(&name)
            .is_some_and(|rest| rest.ends_with(['/', '\\']))
    })
}

/// The other slot.
pub fn other_slot(slot: Option<u8>) -> u8 {
    if slot == Some(b'a') { b'b' } else { b'a' }
}

/// Write `bytes` as campaign `campaign`'s shell in `slot` under `dir`: to
/// a temporary file in `<dir>/shells`, flushed, then renamed over the
/// shell. Writes nothing outside `dir`. Returns the shell's path.
pub fn save(dir: &Path, campaign: u64, slot: u8, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let shells = dir.join(SHELLS);
    std::fs::create_dir_all(&shells)?;
    let name = shell_name(campaign, slot);
    let path = shells.join(&name);
    let tmp = shells.join(format!(".{name}{TMP}"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The suffix of a shell being written.
const TMP: &str = ".tmp";

/// Remove the temporary files a stopped sidecar left in `<dir>/shells`.
pub fn clear_temps(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir.join(SHELLS)) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name.ends_with(TMP) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Every `name` in the `coalition` table of a template's `mission`: the
/// names of the groups, units and statics the author placed.
pub fn placed_names(bytes: &[u8]) -> Result<Vec<String>, String> {
    let m = members(bytes)?;
    let src = member(&m, "mission")?;
    let d = parse_named(src, "mission")?;
    let Some(c) = d.root.get(&Key::s("coalition")) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    collect_names(src, c, &mut out);
    Ok(out)
}

fn collect_names(src: &[u8], n: &lua::Node, out: &mut Vec<String>) {
    for (k, v) in &n.fields {
        if v.table {
            collect_names(src, v, out);
        } else if *k == Key::s("name")
            && let Some(Lv::String(b)) = lua::scalar_value(src, v)
        {
            out.push(String::from_utf8_lossy(&b).into_owned());
        }
    }
}

/// The airport IDs in a template's `warehouses` member.
pub fn airports(bytes: &[u8]) -> Result<Vec<u64>, String> {
    let m = members(bytes)?;
    let src = member(&m, "warehouses")?;
    let d = parse_named(src, "warehouses")?;
    let Some(a) = d.root.get(&Key::s("airports")).filter(|n| n.table) else {
        return Err("`warehouses` has no airports table".into());
    };
    Ok(a.fields
        .iter()
        .filter_map(|(k, _)| match k {
            Key::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests;
