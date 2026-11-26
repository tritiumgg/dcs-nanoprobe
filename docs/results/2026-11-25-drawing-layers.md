# Drawing layers: how DCS keeps and stacks markup

What a server shows about the order of map drawings, what an update, an add under a
removed ID and an add over a held ID do, and how many points a freeform shape takes.
Run 3, part B adds what a client draws, and part C what the setters do, what a progress
bar costs, and which shapes exist. These results feed the design of labels and
markup groups for marks and drawings.

## Run 1: server only

| | |
|---|---|
| DCS | 2.9.31.29434 dedicated server, `dcs_server.release` |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Build | No platform build: eval channel only. No deploy, no DCS restart, no mission load |
| Date | 2026-11-25, local time |
| Mission | The server's own mission, paused at model time 0, no player but the server. The snippets never resume it |
| Script | `lua/experiments/drawing-layers/run.sh <server> order`, then `points` (`order.lua`, `points.lua`), server state |
| Rehearsal | `lua/tests/drawing_layers_experiments.lua`, under the strict mock |

Every drawing a snippet made was removed before it returned; a read after the run
listed none of them. All reads below are LIVE.

### Order of `world.getMarkPanels`

The panels are listed in ascending ID order, whatever the creation order:

| Step | Created, in order | Listed |
|---|---|---|
| Mixed shapes | shape 3 (two points) at 3, line 1, text 2 | 2, 3 (the line was refused, below) |
| One shape | shape 3 (two points) at 13, 11, 12 | 2, 3, 11, 12, 13 |
| Remove 2, add as 0 | text 0 | 0, 3, 11, 12, 13 |
| Freeform added | 20 | 0, 3, 11, 12, 13, 20 |

The numbers are offsets from the snippet's base ID. Run 1 used base 1,610,612,736
(`order.lua` and `points.lua` as in commit `9b3d201`). The scripts now use base
900,000,000, below the platform pool. Only the base moved. The cases are the same, and
the order depends on the offsets, not on the base.

A panel's order shows how DCS keeps its list. A client draws in the same ID order (Run
3, part B).

### Updates

- **Add over a held ID.** `markupToAll` with an ID DCS holds, here a quad over the
  shape-3 drawing 11, changes nothing a server can read: the panel keeps its place and text, and no
  `S_EVENT_MARK_ADDED` comes. The add is ignored, or applied without an event; a server
  cannot tell which.
- **In place.** `setMarkupText`, `setMarkupColor`, `setMarkupColorFill`,
  `setMarkupTypeLine` and `setMarkupFontSize` did not raise. None emitted an event, and
  none changed the panel order. `setMarkupText` changed the panel's text
  (`np 2` to `np changed`). A server cannot read a color, fill, line type or font size.
  On a client all of them change the drawing (Run 3, part B).
- **Position.** `setMarkupPositionStart` on a freeform and `setMarkupPositionEnd` on a
  shape-3 drawing did not raise and did not change the panel's `pos`. On a client both
  move the drawing (Run 3, part B).
- **Remove and add again.** The listing is in ascending ID order, so a drawing added
  under a new, lower ID is listed first. Run 1 read no same-ID re-add that drew a panel:
  the only one re-added a line, which DCS refused (see "Argument shapes"). Run 3 shows
  that an add under an ID removed earlier in the process draws nothing.

### Events

Each scripted add emitted `S_EVENT_MARK_ADDED`, and each remove `S_EVENT_MARK_REMOVED`.
None carried an initiator. A panel's `pos.y` is filled with the ground height, though
the call passed `y = 0`.

A second `removeMark` of an ID removed earlier in the same run emitted a second
`S_EVENT_MARK_REMOVED`, with the text the drawing had. A `removeMark` of an ID never
added emitted nothing.

### Argument shapes

A line (shape 1) given two points, a color, a line type, read only and a message was
refused with `Parameter #7 not table`. DCS wants a fill color after the color, as for
shape 3. The shim passes `args` as the caller gives them, so the caller's array must
hold the fill color.

### Freeform point limit

