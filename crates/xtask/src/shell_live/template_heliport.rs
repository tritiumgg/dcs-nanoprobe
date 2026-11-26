//! `sidecar <name> --part template-heliport --file FILE`: a static template
//! that holds a heliport static, spawned on a running DCS with
//! `PutFromStaticTemplate` (spec/world.md, "Static templates" and "Kinds").
//!
//! The run puts FILE, a static template the Mission Editor saved, into the
//! folder the part's `sidecar.toml` lists, and makes it in place. It also
//! makes the file's first heliport with `PutStatic`, from the same spec,
//! north of it: the twin. It reads each static back from DCS by its
//! platform name with `StaticObject.getByName` and `Airbase.getByName`, and
//! which of them `world.getAirbases` lists. It
//! sets a fuel stock and the red owner under each heliport's name and reads
//! both from DCS. It
//! records each static's state in the World, which must be `LIVE`. The twin
//! carries radio keys other than DCS's defaults, and each heliport's
//! frequency, modulation and callsign are read from DCS. Before the spawns
//! and once further helipads stand, it reads the agent's sampling report.
//! It removes every entity and the file, also when a step fails. On a
//! paused server it reads DCS once, resumes the mission until DCS has
//! dropped the heliports, and pauses it again: DCS takes a FARP's destroy
//! only while the simulation runs (LIVE,
//! docs/results/2026-11-25-farp-fixes.md). It then reads that the agent
//! holds each heliport's airbase entry as a tombstone. The template's
//! heliport must go every way its twin goes.
//!
//! A heliport static with further units is a heliport group: DCS makes one
//! airbase of it under the first unit's name (spec/world.md, "Heliport
//! groups"). A file's heliport group, its twin and the extra helipads, which
//! clone the twin's spec, are all read that way, and so is a group of
//! [`BIG_GROUP`] FARPs made by `PutStatic` after the extra helipads. For
//! each group the run reads that `StaticObject.getByName` finds the
//! airbase's object under every further unit's name, that `Airbase.getByName`
//! finds nothing under them, that `world.getAirbases` lists the airbase
//! once, and the airbase's parking spots: 4 for each `FARP` unit, checked
//! when every unit is a `FARP` and only recorded otherwise. The further
//! units' names are the store's IDs after the static's own, in order: the
//! store allocates a static's further IDs at once after its ID. After the
//! removal the run reads every further name again: no live static, no
//! airbase.

use std::time::Duration;

use nanoprobe_api::static_template::{self, Layout};
use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};

use super::*;

/// The file's name in the listed folder.
const FILE: &str = "np-live-heliport.stm";
/// How far a static may stand from its place, in meters. As the
/// static-template part's tolerance; the call does not move a static.
const NEAR_M: f64 = 1.0;
/// The jet fuel the run stocks at each heliport's warehouse, in kg. DCS
/// must read back this amount, which an empty warehouse does not hold.
const FUEL_KG: f64 = 5_000.0;
/// The owner the run gives each heliport's airbase: red, which the spawned
/// blue heliports are not. DCS reads it as coalition 1.
const OWNER: pb::Coalition = pb::Coalition::Red;
const OWNER_IN_DCS: i64 = 1;
/// How long the airbase and the stock may take to show. Not a design
/// limit; only when this test gives up.
const SHOW_LIMIT: Duration = Duration::from_secs(120);
/// How many helipads the run adds by `PutStatic` beyond the twin, for the
/// sampling read with several FARPs, and how far apart, in meters.
const EXTRA_PADS: usize = 4;
const EXTRA_STEP_M: f64 = 60.0;
/// How many FARPs the large group has, made by one `PutStatic` after the
/// extra helipads: many units, to read one airbase and every name finding it
/// at a size beyond a pair.
const BIG_GROUP: usize = 10;
/// The parking spots DCS gives each `FARP` of a heliport group.
const FARP_SPOTS: usize = 4;
/// The DCS type of the large group's units.
const FARP: &str = "FARP";
/// How far apart the large group's FARPs stand, in meters: the spacing the
/// Mission Editor gives a group's units (ED-CODE, `me_static.lua`), clear of
/// each FARP's parking, which reaches 62 m (LIVE,
/// docs/results/2026-11-25-farp-pairs.md).
const GROUP_STEP_M: f64 = 200.0;
/// How long the run waits before each sampling read: past one report
/// window of the agent (10 s), so the read covers the state it follows.
const REPORT_WAIT: Duration = Duration::from_secs(12);
/// The World's `LIVE` state (`LifecycleState`).
const LIVE: i64 = 3;
/// The twin's radio: values other than DCS's defaults (127.5 MHz, AM,
/// London), the frequency as text, as a file gives it.
const TWIN_RADIO: (&str, f64, f64) = ("131.25", 1.0, 4.0);
/// The callsigns DCS gives the callsign IDs the run uses (LIVE,
/// docs/results/2026-11-25-farp-fixes.md).
const CALLSIGNS: &[(i64, &str)] = &[(1, "London"), (2, "Dallas"), (3, "Paris"), (4, "Moscow")];

/// The radio DCS should read for a heliport of these further keys:
/// frequency in Hz, modulation and callsign; DCS's defaults where a key is
/// missing.
fn expected_radio(extra: &[(String, Lv)]) -> Value {
    let get = |k: &str| extra.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let mhz = match get("heliport_frequency") {
        Some(Lv::Number(n)) => Some(n),
        Some(Lv::String(t)) => std::str::from_utf8(&t)
            .ok()
            .and_then(|t| t.trim().parse().ok()),
        None => Some(127.5),
        _ => None,
    };
    let modulation = match get("heliport_modulation") {
        Some(Lv::Number(n)) => Some(n),
        None => Some(0.0),
        _ => None,
    };
    let callsign = match get("heliport_callsign_id") {
        Some(Lv::Number(n)) => CALLSIGNS
            .iter()
            .find(|(id, _)| *id as f64 == n)
            .map(|(_, c)| *c),
        None => Some("London"),
        _ => None,
    };
    json!({
        "frequency": mhz.map(|m| (m * 1e6).round()),
        "modulation": modulation,
        "callsign": callsign,
    })
}

/// Whether DCS's read of an airbase gives the radio `want`.
fn radio_matches(want: &Value, ab: &Value) -> bool {
    want["frequency"]
        .as_f64()
        .is_some_and(|f| ab["frequency"].as_f64() == Some(f))
        && want["modulation"]
            .as_f64()
            .is_some_and(|m| ab["modulation"].as_f64() == Some(m))
        && want["callsign"].is_string()
        && ab["callsign"] == want["callsign"]
}

/// The agent's sampling report fields the run records (spec/sampling.md,
/// "Reporting").
const REPORT_FIELDS: &[&str] = &[
    "share",
    "frames",
    "frames_over_250",
    "frame_max_us",
    "us_enumeration",
    "rows_enumeration",
    "enum_step_p99_us",
    "enum_step_max_us",
    "us_births",
    "rows_births",
    "us_warehouses",
    "rows_warehouses",
];

