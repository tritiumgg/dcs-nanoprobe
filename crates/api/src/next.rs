//! What the next shell of a campaign is written with (`SetNextMission`),
//! kept in the store's `meta.world` as a Lua value (spec/persistence.md,
//! "The store file").

use nanoprobe_sidecar::shell::DynamicSpawn;
use nanoprobe_sidecar::shell::content::{Briefing, Images};
use nanoprobe_sidecar::shell::weather::{Fog, WeatherSet, Wind};
use nanoprobe_transport::codec::{Lv, Reader, Writer};

/// The next mission's settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Next {
    /// The theater to load; empty keeps the loaded one.
    pub theater: String,
    /// The date and start time, ms since the Unix epoch; `None` carries
    /// the campaign clock.
    pub start_ms: Option<i64>,
    /// The DCS weather table; `None` keeps the template's.
    pub weather: Option<Lv>,
    /// Airports whose dynamic-spawn flags are set.
    pub dynamic: Vec<DynamicSpawn>,
    /// Typed weather; `None` keeps the template's.
    pub conditions: Option<WeatherSet>,
    /// Briefing text.
    pub briefing: Briefing,
    /// Briefing images, blue, red, neutral and the coalition selection
    /// screen; `None` keeps the template's.
    pub images: Images,
}

const BRIEFING_KEYS: [&str; 5] = [
    "sortie",
    "description",
    "blue_task",
    "red_task",
    "neutral_task",
];
const IMAGE_KEYS: [&str; 4] = ["blue", "red", "neutral", "coalition_selection"];
const WIND_KEYS: [&str; 3] = ["ground", "at2000", "at8000"];

fn list(items: Vec<Lv>) -> Lv {
    Lv::Table(
        items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (Lv::Number((i + 1) as f64), v))
            .collect(),
    )
}

fn items(v: &Lv) -> Vec<&Lv> {
    match v {
        Lv::Table(p) => p.iter().map(|(_, v)| v).collect(),
        _ => Vec::new(),
    }
}

fn weather_lv(w: &WeatherSet) -> Lv {
    let mut p = Vec::new();
    let mut num = |k: &str, v: Option<f64>| {
        if let Some(v) = v {
            p.push((s(k), Lv::Number(v)));
        }
    };
    num("temperature_c", w.temperature_c);
    num("qnh_mmhg", w.qnh_mmhg);
    num("visibility_m", w.visibility_m);
    num("ground_turbulence_mps", w.ground_turbulence_mps);
    num("cloud_base_m", w.cloud_base_m);
    num("cloud_thickness_m", w.cloud_thickness_m);
    num("cloud_density", w.cloud_density.map(f64::from));
    num("precipitation", w.precipitation.map(f64::from));
    for (wind, k) in w.wind.iter().zip(WIND_KEYS) {
        if let Some(wind) = wind {
            p.push((
                s(k),
                Lv::Table(vec![
                    (s("speed_mps"), Lv::Number(wind.speed_mps)),
                    (s("toward_deg"), Lv::Number(f64::from(wind.toward_deg))),
                ]),
            ));
        }
    }
    if let Some(preset) = &w.cloud_preset {
        p.push((s("cloud_preset"), s(preset.as_deref().unwrap_or(""))));
    }
    match &w.fog {
        Some(Fog::Off) => p.push((s("fog"), s("off"))),
        Some(Fog::Auto) => p.push((s("fog"), s("auto"))),
        Some(Fog::Manual(keys)) => p.push((
            s("fog"),
            list(
                keys.iter()
                    .map(|&(t, vis, thick)| {
                        list(vec![
                            Lv::Number(f64::from(t)),
                            Lv::Number(vis),
                            Lv::Number(thick),
                        ])
                    })
                    .collect(),
            ),
        )),
        None => {}
    }
    if let Some((on, vis)) = w.dust {
        p.push((s("dust"), list(vec![Lv::Bool(on), Lv::Number(vis)])));
    }
    if let Some(h) = &w.halo {
        p.push((s("halo"), s(h)));
    }
    if let Some(c) = &w.halo_crystals {
        p.push((s("halo_crystals"), s(c)));
    }
    Lv::Table(p)
}

