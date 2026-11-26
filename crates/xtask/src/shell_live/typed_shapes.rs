//! `sidecar <name> --part typed-shapes`: typed drawing shapes (spec/drawing.golden,
//! spec/world.md) against a running DCS.
//!
//! The run makes a campaign on Caucasus, then:
//!
//! 1. `draw`: `ReplaceMarkupGroup` `np-typed` with 12 typed drawings: a dashed
//!    line, a polygon, an arrow, a text box, a skewed quad, a freeform, a circle,
//!    two rectangles at heading 0 and 30 degrees, an oval and two rounded
//!    rectangles at 30 degrees. The run waits until every item's applied spec
//!    revision equals its spec revision, then reads `world.getMarkPanels`:
//!    12 panels with an ID from 2^30, equal to the `dcs_id`s `ListWorld` gives.
//!    The agent's sampling windows over the burst are summed.
//! 2. `radius`: `PutDrawing` of the circle by label with only the radius
//!    changed. The change is applied in place: the DCS ID and the panel IDs
//!    stay, and no mark event for an ID from 2^30 arrives.
//! 3. `style`: the same check for a color and fill change of two shapes and a
//!    font size change of the text box.
//! 4. `geometry`: the oval's heading changes. It is added again under a new
//!    DCS ID above every other item's, and the panel count stays 12.
//! 5. `cleanup`: `RemoveMarkupGroup`; no panel from 2^30 remains. Cleanup runs
//!    also when a step failed.
//!
//! The run resumes the mission through the shared `wait` and puts the pause
//! state it found back.

use std::time::Instant;

use super::drawing_layers::{
    APPLY_LIMIT, Item, MarkEvent, SCRIPT_ID_BASE, all, burst, drawings, idxs, in_place_events,
    map_point, mark, mark_events, markup_group, panel_wait_error, panels, put_drawing,
    put_markup_group, read_panels, recorded, remove_markup_group, replace_markup_group, step,
    wait_panels,
};
use super::static_replace::{PAUSED_LUA, restore_pause};
use super::*;

const THEATER: &str = "Caucasus";
const GROUP: &str = "np-typed";
const ITEMS: usize = 12;
/// The heading of the turned items: 30 degrees, in radians as the API takes it.
const TURN: f64 = std::f64::consts::PI / 6.0;
/// North and east of the first item, meters. The area stands 40 km east of the
/// drawing-layers part's.
const BASE: (f64, f64) = (-289_000.0, 721_000.0);
/// The items stand in a grid of 4 columns, 6 km apart.
const SPACING_M: f64 = 6_000.0;
const COLUMNS: usize = 4;
const RED: [f64; 4] = [1.0, 0.0, 0.0, 1.0];
const GREEN: [f64; 4] = [0.0, 1.0, 0.0, 1.0];
const FILL_RED: [f64; 4] = [1.0, 0.0, 0.0, 0.2];
const FILL_BLUE: [f64; 4] = [0.0, 0.0, 1.0, 0.4];
/// How long the events of a change have to reach the log.
const EVENT_WAIT_S: f64 = 3.0;

/// The labels, in the order the items are added.
const LABELS: [&str; ITEMS] = [
    "L", "PG", "AR", "TB", "QK", "FF", "CI", "R0", "R30", "OV", "RR", "RC",
];

// ---- specs -----------------------------------------------------------------

type Kind = pb::drawing_shape::Kind;

fn color(c: [f64; 4]) -> Option<pb::Color> {
    Some(pb::Color {
        red: c[0],
        green: c[1],
        blue: c[2],
        alpha: c[3],
    })
}

/// A drawing on the theater with the outline `RED` and the fill at alpha 0.2.
fn shape(kind: Kind, line: pb::LineType) -> pb::DrawingShape {
    pb::DrawingShape {
        theater: THEATER.into(),
        color: color(RED),
        fill: color(FILL_RED),
        line_type: line as i32,
        read_only: true,
        kind: Some(kind),
        ..Default::default()
    }
}

/// Where item `k` stands, north and east in meters.
fn center(k: usize) -> (f64, f64) {
    (
        BASE.0 + (k / COLUMNS) as f64 * SPACING_M,
        BASE.1 + (k % COLUMNS) as f64 * SPACING_M,
    )
}

