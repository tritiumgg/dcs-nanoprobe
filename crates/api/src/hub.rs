//! The state every service shares: campaigns and their stores, the
//! reconciler, the simulation backend, leases, idempotency keys and the
//! event logs; and the driver that steps them.
//!
//! One mutex guards it. No lock is held across an `.await`: a call takes the
//! lock, does its work and lets go. Streams wake on [`Hub::tick`], which the
//! driver and every successful change advance.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::{self as pb, ErrorReason, Health, event};
use nanoprobe_sidecar::campaigns::{Backup, BackupReason, Campaigns};
use nanoprobe_sidecar::eventfile::EventFile;
use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::players::{Ctx as RosterCtx, Keyring, Roster, Unmanaged};
use nanoprobe_sidecar::reconciler::{Config as RecConfig, EffectResult, Reconciler};
use nanoprobe_sidecar::server::{ServerStore, StoreLock};
use nanoprobe_sidecar::sim::wire::{Body, Command, Input};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::store::{
    CampaignRecord, CampaignState, Checkpoint, DiskError, Loadout, LostPolicy, Outcome, OwedLoad,
    Store, TierPeriod, checkpoint_of,
};
use nanoprobe_transport::records::CommandHeader;
use prost::Message;
use tokio::sync::{Notify, watch};
use tonic::{Code, Status};

use crate::config::Config;
use crate::events::{EventLog, Link, Lookup, Read, loadout_body, player_body, route_body};
use crate::idem::{self, Begin, Done, Idem};
use crate::idem::{EffectKind, Pending};
use crate::lease::{LeaseError, Leases};
use crate::loadouts::Loadouts;

/// What the server drives: DCS with the agent in it, real or fake.
pub trait Backend: Send + 'static {
    /// The adapter the reconciler reads and writes through.
    fn sim(&mut self) -> &mut dyn SimAdapter;
    /// Let time pass for one step. A real agent runs on its own; a fake one
    /// runs a frame here.
    fn frame(&mut self);
    /// Attach as a new sidecar boot: the agent answers with a `HELLO`.
    fn attach(&mut self, sidecar_boot: u64);
    /// Drop the link.
    fn detach(&mut self);
    /// Load the campaign's mission on `theater` (plan 4.6, "Activate"), for
    /// a backend that loads its missions itself: the fake loads its one
    /// mission. Called only when [`Backend::missions`] gives `None`.
    fn load_mission(&mut self, theater: &str);
    /// The mission folder (spec/config.md, `[missions]`): the hub writes a
    /// campaign's shell there and the reconciler sends a `MISSION_LOAD` of
    /// it. `None` for a backend that loads its missions itself.
    fn missions(&self) -> Option<&crate::real::Missions> {
        None
    }
    /// The health verdict (spec/errors.md, "Health").
    fn health(&self) -> Health;
    /// Time since the agent's heartbeat last advanced, read from the status
    /// block whether or not the sidecar is attached; `None` when unknown.
    fn heartbeat_age(&self) -> Option<Duration>;
    /// Why the last attach failed (spec/errors.md, "Health"), empty for none.
    fn attach_error(&self) -> String;
    /// The largest command payload the agent takes: an eighth of the
    /// inbound ring (spec/errors.md, "Command outcomes"). The default is
    /// that of the default ring, `in_ring_kib = 1024` (spec/config.md).
    fn max_command(&self) -> usize {
        1024 * 1024 / 8
    }
    /// The backend itself, for a test that drives faults on a fake.
    fn as_any(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
    /// The periods the sampling tiers achieve now, in seconds: position,
    /// vitals, warehouse, enumeration (the agent's metrics,
    /// spec/transport.md); `None` when unknown.
    fn achieved(&self) -> Option<[f64; 4]> {
        None
    }
}

/// The sampling tiers by their `SampleRates` field names, in the order of
/// [`Backend::achieved`].
const TIERS: [&str; 4] = ["position", "vitals", "warehouse", "enumeration"];

/// One campaign: its description, its store and what the API keeps beside
/// the store.
#[derive(Debug)]
pub struct Slot {
    /// The campaign as the API shows it.
    pub meta: pb::Campaign,
    /// The world store.
    pub store: Store,
    /// Its event log.
    pub events: EventLog,
    /// The default policy for its lost groups.
    pub on_lost: LostPolicy,
    /// The theater its next activation loads.
    pub theater: String,
    /// For a campaign on disk, whether `store` is its file, opened. One not
    /// opened holds an empty store in memory; the first call that names it
    /// opens it (spec/persistence.md, "Campaign operations"). Always true
    /// for a campaign in memory.
    pub opened: bool,
}

/// Send the activation's owed mission load, or fold in its result
/// (spec/persistence.md, "Campaign operations"). `APPLIED` clears it and
/// holds materializing until the store records a mission after `sent_in`,
/// the one the load was sent in; `NOT_SENT` keeps it owed
/// to be sent again; any other outcome is logged and clears it, and so does a
/// sent load whose command the store no longer holds in flight.
fn owed_load(
    rec: &mut Reconciler,
    store: &mut Store,
    sim: &mut dyn SimAdapter,
    sent_in: Option<(u64, u32)>,
    hold_after: &mut Option<(u64, u32)>,
) {
    let Some(owed) = store.owed_load().cloned() else {
        return;
    };
    match owed.cmd {
        None => {
            if let Some(key) = rec.mission_load(store, sim, &owed.path) {
                store.set_owed_load(Some(OwedLoad {
                    cmd: Some(key),
                    ..owed
                }));
            }
        }
        Some(key) => match rec.effect_result(key).map(|r| r.outcome) {
            Some(Outcome::Applied) => {
                store.set_owed_load(None);
                // The new shell's start counts from the new generation's
                // first sample, not from the running one's.
                if owed.start_ms.is_some() {
                    store.set_mission_start_ms(owed.start_ms);
                    // Unless the new generation's samples came in the same
                    // step, before the result was folded in.
                    let running = store.loaded().map(|l| (l.agent_boot, l.mission_gen));
                    if sent_in.is_none() || running == sent_in {
                        store.await_generation();
                    }
                }
                store.set_shell_slot(slot_in(&owed.path));
                // Hold until the store records a mission other than the
                // one the load was sent in.
                // The command's row is read before the step that folds its
                // result, so it is always there; without it the hold ends at
                // the first mission the store records.
                if sent_in.is_none() {
                    eprintln!(
                        "the activation's MISSION_LOAD of {} was applied with no command row read; spawning resumes at the next recorded mission",
                        owed.path
                    );
                }
                *hold_after = Some(sent_in.unwrap_or((0, 0)));
            }
            Some(Outcome::NotSent) => store.set_owed_load(Some(OwedLoad { cmd: None, ..owed })),
            Some(other) => {
                eprintln!(
                    "the activation's MISSION_LOAD of {} was {other:?}; the campaign runs on the mission DCS holds",
                    owed.path
                );
                store.set_owed_load(None);
                // DCS may have taken the file: it is not written over next.
                if other == Outcome::Unknown {
                    store.set_shell_slot(slot_in(&owed.path));
                }
            }
            None if !store.unanswered().iter().any(|c| (c.epoch, c.seq) == key) => {
                eprintln!(
                    "the activation's MISSION_LOAD of {} has no command in flight; it is cleared",
                    owed.path
                );
                store.set_owed_load(None);
                store.set_shell_slot(slot_in(&owed.path));
            }
            None => {}
        },
    }
}

/// The shell slot a load's path names.
fn slot_in(path: &str) -> Option<u8> {
    path.rsplit(['/', '\\'])
        .next()
        .and_then(|n| n.strip_suffix(".miz"))
        .and_then(|n| n.rsplit('.').next())
        .filter(|s| *s == "a" || *s == "b")
        .map(|s| s.as_bytes()[0])
}

/// A moderation call sent to DCS, waiting for its result. An applied one
/// becomes a `ModerationEvent` with the role of the token that made it.
#[derive(Clone, Debug)]
pub struct PendingModeration {
    /// The command's epoch and sequence number.
    pub cmd: (u64, u64),
    /// What was done.
    pub action: pb::ModerationAction,
    /// The player acted on.
    pub player_key: String,
    /// The reason given.
    pub reason: String,
    /// The role of the caller's token.
    pub role: pb::Role,
}

/// The shared state. Services reach it through [`Hub::lock`].
pub struct Inner {
    /// DCS.
    pub backend: Box<dyn Backend>,
    /// Every campaign, by ID.
    pub campaigns: BTreeMap<u64, Slot>,
    /// The active campaign.
    pub active: Option<u64>,
    /// The reconciler of the active campaign. Volatile: rebuilt from the
    /// store whenever the active campaign or the controller changes.
    pub rec: Option<Reconciler>,
    /// This sidecar's boot, one more at each reattach.
    pub sidecar_boot: u64,
    /// The controller lease.
    pub leases: Leases,
    /// Idempotency keys, server-wide.
    pub idem: Idem,
    /// Events recorded while no campaign is active.
    pub server_log: EventLog,
    /// What the records say about the agent.
    pub link: Link,
    /// The players: keys, who is connected, who is in which unit. Server
    /// scoped: it lives through campaign changes.
    pub roster: Roster,
    /// The loadouts of the units players occupy, by DCS object ID: what
    /// `Player.ammo` shows.
    pub loadouts: Loadouts,
    /// Moderation calls whose result has not come.
    pub moderation: Vec<PendingModeration>,
    /// The `PLAYERS` query in flight.
    players_cmd: Option<(u64, u64)>,
    /// The server's default lost policy (`[world] on_lost`).
    pub on_lost: LostPolicy,
    /// A `CAMPAIGN 0` is owed to the agent: no campaign is active and the
    /// last `HELLO` named one (spec/commands.md).
    clear_campaign: bool,
    /// The campaign folders, with `[store]` on disk.
    files: Option<Campaigns>,
    /// When the active campaign's next checkpoint is due, wall ms.
    next_checkpoint_ms: u64,
    /// When the active campaign's next scheduled backup is due, wall ms.
    next_backup_ms: u64,
    /// Why the last scheduled backup failed, empty when it did not. A
    /// failed backup is taken again at the next interval.
    pub backup_error: String,
    /// Campaign folders that could not be read at start, with why. Their IDs
    /// are never given to a new campaign.
    pub unreadable: Vec<(u64, String)>,
    /// The highest command epoch used, the floor of the next.
    epoch_floor: u64,
    /// When the event retention next runs, wall ms.
    next_retention_ms: u64,
    /// The scheduled backup running on a worker thread.
    backup_job: Option<BackupJob>,
    /// Campaigns with a backup running: one at a time each.
    pub backing_up: std::collections::BTreeSet<u64>,
    /// After an activation's `MISSION_LOAD` was applied, the mission it was
    /// sent in, by agent boot and generation, from its command's row: nothing
    /// materializes until the store records another.
    pub hold_after: Option<(u64, u32)>,
    /// Finished campaigns read through a migrated copy of their store
    /// (spec/persistence.md, "Reading a finished campaign"), with when a
    /// read last named each.
    archive: BTreeMap<u64, (nanoprobe_sidecar::campaigns::ReadCopy, Instant)>,
    /// Bytes the free-space rule asks for on top of what a campaign holds:
    /// a test sets it to make the file system look full.
    pub space_extra: u64,
    /// Whether the last step's verdict was hung: a change publishes a
    /// lifecycle event (`SIM_STATE_HUNG` on entering, the real state on
    /// leaving).
    hung: bool,
    /// The server store, with `[store]` on disk.
    pub server: Option<ServerStore>,
    /// The lock on the store dir, held while the hub lives.
    lock: Option<StoreLock>,
    /// `idempotency_keep_h`.
    idem_keep: Duration,
    /// Stores to close at the end of the call that suspended or finished
    /// them.
    closing: Vec<(u64, CampaignState, Option<Vec<u8>>)>,
    /// Fault injection for tests: the next driver step panics after its
    /// events commit and before any store write, as a crash there would stop
    /// the sidecar.
    #[doc(hidden)]
    pub crash_after_events: bool,
    /// The store write times, for a live measurement (`crate::timings`).
    timings: Option<crate::timings::Timings>,
}

/// A scheduled backup on its worker thread.
struct BackupJob {
    campaign: u64,
    handle: std::thread::JoinHandle<Result<Backup, DiskError>>,
}

/// The shared state and its wake-ups.
pub struct Hub {
    inner: Mutex<Inner>,
    tick: watch::Sender<u64>,
    settled: Notify,
    closing: std::sync::atomic::AtomicBool,
    poisoned: std::sync::atomic::AtomicBool,
    /// The settings.
    pub cfg: Config,
}

/// What a changing call needs of its campaign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// Nothing: the call goes ahead with none (spec/errors.md).
    Nothing,
    /// A campaign that exists and is not finished, active or suspended.
    Campaign,
    /// The active campaign: the call acts on DCS.
    Active,
    /// A campaign to activate: as `Campaign`, but its store is not opened
    /// here; the activation opens it after it suspends the active one
    /// (spec/persistence.md, "Campaign operations").
    Activate,
}

/// A changing call, as [`Hub::write`] runs it.
#[derive(Clone, Copy, Debug)]
pub struct Call<'a> {
    /// The method's name, such as `PutGroup`.
    pub method: &'static str,
    /// The idempotency key.
    pub key: Option<&'a pb::Idempotency>,
    /// The lease presented.
    pub lease: Option<&'a pb::Lease>,
    /// The campaign named, 0 for the active one.
    pub campaign: u64,
    /// What the call needs of its campaign.
    pub need: Need,
}

/// The status for a refused lease.
pub fn lease_status(e: LeaseError) -> Status {
    match e {
        LeaseError::Held => status(
            Code::FailedPrecondition,
            ErrorReason::LeaseHeld,
            "another controller holds the lease",
            0,
            "",
        ),
        LeaseError::Lost => status(
            Code::Aborted,
            ErrorReason::LeaseLost,
            "the lease is not the current one: it expired, was released or was taken over",
            0,
            "lease",
        ),
        LeaseError::NotController => status(
            Code::FailedPrecondition,
            ErrorReason::NotController,
            "the call needs the controller lease",
            0,
            "lease",
        ),
    }
}

/// How long a finished campaign's read copy stays after the last read
/// that named it.
pub const ARCHIVE_KEEP: Duration = Duration::from_secs(60);

/// `FAILED_PRECONDITION` naming the theater.
fn precondition(msg: String) -> Status {
    status(
        Code::FailedPrecondition,
        ErrorReason::Unspecified,
        msg,
        0,
        "theater",
    )
}

/// The state the server reports for DCS: hung and gone from the verdict,
/// otherwise the agent's lifecycle state.
pub fn server_sim_state(health: Health, lifecycle: u32) -> pb::SimState {
    match health {
        Health::Hung => pb::SimState::Hung,
        Health::Gone => pb::SimState::Gone,
        _ => crate::events::sim_state(lifecycle),
    }
}

/// Wall-clock milliseconds since the Unix epoch.
pub fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A [`SimAdapter`] that keeps a copy of every record the reconciler reads.
struct Tee<'a> {
    inner: &'a mut dyn SimAdapter,
    seen: Vec<RawRecord>,
}

impl SimAdapter for Tee<'_> {
    fn position_after(&self, opcode: u16, payload: &[u8]) -> u64 {
        self.inner.position_after(opcode, payload)
    }

    fn send(&mut self, opcode: u16, payload: &[u8]) -> Result<u64, Detached> {
        self.inner.send(opcode, payload)
    }

    fn read(&mut self) -> Vec<RawRecord> {
        let v = self.inner.read();
        self.seen.extend(v.iter().cloned());
        v
    }

    fn flush(&mut self) {
        self.inner.flush();
    }
}

impl Inner {
    /// The reconciler's settings for a campaign.
    fn rec_config(&self, cfg: &Config, campaign: u64) -> RecConfig {
        RecConfig {
            materialize_per_frame: cfg.materialize_per_frame.max(1),
            on_lost: self
                .campaigns
                .get(&campaign)
                .map_or(self.on_lost, |s| s.on_lost),
            destroy_retries: cfg.destroy_retries.max(1),
        }
    }

    /// Detach, build a new reconciler from the active campaign's store and
    /// attach again as a new sidecar boot: the restart path (plan 4.4), which
    /// also starts a new command epoch (spec/commands.md).
    pub fn restart(&mut self, cfg: &Config) {
        self.backend.detach();
        self.link.attached = false;
        self.players_cmd = None;
        self.moderation.clear();
        self.roster.want_resync();
        self.sidecar_boot += 1;
        let floor = self.epoch_floor;
        self.rec = self.active.map(|id| {
            let rc = self.rec_config(cfg, id);
            let store = &mut self.campaigns.get_mut(&id).expect("active campaign").store;
            // The epoch outlives the campaign (spec/commands.md).
            store.seed_epoch(floor);
            Reconciler::new(rc, store)
        });
        self.backend.attach(self.sidecar_boot);
    }

    /// Run `f` with the active campaign's reconciler, its store and the
    /// adapter, when `campaign` is the active one and a reconciler exists.
    pub fn with_rec<T>(
        &mut self,
        campaign: u64,
        f: impl FnOnce(&mut Reconciler, &mut Store, &mut dyn SimAdapter) -> T,
    ) -> Option<T> {
        if self.active != Some(campaign) {
            return None;
        }
        let rec = self.rec.as_mut()?;
        let slot = self.campaigns.get_mut(&campaign)?;
        Some(f(rec, &mut slot.store, self.backend.sim()))
    }

    /// Put the events read from one agent record in their log: the campaign
    /// of the frame they were made in when the server holds that campaign,
    /// otherwise the server log.
    ///
    /// Events whose agent record is in the log already are not appended
    /// again: the record was read again after a crash between the events
    /// commit and the store commit (spec/persistence.md).
    fn log(&mut self, reads: Vec<Read>, source: (u64, u64)) {
        let Some(first) = reads.first() else {
            return;
        };
        let (campaign, log) = match self.campaigns.get_mut(&first.campaign) {
            Some(slot) if first.campaign != 0 => (first.campaign, &mut slot.events),
            _ => (0, &mut self.server_log),
        };
        let source = (source.0 != 0).then(|| format!("{}:{}", source.0, source.1));
        if let Some(s) = source.as_deref() {
            match log.has_source(s) {
                Ok(false) => {}
                Ok(true) => return,
                Err(e) => panic!("the event log could not be read, stopping: {e}"),
            }
        }
        // Every event of one record goes to the same log: they share its
        // frame's campaign.
        // The first event's key is the record's; the others are numbered,
        // since a source is unique (spec/persistence.md, "Event history").
        for (i, r) in reads.into_iter().enumerate() {
            let key = match (&source, i) {
                (Some(s), 0) => Some(s.clone()),
                (Some(s), i) => Some(format!("{s}:{i}")),
                (None, _) => None,
            };
            log.push_from(campaign, r.stamp, r.body, key);
        }
    }

    /// Append an event to the active campaign's log, or the server log.
    pub fn publish(&mut self, body: event::Body) -> u64 {
        let stamp = self.link.stamp();
        match self.active.and_then(|id| self.campaigns.get_mut(&id)) {
            Some(slot) => {
                let id = slot.meta.id;
                slot.events.push(id, stamp, body)
            }
            None => self.server_log.push(0, stamp, body),
        }
    }

    /// One driver step: a frame, every record read and folded in, then the
    /// checkpoint and backup timers.
    fn step(&mut self, cfg: &Config) {
        self.report_timings();
        self.backend.frame();
        // The clock and the agent boot before the step: a DCS restart runs
        // a mission the campaign did not load, and the clock carries on
        // from where it was.
        let before = self.active.map(|id| {
            (
                id,
                self.clock_ms(id),
                self.campaigns
                    .get(&id)
                    .and_then(|s| s.store.loaded().map(|l| l.agent_boot)),
            )
        });
        let records = match (self.active, self.rec.as_mut()) {
            (Some(id), Some(rec)) => {
                let slot = self.campaigns.get_mut(&id).expect("active campaign");
                let mut tee = Tee {
                    inner: self.backend.sim(),
                    seen: Vec::new(),
                };
                // While a mission load is owed, and until the loaded
                // mission's generation follows it, nothing materializes: a
                // campaign never spawns into the mission before its own.
                // The hold ends at the first mission the store records after
                // the one the load was applied in. Another agent boot is a
                // DCS restart, whose own mission ends the hold too.
                if let Some((boot, generation)) = self.hold_after
                    && let Some(l) = slot.store.loaded()
                    && !l.theater.is_empty()
                    && (l.agent_boot, l.mission_gen) != (boot, generation)
                {
                    if l.agent_boot != boot {
                        eprintln!(
                            "DCS restarted after the activation's mission load was applied; the campaign takes the mission it runs now"
                        );
                    }
                    self.hold_after = None;
                }
                rec.hold_world(slot.store.owed_load().is_some() || self.hold_after.is_some());
                // The mission the owed load was sent in, from its command's
                // row, read before this step can fold in its result.
                let sent_in = slot.store.owed_load().and_then(|o| o.cmd).and_then(|key| {
                    slot.store
                        .unanswered()
                        .into_iter()
                        .find(|c| (c.epoch, c.seq) == key)
                        .map(|c| (c.agent_boot, c.mission_gen))
                });
                rec.step_held(&mut slot.store, &mut tee);
                owed_load(
                    rec,
                    &mut slot.store,
                    &mut tee,
                    sent_in,
                    &mut self.hold_after,
                );
                // Route progress made from the samples just read.
                let stamp = self.link.stamp();
                for e in slot.store.take_route_events() {
                    // Keyed, so a sample read again after a crash between
                    // the two commits does not log it twice.
                    let source = format!(
                        "route:{}:{}:{}",
                        e.group.0,
                        e.order_rev,
                        e.index.map_or_else(|| "end".to_owned(), |i| i.to_string())
                    );
                    push_once(&mut slot.events, id, stamp, route_body(&e), Some(source));
                }
                tee.seen
            }
            _ => self.backend.sim().read(),
        };
        if let Some((id, Some(clock), Some(boot))) = before
            && let Some(slot) = self.campaigns.get_mut(&id)
            && slot.store.mission_start_ms().is_some()
            && slot.store.loaded().is_some_and(|l| l.agent_boot != boot)
        {
            slot.store.set_mission_start_ms(None);
            self.write_clock(id, clock);
        }
        let now = wall_ms() as f64 / 1000.0;
        let mut reads = Vec::new();
        // The roster adopts the unmanaged units players occupy: it reaches
        // the reconciler, the store and the adapter through one cell.
        let env = match (self.active, self.rec.as_mut()) {
            (Some(id), Some(rec)) => self
                .campaigns
                .get_mut(&id)
                .map(|slot| RefCell::new((rec, &mut slot.store, self.backend.sim()))),
            _ => None,
        };
        for r in &records {
            if self.active.is_none()
                && let Ok(Input::Hello(h)) = Input::decode(r.kind, &r.payload)
            {
                self.clear_campaign = h.campaign != 0;
            }
            let bound = |o: u64| {
                env.as_ref().and_then(|e| {
                    let g = e.borrow();
                    g.0.entity_of(&*g.1, o)
                })
            };
            let adopt = |u: &Unmanaged| {
                env.as_ref().and_then(|e| {
                    let mut g = e.borrow_mut();
                    let (rec, store, sim) = &mut *g;
                    rec.adopt(store, &mut **sim, u)
                })
            };
            let notes = self.roster.fold(
                r,
                &RosterCtx {
                    now,
                    bound: &bound,
                    adopt: Some(&adopt),
                },
            );
            let unit = |o: u64| bound(o).map(|b| b.0);
            let look = Lookup {
                unit: &unit,
                roster: Some(&self.roster),
            };
            let mut group = Vec::new();
            if let Some(read) = self.link.read_with(r, &look) {
                group.push(read);
            }
            // A unit a player left keeps no loadout.
            for n in &notes {
                if let nanoprobe_sidecar::players::Note::Leave { object, .. } = n
                    && self.roster.players_of(*object).is_empty()
                {
                    self.loadouts.forget(*object);
                    if let Some(e) = env.as_ref() {
                        let mut g = e.borrow_mut();
                        let (rec, store, _) = &mut *g;
                        if let Some((unit, _)) = rec.entity_of(store, *object) {
                            store.forget_loadout(EntityId(unit));
                        }
                    }
                }
            }
            self.loadouts
                .retain_generation(self.link.agent_boot, self.link.mission_gen);
            for n in notes {
                group.push(Read {
                    campaign: self.link.frame_campaign,
                    stamp: self.link.stamp(),
                    body: player_body(n),
                });
            }
            // The ammunition a birth or a takeoff read of a player-occupied
            // unit gave (spec/world.md, "Ammunition"). A read of an object
            // with no `EntityId` yet waits for its bind, which a record
            // since the read may have made.
            if let Some(e) = env.as_ref() {
                let mut g = e.borrow_mut();
                let (rec, store, _) = &mut *g;
                let released = self
                    .loadouts
                    .take_bound(|o| rec.entity_of(store, o).is_some());
                for (object, boot, loadout) in released {
                    apply_loadout(
                        &self.roster,
                        &mut self.loadouts,
                        &self.link,
                        (&**rec, &mut **store),
                        (object, boot, loadout),
                        &mut group,
                    );
                }
                if r.kind == nanoprobe_transport::layout::kind::AMMO as u16
                    && let Ok(a) = nanoprobe_transport::records::Ammo::decode(&r.payload)
                {
                    let loadout = crate::convert::loadout(&a, &self.link.stamp());
                    if rec.entity_of(store, a.object).is_some() {
                        apply_loadout(
                            &self.roster,
                            &mut self.loadouts,
                            &self.link,
                            (&**rec, &mut **store),
                            (a.object, r.agent_boot, loadout),
                            &mut group,
                        );
                    } else {
                        self.loadouts.hold(a.object, r.agent_boot, loadout);
                    }
                }
            }
            reads.push((group, (r.agent_boot, r.number)));
        }
        for (group, source) in reads {
            self.log(group, source);
        }
        self.save_keys();
        self.drive_players();
        self.watch_hung();
        // With no campaign active the agent must hold none: it idles, and
        // its records stamp campaign 0 (plan 4.6, "Suspend").
        if self.clear_campaign && self.active.is_none() {
            let (op, payload) = Command {
                header: CommandHeader::default(),
                body: Body::Campaign(0),
            }
            .encode();
            if self.backend.sim().send(op, &payload).is_ok() {
                self.clear_campaign = false;
            }
        }
        // The events go to disk before any store write of the step, the
        // checkpoint included.
        self.commit_events();
        if std::mem::take(&mut self.crash_after_events) {
            panic!("injected: a crash between the events commit and the store commit");
        }
        self.timers(cfg);
        self.expire_archives();
        self.settle(None);
    }

