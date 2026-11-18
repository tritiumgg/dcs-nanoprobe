//! The world commands of step S15 through the reconciler and the fake sim:
//! controller options, airbase owners, warehouse stock and adjustments,
//! damage, sample rates, the country map, and attacks on units and statics.

use nanoprobe_sidecar::intents::Intent;
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::sim::wire::{lv, opcode};
use nanoprobe_sidecar::stock::{Item, Stock};
use nanoprobe_sidecar::store::{Authority, Entry, LostPolicy, Outcome};
use nanoprobe_sim_fake::harness::{CAMPAIGN, Op, THEATER, World};
use nanoprobe_transport::records::Rates;

fn world(groups: usize, statics: usize) -> World {
    let mut w = World::new(groups, statics, LostPolicy::Report);
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

fn weapon(n: &str, c: f64) -> Stock {
    let mut s = Stock::default();
    s.set(Item::Weapon(n.into()), c);
    s
}

#[test]
fn options_reach_dcs_once_and_again_after_a_new_spawn() {
    let mut w = world(1, 0);
    let g = w.store.groups().next().unwrap().id;
    let spec_rev = w.store.group(g).unwrap().spec_rev;
    w.store.set_option(g, 3, lv::n(2.0)).unwrap();
    w.store.set_option(g, 9, lv::s("x")).unwrap();
    ticks(&mut w, 8);
    assert_eq!(w.sim.option_of(g.0, 3), Some(&lv::n(2.0)));
    assert_eq!(w.sim.option_of(g.0, 9), Some(&lv::s("x")));
    assert_eq!(sent(&w, opcode::GROUP_OPTION), 2);
    ticks(&mut w, 20);
    assert_eq!(sent(&w, opcode::GROUP_OPTION), 2, "an applied option stays");
    assert_eq!(w.store.group(g).unwrap().spec_rev, spec_rev);

    // A changed value is sent; the unchanged one is not.
    w.store.set_option(g, 3, lv::n(1.0)).unwrap();
    ticks(&mut w, 8);
    assert_eq!(w.sim.option_of(g.0, 3), Some(&lv::n(1.0)));
    assert_eq!(sent(&w, opcode::GROUP_OPTION), 3);

    // A mission load removes the group; its options go with the new spawn.
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 8);
    assert_eq!(w.sim.option_of(g.0, 3), Some(&lv::n(1.0)));
    assert_eq!(w.sim.option_of(g.0, 9), Some(&lv::s("x")));
    assert_eq!(sent(&w, opcode::GROUP_OPTION), 5);

    // Dematerialize and materialize: a new spawn in the same generation.
    w.store.set_authority(g, Authority::Campaign).unwrap();
    ticks(&mut w, 10);
    assert_eq!(w.store.group(g).unwrap().state, State::Virtual);
    assert_eq!(w.sim.option_of(g.0, 3), None);
    w.store.set_authority(g, Authority::Sim).unwrap();
    ticks(&mut w, 12);
    assert_eq!(w.sim.option_of(g.0, 3), Some(&lv::n(1.0)));
    assert_eq!(sent(&w, opcode::GROUP_OPTION), 7);
}

#[test]
fn an_airbase_owner_is_sent_once_per_difference_and_generation() {
    let mut w = world(0, 0);
    w.sim.add_airbase("Batumi", 1, weapon("AIM-9X", 4.0));
    ticks(&mut w, 12);
    w.store.set_airbase_owner(THEATER, "Batumi", 2);
    ticks(&mut w, 12);
    assert_eq!(w.sim.airbase_owner("Batumi"), Some(2));
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 1);
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 1);

    // DCS captures it: a fact that replaces the desired owner.
    w.sim.capture_airbase("Batumi", 1);
    ticks(&mut w, 12);
    assert_eq!(
        w.store.desired_owner(THEATER, "Batumi").unwrap().coalition,
        1
    );
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 1, "no command fights it");
    assert_eq!(w.sim.airbase_owner("Batumi"), Some(1));

    // The controller sets blue again: a new difference.
    w.store.set_airbase_owner(THEATER, "Batumi", 2);
    ticks(&mut w, 12);
    assert_eq!(w.sim.airbase_owner("Batumi"), Some(2));
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 2);

    // A mission load gives the mission's owner back; it is sent again.
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 15);
    assert_eq!(w.sim.airbase_owner("Batumi"), Some(2));
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 3);
}

