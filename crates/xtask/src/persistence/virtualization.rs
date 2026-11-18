//! PLAN S20's exit test through the API: a test controller keeps `KEEP` of
//! a 10,000-group world materialized around a moving point (500 on the
//! server, 2,000 on the fake sidecar; PLAN revision 37).
//!
//! The world is 10,000 one-unit ground groups on a land grid, put
//! virtual. One unit in five spawns at 60 % life; every unit has its own
//! heading; one group in ten has a route. At each point of a path the
//! controller asks `QueryArea` what is near, takes the `KEEP` groups
//! nearest, and materializes and dematerializes the difference. It then
//! waits until the sidecar holds exactly those live and the rest virtual.
//!
//! Checks, each at every point:
//!
//! - no group is `lost`, `unspawnable` or `dead`, and a group the
//!   controller did not move keeps its state: the sidecar changes no
//!   authority by itself;
//! - DCS holds each live group once and nothing else of the campaign
//!   (server only);
//! - 40 stationary groups leaving DCS at each point are hit through
//!   `DoEffect` (to 35 %, or 80 % if already at 35 %) and read from DCS
//!   just before the dematerialize; the state the sidecar then holds
//!   matches the read: position, heading, life (server only; the fake has
//!   no DCS truth);
//! - 20 route groups leaving DCS, which turned as they drove, are read the
//!   same way; the captured heading matches the read within 0.2 rad and the
//!   position within the distance they could drive meanwhile, and at least
//!   one had turned away from its spawn heading (server only);
//! - 40 stationary groups entering DCS have, in DCS, the position, heading
//!   and life the sidecar held (server only);
//! - a sampled group with no DCS read or no state in the sidecar fails;
//! - the campaign's unit births, one per spawn since every group has one
//!   unit, number the groups that entered DCS and come at most
//!   `MATERIALIZE_PER_FRAME` in one frame, by their stamps' frame numbers
//!   (server only; the fake emits no births). The run needs the target's
//!   sidecar at that default (spec/config.md);
//! - no command went over the commanded-work budget (server only);
//! - at the end every group and unit keeps its ID.
//!
//! Measured: how long putting the world took, the sidecar's commit and
//! checkpoint times, the agent's `sampling` reports over the run (mean
//! share and frames over 250 µs), and the commands over the commanded
//! budget. Tolerances are this test's own (docs/results, S20).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use nanoprobe_proto::v1alpha1::{self as pb, EntityState, GroupCategory};
use nanoprobe_transport::names::{self, Kind};
use serde_json::{Value, json};

use super::api::{Api, at, route};
use super::scenarios::{COUNTRY, GROUND, ORIGIN, Target, campaign, one_per_entity, state, wait};

const WORLD: usize = 10_000;
/// Meters between grid places.
pub(super) const SPACING: f64 = 300.0;
/// Points checked against land in one eval.
const LAND_CHUNK: usize = 2_000;
/// Groups each materialize or dematerialize call names.
const CALL_CHUNK: usize = 500;
/// Groups per direction whose state is read from DCS at each point.
const SAMPLE: usize = 40;
/// The path of the point, as fractions of the grid's extent.
const PATH: [(f64, f64); 6] = [
    (0.3, 0.3),
    (0.4, 0.35),
    (0.55, 0.5),
    (0.7, 0.7),
    (0.5, 0.45),
    (0.3, 0.3),
];
/// A position read from DCS and one the sidecar holds of a stationary
/// unit agree within this many meters, north and east (DCS sets a
/// ground unit on the terrain, which may move it a little).
const POSITION_M: f64 = 5.0;
/// Radians.
const HEADING_RAD: f64 = 0.01;
/// Percentage points of life.
const LIFE_PCT: f64 = 0.5;
/// A route group's captured heading and the one read from DCS just before
/// the dematerialize agree within this many radians: it may turn at a
/// waypoint in between.
const MOVING_HEADING_RAD: f64 = 0.2;
/// The sidecar's `materialize_per_frame`: the default, which the harness's
/// sidecar runs with.
const MATERIALIZE_PER_FRAME: u64 = 1;
/// The route speed, meters per second.
const ROUTE_SPEED: f64 = 5.0;