A freeform (shape 7) was accepted, and listed as a panel, at every size tried: 3, 16,
64, 128, 256, 512, 1,024, 2,048, 4,096, 7,000 and 7,990 points. The call took at most
6 ms of CPU, at 7,990 points. Lua 5.1's `unpack` stops near 8,000 values, so the shim
passes at most about 7,990 points. At the default 1 MiB inbound ring, the transport's
payload limit binds before that.

### What the mission held before

`world.getMarkPanels` listed no panel before the run, so no player mark ID was read.

## Run 2: ID limit and cost

| | |
|---|---|
| DCS, platform, mission | As run 1: paused at model time 0, no player but the server, never resumed |
| Build | No platform build: eval channel only |
| Date | 2026-11-25, local time |
| Script | `run.sh <server> ids`, then `bench` (`ids.lua`, `bench.lua`), server state |
| Rehearsal | `lua/tests/drawing_layers_experiments.lua` |
| ID base | `ids.lua` sets its own IDs. `bench.lua` used 1,610,612,736 + 3000 and + 4000 (commit `9b3d201`); it now uses 900,000,000 with the same offsets |

Every drawing and mark was removed before each snippet returned. All reads are LIVE.

### Highest ID

A shape-3 drawing with two points was added under each ID, looked up in `world.getMarkPanels` by exact ID,
and removed:

| ID | Call | Panel under that exact ID |
|---|---|---|
| 2^31 - 2, 2^31 - 1 | no raise | yes |
| 2^31, 2^32 - 1, 2^32, 2^40, 2^52, 2^53 - 1 | no raise | no, and no other panel above 1.5 × 2^30 |

A drawing takes IDs up to 2^31 - 1. Above that, DCS takes the call and shows nothing a
server can read, so the platform's drawing IDs stay below 2^31 (spec/world.md, "DCS IDs").

A mark added with `markToAll` under the same IDs showed no panel in the same call at any
of them, 2^31 - 2 included. A mark's panel is not listed within the call that adds it,
so this run read no limit for marks. Marks use 2^30 to 2^30 + 2^24 - 1, below the
drawing limit.

### Cost

`E.bench`, best time per call over 3 rounds, on the server's sim thread:

| Case | Time per call |
|---|---|
| `removeMark` and `markupToAll` of a shape-3 drawing, same ID (the add drew nothing) | 30 µs |
| The same, freeform of 100 points (the add drew nothing) | 63 µs |
| The same, freeform of 1,000 points (the add drew nothing) | 349 µs |
| `setMarkupColor` | 1.9 µs |
| `removeMark` of an ID DCS does not hold | 0.6 µs |

The three same-ID rows measured an add under an ID the case had just removed. Run 3
shows DCS draws nothing for such an add, so these rows are not the cost of an add that
draws. They are re-measured with a new ID per add in a later run (`bench.lua` now does
this). The other two rows stand.

The agent applies commands under its per-frame commanded-work budget (spec/commands.md,
"Budget"). The cost of 32 adds of 1,000-point freeforms, the reconciler's most in a
step, is UNVERIFIED until that run. The budget spreads them over frames.

## Run 3: an add under a removed ID, and part A of the live run

| | |
|---|---|
| DCS | 2.9.31.29434 dedicated server, `dcs_server.release` |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Date | 2026-11-25, local time |

### Add under a removed ID

| | |
|---|---|
| Build | No platform build: eval channel only |
| Script | `lua/experiments/drawing-layers/reid.lua` (`run.sh <server> reid`), server state; the mission paused at model time 0, never resumed |
| ID base | `reid.lua` used 1,610,612,736 (commit `9b3d201`); it now uses 900,000,000. `client.lua` used 1,610,612,736 + 1000; it now uses 900,000,000 + 1000. Only the base moved |

A shape-3 drawing with two points, or for case d a mark, was added under an ID, removed, and added again.
`world.getMarkPanels` was read for the ID after each action. All reads are LIVE.

| Case | Actions | Panel under the ID |
|---|---|---|
| a | add; remove; add again in the same call; remove again | yes; no; no; no |
| b | add; remove; add again after 2 s; read after 4 s more | yes; no; no; no |
| c | add C; remove C; add D under a new ID | yes; no; D yes, C no |
| d | mark M: add; remove; add again; read at 2 s and 4 s | yes; no; no; no, no |

