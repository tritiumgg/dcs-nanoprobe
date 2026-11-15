//! Orders as intents (plan 4.4).
//!
//! An order is stored as what the author wants, with a revision. Progress is
//! derived from observation: a route's progress is the number of its
//! waypoints the group has reached. [`compile`] turns an intent and the
//! group's status into the DCS task table for what remains.
//!
//! The route shape, a few off-road waypoints, is LIVE from E37's spawn-time
//! routes (step S15). The compiled Mission task table is UNVERIFIED until a
//! live `GROUP_ORDER` with it moves a group. The attack tables follow ED's
//! scripting documentation and are UNVERIFIED on a running DCS; S18's part of
//! E8 checks each one.

use nanoprobe_transport::codec::Lv;

use crate::ids::EntityId;
use crate::sim::wire::lv;

/// A route point, in DCS map coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Waypoint {
    /// DCS `x`, north, meters.
    pub x: f64,
    /// DCS `z`, east, meters.
    pub z: f64,
}

/// What a group is told to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// Drive or fly through the waypoints in order.
    Route {
        /// The waypoints.
        waypoints: Vec<Waypoint>,
        /// Meters per second.
        speed: f64,
        /// Keep to roads.
        on_road: bool,
    },
    /// Attack a group, a unit or a static.
    Attack {
        /// The target: a group, a unit, or a static.
        target: EntityId,
    },
    /// Stay where it is.
    Hold,
    /// Circle a point.
    Orbit {
        /// The point.
        at: Waypoint,
        /// Meters above sea level.
        alt: f64,
        /// Meters per second.
        speed: f64,
    },
}

/// How close a group must come to a waypoint to have reached it, in meters.
pub const REACHED_M: f64 = 50.0;

/// The longest time between two samples of a lead unit that still make a
/// segment, in model seconds. Past it the unit may have been moved, paused or
/// respawned, and a straight line between the samples says nothing of where it
/// went.
pub const SEGMENT_MAX_S: f64 = 60.0;