#[test]
fn a_farp_spawned_again_gets_its_owner_and_stock_again() {
    let mut w = world(0, 0);
    w.sim.add_airbase("np#farp", 1, Stock::default());
    ticks(&mut w, 12);
    w.store.set_airbase_owner(THEATER, "np#farp", 2);
    w.store
        .set_stock(THEATER, "np#farp", &weapon("AIM-9X", 4.0));
    ticks(&mut w, 12);
    assert_eq!(w.sim.airbase_owner("np#farp"), Some(2));
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 1);
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 1);

    // The FARP is destroyed, then spawned again in the same generation with
    // the mission's owner, an empty warehouse and a new object: the desired
    // values, kept by name, are sent again. Nothing is sent while it is gone.
    let first = w.sim.airbase_object("np#farp");
    w.sim.destroy_farp("np#farp");
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 1);
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 1);
    w.sim.add_airbase("np#farp", 1, Stock::default());
    assert_ne!(w.sim.airbase_object("np#farp"), first);
    ticks(&mut w, 15);
    assert_eq!(w.sim.airbase_owner("np#farp"), Some(2));
    let stock = w.sim.airbase_stock("np#farp").unwrap();
    assert_eq!(stock.get(&Item::Weapon("AIM-9X".into())), Some(4.0));
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 2);
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 2);
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 2);
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 2);
}

#[test]
fn an_airbase_the_controller_never_set_is_left_alone() {
    let mut w = world(0, 0);
    w.sim.add_airbase("Kutaisi", 1, Stock::default());
    ticks(&mut w, 20);
    w.sim.capture_airbase("Kutaisi", 2);
    ticks(&mut w, 20);
    assert!(w.store.desired_owner(THEATER, "Kutaisi").is_none());
    assert_eq!(sent(&w, opcode::AIRBASE_OWNER), 0);
}

#[test]
fn warehouse_sets_adjustments_and_facts() {
    let mut w = world(0, 0);
    w.sim.add_airbase("Batumi", 1, weapon("AIM-9X", 4.0));
    ticks(&mut w, 12);
    // Setting an item sends only the difference, and keeps other items.
    w.store
        .set_stock(THEATER, "Batumi", &weapon("AIM-9X", 10.0));
    ticks(&mut w, 12);
    let stock = w.sim.airbase_stock("Batumi").unwrap();
    assert_eq!(stock.get(&Item::Weapon("AIM-9X".into())), Some(10.0));
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 1);
    ticks(&mut w, 30);
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 1);

    // An adjustment is an effect: applied once, and a fact for the item.
    let key = w
        .rec
        .adjust_warehouse(&mut w.store, &mut w.sim, "Batumi", &weapon("AIM-9X", 5.0))
        .expect("sent");
    ticks(&mut w, 12);
    let r = w.rec.effect_result(key).expect("answered");
    assert_eq!(r.outcome, Outcome::Applied);
    let stock = w.sim.airbase_stock("Batumi").unwrap();
    assert_eq!(stock.get(&Item::Weapon("AIM-9X".into())), Some(15.0));
    let d = w.store.desired_stock(THEATER, "Batumi").unwrap();
    assert_eq!(d.stock.get(&Item::Weapon("AIM-9X".into())), Some(15.0));
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 1);
    assert_eq!(sent(&w, opcode::WAREHOUSE_ADJUST), 1);

    // An adjustment below zero stops at zero and is applied (LIVE, E6).
    let key = w
        .rec
        .adjust_warehouse(&mut w.store, &mut w.sim, "Batumi", &weapon("AIM-9X", -99.0))
        .unwrap();
    ticks(&mut w, 12);
    assert_eq!(w.rec.effect_result(key).unwrap().outcome, Outcome::Applied);
    let stock = w.sim.airbase_stock("Batumi").unwrap();
    assert_eq!(stock.get(&Item::Weapon("AIM-9X".into())), Some(0.0));
    let d = w.store.desired_stock(THEATER, "Batumi").unwrap();
    assert_eq!(d.stock.get(&Item::Weapon("AIM-9X".into())), Some(0.0));

    // DCS uses stock.
    w.sim
        .use_stock("Batumi", Item::Weapon("AIM-9X".into()), 3.0);
    ticks(&mut w, 12);
    let d = w.store.desired_stock(THEATER, "Batumi").unwrap();
    assert_eq!(d.stock.get(&Item::Weapon("AIM-9X".into())), Some(3.0));
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 1);

    // A mission load resets the stock; the desired count is sent again.
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 15);
    let stock = w.sim.airbase_stock("Batumi").unwrap();
    assert_eq!(stock.get(&Item::Weapon("AIM-9X".into())), Some(3.0));
    assert_eq!(sent(&w, opcode::WAREHOUSE_SET), 2);
}

