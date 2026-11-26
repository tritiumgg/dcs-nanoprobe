//! Labels and markup groups of marks and drawings through the reconciler and the
//! fake sim (spec/world.md, "Labels and markup groups"): the DCS IDs the sidecar
//! chooses and the order it sends them in, updates in place, hidden layers,
//! caps, expiry, and the removal of a mark the platform does not hold.

use std::collections::BTreeMap;

use nanoprobe_sidecar::campaigns::Campaigns;
use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::server::ServerStore;
use nanoprobe_sidecar::sim::wire::{lv, opcode};
use nanoprobe_sidecar::store::{
    CampaignRecord, CampaignState, Entry, Layer, LostPolicy, MarkerError, MarkerPut, MarkerSpec,
    Point, RemoveCause, Store, scope,
};
use nanoprobe_sim_fake::harness::{Op, THEATER, World, new_store_for};
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::result as rc;

fn world() -> World {
    let mut w = World::new(0, 0, LostPolicy::Report);
    w.settle().expect("settles");
    w
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().expect("tick");
    }
}

fn sent(w: &World, op: u16) -> Vec<u64> {
    w.store
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Command(c) if c.opcode == op => Some(c.entity.0),
            _ => None,
        })
        .collect()
}

/// A two-point shape 3 (a line): two points, color, fill, line type, read
/// only, message.
fn two_point(x: f64, color: f64) -> MarkerSpec {
    let point = |x: f64, z: f64| lv::obj(vec![("x", lv::n(x)), ("y", lv::n(0.0)), ("z", lv::n(z))]);
    let rgba = |r: f64| lv::list(vec![lv::n(r), lv::n(0.0), lv::n(0.0), lv::n(1.0)]);
    MarkerSpec::Drawing {
        shape: 3,
        args: lv::list(vec![
            point(x, 0.0),
            point(x + 100.0, 100.0),
            rgba(color),
            rgba(color),
            lv::n(1.0),
            Lv::Bool(true),
            Lv::String(b"".to_vec()),
        ]),
    }
}

fn put(spec: MarkerSpec, label: &str, layer: &str) -> MarkerPut {
    MarkerPut {
        id: EntityId(0),
        scope: scope::ALL,
        target: 0,
        spec,
        drawing: None,
        label: Some(label.into()),
        layer: Some(layer.into()),
        expire_after_s: None,
    }
}

fn layer(name: &str, visible: bool) -> Layer {
    Layer {
        name: name.into(),
        visible,
        expire_after_s: None,
        cap: None,
    }
}

#[test]
fn items_take_ids_in_creation_order_and_are_sent_again_in_it_after_a_load() {
    let mut w = world();
    w.store.put_layer(layer("top", true)).unwrap();
    let t = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "t", "top"))
        .unwrap();
    let n = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "n", ""))
        .unwrap();
    let m = w
        .store
        .put_marker_with(put(mark_spec("m"), "m", "top"))
        .unwrap();
    ticks(&mut w, 10);
    let dcs = |w: &World, e: EntityId| w.sim.drawing(e.0).expect("held").dcs_id;
    // Marks and drawings are sent together, by creation.
    assert!(dcs(&w, t.id) < dcs(&w, n.id));
    assert!(dcs(&w, n.id) < w.sim.mark(m.id.0).expect("held").dcs_id);
    assert_eq!(sent(&w, opcode::DRAWING_SET), vec![t.id.0, n.id.0]);

    w.apply(&Op::LoadMission).unwrap();
    assert!(w.sim.drawings().is_empty());
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert_eq!(sent(&w, opcode::DRAWING_SET)[2..], [t.id.0, n.id.0]);
    assert_eq!(dcs(&w, t.id), w.store.marker(t.id).unwrap().dcs_id);
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

/// A typed circle of `radius` at `north`.
fn circle(north: f64, radius: f64) -> MarkerPut {
    use nanoprobe_sidecar::store::drawing::{DrawingShape, LineType, MapPoint, ShapeKind};
    MarkerPut {
        drawing: Some(DrawingShape {
            theater: THEATER.into(),
            color: [0.0, 0.0, 1.0, 1.0],
            fill: [0.0, 0.0, 1.0, 0.6],
            line_type: LineType::Solid,
            text: String::new(),
            read_only: true,
            kind: ShapeKind::Circle {
                center: MapPoint::new(north, 0.0),
                radius,
            },
        }),
        ..put(two_point(0.0, 1.0), "c", "")
    }
}

