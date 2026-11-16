//! The agent's link, driven by DCS: opened once per process from the hook,
//! pumped on every `onSimulationFrame`, and told about each lifecycle change
//! (spec/transport.md).
//!
//! On the frame that makes the link's system calls the driver also looks for
//! the kill switch and the shim reload request, and writes the agent's log
//! (spec/install.md). No other frame makes a system call.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nanoprobe_agent_link::config::{AgentToml, read_token};
use nanoprobe_agent_link::revisions::RevKey;
pub use nanoprobe_agent_link::sampler::{Found, Read};
use nanoprobe_agent_link::sampler::{Rates, Report, Sampler, clock_cost};
use nanoprobe_agent_link::{Admit, Link, Options};
use nanoprobe_transport::codec::{Lv, Reader, Writer};
use nanoprobe_transport::layout::{
    HandlerState, kind, lifecycle, moderation, opcode as op, opcode_row, result, volume,
};
use nanoprobe_transport::log::{Level, Value};
use nanoprobe_transport::names::{self, Kind};
use nanoprobe_transport::records::{
    Airbase, AuthorEvent, Bind, CommandHeader, Event, ExtensionStats, Log, Menu, Player, Subscribe,
};

use crate::state;

/// What [`frame`] tells the hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Nothing to do.
    Nothing = 0,
    /// Reload the shim: `<root>/reload` was found and removed.
    Reload = 1,
    /// The agent is disabled; the hook stops calling.
    Disabled = 2,
    /// No link is open.
    NoLink = 3,
}

/// The control opcodes the agent handles (spec/commands.md).
const CAMPAIGN: u16 = 1;
const ENUMERATE: u16 = 2;
const RATES: u16 = 3;
const SUBSCRIBE: u16 = 4;
const BIND: u16 = 5;
const BURST: u16 = 6;

/// The `event_field` codes the table reads from a death event.
const INITIATOR: u16 = 2;
const TARGET: u16 = 3;
const INITIATOR_CATEGORY: u16 = 17;

/// Breadcrumb codes for the status block's `crumb` (spec/transport.md,
/// "Breadcrumbs").
pub mod crumbs {
    /// A command's handler runs: this plus its opcode, with `crumb_arg` its
    /// sequence number.
    pub const COMMAND: u64 = 0x1_0000;
}

struct Driver {
    link: Link,
    sampler: Sampler,
    /// The DCS event IDs that remove a row, from the shim.
    deaths: Vec<u32>,
    /// The DCS event IDs after which the target's life is read, after which
    /// the initiator's ammunition is read, and of a birth.
    life_events: Vec<u32>,
    ammo_events: Vec<u32>,
    birth_events: Vec<u32>,
    /// The DCS event IDs after which the target's existence is checked.
    watch_events: Vec<u32>,
    /// A frame began and its `finish` has not run.
    open: bool,
    root: PathBuf,
    /// The commanded-work budget per frame.
    commanded: Duration,
    theater: String,
    mission: String,
    payload: Vec<u8>,
    /// A reload request could not be removed, and that was logged.
    reload_failed: bool,
    /// The hook has a shim loaded in the `server` state.
    shim: bool,
    /// This frame makes the agent's system calls.
    syscall: bool,
    /// A command with a `server` handler waits at the ring's head.
    waiting: bool,
    /// A command with a `hook` handler waits at the ring's head.
    hook_waiting: bool,
    /// The shim reported an extension with work for the pump: a hook for
    /// events or frames, a timer, or a callback waiting.
    ext_active: bool,
    /// This frame's pump is for commands only: it samples nothing.
    forced: bool,
    /// The pump called `command` at least once.
    pumped: bool,
    /// The time this frame's handlers spent from take to done.
    handler_time: Duration,
    /// When `frame` returned, for a pump for commands only.
    pump_started: Instant,
    /// A command ran this frame.
    ran: bool,
    /// The frame's commanded work so far.
    spent: Duration,
    /// The command the shim's handler runs, until `command_done`.
    outstanding: Option<Outstanding>,
    /// Whether the sampler began in this pump, and whether it samples.
    begun: Option<bool>,
    /// The time this pump spent on commands before the sampler began.
    pump_commands: Duration,
    /// The arguments of the command `command` returns, encoded.
    staged: Vec<u8>,
}

/// A command taken in the pump whose handler runs in Lua.
struct Outstanding {
    opcode: u16,
    header: CommandHeader,
    started: Instant,
}

/// What the next command at the ring's head needs.
enum Step {
    /// None waits, or the frame waits for the shim to load.
    Empty,
    /// A command with a `server` handler; it stays at the head.
    Lua,
    /// A command with a `hook` handler and a mission to run in; it stays at
    /// the head for the hook to take.
    Hook,
    /// A command handled in Rust, and the time it counts.
    Done(Duration),
    /// A command taken and admitted for its Lua handler.
    Handler(u16),
}

/// Reads one unit's position by its object ID in the caller's Lua state:
/// `Ok(None)` when the getter gave no position, `Err` when it raised.
pub type ReadPosition<'a> = dyn FnMut(u64) -> Result<Option<[f64; 3]>, ()> + 'a;

static DRIVER: Mutex<Option<Driver>> = Mutex::new(None);

/// Drop the link after a caught panic: its sockets close, so the sidecar
/// sees the session end. The mapped file stays mapped for the trip's
/// `DISABLED`.
pub fn drop_link() {
    let mut guard = match DRIVER.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    // Detach as the kill switch does, so the file shows no attached sidecar,
    // then close the sockets. The link may be what panicked, so a second
    // panic here is caught and the link dropped anyway.
    if let Some(d) = guard.as_mut() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            d.link.disable("a caught panic");
        }));
    }
    *guard = None;
}

/// Run `f` on the driver, if the link is open.
fn with<T>(f: impl FnOnce(&mut Driver) -> T) -> Option<T> {
    let mut guard = DRIVER.try_lock().ok()?;
    guard.as_mut().map(f)
}

/// Write the link's diagnostic lines to the agent's log, and as `LOG`
/// records to the sidecar.
fn notes(d: &mut Driver) {
    for note in d.link.take_notes() {
        state::log(Level::Info, &note, &[]);
        enqueue_log(&mut d.link, 3, "agent", &note, Lv::Nil);
    }
}

/// A record of the agent's own: to `agent.log`, and to the sidecar as a
/// `LOG` record (spec/log.md).
fn record(d: &mut Driver, level: Level, msg: &str, data: &[(&str, Value<'_>)]) {
    state::log(level, msg, data);
    let lv = if data.is_empty() {
        Lv::Nil
    } else {
        Lv::Table(
            data.iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::Str(s) => Lv::String(s.as_bytes().to_vec()),
                        Value::Num(n) => Lv::Number(*n),
                        Value::Int(i) => Lv::Number(*i as f64),
                        Value::Bool(b) => Lv::Bool(*b),
                        Value::Null => Lv::Nil,
                    };
                    (Lv::String(k.as_bytes().to_vec()), v)
                })
                .collect(),
        )
    };
    enqueue_log(&mut d.link, level_code(level), "agent", msg, lv);
}

/// The `LOG` record's level code.
fn level_code(level: Level) -> u32 {
    match level {
        Level::Error => 1,
        Level::Warn => 2,
        Level::Info => 3,
        Level::Debug => 4,
    }
}

/// Queue a `LOG` record. One that would pass the payload limit goes with
/// nil data and its message cut (spec/records.md, "Oversized records").
fn enqueue_log(link: &mut Link, level: u32, target: &str, msg: &str, data: Lv) {
    let mut log = Log {
        level,
        target: target.into(),
        msg: msg.into(),
        data,
    };
    let mut payload = log.encode();
    if payload.len() > link.outbound().ring().max_payload() {
        log.data = Lv::Nil;
        let mut end = log.msg.len().min(1024);
        while !log.msg.is_char_boundary(end) {
            end -= 1;
        }
        log.msg.truncate(end);
        payload = log.encode();
    }
    link.enqueue(kind::LOG as u16, payload);
}

/// The install's build as a number: its first 16 hex digits, or 0.
fn build_number() -> u64 {
    let b = state::build();
    u64::from_str_radix(b.get(..16).unwrap_or(b), 16).unwrap_or(0)
}

/// Open the link: read `agent.toml` and the token from `root`, create the
/// mapped file and listen. A second call does nothing. Returns the mapped
/// file's path.
pub fn start(root: &Path, dcs_version: &str) -> Result<String, String> {
    let mut guard = DRIVER.try_lock().map_err(|_| "the driver is busy")?;
    if let Some(d) = guard.as_ref() {
        return Ok(d.link.map_path().display().to_string());
    }
    let config = AgentToml::read(&root.join("agent.toml"))?;
    let token = match read_token(&root.join("token")) {
        Ok(t) => Some(t),
        Err(e) => {
            state::log(
                Level::Error,
                "no usable token, so no handshake",
                &[("error", Value::Str(&e))],
            );
            None
        }
    };
    let mut opts = Options::from_toml(&config, token);
    opts.build = build_number();
    opts.interface = crate::ABI_VERSION.parse().unwrap_or(0);
    opts.dcs_version = dcs_version.into();
    let path = opts.map_path.display().to_string();
    let mut link =
        Link::open(opts).map_err(|e| format!("could not open the mapped file {path}: {e}"))?;
    state::set_map(link.map().clone(), Instant::now());
    link.lifecycle(lifecycle::STARTING as u32, "", "");
    let rates = Rates {
        position_s: config.sampling.position_s,
        vitals_s: config.sampling.vitals_s,
        ammo_s: config.sampling.ammo_s,
        warehouse_s: config.sampling.warehouse_s,
        enumeration_s: config.sampling.enumeration_s,
    };
    let mut d = Driver {
        sampler: Sampler::new(rates, clock_cost()),
        deaths: Vec::new(),
        life_events: Vec::new(),
        ammo_events: Vec::new(),
        birth_events: Vec::new(),
        watch_events: Vec::new(),
        open: false,
        link,
        root: root.to_path_buf(),
        commanded: Duration::from_secs_f64(config.budget.commanded_ms / 1000.0),
        theater: String::new(),
        mission: String::new(),
        payload: Vec::new(),
        reload_failed: false,
        shim: false,
        syscall: false,
        waiting: false,
        hook_waiting: false,
        ext_active: false,
        forced: false,
        pumped: false,
        handler_time: Duration::ZERO,
        pump_started: Instant::now(),
        ran: false,
        spent: Duration::ZERO,
        outstanding: None,
        begun: None,
        pump_commands: Duration::ZERO,
        staged: Vec::new(),
    };
    let boot = format!("{:016x}", d.link.boot());
    record(
        &mut d,
        Level::Info,
        "link open",
        &[
            ("map", Value::Str(&path)),
            ("port", Value::Int(i64::from(config.transport.port))),
            ("boot", Value::Str(&boot)),
        ],
    );
    notes(&mut d);
    *guard = Some(d);
    Ok(path)
}

/// Fault injection for the S8 tests, built only with the `faults` feature
/// (spec/faults.md). `inject` arms a fault; the next frame takes it.
#[cfg(feature = "faults")]
pub mod faults {
    use std::sync::Mutex;

    /// The faults armed for the next frame.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Armed {
        /// The `frame` entry raises a Lua error instead of pumping.
        pub raise: bool,
        /// The frame panics after it began, holding the driver.
        pub panic: bool,
        /// The frame sleeps this many milliseconds after it began.
        pub stall_ms: u64,
        /// The frame queues this many 1 KiB `LOG` records.
        pub flood: u64,
    }

    pub(super) static ARMED: Mutex<Armed> = Mutex::new(Armed {
        raise: false,
        panic: false,
        stall_ms: 0,
        flood: 0,
    });

    /// The longest stall `inject` accepts.
    pub const STALL_MAX_MS: u64 = 10_000;
    /// The most flood records `inject` accepts.
    pub const FLOOD_MAX: u64 = 100_000;

    /// Arm the fault `name` with `arg`: `raise`, `panic`, `stall` (ms),
    /// `flood` (records), `lose-control`, which drops the link's next
    /// control record, or `sampling`, which scales the sampling budget by
    /// `arg` from now on.
    pub fn inject(name: &str, arg: f64) -> Result<(), String> {
        let n = if arg.is_finite() && arg >= 0.0 {
            arg as u64
        } else {
            0
        };
        if name == "lose-control" {
            return super::with(|d| d.link.faults().lose_control = true)
                .ok_or_else(|| "no link is open".to_owned());
        }
        if name == "sampling" {
            // A share of the sampling budget, for the tenth-of-the-slice
            // test (spec/faults.md).
            return super::set_scale(arg);
        }
        let mut armed = ARMED.lock().map_err(|_| "the fault table is poisoned")?;
        match name {
            "raise" => armed.raise = true,
            "panic" => armed.panic = true,
            "stall" if (1..=STALL_MAX_MS).contains(&n) => armed.stall_ms = n,
            "stall" => return Err(format!("stall takes 1 to {STALL_MAX_MS} ms")),
            "flood" if (1..=FLOOD_MAX).contains(&n) => armed.flood = n,
            "flood" => return Err(format!("flood takes 1 to {FLOOD_MAX} records")),
            _ => return Err(format!("no fault named {name}")),
        }
        Ok(())
    }

    /// Take the armed `raise`.
    pub fn take_raise() -> bool {
        ARMED
            .lock()
            .map(|mut a| std::mem::take(&mut a.raise))
            .unwrap_or(false)
    }

    /// Take the faults the frame runs.
    pub(super) fn take() -> Armed {
        ARMED
            .lock()
            .map(|mut a| {
                let taken = Armed { raise: false, ..*a };
                a.panic = false;
                a.stall_ms = 0;
                a.flood = 0;
                taken
            })
            .unwrap_or_default()
    }
}

