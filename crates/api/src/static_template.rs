//! A static template of the DCS Mission Editor, a `.stm` file, read as data
//! (spec/world.md, "Static templates").
//!
//! The editor saves a static template as one assignment
//! `staticTemplate = {...}` of literal values: the mission's own tree of
//! coalitions, countries, kinds and groups, every place an absolute mission
//! `x` and `y` on the file's `theatre` (ED-CODE,
//! docs/results/2026-11-23-static-templates.md). The file is read by the
//! reader that reads a mission template's members
//! (`nanoprobe_sidecar::shell::lua`). No code from it runs.

use std::collections::BTreeSet;

use nanoprobe_proto::v1alpha1 as pb;
use nanoprobe_sidecar::shell::lua::{self, Key, Node};
use nanoprobe_sidecar::store::check_lv;
use nanoprobe_transport::codec::Lv;

/// The largest static template file that is parsed. The reader's tree is
/// at most 36 times the file's size (`group_template::FILE_MAX`); the
/// largest file Eagle Dynamics ships is 147,286 bytes.
pub const FILE_MAX: usize = 1024 * 1024;

/// What the refusal of a larger file calls it.
pub const FILE_WHAT: &str = "a static template file";

/// Dropped key names kept for the log line.
const DROPPED_MAX: usize = 16;

/// The kinds of group the editor saves that are not built. A file holding
/// one is refused whole, as every entity of a file is made or none is.
const UNBUILT: &[&str] = &["plane", "helicopter", "ship", "complex"];

/// Keys of a vehicle unit the conversion reads or ignores without a log.
const UNIT_USED: &[&str] = &["type", "x", "y", "heading", "skill", "unitId", "name"];

/// Keys of a static's unit that go to its DCS static table unchanged.
const STATIC_PASSED: &[&str] = &[
    "category",
    "shape_name",
    "rate",
    "mass",
    "canCargo",
    "livery_id",
];

/// Keys of a `Heliports` static's unit that also go to its DCS static table
/// unchanged: its radio. `coalition.addStaticObject` reads the frequency as
/// a number or as text, the modulation and the callsign ID (LIVE,
/// docs/results/2026-11-25-farp-fixes.md). On a static of another category
/// they are dropped and logged.
const HELIPORT_PASSED: &[&str] = &[
    "heliport_frequency",
    "heliport_modulation",
    "heliport_callsign_id",
];

/// The static category that makes a FARP.
const HELIPORT_CATEGORY: &str = "Heliports";

/// Keys of a static's unit the conversion reads or ignores without a log.
const STATIC_USED: &[&str] = &["type", "x", "y", "heading", "unitId", "name"];

/// The static category of a grass airfield, and the key prefix only such a
/// static has (ED-CODE, the editor's `unload_static_groups`). Refused: no
/// live check has seen one make its airbase. A `Heliports` static is built
/// as `PutStatic` builds a FARP (spec/world.md, "Kinds"), with its radio
/// keys ([`HELIPORT_PASSED`]).
const GRASS_CATEGORY: &str = "GrassAirfields";
const GRASS_KEY_PREFIX: &str = "grassairfield_";

/// Why a file is refused: the request field it is about and the message.
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    /// The request field, `path` for everything the file holds.
    pub field: &'static str,
    /// The message.
    pub text: String,
}

fn refuse(text: impl Into<String>) -> Refusal {
    Refusal {
        field: "path",
        text: format!("the file is not a static template file: {}", text.into()),
    }
}

/// One unit of a vehicle group.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    /// The DCS type.
    pub type_name: String,
    /// The mission `x`, meters north.
    pub x: f64,
    /// The mission `y`, meters east.
    pub y: f64,
    /// Radians.
    pub heading: f64,
    /// Empty when the file has none, or one that is not text.
    pub skill: String,
}

/// One vehicle group.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The file's `name` for it, empty when it has none.
    pub name: String,
    /// The DCS country number of the country it is under.
    pub country: u32,
    /// Its units, in the file's order.
    pub units: Vec<Unit>,
}

/// One static object.
#[derive(Clone, Debug, PartialEq)]
pub struct Static {
    /// The file's `name` of the static's group, empty when it has none.
    pub name: String,
    /// The DCS country number of the country it is under.
    pub country: u32,
    /// The DCS type.
    pub type_name: String,
    /// The mission `x`, meters north.
    pub x: f64,
    /// The mission `y`, meters east.
    pub y: f64,
    /// Radians.
    pub heading: f64,
    /// The group's `dead`: created destroyed.
    pub dead: bool,
    /// Keys passed to the DCS static table, in the file's order.
    pub extra: Vec<(String, Lv)>,
    /// The further units of a heliport group, after this first one, in the
    /// file's order; empty for anything else.
    pub further: Vec<StaticUnit>,
}

/// A further unit of a heliport group.
#[derive(Clone, Debug, PartialEq)]
pub struct StaticUnit {
    /// The DCS type.
    pub type_name: String,
    /// The mission `x`, meters north.
    pub x: f64,
    /// The mission `y`, meters east.
    pub y: f64,
    /// Radians.
    pub heading: f64,
    /// Keys passed to its DCS unit table, in the file's order.
    pub extra: Vec<(String, Lv)>,
}

/// A static template file.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The file's `theatre`, when it is text.
    pub theatre: Option<String>,
    /// Its vehicle groups, in the file's order.
    pub groups: Vec<Group>,
    /// Its statics, in the file's order.
    pub statics: Vec<Static>,
    /// Keys the conversion does not use, for the log: the first
    /// `DROPPED_MAX` distinct names met, sorted.
    pub dropped: Vec<String>,
    /// The static groups that mix heliports with other statics, which are
    /// built one static per unit, for the log: where each is in the file.
    pub mixed: Vec<String>,
}

/// What the sidecar's log line adds about a file it read: the keys it did
/// not use, and the static groups that mix heliports with other statics,
/// built one static per unit (spec/world.md, "Static templates").
pub fn log_notes(l: &Layout) -> String {
    let mut out = String::new();
    if !l.dropped.is_empty() {
        out.push_str(&format!("; keys not used: {}", l.dropped.join(", ")));
    }
    if !l.mixed.is_empty() {
        out.push_str(&format!(
            "; static groups mixing heliports with other statics, one static per unit: {}",
            l.mixed.join(", ")
        ));
    }
    out
}

/// The last field under a string key, the one Lua keeps of two.
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

/// A key as text for messages and the log: printable and short.
fn shown(k: &Key) -> String {
    match k {
        Key::Str(s) => String::from_utf8_lossy(&s[..s.len().min(40)])
            .escape_debug()
            .to_string(),
        Key::Num(n) => format!("[{n}]"),
    }
}

struct Reader<'a> {
    src: &'a [u8],
    dropped: BTreeSet<String>,
    mixed: Vec<String>,
}

