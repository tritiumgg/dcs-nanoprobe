//! An event history in SQLite: a campaign's `events.sqlite`, or the server
//! event log (spec/persistence.md, "Event history").

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

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
    /// The agent record it was made from: agent boot and record number.
    pub source: Option<(u64, u64)>,
    /// The Protobuf encoding of the event.
    pub body: Vec<u8>,
}

/// What [`EventFile::retain`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retained {
    /// Retention is off, or nothing was old enough.
    Kept,
    /// This many events were deleted.
    Deleted(u64),
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
}

fn source_text(s: Option<(u64, u64)>) -> Option<String> {
    s.map(|(boot, n)| format!("{boot}:{n}"))
}

fn source_of(t: Option<String>) -> Option<(u64, u64)> {
    let t = t?;
    let (a, b) = t.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
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
            let tx = conn.unchecked_transaction()?;
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
        Ok(EventFile {
            path: path.to_owned(),
            conn,
            first: first.and_then(|v| u64::try_from(v).ok()),
            next: last
                .and_then(|v| u64::try_from(v).ok())
                .map_or(1, |l| l + 1),
            pending: Vec::new(),
        })
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The first sequence number held, committed or pending.
    pub fn first_seq(&self) -> Option<u64> {
        self.first.or_else(|| self.pending.first().map(|r| r.seq))
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
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO event (seq, wall_ms, source, body) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for r in &self.pending {
                st.execute(params![
                    r.seq as i64,
                    r.wall_ms as i64,
                    source_text(r.source),
                    r.body
                ])?;
            }
        }
        tx.commit()?;
        self.first.get_or_insert(self.pending[0].seq);
        self.pending.clear();
        Ok(())
    }

    /// Whether an event made from this agent record is in the file: a record
    /// read again after a crash between the events commit and the store
    /// commit (spec/persistence.md, "A crash between the two commits").
    pub fn has_source(&self, agent_boot: u64, number: u64) -> Result<bool, DiskError> {
        if self
            .pending
            .iter()
            .any(|r| r.source == Some((agent_boot, number)))
        {
            return Ok(true);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM event WHERE source = ?1",
                [format!("{agent_boot}:{number}")],
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
                    source: source_of(r.get(2)?),
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

    /// Delete the events older than `retention` (spec/persistence.md, "Event
    /// history"): the cutoff is the earlier of `now_ms` and the newest
    /// event's time, less the retention. The newest event is always kept.
    /// Nothing is deleted when the clock is behind the newest event.
    pub fn retain(&mut self, retention_ms: u64, now_ms: u64) -> Result<Retained, DiskError> {
        if retention_ms == 0 {
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
        if now_ms < newest_ms {
            return Ok(Retained::ClockBehind { now_ms, newest_ms });
        }
        let cutoff = now_ms.min(newest_ms).saturating_sub(retention_ms);
        let n = self.conn.execute(
            "DELETE FROM event WHERE wall_ms < ?1 AND seq < ?2",
            params![i64::try_from(cutoff).unwrap_or(i64::MAX), newest_seq],
        )?;
        let first: Option<i64> = self
            .conn
            .query_row("SELECT min(seq) FROM event", [], |r| r.get(0))?;
        self.first = first.and_then(|v| u64::try_from(v).ok());
        Ok(if n == 0 {
            Retained::Kept
        } else {
            Retained::Deleted(n as u64)
        })
    }

    /// Make `dest` a copy of the committed events: append the rows it lacks
    /// and delete the rows this file no longer holds (spec/persistence.md,
    /// "Backups"). Runs through a connection of its own.
    pub fn copy_into(src: &Path, dest: &Path, campaign: u64) -> Result<(), DiskError> {
        let d = EventFile::open(dest, campaign)?;
        let src = src
            .to_str()
            .ok_or_else(|| DiskError::Corrupt(format!("path {} is not UTF-8", src.display())))?;
        d.conn.execute("ATTACH DATABASE ?1 AS src", [src])?;
        let r = (|| {
            let tx = d.conn.unchecked_transaction()?;
            tx.execute(
                "INSERT INTO main.event (seq, wall_ms, source, body) \
                 SELECT seq, wall_ms, source, body FROM src.event \
                 WHERE seq > (SELECT coalesce(max(seq), 0) FROM main.event)",
                [],
            )?;
            tx.execute(
                "DELETE FROM main.event WHERE seq < (SELECT coalesce(min(seq), 0) FROM src.event)",
                [],
            )?;
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

    fn row(seq: u64, wall_ms: u64, source: Option<(u64, u64)>) -> EventRow {
        EventRow {
            seq,
            wall_ms,
            source,
            body: vec![seq as u8],
        }
    }

    #[test]
    fn events_commit_read_back_and_find_their_source() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("events.sqlite");
        let mut f = EventFile::open(&p, 7).expect("open");
        assert_eq!(f.next_seq(), 1);
        f.push(row(1, 10, Some((5, 100))));
        f.push(row(2, 11, None));
        assert!(f.has_source(5, 100).expect("pending"));
        f.commit().expect("commit");
        drop(f);
        let f = EventFile::open(&p, 7).expect("reopen");
        assert_eq!((f.first_seq(), f.next_seq()), (Some(1), 3));
        assert!(f.has_source(5, 100).expect("committed"));
        assert!(!f.has_source(5, 101).expect("absent"));
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
            f.retain(500, 2_500).expect("retain"),
            Retained::ClockBehind {
                now_ms: 2_500,
                newest_ms: 3_000
            }
        );
        // A clock far ahead counts from the newest event, not from now.
        assert_eq!(
            f.retain(1_500, 1_000_000).expect("retain"),
            Retained::Deleted(1)
        );
        assert_eq!(f.first_seq(), Some(2));
        // Everything older goes, the newest stays.
        assert_eq!(
            f.retain(1, 1_000_000).expect("retain"),
            Retained::Deleted(1)
        );
        assert_eq!(f.first_seq(), Some(3));
        assert_eq!(f.next_seq(), 4);
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
        f.retain(1, 3).expect("retain");
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
}
