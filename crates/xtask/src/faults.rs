//! `faults <name>`: the S8 fault-injection test on a docker-wine target
//! deployed with `--dev --faults`: the eval channel and fault injection
//! (spec/faults.md). The
//! live run of `lua/tests/faults.lua`.
//!
//! `link-probe` (crates/fake-agent) attaches as the sidecar in the game
//! container, as in `cargo xtask link`, and `nanoprobe inspect` reads the
//! frame number. The agent's faults are armed through the eval channel in the
//! `server` state, where the shim keeps the agent's table; the sidecar's
//! faults are the probe's `--short-command` and `--bad-record`, and a
//! `SIGSTOP`. In order:
//!
//! 1. a Lua error raised from the frame entry: the hook logs it, the frame
//!    number advances, the sidecar stays attached;
//! 2. a 1.5 s stall inside a frame, measured by the agent: the sidecar stays
//!    attached;
//! 3. a flood of 10,000 records of about 1 KiB: the sidecar reads a new log
//!    `GAP`;
//! 4. a lost control record, then a mission reload: a second `HELLO` in the
//!    same attach;
//! 5. the sidecar stopped for 2 s: the agent detaches, and the sidecar
//!    attaches again within 2 s of resuming;
//! 6. a command shorter than its header and five broken inbound rings, each
//!    from a new sidecar: the agent detaches naming the check that failed,
//!    and the sidecar attaches again;
//! 7. a panic inside a frame: the agent is disabled with its reason, the hook
//!    goes on, and DCS still answers the eval channel.
//!
//! Before each step that changes what a player would see (the stall, the
//! flood, the reload, the panic) it checks that no player is connected, and
//! waits up to 30 minutes for the server to be empty. Then DCS is restarted,
//! since a disabled agent stays so until then, and the programs are removed.
//! Writes `bundles/faults-<name>-<time>.json`.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use nanoprobe_agent_link::config::AgentToml;
use serde_json::{Value, json};

use crate::install::{self, AGENT_TOML, EVAL_HOOK, Profile, RUNNING, TOKEN};
use crate::link::{build_linux_bins, linux_path, parse_lines, upload_container};
use crate::profile::Target;
use crate::{bundle, eval, remote};

/// Where the probe and its output go in the container.
const DIR: &str = "/tmp/nanoprobe-faults";
/// The probe's command line, for pkill. The brackets keep the pattern from
/// matching the shell that runs it.
const PROBE: &str = "[l]ink-probe --map";
/// Stop the probe: resume it first, since a stopped process does not act on
/// a TERM.
const STOP_PROBE: &str =
    "pkill -CONT -f '[l]ink-probe --map' ; pkill -f '[l]ink-probe --map' ; true";
/// Print the container's wall time in milliseconds, as the probe's `t`.
const NOW_MS: &str = "/tmp/nanoprobe-faults/link-probe --now-ms";
/// The agent's heartbeat detach (crates/agent-link/src/link.rs).
const HEARTBEAT_DETACH: &str = "detached: the sidecar's heartbeat stopped for 0.5 s";
/// The pass limit for the sidecar's attach after it resumes.
const LIMIT_MS: u64 = 2000;
/// The stall injected, in milliseconds.
const STALL_MS: u64 = 1500;
/// How long a step waits for the server to be empty.
const PLAYER_WAIT: Duration = Duration::from_secs(30 * 60);
/// Each broken inbound ring, in the probe's names, and what the agent must
/// say about it (crates/transport/src/ring.rs).
const BAD_RECORDS: &[(&str, &str)] = &[
    ("past-tail", "record passes the tail"),
    ("over-limit", "len over an eighth of the ring"),
    ("misaligned", "not a multiple of 8"),
    ("past-capacity", "more than the capacity ahead of the head"),
    ("padding", "padding does not end at the end of the ring"),
];

struct Live {
    remote: remote::Remote,
    map: String,
    port: String,
    token: String,
    /// The number of the probe started last.
    n: u32,
}

impl Live {
    fn sh(&self, script: &str, args: &[&str]) -> Result<String, String> {
        let out = self.remote.shell(script, args, b"")?;
        match out.code {
            Some(0) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
            code => Err(format!("exit {code:?}: {}", out.stderr)),
        }
    }