fn keep(t: &dyn Target) -> usize {
    if t.live() { 500 } else { 2_000 }
}

fn heading_of(i: usize) -> f64 {
    (i % 628) as f64 / 100.0
}

fn life_of(i: usize) -> f64 {
    if i.is_multiple_of(5) { 60.0 } else { 100.0 }
}

/// Angles a and b apart, folded into [0, pi].
fn angle_gap(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(std::f64::consts::TAU);
    d.min(std::f64::consts::TAU - d)
}

/// `n` places on land on a square grid from `origin`, north and east.
pub(super) fn places(
    t: &mut dyn Target,
    origin: (f64, f64),
    n: usize,
) -> Result<Vec<(f64, f64)>, String> {
    let side = ((n as f64).sqrt().ceil() as usize) * 3 / 2;
    let cand: Vec<(f64, f64)> = (0..side * side)
        .map(|i| {
            (
                origin.0 + SPACING * (i / side) as f64,
                origin.1 + SPACING * (i % side) as f64,
            )
        })
        .collect();
    let mut out = Vec::with_capacity(n);
    for chunk in cand.chunks(LAND_CHUNK) {
        let land = t.on_land(chunk)?;
        out.extend(chunk.iter().zip(land).filter(|(_, l)| *l).map(|(p, _)| *p));
        if out.len() >= n {
            out.truncate(n);
            return Ok(out);
        }
    }
    Err(format!("only {} of {n} grid places are on land", out.len()))
}

fn spec(theater: &str, i: usize, p: (f64, f64)) -> pb::GroupSpec {
    pb::GroupSpec {
        label: format!("v{i}"),
        country: COUNTRY,
        category: GroupCategory::Ground as i32,
        units: vec![pb::UnitSpec {
            r#type: GROUND.into(),
            label: format!("v{i}-0"),
            position: Some(at(theater, p.0, p.1, 0.0)),
            heading: heading_of(i),
            life_percent: (life_of(i) < 100.0).then(|| life_of(i)),
            ..pb::UnitSpec::default()
        }],
        r#virtual: true,
        ..pb::GroupSpec::default()
    }
}

/// The `keep` groups nearest the point, by `QueryArea`, widening the
/// radius until it finds enough. Each with its lead unit's distance.
fn nearest(
    api: &mut Api,
    c: u64,
    theater: &str,
    point: (f64, f64),
    keep: usize,
) -> Result<(BTreeSet<u64>, f64, usize), String> {
    let mut radius = SPACING * (keep as f64).sqrt();
    let mut queries = 0;
    loop {
        queries += 1;
        let found = api.query_area(c, at(theater, point.0, point.1, 0.0), radius)?;
        if found.len() >= keep {
            let mut by: Vec<(f64, u64)> = found
                .iter()
                .filter_map(|g| {
                    let p = g.units.first()?.position.as_ref()?;
                    Some(((p.north - point.0).hypot(p.east - point.1), g.id))
                })
                .collect();
            by.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            return Ok((
                by.into_iter().take(keep).map(|(_, id)| id).collect(),
                radius,
                queries,
            ));
        }
        radius *= 1.5;
    }
}

/// DCS's state of the lead unit of each group, by group: north, east,
/// heading and life as a percentage of its initial life.
fn dcs_state(
    t: &mut dyn Target,
    c: u64,
    groups: &BTreeMap<u64, pb::Group>,
    ids: &[u64],
) -> Result<BTreeMap<u64, Value>, String> {
    let tag = names::tag_of(c);
    let unit_names: Vec<(u64, String)> = ids
        .iter()
        .filter_map(|g| {
            let u = groups.get(g)?.units.first()?;
            Some((*g, names::name(Kind::Unit, tag, u.id)))
        })
        .collect();
    let v = t.unit_state(
        &unit_names
            .iter()
            .map(|(_, n)| n.clone())
            .collect::<Vec<_>>(),
    )?;
    Ok(unit_names
        .into_iter()
        .filter_map(|(g, n)| {
            let s = &v[n.as_str()];
            let (l, l0) = (s["life"].as_f64()?, s["life0"].as_f64()?);
            Some((
                g,
                json!({
                    "north": s["north"], "east": s["east"], "heading": s["heading"],
                    "life_pct": if l0 > 0.0 { 100.0 * l / l0 } else { 0.0 },
                }),
            ))
        })
        .collect())
}

