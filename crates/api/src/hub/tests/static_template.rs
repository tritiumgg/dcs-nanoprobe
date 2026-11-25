//! `PutFromStaticTemplate` through the services, on the fake sim
//! (spec/world.md, "Static templates").

use nanoprobe_proto::errors;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;

use super::*;
use crate::static_template::FILE_MAX;
use crate::static_template::tests::FILE;

/// What the store holds of a group or static and its units, without the
/// identities.
fn held(hub: &Hub, c: u64, id: u64) -> (String, String, Vec<String>) {
    let g = hub.lock();
    let s = &g.campaigns[&c].store;
    let grp = s.group(EntityId(id)).expect("entity");
    let units = grp
        .members
        .iter()
        .map(|m| {
            let u = s.unit(*m).expect("unit");
            format!(
                "{} {:?} {:?} {:?} at {:?} heading {} dead {}",
                u.type_name, u.label, u.skill, u.extra, u.state.at, u.state.heading, u.dead
            )
        })
        .collect();
    (
        format!(
            "{:?} {} country {} category {} {:?} {:?} order {:?} state {:?} authority {:?}",
            grp.kind,
            grp.theater,
            grp.country,
            grp.category,
            grp.extra,
            grp.components,
            grp.order,
            grp.state,
            grp.authority
        ),
        grp.label.clone(),
        units,
    )
}

/// How many groups and statics a campaign's store holds.
fn entities(hub: &Hub, c: u64) -> usize {
    let g = hub.lock();
    let s = &g.campaigns[&c].store;
    (1..200).filter(|i| s.group(EntityId(*i)).is_some()).count()
}

async fn states(hub: &Arc<Hub>, c: u64, want: &[(u64, State)]) {
    let ok = |hub: &Hub| {
        let g = hub.lock();
        let s = &g.campaigns[&c].store;
        s.owed_load().is_none()
            && want
                .iter()
                .all(|(i, st)| s.group(EntityId(*i)).map(|g| g.state) == Some(*st))
    };
    for _ in 0..1000 {
        if ok(hub) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the entities did not reach {want:?}");
}

/// The places DCS reports, as the World service lists them: each unit of
/// each group, then each static, by ID.
async fn listed(channel: &tonic::transport::Channel, c: u64) -> Vec<(u64, f64, f64, bool)> {
    let mut stream = WorldServiceClient::new(channel.clone())
        .list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: c,
                ..pb::WorldFilter::default()
            }),
        })
        .await
        .expect("list")
        .into_inner();
    let mut out = Vec::new();
    while let Some(m) = stream.message().await.expect("message") {
        for e in m.entities {
            match e.kind {
                Some(pb::entity::Kind::Group(g)) => {
                    for u in g.units {
                        let p = u.position.expect("position");
                        out.push((u.id, p.north, p.east, u.alive));
                    }
                }
                Some(pb::entity::Kind::Static(s)) => {
                    let p = s.position.expect("position");
                    out.push((s.id, p.north, p.east, s.alive));
                }
                _ => {}
            }
        }
    }
    out.sort_by_key(|x| x.0);
    out
}