#[test]
fn damage_and_capture_of_a_live_group() {
    let mut w = world(1, 0);
    let g = w.store.groups().next().unwrap().id;
    let unit = w.store.group(g).unwrap().members[0];
    let key = w
        .rec
        .damage(&mut w.store, &mut w.sim, unit, 10.0)
        .expect("sent");
    ticks(&mut w, 6);
    assert_eq!(w.rec.effect_result(key).unwrap().outcome, Outcome::Applied);
    assert_eq!(w.sim.life_percent_of(unit.0), Some(10.0));
    // An unbound unit is not sent.
    assert!(
        w.rec
            .damage(
                &mut w.store,
                &mut w.sim,
                nanoprobe_sidecar::ids::EntityId(999),
                1.0
            )
            .is_none()
    );

    let key = w.rec.capture(&mut w.store, &mut w.sim, g).expect("sent");
    ticks(&mut w, 6);
    assert_eq!(w.rec.effect_result(key).unwrap().outcome, Outcome::Applied);
    let u = w.store.unit(unit).unwrap();
    assert!((u.state.life - 0.1).abs() < 1e-6, "{}", u.state.life);
    assert_eq!(w.store.group(g).unwrap().state, State::Live);
}

#[test]
fn rates_are_sent_after_every_hello() {
    let mut w = world(0, 0);
    let r = Rates {
        position_s: 2.0,
        ..Rates::default()
    };
    w.store.set_rates(r);
    ticks(&mut w, 5);
    assert_eq!(w.sim.rates(), (r, 1));
    ticks(&mut w, 20);
    assert_eq!(w.sim.rates().1, 1, "sent once");
    w.apply(&Op::Detach).unwrap();
    w.apply(&Op::Reattach).unwrap();
    ticks(&mut w, 8);
    assert_eq!(w.sim.rates(), (r, 2));
}

#[test]
fn the_country_map_is_asked_once_per_generation() {
    let mut w = world(1, 0);
    ticks(&mut w, 10);
    assert_eq!(w.sim.ledger().countries_asked, 1);
    assert_eq!(w.store.country_coalition(2), Some(2));
    assert_eq!(w.store.country_coalition(0), Some(1));
    // The same generation, the link drops and returns: the map stays.
    w.apply(&Op::Detach).unwrap();
    w.apply(&Op::Reattach).unwrap();
    ticks(&mut w, 15);
    assert_eq!(w.sim.ledger().countries_asked, 1);
    // A sidecar restart builds a new reconciler, which asks again.
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 15);
    assert_eq!(w.sim.ledger().countries_asked, 2);
    // A new mission generation forgets the map and asks again.
    w.apply(&Op::LoadMission).unwrap();
    assert!(w.store.countries().is_none());
    w.settle().unwrap();
    ticks(&mut w, 10);
    assert_eq!(w.sim.ledger().countries_asked, 3);
    assert!(w.store.countries().is_some());
    assert_eq!(w.rec.counters().coalition_disagreements, 0);
}

#[test]
fn a_lost_country_answer_is_asked_again() {
    let mut w = world(0, 0);
    w.apply(&Op::LoadMission).unwrap();
    // Run until the query is in flight, then lose its answer.
    for _ in 0..40 {
        if w.rec.inflight().any(|c| c.opcode == opcode::COUNTRIES) {
            break;
        }
        w.tick().unwrap();
    }
    assert!(w.rec.inflight().any(|c| c.opcode == opcode::COUNTRIES));
    w.apply(&Op::LoseResult).unwrap();
    w.apply(&Op::Reattach).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 15);
    assert!(w.store.countries().is_some());
    assert!(w.sim.ledger().countries_asked >= 2);
}

