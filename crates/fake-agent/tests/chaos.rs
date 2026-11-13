//! `chaos-runner` against the fake agent as DCS: a run with nothing wrong
//! passes, and a DCS that freezes, or restarts with no kill, fails it with
//! the reason (spec/faults.md, "Chaos"). Unix only: the runner starts DCS
//! through `sh`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use nanoprobe_fake_agent::content::mix;

struct Run {
    dir: PathBuf,
    port: u16,
}

impl Run {
    fn new(name: &str) -> Run {
        let dir =
            std::env::temp_dir().join(format!("nanoprobe-chaos-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), format!("{}\n", "ab".repeat(32))).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .unwrap()
            .port();
        Run { dir, port }
    }

    /// The fake agent's command line.
    fn agent(&self, extra: &str) -> String {
        format!(
            "{} --map {} --port {} --token-file {} {extra}",
            env!("CARGO_BIN_EXE_fake-agent"),
            self.dir.join("map").display(),
            self.port,
            self.dir.join("token").display()
        )
    }

    /// Run the runner with `dcs` as the DCS command; its exit status and
    /// output.
    fn run(&self, dcs: &str, cycles: u64, seed: u64, extra: &[&str]) -> (bool, String) {
        self.run_with(dcs, env!("CARGO_BIN_EXE_link-probe"), cycles, seed, extra)
    }

    /// As [`Run::run`], with `sidecar` as the sidecar's program.
    fn run_with(
        &self,
        dcs: &str,
        sidecar: &str,
        cycles: u64,
        seed: u64,
        extra: &[&str],
    ) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_chaos-runner"))
            .arg("--map")
            .arg(self.dir.join("map"))
            .arg("--dir")
            .arg(self.dir.join("run"))
            .args([
                "--cycles",
                &cycles.to_string(),
                "--seed",
                &format!("{seed:x}"),
            ])
            .args(["--dcs-limit", "20", "--dcs-start", dcs])
            .args(extra)
            .arg("--")
            .arg(sidecar)
            .arg("--map")
            .arg(self.dir.join("map"))
            .args(["--port", &self.port.to_string(), "--token-file"])
            .arg(self.dir.join("token"))
            .args(["--seconds", "600"])
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A seed whose first cycle keeps the pair attached for at least 1.95 s and
/// then kills `victim`, 0 for DCS and 1 for the sidecar (the runner's steady
/// time is 100 + rng % 1900 ms, with rng = mix(seed), and its victim
/// mix(rng) % 2).
fn first_cycle_kills(victim: u64) -> u64 {
    (1..)
        .find(|s: &u64| {
            let rng = mix(*s);
            100 + rng % 1900 >= 1950 && mix(rng) % 2 == victim
        })
        .unwrap()
}

fn long_first_cycle() -> u64 {
    first_cycle_kills(0)
}

/// `script` as a shell command for the runner, which `exec`s it.
fn sh(dir: &Path, script: &str) -> String {
    let path = dir.join("dcs.sh");
    std::fs::write(&path, script).unwrap();
    format!("sh {}", path.display())
}

#[test]
fn a_run_with_nothing_wrong_passes() {
    let r = Run::new("pass");
    let (ok, out) = r.run(&r.agent(""), 4, 1, &[]);
    assert!(ok, "{out}");
    assert!(out.contains("\"ev\":\"end\""), "{out}");
}

#[test]
fn a_dcs_that_stops_pumping_fails_the_run() {
    let r = Run::new("freeze");
    // 0.2 s after attaching, the agent stops for 5 s inside a frame. The
    // runner holds DCS for 10 s before its first kill, so it sees the stop.
    let (ok, out) = r.run(
        &r.agent("--misbehave stall@20:5000"),
        1,
        long_first_cycle(),
        &["--dcs-gap", "10"],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("DCS stopped pumping with no kill"), "{out}");
}

#[test]
fn a_dcs_that_restarts_by_itself_fails_the_run() {
    let r = Run::new("restart");
    // The first DCS dies 2.2 s after it starts and another takes its place,
    // with no kill, in the first cycle's steady time.
    let agent = r.agent("");
    let dcs = sh(
        &r.dir,
        &format!("{agent} & p=$!\nsleep 2.2\nkill -9 $p\nwait $p\nexec {agent}\n"),
    );
    let (ok, out) = r.run(&dcs, 1, long_first_cycle(), &[]);
    assert!(!ok, "{out}");
    assert!(out.contains("DCS restarted with no kill"), "{out}");
}

#[test]
fn a_slow_reconnect_fails_the_run() {
    let r = Run::new("slow");
    // Every sidecar but the first waits 2.5 s before it starts, so the
    // reconnect after the first cycle's sidecar kill takes over 2 s.
    let probe = env!("CARGO_BIN_EXE_link-probe");
    let wrapper = r.dir.join("sidecar.sh");
    std::fs::write(
        &wrapper,
        format!(
            "if [ -e {seen} ]; then sleep 2.5; fi\ntouch {seen}\nexec {probe} \"$@\"\n",
            seen = r.dir.join("seen").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let (ok, out) = r.run_with(
        &r.agent(""),
        wrapper.to_str().unwrap(),
        1,
        first_cycle_kills(1),
        &[],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("to reconnect after killing sidecar"), "{out}");
}
