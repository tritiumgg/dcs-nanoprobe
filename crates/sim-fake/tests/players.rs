//! The player-facing commands of step S16 through the reconciler and the
//! fake sim: marks, drawings and menu items as set-state entities, messages,
//! chat, evals and mission loads as effects, extension removal, and the
//! records the agent writes for the API.

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::sim::wire::{Body, lv, opcode};
use nanoprobe_sidecar::store::{Authority, Entry, LostPolicy, MarkerSpec, Outcome, Point, scope};
use nanoprobe_sim_fake::Config;
use nanoprobe_sim_fake::harness::{Op, THEATER, World};
use nanoprobe_transport::codec::Lv;

fn world(groups: usize) -> World {
    let mut w = World::new(groups, 0, LostPolicy::Report);
    w.settle().expect("settles");
    w
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().expect("tick");
    }
}

fn sent(w: &World, op: u16) -> usize {
    w.store
        .entries()
        .iter()
        .filter(|e| matches!(e, Entry::Command(c) if c.opcode == op))
        .count()
}

/// The entities of the commands of one opcode, in the order they were sent.
fn sent_order(w: &World, op: u16) -> Vec<u64> {
    w.store
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Command(c) if c.opcode == op => Some(c.entity.0),
            _ => None,
        })
        .collect()
}

fn mark(text: &str) -> MarkerSpec {
    MarkerSpec::Mark {
        theater: THEATER.into(),
        at: Point {
            x: 100.0,
            y: 5.0,
            z: 200.0,
        },
        text: text.into(),
        read_only: false,
    }
}

fn menu(parent: EntityId, submenu: bool, text: &str) -> MarkerSpec {
    MarkerSpec::MenuItem {
        parent,
        submenu,
        text: text.into(),
    }
}

fn message(scope: u32, target: u64, clear: bool, seconds: f64, text: &str) -> Body {
    Body::Message {
        scope,
        clear,
        target,
        seconds,
        text: text.into(),
    }
}

/// A drawing of `shape` with as many points as the shape takes (two for a
/// shape of unknown layout), then the style.
fn drawing(shape: u32) -> MarkerSpec {
    let points = match shape {
        5 => 1,
        6 => 4,
        7 => 3,
        _ => 2,
    };
    let rgba = || lv::list(vec![lv::n(1.0), lv::n(0.0), lv::n(0.0), lv::n(1.0)]);
    let mut args: Vec<Lv> = (0..points)
        .map(|i| {
            lv::obj(vec![
                ("x", lv::n(f64::from(i))),
                ("y", lv::n(0.0)),
                ("z", lv::n(2.0)),
            ])
        })
        .collect();
    args.extend([
        rgba(),
        rgba(),
        lv::n(1.0),
        Lv::Bool(true),
        Lv::String(b"m".to_vec()),
    ]);
    MarkerSpec::Drawing {
        shape,
        args: lv::list(args),
    }
}

#[test]
fn a_mark_is_set_replaced_and_removed() {
    let mut w = world(0);
    let (id, rev) = w
        .store
        .put_marker(EntityId(0), scope::COALITION, 2, mark("rally"))
        .unwrap();
    assert_eq!(rev, 1);
    ticks(&mut w, 10);
    let m = w.sim.mark(id.0).expect("set in DCS");
    assert_eq!((m.scope, m.target, m.rev), (scope::COALITION, 2, 1));
    assert_eq!(m.text, "rally");
    assert_eq!(m.at, (100.0, 5.0, 200.0));
    assert_eq!(w.rec.marker_applied(id), 1);
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::MARK_SET), 1, "once per revision");

    // The same spec again is no change; a new text is a new revision.
    assert_eq!(
        w.store
            .put_marker(id, scope::COALITION, 2, mark("rally"))
            .unwrap(),
        (id, 1)
    );
    let (_, rev) = w
        .store
        .put_marker(id, scope::COALITION, 2, mark("rally point"))
        .unwrap();
    assert_eq!(rev, 2);
    ticks(&mut w, 10);
    assert_eq!(w.sim.mark(id.0).unwrap().text, "rally point");
    assert_eq!(w.sim.mark(id.0).unwrap().rev, 2);
    assert_eq!(sent(&w, opcode::MARK_SET), 2);

    // Removal: sent once, and the mark is gone.
    assert_eq!(w.store.remove_marker(id).unwrap(), vec![id]);
    ticks(&mut w, 10);
    assert!(w.sim.mark(id.0).is_none());
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::MARK_REMOVE), 1);
    assert!(w.store.marker(id).unwrap().removed);
    // A removed marker cannot be replaced.
    assert!(
        w.store
            .put_marker(id, scope::ALL, 0, mark("again"))
            .is_err()
    );
}

