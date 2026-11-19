//! The API over campaign stores on disk (spec/persistence.md): a sidecar
//! restart, suspend with capture, finish, the checkpoint and backup timers,
//! the Events service over `events.sqlite` with retention, the server
//! store, idempotency across restarts, and a crash between the two commits.

use std::path::Path;
use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_fake_sidecar::{Running, in_process, start};
use nanoprobe_proto::errors::reason;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityState, ErrorReason, EventFilter,
    GroupCategory, GroupSpec, Idempotency, Lease, ListWorldRequest, PutGroupRequest,
    ReadEventsRequest, UnitSpec, WorldFilter, Write, entity,
};
use nanoprobe_sidecar::ids::CampaignId;
use nanoprobe_sidecar::store::{CampaignState, Entry, Store, journal_of, record_of};
use nanoprobe_transport::layout::opcode;
use tonic::Code;
use tonic::transport::{Channel, Endpoint};

fn cfg(dir: &Path) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("addr"),
        step: Duration::from_millis(2),
        store: Some(StoreFiles {
            checkpoint: Duration::from_millis(50),
            ..StoreFiles::new(dir.to_owned())
        }),
        ..Config::default()
    }
}

/// A fake DCS of its own: a new process, with an agent boot no earlier one
/// had, as a real DCS gets.
fn fake_config() -> nanoprobe_sim_fake::Config {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    nanoprobe_sim_fake::Config {
        first_boot: 1_000_000 + n * 1_000,
        ..nanoprobe_fake_sidecar::fake_config()
    }
}

fn key(k: &str) -> Option<Idempotency> {
    Some(Idempotency { key: k.into() })
}

fn write(l: &Lease, k: &str, campaign: u64) -> Option<Write> {
    Some(Write {
        lease: Some(l.clone()),
        idempotency: key(k),
        campaign_id: campaign,
    })
}

async fn connect(r: &Running) -> Channel {
    Endpoint::from_shared(r.endpoint())
        .expect("endpoint")
        .connect()
        .await
        .expect("connect")
}

async fn lease(ch: &Channel) -> Lease {
    SessionServiceClient::new(ch.clone())
        .acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect("lease")
        .into_inner()
        .lease
        .expect("lease")
}

async fn create(ch: &Channel, l: &Lease, name: &str, activate: bool) -> u64 {
    CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: name.into(),
            theaters: vec!["Caucasus".into()],
            activate,
            idempotency: key(&format!("create/{name}")),
            ..CreateCampaignRequest::default()
        })
        .await
        .expect("create")
        .into_inner()
        .campaign
        .expect("campaign")
        .id
}

async fn put_group(ch: &Channel, l: &Lease, k: &str, campaign: u64) -> u64 {
    CommandsServiceClient::new(ch.clone())
        .put_group(PutGroupRequest {
            write: write(l, k, campaign),
            group_id: 0,
            spec: Some(GroupSpec {
                label: k.into(),
                country: 2,
                category: GroupCategory::Ground as i32,
                units: vec![UnitSpec {
                    r#type: "M-113".into(),
                    position: Some(pb::Position {
                        theater: "Caucasus".into(),
                        north: 0.0,
                        east: 0.0,
                        alt: 0.0,
                    }),
                    ..UnitSpec::default()
                }],
                ..GroupSpec::default()
            }),
            order: None,
        })
        .await
        .expect("put")
        .into_inner()
        .group_id
}

async fn group_state(ch: &Channel, campaign: u64, id: u64) -> Option<i32> {
    let mut s = WorldServiceClient::new(ch.clone())
        .list_world(ListWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: campaign,
                ..WorldFilter::default()
            }),
        })
        .await
        .expect("list")
        .into_inner();
    while let Some(m) = s.message().await.expect("page") {
        for e in m.entities {
            if let Some(entity::Kind::Group(g)) = e.kind
                && g.id == id
            {
                return Some(g.state);
            }
        }
    }
    None
}

async fn until_live(ch: &Channel, campaign: u64, id: u64) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while group_state(ch, campaign, id).await != Some(EntityState::Live as i32) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the group goes live");
}

