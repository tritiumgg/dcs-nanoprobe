//! A campaign's event history: an append-only file beside its store
//! (spec/persistence.md, "Event history").

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The bytes before a record's body.
pub const HEADER: usize = 24;

/// One event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The event's sequence number.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub wall_ms: u64,
    /// The event as the Events service sends it.
    pub body: Vec<u8>,
}

/// The history file of one campaign, open for appending.
#[derive(Debug)]
pub struct History {
    path: PathBuf,
    file: File,
    first: Option<u64>,
    next: u64,
    len: u64,
}

/// CRC-32 (IEEE), bitwise.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    for p in parts {
        for &byte in *p {
            c ^= u32::from(byte);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0xEDB8_8320
                } else {
                    c >> 1
                };
            }
        }
    }
    !c
}

fn bad(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The records of `buf`, and the length of the whole records: a last record
/// that is short or fails its check is a write cut by a crash and is not
/// counted. A bad record before the last is an error.
fn scan(buf: &[u8]) -> io::Result<(Vec<Record>, usize)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < buf.len() {
        if buf.len() - at < HEADER {
            break;
        }
        let h = &buf[at..at + HEADER];
        let len = u32::from_le_bytes(h[0..4].try_into().expect("4")) as usize;
        let crc = u32::from_le_bytes(h[4..8].try_into().expect("4"));
        let end = at + HEADER + len;
        if end > buf.len() {
            // A cut write, unless a whole record follows: then this record's
            // length is damaged, and cutting here would drop good records.
            let next = out.last().map_or(0, |r: &Record| r.seq + 2);
            if whole_record_after(buf, at + 1, next) {
                return Err(bad(format!("history: a bad record length at byte {at}")));
            }
            break;
        }
        if crc32(&[&buf[at + 8..end]]) != crc {
            // A cut write, which may leave zeros past it, unless a whole
            // record follows.
            let next = out.last().map_or(0, |r: &Record| r.seq + 2);
            if whole_record_after(buf, at + 1, next) || record_starts_at(buf, end, next) {
                return Err(bad(format!("history: a bad record at byte {at}")));
            }
            break;
        }
        let seq = u64::from_le_bytes(h[8..16].try_into().expect("8"));
        let wall_ms = u64::from_le_bytes(h[16..24].try_into().expect("8"));
        if let Some(last) = out.last().map(|r: &Record| r.seq)
            && seq != last + 1
        {
            return Err(bad(format!(
                "history: record {seq} follows {last} at byte {at}"
            )));
        }
        out.push(Record {
            seq,
            wall_ms,
            body: buf[at + HEADER..end].to_vec(),
        });
        at = end;
    }
    Ok((out, at))
}

/// Whether a whole record that checks, with a sequence number of at least
/// `seq`, starts anywhere in `buf` from `from` on. The caller passes the
/// number after the damaged record's: a later record proves the damage is
/// not a cut write.
fn whole_record_after(buf: &[u8], from: usize, seq: u64) -> bool {
    (from..buf.len().saturating_sub(HEADER - 1)).any(|at| {
        let h = &buf[at..at + HEADER];
        let len = u32::from_le_bytes(h[0..4].try_into().expect("4")) as usize;
        let end = at + HEADER + len;
        let s = u64::from_le_bytes(h[8..16].try_into().expect("8"));
        end <= buf.len()
            && s >= seq
            && crc32(&[&buf[at + 8..end]]) == u32::from_le_bytes(h[4..8].try_into().expect("4"))
    })
}

/// Whether the header at `at` names a record numbered `seq` or later, whole
/// or cut short: a later write after a record whose body is damaged.
fn record_starts_at(buf: &[u8], at: usize, seq: u64) -> bool {
    buf.len() >= at + HEADER
        && u64::from_le_bytes(buf[at + 8..at + 16].try_into().expect("8")) >= seq.max(1)
}

