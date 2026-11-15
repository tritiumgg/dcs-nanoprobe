//! Each implemented call's success path and its main refusal, campaign
//! switching and the restart path, area watches, event resumption, and the
//! stream watcher.

use std::sync::Arc;
use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_api::services::events::Events;
use nanoprobe_fake_sidecar::{fake_config, in_process, start};
use nanoprobe_proto::errors::reason;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_server::EventsService;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityState, ErrorReason, EventFilter,
    GroupCategory, GroupSpec, Idempotency, Lease, ListWorldRequest, Order, PutGroupRequest,
    SubscribeEventsRequest, UnitSpec, WatchWorldRequest, WorldFilter, Write, entity, order,
    world_change,
};
use nanoprobe_sidecar::ids::EntityId;
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

async fn campaign(ch: &Channel, l: &Lease, name: &str, activate: bool) -> u64 {
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

/// Wait until the group satisfies `ok`, polling the list.
async fn until(ch: &Channel, campaign: u64, id: u64, ok: impl Fn(&pb::Group) -> bool) -> pb::Group {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(g) = get_group(ch, campaign, id).await
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

fn live(g: &pb::Group) -> bool {
    g.state == EntityState::Live as i32
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orders_statics_authority_and_policy() {
    let (_hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "calls", true).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let g = cmd
        .put_group(group(&l, "g", c, 0.0))
        .await
        .expect("put")
        .into_inner();
    until(&ch, c, g.group_id, live).await;

    // SetOrder moves the live group.
    let r = cmd
        .set_order(pb::SetOrderRequest {
            write: write(&l, "order", c),
            group_id: g.group_id,
            order: Some(Order {
                intent: Some(order::Intent::Route(pb::RouteOrder {
                    waypoints: vec![pb::Waypoint {
                        position: at(500.0, 0.0),
                        speed: 10.0,
                        on_road: false,
                        extra: None,
                    }],
                })),
            }),
        })
        .await
        .expect("order")
        .into_inner();
    assert_eq!(r.order_revision, 1);
    let moved = until(&ch, c, g.group_id, |x| {
        x.units[0]
            .position
            .as_ref()
            .is_some_and(|p| (p.north - 500.0).abs() < 1.0)
    })
    .await;
    assert_eq!(moved.order_revision, 1);
    // An order to a static or an unknown ID is refused.
    let st = cmd
        .put_static(pb::PutStaticRequest {
            write: write(&l, "static", c),
            static_id: 0,
            spec: Some(pb::StaticSpec {
                label: "tent".into(),
                country: 2,
                r#type: "Tent".into(),
                position: at(-50.0, 0.0),
                heading: 0.0,
                extra: None,
            }),
        })
        .await
        .expect("static")
        .into_inner();
    let e = cmd
        .set_order(pb::SetOrderRequest {
            write: write(&l, "order-static", c),
            group_id: st.static_id,
            order: Some(Order {
                intent: Some(order::Intent::Hold(pb::HoldOrder {})),
            }),
        })
        .await
        .expect_err("a static takes no order");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
    // A position on a theater the campaign does not use is refused.
    let mut off = group(&l, "off", c, 0.0);
    off.spec.as_mut().expect("spec").units[0]
        .position
        .as_mut()
        .expect("pos")
        .theater = "Syria".into();
    let e = cmd.put_group(off).await.expect_err("theater");
    assert_eq!(
        nanoprobe_proto::errors::detail(&e).expect("detail").field,
        "spec.units[0].position.theater"
    );

    // Dematerialize, then materialize again.
    cmd.dematerialize(pb::DematerializeRequest {
        write: write(&l, "demat", c),
        entity_ids: vec![g.group_id],
    })
    .await
    .expect("dematerialize");
    until(&ch, c, g.group_id, |x| {
        x.state == EntityState::Virtual as i32
    })
    .await;
    cmd.materialize(pb::MaterializeRequest {
        write: write(&l, "mat", c),
        entity_ids: vec![g.group_id],
    })
    .await
    .expect("materialize");
    until(&ch, c, g.group_id, live).await;
    let e = cmd
        .materialize(pb::MaterializeRequest {
            write: write(&l, "mat-none", c),
            entity_ids: vec![987_654],
        })
        .await
        .expect_err("unknown");
    assert_eq!(reason(&e), ErrorReason::EntityNotFound);

    // Lost policy, per group and as the campaign's default.
    for (k, id) in [("lp-group", g.group_id), ("lp-default", 0)] {
        cmd.set_lost_policy(pb::SetLostPolicyRequest {
            write: write(&l, k, c),
            group_id: id,
            policy: pb::LostPolicy::Respawn as i32,
        })
        .await
        .expect("policy");
    }
    let e = cmd
        .set_lost_policy(pb::SetLostPolicyRequest {
            write: write(&l, "lp-none", c),
            group_id: g.group_id,
            policy: 0,
        })
        .await
        .expect_err("no policy");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
    // The default policy restarts the reconciler; the group stays live.
    until(&ch, c, g.group_id, live).await;

    // QueryArea finds the group near its new place and not far away.
    let mut world = WorldServiceClient::new(ch.clone());
    let near = world
        .query_area(pb::QueryAreaRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                center: at(500.0, 0.0),
                radius: 100.0,
                ..WorldFilter::default()
            }),
        })
        .await
        .expect("query")
        .into_inner();
    assert_eq!(near.entities.len(), 1);
    let e = world
        .query_area(pb::QueryAreaRequest {
            filter: Some(WorldFilter::default()),
        })
        .await
        .expect_err("no area");
    assert_eq!(reason(&e), ErrorReason::InvalidField);

    // Server status names the campaign and, once the reattach of the
    // default policy's restart is read, an attached agent.
    let st = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let st = MissionServiceClient::new(ch.clone())
                .get_server_status(pb::GetServerStatusRequest {})
                .await
                .expect("status")
                .into_inner()
                .status
                .expect("status");
            if st.attached {
                return st;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("attached again");
    assert_eq!(st.campaign_id, c);
    assert_eq!(st.health, pb::Health::Healthy as i32);
    assert_eq!(st.sim_state, pb::SimState::Running as i32);

    // A not-built call answers UNIMPLEMENTED with a detail.
    let e = cmd
        .do_effect(pb::DoEffectRequest {
            write: write(&l, "boom", c),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::Explosion(pb::Explosion::default())),
            }),
        })
        .await
        .expect_err("not built");
    assert_eq!(e.code(), Code::Unimplemented);
    assert_eq!(
        nanoprobe_proto::errors::detail(&e).expect("detail").field,
        "DoEffect for this effect"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn campaigns_switch_and_a_takeover_keeps_the_world() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let a = campaign(&ch, &l, "a", true).await;
    let b = campaign(&ch, &l, "b", false).await;
    let mut camp = CampaignServiceClient::new(ch.clone());
    let listed = camp
        .list_campaigns(pb::ListCampaignsRequest {})
        .await
        .expect("list")
        .into_inner()
        .campaigns;
    let states: Vec<(u64, i32)> = listed.iter().map(|x| (x.id, x.state)).collect();
    assert_eq!(
        states,
        vec![
            (a, pb::CampaignState::Active as i32),
            (b, pb::CampaignState::Suspended as i32)
        ]
    );
    // A spec written to suspended B reaches DCS only once B is active.
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let gb = cmd
        .put_group(group(&l, "in-b", b, 0.0))
        .await
        .expect("put to B")
        .into_inner();
    let e = cmd
        .materialize(pb::MaterializeRequest {
            write: write(&l, "mat-b", b),
            entity_ids: vec![gb.group_id],
        })
        .await
        .expect_err("B is not active");
    assert_eq!(reason(&e), ErrorReason::CampaignNotActive);
    camp.activate_campaign(pb::ActivateCampaignRequest {
        lease: Some(l.clone()),
        campaign_id: b,
        idempotency: key("activate-b"),
    })
    .await
    .expect("activate");
    until(&ch, b, gb.group_id, live).await;
    let got = camp
        .get_campaign(pb::GetCampaignRequest { campaign_id: 0 })
        .await
        .expect("get")
        .into_inner()
        .campaign
        .expect("campaign");
    assert_eq!(got.id, b);
    assert_eq!(got.loaded_theater, "Caucasus");

    // A takeover restarts the reconciler under a new command epoch; the
    // group is live again and DCS holds it once.
    SessionServiceClient::new(ch.clone())
        .release_lease(pb::ReleaseLeaseRequest {
            lease: Some(l.clone()),
        })
        .await
        .expect("release");
    let l2 = lease(&ch).await;
    until(&ch, b, gb.group_id, live).await;
    cmd.put_group(group(&l2, "after", b, 100.0))
        .await
        .expect("the new controller writes");

    // Suspending B tells the agent no campaign is active: its frames stamp
    // campaign 0 again, so its events go to the server log.
    camp.suspend_campaign(pb::SuspendCampaignRequest {
        lease: Some(l2.clone()),
        campaign_id: b,
        idempotency: key("suspend-b"),
    })
    .await
    .expect("suspend");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if hub.lock().link.frame_campaign == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the agent holds no campaign");
    let e = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest::default())
        .await
        .expect_err("no campaign to watch");
    assert_eq!(reason(&e), ErrorReason::NoActiveCampaign);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_area_watch_sees_a_death_and_label_changes() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "area", true).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let g = cmd
        .put_group(group(&l, "g", c, 0.0))
        .await
        .expect("put")
        .into_inner();
    until(&ch, c, g.group_id, live).await;
    let rev = hub.lock().campaigns[&c].store.revision();
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                center: at(0.0, 0.0),
                radius: 200.0,
                ..WorldFilter::default()
            }),
            since_revision: rev,
            rate: pb::RateClass::Slow as i32,
            min_period: None,
        })
        .await
        .expect("watch")
        .into_inner();
    // A label-only replacement reaches the watcher.
    let mut rep = group(&l, "relabel", c, 0.0);
    rep.group_id = g.group_id;
    let spec = rep.spec.as_mut().expect("spec");
    spec.label = "renamed".into();
    spec.units[0].id = g.unit_ids[0];
    spec.units[0].position = None;
    cmd.put_group(rep).await.expect("relabel");
    let seen = next_group(&mut w).await;
    assert_eq!(seen.label, "renamed");
    // The last unit dies inside the area: the watcher gets that state, even
    // though the group no longer passes the area filter.
    {
        let mut h = hub.lock();
        let store = &mut h.campaigns.get_mut(&c).expect("c").store;
        store.death(EntityId(g.unit_ids[0])).expect("death");
    }
    hub.bump();
    let seen = next_group(&mut w).await;
    assert!(!seen.units[0].alive);
}

