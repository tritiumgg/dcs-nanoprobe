//! `soak campaign|control|pilot|rehearse|status|stop|judge`: the long soak
//! (S21), with the real sidecar and a client attached for the whole run.
//!
//! `soak campaign <name>` runs a campaign on a docker-wine target as
//! `persistence` runs its scenarios (the sidecar in its own container, its
//! store on disk), and for `--hours` churns it through the API as a campaign
//! would: handovers out of DCS and into it, groups removed and created,
//! groups killed and replaced, orders, effects and queries, with a world
//! watch and an event subscription open throughout. `soak control <name>`
//! runs the same churn of DCS objects with no agent loaded, through the eval
//! channel (`lua/scenarios/soak.lua`). `soak rehearse` runs the campaign
//! soak against `nanoprobe-fake-sidecar` here, shortened. `soak pilot
//! <name>` is a short control and a short campaign soak in one go.
//!
//! # Records
//!
//! A run takes one record every `interval` on one clock: record `k` is due
//! `k` intervals after the churn began, and carries `t_s` (that time),
//! `late_s` (how late it was taken), `segment`, and by key:
//!
//! | Key | What | Source |
//! |---|---|---|
//! | `dcs`, `wineserver`, `sidecar` | `rss_kb`, `threads`, `fds`, `pid`, `start`; `cpu_percent` of DCS and the sidecar | `/proc` in the game container |
//! | `container` | `working_set_kb`, `limit_kb` | the container's cgroup |
//! | `heap` | `server_kib`; after a forced collect `server_floor_kib` and `hook_floor_kib` | the eval channel |
//! | `ids` | `max_object_id`, `units`, `groups` in DCS; `spawned_units` so far | the eval channel; the churn's count |
//! | `agent` | the agent's `sampling` reports of the interval | the agent's log |
//! | `store` | file sizes; `journal_rows` and the export's counts; `event_rows` | the store folder; `ExportCampaign`; the subscription |
//! | `world` | groups by lifecycle state | `ListWorld` |
//! | `api` | health, heartbeat age, commands in flight, free bytes | `GetServerStatus` |
//! | `dcs.fps`, `p5_fps`, `model_per_real` | DCS's frame rate over a window | `lua/sampling/fps.lua` |
//! | `players` | players connected | DCS's log |
//!
//! Handles. Under Wine a Windows handle is an object in the Wine server, and
//! Linux has no count of them. What is counted is each process's open file
//! descriptors (`/proc/<pid>/fd`), for DCS, the Wine server and the sidecar,
//! with the Wine server's memory as the place a leaked handle's object
//! stays. On a Windows server the count to read is the process's handle
//! count; this tool does not run there.
//!
//! # What a long run meets
//!
//! - A player joins: the record's `players` says so and the run goes on.
//! - DCS stalls: a reading or a churn step that fails is an entry in
//!   `events.jsonl` and in the record's `errors`; the run goes on.
//! - DCS or the sidecar starts again (a watchdog, a crash): the identity of
//!   the processes changes, the record ends the segment, and the tool waits
//!   for DCS, loads the campaign's mission when no player is connected,
//!   starts the sidecar from its store and goes on in a new segment. The
//!   verdict fits the longest segment.
//! - The session that started the run ends: `--detach` runs the tool in a
//!   process group of its own, with its output in a log under `bundles/`.
//!   `soak status` lists the runs and `soak stop <folder>` ends one at its
//!   next step, with its verdict and its cleanup.
//! - The churn falls short of its plan: each record carries the churn's
//!   totals by kind (`churn`), and a kind under 80 % of its planned rate for
//!   90 minutes takes the verdict away (`churn_short`, EXTEND).
//!
//! Everything goes under `bundles/soak-<name>-<time>/`: `state.json`,
//! `records.jsonl`, `events.jsonl`, the sidecar's log, and at the end
//! `soak.json`, `verdict.json` and `verdict.md`. `soak judge <folder>
//! [--control <folder>]` writes the verdict again, against a control, and
//! keeps the run's provenance; with `--from <s>` and `--to <s>` it judges
//! that window of the records alone, into `verdict-window-<from>-<to>.*`.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use nanoprobe_proto::v1alpha1::{self as pb, EntityState, GroupCategory};
use nanoprobe_transport::names::{self, Kind};
use serde_json::{Value, json};
use tokio::runtime::Runtime;

use super::api::{Api, Streams, at, route};
use super::scenarios::{self, COUNTRY, GROUND, ORIGIN, Target, state};
use super::virtualization::{LAND_CHUNK, SPACING, places};
use super::{FakeTarget, Host, ServerOptions, on_server_with};
use crate::bundle;
use crate::profile::Target as Profile;
use crate::sampling::live::{self, Run};
use crate::soak::series;

const USAGE: &str = "usage: cargo xtask soak campaign <name> [--hours H | --minutes M] [--control <folder>]
           [--store-toml '<key = value>']... [--leave-dead] [--reattach] [--detach]
       cargo xtask soak control <name> [--hours H | --minutes M] [--detach]
       cargo xtask soak pilot <name> [--minutes M] [--detach]
       cargo xtask soak rehearse [--minutes M] [--leak-kib-per-s N [--baseline <folder>]] [--restart-at S] [--stall-at S] [--detach]
       cargo xtask soak status
       cargo xtask soak stop <folder>
       cargo xtask soak judge <folder> [--control <folder>] [--from <s>] [--to <s>]
       cargo xtask soak fit <folder> [--from-minute 75] [--block-minutes 5]";

/// The store's files and their keys in a record's `store`.
pub(super) const STORE_FILES: [(&str, &str); 4] = [
    ("store_bytes", "store.sqlite"),
    ("store_wal_bytes", "store.sqlite-wal"),
    ("events_bytes", "events.sqlite"),
    ("events_wal_bytes", "events.sqlite-wal"),
];

/// How a run is sized and paced. The sizes are the campaign's; the control
/// drives the same numbers of DCS objects.
#[derive(Clone, Debug)]
pub(super) struct Plan {
    /// Seconds the churn runs.
    duration_s: f64,
    /// Seconds between records. A minute resolves every series here and
    /// costs DCS one frame-rate window and three evals.
    interval_s: f64,
    /// Seconds between churn steps, as S11's soak.
    tick_s: f64,
    /// Records between forced collects. Each one stalls DCS for the length
    /// of a full collect, so they are as far apart as still gives a 2-hour
    /// control 6 floors after its warm-up: 15 minutes.
    collect_every: u64,
    /// Records between counts of the journal through `ExportCampaign`,
    /// which reads the whole store: every 30 minutes.
    export_every: u64,
    /// Groups in the campaign. Three in four are virtual at any time, as a
    /// campaign world larger than what is live.
    world: usize,
    /// Groups held in DCS. With `units` 4 this is 1,000 units, about a
    /// fifth of them driving at any time. On the Wine server of S11, DCS
    /// held 112 fps at 1,200 objects with a fifth routed through 90 minutes
    /// of churn, and 50 to 63 fps with stalls at 2,000
    /// (docs/results/2026-11-14-s11-budget-and-baseline.md), so 1,000
    /// leaves the simulation core headroom for the spawn bursts and for a
    /// visitor over 8 hours.
    live: usize,
    /// Units in a group.
    units: usize,
    /// Groups handed out of DCS and as many handed in, per step. With
    /// `replace` and the kills, the 250 live groups turn over in about 24
    /// minutes (48 steps), S11's rate: 2,500 units spawned and destroyed an
    /// hour, so a leak of 1 KiB per spawned unit is 2.4 MiB an hour.
    handover: usize,
    /// Groups removed and created again, per step.
    replace: usize,
    /// Route orders given per step. A route is 4 laps of a 200 m square in
    /// the group's own 300 m cell, 11 minutes at 5 m/s, so about 60 of the
    /// 250 groups drive at a time and no route crosses another.
    orders: usize,
    /// Damage effects per step.
    effects: usize,
    /// A group is killed every this many steps: 4 units every 2 minutes,
    /// 960 wrecks in 8 hours. S11's 90-minute soak held 112 fps while it
    /// killed about 2,250 units (derived from its churn rate, not counted),
    /// so the wrecks of 8 hours stay inside what DCS was seen to carry. 0
    /// kills none.
    kill_every: u64,
    /// The frame-rate window of a record, seconds.
    fps_s: u32,
    /// Rehearsal faults: stop the fake behind the tool's back at this
    /// time, and freeze it for `stall_for_s` at that one.
    restart_at_s: Option<f64>,
    stall_at_s: Option<f64>,
    stall_for_s: f64,
    /// Rehearsal: kibibytes the fake sidecar leaks each second.
    leak_kib_per_s: u64,
    /// A killed group is left dead, for its expiry (spec/world.md,
    /// "Expiry"), and a new group is put at its place (`--leave-dead`).
    /// Without it a dead group is removed and created again, as S21's churn.
    leave_dead: bool,
    /// Lines for the sidecar's `[store]` (`--store-toml`, once per line);
    /// a key given here replaces the run's own default for it.
    store_toml: Vec<String>,
    /// After the churn, start the sidecar again once from its store and
    /// count the live units that have no life value 10 s and 60 s after it
    /// attaches (`--reattach`). The sidecar's memory starts again there,
    /// after the records.
    reattach: bool,
    /// Seconds of uptime before which nothing is fitted, when a later window
    /// of a run is judged (`soak judge --from`); the warm-up otherwise.
    fit_from_s: Option<f64>,
}

impl Plan {
    fn live(duration_s: f64) -> Plan {
        Plan {
            duration_s,
            interval_s: 60.0,
            tick_s: 30.0,
            collect_every: 15,
            export_every: 30,
            world: 1_000,
            live: 250,
            units: 4,
            handover: 4,
            replace: 1,
            orders: 3,
            effects: 2,
            kill_every: 4,
            fps_s: 10,
            restart_at_s: None,
            stall_at_s: None,
            stall_for_s: 0.0,
            leak_kib_per_s: 0,
            leave_dead: false,
            store_toml: Vec::new(),
            reattach: false,
            fit_from_s: None,
        }
    }

    /// A pilot: the live plan with a collect and an export every second
    /// record, so a run of 10 minutes has 4 of each after its warm-up, which
    /// is what a fit needs.
    fn pilot(duration_s: f64) -> Plan {
        Plan {
            collect_every: 2,
            export_every: 2,
            ..Plan::live(duration_s)
        }
    }

    /// The rehearsal: seconds where the live plan has minutes, and a world a
    /// debug build of the fake turns over in that time.
    fn rehearsal(duration_s: f64) -> Plan {
        Plan {
            interval_s: 5.0,
            tick_s: 3.0,
            collect_every: 0,
            export_every: 6,
            world: 60,
            live: 20,
            units: 2,
            handover: 2,
            // The fake has no scripting engine to explode a unit with.
            kill_every: 0,
            fps_s: 0,
            ..Plan::live(duration_s)
        }
    }

    fn json(&self) -> Value {
        json!({
            "duration_s": self.duration_s, "interval_s": self.interval_s, "tick_s": self.tick_s,
            "collect_every_records": self.collect_every, "export_every_records": self.export_every,
            "world_groups": self.world, "live_groups": self.live, "units_per_group": self.units,
            "handover_per_step": self.handover, "replace_per_step": self.replace,
            "orders_per_step": self.orders, "effects_per_step": self.effects,
            "kill_every_steps": self.kill_every, "fps_window_s": self.fps_s,
            "restart_at_s": self.restart_at_s, "stall_at_s": self.stall_at_s,
            "stall_for_s": self.stall_for_s, "leak_kib_per_s": self.leak_kib_per_s,
            "leave_dead": self.leave_dead, "store_toml": self.store_toml,
            "reattach": self.reattach,
        })
    }

    /// The warm-up of a run of this length: a quarter of it, and at most an
    /// hour. The live set turns over once in 24 minutes, and S11 saw DCS's
    /// resident memory slow after 20 minutes and the slowest enumeration
    /// step flatten after 80, so an hour covers what was seen to settle; a
    /// shorter run gives up a quarter and says less.
    fn warmup_s(&self) -> f64 {
        self.fit_from_s
            .unwrap_or((self.duration_s / 4.0).min(3_600.0))
    }

    /// The block length of the fit (`series`, "Blocks"): ten records, and
    /// in a run too short for six such blocks after its warm-up, a sixth of
    /// what is left, so a short run is still fitted. Under three records a
    /// block the fit takes the records themselves.
    fn block_s(&self) -> f64 {
        let sixth = (self.duration_s - self.warmup_s()) / 6.0;
        (self.interval_s * 10.0).min(sixth.max(self.interval_s))
    }
}

/// What a run drives and measures: the campaign through the API, or DCS
/// alone through the eval channel.
trait Subject {
    /// One churn step. What it did, by counter; `errors` lists what failed.
    fn tick(&mut self, n: u64, plan: &Plan) -> Value;
    /// One record's readings. A reading that fails is absent and named in
    /// `errors`.
    fn record(&mut self, collect: bool, export: bool, plan: &Plan) -> Value;
    /// See [`Target::identity`].
    fn identity(&mut self) -> Result<Option<String>, String>;
    /// Bring DCS, and what the run needs beside it, back after a restart.
    fn recover(&mut self, plan: &Plan) -> Result<(), String>;
    fn inject_restart(&mut self) -> Result<(), String> {
        Err("nothing to inject on this target".into())
    }
    fn stall(&mut self, _seconds: f64) -> Result<(), String> {
        Err("nothing to stall on this target".into())
    }
}

fn append(path: &Path, v: &Value) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{v}");
    }
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The run's `state.json`: what `soak status` reads.
struct State {
    path: PathBuf,
    v: Value,
}

impl State {
    fn new(folder: &Path, kind: &str, target: &str, plan: &Plan) -> State {
        let s = State {
            path: folder.join("state.json"),
            v: json!({
                "kind": kind, "target": target, "pid": std::process::id(),
                "started": bundle::utc_now(), "phase": "setup", "records": 0,
                "segment": 0, "plan": plan.json(),
            }),
        };
        s.write();
        s
    }

    fn set(&mut self, key: &str, v: Value) {
        self.v[key] = v;
        self.write();
    }

    fn write(&self) {
        let _ = std::fs::write(&self.path, self.v.to_string());
    }
}

/// Add the numbers of `d` into `totals`, and return its `errors`.
fn add_counters(totals: &mut serde_json::Map<String, Value>, d: &Value) -> Vec<Value> {
    for (k, v) in d.as_object().into_iter().flatten() {
        if let Some(n) = v.as_f64() {
            let t = totals.get(k).and_then(Value::as_f64).unwrap_or(0.0);
            totals.insert(k.clone(), json!(t + n));
        }
    }
    d["errors"].as_array().cloned().unwrap_or_default()
}

/// The share of the container's memory limit at which a run ends itself:
/// with no swap the limit kills DCS, and a run that ends keeps its records.
const MEMORY_STOP: f64 = 0.90;

/// Run the churn and take the records until the plan's duration, a `stop`
/// file, or the memory guard. Returns how the run ended.
fn drive(s: &mut dyn Subject, plan: &Plan, folder: &Path, st: &mut State) -> String {
    let records_path = folder.join("records.jsonl");
    let events_path = folder.join("events.jsonl");
    let event = |what: &str, t_s: f64, detail: Value| {
        println!("  event {what} at {t_s:.0} s: {detail}");
        append(
            &events_path,
            &json!({ "what": what, "t_s": t_s, "wall": bundle::utc_now(), "detail": detail }),
        );
    };
    // The small term keeps a duration that is a whole number of intervals
    // from losing its last record to the division's rounding.
    let total = (plan.duration_s / plan.interval_s + 1e-9).floor() as u64;
    let begin = Instant::now();
    let (mut k, mut tick_n, mut segment) = (0u64, 0u64, 0u64);
    let mut totals = serde_json::Map::new();
    let mut identity = s.identity().ok().flatten();
    let mut down = false;
    let (mut restarted, mut stalled) = (false, 0u8);
    st.set("phase", json!("running"));
    let ended = loop {
        if k >= total {
            break "completed";
        }
        if folder.join("stop").exists() {
            event("stop", begin.elapsed().as_secs_f64(), json!("stop file"));
            break "stopped";
        }
        let now = begin.elapsed().as_secs_f64();
        if let Some(at) = plan.restart_at_s
            && !restarted
            && now >= at
        {
            restarted = true;
            event("inject_restart", now, json!(s.inject_restart().err()));
        }
        if let Some(at) = plan.stall_at_s
            && stalled == 0
            && now >= at
        {
            stalled = 1;
            event(
                "inject_stall",
                now,
                json!({ "seconds": plan.stall_for_s, "error": s.stall(plan.stall_for_s).err() }),
            );
        }
        let record_due = (k + 1) as f64 * plan.interval_s;
        let tick_due = (tick_n + 1) as f64 * plan.tick_s;
        if now >= record_due {
            // A record that could not be taken in its interval is skipped:
            // the next one taken carries the time it was due.
            let due = (now / plan.interval_s).floor() as u64;
            if due > k + 1 {
                event(
                    "records_skipped",
                    now,
                    json!({ "from": k + 1, "to": due - 1 }),
                );
            }
            k = due.min(total);
            let t_s = k as f64 * plan.interval_s;
            let collect = plan.collect_every > 0 && k.is_multiple_of(plan.collect_every);
            let export = plan.export_every > 0
                && (k == 1 || k == total || k.is_multiple_of(plan.export_every));
            // A change of either process is the end of the segment. A
            // target that could not be asked decides nothing. The records
            // taken until DCS is back belong to no segment, so no fit reads
            // a process that just started as a fall.
            let mut segment_end = false;
            let asked = if down { None } else { s.identity().ok() };
            if identity.is_none()
                && let Some(Some(first)) = &asked
            {
                // The first reading that answered: nothing to compare with.
                identity = Some(first.clone());
            }
            if let Some(id) = asked
                && id != identity
            {
                event(
                    "segment_end",
                    t_s,
                    json!({ "segment": segment, "was": identity, "is": id }),
                );
                segment_end = true;
                down = true;
            }
            let mut r = s.record(collect, export, plan);
            r["t_s"] = json!(t_s);
            r["late_s"] = json!(now - t_s);
            r["took_s"] = json!(begin.elapsed().as_secs_f64() - now);
            r["segment"] = if down { Value::Null } else { json!(segment) };
            if segment_end {
                r["segment_end"] = json!(true);
            }
            r["wall"] = json!(bundle::utc_now());
            r["churn"] = Value::Object(totals.clone());
            for e in r["errors"].as_array().cloned().unwrap_or_default() {
                event("reading_failed", t_s, e);
            }
            if r["players"].as_u64().unwrap_or(0) > 0 {
                event("players", t_s, r["players"].clone());
            }
            let guard = r["container"]["working_set_kb"]
                .as_f64()
                .zip(r["container"]["limit_kb"].as_f64())
                .is_some_and(|(used, limit)| used > MEMORY_STOP * limit);
            println!("  record {k} of {total}: {r}");
            append(&records_path, &r);
            st.v["records"] = json!(k);
            st.v["last_t_s"] = json!(t_s);
            st.v["last_wall"] = r["wall"].clone();
            st.v["segment"] = json!(segment);
            st.write();
            if guard {
                event("memory_guard", t_s, r["container"].clone());
                break "memory guard";
            }
            if down {
                match s.recover(plan) {
                    Ok(()) => {
                        segment += 1;
                        identity = s.identity().ok().flatten();
                        down = false;
                        event(
                            "segment_start",
                            t_s,
                            json!({ "segment": segment, "is": identity }),
                        );
                    }
                    Err(e) => event("recover_waiting", t_s, json!(e)),
                }
            }
        } else if !down && now >= tick_due {
            // A step that could not run in its slot is not made up.
            tick_n = (now / plan.tick_s).floor() as u64;
            let d = s.tick(tick_n, plan);
            for e in add_counters(&mut totals, &d) {
                event("step_failed", now, e);
            }
        } else {
            let next = record_due.min(if down { record_due } else { tick_due });
            std::thread::sleep(Duration::from_secs_f64((next - now).clamp(0.05, 1.0)));
        }
    };
    st.set("phase", json!("finishing"));
    st.set("ended", json!(ended));
    ended.to_owned()
}

