//! `sidecar <name> --part payload-preset`: an aircraft given a payload
//! preset on a running DCS (`PutGroup` with `payload_preset`;
//! spec/world.md, "Payload presets").
//!
//! The run writes a preset file of its own into the folder the part's
//! `sidecar.toml` lists, in the form Eagle Dynamics ships its preset files
//! (ED-CODE, docs/results/2026-11-24-payload-presets.md). It makes two
//! one-aircraft groups of one type in the air: one from the preset, one
//! with the same pylons written by hand in `extra.payload`. Neither sends
//! `fuel`, `flare`, `chaff` or `gun`. It reads both aircraft's ammunition
//! and fuel from DCS by their platform names, and the stored unit rows
//! from the campaign's export, and removes everything and the file, also
//! when a step fails.

use std::collections::BTreeMap;

use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};

use super::*;

/// The file's name in the listed folder.
const FILE: &str = "np-live-presets.lua";
/// The aircraft type. Its preset file in the install carries pylon
/// `settings` and starts as the data form (ED-CODE, the results document);
/// the run writes a file of its own and does not read that one.
const TYPE: &str = "F-4E-45MC";
/// The preset's name.
const PRESET: &str = "np live bombs";
/// The DCS country, one the hello mission holds (LIVE,
/// docs/results/2026-11-18-spawn-template.md).
const COUNTRY: u32 = 2;
/// A bomb and its fuze settings, and a missile, on pylons a shipped preset
/// of the type uses for them (ED-CODE, the same file).
const BOMB: &str = "{AB8B8299-F1CC-4359-89B5-2172E0CF4A5A}";
const MISSILE: &str = "{HB_F4E_AIM-7E-2}";
/// Pylon, CLSID, and whether the pylon carries `settings`.
const PYLONS: [(u32, &str, bool); 3] = [(1, BOMB, true), (13, BOMB, true), (9, MISSILE, false)];
/// Where the two aircraft fly: 3 km up, 2 km apart, north of the hello
/// campaigns' start.
const AT: [(f64, f64); 2] = [(-289_000.0, 680_000.0), (-289_000.0, 682_000.0)];
const ALT: f64 = 3000.0;

/// The preset file, in the form of the shipped ones.
fn file() -> String {
    let pylon = |i: usize, (num, clsid, settings): (u32, &str, bool)| {
        let s = if settings {
            "\t\t\t\t\t[\"settings\"] = {\n\
             \t\t\t\t\t\t[\"GUI_fuze_type\"] = 1,\n\
             \t\t\t\t\t\t[\"arm_delay_ctrl_FMU139CB_LD\"] = 1,\n\
             \t\t\t\t\t\t[\"function_delay_ctrl_FMU139CB_LD\"] = 0,\n\
             \t\t\t\t\t},\n"
        } else {
            ""
        };
        format!(
            "\t\t\t\t[{}] = {{\n\t\t\t\t\t[\"CLSID\"] = \"{clsid}\",\n\t\t\t\t\t[\"num\"] = {num},\n{s}\t\t\t\t}},\n",
            i + 1
        )
    };
    let pylons: String = PYLONS
        .iter()
        .enumerate()
        .map(|(i, p)| pylon(i, *p))
        .collect();
    format!(
        "local unitPayloads = {{\n\t[\"name\"] = \"{TYPE}\",\n\t[\"payloads\"] = {{\n\t\t[1] = {{\n\
         \t\t\t[\"displayName\"] = \"{PRESET}\",\n\t\t\t[\"name\"] = \"{PRESET}\",\n\
         \t\t\t[\"pylons\"] = {{\n{pylons}\t\t\t}},\n\t\t\t[\"tasks\"] = {{\n\t\t\t\t[1] = 32,\n\t\t\t}},\n\
         \t\t}},\n\t}},\n\t[\"unitType\"] = \"{TYPE}\",\n}}\nreturn unitPayloads\n"
    )
}

