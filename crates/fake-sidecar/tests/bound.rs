//! The bounds on what a campaign keeps, through the API: a removal reaches a
//! watch and is purged, a dead group expires and a ruin never does, and
//! `GetServerStatus` reports the store's size (spec/world.md, "Removal" and
//! "Expiry"; spec/persistence.md, "Size").

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityState, GroupCategory, GroupSpec,
    Idempotency, Lease, ListWorldRequest, PutGroupRequest, RemovalCause, UnitSpec,
    WatchWorldRequest, WorldFilter, Write, entity, world_change,
};
use nanoprobe_sidecar::store::Entry;
use nanoprobe_sim_fake::FakeSim;
use nanoprobe_transport::layout::opcode;
use tonic::Code;
use tonic::transport::Channel;

fn cfg(dir: Option<&Path>) -> Config {
    cfg_keep(dir, Duration::ZERO)
}

/// As [`cfg`], with a removed entity kept for `removed_keep` before its purge.
fn cfg_keep(dir: Option<&Path>, removed_keep: Duration) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("addr"),
        step: Duration::from_millis(2),
        removed_keep,
        dead_keep: Duration::from_millis(300),
        store: dir.map(|d| StoreFiles {
            checkpoint: Duration::from_millis(50),
            ..StoreFiles::new(d.to_owned())
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

fn at(north: f64) -> Option<pb::Position> {
    Some(pb::Position {
        theater: "Caucasus".into(),
        north,
        east: 0.0,
        alt: 0.0,
    })
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

async fn create(ch: &Channel, l: &Lease, name: &str) -> u64 {
    CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: name.into(),
            theaters: vec!["Caucasus".into()],
            activate: true,
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

/// A group of two units; returns the group's ID.
async fn put_group(ch: &Channel, l: &Lease, k: &str, campaign: u64, north: f64) -> u64 {
    let unit = |n: f64| UnitSpec {
        r#type: "M-113".into(),
        position: at(n),
        ..UnitSpec::default()
    };
    CommandsServiceClient::new(ch.clone())
        .put_group(PutGroupRequest {
            write: write(l, k, campaign),
            group_id: 0,
            spec: Some(GroupSpec {
                label: k.into(),
                country: 2,
                category: GroupCategory::Ground as i32,
                units: vec![unit(north), unit(north + 10.0)],
                ..GroupSpec::default()
            }),
            order: None,
        })
        .await
        .expect("put")
        .into_inner()
        .group_id
}

async fn put_static(
    ch: &Channel,
    l: &Lease,
    k: &str,
    campaign: u64,
    north: f64,
    dead: bool,
) -> u64 {
    CommandsServiceClient::new(ch.clone())
        .put_static(pb::PutStaticRequest {
            write: write(l, k, campaign),
            static_id: 0,
            spec: Some(pb::StaticSpec {
                label: k.into(),
                country: 2,
                r#type: "Tent".into(),
                position: at(north),
                heading: 0.5,
                extra: None,
                dead,
            }),
        })
        .await
        .expect("put")
        .into_inner()
        .static_id
}

fn whole(campaign: u64) -> WorldFilter {
    WorldFilter {
        campaign_id: campaign,
        ..WorldFilter::default()
    }
}

/// The IDs of the groups and statics `ListWorld` lists, with their states.
async fn listed(ch: &Channel, campaign: u64) -> Vec<(u64, i32)> {
    let mut s = WorldServiceClient::new(ch.clone())
        .list_world(ListWorldRequest {
            filter: Some(whole(campaign)),
        })
        .await
        .expect("list")
        .into_inner();
    let mut out = Vec::new();
    while let Some(m) = s.message().await.expect("page") {
        for e in m.entities {
            match e.kind {
                Some(entity::Kind::Group(g)) => out.push((g.id, g.state)),
                Some(entity::Kind::Static(x)) => out.push((x.id, x.state)),
                _ => {}
            }
        }
    }
    out
}

async fn state_of(ch: &Channel, campaign: u64, id: u64) -> Option<i32> {
    listed(ch, campaign)
        .await
        .into_iter()
        .find(|(i, _)| *i == id)
        .map(|(_, s)| s)
}

async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(15), async {
        while !f().await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {what}"));
}

async fn until_state(ch: &Channel, campaign: u64, id: u64, want: EntityState) {
    eventually(&format!("{id} is {want:?}"), || async {
        state_of(ch, campaign, id).await == Some(want as i32)
    })
    .await;
}

async fn size(ch: &Channel) -> pb::StoreSize {
    MissionServiceClient::new(ch.clone())
        .get_server_status(pb::GetServerStatusRequest {})
        .await
        .expect("status")
        .into_inner()
        .status
        .expect("status")
        .store_size
        .expect("store_size")
}

async fn until_size(ch: &Channel, what: &str, f: impl Fn(&pb::StoreSize) -> bool) {
    eventually(what, || async { f(&size(ch).await) }).await;
}

/// One watch's changes, collected by a task until the test ends.
type Seen = Arc<Mutex<Vec<world_change::Change>>>;

async fn watch(ch: &Channel, campaign: u64) -> Seen {
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            filter: Some(whole(campaign)),
            since_revision: 0,
            rate: pb::RateClass::Slow as i32,
            min_period: None,
        })
        .await
        .expect("watch")
        .into_inner();
    let seen: Seen = Arc::default();
    let sink = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok(Some(m)) = w.message().await {
            let mut g = sink.lock().expect("sink");
            g.extend(m.changes.into_iter().filter_map(|c| c.change));
        }
    });
    seen
}

