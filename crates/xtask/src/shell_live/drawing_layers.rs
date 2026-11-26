//! `sidecar <name> --part drawing-layers`: markup groups of drawings (spec/world.md,
//! "Labels and markup groups") against a running DCS.
//!
//! The run makes a campaign on Caucasus, then:
//!
//! 1. `burst`: `PutMarkupGroup` `np-front` and `np-low`,
//!    `ReplaceMarkupGroup` `np-front` with 50 freeform drawings of 100 points,
//!    `PutDrawing` of 3 two-point shape-3 drawings (lines) in `np-low`. The run waits until every
//!    item's applied spec revision equals its spec revision, then reads
//!    `world.getMarkPanels`: 53 panels with an ID from 2^30, ascending in
//!    the order the items were added: the 50 front panels, then the 3 low
//!    ones (marks and drawings share one pool and each add takes the next
//!    ID, so there is no per-group order). The low items stand far east
//!    of the front freeforms, which tells the panels apart. The agent's
//!    sampling windows from the first call to the last applied item are
//!    summed.
//! 2. `in_place`: `PutDrawing` of one front item by label with only its color
//!    changed. Its spec revision rises and is applied, and the panel IDs
//!    are the same before and after. A fallback to `DRAWING_SET` keeps the
//!    panel IDs too, so the verdict reads the API's events: the change
//!    must emit no `S_EVENT_MARK_ADDED`, `_CHANGE` or `_REMOVED` for an ID
//!    from 2^30, and the burst of step 1 must have shown the events arrive.
//! 3. `hide`: `np-front` hidden: the front items' applied revision reads 0,
//!    `ListWorld` gives them `dcs_id` 0 and 3 panels remain. Shown again: 53
//!    panels; the 50 new panel IDs are all above every ID from before the
//!    hide, so the low panels (kept) come first and the front panels last,
//!    and the panel IDs equal the `dcs_id`s `ListWorld` lists. The mark
//!    events of the hide and show are all `platform_mark`, with the 50 adds
//!    among them. The windows of the second burst are summed.
//! 4. `restart`: the harness has no sidecar restart (the sidecar's container
//!    has no restart policy and no part restarts it), so the part records
//!    that it is skipped. The burst of step 1 is the one recorded.
//! 5. `cleanup`: `RemoveMarkupGroup` of both markup groups; no panel from 2^30 remains and
//!    no drawing is listed. Cleanup runs also when a step failed.
//!
//! The run resumes the mission through the shared `wait` and puts the pause
//! state it found back.

use std::time::{Duration, Instant};

use super::payload_preset::sv;
use super::static_replace::{PAUSED_LUA, restore_pause};
use super::*;

const THEATER: &str = "Caucasus";
const FRONT: &str = "np-front";
const LOW: &str = "np-low";
const FRONT_ITEMS: usize = 50;
const LOW_ITEMS: usize = 3;
const POINTS: usize = 100;
/// The lowest DCS ID the sidecar gives a mark or drawing (2^30).
const SCRIPT_ID_BASE: u64 = 1 << 30;
/// DCS's mark events: added, changed, removed (spec/world.md).
const MARK_EVENTS: [u32; 3] = [25, 26, 27];
const MARK_ADDED: u32 = 25;
/// The freeform shape and shape 3, a polygon through its points (a line with two).
const FREEFORM: u32 = 7;
const TWO_POINT: u32 = 3;
/// North and east of the front items' grid, meters; the grid is 5 rows of 10
/// columns, 1.5 km apart.
const BASE: (f64, f64) = (-289_000.0, 681_000.0);
const GRID_M: f64 = 1_500.0;
const COLUMNS: usize = 10;
const RADIUS_M: f64 = 500.0;
/// How far east of the grid's start the low items stand, and the east
/// coordinate that tells a low panel (at or past it) from a front one.
const LOW_EAST_M: f64 = 30_000.0;
const SPLIT_EAST_M: f64 = 20_000.0;
const RED: [f64; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f64; 4] = [0.0, 1.0, 0.0, 1.0];
/// Not design limits; only when this test gives up.
const APPLY_LIMIT: Duration = Duration::from_secs(300);
/// The agent reports every 10 s; this is the wait for the last window.
const REPORT_WAIT: Duration = Duration::from_secs(12);

// ---- specs -----------------------------------------------------------------

fn all() -> Option<pb::Audience> {
    Some(pb::Audience {
        to: Some(pb::audience::To::All(true)),
    })
}

fn list(v: Value) -> Option<prost_types::ListValue> {
    match sv(v).kind {
        Some(prost_types::value::Kind::ListValue(l)) => Some(l),
        _ => None,
    }
}

fn point(north: f64, east: f64) -> Value {
    json!({ "x": north, "y": 0.0, "z": east })
}

/// A drawing's arguments: the points, the color, the fill color, line type
/// 1, read only, an empty message. DCS requires the fill color.
fn args(points: Vec<Value>, color: [f64; 4]) -> Option<prost_types::ListValue> {
    let mut v = points;
    v.extend([
        json!(color),
        json!([color[0], color[1], color[2], 0.2]),
        json!(1),
        json!(true),
        json!(""),
    ]);
    list(Value::Array(v))
}

/// Where front item `k`'s circle is centered, north and east in meters.
fn center(k: usize) -> (f64, f64) {
    (
        BASE.0 + (k / COLUMNS) as f64 * GRID_M,
        BASE.1 + (k % COLUMNS) as f64 * GRID_M,
    )
}

