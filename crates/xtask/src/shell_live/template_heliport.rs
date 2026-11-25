//! `sidecar <name> --part template-heliport --file FILE`: a static template
//! that holds a heliport static, spawned on a running DCS with
//! `PutFromStaticTemplate` (spec/world.md, "Static templates" and "Kinds").
//!
//! The run puts FILE, a static template the Mission Editor saved, into the
//! folder the part's `sidecar.toml` lists, and makes it in place. It also
//! makes the file's first heliport with `PutStatic`, from the same spec,
//! north of it: the twin. It reads each static back from DCS by its
//! platform name with `StaticObject.getByName` and `Airbase.getByName`, and
//! which of them `world.getAirbases` lists. It
//! sets a fuel stock under each heliport's name and reads it from DCS. It
//! records each static's state in the World, removes every entity and the
//! file, also when a step fails, and records what DCS still holds. The
//! template's heliport must go every way its twin goes.

use std::time::Duration;

use nanoprobe_api::static_template::{self, Layout};
use nanoprobe_sidecar::ids::{CampaignId, EntityId, NameKind, name};

use super::*;

/// The file's name in the listed folder.
const FILE: &str = "np-live-heliport.stm";
/// How far a static may stand from its place, in meters. As the
/// static-template part's tolerance; the call does not move a static.
const NEAR_M: f64 = 1.0;
/// The jet fuel the run stocks at each heliport's warehouse, in kg. DCS
/// must read back this amount, which an empty warehouse does not hold.
const FUEL_KG: f64 = 5_000.0;
/// How long the airbase and the stock may take to show. Not a design
/// limit; only when this test gives up.
const SHOW_LIMIT: Duration = Duration::from_secs(120);

/// Whether the file's `i`-th static makes an airbase: its `category` is
/// `Heliports`.
fn heliport(l: &Layout, i: usize) -> bool {
    l.statics[i]
        .extra
        .iter()
        .any(|(k, v)| k == "category" && matches!(v, Lv::String(s) if s.as_slice() == b"Heliports"))
}

/// A read of every static by `StaticObject.getByName` and `Airbase.getByName`,
/// and the names `world.getAirbases` lists among them.
fn read_lua(names: &[String]) -> String {
    let list: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
    format!(
        "local names = {{ {} }}\nlocal want = {{}}\nfor _, n in ipairs(names) do want[n] = true end\n\
         local out = {{ statics = {{}}, airbases = {{}}, listed = {{}} }}\n\
         for i, n in ipairs(names) do\n  local s = StaticObject.getByName(n)\n  \
         if s and s:isExist() then local p = s:getPoint() out.statics[i] = {{ x = p.x, z = p.z, type = s:getTypeName(), country = s:getCountry() }} else out.statics[i] = false end\n  \
         local a = Airbase.getByName(n)\n  \
         if a and a:isExist() then local p = a:getPoint() local d = a:getDesc() out.airbases[i] = {{ x = p.x, z = p.z, id = tonumber(a:getID()), category = d and d.category, type = a:getTypeName() }} else out.airbases[i] = false end\nend\n\
         for _, b in ipairs(world.getAirbases()) do local n = b:getName() if want[n] then out.listed[#out.listed + 1] = n end end\n\
         return out\n",
        list.join(", ")
    )
}

