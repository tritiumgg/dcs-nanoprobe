# ADR 0043: Spawn a whole static-template file in one commit, heliports included

## Status

Accepted

## Context

[ADR 0003](0003-static-template-spawn-in-one-commit.md) refused a static template that
holds a heliport or a grass-airfield static, because each makes an airbase. A FARP
static spawned with `PutStatic` already makes its airbase, and the platform samples it
and keeps its warehouse ([ADR 0032](0032-spawned-farps-and-dynamic-spawn.md), LIVE). A
template's static takes `PutStatic`'s checks and its spawn command, so a heliport static
in a template takes the same path.

The Mission Editor saves a static template as one assignment `staticTemplate = {...}`
of literal values, with coalitions, countries and absolute map places (ED-CODE,
[the static-template results](../results/2026-11-23-static-templates.md)). It saves a
static's unit with `type`, `category`, `shape_name`, `rate`, `heading`, `x`, `y`,
`unitId`, `name` and `tasks`, and a heliport's also with `heliport_frequency`,
`heliport_modulation` and `heliport_callsign_id` (LIVE, a file the editor saved,
[the results](../results/2026-11-24-template-heliports.md)). No live check has seen a
grass-airfield static make its airbase.

A controller that spawns a layout with one call per entity can fail halfway and leave
part of it in the campaign. The editor loads a static template where it was saved;
a controller also needs to place it at a chosen point and turn it.

## Decision

`PutFromStaticTemplate` reads one static-template file under `[missions] template_dirs`
and writes every vehicle group and static in it in one store commit, or none.

- The call reads the file once, as data, with the reader of
  [group templates](0005-spawn-group-from-template-file.md) and a 1 MiB cap. The store
  keeps the converted specs. Nothing opens the file again.
- Every check of every entity runs before the first write: the checks of `PutGroup`
  and `PutStatic`, the size of each spawn command, each value's encoding, and the
  entity IDs left. The call has one write, one idempotency record and one commit.
- Without an `anchor`, each entity stands where the file has it, on the file's
  `theatre`, which must be the campaign's. With one, the center of the box that bounds
  every unit and static goes to the anchor. `heading` turns the layout about that
  origin in both modes.
- Each entity keeps the DCS country number of its file country. An optional `country`
  replaces all of them.
- A static of category `Heliports` is built as any static. Its spec is the one a
  controller would give `PutStatic`: type, place, heading, and `category` and
  `shape_name` in `extra`. DCS makes its airbase under the static's platform name, and
  the platform samples it as ADR 0032 sets out.
- The call refuses the whole file for any plane, helicopter, ship or complex group, a
  static linked to a unit, and a static of category `GrassAirfields` or with a
  `grassairfield_` key.
- The sidecar drops and logs every key it does not convert: routes, tasks, timing and
  a heliport's `heliport_` keys among them. A key can be passed later, additively,
  after a live check reads it back. A controller sets orders afterward with
  `SetOrder`. The call has no `order` and no `virtual` field.
- The answer lists each group with its unit IDs and spec revision, then each static
  with its spec revision, in the order of the file's text, plus the commit's revision.

Rejected:

- Passing the `heliport_` keys. Whether DCS reads them from `coalition.addStaticObject`
  is UNVERIFIED.
- Lifting the grass-airfield refusal with the heliport one. No live check has seen one.
- A convert-only call that returns specs. It would let a controller read any table
  file under the listed folders.
- Refusing the call while paused, or leaving its entities out of the restore burst.
  A call made while paused spawns in the burst, as several `PutGroup` calls do
  ([persistence](../../spec/persistence.md), "Restore burst").

## Consequences

- One store commit is not one DCS spawn. While the mission runs, at most
  `materialize_per_frame` spawns are unanswered at a time outside the restore burst
  ([ADR 0022](0022-materialize-per-frame-bounds-unanswered-spawns.md)). Each entity
  can fail in DCS on its own and shows it in its own last error.
- A refused file makes no entity. Two entries under one key, or under one list number,
  refuse the file.
- A heliport from a template is a FARP to the platform. A controller stocks it with
  `SetWarehouseStock` under the static's platform name. A `FARP`'s warehouse starts
  empty (LIVE, ADR 0032); that a helipad's does is inferred from that, not observed.
- The FARP's radio frequency, modulation and callsign from the file are not applied.
  `dynamicSpawn`, `allowHotStart`, `dynamicCargo` and `hidden` are not converted; a
  controller that needs them uses `PutStatic`.
- Whether a heliport in a group with `dead = true` makes its airbase is UNVERIFIED, as
  for a FARP added dead (ADR 0032).
- A FARP static the platform spawns, by either call, reaches `LOST` and stays in DCS
  after `RemoveEntity` (LIVE,
  [the results](../results/2026-11-24-template-heliports.md)). The template adds
  nothing to that.
- Spec: [world](../../spec/world.md), "Static templates".
