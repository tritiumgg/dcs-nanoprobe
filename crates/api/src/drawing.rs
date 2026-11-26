//! Typed drawings between the API and the store (spec/world.md, "Drawings").

use nanoprobe_proto::v1alpha1::{self as pb, drawing_shape};
use nanoprobe_sidecar::store::drawing::{DrawingShape, LineType, MapPoint, ShapeKind};
use tonic::Status;

use crate::convert::invalid;
use crate::lua::to_lua;

/// The color an absent `color` reads as: opaque black.
const BLACK: [f64; 4] = [0.0, 0.0, 0.0, 1.0];
/// The color an absent `fill` reads as: no fill.
const CLEAR: [f64; 4] = [0.0, 0.0, 0.0, 0.0];

fn color(c: Option<&pb::Color>, default: [f64; 4]) -> [f64; 4] {
    c.map_or(default, |c| [c.red, c.green, c.blue, c.alpha])
}

fn line_type(v: i32, field: &str) -> Result<LineType, Status> {
    Ok(match pb::LineType::try_from(v) {
        Ok(pb::LineType::Unspecified | pb::LineType::Solid) => LineType::Solid,
        Ok(pb::LineType::Dashed) => LineType::Dashed,
        Ok(pb::LineType::Dotted) => LineType::Dotted,
        Ok(pb::LineType::DashDot) => LineType::DashDot,
        Ok(pb::LineType::LongDash) => LineType::LongDash,
        Ok(pb::LineType::TwoDash) => LineType::TwoDash,
        Ok(pb::LineType::None) => LineType::None,
        Err(_) => return Err(invalid(field, "a LineType value")),
    })
}

fn pb_line_type(l: LineType) -> pb::LineType {
    match l {
        LineType::None => pb::LineType::None,
        LineType::Solid => pb::LineType::Solid,
        LineType::Dashed => pb::LineType::Dashed,
        LineType::Dotted => pb::LineType::Dotted,
        LineType::DashDot => pb::LineType::DashDot,
        LineType::LongDash => pb::LineType::LongDash,
        LineType::TwoDash => pb::LineType::TwoDash,
    }
}

fn point(p: Option<&pb::MapPoint>, field: &str) -> Result<MapPoint, Status> {
    p.map(|p| MapPoint::new(p.north, p.east))
        .ok_or_else(|| invalid(field, "a point is required"))
}

fn points(p: &[pb::MapPoint]) -> Vec<MapPoint> {
    p.iter().map(|p| MapPoint::new(p.north, p.east)).collect()
}

fn pb_point(p: MapPoint) -> pb::MapPoint {
    pb::MapPoint {
        north: p.north,
        east: p.east,
    }
}

/// A put's typed shape, checked, under `field`. `theaters` are the
/// campaign's: the theater must be one of them.
pub fn from_pb(
    d: &pb::DrawingShape,
    field: &str,
    theaters: &[String],
) -> Result<DrawingShape, Status> {
    let f = |sub: &str| format!("{field}.{sub}");
    if d.theater.is_empty() {
        return Err(invalid(&f("theater"), "a drawing names its theater"));
    }
    if !theaters.contains(&d.theater) {
        return Err(invalid(
            &f("theater"),
            format!("the campaign does not use the theater `{}`", d.theater),
        ));
    }
    use drawing_shape::Kind as K;
    let kind = match d
        .kind
        .as_ref()
        .ok_or_else(|| invalid(&f("kind"), "a kind is required"))?
    {
        K::Line(l) => ShapeKind::Line {
            from: point(l.from.as_ref(), &f("line.from"))?,
            to: point(l.to.as_ref(), &f("line.to"))?,
        },
        K::Arrow(a) => ShapeKind::Arrow {
            from: point(a.from.as_ref(), &f("arrow.from"))?,
            to: point(a.to.as_ref(), &f("arrow.to"))?,
        },
        K::Polygon(p) => ShapeKind::Polygon(points(&p.points)),
        K::Freeform(p) => ShapeKind::Freeform(points(&p.points)),
        K::Quad(q) => ShapeKind::Quad(
            <[MapPoint; 4]>::try_from(points(&q.corners))
                .map_err(|_| invalid(&f("quad.corners"), "exactly four corners"))?,
        ),
        K::TextBox(t) => ShapeKind::TextBox {
            at: point(t.at.as_ref(), &f("text_box.at"))?,
            font_size: t.font_size,
        },
        K::Circle(c) => ShapeKind::Circle {
            center: point(c.center.as_ref(), &f("circle.center"))?,
            radius: c.radius,
        },
        K::Rectangle(r) => ShapeKind::Rectangle {
            center: point(r.center.as_ref(), &f("rectangle.center"))?,
            length: r.length,
            width: r.width,
            heading: r.heading,
        },
        K::Oval(o) => ShapeKind::Oval {
            center: point(o.center.as_ref(), &f("oval.center"))?,
            half_length: o.half_length,
            half_width: o.half_width,
            heading: o.heading,
        },
        K::RoundedRectangle(r) => ShapeKind::RoundedRectangle {
            center: point(r.center.as_ref(), &f("rounded_rectangle.center"))?,
            length: r.length,
            width: r.width,
            corner_radius: r.corner_radius,
            heading: r.heading,
        },
    };
    let shape = DrawingShape {
        theater: d.theater.clone(),
        color: color(d.color.as_ref(), BLACK),
        fill: color(d.fill.as_ref(), CLEAR),
        line_type: line_type(d.line_type, &f("line_type"))?,
        text: d.text.clone(),
        read_only: d.read_only,
        kind,
    };
    shape.check().map_err(|e| invalid(&f(&e.field), e.want))?;
    Ok(shape)
}

