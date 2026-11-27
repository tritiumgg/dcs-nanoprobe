//! Typed weather, merged over a template's `mission.weather` table
//! (spec/persistence.md, "Weather"). Keys, units and ranges are the Mission
//! Editor's (ED-CODE, DCS 2.9.31.29434: `MissionEditor/modules/me_weather.lua`,
//! `me_mission.lua`, `me_utilities.lua`, `dialogs/me_weather_panel.dlg`,
//! `Config/Effects/clouds.lua` and `icehalo.lua`).

use nanoprobe_transport::codec::Lv;

/// The cloud presets the editor offers (`visibleInGUI`), with the range of
/// cloud bases each allows in meters (`presetAltMin`, `presetAltMax`).
pub const CLOUD_PRESETS: &[(&str, f64, f64)] = &[
    ("Preset1", 840.0, 4200.0),
    ("Preset2", 1260.0, 2520.0),
    ("Preset3", 840.0, 2520.0),
    ("Preset4", 1260.0, 2520.0),
    ("Preset5", 1260.0, 4620.0),
    ("Preset6", 1260.0, 4200.0),
    ("Preset7", 1680.0, 5040.0),
    ("Preset8", 3780.0, 5460.0),
    ("Preset9", 1680.0, 3780.0),
    ("Preset10", 1260.0, 4200.0),
    ("Preset11", 2520.0, 5460.0),
    ("Preset12", 1680.0, 3360.0),
    ("Preset13", 1680.0, 3360.0),
    ("Preset14", 1680.0, 3360.0),
    ("Preset15", 840.0, 5040.0),
    ("Preset16", 1260.0, 4200.0),
    ("Preset17", 0.0, 2520.0),
    ("Preset18", 0.0, 3780.0),
    ("Preset19", 0.0, 2940.0),
    ("Preset20", 0.0, 3780.0),
    ("Preset21", 1260.0, 4200.0),
    ("Preset22", 420.0, 4200.0),
    ("Preset23", 840.0, 3360.0),
    ("Preset24", 420.0, 2520.0),
    ("Preset25", 420.0, 3360.0),
    ("Preset26", 420.0, 2940.0),
    ("Preset27", 420.0, 2520.0),
    ("RainyPreset1", 420.0, 2940.0),
    ("RainyPreset2", 840.0, 2520.0),
    ("RainyPreset3", 840.0, 2520.0),
    ("RainyPreset4", 1260.0, 4200.0),
    ("RainyPreset5", 1260.0, 2520.0),
    ("RainyPreset6", 1260.0, 2940.0),
    ("NEWRAINPRESET4", 840.0, 5174.4),
];

/// Halo presets: `halo.preset`.
pub const HALO_PRESETS: &[&str] = &[
    "off",
    "auto",
    "AtmoHighClouds",
    "CirrusOnly",
    "HighClouds",
    "VolumetricOnly",
];

/// Halo crystal presets: `halo.crystalsPreset`.
pub const HALO_CRYSTALS: &[&str] = &[
    "AllKinds",
    "BasicHaloCircle",
    "BasicHaloWithSundogs",
    "BasicSundogsTangents",
    "SundogsArcs",
    "Tangents",
];

/// Precipitation, `clouds.iprecptns`: the least density and the
/// temperature side each needs (`me_weather.lua`, `precptns`).
/// `(value, least density, needs temperature >= 0, needs <= 0)`.
const PRECIPITATION: &[(u32, u32, bool, bool)] = &[
    (0, 0, false, false),
    (1, 5, true, false),
    (2, 9, true, false),
    (3, 5, false, true),
    (4, 9, false, true),
];

/// The most keys of manual fog.
pub const FOG_KEYS_MAX: usize = 16;

/// One wind layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wind {
    /// m/s, 0 to 50.
    pub speed_mps: f64,
    /// The direction the wind blows toward, degrees from north, 0 to 359.
    pub toward_deg: u32,
}

/// Fog: `fog2`.
#[derive(Clone, Debug, PartialEq)]
pub enum Fog {
    /// `fog2` absent: the editor drops it when off.
    Off,
    /// `fog2.mode` 2.
    Auto,
    /// `fog2.mode` 4 with its keys: time of day in seconds, visibility and
    /// thickness in meters.
    Manual(Vec<(u32, f64, f64)>),
}