    /// Stop the running probe, if any, and start a new one with `extra`.
    fn start_probe(&mut self, extra: &[&str]) -> Result<(), String> {
        self.sh(STOP_PROBE, &[])?;
        self.n += 1;
        let n = self.n;
        let extra = extra.join(" ");
        self.sh(
            &format!(
                "nohup {DIR}/link-probe --map \"$1\" --port \"$2\" --token-file \"$3\" --seconds 86400 {extra} \
                 > {DIR}/probe-{n}.jsonl 2> {DIR}/probe-{n}.err < /dev/null &"
            ),
            &[&self.map, &self.port, &self.token],
        )
        .map(|_| ())
        .map_err(|e| format!("could not start the probe: {e}"))
    }

    /// The current probe's lines so far.
    fn lines(&self) -> Result<Vec<Value>, String> {
        let text = self.sh(
            &format!("cat {DIR}/probe-{}.jsonl 2>/dev/null || true", self.n),
            &[],
        )?;
        Ok(parse_lines(text.as_bytes()))
    }

    /// Poll the probe's lines every half second until `done` holds.
    fn wait(
        &self,
        what: &str,
        limit: Duration,
        done: impl Fn(&[Value]) -> bool,
    ) -> Result<Vec<Value>, String> {
        let start = Instant::now();
        loop {
            let l = self.lines()?;
            if done(&l) {
                return Ok(l);
            }
            if start.elapsed() > limit {
                return Err(format!("{what}: not within {}s", limit.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// Arm a fault through the eval channel in the `server` state.
    fn inject(&self, name: &str, arg: u64) -> Result<(), String> {
        let source = format!(
            "local shim = __nanoprobe_shim\n\
             if not shim then error('no shim in this state', 0) end\n\
             return shim.agent.inject({name:?}, {arg})\n"
        );
        let r = eval::run(
            &self.remote,
            "server",
            "s8-inject",
            source.as_bytes(),
            Duration::from_secs(30),
        )?;
        if r["ok"] == json!(true)
            && r["reply"]["ok"] == json!(true)
            && r["reply"]["values"][0] == json!(true)
        {
            Ok(())
        } else {
            Err(format!("inject {name}: {r}"))
        }
    }

    /// The agent's frame number, from `nanoprobe inspect`.
    fn frame(&self) -> Result<u64, String> {
        let text = self.sh(
            &format!("{DIR}/nanoprobe inspect \"$1\" --records 0"),
            &[&self.map],
        )?;
        text.lines()
            .find_map(|l| match l.split_whitespace().collect::<Vec<_>>()[..] {
                [_, "frame", v] => v.parse().ok(),
                _ => None,
            })
            .ok_or_else(|| "the inspector printed no frame".to_owned())
    }

    /// The container's wall time, as the logs write it.
    fn wall(&self) -> Result<String, String> {
        Ok(self
            .sh("date -u +%Y-%m-%dT%H:%M:%SZ", &[])?
            .trim()
            .to_owned())
    }

    /// The hook's log records from `since` on, with the file it rotated
    /// into: those of this run, not of earlier ones.
    fn hook_since(&self, since: &str) -> Result<Vec<Value>, String> {
        let mut records = Vec::new();
        for path in ["nanoprobe/logs/hook.log.1", "nanoprobe/logs/hook.log"] {
            if let Some(bytes) = self.remote.read(path)? {
                let text = String::from_utf8_lossy(&bytes);
                records.extend(crate::chaos::records_since(&text, since));
            }
        }
        Ok(records)
    }

    /// Return once no player is connected; wait for that up to
    /// [`PLAYER_WAIT`].
    fn empty(&self, step: &str) -> Result<(), String> {
        let start = Instant::now();
        loop {
            let n = self.remote.players()?;
            if n == 0 {
                return Ok(());
            }
            if start.elapsed() > PLAYER_WAIT {
                return Err(format!(
                    "{step}: {n} player(s) connected for {PLAYER_WAIT:?}; stopped"
                ));
            }
            println!("{step}: {n} player(s) connected; waiting");
            std::thread::sleep(Duration::from_secs(30));
        }
    }
}

fn ev<'a>(l: &'a [Value], name: &'a str) -> impl Iterator<Item = &'a Value> {
    l.iter().filter(move |e| e["ev"] == json!(name))
}

fn count(l: &[Value], name: &str) -> usize {
    ev(l, name).count()
}

/// The first `log` line from `target` whose message starts with `msg`.
fn log_line<'a>(l: &'a [Value], target: &str, msg: &str) -> Option<&'a str> {
    ev(l, "log")
        .filter(|e| e["target"] == json!(target))
        .filter_map(|e| e["msg"].as_str())
        .find(|m| m.starts_with(msg))
}

fn logged(l: &[Value], target: &str, msg: &str) -> bool {
    log_line(l, target, msg).is_some()
}

/// The first log `GAP` with a loss after the first `before` lines.
fn new_gap(l: &[Value], before: usize) -> Option<&Value> {
    l.iter().skip(before).find(|e| {
        e["ev"] == json!("gap") && e["class"] == json!(2) && e["count"].as_u64() > Some(0)
    })
}

/// The probe's commands are answered: the second one `WRONG_CAMPAIGN`.
fn answered(l: &[Value]) -> bool {
    ev(l, "result").any(|e| e["seq"] == json!(2) && e["code"] == json!(24))
}

pub fn command(name: &str, rest: &[&str]) -> Result<ExitCode, String> {
    if !rest.is_empty() {
        return Err(format!("unknown options: {}", rest.join(" ")));
    }
    let target = crate::target(name)?;
    if !matches!(target, Target::DockerWine { .. }) {
        return Err("faults needs a docker-wine target".into());
    }
    let remote = crate::remote(name)?.0;
    let players = remote.players()?;
    if players > 0 {
        return Err(format!("{players} player(s) connected; not running."));
    }
    let toml = remote
        .read(AGENT_TOML)?
        .ok_or("no agent.toml on the target: add `map_path` to the profile and deploy")?;
    let config = AgentToml::parse(&String::from_utf8_lossy(&toml))?;
    let active = install::manifest(&remote)?
        .active
        .ok_or("nothing is deployed on the target")?;
    let running = remote
        .read(RUNNING)?
        .map(|b| String::from_utf8_lossy(&b).trim().to_owned());
    if running.as_deref() != Some(&format!("build={active}")) {
        return Err(format!(
            "DCS runs {}, not the deployed {active}: run `cargo xtask restart {name}` first",
            running.as_deref().unwrap_or("no hook")
        ));
    }
    let dev_deploy = remote.read(EVAL_HOOK)?.is_some();
    if !dev_deploy {
        return Err(format!(
            "the deploy has no eval channel: run `cargo xtask deploy {name} --dev --faults` and restart"
        ));
    }
    let mut live = Live {
        map: linux_path(&config.transport.map_path)?,
        port: config.transport.port.to_string(),
        token: remote.full_path(TOKEN),
        remote,
        n: 0,
    };
    // A build with fault injection refuses an unknown fault by name; a build
    // without it says it has none. Anything else is refused before changing
    // anything.
    match live.inject("none", 0) {
        Err(e) if e.contains("no fault named none") => {}
        Err(e) if e.contains("no fault injection") => {
            return Err(format!(
                "the running build has no fault injection: run `cargo xtask deploy {name} --dev --faults` and restart ({e})"
            ));
        }
        Err(e) => {
            return Err(format!(
                "could not reach the agent through the eval channel: {e}"
            ));
        }
        Ok(()) => return Err("the agent accepted a fault named none".into()),
    }
    let bins = build_linux_bins(&[
        ("nanoprobe-fake-agent", "link-probe"),
        ("nanoprobe-cli", "nanoprobe"),
    ])?;
    live.sh(&format!("rm -rf {DIR}"), &[])?;
    upload_container(&live.remote, DIR, &bins[0], "link-probe")?;
    upload_container(&live.remote, DIR, &bins[1], "nanoprobe")?;

    let mut checks: Vec<Value> = Vec::new();
    let result = run(&mut live, &mut checks);
    let mut report = json!({
        "checks": checks,
        "build": active,
        "dev_deploy": dev_deploy,
        "target": name,
        "code": crate::chaos::provenance(),
    });
    if let Err(e) = &result {
        println!("failed: {e}");
        report["error"] = json!(e);
    }
    // Keep every probe's output, read before the clean-up removes it.
    let mut probes = Vec::new();
    for n in 1..=live.n {
        let text = live
            .sh(
                &format!("cat {DIR}/probe-{n}.jsonl {DIR}/probe-{n}.err 2>/dev/null || true"),
                &[],
            )
            .unwrap_or_default();
        probes.push(json!(text));
    }
    report["probes"] = json!(probes);
    report["cleaned"] = json!(live.sh(&format!("rm -rf {DIR}; {STOP_PROBE}"), &[]).is_ok());
    // The panic leaves the agent disabled until DCS restarts. The restart
    // checks for players itself.
    report["restored"] = match crate::restart(name) {
        Ok(_) => json!(true),
        Err(e) => json!({ "error": e }),
    };
    match crate::facts(&target) {
        Ok(facts) => {
            report["dcs"] = json!(facts.dcs);
            report["wine"] = json!(facts.wine);
        }
        Err(e) => report["dcs"] = json!({ "error": e }),
    }
    report["time"] = json!(bundle::utc_now());
    let pass = result.is_ok()
        && report["checks"]
            .as_array()
            .is_some_and(|c| c.iter().all(|c| c["pass"] == json!(true)));
    report["verdict"] = json!({ "pass": pass });

    let folder = bundle::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(format!("faults-{name}-{}.json", bundle::stamp()));
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(if pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn check(checks: &mut Vec<Value>, fault: &str, pass: bool, detail: String) {
    println!("{} {fault}: {detail}", if pass { "ok  " } else { "FAIL" });
    checks.push(json!({ "fault": fault, "pass": pass, "detail": detail }));
}

/// Run one step. A step that ends in an error is a failed check, and the
/// next step runs: the agent and DCS survived it, or the next check says so.
fn step(
    live: &mut Live,
    checks: &mut Vec<Value>,
    name: &str,
    f: impl FnOnce(&mut Live, &mut Vec<Value>) -> Result<(), String>,
) {
    if let Err(e) = f(live, checks) {
        check(checks, name, false, e);
    }
}

fn run(live: &mut Live, checks: &mut Vec<Value>) -> Result<(), String> {
    let minute = Duration::from_secs(60);
    live.start_probe(&[])?;
    live.wait("answered commands", minute, answered)?;

    // 1. A Lua error from the frame entry. The hook catches it; the next
    // frames pump.
    step(live, checks, "raise", |live, checks| {
        let since = live.wall()?;
        live.inject("raise", 0)?;
        let l = live.wait("the raise", minute, |l| {
            logged(l, "hook", "onSimulationFrame raised")
        })?;
        let before = live.frame()?;
        std::thread::sleep(Duration::from_secs(1));
        let after = live.frame()?;
        let raised = live.hook_since(&since)?.into_iter().find(|r| {
            r["msg"] == json!("onSimulationFrame raised")
                && r["data"]["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("injected Lua error"))
        });
        check(
            checks,
            "raise",
            count(&l, "attached") == 1 && after > before && raised.is_some(),
            format!(
                "hook.log: {}; frame {before} then {after}; {} attach(es)",
                raised.unwrap_or(Value::Null),
                count(&l, "attached")
            ),
        );
        Ok(())
    });

    // 2. A stall inside a frame, over the 0.5 s detach limit. The agent
    // logs how long the frame slept.
    step(live, checks, "stall", |live, checks| {
        live.empty("stall")?;
        live.inject("stall", STALL_MS)?;
        let l = live.wait("the stall's end", minute, |l| {
            logged(l, "agent", "injected fault: stall ended after ")
        })?;
        let stalled: u64 = log_line(&l, "agent", "injected fault: stall ended after ")
            .and_then(|m| m.split_whitespace().nth(5))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        std::thread::sleep(Duration::from_secs(2));
        let l = live.lines()?;
        check(
            checks,
            "stall",
            stalled >= STALL_MS && count(&l, "attached") == 1 && !logged(&l, "agent", "detached"),
            format!(
                "the frame stalled {stalled} ms; {} attach(es), detached: {}",
                count(&l, "attached"),
                logged(&l, "agent", "detached")
            ),
        );
        Ok(())
    });

    // 3. A flood past the ring and the event queue. Only a GAP after it
    // counts; the flood's own record is among the oldest, which go first.
    step(live, checks, "flood", |live, checks| {
        live.empty("flood")?;
        let before = live.lines()?.len();
        live.inject("flood", 10_000)?;
        let l = live.wait("a log GAP after the flood", minute, |l| {
            new_gap(l, before).is_some()
        })?;
        check(
            checks,
            "flood",
            count(&l, "attached") == 1,
            format!(
                "GAP read: {}; {} attach(es)",
                new_gap(&l, before).cloned().unwrap_or(Value::Null),
                count(&l, "attached")
            ),
        );
        Ok(())
    });

    // 4. A lost control record: the next LIFECYCLE, from a reload.
    step(live, checks, "lose-control", |live, checks| {
        live.empty("lose-control")?;
        live.inject("lose-control", 0)?;
        let gen_before = ev(&live.lines()?, "lifecycle")
            .filter_map(|e| e["gen"].as_u64())
            .max()
            .unwrap_or(0);
        let reload = "net.load_mission(DCS.getMissionFilename())\nreturn true\n";
        eval::run(&live.remote, "hook", "s8-reload", reload.as_bytes(), minute)?;
        // The HELLO comes with the stop, before the load ends. The next steps
        // need frames, and a load has none, so wait for the new mission too.
        let l = live.wait(
            "a resync HELLO and the reloaded mission",
            Duration::from_secs(300),
            |l| {
                count(l, "hello") >= 2
                    && ev(l, "lifecycle").any(|e| {
                        e["gen"].as_u64() > Some(gen_before)
                            && matches!(e["state"].as_u64(), Some(3 | 4))
                    })
            },
        )?;
        check(
            checks,
            "lose-control",
            count(&l, "attached") == 1 && logged(&l, "agent", "dropped a control record"),
            format!(
                "{} HELLO(s), {} attach(es), drop logged: {}",
                count(&l, "hello"),
                count(&l, "attached"),
                logged(&l, "agent", "dropped a control record")
            ),
        );
        Ok(())
    });

    // 5. A stalled sidecar: stopped for 2 s, then resumed. The time is
    // taken on the container's clock just before the resume.
    step(live, checks, "stalled-peer", |live, checks| {
        let resumed: u64 = live
            .sh(
                &format!("pkill -STOP -f '{PROBE}'; sleep 2; {NOW_MS}; pkill -CONT -f '{PROBE}'"),
                &[],
            )?
            .trim()
            .parse()
            .map_err(|_| "could not read the resume time")?;
        // The first attach after the resume.
        let again = |l: &[Value]| {
            ev(l, "attached")
                .filter_map(|e| e["t"].as_u64())
                .find(|t| *t >= resumed)
        };
        let l = live.wait("a new attach after the stall", minute, |l| {
            again(l).is_some()
        })?;
        let ms = again(&l).map_or(u64::MAX, |t| t - resumed);
        let reason = live
            .wait("the heartbeat detach", minute, |l| {
                logged(l, "agent", HEARTBEAT_DETACH)
            })
            .is_ok();
        check(
            checks,
            "stalled-peer",
            ms < LIMIT_MS && reason,
            format!("attached again {ms} ms after resuming; heartbeat detach read: {reason}"),
        );
        Ok(())
    });

    // 6. A command shorter than its header, and broken inbound rings, each
    // from a new sidecar.
    step(live, checks, "bad commands and records", |live, checks| {
        live.start_probe(&["--short-command"])?;
        let l = live.wait("a detach after the short command", minute, |l| {
            count(l, "attached") >= 2
                && logged(l, "agent", "a command too short for its header; detaching")
        });
        check(
            checks,
            "short-command",
            l.is_ok(),
            match &l {
                Ok(_) => "the agent said so, detached, and the sidecar attached again".into(),
                Err(e) => e.clone(),
            },
        );
        for (bad, expect) in BAD_RECORDS {
            live.start_probe(&["--bad-record", bad])?;
            let l = live.wait("the broken ring", minute, |l| count(l, "injected") > 0)?;
            let what = ev(&l, "injected")
                .next()
                .and_then(|e| e["what"].as_str())
                .unwrap_or_default()
                .to_owned();
            let l = live.wait("a detach naming the inbound ring", minute, |l| {
                count(l, "attached") >= 2 && logged(l, "agent", "detached: inbound ring")
            });
            let said = match &l {
                Ok(l) => log_line(l, "agent", "detached: inbound ring")
                    .unwrap_or_default()
                    .to_owned(),
                Err(e) => e.clone(),
            };
            check(
                checks,
                &format!("bad-record {bad}"),
                l.is_ok() && what == *bad && said.contains(expect),
                said,
            );
        }
        Ok(())
    });

    // 7. A panic inside a frame.
    step(live, checks, "panic", |live, checks| {
        live.empty("panic")?;
        let since = live.wall()?;
        live.inject("panic", 0)?;
        let l = live.wait("the panic's reason and DISABLED", minute, |l| {
            logged(l, "agent", "disabled: a caught panic")
                && ev(l, "lifecycle").any(|e| e["state"] == json!(6))
        });
        std::thread::sleep(Duration::from_secs(2));
        let went_on = live
            .hook_since(&since)?
            .iter()
            .any(|r| r["msg"] == json!("the agent is disabled; the hook goes on without it"));
        let alive = eval::run(
            &live.remote,
            "hook",
            "s8-alive",
            b"return DCS.getRealTime()\n",
            minute,
        );
        check(
            checks,
            "panic",
            l.is_ok() && went_on && alive.is_ok(),
            format!(
                "reason and DISABLED read: {}; hook went on: {went_on}; DCS answers: {}",
                l.is_ok(),
                alive.is_ok()
            ),
        );
        Ok(())
    });

    Ok(())
}
