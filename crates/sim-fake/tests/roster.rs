//! Players and moderation of step S16 through the reconciler and the fake
//! sim: keys, the roster and its occupancy join, menu candidates, and
//! the `MODERATE`, `PLAYERS` and `BANS` commands.

use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::players::{Ctx, Keyring, Note, Roster};
use nanoprobe_sidecar::replay::Batch;
use nanoprobe_sidecar::sim::wire::{Body, lv, moderation, opcode};
use nanoprobe_sidecar::store::{LostPolicy, MarkerSpec, Outcome, scope};
use nanoprobe_sim_fake::harness::World;
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::result as rc;
use nanoprobe_transport::records::Menu;

const SECRET: [u8; 32] = [9; 32];

/// A world and a roster that reads every record the world's reconciler read.
struct Probe {
    w: World,
    roster: Roster,
    cursor: usize,
    notes: Vec<Note>,
}

impl Probe {
    fn new(groups: usize) -> Probe {
        let mut w = World::new(groups, 0, LostPolicy::Report);
        w.settle().expect("settles");
        let mut p = Probe {
            w,
            roster: Roster::new(Keyring::from_secret(SECRET)),
            cursor: 0,
            notes: Vec::new(),
        };
        p.fold();
        p
    }

    fn fold(&mut self) {
        let binding = self.w.rec.binding();
        let bound = |o: u64| binding.get(&o).map(|b| (b.unit.0, b.group.0));
        let ctx = Ctx {
            now: 100.0,
            bound: &bound,
            adopt: None,
        };
        for b in &self.w.recording.batches[self.cursor..] {
            if let Batch::Records(rs) = b {
                for r in rs {
                    self.notes.extend(self.roster.fold(r, &ctx));
                }
            }
        }
        self.cursor = self.w.recording.batches.len();
    }

    fn ticks(&mut self, n: usize) {
        for _ in 0..n {
            self.w.tick().expect("tick");
            self.fold();
        }
    }

    fn take(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }

    fn key(&self, ucid: &str) -> String {
        self.roster.keyring().key_of(ucid)
    }

    /// Send a `MODERATE` for a player the roster knows, as the API does, and
    /// run until its result is read.
    fn moderate(&mut self, body: Body) -> Outcome {
        let cmd = self
            .w
            .rec
            .moderate(&mut self.w.store, &mut self.w.sim, body)
            .expect("sent");
        self.ticks(4);
        self.w.rec.effect_result(cmd).expect("a result").outcome
    }
}

fn moderate(action: u32, player: u32, ucid: &str, slot: &str, side: u32) -> Body {
    Body::Moderate {
        action,
        player,
        side,
        seconds: 0.0,
        ucid: ucid.into(),
        slot: slot.into(),
        reason: "because".into(),
    }
}

#[test]
fn keys_are_stable_across_restarts_and_differ_by_secret() {
    let a = Keyring::from_secret(SECRET);
    let b = Keyring::from_secret(SECRET);
    let c = Keyring::from_secret([1; 32]);
    assert_eq!(a.key_of("ucid-1"), b.key_of("ucid-1"));
    assert_ne!(a.key_of("ucid-1"), c.key_of("ucid-1"));
    assert_ne!(a.key_of("ucid-1"), a.key_of("ucid-2"));
    let k = a.key_of("ucid-1");
    assert_eq!(k.len(), 32);
    assert!(
        k.bytes()
            .all(|x| x.is_ascii_digit() || (b'a'..=b'f').contains(&x))
    );

    // A roster that restarts with the same secret names the player the same.
    let mut p = Probe::new(0);
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.ticks(3);
    assert_eq!(p.roster.key_on(2), Some(k.as_str()));
    assert!(p.roster.player(&k).unwrap().connected);
}

