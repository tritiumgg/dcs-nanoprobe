# Person tests: ground crew, added FARPs, dynamic-spawn shell, static ruins

| | |
|---|---|
| Questions | (1) How a limited warehouse changes when a player refuels, defuels and rearms through the ground crew and deslots, at an airbase and a FARP. (2) Whether a FARP added during the mission can offer dynamic spawn. (3) Whether a client is offered a dynamic-spawn slot in an S19 shell with dynamic spawn on. (4) Whether a destroyed static's ruin goes from a client's screen after `world.removeJunk` |
| Date | 2026-11-16 |
| DCS | Server: 2.9.31.29434 dedicated server, `dcs_server.release`. Client: 2.9.31.29434 on Windows, flown by a person who joined from the server browser. Nothing was deployed to the client. |
| Platform | Server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Host rehearsal: macOS on arm64. |
| Build | Server agent `365d33e8c837e512`, `--dev` |
| Missions | (1, 2) The server's own Afghanistan mission, Dwyer (airbase 11) and its FARP (unit 8624) limited with `cargo xtask miz <mission> <out> --limited-airport 11 --limited-warehouse 8624`; each held 100 of every aircraft, 100 Mk-82 and 100,000 kg of each liquid. Dynamic spawn is on at both in the mission. (3, 4) An S19 shell built from an editor-made Caucasus template with one Client AH-64D at Batumi (not in the repository). |
| Scripts | Warehouse watch: `lua/experiments/warehouse/run.sh <server> none '' 1` repeated about every 3 s, each reading written to a file as it arrived. FARPs, in `lua/experiments/warehouse/`, run with `cargo xtask eval <server> <state> <file>`: `farp-spawn.lua` adds `np-wh-farp`, `farp-stock.lua` stocks it, `dynamic-settings.lua` reads the settings, `farp-dynamic-probe.lua` adds `np-wh-farp2` and tries `env.warehouses`, `farp-visible.lua` adds `np-wh-farp3`. Shell: `NP_SHELL_PERSON=<stop file> cargo xtask sidecar <server> --part shell --template FILE`; `base-read.lua` reads the two airports while it is held. Ruin: `lua/experiments/junk/ruin-person.sh <server> <spawn\|kill\|read\|clear\|destroy> [distance] [power] [radius]`. |
| Rehearsal | `lua/tests/junk_experiment.lua` runs `ruin-person.lua` under the mock. The FARP snippets, `base-read.lua` and the hold mode's hook read ran only live: the mock has no FARP airbases, and the hold needs a person. |

All results are LIVE unless tagged. What the person saw is their report.

## 1. Ground crew and deslot

Rule: each step's warehouse change is recorded as read. A step conserves when fuel aboard
plus warehouse fuel stays the same at every settled reading.

### Dwyer, F-16C_50, hot start

The person asked the ground crew for fuel 50 %, then 100 %, then a rearm from 6 Mk-82 to 2,
each after the last finished. The watch for these was stopped early, so only the net change
from mission start is known: F-16C_50 −1, Mk-82 −2, jet fuel −3,329 kg (the spawn takes
3,249). The deslot then returned the F-16C_50, 2 Mk-82 and 3,167.8 kg, the fuel aboard,
within one reading. Over the whole visit Dwyer lost 161 kg of fuel and no stores. The
engine ran the whole time (hot start), so the loss includes fuel burned.

The net change is consistent with the rearm returning the 4 Mk-82 taken off: the spawn
takes 6 (docs/results/2026-11-16-warehouse-player-deltas.md), the net was −2, and the
deslot returned 2. The rearm's own change was not read. No flight was flown in this run;
the person's view that a takeoff and a landing alone change nothing is UNVERIFIED here.

### FARP London, cold helicopters, no engine running