/// The agent's last sampling report in its log, after [`REPORT_WAIT`], and
/// how many airbase entries the shim holds.
pub(super) fn report(run: &dyn Live, api: &mut Api) -> Result<Value, String> {
    std::thread::sleep(REPORT_WAIT);
    api.renew()?;
    let log = run
        .profile()
        .read("nanoprobe/logs/agent.log")?
        .ok_or("no agent.log")?;
    let text = String::from_utf8_lossy(&log);
    let last = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["msg"] == json!("sampling"))
        .ok_or("no sampling report in agent.log")?;
    let mut out = serde_json::Map::new();
    out.insert("wall".into(), last["wall"].clone());
    for f in REPORT_FIELDS {
        out.insert((*f).into(), last["data"][*f].clone());
    }
    out.insert(
        "airbases".into(),
        run.eval(
            "server",
            "template-heliport-entries",
            "return __nanoprobe_shim and #__nanoprobe_shim.T.airbases\n",
            60,
        )?,
    );
    Ok(Value::Object(out))
}

/// The connections DCS lists, the server's own among them.
pub(super) const PLAYERS_LUA: &str = "return #net.get_player_list()\n";

/// For each name, whether the agent's airbase entry of a spawned FARP of
/// the name is a tombstone; `"none"` when it holds none.
fn tombstones_lua(names: &[String]) -> String {
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    format!(
        "local T = __nanoprobe_shim and __nanoprobe_shim.T\nlocal out = {{}}\n\
         for i, n in ipairs({{ {} }}) do\n  out[i] = 'none'\n  \
         for _, b in ipairs(T and T.airbases or {{}}) do if b.name == n and b.spawned then out[i] = b.gone == true end end\nend\n\
         return out\n",
        list.join(", ")
    )
}

/// Whether the file's `i`-th static makes an airbase: its `category` is
/// `Heliports`.
fn heliport(l: &Layout, i: usize) -> bool {
    l.statics[i]
        .extra
        .iter()
        .any(|(k, v)| k == "category" && matches!(v, Lv::String(s) if s.as_slice() == b"Heliports"))
}

/// A read of every static by `StaticObject.getByName` and `Airbase.getByName`,
/// with each airbase's radio, and the names `world.getAirbases` lists among
/// them that exist: it keeps listing a FARP DCS removed until the next
/// mission load (LIVE, docs/results/2026-11-25-farp-fixes.md).
fn read_lua(names: &[String]) -> String {
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    format!(
        "local names = {{ {} }}\nlocal want = {{}}\nfor _, n in ipairs(names) do want[n] = true end\n\
         local out = {{ statics = {{}}, airbases = {{}}, listed = {{}} }}\n\
         for i, n in ipairs(names) do\n  local s = StaticObject.getByName(n)\n  \
         if s and s:isExist() then local p = s:getPoint() out.statics[i] = {{ x = p.x, z = p.z, type = s:getTypeName(), country = s:getCountry() }} else out.statics[i] = false end\n  \
         local a = Airbase.getByName(n)\n  \
         if a and a:isExist() then local p = a:getPoint() local d = a:getDesc() local c = a:getCommunicator() out.airbases[i] = {{ x = p.x, z = p.z, id = tonumber(a:getID()), category = d and d.category, type = a:getTypeName(), frequency = c:getFrequency(), modulation = c:getModulation(), callsign = a:getCallsign() }} else out.airbases[i] = false end\nend\n\
         for _, b in ipairs(world.getAirbases()) do local n = b:getName() if want[n] and b:isExist() then out.listed[#out.listed + 1] = n end end\n\
         return out\n",
        list.join(", ")
    )
}

/// The verdict on DCS's read after the call: every static that is not a
/// heliport stands within [`NEAR_M`] of its place, as its type and country;
/// every heliport's airbase stands there as its type and is listed by
/// `world.getAirbases`, and so does its static, as its type and country,
/// when `StaticObject.getByName` finds one; no other static makes one.
fn verdict(l: &Layout, seen: &Value) -> Value {
    let mut pass = l.statics.len() == seen["statics"].as_array().map_or(0, Vec::len);
    let mut out = Vec::new();
    let listed = seen["listed"].as_array().cloned().unwrap_or_default();
    for (i, s) in l.statics.iter().enumerate() {
        let st = &seen["statics"][i];
        let ab = &seen["airbases"][i];
        let near = |v: &Value| {
            let (x, z) = (v["x"].as_f64(), v["z"].as_f64());
            matches!((x, z), (Some(x), Some(z)) if (x - s.x).hypot(z - s.y) <= NEAR_M)
        };
        let airbase = heliport(l, i);
        let as_type = |v: &Value| near(v) && v["type"] == json!(s.type_name);
        let as_static = |v: &Value| as_type(v) && v["country"] == json!(s.country);
        // A heliport's airbase stands where its static does, as its type.
        // `StaticObject.getByName` may not find a FARP (it is not in
        // `coalition.getStaticObjects`, LIVE, ADR 0032); when it does, the
        // static must stand there too, as its type.
        let stands = if airbase {
            as_type(ab) && (st == &json!(false) || as_static(st))
        } else {
            as_static(st)
        };
        let made_airbase = ab.is_object();
        let ok = stands
            && made_airbase == airbase
            && (!airbase || listed.iter().any(|n| n == &ab_name(seen, i)));
        pass &= ok;
        out.push(json!({ "type": s.type_name, "heliport": airbase, "stands": stands, "airbase": made_airbase, "ok": ok }));
    }
    json!({ "pass": pass, "statics": out })
}

/// The name `listed` holds for the `i`-th static: its own, filled in by the
/// driver under `names`.
fn ab_name(seen: &Value, i: usize) -> Value {
    seen["names"][i].clone()
}

/// A heliport group as DCS holds it: the airbase's name (the first unit's),
/// the names of the further units, and the DCS type of every unit.
struct Group {
    airbase: String,
    further: Vec<String>,
    types: Vec<String>,
}

/// The names of a static's `n` further units. The store allocates them at
/// once after the static's own ID `id`, in order (`create_static_group`), and
/// `PutStatic` and `PutFromStaticTemplate` create nothing between a static
/// and its further units, so they are `id + 1` to `id + n`.
fn further_names(c: u64, id: u64, n: usize) -> Vec<String> {
    (1..=n as u64)
        .map(|k| name(NameKind::Static, CampaignId(c), EntityId(id + k)))
        .collect()
}

