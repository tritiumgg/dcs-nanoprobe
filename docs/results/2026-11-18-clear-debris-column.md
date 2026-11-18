# Clearing debris at a place of unknown height: the column

| | |
|---|---|
| Step | `ClearDebris` with a `Column`: a square on the map that clears at every height, so a controller clears a wreck's place without the terrain height there |
| Date | 2026-11-18 |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, the run's own provenance read) |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker, no player connected. A fresh shell mission on the Afghanistan map. Host tests and rehearsal: macOS on arm64 |
| Builds | Agent `175228366d9a23a6`, the development build of this branch; the branch changes no file the agent is built from. The probe and the tool are this branch's |
| Script | `cargo xtask commands <target> --part debris --groups 9 --units 4 --template tests/fixtures/templates/afghanistan.miz --min-ground 500`. Its reads and its kill are `lua/experiments/junk/column-origin.lua`, `column-kill.lua` and `column-read.lua` |
| Rehearsal | `lua/tests/world_exit.lua debris` (the probe, the agent and the three snippets on the mock, ground 900 m up), run by `mise run check` |
| Raw results | Bundle `bundles/commands-debris-<target>-20261118T061955Z/` (not committed: it holds server data) |
| Result | PASS. On ground 970 m up, a column with no altitude given removed the 4 wrecks of its row and left every other wreck (count 12; how the 12 divide is UNVERIFIED) |

Every figure in "The live check" is LIVE on these builds unless tagged.

## The form

A sphere and a box take altitudes above sea level. A 30 m sphere at altitude 0 at Herat
removed nothing (LIVE, docs/results/2026-11-17-person-fixes.md); the ground at Herat's
point is 970 m up ("The live check").

`ClearDebris` takes a fourth volume, `Column`: a center, of which the theater, north and
east are read and the altitude ignored, and a `half_width` in meters. The sidecar sends
it as one `CLEAR_DEBRIS` box, the square round the center from 10 km below sea level to
100 km above it. The agent, the opcode's payload and the golden file are unchanged, and
nothing reads the ground height. spec/commands.md, "Layout: CLEAR_DEBRIS", has the rules.

The agent is unchanged, so its time on the sim thread is unchanged and no overhead run
was made. The cost of a column is the cost of a `CLEAR_DEBRIS` box, measured below.

## Host tests

| Test | What it shows |
|---|---|
| `crates/sidecar` `a_column_is_a_square_box_from_the_floor_to_the_ceiling` | The box a column gives |
| `crates/fake-sidecar` `clear_debris_column` | Through the API on the fake sim: the exact box; the same box bit for bit for an altitude of 0, 5,000, 10⁹ and one that is no number; the fake removes by position, so a wreck placed 900 m up, one on the column's edge and a dead static go and a wreck outside stays; a sphere at altitude 0 is sent at y = 0 and removes nothing there; a column at sea level removes the wreck a sphere at 0 would; each refusal with its field and reason |
| `crates/fake-sidecar` `clear_debris_effects` | The sphere, the box and the whole map as before |
| `lua/tests/dispatch.lua clear-debris` | The agent's handler on the mock with the column's box: on ground 900 m up and at sea level it removes the wreck inside and the unmanaged dead static, leaves the wreck outside, the live unit and the dead static named `np#`, and makes no `land.getHeight` call. This is the test of the rule that a managed static stays |
| `lua/tests/world_exit.lua debris` | The live check's whole sequence on the mock, ground 900 m up |

The fake sim copies the handler's `np#` rule, so its `np#` case shows nothing of the
handler. `spec/transport.golden` has no diff. The Buf breaking check passes against main.

The fake sim has no terrain: a test places each wreck at the height its ground has. The
mock's `land.getHeight` and its spawns read one settable ground height.

## The live check

The tool loads a fresh shell mission and takes the first airbase whose ground is at least
500 m above sea level: Herat, ground 970.0 m at its point. The probe spawns 9 groups of
4 M-113s on a grid 150 m apart, one control group of 4 at 1,500 m, and one static the
platform manages, a `Hangar A`, 45 m north of the third group's row. The tool kills the
36 grid units with a scripted explosion 6 m beside each and runs 120 s of model time.

The probe then sends a column round the third group's row: half-width 50 m, so the row
lies 20 m inside it, and no altitude. The tool reads DCS before and after it, while no
other clear has run.

| Read | Before the column | After the column |
|---|---|---|
| Wrecks of the column's row that answer their reads, of 4 | 4 | 0 |
| Wrecks of the group farthest from it, of 4 | 4 | 4 |
| Wrecks of all 36 killed units | 36 | 32 |
| Control units alive, of 4 | 4 | 4 |
| The managed static, 5 m inside the column's northern edge | exists, life 50.0, at 969.7 m | exists, life 50.0, at 969.7 m |

The column's outcome was `APPLIED` with a count of 12 and a `cost_us` of 131. A removed
wreck's reads raise and a left wreck's answer (LIVE, docs/results/2026-11-16-clear-debris.md,
run 1). The column removed its row and no other wreck: the nearest wrecks outside it
stood 70 m from its edge along the row and 100 m across it, and all were left. So this
box did not reach 70 m past its edge.

The clears after it, each `APPLIED`:

| Clear | Count | `cost_us` |
|---|---|---|
| Sphere, 80 m, on the first group, centered at the ground height the tool read | 12 | 136 |
| Box round the second group, from 1,000 m below sea level to 9,000 m above it | 12 | 85 |
| Whole map, 256 boxes | 71 | 7,000 in all |

The whole map's boxes took 4 frames under the 2 ms commanded-work budget: a box cost
24 us at the median, 51 us at p99 and 369 us at most. One frame of the run went over the
budget, at 2,163 us, and it is attributed to one command; the agent's over-budget counter
fits the results. After the clears no killed unit's reads answered and the 4 control
units were alive.

The handler searches the volume for dead statics after `world.removeJunk`
(spec/commands.md). The column's 131 us is the first figure for that search over a box
110 km tall and 100 m wide; the box held one live static and no dead one. The count of
12 covers the row's 4 wrecks and whatever else lay in the column, the kills' craters
among it; run 3's box counted 12 for a group as well
(docs/results/2026-11-16-clear-debris.md). How the 12 divide is UNVERIFIED.

## Open

- A column over water, over a ship's deck or a bridge, and whether junk exists away from
  the ground: UNVERIFIED.
- How far past its edge a box reaches: this run's box took nothing 70 m away; an 8 m
  sphere took wrecks 30 m from its center (docs/results/2026-11-16-clear-debris.md,
  run 2). Between those the reach is UNVERIFIED.
- The cost of the handler's static search with dead statics in a tall box: UNVERIFIED.
- A dead managed static inside a column was not tried live; the mock test covers the
  rule.