fn freeform(k: usize, color: [f64; 4]) -> Option<prost_types::ListValue> {
    let (n, e) = center(k);
    let pts = (0..POINTS)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / POINTS as f64;
            point(n + RADIUS_M * a.cos(), e + RADIUS_M * a.sin())
        })
        .collect();
    args(pts, color)
}

fn two_point(k: usize) -> Option<prost_types::ListValue> {
    let (n, e) = (BASE.0 + k as f64 * GRID_M, BASE.1 + LOW_EAST_M);
    args(vec![point(n, e), point(n + 400.0, e + 400.0)], GREEN)
}

fn front_label(k: usize) -> String {
    format!("front-{k}")
}

fn low_label(k: usize) -> String {
    format!("low-{k}")
}

fn markup_group(name: &str, visible: bool) -> pb::MarkupGroup {
    pb::MarkupGroup {
        name: name.into(),
        visible,
        expire_after_s: None,
        cap: None,
    }
}

fn front_items() -> Vec<pb::MarkupGroupItem> {
    (0..FRONT_ITEMS)
        .map(|k| pb::MarkupGroupItem {
            label: front_label(k),
            audience: all(),
            expire_after_s: None,
            kind: Some(pb::markup_group_item::Kind::Drawing(
                pb::MarkupGroupDrawing {
                    shape: FREEFORM,
                    args: freeform(k, RED),
                },
            )),
        })
        .collect()
}

// ---- pure checks -----------------------------------------------------------

/// A drawing in the World.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    id: u64,
    /// The ID DCS shows it under, 0 when it is not shown.
    dcs_id: u64,
    label: String,
    markup_group: String,
    spec: u64,
    applied: u64,
    error: String,
}

fn in_markup_group<'a>(items: &'a [Item], name: &str) -> Vec<&'a Item> {
    items.iter().filter(|i| i.markup_group == name).collect()
}

/// Whether the markup group holds `n` items and each one's applied spec revision
/// equals its spec revision.
fn applied_all(items: &[&Item], n: usize) -> bool {
    items.len() == n && items.iter().all(|i| i.spec > 0 && i.applied == i.spec)
}

/// Whether the markup group holds `n` items and none is applied.
fn hidden_all(items: &[&Item], n: usize) -> bool {
    items.len() == n && items.iter().all(|i| i.applied == 0)
}

/// One panel of `world.getMarkPanels` with an ID from [`SCRIPT_ID`].
#[derive(Debug, Clone, PartialEq)]
struct Panel {
    idx: u64,
    east: Option<f64>,
}