/// The fields a controller sets; `None` keeps the template's.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WeatherSet {
    /// `season.temperature`, °C.
    pub temperature_c: Option<f64>,
    /// `qnh`, mmHg.
    pub qnh_mmhg: Option<f64>,
    /// `visibility.distance`, m.
    pub visibility_m: Option<f64>,
    /// `wind.atGround`, `at2000`, `at8000`.
    pub wind: [Option<Wind>; 3],
    /// `groundTurbulence` is ten times this.
    pub ground_turbulence_mps: Option<f64>,
    /// `clouds.preset`: `Some(None)` removes it.
    pub cloud_preset: Option<Option<String>>,
    /// `clouds.base`, m.
    pub cloud_base_m: Option<f64>,
    /// `clouds.thickness`, m.
    pub cloud_thickness_m: Option<f64>,
    /// `clouds.density`.
    pub cloud_density: Option<u32>,
    /// `clouds.iprecptns`.
    pub precipitation: Option<u32>,
    /// `fog2`.
    pub fog: Option<Fog>,
    /// `enable_dust` and `dust_density` (the visibility in the dust, m).
    pub dust: Option<(bool, f64)>,
    /// `halo.preset`.
    pub halo: Option<String>,
    /// `halo.crystalsPreset`.
    pub halo_crystals: Option<String>,
}

const WIND_KEYS: [&str; 3] = ["atGround", "at2000", "at8000"];

fn in_range(what: &str, v: f64, lo: f64, hi: f64) -> Result<(), String> {
    if v.is_finite() && (lo..=hi).contains(&v) {
        Ok(())
    } else {
        Err(format!("{what} is {v}; it is {lo} to {hi}"))
    }
}

fn s(k: &str) -> Lv {
    Lv::String(k.as_bytes().to_vec())
}

fn get<'a>(t: &'a Lv, k: &str) -> Option<&'a Lv> {
    match t {
        Lv::Table(p) => p.iter().find(|(key, _)| *key == s(k)).map(|(_, v)| v),
        _ => None,
    }
}

fn num(t: &Lv, path: &[&str]) -> Option<f64> {
    let mut v = t;
    for k in path {
        v = get(v, k)?;
    }
    match v {
        Lv::Number(n) => Some(*n),
        _ => None,
    }
}

fn text<'a>(t: &'a Lv, path: &[&str]) -> Option<&'a [u8]> {
    let mut v = t;
    for k in path {
        v = get(v, k)?;
    }
    match v {
        Lv::String(b) => Some(b),
        _ => None,
    }
}

/// Set `path` in table `t`, making tables on the way; `Lv::Nil` removes.
fn put(t: &mut Lv, path: &[&str], v: Lv) {
    let Lv::Table(pairs) = t else {
        return;
    };
    let key = s(path[0]);
    let pos = pairs.iter().position(|(k, _)| *k == key);
    if path.len() == 1 {
        match (pos, v) {
            (Some(i), Lv::Nil) => {
                pairs.remove(i);
            }
            (Some(i), v) => pairs[i].1 = v,
            (None, Lv::Nil) => {}
            (None, v) => pairs.push((key, v)),
        }
        return;
    }
    let i = match pos {
        Some(i) if matches!(pairs[i].1, Lv::Table(_)) => i,
        Some(i) => {
            pairs[i].1 = Lv::Table(Vec::new());
            i
        }
        None => {
            pairs.push((key, Lv::Table(Vec::new())));
            pairs.len() - 1
        }
    };
    put(&mut pairs[i].1, &path[1..], v);
}

/// A cloud preset's base range, for a preset the editor offers.
pub fn preset_range(id: &str) -> Option<(f64, f64)> {
    CLOUD_PRESETS
        .iter()
        .find(|(p, _, _)| *p == id)
        .map(|&(_, lo, hi)| (lo, hi))
}

impl WeatherSet {
    /// Whether no field is set.
    pub fn is_empty(&self) -> bool {
        *self == WeatherSet::default()
    }