/// The next group upsert on a watch.
async fn next_group(w: &mut tonic::Streaming<pb::WatchWorldResponse>) -> pb::Group {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let m = w.message().await.expect("message").expect("open");
            for ch in m.changes {
                if let Some(world_change::Change::Upsert(pb::Entity {
                    kind: Some(entity::Kind::Group(g)),
                })) = ch.change
                {
                    return g;
                }
            }
        }
    })
    .await
    .expect("an upsert")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_history_replays_to_a_prompt_reader_and_ahead_is_refused() {
    let (hub, ch) = in_process(
        Config {
            stream_buffer: 8,
            ..cfg()
        },
        fake_config(),
    )
    .await
    .expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l, "history", true).await;
    let first = {
        let mut g = hub.lock();
        let mut first = 0;
        for i in 0..30 {
            let s = g.publish(pb::event::Body::World(pb::WorldEvent {
                dcs_id: 1000 + i,
                ..pb::WorldEvent::default()
            }));
            if i == 0 {
                first = s;
            }
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
    // Thirty events through a buffer of eight, read promptly: no error.
    let mut got = Vec::new();
    while got.len() < 30 {
        let m = tokio::time::timeout(Duration::from_secs(10), s.next())
            .await
            .expect("an event")
            .expect("open")
            .expect("not too slow");
        let e = m.event.expect("event");
        if let Some(pb::event::Body::World(w)) = e.body {
            got.push(w.dcs_id);
        }
    }
    assert_eq!(got, (1000..1030).collect::<Vec<_>>());
    // A sequence number ahead of the log is refused, not silently skipped.
    let last = hub.lock().campaigns[&c].events.last();
    let e = svc
        .subscribe_events(Request::new(SubscribeEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                ..EventFilter::default()
            }),
            after_sequence: last + 100,
            live_only: false,
        }))
        .await
        .err()
        .expect("ahead");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
    let e = svc
        .read_events(Request::new(pb::ReadEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                ..EventFilter::default()
            }),
            after_sequence: last + 1,
            limit: 0,
        }))
        .await
        .expect_err("ahead");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
}

