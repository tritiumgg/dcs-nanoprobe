//! `sidecar <name> --part saved-files`: a static template and a payload
//! preset that a person saved from the Mission Editor into the target's
//! Saved Games profile, made on a running DCS.
//!
//! The run writes no file of its own and copies none. The part's
//! `sidecar.toml` lists the folder of each given file, and the calls name
//! the files where they are. The expectations come from the sidecar's own
//! readers over the same bytes, read from the profile here:
//!
//! - The static template is made once, at an anchor north of the hello
//!   campaigns' start with no turn, every entity under one DCS country the
//!   hello mission holds, so a layout saved on any map and under any
//!   country can be made on the shell mission. Every unit and static is read
//!   back from DCS by its platform name: its type and its place against
//!   the place `static_template::specs` gives it.
//! - The preset is given to one aircraft of the file's type in the air, and
//!   a second aircraft gets the same pylons written by hand in
//!   `extra.payload`, as `payload_preset::File::pylons` converts them. Both
//!   aircraft's ammunition is read from DCS, and both stored unit rows from
//!   the campaign's export.
//!
//! Everything made is removed, also when a step fails.

use nanoprobe_api::{payload_preset, static_template};
use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};

use super::payload_preset as pp;
use super::*;

/// The two files, as paths under the profile with `/`, and the preset's
/// name.
#[derive(Clone, Debug)]
pub struct Saved {
    pub stm: String,
    pub preset: String,
    pub preset_name: String,
}

impl Saved {
    /// The folders the part's `sidecar.toml` lists: the two the editor
    /// saves to. Each file must sit directly in its kind's folder
    /// ([`crate::real_files::saved_path`]), so no other folder is listed.
    pub fn folders(&self) -> Result<[String; 2], String> {
        let check = |p: &str, dir: &str| {
            crate::real_files::saved_path(p)?;
            if p.starts_with(dir) {
                Ok(dir.trim_end_matches('/').to_owned())
            } else {
                Err(format!("{p:?} is not in {dir}"))
            }
        };
        Ok([
            check(&self.stm, "StaticTemplate/")?,
            check(&self.preset, "MissionEditor/UnitPayloads/")?,
        ])
    }
}

/// The DCS country of every entity, one the hello mission holds (LIVE,
/// docs/results/2026-11-18-spawn-template.md).
const COUNTRY: u32 = 2;
/// The layout's anchor: 300 m north of where the static-template part's
/// layout stood on land (LIVE, docs/results/2026-11-23-static-templates.md).
const ANCHOR: (f64, f64) = (-289_425.0, 680_035.0);
/// How far an object may be read from where the call placed it, meters, as
/// the static-template part allows.
const NEAR_M: f64 = 1.0;
/// How far a heading may be read from the one asked for, radians; recorded,
/// not judged, as in the static-template part.
const NEAR_RAD: f64 = 0.1;
/// Where the two aircraft fly: 3 km up, 2 km apart, as in the
/// payload-preset part.
const AT: [(f64, f64); 2] = [(-289_000.0, 680_000.0), (-289_000.0, 682_000.0)];
const ALT: f64 = 3000.0;
/// A helicopter flies lower and slower: 500 m up at 50 m/s, a test choice
/// within any helicopter's reach.
const HELI_ALT: f64 = 500.0;
const HELI_SPEED: f64 = 50.0;

/// The keys of an aircraft spawned in the air besides its payload: the
/// payload-preset part's for an airplane, slower for a helicopter.
fn flying(category: pb::GroupCategory) -> Value {
    if category == pb::GroupCategory::Helicopter {
        json!({ "speed": HELI_SPEED, "alt_type": "BARO" })
    } else {
        pp::flying()
    }
}

/// One object the static template makes, in the reader's order: every
/// group's units, then every static.
#[derive(Clone, Debug, PartialEq)]
struct Object {
    type_name: String,
    north: f64,
    east: f64,
    heading: f64,
    is_static: bool,
}