#[test]
fn connect_chat_rename_and_disconnect_become_notes() {
    let mut p = Probe::new(0);
    let k = p.key("ucid-1");
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.ticks(3);
    p.w.sim.client_chat(2, "hello");
    p.w.sim.rename_client(2, "Anna");
    p.w.sim.client_slot(2, 2, "");
    p.ticks(3);
    p.w.sim.disconnect_client(2);
    p.ticks(3);
    let notes = p.take();
    assert!(
        matches!(&notes[0], Note::Connect { key, name, side: 2 } if *key == k && name == "Ann")
    );
    assert!(matches!(&notes[1], Note::Chat { text, name, .. } if text == "hello" && name == "Ann"));
    assert!(
        matches!(&notes[2], Note::Rename { from, name, .. } if from == "Ann" && name == "Anna")
    );
    assert!(matches!(&notes[3], Note::Disconnect { name, .. } if name == "Anna"));
    assert_eq!(notes.len(), 4, "{notes:?}");
    assert!(!p.roster.player(&k).unwrap().connected);
}

#[test]
fn a_slot_and_a_birth_join_in_either_order_and_a_respawn_attaches() {
    // Slot first, then the unit.
    let mut p = Probe::new(0);
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.w.sim.client_slot(2, 2, "40");
    p.ticks(3);
    assert_eq!(p.roster.held_unmatched(), 1);
    let u1 = p.w.sim.birth_client_unit("40");
    p.ticks(3);
    let k = p.key("ucid-1");
    assert_eq!(p.roster.players_of(u1), vec![k.clone()]);
    assert_eq!(p.roster.held_unmatched(), 0);

    // The unit dies and the player respawns in the held slot.
    p.w.sim.kill(u1);
    p.ticks(3);
    assert!(p.roster.players_of(u1).is_empty());
    let u2 = p.w.sim.birth_client_unit("40");
    p.ticks(3);
    assert_eq!(p.roster.players_of(u2), vec![k.clone()]);

    // The unit before the slot, for another player.
    let u3 = p.w.sim.birth_client_unit("41");
    p.w.sim.connect_client(3, "ucid-2", "Bo", 1);
    p.ticks(3);
    p.w.sim.client_slot(3, 1, "41");
    p.ticks(3);
    assert_eq!(p.roster.players_of(u3), vec![p.key("ucid-2")]);
    assert_eq!(p.roster.counts().unmatched, 0);

    // A slot change ends the seat.
    p.w.sim.client_slot(2, 2, "");
    p.ticks(3);
    assert!(p.roster.players_of(u2).is_empty());
    let enters = p
        .notes
        .iter()
        .filter(|n| matches!(n, Note::Enter { .. }))
        .count();
    let leaves = p
        .notes
        .iter()
        .filter(|n| matches!(n, Note::Leave { .. }))
        .count();
    assert_eq!((enters, leaves), (3, 2));
    // The units are not the campaign's: the binding holds no entity.
    assert_eq!(p.roster.counts().unbound, 3);
    assert!(p.roster.occupied_units().is_empty());
}

#[test]
fn an_unmatched_slot_is_counted_not_guessed() {
    let mut p = Probe::new(0);
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.w.sim.client_slot(2, 2, "99");
    // A birth with another mission ID, and one with none.
    p.w.sim.birth_client_unit("98");
    let o = p.w.sim.birth_client_unit("97");
    p.w.sim.birth_event(o, None);
    p.ticks(4);
    assert!(p.roster.players_of(o).is_empty());
    assert_eq!(p.roster.held_unmatched(), 1);
    p.w.sim.disconnect_client(2);
    p.ticks(3);
    assert_eq!(p.roster.counts().unmatched, 1);
}

