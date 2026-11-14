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
pub use nanoprobe_agent_link::sampler::{Found, Read};
use nanoprobe_agent_link::sampler::{Rates, Report, Sampler, clock_cost};
use nanoprobe_agent_link::{Admit, Link, Options};
use nanoprobe_transport::codec::{Lv, Reader};
use nanoprobe_transport::layout::{kind, lifecycle, result};
use nanoprobe_transport::log::{Level, Value};
use nanoprobe_transport::records::{Airbase, Bind, CommandHeader, Event, Log, Subscribe};

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
/// paused. Returns what the hook does next, and whether this is a sampling
/// frame: the hook then calls the shim's pump, and `finish` after it. A
/// frame that does not sample ends here (spec/sampling.md, "The frame").
///
/// `read`, when the caller's state has a position getter by object ID, reads
/// one unit's position: `Ok(None)` when the getter gave no position, `Err`
/// when it raised. The frame then sweeps the positions it owes with it,
/// before it decides whether to pump (spec/sampling.md, "The frame").
pub fn frame(
    real: f64,
    model: f64,
    paused: bool,
    mut read: Option<&mut ReadPosition<'_>>,
) -> (Frame, bool) {
    with(|d| {
        if d.open {
            // The hook did not call finish for the last frame.
            end_frame(d);
        }
        let code = frame_with(d, real, model, paused);
        let sampling = d.open
            && d.link.reading_commands()
            && [lifecycle::RUNNING as u32, lifecycle::PAUSED as u32]
                .contains(&d.link.stamps().lifecycle);
        d.sampler.set_direct(read.is_some());
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
        let pump = d.open && d.sampler.gate(&d.link, now) && sampling;
        if !pump && d.open {
            // Nothing comes between this and `finish` on a frame that does
            // not sample, so the frame ends here and the hook makes one
            // call, not two.
            end_frame(d);
        }
        (code, pump)
    })
    .unwrap_or((Frame::NoLink, false))
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
            ("close_us", Value::Num(r.close_us)),
            ("batch_us", Value::Num(r.batch_us)),
            ("us_fixed", Value::Num(r.spend.fixed_us)),
            ("us_xcall", Value::Num(r.spend.xcall_us)),
            ("us_pump", Value::Num(r.spend.pump_us)),
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
            ("gone_dead", Value::Int(gone[0] as i64)),
            ("gone_removed", Value::Int(gone[1] as i64)),
            ("gone_missing", Value::Int(gone[2] as i64)),
            ("classifier_anomalies", Value::Int(gone[3] as i64)),
            ("classifier_faults", Value::Int(gone[4] as i64)),
            ("revivals", Value::Int(revivals as i64)),
            ("marks_cleared", Value::Int(marks_cleared as i64)),
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
/// commands that are not control commands counts.
fn commands(d: &mut Driver) {
    let mut payload = std::mem::take(&mut d.payload);
    let mut spent = Duration::ZERO;
    let mut first = true;
    while first || spent < d.commanded {
        let Some(opcode) = d.link.take_command(&mut payload) else {
            break;
        };
        first = false;
        spent += run_command(d, opcode, &payload);
    }
    d.payload = payload;
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
    let code = match d.link.admit(&h) {
        Admit::Control => control(d, opcode, body),
        Admit::Run => Some(result::UNKNOWN_OPCODE),
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
        let b = d.sampler.begin(&d.link, Instant::now());
        (b.generation, b.sample)
    })
}

/// The objects whose existence the pump checks now, into `out`; how many.
pub fn checks(out: &mut Vec<u64>) -> usize {
    with(|d| d.sampler.checks(out)).unwrap_or(0)
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
        let p = d.sampler.plan(&mut d.link, Instant::now, slots)?;
        d.sampler.buffer_values(p.tier, p.flag, slots, values);
        Some((p.tier as u8, p.rows, p.flag))
    })
    .flatten()
}

/// The pump ended: close the last batch.
pub fn pump_end() {
    with(|d| d.sampler.end_pump(&mut d.link, Instant::now));
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
