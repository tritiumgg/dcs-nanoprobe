//! The campaign folders in `[store] dir` and the operations on them
//! (spec/persistence.md, "Files" and "Campaign operations").

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::eventfile::EventFile;
use crate::ids::CampaignId;
use crate::store::{
    CampaignRecord, CampaignState, DiskError, MIGRATIONS, Migration, SCHEMA, Store, TierPeriod,
    backup_file, ident_of, next_entity_of, rebase_world, record_of, revision_of, state_of,
};

/// Why a backup was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupReason {
    /// Every `backup_interval_h` while active.
    Scheduled,
    /// On request through the API.
    Manual,
    /// Before a schema migration.
    PreMigration,
}

impl BackupReason {
    fn name(self) -> &'static str {
        match self {
            BackupReason::Scheduled => "scheduled",
            BackupReason::Manual => "manual",
            BackupReason::PreMigration => "pre_migration",
        }
    }

    fn parse(v: &str) -> Option<BackupReason> {
        [
            BackupReason::Scheduled,
            BackupReason::Manual,
            BackupReason::PreMigration,
        ]
        .into_iter()
        .find(|r| r.name() == v)
    }
}

/// A backup kept in a campaign's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    /// Its number, counting up from 1 in the folder, never reused.
    pub number: u32,
    /// When it was taken, milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// The store copy's size in bytes.
    pub size: u64,
    /// The store's schema version.
    pub schema: u32,
    /// The store revision it holds.
    pub revision: u64,
    /// Why it was taken.
    pub reason: BackupReason,
}

/// A campaign in the folder, with its record or why it could not be read.
pub type Listed = (CampaignId, Result<CampaignRecord, DiskError>);

/// The folder of every campaign.
#[derive(Clone, Debug)]
pub struct Campaigns {
    dir: PathBuf,
    changes_keep: usize,
    backup_keep: usize,
}

const STORE: &str = "store.sqlite";
const EVENTS: &str = "events.sqlite";
const BACKUPS: &str = "backups";
const INFO: &str = "backup.txt";
const READ: &str = ".read";

impl Campaigns {
    /// The campaigns in `dir`, which is created if missing. `backup_keep` is
    /// at least 1.
    pub fn new(dir: &Path, changes_keep: usize, backup_keep: usize) -> io::Result<Campaigns> {
        fs::create_dir_all(dir)?;
        Ok(Campaigns {
            dir: dir.to_owned(),
            changes_keep,
            backup_keep: backup_keep.max(1),
        })
    }

    /// The store dir.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The changes a store keeps for a watch to resume from.
    pub fn changes_keep(&self) -> usize {
        self.changes_keep
    }

    /// A campaign's folder.
    pub fn folder(&self, id: CampaignId) -> PathBuf {
        self.dir.join(id.0.to_string())
    }

    /// A campaign's store file.
    pub fn store_path(&self, id: CampaignId) -> PathBuf {
        self.folder(id).join(STORE)
    }

    /// Create a campaign: its folder, its store with the record and an empty
    /// world, and its empty events file. A failure leaves no folder behind.
    pub fn create(&self, id: CampaignId, record: CampaignRecord) -> Result<Store, DiskError> {
        let folder = self.folder(id);
        if folder.exists() {
            return Err(DiskError::Exists(folder));
        }
        let made = (|| {
            fs::create_dir_all(folder.join(BACKUPS))?;
            EventFile::open(&folder.join(EVENTS), id.0)?;
            Store::create_on_disk(&folder.join(STORE), id, record, self.changes_keep)
        })();
        if made.is_err() {
            let _ = fs::remove_dir_all(&folder);
        }
        made
    }

