//! The API on the fake sim: the hello campaign in-process and over a socket,
//! the lease's fencing, idempotency keys, bounded subscriptions, tokens and
//! reflection.

use std::sync::Arc;
use std::time::Duration;

use nanoprobe_api::auth::token_hash;
use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_api::services::events::Events;
use nanoprobe_api::services::world::World;
use nanoprobe_fake_sidecar::{fake_config, in_process, start};
use nanoprobe_proto::errors::reason;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_server::EventsService;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::world_service_server::WorldService;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, ErrorReason, EventFilter,
    GroupCategory, GroupSpec, Idempotency, Lease, ListWorldRequest, PutGroupRequest,
    RenewLeaseRequest, SubscribeEventsRequest, UnitSpec, WatchWorldRequest, WorldFilter, Write,
};
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

const LIMIT: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_campaign_in_process() {
    let (_hub, channel) = in_process(cfg(), fake_config()).await.expect("server");
    let s = nanoprobe_hello::run_on(channel, None, LIMIT)
        .await
        .expect("hello campaign");
    assert_eq!(
        s.states,
        vec![pb::EntityState::Materializing, pb::EntityState::Live]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_campaign_over_a_socket() {
    let server = start(cfg(), fake_config()).await.expect("server");
    let s = nanoprobe_hello::run(&server.endpoint(), None, LIMIT)
        .await
        .expect("hello campaign");
    assert!(s.states.contains(&pb::EntityState::Live));
    // A second run on the same server makes its own campaign.
    let t = nanoprobe_hello::run(&server.endpoint(), None, LIMIT)
        .await
        .expect("second run");
    assert_ne!(s.campaign, t.campaign);
    server.stop().await.expect("stop");
}

fn key(k: &str) -> Option<Idempotency> {
    Some(Idempotency { key: k.into() })
}

async fn lease(channel: &Channel) -> Result<Lease, tonic::Status> {
    let r = SessionServiceClient::new(channel.clone())
        .acquire_lease(AcquireLeaseRequest {
            client_name: "test".into(),
        })
        .await?;
    Ok(r.into_inner().lease.expect("lease"))
}

async fn campaign(channel: &Channel, l: &Lease, name: &str) -> u64 {
    CampaignServiceClient::new(channel.clone())
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

fn group(l: &Lease, k: &str, campaign: u64, label: &str) -> PutGroupRequest {
    PutGroupRequest {
        write: Some(Write {
            lease: Some(l.clone()),
            idempotency: key(k),
            campaign_id: campaign,
        }),
        group_id: 0,
        spec: Some(GroupSpec {
            label: label.into(),
            country: 2,
            category: GroupCategory::Ground as i32,
            units: vec![UnitSpec {
                r#type: "M-113".into(),
                position: Some(pb::Position {
                    theater: "Caucasus".into(),
                    north: 100.0,
                    east: 200.0,
                    alt: 0.0,
                }),
                ..UnitSpec::default()
            }],
            ..GroupSpec::default()
        }),
        order: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_controller_is_fenced_after_a_takeover() {
    let (_hub, ch) = in_process(
        Config {
            lease_ttl: Duration::from_secs(1),
            ..cfg()
        },
        fake_config(),
    )
    .await
    .expect("server");
    let a = lease(&ch).await.expect("A takes the lease");
    let held = lease(&ch).await.expect_err("B is refused");
    assert_eq!(held.code(), Code::FailedPrecondition);
    assert_eq!(reason(&held), ErrorReason::LeaseHeld);
    let c = campaign(&ch, &a, "fence").await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    SessionServiceClient::new(ch.clone())
        .renew_lease(RenewLeaseRequest {
            lease: Some(a.clone()),
        })
        .await
        .expect("A renews");
    cmd.put_group(group(&a, "a1", c, "a1"))
        .await
        .expect("A writes");

    // A stops renewing; B takes over under a higher epoch.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let b = lease(&ch).await.expect("B takes over");
    assert!(b.epoch > a.epoch);

    // A comes back: its writes and its renewal are LEASE_LOST.
    let lost = cmd
        .put_group(group(&a, "a2", c, "a2"))
        .await
        .expect_err("A is fenced");
    assert_eq!(lost.code(), Code::Aborted);
    assert_eq!(reason(&lost), ErrorReason::LeaseLost);
    let lost = SessionServiceClient::new(ch.clone())
        .renew_lease(RenewLeaseRequest {
            lease: Some(a.clone()),
        })
        .await
        .expect_err("A cannot renew");
    assert_eq!(reason(&lost), ErrorReason::LeaseLost);
    // A forging B's epoch without its ID is not the controller.
    let forged = Lease {
        epoch: b.epoch,
        lease_id: a.lease_id.clone(),
    };
    let e = cmd
        .put_group(group(&forged, "a3", c, "a3"))
        .await
        .expect_err("forged");
    assert_eq!(reason(&e), ErrorReason::NotController);
    // No lease at all.
    let mut none = group(&b, "a4", c, "a4");
    none.write.as_mut().expect("write").lease = None;
    let e = cmd.put_group(none).await.expect_err("no lease");
    assert_eq!(reason(&e), ErrorReason::NotController);
    // B writes; A's rejected attempts left nothing behind.
    cmd.put_group(group(&b, "b1", c, "b1"))
        .await
        .expect("B writes");
    let labels = labels(&ch, c).await;
    assert_eq!(labels, vec!["a1".to_owned(), "b1".to_owned()]);
}

async fn labels(ch: &Channel, campaign: u64) -> Vec<String> {
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
    let mut out = Vec::new();
    while let Some(m) = s.message().await.expect("page") {
        for e in m.entities {
            if let Some(pb::entity::Kind::Group(g)) = e.kind {
                out.push(g.label);
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idempotency_key_runs_a_call_once() {
    let (_hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await.expect("lease");
    let c = campaign(&ch, &l, "idem").await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let first = cmd
        .put_group(group(&l, "k", c, "one"))
        .await
        .expect("first")
        .into_inner();
    let again = cmd
        .put_group(group(&l, "k", c, "one"))
        .await
        .expect("repeat")
        .into_inner();
    assert_eq!(first, again);
    let e = cmd
        .put_group(group(&l, "k", c, "two"))
        .await
        .expect_err("another request under the key");
    assert_eq!(e.code(), Code::InvalidArgument);
    assert_eq!(reason(&e), ErrorReason::IdempotencyMismatch);
    let e = cmd
        .put_group(group(&l, "", c, "three"))
        .await
        .expect_err("no key");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
    // The lease is left out of the comparison: a repeat under a new lease
    // returns the first response.
    SessionServiceClient::new(ch.clone())
        .release_lease(pb::ReleaseLeaseRequest {
            lease: Some(l.clone()),
        })
        .await
        .expect("release");
    let l2 = lease(&ch).await.expect("new lease");
    let third = cmd
        .put_group(group(&l2, "k", c, "one"))
        .await
        .expect("repeat under a new lease")
        .into_inner();
    assert_eq!(first, third);
    // A failed call is not remembered: the stale lease fails, and the same
    // key then runs under the current one.
    let e = cmd
        .put_group(group(&l, "k2", c, "four"))
        .await
        .expect_err("stale");
    assert_eq!(reason(&e), ErrorReason::LeaseLost);
    cmd.put_group(group(&l2, "k2", c, "four"))
        .await
        .expect("the key runs once the lease is right");
    assert_eq!(
        labels(&ch, c).await,
        vec!["one".to_owned(), "four".to_owned()]
    );
    // GetOutcome finds the key and its campaign.
    let o = cmd
        .get_outcome(pb::GetOutcomeRequest {
            idempotency_key: "k".into(),
        })
        .await
        .expect("outcome")
        .into_inner();
    assert_eq!(o.campaign_id, c);
    // Ruling B: a set-state call has no outcome of its own.
    assert!(o.result.is_none() && o.value.is_none() && o.entity.is_none());
    let e = cmd
        .get_outcome(pb::GetOutcomeRequest {
            idempotency_key: "never".into(),
        })
        .await
        .expect_err("unknown key");
    assert_eq!(reason(&e), ErrorReason::OutcomeNotFound);
}

/// A hub with an active campaign of `groups` groups, driven by its own task.
async fn busy_hub(cfg: Config, groups: usize) -> (Arc<Hub>, Channel, Lease, u64) {
    let (hub, ch) = in_process(cfg, fake_config()).await.expect("server");
    let l = lease(&ch).await.expect("lease");
    let c = campaign(&ch, &l, "busy").await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    for i in 0..groups {
        cmd.put_group(group(&l, &format!("g{i}"), c, &format!("g{i}")))
            .await
            .expect("put");
    }
    (hub, ch, l, c)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_that_is_not_read_ends_too_slow() {
    let (hub, ch, l, c) = busy_hub(
        Config {
            stream_buffer: 4,
            ..cfg()
        },
        0,
    )
    .await;
    // Call the service directly: its stream is the bounded channel itself,
    // with no transport buffer in between.
    let svc = World(Arc::clone(&hub));
    let mut stream = svc
        .watch_world(Request::new(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                ..WorldFilter::default()
            }),
            since_revision: 0,
            rate: pb::RateClass::Fast as i32,
            min_period: None,
        }))
        .await
        .expect("watch")
        .into_inner();
    // Changes the watch must send, one or more per message.
    let mut cmd = CommandsServiceClient::new(ch.clone());
    for i in 0..20 {
        cmd.put_group(group(&l, &format!("w{i}"), c, "w"))
            .await
            .expect("put");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let mut ok = 0;
    let end = loop {
        match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
            Ok(Some(Ok(_))) => ok += 1,
            Ok(Some(Err(e))) => break e,
            Ok(None) => panic!("the stream ended without an error"),
            Err(_) => panic!("the stream neither sent nor ended"),
        }
    };
    assert!(ok <= 3, "{ok} messages before the end");
    assert_eq!(end.code(), Code::ResourceExhausted);
    assert_eq!(reason(&end), ErrorReason::ConsumerTooSlow);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_resumes_from_a_listed_revision_and_expires() {
    let (hub, ch, l, c) = busy_hub(
        Config {
            world_changes_keep: 4,
            ..cfg()
        },
        6,
    )
    .await;
    let mut world = WorldServiceClient::new(ch.clone());
    let e = world
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                ..WorldFilter::default()
            }),
            since_revision: 0,
            ..WatchWorldRequest::default()
        })
        .await
        .expect_err("revision 0 is no longer kept");
    assert_eq!(e.code(), Code::OutOfRange);
    assert_eq!(reason(&e), ErrorReason::RevisionExpired);

    // List, then watch from the listed revision: the next change arrives as
    // an upsert at a later revision.
    let rev = {
        let g = hub.lock();
        g.campaigns[&c].store.revision()
    };
    let mut w = world
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                kinds: vec![pb::EntityKind::Group as i32],
                ..WorldFilter::default()
            }),
            since_revision: rev,
            rate: pb::RateClass::Slow as i32,
            min_period: None,
        })
        .await
        .expect("watch")
        .into_inner();
    let put = CommandsServiceClient::new(ch.clone())
        .put_group(group(&l, "late", c, "late"))
        .await
        .expect("put")
        .into_inner();
    let seen = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let m = w.message().await.expect("message").expect("open");
            assert!(m.revision >= rev);
            for ch in m.changes {
                if let Some(pb::world_change::Change::Upsert(e)) = ch.change
                    && let Some(pb::entity::Kind::Group(g)) = e.kind
                    && g.id == put.group_id
                {
                    return g.label;
                }
            }
        }
    })
    .await
    .expect("the change arrives");
    assert_eq!(seen, "late");

    // Asking for units directly is refused: they are listed in groups.
    let e = world
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                kinds: vec![pb::EntityKind::Unit as i32],
                ..WorldFilter::default()
            }),
            ..WatchWorldRequest::default()
        })
        .await
        .expect_err("units");
    assert_eq!(reason(&e), ErrorReason::InvalidField);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streams_on_the_active_campaign_end_when_it_changes() {
    let (_hub, ch, l, _c) = busy_hub(cfg(), 1).await;
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            since_revision: 1,
            ..WatchWorldRequest::default()
        })
        .await
        .expect("watch")
        .into_inner();
    let mut ev = pb::events_service_client::EventsServiceClient::new(ch.clone())
        .subscribe_events(SubscribeEventsRequest {
            filter: Some(EventFilter::default()),
            ..SubscribeEventsRequest::default()
        })
        .await
        .expect("subscribe")
        .into_inner();
    campaign(&ch, &l, "other").await;
    for e in [drain(&mut w).await, drain(&mut ev).await] {
        assert_eq!(e.code(), Code::FailedPrecondition);
        assert_eq!(reason(&e), ErrorReason::CampaignNotActive);
    }
}

