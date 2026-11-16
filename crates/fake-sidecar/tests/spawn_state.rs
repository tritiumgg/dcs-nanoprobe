//! Spawning with a unit's life through the API on the fake sim
//! (spec/world.md, "Spawn state"): `PutGroup` takes a unit's life, the life
//! follows the spawn as a `DAMAGE` (none at full life), and a dematerialize
//! and materialize hand the captured life over both ways, as S20's
//! virtualization does.

use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityState, GroupCategory, GroupSpec,
    Idempotency, Lease, ListWorldRequest, PutGroupRequest, UnitSpec, WorldFilter, Write, entity,
};
use nanoprobe_sim_fake::FakeSim;
use tonic::transport::Channel;

fn cfg() -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("addr"),
        step: Duration::from_millis(2),
        ..Config::default()
    }
}

fn write(l: &Lease, k: &str, campaign: u64) -> Option<Write> {
    Some(Write {
        lease: Some(l.clone()),
        idempotency: Some(Idempotency { key: k.into() }),
        campaign_id: campaign,
    })
}

async fn setup(ch: &Channel) -> (Lease, u64) {
    let l = SessionServiceClient::new(ch.clone())
        .acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect("lease")
        .into_inner()
        .lease
        .expect("lease");
    let c = CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: "state".into(),
            theaters: vec!["Caucasus".into()],
            activate: true,
            idempotency: Some(Idempotency {
                key: "create".into(),
            }),
            ..CreateCampaignRequest::default()
        })
        .await
        .expect("create")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    (l, c)
}

fn unit(kind: &str, north: f64) -> UnitSpec {
    UnitSpec {
        r#type: kind.into(),
        position: Some(pb::Position {
            theater: "Caucasus".into(),
            north,
            east: 0.0,
            alt: 3000.0,
        }),
        ..UnitSpec::default()
    }
}

fn put(l: &Lease, k: &str, c: u64, category: GroupCategory, u: UnitSpec) -> PutGroupRequest {
    PutGroupRequest {
        write: write(l, k, c),
        group_id: 0,
        spec: Some(GroupSpec {
            label: k.into(),
            country: 2,
            category: category as i32,
            units: vec![u],
            ..GroupSpec::default()
        }),
        order: None,
    }
}

async fn get_group(ch: &Channel, campaign: u64, id: u64) -> Option<pb::Group> {
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
                return Some(g);
            }
        }
    }
    None
}

async fn until(ch: &Channel, c: u64, id: u64, state: EntityState) -> pb::Group {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(g) = get_group(ch, c, id).await
                && g.state == state as i32
            {
                return g;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the group reaches the state")
}

fn fake<T>(hub: &Hub, f: impl FnOnce(&mut FakeSim) -> T) -> T {
    let mut g = hub.lock();
    let sim = g
        .backend
        .as_any()
        .and_then(|a| a.downcast_mut::<FakeSim>())
        .expect("the fake sim");
    f(sim)
}

async fn until_fake(hub: &Hub, ok: impl Fn(&mut FakeSim) -> bool) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if fake(hub, &ok) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("DCS reaches the state");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_unit_spawns_with_its_life_and_keeps_it_across_a_dematerialize() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let (l, c) = setup(&ch).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let air = cmd
        .put_group(put(
            &l,
            "air",
            c,
            GroupCategory::Airplane,
            UnitSpec {
                life_percent: Some(40.0),
                ..unit("F-15C", 0.0)
            },
        ))
        .await
        .expect("aircraft")
        .into_inner();
    let ground = cmd
        .put_group(put(
            &l,
            "ground",
            c,
            GroupCategory::Ground,
            UnitSpec {
                life_percent: Some(60.0),
                ..unit("M-113", 500.0)
            },
        ))
        .await
        .expect("ground")
        .into_inner();
    let whole = cmd
        .put_group(put(
            &l,
            "whole",
            c,
            GroupCategory::Ground,
            UnitSpec {
                life_percent: Some(100.0),
                ..unit("M-113", 900.0)
            },
        ))
        .await
        .expect("whole")
        .into_inner();
    let (au, gu, wu) = (air.unit_ids[0], ground.unit_ids[0], whole.unit_ids[0]);
    for id in [air.group_id, ground.group_id, whole.group_id] {
        until(&ch, c, id, EntityState::Live).await;
    }
    // Each life follows its spawn as a DAMAGE; a full life sends none.
    until_fake(&hub, |s| {
        s.life_percent_of(au) == Some(40.0) && s.life_percent_of(gu) == Some(60.0)
    })
    .await;
    assert_eq!(fake(&hub, |s| s.life_percent_of(wu)), None);

    // A capture reads back the life DCS holds.
    let cap = cmd
        .capture(pb::CaptureRequest {
            write: write(&l, "capture", c),
            entity_id: air.group_id,
        })
        .await
        .expect("capture")
        .into_inner();
    assert_eq!(
        cap.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    // The store keeps life as f32.
    let near = |v: Option<f64>, x: f64| v.is_some_and(|v| (v - x).abs() < 1e-6);
    let g = get_group(&ch, c, air.group_id).await.expect("group");
    assert!(near(g.units[0].life, 0.4), "life {:?}", g.units[0].life);

    // Dematerialize, then materialize: the life goes into the store and
    // comes back out to DCS.
    cmd.dematerialize(pb::DematerializeRequest {
        write: write(&l, "virtual", c),
        entity_ids: vec![air.group_id],
    })
    .await
    .expect("dematerialize");
    until(&ch, c, air.group_id, EntityState::Virtual).await;
    cmd.materialize(pb::MaterializeRequest {
        write: write(&l, "real", c),
        entity_ids: vec![air.group_id],
    })
    .await
    .expect("materialize");
    until(&ch, c, air.group_id, EntityState::Live).await;
    until_fake(&hub, |s| near(s.life_percent_of(au), 40.0)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_life_out_of_range_or_a_fuel_or_ammunition_is_refused() {
    let (_hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let (l, c) = setup(&ch).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let bad = [
        (
            "life",
            UnitSpec {
                life_percent: Some(100.5),
                ..unit("F-15C", 0.0)
            },
        ),
        (
            "life-nan",
            UnitSpec {
                life_percent: Some(f64::NAN),
                ..unit("F-15C", 0.0)
            },
        ),
        (
            "fuel",
            UnitSpec {
                fuel: Some(0.5),
                ..unit("F-15C", 0.0)
            },
        ),
        (
            "ammo",
            UnitSpec {
                ammo: [("weapons.missiles.AIM_120".to_owned(), 1)].into(),
                ..unit("F-15C", 0.0)
            },
        ),
    ];
    for (k, u) in bad {
        let e = cmd
            .put_group(put(&l, k, c, GroupCategory::Airplane, u))
            .await
            .expect_err(k);
        assert_eq!(e.code(), tonic::Code::InvalidArgument, "{k}: {e}");
    }
}
