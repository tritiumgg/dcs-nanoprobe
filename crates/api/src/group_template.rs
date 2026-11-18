//! A group template of the DCS Mission Editor, converted to a `GroupSpec`
//! (spec/world.md, "Group templates").
//!
//! DCS ships its group templates in one Lua file, `templates.lua`: one
//! assignment `templates = { ["<name>"] = {...}, ... }` of literal values
//! (ED-CODE, docs/results/2026-11-18-spawn-template.md). That a file the
//! editor saves for a user has the same form is UNVERIFIED. The file is
//! read as data, by the reader that reads a mission template's members
//! (`nanoprobe_sidecar::shell::lua`). No code from it runs: a file with any
//! other statement is refused.

use std::collections::BTreeSet;

use nanoprobe_proto::v1alpha1 as pb;
use nanoprobe_sidecar::shell::lua::{self, Key, Node};
use nanoprobe_transport::codec::Lv;

/// The largest group template file that is parsed. The reader keeps a node
/// of 72 bytes for every value. Files built to cost the most, of the
/// shortest values, of the shortest tables and of tables nested as deep as
/// the reader goes, each make a tree of at most 36 times their size: 36 MiB
/// at this cap, and about 600 MiB at the 16 MiB of a mission template (the
/// test `the_parsed_tree_is_bounded_at_the_cap` measures them). The file
/// Eagle Dynamics ships is under a fifth of this.
pub const FILE_MAX: usize = 1024 * 1024;

/// What the refusal of a larger file calls it.
pub const FILE_WHAT: &str = "a group template file";

/// Dropped key names kept for the log line.
const DROPPED_MAX: usize = 16;

/// Why a file or a template is refused: the request field it is about and
/// the message.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// `path` for the file, `template` for the template in it.
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

fn template(text: impl Into<String>) -> Refusal {
    Refusal {
        field: "template",
        text: text.into(),
    }
}

/// One unit of a template.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// The DCS type, the file's `name`.
    pub type_name: String,
    /// The file's `dx`.
    pub dx: f64,
    /// The file's `dy`.
    pub dy: f64,
    /// Radians.
    pub heading: f64,
    /// The file's `skill`, empty when it has none.
    pub skill: String,
}

/// One template of a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    /// Its name, the key it is under.
    pub name: String,
    /// The DCS country number, when the file gives one.
    pub country: Option<u32>,
    /// Its units, in the file's order.
    pub units: Vec<Unit>,
    /// Keys of the file that the conversion does not use, for the log: the
    /// first `DROPPED_MAX` distinct names met, sorted.
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

