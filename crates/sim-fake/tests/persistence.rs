//! Persistence and restore on the fake sim (spec/persistence.md): the store
//! file, restore after a reload and after DCS dies, the paused restore
//! burst, campaign switching, a death pending at a restore, and schema
//! migration.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use nanoprobe_sidecar::campaigns::{BackupReason, Campaigns};
use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::intents::{Intent, REACHED_M, Waypoint};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::sim::wire::opcode;
use nanoprobe_sidecar::stock::Stock;
use nanoprobe_sidecar::store::{
    CampaignRecord, CampaignState, DiskError, Group, Kind, LostPolicy, Migration, REVISION_RESERVE,
    Store, TierPeriod, Unit, schema_of,
};
use nanoprobe_sim_fake::harness::{CAMPAIGN, Op, Schedule, THEATER, World, new_store_for};
use nanoprobe_sim_fake::{Config, Left};
use nanoprobe_transport::layout::result as rc;
use nanoprobe_transport::records::Rates;

fn record(name: &str) -> CampaignRecord {
    CampaignRecord {
        name: name.into(),
        state: CampaignState::Active,
        theaters: vec![THEATER.into()],
        theater: THEATER.into(),
        clock_ms: Some(1_700_000_000_000),
        created_ms: 1,
        component_schemas: BTreeMap::new(),
        outcome: None,
    }
}

fn periods() -> Vec<TierPeriod> {
    vec![
        TierPeriod {
            tier: "position".into(),
            target_s: 1.0,
            achieved_s: 1.2,
        },
        TierPeriod {
            tier: "vitals".into(),
            target_s: 60.0,
            achieved_s: 75.0,
        },
    ]
}

/// The store file at `path` as a second reader sees it while its writer
/// still has it open: a copy, opened, so the reader writes nothing to it.
fn read_back(path: &Path) -> (tempfile::TempDir, Store) {
    let d = temp();
    let copy = d.path().join("copy.sqlite");
    nanoprobe_sidecar::store::backup_file(path, &copy).expect("copy");
    let s = Store::open(&copy, 10_000).expect("open");
    (d, s)
}

/// The sidecar process dies and starts again from its store file.
fn reopen(w: &mut World, path: &Path) {
    let s = Store::open(path, 10_000).expect("reopen");
    w.sidecar_of(s);
}

fn units(s: &Store) -> Vec<Unit> {
    s.groups()
        .flat_map(|g| g.members.iter().map(|m| s.unit(*m).expect("unit").clone()))
        .collect()
}

/// Every table of `a` equals the same table of `b`.
fn same(a: &Store, b: &Store) {
    same_world(a, b);
    assert_eq!(a.record(), b.record());
    assert_eq!(a.last_checkpoint(), b.last_checkpoint());
}

/// [`same`] but for the campaign record and the checkpoints, which a
/// suspend changes.
fn same_world(a: &Store, b: &Store) {
    assert_eq!(a.campaign(), b.campaign());
    // A store read while its writer still has it open moves the revision
    // past what the writer may have handed out (an unclean open).
    assert!(
        b.revision() == a.revision() || b.revision() == a.revision() + REVISION_RESERVE,
        "{} {}",
        a.revision(),
        b.revision()
    );
    assert_eq!(a.epoch(), b.epoch());
    assert_eq!(a.loaded(), b.loaded());
    let ga: Vec<&Group> = a.groups().collect();
    let gb: Vec<&Group> = b.groups().collect();
    assert_eq!(ga, gb);
    assert_eq!(units(a), units(b));
    assert_eq!(a.entries(), b.entries());
    let ea: Vec<_> = a.extensions().collect();
    let eb: Vec<_> = b.extensions().collect();
    assert_eq!(ea, eb);
    assert_eq!(a.countries(), b.countries());
    assert_eq!(a.rates(), b.rates());
    let oa: Vec<_> = a.desired_owners().collect();
    let ob: Vec<_> = b.desired_owners().collect();
    assert_eq!(oa, ob);
    let sa: Vec<_> = a.desired_stocks().collect();
    let sb: Vec<_> = b.desired_stocks().collect();
    assert_eq!(sa, sb);
    // The next EntityId, the queued route events and the model time of the
    // last samples, through copies.
    let (mut ca, mut cb) = (a.clone(), b.clone());
    assert_eq!(ca.take_route_events(), cb.take_route_events());
    assert_eq!(
        ca.checkpoint(0, Vec::new()).expect("cp").model_time,
        cb.checkpoint(0, Vec::new()).expect("cp").model_time
    );
    let spec = new_store_for(1, 1, 0);
    let g = spec.groups().next().expect("g");
    let one = |s: &mut Store| {
        s.create_group(nanoprobe_sidecar::store::NewGroup {
            theater: g.theater.clone(),
            country: g.country,
            category: g.category,
            coalition: g.coalition,
            units: vec![nanoprobe_sidecar::store::NewUnit::new(
                "M-113",
                Default::default(),
            )],
            authority: g.authority,
            label: String::new(),
            extra: nanoprobe_transport::codec::Lv::Nil,
        })
        .expect("ids")
    };
    assert_eq!(one(&mut ca), one(&mut cb), "the next EntityId");
}