An add under an ID removed earlier in the process shows no panel, at once or after real
time, for a drawing and for a mark. An add under a new ID shows one. The call does not
raise and returns normally. In case c, D was still listed at 2 s and 4 s.

Events per ID: each ID emitted one `S_EVENT_MARK_ADDED` and no change event, so the add
that drew nothing emitted none. `S_EVENT_MARK_REMOVED` counts were 3 for A, 2 for B, 2 for
C, 1 for D and 2 for M: every `removeMark` of the ID emitted one, held or not, as in
run 1. Every ID was removed before the snippet returned.

A mission load frees every ID (part B below). Whether a new DCS process starts empty was
not tried here and is UNVERIFIED.

### Part A of the live run

| | |
|---|---|
| Build | `v0.1.0-48-g0ae2a48`, agent build `f2a023ad6e0c3511`, built before the fix for this finding |
| Part | `drawing_layers`, `crates/xtask/src/shell_live/drawing_layers.rs`, run through `cargo xtask`; the bundle is `bundles/drawing-layers-run3/sidecar.json` |

Passed:

- **burst.** 53 commands over 2 agent windows of 20.0 s in all. Applied in 3.9 s. 1,911
  frames; the largest frame cost 413 µs and 1 frame passed 250 µs. 0 overrun frames, 4
  starved frames, fixed cost 4,475 µs. All 53 panels listed in ascending ID order: 3 in
  the low group, before the 50 of the front group. The groups' IDs were 1,355,808,768 to
  1,355,808,770 and 1,360,003,072 to 1,360,003,121.
- **in_place.** A style change was applied after 2 revisions with the same entity,
  one item under the label and all 53 panel IDs unchanged. No mark event came during it.
- **cleanup.** No panel was left.

Before each add in the burst, the build called `removeMark` on the never-held ID, and
every panel appeared. A remove of a never-held ID therefore blocks no later add under it
(LIVE; the spec cites this for "blocks nothing").

Failed: **hide**, with "the panels: not within 300s". The build re-added items under IDs
it had removed. The probe above shows DCS draws nothing for such an add, so the panels
the step waited for never came. The mission's pause was found and restored.

### Part A rerun

| | |
|---|---|
| DCS | 2.9.31.29434 dedicated server under Wine 10.0 (`wine-10.0 (Debian 10.0~repack-6)`) |
| Date | 2026-11-26, local time |
| Build | `v0.1.0-57-ga188877`, agent build `53c83072dc69e430`; every add takes a never-used ID, and the draw rank orders full redraws |
| Part | `drawing_layers`, run through `cargo xtask sidecar <server> --part drawing-layers`; the bundle is `bundles/sidecar-gameserver-20261126T023855Z/sidecar.json` |

Every step passed:

- **burst.** 53 commands over 2 agent windows of 20.0 s, applied in 4.1 s. 2,226 frames;
  the largest frame cost 310 µs and 2 frames passed 250 µs. 0 overrun frames, 3 starved
  frames, fixed cost 5,523 µs. All 53 panels listed in ascending ID order, in the order of
  the adds: the 50 of the front group from 1,073,741,824 (2^30), then the 3 of the low
  group.
- **in_place.** A style change was applied after 2 revisions with the same entity, one
  item under the label and all 53 panel IDs unchanged. No mark event came during it.
- **hide.** The front group's 50 panels went and came back under 50 new IDs, all above
  the old highest ID (1,073,741,877 to 1,073,741,926), in the order of the adds; the low
  group's 3 panels kept their IDs. A hidden item's `dcs_id` read 0. 50 commands over 2
  windows, applied in 4.1 s; the largest frame cost 206 µs and none passed 250 µs.
- **cleanup.** No panel was left.

The run found the mission paused and left it paused. The harness has no sidecar
restart, so that step is skipped.

#### A new DCS process frees a removed ID

`newproc.lua`, around the restore's first DCS restart, 2026-11-26: before the restart a
text under ID 900,011,001 was added (a panel), removed (no panel) and added again (no
panel: a removed ID is dead in the process). After the restart an add under the same ID
made a panel. A new DCS process frees every ID the last one removed (LIVE). The order a
client shows after a restart was not read.

