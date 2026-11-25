//! Revising a static through the API (spec/world.md, "Revising a static"):
//! `PutStatic` with a `static_id` changes the spec at one new revision, the
//! reconciler spawns the static again under its name, a heliport group's
//! further units keep their IDs by index, and every refusal writes nothing.

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
use nanoprobe_sidecar::ids::{self, EntityId};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::sim::wire::lv;
use nanoprobe_sim_fake::FakeSim;
use nanoprobe_transport::codec::Lv;
use tonic::Code;
use tonic::transport::Channel;

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

fn category(c: &str) -> Option<prost_types::Struct> {
    Some(prost_types::Struct {
        fields: [("category".to_owned(), text(c))].into(),
    })
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
        r#type: "FARP".into(),
        position: at("Caucasus", north),
        heading: 0.25,
        extra: category("Heliports"),
    }
}

fn farp(n: usize) -> pb::StaticSpec {
    pb::StaticSpec {
        label: "farps".into(),
        country: 2,
        r#type: "FARP".into(),
        position: at("Caucasus", 0.0),
        heading: 0.5,
        extra: category("Heliports"),
        dead: false,
        further_units: (1..n).map(|i| further(i as f64 * 200.0)).collect(),
    }
}

fn bunker() -> pb::StaticSpec {
    pb::StaticSpec {
        label: "bunker".into(),
        country: 2,
        r#type: "Sandbox".into(),
        position: at("Caucasus", -1_000.0),
        heading: 0.0,
        extra: category("Fortifications"),
        dead: false,
        further_units: Vec::new(),
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
            name: "revise".into(),
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

    fn write(&mut self) -> Option<Write> {
        self.n += 1;
        Some(Write {
            lease: Some(self.lease.clone()),
            idempotency: Some(Idempotency {
                key: format!("w-{}", self.n),
            }),
            campaign_id: self.campaign,
        })
    }

    async fn put(
        &mut self,
        static_id: u64,
        spec: pb::StaticSpec,
    ) -> Result<pb::PutStaticResponse, tonic::Status> {
        let write = self.write();
        CommandsServiceClient::new(self.ch.clone())
            .put_static(pb::PutStaticRequest {
                write,
                static_id,
                spec: Some(spec),
            })
            .await
            .map(|r| r.into_inner())
    }

    async fn remove(&mut self, id: u64) {
        let write = self.write();
        CommandsServiceClient::new(self.ch.clone())
            .remove_entity(pb::RemoveEntityRequest {
                write,
                entity_id: id,
            })
            .await
            .expect("remove");
    }

    fn with_sim<T>(&self, f: impl FnOnce(&mut FakeSim) -> T) -> T {
        let mut inner = self.hub.lock();
        let sim = inner
            .backend
            .as_any()
            .and_then(|b| b.downcast_mut::<FakeSim>())
            .expect("the fake");
        f(sim)
    }

    /// The last `static` table the fake DCS was sent under the static's name.
    fn table(&self, id: u64) -> Option<Lv> {
        let name = ids::name(
            ids::NameKind::Static,
            ids::CampaignId(self.campaign),
            EntityId(id),
        );
        self.with_sim(|s| s.ledger().static_tables.get(&name).cloned())
    }

    /// The spec revisions of the spawns of the entity that reached DCS.
    fn spawned_revs(&self, id: u64) -> Vec<u64> {
        self.with_sim(|s| {
            s.ledger()
                .spawns
                .iter()
                .filter(|sp| sp.2 == id)
                .map(|sp| sp.3)
                .collect()
        })
    }

    fn state(&self, id: u64) -> (State, u64) {
        let g = self.hub.lock();
        let grp = g.campaigns[&self.campaign]
            .store
            .group(EntityId(id))
            .expect("held")
            .clone();
        (grp.state, grp.spec_rev)
    }

    fn further_ids(&self, id: u64) -> Vec<u64> {
        let g = self.hub.lock();
        g.campaigns[&self.campaign]
            .store
            .group(EntityId(id))
            .expect("held")
            .further
            .iter()
            .map(|f| f.id.0)
            .collect()
    }

    fn revision(&self) -> u64 {
        self.hub.lock().campaigns[&self.campaign].store.revision()
    }

    /// Wait until the static is `live` with its spawn at `rev` in DCS.
    async fn live_at(&self, id: u64, rev: u64) {
        for _ in 0..1000 {
            if self.state(id).0 == State::Live && self.spawned_revs(id).contains(&rev) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "static {id} not live at revision {rev}: {:?}, spawned {:?}",
            self.state(id),
            self.spawned_revs(id)
        );
    }
}

fn refused(e: &tonic::Status, code: Code) -> String {
    assert_eq!(e.code(), code, "{e}");
    errors::detail(e).expect("detail").field
}

/// The unit names of a heliport group table, as entity IDs.
fn unit_ids(table: &Lv) -> Vec<u64> {
    lv::items(lv::get(table, "units").expect("units"))
        .iter()
        .map(|u| {
            ids::parse(&lv::str_at(u, "name").expect("name"))
                .expect("a platform name")
                .entity
                .0
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revision_moves_turns_and_retypes_a_static_in_place() {
    let mut rig = Rig::new().await;
    let made = rig.put(0, bunker()).await.expect("create");
    let id = made.static_id;
    rig.live_at(id, 1).await;
    let mut s = bunker();
    s.position = at("Caucasus", -950.0);
    s.heading = 1.5;
    s.r#type = "Sandbox2".into();
    let r = rig.put(id, s).await.expect("revise");
    assert_eq!((r.static_id, r.spec_revision), (id, 2));
    rig.live_at(id, 2).await;
    let t = rig.table(id).expect("table");
    assert_eq!(lv::num_at(&t, "x"), Some(-950.0));
    assert_eq!(lv::num_at(&t, "heading"), Some(1.5));
    assert_eq!(lv::str_at(&t, "type").as_deref(), Some("Sandbox2"));
    assert_eq!(rig.spawned_revs(id), vec![1, 2], "one spawn per revision");
    // DCS holds one object for it: the old one was replaced.
    let objects = rig.with_sim(|s| s.objects().iter().filter(|o| o.2 == id).count());
    assert_eq!(objects, 1);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.state(id), (State::Live, 2));
    assert_eq!(rig.spawned_revs(id), vec![1, 2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_equal_spec_changes_nothing_and_a_label_moves_no_spec_revision() {
    let mut rig = Rig::new().await;
    let id = rig.put(0, bunker()).await.expect("create").static_id;
    rig.live_at(id, 1).await;
    let rev = rig.revision();
    let r = rig.put(id, bunker()).await.expect("equal");
    assert_eq!((r.spec_revision, r.revision), (1, rev));
    let mut s = bunker();
    s.label = "renamed".into();
    let r = rig.put(id, s).await.expect("label");
    assert_eq!(r.spec_revision, 1);
    assert!(r.revision > rev);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.spawned_revs(id), vec![1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_refusal_of_a_revision_names_its_field_and_writes_nothing() {
    let mut rig = Rig::new().await;
    let plain = rig.put(0, bunker()).await.expect("plain").static_id;
    let heli = rig.put(0, farp(2)).await.expect("farp").static_id;
    rig.live_at(plain, 1).await;
    rig.live_at(heli, 1).await;
    let rev = rig.revision();
    let mut cases: Vec<(&str, u64, pb::StaticSpec, &str)> = Vec::new();
    let mut s = bunker();
    s.country = 0;
    cases.push(("country", plain, s, "spec.country"));
    let mut s = bunker();
    s.position = at("Syria", 0.0);
    cases.push(("theater", plain, s, "spec.position.theater"));
    let mut s = bunker();
    s.extra = category("Heliports");
    cases.push(("into Heliports", plain, s, "spec.extra.category"));
    let mut s = farp(1);
    s.extra = category("Fortifications");
    cases.push(("out of Heliports", heli, s, "spec.extra.category"));
    let mut s = bunker();
    s.dead = true;
    cases.push(("dead", plain, s, "spec.dead"));
    let mut s = bunker();
    s.r#type = String::new();
    cases.push(("checked as a create", plain, s, "spec.type"));
    for (what, id, spec, field) in cases {
        let e = rig.put(id, spec).await.expect_err(what);
        assert_eq!(refused(&e, Code::InvalidArgument), field, "{what}");
    }
    // Not held: an unknown ID, a group's ID, a removed static.
    let e = rig.put(9_999, bunker()).await.expect_err("unknown");
    assert_eq!(e.code(), Code::NotFound, "{e}");
    assert_eq!(rig.revision(), rev, "no refusal wrote anything");
    assert_eq!(rig.state(plain), (State::Live, 1));
    assert_eq!(rig.state(heli), (State::Live, 1));
    let write = rig.write();
    let group = CommandsServiceClient::new(rig.ch.clone())
        .put_group(pb::PutGroupRequest {
            write,
            group_id: 0,
            spec: Some(pb::GroupSpec {
                label: "g".into(),
                country: 2,
                category: pb::GroupCategory::Ground as i32,
                units: vec![pb::UnitSpec {
                    r#type: "M-113".into(),
                    position: at("Caucasus", 3_000.0),
                    ..pb::UnitSpec::default()
                }],
                ..pb::GroupSpec::default()
            }),
            order: None,
        })
        .await
        .expect("group")
        .into_inner()
        .group_id;
    let e = rig.put(group, bunker()).await.expect_err("a group's ID");
    assert_eq!(e.code(), Code::NotFound, "{e}");
    rig.remove(plain).await;
    let e = rig.put(plain, bunker()).await.expect_err("removed");
    assert_eq!(e.code(), Code::NotFound, "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dead_statics_spec_is_fixed_even_when_equal() {
    let mut rig = Rig::new().await;
    let mut s = bunker();
    s.dead = true;
    let id = rig.put(0, s).await.expect("created dead").static_id;
    let rev = rig.state(id).1;
    // The equal spec, as created.
    let mut equal = bunker();
    equal.dead = true;
    let e = rig.put(id, equal).await.expect_err("equal, dead");
    // A dead static's refusal comes before every check of the spec (D278, R5).
    assert_eq!(refused(&e, Code::FailedPrecondition), "static_id");
    let mut broken = bunker();
    broken.r#type = String::new();
    let e = rig.put(id, broken).await.expect_err("dead, bad spec");
    assert_eq!(refused(&e, Code::FailedPrecondition), "static_id");
    let e = rig.put(id, bunker()).await.expect_err("dead static");
    assert_eq!(refused(&e, Code::FailedPrecondition), "static_id");
    let mut moved = bunker();
    moved.position = at("Caucasus", 0.0);
    let e = rig.put(id, moved).await.expect_err("dead static moved");
    assert_eq!(refused(&e, Code::FailedPrecondition), "static_id");
    assert_eq!(rig.state(id), (State::Dead, rev));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_heliport_group_grows_and_shrinks_keeping_further_ids_by_index() {
    let mut rig = Rig::new().await;
    let id = rig.put(0, farp(1)).await.expect("single").static_id;
    rig.live_at(id, 1).await;
    // A plain static after it takes the next ID, so new further IDs follow it.
    let other = rig.put(0, bunker()).await.expect("other").static_id;
    assert_eq!(other, id + 1);

    // 1 to 4: three new IDs, after every ID taken so far.
    let r = rig.put(id, farp(4)).await.expect("grow");
    assert_eq!(r.spec_revision, 2);
    rig.live_at(id, 2).await;
    let four = rig.further_ids(id);
    assert_eq!(four, vec![other + 1, other + 2, other + 3]);
    let t = rig.table(id).expect("table");
    assert_eq!(unit_ids(&t), [vec![id], four.clone()].concat());

    // 4 to 2: the first further unit keeps its ID.
    let r = rig.put(id, farp(2)).await.expect("shrink");
    assert_eq!(r.spec_revision, 3);
    rig.live_at(id, 3).await;
    assert_eq!(rig.further_ids(id), vec![four[0]]);
    assert_eq!(unit_ids(&rig.table(id).expect("table")), vec![id, four[0]]);

    // A heliport's type does not change in place, the static's own or a
    // kept further unit's; a new further unit may be another type.
    let mut s = farp(2);
    s.r#type = "SINGLE_HELIPAD".into();
    let e = rig.put(id, s).await.expect_err("retyped");
    assert_eq!(refused(&e, Code::FailedPrecondition), "spec.type");
    let mut s = farp(2);
    s.further_units[0].r#type = "SINGLE_HELIPAD".into();
    let e = rig.put(id, s).await.expect_err("further retyped");
    assert_eq!(
        refused(&e, Code::FailedPrecondition),
        "spec.further_units[0].type"
    );
    assert_eq!(rig.state(id).1, 3, "no refusal wrote a revision");
    let mut s = farp(3);
    s.further_units[1].r#type = "SINGLE_HELIPAD".into();
    let r = rig.put(id, s).await.expect("a new helipad unit");
    assert_eq!(r.spec_revision, 4);
    rig.live_at(id, 4).await;
    assert_eq!(rig.further_ids(id).len(), 2);

    // To 1.
    let r = rig.put(id, farp(1)).await.expect("to one");
    assert_eq!(r.spec_revision, 5);
    rig.live_at(id, 5).await;
    assert!(rig.further_ids(id).is_empty());
    let t = rig.table(id).expect("table");
    assert_eq!(
        unit_ids(&t),
        vec![id],
        "a single heliport is a one-unit group"
    );

    // Every ID dropped still stands for the static in this generation.
    let g = rig.hub.lock();
    let store = &g.campaigns[&rig.campaign].store;
    for f in &four {
        assert_eq!(store.further_owner(EntityId(*f)), Some(EntityId(id)));
    }
    // The helipad unit took the next ID, and no other entity took one.
    assert_eq!(store.further_owner(EntityId(other + 4)), Some(EntityId(id)));
    assert_eq!(store.next_entity(), other + 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revision_out_of_ids_writes_nothing_not_even_the_label() {
    let mut rig = Rig::new().await;
    let id = rig.put(0, farp(1)).await.expect("single").static_id;
    rig.live_at(id, 1).await;
    // One ID left; a group of three needs two.
    {
        let mut g = rig.hub.lock();
        let store = &mut g.campaigns.get_mut(&rig.campaign).expect("c").store;
        store.reserve_id(EntityId(nanoprobe_sidecar::ids::MAX_ID - 1));
        assert_eq!(store.ids_left(), 1);
    }
    let rev = rig.revision();
    let mut s = farp(3);
    s.label = "renamed".into();
    let e = rig.put(id, s).await.expect_err("out of IDs");
    assert_eq!(e.code(), Code::Internal, "{e}");
    assert_eq!(rig.revision(), rev, "nothing written");
    let g = rig.hub.lock();
    let grp = g.campaigns[&rig.campaign]
        .store
        .group(EntityId(id))
        .expect("held")
        .clone();
    assert_eq!((grp.label.as_str(), grp.spec_rev), ("farps", 1));
    assert!(grp.further.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grown_group_is_bounded_as_a_created_one() {
    let mut rig = Rig::new().await;
    let id = rig.put(0, farp(1)).await.expect("single").static_id;
    let mut s = farp(3);
    s.further_units[1].extra = Some(prost_types::Struct {
        fields: [
            ("category".to_owned(), text("Heliports")),
            ("pad".to_owned(), text(&"x".repeat(1024 * 1024 / 8))),
        ]
        .into(),
    });
    let e = rig.put(id, s).await.expect_err("too large");
    assert_eq!(refused(&e, Code::InvalidArgument), "spec");
    assert_eq!(rig.state(id).1, 1, "no new spec revision");
    assert!(rig.further_ids(id).is_empty());
}
