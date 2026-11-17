//! A player-occupied unit's ammunition through the API on the fake sim
//! (spec/world.md, "Ammunition"): an `AMMO` record fills `Unit.ammo` and
//! `Unit.ammo_read` with its cause and stamps, a later one replaces them, and
//! a unit with no `AMMO` record shows an empty map and no read.

use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, AmmoReadCause, CreateCampaignRequest, EntityState,
    GroupCategory, GroupSpec, Idempotency, Lease, ListWorldRequest, PutGroupRequest, UnitSpec,
    WorldFilter, Write, entity,
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
            name: "loadout".into(),
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

fn put(
    l: &Lease,
    k: &str,
    c: u64,
    category: GroupCategory,
    kind: &str,
    north: f64,
) -> PutGroupRequest {
    PutGroupRequest {
        write: write(l, k, c),
        group_id: 0,
        spec: Some(GroupSpec {
            label: k.into(),
            country: 2,
            category: category as i32,
            units: vec![UnitSpec {
                r#type: kind.into(),
                position: Some(pb::Position {
                    theater: "Caucasus".into(),
                    north,
                    east: 0.0,
                    alt: 3000.0,
                }),
                ..UnitSpec::default()
            }],
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

async fn until(ch: &Channel, c: u64, id: u64, ok: impl Fn(&pb::Group) -> bool) -> pb::Group {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(g) = get_group(ch, c, id).await
                && ok(&g)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_player_units_ammunition_shows_with_its_cause() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let (l, c) = setup(&ch).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let air = cmd
        .put_group(put(&l, "air", c, GroupCategory::Airplane, "F-15C", 0.0))
        .await
        .expect("aircraft")
        .into_inner();
    let ai = cmd
        .put_group(put(&l, "ai", c, GroupCategory::Ground, "M-113", 500.0))
        .await
        .expect("ground")
        .into_inner();
    for id in [air.group_id, ai.group_id] {
        until(&ch, c, id, |g| g.state == EntityState::Live as i32).await;
    }
    let (au, iu) = (air.unit_ids[0], ai.unit_ids[0]);
    let pilot = fake(&hub, |s| {
        s.objects()
            .iter()
            .find(|(_, _, entity)| *entity == au)
            .map(|(o, _, _)| *o)
    })
    .expect("the unit's object");

    // A birth read: the record's rows add up by type, and the cause shows.
    fake(&hub, |s| {
        s.emit_ammo(
            pilot,
            1,
            &[
                ("weapons.missiles.AIM_9M", 2.0),
                ("weapons.missiles.AIM_9M", 2.0),
                ("weapons.shells.M61_20_HE", 940.0),
            ],
        )
    });
    let g = until(&ch, c, air.group_id, |g| !g.units[0].ammo.is_empty()).await;
    let u = &g.units[0];
    assert_eq!(u.ammo.len(), 2, "{:?}", u.ammo);
    assert_eq!(u.ammo["weapons.missiles.AIM_9M"], 4);
    assert_eq!(u.ammo["weapons.shells.M61_20_HE"], 940);
    let read = u.ammo_read.as_ref().expect("the read");
    assert_eq!(read.cause, AmmoReadCause::Birth as i32);
    let stamp = read.read.as_ref().expect("the read's stamps");
    assert!(stamp.frame > 0, "{stamp:?}");
    assert!(stamp.wall.is_some(), "{stamp:?}");

    // A takeoff read replaces it, with its own cause.
    fake(&hub, |s| {
        s.emit_ammo(pilot, 2, &[("weapons.missiles.AIM_9M", 3.0)])
    });
    let g = until(&ch, c, air.group_id, |g| {
        g.units[0].ammo_read.as_ref().map(|r| r.cause) == Some(AmmoReadCause::Takeoff as i32)
    })
    .await;
    assert_eq!(g.units[0].ammo.len(), 1, "{:?}", g.units[0].ammo);
    assert_eq!(g.units[0].ammo["weapons.missiles.AIM_9M"], 3);

    // An AI unit has no record: an empty map and no read.
    let g = get_group(&ch, c, ai.group_id).await.expect("ground group");
    assert_eq!(g.units[0].id, iu);
    assert!(g.units[0].ammo.is_empty(), "{:?}", g.units[0].ammo);
    assert!(g.units[0].ammo_read.is_none());
}
