//! Streams at their edges: shutdown, list-then-watch across a death, the
//! position rate, the last buffer slot, campaign switches between
//! campaigns; and calls that must change nothing when they fail, or must
//! take effect when they succeed.

use std::sync::Arc;
use std::time::{Duration, Instant};

use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_api::server::SHUTDOWN_GRACE;
use nanoprobe_api::services::events::Events;
use nanoprobe_fake_sidecar::{fake_config, in_process, start};
use nanoprobe_proto::errors::reason;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_server::EventsService;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityState, ErrorReason, EventFilter,
    GroupCategory, GroupSpec, Idempotency, Lease, ListWorldRequest, Order, PutGroupRequest,
    SubscribeEventsRequest, UnitSpec, WatchWorldRequest, WorldFilter, Write, entity, order,
    world_change,
};
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::store::Entry;
use nanoprobe_sim_fake::FakeSim;
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use tonic::{Code, Request};

fn cfg() -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("addr"),
        step: Duration::from_millis(2),
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

fn at(north: f64, east: f64) -> Option<pb::Position> {
    Some(pb::Position {
        theater: "Caucasus".into(),
        north,
        east,
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

async fn campaign(ch: &Channel, l: &Lease, name: &str) -> u64 {
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

fn group(l: &Lease, k: &str, campaign: u64, north: f64) -> PutGroupRequest {
    PutGroupRequest {
        write: write(l, k, campaign),
        group_id: 0,
        spec: Some(GroupSpec {
            label: k.into(),
            country: 2,
            category: GroupCategory::Ground as i32,
            units: vec![UnitSpec {
                r#type: "M-113".into(),
                position: at(north, 0.0),
                ..UnitSpec::default()
            }],
            ..GroupSpec::default()
        }),
        order: None,
    }
}

fn group_of(hub: &Hub, c: u64, id: u64) -> Option<(EntityState, u64)> {
    let g = hub.lock();
    let grp = g.campaigns[&c].store.group(EntityId(id))?;
    let e = nanoprobe_api::convert::entity(&g.campaigns[&c].store, grp);
    match e.kind {
        Some(entity::Kind::Group(x)) => Some((
            EntityState::try_from(x.state).unwrap_or(EntityState::Unspecified),
            x.spec_revision,
        )),
        _ => None,
    }
}

async fn until_state(hub: &Hub, c: u64, id: u64, want: EntityState) -> u64 {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some((s, rev)) = group_of(hub, c, id)
                && s == want
            {
                return rev;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("group {id} never reached {want:?}"))
}

/// Remove every unit of a group from the fake DCS with no event.
fn vanish(hub: &Hub, campaign: u64, units: &[u64]) {
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
        sim.remove_silently(o);
    }
}

/// How a stream ended, reading past any messages.
async fn ending<T>(s: &mut tonic::Streaming<T>) -> tonic::Status {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match s.message().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("the stream ended with no status"),
                Err(e) => return e,
            }
        }
    })
    .await
    .expect("the stream ends")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_ends_open_streams_with_unavailable() {
    let server = start(cfg(), fake_config()).await.expect("server");
    let ch = Channel::from_shared(server.endpoint())
        .expect("endpoint")
        .connect()
        .await
        .expect("connect");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "stop").await;
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                ..WorldFilter::default()
            }),
            ..WatchWorldRequest::default()
        })
        .await
        .expect("watch")
        .into_inner();
    let mut ev = EventsServiceClient::new(ch.clone())
        .subscribe_events(SubscribeEventsRequest {
            filter: Some(EventFilter::default()),
            ..SubscribeEventsRequest::default()
        })
        .await
        .expect("subscribe")
        .into_inner();
    let t = Instant::now();
    server.stop().await.expect("stop");
    let took = t.elapsed();
    assert!(took < SHUTDOWN_GRACE / 2, "stop took {took:?}");
    assert_eq!(ending(&mut w).await.code(), Code::Unavailable);
    assert_eq!(ending(&mut ev).await.code(), Code::Unavailable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_from_a_listed_revision_sees_a_death_in_between() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "between").await;
    let g = CommandsServiceClient::new(ch.clone())
        .put_group(group(&l, "g", c, 0.0))
        .await
        .expect("put")
        .into_inner();
    until_state(&hub, c, g.group_id, EntityState::Live).await;
    let area = WorldFilter {
        campaign_id: c,
        center: at(0.0, 0.0),
        radius: 200.0,
        ..WorldFilter::default()
    };
    let mut list = WorldServiceClient::new(ch.clone())
        .list_world(ListWorldRequest {
            filter: Some(area.clone()),
        })
        .await
        .expect("list")
        .into_inner();
    let mut rev = 0;
    while let Some(m) = list.message().await.expect("page") {
        rev = m.revision;
        assert_eq!(m.entities.len(), 1);
    }
    // The group dies after the list and before the watch: in the store, and
    // in DCS too, or a later OBJECT for the living unit revives it.
    {
        let mut h = hub.lock();
        let store = &mut h.campaigns.get_mut(&c).expect("c").store;
        store.death(EntityId(g.unit_ids[0])).expect("death");
        let sim = h
            .backend
            .as_any()
            .and_then(|a| a.downcast_mut::<FakeSim>())
            .expect("the fake sim");
        let object = sim
            .objects()
            .into_iter()
            .find(|(_, camp, e)| *camp == c && *e == g.unit_ids[0])
            .map(|(o, _, _)| o)
            .expect("DCS holds the unit");
        sim.kill(object);
    }
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            filter: Some(area),
            since_revision: rev,
            rate: pb::RateClass::Slow as i32,
            min_period: None,
        })
        .await
        .expect("watch")
        .into_inner();
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let m = w.message().await.expect("message").expect("open");
            for ch in m.changes {
                if let Some(world_change::Change::Upsert(pb::Entity {
                    kind: Some(entity::Kind::Group(x)),
                })) = ch.change
                {
                    return x;
                }
            }
        }
    })
    .await
    .expect("the group's last state");
    assert!(!seen.units[0].alive);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moving_group_reaches_a_watch_as_positions_not_upserts() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "moving").await;
    let mut req = group(&l, "g", c, 0.0);
    // A long route: the group is still moving when the count ends.
    req.order = Some(Order {
        intent: Some(order::Intent::Route(pb::RouteOrder {
            waypoints: vec![pb::Waypoint {
                position: at(200_000.0, 0.0),
                speed: 10.0,
                on_road: false,
                extra: None,
            }],
        })),
    });
    let g = CommandsServiceClient::new(ch.clone())
        .put_group(req)
        .await
        .expect("put")
        .into_inner();
    until_state(&hub, c, g.group_id, EntityState::Live).await;
    let rev = hub.lock().campaigns[&c].store.revision();
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                ..WorldFilter::default()
            }),
            since_revision: rev,
            rate: pb::RateClass::Normal as i32,
            min_period: None,
        })
        .await
        .expect("watch")
        .into_inner();
    let (mut upserts, mut batches, mut moved) = (0, 0, false);
    let end = Instant::now() + Duration::from_millis(2_500);
    let mut first_north = None;
    while Instant::now() < end {
        let Ok(m) = tokio::time::timeout(Duration::from_millis(200), w.message()).await else {
            continue;
        };
        for ch in m.expect("message").expect("open").changes {
            match ch.change {
                Some(world_change::Change::Upsert(_)) => upserts += 1,
                Some(world_change::Change::Positions(b)) => {
                    batches += 1;
                    let n = b.north[0];
                    moved |= first_north.is_some_and(|f: f64| n > f);
                    first_north.get_or_insert(n);
                }
                _ => {}
            }
        }
    }
    // Positions moved the store revision on every step; none of it is an
    // upsert. Batches come once per second at the normal rate.
    assert!(moved, "the group moved");
    assert!(upserts <= 1, "{upserts} upserts");
    assert!((2..=4).contains(&batches), "{batches} batches");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_that_stops_reading_ends_too_slow_after_history() {
    let (hub, ch) = in_process(
        Config {
            stream_buffer: 4,
            ..cfg()
        },
        fake_config(),
    )
    .await
    .expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "slot").await;
    // Let the campaign's mission load finish, so its lifecycle events are in
    // the log before the history is counted.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let h = hub.lock();
                if h.link.frame_campaign == c && h.link.lifecycle == 3 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("running");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let first = {
        let mut g = hub.lock();
        let first = g.campaigns[&c].events.last() + 1;
        // Three history events fill a buffer of four but its last slot.
        for i in 0..3 {
            g.publish(pb::event::Body::World(pb::WorldEvent {
                dcs_id: i,
                ..pb::WorldEvent::default()
            }));
        }
        first
    };
    let svc = Events(Arc::clone(&hub));
    let mut s = svc
        .subscribe_events(Request::new(SubscribeEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                ..EventFilter::default()
            }),
            after_sequence: first - 1,
            live_only: false,
        }))
        .await
        .expect("subscribe")
        .into_inner();
    // History fills the buffer but its last slot; then a live event.
    tokio::time::sleep(Duration::from_millis(50)).await;
    hub.lock().publish(pb::event::Body::World(pb::WorldEvent {
        dcs_id: 99,
        ..pb::WorldEvent::default()
    }));
    hub.bump();
    // Read only once the live event has met the full buffer.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut ok = 0;
    let end = loop {
        match tokio::time::timeout(Duration::from_secs(10), s.next()).await {
            Ok(Some(Ok(_))) => ok += 1,
            Ok(Some(Err(e))) => break e,
            Ok(None) => panic!("the stream ended with no status"),
            Err(_) => panic!("no end after {ok} events"),
        }
    };
    assert_eq!(ok, 3);
    assert_eq!(reason(&end), ErrorReason::ConsumerTooSlow);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_campaign_0_subscription_made_between_campaigns_ends_on_activation() {
    let (_hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let mut ev = EventsServiceClient::new(ch.clone())
        .subscribe_events(SubscribeEventsRequest {
            filter: Some(EventFilter::default()),
            ..SubscribeEventsRequest::default()
        })
        .await
        .expect("subscribe")
        .into_inner();
    // A subscription to the server log by name stays.
    let mut server_log = EventsServiceClient::new(ch.clone())
        .subscribe_events(SubscribeEventsRequest {
            filter: Some(EventFilter {
                server_log: true,
                ..EventFilter::default()
            }),
            live_only: true,
            ..SubscribeEventsRequest::default()
        })
        .await
        .expect("subscribe")
        .into_inner();
    campaign(&ch, &l, "later").await;
    let e = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ev.message().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("ended with no status"),
                Err(e) => return e,
            }
        }
    })
    .await
    .expect("the stream ends");
    assert_eq!(reason(&e), ErrorReason::CampaignNotActive);
    // The server-log subscription stays open and delivers.
    {
        let mut g = _hub.lock();
        let stamp = g.link.stamp();
        g.server_log.push(
            0,
            stamp,
            pb::event::Body::World(pb::WorldEvent {
                dcs_id: 4242,
                ..pb::WorldEvent::default()
            }),
        );
    }
    _hub.bump();
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let m = server_log
                .message()
                .await
                .expect("open")
                .expect("not ended");
            if let Some(pb::event::Body::World(w)) = m.event.and_then(|e| e.body)
                && w.dcs_id == 4242
            {
                return;
            }
        }
    })
    .await;
    assert!(got.is_ok(), "the server-log subscription delivers");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_put_group_changes_nothing_and_its_key_runs_once() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "atomic").await;
    let before = hub.lock().campaigns[&c].store.revision();
    let mut bad = group(&l, "k", c, 0.0);
    bad.spec
        .as_mut()
        .expect("spec")
        .components
        .insert(String::new(), prost_types::Struct::default());
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let e = cmd.put_group(bad).await.expect_err("empty name");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
    {
        // No group, and no change since the call named an entity: the
        // store revision itself also moves with the reconciler's work.
        let g = hub.lock();
        let store = &g.campaigns[&c].store;
        assert_eq!(store.groups().count(), 0);
        assert!(store.changes().all(|(rev, id)| *rev <= before || id.0 == 0));
    }
    cmd.put_group(group(&l, "k", c, 0.0)).await.expect("fixed");
    cmd.put_group(group(&l, "k", c, 0.0)).await.expect("repeat");
    assert_eq!(hub.lock().campaigns[&c].store.groups().count(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_policies_take_effect() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "lost").await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    // The campaign's default: a lost group is dead.
    cmd.set_lost_policy(pb::SetLostPolicyRequest {
        write: write(&l, "default", c),
        group_id: 0,
        policy: pb::LostPolicy::Dead as i32,
    })
    .await
    .expect("default");
    let a = cmd
        .put_group(group(&l, "a", c, 0.0))
        .await
        .expect("a")
        .into_inner();
    let b = cmd
        .put_group(group(&l, "b", c, 5_000.0))
        .await
        .expect("b")
        .into_inner();
    // A's own policy: respawn.
    cmd.set_lost_policy(pb::SetLostPolicyRequest {
        write: write(&l, "a-respawn", c),
        group_id: a.group_id,
        policy: pb::LostPolicy::Respawn as i32,
    })
    .await
    .expect("respawn");
    let rev_a = until_state(&hub, c, a.group_id, EntityState::Live).await;
    until_state(&hub, c, b.group_id, EntityState::Live).await;
    vanish(&hub, c, &a.unit_ids);
    vanish(&hub, c, &b.unit_ids);
    // B, with the default, is marked dead. A's members left with no death
    // fact, so the respawn gate holds A as `lost`, and a mission load does
    // not re-create members that may be dead (spec/persistence.md,
    // "Restore").
    until_state(&hub, c, b.group_id, EntityState::Dead).await;
    let lost_rev = until_state(&hub, c, a.group_id, EntityState::Lost).await;
    {
        let g = hub.lock();
        let id = EntityId(a.group_id);
        assert!(g.rec.as_ref().expect("reconciler").held().contains(&id));
        assert!(g.campaigns[&c].store.gate_shut(id));
        assert!(g.rec.as_ref().expect("reconciler").counters().respawns_held >= 1);
    }
    // Hold for 1 s of sidecar steps. A silent removal leaves
    // `enumerated` true (no gap, no HELLO), so the respawn branch runs on every
    // step of the window: if the gate failed, the reconciler would respawn A
    // within a few steps. The count of enumerations does not matter.
    let hold = tokio::time::Instant::now() + Duration::from_secs(1);
    while tokio::time::Instant::now() < hold {
        let (s, rev) = group_of(&hub, c, a.group_id).expect("a");
        assert_eq!(
            (s, rev),
            (EntityState::Lost, lost_rev),
            "the gate lets A go"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The reload does not return A: its members keep their marks.
    {
        let mut g = hub.lock();
        g.backend
            .as_any()
            .and_then(|a| a.downcast_mut::<FakeSim>())
            .expect("the fake sim")
            .load_mission();
    }
    // The load takes A through `materializing` back to `lost`.
    let held_out = |hub: &Hub| {
        hub.lock().campaigns[&c].store.entries().iter().any(|e| {
            matches!(e, Entry::Transition { entity, from: State::Materializing, to: State::Lost, .. }
                if entity.0 == a.group_id)
        })
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        while !held_out(&hub) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("A held out after the load");
    let (state, again) = group_of(&hub, c, a.group_id).expect("a");
    assert_eq!(state, EntityState::Lost);
    assert!(again >= rev_a);
    assert!(
        hub.lock().campaigns[&c]
            .store
            .gate_shut(EntityId(a.group_id))
    );
    assert_eq!(
        group_of(&hub, c, b.group_id).map(|(s, _)| s),
        Some(EntityState::Dead)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn materialize_refuses_a_theater_that_is_not_loaded() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: "two-maps".into(),
            theaters: vec!["Caucasus".into(), "Syria".into()],
            first_theater: "Caucasus".into(),
            activate: true,
            idempotency: key("create/two-maps"),
            ..CreateCampaignRequest::default()
        })
        .await
        .expect("create")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    let mut req = group(&l, "syria", c, 0.0);
    let spec = req.spec.as_mut().expect("spec");
    spec.r#virtual = true;
    spec.units[0].position.as_mut().expect("pos").theater = "Syria".into();
    let g = CommandsServiceClient::new(ch.clone())
        .put_group(req)
        .await
        .expect("put")
        .into_inner();
    // Wait for the agent to run the campaign's mission.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let h = hub.lock();
                if h.link.frame_campaign == c && h.link.theater == "Caucasus" {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("loaded");
    let e = CommandsServiceClient::new(ch.clone())
        .materialize(pb::MaterializeRequest {
            write: write(&l, "mat", c),
            entity_ids: vec![g.group_id],
        })
        .await
        .expect_err("Syria is not loaded");
    assert_eq!(e.code(), Code::FailedPrecondition);
    assert_eq!(reason(&e), ErrorReason::TheaterNotLoaded);
}

/// Watch a virtual group (DCS does not move it) in an area, and move it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_that_leaves_an_area_and_returns_is_sent_again() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "return").await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let mut req = group(&l, "g", c, 0.0);
    req.spec.as_mut().expect("spec").r#virtual = true;
    let g = cmd.put_group(req).await.expect("put").into_inner();
    // A second virtual group far outside the area, which moves.
    let mut far = group(&l, "far", c, 50_000.0);
    far.spec.as_mut().expect("spec").r#virtual = true;
    let far = cmd.put_group(far).await.expect("far").into_inner();
    let rev = hub.lock().campaigns[&c].store.revision();
    let unit = EntityId(g.unit_ids[0]);
    let move_to = |north: f64, who: EntityId| {
        let mut h = hub.lock();
        h.campaigns.get_mut(&c).expect("c").store.observe_position(
            who,
            nanoprobe_sidecar::store::Point {
                x: north,
                y: 0.0,
                z: 0.0,
            },
        );
        drop(h);
        hub.bump();
    };
    move_to(50_100.0, EntityId(far.unit_ids[0]));
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                center: at(0.0, 0.0),
                radius: 200.0,
                ..WorldFilter::default()
            }),
            since_revision: rev,
            rate: pb::RateClass::Fast as i32,
            min_period: None,
        })
        .await
        .expect("watch")
        .into_inner();
    // Every upsert, by group, as (label, in the area).
    let mut seen: Vec<(u64, String, bool)> = Vec::new();
    let read = async |w: &mut tonic::Streaming<pb::WatchWorldResponse>,
                      seen: &mut Vec<(u64, String, bool)>| {
        while let Ok(m) = tokio::time::timeout(Duration::from_millis(300), w.message()).await {
            for ch in m.expect("message").expect("open").changes {
                if let Some(world_change::Change::Upsert(pb::Entity {
                    kind: Some(entity::Kind::Group(x)),
                })) = ch.change
                {
                    let n = x.units[0].position.as_ref().map_or(0.0, |p| p.north);
                    seen.push((x.id, x.label.clone(), n.abs() < 200.0));
                }
            }
        }
    };
    read(&mut w, &mut seen).await;
    // The far group's move shows once, as a last state during the catch-up
    // (world.proto, since_revision), at its new place.
    let far_seen: Vec<_> = seen.iter().filter(|x| x.0 == far.group_id).collect();
    assert_eq!(far_seen.len(), 1, "{seen:?}");
    assert!(!far_seen[0].2, "outside the area");
    // A later move of the far group is never sent.
    seen.clear();
    move_to(50_200.0, EntityId(far.unit_ids[0]));
    read(&mut w, &mut seen).await;
    assert!(seen.is_empty(), "{seen:?}");
    move_to(1_000.0, unit);
    read(&mut w, &mut seen).await;
    assert_eq!(seen, vec![(g.group_id, "g".into(), false)], "it left");
    // Its label changes while it is out: no upsert.
    {
        let mut h = hub.lock();
        let store = &mut h.campaigns.get_mut(&c).expect("c").store;
        store
            .set_label(EntityId(g.group_id), "renamed")
            .expect("label");
    }
    hub.bump();
    seen.clear();
    read(&mut w, &mut seen).await;
    assert!(seen.is_empty(), "{seen:?}");
    // It comes back: an upsert with its current state.
    move_to(0.0, unit);
    read(&mut w, &mut seen).await;
    assert_eq!(seen.first(), Some(&(g.group_id, "renamed".into(), true)));
    // Its label changes back to the old one: an upsert again.
    {
        let mut h = hub.lock();
        let store = &mut h.campaigns.get_mut(&c).expect("c").store;
        store.set_label(EntityId(g.group_id), "g").expect("label");
    }
    hub.bump();
    seen.clear();
    read(&mut w, &mut seen).await;
    assert_eq!(seen, vec![(g.group_id, "g".into(), true)]);
    assert!(seen.iter().all(|x| x.0 != far.group_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_before_any_write() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "refusals").await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let detail = |e: &tonic::Status| nanoprobe_proto::errors::detail(e).expect("detail").field;
    // A spec whose spawn would be over the agent's command limit.
    let mut big = group(&l, "big", c, 0.0);
    let text = "x".repeat(200 * 1024);
    big.spec.as_mut().expect("spec").extra = Some(prost_types::Struct {
        fields: [(
            "note".to_owned(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue(text)),
            },
        )]
        .into(),
    });
    let e = cmd.put_group(big).await.expect_err("too large");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
    assert_eq!(detail(&e), "spec");
    assert!(e.message().contains("would take up to"), "{}", e.message());
    // A route of 300 waypoints is over the limit, in PutGroup and SetOrder.
    let route = Order {
        intent: Some(order::Intent::Route(pb::RouteOrder {
            waypoints: (0..300)
                .map(|i| pb::Waypoint {
                    position: at(f64::from(i), 0.0),
                    speed: 5.0,
                    on_road: false,
                    extra: None,
                })
                .collect(),
        })),
    };
    let mut long = group(&l, "long", c, 0.0);
    long.order = Some(route.clone());
    let e = cmd.put_group(long).await.expect_err("route too long");
    assert_eq!(detail(&e), "order");
    // A static whose extra keys are over the limit.
    let e = cmd
        .put_static(pb::PutStaticRequest {
            write: write(&l, "static-big", c),
            static_id: 0,
            spec: Some(pb::StaticSpec {
                further_units: Vec::new(),
                label: "big".into(),
                country: 2,
                r#type: "Tent".into(),
                position: at(10.0, 10.0),
                heading: 0.0,
                dead: false,
                extra: Some(prost_types::Struct {
                    fields: [(
                        "note".to_owned(),
                        prost_types::Value {
                            kind: Some(prost_types::value::Kind::StringValue(
                                "x".repeat(200 * 1024),
                            )),
                        },
                    )]
                    .into(),
                }),
            }),
        })
        .await
        .expect_err("static too large");
    assert_eq!(detail(&e), "spec");
    // Neither wrote anything, and the big call's key was not remembered.
    assert_eq!(hub.lock().campaigns[&c].store.groups().count(), 0);
    let fitted = cmd
        .put_group(group(&l, "big", c, 0.0))
        .await
        .expect("the key runs once the spec fits")
        .into_inner();
    let e = cmd
        .set_order(pb::SetOrderRequest {
            write: write(&l, "set-long", c),
            group_id: fitted.group_id,
            order: Some(route),
        })
        .await
        .expect_err("route too long");
    assert_eq!(detail(&e), "order");
    {
        let g = hub.lock();
        let grp = g.campaigns[&c]
            .store
            .group(EntityId(fitted.group_id))
            .expect("group");
        assert_eq!(grp.order_rev, 0, "no order was written");
    }
    // Watch periods: a day or more, or negative, is refused; a short one
    // is served at the cap.
    let mut world = WorldServiceClient::new(ch.clone());
    for (secs, nanos, ok) in [(86_400, 0, false), (-1, 0, false), (0, 1_000_000, true)] {
        let r = world
            .watch_world(WatchWorldRequest {
                filter: Some(WorldFilter {
                    campaign_id: c,
                    ..WorldFilter::default()
                }),
                since_revision: 0,
                rate: 0,
                min_period: Some(prost_types::Duration {
                    seconds: secs,
                    nanos,
                }),
            })
            .await;
        match (r, ok) {
            (Ok(_), true) => {}
            (Err(e), false) => assert_eq!(detail(&e), "min_period"),
            (r, _) => panic!("min_period {secs}s {nanos}ns: {:?}", r.map(|_| ())),
        }
    }
}