impl Reader<'_> {
    fn drop_key(&mut self, prefix: &str, k: &Key) {
        if self.dropped.len() < DROPPED_MAX {
            self.dropped.insert(format!("{prefix}{}", shown(k)));
        }
    }

    /// Note every key of `n` that is neither `used` nor `passed`, and
    /// return the passed ones with their values, in the file's order.
    fn sort_keys(
        &mut self,
        prefix: &str,
        n: &Node,
        used: &[&str],
        passed: &[&str],
        at: &str,
    ) -> Result<Vec<(String, Lv)>, Refusal> {
        let mut out: Vec<(String, Lv)> = Vec::new();
        for (k, v) in &n.fields {
            let name = match k {
                Key::Str(s) => std::str::from_utf8(s).ok(),
                Key::Num(_) => None,
            };
            match name {
                Some(s) if passed.contains(&s) => {
                    let v = lua::value(self.src, v)
                        .ok_or_else(|| refuse(format!("`{s}` of {at} is not a value")))?;
                    // The store refuses a value it cannot encode whole; it is
                    // refused here, with the place in the file, checked as
                    // the store holds it: under its key in the static's table.
                    let held = Lv::Table(vec![(Lv::String(s.as_bytes().to_vec()), v.clone())]);
                    check_lv(&held)
                        .map_err(|e| refuse(format!("`{s}` of {at} cannot be stored: {e}")))?;
                    // Of two fields under one key, Lua keeps the later.
                    out.retain(|(o, _)| o != s);
                    if v != Lv::Nil {
                        out.push((s.to_owned(), v));
                    }
                }
                Some(s) if used.contains(&s) => {}
                _ => self.drop_key(prefix, k),
            }
        }
        Ok(out)
    }

    /// The tables of a list, in the file's order. A string key is dropped;
    /// two fields under one number refuse the file.
    fn list<'n>(
        &mut self,
        prefix: &str,
        n: Option<&'n Node>,
        at: &str,
    ) -> Result<Vec<(f64, &'n Node)>, Refusal> {
        let Some(n) = n else {
            return Ok(Vec::new());
        };
        if !n.table {
            return Err(refuse(format!("{at} is not a table")));
        }
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for (k, v) in &n.fields {
            match k {
                Key::Num(i) => {
                    if seen.contains(i) {
                        return Err(refuse(format!("{at} has two entries [{i}]")));
                    }
                    seen.push(*i);
                    if !v.table {
                        return Err(refuse(format!("entry [{i}] of {at} is not a table")));
                    }
                    out.push((*i, v));
                }
                Key::Str(_) => self.drop_key(prefix, k),
            }
        }
        Ok(out)
    }

    fn finite(&self, n: &Node, key: &str, at: &str) -> Result<f64, Refusal> {
        number(self.src, last(n, key))
            .ok_or_else(|| refuse(format!("`{key}` of {at} is not a finite number")))
    }

    fn type_name(&self, n: &Node, at: &str) -> Result<String, Refusal> {
        string(self.src, last(n, "type"))
            .filter(|s| !s.is_empty())
            .ok_or_else(|| refuse(format!("{at} has no `type`, its DCS type")))
    }
}

/// Refuse a table that names one key twice: Lua would keep only the later,
/// and which of the two the author meant is not knowable.
fn unique(n: &Node, at: &str) -> Result<(), Refusal> {
    for (i, (k, _)) in n.fields.iter().enumerate() {
        if n.fields[i + 1..].iter().any(|(o, _)| o == k) {
            let name = match k {
                Key::Str(_) => format!("{:?}", shown(k)),
                Key::Num(_) => shown(k),
            };
            return Err(refuse(format!("{at} has two entries {name}")));
        }
    }
    Ok(())
}

/// Read a static template from its bytes.
///
/// The caller reads at most `FILE_MAX` bytes of a file and refuses a larger
/// one by its size (`template_file::read_for`); the check here bounds the
/// reader's memory for any caller that did not.
pub fn read(src: &[u8]) -> Result<Layout, Refusal> {
    if src.len() > FILE_MAX {
        return Err(refuse(format!("it has over {FILE_MAX} bytes")));
    }
    let doc = lua::parse(src).map_err(refuse)?;
    if doc.name != "staticTemplate" || !doc.root.table {
        return Err(refuse("it does not assign a table to `staticTemplate`"));
    }
    let mut r = Reader {
        src,
        dropped: BTreeSet::new(),
        mixed: Vec::new(),
    };
    let theatre = string(src, last(&doc.root, "theatre"));
    let coalitions = last(&doc.root, "coalition")
        .filter(|c| c.table)
        .ok_or_else(|| refuse("it has no `coalition` table"))?;
    unique(coalitions, "`coalition`")?;
    let mut groups = Vec::new();
    let mut statics = Vec::new();
    for (ck, coal) in &coalitions.fields {
        let cat = format!("coalition {}", shown(ck));
        if !coal.table {
            return Err(refuse(format!("{cat} is not a table")));
        }
        unique(coal, &cat)?;
        for (_, country) in r.list(
            "coalition.",
            last(coal, "country"),
            &format!("the countries of {cat}"),
        )? {
            let at = format!("{cat}, country {}", country_name(country, src));
            let id = number(src, last(country, "id"))
                .filter(|n| n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(n))
                .map(|n| n as u32)
                .ok_or_else(|| refuse(format!("{at} has no `id` that is a DCS country number")))?;
            unique(country, &at)?;
            for (k, v) in &country.fields {
                let kind = match k {
                    Key::Str(s) => std::str::from_utf8(s).unwrap_or(""),
                    Key::Num(_) => "",
                };
                match kind {
                    "id" | "name" => {}
                    "vehicle" | "static" => {
                        let kat = format!("{at}, {kind}");
                        if !v.table {
                            return Err(refuse(format!("{kat} is not a table")));
                        }
                        for (gi, g) in r.list(
                            &format!("{kind}.group."),
                            last(v, "group"),
                            &format!("the groups of {kat}"),
                        )? {
                            let gat = format!("{kat} group [{gi}]");
                            let name = string(src, last(g, "name")).unwrap_or_default();
                            if kind == "vehicle" {
                                groups.push(vehicle(&mut r, g, &gat, name, id)?);
                            } else {
                                statics.extend(static_group(&mut r, g, &gat, name, id)?);
                            }
                        }
                    }
                    _ if UNBUILT.contains(&kind) => {
                        let has = last(v, "group").is_some_and(|g| !g.fields.is_empty());
                        if has {
                            return Err(refuse(format!(
                                "{at} holds {kind} groups; only vehicle groups and statics are built"
                            )));
                        }
                    }
                    _ => r.drop_key("country.", k),
                }
            }
        }
    }
    if groups.is_empty() && statics.is_empty() {
        return Err(refuse("it holds no vehicle group and no static"));
    }
    Ok(Layout {
        theatre,
        groups,
        statics,
        dropped: r.dropped.into_iter().collect(),
        mixed: r.mixed,
    })
}

/// A country for messages: its `name` when it is text.
fn country_name(country: &Node, src: &[u8]) -> String {
    match string(src, last(country, "name")) {
        Some(n) => format!("{:?}", n.chars().take(40).collect::<String>()),
        None => "with no name".into(),
    }
}

fn vehicle(
    r: &mut Reader,
    g: &Node,
    at: &str,
    name: String,
    country: u32,
) -> Result<Group, Refusal> {
    for (k, _) in &g.fields {
        if !matches!(k, Key::Str(s) if s == b"units" || s == b"name") {
            r.drop_key("vehicle.", k);
        }
    }
    let mut units = Vec::new();
    for (ui, u) in r.list(
        "vehicle.units.",
        last(g, "units"),
        &format!("the units of {at}"),
    )? {
        let uat = format!("{at} unit [{ui}]");
        units.push(Unit {
            type_name: r.type_name(u, &uat)?,
            x: r.finite(u, "x", &uat)?,
            y: r.finite(u, "y", &uat)?,
            heading: r.finite(u, "heading", &uat)?,
            skill: string(r.src, last(u, "skill")).unwrap_or_default(),
        });
        r.sort_keys("vehicle.units[].", u, UNIT_USED, &[], &uat)?;
    }
    if units.is_empty() {
        return Err(refuse(format!("{at} has no units")));
    }
    Ok(Group {
        name,
        country,
        units,
    })
}

