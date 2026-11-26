//! The event log (plan 4.7; spec/world.md, "Events and the store") and what
//! the server reads from the agent's records besides the reconciler.
//!
//! Each campaign has a log numbered from 1, and so does the server log, which
//! holds events recorded while no campaign is active. A log is never
//! conflated: a consumer reads every event in order from a sequence number,
//! and a lost span is an explicit `Gap`. A log on disk is backed by an
//! events file (spec/persistence.md, "Event history"): the driver commits
//! the events of a step before it publishes them. A log keeps its newest
//! events in memory and reads older ones from the file.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use nanoprobe_proto::v1alpha1::{
    self as pb, AuthorEvent, Event, Gap as PbGap, LifecycleEvent, MenuAttribution, MenuEvent,
    PlayerEvent, PlayerEventKind, SimState, WorldEvent, event,
};
use nanoprobe_sidecar::eventfile::{EventFile, EventRow, Retained};
use nanoprobe_sidecar::players::{LaunchRule, Note, Roster};
use nanoprobe_sidecar::retention;
use nanoprobe_sidecar::sim::RawRecord;
use nanoprobe_sidecar::sim::wire::Input;
use nanoprobe_sidecar::store::{DiskError, Loadout};
use nanoprobe_transport::layout::{gap_class, lifecycle as lc};
use nanoprobe_transport::records;

use prost::Message;

use crate::lua::from_lua;

/// The most events a log on disk keeps in memory.
pub const DISK_WINDOW: usize = 10_000;

/// One numbered log.
#[derive(Debug)]
pub struct EventLog {
    keep: usize,
    next: u64,
    events: VecDeque<Event>,
    /// The agent boot each event in `events` was made in, 0 when unknown.
    /// Kept beside the events: the public stamp does not carry it.
    boots: VecDeque<u64>,
    file: Option<EventFile>,
    /// The encoded bytes of the events in `events`, kept for a log without
    /// a file: the count the size limit reads. 0 for a log on disk, whose
    /// file keeps its own.
    bytes: u64,
}

fn wall_ms_of(e: &Event) -> u64 {
    e.stamp
        .as_ref()
        .and_then(|s| s.wall)
        .map_or_else(now_ms, |t| {
            u64::try_from(t.seconds)
                .unwrap_or(0)
                .saturating_mul(1000)
                .saturating_add(u64::try_from(t.nanos / 1_000_000).unwrap_or(0))
        })
}

/// The agent boot of an event's source key, `<boot>:<record>[:<i>]`; 0 for
/// no key.
fn boot_of_source(source: Option<&str>) -> u64 {
    source
        .and_then(|s| s.split(':').next())
        .and_then(|b| b.parse().ok())
        .unwrap_or(0)
}

/// An event as the file holds it. One made from an agent record takes the
/// boot of its source key, so a body stored before stamps carried the boot
/// reads it too; one without a key keeps what its body holds, 0 if nothing.
fn decode(r: &EventRow) -> Result<Event, DiskError> {
    let mut e = Event::decode(r.body.as_slice())
        .map_err(|e| DiskError::Corrupt(format!("event {} does not decode: {e}", r.seq)))?;
    let boot = boot_of_source(r.source.as_deref());
    if boot != 0 {
        e.stamp.get_or_insert_with(pb::Stamp::default).agent_boot = boot;
    }
    Ok(e)
}

impl EventLog {
    /// An empty log in memory keeping the newest `keep` events.
    pub fn new(keep: usize) -> EventLog {
        EventLog {
            keep: keep.max(1),
            next: 1,
            events: VecDeque::new(),
            boots: VecDeque::new(),
            file: None,
            bytes: 0,
        }
    }

    /// A log backed by `file`, keeping its newest `keep` events, at most
    /// [`DISK_WINDOW`], in memory.
    pub fn open(file: EventFile, keep: usize) -> Result<EventLog, DiskError> {
        let keep = keep.clamp(1, DISK_WINDOW);
        let next = file.next_seq();
        let from = next.saturating_sub(keep as u64).max(1);
        let mut events = VecDeque::new();
        let mut boots = VecDeque::new();
        for r in file.read_from(from, keep)? {
            events.push_back(decode(&r)?);
            boots.push_back(boot_of_source(r.source.as_deref()));
        }
        Ok(EventLog {
            keep,
            next,
            events,
            boots,
            file: Some(file),
            bytes: 0,
        })
    }

    /// Append an event, numbering it, made from the agent record `source` if
    /// any. Returns its sequence number. A log on disk writes it at the next
    /// [`EventLog::commit`], before it is published.
    pub fn push_from(
        &mut self,
        campaign: u64,
        stamp: pb::Stamp,
        body: event::Body,
        source: Option<String>,
    ) -> u64 {
        let sequence = self.next;
        let boot = boot_of_source(source.as_deref());
        let mut stamp = stamp;
        if boot != 0 {
            // An event from an agent record carries the boot it was made
            // in, whatever agent runs now (spec/records.md, "Stamps").
            stamp.agent_boot = boot;
        }
        let e = Event {
            sequence,
            stamp: Some(stamp),
            body: Some(body),
            campaign_id: campaign,
        };
        self.boots.push_back(boot);
        if let Some(f) = self.file.as_mut() {
            f.push(EventRow {
                seq: sequence,
                wall_ms: wall_ms_of(&e),
                source,
                body: e.encode_to_vec(),
            });
        }
        self.next += 1;
        if self.file.is_none() {
            self.bytes += e.encoded_len() as u64;
        }
        self.events.push_back(e);
        while self.events.len() > self.keep {
            self.pop_oldest();
        }
        sequence
    }

    /// [`EventLog::push_from`] for an event the sidecar made.
    pub fn push(&mut self, campaign: u64, stamp: pb::Stamp, body: event::Body) -> u64 {
        self.push_from(campaign, stamp, body, None)
    }

    /// Whether an event made from `source` is in the log already.
    pub fn has_source(&self, source: &str) -> Result<bool, DiskError> {
        match self.file.as_ref() {
            Some(f) => f.has_source(source),
            None => Ok(false),
        }
    }