fn removal_of(seen: &Seen, id: u64) -> Option<i32> {
    seen.lock().expect("seen").iter().find_map(|c| match c {
        world_change::Change::Removal(r) if r.id == id => Some(r.cause),
        _ => None,
    })
}

/// Whether the watch got an upsert of the group before its removal, and the
/// last such upsert's units, none alive when `dead`.
fn upsert_before_removal(seen: &Seen, id: u64) -> Option<pb::Group> {
    let g = seen.lock().expect("seen");
    let cut = g
        .iter()
        .position(|c| matches!(c, world_change::Change::Removal(r) if r.id == id))?;
    g[..cut].iter().rev().find_map(|c| match c {
        world_change::Change::Upsert(pb::Entity {
            kind: Some(entity::Kind::Group(x)),
        }) if x.id == id => Some(x.clone()),
        _ => None,
    })
}

fn objects_of(hub: &Hub, campaign: u64, ids: &[u64]) -> usize {
    let mut g = hub.lock();
    let sim = g
        .backend
        .as_any()
        .and_then(|a| a.downcast_mut::<FakeSim>())
        .expect("the fake sim");
    sim.objects()
        .into_iter()
        .filter(|(_, c, e)| *c == campaign && ids.contains(e))
        .count()
}

/// Kill every unit of the group in the fake DCS.
fn kill_all(hub: &Hub, campaign: u64, units: &[u64]) {
    let mut g = hub.lock();
    let sim = g
        .backend
        .as_any()
        .and_then(|a| a.downcast_mut::<FakeSim>())
        .expect("the fake sim");
    let objects: Vec<u64> = sim
        .objects()
        .into_iter()
        .filter(|(_, c, e)| *c == campaign && units.contains(e))
        .map(|(o, _, _)| o)
        .collect();
    assert_eq!(objects.len(), units.len(), "DCS holds each unit");
    for o in objects {
        sim.kill(o);
    }
}

fn destroys_for(hub: &Hub, campaign: u64, id: u64) -> usize {
    hub.lock().campaigns[&campaign]
        .store
        .entries()
        .iter()
        .filter(|e| {
            matches!(e, Entry::Command(c) if c.entity.0 == id
                && matches!(u64::from(c.opcode), opcode::GROUP_DESTROY | opcode::STATIC_DESTROY))
        })
        .count()
}