| | UH-1H run 1 | UH-1H run 2 | Mi-8MT |
|---|---|---|---|
| Spawn | −631 kg, airframe −1 | −631 kg, airframe −1 | −1,929 kg, airframe −1 |
| Defuel: tank / warehouse | −123 / +196 kg | −122 / +194 kg | −382 / +382 kg |
| Refuel: tank / warehouse | +122 / −322 kg | +121 / −318 kg | +380 / −380 kg |
| Deslot | +628.21 kg (fuel aboard), airframe +1 | +628 kg, airframe +1 | +1,926.41 kg, airframe +1 |
| Net at the FARP | −128.8 kg | −127 kg | −0.6 kg |

The warehouse side of a ground-crew transfer moves at about 18 to 19 kg/s. For the Mi-8MT
the tank moves at the same rate, and fuel is conserved. For the UH-1H the tank moves at
about 12 kg/s when defueling and 7 kg/s when refueling, and the warehouse loses about
125 kg per defuel and refuel cycle (126 and 124 kg). The loss repeats. Its cause is
UNVERIFIED: the UH-1H's own fuel system, or how its fuel is reported.

The spawn takes the full internal load, and the aircraft reads a little less aboard right
after it: 629 of 631 kg (UH-1H), 1,928.4 of 1,929 kg (Mi-8MT). The deslot returns what the
aircraft reads. For the Mi-8MT the whole net, −0.6 kg, is that spawn gap. For the UH-1H
the rest of the net beyond the cycle loss, 2.8 and 3 kg, is the 2 kg spawn gap plus the
1 kg the tank lost across the cycle (−123 + 122, −122 + 121), which the deslot does not
return. Why the aircraft reads less than the spawn took is not known.

In UH-1H run 1, one refuel request the ground crew did not acknowledge moved nothing; the
person asked again, and no reading changed between the two requests.

Rockets and gun ammunition were not offered at the FARP: this warehouse holds only Mk-82.

The spawn takes `fuelMassMax`, the full internal load, and the deslot returns the fuel
aboard, as in docs/results/2026-11-16-warehouse-player-deltas.md.

## 2. FARPs added during the mission

Rule: a route works when `DCS.getDynamicSpawnSettings` reads `dynamicSpawnAvailable` true
for the added FARP and the person is offered a spawn there.

| FARP (`coalition.addStaticObject`, blue) | Extra fields | Settings read (`id, false`) | Slot screen | F10 map |
|---|---|---|---|---|
| `np-wh-farp`, 2 km north of Dwyer | none | false, false | not listed | not shown |
| `np-wh-farp2`, 4 km north | `dynamicSpawn`, `allowHotStart`, `dynamicCargo` true | true, true | listed, spawn works | not shown |
| `np-wh-farp3`, 6 km north | as `np-wh-farp2`, and `hidden = false` | not read | not checked | shown |
| FARP London (the mission's) | from the editor | true, true | listed | its static is shown |

- The static's ID, its airbase ID and its world ID are one number (1000006, 1000010).
  `DCS.getFarpsAndCarriersMissionData().farps` lists each added FARP under it.
- `DCS.getDynamicSpawnSettings(id, true)` for a FARP returns nil.
- A FARP added without the fields stays off. Setting `dynamicSpawn` in
  `env.warehouses.warehouses` afterward, which listed only 8624, changes nothing.
- A static added by script without `hidden` is hidden from the client's F10 map.
- Each added FARP has its own warehouse. It starts empty, and `setItem` and
  `setLiquidAmount` stock it. A UH-1H spawned at `np-wh-farp2` took 1 UH-1H and 631 kg from
  that FARP's warehouse, and its deslot returned both, 629 kg. The spawned unit was named
  `_UH-1H_12-1`, with nothing before the underscore; at FARP London the name starts with
  the FARP's unit name.
- A grep of ED's Lua in the server install, read only, finds `dynamicSpawn` and
  `allowHotStart` only in the Mission Editor and the client slot dialogs, and no setter
  (ED-CODE). A setter outside Lua is UNVERIFIED. The editor's default for a new warehouse
  is dynamic spawn and hot start off (ED-CODE,
  `MissionEditor/modules/me_mission.lua:9537-9552`).

Observed: `np-wh-farp2`, added with `dynamicSpawn`, `allowHotStart` and `dynamicCargo`
true and stocked, was listed and a cold spawn there worked; `hidden = false` put
`np-wh-farp3` on the F10 map. Not isolated, so UNVERIFIED: whether stock is needed for the
listing, whether `allowHotStart` is needed (no hot start was tried), and whether one FARP
with all the fields is both listed and on the map.

## 3. Dynamic-spawn slot in an S19 shell

Rule: passes when the person is offered a dynamic-spawn slot at the shell's flagged airport
and can take it.

The hold mode activates a campaign with the template as its own, dynamic spawn on its
first airport (12, Anapa-Vityazevo), hot start off, and holds the shell for 45 minutes at
most. Read in the hook while held: `DCS.getDynamicSpawnSettings(12, true)` is true, false;
`DCS.getAvailableSlots` lists the two AH-64D seats at Batumi and nothing else.

The person was offered the AH-64D at Batumi and took it; no dynamic-spawn slot was
offered, and red had no slots. **Fails.** Two candidate causes, not separated, from
`base-read.lua` and ED's Lua:

- Anapa is neutral (coalition 0) in this template. ED's dynamic dialog lists a base only
  for the player's own coalition (ED-CODE,
  `Scripts/UI/MultiplayerSelectRoleMap/MultiplayerSelectDynamicDialog.lua:872`).