async fn drain<T>(s: &mut tonic::Streaming<T>) -> tonic::Status {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match s.message().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("ended without an error"),
                Err(e) => return e,
            }
        }
    })
    .await
    .expect("the stream ends")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_resume_in_order_and_a_slow_subscriber_is_ended() {
    let (hub, _ch, _l, c) = busy_hub(
        Config {
            stream_buffer: 4,
            events_keep: 50,
            ..cfg()
        },
        0,
    )
    .await;
    // Fill the campaign's log.
    {
        let mut g = hub.lock();
        for i in 0..30 {
            g.publish(pb::event::Body::World(pb::WorldEvent {
                dcs_id: i,
                ..pb::WorldEvent::default()
            }));
        }
    }
    let svc = Events(Arc::clone(&hub));
    let read = svc
        .read_events(Request::new(pb::ReadEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                ..EventFilter::default()
            }),
            after_sequence: 0,
            limit: 10,
        }))
        .await
        .expect("read")
        .into_inner();
    assert!(read.more);
    let seqs: Vec<u64> = read.events.iter().map(|e| e.sequence).collect();
    assert_eq!(seqs, (1..=10).collect::<Vec<_>>());
    let last = seqs[9];
    // Resume after the last one read: in order, with no hole.
    let read = svc
        .read_events(Request::new(pb::ReadEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                ..EventFilter::default()
            }),
            after_sequence: last,
            limit: 10,
        }))
        .await
        .expect("read")
        .into_inner();
    let seqs: Vec<u64> = read.events.iter().map(|e| e.sequence).collect();
    assert_eq!(seqs, (11..=20).collect::<Vec<_>>());
    // A DCS ID filter.
    let read = svc
        .read_events(Request::new(pb::ReadEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                dcs_ids: vec![12, 13],
                ..EventFilter::default()
            }),
            after_sequence: last,
            limit: 0,
        }))
        .await
        .expect("read")
        .into_inner();
    let ids: Vec<u32> = read
        .events
        .iter()
        .filter_map(|e| match &e.body {
            Some(pb::event::Body::World(w)) => Some(w.dcs_id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![12, 13]);
    // A live subscriber that does not read is ended.
    let mut s = svc
        .subscribe_events(Request::new(SubscribeEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                ..EventFilter::default()
            }),
            after_sequence: 0,
            live_only: true,
        }))
        .await
        .expect("subscribe")
        .into_inner();
    // Let the subscription reach the live tail before the burst.
    tokio::time::sleep(Duration::from_millis(50)).await;
    {
        let mut g = hub.lock();
        for i in 0..20 {
            g.publish(pb::event::Body::World(pb::WorldEvent {
                dcs_id: 500 + i,
                ..pb::WorldEvent::default()
            }));
        }
    }
    hub.bump();
    let mut ok = 0;
    let end = loop {
        match tokio::time::timeout(Duration::from_secs(10), s.next()).await {
            Ok(Some(Ok(_))) => ok += 1,
            Ok(Some(Err(e))) => break e,
            _ => panic!("no end"),
        }
    };
    assert!(ok <= 3);
    assert_eq!(reason(&end), ErrorReason::ConsumerTooSlow);
    // Past the retained history: SEQUENCE_EXPIRED.
    {
        let mut g = hub.lock();
        for i in 0..60 {
            g.publish(pb::event::Body::World(pb::WorldEvent {
                dcs_id: 100 + i,
                ..pb::WorldEvent::default()
            }));
        }
    }
    let e = svc
        .read_events(Request::new(pb::ReadEventsRequest {
            filter: Some(EventFilter {
                campaign_id: c,
                ..EventFilter::default()
            }),
            after_sequence: 5,
            limit: 0,
        }))
        .await
        .expect_err("expired");
    assert_eq!(e.code(), Code::OutOfRange);
    assert_eq!(reason(&e), ErrorReason::SequenceExpired);
}