#[test]
fn the_stream_watcher_prints_events() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let server = rt.block_on(start(cfg(), fake_config())).expect("server");
    // Wait for the fake mission to load: its LIFECYCLE is in the server log.
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while server.hub.lock().server_log.last() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("an event in the server log");
    });
    let endpoint = server.endpoint();
    let watcher = std::thread::spawn(move || {
        let o = nanoprobe_cli::watch::Options {
            stream: nanoprobe_cli::watch::Stream::Events,
            addr: Some(endpoint),
            campaign: 0,
            since: None,
            token: None,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = nanoprobe_cli::watch::run(&o, &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned())
    });
    // Let it print, then stop the server: the stream ends and so does the
    // watcher, with exit code 1.
    std::thread::sleep(Duration::from_millis(500));
    rt.block_on(server.stop()).expect("stop");
    let (code, out) = watcher.join().expect("watcher");
    assert!(out.lines().any(|l| l.starts_with("event Event")), "{out}");
    assert!(out.contains("Lifecycle"), "{out}");
    assert_eq!(code, 1);
}

#[test]
fn the_stream_watcher_follows_the_listed_campaign_and_refuses_a_bad_token() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let server = rt.block_on(start(cfg(), fake_config())).expect("server");
    let endpoint = server.endpoint();
    let c = rt.block_on(async {
        let ch = Channel::from_shared(endpoint.clone())
            .expect("endpoint")
            .connect()
            .await
            .expect("connect");
        let l = lease(&ch).await;
        let c = campaign(&ch, &l, "watched", true).await;
        CommandsServiceClient::new(ch.clone())
            .put_group(group(&l, "g", c, 0.0))
            .await
            .expect("put");
        c
    });
    let o = nanoprobe_cli::watch::Options {
        stream: nanoprobe_cli::watch::Stream::World,
        addr: Some(endpoint.clone()),
        campaign: 0,
        since: None,
        token: None,
    };
    let watcher = std::thread::spawn(move || {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = nanoprobe_cli::watch::run(&o, &mut out, &mut err);
        (code, String::from_utf8_lossy(&out).into_owned())
    });
    std::thread::sleep(Duration::from_millis(500));
    rt.block_on(server.stop()).expect("stop");
    let (code, out) = watcher.join().expect("watcher");
    let id = format!("campaign_id: {c}");
    assert!(
        out.lines()
            .any(|l| l.starts_with("list ") && l.contains(&id)),
        "{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.starts_with("watch ") && l.contains(&id)),
        "{out}"
    );
    assert_eq!(code, 1);
    // A token that is not header text is an error before any call.
    let o = nanoprobe_cli::watch::Options {
        stream: nanoprobe_cli::watch::Stream::Events,
        addr: Some(endpoint),
        campaign: 0,
        since: None,
        token: Some("bad\ntoken".into()),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(nanoprobe_cli::watch::run(&o, &mut out, &mut err), 1);
    assert!(String::from_utf8_lossy(&err).contains("token"));
}

