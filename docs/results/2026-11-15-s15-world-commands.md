# World commands

| | |
|---|---|
| Step | S15 exit test: 100 groups spawned and removed with every over-budget frame attributed to a command. While paused, a group destroyed and added again under the same name in one call is one group after a resume, with its new units. Experiment table filled. |
| Date | 2026-11-15. Experiment runs, exit test runs 1 to 3, overhead runs 1 and 2 |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, run provenance) |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Host rehearsal: macOS on arm64. |
| Builds | Experiments ran on agent build `f918a40925ccc388` (`deploy --dev`). Exit test run 1 and overhead run 1 ran on `980477b67cd10eff`. Exit test run 3 and overhead run 2 ran on the final tree, `9590d6f6771c67a1`. Exit test run 2 ran on `980477b67cd10eff` (the same tree as run 1 plus the harness fix). |
| Script | `cargo xtask commands <target> --part world` (`crates/xtask/src/commands/world.rs`), which runs the probe in `crates/fake-agent/src/world_exit.rs`; `cargo xtask eval <target> <state> <file>` for the snippets in `lua/experiments/s15/`; `cargo xtask routes <target>` (`crates/xtask/src/routes.rs`) for E37; `cargo xtask sampling <target> --part mirror` for the overhead |
| Rehearsal | `crates/sim-fake/tests/world_commands.rs`, `crates/fake-sidecar/tests/world_commands.rs`, `lua/tests/world_exit.lua`, `lua/tests/s15_experiments.lua`, `lua/tests/routes.lua`, `lua/tests/dispatch.lua`, all run by `mise run check` |
| Bundle | `bundles/commands-<target>-<stamp>/`, `bundles/sampling-<target>-<stamp>/`, `bundles/routes-<target>-<stamp>/` (not committed: they hold server data) |
| Result | PASS with open items. The exit test passes on the final tree. Overhead matches S14 and meets the owner's targets. E8's attack part is inconclusive. Every other experiment has a verdict. |

All figures are LIVE unless tagged. Every run used a server with no players connected.

## Exit test

The probe sends 100 `GROUP_SPAWN` commands for groups of four M-113, reads the world until
all 400 units are seen, then removes the groups with 100 `GROUP_CAPTURE_DESTROY` commands and
reads until all 400 are gone. It checks the `RESULT` stream against the budget. It then runs the paused replace.

Rules the probe applies:

- Every group spawns once, every unit is seen once and is gone once, and no `RESULT` is
  missing or repeated.
- A command is over budget when its `RESULT` carries a `cost_us` above the commanded-work
  budget of 2 ms. A frame is over budget when its commands' costs add up to more than the
  budget.
- An over-budget frame is attributed to a command when exactly one command in it took the
  commanded total past the budget. The agent stops taking commands once the budget is spent,
  so no unpaused frame holds more than the budget plus one command (the cap-plus-one rule).
  A frame that kept taking commands after the budget was spent fails the run.
- The change in the agent's `commands_over_budget` counter over the run lies between the
  number of results above the budget and the number at or above it.
- Paused replace: with the simulation paused, one call destroys a group and adds it again
  under the same name. After the resume the name holds one group with the new units, the old
  unit objects are gone once with `DESTROYED`, and the new units are born once.

| Run | Build | Result | Groups spawned and removed | Over-budget results (counter delta) | Over-budget frames | Most commands in a frame | Largest frame, commanded work |
|---|---|---|---|---|---|---|---|
| 1 | `980477b67cd10eff` | FAIL, in the harness | 100 and 100, 400 units seen and gone | 89 (89) | not reported | not reported | not reported |
| 2 | `980477b67cd10eff` | PASS | 100 and 100, 400 units seen and gone | 91 (91) | 110, all attributed | 7 | 7,328 us |
| 3 | `9590d6f6771c67a1` | PASS | 100 and 100, 400 units seen and gone | 86 (86) | 101, all attributed | 27 | 21,896 us |

