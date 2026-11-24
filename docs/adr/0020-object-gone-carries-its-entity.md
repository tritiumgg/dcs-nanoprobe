# ADR 0020: OBJECT_GONE carries its entity, and the binding mirror stays volatile

## Status

Accepted

## Context

The sidecar binds each DCS object ID to a unit and group `EntityId`. It builds the
binding from `OBJECT` records and from the agent's full enumeration. A death fact comes
from an `OBJECT_GONE` record with reason `DEAD` (spec/world.md, "Lifecycle"). The
sidecar resolves the object ID in the record through its binding.

The binding lives in the sidecar's memory. After a restart the sidecar builds a new
one from the store and the agent's enumeration. The agent queues records while no
sidecar is attached and writes them after the next attach. A death in that queue can
reach a sidecar whose binding does not hold the object, and the binding cannot name
the entity.

The outbound read position lives in the mapped file, not in the store, and the store
can be off (spec/config.md). A durable copy of the binding in the store cannot follow
the read position.

## Decision

`OBJECT_GONE` carries the entity: `entity` and `group_entity`, each a `u64`, copied from
the agent's row before it removes the row. The binding mirror stays in memory.

- `entity` is the `EntityId` the binding held for the object when the row left, 0 when
  it held none. `group_entity` is the same for the object's group
  (spec/records.md, "Layout: OBJECT_GONE").
- The agent copies both fields from the row before it removes it, so a death queued
  while no sidecar is attached names its entity when it is written, with persistence on
  or off.
- The sidecar takes the identity of a gone object from the record when `entity` is not
  0. The record must name a unit of the group it names, or a static with no group.
  Otherwise it counts the record as a bad binding and changes nothing. When `entity` is
  0 it uses its binding.
- A record naming a unit the store purged is a late death of that unit. The
  sidecar drops and counts it (`purged_deaths`).
- The reconciler's own state, the binding mirror included, is volatile. After a restart
  it re-reads the binding from the agent's enumeration and resolves unanswered commands
  at the agent's next `HELLO`.

Rejected:

- A durable binding mirror in the store. The read position is in the mapped file, and
  persistence can be off, so the mirror would not match what the sidecar re-reads.
- A durable last `FRAME` stamp. Restart points are `FRAME` boundaries or the tail, and
  a re-read record is dropped by its number (spec/transport.md, "Consuming").

## Consequences

- `OBJECT_GONE` is 32 bytes.
- A death queued while the sidecar was away names its entity, so the replay needs no
  binding from before the restart.
- The agent's binding can hold an entity the store no longer has, such as a purged
  unit. The sidecar checks the record's entity against the store.
- The replay property is tested with persistence off and with a death queued while the
  agent is detached (docs/results/2026-11-13-s12-sidecar-core.md).
- A record with `entity` 0 falls back to the sidecar's mirror. A record of another
  campaign does not count at all ([ADR 0019](0019-the-campaign-is-a-stamp.md)).
