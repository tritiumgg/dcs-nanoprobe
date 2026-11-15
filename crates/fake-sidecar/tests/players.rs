//! The player-facing calls of step S16 through the API on the fake sim:
//! marks, drawings and menu items, messages and chat, Lua extensions and
//! raw Lua, and the menu and author events.

use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::extension_service_client::ExtensionServiceClient;
use nanoprobe_proto::v1alpha1::players_service_client::PlayersServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, Coalition, CreateCampaignRequest, Idempotency, Lease, Write,
};
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sim_fake::{ExtensionStats, FakeSim};
use prost_types::{ListValue, Value, value::Kind};
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
            name: "players".into(),
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

fn blue() -> Option<pb::Audience> {
    Some(pb::Audience {
        to: Some(pb::audience::To::Coalition(Coalition::Blue as i32)),
    })
}

fn number(n: f64) -> Value {
    Value {
        kind: Some(Kind::NumberValue(n)),
    }
}

fn mark_request(l: &Lease, k: &str, c: u64, id: u64, text: &str) -> pb::PutMarkRequest {
    pb::PutMarkRequest {
        write: write(l, k, c),
        mark_id: id,
        audience: blue(),
        position: Some(pb::Position {
            theater: "Caucasus".into(),
            north: 10.0,
            east: 20.0,
            alt: 3.0,
        }),
        text: text.into(),
        read_only: true,
    }
}

async fn players_group(ch: &Channel, l: &Lease, c: u64) -> pb::PutGroupResponse {
    CommandsServiceClient::new(ch.clone())
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
        .into_inner()
}

/// Wait until the reconciler can send an effect: a message to everyone is
/// accepted. The probe's key changes with each attempt, and it sends one
/// message, which a test that counts messages takes into account.
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
            Err(e) if e.code() == tonic::Code::Unavailable => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(e) => panic!("probe: {e}"),
        }
    }
    panic!("DCS never became ready for effects");
}

