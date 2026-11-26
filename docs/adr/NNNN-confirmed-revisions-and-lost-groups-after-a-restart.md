# ADR NNNN: Confirmed revisions and lost groups after a restart

## Status

Accepted. Refines [ADR 0017](0017-set-state-applied-means-dcs-holds-the-revision.md)
for spawns and [ADR 0024](0024-dead-member-never-respawned.md) for groups lost to failed
destroys.

## Context

The reconciler spawns a `live` group or static again in place when its `spec_rev` is
ahead of the revision its last applied spawn carried. It kept that revision only in
memory. A fresh reconciler took the stored `spec_rev` as applied, so a revision made and
not yet spawned before a sidecar restart was not sent until the next mission generation.

The respawn policy respawns a `lost` group once a full enumeration used since the last
`HELLO` finds none of its objects. A group goes `lost` when its destroys are refused or
unknown to the limit. The property search found a static whose object a script removed
while a foreign campaign held the agent, so no `OBJECT_GONE` named it. The sidecar restart
resolved its destroys in flight `unknown`, the static went `lost`, and the policy
respawned it on an enumeration made while it was still `dematerializing`. Its member had
left DCS unexplained, which the respawn gate exists to stop.

A ruin's spawn answered `AT_REVISION` is `applied`. The agent's answer names no object,
but the reconciler took any object in an `applied` result as the ruin's, and the ruin's
object is exempt from the drift destroy. A result naming a revived live object would have
left it in DCS.

## Decision

- The store keeps, with `spawned_in`, the highest spec revision a spawn of the entity
  resolved `applied` at in that agent boot and generation. The plan compares `spec_rev`
  with it; an `AT_REVISION` answer to the spawn in place confirms it. A body written
  without the revision, with a last spawn, counts the `spec_rev` it was stored with as
  confirmed, as the in-memory default did at its first read.
- The respawn of a group lost to failed destroys waits for a full enumeration that
  began after it went `lost` and was used. A full enumeration's end marks missing each
  member of a `lost` group that the agent's table does not hold, as it does for a `live`
  group. The mark clears at the generation change.
- Only an `APPLIED` result of a ruin's spawn names the ruin's object.

## Consequences

- A revision is owed until DCS confirms it, across sidecar restarts.
- A group lost to failed destroys whose member left with no record is held by the gate
  in that generation. So is one whose destroy ran with its result lost: its respawn waits
  for the next generation.
- The fake sim has a fault switch that names the object in an `AT_REVISION` answer.