/// Run the faults armed for this frame. Each is logged first, so the log
/// says what was done on purpose.
#[cfg(feature = "faults")]
fn injected(d: &mut Driver) {
    let armed = faults::take();
    if armed.flood > 0 {
        record(
            d,
            Level::Warn,
            "injected fault: flood",
            &[("records", Value::Int(armed.flood as i64))],
        );
        let filler = vec![b'x'; 1000];
        for _ in 0..armed.flood {
            enqueue_log(&mut d.link, 4, "agent", "flood", Lv::String(filler.clone()));
        }
    }
    if armed.stall_ms > 0 {
        record(
            d,
            Level::Warn,
            "injected fault: stall",
            &[("ms", Value::Int(armed.stall_ms as i64))],
        );
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(armed.stall_ms));
        // The message carries the time, so a sidecar that reads only the
        // message sees how long the frame stalled.
        let ms = started.elapsed().as_millis();
        record(
            d,
            Level::Warn,
            &format!("injected fault: stall ended after {ms} ms"),
            &[],
        );
    }
    if armed.panic {
        record(d, Level::Warn, "injected fault: panic", &[]);
        panic!("injected panic");
    }
}

/// One frame's first part: DCS real time, model time, and whether DCS is
/// paused. Returns what the hook does next, whether the hook calls the
/// shim's pump (a sampling frame, or a `server` command waits), and whether
/// a command with a `hook` handler waits: the hook then runs its commands
/// after the pump, and calls `finish` after them. A frame that needs neither
/// ends here (spec/sampling.md, "The frame").
///
/// `read`, when the caller's state has a position getter by object ID, reads
/// one unit's position: `Ok(None)` when the getter gave no position, `Err`
/// when it raised. The frame then sweeps the positions it owes with it,
/// before it decides whether to pump (spec/sampling.md, "The frame").
pub fn frame(
    real: f64,
    model: f64,
    paused: bool,
    shim: bool,
    mut read: Option<&mut ReadPosition<'_>>,
) -> (Frame, bool, bool) {
    with(|d| {
        if d.open {
            // The hook did not call finish for the last frame.
            end_frame(d);
        }
        d.shim = shim;
        let code = frame_with(d, real, model, paused);
        let sampling = d.open
            && d.link.reading_commands()
            && [lifecycle::RUNNING as u32, lifecycle::PAUSED as u32]
                .contains(&d.link.stamps().lifecycle);
        d.sampler.set_direct(read.is_some());
        d.sampler.model_time(model, paused);
        // One clock read serves the slice and the sweep's start, and one
        // the sweep's end and the gate.
        let mut now = Instant::now();
        if !d.link.attached() {
            d.sampler.detached();
        }
        d.sampler.slice(&d.link, now);
        if sampling && let Some(read) = read.as_mut() {
            now = d.sampler.sweep(&mut d.link, now, read, Instant::now);
        }
        let sample = d.open && d.sampler.gate(&d.link, now) && sampling;
        // A command with a `server` handler at the ring's head asks for a
        // pump of its own, never on the system-call frame (spec/commands.md,
        // "Handlers").
        // An extension with work asks for a pump of its own on every frame
        // of a running mission, which is commanded work.
        let extensions = d.ext_active
            && [lifecycle::RUNNING as u32, lifecycle::PAUSED as u32]
                .contains(&d.link.stamps().lifecycle);
        let commands = d.open && (d.waiting || extensions) && d.shim && !d.syscall;
        // A command with a `hook` handler at the head: the hook runs it, after
        // the pump if there is one, and ends the frame (spec/commands.md,
        // "Handlers"). Never on the system-call frame.
        let hook = d.open && d.hook_waiting && !d.syscall;
        let pump = sample || commands;
        d.forced = (pump && !sample) || (hook && !pump);
        d.pumped = false;
        d.handler_time = Duration::ZERO;
        if !pump && !hook && d.open {
            // Nothing comes between this and `finish` on a frame that does
            // not sample, so the frame ends here and the hook makes one
            // call, not two.
            end_frame(d);
        }
        d.pump_started = Instant::now();
        (code, pump, hook)
    })
    .unwrap_or((Frame::NoLink, false, false))
}

/// The frame's last part: account its time, publish its batch and store the
/// status block.
pub fn finish() {
    with(|d| {
        if d.open {
            end_frame(d);
        }
    });
}

fn end_frame(d: &mut Driver) {
    let forced = d.forced;
    settle(d, true);
    if forced {
        // A pump for commands only is all commanded work, the cross-state
        // call included, and its time stays out of the sampling debt
        // (spec/commands.md, "Handlers"). The handlers' own spans are out
        // already.
        d.sampler
            .excuse_commanded(d.pump_started.elapsed().saturating_sub(d.handler_time));
    }
    d.open = false;
    if let Some(r) = d.sampler.end_frame(&mut d.link, Instant::now()) {
        report(d, &r);
    }
    d.link.end();
}

/// The `sampling` LOG (spec/sampling.md, "Reporting").
fn report(d: &mut Driver, r: &Report) {
    let tail = d.link.take_frame_tail();
    let gone = d.sampler.gone_counts();
    let revivals = d.sampler.revivals();
    let marks_cleared = d.sampler.marks_cleared();
    let pending = d.sampler.pending_len();
    let pending_counts = d.sampler.pending_counts();
    let enum_end_abandoned = d.sampler.enum_end_abandoned();
    record(
        d,
        Level::Info,
        "sampling",
        &[
            ("share", Value::Num(r.share)),
            ("syscall_per_s", Value::Num(r.syscall_per_s)),
            ("debt_us", Value::Num(r.debt_us)),
            ("credit_us", Value::Num(r.credit_us)),
            ("pumps", Value::Int(r.pumps as i64)),
            ("empty_pumps", Value::Int(r.empty_pumps as i64)),
            ("frames", Value::Int(r.frames as i64)),
            ("frames_over_250", Value::Int(tail.over as i64)),
            ("syscall_over_250", Value::Int(tail.syscall_over as i64)),
            ("frame_max_us", Value::Num(tail.max_us)),
            ("gap_max_s", Value::Num(tail.gap_max_s)),
            ("slice_us", Value::Num(r.slice_us)),
            ("window_s", Value::Num(r.window_s)),
            ("events_us", Value::Num(r.events_us)),
            ("rows", Value::Int(r.rows as i64)),
            ("births", Value::Int(r.births as i64)),
            ("position_s", Value::Num(r.periods[0])),
            ("vitals_s", Value::Num(r.periods[1])),
            ("ammo_s", Value::Num(r.periods[2])),
            ("warehouse_s", Value::Num(r.periods[3])),
            ("enumeration_s", Value::Num(r.periods[4])),
            ("forgiven_us", Value::Num(r.forgiven_us)),
            ("scale", Value::Num(r.scale)),
            ("overrun_us", Value::Num(r.overrun_us)),
            ("clock_interval_us", Value::Num(r.clock_interval_us)),
            ("starved", Value::Int(r.starved as i64)),
            ("lone_rows", Value::Int(r.lone_rows as i64)),
            (
                "maintenance_frames",
                Value::Int(r.maintenance_frames as i64),
            ),
            (
                "maintenance_overrun_us",
                Value::Num(r.maintenance_overrun_us),
            ),
            ("check_pumps", Value::Int(r.check_pumps as i64)),
            ("check_overrun_us", Value::Num(r.check_overrun_us)),
            ("row_frames", Value::Int(r.row_frames as i64)),
            ("overrun_frames", Value::Int(r.overrun_frames as i64)),
            ("overrun_sum_us", Value::Num(r.overrun_sum_us)),
            ("cost_births", Value::Num(r.costs[0])),
            ("cost_enumeration", Value::Num(r.costs[1])),
            ("cost_subscribed", Value::Num(r.costs[2])),
            ("cost_positions", Value::Num(r.costs[3])),
            ("cost_stopped", Value::Num(r.costs[4])),
            ("cost_vitals", Value::Num(r.costs[5])),
            ("cost_ammo", Value::Num(r.costs[6])),
            ("cost_warehouses", Value::Num(r.costs[7])),
            ("birth_step_p50_us", Value::Num(r.steps[0].0)),
            ("birth_step_p99_us", Value::Num(r.steps[0].1)),
            ("birth_step_max_us", Value::Num(r.steps[0].2)),
            ("enum_step_p50_us", Value::Num(r.steps[1].0)),
            ("enum_step_p99_us", Value::Num(r.steps[1].1)),
            ("enum_step_max_us", Value::Num(r.steps[1].2)),
            ("xcall_us", Value::Num(r.xcall_us)),
            ("xcall_seen_us", Value::Num(r.xcall_seen_us)),
            ("close_us", Value::Num(r.close_us)),
            ("batch_us", Value::Num(r.batch_us)),
            ("us_fixed", Value::Num(r.spend.fixed_us)),
            ("us_xcall", Value::Num(r.spend.xcall_us)),
            ("us_pump", Value::Num(r.spend.pump_us)),
            ("pump_work_us", Value::Num(r.spend.pump_work_us)),
            ("us_syscall", Value::Num(r.spend.syscall_us)),
            ("us_births", Value::Num(r.spend.tier_us[0])),
            ("us_enumeration", Value::Num(r.spend.tier_us[1])),
            ("us_subscribed", Value::Num(r.spend.tier_us[2])),
            ("us_positions", Value::Num(r.spend.tier_us[3])),
            ("us_stopped", Value::Num(r.spend.tier_us[4])),
            ("us_vitals", Value::Num(r.spend.tier_us[5])),
            ("us_ammo", Value::Num(r.spend.tier_us[6])),
            ("us_warehouses", Value::Num(r.spend.tier_us[7])),
            ("rows_births", Value::Int(r.spend.tier_rows[0] as i64)),
            ("rows_enumeration", Value::Int(r.spend.tier_rows[1] as i64)),
            ("rows_subscribed", Value::Int(r.spend.tier_rows[2] as i64)),
            ("rows_positions", Value::Int(r.spend.tier_rows[3] as i64)),
            ("rows_stopped", Value::Int(r.spend.tier_rows[4] as i64)),
            ("rows_vitals", Value::Int(r.spend.tier_rows[5] as i64)),
            ("due_vitals", Value::Num(r.vitals_due)),
            ("swept_vitals", Value::Int(r.vitals_swept as i64)),
            ("rows_ammo", Value::Int(r.spend.tier_rows[6] as i64)),
            ("rows_warehouses", Value::Int(r.spend.tier_rows[7] as i64)),
            ("idle", Value::Int(r.idle as i64)),
            ("marked_life", Value::Int(r.marked.0 as i64)),
            ("marked_ammo", Value::Int(r.marked.1 as i64)),
            ("sweeps", Value::Int(r.sweeps as i64)),
            ("life_sweep_s", Value::Num(r.slow_periods[0])),
            ("ammo_sweep_s", Value::Num(r.slow_periods[1])),
            ("sweep_row_us", Value::Num(r.sweep_row_us)),
            ("sweep_batch_us", Value::Num(r.sweep_batch_us)),
            ("sweep_cut", Value::Int(r.sweep_cut as i64)),
            ("sweep_overruns", Value::Int(r.sweep_overruns as i64)),
            ("sweep_overrun_us", Value::Num(r.sweep_overrun_us)),
            ("sweep_forced", Value::Int(r.sweep_forced as i64)),
            ("sweep_wait_subscribed", Value::Int(r.sweep_waits[0] as i64)),
            ("sweep_wait_batch", Value::Int(r.sweep_waits[1] as i64)),
            ("sweep_wait_debt", Value::Int(r.sweep_waits[2] as i64)),
            ("sweep_wait_fund", Value::Int(r.sweep_waits[3] as i64)),
            ("sweep_wait_slice", Value::Int(r.sweep_waits[4] as i64)),
            ("maintenance_us", Value::Num(r.maintenance_us)),
            (
                "maintenance_forgiven_us",
                Value::Num(r.maintenance_forgiven_us),
            ),
            ("gone_dead", Value::Int(gone[0] as i64)),
            ("gone_removed", Value::Int(gone[1] as i64)),
            ("gone_missing", Value::Int(gone[2] as i64)),
            ("classifier_anomalies", Value::Int(gone[3] as i64)),
            ("classifier_faults", Value::Int(gone[4] as i64)),
            ("revivals", Value::Int(revivals as i64)),
            ("marks_cleared", Value::Int(marks_cleared as i64)),
            ("pending_destroy", Value::Int(pending as i64)),
            ("pending_deaths", Value::Int(pending_counts[0] as i64)),
            ("pending_overflow", Value::Int(pending_counts[1] as i64)),
            ("pending_expired", Value::Int(pending_counts[2] as i64)),
            ("enum_end_abandoned", Value::Int(enum_end_abandoned as i64)),
        ],
    );
    if r.starved > 0 {
        // overhead_share is below the floor the fixed work sets
        // (spec/config.md).
        record(
            d,
            Level::Warn,
            "fixed work reached the overhead allowance",
            &[("frames", Value::Int(r.starved as i64))],
        );
    }
}

