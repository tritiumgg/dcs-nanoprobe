//! `sidecar <name> --part mission-content`: typed weather, briefing text and
//! images, mission files, a sound, pictures and a radio broadcast, against a
//! running DCS (spec/persistence.md, "Weather", "Briefing text", "Briefing
//! images", "Mission files"; spec/commands.md, "Layout: BROADCAST").
//!
//! The run makes a campaign on Caucasus, then:
//!
//! 1. `setup`: an own template, the shared Caucasus template with the
//!    `--sound-picture` resources packed (`np-tone.wav` and `np-picture.png`),
//!    is placed in the template folder and set; six files are set with
//!    `SetMissionFile` (a 880 Hz `np-tone.wav` that replaces the template's,
//!    a new 660 Hz `np-new.wav`, a blue and a red PNG, a kneeboard page for
//!    every aircraft and one for the UH-1H). Each answer is recorded and
//!    `ListMissionFiles` lists the six.
//! 2. `next`: a `SetNextMission` with the typed weather, the briefing text
//!    and the briefing images. The same request with rain at -5 degrees is
//!    refused first as `INVALID_FIELD`.
//! 3. `reload`: `Reload`, and the wait for the new mission and the campaign.
//! 4. `readback`: `env.mission` (weather, briefing keys, picture keys), the
//!    dictionary texts, `DCS.getMissionDescription()`, the wind at three
//!    altitudes and the temperature and pressure near the first airbase. Raw
//!    values are recorded; the units are not judged.
//! 5. `effects`: a sound, two pictures (by file name and by the mission's
//!    resource key) and a looped AM broadcast, each after a hold the operator
//!    can set with `NP_CONTENT_HOLD_S` (seconds, default 20).
//! 6. `broadcast-cases`: a second broadcast under the same name, a stop, and
//!    a stop of a name never started. The outcomes are recorded.
//! 7. `cleanup`: the broadcast stopped, the briefing images and the files
//!    removed, the template removed, a reload to the shared template, the
//!    pause state put back, and every file the run placed removed. Cleanup
//!    runs also when a step failed.
//!
//! Each eval request holds one unknown DCS call; a request may hold several
//! known reads. Every value DCS returns is recorded as JSON.

use sha2::{Digest, Sha256};

use super::drawing_layers::{recorded, step};
use super::static_replace::{PAUSED_LUA, restore_pause};
use super::*;

const THEATER: &str = "Caucasus";
/// The own template's name in the template folder.
const TEMPLATE_FILE: &str = "np-content-template.miz";
/// The prefix of the media files in the template folder, which the run
/// removes.
const FILE_PREFIX: &str = "np-content-";
const BROADCAST: &str = "np-bc";
/// The frequencies of the first and the second broadcast, hertz.
const FREQ_HZ: u64 = 251_000_000;
const FREQ_SECOND_HZ: u64 = 252_000_000;
/// The broadcast stands this high above the ground, meters.
const ABOVE_GROUND_M: f64 = 50.0;
/// How long the first picture stays, seconds.
const PICTURE_S: f64 = 15.0;
/// The default hold between visible actions, seconds.
const HOLD_DEFAULT_S: f64 = 20.0;

// ---- media -----------------------------------------------------------------

/// One file the run sets with `SetMissionFile`.
struct Media {
    name: &'static str,
    kind: pb::MissionFileKind,
    aircraft_type: &'static str,
    bytes: Vec<u8>,
}

/// The six files, in the order they are set.
fn media() -> Vec<Media> {
    let m = |name, kind, aircraft_type, bytes| Media {
        name,
        kind,
        aircraft_type,
        bytes,
    };
    let mission = pb::MissionFileKind::Mission;
    vec![
        // Replaces the template's 440 Hz `np-tone.wav`.
        m("np-tone.wav", mission, "", crate::miz::tone_wav(880.0, 0.5)),
        m("np-new.wav", mission, "", crate::miz::tone_wav(660.0, 2.0)),
        m(
            "np-blue.png",
            mission,
            "",
            crate::miz::solid_png(64, 64, [0, 0, 255]),
        ),
        m(
            "np-red.png",
            mission,
            "",
            crate::miz::solid_png(64, 64, [255, 0, 0]),
        ),
        m(
            "np-kb-all.png",
            pb::MissionFileKind::Kneeboard,
            "",
            crate::miz::solid_png(256, 256, [0, 255, 0]),
        ),
        m(
            "np-kb-uh1.png",
            pb::MissionFileKind::KneeboardType,
            "UH-1H",
            crate::miz::solid_png(256, 256, [255, 255, 0]),
        ),
    ]
}

/// The template's file for `media`, in the template folder.
fn local_name(name: &str) -> String {
    format!("{FILE_PREFIX}{name}")
}

/// A path on the profile, in the folder the sidecar lists.
fn on_profile(file: &str) -> String {
    format!("{}/own/{file}", crate::sidecar::MISSIONS_DIR)
}

/// The own template: the shared Caucasus template with the test resources.
fn template_bytes() -> Result<Vec<u8>, String> {
    let miz = crate::miz::generate(
        CAUCASUS,
        &crate::miz::Changes {
            resources: crate::miz::test_resources(),
            ..crate::miz::Changes::default()
        },
    )?;
    with_client_slots(&miz)
}

/// A player slot: one UH-1H with skill `Client`, a hot start on the ground at
/// Anapa-Vityazevo (north -4447, east 244022, ground 43 m), as the Mission
/// Editor writes a helicopter group (ED-CODE, `me_mission.lua`). A person
/// slots in to read the coalition's briefing and kneeboard and to tune its
/// radio; the platform does not manage the group.
fn client_group(
    country: u32,
    country_name: &str,
    id: u32,
    name: &str,
    north: f64,
    east: f64,
    callsign: &str,
) -> String {
    format!(
        r#"{{ [1] = {{ ["id"] = {country}, ["name"] = "{country_name}", ["helicopter"] = {{ ["group"] = {{ [1] = {{
["groupId"] = {id}, ["name"] = "{name}", ["task"] = "Transport", ["uncontrolled"] = false, ["hidden"] = false,
["x"] = {north}, ["y"] = {east}, ["frequency"] = 251, ["modulation"] = 0, ["communication"] = true, ["start_time"] = 0,
["tasks"] = {{}},
["route"] = {{ ["points"] = {{ [1] = {{ ["type"] = "TakeOffGroundHot", ["action"] = "From Ground Area Hot",
["x"] = {north}, ["y"] = {east}, ["alt"] = 43, ["alt_type"] = "BARO", ["speed"] = 0, ["ETA"] = 0,
["ETA_locked"] = true, ["speed_locked"] = true, ["formation_template"] = "",
["task"] = {{ ["id"] = "ComboTask", ["params"] = {{ ["tasks"] = {{}} }} }} }} }} }},
["units"] = {{ [1] = {{ ["type"] = "UH-1H", ["skill"] = "Client", ["unitId"] = {id}, ["name"] = "{name} 1",
["x"] = {north}, ["y"] = {east}, ["alt"] = 43, ["alt_type"] = "BARO", ["heading"] = 0, ["psi"] = 0, ["speed"] = 0,
["livery_id"] = "", ["onboard_num"] = "010", ["callsign"] = {callsign},
["payload"] = {{ ["pylons"] = {{}}, ["fuel"] = "631", ["flare"] = 60, ["chaff"] = 0, ["gun"] = 100 }} }} }},
}} }} }} }} }}"#
    )
}