    /// The checks of each field alone.
    pub fn check(&self) -> Result<(), String> {
        if let Some(v) = self.temperature_c {
            in_range("the temperature", v, -50.0, 50.0)?;
        }
        if let Some(v) = self.qnh_mmhg {
            in_range("the QNH", v, 720.0, 790.0)?;
        }
        if let Some(v) = self.visibility_m {
            in_range("the visibility", v, 0.0, 80_000.0)?;
        }
        for (w, name) in self.wind.iter().zip(WIND_KEYS) {
            if let Some(w) = w {
                in_range(&format!("the wind speed {name}"), w.speed_mps, 0.0, 50.0)?;
                if w.toward_deg > 359 {
                    return Err(format!(
                        "the wind direction {name} is {}; it is 0 to 359",
                        w.toward_deg
                    ));
                }
            }
        }
        if let Some(v) = self.ground_turbulence_mps {
            in_range("the ground turbulence", v, 0.0, 6.0)?;
        }
        if let Some(Some(p)) = &self.cloud_preset
            && preset_range(p).is_none()
        {
            return Err(format!("{p:?} is not a cloud preset"));
        }
        if let Some(v) = self.cloud_base_m {
            in_range("the cloud base", v, 0.0, 5_460.0)?;
        }
        if let Some(v) = self.cloud_thickness_m {
            in_range("the cloud thickness", v, 200.0, 2_000.0)?;
        }
        if let Some(v) = self.cloud_density
            && v > 10
        {
            return Err(format!("the cloud density is {v}; it is 0 to 10"));
        }
        if let Some(v) = self.precipitation
            && !PRECIPITATION.iter().any(|p| p.0 == v)
        {
            return Err(format!("precipitation {v} is not known"));
        }
        if let Some(Fog::Manual(keys)) = &self.fog {
            if keys.is_empty() || keys.len() > FOG_KEYS_MAX {
                return Err(format!("manual fog has 1 to {FOG_KEYS_MAX} keys"));
            }
            let mut last: Option<u32> = None;
            for &(t, vis, thick) in keys {
                if t > 86_400 {
                    return Err(format!("a fog key at {t} s is past 24 hours"));
                }
                if last.is_some_and(|l| t <= l) {
                    return Err("the fog keys' times are not strictly increasing".into());
                }
                last = Some(t);
                in_range("a fog key's visibility", vis, 0.0, 100_000.0)?;
                in_range("a fog key's thickness", thick, 0.0, 5_000.0)?;
            }
        }
        if let Some((true, vis)) = self.dust {
            in_range("the visibility in the dust", vis, 300.0, 20_000.0)?;
        }
        if let Some(h) = &self.halo
            && !HALO_PRESETS.contains(&h.as_str())
        {
            return Err(format!("{h:?} is not a halo preset"));
        }
        if let Some(c) = &self.halo_crystals
            && !HALO_CRYSTALS.contains(&c.as_str())
        {
            return Err(format!("{c:?} is not a halo crystal preset"));
        }
        Ok(())
    }

    /// The template's weather table with these fields set, checked as a
    /// whole. Setting any field makes the weather static and turns the
    /// legacy fog off.
    pub fn merge(&self, template: &Lv) -> Result<Lv, String> {
        self.check()?;
        let Lv::Table(_) = template else {
            return Err("the template's weather is not a table".into());
        };
        let mut w = template.clone();
        if self.is_empty() {
            return Ok(w);
        }
        let n = Lv::Number;
        if let Some(v) = self.temperature_c {
            put(&mut w, &["season", "temperature"], n(v));
        }
        if let Some(v) = self.qnh_mmhg {
            put(&mut w, &["qnh"], n(v));
        }
        if let Some(v) = self.visibility_m {
            put(&mut w, &["visibility", "distance"], n(v));
        }
        for (wind, key) in self.wind.iter().zip(WIND_KEYS) {
            if let Some(wind) = wind {
                put(&mut w, &["wind", key, "speed"], n(wind.speed_mps));
                put(&mut w, &["wind", key, "dir"], n(f64::from(wind.toward_deg)));
            }
        }
        if let Some(v) = self.ground_turbulence_mps {
            // The editor stores its m/s spinbox value times 10.
            put(&mut w, &["groundTurbulence"], n((v * 10.0).round()));
        }
        match &self.cloud_preset {
            Some(Some(p)) => put(&mut w, &["clouds", "preset"], s(p)),
            Some(None) => put(&mut w, &["clouds", "preset"], Lv::Nil),
            None => {}
        }
        if let Some(v) = self.cloud_base_m {
            put(&mut w, &["clouds", "base"], n(v));
        }
        if let Some(v) = self.cloud_thickness_m {
            put(&mut w, &["clouds", "thickness"], n(v));
        }
        if let Some(v) = self.cloud_density {
            put(&mut w, &["clouds", "density"], n(f64::from(v)));
        }
        if let Some(v) = self.precipitation {
            put(&mut w, &["clouds", "iprecptns"], n(f64::from(v)));
        }
        match &self.fog {
            Some(Fog::Off) => put(&mut w, &["fog2"], Lv::Nil),
            Some(Fog::Auto) => {
                put(&mut w, &["fog2"], Lv::Table(vec![(s("mode"), n(2.0))]));
            }
            Some(Fog::Manual(keys)) => {
                let list = keys
                    .iter()
                    .enumerate()
                    .map(|(i, &(t, vis, thick))| {
                        (
                            n((i + 1) as f64),
                            Lv::Table(vec![
                                (s("time"), n(f64::from(t))),
                                (s("visibility"), n(vis)),
                                (s("thickness"), n(thick)),
                            ]),
                        )
                    })
                    .collect();
                put(
                    &mut w,
                    &["fog2"],
                    Lv::Table(vec![(s("mode"), n(4.0)), (s("manual"), Lv::Table(list))]),
                );
            }
            None => {}
        }
        if let Some((on, vis)) = self.dust {
            put(&mut w, &["enable_dust"], Lv::Bool(on));
            put(&mut w, &["dust_density"], n(if on { vis } else { 0.0 }));
        }
        if let Some(h) = &self.halo {
            put(&mut w, &["halo", "preset"], s(h));
        }
        if let Some(c) = &self.halo_crystals {
            put(&mut w, &["halo", "crystalsPreset"], s(c));
        }
        put(&mut w, &["atmosphere_type"], n(0.0));
        put(&mut w, &["enable_fog"], Lv::Bool(false));
        self.gates(&w)?;
        Ok(w)
    }