fn frame_with(d: &mut Driver, real: f64, model: f64, paused: bool) -> Frame {
    if d.link.disabled() {
        return Frame::Disabled;
    }
    let now = Instant::now();
    let syscall = d.link.begin(now, model, real);
    d.syscall = syscall;
    state::set_frame(d.link.stamps().frame);
    #[cfg(feature = "faults")]
    injected(d);
    let state_now = d.link.stamps().lifecycle;
    let want = if paused {
        lifecycle::PAUSED
    } else {
        lifecycle::RUNNING
    } as u32;
    // A frame means the simulation runs. Starting and loading end at
    // `onSimulationStart`; a frame before it ends them too. A stop is left
    // as it is until the next load.
    let follows = [
        lifecycle::STARTING as u32,
        lifecycle::LOADING as u32,
        lifecycle::RUNNING as u32,
        lifecycle::PAUSED as u32,
    ];
    if follows.contains(&state_now) && state_now != want {
        let (theater, mission) = (d.theater.clone(), d.mission.clone());
        d.link.lifecycle(want, &theater, &mission);
    }
    let mut out = Frame::Nothing;
    if syscall {
        let disable = d.root.join("disable");
        if disable.exists() {
            // The link sends the reason with its last LIFECYCLE.
            d.link.disable("the kill switch");
            state::log(
                Level::Warn,
                "kill switch found; stopping",
                &[("path", Value::Str(&disable.display().to_string()))],
            );
            notes(d);
            state::stop("the kill switch");
            state::flush_log();
            return Frame::Disabled;
        }
        let reload = d.root.join("reload");
        if reload.exists() {
            // A request that cannot be removed would reload every second.
            match std::fs::remove_file(&reload) {
                Ok(()) => out = Frame::Reload,
                Err(e) if !d.reload_failed => {
                    d.reload_failed = true;
                    let path = reload.display().to_string();
                    record(
                        d,
                        Level::Error,
                        "a shim reload request that cannot be removed; not reloading",
                        &[
                            ("path", Value::Str(&path)),
                            ("error", Value::Str(&e.to_string())),
                        ],
                    );
                }
                Err(_) => {}
            }
        }
    }
    commands(d);
    notes(d);
    d.link.flush();
    // The log is written inside the frame's measured time (hist_pump).
    if syscall {
        state::flush_log();
    }
    d.open = true;
    out
}

/// Run commands while the frame's commanded work is under the budget, and
/// always one if one waits (spec/commands.md, "Budget"). Only the time of
/// commands that are not control commands counts. Reading stops at a command
/// whose handler runs in the `server` state: the pump takes it.
fn commands(d: &mut Driver) {
    d.waiting = false;
    d.hook_waiting = false;
    d.ran = false;
    d.spent = Duration::ZERO;
    while !d.ran || d.spent < d.commanded || d.link.unbudgeted() {
        match step(d, Who::Frame) {
            Step::Empty => break,
            Step::Lua => {
                d.waiting = true;
                break;
            }
            Step::Hook => {
                d.hook_waiting = true;
                break;
            }
            Step::Done(counted) => {
                d.ran = true;
                d.spent += counted;
            }
            Step::Handler(_) => unreachable!("only the pump takes a Lua command"),
        }
    }
}

/// The opcodes whose handlers need raw Lua (spec/commands.md).
fn needs_raw_lua(opcode: u16) -> bool {
    matches!(
        u64::from(opcode),
        op::EXTENSION_SET | op::EXTENSION_REMOVE | op::EXTENSION_INVOKE | op::EVAL
    )
}

/// Who asks for the next command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Who {
    /// `frame`, reading commands in Rust.
    Frame,
    /// The shim's pump, in the `server` state.
    Server,
    /// The hook's dispatch, in the hook's state.
    Hook,
    /// `settle` at the end of a frame whose Lua state never asked: a command
    /// waiting for a handler is answered `NOT_READY`.
    Refuse,
}

/// The state a sequenced command's Lua handler runs in, or None when Rust
/// answers it (an opcode the agent has no handler for is `UNKNOWN_OPCODE`).
fn handler_state(opcode: u16) -> Option<HandlerState> {
    if has_server_handler(opcode) {
        Some(HandlerState::Server)
    } else if matches!(
        u64::from(opcode),
        op::DAMAGE
            | op::PICTURE
            | op::PICTURE_STOP
            | op::CHAT
            | op::MISSION_LOAD
            | op::MODERATE
            | op::PLAYERS
            | op::BANS
    ) {
        Some(HandlerState::Hook)
    } else {
        None
    }
}

/// The next command at the ring's head, for the caller `who`. A command
/// whose handler runs in another state stays at the head: `command` returns
/// nothing for it, so ring order holds. For `frame`, a sequenced command
/// with a `server` handler stays there (`Lua`), as does one with a `hook`
/// handler when a mission runs (`Hook`); either waits while the lifecycle
/// state is `LOADING`, and is otherwise answered `NOT_READY`, a `server`
/// command when no shim is loaded. For the state that owns the handler it
/// is taken and admitted; an admitted one is left outstanding for its
/// handler (`Handler`), with its arguments in `staged`.
fn step(d: &mut Driver, who: Who) -> Step {
    let mut payload = std::mem::take(&mut d.payload);
    let Some(opcode) = d.link.peek_command(&mut payload) else {
        d.payload = payload;
        return Step::Empty;
    };
    let header = CommandHeader::read(&mut Reader::new(&payload)).ok();
    let state = header
        .as_ref()
        .is_some_and(|h| h.seq != 0)
        .then(|| handler_state(opcode))
        .flatten();
    let lifecycle_now = d.link.stamps().lifecycle;
    let running = [lifecycle::RUNNING as u32, lifecycle::PAUSED as u32].contains(&lifecycle_now);
    let wait = match (who, state) {
        (Who::Server, Some(HandlerState::Hook)) | (Who::Hook, Some(HandlerState::Server)) => {
            Some(Step::Empty)
        }
        (Who::Frame, Some(HandlerState::Server)) if d.shim => Some(Step::Lua),
        (Who::Frame, Some(HandlerState::Hook)) if running => Some(Step::Hook),
        (Who::Frame | Who::Refuse, Some(_)) if lifecycle_now == lifecycle::LOADING as u32 => {
            Some(Step::Empty)
        }
        _ => None,
    };
    if let Some(wait) = wait {
        d.payload = payload;
        return wait;
    }
    let Some(opcode) = d.link.take_command(&mut payload) else {
        d.payload = payload;
        return Step::Empty;
    };
    let out = if state.is_some() {
        lua_command(d, opcode, &payload, matches!(who, Who::Server | Who::Hook))
    } else {
        Step::Done(run_command(d, opcode, &payload))
    };
    d.payload = payload;
    out
}

/// Whether the opcode's handler runs in the `server` state.
fn has_server_handler(opcode: u16) -> bool {
    opcode_row(opcode.into()).and_then(|r| r.state) == Some(HandlerState::Server)
}

/// Whether a sequenced command with a Lua handler is at the ring's head,
/// looked at without taking it.
fn head_is_lua(d: &mut Driver) -> bool {
    let mut payload = std::mem::take(&mut d.payload);
    let lua = d.link.peek_command(&mut payload).is_some_and(|opcode| {
        CommandHeader::read(&mut Reader::new(&payload)).is_ok_and(|h| h.seq != 0)
            && handler_state(opcode).is_some()
    });
    d.payload = payload;
    lua
}

/// A taken command with a `server` handler: admitted, then answered
/// `NOT_READY` outside the pump, or left outstanding for the shim.
fn lua_command(d: &mut Driver, opcode: u16, payload: &[u8], in_pump: bool) -> Step {
    let started = Instant::now();
    let mut rd = Reader::new(payload);
    let h = CommandHeader::read(&mut rd).expect("read at the peek");
    state::crumb_with(crumbs::COMMAND + u64::from(opcode), h.seq);
    let body = &payload[rd.position()..];
    let name = matches!(u64::from(opcode), op::EXTENSION_SET | op::EXTENSION_INVOKE)
        .then(|| Reader::new(body).str().ok().map(str::as_bytes))
        .flatten();
    let mut message = "";
    let code = match d.link.admit(&h, opcode, name) {
        Admit::Answer(code) => code,
        Admit::Control => unreachable!("a sequenced command"),
        Admit::Run if needs_raw_lua(opcode) && !d.link.raw_lua() => result::RAW_LUA_OFF,
        Admit::Run if !in_pump => result::NOT_READY,
        Admit::Run
            if u64::from(opcode) == op::EXTENSION_INVOKE
                && name.is_some_and(|n| !d.link.has_extension(n)) =>
        {
            // Nothing is registered under the name in this campaign.
            message = "no extension of that name";
            result::LUA_ERROR
        }
        Admit::Run => match arguments_with(opcode, &h, body, &|scope, target| {
            bound_target(&d.sampler, scope, target)
        }) {
            Some(args) => {
                d.staged = args;
                d.outstanding = Some(Outstanding {
                    opcode,
                    header: h,
                    started,
                });
                return Step::Handler(opcode);
            }
            None => result::BAD_PAYLOAD,
        },
    };
    d.link.ran(code);
    Step::Done(answer(d, opcode, &h, code, started, message, 0, Lv::Nil))
}

/// The platform name of an entity (spec/world.md, "Names"):
/// `np<marker><campaign tag><entity base 62>`.
fn platform_name(kind: Kind, campaign: u64, entity: u64) -> String {
    names::name(kind, names::tag_of(campaign), entity)
}

/// The fields of one handler's arguments table: encoded `lv` pairs, built by
/// hand. A value that is itself an `lv` payload is copied in as bytes:
/// `Writer::lv` counts depth from the wrapper and would cut an argument that
/// is 16 deep from its own root, which the codec admits.
#[derive(Default)]
struct Fields {
    n: u32,
    pairs: Vec<u8>,
}

impl Fields {
    fn key(&mut self, key: &str) -> Writer {
        self.n += 1;
        let mut w = Writer::new();
        w.lv(&Lv::String(key.as_bytes().to_vec()));
        w
    }

    fn push(&mut self, key: &str, value: &Lv) {
        let mut w = self.key(key);
        w.lv(value);
        self.pairs.extend(w.finish());
    }

    fn num(&mut self, key: &str, v: f64) {
        self.push(key, &Lv::Number(v));
    }

    fn text(&mut self, key: &str, v: &str) {
        self.push(key, &Lv::String(v.as_bytes().to_vec()));
    }

    /// An `lv` the payload holds, copied as it is.
    fn raw(&mut self, key: &str, bytes: &[u8]) {
        let w = self.key(key);
        self.pairs.extend(w.finish());
        self.pairs.extend_from_slice(bytes);
    }

    fn finish(self) -> Vec<u8> {
        let mut out = vec![nanoprobe_transport::layout::lv_tag::TABLE as u8];
        out.extend_from_slice(&self.n.to_le_bytes());
        out.extend(self.pairs);
        out
    }
}

/// The next `lv` of the payload, as the bytes it occupies.
fn lv_bytes<'a>(rd: &mut Reader<'a>, body: &'a [u8]) -> Option<&'a [u8]> {
    let from = rd.position();
    rd.lv().ok()?;
    Some(&body[from..rd.position()])
}

/// A command's arguments for its Lua handler, as an `lv` table, encoded:
/// the fields of its layout by name (spec/commands.md, "Handler arguments"),
/// and the platform names built from the header. `None` when the payload does
/// not decode.
#[cfg(test)]
fn arguments(opcode: u16, h: &CommandHeader, body: &[u8]) -> Option<Vec<u8>> {
    arguments_with(opcode, h, body, &|_, _| None)
}

/// What the agent's binding holds for the entity a `GROUP` or `UNIT` scope
/// names: the DCS group ID, or the slot of the unit's row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bound {
    Group(u64),
    Row(u32),
}

/// The binding lookup of a command's arguments: by scope code and target.
type Lookup<'a> = &'a dyn Fn(u32, u64) -> Option<Bound>;

/// What the sampler's rows hold for a `GROUP` (3) or `UNIT` (4) target.
fn bound_target(sampler: &Sampler, scope: u32, target: u64) -> Option<Bound> {
    match scope {
        3 => sampler.group_id_of_entity(target).map(Bound::Group),
        4 => sampler.slot_of_entity(target).map(Bound::Row),
        _ => None,
    }
}

/// Adds what a bound target is to the arguments: `group_id` for a group,
/// `row` for a unit. `text` writes the number as text.
fn push_bound(f: &mut Fields, bound: Option<Bound>, text: bool) {
    let (key, n) = match bound {
        Some(Bound::Group(id)) => ("group_id", id),
        Some(Bound::Row(slot)) => ("row", u64::from(slot)),
        None => return,
    };
    if text {
        f.text(key, &n.to_string());
    } else {
        f.num(key, n as f64);
    }
}