/// The heliport groups among statics made from `specs` with these IDs, in
/// order; a static without further units is not one.
fn groups_of(c: u64, ids: &[u64], specs: &[pb::StaticSpec]) -> Vec<Group> {
    ids.iter()
        .zip(specs)
        .filter(|(_, s)| !s.further_units.is_empty() || is_heliport_spec(s))
        .map(|(id, s)| Group {
            airbase: name(NameKind::Static, CampaignId(c), EntityId(*id)),
            further: further_names(c, *id, s.further_units.len()),
            types: std::iter::once(s.r#type.clone())
                .chain(s.further_units.iter().map(|u| u.r#type.clone()))
                .collect(),
        })
        .collect()
}

/// Whether a spec's `extra.category` is `Heliports`: a single heliport is
/// sent as a one-unit group too, so the part judges its airbase the same way.
fn is_heliport_spec(s: &pb::StaticSpec) -> bool {
    s.extra
        .as_ref()
        .and_then(|e| e.fields.get("category"))
        .is_some_and(|v| {
            matches!(&v.kind, Some(prost_types::value::Kind::StringValue(c)) if c == "Heliports")
        })
}

/// How far north of a heliport group's first unit its further units reach,
/// in meters; 0 for a single heliport.
fn reach_of(spec: &pb::StaticSpec) -> f64 {
    let Some(p) = spec.position.as_ref() else {
        return 0.0;
    };
    spec.further_units
        .iter()
        .filter_map(|u| u.position.as_ref())
        .map(|q| q.north - p.north)
        .fold(0.0, f64::max)
}

/// The spec of the large group: [`BIG_GROUP`] FARPs of the twin's spec,
/// each [`GROUP_STEP_M`] north of the last, the first beyond the extra
/// helipads and the reach of their further units, so nothing overlaps.
fn big_group(spec: &pb::StaticSpec) -> pb::StaticSpec {
    let mut s = spec.clone();
    s.label = "big group".into();
    s.r#type = FARP.into();
    s.further_units.clear();
    let Some(p) = spec.position.clone() else {
        return s;
    };
    let reach = reach_of(spec);
    let first = p.north + (EXTRA_STEP_M + reach) * (EXTRA_PADS + 1) as f64 + reach;
    if let Some(q) = s.position.as_mut() {
        q.north = first;
    }
    let mut extra = spec.extra.clone();
    if let Some(e) = extra.as_mut() {
        e.fields.retain(|k, _| !k.starts_with("heliport_"));
    }
    s.further_units = (1..BIG_GROUP)
        .map(|k| pb::StaticUnit {
            r#type: FARP.into(),
            position: Some(pb::Position {
                north: first + GROUP_STEP_M * k as f64,
                ..p.clone()
            }),
            heading: spec.heading,
            extra: extra.clone(),
        })
        .collect();
    s
}

/// A read of each group by DCS: the airbase's `id_`, its parking spots, the
/// entries of `world.getAirbases` with that `id_`, and under each further
/// name the `id_` `StaticObject.getByName` finds and whether
/// `Airbase.getByName` finds anything.
fn groups_lua(groups: &[Group]) -> String {
    let list: Vec<String> = groups
        .iter()
        .map(|g| {
            let further: Vec<String> = g.further.iter().map(|n| format!("{n:?}")).collect();
            format!("{{ {:?}, {{ {} }} }}", g.airbase, further.join(", "))
        })
        .collect();
    format!(
        "local groups = {{ {} }}\nlocal listed = world.getAirbases()\nlocal out = {{}}\n\
         for i, g in ipairs(groups) do\n  local r = {{ listed = 0, further = {{}} }}\n  \
         local a = Airbase.getByName(g[1])\n  \
         if a and a:isExist() then\n    r.airbase_id = a.id_\n    local p = a:getParking()\n    r.parking = p and #p\n    \
         for _, b in ipairs(listed) do if b.id_ == a.id_ then r.listed = r.listed + 1 end end\n  end\n  \
         for j, n in ipairs(g[2]) do\n    local s = StaticObject.getByName(n)\n    \
         r.further[j] = {{ static_id = s and s.id_, airbase = Airbase.getByName(n) ~= nil }}\n  end\n  \
         out[i] = r\nend\nreturn out\n",
        list.join(", ")
    )
}

/// The verdict on DCS's read of `groups`: for each, one airbase found, every
/// further name finding the airbase's object and no airbase, the airbase
/// listed once, and 4 parking spots per unit when every unit is a `FARP`.
/// Other types' spots are recorded and not judged.
fn groups_verdict(groups: &[Group], seen: &Value) -> Value {
    let seen = seen.as_array().cloned().unwrap_or_default();
    let mut pass = groups.len() == seen.len();
    let mut out = Vec::new();
    for (g, s) in groups.iter().zip(&seen) {
        let id = s["airbase_id"].as_f64();
        let further = s["further"].as_array().cloned().unwrap_or_default();
        let finds = further.len() == g.further.len()
            && further
                .iter()
                .all(|f| id.is_some() && f["static_id"].as_f64() == id);
        let no_airbase = further.iter().all(|f| f["airbase"] == json!(false));
        let listed_once = s["listed"].as_f64() == Some(1.0);
        let farps = g.types.iter().all(|t| t == FARP);
        let want = farps.then(|| (FARP_SPOTS * g.types.len()) as f64);
        let parking_ok = want.is_none_or(|w| s["parking"].as_f64() == Some(w));
        let ok = id.is_some() && finds && no_airbase && listed_once && parking_ok;
        pass &= ok;
        out.push(json!({
            "airbase": g.airbase,
            "units": g.types.len(),
            "types": g.types,
            "further": g.further,
            "airbase_id": s["airbase_id"],
            "further_find_airbase_object": finds,
            "further_find_no_airbase": no_airbase,
            "listed": s["listed"],
            "parking": s["parking"],
            "parking_wanted": want,
            "parking_ok": parking_ok,
            "read": s["further"],
            "ok": ok,
        }));
    }
    json!({ "pass": pass, "groups": out })
}

/// Wait until DCS shows each group as one airbase (see [`groups_verdict`]),
/// or the limit passes, and give the verdict on the last read.
fn check_groups(
    run: &dyn Live,
    api: &mut Api,
    groups: &[Group],
    what: &str,
) -> Result<Value, String> {
    if groups.is_empty() {
        return Ok(json!({ "pass": true, "groups": [] }));
    }
    let read = groups_lua(groups);
    let seen = wait(run, api, SHOW_LIMIT, what, |_| {
        let seen = run.eval("server", "template-heliport-groups", &read, 60)?;
        Ok((groups_verdict(groups, &seen)["pass"] == json!(true)).then_some(seen))
    })
    .or_else(|_| run.eval("server", "template-heliport-groups", &read, 60))?;
    Ok(groups_verdict(groups, &seen))
}

/// A group check's verdict for the record: an error is a failed check.
fn recorded(r: Result<Value, String>) -> Value {
    r.unwrap_or_else(|e| json!({ "pass": false, "error": e }))
}

/// A read of every further name after the removal: whether
/// `StaticObject.getByName` finds nothing or an object that does not exist,
/// and whether `Airbase.getByName` finds nothing.
fn further_lua(names: &[String]) -> String {
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    format!(
        "local out = {{}}\nfor i, n in ipairs({{ {} }}) do\n  local s = StaticObject.getByName(n)\n  \
         out[i] = {{ static_gone = s == nil or not s:isExist(), airbase_nil = Airbase.getByName(n) == nil }}\nend\nreturn out\n",
        list.join(", ")
    )
}

/// Whether DCS's read after the removal ([`further_lua`]) shows all of the
/// `n` names gone.
fn further_gone(seen: &Value, n: usize) -> bool {
    seen.as_array().map_or(n == 0, |a| {
        a.len() == n
            && a.iter()
                .all(|f| f["static_gone"] == json!(true) && f["airbase_nil"] == json!(true))
    })
}

/// What the World lists of the campaign's statics: ID, revision, state and
/// last error. The World service serves no airbases
/// (`crates/api/src/convert.rs`, the filter's kinds).
fn world(api: &mut Api, campaign: u64) -> Result<Value, String> {
    let mut s = api
        .rt
        .block_on(api.world.list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: campaign,
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
                statics.push(json!({ "id": g.id, "spec_revision": g.spec_revision, "state": g.state, "last_error": g.last_error }));
            }
        }
    }
    Ok(json!(statics))
}

/// How far north of the file's first heliport the `PutStatic` twin stands,
/// in meters: clear of the file's objects, on the same ground.
const TWIN_NORTH_M: f64 = 60.0;

/// The one theater the part's mission folder holds a shell for: the
/// sidecar part writes `Caucasus.miz` only (`sidecar.rs`, `with_map`).
const THEATER: &str = "Caucasus";

/// The layout of FILE, its first heliport's index and its theater; refused
/// when FILE is not a static template, holds no `Heliports` static, or has
/// a `theatre` other than [`THEATER`]. `sidecar` checks FILE with it before
/// it touches the target.
pub fn layout(file: &[u8]) -> Result<(Layout, usize, String), String> {
    let layout = static_template::read(file).map_err(|r| r.text)?;
    let pad = (0..layout.statics.len())
        .find(|i| heliport(&layout, *i))
        .ok_or("the file holds no `Heliports` static")?;
    let theater = layout.theatre.clone().ok_or("the file has no `theatre`")?;
    if theater != THEATER {
        return Err(format!(
            "the file's `theatre` is `{theater}`; the part runs on {THEATER} only"
        ));
    }
    Ok((layout, pad, theater))
}

/// How the template's heliport `pad` and its twin, the last name, went: the
/// same World state, both present; the same airbase category and type, both
/// read; the same outcome of the removal; and every static of `l` that is
/// not a heliport removed. `checked` is [`check`]'s result, `left`
/// [`remove`]'s.
fn twins(l: &Layout, ids: &[u64], pad: usize, checked: &Value, left: &Value) -> Value {
    let twin = ids.len().saturating_sub(1);
    let gone_of = |i: usize| left["gone"][i].clone();
    let same_removal = gone_of(pad).is_boolean() && gone_of(pad) == gone_of(twin);
    let others_removed = (0..l.statics.len())
        .filter(|i| !heliport(l, *i))
        .all(|i| gone_of(i) == json!(true));
    let state_of = |i: usize| {
        checked["world"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|s| ids.get(i).is_some_and(|id| s["id"] == json!(id)))
            })
            .map(|s| s["state"].clone())
    };
    let same_state = matches!((state_of(pad), state_of(twin)), (Some(a), Some(b)) if a == b);
    let kind_of = |i: usize| {
        let a = &checked["seen"]["airbases"][i];
        (a["category"].clone(), a["type"].clone())
    };
    let same_airbase = kind_of(pad).0.is_number() && kind_of(pad) == kind_of(twin);
    json!({
        "pass": twin > pad && same_removal && others_removed && same_state && same_airbase,
        "same_state": same_state,
        "same_airbase": same_airbase,
        "same_removal": same_removal,
        "others_removed": others_removed,
    })
}