#[test]
fn a_circle_is_set_as_one_and_its_radius_changes_in_place() {
    use nanoprobe_sidecar::store::drawing::shape;
    let mut w = world();
    let c = w.store.put_marker_with(circle(0.0, 1500.0)).unwrap();
    ticks(&mut w, 10);
    let held = w.sim.drawing(c.id.0).expect("drawn");
    assert_eq!(held.shape, shape::CIRCLE);
    let id0 = held.dcs_id;
    // The radius: one DRAWING_UPDATE, the ID kept.
    w.store.put_marker_with(circle(0.0, 2500.0)).unwrap();
    ticks(&mut w, 10);
    let held = w.sim.drawing(c.id.0).unwrap();
    assert_eq!((held.updates, held.rev, held.dcs_id), (1, 2, id0));
    assert_eq!(lv::items(&held.args)[1], &Lv::Number(2500.0));
    assert_eq!(sent(&w, opcode::DRAWING_UPDATE), vec![c.id.0]);
    // The center: a DRAWING_SET under a new ID.
    w.store.put_marker_with(circle(100.0, 2500.0)).unwrap();
    ticks(&mut w, 10);
    let held = w.sim.drawing(c.id.0).unwrap();
    assert!(held.dcs_id > id0, "a moved circle takes a new ID");
    assert_eq!(sent(&w, opcode::DRAWING_SET).len(), 2);
    assert_eq!(w.sim.ledger().drawing_layouts_refused, 0);
}

#[test]
fn a_style_change_is_applied_in_place_and_a_point_change_is_set_again_under_a_new_id() {
    let mut w = world();
    let d = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "d", ""))
        .unwrap();
    ticks(&mut w, 10);
    let id0 = w.sim.drawing(d.id.0).unwrap().dcs_id;
    // The color: one DRAWING_UPDATE, the ID kept.
    w.store
        .put_marker_with(put(two_point(0.0, 0.5), "d", ""))
        .unwrap();
    ticks(&mut w, 10);
    let held = w.sim.drawing(d.id.0).unwrap();
    assert_eq!((held.updates, held.rev, held.dcs_id), (1, 2, id0));
    assert_eq!(sent(&w, opcode::DRAWING_UPDATE), vec![d.id.0]);
    assert_eq!(sent(&w, opcode::DRAWING_SET).len(), 1);
    assert_eq!(w.rec.marker_applied(d.id), 2);
    // The points: a DRAWING_SET under a new ID, since DCS shows nothing for
    // an add under an ID it held or removed.
    w.store
        .put_marker_with(put(two_point(50.0, 0.5), "d", ""))
        .unwrap();
    ticks(&mut w, 10);
    let held = w.sim.drawing(d.id.0).unwrap();
    assert_eq!((held.updates, held.rev), (0, 3));
    assert!(held.dcs_id > id0, "a full set takes a new ID");
    assert_eq!(held.dcs_id, w.store.marker(d.id).unwrap().dcs_id);
    assert_eq!(sent(&w, opcode::DRAWING_SET).len(), 2);
    assert!(w.sim.ledger().ignored_adds.is_empty());
    // A load drops the drawing and the next steps set it again; a style
    // change after that is in place again.
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    w.store
        .put_marker_with(put(two_point(50.0, 0.2), "d", ""))
        .unwrap();
    ticks(&mut w, 10);
    assert_eq!(sent(&w, opcode::DRAWING_UPDATE).len(), 2);
    let held = w.sim.drawing(d.id.0).unwrap();
    assert_eq!(held.rev, 4);
    let MarkerSpec::Drawing { args, .. } = two_point(50.0, 0.2) else {
        unreachable!()
    };
    assert_eq!(held.args, args, "the fake holds the new spec's arguments");
}

#[test]
fn an_update_answered_not_bound_is_set_whole_and_reaches_the_spec_revision() {
    let mut w = world();
    let d = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "d", ""))
        .unwrap();
    ticks(&mut w, 10);
    // DCS loses the drawing; the sidecar does not know.
    w.sim.lose_drawing(d.id.0);
    let spec_rev = w
        .store
        .put_marker_with(put(two_point(0.0, 0.5), "d", ""))
        .unwrap()
        .rev;
    ticks(&mut w, 10);
    assert_eq!(
        sent(&w, opcode::DRAWING_UPDATE),
        vec![d.id.0],
        "tried in place"
    );
    assert_eq!(sent(&w, opcode::DRAWING_SET).len(), 2, "then set whole");
    let held = w.sim.drawing(d.id.0).expect("set again");
    assert_eq!((held.rev, held.updates), (spec_rev, 0));
    assert_eq!(w.rec.marker_applied(d.id), spec_rev);
}