// ---------------------------------------------------------------------------
// Readings on a docker-wine target.

/// Prints `key value` lines: for DCS, the Wine server and the sidecar the
/// process's PID, start time, CPU ticks, resident memory, threads and open
/// descriptors; the container's memory and limit; and the sizes of the
/// store files in `$1` when it is not empty. DCS is found as `meter`'s
/// sampler finds it, the sidecar and the Wine server by process name. The
/// sidecar is visible only when its container shares the game container's
/// process namespace.
const PROCESSES: &str = r#"S=$1
N="dcs.exe dcs_server.exe"
best=; most=-1; W=; C=
for p in /proc/[0-9]*; do
  c=$(cat $p/comm 2>/dev/null) || continue
  case "$c" in
    wineserver*) W=${p#/proc/}; continue ;;
    nanoprobe-sidec*) C=${p#/proc/}; continue ;;
  esac
  a=$(tr '\0' '\n' <$p/cmdline 2>/dev/null | head -n 1 | tr '\\' '/')
  [ -n "$a" ] || continue
  b=$(printf %s "${a##*/}" | tr 'A-Z' 'a-z')
  case " $N " in *" $b "*) ;; *) continue ;; esac
  k=$(sed -n 's/^VmRSS:[^0-9]*\([0-9]*\).*/\1/p' $p/status 2>/dev/null)
  if [ "${k:-0}" -gt "$most" ]; then most=${k:-0}; best=${p#/proc/}; fi
done
one() {
  [ -n "$2" ] && [ -r /proc/$2/status ] || return 0
  st=$(sed 's/.*) //' /proc/$2/stat 2>/dev/null)
  [ -n "$st" ] || return 0
  echo "$1.pid $2"
  echo "$1.start $(echo "$st" | cut -d' ' -f20)"
  u=$(echo "$st" | cut -d' ' -f12); k=$(echo "$st" | cut -d' ' -f13)
  echo "$1.cpu_ticks $(( ${u:-0} + ${k:-0} ))"
  echo "$1.rss_kb $(sed -n 's/^VmRSS:[^0-9]*\([0-9]*\).*/\1/p' /proc/$2/status)"
  echo "$1.threads $(sed -n 's/^Threads:[^0-9]*\([0-9]*\).*/\1/p' /proc/$2/status)"
  n=$(ls /proc/$2/fd 2>/dev/null | wc -l)
  [ "${n:-0}" -gt 0 ] && echo "$1.fds $n"
  return 0
}
one dcs "$best"; one wineserver "$W"; one sidecar "$C"
hz=$(getconf CLK_TCK 2>/dev/null); echo "clock.hz ${hz:-100}"; echo "clock.epoch $(date +%s)"
G=/sys/fs/cgroup; mc=; mi=; mx=
if [ -r $G/memory.current ]; then
  mc=$(cat $G/memory.current); mi=$(sed -n 's/^inactive_file //p' $G/memory.stat); mx=$(cat $G/memory.max)
elif [ -r $G/memory/memory.usage_in_bytes ]; then
  mc=$(cat $G/memory/memory.usage_in_bytes)
  mi=$(sed -n 's/^total_inactive_file //p' $G/memory/memory.stat)
  mx=$(cat $G/memory/memory.limit_in_bytes)
fi
[ -n "$mc" ] && echo "container.memory_bytes $mc"
[ -n "$mi" ] && echo "container.inactive_file_bytes $mi"
case "$mx" in ''|max) ;; *) echo "container.limit_bytes $mx" ;; esac
if [ -n "$S" ]; then
  for f in store.sqlite store.sqlite-wal events.sqlite events.sqlite-wal; do
    [ -e "$S/$f" ] && echo "file.$f $(stat -c %s "$S/$f")"
  done
fi
exit 0
"#;

/// The `key value` lines of [`PROCESSES`] as nested JSON: `a.b 1` is
/// `{"a": {"b": 1}}`. The first dot splits; a line that is not a key and a
/// number is dropped.
pub(super) fn parse_lines(text: &str) -> Value {
    let mut out = json!({});
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(value), None) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Some((a, b)), Ok(n)) = (key.split_once('.'), value.parse::<f64>()) else {
            continue;
        };
        out[a][b] = json!(n);
    }
    out
}

/// The process and file figures of the game container, read once.
pub(super) fn processes(run: &Run, store: &str) -> Result<Value, String> {
    let out = run.sh(PROCESSES, &[store])?;
    let v = parse_lines(&String::from_utf8_lossy(&out));
    if v["clock"]["epoch"].is_null() {
        return Err("the process reading printed nothing".into());
    }
    Ok(v)
}

/// A process's PID and start time, which name one run of it.
pub(super) fn identity_of(processes: &Value, name: &str) -> Option<String> {
    let p = &processes[name];
    Some(format!("{}:{}", p["pid"].as_f64()?, p["start"].as_f64()?))
}

/// Counts DCS's groups whose name starts with the prefix and their units,
/// and reads the largest runtime object ID among the units and the state's
/// Lua heap. The runtime ID is the object's `id_` field, the number DCS
/// steps by 256 per object (LIVE for the step: S11's soak read it through
/// the agent's records; UNVERIFIED that `id_` is that number until a run
/// compares them).
const DCS_OBJECTS: &str = r#"
local prefix = __PREFIX__
local groups, units, max = 0, 0, 0
for _, side in ipairs({ 0, 1, 2 }) do
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    local ok, n = pcall(g.getName, g)
    if ok and type(n) == 'string' and n:sub(1, #prefix) == prefix then
      groups = groups + 1
      for _, u in ipairs(g:getUnits() or {}) do
        units = units + 1
        local id = tonumber(u.id_)
        if id and id > max then max = id end
      end
    end
  end
end
return { groups = groups, units = units, max_object_id = max > 0 and max or nil, heap_kib = collectgarbage('count') }
"#;

/// One record's readings outside the API on a docker-wine target. `store`
/// is the campaign's store folder as the game container sees it, or empty;
/// `prefix` starts the names of the run's DCS groups.
pub(super) fn probe(
    run: &Run,
    store: &str,
    prefix: &str,
    fps_s: u32,
    cpu_before: &mut BTreeMap<&'static str, (f64, f64)>,
) -> Value {
    let mut errors = Vec::new();
    let mut v = json!({});
    match processes(run, store) {
        Ok(p) => {
            for name in ["dcs", "wineserver", "sidecar"] {
                if p[name].is_object() {
                    v[name] = p[name].clone();
                }
            }
            for name in ["dcs", "sidecar"] {
                let (Some(ticks), Some(epoch), Some(hz)) = (
                    p[name]["cpu_ticks"].as_f64(),
                    p["clock"]["epoch"].as_f64(),
                    p["clock"]["hz"].as_f64(),
                ) else {
                    continue;
                };
                if let Some((t0, e0)) = cpu_before.get(name).copied()
                    && epoch > e0
                    && ticks >= t0
                {
                    v[name]["cpu_percent"] = json!((ticks - t0) / hz / (epoch - e0) * 100.0);
                }
                cpu_before.insert(name, (ticks, epoch));
            }
            let c = &p["container"];
            if let Some(used) = c["memory_bytes"].as_f64() {
                let cache = c["inactive_file_bytes"].as_f64().unwrap_or(0.0);
                v["container"]["working_set_kb"] = json!((used - cache) / 1024.0);
            }
            if let Some(limit) = c["limit_bytes"].as_f64() {
                v["container"]["limit_kb"] = json!(limit / 1024.0);
            }
            for (key, file) in STORE_FILES {
                if let Some(n) = p["file"][file].as_f64() {
                    v["store"][key] = json!(n);
                }
            }
        }
        Err(e) => errors.push(json!(format!("processes: {e}"))),
    }
    let source = DCS_OBJECTS.replace("__PREFIX__", &format!("{prefix:?}"));
    match run.eval("server", "soak-objects", &source, 60) {
        Ok(o) => {
            v["ids"] = json!({
                "max_object_id": o["max_object_id"], "units": o["units"], "groups": o["groups"],
            });
            v["heap"]["server_kib"] = o["heap_kib"].clone();
        }
        Err(e) => errors.push(json!(format!("objects: {e}"))),
    }
    if fps_s > 0 {
        match run.fps_window("soak", 0, fps_s) {
            Ok(f) => {
                for key in ["fps", "p5_fps", "model_per_real"] {
                    v["dcs"][key] = f[key].clone();
                }
                v["moving"] = f["moving"].clone();
            }
            Err(e) => errors.push(json!(format!("fps: {e}"))),
        }
    }
    match run.remote.players() {
        Ok(n) => v["players"] = json!(n),
        Err(e) => errors.push(json!(format!("players: {e}"))),
    }
    v["errors"] = json!(errors);
    v
}

/// A forced full collect in the `server` and the `hook` state, and each
/// heap right after it: exact floors, as S11's soak read the `server`
/// state's. The agent's shim runs in `server`; its loader and the eval hook
/// run in `hook`.
pub(super) fn heap_floors(run: &Run) -> Result<Value, String> {
    let mut v = json!({});
    for state in ["server", "hook"] {
        let begin = Instant::now();
        let kib = run.eval(
            state,
            "soak-collect",
            "collectgarbage('collect'); return collectgarbage('count')",
            120,
        )?;
        v[format!("{state}_floor_kib")] = json!(
            kib.as_f64()
                .ok_or_else(|| format!("the {state} heap reading is not a number: {kib}"))?
        );
        v[format!("{state}_collect_ms")] = json!(begin.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(v)
}

/// The agent's `sampling` reports after the first `seen`, and how many the
/// log holds; with no `seen`, only the count. A log that holds fewer than
/// `seen` started again: all of it is read.
pub(super) fn agent_reports_after(
    run: &Run,
    seen: Option<usize>,
) -> Result<(Vec<Value>, usize), String> {
    let log = run.remote.full_path("nanoprobe/logs/agent.log");
    let out = run.sh(
        "n=$(grep -c -F '\"msg\":\"sampling\"' \"$1\" 2>/dev/null); n=${n:-0}; echo \"$n\"; \
         [ \"$2\" = count ] && exit 0; \
         s=$2; [ \"$n\" -lt \"$s\" ] && s=0; \
         grep -F '\"msg\":\"sampling\"' \"$1\" 2>/dev/null | tail -n +$((s + 1)) || true",
        &[&log, &seen.map_or("count".to_owned(), |n| n.to_string())],
    )?;
    let text = String::from_utf8_lossy(&out);
    let mut lines = text.lines();
    let total: usize = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .ok_or("the agent log's report count is not a number")?;
    let reports = lines
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["msg"] == "sampling")
        .map(|l| l["data"].clone())
        .collect();
    Ok((reports, total))
}

/// The agent's figures over the reports of one interval: the mean share,
/// the frames and how many took over 250 µs, the slowest steps, and the
/// last report's sizes of the agent's own structures.
fn agent_record(reports: &[Value]) -> Value {
    if reports.is_empty() {
        return json!({ "reports": 0 });
    }
    let (_, span) = live::span(reports);
    let tail = live::tail(reports);
    let max = |k: &str| {
        reports
            .iter()
            .filter_map(|r| r[k].as_f64())
            .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x))))
    };
    let last = &reports[reports.len() - 1];
    let frames = tail["frames"].as_f64().unwrap_or(0.0);
    let over = tail["over_250"].as_f64().unwrap_or(0.0);
    json!({
        "reports": reports.len(),
        "share_percent": span["mean"].as_f64().map(|s| s * 100.0),
        "frames": tail["frames"],
        "frames_over_250": tail["over_250"],
        "frames_over_250_percent": (frames > 0.0).then(|| over / frames * 100.0),
        "frame_max_us": tail["max_us"],
        "enum_step_max_us": max("enum_step_max_us"),
        "birth_step_max_us": max("birth_step_max_us"),
        "rows": last["rows"],
        "births": last["births"],
        "pending_destroy": last["pending_destroy"],
        "pending_deaths": last["pending_deaths"],
        "loadouts_waiting": last["loadouts_waiting"],
        "revivals": last["revivals"],
        "scale": last["scale"],
        "window_s": reports.iter().filter_map(|r| r["window_s"].as_f64()).sum::<f64>(),
    })
}

/// A local process's resident memory, threads and descriptors, for the
/// fake sidecar: `ps` for the memory, `/proc` on Linux and `lsof` and
/// `ps -M` on macOS for the rest.
pub(super) fn local_process(pid: u32) -> Value {
    let text = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .stderr(Stdio::null())
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    };
    let p = pid.to_string();
    let mut v = json!({ "pid": pid });
    if let Some(rss) =
        text("ps", &["-o", "rss=", "-p", &p]).and_then(|t| t.trim().parse::<f64>().ok())
    {
        v["rss_kb"] = json!(rss);
    }
    if let Ok(dir) = std::fs::read_dir(format!("/proc/{p}/fd")) {
        v["fds"] = json!(dir.count());
        if let Ok(tasks) = std::fs::read_dir(format!("/proc/{p}/task")) {
            v["threads"] = json!(tasks.count());
        }
    } else {
        // `lsof` and `ps -M` each print one header line.
        if let Some(t) = text("lsof", &["-p", &p]) {
            v["fds"] = json!(t.lines().count().saturating_sub(1));
        }
        if let Some(t) = text("ps", &["-M", "-p", &p]) {
            v["threads"] = json!(t.lines().count().saturating_sub(1));
        }
    }
    v
}

// ---------------------------------------------------------------------------
// The campaign soak.

/// A place of the ring and the group the campaign holds there.
struct Slot {
    group: u64,
    generation: u32,
    /// The step this group was last given a route, if it was.
    routed: Option<u64>,
}

/// Seconds a route of [`Campaign::lap_order`] takes: 16 legs of two thirds
/// of [`SPACING`] at 5 m/s.
const ROUTE_S: f64 = 16.0 * SPACING * 2.0 / 3.0 / 5.0;

/// Whether a group given a route at step `routed` still drives it at step
/// `n`. A group whose route has ended can be given another, or be killed.
fn driving(routed: Option<u64>, n: u64, plan: &Plan) -> bool {
    let steps = (ROUTE_S / plan.tick_s).ceil() as u64 + 1;
    routed.is_some_and(|at| n < at.saturating_add(steps))
}

struct Campaign<'a, 'r> {
    t: &'a mut dyn Target,
    api: Api<'r>,
    streams: Option<Streams>,
    c: u64,
    theater: String,
    places: Vec<(f64, f64)>,
    slots: Vec<Slot>,
    place_of: BTreeMap<u64, usize>,
    /// The places whose group the churn put into DCS, oldest first.
    window: VecDeque<usize>,
    /// Where the ring is searched for the next virtual group.
    next_in: usize,
    /// The place killed and whether a step has looked at it since.
    killed: Option<(usize, bool)>,
    /// Groups left dead (`Plan::leave_dead`), with their units.
    left_dead: Vec<(u64, Vec<u64>)>,
    order_next: usize,
    reports_seen: usize,
    spawned_units: u64,
    /// The agent's whole-run sums: share times window, window, frames and
    /// frames over 250 µs.
    whole: [f64; 4],
    rng: u64,
}

fn in_dcs(g: &pb::Group) -> bool {
    matches!(state(g), EntityState::Live | EntityState::Materializing)
}

impl<'a, 'r> Campaign<'a, 'r> {
    /// Create the campaign and its world, and wait until the live groups
    /// are in DCS.
    fn setup(
        t: &'a mut dyn Target,
        rt: &'r Runtime,
        plan: &Plan,
        tag: &str,
    ) -> Result<Campaign<'a, 'r>, String> {
        let mut api = Api::connect_with(
            rt,
            &t.endpoint(),
            t.token().as_deref(),
            &format!("{tag}-soak"),
            Duration::from_secs(90),
            CALL_TIMEOUT,
        )?;
        let c = scenarios::campaign(t, &mut api, &format!("soak-{tag}"))?;
        let theater = t.theater();
        let places = places(t, ORIGIN, plan.world)?;
        let mut s = Campaign {
            t,
            api,
            streams: None,
            c,
            theater,
            places,
            slots: Vec::new(),
            place_of: BTreeMap::new(),
            window: VecDeque::new(),
            next_in: plan.live % plan.world,
            killed: None,
            left_dead: Vec::new(),
            order_next: 0,
            reports_seen: 0,
            spawned_units: 0,
            whole: [0.0; 4],
            rng: 0x9E37_79B9_7F4A_7C15,
        };
        println!("== {} groups, {} of them in DCS", plan.world, plan.live);
        for place in 0..plan.world {
            let live = place < plan.live;
            let id = s
                .api
                .put_group(c, s.spec(place, 0, plan.units, !live), None)?
                .group_id;
            s.slots.push(Slot {
                group: id,
                generation: 0,
                routed: None,
            });
            s.place_of.insert(id, place);
            if live {
                s.window.push_back(place);
            }
        }
        s.spawned_units = (plan.live * plan.units) as u64;
        let want: Vec<u64> = s.slots[..plan.live].iter().map(|x| x.group).collect();
        scenarios::wait(
            s.t,
            &mut s.api,
            "the live groups",
            900.0,
            5.0,
            true,
            |api, _| {
                let g = api.groups(c)?;
                Ok(want
                    .iter()
                    .all(|id| g.get(id).is_some_and(scenarios::live))
                    .then_some(()))
            },
        )?;
        s.reports_seen =
            s.t.agent_reports_after(None)
                .ok()
                .flatten()
                .map_or(0, |(_, total)| total);
        s.streams = Some(s.api.attach_streams(c));
        Ok(s)
    }

    /// The group of a place: `units` M-113 in a line 12 m apart, heading
    /// north. The label names the place and the generation, so no two
    /// groups of the run share one.
    fn spec(&self, place: usize, generation: u32, units: usize, r#virtual: bool) -> pb::GroupSpec {
        let p = self.places[place];
        pb::GroupSpec {
            label: format!("s{place}g{generation}"),
            country: COUNTRY,
            category: GroupCategory::Ground as i32,
            units: (0..units)
                .map(|i| pb::UnitSpec {
                    r#type: GROUND.into(),
                    label: format!("s{place}g{generation}-{i}"),
                    position: Some(at(&self.theater, p.0, p.1 + 12.0 * i as f64, 0.0)),
                    ..pb::UnitSpec::default()
                })
                .collect(),
            r#virtual,
            ..pb::GroupSpec::default()
        }
    }

    /// Four laps of a square of 200 m inside the place's own 300 m cell, at
    /// 5 m/s: the group drives for about 11 minutes and crosses no other.
    fn lap_order(&self, place: usize) -> pb::Order {
        let (n, e) = self.places[place];
        let side = SPACING * 2.0 / 3.0;
        let square = [(n + side, e), (n + side, e + side), (n, e + side), (n, e)];
        let points: Vec<(f64, f64, f64)> = (0..16)
            .map(|i| (square[i % 4].0, square[i % 4].1, 0.0))
            .collect();
        route(&self.theater, &points, 5.0)
    }