fn at(k: usize, dn: f64, de: f64) -> pb::MapPoint {
    let (n, e) = center(k);
    map_point(n + dn, e + de)
}

fn ring(k: usize, count: usize, radius: f64) -> Vec<pb::MapPoint> {
    (0..count)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / count as f64;
            at(k, radius * a.cos(), radius * a.sin())
        })
        .collect()
}

/// The spec of the item labeled `label`, which is item `k`.
fn spec(k: usize, label: &str) -> pb::DrawingShape {
    let c = Some(map_point(center(k).0, center(k).1));
    let solid = pb::LineType::Solid;
    match label {
        "L" => shape(
            Kind::Line(pb::drawing_shape::Line {
                from: Some(at(k, 0.0, -900.0)),
                to: Some(at(k, 300.0, 900.0)),
            }),
            pb::LineType::Dashed,
        ),
        "PG" => shape(
            Kind::Polygon(pb::drawing_shape::Polygon {
                points: ring(k, 5, 800.0),
            }),
            solid,
        ),
        "AR" => shape(
            Kind::Arrow(pb::drawing_shape::Arrow {
                from: Some(at(k, -500.0, -800.0)),
                to: Some(at(k, 500.0, 800.0)),
            }),
            solid,
        ),
        "TB" => pb::DrawingShape {
            text: "typed".into(),
            ..shape(
                Kind::TextBox(pb::drawing_shape::TextBox {
                    at: c,
                    font_size: 14,
                }),
                solid,
            )
        },
        "QK" => shape(
            Kind::Quad(pb::drawing_shape::Quad {
                corners: vec![
                    at(k, -600.0, -900.0),
                    at(k, -400.0, 800.0),
                    at(k, 700.0, 500.0),
                    at(k, 500.0, -700.0),
                ],
            }),
            solid,
        ),
        "FF" => shape(
            Kind::Freeform(pb::drawing_shape::Freeform {
                points: ring(k, 12, 700.0),
            }),
            solid,
        ),
        "CI" => circle(k, 1_500.0),
        "R0" => shape(rectangle(c, 0.0), solid),
        "R30" => shape(rectangle(c, TURN), solid),
        "OV" => oval(k, TURN),
        "RR" => shape(rounded(c, 400.0), solid),
        "RC" => shape(rounded(c, 750.0), solid),
        other => unreachable!("no item labeled {other}"),
    }
}

fn circle(k: usize, radius: f64) -> pb::DrawingShape {
    let (n, e) = center(k);
    shape(
        Kind::Circle(pb::drawing_shape::Circle {
            center: Some(map_point(n, e)),
            radius,
        }),
        pb::LineType::Solid,
    )
}

fn oval(k: usize, heading: f64) -> pb::DrawingShape {
    let (n, e) = center(k);
    shape(
        Kind::Oval(pb::drawing_shape::Oval {
            center: Some(map_point(n, e)),
            half_length: 1_500.0,
            half_width: 750.0,
            heading,
        }),
        pb::LineType::Solid,
    )
}

fn rectangle(center: Option<pb::MapPoint>, heading: f64) -> Kind {
    Kind::Rectangle(pb::drawing_shape::Rectangle {
        center,
        length: 3_000.0,
        width: 1_500.0,
        heading,
    })
}

fn rounded(center: Option<pb::MapPoint>, corner_radius: f64) -> Kind {
    Kind::RoundedRectangle(pb::drawing_shape::RoundedRectangle {
        center,
        length: 3_000.0,
        width: 1_500.0,
        corner_radius,
        heading: TURN,
    })
}

/// The 12 items, in the order they are added.
fn specs() -> Vec<(&'static str, pb::DrawingShape)> {
    LABELS
        .iter()
        .enumerate()
        .map(|(k, l)| (*l, spec(k, l)))
        .collect()
}

fn index(label: &str) -> usize {
    LABELS
        .iter()
        .position(|l| *l == label)
        .unwrap_or_else(|| unreachable!("no item labeled {label}"))
}

