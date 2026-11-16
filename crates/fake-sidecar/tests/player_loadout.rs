//! A player's loadout through the API on the fake sim (spec/world.md,
//! "Ammunition"): `Player.ammo` and `Player.ammo_read` for the unit a player
//! occupies, adopted or managed, and one `LoadoutEvent` per read after the
//! world event that caused it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::players_service_client::PlayersServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, AmmoReadCause, CreateCampaignRequest, Idempotency, Lease,
    Write, entity,
};
use nanoprobe_sim_fake::FakeSim;
use tonic::transport::Channel;

const UCID_A: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const UCID_B: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
const AIM9: &str = "weapons.missiles.AIM_9M";
const GUN: &str = "weapons.shells.M61_20_HE";
const BIRTH: u32 = 15;
const TAKEOFF: u32 = 3;

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
            name: "player loadout".into(),
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

fn fake<T>(hub: &Hub, f: impl FnOnce(&mut FakeSim) -> T) -> T {
    let mut g = hub.lock();
    let sim = g
        .backend
        .as_any()
        .and_then(|a| a.downcast_mut::<FakeSim>())
        .expect("the fake sim");
    f(sim)
}

async fn until<F, Fut>(mut ok: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if ok().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the state is reached");
}

async fn players_now(ch: &Channel) -> Vec<pb::Player> {
    PlayersServiceClient::new(ch.clone())
        .list_players(pb::ListPlayersRequest {
            include_offline: false,
        })
        .await
        .expect("players")
        .into_inner()
        .players
}

async fn history(ch: &Channel, c: u64, filter: pb::EventFilter) -> Vec<pb::Event> {
    EventsServiceClient::new(ch.clone())
        .read_events(pb::ReadEventsRequest {
            filter: Some(pb::EventFilter {
                campaign_id: c,
                ..filter
            }),
            after_sequence: 0,
            limit: 10_000,
        })
        .await
        .expect("events")
        .into_inner()
        .events
}

fn loadouts(events: &[pb::Event]) -> Vec<&pb::LoadoutEvent> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            Some(pb::event::Body::Loadout(l)) => Some(l),
            _ => None,
        })
        .collect()
}

/// The sequence of the first world event with a DCS ID after `after`.
fn world_seq(events: &[pb::Event], dcs_id: u32, after: u64) -> u64 {
    events
        .iter()
        .find(|e| {
            e.sequence > after
                && matches!(&e.body, Some(pb::event::Body::World(w)) if w.dcs_id == dcs_id)
        })
        .map(|e| e.sequence)
        .unwrap_or_else(|| panic!("no world event {dcs_id} after {after}"))
}

/// A subscriber on the live stream of the campaign's events.
fn subscribe(ch: &Channel, c: u64, filter: pb::EventFilter) -> Arc<Mutex<Vec<pb::Event>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let mut client = EventsServiceClient::new(ch.clone());
    tokio::spawn(async move {
        let mut s = client
            .subscribe_events(pb::SubscribeEventsRequest {
                filter: Some(pb::EventFilter {
                    campaign_id: c,
                    ..filter
                }),
                after_sequence: 0,
                live_only: false,
            })
            .await
            .expect("subscribe")
            .into_inner();
        while let Ok(Some(m)) = s.message().await {
            if let Some(e) = m.event {
                sink.lock().expect("sink").push(e);
            }
        }
    });
    seen
}

fn loadout_count(seen: &Arc<Mutex<Vec<pb::Event>>>) -> usize {
    loadouts(&seen.lock().expect("sink")).len()
}

fn model_time(hub: &Hub) -> f64 {
    hub.lock().link.model_time
}

async fn managed_unit(
    hub: &Hub,
    ch: &Channel,
    l: &Lease,
    c: u64,
    key: &str,
    kind: &str,
    category: pb::GroupCategory,
) -> (u64, u64, u64, u64) {
    let g = CommandsServiceClient::new(ch.clone())
        .put_group(pb::PutGroupRequest {
            write: write(l, key, c),
            group_id: 0,
            spec: Some(pb::GroupSpec {
                label: key.into(),
                country: 2,
                category: category as i32,
                units: vec![pb::UnitSpec {
                    r#type: kind.into(),
                    position: Some(pb::Position {
                        theater: "Caucasus".into(),
                        north: 0.0,
                        east: 0.0,
                        alt: 3000.0,
                    }),
                    ..pb::UnitSpec::default()
                }],
                ..pb::GroupSpec::default()
            }),
            order: None,
        })
        .await
        .expect("group")
        .into_inner();
    let unit = g.unit_ids[0];
    until(|| async { fake(hub, |s| s.objects().iter().any(|(_, _, e)| *e == unit)) }).await;
    let object = fake(hub, |s| {
        s.objects()
            .into_iter()
            .find(|(_, _, e)| *e == unit)
            .map(|(o, _, _)| o)
            .expect("object")
    });
    (object, unit, g.group_id, 0)
}