/// The panels of a read ([`PANELS`]). An empty Lua table is JSON `{}`.
fn panels(seen: &Value) -> Vec<Panel> {
    seen["panels"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some(Panel {
                        idx: p["idx"].as_f64()? as u64,
                        east: p["z"].as_f64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn idxs(p: &[Panel]) -> Vec<u64> {
    p.iter().map(|p| p.idx).collect()
}

/// The verdict on a read of the panels: `count` panels from [`SCRIPT_ID`],
/// listed in ascending ID order, `low` of them in the low area and the rest
/// in the front area. All marks and drawings take their DCS IDs from one
/// pool, in the order they are added, so `low_last` says whether the low
/// panels were added after the front ones (the burst) or before them (a show
/// re-adds the front ones last): every low panel then sits after, or before,
/// every front panel.
fn order_verdict(p: &[Panel], count: usize, low: usize, low_last: bool) -> Value {
    let is_low = |q: &&Panel| q.east.is_some_and(|e| e >= BASE.1 + SPLIT_EAST_M);
    let lows: Vec<u64> = p.iter().filter(is_low).map(|q| q.idx).collect();
    let fronts: Vec<u64> = p.iter().filter(|q| !is_low(q)).map(|q| q.idx).collect();
    let ascending = p.windows(2).all(|w| w[0].idx < w[1].idx);
    let located = p.iter().all(|q| q.east.is_some());
    let ordered = if low_last {
        matches!((fronts.iter().max(), lows.iter().min()), (Some(f), Some(l)) if f < l)
    } else {
        matches!((lows.iter().max(), fronts.iter().min()), (Some(l), Some(f)) if l < f)
    };
    let counts = p.len() == count && lows.len() == low && fronts.len() == count - low;
    json!({
        "pass": counts && ascending && located && (ordered || low == 0 || count == low),
        "count": p.len(),
        "wanted": count,
        "low": lows.len(),
        "front": fronts.len(),
        "ascending": ascending,
        "located": located,
        "in_add_order": ordered,
        "low_ids": lows,
        "front_first": fronts.first(),
        "front_last": fronts.last(),
    })
}

/// A mark event of the API: the DCS event ID and the mark's ID.
#[derive(Debug, Clone, PartialEq)]
struct MarkEvent {
    dcs_id: u32,
    mark_id: Option<u64>,
    /// `WorldEvent.platform_mark`: the ID was issued by the platform in the
    /// DCS process now running.
    platform_mark: bool,
}

/// A mark event of the platform's own marks and drawings.
fn scripted(e: &MarkEvent) -> bool {
    MARK_EVENTS.contains(&e.dcs_id) && e.platform_mark
}

/// The verdict on the IDs of a hide and a show of the front group.
/// `before` is every panel ID before the hide, `hidden` the front items as
/// listed while hidden, `shown` all items listed after the show,
/// `after` every panel ID after it and `during` the mark events from the
/// hide to the show. Hidden items list `dcs_id` 0. A show adds every item
/// again under a new ID, all above every ID before; the panel IDs are the
/// listed `dcs_id`s; and every event for an ID from 2^30 is `platform_mark`,
/// with the show's adds among them.
fn id_verdict(
    before: &[u64],
    hidden: &[Item],
    shown: &[Item],
    after: &[u64],
    during: &[MarkEvent],
    adds_wanted: usize,
) -> Value {
    let zero = hidden.len() == FRONT_ITEMS && hidden.iter().all(|i| i.dcs_id == 0);
    let top_before = before.iter().max().copied().unwrap_or(0);
    let fresh: Vec<u64> = after
        .iter()
        .copied()
        .filter(|id| !before.contains(id))
        .collect();
    let above = fresh.len() == adds_wanted && fresh.iter().all(|id| *id > top_before);
    let mut listed: Vec<u64> = shown
        .iter()
        .map(|i| i.dcs_id)
        .filter(|id| *id != 0)
        .collect();
    let mut painted = after.to_vec();
    listed.sort_unstable();
    painted.sort_unstable();
    let equal = listed == painted;
    let unmarked: Vec<u64> = during
        .iter()
        .filter(|e| MARK_EVENTS.contains(&e.dcs_id) && !e.platform_mark)
        .filter_map(|e| e.mark_id)
        .filter(|m| *m >= SCRIPT_ID_BASE)
        .collect();
    let adds = during
        .iter()
        .filter(|e| e.dcs_id == MARK_ADDED && scripted(e))
        .count();
    json!({
        "pass": zero && above && equal && unmarked.is_empty() && adds >= adds_wanted,
        "hidden_dcs_id_0": zero,
        "new_ids_above_old": above,
        "new_ids": fresh.len(),
        "top_before": top_before,
        "panel_ids_equal_listed": equal,
        "scripted_ids_without_platform_mark": unmarked,
        "platform_adds": adds,
        "adds_wanted": adds_wanted,
    })
}

/// The verdict on an in-place change. `earlier` is the mark events before
/// it: the burst's `wanted` adds must be among them, or the events do not
/// arrive and a quiet change proves nothing. `during` is the mark events
/// from the change on: a fallback to `DRAWING_SET` removes and adds the ID,
/// so it must hold none for an ID from 2^30.
fn in_place_events(earlier: &[MarkEvent], during: &[MarkEvent], wanted: usize) -> Value {
    let seen = earlier
        .iter()
        .filter(|e| e.dcs_id == MARK_ADDED && scripted(e))
        .count();
    let loud: Vec<&MarkEvent> = during.iter().filter(|e| scripted(e)).collect();
    json!({
        "pass": seen >= wanted && loud.is_empty(),
        "adds_before": seen,
        "adds_wanted": wanted,
        "events_during": loud.len(),
        "ids_during": loud.iter().filter_map(|e| e.mark_id).collect::<Vec<_>>(),
        "kinds_during": loud.iter().map(|e| e.dcs_id).collect::<Vec<_>>(),
    })
}

/// The sum of the sampling windows given: frames, frames over 250 us, the
/// longest frame, starved frames, overrun frames and the real time covered.
fn sum_windows(lines: &[Value]) -> Value {
    let sum = |k: &str| {
        lines
            .iter()
            .filter_map(|l| l["data"][k].as_f64())
            .sum::<f64>()
    };
    let max = lines
        .iter()
        .filter_map(|l| l["data"]["frame_max_us"].as_f64())
        .fold(0.0, f64::max);
    json!({
        "windows": lines.len(),
        "window_s": sum("window_s"),
        "frames": sum("frames"),
        "frames_over_250": sum("frames_over_250"),
        "frame_max_us": max,
        "starved": sum("starved"),
        "overrun_frames": sum("overrun_frames"),
        "us_fixed": sum("us_fixed"),
    })
}

// ---- Lua -------------------------------------------------------------------

/// The panels with an ID from 2^30 and the count of all panels. DCS lists
/// panels in ascending ID order (LIVE, docs/results/2026-11-25-drawing-layers.md).
/// A panel's ID is `idx`; the mock names it `id`.
const PANELS: &str = r#"local base = 1073741824
local ok, list = pcall(world.getMarkPanels)
if not ok or type(list) ~= 'table' then
  return { error = tostring(list), panels = {}, total = 0 }
end
local out = { panels = {}, total = 0 }
for _, p in ipairs(list) do
  out.total = out.total + 1
  local id = p.idx or p.id
  if type(id) == 'number' and id >= base then
    local pos = p.pos
    out.panels[#out.panels + 1] = { idx = id, x = pos and pos.x, z = pos and pos.z }
  end
end
return out
"#;

// ---- API -------------------------------------------------------------------

fn markup_groups(api: &mut Api, c: u64) -> Result<Vec<pb::MarkupGroup>, String> {
    Ok(api
        .rt
        .block_on(
            api.players
                .list_markup_groups(pb::ListMarkupGroupsRequest { campaign_id: c }),
        )
        .map_err(status_text("ListMarkupGroups"))?
        .into_inner()
        .markup_groups)
}

fn put_markup_group(api: &mut Api, c: u64, l: pb::MarkupGroup) -> Result<(), String> {
    let req = pb::PutMarkupGroupRequest {
        write: api.write("markup-group", c),
        markup_group: Some(l),
    };
    api.rt
        .block_on(api.players.put_markup_group(req))
        .map(drop)
        .map_err(status_text("PutMarkupGroup"))
}

fn remove_markup_group(api: &mut Api, c: u64, name: &str) -> Result<(), String> {
    let req = pb::RemoveMarkupGroupRequest {
        write: api.write("remove-markup-group", c),
        name: name.into(),
    };
    api.rt
        .block_on(api.players.remove_markup_group(req))
        .map(drop)
        .map_err(status_text("RemoveMarkupGroup"))
}

fn replace_markup_group(
    api: &mut Api,
    c: u64,
    name: &str,
    items: Vec<pb::MarkupGroupItem>,
) -> Result<Vec<u64>, String> {
    let req = pb::ReplaceMarkupGroupRequest {
        write: api.write("replace", c),
        name: name.into(),
        items,
    };
    Ok(api
        .rt
        .block_on(api.players.replace_markup_group(req))
        .map_err(status_text("ReplaceMarkupGroup"))?
        .into_inner()
        .ids)
}

fn put_drawing(
    api: &mut Api,
    c: u64,
    what: &str,
    label: &str,
    markup_group: &str,
    args: Option<prost_types::ListValue>,
    shape: u32,
) -> Result<u64, String> {
    let req = pb::PutDrawingRequest {
        write: api.write(what, c),
        drawing_id: 0,
        audience: all(),
        shape,
        args,
        label: label.into(),
        markup_group: markup_group.into(),
        expire_after_s: None,
    };
    Ok(api
        .rt
        .block_on(api.players.put_drawing(req))
        .map_err(status_text("PutDrawing"))?
        .into_inner()
        .drawing_id)
}

/// The campaign's drawings in the World.
fn drawings(api: &mut Api, c: u64) -> Result<Vec<Item>, String> {
    let mut s = api
        .rt
        .block_on(api.world.list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: c,
                kinds: vec![pb::EntityKind::Drawing as i32],
                ..pb::WorldFilter::default()
            }),
        }))
        .map_err(status_text("ListWorld"))?
        .into_inner();
    let mut out = Vec::new();
    while let Some(page) = api
        .rt
        .block_on(s.message())
        .map_err(status_text("ListWorld"))?
    {
        for e in page.entities {
            if let Some(entity::Kind::Drawing(d)) = e.kind {
                out.push(Item {
                    id: d.id,
                    dcs_id: d.dcs_id,
                    label: d.label,
                    markup_group: d.markup_group,
                    spec: d.spec_revision,
                    applied: d.applied_spec_revision,
                    error: d.last_error,
                });
            }
        }
    }
    Ok(out)
}

/// The campaign's mark events after sequence `after`, with the last
/// sequence read.
fn mark_events(api: &mut Api, c: u64, after: u64) -> Result<(u64, Vec<MarkEvent>), String> {
    let (mut last, mut out) = (after, Vec::new());
    loop {
        let r = api
            .rt
            .block_on(api.events.read_events(pb::ReadEventsRequest {
                filter: Some(pb::EventFilter {
                    campaign_id: c,
                    dcs_ids: MARK_EVENTS.to_vec(),
                    ..pb::EventFilter::default()
                }),
                after_sequence: last,
                limit: 1_000,
            }))
            .map_err(status_text("ReadEvents"))?
            .into_inner();
        for e in &r.events {
            last = last.max(e.sequence);
            if let Some(pb::event::Body::World(w)) = &e.body {
                out.push(MarkEvent {
                    dcs_id: w.dcs_id,
                    mark_id: w.mark_id,
                    platform_mark: w.platform_mark,
                });
            }
        }
        if !r.more || r.events.is_empty() {
            return Ok((last, out));
        }
    }
}

fn read_panels(run: &dyn Live, stem: &str) -> Result<Value, String> {
    run.eval("server", stem, PANELS, 60)
}

// ---- sampling --------------------------------------------------------------

/// Every sampling report in the agent's log, oldest first.
fn sampling_lines(run: &dyn Live) -> Result<Vec<Value>, String> {
    let log = run
        .profile()
        .read("nanoprobe/logs/agent.log")?
        .ok_or("no agent.log")?;
    Ok(String::from_utf8_lossy(&log)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["msg"] == json!("sampling"))
        .collect())
}