    /// Every campaign in the folder, by ID, with its record or why it could
    /// not be read. Reading migrates nothing.
    pub fn list(&self) -> Result<Vec<Listed>, DiskError> {
        let mut out = Vec::new();
        for e in fs::read_dir(&self.dir)? {
            let e = e?;
            let Some(id) = e.file_name().to_str().and_then(|n| n.parse::<u64>().ok()) else {
                continue;
            };
            let id = CampaignId(id);
            let path = e.path().join(STORE);
            if !path.exists() {
                continue;
            }
            let r = record_of(&path).and_then(|(found, schema, r)| {
                if found != id {
                    Err(DiskError::Corrupt(format!(
                        "folder {} holds campaign {}",
                        id.0, found.0
                    )))
                } else if schema > SCHEMA {
                    Err(DiskError::Newer {
                        found: schema,
                        known: SCHEMA,
                    })
                } else {
                    Ok(r)
                }
            });
            out.push((id, r));
        }
        out.sort_by_key(|(id, _)| id.0);
        Ok(out)
    }

    /// Open a campaign's store at this sidecar's schema version.
    pub fn open(&self, id: CampaignId, at_ms: u64) -> Result<Store, DiskError> {
        self.open_with(id, at_ms, MIGRATIONS)
    }

    /// [`Campaigns::open`] with these migration steps. A store at an older
    /// version is backed up first, whatever the schedule.
    pub fn open_with(
        &self,
        id: CampaignId,
        at_ms: u64,
        steps: &[Migration],
    ) -> Result<Store, DiskError> {
        let path = self.store_path(id);
        let (found_id, found) = ident_of(&path)?;
        if found_id != id {
            return Err(DiskError::Corrupt(format!(
                "folder {} holds campaign {}",
                id.0, found_id.0
            )));
        }
        let known = steps.last().map_or(SCHEMA, |s| s.to);
        if found > known {
            return Err(DiskError::Newer { found, known });
        }
        // A finished store is read-only: never migrated or written again.
        if state_of(&path)? == CampaignState::Finished {
            return Err(DiskError::Finished(id));
        }
        if found < known {
            // A backup of this very state is enough (spec/persistence.md).
            let revision = revision_of(&path)?;
            let have = self
                .backups(id)?
                .last()
                .is_some_and(|b| b.schema == found && b.revision == revision);
            if !have {
                self.backup_folder(id)
                    .take(at_ms, found, revision, BackupReason::PreMigration)?;
            }
        }
        Store::open_with(&path, self.changes_keep, steps)
    }

    /// The campaign's events file.
    pub fn events_path(&self, id: CampaignId) -> PathBuf {
        self.folder(id).join(EVENTS)
    }

    /// Open the campaign's events file.
    pub fn events(&self, id: CampaignId) -> Result<EventFile, DiskError> {
        EventFile::open(&self.events_path(id), id.0)
    }

    /// Activate `id`: refuse a missing or finished campaign, suspend the
    /// active campaign in `active`, then open `id` and mark it active
    /// (spec/persistence.md, "Campaign operations"). The caller then loads
    /// its mission and the reconciler restores it. A refusal leaves `active`
    /// as it was; a failure in or after the suspend leaves it empty, and the
    /// caller reports it.
    pub fn activate(
        &self,
        active: &mut Option<Store>,
        id: CampaignId,
        at_ms: u64,
        achieved: Vec<TierPeriod>,
    ) -> Result<(), DiskError> {
        if active.as_ref().is_some_and(|cur| cur.campaign() == id) {
            return Ok(());
        }
        let path = self.store_path(id);
        let (found, _) = ident_of(&path)?;
        if found != id {
            return Err(DiskError::Corrupt(format!(
                "folder {} holds campaign {}",
                id.0, found.0
            )));
        }
        if state_of(&path)? == CampaignState::Finished {
            return Err(DiskError::Finished(id));
        }
        if let Some(cur) = active.as_mut() {
            let suspended = self.suspend(cur, at_ms, achieved);
            // From here the old store is closed or half-closed: it leaves
            // `active` either way.
            *active = None;
            suspended?;
        }
        let mut s = self.open(id, at_ms)?;
        let mut r = s
            .record()
            .cloned()
            .ok_or_else(|| DiskError::Corrupt("no campaign record".into()))?;
        r.state = CampaignState::Active;
        s.set_record(r)?;
        *active = Some(s);
        Ok(())
    }