#[test]
fn a_hidden_layer_leaves_dcs_and_comes_back_when_shown() {
    let mut w = world();
    let d = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "d", "sectors"))
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(d.id.0).is_some());
    w.store.put_layer(layer("sectors", false)).unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(d.id.0).is_none());
    assert_eq!(w.rec.marker_applied(d.id), 0, "a hidden item reads 0");
    assert!(!w.store.marker(d.id).unwrap().removed);
    // A change while hidden is kept and sent when shown.
    w.store
        .put_marker_with(put(two_point(9.0, 1.0), "d", "sectors"))
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(d.id.0).is_none());
    w.store.put_layer(layer("sectors", true)).unwrap();
    ticks(&mut w, 10);
    assert_eq!(w.sim.drawing(d.id.0).unwrap().rev, 2);
    // Removed after showing again: the removal is sent.
    w.store.remove_layer("sectors").unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(d.id.0).is_none());
    assert_eq!(sent(&w, opcode::DRAWING_REMOVE).len(), 2);
}

#[test]
fn a_hidden_layer_shown_with_no_spec_change_sets_the_drawing_at_its_revision() {
    let mut w = world();
    let d = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "d", "sectors"))
        .unwrap();
    ticks(&mut w, 10);
    let rev = w.sim.drawing(d.id.0).unwrap().rev;
    w.store.put_layer(layer("sectors", false)).unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(d.id.0).is_none());
    assert_eq!(w.rec.marker_applied(d.id), 0);
    // The same revision goes out again after the remove.
    w.store.put_layer(layer("sectors", true)).unwrap();
    ticks(&mut w, 10);
    assert_eq!(w.sim.drawing(d.id.0).expect("back").rev, rev);
    assert_eq!(w.rec.marker_applied(d.id), rev);
    assert_eq!(w.store.marker(d.id).unwrap().spec_rev, rev);
}

#[test]
fn a_cap_and_an_expiry_remove_items_from_dcs() {
    let mut w = world();
    w.frame_ms = 100;
    w.store
        .put_layer(Layer {
            cap: Some(1),
            expire_after_s: Some(3.0),
            ..layer("pings", true)
        })
        .unwrap();
    let a = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "a", "pings"))
        .unwrap();
    ticks(&mut w, 5);
    let b = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "b", "pings"))
        .unwrap();
    assert_eq!(b.evicted, vec![a.id]);
    ticks(&mut w, 5);
    assert!(w.sim.drawing(a.id.0).is_none() && w.sim.drawing(b.id.0).is_some());
    // 3 s of model time later, b expires.
    ticks(&mut w, 400);
    let m = w.store.marker(b.id).unwrap();
    assert_eq!(m.removed_cause, Some(RemoveCause::Expired));
    assert!(w.sim.drawing(b.id.0).is_none());
}

#[test]
fn a_mark_the_platform_does_not_hold_is_removed_by_its_dcs_id() {
    let mut w = world();
    let m = w
        .store
        .put_marker_with(put(
            MarkerSpec::Mark {
                theater: THEATER.into(),
                at: Point {
                    x: 1.0,
                    y: 0.0,
                    z: 2.0,
                },
                text: "m".into(),
                read_only: true,
            },
            "m",
            "",
        ))
        .unwrap();
    ticks(&mut w, 10);
    let held = w.sim.mark(m.id.0).unwrap().dcs_id;
    let key = w
        .rec
        .mark_clear(&mut w.store, &mut w.sim, 17)
        .expect("sent");
    ticks(&mut w, 5);
    assert_eq!(w.sim.ledger().mark_clears, vec![17]);
    let code = |w: &World, key| {
        w.rec
            .effect_result(key)
            .and_then(|r| r.result.as_ref())
            .map(|r| u64::from(r.code))
    };
    assert_eq!(code(&w, key), Some(rc::APPLIED));
    // An ID the shim holds is refused.
    let key = w
        .rec
        .mark_clear(&mut w.store, &mut w.sim, held)
        .expect("sent");
    ticks(&mut w, 5);
    assert_eq!(code(&w, key), Some(rc::BAD_PAYLOAD));
    assert!(w.sim.mark(m.id.0).is_some());
}