/// The objects the call makes from `src`, by the sidecar's own reader and
/// conversion, and a record of the file: its theater, countries, counts and
/// dropped keys.
fn layout(src: &[u8]) -> Result<(Vec<Object>, Vec<usize>, Value), String> {
    let l = static_template::read(src).map_err(|r| format!("the reader refused it: {}", r.text))?;
    let anchor = caucasus(ANCHOR.0, ANCHOR.1);
    let s = static_template::specs(&l, Some(&anchor), 0.0, Some(COUNTRY), "")
        .map_err(|r| format!("the conversion refused it: {}", r.text))?;
    let mut objects = Vec::new();
    for g in &s.groups {
        for u in &g.units {
            let p = u.position.clone().unwrap_or_default();
            objects.push(Object {
                type_name: u.r#type.clone(),
                north: p.north,
                east: p.east,
                heading: u.heading,
                is_static: false,
            });
        }
    }
    for x in &s.statics {
        let p = x.position.clone().unwrap_or_default();
        objects.push(Object {
            type_name: x.r#type.clone(),
            north: p.north,
            east: p.east,
            heading: x.heading,
            is_static: true,
        });
    }
    let units: Vec<usize> = s.groups.iter().map(|g| g.units.len()).collect();
    let mut countries: Vec<u32> = l
        .groups
        .iter()
        .map(|g| g.country)
        .chain(l.statics.iter().map(|s| s.country))
        .collect();
    countries.sort_unstable();
    countries.dedup();
    let record = json!({
        "theatre": l.theatre,
        "countries_in_file": countries,
        "groups": s.groups.len(),
        "units": units.iter().sum::<usize>(),
        "statics": s.statics.len(),
        "static_types": l.statics.iter().map(|s| s.type_name.clone()).collect::<Vec<_>>(),
        "static_extra_keys": l.statics.iter().map(|s| s.extra.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "dropped": l.dropped,
    });
    Ok((objects, units, record))
}

/// The verdict on the objects DCS returned, each a table with `x`, `z`,
/// the heading's `hx` and `hz`, `type` and `country`, or `false`.
fn layout_verdict(want: &[Object], seen: &Value) -> Value {
    let mut pass = seen.as_array().is_some_and(|a| a.len() == want.len()) && !want.is_empty();
    let mut objects = Vec::new();
    for (i, w) in want.iter().enumerate() {
        let u = &seen[i];
        let (x, z) = (u["x"].as_f64(), u["z"].as_f64());
        let heading = match (u["hx"].as_f64(), u["hz"].as_f64()) {
            (Some(hx), Some(hz)) => Some(hz.atan2(hx).rem_euclid(std::f64::consts::TAU)),
            _ => None,
        };
        let placed = matches!((x, z), (Some(x), Some(z))
            if (x - w.north).abs() <= NEAR_M && (z - w.east).abs() <= NEAR_M);
        let heading_near = heading.is_some_and(|got| {
            let d = (got - w.heading).rem_euclid(std::f64::consts::TAU);
            d.min(std::f64::consts::TAU - d) <= NEAR_RAD
        });
        let typed = u["type"] == json!(w.type_name) && u["country"] == json!(COUNTRY);
        pass &= placed && typed;
        objects.push(json!({
            "type_asked": w.type_name, "static": w.is_static,
            "want_north": w.north, "want_east": w.east, "want_heading": w.heading,
            "north": x, "east": z, "heading": heading,
            "placed": placed, "heading_near": heading_near, "typed": typed,
            "type": u["type"], "country": u["country"],
        }));
    }
    json!({ "pass": pass, "objects": objects })
}

/// Whether the answer holds the groups, units per group and statics the
/// reader found.
fn answer_ok(m: &pb::PutFromStaticTemplateResponse, units: &[usize], statics: usize) -> bool {
    m.groups.len() == units.len()
        && m.groups
            .iter()
            .zip(units)
            .all(|(g, n)| g.unit_ids.len() == *n)
        && m.statics.len() == statics
        && m.revision > 0
}

/// The DCS name of every made unit and static, in the order of
/// [`layout`]'s objects.
fn layout_names(c: u64, m: &pb::PutFromStaticTemplateResponse) -> Vec<String> {
    m.groups
        .iter()
        .flat_map(|g| g.unit_ids.iter())
        .map(|u| name(NameKind::Unit, CampaignId(c), EntityId(*u)))
        .chain(
            m.statics
                .iter()
                .map(|s| name(NameKind::Static, CampaignId(c), EntityId(s.static_id))),
        )
        .collect()
}

/// A read of every object: a unit by `Unit.getByName`, a static by
/// `StaticObject.getByName`.
fn layout_lua(names: &[String], statics: &[bool]) -> String {
    let objs: Vec<String> = names
        .iter()
        .zip(statics)
        .map(|(n, s)| {
            let get = if *s { "StaticObject" } else { "Unit" };
            format!("{{ {get}.getByName({n:?}) }}")
        })
        .collect();
    format!(
        "local out = {{}}\nfor i, o in ipairs({{ {} }}) do\n  local u = o[1]\n  if u then\n    \
         local p = u:getPosition()\n    out[i] = {{ x = p.p.x, z = p.p.z, hx = p.x.x, hz = p.x.z, type = u:getTypeName(), country = u:getCountry() }}\n  \
         else\n    out[i] = false\n  end\nend\nreturn out\n",
        objs.join(", ")
    )
}

/// A `google.protobuf.Struct` as JSON.
fn struct_json(s: &prost_types::Struct) -> Value {
    fn one(v: &prost_types::Value) -> Value {
        use prost_types::value::Kind as K;
        match &v.kind {
            None | Some(K::NullValue(_)) => Value::Null,
            Some(K::BoolValue(b)) => json!(b),
            Some(K::NumberValue(n)) => json!(n),
            Some(K::StringValue(s)) => json!(s),
            Some(K::ListValue(l)) => Value::Array(l.values.iter().map(one).collect()),
            Some(K::StructValue(s)) => struct_json(s),
        }
    }
    Value::Object(s.fields.iter().map(|(k, v)| (k.clone(), one(v))).collect())
}

/// Run the part; the result goes under `saved_files`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    saved: &Saved,
    full: &dyn Fn(&str) -> String,
    results: &mut Value,
) -> Result<(), String> {
    let stm = run
        .profile()
        .read(&saved.stm)?
        .ok_or_else(|| format!("no file {} in the profile", saved.stm))?;
    let preset = run
        .profile()
        .read(&saved.preset)?
        .ok_or_else(|| format!("no file {} in the profile", saved.preset))?;

    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);
    println!("== saved-files: a campaign on Caucasus");
    let c = api.create("saved-files-live", &["Caucasus"], true)?;
    loaded(run, &mut api, "Caucasus", generation)?;

    println!("== saved-files: the static template {}", saved.stm);
    let t = template_steps(run, &mut api, c, &stm, &full(&saved.stm))
        .unwrap_or_else(|e| json!({ "pass": false, "error": e }));
    println!(
        "   {} {}",
        if t["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        },
        t["objects"]
    );
    println!(
        "== saved-files: the preset {:?} in {}",
        saved.preset_name, saved.preset
    );
    let p = preset_steps(
        run,
        &mut api,
        c,
        &preset,
        &full(&saved.preset),
        &saved.preset_name,
    )
    .unwrap_or_else(|e| json!({ "pass": false, "error": e }));
    println!(
        "   {} {}",
        if p["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        },
        p["ammo"]
    );
    let pass = t["pass"] == json!(true) && p["pass"] == json!(true);
    results["saved_files"] = json!({
        "pass": pass,
        "static_template": t,
        "payload_preset": p,
    });
    Ok(())
}

fn template_steps(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    src: &[u8],
    path: &str,
) -> Result<Value, String> {
    let (want, units, record) = layout(src)?;
    let surface = run.eval(
        "server",
        "saved-files-surface",
        &format!(
            "local out = {{}}\nfor i, p in ipairs({{{}}}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n",
            want.iter()
                .map(|o| format!("{{{}, {}}}", o.north, o.east))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        60,
    )?;
    api.keep();
    let req = pb::PutFromStaticTemplateRequest {
        write: api.write("saved-files", c),
        path: path.to_owned(),
        anchor: Some(caucasus(ANCHOR.0, ANCHOR.1)),
        heading: 0.0,
        country: Some(COUNTRY),
        label: String::new(),
    };
    let m = api
        .rt
        .block_on(api.commands.put_from_static_template(req))
        .map(tonic::Response::into_inner)
        .map_err(status_text("PutFromStaticTemplate"))?;
    let names = layout_names(c, &m);
    let statics: Vec<bool> = want.iter().map(|o| o.is_static).collect();
    let read = (|| {
        for id in m
            .groups
            .iter()
            .map(|g| g.group_id)
            .chain(m.statics.iter().map(|s| s.static_id))
        {
            live(run, api, c, id)?;
        }
        let seen = run.eval(
            "server",
            "saved-files-read",
            &layout_lua(&names, &statics),
            60,
        )?;
        api.keep();
        Ok::<_, String>(seen)
    })();
    println!("== saved-files: remove the layout");
    let left = remove_layout(run, api, c, &m, &names, &statics);
    let seen = read?;
    let mut v = layout_verdict(&want, &seen);
    let answered = answer_ok(&m, &units, record["statics"].as_u64().unwrap_or(0) as usize);
    v["pass"] = json!(v["pass"] == json!(true) && answered && left.is_ok());
    v["answer_matches_the_reader"] = json!(answered);
    v["file"] = record;
    v["surface"] = surface;
    v["anchor"] =
        json!({ "north": ANCHOR.0, "east": ANCHOR.1, "heading": 0.0, "country": COUNTRY });
    v["dcs_names"] = json!(names);
    v["entities_left"] = match left {
        Ok(()) => json!(0),
        Err(e) => json!({ "error": e }),
    };
    Ok(v)
}

fn remove_layout(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    m: &pb::PutFromStaticTemplateResponse,
    names: &[String],
    statics: &[bool],
) -> Result<(), String> {
    api.keep();
    let mut sent = Ok(());
    for id in m
        .groups
        .iter()
        .map(|g| g.group_id)
        .chain(m.statics.iter().map(|s| s.static_id))
    {
        let req = pb::RemoveEntityRequest {
            write: api.write("remove", c),
            entity_id: id,
        };
        if let Err(e) = api
            .rt
            .block_on(api.commands.remove_entity(req))
            .map_err(status_text("RemoveEntity"))
        {
            sent = sent.and(Err(e));
        }
    }
    let read = layout_lua(names, statics);
    wait(
        run,
        api,
        Duration::from_secs(120),
        "the layout removed",
        |_| {
            let seen = run.eval("server", "saved-files-left", &read, 60)?;
            let left = seen
                .as_array()
                .map(|a| a.iter().filter(|o| *o != &json!(false)).count())
                .unwrap_or(usize::MAX);
            Ok((left == 0).then_some(()))
        },
    )?;
    sent
}

/// The group category of a DCS type, from its description (`Unit.Category`:
/// 0 an airplane, 1 a helicopter); an airplane when the read fails.
fn category(run: &dyn Live, unit_type: &str) -> (pb::GroupCategory, Value) {
    let seen = run
        .eval(
            "server",
            "saved-files-category",
            &format!(
                "local ok, d = pcall(Unit.getDescByName, {unit_type:?})\nif ok and type(d) == 'table' then return d.category end\nreturn -1\n"
            ),
            60,
        )
        .unwrap_or_else(|e| json!({ "error": e }));
    let c = if seen.as_i64() == Some(1) {
        pb::GroupCategory::Helicopter
    } else {
        pb::GroupCategory::Airplane
    };
    (c, seen)
}

fn aircraft(
    label: &str,
    unit_type: &str,
    category: pb::GroupCategory,
    at: (f64, f64),
    extra: Value,
    preset: Option<pb::PayloadPreset>,
) -> pb::GroupSpec {
    pb::GroupSpec {
        label: label.into(),
        country: COUNTRY,
        category: category as i32,
        units: vec![pb::UnitSpec {
            r#type: unit_type.into(),
            label: format!("{label}-0"),
            position: Some(pb::Position {
                alt: if category == pb::GroupCategory::Helicopter {
                    HELI_ALT
                } else {
                    ALT
                },
                ..caucasus(at.0, at.1)
            }),
            skill: "High".into(),
            extra: Some(pp::obj(extra)),
            payload_preset: preset,
            ..pb::UnitSpec::default()
        }],
        ..pb::GroupSpec::default()
    }
}

fn preset_steps(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    src: &[u8],
    path: &str,
    preset_name: &str,
) -> Result<Value, String> {
    let f = payload_preset::read(src.to_vec())
        .map_err(|r| format!("the reader refused the file: {}", r.text))?;
    let unit_type = f.unit_type.clone();
    let p = f
        .pylons(&unit_type, preset_name)
        .map_err(|r| format!("the preset was refused: {}", r.text))?;
    let pylons = struct_json(&p.pylons);
    let names_in_file: Vec<String> = f.names().map(|(n, _)| n.to_owned()).collect();
    let (cat, cat_read) = category(run, &unit_type);
    let mut hand = flying(cat);
    hand["payload"] = json!({ "pylons": pylons });
    let mut made = Vec::new();
    let read = (|| {
        for s in [
            aircraft(
                "np-saved-preset",
                &unit_type,
                cat,
                AT[0],
                flying(cat),
                Some(pb::PayloadPreset {
                    path: path.to_owned(),
                    name: preset_name.to_owned(),
                }),
            ),
            aircraft("np-saved-hand", &unit_type, cat, AT[1], hand.clone(), None),
        ] {
            made.push(pp::put(api, c, s).map_err(status_text("PutGroup"))?);
        }
        for m in &made {
            live(run, api, c, m.group_id)?;
        }
        let seen = run.eval(
            "server",
            "saved-files-ammo",
            &pp::read_lua(&pp::dcs_names(c, &made)),
            60,
        )?;
        api.keep();
        let units: Vec<u64> = made.iter().flat_map(|m| m.unit_ids.clone()).collect();
        let rows = pp::stored(api, c, &units)?;
        Ok::<_, String>((seen, rows))
    })();
    println!("== saved-files: remove the aircraft");
    let names = pp::dcs_names(c, &made);
    let left = pp::remove(run, api, c, &made, &names);
    let (seen, stored) = read?;
    let mut v = preset_verdict(&unit_type, &pylons, &seen, &stored);
    v["pass"] = json!(v["pass"] == json!(true) && left.is_ok());
    v["unit_type"] = json!(unit_type);
    v["category_read"] = cat_read;
    v["preset"] = json!(preset_name);
    v["presets_in_file"] = json!(names_in_file);
    v["dropped"] = json!(p.dropped);
    v["pylons_from_reader"] = pylons;
    v["dcs_names"] = json!(names);
    v["groups_left"] = match left {
        Ok(()) => json!(0),
        Err(e) => json!({ "error": e }),
    };
    Ok(v)
}

/// Both aircraft in DCS, of the type, with the same ammunition; at least one
/// weapon when the preset has a pylon; the preset aircraft's stored pylons
/// the reader's, and both stored rows the same.
fn preset_verdict(unit_type: &str, pylons: &Value, seen: &Value, stored: &[Value]) -> Value {
    let a = &seen[0];
    let b = &seen[1];
    let present = a.is_object() && b.is_object();
    let typed = a["type"] == json!(unit_type) && b["type"] == json!(unit_type);
    let has_pylons = pylons.as_object().is_some_and(|m| !m.is_empty());
    let armed = !has_pylons || pp::weapons(&a["ammo"]) > 0;
    let ammo_same = present && a["ammo"] == b["ammo"];
    let specs_same = stored.len() == 2 && stored[0] == stored[1];
    let as_read = stored
        .first()
        .is_some_and(|e| pp::sv(e["payload"]["pylons"].clone()) == pp::sv(pylons.clone()));
    json!({
        "pass": present && typed && ammo_same && armed && specs_same && as_read,
        "present": present,
        "typed": typed,
        "ammo_same": ammo_same,
        "armed": armed,
        "stored_extra_same": specs_same,
        "pylons_as_read": as_read,
        "ammo": [a["ammo"], b["ammo"]],
        "fuel": [a["fuel"], b["fuel"]],
        "seen": seen,
        "stored_extra": stored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STM: &str = r#"staticTemplate =
{
    ["theatre"] = "Syria",
    ["coalition"] =
    {
        ["red"] =
        {
            ["name"] = "red",
            ["country"] =
            {
                [1] =
                {
                    ["id"] = 0,
                    ["name"] = "Russia",
                    ["vehicle"] =
                    {
                        ["group"] =
                        {
                            [1] =
                            {
                                ["groupId"] = 1,
                                ["name"] = "G",
                                ["x"] = 100,
                                ["y"] = 200,
                                ["units"] =
                                {
                                    [1] = {["type"] = "BTR-80", ["unitId"] = 1, ["name"] = "U1", ["x"] = 100, ["y"] = 200, ["heading"] = 0.5, ["skill"] = "Average"},
                                    [2] = {["type"] = "BTR-80", ["unitId"] = 2, ["name"] = "U2", ["x"] = 120, ["y"] = 200, ["heading"] = 0, ["skill"] = "Average"},
                                },
                            },
                        },
                    },
                    ["static"] =
                    {
                        ["group"] =
                        {
                            [1] =
                            {
                                ["groupId"] = 2,
                                ["name"] = "S",
                                ["heading"] = 1,
                                ["x"] = 140,
                                ["y"] = 260,
                                ["dead"] = false,
                                ["units"] =
                                {
                                    [1] = {["type"] = "Tent", ["category"] = "Fortifications", ["unitId"] = 3, ["name"] = "S", ["x"] = 140, ["y"] = 260, ["heading"] = 1},
                                },
                            },
                        },
                    },
                },
            },
        },
    },
} -- end of staticTemplate
"#;

    #[test]
    fn the_layout_goes_to_the_anchor_under_the_one_country() {
        let (want, units, record) = layout(STM.as_bytes()).expect("layout");
        assert_eq!(units, vec![2]);
        assert_eq!(record["statics"], json!(1));
        assert_eq!(record["countries_in_file"], json!([0]));
        assert_eq!(record["theatre"], json!("Syria"));
        assert_eq!(want.len(), 3);
        assert!(!want[0].is_static && !want[1].is_static && want[2].is_static);
        // The origin is the box's middle, (120, 230): the first unit is 20 m
        // south and 30 m west of it.
        assert!((want[0].north - (ANCHOR.0 - 20.0)).abs() < 1e-9);
        assert!((want[0].east - (ANCHOR.1 - 30.0)).abs() < 1e-9);
        assert!((want[0].heading - 0.5).abs() < 1e-12);
        assert!((want[2].north - (ANCHOR.0 + 20.0)).abs() < 1e-9);
        assert_eq!(want[2].type_name, "Tent");
    }

    fn seen_for(want: &[Object]) -> Value {
        json!(want
            .iter()
            .map(|o| json!({ "x": o.north, "z": o.east, "hx": o.heading.cos(), "hz": o.heading.sin(), "type": o.type_name, "country": COUNTRY }))
            .collect::<Vec<_>>())
    }

    #[test]
    fn the_layout_verdict_needs_every_object_typed_and_placed() {
        let (want, _, _) = layout(STM.as_bytes()).expect("layout");
        assert_eq!(layout_verdict(&want, &seen_for(&want))["pass"], json!(true));
        let mut moved = seen_for(&want);
        moved[1]["x"] = json!(want[1].north + 2.0);
        assert_eq!(layout_verdict(&want, &moved)["pass"], json!(false));
        let mut gone = seen_for(&want);
        gone[2] = json!(false);
        assert_eq!(layout_verdict(&want, &gone)["pass"], json!(false));
        let mut short = seen_for(&want);
        short.as_array_mut().unwrap().pop();
        assert_eq!(layout_verdict(&want, &short)["pass"], json!(false));
        let mut recountried = seen_for(&want);
        recountried[2]["country"] = json!(0);
        assert_eq!(layout_verdict(&want, &recountried)["pass"], json!(false));
        let mut retyped = seen_for(&want);
        retyped[0]["type"] = json!("T-72B");
        assert_eq!(layout_verdict(&want, &retyped)["pass"], json!(false));
        assert_eq!(layout_verdict(&[], &json!([]))["pass"], json!(false));
    }

    #[test]
    fn the_answer_must_match_the_reader() {
        let m = pb::PutFromStaticTemplateResponse {
            groups: vec![pb::StaticTemplateGroup {
                group_id: 1,
                unit_ids: vec![2, 3],
                spec_revision: 1,
            }],
            statics: vec![pb::StaticTemplateStatic {
                static_id: 4,
                spec_revision: 1,
            }],
            revision: 5,
        };
        assert!(answer_ok(&m, &[2], 1));
        assert!(!answer_ok(&m, &[3], 1));
        assert!(!answer_ok(&m, &[2], 0));
        assert!(!answer_ok(&m, &[2, 1], 1));
    }

    #[test]
    fn the_folders_are_the_files_parents() {
        let s = Saved {
            stm: "StaticTemplate/a b.stm".into(),
            preset: "MissionEditor/UnitPayloads/F-16C_50.lua".into(),
            preset_name: "x".into(),
        };
        assert_eq!(
            s.folders().unwrap(),
            [
                "StaticTemplate".to_owned(),
                "MissionEditor/UnitPayloads".to_owned()
            ]
        );
        for (stm, preset) in [
            ("a.stm", "MissionEditor/UnitPayloads/F-16C_50.lua"),
            ("Config/a.stm", "MissionEditor/UnitPayloads/F-16C_50.lua"),
            (
                "StaticTemplate/../a.stm",
                "MissionEditor/UnitPayloads/F-16C_50.lua",
            ),
            ("StaticTemplate/a.stm", "Config/serverSettings.lua"),
            ("MissionEditor/UnitPayloads/x.lua", "StaticTemplate/a.stm"),
            ("StaticTemplate/a.stm", "/etc/x.lua"),
        ] {
            let bad = Saved {
                stm: stm.into(),
                preset: preset.into(),
                preset_name: "x".into(),
            };
            assert!(bad.folders().is_err(), "{stm} {preset}");
        }
    }

    #[test]
    fn the_preset_verdict_compares_with_the_reader() {
        let pylons = json!({ "1": { "CLSID": "{A}" } });
        let a = json!({ "type": "F-16C_50", "ammo": { "weapons.missiles.X": 1 } });
        let seen = json!([a, a]);
        let row = json!({ "speed": 180, "payload": { "pylons": pylons } });
        let v = preset_verdict("F-16C_50", &pylons, &seen, &[row.clone(), row.clone()]);
        assert_eq!(v["pass"], json!(true), "{v}");
        let other = json!({ "speed": 180, "payload": { "pylons": { "2": { "CLSID": "{A}" } } } });
        assert_eq!(
            preset_verdict("F-16C_50", &pylons, &seen, &[other.clone(), other])["pass"],
            json!(false)
        );
        // Gun rounds alone do not show the pylons were applied.
        let guns = json!({ "type": "F-16C_50", "ammo": { "weapons.shells.M61_20_HE": 500 } });
        assert_eq!(
            preset_verdict(
                "F-16C_50",
                &pylons,
                &json!([guns, guns]),
                &[row.clone(), row.clone()]
            )["pass"],
            json!(false)
        );
        let empty = json!([{ "type": "F-16C_50", "ammo": {} }, { "type": "F-16C_50", "ammo": {} }]);
        assert_eq!(
            preset_verdict("F-16C_50", &pylons, &empty, &[row.clone(), row.clone()])["pass"],
            json!(false)
        );
        assert_eq!(
            preset_verdict("F-16C", &pylons, &seen, &[row.clone(), row])["pass"],
            json!(false)
        );
    }

    #[test]
    fn a_struct_reads_back_as_json() {
        let v = json!({ "1": { "CLSID": "{A}", "settings": { "n": 2.0, "on": true } } });
        assert_eq!(struct_json(&pp::obj(v.clone())), v);
    }
}
