//! `nanoprobe top`: a live view of the transport file and its rates.

use std::fmt::Write as _;
use std::time::Duration;

use nanoprobe_transport::layout::status as s;
use nanoprobe_transport::mem::Memory;
use nanoprobe_transport::status::{self, Snapshot};

use crate::util::{self, flag_names, hist_summary, named};

type Hist = [u64; s::HIST_BUCKETS];

/// The values of the last refresh, for the rates.
struct Previous {
    at: Duration,
    boot: u64,
    frame: u64,
    counters: [u64; 5],
    pump: Option<Hist>,
    interval: Option<Hist>,
    syscall: Option<Hist>,
    heartbeat: u64,
    heartbeat_since: Duration,
}

/// The state `top` carries between refreshes.
pub struct Top {
    title: String,
    prev: Option<Previous>,
}

const COUNTERS: [(&str, usize); 5] = [
    ("samples_dropped", s::SAMPLES_DROPPED),
    ("events_dropped", s::EVENTS_DROPPED),
    ("logs_dropped", s::LOGS_DROPPED),
    ("gaps", s::GAPS),
    ("commands_applied", s::COMMANDS_APPLIED),
];

impl Top {
    /// A view titled `title`, usually the file's path.
    pub fn new(title: &str) -> Top {
        Top {
            title: title.to_string(),
            prev: None,
        }
    }

    /// Read the file and render one screen. `now` is the time since the view
    /// started; the rates use the difference to the last call.
    pub fn render(&mut self, live: &impl Memory, file_len: u64, now: Duration) -> String {
        let snap = util::snapshot(live);
        let metrics = util::metrics(live);
        let boot = snap.u64(s::AGENT_BOOT);
        // A new DCS process restarts every counter.
        if self.prev.as_ref().is_some_and(|p| p.boot != boot) {
            self.prev = None;
        }
        let prev = self.prev.as_ref();
        let dt = prev
            .map(|p| now.saturating_sub(p.at).as_secs_f64())
            .filter(|&d| d > 0.0);

        let mut out = String::new();
        let _ = writeln!(out, "nanoprobe top  {}", self.title);
        match status::check(live, file_len) {
            Ok(_) => {}
            Err(e) => {
                let _ = writeln!(out, "header check: FAILED: {e}");
            }
        }
        let lifecycle = u64::from(snap.u32(s::LIFECYCLE));
        let flags = u64::from(snap.u32(s::FLAGS));
        let attached = flags & 1 != 0;
        let _ = writeln!(out, "lifecycle        {}", named("lifecycle", lifecycle));
        let _ = writeln!(
            out,
            "attached         {}   flags {}",
            if attached { "yes" } else { "no" },
            flag_names("agent_flags", flags)
        );
        let _ = writeln!(out, "agent boot       {:#x}", boot);
        let _ = writeln!(
            out,
            "sidecar boot     {:#x} (seen by the agent)",
            snap.u64(s::SIDECAR_BOOT_SEEN)
        );

        let frame = snap.u64(s::FRAME);
        let _ = writeln!(
            out,
            "frame            {frame}   {} frames/s",
            rate(prev.map(|p| p.frame), frame, dt)
        );

        let (pump, interval, syscall) = match &metrics {
            Some(m) => (Some(m.hist[0]), Some(m.hist[1]), Some(m.hist[4])),
            None => (None, None, None),
        };
        let _ = writeln!(
            out,
            "pump time        {}",
            window(prev.and_then(|p| p.pump.as_ref()), pump.as_ref())
        );
        let _ = writeln!(
            out,
            "frame interval   {}",
            window(prev.and_then(|p| p.interval.as_ref()), interval.as_ref())
        );
        let _ = writeln!(
            out,
            "system-call frame {}",
            window(prev.and_then(|p| p.syscall.as_ref()), syscall.as_ref())
        );

        for (label, tail, head, cap) in [
            ("outbound ring", s::OUT_TAIL, s::OUT_HEAD, s::OUT_CAPACITY),
            ("inbound ring", s::IN_TAIL, s::IN_HEAD, s::IN_CAPACITY),
        ] {
            let _ = writeln!(out, "{label:<16} {}", fill(&snap, tail, head, cap));
        }

        let mut counters = [0u64; 5];
        for (i, (name, off)) in COUNTERS.iter().enumerate() {
            let v = snap.u64(*off);
            counters[i] = v;
            let r = rate(prev.map(|p| p.counters[i]), v, dt);
            let _ = writeln!(out, "{name:<16} {v}   {r}/s");
        }
        let _ = writeln!(out, "control queued   {}", snap.u32(s::CONTROL_QUEUED));
        let _ = writeln!(
            out,
            "event queue      {} bytes",
            snap.u64(s::EVENT_QUEUE_BYTES)
        );
        let _ = writeln!(out, "lua heap         {} KiB", snap.u64(s::LUA_HEAP_KIB));

        let heartbeat = snap.u64(s::SIDECAR_HEARTBEAT);
        let heartbeat_since = match prev {
            Some(p) if p.heartbeat == heartbeat => p.heartbeat_since,
            _ => now,
        };
        let state = match prev {
            None => "first sample".to_string(),
            Some(p) if p.heartbeat != heartbeat => "changed since the last refresh".to_string(),
            Some(_) => format!(
                "unchanged for {:.1} s",
                now.saturating_sub(heartbeat_since).as_secs_f64()
            ),
        };
        let _ = writeln!(out, "heartbeat        {heartbeat}   {state}");

        let crumb = snap.u64(s::CRUMB);
        if crumb == 0 {
            let _ = writeln!(out, "breadcrumb       none");
        } else {
            let _ = writeln!(
                out,
                "breadcrumb       {crumb:#x} arg {:#x} at frame {} clock {}",
                snap.u64(s::CRUMB_ARG),
                snap.u64(s::CRUMB_FRAME),
                snap.u64(s::CRUMB_CLOCK)
            );
        }

        self.prev = Some(Previous {
            at: now,
            boot,
            frame,
            counters,
            pump: pump.or_else(|| self.prev.as_ref().and_then(|p| p.pump)),
            interval: interval.or_else(|| self.prev.as_ref().and_then(|p| p.interval)),
            syscall: syscall.or_else(|| self.prev.as_ref().and_then(|p| p.syscall)),
            heartbeat,
            heartbeat_since,
        });
        out
    }
}