    /// Write the events appended since the last commit.
    pub fn commit(&mut self) -> Result<(), DiskError> {
        match self.file.as_mut() {
            Some(f) => f.commit(),
            None => Ok(()),
        }
    }

    /// Drop the oldest event from memory.
    fn pop_oldest(&mut self) {
        self.boots.pop_front();
        if let Some(e) = self.events.pop_front()
            && self.file.is_none()
        {
            self.bytes = self.bytes.saturating_sub(e.encoded_len() as u64);
        }
    }

    /// Apply the retention (spec/persistence.md, "Event history"): the time
    /// limit `retention` (zero is none) and the size limit `max_bytes` (zero
    /// is none), oldest events first. A log on disk deletes at most
    /// `max_rows` rows from its file and drops from memory what the file
    /// deleted; [`Retained::Deleted`] says whether more may remain. A log in
    /// memory applies both limits to every event, by its stamp's wall time
    /// and its encoded size, and ignores `max_rows`. Either way the newest
    /// event stays, and the time limit is skipped when the clock is behind
    /// it.
    pub fn retain(
        &mut self,
        retention: Duration,
        now_ms: u64,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<Retained, DiskError> {
        let retention_ms = u64::try_from(retention.as_millis()).unwrap_or(u64::MAX);
        let Some(f) = self.file.as_mut() else {
            return Ok(self.retain_memory(retention_ms, now_ms, max_bytes));
        };
        let r = f.retain(retention_ms, now_ms, max_rows, max_bytes)?;
        let first = self.first();
        while self.events.front().is_some_and(|e| e.sequence < first) {
            self.pop_oldest();
        }
        Ok(r)
    }

    /// [`EventLog::retain`] for a log without a file.
    fn retain_memory(&mut self, retention_ms: u64, now_ms: u64, max_bytes: u64) -> Retained {
        /// Events looked at in one call of the shared rule.
        const CHUNK: usize = 4_096;
        let Some(newest_ms) = self.events.back().map(wall_ms_of) else {
            return Retained::Kept;
        };
        if retention_ms == 0 && max_bytes == 0 {
            return Retained::Kept;
        }
        let behind = retention_ms != 0 && now_ms < newest_ms;
        let cutoff = (retention_ms != 0 && !behind)
            .then(|| now_ms.min(newest_ms).saturating_sub(retention_ms));
        let newest_seq = self.last();
        let mut excess = if max_bytes == 0 {
            0
        } else {
            self.bytes.saturating_sub(max_bytes)
        };
        let mut n = 0;
        loop {
            let batch: Vec<_> = self
                .events
                .iter()
                .take(CHUNK)
                .map(|e| (e.sequence, wall_ms_of(e), e.encoded_len() as u64))
                .collect();
            let cut = retention::cut(&batch, CHUNK, newest_seq, cutoff, excess);
            for _ in 0..cut.rows {
                self.pop_oldest();
            }
            n += cut.rows;
            excess = excess.saturating_sub(cut.bytes);
            if !cut.more {
                break;
            }
        }
        if n == 0 {
            if behind {
                Retained::ClockBehind { now_ms, newest_ms }
            } else {
                Retained::Kept
            }
        } else {
            Retained::Deleted { n, more: false }
        }
    }

    /// The newest sequence number, 0 for none yet.
    pub fn last(&self) -> u64 {
        self.next - 1
    }

    /// The oldest sequence number kept, or the next one when empty.
    pub fn first(&self) -> u64 {
        match self.file.as_ref() {
            Some(f) => f.first_seq().unwrap_or(self.next),
            None => self.events.front().map_or(self.next, |e| e.sequence),
        }
    }

    /// The events held, in memory or in the file.
    pub fn rows(&self) -> u64 {
        self.next - self.first()
    }

    /// The bytes of the events' bodies kept, as `events_max_mb` counts them.
    pub fn bytes(&self) -> u64 {
        match self.file.as_ref() {
            Some(f) => f.bytes(),
            None => self.bytes,
        }
    }

    /// The wall-clock time of the oldest event held, milliseconds since the
    /// Unix epoch; `None` for none.
    pub fn oldest_ms(&self) -> Option<u64> {
        match self.file.as_ref() {
            Some(f) => f.first_wall_ms(),
            None => self.events.front().map(wall_ms_of),
        }
    }

    /// Whether a reader that has every event up to `after` can go on from
    /// here: the events after it are all kept, and `after` is not ahead of
    /// the newest.
    pub fn covers(&self, after: u64) -> bool {
        after <= self.last() && after + 1 >= self.first()
    }

    /// The event numbered `sequence` and the agent boot it was made in (0
    /// when unknown), if the log still holds it. The boot is the one in the
    /// event's source key (spec/persistence.md, "Event history").
    pub fn get(&self, sequence: u64) -> Option<(Event, u64)> {
        if sequence == 0 {
            return None;
        }
        if let Some(i) = self
            .events
            .front()
            .and_then(|f| sequence.checked_sub(f.sequence))
            && let Some(e) = self.events.get(i as usize)
        {
            return Some((e.clone(), self.boots.get(i as usize).copied().unwrap_or(0)));
        }
        let f = self.file.as_ref()?;
        let row = f.read_from(sequence, 1).ok()?.into_iter().next()?;
        if row.seq != sequence {
            return None;
        }
        Some((decode(&row).ok()?, boot_of_source(row.source.as_deref())))
    }

    /// Up to `limit` events after `after`, in order: from memory, or from
    /// the file for those older than memory holds. A read of the file that
    /// fails returns what memory holds from there.
    pub fn after(&self, after: u64, limit: usize) -> Vec<Event> {
        let mem_first = self.events.front().map_or(self.next, |e| e.sequence);
        let mut out = Vec::new();
        if after + 1 < mem_first
            && let Some(f) = self.file.as_ref()
        {
            let want = usize::try_from(mem_first - (after + 1)).unwrap_or(usize::MAX);
            if let Ok(rows) = f.read_from(after + 1, want.min(limit)) {
                for r in rows {
                    match decode(&r) {
                        Ok(e) => out.push(e),
                        Err(_) => return out,
                    }
                }
            }
            if out.len() < want.min(limit) {
                // The file lost rows the reader needs: stop, and let the
                // reader find the gap.
                return out;
            }
        }
        let from = out.last().map_or(after, |e| e.sequence);
        let skip = usize::try_from((from + 1).saturating_sub(mem_first)).unwrap_or(usize::MAX);
        out.extend(
            self.events
                .iter()
                .skip(skip)
                .take(limit.saturating_sub(out.len()))
                .cloned(),
        );
        out
    }
}

/// What the server knows of the agent from its records.
#[derive(Clone, Debug, Default)]
pub struct Link {
    /// A `HELLO` was read from the agent boot now running.
    pub attached: bool,
    /// The agent boot of the last `HELLO`.
    pub agent_boot: u64,
    /// The agent boot that wrote the last record read. A new DCS process's
    /// first `FRAME` and queued records come before its `HELLO`.
    record_boot: u64,
    /// The DCS version the agent reported.
    pub dcs_version: String,
    /// The install's build the agent reported.
    pub agent_build: u64,
    /// The agent DLL's interface version (the DLL-to-shim ABI), reported and
    /// not checked (spec/transport.md, "Layout version").
    pub agent_interface: u32,
    /// `GAP` records of events read.
    pub event_gaps: u64,
    /// The last lifecycle emitted as an event: state, generation, theater.
    emitted: Option<(u32, u32, String)>,
    /// Real time of the last `FRAME`, seconds.
    pub real_time: f64,
    /// The agent's lifecycle code, 0 before the first.
    pub lifecycle: u32,
    /// The loaded mission's theater.
    pub theater: String,
    /// Stamps of the last `FRAME` read.
    pub frame: u64,
    /// Model time of the last `FRAME`.
    pub model_time: f64,
    /// Mission generation of the last `FRAME`.
    pub mission_gen: u32,
    /// Campaign of the last `FRAME`, 0 for none.
    pub frame_campaign: u64,
    /// Wall-clock minus the agent's real time, from the last live `FRAME`.
    pub offset: Option<f64>,
    /// When the last record was read.
    pub last_record: Option<SystemTime>,
    /// The latest `EXTENSION` record of each extension, by name, from the
    /// agent boot now running.
    pub extensions: std::collections::BTreeMap<String, records::ExtensionStats>,
}

/// What the server knows of the objects and players a record names.
pub struct Lookup<'a> {
    /// The unit `EntityId` a DCS object ID is bound to.
    pub unit: &'a dyn Fn(u64) -> Option<u64>,
    /// Who is in which object, and who is connected.
    pub roster: Option<&'a Roster>,
    /// Whether the platform issued a DCS mark ID in an agent boot:
    /// `(boot, mark_id)`.
    pub issued: &'a dyn Fn(u64, u64) -> bool,
}