fn items() -> Vec<pb::MarkupGroupItem> {
    specs()
        .into_iter()
        .map(|(label, s)| pb::MarkupGroupItem {
            label: label.into(),
            audience: all(),
            expire_after_s: None,
            kind: Some(pb::markup_group_item::Kind::Drawing(
                pb::MarkupGroupDrawing {
                    typed_shape: Some(s),
                    ..Default::default()
                },
            )),
        })
        .collect()
}

// ---- pure checks -----------------------------------------------------------

fn find<'a>(items: &'a [Item], label: &str) -> Option<&'a Item> {
    items.iter().find(|i| i.label == label)
}

/// Whether the group holds the 12 items and each one's applied spec revision
/// equals its spec revision.
fn applied_all(items: &[Item]) -> bool {
    let ours: Vec<&Item> = items.iter().filter(|i| i.markup_group == GROUP).collect();
    ours.len() == ITEMS && ours.iter().all(|i| i.spec > 0 && i.applied == i.spec)
}

/// The verdict on a read of the panels: `count` panels, each from 2^30, and
/// their IDs equal the `dcs_id`s `ListWorld` lists for the items.
fn panels_verdict(panel_ids: &[u64], listed: &[Item], count: usize) -> Value {
    let mut ids = panel_ids.to_vec();
    let mut dcs: Vec<u64> = listed
        .iter()
        .map(|i| i.dcs_id)
        .filter(|id| *id != 0)
        .collect();
    ids.sort_unstable();
    dcs.sort_unstable();
    let from_base = ids.iter().all(|id| *id >= SCRIPT_ID_BASE);
    let equal = ids == dcs;
    json!({
        "pass": ids.len() == count && from_base && equal,
        "count": ids.len(),
        "wanted": count,
        "from_2_30": from_base,
        "panel_ids_equal_listed": equal,
        "panel_ids": ids,
        "listed_dcs_ids": dcs,
    })
}

/// The verdict on a change applied in place. Each of `labels` keeps its entity
/// and its DCS ID, and its spec revision rises and is applied. The panel IDs
/// are the same before and after, `count` panels stand, and `events` (the
/// verdict of [`in_place_events`]) shows no mark event for an ID from 2^30.
fn in_place_verdict(
    labels: &[&str],
    before: &[Item],
    after: &[Item],
    panels_before: &[u64],
    panels_after: &[u64],
    events: &Value,
    count: usize,
) -> Value {
    let rows: Vec<Value> = labels
        .iter()
        .map(|l| {
            let (b, a) = (find(before, l), find(after, l));
            let same_dcs_id =
                matches!((b, a), (Some(b), Some(a)) if b.dcs_id != 0 && b.dcs_id == a.dcs_id);
            let same_entity = matches!((b, a), (Some(b), Some(a)) if b.id == a.id);
            let revised =
                matches!((b, a), (Some(b), Some(a)) if a.spec > b.spec && a.applied == a.spec);
            json!({
                "label": l,
                "pass": same_dcs_id && same_entity && revised,
                "same_dcs_id": same_dcs_id,
                "same_entity": same_entity,
                "revised_and_applied": revised,
                "dcs_id_before": b.map(|i| i.dcs_id),
                "dcs_id_after": a.map(|i| i.dcs_id),
            })
        })
        .collect();
    let same_panels = panels_before == panels_after;
    json!({
        "pass": rows.iter().all(|r| r["pass"] == json!(true))
            && same_panels
            && panels_after.len() == count
            && events["pass"] == json!(true),
        "items": rows,
        "panel_ids_unchanged": same_panels,
        "panels_after": panels_after.len(),
        "events": events,
    })
}

/// The verdict on a geometry change: the item is added again under a new DCS
/// ID above every other item's, its panel stands, and `count` panels remain.
fn geometry_verdict(
    label: &str,
    before: &[Item],
    after: &[Item],
    panel_ids: &[u64],
    count: usize,
) -> Value {
    let old = find(before, label).map_or(0, |i| i.dcs_id);
    let new = find(after, label).map_or(0, |i| i.dcs_id);
    let others = after
        .iter()
        .filter(|i| i.label != label)
        .map(|i| i.dcs_id)
        .max()
        .unwrap_or(0);
    let above = new != 0 && new > others;
    let changed = new != old;
    let painted = panel_ids.contains(&new);
    json!({
        "pass": above && changed && painted && panel_ids.len() == count,
        "dcs_id_before": old,
        "dcs_id_after": new,
        "top_of_the_others": others,
        "above_every_other": above,
        "dcs_id_changed": changed,
        "panel_shown": painted,
        "panels": panel_ids.len(),
        "wanted": count,
    })
}

