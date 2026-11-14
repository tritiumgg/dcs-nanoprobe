//! Measurements a live benchmark takes of a docker-wine target: the process
//! and container sampler, the mover count, and the frame meter.
//!
//! The sampler is a shell loop that runs in the game container beside DCS.
//! Once a second it appends one line to a file: the DCS process's CPU time
//! and resident memory, the container's CPU, memory and throttling, the load
//! average, and the host's temperature. It finds the DCS process by the name
//! of its executable, so it needs no process ID from the profile, and it
//! prints `-` for every figure the machine does not have: a cgroup v1 or v2
//! container, a container with no cgroup files, or a host with no
//! temperature sensor all work.
//!
//! The mover count (`lua/scenarios/movers.lua`) says how many of the
//! scenario's ground groups and ships moved between two calls. The frame
//! meter (`lua/sampling/frames.lua`) counts frames in the hook state and
//! writes a line to `dcs.log` at the first and last frame of its window;
//! [`window_from_log`] reads their millisecond stamps, so the window is tied
//! to the wall clock to a frame. (`os.time()` has whole-second resolution and
//! does not line up with the host's clock.)

use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};

use crate::remote::Remote;
use crate::sampling::live::Run;

/// The sampler. Arguments: the folder it writes in, and optionally the
/// executable names that count as DCS, separated by spaces, lower case.
/// It writes `samples.txt` in the folder until the file `stop` exists
/// there. Its output starts with `hz <ticks per second>` and `tz <offset>`
/// lines, then one line per second of 16 space-separated fields, `-` where a
/// source is missing: wall clock (epoch seconds); the DCS process's utime and
/// stime (ticks; `gone` while no such process runs); the container's CPU
/// time (µs); the process's resident memory (kB); the core it last ran on;
/// that core's frequency and the mean over all cores (kHz); the 1-minute
/// load average; the container's memory use and inactive file cache
/// (bytes; `docker stats` shows their difference); the container's throttled
/// periods and throttled time (µs); the hottest package and core
/// temperatures (millidegrees); the host's available memory (kB).
const SAMPLER: &str = r#"D=$1; N=${2:-dcs.exe dcs_server.exe}
hz=$(getconf CLK_TCK 2>/dev/null); echo "hz ${hz:-100}"
echo "tz $(date +%z)"
G=/sys/fs/cgroup
if [ -r $G/cpu.stat ]; then V=2; elif [ -r $G/cpuacct/cpuacct.usage ] || [ -r $G/cpu,cpuacct/cpuacct.usage ]; then V=1; else V=0; fi
CA=; for d in $G/cpuacct $G/cpu,cpuacct; do [ -r $d/cpuacct.usage ] && CA=$d; done
CS=; for d in $G/cpu $G/cpu,cpuacct; do [ -r $d/cpu.stat ] && CS=$d; done
H=; for h in /sys/class/hwmon/hwmon*; do [ "$(cat $h/name 2>/dev/null)" = coretemp ] && H=$h; done
find_pid() {
  best=; most=-1
  for p in /proc/[0-9]*; do
    a=$(tr '\0' '\n' <$p/cmdline 2>/dev/null | head -n 1 | tr '\\' '/')
    [ -n "$a" ] || continue
    b=$(printf %s "${a##*/}" | tr 'A-Z' 'a-z')
    case " $N " in *" $b "*) ;; *) continue ;; esac
    k=$(sed -n 's/^VmRSS:[^0-9]*\([0-9]*\).*/\1/p' $p/status 2>/dev/null)
    if [ "${k:-0}" -gt "$most" ]; then most=${k:-0}; best=${p#/proc/}; fi
  done
  echo "$best"
}
P=
while [ ! -e "$D/stop" ]; do
  t=$(date +%s.%N); case $t in *[!0-9.]*) t=$(date +%s) ;; esac
  [ -n "$P" ] && [ ! -r /proc/$P/stat ] && P=
  [ -n "$P" ] || P=$(find_pid)
  s="gone gone"; core=; rss=
  if [ -n "$P" ]; then
    st=$(sed 's/.*) //' /proc/$P/stat 2>/dev/null)
    if [ -n "$st" ]; then
      s=$(echo "$st" | cut -d' ' -f12,13); core=$(echo "$st" | cut -d' ' -f37)
      rss=$(sed -n 's/^VmRSS:[^0-9]*\([0-9]*\).*/\1/p' /proc/$P/status 2>/dev/null)
    fi
  fi
  u=; thn=; thu=; mc=; mi=
  if [ $V = 2 ]; then
    u=$(sed -n 's/^usage_usec //p' $G/cpu.stat 2>/dev/null)
    thn=$(sed -n 's/^nr_throttled //p' $G/cpu.stat 2>/dev/null)
    thu=$(sed -n 's/^throttled_usec //p' $G/cpu.stat 2>/dev/null)
    mc=$(cat $G/memory.current 2>/dev/null)
    mi=$(sed -n 's/^inactive_file //p' $G/memory.stat 2>/dev/null)
  elif [ $V = 1 ]; then
    n=$(cat $CA/cpuacct.usage 2>/dev/null); [ -n "$n" ] && u=$((n / 1000))
    [ -n "$CS" ] && thn=$(sed -n 's/^nr_throttled //p' $CS/cpu.stat 2>/dev/null)
    n=; [ -n "$CS" ] && n=$(sed -n 's/^throttled_time //p' $CS/cpu.stat 2>/dev/null); [ -n "$n" ] && thu=$((n / 1000))
    mc=$(cat $G/memory/memory.usage_in_bytes 2>/dev/null)
    mi=$(sed -n 's/^total_inactive_file //p' $G/memory/memory.stat 2>/dev/null)
  fi
  fc=; [ -n "$core" ] && fc=$(cat /sys/devices/system/cpu/cpu$core/cpufreq/scaling_cur_freq 2>/dev/null)
  fa=$(cat /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq 2>/dev/null | awk '{s+=$1;n++} END{if(n) printf "%d", s/n}')
  la=$(cut -d' ' -f1 /proc/loadavg 2>/dev/null)
  pk=; cm=
  if [ -n "$H" ]; then
    pk=$(cat $H/temp1_input 2>/dev/null)
    cm=$(cat $H/temp[2-9]_input 2>/dev/null | awk 'BEGIN{m=0} {if($1>m)m=$1} END{if(NR) print m}')
  fi
  ma=$(sed -n 's/^MemAvailable:[^0-9]*\([0-9]*\).*/\1/p' /proc/meminfo 2>/dev/null)
  echo "$t $s ${u:--} ${rss:--} ${core:--} ${fc:--} ${fa:--} ${la:--} ${mc:--} ${mi:--} ${thn:--} ${thu:--} ${pk:--} ${cm:--} ${ma:--}"
  sleep 1