Run 1 failed in the harness's own read, not in the platform. Both probe verdicts passed: 100
spawned and removed, 400 units seen and gone, counter delta 89, no unattributed frame, no
duplicate, and the paused replace passed. The failing step read DCS after the resume and
counted `Unit.getByName(<old unit name>)` as an old unit left. The replacement keeps the old
unit names, so the 4 new units counted as 4 old ones (`old_units_left 4`). The read now counts
the groups under the name across every coalition and requires each unit to be of the new
type, and the run waits for the agent's pending-destroy set to drain.

No run has a duplicate spawn, object, or gone record, or a bad result (run 3: `bad_results`
0). No unpaused frame held more than the budget plus one command. One `GROUP_SPAWN` of a
four-unit group cost up to about 22 ms of commanded work in a single frame (frame 484, run
3). That frame is over budget and is attributed to that command: the budget is checked
between commands, not inside one.

Run 3 also sends `DAMAGE` and reads the agent's pending-destroy counters.

| Measure, run 3 | Value |
|---|---|
| `DAMAGE` 50 % to one unit per group, through hook dispatch | 100 sent, 100 applied, 100 life values read back |
| `DAMAGE` cost | median 70 us, p99 122 us, max 133 us |
| `DAMAGE` commands over budget | 0 |
| Over-budget frames that contain a `DAMAGE` | 27 (the frame's other commands set the total) |
| Pending destroys | drained, `pending_destroy` 0, `pending_expired` 0, `pending_overflow` 0 |

Paused replace, runs 2 and 3: applied; the old units are gone once, each with `DESTROYED`
(4); 4 births while paused; one group under the name; `old_in_world` 0. Run 2's new group
object ID is 1000500. After the resume DCS read one group with four units. Run 3 read each
type as BTR-80.

## Overhead against S14

The mirror part at 2,000 objects, compared with S14 run 2 on the final S14 tree
(`2026-11-14-s14-command-path.md`).

| Measure | S14 run 2 | Run 1 (`980477b6`) | Run 2 (`9590d6f6`) |
|---|---|---|---|
| Mean share from the 800 spawn | 0.201 % | 0.204 % | 0.204 % |
| Mean share at 2,000, settled | 0.193 % | 0.195 % | 0.190 % |
| Largest 10 s window | 0.36 % | 0.307 % | 0.354 % (from 800) |
| Frames over 250 us | 104 of 38,998 (0.27 %) | 91 of 40,276 (0.23 %) | 93 of 39,857 (0.23 %) |
| Largest frame | 3,205 us | under 1,024 us (pump histogram top bucket) | under 2,048 us (histogram bucket) |
| Median position interval, stall-free | 1.006 s | 0.998 s | 0.998 s |
| Worst stall-free window | 3.07 s | 1.069 s (ceiling 1.25 s) | 1.063 s (ceiling 1.25 s) |
| Fill-window position | 2.07 s | worst 1.11 s (limit 3 s) | worst 1.04 s |
| DCS fps at 2,000 (p5) | 51.4 (0.40) | 55.3 (0.41) | 54.3 (0.41) |

The mirror matched at 500, 1,200 and 2,000 in both runs. Run 1 reported two stall windows at
2,000 objects (gaps of 3.0 s and 2.2 s) without judging them. The owner's targets hold: mean
share at or under 0.3 % and frames over 250 us under 1 %. The window ceiling and the
fill-window check passed in both runs, unlike S14 run 2. The mirror run sends no commands,
so S15's command path does no work in it. The largest frame is a histogram bucket here, not
the exact figure the S14 table gives. Overhead run 1 did not repeat the check for players
before the launch; the tool's restart refuses a server with players.

## Experiments

| Experiment | Rule | Runs | Verdict | Key facts |
|---|---|---|---|---|
| E6 warehouse | Set, then read back | 2 | Met on a limited warehouse. Not met on an unlimited one: the set is ignored. | Weapons, aircraft and four liquids read back. Unlimited airbases ignore every setter. LIVE. |
| E8 capture | Captured, destroyed, added again with the same name, compared | 1 | Met for aircraft and helicopter. Ship met except fuel, which a ship does not have. | Flares and chaff do not appear in `getAmmo`. LIVE. |
| E8 attack | Whether a re-issued attack task with the new group ID engages | 2 | Inconclusive | The control group never fired. LIVE. |
| E14 ammo slots | Verified on the ground-unit types | 1 | Met: 10 of 10 types | Read equals requested plus a per-slot offset. One unmapped row on the M-2 Bradley. LIVE. |
| E16 damage | Read back | 1 | Met for aircraft, helicopter and ground unit at 50 %. Ship removed at 50 %. Static does not respond. | At 0 every unit type is removed; only the airborne unit logged death events. LIVE. |
| E21b names | Accepted, found and read back exactly | 1 | Met for lookup, `getName`, listing and client display. Event initiator not tested. | Base-64 alphabet and leading `~ . # !` pass. LIVE. |
| E37 route shapes | Cost of each shape at 400 and 1,200 units | 2 sweeps | Decision below | Dense waypoints and single-unit groups cost the most. LIVE. |

### E6 warehouse

Run 1 used the template mission. Its warehouse flags were unreadable (`env.mission.warehouses`
is not exposed in the `server` state), so the snippet picked the first airbase. Every count
read 1,000,000. `setItem(12)` on a weapon and an aircraft, and `setLiquidAmount` on liquids 0
to 3, raised nothing and read back 1,000,000, while the inventory read showed +5 after an
`addItem`. The airbases of that mission behave as unlimited.

Run 2 used a mission packed with `cargo xtask miz <template> <out> --limited-airport 12`, which
rewrites the `warehouses` member so that airport 12 is limited, with 10 F-15C, 100 Mk-82 and the
template's fuel. The mission loaded with `cargo xtask load`.

| Check on the limited airbase | Result |
|---|---|
| Mk-82 at the start | 100 |
| `setItem` 12 | reads back 12 through `getItemCount` and the inventory |
| `addItem` 5 | 17 |
| `removeItem` 3 | 14 |
| Remove 24 when 14 are present | 0, no error |
| Liquids 0 to 3: set 54321 | reads back 54321; add and remove apply; remove past zero gives 0 |
| F-15C: `setItem` 12 | reads back 12; add and remove apply |

The F-15C inventory showed 0 aircraft at the start: the `aircrafts` table did not register as
stock. The cause is not diagnosed. Every value was set back after the read.

### E8 capture

An F-15C (air), a UH-1H (helicopter) and a speedboat (ship) were captured, destroyed, added
again under the same name and compared. Name, type, heading, pitch, roll, life, `life0`,
position and ammunition rows (type and count, offset 0) were equal within tolerance for all
three. Fuel was within tolerance for the aircraft (difference -1.3 kg) and helicopter
(-0.26 kg). The object ID and mission ID change on add, as expected. The ship has no
readable fuel. No flare or chaff row appears in `getAmmo` for any of the three; the F-15C
lists 3 ammunition rows. Flares and chaff cannot be read that way.

### E8 attack

Group A was 3 M-1 Abrams with the rule of engagement set to open fire. Group B was 2 T-72B,
ordered to hold fire, 1,500 m away in run 1 and 1,000 m in run 2. The phases were: A attacks
B; B is destroyed; B is added again under the same name; A's task is issued again with B's
new ID.

B re-added with a new group ID (1000060, then 1000063; the name was free right after the
destroy), and the task could be issued again. In neither run did A fire in any phase: no
`SHOT`, `SHOOTING_START`, `HIT` or `KILL` event, and its ammunition stayed at 10,056 rounds.
The first phase lasted 25 s in run 1 and 45 s in run 2. The control phase, the first task
left as it was, never produced fire, so the result cannot say whether a re-issued task engages.
The cause is not found. A clear line of sight held (`land.isVisible` true, heights 72 and 86
m).

The owner notes that DCS AI is unreliable with attack orders and can make a pass or two before
it hits. A window of 25 to 45 s with one three-unit shooter is too short to call a task
ignored. S18's part of E8 needs a longer window and several shooters before it concludes that
a task was ignored. `AttackUnit` on a unit and on a static is UNVERIFIED and goes to S18.

### E14 ammunition slots

The ten types are T-72B, M-1 Abrams, BMP-2, M-2 Bradley, ZSU-23-4 Shilka, Osa 9A33 ln,
Leopard-2, BTR-80, M1126 Stryker ICV and Ural-375 ZU-23. Every found slot maps to one
`getAmmo` row. With all rows asked at once, the count read equals the count requested plus
the slot's offset, on every row of every type: T-72B 7 of 7, M-1 6 of 6, BMP-2 5 of 5, M-2
4 of 4, ZSU 2 of 2, Osa 1 of 1, Leopard 4 of 4, BTR-80 4 of 4, Stryker 2 of 2, Ural 2 of 2.

The offset is not a single shift. For 1 round asked it reads +11 on a T-72B gun, +15 on an
M-1 gun and +302 on a Bradley 25 mm slot. It differs between slots of one row, and it is 0
for single-count rows (Osa, TOW, Konkurs, and the 7.62 mm rows on the BMP-2 and Bradley) in
the small probe. The M-2 Bradley's row 2 (M791 25 mm AP) has no slot that feeds it. Restoring
ground ammunition therefore needs a per-type map. That goes to S18.

### E16 damage

`a_unit_set_life_percentage` through the `hook` state.

| Subject | At 50 | At 0 |
|---|---|---|
| Air F-15C | life 20 to 10, read back | not found at 1 s, gone at 3 and 10 s |
| Parked F-15C | life 20 to 10, read back | gone |
| UH-1H | life 12 to 6, read back | gone |
| M-113 (ground) | life 3 to 1.5, read back | gone |
| Speedboat | gone 1 s after the call, life was 1.6 before; not readable | not tested separately |
| Static (hangar) | setter returns ok with the static's ID, life stays 50 | life stays 50: no effect |

Only the airborne F-15C logged `UNIT_LOST` and `EJECTION` (at 8.2 s). The helicopter, parked
aircraft and ground unit logged no death event when set to 0. A unit killed this way reads as
removed or missing, not dead. The reconciler must read it so; this is an open item.

### E21b names

The snippet `lua/experiments/s15/e21b-names.lua` added 33 objects. All were found by
`getByName`, read back byte for byte with `getName`, and appeared in the `coalition.getGroups`
and `getStaticObjects` listings. 0 failures.

- Names that differ only by case (`np.g.aB` and `np.g.Ab`, and the same for units and
  statics): all exist, with distinct IDs in every pair. A unit `xY` and a static `Xy` also
  coexist.
- A group, unit and static named with the whole alphabet `A-Z a-z 0-9 - _ + /` (prefixed `G`,
  `U`, `S`) are accepted and read back exactly. So are `~G`, `~U` and `~S`.
- `.k3F19`, `#k3F7` and `!k3F2A`, each as a group and as a unit, and as statics with an `s`
  suffix, are accepted, found, exact and listed. A unit `.k3F19` and a static `!k3F19` share
  one namespace and both survive.

Client display: the owner looked at these names on a Windows client. They display exactly as
written (LIVE, owner).

Not covered: the event initiator check. The mission started paused after load, the snippet
waited 6 s of model time, and it timed out before its event section and cleanup. The objects
were removed with `cargo xtask reset <target>`. The snippet needs `DCS.setPause(false)` first;
the file is unchanged. The event round trip is UNVERIFIED.

### E37 route shapes

`cargo xtask routes <target>` loads a fresh shell mission and, for each shape, adds the same
number of ground units, settles 20 s, counts frames for 45 s, and removes them. 1 sample per
shape. Sweep 1 used 400 units; sweep 2 used 1,200 (`--units 1200`).

The DCS CPU column was empty in sweep 1. `window_samples` applied the container's UTC offset to
the window stamps, but DCS under Wine stamps `dcs.log` in UTC, so no sample fell in any
window. It now falls back to offset 0 when the container's offset finds no samples (a test
covers it). Sweep 2 has the column.

