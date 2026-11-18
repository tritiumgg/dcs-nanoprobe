//! The agent's object table, its time slice and the sampling tiers
//! (spec/sampling.md, spec/records.md "The object table").
//!
//! Plain Rust, with every time passed in, so the host tests drive it with
//! made-up clocks. The agent DLL calls it from the shim's pump: the shim asks
//! [`Sampler::plan`] for the next batch, reads the getters for each row, and
//! passes each row's values to one of the sinks, which say when the budget
//! is spent. The table's slots are stable; the shim keeps each slot's DCS
//! object in its own arrays, by the slot numbers this module gives it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::kind;
use nanoprobe_transport::records::{
    Airbase, Ammo, Attitude, EnumBegin, EnumEnd, Object, ObjectGone, Positions, Seen, Vitals,
    Warehouse,
};

use crate::link::Link;

/// What the sampler needs of the link: the queue, the sample writer, and
/// the frame's figures. [`Link`] is the one the agent uses; the tests use a
/// recorder.
pub trait Port {
    /// Queue an event-class record.
    fn enqueue(&mut self, kind: u16, payload: Vec<u8>);
    /// Write a sample record; false when it was dropped.
    fn sample(&mut self, kind: u16, payload: &[u8]) -> bool;
    /// Write the queued records.
    fn flush(&mut self);
    /// The largest payload a record may have.
    fn max_payload(&self) -> usize;
    /// When the current frame began.
    fn began(&self) -> Instant;
    /// The running average interval between frames, µs.
    fn frame_interval_us(&self) -> f64;
    /// `overhead_share`.
    fn overhead_share(&self) -> f64;
    /// Whether the current frame made the system calls.
    fn syscall_frame(&self) -> bool;
    /// `HELLO` records written so far.
    fn hellos(&self) -> u64;
    /// Event-class records the link has dropped so far, counted at the
    /// moment of each drop.
    fn event_drops(&self) -> u64;
    /// The boot of the last sidecar that attached.
    fn served_boot(&self) -> u64;
    /// Store the slice, periods and rows per clock read for the status block.
    fn set_sampling(&mut self, slice_us: f64, periods: [f64; 4], rows_per_clock: u64);
    /// The frame's commanded work, µs, which its agent time leaves out
    /// (spec/sampling.md, "Slice").
    fn set_frame_commanded(&mut self, us: f64);
}

impl Port for Link {
    fn enqueue(&mut self, kind: u16, payload: Vec<u8>) {
        Link::enqueue(self, kind, payload);
    }
    fn sample(&mut self, kind: u16, payload: &[u8]) -> bool {
        Link::sample(self, kind, payload)
    }
    fn flush(&mut self) {
        Link::flush(self);
    }
    fn max_payload(&self) -> usize {
        self.outbound().ring().max_payload()
    }
    fn began(&self) -> Instant {
        Link::began(self)
    }
    fn frame_interval_us(&self) -> f64 {
        Link::frame_interval_us(self)
    }
    fn overhead_share(&self) -> f64 {
        Link::overhead_share(self)
    }
    fn syscall_frame(&self) -> bool {
        Link::syscall_frame(self)
    }
    fn hellos(&self) -> u64 {
        Link::hellos(self)
    }
    fn event_drops(&self) -> u64 {
        Link::event_drops(self)
    }
    fn served_boot(&self) -> u64 {
        Link::served_boot(self)
    }
    fn set_sampling(&mut self, slice_us: f64, periods: [f64; 4], rows_per_clock: u64) {
        Link::set_sampling(self, slice_us, periods, rows_per_clock);
    }
    fn set_frame_commanded(&mut self, us: f64) {
        Link::set_frame_commanded(self, us);
    }
}

/// `object_kind` codes.
pub mod object_kind {
    /// A unit.
    pub const UNIT: u32 = 1;
    /// A static.
    pub const STATIC: u32 = 2;
    /// A group, in `BIND` only.
    pub const GROUP: u32 = 3;
}

/// `gone_reason` codes.
pub mod gone {
    /// A death fact: a death event named it, or the reads of the gone row
    /// show a kill.
    pub const DEAD: u32 = 1;
    /// The agent destroyed it for a command.
    pub const DESTROYED: u32 = 2;
    /// DCS no longer has it and the cause is unknown; a `DEAD` may follow.
    pub const MISSING: u32 = 3;
    /// DCS no longer has it and the reads show a removal, not a kill.
    pub const REMOVED: u32 = 4;
}

/// DCS `Object.Category` of a unit and of a static (LIVE, S9).
const CATEGORY_UNIT: f64 = 1.0;

const CATEGORY_STATIC: f64 = 3.0;
/// DCS `Object.Category.CARGO`: a static that can move (LIVE, S10).
const CATEGORY_CARGO: f64 = 6.0;

/// The tiers, in the order the budget goes to them (spec/sampling.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Tier {
    /// Births waiting to be resolved by the shim.
    Births = 1,
    /// The full enumeration, while one runs.
    Enumeration = 2,
    /// Positions of subscribed rows.
    Subscribed = 3,
    /// Positions of every row.
    Positions = 4,
    /// Attitude of rows that stopped.
    Stopped = 5,
    /// Life.
    Vitals = 6,
    /// Warehouses and airbase owners.
    Warehouses = 7,
}

impl Tier {
    /// Every tier, in budget order.
    pub const ALL: [Tier; 7] = [
        Tier::Births,
        Tier::Enumeration,
        Tier::Subscribed,
        Tier::Positions,
        Tier::Stopped,
        Tier::Vitals,
        Tier::Warehouses,
    ];

    fn index(self) -> usize {
        self as usize - 1
    }
}

/// The target periods of the tiers, seconds (`[sampling]` in `agent.toml`,
/// `RATES`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rates {
    /// Positions.
    pub position_s: f64,
    /// Life.
    pub vitals_s: f64,
    /// Warehouses and airbases.
    pub warehouse_s: f64,
    /// The full enumeration.
    pub enumeration_s: f64,
}

impl Default for Rates {
    fn default() -> Rates {
        Rates {
            position_s: 1.0,
            vitals_s: 5.0,
            warehouse_s: 60.0,
            enumeration_s: 60.0,
        }
    }
}

/// What the shim learns about a unit or static, to add its row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// DCS object ID.
    pub object: u64,
    /// DCS group ID, 0 for a static.
    pub group: u64,
    /// `UNIT` or `STATIC`.
    pub kind: u32,
    /// DCS category number.
    pub category: u32,
    /// DCS coalition.
    pub coalition: u32,
    /// DCS country.
    pub country: u32,
    /// `getLife0()`.
    pub life0: f32,
    /// The object's name.
    pub name: String,
    /// The type name.
    pub type_name: String,
    /// The group's name, empty for a static.
    pub group_name: String,
    /// A static that can move: cargo (DCS `Object.Category.CARGO`). Its
    /// position is read with the units'. Not part of the `OBJECT` record.
    pub movable: bool,
}

/// A loadout read an event asked for.
#[derive(Clone, Debug)]
struct LoadoutMark {
    /// The unit's DCS object ID: a birth's row does not exist yet.
    object: u64,
    /// [`Ammo::BIRTH`] or [`Ammo::TAKEOFF`]; the later event's.
    cause: u32,
    /// Pumps that found no row for the object.
    waits: u32,
}

#[derive(Clone, Debug)]
struct Row {
    version: u32,
    found: Found,
    entity: u64,
    group_entity: u64,
    /// Bound by `BIND`, not by its name.
    bound_entity: bool,
    bound_group: bool,
    /// The enumeration this row was in the table at the start of, and
    /// whether that enumeration found it.
    enum_member: u64,
    enum_seen: bool,
    /// The `HELLO` count plus one when the row's last `OBJECT` was queued:
    /// an `OBJECT` the current attach's sidecar may not have read has a
    /// different value. 0 when none was queued.
    sent: u64,
    /// Left out of every tier until an event or an enumeration settles it.
    suspect: bool,
    last: Option<[f64; 3]>,
    moving: bool,
    /// The row's `OBJECT` payload, encoded once and patched with each
    /// enumeration's number; empty until first needed, and emptied when
    /// the row's entities change.
    encoded: Vec<u8>,
    /// Waiting for its attitude: in the list of rows that stopped, or in
    /// the batch reading them.
    in_stopped: bool,
    /// Position reads in a row that found it unmoved, up to `IDLE_AFTER`.
    unmoved: u8,
    /// An event named it since its life was last read: it is in the list of
    /// rows to read.
    life_marked: bool,
    /// When its life was last read.
    life_at: Option<Instant>,
    /// A subscription names it.
    watched: bool,
    /// Its object ID waits in the queue of existence checks.
    checking: bool,
    /// A death event named it since its last existence check.
    death_named: bool,
    /// Its check is in the urgent queue.
    urgent: bool,
}

impl Row {
    fn new(found: Found, entity: u64, group_entity: u64, version: u32) -> Row {
        Row {
            version,
            found,
            entity,
            group_entity,
            bound_entity: false,
            bound_group: false,
            enum_member: 0,
            enum_seen: false,
            sent: 0,
            suspect: false,
            last: None,
            moving: false,
            encoded: Vec::new(),
            in_stopped: false,
            unmoved: 0,
            life_marked: false,
            life_at: None,
            watched: false,
            checking: false,
            death_named: false,
            urgent: false,
        }
    }

    fn unit(&self) -> bool {
        self.found.kind == object_kind::UNIT
    }

    /// Its position is read by the positions tier: a unit, or a static
    /// that can move.
    fn swept(&self) -> bool {
        self.unit() || self.found.movable
    }

    /// Read less often: unmoved for `IDLE_AFTER` position reads.
    fn idle(&self) -> bool {
        self.unmoved >= IDLE_AFTER
    }

    /// Its life is read every vitals period: a subscription names
    /// it. Any other row's are read when it is added, after a hit names it,
    /// and on the slow sweep.
    fn vital(&self) -> bool {
        self.watched
    }
}

/// What a read of a gone object gave.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Read<T> {
    /// The call returned; `Option` types hold `None` for nil or another type.
    Value(T),
    /// The call raised a Lua error.
    Raised,
}

/// A gone row's class, from its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// A unit of DCS category 2.
    Ground,
    /// A unit of category 0 or 1.
    Aircraft,
    /// A unit of category 3.
    Ship,
    /// A static that cannot move.
    Static,
    /// A static that can move.
    Cargo,
    /// A unit of any other category.
    Other,
}

/// How the reads on a gone row of a class are read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rule {
    /// Off: every gone row of the class leaves as `MISSING`, and no anomaly
    /// is counted.
    enabled: bool,
    /// Which read means dead.
    dead: DeadRead,
    /// Reads that all raise may be recorded as `REMOVED`; otherwise they
    /// give `MISSING` with no anomaly.
    removed_on_raise: bool,
}

/// Which read of a gone row means dead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadRead {
    /// `Unit.isDead` true and `Unit.getLife` a finite number.
    UnitFlags,
    /// `StaticObject.getLife` a number at or below 0. The caller
    /// passes it as `life`, and `dead` as `Read::Value(None)`.
    StaticLife,
}

/// The settings of every class: the only place that decides what a read of
/// a gone row means.
fn rule(class: Class) -> Rule {
    match class {
        Class::Ground => Rule {
            enabled: true,
            dead: DeadRead::UnitFlags,
            removed_on_raise: true,
        },
        // Off by ruling: a ship on Wine never read dead, and the death
        // events of both come at once and correct the row.
        Class::Aircraft | Class::Ship => Rule {
            enabled: false,
            dead: DeadRead::UnitFlags,
            removed_on_raise: false,
        },
        Class::Static => Rule {
            enabled: true,
            dead: DeadRead::StaticLife,
            removed_on_raise: true,
        },
        // Off by ruling: a crate killed by gunfire sends no event at all.
        Class::Cargo => Rule {
            enabled: false,
            dead: DeadRead::StaticLife,
            removed_on_raise: false,
        },
        Class::Other => Rule {
            enabled: false,
            dead: DeadRead::UnitFlags,
            removed_on_raise: false,
        },
    }
}

/// The class of a row.
fn class_of(row: &Row) -> Class {
    if !row.unit() {
        return if row.found.movable {
            Class::Cargo
        } else {
            Class::Static
        };
    }
    match row.found.category {
        2 => Class::Ground,
        0 | 1 => Class::Aircraft,
        3 => Class::Ship,
        _ => Class::Other,
    }
}

/// The reason a gone row leaves with, and whether the reads are an anomaly.
fn classify(rule: Rule, dead: &Read<Option<bool>>, life: &Read<Option<f64>>) -> (u32, bool) {
    if !rule.enabled {
        return (gone::MISSING, false);
    }
    let raised = if rule.removed_on_raise {
        gone::REMOVED
    } else {
        gone::MISSING
    };
    match rule.dead {
        DeadRead::UnitFlags => match (dead, life) {
            (Read::Value(Some(true)), Read::Value(Some(l))) if l.is_finite() => (gone::DEAD, false),
            (Read::Raised, Read::Raised) => (raised, false),
            _ => (gone::MISSING, true),
        },
        DeadRead::StaticLife => match life {
            Read::Value(Some(l)) if *l <= 0.0 => (gone::DEAD, false),
            Read::Raised => (raised, false),
            _ => (gone::MISSING, true),
        },
    }
}

/// What the table keeps of an object whose row left as `MISSING` or
/// `REMOVED`, until a death event for it arrives or the generation ends.
#[derive(Clone, Copy, Debug)]
struct Kept {
    entity: u64,
    group_entity: u64,
    reason: u32,
}

/// One planned batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The tier.
    pub tier: Tier,
    /// Rows in the batch: slots written to the plan's buffer for the row
    /// tiers, a count of items for births and the enumeration.
    pub rows: usize,
    /// For [`Tier::Enumeration`]: a new enumeration starts with this batch.
    /// For [`Tier::Subscribed`]: attitude is read with each position.
    pub flag: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct TierState {
    /// Rows owed.
    credit: f64,
    /// Running average cost per row, µs; 0 until measured.
    cost: f64,
    /// Next slot (or list index) to read.
    cursor: usize,
    /// When the current sweep began, and the last completion.
    sweep_start: Option<Instant>,
    last_complete: Option<Instant>,
    /// The first sampling frame on which the tier owed rows: a tier that
    /// has never completed a sweep reports its period from here.
    owed_since: Option<Instant>,
    /// The achieved period, seconds.
    period: f64,
}

/// The batch in progress: its tier, slots, and the rows done.
#[derive(Debug, Default)]
struct Batch {
    tier: Option<Tier>,
    slots: Vec<u32>,
    /// The generation of each slot's row when planned.
    versions: Vec<u32>,
    /// The tier's cursor place after each slot.
    after: Vec<usize>,
    /// How many of the batch's slots come before the end of the list, when
    /// its places pass it: the rest are the next pass's.
    wrap: Option<usize>,
    done: usize,
    started: Option<Instant>,
    /// The clock at the first row's sink, and at the last clock check with
    /// the rows done by then: the cost per row inside the batch.
    first: Option<Instant>,
    last: Option<(Instant, usize)>,
    /// Planned past what was left: its planned rows bound it, not the clock.
    past: bool,
    /// Read by the host in `frame`, outside the pump: its fixed part is not
    /// a pump batch's and stays out of that estimate.
    here: bool,
    /// Births' and the enumeration's steps over a quarter of the largest
    /// slice, such as one `coalition.getGroups`: their count and time. They
    /// stay out of the tier's cost per row, which plans the cheap steps.
    dear: usize,
    dear_us: f64,
    /// The other steps after the batch's first, which holds its fixed part:
    /// their count and time.
    cheap: usize,
    cheap_us: f64,
    /// Enumeration items counted since the last clock read
    /// ([`Sampler::seen_item`]).
    unmarked: usize,
    /// The batch's first slots that an event marked: they are not the
    /// cursor's, and do not count against the tier's credit.
    lead: usize,
    positions: Positions,
    vitals: Vitals,
    attitude: Attitude,
}

/// The slice's state (spec/sampling.md, "Slice").
#[derive(Clone, Copy, Debug, Default)]
struct Budget {
    debt: f64,
    syscall_per_s: f64,
    /// System-call time in the current second, and when it began.
    syscall_window: f64,
    syscall_window_start: Option<Instant>,
    slice: f64,
    /// The sampling budget of the frame, µs, and the instant it runs out.
    budget: f64,
    deadline: Option<Instant>,
    /// Event time since the last frame, µs.
    events_us: f64,
    /// Commanded-work time this frame, µs: handler take-to-done spans and a
    /// pump for commands only. It counts against the commanded budget, not
    /// the debt, the credit or the window (spec/commands.md, "Handlers").
    commanded_us: f64,
    /// Repayment planned this frame.
    repay: f64,
    /// What the test scale withheld from this frame's budget, µs.
    withheld: f64,
    allowance: f64,
    charge: f64,
}

/// The 10-second report window.
#[derive(Clone, Copy, Debug, Default)]
struct Window {
    start: Option<Instant>,
    agent_us: f64,
    forgiven_us: f64,
    /// Births' and the enumeration's time past their balance's bound, µs.
    maintenance_forgiven_us: f64,
    /// Vitals rows due in the window at the target period, and the rows
    /// the vitals cursor read: event-marked reads are in neither.
    vitals_due: f64,
    vitals_swept: u64,
    /// Event handling time in the window, µs.
    events_us: f64,
    /// Where the window's agent time went.
    spend: Spend,
}

/// Where a window's agent time went, µs, and the rows it bought. The parts
/// do not add up to the window's agent time: what is left is the end of
/// each frame, from the end of the pump or the decision not to pump.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    /// From the start of `frame` to the decision to pump, on frames that
    /// are not system-call frames.
    pub fixed_us: f64,
    /// The cross-state calls, from the decision to the start of the pump.
    pub xcall_us: f64,
    /// The pumps, from their start to their end.
    pub pump_us: f64,
    /// The system-call frames.
    pub syscall_us: f64,
    /// Each tier's batches, from plan to close, by the tier's number less
    /// one: births first, then the enumeration.
    pub tier_us: [f64; 7],
    /// Each tier's rows, in the same order.
    pub tier_rows: [u64; 7],
    /// The pumps' batches of every tier, rows and births' and the
    /// enumeration's steps, from plan to close: the work the pump bound
    /// weighs against the calls ("The pump's worth").
    pub pump_work_us: f64,
}

/// What the shim's pump learns at its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Begin {
    /// The mission generation: the shim drops its arrays when it changes.
    pub generation: u32,
    /// Whether to sample at all this frame.
    pub sample: bool,
}

/// Why the host's sweep read no row on a frame that owed some.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SweepWait {
    /// The pump for the subscribed rows read with their attitude goes first.
    Subscribed,
    /// The budget paid for rows, but fewer than the sweep's minimum batch.
    MinBatch,
    /// The savings would pay for a row, but the frame's slice, cut by the
    /// repayment of debt, does not; without the repayment it would.
    Debt,
    /// The savings do not yet pay for a row.
    Fund,
    /// The savings would pay for a row, but the frame's slice less its
    /// fixed work does not, with no repayment to blame: the fixed work, the
    /// system-call charge, the slice's cap, or a frame that takes no credit.
    Slice,
}

/// An enumeration's `ENUM_END`, held until its misses are resolved.
struct EndHold {
    /// The enumeration's number.
    n: u64,
    /// The misses whose existence check has not decided yet.
    pending: HashSet<u64>,
    /// When the enumeration ended.
    since: Instant,
}