- `getInventory` lists no aircraft at Anapa or Batumi (Batumi is blue). The editor writes a base's aircraft
  list only when its dynamic spawn is on (ED-CODE, `me_mission.lua:4807`), and the shell
  sets the flag without writing one.

## 4. A destroyed static's ruin on a client

Rule: passes when the ruin goes from the client's screen after the call.

A `FARP Fuel Depot` static, `hidden = false`, was added 150 m ahead of the person's AH-64D
and destroyed with an explosion of power 200: found by name, `isExist` false, life 0. The
person saw a ruin with fire.

| Call | Server reads | Person saw |
|---|---|---|
| `world.removeJunk`, sphere 30 m, while burning | count 1; the static still found by name, dead | ruin stays; the fire went out on its own later |
| `world.removeJunk`, sphere 100 m, cold | count 0 | ruin stays |
| `Object.destroy` on the dead static | no error; still found by name, dead | ruin gone |

`world.removeJunk` does not remove a destroyed static's ruin on a client. `Object.destroy`
on the dead static does. What the first `removeJunk` call removed is not readable from
scripting.

Clear-debris run 2 (docs/results/2026-11-16-clear-debris.md, "Per wreck") found a
`FARP Fuel Depot` no longer found by `getByName` after `Object.destroy`. Here it was still
found. The runs differ in three ways, none isolated: here two `removeJunk` calls came
before the destroy, the destroy came about 3 minutes after the kill rather than 1 s or
300 s, and a client was connected.

## Open

- The UH-1H's ground-crew fuel loss: its cause, and whether other modules lose fuel too.
- The fuel change of each step of the F-16C's defuel, refuel and rearm (only the net is
  known).
- The S19 shell's dynamic spawn: a shell with the flag on a base of a player's coalition,
  with an aircraft list, offered to a client.
- A ruin cleared by `Object.destroy` for a client who joins after the call; aircraft and
  scenery ruins on a client.
- Whether `np-wh-farp3`, with `hidden = false` and the dynamic-spawn fields, is also
  listed in the slot screen (its map icon was checked, the slot screen was not); whether an
  added FARP needs stock to be listed; a hot start at an added FARP.
- Why a dead static is found by name after `Object.destroy` here and not in clear-debris
  run 2.