    fn next_random(&mut self) -> u64 {
        // xorshift: the run needs variety, not unpredictability.
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// Remove the group of a place and create one there, in DCS.
    fn replace(&mut self, place: usize, units: usize) -> Result<(), String> {
        let old = self.slots[place].group;
        self.api.remove_entity(self.c, old)?;
        self.place_of.remove(&old);
        self.put_at(place, units)
    }

    /// Leave the dead group of a place as it is, for its expiry, and create
    /// a new one there, in DCS.
    fn renew(&mut self, place: usize, units: usize, members: Vec<u64>) -> Result<(), String> {
        let old = self.slots[place].group;
        self.put_at(place, units)?;
        self.place_of.remove(&old);
        self.left_dead.push((old, members));
        Ok(())
    }

    /// Create the next generation's group at a place, in DCS.
    fn put_at(&mut self, place: usize, units: usize) -> Result<(), String> {
        let generation = self.slots[place].generation + 1;
        let spec = self.spec(place, generation, units, false);
        let id = self.api.put_group(self.c, spec, None)?.group_id;
        self.slots[place] = Slot {
            group: id,
            generation,
            routed: None,
        };
        self.place_of.insert(id, place);
        self.window.retain(|p| *p != place);
        self.window.push_back(place);
        self.spawned_units += units as u64;
        Ok(())
    }
}

/// How long one API call of the soak waits. The sidecar answers a call in
/// milliseconds (`QueryArea` over 10,000 groups took under 50 ms, S20), so
/// 30 s is a sidecar that does not answer, and a step that meets one is cut
/// short instead of waiting two minutes for each of its calls.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Groups replaced for being dead, lost or unspawnable in one step, at
/// most. After a DCS restart many may be waiting; the rest go next step.
const REPLACE_CAP: usize = 20;

impl Subject for Campaign<'_, '_> {
    fn tick(&mut self, n: u64, plan: &Plan) -> Value {
        let mut d: BTreeMap<&str, u64> = BTreeMap::new();
        let mut errors: Vec<Value> = Vec::new();
        let note = |errors: &mut Vec<Value>, what: &str, r: Result<(), String>| -> bool {
            if let Err(e) = &r {
                errors.push(json!(format!("{what}: {e}")));
            }
            r.is_ok()
        };
        // A step that has used its whole slot stops there: its calls are
        // waiting on a sidecar or a DCS that does not answer, and the next
        // step reads the world again.
        let begin = Instant::now();
        let slot = Duration::from_secs_f64(plan.tick_s);
        let counted = |mut d: BTreeMap<&str, u64>, mut errors: Vec<Value>, cut: Option<&str>| {
            if let Some(before) = cut {
                errors.push(json!(format!("the step was cut short before its {before}")));
                *d.entry("steps_cut").or_default() += 1;
            } else {
                *d.entry("steps").or_default() += 1;
            }
            let mut v = json!(d);
            v["errors"] = json!(errors);
            v
        };
        let groups = match self.api.groups(self.c) {
            Ok(g) => g,
            Err(e) => return json!({ "steps_failed": 1, "errors": [format!("ListWorld: {e}")] }),
        };
        let units = plan.units;
        // The window follows the store: a group that left DCS by itself
        // (dead, lost) is no longer in it.
        self.window
            .retain(|p| groups.get(&self.slots[*p].group).is_some_and(in_dcs));

        // The group killed last time stays out of the handover until the
        // store shows it dead. One that still stands whole a step later was
        // missed, and is churned as any other.
        if let Some((place, seen)) = self.killed {
            let g = groups.get(&self.slots[place].group);
            let alive = g.map_or(0, |g| g.units.iter().filter(|u| u.alive).count());
            if g.is_none_or(|g| !in_dcs(g)) || alive < units {
                self.killed = None;
            } else if !seen {
                self.killed = Some((place, true));
            } else {
                *d.entry("kills_missed").or_default() += 1;
                self.killed = None;
            }
        }

        // Dead, lost and unspawnable groups are removed and created again.
        let gone: Vec<(usize, EntityState)> = (0..self.slots.len())
            .filter_map(|p| {
                let s = groups.get(&self.slots[p].group).map(state);
                match s {
                    Some(
                        s @ (EntityState::Dead | EntityState::Lost | EntityState::Unspawnable),
                    ) => Some((p, s)),
                    _ => None,
                }
            })
            .take(REPLACE_CAP)
            .collect();
        for (place, was) in gone {
            if plan.leave_dead && was == EntityState::Dead {
                let members: Vec<u64> = groups[&self.slots[place].group]
                    .units
                    .iter()
                    .map(|u| u.id)
                    .collect();
                let r = self.renew(place, units, members);
                if note(&mut errors, "dead_left", r) {
                    *d.entry("dead_left").or_default() += 1;
                    *d.entry("put").or_default() += 1;
                }
                continue;
            }
            let key = match was {
                EntityState::Dead => "dead_replaced",
                EntityState::Lost => "lost_replaced",
                _ => "unspawnable_replaced",
            };
            let r = self.replace(place, units);
            if note(&mut errors, key, r) {
                *d.entry(key).or_default() += 1;
                *d.entry("removed").or_default() += 1;
                *d.entry("put").or_default() += 1;
            }
        }

        // Handover: the oldest live groups out, the next virtual ones in.
        let out: Vec<usize> = self
            .window
            .iter()
            .copied()
            .filter(|p| {
                groups
                    .get(&self.slots[*p].group)
                    .is_some_and(scenarios::live)
            })
            .filter(|p| self.killed.is_none_or(|(k, _)| k != *p))
            .take(plan.handover)
            .collect();
        if !out.is_empty() {
            let ids: Vec<u64> = out.iter().map(|p| self.slots[*p].group).collect();
            let r = self.api.materialize(self.c, &ids, false);
            if note(&mut errors, "Dematerialize", r) {
                self.window.retain(|p| !out.contains(p));
                *d.entry("dematerialized").or_default() += ids.len() as u64;
            }
        }
        let mut entering = Vec::new();
        for step in 0..self.slots.len() {
            // Refill the window, twice the handover at most: after a loss
            // the rest come in over the next steps.
            let short = plan.live.saturating_sub(self.window.len());
            if entering.len() >= short.min(plan.handover * 2) {
                break;
            }
            let p = (self.next_in + step) % self.slots.len();
            if groups
                .get(&self.slots[p].group)
                .is_some_and(|g| state(g) == EntityState::Virtual)
                && !out.contains(&p)
            {
                entering.push(p);
            }
        }
        if let Some(last) = entering.last() {
            self.next_in = (last + 1) % self.slots.len();
            let ids: Vec<u64> = entering.iter().map(|p| self.slots[*p].group).collect();
            let r = self.api.materialize(self.c, &ids, true);
            if note(&mut errors, "Materialize", r) {
                self.window.extend(entering.iter().copied());
                self.spawned_units += (ids.len() * units) as u64;
                *d.entry("materialized").or_default() += ids.len() as u64;
            }
        }

        if begin.elapsed() > slot {
            return counted(d, errors, Some("replacements"));
        }
        // Replace: the next oldest live groups are removed and created.
        let replace: Vec<usize> = self
            .window
            .iter()
            .copied()
            .filter(|p| {
                groups
                    .get(&self.slots[*p].group)
                    .is_some_and(scenarios::live)
            })
            .filter(|p| self.killed.is_none_or(|(k, _)| k != *p))
            .take(plan.replace)
            .collect();
        for place in replace {
            let r = self.replace(place, units);
            if note(&mut errors, "replace", r) {
                *d.entry("removed").or_default() += 1;
                *d.entry("put").or_default() += 1;
            }
        }

        if begin.elapsed() > slot {
            return counted(d, errors, Some("orders"));
        }
        // Orders: live groups not driving a route get one, round the window.
        let candidates: Vec<usize> = self
            .window
            .iter()
            .copied()
            .filter(|p| !driving(self.slots[*p].routed, n, plan))
            .filter(|p| {
                groups
                    .get(&self.slots[*p].group)
                    .is_some_and(scenarios::live)
            })
            .collect();
        for i in 0..plan.orders.min(candidates.len()) {
            let place = candidates[(self.order_next + i) % candidates.len()];
            let order = self.lap_order(place);
            let r = self.api.set_order(self.c, self.slots[place].group, order);
            if note(&mut errors, "SetOrder", r) {
                self.slots[place].routed = Some(n);
                *d.entry("orders").or_default() += 1;
            }
        }
        self.order_next = self.order_next.wrapping_add(plan.orders);

        if begin.elapsed() > slot {
            return counted(d, errors, Some("effects"));
        }
        // Effects: damage on units of live groups.
        let live_units: Vec<u64> = self
            .window
            .iter()
            .filter_map(|p| groups.get(&self.slots[*p].group))
            .filter(|g| scenarios::live(g))
            .flat_map(|g| g.units.iter().filter(|u| u.alive).map(|u| u.id))
            .collect();
        for _ in 0..plan.effects.min(live_units.len()) {
            let unit = live_units[(self.next_random() % live_units.len() as u64) as usize];
            let life = 50.0 + (self.next_random() % 40) as f64;
            let r = self.api.damage(self.c, unit, life).map(|_| ());
            if note(&mut errors, "DoEffect damage", r) {
                *d.entry("effects").or_default() += 1;
            }
        }

        if begin.elapsed() > slot {
            return counted(d, errors, Some("kill"));
        }
        // A kill: an explosion on each unit of the oldest standing group,
        // by the unit's DCS name through the scripting engine. The API's
        // `Explosion` effect is not built, and the deaths reach the platform
        // as DCS's own events either way.
        if plan.kill_every > 0 && n.is_multiple_of(plan.kill_every) && self.killed.is_none() {
            let target = self.window.iter().copied().find(|p| {
                !driving(self.slots[*p].routed, n, plan)
                    && groups
                        .get(&self.slots[*p].group)
                        .is_some_and(|g| scenarios::live(g) && g.units.iter().all(|u| u.alive))
            });
            if let Some(place) = target {
                let tag = names::tag_of(self.c);
                let unit_names: Vec<String> = groups[&self.slots[place].group]
                    .units
                    .iter()
                    .map(|u| names::name(Kind::Unit, tag, u.id))
                    .collect();
                match self.t.explode_units(&unit_names) {
                    Ok(found) => {
                        self.killed = Some((place, false));
                        *d.entry("kills").or_default() += 1;
                        *d.entry("explosions").or_default() += found as u64;
                    }
                    Err(e) => errors.push(json!(format!("kill: {e}"))),
                }
            }
        }

        if begin.elapsed() > slot {
            return counted(d, errors, Some("queries"));
        }
        // Queries, as a campaign asks what is near a point.
        let (pn, pe) = self.places[(n as usize * 7) % self.places.len()];
        let r = self
            .api
            .query_area(self.c, at(&self.theater, pn, pe, 0.0), 2_000.0)
            .map(|_| ());
        if note(&mut errors, "QueryArea", r) {
            *d.entry("queries").or_default() += 1;
        }
        counted(d, errors, None)
    }

    fn record(&mut self, collect: bool, export: bool, plan: &Plan) -> Value {
        let mut v = self.t.soak_probe(self.c, plan.fps_s);
        let mut errors = v["errors"].as_array().cloned().unwrap_or_default();
        if collect {
            match self.t.heap_floors() {
                Ok(Some(f)) => {
                    for (k, x) in f.as_object().into_iter().flatten() {
                        v["heap"][k] = x.clone();
                    }
                }
                Ok(None) => {}
                Err(e) => errors.push(json!(format!("collect: {e}"))),
            }
        }
        match self.t.agent_reports_after(Some(self.reports_seen)) {
            Ok(Some((reports, total))) => {
                self.reports_seen = total;
                for r in &reports {
                    let w = r["window_s"].as_f64().unwrap_or(10.0);
                    self.whole[0] += r["share"].as_f64().unwrap_or(0.0) * w;
                    self.whole[1] += w;
                    self.whole[2] += r["frames"].as_f64().unwrap_or(0.0);
                    self.whole[3] += r["frames_over_250"].as_f64().unwrap_or(0.0);
                }
                v["agent"] = agent_record(&reports);
            }
            Ok(None) => {}
            Err(e) => errors.push(json!(format!("agent reports: {e}"))),
        }
        v["agent_whole"] = json!({
            "share_percent": (self.whole[1] > 0.0).then(|| self.whole[0] / self.whole[1] * 100.0),
            "frames": self.whole[2],
            "frames_over_250_percent": (self.whole[2] > 0.0).then(|| self.whole[3] / self.whole[2] * 100.0),
        });
        v["ids"]["spawned_units"] = json!(self.spawned_units);
        match self.api.groups(self.c) {
            Ok(groups) => {
                let mut by: BTreeMap<&str, u64> = BTreeMap::new();
                for g in groups.values() {
                    *by.entry(match state(g) {
                        EntityState::Virtual => "virtual",
                        EntityState::Materializing => "materializing",
                        EntityState::Live => "live",
                        EntityState::Dematerializing => "dematerializing",
                        EntityState::Dead => "dead",
                        EntityState::Lost => "lost",
                        EntityState::Unspawnable => "unspawnable",
                        EntityState::Unspecified => "unspecified",
                    })
                    .or_default() += 1;
                }
                v["world"] = json!(by);
                v["world"]["groups"] = json!(groups.len());
            }
            Err(e) => errors.push(json!(format!("ListWorld: {e}"))),
        }
        match self.api.server_status() {
            Ok(s) => {
                v["api"] = json!({
                    "health": s.health, "sim_state": s.sim_state, "attached": s.attached,
                    "heartbeat_age_s": s.heartbeat_age.map(|d| d.seconds as f64 + f64::from(d.nanos) / 1e9),
                    "commands_queued": s.commands_queued, "event_gaps": s.event_gaps,
                    "store_free_bytes": s.store_free_bytes,
                    "last_backup_s": s.last_backup.map(|t| t.seconds),
                    "last_backup_failure_s": s.last_backup_failure.map(|t| t.seconds),
                    "backup_error": s.backup_error,
                });
                if let Some(z) = &s.store_size {
                    v["store_size"] = store_size_json(z);
                }
            }
            Err(e) => errors.push(json!(format!("GetServerStatus: {e}"))),
        }
        if let Some(streams) = &self.streams {
            let s = streams.snapshot();
            v["store"]["event_rows"] = s["event_sequence"].clone();
            v["streams"] = s;
        }
        if export {
            match self.api.export_counts(self.c) {
                Ok(e) => {
                    for (k, x) in e.as_object().into_iter().flatten() {
                        // `event_rows` is the subscription's, on every
                        // record: one source for one series.
                        let key = if k == "event_rows" {
                            "event_rows_export"
                        } else {
                            k.as_str()
                        };
                        v["store"][key] = x.clone();
                    }
                }
                Err(e) => errors.push(json!(format!("export: {e}"))),
            }
        }
        v["errors"] = json!(errors);
        v
    }

    fn identity(&mut self) -> Result<Option<String>, String> {
        self.t.identity()
    }

    fn recover(&mut self, _plan: &Plan) -> Result<(), String> {
        self.t.soak_recover()?;
        self.api = self
            .api
            .reconnect(&self.t.endpoint(), self.t.token().as_deref())?;
        self.killed = None;
        self.reports_seen = self
            .t
            .agent_reports_after(None)
            .ok()
            .flatten()
            .map_or(0, |(_, total)| total);
        Ok(())
    }

    fn inject_restart(&mut self) -> Result<(), String> {
        self.t.inject_restart()
    }

