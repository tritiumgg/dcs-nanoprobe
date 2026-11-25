//! `sidecar <name> --part static-replace`: `PutStatic` with a `static_id`
//! revises a static's spec in place, and the sidecar spawns the static again
//! under its name (spec/world.md, "Revising a static").
//!
//! The run makes a campaign on Caucasus and runs two variants, `running`
//! and `paused`, each on its own ground 3 km east of the other's. A variant
//! makes a fortification F, a cargo K (running variant only) and a single
//! FARP P with `PutStatic`, and reads each from DCS by its platform name and
//! from the World, which must list each `LIVE`. It stocks jet fuel with the
//! API, adds an item the controller never set (`weapons.bombs.Mk_82`) in DCS,
//! and gives P the red owner: the running variant sets it with
//! `SetAirbaseOwner`, the paused variant sets no owner and captures P in DCS
//! with `autoCapture(false)` and `setCoalition(1)`, as the agent's owner
//! handler does: without the first, DCS keeps the country's coalition
//! (LIVE, run 1). It reads all three.
//!
//! The paused variant then pauses the mission (only with no player but the
//! server's own connection) and does the rest while it is paused; the
//! running variant does it with the mission running.
//!
//! 1. F is revised 50 m north and turned a quarter turn. The run polls
//!    until DCS shows F there (1 m, 0.01 rad) and the World lists F `LIVE` at
//!    spec revision 2, records what `world.searchObjects` finds within 5 m of
//!    the old place, and checks the World lists no unmanaged static. The
//!    paused variant holds paused for 70 s, more than one enumeration, and
//!    reads again.
//! 2. Running variant: K is revised 30 m north; after DCS shows it there and
//!    10 s more, the World's K stands within 1 m of the revised place.
//! 3. P goes through three revisions: 1 to 4 FARPs, 4 to 2, 2 to 1. Before
//!    the first, a marker static takes an ID, so the three further units'
//!    IDs are the next three. Each revision must give a new airbase under
//!    P's name (a new airbase object; DCS keeps its `getID()`; the parking
//!    spots of every unit, one existing airbase in `world.getAirbases`), every
//!    present further name finding P's static object, every dropped name
//!    finding nothing live, and P `LIVE` at the new spec revision. The
//!    revision carries the old airbase's state: the run polls until DCS
//!    reads coalition 1 and the fuel, waits 130 s more (two warehouse
//!    samples), and reads again, when the Mk_82 still reads 10.
//! 4. A revision of P to `SINGLE_HELIPAD` is refused (`FAILED_PRECONDITION`):
//!    a heliport's type does not change in place. DCS still shows the same
//!    `FARP` airbase object with its 4 spots.
//!
//! 5. After both variants, an eval-only probe on an unmanaged name
//!    `np-sr-retype-probe`, 1 km north of the running variant's FARP. It
//!    resumes the mission first and again before its destroy, which DCS
//!    takes only while the simulation runs. A one-unit `FARP` group spawned with
//!    `coalition.addGroup(country, -1, ...)`, then a one-unit
//!    `SINGLE_HELIPAD` group over the same name. After each spawn and 10 s
//!    later it reads the airbase's type, `getID()`, object `id_` and
//!    `#getParking()`; then it destroys the object and waits until nothing
//!    is left under the name. It records what DCS gives a `SINGLE_HELIPAD`
//!    over a `FARP` (the reason for step 4's refusal): every read after
//!    that spawn is recorded, not judged.
//!
//! Every read is recorded. The run reads the agent's sampling report before
//! the variants and after them, removes every entity with the mission
//! running, also when a step fails, and restores the pause state it found.
//!
//! The run never calls the shared `wait`: it resumes the mission. Its own
//! poll sleeps and keeps the lease.

use std::f64::consts::{FRAC_PI_2, TAU};
use std::time::{Duration, Instant};

use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};

use super::template_heliport::{PLAYERS_LUA, report};
use super::*;

const THEATER: &str = "Caucasus";
/// The country of every static: USA, blue.
const COUNTRY: u32 = 2;
/// The first variant's ground, north and east in meters.
const BASE: (f64, f64) = (-289_000.0, 681_000.0);
/// How far east of the last variant's ground the next one is.
const VARIANT_GAP_M: f64 = 3_000.0;
/// How far a static may stand from its place, in meters.
const NEAR_M: f64 = 1.0;
/// How far a heading may differ, in radians.
const HEADING_TOL: f64 = 0.01;
/// How far north the fortification and the cargo move, in meters.
const FORT_MOVE_M: f64 = 50.0;
const CARGO_MOVE_M: f64 = 30.0;
/// How far apart a FARP group's units stand, east, in meters.
const FARP_STEP_M: f64 = 200.0;
/// How far from the pad the marker static stands, north, in meters.
const MARKER_M: f64 = 500.0;
/// The radius of the search at a static's old place, in meters.
const SEARCH_M: f64 = 5.0;
/// The jet fuel the run stocks, in kg, and the item it adds in DCS.
const FUEL_KG: f64 = 5_000.0;
const ITEM: &str = "weapons.bombs.Mk_82";
const ITEM_COUNT: f64 = 10.0;
/// The owner the run gives P: red, DCS coalition 1.
const OWNER: pb::Coalition = pb::Coalition::Red;
const OWNER_IN_DCS: i64 = 1;
/// The World's `LIVE` state.
const LIVE: i64 = 3;
/// The parking spots DCS gives a `FARP` and a `SINGLE_HELIPAD`.
const FARP_SPOTS: usize = 4;
const HELIPAD_SPOTS: usize = 1;
const FARP: &str = "FARP";
const HELIPAD: &str = "SINGLE_HELIPAD";
/// The time between two polls.
const POLL: Duration = Duration::from_secs(3);
/// How long a static, an airbase or a removal may take to show, and how long
/// the stock may take to come back after a revision. Not design limits; only
/// when this test gives up.
const SHOW_LIMIT: Duration = Duration::from_secs(120);
const STOCK_LIMIT: Duration = Duration::from_secs(150);
const PAUSE_LIMIT: Duration = Duration::from_secs(20);
/// How long the run waits after the stock came back: past two warehouse
/// samples of 60 s. The fuel and the red owner of the running variant are
/// desired values, which the carry never sends: had an empty first sample
/// lowered them, DCS would read them lowered here.
const SETTLE: Duration = Duration::from_secs(130);
/// How long the paused variant holds paused after the fortification moved.
const HOLD_PAUSED: Duration = Duration::from_secs(70);
/// How long the run waits after the cargo showed at its new place.
const CARGO_WAIT: Duration = Duration::from_secs(10);
/// The retype probe's name: no platform name has this form, so the sidecar
/// manages nothing under it.
const RETYPE_PROBE_NAME: &str = "np-sr-retype-probe";
/// How far north of the first variant's FARP the probe stands, in meters:
/// 500 m north of its marker, on land. Run 3's probe stood 500 m south of
/// the FARP and read surface type 3, water.
const PROBE_NORTH_M: f64 = 1_000.0;
/// How long the probe waits after each spawn before it reads again.
const PROBE_WAIT: Duration = Duration::from_secs(10);

/// One step of a FARP's revisions: its label, the count of further units
/// and the type of every unit.
struct Step {
    label: &'static str,
    further: usize,
    kind: &'static str,
}

const STEPS: [Step; 3] = [
    Step {
        label: "1 to 4",
        further: 3,
        kind: FARP,
    },
    Step {
        label: "4 to 2",
        further: 1,
        kind: FARP,
    },
    Step {
        label: "2 to 1",
        further: 0,
        kind: FARP,
    },
];

/// The places of one variant, north and east in meters.
struct Places {
    fort: (f64, f64),
    cargo: (f64, f64),
    farp: (f64, f64),
    marker: (f64, f64),
}

impl Places {
    fn of(variant: usize) -> Places {
        let (n, e) = (BASE.0, BASE.1 + VARIANT_GAP_M * variant as f64);
        Places {
            fort: (n, e),
            cargo: (n, e + 100.0),
            farp: (n, e + 300.0),
            marker: (n + MARKER_M, e + 300.0),
        }
    }

    fn fort_to(&self) -> (f64, f64) {
        (self.fort.0 + FORT_MOVE_M, self.fort.1)
    }

    fn cargo_to(&self) -> (f64, f64) {
        (self.cargo.0 + CARGO_MOVE_M, self.cargo.1)
    }

    /// Every place the variant uses, to read the surface type: the
    /// fortification and the cargo before and after the move, each unit of
    /// the largest FARP group, and the marker.
    fn all(&self) -> Vec<(String, (f64, f64))> {
        let mut v = vec![
            ("fortification".to_string(), self.fort),
            ("fortification moved".into(), self.fort_to()),
            ("cargo".into(), self.cargo),
            ("cargo moved".into(), self.cargo_to()),
        ];
        let units = STEPS.iter().map(|s| s.further).max().unwrap_or(0);
        for k in 0..=units {
            v.push((
                format!("farp unit {k}"),
                (self.farp.0, self.farp.1 + FARP_STEP_M * k as f64),
            ));
        }
        v.push(("marker".into(), self.marker));
        v
    }
}

