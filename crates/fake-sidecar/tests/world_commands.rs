//! The world commands of step S15 through the API on the fake sim: options,
//! removal, airbase owners, warehouse stock and adjustments, sample rates,
//! damage, capture, coalitions and attacks on units and statics.

use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::{fake_config, in_process};
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::events_service_client::EventsServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, Coalition, CreateCampaignRequest, EntityState, GroupCategory,
    GroupSpec, Idempotency, Lease, ListWorldRequest, Order, PutGroupRequest, UnitSpec,
    WatchWorldRequest, WorldFilter, Write, entity, order, world_change,
};
use nanoprobe_sidecar::stock::Item;
use nanoprobe_sim_fake::FakeSim;
use prost::Message;
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
            name: "world".into(),
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

fn group(
    l: &Lease,
    k: &str,
    campaign: u64,
    north: f64,
    country: u32,
    virt: bool,
) -> PutGroupRequest {
    PutGroupRequest {
        write: write(l, k, campaign),
        group_id: 0,
        spec: Some(GroupSpec {
            label: k.into(),
            country,
            category: GroupCategory::Ground as i32,
            units: vec![UnitSpec {
                r#type: "M-113".into(),
                position: Some(pb::Position {
                    theater: "Caucasus".into(),
                    north,
                    east: 0.0,
                    alt: 0.0,
                }),
                ..UnitSpec::default()
            }],
            r#virtual: virt,
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

fn live(g: &pb::Group) -> bool {
    g.state == EntityState::Live as i32
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
async fn options_removal_and_coalitions() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let g = cmd
        .put_group(group(&l, "g", c, 0.0, 2, false))
        .await
        .expect("put")
        .into_inner();
    // A materialized group's coalition comes from its OBJECT records.
    let seen = until(&ch, c, g.group_id, |x| {
        live(x) && x.coalition == Coalition::Blue as i32
    })
    .await;
    assert_eq!(seen.spec_revision, g.spec_revision);

    // Not materialized: the country map. A country it lacks stays unspecified.
    let v = cmd
        .put_group(group(&l, "v", c, 500.0, 2, true))
        .await
        .expect("put")
        .into_inner();
    until(&ch, c, v.group_id, |x| {
        x.coalition == Coalition::Blue as i32
    })
    .await;
    let u = cmd
        .put_group(group(&l, "u", c, 900.0, 99, true))
        .await
        .expect("put")
        .into_inner();
    let unknown = get_group(&ch, c, u.group_id).await.expect("listed");
    assert_eq!(unknown.coalition, Coalition::Unspecified as i32);

    let r = cmd
        .set_options(pb::SetOptionsRequest {
            write: write(&l, "opt", c),
            group_id: g.group_id,
            options: [(3u32, prost_types::Value::from(2.0))]
                .into_iter()
                .collect(),
        })
        .await
        .expect("options")
        .into_inner();
    assert!(r.revision > 0);
    let id = g.group_id;
    until_fake(&hub, |s| s.option_of(id, 3).is_some()).await;
    // The spec revision did not move.
    let after = get_group(&ch, c, g.group_id).await.expect("listed");
    assert_eq!(after.spec_revision, g.spec_revision);
    let e = cmd
        .set_options(pb::SetOptionsRequest {
            write: write(&l, "opt-none", c),
            group_id: g.group_id,
            options: Default::default(),
        })
        .await
        .expect_err("no options");
    assert_eq!(e.code(), tonic::Code::InvalidArgument);

    // Removal takes the group out of DCS and out of the list.
    cmd.remove_entity(pb::RemoveEntityRequest {
        write: write(&l, "rm", c),
        entity_id: g.group_id,
    })
    .await
    .expect("remove");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if get_group(&ch, c, g.group_id).await.is_none()
                && fake(&hub, |s| {
                    s.objects()
                        .iter()
                        .all(|(_, camp, e)| !(*camp == c && *e != 0 && *e == id + 1))
                })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("removed");
    let e = cmd
        .remove_entity(pb::RemoveEntityRequest {
            write: write(&l, "rm-none", c),
            entity_id: 987_654,
        })
        .await
        .expect_err("unknown");
    assert_eq!(e.code(), tonic::Code::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn airbases_warehouses_and_rates() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    fake(&hub, |s| {
        let mut st = nanoprobe_sidecar::stock::Stock::default();
        st.set(Item::Weapon("AIM-9X".into()), 4.0);
        s.add_airbase("Batumi", 1, st);
    });
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    cmd.set_airbase_owner(pb::SetAirbaseOwnerRequest {
        write: write(&l, "owner", c),
        theater: "Caucasus".into(),
        airbase: "Batumi".into(),
        owner: Coalition::Blue as i32,
    })
    .await
    .expect("owner");
    until_fake(&hub, |s| s.airbase_owner("Batumi") == Some(2)).await;
    let e = cmd
        .set_airbase_owner(pb::SetAirbaseOwnerRequest {
            write: write(&l, "owner-bad", c),
            theater: "Caucasus".into(),
            airbase: "Batumi".into(),
            owner: Coalition::Unspecified as i32,
        })
        .await
        .expect_err("unspecified");
    assert_eq!(e.code(), tonic::Code::InvalidArgument);

    cmd.set_warehouse_stock(pb::SetWarehouseStockRequest {
        write: write(&l, "stock", c),
        theater: "Caucasus".into(),
        airbase: "Batumi".into(),
        weapon: [("AIM-9X".to_owned(), 10.0)].into_iter().collect(),
        stock: [("AIM-120".to_owned(), 2.0)].into_iter().collect(),
        aircraft: [("F-16C".to_owned(), 3.0)].into_iter().collect(),
        liquids: [(0u32, 5000.0)].into_iter().collect(),
    })
    .await
    .expect("stock");
    until_fake(&hub, |s| {
        s.airbase_stock("Batumi").is_some_and(|k| {
            k.get(&Item::Weapon("AIM-9X".into())) == Some(10.0)
                && k.get(&Item::Weapon("AIM-120".into())) == Some(2.0)
                && k.get(&Item::Aircraft("F-16C".into())) == Some(3.0)
                && k.get(&Item::Liquid(0)) == Some(5000.0)
        })
    })
    .await;

    // An adjustment is an effect that returns with its result.
    let r = cmd
        .adjust_warehouse_stock(pb::AdjustWarehouseStockRequest {
            write: write(&l, "adjust", c),
            theater: "Caucasus".into(),
            airbase: "Batumi".into(),
            weapon: [("AIM-9X".to_owned(), -4.0)].into_iter().collect(),
            aircraft: Default::default(),
            liquids: Default::default(),
        })
        .await
        .expect("adjust")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    assert_eq!(
        fake(&hub, |s| s
            .airbase_stock("Batumi")
            .and_then(|k| k.get(&Item::Weapon("AIM-9X".into())))),
        Some(6.0)
    );
    // The same key again returns the first response and sends nothing.
    cmd.adjust_warehouse_stock(pb::AdjustWarehouseStockRequest {
        write: write(&l, "adjust", c),
        theater: "Caucasus".into(),
        airbase: "Batumi".into(),
        weapon: [("AIM-9X".to_owned(), -4.0)].into_iter().collect(),
        aircraft: Default::default(),
        liquids: Default::default(),
    })
    .await
    .expect("repeat");
    assert_eq!(
        fake(&hub, |s| s
            .airbase_stock("Batumi")
            .and_then(|k| k.get(&Item::Weapon("AIM-9X".into())))),
        Some(6.0)
    );

    cmd.set_sample_rates(pb::SetSampleRatesRequest {
        write: write(&l, "rates", c),
        position_period: 2.0,
        warehouse_period: 9.0,
        ..pb::SetSampleRatesRequest::default()
    })
    .await
    .expect("rates");
    until_fake(&hub, |s| s.rates().0.warehouse_s == 9.0).await;
    let e = cmd
        .set_sample_rates(pb::SetSampleRatesRequest {
            write: write(&l, "rates-bad", c),
            vitals_period: -1.0,
            ..pb::SetSampleRatesRequest::default()
        })
        .await
        .expect_err("negative");
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn damage_capture_and_attacks_on_units_and_statics() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let a = cmd
        .put_group(group(&l, "a", c, 0.0, 2, false))
        .await
        .expect("put")
        .into_inner();
    let b = cmd
        .put_group(group(&l, "b", c, 300.0, 2, false))
        .await
        .expect("put")
        .into_inner();
    let st = cmd
        .put_static(pb::PutStaticRequest {
            write: write(&l, "static", c),
            static_id: 0,
            spec: Some(pb::StaticSpec {
                label: "tent".into(),
                country: 2,
                r#type: "Tent".into(),
                position: Some(pb::Position {
                    theater: "Caucasus".into(),
                    north: -50.0,
                    east: 0.0,
                    alt: 0.0,
                }),
                heading: 0.0,
                extra: None,
            }),
        })
        .await
        .expect("static")
        .into_inner();
    until(&ch, c, a.group_id, live).await;
    until(&ch, c, b.group_id, live).await;

    for (k, target) in [("unit", b.unit_ids[0]), ("static", st.static_id)] {
        let r = cmd
            .set_order(pb::SetOrderRequest {
                write: write(&l, &format!("attack-{k}"), c),
                group_id: a.group_id,
                order: Some(Order {
                    intent: Some(order::Intent::Attack(pb::AttackOrder { target_id: target })),
                }),
            })
            .await
            .unwrap_or_else(|e| panic!("attack on a {k}: {e}"))
            .into_inner();
        assert!(r.order_revision > 0);
    }
    let e = cmd
        .set_order(pb::SetOrderRequest {
            write: write(&l, "nobody", c),
            group_id: a.group_id,
            order: Some(Order {
                intent: Some(order::Intent::Attack(pb::AttackOrder {
                    target_id: 987_654,
                })),
            }),
        })
        .await
        .expect_err("unknown target");
    assert_eq!(e.code(), tonic::Code::NotFound);
    let a_id = a.group_id;
    until_fake(&hub, |s| s.attack_unit_of(c, a_id).is_some()).await;

    // Damage is an effect with a result; the fake holds the new life.
    let unit = b.unit_ids[0];
    let r = cmd
        .do_effect(pb::DoEffectRequest {
            write: write(&l, "damage", c),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::Damage(pb::Damage {
                    unit_id: unit,
                    life_percent: 10.0,
                })),
            }),
        })
        .await
        .expect("damage")
        .into_inner();
    assert_eq!(
        r.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    assert_eq!(fake(&hub, |s| s.life_percent_of(unit)), Some(10.0));
    let e = cmd
        .do_effect(pb::DoEffectRequest {
            write: write(&l, "damage-bad", c),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::Damage(pb::Damage {
                    unit_id: unit,
                    life_percent: 101.0,
                })),
            }),
        })
        .await
        .expect_err("out of range");
    assert_eq!(e.code(), tonic::Code::InvalidArgument);
    // The other effects are not built.
    let e = cmd
        .do_effect(pb::DoEffectRequest {
            write: write(&l, "boom", c),
            effect: Some(pb::Effect {
                effect: Some(pb::effect::Effect::Explosion(pb::Explosion::default())),
            }),
        })
        .await
        .expect_err("not built");
    assert_eq!(e.code(), tonic::Code::Unimplemented);

    // Capture returns the entity as DCS read it.
    let cap = cmd
        .capture(pb::CaptureRequest {
            write: write(&l, "capture", c),
            entity_id: b.group_id,
        })
        .await
        .expect("capture")
        .into_inner();
    assert_eq!(
        cap.result.expect("result").outcome,
        pb::Outcome::Applied as i32
    );
    let Some(entity::Kind::Group(g)) = cap.entity.and_then(|e| e.kind) else {
        panic!("a group")
    };
    assert_eq!(g.id, b.group_id);
    let life = g.units[0].life.expect("life");
    assert!((life - 0.1).abs() < 1e-6, "{life}");
    // An entity DCS does not hold cannot be captured.
    let v = cmd
        .put_group(group(&l, "v", c, 900.0, 2, true))
        .await
        .expect("put")
        .into_inner();
    let e = cmd
        .capture(pb::CaptureRequest {
            write: write(&l, "capture-virtual", c),
            entity_id: v.group_id,
        })
        .await
        .expect_err("virtual");
    assert_eq!(e.code(), tonic::Code::FailedPrecondition);
}