/// The API's player event for what the roster saw.
pub fn player_body(n: Note) -> event::Body {
    let (kind, key, name, side, unit, seat, text, from) = match n {
        Note::Connect { key, name, side } => (
            PlayerEventKind::Connect,
            key,
            name,
            side,
            None,
            None,
            None,
            None,
        ),
        Note::Disconnect { key, name, side } => (
            PlayerEventKind::Disconnect,
            key,
            name,
            side,
            None,
            None,
            None,
            None,
        ),
        Note::Enter {
            key,
            name,
            side,
            unit,
            seat,
        } => (
            PlayerEventKind::EnterUnit,
            key,
            name,
            side,
            Some(unit).filter(|u| *u != 0),
            Some(seat),
            None,
            None,
        ),
        Note::Leave {
            key,
            name,
            side,
            unit,
            seat,
            ..
        } => (
            PlayerEventKind::LeaveUnit,
            key,
            name,
            side,
            Some(unit).filter(|u| *u != 0),
            Some(seat),
            None,
            None,
        ),
        Note::Chat {
            key,
            name,
            side,
            text,
        } => (
            PlayerEventKind::Chat,
            key,
            name,
            side,
            None,
            None,
            Some(text),
            None,
        ),
        Note::Rename {
            key,
            name,
            side,
            from,
        } => (
            PlayerEventKind::Rename,
            key,
            name,
            side,
            None,
            None,
            None,
            Some(from),
        ),
    };
    // A rename carries the name before it in `text`.
    event::Body::Player(PlayerEvent {
        kind: kind as i32,
        player_key: key,
        name,
        unit_id: unit,
        seat,
        text: text.or(from),
        coalition: crate::convert::coalition_of_dcs(side) as i32,
    })
}

/// An event read from the agent, before it is put in a log.
#[derive(Clone, Debug, PartialEq)]
pub struct Read {
    /// The campaign of the frame it was made in, 0 for none.
    pub campaign: u64,
    /// Its stamp.
    pub stamp: pb::Stamp,
    /// The event.
    pub body: event::Body,
}

