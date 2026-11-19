//! The store on disk (spec/persistence.md): one SQLite file per campaign, in
//! WAL mode, written in one transaction at a time.
//!
//! A transaction writes every entity row that differs from the file, the
//! extensions that changed, the journal entries made since the last one, and
//! `meta`. The rows are compared with a shadow copy of what the file holds,
//! so no mutator of the store has to mark what it changed.
//!
//! The codec destructures every struct it writes without `..`: a field added
//! to the store does not compile until it is persisted here too.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use nanoprobe_transport::codec::{Lv, Reader, Writer};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use nanoprobe_transport::records::Rates;

use super::{
    Authority, CapturedMore, DesiredOwner, DesiredStock, Entry, Extension, Group, IdemRow,
    Journalled, Kind, LeftAs, Loaded, LostPolicy, Outcome, OwedLoad, Point, RemoveCause,
    RouteEvent, Store, Unit, UnitState, check_lv,
};
use crate::ids::{CampaignId, EntityId};
use crate::intents::{Intent, Waypoint};
use crate::lifecycle::State;
use crate::stock::Stock;

/// The schema version this sidecar writes (spec/persistence.md).
pub const SCHEMA: u32 = 1;

/// What [`Store::prune_journal`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalPrune {
    /// Retention is off, the store has no file, or the oldest entry is
    /// inside the retention: nothing was deleted and nothing is left to.
    Kept,
    /// Entries were deleted.
    Deleted {
        /// How many.
        rows: u64,
        /// The batch was full and every entry in it was old: more old
        /// entries may remain.
        more: bool,
    },
    /// The clock is behind the newest entry: nothing was deleted.
    ClockBehind {
        /// The clock, milliseconds since the Unix epoch.
        now_ms: u64,
        /// The newest entry's time.
        newest_ms: u64,
    },
}

/// A store file that could not be read or written.
#[derive(Debug)]
pub enum DiskError {
    /// SQLite failed.
    Sql(rusqlite::Error),
    /// A row does not decode.
    Corrupt(String),
    /// The file's schema is newer than this sidecar's.
    Newer {
        /// The version in the file.
        found: u32,
        /// The newest this sidecar reads.
        known: u32,
    },
    /// A file is already there.
    Exists(PathBuf),
    /// A file operation failed.
    Io(std::io::Error),
    /// The campaign is finished: its store is read-only.
    Finished(CampaignId),
    /// A value does not encode whole (spec/persistence.md, "Encodings").
    TooLarge(String),
}

/// The name of SQLite's result code `code`, primary or extended, for a log
/// line that names its cause (`SQLITE_IOERR_GETTEMPPATH` where the message
/// says only "disk I/O error").
pub fn sqlite_code_name(code: i32) -> Option<&'static str> {
    use rusqlite::ffi;
    macro_rules! names {
        ($($n:ident),* $(,)?) => {
            $(if code == ffi::$n { return Some(stringify!($n)); })*
        };
    }
    names!(
        SQLITE_ERROR,
        SQLITE_INTERNAL,
        SQLITE_PERM,
        SQLITE_ABORT,
        SQLITE_BUSY,
        SQLITE_LOCKED,
        SQLITE_NOMEM,
        SQLITE_READONLY,
        SQLITE_INTERRUPT,
        SQLITE_IOERR,
        SQLITE_CORRUPT,
        SQLITE_NOTFOUND,
        SQLITE_FULL,
        SQLITE_CANTOPEN,
        SQLITE_PROTOCOL,
        SQLITE_EMPTY,
        SQLITE_SCHEMA,
        SQLITE_TOOBIG,
        SQLITE_CONSTRAINT,
        SQLITE_MISMATCH,
        SQLITE_MISUSE,
        SQLITE_NOLFS,
        SQLITE_AUTH,
        SQLITE_FORMAT,
        SQLITE_RANGE,
        SQLITE_NOTADB,
        SQLITE_ERROR_MISSING_COLLSEQ,
        SQLITE_ERROR_RETRY,
        SQLITE_ERROR_SNAPSHOT,
        SQLITE_IOERR_READ,
        SQLITE_IOERR_SHORT_READ,
        SQLITE_IOERR_WRITE,
        SQLITE_IOERR_FSYNC,
        SQLITE_IOERR_DIR_FSYNC,
        SQLITE_IOERR_TRUNCATE,
        SQLITE_IOERR_FSTAT,
        SQLITE_IOERR_UNLOCK,
        SQLITE_IOERR_RDLOCK,
        SQLITE_IOERR_DELETE,
        SQLITE_IOERR_BLOCKED,
        SQLITE_IOERR_NOMEM,
        SQLITE_IOERR_ACCESS,
        SQLITE_IOERR_CHECKRESERVEDLOCK,
        SQLITE_IOERR_LOCK,
        SQLITE_IOERR_CLOSE,
        SQLITE_IOERR_DIR_CLOSE,
        SQLITE_IOERR_SHMOPEN,
        SQLITE_IOERR_SHMSIZE,
        SQLITE_IOERR_SHMLOCK,
        SQLITE_IOERR_SHMMAP,
        SQLITE_IOERR_SEEK,
        SQLITE_IOERR_DELETE_NOENT,
        SQLITE_IOERR_MMAP,
        SQLITE_IOERR_GETTEMPPATH,
        SQLITE_IOERR_CONVPATH,
        SQLITE_IOERR_VNODE,
        SQLITE_IOERR_AUTH,
        SQLITE_IOERR_BEGIN_ATOMIC,
        SQLITE_IOERR_COMMIT_ATOMIC,
        SQLITE_IOERR_ROLLBACK_ATOMIC,
        SQLITE_IOERR_DATA,
        SQLITE_IOERR_CORRUPTFS,
        SQLITE_IOERR_IN_PAGE,
        SQLITE_LOCKED_SHAREDCACHE,
        SQLITE_LOCKED_VTAB,
        SQLITE_BUSY_RECOVERY,
        SQLITE_BUSY_SNAPSHOT,
        SQLITE_BUSY_TIMEOUT,
        SQLITE_CANTOPEN_NOTEMPDIR,
        SQLITE_CANTOPEN_ISDIR,
        SQLITE_CANTOPEN_FULLPATH,
        SQLITE_CANTOPEN_CONVPATH,
        SQLITE_CANTOPEN_DIRTYWAL,
        SQLITE_CANTOPEN_SYMLINK,
        SQLITE_CORRUPT_VTAB,
        SQLITE_CORRUPT_SEQUENCE,
        SQLITE_CORRUPT_INDEX,
        SQLITE_READONLY_RECOVERY,
        SQLITE_READONLY_CANTLOCK,
        SQLITE_READONLY_ROLLBACK,
        SQLITE_READONLY_DBMOVED,
        SQLITE_READONLY_CANTINIT,
        SQLITE_READONLY_DIRECTORY,
        SQLITE_ABORT_ROLLBACK,
        SQLITE_CONSTRAINT_CHECK,
        SQLITE_CONSTRAINT_COMMITHOOK,
        SQLITE_CONSTRAINT_FOREIGNKEY,
        SQLITE_CONSTRAINT_FUNCTION,
        SQLITE_CONSTRAINT_NOTNULL,
        SQLITE_CONSTRAINT_PRIMARYKEY,
        SQLITE_CONSTRAINT_TRIGGER,
        SQLITE_CONSTRAINT_UNIQUE,
        SQLITE_CONSTRAINT_VTAB,
        SQLITE_CONSTRAINT_ROWID,
        SQLITE_CONSTRAINT_PINNED,
        SQLITE_CONSTRAINT_DATATYPE,
    );
    None
}

impl fmt::Display for DiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiskError::Sql(e @ rusqlite::Error::SqliteFailure(c, _)) => {
                let name = sqlite_code_name(c.extended_code).unwrap_or("unnamed");
                write!(f, "store: {e} ({name}, code {})", c.extended_code)
            }
            DiskError::Sql(e) => write!(f, "store: {e}"),
            DiskError::Corrupt(m) => write!(f, "store: a row does not decode: {m}"),
            DiskError::Newer { found, known } => write!(
                f,
                "store: schema version {found} is newer than this sidecar's {known}"
            ),
            DiskError::Exists(p) => write!(f, "store: {} exists", p.display()),
            DiskError::Io(e) => write!(f, "store: {e}"),
            DiskError::Finished(id) => write!(f, "campaign {} is finished", id.0),
            DiskError::TooLarge(m) => write!(f, "store: a value does not encode: {m}"),
        }
    }
}

impl std::error::Error for DiskError {}

impl From<rusqlite::Error> for DiskError {
    fn from(e: rusqlite::Error) -> DiskError {
        DiskError::Sql(e)
    }
}

impl From<std::io::Error> for DiskError {
    fn from(e: std::io::Error) -> DiskError {
        DiskError::Io(e)
    }
}

/// One migration step: it takes a store at version `to - 1` to `to`, inside
/// the transaction that also sets the version.
#[derive(Clone, Copy)]
pub struct Migration {
    /// The version after the step.
    pub to: u32,
    /// The step.
    pub run: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

/// The steps of this sidecar, oldest first. Empty while [`SCHEMA`] is 1.
pub const MIGRATIONS: &[Migration] = &[];

/// A campaign's state (spec/persistence.md, "Campaign operations").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignState {
    /// The one DCS runs.
    Active,
    /// Closed; may be activated again.
    Suspended,
    /// Closed for good, read-only.
    Finished,
}

/// The campaign record kept in the store's `meta`.
#[derive(Clone, Debug, PartialEq)]
pub struct CampaignRecord {
    /// The name.
    pub name: String,
    /// Its state.
    pub state: CampaignState,
    /// The maps it uses.
    pub theaters: Vec<String>,
    /// The theater to load when it is activated.
    pub theater: String,
    /// The campaign clock, milliseconds since the Unix epoch, or `None`.
    pub clock_ms: Option<i64>,
    /// When it was created, milliseconds since the Unix epoch.
    pub created_ms: u64,
    /// The author's component schema versions by name.
    pub component_schemas: BTreeMap<String, u32>,
    /// The outcome of a finished campaign, as the API's encoding of it.
    pub outcome: Option<Vec<u8>>,
}

/// The period one sampling tier achieved, with its target, in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct TierPeriod {
    /// The tier (spec/sampling.md, "Tiers").
    pub tier: String,
    /// The target period.
    pub target_s: f64,
    /// The period achieved.
    pub achieved_s: f64,
}

/// A checkpoint row.
#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    /// Its number, from 1.
    pub seq: u64,
    /// When it was taken, milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// The store revision it holds.
    pub revision: u64,
    /// The model time of the samples it holds.
    pub model_time: f64,
    /// The mission generation it was taken in, 0 before the first.
    pub mission_gen: u32,
    /// The periods the tiers achieved when it was taken.
    pub achieved: Vec<TierPeriod>,
}

/// The file behind a store, and what it holds.
pub struct Disk {
    path: PathBuf,
    conn: Connection,
    groups: BTreeMap<EntityId, Group>,
    units: BTreeMap<EntityId, Unit>,
    extensions: BTreeMap<String, Extension>,
    /// The revision floors of removed extensions, as the file holds them.
    floors: BTreeMap<String, u64>,
    /// Entries of `Store::journal` already written.
    journal_written: usize,
    /// The last journal row's `seq`.
    journal_seq: u64,
    /// The bytes of the bodies of the journal rows in the file, as
    /// `meta.journal_bytes` holds them.
    journal_bytes: u64,
    /// `meta.journal_bytes` is missing from the file: the next write adds it.
    journal_bytes_unwritten: bool,
    world: Vec<u8>,
    /// The model time of the samples last written, by a checkpoint.
    sample_time: f64,
    record: CampaignRecord,
    record_written: bool,
    last_checkpoint: Option<Checkpoint>,
    /// Whether the store is open: written true at open and false at a clean
    /// close, so an open finds an unclean stop.
    open: bool,
}

impl fmt::Debug for Disk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Disk").field("path", &self.path).finish()
    }
}

/// The store's link to its file. A clone of a store is in memory only: two
/// stores never write one file.
#[derive(Default)]
pub struct Slot(pub(super) Option<Box<Disk>>);

impl Clone for Slot {
    fn clone(&self) -> Slot {
        Slot(None)
    }
}

impl fmt::Debug for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(d) => d.fmt(f),
            None => f.write_str("in memory"),
        }
    }
}

/// `path` with `suffix` after its whole name: SQLite's `-wal` and `-shm`.
fn wal(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Flush a folder's entries, so a rename in it survives a power loss. Not
/// possible on Windows, where the rename is flushed with the file system.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn connect(path: &Path) -> Result<Connection, DiskError> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

const TABLES: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);
CREATE TABLE entity (id INTEGER NOT NULL, kind TEXT NOT NULL, body BLOB NOT NULL, PRIMARY KEY (id, kind));
CREATE TABLE extension (name TEXT PRIMARY KEY, body BLOB NOT NULL);
CREATE TABLE journal (seq INTEGER PRIMARY KEY, at_ms INTEGER NOT NULL, kind TEXT NOT NULL, entity INTEGER NOT NULL, body BLOB NOT NULL);
CREATE TABLE inflight (epoch INTEGER NOT NULL, seq INTEGER NOT NULL, body BLOB NOT NULL, PRIMARY KEY (epoch, seq));
CREATE TABLE idempotency (key TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, request_hash BLOB NOT NULL, response BLOB NOT NULL);
CREATE TABLE checkpoint (seq INTEGER PRIMARY KEY, body BLOB NOT NULL);
CREATE TABLE template (theater TEXT PRIMARY KEY, body BLOB NOT NULL);
";

/// The template table, made at open in a schema 1 file written before it
/// (schema 1 has not shipped; spec/persistence.md).
const TEMPLATE_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS template (theater TEXT PRIMARY KEY, body BLOB NOT NULL);";

/// The schema version of the store file at `path`.
pub fn schema_of(path: &Path) -> Result<u32, DiskError> {
    let conn = Connection::open(path)?;
    read_schema(&conn)
}

/// The campaign ID and schema version of the store file at `path`, read
/// without opening the store. Both have the same layout in every version.
pub fn ident_of(path: &Path) -> Result<(CampaignId, u32), DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let schema = read_schema(&conn)?;
    let campaign = CampaignId(dec_u(&get_meta(&conn, "campaign_id")?, "campaign_id")?);
    Ok((campaign, schema))
}

/// The campaign ID, schema version and campaign record of the store file at
/// `path`, read without opening the store: no migration runs. A newer
/// schema is refused before the record is read.
pub fn record_of(path: &Path) -> Result<(CampaignId, u32, CampaignRecord), DiskError> {
    let (campaign, schema) = ident_of(path)?;
    if schema > SCHEMA {
        return Err(DiskError::Newer {
            found: schema,
            known: SCHEMA,
        });
    }
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let record = dec_record(&get_meta(&conn, "campaign")?)?;
    Ok((campaign, schema, record))
}

/// The last checkpoint in the store file at `path`, read without opening
/// the store. The caller has checked the schema is not newer than this
/// sidecar's.
pub fn checkpoint_of(path: &Path) -> Result<Option<Checkpoint>, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let body: Option<Vec<u8>> = conn
        .query_row(
            "SELECT body FROM checkpoint ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    match body {
        Some(b) => Ok(Some(dec_checkpoint(&decode(&b)?)?)),
        None => Ok(None),
    }
}

/// The store revision in the store file at `path`, read without opening the
/// store.
pub fn revision_of(path: &Path) -> Result<u64, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Rd(&get_meta(&conn, "world")?).u("revision")
}

/// The campaign's own template for `theater` in the store file at `path`,
/// read without opening the store; `None` when it has none.
pub fn template_of(path: &Path, theater: &str) -> Result<Option<Vec<u8>>, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_table: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'template'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(None);
    }
    Ok(conn
        .query_row(
            "SELECT body FROM template WHERE theater = ?1",
            params![theater],
            |r| r.get(0),
        )
        .optional()?)
}

/// The theaters the store file at `path` holds a campaign template for,
/// read without opening the store.
pub fn template_theaters_of(path: &Path) -> Result<Vec<String>, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let has_table = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'template'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(Vec::new());
    }
    let mut q = conn.prepare("SELECT theater FROM template ORDER BY theater")?;
    let rows = q.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The next mission's settings in the store file at `path`, read without
/// opening the store.
pub fn next_mission_of(path: &Path) -> Result<Option<Lv>, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(match Rd(&get_meta(&conn, "world")?).get("next_mission") {
        Lv::Nil => None,
        v => Some(v.clone()),
    })
}

/// The shell slot DCS may hold, in the store file at `path`, read without
/// opening the store: the slot of an owed load already sent, else that of
/// the last load applied.
pub fn shell_slot_of(path: &Path) -> Result<Option<u8>, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let world = get_meta(&conn, "world")?;
    let w = Rd(&world);
    if let v @ Lv::Table(_) = w.get("owed_load") {
        let owed = dec_owed_load(v)?;
        let named = owed
            .path
            .strip_suffix(".miz")
            .and_then(|p| p.rsplit_once('.'))
            .map(|(_, s)| s.as_bytes())
            .filter(|s| *s == b"a" || *s == b"b");
        if let (Some(_), Some(s)) = (owed.cmd, named) {
            return Ok(Some(s[0]));
        }
    }
    Ok(match w.get("shell_slot") {
        Lv::String(b) if b.len() == 1 => Some(b[0]),
        _ => None,
    })
}

