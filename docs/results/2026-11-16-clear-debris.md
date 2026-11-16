# Clearing wrecks and debris

| | |
|---|---|
| Experiment | E-junk: what `world.removeJunk(volume)` does in the mission scripting state |
| Date | 2026-11-16 |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, `cargo xtask target check`) |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker, no player connected. Runs 1 and 2 on a mission on the Afghanistan map; runs 3 and 4 on a fresh shell mission on the Caucasus map. Host rehearsal: macOS on arm64. |
| Script | `cargo xtask eval <target> server lua/experiments/junk/junk.lua --timeout 500` (run 1); the same with `junk2.lua` (run 2); `cargo xtask commands <target> --part debris --groups 20 --units 4` (run 3, "The command"); `cargo xtask sampling <target> --part mirror` (run 4, "Overhead") |
| Rehearsal | `lua/tests/junk_experiment.lua` (runs 1 and 2), `lua/tests/world_exit.lua debris` (run 3), run by `mise run check` |
| Bundle | `bundles/junk-run1.json`, `bundles/junk-run2.json`, `bundles/commands-debris-<target>-<stamp>/`, `bundles/sampling-<target>-<stamp>/` (not committed: they hold server data) |
| Result | PASS for clearing a volume: every rule held in run 1. No per-wreck form: neither way passed its rule in run 2. The command passed in run 3. In run 4 the owner's overhead targets held; the tool's verdict is FAIL on two checks that main fails too. |

All figures are LIVE, from run 1 unless the section names another run.

## Rule

Written before the first run. The platform builds a clear-debris command when, in one
run:

1. a sphere clears every ground wreck inside it;
2. no call removes a live unit or a live static anywhere, the whole-map calls included;
3. a dead unit outside a local volume is left;
4. one call, or several, clears the wrecks of the whole map.

A live object removed fails the rule and stops the step. Everything else is recorded:
the other shapes, the return value, statics, aircraft, scenery, craters, burning wrecks
and their death events, and the cost of each call.

## ED's files

No Lua file in the server install calls or names `removeJunk` or `VolumeType`, and the
string `removeJunk` is in no file under `bin/`, `bin-mt/` or `Mods/tech` (ED-CODE,
searched read-only). `world.removeJunk` and `world.VolumeType` (`SEGMENT` 0, `BOX` 1,
`SPHERE` 2, `PYRAMID` 3) are in the mission scripting state's listing (LIVE, PRIOR
listing, and run 1). What the call takes and does is known only from the runs below.

## What the snippet does

It lays out clusters 1,500 m apart, 4 to 14 km from the mission's first airbase, all on
land. Ground clusters are M-113 groups, killed with a scripted explosion of power 30,
6 m beside each unit, which leaves a burning wreck. Statics are two `FARP Fuel Depot`,
killed with power 1,000. A UH-1H hovering 60 m up and an F-15C flying 300 m up are killed
with power 1,000. One scenery object near the airbase is hit with power 500, and open
ground with power 500 for a crater. Live controls: one M-113 inside the sphere cluster,
one M-113 1.5 km away, one fuel depot 300 m from the dead ones. A dead M-113 400 m from
the sphere cluster is the outside control.

The kills come at 3 s of model time, the burning cluster's at 60 s. Each cluster is
cleared at 100 s with one volume; the burning cluster at 63 s, while it burns. The whole
map is cleared at 110 s, and again at 116 s with a sphere, a box and a larger sphere.
Each object is read before and after: `Object.isExist`, `Unit.isDead`, `Unit.getLife`,
`Object.getPoint`, `getByName`; `StaticObject.getLife` for a static. Death events naming
the snippet's objects are logged.

## Results

### Shapes and the return value

Each call returned a number: how many junk objects it removed. The count is larger than
the number of wrecks: craters and debris count too.

