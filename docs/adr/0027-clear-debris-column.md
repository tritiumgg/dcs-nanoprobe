# ADR 0027: ClearDebris takes a ground column

## Status

Accepted

## Context

A controller removes a wreck with `ClearDebris` at the wreck's position
([ADR 0026](0026-destroyed-statics-stay-ruins.md)). The controller usually knows north
and east, not the terrain height. A `Sphere` or `Box` takes `alt` above sea level, and
0 is sea level.

A 30 m sphere at altitude 0 removed nothing on an airfield about 900 m above sea level
(LIVE, docs/results/2026-11-17-person-fixes.md). A sphere that sits on the ground also
misses wrecks near its rim on modest slopes. A tall box is the one form shown to clear
ground of unknown height (LIVE, docs/results/2026-11-18-clear-debris-column.md).

A ground-height read in DCS would give the height. Its cost and its result over water
are UNVERIFIED.

## Decision

`ClearDebris.volume` accepts a `Column`: a square on the map that clears at every
height.

- A `Column` is a `center` and a `half_width`. Each side is 2 x `half_width`, aligned
  with north and east. Only the `center`'s theater, north and east are read. `alt` is
  ignored, and any value is accepted.
- The sidecar sends a column as one `CLEAR_DEBRIS` (opcode 63) with volume `BOX`
  from the floor to the ceiling of the whole-map tiles (`FLOOR_M`, `CEILING_M` in
  `crates/sidecar/src/reconciler/debris.rs`).
  A column is an existing `BOX` payload: the agent and the golden file have no column.
- A column takes debris at every height inside it. Like the other volumes, it destroys
  dead statics that nobody manages, and never a static the platform manages.
- The sidecar names the field path in each refusal. A theater that does not match the
  loaded one is `FAILED_PRECONDITION` (`THEATER_NOT_LOADED`). A `half_width` that is
  not finite or not above 0, or an edge beyond 10,000 km, is `INVALID_ARGUMENT`.
- `Sphere` and `Box` keep `alt` above sea level. Their proto comments point to `Column`
  for clearing at the ground.

Rejected:

- A ground-height read in the agent. Its cost and its over-water result are
  UNVERIFIED, and it needs a payload change.
- Change the meaning of `alt` in `Sphere` and `Box`, or in the shared `Position`. It
  breaks existing requests.
- Leave the y axis to each controller author. A wrong guess fails with a count of 0 and
  no error.

## Consequences

- On ground 970 m up, a column with no altitude removed the four wrecks of its row and
  left every other wreck (LIVE,
  docs/results/2026-11-18-clear-debris-column.md).
- A column's corners reach 1.41 x `half_width` from the center, and DCS's edge is not
  exact: an 8 m sphere removed wrecks 10 and 30 m from its center (LIVE). A column
  also takes a wreck of another unit inside it.
- The search for dead statics runs over a tall narrow box. A column that removed 12
  objects cost 131 µs (a sphere and a box of the same count: 136 and 85 µs; LIVE, same
  results doc). The cost over many statics is
  UNVERIFIED.
- The wire layout has no column: [spec/commands.md](../../spec/commands.md),
  "Layout: CLEAR_DEBRIS".