/// The number of sampling reports now: where a burst's windows start.
fn mark(run: &dyn Live) -> usize {
    sampling_lines(run).map_or(0, |l| l.len())
}

/// The windows since `start`, summed, after the last window had time to be
/// written. `applied_s` is the real time from the first call to the last item
/// applied, and `commands` the items sent.
fn burst(
    run: &dyn Live,
    api: &mut Api,
    start: usize,
    applied_s: f64,
    commands: usize,
) -> Result<Value, String> {
    std::thread::sleep(REPORT_WAIT);
    api.keep();
    let lines = sampling_lines(run)?;
    let mut v = sum_windows(&lines[start.min(lines.len())..]);
    v["applied_s"] = json!(applied_s);
    v["commands"] = json!(commands);
    Ok(v)
}

// ---- the run ---------------------------------------------------------------

/// Run the part; the result goes under `drawing_layers`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    results: &mut Value,
) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);
    let found_paused = run.eval("hook", "drawing-layers-paused", PAUSED_LUA, 60)?;
    println!("== drawing-layers: a campaign on {THEATER}");
    let c = api.create("drawing-layers-live", &[THEATER], true)?;
    loaded(run, &mut api, THEATER, generation)?;
    api.renew()?;

    let mut v = json!({ "steps": {} });
    let ran = steps(run, &mut api, c, &mut v);
    println!("== drawing-layers: remove the markup groups");
    let cleanup = recorded(cleanup(run, &mut api, c));
    v["steps"]["cleanup"] = cleanup;
    let restored = recorded(restore_pause(run, &mut api, &found_paused));
    v["restore_pause"] = restored;
    v["pause_found"] = found_paused;
    if let Err(e) = &ran {
        v["error"] = json!(e);
    }
    v["pass"] = json!(
        ran.is_ok()
            && v["restore_pause"]["pass"] == json!(true)
            && v["steps"]
                .as_object()
                .is_some_and(|o| o.values().all(|s| s["pass"] == json!(true)))
    );
    println!(
        "   {}",
        if v["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        }
    );
    results["drawing_layers"] = v;
    Ok(())
}

