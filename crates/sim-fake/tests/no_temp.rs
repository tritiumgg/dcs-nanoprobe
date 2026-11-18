//! The store's file paths on a host with no temporary folder (a `FROM
//! scratch` image run as a user): commits, the purge, a checkpoint, the
//! journal's and the event history's retention by time and by size, a
//! scheduled backup and the next one, an export, the read copy under
//! `.read/` and a restore. None may need a temporary file
//! (spec/persistence.md, "Backups"). The VFS that refuses them is in
//! `common/notemp.rs`; the churn test runs the reconciler's purge and expiry
//! under it too.

#[path = "common/notemp.rs"]
mod notemp;

use std::collections::BTreeMap;

use nanoprobe_sidecar::campaigns::{BackupReason, Campaigns};
use nanoprobe_sidecar::eventfile::{EventRow, Retained};
use nanoprobe_sidecar::ids::{CampaignId, EntityId};
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sidecar::store::{
    Authority, CampaignRecord, CampaignState, DiskError, IdemRow, JournalPrune, NewGroup, NewUnit,
    Point, UnitState,
};
use nanoprobe_sim_fake::harness::{CAMPAIGN, THEATER};

/// After the wall clock, which stamps the entries made before the test
/// sets the store's clock: the time retention takes a prefix in order.
const START_MS: u64 = 1_900_000_000_000;
/// The hub's `JOURNAL_BATCH` and `EVENT_BATCH`.
const BATCH: usize = 1_000;
/// Past what SQLite holds in memory for a copy: the backup failed from
/// about 16,000 events of 110 bytes in a `FROM scratch` image
/// (docs/results/2026-11-18-sidecar-bound.md, "Backups with no temporary
/// folder").
const EVENTS: u64 = 50_000;
const GROUPS: u64 = 3_000;

fn record() -> CampaignRecord {
    CampaignRecord {
        name: "no temp".into(),
        state: CampaignState::Active,
        theaters: vec![THEATER.into()],
        theater: THEATER.into(),
        clock_ms: Some(START_MS as i64),
        created_ms: START_MS,
        component_schemas: BTreeMap::new(),
        outcome: None,
    }
}

fn spec(n: u64) -> NewGroup {
    let units = (0..3)
        .map(|u| NewUnit {
            label: format!("put {n}.{u}"),
            skill: "Average".into(),
            ..NewUnit::new(
                "M-113",
                UnitState {
                    at: Point {
                        x: (n % 50) as f64 * 1_000.0 + 500.0,
                        y: 0.0,
                        z: f64::from(u) * 20.0,
                    },
                    ..UnitState::default()
                },
            )
        })
        .collect();
    NewGroup {
        theater: THEATER.into(),
        country: 2,
        category: 2,
        coalition: 2,
        units,
        authority: Authority::Sim,
        label: format!("put {n}"),
        extra: lv::obj(vec![("task", lv::s("Ground Nothing"))]),
    }
}

fn event(seq: u64) -> EventRow {
    EventRow {
        seq,
        wall_ms: START_MS + seq,
        source: Some(format!("1:{seq}")),
        body: vec![seq as u8; 110],
    }
}