/// A static the run made: its ID and DCS name.
#[derive(Clone)]
struct Obj {
    id: u64,
    name: String,
}

/// What the run made, for the removal: every static ID, and every DCS name
/// that must be gone at the end (the further units' too).
#[derive(Default)]
struct Made {
    ids: Vec<u64>,
    names: Vec<String>,
}

impl Made {
    fn name(&mut self, n: String) {
        if !self.names.contains(&n) {
            self.names.push(n);
        }
    }

    fn static_(&mut self, c: u64, id: u64) -> Obj {
        let n = dcs_name(c, id);
        self.ids.push(id);
        self.name(n.clone());
        Obj { id, name: n }
    }
}

fn dcs_name(c: u64, id: u64) -> String {
    name(NameKind::Static, CampaignId(c), EntityId(id))
}

/// What a variant shares between its steps.
struct Ctx<'a> {
    run: &'a dyn Live,
    c: u64,
    at: Places,
    paused: bool,
}

// ---- specs -----------------------------------------------------------------

fn category(c: &str) -> Option<prost_types::Struct> {
    Some(prost_types::Struct {
        fields: [(
            "category".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue(c.into())),
            },
        )]
        .into(),
    })
}

/// A static of one unit: its type and extra category.
fn plain(kind: &str, cat: &str, at: (f64, f64), heading: f64) -> pb::StaticSpec {
    pb::StaticSpec {
        label: kind.into(),
        country: COUNTRY,
        r#type: kind.into(),
        position: Some(caucasus(at.0, at.1)),
        heading,
        extra: category(cat),
        ..pb::StaticSpec::default()
    }
}

fn fortification(at: (f64, f64), heading: f64) -> pb::StaticSpec {
    plain("Hangar A", "Fortifications", at, heading)
}

fn cargo(at: (f64, f64)) -> pb::StaticSpec {
    plain("ammo_cargo", "Cargos", at, 0.0)
}

/// A heliport of `kind` with `further` more units of the same type, each
/// [`FARP_STEP_M`] east of the last.
fn heliport(at: (f64, f64), kind: &str, further: usize) -> pb::StaticSpec {
    let mut s = plain(kind, "Heliports", at, 0.0);
    s.further_units = (1..=further)
        .map(|k| pb::StaticUnit {
            r#type: kind.into(),
            position: Some(caucasus(at.0, at.1 + FARP_STEP_M * k as f64)),
            heading: 0.0,
            extra: category("Heliports"),
        })
        .collect();
    s
}

// ---- pure checks -----------------------------------------------------------

/// The parking spots DCS should give a heliport of `units` units of `kind`.
fn parking_wanted(kind: &str, units: usize) -> usize {
    units
        * if kind == FARP {
            FARP_SPOTS
        } else {
            HELIPAD_SPOTS
        }
}

/// The IDs a heliport's further units have after a revision to `count`
/// units, and the IDs it drops. Unit `i` keeps its ID; a longer list takes
/// consecutive IDs from `fresh_from`.
#[derive(Debug, PartialEq)]
struct FurtherPlan {
    ids: Vec<u64>,
    dropped: Vec<u64>,
}

fn further_plan(current: &[u64], count: usize, fresh_from: u64) -> FurtherPlan {
    let kept = current.len().min(count);
    let mut ids = current[..kept].to_vec();
    ids.extend((0..(count - kept) as u64).map(|k| fresh_from + k));
    FurtherPlan {
        ids,
        dropped: current[kept..].to_vec(),
    }
}

/// The angle between two headings, in radians, 0 to pi.
fn heading_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(TAU);
    d.min(TAU - d)
}

/// Whether a read of a static (`read_static`) shows it existing within
/// [`NEAR_M`] of `at` (north, east) and, with a heading, within
/// [`HEADING_TOL`] of it.
fn stands(s: &Value, at: (f64, f64), heading: Option<f64>) -> bool {
    s["found"] == json!(true)
        && s["exists"] == json!(true)
        && matches!((s["x"].as_f64(), s["z"].as_f64()),
            (Some(x), Some(z)) if (x - at.0).hypot(z - at.1) <= NEAR_M)
        && heading.is_none_or(|h| {
            s["heading"]
                .as_f64()
                .is_some_and(|d| heading_diff(d, h) <= HEADING_TOL)
        })
}

/// The World's row of static `id`.
fn row(world: &Value, id: u64) -> Option<&Value> {
    world.as_array()?.iter().find(|r| r["id"] == json!(id))
}

/// Whether the World lists static `id` `LIVE`, at spec revision `rev` when
/// given.
fn row_live(world: &Value, id: u64, rev: Option<u64>) -> bool {
    row(world, id).is_some_and(|r| {
        r["state"].as_i64() == Some(LIVE)
            && rev.is_none_or(|v| r["spec_revision"].as_u64() == Some(v))
    })
}

/// Whether the World lists static `id` within [`NEAR_M`] of `at`.
fn row_near(world: &Value, id: u64, at: (f64, f64)) -> bool {
    row(world, id).is_some_and(|r| {
        matches!((r["north"].as_f64(), r["east"].as_f64()),
            (Some(n), Some(e)) if (n - at.0).hypot(e - at.1) <= NEAR_M)
    })
}