/// GROUP_ORDER commands sent to a group that already held that order: the
/// agent answers them `AT_REVISION`. DCS never applies one twice (the agent
/// keeps the applied revision), so this is the sidecar issuing it twice.
fn orders_issued_twice(w: &World) -> Vec<(u64, u32, EntityId, u64)> {
    let journalled: BTreeMap<(u64, u64, u64), _> = w
        .store
        .entries()
        .iter()
        .filter_map(|e| match e {
            nanoprobe_sidecar::store::Entry::Command(j) => {
                Some(((j.agent_boot, j.epoch, j.seq), j))
            }
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for (key, code) in &w.sim.ledger().results {
        let Some(j) = journalled.get(key) else {
            continue;
        };
        if j.opcode == opcode::GROUP_ORDER && u64::from(*code) == rc::AT_REVISION {
            out.push((j.agent_boot, j.mission_gen, j.entity, j.rev));
        }
    }
    out.sort();
    out
}

/// GROUP_ORDER results the agent gave for commands in the journal: the
/// order checks below look at these, so a test asserts there are some.
fn order_results(w: &World) -> usize {
    let journalled: BTreeSet<(u64, u64, u64)> = w
        .store
        .entries()
        .iter()
        .filter_map(|e| match e {
            nanoprobe_sidecar::store::Entry::Command(j) if j.opcode == opcode::GROUP_ORDER => {
                Some((j.agent_boot, j.epoch, j.seq))
            }
            _ => None,
        })
        .collect();
    w.sim
        .ledger()
        .results
        .keys()
        .filter(|k| journalled.contains(k))
        .count()
}

/// GROUP_ORDER commands DCS applied more than once for one order revision
/// in one generation of one agent boot.
fn orders_applied_twice(w: &World) -> Vec<(u64, u32, EntityId, u64)> {
    let journalled: BTreeMap<(u64, u64, u64), _> = w
        .store
        .entries()
        .iter()
        .filter_map(|e| match e {
            nanoprobe_sidecar::store::Entry::Command(j) => {
                Some(((j.agent_boot, j.epoch, j.seq), j))
            }
            _ => None,
        })
        .collect();
    let mut seen: BTreeMap<(u64, u32, EntityId, u64), u32> = BTreeMap::new();
    for (key, code) in &w.sim.ledger().results {
        if let Some(j) = journalled.get(key)
            && j.opcode == opcode::GROUP_ORDER
            && u64::from(*code) == rc::APPLIED
        {
            *seen
                .entry((j.agent_boot, j.mission_gen, j.entity, j.rev))
                .or_default() += 1;
        }
    }
    seen.into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(k, _)| k)
        .collect()
}

/// The fake's groups of `campaign` by entity: none twice.
fn no_duplicate(w: &World, campaign: u64) {
    let mut entities = BTreeSet::new();
    for (_, name, _) in w.sim.platform_groups(campaign) {
        assert!(entities.insert(name.clone()), "{name} twice in DCS");
    }
    let mut unit_entities = BTreeSet::new();
    for (_, c, e) in w.sim.objects() {
        if c == campaign && e != 0 {
            assert!(unit_entities.insert(e), "unit {e} twice in DCS");
        }
    }
}

fn ticks(w: &mut World, n: usize) {
    for _ in 0..n {
        w.tick().expect("tick");
    }
}

fn first_group(w: &World) -> EntityId {
    w.store
        .groups()
        .find(|g| g.kind == Kind::Group)
        .expect("a group")
        .id
}

fn route(points: &[(f64, f64)]) -> Intent {
    Intent::Route {
        waypoints: points.iter().map(|&(x, z)| Waypoint { x, z }).collect(),
        speed: 10.0,
        on_road: false,
    }
}

/// Run frames until the group's lead is at its route's last waypoint.
fn arrive(w: &mut World, id: EntityId, last: (f64, f64)) {
    for _ in 0..2_000 {
        let g = w.store.group(id).expect("group");
        let Some(Intent::Route { waypoints, .. }) = &g.order else {
            panic!("a route");
        };
        let lead = g
            .members
            .iter()
            .map(|m| w.store.unit(*m).expect("unit"))
            .find(|u| !u.dead)
            .expect("a living member");
        let (dx, dz) = (lead.state.at.x - last.0, lead.state.at.z - last.1);
        if g.progress == waypoints.len() && (dx * dx + dz * dz).sqrt() <= REACHED_M {
            return;
        }
        w.tick().expect("tick");
    }
    panic!("the convoy never arrived");
}

fn temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

#[test]
fn a_store_reads_back_exactly_as_written() {
    for seed in 0..12u64 {
        let d = temp();
        let path = d.path().join("store.sqlite");
        let mut w = World::new_late(3, 1, LostPolicy::Respawn, seed);
        w.store.set_extension("probe", "return 1");
        w.store.set_airbase_owner(THEATER, "Batumi", 2);
        let mut stock = Stock::default();
        stock
            .weapon
            .insert("weapons.missiles.AIM_120C".into(), 12.0);
        stock.liquids.insert(0, 5_000.0);
        w.store.set_stock(THEATER, "Batumi", &stock);
        w.store
            .set_countries(Some(BTreeMap::from([(2, 2), (0, 1)])));
        w.store.set_rates(Rates {
            position_s: 1.0,
            vitals_s: 60.0,
            ammo_s: 5.0,
            warehouse_s: 60.0,
            enumeration_s: 30.0,
        });
        w.store.save_as(&path, record("a")).expect("save");
        let mut s = Schedule::new(seed);
        for i in 0..150 {
            let op = s.op_late();
            w.apply(&op).expect("op");
            if i % 25 == 24 {
                w.store.commit().expect("commit");
                same(&w.store, &read_back(&path).1);
            }
        }
        w.store.commit().expect("commit");
        same(&w.store, &read_back(&path).1);
    }
}

#[test]
fn a_removed_extension_is_absent_after_a_reopen() {
    let d = temp();
    let path = d.path().join("store.sqlite");
    let mut w = World::new_late(3, 1, LostPolicy::Respawn, 0);
    w.store.set_extension("probe", "return 1");
    w.store.save_as(&path, record("a")).expect("save");
    assert!(w.store.remove_extension("probe"));
    w.store.commit().expect("commit");
    let (_d, mut s) = read_back(&path);
    assert!(s.extension("probe").is_none());
    assert!(s.set_extension("probe", "return 1") > 1);
}

#[test]
fn a_removal_the_agent_has_not_confirmed_is_sent_again_after_a_reopen() {
    let d = temp();
    let path = d.path().join("store.sqlite");
    let mut w = World::new(0, 0, LostPolicy::Report);
    w.settle().expect("settles");
    w.store.set_extension("probe", "return {}");
    w.store.save_as(&path, record("a")).expect("save");
    ticks(&mut w, 10);
    assert_eq!(w.sim.extension_rev("probe"), 1);
    assert!(w.store.remove_extension("probe"));
    w.store.commit().expect("commit");
    // The sidecar stops before the agent is told; the agent keeps its boot.
    reopen(&mut w, &path);
    assert_eq!(w.store.extension_removals().count(), 1);
    ticks(&mut w, 20);
    assert_eq!(w.sim.extension_rev("probe"), 0);
    assert_eq!(w.sim.ledger().extension_removals, vec!["probe".to_owned()]);
    assert_eq!(w.store.extension_removals().count(), 0);
}

/// One seed of the deterministic simulation's late schedule, 300 operations
/// then settling. With `file`, every sidecar restart reads its store back
/// from the file and keeps nothing else, and writes happen only where the
/// store makes them and at every third operation, so a restart loses what
/// came after the last. Without it, restarts keep the store in memory.
fn run_persistent(seed: u64, file: bool) -> Result<(), String> {
    let d = temp();
    let path = d.path().join("store.sqlite");
    let policy = [LostPolicy::Report, LostPolicy::Respawn, LostPolicy::Dead][(seed % 3) as usize];
    let mut w = World::new_late(3, 1, policy, seed);
    if file {
        w.store.save_as(&path, record("a")).expect("save");
    }
    let mut s = Schedule::new(seed ^ 0x5eed);
    for i in 0..300 {
        let op = s.op_late();
        let r = match op {
            Op::SidecarRestart if file => {
                reopen(&mut w, &path);
                w.tick()
            }
            Op::SidecarCrashInWrite(_) if file => w.apply(&op).map(|()| reopen(&mut w, &path)),
            _ => w.apply(&op),
        };
        r.map_err(|v| format!("op {i} {op:?}: {}", v.0))?;
        if file && i % 3 == 0 {
            w.store.commit().expect("commit");
        }
    }
    w.settle().map_err(|v| format!("settling: {}", v.0))?;
    no_duplicate(&w, CAMPAIGN);
    Ok(())
}

#[test]
fn seeded_schedules_hold_with_the_store_read_back_from_its_file() {
    for seed in 0..32 {
        if let Err(e) = run_persistent(seed, true) {
            panic!("seed {seed}: {e}");
        }
    }
}

/// The sweep in the results document: `NANOPROBE_SEEDS` seeds (default
/// 1,000), each with the store read back from its file and with it kept in
/// memory. Fails when a seed fails with the file and passes in memory;
/// prints every failing seed.
#[test]
#[ignore = "a sweep of about a minute in a release build"]
fn seed_sweep_compares_file_and_memory() {
    let seeds: u64 = std::env::var("NANOPROBE_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000);
    let mut file_only = Vec::new();
    for seed in 0..seeds {
        let (file, memory) = (run_persistent(seed, true), run_persistent(seed, false));
        if file.is_err() || memory.is_err() {
            println!("seed {seed}: file {file:?}, memory {memory:?}");
        }
        if file.is_err() && memory.is_ok() {
            file_only.push(seed);
        }
    }
    assert!(
        file_only.is_empty(),
        "fail only with the file: {file_only:?}"
    );
}

/// After a reload and after DCS dies, the world returns from the store, the
/// convoy continues to its destination, no group is duplicated and no order
/// is applied twice.
#[test]
fn the_world_returns_after_a_reload_and_after_dcs_dies() {
    for sidecar_dies_too in [false, true] {
        let d = temp();
        let path = d.path().join("store.sqlite");
        // Slow enough that sampled positions see each waypoint reached.
        let mut w = World::with_config(
            3,
            1,
            LostPolicy::Report,
            Config {
                event_queue: 6,
                step_m: 5.0,
                ..Config::default()
            },
        );
        w.store.save_as(&path, record("a")).expect("save");
        w.settle().expect("settle");
        let convoy = first_group(&w);
        let legs = [(0.0, 600.0), (600.0, 600.0), (600.0, 1_200.0)];
        w.store.set_order(convoy, route(&legs)).expect("order");
        ticks(&mut w, 150);
        w.store.checkpoint(10_000, periods()).expect("checkpoint");
        let before = w.store.group(convoy).expect("g").progress;
        let g = w.store.group(convoy).expect("g");
        let lead = w.store.unit(g.members[0]).expect("u");
        assert!(
            before >= 1,
            "the convoy moved: {:?} {:?} {:?}",
            g.state,
            lead.state.at,
            w.sim.applied_order(convoy.0)
        );

        // A reload: the mission loads again under the running sidecar.
        w.apply(&Op::LoadMission).expect("load");
        w.settle().expect("after the reload");
        assert_eq!(w.store.group(convoy).expect("g").state, State::Live);
        assert!(w.store.group(convoy).expect("g").progress >= before);
        no_duplicate(&w, CAMPAIGN);
        ticks(&mut w, 10);
        w.store.checkpoint(20_000, periods()).expect("checkpoint");
        let progress = w.store.group(convoy).expect("g").progress;
        let parked: Vec<Unit> = units(&w.store)
            .into_iter()
            .filter(|u| u.group != convoy && !u.dead)
            .collect();

        // DCS dies; the host starts it again.
        w.apply(&Op::Crash).expect("crash");
        if sidecar_dies_too {
            reopen(&mut w, &path);
        }
        w.apply(&Op::RestartDcs).expect("restart");
        w.settle().expect("after DCS restarts");
        assert_eq!(w.store.group(convoy).expect("g").state, State::Live);
        assert!(w.store.group(convoy).expect("g").progress >= progress);
        // What does not move is back where the store had it.
        for u in &parked {
            let now = w.store.unit(u.id).expect("unit").state.at;
            let (dx, dz) = (now.x - u.state.at.x, now.z - u.state.at.z);
            assert!((dx * dx + dz * dz).sqrt() < 1.0, "{} moved", u.id);
        }
        no_duplicate(&w, CAMPAIGN);

        arrive(&mut w, convoy, legs[2]);
        w.settle().expect("settle");
        assert!(
            orders_issued_twice(&w).is_empty(),
            "{:?}",
            orders_issued_twice(&w)
        );
        assert!(
            orders_applied_twice(&w).is_empty(),
            "{:?}",
            orders_applied_twice(&w)
        );
        no_duplicate(&w, CAMPAIGN);
        assert!(order_results(&w) > 0);
        assert_eq!(w.sim.ledger().replacements, 0);
        let cp = Store::open(&path, 10_000)
            .expect("open")
            .last_checkpoint()
            .cloned()
            .expect("a checkpoint");
        assert_eq!(
            cp.achieved,
            periods(),
            "the periods are kept with the values"
        );
    }
}

/// While paused, a restore spawns every waiting group in one frame under the
/// burst, and a destroy and a re-add under the same name leave exactly one
/// group per entity after the resume.
#[test]
fn a_paused_restore_burst_leaves_no_duplicate_after_the_resume() {
    let d = temp();
    let path = d.path().join("store.sqlite");
    let mut w = World::new(8, 2, LostPolicy::Report);
    w.store.save_as(&path, record("a")).expect("save");
    w.settle().expect("settle");

    // The mission loads again and starts paused.
    w.sim.load_mission();
    for _ in 0..10 {
        if w.sim.running() {
            break;
        }
        w.sim.frame();
    }
    w.sim.pause();
    assert!(w.sim.paused());
    reopen(&mut w, &path);
    // All 10 spawns in one frame: the burst lifts the 4 commands a frame.
    let mut most = 0;
    for _ in 0..6 {
        let before = w.sim.ledger().spawns.len();
        w.tick().expect("tick");
        most = most.max(w.sim.ledger().spawns.len() - before);
    }
    assert_eq!(most, 10);
    assert!(
        w.sim.ledger().bursts.contains(&(true, true)),
        "{:?}",
        w.sim.ledger().bursts
    );
    let live = w.store.groups().filter(|g| g.state == State::Live).count();
    assert_eq!(live, 10);

    // Destroy and re-add two groups while paused.
    let ids: Vec<EntityId> = w
        .store
        .groups()
        .filter(|g| g.kind == Kind::Group)
        .map(|g| g.id)
        .take(2)
        .collect();
    for id in &ids {
        w.store
            .set_authority(*id, nanoprobe_sidecar::store::Authority::Campaign)
            .expect("authority");
    }
    ticks(&mut w, 6);
    assert!(!w.sim.pending_destroys().is_empty(), "the destroys wait");
    for id in &ids {
        assert_eq!(w.store.group(*id).expect("g").state, State::Virtual);
        w.store
            .set_authority(*id, nanoprobe_sidecar::store::Authority::Sim)
            .expect("authority");
    }
    // A sidecar restart in the middle of the pause.
    w.store.commit().expect("commit");
    reopen(&mut w, &path);
    ticks(&mut w, 6);
    for id in &ids {
        assert_eq!(w.store.group(*id).expect("g").state, State::Live);
    }
    assert!(w.sim.paused());
    // DCS holds the old objects and the re-added group at once until the
    // resume: the case the pending-destroy rule is for.
    let pending: BTreeSet<u64> = w
        .sim
        .pending_destroys()
        .into_iter()
        .map(|(_, e)| e)
        .collect();
    let alive: BTreeSet<u64> = w.units_in_dcs().into_iter().map(|(_, e)| e).collect();
    for id in &ids {
        for m in &w.store.group(*id).expect("g").members {
            assert!(pending.contains(&m.0) && alive.contains(&m.0), "{}", m.0);
        }
    }

    w.sim.resume();
    ticks(&mut w, 10);
    assert!(w.sim.pending_destroys().is_empty());
    w.settle().expect("after the resume");
    no_duplicate(&w, CAMPAIGN);
    assert_eq!(w.sim.platform_groups(CAMPAIGN).len(), 10);
    assert!(
        w.sim.ledger().bursts.iter().any(|(on, _)| !on),
        "the burst ends"
    );
    assert_eq!(w.sim.ledger().replacements, 0);
}

/// Suspend A, activate B, then A: each returns as it was left, and nothing
/// of one appears in the other.
#[test]
fn suspend_a_activate_b_then_a_and_each_returns_as_left() {
    const B: u64 = CAMPAIGN + 1;
    let d = temp();
    let c = Campaigns::new(d.path(), 10_000, 3).expect("campaigns");
    let a_id = CampaignId(CAMPAIGN);
    let b_id = CampaignId(B);

    // Campaign A, active, with a moving convoy and a dead unit.
    let mut w = World::new(3, 1, LostPolicy::Report);
    std::fs::create_dir_all(c.folder(a_id)).expect("folder");
    w.store
        .save_as(&c.store_path(a_id), record("a"))
        .expect("save a");
    w.settle().expect("settle");
    let convoy = first_group(&w);
    w.store
        .set_order(convoy, route(&[(0.0, 4_000.0), (4_000.0, 4_000.0)]))
        .expect("order");
    ticks(&mut w, 12);
    let victim = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| {
            *e != 0
                && !w
                    .store
                    .group(convoy)
                    .expect("g")
                    .members
                    .contains(&EntityId(*e))
        })
        .expect("a unit");
    w.sim.kill(victim.0);
    w.settle().expect("settle");
    assert!(w.store.unit(EntityId(victim.1)).expect("unit").dead);
    let a_left: Vec<Unit> = units(&w.store);
    let a_progress = w.store.group(convoy).expect("g").progress;
    w.store.set_rates(Rates {
        position_s: 3.0,
        vitals_s: 60.0,
        ammo_s: 5.0,
        warehouse_s: 60.0,
        enumeration_s: 30.0,
    });

    // Campaign B, created suspended, with its own groups under the same
    // EntityIds.
    let mut b = new_store_for(B, 2, 0);
    std::fs::create_dir_all(c.folder(b_id)).expect("folder");
    let mut rb = record("b");
    rb.state = CampaignState::Suspended;
    b.save_as(&c.store_path(b_id), rb).expect("save b");
    drop(b);

    // Suspend A, activate B.
    let a_before = w.store.clone();
    let a = std::mem::replace(&mut w.store, Store::new(a_id, 1));
    let mut active = Some(a);
    c.activate(&mut active, b_id, 30_000, periods())
        .expect("activate b");
    w.sidecar_of(active.take().expect("b"));
    // A's file holds A as it was left.
    let mut a_file = Store::open(&c.store_path(a_id), 10_000).expect("a");
    same_world(&a_before, &a_file);
    assert_eq!(a_file.record().expect("r").state, CampaignState::Suspended);
    a_file.close().expect("close");
    drop(a_file);
    w.apply(&Op::LoadMission).expect("load b's mission");
    w.settle().expect("b settles");
    assert!(
        w.sim.objects().iter().all(|(_, camp, _)| *camp != CAMPAIGN),
        "nothing of A in DCS while B is active"
    );
    assert_eq!(w.sim.platform_groups(B).len(), 2);
    ticks(&mut w, 5);
    let b_left: Vec<Unit> = units(&w.store);
    // Changes no write trigger catches: only the suspend writes them.
    let rates = |p: f64| Rates {
        position_s: p,
        vitals_s: 60.0,
        ammo_s: 5.0,
        warehouse_s: 60.0,
        enumeration_s: 30.0,
    };
    w.store.set_rates(rates(2.0));

    // Back to A.
    let b_before = w.store.clone();
    let b = std::mem::replace(&mut w.store, Store::new(b_id, 1));
    let mut active = Some(b);
    c.activate(&mut active, a_id, 40_000, periods())
        .expect("activate a");
    w.sidecar_of(active.take().expect("a"));
    w.apply(&Op::LoadMission).expect("load a's mission");
    w.settle().expect("a settles");

    assert!(
        w.sim.objects().iter().all(|(_, camp, _)| *camp != B),
        "nothing of B in DCS while A is active"
    );
    no_duplicate(&w, CAMPAIGN);
    assert_eq!(w.store.rates().map(|r| r.position_s), Some(3.0));
    // A's units are where they were left, the dead one is still dead and
    // not in DCS, and the convoy's order goes on from where it was.
    let in_dcs: BTreeSet<u64> = w.units_in_dcs().into_iter().map(|(_, e)| e).collect();
    for u in &a_left {
        let now = w.store.unit(u.id).expect("unit");
        assert_eq!(now.dead, u.dead, "{}", u.id);
        assert_eq!(in_dcs.contains(&u.id.0), !u.dead, "{}", u.id);
        assert_eq!(now.type_name, u.type_name);
        assert_eq!(now.label, u.label);
        let (dx, dz) = (now.state.at.x - u.state.at.x, now.state.at.z - u.state.at.z);
        let g = w.store.group(u.group).expect("g");
        if g.id != convoy {
            assert!((dx * dx + dz * dz).sqrt() < 1.0, "{} moved", u.id);
        }
    }
    assert!(w.store.group(convoy).expect("g").progress >= a_progress);
    arrive(&mut w, convoy, (4_000.0, 4_000.0));
    assert!(order_results(&w) > 0);
    assert!(orders_issued_twice(&w).is_empty());
    assert!(orders_applied_twice(&w).is_empty());

    // B's store is as B was left.
    let b_again = Store::open(&c.store_path(b_id), 10_000).expect("b");
    same_world(&b_before, &b_again);
    for u in &b_left {
        let now = b_again.unit(u.id).expect("unit");
        assert_eq!(now.state.at, u.state.at, "{}", u.id);
        assert_eq!(now.dead, u.dead);
    }
    assert_eq!(b_again.record().expect("r").state, CampaignState::Suspended);
    assert_eq!(b_again.rates(), Some(rates(2.0)));
    let listed = c.list().expect("list");
    let states: Vec<_> = listed
        .iter()
        .map(|(id, r)| (id.0, r.as_ref().expect("record").state))
        .collect();
    assert_eq!(
        states,
        vec![
            (CAMPAIGN, CampaignState::Active),
            (B, CampaignState::Suspended)
        ]
    );
    // A suspend takes no backup: backups are scheduled, manual or before a
    // migration.
    assert!(c.backups(a_id).expect("backups").is_empty());
}

/// A restore while a death is pending does not re-create the dead member,
/// and spawns the rest: after a reload, and after DCS and the sidecar die
/// with the store read back from its file, under the report and respawn
/// policies.
#[test]
fn a_restore_with_a_death_pending_does_not_recreate_the_dead_member() {
    for left in [Left::Missing, Left::Removed] {
        for policy in [LostPolicy::Report, LostPolicy::Respawn] {
            for crash in [false, true] {
                let d = temp();
                let path = d.path().join("store.sqlite");
                let mut w = World::new(2, 0, policy);
                w.store.save_as(&path, record("a")).expect("save");
                w.settle().expect("settle");
                let id = first_group(&w);
                let members = w.store.group(id).expect("g").members.clone();
                let object = w
                    .units_in_dcs()
                    .into_iter()
                    .find(|(_, e)| *e == members[0].0)
                    .expect("in DCS")
                    .0;
                // Killed; the death event would come 60 s later.
                w.sim.kill_late(object, 60.0, left);
                ticks(&mut w, 30);
                assert_eq!(w.sim.late_pending(), 1);
                assert!(w.store.held(members[0]), "left, no death fact");
                let spawned = w.sim.ledger().spawns.len();

                if crash {
                    w.apply(&Op::Crash).expect("crash");
                    reopen(&mut w, &path);
                    w.apply(&Op::RestartDcs).expect("restart");
                } else {
                    w.apply(&Op::LoadMission).expect("reload");
                }
                w.settle().expect("settle");
                let case = format!("{left:?} {policy:?} crash {crash}");
                assert_eq!(w.store.group(id).expect("g").state, State::Live, "{case}");
                let in_dcs: BTreeSet<u64> = w.units_in_dcs().into_iter().map(|(_, e)| e).collect();
                assert!(!in_dcs.contains(&members[0].0), "{case}: re-created");
                assert!(
                    in_dcs.contains(&members[1].0) && in_dcs.contains(&members[2].0),
                    "{case}"
                );
                let restore: Vec<_> = w.sim.ledger().spawns[spawned..]
                    .iter()
                    .filter(|s| s.2 == id.0)
                    .collect();
                assert!(!restore.is_empty(), "{case}: restored");
                assert!(
                    restore.iter().all(|s| !s.4.contains(&members[0].0)),
                    "{case}"
                );
            }
        }
    }
}

fn add_notes(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE note (id INTEGER PRIMARY KEY, text TEXT NOT NULL);
         INSERT INTO note (text) SELECT 'migrated' FROM entity;",
    )
}