/// The table, the tiers and the slice.
pub struct Sampler {
    rows: Vec<Option<Row>>,
    free: Vec<u32>,
    by_object: HashMap<u64, u32>,
    dead: HashSet<u64>,
    /// Objects whose row left as `MISSING` or `REMOVED` in this generation.
    kept: HashMap<u64, Kept>,
    /// Objects the agent destroyed that DCS may still hold, with the unpaused
    /// model time they entered at; and the order their existence is checked.
    pending: HashMap<u64, f64>,
    pending_order: VecDeque<u64>,
    /// `(entered_at, id)` in insertion order; unpaused model time only grows,
    /// so expiry pops from the front. IDs no longer in `pending` are skipped.
    pending_expiry: VecDeque<(f64, u64)>,
    /// Unpaused model seconds since the generation began, and the model time
    /// last seen.
    unpaused_s: f64,
    last_model: Option<f64>,
    /// `DEAD` events that named a pending object, objects that did not fit
    /// the set, and objects dropped at the backstop.
    pending_deaths: u64,
    pending_overflow: u64,
    pending_expired: u64,
    /// Rows [`Sampler::gone`] removed, by reason.
    gone_dead: u64,
    gone_removed: u64,
    gone_missing: u64,
    /// Rows the classifier could not decide, and `REMOVED` rows a death
    /// event later showed to be kills.
    classifier_anomalies: u64,
    classifier_faults: u64,
    /// Object IDs whose `isExist` the next pump calls.
    checks: VecDeque<u64>,
    /// Objects the last [`Sampler::checks`] handed out whose row a death
    /// event named: gone, they leave as `DEAD`.
    named: Vec<u64>,
    /// Objects of the dead set found alive again and added back.
    revivals: u64,
    /// Death-event marks cleared because the row was alive at its check.
    marks_cleared: u64,
    /// Enumerations whose `ENUM_END` was held and then abandoned.
    enum_end_abandoned: u64,
    /// The `ENUM_END` being held, and the pending misses handed out to the
    /// pump running.
    end_hold: Option<EndHold>,
    handed: Vec<u64>,
    /// Objects whose entry in `checks` is stale because the urgent queue
    /// holds the check: entries to skip, by object.
    promoted: HashMap<u64, u32>,
    generation: u32,
    campaign: u64,
    defaults: Rates,
    rates: Rates,
    tiers: [TierState; 7],
    budget: Budget,
    window: Window,
    batch: Batch,
    rows_per_clock: u64,
    clock_cost_us: f64,
    since_clock: u64,
    /// Births the shim holds.
    births: usize,
    /// Airbases the shim holds.
    airbases: usize,
    /// Enumeration: the number of the one running, and of the last.
    enumeration: Option<u64>,
    enum_count: u64,
    enum_objects: u32,
    enum_due: bool,
    enum_last_begin: Option<Instant>,
    enum_queued: bool,
    /// The `full` flag: the next enumeration writes `OBJECT` for every row.
    /// `ENUMERATE`, every `HELLO` and every dropped event-class record set it
    /// (spec/records.md, "Layout: SEEN"); `ENUM_BEGIN` takes it.
    full: bool,
    /// Whether the running enumeration took the flag.
    enum_full: bool,
    /// The link's count of dropped event-class records at the last
    /// `ENUM_BEGIN`.
    drops_seen: u64,
    /// The IDs of the open `SEEN` batch, and the enumeration it belongs to.
    seen_ids: Vec<u64>,
    seen_n: u64,
    /// IDs the running enumeration wrote in `SEEN` records, queued or open.
    enum_seen_ids: u32,
    /// Begins of the last two enumerations that ended.
    enum_ended_begins: (Option<Instant>, Option<Instant>),
    /// Subscriptions: entities, their period, the attitude flag, and the
    /// slots they name.
    sub_entities: HashSet<u64>,
    sub_period: f64,
    sub_attitude: bool,
    sub_slots: Vec<u32>,
    sub_dirty: bool,
    stopped: Vec<u32>,
    /// OBJECTs queued since the last flush.
    dirty: bool,
    hellos_seen: u64,
    sidecar_seen: u64,
    last_frame: Option<Instant>,
    sampling: bool,
    pump_start: Option<Instant>,
    /// Rows per batch at most.
    max_batch: usize,
    /// When [`Sampler::decide`] ran this frame.
    decided: Option<Instant>,
    /// Running average cost of the cross-state call, µs.
    xcall_us: f64,
    /// Running average cost of closing a batch, µs.
    close_us: f64,
    /// Running average of a batch's fixed cost, µs: planning it, the
    /// shim's dispatch and its return, apart from its rows.
    batch_us: f64,
    /// The part of `batch_us` after a batch's last clock check, µs: the
    /// shim's return to the next plan. The stop check keeps room for it.
    tail_us: f64,
    /// The running mean deviation of closing a batch from `close_us`, µs.
    /// The stop check keeps twice it in reserve, so a close slower than the
    /// mean rarely runs past the budget.
    close_dev_us: f64,
    /// What births and the enumeration may still spend, µs: each frame adds
    /// their share of its sampling allowance ([`Sampler::slice`]), and their
    /// steps take their time from it.
    maintenance_us: f64,
    /// What the balance earned this frame, µs: the row tiers' allowance is
    /// the frame's less this.
    maintenance_earned: f64,
    /// The time of this frame's birth and enumeration batches that their
    /// balance paid, µs: it stays out of the row tiers' debt.
    maintenance_paid: f64,
    /// How far below 0 their own time may take the balance, µs: the debt's
    /// cap, one second's budget, and at least the floor's 1,000 µs.
    maintenance_bound: f64,
    /// The running pump's cross-state call, µs; whether it planned a batch
    /// of a row tier; and what its balance paid of it so far, µs. A pump
    /// that only resolved births or read the enumeration is theirs whole.
    pump_call_us: f64,
    /// The frame's commanded time when the pump was decided, and what
    /// [`Sampler::excuse`] moved the decision past since, µs.
    decided_commanded: f64,
    excused_us: f64,
    pump_rows: bool,
    pump_paid: f64,
    /// When the frame before this one began.
    last_began: Option<Instant>,
    /// Pumps and frames this window.
    pumps: u64,
    frames: u64,
    /// Pumps this window that planned no row.
    empty_pumps: u64,
    /// Objects whose existence check a HIT, BDA, KILL or death event
    /// queued: they want a pump at once, not after the pump's wait.
    urgent: VecDeque<u64>,
    /// Tiers a pump planned a batch of, in the pump running.
    planned: [bool; 7],
    /// Tiers below the positions whose owed rows a sampling pump planned
    /// none of for lack of budget, since a planned row of the tier last
    /// cleared the mark. Each holds when the mark was first set: the tier
    /// leaves `pump_worth` for `PUMP_WAIT_S` from then, and a later pump
    /// does not restart it.
    unaffordable: [Option<Instant>; 7],
    /// A test switches the starvation guard off.
    #[cfg(test)]
    no_guard: bool,
    /// A test caps vitals' credit at one sweep's rows, to measure what the
    /// full-cycle cap changes.
    #[cfg(test)]
    vitals_cap_one_sweep: bool,
    /// The vitals rows due at the target period since the sampler began,
    /// uncapped, and the rows the vitals cursor read: a test's measure of
    /// the period as a rate.
    #[cfg(test)]
    vitals_due: f64,
    #[cfg(test)]
    vitals_swept: u64,
    /// When a batch of each tier was last planned.
    last_planned: [Option<Instant>; 7],
    /// The tier the pump's planning runs late, which takes one row.
    late_tier: Option<Tier>,
    /// `xcall_us` decayed on this frame: it decays at most once a frame.
    x_decayed: bool,
    /// This pump is wanted for the subscribed rows read with their attitude:
    /// it may spend the sweep's savings on them, and `pump_reserve` of its
    /// budget is left for the other tiers to keep, µs.
    sub_pump: bool,
    pump_reserve: f64,
    /// Batches planned this frame.
    batches: u32,
    /// When the last birth or enumeration step ended.
    step_at: Option<Instant>,
    /// Histograms of birth (0) and enumeration (1) steps, µs, since the
    /// agent started, and the largest in this window.
    steps: [[u64; 32]; 2],
    step_max: [f64; 2],

    /// The sampling budget's scale (spec/faults.md).
    #[cfg_attr(
        not(feature = "faults"),
        allow(dead_code, reason = "1 without fault injection")
    )]
    scale: f64,
    /// Frames this window whose fixed work reached the allowance.
    starved: u64,
    /// This frame took a row that costs more than its whole budget.
    lone: bool,
    /// Frames this window that took such a row.
    lone_rows: u64,
    /// This frame resolved births or ran the enumeration.
    maintenance: bool,
    /// Frames this window that only swept rows, and those that ran past the
    /// budget by more than one clock interval.
    row_frames: u64,
    overrun_frames: u64,
    /// What those frames ran past their budget in all, µs.
    overrun_sum_us: f64,
    /// Frames this window that did, and the most one ran past its budget.
    maintenance_frames: u64,
    maintenance_overrun: f64,
    /// Whether this pump handed out an existence check.
    checked: bool,
    /// Pumps in the window that planned no row but made checks, and the most
    /// one ran past its budget.
    check_pumps: u64,
    check_overrun: f64,
    /// When the pump began sampling, and the most it spent past its budget
    /// this window, µs.
    overrun_max: f64,
    /// Unit rows in the table.
    units: usize,
    /// The host sweeps positions by object ID in its own state, inside
    /// `frame` ([`Sampler::sweep_begin`]); the pump then reads no position
    /// but a static's point.
    direct: bool,
    /// What the sweep took of this frame, µs.
    sweep_spent: f64,
    /// The part of the credit set aside for the host's next sweep, µs: the
    /// pump does not spend it.
    sweep_fund: f64,
    /// When the host last swept positions, and the sweeps this window.
    last_sweep: Option<Instant>,
    sweeps: u64,
    /// Running averages of the host's sweep, µs: one row's reads inside the
    /// loop, a sweep's time outside the loop, and the part of that after
    /// the loop, which the deadline leaves room for.
    sweep_row_us: f64,
    sweep_batch_us: f64,
    sweep_tail_us: f64,
    /// Sweeps this window that the deadline stopped; those that ran past
    /// their budget by more than the rows between two clock reads; and the
    /// most one ran past it, µs.
    sweep_cut: u64,
    sweep_overruns: u64,
    sweep_overrun_max: f64,
    /// Sweeps this window that took one row past their budget because their
    /// fixed part alone passed it.
    sweep_forced: u64,
    /// Frames this window on which the sweep owed rows and read none, by
    /// [`SweepWait`] in its order.
    sweep_waits: [u64; 5],
    /// What a full sweep of the rows owed costs, µs: the most the pump
    /// leaves alone for it.
    sweep_need: f64,
    /// Why this frame's sweep read no row, or `None` when it read rows or
    /// owed none.
    sweep_wait: Option<SweepWait>,
    /// When the shim's pump last ran.
    last_pump: Option<Instant>,
    /// Completed sweeps of positions and vitals.
    /// With a row's slot they pick the sweep that reads a row read less
    /// often ([`Sampler::readable`]).
    passes: [u64; 2],
    /// The slow sweep of life: when the one running began, and how
    /// long the last one took, seconds.
    slow_start: Option<Instant>,
    slow_period: f64,
    /// Unit rows read less often because they have not moved; statics that
    /// can move, and those of them read less often.
    idle_units: usize,
    cargo: usize,
    idle_cargo: usize,
    /// Rows whose life is read every vitals period, and unit rows
    /// a subscription names.
    vital_rows: usize,
    /// Rows left out of every tier because a read raised on them: all of
    /// them, the units, and the cargo statics. They are in `units` and
    /// `cargo`, and in no other count.
    suspect_rows: usize,
    suspect_units: usize,
    suspect_cargo: usize,
    /// A row became suspect or was settled: the counts are taken again.
    recount: bool,
    /// Rows an event named since their life was read.
    life_marks: Vec<u32>,
    /// Loadout reads a birth or a takeoff event asked for, in the order
    /// they came, one entry per object.
    loadouts: Vec<LoadoutMark>,
    /// The marks the last [`Sampler::loadouts`] handed out. A read whose
    /// `AMMO` the ring dropped puts its mark back.
    loadouts_handed: Vec<LoadoutMark>,
    /// Oldest loadout marks dropped because the list was full.
    loadouts_dropped: u64,
    /// Loadout reads made, since start: one per `ammo` call for a unit row.
    loadout_reads: u64,
    /// A sample record's payload while it is written, kept for its
    /// capacity.
    scratch: Vec<u8>,
    /// The running average of the cross-state call as measured at each
    /// pump, µs, with no decay between pumps.
    xcall_seen: f64,
}

/// One second's budget is the cap on debt.
const SECOND_US: f64 = 1e6;
/// The most a frame's slice can be.
const SLICE_CAP_US: f64 = 250.0;
/// The least a frame keeps back from the cap for its finish, µs.
const FINISH_FLOOR_US: f64 = 6.0;
/// The most allowance a frame carries forward as credit, µs.
const CREDIT_CAP_US: f64 = 150.0;
/// The most rows in one batch.
const MAX_BATCH: usize = 256;
/// Births the shim keeps at most while detached (spec/sampling.md).
pub const MAX_BIRTHS: usize = 4096;
/// Clock reads may cost this share of a sweep at most.
const CLOCK_SHARE: f64 = 0.02;
/// The least sampling budget worth a cross-state call, µs.
const MIN_BUDGET_US: f64 = 1.0;
/// The most births and the enumeration may save up, µs, and the most the
/// scale's surcharge takes them below 0; their own time may take them
/// further.
const MAINTENANCE_CAP_US: f64 = 250.0;
const MAINTENANCE_FLOOR_US: f64 = -1000.0;
/// The share of each frame's sampling allowance that births and the
/// enumeration may take while a row tier owes rows; the larger one while an
/// enumeration has run past its period. While they wait, the host's sweep
/// saves from the rest only (spec/sampling.md, "Tiers").
const MAINTENANCE_SHARE: f64 = 0.5;
const MAINTENANCE_SHARE_OVERDUE: f64 = 0.75;

/// Movement under this many metres since the last sample is no movement.
const STOP_M: f64 = 0.5;
/// A unit or cargo static unmoved for this many position reads in a row is
/// read less often: once in `IDLE_EVERY` sweeps. The first read that finds
/// it moved returns it to every sweep.
const IDLE_AFTER: u8 = 3;
const IDLE_EVERY: u64 = 5;
/// A row no subscription names has its
/// life read once in this many vitals sweeps, besides the reads its
/// events ask for: the slow sweep (spec/sampling.md, "Tiers"). `None`
/// switches the slow sweep off; such a row is then read only when an event
/// asks.
const VITALS_EVERY: Option<u64> = Some(12);
/// A row an event names is read again no sooner than this after its last
/// read, seconds.
const LIFE_AGAIN_S: f64 = 1.0;
/// The host's sweep waits until it can read this many rows, or as many as
/// credit can ever pay for, so a batch's fixed part is shared by many rows.
/// A sweep that reads every row owed goes at once when the last one is
/// `SWEEP_LATE` of the position period old.
const SWEEP_MIN: usize = 64;
const SWEEP_LATE: f64 = 0.1;
/// The host's sweep reads the clock at least every this many rows, and stops
/// at its deadline: a slow getter or a low cost estimate cannot carry it
/// past the slice.
const SWEEP_CLOCK_ROWS: u64 = 32;
/// Work that is not worth a cross-state call by its size waits at most this
/// long for a pump, seconds.
const PUMP_WAIT_S: f64 = 1.0;

/// Existence checks one pump asks for at most.
pub const CHECKS_PER_PUMP: usize = 20;
/// Loadout reads one pump hands out at most (spec/sampling.md, "Loadout
/// reads").
pub const LOADOUTS_PER_PUMP: usize = 8;
/// The most loadout marks that wait; a mark past it is dropped and counted.
pub const LOADOUT_MARKS_MAX: usize = 1024;
/// Pumps a mark waits for its object's row, which a birth's mark precedes,
/// before it is dropped.
pub const LOADOUT_WAIT_PUMPS: u32 = 600;
/// Pending-destroy objects one pump checks at most, within `CHECKS_PER_PUMP`.
const PENDING_PER_PUMP: usize = 4;
/// The most objects the pending-destroy set holds. The object table has no
/// fixed capacity, so this is the bound on it (spec/commands.md, "A destroy
/// while paused is pending").
pub const PENDING_CAP: usize = 65_536;
/// Seconds of unpaused model time after which an object still existing leaves
/// the pending-destroy set.
const PENDING_EXPIRY_S: f64 = 60.0;

/// The index of a tier's sweep count in [`Sampler::passes`].
fn pass_of(tier: Tier) -> Option<usize> {
    match tier {
        Tier::Positions => Some(0),
        Tier::Vitals => Some(1),
        _ => None,
    }
}

impl Sampler {
    /// A sampler with the `agent.toml` rates. `clock_cost_us` is the cost of
    /// one clock read, measured by [`clock_cost`].
    pub fn new(defaults: Rates, clock_cost_us: f64) -> Sampler {
        Sampler {
            rows: Vec::new(),
            free: Vec::new(),
            by_object: HashMap::new(),
            dead: HashSet::new(),
            kept: HashMap::new(),
            pending: HashMap::new(),
            pending_order: VecDeque::new(),
            pending_expiry: VecDeque::new(),
            unpaused_s: 0.0,
            last_model: None,
            pending_deaths: 0,
            pending_overflow: 0,
            pending_expired: 0,
            gone_dead: 0,
            gone_removed: 0,
            gone_missing: 0,
            classifier_anomalies: 0,
            classifier_faults: 0,
            checks: VecDeque::new(),
            named: Vec::new(),
            revivals: 0,
            marks_cleared: 0,
            enum_end_abandoned: 0,
            end_hold: None,
            handed: Vec::new(),
            promoted: HashMap::new(),
            generation: 0,
            campaign: 0,
            defaults,
            rates: defaults,
            tiers: Default::default(),
            budget: Budget::default(),
            window: Window::default(),
            batch: Batch::default(),
            rows_per_clock: 1,
            clock_cost_us,
            since_clock: 0,
            births: 0,
            airbases: 0,
            enumeration: None,
            enum_count: 0,
            enum_objects: 0,
            enum_due: true,
            enum_last_begin: None,
            enum_queued: false,
            full: true,
            enum_full: true,
            drops_seen: 0,
            seen_ids: Vec::new(),
            seen_n: 0,
            enum_seen_ids: 0,
            enum_ended_begins: (None, None),
            sub_entities: HashSet::new(),
            sub_period: 1.0,
            sub_attitude: false,
            sub_slots: Vec::new(),
            sub_dirty: false,
            stopped: Vec::new(),
            dirty: false,
            hellos_seen: 0,
            sidecar_seen: 0,
            last_frame: None,
            sampling: false,
            pump_start: None,
            max_batch: MAX_BATCH,
            decided: None,
            xcall_us: 0.0,
            close_us: 0.0,
            batch_us: 0.0,
            tail_us: 0.0,
            close_dev_us: 0.0,
            maintenance_us: 0.0,
            maintenance_earned: 0.0,
            maintenance_paid: 0.0,
            maintenance_bound: -MAINTENANCE_FLOOR_US,
            pump_call_us: 0.0,
            decided_commanded: 0.0,
            excused_us: 0.0,
            pump_rows: false,
            pump_paid: 0.0,
            last_began: None,
            pumps: 0,
            empty_pumps: 0,
            urgent: VecDeque::new(),
            planned: [false; 7],
            unaffordable: [None; 7],
            #[cfg(test)]
            no_guard: false,
            #[cfg(test)]
            vitals_cap_one_sweep: false,
            #[cfg(test)]
            vitals_due: 0.0,
            #[cfg(test)]
            vitals_swept: 0,
            last_planned: [None; 7],
            late_tier: None,
            x_decayed: false,
            sub_pump: false,
            pump_reserve: 0.0,
            frames: 0,
            batches: 0,
            step_at: None,
            steps: [[0; 32]; 2],
            step_max: [0.0; 2],

            scale: 1.0,
            starved: 0,
            lone: false,
            lone_rows: 0,
            maintenance: false,
            row_frames: 0,
            overrun_frames: 0,
            overrun_sum_us: 0.0,
            maintenance_frames: 0,
            maintenance_overrun: 0.0,
            checked: false,
            check_pumps: 0,
            check_overrun: 0.0,
            overrun_max: 0.0,
            units: 0,
            direct: false,
            sweep_spent: 0.0,
            sweep_fund: 0.0,
            sweep_row_us: 0.0,
            sweep_batch_us: 0.0,
            sweep_tail_us: 0.0,
            sweep_cut: 0,
            sweep_overruns: 0,
            sweep_overrun_max: 0.0,
            sweep_forced: 0,
            sweep_waits: [0; 5],
            sweep_need: 0.0,
            sweep_wait: None,
            last_sweep: None,
            sweeps: 0,
            last_pump: None,
            passes: [0; 2],
            slow_start: None,
            slow_period: 0.0,
            idle_units: 0,
            cargo: 0,
            idle_cargo: 0,
            vital_rows: 0,
            suspect_rows: 0,
            suspect_units: 0,
            suspect_cargo: 0,
            recount: false,
            life_marks: Vec::new(),
            loadouts: Vec::new(),
            loadouts_handed: Vec::new(),
            loadouts_dropped: 0,
            loadout_reads: 0,
            scratch: Vec::new(),
            xcall_seen: 0.0,
        }
    }

    // ---- the table -------------------------------------------------------

    /// Rows in the table.
    pub fn len(&self) -> usize {
        self.by_object.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.by_object.is_empty()
    }

    /// The slot of an object, if the table has it.
    pub fn slot_of(&self, object: u64) -> Option<u32> {
        self.by_object.get(&object).copied()
    }

    /// The shim's slot of the row the binding gives the unit or static
    /// `entity`: the slot `add` returned, one past the row's index.
    pub fn slot_of_entity(&self, entity: u64) -> Option<u32> {
        if entity == 0 {
            return None;
        }
        self.rows
            .iter()
            .position(|r| {
                r.as_ref()
                    .is_some_and(|r| r.entity == entity && r.found.object != 0)
            })
            .map(|index| index as u32 + 1)
    }

    /// The DCS group ID of the group the binding gives `entity`.
    pub fn group_id_of_entity(&self, entity: u64) -> Option<u64> {
        if entity == 0 {
            return None;
        }
        self.rows.iter().flatten().find_map(|r| {
            (r.group_entity == entity && r.found.object != 0 && r.found.group != 0)
                .then_some(r.found.group)
        })
    }

    /// The object in a slot.
    pub fn object_at(&self, slot: u32) -> Option<u64> {
        self.row(slot).map(|r| r.found.object)
    }

