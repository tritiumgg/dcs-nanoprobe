//! Typed drawings (spec/world.md, "Drawings"; ADR 0052).
//!
//! A drawing is a [`DrawingShape`]: a kind with its geometry and a style. The
//! store keeps it, and the reconciler sends its lowering: the DCS call's
//! shape code and arguments ([`DrawingShape::lower`]). Every DCS markup call
//! is made from a lowering, so only the kinds below reach DCS (LIVE,
//! docs/results/2026-11-25-drawing-layers.md and
//! docs/results/2026-11-26-typed-shapes.md).
//!
//! Ovals and rounded rectangles are expanded into freeform points, one every
//! 10 degrees, as the mission editor draws its own (ED-CODE,
//! `MissionEditor/modules/me_draw_panel.lua`, 36 points a full turn).

use nanoprobe_transport::codec::Lv;

use crate::sim::wire::lv;

/// The `markupToAll` shape codes, and the code of a `circleToAll` circle
/// (spec/commands.md, "Codes: drawing"), as the `DRAWING_SET` field takes
/// them.
pub mod shape {
    use nanoprobe_transport::layout::drawing as code;

    /// A line through two points.
    pub const LINE: u32 = code::LINE as u32;
    /// A polygon through its points: a line with two.
    pub const POLYGON: u32 = code::POLYGON as u32;
    /// An arrow from one point to another.
    pub const ARROW: u32 = code::ARROW as u32;
    /// A text box at a point.
    pub const TEXT_BOX: u32 = code::TEXT_BOX as u32;
    /// A quad through four points.
    pub const QUAD: u32 = code::QUAD as u32;
    /// A freeform through its points.
    pub const FREEFORM: u32 = code::FREEFORM as u32;
    /// A circle, through `trigger.action.circleToAll`; never a `markupToAll`
    /// shape.
    pub const CIRCLE: u32 = code::CIRCLE as u32;
}

/// The most points a polygon or freeform takes: Lua 5.1's `unpack` stops
/// near 8,000 values (LIVE, docs/results/2026-11-25-drawing-layers.md).
pub const MAX_POINTS: usize = 7_990;
/// The farthest from 0 a point, and the largest a size, may be: 10,000 km.
pub const REACH_M: f64 = 10_000_000.0;
/// The smallest size: a radius, a half-axis, a length, a width or a corner
/// radius. At 1 m, points 10 degrees apart are 17 cm apart, far above the
/// 1 mm the expansion rounds to, so no outline collapses.
pub const MIN_SIZE_M: f64 = 1.0;
/// The font sizes a text box takes. 8 to 32 are LIVE; the cap keeps near them.
pub const FONT_SIZES: std::ops::RangeInclusive<u32> = 1..=100;
/// The expansion's step, degrees.
pub const STEP_DEG: u32 = 10;

/// Outline styles, by their DCS value (LIVE, client,
/// docs/results/2026-11-25-drawing-layers.md, "Shape IDs, line types and
/// text size").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineType {
    /// No outline: DCS 0.
    None = 0,
    /// Solid.
    Solid = 1,
    /// Dashed.
    Dashed = 2,
    /// Dotted.
    Dotted = 3,
    /// Dash and dot.
    DashDot = 4,
    /// Long dash.
    LongDash = 5,
    /// Long dash and short dash.
    TwoDash = 6,
}

impl LineType {
    /// The line type of a DCS value from 0 to 6.
    pub fn from_dcs(v: u32) -> Option<LineType> {
        Some(match v {
            0 => LineType::None,
            1 => LineType::Solid,
            2 => LineType::Dashed,
            3 => LineType::Dotted,
            4 => LineType::DashDot,
            5 => LineType::LongDash,
            6 => LineType::TwoDash,
            _ => return None,
        })
    }

    /// The DCS value.
    pub fn dcs(self) -> u32 {
        self as u32
    }
}

/// A point on the map, meters north and east (DCS x and z).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapPoint {
    /// Meters north.
    pub north: f64,
    /// Meters east.
    pub east: f64,
}

impl MapPoint {
    /// A point.
    pub fn new(north: f64, east: f64) -> MapPoint {
        MapPoint { north, east }
    }
}

/// What a drawing draws, with its geometry. Heading is radians clockwise
/// from north, of the length axis.
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeKind {
    /// A line.
    Line {
        /// One end.
        from: MapPoint,
        /// The other end.
        to: MapPoint,
    },
    /// A polygon through 2 or more points; 2 draw a line.
    Polygon(Vec<MapPoint>),
    /// An arrow.
    Arrow {
        /// Its tail.
        from: MapPoint,
        /// Its head.
        to: MapPoint,
    },
    /// A text box: the text is the drawing's text, the fill its background.
    TextBox {
        /// Where it stands.
        at: MapPoint,
        /// The font size.
        font_size: u32,
    },
    /// A quad through four corners, in order round the outline.
    Quad([MapPoint; 4]),
    /// A freeform through 3 or more points.
    Freeform(Vec<MapPoint>),
    /// A circle.
    Circle {
        /// Its center.
        center: MapPoint,
        /// Its radius, meters.
        radius: f64,
    },
    /// A rectangle, sent as a quad.
    Rectangle {
        /// Its center.
        center: MapPoint,
        /// Along the heading, meters.
        length: f64,
        /// Across the heading, meters.
        width: f64,
        /// Radians clockwise from north.
        heading: f64,
    },
    /// An oval, sent as a freeform of 36 points.
    Oval {
        /// Its center.
        center: MapPoint,
        /// Half its length, along the heading, meters.
        half_length: f64,
        /// Half its width, meters.
        half_width: f64,
        /// Radians clockwise from north.
        heading: f64,
    },
    /// A rectangle with round corners, sent as a freeform of 36 to 40
    /// points.
    RoundedRectangle {
        /// Its center.
        center: MapPoint,
        /// Along the heading, meters.
        length: f64,
        /// Across the heading, meters.
        width: f64,
        /// The corners' radius, meters.
        corner_radius: f64,
        /// Radians clockwise from north.
        heading: f64,
    },
}

