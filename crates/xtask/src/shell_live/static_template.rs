//! `sidecar <name> --part static-template`: the groups and statics of a
//! static template file on a running DCS (`PutFromStaticTemplate`;
//! spec/world.md, "Static templates").
//!
//! The run writes a static template of its own into the folder the part's
//! `sidecar.toml` lists: one ground group of two units and two statics, at
//! places whose offsets from the layout's origin differ on each axis. It
//! makes the layout twice, in place and at an anchor with a quarter turn,
//! reads every unit and static back from DCS by its platform name, and
//! removes everything and the file, also when a step fails.

use std::f64::consts::FRAC_PI_2;

use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};

use super::*;

/// The file's name in the listed folder.
const FILE: &str = "np-live-layout.stm";
/// The DCS country of every entity, one the hello mission holds (LIVE,
/// docs/results/2026-11-18-spawn-template.md).
const COUNTRY: u32 = 2;
/// The ground unit type both units have (LIVE, the same document).
const UNIT_TYPE: &str = "M-113";

/// One object of the file: its DCS type, mission `x` and `y`, heading, and
/// whether it is a static.
struct Object {
    type_name: &'static str,
    x: f64,
    y: f64,
    heading: f64,
    is_static: bool,
}

/// The layout, 200 m north of the hello campaigns' start, where the group
/// template's run found land (LIVE, the same document). The two statics are
/// fortifications in Eagle Dynamics' database (ED-CODE,
/// `Scripts/Database/db_countries.lua`, `default_Fortifications`). Every
/// offset from the origin differs on its two axes.
const OBJECTS: [Object; 4] = [
    Object {
        type_name: UNIT_TYPE,
        x: -289_700.0,
        y: 680_050.0,
        heading: 0.0,
        is_static: false,
    },
    Object {
        type_name: UNIT_TYPE,
        x: -289_690.0,
        y: 680_080.0,
        heading: 1.0,
        is_static: false,
    },
    Object {
        type_name: "FARP Tent",
        x: -289_760.0,
        y: 680_020.0,
        heading: 0.5,
        is_static: true,
    },
    Object {
        type_name: "FARP Fuel Depot",
        x: -289_730.0,
        y: 679_990.0,
        heading: 0.0,
        is_static: true,
    },
];

/// The layout's origin: the middle of x -289,760 to -289,690 and of y
/// 679,990 to 680,080.
const ORIGIN: (f64, f64) = (-289_725.0, 680_035.0);

/// The anchor of the second call: 300 m north of the origin.
const ANCHOR: (f64, f64) = (-289_425.0, 680_035.0);

/// How far an object may be read from where the call placed it, meters. An
/// object spawns where its table says; this allows for the read's rounding.
const NEAR_M: f64 = 1.0;
/// How far a heading may be read from the one asked for, radians: a vehicle
/// on a slope does not point exactly where it was told.
const NEAR_RAD: f64 = 0.1;

