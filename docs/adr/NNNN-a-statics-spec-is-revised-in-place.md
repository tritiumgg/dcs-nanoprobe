# ADR NNNN: A static's spec is revised in place

## Status

Accepted. Amends [ADR 0046](0046-heliport-groups-spawn-as-one-airbase.md): a static's
spec changes by revision, against its consequence "A static's spec does not change after
creation". A further
unit's ID is kept by index across revisions, as that consequence asked; a dropped unit's
ID stays mapped to its static until the next mission load, where that consequence said
to drop it.

## Context

`PutGroup` revises a group's spec in place, and the reconciler spawns a `live` entity
again under its name when its spec is ahead of its applied spawn. Without a revision of a
static, a controller moves a static or grows a FARP into a heliport group only by removing
it and creating another under a new ID.

What DCS does (LIVE, DCS 2.9.31.29434 on the server under Wine,
[the farp-groups results](../results/2026-11-25-farp-groups.md)):

- A heliport group spawn over a name replaces what holds it at once, paused or running,
  at any change of size. A dropped further name then gives the old handle with `isExist`
  false.
- `addStaticObject` over a heliport group's first name leaves the group standing.
- A new airbase has an empty warehouse, and takes the coalition of its country.

A spawn under the name of an existing static replaces it
([ADR 0015](0015-scripted-destroys-report-through-command-done.md)). A fortification
moved in place by `addStaticObject` was gone at once, also while paused, unlike ADR 0015's
"while paused, DCS lets them go at the resume", and a FARP's new
airbase keeps the old one's `getID()` with a new object (LIVE,
[the static-replace results](../results/2026-11-25-static-replace.md)).

## Decision

`PutStatic` with a `static_id` revises that static's spec in place
([world](../../spec/world.md), "Revising a static").

- It may change the type of a static that is no heliport, the place on the same theater,
  the heading, the extra keys, and a heliport's further units, a kept unit keeping its
  type. A static stays
  where it was spawned, so its place is spec. One call is one new spec revision. The label
  changes with no spec revision. An equal spec changes nothing.
- Further unit i keeps its ID. Units past the old count take new IDs; a dropped unit's ID
  stays mapped to its static until the next mission load, and no other entity takes it.
- Refused with nothing written: another country, another theater, a switch into or out
  of `Heliports`, and `dead` (`INVALID_ARGUMENT`); another type for a heliport or a kept
  further unit, and a static in state `dead`, created dead or not, whatever the spec
  (`FAILED_PRECONDITION`).
- No opcode, layout or golden change. The reconciler's spawn of a `live` entity that is
  behind its spec applies the revision, and counts against the materialization budget.
  `STATIC_SPAWN` names the replaced static in `destroyed`
  ([commands](../../spec/commands.md), "Layout: STATIC_SPAWN").
- When a heliport's `STATIC_SPAWN` of the current generation resolves, whatever its
  outcome, the reconciler forgets what it saw and sent of that airbase name's owner and
  stock. Every spawn resolves, by its `RESULT` or as unknown at the next `HELLO`; the old
  object's `OBJECT_GONE`, which does the same through the airbase's object, can be lost to
  a gap. So the new airbase's empty first sample is never read as play, and the desired
  owner and stock are sent again. A result of an earlier generation changes nothing.
- At that point, for a `live` FARP spawned again in the generation, when the spawn applied
  or its outcome is unknown, the reconciler keeps the old airbase's last sampled owner and
  stock, or what it kept when the old object's `OBJECT_GONE` came first. A spawn DCS did
  not take builds no carry and keeps those samples for the next one. Once the new airbase
  is sampled it sends, once, the owner when no desired owner is set and every item without
  a desired count, each where the new sample does not already show it; a carried command
  that does not apply for now is sent again. The carry is kept until a sample after its
  commands are answered, so a later revision carries the first airbase's state. A desired
  value wins and is never set by the carry.
- A static that cannot move keeps its spec place against its `POSITIONS` rows. While a
  cargo's spawn is owed, a position row of its old object does not replace the revised
  place.

Rejected:

- A guard keyed on the airbase's object ID changing under a name. `WAREHOUSE` carries no
  object, and an `OBJECT_GONE` can be lost to a gap.
- No carry, with only desired values sent again. A never-set item would read 0 and a
  captured owner would revert to the country's coalition after every FARP revision.
- Making the carried values desired. It would change what "desired" means: a value the
  controller set.
- Revising a destroyed static's ruin. A ruin is kept by generation, not by revision, and a
  heliport ruin is an open item ([ADR 0045](0045-spawned-farps-are-tracked-statics.md)).
- A switch between heliport and other categories through `addStaticObject` and
  `addGroup`: `addStaticObject` over a group's first name leaves the group.
- A country change, as for a group.
- A heliport type change in place. A `SINGLE_HELIPAD` over a `FARP`'s name kept the
  `FARP`'s 4 spots (LIVE, runs 2 and 3 of docs/results/2026-11-25-static-replace.md),
  so the platform would report a helipad where DCS has a `FARP`'s parking. It stays
  refused until a live check shows a clean change.

## Consequences

- A FARP revision makes a new airbase with an empty warehouse, and the old airbase's state
  is carried to it. What DCS changed since the last sample, a draw or a capture, is lost.
- Whether players on a replaced airbase's pads keep their aircraft is UNVERIFIED.
- `GroupSpec` and `StaticSpec` revisions share the in-place spawn; the store's group
  record carries `retired` further IDs, an additive disk key.
- A static that cannot move keeps its spec place: its `POSITIONS` rows do not change it,
  so an equal spec stays equal.
- Spec: [world](../../spec/world.md), "Revisions", "Heliport groups", "Revising a
  static"; [commands](../../spec/commands.md), "Layout: STATIC_SPAWN".