fn arguments_with(
    opcode: u16,
    h: &CommandHeader,
    body: &[u8],
    held: Lookup<'_>,
) -> Option<Vec<u8>> {
    // A name is built only for an entity in range (spec/world.md, "Names").
    let named = matches!(
        u64::from(opcode),
        op::GROUP_SPAWN
            | op::GROUP_ORDER
            | op::GROUP_DESTROY
            | op::GROUP_CAPTURE_DESTROY
            | op::GROUP_OPTION
            | op::STATIC_SPAWN
            | op::STATIC_DESTROY
            | op::DAMAGE
            | op::CAPTURE
    );
    if named && !(1..=names::MAX_ENTITY).contains(&h.entity) {
        return None;
    }
    let mut rd = Reader::new(body);
    let mut f = Fields::default();
    let group = || platform_name(Kind::Group, h.campaign, h.entity);
    match u64::from(opcode) {
        op::EXTENSION_SET | op::EXTENSION_INVOKE => {
            let key = if u64::from(opcode) == op::EXTENSION_SET {
                "source"
            } else {
                "args"
            };
            f.text("name", rd.str().ok()?);
            f.raw(key, lv_bytes(&mut rd, body)?);
        }
        op::GROUP_SPAWN => {
            f.num("country", f64::from(rd.u32().ok()?));
            f.num("category", f64::from(rd.u32().ok()?));
            rd.str().ok()?;
            f.text("name", &group());
            f.raw("group", lv_bytes(&mut rd, body)?);
        }
        op::GROUP_ORDER => {
            f.text("name", &group());
            f.raw("task", lv_bytes(&mut rd, body)?);
        }
        op::GROUP_DESTROY | op::GROUP_CAPTURE_DESTROY => f.text("name", &group()),
        op::GROUP_OPTION => {
            f.num("option", f64::from(rd.u32().ok()?));
            rd.skip(4).ok()?;
            f.text("name", &group());
            f.raw("value", lv_bytes(&mut rd, body)?);
        }
        op::STATIC_SPAWN => {
            f.num("country", f64::from(rd.u32().ok()?));
            rd.skip(4).ok()?;
            rd.str().ok()?;
            f.text("name", &platform_name(Kind::Static, h.campaign, h.entity));
            f.raw("static", lv_bytes(&mut rd, body)?);
        }
        op::STATIC_DESTROY => f.text("name", &platform_name(Kind::Static, h.campaign, h.entity)),
        op::AIRBASE_OWNER => {
            f.num("coalition", f64::from(rd.u32().ok()?));
            rd.skip(4).ok()?;
            f.text("airbase", rd.str().ok()?);
        }
        op::WAREHOUSE_SET | op::WAREHOUSE_ADJUST => {
            let key = if u64::from(opcode) == op::WAREHOUSE_SET {
                "stock"
            } else {
                "delta"
            };
            f.text("airbase", rd.str().ok()?);
            f.raw(key, lv_bytes(&mut rd, body)?);
        }
        op::DAMAGE => {
            // `+ 0.0` turns -0.0 into 0.0 so the text reads "0.0000".
            let life = rd.f64().ok()? + 0.0;
            if !life.is_finite() || !(0.0..=100.0).contains(&life) {
                return None;
            }
            f.text("name", &platform_name(Kind::Unit, h.campaign, h.entity));
            // Text Rust formats, so no Lua locale reads it (spec/commands.md,
            // "Layout: DAMAGE").
            f.text("life_percent", &format!("{life:.4}"));
        }
        op::SOUND => {
            let scope = rd.u32().ok()?;
            rd.skip(4).ok()?;
            let target = rd.u64().ok()?;
            let file = rd.str().ok()?;
            let name = scope_name(scope, target, h)?;
            f.num("scope", f64::from(scope));
            f.num("target", target as f64);
            if let Some(name) = name {
                match held(scope, target) {
                    bound @ Some(_) => push_bound(&mut f, bound, false),
                    None => f.text("name", &name),
                }
            }
            f.text("file", file);
        }
        op::PICTURE => {
            let scope = rd.u32().ok()?;
            let clear_view = rd.u32().ok()?;
            let target = rd.u64().ok()?;
            let seconds = rd.f64().ok()? + 0.0;
            let size = rd.f64().ok()? + 0.0;
            let horizontal = rd.u32().ok()?;
            let vertical = rd.u32().ok()?;
            let size_units = rd.u32().ok()?;
            rd.skip(4).ok()?;
            let file = rd.str().ok()?;
            let name = scope_name(scope, target, h)?;
            if !(seconds.is_finite() && (0.0..=PICTURE_SECONDS_MAX).contains(&seconds))
                || !(size.is_finite() && (PICTURE_SIZE_MIN..=PICTURE_SIZE_MAX).contains(&size))
                || !nanoprobe_transport::picture_file_ok(file)
            {
                return None;
            }
            // Text Rust formats, so no Lua locale reads it, and a file name
            // the hook can put in source (spec/commands.md, "Handler
            // arguments").
            f.text("scope", &scope.to_string());
            f.text("target", &target.to_string());
            if let Some(name) = name {
                match held(scope, target) {
                    bound @ Some(_) => push_bound(&mut f, bound, true),
                    None => f.text("name", &name),
                }
            }
            f.text("file", file);
            f.text("seconds", &format!("{seconds:.4}"));
            f.text("clear_view", if clear_view != 0 { "true" } else { "false" });
            f.text("size", &format!("{size:.4}"));
            f.text("horizontal", &horizontal.to_string());
            f.text("vertical", &vertical.to_string());
            f.text("size_units", &size_units.to_string());
        }
        op::CAPTURE => {
            f.text("name", &group());
            f.text(
                "static_name",
                &platform_name(Kind::Static, h.campaign, h.entity),
            );
        }
        op::EXTENSION_REMOVE => f.text("name", rd.str().ok()?),
        op::MARK_REMOVE | op::DRAWING_REMOVE | op::MENU_REMOVE => {
            f.text("entity", &h.entity.to_string());
        }
        op::MARK_SET => {
            let scope = rd.u32().ok()?;
            let read_only = rd.u32().ok()?;
            let target = rd.u64().ok()?;
            let (x, y, z) = (rd.f64().ok()?, rd.f64().ok()?, rd.f64().ok()?);
            let text = rd.str().ok()?;
            f.text("entity", &h.entity.to_string());
            scoped(&mut f, scope, target, h, &[1, 2, 3], held)?;
            f.push("read_only", &Lv::Bool(read_only != 0));
            f.num("x", x);
            f.num("y", y);
            f.num("z", z);
            f.text("text", text);
        }
        op::DRAWING_SET => {
            let scope = rd.u32().ok()?;
            let shape = rd.u32().ok()?;
            let target = rd.u64().ok()?;
            f.text("entity", &h.entity.to_string());
            scoped(&mut f, scope, target, h, &[1, 2], held)?;
            f.num("shape", f64::from(shape));
            f.raw("args", lv_bytes(&mut rd, body)?);
        }
        op::MENU_SET => {
            let parent = rd.u64().ok()?;
            let target = rd.u64().ok()?;
            let scope = rd.u32().ok()?;
            let submenu = rd.u32().ok()?;
            let text = rd.str().ok()?;
            f.text("entity", &h.entity.to_string());
            f.text("parent", &parent.to_string());
            scoped(&mut f, scope, target, h, &[1, 2, 3], held)?;
            f.push("submenu", &Lv::Bool(submenu != 0));
            f.text("text", text);
        }
        op::MESSAGE => {
            let scope = rd.u32().ok()?;
            let clear = rd.u32().ok()?;
            let target = rd.u64().ok()?;
            let seconds = rd.f64().ok()? + 0.0;
            let text = rd.str().ok()?;
            if !(seconds.is_finite() && (0.0..=MESSAGE_SECONDS_MAX).contains(&seconds)) {
                return None;
            }
            scoped(&mut f, scope, target, h, &[1, 2, 3, 4, 5], held)?;
            f.push("clear", &Lv::Bool(clear != 0));
            f.num("seconds", seconds);
            f.text("text", text);
        }
        op::CHAT => {
            let player = rd.u32().ok()?;
            rd.skip(4).ok()?;
            let text = rd.str().ok()?;
            if text.is_empty() || text.len() > CHAT_BYTES_MAX {
                return None;
            }
            f.num("player", f64::from(player));
            f.text("text", text);
        }
        op::MISSION_LOAD => {
            let path = rd.str().ok()?;
            if path.is_empty() || path.len() > PATH_BYTES_MAX {
                return None;
            }
            f.text("path", path);
        }
        op::EVAL => f.raw("source", lv_bytes(&mut rd, body)?),
        op::MODERATE => {
            let action = rd.u32().ok()?;
            let player = rd.u32().ok()?;
            let side = rd.u32().ok()?;
            rd.skip(4).ok()?;
            let seconds = rd.f64().ok()? + 0.0;
            let ucid = rd.str().ok()?;
            let slot = rd.str().ok()?;
            let reason = rd.str().ok()?;
            let known = [
                moderation::KICK,
                moderation::BAN,
                moderation::UNBAN,
                moderation::SLOT,
            ];
            // Player 1 is the server itself.
            if !known.contains(&u64::from(action))
                || ucid.is_empty()
                || !(seconds.is_finite() && seconds >= 0.0)
                || player == SERVER_PLAYER
            {
                return None;
            }
            f.num("action", f64::from(action));
            f.num("player", f64::from(player));
            f.num("side", f64::from(side));
            f.num("seconds", seconds);
            f.text("ucid", ucid);
            f.text("slot", slot);
            f.text("reason", reason);
        }
        op::CLEAR_DEBRIS => {
            let shape = u64::from(rd.u32().ok()?);
            rd.skip(4).ok()?;
            let mut v = [0.0f64; 7];
            for x in &mut v {
                *x = rd.f64().ok()? + 0.0;
            }
            let [x, y, z, x2, y2, z2, radius] = v;
            if !v
                .iter()
                .all(|n| n.is_finite() && n.abs() <= DEBRIS_REACH_MAX)
            {
                return None;
            }
            let sound = match shape {
                volume::SPHERE => radius > 0.0,
                volume::BOX => x <= x2 && y <= y2 && z <= z2,
                _ => false,
            };
            if !sound {
                return None;
            }
            f.num("volume", shape as f64);
            for (key, n) in [
                ("x", x),
                ("y", y),
                ("z", z),
                ("x2", x2),
                ("y2", y2),
                ("z2", z2),
                ("radius", radius),
            ] {
                f.num(key, n);
            }
        }
        _ => {}
    }
    Some(f.finish())
}

/// The farthest from 0 a `CLEAR_DEBRIS` coordinate or radius may be, in
/// meters: 10,000 km.
const DEBRIS_REACH_MAX: f64 = 10_000_000.0;

/// The longest a `PICTURE` stays, in seconds.
const PICTURE_SECONDS_MAX: f64 = 86_400.0;
/// The smallest `PICTURE` size: its four-decimal text is never 0.
const PICTURE_SIZE_MIN: f64 = 0.0001;
/// The largest `PICTURE` size.
const PICTURE_SIZE_MAX: f64 = 10_000.0;
/// The longest a `MESSAGE` stays, in seconds.
const MESSAGE_SECONDS_MAX: f64 = 86_400.0;
/// The DCS connection ID of the server itself.
const SERVER_PLAYER: u32 = 1;
/// The longest `CHAT` text, in bytes.
const CHAT_BYTES_MAX: usize = 1024;
/// The longest `MISSION_LOAD` path, in bytes.
const PATH_BYTES_MAX: usize = 1024;
/// The largest target a Lua number holds exactly.
const EXACT_TARGET_MAX: u64 = 1 << 53;

/// The check of a `SOUND` or `PICTURE` scope and target: `None` when the
/// scope is not 1 to 5 or the target is out of range for it. Otherwise the
/// platform name of a `GROUP` or `UNIT` target, empty with campaign 0, or
/// `None` inside for the other scopes.
fn scope_name(scope: u32, target: u64, h: &CommandHeader) -> Option<Option<String>> {
    match scope {
        1 => Some(None),
        2 | 5 => (target <= u64::from(u32::MAX)).then_some(None),
        3 | 4 if target < EXACT_TARGET_MAX => {
            let kind = if scope == 3 { Kind::Group } else { Kind::Unit };
            Some(Some(if h.campaign == 0 {
                String::new()
            } else {
                platform_name(kind, h.campaign, target)
            }))
        }
        _ => None,
    }
}

/// Adds a scoped command's `scope`, `name` and `target` to its arguments:
/// `name` is the platform name of a `GROUP` or `UNIT` target and empty
/// otherwise, `target` is a number for `COALITION`, `GROUP` and `COUNTRY` and
/// absent otherwise. A `GROUP` or `UNIT` target the agent's binding holds adds
/// `group_id` or `row` (see [`Bound`]) and its `name` is empty. `None` when the scope is not one of `allowed` or the target is
/// out of range for it (spec/commands.md, "Handler arguments").
fn scoped(
    f: &mut Fields,
    scope: u32,
    target: u64,
    h: &CommandHeader,
    allowed: &[u32],
    held: Lookup<'_>,
) -> Option<()> {
    if !allowed.contains(&scope) {
        return None;
    }
    let name = scope_name(scope, target, h)?.unwrap_or_default();
    let bound = held(scope, target);
    f.num("scope", f64::from(scope));
    if matches!(scope, 2 | 3 | 5) {
        f.num("target", target as f64);
    }
    f.text("name", if bound.is_some() { "" } else { &name });
    push_bound(f, bound, false);
    Some(())
}

/// What a spawn's result carries: the DCS ID the handler returned becomes the
/// `RESULT`'s `object`, and the value is nil (spec/commands.md, "Handler
/// arguments"). Any other result passes through.
fn spawn_result(opcode: u16, code: u64, value: Lv) -> (u64, Lv) {
    let spawn = matches!(u64::from(opcode), op::GROUP_SPAWN | op::STATIC_SPAWN);
    match value {
        Lv::Number(n) if spawn && code == result::APPLIED && n >= 0.0 && n.fract() == 0.0 => {
            (n as u64, Lv::Nil)
        }
        value => (0, value),
    }
}

/// The longest `RESULT` message, in bytes (spec/records.md, "Commands").
const MESSAGE_CAP: usize = 1024;

