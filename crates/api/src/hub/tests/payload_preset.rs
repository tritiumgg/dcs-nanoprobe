//! `PutGroup` with a payload preset, through the services, on the fake sim
//! (spec/world.md, "Payload presets").

use nanoprobe_proto::errors;
use nanoprobe_proto::v1alpha1::campaign_service_client::CampaignServiceClient;
use nanoprobe_proto::v1alpha1::commands_service_client::CommandsServiceClient;
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::lifecycle::State;
use prost_types::value::Kind as V;

use super::*;
use crate::payload_preset::FILE_MAX;
use crate::payload_preset::tests::{FILE, by_hand};

fn at() -> pb::Position {
    pb::Position {
        theater: "Caucasus".into(),
        north: -280_000.0,
        east: 640_000.0,
        alt: 3000.0,
    }
}

fn st(v: serde_json::Value) -> prost_types::Struct {
    let serde_json::Value::Object(m) = v else {
        panic!("an object")
    };
    prost_types::Struct {
        fields: m.into_iter().map(|(k, v)| (k, val(v))).collect(),
    }
}

fn val(v: serde_json::Value) -> prost_types::Value {
    let kind = match v {
        serde_json::Value::Null => V::NullValue(0),
        serde_json::Value::Bool(b) => V::BoolValue(b),
        serde_json::Value::Number(n) => V::NumberValue(n.as_f64().expect("f64")),
        serde_json::Value::String(s) => V::StringValue(s),
        serde_json::Value::Array(a) => V::ListValue(prost_types::ListValue {
            values: a.into_iter().map(val).collect(),
        }),
        o @ serde_json::Value::Object(_) => V::StructValue(st(o)),
    };
    prost_types::Value { kind: Some(kind) }
}

fn jet(extra: Option<prost_types::Struct>, preset: Option<pb::PayloadPreset>) -> pb::UnitSpec {
    pb::UnitSpec {
        r#type: "Test-Jet".into(),
        position: Some(at()),
        skill: "High".into(),
        extra,
        payload_preset: preset,
        ..pb::UnitSpec::default()
    }
}

fn group(units: Vec<pb::UnitSpec>) -> pb::GroupSpec {
    pb::GroupSpec {
        label: "Jets".into(),
        country: 2,
        category: pb::GroupCategory::Airplane as i32,
        units,
        ..pb::GroupSpec::default()
    }
}