#[test]
fn an_attack_on_a_unit_names_its_dcs_object_and_follows_a_respawn() {
    let mut w = world(2, 0);
    let ids: Vec<_> = w.store.groups().map(|g| g.id).collect();
    let (a, b) = (ids[0], ids[1]);
    let unit = w.store.group(b).unwrap().members[0];
    let rev = w
        .store
        .set_order(a, Intent::Attack { target: unit })
        .unwrap();
    ticks(&mut w, 12);
    let object = w
        .sim
        .objects()
        .into_iter()
        .find(|(_, _, e)| *e == unit.0)
        .map(|(o, _, _)| o)
        .unwrap();
    assert_eq!(w.sim.attack_unit_of(CAMPAIGN, a.0), Some(object));
    assert_eq!(w.sim.applied_order(a.0), rev);

    // The target group spawns again in this generation: new object IDs, so
    // the attacker's order gets a new revision and is compiled again.
    w.store.set_authority(b, Authority::Campaign).unwrap();
    ticks(&mut w, 10);
    w.store.set_authority(b, Authority::Sim).unwrap();
    ticks(&mut w, 15);
    let object2 = w
        .sim
        .objects()
        .into_iter()
        .find(|(_, _, e)| *e == unit.0)
        .map(|(o, _, _)| o)
        .unwrap();
    assert_ne!(object, object2);
    assert!(w.store.group(a).unwrap().order_rev > rev);
    assert_eq!(w.sim.attack_unit_of(CAMPAIGN, a.0), Some(object2));
}

#[test]
fn an_attack_waits_for_a_static_that_is_not_in_dcs() {
    let mut w = world(1, 1);
    let g = w
        .store
        .groups()
        .find(|g| g.kind == nanoprobe_sidecar::store::Kind::Group)
        .unwrap()
        .id;
    let st = w
        .store
        .groups()
        .find(|g| g.kind == nanoprobe_sidecar::store::Kind::Static)
        .unwrap()
        .id;
    w.store.set_authority(st, Authority::Campaign).unwrap();
    ticks(&mut w, 12);
    w.store.set_order(g, Intent::Attack { target: st }).unwrap();
    ticks(&mut w, 10);
    assert_eq!(
        w.sim.attack_unit_of(CAMPAIGN, g.0),
        None,
        "the target is not in DCS"
    );
    w.store.set_authority(st, Authority::Sim).unwrap();
    ticks(&mut w, 15);
    let object = w
        .sim
        .objects()
        .into_iter()
        .find(|(_, _, e)| *e == st.0)
        .map(|(o, _, _)| o)
        .unwrap();
    assert_eq!(w.sim.attack_unit_of(CAMPAIGN, g.0), Some(object));
}

#[test]
fn a_resync_hello_in_the_same_generation_keeps_the_country_map() {
    use nanoprobe_sidecar::sim::RawRecord;
    use nanoprobe_transport::layout::kind;
    use nanoprobe_transport::records::Hello;

    let mut w = world(0, 0);
    ticks(&mut w, 10);
    assert_eq!(w.sim.ledger().countries_asked, 1);
    let h = Hello {
        agent_boot: w.sim.agent_boot(),
        campaign: CAMPAIGN,
        epoch: 1,
        ..Hello::default()
    };
    let rec = RawRecord {
        agent_boot: w.sim.agent_boot(),
        number: 1 << 40,
        kind: kind::HELLO as u16,
        payload: h.encode(),
        resync: true,
    };
    w.rec.record(&mut w.store, &rec);
    ticks(&mut w, 15);
    assert_eq!(w.sim.ledger().countries_asked, 1);
    assert!(w.store.countries().is_some());
}