    /// Publish a lifecycle event when the verdict enters or leaves hung
    /// (spec/errors.md, "Health"): `SIM_STATE_HUNG` on entering, the state
    /// DCS shows on leaving (`SIM_STATE_GONE` when nothing listens).
    fn watch_hung(&mut self) {
        let health = self.backend.health();
        let hung = health == Health::Hung;
        if hung == self.hung {
            return;
        }
        self.hung = hung;
        let state = server_sim_state(health, self.link.lifecycle);
        let theater = self.link.theater.clone();
        self.publish(event::Body::Lifecycle(pb::LifecycleEvent {
            state: state as i32,
            theater,
        }));
    }

    /// Write the player keys learned since the last step to the server
    /// store. A failed write is tried again at the next step.
    fn save_keys(&mut self) {
        let Some(server) = self.server.as_ref() else {
            return;
        };
        for (key, ucid) in self.roster.keyring().unsaved() {
            if server.put_player(&key, &ucid).is_err() {
                return;
            }
            self.roster.keyring_mut().saved(&key);
        }
    }

    /// What the roster and the moderation calls need after a step: the
    /// `PLAYERS` query when one is owed, the occupied units for `SUBSCRIBE`,
    /// and the events of moderation calls that were applied.
    fn drive_players(&mut self) {
        let Some(id) = self.active else {
            self.moderation.clear();
            return;
        };
        let Some(rec) = self.rec.as_mut() else {
            return;
        };
        rec.set_subscribed(self.roster.occupied_units());
        if let Some(cmd) = self.players_cmd {
            match rec.effect_result(cmd) {
                Some(r) => {
                    // A lost query is asked again; one the agent refused or
                    // rejected waits for the next `HELLO` or mission load.
                    if matches!(r.outcome, Outcome::Unknown | Outcome::NotSent) {
                        self.roster.want_resync();
                    }
                    self.players_cmd = None;
                }
                None if !rec.inflight().any(|j| (j.epoch, j.seq) == cmd) => {
                    self.roster.want_resync();
                    self.players_cmd = None;
                }
                None => {}
            }
        }
        if self.players_cmd.is_none() && self.roster.resync_wanted() {
            let slot = self.campaigns.get_mut(&id).expect("active campaign");
            let sim = self.backend.sim();
            if let Some(cmd) = rec.query_players(&mut slot.store, sim) {
                self.roster.take_resync();
                self.players_cmd = Some(cmd);
            }
        }
        let mut applied = Vec::new();
        self.moderation.retain(|m| match rec.effect_result(m.cmd) {
            Some(r) => {
                if r.outcome == Outcome::Applied {
                    applied.push((m.clone(), r.clone()));
                }
                false
            }
            None => rec.inflight().any(|j| (j.epoch, j.seq) == m.cmd),
        });
        for (m, r) in applied {
            let result = crate::services::commands::command_result(Some(&r));
            self.publish(event::Body::Moderation(pb::ModerationEvent {
                action: m.action as i32,
                player_key: m.player_key,
                reason: m.reason,
                role: m.role as i32,
                result: Some(result),
            }));
        }
    }

    /// Keep a call's answer on disk (spec/persistence.md, "Idempotency
    /// keys"): in the store of the campaign it acted on, written with its
    /// change by the next commit, or in the server store for a call on no
    /// campaign. Returns the campaign whose store took it.
    pub(crate) fn remember(&mut self, key: &str, d: &idem::Digest32, done: &Done) -> Option<u64> {
        let server = self.server.as_ref()?;
        let row = nanoprobe_sidecar::store::IdemRow {
            key: key.to_owned(),
            at_ms: wall_ms(),
            request_hash: d.to_vec(),
            response: idem::encode_done(done),
        };
        match self.campaigns.get_mut(&done.campaign) {
            Some(slot) if slot.opened && slot.store.on_disk() => {
                slot.store.put_idempotency(row);
                Some(done.campaign)
            }
            _ => {
                if let Err(e) = server.put_idempotency(&row, None) {
                    panic!("the server store could not be written, stopping: {e}");
                }
                None
            }
        }
    }

    /// A key's row in the server store's index, written before the commit
    /// that writes the key in the campaign's store: an index row whose key
    /// the store never got finds nothing, and the repeat runs as new.
    pub(crate) fn index(&mut self, key: &str, placed: Option<u64>) {
        let (Some(c), Some(server)) = (placed, self.server.as_ref()) else {
            return;
        };
        let row = nanoprobe_sidecar::store::IdemRow {
            key: key.to_owned(),
            at_ms: wall_ms(),
            request_hash: Vec::new(),
            response: Vec::new(),
        };
        if let Err(e) = server.put_idempotency(&row, Some(c)) {
            panic!("the server store could not be written, stopping: {e}");
        }
    }

    /// A key a store remembers that the table in memory does not: through
    /// the server store's index, or in the named campaign's store when the
    /// index row was lost to a crash. Its digest, answer and age.
    fn recall(&mut self, key: &str, named: u64) -> Option<(idem::Digest32, Done, Duration)> {
        let server = self.server.as_ref()?;
        let found = match server.idempotency(key) {
            Ok(f) => f,
            Err(e) => panic!("the server store could not be read, stopping: {e}"),
        };
        let row = match found {
            Some(i) if i.campaign.is_none() => i.answer.map(|(h, r)| (h, r, i.at_ms)),
            Some(i) => {
                let c = i.campaign.unwrap_or(0);
                self.stored_key(c, key)
            }
            None => {
                let c = if named == 0 { self.active? } else { named };
                self.stored_key(c, key)
            }
        }?;
        let (hash, response, at_ms) = row;
        let d: idem::Digest32 = hash.as_slice().try_into().ok()?;
        let done = idem::decode_done(&response)?;
        let age = Duration::from_millis(wall_ms().saturating_sub(at_ms));
        // A key past `idempotency_keep_h` is forgotten: the call runs as new.
        if age >= self.idem_keep {
            return None;
        }
        Some((d, done, age))
    }

    /// A key kept in campaign `c`'s store, opening it if it is not finished.
    fn stored_key(&mut self, c: u64, key: &str) -> Option<(Vec<u8>, Vec<u8>, u64)> {
        let slot = self.campaigns.get(&c)?;
        if slot.meta.state == pb::CampaignState::Finished as i32 {
            // A finished store is read without being opened.
            let path = self.files.as_ref()?.store_path(CampaignId(c));
            return match nanoprobe_sidecar::store::idempotency_of(&path, key) {
                Ok(r) => r.map(|r| (r.request_hash, r.response, r.at_ms)),
                Err(e) => panic!("the store of campaign {c} could not be read, stopping: {e}"),
            };
        }
        self.open_store(c).ok()?;
        let row = match self.campaigns[&c].store.idempotency(key) {
            Ok(r) => r?,
            Err(e) => panic!("the store of campaign {c} could not be read, stopping: {e}"),
        };
        Some((row.request_hash, row.response, row.at_ms))
    }

    /// Read the campaign folders (see [`Hub::open`]).
    fn load(&mut self, cfg: &Config, sf: &crate::config::StoreFiles) -> Result<(), String> {
        // Two sidecars never write one store dir (spec/persistence.md).
        self.lock = Some(
            nanoprobe_sidecar::server::lock(&sf.dir)
                .map_err(|e| format!("the store folder {}: {e}", sf.dir.display()))?,
        );
        let server = ServerStore::open(&sf.dir, wall_ms())
            .map_err(|e| format!("the server store in {}: {e}", sf.dir.display()))?;
        server
            .migrate_players_json(&sf.dir)
            .map_err(|e| format!("the server store in {}: {e}", sf.dir.display()))?;
        self.roster = Roster::new(
            keyring_of(&server)
                .map_err(|e| format!("the server store in {}: {e}", sf.dir.display()))?,
        );
        let mut log = EventFile::open(&server.events_path(), 0)
            .and_then(|f| EventLog::open(f, cfg.events_keep))
            .map_err(|e| format!("the server event log: {e}"))?;
        retain(&mut log, 0, sf.server_log_retention)
            .map_err(|e| format!("the server event log: {e}"))?;
        self.server_log = log;
        self.epoch_floor = server.epoch();
        self.server = Some(server);
        let files = Campaigns::new(&sf.dir, cfg.world_changes_keep, sf.backup_keep)
            .map_err(|e| format!("the store folder {}: {e}", sf.dir.display()))?;
        let listed = files
            .list()
            .map_err(|e| format!("the store folder {}: {e}", sf.dir.display()))?;
        let mut active = Vec::new();
        if let Some(dir) = self.backend.missions().and_then(|m| m.dir().ok()) {
            nanoprobe_sidecar::shell::clear_temps(dir);
        }
        for (id, r) in listed {
            // Read copies a stopped sidecar left behind.
            files.clear_read_copies(id);
            let path = files.store_path(id);
            let read = r.and_then(|r| {
                let (_, schema, _) = nanoprobe_sidecar::store::record_of(&path)?;
                Ok((r, schema, checkpoint_of(&path)?))
            });
            let (record, schema, cp) = match read {
                Ok(x) => x,
                Err(e) => {
                    self.unreadable.push((id.0, e.to_string()));
                    continue;
                }
            };
            let events = files.events(id).and_then(|f| {
                let mut l = EventLog::open(f, cfg.events_keep)?;
                // A finished campaign is read-only: its history is kept whole.
                if record.state != CampaignState::Finished {
                    retain(&mut l, id.0, sf.event_retention)?;
                }
                Ok(l)
            });
            let events = match events {
                Ok(l) => l,
                Err(e) => {
                    self.unreadable.push((id.0, format!("events: {e}")));
                    continue;
                }
            };
            if record.state == CampaignState::Active {
                active.push(id.0);
            }
            self.campaigns.insert(
                id.0,
                Slot {
                    meta: pb::Campaign {
                        templates: nanoprobe_sidecar::store::template_theaters_of(&path)
                            .unwrap_or_default(),
                        ..meta_from(id.0, schema, &record, cp.as_ref())
                    },
                    store: Store::new(id, cfg.world_changes_keep),
                    events,
                    on_lost: self.on_lost,
                    theater: record.theater.clone(),
                    opened: false,
                },
            );
        }
        self.files = Some(files);
        match active.as_slice() {
            [] => {}
            [id] => {
                self.open_store(*id).map_err(|e| e.message().to_owned())?;
                self.active = Some(*id);
                self.start_timers(cfg, *id);
            }
            many => {
                return Err(format!(
                    "more than one campaign is marked active: {many:?}; suspend all but one \
                     by restoring a backup, or remove the extra folders"
                ));
            }
        }
        Ok(())
    }

    /// The campaign a call names: 0 resolves to the active one. A campaign
    /// on disk that is not finished is opened.
    pub fn resolve(&mut self, id: u64, need: Need) -> Result<u64, Status> {
        if need == Need::Nothing && id == 0 {
            return Ok(self.active.unwrap_or(0));
        }
        let id = if id == 0 {
            self.active.ok_or_else(|| {
                status(
                    Code::FailedPrecondition,
                    ErrorReason::NoActiveCampaign,
                    "no campaign is active",
                    0,
                    "campaign_id",
                )
            })?
        } else {
            id
        };
        let slot = self.campaigns.get(&id).ok_or_else(|| {
            status(
                Code::NotFound,
                ErrorReason::CampaignNotFound,
                format!("no campaign {id}"),
                0,
                "campaign_id",
            )
        })?;
        if slot.meta.state == pb::CampaignState::Finished as i32 {
            if need != Need::Nothing {
                return Err(status(
                    Code::FailedPrecondition,
                    ErrorReason::CampaignReadOnly,
                    format!("campaign {id} is finished"),
                    0,
                    "campaign_id",
                ));
            }
        } else if need != Need::Activate {
            self.open_store(id)?;
        }
        if need == Need::Active && self.active != Some(id) {
            return Err(status(
                Code::FailedPrecondition,
                ErrorReason::CampaignNotActive,
                format!("campaign {id} is not the active one"),
                0,
                "campaign_id",
            ));
        }
        Ok(id)
    }

    /// The campaign a read names, active or not; 0 is the active one. A
    /// read opens no store; one that reads a store's contents makes a
    /// migrated copy of one that is not open first ([`Inner::open_archive`],
    /// [`Inner::read_store`]).
    pub fn resolve_read(&mut self, id: u64) -> Result<u64, Status> {
        let id = if id == 0 {
            self.active.ok_or_else(|| {
                status(
                    Code::FailedPrecondition,
                    ErrorReason::NoActiveCampaign,
                    "no campaign is active",
                    0,
                    "campaign_id",
                )
            })?
        } else {
            id
        };
        if !self.campaigns.contains_key(&id) {
            return Err(status(
                Code::NotFound,
                ErrorReason::CampaignNotFound,
                format!("no campaign {id}"),
                0,
                "campaign_id",
            ));
        }
        Ok(id)
    }

    /// The store a read of campaign `id` sees: for a finished campaign on
    /// disk, the migrated copy [`Inner::open_archive`] made; otherwise its
    /// own.
    pub fn read_store(&self, id: u64) -> &Store {
        let slot = &self.campaigns[&id];
        match self.archive.get(&id) {
            Some((copy, _)) if !slot.opened => copy.store(),
            _ => &slot.store,
        }
    }

    /// Make or keep the read copy of a campaign on disk whose store is not
    /// open (spec/persistence.md, "Reading a campaign that is not open");
    /// nothing for any other. A copy no read named for [`ARCHIVE_KEEP`], or
    /// of a campaign whose store opened since, is dropped by the step.
    pub fn open_archive(&mut self, id: u64) -> Result<(), Status> {
        let closed = self.campaigns.get(&id).is_some_and(|s| !s.opened);
        if !closed || self.files.is_none() {
            return Ok(());
        }
        if let Some((_, at)) = self.archive.get_mut(&id) {
            *at = Instant::now();
            return Ok(());
        }
        self.space_check(id, 0)?;
        let files = self.files.as_ref().expect("checked");
        let copy = files
            .read_copy(CampaignId(id), wall_ms())
            .map_err(|e| disk_status(id, &e))?;
        self.archive.insert(id, (copy, Instant::now()));
        Ok(())
    }

    /// Drop the read copies no read named for [`ARCHIVE_KEEP`].
    fn expire_archives(&mut self) {
        let campaigns = &self.campaigns;
        self.archive.retain(|id, (_, at)| {
            at.elapsed() < ARCHIVE_KEEP && campaigns.get(id).is_some_and(|s| !s.opened)
        });
    }

    /// Free bytes on the store's file system and the bytes campaign `id`
    /// holds there: its store, the store's WAL and its events file
    /// (spec/persistence.md, "Free space"). `None` with the store off.
    pub fn space(&self, id: Option<u64>) -> Option<(u64, u64)> {
        let files = self.files.as_ref()?;
        let free = nanoprobe_sidecar::space::free_bytes(files.dir()).ok()?;
        let held = self.space_extra.saturating_add(id.map_or(0, |id| {
            let store = files.store_path(CampaignId(id));
            let mut wal = store.clone().into_os_string();
            wal.push("-wal");
            [
                store,
                std::path::PathBuf::from(wal),
                files.events_path(CampaignId(id)),
            ]
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum()
        }));
        Some((free, held))
    }

    /// Refuse with `SPACE_LOW` when the store's file system has less free
    /// space than campaign `id` holds there plus `extra` bytes.
    pub fn space_check(&self, id: u64, extra: u64) -> Result<(), Status> {
        match self.space(Some(id)) {
            Some((free, held)) if free < held.saturating_add(extra) => Err(status(
                Code::ResourceExhausted,
                ErrorReason::SpaceLow,
                format!(
                    "the store's file system has {free} bytes free; campaign {id} needs {}",
                    held.saturating_add(extra)
                ),
                0,
                "",
            )),
            _ => Ok(()),
        }
    }

    /// The campaign folders, when the stores are on disk.
    pub fn files(&self) -> Option<&Campaigns> {
        self.files.as_ref()
    }

    /// Open a campaign's store file if it is on disk and not open yet
    /// (and migrate it, spec/persistence.md). A finished store is never
    /// opened.
    fn open_store(&mut self, id: u64) -> Result<(), Status> {
        let Some(files) = self.files.as_ref() else {
            return Ok(());
        };
        let Some(slot) = self.campaigns.get_mut(&id) else {
            return Ok(());
        };
        if slot.opened {
            return Ok(());
        }
        let store = files
            .open(CampaignId(id), wall_ms())
            .map_err(|e| disk_status(id, &e))?;
        slot.meta.last_checkpoint = store.last_checkpoint().map(checkpoint_info);
        // Its keys go into the server store's index: one written by a call
        // whose index row a crash lost is found again.
        let since = wall_ms().saturating_sub(ms(self.idem_keep));
        let keys = store
            .idempotency_since(since)
            .map_err(|e| disk_status(id, &e))?;
        slot.store = store;
        slot.opened = true;
        if let Some(server) = self.server.as_ref() {
            for (key, at_ms) in keys {
                let row = nanoprobe_sidecar::store::IdemRow {
                    key,
                    at_ms,
                    request_hash: Vec::new(),
                    response: Vec::new(),
                };
                server
                    .put_idempotency(&row, Some(id))
                    .map_err(|e| disk_status(id, &e))?;
            }
        }
        Ok(())
    }

    /// Write a campaign's store, if it is open on disk. A failure stops the
    /// sidecar through its panic path (spec/persistence.md, "Writes").
    fn commit(&mut self, id: u64) {
        if let Some(slot) = self.campaigns.get_mut(&id)
            && slot.opened
            && let Err(e) = slot.store.commit()
        {
            panic!("the store of campaign {id} could not be written, stopping: {e}");
        }
    }

    /// End a step or a call: write the store of the campaign it acted on
    /// and of the active one, then let the commands it sent through
    /// (spec/persistence.md, "Writes"). An acknowledged change is on disk
    /// before the call answers, and a command before it reaches DCS.
    fn settle(&mut self, acted: Option<u64>) {
        let t0 = self.timings.is_some().then(Instant::now);
        // The events first, then the stores (spec/persistence.md, "A crash
        // between the two commits").
        self.commit_events();
        for (id, state, outcome) in std::mem::take(&mut self.closing) {
            self.close_store(id, state, outcome);
        }
        if let Some(a) = self.active {
            self.epoch_floor = self.epoch_floor.max(self.campaigns[&a].store.epoch());
        }
        if let Some(id) = acted {
            self.commit(id);
        }
        if let Some(a) = self.active
            && Some(a) != acted
        {
            self.commit(a);
        }
        // A new epoch is on disk before a command of it reaches DCS.
        if let Some(server) = self.server.as_mut() {
            server.raise_epoch(self.epoch_floor);
            if let Err(e) = server.commit() {
                panic!("the server store could not be written, stopping: {e}");
            }
        }
        if let (Some(t), Some(t0)) = (self.timings.as_mut(), t0) {
            t.commit(t0.elapsed());
        }
        self.backend.sim().flush();
    }

    /// Print the store write times when a period is due (`crate::timings`).
    fn report_timings(&mut self) {
        let Some(t) = self.timings.as_mut() else {
            return;
        };
        let campaigns = &self.campaigns;
        let active = self.active;
        let units = || {
            active.and_then(|a| campaigns.get(&a)).map_or(0, |s| {
                s.store
                    .groups()
                    .filter(|g| !g.removed)
                    .map(|g| g.members.len())
                    .sum()
            })
        };
        if let Some(line) = t.due(Instant::now(), units) {
            eprintln!("{line}");
        }
    }

    /// The periods the sampling tiers achieve now, with the targets the
    /// campaign set; none when the backend does not know them.
    fn achieved(&self, id: u64) -> Vec<TierPeriod> {
        let Some(periods) = self.backend.achieved() else {
            return Vec::new();
        };
        let targets = self
            .campaigns
            .get(&id)
            .and_then(|s| s.store.rates())
            .map(|r| [r.position_s, r.vitals_s, r.warehouse_s, r.enumeration_s])
            .unwrap_or_default();
        TIERS
            .iter()
            .zip(periods)
            .zip(targets)
            .map(|((tier, achieved_s), target_s)| TierPeriod {
                tier: (*tier).to_owned(),
                target_s,
                achieved_s,
            })
            .collect()
    }

