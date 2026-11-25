# FARP pairs: two FARPs in one static group

A static group of two `FARP` units, in a mission file or added by
`coalition.addGroup(country, -1, group)`, is one airbase. The airbase takes the first
unit's name and the mission's unit ID, owns the parking of both FARPs, and has one
warehouse. That warehouse holds the stock of the first unit's `warehouses` entry; an
entry for the second unit is loaded, and no read here shows it used. Both unit names
find the same object through `StaticObject.getByName`. The hook data that ED's slot
dialog lists FARPs from has one entry for the pair (LIVE reads; the dialog's use of it
is ED-CODE). A script can make such a pair at run time with `coalition.addGroup`;
`coalition.addStaticObject` cannot.

## Setup

| | |
|---|---|
| DCS | 2.9.31.29434 dedicated server, `dcs_server.release` |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Build | Agent `365d33e8c837e512`, `--dev` (the eval channel and the mission loader) |
| Date | 2026-11-24, local time |
| Missions | Built from `tests/fixtures/templates/caucasus.miz` with `cargo xtask miz <template> <out> --heliport-group -356760,619193,2,first` and `...,2,each`, loaded with `cargo xtask load <server> <miz>` |
| Scripts | `lua/experiments/farp-pairs/`: `read.lua`, `parking.lua`, `second.lua` and `make.lua` in the server state, `slots.lua` in the hook state, through `cargo xtask eval <server> <state> <file>`; `run.sh` fills in `make.lua`'s try and `parking.lua`'s airbase name |
| Order | `first` mission: `read.lua`, `slots.lua`, `parking.lua np-hg-1`. `each` mission: `read.lua`, `slots.lua`, `parking.lua np-hg-1`, `second.lua`, `make.lua` with `group-static`, `static-units` and `same-group`, `parking.lua np-group-static-1`, `slots.lua`, `make.lua group-ground`, `make.lua group-dynamic`, `slots.lua` |
| Rehearsal | `lua/tests/farp_pairs_experiments.lua` runs every snippet under the strict mock. The mock makes one FARP airbase per `addStaticObject` and none from a static group, so it checks that each snippet finishes, records its reads and puts back what it sets, not what DCS does. `read.lua`, `slots.lua` and `make.lua` were rehearsed before the live run; `parking.lua`, `second.lua` and the `group-dynamic` try were written during it, after the first reads, and rehearsed after it |

`--heliport-group X,Y,UNITS,first|each` adds one blue (USA) static group `np-hg` of
`FARP` units `np-hg-1`, `np-hg-2`, ..., each 200 m north of the last, and limited
warehouse entries with dynamic spawn on: for the first unit only (`first`), as the
Mission Editor writes one, or for each unit (`each`). Unit `k`'s entry holds `10 * k`
tons of jet fuel and nothing else, so a read shows which entry an airbase took. The
test mission had group ID 1 and units 2 (`np-hg-1`) and 3 (`np-hg-2`).

## How the Mission Editor writes a heliport group

| Fact | Tag |
|---|---|
| Placing a unit of category `Heliport`, `Warehouse` or `GrassAirfield` calls `addWarehouse(group.units[1].unitId, coalition)`: the group's warehouse is keyed by its first unit (`MissionEditor/modules/me_mission.lua`, near line 6202) | ED-CODE |
| `addWarehouse` returns at once when the key has an entry, so adding a second unit to the group adds no entry (`me_mission.lua`, `addWarehouse`, near line 9672) | ED-CODE |
| Removing a unit calls `delWarehouse(unit.unitId)`, which drops that key's entry and the supplier links that name it (`me_mission.lua`, `remove_unit`, `delWarehouse`) | ED-CODE |
| A heliport unit carries `heliport_callsign_id`, `heliport_frequency` and `heliport_modulation` (`me_mission.lua`, near line 4465) | ED-CODE |
| The static panel reads and writes the warehouse of `vdata.group.units[1]` only (`MissionEditor/modules/me_static.lua`, near lines 787 and 823) | ED-CODE |

How a mission ends up with an entry for the second unit is not in this code.

## 1. A pair in the mission file, the first unit's entry only

