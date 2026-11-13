//! The replay driver: feeds recorded agent records to the sidecar (plan 15,
//! "Record and replay"). DCS is not deterministic, so a recording drives the
//! sidecar only.
//!
//! A recording is the records the sidecar read, in the batches it read them,
//! with a marker where the sidecar restarted. [`Recording::from_records`]
//! makes one from a plain stream, one batch per `FRAME`.
//!
//! On disk a recording is the stream recorder's format (`link-probe
//! --record`): JSON Lines, one record per line in the order read, padding
//! left out:
//!
//! ```text
//! {"t":<wall ms>,"boot":"<16 hex>","number":N,"kind":N,"payload":"<hex>"}
//! ```
//!
//! The file has no batches and no restart marker: [`Recording::read_jsonl`]
//! makes one batch per `FRAME`, and [`fold`] takes restart points as an
//! argument. A recording of a real server holds server data and stays out of
//! the repository.

use std::io::{self, BufRead, Write};

use nanoprobe_transport::layout::kind;

use crate::reconciler::{Config, Counters, Reconciler};
use crate::sim::RawRecord;
use crate::store::Store;

/// The longest payload a recording may hold: an eighth of the largest ring.
pub const MAX_PAYLOAD: usize = 8 << 20;

/// One batch: what one read returned, or a sidecar restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Batch {
    /// Records, in ring order.
    Records(Vec<RawRecord>),
    /// The sidecar restarted here: its volatile state was lost.
    SidecarRestart,
}

/// A recording.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recording {
    /// The batches, in order.
    pub batches: Vec<Batch>,
}

impl Recording {
    /// A recording of a plain stream of records, one batch per `FRAME`.
    pub fn from_records(records: impl IntoIterator<Item = RawRecord>) -> Recording {
        let mut batches = Vec::new();
        let mut cur = Vec::new();
        for r in records {
            if u64::from(r.kind) == kind::FRAME && !cur.is_empty() {
                batches.push(Batch::Records(std::mem::take(&mut cur)));
            }
            cur.push(r);
        }
        if !cur.is_empty() {
            batches.push(Batch::Records(cur));
        }
        Recording { batches }
    }

    /// Every record, in order.
    pub fn records(&self) -> impl Iterator<Item = &RawRecord> {
        self.batches.iter().flat_map(|b| match b {
            Batch::Records(r) => r.as_slice(),
            Batch::SidecarRestart => &[],
        })
    }

    /// Write the records as JSON Lines, with `t` 0. Restart markers are
    /// not written.
    pub fn write_jsonl(&self, w: &mut impl Write) -> io::Result<()> {
        for r in self.records() {
            let hex: String = r.payload.iter().map(|b| format!("{b:02x}")).collect();
            writeln!(
                w,
                "{{\"t\":0,\"boot\":\"{:016x}\",\"number\":{},\"kind\":{},\"payload\":\"{hex}\"}}",
                r.agent_boot, r.number, r.kind
            )?;
        }
        Ok(())
    }

    /// Read JSON Lines, one batch per `FRAME`. A blank line is skipped; any
    /// other line that is not a record fails, naming its line number.
    pub fn read_jsonl(r: impl BufRead) -> io::Result<Recording> {
        let mut records = Vec::new();
        for (i, line) in r.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let rec = parse_line(&line).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("line {}: not a record", i + 1),
                )
            })?;
            records.push(rec);
        }
        Ok(Recording::from_records(records))
    }
}

/// The text of `"key":` in a flat JSON object, up to the next `,` or `}`, with
/// quotes taken off.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let start = line.find(&pat)? + pat.len();
    let rest = line[start..].trim_start();
    if let Some(q) = rest.strip_prefix('"') {
        q.find('"').map(|end| &q[..end])
    } else {
        let end = rest.find([',', '}'])?;
        Some(rest[..end].trim())
    }
}

fn parse_line(line: &str) -> Option<RawRecord> {
    let line = line.trim();
    if !line.starts_with('{') || !line.ends_with('}') {
        return None;
    }
    let hex = field(line, "payload")?;
    if hex.len() % 2 != 0 || hex.len() / 2 > MAX_PAYLOAD {
        return None;
    }
    let payload = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    Some(RawRecord {
        agent_boot: u64::from_str_radix(field(line, "boot")?, 16).ok()?,
        number: field(line, "number")?.parse().ok()?,
        kind: field(line, "kind")?.parse().ok()?,
        payload,
    })
}