/// A group's lead unit as the sidecar holds it: north, east, heading and
/// life percentage (life over life_max).
fn held(g: &pb::Group) -> Option<(f64, f64, Option<f64>, Option<f64>)> {
    let u = g.units.first()?;
    let p = u.position.as_ref()?;
    let life = match (u.life, u.life_max) {
        (Some(l), Some(m)) if m > 0.0 => Some(100.0 * l / m),
        _ => None,
    };
    Some((
        p.north,
        p.east,
        u.attitude.as_ref().map(|a| a.heading),
        life,
    ))
}

fn mismatch(
    dir: &str,
    g: u64,
    dcs: &Value,
    side: (f64, f64, Option<f64>, Option<f64>),
    want_life: f64,
) -> Option<Value> {
    let (n, e) = (dcs["north"].as_f64()?, dcs["east"].as_f64()?);
    let gap = (n - side.0).hypot(e - side.1);
    let dh = dcs["heading"]
        .as_f64()
        .zip(side.2)
        .map(|(a, b)| angle_gap(a, b));
    let dl = dcs["life_pct"].as_f64().map(|l| (l - want_life).abs());
    // Leaving, the sidecar must hold the captured life; entering, the
    // caller passes none and DCS's read is what counts.
    let side_life = match side.3 {
        Some(l) => (l - want_life).abs() <= LIFE_PCT,
        None => dir == "in",
    };
    let ok = gap <= POSITION_M
        && dh.is_some_and(|d| d <= HEADING_RAD)
        && dl.is_some_and(|d| d <= LIFE_PCT)
        && side_life;
    (!ok).then(|| {
        json!({
            "dir": dir, "group": g, "gap_m": gap, "heading_gap": dh, "life_gap": dl,
            "dcs": dcs, "sidecar": [side.0, side.1, side.2, side.3], "want_life": want_life,
        })
    })
}

/// The DCS event ID of `S_EVENT_BIRTH`.
const S_EVENT_BIRTH: u32 = 15;

/// The frames of the campaign's unit births after event `after`, and the
/// last event's sequence number.
fn births_since(api: &mut Api, c: u64, after: u64) -> Result<(Vec<u64>, u64), String> {
    let mut frames = Vec::new();
    let mut last = after;
    loop {
        let page = api.events(c, last, 1_000)?;
        let Some(end) = page.last().map(|e| e.sequence) else {
            return Ok((frames, last));
        };
        for e in &page {
            if let Some(pb::event::Body::World(w)) = &e.body
                && w.dcs_id == S_EVENT_BIRTH
                && let Some(st) = &e.stamp
            {
                frames.push(st.frame);
            }
        }
        last = end;
    }
}

/// Births per frame: how many, the frames they span, the most in one frame
/// and the median gap between births, in frames.
fn birth_pace(mut frames: Vec<u64>) -> Value {
    if frames.is_empty() {
        return json!({ "births": 0 });
    }
    frames.sort_unstable();
    let mut per: BTreeMap<u64, usize> = BTreeMap::new();
    for f in &frames {
        *per.entry(*f).or_default() += 1;
    }
    let mut gaps: Vec<u64> = frames.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_unstable();
    json!({
        "births": frames.len(),
        "frames_spanned": frames[frames.len() - 1] - frames[0] + 1,
        "most_in_a_frame": per.values().copied().max().unwrap_or(0),
        "median_gap_frames": gaps.get(gaps.len() / 2),
    })
}

/// Every check's tally, for the verdict.
#[derive(Default)]
struct Tally {
    /// A group lost, unspawnable or dead, or one the controller did not
    /// move that changed state (the sidecar moved its authority).
    bad_states: Vec<Value>,
    duplicates: Vec<Value>,
    handover_bad: Vec<Value>,
    handover_in: usize,
    handover_out: usize,
    /// Route groups checked leaving, and of those how many DCS had turned
    /// away from their spawn heading.
    moving_out: usize,
    moving_turned: usize,
    /// Stationary units DCS hit before they left.
    hit_out: usize,
    largest_gap_m: f64,
    largest_heading_gap: f64,
    moves: Vec<Value>,
}