fn recorded(r: Result<Value, String>) -> Value {
    r.unwrap_or_else(|e| json!({ "pass": false, "error": e }))
}

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

fn steps(run: &dyn Live, api: &mut Api, c: u64, rec: &mut Value) -> Result<(), String> {
    println!("== drawing-layers: two markup groups, 50 freeforms, 3 two-point shapes");
    step(rec, "burst", first_burst(run, api, c))?;
    println!("== drawing-layers: a color changed in place");
    step(rec, "in_place", in_place(run, api, c))?;
    println!("== drawing-layers: hide and show np-front");
    step(rec, "hide", hide_show(run, api, c))?;
    rec["steps"]["restart"] = json!({
        "pass": true,
        "skipped": "the harness has no sidecar restart: its container has no restart policy and no part restarts it",
    });
    Ok(())
}

/// Wait until the two markup groups' items are applied, the time it took.
fn wait_applied(run: &dyn Live, api: &mut Api, c: u64, front: usize) -> Result<f64, String> {
    let t = Instant::now();
    wait(run, api, APPLY_LIMIT, "every item applied", |api| {
        let d = drawings(api, c)?;
        Ok((applied_all(&in_markup_group(&d, FRONT), front)
            && applied_all(&in_markup_group(&d, LOW), LOW_ITEMS))
        .then_some(()))
    })?;
    Ok(t.elapsed().as_secs_f64())
}

/// How many panel IDs a timeout message lists.
const WAIT_IDS_SHOWN: usize = 10;

/// The text of a panel wait that timed out: the phase, the wanted count, the
/// count of the last read and its first IDs. `last` is `None` when no read
/// came back.
fn panel_wait_error(phase: &str, wanted: usize, last: Option<&[Panel]>, cause: &str) -> String {
    let read = match last {
        None => "no read".to_string(),
        Some(p) => {
            let ids: Vec<String> = p
                .iter()
                .take(WAIT_IDS_SHOWN)
                .map(|q| q.idx.to_string())
                .collect();
            let more = p.len().saturating_sub(WAIT_IDS_SHOWN);
            let tail = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            format!("last read {} panels [{}{tail}]", p.len(), ids.join(", "))
        }
    };
    format!("the panels {phase}: {cause}; wanted {wanted}, {read}")
}

/// Wait until the read has `count` panels from 2^30; the last read. A timeout
/// names the `phase`, the wanted count and the last read.
fn wait_panels(
    run: &dyn Live,
    api: &mut Api,
    stem: &str,
    phase: &str,
    count: usize,
) -> Result<Value, String> {
    let mut last: Option<Vec<Panel>> = None;
    wait(run, api, APPLY_LIMIT, "the panels", |_| {
        let seen = read_panels(run, stem)?;
        let now = panels(&seen);
        let done = now.len() == count;
        last = Some(now);
        Ok(done.then_some(seen))
    })
    .map_err(|e| panel_wait_error(phase, count, last.as_deref(), &e))
}

fn first_burst(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    let start = mark(run);
    let t = Instant::now();
    put_markup_group(api, c, markup_group(FRONT, true))?;
    put_markup_group(api, c, markup_group(LOW, true))?;
    let ids = replace_markup_group(api, c, FRONT, front_items())?;
    for k in 0..LOW_ITEMS {
        put_drawing(api, c, "low", &low_label(k), LOW, two_point(k), TWO_POINT)?;
    }
    let applied_s = wait_applied(run, api, c, FRONT_ITEMS)
        .map(|_| t.elapsed().as_secs_f64())
        .map_err(|e| format!("{e} (items sent: {})", ids.len()))?;
    let seen = wait_panels(
        run,
        api,
        "drawing-layers-panels",
        "after the first burst",
        FRONT_ITEMS + LOW_ITEMS,
    )?;
    let verdict = order_verdict(&panels(&seen), FRONT_ITEMS + LOW_ITEMS, LOW_ITEMS, true);
    let listed = markup_groups(api, c)?;
    let markup_groups_ok = [FRONT, LOW]
        .iter()
        .all(|n| listed.iter().any(|l| l.name == *n && l.visible));
    let bursts = burst(run, api, start, applied_s, FRONT_ITEMS + LOW_ITEMS)?;
    Ok(json!({
        "pass": verdict["pass"] == json!(true) && markup_groups_ok && ids.len() == FRONT_ITEMS,
        "replace_ids": ids.len(),
        "markup_groups_ok": markup_groups_ok,
        "markup_groups": listed.iter().map(|l| json!({ "name": l.name, "visible": l.visible })).collect::<Vec<_>>(),
        "panels": verdict,
        "total_panels": seen["total"],
        "agent_windows": bursts,
    }))
}

