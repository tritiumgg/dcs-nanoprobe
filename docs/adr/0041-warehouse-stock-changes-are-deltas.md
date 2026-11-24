# ADR 0041: Warehouse stock changes are deltas

## Status

Accepted

## Context

A limited warehouse changes under players. A spawn takes the airframe, each loaded
store and the full internal fuel load. A deslot on the ground returns them. A rearm
and a refuel change the counts again (LIVE,
[warehouse player deltas](../results/2026-11-16-warehouse-player-deltas.md);
[refuel](../results/2026-11-16-person-tests-2.md)). The rearm was seen only in the
net change, not on its own.

A controller that wants to add 20 bombs to a base cannot know the count it will meet.
An absolute set from a stale read would erase what players took or returned since the
last sample. A set must still exist for a restore, which has a target count.

## Decision

`AdjustWarehouseStock` adds to or takes from the counts and is an effect: applied at
most once and never sent again. `SetWarehouseStock` serves restore and a
controller that has a target.

- The command is `WAREHOUSE_ADJUST` with the airbase's DCS name and a delta, in the
  shape of `WAREHOUSE_SET`'s stock (spec/commands.md, "Layout: WAREHOUSE_ADJUST").
- A positive amount calls `addItem` or `addLiquid`. A negative amount calls
  `removeItem` or `removeLiquid`. Taking more than a limited warehouse holds leaves 0,
  with no error (LIVE, see
  [world commands](../results/2026-11-15-s15-world-commands.md)).
- An applied adjust changes the store's desired counts for the items it names that
  the controller set, clamped at 0 (spec/world.md). It is never sent again.
- A call that raises stops the handler there. Items before the raise stay changed and
  the answer is `DCS_REFUSED`. The sidecar takes those changes from the next sample as
  facts.

Rejected:

- Compare-and-set against the 60-second sample: it fails on every rearm, since the
  count changes between the sample and the call.
- Sending the delta again after a lost result: it could apply twice. A lost result is
  `unknown`.

## Consequences

- A lost answer leaves the outcome unknown. The controller reads the next sample to
  find out.
- The absolute set and the delta share one desired count in the store, so an adjust
  followed by a set ends at the set's count.
- Whether gun rounds, flares and chaff are warehouse items is UNVERIFIED.