#[test]
fn a_spec_change_on_a_live_group_spawns_it_again_in_place() {
    let mut w = world(2, 0);
    let ids: Vec<_> = w.store.groups().map(|g| g.id).collect();
    let (a, b) = (ids[0], ids[1]);
    let unit = w.store.group(b).unwrap().members[0];
    let order = w
        .store
        .set_order(a, Intent::Attack { target: unit })
        .unwrap();
    w.store.set_option(a, 3, lv::n(2.0)).unwrap();
    ticks(&mut w, 12);
    assert_eq!(w.sim.applied_order(a.0), order);
    assert_eq!(w.sim.option_of(a.0, 3), Some(&lv::n(2.0)));
    let (spawns, orders, options) = (
        sent(&w, opcode::GROUP_SPAWN),
        sent(&w, opcode::GROUP_ORDER),
        sent(&w, opcode::GROUP_OPTION),
    );
    let old: Vec<u64> = w
        .sim
        .objects()
        .into_iter()
        .filter(|(_, _, e)| w.store.group(a).unwrap().members.iter().any(|m| m.0 == *e))
        .map(|(o, _, _)| o)
        .collect();
    assert!(!old.is_empty());

    // No change, no spawn.
    ticks(&mut w, 10);
    assert_eq!(
        sent(&w, opcode::GROUP_SPAWN),
        spawns,
        "no spawn at the same revision"
    );

    // A spec change: one spawn, in place, and the group stays live.
    let m = w.store.group(a).unwrap().members[0];
    let rev = w.store.set_unit_type(m, "BTR-80").unwrap();
    for _ in 0..30 {
        w.tick().expect("tick");
        assert_eq!(w.store.group(a).unwrap().state, State::Live);
        assert!(
            sent(&w, opcode::GROUP_SPAWN) <= spawns + 1,
            "one spawn in flight at most"
        );
    }
    assert_eq!(sent(&w, opcode::GROUP_SPAWN), spawns + 1);
    let now: Vec<u64> = w
        .sim
        .objects()
        .into_iter()
        .filter(|(_, _, e)| w.store.group(a).unwrap().members.iter().any(|m| m.0 == *e))
        .map(|(o, _, _)| o)
        .collect();
    assert_eq!(now.len(), old.len());
    assert!(now.iter().all(|o| !old.contains(o)), "new object IDs");
    assert_eq!(
        w.sim
            .ledger()
            .spawns
            .iter()
            .filter(|s| s.2 == a.0 && s.3 == rev)
            .count(),
        1
    );

    // The order and the options follow the spawn, once.
    assert_eq!(sent(&w, opcode::GROUP_ORDER), orders + 1);
    assert_eq!(sent(&w, opcode::GROUP_OPTION), options + 1);
    assert_eq!(w.sim.option_of(a.0, 3), Some(&lv::n(2.0)));
    assert!(w.sim.applied_order(a.0) >= order);
    assert!(w.store.group(a).unwrap().order_rev >= order);
    ticks(&mut w, 15);
    assert_eq!(sent(&w, opcode::GROUP_SPAWN), spawns + 1);
    assert_eq!(sent(&w, opcode::GROUP_ORDER), orders + 1);
    assert_eq!(sent(&w, opcode::GROUP_OPTION), options + 1);
}

#[test]
fn a_spec_change_on_a_live_static_spawns_it_again() {
    let mut w = world(0, 1);
    let s = w.store.groups().next().unwrap().id;
    let spawns = sent(&w, opcode::STATIC_SPAWN);
    let m = w.store.group(s).unwrap().members[0];
    w.store.set_unit_type(m, "Hangar").unwrap();
    ticks(&mut w, 15);
    assert_eq!(sent(&w, opcode::STATIC_SPAWN), spawns + 1);
    assert_eq!(w.store.group(s).unwrap().state, State::Live);
    ticks(&mut w, 10);
    assert_eq!(sent(&w, opcode::STATIC_SPAWN), spawns + 1);
}

