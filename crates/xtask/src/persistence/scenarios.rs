//! The S18 live tests as scenarios over a [`Target`]: the server, or the
//! fake sidecar for a rehearsal. Each scenario returns a JSON verdict with
//! `pass` and what it saw; an `Err` is a scenario that could not run to its
//! end.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use nanoprobe_proto::v1alpha1::{self as pb, EntityState, GroupCategory};
use nanoprobe_transport::names::{self, Kind};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

use super::api::{self, Api, at, ps, route};

/// A unit DCS holds.
#[derive(Clone, Debug)]
pub struct DcsUnit {
    pub name: String,
    pub north: f64,
    pub east: f64,
    pub alt: f64,
    pub in_air: bool,
}

/// A group DCS holds, with a platform name.
#[derive(Clone, Debug)]
pub struct DcsGroup {
    pub name: String,
    pub units: Vec<DcsUnit>,
}

/// What the scenarios do to DCS and the sidecar.
pub trait Target {
    fn theater(&self) -> String;
    /// The API's address now: it may change when the sidecar restarts.
    fn endpoint(&self) -> String;
    fn token(&self) -> Option<String>;
    /// Whether this is the server, so DCS-side checks are judged.
    fn live(&self) -> bool;
    /// Run model time for `seconds`.
    fn hold(&mut self, seconds: f64) -> Result<(), String>;
    /// Wait without running model time.
    fn pause_wait(&mut self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
    /// Load the active campaign's mission again behind the sidecar, and
    /// return once DCS reports it started; it starts paused.
    fn reload(&mut self) -> Result<(), String>;
    /// Whether a reload also restarts the sidecar (the fake's does).
    fn reload_restarts_sidecar(&self) -> bool {
        false
    }
    /// Kill DCS. It starts again, the sidecar is started again from its
    /// store, and the active campaign's mission is loaded.
    fn kill_dcs(&mut self) -> Result<(), String>;
    /// Kill the sidecar with SIGKILL and start it again from its store.
    fn kill_sidecar(&mut self) -> Result<(), String>;
    /// Kill the sidecar with SIGKILL and leave it stopped.
    fn kill_sidecar_stopped(&mut self) -> Result<(), String>;
    /// Start the sidecar again from its store.
    fn start_sidecar(&mut self) -> Result<(), String>;
    /// Stop or continue the sidecar's process.
    fn freeze_sidecar(&mut self, frozen: bool) -> Result<(), String>;
    /// Stop or continue DCS's processes.
    fn freeze_dcs(&mut self, frozen: bool) -> Result<(), String>;
    /// Every group DCS holds with a platform name. `None` on the fake.
    fn dcs_groups(&mut self) -> Result<Option<Vec<DcsGroup>>, String>;
    /// A mark in DCS's log of mission starts. `None` on the fake.
    fn starts_mark(&mut self) -> Option<usize> {
        None
    }
    /// Mission starts DCS logged since `mark`.
    fn starts_since(&mut self, _mark: usize) -> usize {
        0
    }
    /// Whether DCS is paused. `None` on the fake.
    fn paused(&mut self) -> Result<Option<bool>, String>;
    /// Fails when a player is connected: before a step that loads a
    /// mission. Always passes on the fake.
    fn no_players(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// Players connected now. `None` on the fake.
    fn players(&mut self) -> Result<Option<usize>, String>;
    /// Remove one unit by name with `Unit.destroy`, which emits no death.
    fn remove_unit(&mut self, unit: &str) -> Result<(), String>;
    /// Kill one unit by name with an explosion on it.
    fn kill_unit(&mut self, unit: &str) -> Result<(), String>;
    /// String keys that look like integers in the DCS tables the restore
    /// path carries, for the units named.
    fn integer_keys(&mut self, units: &[String]) -> Result<Value, String>;
    /// DCS's own reads of the units named, by name: callsign and life.
    /// An empty object on the fake.
    fn unit_state(&mut self, units: &[String]) -> Result<Value, String>;
    /// DCS's controller option IDs and values the scenario sets.
    fn option_ids(&mut self) -> Result<Value, String>;
    /// The sidecar's commit timing lines so far, this boot and earlier.
    fn timings(&mut self) -> Result<Vec<Value>, String>;
    /// Which points are on land.
    fn on_land(&mut self, points: &[(f64, f64)]) -> Result<Vec<bool>, String>;
}

/// Where the scenarios spawn: the hello's start, which is land on Caucasus
/// (docs/results/2026-11-15-s17-api-on-real-dcs.md).
pub const ORIGIN: (f64, f64) = (-290_000.0, 680_000.0);
/// A ground unit type every server has.
const GROUND: &str = "M-113";
/// An aircraft type the server spawns, and its launchers, named as
/// `get_weapon_display_name_by_wstype` names their weapons (LIVE, E39):
/// the 610-gallon tank and AIM-120B.
const AIRCRAFT: &str = "F-15C";
const TANK610: &str = "{E1F29B21-F291-4589-9FD8-3272EEC69506}";
const AIM120B: &str = "{C8E06185-7CD6-4C90-959F-044679E90751}";
/// USA.
const COUNTRY: u32 = 2;

fn state(g: &pb::Group) -> EntityState {
    EntityState::try_from(g.state).unwrap_or(EntityState::Unspecified)
}

fn live(g: &pb::Group) -> bool {
    state(g) == EntityState::Live
}

fn ground(theater: &str, label: &str, n: usize, at0: (f64, f64), spacing: f64) -> pb::GroupSpec {
    pb::GroupSpec {
        label: label.into(),
        country: COUNTRY,
        category: GroupCategory::Ground as i32,
        units: (0..n)
            .map(|i| pb::UnitSpec {
                r#type: GROUND.into(),
                label: format!("{label}-{i}"),
                position: Some(at(theater, at0.0, at0.1 + spacing * i as f64, 0.0)),
                ..pb::UnitSpec::default()
            })
            .collect(),
        ..pb::GroupSpec::default()
    }
}

/// An F-15C in the air at `alt`, with a payload by pylon number, a
/// callsign table and a board number: tables whose integer keys travel as
/// decimal strings (spec/world.md, "Lua tables in the API").
fn aircraft(theater: &str, label: &str, at0: (f64, f64), alt: f64) -> pb::GroupSpec {
    let extra = json!({
        "speed": 180,
        "alt_type": "BARO",
        "onboard_num": "010",
        "callsign": { "1": 1, "2": 1, "3": 1, "name": "Enfield11" },
        "payload": {
            "pylons": {
                "1": { "CLSID": AIM120B },
                "2": { "CLSID": TANK610 },
                "6": { "CLSID": TANK610 },
                "10": { "CLSID": TANK610 },
                "11": { "CLSID": AIM120B },
            },
            "fuel": 6103,
            "flare": 60,
            "chaff": 120,
            "gun": 100,
        },
    });
    pb::GroupSpec {
        label: label.into(),
        country: COUNTRY,
        category: GroupCategory::Airplane as i32,
        units: vec![pb::UnitSpec {
            r#type: AIRCRAFT.into(),
            label: format!("{label}-0"),
            position: Some(at(theater, at0.0, at0.1, alt)),
            heading: 0.0,
            skill: "High".into(),
            extra: Some(ps(&extra)),
            ..pb::UnitSpec::default()
        }],
        extra: Some(ps(&json!({ "task": "CAP" }))),
        ..pb::GroupSpec::default()
    }
}

fn orbit(theater: &str, c: (f64, f64), alt: f64) -> pb::Order {
    pb::Order {
        intent: Some(pb::order::Intent::Orbit(pb::OrbitOrder {
            center: Some(at(theater, c.0, c.1, alt)),
            speed: 180.0,
            // A radius is refused: DCS's own orbit.
            radius: 0.0,
        })),
    }
}

/// Run until `f` gives a value, holding model time `step` at a time, for
/// at most `limit` model seconds (real seconds on a paused wait).
fn wait<T>(
    t: &mut dyn Target,
    api: &mut Api,
    what: &str,
    limit: f64,
    step: f64,
    running: bool,
    mut f: impl FnMut(&mut Api, &mut dyn Target) -> Result<Option<T>, String>,
) -> Result<T, String> {
    let mut spent = 0.0;
    loop {
        if let Some(v) = f(api, t)? {
            return Ok(v);
        }
        if spent >= limit {
            return Err(format!("{what}: not within {limit} s"));
        }
        if running {
            t.hold(step)?;
        } else {
            t.pause_wait(step);
        }
        spent += step;
    }
}

/// Wait until every group of `ids` is live.
fn all_live(
    t: &mut dyn Target,
    api: &mut Api,
    campaign: u64,
    ids: &[u64],
    limit: f64,
    running: bool,
) -> Result<BTreeMap<u64, pb::Group>, String> {
    wait(t, api, "every group live", limit, 5.0, running, |api, _| {
        let g = api.groups(campaign)?;
        Ok(ids
            .iter()
            .all(|id| g.get(id).is_some_and(live))
            .then_some(g))
    })
}

/// DCS's groups of `campaign` by entity: each with how many DCS groups
/// carry its name, and the last one's units.
fn dcs_of(groups: &[DcsGroup], campaign: u64) -> BTreeMap<u64, (usize, Vec<DcsUnit>)> {
    let tag = names::tag_of(campaign);
    let mut out: BTreeMap<u64, (usize, Vec<DcsUnit>)> = BTreeMap::new();
    for g in groups {
        if let Some(p) = names::parse(&g.name)
            && p.kind == Kind::Group
            && p.tag == tag
        {
            let e = out.entry(p.entity).or_default();
            e.0 += 1;
            e.1 = g.units.clone();
        }
    }
    out
}

/// One group per entity: every live group of the store is in DCS once
/// with one unit per living member, and DCS has no other group of the
/// campaign. Also every other platform group, of any campaign, counted.
fn one_per_entity(t: &mut dyn Target, api: &mut Api, campaign: u64) -> Result<Value, String> {
    let store = api.groups(campaign)?;
    let Some(groups) = t.dcs_groups()? else {
        let ok = store
            .values()
            .all(|g| g.units.iter().all(|u| !u.alive || live(g)));
        return Ok(json!({ "pass": ok, "dcs": "not judged on the fake" }));
    };
    let dcs = dcs_of(&groups, campaign);
    let mut bad = Vec::new();
    for (id, g) in &store {
        let want = g.units.iter().filter(|u| u.alive && u.left == 0).count();
        match (live(g), dcs.get(id)) {
            (true, Some((1, units))) if units.len() == want => {}
            (true, other) => bad.push(json!({
                "group": id, "want_units": want,
                "dcs_groups": other.map(|o| o.0), "dcs_units": other.map(|o| o.1.len()),
            })),
            (false, Some((n, _))) => {
                bad.push(json!({ "group": id, "state": g.state, "dcs_groups": n }))
            }
            (false, None) => {}
        }
    }
    for id in dcs.keys() {
        if !store.contains_key(id) {
            bad.push(json!({ "unknown_in_dcs": id }));
        }
    }
    let other: usize = groups.len() - dcs.values().map(|v| v.0).sum::<usize>();
    Ok(
        json!({ "pass": bad.is_empty(), "bad": bad, "dcs_groups": groups.len(), "other_platform_groups": other }),
    )
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

/// The first unit's position of a group as the store has it.
fn lead(g: &pb::Group) -> Option<(f64, f64, f64)> {
    g.units
        .iter()
        .find(|u| u.alive)
        .and_then(|u| u.position.as_ref())
        .map(|p| (p.north, p.east, p.alt))
}

/// A straight route from `start` of the longest length in `lengths` whose
/// samples are all on land.
fn land_route(
    t: &mut dyn Target,
    start: (f64, f64),
    lengths: &[f64],
) -> Result<(f64, f64), String> {
    // North first: the hello's own route goes north (S17).
    let ways = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)];
    for len in lengths {
        for (dn, de) in ways {
            let pts: Vec<(f64, f64)> = (0..=10)
                .map(|i| {
                    let f = len * f64::from(i) / 10.0;
                    (start.0 + dn * f, start.1 + de * f)
                })
                .collect();
            if t.on_land(&pts)?.iter().all(|b| *b) {
                return Ok(pts[10]);
            }
        }
    }
    Err("no straight route on land from the start".into())
}

/// Places for `n` groups on land, on a grid from `origin`.
fn land_grid(
    t: &mut dyn Target,
    origin: (f64, f64),
    n: usize,
    spacing: f64,
) -> Result<Vec<(f64, f64)>, String> {
    let side = (n as f64).sqrt().ceil() as usize * 2;
    let cand: Vec<(f64, f64)> = (0..side * side)
        .map(|i| {
            (
                origin.0 + spacing * (i / side) as f64,
                origin.1 + spacing * (i % side) as f64,
            )
        })
        .collect();
    let land = t.on_land(&cand)?;
    let out: Vec<(f64, f64)> = cand
        .into_iter()
        .zip(land)
        .filter(|(_, l)| *l)
        .map(|(p, _)| p)
        .take(n)
        .collect();
    if out.len() < n {
        return Err(format!("only {} of {n} grid places are on land", out.len()));
    }
    Ok(out)
}

/// Create and activate a campaign, and wait for its mission to load.
fn campaign(t: &mut dyn Target, api: &mut Api, name: &str) -> Result<u64, String> {
    // Activation loads the campaign's mission.
    t.no_players()?;
    let c = api.create(name, &t.theater())?;
    let theater = t.theater();
    wait(
        t,
        api,
        "the campaign's mission",
        600.0,
        5.0,
        true,
        |api, _| Ok((api.campaign(c.id)?.loaded_theater == theater).then_some(())),
    )?;
    Ok(c.id)
}

fn units_in_air(
    t: &mut dyn Target,
    campaign: u64,
    group: u64,
) -> Result<Option<Vec<bool>>, String> {
    Ok(t.dcs_groups()?.map(|g| {
        dcs_of(&g, campaign)
            .get(&group)
            .map(|(_, u)| u.iter().map(|u| u.in_air).collect())
            .unwrap_or_default()
    }))
}

/// Where DCS holds a group's first unit, `None` on the fake.
fn dcs_lead(
    t: &mut dyn Target,
    campaign: u64,
    group: u64,
) -> Result<Option<(f64, f64, f64)>, String> {
    Ok(t.dcs_groups()?.and_then(|g| {
        dcs_of(&g, campaign)
            .get(&group)
            .and_then(|(_, u)| u.first().map(|u| (u.north, u.east, u.alt)))
    }))
}

/// 1. A reload, then a DCS kill, with a convoy under way and an aircraft
///    airborne: the world returns, the convoy arrives, the aircraft flies
///    on, and no group is duplicated. Positions come from DCS: before the
///    restore while it drives, after it while the mission is still paused.
///    The order revisions are recorded; how many times DCS was given an
///    order is not visible through the API (an open item).
pub fn reload_and_kill(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    let theater = t.theater();
    let c = campaign(t, api, &format!("s18-reload-{tag}"))?;
    let end = land_route(t, ORIGIN, &[3000.0, 2500.0, 2000.0, 1500.0, 1000.0])?;
    let convoy = api
        .put_group(
            c,
            ground(&theater, "convoy", 2, ORIGIN, 20.0),
            Some(route(&theater, &[(end.0, end.1, 0.0)], 10.0)),
        )?
        .group_id;
    let air_at = (ORIGIN.0 + 8000.0, ORIGIN.1);
    let jet = api
        .put_group(
            c,
            aircraft(&theater, "jet", air_at, 3000.0),
            Some(orbit(&theater, air_at, 3000.0)),
        )?
        .group_id;
    let mut out = json!({ "campaign": c, "convoy": convoy, "jet": jet, "end": [end.0, end.1] });
    all_live(t, api, c, &[convoy, jet], 300.0, true)?;
    let length = dist(ORIGIN, end);
    let mut phases = Vec::new();
    // A reload at a fifth of the route, a DCS kill at half of it.
    for (phase, share) in [("reload", 0.2), ("dcs_kill", 0.5)] {
        let before = wait(
            t,
            api,
            "the convoy under way",
            900.0,
            5.0,
            true,
            |api, t| {
                let g = api.groups(c)?;
                let Some(l) = g.get(&convoy).and_then(lead) else {
                    return Ok(None);
                };
                let air = units_in_air(t, c, jet)?;
                let flying = air.as_ref().is_none_or(|a| a.iter().any(|b| *b));
                Ok((dist(ORIGIN, (l.0, l.1)) >= share * length && flying).then_some(g))
            },
        )?;
        // DCS's own positions, read with the store's, before the restore.
        let convoy_before = dcs_lead(t, c, convoy)?;
        let jet_before = dcs_lead(t, c, jet)?;
        let orders: BTreeMap<u64, (u64, u64)> = before
            .iter()
            .map(|(id, g)| (*id, (g.order_revision, g.applied_order_revision)))
            .collect();
        match phase {
            "reload" => reload(t, api)?,
            _ => t.kill_dcs()?,
        }
        if phase != "reload" {
            // A new connection: the sidecar started again.
            *api = reconnect(t, api)?;
        }
        // The mission starts paused: the restore spawns while nothing
        // drives, so DCS's positions are where the restore put the units.
        t.pause_wait(5.0);
        let back = all_live(t, api, c, &[convoy, jet], 600.0, false)?;
        let paused = t.paused()?;
        let once = one_per_entity(t, api, c)?;
        let convoy_after = dcs_lead(t, c, convoy)?;
        let jet_after = dcs_lead(t, c, jet)?;
        let from = |p: Option<(f64, f64, f64)>| p.map(|l| dist(ORIGIN, (l.0, l.1)));
        let (m0, m1) = (from(convoy_before), from(convoy_after));
        // Respawned where it was, not at its start: within 100 m of where
        // DCS had it, the distance it drives in 10 s.
        let kept = match (m0, m1) {
            (Some(a), Some(b)) => (b - a).abs() <= 100.0,
            _ => !t.live(),
        };
        let air = units_in_air(t, c, jet)?;
        let same_orders = back
            .iter()
            .all(|(id, g)| orders.get(id).is_some_and(|o| o.0 == g.order_revision));
        phases.push(json!({
            "phase": phase,
            "paused_at_restore": paused,
            "convoy_from_start_m_before": m0,
            "convoy_from_start_m_after": m1,
            "convoy_kept_its_place": kept,
            "jet_alt_before": jet_before.map(|l| l.2),
            "jet_alt_after": jet_after.map(|l| l.2),
            "jet_in_air": air,
            "one_per_entity": once,
            "order_revisions_unchanged": same_orders,
            "orders_before": orders.iter().map(|(k, v)| json!([k, v.0, v.1])).collect::<Vec<_>>(),
            "orders_after": back.iter().map(|(k, g)| json!([k, g.order_revision, g.applied_order_revision])).collect::<Vec<_>>(),
        }));
    }
    let arrived = wait(
        t,
        api,
        "the convoy at its end",
        900.0,
        10.0,
        true,
        |api, _| {
            let g = api.groups(c)?;
            Ok(g.get(&convoy)
                .and_then(lead)
                .filter(|l| dist((l.0, l.1), end) <= 50.0)
                .map(|l| (l.0, l.1)))
        },
    );
    let at_end = dcs_lead(t, c, convoy)?;
    let once = one_per_entity(t, api, c)?;
    let air = units_in_air(t, c, jet)?;
    let jet_now = api.groups(c)?.get(&jet).cloned();
    let pass = phases.iter().all(|p| {
        p["one_per_entity"]["pass"] == json!(true)
            && p["convoy_kept_its_place"] == json!(true)
            && p["paused_at_restore"] != json!(false)
            && p["jet_in_air"]
                .as_array()
                .is_none_or(|a| a.contains(&json!(true)))
    }) && arrived.is_ok()
        && at_end.is_none_or(|l| dist((l.0, l.1), end) <= 50.0)
        && once["pass"] == json!(true)
        && air.as_ref().is_none_or(|a| a.iter().any(|b| *b))
        && jet_now.as_ref().is_some_and(live);
    out["phases"] = json!(phases);
    out["arrived"] = json!(arrived.as_ref().map(|p| [p.0, p.1]).map_err(|e| e.clone()));
    out["dcs_convoy_at_end"] = json!(at_end.map(|l| [l.0, l.1]));
    out["one_per_entity_at_end"] = once;
    out["jet_in_air_at_end"] = json!(air);
    out["pass"] = json!(pass);
    Ok(out)
}

/// 2. A paused restore that destroys and re-adds groups: after a reload the
///    mission is paused and the restore burst spawns ten groups; two are
///    dematerialized and materialized again under the same names while
///    paused, with a sidecar restart between; after the resume DCS holds
///    exactly one group per entity.
pub fn paused_burst(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    let theater = t.theater();
    let c = campaign(t, api, &format!("s18-burst-{tag}"))?;
    let places = land_grid(t, (ORIGIN.0 + 2000.0, ORIGIN.1), 10, 200.0)?;
    let mut ids = Vec::new();
    for (i, p) in places.iter().enumerate() {
        ids.push(
            api.put_group(c, ground(&theater, &format!("b{i}"), 2, *p, 15.0), None)?
                .group_id,
        );
    }
    all_live(t, api, c, &ids, 300.0, true)?;
    reload(t, api)?;
    let paused_at_load = t.paused()?;
    t.pause_wait(5.0);
    all_live(t, api, c, &ids, 300.0, false)?;
    let after_burst = one_per_entity(t, api, c)?;
    let two = [ids[0], ids[1]];
    api.materialize(c, &two, false)?;
    wait(t, api, "two groups virtual", 120.0, 1.0, false, |api, _| {
        let g = api.groups(c)?;
        Ok(two
            .iter()
            .all(|id| g.get(id).is_some_and(|g| state(g) == EntityState::Virtual))
            .then_some(()))
    })?;
    let while_virtual = t.dcs_groups()?.map(|g| {
        let d = dcs_of(&g, c);
        two.iter()
            .map(|id| d.get(id).map_or(0, |v| v.0))
            .collect::<Vec<_>>()
    });
    api.materialize(c, &two, true)?;
    t.kill_sidecar()?;
    *api = reconnect(t, api)?;
    all_live(t, api, c, &ids, 300.0, false)?;
    let paused_before_resume = t.paused()?;
    let during_pause = one_per_entity(t, api, c)?;
    t.hold(10.0)?;
    let after_resume = one_per_entity(t, api, c)?;
    Ok(json!({
        "campaign": c,
        "groups": ids,
        "paused_at_load": paused_at_load,
        "paused_before_resume": paused_before_resume,
        "after_burst": after_burst,
        "dcs_groups_of_the_two_while_virtual": while_virtual,
        "during_pause": during_pause,
        "after_resume": after_resume,
        "pass": after_resume["pass"] == json!(true)
            && after_burst["pass"] == json!(true)
            && during_pause["pass"] == json!(true)
            && (paused_at_load == Some(true) || !t.live())
            && (paused_before_resume == Some(true) || !t.live()),
    }))
}

/// 3. Suspend A, activate B, then A: each comes back where DCS held it when
///    it was left, and nothing of one is in DCS while the other is active.
pub fn switch(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    let theater = t.theater();
    let places = land_grid(t, (ORIGIN.0 + 4000.0, ORIGIN.1), 5, 300.0)?;
    let a = campaign(t, api, &format!("s18-a-{tag}"))?;
    let mut a_ids = Vec::new();
    for (i, p) in places[..3].iter().enumerate() {
        a_ids.push(
            api.put_group(a, ground(&theater, &format!("a{i}"), 2, *p, 15.0), None)?
                .group_id,
        );
    }
    api.sample_rates(a, 2.0, 10.0)?;
    all_live(t, api, a, &a_ids, 300.0, true)?;
    t.hold(10.0)?;
    // Where DCS holds A's groups when A is suspended; the store's on the
    // fake.
    let where_a =
        |t: &mut dyn Target, api: &mut Api| -> Result<BTreeMap<u64, (f64, f64)>, String> {
            Ok(match t.dcs_groups()? {
                Some(g) => dcs_of(&g, a)
                    .into_iter()
                    .filter_map(|(id, (_, u))| u.first().map(|u| (id, (u.north, u.east))))
                    .collect(),
                None => api
                    .groups(a)?
                    .into_iter()
                    .filter_map(|(id, g)| lead(&g).map(|l| (id, (l.0, l.1))))
                    .collect(),
            })
        };
    let a_left = where_a(t, api)?;
    let suspended = api.suspend(a)?;
    let b = campaign(t, api, &format!("s18-b-{tag}"))?;
    let mut b_ids = Vec::new();
    for (i, p) in places[3..].iter().enumerate() {
        b_ids.push(
            api.put_group(b, ground(&theater, &format!("b{i}"), 2, *p, 15.0), None)?
                .group_id,
        );
    }
    all_live(t, api, b, &b_ids, 300.0, true)?;
    let b_once = one_per_entity(t, api, b)?;
    let a_in_dcs_under_b = t.dcs_groups()?.map(|g| dcs_of(&g, a).len());
    let theater_c = theater.clone();
    t.no_players()?;
    api.activate(a)?;
    wait(t, api, "A's mission", 600.0, 5.0, true, |api, _| {
        Ok((api.campaign(a)?.loaded_theater == theater_c).then_some(()))
    })?;
    all_live(t, api, a, &a_ids, 300.0, true)?;
    let a_once = one_per_entity(t, api, a)?;
    let b_in_dcs_under_a = t.dcs_groups()?.map(|g| dcs_of(&g, b).len());
    let back = where_a(t, api)?;
    let moved: Vec<Value> = back
        .iter()
        .filter_map(|(id, l)| {
            let was = a_left.get(id)?;
            Some(json!({ "group": id, "moved_m": dist(*l, *was) }))
        })
        .collect();
    let in_place = moved.len() == a_ids.len()
        && moved
            .iter()
            .all(|m| m["moved_m"].as_f64().is_some_and(|d| d <= 5.0));
    let b_state = api.campaign(b)?.state;
    Ok(json!({
        "a": a, "b": b,
        "a_suspended_state": suspended.state,
        "b_state_after": b_state,
        "b_once": b_once,
        "a_groups_in_dcs_while_b": a_in_dcs_under_b,
        "b_groups_in_dcs_while_a": b_in_dcs_under_a,
        "a_once": a_once,
        "a_moved": moved,
        "pass": in_place
            && a_once["pass"] == json!(true)
            && b_once["pass"] == json!(true)
            && a_in_dcs_under_b.is_none_or(|n| n == 0)
            && b_in_dcs_under_a.is_none_or(|n| n == 0)
            && b_state == pb::CampaignState::Suspended as i32
            && suspended.state == pb::CampaignState::Suspended as i32,
    }))
}

/// 4. A reload with a death pending: a unit is killed and the mission
///    loaded again, once with the sidecar paused from before the kill until
///    after the reload started, so it reads the death late, and once with
///    it running; the dead member is not spawned again and is recorded
///    dead. Then the respawn gate: a member removed by script, which DCS
///    reports with no death, is held by its mark across the reload and not
///    spawned again.
pub fn death_pending(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    if !t.live() {
        return Ok(json!({ "pass": null, "skipped": "a kill needs DCS" }));
    }
    let theater = t.theater();
    let c = campaign(t, api, &format!("s18-death-{tag}"))?;
    let places = land_grid(t, (ORIGIN.0 + 6000.0, ORIGIN.1), 3, 1000.0)?;
    let mut cases = Vec::new();
    for (i, case) in ["late", "prompt", "removed"].into_iter().enumerate() {
        let put = api.put_group(
            c,
            ground(&theater, &format!("d{i}"), 3, places[i], 150.0),
            None,
        )?;
        let g = put.group_id;
        let victim = put.unit_ids[1];
        all_live(t, api, c, &[g], 300.0, true)?;
        let dcs_name = names::name(Kind::Unit, names::tag_of(c), victim);
        let frozen = case == "late";
        if frozen {
            t.freeze_sidecar(true)?;
        }
        let mut gone_before = None;
        let r = (|| {
            if case == "removed" {
                t.remove_unit(&dcs_name)?;
            } else {
                t.kill_unit(&dcs_name)?;
            }
            t.hold(3.0)?;
            gone_before = t.dcs_groups()?.map(|gs| {
                !gs.iter()
                    .flat_map(|g| g.units.iter())
                    .any(|u| u.name == dcs_name)
            });
            reload(t, api)
        })();
        if frozen {
            t.freeze_sidecar(false)?;
        }
        r?;
        let back = all_live(t, api, c, &[g], 300.0, true)?;
        t.hold(10.0)?;
        let once = one_per_entity(t, api, c)?;
        let unit = api.groups(c)?[&g]
            .units
            .iter()
            .find(|u| u.id == victim)
            .cloned()
            .or_else(|| back[&g].units.iter().find(|u| u.id == victim).cloned());
        let in_dcs = t.dcs_groups()?.map(|gs| {
            gs.iter()
                .flat_map(|g| g.units.iter())
                .any(|u| u.name == dcs_name)
        });
        let recorded = match case {
            // A death fact: dead, no mark.
            "late" | "prompt" => unit.as_ref().is_some_and(|u| !u.alive && u.left == 0),
            // No death fact: a mark holds it.
            _ => unit
                .as_ref()
                .is_some_and(|u| u.alive && u.left == pb::UnitLeft::Removed as i32),
        };
        cases.push(json!({
            "case": case,
            "group": g,
            "victim": victim,
            "victim_gone_from_dcs_before_reload": gone_before,
            "victim_alive_in_store": unit.as_ref().map(|u| u.alive),
            "victim_left": unit.as_ref().map(|u| u.left),
            "victim_in_dcs": in_dcs,
            "recorded_as_expected": recorded,
            "one_per_entity": once,
            "pass": in_dcs == Some(false) && gone_before == Some(true) && recorded && once["pass"] == json!(true),
        }));
    }
    let pass = cases.iter().all(|c| c["pass"] == json!(true));
    Ok(json!({ "campaign": c, "cases": cases, "pass": pass }))
}

/// 5. The write cost at the server's unit count: 2,000 units in 200 groups,
///    one in five moving, with model time running for `measure` seconds.
pub fn write_cost(
    t: &mut dyn Target,
    api: &mut Api,
    tag: &str,
    groups: usize,
    measure: f64,
) -> Result<Value, String> {
    let theater = t.theater();
    let c = campaign(t, api, &format!("s18-cost-{tag}"))?;
    let places = land_grid(t, (ORIGIN.0 - 6000.0, ORIGIN.1 - 6000.0), groups, 250.0)?;
    let mut ids = Vec::new();
    for (i, p) in places.iter().enumerate() {
        let order = (i % 5 == 0).then(|| route(&theater, &[(p.0 + 1500.0, p.1, 0.0)], 5.0));
        ids.push(
            api.put_group(c, ground(&theater, &format!("c{i}"), 10, *p, 15.0), order)?
                .group_id,
        );
    }
    let spawned = Instant::now();
    all_live(t, api, c, &ids, 900.0, true)?;
    let spawn_s = spawned.elapsed().as_secs_f64();
    let before = t.timings()?.len();
    t.hold(measure)?;
    // One more period for the last line.
    t.hold(12.0)?;
    let lines: Vec<Value> = t.timings()?.into_iter().skip(before).collect();
    let at_count: Vec<&Value> = lines
        .iter()
        .filter(|l| {
            l["units"]
                .as_u64()
                .is_some_and(|u| u as usize >= groups * 10 * 9 / 10)
        })
        .collect();
    let max = |k: &str| at_count.iter().filter_map(|l| l[k].as_u64()).max();
    let checkpoints: Vec<u64> = at_count
        .iter()
        .flat_map(|l| l["checkpoints"].as_array().cloned().unwrap_or_default())
        .filter_map(|v| v.as_u64())
        .collect();
    let once = one_per_entity(t, api, c)?;
    // The bounds are this step's (docs/results): a commit's p99 within a
    // frame at 60 fps, every checkpoint within a tenth of `checkpoint_s`.
    let p99 = max("p99_us");
    let ckpt = checkpoints.iter().copied().max();
    let pass = !at_count.is_empty()
        && p99.is_some_and(|v| v <= 16_667)
        && ckpt.is_some_and(|v| v <= 500_000)
        && once["pass"] == json!(true);
    Ok(json!({
        "campaign": c,
        "groups": ids.len(),
        "units": ids.len() * 10,
        "spawn_s_real": spawn_s,
        "lines": lines,
        "worst_p99_us": p99,
        "worst_median_us": max("median_us"),
        "worst_max_us": max("max_us"),
        "checkpoints_us": checkpoints,
        "one_per_entity": once,
        "pass": pass,
    }))
}

/// 6. Capture and restore the tables the restore path carries: an
///    aircraft's payload by pylon, its callsign table and board number (extra
///    keys whose integer keys travel as decimal strings), and controller
///    options on it and on a ground group; through a capture, a
///    dematerialize and materialize, and a reload. DCS's own reads of each
///    unit (its callsign and life) must match after each. Then
///    no string key in the DCS tables looks like an integer.
pub fn tables(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    let theater = t.theater();
    let c = campaign(t, api, &format!("s18-tables-{tag}"))?;
    let places = land_grid(t, (ORIGIN.0 + 9000.0, ORIGIN.1), 1, 400.0)?;
    let ids_opt = t.option_ids()?;
    let air_at = (ORIGIN.0 + 12_000.0, ORIGIN.1 + 3000.0);
    let jet = api
        .put_group(
            c,
            aircraft(&theater, "tjet", air_at, 4000.0),
            Some(orbit(&theater, air_at, 4000.0)),
        )?
        .group_id;
    let tank = api
        .put_group(c, ground(&theater, "ttank", 1, places[0], 0.0), None)?
        .group_id;
    let opt = |k: &str| ids_opt[k].as_u64().map(|v| v as u32);
    if let (Some(gi), Some(gv), Some(ai), Some(av)) = (
        opt("ground_roe"),
        ids_opt["ground_hold"].as_f64(),
        opt("air_roe"),
        ids_opt["air_hold"].as_f64(),
    ) {
        api.set_options(c, tank, [(gi, api::pv(&json!(gv)))].into())?;
        api.set_options(c, jet, [(ai, api::pv(&json!(av)))].into())?;
    }
    all_live(t, api, c, &[jet, tank], 300.0, true)?;
    t.hold(5.0)?;
    let names_: Vec<String> = api
        .groups(c)?
        .values()
        .flat_map(|g| {
            g.units
                .iter()
                .map(|u| names::name(Kind::Unit, names::tag_of(c), u.id))
                .collect::<Vec<_>>()
        })
        .collect();
    let s0 = t.unit_state(&names_)?;
    let captured = api.capture(c, jet)?;
    let outcome = captured.result.as_ref().map(|r| r.outcome);
    api.materialize(c, &[jet, tank], false)?;
    wait(t, api, "virtual", 120.0, 2.0, true, |api, _| {
        let g = api.groups(c)?;
        Ok([jet, tank]
            .iter()
            .all(|id| g.get(id).is_some_and(|g| state(g) == EntityState::Virtual))
            .then_some(()))
    })?;
    api.materialize(c, &[jet, tank], true)?;
    all_live(t, api, c, &[jet, tank], 300.0, true)?;
    t.hold(5.0)?;
    let s1 = t.unit_state(&names_)?;
    reload(t, api)?;
    t.hold(5.0)?;
    all_live(t, api, c, &[jet, tank], 300.0, true)?;
    t.hold(5.0)?;
    let s2 = t.unit_state(&names_)?;
    let keys = t.integer_keys(&names_)?;
    let no_bad_keys = match &keys["string_integer_keys"] {
        Value::Array(a) => a.is_empty(),
        // An empty Lua table comes back as an empty object.
        Value::Object(o) => o.is_empty(),
        _ => false,
    };
    // Every unit's description was read, and every read saw keys.
    let reads = keys["read"].as_array().cloned().unwrap_or_default();
    let all_read = !t.live()
        || (names_.iter().all(|n| {
            reads
                .iter()
                .any(|r| r["what"] == json!(format!("{n}:getDesc")))
        }) && reads
            .iter()
            .all(|r| r["keys"].as_u64().is_some_and(|k| k > 0)));
    let air = units_in_air(t, c, jet)?;
    let pass = s0 == s1
        && s1 == s2
        && no_bad_keys
        && all_read
        && outcome == Some(pb::Outcome::Applied as i32)
        && air.as_ref().is_none_or(|a| a.iter().any(|b| *b));
    Ok(json!({
        "campaign": c,
        "option_ids": ids_opt,
        "capture_outcome": outcome,
        "spawned": s0, "after_rematerialize": s1, "after_reload": s2,
        "jet_in_air": air,
        "keys": keys,
        "pass": pass,
    }))
}

/// 8. Spawning with a unit's life (spec/world.md, "Spawn state"): an
///    F-15C put at 60 % and an M-113 at 40 %. DCS must hold those lives, and
///    hold them again after a capture, a dematerialize and materialize, and
///    a mission reload.
pub fn spawn_state(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    let theater = t.theater();
    let c = campaign(t, api, &format!("s19-life-{tag}"))?;
    let places = land_grid(t, (ORIGIN.0 + 9000.0, ORIGIN.1 + 1500.0), 1, 400.0)?;
    let air_at = (ORIGIN.0 + 12_000.0, ORIGIN.1 + 6000.0);
    let mut jet_spec = aircraft(&theater, "ljet", air_at, 4000.0);
    jet_spec.units[0].life_percent = Some(60.0);
    let jet = api
        .put_group(c, jet_spec, Some(orbit(&theater, air_at, 4000.0)))?
        .group_id;
    let mut tank_spec = ground(&theater, "ltank", 1, places[0], 0.0);
    tank_spec.units[0].life_percent = Some(40.0);
    let tank = api.put_group(c, tank_spec, None)?.group_id;
    all_live(t, api, c, &[jet, tank], 300.0, true)?;
    t.hold(5.0)?;
    let groups = api.groups(c)?;
    let jet_unit = names::name(Kind::Unit, names::tag_of(c), groups[&jet].units[0].id);
    let tank_unit = names::name(Kind::Unit, names::tag_of(c), groups[&tank].units[0].id);
    let names_ = vec![jet_unit.clone(), tank_unit.clone()];
    let s0 = t.unit_state(&names_)?;
    let captured = api.capture(c, jet)?;
    let outcome = captured.result.as_ref().map(|r| r.outcome);
    api.materialize(c, &[jet, tank], false)?;
    wait(t, api, "virtual", 120.0, 2.0, true, |api, _| {
        let g = api.groups(c)?;
        Ok([jet, tank]
            .iter()
            .all(|id| g.get(id).is_some_and(|g| state(g) == EntityState::Virtual))
            .then_some(()))
    })?;
    api.materialize(c, &[jet, tank], true)?;
    all_live(t, api, c, &[jet, tank], 300.0, true)?;
    t.hold(5.0)?;
    let s1 = t.unit_state(&names_)?;
    reload(t, api)?;
    t.hold(5.0)?;
    all_live(t, api, c, &[jet, tank], 300.0, true)?;
    t.hold(5.0)?;
    let s2 = t.unit_state(&names_)?;
    let pct = |s: &Value, n: &str| -> Option<f64> {
        let (l, l0) = (s[n]["life"].as_f64()?, s[n]["life0"].as_f64()?);
        (l0 > 0.0).then(|| 100.0 * l / l0)
    };
    let near = |v: Option<f64>, x: f64| v.is_some_and(|v| (v - x).abs() <= 0.5);
    let held = |s: &Value| near(pct(s, &jet_unit), 60.0) && near(pct(s, &tank_unit), 40.0);
    let pass = (!t.live() || (held(&s0) && held(&s1) && held(&s2)))
        && outcome == Some(pb::Outcome::Applied as i32);
    Ok(json!({
        "campaign": c,
        "capture_outcome": outcome,
        "spawned": s0, "after_rematerialize": s1, "after_reload": s2,
        "pass": pass,
    }))
}

/// 7. A sidecar crash between an activation and its mission load: DCS is
///    stopped, the activation answers, the sidecar is killed, DCS goes on,
///    and the sidecar starts again. The campaign's mission is loaded once,
///    and nothing of the campaign spawns before it.
pub fn owed_load(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    let theater = t.theater();
    // The previous campaign runs, with a group, so a spawn of the new one
    // before its load would show in the old mission.
    let old = campaign(t, api, &format!("s18-owed-old-{tag}"))?;
    let g_old = api
        .put_group(
            old,
            ground(&theater, "o", 1, (ORIGIN.0 + 3000.0, ORIGIN.1), 0.0),
            None,
        )?
        .group_id;
    all_live(t, api, old, &[g_old], 300.0, true)?;
    let fresh = api.create_inactive(&format!("s18-owed-{tag}"), &theater)?;
    let g_new = api
        .put_group(
            fresh,
            ground(&theater, "n", 1, (ORIGIN.0 + 3100.0, ORIGIN.1), 0.0),
            None,
        )?
        .group_id;
    let mark = t.starts_mark();
    t.freeze_dcs(true)?;
    let activated = (|| -> Result<pb::Campaign, String> {
        let r = api.activate(fresh);
        t.kill_sidecar_stopped()?;
        r
    })();
    let thaw = t.freeze_dcs(false);
    let activated = activated?;
    thaw?;
    // Until DCS starts the new mission, nothing of the campaign may be in
    // the old one: sample DCS, then the starts, so a sample is known to be
    // of the old mission.
    let mut early = Vec::new();
    let mut samples = 0;
    let mut sample = |t: &mut dyn Target| -> Result<bool, String> {
        let Some(m) = mark else { return Ok(true) };
        let n = t.dcs_groups()?.map_or(0, |g| dcs_of(&g, fresh).len());
        let started = t.starts_since(m) > 0;
        if !started {
            samples += 1;
            if n > 0 {
                early.push(n);
            }
        }
        Ok(started)
    };
    sample(t)?;
    t.start_sidecar()?;
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut saw_start = false;
    while Instant::now() < deadline {
        if sample(t)? {
            saw_start = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    *api = reconnect(t, api)?;
    wait(
        t,
        api,
        "the campaign's mission",
        600.0,
        5.0,
        true,
        |api, _| Ok((api.campaign(fresh)?.loaded_theater == theater).then_some(())),
    )?;
    all_live(t, api, fresh, &[g_new], 300.0, true)?;
    // Some model time for a second load to show, if one were sent.
    t.hold(30.0)?;
    let ev = api.events(fresh, 0, 1000)?;
    let loads = ev
        .iter()
        .filter(|e| matches!(&e.body, Some(pb::event::Body::Lifecycle(l)) if l.state == pb::SimState::Loading as i32))
        .count();
    let starts = mark.map(|m| t.starts_since(m));
    let once = one_per_entity(t, api, fresh)?;
    let old_in_dcs = t.dcs_groups()?.map(|g| dcs_of(&g, old).len());
    Ok(json!({
        "old": old, "campaign": fresh, "activated_state": activated.state,
        "loading_events": loads,
        "dcs_mission_starts": starts,
        "samples_before_the_load": samples,
        "saw_the_start": saw_start,
        "campaign_groups_in_the_old_mission": early,
        "one_per_entity": once,
        "old_groups_in_dcs": old_in_dcs,
        "pass": starts.is_none_or(|n| n == 1)
            && (loads == 1 || !t.live())
            && early.is_empty()
            && ((samples > 0 && saw_start) || !t.live())
            && activated.state == pb::CampaignState::Active as i32
            && once["pass"] == json!(true)
            && old_in_dcs.is_none_or(|n| n == 0),
    }))
}

/// Reload, and connect again when that restarted the sidecar.
fn reload(t: &mut dyn Target, api: &mut Api) -> Result<(), String> {
    t.reload()?;
    if t.reload_restarts_sidecar() {
        *api = reconnect(t, api)?;
    }
    Ok(())
}

/// A new connection after the sidecar started again, with the run's
/// client details.
fn reconnect<'r>(t: &mut dyn Target, api: &Api<'r>) -> Result<Api<'r>, String> {
    api.reconnect(&t.endpoint(), t.token().as_deref())
}

/// The scenarios by name, in the order `all` runs them.
pub const ALL: [&str; 8] = [
    "reload", "burst", "switch", "death", "tables", "owed", "cost", "state",
];

/// Run the scenarios named, each with its own connection, recording each
/// verdict in `results` as it ends. Stops at the first that errs.
pub fn run(
    rt: &Runtime,
    t: &mut dyn Target,
    which: &[&str],
    tag: &str,
    results: &mut Value,
) -> Result<(), String> {
    for name in which {
        println!("== {name}");
        let players_before = t.players()?;
        if let Some(n @ 1..) = players_before {
            return Err(format!("{name}: {n} player(s) connected; not starting it"));
        }
        let mut api = Api::connect(
            rt,
            &t.endpoint(),
            t.token().as_deref(),
            &format!("{tag}-{name}"),
            Duration::from_secs(90),
        )?;
        let r = match *name {
            "reload" => reload_and_kill(t, &mut api, tag),
            "burst" => paused_burst(t, &mut api, tag),
            "switch" => switch(t, &mut api, tag),
            "death" => death_pending(t, &mut api, tag),
            "tables" => tables(t, &mut api, tag),
            "owed" => owed_load(t, &mut api, tag),
            "cost" => write_cost(t, &mut api, tag, 200, 120.0),
            "state" => spawn_state(t, &mut api, tag),
            other => Err(format!("no scenario {other}")),
        };
        api.release();
        match r {
            Ok(v) => {
                println!(
                    "   {}",
                    match &v["pass"] {
                        Value::Bool(true) => "PASS",
                        Value::Null => "SKIP",
                        _ => "FAIL",
                    }
                );
                results[*name] = v;
                results[*name]["players"] = json!([players_before, t.players().ok().flatten()]);
            }
            Err(e) => {
                println!("   ERROR {e}");
                results[*name] = json!({ "pass": false, "error": e });
                return Err(format!("{name}: {e}"));
            }
        }
    }
    Ok(())
}