#[test]
fn after_a_sidecar_restart_in_the_same_generation_removals_and_hides_reach_dcs() {
    let mut w = world();
    let a = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "a", "sectors"))
        .unwrap();
    let b = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "b", ""))
        .unwrap();
    let c = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "c", ""))
        .unwrap();
    ticks(&mut w, 10);
    assert_eq!(w.sim.drawings().len(), 3);
    let sets = w.sim.ledger().pin_sets.clone();
    // Changed while no step runs, then the sidecar restarts: the old process
    // sent neither the removal nor the hide.
    w.store.put_layer(layer("sectors", false)).unwrap();
    w.store.remove_marker(b.id).unwrap();
    w.apply(&Op::SidecarRestart).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(a.id.0).is_none(), "the hidden layer left DCS");
    assert!(w.sim.drawing(b.id.0).is_none(), "the removal was sent");
    // The item DCS held is added again, under a new ID at a new revision: a
    // re-add after a restart takes a never-used ID.
    assert_eq!(
        w.sim.ledger().pin_sets.get(&c.id.0),
        Some(&(sets[&c.id.0] + 1))
    );
    assert_eq!(w.rec.marker_applied(c.id), 2);
    let held = w.sim.drawing(c.id.0).expect("c");
    assert_eq!(held.dcs_id, w.store.marker(c.id).unwrap().dcs_id);
    assert!(w.sim.ledger().ignored_adds.is_empty());
    // Removals go before sets in a step. A step's commands may be answered
    // before the campaign is announced and sent again in the next.
    let order: Vec<u16> = w
        .store
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Command(cmd)
                if [opcode::DRAWING_SET, opcode::DRAWING_REMOVE].contains(&cmd.opcode) =>
            {
                Some(cmd.opcode)
            }
            _ => None,
        })
        .skip(3)
        .collect();
    // The first step after the restart: both removals, then the set.
    assert_eq!(
        order[..3],
        [
            opcode::DRAWING_REMOVE,
            opcode::DRAWING_REMOVE,
            opcode::DRAWING_SET
        ],
        "{order:?}"
    );
}

fn mark_spec(text: &str) -> MarkerSpec {
    MarkerSpec::Mark {
        theater: THEATER.into(),
        at: Point {
            x: 1.0,
            y: 0.0,
            z: 2.0,
        },
        text: text.into(),
        read_only: true,
    }
}

#[test]
fn hide_then_show_draws_a_mark_and_a_drawing_again_under_new_ids() {
    let mut w = world();
    let d = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "d", "sectors"))
        .unwrap();
    let m = w
        .store
        .put_marker_with(put(mark_spec("m"), "m", "sectors"))
        .unwrap();
    ticks(&mut w, 10);
    let (d0, m0) = (
        w.sim.drawing(d.id.0).unwrap().dcs_id,
        w.sim.mark(m.id.0).unwrap().dcs_id,
    );
    assert_eq!(w.store.marker(d.id).unwrap().dcs_id, d0);
    w.store.put_layer(layer("sectors", false)).unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(d.id.0).is_none() && w.sim.mark(m.id.0).is_none());
    assert_eq!(w.store.marker(d.id).unwrap().dcs_id, 0, "0 while hidden");
    assert_eq!(w.store.marker(m.id).unwrap().dcs_id, 0);
    w.store.put_layer(layer("sectors", true)).unwrap();
    ticks(&mut w, 10);
    let (d1, m1) = (
        w.sim.drawing(d.id.0).expect("drawn again").dcs_id,
        w.sim.mark(m.id.0).expect("drawn again").dcs_id,
    );
    assert!(d1 > d0 && m1 > m0, "never-used IDs");
    assert_eq!(w.store.marker(d.id).unwrap().dcs_id, d1);
    assert_eq!(w.store.marker(m.id).unwrap().dcs_id, m1);
    assert!(w.sim.ledger().ignored_adds.is_empty());
    // Again: every show takes another.
    w.store.put_layer(layer("sectors", false)).unwrap();
    ticks(&mut w, 10);
    w.store.put_layer(layer("sectors", true)).unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(d.id.0).unwrap().dcs_id > d1);
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

#[test]
fn a_hide_and_show_before_the_removal_is_sent_still_adds_under_a_new_id() {
    let mut w = world();
    let d = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "d", "sectors"))
        .unwrap();
    ticks(&mut w, 10);
    let d0 = w.sim.drawing(d.id.0).unwrap().dcs_id;
    w.store.put_layer(layer("sectors", false)).unwrap();
    w.store.put_layer(layer("sectors", true)).unwrap();
    ticks(&mut w, 10);
    let held = w.sim.drawing(d.id.0).expect("drawn");
    assert!(held.dcs_id > d0);
    assert_eq!(held.dcs_id, w.store.marker(d.id).unwrap().dcs_id);
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

#[test]
fn a_changed_mark_and_a_load_add_under_new_ids_in_the_same_boot() {
    let mut w = world();
    let m = w
        .store
        .put_marker_with(put(mark_spec("one"), "m", ""))
        .unwrap();
    ticks(&mut w, 10);
    let mut last = w.sim.mark(m.id.0).unwrap().dcs_id;
    for text in ["two", "three"] {
        w.store
            .put_marker_with(put(mark_spec(text), "m", ""))
            .unwrap();
        ticks(&mut w, 10);
        let id = w.sim.mark(m.id.0).expect("drawn").dcs_id;
        assert!(id > last);
        last = id;
    }
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    let id = w.sim.mark(m.id.0).expect("drawn after the load").dcs_id;
    assert!(id > last, "the store issues a never-used ID");
    assert_eq!(w.store.marker(m.id).unwrap().dcs_id, id);
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

#[test]
fn a_sidecar_restart_adds_every_visible_item_under_a_new_id() {
    let mut w = world();
    let a = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "a", ""))
        .unwrap();
    let m = w
        .store
        .put_marker_with(put(mark_spec("m"), "m", ""))
        .unwrap();
    ticks(&mut w, 10);
    let (a0, m0) = (
        w.sim.drawing(a.id.0).unwrap().dcs_id,
        w.sim.mark(m.id.0).unwrap().dcs_id,
    );
    w.apply(&Op::SidecarRestart).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    let (a1, m1) = (
        w.sim.drawing(a.id.0).expect("held").dcs_id,
        w.sim.mark(m.id.0).expect("held").dcs_id,
    );
    assert!(a1 > a0 && m1 > m0);
    assert_eq!(w.store.marker(a.id).unwrap().dcs_id, a1);
    assert_eq!(w.store.marker(m.id).unwrap().dcs_id, m1);
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

