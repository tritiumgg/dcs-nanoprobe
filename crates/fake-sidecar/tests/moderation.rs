//! Players and moderation of step S16 through the API on the fake sim:
//! player keys, the player list and occupancy, player events, menu and world
//! event attribution, chat to one player, and the Moderation service. No
//! ucid appears in anything the API returns.

use std::sync::Arc;
use std::time::Duration;

use nanoprobe_api::auth::token_hash;
use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process, start};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::moderation_service_client::ModerationServiceClient;
use nanoprobe_proto::v1alpha1::players_service_client::PlayersServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, Coalition, CreateCampaignRequest, Idempotency, Lease, Write,
};
use nanoprobe_sidecar::players::{Ctx, Keyring, Roster};
use nanoprobe_sidecar::sim::RawRecord;
use nanoprobe_sidecar::sim::wire::{Input, lv};
use nanoprobe_sim_fake::FakeSim;
use nanoprobe_transport::records::Player;
use prost::Message;
use tonic::transport::Channel;
use tonic::{Code, Request};

const SECRET: [u8; 32] = [5; 32];
/// A ucid with the shape DCS gives, which no output may carry.
const UCID_A: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const UCID_B: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
const UCID_NEVER_SEEN: &str = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";
const UCID_C: &str = "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4";
const UCID_D: &str = "e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5";
const UCIDS: [&str; 5] = [UCID_A, UCID_B, UCID_C, UCID_D, UCID_NEVER_SEEN];

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
            name: "moderation".into(),
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

/// Poll until `ok` holds.
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

/// Wait until the reconciler can send an effect.
async fn until_effects(ch: &Channel, l: &Lease, c: u64) {
    let mut players = PlayersServiceClient::new(ch.clone());
    for i in 0..500 {
        let r = players
            .send_chat(pb::SendChatRequest {
                write: write(l, &format!("probe-{i}"), c),
                player_key: String::new(),
                text: "probe".into(),
            })
            .await;
        match r {
            Ok(_) => return,
            Err(e) if e.code() == Code::Unavailable => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("probe: {e}"),
        }
    }
    panic!("DCS never became ready for effects");
}

fn keyed(hub: &Hub) {
    hub.lock().roster = Roster::new(Keyring::from_secret(SECRET));
}

fn key_of(ucid: &str) -> String {
    Keyring::from_secret(SECRET).key_of(ucid)
}

async fn players_now(ch: &Channel, offline: bool) -> Vec<pb::Player> {
    PlayersServiceClient::new(ch.clone())
        .list_players(pb::ListPlayersRequest {
            include_offline: offline,
        })
        .await
        .expect("players")
        .into_inner()
        .players
}

async fn events_of(ch: &Channel, filter: pb::EventFilter) -> Vec<pb::Event> {
    EventsServiceClient::new(ch.clone())
        .read_events(pb::ReadEventsRequest {
            filter: Some(filter),
            after_sequence: 0,
            limit: 10_000,
        })
        .await
        .expect("events")
        .into_inner()
        .events
}

fn campaign_events(c: u64) -> pb::EventFilter {
    pb::EventFilter {
        campaign_id: c,
        ..pb::EventFilter::default()
    }
}

fn no_ucid(what: &str, text: &str) {
    for u in UCIDS {
        assert!(!text.contains(u), "{what} carries a ucid: {text}");
    }
}