/// The next `EntityId` in the store file at `path`, read without opening
/// the store.
pub fn next_entity_of(path: &Path) -> Result<u64, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Rd(&get_meta(&conn, "world")?).u("next_entity")
}

/// Raise the store file at `path` above what another copy of the campaign
/// handed out, for an offline restore (spec/persistence.md, "Restore a
/// backup"): its next `EntityId` becomes at least `next_entity`, its
/// revision the larger of its own and `revision` plus
/// [`REVISION_RESERVE`], and it is marked closed; its record is
/// `suspended` unless it is `finished`; its commands in flight, its owed
/// mission load and a start set for the next shell are cleared; its shell
/// slot is `shell_slot`, the replaced store's. Returns
/// the new next `EntityId` and revision.
pub fn rebase_world(
    path: &Path,
    next_entity: u64,
    revision: u64,
    shell_slot: Option<u8>,
) -> Result<(u64, u64), DiskError> {
    let mut conn = connect(path)?;
    let world = get_meta(&conn, "world")?;
    let (own_next, own_rev) = {
        let r = Rd(&world);
        (r.u("next_entity")?, r.u("revision")?)
    };
    let next = own_next.max(next_entity);
    let rev = own_rev
        .max(revision)
        .checked_add(REVISION_RESERVE)
        .ok_or_else(|| DiskError::Corrupt("revision overflow".into()))?;
    let Lv::Table(pairs) = world else {
        return Err(DiskError::Corrupt("meta world is not a table".into()));
    };
    // A zero or false value has no key (the encoders drop nil), so the
    // three keys are replaced, not edited.
    let ours = |k: &Lv| matches!(k, Lv::String(n) if [&b"next_entity"[..], b"revision", b"open"].contains(&n.as_slice()));
    let mut pairs: Vec<(Lv, Lv)> = pairs.into_iter().filter(|(k, _)| !ours(k)).collect();
    for (k, v) in [("next_entity", u(next)), ("revision", u(rev))] {
        if !matches!(v, Lv::Nil) {
            pairs.push((s(k), v));
        }
    }
    // A restored store owes no mission load and waits for no command; a
    // start set for the next shell is not carried over.
    let mut pairs: Vec<(Lv, Lv)> = pairs
        .into_iter()
        .filter(|(k, _)| *k != s("owed_load"))
        .map(|(k, v)| {
            if k == s("next_mission")
                && let Lv::Table(n) = v
            {
                let n = n.into_iter().filter(|(k, _)| *k != s("start_ms")).collect();
                return (k, Lv::Table(n));
            }
            (k, v)
        })
        .collect();
    pairs.retain(|(k, _)| *k != s("mission_start_ms") && *k != s("shell_slot"));
    // The slot DCS may hold is the replaced store's, not the backup's.
    if let Some(c) = shell_slot {
        pairs.push((s("shell_slot"), s(&char::from(c).to_string())));
    }
    let mut record = dec_record(&get_meta(&conn, "campaign")?)?;
    if record.state != CampaignState::Finished {
        record.state = CampaignState::Suspended;
    }
    let tx = conn.transaction()?;
    put_meta(&tx, "world", &Lv::Table(pairs))?;
    put_meta(&tx, "campaign", &enc_record(&record))?;
    tx.execute("DELETE FROM inflight", [])?;
    tx.commit()?;
    Ok((next, rev))
}