#[test]
fn an_occupied_managed_unit_is_subscribed_and_the_menu_names_candidates() {
    let mut p = Probe::new(1);
    let (object, _, entity) =
        p.w.sim
            .objects()
            .into_iter()
            .find(|(_, _, e)| *e != 0)
            .expect("a managed unit");
    let group = p.w.store.groups().next().unwrap().id;
    let (item, _) =
        p.w.store
            .put_marker(
                EntityId(0),
                scope::GROUP,
                group.0,
                MarkerSpec::MenuItem {
                    parent: EntityId(0),
                    submenu: false,
                    text: "Go".into(),
                },
            )
            .unwrap();
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.w.sim.connect_client(3, "ucid-2", "Bo", 1);
    p.w.sim.client_slot(2, 2, "m1");
    p.ticks(3);
    p.w.sim.birth_event(object, Some("m1"));
    p.ticks(3);
    let k1 = p.key("ucid-1");
    let occ: Vec<_> = p.roster.occupancies().collect();
    assert_eq!((occ.len(), occ[0].unit, occ[0].group), (1, entity, group.0));
    assert_eq!(p.roster.counts().unbound, 0);

    // The reconciler writes SUBSCRIBE for the occupied unit.
    p.w.rec.set_subscribed(p.roster.occupied_units());
    p.ticks(3);
    assert!(p.w.sim.subscribed().contains(&object));

    // The group's menu item: no player named, so the occupants are candidates.
    let menu = |player| Menu {
        entity: item.0,
        target: group.0,
        scope: scope::GROUP,
        player,
    };
    assert_eq!(p.roster.candidates(&menu(0)), vec![k1.clone()]);
    assert_eq!(p.roster.selector(&menu(0)), None);
    // DCS names a player: that one, as the selector.
    assert_eq!(p.roster.selector(&menu(3)), Some(p.key("ucid-2")));
    // A coalition item: the coalition's connected players. An ALL item: all.
    let blue = Menu {
        scope: scope::COALITION,
        target: 2,
        ..menu(0)
    };
    assert_eq!(p.roster.candidates(&blue), vec![k1.clone()]);
    let all = Menu {
        scope: scope::ALL,
        target: 0,
        ..menu(0)
    };
    assert_eq!(p.roster.candidates(&all).len(), 2);

    // Leaving the slot drops the subscription.
    p.w.sim.client_slot(2, 2, "");
    p.ticks(2);
    p.w.rec.set_subscribed(p.roster.occupied_units());
    p.ticks(3);
    assert!(p.w.sim.subscribed().is_empty());
}

#[test]
fn every_moderation_call_reaches_the_player_and_ucid_mismatch_is_not_bound() {
    let mut p = Probe::new(0);
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.w.sim.connect_client(3, "ucid-2", "Bo", 1);
    p.ticks(4);

    // A move to a slot the mission lists, then to spectators.
    p.w.sim.offer_slot("55", 2);
    assert_eq!(
        p.moderate(moderate(moderation::SLOT, 2, "ucid-1", "55", 2)),
        Outcome::Applied
    );
    assert_eq!(p.w.sim.clients()[&2].slot, "55");
    assert_eq!(p.roster.player(&p.key("ucid-1")).unwrap().slot, "55");
    assert_eq!(
        p.moderate(moderate(moderation::SLOT, 2, "ucid-1", "", 0)),
        Outcome::Applied
    );
    assert_eq!(p.w.sim.clients()[&2].slot, "");

    // The connection ID belongs to someone else: NOT_BOUND, nothing changes.
    let cmd =
        p.w.rec
            .moderate(
                &mut p.w.store,
                &mut p.w.sim,
                moderate(moderation::KICK, 3, "ucid-1", "", 0),
            )
            .unwrap();
    p.ticks(4);
    let r = p.w.rec.effect_result(cmd).unwrap();
    assert_eq!(r.outcome, Outcome::Refused);
    assert_eq!(u64::from(r.result.as_ref().unwrap().code), rc::NOT_BOUND);
    assert!(p.w.sim.clients().contains_key(&3));
    // A connection that is not connected is NOT_BOUND too.
    assert_eq!(
        p.moderate(moderate(moderation::KICK, 9, "ucid-1", "", 0)),
        Outcome::Refused
    );

    // A kick removes the player, and the roster sees the disconnect.
    assert_eq!(
        p.moderate(moderate(moderation::KICK, 3, "ucid-2", "", 0)),
        Outcome::Applied
    );
    assert!(!p.w.sim.clients().contains_key(&3));
    assert!(!p.roster.player(&p.key("ucid-2")).unwrap().connected);

    // A ban of a connected player removes them and holds them out.
    let mut ban = moderate(moderation::BAN, 2, "ucid-1", "", 0);
    if let Body::Moderate { seconds, .. } = &mut ban {
        *seconds = 600.0;
    }
    assert_eq!(p.moderate(ban), Outcome::Applied);
    assert!(!p.w.sim.connect_client(2, "ucid-1", "Ann", 2));
    // A ban of a ucid never seen is by ucid, with player 0.
    assert_eq!(
        p.moderate(moderate(moderation::BAN, 0, "ucid-9", "", 0)),
        Outcome::Applied
    );
    assert_eq!(p.w.sim.bans().len(), 2);

    // The ban list: ucid, name, end and reason only.
    let cmd = p.w.rec.query_bans(&mut p.w.store, &mut p.w.sim).unwrap();
    p.ticks(4);
    let r = p.w.rec.effect_result(cmd).unwrap().result.clone().unwrap();
    let rows = lv::items(&r.value);
    assert_eq!(rows.len(), 2);
    let Lv::Table(first) = rows[0] else {
        panic!("a table")
    };
    assert_eq!(first.len(), 4);
    assert_eq!(lv::str_at(rows[0], "name").as_deref(), Some("Ann"));
    assert_eq!(lv::str_at(rows[1], "name").as_deref(), Some(""));

    // Unban: applied for a ucid on the list, refused for one that is not.
    assert_eq!(
        p.moderate(moderate(moderation::UNBAN, 0, "ucid-1", "", 0)),
        Outcome::Applied
    );
    assert_eq!(
        p.moderate(moderate(moderation::UNBAN, 0, "ucid-1", "", 0)),
        Outcome::Refused
    );
    assert!(p.w.sim.connect_client(2, "ucid-1", "Ann", 2));

    // The agent refuses what a handler cannot take: player 1, no ucid.
    for body in [
        moderate(moderation::KICK, 1, "ucid-1", "", 0),
        moderate(moderation::KICK, 2, "", "", 0),
        moderate(7, 2, "ucid-1", "", 0),
    ] {
        assert_eq!(p.moderate(body), Outcome::Rejected);
    }
    assert_eq!(p.w.sim.ledger().moderations.len(), 6);
    assert_eq!(p.w.sim.ledger().bans_asked, 1);
}