    /// Suspend a campaign: a final checkpoint and the state set to
    /// `suspended`. The caller then drops the store, which closes it.
    pub fn suspend(
        &self,
        store: &mut Store,
        at_ms: u64,
        achieved: Vec<TierPeriod>,
    ) -> Result<(), DiskError> {
        self.close(store, at_ms, achieved, CampaignState::Suspended, None)
    }

    /// Finish a campaign: as suspend, with the outcome record, and the state
    /// `finished`.
    pub fn finish(
        &self,
        store: &mut Store,
        at_ms: u64,
        achieved: Vec<TierPeriod>,
        outcome: Vec<u8>,
    ) -> Result<(), DiskError> {
        self.close(
            store,
            at_ms,
            achieved,
            CampaignState::Finished,
            Some(outcome),
        )
    }

    fn close(
        &self,
        store: &mut Store,
        at_ms: u64,
        achieved: Vec<TierPeriod>,
        state: CampaignState,
        outcome: Option<Vec<u8>>,
    ) -> Result<(), DiskError> {
        let mut r = store
            .record()
            .cloned()
            .ok_or_else(|| DiskError::Corrupt("no campaign record".into()))?;
        r.state = state;
        if outcome.is_some() {
            r.outcome = outcome;
        }
        store.close_with(at_ms, achieved, r).map(|_| ())
    }

    /// Take a backup of a campaign's store through a connection of its own,
    /// so it may run on a worker thread while the driver goes on writing
    /// (spec/persistence.md, "Backups"). The caller committed the store and
    /// its events first, and passes the revision it holds. Then the events
    /// copy is extended and old backups pruned.
    pub fn backup(
        &self,
        id: CampaignId,
        at_ms: u64,
        revision: u64,
        reason: BackupReason,
    ) -> Result<Backup, DiskError> {
        self.backup_folder(id).take(at_ms, SCHEMA, revision, reason)
    }

    /// The backups of a campaign's folder.
    pub fn backup_folder(&self, id: CampaignId) -> BackupFolder {
        BackupFolder {
            dir: self.folder(id),
            log: id.0,
            keep: self.backup_keep,
        }
    }

    /// The backups kept, oldest first.
    pub fn backups(&self, id: CampaignId) -> Result<Vec<Backup>, DiskError> {
        self.backup_folder(id).list()
    }

