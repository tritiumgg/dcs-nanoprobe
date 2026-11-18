//! The world commands' exit test (step S15), run by `command-probe --part
//! world`: the sidecar's real store and reconciler over the real link
//! ([`LinkSim`]), against an agent in DCS or in the stock-Lua mock.
//!
//! The exit test, as the plan words it: 100 groups spawned and removed with
//! every over-budget frame attributed to a command; and, while paused, a
//! group destroyed and added again under the same name in one call is one
//! group after a resume, with its new units.
//!
//! `--scenario groups` (part A) and `--scenario paused` (part B) are two
//! runs of the probe, each with its own `--campaign`.
//!
//! # Part A: `--scenario groups`
//!
//! `--groups` (100) managed ground groups of `--units` (4) `M-113`s each go
//! into the store on a grid of `--spacing` meters around `--origin-x` and
//! `--origin-z`, under DCS authority, and the reconciler spawns them, at most
//! `--per-step` (10) unanswered at a time (`materialize_per_frame`) so that
//! one frame can hold several commands. The
//! run waits until every group is `live` and every unit's `OBJECT` came, then
//! sends `DAMAGE` 50 % to the first unit of every group (the reconciler's damage
//! effect, which the hook dispatches) and reads each unit's life back from a
//! `VITALS` row, then removes every group from the store, and waits until every
//! group is `virtual` and every unit's `OBJECT_GONE` with `DESTROYED` came,
//! and until the agent's `sampling` log reports `pending_destroy` 0.
//!
//! ## Damage
//!
//! Every `DAMAGE` must be `APPLIED`. The unit's life, from a `VITALS` row that
//! comes after the damage, must be within `DAMAGE_TOLERANCE` (0.10) of half
//! the `life0` of its `OBJECT`: DCS's `getLife` may differ from the set
//! percentage by a rounding, and an undamaged unit reads its `life0`, for
//! example 3 for an M-113 (LIVE, E16), which is outside the tolerance. The
//! `RESULT`s' `cost_us` give the command's cost (median, p99, maximum), and
//! the `DAMAGE` results are among those the over-budget analysis judges.
//!
//! ## Over budget
//!
//! A frame's commanded work is what the agent spent on the commands it took
//! in that frame. The agent takes commands in ring order while the time spent
//! so far in the frame is under the commanded-work budget, and always one
//! (spec/commands.md, "Budget"). The link shows two things of it: each
//! sequenced command's `RESULT` carries `cost_us` and follows the `FRAME`
//! record of the frame that took it, and the status block counts
//! `commands_over_budget`, one for each sequenced command whose own time
//! passed the budget. The link does not show an agent time for each frame:
//! the status block holds histograms, and the 250 us figure is only in a
//! 10 s `sampling` log. So the definitions below use what each frame's
//! `RESULT`s say, which is exact for commanded work.
//!
//! - An over-budget command is a sequenced `RESULT` with `cost_us` above the
//!   budget (`--commanded-ms`, 2.0 by default, the agent's setting). A control
//!   command costs nothing against the budget and is left out. The agent's
//!   `cost_us` is its time cut to whole microseconds, so a `cost_us` equal to
//!   the budget may or may not have been counted: the counter must lie
//!   between the number above the budget and the number at or above it.
//! - An over-budget frame is a frame whose commands' `cost_us` add up to
//!   more than the budget.
//! - An over-budget frame is attributed to a command when exactly one
//!   command of the frame carried it over: the commands before it add up to
//!   no more than the budget. Then the agent stopped after it, as the rule
//!   says. A frame where the commands before the last already passed the
//!   budget means the agent kept taking commands with the budget spent, and
//!   its excess belongs to no command.
//!
//! The run passes when: every group was spawned once and removed, every unit
//! had one `OBJECT` and one `OBJECT_GONE` with `DESTROYED`, no `OBJECT` or
//! `RESULT` is missing or repeated, no over-budget frame is unattributed, and
//! the change in `commands_over_budget` over the run lies between the two
//! counts above, every `DAMAGE` was applied and read back, and the agent's
//! `sampling` log shows `pending_expired` 0, `pending_overflow` 0 and, after
//! the removal, `pending_destroy` 0 (the agent reports every 10 s, so the
//! probe waits `DRAIN_S` for a report after the removal). The probe waits two frames after the last `RESULT` before
//! it reads the counter, since the agent stores counters at the end of the
//! frame.
//!
//! # Part B: `--scenario paused`
//!
//! The probe spawns one group G through the reconciler and prints
//! `{"ev":"phase","name":"spawned",...}`. The caller pauses the simulation
//! and creates `paused` in `--control-dir`. The probe checks that the frames
//! say `PAUSED`, then changes G's spec in the store (each unit's type becomes
//! `BTR-80`, as a PutGroup replacement does) and lets the reconciler send the
//! replacing `GROUP_SPAWN`, a destroy and an add in one call. It prints phase
//! `replaced` with what the agent said while paused. The caller resumes and creates `resumed`. The probe
//! waits `--settle` (5) seconds of model time, requests a full enumeration
//! (`ENUMERATE`), and prints phase `settled`. The caller may then read DCS
//! itself, and creates `checked`. The probe destroys G and prints the
//! verdict.
//!
//! The run passes when: the replace was `APPLIED`; `OBJECT_GONE` with
//! `DESTROYED` came exactly once for each old unit; the enumeration holds
//! exactly G's units under G's name, and none of the old object IDs anywhere; and each new unit's `OBJECT` came.
//!
//! # The naming check: `--scenario names`
//!
//! The probe creates one group of `--units` `M-113`s and one `Hangar A`
//! static, and waits until both are `live` and every unit's and the static's
//! `OBJECT` came. It prints phase `spawned` with the names DCS reported. With
//! `--control-dir` it waits there for `checked`, so the caller can read DCS.
//! It then restarts the sidecar: a new link session and a new reconciler from
//! the kept store, with no memory of the binding. It waits for a full
//! enumeration that begins after the new `HELLO` and prints phase
//! `rebound`. Then it removes both entities and waits for every
//! `OBJECT_GONE` with `DESTROYED`.
//!
//! The run passes when: every name DCS reported is the platform name of the
//! campaign's tag (spec/world.md, "Names"), with the entity and the group's
//! name the agent bound it to; after the restart the enumeration reports
//! every unit and the static again under the same names and entities, both
//! entities stay `live`, and no spawn is sent; the reconciler counted no
//! `foreign_name`; and every object went `DESTROYED` at the removal.
//!
//! # Clearing debris: `--scenario debris`
//!
//! The probe creates `--groups` groups of `--units` `M-113`s on the grid of
//! part A, and one control group of `--units` 1,500 m from the grid, and waits
//! until all are `live` with every unit's `OBJECT`. It prints phase `spawned`
//! with the grid's unit names and waits in `--control-dir` for `go`: the
//! caller kills the grid's units with scripted explosions, runs model time
//! until their wrecks stop burning, and writes the ground height at the first
//! group into `go`. The probe then sends three `CLEAR_DEBRIS`:
//! a sphere on the first group, a box round the second, and a clear of the
//! whole map (the reconciler's tiles), and waits for their results.
//!
//! The run passes when: the three outcomes are `APPLIED`; the sphere and the
//! box each removed at least `--units` junk objects, a group's wrecks, and the
//! whole map at least those of the other `--groups` less 2; no control unit
//! went; the clears spread over more than one frame; every over-budget frame
//! of the run is attributed to one command; and the agent's over-budget
//! counter fits the results. The verdict gives each clear's count and cost,
//! the whole map's tiles' cost (median, p99, maximum) and the frames they took.
//! The caller reads DCS after the verdict: every killed unit's wreck removed
//! and every control unit alive.
//!
//! Test tooling only: built from source, never released.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::link::Config;
use nanoprobe_sidecar::reconciler::{Config as RecConfig, Reconciler};
use nanoprobe_sidecar::sim::wire::{Body, Command, Debris, Input, gone_reason, lv, opcode};
use nanoprobe_sidecar::sim::{Detached, RawRecord, SimAdapter};
use nanoprobe_sidecar::store::{
    Authority, Entry, NewGroup, NewUnit, Outcome, Point, Store, UnitState,
};
use nanoprobe_transport::codec::Lv;
use nanoprobe_transport::layout::status as s;
use nanoprobe_transport::layout::{kind, lifecycle, result};
use nanoprobe_transport::map::MappedFile;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::records::{
    CommandHeader, CommandResult, Log as LogRecord, Object, ObjectGone,
};

use crate::linksim::LinkSim;
use crate::{arg, json_string, read_token};

