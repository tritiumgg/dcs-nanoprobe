//! `sampling`: the mirror oracle of step S10 (spec/sampling.md).
//!
//! `sampling <name> [--part ...]` is the S10 exit test on a docker-wine
//! target; see [`live`]. `sampling compare <oracle.json> <mirror.json>
//! [--no-ages] [--strict] [--recording FILE] [--out FILE]` sets the oracle, a
//! brute-force dump of every unit and static read in one frame by
//! `lua/sampling/oracle.lua`, against the sidecar's mirror as
//! `link-probe --mirror` writes it, and prints the verdict as JSON. It exits
//! 0 when the mirror matches and 1 when it does not. `sampling deaths
//! <probe.json> <recording.jsonl> [--catch-s N] [--hit-s N]` gives the death probe's verdict
//! ([`deaths`]).
//!
//! The mirror matches when:
//!
//! - every object the oracle lists is in the mirror, and the mirror lists no
//!   other, apart from objects the oracle names in `in_transit`;
//! - every mirrored position lies within the distance the object can have
//!   moved between its sample and the oracle's read, plus 1 m: the larger of
//!   its present speed and its type's top speed, times the model time
//!   between them. The present speed alone fails a unit that slowed since its
//!   sample (LIVE, S10: units in a column);
//! - no sample is older than twice `P`, the period its row is read at
//!   (spec/sampling.md, "Tiers"). For a position: the position period for an
//!   object that moves, five times it for one that does not, and 60 s, the
//!   enumeration period, for a static that cannot move. For life and fuel:
//!   the vitals period for a row a subscription names, twelve times it for
//!   any other row.
//!   Each period is the tier's target, or the period the agent last reported
//!   in the status block when that is longer; `--strict` holds to the
//!   targets, the rule at 500 objects. `--no-ages` reports ages without
//!   failing on them, at a tenth of the slice, where the rates are meant to
//!   fall;
//! - a moving object's position older than twice `P` passes with one
//!   excuse, and is counted as `late`: it is no older than six periods, the
//!   oracle read early (`lua/sampling/oracle-early.lua`, before the sample)
//!   and found the object halted, and the sample lies within 1.5 m of where
//!   it stood. Such a row was on the slow schedule and has started to move;
//! - no tier that owes rows reports a period of 0;
//! - with `--recording`, the median over objects of the interval between an
//!   object's last two samples in the recording is within 25 % of the period
//!   the agent reported: positions of the objects that move, life and fuel
//!   of the subscribed rows. Fewer than 20 objects are reported and not judged,
//!   and with `--no-ages` the figures are reported only;
//! - every mirrored life equals the oracle's, and every unit's fuel is within
//!   0.002 a second of the sample's age, plus 0.001;
//! - every row has a position and vitals, unless the mirror added it less
//!   than 1.5 times the tier's achieved period (or target, when longer) plus
//!   2 s before its last read: a tier that the budget has slowed reaches a
//!   new row only on its next sweep.
//!
//! The oracle's file is the eval reply's first value: `{model, chunks,
//! rows_n, failed, early_model}`, or `{model, rows, failed}`. The mirror's
//! file holds `periods` and `rows`, each with `object`, `pos`, `pos_model`,
//! `life`, `fuel` and `vit_model`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use serde_json::{Value, json};

/// The slack on a position, metres.
const POSITION_SLACK_M: f64 = 1.0;
/// Fuel may change this fraction a second, and this much besides.
const FUEL_RATE: f64 = 0.002;
const FUEL_SLACK: f64 = 0.001;
/// The sampler's schedule (spec/sampling.md, "Tiers"): a unit or cargo
/// static that has not moved is read once in this many position sweeps, and
/// a row no subscription names has its life and fuel read once in this many
/// vitals sweeps; a static that cannot move has its position read once an
/// enumeration, every this many seconds.
const IDLE_EVERY: f64 = 5.0;
const VITALS_EVERY: f64 = 12.0;
const ENUMERATION_S: f64 = 60.0;
/// An object moves when the oracle reads its speed above this, m/s.
const MOVING_M_S: f64 = 1.0;
/// An object was halted at the early read when its speed was at most this,
/// m/s, and its sample lies within this many metres of where it stood, plus
/// the position slack: the sampler's own stop distance.
const HALTED_M_S: f64 = 0.5;
const STOP_M: f64 = 0.5;
/// The median interval between an object's successive samples in the
/// recording may differ from the period the agent reports by this share.
const INTERVAL_SHARE: f64 = 0.25;
/// The fewest objects with two samples the recording must hold for that
/// check.
const INTERVALS_MIN: usize = 20;

/// One object as the oracle read it.
#[derive(Clone, Debug, PartialEq)]
pub struct Truth {
    /// DCS object ID.
    pub id: u64,
    /// Its name.
    pub name: String,
    /// 1 unit, 2 static.
    pub kind: u64,
    /// Position.
    pub pos: [f64; 3],
    /// Speed, m/s.
    pub speed: f64,
    /// The type's top speed, m/s (`getDesc().speedMax`); 0 for a static.
    pub top: f64,
    /// Life.
    pub life: f64,
    /// Fuel, for a unit.
    pub fuel: Option<f64>,
    /// A subscription names it, so its life and fuel are read on every
    /// vitals sweep. Any other row's are read on the slow sweep.
    pub watched: bool,
    /// A static that can move.
    pub cargo: bool,
    /// Its position and speed at the early read
    /// (`lua/sampling/oracle-early.lua`), when the oracle made one.
    pub early: Option<([f64; 3], f64)>,
}

/// One object as the mirror holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Mirrored {
    /// DCS object ID.
    pub id: u64,
    /// Position and the model time of its sample.
    pub pos: Option<([f64; 3], f64)>,
    /// Life, fuel and the model time of their sample.
    pub vitals: Option<(f64, f64, f64)>,
    /// The real time the mirror added the row.
    pub since_real: Option<f64>,
}

/// How long a new row may wait for its first samples: a tier's achieved
/// period, or its target when longer, times 1.5, plus 2 s, counted from when
/// the mirror added the row to the mirror's last read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Grace {
    /// The real time of the mirror's last `FRAME`.
    pub now_real: f64,
    /// Seconds a new row may wait for a position.
    pub position_s: f64,
    /// Seconds a new row may wait for vitals.
    pub vitals_s: f64,
    /// The oldest a position and a vitals sample may be, seconds: twice
    /// `P`, each tier's target period or, when longer and the compare is
    /// not strict, the period the agent last reported. `None` reports ages
    /// without failing on them, as at a tenth of the slice, where the rates
    /// are meant to fall.
    pub max_age_s: Option<(f64, f64)>,
    /// The model time of the oracle's early read, when it made one.
    pub early_model: Option<f64>,
}

impl Grace {
    /// From the periods the agent reported and the default targets. A
    /// `strict` compare holds every sample to its target, whatever the agent
    /// reported: the rule at 500 objects, where a slower tier is a defect.
    pub fn from_periods(now_real: f64, periods: &[f64], strict: bool) -> Grace {
        let wait = |achieved: f64, target: f64| achieved.max(target) * 1.5 + 2.0;
        let (position, vitals) = (
            periods.first().copied().unwrap_or(0.0),
            periods.get(1).copied().unwrap_or(0.0),
        );
        let p = |achieved: f64, target: f64| if strict { target } else { achieved.max(target) };
        Grace {
            now_real,
            position_s: wait(position, 1.0),
            vitals_s: wait(vitals, 5.0),
            max_age_s: Some((2.0 * p(position, 1.0), 2.0 * p(vitals, 5.0))),
            early_model: None,
        }
    }

