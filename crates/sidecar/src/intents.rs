//! Orders as intents (plan 4.4).
//!
//! An order is stored as what the author wants, with a revision. Progress is
//! derived from observation: a route's progress is the number of its
//! waypoints the group has reached. [`compile`] turns an intent and the
//! group's status into the DCS task table for what remains.
//!
//! The task tables follow ED's scripting documentation and are UNVERIFIED on
//! a running DCS until step S15 checks each one.

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
    /// Attack a group.
    Attack {
        /// The target group.
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

/// The progress of `intent` after the group is seen at a point, from its
/// earlier progress. Positions are sampled, so a group may pass a waypoint
/// between two samples: a group seen at a later waypoint has passed every
/// one before it. Progress never goes back.
pub fn progress(intent: &Intent, earlier: usize, x: f64, z: f64) -> usize {
    match intent {
        Intent::Route { waypoints, .. } => {
            let p = earlier.min(waypoints.len());
            let near = |w: &Waypoint| {
                let (dx, dz) = (w.x - x, w.z - z);
                dx * dx + dz * dz <= REACHED_M * REACHED_M
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

/// The task table for what remains of `intent`. `group_of` gives the DCS
/// group ID of a bound target group.
pub fn compile(
    intent: &Intent,
    status: Status,
    group_of: impl Fn(EntityId) -> Option<u64>,
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
        Intent::Attack { target } => match group_of(*target) {
            Some(id) => Compiled::Task(lv::obj(vec![
                ("id", lv::s("AttackGroup")),
                ("params", lv::obj(vec![("groupId", lv::n(id as f64))])),
            ])),
            None => Compiled::Waiting("the target group is not in DCS"),
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
        assert_eq!(progress(&r, 0, 500.0, 0.0), 0);
        assert_eq!(progress(&r, 0, 10.0, 0.0), 1);
        assert_eq!(progress(&r, 1, 990.0, 10.0), 2);
        assert_eq!(progress(&r, 2, 0.0, 0.0), 2);
        assert_eq!(progress(&r, 2, 1_000.0, 1_000.0), 3);
        assert_eq!(progress(&r, 9, 0.0, 0.0), 3);
        // Seen at the last waypoint without a sample at the ones before it.
        assert_eq!(progress(&r, 0, 1_000.0, 1_000.0), 3);
        assert_eq!(progress(&Intent::Hold, 0, 0.0, 0.0), 0);
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
        let Compiled::Task(t) = compile(&a, s, |e| (e == EntityId(4)).then_some(77)) else {
            panic!()
        };
        assert_eq!(
            lv::num_at(lv::get(&t, "params").unwrap(), "groupId"),
            Some(77.0)
        );
        assert!(route_points(&t).is_empty());
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