done
"#;

/// The fields of a sampler line.
const FIELDS: usize = 16;

/// One line of the sampler. A missing source is `None`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sample {
    /// Wall clock, epoch seconds.
    pub t: f64,
    /// The DCS process's user and system CPU time, ticks.
    pub utime: Option<u64>,
    pub stime: Option<u64>,
    /// The container's CPU time, µs.
    pub cgroup_cpu_us: Option<u64>,
    /// The DCS process's resident memory, kB.
    pub rss_kb: Option<u64>,
    pub core: Option<u64>,
    pub freq_core_khz: Option<u64>,
    pub freq_mean_khz: Option<u64>,
    pub load1: Option<f64>,
    pub mem_current: Option<u64>,
    pub mem_inactive: Option<u64>,
    pub nr_throttled: Option<u64>,
    pub throttled_us: Option<u64>,
    pub temp_pkg_mdeg: Option<u64>,
    pub temp_core_mdeg: Option<u64>,
    pub mem_available_kb: Option<u64>,
}

/// What a sampler file holds: its header and its samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// Clock ticks per second of the process times.
    pub hz: f64,
    /// The container's offset from UTC, seconds (`date +%z`), 0 when unread.
    pub utc_offset_s: i64,
    pub samples: Vec<Sample>,
}

fn opt<T: std::str::FromStr>(field: &str) -> Option<T> {
    if field == "-" || field == "gone" {
        None
    } else {
        field.parse().ok()
    }
}