/// The IDs of the statics the World lists that are not managed.
fn unmanaged(world: &Value) -> Vec<u64> {
    world
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|r| r["managed"] != json!(true))
                .filter_map(|r| r["id"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// The verdict on DCS's read of P after a revision to `step` ([`farp_lua`]
/// with P, the present further names, then the dropped ones): a new airbase
/// object (`id_`; DCS keeps `getID()`), the parking spots of every unit, the type, one listed airbase, each
/// present name finding P's static object, each of the `dropped` names
/// finding nothing live, and the World's `LIVE` at the new revision.
fn show_verdict(
    step: &Step,
    p: &str,
    before: Option<f64>,
    dropped: usize,
    seen: &Value,
    world_live: bool,
) -> Value {
    let ab = &seen["airbase"];
    // The airbase's object, which is its static's: DCS keeps the airbase's
    // `getID()` across a replace and gives it a new object (LIVE, run 1).
    let id = ab["id_"].as_f64();
    let new_id = matches!((id, before), (Some(a), Some(b)) if a != b);
    let wanted = parking_wanted(step.kind, 1 + step.further);
    let parking = ab["parking"].as_f64() == Some(wanted as f64);
    let kind = ab["type"] == json!(step.kind);
    let listed = seen["listed"].as_f64() == Some(1.0);
    let statics = |i: usize| &seen["statics"][i];
    let count = seen["statics"].as_array().map_or(0, Vec::len) == 1 + step.further + dropped;
    let present = (1..=step.further).all(|j| {
        let s = statics(j);
        s["exists"] == json!(true) && s["name"] == json!(p)
    });
    let gone = (0..dropped).all(|j| {
        let s = statics(1 + step.further + j);
        *s == json!(false) || (s.is_object() && s["exists"] == json!(false))
    });
    let first = statics(0);
    let first_ok = first["exists"] == json!(true) && first["type"] == json!(step.kind);
    let pass =
        count && new_id && parking && kind && listed && present && gone && first_ok && world_live;
    json!({
        "pass": pass,
        "new_airbase_object": new_id,
        "parking": parking,
        "parking_wanted": wanted,
        "airbase_type": kind,
        "listed_once": listed,
        "further_find_airbase_object": present,
        "dropped_gone": gone,
        "first_unit_type": first_ok,
        "world_live": world_live,
    })
}

/// Whether a read of an airbase shows the owner and the fuel the run set.
fn stock_back(ab: &Value) -> bool {
    ab["coalition"].as_i64() == Some(OWNER_IN_DCS) && ab["fuel"].as_f64() == Some(FUEL_KG)
}

/// Whether a read of the new airbase, after the wait, still shows the owner
/// and the fuel, and the item the controller never set, carried over.
fn stock_settled(ab: &Value) -> bool {
    stock_back(ab) && ab["mk82"].as_f64() == Some(ITEM_COUNT)
}

/// Whether a read of the first airbase shows the owner, the fuel and the
/// item the run added.
fn stock_first(ab: &Value) -> bool {
    stock_back(ab) && ab["mk82"].as_f64() == Some(ITEM_COUNT)
}

/// Whether every step of a record passed.
fn all_pass(steps: &Value) -> bool {
    steps
        .as_object()
        .is_some_and(|o| o.values().all(|v| v["pass"] == json!(true)))
}

/// Whether the removal's read shows every name gone and no airbase of them
/// listed.
fn removal_done(seen: &Value, n: usize) -> bool {
    seen["names"].as_array().is_some_and(|a| {
        a.len() == n
            && a.iter()
                .all(|f| f["static_gone"] == json!(true) && f["airbase_gone"] == json!(true))
    }) && match &seen["listed"] {
        // An empty Lua table is JSON `{}`.
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

// ---- Lua -------------------------------------------------------------------

fn list(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("{n:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A function that reads a static by its platform name: `false` when
/// `StaticObject.getByName` finds nothing, else whether it exists and, when
/// it does, its place (north `x`, east `z`), heading, type and name. Both
/// ways to read the object's ID are recorded: `id_` and `getID`.
const READ_STATIC: &str = r#"local function read_static(n)
  local s = StaticObject.getByName(n)
  if s == nil then return false end
  local r = { found = true, exists = s:isExist(), id_ = s.id_ }
  local ok, v = pcall(function() return s:getID() end)
  if ok then r.get_id = tonumber(v) else r.get_id_error = tostring(v) end
  if r.exists then
    local pos = s:getPosition()
    r.x = pos.p.x
    r.z = pos.p.z
    r.heading = math.atan2(pos.x.z, pos.x.x)
    r.type = s:getTypeName()
    r.name = s:getName()
  end
  return r
end
"#;

/// A read of statics by name.
fn statics_lua(names: &[String]) -> String {
    format!(
        "{READ_STATIC}local out = {{}}\nfor i, n in ipairs({{ {} }}) do out[i] = read_static(n) end\nreturn out\n",
        list(names)
    )
}

const FARP_READ: &str = r#"local p = @P@
local out = { statics = {}, listed = 0, airbase = false }
local a = Airbase.getByName(p)
if a and a:isExist() then
  local r = { type = a:getTypeName(), id = tonumber(a:getID()), id_ = a.id_ }
  local pt = a:getPoint()
  r.x = pt.x
  r.z = pt.z
  local ok, spots = pcall(function() return a:getParking() end)
  if ok and type(spots) == 'table' then r.parking = #spots else r.parking_error = tostring(spots) end
  r.coalition = a:getCoalition()
  local okw, w = pcall(function()
    local wh = a:getWarehouse()
    return { fuel = wh:getLiquidAmount(0), mk82 = wh:getItemCount("@ITEM@") }
  end)
  if okw then r.fuel = w.fuel r.mk82 = w.mk82 else r.warehouse_error = tostring(w) end
  out.airbase = r
end
for _, b in ipairs(world.getAirbases()) do
  if b:isExist() and b:getName() == p then out.listed = out.listed + 1 end
end
for i, n in ipairs({ @NAMES@ }) do out.statics[i] = read_static(n) end
return out
"#;

/// A read of airbase `p`: its ID, type, place, parking spots, coalition and
/// stock, how many existing airbases `world.getAirbases` lists under its
/// name, and a read of each of `names` as a static.
fn farp_lua(p: &str, names: &[String]) -> String {
    let body = FARP_READ
        .replace("@P@", &format!("{p:?}"))
        .replace("@ITEM@", ITEM)
        .replace("@NAMES@", &list(names));
    format!("{READ_STATIC}{body}")
}

const SEARCH: &str = r#"local out = { found = {} }
local ok, err = pcall(function()
  local y = 0
  local okh, h = pcall(land.getHeight, { x = @X@, y = @Z@ })
  if okh then y = h end
  local volume = { id = world.VolumeType.SPHERE, params = { point = { x = @X@, y = y, z = @Z@ }, radius = @R@ } }
  world.searchObjects(Object.Category.STATIC, volume, function(o)
    local okn, n = pcall(function() return o:getName() end)
    out.found[#out.found + 1] = { name = okn and n or nil, id_ = o.id_, exists = o:isExist() }
    return true
  end)
end)
out.ok = ok
if not ok then out.error = tostring(err) end
return out
"#;

/// The statics `world.searchObjects` finds within `r` meters of a place.
fn search_lua(at: (f64, f64), r: f64) -> String {
    SEARCH
        .replace("@X@", &at.0.to_string())
        .replace("@Z@", &at.1.to_string())
        .replace("@R@", &r.to_string())
}

/// Add the item the controller never set to airbase `p`'s warehouse, and
/// read its count.
fn add_item_lua(p: &str) -> String {
    format!(
        "local w = Airbase.getByName({p:?}):getWarehouse()\nw:addItem(\"{ITEM}\", {ITEM_COUNT})\nreturn w:getItemCount(\"{ITEM}\")\n"
    )
}

/// For each name, whether DCS holds no live static and no live airbase under
/// it, and which of the names `world.getAirbases` lists as existing.
fn gone_lua(names: &[String]) -> String {
    format!(
        "local names = {{ {} }}\nlocal want = {{}}\nlocal out = {{ names = {{}}, listed = {{}} }}\n\
         for i, n in ipairs(names) do\n  want[n] = true\n  local s = StaticObject.getByName(n)\n  local a = Airbase.getByName(n)\n  \
         out.names[i] = {{ static_gone = s == nil or not s:isExist(), airbase_gone = a == nil or not a:isExist() }}\nend\n\
         for _, b in ipairs(world.getAirbases()) do\n  local n = b:getName()\n  if want[n] and b:isExist() then out.listed[#out.listed + 1] = n end\nend\n\
         return out\n",
        list(names)
    )
}

/// The surface type under each place.
fn surface_lua(places: &[(String, (f64, f64))]) -> String {
    let pts: Vec<String> = places
        .iter()
        .map(|(_, p)| format!("{{{}, {}}}", p.0, p.1))
        .collect();
    format!(
        "local out = {{}}\nfor i, p in ipairs({{ {} }}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n",
        pts.join(", ")
    )
}

/// The retype probe's snippet: `@DO@` is `farp`, `helipad`, `read` or
/// `destroy`. `farp` and `helipad` spawn a one-unit heliport group of that
/// type under the probe's name with `coalition.addGroup(country, -1, ...)`,
/// as the reconciler spawns a single heliport, and every action but
/// `destroy` returns a read of the name: the airbase's type, `getID()`,
/// object `id_` and spot count, the static's type and `id_`, and the
/// surface type at the place.
const RETYPE_PROBE: &str = r#"local n = @NAME@
local what = '@DO@'
local function try(f)
  local ok, v = pcall(f)
  if ok then return v end
  return 'raised: ' .. tostring(v)
end
local function read()
  local r = { listed = 0, surface = try(function() return land.getSurfaceType({ x = @X@, y = @Z@ }) end) }
  local a = Airbase.getByName(n)
  if a and a:isExist() then
    r.airbase = {
      type = try(function() return a:getTypeName() end),
      get_id = try(function() return tonumber(a:getID()) end),
      id_ = a.id_,
      parking = try(function() return #a:getParking() end),
    }
  else
    r.airbase = false
  end
  local s = StaticObject.getByName(n)
  if s and s:isExist() then
    r.static = { type = try(function() return s:getTypeName() end), id_ = s.id_ }
  else
    r.static = false
  end
  for _, b in ipairs(world.getAirbases()) do
    if b:isExist() and b:getName() == n then r.listed = r.listed + 1 end
  end
  return r
end
if what == 'farp' or what == 'helipad' then
  local t = what == 'farp' and 'FARP' or 'SINGLE_HELIPAD'
  local unit = { name = n, type = t, category = 'Heliports', x = @X@, y = @Z@, heading = 0 }
  local made = try(function()
    local g = coalition.addGroup(@COUNTRY@, -1, { name = n .. '-group', units = { unit } })
    return g and g.id_ or 'no object'
  end)
  local r = read()
  r.made = made
  return r
elseif what == 'destroy' then
  local out = { static = false, airbase = false }
  local s = StaticObject.getByName(n)
  if s and s:isExist() then out.static = try(function() s:destroy() return true end) end
  local a = Airbase.getByName(n)
  if a and a:isExist() then out.airbase = try(function() a:destroy() return true end) end
  return out
end
return read()
"#;

/// The retype probe's snippet for `action` at `at`.
fn retype_probe_lua(action: &str, at: (f64, f64)) -> String {
    RETYPE_PROBE
        .replace("@NAME@", &format!("{RETYPE_PROBE_NAME:?}"))
        .replace("@DO@", action)
        .replace("@X@", &at.0.to_string())
        .replace("@Z@", &at.1.to_string())
        .replace("@COUNTRY@", &COUNTRY.to_string())
}

const PAUSED_LUA: &str = "return DCS.getPause()\n";

fn set_pause_lua(want: bool) -> String {
    format!("DCS.setPause({want})\nreturn DCS.getPause()\n")
}

// ---- API -------------------------------------------------------------------

fn put(
    api: &mut Api,
    c: u64,
    what: &str,
    id: u64,
    spec: pb::StaticSpec,
) -> Result<pb::PutStaticResponse, String> {
    let req = pb::PutStaticRequest {
        write: api.write(what, c),
        static_id: id,
        spec: Some(spec),
    };
    api.rt
        .block_on(api.commands.put_static(req))
        .map(tonic::Response::into_inner)
        .map_err(status_text("PutStatic"))
}

/// Set the airbase's jet fuel, and its owner when `owner` is set.
fn own_and_stock(api: &mut Api, c: u64, airbase: &str, owner: bool) -> Result<(), String> {
    if owner {
        let req = pb::SetAirbaseOwnerRequest {
            write: api.write("owner", c),
            theater: THEATER.into(),
            airbase: airbase.into(),
            owner: OWNER as i32,
        };
        api.rt
            .block_on(api.commands.set_airbase_owner(req))
            .map_err(status_text("SetAirbaseOwner"))?;
    }
    let req = pb::SetWarehouseStockRequest {
        write: api.write("stock", c),
        theater: THEATER.into(),
        airbase: airbase.into(),
        liquids: [(0u32, FUEL_KG)].into_iter().collect(),
        ..pb::SetWarehouseStockRequest::default()
    };
    api.rt
        .block_on(api.commands.set_warehouse_stock(req))
        .map_err(status_text("SetWarehouseStock"))?;
    Ok(())
}

fn remove_entity(api: &mut Api, c: u64, id: u64) -> Result<(), String> {
    let req = pb::RemoveEntityRequest {
        write: api.write("remove", c),
        entity_id: id,
    };
    api.rt
        .block_on(api.commands.remove_entity(req))
        .map_err(status_text("RemoveEntity"))?;
    Ok(())
}

/// The campaign's statics in the World: ID, managed flag, type, place,
/// heading, state, spec revision and last error.
fn world_statics(api: &mut Api, c: u64) -> Result<Value, String> {
    let mut s = api
        .rt
        .block_on(api.world.list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: c,
                ..pb::WorldFilter::default()
            }),
        }))
        .map_err(status_text("ListWorld"))?
        .into_inner();
    let mut statics = Vec::new();
    while let Some(page) = api
        .rt
        .block_on(s.message())
        .map_err(status_text("ListWorld"))?
    {
        for e in page.entities {
            if let Some(entity::Kind::Static(g)) = e.kind {
                let at = g.position.clone().unwrap_or_default();
                statics.push(json!({
                    "id": g.id,
                    "managed": g.managed,
                    "type": g.r#type,
                    "north": at.north,
                    "east": at.east,
                    "heading": g.heading,
                    "state": g.state,
                    "spec_revision": g.spec_revision,
                    "last_error": g.last_error,
                }));
            }
        }
    }
    Ok(json!(statics))
}

// ---- waiting ---------------------------------------------------------------

/// Poll `check` until it gives `Some` or `limit` passes, sleeping
/// [`POLL`] between checks and keeping the lease. It never runs the
/// simulation, as the shared `wait` does.
fn poll<T>(
    api: &mut Api,
    limit: Duration,
    what: &str,
    mut check: impl FnMut(&mut Api) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(v) = check(api)? {
            return Ok(v);
        }
        if Instant::now() > deadline {
            return Err(format!("{what}: not within {limit:?}"));
        }
        api.keep();
        std::thread::sleep(POLL);
    }
}

/// Sleep for `total`, keeping the lease.
fn sleep_keep(api: &mut Api, total: Duration) {
    let end = Instant::now() + total;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        api.keep();
        std::thread::sleep(left.min(Duration::from_secs(15)));
    }
}

// ---- pause -----------------------------------------------------------------

/// The connections DCS lists.
fn players(run: &dyn Live) -> Result<i64, String> {
    run.eval("hook", "static-replace-players", PLAYERS_LUA, 60)?
        .as_i64()
        .ok_or_else(|| "no player count".into())
}

/// Pause or resume the mission, only with no player but the server's own
/// connection, and wait until the hook reads the new state.
fn set_pause(run: &dyn Live, api: &mut Api, want: bool) -> Result<Value, String> {
    let n = players(run)?;
    if n > 1 {
        return Err(format!(
            "{n} connections: a player is connected, the pause was not changed"
        ));
    }
    run.eval("hook", "static-replace-pause", &set_pause_lua(want), 60)?;
    poll(api, PAUSE_LIMIT, "the pause state", |_| {
        let now = run.eval("hook", "static-replace-paused", PAUSED_LUA, 60)?;
        Ok((now == json!(want)).then_some(json!({ "pass": true, "players": n, "paused": now })))
    })
}

/// Resume the mission if it is paused.
fn ensure_running(run: &dyn Live, api: &mut Api) -> Result<Value, String> {
    let paused = run.eval("hook", "static-replace-paused", PAUSED_LUA, 60)?;
    if paused == json!(true) {
        let mut v = set_pause(run, api, false)?;
        v["was_paused"] = json!(true);
        Ok(v)
    } else {
        Ok(json!({ "pass": true, "was_paused": false }))
    }
}

/// Put the mission in the pause state the run found.
fn restore_pause(run: &dyn Live, api: &mut Api, found: &Value) -> Result<Value, String> {
    let want = found == &json!(true);
    let now = run.eval("hook", "static-replace-paused", PAUSED_LUA, 60)?;
    if now == json!(want) {
        return Ok(json!({ "pass": true, "found": found, "changed": false }));
    }
    let mut v = set_pause(run, api, want)?;
    v["found"] = found.clone();
    v["changed"] = json!(true);
    Ok(v)
}

// ---- the run ---------------------------------------------------------------

/// Run the part; the result goes under `static_replace`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    results: &mut Value,
) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);
    let found_paused = run.eval("hook", "static-replace-paused", PAUSED_LUA, 60)?;
    println!("== static-replace: a campaign on {THEATER}");
    let c = api.create("static-replace-live", &[THEATER], true)?;
    loaded(run, &mut api, THEATER, generation)?;
    api.renew()?;

    let mut made = Made::default();
    let mut v = steps(run, &mut api, c, &mut made);
    let restored = recorded(restore_pause(run, &mut api, &found_paused));
    v["pass"] = json!(v["pass"] == json!(true) && restored["pass"] == json!(true));
    v["restore_pause"] = restored;
    v["pause_found"] = found_paused;
    println!(
        "   {}",
        if v["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        }
    );
    results["static_replace"] = v;
    Ok(())
}