| Read | Result | Tag |
|---|---|---|
| `world.getAirbases`, entries that are not airdromes | One: `np-hg-1`, category 1 (helipad), exists | LIVE |
| `Airbase.getByName('np-hg-1')` | An airbase: `getID()` 2 (the mission's unit ID), `getCategory()` 4, type `FARP`, blue, at the first unit's place, on land | LIVE |
| `Airbase.getByName('np-hg-2')`, `Airbase.getByName('np-hg')` | `nil` | LIVE |
| `StaticObject.getByName('np-hg-1')` and `('np-hg-2')` | The same object for both, class `Airbase`, the airbase's object ID | LIVE |
| `coalition.getStaticObjects`, all sides | Empty | LIVE |
| The airbase's `getParking()` | 8 spots, type 40. Four at offsets (-18, 4), (62, 4), (-58, 62), (22, 62) m from the airbase (north, east), the first FARP; four 200 m further north, at (182, 4), (262, 4), (142, 62), (222, 62), the second FARP | LIVE |
| `world.searchObjects` within 60 m of each unit's place, units, statics, bases and scenery | The airbase at the first unit's place; nothing at the second's | LIVE |
| `getWarehouse()` stock | Jet fuel 10,000 kg, the first unit's entry | LIVE |
| `env.warehouses.warehouses` | One entry, key 2, jet fuel 10, limited, dynamic spawn on | LIVE |
| Hook: `DCS.getFarpsAndCarriersMissionData().farps` | One entry, key 2: `np-hg-1`, blue, `FARP`, the first unit's place | LIVE |
| Hook: `DCS.getDynamicSpawnSettings(2, false)` | `dynamicSpawnAvailable` true, `allowHotSpawn` false | LIVE |
| Hook: `DCS.getAvailableSlots('blue')` | 0; the mission has no client slot | LIVE |

The stock question has one answer: there is one warehouse. The `read.lua` check that
sets stock at one pad and reads the other did not run, because `np-hg-2` has no
airbase of its own. `second.lua` (in the run of section 2) reached the warehouse
through `StaticObject.getByName('np-hg-2')`, set 7 Mk-82 there, and read 7 through
`Airbase.getByName('np-hg-1')`: the same stock (LIVE). `Warehouse.getByName` with
either unit name returned `nil` (LIVE).

ED's slot dialog lists a FARP for dynamic spawn from
`DCS.getFarpsAndCarriersMissionData().farps` when `DCS.getDynamicSpawnSettings(id,
false)` says it is available (ED-CODE,
`Scripts/UI/MultiplayerSelectRoleMap/MultiplayerSelectDynamicDialog.lua`,
`getDynamicsFarpsAndCarriers`). So a player sees one spawn point for the pair, under the
first unit's name (ED-CODE with the LIVE reads above). That a dynamic spawn there can
use the second FARP's pads is inferred from the parking list and UNVERIFIED: no client
was connected. Whether a client draws the second FARP's model is UNVERIFIED.

## 2. A pair with an entry for each unit

The same mission with entries for units 2 (10 t of jet fuel) and 3 (20 t), loaded in
the same DCS session.

| Read | Result | Tag |
|---|---|---|
| `env.warehouses.warehouses` | Two entries: key 2 (10 t) and key 3 (20 t) | LIVE |
| Airbases, names, statics, parking, objects near each place | As in section 1 | LIVE |
| The airbase's stock | Jet fuel 10,000 kg: the first unit's entry | LIVE |
| Hook: FARP list and dynamic-spawn settings | As in section 1: one FARP, key 2 | LIVE |

The second unit's entry is loaded and has no effect that any read here shows. No
scripting call reached a warehouse holding its 20 t.

## 3. A pair made by a script

Each try in `make.lua`, run in the section 2 mission, 400 m or more east of `np-hg`.
Every place was land. `make.lua` reads names, object IDs (`id_`) and jet fuel. The IDs
in the table are the keys of `DCS.getFarpsAndCarriersMissionData().farps`, read with
`slots.lua` after the tries; for the mission's pair that key is `getID()`, 2 (section
1). `getID()` of a script-made airbase was not read (UNVERIFIED). The parking row is
`parking.lua np-group-static-1`.

| Try | DCS made | Tag |
|---|---|---|
| `coalition.addGroup(country.id.USA, -1, { name, units = { FARP, FARP } })` | One airbase under the first unit's name, key 1000001 (no `unitId` given). It returns that airbase. `StaticObject.getByName` finds it under both unit names; `Airbase.getByName` and `Group.getByName` find nothing under the second unit's or the group's name. Its parking is the 8 spots of section 1, at the same offsets. Its jet fuel is 0 at spawn; other stock was not read | LIVE |
| The same with `dynamicSpawn = true` on each unit and on the group | As above, key 1000011; `DCS.getDynamicSpawnSettings(1000011, false)`, read with `slots.lua` after the try, says `dynamicSpawnAvailable` true. Which of the two keys DCS read is UNVERIFIED | LIVE |
| `coalition.addStaticObject(country.id.USA, s)`, `s` a FARP whose table also lists two FARPs under `units` | One airbase under the first name, key 1000003; the `units` list is ignored: `StaticObject.getByName` finds nothing under the second name | LIVE |
| Two `coalition.addStaticObject` calls with one `groupId` and their own `unitId` | Two airbases, keys 900101 and 900102 (the given `unitId`s), each with its own warehouse: 1,234 kg of jet fuel set at the first, 0 read at the second. A shared `groupId` does not join two statics | LIVE |
| `coalition.addGroup(country.id.USA, Group.Category.GROUND, two FARP units)` | A ground group of 2, no airbase, no static | LIVE |

`DCS.getFarpsAndCarriersMissionData().farps`, read with `slots.lua` after the tries,
lists the airbases scripts make, with names and places, despite its name (LIVE).
Without `dynamicSpawn` they read `dynamicSpawnAvailable` false (LIVE).

## 4. Draft for the coordinator quorum: a FARP pair the platform spawns

A script can make the pair, so the platform can. Today it cannot:

