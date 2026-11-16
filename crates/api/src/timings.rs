//! A test diagnostic: the time the sidecar's store writes take, for live
//! measurement (`cargo xtask persistence`). Off unless the environment sets
//! `NANOPROBE_COMMIT_TIMINGS` to a period in whole seconds. Then, once a
//! period, the driver prints one JSON line on standard error:
//!
//! ```text
//! {"ev":"commit_timings","commits":N,"median_us":..,"p99_us":..,"max_us":..,
//!  "checkpoints":[us, ...],"units":N}
//! ```
//!
//! A commit is one [`crate::hub::Inner`] settle: the events, the stores and
//! the server store written, before the step's commands are let through. A
//! checkpoint is the timer's. `units` is the active campaign's unit count
//! when the line is printed. Not part of the product's interface.

use std::time::{Duration, Instant};

/// The environment variable that turns the report on.
pub const VAR: &str = "NANOPROBE_COMMIT_TIMINGS";

/// Write times gathered since the last report.
#[derive(Debug)]
pub struct Timings {
    period: Duration,
    since: Instant,
    commits_us: Vec<u64>,
    checkpoints_us: Vec<u64>,
}

impl Timings {
    /// From [`VAR`]; `None` when it is unset, empty, zero or not a number.
    pub fn from_env() -> Option<Timings> {
        let s: u64 = std::env::var(VAR).ok()?.trim().parse().ok()?;
        (s > 0).then(|| Timings::new(Duration::from_secs(s), Instant::now()))
    }

    /// Empty, the first period starting at `now`.
    pub fn new(period: Duration, now: Instant) -> Timings {
        Timings {
            period,
            since: now,
            commits_us: Vec::new(),
            checkpoints_us: Vec::new(),
        }
    }

    /// One settle's time.
    pub fn commit(&mut self, took: Duration) {
        self.commits_us.push(us(took));
    }

    /// One timer checkpoint's time.
    pub fn checkpoint(&mut self, took: Duration) {
        self.checkpoints_us.push(us(took));
    }

    /// The report line when a period has passed since the last, which
    /// starts the next period; `units` is called only then.
    pub fn due(&mut self, now: Instant, units: impl FnOnce() -> usize) -> Option<String> {
        if now.duration_since(self.since) < self.period {
            return None;
        }
        self.since = now;
        let mut c = std::mem::take(&mut self.commits_us);
        c.sort_unstable();
        let at = |q: f64| -> u64 {
            if c.is_empty() {
                return 0;
            }
            // Nearest rank.
            let i = ((q * c.len() as f64).ceil() as usize).clamp(1, c.len()) - 1;
            c[i]
        };
        let checkpoints: Vec<String> = std::mem::take(&mut self.checkpoints_us)
            .iter()
            .map(u64::to_string)
            .collect();
        Some(format!(
            "{{\"ev\":\"commit_timings\",\"commits\":{},\"median_us\":{},\"p99_us\":{},\"max_us\":{},\"checkpoints\":[{}],\"units\":{}}}",
            c.len(),
            at(0.5),
            at(0.99),
            c.last().copied().unwrap_or(0),
            checkpoints.join(","),
            units()
        ))
    }
}

fn us(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_comes_once_a_period_with_ranks_of_the_commits() {
        let t0 = Instant::now();
        let mut t = Timings::new(Duration::from_secs(10), t0);
        for v in 1..=100 {
            t.commit(Duration::from_micros(v));
        }
        t.checkpoint(Duration::from_micros(7000));
        assert!(t.due(t0 + Duration::from_secs(9), || 0).is_none());
        assert_eq!(
            t.due(t0 + Duration::from_secs(10), || 42).unwrap(),
            r#"{"ev":"commit_timings","commits":100,"median_us":50,"p99_us":99,"max_us":100,"checkpoints":[7000],"units":42}"#
        );
        // The next period starts empty.
        assert_eq!(
            t.due(t0 + Duration::from_secs(20), || 0).unwrap(),
            r#"{"ev":"commit_timings","commits":0,"median_us":0,"p99_us":0,"max_us":0,"checkpoints":[],"units":0}"#
        );
    }
}