/// Run the part; the result goes under `template_heliport`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    file: &[u8],
    results: &mut Value,
) -> Result<(), String> {
    let (layout, pad, theater) = layout(file)?;
    // The file's statics, then the twin: the same heliport, given to
    // `PutStatic` as the template's spec, north of it.
    let mut all = layout.clone();
    let mut twin = layout.statics[pad].clone();
    // A heliport group's further units move with it, so the twin's stand
    // clear of the file's.
    let reach = twin
        .further
        .iter()
        .map(|f| f.x - twin.x)
        .fold(0.0, f64::max);
    twin.x += TWIN_NORTH_M + reach;
    for f in &mut twin.further {
        f.x += TWIN_NORTH_M + reach;
    }
    twin.extra.retain(|(k, _)| !k.starts_with("heliport_"));
    twin.extra.extend([
        (
            "heliport_frequency".to_string(),
            Lv::String(TWIN_RADIO.0.as_bytes().to_vec()),
        ),
        ("heliport_modulation".to_string(), Lv::Number(TWIN_RADIO.1)),
        ("heliport_callsign_id".to_string(), Lv::Number(TWIN_RADIO.2)),
    ]);
    all.statics.push(twin);
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);

    println!("== template-heliport: a campaign on {theater}");
    let c = api.create("template-heliport-live", &[&theater], true)?;
    loaded(run, &mut api, &theater, generation)?;
    let places: Vec<String> = all
        .statics
        .iter()
        .map(|s| format!("{{{}, {}}}", s.x, s.y))
        .collect();
    let surface = run.eval(
        "server",
        "template-heliport-surface",
        &format!(
            "local out = {{}}\nfor i, p in ipairs({{{}}}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n",
            places.join(", ")
        ),
        60,
    )?;
    api.renew()?;

    let on_profile = format!("{}/own/{FILE}", crate::sidecar::MISSIONS_DIR);
    run.profile().write(&on_profile, file)?;
    let path = run.template_path(FILE);
    let out = steps(run, &mut api, c, &path, &layout, pad, &all);
    let removed = run.profile().remove_file(&on_profile);
    let gone = run.profile().read(&on_profile).map(|f| f.is_none());
    let mut v = out?;
    removed?;
    v["surface"] = surface;
    v["file_removed"] = json!(gone?);
    v["pass"] = json!(v["pass"] == json!(true) && v["file_removed"] == json!(true));
    println!(
        "   {} {}",
        if v["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        },
        v["dcs"]
    );
    results["template_heliport"] = v;
    Ok(())
}