/// A step's result for the record: an error is a failed step.
fn recorded(r: Result<Value, String>) -> Value {
    r.unwrap_or_else(|e| json!({ "pass": false, "error": e }))
}

/// Record a step's result under `key`; an error is recorded and given back.
fn step(rec: &mut Value, key: &str, r: Result<Value, String>) -> Result<(), String> {
    match r {
        Ok(v) => {
            rec["steps"][key] = v;
            Ok(())
        }
        Err(e) => {
            rec["steps"][key] = json!({ "pass": false, "error": e });
            Err(e)
        }
    }
}

fn steps(run: &dyn Live, api: &mut Api, c: u64, made: &mut Made) -> Value {
    println!("== static-replace: the agent's sampling report");
    let report_before = report(run, api);
    let mut variants = Vec::new();
    for (i, paused) in [false, true].into_iter().enumerate() {
        println!(
            "== static-replace: the {} variant",
            if paused { "paused" } else { "running" }
        );
        variants.push(variant(run, api, c, made, i, paused));
    }
    // The mission runs again before the report and the removal.
    let running = recorded(ensure_running(run, api));
    println!("== static-replace: the agent's sampling report, with heliports standing");
    let report_after = report(run, api);
    println!("== static-replace: SINGLE_HELIPAD over FARP, an unmanaged probe");
    let probe = recorded(retype_probe(run, api));
    println!("== static-replace: remove everything");
    let removal = recorded(remove_all(run, api, c, made));
    let reports_ok = report_before.is_ok() && report_after.is_ok();
    let pass = variants.iter().all(|v| v["pass"] == json!(true))
        && running["pass"] == json!(true)
        && probe["pass"] == json!(true)
        && removal["pass"] == json!(true)
        && reports_ok;
    json!({
        "pass": pass,
        "variants": variants,
        "running_again": running,
        "retype_probe": probe,
        "report_before": recorded_report(report_before),
        "report_after": recorded_report(report_after),
        "removal": removal,
    })
}

fn recorded_report(r: Result<Value, String>) -> Value {
    r.unwrap_or_else(|e| json!({ "error": e }))
}

/// One variant: its steps are recorded under `steps`; an error stops it and
/// is recorded under `error`.
fn variant(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    made: &mut Made,
    index: usize,
    paused: bool,
) -> Value {
    let cx = Ctx {
        run,
        c,
        at: Places::of(index),
        paused,
    };
    let mut rec = json!({
        "variant": if paused { "paused" } else { "running" },
        "steps": {},
    });
    if let Err(e) = variant_steps(&cx, api, made, &mut rec) {
        rec["error"] = json!(e);
    }
    rec["pass"] = json!(rec.get("error").is_none() && all_pass(&rec["steps"]));
    rec
}

fn variant_steps(cx: &Ctx, api: &mut Api, made: &mut Made, rec: &mut Value) -> Result<(), String> {
    step(rec, "start_running", ensure_running(cx.run, api))?;
    let surface = cx.run.eval(
        "server",
        "static-replace-surface",
        &surface_lua(&cx.at.all()),
        60,
    );
    step(
        rec,
        "surface",
        surface.map(|s| {
            let labeled: Vec<Value> = cx
                .at
                .all()
                .iter()
                .enumerate()
                .map(
                    |(i, (l, p))| json!({ "place": l, "north": p.0, "east": p.1, "surface": s[i] }),
                )
                .collect();
            json!({ "pass": true, "places": labeled })
        }),
    )?;
    let (fort, cargo_obj, pad) = create(cx, api, made, rec)?;
    step(rec, "stock", stock(cx, api, &pad))?;
    let mut paused_now = false;
    if cx.paused {
        step(rec, "pause", set_pause(cx.run, api, true))?;
        paused_now = true;
    }
    let inner = after_pause(cx, api, made, &fort, cargo_obj.as_ref(), &pad, rec);
    if paused_now {
        let resumed = step(rec, "resume", set_pause(cx.run, api, false));
        return inner.and(resumed);
    }
    inner
}