    fn stall(&mut self, seconds: f64) -> Result<(), String> {
        self.t.inject_stall(seconds)
    }
}

/// The campaign soak over a target whose sidecar runs.
fn run_campaign(
    rt: &Runtime,
    t: &mut dyn Target,
    plan: &Plan,
    folder: &Path,
    st: &mut State,
    tag: &str,
) -> Result<String, String> {
    let mut s = Campaign::setup(t, rt, plan, tag)?;
    let ended = drive(&mut s, plan, folder, st);
    s.streams = None;
    let end = end_checks(&mut s, plan, &ended);
    println!("== end checks: {end}");
    let _ = std::fs::write(
        folder.join("end.json"),
        serde_json::to_vec_pretty(&end).unwrap_or_default(),
    );
    s.api.release();
    Ok(ended)
}

/// What a record keeps of the status's `store_size` (spec/persistence.md,
/// "Size"); times in seconds since the Unix epoch, and `entities_held`, the
/// groups and statics the store holds in any state.
fn store_size_json(z: &pb::StoreSize) -> Value {
    json!({
        "groups_live": z.groups_live, "groups_dead": z.groups_dead, "ruins": z.ruins,
        "awaiting_purge": z.awaiting_purge, "units": z.units,
        "entities_held": z.groups_live + z.groups_dead + z.ruins + z.awaiting_purge,
        "oldest_removal_s": z.oldest_removal.map(|t| t.seconds),
        "journal_rows": z.journal_rows, "journal_bytes": z.journal_bytes,
        "oldest_journal_entry_s": z.oldest_journal_entry.map(|t| t.seconds),
        "event_rows": z.event_rows, "event_bytes": z.event_bytes,
        "oldest_event_s": z.oldest_event.map(|t| t.seconds),
        "store_file_bytes": z.store_file_bytes, "events_file_bytes": z.events_file_bytes,
        "purged": z.purged, "expired": z.expired, "purged_deaths": z.purged_deaths,
        "orphan_destroys": z.orphan_destroys, "work_set": z.work_set,
    })
}

/// After the churn: what DCS holds against what the store says, that no
/// member of a group left dead is in DCS again, and with
/// `Plan::reattach` the units without a life value after the sidecar
/// starts again over the campaign. Every check is a count; a failure to
/// read is in `errors`.
fn end_checks(s: &mut Campaign<'_, '_>, plan: &Plan, ended: &str) -> Value {
    let mut out = json!({});
    let mut errors: Vec<Value> = Vec::new();
    // The last step's handovers finish before the snapshot.
    std::thread::sleep(Duration::from_secs_f64(plan.interval_s.min(30.0)));
    match s.api.server_status() {
        Ok(st) => {
            out["status"] = json!({
                "store_size": st.store_size.as_ref().map(store_size_json),
                "last_backup_s": st.last_backup.map(|t| t.seconds),
                "last_backup_failure_s": st.last_backup_failure.map(|t| t.seconds),
                "backup_error": st.backup_error,
            });
        }
        Err(e) => errors.push(json!(format!("GetServerStatus: {e}"))),
    }
    let groups = match s.api.groups(s.c) {
        Ok(g) => g,
        Err(e) => {
            errors.push(json!(format!("ListWorld: {e}")));
            out["errors"] = json!(errors);
            return out;
        }
    };
    let tag = names::tag_of(s.c);
    let dead_names: Vec<String> = s
        .left_dead
        .iter()
        .flat_map(|(_, units)| units.iter().map(|u| names::name(Kind::Unit, tag, *u)))
        .collect();
    out["left_dead"] = json!({
        "groups": s.left_dead.len(),
        "still_listed": s.left_dead.iter().filter(|(g, _)| groups.contains_key(g)).count(),
    });
    out["dcs"] = dcs_now(s, &groups, &dead_names, &mut errors);
    // A run stopped by hand or by the memory guard is left as it is.
    if plan.reattach {
        out["reattach"] = if ended == "completed" {
            reattach(s, plan, &dead_names, &mut errors)
        } else {
            json!(format!("skipped: the run ended {ended}"))
        };
    }
    out["errors"] = json!(errors);
    out
}

/// The units of the groups in DCS that are alive, and those of them with no
/// life value.
fn lives(s: &mut Campaign<'_, '_>) -> Result<Value, String> {
    let g = s.api.groups(s.c)?;
    let units: Vec<&pb::Unit> = g
        .values()
        .filter(|g| in_dcs(g))
        .flat_map(|g| g.units.iter())
        .filter(|u| u.alive)
        .collect();
    Ok(json!({
        "units_alive": units.len(),
        "without_life": units.iter().filter(|u| u.life.is_none()).count(),
    }))
}

/// Start the sidecar again from its store, by the recovery path, and count
/// the units of the groups in DCS that are alive with no life value just
/// before, and 10 s and 60 s after it attached.
fn reattach(
    s: &mut Campaign<'_, '_>,
    plan: &Plan,
    dead_names: &[String],
    errors: &mut Vec<Value>,
) -> Value {
    let before = lives(s).unwrap_or_else(|e| json!(format!("ListWorld: {e}")));
    let t0 = Instant::now();
    if let Err(e) = s.recover(plan) {
        errors.push(json!(format!("restart: {e}")));
        return json!(null);
    }
    let deadline = Instant::now() + Duration::from_secs(300);
    let attached = loop {
        if s.api.server_status().is_ok_and(|st| st.attached) {
            break Instant::now();
        }
        if Instant::now() > deadline {
            errors.push(json!("the sidecar did not attach within 300 s"));
            return json!({ "attached": false });
        }
        std::thread::sleep(Duration::from_secs(1));
    };
    let mut out = json!({
        "before": before,
        "restart_to_attach_s": (attached - t0).as_secs_f64(),
    });
    for at in [10u64, 60] {
        let due = attached + Duration::from_secs(at);
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        match lives(s) {
            Ok(v) => out[format!("after_{at}_s")] = v,
            Err(e) => errors.push(json!(format!("ListWorld after {at} s: {e}"))),
        }
    }
    // The recovery path reconciles the store against DCS: what it holds
    // after, a dead group spawned again included.
    match s.api.groups(s.c) {
        Ok(g) => out["dcs"] = dcs_now(s, &g, dead_names, errors),
        Err(e) => errors.push(json!(format!("ListWorld after the restart: {e}"))),
    }
    out
}

/// DCS's groups against the store's, as [`compare_dcs`] counts them.
fn dcs_now(
    s: &mut Campaign<'_, '_>,
    groups: &BTreeMap<u64, pb::Group>,
    dead_names: &[String],
    errors: &mut Vec<Value>,
) -> Value {
    match s.t.dcs_groups() {
        Ok(Some(dcs)) => compare_dcs(groups, &dcs, names::tag_of(s.c), dead_names),
        Ok(None) => json!("not read on this target"),
        Err(e) => {
            errors.push(json!(format!("DCS groups: {e}")));
            json!(null)
        }
    }
}

/// The campaign's groups DCS holds against what `ListWorld` says: those
/// with no row, those listed in a state out of DCS, live groups DCS does
/// not hold, and units of groups left dead that DCS holds again. Groups of
/// other campaigns and of no campaign are left out.
fn compare_dcs(
    groups: &BTreeMap<u64, pb::Group>,
    dcs: &[scenarios::DcsGroup],
    tag: names::Tag,
    dead_names: &[String],
) -> Value {
    let mine: Vec<(u64, &scenarios::DcsGroup)> = dcs
        .iter()
        .filter_map(|g| {
            names::parse(&g.name)
                .filter(|p| p.tag == tag && p.kind == Kind::Group)
                .map(|p| (p.entity, g))
        })
        .collect();
    let held: std::collections::BTreeSet<u64> = mine.iter().map(|(id, _)| *id).collect();
    let no_row: Vec<u64> = held
        .iter()
        .copied()
        .filter(|id| !groups.contains_key(id))
        .collect();
    let out_of_dcs: Vec<u64> = held
        .iter()
        .copied()
        .filter(|id| groups.get(id).is_some_and(|g| !in_dcs(g)))
        .collect();
    let live_missing: Vec<u64> = groups
        .iter()
        .filter(|(id, g)| scenarios::live(g) && !held.contains(id))
        .map(|(id, _)| *id)
        .collect();
    let respawned = mine
        .iter()
        .flat_map(|(_, g)| g.units.iter())
        .filter(|u| dead_names.contains(&u.name))
        .count();
    json!({
        "groups_of_the_campaign": held.len(),
        "without_a_row": no_row,
        "listed_out_of_dcs": out_of_dcs,
        "live_not_in_dcs": live_missing,
        "dead_members_in_dcs": respawned,
    })
}

// ---------------------------------------------------------------------------
// The control: DCS alone, no agent loaded.

struct Control<'a> {
    run: &'a Run,
    name: &'a str,
    shell: &'a [u8],
    places: Vec<(f64, f64)>,
    boot: Option<String>,
    cpu_before: BTreeMap<&'static str, (f64, f64)>,
    agent_log_lines: usize,
}

/// DCS's frame rate without the agent (`lua/sampling/fps-control.lua`),
/// with `@MEASURE@` to fill in.
const CONTROL_FPS: &str = include_str!("../../../../lua/sampling/fps-control.lua");

/// The agent log's line count: it stays where it was while no agent runs.
fn agent_log_lines(run: &Run) -> usize {
    let log = run.remote.full_path("nanoprobe/logs/agent.log");
    run.sh("wc -l < \"$1\" 2>/dev/null || echo 0", &[&log])
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o).trim().parse().ok())
        .unwrap_or(0)
}

impl Control<'_> {
    /// Load the shell mission through the eval hook and wait until it runs.
    /// The development loader's wait reads the agent hook's log, which no
    /// agent writes here.
    fn load_shell(&self) -> Result<(), String> {
        use crate::install::Profile as _;
        self.run
            .remote
            .write("nanoprobe/dev/missions/np-soak-control.miz", self.shell)?;
        let v = self.run.eval(
            "hook",
            "soak-load",
            "local w = lfs.writedir()\nlocal s = w:find('\\\\', 1, true) and '\\\\' or '/'\n\
             return net.load_mission(w .. table.concat({ 'nanoprobe', 'dev', 'missions', 'np-soak-control.miz' }, s))",
            120,
        )?;
        if v == json!(false) {
            return Err("net.load_mission refused the shell mission".into());
        }
        let deadline = Instant::now() + Duration::from_secs(900);
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let name = self
                .run
                .eval("hook", "soak-mission", "return DCS.getMissionName()", 60);
            if name.is_ok_and(|n| n.as_str().is_some_and(|n| n.contains("np-soak-control"))) {
                break;
            }
            if Instant::now() > deadline {
                return Err("the shell mission did not start within 15 minutes".into());
            }
        }
        self.run.hold(3.0)
    }

    fn fill(&self, plan: &Plan) -> Result<(), String> {
        let list: Vec<String> = self
            .places
            .iter()
            .map(|(n, e)| format!("{{{n}, {e}}}"))
            .collect();
        let v = self.run.eval(
            "server",
            "soak-setup",
            &format!(
                "local E = ...\nlocal K = E.lib('soak')\n\
                 return K.setup{{ places = {{ {} }}, live = {}, units = {}, type = {GROUND:?}, country = {COUNTRY}, \
                 spacing = 12, loop = {}, speed = 5 }}\n",
                list.join(", "),
                plan.live,
                plan.units,
                SPACING * 2.0 / 3.0,
            ),
            600,
        )?;
        let want = (plan.live * plan.units) as u64;
        match v["units"].as_u64() {
            Some(n) if n == want => Ok(()),
            _ => Err(format!("the control's setup holds {v}, not {want} units")),
        }
    }
}

impl Subject for Control<'_> {
    fn tick(&mut self, n: u64, plan: &Plan) -> Value {
        let kill = plan.kill_every > 0 && n.is_multiple_of(plan.kill_every);
        match self.run.eval(
            "server",
            "soak-tick",
            &format!(
                "local E = ...\nlocal K = E.lib('soak')\n\
                 return K.tick{{ handover = {}, replace = {}, kill = {kill}, orders = {} }}\n",
                plan.handover, plan.replace, plan.orders
            ),
            120,
        ) {
            Ok(v) => json!({
                "steps": 1,
                "spawned_units": v["spawned_units"], "destroyed_units": v["destroyed_units"],
                "killed_units": v["killed_units"], "orders": v["orders"],
                "errors": v["errors"].as_array().cloned().unwrap_or_default(),
            }),
            Err(e) => json!({ "steps_failed": 1, "errors": [format!("tick: {e}")] }),
        }
    }

    fn record(&mut self, collect: bool, _export: bool, plan: &Plan) -> Value {
        // `lua/sampling/fps.lua` counts the agent's frames and so loads the
        // agent: the control reads its frame rate without it.
        let mut v = probe(self.run, "", "ctl-", 0, &mut self.cpu_before);
        let mut errors = v["errors"].as_array().cloned().unwrap_or_default();
        if plan.fps_s > 0 {
            let source = CONTROL_FPS.replace("@MEASURE@", &plan.fps_s.to_string());
            match self
                .run
                .eval("hook", "soak-fps", &source, u64::from(plan.fps_s) + 180)
            {
                Ok(f) => {
                    for key in ["fps", "p5_fps", "model_per_real"] {
                        v["dcs"][key] = f[key].clone();
                    }
                }
                Err(e) => errors.push(json!(format!("fps: {e}"))),
            }
        }
        match self.run.eval(
            "server",
            "soak-stats",
            "local E = ...\nreturn E.lib('soak').stats()",
            60,
        ) {
            Ok(s) => v["ids"]["spawned_units"] = s["spawned_units_total"].clone(),
            Err(e) => errors.push(json!(format!("stats: {e}"))),
        }
        if collect {
            match heap_floors(self.run) {
                Ok(f) => {
                    for (k, x) in f.as_object().into_iter().flatten() {
                        v["heap"][k] = x.clone();
                    }
                }
                Err(e) => errors.push(json!(format!("collect: {e}"))),
            }
        }
        // No agent: its log must not grow.
        v["agent_log_lines_added"] =
            json!(agent_log_lines(self.run).saturating_sub(self.agent_log_lines));
        v["errors"] = json!(errors);
        v
    }

    fn identity(&mut self) -> Result<Option<String>, String> {
        Ok(identity_of(&processes(self.run, "")?, "dcs"))
    }

    fn recover(&mut self, plan: &Plan) -> Result<(), String> {
        self.run.eval("hook", "soak-up", "return 1", 60)?;
        if self.run.remote.players()? > 0 {
            return Err("a player is connected; the shell mission is not loaded".into());
        }
        self.load_shell()?;
        self.fill(plan)?;
        self.cpu_before.clear();
        self.boot = identity_of(&processes(self.run, "")?, "dcs");
        Ok(())
    }
}

/// The control on a docker-wine target: deploy with `--dev` for the eval
/// channel and the scenario library, take the agent's loader out of the
/// profile, restart DCS so no agent is loaded, churn, and put the loader
/// back. The kill switch is not used: the eval hook honors it too.
fn control_on_server(
    repository: &Path,
    name: &str,
    plan: &Plan,
) -> Result<(PathBuf, String), String> {
    use crate::install::{LOADER, Profile as _};
    let (run, shell) = live::open_eval(repository, name, "soak-control")?;
    let folder = run.folder.clone();
    let mut st = State::new(&folder, "control", name, plan);
    println!("== control: folder {}", folder.display());
    crate::deploy(name, None, true, false)?;
    let loader = run
        .remote
        .read(LOADER)?
        .ok_or("the profile has no agent loader")?;
    // Kept here too: a run that dies leaves the profile without it.
    std::fs::write(folder.join("nanoprobe-hook.lua.saved"), &loader).map_err(|e| e.to_string())?;
    let result = (|| {
        run.remote.remove_file(LOADER)?;
        let (remote, ssh, command) = crate::remote(name)?;
        let command = command.ok_or_else(|| format!("target {name:?} has no `restart` key"))?;
        if remote.players()? > 0 {
            return Err("a player is connected; not restarting".to_owned());
        }
        println!("== restart without the agent's loader");
        let lines_before = agent_log_lines(&run);
        let out = crate::ssh::run(&ssh, &command, b"")?;
        if out.code != Some(0) {
            return Err(format!("the restart command failed: {}", out.stderr));
        }
        let deadline = Instant::now() + Duration::from_secs(900);
        while run.eval("hook", "soak-up", "return 1", 30).is_err() {
            if Instant::now() > deadline {
                return Err("the eval hook did not answer within 15 minutes of the restart".into());
            }
            std::thread::sleep(Duration::from_secs(5));
        }
        let mut c = Control {
            run: &run,
            name,
            shell: &shell,
            places: Vec::new(),
            boot: None,
            cpu_before: BTreeMap::new(),
            agent_log_lines: lines_before,
        };
        c.load_shell()?;
        c.agent_log_lines = agent_log_lines(&run);
        // The campaign's places: the same land grid.
        c.places = control_places(&run, plan.world)?;
        c.fill(plan)?;
        let _ = c.name;
        Ok(drive(&mut c, plan, &folder, &mut st))
    })();
    let cleared = run.eval(
        "server",
        "soak-clear",
        "local E = ...\nreturn E.lib('soak').clear()",
        300,
    );
    // The loader goes back whatever the run did, and a failure to write it
    // is reported with the run's own.
    let not_back = run.remote.write(LOADER, &loader).err().map(|e| {
        format!(
            "the agent's loader was not written back to the profile ({e}); its copy is {}",
            folder.join("nanoprobe-hook.lua.saved").display()
        )
    });
    let ended = match (result, not_back) {
        (Ok(ended), None) => ended,
        (Ok(_), Some(l)) => return Err(l),
        (Err(e), None) => return Err(e),
        (Err(e), Some(l)) => return Err(format!("{e}; and {l}")),
    };
    println!(
        "== the agent's loader is back in the profile; DCS runs without the agent until it restarts"
    );
    if let Err(e) = cleared {
        println!("   the control's groups were not cleared: {e}");
    }
    finish(&folder, name, "control", plan, &ended, None, &mut st)?;
    Ok((folder, ended))
}

/// The first `n` places of the land grid the campaign uses, checked through
/// the eval channel.
fn control_places(run: &Run, n: usize) -> Result<Vec<(f64, f64)>, String> {
    let side = ((n as f64).sqrt().ceil() as usize) * 3 / 2;
    let cand: Vec<(f64, f64)> = (0..side * side)
        .map(|i| {
            (
                ORIGIN.0 + SPACING * (i / side) as f64,
                ORIGIN.1 + SPACING * (i % side) as f64,
            )
        })
        .collect();
    // In chunks, as `virtualization::places` asks: the eval channel's reply
    // holds at most 2,000 entries of an array (spec/eval.md).
    let mut out = Vec::with_capacity(n);
    for chunk in cand.chunks(LAND_CHUNK) {
        let land = super::on_land(run, chunk)?;
        out.extend(chunk.iter().zip(land).filter(|(_, l)| *l).map(|(p, _)| *p));
        if out.len() >= n {
            out.truncate(n);
            return Ok(out);
        }
    }
    if out.len() < n {
        return Err(format!("only {} of {n} grid places are on land", out.len()));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The verdict.

/// The owner's targets (GOAL.md, criterion 1): the agent's mean share of
/// real time, and the share of frames in which it took over 250 µs (a 99th
/// percentile of at most 250 µs is at most 1 % of frames over it).
const SHARE_PERCENT: f64 = 0.3;
/// The shortest campaign soak that is a verdict (the owner's ruling on the
/// soak's length; PLAN revision 38).
const SOAK_HOURS: f64 = 8.0;
const OVER_250_PERCENT: f64 = 1.0;

/// The series a live campaign soak must have data for. Without one of them
/// the run does not pass.
fn required(kind: &str) -> Vec<&'static str> {
    match kind {
        "rehearsal" => vec![
            "sidecar_rss_kb",
            "sidecar_fds",
            "store_bytes",
            "events_bytes",
            "journal_rows",
            "event_rows",
            "world_groups",
        ],
        "control" => vec![
            "heap_server_floor_kib",
            "heap_hook_floor_kib",
            "dcs_rss_kb",
            "container_working_set_kb",
            "dcs_fds",
            "dcs_threads",
            "wineserver_fds",
            "wineserver_rss_kb",
        ],
        _ => series::DEFS.iter().map(|d| d.key).collect(),
    }
}

fn mean(records: &[Value], pointer: &str) -> Option<f64> {
    let v: Vec<f64> = records
        .iter()
        .filter_map(|r| r.pointer(pointer).and_then(Value::as_f64))
        .collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn lowest(records: &[Value], pointer: &str) -> Option<f64> {
    records
        .iter()
        .filter_map(|r| r.pointer(pointer).and_then(Value::as_f64))
        .fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x))))
}

/// Growth per hour of a cumulative figure, from the first record after
/// `after_s` that has it to the last. The warm-up's start-up burst is not an
/// hour's growth.
fn per_hour(records: &[Value], pointer: &str, after_s: f64) -> Option<f64> {
    let v: Vec<(f64, f64)> = records
        .iter()
        .filter_map(|r| Some((r["t_s"].as_f64()?, r.pointer(pointer)?.as_f64()?)))
        .filter(|(t, _)| *t > after_s)
        .collect();
    let (first, last) = (v.first()?, v.last()?);
    (last.0 > first.0).then(|| (last.1 - first.1) / (last.0 - first.0) * 3600.0)
}

/// The churn a step of a run of `kind` plans, by counter of the records'
/// `churn` totals. The control's Lua churn counts units, not groups, so it is
/// checked by its steps and routes.
fn planned_churn(kind: &str, plan: &Plan) -> Vec<(&'static str, f64)> {
    let kill = if plan.kill_every > 0 {
        1.0 / plan.kill_every as f64
    } else {
        0.0
    };
    let (handover, replace) = (plan.handover as f64, plan.replace as f64);
    let per_step = match kind {
        "campaign" | "rehearsal" => vec![
            ("steps", 1.0),
            ("dematerialized", handover),
            ("materialized", handover),
            ("removed", replace),
            ("put", replace),
            ("orders", plan.orders as f64),
            ("effects", plan.effects as f64),
            ("queries", 1.0),
            ("kills", kill),
        ],
        "control" => vec![("steps", 1.0), ("orders", plan.orders as f64)],
        _ => Vec::new(),
    };
    per_step.into_iter().filter(|(_, r)| *r > 0.0).collect()
}

/// The span the achieved churn is counted over, seconds.
const CHURN_BLOCK_S: f64 = 1_800.0;
/// The share of the planned rate a block must reach.
const CHURN_SHARE: f64 = 0.8;
/// Consecutive short blocks that make a stretch. A stall or a DCS restart
/// of up to 30 minutes touches at most two half hours and the run rides
/// through it; 90 minutes under the plan is a churn that no longer runs as
/// planned.
const CHURN_SHORT_BLOCKS: usize = 3;

