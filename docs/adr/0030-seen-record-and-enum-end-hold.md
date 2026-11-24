# ADR 0030: The compact SEEN record, and ENUM_END held until misses resolve

## Status

Accepted

## Context

A full enumeration walks every group and static in DCS. It tells the sidecar which rows
exist, so the sidecar can find objects that left without an event, and groups that are
`lost` ([spec/sampling.md](../../spec/sampling.md), "Full enumeration").

Two forces shape the records:

- Most rows are unchanged between enumerations. An `OBJECT` record per row, for every
  enumeration, repeats data the sidecar already holds.
- An enumeration is sliced over frames, and the world changes while it runs. A row the
  walk did not find may be gone, or may be alive and missed because the walk reached
  its group earlier. Without a rule, the sidecar reads every miss as gone at
  `ENUM_END`, and a `lost` group can be respawned over live units
  ([ADR 0025](0025-entity-lifecycle-lost-respawn.md)).

Records and events can also be lost. The ring can drop an event-class record, and the
sidecar then holds a mirror that no `OBJECT` corrects.

## Decision

An enumeration reports a row it found with a `SEEN` ID when the row's last `OBJECT` is
still true, and the agent writes `ENUM_END` only after each miss has an answer.

- `SEEN` (kind 25) holds the DCS object IDs of one batch. It means the row is in the
  table, and every field of its last `OBJECT` queued in this attach is what an `OBJECT`
  would say now. It carries no data to add a row, so a `SEEN` never adds one.
  `ENUM_END` has a `seen` count beside `objects`, and the sidecar compares each
  separately.
- The agent writes a full `OBJECT` when the agent's one sticky `full` flag is set, when
  the row's encoded payload is empty (a `BIND` or a campaign change clears it), or when
  no `OBJECT` for the row was queued since the last `HELLO`. `ENUMERATE`, every `HELLO`
  and every dropped event-class record other than a `LOG` set the flag. The next
  `ENUM_BEGIN` takes it.
- A miss is a row in the table at `ENUM_BEGIN` that the enumeration did not find. The
  agent checks each one first ([ADR 0023](0023-gone-row-classifier.md)). Gone gives
  `OBJECT_GONE`. Alive, or a check that cannot decide, gives an `OBJECT` with the
  enumeration's number. Then it writes `ENUM_END`. The next `ENUM_BEGIN` waits.
- A row added after `ENUM_BEGIN` is never a miss. Misses take the urgent check path,
  ahead of slow-sweep checks.
- A generation change, a detach, a new `HELLO` or a sidecar restart drops the hold with
  no `ENUM_END`. A hold that lasts one enumeration period is abandoned with no
  `ENUM_END` and counted in `enum_end_abandoned`. The sidecar removes nothing for it.
- An open `SEEN` batch is queued before any other event-class record in the same pump,
  and every `SEEN` of an enumeration comes before its `ENUM_END`.
- The sidecar uses an enumeration only when its counts match and no `GAP` fell between
  the begin and the end. A `SEEN` outside the open enumeration, or of an older
  generation, is ignored. A `SEEN` ID the mirror does not hold refuses the
  enumeration. After a `GAP`, it refuses every enumeration with `seen` above 0 until one
  with `seen` 0 applies.

Rejected:

- A `SEEN` rule keyed on "started by `ENUMERATE`" or on a `GAP` since the last
  `ENUM_END`. The enumeration triggers share booleans, and a loss during enumeration N,
  or one whose `GAP` is unwritten, would pass.
- Let a dropped `LOG` set `full`. A `LOG` cannot leave the mirror or a binding stale.

## Consequences

- `SEEN` saves little. The saving was 0.15 µs of 6.2 µs an item (LIVE,
  docs/results/2026-11-14-s11-budget-and-baseline.md): the cost is DCS's own calls and
  the shim's, not the record. A `SEEN` ID is 8 bytes. Its other gain,
  less ring traffic per enumeration, is UNVERIFIED: no run measured it.
- The hold counts in the enumeration's duration, which the period bound includes. A
  group whose `getUnits` failed stays `live`, never `lost`.
- The layouts are in [spec/records.md](../../spec/records.md) ("Layout: SEEN",
  "Layout: ENUM_END"). The kind and the `seen` word are in `spec/transport.golden`.