/// What each path did: its name, the temporary files it was refused, and
/// its error.
#[derive(Default)]
struct Paths(Vec<(String, Vec<&'static str>, Option<String>)>);

impl Paths {
    fn run<T>(&mut self, what: &str, f: impl FnOnce() -> Result<T, DiskError>) -> Option<T> {
        notemp::take_refused();
        let r = f();
        let refused = notemp::take_refused();
        let (out, err) = match r {
            Ok(v) => (Some(v), None),
            Err(e) => (None, Some(e.to_string())),
        };
        self.0.push((what.to_owned(), refused, err));
        out
    }

    fn check(&self) {
        let bad: Vec<String> = self
            .0
            .iter()
            .filter(|(_, refused, err)| !refused.is_empty() || err.is_some())
            .map(|(what, refused, err)| format!("{what}: refused {refused:?}, error {err:?}"))
            .collect();
        assert!(
            bad.is_empty(),
            "paths that need a temporary file:\n{}",
            bad.join("\n")
        );
    }
}

/// Run a retention until it says nothing old is left; the rows deleted.
fn drain_events(mut f: impl FnMut() -> Result<Retained, DiskError>) -> Result<u64, DiskError> {
    let mut deleted = 0;
    loop {
        match f()? {
            Retained::Deleted { n, more } => {
                deleted += n;
                if !more {
                    return Ok(deleted);
                }
            }
            _ => return Ok(deleted),
        }
    }
}

fn drain_journal(mut f: impl FnMut() -> Result<JournalPrune, DiskError>) -> Result<u64, DiskError> {
    let mut deleted = 0;
    loop {
        match f()? {
            JournalPrune::Deleted { rows, more } => {
                deleted += rows;
                if !more {
                    return Ok(deleted);
                }
            }
            _ => return Ok(deleted),
        }
    }
}

/// The backups' events copy holds the history's rows exactly, and its byte
/// count is the sum over its rows.
fn copy_matches(c: &Campaigns, id: CampaignId) {
    let span = |path: &std::path::Path| -> (i64, i64, i64, i64) {
        let conn = rusqlite::Connection::open(path).expect("open");
        conn.query_row(
            "SELECT min(seq), max(seq), count(*), coalesce(sum(length(body)), 0) FROM event",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("span")
    };
    let copy = c.folder(id).join("backups").join("events.sqlite");
    let (a, b) = (span(&c.events_path(id)), span(&copy));
    assert_eq!(a, b, "the copy holds the history's rows");
    let conn = rusqlite::Connection::open(&copy).expect("open");
    let bytes: Vec<u8> = conn
        .query_row("SELECT value FROM meta WHERE key = 'bytes'", [], |r| {
            r.get(0)
        })
        .expect("a byte count");
    assert_eq!(String::from_utf8(bytes).expect("text"), b.3.to_string());
}

#[test]
fn no_path_of_the_store_needs_a_temporary_file() {
    notemp::install();
    let d = tempfile::tempdir().expect("dir");
    notemp::assert_refusing(d.path());
    let c = Campaigns::new(d.path(), 10_000, 3).expect("campaigns");
    let id = CampaignId(CAMPAIGN);
    let mut p = Paths::default();
    let mut s = p.run("create", || c.create(id, record())).expect("a store");
    let mut h = p.run("events open", || c.events(id)).expect("events");
    let mut now = START_MS;
    let mut made: Vec<EntityId> = Vec::new();
    p.run("store commits", || {
        for n in 0..GROUPS {
            now += 10;
            s.set_now(now);
            made.push(s.create_group(spec(n)).expect("group"));
            if n % 100 == 99 {
                s.commit()?;
            }
        }
        s.commit()
    });
    p.run("events commits", || {
        for seq in 1..=EVENTS {
            h.push(event(seq));
            if seq % 1_000 == 0 {
                h.commit()?;
            }
        }
        h.commit()
    });
    p.run("checkpoint", || s.checkpoint(now, Vec::new()).map(|_| ()));
    p.run("scheduled backup", || {
        c.backup(id, now, s.revision(), BackupReason::Scheduled)
            .map(|_| ())
    });
    p.run("idempotency answers", || {
        for k in 0..5_000u64 {
            s.put_idempotency(IdemRow {
                key: format!("key-{k}"),
                at_ms: START_MS + k,
                request_hash: vec![7; 32],
                response: vec![1; 64],
            });
        }
        s.commit()
    });
    let pruned = p.run("idempotency retention", || {
        s.prune_idempotency(START_MS + 4_000)
    });
    assert_eq!(pruned, Some(4_000), "the retention deleted the old keys");
    p.run("removal and purge", || {
        for g in &made[..GROUPS as usize / 2] {
            s.remove_entity(*g).expect("remove");
        }
        s.commit()?;
        for g in &made[..GROUPS as usize / 2] {
            s.purge(*g).expect("a removed group");
        }
        s.commit()
    });
    let later = now + 40 * 86_400_000;
    // Each retention deletes: a step that deletes nothing proves nothing.
    // The events span 50 s and the journal 30 s, so a retention of 1 s
    // takes most of each.
    let events_by_time = p.run("event retention by time", || {
        drain_events(|| h.retain(1_000, later, BATCH, 0))
    });
    let events_by_size = p.run("event retention by size", || {
        let max = h.bytes() / 2;
        drain_events(|| h.retain(0, later, BATCH, max))
    });
    let journal_by_time = p.run("journal retention by time", || {
        drain_journal(|| s.prune_journal(1_000, later, BATCH, 0))
    });
    let journal_by_size = p.run("journal retention by size", || {
        let max = s.journal_bytes() / 2;
        drain_journal(|| s.prune_journal(0, later, BATCH, max))
    });
    for (what, n) in [
        ("events by time", events_by_time),
        ("events by size", events_by_size),
        ("journal by size", journal_by_size),
        ("journal by time", journal_by_time),
    ] {
        assert!(n.unwrap_or(0) > 0, "the {what} retention deleted nothing");
    }
    // Sources of several boots, as a live history has: the copy's `source`
    // index takes them all over its existing pages.
    p.run("more events commits", || {
        let next = h.next_seq();
        for seq in next..next + EVENTS {
            h.push(EventRow {
                source: Some(format!("{}:{seq}", seq % 7)),
                ..event(seq)
            });
            if seq % 1_000 == 0 {
                h.commit()?;
            }
        }
        h.commit()
    });
    p.run("next scheduled backup", || {
        c.backup(id, later, s.revision(), BackupReason::Scheduled)
            .map(|_| ())
    });
    copy_matches(&c, id);
    p.run("export", || {
        nanoprobe_sidecar::export::export(id.0, &c.store_path(id), &c.events_path(id), &mut |_| {
            true
        })
    });
    p.run("read copy under .read/", || {
        c.read_copy(id, later).map(drop)
    });
    drop(s);
    drop(h);
    p.run("restore", || c.restore(id, 1, later + 1).map(drop));
    p.check();
}
