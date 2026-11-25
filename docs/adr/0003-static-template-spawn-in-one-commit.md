# ADR 0003: Spawn a whole static-template file in one commit

## Status

Superseded by [ADR 0043](0043-static-template-spawn-with-heliports.md)

## Context

The Mission Editor saves the objects of an open mission as a static template, a `.stm`
file: one assignment `staticTemplate = {...}` of literal values, with coalitions,
countries and absolute map places (ED-CODE,
[the static-template results](../results/2026-11-23-static-templates.md)). DCS ships
23 of them, all vehicle groups. The 23 shipped files hold 44 groups and 346 units
(ED-CODE). A controller that spawns it with one `PutGroup` or `PutStatic` per entity
can fail halfway and leave a part of the layout in the campaign.

The editor loads a static template where it was saved, with no anchor and no turn
(ED-CODE). A controller also needs to place the layout at a chosen point and turn it.
The form of a static in a saved file is taken from the editor's code. No shipped file
holds one, so it is UNVERIFIED against a saved file.

DCS spawns each entity on its own. One store commit can still hold many entities, and
a campaign that is paused spawns every waiting group in one frame
([persistence](../../spec/persistence.md), "Restore burst").

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
- The call refuses the whole file for any plane, helicopter, ship or complex group, a
  static linked to a unit, and a heliport or grass-airfield static. These would make
  airbases or links the world model has not seen.
- The sidecar drops and logs every key it does not convert. This includes routes,
  tasks and timing. A controller sets orders afterward with `SetOrder`. The call has
  no `order` and no `virtual` field.
- The answer lists each group with its unit IDs and spec revision, then each static
  with its spec revision, in the order of the file's text, plus the commit's revision.
  Spec: [world](../../spec/world.md), "Static templates".

Rejected:

- A convert-only call that returns specs. It would let a controller read any table
  file under the listed folders.
- Refusing the call while paused, or leaving its entities out of the restore burst.
  A paused campaign spawns every waiting entity in the restore burst, however it came
  to wait ([persistence](../../spec/persistence.md), "Restore burst"). A call made
  while paused spawns in the burst, as several `PutGroup` calls do.

## Consequences

- One store commit is not one DCS spawn. While the mission runs, the reconciler's
  budget bounds the spawns: outside the restore burst, at most `materialize_per_frame`
  are unanswered at a time
  ([ADR 0022](0022-materialize-per-frame-bounds-unanswered-spawns.md)). Each entity
  can fail in DCS on its own and shows it in its own last error.
- A refused file makes no entity. The controller fixes the file and calls again.
- Two entries under one key, or under one list number, refuse the file. Lua would keep
  the later and the author's intent is unknown.
- The form of a static as the editor saves it is UNVERIFIED. A live
  check placed two units and two `Fortifications` statics within 15 µm of the
  requested places (LIVE, same results document).
- A key a file carries that the sidecar drops can be passed later, additively, after a
  live check reads it back.
