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

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use nanoprobe_transport::codec::{Lv, Reader, Writer};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use nanoprobe_transport::records::Rates;

use super::{
    Authority, CapturedMore, DesiredOwner, DesiredStock, Entry, Extension, Group, IdemRow,
    Journalled, Kind, LeftAs, Loaded, LostPolicy, Outcome, Point, RouteEvent, Store, Unit,
    UnitState,
};
use crate::ids::{CampaignId, EntityId};
use crate::intents::{Intent, Waypoint};
use crate::lifecycle::State;
use crate::stock::Stock;

/// The schema version this sidecar writes (spec/persistence.md).
pub const SCHEMA: u32 = 1;

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

impl fmt::Display for DiskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
";

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
        store.save_as(path, record)?;
        Ok(store)
    }

    /// Write this store, as it is, to a new store file at `path` and keep it
    /// linked. A store already on disk is unlinked from its old file. The
    /// file is made under a temporary name in one transaction, flushed and
    /// renamed into place, so a crash leaves no file or a whole one.
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
    /// transaction. Nothing to do for a store in memory.
    pub fn commit(&mut self) -> Result<(), DiskError> {
        let Some(mut d) = self.disk.0.take() else {
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
        self.idempotency.clear();
        if self.keep_journal {
            return;
        }
        if let Some(d) = self.disk.0.as_mut() {
            self.journal.drain(..d.journal_written);
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
            self.drain_journal();
        }
        r
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
            countries,
            sample_time,
            route_events,
            disk: _,
            markers: _,
        } = store;
        let _ = take_overflow();
        let at_ms = now_ms();
        let tx = self.conn.transaction()?;
        if init {
            tx.execute_batch(TABLES)?;
            put_meta(&tx, "schema", &Lv::Number(f64::from(SCHEMA)))?;
            put_meta(&tx, "campaign_id", &u(campaign.0))?;
        }
        let full = cp.is_some();
        let mut changed_groups = Vec::new();
        for (id, g) in groups {
            if self.groups.get(id) != Some(g) {
                put_entity(&tx, *id, "group", &enc_group(g))?;
                changed_groups.push(*id);
            }
        }
        let mut changed_units = Vec::new();
        for (id, un) in units {
            let differs = match self.units.get(id) {
                None => true,
                Some(w) if full => w != un,
                Some(w) => !durable_eq(w, un),
            };
            if differs {
                put_entity(&tx, *id, "unit", &enc_unit(un))?;
                changed_units.push(*id);
            }
        }
        let gone_groups: Vec<EntityId> = self
            .groups
            .keys()
            .filter(|id| !groups.contains_key(id))
            .copied()
            .collect();
        let gone_units: Vec<EntityId> = self
            .units
            .keys()
            .filter(|id| !units.contains_key(id))
            .copied()
            .collect();
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
        for e in journal.iter().skip(self.journal_written) {
            seq += 1;
            let body = enc_entry(e);
            let (kind, entity) = entry_key(&body);
            tx.execute(
                "INSERT INTO journal (seq, at_ms, kind, entity, body) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![seq as i64, at_ms as i64, kind, entity as i64, encode(&body)],
            )?;
            match e {
                Entry::Command(c) => {
                    tx.execute(
                        "INSERT OR REPLACE INTO inflight (epoch, seq, body) VALUES (?1, ?2, ?3)",
                        params![c.epoch as i64, c.seq as i64, encode(&body)],
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
        for r in idempotency {
            tx.execute(
                "INSERT OR REPLACE INTO idempotency (key, at_ms, request_hash, response) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![r.key, r.at_ms as i64, r.request_hash, r.response],
            )?;
        }
        // Sampled state moves only at a checkpoint.
        let sample_time = if full { *sample_time } else { self.sample_time };
        let world = encode(&t(vec![
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
                "countries",
                countries.as_ref().map_or(Lv::Nil, enc_countries),
            ),
            ("sample_time", f(sample_time)),
            (
                "route_events",
                list(route_events.iter().map(enc_route_event).collect()),
            ),
        ]));
        if world != self.world {
            put_meta_raw(&tx, "world", &world)?;
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
        self.journal_written = journal.len();
        self.world = world;
        self.sample_time = sample_time;
        self.record_written = true;
        Ok(())
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
        if !un.dead {
            let g = &groups[&un.group];
            store
                .grid
                .set(un.id, &g.theater, un.state.at.x, un.state.at.z);
        }
    }
    let world = world_bytes;
    store.groups = groups.clone();
    store.units = units.clone();
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

fn get_meta(conn: &Connection, key: &str) -> Result<Lv, DiskError> {
    let v: Option<Vec<u8>> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?;
    decode(&v.ok_or_else(|| DiskError::Corrupt(format!("no meta {key}")))?)
}

// ---- lv helpers ----

/// Whether `v` encodes whole: no table nested deeper than the codec's
/// [`MAX_DEPTH`](nanoprobe_transport::codec::MAX_DEPTH) and none with more
/// pairs than [`MAX_PAIRS`](nanoprobe_transport::codec::MAX_PAIRS). The
/// transport writer writes nil past either; the store refuses instead
/// (spec/persistence.md, "Encodings").
pub fn check_lv(v: &Lv) -> Result<(), String> {
    fn at(v: &Lv, depth: usize) -> Result<(), String> {
        let Lv::Table(pairs) = v else {
            return Ok(());
        };
        if depth >= nanoprobe_transport::codec::MAX_DEPTH {
            return Err(format!(
                "a table nested deeper than {} levels",
                nanoprobe_transport::codec::MAX_DEPTH
            ));
        }
        if pairs.len() > nanoprobe_transport::codec::MAX_PAIRS as usize {
            return Err(format!(
                "a table of more than {} pairs",
                nanoprobe_transport::codec::MAX_PAIRS
            ));
        }
        for (k, v) in pairs {
            at(k, depth + 1)?;
            at(v, depth + 1)?;
        }
        Ok(())
    }
    at(v, 0)
}

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
    let UnitState {
        at,
        heading,
        life,
        fuel,
    } = v;
    t(vec![
        ("at", enc_point(at)),
        ("heading", f(*heading)),
        ("life", f(f64::from(*life))),
        ("fuel", f(f64::from(*fuel))),
    ])
}

fn dec_unit_state(v: &Lv) -> Result<UnitState, DiskError> {
    let r = Rd(v);
    Ok(UnitState {
        at: dec_point(r.get("at"))?,
        heading: r.f("heading")?,
        life: r.f("life")? as f32,
        fuel: r.f("fuel")? as f32,
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
    } = v;
    let CapturedMore {
        life0,
        pitch,
        roll,
        ammo,
    } = captured;
    t(vec![
        ("kind", s("unit")),
        (
            "captured",
            t(vec![
                ("life0", f(*life0)),
                ("pitch", f(*pitch)),
                ("roll", f(*roll)),
                ("ammo", nested(ammo)),
            ]),
        ),
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
                ammo: c.nested("ammo")?,
            }
        },
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
        options,
        last_lead,
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
        Entry::Removed { entity } => k("removed", vec![("entity", u(entity.0))]),
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
        "removed" => Entry::Removed { entity: entity()? },
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
        ammo_s,
        warehouse_s,
        enumeration_s,
    } = v;
    t(vec![
        ("position_s", Lv::Number(*position_s)),
        ("vitals_s", Lv::Number(*vitals_s)),
        ("ammo_s", Lv::Number(*ammo_s)),
        ("warehouse_s", Lv::Number(*warehouse_s)),
        ("enumeration_s", Lv::Number(*enumeration_s)),
    ])
}

fn dec_rates(v: &Lv) -> Result<Rates, DiskError> {
    let r = Rd(v);
    Ok(Rates {
        position_s: r.f("position_s")?,
        vitals_s: r.f("vitals_s")?,
        ammo_s: r.f("ammo_s")?,
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
}