- `PutStatic` sends `STATIC_SPAWN`, whose agent handler calls
  `coalition.addStaticObject` (`lua/agent/shim.lua`, handler 21): one FARP.
- `PutFromStaticTemplate` makes one static spec per unit of a static group, drops the
  group's `groupId` and passes no `unitId` (`crates/api/src/static_template.rs`,
  `static_group`). A template's pair is two plain `addStaticObject` calls, so two
  FARPs, each with its own airbase and stock, as two single heliports from one template
  were (LIVE, docs/results/2026-11-24-template-heliports.md) and two `PutStatic` FARPs
  were (LIVE, ADR 0032). That a template pair spawns this way is read from the code,
  not run. The `same-group` try adds that a shared `groupId` alone does not join two
  FARPs (LIVE).

The question for the quorum: should the platform spawn a static group of heliports as
one `coalition.addGroup(country, -1, group)`, so the pair is one airbase with one stock,
as the mission file makes it?

Options:

1. **A heliport group in `STATIC_SPAWN`.** The static spec gains an optional list of
   further units (type, place, heading, the `extra` keys), valid only for category
   `Heliports`. The agent sends `coalition.addGroup(country, -1, ...)` when the list is
   not empty. `PutFromStaticTemplate` passes a template's heliport group as one such
   spec. The transport layout does not change: `STATIC_SPAWN` carries the static as
   one Lua value (`lv static`, spec/transport.golden), which can hold the list. What
   changes is the `STATIC_SPAWN` semantics in spec/commands.md, the agent handler, and
   the proto's `StaticSpec` (additive). Each further unit needs a DCS name, and that
   name also finds the airbase through `StaticObject.getByName` (LIVE, section 1); the
   naming rule (spec/world.md, "Names") and the replace-by-name and purge steps must
   either name further units under the static's platform name or say why they can
   ignore them. The handler compares the new object's `id_` with
   `Airbase.getByName(name)`; `addGroup(-1)` returns the airbase itself, under the
   first unit's name (LIVE, section 3), so that comparison holds for the first name.
2. **Templates only.** Keep `PutStatic` one unit, and let `PutFromStaticTemplate` send
   a heliport group as one `addGroup(-1)`. Smaller surface; a controller cannot build
   a pair without a template file. The same handler and naming questions apply.
3. **No pair spawn.** A campaign that needs a pair puts it in the mission file, which
   the platform's mission shells load, or keeps two FARPs in step with
   `SetWarehouseStock` from the controller. The stocks drift between a player's draw
   and the controller's next set; neither keeps one stock.

`GROUP_SPAWN` is not a route: its `category` is a `u32` (spec/transport.golden), which
cannot carry -1, and its handler (`lua/agent/shim.lua`, handler 16) reads a `Group`
from the return, while `addGroup(-1)` returns an `Airbase` (LIVE, section 3).

Facts any option rests on:

- In a mission file the pair's airbase `getID()` is the first unit's `unitId` (LIVE,
  section 1).
- `addStaticObject` keys a single FARP by a given `unitId` (LIVE, `same-group`).
  Whether `addGroup(-1)` honors a given `unitId` is UNVERIFIED; without one it took
  1000001 (LIVE).
- A script pair's jet fuel is 0 at spawn (LIVE); its other stock was not read. A
  single script FARP's warehouse starts empty (LIVE, ADR 0032).
- Dynamic spawn needs `dynamicSpawn = true` at spawn (LIVE, section 3; ADR 0032).
- A FARP the platform spawns reaches `LOST` and stays after `RemoveEntity` (LIVE,
  docs/results/2026-11-24-template-heliports.md); its cause is another step's finding.
  How a pair is removed, and whether `destroy()` on its airbase removes both FARPs, is
  UNVERIFIED.

## Open

- Whether a player's dynamic spawn uses the second FARP's pads, and whether a client
  draws the second FARP. Needs a client.
- Whether `addGroup(-1)` honors a heliport unit's `unitId`, which `dynamicSpawn` key it
  reads, what `getID()` returns for a script-made airbase, and its stock other than jet
  fuel.
- How a pair is removed: `destroy()` on the airbase, then the parking and the second
  FARP.
- A static template's heliport group spawns as separate FARPs today (section 4).

## Runs

Live runs used: 1 of 3. One DCS session: the `first` mission loaded and read, the
`each` mission loaded and read, the script tries made in it, then one restart to put
the server's own mission back.

After the live run the review changed `second.lua` to read and put back each
warehouse's own count, and `parking.lua` to skip offsets when the airbase has no point.
Neither change alters a read this run recorded.

macOS (aarch64), this commit's tree, 2026-11-24.

| Command | Result |
|---|---|
| `cargo test -p xtask miz` | 16 passed, 5 of them for the heliport-group option: it parses and refuses bad values, keys the first unit or each unit with its own fuel, refuses a template whose blue coalition lists countries or whose `warehouses` section is not empty, and adds to the committed Caucasus template |
| `lua lua/tests/farp_pairs_experiments.lua` | Passes under the strict mock: every snippet, and each try of `make.lua` |
| `mise run check` | Passes: 1,525 tests passed and 0 failed, and every other step |