/// The template with a blue and a red player slot (run 4: the person checks
/// each coalition's briefing, the kneeboard and the radio from an aircraft).
fn with_client_slots(miz: &[u8]) -> Result<Vec<u8>, String> {
    let mut m = shell::members(miz)?;
    let blue = client_group(
        2,
        "USA",
        9101,
        "NP Blue Huey",
        -4600.0,
        244_100.0,
        r#"{ [1] = 1, [2] = 1, [3] = 1, ["name"] = "Enfield11" }"#,
    );
    let red = client_group(0, "Russia", 9102, "NP Red Huey", -4300.0, 243_900.0, "101");
    for (name, data) in &mut m {
        if name != "mission" {
            continue;
        }
        let text =
            String::from_utf8(data.clone()).map_err(|_| "mission is not UTF-8".to_owned())?;
        let mut out = text.clone();
        for (side, group) in [("blue", &blue), ("red", &red)] {
            let empty = format!("[\"name\"] = \"{side}\",\n\t\t\t[\"country\"] = {{}},");
            if !out.contains(&empty) {
                return Err(format!(
                    "the template's {side} coalition has countries already"
                ));
            }
            out = out.replacen(
                &empty,
                &format!("[\"name\"] = \"{side}\",\n\t\t\t[\"country\"] = {group},"),
                1,
            );
        }
        *data = out.into_bytes();
    }
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (n, d) in &m {
        w.start_file(n.as_str(), o).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut w, d).map_err(|e| e.to_string())?;
    }
    Ok(w.finish().map_err(|e| e.to_string())?.into_inner())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn kind_name(kind: i32) -> String {
    pb::MissionFileKind::try_from(kind)
        .map_or_else(|_| kind.to_string(), |k| k.as_str_name().into())
}

fn file_json(f: &pb::MissionFile) -> Value {
    json!({
        "name": f.name,
        "kind": kind_name(f.kind),
        "aircraft_type": f.aircraft_type,
        "size_bytes": f.size_bytes,
        "sha256": hex(&f.sha256),
    })
}

/// Whether the sidecar's answer to a `SetMissionFile` describes `m`.
fn answer_matches(m: &Media, a: &pb::MissionFile) -> bool {
    a.name == m.name
        && a.kind == m.kind as i32
        && a.aircraft_type == m.aircraft_type
        && a.size_bytes == m.bytes.len() as u64
        && hex(&a.sha256) == sha_hex(&m.bytes)
}

// ---- requests --------------------------------------------------------------

type Precip = pb::Precipitation;

fn wind(speed_mps: f64, toward_deg: u32) -> Option<pb::Wind> {
    Some(pb::Wind {
        speed_mps,
        toward_deg,
    })
}

/// The typed weather of step 2. `precipitation` is snow, or rain for the
/// refused case.
fn conditions(precipitation: Precip) -> pb::Weather {
    pb::Weather {
        temperature_c: Some(-5.0),
        qnh_mmhg: Some(750.0),
        visibility_m: Some(30_000.0),
        wind_ground: wind(8.0, 270),
        wind_2000m: wind(15.0, 300),
        wind_8000m: wind(25.0, 320),
        ground_turbulence_mps: Some(2.0),
        clouds: Some(pb::Clouds {
            preset: pb::CloudPreset::None as i32,
            base_m: Some(1200.0),
            thickness_m: Some(800.0),
            density: Some(9),
            precipitation: precipitation as i32,
        }),
        fog: Some(pb::Fog {
            mode: pb::FogMode::Off as i32,
            manual: Vec::new(),
        }),
        dust: Some(pb::Dust {
            enabled: false,
            visibility_m: 0.0,
        }),
        halo: Some(pb::Halo {
            preset: pb::HaloPreset::Auto as i32,
            crystals: pb::HaloCrystals::Unspecified as i32,
        }),
    }
}

const SORTIE: &str = "NP content check";
const DESCRIPTION: &str = "Line one\nLine two: Привет, ñ";
const BLUE_TASK: &str = "Blue task text";
const RED_TASK: &str = "Red task text";
const NEUTRAL_TASK: &str = "Neutral task text";

fn briefing() -> pb::Briefing {
    pb::Briefing {
        sortie: Some(SORTIE.into()),
        description: Some(DESCRIPTION.into()),
        blue_task: Some(BLUE_TASK.into()),
        red_task: Some(RED_TASK.into()),
        neutral_task: Some(NEUTRAL_TASK.into()),
    }
}

fn list(files: &[&str]) -> Option<pb::ImageList> {
    Some(pb::ImageList {
        files: files.iter().map(|f| (*f).to_owned()).collect(),
    })
}

fn images() -> pb::BriefingImages {
    pb::BriefingImages {
        blue: list(&["np-blue.png", "np-picture.png"]),
        red: list(&["np-red.png"]),
        neutral: list(&[]),
        coalition_selection: list(&["np-blue.png"]),
    }
}

/// The `SetNextMission` of step 2.
/// The start of the content mission: 2016-01-15 12:00 in the mission's own
/// calendar. A winter date, so the editor's temperature range for the date
/// holds -5 degrees: run 1 set -5 on the template's June date and the sim
/// read 8.1 degrees at the ground.
const START_S: i64 = 1_452_859_200;

fn next_mission(precipitation: Precip) -> pb::NextMission {
    pb::NextMission {
        start: Some(prost_types::Timestamp {
            seconds: START_S,
            nanos: 0,
        }),
        conditions: Some(conditions(precipitation)),
        briefing: Some(briefing()),
        briefing_images: Some(images()),
        ..pb::NextMission::default()
    }
}

/// The `SetNextMission` of cleanup: every coalition's images removed.
fn next_without_images() -> pb::NextMission {
    pb::NextMission {
        briefing_images: Some(pb::BriefingImages {
            blue: list(&[]),
            red: list(&[]),
            neutral: list(&[]),
            coalition_selection: list(&[]),
        }),
        ..pb::NextMission::default()
    }
}

fn scope_all() -> i32 {
    pb::EffectScope::All as i32
}

fn sound(file: &str) -> pb::effect::Effect {
    pb::effect::Effect::Sound(pb::Sound {
        scope: scope_all(),
        target: 0,
        file: file.into(),
    })
}

/// A picture in the middle of the window, at half its size's unit: size 50,
/// centered, in percent of the window (the values of
/// docs/results/2026-11-15-s15-world-commands.md, ED-CODE).
fn picture(file: &str, seconds: f64) -> pb::effect::Effect {
    pb::effect::Effect::Picture(pb::Picture {
        scope: scope_all(),
        target: 0,
        file: file.into(),
        seconds,
        clear_view: true,
        size: 50.0,
        horizontal: 1,
        vertical: 1,
        size_units: 1,
    })
}

fn broadcast(at: &Airbase, frequency_hz: u64) -> pb::effect::Effect {
    pb::effect::Effect::Broadcast(pb::Broadcast {
        name: BROADCAST.into(),
        file: "np-new.wav".into(),
        position: Some(at.position(ABOVE_GROUND_M)),
        frequency_hz,
        modulation: pb::RadioModulation::Am as i32,
        r#loop: true,
        power_w: Some(1000.0),
    })
}

fn broadcast_stop(name: &str) -> pb::effect::Effect {
    pb::effect::Effect::BroadcastStop(pb::BroadcastStop { name: name.into() })
}