/// A group of one unit, spawned by DCS, whose unit a player can occupy.
async fn unit_object(hub: &Hub, ch: &Channel, l: &Lease, c: u64) -> (u64, u64) {
    let g = CommandsServiceClient::new(ch.clone())
        .put_group(pb::PutGroupRequest {
            write: write(l, "group", c),
            group_id: 0,
            spec: Some(pb::GroupSpec {
                label: "g".into(),
                country: 2,
                category: pb::GroupCategory::Ground as i32,
                units: vec![pb::UnitSpec {
                    r#type: "M-113".into(),
                    position: Some(pb::Position {
                        theater: "Caucasus".into(),
                        north: 0.0,
                        east: 0.0,
                        alt: 0.0,
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
    (object, unit)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_follow_the_secret_and_chat_goes_to_a_key() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    keyed(&hub);
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    fake(&hub, |s| s.connect_client(2, UCID_A, "Ann", 2));
    until(|| async { players_now(&ch, false).await.len() == 1 }).await;
    let p = &players_now(&ch, false).await[0];
    // The key is the first 16 bytes of HMAC-SHA-256 under the secret: the
    // same on a server restarted with the secret, another under another.
    assert_eq!(p.key, key_of(UCID_A));
    assert_ne!(p.key, Keyring::from_secret([6; 32]).key_of(UCID_A));
    assert_eq!((p.name.as_str(), p.connected), ("Ann", true));
    assert_eq!(p.coalition, Coalition::Blue as i32);

    until_effects(&ch, &l, c).await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let chat = |k: &str, key: &str| pb::SendChatRequest {
        write: write(&l, k, c),
        player_key: key.into(),
        text: "welcome".into(),
    };
    let r = players
        .send_chat(chat("to-ann", &p.key))
        .await
        .expect("chat")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    assert!(fake(&hub, |s| s
        .ledger()
        .chats
        .contains(&(2, "welcome".into()))));
    // A key the server never saw is NOT_FOUND.
    let e = players
        .send_chat(chat("to-nobody", &key_of(UCID_NEVER_SEEN)))
        .await
        .expect_err("unknown");
    assert_eq!(e.code(), Code::NotFound);

    // An offline player: listed on request, and chat is FAILED_PRECONDITION.
    fake(&hub, |s| s.disconnect_client(2));
    until(|| async { players_now(&ch, false).await.is_empty() }).await;
    let all = players_now(&ch, true).await;
    assert_eq!((all.len(), all[0].connected), (1, false));
    let e = players
        .send_chat(chat("to-offline", &p.key))
        .await
        .expect_err("offline");
    assert_eq!(e.code(), Code::FailedPrecondition);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn occupancy_player_events_attribution_and_fuel() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    keyed(&hub);
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let (object, unit) = unit_object(&hub, &ch, &l, c).await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let item = players
        .put_menu_item(pb::PutMenuItemRequest {
            write: write(&l, "item", c),
            item_id: 0,
            parent_id: 0,
            audience: Some(pb::Audience {
                to: Some(pb::audience::To::All(true)),
            }),
            text: "Go".into(),
            submenu: false,
        })
        .await
        .expect("item")
        .into_inner()
        .item_id;
    until(|| async { fake(&hub, |s| s.menu_item(item).is_some()) }).await;
    let key = key_of(UCID_A);

    // Connect, take the slot of the unit, chat, rename; the BIRTH of the
    // unit comes after the slot.
    fake(&hub, |s| {
        s.connect_client(2, UCID_A, "Ann", 2);
        s.client_slot(2, 2, "m7");
    });
    until(|| async { players_now(&ch, false).await.len() == 1 }).await;
    fake(&hub, |s| {
        s.birth_event(object, Some("m7"));
        s.client_chat(2, "ready");
        s.rename_client(2, "Anna");
        s.client_slot(2, 2, "m7");
    });
    until(|| async { players_now(&ch, false).await[0].unit_id == unit }).await;
    let p = players_now(&ch, false).await.remove(0);
    assert_eq!((p.name.as_str(), p.seat), ("Anna", 0));

    // Vitals of the occupied unit: the fuel used counts.
    for fuel in [0.9f32, 0.6] {
        fake(&hub, |s| s.emit_vitals(object, 1.0, fuel));
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    until(|| async { players_now(&ch, false).await[0].fuel_used > 0.0 }).await;
    let used = players_now(&ch, false).await[0].fuel_used;
    assert!((used - 0.3).abs() < 1e-3, "{used}");

    // The occupied unit is subscribed.
    until(|| async { fake(&hub, |s| s.subscribed().contains(&object)) }).await;

    let occ = players
        .list_occupancy(pb::ListOccupancyRequest {
            campaign_id: c,
            player_key: key.clone(),
            ..pb::ListOccupancyRequest::default()
        })
        .await
        .expect("occupancy")
        .into_inner()
        .occupancy;
    assert_eq!(occ.len(), 1);
    assert_eq!((occ[0].unit_id, occ[0].seat), (unit, 0));
    assert!(occ[0].from.is_some() && occ[0].to.is_none());

    // World, author and menu events name the player.
    fake(&hub, |s| {
        s.event_between(24, Some(object), None);
        s.event_between(24, None, Some(object));
        s.emit_author_event(&[object], "tick", lv::n(1.0));
        assert!(s.select_menu(item, 0));
        assert!(s.select_menu(item, 2));
        assert!(s.select_menu(item, 9));
    });
    until(|| async {
        events_of(&ch, campaign_events(c))
            .await
            .iter()
            .filter(|e| matches!(e.body, Some(pb::event::Body::Menu(_))))
            .count()
            == 3
    })
    .await;
    let events = events_of(&ch, campaign_events(c)).await;
    let menus: Vec<&pb::MenuEvent> = events
        .iter()
        .filter_map(|e| match &e.body {
            Some(pb::event::Body::Menu(m)) => Some(m),
            _ => None,
        })
        .collect();
    // No player named: the candidates, by occupancy and connection.
    assert_eq!(menus[0].attribution, pb::MenuAttribution::Audience as i32);
    assert_eq!(menus[0].player_keys, vec![key.clone()]);
    // DCS names connection 2: that player. Connection 9 is no one.
    assert_eq!(menus[1].attribution, pb::MenuAttribution::Selector as i32);
    assert_eq!(menus[1].player_keys, vec![key.clone()]);
    assert_eq!(menus[2].attribution, pb::MenuAttribution::Selector as i32);
    assert!(menus[2].player_keys.is_empty());
    let worlds: Vec<&pb::WorldEvent> = events
        .iter()
        .filter_map(|e| match &e.body {
            Some(pb::event::Body::World(w)) if w.dcs_id == 24 => Some(w),
            _ => None,
        })
        .collect();
    assert_eq!(worlds.len(), 2);
    assert_eq!(worlds[0].initiator_players, vec![key.clone()]);
    assert_eq!(worlds[0].initiator_id, Some(unit));
    assert!(worlds[0].target_players.is_empty());
    assert_eq!(worlds[1].target_players, vec![key.clone()]);
    let author = events
        .iter()
        .find_map(|e| match &e.body {
            Some(pb::event::Body::Author(a)) => Some(a),
            _ => None,
        })
        .expect("an author event");
    assert_eq!(author.player_keys, vec![key.clone()]);

    // The player filter finds the player's events and no one else's.
    let mine = events_of(
        &ch,
        pb::EventFilter {
            campaign_id: c,
            player_key: key.clone(),
            ..pb::EventFilter::default()
        },
    )
    .await;
    assert!(mine.len() >= 8, "{}", mine.len());

    // Leave the slot, then the server.
    fake(&hub, |s| {
        s.client_slot(2, 2, "");
        s.disconnect_client(2);
    });
    until(|| async { players_now(&ch, false).await.is_empty() }).await;
    let events = events_of(&ch, campaign_events(c)).await;
    let kinds: Vec<(i32, Option<u64>, Option<String>)> = events
        .iter()
        .filter_map(|e| match &e.body {
            Some(pb::event::Body::Player(p)) => {
                assert_eq!(p.player_key, key);
                Some((p.kind, p.unit_id, p.text.clone()))
            }
            _ => None,
        })
        .collect();
    use pb::PlayerEventKind as K;
    let kind = |k: K| k as i32;
    assert_eq!(kinds[0].0, kind(K::Connect));
    let has = |k: K| kinds.iter().any(|x| x.0 == kind(k));
    assert!(has(K::Chat) && has(K::Rename) && has(K::LeaveUnit) && has(K::Disconnect));
    assert!(kinds.contains(&(kind(K::EnterUnit), Some(unit), None)));
    assert!(kinds.contains(&(kind(K::Chat), None, Some("ready".into()))));
    assert!(kinds.contains(&(kind(K::Rename), None, Some("Ann".into()))));
    let occ = players
        .list_occupancy(pb::ListOccupancyRequest {
            campaign_id: c,
            ..pb::ListOccupancyRequest::default()
        })
        .await
        .expect("occupancy")
        .into_inner()
        .occupancy;
    assert_eq!(occ.len(), 1);
    assert!(occ[0].to.is_some());
    no_ucid("the events", &format!("{events:?}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_moderation_service_works_end_to_end_without_a_ucid() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    keyed(&hub);
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    fake(&hub, |s| {
        s.connect_client(2, UCID_A, "Ann", 2);
        s.connect_client(3, UCID_B, "Bo", 1);
        s.connect_client(4, UCID_C, "Cy", 1);
    });
    until(|| async { players_now(&ch, false).await.len() == 3 }).await;
    until_effects(&ch, &l, c).await;
    let (ka, kb) = (key_of(UCID_A), key_of(UCID_B));
    let mut m = ModerationServiceClient::new(ch.clone());
    let mut out = String::new();
    let applied = |r: Option<pb::CommandResult>| {
        let r = r.expect("a result");
        assert_eq!(r.outcome, pb::Outcome::Applied as i32, "{r:?}");
    };

    // Move to a slot, then to spectators.
    let r = m
        .move_player(pb::MovePlayerRequest {
            write: write(&l, "move1", c),
            player_key: ka.clone(),
            coalition: Coalition::Blue as i32,
            slot: "55".into(),
        })
        .await
        .expect("move")
        .into_inner();
    out += &format!("{r:?}");
    applied(r.result);
    assert_eq!(fake(&hub, |s| s.clients()[&2].slot.clone()), "55");
    let r = m
        .move_player(pb::MovePlayerRequest {
            write: write(&l, "move2", c),
            player_key: ka.clone(),
            coalition: Coalition::Unspecified as i32,
            slot: String::new(),
        })
        .await
        .expect("spectators")
        .into_inner();
    applied(r.result);
    assert_eq!(fake(&hub, |s| s.clients()[&2].slot.clone()), "");

    // The connection ID now belongs to someone else: NOT_BOUND. The roster
    // is made to believe a ghost holds connection 4, where Cy is.
    let ghost = key_of(UCID_NEVER_SEEN);
    let fresh = key_of(UCID_D);
    {
        let mut g = hub.lock();
        let (kind, payload) = Input::Player(Player {
            event: 1,
            player: 4,
            side: 1,
            seat: 0,
            client: UCID_NEVER_SEEN.into(),
            name: "Ghost".into(),
            slot: String::new(),
            text: String::new(),
        })
        .encode();
        let bound = |_: u64| None;
        g.roster.fold(
            &RawRecord {
                agent_boot: 1,
                number: 0,
                kind,
                payload,
                resync: false,
            },
            &Ctx {
                now: 0.0,
                bound: &bound,
                adopt: None,
            },
        );
    }
    let r = m
        .kick(pb::KickRequest {
            write: write(&l, "kick-ghost", c),
            player_key: ghost.clone(),
            reason: "x".into(),
        })
        .await
        .expect("kick")
        .into_inner();
    out += &format!("{r:?}");
    let res = r.result.expect("result");
    assert_eq!(res.outcome, pb::Outcome::Refused as i32);
    assert_eq!(res.code, pb::ResultCode::NotBound as i32);
    assert!(fake(&hub, |s| s.clients().contains_key(&4)), "Cy stays");

    // Kick a player: gone, with an event that names the key and the role.
    let r = m
        .kick(pb::KickRequest {
            write: write(&l, "kick-bo", c),
            player_key: kb.clone(),
            reason: "spam".into(),
        })
        .await
        .expect("kick")
        .into_inner();
    out += &format!("{r:?}");
    applied(r.result);
    assert!(!fake(&hub, |s| s.clients().contains_key(&3)));

    // Ban a connected player by key, and a ucid never seen by ucid.
    let r = m
        .ban(pb::BanRequest {
            write: write(&l, "ban-ann", c),
            target: Some(pb::ban_request::Target::PlayerKey(ka.clone())),
            seconds: 600.0,
            reason: "cheating".into(),
        })
        .await
        .expect("ban")
        .into_inner();
    out += &format!("{r:?}");
    applied(r.result);
    assert!(!fake(&hub, |s| s.connect_client(2, UCID_A, "Ann", 2)));
    let r = m
        .ban(pb::BanRequest {
            write: write(&l, "ban-new", c),
            target: Some(pb::ban_request::Target::Ucid(UCID_D.into())),
            seconds: 0.0,
            reason: "known abuser".into(),
        })
        .await
        .expect("ban by ucid")
        .into_inner();
    out += &format!("{r:?}");
    applied(r.result);
    assert_eq!(fake(&hub, |s| s.bans().len()), 2);

    // The list names keys. An entry for a ucid the server never saw gets the
    // key derived like any other, which names the entry for an unban.
    let r = m
        .list_bans(pb::ListBansRequest {
            write: write(&l, "bans", c),
        })
        .await
        .expect("bans")
        .into_inner();
    out += &format!("{r:?}");
    applied(r.result.clone());
    let keys: Vec<&str> = r.bans.iter().map(|b| b.player_key.as_str()).collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&ka.as_str()) && keys.contains(&fresh.as_str()));
    let ann = r.bans.iter().find(|b| b.player_key == ka).unwrap();
    assert_eq!(
        (ann.name.as_str(), ann.reason.as_str()),
        ("Ann", "cheating")
    );
    assert!(ann.until.is_some());
    assert!(
        r.bans
            .iter()
            .find(|b| b.player_key == fresh)
            .unwrap()
            .until
            .is_none()
    );

    // Unban by key, and by a ucid that is not on the list (refused).
    let r = m
        .unban(pb::UnbanRequest {
            write: write(&l, "unban-ann", c),
            target: Some(pb::unban_request::Target::PlayerKey(ka.clone())),
        })
        .await
        .expect("unban")
        .into_inner();
    applied(r.result);
    let r = m
        .unban(pb::UnbanRequest {
            write: write(&l, "unban-again", c),
            target: Some(pb::unban_request::Target::Ucid(UCID_A.into())),
        })
        .await
        .expect("unban")
        .into_inner();
    out += &format!("{r:?}");
    assert_eq!(r.result.unwrap().outcome, pb::Outcome::Refused as i32);
    assert!(fake(&hub, |s| s.connect_client(2, UCID_A, "Ann", 2)));

    // What a call cannot take.
    for (what, e) in [
        (
            "kick of an unseen key",
            m.kick(pb::KickRequest {
                write: write(&l, "k1", c),
                player_key: "f".repeat(32),
                reason: String::new(),
            })
            .await
            .map(|_| ()),
        ),
        (
            "ban with no target",
            m.ban(pb::BanRequest {
                write: write(&l, "k2", c),
                ..pb::BanRequest::default()
            })
            .await
            .map(|_| ()),
        ),
        (
            "ban for -1 s",
            m.ban(pb::BanRequest {
                write: write(&l, "k3", c),
                target: Some(pb::ban_request::Target::Ucid("abc".into())),
                seconds: -1.0,
                reason: String::new(),
            })
            .await
            .map(|_| ()),
        ),
    ] {
        let e = e.expect_err(what);
        out += e.message();
        assert!(
            matches!(e.code(), Code::NotFound | Code::InvalidArgument),
            "{what}: {e}"
        );
    }
    // A kick of a player who left: FAILED_PRECONDITION.
    fake(&hub, |s| s.disconnect_client(2));
    until(|| async { players_now(&ch, false).await.iter().all(|p| p.key != ka) }).await;
    let e = m
        .kick(pb::KickRequest {
            write: write(&l, "k4", c),
            player_key: ka.clone(),
            reason: String::new(),
        })
        .await
        .expect_err("offline");
    assert_eq!(e.code(), Code::FailedPrecondition);
    out += e.message();

    // Each applied call is an event with the role of the token.
    let events = events_of(&ch, campaign_events(c)).await;
    let moderated: Vec<&pb::ModerationEvent> = events
        .iter()
        .filter_map(|e| match &e.body {
            Some(pb::event::Body::Moderation(m)) => Some(m),
            _ => None,
        })
        .collect();
    use pb::ModerationAction as A;
    let seen: Vec<(i32, &str)> = moderated
        .iter()
        .map(|m| (m.action, m.player_key.as_str()))
        .collect();
    assert_eq!(
        seen,
        vec![
            (A::Move as i32, ka.as_str()),
            (A::Move as i32, ka.as_str()),
            (A::Kick as i32, kb.as_str()),
            (A::Ban as i32, ka.as_str()),
            (A::Ban as i32, fresh.as_str()),
            (A::Unban as i32, ka.as_str()),
        ]
    );
    assert!(moderated.iter().all(|m| m.role == pb::Role::Admin as i32));
    assert!(
        moderated
            .iter()
            .all(|m| m.result.as_ref().unwrap().outcome == pb::Outcome::Applied as i32)
    );
    assert_eq!(moderated[2].reason, "spam");
    // The key filter finds a player's moderation events.
    let mine = events_of(
        &ch,
        pb::EventFilter {
            campaign_id: c,
            player_key: kb.clone(),
            ..pb::EventFilter::default()
        },
    )
    .await;
    assert!(
        mine.iter()
            .any(|e| matches!(e.body, Some(pb::event::Body::Moderation(_))))
    );

    // No ucid in any response, event, error, or the debug text of the state.
    no_ucid("a response", &out);
    no_ucid("the events", &format!("{events:?}"));
    no_ucid(
        "the players",
        &format!("{:?}", players_now(&ch, true).await),
    );
    let g = hub.lock();
    no_ucid("the roster's debug text", &format!("{:?}", g.roster));
    let log = g.server_log.after(0, 10_000).cloned().collect::<Vec<_>>();
    for e in log {
        no_ucid("the server log", &format!("{:?}", e.encode_to_vec()));
        no_ucid("the server log", &format!("{e:?}"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_an_admin_moderates() {
    const CONTROLLER: &str = "controller-0123456789abcdef01234567";
    const ADMIN: &str = "admin-0123456789abcdef0123456789abcd";
    let dir = std::env::temp_dir().join(format!("nanoprobe-mod-tokens-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("tokens.toml");
    std::fs::write(
        &path,
        format!(
            "[[token]]\nname = \"campaign\"\nrole = \"controller\"\nsha256 = \"{}\"\n\n\
             [[token]]\nname = \"ops\"\nrole = \"admin\"\nsha256 = \"{}\"\n",
            token_hash(CONTROLLER),
            token_hash(ADMIN),
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
        move |mut r: Request<()>| {
            r.metadata_mut().insert(
                "authorization",
                format!("Bearer {t}").parse().expect("ascii"),
            );
            Ok(r)
        }
    };
    let mut ctl = ModerationServiceClient::with_interceptor(ch.clone(), with(CONTROLLER));
    let e = ctl
        .kick(pb::KickRequest::default())
        .await
        .expect_err("controller");
    assert_eq!(e.code(), Code::PermissionDenied);
    let e = ctl
        .list_bans(pb::ListBansRequest::default())
        .await
        .expect_err("controller");
    assert_eq!(e.code(), Code::PermissionDenied);
    // An admin passes the role check and meets the next one, the lease.
    let mut adm = ModerationServiceClient::with_interceptor(ch.clone(), with(ADMIN));
    let e = adm
        .kick(pb::KickRequest {
            write: write(&Lease::default(), "k", 0),
            ..pb::KickRequest::default()
        })
        .await
        .expect_err("no lease");
    assert_ne!(e.code(), Code::PermissionDenied);
    let hub: Arc<Hub> = Arc::clone(&server.hub);
    drop(hub);
    server.stop().await.expect("stop");
    let _ = std::fs::remove_dir_all(&dir);
}
