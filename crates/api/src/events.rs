//! The event log (plan 4.7; spec/world.md, "Events and the store") and what
//! the server reads from the agent's records besides the reconciler.
//!
//! Each campaign has a log numbered from 1, and so does the server log, which
//! holds events recorded while no campaign is active. A log is never
//! conflated: a consumer reads every event in order from a sequence number,
//! and a lost span is an explicit `Gap`. A campaign's log on disk is backed
//! by its `history.log` (spec/persistence.md, "Event history"): an event is
//! appended and flushed before it is published. Every log keeps the newest
//! `events_keep` in memory for the Events service.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, SystemTime};

use nanoprobe_proto::v1alpha1::{
    self as pb, AuthorEvent, Event, Gap as PbGap, LifecycleEvent, MenuAttribution, MenuEvent,
    PlayerEvent, PlayerEventKind, SimState, WorldEvent, event,
};
use nanoprobe_sidecar::history::History;
use nanoprobe_sidecar::players::{LaunchRule, Note, Roster};
use nanoprobe_sidecar::sim::RawRecord;
use nanoprobe_sidecar::sim::wire::Input;
use nanoprobe_transport::layout::{gap_class, lifecycle as lc};
use nanoprobe_transport::records;

use prost::Message;

use crate::lua::from_lua;

/// One numbered log.
#[derive(Debug)]
pub struct EventLog {
    keep: usize,
    next: u64,
    events: VecDeque<Event>,
    history: Option<History>,
}

impl EventLog {
    /// An empty log in memory keeping the newest `keep` events.
    pub fn new(keep: usize) -> EventLog {
        EventLog {
            keep: keep.max(1),
            next: 1,
            events: VecDeque::new(),
            history: None,
        }
    }

    /// A log backed by `history`, keeping its newest `keep` events in
    /// memory. A record whose body does not decode as an event is an error.
    pub fn open(history: History, keep: usize) -> io::Result<EventLog> {
        let keep = keep.max(1);
        let mut events = VecDeque::new();
        for r in history.read_from(0)? {
            let e = Event::decode(r.body.as_slice()).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("history: event {} does not decode: {e}", r.seq),
                )
            })?;
            events.push_back(e);
            if events.len() > keep {
                events.pop_front();
            }
        }
        Ok(EventLog {
            keep,
            next: history.next_seq(),
            events,
            history: Some(history),
        })
    }

    /// Append an event, numbering it. Returns its sequence number. A log on
    /// disk appends and flushes the event's record first; a failure stops
    /// the sidecar through its panic path (spec/persistence.md, "Event
    /// history").
    pub fn push(&mut self, campaign: u64, stamp: pb::Stamp, body: event::Body) -> u64 {
        let sequence = self.next;
        let e = Event {
            sequence,
            stamp: Some(stamp),
            body: Some(body),
            campaign_id: campaign,
        };
        if let Some(h) = self.history.as_mut() {
            let wall_ms = e
                .stamp
                .as_ref()
                .and_then(|s| s.wall)
                .map_or_else(now_ms, |t| {
                    u64::try_from(t.seconds)
                        .unwrap_or(0)
                        .saturating_mul(1000)
                        .saturating_add(u64::try_from(t.nanos / 1_000_000).unwrap_or(0))
                });
            if let Err(err) = h
                .append(sequence, wall_ms, &e.encode_to_vec())
                .and_then(|()| h.flush())
            {
                panic!("the event history could not be written, stopping: {err}");
            }
        }
        self.next += 1;
        self.events.push_back(e);
        while self.events.len() > self.keep {
            self.events.pop_front();
        }
        sequence
    }

    /// The newest sequence number, 0 for none yet.
    pub fn last(&self) -> u64 {
        self.next - 1
    }

    /// The oldest sequence number kept, or the next one when empty.
    pub fn first(&self) -> u64 {
        self.events.front().map_or(self.next, |e| e.sequence)
    }

    /// Whether a reader that has every event up to `after` can go on from
    /// here: the events after it are all kept, and `after` is not ahead of
    /// the newest.
    pub fn covers(&self, after: u64) -> bool {
        after <= self.last() && after + 1 >= self.first()
    }

    /// Up to `limit` events after `after`, in order.
    pub fn after(&self, after: u64, limit: usize) -> impl Iterator<Item = &Event> {
        let first = self.first();
        let skip = usize::try_from((after + 1).saturating_sub(first)).unwrap_or(usize::MAX);
        self.events.iter().skip(skip).take(limit)
    }
}

/// What the server knows of the agent from its records.
#[derive(Clone, Debug, Default)]
pub struct Link {
    /// A `HELLO` was read from the agent boot now running.
    pub attached: bool,
    /// The agent boot of the last `HELLO`.
    pub agent_boot: u64,
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
        pb::Stamp {
            wall: wall(at),
            model_time: self.model_time,
            mission_generation: self.mission_gen,
            frame: self.frame,
        }
    }

    /// Fold one record in. Returns the event it carries, if any. An
    /// `AUTHOR_EVENT` names no entity.
    pub fn read(&mut self, r: &RawRecord) -> Option<Read> {
        self.read_with(
            r,
            &Lookup {
                unit: &|_| None,
                roster: None,
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
                let end = |real: f64, model: f64, gen_: u32| pb::Stamp {
                    wall: self.offset.and_then(|o| wall(o + real)),
                    model_time: model,
                    mission_generation: gen_,
                    frame: 0,
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
        WorldEvent {
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
            ..WorldEvent::default()
        }
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

    fn body(id: u32) -> event::Body {
        event::Body::World(WorldEvent {
            dcs_id: id,
            ..WorldEvent::default()
        })
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
        let seqs: Vec<u64> = l.after(3, 10).map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![4, 5]);
        let seqs: Vec<u64> = l.after(0, 2).map(|e| e.sequence).collect();
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

    #[test]
    fn filters_pass_gaps_and_lifecycle() {
        let mut l = EventLog::new(10);
        l.push(0, pb::Stamp::default(), body(8));
        l.push(0, pb::Stamp::default(), event::Body::Gap(PbGap::default()));
        let got: Vec<u64> = l
            .after(0, 10)
            .filter(|e| passes(e, &[1], ""))
            .map(|e| e.sequence)
            .collect();
        assert_eq!(got, vec![2]);
    }
}