#[test]
fn the_stream_watcher_sends_its_token() {
    const TOKEN: &str = "watcher-0123456789abcdef0123456789abcdef";
    /// Removes the folder when the test ends, whether it passes or fails.
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch = Scratch(
        std::env::temp_dir().join(format!("nanoprobe-watch-tokens-{}", std::process::id())),
    );
    let dir = scratch.0.clone();
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("tokens.toml");
    std::fs::write(
        &path,
        format!(
            "[[token]]\nname = \"watcher\"\nrole = \"observer\"\nsha256 = \"{}\"\n",
            nanoprobe_api::auth::token_hash(TOKEN)
        ),
    )
    .expect("write");
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let server = rt
        .block_on(start(
            Config {
                tokens_path: Some(path),
                ..cfg()
            },
            fake_config(),
        ))
        .expect("server");
    let run = |token: &str| {
        let o = nanoprobe_cli::watch::Options {
            stream: nanoprobe_cli::watch::Stream::Events,
            addr: Some(server.endpoint()),
            campaign: 0,
            since: None,
            token: Some(token.to_owned()),
        };
        std::thread::spawn(move || {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = nanoprobe_cli::watch::run(&o, &mut out, &mut err);
            (
                code,
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
            )
        })
    };
    // A wrong token of valid length is refused by the server.
    let (code, _, err) = run(&"w".repeat(40)).join().expect("watcher");
    assert_eq!(code, 1);
    assert!(
        err.contains("Unauthenticated") || err.contains("token"),
        "{err}"
    );
    // The right one is let in: the stream runs until the server stops.
    let ok = run(TOKEN);
    std::thread::sleep(Duration::from_millis(500));
    rt.block_on(server.stop()).expect("stop");
    let (code, out, err) = ok.join().expect("watcher");
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("stopping"),
        "ended by the stop, not refused: {err}"
    );
    let _ = out;
}
