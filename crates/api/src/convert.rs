//! Conversions between the store's types and the API's (spec/world.md).

use std::collections::BTreeMap;

use nanoprobe_proto::errors::status;
use nanoprobe_proto::v1alpha1::{
    self as pb, EntityKind, EntityState, ErrorReason, GroupCategory, WorldFilter, entity, order,
};
use nanoprobe_sidecar::ids::EntityId;
use nanoprobe_sidecar::intents::{Intent, Waypoint};
use nanoprobe_sidecar::lifecycle::State;
use nanoprobe_sidecar::store::{Group, Kind, Point, Store, Unit};
use prost::Message;
use tonic::{Code, Status};

use crate::lua::lua_to_struct;

/// `INVALID_FIELD` about a request field.
pub fn invalid(field: &str, message: impl Into<String>) -> Status {
    status(
        Code::InvalidArgument,
        ErrorReason::InvalidField,
        message,
        0,
        field,
    )
}

/// `ENTITY_NOT_FOUND`.
pub fn not_found(id: u64) -> Status {
    status(
        Code::NotFound,
        ErrorReason::EntityNotFound,
        format!("no entity {id} in the campaign"),
        id,
        "",
    )
}

/// A call a later step builds: `UNIMPLEMENTED`, with the method named in
/// the detail's field.
pub fn not_built(what: &str) -> Status {
    status(
        Code::Unimplemented,
        ErrorReason::Unspecified,
        format!("{what} is not built yet"),
        0,
        what,
    )
}

/// Milliseconds since the Unix epoch of a timestamp.
pub fn ms_of_timestamp(t: &prost_types::Timestamp) -> i64 {
    t.seconds
        .saturating_mul(1000)
        .saturating_add(i64::from(t.nanos / 1_000_000))
}

/// A finite number, or `INVALID_FIELD`.
pub fn finite(v: f64, field: &str) -> Result<f64, Status> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(invalid(field, "a number must be finite"))
    }
}

/// The API's state for a lifecycle state.
pub fn state(s: State) -> EntityState {
    match s {
        State::Virtual => EntityState::Virtual,
        State::Materializing => EntityState::Materializing,
        State::Live => EntityState::Live,
        State::Dematerializing => EntityState::Dematerializing,
        State::Dead => EntityState::Dead,
        State::Lost => EntityState::Lost,
        State::Unspawnable => EntityState::Unspawnable,
    }
}

/// DCS's `Group.Category` number for an API category (the API's numbers are
/// DCS's plus one).
pub fn dcs_category(c: GroupCategory) -> Option<u32> {
    match c {
        GroupCategory::Unspecified => None,
        other => u32::try_from(other as i32 - 1).ok(),
    }
}

fn category(dcs: u32) -> GroupCategory {
    i32::try_from(dcs)
        .ok()
        .and_then(|n| GroupCategory::try_from(n + 1).ok())
        .unwrap_or(GroupCategory::Unspecified)
}

/// An API position from a store point on a theater.
pub fn position(theater: &str, p: Point) -> pb::Position {
    pb::Position {
        theater: theater.to_owned(),
        north: p.x,
        east: p.z,
        alt: p.y,
    }
}