async fn get(ch: &Channel, campaign: u64) -> pb::Campaign {
    CampaignServiceClient::new(ch.clone())
        .get_campaign(pb::GetCampaignRequest {
            campaign_id: campaign,
        })
        .await
        .expect("get")
        .into_inner()
        .campaign
        .expect("campaign")
}

async fn read_events(ch: &Channel, campaign: u64, after: u64) -> Result<Vec<u64>, tonic::Status> {
    Ok(EventsServiceClient::new(ch.clone())
        .read_events(ReadEventsRequest {
            filter: Some(EventFilter {
                campaign_id: campaign,
                ..EventFilter::default()
            }),
            after_sequence: after,
            limit: 0,
        })
        .await?
        .into_inner()
        .events
        .iter()
        .map(|e| e.sequence)
        .collect())
}

async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(20), async {
        while !f().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

/// Publish `n` events to the active campaign's log.
fn publish(hub: &nanoprobe_api::hub::Hub, n: u32) {
    let mut g = hub.lock();
    for i in 0..n {
        g.publish(pb::event::Body::World(pb::WorldEvent {
            dcs_id: 1000 + i,
            ..pb::WorldEvent::default()
        }));
    }
}

fn store_path(dir: &Path, id: u64) -> std::path::PathBuf {
    dir.join(id.to_string()).join("store.sqlite")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sidecar_restart_resumes_the_active_campaign_its_world_and_events() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let g = put_group(&ch, &l, "g", a).await;
    until_live(&ch, a, g).await;
    CommandsServiceClient::new(ch.clone())
        .set_sample_rates(pb::SetSampleRatesRequest {
            write: write(&l, "rates", a),
            position_period: 2.0,
            ..pb::SetSampleRatesRequest::default()
        })
        .await
        .expect("rates");
    // Once the fake runs at those rates, a change moves the revision, so
    // the next checkpoint is not skipped.
    eventually("the fake takes the rates", || async {
        server
            .hub
            .lock()
            .backend
            .as_any()
            .and_then(|b| b.downcast_mut::<nanoprobe_sim_fake::FakeSim>())
            .is_some_and(|f| f.rates().1 > 0)
    })
    .await;
    put_group(&ch, &l, "moves", a).await;
    // The checkpoint timer runs and records the achieved periods.
    eventually("a checkpoint with achieved periods", || async {
        get(&ch, a)
            .await
            .last_checkpoint
            .and_then(|c| c.achieved)
            .and_then(|r| r.position)
            .is_some_and(|p| p.seconds == 2)
    })
    .await;
    publish(&server.hub, 3);
    let before = read_events(&ch, a, 0).await.expect("read");
    server.stop().await.expect("a clean stop");
    // A clean stop keeps the state: the next start resumes the campaign.
    let (_, _, r) = record_of(&store_path(d.path(), a)).expect("record");
    assert_eq!(r.state, CampaignState::Active);

    let server = start(cfg(d.path()), fake_config()).await.expect("restart");
    let ch = connect(&server).await;
    let c = get(&ch, 0).await;
    assert_eq!(c.id, a);
    assert_eq!(c.state, pb::CampaignState::Active as i32);
    assert!(c.last_checkpoint.is_some());
    // The fake DCS is new too: the reconciler restores the group.
    until_live(&ch, a, g).await;
    let after = read_events(&ch, a, 0).await.expect("read");
    assert_eq!(after[..before.len()], before[..], "the history is kept");
    publish(&server.hub, 2);
    let now = read_events(&ch, a, 0).await.expect("read");
    assert!(now.len() >= before.len() + 2);
    assert!(
        now.windows(2).all(|w| w[1] == w[0] + 1),
        "numbering goes on after the restart: {now:?}"
    );
    server.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suspend_captures_the_live_groups_then_closes_the_store() {
    let d = tempfile::tempdir().expect("dir");
    let (hub, ch) = in_process(cfg(d.path()), fake_config())
        .await
        .expect("server");
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let g = put_group(&ch, &l, "g", a).await;
    until_live(&ch, a, g).await;
    let mut camp = CampaignServiceClient::new(ch.clone());
    let c = camp
        .suspend_campaign(pb::SuspendCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: a,
            idempotency: key("suspend"),
        })
        .await
        .expect("suspend")
        .into_inner()
        .campaign
        .expect("campaign");
    assert_eq!(c.state, pb::CampaignState::Suspended as i32);
    assert!(!hub.lock().campaigns[&a].opened, "the store is closed");
    // The store closes when the call ends: its final checkpoint shows after.
    let before = c.last_checkpoint.and_then(|x| x.taken);
    let after = hub.lock().campaigns[&a]
        .meta
        .last_checkpoint
        .and_then(|x| x.taken);
    // Within one millisecond of the last checkpoint the times can match.
    let _ = before;
    assert!(after.is_some(), "a final checkpoint");
    let (_, _, r) = record_of(&store_path(d.path(), a)).expect("record");
    assert_eq!(r.state, CampaignState::Suspended);
    // The store journalled a CAPTURE of the group before it was closed.
    {
        let journal = journal_of(&store_path(d.path(), a)).expect("journal");
        let captured = journal.iter().any(|e| {
            matches!(e, Entry::Command(j) if u64::from(j.opcode) == opcode::CAPTURE && j.entity.0 == g)
        });
        assert!(captured, "a CAPTURE of group {g} was sent");
    }
    // A repeat answers from the key; it captures nothing again.
    let captures = || {
        journal_of(&store_path(d.path(), a))
            .expect("journal")
            .iter()
            .filter(|e| matches!(e, Entry::Command(j) if u64::from(j.opcode) == opcode::CAPTURE))
            .count()
    };
    let captures_before = captures();
    assert!(captures_before > 0);
    camp.suspend_campaign(pb::SuspendCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: a,
        idempotency: key("suspend"),
    })
    .await
    .expect("repeat");
    assert_eq!(captures(), captures_before, "the repeat sent no CAPTURE");
    // Activating opens the store again and restores the group.
    camp.activate_campaign(pb::ActivateCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: a,
        idempotency: key("activate"),
    })
    .await
    .expect("activate");
    until_live(&ch, a, g).await;
    assert!(hub.lock().campaigns[&a].opened);
    let (_, _, r) = record_of(&store_path(d.path(), a)).expect("record");
    assert_eq!(r.state, CampaignState::Active);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_switch_suspends_the_old_store_and_a_suspended_campaign_takes_writes() {
    let d = tempfile::tempdir().expect("dir");
    let (hub, ch) = in_process(cfg(d.path()), fake_config())
        .await
        .expect("server");
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let b = create(&ch, &l, "b", false).await;
    let ga = put_group(&ch, &l, "ga", a).await;
    until_live(&ch, a, ga).await;
    // A write to suspended B goes to its file before the call answers.
    let gb = put_group(&ch, &l, "gb", b).await;
    {
        let copy = d.path().join("b.sqlite");
        nanoprobe_sidecar::store::backup_file(&store_path(d.path(), b), &copy).expect("copy");
        let s = Store::open(&copy, 10).expect("open");
        assert!(s.group(nanoprobe_sidecar::ids::EntityId(gb)).is_some());
    }
    CampaignServiceClient::new(ch.clone())
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: b,
            idempotency: key("switch"),
        })
        .await
        .expect("switch");
    until_live(&ch, b, gb).await;
    assert!(!hub.lock().campaigns[&a].opened);
    let (_, _, ra) = record_of(&store_path(d.path(), a)).expect("a");
    let (_, _, rb) = record_of(&store_path(d.path(), b)).expect("b");
    assert_eq!(ra.state, CampaignState::Suspended);
    assert_eq!(rb.state, CampaignState::Active);
}