/// A key kept in the store file at `path`, read without opening the store:
/// for a finished campaign, whose store is never opened again.
pub fn idempotency_of(path: &Path, key: &str) -> Result<Option<IdemRow>, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(conn
        .query_row(
            "SELECT key, at_ms, request_hash, response FROM idempotency WHERE key = ?1",
            [key],
            |r| {
                Ok(IdemRow {
                    key: r.get(0)?,
                    at_ms: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                    request_hash: r.get(2)?,
                    response: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Every journal entry in the store file at `path`, oldest first, read
/// without opening the store. The sidecar never reads its journal back; this
/// is for audits and tests.
pub fn journal_of(path: &Path) -> Result<Vec<Entry>, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut st = conn.prepare("SELECT body FROM journal ORDER BY seq")?;
    let mut rows = st.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let body: Vec<u8> = row.get(0)?;
        out.push(dec_entry(&decode(&body)?)?);
    }
    Ok(out)
}

/// The state in the campaign record of the store file at `path`, read
/// without opening the store. Only the record's `state` key is read, which
/// keeps its layout in every schema version.
pub fn state_of(path: &Path) -> Result<CampaignState, DiskError> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    dec_campaign_state(&Rd(&get_meta(&conn, "campaign")?).s("state")?)
}

/// How far an open moves the store revision past the last one written when
/// the store was not closed cleanly: revisions handed out after that write
/// are never handed out again (spec/persistence.md, "Opening a store").
pub const REVISION_RESERVE: u64 = 1 << 32;

fn read_schema(conn: &Connection) -> Result<u32, DiskError> {
    let v: Option<Vec<u8>> = conn
        .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| {
            r.get(0)
        })
        .optional()?;
    let v = v.ok_or_else(|| DiskError::Corrupt("no schema version".into()))?;
    let lv = decode(&v)?;
    match lv {
        Lv::Number(n) if n >= 1.0 && n.fract() == 0.0 && n <= f64::from(u32::MAX) => Ok(n as u32),
        _ => Err(DiskError::Corrupt("schema version".into())),
    }
}

/// Copy the store file at `path` to `dest` consistently, with SQLite's
/// `VACUUM INTO`, whether or not a store has it open.
pub fn backup_file(path: &Path, dest: &Path) -> Result<(), DiskError> {
    // Never create: a missing store is an error, not an empty backup.
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    vacuum_into(&conn, dest)
}

fn vacuum_into(conn: &Connection, dest: &Path) -> Result<(), DiskError> {
    if dest.exists() {
        return Err(DiskError::Exists(dest.to_owned()));
    }
    let dest = dest
        .to_str()
        .ok_or_else(|| DiskError::Corrupt(format!("path {} is not UTF-8", dest.display())))?;
    conn.execute("VACUUM INTO ?1", params![dest])?;
    Ok(())
}

impl Store {
    /// Create the store file at `path` for a new campaign, with an empty
    /// world, and keep it linked: every later change is written to it
    /// (spec/persistence.md, "Writes").
    pub fn create_on_disk(
        path: &Path,
        campaign: CampaignId,
        record: CampaignRecord,
        changes_keep: usize,
    ) -> Result<Store, DiskError> {
        let mut store = Store::new(campaign, changes_keep);
        // A store linked to a file holds only the entries it has not
        // written. `Store::new` keeps them all, for a store in memory.
        store.keep_journal = false;
        store.save_as(path, record)?;
        Ok(store)
    }

    /// Write this store, as it is, to a new store file at `path` and keep it
    /// linked. A store already on disk is unlinked from its old file. The
    /// file is made under a temporary name in one transaction, flushed and
    /// renamed into place, so a crash leaves no file or a whole one. It keeps
    /// the store's `keep_journal` setting: a store from [`Store::new`] still
    /// holds every entry, for the tests that read them; a campaign's store is
    /// made with [`Store::create_on_disk`], which holds only what it has not
    /// written.
    pub fn save_as(&mut self, path: &Path, record: CampaignRecord) -> Result<(), DiskError> {
        if path.exists() {
            return Err(DiskError::Exists(path.to_owned()));
        }
        let tmp = path.with_extension("sqlite.new");
        for p in [tmp.clone(), wal(&tmp, "-wal"), wal(&tmp, "-shm")] {
            match std::fs::remove_file(&p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        let mut d = Disk {
            path: path.to_owned(),
            conn: connect(&tmp)?,
            groups: BTreeMap::new(),
            units: BTreeMap::new(),
            extensions: BTreeMap::new(),
            floors: BTreeMap::new(),
            journal_written: 0,
            journal_seq: 0,
            journal_bytes: 0,
            journal_bytes_unwritten: true,
            world: Vec::new(),
            sample_time: 0.0,
            record,
            record_written: false,
            last_checkpoint: None,
            open: true,
        };
        d.write_with(self, None, true)?;
        // The last connection's close folds the WAL into the file.
        let conn = std::mem::replace(&mut d.conn, Connection::open_in_memory()?);
        conn.close().map_err(|(_, e)| DiskError::Sql(e))?;
        std::fs::File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent() {
            sync_dir(dir);
        }
        d.conn = connect(path)?;
        self.disk = Slot(Some(Box::new(d)));
        self.drain_journal();
        Ok(())
    }

    /// Open the store file at `path` at this sidecar's schema version. An
    /// older file is migrated by [`MIGRATIONS`]; the caller backs it up
    /// first (spec/persistence.md, "Schema version and migration").
    pub fn open(path: &Path, changes_keep: usize) -> Result<Store, DiskError> {
        Store::open_with(path, changes_keep, MIGRATIONS)
    }

    /// [`Store::open`] with these migration steps: the newest version is the
    /// last step's, or [`SCHEMA`] with none.
    pub fn open_with(
        path: &Path,
        changes_keep: usize,
        steps: &[Migration],
    ) -> Result<Store, DiskError> {
        if !path.exists() {
            return Err(DiskError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{} does not exist", path.display()),
            )));
        }
        let mut conn = connect(path)?;
        let known = steps.last().map_or(SCHEMA, |s| s.to);
        let found = read_schema(&conn)?;
        if found > known {
            return Err(DiskError::Newer { found, known });
        }
        if found < known {
            let tx = conn.transaction()?;
            for step in steps.iter().filter(|s| s.to > found) {
                (step.run)(&tx)?;
            }
            put_meta(&tx, "schema", &Lv::Number(f64::from(known)))?;
            tx.commit()?;
        }
        load(path, conn, changes_keep)
    }

    /// Whether the store is linked to a file.
    pub fn on_disk(&self) -> bool {
        self.disk.0.is_some()
    }

    /// The file the store is linked to.
    pub fn path(&self) -> Option<&Path> {
        self.disk.0.as_ref().map(|d| d.path.as_path())
    }

    /// Write every change since the last write to the file, in one
    /// transaction. A store in memory writes nothing and forgets its marks.
    pub fn commit(&mut self) -> Result<(), DiskError> {
        let Some(mut d) = self.disk.0.take() else {
            // Nothing compares the marks of a store with no file.
            self.since_checkpoint.clear();
            self.checkpoint_all = false;
            self.drain_journal();
            return Ok(());
        };
        let r = d.write(self);
        self.disk.0 = Some(d);
        if r.is_ok() {
            self.drain_journal();
        }
        r
    }

    /// Drop the journal entries and idempotency rows written, unless the
    /// store keeps its journal ([`Store::keep_journal`]).
    fn drain_journal(&mut self) {
        self.dirty.clear();
        self.dirty_all = false;
        self.idempotency.clear();
        self.templates_dirty.clear();
        if self.keep_journal {
            return;
        }
        if let Some(d) = self.disk.0.as_mut() {
            self.journal.drain(..d.journal_written);
            let gone: u64 = self
                .journal_at
                .drain(..d.journal_written)
                .map(|a| a.1)
                .sum();
            self.journal_mem_bytes -= gone;
            d.journal_written = 0;
        }
    }

    /// Write the store and a checkpoint row with the periods the sampling
    /// tiers achieved. Returns the checkpoint.
    pub fn checkpoint(
        &mut self,
        at_ms: u64,
        achieved: Vec<TierPeriod>,
    ) -> Result<Checkpoint, DiskError> {
        let revision = self.rev;
        let model_time = self.sample_time;
        let mission_gen = self.loaded.as_ref().map_or(0, |l| l.mission_gen);
        let Some(mut d) = self.disk.0.take() else {
            self.since_checkpoint.clear();
            self.checkpoint_all = false;
            self.drain_journal();
            return Ok(Checkpoint {
                seq: 0,
                at_ms,
                revision,
                model_time,
                mission_gen,
                achieved,
            });
        };
        if let Some(last) = d
            .last_checkpoint
            .as_ref()
            .filter(|c| c.revision == revision)
        {
            // Nothing moved: no new checkpoint.
            let last = last.clone();
            self.disk.0 = Some(d);
            return Ok(last);
        }
        let r = (|| {
            let seq = d.last_checkpoint.as_ref().map_or(0, |c| c.seq) + 1;
            let c = Checkpoint {
                seq,
                at_ms,
                revision,
                model_time,
                mission_gen,
                achieved,
            };
            d.write_with(self, Some(&c), false)?;
            d.last_checkpoint = Some(c.clone());
            Ok(c)
        })();
        self.disk.0 = Some(d);
        if r.is_ok() {
            self.since_checkpoint.clear();
            self.checkpoint_all = false;
            self.drain_journal();
        }
        r
    }

    /// Close the store in one transaction (spec/persistence.md, "Campaign
    /// operations"): the final checkpoint, `record`, the keys staged with
    /// [`Store::put_idempotency`], and the closed mark. On a failure the
    /// store stays open. The caller then drops it.
    pub fn close_with(
        &mut self,
        at_ms: u64,
        achieved: Vec<TierPeriod>,
        record: CampaignRecord,
    ) -> Result<Checkpoint, DiskError> {
        let revision = self.rev;
        let model_time = self.sample_time;
        let mission_gen = self.loaded.as_ref().map_or(0, |l| l.mission_gen);
        let Some(mut d) = self.disk.0.take() else {
            return Ok(Checkpoint {
                seq: 0,
                at_ms,
                revision,
                model_time,
                mission_gen,
                achieved,
            });
        };
        let (old_record, old_open) = (d.record.clone(), d.open);
        d.record = record;
        d.record_written = false;
        d.open = false;
        let c = Checkpoint {
            seq: d.last_checkpoint.as_ref().map_or(0, |c| c.seq) + 1,
            at_ms,
            revision,
            model_time,
            mission_gen,
            achieved,
        };
        let r = d.write_with(self, Some(&c), false);
        match r {
            Ok(()) => d.last_checkpoint = Some(c.clone()),
            Err(_) => {
                d.record = old_record;
                d.open = old_open;
            }
        }
        self.disk.0 = Some(d);
        r.map(|()| {
            self.since_checkpoint.clear();
            self.checkpoint_all = false;
            self.drain_journal();
            c
        })
    }

    /// Close the store cleanly: write everything and mark it closed, so the
    /// next open keeps the revision. The caller then drops it.
    pub fn close(&mut self) -> Result<(), DiskError> {
        if let Some(d) = self.disk.0.as_mut() {
            d.open = false;
        }
        let r = self.commit();
        if r.is_err()
            && let Some(d) = self.disk.0.as_mut()
        {
            // Not closed: a later write must not mark it so.
            d.open = true;
        }
        r
    }

    /// A changing call's answer kept in this store under `key`, written or
    /// waiting for the next commit.
    pub fn idempotency(&self, key: &str) -> Result<Option<IdemRow>, DiskError> {
        if let Some(r) = self.idempotency.iter().rev().find(|r| r.key == key) {
            return Ok(Some(r.clone()));
        }
        let Some(d) = self.disk.0.as_ref() else {
            return Ok(None);
        };
        Ok(d.conn
            .query_row(
                "SELECT key, at_ms, request_hash, response FROM idempotency WHERE key = ?1",
                [key],
                |r| {
                    Ok(IdemRow {
                        key: r.get(0)?,
                        at_ms: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                        request_hash: r.get(2)?,
                        response: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// The keys of the calls answered at or after `since_ms`, for the server
    /// store's index (spec/persistence.md, "Idempotency keys").
    pub fn idempotency_since(&self, since_ms: u64) -> Result<Vec<(String, u64)>, DiskError> {
        let Some(d) = self.disk.0.as_ref() else {
            return Ok(Vec::new());
        };
        let mut st = d
            .conn
            .prepare("SELECT key, at_ms FROM idempotency WHERE at_ms >= ?1")?;
        let rows = st.query_map([i64::try_from(since_ms).unwrap_or(i64::MAX)], |r| {
            Ok((r.get(0)?, u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0)))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Delete the keys answered before `before_ms`.
    pub fn prune_idempotency(&mut self, before_ms: u64) -> Result<usize, DiskError> {
        let Some(d) = self.disk.0.as_ref() else {
            return Ok(0);
        };
        Ok(d.conn.execute(
            "DELETE FROM idempotency WHERE at_ms < ?1",
            [i64::try_from(before_ms).unwrap_or(i64::MAX)],
        )?)
    }

    /// The journal's body bytes the file holds, from the running count.
    pub(super) fn disk_journal_bytes(&self) -> u64 {
        self.disk.0.as_ref().map_or(0, |d| d.journal_bytes)
    }

    /// The journal rows written to the file and the time of the oldest.
    /// The journal is contiguous, so the rows are the span of `seq`.
    pub(super) fn disk_journal_extent(&self) -> (u64, Option<u64>) {
        let Some(d) = self.disk.0.as_ref() else {
            return (0, None);
        };
        let first: Option<(i64, i64)> = d
            .conn
            .query_row(
                "SELECT seq, at_ms FROM journal ORDER BY seq LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .ok()
            .flatten();
        let Some((min, at)) = first else {
            return (0, None);
        };
        let max = i64::try_from(d.journal_seq).unwrap_or(i64::MAX);
        let rows = u64::try_from(max - min + 1).unwrap_or(0);
        (rows, u64::try_from(at).ok())
    }

    /// Delete the oldest journal entries, at most `max_rows` of them
    /// (spec/persistence.md, "Journal retention"): those older than
    /// `retention_ms`, and while the bytes of the bodies kept are over
    /// `max_bytes`. The time cutoff is the earlier of `now_ms` and the newest
    /// entry's `at_ms`, less the retention. The delete takes a prefix in
    /// `seq` order ([`crate::retention::cut`]): the time rule ends at the
    /// first entry inside the retention, even if later entries are older;
    /// the size rule goes on past it. The newest entry is always kept, so
    /// `seq` goes on from it after a reopen. A retention of 0 turns the time
    /// rule off and a `max_bytes` of 0 turns the size rule off; with both
    /// off, with `max_rows` 0, or in a store in memory nothing is deleted.
    /// When the clock is behind the newest entry the time rule is skipped
    /// and the size rule applies. No other table is touched, and entries not
    /// yet written are not affected.
    pub fn prune_journal(
        &mut self,
        retention_ms: u64,
        now_ms: u64,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<JournalPrune, DiskError> {
        let Some(d) = self.disk.0.as_mut() else {
            return Ok(JournalPrune::Kept);
        };
        if (retention_ms == 0 && max_bytes == 0) || max_rows == 0 {
            return Ok(JournalPrune::Kept);
        }
        let newest: Option<(i64, i64)> = d
            .conn
            .query_row(
                "SELECT seq, at_ms FROM journal ORDER BY seq DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((newest_seq, newest_ms)) = newest else {
            return Ok(JournalPrune::Kept);
        };
        let newest_ms = u64::try_from(newest_ms).unwrap_or(0);
        let behind = retention_ms != 0 && now_ms < newest_ms;
        let cutoff = (retention_ms != 0 && !behind)
            .then(|| now_ms.min(newest_ms).saturating_sub(retention_ms));
        let excess = if max_bytes == 0 {
            0
        } else {
            d.journal_bytes.saturating_sub(max_bytes)
        };
        if cutoff.is_none() && excess == 0 {
            return Ok(if behind {
                JournalPrune::ClockBehind { now_ms, newest_ms }
            } else {
                JournalPrune::Kept
            });
        }
        let mut st = d
            .conn
            .prepare("SELECT seq, at_ms, length(body) FROM journal ORDER BY seq LIMIT ?1")?;
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
            return Ok(if behind {
                JournalPrune::ClockBehind { now_ms, newest_ms }
            } else {
                JournalPrune::Kept
            });
        };
        let kept = d.journal_bytes.saturating_sub(cut.bytes);
        let tx = d.conn.unchecked_transaction()?;
        let n = tx.execute(
            "DELETE FROM journal WHERE seq <= ?1",
            [i64::try_from(last).unwrap_or(i64::MAX)],
        )?;
        put_meta_raw(&tx, "journal_bytes", kept.to_string().as_bytes())?;
        tx.commit()?;
        d.journal_bytes = kept;
        d.journal_bytes_unwritten = false;
        Ok(JournalPrune::Deleted {
            rows: n as u64,
            more: cut.more,
        })
    }

    /// The last checkpoint written, if any.
    pub fn last_checkpoint(&self) -> Option<&Checkpoint> {
        self.disk
            .0
            .as_ref()
            .and_then(|d| d.last_checkpoint.as_ref())
    }

    /// The campaign record, for a store on disk.
    pub fn record(&self) -> Option<&CampaignRecord> {
        self.disk.0.as_ref().map(|d| &d.record)
    }

    /// Change the campaign record and write it with the rest.
    pub fn set_record(&mut self, record: CampaignRecord) -> Result<(), DiskError> {
        if let Some(d) = self.disk.0.as_mut() {
            d.record = record;
            d.record_written = false;
        }
        self.commit()
    }

    /// Set the campaign record without writing it: it goes to disk with the
    /// next commit, with the rest of that commit's changes.
    pub fn stage_record(&mut self, record: CampaignRecord) {
        if let Some(d) = self.disk.0.as_mut() {
            d.record = record;
            d.record_written = false;
        }
    }

    /// Write every change, then copy the file to `dest` consistently.
    pub fn backup_to(&mut self, dest: &Path) -> Result<(), DiskError> {
        self.commit()?;
        match self.disk.0.as_ref() {
            Some(d) => vacuum_into(&d.conn, dest),
            None => Ok(()),
        }
    }
}

impl Disk {
    fn write(&mut self, store: &Store) -> Result<(), DiskError> {
        self.write_with(store, None, false)
    }

    /// Write one transaction: the journal entries since the last one with
    /// the `inflight` rows they insert or delete, every row whose durable
    /// fields changed, `meta`, the idempotency rows, and with a checkpoint
    /// `cp` the sampled state and the checkpoint row (spec/persistence.md,
    /// "Writes"). `init` makes the tables first, for a new file.
    fn write_with(
        &mut self,
        store: &Store,
        cp: Option<&Checkpoint>,
        init: bool,
    ) -> Result<(), DiskError> {
        let Store {
            campaign,
            next_entity,
            rev,
            epoch: _,
            loaded,
            groups,
            units,
            journal,
            inflight: _,
            last_record,
            last_stamp,
            keep_journal: _,
            idempotency,
            changes: _,
            changes_keep: _,
            grid: _,
            extensions,
            owners,
            stocks,
            rates,
            owed_load,
            mission_start_ms,
            model0,
            shell_slot,
            next_mission,
            templates,
            templates_dirty,
            countries,
            sample_time,
            route_events,
            disk: _,
            markers: _,
            dirty: _,
            dirty_all: _,
            since_checkpoint: _,
            checkpoint_all: _,
            touched: _,
            touched_all: _,
            now_ms: _,
            removed_ids: _,
            purged_recent: _,
            dead_ids: _,
            journal_at: _,
            journal_mem_bytes: _,
            purged: _,
            expired: _,
            purged_deaths: _,
            orphan_destroys: _,
        } = store;
        let _ = take_overflow();
        // The step's clock when the driver set one, so the journal's times
        // and the purge read one clock.
        let at_ms = if store.now_ms != 0 {
            store.now_ms
        } else {
            now_ms()
        };
        let full = cp.is_some();
        // A step's commit compares only the groups marked since the last
        // write, with their members, durable fields only; a checkpoint
        // compares the groups marked since the last checkpoint, sampled
        // state too. A new file and a write after a bulk change compare
        // every row.
        let scan_all = init || store.dirty_all || (full && store.checkpoint_all);
        let rows = if scan_all {
            self.rows_all(groups, units, full)
        } else {
            let marks = if full {
                &store.since_checkpoint
            } else {
                &store.dirty
            };
            let marked = self.rows_marked(groups, units, marks, full);
            if cfg!(debug_assertions) {
                // Every change the marks missed would be written only at
                // the next checkpoint, or never: the tests check that none
                // is missed.
                let every = self.rows_all(groups, units, full);
                assert_eq!(
                    marked, every,
                    "a store change was made without marking its row"
                );
            }
            marked
        };
        let Rows {
            changed_groups,
            changed_units,
            gone_groups,
            gone_units,
        } = rows;
        let tx = self.conn.transaction()?;
        if init {
            tx.execute_batch(TABLES)?;
            put_meta(&tx, "schema", &Lv::Number(f64::from(SCHEMA)))?;
            put_meta(&tx, "campaign_id", &u(campaign.0))?;
        }
        for id in &changed_groups {
            put_entity(&tx, *id, "group", &enc_group(&groups[id]))?;
        }
        for id in &changed_units {
            put_entity(&tx, *id, "unit", &enc_unit(&units[id]))?;
        }
        for (ids, kind) in [(&gone_groups, "group"), (&gone_units, "unit")] {
            for id in ids {
                tx.execute(
                    "DELETE FROM entity WHERE id = ?1 AND kind = ?2",
                    params![id.0 as i64, kind],
                )?;
            }
        }
        let mut changed_ext = Vec::new();
        for (name, e) in extensions {
            if self.extensions.get(name) != Some(e) {
                tx.execute(
                    "INSERT OR REPLACE INTO extension (name, body) VALUES (?1, ?2)",
                    params![name, encode(&enc_extension(e))],
                )?;
                changed_ext.push(name.clone());
            }
        }
        // A removed extension keeps its row as a tombstone holding the
        // highest revision it had, so a registration of the name after a
        // reopen still gets a higher one.
        let mut changed_floors = Vec::new();
        for (name, rev) in store.extension_floors() {
            if !extensions.contains_key(name) && self.floors.get(name) != Some(rev) {
                tx.execute(
                    "INSERT OR REPLACE INTO extension (name, body) VALUES (?1, ?2)",
                    params![name, encode(&enc_tombstone(name, *rev))],
                )?;
                changed_floors.push((name.clone(), *rev));
            }
        }
        let mut seq = self.journal_seq;
        let mut added_bytes = 0u64;
        for e in journal.iter().skip(self.journal_written) {
            seq += 1;
            let body = enc_entry(e);
            let (kind, entity) = entry_key(&body);
            let encoded = encode(&body);
            added_bytes += encoded.len() as u64;
            tx.execute(
                "INSERT INTO journal (seq, at_ms, kind, entity, body) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![seq as i64, at_ms as i64, kind, entity as i64, encoded],
            )?;
            match e {
                Entry::Command(c) => {
                    tx.execute(
                        "INSERT OR REPLACE INTO inflight (epoch, seq, body) VALUES (?1, ?2, ?3)",
                        params![c.epoch as i64, c.seq as i64, encoded],
                    )?;
                }
                Entry::Outcome { epoch, seq, .. } => {
                    tx.execute(
                        "DELETE FROM inflight WHERE epoch = ?1 AND seq = ?2",
                        params![*epoch as i64, *seq as i64],
                    )?;
                }
                _ => {}
            }
        }
        let journal_bytes = self.journal_bytes + added_bytes;
        if added_bytes > 0 || self.journal_bytes_unwritten {
            put_meta_raw(&tx, "journal_bytes", journal_bytes.to_string().as_bytes())?;
        }
        for r in idempotency {
            tx.execute(
                "INSERT OR REPLACE INTO idempotency (key, at_ms, request_hash, response) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![r.key, r.at_ms as i64, r.request_hash, r.response],
            )?;
        }
        // Sampled state moves only at a checkpoint.
        let sample_time = if full { *sample_time } else { self.sample_time };
        let mut world_pairs = vec![
            ("next_entity", u(*next_entity)),
            ("open", b(self.open)),
            ("revision", u(*rev)),
            ("loaded", loaded.as_ref().map_or(Lv::Nil, enc_loaded)),
            ("last_record", opt_record(*last_record)),
            (
                "last_stamp",
                last_stamp.map_or(Lv::Nil, |(a, g, c)| {
                    list(vec![
                        s(&a.to_string()),
                        s(&g.to_string()),
                        s(&c.to_string()),
                    ])
                }),
            ),
            ("owners", enc_owners(owners)),
            ("stocks", enc_stocks(stocks)),
            ("rates", rates.as_ref().map_or(Lv::Nil, enc_rates)),
            (
                "owed_load",
                owed_load.as_ref().map_or(Lv::Nil, enc_owed_load),
            ),
        ];
        world_pairs.extend(enc_world_mission(
            *mission_start_ms,
            next_mission,
            *model0,
            *shell_slot,
        ));
        world_pairs.extend([
            (
                "countries",
                countries.as_ref().map_or(Lv::Nil, enc_countries),
            ),
            ("sample_time", f(sample_time)),
            (
                "route_events",
                list(route_events.iter().map(enc_route_event).collect()),
            ),
        ]);
        let world = encode(&t(world_pairs));
        if world != self.world {
            put_meta_raw(&tx, "world", &world)?;
        }
        let theaters: Vec<&String> = if init {
            templates.keys().collect()
        } else {
            templates_dirty.iter().collect()
        };
        for theater in theaters {
            match templates.get(theater) {
                Some(body) => tx.execute(
                    "INSERT OR REPLACE INTO template (theater, body) VALUES (?1, ?2)",
                    params![theater, body],
                )?,
                None => tx.execute("DELETE FROM template WHERE theater = ?1", params![theater])?,
            };
        }
        if !self.record_written {
            put_meta(&tx, "campaign", &enc_record(&self.record))?;
        }
        if let Some(c) = cp {
            tx.execute("DELETE FROM checkpoint", [])?;
            tx.execute(
                "INSERT INTO checkpoint (seq, body) VALUES (?1, ?2)",
                params![c.seq as i64, encode(&enc_checkpoint(c))],
            )?;
        }
        if let Some(e) = take_overflow() {
            // Dropping the transaction rolls it back.
            return Err(DiskError::TooLarge(e));
        }
        tx.commit()?;
        for id in changed_groups {
            self.groups.insert(id, groups[&id].clone());
        }
        for id in changed_units {
            self.units.insert(id, units[&id].clone());
        }
        for id in gone_groups {
            self.groups.remove(&id);
        }
        for id in gone_units {
            self.units.remove(&id);
        }
        for name in changed_ext {
            self.floors.remove(&name);
            self.extensions
                .insert(name.clone(), extensions[&name].clone());
        }
        self.extensions.retain(|n, _| extensions.contains_key(n));
        self.floors.extend(changed_floors);
        self.journal_seq = seq;
        self.journal_bytes = journal_bytes;
        self.journal_bytes_unwritten = false;
        self.journal_written = journal.len();
        self.world = world;
        self.sample_time = sample_time;
        self.record_written = true;
        Ok(())
    }
}

/// The entity rows one write puts or deletes, each list in ID order.
#[derive(Debug, Default, PartialEq)]
struct Rows {
    changed_groups: Vec<EntityId>,
    changed_units: Vec<EntityId>,
    gone_groups: Vec<EntityId>,
    gone_units: Vec<EntityId>,
}

impl Disk {
    fn group_differs(&self, g: &Group, full: bool) -> bool {
        match self.groups.get(&g.id) {
            None => true,
            Some(w) if full => w != g,
            // The lead position is sampled bookkeeping.
            Some(w) if w.last_lead == g.last_lead => w != g,
            Some(w) => {
                let mut same = g.clone();
                same.last_lead = w.last_lead;
                *w != same
            }
        }
    }

    fn unit_differs(&self, un: &Unit, full: bool) -> bool {
        match self.units.get(&un.id) {
            None => true,
            Some(w) if full => w != un,
            Some(w) => !durable_eq(w, un),
        }
    }

    /// Every row that differs from what the file holds.
    fn rows_all(
        &self,
        groups: &BTreeMap<EntityId, Group>,
        units: &BTreeMap<EntityId, Unit>,
        full: bool,
    ) -> Rows {
        Rows {
            changed_groups: groups
                .values()
                .filter(|g| self.group_differs(g, full))
                .map(|g| g.id)
                .collect(),
            changed_units: units
                .values()
                .filter(|u| self.unit_differs(u, full))
                .map(|u| u.id)
                .collect(),
            gone_groups: self
                .groups
                .keys()
                .filter(|id| !groups.contains_key(id))
                .copied()
                .collect(),
            gone_units: self
                .units
                .keys()
                .filter(|id| !units.contains_key(id))
                .copied()
                .collect(),
        }
    }

    /// The rows of the marked groups and of their members, now and as
    /// written, that differ from what the file holds, durable fields only.
    fn rows_marked(
        &self,
        groups: &BTreeMap<EntityId, Group>,
        units: &BTreeMap<EntityId, Unit>,
        marked: &BTreeSet<EntityId>,
        full: bool,
    ) -> Rows {
        let mut member_ids = BTreeSet::new();
        let mut rows = Rows::default();
        for id in marked {
            match groups.get(id) {
                Some(g) => {
                    if self.group_differs(g, full) {
                        rows.changed_groups.push(*id);
                    }
                    member_ids.extend(g.members.iter().copied());
                }
                None if self.groups.contains_key(id) => rows.gone_groups.push(*id),
                None => {}
            }
            if let Some(w) = self.groups.get(id) {
                member_ids.extend(w.members.iter().copied());
            }
        }
        for id in member_ids {
            match units.get(&id) {
                Some(u) if self.unit_differs(u, full) => rows.changed_units.push(id),
                Some(_) => {}
                None if self.units.contains_key(&id) => rows.gone_units.push(id),
                None => {}
            }
        }
        rows
    }
}

/// Whether two versions of a unit differ only in sampled state: the state a
/// journal or API commit does not write (spec/persistence.md, "Writes").
fn durable_eq(a: &Unit, b: &Unit) -> bool {
    let Unit {
        id,
        group,
        type_name,
        state: _,
        dead,
        missing,
        removed,
        left_logged,
        object,
        label,
        skill,
        captured,
        // Volatile: never written.
        loadout: _,
        extra,
    } = a;
    *id == b.id
        && *group == b.group
        && *type_name == b.type_name
        && *dead == b.dead
        && *missing == b.missing
        && *removed == b.removed
        && *left_logged == b.left_logged
        && *object == b.object
        && *label == b.label
        && *skill == b.skill
        && *captured == b.captured
        && *extra == b.extra
}

/// A journal entry's `kind` and the entity it names, 0 for none, from its
/// encoded body.
fn entry_key(body: &Lv) -> (String, u64) {
    let r = Rd(body);
    let kind = r.s("kind").unwrap_or_default();
    let entity = ["entity", "unit", "group"]
        .iter()
        .find_map(|k| r.u(k).ok().filter(|v| *v != 0))
        .unwrap_or(0);
    (kind, entity)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn opt_record(v: Option<(u64, u64)>) -> Lv {
    v.map_or(Lv::Nil, |(a, n)| {
        list(vec![s(&a.to_string()), s(&n.to_string())])
    })
}

fn load(path: &Path, conn: Connection, changes_keep: usize) -> Result<Store, DiskError> {
    conn.execute_batch(TEMPLATE_TABLE)?;
    let campaign = CampaignId(dec_u(&get_meta(&conn, "campaign_id")?, "campaign_id")?);
    let record = dec_record(&get_meta(&conn, "campaign")?)?;
    let world_bytes: Vec<u8> = conn
        .query_row("SELECT value FROM meta WHERE key = 'world'", [], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| DiskError::Corrupt("no meta world".into()))?;
    let mut store = Store::new(campaign, changes_keep);
    {
        let mut q = conn.prepare("SELECT theater, body FROM template")?;
        let rows = q.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (theater, body) = row?;
            store.templates.insert(theater, body);
        }
    }
    {
        let w = decode(&world_bytes)?;
        let w = Rd(&w);
        store.next_entity = w.u("next_entity")?;
        store.rev = w.u("revision")?;
        if w.b("open")? {
            store.rev += REVISION_RESERVE;
        }
        store.last_record = match w.list("last_record")?.as_slice() {
            [] => None,
            [boot, n] => Some((dec_u(boot, "last_record")?, dec_u(n, "last_record")?)),
            _ => return Err(DiskError::Corrupt("last_record".into())),
        };
        store.last_stamp = match w.list("last_stamp")?.as_slice() {
            [] => None,
            [a, g, c] => Some((
                dec_u(a, "last_stamp")?,
                u32::try_from(dec_u(g, "last_stamp")?)
                    .map_err(|_| DiskError::Corrupt("last_stamp".into()))?,
                dec_u(c, "last_stamp")?,
            )),
            _ => return Err(DiskError::Corrupt("last_stamp".into())),
        };
        store.loaded = match w.get("loaded") {
            Lv::Nil => None,
            l => Some(dec_loaded(l)?),
        };
        store.owners = dec_owners(w.get("owners"))?;
        store.stocks = dec_stocks(w.get("stocks"))?;
        store.rates = match w.get("rates") {
            Lv::Nil => None,
            v => Some(dec_rates(v)?),
        };
        store.mission_start_ms = opt_i64(w.get("mission_start_ms"), "mission_start_ms")?;
        store.model0 = match w.list("model0")?.as_slice() {
            [] => None,
            [a, g, Lv::Number(t0)] => Some((
                dec_u(a, "model0")?,
                u32::try_from(dec_u(g, "model0")?)
                    .map_err(|_| DiskError::Corrupt("model0".into()))?,
                *t0,
            )),
            _ => return Err(DiskError::Corrupt("model0".into())),
        };
        store.shell_slot = match w.get("shell_slot") {
            Lv::Nil => None,
            Lv::String(b) if b.as_slice() == b"a" || b.as_slice() == b"b" => Some(b[0]),
            _ => return Err(DiskError::Corrupt("shell_slot".into())),
        };
        store.next_mission = match w.get("next_mission") {
            Lv::Nil => None,
            v => Some(v.clone()),
        };
        // A store written before the key has no owed load.
        store.owed_load = match w.get("owed_load") {
            Lv::Nil => None,
            v => Some(dec_owed_load(v)?),
        };
        store.countries = match w.get("countries") {
            Lv::Nil => None,
            v => Some(dec_countries(v)?),
        };
        store.sample_time = w.f("sample_time")?;
        store.route_events = w
            .list("route_events")?
            .into_iter()
            .map(dec_route_event)
            .collect::<Result<_, _>>()?;
    }
    let mut groups = BTreeMap::new();
    let mut units = BTreeMap::new();
    {
        let mut st = conn.prepare("SELECT id, kind, body FROM entity ORDER BY id, kind")?;
        let mut rows = st.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let kind: String = row.get(1)?;
            let body: Vec<u8> = row.get(2)?;
            let lv = decode(&body)?;
            match kind.as_str() {
                "unit" => {
                    let un = dec_unit(&lv)?;
                    if un.id.0 as i64 != id {
                        return Err(DiskError::Corrupt(format!(
                            "unit row {id} holds {}",
                            un.id.0
                        )));
                    }
                    units.insert(un.id, un);
                }
                "group" => {
                    let g = dec_group(&lv)?;
                    if g.id.0 as i64 != id {
                        return Err(DiskError::Corrupt(format!(
                            "group row {id} holds {}",
                            g.id.0
                        )));
                    }
                    groups.insert(g.id, g);
                }
                k => return Err(DiskError::Corrupt(format!("entity {id}: kind {k}"))),
            }
        }
    }
    let mut extensions = BTreeMap::new();
    let mut floors = BTreeMap::new();
    {
        let mut st = conn.prepare("SELECT body FROM extension")?;
        let mut rows = st.query([])?;
        while let Some(row) = rows.next()? {
            let body: Vec<u8> = row.get(0)?;
            let lv = decode(&body)?;
            let e = dec_extension(&lv)?;
            if Rd(&lv).b("removed")? {
                floors.insert(e.name, e.spec_rev);
            } else {
                extensions.insert(e.name.clone(), e);
            }
        }
    }
    // The journal is not read: what a restart needs of it is in `inflight`.
    let mut inflight = BTreeMap::new();
    {
        let mut st = conn.prepare("SELECT epoch, seq, body FROM inflight ORDER BY epoch, seq")?;
        let mut rows = st.query([])?;
        while let Some(row) = rows.next()? {
            let body: Vec<u8> = row.get(2)?;
            match dec_entry(&decode(&body)?)? {
                Entry::Command(c) => {
                    inflight.insert((c.epoch, c.seq), c);
                }
                e => {
                    return Err(DiskError::Corrupt(format!("inflight row holds {e:?}")));
                }
            }
        }
    }
    let journal_seq: Option<i64> =
        conn.query_row("SELECT max(seq) FROM journal", [], |r| r.get(0))?;
    let kept_bytes = get_meta_raw(&conn, "journal_bytes")?
        .and_then(|v| String::from_utf8(v).ok())
        .and_then(|v| v.parse::<u64>().ok());
    let journal_bytes_unwritten = kept_bytes.is_none();
    let journal_bytes = match kept_bytes {
        Some(n) => n,
        None => {
            let n: i64 = conn.query_row(
                "SELECT coalesce(sum(length(body)), 0) FROM journal",
                [],
                |r| r.get(0),
            )?;
            u64::try_from(n).unwrap_or(0)
        }
    };
    let last_checkpoint: Option<Vec<u8>> = conn
        .query_row(
            "SELECT body FROM checkpoint ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let last_checkpoint = match last_checkpoint {
        Some(b) => Some(dec_checkpoint(&decode(&b)?)?),
        None => None,
    };
    // A unit whose group is not in the file cannot be: the group row and its
    // units are written in one transaction.
    for un in units.values() {
        if !groups.contains_key(&un.group) {
            return Err(DiskError::Corrupt(format!(
                "unit {} of a group not in the store",
                un.id.0
            )));
        }
    }
    for g in groups.values() {
        if let Some(m) = g.members.iter().find(|m| !units.contains_key(m)) {
            return Err(DiskError::Corrupt(format!(
                "group {} names member {}, not in the store",
                g.id.0, m.0
            )));
        }
    }
    if let Some(id) = groups
        .keys()
        .chain(units.keys())
        .find(|id| id.0 >= store.next_entity)
    {
        return Err(DiskError::Corrupt(format!(
            "entity {} at or past the next EntityId {}",
            id.0, store.next_entity
        )));
    }
    for un in units.values() {
        // A dead static stays in the grid: it is a ruin where it stood. A
        // removed entity's units are not in it.
        let g = &groups[&un.group];
        if !g.removed && (!un.dead || g.kind == Kind::Static) {
            store
                .grid
                .set(un.id, &g.theater, un.state.at.x, un.state.at.z);
        }
    }
    let world = world_bytes;
    store.groups = groups.clone();
    store.units = units.clone();
    store.reindex();
    store.extensions = extensions.clone();
    store.set_extension_floors(floors.clone());
    store.inflight = inflight;
    store.keep_journal = false;
    let sample_time = store.sample_time;
    store.disk = Slot(Some(Box::new(Disk {
        path: path.to_owned(),
        conn,
        groups,
        units,
        extensions,
        floors,
        journal_written: 0,
        journal_seq: u64::try_from(journal_seq.unwrap_or(0)).unwrap_or(0),
        journal_bytes,
        journal_bytes_unwritten,
        world,
        sample_time,
        record,
        record_written: true,
        last_checkpoint,
        open: true,
    })));
    // Mark it open, and keep a moved revision, before anything else.
    store.commit()?;
    Ok(store)
}

// ---- rows ----

fn put_entity(tx: &Transaction<'_>, id: EntityId, kind: &str, body: &Lv) -> Result<(), DiskError> {
    tx.execute(
        "INSERT OR REPLACE INTO entity (id, kind, body) VALUES (?1, ?2, ?3)",
        params![id.0 as i64, kind, encode(body)],
    )?;
    Ok(())
}

fn put_meta(tx: &Transaction<'_>, key: &str, v: &Lv) -> Result<(), DiskError> {
    put_meta_raw(tx, key, &encode(v))
}

fn put_meta_raw(tx: &Transaction<'_>, key: &str, v: &[u8]) -> Result<(), DiskError> {
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, v],
    )?;
    Ok(())
}

fn get_meta_raw(conn: &Connection, key: &str) -> Result<Option<Vec<u8>>, DiskError> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?)
}

/// The bytes of an entry's body as the journal stores it: what the size
/// limit of the journal counts (spec/persistence.md, "Journal retention").
pub(crate) fn entry_bytes(e: &Entry) -> u64 {
    encode(&enc_entry(e)).len() as u64
}

fn get_meta(conn: &Connection, key: &str) -> Result<Lv, DiskError> {
    let v: Option<Vec<u8>> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?;
    decode(&v.ok_or_else(|| DiskError::Corrupt(format!("no meta {key}")))?)
}

// ---- lv helpers ----

thread_local! {
    /// Why a value encoded since the last [`take_overflow`] did not encode
    /// whole, if one did not.
    static OVERFLOW: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Take what [`encode`] found since the last call.
fn take_overflow() -> Option<String> {
    OVERFLOW.with(|o| o.borrow_mut().take())
}

/// Encode `v`. A value that does not encode whole ([`check_lv`]) is noted,
/// and the transaction that writes it fails before it commits.
fn encode(v: &Lv) -> Vec<u8> {
    if let Err(e) = check_lv(v) {
        OVERFLOW.with(|o| {
            o.borrow_mut().get_or_insert(e);
        });
    }
    let mut w = Writer::new();
    w.lv(v);
    w.finish()
}

fn decode(b: &[u8]) -> Result<Lv, DiskError> {
    let mut r = Reader::new(b);
    r.lv().map_err(|e| DiskError::Corrupt(e.to_string()))
}

fn t(pairs: Vec<(&str, Lv)>) -> Lv {
    Lv::Table(
        pairs
            .into_iter()
            .filter(|(_, v)| !matches!(v, Lv::Nil))
            .map(|(k, v)| (s(k), v))
            .collect(),
    )
}

fn s(v: &str) -> Lv {
    Lv::String(v.as_bytes().to_vec())
}

/// A `u64` as decimal text: a Lua number would round it past 2^53.
fn u(v: u64) -> Lv {
    if v == 0 { Lv::Nil } else { s(&v.to_string()) }
}

fn f(v: f64) -> Lv {
    if v == 0.0 && v.is_sign_positive() {
        Lv::Nil
    } else {
        Lv::Number(v)
    }
}

fn b(v: bool) -> Lv {
    if v { Lv::Bool(true) } else { Lv::Nil }
}

fn list(items: Vec<Lv>) -> Lv {
    Lv::Table(
        items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Lv::Number((i + 1) as f64), v))
            .collect(),
    )
}

/// A nested value as its own encoding, so the row's depth stays fixed.
fn nested(v: &Lv) -> Lv {
    if matches!(v, Lv::Nil) {
        Lv::Nil
    } else {
        Lv::String(encode(v))
    }
}

/// The keys of `meta.world` that follow the loaded mission: its start, the
/// next mission's settings, the model time of the generation's first sample
/// and the shell slot DCS may hold.
fn enc_world_mission(
    mission_start_ms: Option<i64>,
    next_mission: &Option<Lv>,
    model0: Option<(u64, u32, f64)>,
    shell_slot: Option<u8>,
) -> Vec<(&'static str, Lv)> {
    vec![
        (
            "mission_start_ms",
            mission_start_ms.map_or(Lv::Nil, |v| s(&v.to_string())),
        ),
        ("next_mission", next_mission.clone().unwrap_or(Lv::Nil)),
        (
            "model0",
            model0.map_or(Lv::Nil, |(a, g, t0)| {
                list(vec![s(&a.to_string()), s(&g.to_string()), Lv::Number(t0)])
            }),
        ),
        (
            "shell_slot",
            shell_slot.map_or(Lv::Nil, |c| s(&char::from(c).to_string())),
        ),
    ]
}

fn opt_pair(v: Option<(u64, u32)>) -> Lv {
    v.map_or(Lv::Nil, |(a, g)| {
        list(vec![s(&a.to_string()), s(&g.to_string())])
    })
}

/// A reader of a table row.
struct Rd<'a>(&'a Lv);

static NIL: Lv = Lv::Nil;

impl<'a> Rd<'a> {
    fn get(&self, key: &str) -> &'a Lv {
        if let Lv::Table(pairs) = self.0 {
            for (k, v) in pairs {
                if let Lv::String(kb) = k
                    && kb == key.as_bytes()
                {
                    return v;
                }
            }
        }
        &NIL
    }

    fn s(&self, key: &str) -> Result<String, DiskError> {
        match self.get(key) {
            Lv::Nil => Ok(String::new()),
            Lv::String(v) => String::from_utf8(v.clone())
                .map_err(|_| DiskError::Corrupt(format!("{key} is not UTF-8"))),
            _ => Err(DiskError::Corrupt(format!("{key} is not a string"))),
        }
    }

    fn u(&self, key: &str) -> Result<u64, DiskError> {
        dec_u(self.get(key), key)
    }

    fn u32(&self, key: &str) -> Result<u32, DiskError> {
        u32::try_from(self.u(key)?).map_err(|_| DiskError::Corrupt(format!("{key} too large")))
    }

    fn opt_f(&self, key: &str) -> Result<Option<f64>, DiskError> {
        match self.get(key) {
            Lv::Nil => Ok(None),
            Lv::Number(n) => Ok(Some(*n)),
            _ => Err(DiskError::Corrupt(format!("{key} is not a number"))),
        }
    }

    fn f(&self, key: &str) -> Result<f64, DiskError> {
        match self.get(key) {
            Lv::Nil => Ok(0.0),
            Lv::Number(n) => Ok(*n),
            _ => Err(DiskError::Corrupt(format!("{key} is not a number"))),
        }
    }

    fn b(&self, key: &str) -> Result<bool, DiskError> {
        match self.get(key) {
            Lv::Nil => Ok(false),
            Lv::Bool(v) => Ok(*v),
            _ => Err(DiskError::Corrupt(format!("{key} is not a boolean"))),
        }
    }

    fn nested(&self, key: &str) -> Result<Lv, DiskError> {
        match self.get(key) {
            Lv::Nil => Ok(Lv::Nil),
            Lv::String(v) => decode(v),
            _ => Err(DiskError::Corrupt(format!("{key} is not nested"))),
        }
    }

    fn list(&self, key: &str) -> Result<Vec<&'a Lv>, DiskError> {
        match self.get(key) {
            Lv::Nil => Ok(Vec::new()),
            Lv::Table(pairs) => {
                let mut out = Vec::with_capacity(pairs.len());
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if !matches!(k, Lv::Number(n) if *n == (i + 1) as f64) {
                        return Err(DiskError::Corrupt(format!("{key} is not a list")));
                    }
                    out.push(v);
                }
                Ok(out)
            }
            _ => Err(DiskError::Corrupt(format!("{key} is not a list"))),
        }
    }

    fn pair(&self, key: &str) -> Result<Option<(u64, u32)>, DiskError> {
        let l = self.list(key)?;
        match l.as_slice() {
            [] => Ok(None),
            [a, g] => {
                let a = dec_u(a, key)?;
                let g = u32::try_from(dec_u(g, key)?)
                    .map_err(|_| DiskError::Corrupt(format!("{key} too large")))?;
                Ok(Some((a, g)))
            }
            _ => Err(DiskError::Corrupt(format!("{key} is not a pair"))),
        }
    }
}