    fn young(&self, since: Option<f64>, wait: f64) -> bool {
        since.is_some_and(|s| self.now_real - s < wait)
    }
}

/// The verdict of one comparison.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Verdict {
    /// Objects the oracle read.
    pub oracle: usize,
    /// Objects the mirror held.
    pub mirror: usize,
    /// In the oracle and not the mirror.
    pub missing: Vec<u64>,
    /// In the mirror and not the oracle.
    pub extra: Vec<u64>,
    /// In both, with no position in the mirror yet.
    pub unpositioned: Vec<u64>,
    /// Positions past the distance allowed: object and metres off.
    pub far: Vec<(u64, f64)>,
    /// Life or fuel that differs: object.
    pub vitals: Vec<u64>,
    /// Objects with no vitals yet.
    pub unvital: Vec<u64>,
    /// The largest position error, metres, and its allowance.
    pub worst: (f64, f64),
    /// Getters that raised in the oracle.
    pub oracle_failed: u64,
    /// Samples not yet taken for rows too new to have had them.
    pub young: usize,
    /// Position and vitals samples older than `max_age_s`: object and age,
    /// seconds.
    pub old: Vec<(u64, f64)>,
    pub old_vitals: Vec<(u64, f64)>,
    /// The oldest position and vitals samples, seconds.
    pub oldest: (f64, f64),
    /// Moving objects, and those of them whose position is older than
    /// twice `P` with the one excuse: no older than a slow period and one
    /// more, and halted at the sampled position when the oracle read early.
    /// They are reported and do not fail.
    pub moving: usize,
    pub late: Vec<(u64, f64)>,
    /// Tiers that owe rows and report a period of 0.
    pub zero_periods: Vec<&'static str>,
    /// For positions and vitals: the median interval between an object's
    /// successive samples in the recording, the period the agent reported,
    /// and the intervals counted; and the tiers whose median is off.
    pub intervals: Vec<(&'static str, f64, f64, usize)>,
    pub interval_off: Vec<&'static str>,
}

impl Verdict {
    /// Whether the mirror matches.
    pub fn matches(&self) -> bool {
        self.missing.is_empty()
            && self.extra.is_empty()
            && self.unpositioned.is_empty()
            && self.far.is_empty()
            && self.vitals.is_empty()
            && self.unvital.is_empty()
            && self.oracle_failed == 0
            && self.old.is_empty()
            && self.old_vitals.is_empty()
            && self.zero_periods.is_empty()
            && self.interval_off.is_empty()
    }

    /// The verdict as JSON.
    pub fn json(&self) -> Value {
        let ids = |v: &[u64]| v.iter().take(20).copied().collect::<Vec<_>>();
        json!({
            "matches": self.matches(),
            "oracle": self.oracle,
            "mirror": self.mirror,
            "missing": self.missing.len(),
            "extra": self.extra.len(),
            "unpositioned": self.unpositioned.len(),
            "far": self.far.len(),
            "vitals": self.vitals.len(),
            "unvital": self.unvital.len(),
            "worst_m": self.worst.0,
            "worst_allowed_m": self.worst.1,
            "oracle_failed": self.oracle_failed,
            "young": self.young,
            "missing_ids": ids(&self.missing),
            "extra_ids": ids(&self.extra),
            "far_ids": self.far.iter().take(20).map(|(i, d)| json!([i, d])).collect::<Vec<_>>(),
            "vitals_ids": ids(&self.vitals),
            "old": self.old.len(),
            "old_vitals": self.old_vitals.len(),
            "oldest_position_s": self.oldest.0,
            "oldest_vitals_s": self.oldest.1,
            "moving": self.moving,
            "late": self.late.len(),
            "late_ids": self.late.iter().take(20).map(|(i, a)| json!([i, a])).collect::<Vec<_>>(),
            "old_ids": self.old.iter().take(20).map(|(i, a)| json!([i, a])).collect::<Vec<_>>(),
            "old_vitals_ids": self.old_vitals.iter().take(20).map(|(i, a)| json!([i, a])).collect::<Vec<_>>(),
            "zero_periods": self.zero_periods,
            "intervals": self.intervals.iter().map(|(tier, median, reported, n)| json!({
                "tier": tier, "median_s": median, "reported_s": reported, "intervals": n,
            })).collect::<Vec<_>>(),
            "interval_off": self.interval_off,
        })
    }
}

/// Compare. `oracle_model` is the model time the oracle read at;
/// `in_transit` names objects born or killed too close to the read to count.
pub fn compare(
    oracle: &[Truth],
    oracle_model: f64,
    mirror: &[Mirrored],
    in_transit: &[u64],
    grace: Grace,
) -> Verdict {
    let truth: BTreeMap<u64, &Truth> = oracle.iter().map(|t| (t.id, t)).collect();
    let held: BTreeMap<u64, &Mirrored> = mirror.iter().map(|m| (m.id, m)).collect();
    let mut v = Verdict {
        oracle: oracle.len(),
        mirror: mirror.len(),
        ..Verdict::default()
    };
    for (id, t) in &truth {
        if in_transit.contains(id) {
            continue;
        }
        let Some(m) = held.get(id) else {
            v.missing.push(*id);
            continue;
        };
        match m.pos {
            None if grace.young(m.since_real, grace.position_s) => v.young += 1,
            None => v.unpositioned.push(*id),
            Some((p, at)) => {
                let p_sample = p;
                let d = p
                    .iter()
                    .zip(&t.pos)
                    .map(|(a, b)| (a - b) * (a - b))
                    .sum::<f64>()
                    .sqrt();
                let age = (oracle_model - at).abs();
                v.oldest.0 = v.oldest.0.max(age);
                let moving = t.speed > MOVING_M_S;
                v.moving += usize::from(moving);
                if let Some((p, _)) = grace.max_age_s {
                    if t.kind == 2 && !t.cargo {
                        // Read when added and at each enumeration.
                        if age > 2.0 * ENUMERATION_S {
                            v.old.push((*id, age));
                        }
                    } else if !moving && age > p * IDLE_EVERY {
                        v.old.push((*id, age));
                    } else if moving && age > p {
                        // The one excuse: it stood still when it was read,
                        // on the slow schedule, and has started to move since.
                        let halted = t.early.is_some_and(|(q, speed)| {
                            let d = q
                                .iter()
                                .zip(&p_sample)
                                .map(|(a, b)| (a - b) * (a - b))
                                .sum::<f64>()
                                .sqrt();
                            speed <= HALTED_M_S && d <= STOP_M + POSITION_SLACK_M
                        });
                        let after_early = grace.early_model.is_some_and(|e| at >= e);
                        if halted && after_early && age <= (IDLE_EVERY + 1.0) * p / 2.0 {
                            v.late.push((*id, age));
                        } else {
                            v.old.push((*id, age));
                        }
                    }
                }
                let allowed = t.speed.max(t.top) * age + POSITION_SLACK_M;
                if d - allowed > v.worst.0 - v.worst.1 {
                    v.worst = (d, allowed);
                }
                if d > allowed {
                    v.far.push((*id, d));
                }
            }
        }
        match m.vitals {
            None if grace.young(m.since_real, grace.vitals_s) => v.young += 1,
            None => v.unvital.push(*id),
            Some((life, fuel, at)) => {
                let age = (oracle_model - at).abs();
                v.oldest.1 = v.oldest.1.max(age);
                if grace
                    .max_age_s
                    .is_some_and(|(_, w)| age > if t.watched { w } else { w * VITALS_EVERY })
                {
                    v.old_vitals.push((*id, age));
                }
                let life_ok = (life - t.life).abs() <= 1e-4 * t.life.abs().max(1.0);
                let fuel_ok = t.fuel.is_none_or(|f| {
                    (fuel - f).abs() <= FUEL_RATE * (oracle_model - at).abs() + FUEL_SLACK
                });
                if !(life_ok && fuel_ok) {
                    v.vitals.push(*id);
                }
            }
        }
    }
    for id in held.keys() {
        if !truth.contains_key(id) && !in_transit.contains(id) {
            v.extra.push(*id);
        }
    }
    v
}