/// The file, in the form the Mission Editor writes (ED-CODE,
/// `MissionEditor/modules/me_mission.lua`, `unload_nonair_groups` and
/// `unload_static_groups`).
fn file() -> String {
    let unit = |i: usize, o: &Object| {
        format!(
            "                                    [{i}] = \n                                    {{\n\
             \x20                                       [\"skill\"] = \"Average\",\n\
             \x20                                       [\"type\"] = \"{}\",\n\
             \x20                                       [\"unitId\"] = {i},\n\
             \x20                                       [\"y\"] = {},\n\
             \x20                                       [\"x\"] = {},\n\
             \x20                                       [\"name\"] = \"np live unit {i}\",\n\
             \x20                                       [\"heading\"] = {},\n\
             \x20                                       [\"playerCanDrive\"] = false,\n\
             \x20                                   }}, -- end of [{i}]\n",
            o.type_name, o.y, o.x, o.heading
        )
    };
    let fixed = |i: usize, o: &Object| {
        format!(
            "                            [{i}] = \n                            {{\n\
             \x20                               [\"heading\"] = {h},\n\
             \x20                               [\"groupId\"] = {g},\n\
             \x20                               [\"hidden\"] = false,\n\
             \x20                               [\"units\"] = \n                                {{\n\
             \x20                                   [1] = \n                                    {{\n\
             \x20                                       [\"category\"] = \"Fortifications\",\n\
             \x20                                       [\"type\"] = \"{t}\",\n\
             \x20                                       [\"unitId\"] = {g},\n\
             \x20                                       [\"rate\"] = 100,\n\
             \x20                                       [\"y\"] = {y},\n\
             \x20                                       [\"x\"] = {x},\n\
             \x20                                       [\"name\"] = \"np live static {i}\",\n\
             \x20                                       [\"heading\"] = {h},\n\
             \x20                                   }}, -- end of [1]\n\
             \x20                               }}, -- end of [\"units\"]\n\
             \x20                               [\"y\"] = {y},\n\
             \x20                               [\"x\"] = {x},\n\
             \x20                               [\"name\"] = \"np live static {i}\",\n\
             \x20                               [\"dead\"] = false,\n\
             \x20                           }}, -- end of [{i}]\n",
            h = o.heading,
            g = 10 + i,
            t = o.type_name,
            y = o.y,
            x = o.x,
        )
    };
    let units: String = OBJECTS[..2]
        .iter()
        .enumerate()
        .map(|(i, o)| unit(i + 1, o))
        .collect();
    let statics: String = OBJECTS[2..]
        .iter()
        .enumerate()
        .map(|(i, o)| fixed(i + 1, o))
        .collect();
    format!(
        "staticTemplate = \n{{\n    [\"coalition\"] = \n    {{\n        [\"blue\"] = \n        {{\n\
         \x20           [\"name\"] = \"blue\",\n            [\"country\"] = \n            {{\n\
         \x20               [1] = \n                {{\n                    [\"id\"] = {COUNTRY},\n\
         \x20                   [\"name\"] = \"USA\",\n\
         \x20                   [\"vehicle\"] = \n                    {{\n                        [\"group\"] = \n                        {{\n\
         \x20                           [1] = \n                            {{\n\
         \x20                               [\"groupId\"] = 1,\n\
         \x20                               [\"units\"] = \n                                {{\n{units}\
         \x20                               }}, -- end of [\"units\"]\n\
         \x20                               [\"y\"] = {y},\n                                [\"x\"] = {x},\n\
         \x20                               [\"name\"] = \"np live group\",\n\
         \x20                           }}, -- end of [1]\n\
         \x20                       }}, -- end of [\"group\"]\n                    }}, -- end of [\"vehicle\"]\n\
         \x20                   [\"static\"] = \n                    {{\n                        [\"group\"] = \n                        {{\n{statics}\
         \x20                       }}, -- end of [\"group\"]\n                    }}, -- end of [\"static\"]\n\
         \x20               }}, -- end of [1]\n            }}, -- end of [\"country\"]\n        }}, -- end of [\"blue\"]\n\
         \x20   }}, -- end of [\"coalition\"]\n    [\"name\"] = \"np live layout\",\n\
         \x20   [\"theatre\"] = \"Caucasus\",\n    [\"desc\"] = \"made by xtask\\\n, removed after\",\n\
         }} -- end of staticTemplate\n",
        y = OBJECTS[0].y,
        x = OBJECTS[0].x,
    )
}

/// Where each object is expected after each call, as north, east and
/// heading: in place every number is the file's; at the anchor, turned a
/// quarter turn clockwise, what was north of the origin is east of the
/// anchor and what was east is south.
fn expected() -> [[(f64, f64, f64); 4]; 2] {
    let in_place = OBJECTS.each_ref().map(|o| (o.x, o.y, o.heading));
    let turned = OBJECTS.each_ref().map(|o| {
        let (dn, de) = (o.x - ORIGIN.0, o.y - ORIGIN.1);
        (ANCHOR.0 - de, ANCHOR.1 + dn, o.heading + FRAC_PI_2)
    });
    [in_place, turned]
}

/// The same places with the file's `x` taken as east and `y` as north, for
/// the record: where the objects would stand if the axes were exchanged.
fn exchanged() -> [[(f64, f64); 4]; 2] {
    let in_place = OBJECTS.each_ref().map(|o| (o.y, o.x));
    let turned = OBJECTS.each_ref().map(|o| {
        let (dn, de) = (o.y - ORIGIN.1, o.x - ORIGIN.0);
        (ANCHOR.0 - de, ANCHOR.1 + dn)
    });
    [in_place, turned]
}

