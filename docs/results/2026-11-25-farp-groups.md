# Heliport groups: one airbase from one static group

`coalition.addGroup(country, -1, group)` joins a static group's `Heliports` units into
one airbase under the first heliport's name, with every heliport's parking and one empty
warehouse. Every other unit of the group is a static of its own. The platform spawns a
heliport group this way, through `PutStatic` and `PutFromStaticTemplate`
([ADR NNNN](../adr/NNNN-heliport-groups-spawn-as-one-airbase.md); spec/world.md,
"Heliport groups").

## Run 1: what DCS makes of a static group

| | |
|---|---|
| DCS | 2.9.31.29434 dedicated server, `dcs_server.release` |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Build | Agent `365d33e8c837e512`, `--dev` (the eval channel) |
| Date | 2026-11-25, local time |
| Mission | The server's own, on the Afghanistan map, paused at model time 0, no player but the server |
| Scripts | `lua/experiments/farp-groups/`, each with `cargo xtask eval <server> <state> FILE`, in the order `spawn.lua`, `slots.lua` (hook), `replace.lua`, `remove.lua`, `run.lua` (hook), `settle.lua` (hook), `after.lua`, `cleanup.lua`, then `lua/experiments/farp-fixes/reload.lua` (hook) |
| Rehearsal | `lua/tests/farp_groups_experiments.lua` runs every snippet under the strict mock, which models what the run read |

`spawn.lua` added 17 static groups, 3 km and more east of Herat, in rows 600 m apart,
units 200 m apart. Every place was land. All reads below are LIVE.

### What a group makes

| Group (`addGroup(USA, -1, ...)`) | Returned | Airbases | Parking | What each unit name finds (`StaticObject.getByName`) |
|---|---|---|---|---|
| 3 `FARP` | The airbase, category 4, first name | 1 | 12 | The airbase, under all three |
| 10 `FARP` | The airbase | 1 | 40 | The airbase, under all ten |
| 2 `SINGLE_HELIPAD` | The airbase | 1 | 2 | The airbase, under both |
| 3 `SINGLE_HELIPAD` | The airbase | 1 | 3 | The airbase, under all three |
| `FARP`, `SINGLE_HELIPAD` | The airbase, type `FARP` | 1 | 5 | The airbase, under both |
| 2 `Hangar A` (`Fortifications`) | The second static, category 3 | 0 | none | Its own static each; nothing shared |
| `FARP`, `Hangar A` | The airbase | 1 | 4 | The airbase; the hangar is its own static |
| 2 `FARP`, 2 `Hangar A` | The airbase | 1 | 8 | The airbase under both FARP names; each hangar its own static |
| `FARP`, `M-113` (no category) | The airbase | 1 | 4 | The airbase; the M-113 is a static, category 3, that `Unit.getByName` also finds |

- `Airbase.getByName` finds the airbase under the first heliport's name only, and
  `Group.getByName` finds nothing under the group's name.
- `world.searchObjects` near a further heliport's place finds no object: the airbase
  stands at the first heliport's place.
- A `unitId` on the first unit is honored: `getID()` and the hook's FARP key were
  900201, as given. Without one, `getID()` is a mission ID from 1000000 up, not the
  object ID.
- The stock at spawn is empty: `getInventory()` lists no aircraft and no weapon, and
  each liquid reads 0.
- The hook lists each airbase once in `DCS.getFarpsAndCarriersMissionData().farps`.
  `DCS.getDynamicSpawnSettings` reads `dynamicSpawnAvailable` true for the group with
  `dynamicSpawn = true` on its first unit only, and false for the group with it on the
  group only: DCS reads the key from the first unit.
- No group spawn raised.

### Removal

`remove.lua` called `destroy()` on `StaticObject.getByName(first name)` of the 3-, 10-
and 3-helipad groups, the `FARP`-helipad group, and every unit of the mixed groups,
paused. Every object still existed in that call. After 15 s of simulation (`run.lua`,
`settle.lua`), each group's airbase was gone: every unit name gave a handle with
`isExist` false, `Airbase.getByName` gave nil, and `world.getAirbases` still listed it,
as for a single FARP (docs/results/2026-11-25-farp-fixes.md). The hangars and the M-113
were gone by name. One `destroy()` on the first name removes the whole group.

### Replace by name

Each try spawned under the names of a live group. `replace.lua` ran its three paused;
`run.lua` ran the same three while the mission ran.

| Try | Paused | Running |
|---|---|---|
| 3 `FARP` group, then a 2 `FARP` group under the same names | The old airbase is gone at once (`isExist` false); the new one holds names 1 and 2 with 8 spots; name 3 gives the old handle, not existing | The same |
| 2 `FARP` group, then one `FARP` by `addStaticObject` under name 1 | The old pair stays and exists; name 1 finds the new FARP (4 spots), name 2 the old pair | The same, also after the simulation ran |
| One `FARP` by `addStaticObject`, then a 2 `FARP` group | The old FARP is gone at once; the pair holds both names | The same |