// ---- the run ---------------------------------------------------------------

/// Run the part; the result goes under `typed_shapes`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    results: &mut Value,
) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);
    let found_paused = run.eval("hook", "typed-shapes-paused", PAUSED_LUA, 60)?;
    println!("== typed-shapes: a campaign on {THEATER}");
    let c = api.create("typed-shapes-live", &[THEATER], true)?;
    loaded(run, &mut api, THEATER, generation)?;
    api.renew()?;

    let mut v = json!({ "steps": {} });
    let ran = steps(run, &mut api, c, &mut v);
    println!("== typed-shapes: remove the markup group");
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
    results["typed_shapes"] = v;
    Ok(())
}

fn steps(run: &dyn Live, api: &mut Api, c: u64, rec: &mut Value) -> Result<(), String> {
    println!("== typed-shapes: 12 typed drawings");
    step(rec, "draw", draw(run, api, c))?;
    println!("== typed-shapes: the circle's radius changed");
    step(
        rec,
        "radius",
        change(
            run,
            api,
            c,
            "radius",
            &[("CI", circle(index("CI"), 2_500.0))],
        ),
    )?;
    println!("== typed-shapes: color, fill and font size changed");
    step(rec, "style", style(run, api, c))?;
    println!("== typed-shapes: the oval's heading changed");
    step(rec, "geometry", geometry(run, api, c))?;
    Ok(())
}

/// Wait until the 12 items are applied, the time it took.
fn wait_applied(run: &dyn Live, api: &mut Api, c: u64) -> Result<f64, String> {
    let t = Instant::now();
    wait(run, api, APPLY_LIMIT, "every item applied", |api| {
        Ok(applied_all(&drawings(api, c)?).then_some(()))
    })?;
    Ok(t.elapsed().as_secs_f64())
}

fn panel_ids(run: &dyn Live, stem: &str) -> Result<Vec<u64>, String> {
    Ok(idxs(&panels(&read_panels(run, stem)?)))
}

fn draw(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    let start = mark(run);
    let t = Instant::now();
    put_markup_group(api, c, markup_group(GROUP, true))?;
    let ids = replace_markup_group(api, c, GROUP, items())?;
    let applied_s = wait_applied(run, api, c)
        .map(|_| t.elapsed().as_secs_f64())
        .map_err(|e| format!("{e} (items sent: {})", ids.len()))?;
    let seen = wait_panels(run, api, "typed-shapes-panels", "after the draw", ITEMS)?;
    let listed = drawings(api, c)?;
    let verdict = panels_verdict(&idxs(&panels(&seen)), &listed, ITEMS);
    let windows = burst(run, api, start, applied_s, ITEMS)?;
    let errors: Vec<&str> = listed
        .iter()
        .map(|i| i.error.as_str())
        .filter(|e| !e.is_empty())
        .collect();
    Ok(json!({
        "pass": verdict["pass"] == json!(true) && ids.len() == ITEMS && errors.is_empty(),
        "replace_ids": ids.len(),
        "item_errors": errors,
        "panels": verdict,
        "total_panels": seen["total"],
        "agent_windows": windows,
    }))
}

/// Put each of `edits` by label, wait for the new revisions to be applied and
/// check the change was made in place.
fn change(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    what: &str,
    edits: &[(&str, pb::DrawingShape)],
) -> Result<Value, String> {
    let before = drawings(api, c)?;
    let panels_before = panel_ids(run, "typed-shapes-before")?;
    let (last, earlier) = mark_events(api, c, 0)?;
    for (label, s) in edits {
        put_drawing(api, c, what, label, GROUP, s.clone())?;
    }
    wait_revised(run, api, c, &before, edits)?;
    let after = settle(run, api, c)?;
    let panels_after = panel_ids(run, "typed-shapes-after")?;
    let (_, during): (u64, Vec<MarkEvent>) = mark_events(api, c, last)?;
    let events = in_place_events(&earlier, &during, ITEMS);
    let labels: Vec<&str> = edits.iter().map(|(l, _)| *l).collect();
    Ok(in_place_verdict(
        &labels,
        &before,
        &after,
        &panels_before,
        &panels_after,
        &events,
        ITEMS,
    ))
}