/// The hold between visible actions, from `NP_CONTENT_HOLD_S`.
fn hold_s() -> f64 {
    hold_from(std::env::var("NP_CONTENT_HOLD_S").ok().as_deref())
}

fn hold_from(value: Option<&str>) -> f64 {
    value
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s >= 0.0)
        .unwrap_or(HOLD_DEFAULT_S)
}

// ---- reads -----------------------------------------------------------------

/// The first airbase of the map, as the server state lists it.
#[derive(Clone)]
struct Airbase {
    name: String,
    x: f64,
    z: f64,
    ground: f64,
}

impl Airbase {
    fn from_value(v: &Value) -> Option<Airbase> {
        Some(Airbase {
            name: v["name"].as_str()?.to_owned(),
            x: v["x"].as_f64()?,
            z: v["z"].as_f64()?,
            ground: v["ground"].as_f64()?,
        })
    }

    /// The airbase's point, `above` meters over the ground; `alt` is above
    /// sea level.
    fn position(&self, above: f64) -> pb::Position {
        pb::Position {
            theater: THEATER.into(),
            north: self.x,
            east: self.z,
            alt: self.ground + above,
        }
    }
}

/// Known reads of the first airbase: its name, point and ground height.
const AIRBASE_LUA: &str = "local a = world.getAirbases()[1]\nlocal p = a:getPoint()\nreturn { name = a:getName(), id = a:getID(), x = p.x, z = p.z, ground = land.getHeight({ x = p.x, y = p.z }) }\n";

/// `env.mission`'s weather, briefing and picture fields (PRIOR that
/// `env.mission` is there).
const MISSION_LUA: &str = "local m = env.mission\nlocal w = m.weather or {}\nreturn {\n  weather = {\n    temperature = w.season and w.season.temperature,\n    qnh = w.qnh,\n    visibility = w.visibility and w.visibility.distance,\n    atGround = w.wind and w.wind.atGround,\n    at2000 = w.wind and w.wind.at2000,\n    at8000 = w.wind and w.wind.at8000,\n    groundTurbulence = w.groundTurbulence,\n    clouds = w.clouds,\n    fog2 = w.fog2,\n    enable_fog = w.enable_fog,\n    enable_dust = w.enable_dust,\n    halo = w.halo,\n    atmosphere_type = w.atmosphere_type,\n  },\n  date = m.date,\n  start_time = m.start_time,\n  sortie = m.sortie,\n  descriptionText = m.descriptionText,\n  descriptionBlueTask = m.descriptionBlueTask,\n  descriptionRedTask = m.descriptionRedTask,\n  descriptionNeutralsTask = m.descriptionNeutralsTask,\n  pictureFileNameB = m.pictureFileNameB,\n  pictureFileNameR = m.pictureFileNameR,\n  pictureFileNameN = m.pictureFileNameN,\n  pictureFileNameServer = m.pictureFileNameServer,\n  maxDictId = m.maxDictId,\n}\n";

/// The dictionary texts of two keys (PRIOR).
fn dict_lua(sortie_key: &str, description_key: &str) -> String {
    format!(
        "return {{ sortie = env.getValueDictByKey({sortie_key:?}), description = env.getValueDictByKey({description_key:?}) }}\n"
    )
}

/// The mission description in the GUI state (PRIOR).
const DESCRIPTION_LUA: &str =
    "local v = DCS.getMissionDescription()\nreturn { type = type(v), value = v }\n";

/// The wind at a point (PRIOR).
fn wind_lua(x: f64, y: f64, z: f64) -> String {
    format!("return atmosphere.getWind({{ x = {x}, y = {y}, z = {z} }})\n")
}

/// The temperature and pressure at a point (PRIOR).
fn temperature_lua(x: f64, y: f64, z: f64) -> String {
    format!(
        "local t, p = atmosphere.getTemperatureAndPressure({{ x = {x}, y = {y}, z = {z} }})\nreturn {{ temperature = t, pressure = p }}\n"
    )
}

