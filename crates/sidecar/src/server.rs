//! The server store: what outlives a campaign (spec/persistence.md, "The
//! server store"), and the lock on the store dir.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use nanoprobe_transport::codec::{Lv, Reader, Writer};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::campaigns::{BackupFolder, BackupReason};
use crate::store::{DiskError, IdemRow, check_lv};

/// The server folder's name inside `[store] dir`.
pub const FOLDER: &str = "server";

/// The lock file's name inside `[store] dir`.
pub const LOCK: &str = "lock";

/// The file S16 kept the player keyring in, read once into the server store.
pub const PLAYERS_JSON: &str = "players.json";

/// The schema version of the server store.
pub const SCHEMA: u32 = 1;

const STORE: &str = "store.sqlite";

const TABLES: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);
CREATE TABLE player_secret (id INTEGER PRIMARY KEY, secret BLOB NOT NULL);
CREATE TABLE player (key TEXT PRIMARY KEY, ucid TEXT NOT NULL, body BLOB NOT NULL);
CREATE TABLE idempotency (key TEXT PRIMARY KEY, at_ms INTEGER NOT NULL, campaign INTEGER, request_hash BLOB, response BLOB);
CREATE TABLE extension (name TEXT PRIMARY KEY, body BLOB NOT NULL);
CREATE TABLE deleted_campaign (id INTEGER PRIMARY KEY, at_ms INTEGER NOT NULL);
";

/// One migration step of the server store, as a campaign store's.
#[derive(Clone, Copy)]
pub struct Migration {
    /// The version after the step.
    pub to: u32,
    /// The step, with its own frozen reading of the version before.
    pub run: fn(&Transaction<'_>) -> rusqlite::Result<()>,
}

/// The steps of this sidecar, oldest first. Empty while [`SCHEMA`] is 1.
pub const MIGRATIONS: &[Migration] = &[];

/// An exclusive OS lock on `<store dir>/lock`, held while the sidecar runs
/// (spec/persistence.md, "The lock"). It ends with the value or the process.
#[derive(Debug)]
pub struct StoreLock(#[allow(dead_code)] fs::File);

/// Take the store dir's lock without waiting. Another process holding it
/// is an error that names the file.
pub fn lock(dir: &Path) -> io::Result<StoreLock> {
    fs::create_dir_all(dir)?;
    let path = dir.join(LOCK);
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    match f.try_lock() {
        Ok(()) => Ok(StoreLock(f)),
        Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "{} is locked: another sidecar uses this store dir",
                path.display()
            ),
        )),
        Err(fs::TryLockError::Error(e)) => Err(e),
    }
}

/// A key the server store remembers (spec/persistence.md, "Idempotency
/// keys").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerIdem {
    /// When the call was answered, milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// The campaign whose store holds the answer, or `None` for a call on
    /// no campaign.
    pub campaign: Option<u64>,
    /// For a call on no campaign, its digest and response.
    pub answer: Option<(Vec<u8>, Vec<u8>)>,
}

/// The server store, open.
#[derive(Debug)]
pub struct ServerStore {
    dir: PathBuf,
    conn: Connection,
    epoch: u64,
    written_epoch: u64,
}

fn enc(v: &Lv) -> Result<Vec<u8>, DiskError> {
    check_lv(v).map_err(DiskError::TooLarge)?;
    let mut w = Writer::new();
    w.lv(v);
    Ok(w.finish())
}

fn dec(b: &[u8]) -> Result<Lv, DiskError> {
    Reader::new(b)
        .lv()
        .map_err(|e| DiskError::Corrupt(e.to_string()))
}

fn put_meta(conn: &Connection, key: &str, v: &Lv) -> Result<(), DiskError> {
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, enc(v)?],
    )?;
    Ok(())
}

fn get_meta(conn: &Connection, key: &str) -> Result<Option<Lv>, DiskError> {
    let v: Option<Vec<u8>> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    v.map(|b| dec(&b)).transpose()
}

fn u_of(v: Option<Lv>, key: &str) -> Result<u64, DiskError> {
    match v {
        None | Some(Lv::Nil) => Ok(0),
        Some(Lv::String(t)) => std::str::from_utf8(&t)
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| DiskError::Corrupt(format!("server {key} is not a number"))),
        _ => Err(DiskError::Corrupt(format!("server {key} is not a number"))),
    }
}

fn schema_of(conn: &Connection) -> Result<u32, DiskError> {
    match get_meta(conn, "schema")? {
        Some(Lv::Number(n)) if n >= 1.0 && n.fract() == 0.0 && n <= f64::from(u32::MAX) => {
            Ok(n as u32)
        }
        _ => Err(DiskError::Corrupt("server schema version".into())),
    }
}

