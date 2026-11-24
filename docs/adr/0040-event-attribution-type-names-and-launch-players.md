# ADR 0040: Events carry type names and a separate launch attribution

## Status

Accepted

## Context

A consumer of events needs to know what a unit was and who fired a weapon. Both are
hard to read from the event alone.

A death event may carry no usable object. A weapon kill's `KILL` comes with the
late death event, which arrives up to 103 s after the victim stops existing, when the
victim and maybe the killer have left the agent's table (LIVE,
[clear debris](../results/2026-11-16-clear-debris.md); 77.1 s in
[life and ammunition](../results/2026-11-14-e38-life-and-ammunition.md)). The player
who fired a weapon is the occupant of the shooter's unit at `S_EVENT_SHOT`, which may
be long before the hit.

DCS puts the unit's type name in `initiator_data.unit_type` and
`target_data.unit_type` on every `DEAD`, `HIT` and `KILL` of the catalogue, scenery
included (LIVE, [event catalogue](../results/2026-11-13-event-catalogue.md)).

## Decision

Events carry `initiator_type` and `target_type`, and a `HIT` or `KILL` carries
`launch_players` and a `launch_rule` beside `initiator_players`, which is never
rewritten.

- The agent reads the two type names from the plain fields `initiator_data.unit_type`
  and `target_data.unit_type` as event field codes 19 and 20. The API reports them as
  `initiator_type` and `target_type`, from the mission's mirror when the event has
  none (spec/records.md).
- At `S_EVENT_SHOT` the sidecar maps the weapon's object ID to the shooter's
  occupants. A `HIT` or `KILL` that names that weapon carries those player keys in
  `launch_players`, with the rule `WEAPON`.
- A gun's hit names no weapon object (UNVERIFIED), and a weapon the sidecar never
  saw shot is unknown. Both fall back to the initiator's occupants before it left,
  with the rule `INITIATOR`.
- The sidecar resolves a late event's object IDs from a per-generation memory of the
  rows that left, which holds the type and the player keys that occupied each one
  (spec/world.md, "Kill attribution", "Launch attribution").

Rejected:

- Rewriting `initiator_players` with the launcher: the field says who occupied the
  initiator at the event, and a consumer would lose that fact.
- Reading the type name from the event's object: the event handler never calls a
  method on an event object, and the object may be gone.

## Consequences

- A consumer reads one more field set per event, and `launch_rule` tells it how far to
  trust `launch_players`.
- That `SHOT` and `KILL` carry the same weapon object ID is UNVERIFIED.
- A scenery target's position is not in an event. Whether a dead scenery object
  answers a position read is UNVERIFIED.