    /// Close a campaign's store with a final checkpoint and `state`
    /// (spec/persistence.md, "Campaign operations"); the slot then holds an
    /// empty store until the campaign is opened again. Nothing for a store
    /// in memory or one not open. A failure stops the sidecar.
    fn close_store(&mut self, id: u64, state: CampaignState, outcome: Option<Vec<u8>>) {
        let achieved = self.achieved(id);
        let (Some(files), Some(slot)) = (self.files.as_ref(), self.campaigns.get_mut(&id)) else {
            return;
        };
        if !slot.opened {
            return;
        }
        let at = wall_ms();
        let r = match outcome {
            Some(o) => files.finish(&mut slot.store, at, achieved, o),
            None if state == CampaignState::Finished => {
                files.finish(&mut slot.store, at, achieved, Vec::new())
            }
            None => files.suspend(&mut slot.store, at, achieved),
        };
        if let Err(e) = r {
            panic!("campaign {id} could not be closed, stopping: {e}");
        }
        slot.meta.last_checkpoint = slot.store.last_checkpoint().map(checkpoint_info);
        slot.store = Store::new(CampaignId(id), files.changes_keep());
        slot.opened = false;
    }

    /// Set the state in an open store's campaign record and write it.
    fn set_state(&mut self, id: u64, state: CampaignState) {
        let Some(slot) = self.campaigns.get_mut(&id) else {
            return;
        };
        if !slot.opened || !slot.store.on_disk() {
            return;
        }
        let Some(mut r) = slot.store.record().cloned() else {
            return;
        };
        r.state = state;
        if let Err(e) = slot.store.set_record(r) {
            panic!("the store of campaign {id} could not be written, stopping: {e}");
        }
    }

    /// Start the checkpoint and backup timers of the campaign just made
    /// active: a checkpoint `checkpoint_s` from now, a backup
    /// `backup_interval_h` after the newest one kept, or from now.
    fn start_timers(&mut self, cfg: &Config, id: u64) {
        let (Some(files), Some(sf)) = (self.files.as_ref(), cfg.store.as_ref()) else {
            return;
        };
        let now = wall_ms();
        let newest = files
            .backups(CampaignId(id))
            .ok()
            .and_then(|b| b.last().map(|b| b.at_ms))
            .unwrap_or(now);
        self.next_checkpoint_ms = now.saturating_add(ms(sf.checkpoint));
        self.next_backup_ms = newest.saturating_add(ms(sf.backup_interval));
    }

    /// The active campaign's checkpoint and scheduled backup, when due. A
    /// checkpoint that fails stops the sidecar; a backup that fails is
    /// reported in `backup_error` and taken again at the next interval.
    fn timers(&mut self, cfg: &Config) {
        let Some(sf) = cfg.store.as_ref() else {
            return;
        };
        let now = wall_ms();
        self.collect_backup(false);
        if now >= self.next_retention_ms {
            // Every hour, each log of a campaign not finished.
            for slot in self.campaigns.values_mut() {
                if slot.meta.state != pb::CampaignState::Finished as i32
                    && let Err(e) = retain(&mut slot.events, slot.meta.id, sf.event_retention)
                {
                    panic!(
                        "the event log of campaign {} could not be written, stopping: {e}",
                        slot.meta.id
                    );
                }
            }
            if let Err(e) = retain(&mut self.server_log, 0, sf.server_log_retention) {
                panic!("the server event log could not be written, stopping: {e}");
            }
            // Keys past `idempotency_keep_h`, from every store.
            let before = now.saturating_sub(ms(self.idem_keep));
            for slot in self.campaigns.values_mut() {
                if slot.opened
                    && let Err(e) = slot.store.prune_idempotency(before)
                {
                    panic!(
                        "the store of campaign {} could not be written, stopping: {e}",
                        slot.meta.id
                    );
                }
            }
            if let Some(server) = self.server.as_ref()
                && let Err(e) = server.prune_idempotency(before)
            {
                panic!("the server store could not be written, stopping: {e}");
            }
            self.next_retention_ms = now.saturating_add(RETENTION_EVERY_MS);
        }
        let Some(id) = self.active else {
            return;
        };
        if !self.campaigns.get(&id).is_some_and(|s| s.opened) {
            return;
        }
        if now >= self.next_checkpoint_ms {
            self.sync_clock(id);
            let achieved = self.achieved(id);
            let slot = self.campaigns.get_mut(&id).expect("active campaign");
            let t0 = Instant::now();
            match slot.store.checkpoint(now, achieved) {
                Ok(c) => slot.meta.last_checkpoint = Some(checkpoint_info(&c)),
                Err(e) => panic!("campaign {id} could not be checkpointed, stopping: {e}"),
            }
            if let Some(t) = self.timings.as_mut() {
                t.checkpoint(t0.elapsed());
            }
            self.next_checkpoint_ms = now.saturating_add(ms(sf.checkpoint));
        }
        if now >= self.next_backup_ms
            && !self.backing_up.contains(&id)
            && let Err(e) = self.space_check(id, 0)
        {
            eprintln!(
                "nanoprobe: the scheduled backup of campaign {id} is skipped: {}",
                e.message()
            );
            self.next_backup_ms = now.saturating_add(ms(sf.backup_interval));
        }
        if now >= self.next_backup_ms && !self.backing_up.contains(&id) {
            // The copy reads the file: write the store and its events first.
            self.commit_events();
            self.commit(id);
            let files = self.files.clone().expect("files with a store on disk");
            let revision = self.campaigns[&id].store.revision();
            self.backing_up.insert(id);
            // The server store is backed up on the same schedule; its
            // revision is the epoch.
            let server = self
                .server
                .as_ref()
                .map(|s| (s.backup_folder(sf.backup_keep), s.epoch()));
            let handle = std::thread::spawn(move || {
                let b = files.backup(CampaignId(id), now, revision, BackupReason::Scheduled)?;
                if let Some((folder, epoch)) = server {
                    folder.take(
                        now,
                        nanoprobe_sidecar::server::SCHEMA,
                        epoch,
                        BackupReason::Scheduled,
                    )?;
                }
                Ok(b)
            });
            self.backup_job = Some(BackupJob {
                campaign: id,
                handle,
            });
            self.next_backup_ms = now.saturating_add(ms(sf.backup_interval));
        }
    }

    /// Take the scheduled backup's result once its worker is done, or wait
    /// for it with `wait`. A failure is recorded in `backup_error` and the
    /// backup is taken again at the next interval.
    fn collect_backup(&mut self, wait: bool) {
        let done = self
            .backup_job
            .as_ref()
            .is_some_and(|j| wait || j.handle.is_finished());
        if !done {
            return;
        }
        let job = self.backup_job.take().expect("a job");
        self.backing_up.remove(&job.campaign);
        self.backup_error = match job.handle.join() {
            Ok(Ok(_)) => String::new(),
            Ok(Err(e)) => format!("campaign {}: {e}", job.campaign),
            Err(_) => format!("campaign {}: the backup worker panicked", job.campaign),
        };
        if !self.backup_error.is_empty() {
            eprintln!("nanoprobe: scheduled backup failed: {}", self.backup_error);
        }
    }

    /// Write every event log's appended events.
    fn commit_events(&mut self) {
        for slot in self.campaigns.values_mut() {
            if let Err(e) = slot.events.commit() {
                panic!(
                    "the event log of campaign {} could not be written, stopping: {e}",
                    slot.meta.id
                );
            }
        }
        if let Err(e) = self.server_log.commit() {
            panic!("the server event log could not be written, stopping: {e}");
        }
    }

    /// A new campaign's ID (spec/world.md, "Identities" and "Names"): the
    /// wall clock in milliseconds, or one more than the newest campaign,
    /// moved forward until its tag is free. Every campaign the stores know
    /// counts: those listed, finished ones included, folders that could not
    /// be read, and the deleted ones and the index entries of the server
    /// store.
    pub fn new_campaign_id(&self) -> u64 {
        let mut existing: Vec<CampaignId> = self
            .campaigns
            .keys()
            .copied()
            .chain(self.unreadable.iter().map(|(id, _)| *id))
            // A folder named past the largest ID is no campaign.
            .filter(|id| *id <= nanoprobe_sidecar::ids::MAX_ID)
            .map(CampaignId)
            .collect();
        if let Some(server) = self.server.as_ref() {
            match server.known_campaigns() {
                Ok(ids) => existing.extend(ids.into_iter().map(CampaignId)),
                Err(e) => panic!("the server store could not be read, stopping: {e}"),
            }
        }
        nanoprobe_sidecar::ids::new_campaign(wall_ms(), &existing).0
    }

    /// Add a campaign, suspended, to be loaded on `theater`. With the
    /// stores on disk its folder is made, its store opened.
    pub fn add_campaign(
        &mut self,
        cfg: &Config,
        meta: pb::Campaign,
        theater: String,
    ) -> Result<(), Status> {
        let id = meta.id;
        let (store, events) = match self.files.as_ref() {
            Some(files) => {
                let record = record_from(&meta, &theater);
                let store = files
                    .create(CampaignId(id), record)
                    .map_err(|e| disk_status(id, &e))?;
                let events = files
                    .events(CampaignId(id))
                    .and_then(|f| EventLog::open(f, cfg.events_keep))
                    .map_err(|e| disk_status(id, &e))?;
                (store, events)
            }
            None => (
                Store::new(CampaignId(id), cfg.world_changes_keep),
                EventLog::new(cfg.events_keep),
            ),
        };
        self.campaigns.insert(
            id,
            Slot {
                meta,
                store,
                events,
                on_lost: self.on_lost,
                theater,
                opened: true,
            },
        );
        Ok(())
    }

    /// Tell the agent no campaign is active (`CAMPAIGN 0`), now if it is
    /// attached, otherwise after its next `HELLO`.
    fn send_no_campaign(&mut self) {
        let (op, payload) = Command {
            header: CommandHeader::default(),
            body: Body::Campaign(0),
        }
        .encode();
        if self.backend.sim().send(op, &payload).is_err() {
            self.clear_campaign = true;
        }
    }

    /// The theater check of a call that activates `campaign` (0 for one
    /// it creates) on `theater`, before the call captures or creates
    /// anything. A repeat of a call whose key is answered or running, here
    /// or in a store from before a restart, is left to the write, which
    /// gives it the first answer; so is a call without the lease, which the
    /// write refuses first, and a finished target, which it refuses too.
    pub fn activation_check(
        &mut self,
        key: Option<&pb::Idempotency>,
        lease: Option<&pb::Lease>,
        campaign: u64,
        theater: &str,
    ) -> Result<(), Status> {
        let now = Instant::now();
        if let Some(k) = key.map(|k| k.key.as_str()).filter(|k| !k.is_empty()) {
            if self.idem.running(k) || self.idem.get(now, k).is_some() {
                return Ok(());
            }
            if let Some((d, done, age)) = self.recall(k, campaign) {
                self.idem.restore(now, k, d, done, age);
                return Ok(());
            }
        }
        self.leases
            .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
            .map_err(lease_status)?;
        let finished = self
            .campaigns
            .get(&campaign)
            .is_some_and(|s| s.meta.state == pb::CampaignState::Finished as i32);
        if theater.is_empty() || finished {
            return Ok(());
        }
        // A campaign the call creates has no settings to render yet.
        if campaign == 0 {
            return self.mission_check(campaign, theater);
        }
        self.render_shell(campaign, theater).map(|_| ())
    }

    /// Whether campaign `id` (0 for one a call creates) has a template it
    /// can load on `theater`: its own, or the shared one in `[missions]
    /// dir` (spec/config.md). A theater with none is refused, before
    /// anything is created, suspended, opened or sent. A backend that loads
    /// its missions itself needs none.
    pub fn mission_check(&self, id: u64, theater: &str) -> Result<(), Status> {
        let Some(template) = self.template_for(id, theater)? else {
            return Ok(());
        };
        nanoprobe_sidecar::shell::check(&template, theater)
            .map_err(|e| precondition(format!("the template for theater {theater:?}: {e}")))
    }

    /// The template campaign `id` loads on `theater`: its own (plan
    /// revision 35), else the shared one; `None` for a backend that loads
    /// its missions itself.
    fn template_for(&self, id: u64, theater: &str) -> Result<Option<Vec<u8>>, Status> {
        let Some(m) = self.backend.missions() else {
            return Ok(None);
        };
        if let Some(own) = self.own_template(id, theater)? {
            return Ok(Some(own));
        }
        m.template(theater).map(Some).map_err(precondition)
    }

    /// Campaign `id`'s own template for `theater`, read from its store,
    /// opened or not.
    fn own_template(&self, id: u64, theater: &str) -> Result<Option<Vec<u8>>, Status> {
        let Some(slot) = self.campaigns.get(&id) else {
            return Ok(None);
        };
        if slot.opened {
            return Ok(slot.store.template(theater).map(<[u8]>::to_vec));
        }
        match self.files.as_ref() {
            Some(f) => {
                nanoprobe_sidecar::store::template_of(&f.store_path(CampaignId(id)), theater)
                    .map_err(|e| disk_status(id, &e))
            }
            None => Ok(None),
        }
    }

    /// Campaign `id`'s next-mission settings, from its store, opened or
    /// not. A value that does not read is logged and taken as none.
    pub fn next_of(&self, id: u64) -> crate::next::Next {
        let Some(slot) = self.campaigns.get(&id) else {
            return crate::next::Next::default();
        };
        let lv = if slot.opened {
            slot.store.next_mission().cloned()
        } else {
            self.files.as_ref().and_then(|f| {
                nanoprobe_sidecar::store::next_mission_of(&f.store_path(CampaignId(id)))
                    .ok()
                    .flatten()
            })
        };
        lv.map(|v| {
            crate::next::Next::from_lv(&v).unwrap_or_else(|e| {
                eprintln!("campaign {id}: {e}; the next mission keeps the template's settings");
                crate::next::Next::default()
            })
        })
        .unwrap_or_default()
    }

    /// Campaign `id`'s clock, ms since the Unix epoch: the start written
    /// into the running mission's shell plus the model time of the last
    /// samples, else the clock its record holds (plan 4.6, "Clock").
    pub fn clock_ms(&self, id: u64) -> Option<i64> {
        let slot = self.campaigns.get(&id)?;
        if slot.opened
            && let Some(start) = slot.store.mission_start_ms()
        {
            let model_ms = (slot.store.model_elapsed() * 1000.0) as i64;
            return Some(start.saturating_add(model_ms));
        }
        slot.meta.clock.as_ref().map(ms_of)
    }

    /// Write the campaign clock into its record, in memory and in its open
    /// store.
    fn sync_clock(&mut self, id: u64) {
        if let Some(ms) = self.clock_ms(id) {
            self.write_clock(id, ms);
        }
    }

    /// Write `ms` as campaign `id`'s clock into its record, in memory and in
    /// its open store.
    fn write_clock(&mut self, id: u64, ms: i64) {
        let Some(slot) = self.campaigns.get_mut(&id) else {
            return;
        };
        let ts = prost_types::Timestamp {
            seconds: ms.div_euclid(1000),
            nanos: i32::try_from(ms.rem_euclid(1000) * 1_000_000).unwrap_or(0),
        };
        slot.meta.clock = Some(ts);
        if slot.opened
            && slot.store.on_disk()
            && let Some(mut r) = slot.store.record().cloned()
            && r.clock_ms != Some(ms)
        {
            r.clock_ms = Some(ms);
            // Written by the commit of the change it belongs to.
            slot.store.stage_record(r);
        }
    }

    /// Write campaign `id`'s shell for `theater` into `[missions] dir` and
    /// return the load it owes; `None` for a backend that loads its missions
    /// itself (spec/persistence.md, "Campaign operations"). The shell goes to
    /// the slot the last owed or applied load did not name, so a file DCS
    /// may hold open is never rewritten. It changes only that file: a
    /// refusal leaves every campaign as it was. The caller then calls
    /// [`Inner::shell_written`].
    pub fn write_shell(&self, id: u64, theater: &str) -> Result<Option<OwedLoad>, Status> {
        let Some((bytes, start)) = self.render_shell(id, theater)? else {
            return Ok(None);
        };
        let m = self
            .backend
            .missions()
            .expect("a template comes from [missions]");
        let dir = m.dir().map_err(precondition)?;
        let slot = nanoprobe_sidecar::shell::other_slot(self.last_slot(id));
        nanoprobe_sidecar::shell::save(dir, id, slot, &bytes).map_err(|e| {
            precondition(format!(
                "the shell of campaign {id} could not be written: {e}"
            ))
        })?;
        Ok(Some(OwedLoad {
            path: m.shell_dcs_path(id, slot),
            cmd: None,
            start_ms: Some(start),
        }))
    }

    /// Campaign `id`'s shell for `theater` and its start, ms since the
    /// Unix epoch, without saving it; `None` for a backend that loads its
    /// missions itself. Every refusal of a shell write but the save's.
    fn render_shell(&self, id: u64, theater: &str) -> Result<Option<(Vec<u8>, i64)>, Status> {
        use nanoprobe_sidecar::shell::{self, Runtime, Start};
        let Some(template) = self.template_for(id, theater)? else {
            return Ok(None);
        };
        shell::check(&template, theater)
            .map_err(|e| precondition(format!("the template for theater {theater:?}: {e}")))?;
        // Settings made for another theater wait for a load of it.
        let next = Some(self.next_of(id))
            .filter(|n| n.theater.is_empty() || n.theater == theater)
            .unwrap_or_default();
        let airports = shell::airports(&template).map_err(precondition)?;
        if let Some(d) = next.dynamic.iter().find(|d| !airports.contains(&d.airbase)) {
            return Err(crate::convert::invalid(
                "next.dynamic_spawn",
                format!(
                    "airport {} is not in the {theater} template's warehouses",
                    d.airbase
                ),
            ));
        }
        let start = match next.start_ms.or_else(|| self.clock_ms(id)) {
            Some(ms) => Start::from_unix_ms(ms),
            None => shell::start_of(&template).map_err(precondition)?,
        };
        let rt = Runtime {
            start: Some(start),
            weather: next.weather.clone(),
            dynamic: next.dynamic.clone(),
        };
        let bytes = shell::write(&template, &rt)
            .map_err(|e| precondition(format!("the shell for theater {theater:?}: {e}")))?;
        Ok(Some((bytes, start.unix_ms())))
    }

    /// The shell slot campaign `id`'s owed load names, else the slot of the
    /// last load DCS applied, from its store, opened or not.
    fn last_slot(&self, id: u64) -> Option<u8> {
        let slot = self.campaigns.get(&id)?;
        if let Some(o) = slot.store.owed_load() {
            return nanoprobe_sidecar::shell::slot_of(id, &o.path);
        }
        if slot.opened {
            return slot.store.shell_slot();
        }
        let files = self.files.as_ref()?;
        nanoprobe_sidecar::store::shell_slot_of(&files.store_path(CampaignId(id)))
            .ok()
            .flatten()
    }

    /// After a shell was written for campaign `id` on `theater`: a start
    /// the next-mission settings set for it is cleared, in the commit that
    /// owes the load (spec/persistence.md, "Mission shells").
    fn shell_written(&mut self, id: u64, theater: &str) {
        let Some(slot) = self.campaigns.get_mut(&id) else {
            return;
        };
        // The start was used only if the settings were for this theater.
        // The clock takes it when the load is applied: a refused load
        // leaves the clock where it was.
        if let Some(mut next) = slot
            .store
            .next_mission()
            .and_then(|v| crate::next::Next::from_lv(v).ok())
            && (next.theater.is_empty() || next.theater == theater)
            && next.start_ms.take().is_some()
            && let Ok(lv) = next.to_lv()
        {
            slot.store.set_next_mission(Some(lv));
        }
    }

    /// Make a campaign the active one, suspending the one before. The
    /// caller resolved `id`, which opened its store.
    pub fn activate(&mut self, cfg: &Config, id: u64) -> Result<(), Status> {
        if self.active == Some(id) {
            return Ok(());
        }
        // The mission comes first: without one, nothing is suspended, opened
        // or sent (spec/persistence.md, "Activate").
        let theater = self
            .campaigns
            .get(&id)
            .map(|s| s.theater.clone())
            .unwrap_or_default();
        let owed = self.write_shell(id, &theater)?;
        if let Some(old) = self.active.take() {
            self.end_run(old);
            self.close_store(old, CampaignState::Suspended, None);
            if let Some(s) = self.campaigns.get_mut(&old) {
                s.meta.state = pb::CampaignState::Suspended as i32;
                s.meta.loaded_theater.clear();
            }
        }
        // Opened after the suspend: if the open fails, no campaign is active.
        if let Err(e) = self.open_store(id) {
            self.send_no_campaign();
            self.restart(cfg);
            return Err(e);
        }
        // The load is owed in the same commit as the state `active`
        // (spec/persistence.md, "Writes").
        self.hold_after = None;
        let loads_itself = owed.is_none();
        if let Some(owed) = owed {
            self.shell_written(id, &theater);
            if let Some(s) = self.campaigns.get_mut(&id) {
                s.store.set_owed_load(Some(owed));
            }
        }
        self.set_state(id, CampaignState::Active);
        if let Some(s) = self.campaigns.get_mut(&id) {
            s.meta.state = pb::CampaignState::Active as i32;
        }
        self.active = Some(id);
        self.start_timers(cfg, id);
        self.restart(cfg);
        // A campaign is restored on the path of a mission load: the
        // reconciler learns the loaded theater from the load's LIFECYCLE.
        // A real DCS loads the owed file once the reconciler can send
        // (`step`); the fake loads its mission now.
        if loads_itself {
            self.backend.load_mission(&theater);
        }
        Ok(())
    }

    /// Reload the active campaign's mission (plan 4.4; spec/persistence.md,
    /// "Campaign operations", Reload): write its shell for the next
    /// mission's theater, or the loaded one, and owe the load as an
    /// activation does. The caller captured the live groups first.
    pub fn reload(&mut self, cfg: &Config, id: u64) -> Result<(), Status> {
        let theater = self.reload_theater(id)?;
        // The clock goes into the shell before the running mission ends.
        self.sync_clock(id);
        let owed = self.write_shell(id, &theater)?;
        let slot = self.campaigns.get_mut(&id).expect("resolved");
        slot.theater = theater.clone();
        if slot.opened
            && slot.store.on_disk()
            && let Some(mut r) = slot.store.record().cloned()
            && r.theater != theater
        {
            r.theater = theater.clone();
            slot.store.stage_record(r);
        }
        self.hold_after = None;
        match owed {
            Some(o) => {
                self.shell_written(id, &theater);
                let slot = self.campaigns.get_mut(&id).expect("resolved");
                slot.store.set_owed_load(Some(o));
                // The checkpoint goes into the commit that owes the load.
                let achieved = self.achieved(id);
                let slot = self.campaigns.get_mut(&id).expect("resolved");
                if slot.opened
                    && slot.store.on_disk()
                    && let Err(e) = slot.store.checkpoint(wall_ms(), achieved)
                {
                    panic!("campaign {id} could not be checkpointed, stopping: {e}");
                }
            }
            None => {
                self.restart(cfg);
                self.backend.load_mission(&theater);
            }
        }
        Ok(())
    }