#[test]
fn drawings_are_set_replaced_and_removed() {
    let mut w = world(0);
    let (id, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, drawing(7))
        .unwrap();
    ticks(&mut w, 10);
    let d = w.sim.drawing(id.0).expect("set in DCS");
    assert_eq!((d.shape, d.scope, d.rev), (7, scope::ALL, 1));
    w.store
        .put_marker(id, scope::COALITION, 1, drawing(4))
        .unwrap();
    ticks(&mut w, 10);
    let d = w.sim.drawing(id.0).unwrap();
    assert_eq!(
        (d.shape, d.scope, d.target, d.rev),
        (4, scope::COALITION, 1, 2)
    );
    // A drawing takes no group.
    assert!(
        w.store
            .put_marker(EntityId(0), scope::GROUP, 5, drawing(1))
            .is_err()
    );
    w.store.remove_marker(id).unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(id.0).is_none());
    assert_eq!(sent(&w, opcode::DRAWING_REMOVE), 1);
}

#[test]
fn a_drawing_of_an_unknown_layout_never_reaches_dcs() {
    // The store takes any layout; the API refuses unknown ones. A store
    // that holds one (an older store file) is refused by the shim, so DCS
    // never sees it.
    let mut w = world(0);
    for shape in [1, 3, 4, 5, 6, 7] {
        let (id, _) = w
            .store
            .put_marker(EntityId(0), scope::ALL, 0, drawing(shape))
            .unwrap();
        ticks(&mut w, 10);
        assert!(w.sim.drawing(id.0).is_some(), "shape {shape} not drawn");
    }
    for shape in [0, 2, 8, 9, 10, 11, 12] {
        let (id, _) = w
            .store
            .put_marker(EntityId(0), scope::ALL, 0, drawing(shape))
            .unwrap();
        ticks(&mut w, 10);
        assert!(w.sim.drawing(id.0).is_none(), "shape {shape} drawn");
    }
    let one_point = MarkerSpec::Drawing {
        shape: 1,
        args: lv::list(vec![lv::obj(vec![("x", lv::n(1.0)), ("z", lv::n(2.0))])]),
    };
    let (id, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, one_point)
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.drawing(id.0).is_none(), "a line of one point drawn");
    assert_eq!(w.sim.ledger().drawing_layouts_refused, 8);
}

#[test]
fn everything_is_set_again_after_a_new_generation_parents_first() {
    let mut w = world(0);
    let (m, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, mark("a"))
        .unwrap();
    let (d, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, drawing(3))
        .unwrap();
    let (root, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(EntityId(0), true, "Root"))
        .unwrap();
    let (sub, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(root, true, "Sub"))
        .unwrap();
    let (leaf, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(sub, false, "Go"))
        .unwrap();
    // Added last, in the order of the first set: reparent the root's
    // sibling under a later item to make an ID order that is not the depth
    // order.
    let (late, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(EntityId(0), true, "Late"))
        .unwrap();
    w.store
        .put_marker(root, scope::ALL, 0, menu(late, true, "Root"))
        .unwrap();
    ticks(&mut w, 20);
    assert!(w.sim.mark(m.0).is_some() && w.sim.drawing(d.0).is_some());
    let order: Vec<u64> = w.sim.menu_items().iter().map(|(e, _)| *e).collect();
    let pos = |e: EntityId| order.iter().position(|x| *x == e.0).expect("held");
    assert!(pos(late) < pos(root) && pos(root) < pos(sub) && pos(sub) < pos(leaf));

    // A mission load removes all of it; the sidecar sets it all again, a
    // parent before its children.
    let before = sent(&w, opcode::MENU_SET);
    w.apply(&Op::LoadMission).unwrap();
    assert!(w.sim.mark(m.0).is_none() && w.sim.menu_items().is_empty());
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.mark(m.0).is_some());
    assert!(w.sim.drawing(d.0).is_some());
    assert_eq!(w.sim.menu_items().len(), 4);
    let again: Vec<u64> = sent_order(&w, opcode::MENU_SET)[before..].to_vec();
    let at = |e: EntityId| again.iter().position(|x| *x == e.0).expect("sent");
    assert!(at(late) < at(root) && at(root) < at(sub) && at(sub) < at(leaf));
    assert_eq!(w.sim.menu_item(leaf.0).unwrap().parent, sub.0);
    // Held once per generation.
    let n = sent(&w, opcode::MENU_SET);
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::MENU_SET), n);
}