fn broken(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch("CREATE TABLE half (x); INSERT INTO nowhere VALUES (1);")
}

/// A stored campaign at schema version N opens at N+1: a backup at N first,
/// the step and the version in one transaction, the world unchanged. A
/// sidecar that knows only N refuses the file afterwards, and a failing step
/// leaves the store at N.
#[test]
fn a_stored_campaign_migrates_from_one_schema_version_to_the_next() {
    let d = temp();
    let c = Campaigns::new(d.path(), 10_000, 5).expect("campaigns");
    let id = CampaignId(CAMPAIGN);
    let mut w = World::new(2, 1, LostPolicy::Report);
    std::fs::create_dir_all(c.folder(id)).expect("folder");
    w.store
        .save_as(&c.store_path(id), record("a"))
        .expect("save");
    w.settle().expect("settle");
    w.store.commit().expect("commit");
    let before = {
        let mut s = Store::open(&c.store_path(id), 10_000).expect("open");
        let copy = s.clone();
        s.close().expect("close");
        copy
    };
    drop(std::mem::replace(&mut w.store, Store::new(id, 1)));
    assert_eq!(schema_of(&c.store_path(id)).expect("schema"), 1);

    // A failing step: nothing changes and the campaign does not open.
    let bad = [Migration { to: 2, run: broken }];
    assert!(c.open_with(id, 1, &bad).is_err());
    assert_eq!(schema_of(&c.store_path(id)).expect("schema"), 1);
    let half: i64 = rusqlite::Connection::open(c.store_path(id))
        .expect("conn")
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'half'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(half, 0, "the failed step's work is rolled back");
    let mut unchanged = Store::open(&c.store_path(id), 10_000).expect("open at 1");
    same_world(&before, &unchanged);
    unchanged.close().expect("close");
    drop(unchanged);

    let steps = [Migration {
        to: 2,
        run: add_notes,
    }];
    let after = c.open_with(id, 2, &steps).expect("migrate");
    assert_eq!(schema_of(&c.store_path(id)).expect("schema"), 2);
    same_world(&before, &after);
    drop(after);
    let notes: i64 = rusqlite::Connection::open(c.store_path(id))
        .expect("conn")
        .query_row("SELECT COUNT(*) FROM note", [], |r| r.get(0))
        .expect("notes");
    assert_eq!(
        notes as usize,
        before.groups().count() + units(&before).len()
    );

    let backups = c.backups(id).expect("backups");
    let migration: Vec<_> = backups
        .iter()
        .filter(|b| b.reason == BackupReason::PreMigration)
        .collect();
    assert_eq!(migration.len(), 2, "one before each attempt");
    let copy: PathBuf = c
        .folder(id)
        .join("backups")
        .join(migration[1].number.to_string())
        .join("store.sqlite");
    assert_eq!(schema_of(&copy).expect("schema"), 1, "the backup is at N");

    // A sidecar that knows only version 1 refuses the file.
    match Store::open(&c.store_path(id), 10_000) {
        Err(DiskError::Newer { found: 2, known: 1 }) => {}
        other => panic!("{other:?}"),
    }
    // Opening at version 2 again runs no step and takes no backup.
    let n = c.backups(id).expect("backups").len();
    c.open_with(id, 3, &steps).expect("open");
    assert_eq!(c.backups(id).expect("backups").len(), n);
}