/// The message cut to [`MESSAGE_CAP`] bytes at a UTF-8 boundary.
fn capped(message: &str) -> &str {
    let mut end = message.len().min(MESSAGE_CAP);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

/// Write a command's `RESULT`, count its time, clear the breadcrumb, and
/// return the time that counts against the budget.
#[allow(clippy::too_many_arguments, reason = "one argument per RESULT field")]
fn answer(
    d: &mut Driver,
    opcode: u16,
    h: &CommandHeader,
    code: u64,
    started: Instant,
    message: &str,
    object: u64,
    value: Lv,
) -> Duration {
    state::crumb(0);
    let message = capped(message);
    let elapsed = started.elapsed();
    let counted = if h.seq == 0 { Duration::ZERO } else { elapsed };
    d.link
        .record_command(elapsed.as_secs_f64() * 1e6, counted > d.commanded);
    let cost = elapsed.as_micros().min(u128::from(u32::MAX)) as u32;
    d.link.answer(opcode, h, code, object, cost, message, value);
    counted
}

/// Answer the outstanding command `LUA_ERROR` "no result"; at the end of a
/// frame whose pump was for commands, and whose shim never called `command`,
/// answer a `server` command at the head `NOT_READY` (spec/commands.md,
/// "Handlers"). A command left because the budget ran out stays in the ring.
fn settle(d: &mut Driver, ending: bool) {
    if let Some(o) = d.outstanding.take() {
        d.link.ran(result::LUA_ERROR);
        let counted = answer(
            d,
            o.opcode,
            &o.header,
            result::LUA_ERROR,
            o.started,
            "no result",
            0,
            Lv::Nil,
        );
        d.spent += counted;
        handled(d, counted);
    }
    if ending && std::mem::take(&mut d.forced) && !d.pumped && head_is_lua(d) {
        // Taken as if no shim were loaded: `NOT_READY`. In the lifecycle
        // state `LOADING` the command waits instead; no frame sees that state
        // today (spec/commands.md, "Handlers").
        if let Step::Done(counted) = step(d, Who::Refuse) {
            d.spent += counted;
        }
    }
}

/// A handler's take-to-done time: commanded work only.
fn handled(d: &mut Driver, counted: Duration) {
    d.handler_time += counted;
    d.sampler.excuse_commanded(counted);
}

/// The shim's pump asks for the next command whose handler it runs:
/// commands handled in Rust are run on the way, in ring order, under the
/// frame's budget. Returns the opcode, with its arguments in `args` as an
/// encoded `lv` table, or None.
pub fn command(args: &mut Vec<u8>, hook: bool) -> Option<u16> {
    with(|d| {
        settle(d, false);
        if d.forced && !d.pumped {
            // The cross-state call of a pump for commands only is commanded
            // work (spec/commands.md, "Handlers").
            d.spent += d.pump_started.elapsed();
        }
        d.pumped = true;
        loop {
            if d.ran && d.spent >= d.commanded && !d.link.unbudgeted() {
                return None;
            }
            let started = Instant::now();
            let step = step(d, if hook { Who::Hook } else { Who::Server });
            if d.begun.is_none() && !matches!(step, Step::Handler(_)) {
                d.pump_commands += started.elapsed();
            }
            match step {
                Step::Empty | Step::Lua | Step::Hook => return None,
                Step::Done(counted) => {
                    d.ran = true;
                    d.spent += counted;
                }
                Step::Handler(opcode) => {
                    d.ran = true;
                    std::mem::swap(args, &mut d.staged);
                    return Some(opcode);
                }
            }
        }
    })
    .flatten()
}

/// Whether the opcode's handler names the objects it destroyed: the spawns of
/// groups and of statics (a replaced group or static), the destroys of a group and of a static, and the
/// capture-and-destroy.
fn reads_destroyed(opcode: u16) -> bool {
    matches!(
        u64::from(opcode),
        op::GROUP_SPAWN
            | op::STATIC_SPAWN
            | op::GROUP_DESTROY
            | op::GROUP_CAPTURE_DESTROY
            | op::STATIC_DESTROY
    )
}

/// `APPLIED`, or `TOO_LARGE`: the command ran and only its value did not fit.
fn is_applied(code: u64) -> bool {
    code == result::APPLIED || code == result::TOO_LARGE
}

/// Whether the outstanding command's handler reports `destroyed`, so the
/// entry reads the argument.
pub fn wants_destroyed() -> bool {
    with(|d| {
        d.outstanding
            .as_ref()
            .is_some_and(|o| reads_destroyed(o.opcode))
    })
    .unwrap_or(false)
}

/// The shim's handler finished the outstanding command with `code`, a
/// message, a value and the object IDs it destroyed. A call with none outstanding is logged and ignored.
/// Returns how many of `destroyed` the agent took: all of them, or 0 when the
/// command did not read them or did not apply.
pub fn command_done(
    code: u64,
    message: &str,
    value: Lv,
    destroyed: &[u64],
    cut_ids: usize,
) -> usize {
    let mut taken = 0;
    with(|d| {
        let Some(o) = d.outstanding.take() else {
            record(
                d,
                Level::Warn,
                "command_done with no command outstanding",
                &[("code", Value::Int(code as i64))],
            );
            return;
        };
        let (code, message, value) = if result::ALL.iter().any(|&(_, c)| c == code) {
            (code, message.to_owned(), value)
        } else {
            (
                result::LUA_ERROR,
                format!("the handler returned the undefined code {code}"),
                Lv::Nil,
            )
        };
        d.link.ran(code);
        if reads_destroyed(o.opcode) && !destroyed.is_empty() && is_applied(code) {
            // Before the RESULT: the rows leave as `DESTROYED`, and every ID
            // enters the pending-destroy set.
            d.sampler.destroyed(&mut d.link, destroyed);
            taken = destroyed.len();
            // IDs past the read limit cannot enter the pending set.
            d.sampler.add_pending_overflow(cut_ids as u64);
        }
        let (object, value) = spawn_result(o.opcode, code, value);
        if code == result::APPLIED && u64::from(o.opcode) == op::GROUP_SPAWN {
            // The new group has no task (spec/commands.md, "Layout: GROUP_SPAWN").
            d.link.clear_order(o.header.entity);
        }
        if code == result::APPLIED
            && matches!(
                u64::from(o.opcode),
                op::GROUP_DESTROY | op::GROUP_CAPTURE_DESTROY | op::STATIC_DESTROY
            )
        {
            // A destroy sets the applied revisions of what it removed to 0.
            d.link.clear_entity(o.header.entity);
        }
        let counted = answer(
            d, o.opcode, &o.header, code, o.started, &message, object, value,
        );
        d.spent += counted;
        handled(d, counted);
        if d.begun.is_none() {
            d.pump_commands += counted;
        }
    });
    taken
}

/// One command. This step has no handler for any opcode but `CAMPAIGN`, so
/// every other command is answered `UNKNOWN_OPCODE` after the rows that need
/// no opcode table (spec/commands.md, "Unknown opcodes"). Returns the time
/// that counts against the budget: none for a control command.
fn run_command(d: &mut Driver, opcode: u16, payload: &[u8]) -> Duration {
    let started = Instant::now();
    let mut rd = Reader::new(payload);
    let Ok(h) = CommandHeader::read(&mut rd) else {
        // A failed consuming check: the sidecar is broken (spec/commands.md,
        // "Layout: command").
        record(
            d,
            Level::Error,
            "a command too short for its header; detaching",
            &[
                ("opcode", Value::Int(i64::from(opcode))),
                ("bytes", Value::Int(payload.len() as i64)),
            ],
        );
        d.link.detach("a command too short for its header");
        return Duration::ZERO;
    };
    state::crumb_with(crumbs::COMMAND + u64::from(opcode), h.seq);
    let body = &payload[rd.position()..];
    let code = match d.link.admit(&h, opcode, None) {
        Admit::Control => control(d, opcode, body),
        Admit::Run => {
            d.link.ran(result::UNKNOWN_OPCODE);
            Some(result::UNKNOWN_OPCODE)
        }
        Admit::Answer(code) => Some(code),
    };
    state::crumb(0);
    let elapsed = started.elapsed();
    let counted = if h.seq == 0 { Duration::ZERO } else { elapsed };
    d.link
        .record_command(elapsed.as_secs_f64() * 1e6, counted > d.commanded);
    if let Some(code) = code {
        let cost = elapsed.as_micros().min(u128::from(u32::MAX)) as u32;
        d.link.answer(opcode, &h, code, 0, cost, "", Lv::Nil);
    }
    counted
}

/// A control command: `CAMPAIGN` and the sampling opcodes (spec/sampling.md,
/// "Commands"). Returns a failure to answer, or None: a control command that
/// worked has no `RESULT`.
fn control(d: &mut Driver, opcode: u16, body: &[u8]) -> Option<u64> {
    match opcode {
        CAMPAIGN => match Reader::new(body).u64() {
            Ok(campaign) => {
                d.link.set_campaign(campaign);
                d.sampler.set_campaign(campaign);
                None
            }
            Err(_) => Some(result::BAD_PAYLOAD),
        },
        ENUMERATE => {
            d.sampler.request_enumeration();
            None
        }
        RATES => match nanoprobe_transport::records::Rates::decode(body) {
            Ok(r) => {
                d.sampler.set_rates(Rates {
                    position_s: r.position_s,
                    vitals_s: r.vitals_s,
                    ammo_s: r.ammo_s,
                    warehouse_s: r.warehouse_s,
                    enumeration_s: r.enumeration_s,
                });
                None
            }
            Err(_) => Some(result::BAD_PAYLOAD),
        },
        SUBSCRIBE => match Subscribe::decode(body) {
            Ok(sub) => {
                d.sampler
                    .subscribe(sub.period_s, sub.attitude == 1, &sub.entity);
                None
            }
            Err(_) => Some(result::BAD_PAYLOAD),
        },
        BIND => match Bind::decode(body) {
            Ok(b) if b.entity.len() == b.object.len() => {
                let pairs: Vec<(u64, u64)> = b
                    .entity
                    .iter()
                    .copied()
                    .zip(b.object.iter().copied())
                    .collect();
                d.sampler.bind(b.kind, &pairs);
                None
            }
            _ => Some(result::BAD_PAYLOAD),
        },
        BURST => match Reader::new(body).u32() {
            Ok(on @ (0 | 1)) => {
                d.link.set_burst(on == 1);
                None
            }
            _ => Some(result::BAD_PAYLOAD),
        },
        _ => Some(result::UNKNOWN_OPCODE),
    }
}

/// A lifecycle change from a callback: `loading` from `onMissionLoadBegin`,
/// `running` or `paused` from `onSimulationStart`, `stopping` from
/// `onSimulationStop`, with DCS real and model time in the callback. Returns
/// the mission generation, or None for an unknown name or no link.
pub fn lifecycle(name: &str, theater: &str, mission: &str, real: f64, model: f64) -> Option<u32> {
    let code = match name {
        "loading" => lifecycle::LOADING,
        "running" => lifecycle::RUNNING,
        "paused" => lifecycle::PAUSED,
        "stopping" => lifecycle::STOPPING,
        _ => return None,
    } as u32;
    with(|d| {
        d.link.set_time(model, real);
        if code == lifecycle::LOADING as u32 {
            // A mission is being loaded and none is loaded yet
            // (spec/records.md, "Layout: LIFECYCLE").
            let generation = d.link.new_generation();
            d.sampler.new_generation(generation);
            d.theater.clear();
            d.mission.clear();
        } else if code == lifecycle::STOPPING as u32 {
            // The stopping mission is still loaded: keep its names.
            if !theater.is_empty() {
                d.theater = theater.into();
            }
            if !mission.is_empty() {
                d.mission = mission.into();
            }
        } else {
            d.theater = theater.into();
            d.mission = mission.into();
        }
        let (theater, mission) = (d.theater.clone(), d.mission.clone());
        d.link.lifecycle(code, &theater, &mission);
        // No frame follows a load's start or a stop until the next mission
        // runs, so these publish now (spec/transport.md, "Producing").
        if code == lifecycle::LOADING as u32 || code == lifecycle::STOPPING as u32 {
            d.link.publish_now();
        }
        notes(d);
        d.link.stamps().mission_gen
    })
}

/// Queue an `EVENT` record from the shim's event handler, and count the
/// time since `started` in `hist_event`. A record over the payload limit
/// goes cut (spec/records.md, "Oversized records"); one still over it is a
/// `LOG` at `error` and a count in the pending `GAP` of events. A death
/// event of a unit or static in the table asks for a check of its existence
/// (spec/records.md, "The object table"). Returns the slot plus one of a row
/// removed: always 0, the check removes it.
pub fn event(event: Event, started: Instant) -> u32 {
    with(|d| {
        let mut freed = 0;
        let field = |code| {
            event
                .fields
                .iter()
                .find(|(c, _)| *c == code)
                .and_then(|(_, v)| match v {
                    Lv::Number(n) => Some(*n),
                    _ => None,
                })
        };
        if d.deaths.contains(&event.id) {
            freed = d
                .sampler
                .death(&mut d.link, field(INITIATOR), field(INITIATOR_CATEGORY));
        }
        // A hit asks for a read of the target's life, and a shot for a read
        // of the shooter's ammunition (spec/sampling.md, "Tiers").
        if d.life_events.contains(&event.id) {
            d.sampler.mark(field(TARGET), true);
        }
        if d.ammo_events.contains(&event.id) {
            d.sampler.mark(field(INITIATOR), false);
        }
        // A hit, a damage assessment or a kill asks for one check of
        // whether the target still exists (spec/records.md, "The object
        // table").
        if d.watch_events.contains(&event.id) {
            d.sampler.watch(field(TARGET));
        }
        // The shim keeps the birth of a unit, a static or a cargo static for
        // the next pump (`forward` in the shim).
        if d.birth_events.contains(&event.id)
            && field(INITIATOR).is_some()
            && field(INITIATOR_CATEGORY).is_some_and(|c| [1.0, 3.0, 6.0].contains(&c))
        {
            d.sampler.birth_waits();
        }
        let max = d.link.outbound().ring().max_payload();
        let mut payload = event.encode();
        if payload.len() > max {
            payload = event.cut().encode();
        }
        if payload.len() > max {
            record(
                d,
                Level::Error,
                "an EVENT over the payload limit",
                &[("id", Value::Int(i64::from(event.id)))],
            );
        }
        // The link counts a record still over the limit in the pending GAP.
        d.link.enqueue(kind::EVENT as u16, payload);
        let us = started.elapsed().as_secs_f64() * 1e6;
        d.link.record_event(us);
        d.sampler.add_event_time(us);
        freed
    })
    .unwrap_or(0)
}

/// Queue a `PLAYER` record from the hook's callbacks. An event-class record:
/// it is held when the ring is full, and a loss is a `GAP`.
pub fn player(record: &Player) {
    with(|d| d.link.enqueue(kind::PLAYER as u16, record.encode()));
}

/// Queue a `MENU` record from the shim's menu callback.
pub fn menu(record: &Menu) {
    with(|d| d.link.enqueue(kind::MENU as u16, record.encode()));
}

/// The most objects `emit` takes.
pub const EMIT_OBJECTS_MAX: usize = 16;
/// The longest `emit` name, in bytes.
pub const EMIT_NAME_MAX: usize = 128;
/// The most bytes an `emit` payload encodes to.
pub const EMIT_PAYLOAD_MAX: usize = 16 * 1024;

/// Queue an `AUTHOR_EVENT` record from `emit`. The reason is `disabled` with
/// raw Lua off or no link, and `objects` for an object the agent's table
/// does not hold (spec/commands.md, "emit"). The caller has checked the name
/// and the payload.
pub fn emit(name: &str, objects: &[u64], payload: Lv) -> Result<(), &'static str> {
    with(|d| {
        if !d.link.raw_lua() {
            return Err("disabled");
        }
        if objects.iter().any(|&o| d.sampler.slot_of(o).is_none()) {
            return Err("objects");
        }
        let record = AuthorEvent {
            objects: objects.to_vec(),
            name: name.into(),
            payload,
        };
        d.link.enqueue(kind::AUTHOR_EVENT as u16, record.encode());
        Ok(())
    })
    .unwrap_or(Err("disabled"))
}