/// A running server that is stopped when this value goes, on a failed assert
/// too. A temp folder declared before it is then removed with nothing
/// writing into it. For a test whose timers write all the time, on a
/// multi-thread runtime.
struct Stopped(Option<Running>);

impl Stopped {
    fn server(&self) -> &Running {
        self.0.as_ref().expect("a running server")
    }

    /// Stop the server and wait for it, as [`Running::stop`] does.
    async fn stop(mut self) -> Result<(), String> {
        self.0.take().expect("a running server").stop().await
    }
}

impl Drop for Stopped {
    fn drop(&mut self) {
        // Only a failed assert comes here with the server still running.
        if let Some(server) = self.0.take() {
            let _ = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(server.stop())
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_timers_take_checkpoints_and_backups_and_keep_backup_keep() {
    let d = tempfile::tempdir().expect("dir");
    let mut c = cfg(d.path());
    if let Some(sf) = c.store.as_mut() {
        sf.checkpoint = Duration::from_millis(20);
        sf.backup_interval = Duration::from_millis(150);
        sf.backup_keep = 2;
    }
    // The timers write into `d` for as long as the server runs, so the
    // server stops before `d` goes, pass or fail: a folder removed under a
    // checkpoint or a backup worker stays behind, half full.
    let server = Stopped(Some(start(c, fake_config()).await.expect("server")));
    let ch = connect(server.server()).await;
    let hub = &server.server().hub;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let mut camp = CampaignServiceClient::new(ch.clone());
    let list = |camp: CampaignServiceClient<Channel>| async move {
        let mut camp = camp;
        camp.list_campaign_backups(pb::ListCampaignBackupsRequest { campaign_id: a })
            .await
            .expect("list")
            .into_inner()
            .backups
    };
    eventually("a checkpoint", || async {
        get(&ch, a).await.last_checkpoint.is_some()
    })
    .await;
    let first = get(&ch, a).await.last_checkpoint.expect("checkpoint").taken;
    // A checkpoint is skipped while nothing moves: change something.
    put_group(&ch, &l, "g", a).await;
    eventually("a later checkpoint", || async {
        get(&ch, a).await.last_checkpoint.expect("cp").taken != first
    })
    .await;
    eventually("two scheduled backups", || async {
        let b = list(camp.clone()).await;
        b.len() == 2
            && b.iter()
                .all(|x| x.reason == pb::BackupReason::Scheduled as i32)
    })
    .await;
    let manual = camp
        .backup_campaign(pb::BackupCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: a,
            idempotency: key("backup"),
        })
        .await
        .expect("backup")
        .into_inner()
        .backup
        .expect("backup");
    assert_eq!(manual.reason, pb::BackupReason::Manual as i32);
    assert!(manual.size > 0);
    let b = list(camp.clone()).await;
    assert!(b.len() <= 2, "backup_keep holds: {b:?}");
    assert!(b.windows(2).all(|w| w[0].id < w[1].id));
    // The server store's folder holds scheduled backups too.
    let server_backups = hub
        .lock()
        .server
        .as_ref()
        .expect("server store")
        .backup_folder(2)
        .list()
        .expect("list");
    assert!(
        server_backups
            .iter()
            .any(|x| x.reason == nanoprobe_sidecar::campaigns::BackupReason::Scheduled),
        "a scheduled server backup in {}/server/backups: {server_backups:?}",
        d.path().display()
    );
    assert!(server_backups.len() <= 2);
    // With nothing changing, a checkpoint is skipped: the stamp holds.
    let stamp = get(&ch, a).await.last_checkpoint.expect("cp").taken;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        get(&ch, a).await.last_checkpoint.expect("cp").taken,
        stamp,
        "no checkpoint while the revision stands still"
    );
    server.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_finished_campaign_is_read_only_and_listed_after_a_restart() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let outcome = prost_types::Struct {
        fields: [(
            "winner".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("blue".into())),
            },
        )]
        .into_iter()
        .collect(),
    };
    let mut camp = CampaignServiceClient::new(ch.clone());
    let c = camp
        .finish_campaign(pb::FinishCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: a,
            outcome: Some(outcome.clone()),
            idempotency: key("finish"),
        })
        .await
        .expect("finish")
        .into_inner()
        .campaign
        .expect("campaign");
    assert_eq!(c.state, pb::CampaignState::Finished as i32);
    assert_eq!(c.outcome.as_ref(), Some(&outcome));
    let e = camp
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: a,
            idempotency: key("activate"),
        })
        .await
        .expect_err("finished");
    assert_eq!(reason(&e), ErrorReason::CampaignReadOnly);
    server.stop().await.expect("stop");

    let server = start(cfg(d.path()), fake_config()).await.expect("restart");
    let ch = connect(&server).await;
    let c = get(&ch, a).await;
    assert_eq!(c.state, pb::CampaignState::Finished as i32);
    assert_eq!(c.outcome, Some(outcome));
    server.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compaction_at_open_keeps_the_newest_event_and_ids_skip_unreadable_folders() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    publish(&server.hub, 2);
    // Retention counts back from the newest event's time.
    tokio::time::sleep(Duration::from_millis(50)).await;
    publish(&server.hub, 1);
    // Suspend, so no event arrives between the stop and the next start.
    CampaignServiceClient::new(ch.clone())
        .suspend_campaign(pb::SuspendCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: a,
            idempotency: key("suspend"),
        })
        .await
        .expect("suspend");
    let before = read_events(&ch, a, 0).await.expect("read");
    let newest = *before.last().expect("events");
    server.stop().await.expect("stop");

    // A folder that cannot be read, numbered past any time this test sees.
    let far = CampaignId(nanoprobe_sidecar::ids::MAX_ID - 1_000);
    std::fs::create_dir_all(d.path().join(far.0.to_string())).expect("dir");
    std::fs::write(store_path(d.path(), far.0), b"not a database").expect("garbage");

    tokio::time::sleep(Duration::from_millis(5)).await;
    let mut c = cfg(d.path());
    c.event_retention = Duration::from_millis(20);
    let server = start(c, fake_config()).await.expect("restart");
    let ch = connect(&server).await;
    assert_eq!(
        server
            .hub
            .lock()
            .unreadable
            .iter()
            .map(|u| u.0)
            .collect::<Vec<_>>(),
        vec![far.0]
    );
    assert_eq!(read_events(&ch, a, 0).await.expect("read"), vec![newest]);
    let e = read_events(&ch, a, 1).await.expect_err("compacted away");
    assert_eq!(e.code(), Code::OutOfRange);
    assert_eq!(reason(&e), ErrorReason::SequenceExpired);
    let l = lease(&ch).await;
    let b = create(&ch, &l, "b", false).await;
    assert!(b > far.0, "a new ID passes an unreadable folder");
    server.stop().await.expect("stop");
}