/// Backups keep the newest `backup_keep`, number up without reuse, and
/// share one history copy that grows by what the history gained.
#[test]
fn backups_are_pruned_numbered_up_and_share_the_history() {
    let d = temp();
    let c = Campaigns::new(d.path(), 10_000, 2).expect("campaigns");
    let id = CampaignId(CAMPAIGN);
    let mut s = c.create(id, record("a")).expect("create");
    let mut h = c.history(id).expect("history");
    for i in 1..=5u64 {
        h.append(i, i, &[i as u8; 64]).expect("append");
        h.flush().expect("flush");
        c.backup(&mut s, i * 1_000, BackupReason::Manual)
            .expect("backup");
    }
    let kept: Vec<u32> = c
        .backups(id)
        .expect("backups")
        .iter()
        .map(|b| b.number)
        .collect();
    assert_eq!(kept, vec![4, 5]);
    let copy = std::fs::read(c.folder(id).join("backups/history.log")).expect("copy");
    let orig = std::fs::read(c.folder(id).join("history.log")).expect("history");
    assert_eq!(copy, orig);
    c.backup(&mut s, 9_000, BackupReason::Scheduled)
        .expect("backup");
    assert_eq!(c.backups(id).expect("backups").last().expect("b").number, 6);
}

/// What one write costs with 2,000 units, every one moved since the last:
/// printed for the results document, not asserted.
#[test]
fn the_cost_of_a_write_with_two_thousand_units() {
    let d = temp();
    let mut s = new_store_for(CAMPAIGN, 667, 0);
    s.save_as(&d.path().join("store.sqlite"), record("a"))
        .expect("save");
    let ids: Vec<EntityId> = units(&s).iter().map(|u| u.id).collect();
    assert!(ids.len() >= 2_000);
    let mut moved = Vec::new();
    let mut idle = Vec::new();
    for round in 0..10 {
        for (i, id) in ids.iter().enumerate() {
            s.observe_position(
                *id,
                nanoprobe_sidecar::store::Point {
                    x: f64::from(round) + i as f64,
                    y: 0.0,
                    z: 0.0,
                },
            );
        }
        let t = Instant::now();
        s.commit().expect("commit");
        moved.push(t.elapsed());
        let t = Instant::now();
        s.commit().expect("commit");
        idle.push(t.elapsed());
    }
    moved.sort();
    idle.sort();
    println!(
        "write with {} units: all moved median {:?}, nothing changed median {:?}",
        ids.len(),
        moved[5],
        idle[5]
    );
}