    /// The checks across fields, on the merged table.
    fn gates(&self, w: &Lv) -> Result<(), String> {
        if !matches!(get(w, "clouds"), Some(Lv::Table(_))) {
            return Err("the weather has no clouds table".into());
        }
        if !matches!(get(w, "season"), Some(Lv::Table(_))) {
            return Err("the weather has no season table".into());
        }
        let preset =
            text(w, &["clouds", "preset"]).map(|b| String::from_utf8_lossy(b).into_owned());
        let base = num(w, &["clouds", "base"]);
        match &preset {
            Some(p) => {
                if self.cloud_thickness_m.is_some() || self.cloud_density.is_some() {
                    return Err(format!(
                        "cloud thickness and density are set only with no preset; the clouds have preset {p:?}"
                    ));
                }
                if self.precipitation.is_some_and(|v| v != 0) {
                    return Err(format!(
                        "precipitation is set only with no preset; the clouds have preset {p:?}"
                    ));
                }
                if let (Some((lo, hi)), Some(b)) = (preset_range(p), base) {
                    in_range(&format!("the cloud base with preset {p}"), b, lo, hi)?;
                }
            }
            None => {
                if let Some(b) = base {
                    in_range("the cloud base with no preset", b, 300.0, 5_000.0)?;
                }
                let density = num(w, &["clouds", "density"]).unwrap_or(0.0);
                let rain = num(w, &["clouds", "iprecptns"]).unwrap_or(0.0);
                let temp = num(w, &["season", "temperature"]).unwrap_or(20.0);
                if let Some(&(v, least, warm, cold)) =
                    PRECIPITATION.iter().find(|p| f64::from(p.0) == rain)
                    && v != 0
                {
                    if density < f64::from(least) {
                        return Err(format!(
                            "precipitation {v} needs a cloud density of {least} or more; it is {density}"
                        ));
                    }
                    if warm && temp < 0.0 {
                        return Err(format!(
                            "rain needs a temperature of 0 or more; it is {temp}"
                        ));
                    }
                    if cold && temp > 0.0 {
                        return Err(format!(
                            "snow needs a temperature of 0 or less; it is {temp}"
                        ));
                    }
                }
            }
        }
        let fog_on = matches!(num(w, &["fog2", "mode"]), Some(m) if m == 2.0 || m == 4.0);
        let dust_on = matches!(get(w, "enable_dust"), Some(Lv::Bool(true)));
        if fog_on && dust_on {
            return Err("fog that is not off cannot be set together with dust".into());
        }
        if self.halo_crystals.is_some() && text(w, &["halo", "preset"]).is_none_or(|p| p == b"off")
        {
            return Err("halo crystals are set only with a halo preset other than off".into());
        }
        Ok(())
    }
}