/// Steps 2d to 2f of the brief: the fortification, the cargo and the FARP.
fn after_pause(
    cx: &Ctx,
    api: &mut Api,
    made: &mut Made,
    fort: &Obj,
    cargo_obj: Option<&Obj>,
    pad: &Obj,
    rec: &mut Value,
) -> Result<(), String> {
    step(rec, "fortification", fortification_step(cx, api, fort))?;
    if let Some(k) = cargo_obj {
        step(rec, "cargo", cargo_step(cx, api, k))?;
    }
    step(rec, "farp", farp_steps(cx, api, made, pad))
}

/// Make the fortification, the cargo (running variant) and the FARP, and
/// wait until DCS shows each and the World lists each `LIVE`. Recorded under
/// `create`; an error is given back when something did not show.
fn create(
    cx: &Ctx,
    api: &mut Api,
    made: &mut Made,
    rec: &mut Value,
) -> Result<(Obj, Option<Obj>, Obj), String> {
    let mut put_one = |api: &mut Api, what: &str, spec: pb::StaticSpec| -> Result<Obj, String> {
        let r = put(api, cx.c, what, 0, spec)?;
        Ok(made.static_(cx.c, r.static_id))
    };
    let fort = put_one(api, "fort", fortification(cx.at.fort, 0.0));
    let cargo_obj = if cx.paused {
        Ok(None)
    } else {
        put_one(api, "cargo", cargo(cx.at.cargo)).map(Some)
    };
    let pad = put_one(api, "pad", heliport(cx.at.farp, FARP, 0));
    let (fort, cargo_obj, pad) = match (fort, cargo_obj, pad) {
        (Ok(f), Ok(k), Ok(p)) => (f, k, p),
        (f, k, p) => {
            let e = [f.err(), k.err(), p.err()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
            rec["steps"]["create"] = json!({ "pass": false, "error": e });
            return Err(e);
        }
    };
    let mut names = vec![fort.name.clone()];
    names.extend(cargo_obj.iter().map(|k| k.name.clone()));
    let mut watch = vec![pad.name.clone()];
    watch.extend(names.iter().cloned());
    let read = farp_lua(&pad.name, &watch);
    let ids: Vec<u64> = std::iter::once(fort.id)
        .chain(cargo_obj.iter().map(|k| k.id))
        .chain([pad.id])
        .collect();
    let mut last = Value::Null;
    let shown = poll(api, SHOW_LIMIT, "the statics and the airbase", |api| {
        let seen = cx.run.eval("server", "static-replace-create", &read, 60)?;
        let world = world_statics(api, cx.c)?;
        let present = seen["statics"]
            .as_array()
            .is_some_and(|a| a.iter().all(|s| s["exists"] == json!(true)));
        let live = ids.iter().all(|id| row_live(&world, *id, None));
        let ok = present && seen["airbase"].is_object() && live;
        last = json!({ "seen": seen, "world": world });
        Ok(ok.then_some(()))
    });
    let pass = shown.is_ok();
    rec["steps"]["create"] = json!({
        "pass": pass,
        "ids": { "fortification": fort.id, "cargo": cargo_obj.as_ref().map(|k| k.id), "farp": pad.id },
        "names": watch,
        "read": last,
        "error": shown.as_ref().err(),
    });
    shown?;
    Ok((fort, cargo_obj, pad))
}

/// Capture airbase `p` for coalition 1 in DCS, under `pcall`.
fn capture_lua(p: &str) -> String {
    format!(
        "local ok, err = pcall(function() local a = Airbase.getByName({p:?}) a:autoCapture(false) a:setCoalition({OWNER_IN_DCS}) end)\nreturn {{ ok = ok, error = (not ok) and tostring(err) or nil }}\n"
    )
}

/// Stock P's fuel and give it the owner (`SetAirbaseOwner` in the running
/// variant; in the paused variant no owner is set and DCS captures P), add
/// the item the controller never set, and wait until DCS reads the owner,
/// the fuel and the item.
fn stock(cx: &Ctx, api: &mut Api, pad: &Obj) -> Result<Value, String> {
    own_and_stock(api, cx.c, &pad.name, !cx.paused)?;
    let captured = if cx.paused {
        Some(cx.run.eval(
            "server",
            "static-replace-capture",
            &capture_lua(&pad.name),
            60,
        )?)
    } else {
        None
    };
    let added = cx.run.eval(
        "server",
        "static-replace-add-item",
        &add_item_lua(&pad.name),
        60,
    )?;
    let read = farp_lua(&pad.name, std::slice::from_ref(&pad.name));
    let mut last = Value::Null;
    let r = poll(api, SHOW_LIMIT, "the owner, the fuel and the item", |_| {
        let seen = cx.run.eval("server", "static-replace-stock", &read, 60)?;
        let ok = stock_first(&seen["airbase"]);
        last = seen;
        Ok(ok.then_some(()))
    });
    Ok(json!({
        "pass": r.is_ok(),
        "item_count_after_add": added,
        "captured_in_dcs": captured,
        "read": last,
        "error": r.err(),
    }))
}

/// Revise the fortification 50 m north and a quarter turn, and read it.
fn fortification_step(cx: &Ctx, api: &mut Api, fort: &Obj) -> Result<Value, String> {
    let read = statics_lua(std::slice::from_ref(&fort.name));
    let before = cx
        .run
        .eval("server", "static-replace-fort-before", &read, 60)?;
    let (old, to) = (cx.at.fort, cx.at.fort_to());
    let r = put(api, cx.c, "fort", fort.id, fortification(to, FRAC_PI_2))?;
    let mut last = Value::Null;
    let shown = poll(
        api,
        SHOW_LIMIT,
        "the fortification at its new place",
        |api| {
            let seen = cx.run.eval("server", "static-replace-fort", &read, 60)?;
            let world = world_statics(api, cx.c)?;
            let ok = stands(&seen[0], to, Some(FRAC_PI_2)) && row_live(&world, fort.id, Some(2));
            last = json!({ "seen": seen, "world": world });
            Ok(ok.then_some(()))
        },
    );
    let old_search = search_lua(old, SEARCH_M);
    let searched = cx
        .run
        .eval("server", "static-replace-fort-search", &old_search, 60)?;
    let mut out = json!({
        "put_spec_revision": r.spec_revision,
        "dcs_object_id": {
            "source": "StaticObject.getByName(name).id_; getID read too",
            "before": before[0]["id_"], "before_get_id": before[0]["get_id"],
            "after": last["seen"][0]["id_"], "after_get_id": last["seen"][0]["get_id"],
        },
        "before": before[0],
        "after": last,
        "old_place_search": searched,
        "error": shown.as_ref().err(),
    });
    let world_ok = unmanaged(&last["world"]).is_empty();
    let mut pass = shown.is_ok() && r.spec_revision == 2 && world_ok;
    out["world_unmanaged"] = json!(unmanaged(&last["world"]));
    if cx.paused && shown.is_ok() {
        sleep_keep(api, HOLD_PAUSED);
        let seen = cx
            .run
            .eval("server", "static-replace-fort-held", &read, 60)?;
        let world = world_statics(api, cx.c)?;
        let search = cx
            .run
            .eval("server", "static-replace-fort-held-search", &old_search, 60)?;
        let held_ok = stands(&seen[0], to, Some(FRAC_PI_2))
            && row_live(&world, fort.id, Some(2))
            && unmanaged(&world).is_empty();
        pass &= held_ok;
        out["held_paused"] = json!({
            "seconds": HOLD_PAUSED.as_secs(),
            "pass": held_ok,
            "seen": seen,
            "world": world,
            "old_place_search": search,
        });
    }
    out["pass"] = json!(pass);
    Ok(out)
}

/// Revise the cargo 30 m north; after DCS shows it there and a wait, the
/// World's cargo stands at the revised place.
fn cargo_step(cx: &Ctx, api: &mut Api, k: &Obj) -> Result<Value, String> {
    let read = statics_lua(std::slice::from_ref(&k.name));
    let to = cx.at.cargo_to();
    let r = put(api, cx.c, "cargo", k.id, cargo(to))?;
    let mut last = Value::Null;
    let shown = poll(api, SHOW_LIMIT, "the cargo at its new place", |_| {
        let seen = cx.run.eval("server", "static-replace-cargo", &read, 60)?;
        let ok = stands(&seen[0], to, None);
        last = seen;
        Ok(ok.then_some(()))
    });
    let mut out = json!({
        "put_spec_revision": r.spec_revision,
        "dcs": last,
        "error": shown.as_ref().err(),
    });
    let mut pass = shown.is_ok();
    if pass {
        sleep_keep(api, CARGO_WAIT);
        let seen = cx
            .run
            .eval("server", "static-replace-cargo-later", &read, 60)?;
        let world = world_statics(api, cx.c)?;
        let world_ok = row_near(&world, k.id, to);
        pass &= world_ok && stands(&seen[0], to, None);
        out["later"] = json!({ "dcs": seen, "world": world, "world_near": world_ok });
    }
    out["pass"] = json!(pass);
    Ok(out)
}

/// What the FARP steps carry from one to the next.
struct Farp {
    further: Vec<u64>,
    airbase_id: Option<f64>,
    marker: Option<u64>,
}

/// The FARP's three revisions, then the refused retype. An error in one is
/// recorded and ends the list.
fn farp_steps(cx: &Ctx, api: &mut Api, made: &mut Made, pad: &Obj) -> Result<Value, String> {
    // The first airbase's ID, before any revision.
    let first = cx.run.eval(
        "server",
        "static-replace-farp-first",
        &farp_lua(&pad.name, std::slice::from_ref(&pad.name)),
        60,
    )?;
    let mut state = Farp {
        further: Vec::new(),
        airbase_id: first["airbase"]["id_"].as_f64(),
        marker: None,
    };
    let mut out = Vec::new();
    let mut pass = true;
    for s in &STEPS {
        println!("   FARP {}", s.label);
        match farp_step(cx, api, made, pad, &mut state, s) {
            Ok((v, go)) => {
                pass &= v["pass"] == json!(true);
                out.push(v);
                if !go {
                    break;
                }
            }
            Err(e) => {
                pass = false;
                out.push(json!({ "step": s.label, "pass": false, "error": e }));
                break;
            }
        }
    }
    let refused = if pass {
        println!("   FARP to SINGLE_HELIPAD, refused");
        retype_refused(cx, api, pad, state.airbase_id)
            .unwrap_or_else(|e| json!({ "pass": false, "error": e }))
    } else {
        json!({ "pass": false, "error": "not tried: a step before it failed" })
    };
    Ok(json!({
        "pass": pass && out.len() == STEPS.len() && refused["pass"] == json!(true),
        "steps": out,
        "retype_refused": refused,
    }))
}

/// A heliport's type change is refused (spec/world.md, "Revising a
/// static"): `PutStatic` with `SINGLE_HELIPAD` for the single `FARP` P
/// answers `FAILED_PRECONDITION`, and DCS still shows the same `FARP`
/// airbase object with its 4 spots.
fn retype_refused(
    cx: &Ctx,
    api: &mut Api,
    pad: &Obj,
    object: Option<f64>,
) -> Result<Value, String> {
    let rev = |api: &mut Api| -> Result<Value, String> {
        let w = world_statics(api, cx.c)?;
        Ok(row(&w, pad.id).map_or(Value::Null, |r| r["spec_revision"].clone()))
    };
    let rev_before = rev(api)?;
    let answer = put(
        api,
        cx.c,
        "retype",
        pad.id,
        heliport(cx.at.farp, HELIPAD, 0),
    );
    let refused = matches!(&answer, Err(e) if e.contains("FailedPrecondition")
        && e.contains("a heliport's type does not change in place"));
    let rev_after = rev(api)?;
    let unchanged = rev_before.is_number() && rev_before == rev_after;
    let seen = cx.run.eval(
        "server",
        "static-replace-farp-retype",
        &farp_lua(&pad.name, std::slice::from_ref(&pad.name)),
        60,
    )?;
    let ab = &seen["airbase"];
    let same = ab["id_"].as_f64().is_some() && ab["id_"].as_f64() == object;
    let farp =
        ab["type"] == json!(FARP) && ab["parking"].as_f64() == Some(parking_wanted(FARP, 1) as f64);
    Ok(json!({
        "pass": refused && unchanged && same && farp,
        "refused": refused,
        "spec_revision": { "before": rev_before, "after": rev_after },
        "answer": answer.err(),
        "same_object": same,
        "farp_with_its_spots": farp,
        "seen": seen,
    }))
}

/// One revision of the FARP. The flag says whether the next may run.
fn farp_step(
    cx: &Ctx,
    api: &mut Api,
    made: &mut Made,
    pad: &Obj,
    state: &mut Farp,
    s: &Step,
) -> Result<(Value, bool), String> {
    let names = |ids: &[u64]| -> Vec<String> { ids.iter().map(|id| dcs_name(cx.c, *id)).collect() };
    let mut current = vec![pad.name.clone()];
    current.extend(names(&state.further));
    let before = cx.run.eval(
        "server",
        "static-replace-farp-before",
        &farp_lua(&pad.name, &current),
        60,
    )?;
    let airbase_before = state.airbase_id;
    if state.marker.is_none() {
        let m = put(api, cx.c, "marker", 0, fortification(cx.at.marker, 0.0))?;
        made.static_(cx.c, m.static_id);
        state.marker = Some(m.static_id);
    }
    let fresh_from = state.marker.map_or(0, |m| m + 1);
    let plan = further_plan(&state.further, s.further, fresh_from);
    let present = names(&plan.ids);
    let dropped = names(&plan.dropped);
    for n in present.iter().chain(&dropped) {
        made.name(n.clone());
    }
    let r = put(
        api,
        cx.c,
        "pad",
        pad.id,
        heliport(cx.at.farp, s.kind, s.further),
    )?;
    state.further = plan.ids.clone();
    let mut watch = vec![pad.name.clone()];
    watch.extend(present.iter().cloned());
    watch.extend(dropped.iter().cloned());
    let read = farp_lua(&pad.name, &watch);
    let mut last = Value::Null;
    let shown = poll(api, SHOW_LIMIT, "the new airbase", |api| {
        let seen = cx.run.eval("server", "static-replace-farp", &read, 60)?;
        let world = world_statics(api, cx.c)?;
        let live = row_live(&world, pad.id, Some(r.spec_revision));
        let verdict = show_verdict(s, &pad.name, airbase_before, dropped.len(), &seen, live);
        let ok = verdict["pass"] == json!(true);
        last = json!({ "seen": seen, "world": world, "verdict": verdict });
        Ok(ok.then_some(()))
    });
    let airbase_after = last["seen"]["airbase"]["id_"].as_f64();
    state.airbase_id = airbase_after.or(state.airbase_id);
    let mut out = json!({
        "step": s.label,
        "kind": s.kind,
        "further_units": s.further,
        "put_spec_revision": r.spec_revision,
        "airbase_object_before": airbase_before,
        "airbase_object_after": airbase_after,
        "airbase_get_id_after": last["seen"]["airbase"]["id"],
        "static_object_id_before": before["statics"][0]["id_"],
        "static_object_id_after": last["seen"]["statics"][0]["id_"],
        "parking_wanted": parking_wanted(s.kind, 1 + s.further),
        "parking": last["seen"]["airbase"]["parking"],
        "further_names": present,
        "dropped_names": dropped,
        "before": before,
        "shown": last,
        "error": shown.as_ref().err(),
    });
    if shown.is_err() {
        out["pass"] = json!(false);
        return Ok((out, false));
    }
    // The sidecar sends the owner and the stock again once it samples the
    // new airbase's warehouse.
    let stock_read = farp_lua(&pad.name, std::slice::from_ref(&pad.name));
    let mut last_stock = Value::Null;
    let back = poll(api, STOCK_LIMIT, "the owner and the fuel again", |_| {
        let seen = cx
            .run
            .eval("server", "static-replace-farp-stock", &stock_read, 60)?;
        let ok = stock_back(&seen["airbase"]);
        last_stock = seen;
        Ok(ok.then_some(()))
    });
    out["stock_back"] =
        json!({ "pass": back.is_ok(), "read": last_stock, "error": back.as_ref().err() });
    let mut pass = back.is_ok();
    if pass {
        sleep_keep(api, SETTLE);
        let seen = cx
            .run
            .eval("server", "static-replace-farp-settled", &stock_read, 60)?;
        let ok = stock_settled(&seen["airbase"]);
        pass &= ok;
        out["settled"] = json!({ "seconds": SETTLE.as_secs(), "pass": ok, "read": seen });
    }
    out["pass"] = json!(pass);
    Ok((out, true))
}

/// Where the retype probe stands.
fn probe_place() -> (f64, f64) {
    let f = Places::of(0).farp;
    (f.0 + PROBE_NORTH_M, f.1)
}

/// The retype probe (step 5 of the module's list). It passes when DCS read
/// the first `FARP` with its 4 spots, listed once, and holds nothing under
/// the name after the destroy. Every read after the `SINGLE_HELIPAD` spawn
/// is recorded, not judged.
fn retype_probe(run: &dyn Live, api: &mut Api) -> Result<Value, String> {
    let at = probe_place();
    let ev = |what: &str| {
        run.eval(
            "server",
            "static-replace-probe",
            &retype_probe_lua(what, at),
            60,
        )
    };
    // Run 3 found the mission paused during the probe, and DCS takes a
    // destroy only while the simulation runs: the probe resumes it first,
    // and again before the destroy.
    let running = recorded(ensure_running(run, api));
    let reads = (|| -> Result<Value, String> {
        let farp = ev("farp")?;
        sleep_keep(api, PROBE_WAIT);
        let farp_later = ev("read")?;
        let helipad = ev("helipad")?;
        sleep_keep(api, PROBE_WAIT);
        let helipad_later = ev("read")?;
        Ok(json!({
            "farp": farp,
            "farp_later": farp_later,
            "helipad": helipad,
            "helipad_later": helipad_later,
        }))
    })();
    // The destroy runs whatever the reads gave.
    let running_at_destroy = recorded(ensure_running(run, api));
    let destroyed = ev("destroy");
    let gone_read = gone_lua(&[RETYPE_PROBE_NAME.to_string()]);
    let mut last = Value::Null;
    let gone = poll(api, SHOW_LIMIT, "nothing under the probe's name", |_| {
        let seen = run.eval("server", "static-replace-probe-gone", &gone_read, 60)?;
        let ok = removal_done(&seen, 1);
        last = seen;
        Ok(ok.then_some(()))
    });
    let mut out = json!({
        "name": RETYPE_PROBE_NAME,
        "north": at.0,
        "east": at.1,
        "seconds_between_reads": PROBE_WAIT.as_secs(),
        "running": running,
        "running_at_destroy": running_at_destroy,
        "destroy": recorded(destroyed),
        "gone": { "pass": gone.is_ok(), "read": last, "error": gone.as_ref().err() },
    });
    let read_ok = match reads {
        Ok(r) => {
            let ok = probe_read_ok(&r);
            out["reads"] = r;
            ok
        }
        Err(e) => {
            out["reads"] = json!({ "error": e });
            false
        }
    };
    out["pass"] = json!(read_ok && gone.is_ok());
    Ok(out)
}

/// Whether the probe's starting point held: the `FARP` with its spots and
/// listed once, at once and later. What the `SINGLE_HELIPAD` spawn gave is
/// the finding, recorded and not judged.
fn probe_read_ok(r: &Value) -> bool {
    let farp = |v: &Value| {
        v["airbase"]["type"] == json!(FARP)
            && v["airbase"]["parking"].as_f64() == Some(FARP_SPOTS as f64)
            && v["listed"] == json!(1)
    };
    farp(&r["farp"]) && farp(&r["farp_later"])
}

/// Remove every entity the run made with the mission running, and wait until
/// DCS holds nothing live under any of their names.
fn remove_all(run: &dyn Live, api: &mut Api, c: u64, made: &Made) -> Result<Value, String> {
    api.keep();
    let running = recorded(ensure_running(run, api));
    let mut errors = Vec::new();
    for id in &made.ids {
        if let Err(e) = remove_entity(api, c, *id) {
            errors.push(e);
        }
    }
    let read = gone_lua(&made.names);
    let mut last = Value::Null;
    let waited = poll(api, SHOW_LIMIT, "every name gone", |_| {
        let seen = run.eval("server", "static-replace-gone", &read, 60)?;
        let ok = removal_done(&seen, made.names.len());
        last = seen;
        Ok(ok.then_some(()))
    });
    let world = world_statics(api, c);
    Ok(json!({
        "pass": errors.is_empty() && waited.is_ok() && running["pass"] == json!(true),
        "running": running,
        "removed": made.ids,
        "names": made.names,
        "errors": errors,
        "read": last,
        "world_after": world.as_ref().ok(),
        "error": waited.err(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(x: f64, z: f64, heading: f64) -> Value {
        json!({ "found": true, "exists": true, "x": x, "z": z, "heading": heading })
    }

    #[test]
    fn a_static_stands_within_a_meter_and_a_hundredth_of_a_radian() {
        let at = (-289_050.0, 681_000.0);
        assert!(stands(&read(at.0, at.1, FRAC_PI_2), at, Some(FRAC_PI_2)));
        assert!(stands(&read(at.0 + 0.9, at.1, 0.0), at, None));
        assert!(!stands(&read(at.0 + 1.1, at.1, 0.0), at, None));
        assert!(!stands(
            &read(at.0, at.1, FRAC_PI_2 + 0.02),
            at,
            Some(FRAC_PI_2)
        ));
        assert!(!stands(&read(at.0, at.1, 0.0), at, Some(FRAC_PI_2)));
        let mut gone = read(at.0, at.1, 0.0);
        gone["exists"] = json!(false);
        assert!(!stands(&gone, at, None));
        assert!(!stands(&json!(false), at, None));
    }

    #[test]
    fn headings_compare_around_the_turn() {
        assert!(heading_diff(-FRAC_PI_2, 3.0 * FRAC_PI_2) < 1e-9);
        assert!((heading_diff(0.1, TAU - 0.1) - 0.2).abs() < 1e-9);
        assert!((heading_diff(0.0, std::f64::consts::PI) - std::f64::consts::PI).abs() < 1e-9);
    }

    #[test]
    fn parking_is_four_per_farp_and_one_per_helipad() {
        assert_eq!(parking_wanted(FARP, 1), 4);
        assert_eq!(parking_wanted(FARP, 4), 16);
        assert_eq!(parking_wanted(FARP, 2), 8);
        assert_eq!(parking_wanted(HELIPAD, 1), 1);
        assert_eq!(parking_wanted(HELIPAD, 3), 3);
    }

    #[test]
    fn further_ids_keep_their_index_and_a_growth_takes_the_fresh_ones() {
        let grow = further_plan(&[], 3, 11);
        assert_eq!(grow.ids, vec![11, 12, 13]);
        assert!(grow.dropped.is_empty());
        let shrink = further_plan(&[11, 12, 13], 1, 99);
        assert_eq!(shrink.ids, vec![11]);
        assert_eq!(shrink.dropped, vec![12, 13]);
        let none = further_plan(&[11], 0, 99);
        assert!(none.ids.is_empty());
        assert_eq!(none.dropped, vec![11]);
        let same = further_plan(&[], 0, 99);
        assert_eq!(
            same,
            FurtherPlan {
                ids: vec![],
                dropped: vec![]
            }
        );
        let partial = further_plan(&[11, 12], 3, 40);
        assert_eq!(partial.ids, vec![11, 12, 40]);
    }

    #[test]
    fn the_steps_run_in_the_briefed_order() {
        let got: Vec<_> = STEPS.iter().map(|s| (s.label, s.further, s.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("1 to 4", 3, FARP),
                ("4 to 2", 1, FARP),
                ("2 to 1", 0, FARP),
            ]
        );
    }

    #[test]
    fn the_variants_stand_3_km_apart_and_keep_their_objects_in_their_own_ground() {
        let (a, b) = (Places::of(0), Places::of(1));
        assert_eq!(b.fort.1 - a.fort.1, 3_000.0);
        let reach = |p: &Places| {
            p.all()
                .iter()
                .map(|(_, at)| at.1)
                .fold((f64::MAX, f64::MIN), |(lo, hi), e| (lo.min(e), hi.max(e)))
        };
        assert!(reach(&a).1 < reach(&b).0);
        assert_eq!(a.fort.0, -289_000.0);
        assert_eq!(a.fort.1, 681_000.0);
        assert_eq!(a.fort_to().0 - a.fort.0, 50.0);
        assert_eq!(a.cargo_to().0 - a.cargo.0, 30.0);
        // The surface list holds every unit of the largest group.
        assert_eq!(a.all().len(), 4 + 4 + 1);
    }

    #[test]
    fn the_specs_carry_their_categories_and_units() {
        let f = fortification((1.0, 2.0), 0.5);
        assert_eq!(
            (f.r#type.as_str(), f.country, f.heading),
            ("Hangar A", 2, 0.5)
        );
        assert!(is_category(&f, "Fortifications"));
        assert!(is_category(&cargo((1.0, 2.0)), "Cargos"));
        let h = heliport((10.0, 20.0), FARP, 3);
        assert!(is_category(&h, "Heliports"));
        assert_eq!(h.further_units.len(), 3);
        let east: Vec<f64> = h
            .further_units
            .iter()
            .map(|u| u.position.as_ref().expect("placed").east)
            .collect();
        assert_eq!(east, vec![220.0, 420.0, 620.0]);
        assert!(
            h.further_units
                .iter()
                .all(|u| u.r#type == FARP && u.extra.is_some())
        );
        assert!(heliport((0.0, 0.0), HELIPAD, 0).further_units.is_empty());
    }

    fn is_category(s: &pb::StaticSpec, want: &str) -> bool {
        matches!(
            s.extra.as_ref().and_then(|e| e.fields.get("category")).and_then(|v| v.kind.clone()),
            Some(prost_types::value::Kind::StringValue(c)) if c == want
        )
    }

    fn seen_pad(parking: f64, further: usize, dropped: usize) -> Value {
        let mut statics =
            vec![json!({ "found": true, "exists": true, "type": FARP, "name": "np#p" })];
        statics
            .extend((0..further).map(|_| json!({ "found": true, "exists": true, "name": "np#p" })));
        statics.extend((0..dropped).map(|_| json!(false)));
        json!({
            // `getID()` stays across a replace; the object `id_` changes.
            "airbase": { "id": 10, "id_": 20, "type": FARP, "parking": parking },
            "listed": 1,
            "statics": statics,
        })
    }

    #[test]
    fn a_new_airbase_with_its_spots_and_names_passes() {
        let s = &STEPS[0];
        let seen = seen_pad(16.0, 3, 0);
        assert_eq!(
            show_verdict(s, "np#p", Some(10.0), 0, &seen, true)["pass"],
            json!(true)
        );
        // Shrinking: two dropped names that find nothing or a dead object.
        let s = &STEPS[1];
        let mut seen = seen_pad(8.0, 1, 2);
        assert_eq!(
            show_verdict(s, "np#p", Some(10.0), 2, &seen, true)["pass"],
            json!(true)
        );
        seen["statics"][3] = json!({ "found": true, "exists": false });
        assert_eq!(
            show_verdict(s, "np#p", Some(10.0), 2, &seen, true)["pass"],
            json!(true)
        );
    }

    #[test]
    fn a_revision_that_shows_the_old_airbase_or_a_wrong_group_fails() {
        let s = &STEPS[0];
        let good = seen_pad(16.0, 3, 0);
        let mut cases = vec![
            ("same airbase id", good.clone(), Some(20.0), true),
            ("no id before", good.clone(), None, true),
            ("world not live", good.clone(), Some(10.0), false),
            ("15 spots", seen_pad(15.0, 3, 0), Some(10.0), true),
        ];
        let mut v = good.clone();
        v["listed"] = json!(2);
        cases.push(("listed twice", v, Some(10.0), true));
        let mut v = good.clone();
        v["airbase"]["type"] = json!(HELIPAD);
        cases.push(("another type", v, Some(10.0), true));
        let mut v = good.clone();
        v["statics"][2]["name"] = json!("other");
        cases.push(("a name finds another object", v, Some(10.0), true));
        let mut v = good.clone();
        v["statics"][3] = json!(false);
        cases.push(("a name finds nothing", v, Some(10.0), true));
        let mut v = good.clone();
        v["airbase"] = json!(false);
        cases.push(("no airbase", v, Some(10.0), true));
        let mut v = good.clone();
        v["statics"].as_array_mut().expect("list").pop();
        cases.push(("a read cut short", v, Some(10.0), true));
        let mut v = good.clone();
        v["statics"][0] = json!(false);
        cases.push(("P finds nothing", v, Some(10.0), true));
        for (what, v, before, live) in cases {
            assert_eq!(
                show_verdict(s, "np#p", before, 0, &v, live)["pass"],
                json!(false),
                "{what}"
            );
        }
        let shrink = &STEPS[1];
        let mut v = seen_pad(8.0, 1, 2);
        v["statics"][2] = json!({ "found": true, "exists": true, "name": "np#p" });
        assert_eq!(
            show_verdict(shrink, "np#p", Some(10.0), 2, &v, true)["pass"],
            json!(false),
            "a dropped name still finds a live object"
        );
    }

    #[test]
    fn the_stock_reads_are_judged_in_their_phase() {
        let ab = |coalition: i64, fuel: f64, mk82: f64| json!({ "coalition": coalition, "fuel": fuel, "mk82": mk82 });
        assert!(stock_first(&ab(1, 5000.0, 10.0)));
        assert!(!stock_first(&ab(1, 5000.0, 0.0)));
        assert!(stock_back(&ab(1, 5000.0, 0.0)));
        assert!(!stock_back(&ab(3, 5000.0, 0.0)));
        assert!(!stock_back(&ab(1, 0.0, 0.0)));
        assert!(stock_settled(&ab(1, 5000.0, 10.0)));
        assert!(!stock_settled(&ab(1, 5000.0, 0.0)));
        assert!(!stock_settled(&ab(3, 5000.0, 10.0)));
        assert!(!stock_back(&json!(false)));
    }

    fn world() -> Value {
        json!([
            { "id": 1, "managed": true, "state": 3, "spec_revision": 2, "north": 100.0, "east": 200.0 },
            { "id": 2, "managed": false, "state": 3, "spec_revision": 1, "north": 0.0, "east": 0.0 },
            { "id": 3, "managed": true, "state": 6, "spec_revision": 2 },
        ])
    }

    #[test]
    fn the_world_reads_are_judged_by_id_state_revision_and_place() {
        let w = world();
        assert!(row_live(&w, 1, Some(2)));
        assert!(row_live(&w, 1, None));
        assert!(!row_live(&w, 1, Some(3)));
        assert!(!row_live(&w, 3, None));
        assert!(!row_live(&w, 9, None));
        assert!(row_near(&w, 1, (100.5, 200.0)));
        assert!(!row_near(&w, 1, (102.0, 200.0)));
        assert!(!row_near(&w, 3, (0.0, 0.0)));
        assert_eq!(unmanaged(&w), vec![2]);
        assert!(unmanaged(&json!([{ "id": 1, "managed": true }])).is_empty());
    }

    #[test]
    fn the_removal_is_done_when_every_name_is_gone_and_none_is_listed() {
        let gone = json!({ "static_gone": true, "airbase_gone": true });
        let seen = json!({ "names": [gone, gone], "listed": [] });
        assert!(removal_done(&seen, 2));
        assert!(!removal_done(&seen, 3));
        let mut v = seen.clone();
        v["listed"] = json!({});
        assert!(removal_done(&v, 2));
        v["listed"] = json!(["np#p"]);
        assert!(!removal_done(&v, 2));
        let mut v = seen.clone();
        v["names"][1]["static_gone"] = json!(false);
        assert!(!removal_done(&v, 2));
        let mut v = seen.clone();
        v["names"][0]["airbase_gone"] = json!(false);
        assert!(!removal_done(&v, 2));
    }

    #[test]
    fn a_variant_passes_only_when_every_step_does() {
        assert!(all_pass(
            &json!({ "a": { "pass": true }, "b": { "pass": true } })
        ));
        assert!(!all_pass(
            &json!({ "a": { "pass": true }, "b": { "pass": false } })
        ));
        assert!(!all_pass(&json!({ "a": { "error": "x" } })));
    }

    #[test]
    fn the_snippets_name_what_they_read() {
        let names = vec!["np#1#s#2".to_string(), "np#1#s#3".to_string()];
        let lua = farp_lua("np#1#s#2", &names);
        assert!(lua.contains("local p = \"np#1#s#2\""));
        assert!(lua.contains("{ \"np#1#s#2\", \"np#1#s#3\" }"));
        assert!(lua.contains(ITEM));
        assert!(!lua.contains('@'));
        let s = search_lua((-289_050.0, 681_000.0), 5.0);
        assert!(s.contains("x = -289050") && s.contains("radius = 5"));
        assert!(!s.contains('@'));
        assert!(capture_lua("np#1#s#2").contains("setCoalition(1)"));
        assert!(add_item_lua("np#1#s#2").contains("addItem(\"weapons.bombs.Mk_82\", 10)"));
        assert!(gone_lua(&names).contains("\"np#1#s#3\""));
        for what in ["farp", "helipad", "read", "destroy"] {
            let p = retype_probe_lua(what, probe_place());
            assert!(!p.contains('@'), "{what}");
            assert!(p.contains("local n = \"np-sr-retype-probe\""));
            assert!(p.contains(&format!("local what = '{what}'")));
            assert!(p.contains("coalition.addGroup(2, -1,"));
        }
    }

    #[test]
    fn the_probe_stands_apart_from_both_variants_and_judges_its_reads() {
        let at = probe_place();
        for v in [Places::of(0), Places::of(1)] {
            for (l, p) in v.all() {
                let d = ((p.0 - at.0).powi(2) + (p.1 - at.1).powi(2)).sqrt();
                assert!(d >= MARKER_M, "{l} is {d} m from the probe");
            }
        }
        let farp = json!({ "airbase": { "type": FARP, "parking": 4 }, "listed": 1 });
        let pad = json!({ "airbase": { "type": HELIPAD, "parking": 4 }, "listed": 1 });
        let good =
            json!({ "farp": farp, "farp_later": farp, "helipad": pad, "helipad_later": pad });
        assert!(probe_read_ok(&good));
        // What the helipad spawn gave is recorded, not judged.
        let mut v = good.clone();
        v["helipad_later"]["airbase"]["parking"] = json!(1);
        assert!(probe_read_ok(&v));
        let mut v = good.clone();
        v["helipad"]["airbase"] = json!(false);
        v["helipad_later"]["listed"] = json!(2);
        assert!(probe_read_ok(&v));
        let mut v = good.clone();
        v["farp"]["airbase"]["parking"] = json!(1);
        assert!(!probe_read_ok(&v));
        let mut v = good.clone();
        v["farp_later"]["listed"] = json!(2);
        assert!(!probe_read_ok(&v));
    }
}