fn sv(v: Value) -> prost_types::Value {
    use prost_types::value::Kind as K;
    let kind = match v {
        Value::Null => K::NullValue(0),
        Value::Bool(b) => K::BoolValue(b),
        Value::Number(n) => K::NumberValue(n.as_f64().unwrap_or_default()),
        Value::String(s) => K::StringValue(s),
        Value::Array(a) => K::ListValue(prost_types::ListValue {
            values: a.into_iter().map(sv).collect(),
        }),
        Value::Object(m) => K::StructValue(prost_types::Struct {
            fields: m.into_iter().map(|(k, v)| (k, sv(v))).collect(),
        }),
    };
    prost_types::Value { kind: Some(kind) }
}

fn obj(v: Value) -> prost_types::Struct {
    match sv(v).kind {
        Some(prost_types::value::Kind::StructValue(s)) => s,
        _ => prost_types::Struct::default(),
    }
}

/// The pylons written by hand, as `extra.payload.pylons` takes them.
fn by_hand() -> Value {
    let mut m = serde_json::Map::new();
    for (num, clsid, settings) in PYLONS {
        let mut p = json!({ "CLSID": clsid });
        if settings {
            p["settings"] = json!({
                "GUI_fuze_type": 1,
                "arm_delay_ctrl_FMU139CB_LD": 1,
                "function_delay_ctrl_FMU139CB_LD": 0,
            });
        }
        m.insert(num.to_string(), p);
    }
    Value::Object(m)
}

/// The keys every aircraft of the run has besides its payload: an
/// aircraft spawned in the air (LIVE, docs/results/2026-11-15-s18-persistence-live.md).
fn flying() -> Value {
    json!({ "speed": 180, "alt_type": "BARO" })
}

fn spec(
    label: &str,
    at: (f64, f64),
    extra: Value,
    preset: Option<pb::PayloadPreset>,
) -> pb::GroupSpec {
    pb::GroupSpec {
        label: label.into(),
        country: COUNTRY,
        category: pb::GroupCategory::Airplane as i32,
        units: vec![pb::UnitSpec {
            r#type: TYPE.into(),
            label: format!("{label}-0"),
            position: Some(pb::Position {
                alt: ALT,
                ..caucasus(at.0, at.1)
            }),
            skill: "High".into(),
            extra: Some(obj(extra)),
            payload_preset: preset,
            ..pb::UnitSpec::default()
        }],
        ..pb::GroupSpec::default()
    }
}

fn put(api: &mut Api, c: u64, spec: pb::GroupSpec) -> Result<pb::PutGroupResponse, tonic::Status> {
    let req = pb::PutGroupRequest {
        write: api.write("payload-preset", c),
        group_id: 0,
        spec: Some(spec),
        order: None,
    };
    api.rt
        .block_on(api.commands.put_group(req))
        .map(tonic::Response::into_inner)
}

/// Run the part; the result goes under `payload_preset`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    results: &mut Value,
) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);

    println!("== payload-preset: a campaign on Caucasus");
    let c = api.create("payload-preset-live", &["Caucasus"], true)?;
    loaded(run, &mut api, "Caucasus", generation)?;

    let on_profile = format!("{}/own/{FILE}", crate::sidecar::MISSIONS_DIR);
    run.profile().write(&on_profile, file().as_bytes())?;
    let path = run.template_path(FILE);
    let out = steps(run, &mut api, c, &path);
    // The file goes whatever the steps did.
    let removed = run.profile().remove_file(&on_profile);
    let gone = run.profile().read(&on_profile).map(|f| f.is_none());
    let mut v = out?;
    removed?;
    v["file_removed"] = json!(gone?);
    v["pass"] = json!(v["pass"] == json!(true) && v["file_removed"] == json!(true));
    println!(
        "   {} {}",
        if v["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        },
        v["ammo"]
    );
    results["payload_preset"] = v;
    Ok(())
}