    /// Every object in the table, for tests and the oracle.
    pub fn objects(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.by_object.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// The entities of a row.
    pub fn entities(&self, object: u64) -> Option<(u64, u64)> {
        let slot = self.slot_of(object)?;
        self.row(slot).map(|r| (r.entity, r.group_entity))
    }

    fn row(&self, slot: u32) -> Option<&Row> {
        self.rows.get(slot as usize).and_then(Option::as_ref)
    }

    fn row_mut(&mut self, slot: u32) -> Option<&mut Row> {
        self.rows.get_mut(slot as usize).and_then(Option::as_mut)
    }

    /// The mission generation of the table.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Time the pump spent on commands before [`Sampler::begin`]: not the
    /// cross-state call's, so it does not count in its estimate.
    pub fn excuse(&mut self, spent: Duration) {
        if let Some(d) = self.decided.as_mut() {
            *d += spent;
            self.excused_us += spent.as_secs_f64() * 1e6;
        }
    }

    /// Time this frame spent on commands whose handler runs in Lua: not the
    /// sampler's, so it is out of the debt, the credit and the window share.
    pub fn excuse_commanded(&mut self, spent: Duration) {
        self.budget.commanded_us += spent.as_secs_f64() * 1e6;
    }

    /// The mission generation changed: every row of the earlier one is gone,
    /// with no record (spec/records.md, "Layout: LIFECYCLE"). The binding and
    /// the dead set go with them; an enumeration in progress ends with no
    /// `ENUM_END`, and a new one is due.
    pub fn new_generation(&mut self, generation: u32) {
        self.generation = generation;
        self.rows.clear();
        self.free.clear();
        self.by_object.clear();
        self.units = 0;
        self.dead.clear();
        self.kept.clear();
        self.pending.clear();
        self.pending_order.clear();
        self.pending_expiry.clear();
        self.unpaused_s = 0.0;
        self.last_model = None;
        self.checks.clear();
        self.urgent.clear();
        self.promoted.clear();
        self.end_hold = None;
        self.handed.clear();
        self.named.clear();
        self.sub_slots.clear();
        self.sub_dirty = true;
        self.stopped.clear();
        self.life_marks.clear();
        self.loadouts.clear();
        self.loadouts_handed.clear();
        self.idle_units = 0;
        self.cargo = 0;
        self.idle_cargo = 0;
        self.vital_rows = 0;
        self.suspect_rows = 0;
        self.suspect_units = 0;
        self.suspect_cargo = 0;
        self.enumeration = None;
        self.enum_queued = false;
        self.enum_due = true;
        // The open batch names rows of the generation that ended.
        self.seen_ids.clear();
        self.births = 0;
        self.airbases = 0;
        self.batch = Batch::default();
        for t in &mut self.tiers {
            t.cursor = 0;
            t.credit = 0.0;
            t.sweep_start = None;
            t.owed_since = None;
        }
        self.slow_start = None;
        self.slow_period = 0.0;
        self.sweep_fund = 0.0;
    }

    /// The active campaign changed: the binding is discarded, and each row
    /// keeps only what its name gives under the new campaign.
    pub fn set_campaign(&mut self, campaign: u64) {
        if campaign == self.campaign {
            return;
        }
        self.campaign = campaign;
        // A kept row's entities are the old campaign's: a late death event
        // must not name them under the new one.
        self.kept.clear();
        for row in self.rows.iter_mut().flatten() {
            row.entity = name_entity(&row.found.name, campaign, row.found.kind);
            row.group_entity = name_entity(&row.found.group_name, campaign, object_kind::GROUP);
            row.encoded.clear();
            row.bound_entity = false;
            row.bound_group = false;
        }
        self.sub_dirty = true;
    }

    /// Whether the table would take this object: it is not in the table.
    /// An object that died in this generation is taken again only when it
    /// lives (revival): the shim checks `isDead` first for an object
    /// [`Sampler::died`] names.
    pub fn wants(&self, object: u64) -> bool {
        !self.by_object.contains_key(&object) && !self.pending.contains_key(&object)
    }

    /// Whether the object left as `DEAD` in this generation.
    pub fn died(&self, object: u64) -> bool {
        self.dead.contains(&object)
    }

    /// An object the running enumeration found. When the table has it, marks
    /// it found, queues its `OBJECT` with the enumeration's number, and
    /// returns its slot plus one. Returns 0 when the table lacks it (the shim
    /// then reads it and calls [`Sampler::add`]; for an object that died in
    /// this generation it checks the object lives first), and `None` when no
    /// enumeration runs.
    pub fn seen(&mut self, link: &mut impl Port, object: u64) -> Option<u32> {
        let n = self.enumeration?;
        let Some(&slot) = self.by_object.get(&object) else {
            return Some(0);
        };
        let row = self.rows[slot as usize].as_mut()?;
        row.enum_seen = true;
        if std::mem::take(&mut row.suspect) {
            self.recount = true;
        }
        // An unchanged row the sidecar has this attach's `OBJECT` of goes in
        // a `SEEN` batch; every other row gets its `OBJECT` whole.
        if self.enum_full || row.encoded.is_empty() || row.sent != link.hellos() + 1 {
            self.enqueue_known(link, slot, n);
        } else {
            if self.seen_ids.is_empty() {
                self.seen_n = n;
            }
            self.seen_ids.push(object);
            self.enum_seen_ids += 1;
            if self.seen_ids.len() >= Seen::capacity(link.max_payload()).max(1) {
                self.flush_seen(link);
            }
        }
        Some(slot + 1)
    }

    /// Queue the open `SEEN` batch, if any.
    fn flush_seen(&mut self, link: &mut impl Port) {
        if self.seen_ids.is_empty() {
            return;
        }
        let payload = Seen {
            enumeration: self.seen_n,
            objects: std::mem::take(&mut self.seen_ids),
        }
        .encode();
        link.enqueue(kind::SEEN as u16, payload);
        self.dirty = true;
    }

    /// Queue an event-class record behind the open `SEEN` batch, which goes
    /// before every other event-class record (spec/records.md, "Layout: SEEN").
    fn put(&mut self, link: &mut impl Port, kind: u16, payload: Vec<u8>) {
        self.flush_seen(link);
        link.enqueue(kind, payload);
    }

    /// Queue the `OBJECT` of the row in `slot` with enumeration `n`'s number,
    /// and count it in the enumeration's objects.
    fn enqueue_known(&mut self, link: &mut impl Port, slot: u32, n: u64) {
        let Some(row) = self.rows.get_mut(slot as usize).and_then(Option::as_mut) else {
            return;
        };
        // The row's payload is encoded once; each enumeration patches its
        // number in (LIVE, S10: re-encoding every known row's strings on
        // every enumeration took most of its time).
        if row.encoded.is_empty() {
            row.encoded = object_record(row, 0).encode();
        }
        row.sent = link.hellos() + 1;
        let mut payload = row.encoded.clone();
        let at = Object::ENUMERATION_AT;
        payload[at..at + 8].copy_from_slice(&n.to_le_bytes());
        self.enum_objects += 1;
        self.put(link, kind::OBJECT as u16, payload);
        self.dirty = true;
    }

    /// Add a row for an object the shim read. `enumerated` when the running
    /// enumeration found it. Returns its slot plus one, or 0 when the table
    /// already has it. An object of the dead set is a revival: the shim found
    /// it existing and not dead, so it leaves the dead set and is added again
    /// with the same entity.
    pub fn add(&mut self, link: &mut impl Port, found: Found, enumerated: bool) -> u32 {
        if !self.wants(found.object) || found.object == 0 {
            return 0;
        }
        let object = found.object;
        self.kept.remove(&object);
        if self.dead.remove(&object) {
            self.revivals += 1;
        }
        let kind_of = found.kind;
        let entity = name_entity(&found.name, self.campaign, found.kind);
        let group_entity = name_entity(&found.group_name, self.campaign, object_kind::GROUP);
        let n = if enumerated { self.enumeration } else { None };
        // Added after the enumeration began: never removed by it
        // (`enum_member` 0).
        let row = Row::new(found, entity, group_entity, 0);
        let slot = match self.free.pop() {
            Some(s) => s,
            None => {
                self.rows.push(None);
                (self.rows.len() - 1) as u32
            }
        };
        let version = self.rows[slot as usize]
            .as_ref()
            .map_or(0, |r| r.version.wrapping_add(1));
        let record = object_record(&row, n.unwrap_or(0));
        if row.vital() {
            self.vital_rows += 1;
        }
        if !row.unit() && row.found.movable {
            self.cargo += 1;
        }
        // A new row's life is read on the next pump: a row no
        // subscription names has no other read until an event names it or
        // the slow sweep reaches it.
        self.life_marks.push(slot);
        self.rows[slot as usize] = Some(Row {
            version,
            life_marked: true,
            sent: link.hellos() + 1,
            ..row
        });
        self.by_object.insert(object, slot);
        if kind_of == object_kind::UNIT {
            self.units += 1;
        }
        if self.sub_entities.contains(&entity) || self.sub_entities.contains(&group_entity) {
            self.sub_dirty = true;
        }
        if n.is_some() {
            self.enum_objects += 1;
        }
        self.enqueue_object(link, record);
        slot + 1
    }

    fn enqueue_object(&mut self, link: &mut impl Port, record: Object) {
        self.put(link, kind::OBJECT as u16, record.encode());
        self.dirty = true;
    }

    /// A death event, with its initiator and the initiator's category. A
    /// row in the table is not removed: its existence is checked on the next
    /// pump, and if it is gone it leaves as `DEAD`. An object whose row left
    /// as `MISSING` or `REMOVED` gets a second `OBJECT_GONE`, `DEAD`. Returns
    /// the slot plus one of a row removed: always 0.
    pub fn death(
        &mut self,
        link: &mut impl Port,
        object: Option<f64>,
        category: Option<f64>,
    ) -> u32 {
        let (Some(object), Some(category)) = (object, category) else {
            return 0;
        };
        if ![CATEGORY_UNIT, CATEGORY_STATIC, CATEGORY_CARGO].contains(&category) {
            return 0;
        }
        if !(0.0..9.007_199_254_740_992e15).contains(&object) {
            return 0;
        }
        let object = object as u64;
        if let Some(&slot) = self.by_object.get(&object) {
            if let Some(row) = self.rows[slot as usize].as_mut() {
                row.death_named = true;
            }
            self.check_soon(slot);
            self.want_check(object);
            return 0;
        }
        if self.pending.contains_key(&object) {
            // The event is forwarded; it removes nothing.
            self.pending_deaths += 1;
            return 0;
        }
        // A late event: the row left as `MISSING` or `REMOVED` and DCS now
        // reports the death.
        if let Some(kept) = self.kept.remove(&object) {
            let record = ObjectGone {
                object,
                reason: gone::DEAD,
                entity: kept.entity,
                group_entity: kept.group_entity,
            };
            self.dead.insert(object);
            if kept.reason == gone::REMOVED {
                self.classifier_faults += 1;
            }
            self.put(link, kind::OBJECT_GONE as u16, record.encode());
        }
        0
    }

    /// `isExist` was false for the row of `object`, and `dead` and `life` are
    /// what `Unit.isDead` and `Unit.getLife` gave on its ID. Removes the row
    /// and returns the slot plus one, or 0 when the table lacks the object.
    /// [`rule`] says, by the row's class, which reads count; a row a death
    /// event marked before its check leaves as `DEAD` whatever they gave.
    pub fn gone(
        &mut self,
        link: &mut impl Port,
        object: u64,
        dead: Read<Option<bool>>,
        life: Read<Option<f64>>,
    ) -> u32 {
        // A pending-destroy object has no row, but the shim may still hold
        // its handle, so its check ends here: it no longer exists.
        // The check rotation and the expiry queue skip the stale ID.
        self.pending.remove(&object);
        let Some(row) = self.slot_of(object).and_then(|s| self.present(s)) else {
            return 0;
        };
        let (mut reason, mut anomaly) = classify(rule(class_of(row)), &dead, &life);
        let mut fault = false;
        if let Some(i) = self.named.iter().position(|o| *o == object) {
            // A death event's mark and `isExist` false: a death fact in
            // every class. A mark over reads that showed a removal is a
            // classifier fault; over any other result but DEAD, an anomaly.
            self.named.swap_remove(i);
            match reason {
                gone::DEAD => {}
                gone::REMOVED => fault = true,
                _ => anomaly = true,
            }
            reason = gone::DEAD;
        }
        let freed = self.remove(link, object, reason);
        if freed != 0 {
            match reason {
                gone::DEAD => self.gone_dead += 1,
                gone::REMOVED => self.gone_removed += 1,
                _ => self.gone_missing += 1,
            }
            if anomaly {
                self.classifier_anomalies += 1;
            }
            if fault {
                self.classifier_faults += 1;
            }
        }
        freed
    }

    /// Objects of the dead set found alive and added again, since start.
    pub fn revivals(&self) -> u64 {
        self.revivals
    }

    /// Death-event marks cleared because the row was alive at its check,
    /// since start.
    pub fn marks_cleared(&self) -> u64 {
        self.marks_cleared
    }

    /// Enumerations whose `ENUM_END` was held past one enumeration period
    /// and abandoned, since start.
    pub fn enum_end_abandoned(&self) -> u64 {
        self.enum_end_abandoned
    }

    /// Whether an `ENUM_END` is held for misses still being checked.
    pub fn end_held(&self) -> bool {
        self.end_hold.is_some()
    }

    /// The existence check of `object` found it existing (`sure`), or could
    /// not decide: `isExist` raised, or the shim holds no object for it. A
    /// survivor's death-event mark is cleared and counted; an undecided
    /// check leaves the mark on the row for the next check. A miss of the
    /// enumeration whose `ENUM_END` is held gets its `OBJECT` with that
    /// enumeration's number.
    pub fn alive(&mut self, link: &mut impl Port, object: u64, sure: bool) {
        if !sure && self.pending.remove(&object).is_some() {
            // `isExist` gave false or raised: DCS removed it, or it is
            // beyond saying.
            // The check rotation and the expiry queue skip the stale ID.
        }
        if let Some(i) = self.named.iter().position(|o| *o == object) {
            self.named.swap_remove(i);
            if sure {
                self.marks_cleared += 1;
            } else if let Some(&slot) = self.by_object.get(&object)
                && let Some(row) = self.rows[slot as usize].as_mut()
            {
                row.death_named = true;
            }
        }
        self.resolve_miss(link, object, true);
    }

    /// Put the marks the last checks handed out, and no check decided, back
    /// on their rows.
    fn keep_marks(&mut self) {
        for object in std::mem::take(&mut self.named) {
            if let Some(&slot) = self.by_object.get(&object)
                && let Some(row) = self.rows[slot as usize].as_mut()
            {
                row.death_named = true;
            }
        }
    }

    /// A miss of the held enumeration is resolved: `kept` when its row stays
    /// (found alive, or undecided), which gives it an `OBJECT`; when its row
    /// left, the `OBJECT_GONE` was queued already. The last one releases the
    /// `ENUM_END`.
    fn resolve_miss(&mut self, link: &mut impl Port, object: u64, kept: bool) {
        let Some(hold) = self.end_hold.as_mut() else {
            return;
        };
        if !hold.pending.remove(&object) {
            return;
        }
        let n = hold.n;
        let done = hold.pending.is_empty();
        if kept && let Some(&slot) = self.by_object.get(&object) {
            if let Some(row) = self.rows[slot as usize].as_mut() {
                row.enum_seen = true;
            }
            self.enqueue_known(link, slot, n);
        }
        if done {
            self.end_hold = None;
            self.finish_enumeration(link, n);
        }
    }

    /// Queue `ENUM_END` for enumeration `n`.
    fn finish_enumeration(&mut self, link: &mut impl Port, n: u64) {
        self.put(
            link,
            kind::ENUM_END as u16,
            EnumEnd {
                enumeration: n,
                objects: self.enum_objects,
                seen: self.enum_seen_ids,
            }
            .encode(),
        );
        self.dirty = true;
        if std::mem::take(&mut self.enum_queued) {
            self.enum_due = true;
        }
    }

    /// The sidecar detached: a held `ENUM_END` is dropped, with no record.
    pub fn detached(&mut self) {
        self.drop_hold();
    }

    /// Drop a held `ENUM_END`; a queued enumeration request stays due.
    fn drop_hold(&mut self) {
        if self.end_hold.take().is_some() {
            self.handed.clear();
            if std::mem::take(&mut self.enum_queued) {
                self.enum_due = true;
            }
        }
    }

    /// Cumulative counts since start: rows [`Sampler::gone`] removed as
    /// `DEAD`, `REMOVED` and `MISSING`, rows it could not decide, and
    /// `REMOVED` rows a death event later showed to be kills.
    pub fn gone_counts(&self) -> [u64; 5] {
        [
            self.gone_dead,
            self.gone_removed,
            self.gone_missing,
            self.classifier_anomalies,
            self.classifier_faults,
        ]
    }

    /// Queue the row in `slot` for an `isExist` check, once.
    fn check_soon(&mut self, slot: u32) {
        let Some(row) = self.rows.get_mut(slot as usize).and_then(Option::as_mut) else {
            return;
        };
        if row.found.object != 0 && !std::mem::replace(&mut row.checking, true) {
            self.checks.push_back(row.found.object);
        }
    }

    /// A HIT, BDA or KILL event named `object` as its target: its existence
    /// is checked once, on the next pump. A survivor is not checked again.
    pub fn watch(&mut self, object: Option<f64>) {
        let Some(object) = object.filter(|o| (0.0..9.007_199_254_740_992e15).contains(o)) else {
            return;
        };
        if let Some(&slot) = self.by_object.get(&(object as u64)) {
            self.check_soon(slot);
            self.want_check(object as u64);
        }
    }

    /// An event asked for this object's existence check: the next pump the
    /// gate allows makes it.
    fn want_check(&mut self, object: u64) {
        // Only a row the queue holds is urgent: one with no entry to consume
        // it would hold `pump_worth` for the generation.
        let Some(&slot) = self.by_object.get(&object) else {
            return;
        };
        let Some(row) = self.rows[slot as usize].as_mut() else {
            return;
        };
        if !row.checking || row.urgent {
            return;
        }
        // It goes ahead of the slow sweep's checks, after the other urgent
        // entries, which keep their order. Its entry in `checks` is skipped
        // when it comes up.
        row.urgent = true;
        *self.promoted.entry(object).or_insert(0) += 1;
        self.urgent.push_back(object);
    }

    /// Fill `out` with at most `CHECKS_PER_PUMP` object IDs whose `isExist`
    /// this pump calls, in the order they were queued, and return their
    /// number. Each queued row is checked once; what does not fit waits for
    /// the next call, so no entry is dropped unchecked. Nothing when the pump
    /// is not sampling.
    pub fn checks(&mut self, out: &mut Vec<u64>) -> usize {
        out.clear();
        // Marks the last checks handed out that neither `gone` nor `alive`
        // decided stay on their rows for the next check.
        self.keep_marks();
        if !self.sampling {
            return 0;
        }
        // The checks cost about a vitals row each: a pump makes as many as
        // its budget pays for, at least one, and the rest wait.
        let cost = self.tiers[Tier::Vitals.index()].cost;
        let afford = if cost > 0.0 {
            (self.budget.budget.max(0.0) / cost) as usize
        } else {
            CHECKS_PER_PUMP
        };
        let most = afford.clamp(1, CHECKS_PER_PUMP);
        while out.len() < most {
            let object = if let Some(o) = self.urgent.pop_front() {
                o
            } else {
                let Some(o) = self.checks.pop_front() else {
                    break;
                };
                if let Some(c) = self.promoted.get_mut(&o) {
                    *c -= 1;
                    if *c == 0 {
                        self.promoted.remove(&o);
                    }
                    continue;
                }
                o
            };
            // A miss handed out, or whose row is no longer held, is resolved
            // when the pump ends.
            if self
                .end_hold
                .as_ref()
                .is_some_and(|h| h.pending.contains(&object))
            {
                self.handed.push(object);
            }
            let Some(&slot) = self.by_object.get(&object) else {
                continue;
            };
            let Some(row) = self.rows[slot as usize].as_mut() else {
                continue;
            };
            row.checking = false;
            row.urgent = false;
            if std::mem::take(&mut row.death_named) {
                self.named.push(object);
            }
            out.push(object);
        }
        // The pending-destroy objects take what the queue left.
        let mut turns = self.pending_order.len().min(PENDING_PER_PUMP);
        while turns > 0 && out.len() < most {
            turns -= 1;
            let Some(o) = self.pending_order.pop_front() else {
                break;
            };
            if self.pending.contains_key(&o) {
                self.pending_order.push_back(o);
                out.push(o);
            }
        }
        self.checked |= !out.is_empty();
        out.len()
    }

    /// A birth or a takeoff event named `object` as its initiator: the next
    /// pump reads the unit's ammunition if a player occupies it
    /// (spec/sampling.md, "Loadout reads"). `cause` is [`Ammo::BIRTH`] or
    /// [`Ammo::TAKEOFF`]. A birth's row does not exist yet, so the mark keeps
    /// the object ID and waits for the row. An object whose row is not a unit
    /// is ignored; a second event for an object already marked replaces its
    /// cause.
    pub fn mark_loadout(&mut self, object: Option<f64>, cause: u32) {
        let Some(object) = object.filter(|o| (0.0..9.007_199_254_740_992e15).contains(o)) else {
            return;
        };
        let object = object as u64;
        if self
            .by_object
            .get(&object)
            .and_then(|&slot| self.rows[slot as usize].as_ref())
            .is_some_and(|row| !row.unit())
        {
            return;
        }
        if let Some(mark) = self.loadouts.iter_mut().find(|m| m.object == object) {
            mark.cause = cause;
        } else {
            // A full list drops its oldest mark: a player's slot-in comes
            // after the AI births of a large spawn.
            if self.loadouts.len() >= LOADOUT_MARKS_MAX {
                self.loadouts.remove(0);
                self.loadouts_dropped += 1;
            }
            self.loadouts.push(LoadoutMark {
                object,
                cause,
                waits: 0,
            });
        }
    }

    /// Loadout marks waiting for a pump.
    pub fn loadouts_waiting(&self) -> usize {
        self.loadouts.len()
    }

    /// Loadout marks dropped because the list was full.
    pub fn loadouts_dropped(&self) -> u64 {
        self.loadouts_dropped
    }

    /// Loadout reads made since start, a retry after a dropped `AMMO`
    /// counted again.
    pub fn loadout_reads(&self) -> u64 {
        self.loadout_reads
    }

    /// The slot of the unit row a loadout mark names, once its row exists.
    fn loadout_slot(&self, object: u64) -> Option<u32> {
        let &slot = self.by_object.get(&object)?;
        let row = self.rows[slot as usize].as_ref()?;
        (row.unit() && !row.suspect).then_some(slot)
    }

    /// Whether a loadout mark has its row and no birth waits: a pump has a
    /// read to make. Births come first, so a pump a birth waits for is not
    /// worth its call for a mark.
    fn loadouts_ready(&self) -> bool {
        self.births == 0
            && self
                .loadouts
                .iter()
                .any(|m| self.loadout_slot(m.object).is_some())
    }

    /// Fill `out` with at most `LOADOUTS_PER_PUMP` pairs of a unit's slot and
    /// the cause of its read, and return how many. A pump hands out as many
    /// as the budget left after the tiers pays for at a vitals row's cost
    /// each, at least one. Births come first: while a birth waits for the
    /// pump, nothing is handed out, and the marks wait with their waits
    /// unchanged. The marks of rows that do not exist yet wait; one that
    /// waits `LOADOUT_WAIT_PUMPS` pumps with no birth pending is dropped.
    /// Nothing when the pump is not sampling.
    pub fn loadouts(&mut self, out: &mut Vec<(u32, u32)>) -> usize {
        out.clear();
        self.loadouts_handed.clear();
        if !self.sampling || self.loadouts.is_empty() || self.births > 0 {
            return 0;
        }
        let cost = self.tiers[Tier::Vitals.index()].cost;
        let afford = if cost > 0.0 {
            (self.remaining_us(Instant::now()).max(0.0) / cost) as usize
        } else {
            LOADOUTS_PER_PUMP
        };
        let most = afford.clamp(1, LOADOUTS_PER_PUMP);
        let marks = std::mem::take(&mut self.loadouts);
        let mut kept = Vec::with_capacity(marks.len());
        for mut mark in marks {
            match self.loadout_slot(mark.object) {
                Some(slot) if out.len() < most => {
                    out.push((slot, mark.cause));
                    self.loadouts_handed.push(mark);
                }
                // A row that exists waits its turn.
                Some(_) => kept.push(mark),
                None => {
                    mark.waits += 1;
                    if mark.waits <= LOADOUT_WAIT_PUMPS && !self.has_other_row(mark.object) {
                        kept.push(mark);
                    }
                }
            }
        }
        self.loadouts = kept;
        self.checked |= !out.is_empty();
        out.len()
    }

    /// Whether the object has a row that is not a unit, a static. A unit row
    /// marked suspect is a unit: its mark waits for the row to clear.
    fn has_other_row(&self, object: u64) -> bool {
        self.by_object
            .get(&object)
            .and_then(|&slot| self.rows[slot as usize].as_ref())
            .is_some_and(|row| !row.unit())
    }

    /// A player-occupied unit's ammunition: pairs of type name and count,
    /// read for the event `cause` named. Writes one `AMMO` record.
    pub fn ammo(&mut self, link: &mut impl Port, slot: u32, cause: u32, items: &[(Vec<u8>, f64)]) {
        let Some(row) = self.present(slot).filter(|r| r.unit()) else {
            return;
        };
        let object = row.found.object;
        self.loadout_reads += 1;
        let list = items
            .iter()
            .enumerate()
            .map(|(i, (t, c))| {
                (
                    Lv::Number((i + 1) as f64),
                    Lv::Table(vec![
                        (Lv::String(b"type".to_vec()), Lv::String(t.clone())),
                        (Lv::String(b"count".to_vec()), Lv::Number(*c)),
                    ]),
                )
            })
            .collect();
        let payload = Ammo {
            object,
            cause,
            ammo: Lv::Table(list),
        }
        .encode();
        if payload.len() > link.max_payload() {
            return;
        }
        // The `OBJECT` of a row the pump just read goes before its sample.
        if self.dirty {
            link.flush();
            self.dirty = false;
        }
        if !link.sample(kind::AMMO as u16, &payload) {
            self.loadout_retry(object);
        }
    }

    /// The ring dropped a unit's `AMMO`: its mark goes back, so a later pump
    /// reads the unit again. Each retry counts as a pump that waited, so the
    /// retries end with `LOADOUT_WAIT_PUMPS`; a newer mark for the unit
    /// stands.
    fn loadout_retry(&mut self, object: u64) {
        let Some(i) = self.loadouts_handed.iter().position(|m| m.object == object) else {
            return;
        };
        let mut mark = self.loadouts_handed.swap_remove(i);
        mark.waits += 1;
        if mark.waits <= LOADOUT_WAIT_PUMPS
            && self.loadouts.len() < LOADOUT_MARKS_MAX
            && !self.loadouts.iter().any(|m| m.object == object)
        {
            self.loadouts.push(mark);
        }
    }

    /// The command destroyed these objects: each row held leaves as
    /// `DESTROYED` at once, before the command's `RESULT`, and every ID
    /// enters the pending-destroy set (spec/commands.md, "A destroy while
    /// paused is pending").
    pub fn destroyed(&mut self, link: &mut impl Port, ids: &[u64]) {
        for &object in ids {
            self.remove(link, object, gone::DESTROYED);
            self.kept.remove(&object);
            if self.pending.contains_key(&object) {
                continue;
            }
            if self.pending.len() >= PENDING_CAP {
                self.pending_overflow += 1;
                continue;
            }
            self.pending.insert(object, self.unpaused_s);
            self.pending_order.push_back(object);
            self.pending_expiry.push_back((self.unpaused_s, object));
        }
    }

    /// Count IDs a `destroyed` array held past the read limit; they cannot
    /// enter the pending set and leave through the existence checks.
    pub fn add_pending_overflow(&mut self, n: u64) {
        self.pending_overflow += n;
    }

    /// The frame's model time: pending objects still existing after
    /// [`PENDING_EXPIRY_S`] unpaused seconds are dropped.
    pub fn model_time(&mut self, model: f64, paused: bool) {
        let before = self.last_model.replace(model);
        if paused || self.pending.is_empty() {
            return;
        }
        if let Some(before) = before {
            self.unpaused_s += (model - before).clamp(0.0, 1.0e6);
        }
        let now = self.unpaused_s;
        while let Some(&(since, object)) = self.pending_expiry.front() {
            if self.pending.get(&object) != Some(&since) {
                // Already removed (or re-entered later): a stale entry.
                self.pending_expiry.pop_front();
                continue;
            }
            if now - since < PENDING_EXPIRY_S {
                break;
            }
            self.pending_expiry.pop_front();
            self.pending.remove(&object);
            self.pending_expired += 1;
        }
    }

    /// Objects in the pending-destroy set.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Cumulative: `DEAD` events that named a pending object, objects over
    /// the set's capacity, objects dropped at the backstop.
    pub fn pending_counts(&self) -> [u64; 3] {
        [
            self.pending_deaths,
            self.pending_overflow,
            self.pending_expired,
        ]
    }

    /// Existence checks waiting for a pump.
    pub fn checks_waiting(&self) -> usize {
        let stale: usize = self.promoted.values().map(|c| *c as usize).sum();
        (self.checks.len() + self.urgent.len()).saturating_sub(stale)
    }

    /// Whether the row in `slot` is one whose `isExist` the vitals tier checks
    /// when it reads it: a row no subscription names.
    pub fn slow_vitals_row(&self, slot: u32) -> bool {
        self.present(slot).is_some_and(|r| !r.vital())
    }

    /// Remove a row and queue its `OBJECT_GONE` with the binding it had.
    /// Returns the slot plus one, or 0 when the table lacks the object.
    pub fn remove(&mut self, link: &mut impl Port, object: u64, reason: u32) -> u32 {
        let Some(slot) = self.by_object.remove(&object) else {
            return 0;
        };
        let Some(row) = self.rows[slot as usize].as_mut() else {
            return 0;
        };
        if row.unit() {
            self.units = self.units.saturating_sub(1);
        } else if row.found.movable {
            self.cargo = self.cargo.saturating_sub(1);
        }
        // The other counts hold only rows that are read; a suspect row left
        // them when it became suspect.
        self.recount = true;
        let gone_record = ObjectGone {
            object,
            reason,
            entity: row.entity,
            group_entity: row.group_entity,
        };
        // Keep the generation for the slot's next row; drop the rest.
        let version = row.version;
        self.rows[slot as usize] = Some(Row::new(Found::default(), 0, 0, version));
        // A freed slot holds a placeholder row with object 0 and is not in
        // by_object, so no tier reads it.
        self.free.push(slot);
        if reason == gone::DEAD {
            self.dead.insert(object);
        }
        if reason == gone::MISSING || reason == gone::REMOVED {
            self.kept.insert(
                object,
                Kept {
                    entity: gone_record.entity,
                    group_entity: gone_record.group_entity,
                    reason,
                },
            );
        }
        self.sub_slots.retain(|s| *s != slot);
        self.stopped.retain(|s| *s != slot);
        self.life_marks.retain(|s| *s != slot);
        self.loadouts.retain(|m| m.object != object);
        self.loadouts_handed.retain(|m| m.object != object);
        self.put(link, kind::OBJECT_GONE as u16, gone_record.encode());
        self.resolve_miss(link, object, false);
        slot + 1
    }

    /// The row in a slot, when the slot holds an object. A freed slot holds
    /// a placeholder with object 0.
    fn present(&self, slot: u32) -> Option<&Row> {
        self.row(slot).filter(|r| r.found.object != 0)
    }

    /// An event named an object as hit: its life is read on a coming pump
    /// (spec/sampling.md, "Tiers"). An object the table lacks is ignored.
    pub fn mark(&mut self, object: Option<f64>) {
        let Some(object) = object.filter(|o| (0.0..9.007_199_254_740_992e15).contains(o)) else {
            return;
        };
        let Some(&slot) = self.by_object.get(&(object as u64)) else {
            return;
        };
        let Some(row) = self.rows[slot as usize].as_mut() else {
            return;
        };
        if !std::mem::replace(&mut row.life_marked, true) {
            self.life_marks.push(slot);
        }
    }

    /// A birth the shim holds for the next pump: counted here, so the frame
    /// knows a pump has work before the shim says how many it holds.
    pub fn birth_waits(&mut self) {
        self.births = (self.births + 1).min(MAX_BIRTHS);
    }

    /// Count again what the rows' flags give: after a subscription changed,
    /// a row was removed, or a row became suspect or was settled. A suspect
    /// row is read by no tier, so it is in none of the counts that set a
    /// tier's rows.
    fn tally(&mut self) {
        self.recount = false;
        let (mut idle_units, mut idle_cargo, mut vital) = (0, 0, 0);
        let (mut out, mut out_units, mut out_cargo) = (0, 0, 0);
        for row in self.rows.iter().flatten().filter(|r| r.found.object != 0) {
            if row.suspect {
                out += 1;
                out_units += usize::from(row.unit());
                out_cargo += usize::from(!row.unit() && row.found.movable);
                continue;
            }
            if row.unit() {
                idle_units += usize::from(row.idle());
            } else if row.found.movable {
                idle_cargo += usize::from(row.idle());
            }
            vital += usize::from(row.vital());
        }
        self.idle_units = idle_units;
        self.idle_cargo = idle_cargo;
        self.vital_rows = vital;
        self.suspect_rows = out;
        self.suspect_units = out_units;
        self.suspect_cargo = out_cargo;
    }

    fn live(&self, slot: u32) -> Option<&Row> {
        self.row(slot)
            .filter(|r| r.found.object != 0 && self.by_object.get(&r.found.object) == Some(&slot))
    }

    /// `BIND`: give entities to rows. A pair whose object (or group) is not
    /// in the table is ignored, and so is a row whose name binds it.
    pub fn bind(&mut self, kind: u32, pairs: &[(u64, u64)]) {
        let campaign = self.campaign;
        for &(entity, id) in pairs {
            match kind {
                object_kind::UNIT | object_kind::STATIC => {
                    if let Some(&slot) = self.by_object.get(&id)
                        && let Some(row) = self.rows[slot as usize].as_mut()
                        && name_entity(&row.found.name, campaign, row.found.kind) == 0
                    {
                        row.entity = entity;
                        row.bound_entity = true;
                        row.encoded.clear();
                    }
                }
                object_kind::GROUP => {
                    for row in self.rows.iter_mut().flatten() {
                        if row.found.object != 0
                            && row.found.group == id
                            && name_entity(&row.found.group_name, campaign, object_kind::GROUP) == 0
                        {
                            row.group_entity = entity;
                            row.encoded.clear();
                            row.bound_group = true;
                        }
                    }
                }
                _ => {}
            }
        }
        self.sub_dirty = true;
    }

    /// `SUBSCRIBE`: replace the subscribed set.
    pub fn subscribe(&mut self, period_s: f64, attitude: bool, entities: &[u64]) {
        self.sub_entities = entities.iter().copied().filter(|e| *e != 0).collect();
        self.sub_period = if period_s.is_finite() && period_s > 0.0 {
            period_s
        } else {
            self.rates.position_s
        };
        self.sub_attitude = attitude;
        self.sub_dirty = true;
        self.tiers[Tier::Subscribed.index()] = TierState::default();
    }

    /// `RATES`: set the target periods; 0 (or anything not a positive
    /// number) keeps the `agent.toml` default.
    pub fn set_rates(&mut self, rates: Rates) {
        let pick = |v: f64, d: f64| if v.is_finite() && v > 0.0 { v } else { d };
        self.rates = Rates {
            position_s: pick(rates.position_s, self.defaults.position_s),
            vitals_s: pick(rates.vitals_s, self.defaults.vitals_s),
            warehouse_s: pick(rates.warehouse_s, self.defaults.warehouse_s),
            enumeration_s: pick(rates.enumeration_s, self.defaults.enumeration_s),
        };
    }

    /// The rates in force.
    pub fn rates(&self) -> Rates {
        self.rates
    }

    /// `ENUMERATE`: start an enumeration, or one more after the one running.
    pub fn request_enumeration(&mut self) {
        // An enumeration that follows a request, a `HELLO` or a drop writes
        // every row whole; the flag holds until `ENUM_BEGIN` takes it.
        self.full = true;
        if self.enumeration.is_some() || self.end_hold.is_some() {
            self.enum_queued = true;
        } else {
            self.enum_due = true;
        }
    }

    fn rebuild_subs(&mut self) {
        if !self.sub_dirty {
            return;
        }
        self.sub_dirty = false;
        let mut slots: Vec<u32> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let r = r.as_ref()?;
                (r.found.object != 0
                    && (self.sub_entities.contains(&r.entity)
                        || self.sub_entities.contains(&r.group_entity)))
                .then_some(i as u32)
            })
            .collect();
        slots.sort_unstable();
        for row in self.rows.iter_mut().flatten() {
            row.watched = false;
        }
        for slot in &slots {
            if let Some(row) = self.rows[*slot as usize].as_mut() {
                row.watched = true;
            }
        }
        self.sub_slots = slots;
        self.tally();
    }

    // ---- the frame -------------------------------------------------------

    /// Count the agent's time for one world event handled between frames.
    pub fn add_event_time(&mut self, us: f64) {
        if us.is_finite() && us > 0.0 {
            self.budget.events_us += us;
            self.window.events_us += us;
        }
    }

    /// The shim tells how many births it holds and how many airbases the
    /// mission has.
    pub fn set_pending(&mut self, births: usize, airbases: usize) {
        self.births = births.min(MAX_BIRTHS);
        self.airbases = airbases;
    }

    /// A new `HELLO` was written, or a sidecar of another boot attached:
    /// an enumeration is due, and a new sidecar drops the old one's rates and
    /// subscriptions (spec/transport.md, "Attaching").
    fn check_attach(&mut self, link: &impl Port) {
        let hellos = link.hellos();
        if hellos == self.hellos_seen {
            return;
        }
        self.hellos_seen = hellos;
        // A held `ENUM_END` belongs to the sidecar that left.
        self.drop_hold();
        self.request_enumeration();
        // A sidecar that attaches holds no samples. Every row is read as a
        // new one is: its position on the coming sweeps, whether or not it
        // has moved, and its life on a coming pump.
        for (slot, row) in self.rows.iter_mut().enumerate() {
            let Some(row) = row.as_mut().filter(|r| r.found.object != 0) else {
                continue;
            };
            row.unmoved = 0;
            if !row.vital() && !std::mem::replace(&mut row.life_marked, true) {
                self.life_marks.push(slot as u32);
            }
        }
        self.recount = true;
        let boot = link.served_boot();
        if boot != self.sidecar_seen {
            self.sidecar_seen = boot;
            self.rates = self.defaults;
            self.subscribe(0.0, false, &[]);
        }
    }

    /// Decide, at `now` in the link's frame, whether this frame calls the
    /// shim's pump: work out the frame's slice, and whether what it leaves
    /// for sampling, after the work so far and the expected cost of the
    /// cross-state call, is worth the call (spec/sampling.md, "Slice"). The
    /// host makes the call only on a sampling frame, and then calls
    /// [`Sampler::begin`] first thing in the pump.
    pub fn decide(&mut self, link: &impl Port, now: Instant) -> bool {
        self.slice(link, now);
        self.gate(link, now)
    }

    /// The first part of [`Sampler::decide`]: the frame's slice. A host that
    /// sweeps positions itself calls this, then the sweep, then
    /// [`Sampler::gate`], so the gate sees the sweep's time as fixed work.
    pub fn slice(&mut self, link: &impl Port, now: Instant) {
        self.check_attach(link);
        // The backstop: a hold that lasts one enumeration period is
        // abandoned with no `ENUM_END`. The sidecar counts an open
        // enumeration refused when the next `ENUM_BEGIN` arrives and wants
        // another; a HELLO abandons it without counting.
        if self.end_hold.as_ref().is_some_and(|h| {
            now.saturating_duration_since(h.since).as_secs_f64() >= self.rates.enumeration_s
        }) {
            self.drop_hold();
            self.enum_end_abandoned += 1;
        }
        self.x_decayed = false;
        let share = link.overhead_share();
        let interval = link.frame_interval_us();
        self.maintenance_bound = (share * SECOND_US).max(-MAINTENANCE_FLOOR_US);
        let b = &mut self.budget;
        b.allowance = share * interval;
        b.charge = b.syscall_per_s * interval / 1e6;
        // A negative debt is credit: allowance earned on frames that did
        // not pump, spent on one that does (spec/sampling.md, "Slice").
        b.repay = b.debt.max(0.0).min(b.allowance / 2.0);
        // After a frame interval over twice the average, such as a stall,
        // the frame spends only its plain slice (spec/sampling.md, "Slice").
        let began = link.began();
        let slow = self.last_began.is_some_and(|l| {
            began.saturating_duration_since(l).as_secs_f64() * 1e6 > 2.0 * interval
        });
        self.last_began = Some(began);
        let credit = if slow { 0.0 } else { (-b.debt).max(0.0) };
        // The frame passes no more than the cap: the slice leaves room for
        // the frame's finish, closing the last batch and writing its record.
        let cap = self.slice_cap();
        let b = &mut self.budget;
        b.slice = (b.allowance - b.charge - b.repay + credit).min(cap);
        self.decided = Some(now);
        self.sweep_spent = 0.0;
        self.maintenance_earned = 0.0;
        if !link.syscall_frame() && interval > 0.0 {
            let fixed = now.saturating_duration_since(link.began()).as_secs_f64() * 1e6;
            self.window.spend.fixed_us += fixed;
            // Births and the enumeration earn their share of each frame's
            // sampling allowance here, once, whichever path reads the
            // positions; the pump adds nothing to it.
            let earned = (self.budget.allowance - self.budget.charge - fixed).max(0.0)
                * self.maintenance_share(now)
                * self.scale();
            // With no birth and no enumeration waiting the balance earns
            // only to repay its own debt: the row tiers keep the rest.
            let waits = self.births > 0 || self.enum_due || self.enumeration.is_some();
            let top = if waits { MAINTENANCE_CAP_US } else { 0.0 };
            let before = self.maintenance_us;
            self.maintenance_us = (before + earned).min(top).max(before);
            self.maintenance_earned = self.maintenance_us - before;
        }
    }

    /// The most a frame's slice can be: the cap less what the frame's finish
    /// takes, closing the last batch and writing its record.
    fn slice_cap(&self) -> f64 {
        let finish = (self.close_us + self.tail_us + 2.0 * self.close_dev_us).max(FINISH_FLOOR_US);
        SLICE_CAP_US - finish
    }

    /// The share of a frame's sampling allowance that births and the
    /// enumeration earn, whether or not a row tier owes rows.
    fn maintenance_share(&self, now: Instant) -> f64 {
        if self.enumeration_overdue(now) {
            MAINTENANCE_SHARE_OVERDUE
        } else {
            MAINTENANCE_SHARE
        }
    }

    /// The second part of [`Sampler::decide`], at `now`: whether what the
    /// slice leaves after the frame's work so far is worth the call.
    pub fn gate(&mut self, link: &impl Port, now: Instant) -> bool {
        let interval = link.frame_interval_us();
        if link.syscall_frame() || interval <= 0.0 {
            return false;
        }
        // The decision is timed from here, after any sweep.
        self.decided = Some(now);
        self.decided_commanded = self.budget.commanded_us;
        self.excused_us = 0.0;
        let worth = !self.direct || self.pump_worth(now);
        // A pump wanted for the subscribed rows read with their attitude may
        // spend the sweep's savings on them: the reserve does not bind it.
        let reserved =
            if self.direct && (self.sub_wanted() || (!self.urgent.is_empty() && !self.behind())) {
                0.0
            } else {
                self.reserved()
            };
        let b = &mut self.budget;
        let fixed = now.saturating_duration_since(link.began()).as_secs_f64() * 1e6 + b.events_us
            - b.commanded_us;
        if fixed - self.sweep_spent >= b.allowance {
            self.starved += 1;
        }
        if !worth {
            return false;
        }
        // Pump only when the call costs at most a third of what the frame
        // has after its fixed work; until then the frame's allowance becomes
        // credit (LIVE, S10: the call costs 22 µs under Wine, against a
        // 25 µs slice at 110 frames a second).
        // A pump also pays each batch's fixed part, so it waits until the
        // call and one batch's fixed part take at most a third, as far as
        // credit can reach beside the sweep's savings (LIVE, S10: small
        // pumps spent most of their time on the call and the batches' fixed
        // parts).
        let reach = 0.8 * (b.allowance - b.charge + CREDIT_CAP_US - fixed - reserved);
        let gate = (3.0 * (self.xcall_us + self.batch_us))
            .min(reach)
            .max(3.0 * self.xcall_us)
            .max(MIN_BUDGET_US);
        // An estimate of the call that is too high can put the floor of
        // three calls above what a frame can ever have. No pump would then
        // come to correct it, so the estimate decays on such a frame (LIVE,
        // S10: one slow call stopped sampling for good before the estimate
        // decayed). It decays at most once a frame.
        if 3.0 * self.xcall_us > reach && !std::mem::replace(&mut self.x_decayed, true) {
            self.xcall_us *= 0.99;
        }
        let left = b.slice - fixed - reserved;
        if left < gate {
            return false;
        }
        // Below scale 1 the pump's budget is `k` of what is left
        // (`Sampler::begin`), which can be less than a batch's fixed part,
        // its close and one row. With no debt the first batch then takes a
        // row anyway, and the overrun becomes debt (`Sampler::affordable`).
        // In debt it takes none, so a pump comes only when the budget pays
        // for a row (the tenth-overrun ruling; S10's debug mock at k = 0.1
        // planned no row on 30 to 50 % of pumps, all of them in debt).
        // A pump for the subscribed rows is not held back by debt.
        let in_debt = self.budget.debt > 0.0;
        let k = self.scale();
        let maintenance = self.births > 0 || self.enum_due || self.enumeration.is_some();
        if k < 1.0 && maintenance && !self.sub_wanted() && self.urgent.is_empty() {
            // A pump that comes for maintenance (births pending, or the
            // enumeration) pays the call and plans nothing when the budget
            // cannot pay its first step (LIVE, S11: 50 births pending for
            // over 2 minutes at k = 0.1, about 8 such pumps a second).
            // Maintenance goes only on a balance that pays a batch
            // (`Sampler::maintenance_pays`). On a short one
            // it plans no step and the pump comes for the owed rows alone, or
            // not at all when none are owed; with only event marks owed,
            // which may not be ripe, or a tier whose cost is unmeasured, it
            // plans nothing unless the budget pays a row. The budget must pay
            // the step, a batch's fixed part and the close. With no debt and
            // the balance paid, a measured step goes alone whatever the
            // budget, as above: the budget never reaches one step at a tenth
            // of the slice, and holding it to that would starve it. An
            // unmeasured step counts as free, so the first one still goes.
            let may = self.maintenance_pays();
            let step = if !may {
                self.cheapest(false)
            } else if self.births > 0 {
                Some(self.tiers[Tier::Births.index()].cost)
            } else {
                Some(self.tiers[Tier::Enumeration.index()].cost)
            };
            return match step {
                None => false,
                Some(step) if in_debt || (!may && !self.credit_owed()) => {
                    k * (left - self.xcall_us)
                        >= step.max(0.0) + self.batch_us + self.close_us + 2.0 * self.close_dev_us
                }
                Some(_) => true,
            };
        }
        if k < 1.0
            && in_debt
            && !self.sub_wanted()
            && let Some(row) = self.cheapest_owed()
        {
            return k * (left - self.xcall_us)
                >= row + self.batch_us + self.close_us + 2.0 * self.close_dev_us;
        }
        true
    }

    /// Tell the sampler whether the host sweeps positions itself, by object
    /// ID, inside `frame`.
    pub fn set_direct(&mut self, direct: bool) {
        self.direct = direct;
    }

    /// What the pump leaves of the frame's slice for the host's sweep, µs.
    fn reserved(&self) -> f64 {
        // The savings hold `1 / k` of what the sweep may spend
        // ([`Sampler::sweep`]).
        if self.direct {
            self.sweep_fund * self.scale()
        } else {
            0.0
        }
    }

    /// Whether a tier above the positions waits for a pump: births, the
    /// enumeration, or subscribed rows read with their attitude.
    fn higher_waits(&self) -> bool {
        self.births > 0 || self.enum_due || self.enumeration.is_some() || self.sub_wanted()
    }

    /// Whether the subscribed rows read with their attitude want a pump:
    /// they owe a batch's worth, the smaller of three calls' worth of rows
    /// and the rows subscribed. A tier owes all its rows once its oldest is
    /// past its period, so that case is in the second term.
    fn sub_wanted(&self) -> bool {
        if !self.sub_attitude {
            return false;
        }
        let t = &self.tiers[Tier::Subscribed.index()];
        let call = if self.xcall_seen > 0.0 {
            self.xcall_seen
        } else {
            20.0
        };
        let calls = if t.cost > 0.0 {
            (3.0 * call / t.cost).ceil()
        } else {
            1.0
        };
        t.credit.floor() >= calls.min(self.sub_slots.len() as f64).max(1.0)
    }

    /// Whether the host's sweep is behind: it owes more positions than a
    /// quarter of a period's rows and one batch. The pump then reads no tier
    /// below the positions (spec/sampling.md, "Tiers": the rank is strict).
    fn behind(&self) -> bool {
        let t = &self.tiers[Tier::Positions.index()];
        let idle = (self.idle_units + self.idle_cargo) as f64;
        let swept = (self.units + self.cargo)
            .saturating_sub(self.suspect_units + self.suspect_cargo) as f64;
        let per_period = swept - idle + idle / IDLE_EVERY as f64;
        self.direct && t.credit > per_period / 4.0 + SWEEP_MIN as f64
    }

    /// Whether the pump has work worth its call, while the host sweeps the
    /// positions: a tier above them waits; or the tiers below them owe at
    /// least three calls' worth of rows, or as much as a frame could spend
    /// on them; or they owe anything and the last pump is `PUMP_WAIT_S` old.
    fn pump_worth(&self, now: Instant) -> bool {
        // Births and the enumeration count only while they may take a batch
        // ([`Sampler::maintenance_pays`]): a pump that can plan nothing costs
        // its call for no rows.
        let maintenance = (self.births > 0 || self.enum_due || self.enumeration.is_some())
            && self.maintenance_pays();
        if maintenance || self.sub_wanted() || !self.urgent.is_empty() || self.loadouts_ready() {
            return true;
        }
        if self.behind() {
            return false;
        }
        // A pending-destroy object resolves only in a pump's existence
        // checks, and the set has a 60 s backstop: with no row owed, nothing
        // else asks for the pump (spec/commands.md, "A destroy while paused
        // is pending").
        if !self.pending.is_empty() {
            return true;
        }
        // Before the first pump the call is unmeasured: LIVE, S10, it costs
        // 20 to 30 µs under Wine.
        let call = if self.xcall_seen > 0.0 {
            self.xcall_seen
        } else {
            20.0
        };
        let waited = self
            .last_pump
            .is_none_or(|t| now.saturating_duration_since(t).as_secs_f64() >= PUMP_WAIT_S);
        let mut owed = self.owed_cost(false);
        // A tier a pump could not afford a row of leaves the sum for the
        // pump's wait from when it was first marked. Pumps that come for
        // other tiers more than once a second do not keep it out.
        if !waited {
            for tier in [Tier::Vitals, Tier::Warehouses] {
                let t = &self.tiers[tier.index()];
                if self.marked_out(tier, now) {
                    let c = if t.cost > 0.0 { t.cost } else { call };
                    owed -= t.credit.floor() * c;
                }
            }
        }
        if owed <= 0.0 {
            return false;
        }
        let b = &self.budget;
        let room = b.allowance - b.charge + CREDIT_CAP_US - call;
        waited || owed >= (3.0 * call).min(room).max(MIN_BUDGET_US)
    }

    /// Mark `tier` unaffordable at `now`, unless it is marked already: the
    /// mark keeps the time it was first set (spec/sampling.md, "The pump's
    /// worth").
    fn mark_unaffordable(&mut self, tier: Tier, now: Instant) {
        self.unaffordable[tier.index()].get_or_insert(now);
    }

    /// Whether `tier`'s mark keeps it out of the pump's worth at `now`: for
    /// `PUMP_WAIT_S` from when it was first set.
    fn marked_out(&self, tier: Tier, now: Instant) -> bool {
        self.unaffordable[tier.index()]
            .is_some_and(|at| now.saturating_duration_since(at).as_secs_f64() < PUMP_WAIT_S)
    }

    /// Sweep positions in the host's own state at `now`, between
    /// [`Sampler::slice`] and [`Sampler::gate`]: the subscribed rows read
    /// without attitude, then every unit's row that is due, each read by
    /// `read` from its object ID. `read` gives `Ok(None)` when the getter
    /// gave no position and `Err` when it raised; a row whose read raised is
    /// left out of every tier, and the sweep ends there. Returns the instant
    /// the sweep ended, which the caller passes to [`Sampler::gate`].
    ///
    /// Nothing else runs between planning a row and reading it, so the sweep
    /// keeps no batch. It waits until it can read `SWEEP_MIN` rows, or every
    /// row owed when the last sweep is `SWEEP_LATE` of a period old, and it
    /// takes half of what the frame has while a tier above the positions
    /// waits for a pump (spec/sampling.md, "Tiers").
    pub fn sweep(
        &mut self,
        link: &mut impl Port,
        now: Instant,
        read: &mut dyn FnMut(u64) -> Result<Option<[f64; 3]>, ()>,
        clock: impl Fn() -> Instant,
    ) -> Instant {
        let interval = link.frame_interval_us();
        self.sweep_wait = None;
        if !self.direct || link.syscall_frame() || interval <= 0.0 {
            return now;
        }
        self.accrue(now);
        let k = self.scale();
        let subs = if self.sub_attitude {
            0
        } else {
            self.tiers[Tier::Subscribed.index()].credit.floor() as usize
        };
        let t = &self.tiers[Tier::Positions.index()];
        let owed = subs + t.credit.floor() as usize;
        // The pump for the subscribed rows read with their attitude runs
        // first: the sweep reads no unsubscribed position until it has. The
        // savings stay as they are. Not while births or an enumeration that
        // is due or running wait: they rank above the subscribed rows and
        // take that pump's budget first, so the sweep would wait on them.
        let maintenance = self.births > 0 || self.enum_due || self.enumeration.is_some();
        if self.sub_wanted() && !maintenance {
            if owed > 0 {
                self.sweep_waited(SweepWait::Subscribed);
            }
            return now;
        }
        let t = &self.tiers[Tier::Positions.index()];
        if owed == 0 {
            self.sweep_fund = 0.0;
            self.sweep_need = 0.0;
            return now;
        }
        let cost = if t.cost > 0.0 { t.cost } else { 1.0 };
        // A sweep costs a fixed part, the record and its flush, plus its
        // rows. The plan keeps them apart, so a small sweep does not read as
        // dear rows and shrink the next one.
        let row = if self.sweep_row_us > 0.0 {
            self.sweep_row_us
        } else {
            cost
        };
        let lead = self.sweep_batch_us;
        // Of a frame, the sweep takes at most what births and the
        // enumeration leave while a tier above the positions waits for a
        // pump: the shares of one frame add up to the frame, so a pump has
        // room in it when the slice's cap binds.
        let part = if self.higher_waits() {
            1.0 - self.maintenance_share(now)
        } else {
            1.0
        };
        // It saves for its batch out of the row tiers' allowance, up to what
        // the rows owed cost: the frame's allowance less what the births' and
        // the enumeration's balance earned on it. It never waits on them.
        // Below a scale of 1 it saves its share of the frame, as the scale's
        // rules have it. The pump leaves the savings alone
        // ([`Sampler::reserved`]), so the tiers below the positions get what
        // the positions do not need.
        let whole = (self.budget.allowance - self.budget.charge).max(0.0);
        let saves = if k < 1.0 || whole <= 0.0 {
            part
        } else {
            (1.0 - self.maintenance_earned / whole).clamp(0.0, 1.0)
        };
        let cap = self.slice_cap();
        let b = &self.budget;
        let fixed = now.saturating_duration_since(link.began()).as_secs_f64() * 1e6 + b.events_us
            - b.commanded_us;
        // The most a frame can ever have, with full credit and no debt: no
        // more than the largest slice. A sweep the savings pay for always
        // fits a frame, and the pump is not shut out by savings the sweep
        // can never spend.
        let reach = ((b.allowance - b.charge + CREDIT_CAP_US).min(cap) - fixed).max(0.0) * part;
        // What a sweep of the rows owed costs, as far as a frame reaches.
        let need = (owed.min(self.max_batch) as f64 * row + lead).min(reach);
        // The scale applies to the savings (spec/faults.md): the sweep
        // spends `k` of them, so they hold `1 / k` of a batch's cost and a
        // sweep at a tenth is still a whole batch, ten times less often.
        let full = need / k.max(1e-9);
        self.sweep_need = need;
        let fund = (self.sweep_fund + saves * (b.allowance - b.charge).max(0.0)).min(full);
        self.sweep_fund = fund;
        let budget = (fund * k).min((b.slice - fixed).max(0.0));
        let rows_for = |us: f64| ((us - lead).max(0.0) / row + 1e-9).floor() as usize;
        let least = SWEEP_MIN.min(rows_for(reach)).max(1);
        let mut n = owed.min(rows_for(budget)).min(self.max_batch);
        // A fixed part that alone passes what a frame can spend: with full
        // savings and no debt the sweep takes one row, so it measures its
        // cost again and never stops for good.
        let forced = n == 0 && fund >= full && b.debt <= 0.0 && budget > 0.0;
        if forced {
            n = 1;
        }
        let late = self.last_sweep.is_none_or(|l| {
            now.saturating_duration_since(l).as_secs_f64() >= SWEEP_LATE * self.rates.position_s
        });
        if n == 0 || !(n >= least || (late && n == owed)) {
            let why = if n > 0 {
                SweepWait::MinBatch
            } else if rows_for(fund * k) == 0 {
                SweepWait::Fund
            } else if b.repay > 0.0 && rows_for(((b.slice + b.repay).min(cap) - fixed).max(0.0)) > 0
            {
                SweepWait::Debt
            } else {
                SweepWait::Slice
            };
            self.sweep_waited(why);
            return now;
        }
        // The flush of queued OBJECT records is the frame's time, not the
        // sweep's: it stays out of the sweep's costs.
        let mut began = now;
        if self.dirty {
            // OBJECT records go before the samples of this frame.
            link.flush();
            self.dirty = false;
            began = clock();
        }
        // The loop reads the clock every few rows and stops at the
        // deadline, which leaves room for writing the record after it.
        // Clock reads cost at most `CLOCK_SHARE` of the rows between them
        // (E33), and come at least every `SWEEP_CLOCK_ROWS` rows.
        let step =
            ((self.clock_cost_us / (CLOCK_SHARE * row)).ceil() as u64).clamp(1, SWEEP_CLOCK_ROWS);
        let loop_start = clock();
        let deadline = now + Duration::from_secs_f64((budget - self.sweep_tail_us).max(0.0) / 1e6);
        let mut since = 0u64;
        let mut cut = false;
        let mut raised = false;
        let mut done = [0usize; 2];
        for (stage, tier) in [Tier::Subscribed, Tier::Positions].into_iter().enumerate() {
            let want = if stage == 0 { subs.min(n) } else { n - done[0] };
            if want == 0 || raised || cut {
                continue;
            }
            let len = self.list_len(tier);
            if len == 0 {
                continue;
            }
            let start = self.tiers[tier.index()].cursor.min(len);
            let mut place = start;
            self.tiers[tier.index()].sweep_start.get_or_insert(now);
            let mut looked = 0;
            let mut completed = false;
            while done[stage] < want && looked < len {
                if place >= len {
                    place = 0;
                    completed = true;
                    self.completed(tier, now);
                }
                let here = place;
                place += 1;
                looked += 1;
                let Some(slot) = self.readable(tier, here) else {
                    continue;
                };
                let object = self.rows[slot as usize]
                    .as_ref()
                    .map_or(0, |r| r.found.object);
                match read(object) {
                    Ok(p) => {
                        if let Some(p) = p
                            && p.iter().all(|v| v.is_finite())
                            && p != [0.0, 0.0, 0.0]
                        {
                            let b = &mut self.batch.positions;
                            b.object.push(object);
                            b.x.push(p[0]);
                            b.y.push(p[1]);
                            b.z.push(p[2]);
                            self.moved(slot, p);
                        } else {
                            // No position: the object is gone with no event,
                            // until the enumeration removes its row. It
                            // counts as unmoved, so it is soon read less.
                            self.unmoved(slot);
                            self.check_soon(slot);
                        }
                        done[stage] += 1;
                        since += 1;
                        if since >= step && done[stage] < want {
                            since = 0;
                            if clock() >= deadline {
                                cut = true;
                                break;
                            }
                        }
                    }
                    Err(()) => {
                        self.suspect(slot);
                        raised = true;
                        break;
                    }
                }
            }
            let t = &mut self.tiers[tier.index()];
            t.credit = (t.credit - done[stage] as f64).max(0.0);
            t.cursor = if place >= len { 0 } else { place };
            // The sweep is complete when nothing more is due before the end
            // of the list.
            let cursor = t.cursor;
            if !raised
                && ((place >= len && !completed && looked > 0)
                    || (cursor > 0 && (cursor..len).all(|p| self.readable(tier, p).is_none())))
            {
                self.completed(tier, now);
                self.tiers[tier.index()].cursor = 0;
            }
        }
        let rows = done[0] + done[1];
        let loop_end = clock();
        let max = link.max_payload();
        let Sampler { batch, scratch, .. } = self;
        if !batch.positions.object.is_empty() {
            write_split(link, kind::POSITIONS, &batch.positions, max, scratch);
        }
        batch.positions.object.clear();
        batch.positions.x.clear();
        batch.positions.y.clear();
        batch.positions.z.clear();
        let end = clock();
        // The frame's time from the plan to the end, and the sweep's own.
        let whole = end.saturating_duration_since(now).as_secs_f64() * 1e6;
        let spent = end.saturating_duration_since(began).as_secs_f64() * 1e6;
        // What the scale withheld of the sweep counts as spent
        // (spec/faults.md).
        self.budget.withheld += spent * (1.0 - k) / k.max(1e-9);
        let ewma = |old: f64, new: f64| {
            if old <= 0.0 {
                new
            } else {
                old * 0.9 + new * 0.1
            }
        };
        let reads = loop_end.saturating_duration_since(loop_start).as_secs_f64() * 1e6;
        self.sweep_tail_us = ewma(
            self.sweep_tail_us,
            end.saturating_duration_since(loop_end).as_secs_f64() * 1e6,
        );
        self.sweep_batch_us = ewma(self.sweep_batch_us, (spent - reads).max(0.0));
        if rows > 0 {
            self.sweep_row_us = ewma(self.sweep_row_us, reads / rows as f64);
        }
        if cut {
            self.sweep_cut += 1;
        }
        // Past its budget by more than the rows between two clock reads
        // cost, at this sweep's own cost a row: the bound of spec/faults.md,
        // `sampling`.
        let over = whole - budget;
        let row_now = if rows > 0 { reads / rows as f64 } else { 0.0 };
        if forced {
            // It passes its budget by its fixed part, by design: counted
            // apart from the sweeps that the bound holds.
            self.sweep_forced += 1;
        } else {
            self.sweep_overrun_max = self.sweep_overrun_max.max(over);
            if over > step as f64 * row_now.max(cost) {
                self.sweep_overruns += 1;
            }
        }
        if rows > 0 {
            // The sweep's whole time over its rows: what a row costs it all
            // told, the record and the planning included. A small first
            // sweep reads dear, so a cheaper measure counts for half.
            let per_row = spent / rows as f64;
            let t = &mut self.tiers[Tier::Positions.index()];
            t.cost = if t.cost <= 0.0 {
                per_row
            } else if per_row < t.cost {
                (t.cost + per_row) / 2.0
            } else {
                t.cost * 0.9 + per_row * 0.1
            };
        }
        // What the sweep spent, and what the scale withheld of it, leave
        // its savings.
        self.sweep_fund = (self.sweep_fund - spent / k.max(1e-9)).max(0.0);
        let subscribed = spent * done[0] as f64 / rows.max(1) as f64;
        self.window.spend.tier_us[Tier::Subscribed.index()] += subscribed;
        self.window.spend.tier_us[Tier::Positions.index()] += spent - subscribed;
        self.window.spend.tier_rows[Tier::Subscribed.index()] += done[0] as u64;
        self.window.spend.tier_rows[Tier::Positions.index()] += done[1] as u64;
        self.sweep_spent += spent;
        self.sweeps += 1;
        self.last_sweep = Some(now);
        end
    }

    /// The sweep owed rows and read none, for `why`.
    fn sweep_waited(&mut self, why: SweepWait) {
        self.sweep_wait = Some(why);
        self.sweep_waits[why as usize] += 1;
    }

    /// A tier's sweep completed at `now`: its achieved period, and its count
    /// of sweeps, which moves the rows read less often to their next turn.
    fn completed(&mut self, tier: Tier, now: Instant) {
        complete(&mut self.tiers[tier.index()], now);
        if let Some(i) = pass_of(tier) {
            self.passes[i] += 1;
            // A slow sweep has read every row once its tier has completed
            // `every` sweeps.
            if tier == Tier::Vitals
                && let Some(every) = VITALS_EVERY
                && self.passes[i].is_multiple_of(every)
            {
                if let Some(start) = self.slow_start {
                    self.slow_period = now.saturating_duration_since(start).as_secs_f64();
                }
                self.slow_start = Some(now);
            }
        }
    }

    /// The achieved period of the slow sweep of life at `now`,
    /// seconds: the longer of the last sweep's time and the time the running
    /// one has taken, and 0 for a sweep that is switched off or has no rows.
    fn slow_periods(&self, now: Instant) -> f64 {
        self.slow_start.map_or(0.0, |start| {
            self.slow_period
                .max(now.saturating_duration_since(start).as_secs_f64())
        })
    }

    /// A read gave a row no position: count it as an unmoved read.
    fn unmoved(&mut self, slot: u32) {
        let Some(row) = self.rows.get_mut(slot as usize).and_then(Option::as_mut) else {
            return;
        };
        let was = row.idle();
        row.unmoved = row.unmoved.saturating_add(1).min(IDLE_AFTER);
        if !was && row.idle() {
            if row.unit() {
                self.idle_units += 1;
            } else if row.found.movable {
                self.idle_cargo += 1;
            }
        }
    }

    /// Note a row's position `p`: whether it moved since its last sample,
    /// which sets how often it is read, and whether it stopped, which asks
    /// for a unit's attitude once.
    fn moved(&mut self, slot: u32, p: [f64; 3]) {
        let Some(row) = self.rows.get_mut(slot as usize).and_then(Option::as_mut) else {
            return;
        };
        let moved = row.last.is_some_and(|l| {
            let d2: f64 = l.iter().zip(&p).map(|(a, b)| (a - b) * (a - b)).sum();
            d2 > STOP_M * STOP_M
        });
        let unit = row.unit();
        let stopped = row.moving && !moved && unit;
        let was = row.idle();
        row.unmoved = if moved {
            0
        } else {
            row.unmoved.saturating_add(1).min(IDLE_AFTER)
        };
        let idle = row.idle();
        row.moving = moved;
        row.last = Some(p);
        let push = stopped && !std::mem::replace(&mut row.in_stopped, true);
        if was != idle && (unit || row.found.movable) {
            let count = if unit {
                &mut self.idle_units
            } else {
                &mut self.idle_cargo
            };
            if idle {
                *count += 1;
            } else {
                *count = count.saturating_sub(1);
            }
        }
        if push {
            self.stopped.push(slot);
        }
    }

    /// Start the shim's pump at `now`, after [`Sampler::decide`] said to:
    /// fix the sampling budget, the slice less the work so far, times the
    /// test scale.
    pub fn begin(&mut self, link: &impl Port, now: Instant) -> Begin {
        self.pumps += 1;
        self.last_pump = Some(now);
        self.pump_call_us = 0.0;
        self.pump_rows = false;
        self.pump_paid = 0.0;
        if let Some(d) = self.decided.take() {
            let whole = now.saturating_duration_since(d).as_secs_f64() * 1e6;
            // Commanded work between the decision and the pump is out of
            // the debt already (`end_frame`): the balance does not pay it.
            // What `excuse` moved the decision past is out of `whole`
            // already, so only the rest comes off.
            let commanded = self.budget.commanded_us - self.decided_commanded;
            self.pump_call_us = (whole - (commanded - self.excused_us).max(0.0)).max(0.0);
            self.window.spend.xcall_us += whole;
            // The call's cost for `pump_worth`, which must not decay between
            // pumps: it decides how rare they are.
            let seen = whole.min(SLICE_CAP_US / 4.0);
            self.xcall_seen = if self.xcall_seen <= 0.0 {
                seen
            } else {
                self.xcall_seen * 0.9 + seen * 0.1
            };
            // One slow call, such as a garbage collection, counts no more
            // than a quarter of the largest slice.
            let xcall = whole.min(SLICE_CAP_US / 4.0);
            self.xcall_us = if self.xcall_us <= 0.0 {
                xcall
            } else {
                self.xcall_us * 0.9 + xcall * 0.1
            };
        }
        let interval = link.frame_interval_us();
        let k = self.scale();
        // The estimates of closing a batch and of the call decay on every
        // pump, so one that grew too large comes down again; that of a
        // batch's fixed part on every pump but one that only resolved
        // births or read the enumeration (`finish_pump`). Between pumps
        // they hold: with the sweep in `frame` the pump comes a few times a
        // second, and an estimate that decayed on every frame read near 0 by
        // the next pump (LIVE, S10: the gate then opened 20 to 30 times a
        // second for pumps too small).
        self.close_us *= 0.99;
        if !std::mem::replace(&mut self.x_decayed, true) {
            self.xcall_us *= 0.99;
        }
        // What the host's sweep saved is not the pump's to spend, except on
        // the subscribed rows read with their attitude when they want this
        // pump: the other tiers keep the reserve.
        let reserve = self.reserved();
        self.sub_pump = self.direct && self.sub_wanted();
        // The same for a pump that makes the checks an event asked for: they
        // cost little and are not the sweep's to hold back; the rows of the
        // other tiers keep the reserve.
        let release = self.sub_pump || (self.direct && !self.urgent.is_empty() && !self.behind());
        let reserved = if release { 0.0 } else { reserve };
        self.pump_reserve = if release { reserve * k } else { 0.0 };
        let b = &mut self.budget;
        let fixed = now.saturating_duration_since(link.began()).as_secs_f64() * 1e6 + b.events_us
            - b.commanded_us
            + reserved;
        // The one point the test scale applies (spec/faults.md). What it
        // withholds is not spent and never becomes debt.
        b.budget = (b.slice - fixed) * k;
        // What the scale withholds counts as spent, so it never returns as
        // credit (spec/faults.md).
        b.withheld += ((b.slice - fixed) * (1.0 - k)).max(0.0);
        let sample = b.budget > 0.0 && interval > 0.0;
        b.deadline = sample.then(|| now + Duration::from_secs_f64(b.budget / 1e6));
        self.sampling = sample;
        self.batches = 0;
        self.planned = [false; 7];
        self.pump_start = Some(now);
        self.since_clock = 0;
        self.batch = Batch::default();
        if sample {
            self.accrue(now);
        }
        Begin {
            generation: self.generation,
            sample,
        }
    }

    /// The sampling budget's scale `k` (spec/faults.md): 1 in a build
    /// without fault injection.
    #[cfg(not(feature = "faults"))]
    #[inline(always)]
    const fn scale(&self) -> f64 {
        1.0
    }

    /// The sampling budget's scale `k` (spec/faults.md).
    #[cfg(feature = "faults")]
    fn scale(&self) -> f64 {
        self.scale
    }

    /// Set the sampling budget's scale, in (0, 1], for the tenth-of-the-slice
    /// test (spec/faults.md). It stays until set again or the agent reloads.
    #[cfg(feature = "faults")]
    pub fn set_scale(&mut self, scale: f64) -> Result<(), String> {
        if !(scale > 0.0 && scale <= 1.0) {
            return Err("sampling takes a scale above 0 and at most 1".into());
        }
        self.scale = scale;
        Ok(())
    }

    fn accrue(&mut self, now: Instant) {
        let dt = self
            .last_frame
            .map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        self.last_frame = Some(now);
        self.rebuild_subs();
        if self.recount {
            self.tally();
        }
        // The rows a tier reads: none that is suspect.
        let rows = self.len().saturating_sub(self.suspect_rows) as f64;
        let units = self.units.saturating_sub(self.suspect_units) as f64;
        let cargo = self.cargo.saturating_sub(self.suspect_cargo) as f64;
        // The rows one sweep of a tier reads: every row read each sweep, and
        // the share of the rows read once in several sweeps.
        let part = |all: f64, often: f64, every: Option<u64>| {
            often + every.map_or(0.0, |e| (all - often) / e as f64)
        };
        let idle = (self.idle_units + self.idle_cargo) as f64;
        let swept = units + cargo;
        let periods = [
            (
                Tier::Subscribed,
                self.sub_slots.len() as f64,
                self.sub_period,
            ),
            (
                Tier::Positions,
                part(swept, swept - idle, Some(IDLE_EVERY)),
                self.rates.position_s,
            ),
            (
                Tier::Vitals,
                part(rows, self.vital_rows as f64, VITALS_EVERY),
                self.rates.vitals_s,
            ),
            (
                Tier::Warehouses,
                self.airbases as f64,
                self.rates.warehouse_s,
            ),
        ];
        #[cfg(test)]
        let one_sweep = self.vitals_cap_one_sweep;
        #[cfg(not(test))]
        let one_sweep = false;
        for (tier, n, period) in periods {
            let period = period.max(1e-3);
            if tier == Tier::Vitals {
                self.window.vitals_due += n * dt / period;
                #[cfg(test)]
                {
                    self.vitals_due += n * dt / period;
                }
            }
            let t = &mut self.tiers[tier.index()];
            // A tier holds at most one sweep's rows. Vitals holds one full slow
            // cycle, every row once: a stall or a higher tier's hold costs it
            // more than a twelfth of its rows, and the rows owed are repaid.
            let cap = match tier {
                Tier::Vitals if !one_sweep => n.max(rows),
                _ => n,
            };
            t.credit = (t.credit + n * dt / period).min(cap.max(1.0));
            if t.credit >= 1.0 {
                t.owed_since.get_or_insert(now);
                // A slow sweep's first period runs from the first row owed.
                if tier == Tier::Vitals && VITALS_EVERY.is_some() {
                    self.slow_start.get_or_insert(now);
                }
            }
            if n == 0.0 {
                // A tier with no rows completes once per target period.
                let due = t
                    .last_complete
                    .is_none_or(|l| now.saturating_duration_since(l).as_secs_f64() >= period);
                if due {
                    self.completed(tier, now);
                }
            }
        }
        // The enumeration keeps its period.
        if self.enumeration.is_none()
            && self.end_hold.is_none()
            && self.enum_last_begin.is_none_or(|t| {
                now.saturating_duration_since(t).as_secs_f64() >= self.rates.enumeration_s
            })
        {
            self.enum_due = true;
        }
        if self.enum_due || self.enumeration.is_some() {
            self.tiers[Tier::Enumeration.index()]
                .owed_since
                .get_or_insert(now);
        }
    }

    /// Whether the sampling budget is spent at `now`.
    /// Whether the rows' budget is spent at `now`: the budget less the
    /// expected cost of closing the batch.
    fn spent(&self, now: Instant) -> bool {
        self.remaining_us(now) <= 0.0
    }

    /// The budget left for rows at `now`, µs: what is left of the budget,
    /// less the expected cost of closing the batch.
    fn remaining_us(&self, now: Instant) -> f64 {
        self.budget.deadline.map_or(0.0, |d| {
            d.saturating_duration_since(now).as_secs_f64() * 1e6
                - self.close_us
                - 2.0 * self.close_dev_us
                - self.tail_us
        })
    }

    /// Fold one measured close of a batch into the estimate, µs.
    fn learn_close(&mut self, us: f64) {
        let closed = us.min(SLICE_CAP_US / 4.0);
        self.close_us = if self.close_us <= 0.0 {
            closed
        } else {
            self.close_dev_us = self.close_dev_us * 0.9 + (closed - self.close_us).abs() * 0.1;
            self.close_us * 0.9 + closed * 0.1
        };
    }

    /// Plan the next batch at `now`, closing the last one first, and write
    /// its slots into `slots` for the row tiers. `None` when the budget is
    /// spent or nothing is owed.
    pub fn plan(
        &mut self,
        link: &mut impl Port,
        clock: impl Fn() -> Instant,
        slots: &mut Vec<u32>,
    ) -> Option<Plan> {
        let now = clock();
        if self.batch.tier.is_some() {
            self.close_batch(link, now);
            // Closing writes the batch's records; the next rows leave room
            // for it (spec/sampling.md, "Tiers").
            // One slow close, such as a garbage collection, counts no more
            // than a quarter of the largest slice.
            self.learn_close(clock().saturating_duration_since(now).as_secs_f64() * 1e6);
        }
        let now = clock();
        slots.clear();
        // The frame's first batch goes to the lone-row rule even when the
        // reserves for closing take the whole budget, as at a tenth of the
        // slice; later batches stop once the budget is spent.
        if !self.sampling || (self.batches > 0 && self.spent(now)) {
            return None;
        }
        let remaining = self.remaining_us(now);
        let behind = self.behind();
        // A tier below the positions that the pumps could not afford and
        // that has owed rows for over two of its periods goes first: the
        // rank is strict between pumps that can pay for every tier, and no
        // tier is starved for good. Not at a scaled budget, where the rank
        // is what the test measures.
        let late = |tier: Tier| {
            let period = match tier {
                Tier::Vitals => self.rates.vitals_s,
                Tier::Warehouses => self.rates.warehouse_s,
                _ => return false,
            };
            let t = &self.tiers[tier.index()];
            // Timed from its last planned batch, or from its first owed row.
            let since = self.last_planned[tier.index()].or(t.owed_since);
            self.scale() >= 1.0
                && self.unaffordable[tier.index()].is_some()
                && !behind
                && since.is_some_and(|s| {
                    // Vitals waits a period; a tier whose rows are few waits
                    // until two are owed and the pump's wait is over.
                    let waited = now.saturating_duration_since(s).as_secs_f64();
                    if tier == Tier::Vitals {
                        waited > period
                    } else {
                        t.credit >= 2.0 && waited > PUMP_WAIT_S
                    }
                })
        };
        #[cfg(test)]
        let lates = if self.no_guard {
            [false; 7]
        } else {
            Tier::ALL.map(late)
        };
        #[cfg(not(test))]
        let lates = Tier::ALL.map(late);
        let is_late = |t: Tier| lates[Tier::ALL.iter().position(|x| *x == t).unwrap_or(0)];
        // Such a tier goes right after the subscribed rows, with one row;
        // no tier below a tier that owes vitals goes ahead of it.
        let mut order = Tier::ALL;
        order.sort_by_key(|t| match (is_late(*t), *t) {
            (true, _) => 3,
            (false, Tier::Births) => 0,
            (false, Tier::Enumeration) => 1,
            (false, Tier::Subscribed) => 2,
            (false, _) => 4 + Tier::ALL.iter().position(|x| x == t).unwrap_or(0),
        });
        for tier in order {
            if !self.maintenance_may(tier) {
                continue;
            }
            // A pump not wanted for the subscribed rows read with their
            // attitude reads none of them: they wait for a batch's worth, or
            // for their oldest to pass its period, and their own pump.
            if tier == Tier::Subscribed
                && self.direct
                && self.sub_attitude
                && !self.sub_pump
                && !self.sub_wanted()
            {
                continue;
            }
            // The host sweeps these in its own state.
            if self.direct
                && (tier == Tier::Positions || (tier == Tier::Subscribed && !self.sub_attitude))
            {
                continue;
            }
            // The rank is strict: while the host's sweep is behind, no tier
            // below the positions gets rows.
            if behind && matches!(tier, Tier::Stopped | Tier::Vitals | Tier::Warehouses) {
                continue;
            }
            // On a pump for the subscribed rows the other tiers keep the
            // sweep's savings.
            let remaining = if tier == Tier::Subscribed {
                remaining
            } else {
                remaining - self.pump_reserve
            };
            // Vitals is the ruled rank's own tier: late, it takes its rows.
            self.late_tier = (is_late(tier) && tier != Tier::Vitals).then_some(tier);
            if let Some(plan) = self.plan_tier(link, tier, now, remaining, slots) {
                self.batches += 1;
                self.last_planned[tier.index()] = Some(now);
                self.planned[tier.index()] = true;
                self.unaffordable[tier.index()] = None;
                if self.dirty && tier >= Tier::Subscribed {
                    // OBJECT records go before the samples of this frame.
                    link.flush();
                    self.dirty = false;
                }
                let cost = self.tiers[plan.tier.index()].cost;
                // A batch planned past what is left: one dear row, or a
                // fixed part that alone passes the budget.
                let lead = (self.batch_us - self.tail_us).max(0.0);
                let past = cost > 0.0 && plan.rows as f64 * cost + lead > remaining;
                if past {
                    self.lone = true;
                }
                if matches!(plan.tier, Tier::Births | Tier::Enumeration) {
                    self.maintenance = true;
                } else {
                    self.pump_rows = true;
                }
                self.batch.tier = Some(plan.tier);
                self.batch.done = 0;
                self.batch.started = Some(now);
                self.batch.first = None;
                self.batch.last = None;
                self.batch.past = past;
                self.batch.dear = 0;
                self.batch.dear_us = 0.0;
                self.batch.cheap = 0;
                self.batch.cheap_us = 0.0;
                self.batch.unmarked = 0;
                self.batch.here = false;
                self.step_at = Some(now);
                self.batch.slots.clone_from(slots);
                self.batch.versions = slots
                    .iter()
                    .map(|s| self.row(*s).map_or(0, |r| r.version))
                    .collect();
                return Some(plan);
            }
        }
        None
    }

    /// Whether a row tier with a measured cost owes rows by its credit, not
    /// by marks.
    fn credit_owed(&self) -> bool {
        !self.stopped.is_empty()
            || [
                Tier::Subscribed,
                Tier::Positions,
                Tier::Vitals,
                Tier::Warehouses,
            ]
            .iter()
            .any(|t| self.tiers[t.index()].credit >= 1.0 && self.tiers[t.index()].cost > 0.0)
    }

    /// Whether a row tier owes rows: a stopped row, an event mark, or a
    /// tier with a row's credit. It sets the balance births and the
    /// enumeration need for a batch ([`Sampler::maintenance_pays`]).
    fn rows_owed(&self) -> bool {
        !self.stopped.is_empty()
            || !self.life_marks.is_empty()
            || [
                Tier::Subscribed,
                Tier::Positions,
                Tier::Vitals,
                Tier::Warehouses,
            ]
            .iter()
            .any(|t| self.tiers[t.index()].credit >= 1.0)
    }

    /// Charge `charge` µs to the births' and the enumeration's balance, for
    /// `us` µs of their time; `charge` passes `us` by what the scale
    /// withholds. The −1,000 µs floor stops that surplus. Their own time
    /// may take the balance below the floor, down to the bound the debt has,
    /// one second's budget. A batch starts only on a balance that pays it
    /// ([`Sampler::maintenance_pays`]), so only a step dearer than that
    /// bound, a stall inside it, passes it; what passes is forgiven and counted.
    /// None of it becomes the row tiers' debt.
    fn pay_maintenance(&mut self, us: f64, charge: f64) {
        let us = us.max(0.0);
        let owed = (self.maintenance_us - charge)
            .max(MAINTENANCE_FLOOR_US)
            .min(self.maintenance_us - us);
        let bounded = owed.max(-self.maintenance_bound);
        self.window.maintenance_forgiven_us += bounded - owed;
        self.maintenance_us = bounded;
        self.maintenance_paid += us;
        self.pump_paid += us;
    }

    /// Whether `tier` may take a batch: births and the enumeration only while
    /// their balance pays one ([`Sampler::maintenance_pays`]). Births go
    /// before the enumeration in the tier order.
    fn maintenance_may(&self, tier: Tier) -> bool {
        !matches!(tier, Tier::Births | Tier::Enumeration) || self.maintenance_pays()
    }

    /// Whether births' and the enumeration's balance pays a batch
    /// (spec/sampling.md, "Tiers"): while a row tier owes rows it covers
    /// [`Sampler::maintenance_batch`]; with no row owed it is at or above 0.
    /// The plan, the pump's worth and the gate below scale 1 all ask this.
    fn maintenance_pays(&self) -> bool {
        let need = if self.rows_owed() {
            self.maintenance_batch()
        } else {
            0.0
        };
        self.maintenance_us >= need
    }

    /// What births and the enumeration must have saved to take a batch:
    /// three calls, or the owed rows' cost and a batch's fixed part if that
    /// is less, at most what a frame can reach. Asked only while a row is
    /// owed ([`Sampler::maintenance_pays`]).
    fn maintenance_batch(&self) -> f64 {
        let call = if self.xcall_seen > 0.0 {
            self.xcall_seen
        } else {
            20.0
        };
        let b = &self.budget;
        let reach = 0.8 * (b.allowance - b.charge + CREDIT_CAP_US);
        (3.0 * call)
            .min(self.owed_cost(true) + self.batch_us)
            .min(reach)
            .max(MIN_BUDGET_US)
    }

    /// What the rows still owed cost, µs. `all` counts the positions and the
    /// subscribed rows the pump itself reads too; a tier a pump left starved
    /// counts too.
    fn owed_cost(&self, all: bool) -> f64 {
        let call = if self.xcall_seen > 0.0 {
            self.xcall_seen
        } else {
            20.0
        };
        // An unmeasured tier counts as a call's worth, so it gets its first
        // batch.
        let cost = |t: Tier| {
            let c = self.tiers[t.index()].cost;
            if c > 0.0 { c } else { call }
        };
        let mut tiers = vec![Tier::Vitals, Tier::Warehouses];
        if all && !self.direct {
            tiers.extend([Tier::Subscribed, Tier::Positions]);
        }
        tiers
            .iter()
            .map(|t| self.tiers[t.index()].credit.floor() * cost(*t))
            .sum::<f64>()
            + self.stopped.len() as f64 * cost(Tier::Stopped)
            + (self.life_marks.len() + self.checks.len()) as f64 * cost(Tier::Vitals)
    }

    /// The cost of the cheapest row or item the pump owes, µs; `None` when
    /// it owes nothing. An unmeasured tier counts as free, so it still gets
    /// its first batch.
    fn cheapest_owed(&self) -> Option<f64> {
        self.cheapest(true)
    }

    /// [`Sampler::cheapest_owed`], counting births and the enumeration only
    /// when `maintenance`.
    fn cheapest(&self, maintenance: bool) -> Option<f64> {
        let cost = |t: Tier| self.tiers[t.index()].cost.max(0.0);
        // While the host's sweep is behind, `plan` gives no row to the tiers
        // below the positions.
        let held = self.behind();
        let mut owed = vec![];
        if !held {
            for t in [Tier::Vitals, Tier::Warehouses] {
                if self.tiers[t.index()].credit >= 1.0 {
                    owed.push(cost(t));
                }
            }
            if !self.stopped.is_empty() {
                owed.push(cost(Tier::Stopped));
            }
            if !self.life_marks.is_empty() || !self.checks.is_empty() {
                owed.push(cost(Tier::Vitals));
            }
        }
        if !self.direct {
            for t in [Tier::Subscribed, Tier::Positions] {
                if self.tiers[t.index()].credit >= 1.0 {
                    owed.push(cost(t));
                }
            }
        }
        if self.sub_wanted() {
            owed.push(cost(Tier::Subscribed));
        }
        if maintenance && self.births > 0 {
            owed.push(cost(Tier::Births));
        }
        if maintenance && (self.enum_due || self.enumeration.is_some()) {
            owed.push(cost(Tier::Enumeration));
        }
        owed.into_iter().reduce(f64::min)
    }

    /// An enumeration that has run longer than its period.
    fn enumeration_overdue(&self, now: Instant) -> bool {
        self.enumeration.is_some()
            && self.enum_last_begin.is_some_and(|t| {
                now.saturating_duration_since(t).as_secs_f64() >= self.rates.enumeration_s
            })
    }

    /// How many rows of a tier the remaining budget allows, at most `owed`.
    /// A row that costs more than a whole budget is taken alone when there
    /// is no debt.
    fn affordable(&self, tier: Tier, remaining: f64, owed: usize) -> usize {
        if owed == 0 {
            return 0;
        }
        let cost = self.tiers[tier.index()].cost;
        // Each batch pays its fixed cost once, apart from its rows.
        // `remaining` already keeps room for the batch's tail.
        let lead = (self.batch_us - self.tail_us).max(0.0);
        if cost <= 0.0 {
            // Unmeasured: a few rows, when the fixed part leaves room.
            // A warehouse's read is the dearest of any tier (LIVE, about
            // 50 µs): unmeasured, it goes first in a pump or with room for
            // it, one row, so that it does not carry a frame past its cap.
            if tier == Tier::Warehouses {
                let room = remaining > lead + SLICE_CAP_US / 4.0;
                return if room { owed.min(1) } else { 0 };
            }
            return if remaining > lead { owed.min(8) } else { 0 };
        }
        let n = ((remaining - lead) / cost).floor().max(0.0) as usize;
        if n == 0 {
            // A row that costs more than the whole budget goes alone at its
            // tier's turn, and one that costs more than what is left goes
            // alone as the frame's first batch; both on a frame with no
            // debt (spec/sampling.md, "Tiers").
            // When the batch's fixed part and the reserves for closing it
            // pass what is left, as at a tenth of the slice, the first batch
            // takes the rows the budget `B` allows, at least one, and the
            // overrun becomes debt.
            let dear = cost > self.budget.budget.max(0.0) || self.batches == 0;
            // On a pump for the subscribed rows the sweep's withheld debt
            // does not block them.
            let debt = self.budget.debt > 0.0 && !(self.sub_pump && tier == Tier::Subscribed);
            if debt || !dear {
                return 0;
            }
            return ((self.budget.budget.max(0.0) / cost).floor() as usize).clamp(1, owed);
        }
        n.min(owed)
    }

    fn plan_tier(
        &mut self,
        link: &mut impl Port,
        tier: Tier,
        now: Instant,
        remaining: f64,
        slots: &mut Vec<u32>,
    ) -> Option<Plan> {
        match tier {
            Tier::Births => {
                let n = self.affordable(tier, remaining, self.births.min(self.max_batch));
                (n > 0).then_some(Plan {
                    tier,
                    rows: n,
                    flag: false,
                })
            }
            Tier::Enumeration => {
                let mut start = false;
                if self.enumeration.is_none() && self.enum_due {
                    self.enum_due = false;
                    self.enum_count += 1;
                    let n = self.enum_count;
                    self.enumeration = Some(n);
                    self.enum_objects = 0;
                    self.enum_seen_ids = 0;
                    self.flush_seen(link);
                    let drops = link.event_drops();
                    self.full |= drops != self.drops_seen;
                    self.drops_seen = drops;
                    self.enum_full = std::mem::take(&mut self.full);
                    self.enum_last_begin = Some(now);
                    for row in self.rows.iter_mut().flatten() {
                        if row.found.object != 0 {
                            row.enum_member = n;
                            row.enum_seen = false;
                        }
                    }
                    self.put(
                        link,
                        kind::ENUM_BEGIN as u16,
                        EnumBegin { enumeration: n }.encode(),
                    );
                    self.dirty = true;
                    start = true;
                }
                self.enumeration?;
                let n = self
                    .affordable(tier, remaining, self.max_batch)
                    .max(usize::from(start));
                (n > 0).then_some(Plan {
                    tier,
                    rows: n,
                    flag: start,
                })
            }
            Tier::Subscribed | Tier::Positions | Tier::Vitals | Tier::Warehouses => {
                // The rows an event marked go first: they are read at once,
                // not at the cursor's turn.
                let ripe = self.ripe(tier, now);
                let owed = ripe + self.tiers[tier.index()].credit.floor() as usize;
                let mut n = self.affordable(tier, remaining, owed.min(self.max_batch));
                if self.late_tier == Some(tier) {
                    n = n.min(1);
                }
                if n == 0 {
                    return None;
                }
                self.batch.lead = self.take_marks(tier, now, n.min(ripe), slots);
                let lead = self.batch.lead;
                if n > lead && self.tiers[tier.index()].credit >= 1.0 {
                    self.fill(tier, n, now, slots);
                } else {
                    self.batch.after.clear();
                }
                (!slots.is_empty()).then_some(Plan {
                    tier,
                    rows: slots.len(),
                    flag: tier == Tier::Subscribed && self.sub_attitude,
                })
            }
            Tier::Stopped => {
                let n = self.affordable(tier, remaining, self.stopped.len().min(self.max_batch));
                if n == 0 {
                    return None;
                }
                self.batch.lead = 0;
                slots.extend(self.stopped.drain(..n));
                Some(Plan {
                    tier,
                    rows: slots.len(),
                    flag: false,
                })
            }
        }
    }

    /// How many places a tier's cursor runs over: subscribed slots, airbases
    /// or table slots.
    fn list_len(&self, tier: Tier) -> usize {
        match tier {
            Tier::Subscribed => self.sub_slots.len(),
            Tier::Warehouses => self.airbases,
            _ => self.rows.len(),
        }
    }

    /// Whether an event-marked row may be read at `now`: its last read is
    /// old enough.
    fn again(at: Option<Instant>, now: Instant, wait_s: f64) -> bool {
        at.is_none_or(|t| now.saturating_duration_since(t).as_secs_f64() >= wait_s)
    }

    /// How many rows an event marked for a tier may be read at `now`.
    fn ripe(&self, tier: Tier, now: Instant) -> usize {
        match tier {
            Tier::Vitals => self
                .life_marks
                .iter()
                .filter(|s| {
                    self.present(**s)
                        .is_some_and(|r| Self::again(r.life_at, now, LIFE_AGAIN_S))
                })
                .count(),
            _ => 0,
        }
    }

    /// Move up to `n` marked rows that may be read at `now` from a tier's
    /// list into `slots`. Returns how many it moved. They stay marked until
    /// the batch closes.
    fn take_marks(&mut self, tier: Tier, now: Instant, n: usize, slots: &mut Vec<u32>) -> usize {
        if n == 0 {
            return 0;
        }
        if tier != Tier::Vitals {
            return 0;
        }
        let mut marks = std::mem::take(&mut self.life_marks);
        let mut taken = 0;
        marks.retain(|s| {
            let ready = taken < n
                && self
                    .present(*s)
                    .is_some_and(|r| !r.suspect && Self::again(r.life_at, now, LIFE_AGAIN_S));
            if ready {
                slots.push(*s);
                taken += 1;
            }
            !ready
        });
        self.life_marks = marks;
        taken
    }

    /// The slot at a cursor place, when the tier reads it on this sweep.
    /// Positions are the units' and the cargo statics'. A unit or a cargo
    /// static that has not moved, the life of a row that no
    /// subscription names are read once in several sweeps: the
    /// sweep's count and the slot pick the sweep, so each sweep reads an
    /// even share of them.
    fn readable(&self, tier: Tier, place: usize) -> Option<u32> {
        self.readable_at(tier, place, 0)
    }

    /// [`Sampler::readable`] in the pass `ahead` after the current one.
    fn readable_at(&self, tier: Tier, place: usize, ahead: u64) -> Option<u32> {
        match tier {
            Tier::Warehouses => Some(place as u32),
            Tier::Subscribed => {
                let slot = *self.sub_slots.get(place)?;
                self.present(slot).filter(|r| !r.suspect).map(|_| slot)
            }
            _ => {
                let slot = place as u32;
                let row = self.present(slot).filter(|r| !r.suspect)?;
                let turn = |every: Option<u64>| {
                    every.is_some_and(|every| {
                        pass_of(tier).is_some_and(|i| {
                            (self.passes[i] + ahead + u64::from(slot)).is_multiple_of(every)
                        })
                    })
                };
                let reads = match tier {
                    Tier::Positions => row.swept() && (!row.idle() || turn(Some(IDLE_EVERY))),
                    Tier::Vitals => row.vital() || turn(VITALS_EVERY),
                    _ => true,
                };
                reads.then_some(slot)
            }
        }
    }

    /// Fill `slots` with up to `n` readable places of a tier from its
    /// cursor, passing the end of the list at most once. The batch keeps the
    /// cursor place after each slot.
    ///
    /// Each place is evaluated once in a pass: the places past the end of
    /// the list belong to the next pass, which the batch's close counts. The
    /// batch notes where that pass begins, in `wrap`. A fill that finds
    /// nothing from a cursor inside the list completes the lap at once, and
    /// the next fill starts at the head.
    fn fill(&mut self, tier: Tier, n: usize, now: Instant, slots: &mut Vec<u32>) {
        let len = self.list_len(tier);
        self.batch.after.clear();
        self.batch.wrap = None;
        if len == 0 {
            return;
        }
        let start = self.tiers[tier.index()].cursor.min(len);
        let mut place = start;
        self.tiers[tier.index()].sweep_start.get_or_insert(now);
        let mut looked = 0;
        let mut ahead = 0;
        while slots.len() < n && looked < len {
            if place >= len {
                place = 0;
                ahead = 1;
                self.batch.wrap = Some(self.batch.after.len());
            }
            let here = place;
            place += 1;
            looked += 1;
            if let Some(slot) = self.readable_at(tier, here, ahead) {
                slots.push(slot);
                self.batch.after.push(place);
            }
        }
        if self.batch.after.is_empty() && looked == len {
            // Nothing is due from the cursor to the end or from the head to
            // the cursor, one pass on: the lap is complete. From the head
            // that is the whole list in this pass, and the lap is complete
            // as well, so the pass count moves to a pass with rows due.
            self.batch.wrap = None;
            self.completed(tier, now);
            self.tiers[tier.index()].cursor = 0;
        }
    }

    /// Close the batch in progress at `now`: write its record, move the
    /// tier's cursor past the rows done, update its cost per row and its
    /// credit, and note a completed sweep.
    fn close_batch(&mut self, link: &mut impl Port, now: Instant) {
        let Some(tier) = self.batch.tier.take() else {
            return;
        };
        if tier == Tier::Enumeration {
            // No other record of the pump's frame, or of an event after it,
            // goes ahead of the batch's `SEEN`.
            self.flush_seen(link);
        }
        let done = self
            .batch
            .done
            .min(self.batch.slots.len().max(self.batch.done));
        let elapsed = self.batch.started.map_or(0.0, |s| {
            now.saturating_duration_since(s).as_secs_f64() * 1e6
        });
        // A batch costs a fixed part plus its rows. The cost per row comes
        // from the clock reads inside the batch, between the first row's
        // sink and the last clock check, when there were two; the rest of
        // the batch's time is its fixed part. Without them, the batch's time
        // less the fixed part's estimate is spread over its rows.
        let inner = match (self.batch.first, self.batch.last) {
            (Some(a), Some((b, k))) if k > 1 => {
                Some(b.saturating_duration_since(a).as_secs_f64() * 1e6 / (k - 1) as f64)
            }
            _ => None,
        };
        self.window.spend.tier_us[tier.index()] += elapsed;
        self.window.spend.pump_work_us += elapsed;
        self.window.spend.tier_rows[tier.index()] += done as u64;
        let row_tier = !matches!(tier, Tier::Births | Tier::Enumeration);
        let per_row = match inner {
            // The host's own batch: its whole time over its rows, so the
            // sweep plans with what a row costs it all told; and no part in
            // the pump's estimates.
            _ if self.batch.here => elapsed / done.max(1) as f64,
            Some(per_row) if row_tier => {
                let fixed = (elapsed - per_row * done as f64).clamp(0.0, SLICE_CAP_US / 4.0);
                self.batch_us = if self.batch_us <= 0.0 {
                    fixed
                } else {
                    self.batch_us * 0.9 + fixed * 0.1
                };
                if let Some((b, k)) = self.batch.last {
                    let after = now.saturating_duration_since(b).as_secs_f64() * 1e6;
                    let tail = (after - per_row * done.saturating_sub(k) as f64).clamp(0.0, fixed);
                    self.tail_us = if self.tail_us <= 0.0 {
                        tail
                    } else {
                        self.tail_us * 0.9 + tail * 0.1
                    };
                }
                per_row
            }
            // Births and the enumeration time each step: their cost per row
            // leaves out the dear steps and the batch's fixed part.
            _ if !row_tier => {
                if self.batch.cheap > 0 {
                    self.batch.cheap_us / self.batch.cheap as f64
                } else if done > self.batch.dear {
                    (elapsed - self.batch.dear_us - self.batch_us).max(elapsed / 100.0)
                        / (done - self.batch.dear) as f64
                } else {
                    self.tiers[tier.index()].cost
                }
            }
            Some(per_row) => per_row,
            None => (elapsed - self.batch_us).max(elapsed / 100.0) / done.max(1) as f64,
        };
        // A row tier's batch without two clock reads cannot tell its rows
        // from its fixed part: its time would count as rows, and the planner
        // would plan ever smaller batches. It leaves the cost as it is; the
        // last planned row's clock read gives every batch of two rows or
        // more its own measure. An unmeasured tier takes it as a first
        // estimate, which later batches correct.
        // The host's own batch is timed as a whole, so it always measures.
        // A batch that cannot tell them apart still corrects a cost that is
        // too high: its whole time less the fixed part's estimate bounds
        // what its rows cost (LIVE, S10: one slow first read left
        // a tier at 1.5 ms a row, and every later batch had one row).
        let measured = self.batch.here
            || inner.is_some()
            || !row_tier
            || self.tiers[tier.index()].cost <= 0.0
            || per_row < self.tiers[tier.index()].cost;
        if done > 0 && measured {
            let t = &mut self.tiers[tier.index()];
            t.cost = if t.cost <= 0.0 {
                per_row
            } else {
                t.cost * 0.9 + per_row * 0.1
            };
        }
        if matches!(tier, Tier::Births | Tier::Enumeration) {
            // At a scaled budget the batch is a whole batch to the pump's
            // gate however few rows its scaled budget buys, so the balance
            // pays at least the share of a batch the scale withholds.
            let spent = elapsed.max(self.maintenance_batch() * (1.0 - self.scale()));
            self.pay_maintenance(elapsed, spent);
        }
        match tier {
            Tier::Births => self.births = self.births.saturating_sub(done),
            Tier::Enumeration => {}
            Tier::Stopped => {
                let from = done.min(self.batch.slots.len());
                let finished: Vec<u32> = self.batch.slots[..from].to_vec();
                for slot in finished {
                    if let Some(row) = self.row_mut(slot) {
                        row.in_stopped = false;
                    }
                }
                let rest: Vec<u32> = self.batch.slots[from..].to_vec();
                self.stopped.extend(rest);
            }
            Tier::Subscribed | Tier::Positions | Tier::Vitals | Tier::Warehouses => {
                let all = done.min(self.batch.slots.len());
                // The batch's first rows are the ones an event marked: those
                // read are no longer marked, and the rest wait in the list
                // again. Every row read notes when.
                let lead = std::mem::take(&mut self.batch.lead).min(self.batch.slots.len());
                if tier == Tier::Vitals {
                    for i in 0..self.batch.slots.len().max(lead) {
                        let Some(&slot) = self.batch.slots.get(i) else {
                            break;
                        };
                        let read = i < all;
                        if !read && i >= lead {
                            break;
                        }
                        let Some(row) = self.rows.get_mut(slot as usize).and_then(Option::as_mut)
                        else {
                            continue;
                        };
                        if read {
                            row.life_at = Some(now);
                            if i < lead {
                                row.life_marked = false;
                            }
                        } else if row.found.object != 0 {
                            self.life_marks.push(slot);
                        }
                    }
                }
                // A row no subscription names has its existence checked when
                // its life is read.
                if tier == Tier::Vitals {
                    let slow: Vec<u32> = self.batch.slots[..all]
                        .iter()
                        .copied()
                        .filter(|&s| self.slow_vitals_row(s))
                        .collect();
                    for slot in slow {
                        self.check_soon(slot);
                    }
                }
                // The cursor's rows: the ones after the marked.
                let done = all.saturating_sub(lead);
                if tier == Tier::Vitals {
                    self.window.vitals_swept += done as u64;
                    #[cfg(test)]
                    {
                        self.vitals_swept += done as u64;
                    }
                }
                let len = self.list_len(tier);
                let wrap = self.batch.wrap.take();
                let t = &mut self.tiers[tier.index()];
                t.credit = (t.credit - done as f64).max(0.0);
                if done > 0 && done <= self.batch.after.len() {
                    let end = self.batch.after[done - 1];
                    t.cursor = if end >= len { 0 } else { end };
                    let cursor = t.cursor;
                    // A batch that read a place past the end of the list
                    // completed the lap its first places were in, and the
                    // places it read are the next pass's. It also completes
                    // the lap when it reached the end or no place after its
                    // last row is due.
                    let past = wrap.is_some_and(|w| done > w);
                    let rest_empty = !past
                        && cursor > 0
                        && (cursor..len).all(|p| self.readable(tier, p).is_none());
                    if past || end >= len || rest_empty {
                        self.completed(tier, now);
                        if !past {
                            self.tiers[tier.index()].cursor = 0;
                        }
                    }
                }
            }
        }
        self.write_batch(link, tier);
    }

    fn write_batch(&mut self, link: &mut impl Port, tier: Tier) {
        let max = link.max_payload();
        let b = &mut self.batch;
        let scratch = &mut self.scratch;
        match tier {
            Tier::Subscribed | Tier::Positions => {
                if !b.positions.object.is_empty() {
                    write_split(link, kind::POSITIONS, &b.positions, max, scratch);
                }
                if !b.attitude.object.is_empty() {
                    write_split(link, kind::ATTITUDE, &b.attitude, max, scratch);
                }
            }
            Tier::Stopped => {
                if !b.attitude.object.is_empty() {
                    write_split(link, kind::ATTITUDE, &b.attitude, max, scratch);
                }
            }
            Tier::Vitals => {
                if !b.vitals.object.is_empty() {
                    write_split(link, kind::VITALS, &b.vitals, max, scratch);
                }
            }
            // A static's point, read when its row is added and at each
            // enumeration ([`Sampler::spot`]). The `OBJECT` records go
            // before the samples.
            Tier::Births | Tier::Enumeration if !b.positions.object.is_empty() => {
                link.flush();
                self.dirty = false;
                write_split(link, kind::POSITIONS, &b.positions, max, scratch);
            }
            _ => {}
        }
        // Cleared, not replaced: the batches keep their capacity.
        b.positions.object.clear();
        b.positions.x.clear();
        b.positions.y.clear();
        b.positions.z.clear();
        b.vitals.object.clear();
        b.vitals.life.clear();
        b.attitude.object.clear();
        b.attitude.heading.clear();
        b.attitude.pitch.clear();
        b.attitude.roll.clear();
    }

    // ---- sinks -----------------------------------------------------------

    /// Count one row done, read the clock every `rows_per_clock` rows, and
    /// say whether to stop.
    fn tick(&mut self, now: impl FnOnce() -> Instant) -> bool {
        self.batch.done += 1;
        if self.batch.here {
            // The host's own batch is bound by its plan and timed as a
            // whole: no clock read inside it.
            return false;
        }
        self.since_clock += 1;
        let first = self.batch.done == 1;
        let last = self.batch.done == self.batch.slots.len();
        if self.since_clock < self.rows_per_clock && !first && !last {
            return false;
        }
        // The first and the last planned row's sinks also read the clock,
        // so the batch's cost per row is measured apart from its fixed part.
        let t = now();
        if first {
            self.batch.first = Some(t);
        } else {
            self.batch.last = Some((t, self.batch.done));
        }
        self.since_clock = 0;
        !self.batch.past && self.spent(t)
    }

    /// The row the batch's `i`-th... the batch row for a slot: its object, if
    /// the slot is the next row of the batch and its row is unchanged.
    fn batch_row(&self, slot: u32) -> Option<u64> {
        let i = self.batch.done;
        if self.batch.slots.get(i) != Some(&slot) {
            return None;
        }
        let row = self.live(slot)?;
        (self.batch.versions.get(i) == Some(&row.version)).then_some(row.found.object)
    }

    /// The values the shim's plan buffer holds for a batch's slots: for
    /// positions without attitude, a unit's object ID, which the shim hands
    /// straight to `DCS.getUnitPositionByObjectId`, and minus the slot plus
    /// one for a static; for vitals, the slot plus one for a unit and minus
    /// it for a static; otherwise the slot plus one.
    pub fn buffer_values(&self, tier: Tier, flag: bool, slots: &[u32], out: &mut Vec<i64>) {
        out.clear();
        let ids = matches!(tier, Tier::Subscribed | Tier::Positions) && !flag;
        let vitals = tier == Tier::Vitals;
        out.extend(slots.iter().map(|&slot| {
            let unit = self.row(slot).filter(|r| r.found.kind == object_kind::UNIT);
            match unit {
                Some(r) if ids => r.found.object as i64,
                None if ids || vitals => -(i64::from(slot) + 1),
                _ => i64::from(slot) + 1,
            }
        }));
    }

    /// A position for the batch's next row; true to stop.
    pub fn position_next(&mut self, p: Option<[f64; 3]>, now: impl FnOnce() -> Instant) -> bool {
        match self.batch.slots.get(self.batch.done).copied() {
            Some(slot) => self.position(slot, p, now),
            None => self.tick(now),
        }
    }

    /// Leave the batch's next row out of every tier: its getter raised.
    pub fn suspect_next(&mut self) {
        if let Some(slot) = self.batch.slots.get(self.batch.done).copied() {
            self.suspect(slot);
        }
    }

    /// A position for a slot of the batch. Returns true to stop.
    pub fn position(
        &mut self,
        slot: u32,
        p: Option<[f64; 3]>,
        now: impl FnOnce() -> Instant,
    ) -> bool {
        if let (Some(object), Some(p)) = (self.batch_row(slot), p)
            && p.iter().all(|v| v.is_finite())
            && p != [0.0, 0.0, 0.0]
        {
            self.batch.positions.object.push(object);
            self.batch.positions.x.push(p[0]);
            self.batch.positions.y.push(p[1]);
            self.batch.positions.z.push(p[2]);
            self.moved(slot, p);
        } else if self.batch_row(slot).is_some() {
            self.check_soon(slot);
        }
        self.tick(now)
    }

    /// Attitude for a slot of the batch from the unit's `x` (forward) and `y`
    /// (up) vectors of `getPosition()`. Does not count a row: it goes with a
    /// position of a subscribed row, or alone for a stopped one.
    pub fn attitude(&mut self, slot: u32, x: [f64; 3], y: [f64; 3]) {
        let i = self.batch.done;
        // For the subscribed tier the position came first and counted the row.
        let row_index = if self.batch.tier == Some(Tier::Stopped) {
            i
        } else {
            i.saturating_sub(1)
        };
        if self.batch.slots.get(row_index) != Some(&slot) {
            return;
        }
        let Some(row) = self.live(slot) else { return };
        if self.batch.versions.get(row_index) != Some(&row.version) {
            return;
        }
        let object = row.found.object;
        if let Some((h, p, r)) = euler(x, y) {
            self.batch.attitude.object.push(object);
            self.batch.attitude.heading.push(h as f32);
            self.batch.attitude.pitch.push(p as f32);
            self.batch.attitude.roll.push(r as f32);
        }
    }

    /// A stopped row's attitude done. Returns true to stop.
    pub fn stopped_done(&mut self, now: impl FnOnce() -> Instant) -> bool {
        self.tick(now)
    }

    /// A static's point, which the shim reads when it adds the static's row
    /// and when an enumeration finds it: a static that cannot move has no
    /// other position read (spec/sampling.md, "Tiers"). It goes into the
    /// batch of births or of the enumeration in progress.
    pub fn spot(&mut self, object: u64, p: [f64; 3]) {
        if !matches!(self.batch.tier, Some(Tier::Births | Tier::Enumeration))
            || !p.iter().all(|v| v.is_finite())
        {
            return;
        }
        let Some(&slot) = self.by_object.get(&object) else {
            return;
        };
        let Some(row) = self.rows[slot as usize].as_mut() else {
            return;
        };
        if row.unit() {
            return;
        }
        row.last = Some(p);
        let b = &mut self.batch.positions;
        b.object.push(object);
        b.x.push(p[0]);
        b.y.push(p[1]);
        b.z.push(p[2]);
    }

    /// Life for a slot of the batch. Returns true to stop.
    pub fn vitals(&mut self, slot: u32, life: Option<f64>, now: impl FnOnce() -> Instant) -> bool {
        if let (Some(object), Some(life)) = (self.batch_row(slot), life) {
            self.batch.vitals.object.push(object);
            self.batch.vitals.life.push(life as f32);
        }
        self.tick(now)
    }

    /// One airbase of the batch: its owner and stock. Returns true to stop.
    pub fn airbase(
        &mut self,
        link: &mut impl Port,
        index: u32,
        airbase: Airbase,
        stock: Option<Lv>,
        now: impl FnOnce() -> Instant,
    ) -> bool {
        let i = self.batch.done;
        // An empty name means the shim writes nothing for this index this
        // pass: a FARP whose static is gone, or an airbase whose coalition
        // could not be read (spec/sampling.md). Its index stays.
        if self.batch.slots.get(i) == Some(&index) && !airbase.airbase.is_empty() {
            let max = link.max_payload();
            let name = airbase.airbase.clone();
            let a = airbase.encode();
            if a.len() <= max {
                link.sample(kind::AIRBASE as u16, &a);
            }
            if let Some(stock) = stock {
                let w = Warehouse {
                    airbase: name,
                    stock,
                }
                .encode();
                if w.len() <= max {
                    link.sample(kind::WAREHOUSE as u16, &w);
                }
            }
        }
        self.tick(now)
    }

    /// Births or enumeration items done. Returns true to stop.
    pub fn item(&mut self, now: impl FnOnce() -> Instant) -> bool {
        // Each step is one DCS call or one object's reads; time it alone
        // (spec/sampling.md, "Reporting").
        let t = now();
        self.mark_steps(t, 1);
        self.tick(|| t)
    }

    /// Account the steps since the last clock read at `t`: `this` more (the
    /// one ending now, or none) after the items [`Sampler::seen_item`]
    /// counted without reading the clock. Each gets the mean time.
    fn mark_steps(&mut self, t: Instant, this: usize) {
        let kind = usize::from(self.batch.tier == Some(Tier::Enumeration));
        let k = std::mem::take(&mut self.batch.unmarked) + this;
        let first = self.batch.done + this <= k;
        if let (Some(last), true) = (self.step_at, k > 0) {
            let total = t.saturating_duration_since(last).as_secs_f64() * 1e6;
            let us = total / k as f64;
            self.steps[kind][bucket(us)] += k as u64;
            self.step_max[kind] = self.step_max[kind].max(us);
            if us > SLICE_CAP_US / 4.0 {
                self.batch.dear += k;
                self.batch.dear_us += total;
            } else if !first {
                // The batch's first step holds its fixed part.
                self.batch.cheap += k;
                self.batch.cheap_us += total;
            }
        }
        self.step_at = Some(t);
    }

    /// One enumeration item for an object the enumeration found: [`Sampler::seen`],
    /// and, when the table has the object or it died, the item counted, so
    /// the shim makes one call. The clock is read every `rows_per_clock`
    /// items. Returns the slot plus one (0 when the table lacks the object:
    /// the shim reads it and calls [`Sampler::item`]), and whether to stop.
    pub fn seen_item(
        &mut self,
        link: &mut impl Port,
        object: u64,
        now: impl FnOnce() -> Instant,
    ) -> (Option<u32>, bool) {
        let found = self.seen(link, object);
        if found == Some(0) {
            return (found, false);
        }
        self.batch.done += 1;
        self.since_clock += 1;
        self.batch.unmarked += 1;
        let first = self.batch.done == 1;
        if self.since_clock < self.rows_per_clock && !first {
            return (found, false);
        }
        let t = now();
        self.mark_steps(t, 0);
        self.since_clock = 0;
        (found, !self.batch.past && self.spent(t))
    }

    /// Leave a row out of every tier until an event or an enumeration
    /// settles it: a getter raised on it.
    pub fn suspect(&mut self, slot: u32) {
        if let Some(row) = self.row_mut(slot)
            && row.found.object != 0
            && !std::mem::replace(&mut row.suspect, true)
        {
            self.recount = true;
        }
        self.check_soon(slot);
    }

    /// The running enumeration read everything: rows that were in the table
    /// at its start and that it did not find are queued for an existence
    /// check, which classifies them, and `ENUM_END` is queued.
    pub fn enumeration_done(&mut self, link: &mut impl Port, now: Instant) {
        let Some(n) = self.enumeration.take() else {
            return;
        };
        let missing: Vec<u32> = (0..self.rows.len() as u32)
            .filter(|&s| {
                self.present(s)
                    .is_some_and(|r| r.enum_member == n && !r.enum_seen)
            })
            .collect();
        // Each miss is checked at once; `ENUM_END` waits for the checks, so
        // the rows that leave are gone before it, and the rows that stay get
        // their `OBJECT` (spec/records.md, "Layout: ENUM_END").
        let mut pending = HashSet::new();
        for slot in missing {
            self.check_soon(slot);
            if let Some(object) = self.present(slot).map(|r| r.found.object) {
                self.want_check(object);
                pending.insert(object);
            }
        }
        if pending.is_empty() {
            self.finish_enumeration(link, n);
        } else {
            self.end_hold = Some(EndHold {
                n,
                pending,
                since: now,
            });
            self.handed.clear();
        }
        let begin = self.enum_last_begin;
        self.enum_ended_begins = (self.enum_ended_begins.1, begin);
        let t = &mut self.tiers[Tier::Enumeration.index()];
        t.period = match self.enum_ended_begins {
            (Some(a), Some(b)) => b.saturating_duration_since(a).as_secs_f64(),
            (None, Some(b)) => now.saturating_duration_since(b).as_secs_f64(),
            _ => 0.0,
        };
    }

    /// The enumeration that is running, if any.
    pub fn enumeration(&self) -> Option<u64> {
        self.enumeration
    }

    /// End the pump at `now`: close the batch. [`Sampler::end_frame`] does
    /// the accounting once the frame is published.
    pub fn end_pump(&mut self, link: &mut impl Port, clock: impl Fn() -> Instant) {
        self.finish_pump(link, clock, true);
    }

    /// Ends the pump. `timed` is false when `clock` does not advance across
    /// the close, so the close teaches no estimate.
    fn finish_pump(&mut self, link: &mut impl Port, clock: impl Fn() -> Instant, timed: bool) {
        let now = clock();
        // A miss handed out whose check neither `gone` nor `alive` reported
        // is undecided: it stays.
        for object in std::mem::take(&mut self.handed) {
            self.resolve_miss(link, object, true);
        }
        // The close that ends the pump costs what the one in `plan` does,
        // and teaches the same estimate: a pump of one batch has no other.
        let open = self.batch.tier.is_some();
        self.close_batch(link, now);
        // The pump ends after its last close, which `spent` includes.
        let end = clock();
        if open && timed {
            self.learn_close(end.saturating_duration_since(now).as_secs_f64() * 1e6);
        }
        if self.batches == 0 {
            self.empty_pumps += 1;
        }
        // A tier below the positions that owed rows and got none for lack of
        // budget waits for the pump's wait. A tier the rank held back
        // (`behind`) is not starved.
        if self.sampling && self.direct && !self.behind() {
            for tier in [Tier::Vitals, Tier::Warehouses] {
                let i = tier.index();
                if !self.planned[i] && self.tiers[i].credit >= 1.0 {
                    self.mark_unaffordable(tier, now);
                }
            }
        }
        self.sub_pump = false;
        self.pump_reserve = 0.0;
        let spent = self
            .pump_start
            .take()
            .map(|start| end.saturating_duration_since(start).as_secs_f64() * 1e6);
        if let Some(spent) = spent {
            self.window.spend.pump_us += spent;
            // Only row batches teach a batch's fixed part, so a pump that
            // planned none of them leaves its estimate as it is.
            if !(self.maintenance && !self.pump_rows) {
                self.batch_us *= 0.99;
            }
            // A pump that only resolved births or read the enumeration is
            // theirs whole: its call and its closes too. A pump that also
            // read rows or made checks is the row tiers' but for their
            // batches: charging them a share of its call held the
            // enumeration longer and slowed vitals in the host tests. Below
            // a scale of 1 each batch already pays the share of a whole
            // batch the scale withholds.
            if self.maintenance && !self.pump_rows && !self.checked && self.scale() >= 1.0 {
                let rest = (self.pump_call_us + spent - self.pump_paid).max(0.0);
                self.pay_maintenance(rest, rest);
            }
        }
        if self.sampling
            && let Some(spent) = spent
        {
            let over = spent - self.budget.budget.max(0.0);
            let maintenance = std::mem::take(&mut self.maintenance);
            if std::mem::take(&mut self.lone) {
                self.lone_rows += 1;
            } else if self.batches == 0 {
                // A pump that planned no batch, because the scaled budget
                // pays for no row. With no check its spend is the call's own
                // cost, which `empty_pumps` counts; with checks it is a
                // check pump, counted apart. Neither is a frame of rows.
                if std::mem::take(&mut self.checked) {
                    self.check_pumps += 1;
                    self.check_overrun = self.check_overrun.max(over);
                }
            } else if maintenance {
                self.maintenance_frames += 1;
                self.maintenance_overrun = self.maintenance_overrun.max(over);
            } else {
                self.overrun_max = self.overrun_max.max(over);
                self.row_frames += 1;
                if over > self.rows_per_clock as f64 * self.cheapest_cost() {
                    self.overrun_frames += 1;
                    self.overrun_sum_us += over;
                }
            }
        }
        self.checked = false;
        self.sampling = false;
        if self.dirty {
            link.flush();
            self.dirty = false;
        }
    }

    /// The frame ended at `now`: account its agent time (spec/sampling.md,
    /// "Slice"), and store the slice and periods for the status block.
    /// Returns a `sampling` report when one is due.
    pub fn end_frame(&mut self, link: &mut impl Port, now: Instant) -> Option<Report> {
        if self.batch.tier.is_some() {
            self.finish_pump(link, || now, false);
        }
        let share = link.overhead_share();
        let frame_us = now.saturating_duration_since(link.began()).as_secs_f64() * 1e6;
        let b = &mut self.budget;
        let commanded_us = std::mem::take(&mut b.commanded_us);
        link.set_frame_commanded(commanded_us);
        let b = &mut self.budget;
        let agent_us = (frame_us - commanded_us).max(0.0) + std::mem::take(&mut b.events_us);
        let cap = share * SECOND_US;
        let start = *b.syscall_window_start.get_or_insert(now);
        if now.saturating_duration_since(start) >= Duration::from_secs(1) {
            let secs = now.saturating_duration_since(start).as_secs_f64();
            let rate = b.syscall_window / secs;
            b.syscall_per_s = if b.syscall_per_s == 0.0 {
                rate
            } else {
                b.syscall_per_s * 0.8 + rate * 0.2
            };
            b.syscall_window = 0.0;
            b.syscall_window_start = Some(now);
        }
        let mut forgiven = 0.0;
        if link.syscall_frame() {
            b.syscall_window += agent_us;
            self.window.spend.syscall_us += agent_us;
        } else {
            let interval = link.frame_interval_us();
            let allowance = share * interval;
            let charge = b.syscall_per_s * interval / 1e6;
            // Births and the enumeration keep an account of their own: the
            // time their balance paid leaves the debt, and what the balance
            // earned leaves the allowance (spec/sampling.md, "Tiers").
            let spent = (agent_us + std::mem::take(&mut b.withheld)
                - std::mem::take(&mut self.maintenance_paid))
            .max(0.0);
            let earned = std::mem::take(&mut self.maintenance_earned);
            let d = b.debt + spent - (allowance - charge - earned);
            if d > cap {
                forgiven = d - cap;
            }
            // Credit stops at 150 µs, so a whole pump frame, its closing
            // and finish included, stays under 250 µs.
            b.debt = d.clamp(-CREDIT_CAP_US, cap);
        }
        self.frames += 1;
        let w = &mut self.window;
        let wstart = *w.start.get_or_insert(now);
        w.agent_us += agent_us;
        w.forgiven_us += forgiven;
        let rpc = self.rows_per_clock();
        self.rows_per_clock = rpc;
        link.set_sampling(self.budget.slice.max(0.0), self.periods_at(Some(now)), rpc);
        let elapsed = now.saturating_duration_since(wstart);
        if elapsed < Duration::from_secs(10) {
            return None;
        }
        let report = Report {
            window_s: elapsed.as_secs_f64(),
            events_us: self.window.events_us,
            share: self.window.agent_us / (elapsed.as_secs_f64() * 1e6),
            syscall_per_s: self.budget.syscall_per_s,
            debt_us: self.budget.debt.max(0.0),
            credit_us: (-self.budget.debt).max(0.0),
            pumps: std::mem::take(&mut self.pumps),
            empty_pumps: std::mem::take(&mut self.empty_pumps),
            frames: std::mem::take(&mut self.frames),
            slice_us: self.budget.slice,
            rows: self.len(),
            births: self.births,
            periods: self.periods_at(Some(now)),
            forgiven_us: self.window.forgiven_us,
            scale: self.scale(),
            overrun_us: std::mem::take(&mut self.overrun_max),
            clock_interval_us: self.rows_per_clock as f64 * self.cheapest_cost(),
            starved: std::mem::take(&mut self.starved),
            lone_rows: std::mem::take(&mut self.lone_rows),
            maintenance_frames: std::mem::take(&mut self.maintenance_frames),
            maintenance_overrun_us: std::mem::take(&mut self.maintenance_overrun),
            check_pumps: std::mem::take(&mut self.check_pumps),
            check_overrun_us: std::mem::take(&mut self.check_overrun),
            row_frames: std::mem::take(&mut self.row_frames),
            overrun_frames: std::mem::take(&mut self.overrun_frames),
            overrun_sum_us: std::mem::take(&mut self.overrun_sum_us),
            costs: self.tiers.map(|t| t.cost),
            xcall_us: self.xcall_us,
            xcall_seen_us: self.xcall_seen,
            close_us: self.close_us,
            batch_us: self.batch_us,
            spend: self.window.spend,
            idle: self.idle_units + self.idle_cargo,
            marked: self.life_marks.len(),
            sweeps: std::mem::take(&mut self.sweeps),
            slow_periods: self.slow_periods(now),
            sweep_row_us: self.sweep_row_us,
            sweep_batch_us: self.sweep_batch_us,
            sweep_cut: std::mem::take(&mut self.sweep_cut),
            sweep_overruns: std::mem::take(&mut self.sweep_overruns),
            sweep_forced: std::mem::take(&mut self.sweep_forced),
            sweep_waits: std::mem::take(&mut self.sweep_waits),
            maintenance_us: self.maintenance_us,
            maintenance_forgiven_us: self.window.maintenance_forgiven_us,
            vitals_due: self.window.vitals_due,
            vitals_swept: self.window.vitals_swept,
            sweep_overrun_us: std::mem::take(&mut self.sweep_overrun_max),
            steps: [0, 1].map(|k| {
                (
                    quantile(&self.steps[k], 0.5),
                    quantile(&self.steps[k], 0.99),
                    std::mem::take(&mut self.step_max[k]),
                )
            }),
        };
        self.window = Window {
            start: Some(now),
            ..Window::default()
        };
        Some(report)
    }

    /// The achieved periods: position, vitals, warehouse, enumeration.
    pub fn periods(&self) -> [f64; 4] {
        self.periods_at(None)
    }

    /// The achieved periods at `now`: each the longer of the last completed
    /// period and the time since the last completion, so a tier that stalls
    /// shows its stall as it grows (spec/sampling.md, "Achieved period").
    pub fn periods_at(&self, now: Option<Instant>) -> [f64; 4] {
        let since = |t: Option<Instant>| match (now, t) {
            (Some(n), Some(t)) => n.saturating_duration_since(t).as_secs_f64(),
            _ => 0.0,
        };
        // A tier that has owed rows and never completed a sweep shows the
        // time since it first owed them, so a starved tier's period grows;
        // 0 means it has never owed rows (spec/sampling.md, "Achieved
        // period").
        let tier = |t: Tier| {
            let s = &self.tiers[t.index()];
            if s.period > 0.0 {
                s.period.max(since(s.last_complete))
            } else {
                since(s.owed_since)
            }
        };
        let e = &self.tiers[Tier::Enumeration.index()];
        let enumeration = if e.period > 0.0 {
            e.period.max(since(self.enum_ended_begins.1))
        } else {
            since(e.owed_since)
        };
        [
            tier(Tier::Positions),
            tier(Tier::Vitals),
            tier(Tier::Warehouses),
            enumeration,
        ]
    }

    /// The achieved period of the subscribed positions, seconds.
    pub fn subscribed_period(&self) -> f64 {
        self.tiers[Tier::Subscribed.index()].period
    }

    /// The debt, µs.
    pub fn debt_us(&self) -> f64 {
        self.budget.debt.max(0.0)
    }

    /// Why this frame's sweep read no row, when it owed rows and read none.
    pub fn sweep_wait(&self) -> Option<SweepWait> {
        self.sweep_wait
    }

    /// Allowance carried as credit, µs.
    pub fn credit_us(&self) -> f64 {
        (-self.budget.debt).max(0.0)
    }

    /// Each tier's running cost per row, µs.
    #[cfg(test)]
    fn costs(&self) -> [f64; 7] {
        self.tiers.map(|t| t.cost)
    }

    /// The running estimate of a batch's fixed cost, µs.
    #[cfg(test)]
    fn batch_us(&self) -> f64 {
        self.batch_us
    }

    /// The last frame's slice, µs.
    pub fn slice_us(&self) -> f64 {
        self.budget.slice
    }

    /// The cheapest tier's cost per row, µs; infinite before one is measured.
    fn cheapest_cost(&self) -> f64 {
        [Tier::Positions, Tier::Vitals, Tier::Subscribed]
            .iter()
            .map(|t| self.tiers[t.index()].cost)
            .filter(|c| *c > 0.0)
            .fold(f64::INFINITY, f64::min)
    }

    /// Rows between clock reads: clock reads under 2% of the cheapest tier's
    /// rows.
    pub fn rows_per_clock(&self) -> u64 {
        let cheapest = self.cheapest_cost();
        if !cheapest.is_finite() || self.clock_cost_us <= 0.0 {
            return 1;
        }
        (self.clock_cost_us / (CLOCK_SHARE * cheapest))
            .ceil()
            .clamp(1.0, 1024.0) as u64
    }
}

