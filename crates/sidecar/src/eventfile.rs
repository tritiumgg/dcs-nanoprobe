//! An event history in SQLite: a campaign's `events.sqlite`, or the server
//! event log (spec/persistence.md, "Event history").

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::store::DiskError;

/// The layout version in `meta.format`.
pub const FORMAT: u32 = 1;

/// The Protobuf package of the event bodies, in `meta.package`.
pub const PACKAGE: &str = "nanoprobe.v1alpha1";

const TABLES: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS event (seq INTEGER PRIMARY KEY, wall_ms INTEGER NOT NULL, source TEXT UNIQUE, body BLOB NOT NULL);
";

/// One event row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventRow {
    /// Its sequence number.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub wall_ms: u64,
    /// What it was made from, unique in the file: `<agent boot>:<record
    /// number>` for an agent record, `route:<group>:<order revision>:<index
    /// or end>` for route progress; `None` for an event the sidecar made.
    pub source: Option<String>,
    /// The Protobuf encoding of the event.
    pub body: Vec<u8>,
}

/// What [`EventFile::retain`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retained {
    /// Retention is off, or nothing was old enough.
    Kept,
    /// This many events were deleted, and `more` old ones remain.
    Deleted {
        /// How many events this call deleted.
        n: u64,
        /// Whether events older than the cutoff remain, for another call.
        more: bool,
    },
    /// The clock is behind the newest event: nothing was deleted.
    ClockBehind {
        /// The clock, milliseconds since the Unix epoch.
        now_ms: u64,
        /// The newest event's time.
        newest_ms: u64,
    },
}

/// An events file, open, with the rows appended since its last commit.
#[derive(Debug)]
pub struct EventFile {
    path: PathBuf,
    conn: Connection,
    first: Option<u64>,
    next: u64,
    pending: Vec<EventRow>,
    /// The bytes of the bodies of the events in the file, as `meta.bytes`
    /// holds them.
    bytes: u64,
    /// `meta.bytes` is missing from the file: the next commit adds it.
    bytes_unwritten: bool,
}

fn connect(path: &Path) -> Result<Connection, DiskError> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