/// The drawing a put through the deprecated `shape` and `args` describes,
/// with an empty theater; `INVALID_ARGUMENT` on `field` when it is not a
/// layout the platform sends (spec/commands.md, "Layout: DRAWING_SET").
pub fn from_legacy(
    shape: u32,
    args: Option<&prost_types::ListValue>,
    field: &str,
) -> Result<DrawingShape, Status> {
    let args = to_lua(&prost_types::Value {
        kind: Some(prost_types::value::Kind::ListValue(
            args.cloned().unwrap_or_default(),
        )),
    });
    DrawingShape::from_legacy(shape, &args).ok_or_else(|| {
        invalid(
            field,
            "shape 1 or 4 with two points, 3 with 2 to 7,990, 5 with one, 6 with four, or 7 with 3 to 7,990; each point an object of finite x, y and z within 10,000 km of 0; then color and fill (each a list of four numbers from 0 to 1), the line type (a whole number from 0 to 6; for shape 5 the font size, a whole number from 1 to 100), read only and the message",
        )
    })
}

/// The API form of a stored drawing.
pub fn to_pb(d: &DrawingShape) -> pb::DrawingShape {
    use drawing_shape::Kind as K;
    let c = |v: &[f64; 4]| pb::Color {
        red: v[0],
        green: v[1],
        blue: v[2],
        alpha: v[3],
    };
    let pts = |p: &[MapPoint]| p.iter().copied().map(pb_point).collect();
    let kind = match &d.kind {
        ShapeKind::Line { from, to } => K::Line(drawing_shape::Line {
            from: Some(pb_point(*from)),
            to: Some(pb_point(*to)),
        }),
        ShapeKind::Arrow { from, to } => K::Arrow(drawing_shape::Arrow {
            from: Some(pb_point(*from)),
            to: Some(pb_point(*to)),
        }),
        ShapeKind::Polygon(p) => K::Polygon(drawing_shape::Polygon { points: pts(p) }),
        ShapeKind::Freeform(p) => K::Freeform(drawing_shape::Freeform { points: pts(p) }),
        ShapeKind::Quad(p) => K::Quad(drawing_shape::Quad { corners: pts(p) }),
        ShapeKind::TextBox { at, font_size } => K::TextBox(drawing_shape::TextBox {
            at: Some(pb_point(*at)),
            font_size: *font_size,
        }),
        ShapeKind::Circle { center, radius } => K::Circle(drawing_shape::Circle {
            center: Some(pb_point(*center)),
            radius: *radius,
        }),
        ShapeKind::Rectangle {
            center,
            length,
            width,
            heading,
        } => K::Rectangle(drawing_shape::Rectangle {
            center: Some(pb_point(*center)),
            length: *length,
            width: *width,
            heading: *heading,
        }),
        ShapeKind::Oval {
            center,
            half_length,
            half_width,
            heading,
        } => K::Oval(drawing_shape::Oval {
            center: Some(pb_point(*center)),
            half_length: *half_length,
            half_width: *half_width,
            heading: *heading,
        }),
        ShapeKind::RoundedRectangle {
            center,
            length,
            width,
            corner_radius,
            heading,
        } => K::RoundedRectangle(drawing_shape::RoundedRectangle {
            center: Some(pb_point(*center)),
            length: *length,
            width: *width,
            corner_radius: *corner_radius,
            heading: *heading,
        }),
    };
    pb::DrawingShape {
        theater: d.theater.clone(),
        color: Some(c(&d.color)),
        fill: Some(c(&d.fill)),
        line_type: pb_line_type(d.line_type) as i32,
        text: d.text.clone(),
        read_only: d.read_only,
        kind: Some(kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theaters() -> Vec<String> {
        vec!["Caucasus".into()]
    }

    fn oval() -> pb::DrawingShape {
        pb::DrawingShape {
            theater: "Caucasus".into(),
            line_type: pb::LineType::Dashed as i32,
            kind: Some(drawing_shape::Kind::Oval(drawing_shape::Oval {
                center: Some(pb::MapPoint {
                    north: 1.0,
                    east: 2.0,
                }),
                half_length: 100.0,
                half_width: 50.0,
                heading: 0.5,
            })),
            ..Default::default()
        }
    }

    #[test]
    fn a_typed_shape_round_trips_with_its_defaults_filled() {
        let d = from_pb(&oval(), "typed_shape", &theaters()).expect("ok");
        assert_eq!(d.color, BLACK);
        assert_eq!(d.fill, CLEAR);
        let back = to_pb(&d);
        assert_eq!(back.kind, oval().kind);
        assert_eq!(from_pb(&back, "typed_shape", &theaters()).unwrap(), d);
    }

    #[test]
    fn refusals_name_the_field() {
        let field = |d: pb::DrawingShape| {
            let s = from_pb(&d, "typed_shape", &theaters()).unwrap_err();
            nanoprobe_proto::errors::detail(&s)
                .map(|d| d.field)
                .unwrap_or_default()
        };
        let mut d = oval();
        d.theater = "Syria".into();
        assert_eq!(field(d), "typed_shape.theater");
        let mut d = oval();
        d.kind = None;
        assert_eq!(field(d), "typed_shape.kind");
        let mut d = oval();
        d.line_type = 99;
        assert_eq!(field(d), "typed_shape.line_type");
        let mut d = oval();
        d.fill = Some(pb::Color {
            red: 1.5,
            ..Default::default()
        });
        assert_eq!(field(d), "typed_shape.fill");
        let mut d = oval();
        d.kind = Some(drawing_shape::Kind::Quad(drawing_shape::Quad {
            corners: vec![pb::MapPoint::default(); 3],
        }));
        assert_eq!(field(d), "typed_shape.quad.corners");
    }
}
