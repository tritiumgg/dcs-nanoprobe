# Warehouse changes from a player's actions

| | |
|---|---|
| Question | How a limited warehouse's inventory changes when a player client slots in, rearms, deslots, flies to a base and deslots, or is killed on the ground, at an airbase and at a FARP |
| Date | 2026-11-16, one live run (one mission load) |
| DCS | Server: 2.9.31.29434 dedicated server, branch `dcs_server.release`. Client: DCS 2.9.31.29434 on Windows. Both read with `cargo xtask target check`. |
| Platform | Server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Client: Windows, driven over SSH with no person at the controls. Host rehearsal: macOS on arm64. |
| Builds | Server: agent build `365d33e8c837e512`, `--dev`. Client: `b39469635a846ca1`, `--dev`, removed after the run. |
| Mission | The server's own mission, with the warehouses of airbase 11 (Dwyer) and of the FARP with unit ID 8624 limited: `cargo xtask miz <mission> <out> --limited-airport 11 --limited-warehouse 8624`, loaded with `cargo xtask load`. The `start_inventory` read before the first step: each held 100 of every aircraft type, from the mission's own `aircrafts` table, which the tool keeps. Each held 100 Mk-82, which the tool writes into an empty `weapons` table, and no other weapon. Each held 100,000 of each liquid, from the mission's `InitFuel` of 100. The `stock` action was not run. |
| Script | `lua/experiments/warehouse/run.sh <server> <action> [slot] [seconds] [stock]` runs `step.lua` in the server's hook state. `cargo xtask eval <server> server lua/experiments/warehouse/farp-spawn.lua` runs `farp-spawn.lua`. The client joined with `cargo xtask client join` and entered its aircraft with `lua/experiments/alone/e31-spawn.lua`. |
| Rehearsal | `lua/tests/warehouse_experiments.lua`, run by `mise run check` |
| Result | Slot, deslot on the ground and kill on the ground have verdicts at the airbase. The rearm, the flight and every FARP step with a player wait for a person. |

All results are LIVE.

## Rule

Written before the runs.

Each step reads every limited warehouse with `getInventory()` before the action. It reads
them again once a second during the watch and once at its end. Each read records real time
from the start and model time. It also reads the player's unit: fuel fraction,
`fuelMassMax`, life and `getAmmo`. A step is settled when the run reads the warehouse at
least once after the action and the last 30 s of the watch show no further change. Its
verdict is the net delta per item and liquid, set against what the unit carried:

- **Slot, at spawn:** stores and fuel are taken when each loaded store's count falls by
  the number loaded, and jet fuel falls by the unit's fuel mass (fuel fraction ×
  `fuelMassMax`), within 1 %.
- **Deslot on the ground, and kill on the ground:** returned when each store's count
  rises by the number the unit held and jet fuel rises by the unit's fuel mass at the
  action, within 1 %. Not returned when the counts do not change. Any other change is
  recorded as it is.
- **Rearm and refuel, and take off, land and deslot:** recorded the same way, against
  the ground crew's order and the fuel on landing.

The 1 % tolerance covers the fuel burned between the last unit read and the action.

## Deltas at the airbase

The slot was an F-16C_50 (unit 8572) parked hot at Dwyer. Its mission loadout lists
Mk-82, GBU-12, AIM-9, a fuel tank and a targeting pod. Only the Mk-82 was in stock.
`fuelMassMax` read 3,249 kg.

Times are seconds of the watch, which reads once a second. In a slot run the watch starts,
then `net.spawn_player` is sent to the client: about 8 s in for run 1 and 5 s in for run 2,
plus the start-up of `cargo xtask eval`, which was not timed.

| Step | Run | Aircraft F-16C_50 | Jet fuel (liquid 0), kg | Mk-82 | Unit | Change seen | Watch | Verdict |
|---|---|---|---|---|---|---|---|---|
| 1. Slot and spawn | 1 | −1 | −3,249 | −6 | first read after the spawn: fuel 0.99160; at the end 6 Mk-82 and 510 gun rounds | 19 s, one change | 90 s, none after | Taken |
| 1. Slot and spawn | 2 | −1 | −3,249 | −6 | at the end: fuel 0.99055 | 8 s, one change | 40 s, none after | Taken |
| 3. Deslot on the ground | 1 | +1 | +3,205.98 | +6 | before: fuel 0.98676 (3,205.98 kg), 6 Mk-82 | 1 s, one change | 120 s, none after | Returned, fuel exact |
| 3. Deslot on the ground | 2 | +1 | +3,212.05 | +6 | before: fuel 0.98863 (3,212.05 kg), 6 Mk-82 | 1 s, one change | 60 s, none after | Returned, fuel exact |
| 5. Killed on the ground | 1 | 0 | 0 | 0 | before: fuel 0.99039, 6 Mk-82. An explosion of 200 at the unit; `coalition.getPlayers` listed no unit at the end. | none | 120 s, then a deslot and 60 s more | Not returned: the airframe, stores and fuel are lost |