fn in_place(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    let k = 7;
    let before = drawings(api, c)?;
    let was = before
        .iter()
        .find(|i| i.label == front_label(k))
        .cloned()
        .ok_or("the item to change is not listed")?;
    let panels_before = panels(&read_panels(run, "drawing-layers-before")?);
    let (last, earlier) = mark_events(api, c, 0)?;
    put_drawing(
        api,
        c,
        "color",
        &front_label(k),
        FRONT,
        freeform(k, GREEN),
        FREEFORM,
    )?;
    let now = wait(run, api, APPLY_LIMIT, "the revision applied", |api| {
        let d = drawings(api, c)?;
        Ok(d.into_iter()
            .find(|i| i.label == front_label(k) && i.spec > was.spec && i.applied == i.spec))
    })?;
    // Let an event the change caused reach the log.
    run.hold(3.0)?;
    api.keep();
    let panels_after = panels(&read_panels(run, "drawing-layers-after")?);
    let (_, during) = mark_events(api, c, last)?;
    let events = in_place_events(&earlier, &during, FRONT_ITEMS + LOW_ITEMS);
    let same = idxs(&panels_before) == idxs(&panels_after);
    let one = drawings(api, c)?
        .iter()
        .filter(|i| i.label == front_label(k))
        .count()
        == 1;
    Ok(json!({
        "pass": same
            && one
            && now.id == was.id
            && panels_after.len() == FRONT_ITEMS + LOW_ITEMS
            && events["pass"] == json!(true),
        "same_entity": now.id == was.id,
        "one_item_of_the_label": one,
        "spec_before": was.spec,
        "spec_after": now.spec,
        "applied_after": now.applied,
        "panel_ids_unchanged": same,
        "panels_after": panels_after.len(),
        "events": events,
    }))
}

fn hide_show(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    let before = idxs(&panels(&read_panels(run, "drawing-layers-prehide")?));
    let (last, _) = mark_events(api, c, 0)?;
    put_markup_group(api, c, markup_group(FRONT, false))?;
    wait(run, api, APPLY_LIMIT, "np-front hidden", |api| {
        let d = drawings(api, c)?;
        Ok((hidden_all(&in_markup_group(&d, FRONT), FRONT_ITEMS)).then_some(()))
    })?;
    let hidden_items: Vec<Item> = in_markup_group(&drawings(api, c)?, FRONT)
        .into_iter()
        .cloned()
        .collect();
    let hidden = wait_panels(run, api, "drawing-layers-hidden", "after hide", LOW_ITEMS)?;
    let start = mark(run);
    let t = Instant::now();
    put_markup_group(api, c, markup_group(FRONT, true))?;
    let applied_s = wait_applied(run, api, c, FRONT_ITEMS).map(|_| t.elapsed().as_secs_f64())?;
    let shown = wait_panels(
        run,
        api,
        "drawing-layers-shown",
        "after show",
        FRONT_ITEMS + LOW_ITEMS,
    )?;
    let verdict = order_verdict(&panels(&shown), FRONT_ITEMS + LOW_ITEMS, LOW_ITEMS, false);
    // Let the show's events reach the log.
    run.hold(3.0)?;
    api.keep();
    let (_, during) = mark_events(api, c, last)?;
    let ids = id_verdict(
        &before,
        &hidden_items,
        &drawings(api, c)?,
        &idxs(&panels(&shown)),
        &during,
        FRONT_ITEMS,
    );
    let windows = burst(run, api, start, applied_s, FRONT_ITEMS)?;
    Ok(json!({
        "pass": verdict["pass"] == json!(true) && ids["pass"] == json!(true),
        "panels_hidden": panels(&hidden).len(),
        "panels_shown": verdict,
        "ids": ids,
        "agent_windows": windows,
    }))
}