/// Author components as the API shows them. Each is stored as an encoded
/// `google.protobuf.Struct`.
pub fn components(c: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, prost_types::Struct> {
    c.iter()
        .filter_map(|(k, v)| {
            prost_types::Struct::decode(v.as_slice())
                .ok()
                .map(|s| (k.clone(), s))
        })
        .collect()
}

/// A sampled life or fuel value. The store keeps 0 for both until the
/// first sample, so a unit with both at 0 has none.
fn sampled(v: f32, u: &Unit) -> Option<f64> {
    (u.state.life != 0.0 || u.state.fuel != 0.0).then_some(f64::from(v))
}

fn life(u: &Unit) -> Option<f64> {
    if u.dead {
        Some(0.0)
    } else {
        sampled(u.state.life, u)
    }
}

fn unit(u: &Unit, theater: &str) -> pb::Unit {
    pb::Unit {
        id: u.id.0,
        group_id: u.group.0,
        r#type: u.type_name.clone(),
        label: u.label.clone(),
        alive: !u.dead,
        position: Some(position(theater, u.state.at)),
        attitude: Some(pb::Attitude {
            heading: u.state.heading,
            pitch: u.captured.pitch,
            roll: u.captured.roll,
        }),
        life_max: (u.captured.life0 > 0.0).then_some(u.captured.life0),
        life: life(u),
        fuel: sampled(u.state.fuel, u),
        left: if u.dead {
            pb::UnitLeft::Unspecified
        } else if u.removed {
            pb::UnitLeft::Removed
        } else if u.missing {
            pb::UnitLeft::Missing
        } else {
            pb::UnitLeft::Unspecified
        } as i32,
        ..pb::Unit::default()
    }
}

/// The API coalition of a DCS coalition number (0 neutral, 1 red, 2 blue).
pub fn coalition_of_dcs(c: u32) -> pb::Coalition {
    match c {
        0 => pb::Coalition::Neutral,
        1 => pb::Coalition::Red,
        2 => pb::Coalition::Blue,
        _ => pb::Coalition::Unspecified,
    }
}

/// The DCS coalition number of an API coalition, `None` for unspecified or
/// a value the API does not define.
pub fn dcs_coalition(c: i32) -> Option<u32> {
    match pb::Coalition::try_from(c) {
        Ok(pb::Coalition::Neutral) => Some(0),
        Ok(pb::Coalition::Red) => Some(1),
        Ok(pb::Coalition::Blue) => Some(2),
        _ => None,
    }
}

/// The coalition of a group or static: from the `OBJECT` records of its
/// objects while DCS holds them, else from the country map the agent gave
/// (spec/commands.md, `COUNTRIES`), else unspecified.
fn coalition(store: &Store, g: &Group) -> i32 {
    let materialized = matches!(g.state, State::Live | State::Dematerializing);
    let dcs = g
        .observed_coalition
        .filter(|_| materialized)
        .or_else(|| store.country_coalition(g.country));
    dcs.map_or(pb::Coalition::Unspecified, coalition_of_dcs) as i32
}

/// The API entity of a store group or static.
pub fn entity(store: &Store, g: &Group) -> pb::Entity {
    let kind = match g.kind {
        Kind::Group => entity::Kind::Group(pb::Group {
            id: g.id.0,
            label: g.label.clone(),
            managed: true,
            state: state(g.state) as i32,
            coalition: coalition(store, g),
            country: g.country,
            category: category(g.category) as i32,
            units: g
                .members
                .iter()
                .filter_map(|m| store.unit(*m))
                .map(|u| unit(u, &g.theater))
                .collect(),
            spec_revision: g.spec_rev,
            applied_spec_revision: 0,
            order_revision: g.order_rev,
            applied_order_revision: 0,
            last_error: g.error.clone().unwrap_or_default(),
            theater: g.theater.clone(),
            components: components(&g.components),
        }),
        Kind::Static => {
            let u = g.members.first().and_then(|m| store.unit(*m));
            entity::Kind::Static(pb::Static {
                id: g.id.0,
                label: g.label.clone(),
                managed: true,
                r#type: u.map(|u| u.type_name.clone()).unwrap_or_default(),
                coalition: coalition(store, g),
                country: g.country,
                alive: u.is_some_and(|u| !u.dead),
                position: u.map(|u| position(&g.theater, u.state.at)),
                heading: u.map_or(0.0, |u| u.state.heading),
                life: u.and_then(life),
                state: state(g.state) as i32,
                last_error: g.error.clone().unwrap_or_default(),
                spec_revision: g.spec_rev,
                applied_spec_revision: 0,
            })
        }
    };
    pb::Entity { kind: Some(kind) }
}

/// The further DCS keys of a group, unit or static, as the API shows them.
pub fn extra(v: &nanoprobe_transport::codec::Lv) -> Option<prost_types::Struct> {
    lua_to_struct(v)
}

/// A checked [`WorldFilter`].
#[derive(Clone, Debug, Default)]
pub struct Filter {
    groups: bool,
    statics: bool,
    coalitions: Vec<i32>,
    area: Option<(String, f64, f64, f64)>,
}

impl Filter {
    /// Check a request's filter. `needs_area` is for `QueryArea`.
    pub fn new(f: Option<&WorldFilter>, needs_area: bool) -> Result<Filter, Status> {
        let f = f.cloned().unwrap_or_default();
        let mut groups = f.kinds.is_empty();
        let mut statics = f.kinds.is_empty();
        for k in &f.kinds {
            match EntityKind::try_from(*k) {
                Ok(EntityKind::Group) => groups = true,
                Ok(EntityKind::Static) => statics = true,
                Ok(EntityKind::Unit) => {
                    return Err(invalid(
                        "filter.kinds",
                        "units are listed inside their groups; ask for ENTITY_KIND_GROUP",
                    ));
                }
                Ok(EntityKind::Unspecified) | Err(_) => {
                    return Err(invalid("filter.kinds", format!("unknown kind {k}")));
                }
                // Airbases, marks, drawings and menu items are not served yet.
                Ok(_) => {}
            }
        }
        let area = match &f.center {
            Some(c) => {
                if c.theater.is_empty() {
                    return Err(invalid(
                        "filter.center.theater",
                        "a center names its theater",
                    ));
                }
                if !(c.north.is_finite() && c.east.is_finite()) {
                    return Err(invalid("filter.center", "coordinates must be finite"));
                }
                if !(f.radius.is_finite() && f.radius > 0.0) {
                    return Err(invalid(
                        "filter.radius",
                        "a radius above 0 is required with a center",
                    ));
                }
                Some((c.theater.clone(), c.north, c.east, f.radius))
            }
            None if needs_area => {
                return Err(invalid(
                    "filter.center",
                    "QueryArea needs a center and a radius",
                ));
            }
            None => None,
        };
        Ok(Filter {
            groups,
            statics,
            coalitions: f.coalitions,
            area,
        })
    }

    /// Whether a store group or static passes the kind and coalition parts
    /// of the filter. The coalition is the one [`entity`] reports, so an
    /// entity whose coalition is unspecified passes no coalition filter.
    pub fn kind_passes(&self, store: &Store, g: &Group) -> bool {
        let kind_ok = match g.kind {
            Kind::Group => self.groups,
            Kind::Static => self.statics,
        };
        kind_ok
            && !g.removed
            && (self.coalitions.is_empty() || self.coalitions.contains(&coalition(store, g)))
    }

    /// Whether a store group or static passes: its kind and coalition, and
    /// a live member inside the area.
    pub fn passes(&self, store: &Store, g: &Group) -> bool {
        if !self.kind_passes(store, g) {
            return false;
        }
        match &self.area {
            None => true,
            Some((theater, ..)) => {
                *theater == g.theater
                    && g.members
                        .iter()
                        .filter_map(|m| store.unit(*m))
                        .any(|u| !u.dead && self.unit_in_area(u))
            }
        }
    }

    /// The groups and statics that may pass, before [`Filter::passes`]: those
    /// with a unit in the area's grid cells, or every one.
    pub fn candidates(&self, store: &Store) -> Vec<EntityId> {
        match &self.area {
            None => store.groups().map(|g| g.id).collect(),
            Some((theater, n, e, r)) => {
                let mut v: Vec<EntityId> = store
                    .grid()
                    .within(theater, *n, *e, *r)
                    .into_iter()
                    .filter_map(|u| store.unit(u).map(|u| u.group))
                    .collect();
                v.sort();
                v.dedup();
                v
            }
        }
    }

    /// Whether a unit is in the filter's area, for position batches.
    pub fn unit_in_area(&self, u: &Unit) -> bool {
        match &self.area {
            None => true,
            Some((_, n, e, r)) => {
                let (dx, dz) = (u.state.at.x - n, u.state.at.z - e);
                dx * dx + dz * dz <= r * r
            }
        }
    }
}

fn waypoint(p: Option<&pb::Position>, theater: &str, field: &str) -> Result<Waypoint, Status> {
    let p = p.ok_or_else(|| invalid(field, "a position is required"))?;
    if p.theater != theater {
        return Err(invalid(
            field,
            format!(
                "the position is on `{}`; the group is on `{theater}`",
                p.theater
            ),
        ));
    }
    if !(p.north.is_finite() && p.east.is_finite()) {
        return Err(invalid(field, "coordinates must be finite"));
    }
    Ok(Waypoint {
        x: p.north,
        z: p.east,
    })
}

/// The store's intent for an API order on a group on `theater`, checked
/// against the campaign's store.
pub fn intent(
    o: Option<&pb::Order>,
    theater: &str,
    field: &str,
    store: &Store,
) -> Result<Intent, Status> {
    let o = o
        .and_then(|o| o.intent.as_ref())
        .ok_or_else(|| invalid(field, "an order needs an intent"))?;
    Ok(match o {
        order::Intent::Route(r) => {
            let first = r.waypoints.first().ok_or_else(|| {
                invalid(
                    &format!("{field}.route.waypoints"),
                    "a route needs a waypoint",
                )
            })?;
            let mut points = Vec::with_capacity(r.waypoints.len());
            for (i, w) in r.waypoints.iter().enumerate() {
                let f = format!("{field}.route.waypoints[{i}]");
                if w.speed.to_bits() != first.speed.to_bits() || w.on_road != first.on_road {
                    return Err(invalid(
                        &f,
                        "every waypoint of a route has the same speed and road flag for now",
                    ));
                }
                if w.extra.as_ref().is_some_and(|e| !e.fields.is_empty()) {
                    return Err(invalid(
                        &format!("{f}.extra"),
                        "waypoint extra keys are not built yet",
                    ));
                }
                points.push(waypoint(
                    w.position.as_ref(),
                    theater,
                    &format!("{f}.position"),
                )?);
            }
            if !(first.speed.is_finite() && first.speed >= 0.0) {
                return Err(invalid(
                    &format!("{field}.route.waypoints[0].speed"),
                    "a speed is finite and 0 or more",
                ));
            }
            Intent::Route {
                waypoints: points,
                speed: first.speed,
                on_road: first.on_road,
            }
        }
        order::Intent::Attack(a) => {
            let target = EntityId(a.target_id);
            // A group, a static, or a unit of a group; not a removed one.
            if !store.attackable(target) {
                return Err(not_found(a.target_id));
            }
            Intent::Attack { target }
        }
        order::Intent::Hold(_) => Intent::Hold,
        order::Intent::Orbit(o) => {
            if o.radius != 0.0 {
                return Err(invalid(
                    &format!("{field}.orbit.radius"),
                    "an orbit radius is not built yet",
                ));
            }
            if !(o.speed.is_finite() && o.speed >= 0.0) {
                return Err(invalid(
                    &format!("{field}.orbit.speed"),
                    "a speed is finite and 0 or more",
                ));
            }
            let c = o.center.as_ref();
            let at = waypoint(c, theater, &format!("{field}.orbit.center"))?;
            Intent::Orbit {
                at,
                alt: finite(
                    c.map_or(0.0, |c| c.alt),
                    &format!("{field}.orbit.center.alt"),
                )?,
                speed: o.speed,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nanoprobe_sidecar::ids::CampaignId;
    use nanoprobe_sidecar::store::{Authority, NewGroup, NewUnit, UnitState};
    use nanoprobe_transport::codec::Lv;

    fn store() -> (Store, EntityId, EntityId, EntityId) {
        let mut s = Store::new(CampaignId(1), 100);
        let g = s
            .create_group(NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: vec![NewUnit::new("M-113", UnitState::default())],
                authority: Authority::Sim,
                label: "g".into(),
                extra: Lv::Nil,
            })
            .expect("group");
        let st = s
            .create_static(NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 0,
                coalition: 2,
                units: vec![NewUnit::new("Tent", UnitState::default())],
                authority: Authority::Sim,
                label: "s".into(),
                extra: Lv::Nil,
            })
            .expect("static");
        let u = s.group(g).expect("g").members[0];
        (s, g, st, u)
    }

    fn at(n: f64, e: f64) -> Option<pb::Position> {
        Some(pb::Position {
            theater: "Caucasus".into(),
            north: n,
            east: e,
            alt: 0.0,
        })
    }

    fn wp(speed: f64, on_road: bool) -> pb::Waypoint {
        pb::Waypoint {
            position: at(1.0, 2.0),
            speed,
            on_road,
            extra: None,
        }
    }

    fn order(i: order::Intent) -> pb::Order {
        pb::Order { intent: Some(i) }
    }

    fn field_of(e: &Status) -> String {
        nanoprobe_proto::errors::detail(e).expect("detail").field
    }

    #[test]
    fn orders_convert_and_refuse_what_is_not_built() {
        let (s, g, st, u) = store();
        let route = order(order::Intent::Route(pb::RouteOrder {
            waypoints: vec![wp(5.0, true), wp(5.0, true)],
        }));
        assert!(matches!(
            intent(Some(&route), "Caucasus", "order", &s),
            Ok(Intent::Route { speed, on_road: true, .. }) if speed == 5.0
        ));
        let mixed = order(order::Intent::Route(pb::RouteOrder {
            waypoints: vec![wp(5.0, false), wp(6.0, false)],
        }));
        let e = intent(Some(&mixed), "Caucasus", "order", &s).expect_err("mixed");
        assert_eq!(field_of(&e), "order.route.waypoints[1]");
        let elsewhere = order(order::Intent::Route(pb::RouteOrder {
            waypoints: vec![wp(5.0, false)],
        }));
        assert!(intent(Some(&elsewhere), "Syria", "order", &s).is_err());
        let orbit = |radius: f64, speed: f64| {
            order(order::Intent::Orbit(pb::OrbitOrder {
                center: at(0.0, 0.0),
                speed,
                radius,
            }))
        };
        assert!(intent(Some(&orbit(0.0, 100.0)), "Caucasus", "order", &s).is_ok());
        let e = intent(Some(&orbit(500.0, 100.0)), "Caucasus", "order", &s).expect_err("r");
        assert_eq!(field_of(&e), "order.orbit.radius");
        let e = intent(Some(&orbit(0.0, f64::NAN)), "Caucasus", "order", &s).expect_err("nan");
        assert_eq!(field_of(&e), "order.orbit.speed");
        let attack = |t: EntityId| order(order::Intent::Attack(pb::AttackOrder { target_id: t.0 }));
        assert!(intent(Some(&attack(g)), "Caucasus", "order", &s).is_ok());
        // A static and a unit are targets too.
        for t in [st, u] {
            assert!(matches!(
                intent(Some(&attack(t)), "Caucasus", "order", &s),
                Ok(Intent::Attack { target }) if target == t
            ));
        }
        let e = intent(Some(&attack(EntityId(999))), "Caucasus", "order", &s).expect_err("none");
        assert_eq!(e.code(), Code::NotFound);
        // A removed group, a removed static and a unit of a removed group.
        let mut s = s;
        s.remove_entity(g).expect("remove g");
        s.remove_entity(st).expect("remove st");
        for t in [g, st, u] {
            let e = intent(Some(&attack(t)), "Caucasus", "order", &s).expect_err("removed");
            assert_eq!(e.code(), Code::NotFound);
        }
        assert!(intent(None, "Caucasus", "order", &s).is_err());
    }

    #[test]
    fn a_coalition_comes_from_objects_while_materialized_and_from_the_country_map_otherwise() {
        let (mut s, g, _, _) = store();
        let country = s.group(g).expect("g").country;
        let of = |s: &Store| {
            let e = entity(s, s.group(g).expect("g"));
            let Some(entity::Kind::Group(pg)) = e.kind else {
                panic!("group")
            };
            pg.coalition
        };
        assert_eq!(of(&s), pb::Coalition::Unspecified as i32);
        s.set_countries(Some([(country, 1)].into_iter().collect()));
        assert_eq!(of(&s), pb::Coalition::Red as i32);
        // The state is not live: the objects' coalition is not used.
        s.observe_coalition(g, 2);
        assert_eq!(of(&s), pb::Coalition::Red as i32);
        s.set_countries(Some(Default::default()));
        assert_eq!(of(&s), pb::Coalition::Unspecified as i32);
    }

    #[test]
    fn a_coalition_filter_uses_the_coalition_the_entity_reports() {
        let (mut s, g, _, _) = store();
        let country = s.group(g).expect("g").country;
        let filter = |c: pb::Coalition| {
            Filter::new(
                Some(&WorldFilter {
                    coalitions: vec![c as i32],
                    ..WorldFilter::default()
                }),
                false,
            )
            .expect("filter")
        };
        let grp = |s: &Store| s.group(g).expect("g").clone();
        // Unspecified until the country map arrives.
        assert!(!filter(pb::Coalition::Red).passes(&s, &grp(&s)));
        s.set_countries(Some([(country, 1)].into_iter().collect()));
        assert!(filter(pb::Coalition::Red).passes(&s, &grp(&s)));
        assert!(!filter(pb::Coalition::Blue).passes(&s, &grp(&s)));
    }

    #[test]
    fn entities_show_the_store_fields() {
        let (mut s, g, st, u) = store();
        s.set_label(u, "lead").expect("label");
        let comp = prost_types::Struct {
            fields: [(
                "fuel".to_owned(),
                prost_types::Value {
                    kind: Some(prost_types::value::Kind::NumberValue(3.0)),
                },
            )]
            .into(),
        };
        s.set_component(g, "supply", Some(comp.encode_to_vec()))
            .expect("component");
        let e = entity(&s, s.group(g).expect("g"));
        let Some(entity::Kind::Group(pg)) = e.kind else {
            panic!("group")
        };
        assert_eq!(pg.label, "g");
        assert_eq!(pg.units[0].label, "lead");
        assert_eq!(pg.components["supply"], comp);
        assert_eq!(pg.category, GroupCategory::Ground as i32);
        // Never sampled: no life or fuel.
        assert_eq!(pg.units[0].life, None);
        assert_eq!(pg.units[0].fuel, None);
        s.observe_vitals(u, 0.5, 0.0);
        let e = entity(&s, s.group(g).expect("g"));
        let Some(entity::Kind::Group(pg)) = e.kind else {
            panic!("group")
        };
        assert_eq!(pg.units[0].life, Some(0.5));
        assert_eq!(pg.units[0].fuel, Some(0.0));
        let e = entity(&s, s.group(st).expect("s"));
        assert!(matches!(e.kind, Some(entity::Kind::Static(ref x)) if x.r#type == "Tent"));
    }

    #[test]
    fn an_area_filter_needs_a_live_unit_inside() {
        let (mut s, g, _, u) = store();
        let f = Filter::new(
            Some(&WorldFilter {
                center: at(0.0, 0.0),
                radius: 100.0,
                ..WorldFilter::default()
            }),
            true,
        )
        .expect("filter");
        let grp = s.group(g).expect("g").clone();
        assert!(f.passes(&s, &grp));
        assert!(f.candidates(&s).contains(&g));
        s.observe_position(
            u,
            Point {
                x: 1000.0,
                y: 0.0,
                z: 0.0,
            },
        );
        assert!(!f.passes(&s, s.group(g).expect("g")));
        assert!(f.kind_passes(&s, s.group(g).expect("g")));
        assert!(Filter::new(None, true).is_err());
    }
}

#[cfg(test)]
mod left_tests {
    use nanoprobe_sidecar::ids::CampaignId;
    use nanoprobe_sidecar::store::{LeftAs, NewGroup, NewUnit};
    use nanoprobe_transport::codec::Lv;

    use super::*;

    #[test]
    fn a_unit_shows_how_it_left() {
        let mut s = Store::new(CampaignId(1), 10);
        let g = s
            .create_group(NewGroup {
                theater: "Caucasus".into(),
                country: 2,
                category: 2,
                coalition: 2,
                units: (0..4)
                    .map(|_| NewUnit::new("M-113", Default::default()))
                    .collect(),
                authority: nanoprobe_sidecar::store::Authority::Sim,
                label: String::new(),
                extra: Lv::Nil,
            })
            .expect("group");
        let m = s.group(g).expect("g").members.clone();
        s.left(m[1], 11, LeftAs::Missing);
        s.left(m[2], 12, LeftAs::Removed);
        s.left(m[3], 13, LeftAs::Missing);
        s.death(m[3]).expect("death");
        let left = |u| unit(s.unit(u).expect("unit"), "Caucasus").left;
        assert_eq!(left(m[0]), pb::UnitLeft::Unspecified as i32, "alive");
        assert_eq!(left(m[1]), pb::UnitLeft::Missing as i32);
        assert_eq!(left(m[2]), pb::UnitLeft::Removed as i32);
        assert_eq!(left(m[3]), pb::UnitLeft::Unspecified as i32, "dead");
    }
}