| Call | Volume | Wrecks inside | Returned | Cost (us) |
|---|---|---|---|---|
| empty_small | sphere, 50 m, nothing dead anywhere | 0 | 0 | 6 |
| empty_map | sphere, 2,000 km, nothing dead anywhere | 0 | 0 | 4 |
| burning_sphere | sphere, 150 m, 2 burning wrecks | 2 burning | 2 | 32 |
| many_one | sphere, 5 m, on one wreck in a field of 100 | 1 | 5 | 48 |
| sphere | sphere, 150 m | 4 (1 still burning) | 10 | 55 |
| box | box, 200 by 100 m, 600 m tall | 4 | 12 | 61 |
| segment | segment, 220 m along the row | 2 | 6 | 38 |
| pyramid | pyramid from 400 m up, pointing down | 2 | 6 | 43 |
| static | sphere, 100 m, 2 dead statics | 2 statics | 2 | 17 |
| heli | sphere, 600 m round the UH-1H's kill point | 1 aircraft | 4 | 24 |
| plane | sphere, 3 km round the F-15C's kill point | 1 aircraft | 2 | 16 |
| scenery | sphere, 60 m on the hit scenery object | 1 scenery object | 1 | 11 |
| crater | sphere, 50 m on the open-ground explosion | none | 1 | 10 |
| map_sphere | sphere, 2,000 km round (0, 0, 0) | 105 cold, 2 burning | 312 | 1,350 |
| map_sphere_again | the same, 6 s later | 2 that exploded since | 4 | 41 |
| map_box | box, x and z from -1,000 to 1,000 km | 0 | 0 | 5 |
| map_sphere_1e7 | sphere, 10,000 km | 0 | 0 | 4 |

All four shapes work. The 2,000 km sphere cleared every cold wreck of the run in one call.
A 2,000 km box and a 10,000 km sphere were accepted, but only on a map already cleared, so
they show no removal; the largest box that removed junk here was 200 m.

The volume's edge is not exact to a unit's position: the 5 m sphere on one wreck removed
two wrecks whose units had stood 25 m apart.

### What is removed and what stays

| Object | After the clear |
|---|---|
| Live unit, inside and outside the volumes | Untouched, in every call (rule 2) |
| Live static | Untouched, in every call (rule 2) |
| Ground wreck that has stopped burning | Removed: `isDead`, `getLife` and `getPoint` raise from then on, as after `Object.destroy` (rule 1) |
| Ground wreck still burning | Left. It reads dead with life 1, then explodes, sends its death events (`S_EVENT_UNIT_LOST`, `S_EVENT_DEAD`), and a later clear removes it |
| Dead unit outside the volume | Left (rule 3) |
| Dead static | Its object stays: `StaticObject.getLife` reads 0, `getByName` finds it, `world.searchObjects` lists it. The call returned 2 there; what it removed (debris, craters) is not readable from scripting |
| Aircraft wreck | The calls returned 4 and 2. `world.searchObjects` lists no aircraft wreck before or after, so what was removed is not readable from scripting (UNVERIFIED) |
| Destroyed scenery | Stays destroyed: `isExist` false and life 0 before and after |
| Crater | The call on open ground returned 1, the only junk there, so a crater counts as junk (inferred). The ground height is unchanged before and after, so a crater is not a terrain change |

The burning cluster: cleared at 63 s while burning (count 2), its wrecks still read dead
with life 1, they exploded at 110.6 and 112.2 s with their death events, and the call at
116 s removed them (count 4). The same held for one wreck of the sphere cluster that was
still burning at 100 s. So `world.removeJunk` cancels no death event, unlike
`Object.destroy` on a burning wreck (spec/records.md). Death events came 10 to 103 s
after the kills in this run, longer than the 84 s E38 saw; spec/records.md carries 103 s.

### Cost

A call on an empty volume costs 4 to 6 us at any size. A call that removes objects costs
about 4 us per object removed: 1,350 us for 312 objects. Cost follows what a call removes,
not the volume's size.

### On a client

Whether the removal reaches a connected client, and what a client sees removed for a
static, an aircraft or scenery, is open: it needs the Windows PC as a client.

## Per wreck

Run 2 (`lua/experiments/junk/junk2.lua`, bundle `bundles/junk-run2.json`, same server,
build, platform and date) asks whether one dead object's wreck can be removed on its
own, so a controller can clear it a set time after the death.

Rule, written before the run: removal by `Object.destroy` works per wreck when a
`world.removeJunk` sphere on a destroyed wreck counts less than on a control that was
not destroyed, at 300 s; removal by a small sphere works when one radius removes its
wreck and no neighbor.

Subjects, killed at 3 s as in run 1: M-113s, `FARP Fuel Depot` statics and UH-1H
helicopters, each destroyed 1 s or 300 s after the kill, or left as a control; and rows
of four dead M-113s, a target and neighbors 10, 30 and 70 m from it. At 306 s a 30 m
sphere (600 m for a helicopter) is cleared on each subject, and a 1, 3 or 8 m sphere on
each row's target.