fn steps(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    path: &str,
    l: &Layout,
    pad: usize,
    all: &Layout,
) -> Result<Value, String> {
    println!("== template-heliport: the agent's report with no FARP");
    let report_none = report(run, api)?;
    println!("== template-heliport: the file in place from {path}, and its heliport by PutStatic");
    let req = pb::PutFromStaticTemplateRequest {
        write: api.write("template-heliport", c),
        path: path.to_owned(),
        anchor: None,
        heading: 0.0,
        country: None,
        label: String::new(),
    };
    let made = api
        .rt
        .block_on(api.commands.put_from_static_template(req))
        .map_err(status_text("PutFromStaticTemplate"))?
        .into_inner();
    let mut ids: Vec<u64> = made.statics.iter().map(|s| s.static_id).collect();
    // The twin's spec is the template's own, moved north, with its radio:
    // the last static of `all`.
    let specs = static_template::specs(all, None, 0.0, None, "").map_err(|r| r.text)?;
    let mut spec = specs.statics.last().cloned().ok_or("no twin spec")?;
    spec.label = "twin".into();
    let req = pb::PutStaticRequest {
        write: api.write("twin", c),
        static_id: 0,
        spec: Some(spec.clone()),
    };
    let twin = api
        .rt
        .block_on(api.commands.put_static(req))
        .map_err(status_text("PutStatic"));
    if let Ok(t) = &twin {
        ids.push(t.get_ref().static_id);
    }
    let names: Vec<String> = ids
        .iter()
        .map(|id| name(NameKind::Static, CampaignId(c), EntityId(*id)))
        .collect();
    let pads: Vec<String> = names
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < all.statics.len() && heliport(all, *i))
        .map(|(_, n)| n.clone())
        .collect();
    let checked = if twin.is_ok() {
        check(run, api, c, all, THEATER, &names, &pads)
    } else {
        Err("the twin was not made".into())
    };
    // The template's and the twin's heliport groups, read as one airbase each.
    let first_groups = groups_of(c, &ids, &specs.statics);
    let groups_first = if checked.is_ok() {
        println!("== template-heliport: the heliport groups of the file and the twin");
        check_groups(
            run,
            api,
            &first_groups,
            "the groups of the file and the twin",
        )
    } else {
        Err("the twin was not checked".into())
    };
    // Further helipads by `PutStatic`, north of the twin, for the report
    // with several FARPs.
    let mut extra_ids = Vec::new();
    let mut extra_specs = Vec::new();
    let report_several = if checked.is_ok() {
        println!("== template-heliport: {EXTRA_PADS} more helipads, and the agent's report");
        let step = EXTRA_STEP_M + reach_of(&spec);
        for k in 0..EXTRA_PADS {
            let mut s = spec.clone();
            let dn = step * (k + 1) as f64;
            if let Some(p) = s.position.as_mut() {
                p.north += dn;
            }
            for u in &mut s.further_units {
                if let Some(p) = u.position.as_mut() {
                    p.north += dn;
                }
            }
            s.label = format!("extra {k}");
            let req = pb::PutStaticRequest {
                write: api.write("extra", c),
                static_id: 0,
                spec: Some(s.clone()),
            };
            match api.rt.block_on(api.commands.put_static(req)) {
                Ok(r) => {
                    extra_ids.push(r.into_inner().static_id);
                    extra_specs.push(s);
                }
                Err(e) => println!("   FAIL PutStatic: {e}"),
            }
        }
        report(run, api).ok()
    } else {
        None
    };
    let extra_groups = groups_of(c, &extra_ids, &extra_specs);
    let groups_extra = if checked.is_ok() {
        check_groups(run, api, &extra_groups, "the groups of the extra helipads")
    } else {
        Err("the twin was not checked".into())
    };
    // The large group, one `PutStatic` of [`BIG_GROUP`] FARPs.
    let big = big_group(&spec);
    let mut big_ids = Vec::new();
    let groups_big = if checked.is_ok() {
        println!("== template-heliport: a group of {BIG_GROUP} FARPs");
        let req = pb::PutStaticRequest {
            write: api.write("big", c),
            static_id: 0,
            spec: Some(big.clone()),
        };
        match api.rt.block_on(api.commands.put_static(req)) {
            Ok(r) => {
                big_ids.push(r.into_inner().static_id);
                let g = groups_of(c, &big_ids, std::slice::from_ref(&big));
                check_groups(run, api, &g, "the group of the large group")
            }
            Err(e) => Err(status_text("PutStatic")(e)),
        }
    } else {
        Err("the twin was not checked".into())
    };
    let big_groups = groups_of(c, &big_ids, std::slice::from_ref(&big));
    let mut all_ids = ids.clone();
    all_ids.extend(&extra_ids);
    all_ids.extend(&big_ids);
    let all_names: Vec<String> = all_ids
        .iter()
        .map(|id| name(NameKind::Static, CampaignId(c), EntityId(*id)))
        .collect();
    let mut all_pads = pads.clone();
    all_pads.extend(all_names[ids.len()..].iter().cloned());
    println!("== template-heliport: remove everything");
    let further: Vec<String> = [&first_groups, &extra_groups, &big_groups]
        .into_iter()
        .flatten()
        .flat_map(|g| g.further.iter().cloned())
        .collect();
    let left = remove(run, api, c, &all_ids, &all_names, &all_pads, &further);
    if let Err(e) = &left {
        println!("   FAIL not removed: {e}");
    }
    let mut v = checked?;
    twin?;
    let left = left?;
    let t = twins(l, &ids, pad, &v, &left);
    v["pass"] = json!(
        v["pass"] == json!(true)
            && made.groups.is_empty()
            && made.statics.len() == l.statics.len()
            && t["pass"] == json!(true)
    );
    v["twins"] = t;
    let heliports_removed = all_pads.iter().all(|p| {
        all_names
            .iter()
            .position(|n| n == p)
            .is_some_and(|i| left["gone"][i] == json!(true))
    });
    let tombstones = left["tombstones"].as_array().is_some_and(|t| {
        all_names
            .iter()
            .zip(t)
            .all(|(n, g)| !all_pads.contains(n) || g == &json!(true))
    });
    let extras = extra_ids.len() == EXTRA_PADS;
    let groups = json!({
        "file_and_twin": recorded(groups_first),
        "extra_pads": recorded(groups_extra),
        "big_group": recorded(groups_big),
    });
    let groups_ok = ["file_and_twin", "extra_pads", "big_group"]
        .iter()
        .all(|k| groups[k]["pass"] == json!(true));
    let further_ok = left["further"]["ok"] == json!(true);
    v["pass"] = json!(
        v["pass"] == json!(true)
            && heliports_removed
            && tombstones
            && extras
            && big_ids.len() == 1
            && groups_ok
            && further_ok
    );
    v["groups"] = groups;
    v["big_group_id"] = json!(big_ids);
    v["heliports_removed"] = json!(heliports_removed);
    v["tombstones"] = json!(tombstones);
    v["extra_pads"] = json!(extra_ids);
    v["report_no_farp"] = report_none;
    v["report_farps"] = json!(report_several);
    v["answer"] = json!({
        "groups": made.groups.len(),
        "statics": made.statics.iter().map(|s| json!({ "static_id": s.static_id, "spec_revision": s.spec_revision })).collect::<Vec<_>>(),
        "revision": made.revision,
    });
    v["dcs_names"] = json!(names);
    v["after_remove"] = left;
    Ok(v)
}