fn static_group(
    r: &mut Reader,
    g: &Node,
    at: &str,
    name: String,
    country: u32,
) -> Result<Vec<Static>, Refusal> {
    let linked = matches!(
        last(g, "linkOffset").and_then(|n| lua::scalar_value(r.src, n)),
        Some(Lv::Bool(true))
    ) || last(g, "route")
        .and_then(|rt| last(rt, "points"))
        .is_some_and(|ps| ps.fields.iter().any(|(_, p)| last(p, "linkUnit").is_some()));
    if linked {
        return Err(refuse(format!(
            "{at} is linked to a unit; a linked static is not built"
        )));
    }
    let dead = matches!(
        last(g, "dead").and_then(|d| lua::scalar_value(r.src, d)),
        Some(Lv::Bool(true))
    );
    for (k, _) in &g.fields {
        let used = ["units", "name", "dead", "linkOffset"];
        if !matches!(k, Key::Str(s) if used.iter().any(|u| u.as_bytes() == s)) {
            r.drop_key("static.", k);
        }
    }
    let mut out = Vec::new();
    for (ui, u) in r.list(
        "static.units.",
        last(g, "units"),
        &format!("the units of {at}"),
    )? {
        let uat = format!("{at} unit [{ui}]");
        let grass = string(r.src, last(u, "category")).is_some_and(|c| c == GRASS_CATEGORY)
            || u.fields.iter().any(
                |(k, _)| matches!(k, Key::Str(s) if s.starts_with(GRASS_KEY_PREFIX.as_bytes())),
            );
        if grass {
            return Err(refuse(format!(
                "{uat} is a grass airfield, which is not built"
            )));
        }
        let heliport = string(r.src, last(u, "category")).is_some_and(|c| c == HELIPORT_CATEGORY);
        let passed: Vec<&str> = if heliport {
            STATIC_PASSED
                .iter()
                .chain(HELIPORT_PASSED)
                .copied()
                .collect()
        } else {
            STATIC_PASSED.to_vec()
        };
        out.push((
            heliport,
            Static {
                name: name.clone(),
                country,
                type_name: r.type_name(u, &uat)?,
                x: r.finite(u, "x", &uat)?,
                y: r.finite(u, "y", &uat)?,
                heading: r.finite(u, "heading", &uat)?,
                dead,
                extra: r.sort_keys("static.units[].", u, STATIC_USED, &passed, &uat)?,
                further: Vec::new(),
            },
        ));
    }
    if out.is_empty() {
        return Err(refuse(format!("{at} has no units")));
    }
    let heliports = out.iter().filter(|(h, _)| *h).count();
    if out.len() > 1 && heliports == out.len() {
        // A heliport group is one static with further units: DCS makes one
        // airbase of it (spec/world.md, "Heliport groups").
        let mut units = out.into_iter().map(|(_, s)| s);
        let mut first = units.next().expect("not empty");
        first.further = units
            .map(|s| StaticUnit {
                type_name: s.type_name,
                x: s.x,
                y: s.y,
                heading: s.heading,
                extra: s.extra,
            })
            .collect();
        return Ok(vec![first]);
    }
    if heliports > 0 && heliports < out.len() {
        r.mixed.push(at.to_string());
    }
    Ok(out.into_iter().map(|(_, s)| s).collect())
}

/// The specs a call makes from a layout, in the file's order.
#[derive(Clone, Debug, PartialEq)]
pub struct Specs {
    /// The theater every entity is on.
    pub theater: String,
    /// One spec per vehicle group.
    pub groups: Vec<pb::GroupSpec>,
    /// One spec per static.
    pub statics: Vec<pb::StaticSpec>,
}