/// The verdict on what DCS returned: for each call, each object as a table
/// with `x`, `z`, the heading's `hx` and `hz`, `type` and `country`.
fn verdict(seen: &Value) -> Value {
    let mut calls = Vec::new();
    let mut pass = true;
    for (c, want) in expected().into_iter().enumerate() {
        let mut objects = Vec::new();
        for (i, (n, e, h)) in want.into_iter().enumerate() {
            let u = &seen[c][i];
            let (x, z) = (u["x"].as_f64(), u["z"].as_f64());
            let heading = match (u["hx"].as_f64(), u["hz"].as_f64()) {
                (Some(hx), Some(hz)) => Some(hz.atan2(hx).rem_euclid(std::f64::consts::TAU)),
                _ => None,
            };
            let near = |a: f64, b: f64| (a - n).abs() <= NEAR_M && (b - e).abs() <= NEAR_M;
            let placed = matches!((x, z), (Some(x), Some(z)) if near(x, z));
            let (sn, se) = exchanged()[c][i];
            let swapped = matches!((x, z), (Some(x), Some(z)) if (x - sn).abs() <= NEAR_M && (z - se).abs() <= NEAR_M);
            let turned = heading.is_some_and(|got| {
                let d = (got - h).rem_euclid(std::f64::consts::TAU);
                d.min(std::f64::consts::TAU - d) <= NEAR_RAD
            });
            let typed = u["type"] == json!(OBJECTS[i].type_name) && u["country"] == json!(COUNTRY);
            pass &= placed && typed;
            objects.push(json!({
                "type_asked": OBJECTS[i].type_name, "static": OBJECTS[i].is_static,
                "want_north": n, "want_east": e, "want_heading": h,
                "north": x, "east": z, "heading": heading,
                "placed": placed, "at_the_exchanged_place": swapped, "heading_near": turned,
                "type": u["type"], "country": u["country"],
            }));
        }
        calls.push(json!(objects));
    }
    json!({ "pass": pass, "calls": calls })
}

fn put(
    api: &mut Api,
    campaign: u64,
    path: &str,
    anchor: Option<pb::Position>,
    heading: f64,
) -> Result<pb::PutFromStaticTemplateResponse, tonic::Status> {
    let req = pb::PutFromStaticTemplateRequest {
        write: api.write("static-template", campaign),
        path: path.to_owned(),
        anchor,
        heading,
        country: None,
        label: String::new(),
    };
    api.rt
        .block_on(api.commands.put_from_static_template(req))
        .map(tonic::Response::into_inner)
}

/// Run the part; the result goes under `static_template`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    results: &mut Value,
) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);

    println!("== static-template: a campaign on Caucasus");
    let c = api.create("static-template-live", &["Caucasus"], true)?;
    loaded(run, &mut api, "Caucasus", generation)?;
    // Each expected place must be on land, or the read says nothing.
    let places: Vec<String> = expected()
        .iter()
        .flatten()
        .map(|(n, e, _)| format!("{{{n}, {e}}}"))
        .collect();
    let surface = run.eval(
        "server",
        "static-template-surface",
        &format!(
            "local out = {{}}\nfor i, p in ipairs({{{}}}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n",
            places.join(", ")
        ),
        60,
    )?;
    api.renew()?;
    if surface.as_array().is_none_or(|a| {
        a.len() != places.len() || a.iter().any(|v| !matches!(v.as_u64(), Some(1 | 4 | 5)))
    }) {
        return Err(format!(
            "an expected place is not on land: surface types {surface}"
        ));
    }

    let on_profile = format!("{}/own/{FILE}", crate::sidecar::MISSIONS_DIR);
    run.profile().write(&on_profile, file().as_bytes())?;
    let path = run.template_path(FILE);
    let out = steps(run, &mut api, c, &path);
    // The file goes whatever the steps did.
    let removed = run.profile().remove_file(&on_profile);
    let gone = run.profile().read(&on_profile).map(|f| f.is_none());
    let mut v = out?;
    removed?;
    v["surface"] = surface;
    v["file_removed"] = json!(gone?);
    v["pass"] = json!(v["pass"] == json!(true) && v["file_removed"] == json!(true));
    println!(
        "   {} {}",
        if v["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        },
        v["calls"]
    );
    results["static_template"] = v;
    Ok(())
}