fn connect(path: &Path) -> Result<Connection, DiskError> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(conn)
}

impl ServerStore {
    /// Open the server store in `store_dir`'s server folder, making it if it
    /// is missing. An older schema is backed up and migrated as a campaign
    /// store is; a newer one is refused.
    pub fn open(store_dir: &Path, at_ms: u64) -> Result<ServerStore, DiskError> {
        ServerStore::open_with(store_dir, at_ms, MIGRATIONS)
    }

    /// [`ServerStore::open`] with these migration steps.
    pub fn open_with(
        store_dir: &Path,
        at_ms: u64,
        steps: &[Migration],
    ) -> Result<ServerStore, DiskError> {
        let dir = store_dir.join(FOLDER);
        fs::create_dir_all(dir.join("backups"))?;
        let path = dir.join(STORE);
        let known = steps.last().map_or(SCHEMA, |s| s.to);
        if !path.exists() {
            ServerStore::create(&path)?;
        }
        let mut conn = connect(&path)?;
        let found = schema_of(&conn)?;
        if found > known {
            return Err(DiskError::Newer { found, known });
        }
        if found < known {
            let folder = BackupFolder {
                dir: dir.clone(),
                log: 0,
                keep: usize::MAX,
            };
            let have = folder
                .list()?
                .last()
                .is_some_and(|b| b.schema == found && b.revision == 0);
            if !have {
                folder.take(at_ms, found, 0, BackupReason::PreMigration)?;
            }
            let tx = conn.transaction()?;
            for step in steps.iter().filter(|s| s.to > found) {
                (step.run)(&tx)?;
            }
            put_meta(&tx, "schema", &Lv::Number(f64::from(known)))?;
            tx.commit()?;
        }
        let epoch = u_of(get_meta(&conn, "epoch")?, "epoch")?;
        put_meta(&conn, "open", &Lv::Bool(true))?;
        Ok(ServerStore {
            dir,
            conn,
            epoch,
            written_epoch: epoch,
        })
    }

