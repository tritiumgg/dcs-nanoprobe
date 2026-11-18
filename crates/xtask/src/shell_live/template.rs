//! `sidecar <name> --part template`: a group made from a group template file
//! on a running DCS (`PutGroupFromTemplate`; spec/world.md, "Group
//! templates").
//!
//! The run writes a one-unit template file of its own into the folder the
//! part's `sidecar.toml` lists, makes two groups from it at one anchor, the
//! second turned a quarter turn, reads each unit back from DCS by its
//! platform name, and removes the groups and the file. The unit's `dx` and
//! `dy` differ, so the read shows where the conversion puts each, and that
//! the turn moves both.

use std::f64::consts::FRAC_PI_2;

use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};

use super::*;

/// The file's name in the listed folder.
const FILE: &str = "np-live-templates.lua";
/// The template's name in the file.
const TEMPLATE: &str = "np live one";
/// The unit's offsets, meters. They differ, so a place tells them apart.
const DX: f64 = 120.0;
const DY: f64 = 50.0;
/// The anchor, north of the hello campaigns' start: 50 m south and 120 m
/// east of that start, where the turned unit would stand, is water (LIVE,
/// `land.getSurfaceType`; docs/results/2026-11-18-spawn-template.md).
const ANCHOR_NORTH_M: f64 = 200.0;

/// The anchor both groups are made at.
fn anchor() -> (f64, f64) {
    let (north, east) = crate::sidecar::HELLO_POINTS[0];
    (north + ANCHOR_NORTH_M, east)
}
/// How far a unit may be read from where the call placed it, meters. A
/// ground unit spawns where its table says; this allows for the read's
/// rounding only.
const NEAR_M: f64 = 1.0;
/// How far a heading may be read from the one asked for, radians: a vehicle
/// on a slope does not point exactly where it was told.
const NEAR_RAD: f64 = 0.1;

/// The file, in the form the Mission Editor writes.
fn file() -> String {
    format!(
        "templates = \n{{\n    [\"{TEMPLATE}\"] = \n    {{\n        [\"type\"] = \"vehicle\",\n        \
         [\"name\"] = \"{TEMPLATE}\",\n        [\"country\"] = 2,\n        [\"units\"] = \n        {{\n            \
         [1] = \n            {{\n                [\"dx\"] = {DX},\n                [\"dy\"] = {DY},\n                \
         [\"name\"] = \"M-113\",\n                [\"skill\"] = \"High\",\n                [\"heading\"] = 0,\n            \
         }}, -- end of [1]\n        }}, -- end of [\"units\"]\n    }}, -- end of [\"{TEMPLATE}\"]\n}} -- end of templates\n"
    )
}

/// Where each of the two units is expected: north, east and heading. The
/// first group keeps the file's layout, with `dx` north and `dy` east. The
/// second is turned a quarter turn clockwise: what was north of the anchor
/// is east of it, and what was east is south.
fn expected(north: f64, east: f64) -> [(f64, f64, f64); 2] {
    [
        (north + DX, east + DY, 0.0),
        (north - DY, east + DX, FRAC_PI_2),
    ]
}

/// The same two places with `dx` taken as east and `dy` as north, for the
/// record: where the units would stand if the axes were exchanged.
fn exchanged(north: f64, east: f64) -> [(f64, f64); 2] {
    [(north + DY, east + DX), (north - DX, east + DY)]
}

/// The verdict on what DCS returned for the two units: each a table with
/// `x`, `z`, the heading's `hx` and `hz`, `type` and `country`.
fn verdict(north: f64, east: f64, seen: &Value) -> Value {
    let mut units = Vec::new();
    let mut pass = true;
    for (i, (n, e, h)) in expected(north, east).into_iter().enumerate() {
        let u = &seen[i];
        let (x, z) = (u["x"].as_f64(), u["z"].as_f64());
        let heading = match (u["hx"].as_f64(), u["hz"].as_f64()) {
            (Some(hx), Some(hz)) => Some(hz.atan2(hx).rem_euclid(std::f64::consts::TAU)),
            _ => None,
        };
        let placed = matches!((x, z), (Some(x), Some(z)) if (x - n).abs() <= NEAR_M && (z - e).abs() <= NEAR_M);
        let (sn, se) = exchanged(north, east)[i];
        let swapped = matches!((x, z), (Some(x), Some(z)) if (x - sn).abs() <= NEAR_M && (z - se).abs() <= NEAR_M);
        let turned = heading.is_some_and(|got| {
            let d = (got - h).rem_euclid(std::f64::consts::TAU);
            d.min(std::f64::consts::TAU - d) <= NEAR_RAD
        });
        let typed = u["type"] == json!("M-113") && u["country"] == json!(2);
        pass &= placed && typed;
        units.push(json!({
            "want_north": n, "want_east": e, "want_heading": h,
            "north": x, "east": z, "heading": heading,
            "placed": placed, "at_the_exchanged_place": swapped, "heading_near": turned,
            "type": u["type"], "country": u["country"],
        }));
    }
    json!({ "pass": pass, "units": units })
}