/// What the World service lists of the campaign right after a call: the ID
/// and spec revision of every group and static.
fn world(api: &mut Api, campaign: u64) -> Result<Value, String> {
    let mut s = api
        .rt
        .block_on(api.world.list_world(pb::ListWorldRequest {
            filter: Some(pb::WorldFilter {
                campaign_id: campaign,
                ..pb::WorldFilter::default()
            }),
        }))
        .map_err(status_text("ListWorld"))?
        .into_inner();
    let mut out = Vec::new();
    while let Some(page) = api
        .rt
        .block_on(s.message())
        .map_err(status_text("ListWorld"))?
    {
        for e in page.entities {
            match e.kind {
                Some(entity::Kind::Group(g)) => out.push(json!([g.id, g.spec_revision])),
                Some(entity::Kind::Static(g)) => out.push(json!([g.id, g.spec_revision])),
                _ => {}
            }
        }
    }
    Ok(json!(out))
}

fn steps(run: &dyn Live, api: &mut Api, c: u64, path: &str) -> Result<Value, String> {
    println!("== static-template: a path outside the listed folder is refused");
    let outside = put(api, c, &run.config_path(), None, 0.0);
    let refused = matches!(&outside, Err(s) if s.code() == tonic::Code::InvalidArgument
        && s.message().contains("is not a file in a folder"));
    let refusal = outside.err().map(|s| s.message().to_owned());

    println!("== static-template: the layout in place and at an anchor from {path}");
    let mut made = Vec::new();
    let mut commits = Vec::new();
    let read = spawn_and_read(run, api, c, path, &mut made, &mut commits);
    // What was made goes whatever the read did: the run leaves DCS as it
    // found it.
    println!("== static-template: remove everything");
    let names = dcs_names(c, &made);
    let left = remove(run, api, c, &made, &names);
    if let Err(e) = &left {
        println!("   FAIL the entities were not removed: {e}");
    }
    let seen = read?;
    let mut v = verdict(&seen);
    let listed = listed_ok(&made, &commits);
    v["pass"] = json!(v["pass"] == json!(true) && refused && listed && left.is_ok());
    v["outside_refused"] = json!(refused);
    v["outside_refusal"] = json!(refusal);
    v["all_listed_after_each_call"] = json!(listed);
    v["answers"] = json!(made
        .iter()
        .map(|m| json!({
            "groups": m.groups.iter().map(|g| json!({ "group_id": g.group_id, "unit_ids": g.unit_ids, "spec_revision": g.spec_revision })).collect::<Vec<_>>(),
            "statics": m.statics.iter().map(|s| json!({ "static_id": s.static_id, "spec_revision": s.spec_revision })).collect::<Vec<_>>(),
            "revision": m.revision,
        }))
        .collect::<Vec<_>>());
    v["world_after_each_call"] = json!(commits);
    v["origin"] = json!({ "north": ORIGIN.0, "east": ORIGIN.1 });
    v["anchor"] = json!({ "north": ANCHOR.0, "east": ANCHOR.1, "heading": FRAC_PI_2 });
    v["dcs_names"] = json!(names);
    v["seen"] = seen;
    v["entities_left"] = json!(left.is_ok().then_some(0));
    Ok(v)
}

/// Whether each call answered one group of two units and two statics, and
/// the World service listed every one of them, with its spec revision, at
/// once after the call. This does not show that the call wrote them in one
/// commit; the host tests show that a refused call writes none.
fn listed_ok(made: &[pb::PutFromStaticTemplateResponse], commits: &[Value]) -> bool {
    made.len() == 2
        && commits.len() == 2
        && made.iter().zip(commits).all(|(m, w)| {
            let listed = |id: u64, rev: u64| {
                w.as_array()
                    .is_some_and(|a| a.iter().any(|e| e == &json!([id, rev])))
            };
            m.groups.len() == 1
                && m.groups[0].unit_ids.len() == 2
                && m.statics.len() == 2
                && m.revision > 0
                && m.groups.iter().all(|g| listed(g.group_id, g.spec_revision))
                && m.statics
                    .iter()
                    .all(|s| listed(s.static_id, s.spec_revision))
        })
}

/// The DCS name of every made unit and static, call by call, in the order
/// of `OBJECTS`.
fn dcs_names(c: u64, made: &[pb::PutFromStaticTemplateResponse]) -> Vec<Vec<String>> {
    made.iter()
        .map(|m| {
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
        })
        .collect()
}