async fn store_unit(ch: &Channel, c: u64, id: u64) -> pb::Unit {
    let mut s = WorldServiceClient::new(ch.clone())
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: c,
                ..pb::WorldFilter::default()
            }),
        })
        .await
        .expect("list")
        .into_inner();
    while let Some(m) = s.message().await.expect("page") {
        for e in m.entities {
            if let Some(entity::Kind::Group(g)) = e.kind
                && let Some(u) = g.units.into_iter().find(|u| u.id == id)
            {
                return u;
            }
        }
    }
    panic!("no unit {id}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adopted_aircraft_shows_its_loadout_on_the_player_and_in_events() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let (l, c) = setup(&ch).await;
    until(|| async { fake(&hub, |s| s.attached()) }).await;
    // An AI unit of the campaign gets no read.
    let (_, ai_unit, _, _) =
        managed_unit(&hub, &ch, &l, c, "ai", "M-113", pb::GroupCategory::Ground).await;

    fake(&hub, |s| {
        s.connect_client(2, UCID_A, "Ann", 2);
        s.client_slot(2, 2, "m1");
    });
    until(|| async { players_now(&ch).await.len() == 1 }).await;
    let key = players_now(&ch).await[0].key.clone();
    let all = subscribe(&ch, c, pb::EventFilter::default());
    let by_key = subscribe(
        &ch,
        c,
        pb::EventFilter {
            player_key: key.clone(),
            ..pb::EventFilter::default()
        },
    );
    let by_takeoff = subscribe(
        &ch,
        c,
        pb::EventFilter {
            dcs_ids: vec![TAKEOFF],
            ..pb::EventFilter::default()
        },
    );
    let by_birth_and_other = subscribe(
        &ch,
        c,
        pb::EventFilter {
            dcs_ids: vec![BIRTH],
            player_key: "nobody".into(),
            ..pb::EventFilter::default()
        },
    );

    // A birth read: the rows of one type add up.
    let object = fake(&hub, |s| s.birth_client_unit_in("m1", 77, "F-16C_50"));
    until(|| async { players_now(&ch).await[0].unit_id != 0 }).await;
    let p = &players_now(&ch).await[0];
    assert!(p.ammo.is_empty() && p.ammo_read.is_none(), "{p:?}");
    let unit = p.unit_id;
    // The agent takes the `BIND` a step after the sidecar sends it.
    until(|| async { fake(&hub, |s| s.binding_of(object)).is_some_and(|(_, g)| g != 0) }).await;
    let (_, group) = fake(&hub, |s| s.binding_of(object)).expect("a row");
    assert!(
        store_unit_missing(&ch, c, unit).await,
        "the adopted unit has no store row"
    );
    fake(&hub, |s| {
        s.emit_ammo(object, 1, &[(AIM9, 2.0), (AIM9, 2.0), (GUN, 940.0)])
    });
    until(|| async { !players_now(&ch).await[0].ammo.is_empty() }).await;
    let p = players_now(&ch).await.remove(0);
    assert_eq!(p.unit_id, unit);
    assert_eq!(p.ammo.len(), 2, "{:?}", p.ammo);
    assert_eq!(p.ammo[AIM9], 4);
    assert_eq!(p.ammo[GUN], 940);
    let read = p.ammo_read.as_ref().expect("the read");
    assert_eq!(read.cause, AmmoReadCause::Birth as i32);
    assert!(read.read.as_ref().is_some_and(|s| s.frame > 0));

    // A takeoff read replaces it.
    let now = model_time(&hub);
    fake(&hub, |s| {
        s.emit_event(
            TAKEOFF,
            vec![
                (1, nanoprobe_transport::codec::Lv::Number(now)),
                (2, nanoprobe_transport::codec::Lv::Number(object as f64)),
            ],
        );
        s.emit_ammo(object, 2, &[(AIM9, 3.0), (AIM9, 1.0), ("flare", 60.0)]);
    });
    until(|| async {
        players_now(&ch).await[0]
            .ammo_read
            .as_ref()
            .is_some_and(|r| r.cause == AmmoReadCause::Takeoff as i32)
    })
    .await;
    let p = players_now(&ch).await.remove(0);
    assert_eq!(p.ammo.len(), 2, "{:?}", p.ammo);
    assert_eq!(p.ammo[AIM9], 4);
    assert_eq!(p.ammo["flare"], 60);

    // One event per read, each after the world event that caused it.
    until(|| async { loadout_count(&all) == 2 }).await;
    let events = history(&ch, c, pb::EventFilter::default()).await;
    let ls = loadouts(&events);
    assert_eq!(ls.len(), 2, "{ls:?}");
    for l in &ls {
        assert_eq!(l.unit_id, unit);
        assert_eq!(l.group_id, group);
        assert_eq!(l.r#type, "F-16C_50");
        assert_eq!(l.player_keys, vec![key.clone()]);
    }
    assert_eq!(ls[0].ammo[AIM9], 4);
    assert_eq!(
        ls[0].read.as_ref().expect("read").cause,
        AmmoReadCause::Birth as i32
    );
    assert_eq!(ls[1].ammo["flare"], 60);
    assert_eq!(
        ls[1].read.as_ref().expect("read").cause,
        AmmoReadCause::Takeoff as i32
    );
    let seq = |cause: i32| {
        events
            .iter()
            .find(|e| {
                matches!(&e.body, Some(pb::event::Body::Loadout(l))
                    if l.read.as_ref().map(|r| r.cause) == Some(cause))
            })
            .expect("a loadout event")
            .sequence
    };
    let birth = world_seq(&events, BIRTH, 0);
    let takeoff = world_seq(&events, TAKEOFF, 0);
    assert!(birth < seq(AmmoReadCause::Birth as i32));
    assert!(takeoff < seq(AmmoReadCause::Takeoff as i32));
    assert!(seq(AmmoReadCause::Birth as i32) < takeoff);

    // The filters: the live stream, the player's key and the cause's DCS ID.
    until(|| async { loadout_count(&by_key) == 2 && loadout_count(&by_takeoff) == 1 }).await;
    {
        let t = by_takeoff.lock().expect("sink");
        assert_eq!(
            loadouts(&t)[0].read.as_ref().expect("read").cause,
            AmmoReadCause::Takeoff as i32
        );
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(loadout_count(&by_birth_and_other), 0);
    let other = history(
        &ch,
        c,
        pb::EventFilter {
            player_key: "nobody".into(),
            ..pb::EventFilter::default()
        },
    )
    .await;
    assert!(loadouts(&other).is_empty());
    let birth_only = history(
        &ch,
        c,
        pb::EventFilter {
            dcs_ids: vec![BIRTH],
            ..pb::EventFilter::default()
        },
    )
    .await;
    let bo = loadouts(&birth_only);
    assert_eq!(bo.len(), 1);
    assert_eq!(
        bo[0].read.as_ref().expect("read").cause,
        AmmoReadCause::Birth as i32
    );

    // The AI unit has no loadout and no event.
    let ai = store_unit(&ch, c, ai_unit).await;
    assert!(ai.ammo.is_empty() && ai.ammo_read.is_none());

    // Leaving the unit clears the player's fields.
    fake(&hub, |s| s.client_slot(2, 2, ""));
    until(|| async { players_now(&ch).await[0].unit_id == 0 }).await;
    let p = players_now(&ch).await.remove(0);
    assert!(p.ammo.is_empty() && p.ammo_read.is_none(), "{p:?}");
    assert_eq!(
        loadouts(&history(&ch, c, pb::EventFilter::default()).await).len(),
        2
    );
}

async fn store_unit_missing(ch: &Channel, c: u64, id: u64) -> bool {
    let mut s = WorldServiceClient::new(ch.clone())
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: c,
                ..pb::WorldFilter::default()
            }),
        })
        .await
        .expect("list")
        .into_inner();
    while let Some(m) = s.message().await.expect("page") {
        for e in m.entities {
            if let Some(entity::Kind::Group(g)) = e.kind
                && g.units.iter().any(|u| u.id == id)
            {
                return false;
            }
        }
    }
    true
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_managed_unit_shows_the_same_loadout_on_the_unit_and_the_player() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let (l, c) = setup(&ch).await;
    until(|| async { fake(&hub, |s| s.attached()) }).await;
    let (object, unit, group, _) = managed_unit(
        &hub,
        &ch,
        &l,
        c,
        "air",
        "F-15C",
        pb::GroupCategory::Airplane,
    )
    .await;
    fake(&hub, |s| {
        s.connect_client(3, UCID_B, "Bob", 2);
        s.client_slot(3, 2, "m2");
    });
    until(|| async { players_now(&ch).await.len() == 1 }).await;
    fake(&hub, |s| s.birth_event(object, Some("m2")));
    until(|| async { players_now(&ch).await[0].unit_id == unit }).await;
    fake(&hub, |s| {
        s.emit_ammo(object, 1, &[(AIM9, 2.0), (AIM9, 4.0)])
    });
    until(|| async { !players_now(&ch).await[0].ammo.is_empty() }).await;
    let p = players_now(&ch).await.remove(0);
    let u = store_unit(&ch, c, unit).await;
    assert_eq!(p.ammo[AIM9], 6);
    assert_eq!(u.ammo, p.ammo);
    assert_eq!(u.ammo_read, p.ammo_read);
    let ls = history(&ch, c, pb::EventFilter::default()).await;
    let ls = loadouts(&ls);
    assert_eq!(ls.len(), 1);
    assert_eq!((ls[0].unit_id, ls[0].group_id), (unit, group));
    assert_eq!(ls[0].r#type, "F-15C");
    assert_eq!(ls[0].ammo, p.ammo);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_before_the_bind_still_reaches_the_player_and_the_event() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let (_l, c) = setup(&ch).await;
    until(|| async { fake(&hub, |s| s.attached()) }).await;
    fake(&hub, |s| {
        s.connect_client(2, UCID_A, "Ann", 2);
        s.client_slot(2, 2, "m1");
    });
    until(|| async { players_now(&ch).await.len() == 1 }).await;
    let key = players_now(&ch).await[0].key.clone();

    // The object and its `AMMO` record come before the birth that seats the
    // player, so the sidecar has no `EntityId` for the object at the read:
    // it holds the read, and the player shows no ammunition yet.
    let object = fake(&hub, |s| {
        let o = s.place_client_unit_in("m1", 77, "F-16C_50");
        s.emit_ammo(o, 1, &[(AIM9, 2.0), (AIM9, 2.0), (GUN, 100.0)]);
        o
    });
    until(|| async { hub.lock().loadouts.held_len() == 1 }).await;
    assert!(players_now(&ch).await[0].ammo.is_empty());
    fake(&hub, |s| s.birth_event(object, Some("m1")));
    until(|| async { !players_now(&ch).await[0].ammo.is_empty() }).await;
    let p = players_now(&ch).await.remove(0);
    // The agent takes the `BIND` a step after the sidecar sends it.
    until(|| async { fake(&hub, |s| s.binding_of(object)).is_some_and(|(_, g)| g != 0) }).await;
    let (unit, group) = fake(&hub, |s| s.binding_of(object)).expect("a row");
    assert_eq!(p.unit_id, unit);
    assert_eq!(p.ammo[AIM9], 4);
    assert_eq!(p.ammo[GUN], 100);
    assert_eq!(
        p.ammo_read.as_ref().expect("read").cause,
        AmmoReadCause::Birth as i32
    );
    until(|| async {
        let e = history(&ch, c, pb::EventFilter::default()).await;
        !loadouts(&e).is_empty()
    })
    .await;
    let events = history(&ch, c, pb::EventFilter::default()).await;
    let ls = loadouts(&events);
    assert_eq!(ls.len(), 1, "{ls:?}");
    assert_eq!(ls[0].unit_id, unit);
    assert_eq!(ls[0].group_id, group);
    assert_eq!(ls[0].player_keys, vec![key]);
    assert_eq!(ls[0].ammo[AIM9], 4);
    let seq = events
        .iter()
        .find(|e| matches!(&e.body, Some(pb::event::Body::Loadout(_))))
        .expect("event")
        .sequence;
    assert!(world_seq(&events, BIRTH, 0) < seq);
    assert_eq!(
        hub.lock().loadouts.held_len(),
        0,
        "nothing stays held after the bind"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_with_no_ammunition_rows_is_a_read_and_a_departed_player_clears_the_unit() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let (l, c) = setup(&ch).await;
    until(|| async { fake(&hub, |s| s.attached()) }).await;
    let (object, unit, _, _) = managed_unit(
        &hub,
        &ch,
        &l,
        c,
        "air",
        "F-15C",
        pb::GroupCategory::Airplane,
    )
    .await;
    fake(&hub, |s| {
        s.connect_client(3, UCID_B, "Bob", 2);
        s.client_slot(3, 2, "m2");
    });
    until(|| async { players_now(&ch).await.len() == 1 }).await;
    fake(&hub, |s| s.birth_event(object, Some("m2")));
    until(|| async { players_now(&ch).await[0].unit_id == unit }).await;
    fake(&hub, |s| s.emit_ammo(object, 1, &[]));
    until(|| async { players_now(&ch).await[0].ammo_read.is_some() }).await;
    let p = players_now(&ch).await.remove(0);
    assert!(p.ammo.is_empty());
    assert_eq!(
        p.ammo_read.as_ref().expect("read").cause,
        AmmoReadCause::Birth as i32
    );
    until(|| async { store_unit(&ch, c, unit).await.ammo_read.is_some() }).await;
    assert!(store_unit(&ch, c, unit).await.ammo.is_empty());

    // The last player leaves: the unit shows no read either.
    fake(&hub, |s| s.emit_ammo(object, 1, &[(AIM9, 2.0)]));
    until(|| async { !store_unit(&ch, c, unit).await.ammo.is_empty() }).await;
    fake(&hub, |s| s.disconnect_client(3));
    until(|| async {
        let u = store_unit(&ch, c, unit).await;
        u.ammo.is_empty() && u.ammo_read.is_none()
    })
    .await;
}