fn number(src: &[u8], n: Option<&Node>) -> Option<f64> {
    match lua::scalar_value(src, n?) {
        Some(Lv::Number(v)) if v.is_finite() => Some(v),
        _ => None,
    }
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

fn note_unused(dropped: &mut BTreeSet<String>, prefix: &str, n: &Node, used: &[&str]) {
    for (k, _) in &n.fields {
        let known = matches!(k, Key::Str(s) if used.iter().any(|u| u.as_bytes() == s));
        if !known && dropped.len() < DROPPED_MAX {
            dropped.insert(shown(prefix, k));
        }
    }
}

/// Read the template `name` from the bytes of a group template file.
///
/// The caller reads at most `FILE_MAX` bytes of a file, and refuses a
/// larger one by its size (`template_file::read_for`). The check here keeps
/// the reader's memory bounded for any caller that did not, and words its
/// refusal differently, so a test can tell which of the two refused.
pub fn read(src: &[u8], name: &str) -> Result<Template, Refusal> {
    if src.len() > FILE_MAX {
        return Err(file(format!(
            "the file is not a group template file: it has over {FILE_MAX} bytes"
        )));
    }
    let doc =
        lua::parse(src).map_err(|e| file(format!("the file is not a group template file: {e}")))?;
    if doc.name != "templates" || !doc.root.table {
        return Err(file(
            "the file is not a group template file: it does not assign a table to `templates`",
        ));
    }
    if name.is_empty() {
        return Err(template("a template name is required"));
    }
    let t = last(&doc.root, name)
        .ok_or_else(|| template(format!("the file has no template named {name:?}")))?;
    if !t.table {
        return Err(template(format!("the template {name:?} is not a table")));
    }
    match string(src, last(t, "type")).as_deref() {
        Some("vehicle") => {}
        _ => {
            return Err(template(format!(
                "the template {name:?} does not have the type \"vehicle\", the only one built"
            )));
        }
    }
    let country = number(src, last(t, "country"))
        .filter(|n| n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(n))
        .map(|n| n as u32);
    let mut dropped = BTreeSet::new();
    note_unused(&mut dropped, "", t, &["type", "name", "country", "units"]);

    let list = last(t, "units").filter(|u| u.table);
    let mut rows: Vec<(f64, &Node)> = Vec::new();
    for (k, v) in list.map(|u| u.fields.as_slice()).unwrap_or_default() {
        match k {
            Key::Num(n) => rows.push((*n, v)),
            Key::Str(_) => {
                if dropped.len() < DROPPED_MAX {
                    dropped.insert(shown("units.", k));
                }
            }
        }
    }
    if rows.is_empty() {
        return Err(template(format!("the template {name:?} has no units")));
    }
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut units = Vec::with_capacity(rows.len());
    for (i, (key, u)) in rows.iter().enumerate() {
        let at = i + 1;
        if *key != at as f64 || !u.table {
            return Err(template(format!(
                "the units of the template {name:?} are not tables numbered 1 to {}",
                rows.len()
            )));
        }
        let type_name = string(src, last(u, "name"))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                template(format!(
                    "unit {at} of the template {name:?} has no `name`, its DCS type"
                ))
            })?;
        let finite = |key: &str| {
            number(src, last(u, key)).ok_or_else(|| {
                template(format!(
                    "`{key}` of unit {at} of the template {name:?} is not a finite number"
                ))
            })
        };
        units.push(Unit {
            type_name,
            dx: finite("dx")?,
            dy: finite("dy")?,
            heading: finite("heading")?,
            skill: string(src, last(u, "skill")).unwrap_or_default(),
        });
        note_unused(
            &mut dropped,
            "units[].",
            u,
            &["dx", "dy", "name", "skill", "heading"],
        );
    }
    Ok(Template {
        name: name.to_owned(),
        country,
        units,
        dropped: dropped.into_iter().collect(),
    })
}

/// A template's offset as meters north and east of its origin.
///
/// The Mission Editor saves a unit's `dx` as its mission `x` less the
/// group's, and `dy` as its `y` less the group's (ED-CODE,
/// docs/results/2026-11-18-spawn-template.md). A unit table's `x` and `y`
/// are the world's `x` and `z` (LIVE,
/// docs/results/2026-11-15-s18-persistence-live.md), which the API names
/// `north` and `east`. Every placement goes through this function.
fn north_east(dx: f64, dy: f64) -> (f64, f64) {
    (dx, dy)
}