/// A `RESULT` with the frame it followed.
#[derive(Clone, Debug)]
pub struct Stamped {
    /// The frame sequence number of the `FRAME` before the `RESULT`.
    pub frame: u64,
    /// The `RESULT`.
    pub result: CommandResult,
}

/// One full enumeration as the probe read it.
#[derive(Clone, Debug, Default)]
pub struct EnumRun {
    /// The enumeration's number.
    pub id: u64,
    /// Its `OBJECT` rows.
    pub objects: Vec<Object>,
    /// Whether `ENUM_END` came.
    pub ended: bool,
}

/// Everything the probe read from the agent, in ring order.
#[derive(Default)]
pub struct Log {
    /// The frame number of the last `FRAME`.
    pub frame: u64,
    /// Its model time, seconds.
    pub model_time: f64,
    /// The lifecycle code of the last current (not replayed) `FRAME`.
    pub lifecycle: u32,
    /// `FRAME` records read.
    pub frames_read: u64,
    /// Every `RESULT`.
    pub results: Vec<Stamped>,
    /// Every `OBJECT`.
    pub objects: Vec<Object>,
    /// Every `OBJECT_GONE`.
    pub gone: Vec<ObjectGone>,
    /// Every enumeration, in the order its `ENUM_BEGIN` came.
    pub enums: Vec<EnumRun>,
    /// The latest `VITALS` life of each DCS object.
    pub vitals: BTreeMap<u64, f32>,
    /// Every `sampling` log of the agent, in ring order.
    pub sampling: Vec<Sampling>,
}

/// The pending-destroy fields of one `sampling` log. A field the agent left
/// out reads `u64::MAX`, which no check accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sampling {
    /// `pending_destroy`: destroys the agent still holds.
    pub destroy: u64,
    /// `pending_overflow`: destroys it could not hold.
    pub overflow: u64,
    /// `pending_expired`: destroys it gave up on.
    pub expired: u64,
}

impl Sampling {
    /// The fields of a `sampling` log's `data` table.
    pub fn from_data(data: &Lv) -> Sampling {
        let field = |key: &str| {
            let Lv::Table(pairs) = data else {
                return u64::MAX;
            };
            pairs
                .iter()
                .find_map(|(k, v)| match (k, v) {
                    (Lv::String(k), Lv::Number(n)) if k == key.as_bytes() && *n >= 0.0 => {
                        Some(*n as u64)
                    }
                    _ => None,
                })
                .unwrap_or(u64::MAX)
        };
        Sampling {
            destroy: field("pending_destroy"),
            overflow: field("pending_overflow"),
            expired: field("pending_expired"),
        }
    }
}

struct Tap<'a> {
    sim: &'a mut LinkSim,
    log: &'a mut Log,
}

impl SimAdapter for Tap<'_> {
    fn position_after(&self, opcode: u16, payload: &[u8]) -> u64 {
        self.sim.position_after(opcode, payload)
    }

    fn send(&mut self, opcode: u16, payload: &[u8]) -> Result<u64, Detached> {
        self.sim.send(opcode, payload)
    }

    fn read(&mut self) -> Vec<RawRecord> {
        let records = self.sim.read();
        for r in &records {
            self.log.take(r);
        }
        records
    }

    fn flush(&mut self) {
        self.sim.flush();
    }
}