#[test]
fn a_submenu_leaves_after_the_items_under_it() {
    let mut w = world(0);
    let (root, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(EntityId(0), true, "Root"))
        .unwrap();
    let (sub, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(root, true, "Sub"))
        .unwrap();
    let (leaf, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(sub, false, "Go"))
        .unwrap();
    ticks(&mut w, 15);
    assert_eq!(w.sim.menu_items().len(), 3);
    let gone = w.store.remove_marker(root).unwrap();
    assert_eq!(gone, vec![leaf, sub, root]);
    ticks(&mut w, 15);
    assert!(w.sim.menu_items().is_empty());
    assert_eq!(
        sent_order(&w, opcode::MENU_REMOVE),
        vec![leaf.0, sub.0, root.0]
    );
    // A parent must be a submenu of the same audience.
    let (cmd, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(EntityId(0), false, "x"))
        .unwrap();
    assert!(
        w.store
            .put_marker(EntityId(0), scope::ALL, 0, menu(cmd, false, "y"))
            .is_err()
    );
    let (s2, _) = w
        .store
        .put_marker(
            EntityId(0),
            scope::COALITION,
            1,
            menu(EntityId(0), true, "s"),
        )
        .unwrap();
    assert!(
        w.store
            .put_marker(EntityId(0), scope::COALITION, 2, menu(s2, false, "y"))
            .is_err()
    );
    // No item is its own ancestor.
    assert!(
        w.store
            .put_marker(s2, scope::COALITION, 1, menu(s2, true, "s"))
            .is_err()
    );
}

#[test]
fn a_group_scoped_marker_waits_for_its_group_and_follows_a_respawn() {
    let mut w = world(1);
    let g = w.store.groups().next().unwrap().id;
    let (m, _) = w
        .store
        .put_marker(EntityId(0), scope::GROUP, g.0, mark("here"))
        .unwrap();
    let (mi, _) = w
        .store
        .put_marker(
            EntityId(0),
            scope::GROUP,
            g.0,
            menu(EntityId(0), false, "Hi"),
        )
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.mark(m.0).is_some() && w.sim.menu_item(mi.0).is_some());
    assert_eq!(sent(&w, opcode::MARK_SET), 1);

    // The group leaves DCS; its marks and menu items go with its DCS ID.
    w.store.set_authority(g, Authority::Campaign).unwrap();
    ticks(&mut w, 12);
    assert!(w.sim.mark(m.0).is_none() && w.sim.menu_item(mi.0).is_none());
    ticks(&mut w, 20);
    assert_eq!(
        sent(&w, opcode::MARK_SET),
        1,
        "nothing is sent for no group"
    );
    // It spawns again: the marker is set again, at a new revision.
    w.store.set_authority(g, Authority::Sim).unwrap();
    ticks(&mut w, 20);
    assert_eq!(w.sim.mark(m.0).expect("set again").rev, 2);
    assert!(w.sim.menu_item(mi.0).is_some());
    assert_eq!(sent(&w, opcode::MARK_SET), 2);
    assert_eq!(sent(&w, opcode::MENU_SET), 2);
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::MARK_SET), 2);
}

#[test]
fn a_marker_set_before_its_group_exists_in_dcs_is_set_when_it_does() {
    let mut w = world(1);
    let g = w.store.groups().next().unwrap().id;
    w.store.set_authority(g, Authority::Campaign).unwrap();
    ticks(&mut w, 12);
    let (m, _) = w
        .store
        .put_marker(EntityId(0), scope::GROUP, g.0, mark("later"))
        .unwrap();
    ticks(&mut w, 20);
    assert_eq!(sent(&w, opcode::MARK_SET), 0);
    w.store.set_authority(g, Authority::Sim).unwrap();
    ticks(&mut w, 20);
    assert_eq!(w.sim.mark(m.0).expect("set").rev, 1);
}

#[test]
fn a_mark_that_waits_for_its_group_goes_on_top_when_it_binds() {
    // spec/world.md, "Redraw order": an item that waits for its audience is
    // added when it can be and goes on top; the items after it are not held.
    let mut w = world(1);
    let g = w.store.groups().next().unwrap().id;
    w.store.set_authority(g, Authority::Campaign).unwrap();
    ticks(&mut w, 12);
    let (m, _) = w
        .store
        .put_marker(EntityId(0), scope::GROUP, g.0, mark("waits"))
        .unwrap();
    let (d, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, drawing(7))
        .unwrap();
    ticks(&mut w, 20);
    assert!(w.sim.mark(m.0).is_none(), "the group is not in DCS");
    let drawn = w.sim.drawing(d.0).expect("not held back").dcs_id;
    w.store.set_authority(g, Authority::Sim).unwrap();
    ticks(&mut w, 20);
    assert!(w.sim.mark(m.0).expect("set").dcs_id > drawn, "on top");
}