async fn unit_ids(ch: &Channel, campaign: u64, id: u64) -> Vec<u64> {
    let mut s = WorldServiceClient::new(ch.clone())
        .list_world(ListWorldRequest {
            filter: Some(whole(campaign)),
        })
        .await
        .expect("list")
        .into_inner();
    while let Some(m) = s.message().await.expect("page") {
        for e in m.entities {
            if let Some(entity::Kind::Group(g)) = e.kind
                && g.id == id
            {
                return g.units.iter().map(|u| u.id).collect();
            }
        }
    }
    panic!("group {id} not listed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_group_reaches_the_watch_then_is_purged() {
    let d = tempfile::tempdir().expect("dir");
    // A keep of seconds, so the watch reads the removal before the purge.
    let (hub, ch) = in_process(
        cfg_keep(Some(d.path()), Duration::from_secs(3)),
        fake_config(),
    )
    .await
    .expect("server");
    let l = lease(&ch).await;
    let c = create(&ch, &l, "rm").await;
    let g = put_group(&ch, &l, "g", c, 0.0).await;
    until_state(&ch, c, g, EntityState::Live).await;
    let seen = watch(&ch, c).await;
    eventually("the watch holds the group", || async {
        seen.lock().expect("seen").iter().any(|ch| {
            matches!(ch, world_change::Change::Upsert(pb::Entity {
                kind: Some(entity::Kind::Group(x)) }) if x.id == g)
        })
    })
    .await;

    let mut cmd = CommandsServiceClient::new(ch.clone());
    let req = pb::RemoveEntityRequest {
        write: write(&l, "remove", c),
        entity_id: g,
    };
    cmd.remove_entity(req.clone()).await.expect("remove");

    eventually("the watch reads the removal", || async {
        removal_of(&seen, g).is_some()
    })
    .await;
    assert_eq!(removal_of(&seen, g), Some(RemovalCause::Controller as i32));
    let last = upsert_before_removal(&seen, g).expect("last state before the removal");
    assert_eq!(last.units.len(), 2);

    until_size(&ch, "the group is purged", |s| s.purged >= 1).await;
    assert_eq!(size(&ch).await.awaiting_purge, 0);
    let e = cmd
        .remove_entity(pb::RemoveEntityRequest {
            write: write(&l, "again", c),
            entity_id: g,
        })
        .await
        .expect_err("a purged ID");
    assert_eq!(e.code(), Code::NotFound);
    assert!(state_of(&ch, c, g).await.is_none(), "ListWorld lists it");
    // The first call's key still gets its first answer.
    cmd.remove_entity(req).await.expect("the keyed repeat");
    // DCS holds nothing of it.
    assert_eq!(objects_of(&hub, c, &[g]), 0);
}

/// A watch that runs only after a group was removed and purged gets
/// `removed_id` for it, not a `Removal`. The server's watch reads at every
/// step, so the test removes and purges in one hold of the hub's lock, as a
/// watch that fell behind by a step would see it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_that_read_late_gets_removed_id_after_the_purge() {
    let d = tempfile::tempdir().expect("dir");
    let (hub, ch) = in_process(cfg(Some(d.path())), fake_config())
        .await
        .expect("server");
    let l = lease(&ch).await;
    let c = create(&ch, &l, "late").await;
    let g = put_group(&ch, &l, "g", c, 0.0).await;
    until_state(&ch, c, g, EntityState::Live).await;
    let seen = watch(&ch, c).await;
    eventually("the watch holds the group", || async {
        seen.lock().expect("seen").iter().any(|ch| {
            matches!(ch, world_change::Change::Upsert(pb::Entity {
                kind: Some(entity::Kind::Group(x)) }) if x.id == g)
        })
    })
    .await;
    {
        let mut inner = hub.lock();
        let store = &mut inner.campaigns.get_mut(&c).expect("campaign").store;
        let id = nanoprobe_sidecar::ids::EntityId(g);
        store.remove_entity(id).expect("remove");
        assert!(store.purge(id).is_some(), "purged");
    }
    eventually("the watch reads removed_id", || async {
        seen.lock()
            .expect("seen")
            .iter()
            .any(|c| matches!(c, world_change::Change::RemovedId(id) if *id == g))
    })
    .await;
    assert!(
        removal_of(&seen, g).is_none(),
        "a Removal for a purged group"
    );
}