/// Whether raw Lua is on, for `emit`'s fast refusal.
pub fn raw_lua() -> bool {
    with(|d| d.link.raw_lua()).unwrap_or(false)
}

/// Write one `EXTENSION` record in the frame in progress. The record carries
/// the agent's applied revision of the extension, whatever the shim held.
/// Returns whether it was written: a sample is dropped and counted when the
/// ring has no room, and none is written outside a frame.
pub fn extension(mut stats: ExtensionStats) -> bool {
    with(|d| {
        if !d.open {
            return false;
        }
        stats.rev = d
            .link
            .revisions()
            .get(&RevKey::Extension(stats.name.as_bytes().to_vec()));
        d.link.sample(kind::EXTENSION as u16, &stats.encode())
    })
    .unwrap_or(false)
}

/// What the shim needs to run extension hooks in this frame.
pub struct ExtensionBudget {
    /// Microseconds of the frame's commanded-work budget left.
    pub left_us: f64,
    /// The active campaign, as decimal text.
    pub campaign: String,
    /// Raw Lua is on.
    pub raw_lua: bool,
    /// The lifecycle state's code.
    pub lifecycle: u32,
}

/// Charge `spent_us` of extension hook time to the frame's commanded work, as
/// a handler's time is, and report what is left of the budget. `active`, when
/// given, says whether any extension has work for the next pump.
pub fn extension_budget(spent_us: f64, active: Option<bool>) -> ExtensionBudget {
    with(|d| {
        if let Some(active) = active {
            d.ext_active = active;
        }
        if spent_us.is_finite() && spent_us > 0.0 {
            let spent = Duration::from_secs_f64(spent_us.min(1e9) / 1e6);
            d.spent += spent;
            handled(d, spent);
        }
        ExtensionBudget {
            left_us: d.commanded.saturating_sub(d.spent).as_secs_f64() * 1e6,
            campaign: d.link.stamps().campaign.to_string(),
            raw_lua: d.link.raw_lua(),
            lifecycle: d.link.stamps().lifecycle,
        }
    })
    .unwrap_or(ExtensionBudget {
        left_us: 0.0,
        campaign: String::new(),
        raw_lua: false,
        lifecycle: 0,
    })
}

/// The DCS event IDs that remove a row, from the shim's `world.event`.
pub fn set_deaths(ids: Vec<u32>) {
    with(|d| d.deaths = ids);
}

/// The DCS event IDs that ask for a read, from the shim's `world.event`:
/// `which` 1 for the target's life, 2 for the initiator's ammunition, 3 for
/// a birth, 4 for a check of whether the target still exists.
pub fn set_reads(which: u32, ids: Vec<u32>) {
    with(|d| match which {
        1 => d.life_events = ids,
        2 => d.ammo_events = ids,
        3 => d.birth_events = ids,
        4 => d.watch_events = ids,
        _ => {}
    });
}

/// Queue a `LOG` record from Lua: `target` is `hook`, `shim` or `agent`.
pub fn log(level: Level, target: &str, msg: &str, data: Lv) {
    with(|d| enqueue_log(&mut d.link, level_code(level), target, msg, data));
}

/// What the status entry reports about the link.
pub struct Summary {
    /// The mapped file's path.
    pub map: String,
    /// The agent's boot, 16 hex digits.
    pub boot: String,
    /// A sidecar is attached.
    pub attached: bool,
    /// The `lifecycle` code.
    pub lifecycle: u32,
    /// The mission generation.
    pub mission_gen: u32,
    /// The `agent_flags` bits.
    pub flags: u32,
    /// The frame number.
    pub frame: u64,
    /// Objects in the pending-destroy set.
    pub pending_destroy: usize,
    /// Objects over the pending set's capacity, cumulative.
    pub pending_overflow: u64,
    /// Objects dropped from the pending set at the backstop, cumulative.
    pub pending_expired: u64,
}

/// The link's state, if it is open.
pub fn summary() -> Option<Summary> {
    with(|d| Summary {
        map: d.link.map_path().display().to_string(),
        boot: format!("{:016x}", d.link.boot()),
        attached: d.link.attached(),
        lifecycle: d.link.stamps().lifecycle,
        mission_gen: d.link.stamps().mission_gen,
        flags: d.link.flags(),
        frame: d.link.stamps().frame,
        pending_destroy: d.sampler.pending_len(),
        pending_overflow: d.sampler.pending_counts()[1],
        pending_expired: d.sampler.pending_counts()[2],
    })
}

/// The shim's pump began (spec/sampling.md): the births and airbases it
/// holds, and the `server` state's heap. Returns the mission generation and
/// whether to sample, or None with no link.
pub fn pump_begin(births: usize, airbases: usize, heap_kib: f64) -> Option<(u32, bool)> {
    with(|d| {
        if heap_kib.is_finite() && heap_kib >= 0.0 {
            d.link.set_lua_heap(heap_kib as u64);
        }
        d.sampler.set_pending(births, airbases);
        // The sampler begins after the pump's commands (spec/commands.md,
        // "Handlers"), at its first `checks` or `plan`.
        d.begun = None;
        d.pump_commands = Duration::ZERO;
        (d.sampler.generation(), !d.forced)
    })
}

/// Begin the sampler's part of the pump once, after the commands: their
/// time is not the cross-state call's. A pump for commands only does not
/// begin it. Returns whether to sample.
fn begun(d: &mut Driver) -> bool {
    if d.forced {
        return false;
    }
    if let Some(sample) = d.begun {
        return sample;
    }
    d.sampler.excuse(std::mem::take(&mut d.pump_commands));
    let sample = d.sampler.begin(&d.link, Instant::now()).sample;
    d.begun = Some(sample);
    sample
}

/// The objects whose existence the pump checks now, into `out`; how many.
pub fn checks(out: &mut Vec<u64>) -> usize {
    with(|d| if begun(d) { d.sampler.checks(out) } else { 0 }).unwrap_or(0)
}

/// An object in the table no longer exists, with what the reads that tell a
/// death from a removal gave. Returns the slot plus one of the row removed,
/// or 0.
pub fn gone(object: u64, dead: Read<Option<bool>>, life: Read<Option<f64>>) -> u32 {
    with(|d| d.sampler.gone(&mut d.link, object, dead, life)).unwrap_or(0)
}

/// The existence check found the object existing (`sure`), or could not
/// decide.
pub fn alive(object: u64, sure: bool) {
    with(|d| d.sampler.alive(&mut d.link, object, sure));
}

/// The next batch: its tier code, rows and flag, with the slots in `slots`.
pub fn plan(slots: &mut Vec<u32>, values: &mut Vec<i64>) -> Option<(u8, usize, bool)> {
    with(|d| {
        if !begun(d) {
            return None;
        }
        let p = d.sampler.plan(&mut d.link, Instant::now, slots)?;
        d.sampler.buffer_values(p.tier, p.flag, slots, values);
        Some((p.tier as u8, p.rows, p.flag))
    })
    .flatten()
}

/// The pump ended: close the last batch.
pub fn pump_end() {
    with(|d| {
        settle(d, false);
        // A sampling pump whose shim asked for no row still began.
        if !d.forced {
            begun(d);
            d.sampler.end_pump(&mut d.link, Instant::now);
        }
    });
}

/// A position for the batch's next row; true to stop.
pub fn position(p: Option<[f64; 3]>) -> bool {
    with(|d| d.sampler.position_next(p, Instant::now)).unwrap_or(true)
}

/// Attitude for a slot of the batch, from `getPosition()`'s `x` and `y`.
pub fn attitude(slot: u32, x: [f64; 3], y: [f64; 3]) {
    with(|d| d.sampler.attitude(slot, x, y));
}

/// A stopped row's attitude done; true to stop.
pub fn stopped() -> bool {
    with(|d| d.sampler.stopped_done(Instant::now)).unwrap_or(true)
}

/// A static's point, read when its row is added and at each enumeration.
pub fn spot(object: u64, p: [f64; 3]) {
    with(|d| d.sampler.spot(object, p));
}

/// Life and fuel for a slot of the batch; true to stop.
pub fn vitals(slot: u32, life: Option<f64>, fuel: Option<f64>) -> bool {
    with(|d| d.sampler.vitals(slot, life, fuel, Instant::now)).unwrap_or(true)
}

/// A unit's ammunition; true to stop.
pub fn ammo(slot: u32, items: &[(Vec<u8>, f64)]) -> bool {
    with(|d| d.sampler.ammo(&mut d.link, slot, items, Instant::now)).unwrap_or(true)
}

/// One airbase of the batch, with its stock; true to stop.
pub fn airbase(index: u32, airbase: Airbase, stock: Option<Lv>) -> bool {
    with(|d| {
        d.sampler
            .airbase(&mut d.link, index, airbase, stock, Instant::now)
    })
    .unwrap_or(true)
}

/// A birth or an enumeration item done; true to stop.
pub fn item() -> bool {
    with(|d| d.sampler.item(Instant::now)).unwrap_or(true)
}

/// An object the running enumeration found: its slot plus one, 0 when the
/// table lacks it, None when it died or no enumeration runs; and, unless 0,
/// the item counted and whether to stop.
pub fn seen(object: u64) -> (Option<u32>, bool) {
    with(|d| d.sampler.seen_item(&mut d.link, object, Instant::now)).unwrap_or((None, true))
}

/// Add a row: its slot plus one, or 0.
pub fn add(found: Found, enumerated: bool) -> u32 {
    with(|d| d.sampler.add(&mut d.link, found, enumerated)).unwrap_or(0)
}

/// Whether the table would take an object, and whether it died in this
/// generation (the shim then takes it only if it is not dead).
pub fn wants(object: u64) -> (bool, bool) {
    with(|d| (d.sampler.wants(object), d.sampler.died(object))).unwrap_or((false, false))
}

/// The running enumeration read everything.
pub fn enumeration_done() {
    with(|d| d.sampler.enumeration_done(&mut d.link, Instant::now()));
}

/// A getter raised on a slot.
pub fn suspect(slot: u32) {
    with(|d| d.sampler.suspect(slot));
}

/// A getter raised on the batch's next row.
pub fn suspect_next() {
    with(|d| d.sampler.suspect_next());
}