/// `12.3` for the change per second, or `-` when there is no earlier value or
/// the counter went back.
fn rate(prev: Option<u64>, now: u64, dt: Option<f64>) -> String {
    match (prev, dt) {
        (Some(p), Some(dt)) if now >= p => format!("{:.1}", (now - p) as f64 / dt),
        _ => "-".into(),
    }
}

/// Bytes in use and the percent of the capacity.
fn fill(snap: &Snapshot, tail: usize, head: usize, cap: usize) -> String {
    let (tail, head, cap) = (snap.u64(tail), snap.u64(head), snap.u64(cap));
    if cap == 0 {
        return "capacity 0".into();
    }
    match tail.checked_sub(head) {
        Some(used) => format!(
            "{used} of {cap} bytes ({:.1}%)",
            used as f64 * 100.0 / cap as f64
        ),
        None => format!("head {head} is past tail {tail}"),
    }
}

/// The histogram of the window between two refreshes, or all time on the first
/// refresh or after a counter went back.
fn window(prev: Option<&Hist>, now: Option<&Hist>) -> String {
    let Some(now) = now else {
        return "unavailable".into();
    };
    let Some(prev) = prev else {
        return format!("{}  (since start)", hist_summary(now));
    };
    let mut diff = [0u64; s::HIST_BUCKETS];
    for i in 0..s::HIST_BUCKETS {
        match now[i].checked_sub(prev[i]) {
            Some(d) => diff[i] = d,
            None => return format!("{}  (since start)", hist_summary(now)),
        }
    }
    hist_summary(&diff)
}