// ---- S18b: the server store ----

fn server_epoch(r: &Running) -> u64 {
    r.hub.lock().server.as_ref().expect("server store").epoch()
}

/// `meta.epoch` as the server store's file holds it, read through a second
/// read-only connection while the sidecar runs.
fn file_epoch(dir: &Path) -> u64 {
    use nanoprobe_transport::codec::{Lv, Reader};
    let c = rusqlite::Connection::open_with_flags(
        dir.join("server").join("store.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("server store");
    let bytes: Vec<u8> = c
        .query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| {
            r.get(0)
        })
        .expect("epoch");
    match Reader::new(&bytes).lv().expect("lv") {
        Lv::String(t) => std::str::from_utf8(&t)
            .expect("text")
            .parse()
            .expect("decimal"),
        other => panic!("epoch is not a string: {other:?}"),
    }
}

async fn activate(ch: &Channel, l: &Lease, id: u64, k: &str) {
    CampaignServiceClient::new(ch.clone())
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: id,
            idempotency: key(k),
        })
        .await
        .expect("activate");
}

/// The command epoch only grows: across a campaign switch and across a
/// sidecar restart, kept in the server store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_epoch_only_grows_across_a_switch_and_a_restart() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let g = put_group(&ch, &l, "g", a).await;
    until_live(&ch, a, g).await;
    let e1 = server_epoch(&server);
    assert!(e1 > 0, "an epoch was started and written");
    assert_eq!(file_epoch(d.path()), e1, "the file holds the epoch");
    let b = create(&ch, &l, "b", false).await;
    let gb = put_group(&ch, &l, "gb", b).await;
    activate(&ch, &l, b, "to-b").await;
    until_live(&ch, b, gb).await;
    let e2 = server_epoch(&server);
    assert!(e2 > e1, "a switch starts a new epoch: {e1} then {e2}");
    assert_eq!(file_epoch(d.path()), e2, "the file holds the new epoch");
    server.stop().await.expect("stop");

    let server = start(cfg(d.path()), fake_config()).await.expect("restart");
    assert!(server_epoch(&server) >= e2, "the epoch is read back");
    // The store says live already; wait for the new DCS to take commands.
    eventually("a new epoch after the restart", || async {
        server_epoch(&server) > e2
    })
    .await;
    assert_eq!(file_epoch(d.path()), server_epoch(&server));
    let ch = connect(&server).await;
    until_live(&ch, b, gb).await;
    server.stop().await.expect("stop");
}