fn code_of<T>(r: Result<T, tonic::Status>) -> tonic::Code {
    r.err().expect("an error").code()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marks_drawings_and_menus_are_set_replaced_removed_and_set_again() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let mut cmd = CommandsServiceClient::new(ch.clone());

    // A mark: set, then replaced under its ID.
    let m = players
        .put_mark(mark_request(&l, "m1", c, 0, "rally"))
        .await
        .expect("mark")
        .into_inner();
    assert_ne!(m.mark_id, 0);
    until_fake(&hub, |s| s.mark(m.mark_id).is_some()).await;
    let held = fake(&hub, |s| s.mark(m.mark_id).cloned()).expect("held");
    assert_eq!(
        (
            held.scope,
            held.target,
            held.read_only,
            held.at,
            held.text.as_str()
        ),
        (2, 2, true, (10.0, 3.0, 20.0), "rally")
    );
    let again = players
        .put_mark(mark_request(&l, "m2", c, m.mark_id, "rally point"))
        .await
        .expect("replace")
        .into_inner();
    assert_eq!(again.mark_id, m.mark_id);
    until_fake(&hub, |s| {
        s.mark(m.mark_id).is_some_and(|x| x.text == "rally point")
    })
    .await;
    assert_eq!(fake(&hub, |s| s.mark(m.mark_id).unwrap().rev), 2);

    // A drawing, for everyone; the points pass through as tables.
    let d = players
        .put_drawing(pb::PutDrawingRequest {
            write: write(&l, "d1", c),
            drawing_id: 0,
            audience: all(),
            shape: 7,
            args: Some(ListValue {
                values: vec![number(1.0), number(2.0)],
            }),
        })
        .await
        .expect("drawing")
        .into_inner();
    until_fake(&hub, |s| s.drawing(d.drawing_id).is_some()).await;
    let held = fake(&hub, |s| s.drawing(d.drawing_id).cloned()).unwrap();
    assert_eq!((held.shape, held.scope), (7, 1));
    assert_eq!(held.args, lv::list(vec![lv::n(1.0), lv::n(2.0)]));

    // A submenu and a command under it.
    let sub = players
        .put_menu_item(pb::PutMenuItemRequest {
            write: write(&l, "s1", c),
            item_id: 0,
            parent_id: 0,
            audience: all(),
            text: "Tools".into(),
            submenu: true,
        })
        .await
        .expect("submenu")
        .into_inner();
    let item = players
        .put_menu_item(pb::PutMenuItemRequest {
            write: write(&l, "i1", c),
            item_id: 0,
            parent_id: sub.item_id,
            audience: all(),
            text: "Go".into(),
            submenu: false,
        })
        .await
        .expect("item")
        .into_inner();
    until_fake(&hub, |s| s.menu_item(item.item_id).is_some()).await;
    assert_eq!(
        fake(&hub, |s| s.menu_item(item.item_id).unwrap().parent),
        sub.item_id
    );

    // Refused calls: a unit audience, a drawing for a group, a parent that
    // is not a submenu, an unknown entity, a kind mismatch.
    let unit = Some(pb::Audience {
        to: Some(pb::audience::To::UnitId(5)),
    });
    let mut bad = mark_request(&l, "bad1", c, 0, "x");
    bad.audience = unit;
    assert_eq!(
        code_of(players.put_mark(bad).await),
        tonic::Code::InvalidArgument
    );
    let bad = pb::PutMenuItemRequest {
        write: write(&l, "bad2", c),
        item_id: 0,
        parent_id: item.item_id,
        audience: all(),
        text: "y".into(),
        submenu: false,
    };
    assert_eq!(
        code_of(players.put_menu_item(bad).await),
        tonic::Code::InvalidArgument
    );
    let bad = mark_request(&l, "bad3", c, 987_654, "x");
    assert_eq!(code_of(players.put_mark(bad).await), tonic::Code::NotFound);
    let bad = mark_request(&l, "bad4", c, d.drawing_id, "x");
    assert_eq!(
        code_of(players.put_mark(bad).await),
        tonic::Code::InvalidArgument
    );
    let mut bad = mark_request(&l, "bad5", c, 0, "x");
    bad.position = None;
    assert_eq!(
        code_of(players.put_mark(bad).await),
        tonic::Code::InvalidArgument
    );

    // A mission load: everything is set again.
    fake(&hub, |s| s.load_mission());
    until_fake(&hub, |s| {
        s.mark(m.mark_id).is_some()
            && s.drawing(d.drawing_id).is_some()
            && s.menu_items().len() == 2
    })
    .await;
    let order: Vec<u64> = fake(&hub, |s| s.menu_items().iter().map(|(e, _)| *e).collect());
    assert_eq!(order, vec![sub.item_id, item.item_id]);

    // RemoveEntity takes a mark, a drawing and a submenu with its items.
    for (k, id) in [("r1", m.mark_id), ("r2", d.drawing_id), ("r3", sub.item_id)] {
        cmd.remove_entity(pb::RemoveEntityRequest {
            write: write(&l, k, c),
            entity_id: id,
        })
        .await
        .expect("remove");
    }
    until_fake(&hub, |s| {
        s.mark(m.mark_id).is_none()
            && s.drawing(d.drawing_id).is_none()
            && s.menu_items().is_empty()
    })
    .await;
    // Removing again changes nothing and is not an error.
    cmd.remove_entity(pb::RemoveEntityRequest {
        write: write(&l, "r4", c),
        entity_id: m.mark_id,
    })
    .await
    .expect("idempotent");
    assert_eq!(
        code_of(
            players
                .put_menu_item(pb::PutMenuItemRequest {
                    write: write(&l, "i2", c),
                    item_id: item.item_id,
                    parent_id: 0,
                    audience: all(),
                    text: "back".into(),
                    submenu: false,
                })
                .await
        ),
        tonic::Code::NotFound,
        "a removed item cannot be replaced"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn messages_and_chat_are_effects_sent_once() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut players = PlayersServiceClient::new(ch.clone());
    until_effects(&ch, &l, c).await;
    let probes = fake(&hub, |s| s.ledger().chats.len());
    let msg = |k: &str, audience, seconds: f64, text: &str| pb::SendMessageRequest {
        write: write(&l, k, c),
        audience,
        text: text.into(),
        seconds,
        clear: true,
    };
    let r = players
        .send_message(msg("msg", blue(), 12.5, "hello"))
        .await
        .expect("message")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    assert_eq!(
        fake(&hub, |s| s.ledger().messages.clone()),
        vec![(2, 2, true, 12.5, "hello".to_owned())]
    );
    // The same key sends nothing again.
    players
        .send_message(msg("msg", blue(), 12.5, "hello"))
        .await
        .expect("repeat");
    assert_eq!(fake(&hub, |s| s.ledger().messages.len()), 1);
    // Invalid calls send nothing.
    for (k, seconds, text) in [
        ("neg", -1.0, "x"),
        ("nan", f64::NAN, "x"),
        ("empty", 1.0, ""),
    ] {
        assert_eq!(
            code_of(players.send_message(msg(k, all(), seconds, text)).await),
            tonic::Code::InvalidArgument,
            "{k}"
        );
    }
    let e = players
        .send_message(msg(
            "nogroup",
            Some(pb::Audience {
                to: Some(pb::audience::To::GroupId(4242)),
            }),
            1.0,
            "x",
        ))
        .await
        .expect_err("no such group");
    assert_eq!(e.code(), tonic::Code::NotFound);

    let r = players
        .send_chat(pb::SendChatRequest {
            write: write(&l, "chat", c),
            player_key: String::new(),
            text: "welcome".into(),
        })
        .await
        .expect("chat")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    assert_eq!(
        fake(&hub, |s| s.ledger().chats[probes..].to_vec()),
        vec![(0, "welcome".to_owned())]
    );
    // A player key the server never saw is not found.
    let e = players
        .send_chat(pb::SendChatRequest {
            write: write(&l, "chat2", c),
            player_key: "p".into(),
            text: "x".into(),
        })
        .await
        .expect_err("no such player");
    assert_eq!(e.code(), tonic::Code::NotFound);

    // A new generation sends no effect again.
    fake(&hub, |s| s.load_mission());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake(&hub, |s| s.ledger().messages.len()), 1);
    assert_eq!(fake(&hub, |s| s.ledger().chats.len()), probes + 1);

    // The calls left unbuilt still answer UNIMPLEMENTED.
    let e = players
        .erase_player(pb::ErasePlayerRequest::default())
        .await
        .expect_err("not built");
    assert_eq!(e.code(), tonic::Code::Unimplemented);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extensions_are_registered_listed_invoked_evaluated_and_removed() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut ext = ExtensionServiceClient::new(ch.clone());
    let list = |ext: &mut ExtensionServiceClient<Channel>| {
        let mut ext = ext.clone();
        async move {
            ext.list_lua(pb::ListLuaRequest { campaign_id: c })
                .await
                .expect("list")
                .into_inner()
                .extensions
        }
    };

    let r = ext
        .register_lua(pb::RegisterLuaRequest {
            write: write(&l, "reg", c),
            name: "count".into(),
            source: "return {}".into(),
        })
        .await
        .expect("register")
        .into_inner();
    assert_eq!(r.revision, 1);
    until_fake(&hub, |s| s.extension_rev("count") == 1).await;
    let mut seen = Vec::new();
    for _ in 0..200 {
        seen = list(&mut ext).await;
        if seen.first().is_some_and(|e| e.applied_revision == 1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(seen.len(), 1);
    assert_eq!(
        (
            seen[0].name.as_str(),
            seen[0].revision,
            seen[0].applied_revision,
            seen[0].calls
        ),
        ("count", 1, 1, 0)
    );

    // The agent's EXTENSION record fills the run time.
    fake(&hub, |s| {
        s.emit_extension_stats(ExtensionStats {
            calls: 9,
            total_us: 900,
            max_us: 300,
            rev: 1,
            events_skipped: 2,
            stopped: 1,
            name: "count".into(),
        })
    });
    let mut stats = None;
    for _ in 0..200 {
        stats = list(&mut ext).await.into_iter().find(|e| e.calls == 9);
        if stats.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let s = stats.expect("the record arrived");
    assert_eq!(
        (s.total_us, s.max_us, s.events_skipped, s.stopped),
        (900, 300, 2, true)
    );
    // The latest record of the name wins.
    fake(&hub, |s| {
        s.emit_extension_stats(ExtensionStats {
            calls: 10,
            rev: 1,
            name: "count".into(),
            ..ExtensionStats::default()
        })
    });
    for _ in 0..200 {
        if list(&mut ext)
            .await
            .iter()
            .any(|e| e.calls == 10 && !e.stopped)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        list(&mut ext)
            .await
            .iter()
            .any(|e| e.calls == 10 && !e.stopped)
    );

    // Invoke: the first value the extension returns.
    let r = ext
        .invoke_lua(pb::InvokeLuaRequest {
            write: write(&l, "inv", c),
            name: "count".into(),
            args: Some(ListValue {
                values: vec![number(5.0)],
            }),
        })
        .await
        .expect("invoke")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    assert_eq!(r.value, Some(number(1.0)));
    let e = ext
        .invoke_lua(pb::InvokeLuaRequest {
            write: write(&l, "inv2", c),
            name: "missing".into(),
            args: None,
        })
        .await
        .expect_err("unknown");
    assert_eq!(e.code(), tonic::Code::NotFound);

    // Eval: a value and an error.
    let r = ext
        .eval_lua(pb::EvalLuaRequest {
            write: write(&l, "ev", c),
            source: "return 42".into(),
        })
        .await
        .expect("eval")
        .into_inner();
    assert_eq!(r.value, Some(number(42.0)));
    let r = ext
        .eval_lua(pb::EvalLuaRequest {
            write: write(&l, "ev2", c),
            source: "error('x')".into(),
        })
        .await
        .expect("eval")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Refused as i32
    );
    assert_eq!(r.value, None);
    assert_eq!(fake(&hub, |s| s.ledger().evals.len()), 2);
    let e = ext
        .eval_lua(pb::EvalLuaRequest {
            write: write(&l, "ev3", c),
            source: String::new(),
        })
        .await
        .expect_err("empty");
    assert_eq!(e.code(), tonic::Code::InvalidArgument);

    // Removal reaches the agent; the list is empty.
    ext.remove_lua(pb::RemoveLuaRequest {
        write: write(&l, "rm", c),
        name: "count".into(),
    })
    .await
    .expect("remove");
    until_fake(&hub, |s| s.extension_rev("count") == 0).await;
    assert!(list(&mut ext).await.is_empty());
    let e = ext
        .remove_lua(pb::RemoveLuaRequest {
            write: write(&l, "rm2", c),
            name: "count".into(),
        })
        .await
        .expect_err("gone");
    assert_eq!(e.code(), tonic::Code::NotFound);
    let e = ext
        .get_data(pb::GetDataRequest::default())
        .await
        .expect_err("not built");
    assert_eq!(e.code(), tonic::Code::Unimplemented);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_lua_off_comes_back_in_the_result() {
    let mut f = fake_config();
    f.raw_lua = false;
    let (hub, ch) = in_process(cfg(), f).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut ext = ExtensionServiceClient::new(ch.clone());
    until_effects(&ch, &l, c).await;
    let r = ext
        .eval_lua(pb::EvalLuaRequest {
            write: write(&l, "ev", c),
            source: "return 1".into(),
        })
        .await
        .expect("eval")
        .into_inner();
    let result = r.result.expect("result");
    assert_eq!(result.outcome, pb::Outcome::Rejected as i32);
    assert_eq!(result.code, pb::ResultCode::RawLuaOff as i32);
    assert!(fake(&hub, |s| s.ledger().evals.is_empty()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn menu_selections_and_author_events_are_events() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut players = PlayersServiceClient::new(ch.clone());
    let item = players
        .put_menu_item(pb::PutMenuItemRequest {
            write: write(&l, "i1", c),
            item_id: 0,
            parent_id: 0,
            audience: all(),
            text: "Go".into(),
            submenu: false,
        })
        .await
        .expect("item")
        .into_inner();
    until_fake(&hub, |s| s.menu_item(item.item_id).is_some()).await;
    // An author event names the units whose DCS objects it lists; an object
    // the platform has no entity for is left out.
    let g = players_group(&ch, &l, c).await;
    until_fake(&hub, |s| {
        s.objects().iter().any(|(_, _, e)| *e == g.unit_ids[0])
    })
    .await;
    let object = fake(&hub, |s| {
        s.objects()
            .into_iter()
            .find(|(_, _, e)| *e == g.unit_ids[0])
            .map(|(o, _, _)| o)
            .expect("object")
    });
    fake(&hub, |s| {
        assert!(s.select_menu(item.item_id, 9));
        s.emit_author_event(&[object, 987_654], "tick", lv::obj(vec![("n", lv::n(3.0))]));
        s.emit_player_record();
    });
    let mut events = EventsServiceClient::new(ch.clone());
    let mut menu = None;
    let mut author = None;
    let mut chat = None;
    for _ in 0..200 {
        let r = events
            .read_events(pb::ReadEventsRequest {
                filter: Some(pb::EventFilter {
                    campaign_id: c,
                    ..pb::EventFilter::default()
                }),
                after_sequence: 0,
                limit: 1_000,
            })
            .await
            .expect("read")
            .into_inner();
        for e in r.events {
            match e.body {
                Some(pb::event::Body::Menu(m)) => menu = Some(m),
                Some(pb::event::Body::Author(a)) => author = Some(a),
                Some(pb::event::Body::Player(p)) => chat = Some(p),
                _ => {}
            }
        }
        if menu.is_some() && author.is_some() && chat.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A chat from a client the server never saw connect is an event: the
    // record names the client.
    let chat = chat.expect("a chat event");
    assert_eq!(
        (chat.name.as_str(), chat.text.as_deref()),
        ("pilot", Some("hello"))
    );
    let m = menu.expect("a menu event");
    assert_eq!(m.item_id, item.item_id);
    // Connection 9 is no player the server knows: SELECTOR, no key.
    assert_eq!(m.attribution, pb::MenuAttribution::Selector as i32);
    assert!(m.player_keys.is_empty());
    let a = author.expect("an author event");
    assert_eq!(a.name, "tick");
    assert_eq!(a.entity_ids, vec![g.unit_ids[0]]);
    assert_eq!(
        a.payload,
        Some(Value {
            kind: Some(Kind::StructValue(prost_types::Struct {
                fields: [("n".to_owned(), number(3.0))].into_iter().collect(),
            }))
        })
    );
}