### Run 3, part B: client check

| | |
|---|---|
| DCS | 2.9.31.29434 on a Windows client and on a dedicated server under Wine 10.0 (`wine-10.0 (Debian 10.0~repack-6)`) |
| Date | 2026-11-25, local time |
| Build | No platform build: eval channel only |
| Scripts | `lua/experiments/drawing-layers/`: `client.lua` (steps `place`, `readd`, `newid`, `over`, `flicker`, `inplace`, `geometry`, `highids`, `watch`, `read`, `clear`), `shapes.lua`, `kinds.lua`, `panels.lua`, `styles.lua`, `reload.lua`, `shot.lua`, `shots.lua`, `loadfree.lua`; server state, run through the eval channel |
| Rehearsal | `lua/tests/drawing_layers_experiments.lua`, under the strict mock |

A person watched the client's F10 map while the server's snippets ran. After each step a screenshot of the client's
screen was taken from the client's hook state through the eval channel with
`DCS.makeScreenShot()` (`shot.lua`, listed by `shots.lua`; 2560 by 1440). Each question
below was read from those screenshots and from the server's `world.getMarkPanels`.
`client.lua` keeps a round counter: each `clear` starts a new round with its own block of
1,000 IDs, because DCS never draws a removed ID again. The first round used IDs from
900,001,000. The IDs below are offsets from the round's base.

Two further probes ran from a scratch folder, not from the repository. The first, a
dead-ID probe, is `loadfree.lua` below in the style of `kinds.lua`; `loadfree.lua`
itself has run only on the mock. The second removed a player's mark with
`trigger.action.removeMark(id)` inline. The `flicker` step was changed before its run:
each second it removes the line's ID and adds a filled quad (shape 6) under a fresh ID
(61 to 70) in one call. All results below are LIVE.

#### What shape 3 draws

`order.lua`, `ids.lua`, `bench.lua`, `points.lua` and `client.lua`'s `rect` helper call
shape 3 with two points, meant as a rectangle. On a client it draws a line from corner
to corner. `shapes.lua` drew five cells beside one airbase:

| Call | Drawn on a client |
|---|---|
| `rectToAll(a, b)` | a filled rectangle |
| `markupToAll` 1, two points | a line |
| `markupToAll` 3, two points | a line |
| `markupToAll` 4, two points | an arrow |
| `markupToAll` 6, four corners | a filled quad |

All ten panels (five shapes, five labels) were listed on the server. `markupToAll` 3
with three points drew a filled triangle: shape 3 is a polygon through its points. A
rectangle through `markupToAll` is shape 6 with four corners. The earlier runs' "rect"
and "square" cases were shape-3 lines.

#### Stacking: ID, creation, shape kind

Stack A was three shape-3 lines: ID 30 (red) created first, ID 10 (green) second, ID 20
(blue) third. Stack B was a red circle (52) created first, a green freeform (50) second,
a blue shape-3 line (51) third and a text box (53) fourth.

| Case | Created in order | Drawn on a client |
|---|---|---|
| Stack A, connected | 30, 10, 20 | Green (10) fully hidden under red and blue |
| Stack B, connected | 52, 50, 51, 53 | Circle (52) over the freeform (50); line (51) over the freeform and under the circle; text box (53) drawn |
| Stack A and B, a client that joined after the adds | same | Same as connected |
| Circle under 900,003,090, added first, and a quad under 900,003,010, added after (`kinds.lua`) | circle, then quad | The circle covers the quad |
| After a mission load, the same IDs placed again | same | Same ID stacking |

A client draws by ascending ID: the highest ID is on top. Creation order does not
decide it, and neither does shape kind (circle against quad). A client that joins after
the adds draws by ID too.

#### Re-add, a lower ID, and an add over a held ID

| Step | Server panels | Client |
|---|---|---|
| Remove ID 10 and add it again (`readd`) | none under 10, now and after 2 s | Unchanged (10 was hidden anyway) |
| Remove ID 30 and add it as ID 5 (`newid`) | 5 listed | The line draws again |
| Add a yellow quad under held ID 20 with no remove (`over`) | unchanged | Nothing appears; the blue line is unchanged |

