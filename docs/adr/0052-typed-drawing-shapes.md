# ADR 0052: Typed drawing shapes

## Status

Accepted.

## Context

0.1.0 took a drawing as a DCS `markupToAll` shape number and an untyped argument list,
passed to DCS as given. An unknown shape can crash DCS inside the call, where `pcall` does
not help: shape 8 with four points crashed and hung the server (LIVE). [ADR
0048](0048-labels-and-markup-groups-for-marks-and-drawings.md) added an allow-list of
the known shapes and their layouts. A controller still had to know each shape's argument
layout, and drawings passed no area filter because their points were opaque.

What DCS draws (LIVE, DCS 2.9.31.29434 under Wine with a Windows client,
[the drawing-layers results](../results/2026-11-25-drawing-layers.md), run 3 parts B and C,
and [the typed-shapes results](../results/2026-11-26-typed-shapes.md)):

- `markupToAll` 1 draws a line, 3 a polygon through its points (a line with two), 4 an
  arrow, 5 a text box, 6 a filled quad and 7 a freeform. Shape 2 draws nothing a client
  shows; shape 8 crashes DCS.
- A quad fills when rotated or skewed. A freeform through points on an ellipse, or round a
  rounded rectangle in order, draws smooth and filled at 16 to 64 points; one whose
  outline crosses itself draws no fill.
- `circleToAll` draws a filled circle, and `setMarkupRadius` resizes it and its fill in
  place.
- Line types 0 to 6 draw none, solid, dashed, dotted, dash-dot, long dash and long dash
  with short dash; 7 draws solid.
- `setMarkupPositionStart` re-fills through the moved first point only, and
  `setMarkupPositionEnd` adds an outline segment and leaves the fill.

The mission editor draws its ovals and circles as polygons of 36 points (ED-CODE,
`MissionEditor/modules/me_draw_panel.lua`).

## Decision

- **A drawing is a `DrawingShape`**: a theater, color, fill, `LineType`, text, read only,
  and one kind: line, polygon, arrow, text box, quad, freeform, circle, rectangle, oval or
  rounded rectangle. The store keeps the typed form; the sidecar lowers it to one DCS call
  for every add. No caller-made argument reaches DCS.
- **Lowerings.** Each `markupToAll` kind is its shape. A circle is `circleToAll`, carried
  as `DRAWING_SET` shape code `0x100`, which the shim routes to its own branch and never to
  `markupToAll`. A rectangle is a quad of its four corners at any heading. An oval is a
  freeform of 36 points, one every 10 degrees; a rounded rectangle is a freeform of four
  10-point corner arcs, with a shared end point sent once (40, 38 or 36 points). Expanded
  points are rounded to 1 mm.
- **Checks.** The API refuses a theater the campaign does not use, coordinates beyond
  10,000 km, sizes outside 1 m to 10,000 km, a corner radius above half the shorter side,
  color parts outside 0 to 1, a font size outside 1 to 100 and point counts outside each
  kind's. The shim checks every layout again, exactly, as a second line: whole line types
  0 to 6, whole font sizes 1 to 100, color parts 0 to 1, finite numbers.
- **Updates.** A put whose DCS call differs only in color, fill, line type, a text box's
  font size and text, or a circle's radius changes in place. Every other change to the
  call, and a typed change that leaves the call as it was (a theater), adds the drawing
  again under a new ID. The position setters are not used: neither moves a whole shape.
- **Area filter.** A drawing passes a `WorldFilter` area when the area meets its segment,
  point, disc, or outline and inside.
- **The 0.1.0 fields stay, deprecated.** `PutDrawingRequest.shape` and `args` are parsed
  into a `DrawingShape` with an empty theater, which passes no area filter; a put sets one
  form. `Drawing.shape` and `args` read as the `markupToAll` call sent. `MarkupGroupDrawing`,
  not in 0.1.0, takes the typed form only: its `shape` and `args` stay in the schema, marked
  deprecated, because Buf's breaking check compares against main, where they exist, and a
  call that sets either is `INVALID_ARGUMENT`. Buf's breaking check passes.
- **No progress-bar kind.** Segments with fill changes in place, or a rectangle put again
  per step, draw one; the spec gives both with their costs.

## Consequences

- A controller describes a drawing by what it is; ovals, rounded rectangles and rotated
  rectangles need no geometry of its own.
- Drawings answer area queries.
- 0.1.0 puts with out-of-range colors, line types or font sizes are refused. DCS was
  never checked with those values, except line type 7, which it draws solid.
- A drawing's theater does not decide when it is drawn: it is drawn on whatever map is
  loaded, as in 0.1.0.
- The store keeps the typed form. The 0.1.0 store (schema 1) has no `marker` table, so no
  released store holds a drawing and no migration is needed. A body written as a lowering
  alone reads back typed where it parses and is sent as stored.