fn put(
    api: &mut Api,
    campaign: u64,
    path: &str,
    at: pb::Position,
    heading: f64,
) -> Result<pb::PutGroupFromTemplateResponse, tonic::Status> {
    let req = pb::PutGroupFromTemplateRequest {
        write: api.write("template", campaign),
        path: path.to_owned(),
        template: TEMPLATE.into(),
        anchor: Some(at),
        heading,
        country: None,
        label: String::new(),
        order: None,
    };
    api.rt
        .block_on(api.commands.put_group_from_template(req))
        .map(tonic::Response::into_inner)
}

/// Run the part; the result goes under `template`.
pub fn drive(run: &Run, endpoint: &str, token: &str, results: &mut Value) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);
    let (north, east) = anchor();

    println!("== template: a campaign on Caucasus");
    let c = api.create("template-live", &["Caucasus"], true)?;
    loaded(run, &mut api, "Caucasus", generation)?;
    // Each expected place must be on land, or the read says nothing.
    let surface = run.eval(
        "server",
        "template-surface",
        &format!(
            "local out = {{}}\nfor i, p in ipairs({{{}}}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n",
            expected(north, east)
                .iter()
                .map(|(n, e, _)| format!("{{{n}, {e}}}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        60,
    )?;
    api.renew()?;
    if surface
        .as_array()
        .is_none_or(|a| a.len() != 2 || a.iter().any(|v| !matches!(v.as_u64(), Some(1 | 4 | 5))))
    {
        return Err(format!(
            "an expected place is not on land: surface types {surface}"
        ));
    }

    let on_profile = format!("{}/own/{FILE}", crate::sidecar::MISSIONS_DIR);
    run.remote.write(&on_profile, file().as_bytes())?;
    let path = format!("{TEMPLATE_DIR}/{FILE}");
    let out = steps(run, &mut api, c, &path, north, east);
    // The file goes whatever the steps did.
    let removed = run.remote.remove_file(&on_profile);
    let gone = run.remote.read(&on_profile).map(|f| f.is_none());
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
        v["units"]
    );
    results["template"] = v;
    Ok(())
}

fn steps(
    run: &Run,
    api: &mut Api,
    c: u64,
    path: &str,
    north: f64,
    east: f64,
) -> Result<Value, String> {
    println!("== template: a path outside the listed folder is refused");
    let outside = put(
        api,
        c,
        "/etc/nanoprobe/sidecar.toml",
        caucasus(north, east),
        0.0,
    );
    let refused = matches!(&outside, Err(s) if s.code() == tonic::Code::InvalidArgument
        && s.message().contains("is not a file in a folder"));
    let refusal = outside.err().map(|s| s.message().to_owned());

    println!("== template: two groups from {path}");
    let mut made = Vec::new();
    let read = spawn_and_read(run, api, c, path, north, east, &mut made);
    // The groups made go whatever the read did: the run leaves DCS as it
    // found it.
    println!("== template: remove the groups");
    let names = unit_names(c, &made);
    let left = remove(run, api, c, &made, &names);
    if let Err(e) = &left {
        println!("   FAIL the groups were not removed: {e}");
    }
    let seen = read?;
    let mut v = verdict(north, east, &seen);
    v["pass"] = json!(v["pass"] == json!(true) && refused && left.is_ok());
    v["outside_refused"] = json!(refused);
    v["outside_refusal"] = json!(refusal);
    v["anchor"] = json!({ "north": north, "east": east });
    v["unit_names"] = json!(names);
    v["seen"] = seen;
    v["units_left"] = json!(left.is_ok().then_some(0));
    Ok(v)
}

/// The DCS name of each made group's unit.
fn unit_names(c: u64, made: &[pb::PutGroupFromTemplateResponse]) -> Vec<String> {
    made.iter()
        .filter_map(|m| m.unit_ids.first())
        .map(|u| name(NameKind::Unit, CampaignId(c), EntityId(*u)))
        .collect()
}

/// Names as the items of a Lua list.
fn lua_list(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("{n:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Make the two groups, wait for each to be live, and read each unit from
/// DCS. Every group made is in `made`, also when a later step fails.
fn spawn_and_read(
    run: &Run,
    api: &mut Api,
    c: u64,
    path: &str,
    north: f64,
    east: f64,
    made: &mut Vec<pb::PutGroupFromTemplateResponse>,
) -> Result<Value, String> {
    for heading in [0.0, FRAC_PI_2] {
        made.push(
            put(api, c, path, caucasus(north, east), heading)
                .map_err(status_text("PutGroupFromTemplate"))?,
        );
    }
    for m in made.iter() {
        live(run, api, c, m.group_id)?;
    }
    let seen = run.eval(
        "server",
        "template-read",
        &format!(
            "local out = {{}}\nfor i, n in ipairs({{{}}}) do\n  local u = Unit.getByName(n)\n  if u then\n    \
             local p = u:getPosition()\n    out[i] = {{ x = p.p.x, z = p.p.z, hx = p.x.x, hz = p.x.z, type = u:getTypeName(), country = u:getCountry() }}\n  \
             else\n    out[i] = false\n  end\nend\nreturn out\n",
            lua_list(&unit_names(c, made))
        ),
        60,
    )?;
    api.keep();
    Ok(seen)
}

/// Remove every made group and hold the simulation running until DCS has
/// dropped their units. Every removal is tried; the first error is given.
fn remove(
    run: &Run,
    api: &mut Api,
    c: u64,
    made: &[pb::PutGroupFromTemplateResponse],
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
    let list = lua_list(names);
    wait(
        run,
        api,
        Duration::from_secs(120),
        "the groups removed",
        |_| {
            let left = run.eval(
                "server",
                "template-left",
                &format!(
                    "local n = 0\nfor _, name in ipairs({{{list}}}) do if Unit.getByName(name) then n = n + 1 end end\nreturn n\n"
                ),
                60,
            )?;
            Ok((left == json!(0)).then_some(()))
        },
    )?;
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(x: f64, z: f64, heading: f64) -> Value {
        json!({ "x": x, "z": z, "hx": heading.cos(), "hz": heading.sin(), "type": "M-113", "country": 2 })
    }

    #[test]
    fn the_file_is_one_the_sidecar_reads() {
        let t = nanoprobe_api::group_template::read(file().as_bytes(), TEMPLATE).expect("read");
        assert_eq!(t.country, Some(2));
        assert_eq!(t.units.len(), 1);
        assert_eq!((t.units[0].dx, t.units[0].dy), (DX, DY));
        assert_eq!(t.units[0].type_name, "M-113");
        assert!(t.dropped.is_empty());
        // The places the verdict expects are the ones the call's own
        // conversion gives.
        let (n, e) = anchor();
        for (heading, want) in [0.0, FRAC_PI_2].into_iter().zip(expected(n, e)) {
            let s = nanoprobe_api::group_template::spec(&t, &caucasus(n, e), heading, None, "")
                .expect("spec");
            let p = s.units[0].position.clone().expect("position");
            assert!((p.north - want.0).abs() < 1e-6, "{p:?}");
            assert!((p.east - want.1).abs() < 1e-6, "{p:?}");
            assert_eq!(s.units[0].heading, want.2);
        }
    }

    #[test]
    fn the_verdict_passes_where_the_call_places_and_fails_elsewhere() {
        let (n, e) = (-290_000.0, 680_000.0);
        let first = || unit(n + DX, e + DY, 0.0);
        let second = || unit(n - DY, e + DX, FRAC_PI_2);
        let good = json!([
            unit(n + DX, e + DY + 0.4, 0.02),
            unit(n - DY - 0.3, e + DX, FRAC_PI_2)
        ]);
        let v = verdict(n, e, &good);
        assert_eq!(v["pass"], json!(true), "{v}");
        assert_eq!(v["units"][0]["heading_near"], json!(true));
        assert_eq!(v["units"][1]["heading_near"], json!(true));
        assert_eq!(v["units"][0]["at_the_exchanged_place"], json!(false));
        // The axes exchanged: `dx` taken as east and `dy` as north.
        let swapped = json!([unit(n + DY, e + DX, 0.0), unit(n - DX, e + DY, FRAC_PI_2)]);
        let v = verdict(n, e, &swapped);
        assert_eq!(v["pass"], json!(false));
        assert_eq!(v["units"][0]["at_the_exchanged_place"], json!(true));
        assert_eq!(v["units"][1]["at_the_exchanged_place"], json!(true));
        for bad in [
            // A unit DCS does not hold, and a unit missing from the answer.
            json!([false, second()]),
            json!([first()]),
            // A place 2 m off.
            json!([unit(n + DX + 2.0, e + DY, 0.0), second()]),
            // `dy` dropped, and `dy` with the wrong sign.
            json!([unit(n + DX, e, 0.0), unit(n, e + DX, FRAC_PI_2)]),
            json!([unit(n + DX, e - DY, 0.0), unit(n + DY, e + DX, FRAC_PI_2)]),
            // `dx` with the wrong sign.
            json!([unit(n - DX, e + DY, 0.0), unit(n - DY, e - DX, FRAC_PI_2)]),
            // The turn made to the left.
            json!([first(), unit(n + DY, e - DX, FRAC_PI_2)]),
            // A wrong type.
            json!([
                json!({ "x": n + DX, "z": e + DY, "hx": 1.0, "hz": 0.0, "type": "M-1", "country": 2 }),
                second()
            ]),
        ] {
            assert_eq!(verdict(n, e, &bad)["pass"], json!(false), "{bad}");
        }
        // A heading read far off is recorded and does not fail the place.
        let v = verdict(n, e, &json!([unit(n + DX, e + DY, 1.0), second()]));
        assert_eq!(v["pass"], json!(true));
        assert_eq!(v["units"][0]["heading_near"], json!(false));
    }
}
