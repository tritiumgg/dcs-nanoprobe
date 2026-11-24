# ADR 0026: Destroyed statics stay ruins; wrecks are the controller's

## Status

Accepted

## Context

A campaign static that players destroy is a fact the platform cannot undo. A mission
reload or a DCS restart spawns every managed entity again. Without a rule, the static
comes back alive, and the destruction is lost.

A destroyed unit leaves a wreck, and the wreck is a different matter. The platform has
no basis to pick when a wreck should go. That depends on the campaign.

What DCS does (LIVE, DCS 2.9.31.29434, a Windows client on the server under Wine):

- A static spawned with `dead = true` shows a destroyed model for a fuel depot, a
  shelter, an ammo dump coating, an F-15C and a UH-1H. A windsock stays intact. A
  BRDM-2 shows alive. A speedboat is refused
  (docs/results/2026-11-17-person-fixes.md).
- `world.removeJunk` leaves a dead static, whether an explosion killed it or it was
  added dead. A client keeps showing its ruin until `Object.destroy` removes it
  (docs/results/2026-11-16-person-tests-2.md, docs/results/2026-11-18-ruins.md).
- A dead static's `isExist` reads false, so the agent holds no row for it.
- A wreck cannot be removed alone. A clear takes wrecks within about 30 m as well
  (docs/results/2026-11-16-clear-debris.md).

## Decision

A managed static that dies stays in the store as `dead`, DCS holds it as a ruin, and
only `RemoveEntity` takes it away. The controller owns the wrecks of units.

- A dead static is never spawned alive again. After a mission load, a restart, an
  activation or a `Materialize`, the reconciler sends `STATIC_SPAWN` with `dead = true`
  at the static's spec revision. An applied spawn at that revision in the generation
  is answered `AT_REVISION`, so a static that died in the generation is never
  replaced.
- A ruin never expires. The expiry that removes a dead group does not apply to it.
- `ClearDebris` never destroys a static the platform manages, dead or alive. It
  destroys dead statics that nobody manages. Only a `STATIC_DESTROY` the platform sends
  takes a ruin out of DCS ([ADR 0027](0027-clear-debris-column.md)).
- The platform does not clear wrecks and has no keep-wreck flag. The controller
  chooses a time, then sends a small `ClearDebris` at the wreck's position. To keep a
  destroyed unit, the controller puts a static spawned `dead` where the unit died
  (`PutStatic` with `dead`).
- The store keeps, per dead static, the agent boot and generation of the last ruin
  spawn written and of the last time DCS held the dead object. Both are written before
  the commands reach DCS. A sidecar restart sends nothing for a ruin marked held.

Rejected:

- Let `ClearDebris` destroy dead managed statics. A cleanup would erase a campaign
  fact.
- A platform-managed keep-wreck flag. It would make the platform decide how long a
  wreck lasts. That depends on the campaign.

## Consequences

- The ruin of a type with no destroyed model looks intact after a restore. A ground
  vehicle's wreck cannot be kept as a static spawned dead. Other types are UNVERIFIED.
- A ruin is not observed. Nothing tells the sidecar that another script removed it. The
  next generation spawns it again.
- A ruin's spawn counts against the materialization budget. A ruin owed a spawn counts
  as waiting for the restore burst.
- Whether a FARP added dead makes its airbase is UNVERIFIED.
- The rules are in [spec/world.md](../../spec/world.md) ("Ruins") and
  [spec/commands.md](../../spec/commands.md) ("Layout: CLEAR_DEBRIS").