/// A typed drawing.
#[derive(Clone, Debug, PartialEq)]
pub struct DrawingShape {
    /// The map; empty for a drawing put through the 0.1.0 fields, which
    /// passes no area filter.
    pub theater: String,
    /// Outline color (a text box's text color), red, green, blue, alpha.
    pub color: [f64; 4],
    /// Fill color (a text box's background).
    pub fill: [f64; 4],
    /// The outline; a text box takes [`LineType::Solid`], which is not sent.
    pub line_type: LineType,
    /// A text box's text; for any other kind the panel's message.
    pub text: String,
    /// Whether players cannot change it.
    pub read_only: bool,
    /// What it draws.
    pub kind: ShapeKind,
}

/// Why a drawing is refused: the field and what it must be.
#[derive(Clone, Debug, PartialEq)]
pub struct BadField {
    /// The field, under the drawing (`circle.radius`, `color`).
    pub field: String,
    /// What it must be.
    pub want: &'static str,
}

fn bad(field: impl Into<String>, want: &'static str) -> BadField {
    BadField {
        field: field.into(),
        want,
    }
}

fn point_ok(p: &MapPoint) -> bool {
    p.north.is_finite() && p.east.is_finite() && p.north.hypot(p.east) <= REACH_M
}

fn size_ok(v: f64) -> bool {
    v.is_finite() && (MIN_SIZE_M..=REACH_M).contains(&v)
}