#[test]
fn markers_survive_a_sidecar_restart_and_a_dcs_restart() {
    let mut w = world(0);
    let (m, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, mark("a"))
        .unwrap();
    ticks(&mut w, 10);
    let first = w.sim.mark(m.0).unwrap().dcs_id;
    // A restart builds a new reconciler: it adds the mark again under a new
    // ID at a new revision, since DCS shows nothing for an ID it held.
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 20);
    assert_eq!(w.sim.ledger().pin_sets[&m.0], 2);
    assert_eq!(w.rec.marker_applied(m), 2);
    assert_eq!(sent(&w, opcode::MARK_SET), 2);
    let held = w.sim.mark(m.0).unwrap();
    assert!(held.dcs_id > first);
    assert_eq!(held.dcs_id, w.store.marker(m).unwrap().dcs_id);
    // A removal sent after a restart reaches the agent that holds it.
    w.store.remove_marker(m).unwrap();
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 20);
    assert!(w.sim.mark(m.0).is_none());
    // DCS restarts: nothing is held, and a removed marker is not sent.
    let (m2, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, mark("b"))
        .unwrap();
    w.apply(&Op::Crash).unwrap();
    w.apply(&Op::RestartDcs).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.mark(m2.0).is_some());
    assert!(w.sim.mark(m.0).is_none());
}

#[test]
fn messages_chat_and_evals_are_sent_once() {
    let mut w = world(1);
    let g = w.store.groups().next().unwrap().id;
    let k1 = w
        .rec
        .message(
            &mut w.store,
            &mut w.sim,
            message(2, 2, true, 12.5, "hello blue"),
        )
        .expect("sent");
    let k2 = w
        .rec
        .message(
            &mut w.store,
            &mut w.sim,
            message(3, g.0, false, 5.0, "group"),
        )
        .expect("a group DCS holds");
    assert!(
        w.rec
            .message(
                &mut w.store,
                &mut w.sim,
                message(3, 999, false, 5.0, "none")
            )
            .is_none(),
        "no group in DCS"
    );
    let k3 = w
        .rec
        .chat(&mut w.store, &mut w.sim, 0, "welcome")
        .expect("sent");
    let k4 = w
        .rec
        .eval(&mut w.store, &mut w.sim, "return 42")
        .expect("sent");
    let k5 = w
        .rec
        .eval(&mut w.store, &mut w.sim, "error('x')")
        .expect("sent");
    ticks(&mut w, 12);
    for k in [k1, k2, k3, k4] {
        assert_eq!(w.rec.effect_result(k).unwrap().outcome, Outcome::Applied);
    }
    assert_eq!(w.rec.effect_result(k5).unwrap().outcome, Outcome::Refused);
    assert_eq!(
        w.rec
            .effect_result(k4)
            .unwrap()
            .result
            .as_ref()
            .unwrap()
            .value,
        Lv::Number(42.0)
    );
    let l = w.sim.ledger();
    assert_eq!(
        l.messages,
        vec![
            (2, 2, true, 12.5, "hello blue".to_owned()),
            (3, g.0, false, 5.0, "group".to_owned())
        ]
    );
    assert_eq!(l.chats, vec![(0, "welcome".to_owned())]);
    assert_eq!(l.evals.len(), 2);

    // Never sent again: not by time, a link drop, a restart or a load.
    ticks(&mut w, 40);
    w.apply(&Op::Detach).unwrap();
    w.apply(&Op::Reattach).unwrap();
    w.apply(&Op::SidecarRestart).unwrap();
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    let l = w.sim.ledger();
    assert_eq!((l.messages.len(), l.chats.len(), l.evals.len()), (2, 1, 2));
    assert_eq!(sent(&w, opcode::MESSAGE), 2);
    assert_eq!(sent(&w, opcode::CHAT), 1);
    assert_eq!(sent(&w, opcode::EVAL), 2);
}

