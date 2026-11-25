# ADR NNNN: A heliport group spawns as one airbase

## Status

Accepted. Amends [ADR 0044](0044-static-template-spawn-with-heliports.md): a static
template's group of heliports is one static, not one per unit.
[ADR 0045](0045-spawned-farps-are-tracked-statics.md) holds for a heliport group as for a
single FARP.

## Context

In a mission file, a static group of two `FARP` units is one airbase: it takes the first
unit's name, owns the parking of both FARPs and has one warehouse. A script makes the
same with `coalition.addGroup(country, -1, group)`, which returns the airbase. Two
`coalition.addStaticObject` calls make two airbases with two stocks, also with a shared
`groupId` (LIVE, [the farp-pairs results](../results/2026-11-25-farp-pairs.md)). The
platform spawned every static with `addStaticObject`, so a controller could not build a
FARP site whose pads share one stock, and a template's group of FARPs came out as
separate FARPs.

`StaticObject.getByName` finds the group's airbase under every unit's name, and every
read the agent makes sees one object under the first unit's name (LIVE). What DCS makes
of a static group of other categories, of a group that mixes them, and how a group
behaves when removed, replaced or damaged is in
[the farp-groups results](../results/2026-11-25-farp-groups.md).

## Decision

A static with further heliport units spawns as one `addGroup(country, -1, group)`
([world](../../spec/world.md), "Heliport groups").

- `StaticSpec.further_units` lists the further units, each a `StaticUnit` with its
  type, place, heading and further keys. The static and every further unit have
  `extra.category` `"Heliports"`. There is no count limit besides the size of one
  `STATIC_SPAWN`.
- `STATIC_SPAWN` keeps its layout. Its `static` value is the group table for every live
  `Heliports` static, one unit or more, and the handler picks the call by its `units` key
  ([commands](../../spec/commands.md), "Layout: STATIC_SPAWN"). A group spawn that
  returns no airbase is refused, and what it made is destroyed.
- Each further unit has an `EntityId` of its own, given when the static is created and
  kept in the static's spec. Its DCS name carries that ID. The store maps it to its
  static, so a name that carries it is never read as an orphan.
- The group's name is the group form of the static's entity. The sidecar copies the
  first unit's `dynamicSpawn`, `allowHotStart`, `dynamicCargo` and `hidden` into the
  group table.
- `PutFromStaticTemplate` turns a static group whose every unit is a heliport into one
  static with further units. A group that mixes heliports with other statics stays one
  static per unit, and is logged.
- Every live `Heliports` static goes through `addGroup(-1)`, a single one as a one-unit
  group; a single heliport's ruin keeps `addStaticObject`. A group spawn under a name replaces what holds it, where `addStaticObject`
  under a group's first name leaves the group standing. A single FARP's spawn returns
  the same airbase object either way, so its birth read, tracking and removal under
  [ADR 0045](0045-spawned-farps-are-tracked-statics.md) are unchanged, and no
  `unitId` is passed, as before.
- Tracking, removal, owner and stock are a single FARP's, under the first unit's name.

Rejected:

- Templates only, with `PutStatic` kept to one unit. A controller could not build a
  site without a template file.
- No group spawn, with the controller keeping two FARPs' stocks in step. The stocks
  drift between a player's draw and the controller's next set.
- `GROUP_SPAWN` with category -1. Its `category` is a `u32`, and its handler reads a
  `Group` from the return, where `addGroup(-1)` returns an `Airbase`.
- Further units named with the unit form of the static's entity (`np.`). It gives one
  further name, so a group would hold two units.
- An index suffix on the static's name. It falls outside the name rule
  ([world](../../spec/world.md), "Names"). It stays the fallback if the reserved IDs
  fail live.

## Consequences

- A further unit takes an `EntityId` that no API call names: the IDs a controller sees
  skip it.
- A heliport group cannot be created dead, and its ruin after a death in play is
  [the results'](../results/2026-11-25-farp-groups.md) open item.
- A static's spec does not change after creation, so a further unit's ID is fixed. A
  call that replaces a static's spec, when one is built, must keep each further unit's
  ID by its index, take new IDs for new indexes and drop the IDs of missing ones.
- That a player's dynamic spawn uses a further unit's pads is UNVERIFIED.
