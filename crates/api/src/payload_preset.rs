//! A payload preset of the DCS Mission Editor, converted to a unit's
//! `extra.payload.pylons` (spec/world.md, "Payload presets").
//!
//! DCS ships one preset file per aircraft type. Most are one statement
//! `local unitPayloads = {...}` of literal values followed by
//! `return unitPayloads`; some are Lua code (ED-CODE,
//! docs/results/2026-11-24-payload-presets.md). The file is read as data
//! by `lua::parse_returned`. No code from it runs: a file in any other form
//! is refused whole.

use std::collections::{BTreeMap, BTreeSet};

use nanoprobe_sidecar::shell::lua::{self, Doc, Key, Node};
use nanoprobe_sidecar::store::check_lv;
use nanoprobe_transport::codec::Lv;
use prost_types::{Struct, Value, value::Kind};

use crate::lua::{from_lua, to_lua};

/// The largest preset file that is parsed: the cap of the template readers
/// (`group_template::FILE_MAX`), which bounds the parsed tree at 36 MiB.
/// The largest file Eagle Dynamics ships is about 220 KB.
pub const FILE_MAX: usize = 1024 * 1024;

/// What the refusal of a larger file calls it.
pub const FILE_WHAT: &str = "a payload preset file";

/// Dropped key names kept for the log line.
const DROPPED_MAX: usize = 16;

/// The largest pylon number taken. DCS numbers an aircraft's pylons from 1;
/// no shipped aircraft has more than a few dozen.
const PYLON_MAX: f64 = 1000.0;

/// Why a preset is refused: the request field it is about, below the
/// unit's `payload_preset`, and the message.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// `path` for the file, `name` for the preset in it.
    pub field: &'static str,
    /// The message.
    pub text: String,
}

fn file(text: impl Into<String>) -> Refusal {
    Refusal {
        field: "path",
        text: text.into(),
    }
}

fn preset(text: impl Into<String>) -> Refusal {
    Refusal {
        field: "name",
        text: text.into(),
    }
}

/// A parsed preset file.
#[derive(Debug)]
pub struct File {
    src: Vec<u8>,
    doc: Doc,
    /// The DCS type the presets are for: the file's `unitType`, else its
    /// `name`, as the editor takes it.
    pub unit_type: String,
    /// The place in `payloads` of every preset with a string `name`, by
    /// name, so a lookup does not scan the file.
    names: BTreeMap<String, Vec<usize>>,
}

/// One preset's pylons, in the form `extra.payload.pylons` takes.
#[derive(Clone, Debug, PartialEq)]
pub struct Pylons {
    /// Keyed by the pylon number in decimal, each `{CLSID, settings?}`.
    pub pylons: Struct,
    /// Keys of the preset that the conversion does not use, for the log:
    /// the first `DROPPED_MAX` distinct names met, sorted.
    pub dropped: Vec<String>,
}

/// The last field under a string key, which is the one Lua keeps when a
/// table constructor names a key twice.
fn last<'a>(n: &'a Node, key: &str) -> Option<&'a Node> {
    n.fields
        .iter()
        .rev()
        .find(|(k, _)| matches!(k, Key::Str(s) if s == key.as_bytes()))
        .map(|(_, v)| v)
}

fn string(src: &[u8], n: Option<&Node>) -> Option<String> {
    match lua::scalar_value(src, n?) {
        Some(Lv::String(s)) => String::from_utf8(s).ok(),
        _ => None,
    }
}

/// A key for the log: printable, and short.
fn shown(prefix: &str, k: &Key) -> String {
    let name = match k {
        Key::Str(s) => String::from_utf8_lossy(&s[..s.len().min(40)]).into_owned(),
        Key::Num(n) => format!("[{n}]"),
    };
    format!("{prefix}{}", name.escape_debug())
}