    /// Restore backup `number` over a campaign's store, offline: the caller
    /// holds the store dir's lock, so no sidecar has the store open
    /// (spec/persistence.md, "Restore a backup"). The current store is first
    /// copied to `restore-<at_ms>/store.sqlite` in the campaign's folder;
    /// its WAL files are removed; the backup is copied in; and the restored
    /// store's next `EntityId` and revision are raised above both copies'.
    pub fn restore(&self, id: CampaignId, number: u32, at_ms: u64) -> Result<Restored, DiskError> {
        let backup = self
            .backups(id)?
            .into_iter()
            .find(|b| b.number == number)
            .ok_or_else(|| {
                DiskError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("campaign {} has no complete backup {number}", id.0),
                ))
            })?;
        let path = self.store_path(id);
        let (found, _) = ident_of(&path)?;
        if found != id {
            return Err(DiskError::Corrupt(format!(
                "folder {} holds campaign {}",
                id.0, found.0
            )));
        }
        let source = self
            .folder(id)
            .join(BACKUPS)
            .join(number.to_string())
            .join(STORE);
        let (backup_id, _) = ident_of(&source)?;
        if backup_id != id {
            return Err(DiskError::Corrupt(format!(
                "backup {number} holds campaign {}",
                backup_id.0
            )));
        }
        // What the current store handed out, read with its WAL applied.
        let next_entity = next_entity_of(&path)?;
        let revision = revision_of(&path)?;
        let aside_dir = self.folder(id).join(format!("restore-{at_ms}"));
        fs::create_dir(&aside_dir)?;
        let aside = aside_dir.join(STORE);
        backup_file(&path, &aside)?;
        sync_file(&aside)?;
        // The copy goes in under a temporary name, then over the store, so a
        // crash leaves the old store or the whole backup.
        let tmp = self.folder(id).join(".restore.sqlite");
        let _ = fs::remove_file(&tmp);
        fs::copy(&source, &tmp)?;
        sync_file(&tmp)?;
        for wal in ["-wal", "-shm"] {
            let mut p = path.clone().into_os_string();
            p.push(wal);
            match fs::remove_file(PathBuf::from(p)) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        fs::rename(&tmp, &path)?;
        let (next_entity, revision) = rebase_world(&path, next_entity, revision)?;
        Ok(Restored {
            backup,
            aside,
            next_entity,
            revision,
        })
    }

    /// A consistent copy of a campaign's store, migrated to this sidecar's
    /// schema and opened, for reading a finished campaign without opening
    /// its store (spec/persistence.md, "Reading a finished campaign"). The
    /// copy is under `.read/` in the campaign's folder and is removed when
    /// the value drops.
    pub fn read_copy(&self, id: CampaignId, at_ms: u64) -> Result<ReadCopy, DiskError> {
        let path = self.store_path(id);
        let (found, schema) = ident_of(&path)?;
        if found != id {
            return Err(DiskError::Corrupt(format!(
                "folder {} holds campaign {}",
                id.0, found.0
            )));
        }
        let known = MIGRATIONS.last().map_or(SCHEMA, |s| s.to);
        if schema > known {
            return Err(DiskError::Newer {
                found: schema,
                known,
            });
        }
        let dir = self.folder(id).join(READ);
        fs::create_dir_all(&dir)?;
        let copy = dir.join(format!("{at_ms}-{}.sqlite", std::process::id()));
        let _ = fs::remove_file(&copy);
        backup_file(&path, &copy)?;
        let mut guard = ReadCopy {
            store: None,
            path: copy,
        };
        guard.store = Some(Store::open(&guard.path, self.changes_keep)?);
        Ok(guard)
    }

    /// Remove copies a stopped sidecar left under `.read/`.
    pub fn clear_read_copies(&self, id: CampaignId) {
        let _ = fs::remove_dir_all(self.folder(id).join(READ));
    }

    /// Delete a campaign's folder. Never automatic.
    pub fn delete(&self, id: CampaignId) -> io::Result<()> {
        fs::remove_dir_all(self.folder(id))
    }
}

/// What [`Campaigns::restore`] did.
#[derive(Debug)]
pub struct Restored {
    /// The backup restored.
    pub backup: Backup,
    /// Where the store it replaced was copied.
    pub aside: PathBuf,
    /// The restored store's next `EntityId`.
    pub next_entity: u64,
    /// The restored store's revision.
    pub revision: u64,
}

/// A migrated copy of a campaign's store, open for reading; the files are
/// removed when it drops.
#[derive(Debug)]
pub struct ReadCopy {
    store: Option<Store>,
    path: PathBuf,
}

impl ReadCopy {
    /// The store.
    pub fn store(&self) -> &Store {
        self.store.as_ref().expect("open until dropped")
    }
}

impl Drop for ReadCopy {
    fn drop(&mut self) {
        // The connection closes first, then its files go.
        drop(self.store.take());
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.path.clone().into_os_string();
            p.push(suffix);
            let _ = fs::remove_file(PathBuf::from(p));
        }
    }
}

fn sync_file(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new().write(true).open(path)?.sync_all()
}

/// The backups of one store folder: a campaign's, or the server store's
/// (spec/persistence.md, "Backups").
#[derive(Clone, Debug)]
pub struct BackupFolder {
    /// The folder of the store, its events file and `backups/`.
    pub dir: PathBuf,
    /// The campaign ID in its events file's `meta`, 0 for the server.
    pub log: u64,
    /// `backup_keep`.
    pub keep: usize,
}