/// A group purged while DCS still holds its objects: they are destroyed as
/// drift, which the status counts, and no death of theirs is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_purged_while_dcs_holds_it_is_destroyed_as_drift() {
    let d = tempfile::tempdir().expect("dir");
    let (hub, ch) = in_process(cfg(Some(d.path())), fake_config())
        .await
        .expect("server");
    let l = lease(&ch).await;
    let c = create(&ch, &l, "drift").await;
    let g = put_group(&ch, &l, "g", c, 0.0).await;
    until_state(&ch, c, g, EntityState::Live).await;
    let units = unit_ids(&ch, c, g).await;
    assert_eq!(objects_of(&hub, c, &units), 2, "DCS holds the units");
    {
        let mut inner = hub.lock();
        let store = &mut inner.campaigns.get_mut(&c).expect("campaign").store;
        let id = nanoprobe_sidecar::ids::EntityId(g);
        store.remove_entity(id).expect("remove");
        assert!(store.purge(id).is_some(), "purged");
        // The purge went round the reconciler, which still binds the
        // objects: a restart of the sidecar drops the binding, and its
        // first enumeration finds objects of a name the store lacks.
        inner.restart(&cfg(Some(d.path())));
    }
    until_size(&ch, "the objects are destroyed as drift", |s| {
        s.orphan_destroys >= 1
    })
    .await;
    eventually("DCS holds nothing of it", || async {
        objects_of(&hub, c, &units) == 0
    })
    .await;
    let s = size(&ch).await;
    assert_eq!(s.purged_deaths, 0);
    assert!(state_of(&ch, c, g).await.is_none(), "ListWorld lists it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_group_expires_and_a_ruin_does_not() {
    let d = tempfile::tempdir().expect("dir");
    let (hub, ch) = in_process(cfg(Some(d.path())), fake_config())
        .await
        .expect("server");
    let l = lease(&ch).await;
    let c = create(&ch, &l, "exp").await;
    let g = put_group(&ch, &l, "g", c, 0.0).await;
    let wreck = put_static(&ch, &l, "wreck", c, -500.0, true).await;
    let tent = put_static(&ch, &l, "tent", c, -900.0, false).await;
    until_state(&ch, c, g, EntityState::Live).await;
    until_state(&ch, c, tent, EntityState::Live).await;
    until_state(&ch, c, wreck, EntityState::Dead).await;
    let units = unit_ids(&ch, c, g).await;
    let seen = watch(&ch, c).await;

    // The static is destroyed in DCS: a ruin.
    {
        let mut h = hub.lock();
        let sim = h
            .backend
            .as_any()
            .and_then(|a| a.downcast_mut::<FakeSim>())
            .expect("the fake sim");
        let o = sim
            .objects()
            .into_iter()
            .find(|(_, camp, e)| *camp == c && *e == tent)
            .expect("the static is in DCS")
            .0;
        sim.kill(o);
    }
    until_state(&ch, c, tent, EntityState::Dead).await;

    kill_all(&hub, c, &units);
    eventually("the watch reads the expiry", || async {
        removal_of(&seen, g).is_some()
    })
    .await;
    assert_eq!(removal_of(&seen, g), Some(RemovalCause::Expired as i32));
    let last = upsert_before_removal(&seen, g).expect("last state before the removal");
    assert!(
        last.units.iter().all(|u| !u.alive),
        "its last state is dead"
    );
    until_size(&ch, "the expiry is counted", |s| s.expired >= 1).await;
    assert_eq!(size(&ch).await.expired, 1, "only the group expired");

    // The expiry sent nothing to DCS: the sidecar wrote no destroy for the
    // group, and the dead group's objects are the fake's graves.
    assert_eq!(destroys_for(&hub, c, g), 0, "a destroy was written");
    // The ruins never expire, so wait past several expiry periods.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert_eq!(removal_of(&seen, wreck), None);
    assert_eq!(removal_of(&seen, tent), None);
    assert_eq!(
        state_of(&ch, c, wreck).await,
        Some(EntityState::Dead as i32)
    );
    assert_eq!(state_of(&ch, c, tent).await, Some(EntityState::Dead as i32));
    assert_eq!(size(&ch).await.expired, 1);
    assert_eq!(size(&ch).await.ruins, 2);
    assert!(state_of(&ch, c, g).await.is_none(), "the group is listed");
}