const OBSERVER_TOKEN: &str = "observer-0123456789abcdef0123456789";
const CONTROLLER_TOKEN: &str = "controller-0123456789abcdef01234567";

/// A scratch folder, removed when the test ends, whether it passes or fails.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokens_give_roles() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("nanoprobe-tokens-{}", std::process::id())));
    let dir = scratch.0.clone();
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("tokens.toml");
    std::fs::write(
        &path,
        format!(
            "[[token]]\nname = \"watcher\"\nrole = \"observer\"\nsha256 = \"{}\"\n\n\
             [[token]]\nname = \"campaign\"\nrole = \"controller\"\nsha256 = \"{}\"\n",
            token_hash(OBSERVER_TOKEN),
            token_hash(CONTROLLER_TOKEN),
        ),
    )
    .expect("write");
    let server = start(
        Config {
            tokens_path: Some(path),
            ..cfg()
        },
        fake_config(),
    )
    .await
    .expect("server");
    let ch = Channel::from_shared(server.endpoint())
        .expect("endpoint")
        .connect()
        .await
        .expect("connect");
    let with = |t: &'static str| {
        assert!(t.len() >= nanoprobe_api::auth::MIN_TOKEN_LEN);
        move |mut r: Request<()>| {
            r.metadata_mut().insert(
                "authorization",
                format!("Bearer {t}").parse().expect("ascii"),
            );
            Ok(r)
        }
    };
    let e = SessionServiceClient::new(ch.clone())
        .hello(pb::HelloRequest::default())
        .await
        .expect_err("no token");
    assert_eq!(e.code(), Code::Unauthenticated);
    assert_eq!(reason(&e), ErrorReason::TokenRequired);
    let mut obs = SessionServiceClient::with_interceptor(ch.clone(), with(OBSERVER_TOKEN));
    let hello = obs
        .hello(pb::HelloRequest::default())
        .await
        .expect("hello")
        .into_inner();
    assert_eq!(hello.role, pb::Role::Observer as i32);
    let e = obs
        .acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect_err("observer");
    assert_eq!(e.code(), Code::PermissionDenied);
    assert_eq!(reason(&e), ErrorReason::RoleRequired);
    // A changing call that reads a file is a controller's too, and the role
    // is checked before the path is looked at.
    let e = CommandsServiceClient::with_interceptor(ch.clone(), with(OBSERVER_TOKEN))
        .put_group_from_template(pb::PutGroupFromTemplateRequest {
            path: "/nowhere/templates.lua".into(),
            template: "t".into(),
            ..pb::PutGroupFromTemplateRequest::default()
        })
        .await
        .expect_err("observer");
    assert_eq!(e.code(), Code::PermissionDenied);
    assert_eq!(reason(&e), ErrorReason::RoleRequired);
    let mut ctl = SessionServiceClient::with_interceptor(ch.clone(), with(CONTROLLER_TOKEN));
    ctl.acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect("controller");
    // Reflection needs a token too.
    {
        use tonic_reflection::pb::v1::ServerReflectionRequest;
        use tonic_reflection::pb::v1::server_reflection_client::ServerReflectionClient;
        use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
        let req = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(MessageRequest::ListServices(String::new())),
        };
        let e = ServerReflectionClient::new(ch.clone())
            .server_reflection_info(tokio_stream::iter([req]))
            .await
            .expect_err("no token");
        assert_eq!(reason(&e), ErrorReason::TokenRequired);
    }
    server.stop().await.expect("stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_listener_setting_refuses_to_start() {
    let e = start(
        Config {
            listen: "0.0.0.0:0".parse().expect("addr"),
            ..cfg()
        },
        fake_config(),
    )
    .await
    .err()
    .expect("refused");
    assert!(e.contains("not loopback"), "{e}");
    // A tokens file with one defect, an unknown role, on a port this test
    // holds: the refusal is the file's, so nothing was bound first.
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let port = held.local_addr().expect("addr").port();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("nanoprobe-badtokens-{}", std::process::id())));
    let dir = scratch.0.clone();
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("tokens.toml");
    std::fs::write(
        &path,
        format!(
            "[[token]]\nname = \"x\"\nrole = \"root\"\nsha256 = \"{}\"\n",
            token_hash(CONTROLLER_TOKEN)
        ),
    )
    .expect("write");
    let e = start(
        Config {
            listen: format!("127.0.0.1:{port}").parse().expect("addr"),
            tokens_path: Some(path),
            ..cfg()
        },
        fake_config(),
    )
    .await
    .err()
    .expect("a bad tokens file refuses to start");
    assert!(e.contains("unknown role"), "{e}");
    drop(held);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reflection_lists_the_services() {
    use tonic_reflection::pb::v1::ServerReflectionRequest;
    use tonic_reflection::pb::v1::server_reflection_client::ServerReflectionClient;
    use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
    use tonic_reflection::pb::v1::server_reflection_response::MessageResponse;

    let (_hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let req = ServerReflectionRequest {
        host: String::new(),
        message_request: Some(MessageRequest::ListServices(String::new())),
    };
    let mut s = ServerReflectionClient::new(ch)
        .server_reflection_info(tokio_stream::iter([req]))
        .await
        .expect("reflection")
        .into_inner();
    let r = s.message().await.expect("message").expect("response");
    let Some(MessageResponse::ListServicesResponse(l)) = r.message_response else {
        panic!("not a service list");
    };
    let names: Vec<String> = l.service.into_iter().map(|s| s.name).collect();
    for want in [
        "nanoprobe.v1alpha1.SessionService",
        "nanoprobe.v1alpha1.WorldService",
        "nanoprobe.v1alpha1.EventsService",
        "nanoprobe.v1alpha1.CommandsService",
        "nanoprobe.v1alpha1.CampaignService",
        "nanoprobe.v1alpha1.MissionService",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "{want} missing from {names:?}"
        );
    }
}
