//! The harness's ruin rule: a spawn over the dead object of a static is a
//! violation only when the sidecar had read the static's death before it
//! wrote the spawn. spec/world.md, "Ruins", says more: the object of a static
//! that died in the generation is never replaced. That fails when a campaign
//! switch cleared the agent's revisions and the death is read only after the
//! spawn was written (docs/results/2026-11-18-ruin-oracle-late-death.md); the
//! sidecar cannot know of a death it has not read. Each test plants a ruin
//! spawn by hand, as a faulty reconciler would write it.

use nanoprobe_sidecar::ids::{self, CampaignId, EntityId, NameKind};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sidecar::sim::wire::{Body, Command, lv};
use nanoprobe_sidecar::store::{Kind, LostPolicy};
use nanoprobe_sim_fake::Config;
use nanoprobe_sim_fake::harness::{CAMPAIGN, Violation, World};
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::records::CommandHeader;

fn world() -> (World, EntityId) {
    let mut w = World::with_config(
        1,
        1,
        LostPolicy::Report,
        Config {
            event_queue: 10_000,
            ..Config::default()
        },
    );
    ticks(&mut w, 40);
    let id = w
        .store
        .groups()
        .find(|g| g.kind == Kind::Static)
        .map(|g| g.id)
        .expect("a static");
    assert_eq!(w.store.group(id).expect("static").state, State::Live);
    (w, id)
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().unwrap();
    }
}

fn object_of(w: &World, id: EntityId) -> u64 {
    w.units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == id.0)
        .map(|(o, _)| o)
        .expect("the static is in DCS")
}

/// Write a ruin spawn for the static into the ring, outside the reconciler,
/// at a revision the agent does not hold and a sequence number above any
/// the reconciler wrote; `n` tells several plants apart. The agent sees it
/// after the next flush.
fn write_ruin_spawn(w: &mut World, id: EntityId, n: u64) {
    let rev = w.store.group(id).expect("static").spec_rev + 1_000 + n;
    let cmd = Command {
        header: CommandHeader {
            seq: (1 << 40) + n,
            epoch: 1 << 40,
            campaign: CAMPAIGN,
            entity: id.0,
            rev,
            mission_gen: w.sim.mission_gen(),
        },
        body: Body::StaticSpawn {
            country: 2,
            name: ids::name(NameKind::Static, CampaignId(CAMPAIGN), id),
            table: lv::obj(vec![
                ("type", lv::s("Tent")),
                ("x", lv::n(0.0)),
                ("y", lv::n(0.0)),
                ("dead", Lv::Bool(true)),
            ]),
        },
    };
    let (op, payload) = cmd.encode();
    w.sim.send(op, &payload).expect("attached");
}

/// [`write_ruin_spawn`], then a flush.
fn plant_ruin_spawn(w: &mut World, id: EntityId, n: u64) {
    write_ruin_spawn(w, id, n);
    w.sim.flush();
}

/// A ruin spawn written after the sidecar read the static's death replaces
/// the dead object DCS holds: the harness reports it.
#[test]
fn a_ruin_spawned_over_a_death_the_sidecar_had_is_a_violation() {
    let (mut w, id) = world();
    let o = object_of(&w, id);
    w.sim.kill(o);
    ticks(&mut w, 5);
    assert_eq!(w.store.group(id).expect("static").state, State::Dead);
    plant_ruin_spawn(&mut w, id, 0);
    match w.tick() {
        Err(Violation(v)) => assert_eq!(
            v,
            format!("a ruin was spawned over static {id}, which died in that generation")
        ),
        Ok(()) => panic!("the spawn over the ruin was not reported"),
    }
}

/// A ruin spawn written before the sidecar read the static's death counts
/// as not known, even when the death record is in the ring when the spawn
/// runs; one written after the read counts as known. The fake is driven by
/// hand here, frame by frame, with no reconciler step.
#[test]
fn a_spawn_counts_as_known_only_when_written_after_the_death_was_read() {
    let (mut w, id) = world();
    let o = object_of(&w, id);
    w.sim.kill(o);
    w.sim.frame();
    plant_ruin_spawn(&mut w, id, 0);
    w.sim.frame();
    let key = {
        let l = w.sim.ledger();
        assert_eq!(l.ruins_replaced.len(), 1);
        let (b, g, c, entity, spawned, known) = l.ruins_replaced[0];
        assert_eq!((entity, spawned, known), (id.0, false, false));
        let key = (b, g, c, entity);
        assert!(l.reported_deaths.contains(&key), "the death is in the ring");
        assert!(!l.deaths_read.contains_key(&key), "and not read");
        key
    };
    w.sim.read();
    w.sim.flush();
    assert!(w.sim.ledger().deaths_read.contains_key(&key));
    plant_ruin_spawn(&mut w, id, 1);
    w.sim.frame();
    let l = w.sim.ledger();
    assert_eq!(l.ruins_replaced.len(), 2);
    let (_, _, _, entity, spawned, known) = l.ruins_replaced[1];
    assert_eq!((entity, spawned, known), (id.0, true, true));
}

/// A death read after the spawn was written and before it ran counts as not
/// known: it reached the sidecar too late to stop the spawn. A read with no
/// flush does not count at all.
#[test]
fn a_death_read_while_the_spawn_waits_in_the_ring_counts_as_not_known() {
    let (mut w, id) = world();
    let o = object_of(&w, id);
    w.sim.kill(o);
    w.sim.frame();
    write_ruin_spawn(&mut w, id, 0);
    w.sim.read();
    let key = |w: &World| {
        let l = w.sim.ledger();
        l.reported_deaths.iter().copied().find(|k| k.3 == id.0)
    };
    let k = key(&w).expect("the death is in the ring");
    assert!(
        !w.sim.ledger().deaths_read.contains_key(&k),
        "a read with no flush is not counted"
    );
    w.sim.flush();
    assert!(w.sim.ledger().deaths_read.contains_key(&k));
    w.sim.frame();
    let l = w.sim.ledger();
    assert_eq!(l.ruins_replaced.len(), 1);
    let (_, _, _, entity, spawned, known) = l.ruins_replaced[0];
    assert_eq!((entity, spawned, known), (id.0, false, false));
}