/// The oracle's rows from its JSON.
pub fn read_oracle(v: &Value) -> Result<(f64, Vec<Truth>, u64), String> {
    let model = v["model"].as_f64().ok_or("the oracle has no model time")?;
    let failed = v["failed"].as_u64().unwrap_or(0);
    // Rows come in `chunks` from the oracle snippet, which the eval
    // channel's limit on table entries requires, or as `rows`.
    let list = |v: &Value| -> Result<Vec<Value>, String> {
        match v {
            Value::Array(a) => Ok(a.clone()),
            // An empty Lua table encodes as an object.
            Value::Object(o) if o.is_empty() => Ok(Vec::new()),
            _ => Err("the oracle has no rows".into()),
        }
    };
    let rows: Vec<Value> = if v["chunks"].is_null() {
        list(&v["rows"])?
    } else {
        let mut all = Vec::new();
        for c in list(&v["chunks"])? {
            all.extend(list(&c)?);
        }
        if let Some(n) = v["rows_n"].as_u64()
            && n as usize != all.len()
        {
            return Err(format!("the oracle read {n} rows and sent {}", all.len()));
        }
        all
    };
    let rows = rows.as_slice();
    let mut out = Vec::new();
    for r in rows {
        let num = |k: &str| {
            r[k].as_f64()
                .ok_or_else(|| format!("an oracle row has no {k}: {r}"))
        };
        out.push(Truth {
            id: num("id")? as u64,
            name: r["name"].as_str().unwrap_or_default().to_owned(),
            kind: num("kind")? as u64,
            pos: [num("x")?, num("y")?, num("z")?],
            speed: num("speed")?,
            top: r["top"].as_f64().unwrap_or(0.0),
            life: num("life")?,
            fuel: r["fuel"].as_f64(),
            watched: false,
            cargo: r["cargo"].as_bool().unwrap_or(false),
            early: match (
                r["ex"].as_f64(),
                r["ey"].as_f64(),
                r["ez"].as_f64(),
                r["es"].as_f64(),
            ) {
                (Some(x), Some(y), Some(z), Some(speed)) => Some(([x, y, z], speed)),
                _ => None,
            },
        });
    }
    Ok((model, out, failed))
}

/// The mirror's rows from its JSON.
pub fn read_mirror(v: &Value) -> Result<Vec<Mirrored>, String> {
    let rows = v["rows"].as_array().ok_or("the mirror has no rows")?;
    let mut out = Vec::new();
    for r in rows {
        let id = r["object"]
            .as_u64()
            .ok_or_else(|| format!("a mirror row has no object: {r}"))?;
        let pos = match (&r["pos"], r["pos_model"].as_f64()) {
            (Value::Array(p), Some(at)) if p.len() == 3 => {
                let c: Vec<f64> = p.iter().filter_map(Value::as_f64).collect();
                (c.len() == 3).then(|| ([c[0], c[1], c[2]], at))
            }
            _ => None,
        };
        let vitals = match (r["life"].as_f64(), r["vit_model"].as_f64()) {
            (Some(life), Some(at)) => Some((life, r["fuel"].as_f64().unwrap_or(f64::NAN), at)),
            _ => None,
        };
        out.push(Mirrored {
            id,
            pos,
            vitals,
            since_real: r["since_real"].as_f64(),
        });
    }
    Ok(out)
}

fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// How [`compare_files`] judges.
#[derive(Clone, Copy, Debug, Default)]
pub struct Judge<'a> {
    /// Fail on old samples. Off at a tenth of the slice, where the rates are
    /// meant to fall and ages are reported only.
    pub ages: bool,
    /// Hold every sample to its target period, whatever the agent reported.
    pub strict: bool,
    /// The stream's recording (`link-probe --record`), or its tail: the
    /// compare then sets the intervals between an object's samples against
    /// the periods the agent reported.
    pub recording: Option<&'a Path>,
    /// The positions interval is reported, not judged against the period
    /// the agent reported at the dump: the live parts judge it over
    /// stall-free windows ([`positions`]). A reported period at one instant
    /// can be an enumeration's or a stall's.
    pub positions_by_window: bool,
}

/// The median, over the objects in `ids`, of the interval between an
/// object's last two samples in the `window_s` seconds of model time before
/// `upto`: from the `POSITIONS` records of a recording when `positions`,
/// else from its `VITALS` records. The last interval of each object is what
/// the period the agent reports at `upto` describes: its last sweep. Returns
/// the median and the objects counted.
fn median_interval(
    recording: &str,
    ids: &std::collections::BTreeSet<u64>,
    positions: bool,
    upto: f64,
    window_s: f64,
) -> (f64, usize) {
    use nanoprobe_transport::layout::kind;
    use nanoprobe_transport::records::{Frame, Positions, Vitals};

    let mut model = 0.0;
    let mut last: BTreeMap<u64, f64> = BTreeMap::new();
    let mut latest: BTreeMap<u64, f64> = BTreeMap::new();
    for line in recording.lines() {
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let k = r["kind"].as_u64().unwrap_or(0);
        let wanted = if positions {
            kind::POSITIONS
        } else {
            kind::VITALS
        };
        if k != kind::FRAME && k != wanted {
            continue;
        }
        let Some(payload) = r["payload"].as_str().and_then(unhex) else {
            continue;
        };
        if k == kind::FRAME {
            if let Ok(f) = Frame::decode(&payload) {
                model = f.model_time;
            }
            continue;
        }
        let objects = if positions {
            Positions::decode(&payload).map(|p| p.object).ok()
        } else {
            Vitals::decode(&payload).map(|v| v.object).ok()
        };
        if model < upto - window_s || model > upto {
            continue;
        }
        for id in objects.into_iter().flatten().filter(|id| ids.contains(id)) {
            if let Some(before) = last.insert(id, model)
                && model > before
            {
                latest.insert(id, model - before);
            }
        }
    }
    let mut intervals: Vec<f64> = latest.into_values().collect();
    intervals.sort_by(f64::total_cmp);
    let median = intervals.get(intervals.len() / 2).copied().unwrap_or(0.0);
    (median, intervals.len())
}