/// A read of every made object: a unit by `Unit.getByName`, a static by
/// `StaticObject.getByName`.
fn read_lua(names: &[Vec<String>]) -> String {
    let call = |i: usize, n: &str| {
        let get = if OBJECTS[i].is_static {
            "StaticObject"
        } else {
            "Unit"
        };
        format!("{{ {get}.getByName({n:?}) }}")
    };
    let calls: Vec<String> = names
        .iter()
        .map(|c| {
            let objs: Vec<String> = c.iter().enumerate().map(|(i, n)| call(i, n)).collect();
            format!("{{ {} }}", objs.join(", "))
        })
        .collect();
    format!(
        "local out = {{}}\nfor c, objs in ipairs({{ {} }}) do\n  out[c] = {{}}\n  for i, o in ipairs(objs) do\n    local u = o[1]\n    \
         if u then\n      local p = u:getPosition()\n      out[c][i] = {{ x = p.p.x, z = p.p.z, hx = p.x.x, hz = p.x.z, type = u:getTypeName(), country = u:getCountry() }}\n    \
         else\n      out[c][i] = false\n    end\n  end\nend\nreturn out\n",
        calls.join(", ")
    )
}

/// Make the layout twice, wait for every entity to be live, and read every
/// object from DCS. Every answer is in `made`, also when a later step fails.
fn spawn_and_read(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    path: &str,
    made: &mut Vec<pb::PutFromStaticTemplateResponse>,
    commits: &mut Vec<Value>,
) -> Result<Value, String> {
    let anchor = caucasus(ANCHOR.0, ANCHOR.1);
    for (at, heading) in [(None, 0.0), (Some(anchor), FRAC_PI_2)] {
        made.push(put(api, c, path, at, heading).map_err(status_text("PutFromStaticTemplate"))?);
        commits.push(world(api, c)?);
    }
    for m in made.iter() {
        for id in m
            .groups
            .iter()
            .map(|g| g.group_id)
            .chain(m.statics.iter().map(|s| s.static_id))
        {
            live(run, api, c, id)?;
        }
    }
    let seen = run.eval(
        "server",
        "static-template-read",
        &read_lua(&dcs_names(c, made)),
        60,
    )?;
    api.keep();
    Ok(seen)
}