/// Where the churn a run achieved fell short of its plan: each kind's count
/// over consecutive half hours against the planned rate, and the stretches of
/// at least [`CHURN_SHORT_BLOCKS`] blocks under [`CHURN_SHARE`] of it. A kind
/// the plan has and no record counts is short too. A run shorter than
/// [`CHURN_SHORT_BLOCKS`] blocks (90 minutes) is not checked.
fn churn_short(kind: &str, plan: &Plan, records: &[Value]) -> Vec<Value> {
    let times: Vec<f64> = records.iter().filter_map(|r| r["t_s"].as_f64()).collect();
    let (Some(&t0), Some(&t1)) = (times.first(), times.last()) else {
        return Vec::new();
    };
    let blocks = ((t1 - t0) / CHURN_BLOCK_S).floor() as usize;
    if blocks < CHURN_SHORT_BLOCKS {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (counter, per_step) in planned_churn(kind, plan) {
        let planned = per_step * 3600.0 / plan.tick_s;
        let points: Vec<(f64, f64)> = records
            .iter()
            .filter_map(|r| Some((r["t_s"].as_f64()?, r["churn"][counter].as_f64()?)))
            .collect();
        if points.is_empty() {
            out.push(
                json!({ "kind": counter, "planned_per_hour": planned, "achieved_per_hour": null,
                "from_s": t0, "to_s": t1, "why": "no record counts it" }),
            );
            continue;
        }
        // The count at time t: the last record at or before it, else 0.
        let at = |t: f64| {
            points
                .iter()
                .take_while(|p| p.0 <= t + 1e-6)
                .last()
                .map_or(0.0, |p| p.1)
        };
        let mut run: Option<(f64, f64, usize)> = None;
        let close = |run: &mut Option<(f64, f64, usize)>, out: &mut Vec<Value>| {
            if let Some((from, count, n)) = run.take()
                && n >= CHURN_SHORT_BLOCKS
            {
                let to = from + n as f64 * CHURN_BLOCK_S;
                out.push(json!({ "kind": counter, "planned_per_hour": planned,
                    "achieved_per_hour": count / (to - from) * 3600.0, "from_s": from, "to_s": to,
                    "why": format!("under {:.0} % of the plan for {} half hours", CHURN_SHARE * 100.0, n) }));
            }
        };
        for b in 0..blocks {
            let a = t0 + b as f64 * CHURN_BLOCK_S;
            let count = at(a + CHURN_BLOCK_S) - at(a);
            if count / CHURN_BLOCK_S * 3600.0 < CHURN_SHARE * planned {
                let r = run.get_or_insert((a, 0.0, 0));
                r.1 += count;
                r.2 += 1;
            } else {
                close(&mut run, &mut out);
            }
        }
        close(&mut run, &mut out);
    }
    out
}

/// The verdict of a run's records, against a control's when there is one.
fn verdict(
    kind: &str,
    plan: &Plan,
    records: &[Value],
    control: Option<&[Value]>,
    ended: &str,
) -> Value {
    let mut caps = BTreeMap::new();
    // 80 % of the container's memory limit: with no swap the limit kills
    // DCS, and the rest is room for what a day brings that a run did not.
    if let Some(limit) = records
        .iter()
        .find_map(|r| r["container"]["limit_kb"].as_f64())
    {
        caps.insert("container_working_set_kb".to_owned(), 0.8 * limit);
    }
    // A day of the store and of the event history each stay under 5 % of
    // the free space the store's file system had at the start.
    if let Some(free) = records
        .iter()
        .find_map(|r| r["api"]["store_free_bytes"].as_f64())
    {
        caps.insert("store_bytes".to_owned(), 0.05 * free);
        caps.insert("events_bytes".to_owned(), 0.05 * free);
    }
    let control_warmup_s = control
        .and_then(|c| c.iter().filter_map(|r| r["t_s"].as_f64()).reduce(f64::max))
        .map_or(plan.warmup_s(), |end| (end / 4.0).min(3_600.0));
    let options = series::Options {
        warmup_s: plan.warmup_s(),
        control_warmup_s,
        block_s: plan.block_s(),
        caps,
    };
    let mut v = series::judge(series::DEFS, records, control, &options);
    let missing: Vec<&str> = required(kind)
        .into_iter()
        .filter(|k| v["series"][*k]["verdict"] == json!("no data"))
        .collect();
    let whole = records
        .iter()
        .rev()
        .find(|r| r["agent_whole"]["frames"].as_f64().is_some_and(|f| f > 0.0))
        .map_or(Value::Null, |r| r["agent_whole"].clone());
    let targets = json!({
        "share_percent": whole["share_percent"],
        "share_limit_percent": SHARE_PERCENT,
        "frames_over_250_percent": whole["frames_over_250_percent"],
        "frames_over_250_limit_percent": OVER_250_PERCENT,
        "met": whole["share_percent"].as_f64().is_none_or(|s| s <= SHARE_PERCENT)
            && whole["frames_over_250_percent"].as_f64().is_none_or(|s| s <= OVER_250_PERCENT),
        "measured": whole["share_percent"].is_number(),
    });
    let hours = records
        .last()
        .and_then(|r| r["t_s"].as_f64())
        .unwrap_or(0.0)
        / 3600.0;
    let churn = records.last().map_or(Value::Null, |r| r["churn"].clone());
    v["owner_targets"] = targets.clone();
    v["missing_required"] = json!(missing);
    v["health"] = json!({
        "fps_mean": mean(records, "/dcs/fps"), "fps_lowest": lowest(records, "/dcs/fps"),
        "p5_fps_lowest": lowest(records, "/dcs/p5_fps"),
        "model_per_real_mean": mean(records, "/dcs/model_per_real"),
        "dcs_cpu_percent_mean": mean(records, "/dcs/cpu_percent"),
        "records_with_players": records.iter().filter(|r| r["players"].as_u64().unwrap_or(0) > 0).count(),
        "records_with_errors": records.iter().filter(|r| r["errors"].as_array().is_some_and(|e| !e.is_empty())).count(),
        "segment_ends": records.iter().filter(|r| r["segment_end"] == json!(true)).count(),
        "world_lost_highest": records.iter().filter_map(|r| r["world"]["lost"].as_u64()).max(),
    });
    v["history"] = json!({
        "hours": hours,
        "per_hour_after_s": plan.warmup_s(),
        "churn": churn,
        "journal_rows_per_hour": per_hour(records, "/store/journal_rows", plan.warmup_s()),
        "journal_json_bytes_per_hour": per_hour(records, "/store/journal_json_bytes", plan.warmup_s()),
        "store_bytes_per_hour": per_hour(records, "/store/store_bytes", plan.warmup_s()),
        "event_rows_per_hour": per_hour(records, "/store/event_rows", plan.warmup_s()),
        "events_bytes_per_hour": per_hour(records, "/store/events_bytes", plan.warmup_s()),
    });
    // A series DCS itself moves (its heap, its memory, its descriptors) has
    // no verdict on the platform without a control to take DCS's own slope
    // away, whatever it reads alone: it needs its control. It fails nothing
    // and the run is not conclusive.
    let needs_control: Vec<Value> = series::DEFS
        .iter()
        .filter(|d| {
            let s = &v["series"][d.key];
            s["control_missing"] == json!(true) && s["verdict"] != json!("no data")
        })
        .map(|d| json!(d.key))
        .collect();
    let growing: Vec<Value> = v["growing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|k| !needs_control.contains(k))
        .cloned()
        .collect();
    // Settled series pass: their growth ended inside the run, so they are
    // neither unresolved nor rising at its end. They are listed, and the
    // results say what filled.
    let settled: Vec<Value> = series::DEFS
        .iter()
        .filter(|d| v["series"][d.key]["verdict"] == json!("settled"))
        .map(|d| json!(d.key))
        .collect();
    // The soak the plan asks for runs to its end and for at least
    // [`SOAK_HOURS`]: a shorter campaign run, or one that was stopped, is
    // not the verdict.
    // The length that counts is the fitted segment's: a restart in the
    // middle of 8 hours leaves two halves, and the fit reads one.
    let fitted_hours = v["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["segment"] == v["segment_used"])
        .and_then(|s| Some(s["to_s"].as_f64()? - s["from_s"].as_f64()?))
        .map_or(0.0, |span| (span + plan.interval_s) / 3600.0);
    let short_run =
        kind == "campaign" && (fitted_hours < SOAK_HOURS - 1e-6 || ended != "completed");
    let over_cap = v["over_cap"].as_array().is_some_and(|a| !a.is_empty());
    // A run the memory guard ended came within a tenth of the limit that
    // kills DCS: that is a failure, whatever the fitted line projects.
    let memory_guard = ended == "memory guard";
    let ok = growing.is_empty() && !over_cap && !memory_guard && targets["met"] == json!(true);
    // A churn that fell short of its plan for 90 minutes or more did not test
    // what the run was planned to test, so the run is no verdict and extends
    // (EXTEND, not FAIL): the records show nothing wrong with the platform,
    // only that part of the load was missing. The stretches are listed.
    let churn = churn_short(kind, plan, records);
    v["conclusive"] = json!(
        v["extend"] == json!(false)
            && missing.is_empty()
            && needs_control.is_empty()
            && !short_run
            && churn.is_empty()
    );
    v["churn_short"] = json!(churn);
    v["growing"] = json!(growing);
    v["needs_control"] = json!(needs_control);
    v["settled"] = json!(settled);
    v["short_run"] = json!(short_run);
    v["fitted_hours"] = json!(fitted_hours);
    v["memory_guard"] = json!(memory_guard);
    v["ok"] = json!(ok);
    v
}

/// PASS, FAIL, or EXTEND when nothing failed and a series is unresolved,
/// rising, has no data or lacks its control, the run was short, or its churn
/// fell short of the plan.
fn outcome(v: &Value) -> (&'static str, u8) {
    match (v["ok"] == json!(true), v["conclusive"] == json!(true)) {
        (false, _) => ("FAIL", 1),
        (true, false) => ("EXTEND", 2),
        (true, true) => ("PASS", 0),
    }
}

/// The sidecar's commit timing lines (`nanoprobe_api::timings`) in the log
/// the run saved, by quarter of the run: the mean of the lines' median and
/// 99th percentile commit, the longest commit and the mean checkpoint, in
/// microseconds. A store that slows with the run shows here.
fn commit_timings(folder: &Path) -> Value {
    let text: String = ["nanoprobe-xtask-persistence.log", "fake-sidecar.log"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(folder.join(f)).ok())
        .collect();
    let lines: Vec<Value> = text
        .lines()
        .filter(|l| l.contains("\"ev\":\"commit_timings\""))
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect();
    let quarter = lines.len().div_ceil(4).max(1);
    let quarters: Vec<Value> = lines
        .chunks(quarter)
        .map(|c| {
            let mean =
                |k: &str| c.iter().filter_map(|l| l[k].as_f64()).sum::<f64>() / c.len() as f64;
            let checkpoints: Vec<f64> = c
                .iter()
                .flat_map(|l| l["checkpoints"].as_array().cloned().unwrap_or_default())
                .filter_map(|x| x.as_f64())
                .collect();
            json!({
                "lines": c.len(),
                "median_us": mean("median_us"),
                "p99_us": mean("p99_us"),
                "max_us": c.iter().filter_map(|l| l["max_us"].as_f64()).fold(0.0, f64::max),
                "checkpoint_us": (!checkpoints.is_empty())
                    .then(|| checkpoints.iter().sum::<f64>() / checkpoints.len() as f64),
                "units": c.last().map(|l| l["units"].clone()),
            })
        })
        .collect();
    json!({ "lines": lines.len(), "quarters": quarters })
}

/// The shortest control that is a verdict's baseline (PLAN revision 38).
const CONTROL_HOURS: f64 = 2.0;

/// Whether the folder holds a control a run of `kind` on `target` can be
/// judged against, and what is wrong with it. A folder that is no control,
/// one of another target, and one during which an agent ran are not used at
/// all: their series say nothing of DCS alone. A control that is short or
/// did not run to its end is used, and the verdict is not conclusive. A
/// pilot's ten-minute control is judged against by a pilot the same way.
fn control_check(folder: &Path, target: &str, kind: &str) -> (bool, Vec<String>) {
    let read = |file: &str| -> Option<Value> {
        std::fs::read_to_string(folder.join(file))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    };
    let Some(c) = read("soak.json").or_else(|| read("state.json")) else {
        return (false, vec![format!("{} holds no run", folder.display())]);
    };
    let mut faults = Vec::new();
    let mut usable = true;
    if c["kind"] != json!("control") {
        // A rehearsal has no control run: any rehearsal folder stands in.
        if kind != "rehearsal" {
            usable = false;
            faults.push(format!("it is a {} run, not a control", c["kind"]));
        }
    } else if c["target"].as_str() != Some(target) {
        usable = false;
        faults.push(format!("it ran on {}, not on {target}", c["target"]));
    }
    let records = read_jsonl(&folder.join("records.jsonl"));
    if let Some(r) = records
        .iter()
        .find(|r| r["agent_log_lines_added"].as_u64().unwrap_or(0) > 0)
    {
        usable = false;
        faults.push(format!(
            "the agent's log grew during it (by the record at {} s): an agent ran",
            r["t_s"]
        ));
    }
    if kind == "campaign" {
        let hours = records
            .last()
            .and_then(|r| r["t_s"].as_f64())
            .unwrap_or(0.0)
            / 3600.0;
        if hours < CONTROL_HOURS - 1e-6 {
            faults.push(format!("it ran {hours:.2} hours, not {CONTROL_HOURS}"));
        }
        if c["ended"].as_str().is_some_and(|e| e != "completed") || c["ended"].is_null() {
            faults.push(format!("it ended: {}", c["ended"]));
        }
    }
    (usable, faults)
}

/// The verdict's churn stretches as lines for `verdict.md`, or nothing.
fn churn_lines(v: &Value) -> String {
    let lines: Vec<String> = v["churn_short"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            format!(
                "- churn short: `{}` from {} s to {} s, {} an hour against {} planned ({})\n",
                s["kind"].as_str().unwrap_or("?"),
                s["from_s"],
                s["to_s"],
                s["achieved_per_hour"]
                    .as_f64()
                    .map_or("none".to_owned(), |a| format!("{a:.0}")),
                s["planned_per_hour"]
                    .as_f64()
                    .map_or("?".to_owned(), |p| format!("{p:.0}")),
                s["why"].as_str().unwrap_or(""),
            )
        })
        .collect();
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.concat())
    }
}