    /// Delete a campaign that is not active: close its store, remove its
    /// folder and its shell (spec/persistence.md, "Campaign operations").
    /// `confirm` must equal its name.
    pub fn delete_campaign(&mut self, cfg: &Config, id: u64, confirm: &str) -> Result<(), Status> {
        let slot = self.campaigns.get(&id).ok_or_else(|| {
            status(
                Code::NotFound,
                ErrorReason::CampaignNotFound,
                format!("no campaign {id}"),
                0,
                "campaign_id",
            )
        })?;
        if confirm.is_empty() {
            return Err(status(
                Code::FailedPrecondition,
                ErrorReason::ConfirmationRequired,
                "a delete needs confirm_name, the campaign's name",
                0,
                "confirm_name",
            ));
        }
        if slot.meta.name != confirm {
            return Err(crate::convert::invalid(
                "confirm_name",
                "must equal the campaign's name",
            ));
        }
        if self.active == Some(id) || self.backing_up.contains(&id) {
            return Err(status(
                Code::FailedPrecondition,
                ErrorReason::Unspecified,
                format!("campaign {id} is active or backing up; suspend it and wait first"),
                0,
                "campaign_id",
            ));
        }
        if slot.opened && slot.store.on_disk() {
            let state = slot
                .store
                .record()
                .map_or(CampaignState::Suspended, |r| r.state);
            self.close_store(id, state, None);
        }
        self.closing.retain(|(c, ..)| *c != id);
        // Every file of the folder is closed before it is removed: a read
        // copy and the events file too (an open file stops a removal on
        // Windows).
        self.archive.remove(&id);
        let mut slot = self.campaigns.remove(&id).expect("checked");
        let keep = cfg.events_keep;
        drop(std::mem::replace(&mut slot.events, EventLog::new(keep)));
        if let Some(files) = self.files.as_ref()
            && let Err(e) = files.delete(CampaignId(id))
        {
            // What is left is still the campaign: it stays listed.
            if let Ok(l) = files
                .events(CampaignId(id))
                .and_then(|f| EventLog::open(f, keep))
            {
                slot.events = l;
            }
            self.campaigns.insert(id, slot);
            return Err(status(
                Code::Internal,
                ErrorReason::Internal,
                format!("the folder of campaign {id} could not be removed: {e}"),
                0,
                "",
            ));
        }
        if let Some(dir) = self.backend.missions().and_then(|m| m.dir().ok()) {
            for slot in *b"ab" {
                let shell = dir
                    .join(nanoprobe_sidecar::shell::SHELLS)
                    .join(nanoprobe_sidecar::shell::shell_name(id, slot));
                let _ = std::fs::remove_file(shell);
            }
        }
        Ok(())
    }

    /// Set or remove campaign `id`'s own template for `theater` (plan
    /// revision 35). An empty `miz` removes it.
    pub fn set_template(&mut self, id: u64, theater: &str, miz: &[u8]) -> Result<(), Status> {
        use nanoprobe_sidecar::shell;
        let slot = self.campaigns.get(&id).expect("resolved");
        if !slot.meta.theaters.iter().any(|t| t == theater) {
            return Err(crate::convert::invalid(
                "theater",
                format!("{theater:?} is not one of the campaign's theaters"),
            ));
        }
        if !miz.is_empty() {
            if miz.len() > shell::TEMPLATE_MAX {
                return Err(crate::convert::invalid(
                    "miz",
                    format!("a template is at most {} bytes", shell::TEMPLATE_MAX),
                ));
            }
            self.space_check(id, miz.len() as u64)?;
            shell::check(miz, theater).map_err(|e| crate::convert::invalid("miz", e))?;
            let names = shell::placed_names(miz).map_err(|e| crate::convert::invalid("miz", e))?;
            if let Some(n) = names
                .iter()
                .find(|n| nanoprobe_transport::names::parse(n).is_some())
            {
                return Err(crate::convert::invalid(
                    "miz",
                    format!("the template places {n:?}, which reads as a platform name"),
                ));
            }
        }
        let slot = self.campaigns.get_mut(&id).expect("resolved");
        slot.store
            .set_template(theater, (!miz.is_empty()).then(|| miz.to_vec()));
        slot.meta.templates = slot.store.template_theaters().map(str::to_owned).collect();
        Ok(())
    }

    /// The theater a reload of campaign `id` loads, after the refusals that
    /// come before anything is captured: an owed load, a theater not the
    /// campaign's, and no template for it.
    fn reload_theater(&self, id: u64) -> Result<String, Status> {
        let slot = self.campaigns.get(&id).expect("resolved");
        if slot.store.owed_load().is_some() {
            return Err(status(
                Code::FailedPrecondition,
                ErrorReason::Unspecified,
                "a mission load is owed already; wait for its new mission",
                0,
                "",
            ));
        }
        let next = self.next_of(id);
        let theater = if !next.theater.is_empty() {
            next.theater.clone()
        } else if let Some(l) = slot
            .store
            .loaded()
            .filter(|l| !l.theater.is_empty() && slot.meta.theaters.contains(&l.theater))
        {
            // A DCS restart can leave a mission on a map the campaign lacks:
            // the reload then brings the campaign back to its own map.
            l.theater.clone()
        } else {
            slot.theater.clone()
        };
        // Every refusal of the shell but its save comes before the capture.
        self.render_shell(id, &theater)?;
        Ok(theater)
    }

    /// The checks of a `Reload` before the call captures anything, as
    /// [`Inner::activation_check`] does for an activation: a repeat of an
    /// answered or running key, and a call without the lease, are left to
    /// the write.
    pub fn reload_check(
        &mut self,
        key: Option<&pb::Idempotency>,
        lease: Option<&pb::Lease>,
        campaign: u64,
    ) -> Result<(), Status> {
        let now = Instant::now();
        if let Some(k) = key.map(|k| k.key.as_str()).filter(|k| !k.is_empty()) {
            if self.idem.running(k) || self.idem.get(now, k).is_some() {
                return Ok(());
            }
            if let Some((d, done, age)) = self.recall(k, campaign) {
                self.idem.restore(now, k, d, done, age);
                return Ok(());
            }
        }
        if self
            .leases
            .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
            .is_err()
        {
            return Ok(());
        }
        let id = self.resolve(campaign, Need::Active)?;
        self.reload_theater(id).map(|_| ())
    }

    /// Set campaign `id`'s next-mission settings.
    pub fn set_next(&mut self, id: u64, next: &crate::next::Next) -> Result<(), Status> {
        let slot = self.campaigns.get_mut(&id).expect("resolved");
        if !next.theater.is_empty() && !slot.meta.theaters.contains(&next.theater) {
            return Err(crate::convert::invalid(
                "next.theater",
                format!("{:?} is not one of the campaign's theaters", next.theater),
            ));
        }
        let lv = next
            .to_lv()
            .map_err(|e| crate::convert::invalid("next.weather", e))?;
        slot.store.set_next_mission(Some(lv));
        Ok(())
    }

    /// The steps of leaving campaign `id`'s running mission, for a suspend
    /// and for a switch: its clock goes into the record, the slot of a load
    /// already sent is kept, and the owed load and the mission start are
    /// cleared.
    fn end_run(&mut self, id: u64) {
        self.sync_clock(id);
        if let Some(s) = self.campaigns.get_mut(&id) {
            // A load already sent may still run: its file is not written
            // over next.
            if let Some(o) = s.store.owed_load().filter(|o| o.cmd.is_some()).cloned() {
                s.store.set_shell_slot(slot_in(&o.path));
            }
            s.store.set_owed_load(None);
            s.store.set_mission_start_ms(None);
        }
    }

    /// Suspend a campaign, or finish it with `outcome` (the encoded
    /// `google.protobuf.Struct`). The active one first tells the agent no
    /// campaign is active; then its store takes a final checkpoint and is
    /// closed (spec/persistence.md, "Campaign operations"). DCS keeps
    /// running whatever it holds, unmanaged. The caller captured the live
    /// groups before.
    pub fn suspend(&mut self, cfg: &Config, id: u64, outcome: Option<Vec<u8>>) {
        let finished = outcome.is_some();
        if self.active == Some(id) {
            self.hold_after = None;
            self.send_no_campaign();
        }
        self.end_run(id);
        let state = if finished {
            CampaignState::Finished
        } else {
            CampaignState::Suspended
        };
        // Closed at the end of the call, so the call's key goes into the
        // store in the closing transaction (spec/persistence.md,
        // "Idempotency keys").
        self.closing.push((id, state, outcome.clone()));
        if let Some(s) = self.campaigns.get_mut(&id) {
            s.meta.state = if finished {
                pb::CampaignState::Finished as i32
            } else {
                pb::CampaignState::Suspended as i32
            };
            s.meta.loaded_theater.clear();
            if let Some(o) = outcome {
                s.meta.outcome = prost_types::Struct::decode(o.as_slice()).ok();
            }
        }
        if self.active == Some(id) {
            self.active = None;
            self.restart(cfg);
        }
    }