/// An offset such as `+0100` or `-0530`, in seconds.
fn parse_offset(text: &str) -> Option<i64> {
    let (sign, digits) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Parse a sampler file's text. Lines that are cut off, as the last line of
/// a file being written can be, or that have another field count, are
/// skipped.
pub fn parse_sampler(text: &str) -> Parsed {
    let mut parsed = Parsed {
        hz: 100.0,
        utc_offset_s: 0,
        samples: Vec::new(),
    };
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["hz", n] => {
                if let Some(n) = n.parse::<f64>().ok().filter(|n| *n > 0.0) {
                    parsed.hz = n;
                }
            }
            ["tz", z] => parsed.utc_offset_s = parse_offset(z).unwrap_or(0),
            _ if f.len() == FIELDS => {
                let Some(t) = opt::<f64>(f[0]) else { continue };
                parsed.samples.push(Sample {
                    t,
                    utime: opt(f[1]),
                    stime: opt(f[2]),
                    cgroup_cpu_us: opt(f[3]),
                    rss_kb: opt(f[4]),
                    core: opt(f[5]),
                    freq_core_khz: opt(f[6]),
                    freq_mean_khz: opt(f[7]),
                    load1: opt(f[8]),
                    mem_current: opt(f[9]),
                    mem_inactive: opt(f[10]),
                    nr_throttled: opt(f[11]),
                    throttled_us: opt(f[12]),
                    temp_pkg_mdeg: opt(f[13]),
                    temp_core_mdeg: opt(f[14]),
                    mem_available_kb: opt(f[15]),
                });
            }
            _ => {}
        }
    }
    parsed
}

/// The time and value of the first and last of the samples for which `get`
/// has a value: `(first_t, first, last_t, last)`.
fn ends<T: Copy>(
    samples: &[Sample],
    get: impl Fn(&Sample) -> Option<T>,
) -> Option<(f64, T, f64, T)> {
    let mut have = samples.iter().filter_map(|s| get(s).map(|v| (s.t, v)));
    let first = have.next()?;
    let last = have.next_back().unwrap_or(first);
    Some((first.0, first.1, last.0, last.1))
}

/// A counter's rate between the first and last sample that has it, per
/// second, or `None` when the span is shorter than a second or the counter
/// went backwards (a restarted process).
fn rate(samples: &[Sample], get: impl Fn(&Sample) -> Option<f64>) -> Option<f64> {
    let (t0, x, t1, y) = ends(samples, get)?;
    let dt = t1 - t0;
    (dt >= 1.0 && y >= x).then(|| (y - x) / dt)
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

/// One window of samples as a record. CPU is in percent of one core, as
/// `top` shows it: a process using two cores reads 200. A key is `null`
/// when the machine gave no figure for it.
pub fn summarize(samples: &[Sample], hz: f64) -> Value {
    let process_cpu = rate(samples, |s| Some((s.utime? + s.stime?) as f64 / hz)).map(|r| r * 100.0);
    let cgroup_cpu = rate(samples, |s| Some(s.cgroup_cpu_us? as f64 / 1e6)).map(|r| r * 100.0);
    let rss: Vec<f64> = samples
        .iter()
        .filter_map(|s| s.rss_kb.map(|k| k as f64))
        .collect();
    let throttled = ends(samples, |s| s.nr_throttled).map(|(_, a, _, b)| b.saturating_sub(a));
    let throttled_us = ends(samples, |s| s.throttled_us).map(|(_, a, _, b)| b.saturating_sub(a));
    let working_set =
        mean(samples.iter().filter_map(|s| {
            Some(s.mem_current?.saturating_sub(s.mem_inactive.unwrap_or(0)) as f64)
        }));
    json!({
        "samples": samples.len(),
        "process_gone": samples.iter().filter(|s| s.utime.is_none()).count(),
        "process_cpu_percent": process_cpu,
        "cgroup_cpu_percent": cgroup_cpu,
        "rss_kb_mean": mean(rss.iter().copied()),
        "rss_kb_max": rss.iter().copied().fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v)))),
        "load1_mean": mean(samples.iter().filter_map(|s| s.load1)),
        "cgroup_working_set_bytes_mean": working_set,
        "nr_throttled": throttled,
        "throttled_us": throttled_us,
        "freq_mean_khz": mean(samples.iter().filter_map(|s| s.freq_mean_khz.map(|k| k as f64))),
        "temp_core_max_mdeg": samples.iter().filter_map(|s| s.temp_core_mdeg).max(),
        "mem_available_kb_min": samples.iter().filter_map(|s| s.mem_available_kb).min(),
    })
}

/// The sampler's remote side: where it runs and how it is started, read
/// and stopped.
pub struct Sampler<'a> {
    remote: &'a Remote,
    dir: String,
}

