//! What the next shell of a campaign is written with (`SetNextMission`),
//! kept in the store's `meta.world` as a Lua value (spec/persistence.md,
//! "The store file").

use nanoprobe_sidecar::shell::DynamicSpawn;
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
    /// As kept in the store.
    pub fn to_lv(&self) -> Lv {
        let mut p = vec![(s("theater"), s(&self.theater))];
        if let Some(ms) = self.start_ms {
            p.push((s("start_ms"), s(&ms.to_string())));
        }
        // A nested Lua value is its own encoding inside a string, so the
        // row's nesting stays fixed (spec/persistence.md, "Encodings").
        if let Some(w) = &self.weather {
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
        Lv::Table(p)
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
        Ok(Next {
            theater,
            start_ms,
            weather,
            dynamic,
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
        };
        assert_eq!(Next::from_lv(&n.to_lv()), Ok(n));
        let empty = Next::default();
        assert_eq!(Next::from_lv(&empty.to_lv()), Ok(empty));
        assert!(Next::from_lv(&Lv::Nil).is_err());
    }

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
}