/// Write the run's verdict and `soak.json` from its records.
fn finish(
    folder: &Path,
    name: &str,
    kind: &str,
    plan: &Plan,
    ended: &str,
    control: Option<&Path>,
    st: &mut State,
) -> Result<u8, String> {
    let records = read_jsonl(&folder.join("records.jsonl"));
    let (control_records, control_faults) = match control {
        Some(c) => {
            let (usable, faults) = control_check(c, name, kind);
            (usable.then(|| read_jsonl(&c.join("records.jsonl"))), faults)
        }
        None => (None, Vec::new()),
    };
    let mut v = verdict(kind, plan, &records, control_records.as_deref(), ended);
    if !control_faults.is_empty() {
        v["control_invalid"] = json!(control_faults);
        v["conclusive"] = json!(false);
    }
    // A control is the baseline: it has no verdict of its own, unless the
    // agent's log grew during it, which means an agent ran and it is no
    // control.
    let agent_ran = records
        .iter()
        .find(|r| r["agent_log_lines_added"].as_u64().unwrap_or(0) > 0);
    let (word, code) = match (kind, agent_ran) {
        ("control", Some(r)) => {
            v["control_invalid"] = json!([format!(
                "the agent's log grew by {} lines by the record at {} s: an agent ran",
                r["agent_log_lines_added"], r["t_s"]
            )]);
            ("CONTROL-INVALID", 1)
        }
        ("control", None) => ("CONTROL", 0),
        _ => outcome(&v),
    };
    let write = |file: &str, text: String| {
        let path = folder.join(file);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    };
    write(
        "verdict.json",
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )?;
    write(
        "verdict.md",
        format!(
            "{word}: {kind} on {name}, {} records, ended: {ended}\n\n{}{}\n",
            records.len(),
            churn_lines(&v),
            series::table(&v)
        ),
    )?;
    let timings = commit_timings(folder);
    // A run judged again (`soak judge`) keeps what its first verdict wrote of
    // the run itself: the build, DCS and the times are the run's, not those
    // of the host or the target when it is judged again.
    let before: Option<Value> = std::fs::read_to_string(folder.join("soak.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let (provenance, ended_at, judged_at) = match &before {
        Some(b) if !b["provenance"].is_null() => (
            b["provenance"].clone(),
            b["ended_at"].clone(),
            json!(bundle::utc_now()),
        ),
        _ => (
            if kind == "rehearsal" {
                json!({ "platform": "fake sidecar on this host" })
            } else {
                crate::soak::provenance(name)
            },
            json!(bundle::utc_now()),
            Value::Null,
        ),
    };
    write(
        "soak.json",
        serde_json::to_string_pretty(&json!({
            "kind": kind, "target": name, "provenance": provenance, "plan": plan.json(),
            "started": st.v["started"], "ended_at": ended_at, "judged_again_at": judged_at,
            "ended": ended, "records": records.len(),
            "control": control.map(|c| c.display().to_string()),
            "outcome": word, "verdict": v, "commit_timings": timings,
            "events": read_jsonl(&folder.join("events.jsonl")),
        }))
        .unwrap_or_default(),
    )?;
    st.set("phase", json!("done"));
    st.set("outcome", json!(word));
    println!("{}", series::table(&v));
    println!("== {word}: {}", folder.join("soak.json").display());
    Ok(code)
}

// ---------------------------------------------------------------------------
// Commands.

fn runtime() -> Result<Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

/// The campaign soak on a docker-wine target. Returns the folder and how
/// the run ended.
fn campaign_on_server(
    repository: &Path,
    name: &str,
    plan: &Plan,
    control: Option<&Path>,
) -> Result<(PathBuf, u8), String> {
    let Profile::DockerWine { ssh, container, .. } = crate::target(name)? else {
        return Err("soak runs on a docker-wine target".into());
    };
    let rt = runtime()?;
    let live::Opened { run, shell, .. } = live::open(repository, name, "soak")?;
    let host = Host { ssh, container };
    let folder = run.folder.clone();
    let mut st = State::new(&folder, "campaign", name, plan);
    println!("== campaign soak: folder {}", folder.display());
    let tag = bundle::stamp();
    let mut extra = json!({});
    let mut ended = String::new();
    // Two scheduled backups in 8 hours, as a day of a campaign takes one:
    // the backup's worker and its copy of the store are part of the run.
    let options = ServerOptions {
        share_pid: true,
        store_toml: store_lines("backup_interval_h = 3", &plan.store_toml),
    };
    let r = on_server_with(
        repository,
        &run,
        &host,
        name,
        &shell,
        &mut extra,
        &options,
        &mut |t| {
            ended = run_campaign(&rt, t, plan, &folder, &mut st, &tag)?;
            Ok(())
        },
    );
    st.set("server", extra);
    if let Err(e) = &r {
        st.set("error", json!(e));
        if ended.is_empty() {
            ended = format!("failed: {e}");
        }
    }
    let code = finish(&folder, name, "campaign", plan, &ended, control, &mut st)?;
    r?;
    Ok((folder, code))
}

/// The campaign soak against the fake sidecar here, shortened, with a leak
/// planted when the plan has one. The rehearsal passes when the tool ran to
/// its end, the verdict reads the planted leak as growing and the group
/// count as flat, and an injected restart ended one segment.
fn rehearse(repository: &Path, plan: &Plan, baseline: Option<&Path>) -> Result<u8, String> {
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-q", "-p", "nanoprobe-fake-sidecar"])
        .current_dir(repository)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("nanoprobe-fake-sidecar did not build".into());
    }
    let tag = bundle::stamp();
    let folder = repository.join(format!("bundles/soak-rehearse-{tag}"));
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let mut st = State::new(&folder, "rehearsal", "rehearse", plan);
    let target_dir =
        std::env::var("CARGO_TARGET_DIR").map_or_else(|_| repository.join("target"), PathBuf::from);
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| e.to_string())?
        .port();
    let mut t = FakeTarget {
        program: target_dir.join("debug/nanoprobe-fake-sidecar"),
        store: folder.join("store"),
        log: folder.join("fake-sidecar.log"),
        port,
        child: None,
        extra: {
            // The keep windows and the backup interval in seconds: the
            // purge follows each removal, and the run takes three
            // scheduled backups.
            let mut x: Vec<String> = [
                "--removed-keep-s",
                "0",
                "--dead-keep-s",
                "30",
                "--backup-interval-s",
            ]
            .map(String::from)
            .to_vec();
            x.push(((plan.duration_s / 3.0).max(30.0) as u64).to_string());
            if plan.leak_kib_per_s > 0 {
                x.extend(["--leak-kib-per-s".into(), plan.leak_kib_per_s.to_string()]);
            }
            x
        },
    };
    t.start_sidecar()?;
    let rt = runtime()?;
    let r = run_campaign(&rt, &mut t, plan, &folder, &mut st, &tag);
    t.stop();
    let ended = r?;
    finish(
        &folder,
        "rehearse",
        "rehearsal",
        plan,
        &ended,
        None,
        &mut st,
    )?;
    let v: Value = serde_json::from_str(
        &std::fs::read_to_string(folder.join("verdict.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let of = |k: &str| v["series"][k]["verdict"].as_str().unwrap_or("").to_owned();
    let mut bad = Vec::new();
    if ended != "completed" {
        bad.push(format!("the run ended: {ended}"));
    }
    // The group count is exact, so minutes resolve it. The descriptor
    // count moves by one or two with a backup or an export, which minutes
    // cannot tell from one an hour: it is reported, not required.
    if of("world_groups") != "flat" {
        bad.push(format!(
            "world_groups reads {:?}, not flat",
            of("world_groups")
        ));
    }
    if plan.leak_kib_per_s > 0 && of("sidecar_rss_kb") != "growing" {
        bad.push(format!(
            "the planted leak reads {:?}, not growing",
            of("sidecar_rss_kb")
        ));
    }
    // The fake grows by itself, so a leak is shown only against a baseline:
    // a completed rehearsal of the same plan with no leak.
    if plan.leak_kib_per_s > 0 && baseline.is_none() {
        bad.push(
            "a planted leak needs --baseline <folder>, a rehearsal of the same plan with no leak"
                .into(),
        );
    }
    if let Some(b) = baseline {
        let base: Value = std::fs::read_to_string(b.join("soak.json"))
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .map_err(|e| format!("{}: {e}", b.join("soak.json").display()))?;
        let mut want = plan.json();
        want["leak_kib_per_s"] = json!(0);
        if base["kind"] != json!("rehearsal") || base["ended"] != json!("completed") {
            bad.push(format!(
                "the baseline is a {} run that ended {}, not a completed rehearsal",
                base["kind"], base["ended"]
            ));
        } else if base["plan"] != want {
            bad.push("the baseline's plan is not this run's plan with no leak".into());
        }
    }
    // Against a rehearsal with no leak, the slope's excess is the planted
    // rate: the fake grows by itself at the rehearsal's rates, so the
    // excess, not the slope, is what shows the fit recovers a known leak.
    if let Some(b) = baseline {
        let base: Value = std::fs::read_to_string(b.join("verdict.json"))
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            .map_err(|e| format!("{}: {e}", b.join("verdict.json").display()))?;
        let slope = |v: &Value| v["series"]["sidecar_rss_kb"]["slope"].as_f64();
        let planted = plan.leak_kib_per_s as f64 * 3600.0;
        match (slope(&v), slope(&base)) {
            (Some(leak), Some(base)) => {
                let excess = leak - base;
                println!(
                    "   sidecar memory: {leak:.0} KiB an hour with the leak, {base:.0} without; \
                     the excess {excess:.0} against {planted:.0} planted"
                );
                if (excess - planted).abs() > LEAK_TOLERANCE * planted {
                    bad.push(format!(
                        "the slope's excess over the baseline is {excess:.0} KiB an hour, not within \
                         {} % of the {planted:.0} planted",
                        LEAK_TOLERANCE * 100.0
                    ));
                }
            }
            _ => bad.push("the baseline or the run has no sidecar memory slope".into()),
        }
    }
    if plan.stall_at_s.is_some() {
        let events = read_jsonl(&folder.join("events.jsonl"));
        let stalled = events
            .iter()
            .position(|e| e["what"] == "inject_stall" && e["detail"]["error"].is_null());
        let skipped =
            stalled.is_some_and(|i| events[i..].iter().any(|e| e["what"] == "records_skipped"));
        if !skipped {
            bad.push("the injected stall skipped no record".into());
        }
    }
    if plan.restart_at_s.is_some() && v["health"]["segment_ends"] != json!(1) {
        bad.push(format!(
            "the injected restart ended {} segments, not 1",
            v["health"]["segment_ends"]
        ));
    }
    if !required("rehearsal").iter().all(|k| of(k) != "no data") {
        bad.push(format!("no data for {}", v["missing_required"]));
    }
    if bad.is_empty() {
        println!("== REHEARSAL PASS: {}", folder.display());
        Ok(0)
    } else {
        println!(
            "== REHEARSAL FAIL: {}: {}",
            folder.display(),
            bad.join("; ")
        );
        Ok(1)
    }
}

/// How far the planted leak's measured rate may be from the planted one:
/// a tenth. The fit's own noise over six minutes is under 2 % of the
/// planted rate, and the baseline's growth differs from run to run.
const LEAK_TOLERANCE: f64 = 0.10;

/// The runs under `bundles/`, newest last, each with its phase, whether
/// its process is alive, and its last record.
fn status(repository: &Path) -> Result<u8, String> {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(repository.join("bundles"))
        .map_err(|e| format!("bundles/: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("state.json").is_file())
        .collect();
    folders.sort();
    for f in folders {
        let Ok(v) = std::fs::read_to_string(f.join("state.json"))
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
        else {
            continue;
        };
        let alive = v["pid"].as_u64().is_some_and(|pid| {
            Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        });
        println!(
            "{}: {} on {}, phase {}, process {} {}, record {} at {} s ({}), segment {}, {} events{}",
            f.display(),
            v["kind"].as_str().unwrap_or("?"),
            v["target"].as_str().unwrap_or("?"),
            v["phase"].as_str().unwrap_or("?"),
            v["pid"],
            if alive { "alive" } else { "not running" },
            v["records"],
            v["last_t_s"],
            v["last_wall"].as_str().unwrap_or("no record yet"),
            v["segment"],
            read_jsonl(&f.join("events.jsonl")).len(),
            v["outcome"]
                .as_str()
                .map_or(String::new(), |o| format!(", {o}")),
        );
    }
    Ok(0)
}

/// Run this command again in a process group of its own, with its output
/// in a log under `bundles/`, and return at once. On macOS it runs under
/// `caffeinate -i`, so an idle machine does not sleep under an 8-hour run.
#[cfg(unix)]
fn detach(repository: &Path, args: &[&str]) -> Result<u8, String> {
    use std::os::unix::process::CommandExt as _;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = repository.join("bundles");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let log_path = dir.join(format!("soak-detached-{}.log", bundle::stamp()));
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let mut command = if cfg!(target_os = "macos") {
        let mut c = Command::new("caffeinate");
        c.arg("-i").arg(&exe);
        c
    } else {
        Command::new(&exe)
    };
    let child = command
        .arg("soak")
        .args(args.iter().filter(|a| **a != "--detach"))
        .current_dir(repository)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    println!(
        "detached: process {}, output in {}; `cargo xtask soak status` lists the run",
        child.id(),
        log_path.display()
    );
    Ok(0)
}

#[cfg(not(unix))]
fn detach(_repository: &Path, _args: &[&str]) -> Result<u8, String> {
    Err("--detach needs a Unix host".into())
}

/// The seconds `--hours` or `--minutes` give, or `default`.
fn duration(args: &[&str], default: f64) -> Result<f64, String> {
    let mut seconds = default;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let scale = match *a {
            "--hours" => 3600.0,
            "--minutes" => 60.0,
            _ => continue,
        };
        let v: f64 = it
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("{a} needs a number\n{USAGE}"))?;
        seconds = v * scale;
    }
    if !(60.0..=24.0 * 3600.0).contains(&seconds) {
        return Err("a run is from 1 minute to 24 hours".into());
    }
    Ok(seconds)
}

/// Refuse an argument that is not one of `flags`, or one of `valued` with
/// its value, before a target is touched.
fn known(args: &[&str], flags: &[&str], valued: &[&str]) -> Result<(), String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if *a == "--detach" || flags.contains(a) {
            continue;
        }
        if valued.contains(a) {
            it.next()
                .ok_or_else(|| format!("{a} needs a value\n{USAGE}"))?;
            continue;
        }
        return Err(format!("unknown argument `{a}`\n{USAGE}"));
    }
    Ok(())
}

/// Every value given after `flag`, in order.
fn values(args: &[&str], flag: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if *a == flag {
            let v = it
                .next()
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?;
            out.push((*v).to_owned());
        }
    }
    Ok(out)
}

/// The `[store]` lines of a run: `defaults` (one `key = value` a line)
/// less the keys `extra` sets, then `extra`. TOML refuses a key twice.
fn store_lines(defaults: &str, extra: &[String]) -> String {
    let key = |l: &str| l.split('=').next().unwrap_or("").trim().to_owned();
    let set: Vec<String> = extra.iter().map(|l| key(l)).collect();
    let mut out = String::new();
    for l in defaults.lines().filter(|l| !set.contains(&key(l))) {
        out.push_str(l);
        out.push('\n');
    }
    for l in extra {
        out.push_str(l.trim());
        out.push('\n');
    }
    out
}

/// How `soak fit` judges a series (docs/results/2026-11-18-sidecar-bound.md,
/// "Method and limits").
#[derive(Clone, Copy, Debug)]
enum Rule {
    /// Slope plus noise at most this, per hour.
    Slope(f64),
    /// Slope plus noise at most this share of the mean, per hour.
    Share(f64),
    /// Every value at most this.
    Max(f64),
    /// Slope within this share of this rate per hour, from the run's start
    /// when `whole`.
    Rate {
        per_h: f64,
        within: f64,
        whole: bool,
    },
    /// Fitted and reported, not judged.
    Report,
}

/// The series of a campaign soak with the sidecar's bounds, by record
/// pointer.
const FIT_SERIES: &[(&str, Rule)] = &[
    ("/sidecar/rss_kb", Rule::Slope(1_024.0)),
    ("/sidecar/cpu_percent", Rule::Slope(1.0)),
    ("/store_size/entities_held", Rule::Slope(10.0)),
    ("/store_size/units", Rule::Slope(40.0)),
    ("/store_size/work_set", Rule::Share(0.05)),
    // 30 an hour kept an hour, one step's kill, and a death or an expiry
    // that lands a step late.
    ("/store_size/groups_dead", Rule::Max(34.0)),
    ("/store_size/awaiting_purge", Rule::Max(10.0)),
    (
        "/store_size/purged",
        Rule::Rate {
            per_h: 150.0,
            within: 0.2,
            whole: false,
        },
    ),
    (
        "/store_size/expired",
        Rule::Rate {
            per_h: 30.0,
            within: 0.2,
            whole: false,
        },
    ),
    (
        "/streams/deaths",
        Rule::Rate {
            per_h: 120.0,
            within: 0.2,
            whole: true,
        },
    ),
    (
        "/churn/kills",
        Rule::Rate {
            per_h: 30.0,
            within: 0.2,
            whole: true,
        },
    ),
    ("/agent/pending_destroy", Rule::Slope(1.0)),
    ("/agent/pending_deaths", Rule::Slope(1.0)),
    ("/agent/loadouts_waiting", Rule::Slope(1.0)),
    ("/agent/births", Rule::Slope(1.0)),
    ("/store_size/journal_rows", Rule::Report),
    ("/store_size/event_rows", Rule::Report),
    ("/store_size/journal_bytes", Rule::Report),
    ("/store_size/event_bytes", Rule::Report),
    ("/store_size/store_file_bytes", Rule::Report),
    ("/store_size/events_file_bytes", Rule::Report),
];

/// Fit each series of [`FIT_SERIES`] by least squares over the medians of
/// `block_s` blocks of `t_s` from `from_s` (from 0 for a whole-run rate),
/// with twice the slope's standard error as its noise, and judge it; count
/// the scheduled backups the status reported, and read the owner's targets
/// from the last record. `pass` is every judged series, two backups or
/// more, no failed backup and the targets.
fn fit_run(records: &[Value], from_s: f64, block_s: f64) -> Value {
    let mut series = serde_json::Map::new();
    let mut pass = true;
    for (ptr, rule) in FIT_SERIES {
        let start = match rule {
            Rule::Rate { whole: true, .. } => 0.0,
            _ => from_s,
        };
        let pts: Vec<(f64, f64)> = records
            .iter()
            .filter_map(|r| Some((r["t_s"].as_f64()?, r.pointer(ptr)?.as_f64()?)))
            .filter(|(t, _)| *t >= start)
            .collect();
        let mut blocks: BTreeMap<i64, Vec<(f64, f64)>> = BTreeMap::new();
        for p in &pts {
            blocks
                .entry(((p.0 - start) / block_s.max(1.0)).floor() as i64)
                .or_default()
                .push(*p);
        }
        let fitted: Vec<(f64, f64)> = blocks
            .values()
            .map(|b| {
                let x = b.iter().map(|p| p.0).sum::<f64>() / b.len() as f64;
                let mut ys: Vec<f64> = b.iter().map(|p| p.1).collect();
                ys.sort_by(f64::total_cmp);
                let n = ys.len();
                let y = if n % 2 == 1 {
                    ys[n / 2]
                } else {
                    (ys[n / 2 - 1] + ys[n / 2]) / 2.0
                };
                (x, y)
            })
            .collect();
        let line = crate::soak::fit(&fitted);
        let max = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        let (slope, noise, mean) = line.as_ref().map_or((None, None, None), |l| {
            (
                Some(l.slope * 3600.0),
                Some(2.0 * l.stderr * 3600.0),
                Some(l.mean_y),
            )
        });
        let ok = match (rule, slope, noise, mean) {
            (Rule::Report, ..) => None,
            (Rule::Max(m), ..) => Some(!pts.is_empty() && max <= *m),
            (Rule::Slope(l), Some(sl), Some(n), _) => Some(sl + n <= *l),
            (Rule::Share(f), Some(sl), Some(n), Some(m)) => Some(sl + n <= f * m.abs()),
            (Rule::Rate { per_h, within, .. }, Some(sl), ..) => {
                Some((sl - per_h).abs() <= within * per_h)
            }
            _ => Some(false),
        };
        if ok == Some(false) {
            pass = false;
        }
        series.insert(
            ptr.trim_start_matches('/').replace('/', "."),
            json!({
                "rule": format!("{rule:?}"), "points": pts.len(), "blocks": fitted.len(),
                "slope_per_h": slope, "noise_per_h": noise, "mean": mean,
                "max": (!pts.is_empty()).then_some(max), "ok": ok,
            }),
        );
    }
    let distinct = |ptr: &str| -> Vec<i64> {
        let mut v: Vec<i64> = records
            .iter()
            .filter_map(|r| r.pointer(ptr)?.as_i64())
            .collect();
        v.dedup();
        v
    };
    let backups = distinct("/api/last_backup_s");
    let failures = distinct("/api/last_backup_failure_s");
    let last = records.last().cloned().unwrap_or(Value::Null);
    let share = last
        .pointer("/agent_whole/share_percent")
        .and_then(Value::as_f64);
    let over = last
        .pointer("/agent_whole/frames_over_250_percent")
        .and_then(Value::as_f64);
    let targets = share.is_some_and(|s| s <= 0.3) && over.is_some_and(|o| o <= 1.0);
    pass = pass && backups.len() >= 2 && failures.is_empty() && targets;
    json!({
        "from_s": from_s, "block_s": block_s, "records": records.len(),
        "series": series,
        "backups": { "taken_s": backups, "failed_s": failures,
                     "last_error": last.pointer("/api/backup_error") },
        "owner_targets": { "share_percent": share, "frames_over_250_percent": over, "ok": targets },
        "pass": pass,
    })
}

fn number(args: &[&str], flag: &str) -> Result<Option<f64>, String> {
    match args.iter().position(|a| *a == flag) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .and_then(|v| v.parse().ok())
            .map(Some)
            .ok_or_else(|| format!("{flag} needs a number\n{USAGE}")),
    }
}

/// Write a run's verdict again from its records, against a control when
/// one is given. A run whose controller died has no `soak.json`: its kind,
/// target and plan come from `state.json`, which a run writes when it
/// starts, and it is judged as ended by its controller's death.
fn judge(folder: &Path, control: Option<&Path>) -> Result<u8, String> {
    let read = |file: &str| -> Option<Value> {
        std::fs::read_to_string(folder.join(file))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    };
    let state = read("state.json");
    let soak = read("soak.json");
    let Some(from) = soak.as_ref().or(state.as_ref()) else {
        return Err(format!(
            "{} holds neither soak.json nor state.json",
            folder.display()
        ));
    };
    let kind = from["kind"].as_str().unwrap_or("campaign").to_owned();
    let name = from["target"].as_str().unwrap_or("?").to_owned();
    let plan = Plan {
        interval_s: from["plan"]["interval_s"].as_f64().unwrap_or(60.0),
        ..Plan::live(
            from["plan"]["duration_s"]
                .as_f64()
                .unwrap_or(SOAK_HOURS * 3600.0),
        )
    };
    let ended = match &soak {
        Some(s) => s["ended"].as_str().unwrap_or("completed").to_owned(),
        None => from["ended"]
            .as_str()
            .unwrap_or("the controller died")
            .to_owned(),
    };
    let mut st = State {
        path: folder.join("state.json"),
        v: state.unwrap_or_else(|| json!({})),
    };
    // Without `--control`, the control the run was judged with before.
    let before = soak
        .as_ref()
        .and_then(|s| s["control"].as_str())
        .map(PathBuf::from);
    let control = control.map(Path::to_path_buf).or(before);
    match &control {
        Some(c) => println!("== judged against the control {}", c.display()),
        None => println!("== judged without a control"),
    }
    finish(
        folder,
        &name,
        &kind,
        &plan,
        &ended,
        control.as_deref(),
        &mut st,
    )
}