/// The spec of a group made from `t` with its origin at `anchor`.
///
/// `heading`, radians clockwise from north, turns the layout about the
/// anchor and is added to each unit's heading, the sum taken within a full
/// turn as the editor's own turn takes it (ED-CODE, the same document); at
/// 0 every value is the file's own. Every unit is on the anchor's theater at the anchor's
/// altitude. An empty `label` takes the template's name. `country` is the
/// request's; without one the template's own is used.
pub fn spec(
    t: &Template,
    anchor: &pb::Position,
    heading: f64,
    country: Option<u32>,
    label: &str,
) -> Result<pb::GroupSpec, Refusal> {
    let country = country.or(t.country).ok_or_else(|| {
        template(format!(
            "the template {:?} has no country number; give `country`",
            t.name
        ))
    })?;
    let (sin, cos) = heading.sin_cos();
    let mut units = Vec::with_capacity(t.units.len());
    for (i, u) in t.units.iter().enumerate() {
        let (n, e) = north_east(u.dx, u.dy);
        // At heading 0 the offsets are the file's, bit for bit.
        let (n, e) = if heading == 0.0 {
            (n, e)
        } else {
            (n * cos - e * sin, n * sin + e * cos)
        };
        let at = pb::Position {
            theater: anchor.theater.clone(),
            north: anchor.north + n,
            east: anchor.east + e,
            alt: anchor.alt,
        };
        let turned = if heading == 0.0 {
            u.heading
        } else {
            (u.heading + heading) % std::f64::consts::TAU
        };
        if !(at.north.is_finite() && at.east.is_finite() && turned.is_finite()) {
            return Err(template(format!(
                "unit {} of the template {:?} is not at a finite place from the anchor",
                i + 1,
                t.name
            )));
        }
        units.push(pb::UnitSpec {
            r#type: u.type_name.clone(),
            position: Some(at),
            heading: turned,
            skill: u.skill.clone(),
            ..pb::UnitSpec::default()
        });
    }
    Ok(pb::GroupSpec {
        label: if label.is_empty() {
            t.name.clone()
        } else {
            label.to_owned()
        },
        country,
        category: pb::GroupCategory::Ground as i32,
        units,
        ..pb::GroupSpec::default()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::f64::consts::{FRAC_PI_2, PI};

    use proptest::prelude::*;

    use super::*;

    /// A file in the form the Mission Editor writes, made for these tests.
    pub(crate) const FILE: &str = r#"templates =
{
    ["Test Battery"] =
    {
        ["type"] = "vehicle",
        ["name"] = "Test Battery",
        ["country"] = 34,
        ["units"] =
        {
            [1] =
            {
                ["dx"] = 0,
                ["dy"] = 0,
                ["name"] = "Type A",
                ["skill"] = "High",
                ["heading"] = 4.7123889803847,
            }, -- end of [1]
            [2] =
            {
                ["dx"] = 100,
                ["dy"] = -25.5,
                ["name"] = "Type B",
                ["skill"] = "Average",
                ["heading"] = 0,
            }, -- end of [2]
        }, -- end of ["units"]
    }, -- end of ["Test Battery"]
    ["Zero Country"] =
    {
        ["type"] = "vehicle",
        ["name"] = "Zero Country",
        ["country"] = 0,
        ["units"] =
        {
            [1] =
            {
                ["dx"] = 300,
                ["dy"] = 0,
                ["name"] = "Type C",
                ["skill"] = "High",
                ["heading"] = 0,
            }, -- end of [1]
        }, -- end of ["units"]
    }, -- end of ["Zero Country"]
} -- end of templates
"#;

    fn anchor() -> pb::Position {
        pb::Position {
            theater: "Caucasus".into(),
            north: -280_000.0,
            east: 640_000.0,
            alt: 12.0,
        }
    }

    fn one(body: &str) -> String {
        format!("templates = {{ [\"t\"] = {body} }}")
    }

    fn refused(src: &str, name: &str) -> Refusal {
        read(src.as_bytes(), name).expect_err("refused")
    }

    #[test]
    fn reads_a_template_in_the_editors_form() {
        let t = read(FILE.as_bytes(), "Test Battery").expect("read");
        assert_eq!(t.name, "Test Battery");
        assert_eq!(t.country, Some(34));
        assert_eq!(
            t.units,
            vec![
                Unit {
                    type_name: "Type A".into(),
                    dx: 0.0,
                    dy: 0.0,
                    heading: 4.7123889803847,
                    skill: "High".into(),
                },
                Unit {
                    type_name: "Type B".into(),
                    dx: 100.0,
                    dy: -25.5,
                    heading: 0.0,
                    skill: "Average".into(),
                },
            ]
        );
        assert!(t.dropped.is_empty());
        assert_eq!(
            read(FILE.as_bytes(), "Zero Country").expect("zero").country,
            Some(0)
        );
    }

    #[test]
    fn places_units_at_their_offsets_from_the_anchor() {
        let t = read(FILE.as_bytes(), "Test Battery").expect("read");
        let s = spec(&t, &anchor(), 0.0, None, "").expect("spec");
        assert_eq!(s.label, "Test Battery");
        assert_eq!(s.country, 34);
        assert_eq!(s.category, pb::GroupCategory::Ground as i32);
        assert!(!s.r#virtual && s.extra.is_none() && s.components.is_empty());
        let at = |i: usize| s.units[i].position.clone().expect("position");
        assert_eq!(at(0), anchor());
        // `dx` is north and `dy` is east.
        assert_eq!(
            at(1),
            pb::Position {
                north: -280_000.0 + 100.0,
                east: 640_000.0 - 25.5,
                ..anchor()
            }
        );
        assert_eq!(s.units[0].heading, 4.7123889803847);
        assert_eq!(s.units[0].r#type, "Type A");
        assert_eq!(s.units[0].skill, "High");
        assert_eq!(s.units[1].skill, "Average");
        assert!(
            s.units
                .iter()
                .all(|u| u.id == 0 && u.label.is_empty() && u.extra.is_none())
        );
    }

    #[test]
    fn a_heading_turns_the_layout_and_each_unit() {
        let t = read(FILE.as_bytes(), "Test Battery").expect("read");
        // A quarter turn clockwise: what was north of the anchor is east.
        let s = spec(&t, &anchor(), FRAC_PI_2, None, "").expect("spec");
        let p = s.units[1].position.clone().expect("position");
        assert!((p.north - (anchor().north + 25.5)).abs() < 1e-6, "{p:?}");
        assert!((p.east - (anchor().east + 100.0)).abs() < 1e-6, "{p:?}");
        assert_eq!(p.alt, 12.0);
        // West turned a quarter is north: the sum stays within a full turn.
        assert!(s.units[0].heading.abs() < 1e-9, "{}", s.units[0].heading);
        assert_eq!(s.units[1].heading, FRAC_PI_2);
        // Half a turn puts it opposite, and turns west to east.
        let s = spec(&t, &anchor(), PI, None, "").expect("spec");
        assert!((s.units[0].heading - FRAC_PI_2).abs() < 1e-9);
        assert_eq!(s.units[1].heading, PI);
        let p = s.units[1].position.clone().expect("position");
        assert!((p.north - (anchor().north - 100.0)).abs() < 1e-6, "{p:?}");
        assert!((p.east - (anchor().east + 25.5)).abs() < 1e-6, "{p:?}");
    }

    #[test]
    fn a_turned_heading_is_the_remainder_of_a_full_turn() {
        let src = one(
            r#"{ type = "vehicle", country = 1, units = { { dx = 10, dy = 0, name = "A", heading = 0 }, { dx = 0, dy = 0, name = "A", heading = 7 } } }"#,
        );
        let t = read(src.as_bytes(), "t").expect("read");
        // Without a turn a heading over a full turn is the file's own.
        let s = spec(&t, &anchor(), 0.0, None, "").expect("spec");
        assert_eq!(s.units[1].heading, 7.0);
        // A turn left: the remainder keeps the sum's sign, as `math.mod`
        // does, and what was north of the anchor is west of it.
        let s = spec(&t, &anchor(), -FRAC_PI_2, None, "").expect("spec");
        assert_eq!(s.units[0].heading, -FRAC_PI_2);
        let p = s.units[0].position.clone().expect("position");
        assert!((p.north - anchor().north).abs() < 1e-6, "{p:?}");
        assert!((p.east - (anchor().east - 10.0)).abs() < 1e-6, "{p:?}");
        // More than a full turn right is the turn less the full one.
        let s = spec(&t, &anchor(), 5.0 * FRAC_PI_2, None, "").expect("spec");
        assert!((s.units[0].heading - FRAC_PI_2).abs() < 1e-9);
        let p = s.units[0].position.clone().expect("position");
        assert!((p.north - anchor().north).abs() < 1e-6, "{p:?}");
        assert!((p.east - (anchor().east + 10.0)).abs() < 1e-6, "{p:?}");
        // A turn left of more than a full turn stays negative.
        let s = spec(&t, &anchor(), -5.0 * FRAC_PI_2, None, "").expect("spec");
        assert!((s.units[0].heading + FRAC_PI_2).abs() < 1e-9);
        // A file heading of 7 turned a quarter right is 8.57, which is a
        // full turn and 2.29.
        let s = spec(&t, &anchor(), FRAC_PI_2, None, "").expect("spec");
        let turned = s.units[1].heading;
        assert!((turned - 2.287_611_019_615_31).abs() < 1e-9, "{turned}");
    }

    #[test]
    fn the_country_and_the_label_come_from_the_request_when_given() {
        let t = read(FILE.as_bytes(), "Test Battery").expect("read");
        assert_eq!(spec(&t, &anchor(), 0.0, Some(2), "").expect("s").country, 2);
        // 0 is a country, so it overrides too.
        assert_eq!(spec(&t, &anchor(), 0.0, Some(0), "").expect("s").country, 0);
        assert_eq!(
            spec(&t, &anchor(), 0.0, None, "mine").expect("s").label,
            "mine"
        );
        let none =
            one(r#"{ type = "vehicle", units = { { dx = 0, dy = 0, name = "A", heading = 0 } } }"#);
        let t = read(none.as_bytes(), "t").expect("read");
        assert_eq!(t.country, None);
        assert_eq!(t.units[0].skill, "");
        let r = spec(&t, &anchor(), 0.0, None, "").expect_err("no country");
        assert_eq!(r.field, "template");
        assert!(r.text.contains("has no country number"), "{}", r.text);
        assert_eq!(spec(&t, &anchor(), 0.0, Some(7), "").expect("s").country, 7);
        // A country that is not a whole number in range is none.
        for bad in ["-1", "1.5", "4294967296", "\"USA\""] {
            let src = one(&format!(
                r#"{{ type = "vehicle", country = {bad}, units = {{ {{ dx = 0, dy = 0, name = "A", heading = 0 }} }} }}"#
            ));
            assert_eq!(
                read(src.as_bytes(), "t").expect("read").country,
                None,
                "{bad}"
            );
        }
    }

    #[test]
    fn a_file_that_is_not_a_group_template_file_is_refused() {
        let unit = r#"{ dx = 0, dy = 0, name = "A", heading = 0 }"#;
        let body = format!(r#"{{ type = "vehicle", country = 1, units = {{ {unit} }} }}"#);
        // Each construct is on a line of its own, so the line in the
        // refusal shows which one the reader stopped at.
        let good = format!("templates =\n{{\n t = {body},\n}}\n");
        assert!(read(good.as_bytes(), "t").is_ok());
        for (src, want) in [
            (
                format!("-- a\n-- b\nlocal templates =\n{{ t = {body} }}"),
                "line 3: expected `=`",
            ),
            (
                format!("{good}return templates"),
                "line 5: expected the end after the value",
            ),
            (
                format!("{good}templates.u = 1"),
                "line 5: expected the end after the value",
            ),
            (
                format!("{good}os.execute('x')"),
                "line 5: expected the end after the value",
            ),
            (
                format!("templates =\n{{\n t = f({body}),\n}}"),
                "line 3: expected",
            ),
            (
                format!("templates =\n{{\n t = {body},\n u = function() end,\n}}"),
                "line 4: expected",
            ),
            (
                format!("templates =\n{{\n t = {body},\n u = 1 + 1,\n}}"),
                "line 4: expected",
            ),
            (
                format!("templates =\n{{\n t = {body},\n u = \"a\" .. \"b\",\n}}"),
                "line 4: expected",
            ),
            (
                format!("templates =\n{{\n t = {body},\n u = t,\n}}"),
                "line 4: expected",
            ),
            (
                format!(
                    "templates =\n{{\n t = {body},\n u = {}0{},\n}}",
                    "{".repeat(300),
                    "}".repeat(300)
                ),
                "line 4: expected",
            ),
            (format!("other = {{ t = {body} }}"), "does not assign"),
            ("templates = 5".to_owned(), "does not assign"),
            (String::new(), "line 1: expected an assignment"),
            ("\u{0}\u{1}".to_owned(), "line 1: expected an assignment"),
        ] {
            let r = refused(&src, "t");
            assert_eq!(r.field, "path", "{src}");
            assert!(
                r.text
                    .starts_with("the file is not a group template file: "),
                "{}",
                r.text
            );
            assert!(r.text.contains(want), "{src}: {}", r.text);
        }
    }

    #[test]
    fn a_file_over_the_cap_is_refused_before_it_is_parsed() {
        let mut src = FILE.as_bytes().to_vec();
        src.resize(FILE_MAX, b' ');
        assert!(read(&src, "Test Battery").is_ok());
        src.push(b' ');
        let r = read(&src, "Test Battery").expect_err("over");
        assert_eq!(r.field, "path");
        // Not the text of the file read's refusal, which a caller's file
        // gets first (`hub::tests::group_template`, case `r-big`).
        assert_eq!(
            r.text,
            format!("the file is not a group template file: it has over {FILE_MAX} bytes")
        );
    }

    #[test]
    fn a_template_the_conversion_cannot_use_is_refused() {
        let unit = r#"{ dx = 0, dy = 0, name = "A", heading = 0 }"#;
        let with = |units: &str| {
            one(&format!(
                r#"{{ type = "vehicle", country = 1, units = {units} }}"#
            ))
        };
        let cases: Vec<(String, &str, &str)> = vec![
            (
                with(&format!("{{ {unit} }}")),
                "",
                "a template name is required",
            ),
            (
                with(&format!("{{ {unit} }}")),
                "u",
                "the file has no template named \"u\"",
            ),
            (one("5"), "t", "is not a table"),
            (
                one(&format!(
                    r#"{{ type = "plane", country = 1, units = {{ {unit} }} }}"#
                )),
                "t",
                "does not have the type \"vehicle\"",
            ),
            (
                one(&format!(r#"{{ country = 1, units = {{ {unit} }} }}"#)),
                "t",
                "does not have the type \"vehicle\"",
            ),
            (with("{}"), "t", "has no units"),
            (with("5"), "t", "has no units"),
            (
                one(r#"{ type = "vehicle", country = 1 }"#),
                "t",
                "has no units",
            ),
            (with(&format!("{{ [2] = {unit} }}")), "t", "numbered 1 to 1"),
            (
                with(&format!("{{ {unit}, [3] = {unit} }}")),
                "t",
                "numbered 1 to 2",
            ),
            (
                with(&format!("{{ [1.5] = {unit} }}")),
                "t",
                "numbered 1 to 1",
            ),
            (with("{ 7 }"), "t", "numbered 1 to 1"),
            (
                with("{ { dx = 0, dy = 0, heading = 0 } }"),
                "t",
                "unit 1 of the template \"t\" has no `name`",
            ),
            (
                with(r#"{ { dx = 0, dy = 0, heading = 0, name = "" } }"#),
                "t",
                "has no `name`",
            ),
            (
                with(r#"{ { dx = 0, dy = 0, heading = 0, name = 5 } }"#),
                "t",
                "has no `name`",
            ),
            (
                with(r#"{ { dy = 0, heading = 0, name = "A" } }"#),
                "t",
                "`dx` of unit 1",
            ),
            (
                with(r#"{ { dx = "1", dy = 0, heading = 0, name = "A" } }"#),
                "t",
                "`dx` of unit 1",
            ),
            (
                with(r#"{ { dx = 0, dy = 1e999, heading = 0, name = "A" } }"#),
                "t",
                "`dy` of unit 1",
            ),
            (
                with(r#"{ { dx = 0, dy = 0, heading = {}, name = "A" } }"#),
                "t",
                "`heading` of unit 1",
            ),
            (
                with(&format!(
                    r#"{{ {unit}, {{ dx = 0, dy = 0, heading = 0/0, name = "A" }} }}"#
                )),
                "t",
                "line 1",
            ),
        ];
        for (src, name, want) in cases {
            let r = refused(&src, name);
            assert!(r.text.contains(want), "{src}: {}", r.text);
            let field = if want == "line 1" { "path" } else { "template" };
            assert_eq!(r.field, field, "{src}: {}", r.text);
        }
    }

    #[test]
    fn a_place_that_is_not_finite_is_refused() {
        let src = one(
            r#"{ type = "vehicle", country = 1, units = { { dx = 1e308, dy = 0, name = "A", heading = 0 } } }"#,
        );
        let t = read(src.as_bytes(), "t").expect("read");
        let far = pb::Position {
            north: 1e308,
            ..anchor()
        };
        let r = spec(&t, &far, 0.0, None, "").expect_err("overflow");
        assert_eq!(r.field, "template");
        assert!(r.text.contains("not at a finite place"), "{}", r.text);
    }

    #[test]
    fn names_are_ignored_unknown_keys_are_dropped_and_the_last_duplicate_wins() {
        let src = r#"templates = {
            ["t"] = { type = "plane" },
            ["t"] = {
                type = "vehicle", name = "np!abc1", country = 1, extra = { 1 }, [5] = true,
                units = {
                    { dx = 1, dx = 2, dy = 0, name = "A", heading = 0, skill = 3, payload = {} },
                    note = "x",
                },
            },
        }"#;
        let t = read(src.as_bytes(), "t").expect("read");
        assert_eq!(t.name, "t");
        assert_eq!(t.units.len(), 1);
        assert_eq!(t.units[0].dx, 2.0);
        // A skill that is not a string is no skill.
        assert_eq!(t.units[0].skill, "");
        assert_eq!(
            t.dropped,
            vec!["[5]", "extra", "units.note", "units[].payload"]
        );
        let s = spec(&t, &anchor(), 0.0, None, "").expect("spec");
        assert_eq!(s.label, "t");
        assert!(s.units[0].label.is_empty());
        // The log's list of dropped keys is bounded.
        let many: String = (0..100).map(|i| format!("k{i} = 1, ")).collect();
        let src = one(&format!(
            r#"{{ {many} type = "vehicle", units = {{ {{ dx = 0, dy = 0, name = "A", heading = 0 }} }} }}"#
        ));
        assert_eq!(
            read(src.as_bytes(), "t").expect("read").dropped.len(),
            DROPPED_MAX
        );
    }

    fn heap(n: &Node) -> usize {
        n.fields.capacity() * std::mem::size_of::<(Key, Node)>()
            + n.fields
                .iter()
                .map(|(k, v)| {
                    heap(v)
                        + match k {
                            Key::Str(s) => s.capacity(),
                            Key::Num(_) => 0,
                        }
                })
                .sum::<usize>()
    }

    /// The reader's tree for files at the cap, built to cost the most a
    /// byte in different ways: short values, short tables, and tables
    /// nested as deep as the reader goes. The largest bounds what a hostile
    /// file under the cap can make the sidecar hold.
    #[test]
    fn the_parsed_tree_is_bounded_at_the_cap() {
        let chain = format!("{}0{},", "{".repeat(190), "}".repeat(190));
        let mut worst = (0, "");
        for piece in ["0,", "{},", "{0},", "{{0}},", "a=0,", chain.as_str()] {
            let mut src = b"templates={".to_vec();
            while src.len() + piece.len() < FILE_MAX {
                src.extend_from_slice(piece.as_bytes());
            }
            src.push(b'}');
            let doc = lua::parse(&src).expect("parse");
            let bytes = heap(&doc.root);
            eprintln!(
                "group template cap: {:>12} repeated to {} bytes of source makes {bytes} bytes of tree, {} a source byte",
                &piece[..piece.len().min(12)],
                src.len(),
                bytes / src.len()
            );
            if bytes > worst.0 {
                worst = (bytes, piece);
            }
            // It still reads as a file with no such template.
            assert_eq!(read(&src, "t").expect_err("no template").field, "template");
        }
        assert!(worst.0 <= TREE_MAX, "{worst:?}");
    }

    /// The most the tree of a file at the cap may take: 37,748,304 bytes is
    /// the largest measured, of 72 bytes for each 2 bytes of source.
    const TREE_MAX: usize = 40 * 1024 * 1024;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Any bytes: the reader answers and never panics, and what it
        /// accepts is a template whose numbers are finite.
        #[test]
        fn arbitrary_bytes_are_read_or_refused(
            src in proptest::collection::vec(any::<u8>(), 0..400),
            name in ".{0,8}",
        ) {
            if let Ok(t) = read(&src, &name) {
                prop_assert!(!t.units.is_empty());
                prop_assert!(t.units.iter().all(|u| u.dx.is_finite() && u.dy.is_finite() && u.heading.is_finite() && !u.type_name.is_empty()));
            }
        }

        /// Text built from the pieces of a template file, in any order:
        /// near-valid files, which random bytes almost never are.
        #[test]
        fn shuffled_template_text_is_read_or_refused(
            parts in proptest::collection::vec(
                prop_oneof![
                    Just("templates"), Just(" = "), Just("{"), Just("}"), Just(","),
                    Just("[\"t\"]"), Just("t"), Just("[1]"), Just("units"), Just("type"),
                    Just("\"vehicle\""), Just("country"), Just("dx"), Just("dy"),
                    Just("heading"), Just("name"), Just("skill"), Just("\"A\""),
                    Just("0"), Just("-1.5e3"), Just("1e999"), Just("0x10"), Just("nil"),
                    Just("true"), Just("--x\n"), Just("[[a]]"), Just("[==["), Just("'"),
                    Just("\""), Just("\\"), Just("local "), Just("return "), Just("f("),
                    Just(")"), Just(";"), Just(".."), Just("\n"),
                ],
                0..60,
            ),
        ) {
            let src = format!("templates = {{ t = {{ type = \"vehicle\", units = {{ {{ {} }} }} }} }}", parts.concat());
            if let Ok(t) = read(src.as_bytes(), "t") {
                let s = spec(&t, &anchor(), 1.0, Some(1), "");
                if let Ok(s) = s {
                    let finite = s.units.iter().all(|u| {
                        let p = u.position.as_ref().expect("position");
                        p.north.is_finite() && p.east.is_finite() && u.heading.is_finite()
                    });
                    prop_assert!(finite);
                }
            }
        }

        /// Whole templates built from generated parts: unit counts, plain
        /// and extreme numbers, the string forms and repeated keys. One of
        /// plain numbers is always read and always converts. One with an
        /// extreme number is read, or refused for that number; what is read
        /// converts to finite values, or is refused for its place.
        #[test]
        fn generated_templates_convert_or_are_refused_for_a_number(
            units in proptest::collection::vec(
                (
                    number(),
                    number(),
                    number(),
                    prop_oneof![Just("\"A\""), Just("'B'"), Just("[[C d]]"), Just("\"a\\\"b\\65\"")],
                    any::<bool>(),
                ),
                1..6,
            ),
            north in -1e6f64..1e6,
            east in -1e6f64..1e6,
            heading in prop_oneof![Just(0.0), -10.0f64..10.0],
        ) {
            let body: String = units
                .iter()
                .enumerate()
                .map(|(i, (dx, dy, h, name, twice))| {
                    format!(
                        "[{}] = {{ {}dx = {}, dy = {}, heading = {}, name = {name}, skill = \"High\" }},\n",
                        i + 1,
                        if *twice { "dx = 5, " } else { "" },
                        dx.0,
                        dy.0,
                        h.0,
                    )
                })
                .collect();
            let src = format!(
                "templates =\n{{\n[\"t\"] =\n{{ type = \"vehicle\", country = 3, units =\n{{\n{body}}} }} }}"
            );
            let plain = units.iter().all(|(dx, dy, h, _, _)| dx.1 && dy.1 && h.1);
            let at = pb::Position { north, east, ..anchor() };
            match read(src.as_bytes(), "t") {
                Ok(t) => {
                    prop_assert_eq!(t.units.len(), units.len());
                    if plain {
                        let same = t.units.iter().zip(&units).all(|(u, (dx, dy, h, _, _))| {
                            Ok(u.dx) == dx.0.parse::<f64>()
                                && Ok(u.dy) == dy.0.parse::<f64>()
                                && Ok(u.heading) == h.0.parse::<f64>()
                        });
                        prop_assert!(same, "{}", src);
                    }
                    match spec(&t, &at, heading, None, "") {
                        Ok(s) => {
                            let finite = s.units.iter().all(|u| {
                                let p = u.position.as_ref().expect("position");
                                p.north.is_finite() && p.east.is_finite() && u.heading.is_finite()
                            });
                            prop_assert!(finite, "{}", src);
                            prop_assert_eq!(s.country, 3);
                            prop_assert_eq!(s.units.len(), units.len());
                        }
                        Err(r) => {
                            prop_assert!(!plain, "{}: {}", src, r.text);
                            prop_assert_eq!(r.field, "template");
                        }
                    }
                }
                Err(r) => {
                    prop_assert!(!plain, "{}: {}", src, r.text);
                    let known = r.field == "template" || r.field == "path";
                    prop_assert!(known);
                }
            }
        }
    }

    /// A number as a template file writes one, and whether it is a plain
    /// decimal of ordinary size.
    fn number() -> impl Strategy<Value = (String, bool)> {
        prop_oneof![
            4 => (-1e6f64..1e6).prop_map(|v| (format!("{v}"), true)),
            2 => any::<i32>().prop_map(|v| (v.to_string(), true)),
            1 => prop_oneof![
                Just("1e308"),
                Just("-1e308"),
                Just("1.7e308"),
                Just("1e999"),
                Just("-1e999"),
                Just("1e-320"),
                Just("0x10"),
                Just("1e400000"),
            ]
            .prop_map(|s| (s.to_owned(), false)),
        ]
    }
}