/// A wall-clock time from seconds since the Unix epoch. A value from the
/// agent that is not finite or not in range is no time at all.
fn wall(seconds: f64) -> Option<prost_types::Timestamp> {
    let d = Duration::try_from_secs_f64(seconds).ok()?;
    Some(prost_types::Timestamp {
        seconds: i64::try_from(d.as_secs()).ok()?,
        nanos: i32::try_from(d.subsec_nanos()).ok()?,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The API's state for an agent `lifecycle` code.
pub fn sim_state(code: u32) -> SimState {
    match u64::from(code) {
        lc::STARTING => SimState::Starting,
        lc::LOADING => SimState::Loading,
        lc::RUNNING => SimState::Running,
        lc::PAUSED => SimState::Paused,
        lc::STOPPING => SimState::Stopping,
        lc::DISABLED => SimState::Disabled,
        _ => SimState::Unspecified,
    }
}

impl Link {
    /// The stamp of the last frame read. Its wall-clock time is the agent's
    /// real time plus the measured offset (spec/records.md, "Stamps"), so a
    /// replayed frame keeps the time it was made; before the first live
    /// frame it is now.
    pub fn stamp(&self) -> pb::Stamp {
        let at = match self.offset {
            Some(o) => o + self.real_time,
            None => now_s(),
        };
        crate::convert::stamp(
            self.boot(),
            self.mission_gen,
            self.frame,
            self.model_time,
            wall(at),
        )
    }

    /// The agent boot of what is read now: that of the last record while
    /// attached, else 0 (none known).
    fn boot(&self) -> u64 {
        if self.attached { self.record_boot } else { 0 }
    }

    /// Fold one record in. Returns the event it carries, if any. An
    /// `AUTHOR_EVENT` names no entity.
    pub fn read(&mut self, r: &RawRecord) -> Option<Read> {
        self.read_with(
            r,
            &Lookup {
                unit: &|_| None,
                roster: None,
                issued: &|_, _| false,
            },
        )
    }

    /// Fold one record in, with `entity` giving the unit a DCS object ID is
    /// bound to. An `AUTHOR_EVENT` names the units whose objects it has.
    pub fn read_bound(
        &mut self,
        r: &RawRecord,
        entity: &dyn Fn(u64) -> Option<u64>,
    ) -> Option<Read> {
        self.read_with(
            r,
            &Lookup {
                unit: entity,
                roster: None,
                issued: &|_, _| false,
            },
        )
    }

    /// Fold one record in, with what the server knows of the objects and
    /// the players in them. An `EVENT` names the players in its initiator
    /// and target, an `AUTHOR_EVENT` the players in its objects, a `MENU`
    /// the players who could have chosen the item.
    pub fn read_with(&mut self, r: &RawRecord, look: &Lookup<'_>) -> Option<Read> {
        let entity = look.unit;
        self.last_record = Some(SystemTime::now());
        self.record_boot = r.agent_boot;
        let input = Input::decode(r.kind, &r.payload).ok()?;
        match input {
            Input::Frame(f) => {
                self.frame = f.frame;
                self.model_time = f.model_time;
                self.mission_gen = f.mission_gen;
                self.frame_campaign = f.campaign;
                self.real_time = f.real_time;
                if f.replay == 0 && f.real_time.is_finite() {
                    self.offset = Some(now_s() - f.real_time);
                }
                None
            }
            Input::Hello(h) => {
                if h.agent_boot != self.agent_boot {
                    // A new DCS process: its first lifecycle is news.
                    self.emitted = None;
                }
                if h.agent_boot != self.agent_boot {
                    self.extensions.clear();
                }
                self.attached = true;
                self.agent_boot = h.agent_boot;
                self.agent_build = h.build;
                self.agent_interface = h.interface;
                self.dcs_version = h.dcs_version;
                None
            }
            Input::Lifecycle(l) => {
                self.lifecycle = l.state;
                self.mission_gen = l.mission_gen;
                self.theater = l.theater.clone();
                // The agent repeats its state after every HELLO; only a
                // change is an event.
                let now = (l.state, l.mission_gen, l.theater.clone());
                if self.emitted.as_ref() == Some(&now) {
                    return None;
                }
                self.emitted = Some(now);
                Some(Read {
                    campaign: self.frame_campaign,
                    stamp: self.stamp(),
                    body: event::Body::Lifecycle(LifecycleEvent {
                        state: sim_state(l.state) as i32,
                        theater: l.theater,
                    }),
                })
            }
            Input::Gap(g) if u64::from(g.class) == gap_class::EVENTS => {
                self.event_gaps += 1;
                let end = |real: f64, model: f64, gen_: u32| {
                    crate::convert::stamp(
                        self.boot(),
                        gen_,
                        0,
                        model,
                        self.offset.and_then(|o| wall(o + real)),
                    )
                };
                Some(Read {
                    campaign: self.frame_campaign,
                    stamp: self.stamp(),
                    body: event::Body::Gap(PbGap {
                        from: Some(end(g.from_real, g.from_model, g.from_gen)),
                        to: Some(end(g.to_real, g.to_model, g.to_gen)),
                        lost: g.count,
                    }),
                })
            }
            Input::Event(e) => Some(Read {
                campaign: self.frame_campaign,
                stamp: self.stamp(),
                body: event::Body::World(self.world_event(&e, look)),
            }),
            Input::AuthorEvent(a) => {
                let mut player_keys: Vec<String> = Vec::new();
                if let Some(roster) = look.roster {
                    for o in &a.objects {
                        for k in roster.players_of(*o) {
                            if !player_keys.contains(&k) {
                                player_keys.push(k);
                            }
                        }
                    }
                }
                Some(Read {
                    campaign: self.frame_campaign,
                    stamp: self.stamp(),
                    body: event::Body::Author(AuthorEvent {
                        name: a.name,
                        payload: Some(from_lua(&a.payload)),
                        player_keys,
                        entity_ids: a.objects.iter().filter_map(|o| entity(*o)).collect(),
                    }),
                })
            }
            Input::Menu(m) => {
                let (attribution, player_keys) = match look.roster {
                    Some(roster) if m.player != 0 => match roster.selector(&m) {
                        Some(k) => (MenuAttribution::Selector, vec![k]),
                        // DCS named a connection the server has no player on:
                        // no one is named.
                        None => (MenuAttribution::Selector, Vec::new()),
                    },
                    Some(roster) => (MenuAttribution::Audience, roster.candidates(&m)),
                    None if m.player != 0 => (MenuAttribution::Selector, Vec::new()),
                    None => (MenuAttribution::Audience, Vec::new()),
                };
                Some(Read {
                    campaign: self.frame_campaign,
                    stamp: self.stamp(),
                    body: event::Body::Menu(MenuEvent {
                        item_id: m.entity,
                        player_keys,
                        attribution: attribution as i32,
                    }),
                })
            }
            Input::Extension(e) => {
                self.extensions.insert(e.name.clone(), e);
                None
            }
            // A player record becomes player events through the roster
            // (`player_body`).
            _ => None,
        }
    }
}

/// `event_field` codes (spec/records.md, "Codes: event_field").
mod field {
    pub const TIME: u64 = 1;
    pub const INITIATOR: u64 = 2;
    pub const TARGET: u64 = 3;
    pub const WEAPON_NAME: u64 = 5;
    pub const SUBPLACE: u64 = 7;
    pub const TEXT: u64 = 8;
    pub const MARK_ID: u64 = 9;
    pub const POS_X: u64 = 10;
    pub const POS_Y: u64 = 11;
    pub const POS_Z: u64 = 12;
    pub const COALITION: u64 = 13;
    pub const COMMENT: u64 = 15;
    pub const ROUNDS: u64 = 16;
    pub const INITIATOR_CATEGORY: u64 = 17;
    pub const TARGET_CATEGORY: u64 = 18;
    pub const INITIATOR_TYPE: u64 = 19;
    pub const TARGET_TYPE: u64 = 20;
}

/// Fields shown in `WorldEvent.extra`, by name. The others hold DCS object
/// IDs, which the API never shows: the initiator and target become entities
/// and player keys.
const EXTRA: [(u64, &str); 6] = [
    (field::TIME, "time"),
    (field::SUBPLACE, "subplace"),
    (field::COALITION, "coalition"),
    (field::COMMENT, "comment"),
    (field::INITIATOR_CATEGORY, "initiator_category"),
    (field::TARGET_CATEGORY, "target_category"),
];

impl Link {
    /// The API's world event for an `EVENT` record. Every field is optional:
    /// a value of the wrong type is left out.
    fn world_event(&self, e: &records::Event, look: &Lookup<'_>) -> WorldEvent {
        let object = |code: u64| e.number(code).filter(|n| *n >= 0.0).map(|n| n as u64);
        let (initiator, target) = (object(field::INITIATOR), object(field::TARGET));
        let attr = look.roster.map(|r| r.attribute(e)).unwrap_or_default();
        let position = match (
            e.number(field::POS_X),
            e.number(field::POS_Y),
            e.number(field::POS_Z),
        ) {
            (Some(x), Some(y), Some(z)) if x.is_finite() && y.is_finite() && z.is_finite() => {
                Some(pb::Position {
                    theater: self.theater.clone(),
                    north: x,
                    east: z,
                    alt: y,
                })
            }
            _ => None,
        };
        let fields: std::collections::BTreeMap<String, prost_types::Value> = EXTRA
            .iter()
            .filter_map(|(code, name)| e.get(*code).map(|v| ((*name).to_owned(), from_lua(v))))
            .collect();
        let mut w = WorldEvent {
            dcs_id: e.id,
            initiator_id: initiator
                .and_then(|o| (look.unit)(o))
                .or(attr.initiator_entity),
            target_id: target.and_then(|o| (look.unit)(o)).or(attr.target_entity),
            initiator_players: attr.initiator_players,
            target_players: attr.target_players,
            initiator_type: e.text(field::INITIATOR_TYPE).or(attr.initiator_type),
            target_type: e.text(field::TARGET_TYPE).or(attr.target_type),
            dated_time: attr.dated_time,
            launch_players: attr.launch_players,
            launch_rule: match attr.launch_rule {
                Some(LaunchRule::Weapon) => pb::LaunchRule::Weapon as i32,
                Some(LaunchRule::Initiator) => pb::LaunchRule::Initiator as i32,
                None => pb::LaunchRule::Unspecified as i32,
            },
            weapon: e.text(field::WEAPON_NAME),
            position,
            text: e.text(field::TEXT),
            rounds: e
                .number(field::ROUNDS)
                .filter(|n| n.is_finite() && *n >= 0.0 && *n <= f64::from(u32::MAX))
                .map(|n| n as u32),
            extra: (!fields.is_empty()).then_some(prost_types::Struct { fields }),
            // The mark events: added, changed and removed (25 to 27).
            mark_id: e
                .number(field::MARK_ID)
                .filter(|n| {
                    (25..=27).contains(&e.id)
                        && n.is_finite()
                        && *n >= 0.0
                        && *n < 9.007_199_254_740_992e15
                })
                .map(|n| n as u64),
            ..WorldEvent::default()
        };
        w.platform_mark = w
            .mark_id
            .is_some_and(|id| (look.issued)(self.agent_boot, id));
        w
    }
}

/// The event body for a route event the store made.
pub fn route_body(e: &nanoprobe_sidecar::store::RouteEvent) -> event::Body {
    use nanoprobe_proto::v1alpha1::{RouteEvent, RouteEventKind};
    let (kind, waypoint_index) = match e.index {
        Some(i) => (
            RouteEventKind::WaypointReached,
            Some(u32::try_from(i).unwrap_or(u32::MAX)),
        ),
        None => (RouteEventKind::RouteComplete, None),
    };
    event::Body::Route(RouteEvent {
        kind: kind as i32,
        group_id: e.group.0,
        order_revision: e.order_rev,
        waypoint_index,
        model_time: e.model_time,
        observed: e.observed,
    })
}

/// The DCS event ID of `S_EVENT_TAKEOFF`.
pub const S_EVENT_TAKEOFF: u32 = 3;
/// The DCS event ID of `S_EVENT_BIRTH`.
pub const S_EVENT_BIRTH: u32 = 15;

/// The API's loadout event for an `AMMO` read of an occupied unit.
pub fn loadout_body(
    unit_id: u64,
    group_id: u64,
    type_name: &str,
    player_keys: Vec<String>,
    l: &Loadout,
) -> event::Body {
    event::Body::Loadout(pb::LoadoutEvent {
        unit_id,
        group_id,
        r#type: type_name.to_owned(),
        player_keys,
        ammo: l.ammo.clone().into_iter().collect(),
        read: Some(crate::convert::ammo_read(l)),
    })
}

/// Whether an event passes a filter's DCS IDs and player key. Gap and
/// lifecycle events pass every filter.
pub fn passes(e: &Event, dcs_ids: &[u32], player_key: &str) -> bool {
    match &e.body {
        Some(event::Body::Gap(_) | event::Body::Lifecycle(_)) | None => true,
        Some(event::Body::World(w)) => {
            (dcs_ids.is_empty() || dcs_ids.contains(&w.dcs_id))
                && (player_key.is_empty()
                    || w.initiator_players.iter().any(|p| p == player_key)
                    || w.target_players.iter().any(|p| p == player_key))
        }
        Some(event::Body::Player(p)) => {
            dcs_ids.is_empty() && (player_key.is_empty() || p.player_key == player_key)
        }
        Some(event::Body::Author(a)) => {
            dcs_ids.is_empty()
                && (player_key.is_empty() || a.player_keys.iter().any(|k| k == player_key))
        }
        // A loadout read passes the filter of the event that caused it.
        Some(event::Body::Loadout(l)) => {
            let cause = match l.read.as_ref().map(|r| r.cause) {
                Some(c) if c == pb::AmmoReadCause::Birth as i32 => Some(S_EVENT_BIRTH),
                Some(c) if c == pb::AmmoReadCause::Takeoff as i32 => Some(S_EVENT_TAKEOFF),
                _ => None,
            };
            (dcs_ids.is_empty() || cause.is_some_and(|c| dcs_ids.contains(&c)))
                && (player_key.is_empty() || l.player_keys.iter().any(|k| k == player_key))
        }
        Some(event::Body::Route(_)) => dcs_ids.is_empty() && player_key.is_empty(),
        Some(event::Body::Moderation(m)) => {
            dcs_ids.is_empty() && (player_key.is_empty() || m.player_key == player_key)
        }
        Some(event::Body::Menu(m)) => {
            dcs_ids.is_empty()
                && (player_key.is_empty() || m.player_keys.iter().any(|k| k == player_key))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An event body as the events file keeps it, pinned in
    /// spec/persistence.golden next to the store's rows.
    #[test]
    fn an_event_body_encodes_as_the_golden_file_says() {
        const GOLDEN: &str = include_str!("../../../spec/persistence.golden");
        let e = Event {
            sequence: 3,
            // Boot 0 is not encoded: the golden body predates the field.
            stamp: Some(crate::convert::stamp(
                0,
                2,
                77,
                12.5,
                Some(prost_types::Timestamp {
                    seconds: 1_700_000_000,
                    nanos: 5_000_000,
                }),
            )),
            body: Some(body(9)),
            campaign_id: 7,
        };
        let hex: String = e
            .encode_to_vec()
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        let line = format!("event.world {hex}\n");
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/persistence.golden");
        if std::env::var_os("NANOPROBE_BLESS").is_some() {
            let others: String = GOLDEN
                .lines()
                .filter(|l| !l.starts_with("event."))
                .map(|l| format!("{l}\n"))
                .collect();
            std::fs::write(path, others + &line).expect("bless");
            return;
        }
        let have: String = GOLDEN
            .lines()
            .filter(|l| l.starts_with("event."))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            have, line,
            "the event body changed; see spec/persistence.md"
        );
    }

    fn body(id: u32) -> event::Body {
        event::Body::World(WorldEvent {
            dcs_id: id,
            ..WorldEvent::default()
        })
    }

    /// A log on disk reads older events from its file and newer ones from
    /// memory, with no gap and no repeat where the two meet.
    #[test]
    fn a_log_on_disk_reads_across_the_memory_boundary() {
        let d = tempfile::tempdir().expect("dir");
        let file = EventFile::open(&d.path().join("events.sqlite"), 1).expect("file");
        let mut l = EventLog::open(file, 3).expect("log");
        for i in 0..10 {
            l.push(1, pb::Stamp::default(), body(i));
        }
        l.commit().expect("commit");
        let seqs = |after, limit| -> Vec<u64> {
            l.after(after, limit).iter().map(|e| e.sequence).collect()
        };
        assert_eq!(seqs(0, 100), (1..=10).collect::<Vec<u64>>());
        assert_eq!(seqs(5, 2), vec![6, 7]);
        assert_eq!(seqs(6, 100), vec![7, 8, 9, 10]);
        assert_eq!(seqs(7, 100), vec![8, 9, 10]);
        assert_eq!(seqs(9, 100), vec![10]);
        assert!(seqs(10, 100).is_empty());
    }

    /// The lookup behind `RemovePlayerMark` reads an event and its agent boot
    /// from the file once memory no longer holds it, including after a
    /// reopen.
    #[test]
    fn get_reads_an_event_and_its_boot_from_the_file() {
        let d = tempfile::tempdir().expect("dir");
        let path = d.path().join("events.sqlite");
        {
            let file = EventFile::open(&path, 1).expect("file");
            let mut l = EventLog::open(file, 3).expect("log");
            for i in 0..10u32 {
                // Event n comes from boot 100 + n; the fifth has no key.
                let source = (i != 4).then(|| format!("{}:{}", 100 + i + 1, i));
                let stamp = crate::convert::stamp(
                    0,
                    0,
                    0,
                    0.0,
                    Some(prost_types::Timestamp {
                        seconds: i64::from(i) + 1,
                        nanos: 0,
                    }),
                );
                l.push_from(1, stamp, body(i), source);
            }
            l.commit().expect("commit");
            check_get(&l);
        }
        // A reopen keeps only the newest 3 in memory; the rest come from the
        // file with the same boots.
        let file = EventFile::open(&path, 1).expect("reopen");
        let mut l = EventLog::open(file, 3).expect("log");
        check_get(&l);
        // A sequence the retention purged is gone, and so is one past the
        // newest.
        l.retain(Duration::from_millis(1), 1_000_000, 5, 0)
            .expect("retain");
        assert!(l.get(1).is_none());
        assert!(l.get(5).is_none());
        assert_eq!(l.get(6).map(|g| g.1), Some(106));
        assert!(l.get(11).is_none());
        assert!(l.get(0).is_none());
    }

    fn check_get(l: &EventLog) {
        // In memory: the newest.
        let (e, boot) = l.get(10).expect("memory");
        assert_eq!((e.sequence, boot), (10, 110));
        // Only in the file.
        let (e, boot) = l.get(2).expect("file");
        assert_eq!((e.sequence, boot), (2, 102));
        assert!(matches!(e.body, Some(event::Body::World(ref w)) if w.dcs_id == 1));
        // No source key: boot 0.
        assert_eq!(l.get(5).map(|g| g.1), Some(0));
        assert!(l.get(11).is_none());
    }

    #[test]
    fn a_log_numbers_and_keeps_the_newest() {
        let mut l = EventLog::new(3);
        assert_eq!(l.last(), 0);
        assert!(l.covers(0));
        for i in 0..5 {
            l.push(9, pb::Stamp::default(), body(i));
        }
        assert_eq!((l.first(), l.last()), (3, 5));
        assert!(!l.covers(1));
        assert!(l.covers(2));
        assert!(l.covers(5));
        // A reader ahead of the log is not covered: it would skip events.
        assert!(!l.covers(6));
        assert!(!EventLog::new(3).covers(1));
        let seqs: Vec<u64> = l.after(3, 10).iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![4, 5]);
        let seqs: Vec<u64> = l.after(0, 2).iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![3, 4]);
    }

    #[test]
    fn stamps_use_agent_time_and_lifecycle_repeats_are_dropped() {
        use nanoprobe_transport::records::{Frame, Lifecycle};
        let rec = |i: Input| {
            let (kind, payload) = i.encode();
            RawRecord {
                agent_boot: 1,
                number: 0,
                kind,
                payload,
                resync: false,
            }
        };
        let mut link = Link::default();
        link.read(&rec(Input::Frame(Frame {
            real_time: 100.0,
            ..Frame::default()
        })));
        // A replayed frame made 60 s earlier is stamped 60 s earlier.
        link.read(&rec(Input::Frame(Frame {
            real_time: 40.0,
            replay: 1,
            ..Frame::default()
        })));
        let w = link.stamp().wall.expect("wall").seconds;
        let now = now_s() as i64;
        assert!((now - 61..=now - 59).contains(&w), "{w} vs {now}");
        let l = Input::Lifecycle(Lifecycle {
            state: 3,
            mission_gen: 1,
            theater: "Caucasus".into(),
            mission: String::new(),
        });
        let hello = |boot: u64| {
            Input::Hello(nanoprobe_transport::records::Hello {
                agent_boot: boot,
                ..nanoprobe_transport::records::Hello::default()
            })
        };
        link.read(&rec(hello(1)));
        assert!(link.read(&rec(l.clone())).is_some());
        assert!(link.read(&rec(l.clone())).is_none());
        // The repeat after a HELLO of the same boot is not news.
        link.read(&rec(hello(1)));
        assert!(link.read(&rec(l.clone())).is_none(), "same boot");
        // A new agent boot: the same lifecycle is news again.
        link.read(&rec(hello(2)));
        assert!(link.read(&rec(l)).is_some(), "new boot");
        // Bad agent time is no time, not a panic.
        link.read(&rec(Input::Frame(Frame {
            real_time: f64::INFINITY,
            ..Frame::default()
        })));
        let _ = link.stamp();
        assert_eq!(wall(f64::NAN), None);
        assert_eq!(wall(1e300), None);
    }

    /// D292: the stamp carries the agent boot. Two boots with the same
    /// generation and frame give unequal stamps; a reattach in one boot
    /// keeps the value; detached, the boot is 0; gap ends carry it.
    #[test]
    fn stamps_carry_the_agent_boot() {
        use nanoprobe_transport::records::{Frame, Gap, Hello};
        let rec = |boot: u64, i: Input| {
            let (kind, payload) = i.encode();
            RawRecord {
                agent_boot: boot,
                number: 0,
                kind,
                payload,
                resync: false,
            }
        };
        let hello = |boot: u64| {
            Input::Hello(Hello {
                agent_boot: boot,
                ..Hello::default()
            })
        };
        let frame = || {
            Input::Frame(Frame {
                frame: 9,
                mission_gen: 1,
                model_time: 5.0,
                real_time: 100.0,
                replay: 1,
                ..Frame::default()
            })
        };
        // The wall time of a replayed frame is now: left out.
        let at = |l: &Link| {
            let mut s = l.stamp();
            s.wall = None;
            s
        };
        let mut link = Link::default();
        assert_eq!(link.stamp().agent_boot, 0, "before the first HELLO");
        link.read(&rec(7, hello(7)));
        link.read(&rec(7, frame()));
        let a = at(&link);
        assert_eq!(a.agent_boot, 7);
        link.attached = false;
        assert_eq!(link.stamp().agent_boot, 0, "no agent attached");
        // A sidecar restart in the same boot: a new link reads the same HELLO.
        let mut fresh = Link::default();
        fresh.read(&rec(7, hello(7)));
        fresh.read(&rec(7, frame()));
        assert_eq!(at(&fresh), a, "the same boot keeps the value");
        link.read(&rec(7, hello(7)));
        link.read(&rec(7, frame()));
        assert_eq!(at(&link), a);
        // A DCS restart: the same generation and frame, another boot. Its
        // first FRAME and a GAP come before its HELLO.
        link.read(&rec(8, frame()));
        assert_eq!(link.stamp().agent_boot, 8, "the record's boot");
        let early = link
            .read(&rec(
                8,
                Input::Gap(Gap {
                    class: gap_class::EVENTS as u32,
                    from_gen: 1,
                    to_gen: 1,
                    ..Gap::default()
                }),
            ))
            .expect("a gap event");
        let event::Body::Gap(g) = early.body else {
            panic!("a gap");
        };
        assert_eq!(g.from.expect("from").agent_boot, 8);
        link.read(&rec(8, hello(8)));
        link.read(&rec(8, frame()));
        let b = at(&link);
        assert_eq!(
            (b.mission_generation, b.frame, b.model_time),
            (a.mission_generation, a.frame, a.model_time)
        );
        assert_ne!(b, a);
        assert_eq!(b.agent_boot, 8);
        let read = link
            .read(&rec(
                8,
                Input::Gap(Gap {
                    class: gap_class::EVENTS as u32,
                    count: 2,
                    from_gen: 1,
                    to_gen: 1,
                    ..Gap::default()
                }),
            ))
            .expect("a gap event");
        let event::Body::Gap(g) = read.body else {
            panic!("a gap");
        };
        assert_eq!(g.from.expect("from").agent_boot, 8);
        assert_eq!(g.to.expect("to").agent_boot, 8);
    }

    /// D292: a logged event keeps the boot of its source key after a
    /// reopen and after a DCS restart, also when its stored body has no
    /// boot; one with no key keeps its body's, 0 when absent.
    #[test]
    fn a_logged_event_keeps_its_agent_boot() {
        let d = tempfile::tempdir().expect("dir");
        let path = d.path().join("events.sqlite");
        let at = |boot: u64| crate::convert::stamp(boot, 1, 9, 5.0, None);
        {
            // A body stored with no boot, as before the field, written
            // straight to the file.
            let mut file = EventFile::open(&path, 1).expect("file");
            let old = Event {
                sequence: 1,
                stamp: Some(at(0)),
                body: Some(body(1)),
                campaign_id: 1,
            };
            file.push(EventRow {
                seq: 1,
                wall_ms: 1,
                source: Some("41:1".into()),
                body: old.encode_to_vec(),
            });
            file.commit().expect("commit");
        }
        {
            let file = EventFile::open(&path, 1).expect("file");
            let mut l = EventLog::open(file, 10).expect("log");
            l.push_from(1, at(0), body(2), Some("41:2".into()));
            l.push(1, at(0), body(3));
            // A DCS restart: the same generation and frame, another boot.
            l.push_from(1, at(42), body(4), Some("42:1".into()));
            l.commit().expect("commit");
            let boots: Vec<u64> = (1..=4)
                .map(|n| l.get(n).expect("held").0.stamp.expect("stamp").agent_boot)
                .collect();
            assert_eq!(boots, vec![41, 41, 0, 42]);
        }
        let file = EventFile::open(&path, 1).expect("file");
        // Memory holds one: the rest are read from the file.
        let l = EventLog::open(file, 1).expect("log");
        let boots: Vec<u64> = l
            .after(0, 10)
            .into_iter()
            .map(|e| e.stamp.expect("stamp").agent_boot)
            .collect();
        assert_eq!(boots, vec![41, 41, 0, 42]);
    }

    #[test]
    fn an_event_record_shows_its_plain_fields_and_no_object_ids() {
        use nanoprobe_transport::codec::Lv;
        let e = records::Event {
            id: 24,
            fields: vec![
                (2, Lv::Number(16_777_472.0)),
                (5, Lv::String(b"M2 Browning".to_vec())),
                (10, Lv::Number(100.0)),
                (11, Lv::Number(5.0)),
                (12, Lv::Number(-200.0)),
                (16, Lv::Number(42.0)),
                (17, Lv::Number(1.0)),
                (8, Lv::Bool(true)),
            ],
        };
        let mut link = Link {
            theater: "Caucasus".into(),
            ..Link::default()
        };
        let (kind, payload) = Input::Event(e).encode();
        let r = link
            .read(&RawRecord {
                agent_boot: 1,
                number: 0,
                kind,
                payload,
                resync: false,
            })
            .expect("an event");
        let event::Body::World(w) = r.body else {
            panic!("world event")
        };
        assert_eq!(w.dcs_id, 24);
        assert_eq!(w.weapon.as_deref(), Some("M2 Browning"));
        assert_eq!(w.rounds, Some(42));
        // TEXT of the wrong type is left out.
        assert_eq!(w.text, None);
        let p = w.position.expect("position");
        assert_eq!((p.north, p.east, p.alt), (100.0, -200.0, 5.0));
        let extra = w.extra.expect("extra");
        assert!(extra.fields.contains_key("initiator_category"));
        // The initiator's DCS object ID is not shown.
        assert_eq!(extra.fields.len(), 1);
        assert_eq!(w.initiator_id, None);
    }

    /// The WorldEvent a record of this event ID and fields gives.
    fn world_event(id: u32, fields: Vec<(u16, nanoprobe_transport::codec::Lv)>) -> pb::WorldEvent {
        let mut link = Link {
            theater: "Caucasus".into(),
            ..Link::default()
        };
        let (kind, payload) = Input::Event(records::Event { id, fields }).encode();
        let r = link
            .read(&RawRecord {
                agent_boot: 1,
                number: 0,
                kind,
                payload,
                resync: false,
            })
            .expect("an event");
        let event::Body::World(w) = r.body else {
            panic!("world event")
        };
        w
    }

    #[test]
    fn a_mark_event_shows_its_mark_id_and_others_do_not() {
        use nanoprobe_transport::codec::Lv;
        let mark = |id: u32, v: f64| world_event(id, vec![(field::MARK_ID as u16, Lv::Number(v))]);
        for id in 25..=27 {
            assert_eq!(mark(id, 17.0).mark_id, Some(17), "event {id}");
        }
        assert_eq!(mark(24, 17.0).mark_id, None, "not a mark event");
        assert_eq!(mark(28, 17.0).mark_id, None, "not a mark event");
        assert_eq!(mark(25, -1.0).mark_id, None, "negative");
        assert_eq!(mark(25, f64::NAN).mark_id, None, "NaN");
        assert_eq!(mark(25, f64::INFINITY).mark_id, None, "infinite");
        assert_eq!(mark(25, 1e300).mark_id, None, "too large");
    }

    #[test]
    fn filters_pass_gaps_and_lifecycle() {
        let mut l = EventLog::new(10);
        l.push(0, pb::Stamp::default(), body(8));
        l.push(0, pb::Stamp::default(), event::Body::Gap(PbGap::default()));
        let got: Vec<u64> = l
            .after(0, 10)
            .iter()
            .filter(|e| passes(e, &[1], ""))
            .map(|e| e.sequence)
            .collect();
        assert_eq!(got, vec![2]);
    }
}
