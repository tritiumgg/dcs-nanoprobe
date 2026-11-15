//! The API over campaign stores on disk (spec/persistence.md): a sidecar
//! restart, suspend with capture, finish, the checkpoint and backup timers,
//! and the Events service over `history.log` with compaction.

use std::path::Path;
use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_fake_sidecar::{Running, fake_config, in_process, start};
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
use nanoprobe_sidecar::store::{CampaignState, Entry, Store, record_of};
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
    assert!(c.last_checkpoint.is_some(), "a final checkpoint");
    assert!(!hub.lock().campaigns[&a].opened, "the store is closed");
    let (_, _, r) = record_of(&store_path(d.path(), a)).expect("record");
    assert_eq!(r.state, CampaignState::Suspended);
    // The store journalled a CAPTURE of the group before it was closed.
    {
        let copy = d.path().join("copy.sqlite");
        nanoprobe_sidecar::store::backup_file(&store_path(d.path(), a), &copy).expect("copy");
        let s = Store::open(&copy, 10).expect("open the copy");
        let captured = s.entries().iter().any(|e| {
            matches!(e, Entry::Command(j) if u64::from(j.opcode) == opcode::CAPTURE && j.entity.0 == g)
        });
        assert!(captured, "a CAPTURE of group {g} was sent");
    }
    // A repeat answers from the key; it captures nothing again.
    camp.suspend_campaign(pb::SuspendCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: a,
        idempotency: key("suspend"),
    })
    .await
    .expect("repeat");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_timers_take_checkpoints_and_backups_and_keep_backup_keep() {
    let d = tempfile::tempdir().expect("dir");
    let mut c = cfg(d.path());
    if let Some(sf) = c.store.as_mut() {
        sf.checkpoint = Duration::from_millis(20);
        sf.backup_interval = Duration::from_millis(150);
        sf.backup_keep = 2;
    }
    let (_hub, ch) = in_process(c, fake_config()).await.expect("server");
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
    publish(&server.hub, 3);
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
    let far = CampaignId(u64::MAX / 4);
    std::fs::create_dir_all(d.path().join(far.0.to_string())).expect("dir");
    std::fs::write(store_path(d.path(), far.0), b"not a database").expect("garbage");

    tokio::time::sleep(Duration::from_millis(5)).await;
    let mut c = cfg(d.path());
    if let Some(sf) = c.store.as_mut() {
        sf.event_retention = Duration::from_millis(1);
    }
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