/// The verdict on DCS's read after the call: every static that is not a
/// heliport stands within [`NEAR_M`] of its place, as its type and country;
/// every heliport's airbase stands there as its type and is listed by
/// `world.getAirbases`, and so does its static, as its type and country,
/// when `StaticObject.getByName` finds one; no other static makes one.
fn verdict(l: &Layout, seen: &Value) -> Value {
    let mut pass = l.statics.len() == seen["statics"].as_array().map_or(0, Vec::len);
    let mut out = Vec::new();
    let listed = seen["listed"].as_array().cloned().unwrap_or_default();
    for (i, s) in l.statics.iter().enumerate() {
        let st = &seen["statics"][i];
        let ab = &seen["airbases"][i];
        let near = |v: &Value| {
            let (x, z) = (v["x"].as_f64(), v["z"].as_f64());
            matches!((x, z), (Some(x), Some(z)) if (x - s.x).hypot(z - s.y) <= NEAR_M)
        };
        let airbase = heliport(l, i);
        let as_type = |v: &Value| near(v) && v["type"] == json!(s.type_name);
        let as_static = |v: &Value| as_type(v) && v["country"] == json!(s.country);
        // A heliport's airbase stands where its static does, as its type.
        // `StaticObject.getByName` may not find a FARP (it is not in
        // `coalition.getStaticObjects`, LIVE, ADR 0032); when it does, the
        // static must stand there too, as its type.
        let stands = if airbase {
            as_type(ab) && (st == &json!(false) || as_static(st))
        } else {
            as_static(st)
        };
        let made_airbase = ab.is_object();
        let ok = stands
            && made_airbase == airbase
            && (!airbase || listed.iter().any(|n| n == &ab_name(seen, i)));
        pass &= ok;
        out.push(json!({ "type": s.type_name, "heliport": airbase, "stands": stands, "airbase": made_airbase, "ok": ok }));
    }
    json!({ "pass": pass, "statics": out })
}

/// The name `listed` holds for the `i`-th static: its own, filled in by the
/// driver under `names`.
fn ab_name(seen: &Value, i: usize) -> Value {
    seen["names"][i].clone()
}

/// What the World lists of the campaign's statics: ID, revision, state and
/// last error. The World service serves no airbases
/// (`crates/api/src/convert.rs`, the filter's kinds).
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
    let mut statics = Vec::new();
    while let Some(page) = api
        .rt
        .block_on(s.message())
        .map_err(status_text("ListWorld"))?
    {
        for e in page.entities {
            if let Some(entity::Kind::Static(g)) = e.kind {
                statics.push(json!({ "id": g.id, "spec_revision": g.spec_revision, "state": g.state, "last_error": g.last_error }));
            }
        }
    }
    Ok(json!(statics))
}

/// How far north of the file's first heliport the `PutStatic` twin stands,
/// in meters: clear of the file's objects, on the same ground.
const TWIN_NORTH_M: f64 = 60.0;

/// The one theater the part's mission folder holds a shell for: the
/// sidecar part writes `Caucasus.miz` only (`sidecar.rs`, `with_map`).
const THEATER: &str = "Caucasus";

/// The layout of FILE, its first heliport's index and its theater; refused
/// when FILE is not a static template, holds no `Heliports` static, or has
/// a `theatre` other than [`THEATER`]. `sidecar` checks FILE with it before
/// it touches the target.
pub fn layout(file: &[u8]) -> Result<(Layout, usize, String), String> {
    let layout = static_template::read(file).map_err(|r| r.text)?;
    let pad = (0..layout.statics.len())
        .find(|i| heliport(&layout, *i))
        .ok_or("the file holds no `Heliports` static")?;
    let theater = layout.theatre.clone().ok_or("the file has no `theatre`")?;
    if theater != THEATER {
        return Err(format!(
            "the file's theatre is `{theater}`; the part runs on {THEATER} only"
        ));
    }
    Ok((layout, pad, theater))
}

/// How the template's heliport `pad` and its twin, the last name, went: the
/// same World state, both present; the same airbase category and type, both
/// read; the same outcome of the removal; and every static of `l` that is
/// not a heliport removed. `checked` is [`check`]'s result, `left`
/// [`remove`]'s.
fn twins(l: &Layout, ids: &[u64], pad: usize, checked: &Value, left: &Value) -> Value {
    let twin = ids.len().saturating_sub(1);
    let gone_of = |i: usize| left["gone"][i].clone();
    let same_removal = gone_of(pad).is_boolean() && gone_of(pad) == gone_of(twin);
    let others_removed = (0..l.statics.len())
        .filter(|i| !heliport(l, *i))
        .all(|i| gone_of(i) == json!(true));
    let state_of = |i: usize| {
        checked["world"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|s| ids.get(i).is_some_and(|id| s["id"] == json!(id)))
            })
            .map(|s| s["state"].clone())
    };
    let same_state = matches!((state_of(pad), state_of(twin)), (Some(a), Some(b)) if a == b);
    let kind_of = |i: usize| {
        let a = &checked["seen"]["airbases"][i];
        (a["category"].clone(), a["type"].clone())
    };
    let same_airbase = kind_of(pad).0.is_number() && kind_of(pad) == kind_of(twin);
    json!({
        "pass": twin > pad && same_removal && others_removed && same_state && same_airbase,
        "same_state": same_state,
        "same_airbase": same_airbase,
        "same_removal": same_removal,
        "others_removed": others_removed,
    })
}