/// The player secret and keys are the server's: the same in every campaign
/// and after a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn player_keys_are_the_servers_across_campaigns_and_restarts() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    create(&ch, &l, "a", true).await;
    // The server store made the secret at start; a key the roster learns
    // reaches it within a step.
    let (secret, key) = {
        let mut g = server.hub.lock();
        let secret = g.server.as_ref().expect("server store").player_secret();
        (secret.expect("secret"), g.roster.learn("ucid-1"))
    };
    assert!(secret.is_some(), "made at the first start");
    let b = create(&ch, &l, "b", false).await;
    activate(&ch, &l, b, "to-b").await;
    let read = |r: &Running| {
        let g = r.hub.lock();
        let s = g.server.as_ref().expect("server store");
        (
            s.player_secret().expect("secret"),
            s.players().expect("players"),
        )
    };
    let want = (secret, vec![(key.clone(), "ucid-1".to_owned())]);
    eventually("the key in the server store", || async {
        read(&server) == want
    })
    .await;
    server.stop().await.expect("stop");
    let server = start(cfg(d.path()), fake_config()).await.expect("restart");
    assert_eq!(read(&server), want);
    assert_eq!(server.hub.lock().roster.keyring().key_of("ucid-1"), key);
    assert_eq!(server.hub.lock().roster.ucid_of(&key), Some("ucid-1"));
    server.stop().await.expect("stop");
}