#[test]
fn a_new_agent_boot_starts_the_ids_again() {
    let mut w = world();
    let d = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "d", ""))
        .unwrap();
    ticks(&mut w, 10);
    let first = w.sim.drawing(d.id.0).unwrap().dcs_id;
    let left = w.store.dcs_slots_left();
    w.store
        .put_marker_with(put(two_point(40.0, 1.0), "d", ""))
        .unwrap();
    ticks(&mut w, 10);
    assert_eq!(w.store.dcs_slots_left(), left - 1);
    w.apply(&Op::Crash).unwrap();
    w.apply(&Op::RestartDcs).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    let held = w.sim.drawing(d.id.0).expect("drawn in the new process");
    assert_eq!(held.dcs_id, first, "the new boot starts at slot 0");
    assert_eq!(w.store.dcs_slots_left(), left);
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

#[test]
fn a_full_band_refuses_a_put_and_writes_nothing() {
    let mut w = world();
    w.store.set_slot_limit_for_test(2);
    for l in ["a", "b"] {
        w.store
            .put_marker_with(put(two_point(0.0, 1.0), l, ""))
            .unwrap();
    }
    ticks(&mut w, 10);
    assert_eq!(w.sim.drawings().len(), 2);
    let entries = w.store.entries().len();
    let rev = w.store.revision();
    let r = w.store.put_marker_with(put(two_point(0.0, 1.0), "c", ""));
    assert!(matches!(r, Err(MarkerError::DcsIdsFull(_))), "{r:?}");
    assert_eq!(
        (w.store.entries().len(), w.store.revision()),
        (entries, rev)
    );
    assert!(w.store.marker_by_label("c").is_none());
    // A full set of a drawn item needs a slot, and the pool has none: the
    // put refuses and writes nothing. A style change in place needs none and
    // goes through as one DRAWING_UPDATE.
    let a = w.store.marker_by_label("a").expect("a");
    let before = (w.store.entries().len(), w.store.revision());
    let r = w.store.put_marker_with(put(two_point(50.0, 1.0), "a", ""));
    assert!(matches!(r, Err(MarkerError::DcsIdsFull(_))), "{r:?}");
    assert_eq!((w.store.entries().len(), w.store.revision()), before);
    assert_eq!(w.store.marker(a).unwrap().spec_rev, 1);
    w.store
        .put_marker_with(put(two_point(0.0, 0.5), "a", ""))
        .expect("a color change needs no slot");
    ticks(&mut w, 10);
    assert_eq!(sent(&w, opcode::DRAWING_UPDATE), vec![a.0]);
    assert_eq!(sent(&w, opcode::DRAWING_SET).len(), 2);
    assert_eq!(w.sim.drawing(a.0).expect("held").updates, 1);
}

