//! Ruins through the API (spec/world.md, "Ruins"): a destroyed campaign
//! static reads destroyed in the World service and is spawned dead again
//! after a suspend and activate, a DCS restart and a sidecar restart;
//! `PutStatic` creates one destroyed; only a removal takes it out of DCS.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{Running, in_process, start, start_with};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityState, Idempotency, Lease,
    ListWorldRequest, WorldFilter, Write, entity,
};
use nanoprobe_sidecar::store::{Entry, journal_of};
use nanoprobe_sim_fake::FakeSim;
use nanoprobe_transport::layout::opcode;
use tonic::Code;
use tonic::transport::{Channel, Endpoint};

fn cfg(dir: Option<&Path>) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("addr"),
        step: Duration::from_millis(2),
        store: dir.map(|d| StoreFiles {
            checkpoint: Duration::from_millis(50),
            ..StoreFiles::new(d.to_owned())
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
        first_boot: 2_000_000 + n * 1_000,
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

fn at(north: f64, east: f64) -> Option<pb::Position> {
    Some(pb::Position {
        theater: "Caucasus".into(),
        north,
        east,
        alt: 0.0,
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

fn a_static(l: &Lease, k: &str, campaign: u64, north: f64, dead: bool) -> pb::PutStaticRequest {
    pb::PutStaticRequest {
        write: write(l, k, campaign),
        static_id: 0,
        spec: Some(pb::StaticSpec {
            further_units: Vec::new(),
            label: k.into(),
            country: 2,
            r#type: "Tent".into(),
            position: at(north, 0.0),
            heading: 0.5,
            extra: None,
            dead,
        }),
    }
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
        .put_static(a_static(l, k, campaign, north, dead))
        .await
        .expect("put")
        .into_inner()
        .static_id
}

async fn get_static(ch: &Channel, filter: WorldFilter, id: u64) -> Option<pb::Static> {
    let mut s = WorldServiceClient::new(ch.clone())
        .list_world(ListWorldRequest {
            filter: Some(filter),
        })
        .await
        .expect("list")
        .into_inner();
    while let Some(m) = s.message().await.expect("page") {
        for e in m.entities {
            if let Some(entity::Kind::Static(x)) = e.kind
                && x.id == id
            {
                return Some(x);
            }
        }
    }
    None
}

fn whole(campaign: u64) -> WorldFilter {
    WorldFilter {
        campaign_id: campaign,
        ..WorldFilter::default()
    }
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

async fn until_state(ch: &Channel, campaign: u64, id: u64, state: EntityState) -> pb::Static {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(s) = get_static(ch, whole(campaign), id).await
                && s.state == state as i32
            {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: static {id} is {state:?}"))
}

fn with_sim<T>(hub: &Hub, f: impl FnOnce(&mut FakeSim) -> T) -> T {
    let mut inner = hub.lock();
    let sim = inner
        .backend
        .as_any()
        .and_then(|b| b.downcast_mut::<FakeSim>())
        .expect("the fake");
    f(sim)
}

/// The static's dead object in the fake DCS: whether it was spawned dead.
fn ruin(hub: &Hub, campaign: u64, id: u64) -> Option<bool> {
    with_sim(hub, |sim| {
        sim.ruins(campaign)
            .into_iter()
            .find(|r| r.entity == id)
            .map(|r| r.spawned)
    })
}

/// Spawns that reached the fake DCS for the static: live ones, and ruins.
fn spawns(hub: &Hub, id: u64) -> (usize, usize) {
    with_sim(hub, |sim| {
        let l = sim.ledger();
        (
            l.spawns.iter().filter(|s| s.2 == id).count(),
            l.ruin_spawns.iter().filter(|s| s.2 == id).count(),
        )
    })
}

/// `STATIC_SPAWN` and `STATIC_DESTROY` commands the campaign's store file
/// journals for the static.
fn written(dir: &Path, campaign: u64, id: u64) -> (usize, usize) {
    let journal =
        journal_of(&dir.join(campaign.to_string()).join("store.sqlite")).expect("journal");
    let count = |op: u64| {
        journal
            .iter()
            .filter(
                |e| matches!(e, Entry::Command(c) if c.entity.0 == id && u64::from(c.opcode) == op),
            )
            .count()
    };
    (count(opcode::STATIC_SPAWN), count(opcode::STATIC_DESTROY))
}

async fn until_ruin(hub: &Hub, campaign: u64, id: u64, spawned: bool) {
    eventually(&format!("static {id} is a ruin in DCS"), || async {
        ruin(hub, campaign, id) == Some(spawned)
    })
    .await;
}

async fn until_no_ruin(hub: &Hub, campaign: u64, id: u64) {
    eventually(&format!("DCS holds no ruin of {id}"), || async {
        ruin(hub, campaign, id).is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_destroyed_static_is_a_ruin_across_a_suspend_and_both_restarts() {
    let d = tempfile::tempdir().expect("dir");
    let server = start(cfg(Some(d.path())), fake_config())
        .await
        .expect("server");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a").await;
    let id = put_static(&ch, &l, "tent", a, -50.0, false).await;
    until_state(&ch, a, id, EntityState::Live).await;

    // It is destroyed in DCS: the World service shows it destroyed, where
    // it stood, and its own dead object is the ruin.
    with_sim(&server.hub, |sim| {
        let o = sim
            .objects()
            .into_iter()
            .find(|(_, c, e)| *c == a && *e == id)
            .expect("the static is in DCS")
            .0;
        sim.kill(o);
    });
    let s = until_state(&ch, a, id, EntityState::Dead).await;
    assert!(!s.alive, "a dead static reads alive");
    assert_eq!(s.position.as_ref().map(|p| p.north), Some(-50.0));
    until_ruin(&server.hub, a, id, false).await;
    assert_eq!(spawns(&server.hub, id), (1, 0));
    // An area query finds the ruin; one elsewhere does not.
    let near = WorldFilter {
        campaign_id: a,
        center: at(-50.0, 0.0),
        radius: 100.0,
        ..WorldFilter::default()
    };
    assert!(get_static(&ch, near.clone(), id).await.is_some());
    let far = WorldFilter {
        center: at(50_000.0, 0.0),
        ..near.clone()
    };
    assert!(get_static(&ch, far, id).await.is_none());

    // The sidecar alone starts again in the generation the static died in.
    // The mark its death left comes back from the store's file: no spawn is
    // written over the static's own dead object, and no destroy.
    let hub = Arc::clone(&server.hub);
    server.stop().await.expect("a clean stop");
    assert_eq!(written(d.path(), a, id), (1, 0), "the one live spawn");
    let sim = with_sim(&hub, |sim| {
        std::mem::replace(sim, FakeSim::new(fake_config()))
    });
    drop(hub);
    let server = start_with(cfg(Some(d.path())), sim).await.expect("restart");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let probe = put_static(&ch, &l, "probe", a, -900.0, false).await;
    until_state(&ch, a, probe, EntityState::Live).await;
    assert_eq!(ruin(&server.hub, a, id), Some(false));
    assert_eq!(
        written(d.path(), a, id),
        (1, 0),
        "the restarted sidecar wrote a command for a static that died in this generation"
    );

    // A suspend and an activate: the new mission gets the ruin, spawned
    // dead, and never the live static.
    let mut camp = CampaignServiceClient::new(ch.clone());
    camp.suspend_campaign(pb::SuspendCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: a,
        idempotency: key("suspend"),
    })
    .await
    .expect("suspend");
    camp.activate_campaign(pb::ActivateCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: a,
        idempotency: key("activate"),
    })
    .await
    .expect("activate");
    until_ruin(&server.hub, a, id, true).await;
    assert_eq!(spawns(&server.hub, id), (1, 1));
    let s = until_state(&ch, a, id, EntityState::Dead).await;
    assert!(!s.alive);
    assert!(get_static(&ch, near.clone(), id).await.is_some());

    // The sidecar and DCS both start again: the new DCS gets the ruin.
    server.stop().await.expect("a clean stop");
    let server = start(cfg(Some(d.path())), fake_config())
        .await
        .expect("restart");
    let ch = connect(&server).await;
    until_ruin(&server.hub, a, id, true).await;
    assert_eq!(
        spawns(&server.hub, id),
        (0, 1),
        "no live spawn in the new DCS"
    );
    let s = until_state(&ch, a, id, EntityState::Dead).await;
    assert!(!s.alive);

    // The sidecar alone starts again while DCS goes on: nothing is spawned
    // or destroyed, and a static put afterwards goes live. The spawns are
    // counted as the store's file journals them: the agent would answer a
    // spawn written again `AT_REVISION`, which DCS's own count cannot show.
    // So this also checks that the ruin marks came back from the file.
    let hub = Arc::clone(&server.hub);
    server.stop().await.expect("a clean stop");
    let written_before = written(d.path(), a, id);
    assert_eq!(written_before, (3, 0), "one live spawn and two ruins");
    let sim = with_sim(&hub, |sim| {
        std::mem::replace(sim, FakeSim::new(fake_config()))
    });
    drop(hub);
    let server = start_with(cfg(Some(d.path())), sim).await.expect("restart");
    let ch = connect(&server).await;
    let l = lease(&ch).await;
    let later = put_static(&ch, &l, "later", a, -500.0, false).await;
    until_state(&ch, a, later, EntityState::Live).await;
    assert_eq!(ruin(&server.hub, a, id), Some(true));
    assert_eq!(spawns(&server.hub, id), (0, 1));
    assert_eq!(
        written(d.path(), a, id),
        written_before,
        "the restarted sidecar wrote a spawn or a destroy for the ruin"
    );

    // Only the removal takes it out of DCS and out of the World service.
    CommandsServiceClient::new(ch.clone())
        .remove_entity(pb::RemoveEntityRequest {
            write: write(&l, "remove", a),
            entity_id: id,
        })
        .await
        .expect("remove");
    until_no_ruin(&server.hub, a, id).await;
    assert!(get_static(&ch, whole(a), id).await.is_none());
    server.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_static_creates_a_ruin_and_authority_moves_it() {
    let (hub, ch) = in_process(cfg(None), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let a = create(&ch, &l, "a").await;
    let mut cmd = CommandsServiceClient::new(ch.clone());

    // Created destroyed: `dead` from the start, spawned dead, never alive.
    let id = put_static(&ch, &l, "wreck", a, -80.0, true).await;
    let s = until_state(&ch, a, id, EntityState::Dead).await;
    assert!(!s.alive && s.managed);
    until_ruin(&hub, a, id, true).await;
    assert_eq!(spawns(&hub, id), (0, 1));

    // A `dead` key among the further keys is refused.
    let mut bad = a_static(&l, "bad", a, -90.0, false);
    bad.spec.as_mut().expect("spec").extra = Some(prost_types::Struct {
        fields: [(
            "dead".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::BoolValue(true)),
            },
        )]
        .into_iter()
        .collect(),
    });
    let e = cmd.put_static(bad).await.expect_err("extra.dead");
    assert_eq!(e.code(), Code::InvalidArgument);

    // A dematerialize destroys the ruin; a materialize spawns it dead.
    cmd.dematerialize(pb::DematerializeRequest {
        write: write(&l, "out", a),
        entity_ids: vec![id],
    })
    .await
    .expect("dematerialize");
    until_no_ruin(&hub, a, id).await;
    let s = until_state(&ch, a, id, EntityState::Dead).await;
    assert!(!s.alive);
    cmd.materialize(pb::MaterializeRequest {
        write: write(&l, "in", a),
        entity_ids: vec![id],
    })
    .await
    .expect("materialize");
    until_ruin(&hub, a, id, true).await;
    assert_eq!(spawns(&hub, id), (0, 2));

    // A clear of debris over the ruin makes the sidecar send nothing for
    // it. That the agent's handler leaves a managed dead static is checked
    // on the mock (lua/tests/dispatch.lua); the fake's clear touches none.
    let r = cmd
        .do_effect(pb::DoEffectRequest {
            write: write(&l, "clear", a),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::ClearDebris(pb::ClearDebris {
                    volume: Some(pb::clear_debris::Volume::Sphere(pb::Sphere {
                        center: at(-80.0, 0.0),
                        radius: 100.0,
                    })),
                })),
            }),
        })
        .await
        .expect("clear")
        .into_inner();
    assert_eq!(
        r.result.as_ref().expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    // The store's journal counts what the sidecar wrote for the ruin: one
    // destroy, the dematerialize's, before the clear and well after it.
    let destroys = || {
        hub.lock().campaigns[&a]
            .store
            .entries()
            .iter()
            .filter(|e| {
                matches!(e, Entry::Command(c)
                    if c.entity.0 == id && u64::from(c.opcode) == opcode::STATIC_DESTROY)
            })
            .count()
    };
    assert_eq!(destroys(), 1);
    // A hundred steps of the sidecar: time for a destroy to be planned.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(destroys(), 1, "the clear made the sidecar destroy the ruin");
    assert_eq!(ruin(&hub, a, id), Some(true));
    assert_eq!(spawns(&hub, id), (0, 2));
}