/// The `sampling` report of spec/sampling.md, "Reporting".
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// The window's real time, seconds.
    pub window_s: f64,
    /// Event handling time in the window, µs: part of `share`.
    pub events_us: f64,
    /// Agent time over real time in the window.
    pub share: f64,
    /// `C`, µs per second.
    pub syscall_per_s: f64,
    /// `D`, µs.
    pub debt_us: f64,
    /// Allowance carried as credit, µs.
    pub credit_us: f64,
    /// Frames in the window that called the shim's pump, and all frames.
    pub pumps: u64,
    /// Pumps in the window that planned no row.
    pub empty_pumps: u64,
    /// All frames in the window.
    pub frames: u64,
    /// `S` of the last frame, µs.
    pub slice_us: f64,
    /// Rows in the table.
    pub rows: usize,
    /// Births the shim holds, waiting to be resolved.
    pub births: usize,
    /// The achieved periods.
    pub periods: [f64; 4],
    /// Debt dropped by the cap in the window, µs.
    pub forgiven_us: f64,
    /// The sampling budget's scale `k`.
    pub scale: f64,
    /// The most a frame's sampling ran past its budget in the window, µs.
    pub overrun_us: f64,
    /// What `rows_per_clock` rows of the cheapest tier cost, µs: the most a
    /// frame's sampling can run past its budget between two clock reads.
    pub clock_interval_us: f64,
    /// Frames in the window whose fixed work reached the allowance.
    pub starved: u64,
    /// Frames in the window that took one row costing more than the whole
    /// budget; their time is not in `overrun_us`.
    pub lone_rows: u64,
    /// Frames in the window that resolved births or ran the enumeration,
    /// whose steps are single DCS calls; their time is not in `overrun_us`.
    pub maintenance_frames: u64,
    /// The most such a frame ran past its budget, µs.
    pub maintenance_overrun_us: f64,
    /// Pumps that planned no row but made existence checks, and the most one
    /// ran past its budget, µs. Their time is in neither `overrun_us` nor
    /// `empty_pumps`' call cost alone.
    pub check_pumps: u64,
    /// The most such a pump ran past its budget, µs.
    pub check_overrun_us: f64,
    /// Frames in the window that only swept rows.
    pub row_frames: u64,
    /// Of those, frames that ran past the budget by more than one clock
    /// interval.
    pub overrun_frames: u64,
    /// What those frames ran past the budget in all, µs.
    pub overrun_sum_us: f64,
    /// Each tier's running cost per row, µs, in [`Tier::ALL`] order.
    pub costs: [f64; 7],
    /// Birth (0) and enumeration (1) steps: the upper edges of the buckets
    /// holding their p50 and p99 since the agent started, µs, and the
    /// largest step in the window.
    pub steps: [(f64, f64, f64); 2],
    /// The running estimate of the cross-state call, µs.
    pub xcall_us: f64,
    /// The running average of the calls measured, µs: the first call
    /// measured sets it, each later call weighs 0.1, each counts at most
    /// 62.5 µs, with no decay between pumps. 0 before the first pump, when
    /// `pump_worth` uses 20 µs. The call the pump bound uses.
    pub xcall_seen_us: f64,
    /// The running estimate of closing a batch, µs.
    pub close_us: f64,
    /// The running estimate of a batch's fixed cost, µs.
    pub batch_us: f64,
    /// Where the window's agent time went.
    pub spend: Spend,
    /// Units and cargo statics read less often because they have not moved.
    pub idle: usize,
    /// Rows an event marked for a read of life that still wait.
    pub marked: usize,
    /// The host's sweeps of positions in the window.
    pub sweeps: u64,
    /// The achieved period of the slow sweep of life, seconds, for
    /// the rows read once in several sweeps.
    pub slow_periods: f64,
    /// The running cost of one row inside the host's sweep, µs: the getter
    /// under `lua_pcall` and the row's own work. `costs` holds the sweep's
    /// whole time over its rows, which plans it.
    pub sweep_row_us: f64,
    /// The running cost of a sweep apart from its rows, µs: its planning and
    /// its record.
    pub sweep_batch_us: f64,
    /// Sweeps in the window that their deadline stopped before the planned
    /// rows were read.
    pub sweep_cut: u64,
    /// Sweeps in the window that ran past their budget by more than the rows
    /// between two clock reads cost.
    pub sweep_overruns: u64,
    /// The most a sweep in the window ran past its budget, µs.
    pub sweep_overrun_us: f64,
    /// Sweeps in the window that took one row because their fixed part
    /// alone passed their budget. They are in neither count above.
    pub sweep_forced: u64,
    /// Frames in the window on which the sweep owed rows and read none, by
    /// why, in [`SweepWait`]'s order: the subscribed pump, the minimum
    /// batch, the debt, the savings, the slice.
    pub sweep_waits: [u64; 5],
    /// The births' and the enumeration's balance at the window's end, µs.
    pub maintenance_us: f64,
    /// Their time in the window that passed their balance's bound, µs:
    /// forgiven, and not the row tiers' debt. Only a stall inside a step
    /// passes it.
    pub maintenance_forgiven_us: f64,
    /// Vitals rows due in the window at the target period: the period as a
    /// rate is the target × `vitals_due` / `vitals_swept`.
    pub vitals_due: f64,
    /// Vitals rows the cursor read in the window, event-marked reads apart.
    pub vitals_swept: u64,
}