fn dec_u(v: &Lv, key: &str) -> Result<u64, DiskError> {
    match v {
        Lv::Nil => Ok(0),
        Lv::String(t) => std::str::from_utf8(t)
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| DiskError::Corrupt(format!("{key} is not a number"))),
        _ => Err(DiskError::Corrupt(format!("{key} is not a number"))),
    }
}

// ---- enums ----

fn state_name(v: State) -> &'static str {
    match v {
        State::Virtual => "virtual",
        State::Materializing => "materializing",
        State::Live => "live",
        State::Dematerializing => "dematerializing",
        State::Dead => "dead",
        State::Lost => "lost",
        State::Unspawnable => "unspawnable",
    }
}

fn dec_state(v: &str) -> Result<State, DiskError> {
    State::ALL
        .into_iter()
        .find(|st| state_name(*st) == v)
        .ok_or_else(|| DiskError::Corrupt(format!("state {v}")))
}

fn authority_name(v: Authority) -> &'static str {
    match v {
        Authority::Campaign => "campaign",
        Authority::Sim => "sim",
    }
}

fn dec_authority(v: &str) -> Result<Authority, DiskError> {
    match v {
        "campaign" => Ok(Authority::Campaign),
        "sim" => Ok(Authority::Sim),
        _ => Err(DiskError::Corrupt(format!("authority {v}"))),
    }
}