| Shape | Groups at 1,200 | Mean fps (400 units) | Mean fps (1,200) | p5 fps (1,200) | DCS CPU % (1,200) | Moving (400, 1,200) |
|---|---|---|---|---|---|---|
| halted | 120 | 112.7 | 113.2 | 112 | 43 | 0.00, 0.00 |
| offroad-few | 120 | 110.8 | 111.7 | 111 | 86 | 1.00, 1.00 |
| offroad-dense | 120 | 110.8 | 104.7 | 80 | 405 | 1.00, 0.99 |
| onroad | 120 | 110.9 | 110.5 | 107 | 94 | 0.78, 0.88 |
| formation-off | 300 | 111.4 | 111.3 | 110 | 90 | 1.00, 1.00 |
| formation-line | 300 | 111.6 | 111.6 | 110 | 89 | 1.00, 1.00 |
| formation-column | 300 | 112.3 | 111.3 | 110 | 89 | 1.00, 1.00 |
| formation-diamond | 300 | 111.3 | 110.9 | 108 | 91 | 1.00, 1.00 |
| size-1 | 1,200 | 113.5 | 58.6 | 48 | 99 | 1.00, 1.00 |
| size-4 | 300 | 111.5 | 111.1 | 110 | 90 | 1.00, 1.00 |
| size-10 | 120 | 110.6 | 111.2 | 108 | 88 | 1.00, 1.00 |
| held-order | 120 | 110.6 | 111.2 | 110 | 89 | 1.00, 1.00 |
| held-zero | 120 | 111.8 | 112.1 | 111 | 44 | 0.00, 0.00 |