async fn outcome(ch: &Channel, key: &str) -> pb::GetOutcomeResponse {
    CommandsServiceClient::new(ch.clone())
        .get_outcome(pb::GetOutcomeRequest {
            idempotency_key: key.into(),
        })
        .await
        .expect("outcome")
        .into_inner()
}

fn outcome_of(r: &Option<pb::CommandResult>) -> i32 {
    r.as_ref().expect("a result").outcome
}

/// A call that returns `pending` settles: `GetOutcome` and a repeat of the
/// call give the final outcome, and a capture gives its entity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_effect_or_query_settles_under_its_key() {
    let (hub, ch) = in_process(
        Config {
            result_wait: Duration::ZERO,
            ..cfg()
        },
        fake_config(),
    )
    .await
    .expect("server");
    fake(&hub, |s| {
        let mut st = nanoprobe_sidecar::stock::Stock::default();
        st.set(Item::Weapon("AIM-9X".into()), 4.0);
        s.add_airbase("Batumi", 1, st);
    });
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let g = cmd
        .put_group(group(&l, "g", c, 0.0, 2, false))
        .await
        .expect("put")
        .into_inner();
    until(&ch, c, g.group_id, live).await;
    let unit = g.unit_ids[0];

    let damage = || pb::DoEffectRequest {
        write: write(&l, "damage", c),
        effect: Some(pb::Effect {
            effect: Some(pb::effect::Effect::Damage(pb::Damage {
                unit_id: unit,
                life_percent: 10.0,
            })),
        }),
    };
    let first = cmd.do_effect(damage()).await.expect("damage").into_inner();
    assert_eq!(outcome_of(&first.result), pb::Outcome::Pending as i32);
    let settled = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let o = outcome(&ch, "damage").await;
            if outcome_of(&o.result) != pb::Outcome::Pending as i32 {
                return o;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("settles");
    assert_eq!(outcome_of(&settled.result), pb::Outcome::Applied as i32);
    assert_eq!(settled.campaign_id, c);
    // A repeat returns the current outcome, and sends nothing.
    let again = cmd.do_effect(damage()).await.expect("repeat").into_inner();
    assert_eq!(outcome_of(&again.result), pb::Outcome::Applied as i32);

    let adjust = || pb::AdjustWarehouseStockRequest {
        write: write(&l, "adjust", c),
        theater: "Caucasus".into(),
        airbase: "Batumi".into(),
        weapon: [("AIM-9X".to_owned(), 1.0)].into_iter().collect(),
        aircraft: Default::default(),
        liquids: Default::default(),
    };
    let first = cmd
        .adjust_warehouse_stock(adjust())
        .await
        .expect("adjust")
        .into_inner();
    assert_eq!(outcome_of(&first.result), pb::Outcome::Pending as i32);
    tokio::time::timeout(Duration::from_secs(20), async {
        while outcome_of(&outcome(&ch, "adjust").await.result) == pb::Outcome::Pending as i32 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("settles");
    let again = cmd
        .adjust_warehouse_stock(adjust())
        .await
        .expect("repeat")
        .into_inner();
    assert_eq!(outcome_of(&again.result), pb::Outcome::Applied as i32);
    assert_eq!(
        fake(&hub, |s| s
            .airbase_stock("Batumi")
            .and_then(|k| k.get(&Item::Weapon("AIM-9X".into())))),
        Some(5.0),
        "sent once"
    );

    let first = cmd
        .capture(pb::CaptureRequest {
            write: write(&l, "capture", c),
            entity_id: g.group_id,
        })
        .await
        .expect("capture")
        .into_inner();
    assert_eq!(outcome_of(&first.result), pb::Outcome::Pending as i32);
    assert!(first.entity.is_none());
    let settled = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let o = outcome(&ch, "capture").await;
            if outcome_of(&o.result) != pb::Outcome::Pending as i32 {
                return o;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("settles");
    assert_eq!(outcome_of(&settled.result), pb::Outcome::Applied as i32);
    let Some(entity::Kind::Group(grp)) = settled.entity.and_then(|e| e.kind) else {
        panic!("the captured group")
    };
    assert_eq!(grp.id, g.group_id);
}

/// A pending effect whose result left the reconciler, and that is not in
/// flight, settles as `unknown`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pending_effect_with_no_result_left_settles_as_unknown() {
    use nanoprobe_api::idem::{Done, EffectKind, Pending};
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let settled = {
        let mut g = hub.lock();
        let done = Done {
            method: "DoEffect",
            response: Vec::new(),
            campaign: c,
            pending: Some(Pending {
                cmd: (u64::MAX, u64::MAX),
                kind: EffectKind::Damage,
            }),
        };
        g.idem.update("lost", done.clone());
        nanoprobe_api::hub::settle(&mut g, "lost", done)
    };
    assert!(settled.pending.is_none());
    let r = pb::DoEffectResponse::decode(settled.response.as_slice()).expect("response");
    assert_eq!(outcome_of(&r.result), pb::Outcome::Unknown as i32);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_coalition_filter_matches_what_the_world_reports() {
    let (_hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let g = cmd
        .put_group(group(&l, "g", c, 0.0, 2, true))
        .await
        .expect("put")
        .into_inner();
    until(&ch, c, g.group_id, |x| {
        x.coalition == Coalition::Blue as i32
    })
    .await;
    let ids = |coalition: Coalition| {
        let ch = ch.clone();
        async move {
            let mut s = WorldServiceClient::new(ch)
                .list_world(ListWorldRequest {
                    filter: Some(WorldFilter {
                        campaign_id: c,
                        coalitions: vec![coalition as i32],
                        ..WorldFilter::default()
                    }),
                })
                .await
                .expect("list")
                .into_inner();
            let mut v = Vec::new();
            while let Some(m) = s.message().await.expect("page") {
                for e in m.entities {
                    if let Some(entity::Kind::Group(g)) = e.kind {
                        v.push(g.id);
                    }
                }
            }
            v
        }
    };
    assert_eq!(ids(Coalition::Blue).await, vec![g.group_id]);
    assert!(ids(Coalition::Red).await.is_empty());
}

/// A group that is not in DCS takes its coalition from the country map. A
/// map that arrives after a coalition-filtered watch started sends the group
/// with its new coalition.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_country_answer_reaches_a_coalition_filtered_watch() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let u = CommandsServiceClient::new(ch.clone())
        .put_group(group(&l, "u", c, 900.0, 99, true))
        .await
        .expect("put")
        .into_inner();
    // The agent's answer is in before the watch starts.
    tokio::time::timeout(Duration::from_secs(20), async {
        while hub.lock().campaigns[&c].store.countries().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the country answer");
    let rev = hub.lock().campaigns[&c].store.revision();
    let mut w = WorldServiceClient::new(ch.clone())
        .watch_world(WatchWorldRequest {
            filter: Some(WorldFilter {
                campaign_id: c,
                coalitions: vec![Coalition::Blue as i32],
                ..WorldFilter::default()
            }),
            since_revision: rev,
            rate: pb::RateClass::Normal as i32,
            min_period: None,
        })
        .await
        .expect("watch")
        .into_inner();
    {
        let mut h = hub.lock();
        let store = &mut h.campaigns.get_mut(&c).expect("c").store;
        let mut map = store.countries().cloned().unwrap_or_default();
        map.insert(99, 2);
        store.set_countries(Some(map));
    }
    let seen = tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let m = w.message().await.expect("message").expect("open");
            for ch in m.changes {
                if let Some(world_change::Change::Upsert(pb::Entity {
                    kind: Some(entity::Kind::Group(x)),
                })) = ch.change
                    && x.id == u.group_id
                {
                    return x;
                }
            }
        }
    })
    .await
    .expect("the group with its new coalition, from the map change itself");
    assert_eq!(seen.coalition, Coalition::Blue as i32);
}

/// `SOUND`, `PICTURE` and `PICTURE_STOP` through `DoEffect`: each scope
/// reaches DCS with its target, a group or unit DCS does not hold is refused
/// before it is sent, and bad input is `INVALID_ARGUMENT`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sound_and_picture_effects() {
    use nanoprobe_sidecar::sim::wire::Body;
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let g = cmd
        .put_group(group(&l, "g", c, 0.0, 2, false))
        .await
        .expect("put")
        .into_inner();
    until(&ch, c, g.group_id, live).await;
    let unit = g.unit_ids[0];
    let v = cmd
        .put_group(group(&l, "v", c, 900.0, 2, true))
        .await
        .expect("put")
        .into_inner();

    let applied = |r: pb::DoEffectResponse| {
        assert_eq!(
            r.result.expect("result").outcome,
            pb::Outcome::Applied as i32
        );
    };
    let effect = |key: &str, e: pb::effect::Effect| pb::DoEffectRequest {
        write: write(&l, key, c),
        effect: Some(pb::Effect { effect: Some(e) }),
    };
    let sound = |scope: pb::EffectScope, target: u64, file: &str| {
        pb::effect::Effect::Sound(pb::Sound {
            scope: scope as i32,
            target,
            file: file.into(),
        })
    };
    use pb::EffectScope as Scope;
    for (key, scope, target) in [
        ("s-all", Scope::All, 0),
        ("s-side", Scope::Coalition, 2),
        ("s-country", Scope::Country, 80),
        ("s-group", Scope::Group, g.group_id),
        ("s-unit", Scope::Unit, unit),
    ] {
        applied(
            cmd.do_effect(effect(key, sound(scope, target, "gong.ogg")))
                .await
                .unwrap_or_else(|e| panic!("{key}: {e}"))
                .into_inner(),
        );
    }
    let sounds = fake(&hub, |s| s.ledger().sounds.clone());
    assert_eq!(
        sounds,
        vec![
            (1, 0, "gong.ogg".to_owned()),
            (2, 2, "gong.ogg".to_owned()),
            (5, 80, "gong.ogg".to_owned()),
            (3, g.group_id, "gong.ogg".to_owned()),
            (4, unit, "gong.ogg".to_owned()),
        ]
    );

    let picture = |scope: Scope, target: u64, file: &str| pb::Picture {
        scope: scope as i32,
        target,
        file: file.into(),
        seconds: 15.0,
        clear_view: true,
        size: 50.0,
        horizontal: 1,
        vertical: 2,
        size_units: 3,
    };
    applied(
        cmd.do_effect(effect(
            "p-unit",
            pb::effect::Effect::Picture(picture(Scope::Unit, unit, "Key_1")),
        ))
        .await
        .expect("picture")
        .into_inner(),
    );
    applied(
        cmd.do_effect(effect(
            "p-stop",
            pb::effect::Effect::PictureStop(pb::PictureStop {}),
        ))
        .await
        .expect("stop")
        .into_inner(),
    );
    let (pictures, stops) = fake(&hub, |s| {
        (s.ledger().pictures.clone(), s.ledger().picture_stops)
    });
    assert_eq!(stops, 1);
    assert_eq!(
        pictures,
        vec![Body::Picture {
            scope: 4,
            target: unit,
            clear_view: 1,
            seconds: 15.0,
            size: 50.0,
            horizontal: 1,
            vertical: 2,
            size_units: 3,
            file: "Key_1".into(),
        }]
    );

    // A group DCS does not hold (virtual), an unknown entity, and bad input.
    let precondition = [
        ("v-sound", sound(Scope::Group, v.group_id, "x.ogg")),
        (
            "v-picture",
            pb::effect::Effect::Picture(picture(Scope::Group, v.group_id, "k")),
        ),
    ];
    for (key, e) in precondition {
        let e = cmd
            .do_effect(effect(key, e))
            .await
            .expect_err("DCS holds no object");
        assert_eq!(e.code(), tonic::Code::FailedPrecondition, "{key}");
    }
    let e = cmd
        .do_effect(effect("nobody", sound(Scope::Unit, 987_654, "x.ogg")))
        .await
        .expect_err("unknown unit");
    assert_eq!(e.code(), tonic::Code::NotFound);
    let mut long = picture(Scope::All, 0, "k");
    long.file = "a".repeat(256);
    let mut quote = picture(Scope::All, 0, "k");
    quote.file = "a\"b".into();
    let mut negative = picture(Scope::All, 0, "k");
    negative.seconds = -1.0;
    let mut huge = picture(Scope::All, 0, "k");
    huge.size = f64::INFINITY;
    let mut zero = picture(Scope::All, 0, "k");
    zero.size = 0.0;
    let mut negative_zero = picture(Scope::All, 0, "k");
    negative_zero.size = -0.0;
    let mut tiny = picture(Scope::All, 0, "k");
    tiny.size = 1e-5;
    let bad = [
        ("tiny-size", pb::effect::Effect::Picture(tiny)),
        ("unspecified", sound(Scope::Unspecified, 0, "x.ogg")),
        ("big-side", sound(Scope::Coalition, 1 << 32, "x.ogg")),
        ("no-file", sound(Scope::All, 0, "")),
        ("long", pb::effect::Effect::Picture(long)),
        ("quote", pb::effect::Effect::Picture(quote)),
        ("negative", pb::effect::Effect::Picture(negative)),
        ("huge", pb::effect::Effect::Picture(huge)),
        ("zero-size", pb::effect::Effect::Picture(zero)),
        (
            "negative-zero-size",
            pb::effect::Effect::Picture(negative_zero),
        ),
    ];
    for (key, e) in bad {
        let e = cmd.do_effect(effect(key, e)).await.expect_err("invalid");
        assert_eq!(e.code(), tonic::Code::InvalidArgument, "{key}");
    }
    assert_eq!(fake(&hub, |s| s.ledger().sounds.len()), 5);
}

async fn route_events(ch: &Channel, c: u64) -> Vec<pb::RouteEvent> {
    let r = EventsServiceClient::new(ch.clone())
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
    r.events
        .into_iter()
        .filter_map(|e| match e.body {
            Some(pb::event::Body::Route(r)) => Some(r),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_route_makes_one_event_per_waypoint_and_one_completion_across_a_restart() {
    let (hub, ch) = in_process(cfg(), fake_config()).await.expect("server");
    let l = lease(&ch).await;
    let c = campaign(&ch, &l).await;
    let mut cmd = CommandsServiceClient::new(ch.clone());
    let g = cmd
        .put_group(group(&l, "g", c, 0.0, 2, false))
        .await
        .expect("put")
        .into_inner();
    until(&ch, c, g.group_id, live).await;
    let wp = |north: f64, east: f64| pb::Waypoint {
        position: Some(pb::Position {
            theater: "Caucasus".into(),
            north,
            east,
            alt: 0.0,
        }),
        speed: 5.0,
        ..pb::Waypoint::default()
    };
    let route = |k: &str, waypoints: Vec<pb::Waypoint>| pb::SetOrderRequest {
        write: write(&l, k, c),
        group_id: g.group_id,
        order: Some(Order {
            intent: Some(order::Intent::Route(pb::RouteOrder { waypoints })),
        }),
    };
    let first = cmd
        .set_order(route(
            "r1",
            vec![wp(300.0, 0.0), wp(300.0, 300.0), wp(600.0, 300.0)],
        ))
        .await
        .expect("route")
        .into_inner();

    // Restart the sidecar once the first waypoint is in the log.
    tokio::time::timeout(Duration::from_secs(20), async {
        while route_events(&ch, c).await.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a first waypoint");
    {
        let cfg = hub.cfg.clone();
        hub.lock().restart(&cfg);
    }
    let done = |ev: &[pb::RouteEvent]| {
        ev.iter()
            .any(|e| e.kind == pb::RouteEventKind::RouteComplete as i32)
    };
    tokio::time::timeout(Duration::from_secs(20), async {
        while !done(&route_events(&ch, c).await) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the route completes");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ev = route_events(&ch, c).await;
    let shape: Vec<(i32, Option<u32>)> = ev.iter().map(|e| (e.kind, e.waypoint_index)).collect();
    let reached = pb::RouteEventKind::WaypointReached as i32;
    let complete = pb::RouteEventKind::RouteComplete as i32;
    assert_eq!(
        shape,
        vec![
            (reached, Some(0)),
            (reached, Some(1)),
            (reached, Some(2)),
            (complete, None)
        ]
    );
    assert!(ev.iter().all(|e| e.group_id == g.group_id
        && e.order_revision == first.order_revision
        && e.observed));

    // A new order revision completes again, from index 0.
    let second = cmd
        .set_order(route("r2", vec![wp(300.0, 300.0)]))
        .await
        .expect("route")
        .into_inner();
    tokio::time::timeout(Duration::from_secs(20), async {
        while route_events(&ch, c).await.len() < 6 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second route");
    let ev = route_events(&ch, c).await;
    let tail: Vec<(u64, i32, Option<u32>)> = ev[4..]
        .iter()
        .map(|e| (e.order_revision, e.kind, e.waypoint_index))
        .collect();
    assert_eq!(
        tail,
        vec![
            (second.order_revision, reached, Some(0)),
            (second.order_revision, complete, None)
        ]
    );
}