fn policy_name(v: LostPolicy) -> &'static str {
    match v {
        LostPolicy::Report => "report",
        LostPolicy::Respawn => "respawn",
        LostPolicy::Dead => "dead",
    }
}

fn dec_policy(v: &str) -> Result<Option<LostPolicy>, DiskError> {
    match v {
        "" => Ok(None),
        "report" => Ok(Some(LostPolicy::Report)),
        "respawn" => Ok(Some(LostPolicy::Respawn)),
        "dead" => Ok(Some(LostPolicy::Dead)),
        _ => Err(DiskError::Corrupt(format!("lost policy {v}"))),
    }
}

fn outcome_name(v: Outcome) -> &'static str {
    match v {
        Outcome::Applied => "applied",
        Outcome::Refused => "refused",
        Outcome::Rejected => "rejected",
        Outcome::Unknown => "unknown",
        Outcome::NotSent => "not_sent",
    }
}

fn dec_outcome(v: &str) -> Result<Outcome, DiskError> {
    [
        Outcome::Applied,
        Outcome::Refused,
        Outcome::Rejected,
        Outcome::Unknown,
        Outcome::NotSent,
    ]
    .into_iter()
    .find(|o| outcome_name(*o) == v)
    .ok_or_else(|| DiskError::Corrupt(format!("outcome {v}")))
}

fn left_name(v: LeftAs) -> &'static str {
    match v {
        LeftAs::Missing => "missing",
        LeftAs::Removed => "removed",
    }
}

fn dec_left(v: &str) -> Result<LeftAs, DiskError> {
    match v {
        "missing" => Ok(LeftAs::Missing),
        "removed" => Ok(LeftAs::Removed),
        _ => Err(DiskError::Corrupt(format!("left as {v}"))),
    }
}

fn campaign_state_name(v: CampaignState) -> &'static str {
    match v {
        CampaignState::Active => "active",
        CampaignState::Suspended => "suspended",
        CampaignState::Finished => "finished",
    }
}

fn dec_campaign_state(v: &str) -> Result<CampaignState, DiskError> {
    match v {
        "active" => Ok(CampaignState::Active),
        "suspended" => Ok(CampaignState::Suspended),
        "finished" => Ok(CampaignState::Finished),
        _ => Err(DiskError::Corrupt(format!("campaign state {v}"))),
    }
}

// ---- rows of the world ----

fn enc_point(p: &Point) -> Lv {
    let Point { x, y, z } = *p;
    list(vec![Lv::Number(x), Lv::Number(y), Lv::Number(z)])
}

fn dec_point(v: &Lv) -> Result<Point, DiskError> {
    match v {
        Lv::Table(p) if p.len() == 3 => {
            let n = |i: usize| match &p[i].1 {
                Lv::Number(n) => Ok(*n),
                _ => Err(DiskError::Corrupt("point".into())),
            };
            Ok(Point {
                x: n(0)?,
                y: n(1)?,
                z: n(2)?,
            })
        }
        _ => Err(DiskError::Corrupt("point".into())),
    }
}

fn enc_unit_state(v: &UnitState) -> Lv {
    let UnitState { at, heading, life } = v;
    t(vec![
        ("at", enc_point(at)),
        ("heading", f(*heading)),
        ("life", f(f64::from(*life))),
    ])
}

fn dec_unit_state(v: &Lv) -> Result<UnitState, DiskError> {
    let r = Rd(v);
    Ok(UnitState {
        at: dec_point(r.get("at"))?,
        heading: r.f("heading")?,
        life: r.f("life")? as f32,
    })
}

fn enc_unit(v: &Unit) -> Lv {
    let Unit {
        id,
        group,
        type_name,
        state,
        dead,
        missing,
        removed,
        left_logged,
        object,
        label,
        skill,
        extra,
        captured,
        // Volatile: never written.
        loadout: _,
    } = v;
    let CapturedMore {
        life0,
        pitch,
        roll,
        life_percent,
    } = captured;
    let mut c = vec![
        ("life0", f(*life0)),
        ("pitch", f(*pitch)),
        ("roll", f(*roll)),
    ];
    // Absent when not asked, so a row without them reads back as before.
    if let Some(p) = life_percent {
        c.push(("life_percent", f(*p)));
    }
    t(vec![
        ("kind", s("unit")),
        ("captured", t(c)),
        ("id", u(id.0)),
        ("group", u(group.0)),
        ("type", s(type_name)),
        ("state", enc_unit_state(state)),
        ("dead", b(*dead)),
        ("missing", b(*missing)),
        ("removed", b(*removed)),
        ("left_logged", b(*left_logged)),
        ("object", u(*object)),
        ("label", s(label)),
        ("skill", s(skill)),
        ("extra", nested(extra)),
    ])
}

fn dec_unit(v: &Lv) -> Result<Unit, DiskError> {
    let r = Rd(v);
    Ok(Unit {
        id: EntityId(r.u("id")?),
        group: EntityId(r.u("group")?),
        type_name: r.s("type")?,
        state: dec_unit_state(r.get("state"))?,
        dead: r.b("dead")?,
        missing: r.b("missing")?,
        removed: r.b("removed")?,
        left_logged: r.b("left_logged")?,
        object: r.u("object")?,
        label: r.s("label")?,
        skill: r.s("skill")?,
        extra: r.nested("extra")?,
        captured: {
            let c = Rd(r.get("captured"));
            CapturedMore {
                life0: c.f("life0")?,
                pitch: c.f("pitch")?,
                roll: c.f("roll")?,
                life_percent: c.opt_f("life_percent")?,
            }
        },
        loadout: None,
    })
}

fn enc_intent(v: &Intent) -> Lv {
    let wp = |w: &Waypoint| {
        let Waypoint { x, z } = *w;
        list(vec![Lv::Number(x), Lv::Number(z)])
    };
    match v {
        Intent::Route {
            waypoints,
            speed,
            on_road,
        } => t(vec![
            ("kind", s("route")),
            ("waypoints", list(waypoints.iter().map(wp).collect())),
            ("speed", f(*speed)),
            ("on_road", b(*on_road)),
        ]),
        Intent::Attack { target } => t(vec![("kind", s("attack")), ("target", u(target.0))]),
        Intent::Hold => t(vec![("kind", s("hold"))]),
        Intent::Orbit { at, alt, speed } => t(vec![
            ("kind", s("orbit")),
            ("at", wp(at)),
            ("alt", f(*alt)),
            ("speed", f(*speed)),
        ]),
    }
}

fn dec_waypoint(v: &Lv) -> Result<Waypoint, DiskError> {
    match v {
        Lv::Table(p) if p.len() == 2 => match (&p[0].1, &p[1].1) {
            (Lv::Number(x), Lv::Number(z)) => Ok(Waypoint { x: *x, z: *z }),
            _ => Err(DiskError::Corrupt("waypoint".into())),
        },
        _ => Err(DiskError::Corrupt("waypoint".into())),
    }
}

fn dec_intent(v: &Lv) -> Result<Intent, DiskError> {
    let r = Rd(v);
    Ok(match r.s("kind")?.as_str() {
        "route" => Intent::Route {
            waypoints: r
                .list("waypoints")?
                .into_iter()
                .map(dec_waypoint)
                .collect::<Result<_, _>>()?,
            speed: r.f("speed")?,
            on_road: r.b("on_road")?,
        },
        "attack" => Intent::Attack {
            target: EntityId(r.u("target")?),
        },
        "hold" => Intent::Hold,
        "orbit" => Intent::Orbit {
            at: dec_waypoint(r.get("at"))?,
            alt: r.f("alt")?,
            speed: r.f("speed")?,
        },
        k => return Err(DiskError::Corrupt(format!("order kind {k}"))),
    })
}

fn enc_group(v: &Group) -> Lv {
    let Group {
        id,
        kind,
        theater,
        country,
        category,
        members,
        label,
        coalition,
        extra,
        components,
        spec_rev,
        order,
        order_rev,
        progress,
        state,
        authority,
        on_lost,
        error,
        captured,
        spawned_in,
        destroy_failures,
        unknown_spawn_in,
        order_sent_in,
        observed_coalition,
        removed,
        removed_at_ms,
        expired,
        dead_at_ms,
        options,
        last_lead,
        ruin_sent_in,
        ruin_in,
    } = v;
    let kind = match kind {
        Kind::Group => "group",
        Kind::Static => "static",
    };
    t(vec![
        ("kind", s(kind)),
        ("id", u(id.0)),
        ("theater", s(theater)),
        ("country", u(u64::from(*country))),
        ("category", u(u64::from(*category))),
        (
            "members",
            list(members.iter().map(|m| u(m.0).or_zero()).collect()),
        ),
        ("label", s(label)),
        ("coalition", u(u64::from(*coalition))),
        ("extra", nested(extra)),
        (
            "components",
            Lv::Table(
                components
                    .iter()
                    .map(|(k, v)| (s(k), Lv::String(v.clone())))
                    .collect(),
            ),
        ),
        ("spec_rev", u(*spec_rev)),
        ("order", order.as_ref().map_or(Lv::Nil, enc_intent)),
        ("order_rev", u(*order_rev)),
        ("progress", u(*progress as u64)),
        ("state", s(state_name(*state))),
        ("authority", s(authority_name(*authority))),
        ("on_lost", on_lost.map_or(Lv::Nil, |p| s(policy_name(p)))),
        (
            "error",
            error.as_deref().map_or(Lv::Nil, |e| list(vec![s(e)])),
        ),
        ("captured", b(*captured)),
        ("spawned_in", opt_pair(*spawned_in)),
        ("destroy_failures", u(u64::from(*destroy_failures))),
        ("unknown_spawn_in", opt_pair(*unknown_spawn_in)),
        ("ruin_sent_in", opt_pair(*ruin_sent_in)),
        ("ruin_in", opt_pair(*ruin_in)),
        (
            "order_sent_in",
            order_sent_in.map_or(Lv::Nil, |(a, g, r)| {
                list(vec![
                    s(&a.to_string()),
                    s(&g.to_string()),
                    s(&r.to_string()),
                ])
            }),
        ),
        (
            "observed_coalition",
            observed_coalition.map_or(Lv::Nil, |c| list(vec![s(&c.to_string())])),
        ),
        ("removed", b(*removed)),
        ("removed_at_ms", opt_ms(*removed_at_ms)),
        ("expired", b(*expired)),
        ("dead_at_ms", opt_ms(*dead_at_ms)),
        (
            "options",
            list(
                options
                    .iter()
                    .map(|(k, v)| list(vec![s(&k.to_string()), nested(v).or_zero()]))
                    .collect(),
            ),
        ),
        (
            "last_lead",
            last_lead.map_or(Lv::Nil, |(m, x, z, t)| {
                list(vec![
                    s(&m.0.to_string()),
                    Lv::Number(x),
                    Lv::Number(z),
                    Lv::Number(t),
                ])
            }),
        ),
    ])
}

/// An optional wall-clock time in milliseconds: a list of its decimal
/// text, or nil.
fn opt_ms(v: Option<u64>) -> Lv {
    v.map_or(Lv::Nil, |ms| list(vec![s(&ms.to_string())]))
}

/// Read [`opt_ms`]; a body written without the key reads `None`.
fn dec_opt_ms(r: &Rd<'_>, key: &str) -> Result<Option<u64>, DiskError> {
    match r.list(key)?.as_slice() {
        [] => Ok(None),
        [v] => Ok(Some(dec_u(v, key)?)),
        _ => Err(DiskError::Corrupt(key.into())),
    }
}

trait OrZero {
    fn or_zero(self) -> Lv;
}

impl OrZero for Lv {
    /// A list item is never nil: nil would end a Lua list.
    fn or_zero(self) -> Lv {
        if matches!(self, Lv::Nil) {
            s("0")
        } else {
            self
        }
    }
}

fn dec_group(v: &Lv) -> Result<Group, DiskError> {
    let r = Rd(v);
    let kind = match r.s("kind")?.as_str() {
        "group" => Kind::Group,
        "static" => Kind::Static,
        k => return Err(DiskError::Corrupt(format!("group kind {k}"))),
    };
    let members = r
        .list("members")?
        .into_iter()
        .map(|m| dec_u(m, "members").map(EntityId))
        .collect::<Result<_, _>>()?;
    let components = match r.get("components") {
        Lv::Nil => BTreeMap::new(),
        Lv::Table(pairs) => {
            let mut out = BTreeMap::new();
            for (k, v) in pairs {
                match (k, v) {
                    (Lv::String(k), Lv::String(v)) => {
                        let k = String::from_utf8(k.clone())
                            .map_err(|_| DiskError::Corrupt("component name".into()))?;
                        out.insert(k, v.clone());
                    }
                    _ => return Err(DiskError::Corrupt("component".into())),
                }
            }
            out
        }
        _ => return Err(DiskError::Corrupt("components".into())),
    };
    let order = match r.get("order") {
        Lv::Nil => None,
        o => Some(dec_intent(o)?),
    };
    let error = match r.list("error")?.as_slice() {
        [] => None,
        [e] => Some(Rd(&t(vec![("e", (*e).clone())])).s("e")?),
        _ => return Err(DiskError::Corrupt("error".into())),
    };
    let order_sent_in = match r.list("order_sent_in")?.as_slice() {
        [] => None,
        [a, g, rv] => Some((
            dec_u(a, "order_sent_in")?,
            u32::try_from(dec_u(g, "order_sent_in")?)
                .map_err(|_| DiskError::Corrupt("order_sent_in".into()))?,
            dec_u(rv, "order_sent_in")?,
        )),
        _ => return Err(DiskError::Corrupt("order_sent_in".into())),
    };
    Ok(Group {
        id: EntityId(r.u("id")?),
        kind,
        theater: r.s("theater")?,
        country: r.u32("country")?,
        category: r.u32("category")?,
        members,
        label: r.s("label")?,
        coalition: r.u32("coalition")?,
        extra: r.nested("extra")?,
        components,
        spec_rev: r.u("spec_rev")?,
        order,
        order_rev: r.u("order_rev")?,
        progress: usize::try_from(r.u("progress")?)
            .map_err(|_| DiskError::Corrupt("progress".into()))?,
        state: dec_state(&r.s("state")?)?,
        authority: dec_authority(&r.s("authority")?)?,
        on_lost: dec_policy(&r.s("on_lost")?)?,
        error,
        captured: r.b("captured")?,
        spawned_in: r.pair("spawned_in")?,
        destroy_failures: r.u32("destroy_failures")?,
        unknown_spawn_in: r.pair("unknown_spawn_in")?,
        ruin_sent_in: r.pair("ruin_sent_in")?,
        ruin_in: r.pair("ruin_in")?,
        order_sent_in,
        observed_coalition: match r.list("observed_coalition")?.as_slice() {
            [] => None,
            [c] => Some(
                u32::try_from(dec_u(c, "observed_coalition")?)
                    .map_err(|_| DiskError::Corrupt("observed_coalition".into()))?,
            ),
            _ => return Err(DiskError::Corrupt("observed_coalition".into())),
        },
        removed: r.b("removed")?,
        removed_at_ms: dec_opt_ms(&r, "removed_at_ms")?,
        // A body written without the key was removed by the controller.
        expired: matches!(r.get("expired"), Lv::Bool(true)),
        dead_at_ms: dec_opt_ms(&r, "dead_at_ms")?,
        options: {
            let mut out = BTreeMap::new();
            for o in r.list("options")? {
                match Rd(&t(vec![("o", o.clone())])).list("o")?.as_slice() {
                    [k, v] => {
                        let k = u32::try_from(dec_u(k, "option")?)
                            .map_err(|_| DiskError::Corrupt("option".into()))?;
                        let v = match v {
                            Lv::String(b) if b.as_slice() == b"0" => Lv::Nil,
                            Lv::String(b) => decode(b)?,
                            _ => return Err(DiskError::Corrupt("option value".into())),
                        };
                        out.insert(k, v);
                    }
                    _ => return Err(DiskError::Corrupt("option".into())),
                }
            }
            out
        },
        last_lead: match r.list("last_lead")?.as_slice() {
            [] => None,
            [m, Lv::Number(x), Lv::Number(z), Lv::Number(t)] => {
                Some((EntityId(dec_u(m, "last_lead")?), *x, *z, *t))
            }
            _ => return Err(DiskError::Corrupt("last_lead".into())),
        },
    })
}

fn enc_extension(v: &Extension) -> Lv {
    let Extension {
        name,
        spec_rev,
        source,
    } = v;
    t(vec![
        ("name", s(name)),
        ("spec_rev", u(*spec_rev)),
        ("source", s(source)),
    ])
}

