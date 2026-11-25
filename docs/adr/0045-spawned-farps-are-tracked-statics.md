# ADR 0045: A spawned FARP is a tracked static, with its radio

## Status

Accepted. Supersedes the object and removal rules of
[ADR 0032](0032-spawned-farps-and-dynamic-spawn.md), and the rejection of the
`heliport_` keys in [ADR 0044](0044-static-template-spawn-with-heliports.md) with its two
consequences on the radio and on `LOST`.

## Context

ADR 0032 says the platform destroys a FARP's airbase only by destroying its FARP. A FARP
the platform spawned, by `PutStatic` or a template, instead reached `LOST` and stayed in
DCS with its airbase after `RemoveEntity` (LIVE,
[the template-heliports results](../results/2026-11-24-template-heliports.md)).

What DCS does (LIVE, DCS 2.9.31.29434 on the server under Wine,
[the results](../results/2026-11-25-farp-fixes.md)):

- `coalition.addStaticObject` of a `Heliports` static returns an object of class
  `Airbase`, `Object.getCategory` 4. `StaticObject.getByName` returns the same object.
  It reads as a static: `isExist`, `getLife` 3600, `getDesc().category` 1, coalition,
  country, type and place.
- `coalition.getStaticObjects` does not list it. Its birth event has initiator category
  4, which the shim does not queue: it queues only units, statics and cargo (1, 3, 6).
  So the agent never wrote its `OBJECT`, the enumeration's end moved it to `LOST`, and
  `RemoveEntity` sent no `STATIC_DESTROY`.
- `destroy()` on it removes the static and its airbase, but only when the simulation
  runs. On a paused server nothing changes until the mission resumes. Afterward
  `StaticObject.getByName` still returns a handle and `world.getAirbases` still lists
  the airbase, both with `isExist` false, until the next mission load.
- `heliport_frequency`, as a number or as text, `heliport_modulation` and
  `heliport_callsign_id` set the airbase's frequency, modulation and callsign. Without
  them DCS uses 127.5 MHz, AM and London.

## Decision

The agent reads a FARP it spawns as a static, and the platform carries a FARP's radio.

- The `STATIC_SPAWN` handler knows a FARP by the object `addStaticObject` returns,
  `Object.getCategory` 4, not by a lookup of the name: a FARP of the name DCS has not
  yet removed may still answer one (UNVERIFIED). It queues that object as a static birth, so the
  next pump adds its row and writes its `OBJECT`.
- The enumeration's last step visits each airbase entry a `STATIC_SPAWN` made in the
  generation that is not a tombstone, through the entry's object, which is the static.
  It skips an entry whose `isExist` is not true, and ignores a pending-destroy ID as
  every step does. Each visit is one enumeration item on the births' and the
  enumeration's account. With no spawned FARP it makes no DCS call.
- The event filter stays at categories 1, 3 and 6.
- `RemoveEntity` destroys the FARP as any static. The agent writes `OBJECT_GONE
  DESTROYED` at once. The airbase entry turns into a tombstone when the agent reads its
  static's `isExist` false, which on a paused server is after the mission resumes. The
  sidecar sends no `AIRBASE_OWNER` or `WAREHOUSE_SET` to the airbase of a FARP the
  store no longer holds, removed or purged.
- `AIRBASE_OWNER` and `WAREHOUSE_SET` for a spawned FARP go to the object its airbase
  entry holds while the entry stands, for the same reason; other airbases are looked
  up by name. A `STATIC_DESTROY` whose name finds only a removed FARP's handle
  (category 4, `isExist` false) is applied with nothing to destroy; a dead static,
  which also reads false, is still destroyed (ADR 0026).
- ADR 0032's tombstone rule and its store of the desired owner and stock by name stand
  as written.
- `StaticSpec.extra` carries `heliport_frequency` (a number or numeric text, as given),
  `heliport_modulation` and `heliport_callsign_id` with no default and no check beyond
  the value's encoding. `PutFromStaticTemplate` passes them for a `Heliports` static,
  and drops and logs them on a static of another category.

Rejected:

- Queue category-4 births. The mission's airbases would read as statics; whether DCS
  sends births for them is UNVERIFIED.
- Bind the FARP in the sidecar from its spawn's answer. It would bypass the agent's
  object table and its existence checks.
- Look the FARP up by name in the enumeration step. The airbase entry already holds the
  object.
- Keep dropping the radio keys. DCS reads them.

## Consequences

- A FARP the platform spawns is `LIVE` in the World, and `RemoveEntity` takes it and its
  airbase out of DCS once the simulation runs.
- On a paused server a removed FARP's airbase stays until the mission resumes (LIVE).
  Whether a player is offered a slot there meanwhile is UNVERIFIED; the platform
  assumes one may be.
- A FARP the mission file places is not read as a static. Whether
  `coalition.getStaticObjects` lists one is UNVERIFIED.
- A FARP killed in play, and its ruin spawned again with `dead = true`
  ([ADR 0026](0026-destroyed-statics-stay-ruins.md)), are UNVERIFIED. If DCS reports a
  killed FARP with `isExist` false, `STATIC_DESTROY` applies with nothing destroyed, as
  for a removed one; the step that takes up FARP ruins revisits this. Whether a FARP
  added dead makes its airbase is UNVERIFIED.
- Callsign IDs other than 1 to 4, and other modulation values, are UNVERIFIED.
- Spec: [sampling](../../spec/sampling.md), "Tiers"; [records](../../spec/records.md),
  "The object table"; [world](../../spec/world.md), "Kinds";
  [commands](../../spec/commands.md), "Layout: STATIC_SPAWN".