/// The first key a table constructor names twice, as the log shows it. Lua
/// keeps only the later entry, and which one the author meant is not
/// knowable, so such a table is refused.
fn twice(n: &Node) -> Option<String> {
    let mut seen = BTreeSet::new();
    n.fields.iter().find_map(|(k, _)| {
        let id = match k {
            // 0 and -0 are one key in Lua.
            Key::Num(x) => (0u8, (x + 0.0).to_bits().to_be_bytes().to_vec()),
            Key::Str(s) => (1u8, s.clone()),
        };
        (!seen.insert(id)).then(|| shown("", k))
    })
}

fn note_unused(dropped: &mut BTreeSet<String>, prefix: &str, n: &Node, used: &[&str]) {
    for (k, _) in &n.fields {
        let known = matches!(k, Key::Str(s) if used.iter().any(|u| u.as_bytes() == s));
        if !known && dropped.len() < DROPPED_MAX {
            dropped.insert(shown(prefix, k));
        }
    }
}

const NOT_DATA: &str = "the file is not a payload preset file in the data form \
     `local <name> = {...}` then `return <name>`";

/// Read the bytes of a preset file.
///
/// The caller reads at most `FILE_MAX` bytes of a file, and refuses a
/// larger one by its size (`template_file::read_for`). The check here keeps
/// the reader's memory bounded for any caller that did not.
pub fn read(src: Vec<u8>) -> Result<File, Refusal> {
    if src.len() > FILE_MAX {
        return Err(file(format!(
            "the file is not a payload preset file: it has over {FILE_MAX} bytes"
        )));
    }
    let doc = lua::parse_returned(&src).map_err(|e| {
        file(format!(
            "{NOT_DATA}: {e}; a file with Lua code is not read: save the preset from the \
             Mission Editor, or write its pylons in `extra.payload.pylons`"
        ))
    })?;
    if !doc.root.table || !last(&doc.root, "payloads").is_some_and(|p| p.table) {
        return Err(file(format!("{NOT_DATA}: it has no `payloads` table")));
    }
    let unit_type = match last(&doc.root, "unitType") {
        Some(n) => string(&src, Some(n)),
        None => string(&src, last(&doc.root, "name")),
    }
    .filter(|s| !s.is_empty())
    .ok_or_else(|| {
        file(format!(
            "{NOT_DATA}: it has no string `unitType` or `name`, the DCS type of its presets"
        ))
    })?;
    let payloads = last(&doc.root, "payloads").expect("checked above");
    if let Some(k) = twice(payloads) {
        return Err(file(format!(
            "{NOT_DATA}: `payloads` has two entries under {k}"
        )));
    }
    let mut names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (_, p)) in payloads.fields.iter().enumerate() {
        if let Some(n) = p.table.then(|| string(&src, last(p, "name"))).flatten() {
            names.entry(n).or_default().push(i);
        }
    }
    Ok(File {
        src,
        doc,
        unit_type,
        names,
    })
}