#[test]
fn sounds_and_pictures_are_effects_sent_once() {
    use nanoprobe_sidecar::sim::wire::Body;
    let mut w = world(1, 0);
    let g = w.store.groups().next().unwrap().id;
    let unit = w.store.group(g).unwrap().members[0];
    let picture = |scope, target| Body::Picture {
        scope,
        target,
        clear_view: 0,
        seconds: 5.0,
        size: 40.0,
        horizontal: 1,
        vertical: 1,
        size_units: 0,
        file: "Key".into(),
    };
    let keys = [
        w.rec.sound(&mut w.store, &mut w.sim, 1, 0, "a.ogg"),
        w.rec.sound(&mut w.store, &mut w.sim, 2, 2, "b.ogg"),
        w.rec.sound(&mut w.store, &mut w.sim, 5, 80, "c.ogg"),
        w.rec.sound(&mut w.store, &mut w.sim, 3, g.0, "d.ogg"),
        w.rec.sound(&mut w.store, &mut w.sim, 4, unit.0, "e.ogg"),
        w.rec.picture(&mut w.store, &mut w.sim, picture(4, unit.0)),
        w.rec.picture_stop(&mut w.store, &mut w.sim),
    ];
    ticks(&mut w, 10);
    for key in keys {
        let r = w.rec.effect_result(key.expect("sent")).expect("answered");
        assert_eq!(r.outcome, Outcome::Applied);
    }
    let ledger = w.sim.ledger();
    assert_eq!(
        ledger.sounds,
        vec![
            (1, 0, "a.ogg".to_owned()),
            (2, 2, "b.ogg".to_owned()),
            (5, 80, "c.ogg".to_owned()),
            (3, g.0, "d.ogg".to_owned()),
            (4, unit.0, "e.ogg".to_owned()),
        ]
    );
    assert_eq!(ledger.pictures, vec![picture(4, unit.0)]);
    assert_eq!(ledger.picture_stops, 1);

    // Effects are never sent again, a mission load included.
    w.apply(&Op::LoadMission).unwrap();
    w.settle().unwrap();
    ticks(&mut w, 20);
    assert_eq!(sent(&w, opcode::SOUND), 5);
    assert_eq!(sent(&w, opcode::PICTURE), 1);
    assert_eq!(sent(&w, opcode::PICTURE_STOP), 1);
    assert_eq!(w.sim.ledger().sounds.len(), 5);

    // A group or unit DCS holds no object of is not sent; other scopes are.
    let gone = nanoprobe_sidecar::ids::EntityId(999);
    assert!(
        w.rec
            .sound(&mut w.store, &mut w.sim, 3, gone.0, "x.ogg")
            .is_none()
    );
    assert!(
        w.rec
            .picture(&mut w.store, &mut w.sim, picture(4, gone.0))
            .is_none()
    );
    assert!(
        w.rec
            .sound(&mut w.store, &mut w.sim, 1, 0, "x.ogg")
            .is_some()
    );
}

#[test]
fn a_group_driving_a_route_makes_one_event_per_waypoint_and_one_completion() {
    use nanoprobe_sidecar::intents::Waypoint;
    let mut w = world(1, 0);
    let g = w.store.groups().next().expect("group").id;
    let lead = w.store.group(g).unwrap().members[0];
    let at = w.store.unit(lead).unwrap().state.at;
    // The fake sim moves 100 m a frame and reports every fifth frame, so a
    // sample is 500 m after the one before and passes waypoints between.
    let waypoints = vec![
        Waypoint {
            x: at.x + 777.0,
            z: at.z,
        },
        Waypoint {
            x: at.x + 777.0,
            z: at.z + 1_333.0,
        },
        Waypoint {
            x: at.x + 2_501.0,
            z: at.z + 1_333.0,
        },
    ];
    let route = |waypoints: Vec<Waypoint>| Intent::Route {
        waypoints,
        speed: 50.0,
        on_road: false,
    };
    w.store.take_route_events();
    let rev = w.store.set_order(g, route(waypoints.clone())).unwrap();
    // A sidecar restart in the middle must not repeat or lose an event.
    ticks(&mut w, 12);
    w.apply(&Op::SidecarRestart).expect("restart");
    ticks(&mut w, 60);
    let ev = w.store.take_route_events();
    let kinds: Vec<Option<usize>> = ev.iter().map(|e| e.index).collect();
    assert_eq!(kinds, vec![Some(0), Some(1), Some(2), None], "{ev:?}");
    assert!(
        ev.iter()
            .all(|e| e.group == g && e.order_rev == rev && e.observed)
    );
    assert!(ev.windows(2).all(|p| p[0].model_time <= p[1].model_time));
    ticks(&mut w, 20);
    assert!(w.store.take_route_events().is_empty(), "nothing twice");

    // A new order revision numbers from 0 and completes again.
    let rev2 = w
        .store
        .set_order(g, route(waypoints[..1].to_vec()))
        .unwrap();
    ticks(&mut w, 60);
    let ev = w.store.take_route_events();
    let kinds: Vec<Option<usize>> = ev.iter().map(|e| e.index).collect();
    assert_eq!(kinds, vec![Some(0), None], "{ev:?}");
    assert!(ev.iter().all(|e| e.order_rev == rev2));
}