#[test]
fn an_exhausted_pool_refuses_new_adds_and_keeps_what_is_shown() {
    let mut w = world();
    w.store.set_slot_limit_for_test(3);
    w.store.put_layer(layer("l", true)).unwrap();
    let mut ids = Vec::new();
    for (label, l) in [("a", ""), ("b", ""), ("c", "l")] {
        ids.push(
            w.store
                .put_marker_with(put(two_point(0.0, 1.0), label, l))
                .unwrap()
                .id,
        );
    }
    ticks(&mut w, 10);
    assert_eq!(w.sim.drawings().len(), 3);
    assert_eq!(w.store.dcs_slots_left(), 0);
    let state = |w: &World| (w.store.entries().len(), w.store.revision());
    let before = state(&w);
    // A new item, and a full set of one drawn, are refused with nothing
    // written.
    for p in [
        put(two_point(0.0, 1.0), "new", ""),
        put(mark_spec("m"), "m", ""),
        put(two_point(70.0, 1.0), "a", ""),
    ] {
        let r = w.store.put_marker_with(p);
        assert!(matches!(r, Err(MarkerError::DcsIdsFull(_))), "{r:?}");
    }
    assert_eq!(state(&w), before);
    // What is shown stays, and a style change in place still works.
    w.store
        .put_marker_with(put(two_point(0.0, 0.25), "b", ""))
        .expect("style change");
    ticks(&mut w, 10);
    assert_eq!(sent(&w, opcode::DRAWING_UPDATE), vec![ids[1].0]);
    assert_eq!(w.sim.drawings().len(), 3);
    // A removal works, a show of a hidden layer is refused, and the
    // reconciler goes on.
    w.store
        .put_layer(layer("l", false))
        .expect("hide takes no slot");
    ticks(&mut w, 10);
    assert!(w.sim.drawing(ids[2].0).is_none());
    let before = state(&w);
    let r = w.store.put_layer(layer("l", true));
    assert!(matches!(r, Err(MarkerError::DcsIdsFull(_))), "{r:?}");
    assert_eq!(state(&w), before);
    w.store.remove_marker(ids[0]).expect("remove");
    ticks(&mut w, 10);
    assert!(w.sim.drawing(ids[0].0).is_none());
    assert!(w.sim.drawing(ids[1].0).is_some());
    assert!(w.sim.ledger().ignored_adds.is_empty());
    // A new DCS process starts the pool empty.
    w.apply(&Op::Crash).unwrap();
    w.apply(&Op::RestartDcs).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(ids[1].0).is_some());
    assert!(w.store.dcs_slots_left() > 0);
}

#[test]
fn an_add_rejected_for_now_takes_a_fresh_id_when_a_later_add_has_applied() {
    let mut w = world();
    w.sim.pin_not_ready(1);
    let a = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "a", ""))
        .unwrap();
    let b = w
        .store
        .put_marker_with(put(two_point(50.0, 1.0), "b", ""))
        .unwrap();
    ticks(&mut w, 400);
    // `a` was rejected NOT_READY and `b` applied, which moved the shim's
    // process mark past `a`'s ID: the resend is a fresh, higher ID.
    let (da, db) = (
        w.sim.drawing(a.id.0).expect("a shows").dcs_id,
        w.sim.drawing(b.id.0).expect("b shows").dcs_id,
    );
    assert!(da > db, "a is added under a fresh ID: {da} {db}");
    // Two sends: the rejected one and the applied one. A resend under the
    // old ID would be refused and cost a third.
    let sets = sent(&w, opcode::DRAWING_SET);
    assert_eq!(sets.iter().filter(|e| **e == a.id.0).count(), 2, "{sets:?}");
    assert!(!w.rec.marker_refused(a.id, &w.store));
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

#[test]
fn the_crash_retry_is_given_again_at_a_later_revision() {
    let mut w = world();
    // Two refusals at the first revision: the retry is refused too, and
    // that is final.
    w.sim.refuse_adds(3);
    let a = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "a", ""))
        .unwrap();
    ticks(&mut w, 100);
    assert!(w.rec.marker_refused(a.id, &w.store));
    assert!(w.sim.drawing(a.id.0).is_none());
    // A later revision is refused once more and then retried.
    let mut again = put(two_point(0.0, 0.5), "a", "");
    again.id = a.id;
    w.store.put_marker_with(again).unwrap();
    ticks(&mut w, 100);
    assert!(w.sim.drawing(a.id.0).is_some(), "the new revision retries");
    assert!(!w.rec.marker_refused(a.id, &w.store));
    assert_eq!(w.sim.ledger().refused_adds, 3);
}

fn record(name: &str, state: CampaignState) -> CampaignRecord {
    CampaignRecord {
        name: name.into(),
        state,
        theaters: vec![THEATER.into()],
        theater: THEATER.into(),
        clock_ms: Some(1_700_000_000_000),
        created_ms: 1,
        component_schemas: BTreeMap::new(),
        outcome: None,
    }
}

/// Two campaigns, A active in `w` and B suspended, and the server store.
fn two_campaigns(dir: &std::path::Path) -> (World, Campaigns, ServerStore) {
    let c = Campaigns::new(dir, 10_000, 3).expect("campaigns");
    let server = ServerStore::open(dir, 0).expect("server store");
    let mut w = world();
    for (id, state) in [
        (CampaignId(A), CampaignState::Active),
        (CampaignId(B), CampaignState::Suspended),
    ] {
        std::fs::create_dir_all(c.folder(id)).expect("folder");
        let mut s = if id.0 == A {
            std::mem::replace(&mut w.store, Store::new(id, 1))
        } else {
            new_store_for(B, 0, 0)
        };
        s.save_as(&c.store_path(id), record("c", state))
            .expect("save");
        if id.0 == A {
            w.store = s;
        }
    }
    w.settle().expect("settles");
    (w, c, server)
}

