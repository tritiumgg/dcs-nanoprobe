//! Unmanaged units that players occupy, through the API on the fake sim:
//! adoption, the attribution of events (type names, late kills, launch
//! players) and the player keys in the server store.

use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::players_service_client::PlayersServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, Idempotency, Lease, Write,
};
use nanoprobe_sidecar::players::Keyring;
use nanoprobe_sidecar::server::ServerStore;
use nanoprobe_sim_fake::FakeSim;
use nanoprobe_transport::codec::Lv;
use tonic::Code;
use tonic::transport::Channel;

const UCID: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";

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

async fn lease(ch: &Channel) -> Lease {
    SessionServiceClient::new(ch.clone())
        .acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect("lease")
        .into_inner()
        .lease
        .expect("lease")
}

async fn campaign(ch: &Channel, l: &Lease) -> u64 {
    CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: "adoption".into(),
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
        .id
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

async fn world_events(ch: &Channel, c: u64, dcs_id: u32) -> Vec<pb::WorldEvent> {
    EventsServiceClient::new(ch.clone())
        .read_events(pb::ReadEventsRequest {
            filter: Some(pb::EventFilter {
                campaign_id: c,
                ..pb::EventFilter::default()
            }),
            after_sequence: 0,
            limit: 10_000,
        })
        .await
        .expect("events")
        .into_inner()
        .events
        .into_iter()
        .filter_map(|e| match e.body {
            Some(pb::event::Body::World(w)) if w.dcs_id == dcs_id => Some(w),
            _ => None,
        })
        .collect()
}

fn n(v: u64) -> Lv {
    Lv::Number(v as f64)
}

fn t(s: &str) -> Lv {
    Lv::String(s.as_bytes().to_vec())
}

/// Connect a player and put them in a new unmanaged unit; returns the
/// object ID once the API shows the player in a unit.
async fn pilot(hub: &Hub, ch: &Channel, group: u64) -> u64 {
    fake(hub, |s| {
        s.connect_client(2, UCID, "Ann", 2);
        s.client_slot(2, 2, "m1");
    });
    until(|| async { players_now(ch).await.len() == 1 }).await;
    let object = fake(hub, |s| s.birth_client_unit_in("m1", group, "F-16C_50"));
    until(|| async { players_now(ch).await[0].unit_id != 0 }).await;
    object
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_player_in_an_unmanaged_aircraft_enters_a_unit_with_an_id_and_is_subscribed() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    until(|| async { fake(&hub, |s| s.attached()) }).await;
    let object = pilot(&hub, &ch, 77).await;
    let p = players_now(&ch).await.remove(0);
    // The API shows the EntityId the sidecar allocated as soon as it joins the
    // occupant; the agent binds it when the BIND arrives, a step later.
    until(|| async { fake(&hub, |s| s.binding_of(object)).is_some_and(|(u, _)| u != 0) }).await;
    let (unit_entity, group_entity) = fake(&hub, |s| s.binding_of(object)).expect("a row");
    assert_eq!(p.unit_id, unit_entity);
    assert_ne!(group_entity, 0);
    assert_ne!(unit_entity, group_entity);

    // The unit is subscribed by its EntityId, and BIND went once for the
    // unit and once for its group, however many enumerations passed.
    until(|| async { fake(&hub, |s| s.subscribed().contains(&object)) }).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake(&hub, |s| s.binds()), 2);

    let events = EventsServiceClient::new(ch.clone())
        .read_events(pb::ReadEventsRequest {
            filter: Some(pb::EventFilter {
                campaign_id: c,
                ..pb::EventFilter::default()
            }),
            after_sequence: 0,
            limit: 10_000,
        })
        .await
        .expect("events")
        .into_inner()
        .events;
    assert!(events.iter().any(|e| matches!(
        &e.body,
        Some(pb::event::Body::Player(p))
            if p.kind == pb::PlayerEventKind::EnterUnit as i32
                && p.unit_id == Some(unit_entity)
    )));

    // The store has no entity for it: the platform's calls find none.
    let mut commands = CommandsServiceClient::new(ch.clone());
    let e = commands
        .remove_entity(pb::RemoveEntityRequest {
            write: write(&l, "rm", c),
            entity_id: group_entity,
        })
        .await
        .expect_err("not managed");
    assert_eq!(e.code(), Code::NotFound);
    let e = commands
        .set_order(pb::SetOrderRequest {
            write: write(&l, "order", c),
            group_id: group_entity,
            order: None,
        })
        .await
        .expect_err("not managed");
    assert_ne!(e.code(), Code::Ok);
    // DCS still holds the unit: the reconciler never destroys it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(fake(&hub, |s| s.binding_of(object)).is_some());

    // An effect may name it.
    let r = commands
        .do_effect(pb::DoEffectRequest {
            write: write(&l, "damage", c),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::Damage(pb::Damage {
                    unit_id: unit_entity,
                    life_percent: 50.0,
                })),
            }),
        })
        .await;
    assert!(r.is_ok(), "{r:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_carry_types_late_kills_dates_and_launch_players() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    until(|| async { fake(&hub, |s| s.attached()) }).await;
    let shooter = pilot(&hub, &ch, 77).await;
    let key = Keyring::generate().key_of("unused");
    assert_eq!(key.len(), 32);
    let key = players_now(&ch).await[0].key.clone();
    let victim = fake(&hub, |s| s.birth_client_unit_in("m2", 88, "Su-27"));
    let victim2 = fake(&hub, |s| s.birth_client_unit_in("m3", 89, "Su-25"));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The shot, a HIT by weapon, a HIT by gun, a BDA for the first victim.
    fake(&hub, |s| {
        s.emit_event(
            1,
            vec![(1, Lv::Number(10.0)), (2, n(shooter)), (4, n(5000))],
        );
        s.emit_event(
            2,
            vec![
                (2, n(shooter)),
                (3, n(victim)),
                (4, n(5000)),
                (19, t("F-16C_50")),
            ],
        );
        s.emit_event(2, vec![(2, n(shooter)), (3, n(victim))]);
        s.emit_event(
            37,
            vec![(1, Lv::Number(12.5)), (2, n(shooter)), (3, n(victim))],
        );
    });
    until(|| async { world_events(&ch, c, 2).await.len() == 2 }).await;
    let hits = world_events(&ch, c, 2).await;
    // The event's own type name wins; the mirror's fills the gap.
    assert_eq!(hits[0].initiator_type.as_deref(), Some("F-16C_50"));
    assert_eq!(hits[0].target_type.as_deref(), Some("Su-27"));
    assert_eq!(hits[1].initiator_type.as_deref(), Some("F-16C_50"));
    assert_eq!(hits[0].launch_rule, pb::LaunchRule::Weapon as i32);
    assert_eq!(hits[0].launch_players, vec![key.clone()]);
    assert_eq!(hits[1].launch_rule, pb::LaunchRule::Initiator as i32);
    assert_eq!(hits[1].launch_players, vec![key.clone()]);
    assert_eq!(hits[1].initiator_players, vec![key.clone()]);

    // Both rows leave, then the KILL and SCORE come late.
    fake(&hub, |s| {
        s.kill(victim);
        s.kill(shooter);
    });
    until(|| async { players_now(&ch).await[0].unit_id == 0 }).await;
    fake(&hub, |s| {
        s.emit_event(
            28,
            vec![
                (1, Lv::Number(99.0)),
                (2, n(shooter)),
                (3, n(victim)),
                (4, n(5000)),
            ],
        );
        s.emit_event(29, vec![(1, Lv::Number(99.0)), (2, n(shooter))]);
        // A victim that sent no BDA, killed while the shooter is gone.
        s.kill(victim2);
    });
    until(|| async { !world_events(&ch, c, 28).await.is_empty() }).await;
    until(|| async { !world_events(&ch, c, 29).await.is_empty() }).await;
    let kill = world_events(&ch, c, 28).await.remove(0);
    assert_eq!(kill.initiator_players, vec![key.clone()]);
    assert!(
        kill.initiator_id.is_some(),
        "the entity of the row that left"
    );
    assert_eq!(kill.initiator_type.as_deref(), Some("F-16C_50"));
    assert_eq!(kill.target_type.as_deref(), Some("Su-27"));
    // Dated from the BDA, not from the late event's own time (99 s).
    assert_eq!(kill.dated_time, Some(12.5));
    assert_eq!(kill.launch_rule, pb::LaunchRule::Weapon as i32);
    assert_eq!(kill.launch_players, vec![key.clone()]);
    let extra = kill.extra.expect("extra");
    assert!(extra.fields.contains_key("time"), "DCS's own time stays");
    let score = world_events(&ch, c, 29).await.remove(0);
    assert_eq!(score.initiator_players, vec![key.clone()]);

    // A death with no BDA is dated from the time its row left.
    fake(&hub, |s| {
        s.emit_event(8, vec![(1, Lv::Number(150.0)), (2, n(victim2))]);
    });
    // The kill above may already have sent a death of its own: wait for this one.
    let late = |w: &pb::WorldEvent| {
        w.extra
            .as_ref()
            .and_then(|x| x.fields.get("time"))
            .is_some_and(|v| {
                matches!(&v.kind, Some(prost_types::value::Kind::NumberValue(t)) if *t == 150.0)
            })
    };
    until(|| async { world_events(&ch, c, 8).await.iter().any(late) }).await;
    let dead = world_events(&ch, c, 8)
        .await
        .into_iter()
        .find(late)
        .expect("the late death");
    assert_eq!(dead.initiator_type.as_deref(), Some("Su-25"));
    let dated = dead.dated_time.expect("dated from the removal");
    assert!(dated > 0.0 && dated < 150.0, "{dated}");
}