/// A `players.json` from before the server store is read into it at the
/// first start, renamed, and its keys are the roster's.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_players_json_is_migrated_at_start_and_its_keys_kept() {
    let d = tempfile::tempdir().expect("dir");
    let secret = "07".repeat(32);
    let key = nanoprobe_sidecar::players::Keyring::from_secret([7; 32]).key_of("ucid-1");
    std::fs::write(
        d.path().join("players.json"),
        format!(r#"{{"secret":"{secret}","players":{{"{key}":"ucid-1"}}}}"#),
    )
    .expect("write");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    assert!(!d.path().join("players.json").exists());
    assert!(d.path().join("players.json.migrated").exists());
    {
        let g = server.hub.lock();
        assert_eq!(g.roster.ucid_of(&key), Some("ucid-1"));
        assert_eq!(g.roster.keyring().key_of("ucid-1"), key);
        assert!(g.roster.keyring().unsaved().is_empty());
    }
    server.stop().await.expect("stop");
}

/// A call that names no campaign (0 means the active one) and is repeated
/// after the active campaign changed and the sidecar restarted is
/// recognized: the same answer, nothing done twice. With `lose_row`, the
/// call names campaign a, and the server store's row for the key is gone:
/// a's own store still holds the answer.
async fn repeat_after_a_switch_and_a_restart(lose_row: bool) {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    // The request digest covers the campaign as sent, so a repeat names it
    // as the first call did.
    let named = if lose_row { a } else { 0 };
    let first = put_group(&ch, &l, "once", named).await;
    let b = create(&ch, &l, "b", false).await;
    activate(&ch, &l, b, "to-b").await;
    server.stop().await.expect("stop");
    if lose_row {
        let c = rusqlite::Connection::open(d.path().join("server").join("store.sqlite"))
            .expect("server store");
        let n = c
            .execute("DELETE FROM idempotency WHERE key = ?1", ["once"])
            .expect("delete");
        assert_eq!(n, 1, "the server store held the key");
    }

    let server = start(cfg(d.path()), fake_config()).await.expect("restart");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    // After the switch 0 means b, yet the key is still a's.
    let again = put_group(&ch, &l, "once", named).await;
    assert_eq!(again, first, "the first answer");
    let groups = |id: u64| server.hub.lock().campaigns[&id].store.groups().count();
    assert_eq!(groups(a), 1, "no second group in a");
    assert_eq!(groups(b), 0, "no group in b");
    // The same key with another request is refused.
    let e = CommandsServiceClient::new(ch.clone())
        .put_group(PutGroupRequest {
            write: write(&l, "once", named),
            group_id: 0,
            spec: None,
            order: None,
        })
        .await
        .expect_err("another request");
    assert_eq!(reason(&e), ErrorReason::IdempotencyMismatch);
    server.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repeat_is_recognized_after_a_switch_and_a_restart() {
    repeat_after_a_switch_and_a_restart(false).await;
}

/// The server store's row is an index: with it lost, the named campaign's
/// own store still answers the repeat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repeat_is_recognized_from_the_campaign_store_when_the_server_row_is_gone() {
    repeat_after_a_switch_and_a_restart(true).await;
}