fn encode(r: &Record) -> Vec<u8> {
    let mut v = Vec::with_capacity(HEADER + r.body.len());
    v.extend_from_slice(&(r.body.len() as u32).to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    v.extend_from_slice(&r.seq.to_le_bytes());
    v.extend_from_slice(&r.wall_ms.to_le_bytes());
    v.extend_from_slice(&r.body);
    let crc = crc32(&[&v[8..]]);
    v[4..8].copy_from_slice(&crc.to_le_bytes());
    v
}

impl History {
    /// Open the file at `path`, creating it if it is missing, and drop a
    /// torn last record.
    pub fn open(path: &Path) -> io::Result<History> {
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let (records, whole) = scan(&buf)?;
        if whole < buf.len() {
            file.set_len(whole as u64)?;
            file.sync_data()?;
        }
        let first = records.first().map(|r| r.seq);
        let next = records.last().map_or(1, |r| r.seq + 1);
        Ok(History {
            path: path.to_owned(),
            file,
            first,
            next,
            len: whole as u64,
        })
    }

    /// The first sequence number held, if any.
    pub fn first_seq(&self) -> Option<u64> {
        self.first
    }

    /// The sequence number the next record must carry: one more than the
    /// last, or 1 for an empty history.
    pub fn next_seq(&self) -> u64 {
        self.next
    }

    /// The file's length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the history holds no record.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Append one event. `seq` is [`History::next_seq`], or any number for
    /// the first record of an empty history.
    pub fn append(&mut self, seq: u64, wall_ms: u64, body: &[u8]) -> io::Result<()> {
        if self.first.is_some() && seq != self.next {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("history: event {seq} appended after {}", self.next - 1),
            ));
        }
        let bytes = encode(&Record {
            seq,
            wall_ms,
            body: body.to_vec(),
        });
        if let Err(e) = self.file.write_all(&bytes) {
            self.undo_partial();
            return Err(e);
        }
        self.len += bytes.len() as u64;
        self.first.get_or_insert(seq);
        self.next = seq + 1;
        Ok(())
    }

    /// Cut the file back to its last whole record after a write that failed
    /// part way, so the next append does not land after partial bytes. If
    /// the cut fails too, the history refuses every later append.
    fn undo_partial(&mut self) {
        if self.file.set_len(self.len).is_err() {
            self.next = u64::MAX;
            self.first.get_or_insert(0);
        }
    }

    /// Flush what was appended to the disk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// Every record from `seq` on.
    pub fn read_from(&self, seq: u64) -> io::Result<Vec<Record>> {
        let mut f = File::open(&self.path)?;
        let mut buf = Vec::new();
        f.seek(SeekFrom::Start(0))?;
        f.take(self.len).read_to_end(&mut buf)?;
        let (records, _) = scan(&buf)?;
        Ok(records.into_iter().filter(|r| r.seq >= seq).collect())
    }

    /// Drop the records older than `cutoff_ms` by writing the rest to a new
    /// file and renaming it over this one (`event_retention_days`). The
    /// newest record is always kept: it carries the last sequence number, so
    /// the numbers go on from it after a restart.
    pub fn compact(&mut self, cutoff_ms: u64) -> io::Result<()> {
        let all = self.read_from(0)?;
        let newest = all.len().saturating_sub(1);
        let keep: Vec<Record> = all
            .into_iter()
            .enumerate()
            .skip_while(|(i, r)| r.wall_ms < cutoff_ms && *i < newest)
            .map(|(_, r)| r)
            .collect();
        let tmp = self.path.with_extension("log.new");
        {
            let mut f = File::create(&tmp)?;
            for r in &keep {
                f.write_all(&encode(r))?;
            }
            f.sync_data()?;
        }
        fs::rename(&tmp, &self.path)?;
        *self = History::open(&self.path)?;
        Ok(())
    }

    /// Make `dest` a copy of this history, by appending the bytes it lacks;
    /// when its first record is not this history's, it is copied again in
    /// full (spec/persistence.md, "Backups"). Returns the bytes written.
    pub fn copy_into(&self, dest: &Path) -> io::Result<u64> {
        let mut src = File::open(&self.path)?;
        let mut head = vec![0u8; HEADER];
        let have = match File::open(dest) {
            Ok(mut d) => {
                let dlen = d.metadata()?.len();
                let mut dhead = vec![0u8; HEADER];
                let same_start = if dlen >= HEADER as u64 && self.len >= HEADER as u64 {
                    d.read_exact(&mut dhead)?;
                    src.read_exact(&mut head)?;
                    head == dhead
                } else {
                    dlen == 0
                };
                if same_start && dlen <= self.len {
                    dlen
                } else {
                    0
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        let mut out = if have == 0 {
            File::create(dest)?
        } else {
            OpenOptions::new().append(true).open(dest)?
        };
        src.seek(SeekFrom::Start(have))?;
        let n = io::copy(&mut (&mut src).take(self.len - have), &mut out)?;
        out.sync_data()?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[test]
    fn records_read_back_in_order_and_from_a_sequence_number() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        for i in 1..=5u64 {
            h.append(i, 1000 + i, format!("event {i}").as_bytes())
                .expect("append");
        }
        h.flush().expect("flush");
        let h = History::open(&p).expect("reopen");
        assert_eq!(h.next_seq(), 6);
        let r = h.read_from(3).expect("read");
        assert_eq!(r.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
        assert_eq!(r[0].body, b"event 3");
    }

    #[test]
    fn a_torn_last_record_is_dropped_and_a_bad_middle_record_is_an_error() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        h.append(1, 1, b"one").expect("append");
        h.append(2, 2, b"two").expect("append");
        drop(h);
        let full = fs::read(&p).expect("read");
        // Cut the last record short.
        fs::write(&p, &full[..full.len() - 2]).expect("write");
        let mut h = History::open(&p).expect("torn tail");
        assert_eq!(h.next_seq(), 2);
        h.append(2, 2, b"two again").expect("append");
        assert_eq!(h.read_from(0).expect("read").len(), 2);
        // Flip a byte in the first record's body.
        let mut bytes = fs::read(&p).expect("read");
        bytes[HEADER] ^= 0xff;
        fs::write(&p, &bytes).expect("write");
        assert!(History::open(&p).is_err());
    }

    #[test]
    fn a_damaged_middle_record_is_an_error_and_nothing_is_cut() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        for i in 1..=4u64 {
            h.append(i, i, b"event").expect("append");
        }
        drop(h);
        let mut bytes = fs::read(&p).expect("read");
        // A body byte of record 2.
        bytes[2 * HEADER + 5] ^= 0xff;
        fs::write(&p, &bytes).expect("write");
        assert!(History::open(&p).is_err());
        assert_eq!(
            fs::read(&p).expect("read").len(),
            bytes.len(),
            "nothing cut"
        );
    }

    #[test]
    fn a_damaged_record_before_a_cut_one_is_an_error() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        for i in 1..=4u64 {
            h.append(i, i, b"event").expect("append");
        }
        drop(h);
        let mut bytes = fs::read(&p).expect("read");
        bytes[3 * HEADER + 2 * 5 + 1] ^= 0xff; // record 3's body
        bytes.truncate(bytes.len() - 2); // record 4 cut short
        fs::write(&p, &bytes).expect("write");
        assert!(History::open(&p).is_err());
        assert_eq!(
            fs::read(&p).expect("read").len(),
            bytes.len(),
            "nothing cut"
        );
    }

    #[test]
    fn partial_bytes_of_a_failed_append_are_cut_back() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        h.append(1, 1, b"one").expect("append");
        // What a write that failed part way leaves.
        OpenOptions::new()
            .append(true)
            .open(&p)
            .expect("file")
            .write_all(&[7; 10])
            .expect("partial");
        h.undo_partial();
        h.append(2, 2, b"two").expect("append");
        drop(h);
        let h = History::open(&p).expect("reopen");
        assert_eq!(h.read_from(0).expect("read").len(), 2);
    }

    #[test]
    fn compaction_keeps_the_newest_record_so_numbers_go_on() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        for i in 1..=3u64 {
            h.append(i, i, b"old").expect("append");
        }
        h.compact(1_000).expect("compact");
        assert_eq!(h.first_seq(), Some(3));
        drop(h);
        let mut h = History::open(&p).expect("reopen");
        assert_eq!(h.next_seq(), 4);
        h.append(4, 2_000, b"new").expect("append");
    }

    #[test]
    fn zeros_after_a_cut_write_are_cut_too() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        h.append(1, 1, b"one").expect("append");
        h.append(2, 2, b"two").expect("append");
        let good = h.len();
        drop(h);
        let mut bytes = fs::read(&p).expect("read");
        bytes.extend_from_slice(&[0; 100]);
        fs::write(&p, &bytes).expect("write");
        let h = History::open(&p).expect("a zero tail is a cut write");
        assert_eq!(h.len(), good);
        assert_eq!(h.next_seq(), 3);
    }

    #[test]
    fn a_damaged_length_before_good_records_is_an_error_not_a_cut() {
        let d = tmp();
        let p = d.path().join("history.log");
        let mut h = History::open(&p).expect("open");
        for i in 1..=3u64 {
            h.append(i, i, b"event").expect("append");
        }
        drop(h);
        let mut bytes = fs::read(&p).expect("read");
        // Record 2's length now runs past the end of the file.
        let second = HEADER + 5;
        bytes[second..second + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&p, &bytes).expect("write");
        assert!(History::open(&p).is_err());
        assert_eq!(
            fs::read(&p).expect("read").len(),
            bytes.len(),
            "nothing cut"
        );
    }

    #[test]
    fn appending_out_of_sequence_is_refused() {
        let d = tmp();
        let mut h = History::open(&d.path().join("h.log")).expect("open");
        h.append(7, 0, b"x").expect("first");
        assert!(h.append(9, 0, b"y").is_err());
        h.append(8, 0, b"y").expect("next");
    }

    #[test]
    fn a_copy_grows_by_what_the_history_gained_and_restarts_after_compaction() {
        let d = tmp();
        let p = d.path().join("history.log");
        let c = d.path().join("copy.log");
        let mut h = History::open(&p).expect("open");
        for i in 1..=10u64 {
            h.append(i, i * 1000, &[i as u8; 100]).expect("append");
        }
        let first = h.copy_into(&c).expect("copy");
        assert_eq!(first, h.len());
        h.append(11, 11_000, &[11; 100]).expect("append");
        let second = h.copy_into(&c).expect("copy");
        assert_eq!(second, (HEADER + 100) as u64);
        assert_eq!(fs::read(&c).expect("c"), fs::read(&p).expect("p"));
        h.compact(5_000).expect("compact");
        assert_eq!(h.first_seq(), Some(5));
        let third = h.copy_into(&c).expect("copy");
        assert_eq!(third, h.len());
        assert_eq!(fs::read(&c).expect("c"), fs::read(&p).expect("p"));
        h.append(12, 12_000, b"z").expect("append after compaction");
    }
}