/// The verdict of a window of a run, from `from_s` to `to_s`: a part of a
/// run whose load changed partway, judged on its own. The records after
/// `to_s` are left out; those before `from_s` keep the run's clock, so
/// uptime and the 24-hour projections count from the run's start, but are
/// not fitted. It writes `verdict-window-<from>-<to>.json` and `.md` beside
/// the run's verdict, which it leaves as it is. The control is used only
/// for a window that starts with the run, because the control's fit is
/// matched to the soak's first hours of uptime; a later window is judged
/// without it, so the series DCS itself moves read their slopes with no
/// verdict on the platform. A window is shorter than a soak, so its outcome
/// is never PASS: read its series.
fn judge_window(
    folder: &Path,
    control: Option<&Path>,
    from_s: f64,
    to_s: f64,
) -> Result<u8, String> {
    let read = |file: &str| -> Option<Value> {
        std::fs::read_to_string(folder.join(file))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    };
    let from = read("soak.json")
        .or_else(|| read("state.json"))
        .ok_or_else(|| {
            format!(
                "{} holds neither soak.json nor state.json",
                folder.display()
            )
        })?;
    let kind = from["kind"].as_str().unwrap_or("campaign").to_owned();
    let name = from["target"].as_str().unwrap_or("?").to_owned();
    let records: Vec<Value> = read_jsonl(&folder.join("records.jsonl"))
        .into_iter()
        .filter(|r| r["t_s"].as_f64().is_some_and(|t| t <= to_s))
        .collect();
    let times: Vec<f64> = records.iter().filter_map(|r| r["t_s"].as_f64()).collect();
    let (Some(&start), Some(&last)) = (times.first(), times.last()) else {
        return Err(format!("no record up to {to_s} s"));
    };
    let Some(&first) = times.iter().find(|t| **t >= from_s) else {
        return Err(format!("no record from {from_s} s to {to_s} s"));
    };
    let interval_s = from["plan"]["interval_s"].as_f64().unwrap_or(60.0);
    // The run's first record is due one interval after the churn begins: a
    // window that holds it starts with the run and keeps the warm-up.
    let with_control = first <= start + 1e-6;
    let plan = Plan {
        interval_s,
        // Uptime counts from the first record; a later window's first is
        // fitted, the records before it are not.
        fit_from_s: (!with_control).then(|| first - start - interval_s / 2.0),
        ..Plan::live(last + interval_s)
    };
    let control_records = control.filter(|_| with_control).and_then(|c| {
        control_check(c, &name, &kind)
            .0
            .then(|| read_jsonl(&c.join("records.jsonl")))
    });
    let mut v = verdict(&kind, &plan, &records, control_records.as_deref(), "window");
    let in_window = times.iter().filter(|t| **t >= first).count();
    v["window"] = json!({
        "from_s": first, "to_s": last, "records": in_window,
        "control_used": control_records.is_some(),
        "warmup_s": plan.warmup_s(),
    });
    let (word, code) = outcome(&v);
    let stem = format!("verdict-window-{}-{}", first as u64, last as u64);
    let write = |file: String, text: String| {
        let path = folder.join(&file);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    };
    write(
        format!("{stem}.json"),
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )?;
    let table = series::table(&v);
    write(
        format!("{stem}.md"),
        format!(
            "{word} (a window, not the run's verdict): {kind} on {name}, records from {first} s to {last} s, {} records, control {}\n\n{}{table}\n",
            in_window,
            if control_records.is_some() {
                "used"
            } else {
                "not used"
            },
            churn_lines(&v),
        ),
    )?;
    println!("{table}");
    println!(
        "== {word} (window): {}",
        folder.join(format!("{stem}.md")).display()
    );
    Ok(code)
}

/// Whether `word` is one of this module's commands, which `soak` takes
/// where its older form takes a target's name.
pub(crate) fn is_command(word: &str) -> bool {
    matches!(
        word,
        "campaign" | "control" | "pilot" | "rehearse" | "status" | "stop" | "judge" | "fit"
    )
}

