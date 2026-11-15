//! The state every service shares: campaigns and their stores, the
//! reconciler, the simulation backend, leases, idempotency keys and the
//! event logs; and the driver that steps them.
//!
//! One mutex guards it. No lock is held across an `.await`: a call takes the
//! lock, does its work and lets go. Streams wake on [`Hub::tick`], which the
//! driver and every successful change advance.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::{self as pb, ErrorReason, Health, event};
use nanoprobe_sidecar::ids::CampaignId;
use nanoprobe_sidecar::reconciler::{Config as RecConfig, EffectResult, Reconciler};
use nanoprobe_sidecar::sim::wire::{Body, Command, Input};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::store::{LostPolicy, Outcome, Store};
use nanoprobe_transport::records::CommandHeader;
use prost::Message;
use tokio::sync::{Notify, watch};
use tonic::{Code, Status};

use crate::config::Config;
use crate::events::{EventLog, Link, Read, route_body};
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
}

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
    /// The server's default lost policy (`[world] on_lost`).
    pub on_lost: LostPolicy,
    /// A `CAMPAIGN 0` is owed to the agent: no campaign is active and the
    /// last `HELLO` named one (spec/commands.md).
    clear_campaign: bool,
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
        self.sidecar_boot += 1;
        self.rec = self.active.map(|id| {
            let rc = self.rec_config(cfg, id);
            Reconciler::new(rc, &self.campaigns[&id].store)
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

    /// Put an event read from the agent in its log: the campaign of the
    /// frame it was made in when the server holds that campaign, otherwise
    /// the server log.
    fn log(&mut self, r: Read) {
        match self.campaigns.get_mut(&r.campaign) {
            Some(slot) if r.campaign != 0 => {
                slot.events.push(r.campaign, r.stamp, r.body);
            }
            _ => {
                self.server_log.push(0, r.stamp, r.body);
            }
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

    /// One driver step: a frame, then every record read and folded in.
    fn step(&mut self) {
        self.backend.frame();
        let records = match (self.active, self.rec.as_mut()) {
            (Some(id), Some(rec)) => {
                let slot = self.campaigns.get_mut(&id).expect("active campaign");
                let mut tee = Tee {
                    inner: self.backend.sim(),
                    seen: Vec::new(),
                };
                rec.step(&mut slot.store, &mut tee);
                // Route progress made from the samples just read.
                let stamp = self.link.stamp();
                for e in slot.store.take_route_events() {
                    slot.events.push(id, stamp, route_body(&e));
                }
                tee.seen
            }
            _ => self.backend.sim().read(),
        };
        for r in &records {
            if self.active.is_none()
                && let Ok(Input::Hello(h)) = Input::decode(r.kind, &r.payload)
            {
                self.clear_campaign = h.campaign != 0;
            }
            if let Some(read) = self.link.read(r) {
                self.log(read);
            }
        }
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
    }

    /// The campaign a call names: 0 resolves to the active one.
    pub fn resolve(&self, id: u64, need: Need) -> Result<u64, Status> {
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
        if need != Need::Nothing && slot.meta.state == pb::CampaignState::Finished as i32 {
            return Err(status(
                Code::FailedPrecondition,
                ErrorReason::CampaignReadOnly,
                format!("campaign {id} is finished"),
                0,
                "campaign_id",
            ));
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
    pub fn resolve_read(&self, id: u64) -> Result<u64, Status> {
        self.resolve(id, Need::Campaign).or_else(|e| {
            // Reads accept a finished campaign.
            if nanoprobe_proto::errors::reason(&e) == ErrorReason::CampaignReadOnly {
                Ok(id)
            } else {
                Err(e)
            }
        })
    }

    /// A new campaign's ID (spec/world.md, "Identities"): the wall clock in
    /// milliseconds, or one more than the newest.
    pub fn new_campaign_id(&self) -> u64 {
        let newest = self.campaigns.keys().next_back().copied().unwrap_or(0);
        wall_ms().max(newest + 1)
    }

    /// Add a campaign, suspended, to be loaded on `theater`.
    pub fn add_campaign(&mut self, cfg: &Config, meta: pb::Campaign, theater: String) {
        let id = meta.id;
        self.campaigns.insert(
            id,
            Slot {
                meta,
                store: Store::new(CampaignId(id), cfg.world_changes_keep),
                events: EventLog::new(cfg.events_keep),
                on_lost: self.on_lost,
                theater,
            },
        );
    }

    /// Make a campaign the active one, suspending the one before.
    pub fn activate(&mut self, cfg: &Config, id: u64) {
        if self.active == Some(id) {
            return;
        }
        if let Some(old) = self.active.take()
            && let Some(s) = self.campaigns.get_mut(&old)
        {
            s.meta.state = pb::CampaignState::Suspended as i32;
            s.meta.loaded_theater.clear();
        }
        let mut theater = String::new();
        if let Some(s) = self.campaigns.get_mut(&id) {
            s.meta.state = pb::CampaignState::Active as i32;
            theater = s.theater.clone();
        }
        self.active = Some(id);
        self.restart(cfg);
        // A campaign is restored on the path of a mission load: the
        // reconciler learns the loaded theater from the load's LIFECYCLE.
        self.backend.load_mission(&theater);
    }

    /// Suspend a campaign. DCS keeps running whatever it holds, unmanaged.
    pub fn suspend(&mut self, cfg: &Config, id: u64) {
        if let Some(s) = self.campaigns.get_mut(&id) {
            s.meta.state = pb::CampaignState::Suspended as i32;
            s.meta.loaded_theater.clear();
        }
        if self.active == Some(id) {
            self.active = None;
            self.restart(cfg);
        }
    }
}

impl Hub {
    /// A hub over a backend. Nothing is attached until [`Hub::start`].
    pub fn new(cfg: Config, backend: Box<dyn Backend>) -> Arc<Hub> {
        let (tick, _) = watch::channel(0);
        Arc::new(Hub {
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
                on_lost: LostPolicy::Report,
                clear_campaign: false,
            }),
            tick,
            settled: Notify::new(),
            closing: std::sync::atomic::AtomicBool::new(false),
            poisoned: std::sync::atomic::AtomicBool::new(false),
            cfg,
        })
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
        catch_unwind(AssertUnwindSafe(|| f(inner))).unwrap_or_else(|_| {
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
            catch_unwind(AssertUnwindSafe(|| g.step()))
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
                        let result = g
                            .leases
                            .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
                            .map_err(lease_status)
                            .and_then(|()| g.resolve(campaign, need))
                            .and_then(|c| {
                                let f = f.take().expect("a call runs once");
                                let inner: &mut Inner = &mut g;
                                catch_unwind(AssertUnwindSafe(|| f(inner, c))).map_err(|_| {
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
                        let result = g
                            .leases
                            .check(now, lease.map(|l| (l.epoch, l.lease_id.as_str())))
                            .map_err(lease_status)
                            .and_then(|()| g.resolve(campaign, need))
                            .and_then(|c| {
                                let start = start.take().expect("a call runs once");
                                let inner: &mut Inner = &mut g;
                                catch_unwind(AssertUnwindSafe(|| start(inner, c)))
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
                    g.idem.finish(
                        Instant::now(),
                        key,
                        Done {
                            method,
                            response,
                            campaign: c,
                            pending,
                        },
                    );
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