/// A color as DCS takes it: four finite parts, each from 0 to 1.
pub fn color_ok(c: &[f64; 4]) -> bool {
    c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

impl DrawingShape {
    /// Check every field (spec/world.md, "Drawings").
    pub fn check(&self) -> Result<(), BadField> {
        if !color_ok(&self.color) {
            return Err(bad("color", "four finite parts, each from 0 to 1"));
        }
        if !color_ok(&self.fill) {
            return Err(bad("fill", "four finite parts, each from 0 to 1"));
        }
        let points = |name: &str, pts: &[MapPoint]| {
            pts.iter().position(|p| !point_ok(p)).map_or(Ok(()), |i| {
                Err(bad(format!("{name}[{i}]"), "finite, within 10,000 km of 0"))
            })
        };
        let at = |name: &str, p: &MapPoint| {
            if point_ok(p) {
                Ok(())
            } else {
                Err(bad(name.to_owned(), "finite, within 10,000 km of 0"))
            }
        };
        let size = |name: &str, v: f64| {
            if size_ok(v) {
                Ok(())
            } else {
                Err(bad(name.to_owned(), "finite, from 1 m to 10,000 km"))
            }
        };
        let heading = |name: &str, v: f64| {
            if v.is_finite() {
                Ok(())
            } else {
                Err(bad(name.to_owned(), "finite"))
            }
        };
        match &self.kind {
            ShapeKind::Line { from, to } => {
                at("line.from", from)?;
                at("line.to", to)
            }
            ShapeKind::Arrow { from, to } => {
                at("arrow.from", from)?;
                at("arrow.to", to)
            }
            ShapeKind::Polygon(pts) => {
                if !(2..=MAX_POINTS).contains(&pts.len()) {
                    return Err(bad("polygon.points", "2 to 7,990 points"));
                }
                points("polygon.points", pts)
            }
            ShapeKind::Freeform(pts) => {
                if !(3..=MAX_POINTS).contains(&pts.len()) {
                    return Err(bad("freeform.points", "3 to 7,990 points"));
                }
                points("freeform.points", pts)
            }
            ShapeKind::Quad(pts) => points("quad.corners", pts),
            ShapeKind::TextBox { at: p, font_size } => {
                at("text_box.at", p)?;
                if !FONT_SIZES.contains(font_size) {
                    return Err(bad("text_box.font_size", "a whole number from 1 to 100"));
                }
                if self.line_type != LineType::Solid {
                    return Err(bad("line_type", "unset or solid for a text box"));
                }
                Ok(())
            }
            ShapeKind::Circle { center, radius } => {
                at("circle.center", center)?;
                size("circle.radius", *radius)
            }
            ShapeKind::Rectangle {
                center,
                length,
                width,
                heading: h,
            } => {
                at("rectangle.center", center)?;
                size("rectangle.length", *length)?;
                size("rectangle.width", *width)?;
                heading("rectangle.heading", *h)
            }
            ShapeKind::Oval {
                center,
                half_length,
                half_width,
                heading: h,
            } => {
                at("oval.center", center)?;
                size("oval.half_length", *half_length)?;
                size("oval.half_width", *half_width)?;
                heading("oval.heading", *h)
            }
            ShapeKind::RoundedRectangle {
                center,
                length,
                width,
                corner_radius,
                heading: h,
            } => {
                at("rounded_rectangle.center", center)?;
                size("rounded_rectangle.length", *length)?;
                size("rounded_rectangle.width", *width)?;
                heading("rounded_rectangle.heading", *h)?;
                if !(size_ok(*corner_radius) && *corner_radius <= length.min(*width) / 2.0) {
                    return Err(bad(
                        "rounded_rectangle.corner_radius",
                        "from 1 m to half the shorter of length and width",
                    ));
                }
                Ok(())
            }
        }
    }

    /// The points the drawing is sent as, for every kind but a text box and
    /// a circle: the given points, or the expansion of a rectangle, an oval
    /// or a rounded rectangle.
    pub fn outline(&self) -> Vec<MapPoint> {
        match &self.kind {
            ShapeKind::Line { from, to } | ShapeKind::Arrow { from, to } => vec![*from, *to],
            ShapeKind::Polygon(p) | ShapeKind::Freeform(p) => p.clone(),
            ShapeKind::Quad(p) => p.to_vec(),
            ShapeKind::TextBox { at, .. } => vec![*at],
            ShapeKind::Circle { center, .. } => vec![*center],
            ShapeKind::Rectangle {
                center,
                length,
                width,
                heading,
            } => rectangle(*center, *length, *width, *heading).to_vec(),
            ShapeKind::Oval {
                center,
                half_length,
                half_width,
                heading,
            } => oval(*center, *half_length, *half_width, *heading),
            ShapeKind::RoundedRectangle {
                center,
                length,
                width,
                corner_radius,
                heading,
            } => rounded_rectangle(*center, *length, *width, *corner_radius, *heading),
        }
    }

    /// The DCS shape code and the arguments after the coalition and the ID
    /// (spec/commands.md, "Layout: DRAWING_SET").
    pub fn lower(&self) -> (u32, Lv) {
        let color = |c: &[f64; 4]| lv::list(c.iter().map(|v| Lv::Number(*v)).collect());
        let tail = |mid: Lv| {
            [
                color(&self.color),
                color(&self.fill),
                mid,
                Lv::Bool(self.read_only),
                Lv::String(self.text.as_bytes().to_vec()),
            ]
        };
        let code = match &self.kind {
            ShapeKind::Line { .. } => shape::LINE,
            ShapeKind::Polygon(_) => shape::POLYGON,
            ShapeKind::Arrow { .. } => shape::ARROW,
            ShapeKind::TextBox { .. } => shape::TEXT_BOX,
            ShapeKind::Quad(_) | ShapeKind::Rectangle { .. } => shape::QUAD,
            ShapeKind::Freeform(_)
            | ShapeKind::Oval { .. }
            | ShapeKind::RoundedRectangle { .. } => shape::FREEFORM,
            ShapeKind::Circle { .. } => shape::CIRCLE,
        };
        let mut args: Vec<Lv> = Vec::new();
        match &self.kind {
            ShapeKind::Circle { center, radius } => {
                args.push(dcs_point(center));
                args.push(Lv::Number(*radius));
                args.extend(tail(Lv::Number(self.line_type.dcs().into())));
            }
            ShapeKind::TextBox { at, font_size } => {
                args.push(dcs_point(at));
                args.extend(tail(Lv::Number((*font_size).into())));
            }
            _ => {
                args.extend(self.outline().iter().map(dcs_point));
                args.extend(tail(Lv::Number(self.line_type.dcs().into())));
            }
        }
        (code, lv::list(args))
    }

    /// The distance in meters from a point to what the drawing covers: its
    /// segment for a line, an arrow and a two-point polygon, its point for a
    /// text box, its disc for a circle, and for every other kind its outline
    /// and inside (even-odd). 0 when the point is on it or inside.
    pub fn distance_to(&self, p: MapPoint) -> f64 {
        match &self.kind {
            ShapeKind::TextBox { at, .. } => dist(*at, p),
            ShapeKind::Circle { center, radius } => (dist(*center, p) - radius).max(0.0),
            ShapeKind::Line { from, to } | ShapeKind::Arrow { from, to } => {
                segment_distance(*from, *to, p)
            }
            ShapeKind::Polygon(pts) if pts.len() == 2 => segment_distance(pts[0], pts[1], p),
            _ => {
                let pts = self.outline();
                if inside(&pts, p) {
                    return 0.0;
                }
                (0..pts.len())
                    .map(|i| segment_distance(pts[i], pts[(i + 1) % pts.len()], p))
                    .fold(f64::INFINITY, f64::min)
            }
        }
    }

    /// The drawing a 0.1.0 put of `shape` and `args` describes, or `None`
    /// when it is not a layout the platform sends: [`layout_ok`] refuses it,
    /// or shape 5 has a font size that is not a whole number from 1 to 100.
    /// `y` of each point is not kept: DCS fills it with the ground height.
    pub fn from_legacy(code: u32, args: &Lv) -> Option<DrawingShape> {
        if code == shape::CIRCLE || !layout_ok(code, args) {
            return None;
        }
        let items = lv::items(args);
        let n = items.len();
        let pts: Vec<MapPoint> = items[..n - 5].iter().map(|v| map_point(v)).collect();
        let color = four(items[n - 5])?;
        let fill = four(items[n - 4])?;
        let mid = num(items[n - 3])?;
        let read_only = matches!(items[n - 2], Lv::Bool(true));
        let text = match items[n - 1] {
            Lv::String(b) => String::from_utf8(b.clone()).ok()?,
            _ => return None,
        };
        let line = || LineType::from_dcs(mid as u32);
        let (kind, line_type) = match code {
            shape::LINE => (
                ShapeKind::Line {
                    from: pts[0],
                    to: pts[1],
                },
                line()?,
            ),
            shape::ARROW => (
                ShapeKind::Arrow {
                    from: pts[0],
                    to: pts[1],
                },
                line()?,
            ),
            shape::POLYGON => (ShapeKind::Polygon(pts), line()?),
            shape::FREEFORM => (ShapeKind::Freeform(pts), line()?),
            shape::QUAD => (ShapeKind::Quad([pts[0], pts[1], pts[2], pts[3]]), line()?),
            shape::TEXT_BOX => (
                ShapeKind::TextBox {
                    at: pts[0],
                    font_size: mid as u32,
                },
                LineType::Solid,
            ),
            _ => return None,
        };
        let d = DrawingShape {
            theater: String::new(),
            color,
            fill,
            line_type,
            text,
            read_only,
            kind,
        };
        d.check().ok().map(|()| d)
    }
}

impl DrawingShape {
    /// The store's form (spec/persistence.md, "Tables"): a table of
    /// `theater`, `color`, `fill`, `line_type` (the DCS value), `text`,
    /// `read_only`, `kind`, `points` (a list of north, east pairs) and the
    /// kind's sizes by their names.
    pub fn to_lv(&self) -> Lv {
        let color = |c: &[f64; 4]| lv::list(c.iter().map(|v| Lv::Number(*v)).collect());
        let pts = |p: &[MapPoint]| {
            lv::list(
                p.iter()
                    .map(|p| lv::list(vec![Lv::Number(p.north), Lv::Number(p.east)]))
                    .collect(),
            )
        };
        let mut pairs: Vec<(&str, Lv)> = vec![
            ("theater", lv::s(&self.theater)),
            ("color", color(&self.color)),
            ("fill", color(&self.fill)),
            ("line_type", Lv::Number(self.line_type.dcs().into())),
            ("text", lv::s(&self.text)),
            ("read_only", Lv::Bool(self.read_only)),
        ];
        let n = Lv::Number;
        let (kind, points, sizes): (&str, Vec<MapPoint>, Vec<(&str, f64)>) = match &self.kind {
            ShapeKind::Line { from, to } => ("line", vec![*from, *to], vec![]),
            ShapeKind::Arrow { from, to } => ("arrow", vec![*from, *to], vec![]),
            ShapeKind::Polygon(p) => ("polygon", p.clone(), vec![]),
            ShapeKind::Freeform(p) => ("freeform", p.clone(), vec![]),
            ShapeKind::Quad(p) => ("quad", p.to_vec(), vec![]),
            ShapeKind::TextBox { at, font_size } => (
                "text_box",
                vec![*at],
                vec![("font_size", f64::from(*font_size))],
            ),
            ShapeKind::Circle { center, radius } => {
                ("circle", vec![*center], vec![("radius", *radius)])
            }
            ShapeKind::Rectangle {
                center,
                length,
                width,
                heading,
            } => (
                "rectangle",
                vec![*center],
                vec![
                    ("length", *length),
                    ("width", *width),
                    ("heading", *heading),
                ],
            ),
            ShapeKind::Oval {
                center,
                half_length,
                half_width,
                heading,
            } => (
                "oval",
                vec![*center],
                vec![
                    ("half_length", *half_length),
                    ("half_width", *half_width),
                    ("heading", *heading),
                ],
            ),
            ShapeKind::RoundedRectangle {
                center,
                length,
                width,
                corner_radius,
                heading,
            } => (
                "rounded_rectangle",
                vec![*center],
                vec![
                    ("length", *length),
                    ("width", *width),
                    ("corner_radius", *corner_radius),
                    ("heading", *heading),
                ],
            ),
        };
        pairs.push(("kind", lv::s(kind)));
        pairs.push(("points", pts(&points)));
        pairs.extend(sizes.into_iter().map(|(k, v)| (k, n(v))));
        lv::obj(pairs)
    }

    /// The drawing [`DrawingShape::to_lv`] made, or `None` when `v` is not
    /// one or fails [`DrawingShape::check`].
    pub fn from_lv(v: &Lv) -> Option<DrawingShape> {
        let f = |k: &str| lv::num_at(v, k);
        let pts: Vec<MapPoint> = lv::items(lv::get(v, "points")?)
            .into_iter()
            .map(|p| {
                let i = lv::items(p);
                match (i.first(), i.get(1), i.len()) {
                    (Some(Lv::Number(a)), Some(Lv::Number(b)), 2) => Some(MapPoint::new(*a, *b)),
                    _ => None,
                }
            })
            .collect::<Option<_>>()?;
        let one = || (pts.len() == 1).then(|| pts[0]);
        let two = || (pts.len() == 2).then(|| (pts[0], pts[1]));
        let kind = match lv::str_at(v, "kind")?.as_str() {
            "line" => two().map(|(from, to)| ShapeKind::Line { from, to })?,
            "arrow" => two().map(|(from, to)| ShapeKind::Arrow { from, to })?,
            "polygon" => ShapeKind::Polygon(pts.clone()),
            "freeform" => ShapeKind::Freeform(pts.clone()),
            "quad" => ShapeKind::Quad(<[MapPoint; 4]>::try_from(pts.clone()).ok()?),
            "text_box" => {
                let size = f("font_size")?;
                if size.fract() != 0.0 || !(0.0..=f64::from(u32::MAX)).contains(&size) {
                    return None;
                }
                ShapeKind::TextBox {
                    at: one()?,
                    font_size: size as u32,
                }
            }
            "circle" => ShapeKind::Circle {
                center: one()?,
                radius: f("radius")?,
            },
            "rectangle" => ShapeKind::Rectangle {
                center: one()?,
                length: f("length")?,
                width: f("width")?,
                heading: f("heading")?,
            },
            "oval" => ShapeKind::Oval {
                center: one()?,
                half_length: f("half_length")?,
                half_width: f("half_width")?,
                heading: f("heading")?,
            },
            "rounded_rectangle" => ShapeKind::RoundedRectangle {
                center: one()?,
                length: f("length")?,
                width: f("width")?,
                corner_radius: f("corner_radius")?,
                heading: f("heading")?,
            },
            _ => return None,
        };
        let line = f("line_type")?;
        let d = DrawingShape {
            theater: lv::str_at(v, "theater")?,
            color: four(lv::get(v, "color")?)?,
            fill: four(lv::get(v, "fill")?)?,
            line_type: if line.fract() == 0.0 && (0.0..=6.0).contains(&line) {
                LineType::from_dcs(line as u32)?
            } else {
                return None;
            },
            text: lv::str_at(v, "text")?,
            read_only: matches!(lv::get(v, "read_only")?, Lv::Bool(true)),
            kind,
        };
        d.check().ok().map(|()| d)
    }
}

fn dist(a: MapPoint, b: MapPoint) -> f64 {
    (a.north - b.north).hypot(a.east - b.east)
}

fn segment_distance(a: MapPoint, b: MapPoint, p: MapPoint) -> f64 {
    let (dn, de) = (b.north - a.north, b.east - a.east);
    let len2 = dn * dn + de * de;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.north - a.north) * dn + (p.east - a.east) * de) / len2).clamp(0.0, 1.0)
    };
    dist(MapPoint::new(a.north + t * dn, a.east + t * de), p)
}