/// What the store holds of a group and its units, without the identities.
fn held(hub: &Hub, c: u64, id: u64) -> Vec<String> {
    let g = hub.lock();
    let s = &g.campaigns[&c].store;
    let grp = s.group(EntityId(id)).expect("group");
    let mut out = vec![format!(
        "{} {} {} {:?} {:?} {:?}",
        grp.country, grp.category, grp.label, grp.extra, grp.components, grp.order
    )];
    out.extend(grp.members.iter().map(|m| {
        let u = s.unit(*m).expect("unit");
        format!(
            "{} {:?} {:?} {:?} at {:?} heading {}",
            u.type_name, u.label, u.skill, u.extra, u.state.at, u.state.heading
        )
    }));
    out
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preset_gives_the_pylons_written_by_hand_and_is_not_kept() {
    let (dir, missions, mut cfg) = owed_setup();
    let listed_dir = dir.path().join("presets");
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
    let source = write("Test-Jet.lua", FILE.as_bytes());
    std::fs::write(unlisted.join("Test-Jet.lua"), FILE).expect("unlisted source");
    let code = write(
        "code.lua",
        b"local tank = \"{TEST-TANK}\"\nlocal unitPayloads = {}\nreturn unitPayloads\n",
    );
    let mut big = FILE.as_bytes().to_vec();
    big.resize(FILE_MAX + 1, b' ');
    let big = write("big.lua", &big);
    let preset = |path: &str, name: &str| {
        Some(pb::PayloadPreset {
            path: path.to_owned(),
            name: name.into(),
        })
    };
    let named = |name: &str| preset(&source, name);

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
    let a = campaigns
        .create_campaign(create(&lease, "a", "Caucasus", false))
        .await
        .expect("a")
        .into_inner()
        .campaign
        .expect("campaign")
        .id;
    let put = |key: &str, group_id: u64, spec: pb::GroupSpec| pb::PutGroupRequest {
        write: Some(pb::Write {
            lease: Some(lease.clone()),
            idempotency: Some(pb::Idempotency { key: key.into() }),
            campaign_id: a,
        }),
        group_id,
        spec: Some(spec),
        order: None,
    };
    let revision = |hub: &Hub| hub.lock().campaigns[&a].store.revision();
    let before = revision(&hub);

    // Every refusal names its field and its cause, and changes nothing.
    let f = "spec.units[1].payload_preset";
    let one = |key: &str, u: pb::UnitSpec| put(key, 0, group(vec![jet(None, None), u]));
    let with_pylons = st(serde_json::json!({ "payload": { "pylons": {} } }));
    let refusals: Vec<(pb::PutGroupRequest, String, &str)> = vec![
        (
            one(
                "r-unlisted",
                jet(None, preset(&text(&unlisted.join("Test-Jet.lua")), "Clean")),
            ),
            format!("{f}.path"),
            "is not a file in a folder that [missions] template_dirs lists",
        ),
        (
            one("r-relative", jet(None, preset("Test-Jet.lua", "Clean"))),
            format!("{f}.path"),
            "is not an absolute path",
        ),
        (
            one("r-empty-path", jet(None, preset("", "Clean"))),
            format!("{f}.path"),
            "is not an absolute path",
        ),
        (
            one("r-big", jet(None, preset(&big, "Clean"))),
            format!("{f}.path"),
            "a payload preset file is at most 1048576 bytes",
        ),
        (
            one("r-code", jet(None, preset(&code, "Clean"))),
            format!("{f}.path"),
            "line 2: expected `return tank`; a file with Lua code is not read: save the preset from the Mission Editor",
        ),
        (
            one(
                "r-type",
                pb::UnitSpec {
                    r#type: "Other-Jet".into(),
                    ..jet(None, named("Clean"))
                },
            ),
            format!("{f}.path"),
            "the file's presets are for the type \"Test-Jet\"; the unit's type is \"Other-Jet\"",
        ),
        (
            one("r-name", jet(None, named("No Such"))),
            format!("{f}.name"),
            "the file has no preset named \"No Such\"",
        ),
        (
            one("r-display", jet(None, named("Shown name"))),
            format!("{f}.name"),
            "the file has no preset named \"Shown name\"",
        ),
        (
            one("r-twice", jet(None, named("Twice"))),
            format!("{f}.name"),
            "more than one preset named \"Twice\"",
        ),
        (
            one("r-pylons", jet(Some(with_pylons), named("Clean"))),
            "spec.units[1].extra.payload.pylons".into(),
            "carries no `pylons`",
        ),
        (
            one(
                "r-payload",
                jet(
                    Some(st(serde_json::json!({ "payload": 3 }))),
                    named("Clean"),
                ),
            ),
            "spec.units[1].extra.payload".into(),
            "an object `payload` or none",
        ),
        // A unit with no type is refused for that, not for its preset.
        (
            one(
                "r-typeless",
                pb::UnitSpec {
                    r#type: String::new(),
                    ..jet(None, named("Clean"))
                },
            ),
            "spec.units[1].type".into(),
            "a unit needs its DCS type",
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
        let e = commands.put_group(req).await.expect_err(&key);
        assert_eq!(e.code(), Code::InvalidArgument, "{key}: {e}");
        assert_eq!(errors::reason(&e), ErrorReason::InvalidField, "{key}");
        assert_eq!(
            errors::detail(&e).expect("detail").field,
            field,
            "{key}: {e}"
        );
        assert!(e.message().contains(want), "{key}: {}", e.message());
        assert_eq!(revision(&hub), before, "{key} changed the store");
    }
    // More preset files than a call may name are refused before any is
    // read, with the lease and campaign checked first as for any refusal.
    let many: Vec<pb::UnitSpec> = (0..=crate::services::commands::PRESET_FILES_MAX)
        .map(|i| jet(None, preset(&format!("/none-{i}.lua"), "Clean")))
        .collect();
    let e = commands
        .put_group(put("r-many", 0, group(many)))
        .await
        .expect_err("too many files");
    assert_eq!(
        errors::detail(&e).expect("detail").field,
        "spec.units",
        "{e}"
    );
    assert!(
        e.message()
            .contains("names at most 16 payload preset files; this one names 17"),
        "{e}"
    );
    assert_eq!(revision(&hub), before);
    // The lease's refusal is given before a file's: a missing file changes
    // nothing in the answer.
    let e = commands
        .put_group(pb::PutGroupRequest {
            write: Some(pb::Write {
                lease: None,
                idempotency: Some(pb::Idempotency {
                    key: "r-lease".into(),
                }),
                campaign_id: a,
            }),
            ..one("r-lease", jet(None, preset("/none.lua", "Clean")))
        })
        .await
        .expect_err("no lease");
    assert_eq!(errors::reason(&e), ErrorReason::NotController, "{e}");
    assert_eq!(revision(&hub), before);

    // Made from the preset, with the request's other payload keys kept, and
    // two units of one file.
    let others =
        || st(serde_json::json!({ "payload": { "fuel": 5000, "gun": 100 }, "speed": 200 }));
    let made = commands
        .put_group(put(
            "p-1",
            0,
            group(vec![
                jet(Some(others()), named("Bombs*2, Fuel")),
                jet(None, named("Clean")),
            ]),
        ))
        .await
        .expect("made")
        .into_inner();
    // The same group written by hand.
    let mut hand = others();
    let Some(V::StructValue(payload)) = &mut hand.fields.get_mut("payload").expect("payload").kind
    else {
        unreachable!()
    };
    payload.fields.insert(
        "pylons".into(),
        prost_types::Value {
            kind: Some(V::StructValue(by_hand())),
        },
    );
    let clean = st(serde_json::json!({ "payload": { "pylons": {} } }));
    let written = commands
        .put_group(put(
            "h-1",
            0,
            group(vec![jet(Some(hand.clone()), None), jet(Some(clean), None)]),
        ))
        .await
        .expect("by hand")
        .into_inner();
    assert_eq!(
        held(&hub, a, made.group_id),
        held(&hub, a, written.group_id),
        "the preset's group is the one PutGroup makes of the pylons written by hand"
    );
    assert_eq!(made.spec_revision, written.spec_revision);

    // A repeat under the answered key gets the first answer, the file
    // changed; a replace that names the preset reads the file as it is.
    std::fs::write(&source, FILE.replace("{TEST-TANK}", "{TEST-POD}")).expect("change");
    let settled = revision(&hub);
    let again = commands
        .put_group(put(
            "p-1",
            0,
            group(vec![
                jet(Some(others()), named("Bombs*2, Fuel")),
                jet(None, named("Clean")),
            ]),
        ))
        .await
        .expect("the repeat")
        .into_inner();
    assert_eq!(again, made);
    assert_eq!(revision(&hub), settled);
    let replaced = commands
        .put_group(put(
            "p-2",
            made.group_id,
            group(vec![
                pb::UnitSpec {
                    id: made.unit_ids[0],
                    position: None,
                    ..jet(Some(others()), named("Bombs*2, Fuel"))
                },
                pb::UnitSpec {
                    id: made.unit_ids[1],
                    position: None,
                    ..jet(None, named("Clean"))
                },
            ]),
        ))
        .await
        .expect("replaced")
        .into_inner();
    assert!(replaced.spec_revision > made.spec_revision);
    let first = {
        let g = hub.lock();
        let s = &g.campaigns[&a].store;
        let m = s.group(EntityId(made.group_id)).expect("group").members[0];
        format!(
            "{:?}",
            crate::lua::from_lua(&s.unit(m).expect("unit").extra)
        )
    };
    assert!(
        first.contains("{TEST-POD}") && !first.contains("{TEST-TANK}"),
        "{first}"
    );
    // With the file gone, the repeat still gets its first answer.
    std::fs::remove_file(&source).expect("remove the file");
    let settled = revision(&hub);
    let again = commands
        .put_group(put(
            "p-1",
            0,
            group(vec![
                jet(Some(others()), named("Bombs*2, Fuel")),
                jet(None, named("Clean")),
            ]),
        ))
        .await
        .expect("the repeat with the file gone")
        .into_inner();
    assert_eq!(again, made);
    assert_eq!(revision(&hub), settled);
    std::fs::write(&source, FILE).expect("restore the file");

    // Both groups go to DCS by the same path.
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
    live(&hub, a, &[made.group_id, written.group_id]).await;

    let _ = stop.send(());
    task.await.expect("join").expect("serve");
    hub.lock().close_all().expect("close");
}