/// Run the part; the result goes under `template_heliport`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    file: &[u8],
    results: &mut Value,
) -> Result<(), String> {
    let (layout, pad, theater) = layout(file)?;
    // The file's statics, then the twin: the same heliport, given to
    // `PutStatic` as the template's spec, north of it.
    let mut all = layout.clone();
    let mut twin = layout.statics[pad].clone();
    twin.x += TWIN_NORTH_M;
    all.statics.push(twin);
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);

    println!("== template-heliport: a campaign on {theater}");
    let c = api.create("template-heliport-live", &[&theater], true)?;
    loaded(run, &mut api, &theater, generation)?;
    let places: Vec<String> = all
        .statics
        .iter()
        .map(|s| format!("{{{}, {}}}", s.x, s.y))
        .collect();
    let surface = run.eval(
        "server",
        "template-heliport-surface",
        &format!(
            "local out = {{}}\nfor i, p in ipairs({{{}}}) do out[i] = land.getSurfaceType({{ x = p[1], y = p[2] }}) end\nreturn out\n",
            places.join(", ")
        ),
        60,
    )?;
    api.renew()?;

    let on_profile = format!("{}/own/{FILE}", crate::sidecar::MISSIONS_DIR);
    run.profile().write(&on_profile, file)?;
    let path = run.template_path(FILE);
    let out = steps(run, &mut api, c, &path, &layout, pad, &all);
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
        v["dcs"]
    );
    results["template_heliport"] = v;
    Ok(())
}

fn steps(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    path: &str,
    l: &Layout,
    pad: usize,
    all: &Layout,
) -> Result<Value, String> {
    println!("== template-heliport: the file in place from {path}, and its heliport by PutStatic");
    let req = pb::PutFromStaticTemplateRequest {
        write: api.write("template-heliport", c),
        path: path.to_owned(),
        anchor: None,
        heading: 0.0,
        country: None,
        label: String::new(),
    };
    let made = api
        .rt
        .block_on(api.commands.put_from_static_template(req))
        .map_err(status_text("PutFromStaticTemplate"))?
        .into_inner();
    let mut ids: Vec<u64> = made.statics.iter().map(|s| s.static_id).collect();
    // The twin's spec is the template's own, moved north.
    let specs = static_template::specs(l, None, 0.0, None, "").map_err(|r| r.text)?;
    let mut spec = specs.statics[pad].clone();
    if let Some(p) = spec.position.as_mut() {
        p.north += TWIN_NORTH_M;
    }
    spec.label = "twin".into();
    let req = pb::PutStaticRequest {
        write: api.write("twin", c),
        static_id: 0,
        spec: Some(spec),
    };
    let twin = api
        .rt
        .block_on(api.commands.put_static(req))
        .map_err(status_text("PutStatic"));
    if let Ok(t) = &twin {
        ids.push(t.get_ref().static_id);
    }
    let names: Vec<String> = ids
        .iter()
        .map(|id| name(NameKind::Static, CampaignId(c), EntityId(*id)))
        .collect();
    let pads: Vec<String> = names
        .iter()
        .enumerate()
        .filter(|(i, _)| *i < all.statics.len() && heliport(all, *i))
        .map(|(_, n)| n.clone())
        .collect();
    let checked = if twin.is_ok() {
        check(run, api, c, all, THEATER, &names, &pads)
    } else {
        Err("the twin was not made".into())
    };
    println!("== template-heliport: remove everything");
    let left = remove(run, api, c, &ids, &names);
    if let Err(e) = &left {
        println!("   FAIL not removed: {e}");
    }
    let mut v = checked?;
    twin?;
    let left = left?;
    let t = twins(l, &ids, pad, &v, &left);
    v["pass"] = json!(
        v["pass"] == json!(true)
            && made.groups.is_empty()
            && made.statics.len() == l.statics.len()
            && t["pass"] == json!(true)
    );
    v["twins"] = t;
    v["heliports_removed"] = json!(pads.iter().all(|p| {
        names
            .iter()
            .position(|n| n == p)
            .is_some_and(|i| left["gone"][i] == json!(true))
    }));
    v["answer"] = json!({
        "groups": made.groups.len(),
        "statics": made.statics.iter().map(|s| json!({ "static_id": s.static_id, "spec_revision": s.spec_revision })).collect::<Vec<_>>(),
        "revision": made.revision,
    });
    v["dcs_names"] = json!(names);
    v["after_remove"] = left;
    Ok(v)
}