/// The checks of a whole weather table given in place of the template's:
/// it holds `clouds` and `season` tables, without which the editor's loader
/// raises (ED-CODE, `me_weather.lua` `setData`).
pub fn check_whole(w: &Lv) -> Result<(), String> {
    for k in ["clouds", "season"] {
        if !matches!(get(w, k), Some(Lv::Table(_))) {
            return Err(format!("the weather has no {k} table"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template() -> Lv {
        Lv::Table(vec![
            (s("atmosphere_type"), Lv::Number(1.0)),
            (
                s("season"),
                Lv::Table(vec![(s("temperature"), Lv::Number(20.0))]),
            ),
            (s("qnh"), Lv::Number(760.0)),
            (
                s("clouds"),
                Lv::Table(vec![
                    (s("thickness"), Lv::Number(200.0)),
                    (s("density"), Lv::Number(0.0)),
                    (s("preset"), s("Preset2")),
                    (s("base"), Lv::Number(2500.0)),
                    (s("iprecptns"), Lv::Number(0.0)),
                ]),
            ),
            (s("enable_dust"), Lv::Bool(false)),
            (s("dust_density"), Lv::Number(0.0)),
            (s("halo"), Lv::Table(vec![(s("preset"), s("auto"))])),
        ])
    }

    #[test]
    fn an_empty_set_keeps_the_template() {
        assert_eq!(WeatherSet::default().merge(&template()), Ok(template()));
    }

    #[test]
    fn a_set_changes_only_its_fields_and_makes_the_weather_static() {
        let set = WeatherSet {
            qnh_mmhg: Some(750.5),
            wind: [
                Some(Wind {
                    speed_mps: 5.0,
                    toward_deg: 270,
                }),
                None,
                None,
            ],
            ground_turbulence_mps: Some(1.5),
            ..WeatherSet::default()
        };
        let w = set.merge(&template()).expect("merge");
        assert_eq!(num(&w, &["qnh"]), Some(750.5));
        assert_eq!(num(&w, &["wind", "atGround", "speed"]), Some(5.0));
        assert_eq!(num(&w, &["wind", "atGround", "dir"]), Some(270.0));
        assert_eq!(num(&w, &["groundTurbulence"]), Some(15.0));
        assert_eq!(num(&w, &["atmosphere_type"]), Some(0.0));
        assert_eq!(get(&w, "enable_fog"), Some(&Lv::Bool(false)));
        assert_eq!(num(&w, &["season", "temperature"]), Some(20.0));
        assert_eq!(text(&w, &["clouds", "preset"]), Some(&b"Preset2"[..]));
    }

    #[test]
    fn removing_the_preset_opens_the_no_preset_fields() {
        let set = WeatherSet {
            cloud_preset: Some(None),
            cloud_base_m: Some(800.0),
            cloud_thickness_m: Some(1000.0),
            cloud_density: Some(9),
            precipitation: Some(2),
            ..WeatherSet::default()
        };
        let w = set.merge(&template()).expect("merge");
        assert_eq!(text(&w, &["clouds", "preset"]), None);
        assert_eq!(num(&w, &["clouds", "iprecptns"]), Some(2.0));
    }

    #[test]
    fn manual_fog_and_halo_are_written_as_the_editor_does() {
        let set = WeatherSet {
            fog: Some(Fog::Manual(vec![(0, 1000.0, 200.0), (3600, 5000.0, 100.0)])),
            halo: Some("HighClouds".into()),
            halo_crystals: Some("Tangents".into()),
            ..WeatherSet::default()
        };
        let w = set.merge(&template()).expect("merge");
        assert_eq!(num(&w, &["fog2", "mode"]), Some(4.0));
        let manual = get(get(&w, "fog2").expect("fog2"), "manual").expect("manual");
        let Lv::Table(keys) = manual else {
            panic!("a list")
        };
        assert_eq!(keys.len(), 2);
        assert_eq!(
            text(&w, &["halo", "crystalsPreset"]),
            Some(&b"Tangents"[..])
        );
        let off = WeatherSet {
            fog: Some(Fog::Off),
            ..WeatherSet::default()
        };
        assert_eq!(get(&off.merge(&w).expect("off"), "fog2"), None);
    }

    #[test]
    fn each_range_refuses_just_outside_its_bound() {
        let d = WeatherSet::default;
        let no_preset = || WeatherSet {
            cloud_preset: Some(None),
            ..d()
        };
        let fog = |keys: Vec<(u32, f64, f64)>| WeatherSet {
            fog: Some(Fog::Manual(keys)),
            ..d()
        };
        let wind = |speed_mps: f64| WeatherSet {
            wind: [
                Some(Wind {
                    speed_mps,
                    toward_deg: 0,
                }),
                None,
                None,
            ],
            ..d()
        };
        let sixteen_plus_one: Vec<(u32, f64, f64)> = (0..=16).map(|i| (i * 10, 1.0, 1.0)).collect();
        let cases: Vec<(WeatherSet, &str)> = vec![
            (
                WeatherSet {
                    temperature_c: Some(-50.1),
                    ..d()
                },
                "the temperature",
            ),
            (
                WeatherSet {
                    temperature_c: Some(50.1),
                    ..d()
                },
                "the temperature",
            ),
            (
                WeatherSet {
                    qnh_mmhg: Some(719.9),
                    ..d()
                },
                "the QNH",
            ),
            (
                WeatherSet {
                    qnh_mmhg: Some(790.1),
                    ..d()
                },
                "the QNH",
            ),
            (
                WeatherSet {
                    visibility_m: Some(-1.0),
                    ..d()
                },
                "the visibility",
            ),
            (
                WeatherSet {
                    visibility_m: Some(80_001.0),
                    ..d()
                },
                "the visibility",
            ),
            (wind(-0.1), "the wind speed"),
            (wind(50.1), "the wind speed"),
            (
                WeatherSet {
                    ground_turbulence_mps: Some(-0.1),
                    ..d()
                },
                "the ground turbulence",
            ),
            (
                WeatherSet {
                    ground_turbulence_mps: Some(6.1),
                    ..d()
                },
                "the ground turbulence",
            ),
            (
                WeatherSet {
                    cloud_base_m: Some(299.0),
                    ..no_preset()
                },
                "the cloud base with no preset",
            ),
            (
                WeatherSet {
                    cloud_base_m: Some(5_001.0),
                    ..no_preset()
                },
                "the cloud base with no preset",
            ),
            (
                WeatherSet {
                    cloud_thickness_m: Some(199.0),
                    ..no_preset()
                },
                "the cloud thickness",
            ),
            (
                WeatherSet {
                    cloud_thickness_m: Some(2_001.0),
                    ..no_preset()
                },
                "the cloud thickness",
            ),
            (
                WeatherSet {
                    cloud_density: Some(11),
                    ..no_preset()
                },
                "0 to 10",
            ),
            (
                WeatherSet {
                    dust: Some((true, 299.0)),
                    ..d()
                },
                "the visibility in the dust",
            ),
            (
                WeatherSet {
                    dust: Some((true, 20_001.0)),
                    ..d()
                },
                "the visibility in the dust",
            ),
            (fog(sixteen_plus_one), "1 to 16 keys"),
            (fog(vec![(86_401, 1.0, 1.0)]), "past 24 hours"),
            (fog(vec![(0, 100_001.0, 1.0)]), "visibility"),
            (fog(vec![(0, 1.0, 5_001.0)]), "thickness"),
            // A thunderstorm needs a density of 9; 8 is one short.
            (
                WeatherSet {
                    cloud_density: Some(8),
                    precipitation: Some(2),
                    ..no_preset()
                },
                "density of 9",
            ),
        ];
        for (set, want) in cases {
            let e = set.merge(&template()).expect_err(want);
            assert!(e.contains(want), "{want}: {e}");
        }
        // The bounds themselves hold.
        let at = WeatherSet {
            temperature_c: Some(50.0),
            qnh_mmhg: Some(720.0),
            visibility_m: Some(80_000.0),
            ground_turbulence_mps: Some(6.0),
            cloud_base_m: Some(5_000.0),
            cloud_thickness_m: Some(2_000.0),
            cloud_density: Some(9),
            precipitation: Some(2),
            ..no_preset()
        };
        at.merge(&template()).expect("the bounds hold");
        let wind = |speed_mps| {
            Some(Wind {
                speed_mps,
                toward_deg: 0,
            })
        };
        let other_end = WeatherSet {
            temperature_c: Some(-50.0),
            qnh_mmhg: Some(790.0),
            visibility_m: Some(0.0),
            ground_turbulence_mps: Some(0.0),
            wind: [wind(0.0), wind(50.0), None],
            cloud_base_m: Some(300.0),
            cloud_thickness_m: Some(200.0),
            cloud_density: Some(0),
            ..no_preset()
        };
        other_end.merge(&template()).expect("the other bounds hold");
        for visibility in [300.0, 20_000.0] {
            WeatherSet {
                dust: Some((true, visibility)),
                ..d()
            }
            .merge(&template())
            .expect("the dust bounds hold");
        }
        fog(vec![(86_400, 100_000.0, 5_000.0)])
            .merge(&template())
            .expect("the fog bounds hold");
        fog((0..16).map(|i| (i * 10, 1.0, 1.0)).collect())
            .merge(&template())
            .expect("16 keys");
    }

    #[test]
    fn the_gates_are_checked_on_the_merged_table() {
        let refusals: Vec<(WeatherSet, &str)> = vec![
            (
                WeatherSet {
                    cloud_base_m: Some(1000.0),
                    ..WeatherSet::default()
                },
                "with preset Preset2",
            ),
            (
                WeatherSet {
                    cloud_density: Some(5),
                    ..WeatherSet::default()
                },
                "only with no preset",
            ),
            (
                WeatherSet {
                    precipitation: Some(1),
                    ..WeatherSet::default()
                },
                "only with no preset",
            ),
            (
                WeatherSet {
                    cloud_preset: Some(None),
                    cloud_density: Some(4),
                    precipitation: Some(1),
                    ..WeatherSet::default()
                },
                "density of 5",
            ),
            (
                WeatherSet {
                    cloud_preset: Some(None),
                    cloud_density: Some(9),
                    precipitation: Some(3),
                    ..WeatherSet::default()
                },
                "0 or less",
            ),
            (
                WeatherSet {
                    cloud_preset: Some(None),
                    cloud_density: Some(9),
                    precipitation: Some(1),
                    temperature_c: Some(-5.0),
                    ..WeatherSet::default()
                },
                "0 or more",
            ),
            (
                WeatherSet {
                    fog: Some(Fog::Auto),
                    dust: Some((true, 500.0)),
                    ..WeatherSet::default()
                },
                "together with dust",
            ),
            (
                WeatherSet {
                    halo: Some("off".into()),
                    halo_crystals: Some("Tangents".into()),
                    ..WeatherSet::default()
                },
                "other than off",
            ),
            (
                WeatherSet {
                    fog: Some(Fog::Manual(vec![(10, 1.0, 1.0), (10, 1.0, 1.0)])),
                    ..WeatherSet::default()
                },
                "strictly increasing",
            ),
            (
                WeatherSet {
                    fog: Some(Fog::Manual(Vec::new())),
                    ..WeatherSet::default()
                },
                "1 to 16 keys",
            ),
            (
                WeatherSet {
                    wind: [
                        None,
                        Some(Wind {
                            speed_mps: 1.0,
                            toward_deg: 360,
                        }),
                        None,
                    ],
                    ..WeatherSet::default()
                },
                "0 to 359",
            ),
            (
                WeatherSet {
                    qnh_mmhg: Some(f64::NAN),
                    ..WeatherSet::default()
                },
                "QNH",
            ),
            (
                WeatherSet {
                    cloud_preset: Some(Some("clouds01".into())),
                    ..WeatherSet::default()
                },
                "not a cloud preset",
            ),
        ];
        for (set, want) in refusals {
            let e = set.merge(&template()).expect_err(want);
            assert!(e.contains(want), "{want}: {e}");
        }
        // Fog on in the template, dust set by the controller.
        let foggy = WeatherSet {
            fog: Some(Fog::Auto),
            ..WeatherSet::default()
        }
        .merge(&template())
        .expect("fog");
        let dust = WeatherSet {
            dust: Some((true, 400.0)),
            ..WeatherSet::default()
        };
        assert!(dust.merge(&foggy).is_err());
    }

    #[test]
    fn a_template_without_clouds_or_season_is_refused() {
        let bare = Lv::Table(vec![(s("qnh"), Lv::Number(760.0))]);
        let set = WeatherSet {
            qnh_mmhg: Some(750.0),
            ..WeatherSet::default()
        };
        assert!(set.merge(&bare).expect_err("bare").contains("clouds"));
        assert!(check_whole(&bare).is_err());
        check_whole(&template()).expect("whole");
    }
}
