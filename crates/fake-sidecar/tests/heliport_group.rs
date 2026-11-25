//! Heliport groups through the API (spec/world.md, "Heliport groups"):
//! `PutStatic` with further units sends one `STATIC_SPAWN` whose table is
//! the group, every refusal names its field, and the size bound counts each
//! unit.

use std::time::Duration;

use nanoprobe_api::config::Config;
use nanoprobe_api::hub::Hub;
use nanoprobe_fake_sidecar::in_process;
use nanoprobe_proto::errors;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::session_service_client::SessionServiceClient;
use nanoprobe_proto::v1alpha1::{
    self as pb, AcquireLeaseRequest, CreateCampaignRequest, Idempotency, Lease, Write,
};
use nanoprobe_sidecar::ids;
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sim_fake::FakeSim;
use tonic::Code;
use tonic::transport::Channel;

/// The agent's command limit the fake reports: an eighth of the default ring.
const LIMIT: usize = 1024 * 1024 / 8;

fn cfg() -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("addr"),
        step: Duration::from_millis(2),
        ..Config::default()
    }
}

fn text(s: &str) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::StringValue(s.into())),
    }
}

fn number(n: f64) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::NumberValue(n)),
    }
}

fn boolean(b: bool) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::BoolValue(b)),
    }
}

fn extra(pairs: Vec<(&str, prost_types::Value)>) -> Option<prost_types::Struct> {
    Some(prost_types::Struct {
        fields: pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
    })
}

fn heliport() -> Option<prost_types::Struct> {
    extra(vec![("category", text("Heliports"))])
}

fn at(theater: &str, north: f64) -> Option<pb::Position> {
    Some(pb::Position {
        theater: theater.into(),
        north,
        east: 100.0,
        alt: 0.0,
    })
}

fn further(north: f64) -> pb::StaticUnit {
    pb::StaticUnit {
        r#type: "Invisible FARP".into(),
        position: at("Caucasus", north),
        heading: 0.25,
        extra: heliport(),
    }
}