/// Remove both markup groups and wait until no panel from 2^30 and no drawing is
/// left. Every error is recorded; a markup group that is gone already is not one.
fn cleanup(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    api.keep();
    let errors: Vec<String> = [FRONT, LOW]
        .iter()
        .filter_map(|n| remove_markup_group(api, c, n).err())
        .collect();
    let mut last: Option<Vec<Panel>> = None;
    let waited = wait(run, api, APPLY_LIMIT, "no panel left", |api| {
        let seen = read_panels(run, "drawing-layers-clean")?;
        let now = panels(&seen);
        let empty = now.is_empty();
        last = Some(now);
        Ok((empty && drawings(api, c)?.is_empty()).then_some(seen))
    })
    .map_err(|e| panel_wait_error("after cleanup", 0, last.as_deref(), &e));
    Ok(json!({
        "pass": waited.is_ok(),
        "remove_errors": errors,
        "read": waited.as_ref().ok(),
        "error": waited.err(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DCS gives scripted markup an ID from here (2^30).
    const SCRIPT_ID: u64 = 1 << 30;

    fn panel(idx: u64, east: f64) -> Panel {
        Panel {
            idx,
            east: Some(east),
        }
    }

    /// The 53 panels, the low ones first or last.
    fn fifty_three(low_first: bool) -> Vec<Panel> {
        let low_east = BASE.1 + LOW_EAST_M;
        let front_east = BASE.1 + 100.0;
        let (low_from, front_from) = if low_first { (0, 3) } else { (50, 0) };
        let mut p: Vec<Panel> = (0..3)
            .map(|i| panel(SCRIPT_ID + low_from + i, low_east))
            .collect();
        p.extend((0..50).map(|i| panel(SCRIPT_ID + front_from + i, front_east)));
        p.sort_by_key(|q| q.idx);
        p
    }

    #[test]
    fn a_panel_wait_error_names_phase_wanted_and_last_read() {
        let p: Vec<Panel> = (0..53).map(|i| panel(SCRIPT_ID + i, 0.0)).collect();
        let e = panel_wait_error("after show", 53, Some(&p[..50]), "not within 300s");
        assert!(e.contains("after show"), "{e}");
        assert!(e.contains("wanted 53"), "{e}");
        assert!(e.contains("last read 50 panels"), "{e}");
        assert!(e.contains("1073741824, 1073741825"), "{e}");
        assert!(e.contains("and 40 more"), "{e}");
        assert!(!e.contains("1073741834"), "{e}");
        let none = panel_wait_error("after hide", 3, None, "not within 300s");
        assert!(
            none.contains("after hide") && none.contains("no read"),
            "{none}"
        );
        let few = panel_wait_error("after hide", 3, Some(&p[..2]), "x");
        assert!(few.contains("2 panels [") && !few.contains("more"), "{few}");
    }

    #[test]
    fn low_panels_before_front_panels_pass() {
        let v = order_verdict(&fifty_three(true), 53, 3, false);
        assert_eq!(v["pass"], json!(true), "{v}");
        assert_eq!(v["in_add_order"], json!(true));
    }

    #[test]
    fn the_burst_adds_the_front_panels_before_the_low_ones() {
        let v = order_verdict(&fifty_three(false), 53, 3, true);
        assert_eq!(v["pass"], json!(true), "{v}");
        let v = order_verdict(&fifty_three(true), 53, 3, true);
        assert_eq!(v["pass"], json!(false), "{v}");
        assert_eq!(v["in_add_order"], json!(false));
    }

    #[test]
    fn a_low_panel_after_a_front_panel_fails() {
        let v = order_verdict(&fifty_three(false), 53, 3, false);
        assert_eq!(v["pass"], json!(false), "{v}");
        assert_eq!(v["in_add_order"], json!(false));
    }

    #[test]
    fn a_wrong_count_fails() {
        let mut p = fifty_three(true);
        p.pop();
        assert_eq!(order_verdict(&p, 53, 3, false)["pass"], json!(false));
    }

    #[test]
    fn panels_read_as_an_array_or_an_empty_table() {
        let seen = json!({ "panels": [{ "idx": 1073741825u64, "x": 1.0, "z": 2.0 }, { "idx": 1073741826u64 }] });
        let p = panels(&seen);
        assert_eq!(idxs(&p), vec![1073741825, 1073741826]);
        assert_eq!(p[1].east, None);
        assert!(panels(&json!({ "panels": {} })).is_empty());
    }

    fn item(label: &str, markup_group: &str, spec: u64, applied: u64) -> Item {
        Item {
            id: 1,
            dcs_id: 0,
            label: label.into(),
            markup_group: markup_group.into(),
            spec,
            applied,
            error: String::new(),
        }
    }

    #[test]
    fn applied_means_every_applied_revision_equals_its_spec_revision() {
        let a = [item("a", FRONT, 2, 2), item("b", FRONT, 1, 1)];
        let refs: Vec<&Item> = a.iter().collect();
        assert!(applied_all(&refs, 2));
        assert!(!applied_all(&refs, 3));
        let b = [item("a", FRONT, 2, 1)];
        assert!(!applied_all(&b.iter().collect::<Vec<_>>(), 1));
        let c = [item("a", FRONT, 1, 0)];
        assert!(!applied_all(&c.iter().collect::<Vec<_>>(), 1));
        assert!(hidden_all(&c.iter().collect::<Vec<_>>(), 1));
        assert!(!hidden_all(&a.iter().collect::<Vec<_>>(), 2));
    }

    #[test]
    fn windows_sum_and_the_longest_frame_is_the_maximum() {
        let w = |frames: f64, over: f64, max: f64| json!({ "msg": "sampling", "data": { "frames": frames, "frames_over_250": over, "frame_max_us": max, "window_s": 10.0 } });
        let s = sum_windows(&[w(100.0, 1.0, 300.0), w(50.0, 2.0, 900.0)]);
        assert_eq!(s["frames"], json!(150.0));
        assert_eq!(s["frames_over_250"], json!(3.0));
        assert_eq!(s["frame_max_us"], json!(900.0));
        assert_eq!(s["window_s"], json!(20.0));
        assert_eq!(s["windows"], json!(2));
    }

    #[test]
    fn a_freeform_holds_its_points_then_color_fill_line_read_only_and_message() {
        let a = freeform(0, RED).unwrap().values;
        assert_eq!(a.len(), POINTS + 5);
        let r = two_point(0).unwrap().values;
        assert_eq!(r.len(), 2 + 5);
        assert_eq!(front_items().len(), FRONT_ITEMS);
        // Every front item stands west of the split, every low item east.
        let last = center(FRONT_ITEMS - 1);
        assert!(last.1 + RADIUS_M < BASE.1 + SPLIT_EAST_M);
        assert!(BASE.1 + LOW_EAST_M > BASE.1 + SPLIT_EAST_M);
    }

    fn ev(dcs_id: u32, mark_id: u64) -> MarkEvent {
        MarkEvent {
            dcs_id,
            mark_id: Some(mark_id),
            platform_mark: mark_id >= SCRIPT_ID,
        }
    }

    fn burst_adds() -> Vec<MarkEvent> {
        (0..53).map(|i| ev(25, SCRIPT_ID + i)).collect()
    }

    #[test]
    fn an_in_place_change_with_no_mark_event_passes() {
        let v = in_place_events(&burst_adds(), &[], 53);
        assert_eq!(v["pass"], json!(true), "{v}");
    }

    #[test]
    fn a_remove_or_add_of_a_scripted_id_during_the_change_fails() {
        for during in [
            vec![ev(27, SCRIPT_ID + 7)],
            vec![ev(27, SCRIPT_ID + 7), ev(25, SCRIPT_ID + 7)],
            vec![ev(25, SCRIPT_ID + 7)],
        ] {
            let v = in_place_events(&burst_adds(), &during, 53);
            assert_eq!(v["pass"], json!(false), "{v}");
        }
    }

    #[test]
    fn a_player_mark_during_the_change_is_not_the_platforms() {
        let v = in_place_events(&burst_adds(), &[ev(25, 12)], 53);
        assert_eq!(v["pass"], json!(true), "{v}");
    }

    #[test]
    fn a_log_that_never_showed_the_burst_proves_nothing() {
        let v = in_place_events(&[], &[], 53);
        assert_eq!(v["pass"], json!(false), "{v}");
    }

    /// Before IDs, hidden items, shown items, after IDs, events.
    type Show = (Vec<u64>, Vec<Item>, Vec<Item>, Vec<u64>, Vec<MarkEvent>);

    fn listed(dcs_id: u64) -> Item {
        Item {
            dcs_id,
            ..item("x", FRONT, 1, 1)
        }
    }

    /// A hide and show that behave: 3 low panels kept, 50 front ones before
    /// (below the low ones) and 50 new ones above everything.
    fn clean_show() -> Show {
        let before: Vec<u64> = (0..53).map(|i| SCRIPT_ID + i).collect();
        let hidden = (0..50).map(|_| listed(0)).collect();
        let new: Vec<u64> = (53..103).map(|i| SCRIPT_ID + i).collect();
        let mut after: Vec<u64> = before[50..].to_vec();
        after.extend(&new);
        let shown = after.iter().map(|id| listed(*id)).collect();
        let events = new.iter().map(|id| ev(25, *id)).collect();
        (before, hidden, shown, after, events)
    }

    fn verdict(c: &Show) -> Value {
        id_verdict(&c.0, &c.1, &c.2, &c.3, &c.4, 50)
    }

    #[test]
    fn a_hide_and_show_with_new_listed_platform_ids_pass() {
        let c = clean_show();
        assert_eq!(verdict(&c)["pass"], json!(true), "{}", verdict(&c));
    }

    #[test]
    fn a_hidden_item_that_lists_an_id_fails() {
        let mut c = clean_show();
        c.1[3].dcs_id = SCRIPT_ID + 3;
        assert_eq!(verdict(&c)["hidden_dcs_id_0"], json!(false));
        assert_eq!(verdict(&c)["pass"], json!(false));
    }

    #[test]
    fn a_show_that_reuses_an_old_id_fails() {
        let mut c = clean_show();
        // One front panel comes back under an ID from before the hide.
        let last = c.3.len() - 1;
        c.3[last] = SCRIPT_ID + 10;
        c.2[last].dcs_id = SCRIPT_ID + 10;
        let v = verdict(&c);
        assert_eq!(v["pass"], json!(false), "{v}");
    }

    #[test]
    fn panel_ids_that_differ_from_the_listed_ones_fail() {
        let mut c = clean_show();
        c.2[4].dcs_id += 1000;
        assert_eq!(verdict(&c)["panel_ids_equal_listed"], json!(false));
        assert_eq!(verdict(&c)["pass"], json!(false));
    }

    #[test]
    fn a_platform_event_without_platform_mark_fails() {
        let mut c = clean_show();
        c.4[0].platform_mark = false;
        let v = verdict(&c);
        assert_eq!(v["pass"], json!(false), "{v}");
        assert_eq!(
            v["scripted_ids_without_platform_mark"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn a_show_whose_adds_never_reach_the_log_proves_nothing() {
        let mut c = clean_show();
        c.4.truncate(10);
        assert_eq!(verdict(&c)["pass"], json!(false));
    }

    #[test]
    fn the_panel_snippet_reads_the_id_under_either_name() {
        assert!(PANELS.contains("p.idx or p.id"));
        assert!(PANELS.contains("1073741824"));
        assert_eq!(SCRIPT_ID, 1073741824);
    }
}
