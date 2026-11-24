# ADR 0024: A dead member is never spawned again; unexplained members hold the respawn

## Status

Accepted

## Context

A managed group goes `lost` when DCS no longer holds its members and a full enumeration
confirms it. The author's `on_lost` policy may be `respawn`
([spec/config.md](../../spec/config.md)). A respawn of a group with a dead member would
bring the dead back. A respawn of a group that a script removed is a different act
from a respawn of one that was shot down, and the platform often cannot tell them apart
([ADR 0023](0023-gone-row-classifier.md)):

- A weapon kill can leave as `MISSING`, with the death event up to 103 s later (LIVE,
  [docs/results/2026-11-16-clear-debris.md](../results/2026-11-16-clear-debris.md)).
- A scripted kill followed by a scripted destroy before the life sweep reads as
  `REMOVED`.
- A tank group with one tank killed, or a SAM group with its radar killed, is never
  `lost` while other members live. After a reload it returns without the dead.
- After a mission generation change, no death event can arrive for an object of the old
  generation (UNVERIFIED: that the agent's memory of rows that left ends with the
  mission; the fake sim models it so).

## Decision

A member with a death fact is never spawned again, and a `lost` group is not respawned
while a member that is not found has left as `MISSING` or `REMOVED` with no death fact.

- The gate holds on a mark from this generation, or on one the agent reported in an
  `OBJECT_GONE` in an earlier generation. No timer and no enumeration opens it.
- A death fact for the member releases its hold. The spawn leaves the dead member out
  and spawns the survivors. A member found again holds nothing. A revived member is
  bound again and holds nothing.
- A generation change keeps every reported mark and resets only the member's DCS object
  ID. A mark the sidecar inferred at an enumeration's end, with no `OBJECT_GONE`, is
  cleared, because it can name a member DCS never bound.
- Spawns and restores skip held members as they skip dead ones. A held member is never
  the lead of an order.
- A group whose members are all dead or held, with at least one held, goes from
  `materializing` to `lost` and stays there. Only death facts make a group `dead`, or
  the author's `dead` lost policy.
- Only the controller releases a hold: `RemoveEntity` drops the group's marks, and
  `ResolveMember` with `DEAD` or `RESTORE` releases one member. The API shows the mark
  as `Unit.left`.

Rejected:

- Open the gate after a timer. A death event can arrive up to 103 s late (LIVE,
  [docs/results/2026-11-16-clear-debris.md](../results/2026-11-16-clear-debris.md)), and
  none arrives for a crate killed by gunfire.
- Hold a group for the generation because one member died. The rule is that a dead
  member is never spawned again, not that a group with a killed member is never
  respawned.
- Clear all marks at a generation change. A script-removed group would come back
  whole, including members that were killed.

## Consequences

- A group that a script removed comes back short, or not at all, until the author
  acts. The policy then acts as `report`.
- A group where only some members are held stays `lost`, and the API reports its held
  members. The reconciler counts held groups in `respawns_held`.
- Marks persist in the checkpoint and the journal and survive a sidecar restart.
- The full rule is in "Respawn gate" in [spec/world.md](../../spec/world.md). The gate
  is `gate_shut` in `crates/sidecar/src/store.rs`, used by
  `crates/sidecar/src/reconciler.rs`.
- A member that no death event ever explains, such as a crate killed by gunfire, holds
  its group until the controller acts.