/// An activation restores a campaign with a held member left out, as a
/// reload and a DCS restart do.
#[test]
fn an_activation_does_not_recreate_a_held_member() {
    const B: u64 = CAMPAIGN + 1;
    let d = temp();
    let c = Campaigns::new(d.path(), 10_000, 3).expect("campaigns");
    let (a_id, b_id) = (CampaignId(CAMPAIGN), CampaignId(B));
    let mut w = World::new(2, 0, LostPolicy::Report);
    std::fs::create_dir_all(c.folder(a_id)).expect("folder");
    w.store
        .save_as(&c.store_path(a_id), record("a"))
        .expect("save");
    w.settle().expect("settle");
    let id = first_group(&w);
    let members = w.store.group(id).expect("g").members.clone();
    let object = w
        .units_in_dcs()
        .into_iter()
        .find(|(_, e)| *e == members[0].0)
        .expect("in DCS")
        .0;
    w.sim.kill_late(object, 60.0, Left::Missing);
    ticks(&mut w, 30);
    assert!(w.store.held(members[0]));

    let mut b = new_store_for(B, 1, 0);
    std::fs::create_dir_all(c.folder(b_id)).expect("folder");
    b.save_as(&c.store_path(b_id), record("b")).expect("save b");
    drop(b);
    for (to, from) in [(b_id, a_id), (a_id, b_id)] {
        let cur = std::mem::replace(&mut w.store, Store::new(from, 1));
        let mut active = Some(cur);
        c.activate(&mut active, to, 1, periods()).expect("activate");
        w.sidecar_of(active.take().expect("store"));
        w.apply(&Op::LoadMission).expect("load");
        w.settle().expect("settle");
    }
    assert!(w.store.held(members[0]), "the mark survives the switch");
    let in_dcs: BTreeSet<u64> = w.units_in_dcs().into_iter().map(|(_, e)| e).collect();
    assert!(!in_dcs.contains(&members[0].0));
    assert!(in_dcs.contains(&members[1].0) && in_dcs.contains(&members[2].0));
}