fn weather_of(v: &Lv) -> Result<WeatherSet, String> {
    let bad = |k: &str| format!("next mission: conditions {k}");
    let num = |k: &str| -> Result<Option<f64>, String> {
        match get(v, k) {
            None => Ok(None),
            Some(Lv::Number(n)) => Ok(Some(*n)),
            Some(_) => Err(bad(k)),
        }
    };
    let whole = |k: &str| -> Result<Option<u32>, String> {
        num(k)?
            .map(|n| {
                if n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&n) {
                    Ok(n as u32)
                } else {
                    Err(bad(k))
                }
            })
            .transpose()
    };
    let text = |k: &str| -> Result<Option<String>, String> {
        match get(v, k) {
            None => Ok(None),
            Some(Lv::String(b)) => String::from_utf8(b.clone()).map(Some).map_err(|_| bad(k)),
            Some(_) => Err(bad(k)),
        }
    };
    let mut wind = [None; 3];
    for (slot, k) in wind.iter_mut().zip(WIND_KEYS) {
        if let Some(t) = get(v, k) {
            let speed = match get(t, "speed_mps") {
                Some(Lv::Number(n)) => *n,
                _ => return Err(bad(k)),
            };
            let dir = match get(t, "toward_deg") {
                Some(Lv::Number(n)) if n.fract() == 0.0 && (0.0..=359.0).contains(n) => *n as u32,
                _ => return Err(bad(k)),
            };
            *slot = Some(Wind {
                speed_mps: speed,
                toward_deg: dir,
            });
        }
    }
    let fog = match get(v, "fog") {
        None => None,
        Some(Lv::String(b)) if b == b"off" => Some(Fog::Off),
        Some(Lv::String(b)) if b == b"auto" => Some(Fog::Auto),
        Some(t @ Lv::Table(_)) => {
            let mut keys = Vec::new();
            for k in items(t) {
                let f = items(k);
                match f.as_slice() {
                    [Lv::Number(t), Lv::Number(vis), Lv::Number(thick)]
                        if t.fract() == 0.0 && *t >= 0.0 =>
                    {
                        keys.push((*t as u32, *vis, *thick));
                    }
                    _ => return Err(bad("fog")),
                }
            }
            Some(Fog::Manual(keys))
        }
        Some(_) => return Err(bad("fog")),
    };
    let dust = match get(v, "dust").map(items).as_deref() {
        None => None,
        Some([Lv::Bool(on), Lv::Number(vis)]) => Some((*on, *vis)),
        Some(_) => return Err(bad("dust")),
    };
    Ok(WeatherSet {
        temperature_c: num("temperature_c")?,
        qnh_mmhg: num("qnh_mmhg")?,
        visibility_m: num("visibility_m")?,
        wind,
        ground_turbulence_mps: num("ground_turbulence_mps")?,
        cloud_preset: text("cloud_preset")?.map(|p| Some(p).filter(|p| !p.is_empty())),
        cloud_base_m: num("cloud_base_m")?,
        cloud_thickness_m: num("cloud_thickness_m")?,
        cloud_density: whole("cloud_density")?,
        precipitation: whole("precipitation")?,
        fog,
        dust,
        halo: text("halo")?,
        halo_crystals: text("halo_crystals")?,
    })
}

fn briefing_texts(b: &Briefing) -> [Option<&Vec<u8>>; 5] {
    [
        b.sortie.as_ref(),
        b.description.as_ref(),
        b.blue_task.as_ref(),
        b.red_task.as_ref(),
        b.neutral_task.as_ref(),
    ]
}

fn s(v: &str) -> Lv {
    Lv::String(v.as_bytes().to_vec())
}

fn get<'a>(v: &'a Lv, key: &str) -> Option<&'a Lv> {
    match v {
        Lv::Table(p) => p
            .iter()
            .find(|(k, _)| *k == Lv::String(key.as_bytes().to_vec()))
            .map(|(_, v)| v),
        _ => None,
    }
}