    /// Close every open store cleanly, for a sidecar that stops: the active
    /// one with a checkpoint, keeping its state, so the next start resumes
    /// it. A failure is returned; the rest are still closed.
    pub fn close_all(&mut self) -> Result<(), String> {
        self.collect_backup(true);
        for (id, state, outcome) in std::mem::take(&mut self.closing) {
            self.close_store(id, state, outcome);
        }
        let mut errors = Vec::new();
        for slot in self.campaigns.values_mut() {
            if let Err(e) = slot.events.commit() {
                errors.push(format!("events of campaign {}: {e}", slot.meta.id));
            }
        }
        if let Err(e) = self.server_log.commit() {
            errors.push(format!("server events: {e}"));
        }
        if let Some(server) = self.server.as_mut() {
            server.raise_epoch(self.epoch_floor);
            if let Err(e) = server.close() {
                errors.push(format!("server store: {e}"));
            }
        }
        // The next sidecar may take the store dir, even while handlers of
        // this one still hold the hub.
        self.lock = None;
        if let Some(id) = self.active {
            self.sync_clock(id);
        }
        let ids: Vec<u64> = self.campaigns.keys().copied().collect();
        for id in ids {
            let achieved = self.achieved(id);
            let Some(slot) = self.campaigns.get_mut(&id) else {
                continue;
            };
            if !slot.opened || !slot.store.on_disk() {
                continue;
            }
            let r = if self.active == Some(id) {
                slot.store
                    .checkpoint(wall_ms(), achieved)
                    .map(|_| ())
                    .and_then(|()| slot.store.close())
            } else {
                slot.store.close()
            };
            if let Err(e) = r {
                errors.push(format!("campaign {id}: {e}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

/// Apply the event retention to a log (spec/persistence.md, "Event
/// history"). A clock behind the newest event deletes nothing and is logged.
fn retain(l: &mut EventLog, campaign: u64, retention: Duration) -> Result<(), DiskError> {
    if retention.is_zero() {
        return Ok(());
    }
    if let nanoprobe_sidecar::eventfile::Retained::ClockBehind { now_ms, newest_ms } =
        l.retain(retention, wall_ms())?
    {
        eprintln!(
            "nanoprobe: the clock ({now_ms} ms) is behind the newest event of log {campaign} \
             ({newest_ms} ms): its retention is skipped"
        );
    }
    Ok(())
}

/// The server's keyring: the secret the server store keeps, made and kept
/// on first use, and every pair it holds. A secret that cannot be read
/// stops the server: a new one would give every player a new key.
fn keyring_of(server: &ServerStore) -> Result<Keyring, nanoprobe_sidecar::store::DiskError> {
    let secret = match server.player_secret()? {
        Some(s) => s,
        None => {
            let s = Keyring::generate().secret();
            server.set_player_secret(s)?;
            s
        }
    };
    let mut keys = Keyring::from_secret(secret);
    for (key, ucid) in server.players()? {
        keys.restore(&key, &ucid);
    }
    Ok(keys)
}

/// Append an event unless one made from the same `source` is in the log.
fn push_once(
    log: &mut EventLog,
    campaign: u64,
    stamp: pb::Stamp,
    body: event::Body,
    source: Option<String>,
) {
    if let Some(s) = source.as_deref() {
        match log.has_source(s) {
            Ok(false) => {}
            Ok(true) => return,
            Err(e) => panic!("the event log could not be read, stopping: {e}"),
        }
    }
    log.push_from(campaign, stamp, body, source);
}

/// How often the event retention runs.
const RETENTION_EVERY_MS: u64 = 3_600_000;

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn timestamp_ms(ms: u64) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: i64::try_from(ms / 1000).unwrap_or(i64::MAX),
        nanos: i32::try_from((ms % 1000) * 1_000_000).unwrap_or(0),
    }
}

fn ms_of(t: &prost_types::Timestamp) -> i64 {
    t.seconds
        .saturating_mul(1000)
        .saturating_add(i64::from(t.nanos / 1_000_000))
}

/// The API's view of a checkpoint (spec/persistence.md, "Achieved
/// periods"): the achieved periods by tier; the targets are not shown.
pub fn checkpoint_info(c: &Checkpoint) -> pb::CheckpointInfo {
    let mut rates = pb::SampleRates::default();
    for t in &c.achieved {
        let Ok(d) = Duration::try_from_secs_f64(t.achieved_s) else {
            continue;
        };
        let d = prost_types::Duration::try_from(d).ok();
        match t.tier.as_str() {
            "position" => rates.position = d,
            "vitals" => rates.vitals = d,
            "warehouse" => rates.warehouse = d,
            "enumeration" => rates.enumeration = d,
            _ => {}
        }
    }
    pb::CheckpointInfo {
        taken: Some(timestamp_ms(c.at_ms)),
        sim: Some(pb::Stamp {
            wall: None,
            model_time: c.model_time,
            mission_generation: c.mission_gen,
            frame: 0,
        }),
        achieved: (!c.achieved.is_empty()).then_some(rates),
    }
}

/// The campaign record of a new campaign.
fn record_from(meta: &pb::Campaign, theater: &str) -> CampaignRecord {
    CampaignRecord {
        name: meta.name.clone(),
        state: CampaignState::Suspended,
        theaters: meta.theaters.clone(),
        theater: theater.to_owned(),
        clock_ms: meta.clock.as_ref().map(ms_of),
        created_ms: meta
            .created
            .as_ref()
            .map_or(0, |t| u64::try_from(ms_of(t)).unwrap_or(0)),
        component_schemas: meta.component_schemas.clone().into_iter().collect(),
        outcome: None,
    }
}

/// The API's campaign from a stored record.
fn meta_from(id: u64, schema: u32, r: &CampaignRecord, cp: Option<&Checkpoint>) -> pb::Campaign {
    pb::Campaign {
        id,
        name: r.name.clone(),
        state: match r.state {
            CampaignState::Active => pb::CampaignState::Active,
            CampaignState::Suspended => pb::CampaignState::Suspended,
            CampaignState::Finished => pb::CampaignState::Finished,
        } as i32,
        theaters: r.theaters.clone(),
        loaded_theater: String::new(),
        clock: r.clock_ms.map(|c| prost_types::Timestamp {
            seconds: c.div_euclid(1000),
            nanos: i32::try_from(c.rem_euclid(1000) * 1_000_000).unwrap_or(0),
        }),
        platform_schema: schema,
        component_schemas: r.component_schemas.clone().into_iter().collect(),
        created: Some(timestamp_ms(r.created_ms)),
        templates: Vec::new(),
        outcome: r
            .outcome
            .as_ref()
            .and_then(|o| prost_types::Struct::decode(o.as_slice()).ok()),
        last_checkpoint: cp.map(checkpoint_info),
    }
}

/// The status for a store that cannot be created, opened or read.
pub fn disk_status(id: u64, e: &DiskError) -> Status {
    status(
        Code::Internal,
        ErrorReason::Internal,
        format!("the store of campaign {id}: {e}"),
        0,
        "campaign_id",
    )
}

impl Hub {
    /// A hub over a backend. Nothing is attached until [`Hub::start`].
    ///
    /// # Panics
    ///
    /// With `cfg.store` set, when [`Hub::open`] fails.
    pub fn new(cfg: Config, backend: Box<dyn Backend>) -> Arc<Hub> {
        Hub::open(cfg, backend).unwrap_or_else(|e| panic!("{e}"))
    }

    /// A hub over a backend, with the campaigns in `cfg.store`'s folder:
    /// each listed from its record without being opened, its history read
    /// and compacted (`event_retention_days`). A campaign whose record says
    /// active is opened and is the active one again: the sidecar stopped
    /// while it ran. A folder that cannot be read is skipped and listed in
    /// [`Inner::unreadable`]. Nothing is attached until [`Hub::start`].
    pub fn open(cfg: Config, backend: Box<dyn Backend>) -> Result<Arc<Hub>, String> {
        let (tick, _) = watch::channel(0);
        let hub = Arc::new(Hub {
            inner: Mutex::new(Inner {
                backend,
                campaigns: BTreeMap::new(),
                active: None,
                rec: None,
                sidecar_boot: wall_ms(),
                leases: Leases::new(cfg.lease_ttl, wall_ms()),
                idem: Idem::new(cfg.idempotency_keep),
                server_log: EventLog::new(cfg.events_keep),
                link: Link::default(),
                // With persistence on, `load` replaces it with the server
                // store's keyring.
                roster: Roster::new(Keyring::generate()),
                loadouts: Loadouts::default(),
                moderation: Vec::new(),
                players_cmd: None,
                on_lost: cfg.on_lost,
                clear_campaign: false,
                files: None,
                next_checkpoint_ms: 0,
                next_backup_ms: 0,
                backup_error: String::new(),
                unreadable: Vec::new(),
                epoch_floor: 0,
                next_retention_ms: 0,
                backup_job: None,
                backing_up: std::collections::BTreeSet::new(),
                hold_after: None,
                hung: false,
                archive: BTreeMap::new(),
                space_extra: 0,
                server: None,
                lock: None,
                idem_keep: cfg.idempotency_keep,
                crash_after_events: false,
                closing: Vec::new(),
                timings: crate::timings::Timings::from_env(),
            }),
            tick,
            settled: Notify::new(),
            closing: std::sync::atomic::AtomicBool::new(false),
            poisoned: std::sync::atomic::AtomicBool::new(false),
            cfg,
        });
        if let Some(sf) = hub.cfg.store.clone() {
            let mut g = hub.lock();
            g.load(&hub.cfg, &sf)?;
        }
        Ok(hub)
    }

    /// The shared state. Never hold the guard across an `.await`.
    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves the state as it was at the
        // panic; the server keeps serving it.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A receiver that changes after every step and every change.
    pub fn tick(&self) -> watch::Receiver<u64> {
        self.tick.subscribe()
    }

    /// The server is stopping: every stream ends with `UNAVAILABLE` at its
    /// next wake-up.
    pub fn close(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.bump();
    }

    /// Whether [`Hub::close`] was called.
    pub fn closing(&self) -> bool {
        self.closing.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A panic left the state possibly half-changed: stop. Every stream
    /// ends, every later call is refused, and the driver returns an error.
    pub fn poison(&self) {
        self.poisoned
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.close();
    }

    /// Run a changing call that does not go through [`Hub::write`] (the
    /// lease calls): refused while stopping, and a panic in it poisons.
    pub fn guarded<R>(&self, f: impl FnOnce(&mut Inner) -> Result<R, Status>) -> Result<R, Status> {
        let mut g = self.lock();
        if self.closing() {
            return Err(crate::services::stopping());
        }
        let inner: &mut Inner = &mut g;
        catch_unwind(AssertUnwindSafe(|| {
            let r = f(inner);
            inner.settle(None);
            r
        }))
        .unwrap_or_else(|_| {
            drop(g);
            self.poison();
            Err(status(
                Code::Internal,
                ErrorReason::Internal,
                "the call failed inside the server, which stops",
                0,
                "",
            ))
        })
    }

    /// Wake the streams.
    pub fn bump(&self) {
        self.tick.send_modify(|n| *n += 1);
    }

    /// Attach to the agent for the first time.
    pub fn start(&self) {
        let mut g = self.lock();
        let cfg = self.cfg.clone();
        g.restart(&cfg);
    }

    /// One driver step. A panic in it is returned as an error: the state
    /// may be half-changed, so the driver stops and the server with it.
    pub fn step(&self) -> Result<(), String> {
        let r = {
            let mut g = self.lock();
            catch_unwind(AssertUnwindSafe(|| g.step(&self.cfg)))
        };
        if r.is_err() {
            self.poison();
        }
        self.bump();
        r.map_err(|p| {
            let what = p
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            format!("the driver panicked: {what}")
        })
    }

    /// Run the driver: one step every `cfg.step`. Returns when the hub is
    /// dropped elsewhere, or with an error when a step or a call panicked.
    pub async fn drive(self: Arc<Hub>) -> Result<(), String> {
        let mut every = tokio::time::interval(self.cfg.step);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if Arc::strong_count(&self) == 1 {
                return Ok(());
            }
            if self.poisoned.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("a call panicked; the state may be half-changed".into());
            }
            self.step()?;
        }
    }

    /// Start a manual backup of `campaign` (0 for the active one): check
    /// the lease, open the store, wait for a backup of it already running,
    /// write the store and its events, and mark it backing up. Returns the
    /// folders, the campaign and the revision the copy will hold; `None`
    /// when the call's key is answered or running, so the call is a repeat.
    /// The caller copies, then removes the campaign from
    /// [`Inner::backing_up`].
    pub async fn backup_start(
        &self,
        key: Option<&pb::Idempotency>,
        lease: Option<&pb::Lease>,
        campaign: u64,
    ) -> Result<Option<(Campaigns, u64, u64)>, Status> {
        let mut ticks = self.tick();
        loop {
            {
                let mut g = self.lock();
                if self.closing() {
                    return Err(crate::services::stopping());
                }
                let now = Instant::now();
                let k = key.map(|k| k.key.as_str()).unwrap_or_default();
                if k.is_empty() || g.idem.running(k) || g.idem.get(now, k).is_some() {
                    return Ok(None);
                }
                // A key a store remembers from before a restart: a repeat.
                if g.recall(k, campaign).is_some() {
                    return Ok(None);
                }
                g.leases
                    .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
                    .map_err(lease_status)?;
                let id = g.resolve(campaign, Need::Campaign)?;
                let Some(files) = g.files.clone() else {
                    return Ok(None);
                };
                if !g.backing_up.contains(&id) {
                    g.space_check(id, 0)?;
                    g.commit_events();
                    g.commit(id);
                    g.backing_up.insert(id);
                    let revision = g.campaigns[&id].store.revision();
                    return Ok(Some((files, id, revision)));
                }
            }
            if ticks.changed().await.is_err() {
                return Err(crate::services::stopping());
            }
        }
    }

    /// Before a suspend, a finish or a switch of the active campaign:
    /// capture every group and static DCS holds of the active campaign
    /// (`campaign`, 0 for the active one) and wait up to `result_wait` for
    /// the results, which the store takes as they arrive (spec/persistence.md,
    /// "Campaign operations"). Nothing when that campaign is not active, the
    /// lease is refused, the call's key is answered or running, or DCS is
    /// not ready. A repeat of the call that arrives while the first waits
    /// captures again: a capture is a query.
    pub async fn capture_live(
        &self,
        key: Option<&pb::Idempotency>,
        lease: Option<&pb::Lease>,
        campaign: u64,
    ) {
        let sent: Vec<(u64, u64)> = {
            let mut g = self.lock();
            if self.closing() {
                return;
            }
            let now = Instant::now();
            let key = key.map(|k| k.key.as_str()).unwrap_or_default();
            if key.is_empty() || g.idem.running(key) || g.idem.get(now, key).is_some() {
                return;
            }
            if g.leases
                .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
                .is_err()
            {
                return;
            }
            let Some(id) = g.active.filter(|a| campaign == 0 || campaign == *a) else {
                return;
            };
            g.with_rec(id, |rec, store, sim| {
                let live: Vec<EntityId> = store
                    .groups()
                    .filter(|grp| !grp.removed && rec.is_bound(grp.id))
                    .map(|grp| grp.id)
                    .collect();
                live.into_iter()
                    .filter_map(|e| rec.capture(store, sim, e))
                    .collect()
            })
            .unwrap_or_default()
        };
        if sent.is_empty() {
            return;
        }
        let mut ticks = self.tick();
        let deadline = tokio::time::Instant::now() + self.cfg.result_wait;
        loop {
            {
                let g = self.lock();
                let done = g.rec.as_ref().is_none_or(|r| {
                    sent.iter().all(|k| {
                        r.effect_result(*k).is_some()
                            || !r.inflight().any(|j| (j.epoch, j.seq) == *k)
                    })
                });
                if done || self.closing() || tokio::time::Instant::now() >= deadline {
                    return;
                }
            }
            let _ = tokio::time::timeout_at(deadline, ticks.changed()).await;
        }
    }

    /// Run a changing call under its idempotency key (spec/errors.md).
    ///
    /// `hashed` is the request with the lease left out. A repeat returns the
    /// first response; a new call checks the lease, resolves the campaign
    /// and runs `f` with the state and the campaign ID. `f` returns the
    /// response and the campaign the call acted on, which `GetOutcome`
    /// reports. A panic in `f` fails the call with `INTERNAL` and, as a
    /// panic in a driver step does, stops the server: `f` may have changed
    /// part of the state. Its key stays running, so no repeat runs it again.
    pub async fn write<R, F>(
        &self,
        call: Call<'_>,
        hashed: &impl Message,
        f: F,
    ) -> Result<R, Status>
    where
        R: Message + Default,
        F: FnOnce(&mut Inner, u64) -> Result<(R, u64), Status>,
    {
        let Call {
            method,
            key,
            lease,
            campaign,
            need,
        } = call;
        // A stopping server takes no more changes: after a panic its state
        // may be half-changed.
        if self.closing() {
            return Err(crate::services::stopping());
        }
        let key = key.map(|k| k.key.as_str()).unwrap_or_default();
        if key.is_empty() {
            return Err(crate::convert::invalid(
                "idempotency.key",
                "every changing call carries an idempotency key",
            ));
        }
        let d = idem::digest(method, &hashed.encode_to_vec());
        let mut f = Some(f);
        loop {
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            {
                let mut g = self.lock();
                // Checked again under the lock: a call that waited for it
                // while another panicked must not run.
                if self.closing() {
                    return Err(crate::services::stopping());
                }
                let now = Instant::now();
                match g.idem.begin(now, key, d) {
                    Begin::Run => {
                        // A key a store remembers from before a restart.
                        if let Some((sd, done, age)) = g.recall(key, campaign) {
                            g.idem.restore(now, key, sd, done, age);
                            continue;
                        }
                        let result = g
                            .leases
                            .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
                            .map_err(lease_status)
                            .and_then(|()| g.resolve(campaign, need))
                            .and_then(|c| {
                                let f = f.take().expect("a call runs once");
                                let inner: &mut Inner = &mut g;
                                catch_unwind(AssertUnwindSafe(|| {
                                    let r = f(inner, c);
                                    if let Ok((resp, acted)) = &r {
                                        let done = Done {
                                            method,
                                            response: resp.encode_to_vec(),
                                            campaign: *acted,
                                            pending: None,
                                        };
                                        let placed = inner.remember(key, &d, &done);
                                        inner.index(key, placed);
                                        inner.settle(Some(*acted));
                                    }
                                    r
                                }))
                                .map_err(|_| {
                                    // Under the lock, so no other call runs
                                    // between the panic and the refusal.
                                    self.poisoned
                                        .store(true, std::sync::atomic::Ordering::SeqCst);
                                    self.closing
                                        .store(true, std::sync::atomic::Ordering::SeqCst);
                                    status(
                                        Code::Internal,
                                        ErrorReason::Internal,
                                        "the call failed inside the server, which stops",
                                        0,
                                        "",
                                    )
                                })?
                            });
                        match result {
                            Ok((r, c)) => {
                                g.idem.finish(
                                    now,
                                    key,
                                    Done {
                                        method,
                                        response: r.encode_to_vec(),
                                        campaign: c,
                                        pending: None,
                                    },
                                );
                                drop(g);
                                self.settled.notify_waiters();
                                self.bump();
                                return Ok(r);
                            }
                            Err(e) => {
                                let poisoned =
                                    self.poisoned.load(std::sync::atomic::Ordering::SeqCst);
                                if !poisoned {
                                    g.idem.fail(key);
                                }
                                drop(g);
                                self.settled.notify_waiters();
                                if poisoned {
                                    // Wake the streams, which end.
                                    self.bump();
                                }
                                return Err(e);
                            }
                        }
                    }
                    Begin::Done(done) => {
                        return R::decode(done.response.as_slice()).map_err(|e| {
                            status(
                                Code::Internal,
                                ErrorReason::Internal,
                                format!("a remembered response does not decode: {e}"),
                                0,
                                "",
                            )
                        });
                    }
                    Begin::Mismatch => {
                        return Err(status(
                            Code::InvalidArgument,
                            ErrorReason::IdempotencyMismatch,
                            "the idempotency key was used with another request",
                            0,
                            "idempotency.key",
                        ));
                    }
                    Begin::Wait => {}
                }
            }
            settled.await;
        }
    }
}

/// The encoded response of an effect or a query call, from its result;
/// none is `pending`.
pub fn effect_response(
    g: &mut Inner,
    campaign: u64,
    kind: EffectKind,
    result: Option<&EffectResult>,
) -> Vec<u8> {
    crate::services::commands::effect_response(g, campaign, kind, result)
}

/// A remembered call whose effect or query was pending, with its result if
/// the reconciler has it now: the final response, remembered in place of
/// the pending one. Any other call is returned as it is.
pub fn settle(g: &mut Inner, key: &str, done: Done) -> Done {
    let Some(p) = done.pending else {
        return done;
    };
    let Some(rec) = g.rec.as_ref() else {
        return done;
    };
    let r = match rec.effect_result(p.cmd) {
        Some(r) => r.clone(),
        // No result, and the command is not in flight either: the result
        // left the reconciler (it kept only the newest ones, or a new
        // reconciler replaced it) and will not come back. The agent may have
        // taken the command, so the outcome is `unknown` (spec/errors.md).
        None if !rec.inflight().any(|j| (j.epoch, j.seq) == p.cmd) => EffectResult {
            outcome: Outcome::Unknown,
            result: None,
        },
        None => return done,
    };
    let settled = Done {
        response: effect_response(g, done.campaign, p.kind, Some(&r)),
        pending: None,
        ..done
    };
    g.idem.update(key, settled.clone());
    if let Some(d) = g.idem.digest_of(key) {
        let placed = g.remember(key, &d, &settled);
        g.index(key, placed);
        let c = settled.campaign;
        g.settle(Some(c));
    }
    settled
}

impl Hub {
    /// Run an effect or a query under its idempotency key (spec/errors.md).
    ///
    /// `start` runs under the lock like a [`Hub::write`] call: it sends the
    /// command and returns its (epoch, seq). The call then waits, without
    /// the lock, for the agent's result up to `cfg.result_wait`. The response
    /// is built from the result by [`effect_response`], or from none when the
    /// time ran out (the outcome `pending`). The response is remembered under
    /// the key; a pending one is settled later by a repeat of the call or by
    /// `GetOutcome` ([`settle`]).
    pub async fn effect<R, S>(
        &self,
        call: Call<'_>,
        hashed: &impl Message,
        kind: EffectKind,
        start: S,
    ) -> Result<R, Status>
    where
        R: Message + Default,
        S: FnOnce(&mut Inner, u64) -> Result<(u64, u64), Status>,
    {
        let Call {
            method,
            key,
            lease,
            campaign,
            need,
        } = call;
        if self.closing() {
            return Err(crate::services::stopping());
        }
        let key = key.map(|k| k.key.as_str()).unwrap_or_default();
        if key.is_empty() {
            return Err(crate::convert::invalid(
                "idempotency.key",
                "every changing call carries an idempotency key",
            ));
        }
        let d = idem::digest(method, &hashed.encode_to_vec());
        let mut start = Some(start);
        let sent = loop {
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            {
                let mut g = self.lock();
                if self.closing() {
                    return Err(crate::services::stopping());
                }
                let now = Instant::now();
                match g.idem.begin(now, key, d) {
                    Begin::Run => {
                        // A key a store remembers from before a restart.
                        if let Some((sd, done, age)) = g.recall(key, campaign) {
                            g.idem.restore(now, key, sd, done, age);
                            continue;
                        }
                        let result = g
                            .leases
                            .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
                            .map_err(lease_status)
                            .and_then(|()| g.resolve(campaign, need))
                            .and_then(|c| {
                                let start = start.take().expect("a call runs once");
                                let inner: &mut Inner = &mut g;
                                catch_unwind(AssertUnwindSafe(|| {
                                    let r = start(inner, c);
                                    if let Ok(cmd) = &r {
                                        // The key is kept with the command:
                                        // a repeat after a crash never sends
                                        // it again.
                                        let done = Done {
                                            method,
                                            response: effect_response(inner, c, kind, None),
                                            campaign: c,
                                            pending: Some(Pending { cmd: *cmd, kind }),
                                        };
                                        let placed = inner.remember(key, &d, &done);
                                        inner.index(key, placed);
                                        inner.settle(Some(c));
                                    } else {
                                        inner.settle(Some(c));
                                    }
                                    r
                                }))
                                .map_err(|_| {
                                    self.poisoned
                                        .store(true, std::sync::atomic::Ordering::SeqCst);
                                    self.closing
                                        .store(true, std::sync::atomic::Ordering::SeqCst);
                                    status(
                                        Code::Internal,
                                        ErrorReason::Internal,
                                        "the call failed inside the server, which stops",
                                        0,
                                        "",
                                    )
                                })?
                                .map(|k| (k, c))
                            });
                        match result {
                            Ok(sent) => break sent,
                            Err(e) => {
                                if !self.poisoned.load(std::sync::atomic::Ordering::SeqCst) {
                                    g.idem.fail(key);
                                }
                                drop(g);
                                self.settled.notify_waiters();
                                return Err(e);
                            }
                        }
                    }
                    Begin::Done(done) => {
                        let done = settle(&mut g, key, done);
                        return R::decode(done.response.as_slice()).map_err(|e| {
                            status(
                                Code::Internal,
                                ErrorReason::Internal,
                                format!("a remembered response does not decode: {e}"),
                                0,
                                "",
                            )
                        });
                    }
                    Begin::Mismatch => {
                        return Err(status(
                            Code::InvalidArgument,
                            ErrorReason::IdempotencyMismatch,
                            "the idempotency key was used with another request",
                            0,
                            "idempotency.key",
                        ));
                    }
                    Begin::Wait => {}
                }
            }
            settled.await;
        };
        let (cmd, c) = sent;
        // The command is sent: wait for the driver to read its result.
        let mut ticks = self.tick();
        let deadline = tokio::time::Instant::now() + self.cfg.result_wait;
        let result = loop {
            {
                let mut g = self.lock();
                let found = g.rec.as_ref().and_then(|r| r.effect_result(cmd)).cloned();
                let timed_out = tokio::time::Instant::now() >= deadline;
                if found.is_some() || timed_out || self.closing() {
                    let pending = found.is_none().then_some(Pending { cmd, kind });
                    let response = effect_response(&mut g, c, kind, found.as_ref());
                    let r = R::decode(response.as_slice()).unwrap_or_default();
                    let done = Done {
                        method,
                        response,
                        campaign: c,
                        pending,
                    };
                    let placed = g.remember(key, &d, &done);
                    g.index(key, placed);
                    g.settle(Some(c));
                    g.idem.finish(Instant::now(), key, done);
                    drop(g);
                    self.settled.notify_waiters();
                    break r;
                }
            }
            let _ = tokio::time::timeout_at(deadline, ticks.changed()).await;
        };
        Ok(result)
    }
}

/// Apply an `AMMO` read of a bound object: the store's unit shows it, the
/// loadout table keeps it for `Player.ammo`, and a loadout event follows when
/// the roster has players in the unit. A read from another agent boot or
/// mission generation than the loaded store's is dropped.
fn apply_loadout(
    roster: &Roster,
    loadouts: &mut Loadouts,
    link: &Link,
    (rec, store): (&Reconciler, &mut Store),
    (object, boot, loadout): (u64, u64, Loadout),
    out: &mut Vec<Read>,
) {
    let Some((unit, group)) = rec.entity_of(store, object) else {
        return;
    };
    if !store
        .loaded()
        .is_some_and(|l| l.agent_boot == boot && l.mission_gen == loadout.read.mission_gen)
    {
        return;
    }
    store.observe_loadout(EntityId(unit), loadout.clone());
    let keys = roster.players_of(object);
    if !keys.is_empty() {
        out.push(Read {
            campaign: link.frame_campaign,
            stamp: link.stamp(),
            body: loadout_body(
                unit,
                group,
                roster.type_of(object).unwrap_or_default(),
                keys,
                &loadout,
            ),
        });
    }
    loadouts.set(object, boot, loadout);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed Caucasus template.
    const TEMPLATE: &[u8] = include_bytes!("../../../tests/fixtures/templates/caucasus.miz");

    /// A DCS that never answers.
    struct Silent;

    impl SimAdapter for Silent {
        fn position_after(&self, _opcode: u16, _payload: &[u8]) -> u64 {
            0
        }

        fn send(&mut self, _opcode: u16, _payload: &[u8]) -> Result<u64, Detached> {
            Err(Detached)
        }

        fn read(&mut self) -> Vec<RawRecord> {
            Vec::new()
        }

        fn flush(&mut self) {}
    }

    impl Backend for Silent {
        fn sim(&mut self) -> &mut dyn SimAdapter {
            self
        }
        fn frame(&mut self) {}
        fn attach(&mut self, _sidecar_boot: u64) {}
        fn detach(&mut self) {}
        fn load_mission(&mut self, _theater: &str) {}
        fn health(&self) -> Health {
            Health::Gone
        }
        fn heartbeat_age(&self) -> Option<Duration> {
            None
        }
        fn attach_error(&self) -> String {
            String::new()
        }
    }

    /// A DCS whose frame panics.
    struct Broken;

    impl Backend for Broken {
        fn sim(&mut self) -> &mut dyn SimAdapter {
            unreachable!("no frame gets this far")
        }
        fn frame(&mut self) {
            panic!("a broken frame")
        }
        fn attach(&mut self, _sidecar_boot: u64) {}
        fn detach(&mut self) {}
        fn load_mission(&mut self, _theater: &str) {}
        fn health(&self) -> Health {
            Health::Gone
        }
        fn heartbeat_age(&self) -> Option<Duration> {
            None
        }
        fn attach_error(&self) -> String {
            String::new()
        }
    }

    /// The fake sim with a mission folder, as a real DCS is set up: its
    /// missions are loaded by the reconciler's `MISSION_LOAD`, from the files
    /// [`crate::real::Missions`] names. The transport is the fake's; RealSim's
    /// link is checked by the conformance suite.
    struct Folder(nanoprobe_sim_fake::FakeSim, crate::real::Missions);

    impl Backend for Folder {
        fn sim(&mut self) -> &mut dyn SimAdapter {
            &mut self.0
        }
        fn frame(&mut self) {
            self.0.frame();
        }
        fn attach(&mut self, sidecar_boot: u64) {
            self.0.attach(sidecar_boot);
        }
        fn detach(&mut self) {
            self.0.detach();
        }
        fn load_mission(&mut self, _theater: &str) {
            unreachable!("a folder backend's missions are files")
        }
        fn missions(&self) -> Option<&crate::real::Missions> {
            Some(&self.1)
        }
        fn health(&self) -> Health {
            Health::Healthy
        }
        fn heartbeat_age(&self) -> Option<Duration> {
            None
        }
        fn attach_error(&self) -> String {
            String::new()
        }
        fn as_any(&mut self) -> Option<&mut dyn std::any::Any> {
            Some(&mut self.0)
        }
    }

    /// A DCS whose verdict the test sets.
    struct Verdicts(Silent, Arc<Mutex<Health>>);

    impl Backend for Verdicts {
        fn sim(&mut self) -> &mut dyn SimAdapter {
            &mut self.0
        }
        fn frame(&mut self) {}
        fn attach(&mut self, _sidecar_boot: u64) {}
        fn detach(&mut self) {}
        fn load_mission(&mut self, _theater: &str) {}
        fn health(&self) -> Health {
            *self.1.lock().expect("verdict")
        }
        fn heartbeat_age(&self) -> Option<Duration> {
            None
        }
        fn attach_error(&self) -> String {
            String::new()
        }
    }

    #[test]
    fn a_hung_verdict_is_a_lifecycle_event_on_entering_and_leaving() {
        let verdict = Arc::new(Mutex::new(Health::Healthy));
        let hub = Hub::new(
            Config::default(),
            Box::new(Verdicts(Silent, verdict.clone())),
        );
        hub.start();
        let states = |hub: &Hub| -> Vec<i32> {
            hub.lock()
                .server_log
                .after(0, 100)
                .into_iter()
                .filter_map(|e| match e.body {
                    Some(event::Body::Lifecycle(l)) => Some(l.state),
                    _ => None,
                })
                .collect()
        };
        let steps = |n| {
            for _ in 0..n {
                hub.step().expect("step");
            }
        };
        steps(3);
        assert!(states(&hub).is_empty(), "healthy publishes nothing");
        *verdict.lock().unwrap() = Health::Hung;
        steps(3);
        assert_eq!(
            states(&hub),
            [pb::SimState::Hung as i32],
            "once on entering"
        );
        *verdict.lock().unwrap() = Health::Gone;
        steps(3);
        assert_eq!(
            states(&hub),
            [pb::SimState::Hung as i32, pb::SimState::Gone as i32],
            "the real state on leaving"
        );
        *verdict.lock().unwrap() = Health::Hung;
        steps(1);
        *verdict.lock().unwrap() = Health::Healthy;
        steps(1);
        let s = states(&hub);
        assert_eq!(s.len(), 4);
        assert_eq!(
            s[3],
            pb::SimState::Unspecified as i32,
            "no lifecycle read yet"
        );
    }

    /// The active campaign's owed mission load.
    fn owed(g: &Inner) -> Option<OwedLoad> {
        g.active
            .and_then(|id| g.campaigns.get(&id))
            .and_then(|s| s.store.owed_load().cloned())
    }

    fn mission_loads(g: &mut Inner) -> Vec<String> {
        let any = g.backend.as_any().expect("fake");
        let sim = any
            .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
            .expect("fake");
        sim.ledger().mission_loads.clone()
    }

    #[test]
    fn activation_loads_the_theater_s_file_or_changes_nothing() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(dir.path().to_path_buf()),
            dcs_dir: "Z:\\profile\\nanoprobe\\missions\\".into(),
        };
        let cfg = Config::default();
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                missions,
            )),
        );
        hub.start();
        let campaign = |id| pb::Campaign {
            id,
            name: format!("c{id}"),
            ..pb::Campaign::default()
        };
        {
            let mut g = hub.lock();
            g.add_campaign(&cfg, campaign(1), "Caucasus".into())
                .expect("add");
            g.add_campaign(&cfg, campaign(2), "Afghanistan".into())
                .expect("add");
            g.activate(&cfg, 1).expect("Caucasus has a file");
        }
        for _ in 0..200 {
            hub.step().expect("step");
        }
        let mut g = hub.lock();
        assert_eq!(
            mission_loads(&mut g),
            ["Z:\\profile\\nanoprobe\\missions\\shells\\1.a.miz"],
            "exactly one MISSION_LOAD, with dcs_dir joined to the file name"
        );
        let revision = g.campaigns[&1].store.revision();
        let e = g.activate(&cfg, 2).expect_err("no Afghanistan.miz");
        assert_eq!(e.code(), Code::FailedPrecondition);
        assert!(
            e.message().contains("no template for theater"),
            "{}",
            e.message()
        );
        assert_eq!(g.active, Some(1), "the earlier campaign stays active");
        assert_eq!(g.campaigns[&1].meta.state, pb::CampaignState::Active as i32);
        assert_eq!(
            g.campaigns[&1].store.revision(),
            revision,
            "its store is unchanged"
        );
        drop(g);
        for _ in 0..50 {
            hub.step().expect("step");
        }
        assert_eq!(
            mission_loads(&mut hub.lock()).len(),
            1,
            "nothing sent for the refusal"
        );
    }

    /// A mission folder holding only `Caucasus.miz`, a store folder, and the
    /// config that keeps campaigns there.
    fn owed_setup() -> (tempfile::TempDir, crate::real::Missions, Config) {
        let dir = tempfile::tempdir().expect("dir");
        let m = dir.path().join("missions");
        std::fs::create_dir(&m).expect("missions");
        std::fs::write(m.join("Caucasus.miz"), TEMPLATE).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(m),
            dcs_dir: "Z:\\m".into(),
        };
        let cfg = Config {
            store: Some(crate::config::StoreFiles::new(dir.path().join("campaigns"))),
            ..Config::default()
        };
        (dir, missions, cfg)
    }

    /// A hub over a new DCS process whose agent boot is `boot`.
    fn folder_hub(cfg: &Config, missions: &crate::real::Missions, boot: u64) -> Arc<Hub> {
        Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config {
                    first_boot: boot,
                    ..nanoprobe_sim_fake::Config::default()
                }),
                missions.clone(),
            )),
        )
    }

    /// Add campaign 1 on Caucasus and activate it; `settle` commits the
    /// call's end too, as a call that answers does.
    fn add_and_activate(hub: &Hub, cfg: &Config, settle: bool) {
        let mut g = hub.lock();
        let meta = pb::Campaign {
            id: 1,
            name: "c1".into(),
            ..pb::Campaign::default()
        };
        g.add_campaign(cfg, meta, "Caucasus".into()).expect("add");
        g.activate(cfg, 1).expect("activate");
        if settle {
            g.settle(Some(1));
        }
    }

    fn steps(hub: &Hub, n: usize) {
        for _ in 0..n {
            hub.step().expect("step");
        }
    }

    #[test]
    fn a_finished_campaign_reads_through_a_copy_and_refuses_changes() {
        let (_dir, missions, cfg) = owed_setup();
        let hub = folder_hub(&cfg, &missions, 0x1000);
        hub.start();
        let group = {
            let mut g = hub.lock();
            let meta = pb::Campaign {
                id: 1,
                name: "c1".into(),
                ..pb::Campaign::default()
            };
            g.add_campaign(&cfg, meta, "Caucasus".into()).expect("add");
            let id = g
                .campaigns
                .get_mut(&1)
                .expect("slot")
                .store
                .create_group(nanoprobe_sidecar::store::NewGroup {
                    theater: "Caucasus".into(),
                    country: 2,
                    category: 2,
                    coalition: 2,
                    units: vec![nanoprobe_sidecar::store::NewUnit::new(
                        "M-113",
                        nanoprobe_sidecar::store::UnitState::default(),
                    )],
                    authority: nanoprobe_sidecar::store::Authority::Campaign,
                    label: String::new(),
                    extra: nanoprobe_transport::codec::Lv::Nil,
                })
                .expect("group");
            g.activate(&cfg, 1).expect("activate");
            g.settle(Some(1));
            g.suspend(&cfg, 1, Some(Vec::new()));
            g.settle(Some(1));
            id
        };
        steps(&hub, 2);
        let mut g = hub.lock();
        assert!(
            g.campaigns[&1].store.group(group).is_none(),
            "the finished store is closed"
        );
        let e = g.resolve(1, Need::Campaign).expect_err("read-only");
        assert_eq!(
            nanoprobe_proto::errors::reason(&e),
            ErrorReason::CampaignReadOnly
        );
        g.open_archive(1).expect("copy");
        assert!(
            g.read_store(1).group(group).is_some(),
            "read through the copy"
        );
        let read = g
            .files()
            .expect("files")
            .folder(CampaignId(1))
            .join(".read");
        let copies = std::fs::read_dir(&read)
            .expect("dir")
            .filter(|e| {
                e.as_ref()
                    .is_ok_and(|e| e.file_name().to_string_lossy().ends_with(".sqlite"))
            })
            .count();
        assert_eq!(copies, 1);
        g.archive
            .values_mut()
            .for_each(|(_, at)| *at -= ARCHIVE_KEEP);
        drop(g);
        steps(&hub, 1);
        assert_eq!(
            std::fs::read_dir(&read).expect("dir").count(),
            0,
            "an unused copy goes"
        );
    }

    #[test]
    fn an_owed_load_outlives_a_crash_and_is_sent_once() {
        let (_dir, missions, cfg) = owed_setup();
        {
            // Activated while the agent is detached: the load is owed, not
            // sent. The sidecar dies right after the activation's own
            // commit, before the call's end commits.
            let hub = folder_hub(&cfg, &missions, 0x1000);
            add_and_activate(&hub, &cfg, false);
            let mut g = hub.lock();
            assert!(owed(&g).is_some_and(|o| o.cmd.is_none()));
            assert!(mission_loads(&mut g).is_empty());
        }
        {
            let hub = folder_hub(&cfg, &missions, 0x2000);
            hub.start();
            assert!(owed(&hub.lock()).is_some(), "the owed load was on disk");
            steps(&hub, 300);
            let mut g = hub.lock();
            assert_eq!(mission_loads(&mut g), ["Z:\\m\\shells\\1.a.miz"]);
            assert!(owed(&g).is_none(), "APPLIED clears it");
            g.close_all().expect("close");
        }
        let hub = folder_hub(&cfg, &missions, 0x3000);
        hub.start();
        steps(&hub, 100);
        assert!(
            mission_loads(&mut hub.lock()).is_empty(),
            "nothing is owed on a third start"
        );
    }

    #[test]
    fn a_sent_load_is_never_sent_twice_across_a_crash() {
        let (_dir, missions, cfg) = owed_setup();
        {
            let hub = folder_hub(&cfg, &missions, 0x4000);
            hub.start();
            add_and_activate(&hub, &cfg, true);
            // Step until the load is sent and on disk, then die before its
            // result is read.
            for _ in 0..100 {
                if owed(&hub.lock()).is_some_and(|o| o.cmd.is_some()) {
                    break;
                }
                hub.step().expect("step");
            }
            assert!(owed(&hub.lock()).is_some_and(|o| o.cmd.is_some()));
        }
        // A new DCS process: the agent boot that took the load is gone, so
        // its command resolves unknown, and the load is not sent again.
        let hub = folder_hub(&cfg, &missions, 0x5000);
        hub.start();
        steps(&hub, 300);
        let mut g = hub.lock();
        assert!(mission_loads(&mut g).is_empty(), "no second MISSION_LOAD");
        assert!(owed(&g).is_none());
        // DCS may have taken the file: the next shell goes to the other slot.
        assert_eq!(g.campaigns[&1].store.shell_slot(), Some(b'a'));
        let o = g
            .write_shell(1, "Caucasus")
            .expect("shell")
            .expect("a file");
        assert!(o.path.ends_with("1.b.miz"), "{}", o.path);
    }

    #[test]
    fn a_suspend_clears_an_owed_load() {
        let (_dir, missions, cfg) = owed_setup();
        {
            let hub = folder_hub(&cfg, &missions, 0x6000);
            add_and_activate(&hub, &cfg, true);
            let mut g = hub.lock();
            g.suspend(&cfg, 1, None);
            g.settle(Some(1));
            g.close_all().expect("close");
        }
        let hub = folder_hub(&cfg, &missions, 0x7000);
        hub.start();
        steps(&hub, 100);
        let mut g = hub.lock();
        assert!(mission_loads(&mut g).is_empty());
        g.open_store(1).expect("open");
        assert_eq!(
            g.campaigns[&1].store.owed_load(),
            None,
            "the suspend cleared it"
        );
    }

    #[test]
    fn the_default_lost_policy_reaches_every_campaign() {
        let cfg = Config {
            on_lost: LostPolicy::Respawn,
            ..Config::default()
        };
        let hub = Hub::new(cfg.clone(), Box::new(Silent));
        let mut g = hub.lock();
        let meta = pb::Campaign {
            id: 1,
            name: "c1".into(),
            ..pb::Campaign::default()
        };
        g.add_campaign(&cfg, meta, "Caucasus".into()).expect("add");
        assert_eq!(g.rec_config(&cfg, 1).on_lost, LostPolicy::Respawn);
    }

    #[test]
    fn nothing_materializes_before_the_owed_load_s_generation() {
        let missions_dir = tempfile::tempdir().expect("dir");
        std::fs::write(missions_dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(missions_dir.path().to_path_buf()),
            dcs_dir: "Z:\\m".into(),
        };
        let cfg = Config::default();
        // DCS runs the mission before for 20 frames after it answers the
        // load, as `net.load_mission` loads at a later frame.
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config {
                    load_delay_frames: 20,
                    ..nanoprobe_sim_fake::Config::default()
                }),
                missions,
            )),
        );
        hub.start();
        // The previous mission runs on the campaign's theater.
        steps(&hub, 20);
        let before = {
            let mut g = hub.lock();
            let any = g.backend.as_any().expect("fake");
            any.downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake")
                .mission_gen()
        };
        {
            let mut g = hub.lock();
            let meta = pb::Campaign {
                id: 1,
                name: "c1".into(),
                ..pb::Campaign::default()
            };
            g.add_campaign(&cfg, meta, "Caucasus".into()).expect("add");
            let unit = nanoprobe_sidecar::store::NewUnit::new(
                "M-113",
                nanoprobe_sidecar::store::UnitState::default(),
            );
            g.campaigns
                .get_mut(&1)
                .expect("slot")
                .store
                .create_group(nanoprobe_sidecar::store::NewGroup {
                    theater: "Caucasus".into(),
                    country: 2,
                    category: 2,
                    coalition: 2,
                    units: vec![unit],
                    authority: nanoprobe_sidecar::store::Authority::Sim,
                    label: String::new(),
                    extra: nanoprobe_transport::codec::Lv::Nil,
                })
                .expect("group");
            g.activate(&cfg, 1).expect("activate");
        }
        steps(&hub, 300);
        let mut g = hub.lock();
        assert_eq!(mission_loads(&mut g).len(), 1);
        let any = g.backend.as_any().expect("fake");
        let spawns = &any
            .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
            .expect("fake")
            .ledger()
            .spawns;
        assert!(!spawns.is_empty(), "the group materialized after the load");
        assert!(
            spawns
                .iter()
                .all(|(_, generation, ..)| *generation > before),
            "a spawn went into the previous mission: {spawns:?}, generation before {before}"
        );
    }

    #[test]
    fn a_reload_switches_theater_and_back_with_the_other_map_s_entities_virtual() {
        use nanoprobe_sidecar::lifecycle::State;
        const AFGHANISTAN: &[u8] =
            include_bytes!("../../../tests/fixtures/templates/afghanistan.miz");
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        std::fs::write(dir.path().join("Afghanistan.miz"), AFGHANISTAN).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(dir.path().to_path_buf()),
            dcs_dir: "Z:\\m".into(),
        };
        let cfg = Config::default();
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                missions,
            )),
        );
        hub.start();
        let group = |theater: &str| nanoprobe_sidecar::store::NewGroup {
            theater: theater.into(),
            country: 2,
            category: 2,
            coalition: 2,
            units: vec![nanoprobe_sidecar::store::NewUnit::new(
                "M-113",
                nanoprobe_sidecar::store::UnitState::default(),
            )],
            authority: nanoprobe_sidecar::store::Authority::Sim,
            label: String::new(),
            extra: nanoprobe_transport::codec::Lv::Nil,
        };
        let (a, b) = {
            let mut g = hub.lock();
            let meta = pb::Campaign {
                id: 1,
                name: "two maps".into(),
                theaters: vec!["Caucasus".into(), "Afghanistan".into()],
                ..pb::Campaign::default()
            };
            g.add_campaign(&cfg, meta, "Caucasus".into()).expect("add");
            let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
            let a = store.create_group(group("Caucasus")).expect("a");
            let b = store.create_group(group("Afghanistan")).expect("b");
            g.activate(&cfg, 1).expect("activate");
            (a, b)
        };
        let state = |hub: &Hub, id| {
            hub.lock().campaigns[&1]
                .store
                .group(id)
                .expect("group")
                .state
        };
        let switch = |theater: &str| {
            let mut g = hub.lock();
            let any = g.backend.as_any().expect("fake");
            any.downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake")
                .set_load_theater(Some(theater.into()));
            let next = crate::next::Next {
                theater: theater.into(),
                ..crate::next::Next::default()
            };
            g.set_next(1, &next).expect("next");
            g.reload(&cfg, 1).expect("reload");
            assert!(g.reload(&cfg, 1).is_err(), "one owed load at a time");
        };
        steps(&hub, 300);
        assert_eq!(
            (state(&hub, a), state(&hub, b)),
            (State::Live, State::Virtual)
        );
        switch("Afghanistan");
        steps(&hub, 300);
        assert_eq!(
            hub.lock().campaigns[&1]
                .store
                .loaded()
                .map(|l| l.theater.clone()),
            Some("Afghanistan".into())
        );
        assert_eq!(
            (state(&hub, a), state(&hub, b)),
            (State::Virtual, State::Live)
        );
        switch("Caucasus");
        steps(&hub, 300);
        assert_eq!(
            (state(&hub, a), state(&hub, b)),
            (State::Live, State::Virtual)
        );
        let mut g = hub.lock();
        assert_eq!(
            mission_loads(&mut g),
            [
                "Z:\\m\\shells\\1.a.miz",
                "Z:\\m\\shells\\1.b.miz",
                "Z:\\m\\shells\\1.a.miz"
            ],
            "each shell goes to the slot the last load did not name"
        );
        let e = g
            .set_next(
                1,
                &crate::next::Next {
                    theater: "Syria".into(),
                    ..crate::next::Next::default()
                },
            )
            .expect_err("not a theater of the campaign");
        assert_eq!(e.code(), Code::InvalidArgument);
    }

    /// The clock across an applied load whose mission starts late, a
    /// refused load, and a DCS restart that loads no shell.
    #[test]
    fn the_clock_follows_loads_refusals_and_restarts() {
        use nanoprobe_sidecar::shell::Start;
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(dir.path().to_path_buf()),
            dcs_dir: "Z:\\m".into(),
        };
        let cfg = Config::default();
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config {
                    load_delay_frames: 40,
                    ..nanoprobe_sim_fake::Config::default()
                }),
                missions,
            )),
        );
        hub.start();
        fn fake(g: &mut Inner) -> &mut nanoprobe_sim_fake::FakeSim {
            g.backend
                .as_any()
                .expect("fake")
                .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake")
        }
        let at = |y, m, d| {
            Start {
                year: y,
                month: m,
                day: d,
                seconds: 0,
            }
            .unix_ms()
        };
        let owed = |hub: &Hub| hub.lock().campaigns[&1].store.owed_load().is_some();
        let until_applied = |hub: &Hub| {
            for _ in 0..400 {
                hub.step().expect("step");
                if !owed(hub) {
                    return;
                }
            }
            panic!("the load was never answered");
        };
        let clock = |hub: &Hub| hub.lock().clock_ms(1).expect("clock");
        {
            let mut g = hub.lock();
            g.add_campaign(
                &cfg,
                pb::Campaign {
                    id: 1,
                    name: "c".into(),
                    theaters: vec!["Caucasus".into()],
                    clock: Some(prost_types::Timestamp {
                        seconds: at(2030, 1, 1) / 1000,
                        nanos: 0,
                    }),
                    ..pb::Campaign::default()
                },
                "Caucasus".into(),
            )
            .expect("add");
            g.activate(&cfg, 1).expect("activate");
        }
        until_applied(&hub);
        for _ in 0..200 {
            hub.step().expect("step");
        }
        let running = clock(&hub);
        assert!(
            running > at(2030, 1, 1),
            "the clock runs with the model time"
        );

        // A refused load: the running mission's clock goes on.
        fake(&mut hub.lock()).refuse_loads(1);
        let next = |ms| crate::next::Next {
            start_ms: Some(ms),
            ..crate::next::Next::default()
        };
        {
            let mut g = hub.lock();
            g.set_next(1, &next(at(2040, 1, 1))).expect("next");
            g.reload(&cfg, 1).expect("reload");
        }
        until_applied(&hub);
        for _ in 0..50 {
            hub.step().expect("step");
        }
        let after_refusal = clock(&hub);
        assert!(
            after_refusal > running && after_refusal < at(2031, 1, 1),
            "the refused load's start does not take over: {after_refusal}"
        );

        // An applied load whose mission starts 40 frames later: the clock is
        // the new start until the new generation's first sample.
        {
            let mut g = hub.lock();
            g.set_next(1, &next(at(2050, 6, 1))).expect("next");
            g.reload(&cfg, 1).expect("reload");
        }
        until_applied(&hub);
        assert_eq!(clock(&hub), at(2050, 6, 1), "no old model time is added");
        for _ in 0..200 {
            hub.step().expect("step");
        }
        let before_restart = clock(&hub);
        assert!(before_restart > at(2050, 6, 1));

        // A DCS restart runs a mission the campaign did not load: the clock
        // does not go back.
        {
            let mut g = hub.lock();
            fake(&mut g).restart_dcs();
            let boot = g.sidecar_boot;
            g.backend.attach(boot);
        }
        for _ in 0..300 {
            hub.step().expect("step");
        }
        let after_restart = clock(&hub);
        assert!(
            after_restart >= before_restart,
            "the clock went back from {before_restart} to {after_restart}"
        );
        assert_eq!(hub.lock().campaigns[&1].store.mission_start_ms(), None);
    }

    /// The shell of a reload: the start and the weather a request set, in
    /// the second slot; then a reload with no start carries the clock.
    #[test]
    fn a_reload_writes_the_requested_weather_and_start_then_the_clock() {
        use nanoprobe_sidecar::shell::{self, Start};
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(dir.path().to_path_buf()),
            dcs_dir: "Z:\\m".into(),
        };
        let cfg = Config::default();
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                missions,
            )),
        );
        hub.start();
        let applied = |hub: &Hub| {
            for _ in 0..400 {
                hub.step().expect("step");
                if hub.lock().campaigns[&1].store.owed_load().is_none() {
                    return;
                }
            }
            panic!("not applied");
        };
        {
            let mut g = hub.lock();
            g.add_campaign(
                &cfg,
                pb::Campaign {
                    id: 1,
                    name: "c".into(),
                    theaters: vec!["Caucasus".into()],
                    ..pb::Campaign::default()
                },
                "Caucasus".into(),
            )
            .expect("add");
            g.activate(&cfg, 1).expect("activate");
        }
        applied(&hub);
        let start = Start {
            year: 2033,
            month: 3,
            day: 9,
            seconds: 4_000,
        };
        let weather = prost_types::Struct {
            fields: [(
                "season".to_owned(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::StructValue(prost_types::Struct {
                        fields: [(
                            "temperature".to_owned(),
                            prost_types::Value {
                                kind: Some(prost_types::value::Kind::NumberValue(-4.5)),
                            },
                        )]
                        .into(),
                    })),
                },
            )]
            .into(),
        };
        let next = crate::services::mission::next_of(Some(&pb::NextMission {
            theater: String::new(),
            start: Some(prost_types::Timestamp {
                seconds: start.unix_ms() / 1000,
                nanos: 0,
            }),
            weather: Some(weather.clone()),
            dynamic_spawn: Vec::new(),
        }))
        .expect("next");
        {
            let mut g = hub.lock();
            g.set_next(1, &next).expect("set");
            g.reload(&cfg, 1).expect("reload");
        }
        let read = |slot: &str| {
            std::fs::read(dir.path().join("shells").join(format!("1.{slot}.miz"))).expect("shell")
        };
        let b = read("b");
        assert_eq!(shell::start_of(&b), Ok(start));
        let mission = shell::members(&b)
            .expect("members")
            .into_iter()
            .find(|(n, _)| n == "mission")
            .expect("mission")
            .1;
        let d = shell::lua::parse(&mission).expect("parse");
        let w = d.root.get(&shell::lua::Key::s("weather")).expect("weather");
        assert_eq!(
            shell::lua::value(&mission, w),
            Some(crate::lua::struct_to_lua(&weather))
        );
        applied(&hub);
        for _ in 0..100 {
            hub.step().expect("step");
        }
        // No start set: the shell carries the clock as it was at the write.
        let low = hub.lock().clock_ms(1).expect("clock");
        hub.lock().reload(&cfg, 1).expect("reload");
        let high = hub.lock().clock_ms(1).expect("clock");
        let a = shell::start_of(&read("a")).expect("start").unix_ms();
        assert!(
            a / 1000 >= low / 1000 && a / 1000 <= high / 1000 && a > start.unix_ms(),
            "the shell's start {a} is the clock, between {low} and {high}"
        );
    }

    #[test]
    fn resolve_member_works_on_a_suspended_campaign() {
        use nanoprobe_sidecar::store::LeftAs;
        let cfg = Config::default();
        let hub = Hub::new(cfg.clone(), Box::new(Silent));
        let mut g = hub.lock();
        g.add_campaign(
            &cfg,
            pb::Campaign {
                id: 1,
                name: "c".into(),
                ..pb::Campaign::default()
            },
            "Caucasus".into(),
        )
        .expect("add");
        let store = &mut g.campaigns.get_mut(&1).expect("slot").store;
        let group = store
            .create_group(nanoprobe_sidecar::store::NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: vec![
                    nanoprobe_sidecar::store::NewUnit::new(
                        "M-113",
                        nanoprobe_sidecar::store::UnitState::default(),
                    ),
                    nanoprobe_sidecar::store::NewUnit::new(
                        "M-113",
                        nanoprobe_sidecar::store::UnitState::default(),
                    ),
                ],
                authority: nanoprobe_sidecar::store::Authority::Sim,
                label: String::new(),
                extra: nanoprobe_transport::codec::Lv::Nil,
            })
            .expect("group");
        let m = store.group(group).expect("group").members.clone();
        assert!(store.left(m[0], 5, LeftAs::Removed));
        let state = store.group(group).expect("group").state;
        assert_eq!(g.active, None, "suspended");
        let req = |unit: EntityId, r: pb::Resolution| pb::ResolveMemberRequest {
            write: None,
            unit: unit.0,
            resolution: r as i32,
        };
        let e =
            crate::services::commands::resolve_member(&mut g, 1, &req(m[1], pb::Resolution::Dead))
                .expect_err("not held");
        assert_eq!(e.code(), Code::FailedPrecondition);
        let e =
            crate::services::commands::resolve_member(&mut g, 1, &req(group, pb::Resolution::Dead))
                .expect_err("a group");
        assert_eq!(e.code(), Code::InvalidArgument);
        let e = crate::services::commands::resolve_member(
            &mut g,
            1,
            &req(m[0], pb::Resolution::Unspecified),
        )
        .expect_err("no resolution");
        assert_eq!(e.code(), Code::InvalidArgument);
        crate::services::commands::resolve_member(&mut g, 1, &req(m[0], pb::Resolution::Restore))
            .expect("restore");
        let store = &g.campaigns[&1].store;
        assert!(!store.held(m[0]));
        assert_eq!(
            store.group(group).expect("group").state,
            state,
            "the group's state waits for a spawn"
        );
    }

    #[test]
    fn a_reload_is_refused_before_it_captures_anything() {
        let (_dir, missions, cfg) = owed_setup();
        let hub = folder_hub(&cfg, &missions, 0x1000);
        hub.start();
        {
            let mut g = hub.lock();
            g.add_campaign(
                &cfg,
                pb::Campaign {
                    id: 1,
                    name: "c1".into(),
                    theaters: vec!["Caucasus".into(), "Afghanistan".into()],
                    ..pb::Campaign::default()
                },
                "Caucasus".into(),
            )
            .expect("add");
            g.activate(&cfg, 1).expect("activate");
        }
        // While the activation's load is owed.
        let lease = {
            let grant = hub
                .lock()
                .leases
                .acquire(Instant::now(), wall_ms(), "t".into())
                .expect("lease");
            pb::Lease {
                epoch: grant.epoch,
                lease_id: grant.lease_id,
            }
        };
        let e = hub
            .lock()
            .reload_check(None, Some(&lease), 0)
            .expect_err("owed");
        assert_eq!(e.code(), Code::FailedPrecondition);
        steps(&hub, 300);
        let mut g = hub.lock();
        // No template for the next theater, and an airport the template lacks.
        let next = |theater: &str, dynamic| crate::next::Next {
            theater: theater.into(),
            dynamic,
            ..crate::next::Next::default()
        };
        g.set_next(1, &next("Afghanistan", Vec::new()))
            .expect("next");
        let e = g
            .reload_check(None, Some(&lease), 0)
            .expect_err("no template");
        assert!(
            e.message().contains("no template for theater"),
            "{}",
            e.message()
        );
        g.set_next(
            1,
            &next(
                "",
                vec![nanoprobe_sidecar::shell::DynamicSpawn {
                    airbase: 99_999,
                    enabled: true,
                    hot_start: false,
                }],
            ),
        )
        .expect("next");
        let e = g
            .reload_check(None, Some(&lease), 0)
            .expect_err("no airport");
        assert_eq!(e.code(), Code::InvalidArgument);
        let loads = mission_loads(&mut g).len();
        g.set_next(1, &next("", Vec::new())).expect("next");
        g.reload_check(None, Some(&lease), 0).expect("accepted");
        assert_eq!(mission_loads(&mut g).len(), loads, "a check sends nothing");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_create_whose_activation_fails_leaves_no_campaign() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
        let s = served_folder().await;
        // `shells` is a file: the shell cannot be saved.
        std::fs::write(s._dirs.path().join("missions").join("shells"), b"x").expect("file");
        let mut session = SessionServiceClient::new(s.channel.clone());
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let lease = session
            .acquire_lease(pb::AcquireLeaseRequest {
                client_name: "test".into(),
            })
            .await
            .expect("lease")
            .into_inner()
            .lease
            .expect("a lease");
        let before = entries(&s.store);
        let e = campaigns
            .create_campaign(create(&lease, "c7", "Caucasus", true))
            .await
            .expect_err("no shell");
        assert_eq!(e.code(), Code::FailedPrecondition, "{e}");
        assert!(e.message().contains("could not be written"), "{e}");
        let listed = campaigns
            .list_campaigns(pb::ListCampaignsRequest {})
            .await
            .expect("list")
            .into_inner();
        assert!(listed.campaigns.is_empty(), "{:?}", listed.campaigns);
        assert_eq!(entries(&s.store), before, "no folder for the refused one");
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    #[test]
    fn a_full_file_system_refuses_a_copy_and_a_template() {
        let (_dir, missions, cfg) = owed_setup();
        let hub = folder_hub(&cfg, &missions, 0x1000);
        hub.start();
        let mut g = hub.lock();
        g.add_campaign(
            &cfg,
            pb::Campaign {
                id: 1,
                name: "c1".into(),
                theaters: vec!["Caucasus".into()],
                ..pb::Campaign::default()
            },
            "Caucasus".into(),
        )
        .expect("add");
        assert!(g.space_check(1, 0).is_ok(), "a temp folder has room");
        g.space_extra = u64::MAX / 2;
        let low = |r: Result<(), Status>| {
            let e = r.expect_err("space low");
            assert_eq!(e.code(), Code::ResourceExhausted);
            assert_eq!(nanoprobe_proto::errors::reason(&e), ErrorReason::SpaceLow);
        };
        low(g.space_check(1, 0));
        low(g.set_template(1, "Caucasus", TEMPLATE));
        // A closed store is read through a copy, which needs room too.
        g.suspend(&cfg, 1, None);
        g.settle(Some(1));
        low(g.open_archive(1));
        g.space_extra = 0;
        g.open_archive(1).expect("room again");
    }

    #[test]
    fn activation_writes_the_shell_from_the_next_mission_and_the_clock_follows() {
        use nanoprobe_sidecar::shell::{self, DynamicSpawn, Start};
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(dir.path().to_path_buf()),
            dcs_dir: "Z:\\m".into(),
        };
        let cfg = Config::default();
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                missions,
            )),
        );
        hub.start();
        let start = Start {
            year: 2030,
            month: 6,
            day: 1,
            seconds: 36_000,
        };
        {
            let mut g = hub.lock();
            g.add_campaign(
                &cfg,
                pb::Campaign {
                    id: 1,
                    name: "c".into(),
                    ..pb::Campaign::default()
                },
                "Caucasus".into(),
            )
            .expect("add");
            let next = crate::next::Next {
                theater: String::new(),
                start_ms: Some(start.unix_ms()),
                weather: None,
                dynamic: vec![DynamicSpawn {
                    airbase: 12,
                    enabled: true,
                    hot_start: true,
                }],
            };
            let slot = g.campaigns.get_mut(&1).expect("slot");
            slot.store
                .set_next_mission(Some(next.to_lv().expect("encodes")));
            g.activate(&cfg, 1).expect("activate");
            assert_eq!(owed(&g).and_then(|o| o.start_ms), Some(start.unix_ms()));
        }
        let written = std::fs::read(dir.path().join("shells").join("1.a.miz")).expect("shell");
        assert_eq!(shell::start_of(&written), Ok(start));
        let warehouses = shell::members(&written)
            .expect("members")
            .into_iter()
            .find(|(n, _)| n == "warehouses")
            .expect("warehouses")
            .1;
        let text = String::from_utf8(warehouses).expect("utf-8");
        let airport = &text[text.find("[12] =").expect("airport 12")..];
        let airport = &airport[..airport.find("end of [12]").expect("end")];
        assert!(airport.contains("[\"dynamicSpawn\"] = true,"), "{airport}");
        assert!(airport.contains("[\"allowHotStart\"] = true,"), "{airport}");
        for _ in 0..300 {
            hub.step().expect("step");
        }
        let g = hub.lock();
        assert!(owed(&g).is_none(), "APPLIED");
        let s = &g.campaigns[&1].store;
        assert_eq!(s.mission_start_ms(), Some(start.unix_ms()));
        let next = crate::next::Next::from_lv(s.next_mission().expect("next")).expect("reads");
        assert_eq!(next.start_ms, None, "a set start is used once");
        assert_eq!(next.dynamic.len(), 1, "the rest is kept");
        let clock = g.clock_ms(1).expect("clock");
        assert!(
            clock >= start.unix_ms() && clock < start.unix_ms() + 3_600_000,
            "the clock runs from the shell's start: {clock}"
        );
    }

    #[test]
    fn a_campaign_template_is_checked_kept_and_written_into_its_shell() {
        use nanoprobe_sidecar::shell;
        let (_dir, missions, cfg) = owed_setup();
        let hub = folder_hub(&cfg, &missions, 0x1000);
        hub.start();
        let mut g = hub.lock();
        let meta = pb::Campaign {
            id: 1,
            name: "c1".into(),
            theaters: vec!["Caucasus".into()],
            ..pb::Campaign::default()
        };
        g.add_campaign(&cfg, meta, "Caucasus".into()).expect("add");
        let code = |r: Result<(), Status>| r.expect_err("refused").code();
        assert_eq!(
            code(g.set_template(1, "Syria", TEMPLATE)),
            Code::InvalidArgument
        );
        assert_eq!(
            code(g.set_template(1, "Caucasus", b"not a zip")),
            Code::InvalidArgument
        );
        // A placed unit whose name reads as a platform name.
        let platform = nanoprobe_transport::names::name(
            nanoprobe_transport::names::Kind::Group,
            nanoprobe_transport::names::tag_of(1),
            5,
        );
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (n, d) in shell::members(TEMPLATE).expect("members") {
            let d = if n == "mission" {
                String::from_utf8(d)
                    .expect("utf-8")
                    .replacen(
                        "[\"name\"] = \"neutrals\",\n\t\t\t[\"country\"] = {},",
                        &format!(
                            "[\"name\"] = \"neutrals\",\n\t\t\t[\"country\"] = {{ [1] = {{ [\"id\"] = 82, [\"name\"] = \"UN Peacekeepers\", [\"vehicle\"] = {{ [\"group\"] = {{ [1] = {{ [\"name\"] = \"placed\", [\"units\"] = {{ [1] = {{ [\"name\"] = {platform:?}, [\"type\"] = \"M-113\" }} }} }} }} }} }} }},"
                        ),
                        1,
                    )
                    .into_bytes()
            } else {
                d
            };
            w.start_file(n.as_str(), zip::write::SimpleFileOptions::default())
                .expect("start");
            std::io::Write::write_all(&mut w, &d).expect("write");
        }
        let named = w.finish().expect("zip").into_inner();
        let e = g
            .set_template(1, "Caucasus", &named)
            .expect_err("platform name");
        assert!(e.message().contains("platform name"), "{}", e.message());

        // The campaign's own template, with weather of its own, wins.
        let weather = nanoprobe_transport::codec::Lv::Table(vec![(
            nanoprobe_transport::codec::Lv::String(b"own".to_vec()),
            nanoprobe_transport::codec::Lv::Bool(true),
        )]);
        let own = shell::write(
            TEMPLATE,
            &shell::Runtime {
                weather: Some(weather.clone()),
                ..shell::Runtime::default()
            },
        )
        .expect("own");
        g.set_template(1, "Caucasus", &own).expect("set");
        assert_eq!(g.campaigns[&1].meta.templates, ["Caucasus"]);
        g.activate(&cfg, 1).expect("activate");
        let path = missions
            .dir
            .as_ref()
            .expect("dir")
            .join("shells")
            .join("1.a.miz");
        let written = std::fs::read(path).expect("shell");
        let mission = shell::members(&written)
            .expect("members")
            .into_iter()
            .find(|(n, _)| n == "mission")
            .expect("mission")
            .1;
        let d = shell::lua::parse(&mission).expect("parse");
        let w = d.root.get(&shell::lua::Key::s("weather")).expect("weather");
        assert_eq!(shell::lua::value(&mission, w), Some(weather));
        g.set_template(1, "Caucasus", b"").expect("remove");
        assert!(g.campaigns[&1].meta.templates.is_empty());
    }

    #[test]
    fn a_theater_name_is_a_file_name_and_nothing_else() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let m = crate::real::Missions {
            dir: Some(dir.path().to_path_buf()),
            dcs_dir: "/profile/missions".into(),
        };
        assert_eq!(m.template("Caucasus").as_deref(), Ok(TEMPLATE));
        assert_eq!(
            m.shell_dcs_path(42, b'b'),
            "/profile/missions/shells/42.b.miz"
        );
        for bad in ["", "caucasus", "../Caucasus", "Cau/casus", "Caucasus.miz"] {
            assert!(m.template(bad).is_err(), "{bad:?}");
        }
        let unset = crate::real::Missions {
            dir: Some(dir.path().to_path_buf()),
            dcs_dir: String::new(),
        };
        assert!(unset.template("Caucasus").is_err());
        let long = crate::real::Missions {
            dir: None,
            dcs_dir: "x".repeat(crate::real::PATH_MAX),
        };
        assert!(long.check().is_err());
    }

    /// A hub over [`Folder`] with its store on disk, served on a loopback
    /// port with its driver running. `Caucasus.miz` is the only mission.
    struct Served {
        hub: Arc<Hub>,
        store: std::path::PathBuf,
        channel: tonic::transport::Channel,
        stop: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<Result<(), String>>,
        _dirs: tempfile::TempDir,
    }

    async fn served_folder() -> Served {
        let dirs = tempfile::tempdir().expect("dir");
        let missions = dirs.path().join("missions");
        let store = dirs.path().join("store");
        std::fs::create_dir_all(&missions).expect("missions");
        std::fs::create_dir_all(&store).expect("store");
        std::fs::write(missions.join("Caucasus.miz"), TEMPLATE).expect("miz");
        let cfg = Config {
            store: Some(crate::config::StoreFiles::new(store.clone())),
            ..Config::default()
        };
        let hub = Hub::open(
            cfg,
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                crate::real::Missions {
                    dir: Some(missions),
                    dcs_dir: "Z:\\profile\\missions\\".into(),
                },
            )),
        )
        .expect("hub");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let routes = crate::server::routes(&hub, crate::auth::Tokens::open()).expect("routes");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(crate::server::serve(
            Arc::clone(&hub),
            routes,
            listener,
            async move {
                let _ = stopped.await;
            },
        ));
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .expect("endpoint")
            .connect()
            .await
            .expect("connect");
        Served {
            hub,
            store,
            channel,
            stop,
            task,
            _dirs: dirs,
        }
    }

    fn entries(dir: &std::path::Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn create(
        lease: &pb::Lease,
        name: &str,
        theater: &str,
        activate: bool,
    ) -> pb::CreateCampaignRequest {
        pb::CreateCampaignRequest {
            lease: Some(lease.clone()),
            name: name.into(),
            theaters: vec![theater.into()],
            first_theater: theater.into(),
            clock: None,
            activate,
            idempotency: Some(pb::Idempotency {
                key: format!("create-{name}"),
            }),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn export_reload_and_delete_through_the_services() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
        use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
        let s = served_folder().await;
        let mut session = SessionServiceClient::new(s.channel.clone());
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let mut mission = MissionServiceClient::new(s.channel.clone());
        let lease = session
            .acquire_lease(pb::AcquireLeaseRequest {
                client_name: "test".into(),
            })
            .await
            .expect("lease")
            .into_inner()
            .lease
            .expect("a lease");
        let id_of = |r: pb::CreateCampaignResponse| r.campaign.expect("campaign").id;
        let a = id_of(
            campaigns
                .create_campaign(create(&lease, "a", "Caucasus", true))
                .await
                .expect("a")
                .into_inner(),
        );
        let b = id_of(
            campaigns
                .create_campaign(create(&lease, "b", "Caucasus", false))
                .await
                .expect("b")
                .into_inner(),
        );
        let mut stream = campaigns
            .export_campaign(pb::ExportCampaignRequest { campaign_id: a })
            .await
            .expect("export")
            .into_inner();
        let mut bytes = Vec::new();
        while let Some(m) = stream.message().await.expect("message") {
            assert!(m.lines.ends_with(b"\n"), "whole lines");
            bytes.extend(m.lines);
        }
        let first: serde_json::Value =
            serde_json::from_slice(bytes.split(|b| *b == b'\n').next().expect("line"))
                .expect("json");
        assert_eq!(first["kind"], "header");
        assert_eq!(first["campaign_id"], a);

        let write = |key: &str| pb::Write {
            lease: Some(lease.clone()),
            idempotency: Some(pb::Idempotency { key: key.into() }),
            campaign_id: 0,
        };
        let e = mission
            .set_next_mission(pb::SetNextMissionRequest {
                write: Some(write("next-bad")),
                next: Some(pb::NextMission {
                    theater: "Afghanistan".into(),
                    ..pb::NextMission::default()
                }),
            })
            .await
            .expect_err("not a theater of the campaign");
        assert_eq!(e.code(), Code::InvalidArgument);
        // The activation's load is applied before a reload is taken.
        for _ in 0..100 {
            if s.hub.lock().campaigns[&a].store.owed_load().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let r = mission
            .reload(pb::ReloadRequest {
                write: Some(write("reload-1")),
            })
            .await
            .expect("reload")
            .into_inner();
        assert_eq!(
            r.result.expect("result").outcome,
            pb::Outcome::Pending as i32
        );

        let delete = |id, name: &str, key: &str| pb::DeleteCampaignRequest {
            lease: Some(lease.clone()),
            campaign_id: id,
            confirm_name: name.into(),
            idempotency: Some(pb::Idempotency { key: key.into() }),
        };
        let e = campaigns
            .delete_campaign(delete(b, "not b", "del-1"))
            .await
            .expect_err("wrong name");
        assert_eq!(e.code(), Code::InvalidArgument);
        let e = campaigns
            .delete_campaign(delete(a, "a", "del-2"))
            .await
            .expect_err("active");
        assert_eq!(e.code(), Code::FailedPrecondition);
        campaigns
            .delete_campaign(delete(b, "b", "del-3"))
            .await
            .expect("delete b");
        assert!(
            !entries(&s.store).contains(&b.to_string()),
            "folder removed"
        );
        let listed = campaigns
            .list_campaigns(pb::ListCampaignsRequest {})
            .await
            .expect("list")
            .into_inner();
        assert_eq!(listed.campaigns.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_theater_changes_nothing_through_the_services() {
        use nanoprobe_proto::errors;
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
        let s = served_folder().await;
        let mut session = SessionServiceClient::new(s.channel.clone());
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let lease = session
            .acquire_lease(pb::AcquireLeaseRequest {
                client_name: "test".into(),
            })
            .await
            .expect("lease")
            .into_inner()
            .lease
            .expect("a lease");

        // A create with activation on a theater with no file creates
        // nothing, in memory or on disk.
        let before = entries(&s.store);
        let e = campaigns
            .create_campaign(create(&lease, "x", "Afghanistan", true))
            .await
            .expect_err("no Afghanistan.miz");
        assert_eq!(e.code(), Code::FailedPrecondition, "{e}");
        let d = errors::detail(&e).expect("detail");
        assert_eq!(d.reason, pb::ErrorReason::Unspecified as i32);
        assert_eq!(d.field, "theater");
        let listed = campaigns
            .list_campaigns(pb::ListCampaignsRequest {})
            .await
            .expect("list")
            .into_inner();
        assert!(listed.campaigns.is_empty(), "{:?}", listed.campaigns);
        assert_eq!(entries(&s.store), before, "no folder for the refused one");

        // An activation of a campaign with no file changes nothing.
        let a = campaigns
            .create_campaign(create(&lease, "a", "Caucasus", true))
            .await
            .expect("a")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        let b = campaigns
            .create_campaign(create(&lease, "b", "Afghanistan", false))
            .await
            .expect("b")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        // Let a's MISSION_LOAD go out before the refused call.
        for _ in 0..200 {
            if owed(&s.hub.lock()).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let revision = s.hub.lock().campaigns[&a].store.revision();
        let e = campaigns
            .activate_campaign(pb::ActivateCampaignRequest {
                lease: Some(lease.clone()),
                campaign_id: b,
                idempotency: Some(pb::Idempotency {
                    key: "act-b".into(),
                }),
            })
            .await
            .expect_err("no Afghanistan.miz");
        assert_eq!(e.code(), Code::FailedPrecondition, "{e}");
        tokio::time::sleep(Duration::from_millis(100)).await;
        {
            let mut g = s.hub.lock();
            assert_eq!(g.active, Some(a), "a stays active");
            assert_eq!(g.campaigns[&a].meta.state, pb::CampaignState::Active as i32);
            assert_eq!(g.campaigns[&a].store.revision(), revision);
            assert_eq!(mission_loads(&mut g).len(), 1, "only a's MISSION_LOAD");
        }
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    /// A lease of a served hub, for a test.
    async fn lease_of(channel: &tonic::transport::Channel) -> pb::Lease {
        use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
        SessionServiceClient::new(channel.clone())
            .acquire_lease(pb::AcquireLeaseRequest {
                client_name: "test".into(),
            })
            .await
            .expect("lease")
            .into_inner()
            .lease
            .expect("a lease")
    }

    /// Campaign 1 running from 2030-01-01 on the folder hub, past its first
    /// checkpoint's worth of model time, and a campaign 2 beside it.
    fn two_campaigns_with_a_running_clock() -> (tempfile::TempDir, Config, Arc<Hub>, i64) {
        use nanoprobe_sidecar::shell::Start;
        let (dir, missions, cfg) = owed_setup();
        let hub = folder_hub(&cfg, &missions, 0x1000);
        hub.start();
        let at = Start {
            year: 2030,
            month: 1,
            day: 1,
            seconds: 0,
        }
        .unix_ms();
        {
            let mut g = hub.lock();
            for (id, name) in [(1, "a"), (2, "b")] {
                g.add_campaign(
                    &cfg,
                    pb::Campaign {
                        id,
                        name: name.into(),
                        clock: Some(prost_types::Timestamp {
                            seconds: at / 1000,
                            nanos: 0,
                        }),
                        ..pb::Campaign::default()
                    },
                    "Caucasus".into(),
                )
                .expect("add");
            }
            g.activate(&cfg, 1).expect("activate");
        }
        steps(&hub, 400);
        (dir, cfg, hub, at)
    }

    /// A switch leaves the old campaign as a suspend does: its clock is the
    /// running one, and its store holds no mission start and no owed load.
    #[test]
    fn a_switch_suspends_the_old_campaign_with_its_running_clock() {
        let (_dir, cfg, hub, at) = two_campaigns_with_a_running_clock();
        let mut g = hub.lock();
        let running = g.clock_ms(1).expect("clock");
        assert!(running > at, "the clock ran");
        g.activate(&cfg, 2).expect("switch");
        assert_eq!(
            g.campaigns[&1].meta.clock.as_ref().map(ms_of),
            Some(running)
        );
        g.open_store(1).expect("open");
        let s = &g.campaigns[&1].store;
        assert_eq!(s.mission_start_ms(), None);
        assert!(s.owed_load().is_none());
    }

    /// After a switch away and back, a refused load leaves the clock where
    /// the record had it.
    #[test]
    fn a_refused_load_after_a_switch_back_keeps_the_record_clock() {
        let (_dir, cfg, hub, _at) = two_campaigns_with_a_running_clock();
        let running = {
            let mut g = hub.lock();
            let running = g.clock_ms(1).expect("clock");
            g.activate(&cfg, 2).expect("switch");
            running
        };
        steps(&hub, 300);
        {
            // DCS restarts while the other campaign runs, then loads
            // nothing for campaign 1 but a refused mission.
            let mut g = hub.lock();
            let fake = |g: &mut Inner| {
                g.backend
                    .as_any()
                    .expect("fake")
                    .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                    .expect("fake")
                    .restart_dcs();
            };
            fake(&mut g);
            let boot = g.sidecar_boot;
            g.backend.attach(boot);
        }
        steps(&hub, 300);
        {
            let mut g = hub.lock();
            g.backend
                .as_any()
                .expect("fake")
                .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake")
                .refuse_loads(1);
            g.activate(&cfg, 1).expect("back");
        }
        for _ in 0..400 {
            hub.step().expect("step");
            if hub.lock().campaigns[&1].store.owed_load().is_none() {
                break;
            }
        }
        steps(&hub, 50);
        let clock = hub.lock().clock_ms(1).expect("clock");
        assert!(
            clock >= running && clock < running + 3_600_000,
            "the clock stays with the record's: {clock} against {running}"
        );
    }

    /// The clock of a create is a date a mission can carry, as a next
    /// mission's start is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_create_clock_outside_years_1_to_9999_is_refused() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        let s = served_folder().await;
        let lease = lease_of(&s.channel).await;
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let mut n = 0;
        for (seconds, nanos, ok) in [
            (-62_135_596_800, 0, true),
            (253_402_300_799, 999_999_999, true),
            (-62_135_596_801, 0, false),
            (253_402_300_800, 0, false),
            (i64::MAX, 0, false),
            (0, -1, false),
        ] {
            n += 1;
            let mut r = create(&lease, &format!("c{n}"), "Caucasus", false);
            r.clock = Some(prost_types::Timestamp { seconds, nanos });
            let before = entries(&s.store).len();
            let got = campaigns.create_campaign(r).await;
            if ok {
                got.expect("in range");
                continue;
            }
            let e = got.expect_err("out of range");
            assert_eq!(e.code(), Code::InvalidArgument, "{seconds} {nanos}");
            assert_eq!(
                nanoprobe_proto::errors::detail(&e).expect("detail").field,
                "clock"
            );
            assert_eq!(entries(&s.store).len(), before, "no campaign was made");
        }
        let listed = campaigns
            .list_campaigns(pb::ListCampaignsRequest {})
            .await
            .expect("list")
            .into_inner();
        assert_eq!(listed.campaigns.len(), 2);
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    /// DCS restarts into a mission on a map the campaign lacks; a reload with
    /// no next theater brings the campaign back to its own map.
    #[test]
    fn a_reload_after_a_restart_on_a_foreign_theater_uses_the_campaign_s_own() {
        let (_dir, missions, cfg) = owed_setup();
        let hub = folder_hub(&cfg, &missions, 0x1000);
        hub.start();
        add_and_activate(&hub, &cfg, true);
        steps(&hub, 300);
        {
            let mut g = hub.lock();
            let fake = g
                .backend
                .as_any()
                .expect("fake")
                .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake");
            fake.restart_dcs();
            fake.load_mission_on("Afghanistan");
            let boot = g.sidecar_boot;
            g.backend.attach(boot);
        }
        steps(&hub, 300);
        let g = hub.lock();
        assert_eq!(
            g.campaigns[&1].store.loaded().map(|l| l.theater.clone()),
            Some("Afghanistan".into()),
            "DCS runs a map the campaign lacks"
        );
        assert_eq!(g.reload_theater(1).expect("theater"), "Caucasus");
    }

    /// A start set for another theater waits for a load of that theater.
    #[test]
    fn a_start_for_another_theater_survives_a_shell_write() {
        let (_dir, missions, cfg) = owed_setup();
        let hub = folder_hub(&cfg, &missions, 0x1000);
        hub.start();
        let mut g = hub.lock();
        g.add_campaign(
            &cfg,
            pb::Campaign {
                id: 1,
                name: "c1".into(),
                theaters: vec!["Caucasus".into(), "Afghanistan".into()],
                ..pb::Campaign::default()
            },
            "Caucasus".into(),
        )
        .expect("add");
        g.set_next(
            1,
            &crate::next::Next {
                theater: "Afghanistan".into(),
                start_ms: Some(1_900_000_000_000),
                ..crate::next::Next::default()
            },
        )
        .expect("next");
        let start = |g: &Inner| {
            crate::next::Next::from_lv(g.campaigns[&1].store.next_mission().expect("next"))
                .expect("reads")
                .start_ms
        };
        g.shell_written(1, "Caucasus");
        assert_eq!(start(&g), Some(1_900_000_000_000), "kept for Afghanistan");
        g.shell_written(1, "Afghanistan");
        assert_eq!(start(&g), None, "used by the Afghanistan shell");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_file_system_refuses_a_backup_and_shows_in_the_health() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
        let s = served_folder().await;
        let lease = lease_of(&s.channel).await;
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let mut mission = MissionServiceClient::new(s.channel.clone());
        let a = campaigns
            .create_campaign(create(&lease, "a", "Caucasus", true))
            .await
            .expect("a")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        let status = |m: &mut MissionServiceClient<_>| {
            let mut m = m.clone();
            async move {
                m.get_server_status(pb::GetServerStatusRequest {})
                    .await
                    .expect("status")
                    .into_inner()
                    .status
                    .expect("status")
            }
        };
        let ok = status(&mut mission).await;
        assert!(!ok.store_space_low, "a temp folder has room");
        assert!(ok.store_free_bytes.is_some());

        s.hub.lock().space_extra = u64::MAX / 2;
        let low = status(&mut mission).await;
        assert!(low.store_space_low, "the verdict shows the shortage");
        let backup = |key: &str| pb::BackupCampaignRequest {
            lease: Some(lease.clone()),
            campaign_id: a,
            idempotency: Some(pb::Idempotency { key: key.into() }),
        };
        let e = campaigns
            .backup_campaign(backup("bk-1"))
            .await
            .expect_err("space low");
        assert_eq!(e.code(), Code::ResourceExhausted, "{e}");
        assert_eq!(
            nanoprobe_proto::errors::reason(&e),
            nanoprobe_proto::v1alpha1::ErrorReason::SpaceLow
        );
        // A scheduled backup that is due is skipped, and the next one waits
        // for the interval.
        {
            let cfg = s.hub.cfg.clone();
            let mut g = s.hub.lock();
            g.next_backup_ms = 0;
            g.timers(&cfg);
            assert!(g.backup_job.is_none(), "no scheduled backup was started");
            assert!(g.next_backup_ms > wall_ms(), "the next one waits");
            let files = g.files().expect("files");
            assert!(files.backups(CampaignId(a)).expect("backups").is_empty());
        }
        s.hub.lock().space_extra = 0;
        campaigns
            .backup_campaign(backup("bk-2"))
            .await
            .expect("room again");
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_delete_without_a_name_needs_a_confirmation() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        let s = served_folder().await;
        let lease = lease_of(&s.channel).await;
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let b = campaigns
            .create_campaign(create(&lease, "b", "Caucasus", false))
            .await
            .expect("b")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        let e = campaigns
            .delete_campaign(pb::DeleteCampaignRequest {
                lease: Some(lease.clone()),
                campaign_id: b,
                confirm_name: String::new(),
                idempotency: Some(pb::Idempotency {
                    key: "del-e".into(),
                }),
            })
            .await
            .expect_err("no name");
        assert_eq!(e.code(), Code::FailedPrecondition, "{e}");
        assert_eq!(
            nanoprobe_proto::errors::reason(&e),
            nanoprobe_proto::v1alpha1::ErrorReason::ConfirmationRequired
        );
        assert!(entries(&s.store).contains(&b.to_string()));
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    /// A folder that cannot be emptied: the delete answers `INTERNAL` and
    /// the campaign is still listed, with its events log open.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_delete_that_cannot_remove_the_folder_keeps_the_campaign() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
        use std::os::unix::fs::PermissionsExt;
        let s = served_folder().await;
        let lease = lease_of(&s.channel).await;
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let mut events = EventsServiceClient::new(s.channel.clone());
        let b = campaigns
            .create_campaign(create(&lease, "b", "Caucasus", false))
            .await
            .expect("b")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        let folder = s.store.join(b.to_string());
        let set = |mode| {
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode)).expect("mode")
        };
        set(0o555);
        // A user that ignores permissions (root) can remove it: skip.
        if std::fs::File::create(folder.join("probe")).is_ok() {
            let _ = std::fs::remove_file(folder.join("probe"));
            set(0o755);
            let _ = s.stop.send(());
            s.task.await.expect("join").expect("serve");
            return;
        }
        let r = campaigns
            .delete_campaign(pb::DeleteCampaignRequest {
                lease: Some(lease.clone()),
                campaign_id: b,
                confirm_name: "b".into(),
                idempotency: Some(pb::Idempotency {
                    key: "del-x".into(),
                }),
            })
            .await;
        set(0o755);
        let e = r.expect_err("not removable");
        assert_eq!(e.code(), Code::Internal, "{e}");
        assert_eq!(
            nanoprobe_proto::errors::reason(&e),
            nanoprobe_proto::v1alpha1::ErrorReason::Internal
        );
        let listed = campaigns
            .list_campaigns(pb::ListCampaignsRequest {})
            .await
            .expect("list")
            .into_inner();
        assert_eq!(listed.campaigns.len(), 1, "{:?}", listed.campaigns);
        events
            .read_events(pb::ReadEventsRequest {
                filter: Some(pb::EventFilter {
                    campaign_id: b,
                    ..pb::EventFilter::default()
                }),
                after_sequence: 0,
                limit: 0,
            })
            .await
            .expect("the campaign's events still read");
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    /// `GetCampaign` shows the clock the campaign runs, not the one the last
    /// checkpoint wrote.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn get_campaign_shows_the_running_clock_before_any_checkpoint() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_sidecar::shell::Start;
        let s = served_folder().await;
        let lease = lease_of(&s.channel).await;
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let a = campaigns
            .create_campaign(create(&lease, "a", "Caucasus", false))
            .await
            .expect("a")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        let start = Start {
            year: 2040,
            month: 2,
            day: 3,
            seconds: 7_200,
        }
        .unix_ms();
        s.hub
            .lock()
            .set_next(
                a,
                &crate::next::Next {
                    start_ms: Some(start),
                    ..crate::next::Next::default()
                },
            )
            .expect("next");
        campaigns
            .activate_campaign(pb::ActivateCampaignRequest {
                lease: Some(lease.clone()),
                campaign_id: a,
                idempotency: Some(pb::Idempotency {
                    key: "act-a".into(),
                }),
            })
            .await
            .expect("activate");
        for _ in 0..400 {
            if s.hub.lock().campaigns[&a].store.owed_load().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let c = campaigns
            .get_campaign(pb::GetCampaignRequest { campaign_id: a })
            .await
            .expect("get")
            .into_inner()
            .campaign
            .expect("campaign");
        let clock = c.clock.expect("clock");
        let ms = clock.seconds * 1000 + i64::from(clock.nanos) / 1_000_000;
        assert!(
            ms >= start && ms < start + 3_600_000,
            "the clock shows the shell's start: {ms}"
        );
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    /// Serve `hub` on a loopback port with its driver running.
    async fn serve_hub(
        hub: &Arc<Hub>,
    ) -> (
        tonic::transport::Channel,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), String>>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let routes = crate::server::routes(hub, crate::auth::Tokens::open()).expect("routes");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(crate::server::serve(
            Arc::clone(hub),
            routes,
            listener,
            async move {
                let _ = stopped.await;
            },
        ));
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .expect("endpoint")
            .connect()
            .await
            .expect("connect");
        (channel, stop, task)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_repeat_after_a_restart_gets_the_first_answer() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
        let (dir, missions, cfg) = owed_setup();
        let hub = Hub::open(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                missions.clone(),
            )),
        )
        .expect("hub");
        let (channel, stop, task) = serve_hub(&hub).await;
        let lease = SessionServiceClient::new(channel.clone())
            .acquire_lease(pb::AcquireLeaseRequest {
                client_name: "test".into(),
            })
            .await
            .expect("lease")
            .into_inner()
            .lease
            .expect("a lease");
        let first = CampaignServiceClient::new(channel)
            .create_campaign(create(&lease, "a", "Caucasus", true))
            .await
            .expect("created")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        let _ = stop.send(());
        task.await.expect("join").expect("serve");
        hub.lock().close_all().expect("close");
        drop(hub);

        // The file is gone, and DCS and the sidecar are new.
        std::fs::remove_file(dir.path().join("missions").join("Caucasus.miz")).expect("remove");
        let hub = Hub::open(
            cfg,
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                missions,
            )),
        )
        .expect("hub");
        let (channel, stop, task) = serve_hub(&hub).await;
        let lease = SessionServiceClient::new(channel.clone())
            .acquire_lease(pb::AcquireLeaseRequest {
                client_name: "test".into(),
            })
            .await
            .expect("lease")
            .into_inner()
            .lease
            .expect("a lease");
        let again = CampaignServiceClient::new(channel)
            .create_campaign(create(&lease, "a", "Caucasus", true))
            .await
            .expect("the repeat gets the first answer, not a refusal")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        assert_eq!(again, first);
        let _ = stop.send(());
        task.await.expect("join").expect("serve");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_activation_leaves_a_live_group_alone() {
        use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
        use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
        let s = served_folder().await;
        let mut session = SessionServiceClient::new(s.channel.clone());
        let mut campaigns = CampaignServiceClient::new(s.channel.clone());
        let lease = session
            .acquire_lease(pb::AcquireLeaseRequest {
                client_name: "test".into(),
            })
            .await
            .expect("lease")
            .into_inner()
            .lease
            .expect("a lease");
        let a = campaigns
            .create_campaign(create(&lease, "a", "Caucasus", true))
            .await
            .expect("a")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        let b = campaigns
            .create_campaign(create(&lease, "b", "Afghanistan", false))
            .await
            .expect("b")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        for _ in 0..200 {
            if owed(&s.hub.lock()).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let group = {
            let mut g = s.hub.lock();
            let unit = nanoprobe_sidecar::store::NewUnit::new(
                "M-113",
                nanoprobe_sidecar::store::UnitState::default(),
            );
            g.campaigns
                .get_mut(&a)
                .expect("slot")
                .store
                .create_group(nanoprobe_sidecar::store::NewGroup {
                    theater: "Caucasus".into(),
                    country: 2,
                    category: 2,
                    coalition: 2,
                    units: vec![unit],
                    authority: nanoprobe_sidecar::store::Authority::Sim,
                    label: String::new(),
                    extra: nanoprobe_transport::codec::Lv::Nil,
                })
                .expect("group")
        };
        let live = |hub: &Hub| {
            hub.lock().campaigns[&a].store.group(group).map(|g| g.state)
                == Some(nanoprobe_sidecar::lifecycle::State::Live)
        };
        for _ in 0..400 {
            if live(&s.hub) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(live(&s.hub), "the group is live in DCS");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (revision, taken) = {
            let mut g = s.hub.lock();
            let taken = {
                let any = g.backend.as_any().expect("fake");
                any.downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                    .expect("fake")
                    .ledger()
                    .taken
                    .len()
            };
            (g.campaigns[&a].store.revision(), taken)
        };
        let e = campaigns
            .activate_campaign(pb::ActivateCampaignRequest {
                lease: Some(lease.clone()),
                campaign_id: b,
                idempotency: Some(pb::Idempotency {
                    key: "act-b".into(),
                }),
            })
            .await
            .expect_err("no Afghanistan.miz");
        assert_eq!(e.code(), Code::FailedPrecondition, "{e}");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(live(&s.hub), "the group is still live");
        {
            let mut g = s.hub.lock();
            assert_eq!(g.active, Some(a));
            assert_eq!(g.campaigns[&a].store.revision(), revision);
            let any = g.backend.as_any().expect("fake");
            let after = any
                .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake")
                .ledger()
                .taken
                .len();
            assert_eq!(after, taken, "no command, no CAPTURE, reached DCS");
        }
        // A target whose next mission names an airport its template lacks is
        // refused the same way: nothing is captured.
        let c = campaigns
            .create_campaign(create(&lease, "c", "Caucasus", false))
            .await
            .expect("c")
            .into_inner()
            .campaign
            .expect("campaign")
            .id;
        s.hub
            .lock()
            .set_next(
                c,
                &crate::next::Next {
                    dynamic: vec![nanoprobe_sidecar::shell::DynamicSpawn {
                        airbase: 99_999,
                        enabled: true,
                        hot_start: false,
                    }],
                    ..crate::next::Next::default()
                },
            )
            .expect("next");
        let e = campaigns
            .activate_campaign(pb::ActivateCampaignRequest {
                lease: Some(lease.clone()),
                campaign_id: c,
                idempotency: Some(pb::Idempotency {
                    key: "act-c".into(),
                }),
            })
            .await
            .expect_err("no such airport");
        assert_eq!(e.code(), Code::InvalidArgument, "{e}");
        assert_eq!(
            nanoprobe_proto::errors::detail(&e).expect("detail").field,
            "next.dynamic_spawn"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
        {
            let mut g = s.hub.lock();
            assert_eq!(g.active, Some(a));
            assert_eq!(g.campaigns[&a].store.revision(), revision);
            let any = g.backend.as_any().expect("fake");
            let after = any
                .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake")
                .ledger()
                .taken
                .len();
            assert_eq!(after, taken, "nothing was captured");
        }
        let _ = s.stop.send(());
        s.task.await.expect("join").expect("serve");
    }

    /// The load's result and the new generation's `LIFECYCLE` fold in one
    /// step; the hold is stamped from the load's own row, so the group still
    /// waits for the mission the load made.
    #[test]
    fn the_hold_is_stamped_when_the_result_and_the_generation_fold_together() {
        let missions_dir = tempfile::tempdir().expect("dir");
        std::fs::write(missions_dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let missions = crate::real::Missions {
            dir: Some(missions_dir.path().to_path_buf()),
            dcs_dir: "Z:\\m".into(),
        };
        let cfg = Config::default();
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config {
                    load_delay_frames: 0,
                    ..nanoprobe_sim_fake::Config::default()
                }),
                missions,
            )),
        );
        hub.start();
        steps(&hub, 20);
        let before = {
            let mut g = hub.lock();
            let any = g.backend.as_any().expect("fake");
            any.downcast_mut::<nanoprobe_sim_fake::FakeSim>()
                .expect("fake")
                .mission_gen()
        };
        {
            let mut g = hub.lock();
            let meta = pb::Campaign {
                id: 1,
                name: "c1".into(),
                ..pb::Campaign::default()
            };
            g.add_campaign(&cfg, meta, "Caucasus".into()).expect("add");
            let unit = nanoprobe_sidecar::store::NewUnit::new(
                "M-113",
                nanoprobe_sidecar::store::UnitState::default(),
            );
            g.campaigns
                .get_mut(&1)
                .expect("slot")
                .store
                .create_group(nanoprobe_sidecar::store::NewGroup {
                    theater: "Caucasus".into(),
                    country: 2,
                    category: 2,
                    coalition: 2,
                    units: vec![unit],
                    authority: nanoprobe_sidecar::store::Authority::Sim,
                    label: String::new(),
                    extra: nanoprobe_transport::codec::Lv::Nil,
                })
                .expect("group");
            g.activate(&cfg, 1).expect("activate");
        }
        // Step until the load is sent; then let DCS run several frames, so
        // the result and the new generation's records wait together.
        for _ in 0..200 {
            if owed(&hub.lock()).is_some_and(|o| o.cmd.is_some()) {
                break;
            }
            hub.step().expect("step");
        }
        assert!(owed(&hub.lock()).is_some_and(|o| o.cmd.is_some()));
        {
            let mut g = hub.lock();
            for _ in 0..5 {
                g.backend.frame();
            }
        }
        steps(&hub, 300);
        let mut g = hub.lock();
        assert_eq!(mission_loads(&mut g).len(), 1);
        let any = g.backend.as_any().expect("fake");
        let spawns = &any
            .downcast_mut::<nanoprobe_sim_fake::FakeSim>()
            .expect("fake")
            .ledger()
            .spawns;
        assert!(!spawns.is_empty(), "the group materialized after the load");
        assert!(
            spawns
                .iter()
                .all(|(_, generation, ..)| *generation > before),
            "a spawn went into the previous mission: {spawns:?}, generation before {before}"
        );
    }

    /// The first MISSION_LOAD is written and the link drops before the agent
    /// takes it. The reconciler resolves it `NotSent` at the next HELLO and
    /// the hub sends it again; the agent's ledger shows the second only.
    #[test]
    fn a_mission_load_the_agent_never_took_is_sent_again() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("Caucasus.miz"), TEMPLATE).expect("miz");
        let cfg = Config::default();
        let hub = Hub::new(
            cfg.clone(),
            Box::new(Folder(
                nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
                crate::real::Missions {
                    dir: Some(dir.path().to_path_buf()),
                    dcs_dir: "Z:\\missions\\".into(),
                },
            )),
        );
        hub.start();
        {
            let mut g = hub.lock();
            g.add_campaign(
                &cfg,
                pb::Campaign {
                    id: 1,
                    name: "c".into(),
                    ..pb::Campaign::default()
                },
                "Caucasus".into(),
            )
            .expect("add");
            g.activate(&cfg, 1).expect("activate");
        }
        // Step until the load is written, then drop the link before the
        // fake's next frame takes it.
        let mut written = false;
        for _ in 0..200 {
            hub.step().expect("step");
            let mut g = hub.lock();
            if owed(&g).is_some_and(|p| p.cmd.is_some()) {
                let boot = g.sidecar_boot;
                g.backend.detach();
                g.backend.attach(boot);
                written = true;
                break;
            }
        }
        assert!(written, "the MISSION_LOAD was never written");
        assert!(mission_loads(&mut hub.lock()).is_empty(), "not taken yet");
        for _ in 0..200 {
            hub.step().expect("step");
        }
        let mut g = hub.lock();
        assert_eq!(
            mission_loads(&mut g),
            ["Z:\\missions\\shells\\1.a.miz"],
            "sent again after NotSent"
        );
        assert!(owed(&g).is_none());
    }

    #[tokio::test]
    async fn a_panicking_driver_ends_the_server_with_an_error() {
        let hub = Hub::new(Config::default(), Box::new(Broken));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let routes = crate::server::routes(&hub, crate::auth::Tokens::open()).expect("routes");
        let r = tokio::time::timeout(
            Duration::from_secs(10),
            crate::server::serve(Arc::clone(&hub), routes, listener, std::future::pending()),
        )
        .await
        .expect("serve returns");
        let e = r.expect_err("the driver stopped");
        assert!(e.contains("a broken frame"), "{e}");
        // The panic stopped the hub: changing calls are refused.
        assert!(hub.closing());
        assert!(hub.guarded(|_| Ok(())).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_waiting_for_the_lock_does_not_run_after_a_panic() {
        let hub = Hub::new(Config::default(), Box::new(Silent));
        let lease = {
            let mut g = hub.lock();
            let grant = g
                .leases
                .acquire(Instant::now(), wall_ms(), "id".into())
                .expect("lease");
            pb::Lease {
                epoch: grant.epoch,
                lease_id: grant.lease_id,
            }
        };
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let a = {
            let (hub, lease) = (Arc::clone(&hub), lease.clone());
            tokio::spawn(async move {
                let key = pb::Idempotency { key: "a".into() };
                let call = Call {
                    method: "Test",
                    key: Some(&key),
                    lease: Some(&lease),
                    campaign: 0,
                    need: Need::Nothing,
                };
                hub.write(
                    call,
                    &pb::HelloRequest::default(),
                    |_, _| -> Result<(pb::HelloResponse, u64), Status> {
                        // Hold the lock long enough for B to queue behind it.
                        std::thread::sleep(Duration::from_millis(300));
                        panic!("boom")
                    },
                )
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let b = {
            let (hub, lease, ran) = (Arc::clone(&hub), lease.clone(), Arc::clone(&ran));
            tokio::spawn(async move {
                let key = pb::Idempotency { key: "b".into() };
                let call = Call {
                    method: "Test",
                    key: Some(&key),
                    lease: Some(&lease),
                    campaign: 0,
                    need: Need::Nothing,
                };
                hub.write(call, &pb::HelloRequest::default(), |_, c| {
                    ran.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok((pb::HelloResponse::default(), c))
                })
                .await
            })
        };
        let ea = a.await.expect("task").expect_err("A panicked");
        assert_eq!(ea.code(), Code::Internal);
        let eb = b.await.expect("task").expect_err("B is refused");
        assert_eq!(eb.code(), Code::Unavailable);
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "B did not run"
        );
    }

    #[tokio::test]
    async fn a_call_that_panics_stops_the_server_and_keeps_its_key() {
        let hub = Hub::new(Config::default(), Box::new(Silent));
        let lease = {
            let mut g = hub.lock();
            let grant = g
                .leases
                .acquire(Instant::now(), wall_ms(), "id".into())
                .expect("lease");
            pb::Lease {
                epoch: grant.epoch,
                lease_id: grant.lease_id,
            }
        };
        let key = pb::Idempotency { key: "k".into() };
        let call = Call {
            method: "Test",
            key: Some(&key),
            lease: Some(&lease),
            campaign: 0,
            need: Need::Nothing,
        };
        let req = pb::HelloRequest::default();
        let e = hub
            .write(
                call,
                &req,
                |_, _| -> Result<(pb::HelloResponse, u64), Status> { panic!("boom") },
            )
            .await
            .expect_err("the panic fails the call");
        assert_eq!(e.code(), Code::Internal);
        // The server stops: streams end and the driver returns an error.
        assert!(hub.closing());
        let d = tokio::time::timeout(Duration::from_secs(5), Arc::clone(&hub).drive())
            .await
            .expect("the driver returns");
        assert!(d.is_err());
        // No call runs any more, a repeat of the key or any other.
        let again = hub
            .write(call, &req, |_, c| Ok((pb::HelloResponse::default(), c)))
            .await
            .expect_err("stopping");
        assert_eq!(again.code(), Code::Unavailable);
        assert!(hub.lock().idem.running("k"), "the key stays running");
        // The lease calls are refused too.
        use nanoprobe_proto::v1alpha1::session_service_server::SessionService;
        let mut req = tonic::Request::new(pb::AcquireLeaseRequest::default());
        req.extensions_mut().insert(crate::auth::Access::Admin);
        let e = crate::services::session::Session(Arc::clone(&hub))
            .acquire_lease(req)
            .await
            .expect_err("stopping");
        assert_eq!(e.code(), Code::Unavailable);
    }
}