/// The frame that carries a sample is the frame the reconciler first sees a
/// desired value, whatever the sample rate: try every offset.
const OFFSETS: usize = 24;

#[test]
fn a_sample_before_the_first_send_does_not_replace_a_desired_stock() {
    for offset in 0..OFFSETS {
        let mut w = world(0, 0);
        w.sim.add_airbase("Batumi", 1, weapon("AIM-9X", 4.0));
        ticks(&mut w, 12 + offset);
        // DCS changes the count as the controller sets one: the sample
        // that shows it predates our send.
        w.store
            .set_stock(THEATER, "Batumi", &weapon("AIM-9X", 10.0));
        w.sim
            .use_stock("Batumi", Item::Weapon("AIM-9X".into()), 1.0);
        ticks(&mut w, 1);
        let d = w.store.desired_stock(THEATER, "Batumi").unwrap();
        assert_eq!(
            d.stock.get(&Item::Weapon("AIM-9X".into())),
            Some(10.0),
            "offset {offset}"
        );
        ticks(&mut w, 24);
        let stock = w.sim.airbase_stock("Batumi").unwrap();
        assert_eq!(stock.get(&Item::Weapon("AIM-9X".into())), Some(10.0));
    }
}

#[test]
fn a_sample_before_the_first_send_does_not_replace_a_desired_owner() {
    for offset in 0..OFFSETS {
        let mut w = world(0, 0);
        w.sim.add_airbase("Batumi", 1, Stock::default());
        ticks(&mut w, 12 + offset);
        w.store.set_airbase_owner(THEATER, "Batumi", 2);
        w.sim.capture_airbase("Batumi", 0);
        ticks(&mut w, 1);
        assert_eq!(
            w.store.desired_owner(THEATER, "Batumi").unwrap().coalition,
            2,
            "offset {offset}"
        );
        ticks(&mut w, 24);
        assert_eq!(w.sim.airbase_owner("Batumi"), Some(2));
    }
}

#[test]
fn a_sample_while_an_owner_is_unresolved_is_not_a_fact() {
    // Set blue, then set red back before the blue command applies. Blue
    // applies; the desired owner stays red and red is sent again.
    for offset in 0..OFFSETS {
        let mut w = world(0, 0);
        w.sim.add_airbase("Batumi", 1, Stock::default());
        ticks(&mut w, 12 + offset);
        w.store.set_airbase_owner(THEATER, "Batumi", 2);
        ticks(&mut w, 1);
        w.store.set_airbase_owner(THEATER, "Batumi", 1);
        ticks(&mut w, 30);
        assert_eq!(
            w.store.desired_owner(THEATER, "Batumi").unwrap().coalition,
            1,
            "offset {offset}"
        );
        assert_eq!(w.sim.airbase_owner("Batumi"), Some(1), "offset {offset}");
    }
}

#[test]
fn a_sample_while_a_stock_is_unresolved_is_not_a_fact() {
    // Set 10, then set 4 back before the first WAREHOUSE_SET applies. The
    // desired count stays 4 and 4 is sent again.
    for offset in 0..OFFSETS {
        let mut w = world(0, 0);
        w.sim.add_airbase("Batumi", 1, weapon("AIM-9X", 4.0));
        ticks(&mut w, 12 + offset);
        w.store
            .set_stock(THEATER, "Batumi", &weapon("AIM-9X", 10.0));
        ticks(&mut w, 1);
        w.store.set_stock(THEATER, "Batumi", &weapon("AIM-9X", 4.0));
        ticks(&mut w, 30);
        let item = Item::Weapon("AIM-9X".into());
        let d = w.store.desired_stock(THEATER, "Batumi").unwrap();
        assert_eq!(d.stock.get(&item), Some(4.0), "offset {offset}");
        let stock = w.sim.airbase_stock("Batumi").unwrap();
        assert_eq!(stock.get(&item), Some(4.0), "offset {offset}");
    }
}