fn steps(run: &dyn Live, api: &mut Api, c: u64, path: &str) -> Result<Value, String> {
    let preset = || {
        Some(pb::PayloadPreset {
            path: path.to_owned(),
            name: PRESET.into(),
        })
    };
    println!("== payload-preset: a preset with pylons written too is refused");
    let both = put(
        api,
        c,
        spec(
            "np-both",
            AT[0],
            json!({ "speed": 180, "alt_type": "BARO", "payload": { "pylons": by_hand() } }),
            preset(),
        ),
    );
    let refused = matches!(&both, Err(s) if s.code() == tonic::Code::InvalidArgument
        && s.message().contains("carries no `pylons`"));
    let refusal = match both {
        Err(s) => Some(s.message().to_owned()),
        // Accepted, the fault this step looks for: the group goes at once,
        // so it neither stays on the server nor joins the reads below.
        Ok(stray) => {
            let stray = [stray];
            remove(run, api, c, &stray, &dcs_names(c, &stray))?;
            None
        }
    };

    println!("== payload-preset: one {TYPE} from the preset in {path}, one by hand");
    let mut hand = flying();
    hand["payload"] = json!({ "pylons": by_hand() });
    let mut made = Vec::new();
    let read = spawn_and_read(
        run,
        api,
        c,
        &mut made,
        [
            spec("np-preset", AT[0], flying(), preset()),
            spec("np-hand", AT[1], hand, None),
        ],
    );
    println!("== payload-preset: remove everything");
    let names = dcs_names(c, &made);
    let left = remove(run, api, c, &made, &names);
    if let Err(e) = &left {
        println!("   FAIL the groups were not removed: {e}");
    }
    let (seen, stored) = read?;
    let mut v = verdict(&seen, &stored);
    v["pass"] = json!(v["pass"] == json!(true) && refused && left.is_ok());
    v["both_refused"] = json!(refused);
    v["both_refusal"] = json!(refusal);
    v["answers"] = json!(made
        .iter()
        .map(|m| json!({ "group_id": m.group_id, "unit_ids": m.unit_ids, "spec_revision": m.spec_revision }))
        .collect::<Vec<_>>());
    v["dcs_names"] = json!(names);
    v["groups_left"] = json!(left.is_ok().then_some(0));
    Ok(v)
}

/// The DCS name of the one unit of each made group.
fn dcs_names(c: u64, made: &[pb::PutGroupResponse]) -> Vec<String> {
    made.iter()
        .flat_map(|m| m.unit_ids.iter())
        .map(|u| name(NameKind::Unit, CampaignId(c), EntityId(*u)))
        .collect()
}

/// A read of each aircraft: whether it exists, its type, its ammunition by
/// type name, its fuel as a fraction of internal, and its fuel mass at
/// full internal.
fn read_lua(names: &[String]) -> String {
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    format!(
        "local out = {{}}\nfor i, n in ipairs({{ {} }}) do\n  local u = Unit.getByName(n)\n  if u then\n    \
         local ammo = {{}}\n    for _, a in ipairs(u:getAmmo() or {{}}) do\n      local t = a.desc and a.desc.typeName or \"?\"\n      \
         ammo[t] = (ammo[t] or 0) + (a.count or 0)\n    end\n    \
         out[i] = {{ type = u:getTypeName(), ammo = ammo, fuel = u:getFuel(), fuel_mass_max = u:getDesc().fuelMassMax }}\n  \
         else\n    out[i] = false\n  end\nend\nreturn out\n",
        list.join(", ")
    )
}

/// The `extra` of each made unit's row in the campaign's export.
fn stored(api: &mut Api, c: u64, units: &[u64]) -> Result<Vec<Value>, String> {
    let mut campaigns = api.campaigns.clone();
    let rows: BTreeMap<u64, Value> = api.rt.block_on(async move {
        let mut s = campaigns
            .export_campaign(pb::ExportCampaignRequest { campaign_id: c })
            .await
            .map_err(status_text("ExportCampaign"))?
            .into_inner();
        let mut rows = BTreeMap::new();
        while let Some(page) = s.message().await.map_err(status_text("ExportCampaign"))? {
            for line in page.lines.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                let v: Value = serde_json::from_slice(line)
                    .map_err(|e| format!("ExportCampaign: a line is not JSON: {e}"))?;
                if v["kind"] == json!("entity")
                    && let Some(id) = v["id"].as_u64()
                {
                    rows.insert(id, v["body"].clone());
                }
            }
        }
        Ok::<_, String>(rows)
    })?;
    units
        .iter()
        .map(|u| {
            rows.get(u)
                .map(|b| extra_json(&b["extra"]))
                .ok_or_else(|| format!("unit {u} is not in the export"))
        })
        .collect()
}