    /// Make the file under a temporary name in one transaction and rename
    /// it into place.
    fn create(path: &Path) -> Result<(), DiskError> {
        let tmp = path.with_extension("sqlite.new");
        let _ = fs::remove_file(&tmp);
        {
            let mut conn = Connection::open(&tmp)?;
            let tx = conn.transaction()?;
            tx.execute_batch(TABLES)?;
            put_meta(&tx, "schema", &Lv::Number(f64::from(SCHEMA)))?;
            put_meta(&tx, "epoch", &Lv::String(b"0".to_vec()))?;
            tx.commit()?;
            conn.close().map_err(|(_, e)| DiskError::Sql(e))?;
        }
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// The server folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The server folder's backups.
    pub fn backup_folder(&self, keep: usize) -> BackupFolder {
        BackupFolder {
            dir: self.dir.clone(),
            log: 0,
            keep,
        }
    }

    /// The server event log's file.
    pub fn events_path(&self) -> PathBuf {
        self.dir.join("events.sqlite")
    }

    /// The command epoch (spec/commands.md).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Raise the command epoch; written by the next commit. It never goes
    /// back.
    pub fn raise_epoch(&mut self, epoch: u64) {
        self.epoch = self.epoch.max(epoch);
    }

    /// Write the epoch if it moved. The driver commits before any command
    /// of a new epoch reaches DCS.
    pub fn commit(&mut self) -> Result<(), DiskError> {
        if self.epoch != self.written_epoch {
            put_meta(
                &self.conn,
                "epoch",
                &Lv::String(self.epoch.to_string().into_bytes()),
            )?;
            self.written_epoch = self.epoch;
        }
        Ok(())
    }

    /// Write everything and mark the store closed.
    pub fn close(&mut self) -> Result<(), DiskError> {
        self.commit()?;
        put_meta(&self.conn, "open", &Lv::Bool(false))
    }

    /// The secret player keys are derived with, if one was made.
    pub fn player_secret(&self) -> Result<Option<[u8; 32]>, DiskError> {
        let v: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT secret FROM player_secret WHERE id = 1", [], |r| {
                r.get(0)
            })
            .optional()?;
        match v {
            None => Ok(None),
            Some(b) => <[u8; 32]>::try_from(b.as_slice())
                .map(Some)
                .map_err(|_| DiskError::Corrupt("the player secret is not 32 bytes".into())),
        }
    }

    /// Keep the player secret. One per server: a second is refused, since
    /// it would give every player a new key.
    pub fn set_player_secret(&self, secret: [u8; 32]) -> Result<(), DiskError> {
        if self.player_secret()?.is_some_and(|s| s != secret) {
            return Err(DiskError::Exists(self.dir.join("player_secret")));
        }
        self.conn.execute(
            "INSERT OR IGNORE INTO player_secret (id, secret) VALUES (1, ?1)",
            [secret.as_slice()],
        )?;
        Ok(())
    }

    /// Every player key the server gave, with the ucid it came from.
    pub fn players(&self) -> Result<Vec<(String, String)>, DiskError> {
        let mut st = self
            .conn
            .prepare("SELECT key, ucid FROM player ORDER BY key")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Remember a player key and its ucid.
    pub fn put_player(&self, key: &str, ucid: &str) -> Result<(), DiskError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO player (key, ucid, body) VALUES (?1, ?2, ?3)",
            params![key, ucid, enc(&Lv::Table(Vec::new()))?],
        )?;
        Ok(())
    }

    /// Read `players.json` in `store_dir` (S16's keyring file: a `secret` of
    /// 64 hex digits and a `players` object of key to ucid) into the store
    /// once, then rename it `players.json.migrated`. Returns whether there
    /// was one. A file whose secret differs from the store's is refused.
    pub fn migrate_players_json(&self, store_dir: &Path) -> Result<bool, DiskError> {
        let path = store_dir.join(PLAYERS_JSON);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        // The message names no content: the file holds the secret.
        let bad = || DiskError::Corrupt(format!("{} is malformed", path.display()));
        let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
        let hex = v.get("secret").and_then(|s| s.as_str()).ok_or_else(bad)?;
        if hex.len() != 64 || !hex.is_ascii() {
            return Err(bad());
        }
        let mut secret = [0u8; 32];
        for (i, b) in secret.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        let mut pairs = Vec::new();
        if let Some(players) = v.get("players") {
            for (key, ucid) in players.as_object().ok_or_else(bad)? {
                pairs.push((key.clone(), ucid.as_str().ok_or_else(bad)?.to_owned()));
            }
        }
        self.set_player_secret(secret)?;
        for (k, u) in pairs {
            self.put_player(&k, &u)?;
        }
        fs::rename(&path, store_dir.join(format!("{PLAYERS_JSON}.migrated")))?;
        Ok(true)
    }

    /// Remember a changing call's key. With `campaign` the answer is in that
    /// campaign's store and this is its index row; without, the whole
    /// answer is kept here.
    pub fn put_idempotency(&self, row: &IdemRow, campaign: Option<u64>) -> Result<(), DiskError> {
        let (hash, response) = match campaign {
            Some(_) => (None, None),
            None => (Some(&row.request_hash), Some(&row.response)),
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO idempotency (key, at_ms, campaign, request_hash, response) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                row.key,
                row.at_ms as i64,
                campaign.map(|c| c as i64),
                hash,
                response
            ],
        )?;
        Ok(())
    }

    /// The key, if the server remembers it.
    pub fn idempotency(&self, key: &str) -> Result<Option<ServerIdem>, DiskError> {
        Ok(self
            .conn
            .query_row(
                "SELECT at_ms, campaign, request_hash, response FROM idempotency WHERE key = ?1",
                [key],
                |r| {
                    let at: i64 = r.get(0)?;
                    let c: Option<i64> = r.get(1)?;
                    let h: Option<Vec<u8>> = r.get(2)?;
                    let resp: Option<Vec<u8>> = r.get(3)?;
                    Ok(ServerIdem {
                        at_ms: u64::try_from(at).unwrap_or(0),
                        campaign: c.and_then(|c| u64::try_from(c).ok()),
                        answer: h.zip(resp),
                    })
                },
            )
            .optional()?)
    }

    /// Delete the keys answered before `before_ms`. Returns how many.
    pub fn prune_idempotency(&self, before_ms: u64) -> Result<usize, DiskError> {
        Ok(self.conn.execute(
            "DELETE FROM idempotency WHERE at_ms < ?1",
            [i64::try_from(before_ms).unwrap_or(i64::MAX)],
        )?)
    }

    /// Keep a deleted campaign's ID, so its tag is never given to a new
    /// campaign (spec/world.md, "Names").
    pub fn campaign_deleted(&self, id: u64, at_ms: u64) -> Result<(), DiskError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO deleted_campaign (id, at_ms) VALUES (?1, ?2)",
            params![id as i64, at_ms as i64],
        )?;
        Ok(())
    }

    /// Every campaign ID the server store knows besides the folders: the
    /// deleted ones and those its idempotency index names.
    pub fn known_campaigns(&self) -> Result<Vec<u64>, DiskError> {
        let mut st = self.conn.prepare(
            "SELECT id FROM deleted_campaign \
             UNION SELECT campaign FROM idempotency WHERE campaign IS NOT NULL",
        )?;
        let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(id) = u64::try_from(r?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// Server-scoped extension data by name.
    pub fn extension(&self, name: &str) -> Result<Option<Vec<u8>>, DiskError> {
        Ok(self
            .conn
            .query_row("SELECT body FROM extension WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Keep server-scoped extension data.
    pub fn set_extension(&self, name: &str, body: &[u8]) -> Result<(), DiskError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO extension (name, body) VALUES (?1, ?2)",
            params![name, body],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_is_exclusive_and_ends_with_its_holder() {
        let d = tempfile::tempdir().expect("dir");
        let a = lock(d.path()).expect("first");
        let e = lock(d.path()).expect_err("second");
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        drop(a);
        lock(d.path()).expect("free again");
    }

    #[test]
    fn the_epoch_keys_and_players_survive_a_reopen() {
        let d = tempfile::tempdir().expect("dir");
        let mut s = ServerStore::open(d.path(), 1).expect("open");
        assert_eq!(s.epoch(), 0);
        s.raise_epoch(7);
        s.raise_epoch(3);
        s.commit().expect("commit");
        s.set_player_secret([9; 32]).expect("secret");
        assert!(
            s.set_player_secret([8; 32]).is_err(),
            "one secret per server"
        );
        s.put_player("k1", "u1").expect("player");
        let row = IdemRow {
            key: "a".into(),
            at_ms: 5,
            request_hash: vec![1],
            response: vec![2],
        };
        s.put_idempotency(&row, None).expect("idem");
        s.put_idempotency(
            &IdemRow {
                key: "b".into(),
                ..row.clone()
            },
            Some(42),
        )
        .expect("index");
        drop(s);
        let s = ServerStore::open(d.path(), 2).expect("reopen");
        assert_eq!(s.epoch(), 7);
        assert_eq!(s.player_secret().expect("secret"), Some([9; 32]));
        assert_eq!(
            s.players().expect("players"),
            vec![("k1".into(), "u1".into())]
        );
        assert_eq!(
            s.idempotency("a").expect("a"),
            Some(ServerIdem {
                at_ms: 5,
                campaign: None,
                answer: Some((vec![1], vec![2]))
            })
        );
        assert_eq!(
            s.idempotency("b").expect("b").and_then(|i| i.campaign),
            Some(42)
        );
        s.campaign_deleted(99, 1).expect("tombstone");
        let mut known = s.known_campaigns().expect("known");
        known.sort_unstable();
        assert_eq!(known, vec![42, 99]);
        assert_eq!(s.prune_idempotency(6).expect("prune"), 2);
        assert_eq!(s.idempotency("a").expect("a"), None);
    }

    #[test]
    fn players_json_is_read_once_and_renamed() {
        let d = tempfile::tempdir().expect("dir");
        let secret = "ab".repeat(32);
        fs::write(
            d.path().join(PLAYERS_JSON),
            format!(r#"{{"secret":"{secret}","players":{{"k":"u"}}}}"#),
        )
        .expect("write");
        let s = ServerStore::open(d.path(), 1).expect("open");
        assert!(s.migrate_players_json(d.path()).expect("migrate"));
        assert_eq!(s.player_secret().expect("secret"), Some([0xab; 32]));
        assert_eq!(
            s.players().expect("players"),
            vec![("k".into(), "u".into())]
        );
        assert!(!d.path().join(PLAYERS_JSON).exists());
        assert!(d.path().join("players.json.migrated").exists());
        assert!(!s.migrate_players_json(d.path()).expect("again"));
    }

    #[test]
    fn a_migration_backs_up_the_server_store_first() {
        fn step(tx: &Transaction<'_>) -> rusqlite::Result<()> {
            tx.execute_batch("CREATE TABLE later (x);")
        }
        let d = tempfile::tempdir().expect("dir");
        let mut s = ServerStore::open(d.path(), 1).expect("open");
        s.raise_epoch(4);
        s.close().expect("close");
        drop(s);
        let steps = [Migration { to: 2, run: step }];
        let s = ServerStore::open_with(d.path(), 2, &steps).expect("migrate");
        assert_eq!(s.epoch(), 4);
        let b = s.backup_folder(7).list().expect("backups");
        assert_eq!(b.len(), 1);
        assert_eq!((b[0].schema, b[0].reason), (1, BackupReason::PreMigration));
        drop(s);
        assert!(matches!(
            ServerStore::open(d.path(), 3),
            Err(DiskError::Newer { found: 2, known: 1 })
        ));
    }
}
