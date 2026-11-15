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
    CampaignRecord, CampaignState, Checkpoint, DiskError, LostPolicy, Outcome, Store, TierPeriod,
    checkpoint_of,
};
use nanoprobe_transport::records::CommandHeader;
use prost::Message;
use tokio::sync::{Notify, watch};
use tonic::{Code, Status};

use crate::config::Config;
use crate::events::{EventLog, Link, Lookup, Read, player_body, route_body};
use crate::idem::{self, Begin, Done, Idem};
use crate::idem::{EffectKind, Pending};
use crate::lease::{LeaseError, Leases};

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
    /// Load the campaign's mission on `theater` (plan 4.6, "Activate"). The
    /// fake loads its one mission; the shell writer of step S19 does it for
    /// a real DCS.
    fn load_mission(&mut self, theater: &str);
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
    /// vitals, ammunition, warehouse, enumeration (the agent's metrics,
    /// spec/transport.md); `None` when unknown.
    fn achieved(&self) -> Option<[f64; 5]> {
        None
    }
}

/// The sampling tiers by their `SampleRates` field names, in the order of
/// [`Backend::achieved`].
const TIERS: [&str; 5] = ["position", "vitals", "ammo", "warehouse", "enumeration"];

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
    /// The server store, with `[store]` on disk.
    pub server: Option<ServerStore>,
    /// The lock on the store dir, held while the hub lives.
    lock: Option<StoreLock>,
    /// `idempotency_keep_h`.
    idem_keep: Duration,
    /// Fault injection for tests: the next settle panics after the events
    /// commit and before the store commit, as a crash there would stop the
    /// sidecar.
    #[doc(hidden)]
    pub crash_after_events: bool,
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
        let source = (source.0 != 0).then_some(source);
        if let Some((boot, n)) = source {
            match log.has_source(boot, n) {
                Ok(false) => {}
                Ok(true) => return,
                Err(e) => panic!("the event log could not be read, stopping: {e}"),
            }
        }
        // Every event of one record goes to the same log: they share its
        // frame's campaign.
        for r in reads {
            log.push_from(campaign, r.stamp, r.body, source);
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
        self.backend.frame();
        let records = match (self.active, self.rec.as_mut()) {
            (Some(id), Some(rec)) => {
                let slot = self.campaigns.get_mut(&id).expect("active campaign");
                let mut tee = Tee {
                    inner: self.backend.sim(),
                    seen: Vec::new(),
                };
                rec.step_held(&mut slot.store, &mut tee);
                // Route progress made from the samples just read.
                let stamp = self.link.stamp();
                for e in slot.store.take_route_events() {
                    slot.events.push(id, stamp, route_body(&e));
                }
                tee.seen
            }
            _ => self.backend.sim().read(),
        };
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
            for n in notes {
                group.push(Read {
                    campaign: self.link.frame_campaign,
                    stamp: self.link.stamp(),
                    body: player_body(n),
                });
            }
            reads.push((group, (r.agent_boot, r.number)));
        }
        for (group, source) in reads {
            self.log(group, source);
        }
        // A pair learned in this step is written to the keyring file. A
        // failed write is tried again at the next step.
        let _ = self.roster.flush_keys();
        self.drive_players();
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
        self.timers(cfg);
        self.settle(None);
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

    /// After the commit that wrote a key in a campaign's store: its row in
    /// the server store's index.
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
        Some((d, done, age))
    }

    /// A key kept in campaign `c`'s store, opening it if it is not finished.
    fn stored_key(&mut self, c: u64, key: &str) -> Option<(Vec<u8>, Vec<u8>, u64)> {
        let slot = self.campaigns.get(&c)?;
        if slot.meta.state == pb::CampaignState::Finished as i32 {
            return None;
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
        for (id, r) in listed {
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
                    meta: meta_from(id.0, schema, &record, cp.as_ref()),
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

    /// The campaign a read names, active or not; 0 is the active one.
    pub fn resolve_read(&mut self, id: u64) -> Result<u64, Status> {
        self.resolve(id, Need::Campaign).or_else(|e| {
            // Reads accept a finished campaign.
            if nanoprobe_proto::errors::reason(&e) == ErrorReason::CampaignReadOnly {
                Ok(id)
            } else {
                Err(e)
            }
        })
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
        // The events first, then the stores (spec/persistence.md, "A crash
        // between the two commits").
        self.commit_events();
        if std::mem::take(&mut self.crash_after_events) {
            panic!("injected: a crash between the events commit and the store commit");
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
        self.backend.sim().flush();
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
            .map(|r| {
                [
                    r.position_s,
                    r.vitals_s,
                    r.ammo_s,
                    r.warehouse_s,
                    r.enumeration_s,
                ]
            })
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
            let achieved = self.achieved(id);
            let slot = self.campaigns.get_mut(&id).expect("active campaign");
            match slot.store.checkpoint(now, achieved) {
                Ok(c) => slot.meta.last_checkpoint = Some(checkpoint_info(&c)),
                Err(e) => panic!("campaign {id} could not be checkpointed, stopping: {e}"),
            }
            self.next_checkpoint_ms = now.saturating_add(ms(sf.checkpoint));
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

    /// A new campaign's ID (spec/world.md, "Identities"): the wall clock in
    /// milliseconds, or one more than the newest campaign, counting folders
    /// that could not be read.
    pub fn new_campaign_id(&self) -> u64 {
        let newest = self
            .campaigns
            .keys()
            .copied()
            .chain(self.unreadable.iter().map(|(id, _)| *id))
            .max()
            .unwrap_or(0);
        wall_ms().max(newest + 1)
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

    /// Make a campaign the active one, suspending the one before. The
    /// caller resolved `id`, which opened its store.
    pub fn activate(&mut self, cfg: &Config, id: u64) -> Result<(), Status> {
        if self.active == Some(id) {
            return Ok(());
        }
        if let Some(old) = self.active.take() {
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
        self.set_state(id, CampaignState::Active);
        let mut theater = String::new();
        if let Some(s) = self.campaigns.get_mut(&id) {
            s.meta.state = pb::CampaignState::Active as i32;
            theater = s.theater.clone();
        }
        self.active = Some(id);
        self.start_timers(cfg, id);
        self.restart(cfg);
        // A campaign is restored on the path of a mission load: the
        // reconciler learns the loaded theater from the load's LIFECYCLE.
        self.backend.load_mission(&theater);
        Ok(())
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
            self.send_no_campaign();
        }
        let state = if finished {
            CampaignState::Finished
        } else {
            CampaignState::Suspended
        };
        self.close_store(id, state, outcome.clone());
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
            "ammo" => rates.ammo = d,
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
                roster: Roster::new(match &cfg.store_dir {
                    // A keyring file that cannot be read stops the server: a
                    // new secret would give every player a new key.
                    Some(dir) => Keyring::open(dir).expect("the keyring file is readable"),
                    None => Keyring::generate(),
                }),
                moderation: Vec::new(),
                players_cmd: None,
                on_lost: LostPolicy::Report,
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
                server: None,
                lock: None,
                idem_keep: cfg.idempotency_keep,
                crash_after_events: false,
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
                g.leases
                    .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
                    .map_err(lease_status)?;
                let id = g.resolve(campaign, Need::Campaign)?;
                let Some(files) = g.files.clone() else {
                    return Ok(None);
                };
                if !g.backing_up.contains(&id) {
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
                                        inner.settle(Some(*acted));
                                        inner.index(key, placed);
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
        let c = settled.campaign;
        g.settle(Some(c));
        g.index(key, placed);
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
                                        inner.settle(Some(c));
                                        inner.index(key, placed);
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
                    g.settle(Some(c));
                    g.index(key, placed);
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

#[cfg(test)]
mod tests {
    use super::*;

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