/// One eval request, recorded: the value, or the error text.
fn read(run: &dyn Live, state: &str, stem: &str, source: &str) -> Value {
    match run.eval(state, stem, source, 60) {
        Ok(v) => json!({ "ok": true, "value": v }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

/// A Lua list as JSON gives it, in order: an array, or an object keyed by
/// the numbers 1, 2, ...
fn list_of(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => {
            let mut keyed: Vec<(i64, &Value)> = o
                .iter()
                .filter_map(|(k, v)| k.parse().ok().map(|n| (n, v)))
                .collect();
            keyed.sort_by_key(|(n, _)| *n);
            keyed.into_iter().map(|(_, v)| v.clone()).collect()
        }
        _ => Vec::new(),
    }
}

fn near(v: &Value, want: f64) -> bool {
    v.as_f64().is_some_and(|x| (x - want).abs() < 1e-6)
}

fn is_key(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.starts_with("DictKey_"))
}

/// The checks on `env.mission` as the shell wrote it. The first group gates
/// the step: values the spec states in the mission's own units and keys,
/// including the wind's toward direction and the ground turbulence at ten
/// times the editor's value (LIVE, docs/results/2026-11-26-mission-content.md).
/// The second is recorded only: `env.mission` held them as written in run 1,
/// but whether the sim draws the preset and the halo it names is UNVERIFIED,
/// so a change there is reported and does not fail the step.
fn mission_checks(seen: &Value) -> Value {
    let w = &seen["weather"];
    let count = |key: &str| list_of(&seen[key]).len();
    let gated = [
        ("temperature_-5", near(&w["temperature"], -5.0)),
        ("qnh_750", near(&w["qnh"], 750.0)),
        ("visibility_30000", near(&w["visibility"], 30_000.0)),
        ("wind_ground_8", near(&w["atGround"]["speed"], 8.0)),
        ("wind_2000_15", near(&w["at2000"]["speed"], 15.0)),
        ("wind_8000_25", near(&w["at8000"]["speed"], 25.0)),
        ("wind_ground_dir_270", near(&w["atGround"]["dir"], 270.0)),
        ("wind_2000_dir_300", near(&w["at2000"]["dir"], 300.0)),
        ("wind_8000_dir_320", near(&w["at8000"]["dir"], 320.0)),
        ("turbulence_20", near(&w["groundTurbulence"], 20.0)),
        ("cloud_base_1200", near(&w["clouds"]["base"], 1200.0)),
        (
            "cloud_thickness_800",
            near(&w["clouds"]["thickness"], 800.0),
        ),
        ("cloud_density_9", near(&w["clouds"]["density"], 9.0)),
        ("precipitation_snow", near(&w["clouds"]["iprecptns"], 3.0)),
        ("static_atmosphere", near(&w["atmosphere_type"], 0.0)),
        ("legacy_fog_off", w["enable_fog"] == json!(false)),
        ("fog2_absent", w["fog2"].is_null()),
        ("dust_off", w["enable_dust"] == json!(false)),
        ("sortie_is_a_key", is_key(&seen["sortie"])),
        ("description_is_a_key", is_key(&seen["descriptionText"])),
        ("blue_task_is_a_key", is_key(&seen["descriptionBlueTask"])),
        ("red_task_is_a_key", is_key(&seen["descriptionRedTask"])),
        (
            "neutral_task_is_a_key",
            is_key(&seen["descriptionNeutralsTask"]),
        ),
        ("blue_has_2_images", count("pictureFileNameB") == 2),
        ("red_has_1_image", count("pictureFileNameR") == 1),
        ("neutral_has_no_image", count("pictureFileNameN") == 0),
        ("selection_has_1_image", count("pictureFileNameServer") == 1),
    ];
    let recorded_only = [
        ("cloud_preset_absent", w["clouds"]["preset"].is_null()),
        ("halo_preset_auto", w["halo"]["preset"] == json!("auto")),
    ];
    let row = |(name, ok): &(&str, bool)| json!({ "check": name, "ok": ok });
    json!({
        "pass": gated.iter().all(|(_, ok)| *ok),
        "gated": gated.iter().map(row).collect::<Vec<_>>(),
        "recorded_only": recorded_only.iter().map(row).collect::<Vec<_>>(),
    })
}

/// Whether the dictionary read returned the texts the run set. Only a read
/// that returned strings can fail; an error is recorded where it stands.
fn dict_matches(dict: &Value) -> Option<bool> {
    let v = &dict["value"];
    let (s, d) = (v["sortie"].as_str()?, v["description"].as_str()?);
    Some(s == SORTIE && d == DESCRIPTION)
}

// ---- the run ---------------------------------------------------------------

/// What the steps pass on to each other and what cleanup has to undo.
#[derive(Default)]
struct Ctx {
    files_set: Vec<usize>,
    template_set: bool,
    next_set: bool,
    reloaded: bool,
    /// The broadcast `np-bc` may be playing.
    broadcast: bool,
    airbase: Option<Airbase>,
    picture_key: Option<String>,
    /// The paths the run wrote in the template folder.
    placed: Vec<String>,
}

impl Ctx {
    fn place(&mut self, run: &dyn Live, file: &str, bytes: &[u8]) -> Result<String, String> {
        let path = on_profile(file);
        // Recorded first: a write that fails half-way is still removed.
        self.placed.push(path.clone());
        run.profile().write(&path, bytes)?;
        Ok(run.template_path(file))
    }
}

/// Run the part; the result goes under `mission_content`.
pub fn drive(
    run: &dyn Live,
    endpoint: &str,
    token: &str,
    results: &mut Value,
) -> Result<(), String> {
    let mut api = Api::connect(endpoint, token)?;
    let generation = api.status()?.now.map_or(0, |n| n.mission_generation);
    let found_paused = run.eval("hook", "mission-content-paused", PAUSED_LUA, 60)?;
    println!("== mission-content: a campaign on {THEATER}");
    let c = api.create("mission-content-live", &[THEATER], true)?;
    loaded(run, &mut api, THEATER, generation)?;
    api.renew()?;

    let mut v = json!({ "steps": {} });
    let mut ctx = Ctx::default();
    let ran = steps(run, &mut api, c, &mut ctx, &mut v);
    println!("== mission-content: cleanup");
    let cleanup = recorded(cleanup(run, &mut api, c, &mut ctx));
    v["steps"]["cleanup"] = cleanup;
    let restored = recorded(restore_pause(run, &mut api, &found_paused));
    v["restore_pause"] = restored;
    v["pause_found"] = found_paused;
    if let Err(e) = &ran {
        v["error"] = json!(e);
    }
    v["pass"] = json!(
        ran.is_ok()
            && v["restore_pause"]["pass"] == json!(true)
            && v["steps"]
                .as_object()
                .is_some_and(|o| o.values().all(|s| s["pass"] == json!(true)))
    );
    println!(
        "   {}",
        if v["pass"] == json!(true) {
            "ok  "
        } else {
            "FAIL"
        }
    );
    results["mission_content"] = v;
    Ok(())
}

fn steps(
    run: &dyn Live,
    api: &mut Api,
    c: u64,
    ctx: &mut Ctx,
    rec: &mut Value,
) -> Result<(), String> {
    println!("== mission-content: own template and six files");
    step(rec, "setup", setup(run, api, c, ctx))?;
    println!("== mission-content: next mission with weather, briefing and images");
    step(rec, "next", next(api, c, ctx))?;
    println!("== mission-content: reload");
    step(rec, "reload", reload(run, api, c, ctx))?;
    println!("== mission-content: read back");
    step(rec, "readback", readback(run, api, ctx))?;
    println!("== mission-content: sound, pictures and a broadcast");
    step(rec, "effects", effects(run, api, c, ctx))?;
    println!("== mission-content: broadcast cases");
    step(rec, "broadcast-cases", broadcast_cases(run, api, c, ctx))?;
    Ok(())
}

fn set_file(
    api: &mut Api,
    c: u64,
    m: &Media,
    path: &str,
) -> Result<Option<pb::MissionFile>, String> {
    let req = pb::SetMissionFileRequest {
        write: api.write("file", c),
        name: m.name.into(),
        kind: m.kind as i32,
        aircraft_type: m.aircraft_type.into(),
        path: path.into(),
    };
    Ok(api
        .rt
        .block_on(api.mission.set_mission_file(req))
        .map_err(status_text("SetMissionFile"))?
        .into_inner()
        .file)
}

fn set_template(api: &mut Api, c: u64, path: &str) -> Result<(), String> {
    let req = pb::SetCampaignTemplateRequest {
        write: api.write("template", c),
        theater: THEATER.into(),
        path: path.into(),
    };
    api.rt
        .block_on(api.campaigns.set_campaign_template(req))
        .map_err(status_text("SetCampaignTemplate"))?;
    Ok(())
}

fn list_files(api: &mut Api, c: u64) -> Result<Vec<pb::MissionFile>, String> {
    Ok(api
        .rt
        .block_on(
            api.mission
                .list_mission_files(pb::ListMissionFilesRequest { campaign_id: c }),
        )
        .map_err(status_text("ListMissionFiles"))?
        .into_inner()
        .files)
}

fn setup(run: &dyn Live, api: &mut Api, c: u64, ctx: &mut Ctx) -> Result<Value, String> {
    let template = template_bytes()?;
    let path = ctx.place(run, TEMPLATE_FILE, &template)?;
    set_template(api, c, &path)?;
    ctx.template_set = true;
    let all = media();
    let mut answers = Vec::new();
    for (i, m) in all.iter().enumerate() {
        let local = local_name(m.name);
        let path = ctx.place(run, &local, &m.bytes)?;
        // Recorded before the call: a call that fails half-way is undone.
        ctx.files_set.push(i);
        let answer = set_file(api, c, m, &path)?;
        let matches = answer.as_ref().is_some_and(|a| answer_matches(m, a));
        answers.push(json!({
            "name": m.name,
            "sent_bytes": m.bytes.len(),
            "sent_sha256": sha_hex(&m.bytes),
            "answer": answer.as_ref().map(file_json),
            "matches": matches,
        }));
    }
    let listed = list_files(api, c)?;
    let mut names: Vec<&str> = listed.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    let mut wanted: Vec<&str> = all.iter().map(|m| m.name).collect();
    wanted.sort_unstable();
    Ok(json!({
        "pass": answers.iter().all(|a| a["matches"] == json!(true)) && names == wanted,
        "template_bytes": template.len(),
        "files": answers,
        "listed": listed.iter().map(file_json).collect::<Vec<_>>(),
        "listed_count": listed.len(),
    }))
}

fn next(api: &mut Api, c: u64, ctx: &mut Ctx) -> Result<Value, String> {
    let req = pb::SetNextMissionRequest {
        write: api.write("next-refused", c),
        next: Some(next_mission(Precip::Rain)),
    };
    let refused = api.rt.block_on(api.mission.set_next_mission(req));
    let refusal = match &refused {
        Err(s) => {
            let d = nanoprobe_proto::errors::detail(s);
            json!({
                "code": format!("{:?}", s.code()),
                "reason": d.as_ref().and_then(|d| pb::ErrorReason::try_from(d.reason).ok()).map(|r| r.as_str_name()),
                "field": d.as_ref().map(|d| d.field.clone()),
                "message": s.message(),
            })
        }
        Ok(_) => json!({ "accepted": true }),
    };
    let invalid = matches!(&refused, Err(s) if s.code() == tonic::Code::InvalidArgument
        && nanoprobe_proto::errors::reason(s) == pb::ErrorReason::InvalidField);
    // Set before the call: a call that fails half-way is undone.
    ctx.next_set = true;
    api.next(c, next_mission(Precip::Snow))?;
    Ok(json!({
        "pass": invalid,
        "rain_at_minus_5_refused": invalid,
        "refusal": refusal,
    }))
}

/// Wait until the sidecar is attached to a healthy DCS with the campaign
/// active and no load running.
fn ready(run: &dyn Live, api: &mut Api, c: u64) -> Result<(), String> {
    wait(run, api, LOAD_LIMIT, "the campaign ready", |api| {
        let s = api.status()?;
        Ok((s.campaign_id == c
            && s.attached
            && s.health == pb::Health::Healthy as i32
            && s.sim_state != pb::SimState::Loading as i32)
            .then_some(()))
    })
}

/// `Reload` once no mission load is owed: the campaign's activation owes one
/// until DCS applies it, and a reload before that is refused (run 2). Gives
/// the mission generation the reload started from, read after any owed load
/// was applied.
fn reload_when_free(run: &dyn Live, api: &mut Api) -> Result<u32, String> {
    let start = Instant::now();
    loop {
        let before = api.status()?.now.map_or(0, |n| n.mission_generation);
        match api.reload() {
            Ok(()) => return Ok(before),
            Err(e) if e.contains("owed already") && start.elapsed() < LOAD_LIMIT => {
                run.hold(2.0)?;
                api.keep();
            }
            Err(e) => return Err(e),
        }
    }
}

fn reload(run: &dyn Live, api: &mut Api, c: u64, ctx: &mut Ctx) -> Result<Value, String> {
    let t = Instant::now();
    ctx.reloaded = true;
    let before = reload_when_free(run, api)?;
    let after = loaded(run, api, THEATER, before)?;
    ready(run, api, c)?;
    let _ = run.hold(5.0);
    api.keep();
    Ok(json!({
        "pass": after > before,
        "generation_before": before,
        "generation_after": after,
        "seconds": t.elapsed().as_secs_f64(),
    }))
}

fn readback(run: &dyn Live, api: &mut Api, ctx: &mut Ctx) -> Result<Value, String> {
    let mission = read(run, "server", "mission-content-mission", MISSION_LUA);
    api.keep();
    let seen = mission["value"].clone();
    let key = |f: &str| seen[f].as_str().map(str::to_owned);
    ctx.picture_key = list_of(&seen["pictureFileNameB"])
        .first()
        .and_then(|k| k.as_str().map(str::to_owned));
    let dict = match (key("sortie"), key("descriptionText")) {
        (Some(s), Some(d)) => read(run, "server", "mission-content-dict", &dict_lua(&s, &d)),
        _ => json!({ "ok": false, "error": "no dictionary key to read" }),
    };
    api.keep();
    let description = read(run, "hook", "mission-content-description", DESCRIPTION_LUA);
    api.keep();
    let airbase = read(run, "server", "mission-content-airbase", AIRBASE_LUA);
    ctx.airbase = Airbase::from_value(&airbase["value"]);
    api.keep();
    let mut atmosphere = json!({});
    if let Some(a) = &ctx.airbase {
        for (label, y) in [
            ("wind_ground_plus_10m", a.ground + 10.0),
            ("wind_2000m", 2000.0),
            ("wind_8000m", 8000.0),
        ] {
            atmosphere[label] = read(
                run,
                "server",
                &format!("mission-content-{label}"),
                &wind_lua(a.x, y, a.z),
            );
            api.keep();
        }
        atmosphere["temperature_and_pressure_ground"] = read(
            run,
            "server",
            "mission-content-temperature",
            &temperature_lua(a.x, a.ground, a.z),
        );
        api.keep();
    }
    let checks = if mission["ok"] == json!(true) {
        mission_checks(&seen)
    } else {
        json!({ "pass": false, "error": "the mission read failed" })
    };
    let text_ok = dict_matches(&dict);
    Ok(json!({
        "pass": checks["pass"] == json!(true) && text_ok != Some(false),
        "mission": mission,
        "checks": checks,
        "dictionary": dict,
        "dictionary_texts_match": text_ok,
        "get_mission_description": description,
        "first_airbase": airbase,
        "atmosphere": atmosphere,
        "picture_key": ctx.picture_key,
    }))
}

/// One effect, recorded as the outcome the sidecar gives.
fn effect(api: &mut Api, c: u64, what: &str, e: pb::effect::Effect) -> Result<Value, String> {
    let req = pb::DoEffectRequest {
        write: api.write(what, c),
        effect: Some(pb::Effect { effect: Some(e) }),
    };
    let r = api
        .rt
        .block_on(api.commands.do_effect(req))
        .map_err(status_text("DoEffect"))?
        .into_inner();
    Ok(outcome_json(&r))
}

fn outcome_json(r: &pb::DoEffectResponse) -> Value {
    let res = r.result.as_ref();
    let outcome = res.map(|c| c.outcome);
    json!({
        "applied": outcome == Some(pb::Outcome::Applied as i32),
        "outcome": outcome.and_then(|o| pb::Outcome::try_from(o).ok()).map(|o| o.as_str_name()),
        "code": res.and_then(|c| pb::ResultCode::try_from(c.code).ok()).map(|c| c.as_str_name()),
        "message": res.map(|c| c.message.clone()),
        "cost_us": res.map(|c| c.cost_us),
    })
}

/// A hold the operator can set, then the lease kept.
/// The most an operator gate waits, in seconds.
const GATE_MAX_S: u64 = 1800;

/// Wait after a visible action. With `NP_CONTENT_GATE_DIR` set, wait until
/// the operator makes the file `<dir>/<label>` (when the person at the
/// client says ready), at most [`GATE_MAX_S`]; else hold model time for
/// [`hold_s`].
fn pause(run: &dyn Live, api: &mut Api, label: &str) -> Result<(), String> {
    if let Some(dir) = std::env::var_os("NP_CONTENT_GATE_DIR") {
        let file = std::path::Path::new(&dir).join(label);
        println!("   gate: waiting for {}", file.display());
        let start = Instant::now();
        while !file.exists() {
            if start.elapsed().as_secs() > GATE_MAX_S {
                return Err(format!(
                    "gate {label}: no {} in {GATE_MAX_S} s",
                    file.display()
                ));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
            api.keep();
        }
        return Ok(());
    }
    println!("   hold {} s", hold_s());
    run.hold(hold_s())?;
    api.keep();
    Ok(())
}

fn effects(run: &dyn Live, api: &mut Api, c: u64, ctx: &mut Ctx) -> Result<Value, String> {
    let at = ctx.airbase.clone().ok_or("no airbase point was read")?;
    let key = ctx
        .picture_key
        .clone()
        .ok_or("the mission has no briefing picture key")?;
    let mut out = json!({});
    // The person checks the briefing screens and the kneeboard first.
    pause(run, api, "briefing-checked")?;
    out["sound"] = effect(api, c, "sound", sound("np-new.wav"))?;
    pause(run, api, "after-sound")?;
    out["picture_by_name"] = effect(api, c, "picture", picture("np-blue.png", PICTURE_S))?;
    pause(run, api, "after-picture-name")?;
    out["picture_by_key"] = effect(api, c, "picture-key", picture(&key, PICTURE_S))?;
    pause(run, api, "after-picture-key")?;
    ctx.broadcast = true;
    out["broadcast"] = effect(api, c, "broadcast", broadcast(&at, FREQ_HZ))?;
    pause(run, api, "after-broadcast")?;
    out["picture_key"] = json!(key);
    out["airbase"] = json!(at.name);
    out["pass"] = json!(
        ["sound", "picture_by_name", "picture_by_key", "broadcast"]
            .iter()
            .all(|k| out[*k]["applied"] == json!(true))
    );
    Ok(out)
}

/// A call whose outcome is the result: an error status is recorded too.
fn case(api: &mut Api, c: u64, what: &str, e: pb::effect::Effect) -> Value {
    effect(api, c, what, e).unwrap_or_else(|err| json!({ "call_error": err }))
}

fn broadcast_cases(run: &dyn Live, api: &mut Api, c: u64, ctx: &mut Ctx) -> Result<Value, String> {
    let at = ctx.airbase.clone().ok_or("no airbase point was read")?;
    let mut out = json!({});
    out["same_name_other_frequency"] =
        case(api, c, "broadcast-again", broadcast(&at, FREQ_SECOND_HZ));
    pause(run, api, "after-broadcast-again")?;
    out["stop"] = case(api, c, "broadcast-stop", broadcast_stop(BROADCAST));
    ctx.broadcast = false;
    pause(run, api, "after-stop")?;
    out["stop_unknown_name"] = case(api, c, "broadcast-stop-none", broadcast_stop("np-none"));
    // The outcomes are the result; what a client heard is recorded by hand.
    out["pass"] = json!(true);
    Ok(out)
}

/// Undo what the run did. Every undo is tried and recorded; a step that was
/// not reached is not undone.
fn cleanup(run: &dyn Live, api: &mut Api, c: u64, ctx: &mut Ctx) -> Result<Value, String> {
    api.keep();
    let mut out = json!({});
    if ctx.broadcast {
        out["broadcast_stop"] = recorded(effect(api, c, "cleanup-stop", broadcast_stop(BROADCAST)));
        ctx.broadcast = false;
    }
    if ctx.next_set {
        out["next_without_images"] = recorded(
            api.next(c, next_without_images())
                .map(|()| json!({ "pass": true })),
        );
    }
    let all = media();
    let mut removed = Vec::new();
    for i in ctx.files_set.clone() {
        let m = &all[i];
        let r = set_file(api, c, m, "").map(|a| json!({ "pass": a.is_none() }));
        removed.push(json!({ "name": m.name, "result": recorded(r) }));
    }
    if !removed.is_empty() {
        out["files_removed"] = json!({
            "pass": removed.iter().all(|r| r["result"]["pass"] == json!(true)),
            "files": removed,
        });
    }
    if ctx.template_set {
        out["template_removed"] =
            recorded(set_template(api, c, "").map(|()| json!({ "pass": true })));
    }
    if !ctx.files_set.is_empty() || ctx.template_set {
        out["files_left"] =
            recorded(list_files(api, c).map(|f| json!({ "pass": f.is_empty(), "count": f.len() })));
    }
    // A reload to the shared template puts the mission DCS runs back to one
    // with none of the run's content.
    if ctx.reloaded {
        out["reload_clean"] = recorded(reload_clean(run, api, c));
    }
    out["files_placed_removed"] = recorded(remove_placed(run, ctx));
    out["pass"] = json!(
        out.as_object()
            .is_some_and(|o| o.values().all(|s| s["pass"] == json!(true)))
    );
    Ok(out)
}

fn reload_clean(run: &dyn Live, api: &mut Api, c: u64) -> Result<Value, String> {
    let before = reload_when_free(run, api)?;
    let after = loaded(run, api, THEATER, before)?;
    ready(run, api, c)?;
    Ok(json!({ "pass": after > before, "generation_after": after }))
}

/// Remove every file the run placed, and check each is gone. Every removal is
/// tried.
fn remove_placed(run: &dyn Live, ctx: &mut Ctx) -> Result<Value, String> {
    let mut left = Vec::new();
    let mut errors = Vec::new();
    for path in std::mem::take(&mut ctx.placed) {
        if let Err(e) = run.profile().remove_file(&path) {
            errors.push(format!("{path}: {e}"));
        }
        match run.profile().read(&path) {
            Ok(None) => {}
            Ok(Some(_)) => left.push(path),
            Err(e) => errors.push(e),
        }
    }
    Ok(json!({
        "pass": left.is_empty() && errors.is_empty(),
        "left": left,
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(name: &str) -> shell::media::Media {
        shell::media::Media::of_name(name).expect("a media name")
    }

    #[test]
    fn the_template_has_a_blue_and_a_red_player_slot_and_passes_the_check() {
        let miz = template_bytes().expect("template");
        shell::check(&miz, "Caucasus").expect("check");
        let names = shell::placed_names(&miz).expect("names");
        assert!(names.contains(&"NP Blue Huey".to_owned()), "{names:?}");
        assert!(names.contains(&"NP Red Huey 1".to_owned()), "{names:?}");
        let m = shell::members(&miz).expect("members");
        let mission = &m.iter().find(|(n, _)| n == "mission").expect("mission").1;
        let d = shell::lua::parse(mission).expect("parses");
        let k = shell::lua::Key::s;
        for (side, country) in [("blue", 2.0), ("red", 0.0)] {
            let c = d
                .root
                .at(&[
                    k("coalition"),
                    k(side),
                    k("country"),
                    shell::lua::Key::Num(1.0),
                    k("id"),
                ])
                .expect("id");
            assert_eq!(
                shell::lua::scalar_value(mission, c),
                Some(nanoprobe_transport::codec::Lv::Number(country))
            );
        }
    }

    #[test]
    fn there_are_six_files_the_sidecar_accepts() {
        let all = media();
        assert_eq!(all.len(), 6);
        let mut names: Vec<&str> = all.iter().map(|m| m.name).collect();
        for m in &all {
            shell::media::check(kind_of(m.name), &m.bytes)
                .unwrap_or_else(|e| panic!("{}: {e}", m.name));
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 6);
        let mut hashes: Vec<String> = all.iter().map(|m| sha_hex(&m.bytes)).collect();
        hashes.sort_unstable();
        hashes.dedup();
        assert_eq!(hashes.len(), 6, "every file differs");
    }

    #[test]
    fn the_files_have_the_kinds_and_sizes_asked_for() {
        let all = media();
        let get = |n: &str| all.iter().find(|m| m.name == n).expect(n);
        assert_eq!(get("np-kb-all.png").kind, pb::MissionFileKind::Kneeboard);
        let uh1 = get("np-kb-uh1.png");
        assert_eq!(uh1.kind, pb::MissionFileKind::KneeboardType);
        assert_eq!(uh1.aircraft_type, "UH-1H");
        assert!(
            all.iter()
                .filter(|m| m.kind != pb::MissionFileKind::KneeboardType)
                .all(|m| m.aircraft_type.is_empty())
        );
        let side = |b: &[u8]| {
            (
                u32::from_be_bytes(b[16..20].try_into().unwrap()),
                u32::from_be_bytes(b[20..24].try_into().unwrap()),
            )
        };
        assert_eq!(side(&get("np-blue.png").bytes), (64, 64));
        assert_eq!(side(&get("np-red.png").bytes), (64, 64));
        assert_eq!(side(&get("np-kb-all.png").bytes), (256, 256));
        assert_eq!(side(&get("np-kb-uh1.png").bytes), (256, 256));
        // The new sound is 2 s of 16-bit mono at 22,050 Hz; the tones differ
        // from the template's 440 Hz one.
        assert_eq!(get("np-new.wav").bytes.len(), 44 + 2 * 22_050 * 2);
        assert_ne!(
            get("np-tone.wav").bytes,
            crate::miz::tone_wav(440.0, 0.5),
            "the replacement differs from the template's"
        );
    }

    #[test]
    fn the_pictures_are_solid_colors() {
        let all = media();
        // Three pixels in a row.
        let has = |n: &str, rgb: [u8; 3]| {
            let b = &all.iter().find(|m| m.name == n).expect(n).bytes;
            let three: Vec<u8> = rgb.iter().copied().cycle().take(9).collect();
            b.windows(9).any(|w| w == three.as_slice())
        };
        assert!(has("np-blue.png", [0, 0, 255]));
        assert!(has("np-red.png", [255, 0, 0]));
        assert!(has("np-kb-all.png", [0, 255, 0]));
        assert!(has("np-kb-uh1.png", [255, 255, 0]));
        // The blue and the red file differ.
        assert_ne!(
            all[2].bytes,
            all[3].bytes,
            "{:?}",
            (all[2].name, all[3].name)
        );
    }

    #[test]
    fn the_template_packs_the_test_resources() {
        let members = crate::miz::read(&template_bytes().expect("template")).expect("read");
        let names: Vec<&str> = members.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"l10n/DEFAULT/np-tone.wav"));
        assert!(names.contains(&"l10n/DEFAULT/np-picture.png"));
        let map = members
            .iter()
            .find(|(n, _)| n == "l10n/DEFAULT/mapResource")
            .expect("mapResource");
        let map = String::from_utf8_lossy(&map.1);
        assert!(
            map.contains("np-tone.wav") && map.contains("np-picture.png"),
            "{map}"
        );
        assert_eq!(names.last(), Some(&"theatre"));
    }

    #[test]
    fn an_answer_matches_the_file_sent() {
        let all = media();
        let m = &all[1];
        let answer = pb::MissionFile {
            name: m.name.into(),
            kind: m.kind as i32,
            aircraft_type: String::new(),
            size_bytes: m.bytes.len() as u64,
            sha256: Sha256::digest(&m.bytes).to_vec(),
        };
        assert!(answer_matches(m, &answer));
        let mut other = answer.clone();
        other.size_bytes += 1;
        assert!(!answer_matches(m, &other));
        let mut other = answer.clone();
        other.sha256[0] ^= 1;
        assert!(!answer_matches(m, &other));
        let mut other = answer;
        other.kind = pb::MissionFileKind::Kneeboard as i32;
        assert!(!answer_matches(m, &other));
    }

    #[test]
    fn the_weather_is_the_one_asked_for() {
        let w = conditions(Precip::Snow);
        assert_eq!(w.temperature_c, Some(-5.0));
        assert_eq!(w.qnh_mmhg, Some(750.0));
        assert_eq!(w.visibility_m, Some(30_000.0));
        assert_eq!(w.wind_ground, wind(8.0, 270));
        assert_eq!(w.wind_2000m, wind(15.0, 300));
        assert_eq!(w.wind_8000m, wind(25.0, 320));
        assert_eq!(w.ground_turbulence_mps, Some(2.0));
        let clouds = w.clouds.expect("clouds");
        assert_eq!(clouds.preset, pb::CloudPreset::None as i32);
        assert_eq!(clouds.base_m, Some(1200.0));
        assert_eq!(clouds.thickness_m, Some(800.0));
        assert_eq!(clouds.density, Some(9));
        assert_eq!(clouds.precipitation, Precip::Snow as i32);
        assert_eq!(w.fog.expect("fog").mode, pb::FogMode::Off as i32);
        assert!(!w.dust.expect("dust").enabled);
        assert_eq!(w.halo.expect("halo").preset, pb::HaloPreset::Auto as i32);
    }

    #[test]
    fn the_refused_request_differs_only_in_the_precipitation() {
        let snow = next_mission(Precip::Snow);
        let mut rain = next_mission(Precip::Rain);
        assert_eq!(
            rain.conditions.as_ref().and_then(|w| w.temperature_c),
            Some(-5.0)
        );
        rain.conditions
            .as_mut()
            .and_then(|w| w.clouds.as_mut())
            .expect("clouds")
            .precipitation = Precip::Snow as i32;
        assert_eq!(rain, snow);
    }

    #[test]
    #[allow(deprecated)] // the whole weather table stays supported
    fn the_briefing_and_images_are_the_ones_asked_for() {
        let n = next_mission(Precip::Snow);
        let b = n.briefing.expect("briefing");
        assert_eq!(b.sortie.as_deref(), Some("NP content check"));
        assert_eq!(
            b.description.as_deref(),
            Some("Line one\nLine two: Привет, ñ")
        );
        assert_eq!(b.blue_task.as_deref(), Some("Blue task text"));
        assert_eq!(b.red_task.as_deref(), Some("Red task text"));
        assert_eq!(b.neutral_task.as_deref(), Some("Neutral task text"));
        let i = n.briefing_images.expect("images");
        assert_eq!(
            i.blue.expect("blue").files,
            ["np-blue.png", "np-picture.png"]
        );
        assert_eq!(i.red.expect("red").files, ["np-red.png"]);
        assert!(i.neutral.expect("neutral").files.is_empty());
        assert_eq!(
            i.coalition_selection.expect("selection").files,
            ["np-blue.png"]
        );
        assert!(n.weather.is_none(), "the whole table is not set");
        // The cleanup request names no file and no weather.
        let c = next_without_images();
        assert!(c.conditions.is_none() && c.briefing.is_none());
        let i = c.briefing_images.expect("images");
        assert!(
            [i.blue, i.red, i.neutral, i.coalition_selection]
                .iter()
                .all(|l| l.as_ref().is_some_and(|l| l.files.is_empty()))
        );
    }

    #[test]
    fn every_image_the_next_mission_names_is_one_the_run_holds() {
        let held: Vec<&str> = media().iter().map(|m| m.name).collect();
        let template = ["np-picture.png", "np-tone.wav"];
        let i = images();
        for list in [i.blue, i.red, i.neutral, i.coalition_selection]
            .into_iter()
            .flatten()
        {
            for f in &list.files {
                assert!(
                    held.contains(&f.as_str()) || template.contains(&f.as_str()),
                    "{f}"
                );
                assert!(shell::media::Media::of_name(f).expect("type").is_image());
            }
        }
    }

    #[test]
    fn the_broadcast_is_where_and_how_asked() {
        let at = Airbase {
            name: "A".into(),
            x: 10.0,
            z: 20.0,
            ground: 100.0,
        };
        let pb::effect::Effect::Broadcast(b) = broadcast(&at, FREQ_HZ) else {
            panic!("not a broadcast");
        };
        assert_eq!(b.name, "np-bc");
        assert_eq!(b.file, "np-new.wav");
        assert_eq!(b.frequency_hz, 251_000_000);
        assert_eq!(b.modulation, pb::RadioModulation::Am as i32);
        assert!(b.r#loop);
        assert_eq!(b.power_w, Some(1000.0));
        let p = b.position.expect("position");
        assert_eq!((p.north, p.east, p.alt), (10.0, 20.0, 150.0));
        assert_eq!(p.theater, "Caucasus");
        let pb::effect::Effect::Broadcast(second) = broadcast(&at, FREQ_SECOND_HZ) else {
            panic!("not a broadcast");
        };
        assert_eq!(
            (second.name.as_str(), second.frequency_hz),
            ("np-bc", 252_000_000)
        );
        assert!(matches!(
            broadcast_stop("np-none"),
            pb::effect::Effect::BroadcastStop(s) if s.name == "np-none"
        ));
    }

    #[test]
    fn the_pictures_and_the_sound_reach_everyone() {
        let pb::effect::Effect::Picture(p) = picture("np-blue.png", PICTURE_S) else {
            panic!("not a picture");
        };
        assert_eq!(p.scope, pb::EffectScope::All as i32);
        assert_eq!((p.file.as_str(), p.seconds), ("np-blue.png", 15.0));
        assert!(p.size >= 0.0001 && p.size <= 10_000.0);
        let pb::effect::Effect::Sound(s) = sound("np-new.wav") else {
            panic!("not a sound");
        };
        assert_eq!(
            (s.scope, s.file.as_str()),
            (pb::EffectScope::All as i32, "np-new.wav")
        );
    }

    #[test]
    fn the_hold_comes_from_the_environment_or_defaults() {
        assert_eq!(hold_from(None), 20.0);
        assert_eq!(hold_from(Some("5")), 5.0);
        assert_eq!(hold_from(Some(" 2.5 ")), 2.5);
        assert_eq!(hold_from(Some("0")), 0.0);
        assert_eq!(hold_from(Some("-1")), 20.0);
        assert_eq!(hold_from(Some("soon")), 20.0);
        assert_eq!(hold_from(Some("inf")), 20.0);
    }

    #[test]
    fn a_lua_list_is_an_array_or_a_numbered_object() {
        assert_eq!(list_of(&json!(["a", "b"])), [json!("a"), json!("b")]);
        assert_eq!(
            list_of(&json!({ "2": "b", "1": "a", "10": "c" })),
            [json!("a"), json!("b"), json!("c")]
        );
        assert!(list_of(&json!({})).is_empty());
        assert!(list_of(&Value::Null).is_empty());
    }

    /// `env.mission` as the shell should write it.
    fn mission() -> Value {
        json!({
            "weather": {
                "temperature": -5, "qnh": 750, "visibility": 30000,
                "atGround": { "speed": 8, "dir": 270 },
                "at2000": { "speed": 15, "dir": 300 },
                "at8000": { "speed": 25, "dir": 320 },
                "groundTurbulence": 20,
                "clouds": { "base": 1200, "thickness": 800, "density": 9, "iprecptns": 3 },
                "enable_fog": false, "enable_dust": false,
                "halo": { "preset": "auto" },
                "atmosphere_type": 0,
            },
            "sortie": "DictKey_sortie_5",
            "descriptionText": "DictKey_descriptionText_6",
            "descriptionBlueTask": "DictKey_descriptionBlueTask_7",
            "descriptionRedTask": "DictKey_descriptionRedTask_8",
            "descriptionNeutralsTask": "DictKey_descriptionNeutralsTask_9",
            "pictureFileNameB": { "1": "ResKey_np_1", "2": "ResKey_Action_902" },
            "pictureFileNameR": ["ResKey_np_2"],
            "pictureFileNameN": {},
            "pictureFileNameServer": ["ResKey_np_1"],
            "maxDictId": 12,
        })
    }

    #[test]
    fn the_mission_the_spec_describes_passes_the_checks() {
        let v = mission_checks(&mission());
        assert_eq!(v["pass"], json!(true), "{v}");
        assert!(
            v["gated"]
                .as_array()
                .expect("rows")
                .iter()
                .any(|r| r["check"] == json!("turbulence_20") && r["ok"] == json!(true))
        );
    }

    #[test]
    fn a_wrong_or_missing_value_fails_the_checks() {
        let edit = |f: &dyn Fn(&mut Value)| {
            let mut m = mission();
            f(&mut m);
            mission_checks(&m)["pass"] == json!(true)
        };
        assert!(!edit(&|m| m["weather"]["temperature"] = json!(5)));
        assert!(!edit(&|m| m["weather"]["clouds"]["iprecptns"] = json!(1)));
        assert!(!edit(&|m| m["weather"]["enable_fog"] = json!(true)));
        assert!(!edit(&|m| m["weather"]["fog2"] = json!({ "mode": 2 })));
        assert!(!edit(&|m| m["weather"]["atmosphere_type"] = json!(1)));
        assert!(!edit(&|m| m["sortie"] = json!("NP content check")));
        assert!(!edit(&|m| m["pictureFileNameB"] = json!(["ResKey_np_1"])));
        assert!(!edit(&|m| m["pictureFileNameN"] = json!(["ResKey_np_3"])));
        assert!(!edit(&|m| m["pictureFileNameServer"] = json!({})));
        assert!(
            !mission_checks(&Value::Null)["pass"]
                .as_bool()
                .unwrap_or(false)
        );
        // A direction or a turbulence scale that differs fails.
        assert!(!edit(&|m| m["weather"]["atGround"]["dir"] = json!(90)));
        assert!(!edit(&|m| m["weather"]["at2000"]["dir"] = json!(120)));
        assert!(!edit(&|m| m["weather"]["at8000"]["dir"] = json!(140)));
        assert!(!edit(&|m| m["weather"]["groundTurbulence"] = json!(2)));
        // The preset and the halo are recorded, not failed.
        assert!(edit(&|m| m["weather"]["halo"]["preset"] = json!("off")));
    }

    #[test]
    fn the_dictionary_read_fails_only_with_the_wrong_texts() {
        let ok = json!({ "ok": true, "value": { "sortie": SORTIE, "description": DESCRIPTION } });
        assert_eq!(dict_matches(&ok), Some(true));
        let wrong = json!({ "ok": true, "value": { "sortie": "DictKey_sortie_5", "description": DESCRIPTION } });
        assert_eq!(dict_matches(&wrong), Some(false));
        assert_eq!(dict_matches(&json!({ "ok": false, "error": "x" })), None);
    }

    #[test]
    fn the_airbase_read_gives_a_point() {
        let a =
            Airbase::from_value(&json!({ "name": "Batumi", "x": 1.5, "z": -2.5, "ground": 10.0 }))
                .expect("airbase");
        assert_eq!(a.position(50.0).alt, 60.0);
        assert!(Airbase::from_value(&json!({ "name": "Batumi" })).is_none());
    }

    #[test]
    fn the_lua_requests_name_the_calls() {
        assert!(wind_lua(1.0, 2.0, 3.0).contains("atmosphere.getWind({ x = 1, y = 2, z = 3 })"));
        assert!(temperature_lua(1.0, 2.5, 3.0).contains("getTemperatureAndPressure"));
        let d = dict_lua("DictKey_sortie_5", "DictKey_descriptionText_6");
        assert!(d.contains("env.getValueDictByKey(\"DictKey_sortie_5\")"));
        assert!(MISSION_LUA.contains("w.season and w.season.temperature"));
    }
}
