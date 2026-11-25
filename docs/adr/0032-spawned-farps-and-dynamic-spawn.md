# ADR 0032: Spawned FARPs and dynamic-spawn airports

## Status

Accepted. Its object and removal rules are superseded by
[ADR NNNN](NNNN-spawned-farps-are-tracked-statics.md).

## Context

A controller can put a FARP static into a running mission, and a player can then spawn
an aircraft at it from the slot screen. The same holds for an airport the mission
file marks for dynamic spawn. Four facts shape the design (DCS 2.9.31.29434, a
Windows client with a player slotted in, on the server under Wine):

- Dynamic spawn offers an airport only to the coalition `DCS.getAirdromesCoalition()`
  gives it, from the aircraft in its warehouse (ED-CODE,
  `Scripts/UI/MultiplayerSelectRoleMap/MultiplayerSelectDynamicDialog.lua`). The
  editor writes an empty aircraft list for an unlimited airport (ED-CODE,
  `MissionEditor/modules/me_mission.lua`), and an unlimited warehouse ignores a set
  (LIVE, docs/results/2026-11-15-s15-world-commands.md).
- `coalition.addStaticObject` of a FARP makes an airbase of the static's name, with the
  static's ID and its own empty warehouse. It is not in `coalition.getStaticObjects`.
  `world.getAirbases` lists it (LIVE, docs/results/2026-11-16-person-tests-2.md,
  docs/results/2026-11-17-person-fixes.md).
- The static's `dynamicSpawn` key is read at spawn time only. A static added by script
  without `hidden = false` is hidden on the F10 map (LIVE,
  docs/results/2026-11-16-person-tests-2.md).
- An unlimited warehouse reads 1,000,000 from `getItemCount` and `getLiquidAmount`, and
  lists no item and 100,000 of each liquid in `getInventory` (LIVE,
  docs/results/2026-11-17-person-fixes.md).

## Decision

The platform tracks a spawned FARP's airbase and warehouse for the mission generation,
and the mission shell turns on dynamic spawn only for airports with a limited
aircraft stock that the controller sets.

- An airbase made by a FARP static is listed and sampled from its `STATIC_SPAWN` until
  the static is destroyed or dies, when the sidecar drops it. A same-name
  `STATIC_SPAWN` replaces the entry. The airbase is gone at a new generation until
  its static is spawned again.
- The warehouse tier guards `getCoalition` for a gone FARP and keeps its entry as a
  tombstone, so the airbase indexes after it hold. The agent writes no record for a
  tombstone. The sampler accepts an airbase count that grows within a generation.
- The store keeps the desired owner and stock for an airbase by name, so a FARP spawned
  again under the name receives them. The platform destroys an airbase only by
  destroying its FARP.
- The sidecar adds none of the FARP's keys. `StaticSpec.extra` carries `category`,
  `dynamicSpawn`, `allowHotStart`, `dynamicCargo` and `hidden`, with no default.
  [spec/commands.md](../../spec/commands.md) ("Layout: STATIC_SPAWN") lists them.
- For an airport the mission shell enables, the shell writes `unlimitedAircrafts =
  false`, and writes `aircrafts = {}` when the airport has none. Munitions and fuel
  keep the template's flags ([spec/persistence.md](../../spec/persistence.md), "Mission
  shells"). The controller sets the stock with `SetWarehouseStock` and the owner with
  `SetAirbaseOwner`.
- A sample of an unlimited warehouse is not its stock. The `Airbase.stock` comment in
  `proto/nanoprobe/v1alpha1/world.proto` says so.

Rejected:

- Write the airport's `coalition` into the shell from the store's desired owner. Not
  needed: the slot dialog reads the owner `SetAirbaseOwner` sets (LIVE,
  docs/results/2026-11-17-person-fixes.md).
- Leave a gone FARP out of the airbase list. Row indexes would shift.

## Consequences

- Dynamic spawn at an enabled airport also limits the aircraft AI can use when the
  mission spawns them there. The shell documents it.
- A FARP's warehouse starts empty each time. A controller must stock it before a
  player can spawn.
- Whether `allowHotStart` and `dynamicCargo` are needed for a spawn is UNVERIFIED.
  Whether a FARP added dead makes its airbase is UNVERIFIED.
- Whether the editor's airport ID equals `Airbase:getID()` is UNVERIFIED.
- The rules are in [spec/world.md](../../spec/world.md) ("Kinds") and
  [spec/sampling.md](../../spec/sampling.md).