/// The mirror dump's real time and the five achieved periods. A dump that
/// lacks either carries no evidence for the age and period checks, so it is
/// an error.
fn read_evidence(mj: &Value) -> Result<(f64, Vec<f64>), String> {
    let real = mj["real"]
        .as_f64()
        .filter(|r| r.is_finite() && *r > 0.0)
        .ok_or("the mirror dump has no `real` time (a finite number above 0)")?;
    let periods: Option<Vec<f64>> = mj["periods"].as_array().and_then(|a| {
        a.iter()
            .map(|v| v.as_f64().filter(|p| p.is_finite()))
            .collect()
    });
    match periods {
        Some(p) if p.len() == 5 => Ok((real, p)),
        _ => Err("the mirror dump has no `periods` (an array of 5 numbers)".into()),
    }
}

/// Compare two files and return the verdict.
pub fn compare_files(oracle: &Path, mirror: &Path, judge: Judge<'_>) -> Result<Verdict, String> {
    let o = read_json(oracle)?;
    let (model, mut truth, failed) = read_oracle(&o)?;
    let mj = read_json(mirror)?;
    let m = read_mirror(&mj)?;
    // The mirror's file lists the objects the sidecar subscribed.
    let subscribed: Vec<u64> = mj["subscribed"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    for t in &mut truth {
        t.watched = subscribed.contains(&t.id);
    }
    let (real, periods) = read_evidence(&mj)?;
    let mut grace = Grace::from_periods(real, &periods, judge.strict);
    grace.early_model = o["early_model"].as_f64();
    if !judge.ages {
        grace.max_age_s = None;
    }
    let transit: Vec<u64> = o["in_transit"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let mut v = compare(&truth, model, &m, &transit, grace);
    v.oracle_failed = failed;
    // A tier that owes rows never reports 0 (spec/sampling.md, "Achieved
    // period"): positions with a unit or a cargo static, vitals and the
    // enumeration with any object. Ammunition owes rows only for units that
    // fired or that a subscription names, which the oracle does not know.
    let swept = truth.iter().any(|t| t.kind == 1 || t.cargo);
    let period = |i: usize| periods.get(i).copied().unwrap_or(0.0);
    for (name, i, owes) in [
        ("positions", 0, swept),
        ("vitals", 1, !truth.is_empty()),
        ("enumeration", 4, !truth.is_empty()),
    ] {
        if owes && periods.len() > i && period(i) <= 0.0 {
            v.zero_periods.push(name);
        }
    }
    if let Some(path) = judge.recording {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Rows read every sweep: the objects moving now, and the
        // subscribed rows.
        let moving = truth
            .iter()
            .filter(|t| (t.kind == 1 || t.cargo) && t.speed > MOVING_M_S)
            .map(|t| t.id)
            .collect();
        let watched = truth.iter().filter(|t| t.watched).map(|t| t.id).collect();
        for (name, ids, positions, i) in [
            ("positions", &moving, true, 0),
            ("vitals", &watched, false, 1),
        ] {
            let reported = period(i);
            if reported <= 0.0 {
                continue;
            }
            let window = (8.0 * reported).clamp(30.0, 120.0);
            let (median, n) = median_interval(&text, ids, positions, model, window);
            v.intervals.push((name, median, reported, n));
            // Reported, not judged, where ages are not: at a tenth of the
            // slice a sweep can stall behind an overdue enumeration, and
            // the period the agent reports then grows past the intervals
            // of the sweeps before it.
            if judge.ages
                && !(positions && judge.positions_by_window)
                && n >= INTERVALS_MIN
                && (median - reported).abs() > INTERVAL_SHARE * reported
            {
                v.interval_off.push(name);
            }
        }
    }
    Ok(v)
}

const USAGE: &str = "usage: cargo xtask sampling compare <oracle.json> <mirror.json> [--no-ages] [--strict] [--recording FILE] [--out FILE]\n       cargo xtask sampling deaths <probe.json> <recording.jsonl> [--catch-s N] [--hit-s N]\n       cargo xtask sampling --rejudge <bundle dir>\n       cargo xtask sampling <name> [--part mirror|mirror500|tenth|probes|design|deaths|windows]";

/// `cargo xtask sampling ...`.
pub fn command(repo: &Path, args: &[&str]) -> Result<ExitCode, String> {
    match args {
        ["--rejudge", dir] => live::rejudge(Path::new(dir)),
        [name] => live::run(repo, name, Part::Both),
        [name, "--part", "mirror"] => live::run(repo, name, Part::Mirror),
        [name, "--part", "mirror500"] => live::run(repo, name, Part::Mirror500),
        [name, "--part", "tenth"] => live::run(repo, name, Part::Tenth),
        [name, "--part", "probes"] => live::run(repo, name, Part::Probes),
        [name, "--part", "design"] => live::run(repo, name, Part::Design),
        [name, "--part", "deaths"] => live::run(repo, name, Part::Deaths),
        [name, "--part", "windows"] => live::run(repo, name, Part::Windows),
        ["deaths", probe, recording, rest @ ..] => {
            let (mut catch_s, mut hit_s) = (None, None);
            let mut rest = rest;
            while let [flag, n, tail @ ..] = rest {
                let n = n.parse::<f64>().map_err(|e| format!("{flag}: {e}"))?;
                match *flag {
                    "--catch-s" => catch_s = Some(n),
                    "--hit-s" => hit_s = Some(n),
                    _ => return Err(USAGE.to_string()),
                }
                rest = tail;
            }
            if !rest.is_empty() {
                return Err(USAGE.to_string());
            }
            let v = deaths(
                &read_json(Path::new(probe))?,
                Path::new(recording),
                catch_s,
                hit_s,
            )?;
            println!("{v}");
            Ok(if v["ok"] == json!(true) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        ["compare", oracle, mirror, rest @ ..] => {
            // `--no-ages` reports old samples without failing on them, for a
            // tenth of the slice, where the rates are meant to fall.
            let mut judge = Judge {
                ages: true,
                ..Judge::default()
            };
            let mut out = None;
            let mut rest = rest;
            loop {
                rest = match rest {
                    ["--no-ages", rest @ ..] => {
                        judge.ages = false;
                        rest
                    }
                    ["--strict", rest @ ..] => {
                        judge.strict = true;
                        rest
                    }
                    ["--recording", f, rest @ ..] => {
                        judge.recording = Some(Path::new(f));
                        rest
                    }
                    ["--out", f, rest @ ..] => {
                        out = Some(*f);
                        rest
                    }
                    [] => break,
                    _ => return Err(USAGE.into()),
                };
            }
            let v = compare_files(Path::new(oracle), Path::new(mirror), judge)?;
            let text = v.json().to_string();
            println!("{text}");
            if let Some(f) = out {
                std::fs::write(f, format!("{text}\n")).map_err(|e| format!("{f}: {e}"))?;
            }
            Ok(if v.matches() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        _ => Err(USAGE.into()),
    }
}

/// Which parts of the live run to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Both, mirror first.
    Both,
    /// The mirror at 500 and 2,000 objects, on the release build.
    Mirror,
    /// The mirror at 500 objects only, on the release build.
    Mirror500,
    /// The same run at k = 1 and k = 0.1, on the build with fault injection.
    Tenth,
    /// The read-only cost probes at 500, 1,200 and 2,000 objects, with no
    /// sidecar attached.
    Probes,
    /// The design probes at 500 objects, with no sidecar attached: what
    /// tells a cargo static from another, the static list's cost as it
    /// grows, fuel over time, and the events of a fight.
    Design,
    /// The death probe, with the stand-in sidecar attached: the ways for a
    /// unit or static to cease to exist, the events each sends, and the
    /// reason each row leaves the agent's table with.
    Deaths,
    /// On a windows-ssh target: DCS started through its task, a hosted
    /// private session, 1,200 and 2,000 objects measured for three minutes
    /// each. The default part of such a target.
    Windows,
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn reason_name(code: u32) -> &'static str {
    match code {
        1 => "DEAD",
        2 => "DESTROYED",
        3 => "MISSING",
        4 => "REMOVED",
        _ => "UNKNOWN",
    }
}

/// A kill that sent a HIT or BDA is `DEAD` within this many seconds of
/// `isExist` turning false (DECISIONS 2026-11-14 00:38, item 7).
const HIT_KILL_S: f64 = 0.5;

/// A kill that sent neither is `DEAD` within one life sweep (60 s) plus this
/// many seconds, or one enumeration period plus them.
const CATCH_MARGIN_S: f64 = 2.0;

/// The life sweep's period, seconds (owner, 2026-11-14 00:53).
const LIFE_SWEEP_S: f64 = 60.0;

/// A `DEAD` the agent read is followed by a death event within this many
/// seconds, or counted as unconfirmed.
const CONFIRM_S: f64 = 300.0;

/// What the verdict needs of each kind of object that ceased to exist, at
/// least (DECISIONS 2026-11-14 00:38 item 7, amended 01:27 item 1).
const REQUIRED: [(&str, u64); 6] = [
    ("weapon", 10),
    ("explosion", 3),
    ("unit", 5),
    ("group", 5),
    ("static_weapon", 3),
    ("wreck", 1),
];

/// One watched object of `lua/sampling/deaths.lua`, with the records the
/// agent wrote for it.
#[derive(Clone, Debug, Default)]
struct Death {
    name: String,
    stage: String,
    /// Model time `isExist` was first seen false.
    gone_at: Option<f64>,
    /// The `OBJECT_GONE` records, in order: reason code and model time.
    records: Vec<(u32, f64)>,
    /// How many `OBJECT` records the agent wrote for it: more than one is a
    /// revival.
    objects: Vec<f64>,
    /// A HIT or BDA named the object as target by this model time.
    triggered: bool,
    /// The bound for a kill that sent a HIT or BDA; 0 is [`HIT_KILL_S`].
    hit_s: f64,
    /// Model times of the death events (DEAD, UNIT_LOST, CRASH) that named it.
    death_events: Vec<f64>,
    /// `removed` when `Unit.isDead` or `StaticObject.getLife` raised at the
    /// end of the probe.
    read_end: Option<String>,
}

/// The kind a stage counts as in [`REQUIRED`].
fn bucket(stage: &str) -> &str {
    match stage {
        "gun" | "air" => "weapon",
        other => other,
    }
}

/// The verdict on one object, as messages for what failed. `catch_s` is the
/// longest an object with no HIT or BDA waits for its row to go.
fn judge_one(d: &Death, catch_s: f64) -> Vec<String> {
    let mut out = Vec::new();
    let Some(gone) = d.gone_at else {
        // A control that was never lost gets no record.
        if matches!(d.stage.as_str(), "control" | "bystander") && !d.records.is_empty() {
            out.push(format!(
                "{}: a control got {} records",
                d.name,
                d.records.len()
            ));
        }
        return out;
    };
    let first = d.records.first().copied();
    let latency = first.map(|r| r.1 - gone);
    let hit_s = if d.hit_s > 0.0 { d.hit_s } else { HIT_KILL_S };
    let bound = if d.triggered { hit_s } else { catch_s };
    let expect = |reason: u32, name: &str, out: &mut Vec<String>| match first {
        None => out.push(format!("{}: no record, expected {name}", d.name)),
        Some((r, _)) if r != reason => out.push(format!(
            "{}: left as {}, expected {name}",
            d.name,
            reason_name(r)
        )),
        Some(_) => {}
    };
    match d.stage.as_str() {
        "gun" | "air" | "explosion" | "wreck" | "static_weapon" | "static_explosion" => {
            expect(1, "DEAD", &mut out);
            if first.is_some_and(|r| r.0 == 1) {
                if let Some(l) = latency.filter(|l| *l > bound) {
                    out.push(format!(
                        "{}: DEAD {l:.2} s after isExist turned false, bound {bound:.2} s",
                        d.name
                    ));
                }
                if d.records.len() > 1 {
                    out.push(format!(
                        "{}: {} records after a DEAD",
                        d.name,
                        d.records.len() - 1
                    ));
                }
            }
        }
        "unit" | "group" | "static_remove" => {
            expect(4, "REMOVED", &mut out);
            if d.records.len() > 1 {
                out.push(format!(
                    "{}: a removal was followed by {}",
                    d.name,
                    reason_name(d.records[1].0)
                ));
            }
        }
        "shootdown" => {
            // An aircraft's row leaves as MISSING and its events correct it;
            // an event before the check makes it DEAD at once.
            let named_first = first.is_some_and(|r| d.death_events.iter().any(|t| *t <= r.1));
            match (first.map(|r| r.0), d.records.get(1).map(|r| r.0)) {
                (Some(3), second) => {
                    let events = !d.death_events.is_empty();
                    if events && second != Some(1) {
                        out.push(format!(
                            "{}: MISSING with a death event and no DEAD",
                            d.name
                        ));
                    }
                    if !events && second.is_some() {
                        out.push(format!("{}: a second record with no death event", d.name));
                    }
                }
                (Some(1), None) if named_first => {}
                (Some(r), _) => out.push(format!("{}: left as {}", d.name, reason_name(r))),
                (None, _) => out.push(format!("{}: no record", d.name)),
            }
            if d.records.len() > 2 {
                out.push(format!("{}: {} records", d.name, d.records.len()));
            }
        }
        _ => {}
    }
    out
}

/// The verdict of the death probe on `deaths`, a pure function of what the
/// probe and the recording saw.
///
/// `ok` when every count of [`REQUIRED`] is met and no object failed: a kill
/// that sent HIT or BDA is `DEAD` within 0.5 s of `isExist` turning false;
/// any other kill within `catch_s` (a life sweep or an enumeration period,
/// plus 2 s); every removal `REMOVED` and none corrected; a destroy on a
/// burning wreck leaves the `DEAD` standing; an aircraft leaves `MISSING`
/// and then `DEAD` by its events; a control gets no record. A `DEAD` the
/// read gave is followed by a death event within 300 s, or the object
/// answers as removed at the end; the others are counted as unconfirmed,
/// not failed. Statics are judged as one category: one failure fails it.
fn judge_deaths(deaths: &[Death], catch_s: f64) -> Value {
    let mut kills: BTreeMap<String, u64> = BTreeMap::new();
    let mut failures = Vec::new();
    let mut static_failures = 0;
    let mut by_read = 0u64;
    let mut unconfirmed = Vec::new();
    let mut late_dead = Vec::new();
    let mut out = serde_json::Map::new();
    let mut reasons: BTreeMap<&str, u64> = BTreeMap::new();
    for d in deaths {
        if d.gone_at.is_some() {
            *kills.entry(bucket(&d.stage).to_string()).or_default() += 1;
        }
        let failed = judge_one(d, catch_s);
        if d.stage.starts_with("static") {
            static_failures += failed.len();
        }
        failures.extend(failed.iter().cloned());
        for (r, _) in &d.records {
            *reasons.entry(reason_name(*r)).or_default() += 1;
        }
        if let Some((1, t)) = d.records.first().copied()
            && !d.death_events.iter().any(|e| *e <= t)
        {
            by_read += 1;
            let followed = d.death_events.iter().any(|e| *e > t && *e - t <= CONFIRM_S);
            if !followed && d.read_end.as_deref() != Some("removed") {
                unconfirmed.push(json!(d.name));
            }
        }
        if d.records.len() > 1 && d.records[0].0 == 3 && d.records[1].0 == 1 {
            late_dead.push(json!(d.name));
        }
        out.insert(
            d.name.clone(),
            json!({
                "stage": d.stage,
                "ceased": d.gone_at.is_some(),
                "triggered": d.triggered,
                "objects": d.objects,
                "records": d.records.iter().map(|(r, t)| json!({
                    "reason": reason_name(*r),
                    "t": t,
                    "after_gone_s": d.gone_at.map(|g| t - g),
                })).collect::<Vec<_>>(),
                "death_events_after_gone_s": d.death_events.iter()
                    .map(|t| d.gone_at.map(|g| t - g)).collect::<Vec<_>>(),
                "read_at_end": d.read_end,
                "failed": failed,
            }),
        );
    }
    let short: Vec<Value> = REQUIRED
        .iter()
        .filter(|(k, n)| kills.get(*k).copied().unwrap_or(0) < *n)
        .map(|(k, n)| {
            json!(format!(
                "{k}: {} of {n}",
                kills.get(*k).copied().unwrap_or(0)
            ))
        })
        .collect();
    let complete = short.is_empty();
    json!({
        "ok": complete && failures.is_empty(),
        "complete": complete,
        "short": short,
        "kills": kills,
        "required": REQUIRED.iter().map(|(k, n)| (k.to_string(), *n)).collect::<BTreeMap<_, _>>(),
        "failures": failures,
        "statics_ok": static_failures == 0,
        "records": reasons,
        "dead_by_read": by_read,
        "unconfirmed_dead": unconfirmed,
        "late_dead_after_missing": late_dead,
        "catch_s": catch_s,
        "units": out,
    })
}

/// The death probe's verdict: what `lua/sampling/deaths.lua` saw of each
/// watched object, set against the stream the stand-in sidecar recorded
/// (`link-probe --record`): the `OBJECT_GONE` records the agent wrote for it,
/// in order, and the death events that named it. Times are model time, read
/// from the `FRAME` before each record. [`judge_deaths`] gives the verdict.
/// `catch_s` is the longest a kill that sent no HIT or BDA may wait; `None`
/// is a life sweep plus 2 s. `hit_s` is the bound after a HIT or BDA; `None`
/// is 0.5 s. The host rehearsal widens it: its debug build pumps rarely.
pub fn deaths(
    probe: &Value,
    recording: &Path,
    catch_s: Option<f64>,
    hit_s: Option<f64>,
) -> Result<Value, String> {
    use nanoprobe_transport::layout::kind;
    use nanoprobe_transport::records::{Frame, Object, ObjectGone};

    let units = probe["units"]
        .as_object()
        .ok_or("the probe's reply has no units")?;
    let ids: BTreeMap<u64, &str> = units
        .iter()
        .filter_map(|(name, u)| Some((u["id"].as_u64()?, name.as_str())))
        .collect();
    let ended = probe["ended"].as_f64().unwrap_or(f64::INFINITY);
    let text =
        std::fs::read_to_string(recording).map_err(|e| format!("{}: {e}", recording.display()))?;
    let mut model = 0.0;
    let mut records: BTreeMap<u64, Vec<(u32, f64)>> = BTreeMap::new();
    let mut objects: BTreeMap<u64, Vec<f64>> = BTreeMap::new();
    for line in text.lines() {
        let Ok(r) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(payload) = r["payload"].as_str().and_then(unhex) else {
            continue;
        };
        match r["kind"].as_u64().unwrap_or(0) {
            kind::FRAME => {
                if let Ok(f) = Frame::decode(&payload) {
                    model = f.model_time;
                }
            }
            kind::OBJECT => {
                if let Ok(o) = Object::decode(&payload)
                    && ids.contains_key(&o.object)
                    && model <= ended
                {
                    objects.entry(o.object).or_default().push(model);
                }
            }
            kind::OBJECT_GONE => {
                if let Ok(g) = ObjectGone::decode(&payload)
                    && ids.contains_key(&g.object)
                    && model <= ended
                {
                    records.entry(g.object).or_default().push((g.reason, model));
                }
            }
            _ => {}
        }
    }
    let mut facts = Vec::new();
    for (name, u) in units {
        let mut d = Death {
            name: name.clone(),
            stage: u["stage"].as_str().unwrap_or("").to_string(),
            gone_at: u["gone_at"].as_f64(),
            records: u["id"]
                .as_u64()
                .and_then(|id| records.get(&id))
                .cloned()
                .unwrap_or_default(),
            objects: u["id"]
                .as_u64()
                .and_then(|id| objects.get(&id))
                .cloned()
                .unwrap_or_default(),
            read_end: u["read_at_end"].as_str().map(str::to_string),
            hit_s: hit_s.unwrap_or(0.0),
            ..Death::default()
        };
        let gone = d.gone_at.unwrap_or(f64::INFINITY);
        for e in u["events"].as_array().into_iter().flatten() {
            let (Some(t), Some(name)) = (e["t"].as_f64(), e["e"].as_str()) else {
                continue;
            };
            match name {
                "S_EVENT_BDA" if e["as"] == "target" && t <= gone + HIT_KILL_S => {
                    d.triggered = true
                }
                "S_EVENT_DEAD" | "S_EVENT_UNIT_LOST" | "S_EVENT_CRASH" => d.death_events.push(t),
                _ => {}
            }
        }
        if u["noisy"]["S_EVENT_HIT:target"]["first"]
            .as_f64()
            .is_some_and(|t| t <= gone + HIT_KILL_S)
        {
            d.triggered = true;
        }
        d.death_events.sort_by(f64::total_cmp);
        facts.push(d);
    }
    let catch = catch_s.unwrap_or(LIFE_SWEEP_S + CATCH_MARGIN_S);
    let mut v = judge_deaths(&facts, catch);
    v["isexist_calls"] = probe["isexist_calls"].clone();
    v["isexist_us"] = probe["isexist_us"].clone();
    v["seconds"] = probe["seconds"].clone();
    v["watch_s"] = probe["watch_s"].clone();
    v["counts"] = probe["counts"].clone();
    Ok(v)
}

#[path = "sampling_live.rs"]
pub(crate) mod live;
#[path = "sampling_positions.rs"]
pub(crate) mod positions;
#[path = "sampling_windows.rs"]
mod win;

#[cfg(test)]
mod tests {
    use super::*;

    fn truth(id: u64, x: f64, speed: f64) -> Truth {
        Truth {
            id,
            name: format!("u{id}"),
            kind: 1,
            pos: [x, 0.0, 0.0],
            speed,
            top: 0.0,
            life: 3.0,
            fuel: Some(1.0),
            watched: false,
            cargo: false,
            early: None,
        }
    }

    fn held(id: u64, x: f64, at: f64) -> Mirrored {
        Mirrored {
            id,
            pos: Some(([x, 0.0, 0.0], at)),
            vitals: Some((3.0, 1.0, at)),
            since_real: Some(0.0),
        }
    }

    #[test]
    fn a_moving_object_may_be_as_far_as_it_could_have_gone() {
        let v = compare(
            &[truth(1, 110.0, 10.0)],
            20.0,
            &[held(1, 100.0, 19.0)],
            &[],
            Grace::default(),
        );
        assert!(v.matches(), "{v:?}");
        let v = compare(
            &[truth(1, 120.0, 10.0)],
            20.0,
            &[held(1, 100.0, 19.0)],
            &[],
            Grace::default(),
        );
        assert_eq!(v.far.len(), 1);
    }

    /// LIVE, S10: a unit in a column that stopped after its sample is
    /// within reach of its top speed, not of its present speed of 0.
    #[test]
    fn a_unit_that_slowed_may_be_as_far_as_its_top_speed_reaches() {
        let mut t = truth(1, 130.0, 0.0);
        t.top = 6.0;
        let v = compare(
            &[t.clone()],
            20.0,
            &[held(1, 100.0, 15.0)],
            &[],
            Grace::default(),
        );
        assert!(v.matches(), "{v:?}");
        let v = compare(&[t], 20.0, &[held(1, 170.0, 15.0)], &[], Grace::default());
        assert_eq!(v.far.len(), 1);
    }

    fn aged() -> Grace {
        Grace {
            max_age_s: Some((2.0, 10.0)),
            early_model: Some(13.0),
            ..Grace::default()
        }
    }

    /// An object that does not move is read once in five sweeps: its
    /// position fails past twice five periods, with its age, however close
    /// the position. Its life and fuel, read once in twelve vitals sweeps,
    /// fail past twice that.
    #[test]
    fn an_unmoved_object_s_sample_fails_past_twice_its_slow_period() {
        let v = compare(
            &[truth(1, 100.0, 0.0)],
            20.0,
            &[held(1, 100.0, 11.0)],
            &[],
            aged(),
        );
        assert!(v.matches(), "{v:?}");
        let v = compare(
            &[truth(1, 100.0, 0.0)],
            20.0,
            &[held(1, 100.0, 9.0)],
            &[],
            aged(),
        );
        assert_eq!(v.old, vec![(1, 11.0)]);
        assert!(v.old_vitals.is_empty() && !v.matches());
        let mut m = held(1, 100.0, 300.0);
        m.vitals = Some((3.0, 1.0, 175.0));
        let v = compare(&[truth(1, 100.0, 0.0)], 300.0, &[m.clone()], &[], aged());
        assert_eq!(v.old_vitals, vec![(1, 125.0)]);
        // A subscribed row's life and fuel are read every sweep.
        let mut t = truth(1, 100.0, 0.0);
        t.watched = true;
        m.vitals = Some((3.0, 1.0, 289.0));
        let v = compare(&[t], 300.0, &[m], &[], aged());
        assert_eq!(v.old_vitals, vec![(1, 11.0)]);
    }

    /// A moving object's position fails past twice the period, with one
    /// excuse: it is no older than a slow period and one more, and the early
    /// read shows it halted where the sample has it. Then it was read on
    /// the slow schedule and started to move since (S10 ruling on the
    /// adoption brief, point 13).
    #[test]
    fn a_moving_object_s_late_sample_passes_only_if_it_was_halted_when_read() {
        // Sampled at 15.0 standing at x = 100; at 20 it moves at 5 m/s.
        let mut t = truth(1, 110.0, 5.0);
        let m = [held(1, 100.0, 15.0)];
        // No early read: late with no excuse.
        let v = compare(&[t.clone()], 20.0, &m, &[], aged());
        assert_eq!(v.old, vec![(1, 5.0)]);
        assert!(!v.matches());
        // Halted at the sampled position at the early read.
        t.early = Some(([100.2, 0.0, 0.0], 0.0));
        let v = compare(&[t.clone()], 20.0, &m, &[], aged());
        assert_eq!((v.late.clone(), v.old.len()), (vec![(1, 5.0)], 0));
        assert!(v.matches(), "{v:?}");
        // Already moving at the early read, or standing somewhere else.
        t.early = Some(([100.2, 0.0, 0.0], 2.0));
        assert_eq!(compare(&[t.clone()], 20.0, &m, &[], aged()).old.len(), 1);
        t.early = Some(([97.0, 0.0, 0.0], 0.0));
        assert_eq!(compare(&[t.clone()], 20.0, &m, &[], aged()).old.len(), 1);
        // Older than a slow period and one more: 6 s at a 1 s period.
        t.early = Some(([100.2, 0.0, 0.0], 0.0));
        let v = compare(&[t.clone()], 21.5, &m, &[], aged());
        assert_eq!(v.old, vec![(1, 6.5)]);
        // Sampled before the early read: the read proves nothing about it.
        let v = compare(&[t.clone()], 20.0, &[held(1, 100.0, 12.5)], &[], aged());
        assert_eq!(v.old.len(), 1);
        // A fresh sample needs no excuse.
        let v = compare(
            &[truth(1, 105.0, 5.0)],
            20.0,
            &[held(1, 100.0, 19.0)],
            &[],
            aged(),
        );
        assert!(v.matches() && v.late.is_empty(), "{v:?}");
    }

    /// A dump with no real time or no five periods is refused, not judged
    /// against the targets.
    #[test]
    fn a_dump_without_evidence_is_an_error() {
        use serde_json::json;
        let ok = json!({"real": 12.5, "periods": [1.0, 2.0, 3.0, 4.0, 5.0]});
        assert_eq!(read_evidence(&ok).unwrap().1.len(), 5);
        for bad in [
            json!({"periods": [1.0, 2.0, 3.0, 4.0, 5.0]}),
            json!({"real": null, "periods": [1.0, 2.0, 3.0, 4.0, 5.0]}),
            json!({"real": 0.0, "periods": [1.0, 2.0, 3.0, 4.0, 5.0]}),
            json!({"real": 12.5}),
            json!({"real": 12.5, "periods": null}),
            json!({"real": 12.5, "periods": [1.0, 2.0]}),
            json!({"real": 12.5, "periods": [1.0, 2.0, null, 4.0, 5.0]}),
        ] {
            assert!(read_evidence(&bad).is_err(), "{bad}");
        }
    }

    /// A strict compare holds samples to the targets; otherwise the period
    /// the agent reported, when longer, sets the limit (S10 lever ruling 5).
    #[test]
    fn the_reported_period_sets_the_limit_unless_the_compare_is_strict() {
        let slowed = Grace::from_periods(0.0, &[1.6, 5.0], false);
        assert_eq!(slowed.max_age_s, Some((3.2, 10.0)));
        let strict = Grace::from_periods(0.0, &[1.6, 5.0], true);
        assert_eq!(strict.max_age_s, Some((2.0, 10.0)));
        let on_target = Grace::from_periods(0.0, &[0.9, 4.0], false);
        assert_eq!(on_target.max_age_s, Some((2.0, 10.0)));
    }

    #[test]
    fn missing_and_extra_objects_fail_unless_in_transit() {
        let v = compare(
            &[truth(1, 0.0, 0.0), truth(2, 0.0, 0.0)],
            0.0,
            &[held(1, 0.0, 0.0), held(3, 0.0, 0.0)],
            &[],
            Grace::default(),
        );
        assert_eq!((v.missing.clone(), v.extra.clone()), (vec![2], vec![3]));
        assert!(!v.matches());
        let v = compare(
            &[truth(1, 0.0, 0.0), truth(2, 0.0, 0.0)],
            0.0,
            &[held(1, 0.0, 0.0), held(3, 0.0, 0.0)],
            &[2, 3],
            Grace::default(),
        );
        assert!(v.matches());
    }

    #[test]
    fn life_must_match_and_fuel_may_drift_with_age() {
        let mut m = held(1, 0.0, 10.0);
        m.vitals = Some((2.0, 1.0, 10.0));
        assert_eq!(
            compare(&[truth(1, 0.0, 0.0)], 10.0, &[m], &[], Grace::default()).vitals,
            vec![1]
        );
        let mut m = held(1, 0.0, 0.0);
        m.vitals = Some((3.0, 0.985, 0.0));
        assert!(compare(&[truth(1, 0.0, 0.0)], 10.0, &[m], &[], Grace::default()).matches());
    }

    #[test]
    fn a_new_row_may_wait_one_slowed_period_for_its_samples() {
        let m = Mirrored {
            id: 1,
            pos: None,
            vitals: None,
            since_real: Some(95.0),
        };
        let grace = Grace::from_periods(100.0, &[1.0, 20.0], false);
        let v = compare(
            &[truth(1, 0.0, 0.0)],
            0.0,
            std::slice::from_ref(&m),
            &[],
            grace,
        );
        assert_eq!((v.unpositioned.len(), v.unvital.len(), v.young), (1, 0, 1));
        let grace = Grace::from_periods(100.0, &[4.0, 20.0], false);
        assert!(compare(&[truth(1, 0.0, 0.0)], 0.0, &[m], &[], grace).matches());
    }

    #[test]
    fn an_empty_lua_table_reads_as_no_rows() {
        let (model, rows, failed) =
            read_oracle(&json!({"model": 1.5, "rows": {}, "failed": 0})).unwrap();
        assert_eq!((model, rows.len(), failed), (1.5, 0, 0));
    }
    fn death(stage: &str, name: &str, gone: f64, records: &[(u32, f64)]) -> Death {
        Death {
            name: name.to_string(),
            stage: stage.to_string(),
            gone_at: Some(gone),
            records: records.to_vec(),
            ..Death::default()
        }
    }

    /// A set that meets every count of [`REQUIRED`], each object right.
    fn passing() -> Vec<Death> {
        let mut v = Vec::new();
        for i in 0..10 {
            let mut d = death("gun", &format!("g{i}"), 5.0, &[(1, 5.3)]);
            d.triggered = true;
            d.death_events = vec![40.0];
            v.push(d);
        }
        for i in 0..3 {
            v.push(death("explosion", &format!("e{i}"), 20.0, &[(1, 45.0)]));
            let mut w = death("wreck", &format!("w{i}"), 20.0, &[(1, 45.0)]);
            w.read_end = Some("removed".to_string());
            v.push(w);
        }
        for i in 0..5 {
            v.push(death("unit", &format!("u{i}"), 30.0, &[(4, 30.1)]));
            v.push(death("group", &format!("r{i}"), 30.0, &[(4, 30.1)]));
        }
        for i in 0..3 {
            let mut s = death("static_weapon", &format!("s{i}"), 8.0, &[(1, 8.2)]);
            s.triggered = true;
            v.push(s);
        }
        let mut air = death("shootdown", "a1", 9.0, &[(3, 9.2), (1, 50.0)]);
        air.death_events = vec![49.0];
        v.push(air);
        v.push(Death {
            name: "c1".into(),
            stage: "control".into(),
            ..Death::default()
        });
        v
    }

    #[test]
    fn the_death_verdict_passes_a_set_that_meets_the_ruling() {
        let v = judge_deaths(&passing(), 62.0);
        assert_eq!(v["ok"], json!(true), "{v}");
        assert_eq!(v["late_dead_after_missing"], json!(["a1"]));
        // Six explosion and wreck kills read DEAD with no event before them,
        // and none has an event after it: unconfirmed, and not a failure.
        assert_eq!(v["unconfirmed_dead"].as_array().unwrap().len(), 3 + 3);
    }

    #[test]
    fn the_death_verdict_fails_each_rule_it_states() {
        let mut v = passing();
        v[0].records = vec![(1, 5.6)];
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(false),
            "a HIT kill past 0.5 s"
        );
        let mut v = passing();
        v[0].triggered = false;
        v[0].records = vec![(1, 40.0)];
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(true),
            "no HIT, within a sweep"
        );
        v[0].records = vec![(1, 70.0)];
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(false),
            "no HIT, after a sweep"
        );
        let mut v = passing();
        v[1].records = vec![(3, 5.1)];
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(false),
            "a weapon kill as MISSING"
        );
        let mut v = passing();
        let n = v.iter().position(|d| d.stage == "unit").unwrap();
        v[n].records.push((1, 60.0));
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(false),
            "a removal corrected"
        );
        let mut v = passing();
        let n = v.iter().position(|d| d.stage == "wreck").unwrap();
        v[n].records.push((4, 70.0));
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(false),
            "a wreck's DEAD followed"
        );
        let mut v = passing();
        let n = v.iter().position(|d| d.stage == "static_weapon").unwrap();
        v[n].records = vec![(3, 8.2)];
        let r = judge_deaths(&v, 62.0);
        assert_eq!(
            (r["ok"].clone(), r["statics_ok"].clone()),
            (json!(false), json!(false))
        );
        let mut v = passing();
        v.last_mut().unwrap().records = vec![(3, 1.0)];
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(false),
            "a control got a record"
        );
        let mut v = passing();
        let n = v.iter().position(|d| d.stage == "shootdown").unwrap();
        v[n].records.pop();
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(false),
            "MISSING left uncorrected"
        );
        v[n].death_events.clear();
        assert_eq!(
            judge_deaths(&v, 62.0)["ok"],
            json!(true),
            "MISSING with no event"
        );
    }

    #[test]
    fn the_death_verdict_counts_what_the_ruling_requires() {
        let mut v = passing();
        v.retain(|d| d.stage != "group");
        let r = judge_deaths(&v, 62.0);
        assert_eq!(r["complete"], json!(false));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["short"], json!(["group: 0 of 5"]));
        let mut v = passing();
        v.retain(|d| d.stage != "gun");
        assert_eq!(judge_deaths(&v, 62.0)["short"], json!(["weapon: 0 of 10"]));
    }

    #[test]
    fn a_dead_the_read_gave_is_confirmed_by_an_event_within_300_s() {
        let mut v = passing();
        let n = v.iter().position(|d| d.stage == "explosion").unwrap();
        v[n].death_events = vec![75.0];
        let r = judge_deaths(&v, 62.0);
        assert_eq!(r["unconfirmed_dead"].as_array().unwrap().len(), 5);
        v[n].death_events = vec![45.0 + 301.0];
        let r = judge_deaths(&v, 62.0);
        assert_eq!(r["unconfirmed_dead"].as_array().unwrap().len(), 6);
        assert_eq!(r["ok"], json!(true), "an unconfirmed DEAD is not a failure");
    }
}