/// Wait until each edited item's spec revision is above its revision in
/// `before` and applied.
fn wait_revised(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    before: &[Item],
    edits: &[(&str, pb::DrawingShape)],
) -> Result<(), String> {
    wait(run, api, APPLY_LIMIT, "the revisions applied", |api| {
        let now = drawings(api, c)?;
        let done = edits.iter().all(|(label, _)| {
            matches!((find(before, label), find(&now, label)),
                (Some(b), Some(a)) if a.spec > b.spec && a.applied == a.spec)
        });
        Ok(done.then_some(()))
    })
}

/// Let the events of a change reach the log, then list the items.
fn settle(run: &dyn Live, api: &mut Api, c: u64) -> Result<Vec<Item>, String> {
    run.hold(EVENT_WAIT_S)?;
    api.keep();
    drawings(api, c)
}

fn style(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    let restyle = |label: &str| {
        let mut s = spec(index(label), label);
        s.color = color(GREEN);
        s.fill = color(FILL_BLUE);
        s
    };
    let mut text = spec(index("TB"), "TB");
    if let Some(Kind::TextBox(t)) = &mut text.kind {
        t.font_size = 20;
    }
    change(
        run,
        api,
        c,
        "style",
        &[("R30", restyle("R30")), ("OV", restyle("OV")), ("TB", text)],
    )
}

fn geometry(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    let before = drawings(api, c)?;
    let mut turned = oval(index("OV"), 2.0 * TURN);
    // The oval keeps the color and fill the style step gave it.
    turned.color = color(GREEN);
    turned.fill = color(FILL_BLUE);
    let edits = [("OV", turned)];
    put_drawing(api, c, "geometry", "OV", GROUP, edits[0].1.clone())?;
    wait_revised(run, api, c, &before, &edits)?;
    let after = settle(run, api, c)?;
    let seen = wait_panels(
        run,
        api,
        "typed-shapes-geometry",
        "after the heading change",
        ITEMS,
    )?;
    Ok(geometry_verdict(
        "OV",
        &before,
        &after,
        &idxs(&panels(&seen)),
        ITEMS,
    ))
}