/// Even-odd: whether `p` is inside the closed outline through `pts`.
fn inside(pts: &[MapPoint], p: MapPoint) -> bool {
    let mut odd = false;
    let mut j = pts.len() - 1;
    for i in 0..pts.len() {
        let (a, b) = (pts[i], pts[j]);
        if (a.east > p.east) != (b.east > p.east)
            && p.north < (b.north - a.north) * (p.east - a.east) / (b.east - a.east) + a.north
        {
            odd = !odd;
        }
        j = i;
    }
    odd
}

fn dcs_point(p: &MapPoint) -> Lv {
    lv::obj(vec![
        ("x", Lv::Number(p.north)),
        ("y", Lv::Number(0.0)),
        ("z", Lv::Number(p.east)),
    ])
}

fn map_point(v: &Lv) -> MapPoint {
    MapPoint::new(
        lv::num_at(v, "x").unwrap_or(f64::NAN),
        lv::num_at(v, "z").unwrap_or(f64::NAN),
    )
}

fn num(v: &Lv) -> Option<f64> {
    match v {
        Lv::Number(x) => Some(*x),
        _ => None,
    }
}

fn four(v: &Lv) -> Option<[f64; 4]> {
    let items = lv::items(v);
    if items.len() != 4 {
        return None;
    }
    Some([
        num(items[0])?,
        num(items[1])?,
        num(items[2])?,
        num(items[3])?,
    ])
}