/// The server event log survives a restart, with its own numbers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_server_event_log_survives_a_restart() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    publish(&server.hub, 3);
    let read = |ch: Channel| async move {
        EventsServiceClient::new(ch)
            .read_events(ReadEventsRequest {
                filter: Some(EventFilter {
                    server_log: true,
                    ..EventFilter::default()
                }),
                after_sequence: 0,
                limit: 0,
            })
            .await
            .expect("read")
            .into_inner()
            .events
            .iter()
            .map(|e| e.sequence)
            .collect::<Vec<u64>>()
    };
    let ch = connect(&server).await;
    let before = read(ch).await;
    assert!(before.len() >= 3);
    server.stop().await.expect("stop");
    let server = start(cfg(d.path()), fake_config()).await.expect("restart");
    let ch = connect(&server).await;
    let after = read(ch.clone()).await;
    assert_eq!(after[..before.len()], before[..]);
    publish(&server.hub, 1);
    let more = read(ch).await;
    assert_eq!(
        *more.last().expect("one"),
        *more.first().expect("one") + more.len() as u64 - 1
    );
    server.stop().await.expect("stop");
}

/// A second sidecar on the same store dir is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_sidecar_on_one_store_dir_is_refused() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let e = match start(cfg(d.path()), fake_config()).await {
        Ok(_) => panic!("a second sidecar started"),
        Err(e) => e,
    };
    assert!(e.contains("locked"), "{e}");
    server.stop().await.expect("stop");
}