#[test]
fn a_bad_message_or_chat_is_rejected_by_the_agent() {
    let mut w = world(0);
    let k = w
        .rec
        .message(
            &mut w.store,
            &mut w.sim,
            message(1, 0, false, f64::NAN, "x"),
        )
        .unwrap();
    let c = w.rec.chat(&mut w.store, &mut w.sim, 0, "").unwrap();
    ticks(&mut w, 8);
    assert_eq!(w.rec.effect_result(k).unwrap().outcome, Outcome::Rejected);
    assert_eq!(w.rec.effect_result(c).unwrap().outcome, Outcome::Rejected);
    assert!(w.sim.ledger().messages.is_empty() && w.sim.ledger().chats.is_empty());
}

#[test]
fn raw_lua_off_answers_rejected() {
    let mut w = World::with_config(
        0,
        0,
        LostPolicy::Report,
        Config {
            raw_lua: false,
            ..Config::default()
        },
    );
    w.settle().unwrap();
    let k = w.rec.eval(&mut w.store, &mut w.sim, "return 1").unwrap();
    ticks(&mut w, 8);
    let r = w.rec.effect_result(k).unwrap();
    assert_eq!(r.outcome, Outcome::Rejected);
    assert_eq!(
        u64::from(r.result.as_ref().unwrap().code),
        nanoprobe_transport::layout::result::RAW_LUA_OFF
    );
    assert!(w.sim.ledger().evals.is_empty());
}

#[test]
fn a_mission_load_command_is_an_effect_that_loads() {
    let mut w = world(0);
    let gen_before = w.sim.mission_gen();
    let k = w
        .rec
        .mission_load(&mut w.store, &mut w.sim, "missions/next.miz")
        .expect("sent");
    ticks(&mut w, 4);
    assert_eq!(w.rec.effect_result(k).unwrap().outcome, Outcome::Applied);
    assert_eq!(
        w.sim.ledger().mission_loads,
        vec!["missions/next.miz".to_owned()]
    );
    assert_eq!(w.sim.mission_gen(), gen_before + 1);
    w.settle().unwrap();
    assert_eq!(sent(&w, opcode::MISSION_LOAD), 1);
    assert_eq!(w.sim.ledger().mission_loads.len(), 1);
}

#[test]
fn an_extension_is_removed_and_registered_again_above_its_old_revision() {
    let mut w = world(0);
    assert_eq!(w.store.set_extension("count", "return {}"), 1);
    assert_eq!(w.store.set_extension("count", "return {1}"), 2);
    ticks(&mut w, 10);
    assert_eq!(w.sim.extension_rev("count"), 2);
    assert_eq!(w.rec.extension_applied("count"), 2);

    assert!(w.store.remove_extension("count"));
    assert!(!w.store.remove_extension("count"));
    assert_eq!(w.store.extension_removals().count(), 1);
    ticks(&mut w, 10);
    assert_eq!(w.sim.extension_rev("count"), 0);
    assert_eq!(w.sim.ledger().extension_removals, vec!["count".to_owned()]);
    assert_eq!(w.rec.extension_applied("count"), 0);
    assert_eq!(w.store.extension_removals().count(), 0);
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::EXTENSION_REMOVE), 1);

    // Registered again before the agent confirmed gone would still apply:
    // the revision is above any the agent held.
    assert_eq!(w.store.set_extension("count", "return {2}"), 3);
    ticks(&mut w, 10);
    assert_eq!(w.sim.extension_rev("count"), 3);
    // Removed and registered at once, the new source replaces the old.
    assert!(w.store.remove_extension("count"));
    assert_eq!(w.store.set_extension("count", "return {3}"), 4);
    ticks(&mut w, 10);
    assert_eq!(w.sim.extension_rev("count"), 4);
}

#[test]
fn an_extension_removed_across_a_sidecar_restart_is_still_removed() {
    let mut w = world(0);
    w.store.set_extension("e", "return {}");
    ticks(&mut w, 10);
    assert_eq!(w.sim.extension_rev("e"), 1);
    w.store.remove_extension("e");
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 20);
    assert_eq!(w.sim.extension_rev("e"), 0);
}

#[test]
fn the_agent_s_player_records_reach_the_reconciler_without_harm() {
    let mut w = world(1);
    let u = w.sim.objects()[0].0;
    w.sim.emit_author_event(&[u], "tick", lv::n(1.0));
    let (m, _) = w
        .store
        .put_marker(EntityId(0), scope::ALL, 0, menu(EntityId(0), false, "Go"))
        .unwrap();
    ticks(&mut w, 10);
    assert!(w.sim.select_menu(m.0, 7));
    assert!(!w.sim.select_menu(999, 7));
    ticks(&mut w, 6);
    assert!(w.rec.counters().other >= 2);
    assert_eq!(w.rec.counters().undecodable, 0);
}