async fn campaign_keyed(ch: &Channel, l: &Lease, key: &str) -> u64 {
    CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: key.into(),
            theaters: vec!["Caucasus".into()],
            activate: true,
            idempotency: Some(Idempotency { key: key.into() }),
            ..CreateCampaignRequest::default()
        })
        .await
        .expect("create")
        .into_inner()
        .campaign
        .expect("campaign")
        .id
}

/// The key the API gives the one connected player, after it connects.
async fn key_on_join(hub: &Hub, ch: &Channel, conn: u32) -> String {
    until(|| async { fake(hub, |s| s.attached()) }).await;
    fake(hub, |s| {
        s.connect_client(conn, UCID, "Ann", 2);
    });
    until(|| async { players_now(ch).await.len() == 1 }).await;
    let key = players_now(ch).await[0].key.clone();
    fake(hub, |s| s.disconnect_client(conn));
    until(|| async { players_now(ch).await.is_empty() }).await;
    key
}

/// Every `events.sqlite` of a campaign under `dir`.
fn campaign_event_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_owned()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).expect("dir").flatten() {
            let p = e.path();
            if p.is_dir() && p.file_name().is_some_and(|n| n != "server") {
                todo.push(p);
            } else if p.file_name().is_some_and(|n| n == "events.sqlite") {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_are_stable_across_a_restart_with_persistence() {
    let dir = std::env::temp_dir().join(format!("nanoprobe-adoption-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let with_dir = || Config {
        store: Some(StoreFiles::new(dir.clone())),
        ..cfg()
    };
    // The key the API gives in campaign a, in campaign b, and in b again
    // after a restart.
    let (hub, ch) = in_process(with_dir(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    campaign_keyed(&ch, &l, "a").await;
    let in_a = key_on_join(&hub, &ch, 2).await;
    campaign_keyed(&ch, &l, "b").await;
    let in_b = key_on_join(&hub, &ch, 3).await;
    until(|| async { hub.lock().roster.keyring().unsaved().is_empty() }).await;
    // A clean stop releases the store dir for the next sidecar.
    hub.lock().close_all().expect("closes");
    hub.close();
    let (hub, ch) = in_process(with_dir(), fake_config())
        .await
        .expect("restart");
    let after = key_on_join(&hub, &ch, 4).await;
    hub.lock().close_all().expect("closes");
    hub.close();
    assert!(!in_a.is_empty());
    assert_eq!(in_a, in_b, "the same key in two campaigns");
    assert_eq!(in_a, after, "the same key after a restart");
    let server = ServerStore::open(&dir, 0).expect("server store");
    assert_eq!(
        server.players().expect("players"),
        vec![(in_a.clone(), UCID.to_owned())],
        "the server store is the one place a ucid is kept"
    );
    assert!(!dir.join("players.json").exists());
    drop(server);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}

/// One agent record that makes two events (a returning player under a new
/// name: a connect and a rename) is logged with a key for each, and the
/// sidecar goes on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_record_with_two_events_is_logged_with_two_sources() {
    let dir = std::env::temp_dir().join(format!("nanoprobe-two-events-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (hub, ch) = in_process(
        Config {
            store: Some(StoreFiles::new(dir.clone())),
            ..cfg()
        },
        fake_config(),
    )
    .await
    .expect("server");
    let l = lease(&ch).await;
    campaign_keyed(&ch, &l, "a").await;
    key_on_join(&hub, &ch, 2).await;
    fake(&hub, |s| {
        s.connect_client(3, UCID, "Bob", 2);
    });
    until(|| async {
        players_now(&ch)
            .await
            .first()
            .is_some_and(|p| p.name == "Bob")
    })
    .await;
    // A later step still runs: the driver did not stop on the write.
    fake(&hub, |s| s.disconnect_client(3));
    until(|| async { players_now(&ch).await.is_empty() }).await;
    hub.lock().close_all().expect("closes");
    hub.close();
    let files = campaign_event_files(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    let c = rusqlite::Connection::open(&files[0]).expect("events");
    let mut st = c
        .prepare("SELECT source FROM event WHERE source IS NOT NULL")
        .expect("query");
    let sources: Vec<String> = st
        .query_map([], |r| r.get(0))
        .expect("rows")
        .map(|r| r.expect("row"))
        .collect();
    let numbered: Vec<&String> = sources
        .iter()
        .filter(|s| s.split(':').count() == 3)
        .collect();
    assert!(
        !numbered.is_empty(),
        "no record made two events: {sources:?}"
    );
    for n in numbered {
        let first = n.rsplit_once(':').expect("numbered").0;
        assert!(sources.iter().any(|s| s == first), "{n} without {first}");
    }
    drop(st);
    drop(c);
    std::fs::remove_dir_all(&dir).expect("cleanup");
}