- The spawn takes `fuelMassMax`, the full internal load, not the fuel the unit reads a
  moment later. It takes the airframe and each loaded store. The airframe, the fuel and the
  Mk-82 change in the same read.
- A store not in stock is left off the aircraft. The F-16C spawned with its Mk-82 only.
  It did not fail to spawn.
- The warehouse listed no gun rounds, flares or chaff, and the unit spawned with 510 gun
  rounds anyway. Whether those are warehouse items at all is UNVERIFIED.
- A deslot on the ground returns the airframe, every store still loaded, and the fuel
  still aboard, to the kilogram, within 1 s.
- After the kill the player's unit was gone: `coalition.getPlayers` listed none at the end
  of the watch. The run did not read its life or a death event. Nothing returned, and a
  later deslot of the dead player returned nothing.
- Each change appeared in a single 1 s read and did not change again during the watch.
- The airframe counts move with each spawn and deslot. This bears on E6's open item that a
  limited warehouse's `aircrafts` table showed 0 aircraft in the inventory read
  (docs/results/2026-11-15-s15-world-commands.md): here the mission's own table listed
  100 of each type, and the inventory read them.

## FARP spawned during the mission

`farp-spawn.lua` adds a FARP static 2 km north of Dwyer with `coalition.addStaticObject`.

| Check | Result |
|---|---|
| `Airbase.getByName` finds it | yes |
| `getWarehouse` | a warehouse |
| Its inventory | 0 aircraft entries, 0 weapon entries, each liquid 0 |
| `setItem` Mk-82 7 and `setLiquidAmount` 0 to 1,234 | read back 7 and 1,234: limited, by E6's rule that only a limited warehouse reads a set back |

A spawned FARP starts with an empty limited warehouse. A player who lands there finds no
fuel or stores until something adds them.

## Waits for a person

These steps need a pilot at the client and were not run:

- 2. Rearm and refuel on the ground through the ground crew radio menu, at Dwyer and at a
  FARP.
- 4. Take off, land at the same or another base, and deslot.
- Every FARP step with a player. The mission's FARP has no client slot, and Dwyer's
  helicopter spawns are dynamic slots, which the scripted client does not take.

## Open items

- Unlimited warehouses read differently from E6. The mission marks every airport but
  Dwyer with all three `unlimited*` flags true: Herat (airport 1) is one, read from the
  `warehouses` member. In the `list` read, each of those airbases' `getInventory` listed 4
  entries, every count below 1,000,000. The counts were not recorded. E6 run 1 read every
  count of an unlimited warehouse as 1,000,000, on the Caucasus template
  (docs/results/2026-11-15-s15-world-commands.md), and spec/commands.md (`WAREHOUSE_SET`)
  says an unlimited warehouse keeps reading its full count. The two missions differ in map
  (Afghanistan here) and in Dwyer's dynamic spawn; which difference matters is UNVERIFIED.
  This run did not repeat E6's set-and-read control on an unlimited airbase, so the
  spec's sentence is unsettled for this mission. A later run should read Herat's liquid
  counts and `getItemCount` after a `setItem`.
- For the same reason, `step.lua` watches every warehouse, and its `stock` action found no
  item names to copy. Stocking a loadout's other stores needs their warehouse names. A
  later run can take them from `getAmmo` of a spawned unit.
- A FARP static added during the mission creates an airbase with its own warehouse (LIVE,
  this run, through `coalition.addStaticObject`). spec/world.md says the platform never
  creates an airbase or a warehouse. Whether a `STATIC_SPAWN` of a FARP does the same is
  UNVERIFIED. The FARP's naming as an airbase is in
  docs/results/2026-11-15-e21c-farp-name.md. Whether the store tracks such a warehouse
  needs a ruling in a later step.