pub fn run(t: &mut dyn Target, api: &mut Api, tag: &str) -> Result<Value, String> {
    let theater = t.theater();
    let keep = keep(t);
    let c = campaign(t, api, &format!("s20-virtual-{tag}"))?;
    let grid = places(t, (ORIGIN.0, ORIGIN.1), WORLD)?;
    let (n0, n1) = grid
        .iter()
        .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (e0, e1) = grid
        .iter()
        .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));

    // The world, virtual.
    let t_put = Instant::now();
    let mut ids = Vec::with_capacity(WORLD);
    let mut index = BTreeMap::new();
    for (i, p) in grid.iter().enumerate() {
        let order = i.is_multiple_of(10).then(|| {
            route(
                &theater,
                &[(p.0 + 600.0, p.1, 0.0), (p.0 + 600.0, p.1 + 600.0, 0.0)],
                ROUTE_SPEED,
            )
        });
        let id = api.put_group(c, spec(&theater, i, *p), order)?.group_id;
        index.insert(id, i);
        ids.push(id);
    }
    let put_s = t_put.elapsed().as_secs_f64();
    let first = api.groups(c)?;
    let units_at_start: BTreeMap<u64, Vec<u64>> = first
        .iter()
        .map(|(g, grp)| (*g, grp.units.iter().map(|u| u.id).collect()))
        .collect();
    if first.len() != WORLD {
        return Err(format!("{} groups after putting {WORLD}", first.len()));
    }
    let status0 = api.server_status()?;
    let reports0 = t.agent_reports()?.map_or(0, |r| r.len());
    let t_run = Instant::now();

    let mut tally = Tally::default();
    let mut dcs_set: BTreeSet<u64> = BTreeSet::new();
    let (_, mut event_mark) = births_since(api, c, 0)?;
    let mut spawn_heading: BTreeMap<u64, f64> = BTreeMap::new();
    // The life each group's unit has now, percent: set by the spec, then by
    // the hits below.
    let mut life: BTreeMap<u64, f64> = index.iter().map(|(g, i)| (*g, life_of(*i))).collect();
    for (step, f) in PATH.iter().enumerate() {
        let point = (n0 + f.0 * (n1 - n0), e0 + f.1 * (e1 - e0));
        let t_q = Instant::now();
        let (want, radius, queries) = nearest(api, c, &theater, point, keep)?;
        let query_ms = t_q.elapsed().as_secs_f64() * 1e3;
        let entering: Vec<u64> = want.difference(&dcs_set).copied().collect();
        let leaving: Vec<u64> = dcs_set.difference(&want).copied().collect();
        let before = api.groups(c)?;
        // Stationary groups only: a moving one is somewhere else by the
        // time of its capture.
        let still = |g: &u64| !index[g].is_multiple_of(10);
        let in_sample: Vec<u64> = entering
            .iter()
            .copied()
            .filter(still)
            .take(SAMPLE)
            .collect();
        // What the sidecar holds of the entering groups, and a fresh read
        // of the leaving ones from DCS, just before the handover.
        let mut expected_in: BTreeMap<u64, (f64, f64, f64)> = BTreeMap::new();
        for g in &in_sample {
            match held(&before[g]) {
                Some((n, e, Some(h), _)) => {
                    expected_in.insert(*g, (n, e, h));
                }
                _ => tally
                    .handover_bad
                    .push(json!({ "dir": "in", "group": g, "sidecar_held": false })),
            }
        }
        // The heading each entering group spawns with: the one the sidecar
        // holds, the spec's on a first spawn.
        for g in &entering {
            let h = held(&before[g])
                .and_then(|s| s.2)
                .unwrap_or_else(|| heading_of(index[g]));
            spawn_heading.insert(*g, h);
        }
        // DCS hits stationary leaving units, so a capture that kept the
        // stored life would show. A unit whose `OBJECT` record has not come
        // yet has no DCS object in the sidecar, which refuses the effect: it
        // is counted as unbound and the next one is tried.
        let mut out_sample: Vec<u64> = Vec::new();
        let mut unbound_leaving = 0usize;
        for g in leaving.iter().filter(|g| still(g)) {
            if out_sample.len() >= SAMPLE {
                break;
            }
            let hit = if life[g] == 35.0 { 80.0 } else { 35.0 };
            let unit = before[g]
                .units
                .first()
                .map(|u| u.id)
                .ok_or("a group with no unit")?;
            match api.damage(c, unit, hit) {
                Ok(o) if o == pb::Outcome::Applied as i32 => {
                    life.insert(*g, hit);
                    out_sample.push(*g);
                }
                Ok(o) => tally
                    .handover_bad
                    .push(json!({ "dir": "out", "group": g, "damage_outcome": o })),
                Err(e) if e.contains("holds no object") => unbound_leaving += 1,
                Err(e) => tally
                    .handover_bad
                    .push(json!({ "dir": "out", "group": g, "damage_error": e })),
            }
        }
        if !out_sample.is_empty() {
            t.hold(3.0)?;
        }
        // Route groups turn as they drive: their heading at the capture is
        // not the one they spawned with.
        let moving_sample: Vec<u64> = leaving
            .iter()
            .copied()
            .filter(|g| !still(g))
            .take(SAMPLE / 2)
            .collect();
        let read: Vec<u64> = out_sample.iter().chain(&moving_sample).copied().collect();
        let dcs_out = if t.live() {
            dcs_state(t, c, &before, &read)?
        } else {
            BTreeMap::new()
        };
        let t_cmd = Instant::now();
        for chunk in leaving.chunks(CALL_CHUNK) {
            api.materialize(c, chunk, false)?;
        }
        for chunk in entering.chunks(CALL_CHUNK) {
            api.materialize(c, chunk, true)?;
        }
        let untouched: BTreeMap<u64, i32> = before
            .iter()
            .filter(|(g, _)| {
                !want.contains(g) && !dcs_set.contains(g) || want.contains(g) && dcs_set.contains(g)
            })
            .map(|(g, grp)| (*g, grp.state))
            .collect();
        let limit = if t.live() { 900.0 } else { 600.0 };
        let mut polls = 0usize;
        let settled = wait(t, api, "the handover", limit, 2.0, true, |api, _| {
            polls += 1;
            let g = api.groups(c)?;
            for (id, grp) in &g {
                let s = state(grp);
                if matches!(
                    s,
                    EntityState::Lost | EntityState::Unspawnable | EntityState::Dead
                ) {
                    return Err(format!(
                        "move {step}: group {id} is {s:?}: {}",
                        grp.last_error
                    ));
                }
                if untouched.get(id).is_some_and(|s0| *s0 != grp.state) {
                    return Err(format!(
                        "move {step}: group {id} went from {:?} to {s:?} with no command",
                        EntityState::try_from(untouched[id]).unwrap_or(EntityState::Unspecified)
                    ));
                }
            }
            let done = g.iter().all(|(id, grp)| {
                let s = state(grp);
                if want.contains(id) {
                    s == EntityState::Live
                } else {
                    s == EntityState::Virtual
                }
            });
            Ok(done.then_some(g))
        });
        let after = match settled {
            Ok(g) => g,
            Err(e) => {
                tally.bad_states.push(json!({ "move": step, "error": e }));
                return Ok(verdict(&tally, json!({ "stopped_at_move": step })));
            }
        };
        let handover_s = t_cmd.elapsed().as_secs_f64();
        let (birth_frames, mark) = births_since(api, c, event_mark)?;
        event_mark = mark;
        let pace = birth_pace(birth_frames);
        // One unit per group: a birth is a spawn. Every entering group is
        // born once, and no frame holds more births than the budget.
        if t.live()
            && (pace["births"].as_u64() != Some(entering.len() as u64)
                || pace["most_in_a_frame"].as_u64().unwrap_or(0) > MATERIALIZE_PER_FRAME)
        {
            tally.bad_states.push(json!({
                "move": step, "entering": entering.len(), "spawns_per_frame": pace,
            }));
        }
        dcs_set = want.clone();
        // DCS holds each live group once and no other group of the campaign.
        // The fake has no DCS to compare with.
        let dup = if t.live() {
            one_per_entity(t, api, c)?
        } else {
            json!({ "pass": true })
        };
        if dup["pass"] != json!(true) {
            tally.duplicates.push(json!({ "move": step, "check": dup }));
        }
        // Out: the sidecar now holds what DCS had. A sampled group with no
        // DCS read or no state in the sidecar is a failure, not a skip.
        for g in if t.live() { &out_sample[..] } else { &[][..] } {
            match (dcs_out.get(g), held(&after[g])) {
                (Some(dcs), Some(side)) => {
                    tally.handover_out += 1;
                    tally.hit_out += 1;
                    if let Some(m) = mismatch("out", *g, dcs, side, life[g]) {
                        tally.handover_bad.push(m);
                    }
                    if let (Some(n), Some(e)) = (dcs["north"].as_f64(), dcs["east"].as_f64()) {
                        tally.largest_gap_m =
                            tally.largest_gap_m.max((n - side.0).hypot(e - side.1));
                    }
                }
                (dcs, side) => tally.handover_bad.push(json!({
                    "dir": "out", "group": g, "dcs": dcs, "sidecar_held": side.is_some(),
                })),
            }
        }
        // Out, route groups: the captured heading is the one DCS had at the
        // read, and the position is no farther than the unit could drive
        // between the read and the end of the handover.
        let reach_m = POSITION_M + ROUTE_SPEED * (handover_s + 5.0);
        for g in if t.live() {
            &moving_sample[..]
        } else {
            &[][..]
        } {
            let dcs = dcs_out.get(g);
            let side = held(&after[g]);
            let checked = dcs.zip(side).and_then(|(d, s)| {
                let (n, e, h) = (
                    d["north"].as_f64()?,
                    d["east"].as_f64()?,
                    d["heading"].as_f64()?,
                );
                Some((
                    (n - s.0).hypot(e - s.1),
                    angle_gap(h, s.2?),
                    angle_gap(
                        h,
                        spawn_heading
                            .get(g)
                            .copied()
                            .unwrap_or_else(|| heading_of(index[g])),
                    ),
                ))
            });
            match checked {
                Some((gap, dh, turned)) => {
                    tally.moving_out += 1;
                    if turned > MOVING_HEADING_RAD {
                        tally.moving_turned += 1;
                    }
                    if gap > reach_m || dh > MOVING_HEADING_RAD {
                        tally.handover_bad.push(json!({
                            "dir": "out", "moving": true, "group": g, "gap_m": gap,
                            "reach_m": reach_m, "heading_gap": dh, "dcs": dcs,
                        }));
                    }
                }
                None => tally.handover_bad.push(json!({
                    "dir": "out", "moving": true, "group": g, "dcs": dcs, "sidecar_held": side.is_some(),
                })),
            }
        }
        // In: DCS has what the sidecar held, once its DAMAGE applied.
        if t.live() && !expected_in.is_empty() {
            t.hold(5.0)?;
            let ids_in: Vec<u64> = expected_in.keys().copied().collect();
            let dcs_in = dcs_state(t, c, &after, &ids_in)?;
            for (g, (n, e, h)) in &expected_in {
                let Some(dcs) = dcs_in.get(g) else {
                    tally
                        .handover_bad
                        .push(json!({ "dir": "in", "group": g, "dcs": null }));
                    continue;
                };
                tally.handover_in += 1;
                if let Some(m) = mismatch("in", *g, dcs, (*n, *e, Some(*h), None), life[g]) {
                    tally.handover_bad.push(m);
                }
                if let (Some(dn), Some(de)) = (dcs["north"].as_f64(), dcs["east"].as_f64()) {
                    tally.largest_gap_m = tally.largest_gap_m.max((dn - n).hypot(de - e));
                }
                if let Some(dh) = dcs["heading"].as_f64() {
                    tally.largest_heading_gap = tally.largest_heading_gap.max(angle_gap(dh, *h));
                }
            }
        }
        println!(
            "   move {step}: +{} -{} in {handover_s:.1} s",
            entering.len(),
            leaving.len()
        );
        tally.moves.push(json!({
            "move": step, "point": [point.0, point.1], "radius_m": radius,
            "queries": queries, "query_ms": query_ms,
            "entering": entering.len(), "leaving": leaving.len(),
            "handover_s": handover_s, "polls": polls, "births": pace,
            "unbound_hit_tries": unbound_leaving,
        }));
    }
    let run_s = t_run.elapsed().as_secs_f64();

    // Every group and unit keeps its ID.
    let last = api.groups(c)?;
    let units_now: BTreeMap<u64, Vec<u64>> = last
        .iter()
        .map(|(g, grp)| (*g, grp.units.iter().map(|u| u.id).collect()))
        .collect();
    let ids_kept = units_now == units_at_start;
    let status1 = api.server_status()?;
    let reports: Vec<Value> = t
        .agent_reports()?
        .map(|r| r.into_iter().skip(reports0).collect())
        .unwrap_or_default();
    let shares: Vec<f64> = reports.iter().filter_map(|r| r["share"].as_f64()).collect();
    let mean_share = if shares.is_empty() {
        None
    } else {
        Some(shares.iter().sum::<f64>() / shares.len() as f64)
    };
    let tail = crate::sampling::live::tail(&reports);
    let timings = t.timings()?;
    let over_budget = status1
        .commands_over_budget
        .saturating_sub(status0.commands_over_budget);
    let owner_targets = if t.live() {
        json!(mean_share.is_some_and(|s| s <= 0.003) && tail["p99_within_250"] == json!(true))
    } else {
        Value::Null
    };
    let extra = json!({
        "campaign": c, "world": WORLD, "keep": keep,
        "ids_kept": ids_kept,
        "put_world_s": put_s, "run_s": run_s,
        "agent": {
            "reports": reports.len(), "mean_share": mean_share,
            "max_share": shares.iter().copied().fold(None, |a: Option<f64>, s| Some(a.map_or(s, |a| a.max(s)))),
            "tail": tail, "owner_targets": owner_targets,
        },
        "commands_over_budget": over_budget,
        "materialize_per_frame": MATERIALIZE_PER_FRAME,
        "overhead_share_status": status1.overhead_share,
        "sidecar_timings": timings.iter().rev().take(20).collect::<Vec<_>>(),
    });
    let (tally_moving_turned, tally_hit_out) = (tally.moving_turned, tally.hit_out);
    let mut v = verdict(&tally, extra);
    // On the server the run passes only with no command over the
    // commanded-work budget; births per frame are checked at each move (in
    // `bad_states`), and the hits and turns must have made the handover
    // checks able to fail.
    let pass = v["pass"] == json!(true)
        && ids_kept
        && owner_targets != json!(false)
        && (!t.live() || over_budget == 0)
        && (!t.live() || (tally_moving_turned > 0 && tally_hit_out > 0));
    v["pass"] = json!(pass);
    Ok(v)
}

fn verdict(tally: &Tally, extra: Value) -> Value {
    let pass =
        tally.bad_states.is_empty() && tally.duplicates.is_empty() && tally.handover_bad.is_empty();
    let mut v = json!({
        "pass": pass,
        "bad_states": tally.bad_states,
        "duplicates": tally.duplicates,
        "handover": {
            "in_checked": tally.handover_in, "out_checked": tally.handover_out,
            "hit_before_leaving": tally.hit_out,
            "moving_out_checked": tally.moving_out, "moving_turned": tally.moving_turned,
            "bad": tally.handover_bad,
            "largest_gap_m": tally.largest_gap_m,
            "largest_heading_gap_rad": tally.largest_heading_gap,
            "tolerances": {
                "position_m": POSITION_M, "heading_rad": HEADING_RAD, "life_pct": LIFE_PCT,
                "moving_heading_rad": MOVING_HEADING_RAD, "route_speed_m_s": ROUTE_SPEED,
            },
        },
        "moves": tally.moves,
    });
    if let (Some(o), Some(x)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(x.clone());
    }
    v
}