#[test]
fn an_adjust_changes_the_desired_count_whatever_the_sample_timing() {
    // SET A=10 in flight, ADJUST A=+5 before the next sample: desired 15,
    // and a new mission generation sends 15.
    for offset in 0..OFFSETS {
        let mut w = world(0, 0);
        w.sim.add_airbase("Batumi", 1, weapon("AIM-9X", 4.0));
        ticks(&mut w, 12 + offset);
        w.store
            .set_stock(THEATER, "Batumi", &weapon("AIM-9X", 10.0));
        ticks(&mut w, 1);
        w.rec
            .adjust_warehouse(&mut w.store, &mut w.sim, "Batumi", &weapon("AIM-9X", 5.0))
            .expect("sent");
        ticks(&mut w, 30);
        let item = Item::Weapon("AIM-9X".into());
        let d = w.store.desired_stock(THEATER, "Batumi").unwrap();
        assert_eq!(d.stock.get(&item), Some(15.0), "offset {offset}");
        w.apply(&Op::LoadMission).unwrap();
        w.settle().unwrap();
        ticks(&mut w, 15);
        let stock = w.sim.airbase_stock("Batumi").unwrap();
        assert_eq!(stock.get(&item), Some(15.0), "offset {offset}");
    }
}

#[test]
fn an_unresolved_set_holds_only_the_items_it_names() {
    for offset in 0..OFFSETS {
        let mut w = world(0, 0);
        let mut stock = weapon("AIM-9X", 4.0);
        stock.set(Item::Weapon("AIM-120".into()), 6.0);
        w.sim.add_airbase("Batumi", 1, stock);
        ticks(&mut w, 12 + offset);
        let b = Item::Weapon("AIM-120".into());
        let mut both = weapon("AIM-9X", 4.0);
        both.set(b.clone(), 6.0);
        w.store.set_stock(THEATER, "Batumi", &both);
        ticks(&mut w, 12);
        w.store
            .set_stock(THEATER, "Batumi", &weapon("AIM-9X", 10.0));
        ticks(&mut w, 1);
        w.sim.use_stock("Batumi", b.clone(), 2.0);
        ticks(&mut w, 30);
        let d = w.store.desired_stock(THEATER, "Batumi").unwrap();
        assert_eq!(d.stock.get(&b), Some(2.0), "offset {offset}");
    }
}

#[test]
fn a_whole_map_clear_folds_its_tiles_and_a_restart_leaves_it_unknown() {
    use nanoprobe_sidecar::sim::wire::Debris;
    use nanoprobe_transport::codec::Lv;
    let mut w = world(1, 0);
    let value = |w: &World, key| {
        let r = w.rec.effect_result(key).expect("answered");
        (r.outcome, r.result.as_ref().map(|x| x.value.clone()))
    };
    let key = w
        .rec
        .clear_debris(
            &mut w.store,
            &mut w.sim,
            Debris::Sphere {
                center: [1.0, 2.0, 3.0],
                radius: 10.0,
            },
        )
        .expect("sent");
    ticks(&mut w, 6);
    assert_eq!(value(&w, key), (Outcome::Applied, Some(Lv::Number(3.0))));

    let key = w
        .rec
        .clear_debris_everywhere(&mut w.store, &mut w.sim)
        .expect("sent");
    ticks(&mut w, 100);
    assert_eq!(sent(&w, opcode::CLEAR_DEBRIS), 1 + 256);
    assert_eq!(
        value(&w, key),
        (Outcome::Applied, Some(Lv::Number(3.0 * 256.0)))
    );
    // Only the folded result is kept: no tile has one of its own.
    assert!(w.rec.effect_result((key.0, key.1 - 1)).is_none());

    // A restart in the middle loses the fold: the call gets no result and
    // nothing is left in flight, so it settles as `unknown`.
    let key = w
        .rec
        .clear_debris_everywhere(&mut w.store, &mut w.sim)
        .expect("sent");
    ticks(&mut w, 3);
    w.apply(&Op::SidecarRestart).unwrap();
    ticks(&mut w, 150);
    assert!(w.rec.effect_result(key).is_none());
    assert!(w.rec.inflight().all(|j| (j.epoch, j.seq) != key));
    assert!(w.rec.effect_result((key.0, key.1 - 1)).is_none());
}