/// Wait until DCS shows every static and every heliport's airbase; then
/// stock each airbase and wait for DCS to read the stock. The sidecar sends
/// `WAREHOUSE_SET` only for an airbase whose warehouse the agent sampled
/// (spec/world.md, "Kinds"), so the stock in DCS shows the sidecar keeps
/// the airbase and its warehouse.
fn check(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    all: &Layout,
    theater: &str,
    names: &[String],
    pads: &[String],
) -> Result<Value, String> {
    let read = read_lua(names);
    let mut seen = wait(run, api, SHOW_LIMIT, "the statics and the airbases", |_| {
        let mut seen = run.eval("server", "template-heliport-read", &read, 60)?;
        seen["names"] = json!(names);
        Ok((verdict(all, &seen)["pass"] == json!(true)).then_some(seen))
    })
    .or_else(|e| {
        // The last read, for the record.
        let mut seen = run.eval("server", "template-heliport-read", &read, 60)?;
        seen["names"] = json!(names);
        seen["timed_out"] = json!(e);
        Ok::<Value, String>(seen)
    })?;
    let dcs = verdict(all, &seen);
    let mut stocked = Vec::new();
    for pad in pads {
        let req = pb::SetWarehouseStockRequest {
            write: api.write("stock", c),
            theater: theater.to_string(),
            airbase: pad.clone(),
            liquids: [(0u32, FUEL_KG)].into_iter().collect(),
            ..pb::SetWarehouseStockRequest::default()
        };
        let set = api
            .rt
            .block_on(api.commands.set_warehouse_stock(req))
            .map_err(status_text("SetWarehouseStock"));
        let req = pb::SetAirbaseOwnerRequest {
            write: api.write("owner", c),
            theater: theater.to_string(),
            airbase: pad.clone(),
            owner: OWNER as i32,
        };
        let owned = api
            .rt
            .block_on(api.commands.set_airbase_owner(req))
            .map_err(status_text("SetAirbaseOwner"));
        stocked.push(
            json!({ "airbase": pad, "set": set.as_ref().err(), "owner": owned.as_ref().err() }),
        );
    }
    let inv = format!(
        "local out = {{}}\nfor i, n in ipairs({{ {} }}) do local a = Airbase.getByName(n) out[i] = a and {{ fuel = a:getWarehouse():getLiquidAmount(0), coalition = a:getCoalition() }} or false end\nreturn out\n",
        pads.iter()
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let fuel = wait(
        run,
        api,
        SHOW_LIMIT,
        "the stock and the owner in DCS",
        |_| {
            let d = run.eval("server", "template-heliport-fuel", &inv, 60)?;
            let ok = d.as_array().is_some_and(|a| {
                a.len() == pads.len()
                    && a.iter().all(|v| {
                        v["fuel"].as_f64() == Some(FUEL_KG)
                            && v["coalition"].as_i64() == Some(OWNER_IN_DCS)
                    })
            });
            Ok(ok.then_some(d))
        },
    );
    let fuel_ok = fuel.is_ok();
    let fuel = match fuel {
        Ok(d) => d,
        Err(e) => {
            json!({ "error": e, "last": run.eval("server", "template-heliport-fuel", &inv, 60).ok() })
        }
    };
    let world = world(api, c)?;
    // Every static is `LIVE`: the sidecar sees its object.
    let live = world.as_array().is_some_and(|w| {
        w.len() == names.len() && w.iter().all(|s| s["state"].as_i64() == Some(LIVE))
    });
    // Each heliport's radio, as its further keys ask.
    let radio: Vec<Value> = (0..all.statics.len())
        .filter(|i| heliport(all, *i))
        .map(|i| {
            let want = expected_radio(&all.statics[i].extra);
            let ok = radio_matches(&want, &seen["airbases"][i]);
            json!({ "index": i, "want": want, "ok": ok })
        })
        .collect();
    let radio_ok = radio.iter().all(|r| r["ok"] == json!(true));
    seen.as_object_mut().map(|o| o.remove("names"));
    api.keep();
    Ok(json!({
        "pass": dcs["pass"] == json!(true) && fuel_ok && live && radio_ok
            && stocked.iter().all(|s| s["set"].is_null() && s["owner"].is_null()),
        "live": live,
        "radio": radio,
        "dcs": dcs,
        "seen": seen,
        "world": world,
        "stocked": stocked,
        "fuel_in_dcs": fuel,
    }))
}

/// Remove every static, hold until DCS has dropped them and their airbases
/// or the limit passes, and read what DCS and the World still hold. `gone`
/// says, per name, that neither `StaticObject.getByName` nor
/// `Airbase.getByName` finds a live object and `world.getAirbases` does not
/// list it.
fn remove(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    ids: &[u64],
    names: &[String],
    pads: &[String],
    further: &[String],
) -> Result<Value, String> {
    api.keep();
    let mut sent = Ok(());
    for id in ids {
        let req = pb::RemoveEntityRequest {
            write: api.write("remove", c),
            entity_id: *id,
        };
        if let Err(e) = api
            .rt
            .block_on(api.commands.remove_entity(req))
            .map_err(status_text("RemoveEntity"))
        {
            sent = sent.and(Err(e));
        }
    }
    let read = read_lua(names);
    let gone = |seen: &Value| -> Vec<bool> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                seen["statics"][i] == json!(false)
                    && seen["airbases"][i] == json!(false)
                    && !seen["listed"]
                        .as_array()
                        .is_some_and(|l| l.iter().any(|x| x == &json!(n)))
            })
            .collect()
    };
    // Every further unit's name: no live static, no airbase.
    let read_further = || -> Result<(bool, Value), String> {
        if further.is_empty() {
            return Ok((true, json!([])));
        }
        let seen = run.eval(
            "server",
            "template-heliport-further",
            &further_lua(further),
            60,
        )?;
        Ok((further_gone(&seen, further.len()), seen))
    };
    // DCS takes a FARP's destroy only while the simulation runs: on a
    // paused server, read once, then resume until DCS has dropped them, and
    // pause again whatever the wait gave.
    // The server is public: it resumes only with no player but the
    // server's own connection, and stops the wait if one joins.
    let alone = || -> Result<bool, String> {
        let n = run.eval("hook", "template-heliport-players", PLAYERS_LUA, 60)?;
        Ok(n.as_i64().is_some_and(|n| n <= 1))
    };
    let paused = run.eval(
        "hook",
        "template-heliport-paused",
        "return DCS.getPause()\n",
        60,
    )?;
    let mut while_paused = Value::Null;
    let mut resumed = false;
    if paused == json!(true) {
        std::thread::sleep(REPORT_WAIT);
        api.renew()?;
        while_paused = run.eval("server", "template-heliport-left", &read, 60)?;
        if !alone()? {
            return Err("a player is connected: the mission was not resumed".into());
        }
        run.eval(
            "hook",
            "template-heliport-resume",
            "DCS.setPause(false)\nreturn DCS.getPause()\n",
            60,
        )?;
        resumed = true;
    }
    let waited = wait(
        run,
        api,
        SHOW_LIMIT,
        "the statics and airbases gone",
        |_| {
            if resumed && !alone()? {
                return Err("a player joined while the mission ran".into());
            }
            let seen = run.eval("server", "template-heliport-left", &read, 60)?;
            Ok((gone(&seen).iter().all(|g| *g) && read_further()?.0).then_some(()))
        },
    );
    let repaused = if paused == json!(true) {
        Some(run.eval(
            "hook",
            "template-heliport-pause",
            "DCS.setPause(true)\nreturn DCS.getPause()\n",
            60,
        ))
    } else {
        None
    };
    if let Err(e) = &waited
        && e.contains("player")
    {
        if let Some(r) = repaused {
            r?;
        }
        return Err(e.clone());
    }
    let seen = run.eval("server", "template-heliport-left", &read, 60)?;
    let (further_ok, further_seen) = read_further()?;
    // The agent turns each heliport's airbase entry into a tombstone on its
    // next warehouse pass.
    let tomb = tombstones_lua(names);
    let stones = wait(run, api, SHOW_LIMIT, "the tombstones", |_| {
        let t = run.eval("server", "template-heliport-tombstones", &tomb, 60)?;
        let done = t.as_array().is_some_and(|a| {
            names
                .iter()
                .zip(a)
                .all(|(n, g)| !pads.contains(n) || g == &json!(true))
        });
        Ok(done.then_some(t))
    })
    .or_else(|e| {
        let mut t = run.eval("server", "template-heliport-tombstones", &tomb, 60)?;
        if let Some(a) = t.as_array_mut() {
            a.push(json!({ "timed_out": e }));
        }
        Ok::<Value, String>(t)
    })?;
    let w = world(api, c)?;
    sent?;
    if let Some(r) = repaused {
        r?;
    }
    Ok(json!({
        "gone": gone(&seen),
        "further": { "names": further, "seen": further_seen, "ok": further_ok },
        "dcs": seen,
        "paused": paused,
        "while_paused": while_paused,
        "tombstones": stones,
        "world_statics": w,
        "waited": waited.err(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: &str = "staticTemplate = { [\"theatre\"] = \"Caucasus\", [\"coalition\"] = { [\"blue\"] = { [\"country\"] = { [1] = { [\"id\"] = 2, [\"static\"] = { [\"group\"] = { \
        [1] = { [\"name\"] = \"F\", [\"units\"] = { [1] = { [\"type\"] = \"APFC fuel\", [\"category\"] = \"Fortifications\", [\"x\"] = 100, [\"y\"] = 200, [\"heading\"] = 0 } } }, \
        [2] = { [\"name\"] = \"P\", [\"units\"] = { [1] = { [\"type\"] = \"SINGLE_HELIPAD\", [\"category\"] = \"Heliports\", [\"x\"] = 100, [\"y\"] = 240, [\"heading\"] = 0 } } } \
        } } } } } } }";

    fn good() -> Value {
        json!({
            "names": ["np#1#s#1", "np#1#s#2"],
            "statics": [{ "x": 100.0, "z": 200.0, "type": "APFC fuel", "country": 2 }, false],
            "airbases": [false, { "x": 100.2, "z": 240.1, "id": 2, "category": 1, "type": "SINGLE_HELIPAD" }],
            "listed": ["np#1#s#2"],
        })
    }

    #[test]
    fn the_verdict_passes_a_standing_static_and_a_listed_airbase() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        assert!(!heliport(&l, 0) && heliport(&l, 1));
        assert_eq!(verdict(&l, &good())["pass"], json!(true));
    }

    /// `check`'s and `remove`'s results as run 2 recorded them: the
    /// fortification, the template's helipad and its twin.
    fn checked() -> (Value, Value) {
        (
            json!({
                "world": [{ "id": 1, "state": 3 }, { "id": 2, "state": 6 }, { "id": 3, "state": 6 }],
                "seen": { "airbases": [false, { "category": 1, "type": "SINGLE_HELIPAD" }, { "category": 1, "type": "SINGLE_HELIPAD" }] },
            }),
            json!({ "gone": [true, false, false] }),
        )
    }

    #[test]
    fn the_template_heliport_must_go_as_its_twin() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        let ids = [1, 2, 3];
        let (c, r) = checked();
        assert_eq!(twins(&l, &ids, 1, &c, &r)["pass"], json!(true));
        let mut cases = Vec::new();
        let (mut c2, r2) = checked();
        c2["world"] = json!([{ "id": 1, "state": 3 }, { "id": 3, "state": 6 }]);
        cases.push(("one state missing", c2, r2));
        let (mut c2, r2) = checked();
        c2["world"] = json!([]);
        cases.push(("both states missing", c2, r2));
        let (mut c2, r2) = checked();
        c2["world"][2]["state"] = json!(3);
        cases.push(("another state", c2, r2));
        let (mut c2, r2) = checked();
        c2["seen"]["airbases"][2]["category"] = json!(0);
        cases.push(("another airbase category", c2, r2));
        let (mut c2, r2) = checked();
        c2["seen"]["airbases"][2]["type"] = json!("FARP");
        cases.push(("another airbase type", c2, r2));
        let (mut c2, r2) = checked();
        c2["seen"]["airbases"] = json!([false, false, false]);
        cases.push(("no airbase on either side", c2, r2));
        let (c2, mut r2) = checked();
        r2["gone"][2] = json!(true);
        cases.push(("another removal", c2, r2));
        let (c2, mut r2) = checked();
        r2["gone"][0] = json!(false);
        cases.push(("the fortification left", c2, r2));
        for (what, c, r) in cases {
            assert_eq!(twins(&l, &ids, 1, &c, &r)["pass"], json!(false), "{what}");
        }
    }

    #[test]
    fn the_radio_check_takes_the_keys_or_dcs_defaults() {
        let read =
            |f: f64, m: f64, c: &str| json!({ "frequency": f, "modulation": m, "callsign": c });
        let none = expected_radio(&[]);
        assert!(radio_matches(&none, &read(127_500_000.0, 0.0, "London")));
        let twin = vec![
            (
                "heliport_frequency".to_string(),
                Lv::String(b"131.25".to_vec()),
            ),
            ("heliport_modulation".to_string(), Lv::Number(1.0)),
            ("heliport_callsign_id".to_string(), Lv::Number(4.0)),
        ];
        let want = expected_radio(&twin);
        assert!(radio_matches(&want, &read(131_250_000.0, 1.0, "Moscow")));
        for (what, ab) in [
            ("the default frequency", read(127_500_000.0, 1.0, "Moscow")),
            ("AM", read(131_250_000.0, 0.0, "Moscow")),
            ("London", read(131_250_000.0, 1.0, "London")),
            ("no read", json!(false)),
        ] {
            assert!(!radio_matches(&want, &ab), "{what}");
        }
        let number = vec![("heliport_frequency".to_string(), Lv::Number(129.5))];
        assert_eq!(expected_radio(&number)["frequency"], json!(129_500_000.0));
        let unknown = vec![("heliport_callsign_id".to_string(), Lv::Number(9.0))];
        assert!(!radio_matches(
            &expected_radio(&unknown),
            &read(127_500_000.0, 0.0, "London")
        ));
    }

    #[test]
    fn a_file_on_another_theater_or_without_a_heliport_is_refused_before_the_run() {
        assert!(layout(PAD.as_bytes()).is_ok());
        let other = PAD.replace("\"Caucasus\"", "\"Afghanistan\"");
        assert!(
            layout(other.as_bytes())
                .expect_err("theater")
                .contains("Caucasus only")
        );
        let none = PAD.replace("\"Heliports\"", "\"Fortifications\"");
        assert!(
            layout(none.as_bytes())
                .expect_err("heliport")
                .contains("no `Heliports`")
        );
    }

    #[test]
    fn the_verdict_passes_a_heliport_static_that_getbyname_finds() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        let mut v = good();
        v["statics"][1] = json!({ "x": 100.0, "z": 240.0, "type": "SINGLE_HELIPAD", "country": 2 });
        assert_eq!(verdict(&l, &v)["pass"], json!(true));
    }

    #[test]
    fn the_verdict_fails_what_is_missing_or_misplaced() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        let mut cases = Vec::new();
        let mut v = good();
        v["listed"] = json!([]);
        cases.push(("airbase not listed", v));
        let mut v = good();
        v["airbases"][1] = json!(false);
        cases.push(("no airbase", v));
        let mut v = good();
        v["statics"][0] = json!(false);
        cases.push(("static missing", v));
        let mut v = good();
        v["statics"][0]["z"] = json!(202.0);
        cases.push(("static 2 m off", v));
        let mut v = good();
        v["airbases"][1]["x"] = json!(102.0);
        cases.push(("airbase 2 m off", v));
        let mut v = good();
        v["statics"][0]["type"] = json!("other");
        cases.push(("wrong type", v));
        let mut v = good();
        v["statics"][0]["country"] = json!(80);
        cases.push(("wrong country", v));
        let mut v = good();
        v["airbases"][0] = json!({ "x": 100.0, "z": 200.0 });
        cases.push(("an airbase from a fortification", v));
        let mut v = good();
        v["airbases"][1]["type"] = json!("FARP");
        cases.push(("an airbase of another type", v));
        let mut v = good();
        v["statics"][1] = json!({ "x": 100.0, "z": 240.0, "type": "FARP", "country": 2 });
        cases.push(("a heliport static of another type", v));
        let mut v = good();
        v["statics"][1] = json!({ "x": 103.0, "z": 240.0, "type": "SINGLE_HELIPAD", "country": 2 });
        cases.push(("a heliport static 3 m off", v));
        for (what, v) in cases {
            assert_eq!(verdict(&l, &v)["pass"], json!(false), "{what}");
        }
    }

    fn group_spec(units: usize, kind: &str) -> pb::StaticSpec {
        let at = |north: f64| pb::Position {
            theater: "Caucasus".into(),
            north,
            east: 10.0,
            alt: 0.0,
        };
        pb::StaticSpec {
            r#type: kind.into(),
            position: Some(at(1000.0)),
            further_units: (1..units)
                .map(|k| pb::StaticUnit {
                    r#type: kind.into(),
                    position: Some(at(1000.0 + 20.0 * k as f64)),
                    ..pb::StaticUnit::default()
                })
                .collect(),
            ..pb::StaticSpec::default()
        }
    }

    #[test]
    fn a_single_heliport_is_judged_as_a_one_unit_group() {
        let mut single = group_spec(1, "FARP");
        single.extra = Some(prost_types::Struct {
            fields: [(
                "category".to_string(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue("Heliports".into())),
                },
            )]
            .into(),
        });
        let g = groups_of(5, &[4, 6], &[single, group_spec(1, "Hangar A")]);
        assert_eq!(g.len(), 1);
        assert!(g[0].further.is_empty());
        assert_eq!(g[0].types, vec!["FARP"]);
    }

    #[test]
    fn further_names_follow_the_static_id_in_order() {
        let n = |id| name(NameKind::Static, CampaignId(5), EntityId(id));
        assert_eq!(further_names(5, 7, 3), vec![n(8), n(9), n(10)]);
        assert!(further_names(5, 7, 0).is_empty());
        let specs = [group_spec(1, "FARP"), group_spec(3, "FARP")];
        let g = groups_of(5, &[4, 5], &specs);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].airbase, n(5));
        assert_eq!(g[0].further, vec![n(6), n(7)]);
        assert_eq!(g[0].types, vec!["FARP"; 3]);
    }

    #[test]
    fn the_big_group_stands_beyond_the_extra_pads() {
        let twin = group_spec(3, "SINGLE_HELIPAD");
        let big = big_group(&twin);
        assert_eq!(big.r#type, FARP);
        assert_eq!(big.further_units.len(), BIG_GROUP - 1);
        let north = |p: &Option<pb::Position>| p.as_ref().expect("placed").north;
        let first = north(&big.position);
        // Each extra pad is the twin moved by its reach (40 m) and the step;
        // the last one's farthest unit is 40 m north of its first.
        let step = EXTRA_STEP_M + 40.0;
        assert!(first > 1000.0 + step * EXTRA_PADS as f64 + 40.0);
        for (k, u) in big.further_units.iter().enumerate() {
            assert_eq!(u.r#type, FARP);
            assert_eq!(north(&u.position), first + GROUP_STEP_M * (k + 1) as f64);
        }
    }

    fn seen_group(units: usize, parking: f64) -> Value {
        json!([{
            "airbase_id": 12,
            "parking": parking,
            "listed": 1,
            "further": (1..units).map(|_| json!({ "static_id": 12, "airbase": false })).collect::<Vec<_>>(),
        }])
    }

    #[test]
    fn a_group_is_one_airbase_with_four_spots_per_farp() {
        let g = groups_of(5, &[4], &[group_spec(10, "FARP")]);
        assert_eq!(
            groups_verdict(&g, &seen_group(10, 40.0))["pass"],
            json!(true)
        );
        let mut cases = Vec::new();
        cases.push(("39 spots", seen_group(10, 39.0)));
        let mut v = seen_group(10, 40.0);
        v[0]["listed"] = json!(2);
        cases.push(("listed twice", v));
        let mut v = seen_group(10, 40.0);
        v[0]["further"][3]["static_id"] = json!(13);
        cases.push(("a name finds another object", v));
        let mut v = seen_group(10, 40.0);
        v[0]["further"][3]["static_id"] = Value::Null;
        cases.push(("a name finds nothing", v));
        let mut v = seen_group(10, 40.0);
        v[0]["further"][3]["airbase"] = json!(true);
        cases.push(("a name finds an airbase", v));
        cases.push(("a name not read", seen_group(9, 40.0)));
        cases.push(("no airbase", json!([{ "listed": 0, "further": [] }])));
        cases.push(("nothing read", json!([])));
        for (what, v) in cases {
            assert_eq!(groups_verdict(&g, &v)["pass"], json!(false), "{what}");
        }
    }

    #[test]
    fn another_type_has_its_spots_recorded_and_not_judged() {
        let g = groups_of(5, &[4], &[group_spec(2, "SINGLE_HELIPAD")]);
        let v = groups_verdict(&g, &seen_group(2, 2.0));
        assert_eq!(v["pass"], json!(true));
        assert_eq!(v["groups"][0]["parking"], json!(2.0));
        assert_eq!(v["groups"][0]["parking_wanted"], Value::Null);
    }

    #[test]
    fn every_further_name_must_be_gone_after_the_removal() {
        let gone = json!({ "static_gone": true, "airbase_nil": true });
        assert!(further_gone(&json!([gone, gone]), 2));
        assert!(further_gone(&json!([]), 0));
        assert!(!further_gone(&json!([gone]), 2));
        assert!(!further_gone(
            &json!([gone, { "static_gone": false, "airbase_nil": true }]),
            2
        ));
        assert!(!further_gone(
            &json!([gone, { "static_gone": true, "airbase_nil": false }]),
            2
        ));
    }
}