fn group(further_units: Vec<pb::StaticUnit>) -> pb::StaticSpec {
    pb::StaticSpec {
        label: "farps".into(),
        country: 2,
        r#type: "FARP".into(),
        position: at("Caucasus", 0.0),
        heading: 0.5,
        extra: heliport(),
        dead: false,
        further_units,
    }
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

async fn create(ch: &Channel, l: &Lease) -> u64 {
    CampaignServiceClient::new(ch.clone())
        .create_campaign(CreateCampaignRequest {
            lease: Some(l.clone()),
            name: "farps".into(),
            theaters: vec!["Caucasus".into(), "Syria".into()],
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

struct Rig {
    hub: std::sync::Arc<Hub>,
    ch: Channel,
    lease: Lease,
    campaign: u64,
    n: u64,
}

impl Rig {
    async fn new() -> Rig {
        let (hub, ch) = in_process(cfg(), nanoprobe_fake_sidecar::fake_config())
            .await
            .expect("server");
        let lease = lease(&ch).await;
        let campaign = create(&ch, &lease).await;
        Rig {
            hub,
            ch,
            lease,
            campaign,
            n: 0,
        }
    }

    async fn put(&mut self, spec: pb::StaticSpec) -> Result<pb::PutStaticResponse, tonic::Status> {
        self.n += 1;
        CommandsServiceClient::new(self.ch.clone())
            .put_static(pb::PutStaticRequest {
                write: Some(Write {
                    lease: Some(self.lease.clone()),
                    idempotency: Some(Idempotency {
                        key: format!("put-{}", self.n),
                    }),
                    campaign_id: self.campaign,
                }),
                static_id: 0,
                spec: Some(spec),
            })
            .await
            .map(|r| r.into_inner())
    }

    /// The `static` tables the fake DCS was sent, by name.
    fn tables(&self) -> Vec<(String, nanoprobe_transport::codec::Lv)> {
        let mut inner = self.hub.lock();
        let sim = inner
            .backend
            .as_any()
            .and_then(|b| b.downcast_mut::<FakeSim>())
            .expect("the fake");
        sim.ledger()
            .static_tables
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

/// The field a refusal names, after checking it is `INVALID_FIELD`.
fn refused_field(e: &tonic::Status) -> String {
    assert_eq!(e.code(), Code::InvalidArgument, "{e}");
    errors::detail(e).expect("detail").field
}

async fn until_tables(rig: &Rig, n: usize) {
    for _ in 0..1000 {
        if rig.tables().len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the fake DCS got no STATIC_SPAWN");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_heliport_group_is_one_static_spawn_with_the_group_table() {
    let mut rig = Rig::new().await;
    let made = rig
        .put(group(vec![further(10.0), further(20.0)]))
        .await
        .expect("put");
    until_tables(&rig, 1).await;
    let tables = rig.tables();
    assert_eq!(tables.len(), 1, "one command for the whole group");
    let (name, table) = &tables[0];
    let parsed = ids::parse(name).expect("a platform name");
    assert_eq!(parsed.kind, ids::NameKind::Static);
    assert_eq!(parsed.entity.0, made.static_id);
    let g = ids::parse(&lv::str_at(table, "name").expect("group name")).expect("group name");
    assert_eq!(g.kind, ids::NameKind::Group);
    assert_eq!(g.entity.0, made.static_id);
    let units = lv::items(lv::get(table, "units").expect("units"));
    assert_eq!(units.len(), 3);
    let want = [("FARP", 0), ("Invisible FARP", 1), ("Invisible FARP", 2)];
    for (u, (type_name, offset)) in units.iter().zip(want) {
        let n = ids::parse(&lv::str_at(u, "name").expect("name")).expect("unit name");
        assert_eq!(n.kind, ids::NameKind::Static);
        assert_eq!(n.entity.0, made.static_id + offset);
        assert_eq!(lv::str_at(u, "type").as_deref(), Some(type_name));
        assert_eq!(lv::str_at(u, "category").as_deref(), Some("Heliports"));
    }
    assert_eq!(lv::num_at(units[1], "x"), Some(10.0));
    assert_eq!(lv::num_at(units[2], "x"), Some(20.0));
    // The next static takes the ID after the group's last.
    let next = rig.put(group(Vec::new())).await.expect("plain");
    assert_eq!(next.static_id, made.static_id + 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_refusal_of_a_heliport_group_names_its_field() {
    let mut rig = Rig::new().await;
    let mut cases: Vec<(&str, pb::StaticSpec, &str)> = Vec::new();
    // The first unit is no heliport.
    let mut s = group(vec![further(10.0)]);
    s.extra = extra(vec![("category", text("Fortifications"))]);
    cases.push(("first not a heliport", s, "spec.further_units"));
    // A further unit is none.
    let mut f = further(10.0);
    f.extra = extra(vec![("category", text("Fortifications"))]);
    cases.push((
        "further not a heliport",
        group(vec![further(5.0), f]),
        "spec.further_units[1].extra",
    ));
    let mut f = further(10.0);
    f.extra = None;
    cases.push((
        "further without extra",
        group(vec![f]),
        "spec.further_units[0].extra",
    ));
    let mut s = group(vec![further(10.0)]);
    s.dead = true;
    cases.push(("dead", s, "spec.dead"));
    let mut f = further(10.0);
    f.extra = extra(vec![
        ("category", text("Heliports")),
        ("dead", boolean(true)),
    ]);
    cases.push((
        "dead key in a further extra",
        group(vec![f]),
        "spec.further_units[0].extra",
    ));
    let mut f = further(10.0);
    f.position = at("Syria", 10.0);
    cases.push((
        "another theater",
        group(vec![f]),
        "spec.further_units[0].position.theater",
    ));
    let mut f = further(10.0);
    f.heading = f64::NAN;
    cases.push((
        "heading",
        group(vec![further(5.0), f]),
        "spec.further_units[1].heading",
    ));
    let mut f = further(10.0);
    f.r#type = String::new();
    cases.push(("type", group(vec![f]), "spec.further_units[0].type"));
    let mut f = further(10.0);
    f.position = None;
    cases.push(("position", group(vec![f]), "spec.further_units[0].position"));
    let mut s = group(Vec::new());
    s.extra = extra(vec![
        ("category", text("Heliports")),
        ("units", number(1.0)),
    ]);
    cases.push(("a units key", s, "spec.extra"));
    for (what, spec, field) in cases {
        let e = rig.put(spec).await.expect_err(what);
        assert_eq!(refused_field(&e), field, "{what}");
    }
    assert!(rig.tables().is_empty(), "a refusal sends nothing");
}

/// The extra of a heliport with a padding string of `pad` bytes.
fn padded(pad: usize) -> Option<prost_types::Struct> {
    extra(vec![
        ("category", text("Heliports")),
        ("pad", text(&"x".repeat(pad))),
    ])
}

/// The bytes a refusal says the command would take.
fn bytes_of(e: &tonic::Status) -> usize {
    let m = e.message();
    let after = m.split("take up to ").nth(1).expect("a size message");
    after
        .split(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a number")
}

/// The bytes of the last spawn the fake DCS was sent: its table in the
/// agent's `lv` encoding and its name.
fn sent_bytes(rig: &Rig) -> usize {
    let tables = rig.tables();
    let (name, table) = tables.last().expect("a spawn");
    let mut w = nanoprobe_transport::codec::Writer::new();
    w.lv(table);
    w.len() + name.len()
}

/// A put whose bound reaches the limit is accepted and sends a command
/// within the limit; one more byte of padding is refused with the bound it
/// would take. The bound grows `slope` bytes for each byte of padding.
async fn size_is_counted_exactly(
    rig: &mut Rig,
    slope: usize,
    make: impl Fn(usize) -> pb::StaticSpec,
) {
    // Measure the bound's constant.
    let e = rig.put(make(LIMIT + 1_000)).await.expect_err("over");
    assert_eq!(refused_field(&e), "spec");
    let k = bytes_of(&e) - slope * (LIMIT + 1_000);
    let fit = (LIMIT - k) / slope;
    let sent = rig.tables().len();
    rig.put(make(fit)).await.expect("exactly at the limit");
    until_tables(rig, sent + 1).await;
    let bytes = sent_bytes(rig);
    assert!(
        bytes <= LIMIT,
        "the command takes {bytes} bytes, over the limit {LIMIT}"
    );
    let e = rig.put(make(fit + 1)).await.expect_err("one over");
    assert_eq!(refused_field(&e), "spec");
    assert_eq!(bytes_of(&e), k + slope * (fit + 1));
    assert!(bytes_of(&e) > LIMIT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_heliport_groups_size_is_counted_over_every_unit() {
    let mut rig = Rig::new().await;
    size_is_counted_exactly(&mut rig, 1, |pad| {
        let mut f = further(10.0);
        f.extra = padded(pad);
        group(vec![f])
    })
    .await;
    // The bound counts the first unit's extra twice, as a margin: the group
    // table copies only some of its keys.
    let mut s = group(vec![further(10.0)]);
    s.extra = padded(LIMIT / 2 + 600);
    let e = rig.put(s).await.expect_err("first unit counted twice");
    assert_eq!(refused_field(&e), "spec");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_heliport_is_bounded_as_a_group() {
    let mut rig = Rig::new().await;
    size_is_counted_exactly(&mut rig, 2, |pad| {
        let mut s = group(Vec::new());
        s.extra = padded(pad);
        s
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ten_farps_with_a_realistic_extra_fit_the_default_limit() {
    let mut rig = Rig::new().await;
    let farp = |i: usize| {
        extra(vec![
            ("category", text("Heliports")),
            ("shape_name", text("FARPS")),
            ("heliport_frequency", text("127.5")),
            ("heliport_modulation", number(0.0)),
            ("heliport_callsign_id", number(1.0 + i as f64)),
            ("dynamicSpawn", boolean(true)),
        ])
    };
    let mut spec = group(
        (1..10)
            .map(|i| pb::StaticUnit {
                extra: farp(i),
                ..further(i as f64 * 30.0)
            })
            .collect(),
    );
    spec.extra = farp(0);
    let made = rig.put(spec).await.expect("ten FARPs");
    until_tables(&rig, 1).await;
    let tables = rig.tables();
    let units = lv::items(lv::get(&tables[0].1, "units").expect("units"));
    assert_eq!(units.len(), 10);
    assert_eq!(
        lv::get(&tables[0].1, "dynamicSpawn"),
        Some(&nanoprobe_transport::codec::Lv::Bool(true))
    );
    let _ = made;
}