/// Remove the markup group and wait until no panel from 2^30 and no drawing is
/// left. Every error is recorded; a markup group that is gone already is not one.
fn cleanup(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    api.keep();
    let errors: Vec<String> = remove_markup_group(api, c, GROUP)
        .err()
        .into_iter()
        .collect();
    let mut last = None;
    let waited = wait(run, api, APPLY_LIMIT, "no panel left", |api| {
        let seen = read_panels(run, "typed-shapes-clean")?;
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

    fn item(label: &str, id: u64, dcs_id: u64, spec: u64, applied: u64) -> Item {
        Item {
            id,
            dcs_id,
            label: label.into(),
            markup_group: GROUP.into(),
            spec,
            applied,
            error: String::new(),
        }
    }

    /// The 12 items, entity `k + 1`, DCS ID `2^30 + k`, revision 1 applied.
    fn listed() -> Vec<Item> {
        LABELS
            .iter()
            .enumerate()
            .map(|(k, l)| item(l, k as u64 + 1, SCRIPT_ID_BASE + k as u64, 1, 1))
            .collect()
    }

    #[test]
    fn there_are_twelve_items_with_unique_labels() {
        let it = items();
        assert_eq!(it.len(), ITEMS);
        let mut labels: Vec<&str> = it.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, LABELS);
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), ITEMS);
        assert!(it.iter().all(|i| matches!(
            &i.kind,
            Some(pb::markup_group_item::Kind::Drawing(d)) if d.typed_shape.is_some()
        )));
    }

    #[test]
    fn the_kinds_are_the_ones_listed() {
        let kinds: Vec<&'static str> = specs()
            .iter()
            .map(|(_, s)| match s.kind {
                Some(Kind::Line(_)) => "line",
                Some(Kind::Polygon(_)) => "polygon",
                Some(Kind::Arrow(_)) => "arrow",
                Some(Kind::TextBox(_)) => "text_box",
                Some(Kind::Quad(_)) => "quad",
                Some(Kind::Freeform(_)) => "freeform",
                Some(Kind::Circle(_)) => "circle",
                Some(Kind::Rectangle(_)) => "rectangle",
                Some(Kind::Oval(_)) => "oval",
                Some(Kind::RoundedRectangle(_)) => "rounded",
                None => "none",
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "line",
                "polygon",
                "arrow",
                "text_box",
                "quad",
                "freeform",
                "circle",
                "rectangle",
                "rectangle",
                "oval",
                "rounded",
                "rounded"
            ]
        );
    }

    #[test]
    fn the_shapes_have_the_listed_geometry() {
        let s = specs();
        let get = |l: &str| s[index(l)].1.clone();
        assert_eq!(get("L").line_type, pb::LineType::Dashed as i32);
        assert!(matches!(get("PG").kind, Some(Kind::Polygon(p)) if p.points.len() == 5));
        assert!(matches!(get("FF").kind, Some(Kind::Freeform(f)) if f.points.len() == 12));
        assert!(matches!(get("TB").kind, Some(Kind::TextBox(t)) if t.font_size == 14));
        assert!(matches!(get("CI").kind, Some(Kind::Circle(c)) if c.radius == 1_500.0));
        assert!(
            matches!(get("R0").kind, Some(Kind::Rectangle(r)) if r.heading == 0.0 && r.length == 3_000.0 && r.width == 1_500.0)
        );
        assert!(matches!(get("R30").kind, Some(Kind::Rectangle(r)) if r.heading == TURN));
        assert!(
            (TURN.to_degrees() - 30.0).abs() < 1e-9,
            "30 degrees, in radians"
        );
        assert!(
            matches!(get("OV").kind, Some(Kind::Oval(o)) if o.half_length == 1_500.0 && o.half_width == 750.0 && o.heading == TURN)
        );
        assert!(
            matches!(get("RR").kind, Some(Kind::RoundedRectangle(r)) if r.corner_radius == 400.0 && r.heading == TURN)
        );
        assert!(
            matches!(get("RC").kind, Some(Kind::RoundedRectangle(r)) if r.corner_radius == 750.0)
        );
        assert!(specs().iter().all(|(_, s)| s.theater == THEATER));
    }

    #[test]
    fn the_quad_has_no_right_angle() {
        let Some(Kind::Quad(q)) = spec(index("QK"), "QK").kind else {
            panic!("not a quad");
        };
        assert_eq!(q.corners.len(), 4);
        for i in 0..4 {
            let (a, b, c) = (
                &q.corners[i],
                &q.corners[(i + 1) % 4],
                &q.corners[(i + 2) % 4],
            );
            let dot =
                (a.north - b.north) * (c.north - b.north) + (a.east - b.east) * (c.east - b.east);
            let len = ((a.north - b.north).hypot(a.east - b.east))
                * ((c.north - b.north).hypot(c.east - b.east));
            assert!((dot / len).abs() > 0.05, "corner {i} is square");
        }
    }

    #[test]
    fn the_items_stand_6_km_apart() {
        let spots: Vec<(f64, f64)> = (0..ITEMS).map(center).collect();
        for (i, a) in spots.iter().enumerate() {
            for b in &spots[i + 1..] {
                assert!((a.0 - b.0).hypot(a.1 - b.1) >= SPACING_M - 1.0);
            }
        }
    }

    #[test]
    fn applied_means_twelve_items_at_their_revision() {
        let mut l = listed();
        assert!(applied_all(&l));
        l[3].applied = 0;
        assert!(!applied_all(&l));
        l[3].applied = 1;
        l.pop();
        assert!(!applied_all(&l));
    }

    #[test]
    fn panels_equal_to_the_listed_ids_pass() {
        let l = listed();
        let mut ids: Vec<u64> = l.iter().map(|i| i.dcs_id).collect();
        ids.reverse();
        assert_eq!(panels_verdict(&ids, &l, ITEMS)["pass"], json!(true));
        let mut few = ids.clone();
        few.pop();
        assert_eq!(panels_verdict(&few, &l, ITEMS)["pass"], json!(false));
        ids[0] += 100;
        assert_eq!(panels_verdict(&ids, &l, ITEMS)["pass"], json!(false));
        let low: Vec<u64> = (0..ITEMS as u64).collect();
        let mut zero = listed();
        zero.iter_mut().zip(&low).for_each(|(i, id)| i.dcs_id = *id);
        assert_eq!(panels_verdict(&low, &zero, ITEMS)["pass"], json!(false));
    }

    fn quiet() -> Value {
        json!({ "pass": true })
    }

    fn revised(l: &[Item], labels: &[&str]) -> Vec<Item> {
        let mut after = l.to_vec();
        for i in after
            .iter_mut()
            .filter(|i| labels.contains(&i.label.as_str()))
        {
            i.spec += 1;
            i.applied = i.spec;
        }
        after
    }

    #[test]
    fn a_quiet_change_with_the_same_ids_passes() {
        let l = listed();
        let ids: Vec<u64> = l.iter().map(|i| i.dcs_id).collect();
        let v = in_place_verdict(
            &["CI"],
            &l,
            &revised(&l, &["CI"]),
            &ids,
            &ids,
            &quiet(),
            ITEMS,
        );
        assert_eq!(v["pass"], json!(true), "{v}");
    }

    #[test]
    fn a_new_dcs_id_a_loud_change_or_an_unapplied_revision_fails() {
        let l = listed();
        let ids: Vec<u64> = l.iter().map(|i| i.dcs_id).collect();
        let ok = revised(&l, &["CI"]);
        let loud = json!({ "pass": false });
        assert_eq!(
            in_place_verdict(&["CI"], &l, &ok, &ids, &ids, &loud, ITEMS)["pass"],
            json!(false)
        );
        let mut moved = ok.clone();
        moved[index("CI")].dcs_id += 50;
        assert_eq!(
            in_place_verdict(&["CI"], &l, &moved, &ids, &ids, &quiet(), ITEMS)["pass"],
            json!(false)
        );
        let mut other_panels = ids.clone();
        other_panels[0] += 1;
        assert_eq!(
            in_place_verdict(&["CI"], &l, &ok, &ids, &other_panels, &quiet(), ITEMS)["pass"],
            json!(false)
        );
        let mut stale = ok.clone();
        stale[index("CI")].applied = 1;
        assert_eq!(
            in_place_verdict(&["CI"], &l, &stale, &ids, &ids, &quiet(), ITEMS)["pass"],
            json!(false)
        );
        assert_eq!(
            in_place_verdict(&["CI"], &l, &l, &ids, &ids, &quiet(), ITEMS)["pass"],
            json!(false)
        );
    }

    #[test]
    fn a_turned_oval_needs_the_highest_id_and_twelve_panels() {
        let l = listed();
        let mut after = revised(&l, &["OV"]);
        let top = SCRIPT_ID_BASE + 100;
        after[index("OV")].dcs_id = top;
        let ids: Vec<u64> = after.iter().map(|i| i.dcs_id).collect();
        assert_eq!(
            geometry_verdict("OV", &l, &after, &ids, ITEMS)["pass"],
            json!(true)
        );
        let mut few = ids.clone();
        few.pop();
        assert_eq!(
            geometry_verdict("OV", &l, &after, &few, ITEMS)["pass"],
            json!(false)
        );
        // The same ID, or an ID below another item's, fails.
        assert_eq!(
            geometry_verdict("OV", &l, &l, &ids, ITEMS)["pass"],
            json!(false)
        );
        after[index("OV")].dcs_id = SCRIPT_ID_BASE + 5;
        let low: Vec<u64> = after.iter().map(|i| i.dcs_id).collect();
        assert_eq!(
            geometry_verdict("OV", &l, &after, &low, ITEMS)["pass"],
            json!(false)
        );
    }
}
