//! The lifetime of adopted units: they follow the binding across mission
//! generations and sidecar restarts, share a group binding, and are never
//! reconciled.

use std::cell::RefCell;

use nanoprobe_sidecar::players::{Ctx, Keyring, Note, Roster, Unmanaged};
use nanoprobe_sidecar::replay::Batch;
use nanoprobe_sidecar::sim::SimAdapter;
use nanoprobe_sidecar::store::LostPolicy;
use nanoprobe_sim_fake::harness::{Op, World};

struct Probe {
    w: World,
    roster: Roster,
    cursor: usize,
    notes: Vec<Note>,
}

impl Probe {
    fn new() -> Probe {
        let mut w = World::new(1, 0, LostPolicy::Report);
        w.settle().expect("settles");
        let mut p = Probe {
            w,
            roster: Roster::new(Keyring::from_secret([3; 32])),
            cursor: 0,
            notes: Vec::new(),
        };
        p.fold();
        p
    }

    fn fold(&mut self) {
        let w = &mut self.w;
        let env = RefCell::new((&mut w.rec, &mut w.store, &mut w.sim));
        let bound = |o: u64| {
            let g = env.borrow();
            g.0.entity_of(&*g.1, o)
        };
        let adopt = |u: &Unmanaged| {
            let mut g = env.borrow_mut();
            let (rec, store, sim) = &mut *g;
            rec.adopt(store, &mut **sim as &mut dyn SimAdapter, u)
        };
        let ctx = Ctx {
            now: 1.0,
            bound: &bound,
            adopt: Some(&adopt),
        };
        for b in &w.recording.batches[self.cursor..] {
            if let Batch::Records(rs) = b {
                for r in rs {
                    self.notes.extend(self.roster.fold(r, &ctx));
                }
            }
        }
        self.cursor = w.recording.batches.len();
    }

    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.w.tick().expect("tick");
            self.fold();
            self.w.rec.set_subscribed(self.roster.occupied_units());
        }
    }

    fn seat(&mut self, conn: u32, ucid: &str, slot: &str, group: u64) -> u64 {
        self.w.sim.connect_client(conn, ucid, "P", 2);
        self.w.sim.client_slot(conn, 2, slot);
        self.ticks(2);
        let o = self.w.sim.birth_client_unit_in(slot, group, "F-16C_50");
        self.ticks(3);
        o
    }
}

#[test]
fn units_of_one_group_share_one_group_binding_and_are_not_reconciled() {
    let mut p = Probe::new();
    let a = p.seat(2, "ucid-a", "10", 77);
    let b = p.seat(3, "ucid-b", "11", 77);
    p.ticks(30);
    let (ua, ga) = p.w.sim.binding_of(a).expect("row");
    let (ub, gb) = p.w.sim.binding_of(b).expect("row");
    assert!(ua != 0 && ub != 0 && ua != ub);
    assert_eq!(ga, gb, "one group, one group entity");
    // A unit write and a group write for each unit, none repeated.
    assert_eq!(p.w.sim.binds(), 4);
    assert_eq!(p.w.rec.adopted().len(), 2);
    assert!(p.w.rec.is_adopted(nanoprobe_sidecar::ids::EntityId(ga)));
    // Not in the store and not in the managed binding.
    assert!(
        p.w.store
            .unit(nanoprobe_sidecar::ids::EntityId(ua))
            .is_none()
    );
    assert!(!p.w.rec.binding().contains_key(&a));
    assert!(p.w.sim.subscribed().contains(&a) && p.w.sim.subscribed().contains(&b));
    // The IDs are not any the store gives out later.
    let next = p.w.store.adopt_id().expect("id");
    assert!(next.0 > ua.max(ub).max(ga));
}

#[test]
fn a_unit_that_leaves_is_forgotten_and_a_new_generation_drops_every_adoption() {
    let mut p = Probe::new();
    let a = p.seat(2, "ucid-a", "10", 77);
    assert_eq!(p.w.rec.adopted().len(), 1);
    p.w.sim.kill(a);
    p.ticks(3);
    assert!(p.w.rec.adopted().is_empty());

    let b = p.seat(3, "ucid-b", "11", 78);
    assert_eq!(p.w.rec.adopted().len(), 1);
    p.w.sim.load_mission();
    p.ticks(40);
    assert!(p.w.rec.adopted().is_empty(), "the generation changed");
    assert!(p.roster.occupied_units().is_empty());
    let _ = b;
}

#[test]
fn a_sidecar_restart_reads_the_adoption_back_from_the_agent() {
    let mut p = Probe::new();
    let a = p.seat(2, "ucid-a", "10", 77);
    let (unit, group) = p.w.sim.binding_of(a).expect("row");
    let binds = p.w.sim.binds();
    p.w.apply(&Op::SidecarRestart).expect("restart");
    p.ticks(40);
    // The enumeration after the HELLO carries the binding the agent holds.
    let row = p.w.rec.adopted().get(&a).copied().expect("adopted again");
    assert_eq!((row.unit.0, row.group.0), (unit, group));
    assert_eq!(p.w.sim.binds(), binds, "nothing is bound twice");
    // A new adoption never reuses the IDs the agent holds.
    let next = p.w.store.adopt_id().expect("id");
    assert!(next.0 > unit.max(group));
}

#[test]
fn a_binding_the_agent_lost_is_written_again() {
    let mut p = Probe::new();
    let a = p.seat(2, "ucid-a", "10", 77);
    let ids = p.w.sim.binding_of(a).expect("row");
    let binds = p.w.sim.binds();
    // The agent's binding is cleared as a campaign change clears it; the
    // next enumeration carries the rows with no entity.
    let own = p.w.store.campaign().0;
    p.w.sim.set_campaign(own + 1);
    p.w.sim.set_campaign(own);
    assert_eq!(p.w.sim.binding_of(a), Some((0, 0)));
    // Two enumerations must agree before the row is bound again.
    p.ticks(160);
    assert_eq!(p.w.sim.binding_of(a), Some(ids), "bound again, same IDs");
    assert_eq!(p.w.sim.binds(), binds + 2);
}