At 400 units every shape lies within 110.6 to 113.5 fps. The spread is about one frame step
(resolution 1 fps, n = 1), so no cost difference shows. At 1,200 units the shapes differ.
Dense waypoints (81 per route) cost the most among routed shapes: 405 % of a core and a p5
of 80 fps. One unit per group costs frame rate: 58.6 fps, p5 48, at 99 % CPU. The 2-waypoint
off-road route costs 86 % against 43 % for halted units and keeps 111.7 fps. Formation
settings and group sizes of 4 to 10 differ by 5 % CPU or less (88 to 91). A held order adds
nothing visible. A formation name per group is not required.

Decision: the route compiler emits the author's waypoints, off road, and does not add
waypoints. Authors are told that dense waypoints cost the most DCS CPU and that single-unit
groups cost frame rate. On-road groups did not all move: 22 % did not move at 400 units and
12 % did not move at 1,200 (the road route did not take for those). That is an open item.

## Extra probes

The owner asked for three further probes. Their results are recorded; two feed later steps.

### Sound and picture

A mission packed with `cargo xtask miz <template> <out> --sound-picture` carries a generated
0.5 s 440 Hz WAV (`np-tone.wav`, key `ResKey_Action_901`) and an 8 by 8 PNG
(`np-picture.png`, key `ResKey_Action_902`), at `l10n/DEFAULT/<file>`, with a `mapResource`
entry for each.