impl Next {
    /// As kept in the store; a weather too deep or too large to encode whole
    /// is an error, never stored as nil.
    pub fn to_lv(&self) -> Result<Lv, String> {
        let mut p = vec![(s("theater"), s(&self.theater))];
        if let Some(ms) = self.start_ms {
            p.push((s("start_ms"), s(&ms.to_string())));
        }
        // A nested Lua value is its own encoding inside a string, so the
        // row's nesting stays fixed (spec/persistence.md, "Encodings").
        if let Some(w) = &self.weather {
            nanoprobe_sidecar::store::check_lv(w)?;
            let mut wr = Writer::new();
            wr.lv(w);
            p.push((s("weather"), Lv::String(wr.finish())));
        }
        let dynamic = self
            .dynamic
            .iter()
            .enumerate()
            .map(|(i, d)| {
                (
                    Lv::Number((i + 1) as f64),
                    Lv::Table(vec![
                        (s("airbase"), s(&d.airbase.to_string())),
                        (s("enabled"), Lv::Bool(d.enabled)),
                        (s("hot_start"), Lv::Bool(d.hot_start)),
                    ]),
                )
            })
            .collect();
        p.push((s("dynamic"), Lv::Table(dynamic)));
        // Absent when unset, so a next mission without them keeps its
        // layout (spec/persistence.golden).
        if let Some(w) = &self.conditions {
            p.push((s("conditions"), weather_lv(w)));
        }
        let texts: Vec<(Lv, Lv)> = briefing_texts(&self.briefing)
            .into_iter()
            .zip(BRIEFING_KEYS)
            .filter_map(|(t, k)| t.map(|t| (s(k), Lv::String(t.clone()))))
            .collect();
        if !texts.is_empty() {
            p.push((s("briefing"), Lv::Table(texts)));
        }
        let images: Vec<(Lv, Lv)> = self
            .images
            .iter()
            .zip(IMAGE_KEYS)
            .filter_map(|(l, k)| {
                l.as_ref()
                    .map(|l| (s(k), list(l.iter().map(|n| s(n)).collect())))
            })
            .collect();
        if !images.is_empty() {
            p.push((s("images"), Lv::Table(images)));
        }
        Ok(Lv::Table(p))
    }