const A: u64 = nanoprobe_sim_fake::harness::CAMPAIGN;
const B: u64 = A + 1;

/// Switch the agent to campaign `to`, closing the active store into the
/// server store, and load its mission.
fn switch(w: &mut World, c: &Campaigns, server: &ServerStore, to: u64) {
    let cur = std::mem::replace(&mut w.store, Store::new(CampaignId(0), 1));
    let mut active = Some(cur);
    c.activate_with(
        Some(server),
        None,
        &mut active,
        CampaignId(to),
        30_000,
        Vec::new(),
    )
    .expect("activate");
    w.sidecar_of(active.take().expect("store"));
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(w, 10);
}

#[test]
fn campaigns_that_switch_in_one_boot_each_keep_their_items() {
    let d = tempfile::tempdir().expect("dir");
    let (mut w, c, server) = two_campaigns(d.path());
    let a = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "a", ""))
        .unwrap();
    let am = w
        .store
        .put_marker_with(put(mark_spec("am"), "am", ""))
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(a.id.0).is_some() && w.sim.mark(am.id.0).is_some());
    let a_top = w.sim.drawing(a.id.0).unwrap().dcs_id;

    switch(&mut w, &c, &server, B);
    let b = w
        .store
        .put_marker_with(put(two_point(9.0, 1.0), "b", ""))
        .unwrap();
    ticks(&mut w, 10);
    let b_id = w.sim.drawing(b.id.0).expect("b shows").dcs_id;
    assert!(b_id > a_top, "B starts above A's mark: {b_id} {a_top}");
    assert!(!w.rec.marker_refused(b.id, &w.store));
    assert_eq!(
        sent(&w, opcode::DRAWING_SET),
        vec![b.id.0],
        "no refused add"
    );

    switch(&mut w, &c, &server, A);
    ticks(&mut w, 10);
    let a_again = w.sim.drawing(a.id.0).expect("a shows again").dcs_id;
    assert!(a_again > b_id, "A starts above B's mark: {a_again} {b_id}");
    assert!(w.sim.mark(am.id.0).is_some());
    assert_eq!(
        sent(&w, opcode::DRAWING_SET),
        vec![a.id.0],
        "no refused add"
    );
    assert_eq!(sent(&w, opcode::MARK_SET), vec![am.id.0], "no refused add");
    assert!(w.sim.ledger().ignored_adds.is_empty());
    assert!(!w.rec.marker_refused(a.id, &w.store));
    assert!(!w.rec.marker_refused(am.id, &w.store));
}

#[test]
fn a_campaign_whose_predecessor_crashed_is_refused_once_and_adds_under_a_fresh_id() {
    let d = tempfile::tempdir().expect("dir");
    let (mut w, c, server) = two_campaigns(d.path());
    for i in 0..3 {
        w.store
            .put_marker_with(put(two_point(f64::from(i), 1.0), &format!("a{i}"), ""))
            .unwrap();
    }
    ticks(&mut w, 10);
    let a_top = w.sim.drawings().values().map(|d| d.dcs_id).max().unwrap();
    // A ends without its closing transaction: the server store has no row.
    let a = std::mem::replace(&mut w.store, Store::new(CampaignId(0), 1));
    std::mem::forget(a);
    let b_store = c.open(CampaignId(B), 30_000).expect("open b");
    w.sidecar_of(b_store);
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    let b = w
        .store
        .put_marker_with(put(two_point(9.0, 1.0), "b", ""))
        .unwrap();
    ticks(&mut w, 20);
    let held = w.sim.drawing(b.id.0).expect("b shows");
    assert!(held.dcs_id > a_top);
    // The first add was refused, the second applied; no more.
    assert_eq!(sent(&w, opcode::DRAWING_SET), vec![b.id.0, b.id.0]);
    assert!(!w.rec.marker_refused(b.id, &w.store));
    assert!(w.sim.ledger().ignored_adds.is_empty());
    // The mark moved past the shim's: the next add needs no retry.
    let b2 = w
        .store
        .put_marker_with(put(two_point(19.0, 1.0), "b2", ""))
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(b2.id.0).is_some());
    assert_eq!(
        sent(&w, opcode::DRAWING_SET)
            .iter()
            .filter(|e| **e == b2.id.0)
            .count(),
        1
    );
    drop(server);
}

/// The marks and drawings DCS holds, ascending by DCS ID: the order a client
/// draws them in.
fn drawn_order(w: &World) -> Vec<EntityId> {
    let mut v: Vec<(u64, u64)> = w
        .sim
        .marks()
        .iter()
        .map(|(e, m)| (m.dcs_id, *e))
        .chain(w.sim.drawings().iter().map(|(e, d)| (d.dcs_id, *e)))
        .collect();
    v.sort();
    v.into_iter().map(|(_, e)| EntityId(e)).collect()
}