/// Wait until DCS shows every static and every heliport's airbase; then
/// stock each airbase and wait for DCS to read the stock. The sidecar sends
/// `WAREHOUSE_SET` only for an airbase whose warehouse the agent sampled
/// (spec/world.md, "Kinds"), so the stock in DCS shows the sidecar keeps
/// the airbase and its warehouse.
fn check(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    all: &Layout,
    theater: &str,
    names: &[String],
    pads: &[String],
) -> Result<Value, String> {
    let read = read_lua(names);
    let mut seen = wait(run, api, SHOW_LIMIT, "the statics and the airbases", |_| {
        let mut seen = run.eval("server", "template-heliport-read", &read, 60)?;
        seen["names"] = json!(names);
        Ok((verdict(all, &seen)["pass"] == json!(true)).then_some(seen))
    })
    .or_else(|e| {
        // The last read, for the record.
        let mut seen = run.eval("server", "template-heliport-read", &read, 60)?;
        seen["names"] = json!(names);
        seen["timed_out"] = json!(e);
        Ok::<Value, String>(seen)
    })?;
    let dcs = verdict(all, &seen);
    let mut stocked = Vec::new();
    for pad in pads {
        let req = pb::SetWarehouseStockRequest {
            write: api.write("stock", c),
            theater: theater.to_string(),
            airbase: pad.clone(),
            liquids: [(0u32, FUEL_KG)].into_iter().collect(),
            ..pb::SetWarehouseStockRequest::default()
        };
        let set = api
            .rt
            .block_on(api.commands.set_warehouse_stock(req))
            .map_err(status_text("SetWarehouseStock"));
        stocked.push(json!({ "airbase": pad, "set": set.as_ref().err() }));
    }
    let inv = format!(
        "local out = {{}}\nfor i, n in ipairs({{ {} }}) do local a = Airbase.getByName(n) out[i] = a and a:getWarehouse():getLiquidAmount(0) or false end\nreturn out\n",
        pads.iter()
            .map(|n| format!("{n:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let fuel = wait(run, api, SHOW_LIMIT, "the stock in DCS", |_| {
        let d = run.eval("server", "template-heliport-fuel", &inv, 60)?;
        let ok = d.as_array().is_some_and(|a| {
            a.len() == pads.len() && a.iter().all(|v| v.as_f64() == Some(FUEL_KG))
        });
        Ok(ok.then_some(d))
    });
    let fuel_ok = fuel.is_ok();
    let fuel = match fuel {
        Ok(d) => d,
        Err(e) => {
            json!({ "error": e, "last": run.eval("server", "template-heliport-fuel", &inv, 60).ok() })
        }
    };
    let world = world(api, c)?;
    seen.as_object_mut().map(|o| o.remove("names"));
    api.keep();
    Ok(json!({
        "pass": dcs["pass"] == json!(true) && fuel_ok
            && stocked.iter().all(|s| s["set"].is_null()),
        "dcs": dcs,
        "seen": seen,
        "world": world,
        "stocked": stocked,
        "fuel_in_dcs": fuel,
    }))
}

/// Remove every static, hold until DCS has dropped them and their airbases
/// or the limit passes, and read what DCS and the World still hold. `gone`
/// says, per name, that neither `StaticObject.getByName` nor
/// `Airbase.getByName` finds a live object and `world.getAirbases` does not
/// list it.
fn remove(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    ids: &[u64],
    names: &[String],
) -> Result<Value, String> {
    api.keep();
    let mut sent = Ok(());
    for id in ids {
        let req = pb::RemoveEntityRequest {
            write: api.write("remove", c),
            entity_id: *id,
        };
        if let Err(e) = api
            .rt
            .block_on(api.commands.remove_entity(req))
            .map_err(status_text("RemoveEntity"))
        {
            sent = sent.and(Err(e));
        }
    }
    let read = read_lua(names);
    let gone = |seen: &Value| -> Vec<bool> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                seen["statics"][i] == json!(false)
                    && seen["airbases"][i] == json!(false)
                    && !seen["listed"]
                        .as_array()
                        .is_some_and(|l| l.iter().any(|x| x == &json!(n)))
            })
            .collect()
    };
    let waited = wait(
        run,
        api,
        SHOW_LIMIT,
        "the statics and airbases gone",
        |_| {
            let seen = run.eval("server", "template-heliport-left", &read, 60)?;
            Ok(gone(&seen).iter().all(|g| *g).then_some(()))
        },
    );
    let seen = run.eval("server", "template-heliport-left", &read, 60)?;
    let w = world(api, c)?;
    sent?;
    Ok(json!({
        "gone": gone(&seen),
        "dcs": seen,
        "world_statics": w,
        "waited": waited.err(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: &str = "staticTemplate = { [\"theatre\"] = \"Caucasus\", [\"coalition\"] = { [\"blue\"] = { [\"country\"] = { [1] = { [\"id\"] = 2, [\"static\"] = { [\"group\"] = { \
        [1] = { [\"name\"] = \"F\", [\"units\"] = { [1] = { [\"type\"] = \"APFC fuel\", [\"category\"] = \"Fortifications\", [\"x\"] = 100, [\"y\"] = 200, [\"heading\"] = 0 } } }, \
        [2] = { [\"name\"] = \"P\", [\"units\"] = { [1] = { [\"type\"] = \"SINGLE_HELIPAD\", [\"category\"] = \"Heliports\", [\"x\"] = 100, [\"y\"] = 240, [\"heading\"] = 0 } } } \
        } } } } } } }";

    fn good() -> Value {
        json!({
            "names": ["np#1#s#1", "np#1#s#2"],
            "statics": [{ "x": 100.0, "z": 200.0, "type": "APFC fuel", "country": 2 }, false],
            "airbases": [false, { "x": 100.2, "z": 240.1, "id": 2, "category": 1, "type": "SINGLE_HELIPAD" }],
            "listed": ["np#1#s#2"],
        })
    }

    #[test]
    fn the_verdict_passes_a_standing_static_and_a_listed_airbase() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        assert!(!heliport(&l, 0) && heliport(&l, 1));
        assert_eq!(verdict(&l, &good())["pass"], json!(true));
    }

    /// `check`'s and `remove`'s results as run 2 recorded them: the
    /// fortification, the template's helipad and its twin.
    fn checked() -> (Value, Value) {
        (
            json!({
                "world": [{ "id": 1, "state": 3 }, { "id": 2, "state": 6 }, { "id": 3, "state": 6 }],
                "seen": { "airbases": [false, { "category": 1, "type": "SINGLE_HELIPAD" }, { "category": 1, "type": "SINGLE_HELIPAD" }] },
            }),
            json!({ "gone": [true, false, false] }),
        )
    }

    #[test]
    fn the_template_heliport_must_go_as_its_twin() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        let ids = [1, 2, 3];
        let (c, r) = checked();
        assert_eq!(twins(&l, &ids, 1, &c, &r)["pass"], json!(true));
        let mut cases = Vec::new();
        let (mut c2, r2) = checked();
        c2["world"] = json!([{ "id": 1, "state": 3 }, { "id": 3, "state": 6 }]);
        cases.push(("one state missing", c2, r2));
        let (mut c2, r2) = checked();
        c2["world"] = json!([]);
        cases.push(("both states missing", c2, r2));
        let (mut c2, r2) = checked();
        c2["world"][2]["state"] = json!(3);
        cases.push(("another state", c2, r2));
        let (mut c2, r2) = checked();
        c2["seen"]["airbases"][2]["category"] = json!(0);
        cases.push(("another airbase category", c2, r2));
        let (mut c2, r2) = checked();
        c2["seen"]["airbases"][2]["type"] = json!("FARP");
        cases.push(("another airbase type", c2, r2));
        let (mut c2, r2) = checked();
        c2["seen"]["airbases"] = json!([false, false, false]);
        cases.push(("no airbase on either side", c2, r2));
        let (c2, mut r2) = checked();
        r2["gone"][2] = json!(true);
        cases.push(("another removal", c2, r2));
        let (c2, mut r2) = checked();
        r2["gone"][0] = json!(false);
        cases.push(("the fortification left", c2, r2));
        for (what, c, r) in cases {
            assert_eq!(twins(&l, &ids, 1, &c, &r)["pass"], json!(false), "{what}");
        }
    }

    #[test]
    fn a_file_on_another_theater_or_without_a_heliport_is_refused_before_the_run() {
        assert!(layout(PAD.as_bytes()).is_ok());
        let other = PAD.replace("\"Caucasus\"", "\"Afghanistan\"");
        assert!(
            layout(other.as_bytes())
                .expect_err("theater")
                .contains("Caucasus only")
        );
        let none = PAD.replace("\"Heliports\"", "\"Fortifications\"");
        assert!(
            layout(none.as_bytes())
                .expect_err("heliport")
                .contains("no `Heliports`")
        );
    }

    #[test]
    fn the_verdict_passes_a_heliport_static_that_getbyname_finds() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        let mut v = good();
        v["statics"][1] = json!({ "x": 100.0, "z": 240.0, "type": "SINGLE_HELIPAD", "country": 2 });
        assert_eq!(verdict(&l, &v)["pass"], json!(true));
    }

    #[test]
    fn the_verdict_fails_what_is_missing_or_misplaced() {
        let l = static_template::read(PAD.as_bytes()).expect("read");
        let mut cases = Vec::new();
        let mut v = good();
        v["listed"] = json!([]);
        cases.push(("airbase not listed", v));
        let mut v = good();
        v["airbases"][1] = json!(false);
        cases.push(("no airbase", v));
        let mut v = good();
        v["statics"][0] = json!(false);
        cases.push(("static missing", v));
        let mut v = good();
        v["statics"][0]["z"] = json!(202.0);
        cases.push(("static 2 m off", v));
        let mut v = good();
        v["airbases"][1]["x"] = json!(102.0);
        cases.push(("airbase 2 m off", v));
        let mut v = good();
        v["statics"][0]["type"] = json!("other");
        cases.push(("wrong type", v));
        let mut v = good();
        v["statics"][0]["country"] = json!(80);
        cases.push(("wrong country", v));
        let mut v = good();
        v["airbases"][0] = json!({ "x": 100.0, "z": 200.0 });
        cases.push(("an airbase from a fortification", v));
        let mut v = good();
        v["airbases"][1]["type"] = json!("FARP");
        cases.push(("an airbase of another type", v));
        let mut v = good();
        v["statics"][1] = json!({ "x": 100.0, "z": 240.0, "type": "FARP", "country": 2 });
        cases.push(("a heliport static of another type", v));
        let mut v = good();
        v["statics"][1] = json!({ "x": 103.0, "z": 240.0, "type": "SINGLE_HELIPAD", "country": 2 });
        cases.push(("a heliport static 3 m off", v));
        for (what, v) in cases {
            assert_eq!(verdict(&l, &v)["pass"], json!(false), "{what}");
        }
    }
}