pub(crate) fn command(repository: &Path, args: &[&str]) -> Result<ExitCode, String> {
    if args.contains(&"--detach") {
        return detach(repository, args).map(ExitCode::from);
    }
    let code = match args {
        ["campaign", name, rest @ ..] => {
            known(
                rest,
                &["--leave-dead", "--reattach"],
                &["--hours", "--minutes", "--control", "--store-toml"],
            )?;
            let mut plan = Plan::live(duration(rest, 8.0 * 3600.0)?);
            plan.leave_dead = rest.contains(&"--leave-dead");
            plan.reattach = rest.contains(&"--reattach");
            plan.store_toml = values(rest, "--store-toml")?;
            let control = match rest.iter().position(|a| *a == "--control") {
                Some(i) => Some(PathBuf::from(rest.get(i + 1).ok_or(USAGE)?)),
                None => None,
            };
            campaign_on_server(repository, name, &plan, control.as_deref())?.1
        }
        ["control", name, rest @ ..] => {
            let plan = Plan::live(duration(rest, 2.0 * 3600.0)?);
            control_on_server(repository, name, &plan)?;
            0
        }
        ["pilot", name, rest @ ..] => {
            // A control and a campaign soak of the same length. Ten
            // minutes leave each seven records and four floors after its
            // warm-up, which a fit takes as points.
            let plan = Plan::pilot(duration(rest, 10.0 * 60.0)?);
            let (control, _) = control_on_server(repository, name, &plan)?;
            campaign_on_server(repository, name, &plan, Some(&control))?.1
        }
        ["rehearse", rest @ ..] => {
            known(
                rest,
                &[],
                &[
                    "--hours",
                    "--minutes",
                    "--leak-kib-per-s",
                    "--baseline",
                    "--restart-at",
                    "--stall-at",
                ],
            )?;
            let mut plan = Plan::rehearsal(duration(rest, 6.0 * 60.0)?);
            plan.reattach = true;
            plan.leak_kib_per_s = number(rest, "--leak-kib-per-s")?.unwrap_or(0.0) as u64;
            plan.restart_at_s = number(rest, "--restart-at")?;
            plan.stall_at_s = number(rest, "--stall-at")?;
            plan.stall_for_s = 20.0;
            let baseline = match rest.iter().position(|a| *a == "--baseline") {
                Some(i) => Some(PathBuf::from(rest.get(i + 1).ok_or(USAGE)?)),
                None => None,
            };
            rehearse(repository, &plan, baseline.as_deref())?
        }
        ["fit", folder, rest @ ..] => {
            known(rest, &[], &["--from-minute", "--block-minutes"])?;
            let from_s = number(rest, "--from-minute")?.unwrap_or(75.0) * 60.0;
            let block_s = number(rest, "--block-minutes")?.unwrap_or(5.0) * 60.0;
            let records = read_jsonl(&Path::new(folder).join("records.jsonl"));
            let v = fit_run(&records, from_s, block_s);
            std::fs::write(
                Path::new(folder).join("fit.json"),
                serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("{folder}: {e}"))?;
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            u8::from(v["pass"] != json!(true))
        }
        ["status"] => status(repository)?,
        ["stop", folder] => {
            std::fs::write(Path::new(folder).join("stop"), b"")
                .map_err(|e| format!("{folder}: {e}"))?;
            println!("the run ends at its next step, with its verdict and its cleanup");
            0
        }
        ["judge", folder, rest @ ..] => {
            let mut control = None;
            let (mut from, mut to) = (None, None);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                let value = it.next().ok_or(USAGE)?;
                match *a {
                    "--control" => control = Some(PathBuf::from(value)),
                    "--from" => from = Some(value.parse::<f64>().map_err(|_| USAGE)?),
                    "--to" => to = Some(value.parse::<f64>().map_err(|_| USAGE)?),
                    _ => return Err(USAGE.into()),
                }
            }
            if from.is_some() || to.is_some() {
                judge_window(
                    Path::new(folder),
                    control.as_deref(),
                    from.unwrap_or(0.0),
                    to.unwrap_or(f64::INFINITY),
                )?
            } else {
                judge(Path::new(folder), control.as_deref())?
            }
        }
        _ => return Err(USAGE.into()),
    };
    Ok(ExitCode::from(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: u64, state: EntityState) -> pb::Group {
        pb::Group {
            id,
            state: state as i32,
            ..pb::Group::default()
        }
    }

    #[test]
    fn dcs_is_compared_with_the_store_for_this_campaign_only() {
        let tag = names::tag_of(5);
        let other = names::tag_of(6);
        let g = |id: u64, units: &[u64], t: names::Tag| scenarios::DcsGroup {
            name: names::name(Kind::Group, t, id),
            units: units
                .iter()
                .map(|u| scenarios::DcsUnit {
                    name: names::name(Kind::Unit, t, *u),
                    north: 0.0,
                    east: 0.0,
                    alt: 0.0,
                    in_air: false,
                })
                .collect(),
        };
        let groups: BTreeMap<u64, pb::Group> = [
            (1, group(1, EntityState::Live)),
            (2, group(2, EntityState::Live)),
            (3, group(3, EntityState::Virtual)),
        ]
        .into();
        let dcs = vec![
            g(1, &[11], tag),
            g(3, &[31], tag),
            g(4, &[41, 42], tag),
            g(9, &[91], other),
            scenarios::DcsGroup {
                name: "Ground-1".into(),
                units: Vec::new(),
            },
        ];
        let dead = vec![names::name(Kind::Unit, tag, 42)];
        let v = compare_dcs(&groups, &dcs, tag, &dead);
        assert_eq!(v["groups_of_the_campaign"], json!(3));
        assert_eq!(v["without_a_row"], json!([4]));
        assert_eq!(v["listed_out_of_dcs"], json!([3]));
        assert_eq!(v["live_not_in_dcs"], json!([2]));
        assert_eq!(v["dead_members_in_dcs"], json!(1));
    }

    /// Records of a 2-hour run: a flat memory and a growing one, counters
    /// at their rates, two backups, the targets met.
    fn records(rss_kb_per_h: f64, purged_per_h: f64) -> Vec<Value> {
        (0..120)
            .map(|k| {
                let t = k as f64 * 60.0;
                let h = t / 3600.0;
                let wiggle = if k % 2 == 0 { 50.0 } else { -50.0 };
                json!({
                    "t_s": t,
                    "sidecar": { "rss_kb": 60_000.0 + rss_kb_per_h * h + wiggle, "cpu_percent": 25.0 },
                    "store_size": {
                        "entities_held": 1_030, "units": 4_120, "work_set": 40, "groups_dead": 30,
                        "awaiting_purge": 2, "purged": (purged_per_h * h) as u64,
                        "expired": (30.0 * h) as u64,
                    },
                    "streams": { "deaths": (120.0 * h) as u64 },
                    "churn": { "kills": (30.0 * h) as u64 },
                    "agent": { "pending_destroy": 0, "pending_deaths": 0, "loadouts_waiting": 0, "births": 0 },
                    "api": {
                        "last_backup_s": if k >= 60 { Some(1_000 + i64::from(k >= 119)) } else { None },
                        "last_backup_failure_s": null,
                    },
                    "agent_whole": { "share_percent": 0.2, "frames_over_250_percent": 0.1 },
                })
            })
            .collect()
    }

    #[test]
    fn a_plateau_passes_the_fit_and_a_growth_or_a_wrong_rate_fails_it() {
        let ok = fit_run(&records(0.0, 150.0), 4_500.0, 300.0);
        assert_eq!(ok["pass"], json!(true), "{ok:#}");
        let grows = fit_run(&records(3_000.0, 150.0), 4_500.0, 300.0);
        assert_eq!(grows["series"]["sidecar.rss_kb"]["ok"], json!(false));
        assert_eq!(grows["pass"], json!(false));
        let slow = fit_run(&records(0.0, 100.0), 4_500.0, 300.0);
        assert_eq!(slow["series"]["store_size.purged"]["ok"], json!(false));
        let mut one = records(0.0, 150.0);
        for r in &mut one {
            r["api"]["last_backup_s"] = json!(1_000);
        }
        assert_eq!(
            fit_run(&one, 4_500.0, 300.0)["pass"],
            json!(false),
            "one backup"
        );
    }

    #[test]
    fn an_unknown_soak_argument_is_refused() {
        let v = &["--minutes", "120", "--store-toml", "a = 1", "--leave-dead"];
        assert!(known(v, &["--leave-dead"], &["--minutes", "--store-toml"]).is_ok());
        assert!(known(&["--leave_dead"], &["--leave-dead"], &[]).is_err());
        assert!(known(&["--minutes"], &[], &["--minutes"]).is_err());
    }

    #[test]
    fn store_toml_lines_replace_the_defaults_they_name() {
        let extra = values(
            &[
                "--minutes",
                "120",
                "--store-toml",
                "removed_keep_h = 0",
                "--store-toml",
                "backup_interval_h=1",
            ],
            "--store-toml",
        )
        .expect("values");
        assert_eq!(
            store_lines("backup_interval_h = 3", &extra),
            "removed_keep_h = 0\nbackup_interval_h=1\n"
        );
        assert_eq!(
            store_lines("backup_interval_h = 3", &[]),
            "backup_interval_h = 3\n"
        );
        assert!(values(&["--store-toml"], "--store-toml").is_err());
        assert_eq!(
            duration(&["--store-toml", "a = 1", "--minutes", "5"], 0.0).unwrap(),
            300.0
        );
    }

    #[test]
    fn a_route_ends_after_its_laps_so_its_group_can_be_routed_or_killed_again() {
        let plan = Plan::live(8.0 * 3600.0);
        // 3,200 m at 5 m/s is 640 s: 22 steps of 30 s, and one more.
        assert_eq!(ROUTE_S, 640.0);
        assert!(!driving(None, 0, &plan));
        assert!(driving(Some(100), 100, &plan));
        assert!(driving(Some(100), 122, &plan));
        assert!(!driving(Some(100), 123, &plan));
        // Over 8 hours every slot of the window keeps being routed and the
        // kills keep finding a target: no step runs out of candidates.
        let steps = (plan.duration_s / plan.tick_s) as u64;
        let mut routed: Vec<Option<u64>> = vec![None; plan.live];
        let mut next = 0;
        for n in 0..steps {
            let free: Vec<usize> = (0..plan.live)
                .filter(|p| !driving(routed[*p], n, &plan))
                .collect();
            assert!(free.len() > plan.orders, "step {n}: {} free", free.len());
            for i in 0..plan.orders {
                routed[free[(next + i) % free.len()]] = Some(n);
            }
            next += plan.orders;
        }
    }

    #[test]
    fn the_process_lines_become_nested_numbers() {
        let v = parse_lines(
            "dcs.pid 41\ndcs.rss_kb 4500000\nfile.store.sqlite 8192\nnoise\nclock.hz x\n",
        );
        assert_eq!(v["dcs"]["pid"], json!(41.0));
        assert_eq!(v["dcs"]["rss_kb"], json!(4_500_000.0));
        assert_eq!(v["file"]["store.sqlite"], json!(8192.0));
        assert!(v["clock"]["hz"].is_null());
        assert_eq!(
            identity_of(&parse_lines("dcs.pid 41\ndcs.start 900\n"), "dcs").as_deref(),
            Some("41:900")
        );
        assert_eq!(identity_of(&v, "dcs"), None);
    }

    /// A subject that counts what the loop asks of it and changes its
    /// identity once.
    struct Scripted {
        /// Steps taken between a failed recovery and the one that worked.
        down_ticks: u64,
        waiting: bool,
        ticks: u64,
        records: u64,
        identity: &'static str,
        recovers: u32,
        fail_recover: u32,
    }

    impl Subject for Scripted {
        fn tick(&mut self, _n: u64, _plan: &Plan) -> Value {
            self.ticks += 1;
            self.down_ticks += u64::from(self.waiting);
            json!({ "steps": 1, "errors": if self.ticks == 2 { json!(["a step failed"]) } else { json!([]) } })
        }
        fn record(&mut self, collect: bool, export: bool, _plan: &Plan) -> Value {
            self.records += 1;
            if self.records == 2 {
                self.identity = "b";
            }
            json!({ "collect": collect, "export": export, "errors": [], "players": if self.records == 2 { 1 } else { 0 } })
        }
        fn identity(&mut self) -> Result<Option<String>, String> {
            Ok(Some(self.identity.to_owned()))
        }
        fn recover(&mut self, _plan: &Plan) -> Result<(), String> {
            if self.fail_recover > 0 {
                self.fail_recover -= 1;
                self.waiting = true;
                return Err("DCS is not back".into());
            }
            self.waiting = false;
            self.recovers += 1;
            Ok(())
        }
    }

    /// Six records half a second apart: wide enough that a loaded host
    /// takes each in its own interval.
    fn short_plan() -> Plan {
        Plan {
            interval_s: 0.5,
            tick_s: 0.25,
            collect_every: 2,
            export_every: 3,
            ..Plan::rehearsal(3.0)
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nanoprobe-soak-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_restart_ends_a_segment_and_the_run_goes_on() {
        let folder = scratch("segment");
        let plan = short_plan();
        let mut st = State::new(&folder, "test", "none", &plan);
        let mut s = Scripted {
            down_ticks: 0,
            waiting: false,
            ticks: 0,
            records: 0,
            identity: "a",
            recovers: 0,
            fail_recover: 1,
        };
        let ended = drive(&mut s, &plan, &folder, &mut st);
        assert_eq!(ended, "completed");
        let records = read_jsonl(&folder.join("records.jsonl"));
        assert_eq!(records.len(), 6);
        // The clock is the schedule's: record k is at k intervals.
        for (i, r) in records.iter().enumerate() {
            assert!((r["t_s"].as_f64().unwrap() - 0.5 * (i + 1) as f64).abs() < 1e-9);
        }
        // The third record saw the change; the recovery failed once, so the
        // next segment starts one record later. The records between belong
        // to no segment.
        assert_eq!(records[1]["segment"], json!(0));
        assert_eq!(records[2]["segment_end"], json!(true));
        assert!(records[2]["segment"].is_null());
        assert!(records[3]["segment"].is_null());
        assert_eq!(records[4]["segment"], json!(1));
        assert_eq!(s.recovers, 1);
        // Collects and exports follow the record number.
        assert_eq!(records[1]["collect"], json!(true));
        assert_eq!(records[0]["export"], json!(true));
        assert_eq!(records[5]["export"], json!(true));
        let events: Vec<String> = read_jsonl(&folder.join("events.jsonl"))
            .iter()
            .map(|e| e["what"].as_str().unwrap_or("").to_owned())
            .collect();
        for what in [
            "step_failed",
            "players",
            "segment_end",
            "recover_waiting",
            "segment_start",
        ] {
            assert!(
                events.iter().any(|e| e == what),
                "no {what} event in {events:?}"
            );
        }
        // No step runs while DCS is down: two were due between the recovery
        // that failed and the one that worked.
        assert_eq!(s.down_ticks, 0);
        assert!(s.ticks >= 6, "{} steps", s.ticks);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_stop_file_ends_the_run_with_its_records() {
        let folder = scratch("stop");
        let plan = Plan {
            duration_s: 60.0,
            ..short_plan()
        };
        let mut st = State::new(&folder, "test", "none", &plan);
        std::fs::write(folder.join("stop"), b"").unwrap();
        let mut s = Scripted {
            down_ticks: 0,
            waiting: false,
            ticks: 0,
            records: 0,
            identity: "a",
            recovers: 0,
            fail_recover: 0,
        };
        assert_eq!(drive(&mut s, &plan, &folder, &mut st), "stopped");
        assert_eq!(st.v["phase"], json!("finishing"));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn the_memory_guard_ends_a_run_before_the_limit() {
        struct Full;
        impl Subject for Full {
            fn tick(&mut self, _n: u64, _plan: &Plan) -> Value {
                json!({})
            }
            fn record(&mut self, _c: bool, _e: bool, _plan: &Plan) -> Value {
                json!({ "container": { "working_set_kb": 95.0, "limit_kb": 100.0 }, "errors": [] })
            }
            fn identity(&mut self) -> Result<Option<String>, String> {
                Err("not asked".into())
            }
            fn recover(&mut self, _plan: &Plan) -> Result<(), String> {
                Ok(())
            }
        }
        let folder = scratch("guard");
        let plan = short_plan();
        let mut st = State::new(&folder, "test", "none", &plan);
        assert_eq!(drive(&mut Full, &plan, &folder, &mut st), "memory guard");
        assert_eq!(read_jsonl(&folder.join("records.jsonl")).len(), 1);
        let _ = std::fs::remove_dir_all(&folder);
    }

    fn flat_records(n: u64, leak_kb_per_record: f64) -> Vec<Value> {
        (1..=n)
            .map(|k| {
                json!({
                    "t_s": k as f64 * 60.0, "segment": 0,
                    "sidecar": { "rss_kb": 50_000.0 + leak_kb_per_record * k as f64 + (k % 3) as f64, "fds": 12 },
                    "world": { "groups": 1000 },
                    "container": { "working_set_kb": 5.0e6, "limit_kb": 2.0e7 },
                    "agent_whole": { "share_percent": 0.19, "frames": 1000.0, "frames_over_250_percent": 0.2 },
                    "dcs": { "fps": 110.0 },
                    "errors": [],
                })
            })
            .collect()
    }

    #[test]
    fn a_planted_leak_fails_the_verdict_and_a_flat_run_does_not() {
        let plan = Plan::live(4.0 * 3600.0);
        // 1 MiB a minute is 60 MiB an hour, against 16.
        let leak = verdict(
            "rehearsal",
            &plan,
            &flat_records(240, 1024.0),
            None,
            "completed",
        );
        assert_eq!(
            leak["series"]["sidecar_rss_kb"]["verdict"],
            json!("growing")
        );
        assert_eq!(outcome(&leak).0, "FAIL");
        let flat = verdict(
            "rehearsal",
            &plan,
            &flat_records(240, 0.0),
            None,
            "completed",
        );
        assert_eq!(flat["series"]["sidecar_rss_kb"]["verdict"], json!("flat"));
        assert_eq!(flat["series"]["world_groups"]["verdict"], json!("flat"));
        assert_eq!(flat["ok"], json!(true));
        // The rehearsal's required series are not all there, so it does not pass.
        assert_eq!(outcome(&flat).0, "EXTEND");
        assert_eq!(flat["owner_targets"]["met"], json!(true));
    }

    /// Records of a live campaign's DCS-side series, flat.
    fn dcs_records(n: u64) -> Vec<Value> {
        let mut records = flat_records(n, 0.0);
        for (k, r) in records.iter_mut().enumerate() {
            r["dcs"]["rss_kb"] = json!(4_500_000.0 + (k % 5) as f64);
            r["dcs"]["fds"] = json!(300);
        }
        records
    }

    #[test]
    fn a_series_without_its_control_has_no_verdict_whatever_it_reads() {
        let plan = Plan::live(8.0 * 3600.0);
        let records = dcs_records(480);
        // Flat alone: still no verdict on the platform.
        let alone = verdict("campaign", &plan, &records, None, "completed");
        assert_eq!(alone["series"]["dcs_rss_kb"]["verdict"], json!("flat"));
        let needs = alone["needs_control"].as_array().unwrap();
        assert!(needs.contains(&json!("dcs_rss_kb")) && needs.contains(&json!("dcs_fds")));
        assert!(!needs.contains(&json!("sidecar_rss_kb")));
        assert_eq!(alone["conclusive"], json!(false));
        assert_ne!(outcome(&alone).0, "PASS");
        // With its control it is judged, on the platform's slope.
        let control = dcs_records(120);
        let judged = verdict("campaign", &plan, &records, Some(&control), "completed");
        assert_eq!(judged["needs_control"], json!([]));
        assert_eq!(judged["series"]["dcs_rss_kb"]["basis"], json!("platform"));
        assert_eq!(judged["series"]["dcs_rss_kb"]["verdict"], json!("flat"));
    }

    #[test]
    fn a_short_or_stopped_campaign_run_is_not_a_verdict() {
        let records = flat_records(240, 0.0);
        let short = verdict(
            "campaign",
            &Plan::live(4.0 * 3600.0),
            &records,
            None,
            "completed",
        );
        assert_eq!(short["short_run"], json!(true));
        assert_ne!(outcome(&short).0, "PASS");
        let stopped = verdict(
            "campaign",
            &Plan::live(8.0 * 3600.0),
            &flat_records(480, 0.0),
            None,
            "stopped",
        );
        assert_eq!(stopped["short_run"], json!(true));
        let full = verdict(
            "campaign",
            &Plan::live(8.0 * 3600.0),
            &flat_records(480, 0.0),
            None,
            "completed",
        );
        assert_eq!(full["short_run"], json!(false));
        // A rehearsal is as long as it is asked to be.
        let rehearsal = verdict(
            "rehearsal",
            &Plan::live(4.0 * 3600.0),
            &records,
            None,
            "completed",
        );
        assert_eq!(rehearsal["short_run"], json!(false));
    }

    #[test]
    fn a_pilot_of_ten_minutes_is_fitted() {
        let plan = Plan::pilot(600.0);
        // Seven records after the warm-up, fitted as points.
        assert_eq!(plan.warmup_s(), 150.0);
        assert!(plan.block_s() < 3.0 * plan.interval_s);
        let mut records = flat_records(10, 0.0);
        for (k, r) in records.iter_mut().enumerate() {
            if (k + 1) % 2 == 0 {
                r["heap"]["server_floor_kib"] = json!(90_000.0 + k as f64);
            }
        }
        let v = verdict("campaign", &plan, &records, None, "completed");
        assert_eq!(v["series"]["sidecar_rss_kb"]["fitted"], json!("points"));
        assert_ne!(v["series"]["sidecar_rss_kb"]["verdict"], json!("no data"));
        assert_ne!(
            v["series"]["heap_server_floor_kib"]["verdict"],
            json!("no data")
        );
        assert_ne!(v["series"]["world_groups"]["verdict"], json!("no data"));
        // The long plans keep ten-record blocks.
        assert_eq!(Plan::live(8.0 * 3600.0).block_s(), 600.0);
        assert_eq!(Plan::live(2.0 * 3600.0).block_s(), 600.0);
    }

    #[test]
    fn a_run_whose_controller_died_is_judged_from_its_state_and_records() {
        let folder = scratch("judge");
        let plan = Plan::rehearsal(360.0);
        let st = State::new(&folder, "rehearsal", "rehearse", &plan);
        drop(st);
        for r in flat_records(40, 0.0) {
            append(&folder.join("records.jsonl"), &r);
        }
        assert!(!folder.join("soak.json").exists());
        judge(&folder, None).unwrap();
        let soak: Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("soak.json")).unwrap())
                .unwrap();
        assert_eq!(soak["ended"], json!("the controller died"));
        assert_eq!(soak["kind"], json!("rehearsal"));
        assert_eq!(soak["records"], json!(40));
        assert!(folder.join("verdict.json").exists() && folder.join("verdict.md").exists());
        // With neither file there is nothing to judge.
        let empty = scratch("judge-empty");
        assert!(judge(&empty, None).is_err());
        let _ = std::fs::remove_dir_all(&folder);
        let _ = std::fs::remove_dir_all(&empty);
    }

    fn set(v: &mut Value, pointer: &str, x: Value) {
        let mut at = v;
        for part in pointer.trim_start_matches('/').split('/') {
            at = &mut at[part];
        }
        *at = x;
    }

    /// Records that carry every series of [`series::DEFS`], flat, so a
    /// verdict over them misses no required series.
    fn full_records(n: u64, segment_of: impl Fn(u64) -> u64) -> Vec<Value> {
        (1..=n)
            .map(|k| {
                let mut r = json!({
                    "t_s": k as f64 * 60.0, "segment": segment_of(k), "errors": [],
                    "agent_whole": { "share_percent": 0.19, "frames": 1000.0, "frames_over_250_percent": 0.2 },
                });
                for d in series::DEFS {
                    let y = match d.key {
                        "ids_per_spawned_unit" => 256.0 * (1000.0 + 20.0 * k as f64),
                        "agent_share_percent" => 0.19,
                        "frames_over_250_percent" => 0.2,
                        "sidecar_cpu_percent" => 5.0,
                        "container_working_set_kb" => 5.0e6,
                        _ => 1000.0 + (k % 3) as f64 * 1e-3,
                    };
                    set(&mut r, d.pointer, json!(y));
                }
                set(&mut r, "/ids/spawned_units", json!(1000.0 + 20.0 * k as f64));
                set(&mut r, "/container/limit_kb", json!(2.0e7));
                r["churn"] = planned_totals(k);
                r
            })
            .collect()
    }

    /// The live plan's churn totals after `k` records of a minute: two steps
    /// a record.
    fn planned_totals(k: u64) -> Value {
        let s = 2.0 * k as f64;
        json!({ "steps": s, "dematerialized": 4.0 * s, "materialized": 4.0 * s,
            "removed": s, "put": s, "orders": 3.0 * s, "effects": 2.0 * s, "queries": s,
            "kills": s / 4.0 })
    }

    #[test]
    fn a_run_judged_again_keeps_its_provenance_and_end() {
        let folder = scratch("rejudge");
        let plan = Plan::live(4.0 * 3600.0);
        let lines: Vec<String> = flat_records(240, 0.0)
            .iter()
            .map(|r| r.to_string())
            .collect();
        std::fs::write(folder.join("records.jsonl"), lines.join("\n") + "\n").unwrap();
        let mut st = State::new(&folder, "rehearsal", "none", &plan);
        // The first verdict writes the provenance and the end.
        finish(
            &folder,
            "none",
            "rehearsal",
            &plan,
            "completed",
            None,
            &mut st,
        )
        .unwrap();
        let first: Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("soak.json")).unwrap())
                .unwrap();
        assert!(first["judged_again_at"].is_null());
        // The run's own figures, as a live run would have written them.
        let mut run = first.clone();
        run["provenance"] = json!({ "agent_build": "the run's build" });
        run["ended_at"] = json!("the run's end");
        std::fs::write(folder.join("soak.json"), run.to_string()).unwrap();
        finish(
            &folder,
            "none",
            "rehearsal",
            &plan,
            "completed",
            None,
            &mut st,
        )
        .unwrap();
        let again: Value =
            serde_json::from_str(&std::fs::read_to_string(folder.join("soak.json")).unwrap())
                .unwrap();
        assert_eq!(again["provenance"]["agent_build"], json!("the run's build"));
        assert_eq!(again["ended_at"], json!("the run's end"));
        assert!(again["judged_again_at"].is_string());
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// With `--leave-dead` a killed group is not removed: a step's `put`
    /// rises by the kill rate and `removed` stays at the replacements, which
    /// the guard's plan does not take for a short churn.
    #[test]
    fn a_leave_dead_churn_of_two_hours_reads_clean() {
        let mut plan = Plan::live(2.0 * 3600.0);
        plan.leave_dead = true;
        let records: Vec<Value> = (1..=120u64)
            .map(|k| {
                let steps = (k * 2) as f64;
                let kills = (steps / 4.0).floor();
                json!({ "t_s": k as f64 * 60.0, "churn": {
                    "steps": steps, "dematerialized": 4.0 * steps, "materialized": 4.0 * steps,
                    "removed": steps, "put": steps + kills, "dead_left": kills,
                    "orders": 3.0 * steps, "effects": 2.0 * steps, "queries": steps,
                    "kills": kills,
                }})
            })
            .collect();
        assert_eq!(
            churn_short("campaign", &plan, &records),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn a_churn_that_falls_short_of_its_plan_takes_the_verdict_away() {
        let plan = Plan::live(8.0 * 3600.0);
        let control = full_records(120, |_| 0);
        let steady = full_records(480, |_| 0);
        assert_eq!(churn_short("campaign", &plan, &steady), Vec::<Value>::new());
        // The soak of 2026-11-18: kills stop after 3.7 hours and routes fall
        // to a third.
        let mut degraded = full_records(480, |_| 0);
        for (i, r) in degraded.iter_mut().enumerate() {
            let k = i as u64 + 1;
            if k > 222 {
                r["churn"]["kills"] = json!(planned_totals(222)["kills"]);
                let orders =
                    planned_totals(222)["orders"].as_f64().unwrap() + 2.0 * (k - 222) as f64;
                r["churn"]["orders"] = json!(orders);
            }
        }
        let short = churn_short("campaign", &plan, &degraded);
        let kinds: Vec<&str> = short.iter().filter_map(|s| s["kind"].as_str()).collect();
        assert_eq!(kinds, ["orders", "kills"], "{short:?}");
        let kills = &short[1];
        // The stretch begins with the half hour the kills stopped in.
        assert!(
            kills["achieved_per_hour"].as_f64().unwrap() < 3.0,
            "{kills}"
        );
        assert_eq!(kills["planned_per_hour"], json!(30.0));
        // The stretch starts at the first whole half hour after the stop.
        assert!(kills["from_s"].as_f64().unwrap() >= 222.0 * 60.0 - 1800.0);
        let v = verdict("campaign", &plan, &degraded, Some(&control), "completed");
        assert_eq!(v["conclusive"], json!(false));
        assert_eq!(outcome(&v).0, "EXTEND", "{}", series::table(&v));
        // A stall of 30 minutes across two half hours, which the run rides
        // through, is not a stretch.
        let mut stall = full_records(480, |_| 0);
        for (i, r) in stall.iter_mut().enumerate() {
            let k = i as u64 + 1;
            if (100..130).contains(&k) {
                r["churn"] = planned_totals(100);
            } else if k >= 130 {
                r["churn"] = planned_totals(k - 30);
            }
        }
        assert_eq!(churn_short("campaign", &plan, &stall), Vec::<Value>::new());
        // A counter the plan has and no record carries is short.
        let mut silent = full_records(480, |_| 0);
        for r in &mut silent {
            r["churn"].as_object_mut().unwrap().remove("kills");
        }
        let short = churn_short("campaign", &plan, &silent);
        assert_eq!(short.len(), 1);
        assert_eq!(short[0]["why"], json!("no record counts it"));
    }

    #[test]
    fn only_a_whole_run_with_its_control_passes() {
        let plan = Plan::live(8.0 * 3600.0);
        let soak = full_records(480, |_| 0);
        let control = full_records(120, |_| 0);
        let pass = verdict("campaign", &plan, &soak, Some(&control), "completed");
        assert_eq!(pass["missing_required"], json!([]), "{}", pass["no_data"]);
        assert_eq!(outcome(&pass).0, "PASS", "{}", series::table(&pass));
        // One condition at a time takes the verdict away.
        let alone = verdict("campaign", &plan, &soak, None, "completed");
        assert_eq!(alone["missing_required"], json!([]));
        assert!(
            alone["needs_control"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
        );
        assert_eq!(outcome(&alone).0, "EXTEND");
        let stopped = verdict("campaign", &plan, &soak, Some(&control), "stopped");
        assert_eq!(stopped["short_run"], json!(true));
        assert_eq!(outcome(&stopped).0, "EXTEND");
        let short = verdict(
            "campaign",
            &Plan::live(4.0 * 3600.0),
            &full_records(240, |_| 0),
            Some(&control),
            "completed",
        );
        assert_eq!(outcome(&short).0, "EXTEND");
        // A restart in the middle leaves the fit half the run.
        let halves = full_records(480, |k| u64::from(k > 240));
        let split = verdict("campaign", &plan, &halves, Some(&control), "completed");
        assert_eq!(split["short_run"], json!(true), "{}", split["fitted_hours"]);
        assert_eq!(outcome(&split).0, "EXTEND");
        // The memory guard is a failure.
        let guard = verdict("campaign", &plan, &soak, Some(&control), "memory guard");
        assert_eq!(guard["memory_guard"], json!(true));
        assert_eq!(outcome(&guard).0, "FAIL");
    }

    #[test]
    fn a_control_is_checked_before_a_run_is_judged_against_it() {
        let folder = scratch("control-check");
        let write = |kind: &str, ended: &str, records: Vec<Value>| {
            let _ = std::fs::remove_file(folder.join("records.jsonl"));
            std::fs::write(
                folder.join("soak.json"),
                json!({ "kind": kind, "target": "t", "ended": ended }).to_string(),
            )
            .unwrap();
            for r in records {
                append(&folder.join("records.jsonl"), &r);
            }
        };
        // A completed 2-hour control of the same target.
        write("control", "completed", flat_records(120, 0.0));
        assert_eq!(control_check(&folder, "t", "campaign"), (true, vec![]));
        // Another target, or no control at all: not used.
        assert!(!control_check(&folder, "other", "campaign").0);
        write("campaign", "completed", flat_records(120, 0.0));
        assert!(!control_check(&folder, "t", "campaign").0);
        // Short or stopped: used, and named.
        write("control", "stopped", flat_records(20, 0.0));
        let (usable, faults) = control_check(&folder, "t", "campaign");
        assert!(usable);
        assert_eq!(faults.len(), 2, "{faults:?}");
        // An agent ran: no control.
        let mut records = flat_records(120, 0.0);
        records[60]["agent_log_lines_added"] = json!(3);
        write("control", "completed", records);
        let (usable, faults) = control_check(&folder, "t", "campaign");
        assert!(!usable && faults.len() == 1, "{faults:?}");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_missed_owner_target_fails_the_run() {
        let plan = Plan::live(4.0 * 3600.0);
        let mut records = flat_records(240, 0.0);
        for r in &mut records {
            r["agent_whole"]["share_percent"] = json!(0.31);
        }
        let v = verdict("rehearsal", &plan, &records, None, "completed");
        assert_eq!(v["owner_targets"]["met"], json!(false));
        assert_eq!(outcome(&v).0, "FAIL");
    }

    #[test]
    fn the_agents_interval_is_summed_from_its_reports() {
        let reports = vec![
            json!({ "share": 0.002, "window_s": 10.0, "frames": 1000, "frames_over_250": 2, "frame_max_us": 300.0,
                    "enum_step_max_us": 400.0, "birth_step_max_us": 100.0, "rows": 1000, "pending_destroy": 0 }),
            json!({ "share": 0.001, "window_s": 10.0, "frames": 1000, "frames_over_250": 0, "frame_max_us": 200.0,
                    "enum_step_max_us": 700.0, "birth_step_max_us": 90.0, "rows": 1004, "pending_destroy": 3 }),
        ];
        let a = agent_record(&reports);
        assert_eq!(a["reports"], json!(2));
        assert_eq!(a["enum_step_max_us"], json!(700.0));
        assert_eq!(a["rows"], json!(1004));
        assert_eq!(a["pending_destroy"], json!(3));
        assert!((a["frames_over_250_percent"].as_f64().unwrap() - 0.1).abs() < 1e-9);
        assert!((a["share_percent"].as_f64().unwrap() - 0.15).abs() < 1e-6);
        assert_eq!(agent_record(&[])["reports"], json!(0));
    }

    #[test]
    fn durations_and_commands_are_read() {
        assert_eq!(duration(&["--hours", "8"], 0.0).unwrap(), 28_800.0);
        assert_eq!(
            duration(&["--minutes", "12", "--detach"], 0.0).unwrap(),
            720.0
        );
        assert_eq!(duration(&[], 7_200.0).unwrap(), 7_200.0);
        assert!(duration(&["--hours", "25"], 0.0).is_err());
        assert!(duration(&["--minutes"], 0.0).is_err());
        assert_eq!(
            number(&["--restart-at", "90"], "--restart-at").unwrap(),
            Some(90.0)
        );
        assert!(is_command("rehearse") && is_command("campaign") && !is_command("my-server"));
        // The live plan's turnover: 250 groups in about 48 steps.
        let p = Plan::live(8.0 * 3600.0);
        let per_step = p.handover as f64 + p.replace as f64 + 1.0 / p.kill_every as f64;
        assert!((p.live as f64 / per_step - 48.0).abs() < 1.0);
        assert_eq!(p.warmup_s(), 3_600.0);
        assert_eq!(Plan::live(2.0 * 3600.0).warmup_s(), 1_800.0);
    }
}