impl<'a> Sampler<'a> {
    /// A sampler that writes in the container folder `dir`.
    pub fn new(remote: &'a Remote, dir: &str) -> Sampler<'a> {
        Sampler {
            remote,
            dir: dir.to_owned(),
        }
    }

    /// Upload the script and start it in the background.
    pub fn start(&self) -> Result<(), String> {
        let d = &self.dir;
        let up = self.remote.shell(
            &format!("rm -rf {d}; mkdir -p {d} && cat > {d}/sampler.sh"),
            &[],
            SAMPLER.as_bytes(),
        )?;
        if up.code != Some(0) {
            return Err(format!("could not upload the sampler: {}", up.stderr));
        }
        let run = self.remote.shell(
            &format!(
                "nohup sh {d}/sampler.sh {d} > {d}/samples.txt 2> {d}/sampler.err < /dev/null &"
            ),
            &[],
            b"",
        )?;
        if run.code != Some(0) {
            return Err(format!("could not start the sampler: {}", run.stderr));
        }
        Ok(())
    }

    /// The file's lines from line `from` (1-based) on, with the header
    /// lines always included.
    pub fn read(&self, from: usize) -> Result<String, String> {
        let d = &self.dir;
        let o = self.remote.shell(
            &format!(
                "head -n 2 {d}/samples.txt; tail -n +{} {d}/samples.txt",
                from.max(3)
            ),
            &[],
            b"",
        )?;
        if o.code != Some(0) {
            return Err(format!("could not read the sampler: {}", o.stderr));
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// Stop it, return its whole file, and remove the folder.
    pub fn finish(&self) -> Result<String, String> {
        let d = &self.dir;
        let _ = self.remote.shell(&format!("touch {d}/stop"), &[], b"");
        std::thread::sleep(Duration::from_secs(2));
        let o = self.remote.shell(
            &format!("cat {d}/samples.txt; pkill -f '[s]ampler.sh {d}' || true; rm -rf {d}"),
            &[],
            b"",
        )?;
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

/// The mover count at this moment (`lua/scenarios/movers.lua`): `groups`,
/// `moved`, `still`, `new` and `share` since the previous call. `None` when
/// the scenario has no ground group or the call fails.
pub fn movers(run: &Run) -> Option<Value> {
    let v = run
        .eval(
            "server",
            "movers",
            "local E = ...\nreturn E.lib('movers').count({ prefix = 'np-' })\n",
            60,
        )
        .ok()?;
    v.is_object().then_some(v)
}

/// The stamp of a `dcs.log` line, `YYYY-MM-DD HH:MM:SS.mmm`, as seconds
/// since the epoch in the log's own time zone read as UTC. Subtract the
/// container's offset ([`Parsed::utc_offset_s`]) for epoch seconds.
pub fn parse_log_stamp(line: &str) -> Option<f64> {
    let b = line.as_bytes();
    if b.len() < 23 || b[4] != b'-' || b[7] != b'-' || b[10] != b' ' || b[19] != b'.' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| line.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, s, ms) = (n(11..13)?, n(14..16)?, n(17..19)?, n(20..23)?);
    // Howard Hinnant's days_from_civil.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((days * 86_400 + h * 3600 + mi * 60 + s) as f64 + ms as f64 / 1000.0)
}

/// A frame meter window as `dcs.log` stamps it.
#[derive(Debug, Clone, PartialEq)]
pub struct LogWindow {
    /// Log-stamp seconds of the first and last frame (see
    /// [`parse_log_stamp`]).
    pub start: f64,
    pub end: f64,
    /// `DCS.getRealTime()` at the first and last frame.
    pub real_start: f64,
    pub real_end: f64,
    /// Frame intervals the meter counted in the window: the frames after the
    /// first, so `frames` over `wall_s` is the frame rate.
    pub frames: u64,
}

impl LogWindow {
    /// The window's length by the log's stamps, seconds.
    pub fn wall_s(&self) -> f64 {
        self.end - self.start
    }

    /// Frames over the stamped length: the frame rate, timed by the log.
    pub fn fps(&self) -> Option<f64> {
        (self.wall_s() > 0.0).then(|| self.frames as f64 / self.wall_s())
    }

    /// How far the meter's own clock and the log's differ over the window,
    /// seconds. Stamps carry whole milliseconds, and the two lines are
    /// written in different frames, so a few milliseconds is normal.
    pub fn clock_error_s(&self) -> f64 {
        self.wall_s() - (self.real_end - self.real_start)
    }

    /// The window as a record. The stamps are in the log's time zone read as
    /// UTC; subtract the container's offset (`tz_offset_s` in the sampler's
    /// header) for epoch seconds.
    pub fn json(&self) -> Value {
        json!({
            "start_stamp": self.start,
            "end_stamp": self.end,
            "wall_s": self.wall_s(),
            "frames": self.frames,
            "fps": self.fps(),
            "clock_error_s": self.clock_error_s(),
        })
    }
}

/// The last window the frame meter wrote under `tag` in `text`, a stretch
/// of `dcs.log`. `None` when the start and end lines are not both there.
pub fn window_from_log(text: &str, tag: &str) -> Option<LogWindow> {
    let re = Regex::new(&format!(
        r"NANOPROBE-METER[^:]*: {} (start|end) real=([0-9.]+) model=[0-9.]+(?: frames=(\d+))?",
        regex::escape(tag)
    ))
    .ok()?;
    let mut start = None;
    let mut found = None;
    for line in text.lines() {
        let Some(c) = re.captures(line) else { continue };
        let Some(stamp) = parse_log_stamp(line) else {
            continue;
        };
        let real: f64 = c[2].parse().ok()?;
        if &c[1] == "start" {
            start = Some((stamp, real));
        } else if let (Some((s, r)), Some(frames)) = (start.take(), c.get(3)) {
            found = Some(LogWindow {
                start: s,
                end: stamp,
                real_start: r,
                real_end: real,
                frames: frames.as_str().parse().ok()?,
            });
        }
    }
    found
}

/// `tag` with a suffix of this run's own, so that a window an earlier run
/// left in `dcs.log` under the same tag is never read as this run's.
fn run_tag(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{tag}-{:x}-{nanos:x}-{n}", std::process::id())
}

/// Run the frame meter (`lua/sampling/frames.lua`): settle, then count
/// frames for `measure` seconds with the simulation running, and read the
/// window's wall-clock stamps from `dcs.log`. The record is the meter's own
/// reply with `log` added when the two log lines were found.
pub fn frames(run: &Run, tag: &str, settle: u32, measure: u32) -> Result<Value, String> {
    let tag = run_tag(tag);
    let source = std::fs::read_to_string(run.repository.join("lua/sampling/frames.lua"))
        .map_err(|e| format!("lua/sampling/frames.lua: {e}"))?
        .replace("@SETTLE@", &settle.to_string())
        .replace("@MEASURE@", &measure.to_string())
        .replace("@TAG@", &tag);
    let mut v = run.eval(
        "hook",
        "meter-frames",
        &source,
        u64::from(settle + measure) + 120,
    )?;
    let o = run.remote.shell(
        "grep -h 'NANOPROBE-METER' \"$1\" | tail -n 4",
        &[&run.remote.full_path("Logs/dcs.log")],
        b"",
    )?;
    let window = window_from_log(&String::from_utf8_lossy(&o.stdout), &tag);
    if let Some(w) = window {
        v["log"] = w.json();
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "hz 100\ntz +0200\n\
        1000.0 100 20 5000000 2000000 3 3000000 2900000 1.5 8000000000 1000000000 4 300000 55000 60000 9000000\n\
        1001.0 150 30 6000000 2000100 3 3000000 2900000 1.6 8000000000 1000000000 6 500000 56000 61000 9000000\n\
        1002.0 gone gone 6500000 - - - - 1.7 - - 6 500000 - - -\n\
        1003.0 200 4";

    #[test]
    fn the_sampler_file_is_parsed_with_missing_figures_and_a_cut_last_line() {
        let p = parse_sampler(FILE);
        assert_eq!(p.hz, 100.0);
        assert_eq!(p.utc_offset_s, 7200);
        assert_eq!(p.samples.len(), 3, "the cut line is skipped");
        assert_eq!(p.samples[0].utime, Some(100));
        assert_eq!(p.samples[0].rss_kb, Some(2_000_000));
        assert_eq!(p.samples[0].temp_core_mdeg, Some(60_000));
        assert_eq!(p.samples[2].utime, None, "gone");
        assert_eq!(p.samples[2].rss_kb, None);
        assert_eq!(p.samples[2].load1, Some(1.7));
    }

    #[test]
    fn a_machine_with_no_cgroup_or_sensor_gives_a_sample() {
        let line = "1000.0 100 20 - 2000000 - - - 0.5 - - - - - - 9000000";
        let p = parse_sampler(line);
        assert_eq!(p.samples.len(), 1);
        let s = &p.samples[0];
        assert_eq!(s.cgroup_cpu_us, None);
        assert_eq!(s.temp_pkg_mdeg, None);
        assert_eq!(s.nr_throttled, None);
        let v = summarize(&p.samples, p.hz);
        assert!(v["cgroup_cpu_percent"].is_null());
        assert_eq!(v["rss_kb_mean"], json!(2_000_000.0));
    }

    #[test]
    fn a_header_with_a_bad_offset_reads_as_zero() {
        assert_eq!(parse_sampler("tz bogus").utc_offset_s, 0);
        assert_eq!(parse_sampler("tz -0530").utc_offset_s, -19_800);
        assert_eq!(parse_sampler("hz 0").hz, 100.0);
    }

    #[test]
    fn cpu_is_a_percent_of_one_core_over_the_window() {
        let p = parse_sampler(FILE);
        // 50 + 10 ticks at 100 Hz over 1 s of the first two samples: 60 %.
        let v = summarize(&p.samples[..2], p.hz);
        assert!((v["process_cpu_percent"].as_f64().unwrap() - 60.0).abs() < 1e-9);
        // 1,000,000 µs of container CPU in 1 s: 100 %.
        assert!((v["cgroup_cpu_percent"].as_f64().unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(v["nr_throttled"], json!(2));
        assert_eq!(v["throttled_us"], json!(200_000));
        assert_eq!(v["temp_core_max_mdeg"], json!(61_000));
        assert_eq!(v["process_gone"], json!(0));
    }

    #[test]
    fn a_restarted_process_gives_no_cpu_rate() {
        let mut p = parse_sampler(FILE);
        p.samples[1].utime = Some(1);
        p.samples[1].stime = Some(1);
        let v = summarize(&p.samples[..2], p.hz);
        assert!(v["process_cpu_percent"].is_null());
    }

    #[test]
    fn a_log_stamp_is_read_to_the_millisecond() {
        let t = parse_log_stamp("2026-11-13 14:19:03.123 INFO    NANOPROBE (Main): x").unwrap();
        // 2026-11-13 00:00:00 UTC is day 20,770 after the epoch (`date -u -d 2026-11-13 +%s` is 1,794,528,000).
        assert_eq!(t, 20_770.0 * 86_400.0 + 14.0 * 3600.0 + 19.0 * 60.0 + 3.123);
        assert_eq!(parse_log_stamp("no stamp here"), None);
        assert_eq!(parse_log_stamp("2026-13-45 xx"), None);
        let leap = parse_log_stamp("2028-03-01 00:00:00.000 a").unwrap()
            - parse_log_stamp("2028-02-29 00:00:00.000 a").unwrap();
        assert_eq!(leap, 86_400.0);
    }

    #[test]
    fn the_frame_meter_window_is_timed_by_the_log_stamps() {
        let log = "\
2026-11-13 14:19:00.000 INFO    SCRIPTING (Main): other line\n\
2026-11-13 14:19:03.100 INFO    NANOPROBE-METER (Main): old start real=1.000 model=1.000\n\
2026-11-13 14:19:33.150 INFO    NANOPROBE-METER (Main): old end real=31.050 model=31.000 frames=100\n\
2026-11-13 14:20:03.000 INFO    NANOPROBE-METER (Main): run start real=100.000 model=90.000\n\
2026-11-13 14:20:33.500 INFO    NANOPROBE-METER (Main): run end real=130.504 model=120.500 frames=1830\n";
        let w = window_from_log(log, "run").unwrap();
        assert_eq!(w.frames, 1830);
        assert!((w.wall_s() - 30.5).abs() < 1e-6);
        assert!((w.fps().unwrap() - 60.0).abs() < 1e-6);
        assert!((w.clock_error_s() - (-0.004)).abs() < 1e-6);
        assert_eq!(window_from_log(log, "missing"), None);
        // A start with no end is no window.
        let open = "2026-11-13 14:20:03.000 INFO    NANOPROBE-METER (Main): run start real=1.0 model=1.0\n";
        assert_eq!(window_from_log(open, "run"), None);
        let j = w.json();
        assert_eq!(j["frames"], json!(1830));
        // Each run's tag is its own, so an earlier run's window never matches.
        assert_ne!(run_tag("run"), run_tag("run"));
        assert!(run_tag("run").starts_with("run-"));
    }
}