/// Whether the store outlives a sidecar restart (spec/config.md, `[store]
/// enabled`).
#[derive(Clone, Copy)]
pub enum Persistence<'a> {
    /// The store is kept.
    On,
    /// The store is lost; the function makes the one the sidecar starts
    /// with, as the author writes the specs back.
    Off(&'a dyn Fn() -> Store),
}

/// Fold a recording into a store, as a sidecar that reads it and writes
/// nothing. The sidecar restarts at every [`Batch::SidecarRestart`], and at
/// the first `FRAME` at or after each record index in `restarts`: a
/// restarted sidecar reads on from a `FRAME`, whose stamps it applies
/// (spec/transport.md, "Attaching"). An index past the last `FRAME` restarts
/// at the tail. Returns the counters of every reconciler, summed.
pub fn fold(
    cfg: Config,
    store: &mut Store,
    recording: &Recording,
    restarts: &[usize],
    persistence: Persistence<'_>,
) -> Counters {
    let mut rec = Reconciler::new(cfg, store);
    let mut total = Counters::default();
    let restart = |rec: &mut Reconciler, store: &mut Store, total: &mut Counters| {
        total.add(rec.counters());
        if let Persistence::Off(fresh) = persistence {
            *store = fresh();
        }
        *rec = Reconciler::new(cfg, store);
    };
    let mut pending: Vec<usize> = restarts.to_vec();
    pending.sort_unstable();
    let mut i = 0usize;
    for b in &recording.batches {
        match b {
            Batch::SidecarRestart => restart(&mut rec, store, &mut total),
            Batch::Records(rs) => {
                for r in rs {
                    let due = pending.first().is_some_and(|p| *p <= i);
                    if due && u64::from(r.kind) == kind::FRAME {
                        pending.retain(|p| *p > i);
                        restart(&mut rec, store, &mut total);
                    }
                    rec.record(store, r);
                    i += 1;
                }
            }
        }
    }
    if !pending.is_empty() {
        restart(&mut rec, store, &mut total);
    }
    total.add(rec.counters());
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn record() -> impl Strategy<Value = RawRecord> {
        (
            any::<u64>(),
            any::<u64>(),
            any::<u16>(),
            prop::collection::vec(any::<u8>(), 0..64),
        )
            .prop_map(|(agent_boot, number, kind, payload)| RawRecord {
                agent_boot,
                number,
                kind,
                payload,
            })
    }

    proptest! {
        #[test]
        fn records_round_trip_through_json_lines(records in prop::collection::vec(record(), 0..32)) {
            let rec = Recording::from_records(records);
            let mut buf = Vec::new();
            rec.write_jsonl(&mut buf).unwrap();
            prop_assert_eq!(Recording::read_jsonl(buf.as_slice()).unwrap(), rec);
        }

        #[test]
        fn any_line_fails_or_parses_without_a_panic(line in ".{0,200}") {
            let _ = Recording::read_jsonl(line.as_bytes());
        }
    }

    #[test]
    fn a_plain_stream_splits_at_each_frame() {
        let r = |kind: u64, number| RawRecord {
            agent_boot: 1,
            number,
            kind: kind as u16,
            payload: vec![],
        };
        let rec = Recording::from_records([
            r(kind::HELLO, 1),
            r(kind::FRAME, 2),
            r(kind::OBJECT, 3),
            r(kind::FRAME, 4),
        ]);
        assert_eq!(rec.batches.len(), 3);
        assert_eq!(rec.records().count(), 4);
    }

    #[test]
    fn the_recorder_line_parses() {
        let line = r#"{"t":1763000000000,"boot":"00000000000010ab","number":42,"kind":17,"payload":"0700000000000000030000000000000a"}"#;
        let rec = Recording::read_jsonl(line.as_bytes()).unwrap();
        let r = rec.records().next().unwrap();
        assert_eq!((r.agent_boot, r.number, r.kind), (0x10ab, 42, 17));
        assert_eq!(r.payload.len(), 16);
        for bad in [
            r#"{"boot":"zz","number":1,"kind":1,"payload":""}"#,
            r#"{"boot":"1","number":1,"kind":1,"payload":"abc"}"#,
            "nonsense",
        ] {
            assert!(Recording::read_jsonl(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