fn put_meta(conn: &Connection, key: &str, value: &[u8]) -> Result<(), DiskError> {
    conn.execute(
        "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

fn replace_meta(conn: &Connection, key: &str, value: &[u8]) -> Result<(), DiskError> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> Result<Option<Vec<u8>>, DiskError> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

impl EventFile {
    /// Open the events file at `path` for campaign `campaign` (0 for the
    /// server log), making it if it is missing. A file of another campaign,
    /// another format or another package is refused.
    pub fn open(path: &Path, campaign: u64) -> Result<EventFile, DiskError> {
        let conn = connect(path)?;
        {
            // Immediate: the write lock is taken before the first read, so a
            // commit by another connection in between cannot make the
            // upgrade fail at once with SQLITE_BUSY_SNAPSHOT, and a held
            // lock waits under the busy timeout.
            let tx = Transaction::new_unchecked(&conn, TransactionBehavior::Immediate)?;
            tx.execute_batch(TABLES)?;
            put_meta(&tx, "format", FORMAT.to_string().as_bytes())?;
            put_meta(&tx, "campaign_id", campaign.to_string().as_bytes())?;
            put_meta(&tx, "package", PACKAGE.as_bytes())?;
            tx.commit()?;
        }
        let want = |key: &str, value: String| -> Result<(), DiskError> {
            let got = get_meta(&conn, key)?.unwrap_or_default();
            if got != value.as_bytes() {
                return Err(DiskError::Corrupt(format!(
                    "{}: {key} is {}, not {value}",
                    path.display(),
                    String::from_utf8_lossy(&got)
                )));
            }
            Ok(())
        };
        want("format", FORMAT.to_string())?;
        want("campaign_id", campaign.to_string())?;
        want("package", PACKAGE.to_owned())?;
        let (first, last): (Option<i64>, Option<i64>) =
            conn.query_row("SELECT min(seq), max(seq) FROM event", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let kept = get_meta(&conn, "bytes")?
            .and_then(|v| String::from_utf8(v).ok())
            .and_then(|v| v.parse::<u64>().ok());
        let bytes = match kept {
            Some(n) => n,
            None => {
                let n: i64 = conn.query_row(
                    "SELECT coalesce(sum(length(body)), 0) FROM event",
                    [],
                    |r| r.get(0),
                )?;
                u64::try_from(n).unwrap_or(0)
            }
        };
        let mut f = EventFile {
            path: path.to_owned(),
            conn,
            first: first.and_then(|v| u64::try_from(v).ok()),
            next: last
                .and_then(|v| u64::try_from(v).ok())
                .map_or(1, |l| l + 1),
            pending: Vec::new(),
            bytes,
            bytes_unwritten: kept.is_none(),
        };
        if f.bytes_unwritten {
            replace_meta(&f.conn, "bytes", f.bytes.to_string().as_bytes())?;
            f.bytes_unwritten = false;
        }
        Ok(f)
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The first sequence number held, committed or pending.
    pub fn first_seq(&self) -> Option<u64> {
        self.first.or_else(|| self.pending.first().map(|r| r.seq))
    }

    /// The wall-clock time of the first event held, milliseconds since the
    /// Unix epoch; `None` for none.
    pub fn first_wall_ms(&self) -> Option<u64> {
        let at: Option<i64> = self
            .conn
            .query_row("SELECT wall_ms FROM event ORDER BY seq LIMIT 1", [], |r| {
                r.get(0)
            })
            .ok();
        at.and_then(|v| u64::try_from(v).ok())
            .or_else(|| self.pending.first().map(|r| r.wall_ms))
    }

    /// The sequence number the next event takes.
    pub fn next_seq(&self) -> u64 {
        self.next
    }

    /// Append an event, written by the next [`EventFile::commit`]. `seq` must
    /// be [`EventFile::next_seq`].
    pub fn push(&mut self, row: EventRow) {
        debug_assert_eq!(row.seq, self.next);
        self.next = row.seq + 1;
        self.pending.push(row);
    }

    /// Whether events are waiting for the next commit.
    pub fn dirty(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Write every pending event in one transaction. On a failure they stay
    /// pending.
    pub fn commit(&mut self) -> Result<(), DiskError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let added: u64 = self.pending.iter().map(|r| r.body.len() as u64).sum();
        let bytes = self.bytes + added;
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO event (seq, wall_ms, source, body) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for r in &self.pending {
                st.execute(params![r.seq as i64, r.wall_ms as i64, r.source, r.body])?;
            }
        }
        replace_meta(&tx, "bytes", bytes.to_string().as_bytes())?;
        tx.commit()?;
        self.bytes = bytes;
        self.first.get_or_insert(self.pending[0].seq);
        self.pending.clear();
        Ok(())
    }

    /// Whether an event made from `source` is in the file: a record read
    /// again after a crash between the events commit and the store commit
    /// (spec/persistence.md, "A crash between the two commits").
    pub fn has_source(&self, source: &str) -> Result<bool, DiskError> {
        if self
            .pending
            .iter()
            .any(|r| r.source.as_deref() == Some(source))
        {
            return Ok(true);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM event WHERE source = ?1",
                [source],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Up to `limit` committed events from `seq` on, in order.
    pub fn read_from(&self, seq: u64, limit: usize) -> Result<Vec<EventRow>, DiskError> {
        let mut st = self.conn.prepare_cached(
            "SELECT seq, wall_ms, source, body FROM event WHERE seq >= ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = st.query_map(
            params![
                i64::try_from(seq).unwrap_or(i64::MAX),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            |r| {
                Ok(EventRow {
                    seq: u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                    wall_ms: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                    source: r.get(2)?,
                    body: r.get(3)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// The bytes of the events' bodies the file holds, from the running
    /// count (`meta.bytes`).
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Delete at most `max_rows` of the oldest events (spec/persistence.md,
    /// "Event history"): those older than `retention_ms`, and while the
    /// bytes of the bodies kept are over `max_bytes`. The time cutoff is the
    /// earlier of `now_ms` and the newest event's time, less the retention.
    /// The delete takes a prefix in `seq` order
    /// ([`crate::retention::cut`]). The newest event is always kept. A
    /// retention of 0 turns the time rule off and a `max_bytes` of 0 turns
    /// the size rule off; with both off, or with `max_rows` 0, nothing is
    /// deleted. When the clock is behind the newest event the time rule is
    /// skipped and the size rule applies. [`Retained::Deleted`] says whether
    /// more events may remain to delete.
    pub fn retain(
        &mut self,
        retention_ms: u64,
        now_ms: u64,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<Retained, DiskError> {
        if (retention_ms == 0 && max_bytes == 0) || max_rows == 0 {
            return Ok(Retained::Kept);
        }
        self.commit()?;
        let newest: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT seq, wall_ms FROM event ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((newest_seq, newest_ms)) = newest else {
            return Ok(Retained::Kept);
        };
        let newest_ms = u64::try_from(newest_ms).unwrap_or(0);
        let behind = retention_ms != 0 && now_ms < newest_ms;
        let cutoff = (retention_ms != 0 && !behind)
            .then(|| now_ms.min(newest_ms).saturating_sub(retention_ms));
        let excess = if max_bytes == 0 {
            0
        } else {
            self.bytes.saturating_sub(max_bytes)
        };
        let skipped = if behind {
            Retained::ClockBehind { now_ms, newest_ms }
        } else {
            Retained::Kept
        };
        if cutoff.is_none() && excess == 0 {
            return Ok(skipped);
        }
        let mut st = self
            .conn
            .prepare("SELECT seq, wall_ms, length(body) FROM event ORDER BY seq LIMIT ?1")?;
        let rows = st
            .query_map([i64::try_from(max_rows).unwrap_or(i64::MAX)], |r| {
                Ok((
                    u64::try_from(r.get::<_, i64>(0)?).unwrap_or(0),
                    u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                    u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(st);
        let cut = crate::retention::cut(
            &rows,
            max_rows,
            u64::try_from(newest_seq).unwrap_or(0),
            cutoff,
            excess,
        );
        let Some(last) = cut.last else {
            return Ok(skipped);
        };
        let kept = self.bytes.saturating_sub(cut.bytes);
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "DELETE FROM event WHERE seq <= ?1",
            [i64::try_from(last).unwrap_or(i64::MAX)],
        )?;
        replace_meta(&tx, "bytes", kept.to_string().as_bytes())?;
        tx.commit()?;
        self.bytes = kept;
        let first: Option<i64> = self
            .conn
            .query_row("SELECT min(seq) FROM event", [], |r| r.get(0))?;
        self.first = first.and_then(|v| u64::try_from(v).ok());
        Ok(Retained::Deleted {
            n: n as u64,
            more: cut.more,
        })
    }

    /// Make `dest` a copy of the committed events: append the rows it lacks
    /// and delete the rows this file no longer holds (spec/persistence.md,
    /// "Backups"). Runs through a connection of its own.
    ///
    /// Nothing here may need a temporary file: a host with no writable
    /// temporary folder (a `FROM scratch` image run as a user) fails one
    /// with `SQLITE_IOERR_GETTEMPPATH`. The append's bound is read first,
    /// because an `INSERT ... SELECT` that reads its own table buffers every
    /// selected row in a temporary table; and the append is a statement of
    /// its own, outside a transaction, where SQLite keeps no statement
    /// journal (inside one it goes to a temporary file past 64 KiB). The
    /// byte count is removed before the append and written after the
    /// delete, so a copy stopped between them has none and counts its rows
    /// at its next open.
    pub fn copy_into(src: &Path, dest: &Path, campaign: u64) -> Result<(), DiskError> {
        // Made if missing, and checked to be this campaign's.
        drop(EventFile::open(src, campaign)?);
        let d = EventFile::open(dest, campaign)?;
        let src = src
            .to_str()
            .ok_or_else(|| DiskError::Corrupt(format!("path {} is not UTF-8", src.display())))?;
        d.conn.execute("ATTACH DATABASE ?1 AS src", [src])?;
        let r = (|| {
            d.conn
                .execute("DELETE FROM main.meta WHERE key = 'bytes'", [])?;
            let newest: i64 =
                d.conn
                    .query_row("SELECT coalesce(max(seq), 0) FROM main.event", [], |r| {
                        r.get(0)
                    })?;
            d.conn.execute(
                "INSERT INTO main.event (seq, wall_ms, source, body) \
                 SELECT seq, wall_ms, source, body FROM src.event WHERE seq > ?1",
                [newest],
            )?;
            let oldest: i64 =
                d.conn
                    .query_row("SELECT coalesce(min(seq), 0) FROM src.event", [], |r| {
                        r.get(0)
                    })?;
            let tx = d.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM main.event WHERE seq < ?1", [oldest])?;
            let bytes: i64 = tx.query_row(
                "SELECT coalesce(sum(length(body)), 0) FROM main.event",
                [],
                |r| r.get(0),
            )?;
            replace_meta(&tx, "bytes", bytes.to_string().as_bytes())?;
            tx.commit()?;
            Ok::<(), DiskError>(())
        })();
        d.conn.execute("DETACH DATABASE src", [])?;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: u64, wall_ms: u64, source: Option<&str>) -> EventRow {
        EventRow {
            seq,
            wall_ms,
            source: source.map(str::to_owned),
            body: vec![seq as u8],
        }
    }

    /// Opening a file while another connection commits to it must not fail
    /// with "database is locked". A deferred transaction that read and then
    /// wrote failed at once when a commit fell between the two. The race is
    /// timing-dependent, so this is a bounded stress test, not a
    /// deterministic one.
    #[test]
    fn open_waits_for_a_writer_that_commits_in_between() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("events.sqlite");
        let mut f = EventFile::open(&p, 3).expect("open");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let writer = std::thread::spawn(move || {
            let mut seq = 1;
            while !flag.load(std::sync::atomic::Ordering::Relaxed) && seq < 20_000 {
                f.push(row(seq, seq, None));
                f.commit().expect("commit");
                seq += 1;
                // Leave the lock free long enough for the opener's retry.
                std::thread::sleep(std::time::Duration::from_micros(500));
            }
        });
        for _ in 0..200 {
            drop(EventFile::open(&p, 3).expect("open beside a writer"));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().expect("writer");
    }

    #[test]
    fn events_commit_read_back_and_find_their_source() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("events.sqlite");
        let mut f = EventFile::open(&p, 7).expect("open");
        assert_eq!(f.next_seq(), 1);
        f.push(row(1, 10, Some("5:100")));
        f.push(row(2, 11, None));
        assert!(f.has_source("5:100").expect("pending"));
        f.commit().expect("commit");
        drop(f);
        let f = EventFile::open(&p, 7).expect("reopen");
        assert_eq!((f.first_seq(), f.next_seq()), (Some(1), 3));
        assert!(f.has_source("5:100").expect("committed"));
        assert!(!f.has_source("5:101").expect("absent"));
        assert_eq!(f.read_from(2, 10).expect("read"), vec![row(2, 11, None)]);
        // Another campaign's file is refused.
        assert!(matches!(EventFile::open(&p, 8), Err(DiskError::Corrupt(_))));
    }

    #[test]
    fn retention_keeps_the_newest_and_skips_a_clock_behind() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = EventFile::open(&d.path().join("e.sqlite"), 1).expect("open");
        for (seq, at) in [(1, 1_000), (2, 2_000), (3, 3_000)] {
            f.push(row(seq, at, None));
        }
        f.commit().expect("commit");
        // A clock behind the newest event deletes nothing.
        assert_eq!(
            f.retain(500, 2_500, 1_000, 0).expect("retain"),
            Retained::ClockBehind {
                now_ms: 2_500,
                newest_ms: 3_000
            }
        );
        // A clock far ahead counts from the newest event, not from now.
        assert_eq!(
            f.retain(1_500, 1_000_000, 1_000, 0).expect("retain"),
            Retained::Deleted { n: 1, more: false }
        );
        assert_eq!(f.first_seq(), Some(2));
        // Everything older goes, the newest stays.
        assert_eq!(
            f.retain(1, 1_000_000, 1_000, 0).expect("retain"),
            Retained::Deleted { n: 1, more: false }
        );
        assert_eq!(f.first_seq(), Some(3));
        assert_eq!(f.next_seq(), 4);
    }

    #[test]
    fn retention_deletes_in_batches_oldest_first_and_reports_more() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = EventFile::open(&d.path().join("e.sqlite"), 1).expect("open");
        for seq in 1..=10 {
            f.push(row(seq, seq * 100, None));
        }
        f.commit().expect("commit");
        let now = 1_000_000;
        assert_eq!(
            f.retain(1, now, 4, 0).expect("retain"),
            Retained::Deleted { n: 4, more: true }
        );
        assert_eq!(f.first_seq(), Some(5));
        assert_eq!(
            f.retain(1, now, 4, 0).expect("retain"),
            Retained::Deleted { n: 4, more: true }
        );
        // Nine are old; one batch of four more would leave only the newest.
        assert_eq!(
            f.retain(1, now, 4, 0).expect("retain"),
            Retained::Deleted { n: 1, more: false }
        );
        assert_eq!(f.first_seq(), Some(10));
        assert_eq!(f.retain(1, now, 4, 0).expect("retain"), Retained::Kept);
    }

    #[test]
    fn a_full_batch_that_is_deleted_whole_reports_more() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = EventFile::open(&d.path().join("e.sqlite"), 1).expect("open");
        for seq in 1..=5 {
            f.push(row(seq, seq * 100, None));
        }
        f.commit().expect("commit");
        assert_eq!(
            f.retain(1, 1_000_000, 4, 0).expect("retain"),
            Retained::Deleted { n: 4, more: true }
        );
        assert_eq!(f.first_seq(), Some(5));
        // Only the newest remains: nothing more to delete.
        assert_eq!(
            f.retain(1, 1_000_000, 4, 0).expect("retain"),
            Retained::Kept
        );
    }

    #[test]
    fn a_copy_is_extended_and_follows_retention() {
        let d = tempfile::tempdir().expect("dir");
        let src = d.path().join("events.sqlite");
        let dest = d.path().join("copy.sqlite");
        let mut f = EventFile::open(&src, 1).expect("open");
        f.push(row(1, 1, None));
        f.push(row(2, 2, None));
        f.commit().expect("commit");
        EventFile::copy_into(&src, &dest, 1).expect("copy");
        f.push(row(3, 3, None));
        f.commit().expect("commit");
        f.retain(1, 3, 1_000, 0).expect("retain");
        EventFile::copy_into(&src, &dest, 1).expect("copy");
        let c = EventFile::open(&dest, 1).expect("copy");
        let seqs: Vec<u64> = c
            .read_from(0, 10)
            .expect("read")
            .iter()
            .map(|r| r.seq)
            .collect();
        // The copy dropped what the retention deleted.
        assert_eq!(seqs, vec![2, 3]);
    }

    /// A row with a body of `len` bytes.
    fn sized(seq: u64, wall_ms: u64, len: usize) -> EventRow {
        EventRow {
            seq,
            wall_ms,
            source: None,
            body: vec![0; len],
        }
    }

    fn stored_bytes(p: &Path) -> (i64, Vec<u8>) {
        let conn = Connection::open(p).expect("conn");
        let sum = conn
            .query_row(
                "SELECT coalesce(sum(length(body)), 0) FROM event",
                [],
                |r| r.get(0),
            )
            .expect("sum");
        let key = conn
            .query_row("SELECT value FROM meta WHERE key = 'bytes'", [], |r| {
                r.get(0)
            })
            .expect("key");
        (sum, key)
    }

    /// Ten events of 100 bytes, `seq * 100` ms apart.
    fn ten(p: &Path) -> EventFile {
        let mut f = EventFile::open(p, 1).expect("open");
        for seq in 1..=10 {
            f.push(sized(seq, seq * 100, 100));
        }
        f.commit().expect("commit");
        f
    }

    #[test]
    fn the_byte_count_matches_the_rows_after_commits_retention_and_a_reopen() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("e.sqlite");
        let mut f = ten(&p);
        let check = |p: &Path| {
            let (sum, key) = stored_bytes(p);
            assert_eq!(key, sum.to_string().into_bytes());
            sum
        };
        assert_eq!(check(&p), 1_000);
        f.push(sized(11, 1_100, 50));
        f.commit().expect("commit");
        assert_eq!(check(&p), 1_050);
        f.retain(0, 0, 1_000, 500).expect("retain");
        assert_eq!(check(&p), 450);
        drop(f);
        let mut f = EventFile::open(&p, 1).expect("reopen");
        assert_eq!(f.bytes, 450);
        f.push(sized(12, 1_200, 7));
        f.commit().expect("commit");
        assert_eq!(check(&p), 457);
    }

    #[test]
    fn a_size_limit_alone_deletes_the_oldest_until_under_it() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = ten(&d.path().join("e.sqlite"));
        // 1,000 bytes kept over a limit of 650: 350 to delete is 4 rows.
        assert_eq!(
            f.retain(0, 0, 1_000, 650).expect("retain"),
            Retained::Deleted { n: 4, more: false }
        );
        assert_eq!(f.first_seq(), Some(5));
        assert_eq!(f.bytes, 600);
        // Under the limit: nothing.
        assert_eq!(f.retain(0, 0, 1_000, 650).expect("retain"), Retained::Kept);
    }

    #[test]
    fn a_time_limit_alone_ignores_the_size() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = ten(&d.path().join("e.sqlite"));
        // Newest 1,000: cutoff 1,000 - 450 = 550: rows 1..=5 go.
        assert_eq!(
            f.retain(450, 1_000_000, 1_000, 0).expect("retain"),
            Retained::Deleted { n: 5, more: false }
        );
        assert_eq!(f.bytes, 500);
    }

    #[test]
    fn both_limits_delete_what_either_takes() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = ten(&d.path().join("e.sqlite"));
        // Time takes rows 1..=2; the size excess of 500 takes rows 1..=5.
        assert_eq!(
            f.retain(750, 1_000_000, 1_000, 500).expect("retain"),
            Retained::Deleted { n: 5, more: false }
        );
        assert_eq!(f.first_seq(), Some(6));
    }

    #[test]
    fn a_size_limit_keeps_the_newest_event_even_when_it_is_over() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = ten(&d.path().join("e.sqlite"));
        assert_eq!(
            f.retain(0, 0, 1_000, 1).expect("retain"),
            Retained::Deleted { n: 9, more: false }
        );
        assert_eq!(f.first_seq(), Some(10));
        assert_eq!(f.retain(0, 0, 1_000, 1).expect("retain"), Retained::Kept);
    }

    #[test]
    fn a_size_backlog_drains_over_several_passes() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = ten(&d.path().join("e.sqlite"));
        let mut passes = Vec::new();
        while let Retained::Deleted { n, more } = f.retain(0, 0, 3, 200).expect("retain") {
            passes.push((n, more));
            assert!(passes.len() < 10);
        }
        // 800 to delete: batches of 3, 3, 2 (the last is not full).
        assert_eq!(passes, [(3, true), (3, true), (2, false)]);
        assert_eq!(f.bytes, 200);
        assert_eq!(f.first_seq(), Some(9));
    }

    #[test]
    fn the_size_limit_applies_when_the_clock_is_behind() {
        let d = tempfile::tempdir().expect("dir");
        let mut f = ten(&d.path().join("e.sqlite"));
        assert_eq!(
            f.retain(500, 10, 1_000, 0).expect("retain"),
            Retained::ClockBehind {
                now_ms: 10,
                newest_ms: 1_000
            }
        );
        assert_eq!(
            f.retain(500, 10, 1_000, 800).expect("retain"),
            Retained::Deleted { n: 2, more: false }
        );
    }

    #[test]
    fn a_file_without_the_byte_count_gets_it_at_open() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("e.sqlite");
        drop(ten(&p));
        Connection::open(&p)
            .expect("conn")
            .execute("DELETE FROM meta WHERE key = 'bytes'", [])
            .expect("delete");
        let f = EventFile::open(&p, 1).expect("open");
        assert_eq!(f.bytes, 1_000);
        assert_eq!(stored_bytes(&p), (1_000, b"1000".to_vec()));
    }

    #[test]
    fn a_copy_holds_its_own_byte_count() {
        let d = tempfile::tempdir().expect("dir");
        let src = d.path().join("e.sqlite");
        let dest = d.path().join("copy.sqlite");
        let mut f = ten(&src);
        EventFile::copy_into(&src, &dest, 1).expect("copy");
        assert_eq!(stored_bytes(&dest), (1_000, b"1000".to_vec()));
        f.retain(0, 0, 1_000, 600).expect("retain");
        EventFile::copy_into(&src, &dest, 1).expect("copy");
        assert_eq!(stored_bytes(&dest), (600, b"600".to_vec()));
    }
}
