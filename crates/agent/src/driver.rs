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
use nanoprobe_agent_link::{Admit, Link, Options};
use nanoprobe_transport::codec::{Lv, Reader};
use nanoprobe_transport::layout::{kind, lifecycle, result};
use nanoprobe_transport::log::{Level, Value};
use nanoprobe_transport::records::{CommandHeader, Event, Log};

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

/// The opcode of `CAMPAIGN` (spec/commands.md).
const CAMPAIGN: u16 = 1;

/// Breadcrumb codes for the status block's `crumb` (spec/transport.md,
/// "Breadcrumbs").
pub mod crumbs {
    /// A command's handler runs: this plus its opcode, with `crumb_arg` its
    /// sequence number.
    pub const COMMAND: u64 = 0x1_0000;
}

struct Driver {
    link: Link,
    root: PathBuf,
    /// The commanded-work budget per frame.
    commanded: Duration,
    theater: String,
    mission: String,
    payload: Vec<u8>,
    /// A reload request could not be removed, and that was logged.
    reload_failed: bool,
}

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
    let mut d = Driver {
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
    /// `flood` (records), or `lose-control`, which drops the link's next
    /// control record.
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

/// One frame: DCS real time, model time, and whether DCS is paused.
pub fn frame(real: f64, model: f64, paused: bool) -> Frame {
    with(|d| frame_with(d, real, model, paused)).unwrap_or(Frame::NoLink)
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
    d.link.end();
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
    let code = match d.link.admit(&h) {
        Admit::Control if opcode == CAMPAIGN => match rd.u64() {
            Ok(campaign) => {
                d.link.set_campaign(campaign);
                None
            }
            Err(_) => Some(result::BAD_PAYLOAD),
        },
        Admit::Control | Admit::Run => Some(result::UNKNOWN_OPCODE),
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
            d.link.new_generation();
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
/// `LOG` at `error` and a count in the pending `GAP` of events.
pub fn event(event: Event, started: Instant) {
    with(|d| {
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
        d.link.record_event(started.elapsed().as_secs_f64() * 1e6);
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