impl Log {
    fn take(&mut self, r: &RawRecord) {
        if u64::from(r.kind) == kind::LOG {
            // The `sampling` log is the only agent log the probe keeps; it is
            // printed so the caller sees it too.
            if let Ok(l) = LogRecord::decode(&r.payload)
                && l.msg == "sampling"
            {
                let x = Sampling::from_data(&l.data);
                println!(
                    "{{\"ev\":\"log\",\"msg\":\"sampling\",\"data\":{{\"pending_destroy\":{},\
                     \"pending_overflow\":{},\"pending_expired\":{}}}}}",
                    x.destroy, x.overflow, x.expired
                );
                self.sampling.push(x);
            }
            return;
        }
        let Ok(input) = Input::decode(r.kind, &r.payload) else {
            return;
        };
        match input {
            Input::Frame(f) => {
                self.frame = f.frame;
                self.model_time = f.model_time;
                self.frames_read += 1;
                if f.replay == 0 {
                    self.lifecycle = f.lifecycle;
                }
            }
            Input::Result(res) => self.results.push(Stamped {
                frame: self.frame,
                result: res,
            }),
            Input::Object(o) => {
                if o.enumeration != 0
                    && let Some(run) = self.enums.iter_mut().rev().find(|e| e.id == o.enumeration)
                {
                    run.objects.push(o.clone());
                }
                self.objects.push(o);
            }
            Input::ObjectGone(g) => self.gone.push(g),
            Input::Vitals(rows) => {
                for v in rows {
                    self.vitals.insert(v.object, v.life);
                }
            }
            Input::EnumBegin(e) => self.enums.push(EnumRun {
                id: e.enumeration,
                ..EnumRun::default()
            }),
            Input::EnumEnd(e) => {
                if let Some(run) = self.enums.iter_mut().rev().find(|r| r.id == e.enumeration) {
                    run.ended = true;
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// The over-budget analysis.

/// One frame's commands, from its `RESULT`s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameCost {
    /// The frame sequence number.
    pub frame: u64,
    /// Sequenced commands answered in it.
    pub commands: u32,
    /// The sum of their `cost_us`.
    pub total_us: u64,
    /// The sum without the last command's.
    pub before_last_us: u64,
    /// The last command's `(seq, opcode, cost_us)`.
    pub last: (u64, u16, u32),
}

/// What the `RESULT`s say of the budget.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BudgetReport {
    /// The budget, microseconds.
    pub budget_us: u64,
    /// Sequenced `RESULT`s.
    pub results: u64,
    /// Those with `cost_us` above the budget.
    pub over_above: u64,
    /// Those with `cost_us` at or above the budget.
    pub over_at_or_above: u64,
    /// Frames with at least one sequenced command.
    pub frames_with_commands: u64,
    /// Frames whose commands add up to more than the budget.
    pub over_frames: Vec<u64>,
    /// Over-budget frames the agent kept taking commands in after the
    /// budget was spent.
    pub unattributed: Vec<u64>,
    /// The largest frame total, microseconds.
    pub max_frame_us: u64,
    /// The most commands in one frame.
    pub max_commands_in_frame: u32,
}

/// The frames of `results`, in frame order. Control commands (sequence 0)
/// are left out: they cost nothing against the budget.
pub fn frame_costs(results: &[Stamped]) -> Vec<FrameCost> {
    let mut by: BTreeMap<u64, FrameCost> = BTreeMap::new();
    for st in results {
        let r = &st.result;
        if r.seq == 0 {
            continue;
        }
        let f = by.entry(st.frame).or_insert(FrameCost {
            frame: st.frame,
            commands: 0,
            total_us: 0,
            before_last_us: 0,
            last: (0, 0, 0),
        });
        f.before_last_us = f.total_us;
        f.commands += 1;
        f.total_us += u64::from(r.cost_us);
        f.last = (r.seq, r.opcode, r.cost_us);
    }
    by.into_values().collect()
}

/// The budget report for `results` against `budget_us`.
pub fn budget_report(results: &[Stamped], budget_us: u64) -> BudgetReport {
    let mut rep = BudgetReport {
        budget_us,
        ..BudgetReport::default()
    };
    for st in results.iter().filter(|st| st.result.seq != 0) {
        rep.results += 1;
        let c = u64::from(st.result.cost_us);
        rep.over_above += u64::from(c > budget_us);
        rep.over_at_or_above += u64::from(c >= budget_us);
    }
    for f in frame_costs(results) {
        rep.frames_with_commands += 1;
        rep.max_frame_us = rep.max_frame_us.max(f.total_us);
        rep.max_commands_in_frame = rep.max_commands_in_frame.max(f.commands);
        if f.total_us > budget_us {
            rep.over_frames.push(f.frame);
            if f.before_last_us > budget_us {
                rep.unattributed.push(f.frame);
            }
        }
    }
    rep
}

impl BudgetReport {
    /// Whether the counter's change fits the results: at least the commands
    /// above the budget and at most those at or above it.
    pub fn counter_fits(&self, delta: u64) -> bool {
        (self.over_above..=self.over_at_or_above).contains(&delta)
    }
}

// ---------------------------------------------------------------------------
// Arguments and the run's context.

/// How far a damaged unit's life may sit from half its `life0`, as a share of
/// `life0` (see the module's "Damage").
pub const DAMAGE_TOLERANCE: f64 = 0.10;
/// Wall seconds to wait for every damaged unit's `VITALS` row.
const LIFE_S: u64 = 120;
/// Wall seconds to wait, after the removal, for an agent `sampling` log that
/// shows no pending destroy: the agent reports every 10 s, so this is three
/// reports.
const DRAIN_S: u64 = 35;

/// The median, the 99th percentile (nearest rank) and the maximum of `v`;
/// zeros when it is empty.
pub fn cost_stats(v: &[u32]) -> (u32, u32, u32) {
    if v.is_empty() {
        return (0, 0, 0);
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let rank = |p: f64| s[((p * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1];
    (rank(0.5), rank(0.99), *s.last().expect("not empty"))
}

/// Whether `life` is half of `life0`, within [`DAMAGE_TOLERANCE`] of `life0`.
pub fn about_half(life: f32, life0: f32) -> bool {
    life0 > 0.0
        && (f64::from(life) - f64::from(life0) / 2.0).abs() <= DAMAGE_TOLERANCE * f64::from(life0)
}

/// What the damage step found.
#[derive(Default)]
struct DamageReport {
    sent: usize,
    applied: usize,
    read_back: usize,
    /// `cost_us` of the `DAMAGE` `RESULT`s.
    costs: Vec<u32>,
    /// Damage results above the budget, and in an over-budget frame.
    over_budget: usize,
    in_over_frames: usize,
}

struct Opts {
    scenario: String,
    campaign: u64,
    groups: usize,
    units: usize,
    per_step: usize,
    origin: (f64, f64),
    spacing: f64,
    commanded_ms: f64,
    seconds: u64,
    settle: f64,
    control_dir: Option<PathBuf>,
    trace: bool,
}

fn num<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> Result<T, String> {
    arg(args, flag).map_or(Ok(default), |v| {
        v.parse().map_err(|_| format!("{flag} takes a number"))
    })
}

fn opts(args: &[String]) -> Result<Opts, String> {
    let o = Opts {
        scenario: arg(args, "--scenario").unwrap_or_else(|| "groups".into()),
        campaign: num(args, "--campaign", 7u64)?,
        groups: num(args, "--groups", 100usize)?,
        units: num(args, "--units", 4usize)?,
        per_step: num(args, "--per-step", 10usize)?,
        origin: (
            num(args, "--origin-x", 0.0f64)?,
            num(args, "--origin-z", 0.0f64)?,
        ),
        spacing: num(args, "--spacing", 150.0f64)?,
        commanded_ms: num(args, "--commanded-ms", 2.0f64)?,
        seconds: num(args, "--seconds", 600u64)?,
        settle: num(args, "--settle", 5.0f64)?,
        control_dir: arg(args, "--control-dir").map(PathBuf::from),
        trace: args.iter().any(|a| a == "--trace"),
    };
    if o.groups == 0 || o.units == 0 || o.units > 50 {
        return Err("--groups is at least 1 and --units between 1 and 50".into());
    }
    if !(o.commanded_ms > 0.0 && o.commanded_ms <= 1000.0) {
        return Err("--commanded-ms is above 0 and at most 1000".into());
    }
    if o.scenario == "names" && o.campaign == 0 {
        return Err("--scenario names needs a campaign".into());
    }
    if matches!(o.scenario.as_str(), "paused" | "debris") && o.control_dir.is_none() {
        return Err(format!("--scenario {} needs --control-dir", o.scenario));
    }
    if o.scenario == "debris" && o.groups < 3 {
        return Err("--scenario debris needs --groups 3 or more".into());
    }
    Ok(o)
}

struct Ctx {
    o: Opts,
    cfg: Config,
    map: PathBuf,
    store: Store,
    rec: Reconciler,
    sim: LinkSim,
    log: Log,
    deadline: Instant,
    last_progress: Instant,
}

impl Ctx {
    fn step(&mut self) {
        self.rec.step(
            &mut self.store,
            &mut Tap {
                sim: &mut self.sim,
                log: &mut self.log,
            },
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    /// Read records without the reconciler.
    fn read(&mut self) {
        Tap {
            sim: &mut self.sim,
            log: &mut self.log,
        }
        .read();
        std::thread::sleep(Duration::from_millis(2));
    }

    fn check_deadline(&self, what: &str) -> Result<(), String> {
        if Instant::now() > self.deadline {
            return Err(format!("timed out waiting for {what}"));
        }
        Ok(())
    }

    fn progress(&mut self, text: &str) {
        if self.last_progress.elapsed() > Duration::from_secs(5) {
            self.last_progress = Instant::now();
            println!(
                "{{\"ev\":\"progress\",\"what\":{},\"frame\":{},\"results\":{},\"objects\":{},\
                 \"gone\":{}}}",
                json_string(text),
                self.log.frame,
                self.log.results.len(),
                self.log.objects.len(),
                self.log.gone.len()
            );
        }
    }

    /// Step until `done`, for at most `secs` seconds; whether it was done. The
    /// run's own deadline still fails it.
    fn until_for(
        &mut self,
        what: &str,
        secs: u64,
        mut done: impl FnMut(&Ctx) -> bool,
    ) -> Result<bool, String> {
        let by = Instant::now() + Duration::from_secs(secs);
        while !done(self) {
            if Instant::now() > by {
                return Ok(false);
            }
            self.check_deadline(what)?;
            self.step();
            self.progress(what);
        }
        Ok(true)
    }

    /// Step until `done`, or fail on the deadline.
    fn until(&mut self, what: &str, mut done: impl FnMut(&Ctx) -> bool) -> Result<(), String> {
        while !done(self) {
            self.check_deadline(what)?;
            self.step();
            self.progress(what);
        }
        Ok(())
    }

    /// The status block's `commands_over_budget`.
    fn counter(&self) -> Result<u64, String> {
        let m = MappedFile::open_read_only(&self.map).map_err(|e| format!("map: {e}"))?;
        Ok(m.load(s::COMMANDS_OVER_BUDGET, Ordering::Acquire))
    }

    /// Pumps in the buckets from 256 us up, from `hist_pump`.
    fn pumps_ge_256(&self) -> Result<u64, String> {
        let m = MappedFile::open_read_only(&self.map).map_err(|e| format!("map: {e}"))?;
        let first = nanoprobe_transport::status::bucket(256.0);
        Ok((first..s::HIST_BUCKETS)
            .map(|i| m.load(s::HIST_PUMP + 8 * i, Ordering::Relaxed))
            .sum())
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        self.until("a ready link", |c| c.rec.ready(&c.store) && c.sim.ready())
    }
}

/// Run the scenario `--scenario` names; whether it passed.
pub fn run(args: &[String]) -> Result<bool, String> {
    let o = opts(args)?;
    let need = |flag: &str| arg(args, flag).ok_or(format!("{flag} is required"));
    let token = read_token(need("--token-file")?.as_ref()).map_err(|e| format!("token: {e}"))?;
    let port: u16 = need("--port")?
        .parse()
        .map_err(|_| "--port takes a number")?;
    let map = PathBuf::from(need("--map")?);
    let cfg = Config {
        map_path: map.clone(),
        handshake: ([127, 0, 0, 1], port).into(),
        token,
        poll: Duration::from_millis(2),
        handshake_timeout: Duration::from_secs(15),
    };
    let store = Store::new(CampaignId(o.campaign), 1_000);
    let rec = Reconciler::new(
        RecConfig {
            materialize_per_frame: o.per_step.max(1),
            ..RecConfig::default()
        },
        &store,
    );
    let sim = LinkSim::start(cfg.clone(), Default::default());
    let mut c = Ctx {
        cfg,
        deadline: Instant::now() + Duration::from_secs(o.seconds),
        o,
        map,
        store,
        rec,
        sim,
        log: Log::default(),
        last_progress: Instant::now(),
    };
    match c.o.scenario.as_str() {
        "groups" => groups(&mut c),
        "paused" => paused(&mut c),
        "names" => names(&mut c),
        "debris" => debris(&mut c),
        other => Err(format!(
            "--scenario is groups, paused, names or debris, not {other}"
        )),
    }
}

fn unit_name(c: &Ctx, id: EntityId) -> String {
    name(NameKind::Unit, CampaignId(c.o.campaign), id)
}

// ---------------------------------------------------------------------------
// The naming check.

fn names(c: &mut Ctx) -> Result<bool, String> {
    use nanoprobe_transport::layout::opcode;
    c.wait_ready()?;
    let theater = c
        .store
        .loaded()
        .map(|l| l.theater.clone())
        .unwrap_or_default();
    let at = |dx: f64, dz: f64| UnitState {
        at: Point {
            x: c.o.origin.0 + dx,
            y: 0.0,
            z: c.o.origin.1 + dz,
        },
        ..UnitState::default()
    };
    let units = (0..c.o.units)
        .map(|u| NewUnit::new("M-113", at(0.0, u as f64 * 20.0)))
        .collect();
    let group = c
        .store
        .create_group(NewGroup {
            theater: theater.clone(),
            country: 2,
            category: 2,
            coalition: 2,
            units,
            authority: Authority::Sim,
            label: "names group".into(),
            extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
        })
        .map_err(|e| format!("store: {e:?}"))?;
    let mut hangar = NewUnit::new("Hangar A", at(400.0, 0.0));
    hangar.extra = lv::obj(vec![("category", lv::s("Fortifications"))]);
    let stat = c
        .store
        .create_static(NewGroup {
            theater,
            country: 2,
            category: 0,
            coalition: 2,
            units: vec![hangar],
            authority: Authority::Sim,
            label: "names static".into(),
            extra: Lv::Nil,
        })
        .map_err(|e| format!("store: {e:?}"))?;
    let campaign = CampaignId(c.o.campaign);
    let gname = name(NameKind::Group, campaign, group);
    // Platform name -> (entity, group name the OBJECT must carry).
    let mut expected: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for m in &c.store.group(group).expect("group").members {
        expected.insert(name(NameKind::Unit, campaign, *m), (m.0, gname.clone()));
    }
    let sname = name(NameKind::Static, campaign, stat);
    expected.insert(sname.clone(), (stat.0, String::new()));
    let both_live = |c: &Ctx| {
        [group, stat]
            .iter()
            .all(|i| c.store.group(*i).is_some_and(|g| g.state == State::Live))
    };
    let seen_all = |objects: &[Object]| {
        let seen: BTreeSet<&str> = objects.iter().map(|o| o.name.as_str()).collect();
        expected.keys().all(|n| seen.contains(n.as_str()))
    };
    c.until("both live and every OBJECT", |c| {
        both_live(c) && seen_all(&c.log.objects)
    })?;
    // What DCS reported under the platform's names, first and after the restart.
    let judge = |objects: &[Object]| -> (bool, Vec<String>) {
        let mut ok = true;
        let mut seen = Vec::new();
        for (n, (entity, gn)) in &expected {
            let found: Vec<&Object> = objects.iter().filter(|o| &o.name == n).collect();
            let good = !found.is_empty()
                && found
                    .iter()
                    .all(|o| o.entity == *entity && &o.group_name == gn);
            ok &= good;
            if let Some(o) = found.last() {
                seen.push(format!(
                    "{}|{}|{}|{}",
                    o.name, o.group_name, o.entity, o.group_entity
                ));
            }
        }
        (ok, seen)
    };
    let (first_ok, first) = judge(&c.log.objects);
    println!(
        "{{\"ev\":\"phase\",\"name\":\"spawned\",\"group\":{},\"static\":{},\"objects\":{}}}",
        json_string(&gname),
        json_string(&sname),
        str_list(&first)
    );
    // With a control folder, the caller reads DCS while the objects exist.
    if c.o.control_dir.is_some() {
        wait_file(c, "checked", true)?;
    }
    // The sidecar restarts: a new link session and a reconciler that knows
    // only the store.
    let spawns = |c: &Ctx, from: usize| {
        c.log.results[from..]
            .iter()
            .filter(|r| {
                matches!(
                    u64::from(r.result.opcode),
                    opcode::GROUP_SPAWN | opcode::STATIC_SPAWN
                )
            })
            .count()
    };
    let last = c.sim.last_numbers();
    let old = std::mem::replace(&mut c.sim, LinkSim::start(c.cfg.clone(), last));
    drop(old);
    c.rec = Reconciler::new(
        RecConfig {
            materialize_per_frame: c.o.per_step.max(1),
            ..RecConfig::default()
        },
        &c.store,
    );
    let results_at = c.log.results.len();
    let enums_at = c.log.enums.len();
    c.wait_ready()?;
    c.until("a full enumeration after the restart", |c| {
        c.log.enums[enums_at..].iter().any(|e| e.ended)
    })?;
    let run = c.log.enums[enums_at..]
        .iter()
        .find(|e| e.ended)
        .expect("checked")
        .objects
        .clone();
    let (again_ok, again) = judge(&run);
    let live_after = both_live(c);
    let respawned = spawns(c, results_at);
    let foreign = c.rec.counters().foreign;
    println!(
        "{{\"ev\":\"phase\",\"name\":\"rebound\",\"objects\":{},\"live\":{live_after},\
         \"spawns\":{respawned},\"foreign\":{foreign}}}",
        str_list(&again)
    );
    let objects: BTreeSet<u64> = c
        .log
        .objects
        .iter()
        .filter(|o| expected.contains_key(&o.name))
        .map(|o| o.object)
        .collect();
    for id in [group, stat] {
        c.store
            .remove_entity(id)
            .map_err(|e| format!("store: {e:?}"))?;
    }
    c.until("every OBJECT_GONE after the removal", |c| {
        let gone: BTreeSet<u64> = c
            .log
            .gone
            .iter()
            .filter(|g| g.reason == gone_reason::DESTROYED)
            .map(|g| g.object)
            .collect();
        objects.iter().all(|o| gone.contains(o))
    })?;
    let tag = nanoprobe_sidecar::ids::tag(campaign);
    let forms = expected
        .keys()
        .all(|n| nanoprobe_sidecar::ids::parse(n).is_some_and(|p| p.tag == tag))
        && nanoprobe_sidecar::ids::parse(&gname).is_some_and(|p| p.tag == tag);
    let pass = forms && first_ok && again_ok && live_after && respawned == 0 && foreign == 0;
    println!(
        "{{\"ev\":\"verdict\",\"pass\":{pass},\"scenario\":\"names\",\"tag\":{},\
         \"forms\":{forms},\"bound\":{first_ok},\"rebound\":{again_ok},\"live_after\":{live_after},\
         \"spawns_after_restart\":{respawned},\"foreign\":{foreign},\"removed\":{}}}",
        json_string(tag.as_str()),
        objects.len()
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------
// Part A.

fn groups(c: &mut Ctx) -> Result<bool, String> {
    c.wait_ready()?;
    let theater = c
        .store
        .loaded()
        .map(|l| l.theater.clone())
        .unwrap_or_default();
    let counter0 = c.counter()?;
    let pumps0 = c.pumps_ge_256()?;
    let side = (c.o.groups as f64).sqrt().ceil() as usize;
    let mut ids = Vec::new();
    let mut expected: BTreeMap<String, EntityId> = BTreeMap::new();
    for n in 0..c.o.groups {
        let (row, col) = (n / side, n % side);
        let units = (0..c.o.units)
            .map(|u| {
                NewUnit::new(
                    "M-113",
                    UnitState {
                        at: Point {
                            x: c.o.origin.0 + row as f64 * c.o.spacing,
                            y: 0.0,
                            z: c.o.origin.1 + col as f64 * c.o.spacing + u as f64 * 20.0,
                        },
                        ..UnitState::default()
                    },
                )
            })
            .collect();
        let id = c
            .store
            .create_group(NewGroup {
                theater: theater.clone(),
                country: 2,
                category: 2,
                coalition: 2,
                units,
                authority: Authority::Sim,
                label: format!("s15 {n}"),
                extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
            })
            .map_err(|e| format!("store: {e:?}"))?;
        ids.push(id);
        for m in &c.store.group(id).expect("group").members {
            expected.insert(unit_name(c, *m), *m);
        }
    }
    println!(
        "{{\"ev\":\"phase\",\"name\":\"created\",\"groups\":{},\"units\":{}}}",
        ids.len(),
        expected.len()
    );
    let live = |c: &Ctx| {
        ids.iter()
            .all(|i| c.store.group(*i).is_some_and(|g| g.state == State::Live))
    };
    let arrived = |c: &Ctx| {
        let seen: BTreeSet<&str> = c.log.objects.iter().map(|o| o.name.as_str()).collect();
        expected.keys().all(|n| seen.contains(n.as_str()))
    };
    c.until("every group live and every unit's OBJECT", |c| {
        live(c) && arrived(c)
    })?;
    let spawned_results = c.log.results.len();
    println!(
        "{{\"ev\":\"phase\",\"name\":\"spawned\",\"results\":{spawned_results},\"frame\":{}}}",
        c.log.frame
    );
    let damage = damage_units(c, &ids)?;
    println!(
        "{{\"ev\":\"phase\",\"name\":\"damaged\",\"sent\":{},\"applied\":{},\
         \"read_back\":{},\"frame\":{}}}",
        damage.sent, damage.applied, damage.read_back, c.log.frame
    );
    for id in &ids {
        c.store
            .remove_entity(*id)
            .map_err(|e| format!("store: {e:?}"))?;
    }
    let gone_destroyed = |c: &Ctx| {
        c.log
            .gone
            .iter()
            .filter(|g| g.reason == gone_reason::DESTROYED)
            .map(|g| g.object)
            .collect::<BTreeSet<u64>>()
    };
    let units_total = expected.len();
    c.until("every group virtual and every unit's OBJECT_GONE", |c| {
        ids.iter()
            .all(|i| c.store.group(*i).is_some_and(|g| g.state == State::Virtual))
            && gone_destroyed(c).len() >= units_total
    })?;
    println!(
        "{{\"ev\":\"phase\",\"name\":\"removed\",\"results\":{},\"frame\":{}}}",
        c.log.results.len(),
        c.log.frame
    );
    // The agent holds a destroy until the object is gone: wait for a report
    // after the removal that shows none pending. A report that does not come
    // in time is a failure of the verdict, not an error of the run.
    let reports_at = c.log.sampling.len();
    let drained = c.until_for("the agent's pending destroys to drain", DRAIN_S, |c| {
        c.log
            .sampling
            .iter()
            .skip(reports_at)
            .any(|r| r.destroy == 0)
    })?;
    // The agent stores its counters at the end of the frame: read two frames
    // past the last result.
    let last_frame = c.log.results.last().map_or(c.log.frame, |r| r.frame);
    c.until("two frames after the last result", |c| {
        c.log.frame >= last_frame + 2
    })?;
    let counter = c.counter()?;
    let pumps = c.pumps_ge_256()?;
    Ok(groups_verdict(
        c,
        &ids,
        &expected,
        counter.saturating_sub(counter0),
        pumps.saturating_sub(pumps0),
        &damage,
        drained,
    ))
}

/// Send `DAMAGE` 50 % to the first unit of every group, wait for every
/// `RESULT` and for every unit's life to read about half its `life0`.
fn damage_units(c: &mut Ctx, groups: &[EntityId]) -> Result<DamageReport, String> {
    let units: Vec<EntityId> = groups
        .iter()
        .filter_map(|g| c.store.group(*g).and_then(|g| g.members.first().copied()))
        .collect();
    let mut keys: Vec<(u64, u64)> = Vec::new();
    while keys.len() < units.len() {
        c.check_deadline("the damage sends")?;
        for u in units.iter().skip(keys.len()).take(c.o.per_step.max(1)) {
            let mut tap = Tap {
                sim: &mut c.sim,
                log: &mut c.log,
            };
            match c.rec.damage(&mut c.store, &mut tap, *u, 50.0) {
                Some(key) => keys.push(key),
                None => break,
            }
        }
        c.step();
        c.progress("the damage sends");
    }
    let mut rep = DamageReport {
        sent: keys.len(),
        ..DamageReport::default()
    };
    c.until("every DAMAGE answered", |c| {
        keys.iter().all(|k| c.rec.effect_result(*k).is_some())
    })?;
    rep.applied = keys
        .iter()
        .filter(|k| {
            c.rec
                .effect_result(**k)
                .is_some_and(|r| r.outcome == Outcome::Applied)
        })
        .count();
    // The life, from a row after the damage: before it the row says life0.
    let pairs: Vec<(String, EntityId)> = units.iter().map(|u| (unit_name(c, *u), *u)).collect();
    let half = |c: &Ctx, name: &str| {
        c.log
            .objects
            .iter()
            .rev()
            .find(|o| o.name == name)
            .and_then(|o| c.log.vitals.get(&o.object).map(|l| about_half(*l, o.life0)))
            .unwrap_or(false)
    };
    c.until_for("every damaged unit's VITALS row", LIFE_S, |c| {
        pairs.iter().all(|(n, _)| half(c, n))
    })?;
    rep.read_back = pairs.iter().filter(|(n, _)| half(c, n)).count();
    rep.costs = c
        .log
        .results
        .iter()
        .filter(|st| keys.contains(&(st.result.epoch, st.result.seq)))
        .map(|st| st.result.cost_us)
        .collect();
    let budget_us = (c.o.commanded_ms * 1000.0).round() as u64;
    let over: BTreeSet<u64> = frame_costs(&c.log.results)
        .into_iter()
        .filter(|f| f.total_us > budget_us)
        .map(|f| f.frame)
        .collect();
    for st in &c.log.results {
        if keys.contains(&(st.result.epoch, st.result.seq)) {
            rep.over_budget += usize::from(u64::from(st.result.cost_us) > budget_us);
            rep.in_over_frames += usize::from(over.contains(&st.frame));
        }
    }
    Ok(rep)
}

fn groups_verdict(
    c: &Ctx,
    ids: &[EntityId],
    expected: &BTreeMap<String, EntityId>,
    counter_delta: u64,
    pumps_ge_256: u64,
    damage: &DamageReport,
    drained: bool,
) -> bool {
    let budget_us = (c.o.commanded_ms * 1000.0).round() as u64;
    let rep = budget_report(&c.log.results, budget_us);
    if c.o.trace {
        for f in frame_costs(&c.log.results) {
            println!(
                "{{\"ev\":\"frame\",\"frame\":{},\"commands\":{},\"commanded_us\":{},\
                 \"over_budget\":{}}}",
                f.frame,
                f.commands,
                f.total_us,
                f.total_us > budget_us
            );
        }
        for st in &c.log.results {
            let r = &st.result;
            println!(
                "{{\"ev\":\"result\",\"frame\":{},\"seq\":{},\"opcode\":{},\"code\":{},\
                 \"cost_us\":{},\"entity\":{},\"object\":{}}}",
                st.frame, r.seq, r.opcode, r.code, r.cost_us, r.entity, r.object
            );
        }
    }
    let by_frame = frame_costs(&c.log.results);
    for f in by_frame.iter().filter(|f| f.total_us > budget_us) {
        println!(
            "{{\"ev\":\"over_budget_frame\",\"frame\":{},\"commands\":{},\"commanded_us\":{},\
             \"crossing_seq\":{},\"crossing_opcode\":{},\"crossing_cost_us\":{},\
             \"attributed\":{}}}",
            f.frame,
            f.commands,
            f.total_us,
            f.last.0,
            f.last.1,
            f.last.2,
            f.before_last_us <= budget_us
        );
    }
    let spawn = nanoprobe_transport::layout::opcode::GROUP_SPAWN;
    let applied = result::APPLIED;
    let mut spawns: BTreeMap<u64, u32> = BTreeMap::new();
    for st in &c.log.results {
        let r = &st.result;
        if u64::from(r.opcode) == spawn && u64::from(r.code) == applied {
            *spawns.entry(r.entity).or_default() += 1;
        }
    }
    let spawned = ids.iter().filter(|i| spawns.contains_key(&i.0)).count();
    let duplicate_spawns = spawns.values().filter(|n| **n > 1).count();
    let mut by_name: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    for o in &c.log.objects {
        by_name.entry(o.name.as_str()).or_default().insert(o.object);
    }
    let units_seen = expected
        .keys()
        .filter(|n| by_name.contains_key(n.as_str()))
        .count();
    let duplicate_objects = expected
        .keys()
        .filter(|n| by_name.get(n.as_str()).is_some_and(|s| s.len() > 1))
        .count();
    let entity_mismatch = c
        .log
        .objects
        .iter()
        .filter(|o| expected.get(&o.name).is_some_and(|e| e.0 != o.entity))
        .count();
    let mut gone_counts: BTreeMap<u64, u32> = BTreeMap::new();
    for g in &c.log.gone {
        if g.reason == gone_reason::DESTROYED {
            *gone_counts.entry(g.object).or_default() += 1;
        }
    }
    let all_objects: BTreeSet<u64> = by_name
        .iter()
        .filter(|(n, _)| expected.contains_key(**n))
        .flat_map(|(_, s)| s.iter().copied())
        .collect();
    let units_gone = all_objects
        .iter()
        .filter(|o| gone_counts.contains_key(o))
        .count();
    let duplicate_gone = gone_counts.values().filter(|n| **n > 1).count();
    let removed = ids
        .iter()
        .filter(|i| {
            c.store
                .group(**i)
                .is_some_and(|g| g.removed && g.state == State::Virtual)
        })
        .count();
    let bad_results = c
        .log
        .results
        .iter()
        .filter(|st| {
            st.result.seq != 0
                && u64::from(st.result.code) != applied
                && u64::from(st.result.code) != result::AT_REVISION
        })
        .count();
    let counter_ok = rep.counter_fits(counter_delta);
    let (cost_median, cost_p99, cost_max) = cost_stats(&damage.costs);
    let damage_ok = damage.sent == ids.len()
        && damage.applied == damage.sent
        && damage.read_back == damage.sent;
    let max_of = |f: fn(&Sampling) -> u64| c.log.sampling.iter().map(f).max();
    let pending_expired = max_of(|x| x.expired);
    let pending_overflow = max_of(|x| x.overflow);
    let pending_destroy = c.log.sampling.last().map(|x| x.destroy);
    // No report at all fails: the counters were never seen.
    let pending_ok = drained && pending_expired == Some(0) && pending_overflow == Some(0);
    let pass = spawned == ids.len()
        && removed == ids.len()
        && duplicate_spawns == 0
        && units_seen == expected.len()
        && duplicate_objects == 0
        && entity_mismatch == 0
        && all_objects.len() == expected.len()
        && units_gone == expected.len()
        && duplicate_gone == 0
        && bad_results == 0
        && rep.unattributed.is_empty()
        && damage_ok
        && pending_ok
        && counter_ok;
    let opt = |v: Option<u64>| v.map_or("null".to_owned(), |n| n.to_string());
    println!(
        "{{\"ev\":\"verdict\",\"pass\":{pass},\"scenario\":\"groups\",\"groups\":{},\
         \"units\":{},\"spawned\":{spawned},\"removed\":{removed},\"units_seen\":{units_seen},\
         \"units_gone\":{units_gone},\"duplicate_spawns\":{duplicate_spawns},\
         \"duplicate_objects\":{duplicate_objects},\"duplicate_gone\":{duplicate_gone},\
         \"entity_mismatch\":{entity_mismatch},\"bad_results\":{bad_results},\
         \"budget_us\":{},\"results\":{},\"frames_with_commands\":{},\
         \"max_commands_in_frame\":{},\"max_frame_commanded_us\":{},\
         \"over_budget_results_above\":{},\"over_budget_results_at_or_above\":{},\
         \"counter_delta\":{counter_delta},\"counter_ok\":{counter_ok},\
         \"over_budget_frames\":{},\"unattributed_frames\":{},\
         \"pumps_ge_256us\":{pumps_ge_256},\"frames_read\":{},\
         \"damage_sent\":{},\"damage_applied\":{},\"damage_read_back\":{},\
         \"damage_ok\":{damage_ok},\"damage_cost_us_median\":{cost_median},\
         \"damage_cost_us_p99\":{cost_p99},\"damage_cost_us_max\":{cost_max},\
         \"damage_over_budget\":{},\"damage_in_over_budget_frames\":{},\
         \"drained\":{drained},\"pending_destroy\":{},\"pending_expired\":{},\
         \"pending_overflow\":{},\"pending_ok\":{pending_ok}}}",
        ids.len(),
        expected.len(),
        rep.budget_us,
        rep.results,
        rep.frames_with_commands,
        rep.max_commands_in_frame,
        rep.max_frame_us,
        rep.over_above,
        rep.over_at_or_above,
        rep.over_frames.len(),
        json_string(&format!("{:?}", rep.unattributed)),
        c.log.frames_read,
        damage.sent,
        damage.applied,
        damage.read_back,
        damage.over_budget,
        damage.in_over_frames,
        opt(pending_destroy),
        opt(pending_expired),
        opt(pending_overflow),
    );
    pass
}

// ---------------------------------------------------------------------------
// Part B.

/// Create a group of `M-113`s in a row along z from `(x, z)`.
fn row_group(
    c: &mut Ctx,
    theater: &str,
    x: f64,
    z: f64,
    label: String,
) -> Result<EntityId, String> {
    let units = (0..c.o.units)
        .map(|u| {
            NewUnit::new(
                "M-113",
                UnitState {
                    at: Point {
                        x,
                        y: 0.0,
                        z: z + u as f64 * 20.0,
                    },
                    ..UnitState::default()
                },
            )
        })
        .collect();
    c.store
        .create_group(NewGroup {
            theater: theater.into(),
            country: 2,
            category: 2,
            coalition: 2,
            units,
            authority: Authority::Sim,
            label,
            extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
        })
        .map_err(|e| format!("store: {e:?}"))
}

fn debris(c: &mut Ctx) -> Result<bool, String> {
    c.wait_ready()?;
    let theater = c
        .store
        .loaded()
        .map(|l| l.theater.clone())
        .unwrap_or_default();
    let side = (c.o.groups as f64).sqrt().ceil() as usize;
    let (ox, oz) = c.o.origin;
    let mut grid = Vec::new();
    for n in 0..c.o.groups {
        let (row, col) = (n / side, n % side);
        let at = (ox + row as f64 * c.o.spacing, oz + col as f64 * c.o.spacing);
        grid.push((
            row_group(c, &theater, at.0, at.1, format!("debris {n}"))?,
            at,
        ));
    }
    let control = row_group(c, &theater, ox - 1_500.0, oz, "debris control".into())?;
    let members = |c: &Ctx, g: EntityId| c.store.group(g).expect("group").members.clone();
    let names_of = |c: &Ctx, gs: &[EntityId]| -> Vec<String> {
        gs.iter()
            .flat_map(|g| members(c, *g))
            .map(|m| unit_name(c, m))
            .collect()
    };
    let grid_ids: Vec<EntityId> = grid.iter().map(|(g, _)| *g).collect();
    let kill = names_of(c, &grid_ids);
    let kept = names_of(c, &[control]);
    let all: Vec<String> = kill.iter().chain(&kept).cloned().collect();
    c.until("every group live and every unit's OBJECT", |c| {
        grid_ids
            .iter()
            .chain([&control])
            .all(|g| c.store.group(*g).is_some_and(|x| x.state == State::Live))
            && all
                .iter()
                .all(|n| c.log.objects.iter().any(|o| &o.name == n))
    })?;
    let kept_objects: BTreeSet<u64> = c
        .log
        .objects
        .iter()
        .filter(|o| kept.contains(&o.name))
        .map(|o| o.object)
        .collect();
    println!(
        "{{\"ev\":\"phase\",\"name\":\"spawned\",\"kill\":{},\"control\":{}}}",
        str_list(kill.iter()),
        str_list(kept.iter())
    );
    wait_file(c, "go", true)?;
    // The caller writes the ground height at the first group into `go`.
    let ground: f64 = std::fs::read_to_string(control_file(c, "go"))
        .map_err(|e| format!("go: {e}"))?
        .trim()
        .parse()
        .map_err(|_| "go holds no ground height".to_owned())?;
    let counter0 = c.counter()?;
    let results0 = c.log.results.len();
    let ((x0, z0), (x1, z1)) = (grid[0].1, grid[1.min(grid.len() - 1)].1);
    let len = (c.o.units.max(1) - 1) as f64 * 20.0;
    let sphere = Debris::Sphere {
        center: [x0, ground, z0 + len / 2.0],
        radius: len / 2.0 + c.o.spacing / 3.0,
    };
    let boxed = Debris::Box {
        min: [x1 - 30.0, -1_000.0, z1 - 30.0],
        max: [x1 + 30.0, 9_000.0, z1 + len + 30.0],
    };
    let mut keys = Vec::new();
    for (what, v) in [
        ("sphere", Some(sphere)),
        ("box", Some(boxed)),
        ("whole map", None),
    ] {
        let mut tap = Tap {
            sim: &mut c.sim,
            log: &mut c.log,
        };
        let key = match v {
            Some(v) => c.rec.clear_debris(&mut c.store, &mut tap, v),
            None => c.rec.clear_debris_everywhere(&mut c.store, &mut tap),
        };
        keys.push((
            what,
            key.ok_or_else(|| format!("the {what} clear was not sent"))?,
        ));
        c.step();
    }
    c.until("every clear answered", |c| {
        keys.iter().all(|(_, k)| c.rec.effect_result(*k).is_some())
    })?;
    let last_frame = c.log.results.last().map_or(c.log.frame, |r| r.frame);
    c.until("two frames after the last result", |c| {
        c.log.frame >= last_frame + 2
    })?;
    let counter = c.counter()?;
    let budget_us = (c.o.commanded_ms * 1000.0).round() as u64;
    let run_results = &c.log.results[results0..];
    let report = budget_report(run_results, budget_us);
    // The whole map's own tiles: after the box's command, up to its last tile.
    let (box_key, map_key) = (keys[1].1, keys[2].1);
    let tiles: Vec<&Stamped> = run_results
        .iter()
        .filter(|st| {
            st.result.opcode == opcode::CLEAR_DEBRIS
                && st.result.epoch == map_key.0
                && st.result.seq > box_key.1
                && st.result.seq <= map_key.1
        })
        .collect();
    let tile_costs: Vec<u32> = tiles.iter().map(|st| st.result.cost_us).collect();
    let (median, p99, max) = cost_stats(&tile_costs);
    let frames: BTreeSet<u64> = tiles.iter().map(|st| st.frame).collect();
    let mut clears = Vec::new();
    let mut pass = true;
    for (what, k) in &keys {
        let r = c.rec.effect_result(*k).expect("answered");
        let (removed, cost) = r.result.as_ref().map_or((-1.0, 0), |x| {
            let n = match x.value {
                Lv::Number(n) => n,
                _ => -1.0,
            };
            (n, x.cost_us)
        });
        let applied = r.outcome == Outcome::Applied;
        // Each wreck counts once at least: the sphere and the box hold one
        // group each, and the whole map the rest.
        let groups = if *what == "whole map" {
            c.o.groups.saturating_sub(2)
        } else {
            1
        };
        pass &= applied && removed >= (groups * c.o.units) as f64;
        clears.push(format!(
            "{{\"clear\":{},\"applied\":{applied},\"removed\":{removed},\"cost_us\":{cost}}}",
            json_string(what)
        ));
    }
    let control_gone: Vec<u64> = c
        .log
        .gone
        .iter()
        .filter(|g| kept_objects.contains(&g.object))
        .map(|g| g.object)
        .collect();
    let control_live = c
        .store
        .group(control)
        .is_some_and(|g| g.state == State::Live);
    let counter_ok = report.counter_fits(counter.saturating_sub(counter0));
    pass &= control_gone.is_empty()
        && control_live
        && report.unattributed.is_empty()
        && counter_ok
        && frames.len() > 1;
    println!(
        "{{\"ev\":\"verdict\",\"scenario\":\"debris\",\"pass\":{pass},\"clears\":[{}],\
         \"tiles\":{},\"tile_cost_us\":{{\"median\":{median},\"p99\":{p99},\"max\":{max}}},\
         \"tile_frames\":{},\"control_live\":{control_live},\"control_gone\":{},\
         \"budget_us\":{budget_us},\"over_frames\":{},\"unattributed_frames\":{},\
         \"max_frame_us\":{},\"max_commands_in_frame\":{},\"counter_delta\":{},\
         \"counter_ok\":{}}}",
        clears.join(","),
        tiles.len(),
        frames.len(),
        id_list(control_gone),
        report.over_frames.len(),
        id_list(report.unattributed.iter().copied()),
        report.max_frame_us,
        report.max_commands_in_frame,
        counter.saturating_sub(counter0),
        counter_ok,
    );
    Ok(pass)
}

fn control_file(c: &Ctx, name: &str) -> PathBuf {
    c.o.control_dir.clone().expect("checked").join(name)
}

/// Wait for a file, stepping the reconciler, or reading only when it has
/// been stopped.
fn wait_file(c: &mut Ctx, file: &str, stepping: bool) -> Result<(), String> {
    let path = control_file(c, file);
    let until = Instant::now() + Duration::from_secs(300);
    while !Path::new(&path).exists() {
        if Instant::now() > until {
            return Err(format!("no {file} file in 300 s"));
        }
        c.check_deadline(file)?;
        if stepping {
            c.step();
        } else {
            c.read();
        }
        c.progress(file);
    }
    Ok(())
}

fn id_list(v: impl IntoIterator<Item = u64>) -> String {
    let parts: Vec<String> = v.into_iter().map(|n| n.to_string()).collect();
    format!("[{}]", parts.join(","))
}

fn str_list<'a>(v: impl IntoIterator<Item = &'a String>) -> String {
    let parts: Vec<String> = v.into_iter().map(|n| json_string(n)).collect();
    format!("[{}]", parts.join(","))
}

fn paused(c: &mut Ctx) -> Result<bool, String> {
    c.wait_ready()?;
    let theater = c
        .store
        .loaded()
        .map(|l| l.theater.clone())
        .unwrap_or_default();
    let units = (0..c.o.units)
        .map(|u| {
            NewUnit::new(
                "M-113",
                UnitState {
                    at: Point {
                        x: c.o.origin.0,
                        y: 0.0,
                        z: c.o.origin.1 + u as f64 * 20.0,
                    },
                    ..UnitState::default()
                },
            )
        })
        .collect();
    let g = c
        .store
        .create_group(NewGroup {
            theater,
            country: 2,
            category: 2,
            coalition: 2,
            units,
            authority: Authority::Sim,
            label: "s15 replace".into(),
            extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
        })
        .map_err(|e| format!("store: {e:?}"))?;
    let group_name = name(NameKind::Group, CampaignId(c.o.campaign), g);
    let old_names: Vec<String> = c
        .store
        .group(g)
        .expect("group")
        .members
        .iter()
        .map(|m| unit_name(c, *m))
        .collect();
    c.until("G live, its units and nothing in flight", |c| {
        c.store.group(g).is_some_and(|x| x.state == State::Live)
            && old_names
                .iter()
                .all(|n| c.log.objects.iter().any(|o| &o.name == n))
            && c.rec.inflight().next().is_none()
            && c.sim.queued() == 0
    })?;
    let old_objects: BTreeMap<String, u64> = c
        .log
        .objects
        .iter()
        .filter(|o| old_names.contains(&o.name))
        .map(|o| (o.name.clone(), o.object))
        .collect();
    println!(
        "{{\"ev\":\"phase\",\"name\":\"spawned\",\"group\":{},\"units\":{},\"objects\":{}}}",
        json_string(&group_name),
        str_list(old_names.iter()),
        id_list(old_objects.values().copied())
    );
    wait_file(c, "paused", true)?;
    // Frames after the file must say paused.
    let at = c.log.frames_read;
    c.until("a paused frame", |c| {
        c.log.frames_read > at + 2 && u64::from(c.log.lifecycle) == lifecycle::PAUSED
    })
    .map_err(|e| format!("the simulation is not paused: {e}"))?;
    // The group's spec changes in the store, as PutGroup's replacement does:
    // each unit's type. The reconciler sends the replacing spawn.
    let loaded = c.store.loaded().cloned().expect("ready");
    let command_of = |c: &Ctx, op: u64| {
        c.store
            .entries()
            .iter()
            .filter_map(|e| match e {
                Entry::Command(j) if u64::from(j.opcode) == op && j.entity == g => {
                    Some((j.epoch, j.seq, j.rev))
                }
                _ => None,
            })
            .max()
    };
    let first = command_of(c, nanoprobe_transport::layout::opcode::GROUP_SPAWN);
    let members = c.store.group(g).expect("group").members.clone();
    for m in &members {
        c.store
            .set_unit_type(*m, "BTR-80")
            .map_err(|e| format!("store: {e:?}"))?;
    }
    let new_names = old_names.clone();
    let (epoch, seq, spec_rev) = loop {
        c.check_deadline("the replacing spawn")?;
        c.step();
        if let Some(x) = command_of(c, nanoprobe_transport::layout::opcode::GROUP_SPAWN)
            && Some(x) != first
        {
            break x;
        }
    };
    let got = |c: &Ctx| {
        c.log
            .results
            .iter()
            .find(|st| st.result.epoch == epoch && st.result.seq == seq)
            .map(|st| st.result.clone())
    };
    while got(c).is_none() {
        c.check_deadline("the replace's RESULT")?;
        c.step();
    }
    let _ = spec_rev;
    // The reconciler stops here: only the probe's own commands go out.
    let cleanup_seq = c
        .store
        .entries()
        .iter()
        .filter_map(|e| match e {
            Entry::Command(j) => Some(j.seq),
            _ => None,
        })
        .max()
        .unwrap_or(seq)
        + 1;
    // The new units' births come while paused (LIVE under Wine); give them
    // a moment, and do not require them yet.
    let born = |c: &Ctx, n: &String| {
        c.log
            .objects
            .iter()
            .any(|o| &o.name == n && !old_objects.values().any(|x| *x == o.object))
    };
    let wait = Instant::now() + Duration::from_secs(5);
    while Instant::now() < wait && !new_names.iter().all(|n| born(c, n)) {
        c.read();
    }
    let replace = got(c).expect("checked");
    // The new units' births come while paused (LIVE under Wine); give them
    // a moment, and do not require them yet.
    let wait = Instant::now() + Duration::from_secs(5);
    while Instant::now() < wait
        && !new_names
            .iter()
            .all(|n| c.log.objects.iter().any(|o| &o.name == n))
    {
        c.read();
    }
    let gone_paused = old_objects
        .values()
        .filter(|o| {
            c.log
                .gone
                .iter()
                .any(|g| g.object == **o && g.reason == gone_reason::DESTROYED)
        })
        .count();
    let births_paused = new_names.iter().filter(|n| born(c, n)).count();
    println!(
        "{{\"ev\":\"phase\",\"name\":\"replaced\",\"code\":{},\"message\":{},\"object\":{},\
         \"cost_us\":{},\"old_gone_destroyed\":{gone_paused},\"new_births\":{births_paused},\
         \"new_units\":{},\"lifecycle\":{}}}",
        replace.code,
        json_string(&replace.message),
        replace.object,
        replace.cost_us,
        str_list(new_names.iter()),
        c.log.lifecycle
    );
    wait_file(c, "resumed", false)?;
    // `settle` seconds of model time after the frames say running.
    let mut start_model = None;
    loop {
        c.check_deadline("model time after the resume")?;
        c.read();
        if u64::from(c.log.lifecycle) == lifecycle::RUNNING {
            let t0 = *start_model.get_or_insert(c.log.model_time);
            if c.log.model_time - t0 >= c.o.settle {
                break;
            }
        }
        c.progress("model time after the resume");
    }
    let runs_before = c.log.enums.len();
    let (op, payload) = Command {
        header: CommandHeader::default(),
        body: Body::Enumerate,
    }
    .encode();
    c.sim
        .send(op, &payload)
        .map_err(|_| "the link is not attached".to_string())?;
    c.sim.flush();
    while !c.log.enums.iter().skip(runs_before).any(|e| e.ended) {
        c.check_deadline("the enumeration")?;
        c.read();
    }
    let run = c
        .log
        .enums
        .iter()
        .skip(runs_before)
        .find(|e| e.ended)
        .cloned()
        .expect("checked");
    let under_g: BTreeSet<String> = run
        .objects
        .iter()
        .filter(|o| o.group_name == group_name)
        .map(|o| o.name.clone())
        .collect();
    let want: BTreeSet<String> = new_names.iter().cloned().collect();
    let old_ids: BTreeSet<u64> = old_objects.values().copied().collect();
    let old_in_world = run
        .objects
        .iter()
        .filter(|o| old_ids.contains(&o.object))
        .count();
    let groups_under_name = run
        .objects
        .iter()
        .filter(|o| o.group_name == group_name)
        .map(|o| o.group)
        .collect::<BTreeSet<u64>>();
    let mut gone_counts: BTreeMap<u64, u32> = BTreeMap::new();
    for gone in &c.log.gone {
        if gone.reason == gone_reason::DESTROYED {
            *gone_counts.entry(gone.object).or_default() += 1;
        }
    }
    let old_gone_once = old_ids
        .iter()
        .filter(|o| gone_counts.get(o) == Some(&1))
        .count();
    let old_gone_dead = c
        .log
        .gone
        .iter()
        .filter(|x| old_ids.contains(&x.object) && x.reason == gone_reason::DEAD)
        .count();
    println!(
        "{{\"ev\":\"phase\",\"name\":\"settled\",\"group\":{},\"enumeration\":{},\
         \"rows\":{},\"under_group\":{},\"old_in_world\":{old_in_world},\
         \"group_ids_under_name\":{}}}",
        json_string(&group_name),
        run.id,
        run.objects.len(),
        str_list(under_g.iter()),
        id_list(groups_under_name.iter().copied())
    );
    wait_file(c, "checked", false)?;
    // Leave nothing behind: destroy G.
    let (op, payload) = Command {
        header: CommandHeader {
            seq: cleanup_seq,
            epoch,
            campaign: c.o.campaign,
            entity: g.0,
            rev: 0,
            mission_gen: loaded.mission_gen,
        },
        body: Body::GroupDestroy,
    }
    .encode();
    c.sim
        .send(op, &payload)
        .map_err(|_| "the link is not attached".to_string())?;
    c.sim.flush();
    while !c
        .log
        .results
        .iter()
        .any(|st| st.result.epoch == epoch && st.result.seq == cleanup_seq)
    {
        c.check_deadline("the cleanup RESULT")?;
        c.read();
    }
    let new_gone = new_names
        .iter()
        .filter(|n| {
            c.log.objects.iter().filter(|o| &o.name == *n).any(|o| {
                c.log
                    .gone
                    .iter()
                    .any(|g| g.object == o.object && g.reason == gone_reason::DESTROYED)
            })
        })
        .count();
    let applied = u64::from(replace.code) == result::APPLIED;
    let pass = applied
        && replace.object != 0
        && under_g == want
        && groups_under_name.len() == 1
        && old_in_world == 0
        && old_objects.len() == old_names.len()
        && old_gone_once == old_names.len()
        && old_gone_dead == 0
        && new_names.iter().all(|n| born(c, n));
    println!(
        "{{\"ev\":\"verdict\",\"pass\":{pass},\"scenario\":\"paused\",\"applied\":{applied},\
         \"new_group_object\":{},\"old_units\":{},\"old_gone_destroyed_once\":{old_gone_once},\
         \"old_gone_dead\":{old_gone_dead},\"old_gone_while_paused\":{gone_paused},\
         \"new_births_while_paused\":{births_paused},\"new_units\":{},\
         \"enumerated_under_group\":{},\"groups_under_name\":{},\
         \"old_in_world\":{old_in_world},\"cleanup_gone\":{new_gone},\"epoch\":{epoch},\
         \"spec_rev\":{spec_rev},\"frames_read\":{}}}",
        replace.object,
        old_names.len(),
        new_names.len(),
        under_g.len(),
        groups_under_name.len(),
        c.log.frames_read
    );
    Ok(pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(frame: u64, seq: u64, cost_us: u32) -> Stamped {
        Stamped {
            frame,
            result: CommandResult {
                seq,
                epoch: 1,
                campaign: 7,
                entity: 1,
                rev: 1,
                object: 0,
                cost_us,
                opcode: 16,
                code: 0,
                message: String::new(),
                value: nanoprobe_transport::codec::Lv::Nil,
            },
        }
    }

    #[test]
    fn a_frame_one_command_carried_over_is_attributed() {
        // Budget 100: 60 and 60 is over by the second command alone.
        let r = [st(1, 1, 60), st(1, 2, 60), st(2, 3, 30)];
        let rep = budget_report(&r, 100);
        assert_eq!(rep.over_frames, vec![1]);
        assert!(rep.unattributed.is_empty());
        assert_eq!(rep.frames_with_commands, 2);
        assert_eq!(rep.max_commands_in_frame, 2);
        assert_eq!(rep.over_above, 0);
    }

    #[test]
    fn a_frame_that_kept_going_after_the_budget_is_unattributed() {
        let r = [st(1, 1, 150), st(1, 2, 10)];
        let rep = budget_report(&r, 100);
        assert_eq!(rep.over_frames, vec![1]);
        assert_eq!(rep.unattributed, vec![1]);
        assert_eq!(rep.over_above, 1);
    }

    #[test]
    fn control_results_do_not_count() {
        let r = [st(1, 0, 5_000), st(1, 1, 10)];
        let rep = budget_report(&r, 100);
        assert!(rep.over_frames.is_empty());
        assert_eq!(rep.results, 1);
    }

    #[test]
    fn damage_costs_and_life_are_judged_as_documented() {
        assert_eq!(cost_stats(&[]), (0, 0, 0));
        let v: Vec<u32> = (1..=100).collect();
        assert_eq!(cost_stats(&v), (50, 99, 100));
        assert!(about_half(0.5, 1.0) && about_half(0.55, 1.0) && about_half(7.0, 14.0));
        assert!(!about_half(1.0, 1.0) && !about_half(0.3, 1.0) && !about_half(0.5, 0.0));
    }

    #[test]
    fn a_sampling_log_gives_its_pending_counts() {
        let data = Lv::Table(vec![
            (Lv::String(b"pending_destroy".to_vec()), Lv::Number(3.0)),
            (Lv::String(b"pending_expired".to_vec()), Lv::Number(0.0)),
        ]);
        assert_eq!(
            Sampling::from_data(&data),
            Sampling {
                destroy: 3,
                overflow: u64::MAX,
                expired: 0
            }
        );
    }

    #[test]
    fn the_counter_may_sit_between_above_and_at_or_above() {
        let r = [st(1, 1, 100), st(2, 2, 101)];
        let rep = budget_report(&r, 100);
        assert_eq!((rep.over_above, rep.over_at_or_above), (1, 2));
        assert!(rep.counter_fits(1) && rep.counter_fits(2));
        assert!(!rep.counter_fits(0) && !rep.counter_fits(3));
    }
}
