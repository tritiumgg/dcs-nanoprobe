//! Labels and markup groups of marks and drawings through the API on the fake sim
//! (spec/world.md, "Labels and markup groups"): upsert by label, markup groups and their
//! replacement in one call, the marks and drawings in `ListWorld` and
//! `WatchWorld`, and the removal of a player's mark.

use std::time::Duration;

use nanoprobe_api::config::{Config, StoreFiles};
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::players_service_client::PlayersServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, EntityKind, Idempotency, Lease, Write,
    entity, world_change,
};
use nanoprobe_sim_fake::FakeSim;
use prost_types::{ListValue, Value, value::Kind};
use tokio_stream::StreamExt;
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

async fn setup() -> (std::sync::Arc<Hub>, Channel, Lease, u64) {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
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
            name: "layers".into(),
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
    (hub, ch, l, c)
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

fn all() -> Option<pb::Audience> {
    Some(pb::Audience {
        to: Some(pb::audience::To::All(true)),
    })
}

fn number(n: f64) -> Value {
    Value {
        kind: Some(Kind::NumberValue(n)),
    }
}

/// A freeform (shape 7) through three points at `x = n`, with the style tail.
fn args(n: f64) -> Option<ListValue> {
    let rgba = || Value {
        kind: Some(Kind::ListValue(ListValue {
            values: vec![number(1.0), number(0.0), number(0.0), number(1.0)],
        })),
    };
    Some(ListValue {
        values: vec![
            point(n),
            point(n + 100.0),
            point(n + 200.0),
            rgba(),
            rgba(),
            number(1.0),
            Value {
                kind: Some(Kind::BoolValue(true)),
            },
            Value {
                kind: Some(Kind::StringValue(String::new())),
            },
        ],
    })
}

/// A drawing put through the deprecated `shape` and `args`.
#[allow(deprecated)]
fn drawing(
    l: &Lease,
    k: &str,
    c: u64,
    label: &str,
    markup_group: &str,
    n: f64,
) -> pb::PutDrawingRequest {
    pb::PutDrawingRequest {
        write: write(l, k, c),
        audience: all(),
        shape: 7,
        args: args(n),
        label: label.into(),
        markup_group: markup_group.into(),
        ..Default::default()
    }
}

fn item(label: &str, n: f64) -> pb::MarkupGroupItem {
    pb::MarkupGroupItem {
        label: label.into(),
        audience: all(),
        expire_after_s: None,
        kind: Some(pb::markup_group_item::Kind::Drawing(
            pb::MarkupGroupDrawing {
                typed_shape: Some(freeform(n)),
                ..Default::default()
            },
        )),
    }
}

/// The typed form of [`args`]: a freeform through three points at north `n`.
fn freeform(n: f64) -> pb::DrawingShape {
    let red = pb::Color {
        red: 1.0,
        green: 0.0,
        blue: 0.0,
        alpha: 1.0,
    };
    pb::DrawingShape {
        theater: "Caucasus".into(),
        color: Some(red),
        fill: Some(red),
        line_type: pb::LineType::Solid as i32,
        read_only: true,
        kind: Some(pb::drawing_shape::Kind::Freeform(
            pb::drawing_shape::Freeform {
                points: [n, n + 100.0, n + 200.0]
                    .into_iter()
                    .map(|north| pb::MapPoint { north, east: 0.0 })
                    .collect(),
            },
        )),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_label_upserts_and_a_markup_group_is_replaced_hidden_listed_and_removed() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let mut world = WorldServiceClient::new(ch.clone());

    // The same label twice is one drawing.
    let a = players
        .put_drawing(drawing(&l, "a1", c, "front", "lines", 1.0))
        .await
        .expect("put")
        .into_inner();
    let b = players
        .put_drawing(drawing(&l, "a2", c, "front", "lines", 3.0))
        .await
        .expect("upsert")
        .into_inner();
    assert_eq!(a.drawing_id, b.drawing_id);
    // A mark cannot take it.
    let taken = players
        .put_mark(pb::PutMarkRequest {
            write: write(&l, "m1", c),
            audience: all(),
            position: Some(pb::Position {
                theater: "Caucasus".into(),
                north: 1.0,
                east: 2.0,
                alt: 0.0,
            }),
            text: "x".into(),
            label: "front".into(),
            ..Default::default()
        })
        .await;
    assert_eq!(taken.expect_err("taken").code(), tonic::Code::AlreadyExists);
    until_fake(&hub, |s| {
        s.drawing(a.drawing_id).is_some_and(|d| d.rev == 2)
    })
    .await;

    // The markup group was made by the put; ListMarkupGroups shows it.
    let markup_groups = players
        .list_markup_groups(pb::ListMarkupGroupsRequest::default())
        .await
        .expect("list")
        .into_inner()
        .markup_groups;
    assert_eq!(markup_groups.len(), 1);
    assert_eq!(
        (markup_groups[0].name.as_str(), markup_groups[0].visible),
        ("lines", true)
    );

    // Replace: front changes, two segments come.
    let r = players
        .replace_markup_group(pb::ReplaceMarkupGroupRequest {
            write: write(&l, "r1", c),
            name: "lines".into(),
            items: vec![item("front", 5.0), item("seg-1", 1.0), item("seg-2", 2.0)],
        })
        .await
        .expect("replace")
        .into_inner();
    assert_eq!(r.ids.len(), 3);
    assert_eq!(r.ids[0], a.drawing_id);
    until_fake(&hub, |s| r.ids.iter().all(|id| s.drawing(*id).is_some())).await;
    // Fewer segments: the dropped one leaves DCS.
    players
        .replace_markup_group(pb::ReplaceMarkupGroupRequest {
            write: write(&l, "r2", c),
            name: "lines".into(),
            items: vec![item("front", 5.0), item("seg-1", 1.0)],
        })
        .await
        .expect("replace again");
    until_fake(&hub, |s| s.drawing(r.ids[2]).is_none()).await;
    // Every item needs a label.
    let bad = players
        .replace_markup_group(pb::ReplaceMarkupGroupRequest {
            write: write(&l, "r3", c),
            name: "lines".into(),
            items: vec![item("", 1.0)],
        })
        .await;
    assert_eq!(
        bad.expect_err("no label").code(),
        tonic::Code::InvalidArgument
    );

    // ListWorld lists the drawings, filtered by markup group.
    let listed = world
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                kinds: vec![EntityKind::Drawing as i32],
                markup_groups: vec!["lines".into()],
                ..Default::default()
            }),
        })
        .await
        .expect("list")
        .into_inner()
        .next()
        .await
        .expect("page")
        .expect("ok");
    let labels: Vec<String> = listed
        .entities
        .iter()
        .filter_map(|e| match &e.kind {
            Some(entity::Kind::Drawing(d)) => Some(d.label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(labels.len(), 2);
    assert!(labels.contains(&"front".to_owned()) && labels.contains(&"seg-1".to_owned()));

    // Hidden: DCS holds none of them; shown: all come back.
    players
        .put_markup_group(pb::PutMarkupGroupRequest {
            write: write(&l, "h1", c),
            markup_group: Some(pb::MarkupGroup {
                name: "lines".into(),
                visible: false,
                expire_after_s: None,
                cap: None,
            }),
        })
        .await
        .expect("hide");
    until_fake(&hub, |s| s.drawings().is_empty()).await;
    players
        .put_markup_group(pb::PutMarkupGroupRequest {
            write: write(&l, "h2", c),
            markup_group: Some(pb::MarkupGroup {
                name: "lines".into(),
                visible: true,
                expire_after_s: None,
                cap: None,
            }),
        })
        .await
        .expect("show");
    until_fake(&hub, |s| s.drawings().len() == 2).await;

    // Removing the markup group removes its items.
    players
        .remove_markup_group(pb::RemoveMarkupGroupRequest {
            write: write(&l, "x1", c),
            name: "lines".into(),
        })
        .await
        .expect("remove");
    until_fake(&hub, |s| s.drawings().is_empty()).await;
    let missing = players
        .remove_markup_group(pb::RemoveMarkupGroupRequest {
            write: write(&l, "x2", c),
            name: "lines".into(),
        })
        .await;
    assert_eq!(missing.expect_err("gone").code(), tonic::Code::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watch_sees_an_eviction_as_a_removal() {
    let (_hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let mut world = WorldServiceClient::new(ch.clone());
    players
        .put_markup_group(pb::PutMarkupGroupRequest {
            write: write(&l, "p", c),
            markup_group: Some(pb::MarkupGroup {
                name: "pings".into(),
                visible: true,
                expire_after_s: None,
                cap: Some(1),
            }),
        })
        .await
        .expect("markup_group");
    let first = players
        .put_drawing(drawing(&l, "d1", c, "one", "pings", 1.0))
        .await
        .expect("one")
        .into_inner();
    let mut watch = world
        .watch_world(pb::WatchWorldRequest {
            filter: Some(pb::WorldFilter {
                kinds: vec![EntityKind::Drawing as i32],
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("watch")
        .into_inner();
    players
        .put_drawing(drawing(&l, "d2", c, "two", "pings", 1.0))
        .await
        .expect("two");
    let removal = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let msg = watch.next().await.expect("open").expect("ok");
            for ch in msg.changes {
                if let Some(world_change::Change::Removal(r)) = ch.change {
                    return r;
                }
            }
        }
    })
    .await
    .expect("a removal");
    assert_eq!(removal.id, first.drawing_id);
    assert_eq!(removal.cause, pb::RemovalCause::Evicted as i32);
}

/// The `event_field` code of a mark event's mark ID.
const MARK_ID_FIELD: u16 = 9;

/// A DCS mark event (25 to 27) for `mark_id`, and its sequence number once the
/// sidecar has logged it.
async fn mark_event(hub: &Hub, campaign: u64, dcs_id: u32, mark_id: u64) -> u64 {
    let before = hub.lock().campaigns[&campaign].events.last();
    fake(hub, |s| {
        s.emit_event(
            dcs_id,
            vec![(
                MARK_ID_FIELD,
                nanoprobe_transport::codec::Lv::Number(mark_id as f64),
            )],
        );
    });
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            {
                let g = hub.lock();
                let log = &g.campaigns[&campaign].events;
                if log.last() > before {
                    return log.last();
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the sidecar logs the event")
}

fn remove_mark(
    l: &Lease,
    k: &str,
    c: u64,
    mark_id: u64,
    event_sequence: u64,
) -> pb::RemovePlayerMarkRequest {
    pb::RemovePlayerMarkRequest {
        write: write(l, k, c),
        mark_id,
        event_sequence,
    }
}

fn clears(hub: &Hub) -> Vec<u64> {
    fake(hub, |s| s.ledger().mark_clears.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_players_mark_is_removed_by_its_event_while_the_event_is_held() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    // A drawing applied: the mission runs and effects can be sent.
    let d = players
        .put_drawing(drawing(&l, "d", c, "", "", 1.0))
        .await
        .expect("drawing")
        .into_inner();
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let held = fake(&hub, |s| s.drawing(d.drawing_id).unwrap().dcs_id);
    let seq = mark_event(&hub, c, 25, 12).await;
    let r = players
        .remove_player_mark(remove_mark(&l, "pm1", c, 12, seq))
        .await
        .expect("removed")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    assert_eq!(clears(&hub), vec![12]);
    // An event that is not held, a sequence number never given, an event of
    // another mark ID, and a mark ID the platform holds are refused.
    let missing = players
        .remove_player_mark(remove_mark(&l, "pm2", c, 12, seq + 1000))
        .await;
    assert_eq!(
        missing.expect_err("never given").code(),
        tonic::Code::FailedPrecondition
    );
    let other = players
        .remove_player_mark(remove_mark(&l, "pm3", c, 13, seq))
        .await;
    assert_eq!(
        other.expect_err("other ID").code(),
        tonic::Code::InvalidArgument
    );
    let platform = mark_event(&hub, c, 26, held).await;
    let refused = players
        .remove_player_mark(remove_mark(&l, "pm4", c, held, platform))
        .await;
    assert_eq!(
        refused.expect_err("held").code(),
        tonic::Code::InvalidArgument
    );
    assert_eq!(clears(&hub), vec![12]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mark_event_from_before_a_dcs_restart_is_refused_at_the_same_generation() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let d = players
        .put_drawing(drawing(&l, "d", c, "", "", 1.0))
        .await
        .expect("drawing")
        .into_inner();
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let boot = fake(&hub, |s| s.agent_boot());
    // DCS restarts and the campaign's mission loads again.
    {
        let mut g = hub.lock();
        fake_in(&mut g).restart_dcs();
        let sidecar_boot = g.sidecar_boot;
        g.backend.attach(sidecar_boot);
    }
    let running = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let running = hub.lock().campaigns[&c]
                .store
                .loaded()
                .map(|l| (l.agent_boot, l.mission_gen));
            if let Some(r) = running.filter(|r| r.0 > boot && r.1 > 0) {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the mission loads in the new DCS process");
    // A mark event the old process made, stamped with the generation now running.
    let seq = {
        let mut g = hub.lock();
        let stamp = pb::Stamp {
            mission_generation: running.1,
            ..pb::Stamp::default()
        };
        let body = pb::event::Body::World(pb::WorldEvent {
            dcs_id: 25,
            mark_id: Some(12),
            ..pb::WorldEvent::default()
        });
        g.campaigns.get_mut(&c).expect("campaign").events.push_from(
            c,
            stamp,
            body,
            Some(format!("{boot}:9001")),
        )
    };
    let before = clears(&hub);
    let stale = players
        .remove_player_mark(remove_mark(&l, "pm1", c, 12, seq))
        .await;
    assert_eq!(
        stale.expect_err("stale").code(),
        tonic::Code::FailedPrecondition
    );
    assert_eq!(clears(&hub), before, "no MARK_CLEAR is sent");
    // The same event stamped by the running process passes.
    let now = {
        let mut g = hub.lock();
        let stamp = pb::Stamp {
            mission_generation: running.1,
            ..pb::Stamp::default()
        };
        let body = pb::event::Body::World(pb::WorldEvent {
            dcs_id: 25,
            mark_id: Some(12),
            ..pb::WorldEvent::default()
        });
        g.campaigns.get_mut(&c).expect("campaign").events.push_from(
            c,
            stamp,
            body,
            Some(format!("{}:9002", running.0)),
        )
    };
    players
        .remove_player_mark(remove_mark(&l, "pm2", c, 12, now))
        .await
        .expect("removed");
    assert_eq!(clears(&hub), vec![12]);
    // The running boot but another mission generation is refused.
    let other_generation = {
        let mut g = hub.lock();
        let stamp = pb::Stamp {
            mission_generation: running.1 + 1,
            ..pb::Stamp::default()
        };
        let body = pb::event::Body::World(pb::WorldEvent {
            dcs_id: 25,
            mark_id: Some(12),
            ..pb::WorldEvent::default()
        });
        g.campaigns.get_mut(&c).expect("campaign").events.push_from(
            c,
            stamp,
            body,
            Some(format!("{}:9003", running.0)),
        )
    };
    let refused = players
        .remove_player_mark(remove_mark(&l, "pm3", c, 12, other_generation))
        .await;
    assert_eq!(
        refused.expect_err("other generation").code(),
        tonic::Code::FailedPrecondition
    );
    assert_eq!(clears(&hub), vec![12], "no MARK_CLEAR is sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mark_event_the_log_purged_is_refused() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let d = players
        .put_drawing(drawing(&l, "d", c, "", "", 1.0))
        .await
        .expect("drawing")
        .into_inner();
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let seq = mark_event(&hub, c, 25, 12).await;
    {
        // A later event, then a size limit that keeps only the newest.
        let mut g = hub.lock();
        g.publish(pb::event::Body::World(pb::WorldEvent::default()));
        let log = &mut g.campaigns.get_mut(&c).expect("campaign").events;
        log.retain(Duration::ZERO, 0, 100, 1).expect("retain");
        assert!(log.get(seq).is_none(), "the event is purged");
    }
    let gone = players
        .remove_player_mark(remove_mark(&l, "pm1", c, 12, seq))
        .await;
    assert_eq!(
        gone.expect_err("purged").code(),
        tonic::Code::FailedPrecondition
    );
    assert!(clears(&hub).is_empty());
}

fn fake_in(g: &mut nanoprobe_api::hub::Inner) -> &mut FakeSim {
    g.backend
        .as_any()
        .and_then(|a| a.downcast_mut::<FakeSim>())
        .expect("the fake sim")
}

/// The labels of the drawings `ListWorld` lists under a filter, sorted.
async fn listed_labels(
    world: &mut WorldServiceClient<Channel>,
    markup_groups: Vec<String>,
    coalitions: Vec<i32>,
) -> Vec<String> {
    let page = world
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                kinds: vec![EntityKind::Drawing as i32],
                markup_groups,
                coalitions,
                ..Default::default()
            }),
        })
        .await
        .expect("list")
        .into_inner()
        .next()
        .await
        .expect("page")
        .expect("ok");
    let mut labels: Vec<String> = page
        .entities
        .iter()
        .filter_map(|e| match &e.kind {
            Some(entity::Kind::Drawing(d)) => Some(d.label.clone()),
            _ => None,
        })
        .collect();
    labels.sort();
    labels
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_markup_group_and_coalition_filters_of_list_world_select_drawings() {
    let (_hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let mut world = WorldServiceClient::new(ch.clone());
    let to = |coalition: pb::Coalition| {
        Some(pb::Audience {
            to: Some(pb::audience::To::Coalition(coalition as i32)),
        })
    };
    for (k, label, markup_group, audience) in [
        ("k1", "in-lines", "lines", all()),
        ("k2", "in-other", "other", all()),
        ("k3", "bare", "", all()),
        ("k4", "red", "", to(pb::Coalition::Red)),
        ("k5", "blue", "", to(pb::Coalition::Blue)),
    ] {
        let mut d = drawing(&l, k, c, label, markup_group, 1.0);
        d.audience = audience;
        players.put_drawing(d).await.expect("put");
    }
    let strings = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    // A markup group filter leaves out the other markup group and the unlayered ones.
    assert_eq!(
        listed_labels(&mut world, strings(&["lines"]), vec![]).await,
        strings(&["in-lines"])
    );
    // "" names the items with no markup group, and no item of a markup group.
    assert_eq!(
        listed_labels(&mut world, strings(&[""]), vec![]).await,
        strings(&["bare", "blue", "red"])
    );
    // No filter lists all five.
    assert_eq!(listed_labels(&mut world, vec![], vec![]).await.len(), 5);
    // A coalition filter drops the other coalition's drawings and keeps
    // those for everyone.
    assert_eq!(
        listed_labels(&mut world, vec![], vec![pb::Coalition::Red as i32]).await,
        strings(&["bare", "in-lines", "in-other", "red"])
    );
}

/// The labels of the drawings `ListWorld` lists within `radius` of a point.
async fn labels_near(
    world: &mut WorldServiceClient<Channel>,
    north: f64,
    east: f64,
    radius: f64,
) -> Vec<String> {
    labels_on(world, "Caucasus", north, east, radius).await
}

/// As [`labels_near`], with the filter's center on `theater`.
async fn labels_on(
    world: &mut WorldServiceClient<Channel>,
    theater: &str,
    north: f64,
    east: f64,
    radius: f64,
) -> Vec<String> {
    let page = world
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                kinds: vec![EntityKind::Drawing as i32],
                center: Some(pb::Position {
                    theater: theater.into(),
                    north,
                    east,
                    alt: 0.0,
                }),
                radius,
                ..Default::default()
            }),
        })
        .await
        .expect("list")
        .into_inner()
        .next()
        .await
        .expect("page")
        .expect("ok");
    let mut v: Vec<String> = page
        .entities
        .iter()
        .filter_map(|e| match &e.kind {
            Some(entity::Kind::Drawing(d)) => Some(d.label.clone()),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_drawings_are_checked_read_back_and_found_by_area() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let mut world = WorldServiceClient::new(ch.clone());
    let put = |k: &str, label: &str, typed: Option<pb::DrawingShape>| pb::PutDrawingRequest {
        write: write(&l, k, c),
        audience: all(),
        label: label.into(),
        typed_shape: typed,
        ..Default::default()
    };
    let circle = |north: f64, radius: f64| pb::DrawingShape {
        theater: "Caucasus".into(),
        kind: Some(pb::drawing_shape::Kind::Circle(pb::drawing_shape::Circle {
            center: Some(pb::MapPoint { north, east: 0.0 }),
            radius,
        })),
        ..Default::default()
    };
    let oval = pb::DrawingShape {
        theater: "Caucasus".into(),
        kind: Some(pb::drawing_shape::Kind::Oval(pb::drawing_shape::Oval {
            center: Some(pb::MapPoint {
                north: 10_000.0,
                east: 0.0,
            }),
            half_length: 1_500.0,
            half_width: 750.0,
            heading: 0.5,
        })),
        ..Default::default()
    };
    let ci = players
        .put_drawing(put("c", "circle", Some(circle(0.0, 1_000.0))))
        .await
        .expect("circle")
        .into_inner();
    players
        .put_drawing(put("o", "oval", Some(oval)))
        .await
        .expect("oval");
    // A drawing put through the deprecated pair reads back typed, with no
    // theater; it passes no area filter.
    players
        .put_drawing(drawing(&l, "f", c, "legacy", "", 20_000.0))
        .await
        .expect("legacy");
    until_fake(&hub, |s| s.drawings().len() == 3).await;
    // The sidecar sends the circle through circleToAll and the oval as 36 points.
    let held = fake(&hub, |s| s.drawing(ci.drawing_id).cloned()).unwrap();
    assert_eq!(held.shape, 0x100);

    assert_eq!(
        labels_near(&mut world, 0.0, 1_500.0, 600.0).await,
        ["circle"]
    );
    assert!(
        labels_near(&mut world, 0.0, 1_500.0, 400.0)
            .await
            .is_empty()
    );
    // A filter on another theater, at the circle's center, finds nothing.
    assert!(
        labels_on(&mut world, "Syria", 0.0, 0.0, 2_000.0)
            .await
            .is_empty()
    );
    // Inside the oval, far from its outline.
    assert_eq!(labels_near(&mut world, 10_000.0, 0.0, 1.0).await, ["oval"]);
    assert!(
        labels_near(&mut world, 20_000.0, 0.0, 500.0)
            .await
            .is_empty()
    );

    let listed = listed_drawings(&mut world).await;
    let by = |label: &str| listed.iter().find(|d| d.label == label).cloned().unwrap();
    #[allow(deprecated)]
    {
        assert_eq!((by("circle").shape, by("oval").shape), (0, 7));
        assert_eq!(by("oval").args.unwrap().values.len(), 36 + 5);
        assert!(by("circle").args.unwrap().values.is_empty());
    }
    let legacy = by("legacy").typed_shape.unwrap();
    assert!(legacy.theater.is_empty());
    assert!(matches!(
        legacy.kind,
        Some(pb::drawing_shape::Kind::Freeform(ref f)) if f.points.len() == 3
    ));
    assert_eq!(
        by("circle").typed_shape.unwrap().kind,
        circle(0.0, 1_000.0).kind
    );

    // Refusals: both forms, neither, a theater the campaign does not use, a
    // size below 1 m, and the deprecated pair in a markup group.
    let code = |e: tonic::Status| e.code();
    let mut both = put("b", "b", Some(circle(0.0, 10.0)));
    #[allow(deprecated)]
    {
        both.shape = 7;
    }
    let mut syria = circle(0.0, 10.0);
    syria.theater = "Syria".into();
    for req in [
        both,
        put("n", "n", None),
        put("s", "s", Some(syria)),
        put("r", "r", Some(circle(0.0, 0.5))),
    ] {
        assert_eq!(
            code(players.put_drawing(req).await.expect_err("refused")),
            tonic::Code::InvalidArgument
        );
    }
    #[allow(deprecated)]
    let legacy_item = pb::MarkupGroupItem {
        label: "x".into(),
        audience: all(),
        expire_after_s: None,
        kind: Some(pb::markup_group_item::Kind::Drawing(
            pb::MarkupGroupDrawing {
                shape: 7,
                args: args(1.0),
                typed_shape: None,
            },
        )),
    };
    let e = players
        .replace_markup_group(pb::ReplaceMarkupGroupRequest {
            write: write(&l, "g", c),
            name: "g".into(),
            items: vec![legacy_item],
        })
        .await
        .expect_err("the deprecated pair in a markup group");
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
}

async fn listed_drawings(world: &mut WorldServiceClient<Channel>) -> Vec<pb::Drawing> {
    let page = world
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                kinds: vec![EntityKind::Drawing as i32],
                ..Default::default()
            }),
        })
        .await
        .expect("list")
        .into_inner()
        .next()
        .await
        .expect("page")
        .expect("ok");
    page.entities
        .into_iter()
        .filter_map(|e| match e.kind {
            Some(entity::Kind::Drawing(d)) => Some(d),
            _ => None,
        })
        .collect()
}

/// The `dcs_id` of every mark and drawing `ListWorld` lists, by ID.
async fn listed_dcs_ids(ch: &Channel) -> Vec<(u64, u64)> {
    let mut world = WorldServiceClient::new(ch.clone());
    let page = world
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                kinds: vec![EntityKind::Drawing as i32],
                ..Default::default()
            }),
        })
        .await
        .expect("list")
        .into_inner()
        .next()
        .await
        .expect("page")
        .expect("ok");
    page.entities
        .iter()
        .filter_map(|e| match &e.kind {
            Some(entity::Kind::Drawing(d)) => Some((d.id, d.dcs_id)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hidden_item_lists_dcs_id_0_and_a_show_gives_a_new_one() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let d = players
        .put_drawing(drawing(&l, "d", c, "", "lines", 1.0))
        .await
        .expect("drawing")
        .into_inner();
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let shown = listed_dcs_ids(&ch).await;
    assert_eq!(shown.len(), 1);
    let first = shown[0].1;
    assert_ne!(first, 0);
    assert_eq!(
        first,
        fake(&hub, |s| s.drawing(d.drawing_id).unwrap().dcs_id)
    );
    let visible = |visible| pb::PutMarkupGroupRequest {
        write: write(&l, if visible { "s" } else { "h" }, c),
        markup_group: Some(pb::MarkupGroup {
            name: "lines".into(),
            visible,
            ..Default::default()
        }),
    };
    players
        .put_markup_group(visible(false))
        .await
        .expect("hide");
    until_fake(&hub, |s| s.drawings().is_empty()).await;
    assert_eq!(listed_dcs_ids(&ch).await, vec![(d.drawing_id, 0)]);
    players.put_markup_group(visible(true)).await.expect("show");
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let again = listed_dcs_ids(&ch).await[0].1;
    assert_ne!(again, 0);
    assert_ne!(again, first, "every add takes a fresh ID");
}

/// The world event the log holds under `sequence`.
fn logged_world_event(hub: &Hub, campaign: u64, sequence: u64) -> pb::WorldEvent {
    let (e, _) = hub.lock().campaigns[&campaign]
        .events
        .get(sequence)
        .expect("held");
    match e.body {
        Some(pb::event::Body::World(w)) => w,
        other => panic!("not a world event: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_mark_is_true_for_ids_the_platform_issued_this_boot_and_false_for_a_players() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let d = players
        .put_drawing(drawing(&l, "d", c, "", "lines", 1.0))
        .await
        .expect("drawing")
        .into_inner();
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let first = listed_dcs_ids(&ch).await[0].1;
    let on = |name: &str, visible| pb::PutMarkupGroupRequest {
        write: write(&l, name, c),
        markup_group: Some(pb::MarkupGroup {
            name: "lines".into(),
            visible,
            ..Default::default()
        }),
    };
    players
        .put_markup_group(on("h", false))
        .await
        .expect("hide");
    until_fake(&hub, |s| s.drawings().is_empty()).await;
    players.put_markup_group(on("s", true)).await.expect("show");
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let now = listed_dcs_ids(&ch).await[0].1;
    assert_ne!(now, first);
    // The held ID, the ID the show retired, and an ID of a player.
    for (id, want) in [(now, true), (first, true), (12, false)] {
        let seq = mark_event(&hub, c, 25, id).await;
        let w = logged_world_event(&hub, c, seq);
        assert_eq!(w.mark_id, Some(id));
        assert_eq!(w.platform_mark, want, "mark {id}");
    }
    // An event with no mark ID is not a platform mark.
    let seq = mark_event(&hub, c, 24, now).await;
    assert!(!logged_world_event(&hub, c, seq).platform_mark);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_player_mark_refuses_an_id_the_platform_retired() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let d = players
        .put_drawing(drawing(&l, "d", c, "", "lines", 1.0))
        .await
        .expect("drawing")
        .into_inner();
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let first = listed_dcs_ids(&ch).await[0].1;
    for (k, visible) in [("h", false), ("s", true)] {
        players
            .put_markup_group(pb::PutMarkupGroupRequest {
                write: write(&l, k, c),
                markup_group: Some(pb::MarkupGroup {
                    name: "lines".into(),
                    visible,
                    ..Default::default()
                }),
            })
            .await
            .expect("put");
        if visible {
            until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
        } else {
            until_fake(&hub, |s| s.drawings().is_empty()).await;
        }
    }
    assert_ne!(listed_dcs_ids(&ch).await[0].1, first, "the ID is retired");
    let seq = mark_event(&hub, c, 25, first).await;
    let refused = players
        .remove_player_mark(remove_mark(&l, "pm", c, first, seq))
        .await;
    assert_eq!(
        refused.expect_err("retired").code(),
        tonic::Code::InvalidArgument
    );
    assert!(clears(&hub).is_empty());
}

/// The status the API reports.
async fn status(ch: &Channel) -> pb::ServerStatus {
    MissionServiceClient::new(ch.clone())
        .get_server_status(pb::GetServerStatusRequest::default())
        .await
        .expect("status")
        .into_inner()
        .status
        .expect("status")
}

/// A struct value, for a point.
fn point(x: f64) -> Value {
    let f = |v: f64| number(v);
    Value {
        kind: Some(Kind::StructValue(prost_types::Struct {
            fields: [("x", f(x)), ("y", f(0.0)), ("z", f(0.0))]
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
        })),
    }
}

/// A rectangle with a style tail: two points, color, fill, line type, read
/// only, message. A change of `color` alone is a style change in place.
#[allow(deprecated)]
fn styled(l: &Lease, k: &str, c: u64, label: &str, x: f64, color: f64) -> pb::PutDrawingRequest {
    let rgba = || Value {
        kind: Some(Kind::ListValue(ListValue {
            values: vec![number(color), number(0.0), number(0.0), number(1.0)],
        })),
    };
    pb::PutDrawingRequest {
        write: write(l, k, c),
        audience: all(),
        shape: 3,
        args: Some(ListValue {
            values: vec![
                point(x),
                point(x + 100.0),
                rgba(),
                rgba(),
                number(1.0),
                Value {
                    kind: Some(Kind::BoolValue(true)),
                },
                Value {
                    kind: Some(Kind::StringValue(String::new())),
                },
            ],
        }),
        label: label.into(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_reports_the_ids_left_and_warns_below_the_threshold() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let low = nanoprobe_api::services::mission::DCS_IDS_LOW;
    // The threshold is 1% of the pool.
    assert_eq!(low, (1u64 << 30) / 100);
    let full = status(&ch).await;
    assert_eq!(full.dcs_ids_left, 1 << 30);
    assert!(!full.dcs_ids_low);
    // The pool holds `low` + 2 ids: two adds leave exactly the threshold,
    // which is not yet a warning; the third leaves one under it.
    hub.lock()
        .campaigns
        .get_mut(&c)
        .expect("campaign")
        .store
        .set_slot_limit_for_test(low + 2);
    assert_eq!(status(&ch).await.dcs_ids_left, low + 2);
    for (k, label, left, warns) in [
        ("a", "a", low + 1, false),
        ("b", "b", low, false),
        ("c", "c", low - 1, true),
    ] {
        players
            .put_drawing(drawing(&l, k, c, label, "", 1.0))
            .await
            .expect("drawing");
        // The slot is taken when the reconciler adds the drawing.
        let taken = (low + 2 - left) as usize;
        until_fake(&hub, |s| s.drawings().len() == taken).await;
        let s = status(&ch).await;
        assert_eq!((s.dcs_ids_left, s.dcs_ids_low), (left, warns), "{label}");
    }
    // The warning is not part of health.
    assert_eq!(s_health(&ch).await, pb::Health::Healthy);
}

async fn s_health(ch: &Channel) -> pb::Health {
    status(ch).await.health()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exhausted_pool_refuses_new_adds_and_the_sidecar_goes_on() {
    let (hub, ch, l, c) = setup().await;
    let mut players = PlayersServiceClient::new(ch.clone());
    hub.lock()
        .campaigns
        .get_mut(&c)
        .expect("campaign")
        .store
        .set_slot_limit_for_test(2);
    for (k, label) in [("a", "a"), ("b", "b")] {
        players
            .put_drawing(styled(&l, k, c, label, 0.0, 1.0))
            .await
            .expect("drawing");
    }
    until_fake(&hub, |s| s.drawings().len() == 2).await;
    let s = status(&ch).await;
    assert_eq!((s.dcs_ids_left, s.dcs_ids_low), (0, true));
    let revision = hub.lock().campaigns[&c].store.revision();
    let shown = listed_dcs_ids(&ch).await;
    // A new item and a full set of a shown one are refused with nothing
    // written.
    for req in [
        styled(&l, "c", c, "c", 0.0, 1.0),
        styled(&l, "a2", c, "a", 50.0, 1.0),
    ] {
        let e = players.put_drawing(req).await.expect_err("full");
        assert_eq!(e.code(), tonic::Code::ResourceExhausted);
        assert_eq!(
            nanoprobe_proto::errors::reason(&e),
            pb::ErrorReason::DcsIdsFull
        );
        assert!(e.message().contains("pool"), "{}", e.message());
        assert_eq!(hub.lock().campaigns[&c].store.revision(), revision);
    }
    // What is shown stays, with the same IDs.
    assert_eq!(listed_dcs_ids(&ch).await, shown);
    // A style change in place needs no slot and applies.
    players
        .put_drawing(styled(&l, "a3", c, "a", 0.0, 0.5))
        .await
        .expect("a color change needs no slot");
    until_fake(&hub, |s| s.drawings().iter().any(|d| d.1.updates == 1)).await;
    assert_eq!(listed_dcs_ids(&ch).await, shown);
    // Other calls work: the status, a markup group, a removal.
    assert_eq!(s_health(&ch).await, pb::Health::Healthy);
    players
        .put_markup_group(pb::PutMarkupGroupRequest {
            write: write(&l, "g", c),
            markup_group: Some(pb::MarkupGroup {
                name: "g".into(),
                visible: true,
                ..Default::default()
            }),
        })
        .await
        .expect("a markup group takes no slot");
    let list = players
        .list_markup_groups(pb::ListMarkupGroupsRequest { campaign_id: c })
        .await
        .expect("list")
        .into_inner();
    assert_eq!(list.markup_groups.len(), 1);
}

/// A scratch folder, removed at the end.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn create(ch: &Channel, l: &Lease, name: &str) -> u64 {
    create_with(ch, l, name, true).await
}

async fn create_with(ch: &Channel, l: &Lease, name: &str, activate: bool) -> u64 {
    CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: name.into(),
            theaters: vec!["Caucasus".into()],
            activate,
            idempotency: Some(Idempotency { key: name.into() }),
            ..CreateCampaignRequest::default()
        })
        .await
        .expect("create")
        .into_inner()
        .campaign
        .expect("campaign")
        .id
}

/// The adds the shim refused so far.
fn refused(hub: &Hub) -> u32 {
    fake(hub, |s| s.ledger().refused_adds)
}

/// A server with the stores on disk, in a scratch folder.
async fn disk_server(name: &str) -> (Scratch, std::sync::Arc<Hub>, Channel, Lease) {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("nanoprobe-layers-{name}-{}", std::process::id())),
    );
    let _ = std::fs::remove_dir_all(&scratch.0);
    let (hub, ch) = in_process(
        Config {
            store: Some(StoreFiles::new(scratch.0.clone())),
            ..cfg()
        },
        fake_config(),
    )
    .await
    .expect("server");
    let l = SessionServiceClient::new(ch.clone())
        .acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect("lease")
        .into_inner()
        .lease
        .expect("lease");
    (scratch, hub, ch, l)
}

async fn draw_n(ch: &Channel, l: &Lease, c: u64, n: usize) {
    let mut players = PlayersServiceClient::new(ch.clone());
    for i in 0..n {
        players
            .put_drawing(drawing(
                l,
                &format!("{c}-{i}"),
                c,
                &format!("d{i}"),
                "",
                1.0,
            ))
            .await
            .expect("drawing");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ids_left_count_only_the_running_boot() {
    let (_scratch, hub, ch, _l) = disk_server("ids-left-boot").await;
    let full = 1u64 << 30;
    // No campaign is active. The agent attaches; the boot is its own.
    let boot = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let b = hub.lock().link.agent_boot;
            if b != 0 {
                return b;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the agent attaches");
    let put = |b: u64, n: u64| {
        hub.lock()
            .server
            .as_ref()
            .expect("server store")
            .put_dcs_mark(b, n)
            .expect("row");
    };
    // A row of another boot reports the full pool.
    put(boot + 1, 1_000);
    assert_eq!(status(&ch).await.dcs_ids_left, full);
    assert!(!status(&ch).await.dcs_ids_low);
    // A row of the running boot counts.
    put(boot, 1_000);
    assert_eq!(status(&ch).await.dcs_ids_left, full - 1_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_campaign_opened_before_its_activation_takes_the_row_mark() {
    let (_scratch, hub, ch, l) = disk_server("open-first").await;
    let mut campaigns = CampaignServiceClient::new(ch.clone());
    let a = create(&ch, &l, "a").await;
    // B is created suspended and its store stays open while A draws.
    let b = create_with(&ch, &l, "b", false).await;
    draw_n(&ch, &l, a, 3).await;
    until_fake(&hub, |s| s.drawings().len() == 3).await;
    campaigns
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: b,
            idempotency: Some(Idempotency { key: "to-b".into() }),
        })
        .await
        .expect("activate b");
    draw_n(&ch, &l, b, 2).await;
    until_fake(&hub, |s| s.drawings().len() == 2).await;
    assert_eq!(refused(&hub), 0, "B's first adds are not refused");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_campaign_created_active_after_another_drew_is_not_refused() {
    let (_scratch, hub, ch, l) = disk_server("create-active").await;
    let a = create(&ch, &l, "a").await;
    draw_n(&ch, &l, a, 3).await;
    until_fake(&hub, |s| s.drawings().len() == 3).await;
    let b = create(&ch, &l, "b").await;
    draw_n(&ch, &l, b, 2).await;
    until_fake(&hub, |s| s.drawings().len() == 2).await;
    assert_eq!(refused(&hub), 0, "B's first adds are not refused");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn campaigns_share_one_pool_through_a_switch_and_hold_their_items() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("nanoprobe-layers-switch-{}", std::process::id())),
    );
    let dir = scratch.0.clone();
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
    let l = SessionServiceClient::new(ch.clone())
        .acquire_lease(AcquireLeaseRequest::default())
        .await
        .expect("lease")
        .into_inner()
        .lease
        .expect("lease");
    let players = PlayersServiceClient::new(ch.clone());
    let mut campaigns = CampaignServiceClient::new(ch.clone());
    let draw = |players: PlayersServiceClient<Channel>, c: u64, n: usize| {
        let l = l.clone();
        async move {
            let mut players = players;
            for i in 0..n {
                players
                    .put_drawing(drawing(
                        &l,
                        &format!("{c}-{i}"),
                        c,
                        &format!("d{i}"),
                        "",
                        1.0,
                    ))
                    .await
                    .expect("drawing");
            }
        }
    };
    let ids_of = || {
        let mut g = hub.lock();
        fake_in(&mut g)
            .drawings()
            .iter()
            .map(|d| d.1.dcs_id)
            .collect::<Vec<_>>()
    };

    // A draws two, B (created active, so A is suspended) draws three.
    let a = create(&ch, &l, "a").await;
    draw(players.clone(), a, 2).await;
    until_fake(&hub, |s| s.drawings().len() == 2).await;
    let in_a: Vec<u64> = ids_of();
    let b = create(&ch, &l, "b").await;
    draw(players.clone(), b, 3).await;
    until_fake(&hub, |s| s.drawings().len() == 3).await;
    let in_b: Vec<u64> = ids_of();
    assert_eq!(refused(&hub), 0, "B's first adds are not refused");
    // B continues the pool: none of its IDs was issued to A.
    let top_a = *in_a.iter().max().expect("a drew");
    assert!(
        in_b.iter().all(|id| *id > top_a),
        "A issued {in_a:?}, B {in_b:?}"
    );
    let left_b = status(&ch).await.dcs_ids_left;
    assert!(left_b <= (1u64 << 30) - 5, "{left_b}");

    // Back to A: its two items are drawn again under IDs above B's, and A
    // holds both.
    campaigns
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: a,
            idempotency: Some(Idempotency { key: "to-a".into() }),
        })
        .await
        .expect("activate a");
    until_fake(&hub, |s| s.drawings().len() == 2).await;
    let back_a: Vec<u64> = ids_of();
    assert_eq!(
        refused(&hub),
        0,
        "A's adds after the switch are not refused"
    );
    let top_b = *in_b.iter().max().expect("b drew");
    assert!(
        back_a.iter().all(|id| *id > top_b),
        "{back_a:?} vs {in_b:?}"
    );
    assert_eq!(listed_dcs_ids(&ch).await.len(), 2);
    // A new add in A goes above them all.
    draw(players.clone(), a, 3).await;
    until_fake(&hub, |s| s.drawings().len() == 3).await;
    let top_now = ids_of().into_iter().max().expect("ids");
    assert_eq!(refused(&hub), 0, "A's new adds are not refused");
    assert!(top_now > *back_a.iter().max().expect("ids"));
    // And B, switched to again, holds its three.
    campaigns
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(l.clone()),
            campaign_id: b,
            idempotency: Some(Idempotency { key: "to-b".into() }),
        })
        .await
        .expect("activate b");
    until_fake(&hub, |s| s.drawings().len() == 3).await;
    let again_b: Vec<u64> = ids_of();
    assert_eq!(
        refused(&hub),
        0,
        "B's adds after the switch are not refused"
    );
    assert!(again_b.iter().all(|id| *id > top_now), "{again_b:?}");
    assert_eq!(listed_dcs_ids(&ch).await.len(), 3);
    assert!(listed_dcs_ids(&ch).await.iter().all(|(_, id)| *id != 0));

    // The server store holds the mark A closed with, over every ID A issued.
    let (_, next) = hub
        .lock()
        .server
        .as_ref()
        .expect("a server store")
        .dcs_mark()
        .expect("read")
        .expect("a mark");
    assert!(
        next > top_now - (1 << 30),
        "the mark {next} covers the top ID {top_now}"
    );
    hub.lock().close_all().expect("closes");
    hub.close();
}