impl BackupFolder {
    /// Make backup folder `n`: a consistent copy of the store, then the info
    /// file that marks it complete. A failure removes the folder. Then the
    /// events copy is extended and old backups pruned.
    pub fn take(
        &self,
        at_ms: u64,
        schema: u32,
        revision: u64,
        reason: BackupReason,
    ) -> Result<Backup, DiskError> {
        let number = self.next()?;
        let dir = self.dir.join(BACKUPS).join(number.to_string());
        let made = (|| {
            fs::create_dir_all(&dir)?;
            backup_file(&self.dir.join(STORE), &dir.join(STORE))?;
            // VACUUM INTO does not flush the copy: flush it before the
            // marker that says it is complete, then the marker.
            fs::File::open(dir.join(STORE))?.sync_all()?;
            let mut info = fs::File::create(dir.join(INFO))?;
            io::Write::write_all(
                &mut info,
                format!("{at_ms} {schema} {revision} {}\n", reason.name()).as_bytes(),
            )?;
            info.sync_all()?;
            Ok::<u64, DiskError>(fs::metadata(dir.join(STORE))?.len())
        })();
        let size = match made {
            Ok(size) => size,
            Err(e) => {
                let _ = fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        EventFile::copy_into(
            &self.dir.join(EVENTS),
            &self.dir.join(BACKUPS).join(EVENTS),
            self.log,
        )?;
        self.prune(number)?;
        Ok(Backup {
            number,
            at_ms,
            size,
            schema,
            revision,
            reason,
        })
    }

    fn next(&self) -> Result<u32, DiskError> {
        let dir = self.dir.join(BACKUPS);
        fs::create_dir_all(&dir)?;
        // The counter survives pruning: a number is never reused.
        let counter = dir.join("next");
        let mut newest = fs::read_to_string(&counter)
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .unwrap_or(0);
        for n in self.numbered()? {
            newest = newest.max(n);
        }
        let n = newest + 1;
        fs::write(&counter, n.to_string())?;
        Ok(n)
    }

    /// Every numbered folder in `backups/`, complete or not.
    fn numbered(&self) -> Result<Vec<u32>, DiskError> {
        let mut out = Vec::new();
        match fs::read_dir(self.dir.join(BACKUPS)) {
            Ok(rd) => {
                for e in rd {
                    if let Some(n) = e?.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) {
                        out.push(n);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(out)
    }

    /// Keep the newest `backup_keep` scheduled and manual backups and the
    /// newest pre-migration backup of each schema version; remove the
    /// others, and every incomplete folder older than backup `newest`.
    fn prune(&self, newest: u32) -> Result<(), DiskError> {
        let complete = self.list()?;
        let mut gone: Vec<u32> = Vec::new();
        let (pre, kept): (Vec<&Backup>, Vec<&Backup>) = complete
            .iter()
            .partition(|b| b.reason == BackupReason::PreMigration);
        if kept.len() > self.keep {
            gone.extend(kept[..kept.len() - self.keep].iter().map(|b| b.number));
        }
        for b in &pre {
            if pre
                .iter()
                .any(|o| o.schema == b.schema && o.number > b.number)
            {
                gone.push(b.number);
            }
        }
        let complete: Vec<u32> = complete.iter().map(|b| b.number).collect();
        gone.extend(
            self.numbered()?
                .into_iter()
                .filter(|n| *n < newest && !complete.contains(n)),
        );
        for n in gone {
            fs::remove_dir_all(self.dir.join(BACKUPS).join(n.to_string()))?;
        }
        Ok(())
    }

    /// The backups kept, oldest first.
    pub fn list(&self) -> Result<Vec<Backup>, DiskError> {
        let dir = self.dir.join(BACKUPS);
        let mut out = Vec::new();
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for e in rd {
            let e = e?;
            let Some(number) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(info) = fs::read_to_string(e.path().join(INFO)) else {
                // Taken in part: a backup cut by a crash is not one.
                continue;
            };
            let mut w = info.split_whitespace();
            let (Some(at), Some(schema), Some(revision), Some(reason)) =
                (w.next(), w.next(), w.next(), w.next())
            else {
                continue;
            };
            let (Ok(at_ms), Ok(schema), Ok(revision), Some(reason)) = (
                at.parse(),
                schema.parse(),
                revision.parse(),
                BackupReason::parse(reason),
            ) else {
                continue;
            };
            let Ok(meta) = fs::metadata(e.path().join(STORE)) else {
                continue;
            };
            out.push(Backup {
                number,
                at_ms,
                size: meta.len(),
                schema,
                revision,
                reason,
            });
        }
        out.sort_by_key(|b| b.number);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::store::{REVISION_RESERVE, schema_of};

    fn record(state: CampaignState) -> CampaignRecord {
        CampaignRecord {
            name: "c".into(),
            state,
            theaters: vec!["Caucasus".into()],
            theater: "Caucasus".into(),
            clock_ms: None,
            created_ms: 1,
            component_schemas: BTreeMap::new(),
            outcome: None,
        }
    }

    fn campaigns() -> (tempfile::TempDir, Campaigns) {
        let d = tempfile::tempdir().expect("dir");
        let c = Campaigns::new(d.path(), 10, 2).expect("campaigns");
        (d, c)
    }

    #[test]
    fn a_restore_keeps_ids_and_revisions_above_both_copies() {
        let (_d, c) = campaigns();
        let id = CampaignId(1);
        let mut s = c.create(id, record(CampaignState::Active)).expect("create");
        for _ in 0..3 {
            s.adopt_id().expect("id");
        }
        let key = |k: &str| nanoprobe_transport::codec::Lv::String(k.as_bytes().to_vec());
        s.set_owed_load(Some(crate::store::OwedLoad {
            path: "Z:\\m\\shells\\1.a.miz".into(),
            cmd: Some((1, 2)),
            start_ms: Some(5),
        }));
        s.set_next_mission(Some(nanoprobe_transport::codec::Lv::Table(vec![
            (key("theater"), key("Caucasus")),
            (key("start_ms"), key("5")),
        ])));
        s.commit().expect("commit");
        let b = c
            .backup(id, 5, s.revision(), BackupReason::Manual)
            .expect("backup");
        for _ in 0..5 {
            s.adopt_id().expect("id");
        }
        s.commit().expect("commit");
        let (next, rev) = (
            next_entity_of(&c.store_path(id)).expect("next"),
            revision_of(&c.store_path(id)).expect("rev"),
        );
        assert_eq!(next, 9);
        // The sidecar stopped without closing it: its WAL is still there.
        std::mem::forget(s);
        assert!(c.restore(id, 99, 7).is_err(), "no backup 99");
        let r = c.restore(id, b.number, 7).expect("restore");
        assert_eq!(r.next_entity, 9, "the larger next EntityId");
        assert_eq!(r.revision, rev + REVISION_RESERVE);
        assert_eq!(
            next_entity_of(&r.aside).expect("aside"),
            9,
            "the old store is kept aside"
        );
        assert!(!c.folder(id).join("store.sqlite-wal").exists());
        let opened = c.open(id, 8).expect("open the restored store");
        assert_eq!(
            opened.revision(),
            rev + REVISION_RESERVE,
            "closed: no second reserve"
        );
        assert_eq!(
            opened.record().expect("record").state,
            CampaignState::Suspended
        );
        assert_eq!(opened.owed_load(), None);
        assert_eq!(
            opened.next_mission(),
            Some(&nanoprobe_transport::codec::Lv::Table(vec![(
                key("theater"),
                key("Caucasus")
            )])),
            "the start is cleared, the rest kept"
        );
        assert!(opened.unanswered().is_empty());
        assert_eq!(
            c.backups(id).expect("backups"),
            vec![b],
            "backups untouched"
        );
        assert!(c.restore(id, 1, 7).is_err(), "the aside folder exists");
    }

    #[test]
    fn a_finished_campaign_reads_through_a_copy_that_goes_away() {
        let (_d, c) = campaigns();
        let id = CampaignId(1);
        let mut s = c.create(id, record(CampaignState::Active)).expect("create");
        s.adopt_id().expect("id");
        c.finish(&mut s, 3, Vec::new(), b"won".to_vec())
            .expect("finish");
        drop(s);
        let before = fs::read(c.store_path(id)).expect("bytes");
        let copy = c.read_copy(id, 4).expect("copy");
        assert_eq!(
            copy.store().record().expect("record").state,
            CampaignState::Finished
        );
        let path = c.folder(id).join(READ);
        assert!(fs::read_dir(&path).expect("dir").count() > 0);
        drop(copy);
        assert_eq!(
            fs::read_dir(&path).expect("dir").count(),
            0,
            "the copy is removed"
        );
        assert_eq!(
            fs::read(c.store_path(id)).expect("bytes"),
            before,
            "the store is untouched"
        );
        assert!(matches!(c.open(id, 5), Err(DiskError::Finished(_))));
    }

    #[test]
    fn a_bad_folder_is_listed_as_an_error_and_the_rest_read() {
        let (_d, c) = campaigns();
        drop(
            c.create(CampaignId(1), record(CampaignState::Suspended))
                .expect("1"),
        );
        drop(
            c.create(CampaignId(2), record(CampaignState::Suspended))
                .expect("2"),
        );
        // Folder 3 holds campaign 2's store.
        fs::create_dir_all(c.folder(CampaignId(3))).expect("dir");
        fs::copy(c.store_path(CampaignId(2)), c.store_path(CampaignId(3))).expect("copy");
        // Folder 4 holds garbage.
        fs::create_dir_all(c.folder(CampaignId(4))).expect("dir");
        fs::write(c.store_path(CampaignId(4)), b"not a database").expect("garbage");
        // Folder 5 is at a newer schema, with a record this sidecar cannot read.
        drop(
            c.create(CampaignId(5), record(CampaignState::Suspended))
                .expect("5"),
        );
        rusqlite::Connection::open(c.store_path(CampaignId(5)))
            .expect("conn")
            .execute_batch("UPDATE meta SET value = X'00' WHERE key = 'campaign';")
            .expect("damage");
        let mut w = nanoprobe_transport::codec::Writer::new();
        w.lv(&nanoprobe_transport::codec::Lv::Number(f64::from(
            SCHEMA + 1,
        )));
        rusqlite::Connection::open(c.store_path(CampaignId(5)))
            .expect("conn")
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema'",
                [w.finish()],
            )
            .expect("schema");
        let listed = c.list().expect("list");
        let ok: Vec<u64> = listed
            .iter()
            .filter(|(_, r)| r.is_ok())
            .map(|(id, _)| id.0)
            .collect();
        assert_eq!(ok, vec![1, 2]);
        assert!(
            matches!(listed[2].1, Err(DiskError::Corrupt(_))),
            "{:?}",
            listed[2].1
        );
        assert!(listed[3].1.is_err());
        assert!(
            matches!(listed[4].1, Err(DiskError::Newer { .. })),
            "{:?}",
            listed[4].1
        );
        assert!(matches!(
            c.open(CampaignId(5), 1),
            Err(DiskError::Newer { .. })
        ));
    }

    #[test]
    fn a_failed_create_or_backup_leaves_no_folder() {
        let (_d, c) = campaigns();
        let s = c
            .create(CampaignId(1), record(CampaignState::Active))
            .expect("create");
        // A store that cannot be copied fails the backup.
        let moved = c.folder(CampaignId(1)).join("moved.sqlite");
        fs::rename(c.store_path(CampaignId(1)), &moved).expect("move");
        let err = c
            .backup_folder(CampaignId(1))
            .take(1, SCHEMA, 0, BackupReason::Manual);
        assert!(err.is_err());
        fs::rename(&moved, c.store_path(CampaignId(1))).expect("move back");
        assert_eq!(
            c.backup_folder(CampaignId(1)).numbered().expect("numbered"),
            Vec::<u32>::new()
        );
        // A folder left without its marker is removed at the next backup.
        fs::create_dir_all(c.folder(CampaignId(1)).join(BACKUPS).join("2")).expect("dir");
        let b = c
            .backup(CampaignId(1), 2, s.revision(), BackupReason::Manual)
            .expect("backup");
        assert_eq!(b.number, 3);
        assert_eq!(
            c.backup_folder(CampaignId(1)).numbered().expect("numbered"),
            vec![3]
        );
        assert!(b.size > 0);
        // A create that fails leaves no campaign folder behind.
        let bad = Campaigns {
            dir: c.dir.join("not-a-folder"),
            ..c.clone()
        };
        fs::write(&bad.dir, b"a file").expect("file");
        assert!(
            bad.create(CampaignId(1), record(CampaignState::Active))
                .is_err()
        );
        assert!(!bad.folder(CampaignId(1)).exists());
    }

    #[test]
    fn activating_a_missing_or_finished_campaign_keeps_the_active_one() {
        let (_d, c) = campaigns();
        let a = c
            .create(CampaignId(1), record(CampaignState::Active))
            .expect("a");
        let mut f = c
            .create(CampaignId(2), record(CampaignState::Active))
            .expect("f");
        c.finish(&mut f, 1, Vec::new(), b"won".to_vec())
            .expect("finish");
        drop(f);
        let mut active = Some(a);
        assert!(
            c.activate(&mut active, CampaignId(3), 1, Vec::new())
                .is_err()
        );
        assert!(matches!(
            c.activate(&mut active, CampaignId(2), 1, Vec::new()),
            Err(DiskError::Finished(CampaignId(2)))
        ));
        let cur = active.as_ref().expect("still active");
        assert_eq!(cur.campaign(), CampaignId(1));
        assert_eq!(cur.record().expect("r").state, CampaignState::Active);
        // A finished store at an older schema is refused before any backup
        // or migration.
        fn step(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
            tx.execute_batch("CREATE TABLE later (x);")
        }
        let steps = [Migration { to: 2, run: step }];
        assert!(matches!(
            c.open_with(CampaignId(2), 1, &steps),
            Err(DiskError::Finished(CampaignId(2)))
        ));
        assert!(c.backups(CampaignId(2)).expect("backups").is_empty());
        assert_eq!(
            schema_of(&c.store_path(CampaignId(2))).expect("schema"),
            SCHEMA
        );
        assert!(matches!(
            c.open(CampaignId(2), 1),
            Err(DiskError::Finished(CampaignId(2)))
        ));
    }

    #[test]
    fn revisions_handed_out_before_an_unclean_stop_are_not_reused() {
        let (_d, c) = campaigns();
        let mut s = c
            .create(CampaignId(1), record(CampaignState::Active))
            .expect("a");
        let g = s
            .create_group(crate::store::NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: vec![crate::store::NewUnit::new("M-113", Default::default())],
                authority: crate::store::Authority::Sim,
                label: String::new(),
                extra: nanoprobe_transport::codec::Lv::Nil,
            })
            .expect("group");
        s.commit().expect("commit");
        let m = s.group(g).expect("g").members[0];
        // Revisions a watcher saw, never written.
        for x in 1..=5 {
            s.observe_position(
                m,
                crate::store::Point {
                    x: f64::from(x),
                    y: 0.0,
                    z: 0.0,
                },
            );
        }
        let seen = s.revision();
        drop(s);
        let mut s = c.open(CampaignId(1), 1).expect("reopen");
        assert!(s.revision() >= seen - 5 + REVISION_RESERVE);
        assert!(s.revision() > seen);
        // A second unclean stop right after: the first open wrote its moved
        // revision, so the next moves past it again.
        let r1 = s.revision();
        s.observe_position(
            m,
            crate::store::Point {
                x: 99.0,
                y: 0.0,
                z: 0.0,
            },
        );
        drop(s);
        let s = c.open(CampaignId(1), 1).expect("reopen again");
        assert!(s.revision() >= r1 + REVISION_RESERVE);
        // A clean close keeps the revision.
        let mut s = s;
        let rev = s.revision();
        c.suspend(&mut s, 1, Vec::new()).expect("suspend");
        drop(s);
        assert_eq!(c.open(CampaignId(1), 1).expect("open").revision(), rev);
    }
}