/// The distance from the point (`px`, `pz`) to the segment from (`ax`, `az`)
/// to (`bx`, `bz`), on the map plane.
fn segment_distance(ax: f64, az: f64, bx: f64, bz: f64, px: f64, pz: f64) -> f64 {
    let (dx, dz) = (bx - ax, bz - az);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 {
        (((px - ax) * dx + (pz - az) * dz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cz) = (ax + t * dx, az + t * dz);
    ((px - cx).powi(2) + (pz - cz).powi(2)).sqrt()
}

/// The progress of `intent` after the group is seen at a point, from its
/// earlier progress. `from` is the previous sample of the same lead unit, if
/// the caller holds one that makes a segment (same unit, same mission, within
/// [`SEGMENT_MAX_S`], no gap between).
///
/// Positions are sampled, so a group may pass a waypoint between two samples.
/// A waypoint counts as reached when the sample, or the closest point of the
/// segment from the previous sample, lies within [`REACHED_M`] of it. Only the
/// waypoints not yet reached are tested, so a segment cannot reach again what
/// an earlier one did, and a group seen at a later waypoint has passed every
/// one before it: a segment that passes two waypoints reaches both. Progress
/// never goes back.
pub fn progress(
    intent: &Intent,
    earlier: usize,
    from: Option<(f64, f64)>,
    x: f64,
    z: f64,
) -> usize {
    match intent {
        Intent::Route { waypoints, .. } => {
            let p = earlier.min(waypoints.len());
            let near = |w: &Waypoint| {
                let d = match from {
                    Some((fx, fz)) => segment_distance(fx, fz, x, z, w.x, w.z),
                    None => ((w.x - x).powi(2) + (w.z - z).powi(2)).sqrt(),
                };
                d <= REACHED_M
            };
            waypoints[p..]
                .iter()
                .rposition(near)
                .map_or(p, |j| p + j + 1)
        }
        _ => earlier,
    }
}

/// What the compiler needs to know about the group.
#[derive(Clone, Copy, Debug)]
pub struct Status {
    /// Where the group's lead unit is, if known: `x`, `z`.
    pub at: Option<(f64, f64)>,
    /// The intent's progress.
    pub progress: usize,
}

/// What [`compile`] produced.
#[derive(Clone, Debug, PartialEq)]
pub enum Compiled {
    /// The task table to send.
    Task(Lv),
    /// Nothing remains to do; no command is needed.
    Done,
    /// The task cannot be built yet, for the reason given.
    Waiting(&'static str),
}

/// A bound attack target, with the DCS ID its task names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// A group: its DCS group ID.
    Group(u64),
    /// A unit: its DCS unit ID.
    Unit(u64),
    /// A static: its DCS object ID.
    Static(u64),
}

/// The task table for what remains of `intent`. `target_of` gives the bound
/// target an attack names, `None` while DCS does not hold it.
pub fn compile(
    intent: &Intent,
    status: Status,
    target_of: impl Fn(EntityId) -> Option<Target>,
) -> Compiled {
    match intent {
        Intent::Route {
            waypoints,
            speed,
            on_road,
        } => {
            let rest = waypoints.get(status.progress..).unwrap_or(&[]);
            if rest.is_empty() {
                return Compiled::Done;
            }
            // The route keeps the author's waypoints, off road unless asked:
            // the cheapest shape that follows the intent (LIVE, E37, step S15:
            // waypoints every 50 m cost far more DCS CPU at 1,200 units).
            // Open item for a later step: E37 saw 12 to 22 % of on-road
            // groups not move (LIVE).
            let action = if *on_road { "On Road" } else { "Off Road" };
            let point = |x: f64, z: f64| {
                lv::obj(vec![
                    ("x", lv::n(x)),
                    ("y", lv::n(z)),
                    ("type", lv::s("Turning Point")),
                    ("action", lv::s(action)),
                    ("speed", lv::n(*speed)),
                ])
            };
            let mut points = Vec::with_capacity(rest.len() + 1);
            if let Some((x, z)) = status.at {
                points.push(point(x, z));
            }
            points.extend(rest.iter().map(|w| point(w.x, w.z)));
            Compiled::Task(lv::obj(vec![
                ("id", lv::s("Mission")),
                (
                    "params",
                    lv::obj(vec![("route", lv::obj(vec![("points", lv::list(points))]))]),
                ),
            ]))
        }
        Intent::Attack { target } => match target_of(*target) {
            Some(Target::Group(id)) => Compiled::Task(lv::obj(vec![
                ("id", lv::s("AttackGroup")),
                ("params", lv::obj(vec![("groupId", lv::n(id as f64))])),
            ])),
            // A unit's DCS ID is its object ID. UNVERIFIED: S18's part of E8
            // checks `AttackUnit` for a unit.
            Some(Target::Unit(id)) => Compiled::Task(attack_unit(id)),
            // UNVERIFIED: whether `AttackUnit` takes a static's object ID.
            // S18's part of E8 checks it.
            Some(Target::Static(id)) => Compiled::Task(attack_unit(id)),
            None => Compiled::Waiting("the target is not in DCS"),
        },
        Intent::Hold => Compiled::Task(lv::obj(vec![
            ("id", lv::s("Hold")),
            ("params", lv::obj(vec![])),
        ])),
        Intent::Orbit { at, alt, speed } => Compiled::Task(lv::obj(vec![
            ("id", lv::s("Orbit")),
            (
                "params",
                lv::obj(vec![
                    ("pattern", lv::s("Circle")),
                    (
                        "point",
                        lv::obj(vec![("x", lv::n(at.x)), ("y", lv::n(at.z))]),
                    ),
                    ("altitude", lv::n(*alt)),
                    ("speed", lv::n(*speed)),
                ]),
            ),
        ])),
    }
}

fn attack_unit(id: u64) -> Lv {
    lv::obj(vec![
        ("id", lv::s("AttackUnit")),
        ("params", lv::obj(vec![("unitId", lv::n(id as f64))])),
    ])
}

/// The waypoints a compiled route task holds, `x` and `z`; empty for any
/// other task. The fake sim reads routes with it.
pub fn route_points(task: &Lv) -> Vec<Waypoint> {
    if lv::str_at(task, "id").as_deref() != Some("Mission") {
        return Vec::new();
    }
    let points = lv::get(task, "params")
        .and_then(|p| lv::get(p, "route"))
        .and_then(|r| lv::get(r, "points"));
    points
        .map(|p| {
            lv::items(p)
                .into_iter()
                .filter_map(|pt| {
                    Some(Waypoint {
                        x: lv::num_at(pt, "x")?,
                        z: lv::num_at(pt, "y")?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> Intent {
        Intent::Route {
            waypoints: vec![
                Waypoint { x: 0.0, z: 0.0 },
                Waypoint { x: 1_000.0, z: 0.0 },
                Waypoint {
                    x: 1_000.0,
                    z: 1_000.0,
                },
            ],
            speed: 10.0,
            on_road: false,
        }
    }

    #[test]
    fn progress_follows_reached_waypoints_and_never_goes_back() {
        let r = route();
        assert_eq!(progress(&r, 0, None, 500.0, 0.0), 0);
        assert_eq!(progress(&r, 0, None, 10.0, 0.0), 1);
        assert_eq!(progress(&r, 1, None, 990.0, 10.0), 2);
        assert_eq!(progress(&r, 2, None, 0.0, 0.0), 2);
        assert_eq!(progress(&r, 2, None, 1_000.0, 1_000.0), 3);
        assert_eq!(progress(&r, 9, None, 0.0, 0.0), 3);
        // Seen at the last waypoint without a sample at the ones before it.
        assert_eq!(progress(&r, 0, None, 1_000.0, 1_000.0), 3);
        assert_eq!(progress(&Intent::Hold, 0, None, 0.0, 0.0), 0);
    }

    #[test]
    fn a_segment_reaches_a_waypoint_between_two_samples() {
        let r = route();
        // A fast group: samples 400 m before and 400 m past the waypoint at
        // (1000, 0), neither within 50 m of it.
        assert_eq!(progress(&r, 1, Some((600.0, 20.0)), 1_400.0, 20.0), 2);
        assert_eq!(progress(&r, 1, None, 1_400.0, 20.0), 1);
        // Last waypoint passed with no later one.
        assert_eq!(progress(&r, 2, Some((1_000.0, 600.0)), 1_000.0, 1_400.0), 3);
        // A segment passing two waypoints reaches both.
        assert_eq!(progress(&r, 0, Some((-300.0, 10.0)), 1_200.0, 10.0), 2);
        // A segment far away reaches none.
        assert_eq!(progress(&r, 0, Some((-300.0, 500.0)), 1_300.0, 500.0), 0);
        // A segment that stops short of a waypoint does not reach it.
        assert_eq!(progress(&r, 1, Some((300.0, 0.0)), 900.0, 0.0), 1);
        // Equal samples are a sample.
        assert_eq!(progress(&r, 0, Some((10.0, 0.0)), 10.0, 0.0), 1);
    }

    #[test]
    fn a_route_compiles_to_what_remains_from_where_the_group_is() {
        let r = route();
        let c = compile(
            &r,
            Status {
                at: Some((900.0, 0.0)),
                progress: 1,
            },
            |_| None,
        );
        let Compiled::Task(task) = c else {
            panic!("{c:?}")
        };
        assert_eq!(
            route_points(&task),
            vec![
                Waypoint { x: 900.0, z: 0.0 },
                Waypoint { x: 1_000.0, z: 0.0 },
                Waypoint {
                    x: 1_000.0,
                    z: 1_000.0
                },
            ]
        );
        assert_eq!(
            compile(
                &r,
                Status {
                    at: None,
                    progress: 3
                },
                |_| None
            ),
            Compiled::Done
        );
    }

    #[test]
    fn an_attack_waits_for_its_target() {
        let a = Intent::Attack {
            target: EntityId(4),
        };
        let s = Status {
            at: None,
            progress: 0,
        };
        assert!(matches!(compile(&a, s, |_| None), Compiled::Waiting(_)));
        let Compiled::Task(t) = compile(&a, s, |e| (e == EntityId(4)).then_some(Target::Group(77)))
        else {
            panic!()
        };
        assert_eq!(
            lv::num_at(lv::get(&t, "params").unwrap(), "groupId"),
            Some(77.0)
        );
        assert!(route_points(&t).is_empty());
        for (target, key) in [(Target::Unit(5), "unitId"), (Target::Static(6), "unitId")] {
            let Compiled::Task(t) = compile(&a, s, |_| Some(target)) else {
                panic!()
            };
            assert_eq!(lv::str_at(&t, "id").as_deref(), Some("AttackUnit"));
            let want = if let Target::Unit(i) | Target::Static(i) = target {
                i as f64
            } else {
                0.0
            };
            assert_eq!(lv::num_at(lv::get(&t, "params").unwrap(), key), Some(want));
        }
    }

    #[test]
    fn hold_and_orbit_compile() {
        let s = Status {
            at: None,
            progress: 0,
        };
        for i in [
            Intent::Hold,
            Intent::Orbit {
                at: Waypoint { x: 1.0, z: 2.0 },
                alt: 3_000.0,
                speed: 120.0,
            },
        ] {
            assert!(matches!(compile(&i, s, |_| None), Compiled::Task(_)));
        }
    }
}