fn text_value(s: &str) -> prost_types::Value {
    prost_types::Value {
        kind: Some(prost_types::value::Kind::StringValue(s.into())),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_static_template_makes_every_entity_in_one_call_or_none() {
    let (dir, missions, mut cfg) = owed_setup();
    let listed_dir = dir.path().join("templates");
    let unlisted = dir.path().join("elsewhere");
    std::fs::create_dir_all(&listed_dir).expect("listed");
    std::fs::create_dir_all(&unlisted).expect("unlisted");
    cfg.template_dirs = vec![listed_dir.clone()];
    let text = |p: &std::path::Path| p.to_str().expect("utf-8").to_owned();
    let write = |name: &str, body: &[u8]| {
        let p = listed_dir.join(name);
        std::fs::write(&p, body).expect("write");
        text(&p)
    };
    let source = write("layout.stm", FILE.as_bytes());
    std::fs::write(unlisted.join("layout.stm"), FILE).expect("unlisted source");
    let other = write("other.stm", b"templates = {}");
    let code = write("code.stm", b"staticTemplate =\n{\n}\nos.execute('x')\n");
    let mut big = FILE.as_bytes().to_vec();
    big.resize(FILE_MAX + 1, b' ');
    let big = write("big.stm", &big);
    let plane = write(
        "plane.stm",
        FILE.replace("[\"vehicle\"] = ", "[\"plane\"] = ")
            .as_bytes(),
    );
    let syria = write(
        "syria.stm",
        FILE.replace("\"Caucasus\"", "\"Syria\"").as_bytes(),
    );
    let no_theatre = write(
        "no-theatre.stm",
        FILE.replace("[\"theatre\"] = \"Caucasus\",", "").as_bytes(),
    );
    // Every entity passes the reader; the last static is too large for one
    // command to the agent, which is found only when its spec is checked,
    // after the group and the first static are planned.
    let wide = write(
        "wide.stm",
        FILE.replace(
            "[\"category\"] = \"Warehouses\",",
            &format!(
                "[\"category\"] = \"Warehouses\", [\"shape_name\"] = \"{}\",",
                "s".repeat(300_000)
            ),
        )
        .as_bytes(),
    );
    // A static value nested deeper than the store encodes: it passes the
    // size check, and the reader refuses it.
    let deep = write(
        "deep.stm",
        FILE.replace(
            "[\"category\"] = \"Warehouses\",",
            &format!(
                "[\"category\"] = \"Warehouses\", [\"mass\"] = {}1{},",
                "{".repeat(20),
                "}".repeat(20)
            ),
        )
        .as_bytes(),
    );
    // At the store's depth limit: the deepest `mass` the reader passes, and
    // one level more, which it refuses.
    let nested = |n: usize| {
        FILE.replace(
            "[\"category\"] = \"Warehouses\",",
            &format!(
                "[\"category\"] = \"Warehouses\", [\"mass\"] = {}1{},",
                "{".repeat(n),
                "}".repeat(n)
            ),
        )
    };
    let edge = (1..40)
        .find(|n| crate::static_template::read(nested(*n).as_bytes()).is_err())
        .expect("a depth the reader refuses");
    let deepest_ok = write("deepest-ok.stm", nested(edge - 1).as_bytes());
    let edge = write("edge.stm", nested(edge).as_bytes());
    let missing = text(&listed_dir.join("none.stm"));

    let backend = |missions: &crate::real::Missions| {
        Box::new(Folder(
            nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
            missions.clone(),
        ))
    };
    let hub = Hub::open(cfg.clone(), backend(&missions)).expect("hub");
    let (channel, stop, task) = serve_hub(&hub).await;
    let lease = lease_of(&channel).await;
    let mut campaigns = CampaignServiceClient::new(channel.clone());
    let mut commands = CommandsServiceClient::new(channel.clone());
    // Suspended at first: nothing but a call changes its store.
    let a = campaigns
        .create_campaign(create(&lease, "a", "Caucasus", false))
        .await
        .expect("a")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    let from = |key: &str, path: &str| pb::PutFromStaticTemplateRequest {
        write: Some(pb::Write {
            lease: Some(lease.clone()),
            idempotency: Some(pb::Idempotency { key: key.into() }),
            campaign_id: a,
        }),
        path: path.to_owned(),
        anchor: None,
        heading: 0.0,
        country: None,
        label: String::new(),
    };
    let anchor = pb::Position {
        theater: "Caucasus".into(),
        north: -270_000.0,
        east: 650_000.0,
        alt: 3.0,
    };
    let revision = |hub: &Hub| hub.lock().campaigns[&a].store.revision();
    let before = revision(&hub);
    let none = entities(&hub, a);

    // Every refusal names its field and its cause, and changes nothing.
    let refusals: Vec<(pb::PutFromStaticTemplateRequest, &str, &str)> = vec![
        (
            from("r-unlisted", &text(&unlisted.join("layout.stm"))),
            "path",
            "is not a file in a folder that [missions] template_dirs lists",
        ),
        (
            from("r-missing", &missing),
            "path",
            "is not a file in a folder",
        ),
        (
            from("r-relative", "layout.stm"),
            "path",
            "is not an absolute path",
        ),
        (
            from("r-root", &other),
            "path",
            "the file is not a static template file: it does not assign a table to `staticTemplate`",
        ),
        (
            from("r-code", &code),
            "path",
            "the file is not a static template file: line 4:",
        ),
        (
            from("r-big", &big),
            "path",
            "a static template file is at most 1048576 bytes",
        ),
        (
            from("r-plane", &plane),
            "path",
            "holds plane groups; only vehicle groups and statics are built",
        ),
        (
            from("r-syria", &syria),
            "path",
            "the campaign does not use the theater `Syria`",
        ),
        (
            from("r-no-theatre", &no_theatre),
            "path",
            "it has no `theatre`, which a call without an anchor needs",
        ),
        (from("r-wide", &wide), "path", "the agent takes"),
        (from("r-deep", &deep), "path", "`mass` of"),
        (from("r-edge", &edge), "path", "`mass` of"),
        (
            pb::PutFromStaticTemplateRequest {
                anchor: Some(pb::Position {
                    theater: "Syria".into(),
                    ..anchor.clone()
                }),
                ..from("r-anchor-theater", &source)
            },
            "anchor.theater",
            "the campaign does not use the theater `Syria`",
        ),
        (
            pb::PutFromStaticTemplateRequest {
                anchor: Some(pb::Position {
                    north: f64::NAN,
                    ..anchor.clone()
                }),
                ..from("r-north", &source)
            },
            "anchor.north",
            "a number must be finite",
        ),
        (
            pb::PutFromStaticTemplateRequest {
                heading: f64::INFINITY,
                ..from("r-heading", &source)
            },
            "heading",
            "a number must be finite",
        ),
        // The anchor and the heading are refused before the file is.
        (
            pb::PutFromStaticTemplateRequest {
                heading: f64::NAN,
                ..from("r-heading-first", &missing)
            },
            "heading",
            "a number must be finite",
        ),
        // The anchor is refused before the file is.
        (
            pb::PutFromStaticTemplateRequest {
                anchor: Some(pb::Position {
                    theater: "Syria".into(),
                    ..anchor.clone()
                }),
                ..from("r-anchor-first", &code)
            },
            "anchor.theater",
            "the campaign does not use the theater `Syria`",
        ),
    ];
    for (req, field, want) in refusals {
        let key = req
            .write
            .as_ref()
            .expect("write")
            .idempotency
            .clone()
            .expect("key")
            .key;
        let e = commands
            .put_from_static_template(req)
            .await
            .expect_err(&key);
        let detail = errors::detail(&e).expect("detail");
        assert_eq!(e.code(), Code::InvalidArgument, "{key}: {e}");
        assert_eq!(errors::reason(&e), ErrorReason::InvalidField, "{key}");
        assert_eq!(detail.field, field, "{key}: {e}");
        assert!(e.message().contains(want), "{key}: {}", e.message());
        assert_eq!(revision(&hub), before, "{key} changed the store");
        assert_eq!(entities(&hub, a), none, "{key} made an entity");
    }
    for (key, lease) in [
        ("r-no-lease", None),
        (
            "r-wrong-lease",
            Some(pb::Lease {
                lease_id: "not-the-lease".into(),
                ..lease.clone()
            }),
        ),
    ] {
        let e = commands
            .put_from_static_template(pb::PutFromStaticTemplateRequest {
                write: Some(pb::Write {
                    lease,
                    idempotency: Some(pb::Idempotency { key: key.into() }),
                    campaign_id: a,
                }),
                ..from(key, &missing)
            })
            .await
            .expect_err("no lease");
        assert_eq!(errors::reason(&e), ErrorReason::NotController, "{key}: {e}");
    }
    assert_eq!(revision(&hub), before);

    // Made in place: the groups and the statics in the file's order, each
    // with its revision, and the store revision of the call.
    let made = commands
        .put_from_static_template(from("t-1", &source))
        .await
        .expect("made")
        .into_inner();
    assert_eq!(made.groups.len(), 1);
    assert_eq!(made.groups[0].unit_ids.len(), 2);
    assert_eq!(made.statics.len(), 2);
    assert_eq!(made.revision, revision(&hub));
    assert!(made.groups.iter().all(|g| g.spec_revision > 0));
    assert!(made.statics.iter().all(|s| s.spec_revision > 0));
    let ids: Vec<u64> = std::iter::once(made.groups[0].group_id)
        .chain(made.groups[0].unit_ids.iter().copied())
        .chain(made.statics.iter().map(|s| s.static_id))
        .collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "file order: {ids:?}");
    assert_eq!(entities(&hub, a), none + 3);
    {
        let g = hub.lock();
        let s = &g.campaigns[&a].store;
        let bunker = s
            .group(EntityId(made.statics[0].static_id))
            .expect("bunker");
        let depot = s.group(EntityId(made.statics[1].static_id)).expect("depot");
        assert_eq!(
            (bunker.label.as_str(), bunker.state),
            ("Bunker", State::Virtual)
        );
        assert_eq!((depot.label.as_str(), depot.state), ("Depot", State::Dead));
    }

    // The same entities written by hand, through PutGroup and PutStatic.
    let place = |north: f64, east: f64| pb::Position {
        theater: "Caucasus".into(),
        north,
        east,
        alt: 0.0,
    };
    let unit = |ty: &str, north: f64, east: f64, heading: f64, skill: &str| pb::UnitSpec {
        r#type: ty.into(),
        position: Some(place(north, east)),
        heading,
        skill: skill.into(),
        ..pb::UnitSpec::default()
    };
    let group = commands
        .put_group(pb::PutGroupRequest {
            write: from("hand-g", "").write,
            group_id: 0,
            spec: Some(pb::GroupSpec {
                label: "Alpha".into(),
                country: 2,
                category: pb::GroupCategory::Ground as i32,
                units: vec![
                    unit("Type A", -281_000.0, 641_000.0, 1.25, "Average"),
                    unit("Type B", -280_900.0, 641_030.5, 0.0, "High"),
                ],
                ..pb::GroupSpec::default()
            }),
            order: None,
        })
        .await
        .expect("group by hand")
        .into_inner();
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("category".to_string(), text_value("Fortifications"));
    fields.insert("shape_name".to_string(), text_value("shape-a"));
    fields.insert(
        "rate".to_string(),
        prost_types::Value {
            kind: Some(prost_types::value::Kind::NumberValue(100.0)),
        },
    );
    let bunker = commands
        .put_static(pb::PutStaticRequest {
            write: from("hand-s1", "").write,
            static_id: 0,
            spec: Some(pb::StaticSpec {
                label: "Bunker".into(),
                country: 0,
                r#type: "Static A".into(),
                position: Some(place(-281_200.0, 640_800.0)),
                heading: 0.5,
                extra: Some(prost_types::Struct { fields }),
                dead: false,
            }),
        })
        .await
        .expect("bunker by hand")
        .into_inner();
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("category".to_string(), text_value("Warehouses"));
    let depot = commands
        .put_static(pb::PutStaticRequest {
            write: from("hand-s2", "").write,
            static_id: 0,
            spec: Some(pb::StaticSpec {
                label: "Depot".into(),
                country: 0,
                r#type: "Static B".into(),
                position: Some(place(-281_150.0, 640_750.0)),
                heading: 0.0,
                extra: Some(prost_types::Struct { fields }),
                dead: true,
            }),
        })
        .await
        .expect("depot by hand")
        .into_inner();
    for (t, h) in [
        (made.groups[0].group_id, group.group_id),
        (made.statics[0].static_id, bunker.static_id),
        (made.statics[1].static_id, depot.static_id),
    ] {
        assert_eq!(
            held(&hub, a, t),
            held(&hub, a, h),
            "the template's entity is the hand-written one"
        );
    }

    // Anchored and turned a quarter turn, under country 0 and a prefix.
    let turned = commands
        .put_from_static_template(pb::PutFromStaticTemplateRequest {
            anchor: Some(anchor.clone()),
            heading: std::f64::consts::FRAC_PI_2,
            country: Some(0),
            label: "mine".into(),
            ..from("t-2", &source)
        })
        .await
        .expect("turned")
        .into_inner();
    {
        let g = hub.lock();
        let s = &g.campaigns[&a].store;
        let grp = s.group(EntityId(turned.groups[0].group_id)).expect("group");
        assert_eq!((grp.country, grp.label.as_str()), (0, "mine/Alpha"));
        // Type B is 150 m north and 140.25 m east of the origin
        // (-281,050, 640,890.25): turned, 150 m east and 140.25 m south of
        // the anchor.
        let b = s.unit(grp.members[1]).expect("unit");
        assert!(
            (b.state.at.x - (-270_000.0 - 140.25)).abs() < 1e-6,
            "{:?}",
            b.state.at
        );
        assert!(
            (b.state.at.z - (650_000.0 + 150.0)).abs() < 1e-6,
            "{:?}",
            b.state.at
        );
        assert_eq!(b.state.at.y, 3.0);
        assert_eq!(b.state.heading, std::f64::consts::FRAC_PI_2);
        let st = s
            .group(EntityId(turned.statics[1].static_id))
            .expect("static");
        assert_eq!((st.country, st.label.as_str()), (0, "mine/Depot"));
    }

    // With an anchor the file's theatre is not used: a Syria file goes to
    // a Caucasus anchor.
    let moved = commands
        .put_from_static_template(pb::PutFromStaticTemplateRequest {
            anchor: Some(anchor.clone()),
            ..from("t-syria", &syria)
        })
        .await
        .expect("a Syria file at a Caucasus anchor")
        .into_inner();
    {
        let g = hub.lock();
        let s = &g.campaigns[&a].store;
        let grp = s.group(EntityId(moved.groups[0].group_id)).expect("group");
        assert_eq!(grp.theater, "Caucasus");
    }
    // The deepest value the reader passes is stored whole.
    let kept = commands
        .put_from_static_template(from("t-deepest", &deepest_ok))
        .await
        .expect("the deepest value passed")
        .into_inner();
    {
        let g = hub.lock();
        let s = &g.campaigns[&a].store;
        let depot = s.unit(EntityId(kept.statics[1].static_id)).expect("depot");
        assert!(nanoprobe_sidecar::store::check_lv(&depot.extra).is_ok());
        let mass = nanoprobe_transport::codec::Lv::String(b"mass".to_vec());
        let has_mass = matches!(&depot.extra, nanoprobe_transport::codec::Lv::Table(p)
            if p.iter().any(|(k, _)| k == &mass));
        assert!(has_mass, "{:?}", depot.extra);
    }

    // A backup, then the file goes.
    let number = campaigns
        .backup_campaign(pb::BackupCampaignRequest {
            lease: Some(lease.clone()),
            campaign_id: a,
            idempotency: Some(pb::Idempotency { key: "bk".into() }),
        })
        .await
        .expect("backup")
        .into_inner()
        .backup
        .expect("backup")
        .id;
    std::fs::remove_dir_all(&listed_dir).expect("remove the listed folder");
    let settled = revision(&hub);
    let again = commands
        .put_from_static_template(from("t-1", &source))
        .await
        .expect("the repeat")
        .into_inner();
    assert_eq!(again, made);
    let e = commands
        .put_from_static_template(from("t-1-new", &source))
        .await
        .expect_err("the file is gone");
    assert_eq!(errors::detail(&e).expect("detail").field, "path");
    let e = commands
        .put_from_static_template(pb::PutFromStaticTemplateRequest {
            label: "other".into(),
            ..from("t-1", &source)
        })
        .await
        .expect_err("another request under the key");
    assert_eq!(errors::reason(&e), ErrorReason::IdempotencyMismatch, "{e}");
    assert_eq!(revision(&hub), settled);

    // Activated: the entities spawn by the path of every materialization
    // and stand where the file puts them; the destroyed static stays dead.
    let want = [
        (made.groups[0].group_id, State::Live),
        (made.statics[0].static_id, State::Live),
        (made.statics[1].static_id, State::Dead),
    ];
    let activate = |key: &'static str, lease: pb::Lease| pb::ActivateCampaignRequest {
        lease: Some(lease),
        campaign_id: a,
        idempotency: Some(pb::Idempotency { key: key.into() }),
    };
    campaigns
        .activate_campaign(activate("activate", lease.clone()))
        .await
        .expect("activate");
    states(&hub, a, &want).await;
    let check = |seen: &[(u64, f64, f64, bool)]| {
        let of = |id: u64| *seen.iter().find(|x| x.0 == id).expect("listed");
        let near = |got: (u64, f64, f64, bool), n: f64, e: f64| {
            assert!(
                (got.1 - n).abs() < 0.01 && (got.2 - e).abs() < 0.01,
                "{got:?}"
            );
            assert!(got.3, "{got:?} alive");
        };
        near(of(made.groups[0].unit_ids[0]), -281_000.0, 641_000.0);
        near(of(made.groups[0].unit_ids[1]), -280_900.0, 641_030.5);
        near(of(made.statics[0].static_id), -281_200.0, 640_800.0);
    };
    check(&listed(&channel, a).await);

    // One entity ID short of the file's five: refused before the first
    // write, so not even the group is made.
    // The campaign is active, so the driver moves its revision; what the
    // call would have made is looked for instead.
    let count = entities(&hub, a);
    {
        let mut g = hub.lock();
        let s = &mut g.campaigns.get_mut(&a).expect("a").store;
        s.reserve_id(EntityId(nanoprobe_sidecar::ids::MAX_ID - 4));
        assert_eq!(s.ids_left(), 4);
    }
    std::fs::create_dir_all(&listed_dir).expect("listed again");
    std::fs::write(&source, FILE).expect("the file again");
    let e = commands
        .put_from_static_template(from("t-ids", &source))
        .await
        .expect_err("too few IDs");
    assert_eq!(e.code(), Code::Internal, "{e}");
    assert!(e.message().contains("every entity ID"), "{e}");
    {
        let g = hub.lock();
        let s = &g.campaigns[&a].store;
        assert_eq!(s.ids_left(), 4);
        let top = nanoprobe_sidecar::ids::MAX_ID;
        assert!((top - 3..=top).all(|i| s.group(EntityId(i)).is_none()));
    }
    assert_eq!(entities(&hub, a), count);
    // Exactly the file's five left, on a campaign of its own: made, and
    // none left after.
    let b = campaigns
        .create_campaign(create(&lease, "b", "Caucasus", false))
        .await
        .expect("b")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    hub.lock()
        .campaigns
        .get_mut(&b)
        .expect("b")
        .store
        .reserve_id(EntityId(nanoprobe_sidecar::ids::MAX_ID - 5));
    let fit = commands
        .put_from_static_template(pb::PutFromStaticTemplateRequest {
            write: Some(pb::Write {
                lease: Some(lease.clone()),
                idempotency: Some(pb::Idempotency {
                    key: "t-fit".into(),
                }),
                campaign_id: b,
            }),
            ..from("t-fit", &source)
        })
        .await
        .expect("exactly enough IDs")
        .into_inner();
    assert_eq!(fit.statics[1].static_id, nanoprobe_sidecar::ids::MAX_ID);
    assert_eq!(hub.lock().campaigns[&b].store.ids_left(), 0);
    std::fs::remove_dir_all(&listed_dir).expect("remove the listed folder");

    // The sidecar stops, the backup is restored, and a new sidecar spawns
    // the entities from the store, with the file gone.
    let _ = stop.send(());
    task.await.expect("join").expect("serve");
    hub.lock().close_all().expect("close");
    drop(hub);
    nanoprobe_sidecar::campaigns::Campaigns::new(&dir.path().join("campaigns"), 1, 1)
        .expect("campaigns")
        .restore(CampaignId(a), number as u32, 1)
        .expect("restore");
    let hub = Hub::open(cfg, backend(&missions)).expect("hub");
    let (channel, stop, task) = serve_hub(&hub).await;
    let lease = lease_of(&channel).await;
    CampaignServiceClient::new(channel.clone())
        .activate_campaign(activate("activate-restored", lease.clone()))
        .await
        .expect("activate the restored campaign");
    states(&hub, a, &want).await;
    check(&listed(&channel, a).await);
    let again = CommandsServiceClient::new(channel.clone())
        .put_from_static_template(pb::PutFromStaticTemplateRequest {
            write: Some(pb::Write {
                lease: Some(lease.clone()),
                idempotency: Some(pb::Idempotency { key: "t-1".into() }),
                campaign_id: a,
            }),
            ..from("t-1", &source)
        })
        .await
        .expect("the repeat after the restore")
        .into_inner();
    assert_eq!(again.groups, made.groups);
    assert_eq!(again.statics, made.statics);

    let _ = stop.send(());
    task.await.expect("join").expect("serve");
    hub.lock().close_all().expect("close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_helipad_in_a_static_template_is_the_put_static_farp() {
    let (dir, missions, mut cfg) = owed_setup();
    let listed_dir = dir.path().join("templates");
    std::fs::create_dir_all(&listed_dir).expect("listed");
    cfg.template_dirs = vec![listed_dir.clone()];
    let write = |name: &str, body: &str| {
        let p = listed_dir.join(name);
        std::fs::write(&p, body).expect("write");
        p.to_str().expect("utf-8").to_owned()
    };
    let pad = write("pad.stm", crate::static_template::tests::HELIPAD);
    let grass = write(
        "grass.stm",
        &crate::static_template::tests::HELIPAD.replace("\"Heliports\"", "\"GrassAirfields\""),
    );
    let hub = Hub::open(
        cfg.clone(),
        Box::new(Folder(
            nanoprobe_sim_fake::FakeSim::new(nanoprobe_sim_fake::Config::default()),
            missions.clone(),
        )),
    )
    .expect("hub");
    let (channel, stop, task) = serve_hub(&hub).await;
    let lease = lease_of(&channel).await;
    let a = CampaignServiceClient::new(channel.clone())
        .create_campaign(create(&lease, "a", "Caucasus", false))
        .await
        .expect("a")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    let mut commands = CommandsServiceClient::new(channel.clone());
    let write_of = |key: &str| {
        Some(pb::Write {
            lease: Some(lease.clone()),
            idempotency: Some(pb::Idempotency { key: key.into() }),
            campaign_id: a,
        })
    };
    let from = |key: &str, path: &str| pb::PutFromStaticTemplateRequest {
        write: write_of(key),
        path: path.to_owned(),
        anchor: None,
        heading: 0.0,
        country: None,
        label: String::new(),
    };

    // A grass airfield is still refused, and makes nothing.
    let before = hub.lock().campaigns[&a].store.revision();
    let e = commands
        .put_from_static_template(from("grass", &grass))
        .await
        .expect_err("grass");
    assert_eq!(e.code(), Code::InvalidArgument, "{e}");
    assert_eq!(errors::detail(&e).expect("detail").field, "path");
    assert!(
        e.message()
            .contains("is a grass airfield, which is not built"),
        "{}",
        e.message()
    );
    assert_eq!(hub.lock().campaigns[&a].store.revision(), before);

    // The helipad is made, and is the static PutStatic makes from the
    // keys the file passes.
    let made = commands
        .put_from_static_template(from("pad", &pad))
        .await
        .expect("pad")
        .into_inner();
    assert!(made.groups.is_empty());
    assert_eq!(made.statics.len(), 1);
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("category".to_string(), text_value("Heliports"));
    fields.insert("shape_name".to_string(), text_value("FARP"));
    let hand = commands
        .put_static(pb::PutStaticRequest {
            write: write_of("hand"),
            static_id: 0,
            spec: Some(pb::StaticSpec {
                label: "Pad-1".into(),
                country: 2,
                r#type: "SINGLE_HELIPAD".into(),
                position: Some(pb::Position {
                    theater: "Caucasus".into(),
                    north: -356_000.5,
                    east: 619_000.25,
                    alt: 0.0,
                }),
                heading: 0.75,
                extra: Some(prost_types::Struct { fields }),
                dead: false,
            }),
        })
        .await
        .expect("by hand")
        .into_inner();
    assert_eq!(
        held(&hub, a, made.statics[0].static_id),
        held(&hub, a, hand.static_id),
        "the template's helipad is the hand-written FARP"
    );
    let _ = stop.send(());
    task.await.expect("join").expect("serve");
    hub.lock().close_all().expect("close");
}