impl File {
    /// The pylons of the preset `name` for a unit of the DCS type
    /// `unit_type`.
    pub fn pylons(&self, unit_type: &str, name: &str) -> Result<Pylons, Refusal> {
        let src = self.src.as_slice();
        if unit_type != self.unit_type {
            return Err(file(format!(
                "the file's presets are for the type {:?}; the unit's type is {unit_type:?}",
                self.unit_type
            )));
        }
        if name.is_empty() {
            return Err(preset("a preset name is required"));
        }
        let payloads = last(&self.doc.root, "payloads").expect("checked by read");
        let p = match self.names.get(name).map(Vec::as_slice) {
            None | Some([]) => {
                return Err(preset(format!("the file has no preset named {name:?}")));
            }
            Some([i]) => &payloads.fields[*i].1,
            Some(_) => {
                return Err(preset(format!(
                    "the file has more than one preset named {name:?}"
                )));
            }
        };
        let mut dropped = BTreeSet::new();
        note_unused(&mut dropped, "", p, &["name", "pylons"]);
        let rows = match last(p, "pylons") {
            Some(n) if n.table => n.fields.as_slice(),
            _ => {
                return Err(preset(format!("the preset {name:?} has no `pylons` table")));
            }
        };
        if let Some(k) = twice(last(p, "pylons").expect("a table")) {
            return Err(preset(format!(
                "`pylons` of the preset {name:?} has two entries under {k}"
            )));
        }
        let mut pylons = Struct::default();
        for (i, (_, row)) in rows.iter().enumerate() {
            let at = i + 1;
            let bad =
                |what: &str| preset(format!("pylon entry {at} of the preset {name:?} {what}"));
            if !row.table {
                return Err(bad("is not a table"));
            }
            let clsid = string(src, last(row, "CLSID"))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("has no string `CLSID`"))?;
            let num = match last(row, "num").and_then(|n| lua::scalar_value(src, n)) {
                Some(Lv::Number(n)) if n.fract() == 0.0 && (1.0..=PYLON_MAX).contains(&n) => {
                    n as u32
                }
                _ => {
                    return Err(bad(&format!(
                        "has no `num`, a pylon number from 1 to {PYLON_MAX}"
                    )));
                }
            };
            let key = num.to_string();
            if pylons.fields.contains_key(&key) {
                return Err(preset(format!(
                    "the preset {name:?} has two entries for pylon {num}"
                )));
            }
            let mut out = Struct::default();
            out.fields.insert(
                "CLSID".into(),
                Value {
                    kind: Some(Kind::StringValue(clsid)),
                },
            );
            if let Some(s) = last(row, "settings") {
                out.fields
                    .insert("settings".into(), settings(src, s).map_err(|e| bad(&e))?);
            }
            note_unused(
                &mut dropped,
                "pylons[].",
                row,
                &["CLSID", "num", "settings"],
            );
            pylons.fields.insert(
                key,
                Value {
                    kind: Some(Kind::StructValue(out)),
                },
            );
        }
        Ok(Pylons {
            pylons,
            dropped: dropped.into_iter().collect(),
        })
    }
}

/// A pylon's `settings` as the API carries it, when the store can hold it
/// and it comes back from the API form unchanged.
fn settings(src: &[u8], n: &Node) -> Result<Value, String> {
    if !n.table {
        return Err("has `settings` that is not a table".into());
    }
    let v = lua::value(src, n).ok_or("has `settings` that cannot be read")?;
    // Lua keeps no key whose value is nil, so a `nil` in the file is a key
    // DCS would never see; which the author meant is not knowable.
    fn has_nil(v: &Lv) -> bool {
        match v {
            Lv::Nil => true,
            Lv::Table(pairs) => pairs.iter().any(|(k, x)| has_nil(k) || has_nil(x)),
            _ => false,
        }
    }
    if has_nil(&v) {
        return Err("has `settings` holding `nil`".into());
    }
    check_lv(&v).map_err(|e| format!("has `settings` that cannot be stored: {e}"))?;
    let api = from_lua(&v);
    if canon(to_lua(&api)) != canon(v) {
        return Err("has `settings` that the API cannot carry unchanged".into());
    }
    Ok(api)
}

/// A Lua value with every table's pairs sorted by key, so two values that
/// Lua holds alike compare equal.
fn canon(v: Lv) -> Lv {
    fn rank(k: &Lv) -> u8 {
        match k {
            Lv::Number(_) => 0,
            Lv::String(_) => 1,
            _ => 2,
        }
    }
    match v {
        Lv::Table(pairs) => {
            let mut pairs: Vec<(Lv, Lv)> = pairs
                .into_iter()
                .map(|(k, x)| (canon(k), canon(x)))
                .collect();
            pairs.sort_by(|(a, _), (b, _)| {
                rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
                    (Lv::Number(x), Lv::Number(y)) => x.total_cmp(y),
                    (Lv::String(x), Lv::String(y)) => x.cmp(y),
                    _ => format!("{a:?}").cmp(&format!("{b:?}")),
                })
            });
            Lv::Table(pairs)
        }
        other => other,
    }
}

#[cfg(test)]
pub(crate) mod tests;
