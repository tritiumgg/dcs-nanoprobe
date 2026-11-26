# Typed shapes: rotated shapes, a circle and a radius change

## Run 1: the DCS calls, through the eval channel

| | |
|---|---|
| DCS | 2.9.31.29434: the dedicated server under Wine 10.0 (`wine-10.0 (Debian 10.0~repack-6)`) runs the snippets; a Windows client in a blue seat watches |
| Date | 2026-11-26, local time |
| Build | No platform build on the server: eval channel only. The client ran a development build for the screenshots |
| Scripts | `lua/experiments/typed-shapes/rotated.lua` (steps `draw`, `radius`, `clear`, through `run.sh <server> <step>`); screenshots through `lua/experiments/drawing-layers/shot.lua` and `shots.lua`, the camera through `cam.lua`, in the client's hook state |
| Rehearsal | `lua/tests/typed_shapes_experiments.lua`, under the strict mock |

The snippet makes the DCS calls the typed lowering makes: points are rounded to 1 mm and
go round the outline in one direction, and heading is radians clockwise from north, of the
length axis. Each call was timed with one client connected. All results are LIVE.

### Shapes

| Item | Call | Points | Drawn on the client | Cost |
|---|---|---|---|---|
| R0, a 3 km by 1.5 km rectangle at heading 0 | `markupToAll` 6 | 4 | a filled rectangle | 1.8 ms |
| R30, the same at heading 30 degrees | `markupToAll` 6 | 4 | a filled rotated rectangle, straight edges | 16.6 ms |
| QK, a skewed quad (no right angle) | `markupToAll` 6 | 4 | a filled skewed quad | 2.9 ms |
| F30, R30's corners | `markupToAll` 7 | 4 | a filled rotated rectangle | 4.4 ms |
| OV, an oval 3 km by 1.5 km at heading 30 degrees | `markupToAll` 7 | 36 | a smooth filled rotated oval | 1.7 ms |
| RR, a rounded rectangle 3 km by 1.5 km, corner 400 m, heading 30 degrees | `markupToAll` 7 | 40 | filled, with smooth round corners | 1.9 ms |
| RC, the same with corner 750 m (half the width) | `markupToAll` 7 | 38 | a smooth filled capsule | 3.2 ms |
| CI, a circle of 1,500 m | `circleToAll` | | a filled circle | 2.5 ms |

A quad (shape 6) fills at any rotation and when skewed, so a rectangle at any heading is a
quad. A rounded rectangle whose corner radius is half its width shares two arc end points;
sent once each, its 38 points draw a closed, filled capsule. One add took 16.6 ms and the
others 1.7 to 4.4 ms; part C of the drawing-layers run saw single adds of 15 to 16 ms
among adds near 2 ms too (docs/results/2026-11-25-drawing-layers.md). Why is UNVERIFIED.

### Radius

`setMarkupRadius` on CI to 2,500 m took 19 us. The circle and its fill took the new
radius in place, and the server still listed the same 16 panels. The larger circle draws
over the RC label, which has a lower ID.

`clear` removed every drawing; the server listed none of the run's IDs after it.

## Run 2: every kind through the platform

| | |
|---|---|
| DCS | 2.9.31.29434 on the dedicated server under Wine 10.0 (`wine-10.0 (Debian 10.0~repack-6)`); no client |
| Date | 2026-11-26, local time |
| Build | Agent build `7fa5d9157dc0f5e9`, built from this step's tree before it was committed |
| Part | `typed_shapes`, `crates/xtask/src/shell_live/typed_shapes.rs`, run through `cargo xtask sidecar <server> --part typed-shapes`; the bundle is `bundles/sidecar-<server>-20261126T083417Z/sidecar.json` |
| Rehearsal | The part's unit tests (its items and verdicts); the shim's layouts, a circle, a radius change and 36- and 40-point freeforms under the strict mock (`lua/tests/dispatch.lua`); the API path for a circle, an oval and a 0.1.0 freeform against the fake sidecar (`crates/fake-sidecar/tests/layers.rs`, `typed_drawings_are_checked_read_back_and_found_by_area`); a circle's set and radius update in the fake sim (`crates/sim-fake/tests/layers.rs`). The part's own steps were not run against the fake before the live run |

The part puts 12 typed drawings in one `ReplaceMarkupGroup` through the API: a dashed line,
a polygon of 5 points, an arrow, a text box, a skewed quad, a freeform of 12 points, a
circle, a rectangle at heading 0, and a rectangle, an oval, a rounded rectangle and a
capsule turned. The sidecar lowers each and the shim makes the DCS call. In this run the
turned items had heading 30 radians (about 279 degrees) and the oval's geometry change
60 radians (about 198 degrees, which draws the same oval as 18 degrees): the part passed
degrees as radians, fixed after the run.
What the part checks does not depend on the heading; run 1 shows the client fill at 30
degrees.

Passed:

- **draw.** All 12 items applied with no item error, and the server listed 12 panels under
  the IDs `ListWorld` gave. 12 commands applied in 7.4 s over 2 agent windows of 20.7 s
  in all: 2,005 frames; the largest frame cost 434 us and 5 frames passed 250 us; 0
  overrun frames, 3 starved frames, fixed cost 4,040 us. For scale, not as a before and
  after: the 53-command bursts of the drawing-layers part A and its rerun
  (docs/results/2026-11-25-drawing-layers.md) had a largest frame of 413 and 310 us, 1 and
  2 frames over 250 us, and a fixed cost of 4,475 and 5,523 us. The workloads differ, so
  this run measures no change in the shim's cost; it had more frames over 250 us than
  either, and why is UNVERIFIED. The run stayed inside the frame budget: no frame overran.
- **radius.** The circle's radius from 1,500 m to 2,500 m: a new revision applied under the
  same DCS ID, the same 12 panel IDs, and no mark event for any platform ID.
- **style.** The color and fill of the rectangle and the oval, and the text box's font size:
  each applied under its DCS ID, with the same panels and no mark event.
- **geometry.** The oval's heading changed: it was added again under a new DCS ID above
  every other item's, and 12 panels stayed.
- **cleanup.** `RemoveMarkupGroup` left no panel from 2^30.

The pause was found and put back.