/// The live items by ascending draw rank, those never sent last by creation.
fn rank_order(w: &World) -> Vec<EntityId> {
    let mut v: Vec<(u64, EntityId)> = w
        .store
        .markers()
        .filter(|m| !m.removed && m.dcs_id != 0)
        .map(|m| (m.rank, m.id))
        .collect();
    v.sort();
    v.into_iter().map(|(_, e)| e).collect()
}

/// Items alternating mark and drawing, in creation order.
fn interleaved(w: &mut World, n: usize) -> Vec<EntityId> {
    (0..n)
        .map(|i| {
            let spec = if i % 2 == 0 {
                two_point(i as f64, 1.0)
            } else {
                mark_spec("m")
            };
            w.store
                .put_marker_with(put(spec, &format!("i{i}"), ""))
                .unwrap()
                .id
        })
        .collect()
}

#[test]
fn a_mission_load_and_a_new_agent_boot_redraw_in_rank_order_marks_and_drawings_together() {
    let mut w = world();
    let ids = interleaved(&mut w, 6);
    ticks(&mut w, 10);
    assert_eq!(drawn_order(&w), ids);
    w.apply(&Op::LoadMission).unwrap();
    assert!(drawn_order(&w).is_empty());
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert_eq!(drawn_order(&w), ids, "after a mission load");
    assert_eq!(rank_order(&w), ids);
    w.apply(&Op::Crash).unwrap();
    w.apply(&Op::RestartDcs).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert_eq!(drawn_order(&w), ids, "after a new agent boot");
    assert!(w.sim.ledger().ignored_adds.is_empty());
}

#[test]
fn an_item_redrawn_by_a_full_set_or_a_show_moves_to_the_top_of_the_order() {
    let mut w = world();
    w.store.put_layer(layer("l", true)).unwrap();
    let ids = interleaved(&mut w, 5);
    let shown = w
        .store
        .put_marker_with(put(mark_spec("s"), "s", "l"))
        .unwrap()
        .id;
    ticks(&mut w, 10);
    // A full set of the first item.
    w.store
        .put_marker_with(put(two_point(500.0, 1.0), "i0", ""))
        .unwrap();
    ticks(&mut w, 10);
    let want = vec![ids[1], ids[2], ids[3], ids[4], shown, ids[0]];
    assert_eq!(drawn_order(&w), want);
    // A show of the layer's item.
    w.store.put_layer(layer("l", false)).unwrap();
    ticks(&mut w, 10);
    w.store.put_layer(layer("l", true)).unwrap();
    ticks(&mut w, 10);
    let want = vec![ids[1], ids[2], ids[3], ids[4], ids[0], shown];
    assert_eq!(drawn_order(&w), want);
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert_eq!(drawn_order(&w), want, "kept across a load");
}

#[test]
fn an_item_never_sent_goes_last_by_creation_in_a_redraw() {
    let mut w = world();
    w.store.put_layer(layer("l", false)).unwrap();
    // Two items created first and hidden, so never sent.
    let x = w
        .store
        .put_marker_with(put(two_point(0.0, 1.0), "x", "l"))
        .unwrap()
        .id;
    let y = w
        .store
        .put_marker_with(put(mark_spec("y"), "y", "l"))
        .unwrap()
        .id;
    let ids = interleaved(&mut w, 3);
    ticks(&mut w, 10);
    assert_eq!(drawn_order(&w), ids);
    w.apply(&Op::LoadMission).unwrap();
    // Shown before the redraw: they hold rank 0 and have older IDs.
    w.store.put_layer(layer("l", true)).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    let mut want = ids.clone();
    want.extend([x, y]);
    assert_eq!(drawn_order(&w), want);
}

#[test]
fn a_redraw_larger_than_one_steps_budget_keeps_the_order() {
    let mut w = world();
    let ids = interleaved(&mut w, 75);
    ticks(&mut w, 30);
    assert_eq!(drawn_order(&w), ids);
    // The oldest is set again, so it is the last to draw.
    w.store
        .put_marker_with(put(two_point(900.0, 1.0), "i0", ""))
        .unwrap();
    ticks(&mut w, 10);
    let mut want: Vec<EntityId> = ids[1..].to_vec();
    want.push(ids[0]);
    assert_eq!(drawn_order(&w), want);
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 30);
    assert_eq!(drawn_order(&w), want);
    assert_eq!(rank_order(&w), want);
    w.apply(&Op::SidecarRestart).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 30);
    // Items the agent still holds stay where they are; those re-sent go on
    // top in rank order, so the client order is the rank order.
    assert_eq!(drawn_order(&w), rank_order(&w), "after a sidecar restart");
    assert_eq!(drawn_order(&w).len(), want.len());
    assert!(w.sim.ledger().ignored_adds.is_empty());
}
