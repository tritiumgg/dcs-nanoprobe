//! `PutGroupFromTemplate` through the services, on the fake sim
//! (spec/world.md, "Group templates").

use nanoprobe_proto::errors;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_proto::v1alpha1::mission_service_client::MissionServiceClient;
use nanoprobe_proto::v1alpha1::world_service_client::WorldServiceClient;
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;

use super::*;
use crate::group_template::FILE_MAX;
use crate::group_template::tests::FILE;

const NORTH: f64 = -280_000.0;
const EAST: f64 = 640_000.0;

fn anchor() -> pb::Position {
    pb::Position {
        theater: "Caucasus".into(),
        north: NORTH,
        east: EAST,
        alt: 0.0,
    }
}

/// What the store holds of a group and its units, without the identities.
fn held(hub: &Hub, c: u64, id: u64) -> (String, String, Vec<String>) {
    let g = hub.lock();
    let s = &g.campaigns[&c].store;
    let grp = s.group(EntityId(id)).expect("group");
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
            "{} country {} category {} {:?} {:?} order {:?} options {:?}",
            grp.theater,
            grp.country,
            grp.category,
            grp.extra,
            grp.components,
            grp.order,
            grp.options
        ),
        grp.label.clone(),
        units,
    )
}

async fn live(hub: &Arc<Hub>, c: u64, ids: &[u64]) {
    let ok = |hub: &Hub| {
        let g = hub.lock();
        let s = &g.campaigns[&c].store;
        s.owed_load().is_none()
            && ids
                .iter()
                .all(|i| s.group(EntityId(*i)).map(|g| g.state) == Some(State::Live))
    };
    for _ in 0..1000 {
        if ok(hub) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the groups {ids:?} did not become live");
}

/// The places DCS reports for a group's units, as the World service lists
/// them, with the group's own row.
async fn listed(channel: &tonic::transport::Channel, c: u64, id: u64) -> pb::Group {
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
    while let Some(m) = stream.message().await.expect("message") {
        for e in m.entities {
            if let Some(pb::entity::Kind::Group(g)) = e.kind
                && g.id == id
            {
                return g;
            }
        }
    }
    panic!("group {id} is not listed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_is_made_from_a_template_file_and_the_file_is_not_read_again() {
    let (dir, missions, mut cfg) = owed_setup();
    let listed_dir = dir.path().join("templates");
    let unlisted = dir.path().join("elsewhere");
    std::fs::create_dir_all(listed_dir.join("sub")).expect("listed");
    std::fs::create_dir_all(&unlisted).expect("unlisted");
    cfg.template_dirs = vec![listed_dir.clone()];
    let text = |p: &std::path::Path| p.to_str().expect("utf-8").to_owned();
    let write = |name: &str, body: &[u8]| {
        let p = listed_dir.join(name);
        std::fs::write(&p, body).expect("write");
        text(&p)
    };
    let source = write("templates.lua", FILE.as_bytes());
    std::fs::write(unlisted.join("templates.lua"), FILE).expect("unlisted source");
    let other = write("other.lua", b"mission = { t = 1 }");
    // A valid table, then a call on a line of its own.
    let code = write("code.lua", b"templates =\n{\n}\nos.execute('x')\n");
    // More units than one command to the agent holds.
    let units: String = (0..300)
        .map(|_| "{ dx = 0, dy = 0, name = \"A\", heading = 0 },\n")
        .collect();
    let wide = write(
        "wide.lua",
        format!(
            "templates = {{ t = {{ type = \"vehicle\", country = 1, units = {{\n{units}}} }} }}"
        )
        .as_bytes(),
    );
    let missing = text(&listed_dir.join("none.lua"));
    let mut big = FILE.as_bytes().to_vec();
    big.resize(FILE_MAX + 1, b' ');
    let big = write("big.lua", &big);
    let unit = r#"{ dx = 0, dy = 0, name = "A", heading = 0 }"#;
    let odd = write(
        "odd.lua",
        format!(
            "templates = {{ plane = {{ type = \"plane\", country = 1, units = {{ {unit} }} }},\n\
             stateless = {{ type = \"vehicle\", units = {{ {unit} }} }},\n\
             empty = {{ type = \"vehicle\", country = 1, units = {{}} }},\n\
             typeless = {{ type = \"vehicle\", country = 1, units = {{ {{ dx = 0, dy = 0, heading = 0 }} }} }},\n\
             far = {{ type = \"vehicle\", country = 1, units = {{ {{ dx = 1e308, dy = 0, name = \"A\", heading = 0 }} }} }},\n\
             nan = {{ type = \"vehicle\", country = 1, units = {{ {{ dx = \"0\", dy = 0, name = \"A\", heading = 0 }} }} }} }}"
        )
        .as_bytes(),
    );

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
    let mut mission = MissionServiceClient::new(channel.clone());
    // Suspended at first: nothing but a call changes its store.
    let a = campaigns
        .create_campaign(create(&lease, "a", "Caucasus", false))
        .await
        .expect("a")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    let from = |key: &str, path: &str, template: &str| pb::PutGroupFromTemplateRequest {
        write: Some(pb::Write {
            lease: Some(lease.clone()),
            idempotency: Some(pb::Idempotency { key: key.into() }),
            campaign_id: a,
        }),
        path: path.to_owned(),
        template: template.into(),
        anchor: Some(anchor()),
        heading: 0.0,
        country: None,
        label: String::new(),
        order: None,
    };
    let revision = |hub: &Hub| hub.lock().campaigns[&a].store.revision();
    let before = revision(&hub);

    // Every refusal names its field and its cause, and changes nothing.
    let base = |key: &str| from(key, &source, "Test Battery");
    let refusals: Vec<(pb::PutGroupFromTemplateRequest, &str, &str)> = vec![
        (
            from(
                "r-unlisted",
                &text(&unlisted.join("templates.lua")),
                "Test Battery",
            ),
            "path",
            "is not a file in a folder that [missions] template_dirs lists",
        ),
        (
            from(
                "r-dots",
                &text(&listed_dir.join("../elsewhere/templates.lua")),
                "Test Battery",
            ),
            "path",
            "is not a file in a folder",
        ),
        (
            from(
                "r-missing",
                &text(&listed_dir.join("none.lua")),
                "Test Battery",
            ),
            "path",
            "is not a file in a folder",
        ),
        (
            from("r-folder", &text(&listed_dir.join("sub")), "Test Battery"),
            "path",
            "is not a file in a folder",
        ),
        (
            from("r-relative", "templates.lua", "Test Battery"),
            "path",
            "is not an absolute path",
        ),
        (
            from("r-empty-path", "", "Test Battery"),
            "path",
            "is not an absolute path",
        ),
        (
            from("r-root", &other, "t"),
            "path",
            "the file is not a group template file: it does not assign a table to `templates`",
        ),
        (
            from("r-code", &code, "t"),
            "path",
            "the file is not a group template file: line 4: expected the end after the value",
        ),
        (from("r-wide", &wide, "t"), "template", "the agent takes"),
        (
            from("r-big", &big, "Test Battery"),
            "path",
            "a group template file is at most 1048576 bytes",
        ),
        (
            from("r-name", &source, "No Such"),
            "template",
            "the file has no template named \"No Such\"",
        ),
        (
            from("r-no-name", &source, ""),
            "template",
            "a template name is required",
        ),
        (
            from("r-plane", &odd, "plane"),
            "template",
            "does not have the type \"vehicle\"",
        ),
        (from("r-empty", &odd, "empty"), "template", "has no units"),
        (
            from("r-typeless", &odd, "typeless"),
            "template",
            "has no `name`, its DCS type",
        ),
        (from("r-nan", &odd, "nan"), "template", "`dx` of unit 1"),
        (
            pb::PutGroupFromTemplateRequest {
                anchor: Some(pb::Position {
                    north: 1e308,
                    ..anchor()
                }),
                ..from("r-far", &odd, "far")
            },
            "template",
            "is not at a finite place from the anchor",
        ),
        (
            from("r-stateless", &odd, "stateless"),
            "template",
            "has no country number; give `country`",
        ),
        (
            pb::PutGroupFromTemplateRequest {
                anchor: None,
                ..base("r-anchor")
            },
            "anchor",
            "a position is required",
        ),
        (
            pb::PutGroupFromTemplateRequest {
                anchor: Some(pb::Position {
                    theater: "Syria".into(),
                    ..anchor()
                }),
                ..base("r-theater")
            },
            "anchor.theater",
            "the campaign does not use the theater `Syria`",
        ),
        (
            pb::PutGroupFromTemplateRequest {
                anchor: Some(pb::Position {
                    north: f64::NAN,
                    ..anchor()
                }),
                ..base("r-north")
            },
            "anchor.north",
            "",
        ),
        (
            pb::PutGroupFromTemplateRequest {
                heading: f64::INFINITY,
                ..base("r-heading")
            },
            "heading",
            "",
        ),
        // The anchor and the heading are refused before the file is: with
        // a file that is refused too, the field is theirs.
        (
            pb::PutGroupFromTemplateRequest {
                anchor: None,
                ..from("r-anchor-first", &missing, "Test Battery")
            },
            "anchor",
            "a position is required",
        ),
        (
            pb::PutGroupFromTemplateRequest {
                anchor: Some(pb::Position {
                    theater: "Syria".into(),
                    ..anchor()
                }),
                ..from("r-theater-first", &code, "t")
            },
            "anchor.theater",
            "the campaign does not use the theater `Syria`",
        ),
        (
            pb::PutGroupFromTemplateRequest {
                heading: f64::INFINITY,
                ..from("r-heading-first", &missing, "Test Battery")
            },
            "heading",
            "",
        ),
        (
            pb::PutGroupFromTemplateRequest {
                order: Some(pb::Order {
                    intent: Some(pb::order::Intent::Attack(pb::AttackOrder {
                        target_id: 999,
                    })),
                }),
                ..base("r-order")
            },
            "order",
            "",
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
        let e = commands.put_group_from_template(req).await.expect_err(&key);
        let detail = errors::detail(&e).expect("detail");
        if field == "order" {
            // The order's own checks, as `PutGroup` gives them: the target
            // does not exist.
            assert_eq!(
                errors::reason(&e),
                ErrorReason::EntityNotFound,
                "{key}: {e}"
            );
        } else {
            assert_eq!(e.code(), Code::InvalidArgument, "{key}: {e}");
            assert_eq!(errors::reason(&e), ErrorReason::InvalidField, "{key}");
            assert_eq!(detail.field, field, "{key}: {e}");
        }
        assert!(e.message().contains(want), "{key}: {}", e.message());
        assert_eq!(revision(&hub), before, "{key} changed the store");
    }
    // Without the lease, and under a wrong one. The lease is refused
    // before the file is: a missing file changes nothing in the answer.
    for (key, path) in [("r-lease", &source), ("r-lease-first", &missing)] {
        let e = commands
            .put_group_from_template(pb::PutGroupFromTemplateRequest {
                write: Some(pb::Write {
                    lease: None,
                    idempotency: Some(pb::Idempotency { key: key.into() }),
                    campaign_id: a,
                }),
                ..from(key, path, "Test Battery")
            })
            .await
            .expect_err("no lease");
        assert_eq!(errors::reason(&e), ErrorReason::NotController, "{key}: {e}");
    }
    let e = commands
        .put_group_from_template(pb::PutGroupFromTemplateRequest {
            write: Some(pb::Write {
                lease: Some(pb::Lease {
                    lease_id: "not-the-lease".into(),
                    ..lease.clone()
                }),
                idempotency: Some(pb::Idempotency {
                    key: "r-lease-2".into(),
                }),
                campaign_id: a,
            }),
            ..base("r-lease-2")
        })
        .await
        .expect_err("a wrong lease");
    assert_eq!(errors::reason(&e), ErrorReason::NotController, "{e}");
    assert_eq!(revision(&hub), before);

    // Made: the answer carries what PutGroup's does.
    let made = commands
        .put_group_from_template(base("t-1"))
        .await
        .expect("made")
        .into_inner();
    assert_ne!(made.group_id, 0);
    assert_eq!(made.unit_ids.len(), 2);
    assert_eq!(made.revision, revision(&hub));
    assert!(made.spec_revision > 0);

    // The same group written by hand, through PutGroup.
    let unit = |ty: &str, north: f64, east: f64, heading: f64, skill: &str| pb::UnitSpec {
        r#type: ty.into(),
        position: Some(pb::Position {
            north,
            east,
            ..anchor()
        }),
        heading,
        skill: skill.into(),
        ..pb::UnitSpec::default()
    };
    let by_hand = commands
        .put_group(pb::PutGroupRequest {
            write: base("hand").write,
            group_id: 0,
            spec: Some(pb::GroupSpec {
                label: "Test Battery".into(),
                country: 34,
                category: pb::GroupCategory::Ground as i32,
                units: vec![
                    unit("Type A", NORTH, EAST, 4.7123889803847, "High"),
                    unit("Type B", NORTH + 100.0, EAST - 25.5, 0.0, "Average"),
                ],
                ..pb::GroupSpec::default()
            }),
            order: None,
        })
        .await
        .expect("by hand")
        .into_inner();
    assert_eq!(
        held(&hub, a, made.group_id),
        held(&hub, a, by_hand.group_id),
        "the template's group is the one PutGroup makes of the same spec"
    );
    assert_eq!(made.spec_revision, by_hand.spec_revision);

    // The request's country, 0 included, its label and its heading.
    let turned = commands
        .put_group_from_template(pb::PutGroupFromTemplateRequest {
            country: Some(0),
            label: "mine".into(),
            heading: std::f64::consts::FRAC_PI_2,
            ..base("t-2")
        })
        .await
        .expect("turned")
        .into_inner();
    {
        let g = hub.lock();
        let s = &g.campaigns[&a].store;
        let grp = s.group(EntityId(turned.group_id)).expect("group");
        assert_eq!((grp.country, grp.label.as_str()), (0, "mine"));
        let second = s.unit(grp.members[1]).expect("unit");
        assert!((second.state.at.x - (NORTH + 25.5)).abs() < 1e-6);
        assert!((second.state.at.z - (EAST + 100.0)).abs() < 1e-6);
        assert_eq!(second.state.heading, std::f64::consts::FRAC_PI_2);
    }
    // A template whose own country is 0, and one given a country it lacks.
    for (key, path, name, country, want) in [
        ("t-3", &source, "Zero Country", None, 0),
        ("t-4", &odd, "stateless", Some(7), 7),
    ] {
        let id = commands
            .put_group_from_template(pb::PutGroupFromTemplateRequest {
                country,
                ..from(key, path, name)
            })
            .await
            .expect(key)
            .into_inner()
            .group_id;
        assert_eq!(
            hub.lock().campaigns[&a]
                .store
                .group(EntityId(id))
                .expect("group")
                .country,
            want,
            "{key}"
        );
    }

    // A backup, then the file goes, with its folder.
    campaigns
        .backup_campaign(pb::BackupCampaignRequest {
            lease: Some(lease.clone()),
            campaign_id: a,
            idempotency: Some(pb::Idempotency { key: "bk".into() }),
        })
        .await
        .expect("backup");
    std::fs::remove_dir_all(&listed_dir).expect("remove the listed folder");
    let settled = revision(&hub);

    // A repeat under the answered key gets the first answer and makes no
    // group; the same request under a new key is refused for its path; the
    // answered key with another request is a mismatch.
    let again = commands
        .put_group_from_template(base("t-1"))
        .await
        .expect("the repeat")
        .into_inner();
    assert_eq!(again, made);
    let e = commands
        .put_group_from_template(base("t-1-new"))
        .await
        .expect_err("the file is gone");
    assert_eq!(errors::detail(&e).expect("detail").field, "path");
    let e = commands
        .put_group_from_template(from("t-1", &source, "Zero Country"))
        .await
        .expect_err("another request under the key");
    assert_eq!(errors::reason(&e), ErrorReason::IdempotencyMismatch, "{e}");
    assert_eq!(revision(&hub), settled);

    // Activated: both groups go to DCS by the same path and come back the
    // same, at the places the template gives.
    let pair = [made.group_id, by_hand.group_id];
    campaigns
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(lease.clone()),
            campaign_id: a,
            idempotency: Some(pb::Idempotency {
                key: "activate".into(),
            }),
        })
        .await
        .expect("activate");
    live(&hub, a, &pair).await;
    let check = |g: &pb::Group, twin: &pb::Group| {
        assert_eq!(g.state, pb::EntityState::Live as i32);
        assert_eq!(g.applied_spec_revision, twin.applied_spec_revision);
        assert_eq!(g.last_error, "");
        assert_eq!(g.label, "Test Battery");
        assert_eq!(g.country, 34);
        assert_eq!(g.category, pb::GroupCategory::Ground as i32);
        assert_eq!(g.units.len(), 2);
        let places = |g: &pb::Group| -> Vec<(String, bool, f64, f64)> {
            g.units
                .iter()
                .map(|u| {
                    let p = u.position.as_ref().expect("position");
                    assert_eq!(p.theater, "Caucasus");
                    (u.r#type.clone(), u.alive, p.north, p.east)
                })
                .collect()
        };
        assert_eq!(places(g), places(twin));
        let at = places(g);
        assert_eq!(at[0].0, "Type A");
        assert_eq!(at[1].0, "Type B");
        assert!(at.iter().all(|u| u.1));
        assert!((at[0].2 - NORTH).abs() < 0.01 && (at[0].3 - EAST).abs() < 0.01);
        assert!((at[1].2 - (NORTH + 100.0)).abs() < 0.01, "{at:?}");
        assert!((at[1].3 - (EAST - 25.5)).abs() < 0.01, "{at:?}");
    };
    check(
        &listed(&channel, a, pair[0]).await,
        &listed(&channel, a, pair[1]).await,
    );

    // A reload spawns it again from the store.
    mission
        .reload(pb::ReloadRequest {
            write: Some(pb::Write {
                lease: Some(lease.clone()),
                idempotency: Some(pb::Idempotency {
                    key: "reload-1".into(),
                }),
                campaign_id: 0,
            }),
        })
        .await
        .expect("reload");
    live(&hub, a, &pair).await;
    check(
        &listed(&channel, a, pair[0]).await,
        &listed(&channel, a, pair[1]).await,
    );

    // The sidecar stops, the backup is restored, and a new sidecar with a
    // new DCS spawns the group, all with the file gone.
    let _ = stop.send(());
    task.await.expect("join").expect("serve");
    hub.lock().close_all().expect("close");
    drop(hub);
    let store_dir = dir.path().join("campaigns");
    nanoprobe_sidecar::campaigns::Campaigns::new(&store_dir, 1, 1)
        .expect("campaigns")
        .restore(CampaignId(a), 1, 1)
        .expect("restore");
    assert!(!listed_dir.exists());
    let hub = Hub::open(cfg, backend(&missions)).expect("hub");
    let (channel, stop, task) = serve_hub(&hub).await;
    let lease = lease_of(&channel).await;
    let mut campaigns = CampaignServiceClient::new(channel.clone());
    campaigns
        .activate_campaign(pb::ActivateCampaignRequest {
            lease: Some(lease.clone()),
            campaign_id: a,
            idempotency: Some(pb::Idempotency {
                key: "activate-restored".into(),
            }),
        })
        .await
        .expect("activate the restored campaign");
    live(&hub, a, &pair).await;
    check(
        &listed(&channel, a, pair[0]).await,
        &listed(&channel, a, pair[1]).await,
    );
    // The restored store still answers the first call's key.
    let again = CommandsServiceClient::new(channel.clone())
        .put_group_from_template(pb::PutGroupFromTemplateRequest {
            write: Some(pb::Write {
                lease: Some(lease.clone()),
                idempotency: Some(pb::Idempotency { key: "t-1".into() }),
                campaign_id: a,
            }),
            ..from("t-1", &source, "Test Battery")
        })
        .await
        .expect("the repeat after the restore")
        .into_inner();
    assert_eq!(
        (again.group_id, again.unit_ids),
        (made.group_id, made.unit_ids)
    );

    let _ = stop.send(());
    task.await.expect("join").expect("serve");
    hub.lock().close_all().expect("close");
}