/// A unit row's `extra` as JSON. The row holds it as the store's encoding
/// of the Lua table, a byte string, which the export gives in base 64
/// (spec/persistence.md, "Export"); a value in any other form is kept.
fn extra_json(v: &Value) -> Value {
    let decoded = v["bytes_b64"]
        .as_str()
        .and_then(unbase64)
        .and_then(|b| nanoprobe_transport::codec::Reader::new(&b).lv().ok());
    match decoded {
        Some(lv) => nanoprobe_sidecar::export::lv_json(&lv),
        None => v.clone(),
    }
}

/// Standard base 64 with padding.
fn unbase64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s {
        acc = (acc << 6) | u32::from(val(c)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Make both groups, wait until they are live, then read them from DCS and
/// the store. Every answer is in `made`, also when a later step fails.
fn spawn_and_read(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    made: &mut Vec<pb::PutGroupResponse>,
    specs: [pb::GroupSpec; 2],
) -> Result<(Value, Vec<Value>), String> {
    for s in specs {
        made.push(put(api, c, s).map_err(status_text("PutGroup"))?);
    }
    for m in made.iter() {
        live(run, api, c, m.group_id)?;
    }
    let seen = run.eval(
        "server",
        "payload-preset-read",
        &read_lua(&dcs_names(c, made)),
        60,
    )?;
    api.keep();
    let units: Vec<u64> = made.iter().flat_map(|m| m.unit_ids.clone()).collect();
    let rows = stored(api, c, &units)?;
    Ok((seen, rows))
}

/// The weapons an aircraft's ammunition read lists: the rows whose type
/// name `getAmmo` puts under `weapons.`, which leaves out gun rounds.
fn weapons(ammo: &Value) -> u64 {
    ammo.as_object().map_or(0, |m| {
        m.iter()
            .filter(|(k, _)| k.starts_with("weapons."))
            .filter_map(|(_, n)| n.as_u64())
            .sum()
    })
}

/// The verdict: both aircraft in DCS, of the type, with the same
/// ammunition, holding one weapon for each pylon (each pylon of the run
/// carries one); both stored rows with the same `extra`, whose pylons are
/// the ones written by hand.
fn verdict(seen: &Value, stored: &[Value]) -> Value {
    let a = &seen[0];
    let b = &seen[1];
    let present = a.is_object() && b.is_object();
    let typed = a["type"] == json!(TYPE) && b["type"] == json!(TYPE);
    let armed = weapons(&a["ammo"]) == PYLONS.len() as u64;
    let ammo_same = present && a["ammo"] == b["ammo"] && armed;
    let specs_same = stored.len() == 2 && stored[0] == stored[1];
    // Compared as the API carries values, where every number is a double.
    let pylons_as_written = stored
        .first()
        .is_some_and(|e| sv(e["payload"]["pylons"].clone()) == sv(by_hand()));
    json!({
        "pass": present && typed && ammo_same && specs_same && pylons_as_written,
        "present": present,
        "typed": typed,
        "ammo_same": ammo_same,
        "one_weapon_per_pylon": armed,
        "stored_extra_same": specs_same,
        "pylons_as_written": pylons_as_written,
        "ammo": [a["ammo"], b["ammo"]],
        "fuel": [a["fuel"], b["fuel"]],
        "fuel_mass_max": a["fuel_mass_max"],
        "seen": seen,
        "stored_extra": stored,
    })
}

/// Remove both groups and hold the simulation running until DCS has
/// dropped them. Every removal is tried; the first error is given.
fn remove(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    made: &[pb::PutGroupResponse],
    names: &[String],
) -> Result<(), String> {
    api.keep();
    let mut sent = Ok(());
    for m in made {
        let req = pb::RemoveEntityRequest {
            write: api.write("remove", c),
            entity_id: m.group_id,
        };
        let r = api
            .rt
            .block_on(api.commands.remove_entity(req))
            .map_err(status_text("RemoveEntity"));
        if let Err(e) = r {
            sent = sent.and(Err(e));
        }
    }
    let read = read_lua(names);
    wait(
        run,
        api,
        Duration::from_secs(120),
        "the groups removed",
        |_| {
            let seen = run.eval("server", "payload-preset-left", &read, 60)?;
            let left = seen
                .as_array()
                .map(|a| a.iter().filter(|o| *o != &json!(false)).count())
                .unwrap_or(usize::MAX);
            Ok((left == 0).then_some(()))
        },
    )?;
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_gives_the_pylons_written_by_hand() {
        let f = nanoprobe_api::payload_preset::read(file().into_bytes()).expect("read");
        let p = f.pylons(TYPE, PRESET).expect("pylons");
        assert_eq!(
            nanoprobe_api::lua::from_lua(&nanoprobe_api::lua::struct_to_lua(&p.pylons)),
            sv(by_hand())
        );
        assert_eq!(p.dropped, vec!["displayName", "tasks"]);
    }

    fn base64(b: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in b.chunks(3) {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, x)| n | u32::from(*x) << (16 - 8 * i));
            for i in 0..4 {
                if i <= c.len() {
                    out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn an_exported_extra_decodes_to_the_pylons_written_by_hand() {
        let mut extra = flying();
        extra["payload"] = json!({ "pylons": by_hand() });
        let lv = nanoprobe_api::lua::struct_to_lua(&obj(extra.clone()));
        let mut w = nanoprobe_transport::codec::Writer::new();
        w.lv(&lv);
        let row = json!({ "bytes_b64": base64(&w.finish()) });
        let got = extra_json(&row);
        assert_eq!(sv(got.clone()), sv(extra));
        let (seen, _) = good();
        assert_eq!(verdict(&seen, &[got.clone(), got])["pass"], json!(true));
        assert_eq!(extra_json(&json!({ "a": 1 })), json!({ "a": 1 }));
    }

    fn good() -> (Value, Vec<Value>) {
        let ammo = json!({
            "weapons.bombs.Mk_84": 2,
            "weapons.missiles.HB-AIM-7E-2": 1,
            "supply.shells.1": 639,
        });
        let one = json!({ "type": TYPE, "ammo": ammo, "fuel": 1.0, "fuel_mass_max": 5000 });
        let mut extra = flying();
        extra["payload"] = json!({ "pylons": by_hand() });
        (json!([one.clone(), one]), vec![extra.clone(), extra])
    }

    #[test]
    fn the_verdict_passes_a_good_read_and_fails_each_fault() {
        let (seen, stored) = good();
        assert_eq!(verdict(&seen, &stored)["pass"], json!(true));
        let mut s = seen.clone();
        s[1]["ammo"]["weapons.bombs.Mk_84"] = json!(1);
        assert_eq!(verdict(&s, &stored)["pass"], json!(false));
        // DCS dropping every pylon on both leaves the gun alone.
        let mut s = seen.clone();
        s[0]["ammo"] = json!({ "supply.shells.1": 639 });
        s[1]["ammo"] = json!({ "supply.shells.1": 639 });
        assert_eq!(verdict(&s, &stored)["pass"], json!(false));
        let mut s = seen.clone();
        s[0] = json!(false);
        assert_eq!(verdict(&s, &stored)["pass"], json!(false));
        let mut s = seen.clone();
        s[0]["ammo"] = json!({});
        s[1]["ammo"] = json!({});
        assert_eq!(verdict(&s, &stored)["pass"], json!(false));
        let mut s = seen.clone();
        s[1]["type"] = json!("F-15C");
        assert_eq!(verdict(&s, &stored)["pass"], json!(false));
        let mut st = stored.clone();
        st[1]["speed"] = json!(200);
        assert_eq!(verdict(&seen, &st)["pass"], json!(false));
        let mut st = stored.clone();
        st[0]["payload"]["pylons"]["1"]["CLSID"] = json!("x");
        st[1] = st[0].clone();
        assert_eq!(verdict(&seen, &st)["pass"], json!(false));
    }
}