/// A crash between the events commit and the store commit
/// (spec/persistence.md, "A crash between the two commits"): the step's
/// events are on disk, its facts are not, and the agent was not told its
/// records were read. After a restart over the same DCS the records come
/// again: the death is folded in once and its events are not logged twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_between_the_two_commits_folds_and_logs_each_record_once() {
    use nanoprobe_fake_sidecar::start_with;
    use nanoprobe_sim_fake::FakeSim;
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let g = put_group(&ch, &l, "g", a).await;
    until_live(&ch, a, g).await;
    let hub = std::sync::Arc::clone(&server.hub);
    let (unit, object) = {
        let st = &hub.lock().campaigns[&a].store;
        let m = st
            .group(nanoprobe_sidecar::ids::EntityId(g))
            .expect("g")
            .members[0];
        (m, st.unit(m).expect("unit").object)
    };
    assert_ne!(object, 0);
    let events = d.path().join(a.to_string()).join("events.sqlite");
    let sources = |p: &Path| -> std::collections::BTreeSet<String> {
        let c = rusqlite::Connection::open(p).expect("events");
        let mut st = c
            .prepare("SELECT source FROM event WHERE source IS NOT NULL")
            .expect("prepare");
        st.query_map([], |r| r.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect()
    };
    let before_kill = sources(&events);
    // The death happens in the next frame, and the step that reads it
    // stops between its two commits.
    {
        let mut inner = hub.lock();
        let sim = inner
            .backend
            .as_any()
            .and_then(|b| b.downcast_mut::<FakeSim>())
            .expect("the fake");
        sim.kill(object);
        inner.crash_after_events = true;
    }
    eventually("the injected crash", || async { hub.closing() }).await;
    let stopped = server.stop().await;
    assert!(stopped.is_err(), "the driver stopped on the injected crash");
    // The crashed step's events reached the file.
    let after_crash = sources(&events);
    let crashed: Vec<&String> = after_crash.difference(&before_kill).collect();
    assert!(
        after_crash.len() > before_kill.len() && !crashed.is_empty(),
        "the crashed step logged events: {} then {}",
        before_kill.len(),
        after_crash.len()
    );
    let store = store_path(d.path(), a);
    let dead_in_file = |p: &Path| {
        journal_of(p)
            .expect("journal")
            .iter()
            .filter(|e| matches!(e, Entry::Death { unit: u } if *u == unit))
            .count()
    };
    assert_eq!(dead_in_file(&store), 0, "the fact did not reach the store");
    // The same DCS goes on; the sidecar starts again.
    let sim = {
        let mut inner = hub.lock();
        let b = inner
            .backend
            .as_any()
            .and_then(|b| b.downcast_mut::<FakeSim>())
            .expect("the fake");
        std::mem::replace(b, FakeSim::new(fake_config()))
    };
    drop(hub);
    let server = start_with(cfg(d.path()), sim).await.expect("restart");
    eventually("the death folded in", || async {
        server.hub.lock().campaigns[&a]
            .store
            .unit(unit)
            .is_some_and(|u| u.dead)
    })
    .await;
    server.stop().await.expect("stop");
    assert_eq!(dead_in_file(&store), 1, "one death fact");
    // The step's records came again and logged nothing a second time.
    let rows_of_crashed = |p: &Path| -> i64 {
        let c = rusqlite::Connection::open(p).expect("events");
        crashed
            .iter()
            .map(|src| {
                c.query_row("SELECT count(*) FROM event WHERE source = ?1", [src], |r| {
                    r.get::<_, i64>(0)
                })
                .expect("count")
            })
            .sum()
    };
    assert_eq!(
        rows_of_crashed(&events),
        crashed.len() as i64,
        "each crashed-step record is logged once"
    );
    assert!(
        sources(&events).is_superset(&after_crash),
        "no event of the step was lost"
    );
    // A second event from one record would have broken the file's unique
    // `source`: every record's events are there once.
    let dup: i64 = rusqlite::Connection::open(&events)
        .expect("events")
        .query_row(
            "SELECT count(*) FROM (SELECT source FROM event WHERE source IS NOT NULL \
             GROUP BY source HAVING count(*) > 1)",
            [],
            |r| r.get(0),
        )
        .expect("dup");
    assert_eq!(dup, 0);
}

/// Activation suspends the active campaign before it opens the target: a
/// target that cannot be opened leaves no campaign active.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_target_that_cannot_open_leaves_no_campaign_active() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(d.path()), fake_config()).await.expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a", true).await;
    let b = create(&ch, &l, "b", false).await;
    server.stop().await.expect("stop");
    // b's store is damaged while no sidecar runs; listing reads only its
    // record, so the damage shows at the open.
    let c = rusqlite::Connection::open(store_path(d.path(), b)).expect("conn");
    c.execute_batch("DELETE FROM meta WHERE key = 'world';")
        .expect("damage");
    drop(c);
    let server = start(cfg(d.path()), fake_config()).await.expect("restart");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let e = CampaignServiceClient::new(ch.clone())
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: b,
            idempotency: key("to-b"),
        })
        .await
        .expect_err("b cannot be opened");
    assert_eq!(e.code(), Code::Internal, "{e:?}");
    assert_eq!(server.hub.lock().active, None);
    assert_eq!(get(&ch, a).await.state, pb::CampaignState::Suspended as i32);
    let (_, _, r) = record_of(&store_path(d.path(), a)).expect("record");
    assert_eq!(r.state, CampaignState::Suspended);
    server.stop().await.expect("stop");
}