A group spawn over a name replaces what holds it, also while paused. A single static
over a group's first name does not remove the group. The platform never does this: a
static's spec, and so its unit count, does not change.

### A further heliport hit

`trigger.action.explosion` of power 3000 at the second `FARP`'s place of a 2-`FARP`
group, while the mission ran: after 12 s of simulation the airbase existed, with life
3600 and 8 spots, and no event named it. Nothing stands at a further heliport's place to
hit. What ends a group's airbase in play is not read here.

### The Mission Editor

The static panel enables its unit count, 1 to 99, only for a static of category
`Heliports`, outside a planner mission, and each unit it adds copies the first unit's
type (ED-CODE, `MissionEditor/modules/me_static.lua`, `updateNumUnits` and
`onSpinUnitOf`). So the editor saves a multi-unit static group only of heliports of one
type, and never one of other statics.

### Afterwards

`cleanup.lua` destroyed 27 objects still standing, and `reload.lua` loaded the mission
again, paused at model time 0: 30 airbases, none of the run's names found.

## Run 2, part 1: one-unit replaces

Run 2 had two parts under one grant. Part 1: the same server, DCS build 2.9.31.29434 and
Wine 10.0 as run 1, dev agent `365d33e8c837e512`, 2026-11-25, the server's own mission
paused at model time 0. `one.lua` added a 2-`FARP` group and a one-`FARP` group with
`addGroup(-1)` and replaced each under its names, paused: the pair by a one-unit group,
the single by a pair. `one-run.lua` (hook) let the mission run 12 s and did the same
two replaces on two more groups 2 s in; `one-after.lua` read every name, and
`one-clean.lua` and `lua/experiments/farp-fixes/reload.lua` removed them. All LIVE.

| Replace | Paused | Running |
|---|---|---|
| A 2-`FARP` group by a one-`FARP` group | The old airbase is gone at once (`isExist` false); the new one holds name 1 with 4 spots; name 2 gives the old handle, not existing | The same |
| A one-`FARP` group by a 2-`FARP` group | The old airbase is gone at once; the pair holds both names with 8 spots | The same |

So a group spawn of any size replaces what holds its first name at once. With every
heliport sent as a group (spec/commands.md, "Layout: STATIC_SPAWN"), a spawn under a
static's name leaves no older group of it in DCS.

## Run 2, part 2: through `PutStatic` and a template

`cargo xtask sidecar <name> --part template-heliport --file FILE`
(`crates/xtask/src/shell_live/template_heliport.rs`) on the same server, 2026-11-25,
from this commit's tree before review: agent build `7db705ee450d5426`, sidecar binary
sha256 `17354bf04ad3fbb3d256c751392e8fad3c367d40880d8f5c19c42f39285406ae`, in a fresh
shell mission on Caucasus that the part keeps running. FILE was
`lua/experiments/farp-groups/run2-template.stm`, a static template written for the run
in the form the editor saves: a static group of three `FARP`s 200 m apart, each with
frequency `"129.5"`, modulation 0 and callsign 2; a group of two `SINGLE_HELIPAD`s; and
a `Hangar A` (`Fortifications`).

The run made FILE in place: the call answered three statics, one per static group. It
then made the `FARP` group again with `PutStatic`, north of it, with its own radio (the
twin), four more like the twin, and a group of ten `FARP`s, each clear of the others.
For each heliport group it read the airbase under the static's name, the parking, the
object every further unit's name finds, and how often `world.getAirbases` lists it. It
stocked and owned each file heliport and the twin, removed everything, and read every
name again once the simulation ran. All LIVE.

| Check | Result |
|---|---|
| The file's 3-`FARP` group, the twin and the four extra groups | One airbase each, 12 spots, listed once; every further name finds the airbase, and `Airbase.getByName` finds nothing under it |
| The file's 2-helipad group | One airbase, 2 spots, listed once; the further name finds it |
| The 10-`FARP` group | One airbase, 40 spots, listed once; all nine further names find it |
| The radio of the file's two group airbases and the twin's | As each first unit gave it: 129.5 MHz AM Dallas, 127.5 MHz AM London, 131.25 MHz FM Moscow. The twin's further units carried the file's 129.5 MHz AM Dallas, so DCS took the first unit's radio for the group |
| Each entity in the World | `LIVE` |
| `SetWarehouseStock` and `SetAirbaseOwner` under each static's name | DCS read 5,000 kg of jet fuel and coalition 1 at each |
| `RemoveEntity` of every entity | Every heliport and its airbase gone; each airbase entry a tombstone; all 22 further names give nothing or a handle with `isExist` false, and no airbase |

The agent's sampling report, each over its 10 s window:

| | Airbases | Sim-thread share | Longest frame (µs) | Frames over 250 µs | Births (rows, µs) | Enumeration (rows, µs) | Warehouses (rows, µs) |
|---|---|---|---|---|---|---|---|
| No FARP | 21 | 0.060 % | 238.8 | 0 | 0, 0 | 18, 77.4 | 2, 121.8 |
| Seven heliport groups (20 heliports) | 28 | 0.074 % | 197.1 | 0 | 4, 145.3 | 0, 0 | 4, 219.6 |

A heliport group costs the sim thread what a single FARP costs: one airbase entry, one
birth, one enumeration item (spec/world.md, "Heliport groups"). The window with groups
held four births and no enumeration; the per-group items are the design's, read from
the code, not separated in this report. The share with seven
groups is in line with the farp-fixes run's 0.089 % with seven single FARPs
(docs/results/2026-11-25-farp-fixes.md).

Verdict: PASS (LIVE). The rule: every heliport group is one airbase with 4 spots per
`FARP`, listed once, every further name finds it; every entity `LIVE`; the stock and
owner land; removal leaves every name gone and a tombstone per airbase.

## Run 3: the final tree

`cargo xtask sidecar <name> --part template-heliport --file FILE` on the same server,
2026-11-25, from this commit's tree after review: agent build `f6e3e7d9a1fad557`,
sidecar binary sha256 `2ae3d5af653893cf5da0adeceeaf0bf25803b3559bc5a92d5dd3f6c198844936`.
FILE was `lua/experiments/farp-groups/run3-template.stm`: run 2's file with a radio of
its own on each `FARP` of the group (129.5 MHz AM Dallas, 130 MHz AM Paris, 130.5 MHz AM
Moscow) and a group of one `FARP` (128 MHz FM Paris). The part judges a single heliport
as a one-unit group: every live heliport went through `addGroup`. All LIVE.

| Check | Result |
|---|---|
| The call's answer | Four statics, one per static group |
| The 3-`FARP` group, the twin and the four extra groups | One airbase each, 12 spots, listed once; every further name finds the airbase |
| The 2-helipad group | One airbase, 2 spots, listed once |
| The single `FARP`, sent as a one-unit group | One airbase, 4 spots, listed once; `LIVE`; stocked, owned and removed as the groups |
| The 10-`FARP` group | One airbase, 40 spots, listed once |
| The group's radio, its units' radios all different | 129.5 MHz AM Dallas: the first unit's |
| The single `FARP`'s radio | 128 MHz FM Paris, as given |
| Stock and owner under each static's name | 5,000 kg of jet fuel and coalition 1 at each of the four |
| `RemoveEntity` of every entity | Every heliport and its airbase gone, a tombstone each; all 22 further names give nothing or a handle with `isExist` false |

| | Airbases | Sim-thread share | Longest frame (µs) | Frames over 250 µs | Births (rows, µs) | Enumeration (rows, µs) | Warehouses (rows, µs) |
|---|---|---|---|---|---|---|---|
| No FARP | 21 | 0.061 % | 222.8 | 0 | 0, 0 | 18, 79.6 | 2, 104 |
| Eight heliport airbases (seven groups and the single `FARP`, 21 heliports) | 29 | 0.088 % | 224.5 | 0 | 4, 111.5 | 23, 203.8 | 4, 227.3 |

Verdict: PASS (LIVE), by run 2's rule, with the single `FARP` judged as a one-unit group.

## Open

- A size change under one name has no API-level check: `PutStatic` refuses to replace
  a static's spec, so no call spawns a group of another size under a static's names.
  The one-unit replaces are read by eval ("Run 2, part 1: one-unit replaces"). A call
  that replaces a static's spec, when one is built, must keep each further unit's ID by
  its index, take new IDs for new indexes and drop the IDs of missing ones (D275).
- A group spawn that returns no airbase, which the handler refuses and destroys, is
  host-tested only (`lua/tests/dispatch.lua`, with a stubbed `coalition.addGroup`): the
  platform sends the group form only for `Heliports` units, and DCS returned the
  airbase for every such group in runs 1 and 2, so no live run provokes it.
- What ends a heliport group's airbase in play, and the ruin a dead group respawns as
  (ADR 0026).
- Whether a player's dynamic spawn uses a further heliport's pads, and whether a client
  draws the further heliports. Needs a client.
- Which table DCS reads `allowHotStart`, `dynamicCargo` and `hidden` from.

## Runs and host checks

Live runs used: 3 of 3 (run 2 in two parts under one grant). Each restored the server's
own mission, paused at model time 0, its build, `Config/` and `Logs/`.

macOS (aarch64), this commit's tree, 2026-11-25.

| Command | Result |
|---|---|
| `mise run check` | Passes: 1,558 tests passed and 0 failed, and every other step |
| `mise run dll` | Passes: the cross-built DLL's 59 entry points own nothing |
| `lua lua/tests/farp_groups_experiments.lua` | Passes under the strict mock: every snippet of the three runs |