/// The histogram bucket of a value in µs (spec/transport.md, "Metrics").
fn bucket(us: f64) -> usize {
    nanoprobe_transport::status::bucket(us)
}

/// The upper edge of the bucket holding the quantile `q`, µs; 0 when empty.
fn quantile(h: &[u64; 32], q: f64) -> f64 {
    let total: u64 = h.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let want = (total as f64 * q).ceil() as u64;
    let mut seen = 0;
    for (i, c) in h.iter().enumerate() {
        seen += c;
        if seen >= want {
            return 2f64.powi(i as i32);
        }
    }
    0.0
}

fn complete(t: &mut TierState, now: Instant) {
    let since = t.last_complete.or(t.sweep_start);
    if let Some(since) = since {
        t.period = now.saturating_duration_since(since).as_secs_f64();
    }
    t.last_complete = Some(now);
    t.sweep_start = Some(now);
}

fn object_record(row: &Row, enumeration: u64) -> Object {
    let f = &row.found;
    Object {
        object: f.object,
        group: f.group,
        entity: row.entity,
        group_entity: row.group_entity,
        enumeration,
        kind: f.kind,
        category: f.category,
        coalition: f.coalition,
        country: f.country,
        life0: f.life0,
        name: f.name.clone(),
        type_name: f.type_name.clone(),
        group_name: f.group_name.clone(),
    }
}