#[test]
fn moderation_is_an_effect_sent_once() {
    let mut p = Probe::new(0);
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.ticks(3);
    assert_eq!(
        p.moderate(moderate(moderation::KICK, 2, "ucid-1", "", 0)),
        Outcome::Applied
    );
    p.w.apply(&nanoprobe_sim_fake::harness::Op::SidecarRestart)
        .unwrap();
    p.w.apply(&nanoprobe_sim_fake::harness::Op::LoadMission)
        .unwrap();
    p.w.settle().unwrap();
    let sent = p
        .w
        .store
        .entries()
        .iter()
        .filter(|e| matches!(e, nanoprobe_sidecar::store::Entry::Command(c) if c.opcode == opcode::MODERATE))
        .count();
    assert_eq!(sent, 1);
    assert_eq!(p.w.sim.ledger().moderations.len(), 1);
}

#[test]
fn a_players_query_after_a_restart_rebuilds_who_is_connected_and_where() {
    let mut p = Probe::new(0);
    p.w.sim.connect_client(2, "ucid-1", "Ann", 2);
    p.w.sim.connect_client(3, "ucid-2", "Bo", 1);
    p.w.sim.client_slot(3, 1, "41");
    p.ticks(4);
    // A new roster with the same secret knows no one, until PLAYERS answers.
    let mut fresh = Roster::new(Keyring::from_secret(SECRET));
    let cmd = p.w.rec.query_players(&mut p.w.store, &mut p.w.sim).unwrap();
    p.ticks(4);
    assert_eq!(
        p.w.rec.effect_result(cmd).unwrap().outcome,
        Outcome::Applied
    );
    let binding = p.w.rec.binding();
    let bound = |o: u64| binding.get(&o).map(|b| (b.unit.0, b.group.0));
    let ctx = Ctx {
        now: 1.0,
        bound: &bound,
        adopt: None,
    };
    let mut notes = Vec::new();
    for b in &p.w.recording.batches {
        if let Batch::Records(rs) = b {
            for r in rs {
                if u64::from(r.kind) == nanoprobe_transport::layout::kind::RESULT {
                    notes.extend(fresh.fold(r, &ctx));
                }
            }
        }
    }
    assert_eq!(notes.len(), 2);
    assert_eq!(fresh.conn_of(&p.key("ucid-1")), Some(2));
    assert_eq!(fresh.player(&p.key("ucid-2")).unwrap().slot, "41");
    assert_eq!(fresh.held_unmatched(), 1);
    assert_eq!(fresh.counts().resyncs, 1);
    assert_eq!(p.w.sim.ledger().players_asked, 1);
}