ED-CODE: `mapResource` maps a key to a file name, and the file lives at `l10n/DEFAULT/<name>`
(`Scripts/dictionary.lua`). A rule's file field is serialized as
`getValueResourceByKey("<key>")` (`MissionEditor/modules/me_predicates.lua`). The picture
fields run: file, seconds, clear view, start delay, horizontal alignment (0 left, 1 center,
2 right), vertical alignment (0 top, 1 center, 2 bottom), size (1 to 100), size units (0 is
percent of original, 1 is percent of window) and picture effect (0 fast fade-in, 255 none)
(`MissionEditor/modules/me_trigrules.lua`). The group, unit, coalition and country forms put the
target field first. `seconds` 0 shows the picture until a stop. The `a_out_picture*` and
`a_out_sound*` actions are defined in no Lua file under the install; the runtime is native.

LIVE, sound (`server` state): `outSound`, `outSoundForCoalition`, `outSoundForCountry`,
`outSoundForGroup`, `outSoundForUnit` and `outSoundStop` all returned without error for the
bare file name, the `l10n/DEFAULT/` path, a name not in the mission, a relative path, an
absolute path, a number argument and an unknown group ID. `dcs.log` has no line for any of them
(LOG). A caller cannot use the server's call to check a file.

LIVE, picture (`hook` state, through `net.dostring_in('mission', ...)`): `a_out_picture`,
`_s`, `_g`, `_c`, `_u`, `a_out_picture_stop` and `getValueResourceByKey` exist in the mission
state. `getValueResourceByKey("ResKey_Action_902")` returns `l10n/DEFAULT/np-picture.png`; a
missing key returns the key. All calls returned without error, including a missing key or file
and swapped target and file arguments. Argument errors are not reported either.