/// A group whose members are all dead or held, with one held, goes to
/// `lost` at a reload and never to `dead`: `dead` comes only from death
/// facts. RemoveEntity then releases the marks.
#[test]
fn an_all_held_group_stays_out_of_dead_until_the_controller_removes_it() {
    for policy in [LostPolicy::Report, LostPolicy::Respawn] {
        let mut w = World::new(1, 0, policy);
        w.settle().expect("settle");
        let id = first_group(&w);
        let members = w.store.group(id).expect("g").members.clone();
        let objects: BTreeMap<u64, u64> =
            w.units_in_dcs().into_iter().map(|(o, e)| (e, o)).collect();
        w.sim.kill(objects[&members[0].0]);
        w.sim.kill(objects[&members[1].0]);
        w.settle().expect("settle");
        w.sim.kill_late(objects[&members[2].0], 60.0, Left::Removed);
        ticks(&mut w, 30);
        w.apply(&Op::LoadMission).expect("load");
        w.settle().expect("settle");
        let g = w.store.group(id).expect("g");
        assert_eq!(g.state, State::Lost, "{policy:?}");
        assert!(w.store.held(members[2]));
        ticks(&mut w, 200);
        assert_eq!(w.store.group(id).expect("g").state, State::Lost);

        w.store.remove_entity(id).expect("remove");
        assert!(!w.store.held(members[2]));
        ticks(&mut w, 20);
        assert_ne!(w.store.group(id).expect("g").state, State::Dead);
    }
}