    /// From the store.
    pub fn from_lv(v: &Lv) -> Result<Next, String> {
        let text = |v: Option<&Lv>| match v {
            Some(Lv::String(b)) => String::from_utf8(b.clone()).ok(),
            _ => None,
        };
        let theater = text(get(v, "theater")).ok_or("next mission: theater")?;
        let start_ms = match get(v, "start_ms") {
            None => None,
            some => Some(
                text(some)
                    .and_then(|t| t.parse().ok())
                    .ok_or("next mission: start_ms")?,
            ),
        };
        let weather = match get(v, "weather") {
            None => None,
            Some(Lv::String(b)) => Some(
                Reader::new(b)
                    .lv()
                    .map_err(|e| format!("next mission: weather: {e}"))?,
            ),
            Some(_) => return Err("next mission: weather".into()),
        };
        let mut dynamic = Vec::new();
        if let Some(Lv::Table(items)) = get(v, "dynamic") {
            for (_, d) in items {
                let flag = |k| match get(d, k) {
                    Some(Lv::Bool(b)) => Ok(*b),
                    _ => Err(format!("next mission: dynamic {k}")),
                };
                dynamic.push(DynamicSpawn {
                    airbase: text(get(d, "airbase"))
                        .and_then(|t| t.parse().ok())
                        .ok_or("next mission: dynamic airbase")?,
                    enabled: flag("enabled")?,
                    hot_start: flag("hot_start")?,
                });
            }
        }
        let conditions = get(v, "conditions").map(weather_of).transpose()?;
        let mut briefing = Briefing::default();
        if let Some(b) = get(v, "briefing") {
            let slots = [
                &mut briefing.sortie,
                &mut briefing.description,
                &mut briefing.blue_task,
                &mut briefing.red_task,
                &mut briefing.neutral_task,
            ];
            for (slot, k) in slots.into_iter().zip(BRIEFING_KEYS) {
                match get(b, k) {
                    None => {}
                    Some(Lv::String(t)) => *slot = Some(t.clone()),
                    Some(_) => return Err(format!("next mission: briefing {k}")),
                }
            }
        }
        let mut images: Images = Default::default();
        if let Some(t) = get(v, "images") {
            for (slot, k) in images.iter_mut().zip(IMAGE_KEYS) {
                if let Some(l) = get(t, k) {
                    *slot = Some(
                        items(l)
                            .into_iter()
                            .map(|n| text(Some(n)).ok_or(format!("next mission: images {k}")))
                            .collect::<Result<_, _>>()?,
                    );
                }
            }
        }
        Ok(Next {
            theater,
            start_ms,
            weather,
            dynamic,
            conditions,
            briefing,
            images,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_it_writes() {
        let n = Next {
            theater: "Caucasus".into(),
            start_ms: Some(-1_000),
            weather: Some(Lv::Table(vec![(s("a"), Lv::Number(1.0))])),
            dynamic: vec![DynamicSpawn {
                airbase: 12,
                enabled: true,
                hot_start: false,
            }],
            conditions: Some(WeatherSet {
                temperature_c: Some(-3.5),
                wind: [
                    None,
                    Some(Wind {
                        speed_mps: 4.0,
                        toward_deg: 359,
                    }),
                    None,
                ],
                cloud_preset: Some(None),
                cloud_density: Some(7),
                fog: Some(Fog::Manual(vec![(0, 1.0, 2.0), (60, 3.0, 4.0)])),
                dust: Some((false, 0.0)),
                halo: Some("auto".into()),
                ..WeatherSet::default()
            }),
            briefing: Briefing {
                sortie: Some(b"S".to_vec()),
                red_task: Some(Vec::new()),
                ..Briefing::default()
            },
            images: [
                Some(vec!["a.png".into()]),
                Some(Vec::new()),
                None,
                Some(vec!["s.png".into()]),
            ],
        };
        assert_eq!(Next::from_lv(&n.to_lv().expect("encodes")), Ok(n));
        for fog in [Fog::Off, Fog::Auto] {
            let n = Next {
                conditions: Some(WeatherSet {
                    fog: Some(fog),
                    cloud_preset: Some(Some("Preset3".into())),
                    ..WeatherSet::default()
                }),
                ..Next::default()
            };
            assert_eq!(Next::from_lv(&n.to_lv().expect("encodes")), Ok(n));
        }
        let empty = Next::default();
        assert_eq!(Next::from_lv(&empty.to_lv().expect("encodes")), Ok(empty));
        assert!(Next::from_lv(&Lv::Nil).is_err());
    }

    #[allow(deprecated)] // the Struct weather stays tested
    #[test]
    fn a_weather_too_deep_to_store_is_refused() {
        let mut deep = Lv::Table(vec![]);
        for _ in 0..40 {
            deep = Lv::Table(vec![(s("k"), deep)]);
        }
        let st = crate::lua::lua_to_struct(&Lv::Table(vec![(s("w"), deep)])).expect("struct");
        let e = crate::services::mission::next_of(Some(&nanoprobe_proto::v1alpha1::NextMission {
            weather: Some(st),
            ..Default::default()
        }))
        .expect_err("too deep");
        assert_eq!(e.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn to_lv_refuses_a_weather_too_deep_instead_of_storing_nil() {
        let mut deep = Lv::Table(vec![]);
        for _ in 0..40 {
            deep = Lv::Table(vec![(s("k"), deep)]);
        }
        let n = Next {
            weather: Some(deep),
            ..Next::default()
        };
        assert!(n.to_lv().is_err());
    }

    #[test]
    fn a_start_outside_years_1_to_9999_is_refused() {
        let start = |seconds, nanos| nanoprobe_proto::v1alpha1::NextMission {
            start: Some(prost_types::Timestamp { seconds, nanos }),
            ..Default::default()
        };
        for (seconds, nanos) in [
            (i64::MIN / 1000 - 1, 0),
            (i64::MIN, 0),
            (-62_135_596_801, 0),
            (253_402_300_800, 0),
            (0, -1),
        ] {
            let e = crate::services::mission::next_of(Some(&start(seconds, nanos)))
                .expect_err("out of range");
            assert_eq!(e.code(), tonic::Code::InvalidArgument, "{seconds} {nanos}");
        }
        crate::services::mission::next_of(Some(&start(-62_135_596_800, 0))).expect("year 1");
        crate::services::mission::next_of(Some(&start(253_402_300_799, 999_999_999)))
            .expect("year 9999");
    }

    /// The store writes `Next::to_lv` as the `next_mission` of `meta.world`;
    /// the golden file pins that layout.
    #[test]
    fn to_lv_is_the_golden_next_mission() {
        let golden = include_str!("../../../spec/persistence.golden");
        let hex = golden
            .lines()
            .find_map(|l| l.strip_prefix("store.world.s19_keys "))
            .expect("the golden row")
            .trim();
        let bytes: Vec<u8> = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("hex"))
            .collect();
        let world = nanoprobe_transport::codec::Reader::new(&bytes)
            .lv()
            .expect("decodes");
        let Lv::Table(pairs) = world else {
            panic!("not a table");
        };
        let golden_next = pairs
            .into_iter()
            .find(|(k, _)| *k == s("next_mission"))
            .expect("next_mission")
            .1;
        let n = Next {
            theater: "Caucasus".into(),
            start_ms: Some(1_700_000_000_000),
            weather: Some(Lv::Table(vec![(s("k"), Lv::Bool(true))])),
            dynamic: vec![DynamicSpawn {
                airbase: 12,
                enabled: true,
                hot_start: false,
            }],
            ..Next::default()
        };
        assert_eq!(n.to_lv().expect("encodes"), golden_next);
    }
}