/// The layout's origin: the center of the box that bounds every unit's and
/// static's place, as mission `x` and `y`.
pub fn origin(l: &Layout) -> (f64, f64) {
    let places =
        l.groups
            .iter()
            .flat_map(|g| g.units.iter().map(|u| (u.x, u.y)))
            .chain(l.statics.iter().flat_map(|s| {
                std::iter::once((s.x, s.y)).chain(s.further.iter().map(|f| (f.x, f.y)))
            }));
    let (mut lo, mut hi) = (
        (f64::INFINITY, f64::INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
    );
    for (x, y) in places {
        lo = (lo.0.min(x), lo.1.min(y));
        hi = (hi.0.max(x), hi.1.max(y));
    }
    (lo.0 + (hi.0 - lo.0) / 2.0, lo.1 + (hi.1 - lo.1) / 2.0)
}

/// An entity's label: the request's prefix and the file's name.
fn label(prefix: &str, name: &str) -> String {
    match (prefix.is_empty(), name.is_empty()) {
        (true, _) => name.to_owned(),
        (false, true) => prefix.to_owned(),
        (false, false) => format!("{prefix}/{name}"),
    }
}

/// The specs of the entities of `l`.
///
/// With an `anchor`, the layout's origin goes to it, on its theater and at
/// its altitude. Without one, every entity is where the file has it, on the
/// file's `theatre`, at altitude 0. Either way `heading`, radians clockwise
/// from north, turns the layout about its origin and is added to every
/// heading, the sum taken within a full turn as the editor's own turn takes
/// it (`group_template::spec`); at 0 every heading is the file's own, and
/// without an anchor every place too. A file's `x` is meters north and its
/// `y` meters east, as a group template's `dx` and `dy` are.
pub fn specs(
    l: &Layout,
    anchor: Option<&pb::Position>,
    heading: f64,
    country: Option<u32>,
    prefix: &str,
) -> Result<Specs, Refusal> {
    let theater =
        match anchor {
            Some(a) => a.theater.clone(),
            None => l.theatre.clone().filter(|t| !t.is_empty()).ok_or_else(|| {
                refuse("it has no `theatre`, which a call without an anchor needs")
            })?,
        };
    let (ox, oy) = origin(l);
    let (base_n, base_e, alt) = match anchor {
        Some(a) => (a.north, a.east, a.alt),
        None => (ox, oy, 0.0),
    };
    let (sin, cos) = heading.sin_cos();
    let place = |x: f64, y: f64, what: &str| -> Result<pb::Position, Refusal> {
        let (n, e) = if heading == 0.0 && anchor.is_none() {
            (x, y)
        } else {
            let (dn, de) = (x - ox, y - oy);
            let (dn, de) = if heading == 0.0 {
                (dn, de)
            } else {
                (dn * cos - de * sin, dn * sin + de * cos)
            };
            (base_n + dn, base_e + de)
        };
        if !(n.is_finite() && e.is_finite()) {
            return Err(Refusal {
                field: "path",
                text: format!("{what} is not at a finite place"),
            });
        }
        Ok(pb::Position {
            theater: theater.clone(),
            north: n,
            east: e,
            alt,
        })
    };
    let turn = |h: f64| {
        if heading == 0.0 {
            h
        } else {
            (h + heading) % std::f64::consts::TAU
        }
    };
    let mut groups = Vec::with_capacity(l.groups.len());
    for (gi, g) in l.groups.iter().enumerate() {
        let mut units = Vec::with_capacity(g.units.len());
        for (ui, u) in g.units.iter().enumerate() {
            units.push(pb::UnitSpec {
                r#type: u.type_name.clone(),
                position: Some(place(
                    u.x,
                    u.y,
                    &format!("unit {} of vehicle group {}", ui + 1, gi + 1),
                )?),
                heading: turn(u.heading),
                skill: u.skill.clone(),
                ..pb::UnitSpec::default()
            });
        }
        groups.push(pb::GroupSpec {
            label: label(prefix, &g.name),
            country: country.unwrap_or(g.country),
            category: pb::GroupCategory::Ground as i32,
            units,
            ..pb::GroupSpec::default()
        });
    }
    let mut statics = Vec::with_capacity(l.statics.len());
    let extra_of = |keys: &[(String, Lv)]| {
        if keys.is_empty() {
            None
        } else {
            crate::convert::extra(&Lv::Table(
                keys.iter()
                    .map(|(k, v)| (Lv::String(k.as_bytes().to_vec()), v.clone()))
                    .collect(),
            ))
        }
    };
    for (si, s) in l.statics.iter().enumerate() {
        let mut further_units = Vec::with_capacity(s.further.len());
        for (fi, f) in s.further.iter().enumerate() {
            further_units.push(pb::StaticUnit {
                r#type: f.type_name.clone(),
                position: Some(place(
                    f.x,
                    f.y,
                    &format!("unit {} of static {}", fi + 2, si + 1),
                )?),
                heading: turn(f.heading),
                extra: extra_of(&f.extra),
            });
        }
        statics.push(pb::StaticSpec {
            label: label(prefix, &s.name),
            country: country.unwrap_or(s.country),
            r#type: s.type_name.clone(),
            position: Some(place(s.x, s.y, &format!("static {}", si + 1))?),
            heading: turn(s.heading),
            extra: extra_of(&s.extra),
            dead: s.dead,
            further_units,
        });
    }
    Ok(Specs {
        theater,
        groups,
        statics,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::f64::consts::{FRAC_PI_2, PI, TAU};

    use proptest::prelude::*;

    use super::*;

    /// A file in the form the Mission Editor writes (ED-CODE, the editor's
    /// `me_staticTemplate.lua` and `me_mission.lua`), made for these tests.
    pub(crate) const FILE: &str = r#"staticTemplate = 
{
    ["coalition"] = 
    {
        ["blue"] = 
        {
            ["name"] = "blue",
            ["country"] = 
            {
                [3] = 
                {
                    ["id"] = 2,
                    ["name"] = "USA",
                    ["vehicle"] = 
                    {
                        ["group"] = 
                        {
                            [1] = 
                            {
                                ["visible"] = false,
                                ["task"] = "Ground Nothing",
                                ["route"] = 
                                {
                                    ["points"] = 
                                    {
                                        [1] = 
                                        {
                                            ["alt"] = 20,
                                            ["x"] = -281000,
                                            ["y"] = 641000,
                                        }, -- end of [1]
                                    }, -- end of ["points"]
                                }, -- end of ["route"]
                                ["groupId"] = 7,
                                ["hidden"] = false,
                                ["units"] = 
                                {
                                    [1] = 
                                    {
                                        ["transportable"] = 
                                        {
                                            ["randomTransportable"] = false,
                                        }, -- end of ["transportable"]
                                        ["skill"] = "Average",
                                        ["type"] = "Type A",
                                        ["unitId"] = 11,
                                        ["y"] = 641000,
                                        ["x"] = -281000,
                                        ["name"] = "Alpha-1",
                                        ["heading"] = 1.25,
                                        ["playerCanDrive"] = false,
                                    }, -- end of [1]
                                    [2] = 
                                    {
                                        ["skill"] = "High",
                                        ["AddPropVehicle"] = 
                                        {
                                            ["TentedRoof"] = true,
                                        }, -- end of ["AddPropVehicle"]
                                        ["type"] = "Type B",
                                        ["unitId"] = 12,
                                        ["y"] = 641030.5,
                                        ["x"] = -280900,
                                        ["name"] = "Alpha-2",
                                        ["heading"] = 0,
                                    }, -- end of [2]
                                }, -- end of ["units"]
                                ["y"] = 641000,
                                ["x"] = -281000,
                                ["name"] = "Alpha",
                                ["start_time"] = 0,
                            }, -- end of [1]
                        }, -- end of ["group"]
                    }, -- end of ["vehicle"]
                }, -- end of [3]
            }, -- end of ["country"]
        }, -- end of ["blue"]
        ["red"] = 
        {
            ["name"] = "red",
            ["country"] = 
            {
                [1] = 
                {
                    ["id"] = 0,
                    ["name"] = "Russia",
                    ["static"] = 
                    {
                        ["group"] = 
                        {
                            [1] = 
                            {
                                ["heading"] = 0.2,
                                ["route"] = 
                                {
                                    ["points"] = 
                                    {
                                        [1] = 
                                        {
                                            ["alt"] = 0,
                                            ["type"] = "",
                                            ["name"] = "",
                                            ["y"] = 640800,
                                            ["speed"] = 0,
                                            ["x"] = -281200,
                                            ["formation_template"] = "",
                                            ["action"] = "",
                                        }, -- end of [1]
                                    }, -- end of ["points"]
                                }, -- end of ["route"]
                                ["groupId"] = 8,
                                ["hidden"] = false,
                                ["units"] = 
                                {
                                    [1] = 
                                    {
                                        ["category"] = "Fortifications",
                                        ["shape_name"] = "shape-a",
                                        ["type"] = "Static A",
                                        ["unitId"] = 13,
                                        ["rate"] = 100,
                                        ["y"] = 640800,
                                        ["x"] = -281200,
                                        ["name"] = "Bunker",
                                        ["heading"] = 0.5,
                                    }, -- end of [1]
                                }, -- end of ["units"]
                                ["y"] = 640800,
                                ["x"] = -281200,
                                ["name"] = "Bunker",
                                ["dead"] = false,
                            }, -- end of [1]
                            [2] = 
                            {
                                ["heading"] = 0,
                                ["groupId"] = 9,
                                ["units"] = 
                                {
                                    [1] = 
                                    {
                                        ["category"] = "Warehouses",
                                        ["type"] = "Static B",
                                        ["unitId"] = 14,
                                        ["y"] = 640750,
                                        ["x"] = -281150,
                                        ["name"] = "Depot",
                                        ["heading"] = 0,
                                    }, -- end of [1]
                                }, -- end of ["units"]
                                ["y"] = 640750,
                                ["x"] = -281150,
                                ["name"] = "Depot",
                                ["dead"] = true,
                            }, -- end of [2]
                        }, -- end of ["group"]
                    }, -- end of ["static"]
                }, -- end of [1]
            }, -- end of ["country"]
        }, -- end of ["red"]
    }, -- end of ["coalition"]
    ["name"] = "Test Layout",
    ["requiredModules"] = 
    {
    }, -- end of ["requiredModules"]
    ["localization"] = 
    {
        ["DEFAULT"] = 
        {
            ["DictKey_descriptionText_1"] = "",
        }, -- end of ["DEFAULT"]
    }, -- end of ["localization"]
    ["theatre"] = "Caucasus",
    ["desc"] = "two lines\
of description",
} -- end of staticTemplate
"#;

    fn refused(src: &str) -> Refusal {
        let r = read(src.as_bytes()).expect_err("refused");
        assert_eq!(r.field, "path");
        r
    }

    /// A file of one country holding `body` as its kinds.
    pub(crate) fn one(body: &str) -> String {
        format!(
            "staticTemplate = {{ [\"theatre\"] = \"Caucasus\", [\"coalition\"] = {{ [\"blue\"] = {{ [\"country\"] = {{ [1] = {{ [\"id\"] = 2, [\"name\"] = \"USA\", {body} }} }} }} }} }}"
        )
    }

    /// One vehicle unit with `unit` as its fields.
    fn vehicle_unit(unit: &str) -> String {
        one(&format!(
            "[\"vehicle\"] = {{ [\"group\"] = {{ [1] = {{ [\"units\"] = {{ [1] = {{ {unit} }} }} }} }} }}"
        ))
    }

    const UNIT: &str = "[\"type\"] = \"T\", [\"x\"] = 1, [\"y\"] = 2, [\"heading\"] = 0";

    /// A static template of one helipad, made for these tests: the keys a
    /// file the Mission Editor saved holds for a helipad's group and unit
    /// (LIVE, docs/results/2026-11-24-template-heliports.md), its route
    /// point included, without the top-level keys the conversion does not
    /// read.
    pub(crate) const HELIPAD: &str = r#"staticTemplate =
{
	["coalition"] =
	{
		["blue"] =
		{
			["name"] = "blue",
			["country"] =
			{
				[28] =
				{
					["id"] = 2,
					["static"] =
					{
						["group"] =
						{
							[1] =
							{
								["heading"] = 0,
								["route"] = 
								{
									["points"] = 
									{
										[1] = 
										{
											["alt"] = 0,
											["type"] = "",
											["name"] = "",
											["y"] = 619000.25,
											["speed"] = 0,
											["x"] = -356000.5,
											["formation_template"] = "",
											["action"] = "",
										}, -- end of [1]
									}, -- end of ["points"]
								}, -- end of ["route"]
								["groupId"] = 5,
								["units"] =
								{
									[1] =
									{
										["tasks"] = {},
										["category"] = "Heliports",
										["shape_name"] = "FARP",
										["type"] = "SINGLE_HELIPAD",
										["unitId"] = 25,
										["heliport_frequency"] = "127.5",
										["y"] = 619000.25,
										["x"] = -356000.5,
										["name"] = "Pad-1-1",
										["heliport_modulation"] = 0,
										["heliport_callsign_id"] = 1,
										["heading"] = 0.75,
									}, -- end of [1]
								}, -- end of ["units"]
								["y"] = 619000.25,
								["x"] = -356000.5,
								["name"] = "Pad-1",
								["dead"] = false,
							}, -- end of [1]
						}, -- end of ["group"]
					}, -- end of ["static"]
					["name"] = "USA",
				}, -- end of [28]
			}, -- end of ["country"]
		}, -- end of ["blue"]
	}, -- end of ["coalition"]
	["theatre"] = "Caucasus",
} -- end of staticTemplate"#;

    #[test]
    fn a_heliport_key_on_another_static_is_dropped() {
        let l = read(
            one("[\"static\"] = { [\"group\"] = { [1] = { [\"units\"] = { [1] = { [\"type\"] = \"F\", [\"heliport_frequency\"] = 127.5, [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0 } } } } }")
                .as_bytes(),
        )
        .expect("read");
        assert!(l.statics[0].extra.is_empty());
        assert!(
            l.dropped
                .contains(&"static.units[].heliport_frequency".to_string()),
            "{:?}",
            l.dropped
        );
    }

    /// A file of one static group whose units are `(type, category, x)`,
    /// each at y = 5 and heading 0.5; a heliport carries a radio.
    pub(crate) fn static_group(units: &[(&str, &str, f64)]) -> String {
        let units: String = units
            .iter()
            .enumerate()
            .map(|(i, (t, category, x))| {
                format!(
                    "[{}] = {{ [\"type\"] = \"{t}\", [\"category\"] = \"{category}\", [\"heliport_frequency\"] = \"127.{i}\", [\"heliport_callsign_id\"] = {}, [\"x\"] = {x}, [\"y\"] = 5, [\"heading\"] = 0.5 }},",
                    i + 1,
                    i + 1
                )
            })
            .collect();
        one(&format!(
            "[\"static\"] = {{ [\"group\"] = {{ [1] = {{ [\"name\"] = \"Farps\", [\"units\"] = {{ {units} }} }} }} }}"
        ))
    }

    /// A group of three FARPs, for the tests of the service.
    pub(crate) fn three_farps() -> String {
        static_group(&[
            ("FARP", "Heliports", 10.0),
            ("Invisible FARP", "Heliports", 20.0),
            ("Invisible FARP", "Heliports", 30.0),
        ])
    }

    #[test]
    fn a_group_of_heliports_is_one_static_with_further_units() {
        let l = read(three_farps().as_bytes()).expect("read");
        assert_eq!(l.statics.len(), 1);
        assert!(l.mixed.is_empty());
        let s = &l.statics[0];
        assert_eq!((s.type_name.as_str(), s.x, s.y), ("FARP", 10.0, 5.0));
        assert_eq!(s.further.len(), 2);
        for (i, f) in s.further.iter().enumerate() {
            assert_eq!(f.type_name, "Invisible FARP");
            assert_eq!((f.x, f.y, f.heading), (20.0 + 10.0 * i as f64, 5.0, 0.5));
            // Each unit keeps its own radio keys.
            assert!(
                f.extra.contains(&(
                    "heliport_frequency".to_string(),
                    Lv::String(format!("127.{}", i + 1).into_bytes())
                )),
                "{:?}",
                f.extra
            );
            assert!(f.extra.iter().any(|(k, _)| k == "heliport_callsign_id"));
            assert!(
                f.extra
                    .contains(&("category".to_string(), Lv::String(b"Heliports".to_vec())))
            );
        }
        assert!(s.extra.contains(&(
            "heliport_frequency".to_string(),
            Lv::String(b"127.0".to_vec())
        )));
        assert!(
            !l.dropped.iter().any(|k| k.contains("heliport_")),
            "{:?}",
            l.dropped
        );
    }

    #[test]
    fn a_group_mixing_heliports_and_other_statics_stays_one_static_a_unit() {
        let l = read(
            static_group(&[
                ("FARP", "Heliports", 10.0),
                ("Tent", "Fortifications", 20.0),
                ("FARP", "Heliports", 30.0),
            ])
            .as_bytes(),
        )
        .expect("read");
        assert_eq!(l.statics.len(), 3);
        assert!(l.statics.iter().all(|s| s.further.is_empty()));
        assert_eq!(l.mixed.len(), 1, "{:?}", l.mixed);
        // The log line names the group.
        assert!(
            log_notes(&l).contains(&format!(
                "static groups mixing heliports with other statics, one static per unit: {}",
                l.mixed[0]
            )),
            "{}",
            log_notes(&l)
        );
        // A group of one heliport, and one of other statics, are not mixed.
        let l = read(static_group(&[("FARP", "Heliports", 10.0)]).as_bytes()).expect("read");
        assert!(l.mixed.is_empty() && l.statics[0].further.is_empty());
        let l = read(
            static_group(&[
                ("Tent", "Fortifications", 1.0),
                ("Tent", "Fortifications", 2.0),
            ])
            .as_bytes(),
        )
        .expect("read");
        assert!(l.mixed.is_empty());
        assert_eq!(l.statics.len(), 2);
    }

    #[test]
    fn a_heliport_group_makes_one_spec_with_further_units() {
        let l = read(three_farps().as_bytes()).expect("read");
        let s = specs(&l, None, 0.0, None, "").expect("specs");
        assert_eq!(s.statics.len(), 1);
        let st = &s.statics[0];
        assert_eq!(st.further_units.len(), 2);
        assert_eq!(st.further_units[1].r#type, "Invisible FARP");
        assert!(st.further_units[0].extra.is_some());
    }

    #[test]
    fn an_anchored_heliport_group_turns_about_its_middle() {
        let l = read(three_farps().as_bytes()).expect("read");
        let anchor = pb::Position {
            theater: "Caucasus".into(),
            north: 1000.0,
            east: 2000.0,
            alt: 0.0,
        };
        let s = specs(&l, Some(&anchor), FRAC_PI_2, None, "").expect("specs");
        let st = &s.statics[0];
        let at = |p: &Option<pb::Position>| {
            let p = p.as_ref().expect("placed");
            (p.north.round(), p.east.round())
        };
        // The units stand 10 m apart along north; the middle one is the
        // box's center, and a quarter turn lays them along east.
        assert_eq!(at(&st.position), (1000.0, 1990.0));
        assert_eq!(at(&st.further_units[0].position), (1000.0, 2000.0));
        assert_eq!(at(&st.further_units[1].position), (1000.0, 2010.0));
        // Every unit's heading turns with the layout.
        let turned = (0.5 + FRAC_PI_2) % TAU;
        assert_eq!(st.heading, turned);
        for u in &st.further_units {
            assert_eq!(u.heading, turned);
        }
    }

    #[test]
    fn a_helipad_is_a_static_that_keeps_its_radio_keys() {
        let l = read(HELIPAD.as_bytes()).expect("read");
        assert_eq!(l.statics.len(), 1);
        let s = &l.statics[0];
        assert_eq!(
            (s.name.as_str(), s.country, s.type_name.as_str(), s.dead),
            ("Pad-1", 2, "SINGLE_HELIPAD", false)
        );
        assert_eq!((s.x, s.y, s.heading), (-356000.5, 619000.25, 0.75));
        // The frequency stays text, as the file holds it: DCS reads both.
        assert_eq!(
            s.extra,
            [
                ("category".to_string(), Lv::String(b"Heliports".to_vec())),
                ("shape_name".to_string(), Lv::String(b"FARP".to_vec())),
                (
                    "heliport_frequency".to_string(),
                    Lv::String(b"127.5".to_vec())
                ),
                ("heliport_modulation".to_string(), Lv::Number(0.0)),
                ("heliport_callsign_id".to_string(), Lv::Number(1.0)),
            ]
        );
        for k in ["static.units[].tasks", "static.route"] {
            assert!(l.dropped.contains(&k.to_string()), "{k}: {:?}", l.dropped);
        }
        assert!(
            !l.dropped.iter().any(|k| k.contains("heliport_")),
            "{:?}",
            l.dropped
        );
    }

    #[test]
    fn reads_a_file_in_the_editors_form() {
        let l = read(FILE.as_bytes()).expect("read");
        assert_eq!(l.theatre.as_deref(), Some("Caucasus"));
        assert_eq!(l.groups.len(), 1);
        let g = &l.groups[0];
        assert_eq!((g.name.as_str(), g.country), ("Alpha", 2));
        assert_eq!(
            g.units[0],
            Unit {
                type_name: "Type A".into(),
                x: -281000.0,
                y: 641000.0,
                heading: 1.25,
                skill: "Average".into(),
            }
        );
        assert_eq!(g.units[1].type_name, "Type B");
        assert_eq!((g.units[1].x, g.units[1].y), (-280900.0, 641030.5));
        assert_eq!(l.statics.len(), 2);
        let s = &l.statics[0];
        assert_eq!(
            (s.name.as_str(), s.country, s.type_name.as_str(), s.dead),
            ("Bunker", 0, "Static A", false)
        );
        assert_eq!((s.x, s.y, s.heading), (-281200.0, 640800.0, 0.5));
        assert_eq!(
            s.extra.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["category", "shape_name", "rate"]
        );
        assert!(l.statics[1].dead);
        assert_eq!(l.statics[1].type_name, "Static B");
        // Group keys the conversion does not use are named for the log.
        for k in [
            "vehicle.route",
            "vehicle.task",
            "vehicle.x",
            "static.hidden",
        ] {
            assert!(l.dropped.contains(&k.to_string()), "{k}: {:?}", l.dropped);
        }
        assert!(l.dropped.len() <= DROPPED_MAX);
    }

    #[test]
    fn keeps_the_files_order() {
        // Coalition `red` before `blue`, country [3] before [1], and group
        // [2] before [1] in the text: every order is the text's, for the
        // vehicle groups across coalitions and countries, and for statics.
        let g = |name: &str| {
            format!("{{ [\"name\"] = \"{name}\", [\"units\"] = {{ [1] = {{ {UNIT} }} }} }}")
        };
        let src = format!(
            "staticTemplate = {{ [\"theatre\"] = \"Caucasus\", [\"coalition\"] = {{ \
             [\"red\"] = {{ [\"country\"] = {{ \
               [3] = {{ [\"id\"] = 0, [\"vehicle\"] = {{ [\"group\"] = {{ [2] = {a}, [1] = {b} }} }}, \
                        [\"static\"] = {{ [\"group\"] = {{ [2] = {s2}, [1] = {s1} }} }} }}, \
               [1] = {{ [\"id\"] = 18, [\"vehicle\"] = {{ [\"group\"] = {{ [1] = {c} }} }} }} }} }}, \
             [\"blue\"] = {{ [\"country\"] = {{ [1] = {{ [\"id\"] = 2, [\"vehicle\"] = {{ [\"group\"] = {{ [1] = {d} }} }} }} }} }} }} }}",
            a = g("red 3 g2"),
            b = g("red 3 g1"),
            c = g("red 1 g1"),
            d = g("blue 1 g1"),
            s2 = g("second"),
            s1 = g("first"),
        );
        let l = read(src.as_bytes()).expect("read");
        let groups: Vec<_> = l
            .groups
            .iter()
            .map(|g| (g.name.as_str(), g.country))
            .collect();
        assert_eq!(
            groups,
            [
                ("red 3 g2", 0),
                ("red 3 g1", 0),
                ("red 1 g1", 18),
                ("blue 1 g1", 2)
            ]
        );
        let names: Vec<_> = l.statics.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["second", "first"]);
    }

    #[test]
    fn two_entries_under_one_key_refuse_the_file() {
        let group =
            format!("{{ [\"group\"] = {{ [1] = {{ [\"units\"] = {{ [1] = {{ {UNIT} }} }} }} }} }}");
        let r = refused(&one(&format!(
            "[\"vehicle\"] = {group}, [\"vehicle\"] = {group}"
        )));
        assert!(r.text.contains("has two entries \"vehicle\""), "{}", r.text);
        let country = format!(
            "{{ [\"country\"] = {{ [1] = {{ [\"id\"] = 2, [\"vehicle\"] = {group} }} }} }}"
        );
        let r = refused(&format!(
            "staticTemplate = {{ [\"coalition\"] = {{ [\"blue\"] = {country}, [\"blue\"] = {country} }} }}"
        ));
        assert!(
            r.text.contains("`coalition` has two entries \"blue\""),
            "{}",
            r.text
        );
    }

    #[test]
    fn a_value_the_store_cannot_encode_is_refused_with_its_place() {
        let deep = format!("{}1{}", "{".repeat(20), "}".repeat(20));
        let r = refused(&one(&format!(
            "[\"static\"] = {{ [\"group\"] = {{ [1] = {{ [\"units\"] = {{ [1] = {{ [\"type\"] = \"S\", [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0, [\"mass\"] = {deep} }} }} }} }} }}"
        )));
        assert!(r.text.contains("`mass` of"), "{}", r.text);
        assert!(
            r.text
                .contains("static group [1] unit [1] cannot be stored"),
            "{}",
            r.text
        );
        // Nested within the store's depth, the same key is passed.
        let ok = format!("{}1{}", "{".repeat(8), "}".repeat(8));
        let l = read(
            one(&format!(
                "[\"static\"] = {{ [\"group\"] = {{ [1] = {{ [\"units\"] = {{ [1] = {{ [\"type\"] = \"S\", [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0, [\"mass\"] = {ok} }} }} }} }} }}"
            ))
            .as_bytes(),
        )
        .expect("read");
        assert_eq!(l.statics[0].extra[0].0, "mass");
    }

    #[test]
    fn a_coalition_with_two_country_lists_is_refused() {
        let group =
            format!("{{ [\"group\"] = {{ [1] = {{ [\"units\"] = {{ [1] = {{ {UNIT} }} }} }} }} }}");
        let list = format!("{{ [1] = {{ [\"id\"] = 2, [\"vehicle\"] = {group} }} }}");
        let r = refused(&format!(
            "staticTemplate = {{ [\"coalition\"] = {{ [\"blue\"] = {{ [\"country\"] = {list}, [\"country\"] = {list} }} }} }}"
        ));
        assert!(
            r.text
                .contains("coalition blue has two entries \"country\""),
            "{}",
            r.text
        );
        let r = refused(&format!(
            "staticTemplate = {{ [\"coalition\"] = {{ [1] = {{ [\"country\"] = {list} }}, [1] = {{ [\"country\"] = {list} }} }} }}"
        ));
        assert!(
            r.text.contains("`coalition` has two entries [1]"),
            "{}",
            r.text
        );
    }

    #[test]
    fn the_reader_refuses_a_value_exactly_when_the_store_would() {
        let (mut passed, mut refused) = (0, None);
        for n in 10..20 {
            let v = format!("{}1{}", "{".repeat(n), "}".repeat(n));
            let src = one(&format!(
                "[\"static\"] = {{ [\"group\"] = {{ [1] = {{ [\"units\"] = {{ [1] = {{ [\"type\"] = \"S\", [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0, [\"mass\"] = {v} }} }} }} }} }}"
            ));
            match read(src.as_bytes()) {
                // Passed: the static's further keys reach the store whole,
                // by the path the call takes, and the store takes them.
                Ok(l) => {
                    let s = specs(&l, None, 0.0, None, "").expect("specs");
                    let extra = s.statics[0].extra.as_ref().expect("extra kept");
                    let held = crate::lua::extra_to_lua(Some(extra));
                    assert!(check_lv(&held).is_ok(), "{n}");
                    passed += 1;
                }
                Err(r) => {
                    assert!(r.text.contains("`mass` of"), "{n}: {}", r.text);
                    refused.get_or_insert(n);
                }
            }
        }
        // Both sides of the boundary are in the range, and a refused depth
        // is never below a passed one.
        let first = refused.expect("a depth refused");
        assert!(
            passed > 0 && passed == first - 10,
            "{passed} passed, first refused {first}"
        );
    }

    #[test]
    fn static_group_keys_are_named_for_the_log() {
        let src = one(&format!(
            "[\"static\"] = {{ [\"group\"] = {{ [1] = {{ [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0.2, [\"groupId\"] = 3, [\"route\"] = {{}}, [\"dead\"] = false, [\"units\"] = {{ [1] = {{ {UNIT} }} }} }} }} }}"
        ));
        let l = read(src.as_bytes()).expect("read");
        for k in [
            "static.x",
            "static.y",
            "static.heading",
            "static.groupId",
            "static.route",
        ] {
            assert!(l.dropped.contains(&k.to_string()), "{k}: {:?}", l.dropped);
        }
        assert!(!l.dropped.iter().any(|k| k == "static.dead"));
        assert_eq!(
            l.statics[0].heading, 0.0,
            "the unit's heading, not the group's"
        );
    }

    #[test]
    fn unused_keys_are_named_with_where_they_were() {
        let src = one(&format!(
            "[\"vehicle\"] = {{ [\"group\"] = {{ [\"stray\"] = 1, [1] = {{ [\"units\"] = {{ [1] = {{ {UNIT} }} }} }} }} }}"
        ));
        let l = read(src.as_bytes()).expect("read");
        assert!(
            l.dropped.contains(&"vehicle.group.stray".to_string()),
            "{:?}",
            l.dropped
        );
    }

    #[test]
    fn a_skill_that_is_not_text_is_left_out() {
        let l = read(vehicle_unit(&format!("{UNIT}, [\"skill\"] = 3")).as_bytes()).expect("read");
        assert_eq!(l.groups[0].units[0].skill, "");
    }

    #[test]
    fn a_nil_static_key_is_not_passed_and_the_later_of_two_is() {
        let l = read(
            one("[\"static\"] = { [\"group\"] = { [1] = { [\"units\"] = { [1] = { [\"type\"] = \"S\", [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0, [\"rate\"] = nil, [\"mass\"] = 1, [\"mass\"] = 2 } } } } }")
                .as_bytes(),
        )
        .expect("read");
        assert_eq!(
            l.statics[0].extra,
            vec![("mass".to_string(), Lv::Number(2.0))]
        );
    }

    #[test]
    fn refuses_what_it_does_not_build() {
        let unit = |fields: &str| vehicle_unit(fields);
        let cases: Vec<(String, &str)> = vec![
            ("templates = {}".into(), "does not assign a table to `staticTemplate`"),
            ("staticTemplate = 1".into(), "does not assign a table to `staticTemplate`"),
            ("staticTemplate = { [\"theatre\"] = \"Caucasus\" }".into(), "has no `coalition` table"),
            (one(""), "holds no vehicle group and no static"),
            (one("[\"plane\"] = { [\"group\"] = { [1] = {} } }"), "holds plane groups"),
            (one("[\"helicopter\"] = { [\"group\"] = { [1] = {} } }"), "holds helicopter groups"),
            (one("[\"ship\"] = { [\"group\"] = { [1] = {} } }"), "holds ship groups"),
            (one("[\"complex\"] = { [\"group\"] = { [1] = {} } }"), "holds complex groups"),
            (unit("[\"x\"] = 1, [\"y\"] = 2, [\"heading\"] = 0"), "has no `type`"),
            (unit("[\"type\"] = \"\", [\"x\"] = 1, [\"y\"] = 2, [\"heading\"] = 0"), "has no `type`"),
            (unit("[\"type\"] = \"T\", [\"y\"] = 2, [\"heading\"] = 0"), "`x` of"),
            (unit("[\"type\"] = \"T\", [\"x\"] = 1, [\"y\"] = \"2\", [\"heading\"] = 0"), "`y` of"),
            (unit("[\"type\"] = \"T\", [\"x\"] = 1, [\"y\"] = 2"), "`heading` of"),
            (unit("[\"type\"] = \"T\", [\"x\"] = 1e999, [\"y\"] = 2, [\"heading\"] = 0"), "`x` of"),
            (one("[\"vehicle\"] = { [\"group\"] = { [1] = { [\"units\"] = {} } } }"), "has no units"),
            (one("[\"vehicle\"] = { [\"group\"] = { [1] = { [\"units\"] = { [1] = 5 } } } }"), "is not a table"),
            (one("[\"vehicle\"] = { [\"group\"] = { [1] = {}, [1] = {} } }"), "has two entries [1]"),
            (
                "staticTemplate = { [\"coalition\"] = { [\"blue\"] = { [\"country\"] = { [1] = { [\"id\"] = 1.5, [\"vehicle\"] = {} } } } } }".into(),
                "has no `id` that is a DCS country number",
            ),
            (
                "staticTemplate = { [\"coalition\"] = { [\"blue\"] = { [\"country\"] = { [1] = { [\"id\"] = -1 } } } } }".into(),
                "has no `id` that is a DCS country number",
            ),
            (
                one("[\"static\"] = { [\"group\"] = { [1] = { [\"units\"] = { [1] = { [\"type\"] = \"G\", [\"category\"] = \"GrassAirfields\", [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0 } } } } }"),
                "is a grass airfield, which is not built",
            ),
            (
                one("[\"static\"] = { [\"group\"] = { [1] = { [\"units\"] = { [1] = { [\"type\"] = \"G\", [\"grassairfield_frequency\"] = 127.5, [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0 } } } } }"),
                "is a grass airfield, which is not built",
            ),
            (
                "staticTemplate = { [\"coalition\"] = { [\"blue\"] = { [\"country\"] = { [1] = { [\"id\"] = 4294967296, [\"vehicle\"] = {} } } } } }".into(),
                "has no `id` that is a DCS country number",
            ),
            (
                one("[\"static\"] = { [\"group\"] = { [1] = { [\"linkOffset\"] = true, [\"units\"] = { [1] = { [\"type\"] = \"S\", [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0 } } } } }"),
                "a linked static is not built",
            ),
            (
                one("[\"static\"] = { [\"group\"] = { [1] = { [\"route\"] = { [\"points\"] = { [1] = { [\"linkUnit\"] = 4 } } }, [\"units\"] = { [1] = { [\"type\"] = \"S\", [\"x\"] = 0, [\"y\"] = 0, [\"heading\"] = 0 } } } } }"),
                "a linked static is not built",
            ),
        ];
        for (src, want) in cases {
            let r = refused(&src);
            assert!(
                r.text
                    .starts_with("the file is not a static template file: "),
                "{}",
                r.text
            );
            assert!(r.text.contains(want), "{src}\n{} lacks {want:?}", r.text);
        }
    }

    #[test]
    fn an_empty_unbuilt_kind_is_no_refusal() {
        let src = vehicle_unit(UNIT).replace(
            "[\"name\"] = \"USA\",",
            "[\"name\"] = \"USA\", [\"plane\"] = { [\"group\"] = {} },",
        );
        assert_eq!(read(src.as_bytes()).expect("read").groups.len(), 1);
    }

    #[test]
    fn the_reader_refuses_code() {
        for (src, want) in [
            ("local staticTemplate = {}", "line 1"),
            ("staticTemplate = {} return staticTemplate", ""),
            ("staticTemplate = f()", ""),
            ("staticTemplate = { [\"x\"] = 1 + 2 }", ""),
            ("staticTemplate = { [\"x\"] = function() end }", ""),
        ] {
            let r = refused(src);
            assert!(r.text.contains(want), "{src}: {}", r.text);
        }
        let deep = format!("staticTemplate = {}{}", "{".repeat(300), "}".repeat(300));
        refused(&deep);
        let big = format!(
            "staticTemplate = {{ [\"desc\"] = \"{}\" }}",
            "a".repeat(FILE_MAX)
        );
        assert!(refused(&big).text.contains("over 1048576 bytes"));
    }

    fn at(p: &Option<pb::Position>) -> (f64, f64) {
        let p = p.as_ref().expect("position");
        (p.north, p.east)
    }

    /// Every place of a set of specs, groups' units first.
    fn places(s: &Specs) -> Vec<(f64, f64)> {
        s.groups
            .iter()
            .flat_map(|g| g.units.iter().map(|u| at(&u.position)))
            .chain(s.statics.iter().map(|x| at(&x.position)))
            .collect()
    }

    /// The fixture's origin: the middle of x -281,200 to -280,900 and of y
    /// 640,750 to 641,030.5.
    const ORIGIN: (f64, f64) = (-281_050.0, 640_890.25);

    #[test]
    fn in_place_every_number_is_the_files() {
        let l = read(FILE.as_bytes()).expect("read");
        assert_eq!(origin(&l), ORIGIN);
        let s = specs(&l, None, 0.0, None, "").expect("specs");
        assert_eq!(s.theater, "Caucasus");
        assert_eq!(
            places(&s),
            [
                (-281_000.0, 641_000.0),
                (-280_900.0, 641_030.5),
                (-281_200.0, 640_800.0),
                (-281_150.0, 640_750.0)
            ]
        );
        let g = &s.groups[0];
        assert_eq!((g.label.as_str(), g.country), ("Alpha", 2));
        assert_eq!(g.category, pb::GroupCategory::Ground as i32);
        assert_eq!(g.extra, None);
        assert_eq!(
            g.units
                .iter()
                .map(|u| (
                    u.r#type.as_str(),
                    u.heading,
                    u.skill.as_str(),
                    u.label.as_str()
                ))
                .collect::<Vec<_>>(),
            [("Type A", 1.25, "Average", ""), ("Type B", 0.0, "High", "")]
        );
        assert!(g.units.iter().all(|u| {
            u.extra.is_none()
                && u.position
                    .as_ref()
                    .is_some_and(|p| p.alt == 0.0 && p.theater == "Caucasus")
        }));
        let st = &s.statics[0];
        assert_eq!(
            (
                st.label.as_str(),
                st.country,
                st.r#type.as_str(),
                st.heading,
                st.dead
            ),
            ("Bunker", 0, "Static A", 0.5, false)
        );
        let extra = st.extra.as_ref().expect("extra");
        assert_eq!(
            extra.fields.keys().map(String::as_str).collect::<Vec<_>>(),
            ["category", "rate", "shape_name"]
        );
        assert!(s.statics[1].dead);
        assert_eq!(s.statics[1].label, "Depot");
    }

    #[test]
    fn an_anchor_takes_the_origin_and_a_turn_is_about_it() {
        let l = read(FILE.as_bytes()).expect("read");
        let anchor = pb::Position {
            theater: "Syria".into(),
            north: 1_000.0,
            east: -2_000.0,
            alt: 7.0,
        };
        let file: Vec<(f64, f64)> = places(&specs(&l, None, 0.0, None, "").expect("in place"));
        // Moved: each place keeps its offset from the origin.
        let moved = specs(&l, Some(&anchor), 0.0, None, "").expect("moved");
        assert_eq!(moved.theater, "Syria");
        for (p, f) in places(&moved).iter().zip(&file) {
            assert_eq!(
                *p,
                (1_000.0 + (f.0 - ORIGIN.0), -2_000.0 + (f.1 - ORIGIN.1))
            );
        }
        assert!(moved.groups[0].units.iter().all(|u| {
            u.position
                .as_ref()
                .is_some_and(|p| p.alt == 7.0 && p.theater == "Syria")
        }));
        // A quarter turn clockwise: what was north of the origin is east of
        // it, and what was east is south.
        let turned = specs(&l, Some(&anchor), FRAC_PI_2, None, "").expect("turned");
        for (p, f) in places(&turned).iter().zip(&file) {
            let (dn, de) = (f.0 - ORIGIN.0, f.1 - ORIGIN.1);
            assert!((p.0 - (1_000.0 - de)).abs() < 1e-6, "{p:?}");
            assert!((p.1 - (-2_000.0 + dn)).abs() < 1e-6, "{p:?}");
        }
        assert!((turned.groups[0].units[0].heading - (1.25 + FRAC_PI_2)).abs() < 1e-12);
        assert!((turned.statics[0].heading - (0.5 + FRAC_PI_2)).abs() < 1e-12);
        // In place, a turn is about the origin too.
        let spun = specs(&l, None, PI, None, "").expect("spun");
        for (p, f) in places(&spun).iter().zip(&file) {
            assert!((p.0 - (2.0 * ORIGIN.0 - f.0)).abs() < 1e-6, "{p:?}");
            assert!((p.1 - (2.0 * ORIGIN.1 - f.1)).abs() < 1e-6, "{p:?}");
        }
        // The sum is taken within a full turn, keeping its sign.
        let left = specs(&l, None, -FRAC_PI_2, None, "").expect("left");
        assert!((left.statics[1].heading + FRAC_PI_2).abs() < 1e-12);
        let round = specs(&l, None, 3.0 * PI / 2.0, None, "").expect("round");
        assert!((round.groups[0].units[0].heading - ((1.25 + 3.0 * PI / 2.0) % TAU)).abs() < 1e-12);
    }

    #[test]
    fn the_request_sets_the_country_and_the_label_prefix() {
        let l = read(FILE.as_bytes()).expect("read");
        let s = specs(&l, None, 0.0, Some(0), "mine").expect("specs");
        assert!(s.groups.iter().all(|g| g.country == 0));
        assert!(s.statics.iter().all(|g| g.country == 0));
        assert_eq!(s.groups[0].label, "mine/Alpha");
        assert_eq!(s.statics[1].label, "mine/Depot");
        let s = specs(&l, None, 0.0, Some(7), "").expect("specs");
        assert!(s.statics.iter().all(|g| g.country == 7));
        assert_eq!(label("p", ""), "p");
        assert_eq!(label("", ""), "");
    }

    #[test]
    fn without_an_anchor_the_file_needs_a_theatre() {
        let src = vehicle_unit(UNIT).replace("[\"theatre\"] = \"Caucasus\",", "");
        let l = read(src.as_bytes()).expect("read");
        assert_eq!(l.theatre, None);
        let r = specs(&l, None, 0.0, None, "").expect_err("no theatre");
        assert_eq!(r.field, "path");
        assert!(r.text.contains("has no `theatre`"), "{}", r.text);
        for bad in ["5", "\"\""] {
            let src = vehicle_unit(UNIT).replace("\"Caucasus\"", bad);
            let l = read(src.as_bytes()).expect("read");
            let r = specs(&l, None, 0.0, None, "").expect_err("a theatre that is not a name");
            assert_eq!(r.field, "path");
            assert!(r.text.contains("has no `theatre`"), "{bad}: {}", r.text);
        }
        let anchor = pb::Position {
            theater: "Caucasus".into(),
            ..pb::Position::default()
        };
        assert_eq!(
            specs(&l, Some(&anchor), 0.0, None, "")
                .expect("anchored")
                .theater,
            "Caucasus"
        );
        let far = pb::Position {
            north: 1e308,
            ..anchor
        };
        let two = one(
            "[\"vehicle\"] = { [\"group\"] = { [1] = { [\"units\"] = { [1] = { [\"type\"] = \"T\", [\"x\"] = 1e308, [\"y\"] = 0, [\"heading\"] = 0 }, [2] = { [\"type\"] = \"T\", [\"x\"] = -1e308, [\"y\"] = 0, [\"heading\"] = 0 } } } } }",
        );
        let l = read(two.as_bytes()).expect("read");
        let r = specs(&l, Some(&far), 0.0, None, "").expect_err("far");
        assert!(r.text.contains("is not at a finite place"), "{}", r.text);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        #[test]
        fn a_turn_keeps_every_distance_and_the_origin(h in -10.0f64..10.0, n in -1e6f64..1e6, e in -1e6f64..1e6) {
            let l = read(FILE.as_bytes()).expect("read");
            let anchor = pb::Position { theater: "Caucasus".into(), north: n, east: e, alt: 0.0 };
            let file = places(&specs(&l, None, 0.0, None, "").expect("in place"));
            let got = places(&specs(&l, Some(&anchor), h, None, "").expect("turned"));
            for (p, f) in got.iter().zip(&file) {
                let a = ((p.0 - n).powi(2) + (p.1 - e).powi(2)).sqrt();
                let b = ((f.0 - ORIGIN.0).powi(2) + (f.1 - ORIGIN.1).powi(2)).sqrt();
                prop_assert!((a - b).abs() < 1e-6);
            }
            let d = |p: &[(f64, f64)]| ((p[0].0 - p[3].0).powi(2) + (p[0].1 - p[3].1).powi(2)).sqrt();
            prop_assert!((d(&got) - d(&file)).abs() < 1e-6);
        }

        #[test]
        fn arbitrary_bytes_never_panic(b in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = read(&b);
        }

        #[test]
        fn mutated_files_never_panic(at in 0usize..FILE.len(), byte in any::<u8>()) {
            let mut b = FILE.as_bytes().to_vec();
            b[at] = byte;
            let _ = read(&b);
        }
    }
}