The platform accepts `SOUND`, `PICTURE` and `PICTURE_STOP`. Whether a client hears the sound
or sees the picture, for which file form, and which argument form addresses the right target
is UNVERIFIED. The test needs a client and waits for the owner.

### Trigger zones

Recorded. Zones are not part of the platform.

ED-CODE: the generated-task Lua under `Scripts/GeneratedTasks/modules/` builds a zone table
(x, y, radius, color, properties, hidden, name, linked object) and calls
`trigger.misc.addZone`. It uses the returned `zoneId` in a trigger added with
`trigger.misc.addTrigger`. No Lua file under the install defines a remove or change call or
reads `S_EVENT_TRIGGER_ZONE`.

LIVE: `addZone` takes one table. The Mission Editor form returns `{name, zoneId}`, and the
minimal form (name, x, y, radius) also works. The positional forms fail with "Parameter #1
(zone data) missed". Zone `x` and `y` are map x (north) and z (east). `getZone` returns
`{point, radius}` for a name and nil for an unknown name. `trigger.misc` has only
`addTrigger`, `addZone`, `getUserFlag` and `getZone`: no remove, set or update. A second
`addZone` under an existing name creates a second zone with a renamed suffix (`np-z1` became
`np-z11`) and leaves the first one unchanged, so a zone cannot be changed or replaced. Zones
persisted for the run and through a pause. After `cargo xtask load` of the same mission,
`getZone` returned nil for all of them.

Run 1 never settled the event: the unit was still 247 m from the 150 m zone when the snippet
ended. Run 2 drove an M-113 through each of two 150 m zones, one with a trigger on
`c_part_of_group_in_zone` and one without. Exactly one `S_EVENT_TRIGGER_ZONE` (35) fired, at
model 16.8 s, for the zone that had the trigger. The zone alone gave no event, and nothing
fired on leaving. The trigger's flag read 1 from 22 s. The event's keys are `id`, `time`,
`initiator`, `initiator_data`, `target` and `target_data`. The initiator is the group
(`id_` 1000004, the group's `getID`), not the unit. The target is the zone (`id_` 1000001, the
`zoneId`). No unit, place or zone-name field exists. Not tested: a trigger whose action sets no
flag, and a zone made by the Mission Editor.

An event handler added by run 1 through `world.addEventHandler` in the `server` state was
still recording in run 2, after `cargo xtask load` reloaded the mission. Either the `server`
state survives a load or the load did not replace the mission; the cause is UNVERIFIED. It
matters for how the shim replaces its handler on a reload.

### Waypoint Script actions

Partial. A Script action is a `WrappedAction` with `id = 'Script'` and a `command` string in a
waypoint's combo task (ED-CODE, `MissionEditor/modules/me_mission.lua` and
`me_action_db.lua`: "The group will be passed as a single parameter to the function").

Run 1 did not complete. The first Script action fired at the group's spawn, 0.27 s after the
resume, and raised `attempt to call global 'np_wp' (a nil value)` (LOG): a global defined in
the `server` eval state does not exist in the state that runs the command. Then DCS stopped
answering. `dcs.log`, the agent log and the hook log have no line after that second. The
process stayed alive with its ports open and its CPU near 0.4 %, and the hook took no request.
The cause is unknown; no restart was made. No pause, re-route, per-unit, event or cost result
came from that run.