/// Rounded to 1 mm, so an expansion is the same on every host.
fn mm(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// The point `a` meters along the heading and `b` across it (to the right of
/// the heading) from `c`, rounded to 1 mm.
fn at_local(c: MapPoint, heading: f64, a: f64, b: f64) -> MapPoint {
    let (s, co) = heading.sin_cos();
    MapPoint::new(mm(c.north + a * co - b * s), mm(c.east + a * s + b * co))
}

/// The four corners of a rectangle, in order round the outline.
pub fn rectangle(c: MapPoint, length: f64, width: f64, heading: f64) -> [MapPoint; 4] {
    let (l, w) = (length / 2.0, width / 2.0);
    [
        at_local(c, heading, l, w),
        at_local(c, heading, -l, w),
        at_local(c, heading, -l, -w),
        at_local(c, heading, l, -w),
    ]
}

/// An oval's 36 points, from the end of the length axis, every 10 degrees.
pub fn oval(c: MapPoint, half_length: f64, half_width: f64, heading: f64) -> Vec<MapPoint> {
    (0..360 / STEP_DEG)
        .map(|k| {
            let t = f64::from(k * STEP_DEG).to_radians();
            at_local(c, heading, half_length * t.cos(), half_width * t.sin())
        })
        .collect()
}

/// A rounded rectangle's points: four 90-degree arcs in 10-degree steps, 10
/// points each, in the oval's direction. A point equal to the one before it
/// (where the corner radius is half a side, two arcs share an end point), or
/// a last point equal to the first, is sent once: 40, 38 or 36 points.
pub fn rounded_rectangle(
    c: MapPoint,
    length: f64,
    width: f64,
    r: f64,
    heading: f64,
) -> Vec<MapPoint> {
    // A straight side under 1 mm, the rounding of the points, is none: its two
    // arcs then share their end points exactly, so the count stays 36, 38 or 40.
    let side = |half: f64| if half < 0.001 { 0.0 } else { half };
    let (cl, cw) = (side(length / 2.0 - r), side(width / 2.0 - r));
    let centers = [(cl, cw), (-cl, cw), (-cl, -cw), (cl, -cw)];
    let per_arc = 90 / STEP_DEG;
    let mut pts: Vec<MapPoint> = Vec::with_capacity(4 * (per_arc as usize + 1));
    for (q, (ca, cb)) in centers.iter().enumerate() {
        for k in 0..=per_arc {
            let t = f64::from(90 * q as u32 + k * STEP_DEG).to_radians();
            let p = at_local(c, heading, ca + r * t.cos(), cb + r * t.sin());
            if pts.last() != Some(&p) {
                pts.push(p);
            }
        }
    }
    if pts.len() > 1 && pts.last() == pts.first() {
        pts.pop();
    }
    pts
}

fn finite(v: &Lv) -> Option<f64> {
    num(v).filter(|x| x.is_finite())
}

fn whole_in(v: &Lv, lo: f64, hi: f64) -> bool {
    finite(v).is_some_and(|x| x.fract() == 0.0 && (lo..=hi).contains(&x))
}

fn dcs_color_ok(v: &Lv) -> bool {
    list_len(v) == Some(4) && four(v).is_some_and(|c| color_ok(&c))
}

fn dcs_point_ok(v: &Lv) -> bool {
    ["x", "y", "z"]
        .iter()
        .all(|k| matches!(lv::get(v, k), Some(Lv::Number(n)) if n.is_finite()))
}

/// The length of a list whose keys are exactly 1 to n, or `None`.
pub fn list_len(v: &Lv) -> Option<usize> {
    let Lv::Table(pairs) = v else {
        return None;
    };
    let mut keys = Vec::with_capacity(pairs.len());
    for (k, _) in pairs {
        match k {
            Lv::Number(i) => keys.push(*i),
            _ => return None,
        }
    }
    keys.sort_by(f64::total_cmp);
    keys.iter()
        .enumerate()
        .all(|(i, k)| *k == (i + 1) as f64)
        .then_some(keys.len())
}

/// Whether a shape code and its arguments are a layout the platform sends
/// (spec/commands.md, "Layout: DRAWING_SET"), checked exactly as the shim
/// checks it: `markupToAll` shape 1 or 4 with two points, 3 with 2 to 7,990,
/// 5 with one, 6 with four, 7 with 3 to 7,990, each a table of finite `x`,
/// `y` and `z`, then color and fill (four parts each from 0 to 1), the line
/// type (a whole number 0 to 6; for shape 5 the font size, a whole number 1
/// to 100), read only and the message; or the circle, code `0x100`, with its
/// center, a radius from 1 m to 10,000 km, then the same tail. `args` is a
/// list with keys 1 to n and nothing else. DCS can crash inside `markupToAll`
/// on any other shape (LIVE, docs/results/2026-11-25-drawing-layers.md).
pub fn layout_ok(code: u32, args: &Lv) -> bool {
    let Some(n) = list_len(args) else {
        return false;
    };
    let items = lv::items(args);
    let lead = match code {
        shape::CIRCLE => 2,
        _ => match (code, n.checked_sub(5)) {
            (shape::LINE | shape::ARROW, Some(2)) => 2,
            (shape::POLYGON, Some(p)) if (2..=MAX_POINTS).contains(&p) => p,
            (shape::TEXT_BOX, Some(1)) => 1,
            (shape::QUAD, Some(4)) => 4,
            (shape::FREEFORM, Some(p)) if (3..=MAX_POINTS).contains(&p) => p,
            _ => return false,
        },
    };
    if n != lead + 5 {
        return false;
    }
    let lead_ok = if code == shape::CIRCLE {
        dcs_point_ok(items[0]) && finite(items[1]).is_some_and(size_ok)
    } else {
        items[..lead].iter().all(|p| dcs_point_ok(p))
    };
    let mid_ok = if code == shape::TEXT_BOX {
        whole_in(items[n - 3], 1.0, 100.0)
    } else {
        whole_in(items[n - 3], 0.0, 6.0)
    };
    lead_ok
        && dcs_color_ok(items[n - 5])
        && dcs_color_ok(items[n - 4])
        && mid_ok
        && matches!(items[n - 2], Lv::Bool(_))
        && matches!(items[n - 1], Lv::String(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn styled(kind: ShapeKind) -> DrawingShape {
        DrawingShape {
            theater: "Caucasus".into(),
            color: [1.0, 0.0, 0.0, 1.0],
            fill: [1.0, 0.0, 0.0, 0.5],
            line_type: LineType::Dashed,
            text: "t".into(),
            read_only: true,
            kind,
        }
    }

    fn c() -> MapPoint {
        MapPoint::new(1000.0, 2000.0)
    }

    #[test]
    fn point_counts_follow_the_ten_degree_step() {
        assert_eq!(oval(c(), 1500.0, 750.0, 0.5).len(), 36);
        assert_eq!(rounded_rectangle(c(), 3000.0, 1500.0, 400.0, 0.5).len(), 40);
        // Half the width: the arcs share two end points.
        assert_eq!(rounded_rectangle(c(), 3000.0, 1500.0, 750.0, 0.5).len(), 38);
        // Half of both sides: a circle.
        assert_eq!(rounded_rectangle(c(), 1500.0, 1500.0, 750.0, 0.0).len(), 36);
        // Under 1 mm from half the width, turned: still 38, never 37 or 39.
        for h in [0.1, 0.5, 1.0, 2.0, 3.0] {
            assert_eq!(
                rounded_rectangle(c(), 3000.0, 1500.0, 749.9996, h).len(),
                38
            );
            assert_eq!(
                rounded_rectangle(c(), 1500.0, 1500.0, 749.9996, h).len(),
                36
            );
        }
    }

    #[test]
    fn a_rectangle_at_heading_zero_runs_north_by_its_length() {
        let r = rectangle(c(), 3000.0, 1000.0, 0.0);
        assert_eq!(r[0], MapPoint::new(2500.0, 2500.0));
        assert_eq!(r[2], MapPoint::new(-500.0, 1500.0));
        // A quarter turn lays the length east.
        let q = rectangle(c(), 3000.0, 1000.0, std::f64::consts::FRAC_PI_2);
        assert!((q[0].east - 3500.0).abs() < 1e-3 && (q[0].north - 500.0).abs() < 1e-3);
    }

    #[test]
    fn checks_refuse_each_bad_field() {
        let field = |d: DrawingShape| d.check().unwrap_err().field;
        let mut d = styled(ShapeKind::Circle {
            center: c(),
            radius: 0.5,
        });
        assert_eq!(field(d.clone()), "circle.radius");
        d.kind = ShapeKind::Circle {
            center: c(),
            radius: 10.0,
        };
        assert!(d.check().is_ok());
        d.color[0] = 1.5;
        assert_eq!(field(d.clone()), "color");
        let rr = |r: f64| {
            styled(ShapeKind::RoundedRectangle {
                center: c(),
                length: 3000.0,
                width: 1500.0,
                corner_radius: r,
                heading: 0.0,
            })
        };
        assert!(rr(750.0).check().is_ok());
        assert_eq!(field(rr(750.1)), "rounded_rectangle.corner_radius");
        assert_eq!(field(rr(0.0)), "rounded_rectangle.corner_radius");
        let mut t = styled(ShapeKind::TextBox {
            at: c(),
            font_size: 101,
        });
        assert_eq!(field(t.clone()), "text_box.font_size");
        t.kind = ShapeKind::TextBox {
            at: c(),
            font_size: 14,
        };
        assert_eq!(field(t.clone()), "line_type");
        t.line_type = LineType::Solid;
        assert!(t.check().is_ok());
        assert_eq!(
            field(styled(ShapeKind::Freeform(vec![c(), c()]))),
            "freeform.points"
        );
        assert_eq!(
            field(styled(ShapeKind::Polygon(vec![
                c(),
                MapPoint::new(f64::NAN, 0.0)
            ]))),
            "polygon.points[1]"
        );
        assert_eq!(
            field(styled(ShapeKind::Line {
                from: c(),
                to: MapPoint::new(2.0e7, 0.0)
            })),
            "line.to"
        );
        // The reach is the distance from 0, not each axis.
        assert_eq!(
            field(styled(ShapeKind::Line {
                from: c(),
                to: MapPoint::new(8.0e6, 8.0e6)
            })),
            "line.to"
        );
        assert!(
            styled(ShapeKind::Line {
                from: c(),
                to: MapPoint::new(7.0e6, 7.0e6)
            })
            .check()
            .is_ok()
        );
    }

    #[test]
    fn the_layout_check_is_exact() {
        let d = styled(ShapeKind::Circle {
            center: c(),
            radius: 10.0,
        });
        let (code, args) = d.lower();
        assert_eq!(code, shape::CIRCLE);
        assert!(layout_ok(code, &args));
        // The circle code is never a markupToAll shape, and a circle layout
        // under a markupToAll shape is refused.
        assert!(!layout_ok(shape::QUAD, &args));
        let mut items: Vec<Lv> = lv::items(&args).into_iter().cloned().collect();
        items[4] = Lv::Number(7.0);
        assert!(!layout_ok(code, &lv::list(items.clone())), "line type 7");
        items[4] = Lv::Number(2.5);
        assert!(!layout_ok(code, &lv::list(items.clone())), "a fraction");
        items[4] = Lv::Number(2.0);
        items[1] = Lv::Number(0.0);
        assert!(!layout_ok(code, &lv::list(items)), "radius 0");
        assert!(!layout_ok(8, &lv::list(vec![])));
    }

    /// One drawing of each kind, as the golden file lists them.
    fn golden_kinds() -> Vec<(&'static str, DrawingShape)> {
        let p = MapPoint::new;
        let h = 30f64.to_radians();
        let mut text = styled(ShapeKind::TextBox {
            at: p(0.0, 0.0),
            font_size: 14,
        });
        text.line_type = LineType::Solid;
        vec![
            (
                "line",
                styled(ShapeKind::Line {
                    from: p(0.0, 0.0),
                    to: p(100.0, 50.0),
                }),
            ),
            (
                "polygon",
                styled(ShapeKind::Polygon(vec![
                    p(0.0, 0.0),
                    p(100.0, 0.0),
                    p(50.0, 80.0),
                ])),
            ),
            (
                "arrow",
                styled(ShapeKind::Arrow {
                    from: p(0.0, 0.0),
                    to: p(100.0, 50.0),
                }),
            ),
            ("text_box", text),
            (
                "quad",
                styled(ShapeKind::Quad([
                    p(0.0, 0.0),
                    p(100.0, 20.0),
                    p(90.0, 120.0),
                    p(-10.0, 80.0),
                ])),
            ),
            (
                "freeform",
                styled(ShapeKind::Freeform(vec![
                    p(0.0, 0.0),
                    p(100.0, 0.0),
                    p(50.0, 80.0),
                ])),
            ),
            (
                "circle",
                styled(ShapeKind::Circle {
                    center: p(0.0, 0.0),
                    radius: 1500.0,
                }),
            ),
            (
                "rectangle",
                styled(ShapeKind::Rectangle {
                    center: p(0.0, 0.0),
                    length: 3000.0,
                    width: 1500.0,
                    heading: h,
                }),
            ),
            (
                "oval",
                styled(ShapeKind::Oval {
                    center: p(0.0, 0.0),
                    half_length: 1500.0,
                    half_width: 750.0,
                    heading: h,
                }),
            ),
            (
                "rounded_rectangle",
                styled(ShapeKind::RoundedRectangle {
                    center: p(0.0, 0.0),
                    length: 3000.0,
                    width: 1500.0,
                    corner_radius: 400.0,
                    heading: h,
                }),
            ),
            (
                "capsule",
                styled(ShapeKind::RoundedRectangle {
                    center: p(0.0, 0.0),
                    length: 3000.0,
                    width: 1500.0,
                    corner_radius: 750.0,
                    heading: h,
                }),
            ),
        ]
    }

    /// A lowering as one golden line: the name, the code, then the points
    /// (north,east) and any radius or size, in meters to 1 mm.
    fn golden_line(name: &str, d: &DrawingShape) -> String {
        let (code, args) = d.lower();
        let mut parts = vec![name.to_owned(), code.to_string()];
        for v in lv::items(&args) {
            match v {
                Lv::Table(_) if lv::get(v, "x").is_some() => parts.push(format!(
                    "{:.3},{:.3}",
                    lv::num_at(v, "x").unwrap(),
                    lv::num_at(v, "z").unwrap()
                )),
                // A color or fill: its four parts.
                Lv::Table(_) => parts.push(
                    lv::items(v)
                        .iter()
                        .map(|c| match c {
                            Lv::Number(n) => format!("{n:.3}"),
                            _ => "?".into(),
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                Lv::Number(n) => parts.push(format!("{n:.3}")),
                Lv::Bool(b) => parts.push(b.to_string()),
                Lv::String(s) => parts.push(format!("text={}", String::from_utf8_lossy(s))),
                _ => parts.push("?".into()),
            }
        }
        parts.join(" ")
    }

    /// The golden file of the lowerings (spec/world.md, "Drawings"). Set
    /// `NANOPROBE_BLESS=1` to write it after an intended change. Points are
    /// compared to 1 mm, the rounding of the expansion, so a last-bit
    /// difference in a host's sine or cosine does not fail it.
    #[test]
    fn lowerings_match_the_golden_file() {
        const GOLDEN: &str = include_str!("../../../../spec/drawing.golden");
        let lines: Vec<String> = golden_kinds()
            .iter()
            .map(|(name, d)| golden_line(name, d))
            .collect();
        if std::env::var_os("NANOPROBE_BLESS").is_some() {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/drawing.golden");
            std::fs::write(path, lines.join("\n") + "\n").expect("write the golden file");
            return;
        }
        let want: Vec<&str> = GOLDEN.lines().collect();
        assert_eq!(
            want.len(),
            lines.len(),
            "the kinds changed; see spec/world.md"
        );
        for (got, want) in lines.iter().zip(want) {
            let (g, w): (Vec<&str>, Vec<&str>) =
                (got.split(' ').collect(), want.split(' ').collect());
            assert_eq!(g.len(), w.len(), "{got}\n{want}");
            for (a, b) in g.iter().zip(&w) {
                let num = |s: &str| -> Option<Vec<f64>> {
                    s.split(',').map(|x| x.parse().ok()).collect()
                };
                match (num(a), num(b)) {
                    (Some(x), Some(y)) if x.len() == y.len() => {
                        for (x, y) in x.iter().zip(&y) {
                            assert!((x - y).abs() <= 0.0011, "{got}\n{want}");
                        }
                    }
                    _ => assert_eq!(a, b, "{got}\n{want}"),
                }
            }
        }
    }

    #[test]
    fn every_kind_lowers_to_a_layout_the_sidecar_check_takes_and_round_trips() {
        for (name, d) in golden_kinds() {
            let (code, args) = d.lower();
            assert!(layout_ok(code, &args), "{name}");
            assert_eq!(
                DrawingShape::from_lv(&d.to_lv()).as_ref(),
                Some(&d),
                "{name}"
            );
        }
    }

    /// Whether segments a-b and c-d cross at a point inside both.
    fn cross(a: MapPoint, b: MapPoint, c: MapPoint, d: MapPoint) -> bool {
        let o = |p: MapPoint, q: MapPoint, r: MapPoint| {
            (q.north - p.north) * (r.east - p.east) - (q.east - p.east) * (r.north - p.north)
        };
        let (d1, d2, d3, d4) = (o(c, d, a), o(c, d, b), o(a, b, c), o(a, b, d));
        d1 * d2 < 0.0 && d3 * d4 < 0.0
    }

    fn crosses_itself(p: &[MapPoint]) -> bool {
        let n = p.len();
        (0..n).any(|i| {
            (i + 2..n).any(|j| {
                (i, (j + 1) % n) != (0, 0)
                    && !(i == 0 && j == n - 1)
                    && cross(p[i], p[(i + 1) % n], p[j], p[(j + 1) % n])
            })
        })
    }

    proptest::proptest! {
        /// An oval or a rounded rectangle never crosses itself (a crossing
        /// freeform draws no fill, LIVE), has the counted points with none
        /// twice, and lies within its bounds; its lowering passes
        /// [`layout_ok`]. lua/tests/dispatch.lua sends lowerings of the same
        /// sizes through the shim.
        #[test]
        fn expansions_are_simple_and_bounded(
            n in -1.0e6f64..1.0e6,
            e in -1.0e6f64..1.0e6,
            a in 1.0f64..1.0e5,
            b in 1.0f64..1.0e5,
            r_frac in 0.0f64..=1.0,
            h in -10.0f64..10.0,
        ) {
            let c = MapPoint::new(n, e);
            let ov = oval(c, a, b, h);
            proptest::prop_assert_eq!(ov.len(), 36);
            proptest::prop_assert!(!crosses_itself(&ov));
            let reach = a.max(b) + 0.002;
            proptest::prop_assert!(ov.iter().all(|p| dist(*p, c) <= reach));
            let r = (MIN_SIZE_M + r_frac * (a.min(b) / 2.0 - MIN_SIZE_M)).max(MIN_SIZE_M);
            if r <= a.min(b) / 2.0 {
                let rr = rounded_rectangle(c, a, b, r, h);
                proptest::prop_assert!((36..=40).contains(&rr.len()));
                proptest::prop_assert!(!crosses_itself(&rr));
                for w in rr.windows(2) {
                    proptest::prop_assert!(w[0] != w[1]);
                }
                proptest::prop_assert!(rr.first() != rr.last());
                let half_diag = (a * a + b * b).sqrt() / 2.0 + 0.002;
                proptest::prop_assert!(rr.iter().all(|p| dist(*p, c) <= half_diag));
                let d = styled(ShapeKind::RoundedRectangle {
                    center: c,
                    length: a,
                    width: b,
                    corner_radius: r,
                    heading: h,
                });
                let (code, args) = d.lower();
                proptest::prop_assert!(layout_ok(code, &args));
            }
        }

        /// A 0.1.0 layout parses and lowers back to the same arguments, with
        /// `y` 0; the parse fails exactly where the exact check fails.
        #[test]
        fn a_legacy_layout_round_trips(
            code in proptest::sample::select(vec![1u32, 3, 4, 5, 6, 7]),
            pts in proptest::collection::vec((-1.0e6f64..1.0e6, -1.0e6f64..1.0e6), 1..10),
            color in proptest::array::uniform4(0.0f64..=1.0),
            mid in 0u32..8,
            read_only: bool,
        ) {
            let points: Vec<Lv> = pts
                .iter()
                .map(|(n, e)| lv::obj(vec![("x", Lv::Number(*n)), ("y", Lv::Number(0.0)), ("z", Lv::Number(*e))]))
                .collect();
            let c = lv::list(color.iter().map(|v| Lv::Number(*v)).collect());
            let mut items = points;
            items.extend([c.clone(), c, Lv::Number(mid.into()), Lv::Bool(read_only), lv::s("m")]);
            let args = lv::list(items);
            match DrawingShape::from_legacy(code, &args) {
                Some(d) => {
                    proptest::prop_assert!(layout_ok(code, &args));
                    proptest::prop_assert_eq!(d.lower(), (code, args));
                }
                None => proptest::prop_assert!(!layout_ok(code, &args)),
            }
        }
    }

    #[test]
    fn distance_covers_outline_and_inside() {
        let sq = styled(ShapeKind::Rectangle {
            center: MapPoint::new(0.0, 0.0),
            length: 2000.0,
            width: 2000.0,
            heading: 0.0,
        });
        assert_eq!(sq.distance_to(MapPoint::new(0.0, 0.0)), 0.0);
        assert!((sq.distance_to(MapPoint::new(0.0, 3000.0)) - 2000.0).abs() < 1e-6);
        let line = styled(ShapeKind::Line {
            from: MapPoint::new(0.0, 0.0),
            to: MapPoint::new(0.0, 1000.0),
        });
        assert!((line.distance_to(MapPoint::new(100.0, 500.0)) - 100.0).abs() < 1e-9);
        let circle = styled(ShapeKind::Circle {
            center: MapPoint::new(0.0, 0.0),
            radius: 500.0,
        });
        assert_eq!(circle.distance_to(MapPoint::new(100.0, 0.0)), 0.0);
        assert!((circle.distance_to(MapPoint::new(1000.0, 0.0)) - 500.0).abs() < 1e-9);
    }
}