/// A sample batch type the split writer handles.
trait Batched: Sized {
    fn rows(&self) -> usize;
    fn row_bytes() -> usize;
    fn slice(&self, from: usize, to: usize) -> Self;
    fn encode(&self) -> Vec<u8>;
    /// The same payload as `encode`, written over `out`, for columns of
    /// equal lengths: no allocation once `out` has the capacity.
    fn encode_into(&self, out: &mut Vec<u8>);
}

/// A sample batch's first eight bytes: the row count and the padding
/// (spec/records.md, "Samples").
fn head(out: &mut Vec<u8>, rows: usize) {
    out.clear();
    out.extend_from_slice(&(rows as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
}

impl Batched for Positions {
    fn rows(&self) -> usize {
        self.object.len()
    }
    fn row_bytes() -> usize {
        32
    }
    fn slice(&self, a: usize, b: usize) -> Self {
        Positions {
            object: self.object[a..b].to_vec(),
            x: self.x[a..b].to_vec(),
            y: self.y[a..b].to_vec(),
            z: self.z[a..b].to_vec(),
        }
    }
    fn encode(&self) -> Vec<u8> {
        Positions::encode(self)
    }
    fn encode_into(&self, out: &mut Vec<u8>) {
        head(out, self.object.len());
        self.object
            .iter()
            .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        for column in [&self.x, &self.y, &self.z] {
            column
                .iter()
                .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        }
    }
}

impl Batched for Vitals {
    fn rows(&self) -> usize {
        self.object.len()
    }
    fn row_bytes() -> usize {
        12
    }
    fn slice(&self, a: usize, b: usize) -> Self {
        Vitals {
            object: self.object[a..b].to_vec(),
            life: self.life[a..b].to_vec(),
        }
    }
    fn encode(&self) -> Vec<u8> {
        Vitals::encode(self)
    }
    fn encode_into(&self, out: &mut Vec<u8>) {
        head(out, self.object.len());
        self.object
            .iter()
            .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        self.life
            .iter()
            .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
    }
}

impl Batched for Attitude {
    fn rows(&self) -> usize {
        self.object.len()
    }
    fn row_bytes() -> usize {
        20
    }
    fn slice(&self, a: usize, b: usize) -> Self {
        Attitude {
            object: self.object[a..b].to_vec(),
            heading: self.heading[a..b].to_vec(),
            pitch: self.pitch[a..b].to_vec(),
            roll: self.roll[a..b].to_vec(),
        }
    }
    fn encode(&self) -> Vec<u8> {
        Attitude::encode(self)
    }
    fn encode_into(&self, out: &mut Vec<u8>) {
        head(out, self.object.len());
        self.object
            .iter()
            .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        for column in [&self.heading, &self.pitch, &self.roll] {
            column
                .iter()
                .for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
        }
    }
}

/// Write a batch as sample records of at most `max` bytes each.
fn write_split<B: Batched>(
    link: &mut impl Port,
    k: u64,
    batch: &B,
    max: usize,
    scratch: &mut Vec<u8>,
) {
    let per = ((max.saturating_sub(8)) / B::row_bytes()).max(1);
    if batch.rows() <= per {
        // The usual case: one record, encoded with no allocation.
        batch.encode_into(scratch);
        link.sample(k as u16, scratch);
        return;
    }
    let mut a = 0;
    while a < batch.rows() {
        let b = (a + per).min(batch.rows());
        link.sample(k as u16, &batch.slice(a, b).encode());
        a = b;
    }
}

/// Heading, pitch and roll, radians, from the forward and up vectors of a
/// DCS position (x north, y up, z east).
pub fn euler(x: [f64; 3], y: [f64; 3]) -> Option<(f64, f64, f64)> {
    if !x.iter().chain(&y).all(|v| v.is_finite()) {
        return None;
    }
    let mut heading = x[2].atan2(x[0]);
    if heading < 0.0 {
        heading += std::f64::consts::TAU;
    }
    let pitch = x[1].clamp(-1.0, 1.0).asin();
    // Roll: the up vector's sideways lean, against the level right vector.
    let right = [-x[2], 0.0, x[0]];
    let norm = (right[0] * right[0] + right[2] * right[2]).sqrt();
    let roll = if norm > 1e-9 {
        let side = (y[0] * right[0] + y[2] * right[2]) / norm;
        (-side).atan2(y[1])
    } else {
        0.0
    };
    Some((heading, pitch, roll))
}

/// The `EntityId` a platform name gives under `campaign`, or 0
/// (spec/world.md, "Names"). The name's tag must be the campaign's and its
/// marker must match the kind: `.` a unit, `#` a static, `!` a group.
pub fn name_entity(name: &str, campaign: u64, kind: u32) -> u64 {
    use nanoprobe_transport::names::{self, Kind};
    if campaign == 0 {
        return 0;
    }
    let want = match kind {
        object_kind::UNIT => Kind::Unit,
        object_kind::STATIC => Kind::Static,
        object_kind::GROUP => Kind::Group,
        _ => return 0,
    };
    match names::parse(name) {
        Some(p) if p.kind == want && p.tag == names::tag_of(campaign) => p.entity,
        _ => 0,
    }
}

/// The cost of one clock read, µs: the mean of 64 reads.
pub fn clock_cost() -> f64 {
    let start = Instant::now();
    let mut last = start;
    for _ in 0..64 {
        last = std::hint::black_box(Instant::now());
    }
    last.saturating_duration_since(start).as_secs_f64() * 1e6 / 64.0
}

#[cfg(test)]
#[path = "sampler_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sampler_gone_tests.rs"]
mod gone_tests;

#[cfg(test)]
#[path = "sampler_seen_tests.rs"]
mod seen_tests;

#[cfg(test)]
#[path = "sampler_loadout_tests.rs"]
mod loadout_tests;
