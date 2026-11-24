# ADR 0010: No trigger zones

## Status

Accepted

## Context

A controller often needs to know which units are inside an area. DCS has trigger zones,
and a zone could be a world kind with enter and leave events.

The DCS scripting API offers too little to run zones as a managed kind
([the world-command results](../results/2026-11-15-s15-world-commands.md), "Trigger
zones"):

- `trigger.misc` has `addTrigger`, `addZone`, `getUserFlag` and `getZone`. It has no
  call that removes, sets or updates a zone (LIVE, DCS 2.9.31.29434, dedicated server
  under Wine 10.0).
- A second `addZone` under an existing name creates another zone with a renamed suffix
  and leaves the first unchanged, so a zone cannot be changed or replaced (LIVE).
- After a mission load, `getZone` returned nil for every zone made earlier (LIVE).
- Event 35 (`S_EVENT_TRIGGER_ZONE`) fired only for a zone that had a trigger. Its
  initiator is the group, not the unit, and it carries no place or zone name (LIVE).
  Nothing fired on leaving.
- No Lua file in the install defines a call that removes or changes a zone, or reads
  event 35 (ED-CODE).

## Decision

The platform has no zone kind. Controllers compute area membership from unit
positions, and `QueryArea` serves area queries.

- `QueryArea`, or a `WorldFilter` with a `center` and a `radius`, returns every group
  with a unit within the radius and every static within it, virtual or not, from the
  store's grid ([world](../../spec/world.md), "Authority"). A dead unit is left out. A
  dead static is a ruin and is found where it stood.
- A controller keeps its own areas, decides what an entry means, and debounces.
  The platform applies no hysteresis.
- The platform adds no trigger, zone or zone event in DCS.

Rejected:

- A `ZONE` entity kind with per-unit enter and leave events from the sidecar's grid
  and an agent opcode that adds the DCS zone. Zones cannot be removed or changed, so
  a change would leave an old zone in DCS until the next load.
- Event 35 as a source of crossings. It names the group, never a unit or a place, and
  it needs a trigger for each zone.

## Consequences

- A controller that wants enter and leave events polls `QueryArea` and compares sets.
  Its accuracy is the position period ([sampling](../../spec/sampling.md)).
- The zone experiment is recorded in
  [the world-command results](../results/2026-11-15-s15-world-commands.md), "Trigger
  zones".