An add under a removed ID draws nothing on the client either, as on the server. An add
over a held ID is ignored on the client. A fresh ID draws, at its place in the ID order.

#### Flicker on a swap to a fresh ID

Each second for ten seconds, in one call, the line's ID was removed and a filled quad was
added under a fresh ID (61 to 70). Screenshots at about 5 s and 13 s showed the quad,
over the circle (52) and over the text box (53); the server listed 70 at the end. The
person watching reported the quad steady, with one visible flicker in the ten swaps.

A swap to a fresh, higher ID puts the drawing on top of everything with a lower ID. A
swap shows at most one flicker in ten (the person's report).

#### In-place setters on a client

| Call (on the quad 70 and the text 53) | Changed on the client |
|---|---|
| `setMarkupColor` | outline red |
| `setMarkupColorFill` | fill yellow |
| `setMarkupTypeLine` 2 | dashed outline |
| `setMarkupFontSize` 24 | larger text |
| `setMarkupText` | text `np changed` |

Order did not change: the quad stayed on top. No remove and add was needed.

#### Position setters on a client

| Call | Changed on the client |
|---|---|
| `setMarkupPositionStart` on freeform 50 | the first point moved about 3 km south |
| `setMarkupPositionEnd` on quad 70 | one vertex moved north-east; the outline gained a long dashed edge to the new point and the fill stayed |

The server's panel `pos` did not change in either case (run 1).

#### IDs near and above 2^31

Four filled shapes in a row, under 2^31 - 1, 2^31, 2^32 and 2^53 - 1 (`highids`):

| ID | Drawn on a client |
|---|---|
| 2^31 - 1 | yes |
| 2^31 | yes |
| 2^32 | no |
| 2^53 - 1 | no |

Run 2 read no server panel above 2^31 - 1, so 2^31 draws on a client with no panel on
the server. An int32 wrap is a possible cause (UNVERIFIED). The platform's pool tops out
at 2^31 - 1, which draws.

#### Coalition audience

Two filled quads, one for coalition 1 and one for coalition 2, and two labels for all
(`kinds.lua`), watched from a coalition 2 client:

| Item | Drawn on the coalition 2 client |
|---|---|
| Quad for coalition 1 | no |
| Quad for coalition 2 | yes, over the quad under 900,001,010 it overlaps (the higher ID) |
| Labels for all | both |

#### A mission load removes markup, and frees every ID

`reload.lua` loaded the mission again, paused (two players connected).

| After the load | Result |
|---|---|
| Panels on the server | 0 |
| Client map | empty |
| Seven drawings placed under the IDs the load removed | seven panels listed, all drawn, in ID stacking |
| Quads under 900,001,010 and 900,001,030 (removed before the load) and 900,009,999 (never used) | three panels listed, three drawn |

A mission load removes the marks and drawings a script made, on the server and on the
client. It frees every ID, removed ones included, so an add under a pre-load ID draws
again. `E.store` survived the load. The probe for the last row is `loadfree.lua`: it adds
the three quads 20 km north of the first airbase, reads which hold a panel, and a second
run removes them.

#### Player marks

A player placed marks from the F10 map. Players have no drawing tools. `watch` recorded
the mark events; `panels.lua` read `world.getMarkPanels` for IDs below 900,000,000.

| Field of a player's mark panel | Value |
|---|---|
| `author` | the player's name |
| `coalition` | 2 |
| `groupID` | -2147483648 |
| `idx` | 251,658,241 for the first mark (15 × 2^24 + 1), below 2^30 and outside the platform's pool |
| `initiator` | a table with an `id_` field |
| `pos` | a position |
| `text` | the mark's text |
| `time` | a time |

| Question | Result |
|---|---|
| Events for a mark placed with text | `S_EVENT_MARK_ADDED` (25) with empty text, then `S_EVENT_MARK_CHANGE` (26) with the text. Both carry an initiator with a name, coalition 2 and group -1 |
| Author across a disconnect | While the player was out the panel stayed, with `author` and no `initiator` field. After the player rejoined it had its `initiator` again, with the same `id_` |
| What the client shows | An open mark box shows the mark number, the time, the author's name and the text |
| Edit of the text | One `S_EVENT_MARK_CHANGE` with the initiator and the new text. The panel keeps `idx` and `author`; `text` and `time` update |
| Script removal (`trigger.action.removeMark(id)` inline, in the server scripting state) | The mark is gone on the client and no panel is left. `S_EVENT_MARK_REMOVED` (27) carries an initiator with a name, the mark's author and not the script, and the last text |
| Player removes own mark | `S_EVENT_MARK_ADDED` (empty), `S_EVENT_MARK_CHANGE` (text), `S_EVENT_MARK_REMOVED` (text), each with the initiator: the same shape as a script removal |
| Mark closed with no text | One `S_EVENT_MARK_ADDED` with empty text and no change event. The panel exists with `text` `""` |
| ID sequence | 251,658,241, 251,658,242, 251,658,243, 251,658,244: one more for each mark. The counter did not reset at the mission load that came between the first and second mark |

A script removal of a player's mark cannot be told from the player's own removal by its
initiator. The platform does not touch players' marks.

#### Shape IDs, line types and text size

`styles.lua`, 30 to 42 km north of the first airbase:

| Call | Drawn on a client |
|---|---|
| `markupToAll` 1, line type 0 | no line |
| line type 1 | solid |
| line type 2 | dashed |
| line type 3 | dotted |
| line type 4 | dash-dot |
| line type 5 | long dash |
| line type 6 | long dash and short dash |
| line type 7 | solid, as type 1 (an out-of-range value may fall back; UNVERIFIED) |
| `markupToAll` 2 with a center and a radius | refused: `Parameter #5 not table` |
| `markupToAll` 3 with three points | a filled triangle |
| `markupToAll` 5 with one point | a text box at the point, the fill as its background |
| `textToAll` sizes 8, 14 and 32 | the text scales with the size |

The calls take no font family.

### Run 3, part C: setters, progress bars, shapes and a crash

| | |
|---|---|
| DCS | 2.9.31.29434: the dedicated server under Wine 10.0 runs the snippets; a Windows client in a blue seat watches |
| Date | 2026-11-25 and the early hours of 2026-11-26, local time |
| Build | No platform build: eval channel only |
| Scripts | `lua/experiments/drawing-layers/`: `partc.lua` (steps `list`, `draw`, `move`, `refresh`, `other`, `bars`, `sweep`, `shapes2`, `rbars`, `rsweep`, `rhalf`, `clear`, through `run.sh <server> partc <step>`), `startfluid.lua`, `corners.lua`, `startbar.lua`, `unknown.lua` (through `probe.sh`), `shape3.lua`, `cam.lua`; server state, except `cam.lua`, `shot.lua` and `shots.lua` in the client's hook state |
| Rehearsal | `lua/tests/drawing_layers_experiments.lua`, under the strict mock |

`cam.lua` moves the client's F10 view with `Export.LoSetCameraPosition`; a screenshot
3 s later shows the new place. Times are from `E.clock`, with one client connected.
All results are LIVE unless tagged.

#### Setters

`trigger.action` holds `setMarkupColor`, `setMarkupColorFill`, `setMarkupFontSize`,
`setMarkupPositionEnd`, `setMarkupPositionStart`, `setMarkupRadius`, `setMarkupText` and
`setMarkupTypeLine`. No Lua file in the server install names a `setMarkup*` or
`markupToAll` call (ED-CODE, a search of the shipped `.lua` files). Each setter costs 10
to 25 microseconds.

| Call on a drawing a client shows | What the client draws |
|---|---|
| `setMarkupPositionStart` on a quad, freeform, polygon or rectangle | the first point moves and the fill is drawn again through the new point |
| `setMarkupPositionEnd` on the same | an outline segment out to the new point; the fill does not change |
| `setMarkupPositionEnd` on a line | the line ends at the new point |
| any setter after an end-point move | no change to the fill |
| `setMarkupRadius` on a `circleToAll` circle | the circle and its fill take the new radius |

A client that joins after the moves draws each drawing exactly as a connected client
shows it, end-point outline segments and start-point fills included.

#### Progress bars

Each bar is 10 km long and 1.5 km high and steps from 0 to 100% in 11 steps, a second
apart.

| Bar | Step | Cost a step | Drawn |
|---|---|---|---|
| Segments: 10 quads, the fill alpha set per cell | `setMarkupColorFill` | 21 us | correct |
| One quad, both far corners moved | `setMarkupPositionStart` and `End`, then a fill | 33 us | a wedge: only the first point re-fills |
| One quad, first point far top | `setMarkupPositionStart` | 22 to 25 us | a wedge, never a bar |
| Flat `rectToAll`, end moved | `setMarkupPositionEnd` | 20 us | stays at its drawn length |
| `rectToAll` body and two `circleToAll` ends | body end moved, right circle added again | 1.78 ms | the body stays full length with a square end; the moved circle hides inside its fill |
| Freeform capsule, 16 points an end arc, first point the far tip | `setMarkupPositionStart` | 11 us | a spike line out to the tip; the arc stays, no fill |
| Freeform capsule, 16 points an end arc | added again under a fresh ID, the old ID removed in the same call | 1.71 ms | correct at every step |
| `rectToAll` | added again the same way | 1.78 ms | correct at every step |

The slowest re-add step took 15 ms. The person watching saw no flicker in any sweep. A
setter moves one point; a bar end needs two points or an arc, so only segments or a
re-add per step draw a bar of changing length.

#### Ovals and rounded rectangles

A freeform (shape 7) through points on an ellipse draws a smooth filled oval at 16, 32,
36 and 64 points, and so does a polygon (shape 3) of 32 points. A freeform rounded
rectangle draws filled with round corners at 4, 8, 9 and 16 points a corner arc when its
points go round the outline in order. A freeform whose outline crosses itself draws its
outline with no fill. The mission editor draws its own ovals and circles as polygons of
36 points (ED-CODE, `MissionEditor/modules/me_draw_panel.lua`, `circleSides_`).

#### Shape 2, shape 3 with two corners, and shapes 8 and above

| Call | Result |
|---|---|
| `markupToAll` 2 with a center and a point, or with two points | no error and a server panel; the client draws nothing |
| `markupToAll` 2 with a center and two radii, or a center and a radius | refused: `Parameter #5 not table` |
| `markupToAll` 3 with two opposite corners, in each of 4 orders | a line from corner to corner, no fill |
| `markupToAll` 8 with four corners of a square, then color, fill, line type, read only and a message | DCS crashed inside the call: a crash report and a minidump in `dcs.log`, then the process hung with no hook answering. `pcall` did not catch it. |

One earlier run of `shape3.lua` crashed the same way somewhere among shapes 8 to 12; its
header names it. The shape 8 run sent that one call alone, so the crash is shape 8 with
four points. Shapes 9 to 12 were not probed. The platform refuses every shape but 1 and 3
to 7 (spec/commands.md, "Marks, drawings and menu items in DCS").

## Open

- A DCS restart: the order a client shows after it. A new DCS process frees a removed ID
  (LIVE, "A new DCS process frees a removed ID").
- The cost of an add that draws, with a new ID per add (the bench with fresh IDs).
- Why a drawing under 2^31 draws on a client when the server lists no panel for it (an
  int32 wrap is UNVERIFIED).
- Typed shapes, line types and text size in the API, for a later step: the shape's
  argument layout, line types 0 to 7 and text sizes are read above, and the API carries
  them as raw arguments.
- What shape 2 draws: two layouts are taken and show nothing on a client.
- Shapes 9 to 12, each alone: whether any exists. Shape 8 crashes DCS.
- Reuse of IDs after a mission load. A load frees every ID (one kind of load, a reload of
  the same mission), and the platform reuses none within a DCS process. Reuse needs, first,
  a LIVE check of each kind of load, with a late joiner and a mission script's own marks.
  It then needs a shim mark kept for each mission generation, removals bound to the
  generation, and the crash-retry rule kept for each generation.

The client check is `run.sh <server> client <step>`, with the steps in `client.lua`'s
header, and `run.sh <server> reload` between them.