Run 2 used self-contained commands wrapped in `pcall` and kept DCS answering. LIVE (LOG,
`env.info` lines):

- The command at waypoint 1, the start, fires once per group, not once per unit. A group of
  4 logged one line. It fires just after the spawn, before any movement.
- The argument is the Group object. `getName` and `getSize` worked.
- After a `setTask` re-route, the new route's first point fired once.
- The state that runs the command has `world`, `net` and `trigger` as tables and `a_do_script`
  nil, so it is not the `mission` state. A table the command made was not found from the
  `server` or `mission` state. Which state it is, is UNVERIFIED.
- Cost: 0.0 us on every line, below the 1 us clock resolution.

Not settled: points 2 and 3 (arrival or passing), pause behavior, old points after a
re-route, and events 44, 49 and 50. The simulation paused itself at model 4.4 s and 39.3 s
with no pause call from the snippet, so the units never reached point 2 in the first part.
The snippet resumed once only at the start. It needs to resume whenever the simulation is not
in its own deliberate pause. `E.wait(..., 'running')` does that, but it also resumes during
the deliberate pause (`lua/dev/eval-hook.lua`, `lua/dev/eval-prelude.lua`). Controller Lua on
waypoints goes to S16.

## Open items

Each item names the step it goes to where one is known.

- E8 attack is inconclusive: the control group never fired in 2 runs. S18's part of E8
  repeats it with a longer window and several shooters (see E8 attack above).
- `AttackUnit` on a unit and on a static is UNVERIFIED. S18's part of E8 checks it.
- `GROUP_SPAWN` replace while the simulation runs is PRIOR; only the paused case is LIVE.
  S18's restore checks it.
- `AIRBASE_OWNER` with `autoCapture(false)` plus `setCoalition` is PRIOR. S18's restore checks
  it.
- Flares and chaff cannot be read from `getAmmo`. S18 finds a reader for countermeasures or
  records that capture cannot restore them.
- E14 offsets differ per slot, so restoring ground ammunition needs a per-type map. S18.
- Integer-like string keys in DCS tables are UNVERIFIED. S18 checks the tables it restores.
- The compiled Mission task table (route `GROUP_ORDER`) is UNVERIFIED live: E37 used
  spawn-time routes. A live `GROUP_ORDER` route check belongs to S18's restore.
- `DAMAGE` to 0 removes ground units and ships without a death event. The reconciler reads
  them as removed or missing, not dead. A unit killed but not yet checked, then destroyed,
  reads `DESTROYED`, not `DEAD`.
- E21b: the event initiator part is not covered. It needs a rerun with the simulation resumed.
- On-road routes: 12 to 22 % of on-road groups did not move in E37 (400 and 1,200 units). The
  compiler's On Road route needs a check or a fallback in a later step.
- Packing campaign sound and picture files into the shell mission goes to the shell writer.
- Client checks wait for the owner on a Windows client: whether `SOUND` is heard (coalition and
  group forms), whether `PICTURE` shows, `PICTURE_STOP` clears and how `clear_view` behaves,
  and what a client shows after `DAMAGE` to 0 on a ground unit.
- An event handler added by an eval snippet survived `cargo xtask load` (see Trigger zones).
  The cause is UNVERIFIED and matters for the shim's handler replacement on reload. S16
  checks it before it builds the `CHAT` and `MISSION_LOAD` handlers on the reload path.
- Controller Lua on waypoints is S16's: raw Lua gated, always wrapped in `pcall` by the
  compiler, after an experiment that identifies the Script environment and the timing of
  intermediate points.
- Waypoint Script run 1 left DCS unresponsive after a Script error; the cause is unknown.
- E6: with an unlimited warehouse the set is ignored, and a limited warehouse's `aircrafts`
  table showed 0 aircraft in the inventory read. Neither is diagnosed.