/// Remove every made entity and hold the simulation running until DCS has
/// dropped them. Every removal is tried; the first error is given.
fn remove(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    made: &[pb::PutFromStaticTemplateResponse],
    names: &[Vec<String>],
) -> Result<(), String> {
    api.keep();
    let mut sent = Ok(());
    let ids = made.iter().flat_map(|m| {
        m.groups
            .iter()
            .map(|g| g.group_id)
            .chain(m.statics.iter().map(|s| s.static_id))
    });
    for id in ids {
        let req = pb::RemoveEntityRequest {
            write: api.write("remove", c),
            entity_id: id,
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
        "the entities removed",
        |_| {
            let seen = run.eval("server", "static-template-left", &read, 60)?;
            let left = seen
                .as_array()
                .map(|cs| {
                    cs.iter()
                        .flat_map(|c| c.as_array().cloned().unwrap_or_default())
                        .filter(|o| o != &json!(false))
                        .count()
                })
                .unwrap_or(usize::MAX);
            Ok((left == 0).then_some(()))
        },
    )?;
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(x: f64, z: f64, heading: f64, i: usize) -> Value {
        json!({ "x": x, "z": z, "hx": heading.cos(), "hz": heading.sin(), "type": OBJECTS[i].type_name, "country": COUNTRY })
    }

    /// A change to a good read.
    type Edit = dyn Fn(&mut Value);

    fn good() -> Value {
        json!(
            expected()
                .iter()
                .map(|c| c
                    .iter()
                    .enumerate()
                    .map(|(i, (n, e, h))| object(*n, *e, *h, i))
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>()
        )
    }

    #[test]
    fn the_file_is_one_the_sidecar_reads_and_places_as_the_verdict_expects() {
        let l = nanoprobe_api::static_template::read(file().as_bytes()).expect("read");
        assert_eq!(l.groups.len(), 1);
        assert_eq!(l.statics.len(), 2);
        assert_eq!(nanoprobe_api::static_template::origin(&l), ORIGIN);
        assert!(l.groups[0].units.iter().all(|u| u.type_name == UNIT_TYPE));
        assert_eq!(l.statics[0].type_name, "FARP Tent");
        assert_eq!(l.statics[1].type_name, "FARP Fuel Depot");
        let anchor = caucasus(ANCHOR.0, ANCHOR.1);
        for ((at, heading), want) in [(None, 0.0), (Some(&anchor), FRAC_PI_2)]
            .into_iter()
            .zip(expected())
        {
            let s =
                nanoprobe_api::static_template::specs(&l, at, heading, None, "").expect("specs");
            let got: Vec<(f64, f64, f64)> = s.groups[0]
                .units
                .iter()
                .map(|u| (u.position.clone().expect("p"), u.heading))
                .chain(
                    s.statics
                        .iter()
                        .map(|x| (x.position.clone().expect("p"), x.heading)),
                )
                .map(|(p, h)| (p.north, p.east, h))
                .collect();
            for (g, w) in got.iter().zip(&want) {
                assert!(
                    (g.0 - w.0).abs() < 1e-6 && (g.1 - w.1).abs() < 1e-6,
                    "{g:?} {w:?}"
                );
                assert!((g.2 - w.2).abs() < 1e-9, "{g:?} {w:?}");
            }
            assert!(s.groups.iter().all(|g| g.country == COUNTRY));
            assert!(s.statics.iter().all(|g| g.country == COUNTRY));
        }
    }

    #[test]
    fn every_offset_differs_on_its_two_axes() {
        for o in &OBJECTS {
            assert_ne!((o.x - ORIGIN.0).abs(), (o.y - ORIGIN.1).abs());
        }
    }

    #[test]
    fn the_verdict_passes_where_the_call_places_and_fails_elsewhere() {
        let v = verdict(&good());
        assert_eq!(v["pass"], json!(true), "{v}");
        assert_eq!(v["calls"][1][2]["heading_near"], json!(true));
        let mut swapped = good();
        for (c, places) in exchanged().iter().enumerate() {
            for (i, (n, e)) in places.iter().enumerate() {
                swapped[c][i]["x"] = json!(n);
                swapped[c][i]["z"] = json!(e);
            }
        }
        let v = verdict(&swapped);
        assert_eq!(v["pass"], json!(false));
        assert_eq!(v["calls"][1][3]["at_the_exchanged_place"], json!(true));
        let edits: [Box<Edit>; 5] = [
            Box::new(|v| v[0][2] = json!(false)),
            Box::new(|v| v[1][3]["x"] = json!(v[1][3]["x"].as_f64().unwrap() + 2.0)),
            Box::new(|v| v[1][0]["type"] = json!("M-1")),
            Box::new(|v| v[0][3]["country"] = json!(0)),
            // The turn made to the left.
            Box::new(|v| {
                let (n, e) = (OBJECTS[1].x - ORIGIN.0, OBJECTS[1].y - ORIGIN.1);
                v[1][1]["x"] = json!(ANCHOR.0 + e);
                v[1][1]["z"] = json!(ANCHOR.1 - n);
            }),
        ];
        for edit in edits {
            let mut bad = good();
            edit(&mut bad);
            assert_eq!(verdict(&bad)["pass"], json!(false), "{bad}");
        }
        let mut off = good();
        off[0][1]["hx"] = json!(0.0);
        off[0][1]["hz"] = json!(-1.0);
        let v = verdict(&off);
        assert_eq!(v["pass"], json!(true));
        assert_eq!(v["calls"][0][1]["heading_near"], json!(false));
    }

    #[test]
    fn every_entity_must_be_listed() {
        let m = pb::PutFromStaticTemplateResponse {
            groups: vec![pb::StaticTemplateGroup {
                group_id: 1,
                unit_ids: vec![2, 3],
                spec_revision: 1,
            }],
            statics: vec![
                pb::StaticTemplateStatic {
                    static_id: 4,
                    spec_revision: 1,
                },
                pb::StaticTemplateStatic {
                    static_id: 5,
                    spec_revision: 1,
                },
            ],
            revision: 9,
        };
        let all = json!([[1, 1], [4, 1], [5, 1]]);
        assert!(listed_ok(
            &[m.clone(), m.clone()],
            &[all.clone(), all.clone()]
        ));
        assert!(!listed_ok(
            &[m.clone(), m.clone()],
            &[all, json!([[1, 1], [4, 1]])]
        ));
    }
}