/// The row of a removed extension: its name and the highest revision it had.
fn enc_tombstone(name: &str, floor: u64) -> Lv {
    t(vec![
        ("name", s(name)),
        ("spec_rev", u(floor)),
        ("removed", b(true)),
    ])
}

fn dec_extension(v: &Lv) -> Result<Extension, DiskError> {
    let r = Rd(v);
    Ok(Extension {
        name: r.s("name")?,
        spec_rev: r.u("spec_rev")?,
        source: r.s("source")?,
    })
}

fn enc_owed_load(v: &OwedLoad) -> Lv {
    let OwedLoad {
        path,
        cmd,
        start_ms,
    } = v;
    t(vec![
        ("path", s(path)),
        ("start_ms", start_ms.map_or(Lv::Nil, |v| s(&v.to_string()))),
        (
            "cmd",
            cmd.map_or(Lv::Nil, |(e, q)| {
                list(vec![s(&e.to_string()), s(&q.to_string())])
            }),
        ),
    ])
}

fn dec_owed_load(v: &Lv) -> Result<OwedLoad, DiskError> {
    let r = Rd(v);
    let cmd = match r.list("cmd")?.as_slice() {
        [] => None,
        [e, q] => Some((dec_u(e, "owed_load")?, dec_u(q, "owed_load")?)),
        _ => return Err(DiskError::Corrupt("owed_load".into())),
    };
    Ok(OwedLoad {
        path: r.s("path")?,
        cmd,
        start_ms: opt_i64(r.get("start_ms"), "owed_load start_ms")?,
    })
}

/// An `i64` kept as decimal text, or nil for none.
fn opt_i64(v: &Lv, what: &str) -> Result<Option<i64>, DiskError> {
    match v {
        Lv::Nil => Ok(None),
        Lv::String(b) => std::str::from_utf8(b)
            .ok()
            .and_then(|t| t.parse().ok())
            .map(Some)
            .ok_or_else(|| DiskError::Corrupt(what.into())),
        _ => Err(DiskError::Corrupt(what.into())),
    }
}

fn enc_loaded(v: &Loaded) -> Lv {
    let Loaded {
        agent_boot,
        mission_gen,
        theater,
    } = v;
    t(vec![
        ("agent_boot", u(*agent_boot)),
        ("mission_gen", u(u64::from(*mission_gen))),
        ("theater", s(theater)),
    ])
}

fn dec_loaded(v: &Lv) -> Result<Loaded, DiskError> {
    let r = Rd(v);
    Ok(Loaded {
        agent_boot: r.u("agent_boot")?,
        mission_gen: r.u32("mission_gen")?,
        theater: r.s("theater")?,
    })
}

fn enc_journalled(v: &Journalled) -> Lv {
    let Journalled {
        agent_boot,
        epoch,
        seq,
        opcode,
        entity,
        rev,
        mission_gen,
        position,
        units,
    } = v;
    t(vec![
        ("kind", s("command")),
        ("agent_boot", u(*agent_boot)),
        ("epoch", u(*epoch)),
        ("seq", u(*seq)),
        ("opcode", u(u64::from(*opcode))),
        ("entity", u(entity.0)),
        ("rev", u(*rev)),
        ("mission_gen", u(u64::from(*mission_gen))),
        ("position", u(*position)),
        (
            "units",
            list(units.iter().map(|m| u(m.0).or_zero()).collect()),
        ),
    ])
}

fn dec_journalled(r: &Rd<'_>) -> Result<Journalled, DiskError> {
    Ok(Journalled {
        agent_boot: r.u("agent_boot")?,
        epoch: r.u("epoch")?,
        seq: r.u("seq")?,
        opcode: u16::try_from(r.u("opcode")?).map_err(|_| DiskError::Corrupt("opcode".into()))?,
        entity: EntityId(r.u("entity")?),
        rev: r.u("rev")?,
        mission_gen: r.u32("mission_gen")?,
        position: r.u("position")?,
        units: r
            .list("units")?
            .into_iter()
            .map(|m| dec_u(m, "units").map(EntityId))
            .collect::<Result<_, _>>()?,
    })
}

fn enc_entry(e: &Entry) -> Lv {
    let k = |kind: &str, mut rest: Vec<(&str, Lv)>| {
        rest.insert(0, ("kind", s(kind)));
        t(rest)
    };
    match e {
        Entry::Spec { entity, rev } => k("spec", vec![("entity", u(entity.0)), ("rev", u(*rev))]),
        Entry::ExtensionSpec { name, rev } => {
            k("extension_spec", vec![("name", s(name)), ("rev", u(*rev))])
        }
        Entry::ExtensionRemoved { name } => k("extension_removed", vec![("name", s(name))]),
        Entry::Order { entity, rev } => k("order", vec![("entity", u(entity.0)), ("rev", u(*rev))]),
        Entry::Authority { entity, authority } => k(
            "authority",
            vec![
                ("entity", u(entity.0)),
                ("authority", s(authority_name(*authority))),
            ],
        ),
        Entry::Transition {
            entity,
            from,
            to,
            spec_rev,
        } => k(
            "transition",
            vec![
                ("entity", u(entity.0)),
                ("from", s(state_name(*from))),
                ("to", s(state_name(*to))),
                (
                    "spec_rev",
                    spec_rev.map_or(Lv::Nil, |r| list(vec![s(&r.to_string())])),
                ),
            ],
        ),
        Entry::Captured { entity } => k("captured", vec![("entity", u(entity.0))]),
        Entry::Death { unit } => k("death", vec![("unit", u(unit.0))]),
        Entry::Resolve { unit, dead } => {
            k("resolve", vec![("unit", u(unit.0)), ("dead", b(*dead))])
        }
        Entry::Left { unit, object, how } => k(
            "left",
            vec![
                ("unit", u(unit.0)),
                ("object", u(*object)),
                ("how", s(left_name(*how))),
            ],
        ),
        Entry::StaleDeath { unit, object } => k(
            "stale_death",
            vec![("unit", u(unit.0)), ("object", u(*object))],
        ),
        Entry::ClassifierFault { unit, object } => k(
            "classifier_fault",
            vec![("unit", u(unit.0)), ("object", u(*object))],
        ),
        Entry::Revived { unit, object } => {
            k("revived", vec![("unit", u(unit.0)), ("object", u(*object))])
        }
        Entry::Removed { entity, cause } => k(
            "removed",
            vec![
                ("entity", u(entity.0)),
                (
                    "cause",
                    s(match cause {
                        RemoveCause::Controller => "controller",
                        RemoveCause::Expired => "expired",
                    }),
                ),
            ],
        ),
        Entry::Purged { entity } => k("purged", vec![("entity", u(entity.0))]),
        Entry::Option { entity, option } => k(
            "option",
            vec![("entity", u(entity.0)), ("option", u(u64::from(*option)))],
        ),
        Entry::AirbaseOwner {
            theater,
            airbase,
            coalition,
            rev,
        } => k(
            "airbase_owner",
            vec![
                ("theater", s(theater)),
                ("airbase", s(airbase)),
                ("coalition", u(u64::from(*coalition))),
                ("rev", u(*rev)),
            ],
        ),
        Entry::Warehouse {
            theater,
            airbase,
            rev,
        } => k(
            "warehouse",
            vec![
                ("theater", s(theater)),
                ("airbase", s(airbase)),
                ("rev", u(*rev)),
            ],
        ),
        Entry::SampleRates(r) => k("sample_rates", vec![("rates", enc_rates(r))]),
        Entry::Loaded(l) => k("loaded", vec![("loaded", enc_loaded(l))]),
        Entry::Epoch(e) => k("epoch", vec![("epoch", u(*e))]),
        Entry::Command(j) => enc_journalled(j),
        Entry::Outcome {
            epoch,
            seq,
            outcome,
        } => k(
            "outcome",
            vec![
                ("epoch", u(*epoch)),
                ("seq", u(*seq)),
                ("outcome", s(outcome_name(*outcome))),
            ],
        ),
    }
}

fn dec_entry(v: &Lv) -> Result<Entry, DiskError> {
    let r = Rd(v);
    let unit = || r.u("unit").map(EntityId);
    let entity = || r.u("entity").map(EntityId);
    Ok(match r.s("kind")?.as_str() {
        "spec" => Entry::Spec {
            entity: entity()?,
            rev: r.u("rev")?,
        },
        "extension_spec" => Entry::ExtensionSpec {
            name: r.s("name")?,
            rev: r.u("rev")?,
        },
        "extension_removed" => Entry::ExtensionRemoved { name: r.s("name")? },
        "order" => Entry::Order {
            entity: entity()?,
            rev: r.u("rev")?,
        },
        "authority" => Entry::Authority {
            entity: entity()?,
            authority: dec_authority(&r.s("authority")?)?,
        },
        "transition" => Entry::Transition {
            entity: entity()?,
            from: dec_state(&r.s("from")?)?,
            to: dec_state(&r.s("to")?)?,
            spec_rev: match r.list("spec_rev")?.as_slice() {
                [] => None,
                [v] => Some(dec_u(v, "spec_rev")?),
                _ => return Err(DiskError::Corrupt("spec_rev".into())),
            },
        },
        "captured" => Entry::Captured { entity: entity()? },
        "death" => Entry::Death { unit: unit()? },
        "resolve" => Entry::Resolve {
            unit: unit()?,
            dead: r.b("dead")?,
        },
        "left" => Entry::Left {
            unit: unit()?,
            object: r.u("object")?,
            how: dec_left(&r.s("how")?)?,
        },
        "stale_death" => Entry::StaleDeath {
            unit: unit()?,
            object: r.u("object")?,
        },
        "classifier_fault" => Entry::ClassifierFault {
            unit: unit()?,
            object: r.u("object")?,
        },
        "revived" => Entry::Revived {
            unit: unit()?,
            object: r.u("object")?,
        },
        "removed" => Entry::Removed {
            entity: entity()?,
            // A body written without a cause is the controller's.
            cause: match r.get("cause") {
                Lv::Nil => RemoveCause::Controller,
                _ => match r.s("cause")?.as_str() {
                    "controller" => RemoveCause::Controller,
                    "expired" => RemoveCause::Expired,
                    c => return Err(DiskError::Corrupt(format!("removal cause {c}"))),
                },
            },
        },
        "purged" => Entry::Purged { entity: entity()? },
        "option" => Entry::Option {
            entity: entity()?,
            option: r.u32("option")?,
        },
        "airbase_owner" => Entry::AirbaseOwner {
            theater: r.s("theater")?,
            airbase: r.s("airbase")?,
            coalition: r.u32("coalition")?,
            rev: r.u("rev")?,
        },
        "warehouse" => Entry::Warehouse {
            theater: r.s("theater")?,
            airbase: r.s("airbase")?,
            rev: r.u("rev")?,
        },
        "sample_rates" => Entry::SampleRates(dec_rates(r.get("rates"))?),
        "loaded" => Entry::Loaded(dec_loaded(r.get("loaded"))?),
        "epoch" => Entry::Epoch(r.u("epoch")?),
        "command" => Entry::Command(dec_journalled(&r)?),
        "outcome" => Entry::Outcome {
            epoch: r.u("epoch")?,
            seq: r.u("seq")?,
            outcome: dec_outcome(&r.s("outcome")?)?,
        },
        k => return Err(DiskError::Corrupt(format!("journal entry kind {k}"))),
    })
}

fn enc_record(v: &CampaignRecord) -> Lv {
    let CampaignRecord {
        name,
        state,
        theaters,
        theater,
        clock_ms,
        created_ms,
        component_schemas,
        outcome,
    } = v;
    t(vec![
        ("name", s(name)),
        ("state", s(campaign_state_name(*state))),
        ("theaters", list(theaters.iter().map(|x| s(x)).collect())),
        ("theater", s(theater)),
        (
            "clock_ms",
            clock_ms.map_or(Lv::Nil, |c| list(vec![s(&c.to_string())])),
        ),
        ("created_ms", u(*created_ms)),
        (
            "component_schemas",
            Lv::Table(
                component_schemas
                    .iter()
                    .map(|(k, v)| (s(k), Lv::Number(f64::from(*v))))
                    .collect(),
            ),
        ),
        (
            "outcome",
            outcome
                .as_ref()
                .map_or(Lv::Nil, |o| list(vec![Lv::String(o.clone())])),
        ),
    ])
}

fn dec_record(v: &Lv) -> Result<CampaignRecord, DiskError> {
    let r = Rd(v);
    let theaters = r
        .list("theaters")?
        .into_iter()
        .map(|x| Rd(&t(vec![("x", x.clone())])).s("x"))
        .collect::<Result<_, _>>()?;
    let clock_ms = match r.list("clock_ms")?.as_slice() {
        [] => None,
        [c] => Some(
            Rd(&t(vec![("c", (*c).clone())]))
                .s("c")?
                .parse()
                .map_err(|_| DiskError::Corrupt("clock".into()))?,
        ),
        _ => return Err(DiskError::Corrupt("clock".into())),
    };
    let mut component_schemas = BTreeMap::new();
    if let Lv::Table(pairs) = r.get("component_schemas") {
        for (k, v) in pairs {
            match (k, v) {
                (Lv::String(k), Lv::Number(n)) => {
                    let k = String::from_utf8(k.clone())
                        .map_err(|_| DiskError::Corrupt("component schema".into()))?;
                    component_schemas.insert(k, *n as u32);
                }
                _ => return Err(DiskError::Corrupt("component schema".into())),
            }
        }
    }
    let outcome = match r.list("outcome")?.as_slice() {
        [] => None,
        [Lv::String(o)] => Some(o.clone()),
        _ => return Err(DiskError::Corrupt("outcome".into())),
    };
    Ok(CampaignRecord {
        name: r.s("name")?,
        state: dec_campaign_state(&r.s("state")?)?,
        theaters,
        theater: r.s("theater")?,
        clock_ms,
        created_ms: r.u("created_ms")?,
        component_schemas,
        outcome,
    })
}

