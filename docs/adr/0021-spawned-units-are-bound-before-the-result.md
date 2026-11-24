# ADR 0021: A spawned group's units are bound before its RESULT

## Status

Accepted

## Context

A group is `live` when its `GROUP_SPAWN` result is `APPLIED`. A unit is bound when the
sidecar has read its `OBJECT` record. Calls on a unit need it bound: a damage effect or
a capture on an unbound unit is refused (spec/world.md, "Bound units").

The births tier writes a unit's `OBJECT` record after it resolves the unit's birth
event. The birth event arrives in the spawn's frame (LIVE,
docs/results/2026-11-17-s20-virtualization.md), so the object exists then. The births
tier resolves births at 26 to 34 a second on a dedicated server under Wine (LIVE,
docs/results/2026-11-17-bind-lag.md). A restore that spawns faster than that builds a
backlog, and a unit waits for the births ahead of it. Without the reads below, a server
test of 2,740 spawned units had a bind lag of 6.5 s at the median and 25.3 s at most,
and effects aimed at unbound units were refused (LIVE, same document).

spec/commands.md says an `OBJECT` record follows each unit of a spawned group.
spec/records.md says `enumeration` is 0 when an event or a spawn made the record.

## Decision

The `GROUP_SPAWN` handler reads each unit of the new group that exists, and the agent
writes the `OBJECT` record of each in the spawn's frame, before the `RESULT`.

- On `APPLIED`, the handler reads each unit with the births tier's guarded read, each
  under `pcall`, and the agent writes the `OBJECT` records with `enumeration` 0
  (spec/commands.md, "Layout: GROUP_SPAWN").
- On a replacing spawn, the new `OBJECT` records come before the `OBJECT_GONE` of the
  old rows. The reconciler handles that order for every gone reason: a `DESTROYED` for
  an old ID changes nothing, and `MISSING` and `REMOVED` for one count as stale.
- A unit that does not exist yet, or whose read raises, gets no record. The result stays
  `APPLIED`. The unit binds at its birth or at the next full enumeration.
- The reads are part of the command's `cost_us` and count as commanded work, not agent
  overhead. A spawn past `commanded_ms` is counted in `commands_over_budget`.
- A later birth of a unit the handler added costs the births tier one `wants` check and
  writes nothing.
- `live` keeps its meaning: the spawn was applied. spec/world.md, "Bound units", says
  which units are bound at that point and what a call on an unbound unit does, per call.
- Births resolve oldest first, so a row trails its birth by the births ahead of it only
  (spec/sampling.md, "Tiers").

Rejected:

- Redefining `live` to mean every unit is bound. It needs a timeout rule and saves no
  time.
- Raising the births' rate in the sampler, by a larger share or larger births batches.
  Changes to the sampler's rules are a separate decision.

## Consequences

- With the reads, the same server test bound every one of 2,740 units before its
  spawn's result and refused no effect (LIVE, docs/results/2026-11-17-bind-lag.md).
- A unit read inside the handler right after `coalition.addGroup` is LIVE for one-unit
  ground groups. Groups of several units, aircraft, ships and late-activated groups are
  UNVERIFIED. If the read fails for them, the rule stands: it reads what exists, and the
  lag is the oldest-first figure.
- `STATIC_SPAWN` has no such reads. A static binds when its birth resolves.
- A `GAP` of events that drops a record forces a full enumeration, which writes the
  unit's `OBJECT` (spec/records.md, "Delivery classes").