/// Scale the sampling budget (spec/faults.md).
#[cfg(feature = "faults")]
pub fn set_scale(scale: f64) -> Result<(), String> {
    with(|d| d.sampler.set_scale(scale)).ok_or_else(|| "no link is open".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(campaign: u64, entity: u64) -> CommandHeader {
        CommandHeader {
            seq: 1,
            epoch: 1,
            campaign,
            entity,
            rev: 1,
            mission_gen: 1,
        }
    }

    fn s(v: &str) -> Lv {
        Lv::String(v.as_bytes().to_vec())
    }

    fn n(v: f64) -> Lv {
        Lv::Number(v)
    }

    fn table(pairs: Vec<(&str, Lv)>) -> Lv {
        Lv::Table(pairs.into_iter().map(|(k, v)| (s(k), v)).collect())
    }

    /// The arguments of a payload, decoded and sorted by key.
    fn args(opcode: u64, h: &CommandHeader, body: &[u8]) -> Option<Vec<(String, Lv)>> {
        let bytes = arguments(opcode as u16, h, body)?;
        let Lv::Table(pairs) = Reader::new(&bytes).lv().expect("decodes") else {
            panic!("not a table");
        };
        let mut out: Vec<_> = pairs
            .into_iter()
            .map(|(k, v)| match k {
                Lv::String(k) => (String::from_utf8(k).unwrap(), v),
                other => panic!("key {other:?}"),
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Some(out)
    }

    fn expect(opcode: u64, h: &CommandHeader, body: &[u8], mut want: Vec<(&str, Lv)>) {
        want.sort_by(|a, b| a.0.cmp(b.0));
        let want: Vec<_> = want.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        assert_eq!(args(opcode, h, body), Some(want), "opcode {opcode}");
    }

    #[test]
    fn names_carry_the_campaign_tag_and_the_entity_in_base_62() {
        assert_eq!(names::tag_of(71).as_str(), "enP");
        assert_eq!(platform_name(Kind::Group, 71, 36), "np!enPa");
        assert_eq!(platform_name(Kind::Unit, 71, 61), "np.enPz");
        assert_eq!(platform_name(Kind::Static, 71, 62), "np#enP10");
        assert_eq!(
            platform_name(Kind::Unit, 71, (1 << 53) - 1).len(),
            names::MAX_LEN
        );
    }

    #[test]
    fn an_entity_out_of_range_is_a_bad_payload() {
        for e in [0, names::MAX_ENTITY + 1, u64::MAX] {
            assert_eq!(
                arguments(op::STATIC_DESTROY as u16, &header(71, e), &[]),
                None,
                "{e}"
            );
            assert_eq!(
                arguments(op::GROUP_DESTROY as u16, &header(71, e), &[]),
                None,
                "{e}"
            );
        }
        assert!(
            arguments(
                op::STATIC_DESTROY as u16,
                &header(71, names::MAX_ENTITY),
                &[]
            )
            .is_some()
        );
    }

    #[test]
    fn every_layout_decodes_into_its_fields() {
        let h = header(71, 36);
        let group = table(vec![("name", s("np!enPa"))]);
        let mut w = Writer::new();
        w.u32(2).u32(3).str("np!enPa").lv(&group);
        expect(
            op::GROUP_SPAWN,
            &h,
            &w.finish(),
            vec![
                ("country", n(2.0)),
                ("category", n(3.0)),
                ("name", s("np!enPa")),
                ("group", group),
            ],
        );
        let task = table(vec![("id", s("ComboTask"))]);
        let mut w = Writer::new();
        w.lv(&task);
        expect(
            op::GROUP_ORDER,
            &h,
            &w.finish(),
            vec![("name", s("np!enPa")), ("task", task)],
        );
        for opcode in [op::GROUP_DESTROY, op::GROUP_CAPTURE_DESTROY] {
            expect(opcode, &h, &[], vec![("name", s("np!enPa"))]);
        }
        let mut w = Writer::new();
        w.u32(9).pad(4).lv(&Lv::Bool(true));
        expect(
            op::GROUP_OPTION,
            &h,
            &w.finish(),
            vec![
                ("name", s("np!enPa")),
                ("option", n(9.0)),
                ("value", Lv::Bool(true)),
            ],
        );
        let stat = table(vec![("type", s("Barrier"))]);
        let mut w = Writer::new();
        w.u32(1).pad(4).str("x").lv(&stat);
        expect(
            op::STATIC_SPAWN,
            &h,
            &w.finish(),
            vec![
                ("country", n(1.0)),
                ("name", s("np#enPa")),
                ("static", stat),
            ],
        );
        expect(op::STATIC_DESTROY, &h, &[], vec![("name", s("np#enPa"))]);
        let mut w = Writer::new();
        w.u32(2).pad(4).str("Batumi");
        expect(
            op::AIRBASE_OWNER,
            &h,
            &w.finish(),
            vec![("coalition", n(2.0)), ("airbase", s("Batumi"))],
        );
        let stock = table(vec![("liquids", table(vec![]))]);
        for (opcode, key) in [
            (op::WAREHOUSE_SET, "stock"),
            (op::WAREHOUSE_ADJUST, "delta"),
        ] {
            let mut w = Writer::new();
            w.str("Batumi").lv(&stock);
            expect(
                opcode,
                &h,
                &w.finish(),
                vec![("airbase", s("Batumi")), (key, stock.clone())],
            );
        }
        let mut w = Writer::new();
        w.f64(10.5);
        expect(
            op::DAMAGE,
            &h,
            &w.finish(),
            vec![("name", s("np.enPa")), ("life_percent", s("10.5000"))],
        );
        for life in [0.0, 100.0] {
            let mut w = Writer::new();
            w.f64(life);
            assert!(arguments(op::DAMAGE as u16, &h, &w.finish()).is_some());
        }
        expect(
            op::CAPTURE,
            &h,
            &[],
            vec![("name", s("np!enPa")), ("static_name", s("np#enPa"))],
        );
    }

    fn sound(scope: u32, target: u64, file: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(scope).pad(4).u64(target).str(file);
        w.finish()
    }

    fn picture(scope: u32, target: u64, seconds: f64, size: f64, file: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(scope)
            .u32(1)
            .u64(target)
            .f64(seconds)
            .f64(size)
            .u32(2)
            .u32(3)
            .u32(4)
            .pad(4)
            .str(file);
        w.finish()
    }

    #[test]
    fn a_sound_names_its_target_by_scope() {
        let h = header(71, 0);
        expect(
            op::SOUND,
            &h,
            &sound(1, 0, "gong.ogg"),
            vec![
                ("scope", n(1.0)),
                ("target", n(0.0)),
                ("file", s("gong.ogg")),
            ],
        );
        for scope in [2, 5] {
            expect(
                op::SOUND,
                &h,
                &sound(scope, 2, "x.ogg"),
                vec![
                    ("scope", n(f64::from(scope))),
                    ("target", n(2.0)),
                    ("file", s("x.ogg")),
                ],
            );
        }
        // The file is a value, so any text is kept as it is.
        let odd = "a\"b'c\\ \u{e9}";
        expect(
            op::SOUND,
            &h,
            &sound(1, 0, odd),
            vec![("scope", n(1.0)), ("target", n(0.0)), ("file", s(odd))],
        );
        for (scope, name) in [(3, "np!enPa"), (4, "np.enPa")] {
            expect(
                op::SOUND,
                &h,
                &sound(scope, 36, "x.ogg"),
                vec![
                    ("scope", n(f64::from(scope))),
                    ("target", n(36.0)),
                    ("name", s(name)),
                    ("file", s("x.ogg")),
                ],
            );
        }
        // Campaign 0 has no platform names: the name is empty.
        let none = header(0, 0);
        expect(
            op::SOUND,
            &none,
            &sound(3, 36, "x.ogg"),
            vec![
                ("scope", n(3.0)),
                ("target", n(36.0)),
                ("name", s("")),
                ("file", s("x.ogg")),
            ],
        );
        // A scope outside 1 to 5, a target too big for it, a short payload.
        assert!(arguments(op::SOUND as u16, &h, &sound(0, 0, "x")).is_none());
        assert!(arguments(op::SOUND as u16, &h, &sound(6, 0, "x")).is_none());
        assert!(arguments(op::SOUND as u16, &h, &sound(2, 1 << 32, "x")).is_none());
        assert!(arguments(op::SOUND as u16, &h, &sound(4, 1 << 53, "x")).is_none());
        assert!(arguments(op::SOUND as u16, &h, &[0; 15]).is_none());
    }

    #[test]
    fn a_picture_has_its_numbers_as_text() {
        let h = header(71, 0);
        let all = |scope: &str, target: &str| {
            vec![
                ("scope", s(scope)),
                ("target", s(target)),
                ("file", s("Key_1")),
                ("seconds", s("12.5000")),
                ("clear_view", s("true")),
                ("size", s("60.0000")),
                ("horizontal", s("2")),
                ("vertical", s("3")),
                ("size_units", s("4")),
            ]
        };
        expect(
            op::PICTURE,
            &h,
            &picture(1, 0, 12.5, 60.0, "Key_1"),
            all("1", "0"),
        );
        let mut want = all("2", "1");
        expect(
            op::PICTURE,
            &h,
            &picture(2, 1, 12.5, 60.0, "Key_1"),
            want.clone(),
        );
        want[0].1 = s("4");
        want[1].1 = s("36");
        want.push(("name", s("np.enPa")));
        expect(op::PICTURE, &h, &picture(4, 36, 12.5, 60.0, "Key_1"), want);
        let mut want = all("3", "36");
        want.push(("name", s("np!enPa")));
        expect(op::PICTURE, &h, &picture(3, 36, 12.5, 60.0, "Key_1"), want);
        // Out of range, not finite, a bad scope, or a file that is not safe.
        for (scope, seconds, size, file) in [
            (1, -1.0, 1.0, "k"),
            (1, 86_400.5, 1.0, "k"),
            (1, f64::NAN, 1.0, "k"),
            (1, 1.0, f64::INFINITY, "k"),
            (1, 1.0, -0.5, "k"),
            (1, 1.0, 0.0, "k"),
            (1, 1.0, -0.0, "k"),
            (1, 1.0, 1e-5, "k"),
            (1, 1.0, 10_000.5, "k"),
            (0, 1.0, 1.0, "k"),
            (6, 1.0, 1.0, "k"),
            (1, 1.0, 1.0, ""),
            (1, 1.0, 1.0, "a\"); x(\""),
            (1, 1.0, 1.0, "a\\b"),
            (1, 1.0, 1.0, "a\nb"),
            (1, 1.0, 1.0, "a'b"),
        ] {
            assert!(
                arguments(
                    op::PICTURE as u16,
                    &h,
                    &picture(scope, 0, seconds, size, file)
                )
                .is_none(),
                "{scope} {seconds} {size} {file:?}"
            );
        }
        assert!(arguments(op::PICTURE as u16, &h, &[0; 47]).is_none());
        // PICTURE_STOP carries nothing.
        expect(op::PICTURE_STOP, &h, &[], vec![]);
    }

    #[test]
    fn extensions_keep_their_fields() {
        let h = header(1, 0);
        let mut w = Writer::new();
        w.str("ext").lv(&s("return 1"));
        expect(
            op::EXTENSION_SET,
            &h,
            &w.finish(),
            vec![("name", s("ext")), ("source", s("return 1"))],
        );
    }

    #[test]
    fn a_payload_that_does_not_decode_has_no_arguments() {
        let h = header(1, 1);
        let mut w = Writer::new();
        w.u32(2).u32(3).str("n").lv(&Lv::Nil);
        let whole = w.finish();
        for opcode in [
            op::GROUP_SPAWN,
            op::GROUP_ORDER,
            op::GROUP_OPTION,
            op::STATIC_SPAWN,
            op::AIRBASE_OWNER,
            op::WAREHOUSE_SET,
            op::WAREHOUSE_ADJUST,
            op::DAMAGE,
        ] {
            assert!(
                arguments(opcode as u16, &h, &[]).is_none(),
                "{opcode} empty"
            );
        }
        // Cut inside the group table.
        let mut w = Writer::new();
        w.u32(2).u32(3).str("n");
        let mut cut = w.finish();
        cut.push(5);
        cut.extend_from_slice(&9u32.to_le_bytes());
        assert!(arguments(op::GROUP_SPAWN as u16, &h, &cut).is_none());
        assert!(arguments(op::GROUP_SPAWN as u16, &h, &whole).is_some());
        // A DAMAGE life of -0.0 is applied as 0.
        let mut w = Writer::new();
        w.f64(-0.0);
        let got = arguments(op::DAMAGE as u16, &h, &w.finish()).expect("-0.0 life");
        assert!(String::from_utf8_lossy(&got).contains("0.0000"));
        assert!(!String::from_utf8_lossy(&got).contains("-0.0000"));
        // A DAMAGE life that is out of range or not a number.
        for life in [-0.1, 100.1, f64::NAN, f64::INFINITY] {
            let mut w = Writer::new();
            w.f64(life);
            assert!(
                arguments(op::DAMAGE as u16, &h, &w.finish()).is_none(),
                "{life}"
            );
        }
        // A DAMAGE payload one byte short.
        assert!(arguments(op::DAMAGE as u16, &h, &[0; 7]).is_none());
    }

    fn mark(scope: u32, target: u64) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(scope).u32(1).u64(target);
        w.f64(1.5).f64(2.5).f64(-3.5).str("hi");
        w.finish()
    }

    fn message(scope: u32, target: u64, seconds: f64) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(scope).u32(1).u64(target).f64(seconds).str("hello");
        w.finish()
    }

    #[test]
    fn marks_drawings_and_menus_decode_with_their_scope() {
        let h = header(71, 36);
        let entity = ("entity", s("36"));
        expect(
            op::MARK_SET,
            &h,
            &mark(1, 0),
            vec![
                entity.clone(),
                ("scope", n(1.0)),
                ("name", s("")),
                ("read_only", Lv::Bool(true)),
                ("x", n(1.5)),
                ("y", n(2.5)),
                ("z", n(-3.5)),
                ("text", s("hi")),
            ],
        );
        expect(
            op::MARK_SET,
            &h,
            &mark(2, 2),
            vec![
                entity.clone(),
                ("scope", n(2.0)),
                ("target", n(2.0)),
                ("name", s("")),
                ("read_only", Lv::Bool(true)),
                ("x", n(1.5)),
                ("y", n(2.5)),
                ("z", n(-3.5)),
                ("text", s("hi")),
            ],
        );
        let got = args(op::MARK_SET, &h, &mark(3, 37)).expect("group mark");
        assert!(got.contains(&("name".into(), s("np!enPb"))));
        assert!(got.contains(&("target".into(), n(37.0))));
        // A target the binding holds is named by the DCS group ID or the
        // unit's row, with an empty platform name.
        let held = |scope: u32, target: u64| match (scope, target) {
            (3, 37) => Some(Bound::Group(902)),
            (4, 38) => Some(Bound::Row(5)),
            _ => None,
        };
        let bytes = arguments_with(op::MARK_SET as u16, &h, &mark(3, 37), &held).expect("bound");
        let Lv::Table(pairs) = Reader::new(&bytes).lv().expect("decodes") else {
            panic!("not a table");
        };
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == s(k))
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("group_id"), Some(n(902.0)));
        assert_eq!(get("name"), Some(s("")));
        assert_eq!(get("row"), None);
        let w = {
            let mut w = Writer::new();
            w.u32(4).u32(0).u64(38).f64(5.0).str("hi");
            w.finish()
        };
        let bytes = arguments_with(op::MESSAGE as u16, &h, &w, &held).expect("bound unit");
        let Lv::Table(pairs) = Reader::new(&bytes).lv().expect("decodes") else {
            panic!("not a table");
        };
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == s(k))
                .map(|(_, v)| v.clone())
        };
        assert_eq!((get("row"), get("group_id")), (Some(n(5.0)), None));
        // A mark takes no UNIT or COUNTRY scope, and no scope outside 1 to 5.
        for scope in [0, 4, 5, 6] {
            assert!(arguments(op::MARK_SET as u16, &h, &mark(scope, 1)).is_none());
        }
        assert!(arguments(op::MARK_SET as u16, &h, &mark(2, 1 << 32)).is_none());
        assert!(arguments(op::MARK_SET as u16, &h, &mark(3, 1 << 53)).is_none());
        assert!(arguments(op::MARK_SET as u16, &h, &[0; 39]).is_none());

        let shape = table(vec![("0", n(1.0))]);
        let drawing = |scope: u32, target: u64| {
            let mut w = Writer::new();
            w.u32(scope).u32(7).u64(target).lv(&shape);
            w.finish()
        };
        expect(
            op::DRAWING_SET,
            &h,
            &drawing(2, 1),
            vec![
                entity.clone(),
                ("scope", n(2.0)),
                ("target", n(1.0)),
                ("name", s("")),
                ("shape", n(7.0)),
                ("args", shape.clone()),
            ],
        );
        for scope in [0, 3, 4, 5] {
            assert!(arguments(op::DRAWING_SET as u16, &h, &drawing(scope, 1)).is_none());
        }
        assert!(arguments(op::DRAWING_SET as u16, &h, &[0; 16]).is_none());

        let menu = |parent: u64, scope: u32, submenu: u32| {
            let mut w = Writer::new();
            w.u64(parent).u64(37).u32(scope).u32(submenu).str("Menu");
            w.finish()
        };
        expect(
            op::MENU_SET,
            &h,
            &menu(u64::MAX, 3, 1),
            vec![
                entity.clone(),
                ("parent", s("18446744073709551615")),
                ("scope", n(3.0)),
                ("target", n(37.0)),
                ("name", s("np!enPb")),
                ("submenu", Lv::Bool(true)),
                ("text", s("Menu")),
            ],
        );
        let got = args(op::MENU_SET, &h, &menu(0, 1, 0)).expect("root command");
        assert!(got.contains(&("parent".into(), s("0"))));
        assert!(got.contains(&("submenu".into(), Lv::Bool(false))));
        for scope in [0, 4, 5] {
            assert!(arguments(op::MENU_SET as u16, &h, &menu(0, scope, 0)).is_none());
        }
        for opcode in [op::MARK_REMOVE, op::DRAWING_REMOVE, op::MENU_REMOVE] {
            expect(opcode, &h, &[], vec![entity.clone()]);
        }
        // An entity above 2^53 keeps every digit.
        let big = header(71, u64::MAX);
        expect(
            op::MARK_REMOVE,
            &big,
            &[],
            vec![("entity", s("18446744073709551615"))],
        );
    }

    #[test]
    fn a_message_checks_its_scope_and_seconds() {
        let h = header(71, 0);
        expect(
            op::MESSAGE,
            &h,
            &message(4, 36, 10.0),
            vec![
                ("scope", n(4.0)),
                ("name", s("np.enPa")),
                ("clear", Lv::Bool(true)),
                ("seconds", n(10.0)),
                ("text", s("hello")),
            ],
        );
        expect(
            op::MESSAGE,
            &h,
            &message(5, 2, 0.0),
            vec![
                ("scope", n(5.0)),
                ("target", n(2.0)),
                ("name", s("")),
                ("clear", Lv::Bool(true)),
                ("seconds", n(0.0)),
                ("text", s("hello")),
            ],
        );
        for scope in [1, 2, 3] {
            assert!(arguments(op::MESSAGE as u16, &h, &message(scope, 1, 86_400.0)).is_some());
        }
        for scope in [0, 6] {
            assert!(arguments(op::MESSAGE as u16, &h, &message(scope, 1, 1.0)).is_none());
        }
        for seconds in [-0.5, 86_400.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                arguments(op::MESSAGE as u16, &h, &message(1, 0, seconds)).is_none(),
                "{seconds}"
            );
        }
        assert!(arguments(op::MESSAGE as u16, &h, &message(1, 0, -0.0)).is_some());
        assert!(arguments(op::MESSAGE as u16, &h, &[0; 23]).is_none());
    }

    #[test]
    fn chat_load_eval_and_extension_remove_decode() {
        let h = header(0, 0);
        let chat = |player: u32, text: &str| {
            let mut w = Writer::new();
            w.u32(player).pad(4).str(text);
            w.finish()
        };
        expect(
            op::CHAT,
            &h,
            &chat(7, "hi"),
            vec![("player", n(7.0)), ("text", s("hi"))],
        );
        let longest = "a".repeat(1024);
        assert!(arguments(op::CHAT as u16, &h, &chat(0, &longest)).is_some());
        assert!(arguments(op::CHAT as u16, &h, &chat(0, "")).is_none());
        assert!(arguments(op::CHAT as u16, &h, &chat(0, &"a".repeat(1025))).is_none());
        assert!(arguments(op::CHAT as u16, &h, &[0; 7]).is_none());

        let mut w = Writer::new();
        w.str("Missions/a.miz");
        expect(
            op::MISSION_LOAD,
            &h,
            &w.finish(),
            vec![("path", s("Missions/a.miz"))],
        );
        assert!(arguments(op::MISSION_LOAD as u16, &h, &[]).is_none());
        let path = |p: &str| {
            let mut w = Writer::new();
            w.str(p);
            w.finish()
        };
        assert!(arguments(op::MISSION_LOAD as u16, &h, &path("")).is_none());
        assert!(arguments(op::MISSION_LOAD as u16, &h, &path(&"a".repeat(1025))).is_none());
        assert!(arguments(op::MISSION_LOAD as u16, &h, &path(&"a".repeat(1024))).is_some());

        let mut w = Writer::new();
        w.lv(&s("return 1"));
        expect(op::EVAL, &h, &w.finish(), vec![("source", s("return 1"))]);
        assert!(arguments(op::EVAL as u16, &h, &[]).is_none());

        let mut w = Writer::new();
        w.str("ext");
        expect(
            op::EXTENSION_REMOVE,
            &h,
            &w.finish(),
            vec![("name", s("ext"))],
        );
        assert!(arguments(op::EXTENSION_REMOVE as u16, &h, &[]).is_none());
    }

    fn moderate(action: u32, player: u32, seconds: f64, ucid: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(action).u32(player).u32(2).pad(4).f64(seconds);
        w.str(ucid).str("slot-1").str("why");
        w.finish()
    }

    #[test]
    fn moderate_decodes_every_action() {
        let h = header(0, 0);
        for action in 1..=4u32 {
            expect(
                op::MODERATE,
                &h,
                &moderate(action, 7, 30.5, "abc"),
                vec![
                    ("action", n(f64::from(action))),
                    ("player", n(7.0)),
                    ("side", n(2.0)),
                    ("seconds", n(30.5)),
                    ("ucid", s("abc")),
                    ("slot", s("slot-1")),
                    ("reason", s("why")),
                ],
            );
        }
        // Player 0 names none, as a ban by ucid does.
        assert!(arguments(op::MODERATE as u16, &h, &moderate(2, 0, 0.0, "abc")).is_some());
        assert!(arguments(op::MODERATE as u16, &h, &moderate(2, 0, -0.0, "abc")).is_some());
    }

    #[test]
    fn moderate_refuses_what_the_spec_names() {
        let h = header(0, 0);
        let bad = |bytes: Vec<u8>| arguments(op::MODERATE as u16, &h, &bytes).is_none();
        assert!(bad(moderate(0, 7, 0.0, "abc")));
        assert!(bad(moderate(5, 7, 0.0, "abc")));
        assert!(bad(moderate(1, 7, 0.0, "")));
        assert!(bad(moderate(1, 1, 0.0, "abc")));
        for seconds in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(bad(moderate(2, 7, seconds, "abc")), "{seconds}");
        }
        assert!(bad(vec![0; 23]));
        assert!(bad(moderate(1, 7, 0.0, "abc")[..30].to_vec()));
    }

    fn clear(shape: u32, v: [f64; 7]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(shape).pad(4);
        for x in v {
            w.f64(x);
        }
        w.finish()
    }

    #[test]
    fn clear_debris_decodes_a_sphere_and_a_box() {
        let h = header(0, 0);
        let fields = |shape: f64, v: [f64; 7]| {
            let mut out = vec![("volume", n(shape))];
            for (key, x) in ["x", "y", "z", "x2", "y2", "z2", "radius"]
                .into_iter()
                .zip(v)
            {
                out.push((key, n(x)));
            }
            out
        };
        let sphere = [100.0, 5.0, -200.0, 0.0, 0.0, 0.0, 50.0];
        expect(op::CLEAR_DEBRIS, &h, &clear(1, sphere), fields(1.0, sphere));
        let boxed = [-10.0, -1.0, -10.0, 10.0, 1.0, 10.0, 0.0];
        expect(op::CLEAR_DEBRIS, &h, &clear(2, boxed), fields(2.0, boxed));
        // A box may be flat, and the limits are inclusive.
        let flat = [-1e7, 0.0, 0.0, 1e7, 0.0, 1e7, 0.0];
        assert!(arguments(op::CLEAR_DEBRIS as u16, &h, &clear(2, flat)).is_some());
        assert_eq!(
            handler_state(op::CLEAR_DEBRIS as u16),
            Some(HandlerState::Server)
        );
    }

    #[test]
    fn clear_debris_refuses_what_the_spec_names() {
        let h = header(0, 0);
        let bad = |bytes: Vec<u8>| arguments(op::CLEAR_DEBRIS as u16, &h, &bytes).is_none();
        let sphere = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 10.0];
        assert!(bad(clear(0, sphere)));
        assert!(bad(clear(3, sphere)));
        assert!(bad(clear(1, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])));
        assert!(bad(clear(1, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0])));
        assert!(bad(clear(2, [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])));
        assert!(bad(clear(2, [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0])));
        assert!(bad(clear(2, [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0])));
        for at in 0..7 {
            for x in [
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                1.0e7 + 1.0,
                -1.0e7 - 1.0,
            ] {
                let mut v = sphere;
                v[at] = x;
                assert!(bad(clear(1, v)), "{at} {x}");
            }
        }
        assert!(bad(clear(1, sphere)[..63].to_vec()));
    }

    #[test]
    fn the_player_opcodes_run_in_the_hook() {
        for opcode in [op::MODERATE, op::PLAYERS, op::BANS] {
            assert_eq!(handler_state(opcode as u16), Some(HandlerState::Hook));
        }
        for opcode in [op::PLAYERS, op::BANS] {
            let h = header(0, 0);
            assert_eq!(
                arguments(opcode as u16, &h, &[]),
                Some(Fields::default().finish())
            );
        }
    }

    #[test]
    fn chat_and_mission_load_run_in_the_hook() {
        assert_eq!(handler_state(op::CHAT as u16), Some(HandlerState::Hook));
        assert_eq!(
            handler_state(op::MISSION_LOAD as u16),
            Some(HandlerState::Hook)
        );
        assert_eq!(
            handler_state(op::MESSAGE as u16),
            Some(HandlerState::Server)
        );
    }

    #[test]
    fn a_spawn_moves_its_id_into_object() {
        let spawn = op::GROUP_SPAWN as u16;
        let stat = op::STATIC_SPAWN as u16;
        assert_eq!(
            spawn_result(spawn, result::APPLIED, n(1_000_001.0)),
            (1_000_001, Lv::Nil)
        );
        assert_eq!(
            spawn_result(stat, result::APPLIED, n(16_777_728.0)),
            (16_777_728, Lv::Nil)
        );
        // Failures and other opcodes keep their value.
        assert_eq!(
            spawn_result(spawn, result::DCS_REFUSED, n(5.0)),
            (0, n(5.0))
        );
        let capture = op::CAPTURE as u16;
        assert_eq!(spawn_result(capture, result::APPLIED, n(5.0)), (0, n(5.0)));
        assert_eq!(spawn_result(spawn, result::APPLIED, Lv::Nil), (0, Lv::Nil));
        assert_eq!(spawn_result(spawn, result::APPLIED, n(-1.0)), (0, n(-1.0)));
    }
}