| Subject | `Object.destroy` | Reads after | Sphere count afterward (control) |
|---|---|---|---|
| Ground unit, 1 s, still burning | accepted | raise; its death events never came | 1 (3) |
| Ground unit, 300 s, wreck cold | accepted | raise | 3 (3) |
| Static, 1 s and 300 s | accepted | raise; `getByName` and `world.searchObjects` no longer find it | 1 (1) |
| Helicopter, 1 s and 300 s | raises `Object doesn't exist` | raise before and after | 4 (4) |

| Sphere on the target | Returned | Removed |
|---|---|---|
| 1 m | 6 | the target and the neighbor 10 m away |
| 3 m | 6 | the target and the neighbor 10 m away |
| 8 m | 7 | the target and the neighbors 10 and 30 m away |

Result: neither works per wreck, by the rule. `Object.destroy` on a cold ground wreck
leaves the junk DCS clears afterward: the count matches the control's. On a burning
wreck it removes the junk but cancels the death events the platform needs
(spec/records.md). On an aircraft it raises. On a dead static it removes the static's
object; whether the ruin a client sees goes too is open. A sphere of any radius tried,
1 m included, also removed a dead neighbor 10 m away. So the platform offers no
per-wreck form. A controller that wants a wreck gone a set time after a death sends a
small sphere at the wreck's position then, and accepts that wrecks within about 30 m go
too.

## The command

Run 3 checks `CLEAR_DEBRIS` through the platform's command path: `cargo xtask commands
<target> --part debris --groups 20 --units 4` (`crates/xtask/src/commands/world.rs`, the
probe's `debris` scenario in `crates/fake-agent/src/world_exit.rs`), rehearsed by
`lua/tests/world_exit.lua debris`. Agent build `4388741e492cf3eb`.

The probe spawns 20 groups of four M-113 in a grid 150 m apart and a control group
1,500 m away. The tool kills the grid's 80 units with explosions, keeping their handles,
and runs 120 s of model time. The probe then sends a sphere on the first group, a box
round the second, and a clear of the whole map: 256 boxes 250 km square from 10 km below
sea level to 100 km up. After the probe's verdict the tool reads DCS.

| Clear | Outcome | Removed | Cost (us) |
|---|---|---|---|
| Sphere, 80 m, one group | `APPLIED` | 8 | 95 |
| Box, one group | `APPLIED` | 12 | 78 |
| Whole map, 256 boxes | `APPLIED` | 217 | 6,377 in all |

Result: PASS. The kill found all 80 units. Each clear removed at least its wrecks' number
of junk objects, the whole map at least the other 18 groups' 72. After the clears every
killed unit's reads raise, so no wreck is left, and all 4 control units exist alive. The
256 boxes took 4 frames under the 2 ms commanded-work budget: a box cost 20 us at the
median, 37 us at p99 and 981 us at most, the largest frame 2,219 us, one frame over the
budget and attributed to its last command, and the agent's over-budget counter moved by
0, which the results fit. So 250 km boxes remove junk, and a clear of the whole map is
spread over frames by the budget.

## Overhead

The agent change is a decode arm and a handler for a new opcode; nothing on the frame
pump changed. Run 4 measures it: `cargo xtask sampling <target> --part mirror` on agent
build `4388741e492cf3eb`, same server, platform and date, against S15's figures on the
same part (docs/results/2026-11-15-s15-world-commands.md, run 2). The mirror run sends no
commands.

| Measure | S15 run 2 | Run 4 |
|---|---|---|
| Mean share from the 800 spawn | 0.204 % | 0.210 % |
| Mean share at 2,000, settled | 0.190 % | 0.198 % |
| Largest 10 s window | 0.354 % | 0.291 % |
| Frames over 250 us | 93 of 39,857 (0.23 %) | 105 of 41,293 (0.25 %) |
| Median position interval, stall-free | 0.998 s | 1.0 s |

The owner's targets hold. The tool's verdict is FAIL on two checks at k = 1, the vitals
band (4.11 s as a rate) and pumps a second (5.52), which main fails too
(docs/results/2026-11-15-maintenance-no-row-owed.md). DCS froze once at 2,000 objects
(p5 0.8 fps in that step); the stall-free windows passed. Bundle
`bundles/sampling-<target>-<stamp>/`.

## Open

- What a client sees.
- What is removed around a dead static and an aircraft: the counts are LIVE, the objects
  are not readable from scripting.
- The cost of one call with thousands of junk objects is extrapolated from 312.