/// A group that died in a campaign that was then suspended does not expire
/// while it is suspended, and does once the campaign is active again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_group_of_a_suspended_campaign_expires_only_after_activation() {
    let d = tempfile::tempdir().expect("dir");
    let config = Config {
        dead_keep: Duration::from_secs(2),
        ..cfg(Some(d.path()))
    };
    let (hub, ch) = in_process(config, fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = create(&ch, &l, "susp").await;
    let g = put_group(&ch, &l, "g", c, 0.0).await;
    until_state(&ch, c, g, EntityState::Live).await;
    let units = unit_ids(&ch, c, g).await;
    kill_all(&hub, c, &units);
    until_state(&ch, c, g, EntityState::Dead).await;
    let mut camp = CampaignServiceClient::new(ch.clone());
    camp.suspend_campaign(pb::SuspendCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: c,
        idempotency: key("suspend"),
    })
    .await
    .expect("suspend");
    // Past the keep, with the campaign suspended: still dead, not expired.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(state_of(&ch, c, g).await, Some(EntityState::Dead as i32));
    camp.activate_campaign(pb::ActivateCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: c,
        idempotency: key("activate"),
    })
    .await
    .expect("activate");
    eventually("the group expires once active", || async {
        state_of(&ch, c, g).await.is_none()
    })
    .await;
    until_size(&ch, "the expiry is counted", |s| s.expired >= 1).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn store_size_follows_puts_kills_and_removals() {
    let d = tempfile::tempdir().expect("dir");
    // Keep rows and dead groups long, to read the counts between the steps.
    let config = Config {
        removed_keep: Duration::from_secs(3_600),
        dead_keep: Duration::from_secs(3_600),
        ..cfg(Some(d.path()))
    };
    let (hub, ch) = in_process(config, fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = create(&ch, &l, "size").await;
    let s = size(&ch).await;
    assert_eq!(
        (s.groups_live, s.groups_dead, s.ruins, s.awaiting_purge),
        (0, 0, 0, 0)
    );

    let a = put_group(&ch, &l, "a", c, 0.0).await;
    let b = put_group(&ch, &l, "b", c, 1_000.0).await;
    let wreck = put_static(&ch, &l, "wreck", c, -500.0, true).await;
    until_state(&ch, c, a, EntityState::Live).await;
    until_state(&ch, c, b, EntityState::Live).await;
    until_state(&ch, c, wreck, EntityState::Dead).await;
    let s = size(&ch).await;
    assert_eq!(
        (s.groups_live, s.groups_dead, s.ruins, s.awaiting_purge),
        (2, 0, 1, 0)
    );
    // Four group units and the static's own unit row.
    assert_eq!(s.units, 5);
    assert!(s.journal_rows > 0, "journal_rows");
    assert!(s.store_file_bytes > 0, "store_file_bytes");
    assert_eq!((s.purged, s.expired), (0, 0));

    // A group dies.
    let units = unit_ids(&ch, c, a).await;
    kill_all(&hub, c, &units);
    until_size(&ch, "one group dead", |s| s.groups_dead == 1).await;
    // The deaths are events: the history holds rows and its file has bytes.
    until_size(&ch, "event rows", |s| {
        s.event_rows > 0 && s.events_file_bytes > 0
    })
    .await;
    let s = size(&ch).await;
    assert_eq!(
        (s.groups_live, s.groups_dead, s.ruins, s.awaiting_purge),
        (1, 1, 1, 0)
    );

    // A group is removed: it awaits the purge, which is an hour off.
    CommandsServiceClient::new(ch.clone())
        .remove_entity(pb::RemoveEntityRequest {
            write: write(&l, "rm-b", c),
            entity_id: b,
        })
        .await
        .expect("remove");
    until_size(&ch, "one awaiting purge", |s| s.awaiting_purge == 1).await;
    let s = size(&ch).await;
    assert_eq!(
        (s.groups_live, s.groups_dead, s.ruins, s.awaiting_purge),
        (0, 1, 1, 1)
    );
    assert!(s.oldest_removal.is_some(), "oldest_removal");

    // The dead group and the ruin are removed too.
    let mut cmd = CommandsServiceClient::new(ch.clone());
    for (k, id) in [("rm-a", a), ("rm-w", wreck)] {
        cmd.remove_entity(pb::RemoveEntityRequest {
            write: write(&l, k, c),
            entity_id: id,
        })
        .await
        .expect("remove");
    }
    until_size(&ch, "three awaiting purge", |s| s.awaiting_purge == 3).await;
    let s = size(&ch).await;
    assert_eq!((s.groups_live, s.groups_dead, s.ruins), (0, 0, 0));
    assert_eq!(s.purged, 0);
}