fn enc_checkpoint(v: &Checkpoint) -> Lv {
    let Checkpoint {
        seq,
        at_ms,
        revision,
        model_time,
        mission_gen,
        achieved,
    } = v;
    t(vec![
        ("seq", u(*seq)),
        ("at_ms", u(*at_ms)),
        ("revision", u(*revision)),
        ("model_time", f(*model_time)),
        ("mission_gen", u(u64::from(*mission_gen))),
        (
            "achieved",
            list(
                achieved
                    .iter()
                    .map(|p| {
                        let TierPeriod {
                            tier,
                            target_s,
                            achieved_s,
                        } = p;
                        t(vec![
                            ("tier", s(tier)),
                            ("target_s", Lv::Number(*target_s)),
                            ("achieved_s", Lv::Number(*achieved_s)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn dec_checkpoint(v: &Lv) -> Result<Checkpoint, DiskError> {
    let r = Rd(v);
    Ok(Checkpoint {
        seq: r.u("seq")?,
        at_ms: r.u("at_ms")?,
        revision: r.u("revision")?,
        model_time: r.f("model_time")?,
        mission_gen: r.u32("mission_gen")?,
        achieved: r
            .list("achieved")?
            .into_iter()
            .map(|p| {
                let p = Rd(p);
                Ok(TierPeriod {
                    tier: p.s("tier")?,
                    target_s: p.f("target_s")?,
                    achieved_s: p.f("achieved_s")?,
                })
            })
            .collect::<Result<_, DiskError>>()?,
    })
}

fn enc_rates(v: &Rates) -> Lv {
    let Rates {
        position_s,
        vitals_s,
        warehouse_s,
        enumeration_s,
    } = v;
    t(vec![
        ("position_s", Lv::Number(*position_s)),
        ("vitals_s", Lv::Number(*vitals_s)),
        ("warehouse_s", Lv::Number(*warehouse_s)),
        ("enumeration_s", Lv::Number(*enumeration_s)),
    ])
}

fn dec_rates(v: &Lv) -> Result<Rates, DiskError> {
    let r = Rd(v);
    Ok(Rates {
        position_s: r.f("position_s")?,
        vitals_s: r.f("vitals_s")?,
        warehouse_s: r.f("warehouse_s")?,
        enumeration_s: r.f("enumeration_s")?,
    })
}

fn enc_countries(v: &BTreeMap<u32, u32>) -> Lv {
    list(
        v.iter()
            .map(|(c, k)| list(vec![s(&c.to_string()), s(&k.to_string())]))
            .collect(),
    )
}

fn dec_countries(v: &Lv) -> Result<BTreeMap<u32, u32>, DiskError> {
    let mut out = BTreeMap::new();
    for p in Rd(&t(vec![("v", v.clone())])).list("v")? {
        match Rd(&t(vec![("p", p.clone())])).list("p")?.as_slice() {
            [c, k] => {
                let n = |x: &Lv| {
                    u32::try_from(dec_u(x, "country")?)
                        .map_err(|_| DiskError::Corrupt("country".into()))
                };
                out.insert(n(c)?, n(k)?);
            }
            _ => return Err(DiskError::Corrupt("country".into())),
        }
    }
    Ok(out)
}

fn enc_owners(v: &BTreeMap<(String, String), DesiredOwner>) -> Lv {
    list(
        v.iter()
            .map(|((theater, airbase), o)| {
                let DesiredOwner { coalition, rev } = o;
                t(vec![
                    ("theater", s(theater)),
                    ("airbase", s(airbase)),
                    ("coalition", u(u64::from(*coalition))),
                    ("rev", u(*rev)),
                ])
            })
            .collect(),
    )
}

fn dec_owners(v: &Lv) -> Result<BTreeMap<(String, String), DesiredOwner>, DiskError> {
    let mut out = BTreeMap::new();
    for o in Rd(&t(vec![("v", v.clone())])).list("v")? {
        let r = Rd(o);
        out.insert(
            (r.s("theater")?, r.s("airbase")?),
            DesiredOwner {
                coalition: r.u32("coalition")?,
                rev: r.u("rev")?,
            },
        );
    }
    Ok(out)
}

fn enc_stocks(v: &BTreeMap<(String, String), DesiredStock>) -> Lv {
    list(
        v.iter()
            .map(|((theater, airbase), d)| {
                let DesiredStock { stock, rev } = d;
                t(vec![
                    ("theater", s(theater)),
                    ("airbase", s(airbase)),
                    ("stock", nested(&stock.to_lv())),
                    ("rev", u(*rev)),
                ])
            })
            .collect(),
    )
}

fn dec_stocks(v: &Lv) -> Result<BTreeMap<(String, String), DesiredStock>, DiskError> {
    let mut out = BTreeMap::new();
    for o in Rd(&t(vec![("v", v.clone())])).list("v")? {
        let r = Rd(o);
        out.insert(
            (r.s("theater")?, r.s("airbase")?),
            DesiredStock {
                stock: Stock::from_lv(&r.nested("stock")?),
                rev: r.u("rev")?,
            },
        );
    }
    Ok(out)
}

fn enc_route_event(v: &RouteEvent) -> Lv {
    let RouteEvent {
        group,
        order_rev,
        index,
        model_time,
        observed,
    } = v;
    t(vec![
        ("group", u(group.0)),
        ("order_rev", u(*order_rev)),
        (
            "index",
            index.map_or(Lv::Nil, |i| list(vec![s(&i.to_string())])),
        ),
        ("model_time", f(*model_time)),
        ("observed", b(*observed)),
    ])
}

fn dec_route_event(v: &Lv) -> Result<RouteEvent, DiskError> {
    let r = Rd(v);
    Ok(RouteEvent {
        group: EntityId(r.u("group")?),
        order_rev: r.u("order_rev")?,
        index: match r.list("index")?.as_slice() {
            [] => None,
            [i] => Some(
                usize::try_from(dec_u(i, "index")?)
                    .map_err(|_| DiskError::Corrupt("index".into()))?,
            ),
            _ => return Err(DiskError::Corrupt("index".into())),
        },
        model_time: r.f("model_time")?,
        observed: r.b("observed")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The golden file of the store's row encodings (spec/persistence.md,
    /// "Encodings"): `<name> <hex>` per line. Set `NANOPROBE_BLESS=1` to
    /// write it after an intended change, with a new schema version.
    const GOLDEN: &str = include_str!("../../../../spec/persistence.golden");

    fn golden_rows() -> Vec<(String, Vec<u8>)> {
        let mut s = Store::new(CampaignId(7), 10);
        let g = s
            .create_group(super::super::NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: vec![super::super::NewUnit::new(
                    "M-113",
                    UnitState {
                        at: Point {
                            x: 1.5,
                            y: 0.0,
                            z: -2.25,
                        },
                        heading: 0.5,
                        life: 1.0,
                    },
                )],
                authority: Authority::Sim,
                label: "g".into(),
                extra: Lv::Table(vec![(Lv::String(b"k".to_vec()), Lv::Number(1.0))]),
            })
            .expect("group");
        let m = s.group(g).expect("g").members[0];
        let command = Entry::Command(Journalled {
            agent_boot: 11,
            epoch: 3,
            seq: 4,
            opcode: 16,
            entity: g,
            rev: 1,
            mission_gen: 2,
            position: 64,
            units: vec![m],
        });
        let checkpoint = Checkpoint {
            seq: 1,
            at_ms: 1_700_000_000_000,
            revision: 9,
            model_time: 12.5,
            mission_gen: 2,
            achieved: vec![TierPeriod {
                tier: "position".into(),
                target_s: 1.0,
                achieved_s: 1.25,
            }],
        };
        let mut rows = vec![
            (
                "group".to_owned(),
                encode(&enc_group(s.group(g).expect("g"))),
            ),
            ("unit".to_owned(), encode(&enc_unit(s.unit(m).expect("u")))),
            ("entry.command".to_owned(), encode(&enc_entry(&command))),
            (
                "entry.death".to_owned(),
                encode(&enc_entry(&Entry::Death { unit: m })),
            ),
            (
                "checkpoint".to_owned(),
                encode(&enc_checkpoint(&checkpoint)),
            ),
            ("record".to_owned(), encode(&enc_record(&record()))),
            (
                "world.owed_load".to_owned(),
                encode(&enc_owed_load(&OwedLoad {
                    path: "Z:\\m\\shells\\9.a.miz".into(),
                    cmd: Some((3, 7)),
                    start_ms: Some(1_700_000_000_000),
                })),
            ),
            (
                "entry.resolve".to_owned(),
                encode(&enc_entry(&Entry::Resolve {
                    unit: EntityId(2),
                    dead: true,
                })),
            ),
            // A dead group the expiry removed (spec/world.md, "Expiry").
            (
                "group.expired".to_owned(),
                encode(&enc_group(&Group {
                    state: State::Dead,
                    removed: true,
                    removed_at_ms: Some(1_700_086_400_000),
                    expired: true,
                    dead_at_ms: Some(1_700_000_000_000),
                    ..s.group(g).expect("g").clone()
                })),
            ),
            (
                "entry.removed.expired".to_owned(),
                encode(&enc_entry(&Entry::Removed {
                    entity: g,
                    cause: RemoveCause::Expired,
                })),
            ),
            (
                "entry.purged".to_owned(),
                encode(&enc_entry(&Entry::Purged { entity: g })),
            ),
            // The S19 keys of `meta.world`, as the store writes them.
            (
                "world.s19_keys".to_owned(),
                encode(&t(enc_world_mission(
                    Some(-5),
                    &Some(t(vec![
                        ("theater", super::s("Caucasus")),
                        ("start_ms", super::s("1700000000000")),
                        (
                            "weather",
                            Lv::String(encode(&t(vec![("k", Lv::Bool(true))]))),
                        ),
                        (
                            "dynamic",
                            list(vec![t(vec![
                                ("airbase", super::s("12")),
                                ("enabled", Lv::Bool(true)),
                                ("hot_start", Lv::Bool(false)),
                            ])]),
                        ),
                    ])),
                    Some((11, 2, 1.5)),
                    Some(b'b'),
                ))),
            ),
        ];
        for (i, e) in s.entries().iter().enumerate() {
            rows.push((format!("entry.created.{i}"), encode(&enc_entry(e))));
        }
        rows
    }

    /// A SQLite failure names its extended code, which its message does
    /// not: "disk I/O error" is every `SQLITE_IOERR_*`.
    #[test]
    fn a_sqlite_failure_names_its_extended_code() {
        let e = DiskError::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR_GETTEMPPATH),
            Some("disk I/O error".into()),
        ));
        assert_eq!(
            e.to_string(),
            "store: disk I/O error (SQLITE_IOERR_GETTEMPPATH, code 6410)"
        );
        assert_eq!(sqlite_code_name(5), Some("SQLITE_BUSY"));
        assert_eq!(sqlite_code_name(9_999), None);
    }

    /// A unit's asked spawn life reads back; a row without it reads None.
    #[test]
    fn a_units_asked_life_round_trips_and_is_absent_by_default() {
        let mut s = Store::new(CampaignId(7), 10);
        let g = s
            .create_group(super::super::NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: vec![super::super::NewUnit::new("M-113", UnitState::default())],
                authority: Authority::Sim,
                label: "g".into(),
                extra: Lv::Nil,
            })
            .expect("group");
        let u = s.group(g).expect("g").members[0];
        let plain = s.unit(u).expect("u").clone();
        let back = dec_unit(&decode(&encode(&enc_unit(&plain))).expect("decode")).expect("unit");
        assert_eq!(back.captured.life_percent, None);
        s.set_spawn_life(u, 42.5).expect("set");
        let asked = s.unit(u).expect("u").clone();
        let back = dec_unit(&decode(&encode(&enc_unit(&asked))).expect("decode")).expect("unit");
        assert_eq!(back, asked);
        assert_eq!(back.captured.life_percent, Some(42.5));
    }

    /// A dead static's ruin marks read back; a row without them, as an older
    /// store has, reads None, and a dead static stays in the grid.
    #[test]
    fn a_ruins_marks_round_trip_and_are_absent_by_default() {
        let mut s = Store::new(CampaignId(7), 10);
        let spec = |x: f64| super::super::NewGroup {
            theater: "Caucasus".into(),
            country: 2,
            category: 0,
            coalition: 2,
            units: vec![super::super::NewUnit::new(
                "Tent",
                UnitState {
                    at: Point { x, y: 0.0, z: 0.0 },
                    ..UnitState::default()
                },
            )],
            authority: Authority::Sim,
            label: "s".into(),
            extra: Lv::Nil,
        };
        let live = s.create_static(spec(10.0)).expect("static");
        let plain = s.group(live).expect("g").clone();
        let back = dec_group(&decode(&encode(&enc_group(&plain))).expect("decode")).expect("group");
        assert_eq!((back.ruin_sent_in, back.ruin_in), (None, None));
        let id = s.create_ruin(spec(20.0)).expect("ruin");
        assert!(s.is_ruin(id) && !s.is_ruin(live));
        assert_eq!(s.grid().within("Caucasus", 20.0, 0.0, 1.0), vec![id]);
        s.ruin_sent(id, 11, 2).expect("sent");
        s.ruin_held(id, 11, 3).expect("held");
        let marked = s.group(id).expect("g").clone();
        let back =
            dec_group(&decode(&encode(&enc_group(&marked))).expect("decode")).expect("group");
        assert_eq!(back, marked);
        assert_eq!(
            (back.ruin_sent_in, back.ruin_in),
            (Some((11, 2)), Some((11, 3)))
        );
        s.ruin_gone(id).expect("gone");
        let g = s.group(id).expect("g");
        assert_eq!((g.ruin_sent_in, g.ruin_in), (None, None));
        // The removal ends it: no ruin is kept for a removed static.
        s.remove_entity(id).expect("remove");
        assert!(!s.is_ruin(id));
    }

    #[test]
    fn store_rows_encode_as_the_golden_file_says() {
        let text: String = golden_rows()
            .iter()
            .map(|(name, b)| {
                let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
                format!("store.{name} {hex}\n")
            })
            .collect();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/persistence.golden");
        if std::env::var_os("NANOPROBE_BLESS").is_some() {
            let others: String = GOLDEN
                .lines()
                .filter(|l| !l.starts_with("store."))
                .map(|l| format!("{l}\n"))
                .collect();
            std::fs::write(path, others + &text).expect("bless");
            return;
        }
        let have: String = GOLDEN
            .lines()
            .filter(|l| l.starts_with("store."))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(
            have, text,
            "the store's encoding changed; see spec/persistence.md"
        );
        // Every row decodes back.
        for (name, b) in golden_rows() {
            decode(&b).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn a_value_too_deep_or_too_wide_is_refused_not_written_as_nil() {
        let mut deep = Lv::Number(1.0);
        for _ in 0..nanoprobe_transport::codec::MAX_DEPTH {
            deep = Lv::Table(vec![(Lv::Number(1.0), deep)]);
        }
        assert!(check_lv(&deep).is_ok(), "the codec's own depth fits");
        deep = Lv::Table(vec![(Lv::Number(1.0), deep)]);
        assert!(check_lv(&deep).is_err());
        let d = tempfile::tempdir().expect("dir");
        let mut s = Store::new(CampaignId(7), 10);
        s.save_as(&d.path().join("store.sqlite"), record())
            .expect("save");
        let e = s
            .create_group(super::super::NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: vec![super::super::NewUnit::new("M-113", UnitState::default())],
                authority: Authority::Sim,
                label: String::new(),
                extra: deep,
            })
            .expect_err("too deep");
        assert!(matches!(e, super::super::StoreError::TooLarge(_)));
    }

    fn record() -> CampaignRecord {
        CampaignRecord {
            name: "a".into(),
            state: CampaignState::Active,
            theaters: vec!["Caucasus".into()],
            theater: "Caucasus".into(),
            clock_ms: None,
            created_ms: 1,
            component_schemas: BTreeMap::new(),
            outcome: None,
        }
    }

    /// A store with one group of two units, written to `path`.
    fn written(path: &Path) {
        let mut s = Store::new(CampaignId(7), 10);
        s.create_group(super::super::NewGroup {
            theater: "Caucasus".into(),
            country: 2,
            category: 2,
            coalition: 2,
            units: vec![
                super::super::NewUnit::new("M-113", UnitState::default()),
                super::super::NewUnit::new("M-113", UnitState::default()),
            ],
            authority: Authority::Sim,
            label: String::new(),
            extra: Lv::Nil,
        })
        .expect("group");
        s.save_as(path, record()).expect("save");
    }

    fn damaged(sql: &str) -> DiskError {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        written(&p);
        Connection::open(&p)
            .expect("conn")
            .execute_batch(sql)
            .expect("damage");
        Store::open(&p, 10).expect_err("a damaged store opens")
    }

    #[test]
    fn a_damaged_store_is_refused_as_corrupt_by_the_check_it_breaks() {
        for (sql, why) in [
            ("DELETE FROM meta WHERE key = 'world';", "no meta world"),
            (
                "DELETE FROM entity WHERE id = 2 AND kind = 'unit';",
                "names member 2",
            ),
            (
                "DELETE FROM entity WHERE kind = 'group';",
                "of a group not in the store",
            ),
            ("UPDATE entity SET kind = 'tank' WHERE id = 2;", "kind tank"),
            (
                "UPDATE entity SET id = 9 WHERE id = 3;",
                "unit row 9 holds 3",
            ),
            (
                "INSERT INTO inflight (epoch, seq, body) \
                 SELECT 1, seq, body FROM journal WHERE kind = 'spec' LIMIT 1;",
                "inflight row holds",
            ),
        ] {
            match damaged(sql) {
                DiskError::Corrupt(m) => assert!(m.contains(why), "{sql}: {m}"),
                e => panic!("{sql}: {e:?}"),
            }
        }
        // A next EntityId at or below an entity in the store.
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        written(&p);
        let mut s = Store::open(&p, 10).expect("open");
        s.next_entity = 2;
        s.commit().expect("commit");
        drop(s);
        match Store::open(&p, 10) {
            Err(DiskError::Corrupt(m)) => assert!(m.contains("next EntityId"), "{m}"),
            r => panic!("{:?}", r.map(|_| ())),
        }
    }

    #[test]
    fn a_removed_extension_stays_removed_and_keeps_its_revision_floor() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = Store::new(CampaignId(7), 10);
        s.set_extension("keep", "return 0");
        s.set_extension("gone", "return 1");
        assert_eq!(s.set_extension("gone", "return 2"), 2);
        s.save_as(&p, record()).expect("save");
        assert!(s.remove_extension("gone"));
        s.commit().expect("commit");
        drop(s);
        let mut s = Store::open(&p, 10).expect("open");
        assert!(s.extension("gone").is_none());
        assert!(s.extension("keep").is_some());
        assert_eq!(s.set_extension("gone", "return 3"), 3);
        s.commit().expect("commit");
        drop(s);
        let s = Store::open(&p, 10).expect("open");
        assert_eq!(s.extension("gone").map(|e| e.spec_rev), Some(3));
    }

    #[test]
    fn the_last_migration_step_is_this_schema() {
        assert_eq!(MIGRATIONS.last().map_or(SCHEMA, |m| m.to), SCHEMA);
    }

    #[test]
    fn the_record_reads_without_opening_the_store() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        written(&p);
        let (id, schema, r) = record_of(&p).expect("record");
        assert_eq!((id, schema), (CampaignId(7), SCHEMA));
        assert_eq!(r, record());
    }

    #[test]
    fn templates_next_mission_and_start_survive_a_reopen() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = Store::create_on_disk(&p, CampaignId(7), record(), 10).expect("create");
        assert_eq!(template_of(&p, "Caucasus").expect("read"), None);
        s.set_template("Caucasus", Some(b"miz".to_vec()));
        s.set_template("Syria", Some(b"other".to_vec()));
        let next = Lv::Table(vec![(
            Lv::String(b"theater".to_vec()),
            Lv::String(b"Syria".to_vec()),
        )]);
        s.set_next_mission(Some(next.clone()));
        s.set_mission_start_ms(Some(-5));
        s.commit().expect("commit");
        s.set_template("Syria", None);
        s.commit().expect("commit");
        assert_eq!(
            template_of(&p, "Caucasus").expect("read"),
            Some(b"miz".to_vec())
        );
        assert_eq!(template_of(&p, "Syria").expect("read"), None);
        drop(s);
        let s = Store::open(&p, 10).expect("open");
        assert_eq!(s.template("Caucasus"), Some(&b"miz"[..]));
        assert_eq!(s.template_theaters().collect::<Vec<_>>(), ["Caucasus"]);
        assert_eq!(s.next_mission(), Some(&next));
        assert_eq!(s.mission_start_ms(), Some(-5));
    }

    #[test]
    fn a_store_without_the_template_table_opens_and_gains_it() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        drop(Store::create_on_disk(&p, CampaignId(7), record(), 10).expect("create"));
        Connection::open(&p)
            .expect("conn")
            .execute_batch("DROP TABLE template")
            .expect("drop");
        assert_eq!(template_of(&p, "Caucasus").expect("no table"), None);
        let mut s = Store::open(&p, 10).expect("open");
        s.set_template("Caucasus", Some(b"miz".to_vec()));
        s.commit().expect("commit");
        assert_eq!(
            template_of(&p, "Caucasus").expect("read"),
            Some(b"miz".to_vec())
        );
    }

    fn command(seq: u64) -> Entry {
        Entry::Command(Journalled {
            agent_boot: 1,
            epoch: 1,
            seq,
            opcode: 16,
            entity: EntityId(1),
            rev: 1,
            mission_gen: 1,
            position: seq,
            units: vec![],
        })
    }

    /// A store on disk with `n` journal rows, row `i` stamped `i * 1000`.
    fn journalled(p: &Path, n: u64) -> Store {
        let mut s = Store::create_on_disk(p, CampaignId(7), record(), 10).expect("create");
        for i in 1..=n {
            s.journal(Entry::Spec {
                entity: EntityId(i),
                rev: i,
            });
        }
        s.commit().expect("commit");
        stamp(p);
        s
    }

    /// Give each journal row the time `seq * 1000`.
    fn stamp(p: &Path) {
        Connection::open(p)
            .expect("conn")
            .execute_batch("UPDATE journal SET at_ms = seq * 1000")
            .expect("stamp");
    }

    fn journal_seqs(p: &Path) -> Vec<i64> {
        let conn = Connection::open(p).expect("conn");
        let mut st = conn
            .prepare("SELECT seq FROM journal ORDER BY seq")
            .expect("prepare");
        st.query_map([], |r| r.get(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    }

    #[test]
    fn journal_retention_zero_keeps_every_row() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 5);
        assert_eq!(
            s.prune_journal(0, u64::MAX, 1_000, 0).expect("prune"),
            JournalPrune::Kept
        );
        assert_eq!(journal_seqs(&p), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn journal_rows_older_than_the_retention_go_and_the_newest_stays() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 5);
        // Cutoff 5000 - 2500: rows at 1000 and 2000 go.
        assert_eq!(
            s.prune_journal(2_500, 1_000_000, 1_000, 0).expect("prune"),
            JournalPrune::Deleted {
                rows: 2,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [3, 4, 5]);
        // Every row old: the newest still stays.
        assert_eq!(
            s.prune_journal(1, 1_000_000_000, 1_000, 0).expect("prune"),
            JournalPrune::Deleted {
                rows: 2,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [5]);
        assert_eq!(
            s.prune_journal(1, 1_000_000_000, 1_000, 0).expect("prune"),
            JournalPrune::Kept
        );
        assert_eq!(journal_seqs(&p), [5]);
    }

    #[test]
    fn journal_seq_goes_on_from_the_newest_row_after_a_prune_and_reopen() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 5);
        s.prune_journal(1, 1_000_000, 1_000, 0).expect("prune");
        drop(s);
        let mut s = Store::open(&p, 10).expect("open");
        s.journal(Entry::Spec {
            entity: EntityId(9),
            rev: 9,
        });
        s.commit().expect("commit");
        assert_eq!(journal_seqs(&p), [5, 6]);
    }

    #[test]
    fn a_journal_backlog_drains_in_batches_with_commits_between() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 10);
        let mut gone = Vec::new();
        for round in 0..6u64 {
            let next = 11 + round;
            // A commit of new entries between two batches.
            s.journal(Entry::Spec {
                entity: EntityId(next),
                rev: next,
            });
            s.commit().expect("commit");
            let r = s.prune_journal(1, u64::MAX, 4, 0).expect("prune");
            let JournalPrune::Deleted { rows, more } = r else {
                gone.push((round, 0, false));
                continue;
            };
            assert!(rows <= 4);
            gone.push((round, rows, more));
        }
        // The first two rounds are full and report more; a later one ends.
        assert_eq!((gone[0].1, gone[0].2), (4, true));
        assert_eq!((gone[1].1, gone[1].2), (4, true));
        assert!(gone.iter().any(|g| !g.2), "{gone:?}");
        // The new entries (stamped by the commit's clock, not `seq * 1000`)
        // kept their numbers.
        let seqs = journal_seqs(&p);
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
        assert_eq!(*seqs.last().expect("rows"), 16);
    }

    #[test]
    fn an_entry_out_of_order_ends_the_prefix() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 6);
        // Row 3 is inside the retention (a clock that stepped back).
        Connection::open(&p)
            .expect("conn")
            .execute_batch("UPDATE journal SET at_ms = 5500 WHERE seq = 3")
            .expect("stamp");
        // Cutoff 6000 - 1500 = 4500: rows 1 and 2 go; row 3 stops the
        // prefix, though rows 4 and later are older than row 3.
        assert_eq!(
            s.prune_journal(1_500, 1_000_000, 1_000, 0).expect("prune"),
            JournalPrune::Deleted {
                rows: 2,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [3, 4, 5, 6]);
        // An oldest entry inside the retention ends it with nothing deleted.
        assert_eq!(
            s.prune_journal(1_500, 1_000_000, 1_000, 0).expect("prune"),
            JournalPrune::Kept
        );
        assert_eq!(journal_seqs(&p), [3, 4, 5, 6]);
    }

    /// A group with a lead sample, on disk, with a checkpoint written.
    fn with_last_lead(p: &Path) -> (Store, EntityId) {
        let mut s = Store::create_on_disk(p, CampaignId(7), record(), 10).expect("create");
        let g = s
            .create_group(super::super::NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: vec![super::super::NewUnit::new("M-113", UnitState::default())],
                authority: Authority::Sim,
                label: String::new(),
                extra: Lv::Nil,
            })
            .expect("group");
        s.set_order(
            g,
            crate::intents::Intent::Route {
                waypoints: vec![crate::intents::Waypoint { x: 1_000.0, z: 0.0 }],
                speed: 5.0,
                on_road: false,
            },
        )
        .expect("order");
        let lead = s.group(g).expect("g").members[0];
        s.set_sample_time(10.0);
        s.observe_position(
            lead,
            Point {
                x: 500.0,
                y: 0.0,
                z: 0.0,
            },
        );
        assert!(s.group(g).expect("g").last_lead.is_some());
        s.commit().expect("commit");
        s.checkpoint(1_000, Vec::new()).expect("checkpoint");
        (s, g)
    }

    /// Break the samples, optionally move the revision, checkpoint, reopen:
    /// the lead sample the file holds.
    fn lead_after_break(bump_revision: bool) -> Option<(EntityId, f64, f64, f64)> {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let (mut s, g) = with_last_lead(&p);
        s.break_samples();
        assert!(s.group(g).expect("g").last_lead.is_none());
        if bump_revision {
            // Another group changes, so only the break marks `g`.
            let other = s
                .create_group(super::super::NewGroup {
                    theater: "Caucasus".into(),
                    country: 2,
                    category: 2,
                    coalition: 2,
                    units: vec![super::super::NewUnit::new("M-113", UnitState::default())],
                    authority: Authority::Sim,
                    label: String::new(),
                    extra: Lv::Nil,
                })
                .expect("group");
            s.set_label(other, "moved").expect("label");
        }
        // The checkpoint compares every marked row, `last_lead` too; in a
        // debug build it asserts that no change went unmarked.
        s.checkpoint(2_000, Vec::new()).expect("checkpoint");
        s.close().expect("close");
        let r = Store::open(&p, 10).expect("reopen");
        r.group(g).expect("g").last_lead
    }

    #[test]
    fn breaking_the_samples_is_written_at_the_next_checkpoint() {
        assert!(lead_after_break(true).is_none(), "cleared on disk");
    }

    /// `break_samples` moves the revision, so a checkpoint with nothing else
    /// changed still writes the cleared `last_lead`.
    #[test]
    fn breaking_the_samples_alone_is_written_at_the_next_checkpoint() {
        assert!(lead_after_break(false).is_none(), "cleared on disk");
    }

    #[test]
    fn a_clock_behind_the_newest_journal_row_deletes_nothing() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 5);
        assert_eq!(
            s.prune_journal(1, 2_500, 1_000, 0).expect("prune"),
            JournalPrune::ClockBehind {
                now_ms: 2_500,
                newest_ms: 5_000
            }
        );
        assert_eq!(journal_seqs(&p), [1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_store_in_memory_has_no_journal_to_prune() {
        let mut s = Store::new(CampaignId(7), 10);
        s.journal(command(1));
        assert_eq!(
            s.prune_journal(1, u64::MAX, 1_000, 0).expect("prune"),
            JournalPrune::Kept
        );
        assert_eq!(s.entries().len(), 1);
    }

    #[test]
    fn an_unanswered_command_whose_entry_was_deleted_is_still_owed_and_resolves() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = Store::create_on_disk(&p, CampaignId(7), record(), 10).expect("create");
        s.journal(command(1));
        s.journal(command(2));
        s.journal(Entry::Outcome {
            epoch: 1,
            seq: 2,
            outcome: Outcome::Applied,
        });
        s.journal(Entry::Spec {
            entity: EntityId(1),
            rev: 1,
        });
        s.commit().expect("commit");
        stamp(&p);
        assert_eq!(
            s.prune_journal(1, 1_000_000, 1_000, 0).expect("prune"),
            JournalPrune::Deleted {
                rows: 3,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [4]);
        let inflight = || -> i64 {
            Connection::open(&p)
                .expect("conn")
                .query_row("SELECT count(*) FROM inflight", [], |r| r.get(0))
                .expect("count")
        };
        assert_eq!(inflight(), 1);
        drop(s);
        let mut s = Store::open(&p, 10).expect("open");
        assert_eq!(s.unanswered().len(), 1);
        assert_eq!(s.unanswered()[0].seq, 1);
        // The restarted reconciler's outcome resolves it.
        s.journal(Entry::Outcome {
            epoch: 1,
            seq: 1,
            outcome: Outcome::Applied,
        });
        s.commit().expect("commit");
        assert_eq!(inflight(), 0);
        assert!(s.unanswered().is_empty());
        drop(s);
        assert!(Store::open(&p, 10).expect("open").unanswered().is_empty());
    }

    #[test]
    fn a_backup_taken_after_a_delete_holds_no_deleted_row() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 5);
        s.prune_journal(1_500, 1_000_000, 1_000, 0).expect("prune");
        let dest = d.path().join("backup.sqlite");
        backup_file(&p, &dest).expect("backup");
        let seqs: Vec<i64> = {
            let conn = Connection::open(&dest).expect("conn");
            let mut st = conn
                .prepare("SELECT seq FROM journal ORDER BY seq")
                .expect("prepare");
            st.query_map([], |r| r.get(0))
                .expect("query")
                .map(|r| r.expect("row"))
                .collect()
        };
        assert_eq!(seqs, [4, 5]);
    }

    /// `(sum(length(body)), meta.journal_bytes)` as the file holds them.
    fn journal_bytes_of(p: &Path) -> (i64, Option<String>) {
        let conn = Connection::open(p).expect("conn");
        let sum = conn
            .query_row(
                "SELECT coalesce(sum(length(body)), 0) FROM journal",
                [],
                |r| r.get(0),
            )
            .expect("sum");
        let key: Option<Vec<u8>> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'journal_bytes'",
                [],
                |r| r.get(0),
            )
            .optional()
            .expect("key");
        (sum, key.map(|k| String::from_utf8(k).expect("text")))
    }

    fn assert_count_matches(p: &Path) -> i64 {
        let (sum, key) = journal_bytes_of(p);
        assert_eq!(key, Some(sum.to_string()));
        sum
    }

    /// The bytes of the bodies of rows `1..=n`.
    fn first_bytes(p: &Path, n: i64) -> i64 {
        Connection::open(p)
            .expect("conn")
            .query_row(
                "SELECT sum(length(body)) FROM journal WHERE seq <= ?1",
                [n],
                |r| r.get(0),
            )
            .expect("sum")
    }

    #[test]
    fn the_journal_byte_count_matches_the_rows_after_commits_prunes_and_a_reopen() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 6);
        assert!(assert_count_matches(&p) > 0);
        s.journal(command(1));
        s.journal(Entry::Spec {
            entity: EntityId(300),
            rev: 300,
        });
        s.commit().expect("commit");
        assert_count_matches(&p);
        s.prune_journal(1_500, 1_000_000, 2, 0).expect("prune");
        assert_count_matches(&p);
        s.prune_journal(0, 0, 100, 40).expect("prune");
        let kept = assert_count_matches(&p);
        assert!(kept > 0);
        drop(s);
        let mut s = Store::open(&p, 10).expect("open");
        assert_count_matches(&p);
        s.journal(command(2));
        s.commit().expect("commit");
        assert_count_matches(&p);
        s.prune_journal(0, 0, 100, 1).expect("prune");
        assert_eq!(journal_seqs(&p).len(), 1);
        assert_count_matches(&p);
    }

    #[test]
    fn a_journal_size_limit_alone_deletes_the_oldest_until_under_it() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 10);
        let total = assert_count_matches(&p);
        let two = first_bytes(&p, 2);
        // Retention off: the size rule deletes rows 1 and 2 and stops.
        assert_eq!(
            s.prune_journal(0, 0, 1_000, (total - two) as u64)
                .expect("prune"),
            JournalPrune::Deleted {
                rows: 2,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [3, 4, 5, 6, 7, 8, 9, 10]);
        assert_count_matches(&p);
        // Under the limit: nothing to do.
        assert_eq!(
            s.prune_journal(0, 0, 1_000, (total - two) as u64)
                .expect("prune"),
            JournalPrune::Kept
        );
        // A limit of 0 is no limit.
        assert_eq!(
            s.prune_journal(0, 0, 1_000, 0).expect("prune"),
            JournalPrune::Kept
        );
    }

    #[test]
    fn a_journal_time_limit_alone_ignores_the_size() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 5);
        // Cutoff 5000 - 2500 = 2500: rows 1 and 2 go.
        assert_eq!(
            s.prune_journal(2_500, 1_000_000, 1_000, 0).expect("prune"),
            JournalPrune::Deleted {
                rows: 2,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [3, 4, 5]);
        assert_count_matches(&p);
    }

    #[test]
    fn both_journal_limits_delete_what_either_takes() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 10);
        let total = assert_count_matches(&p);
        let five = first_bytes(&p, 5);
        // Time takes rows 1 and 2; the size excess takes rows 1 to 5.
        assert_eq!(
            s.prune_journal(7_500, 1_000_000, 1_000, (total - five) as u64)
                .expect("prune"),
            JournalPrune::Deleted {
                rows: 5,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [6, 7, 8, 9, 10]);
        // Time takes rows 6 and 7; the size limit takes nothing more.
        assert_eq!(
            s.prune_journal(2_500, 1_000_000, 1_000, u64::MAX)
                .expect("prune"),
            JournalPrune::Deleted {
                rows: 2,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [8, 9, 10]);
        assert_count_matches(&p);
    }

    #[test]
    fn a_journal_size_limit_keeps_the_newest_row_and_applies_with_the_clock_behind() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 5);
        // Clock behind: the time rule is skipped, the size rule applies.
        assert_eq!(
            s.prune_journal(1, 2_500, 1_000, 1).expect("prune"),
            JournalPrune::Deleted {
                rows: 4,
                more: false
            }
        );
        assert_eq!(journal_seqs(&p), [5]);
        // The newest row stays however far over the limit it is.
        assert_eq!(
            s.prune_journal(0, 0, 1_000, 1).expect("prune"),
            JournalPrune::Kept
        );
        // Clock behind and no size excess: the clock is reported.
        assert!(matches!(
            s.prune_journal(1, 2_500, 1_000, 0).expect("prune"),
            JournalPrune::ClockBehind { .. }
        ));
        assert_count_matches(&p);
    }

    #[test]
    fn a_journal_size_backlog_drains_over_several_passes() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        let mut s = journalled(&p, 10);
        let mut passes = Vec::new();
        loop {
            let r = s.prune_journal(0, 0, 3, 1).expect("prune");
            let JournalPrune::Deleted { rows, more } = r else {
                break;
            };
            passes.push((rows, more));
            assert_count_matches(&p);
            assert!(passes.len() < 10);
        }
        // Nine rows go in batches of 3, 3, 3; the third batch is full and
        // all deleted, so it says more, and the next pass finds only the
        // newest.
        assert_eq!(passes, [(3, true), (3, true), (3, true)]);
        assert_eq!(journal_seqs(&p), [10]);
    }

    #[test]
    fn a_journal_file_without_the_byte_count_gets_it_at_open() {
        let d = tempfile::tempdir().expect("dir");
        let p = d.path().join("store.sqlite");
        drop(journalled(&p, 5));
        let sum = journal_bytes_of(&p).0;
        Connection::open(&p)
            .expect("conn")
            .execute("DELETE FROM meta WHERE key = 'journal_bytes'", [])
            .expect("delete");
        assert_eq!(journal_bytes_of(&p).1, None);
        let mut s = Store::open(&p, 10).expect("open");
        assert_eq!(journal_bytes_of(&p), (sum, Some(sum.to_string())));
        // The count in memory is the computed one: a prune subtracts from it.
        s.prune_journal(0, 0, 1_000, 1).expect("prune");
        assert_count_matches(&p);
    }
}
