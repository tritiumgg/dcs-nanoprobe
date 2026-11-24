# ADR 0023: The agent classifies a gone row as DEAD, REMOVED or MISSING

## Status

Accepted

## Context

The sidecar must tell a death from a removal. A death is a fact the platform cannot
undo. A removal by another script is drift. The respawn rule depends on the difference
([ADR 0024](0024-dead-member-never-respawned.md)).

DCS gives three signals, and none is enough alone. All are LIVE on DCS 2.9.31.29434,
on a Windows client and on the server under Wine
(docs/results/2026-11-14-e38-life-and-ammunition.md,
docs/results/2026-11-18-ruin-oracle-late-death.md):

- A death event reaches the handler up to 103 s after the object stopped existing
  (docs/results/2026-11-16-clear-debris.md says 10 to 103 s). A scripted removal sends
  none. A cargo static killed by gunfire sends none. A death event counts only when
  its initiator category is unit, static or cargo
  ([spec/records.md](../../spec/records.md)).
- A killed ground unit keeps answering `Unit.isDead` and `Unit.getLife` for as long as
  it was watched (325 s). A removed one raises on both.
- The target's `isExist` is already false when a fatal hit, a BDA or a KILL event is
  delivered.
- A script that sets a burning or dead unit's life revives it with the same object ID
  and no event.

Aircraft and ships raise on both reads within seconds of a kill, so a raise says
nothing about a removal. A ship on Wine never read dead (0 of 5).

## Decision

When an existence check finds a row gone, the agent reads the object once, in the same
pump, and writes `OBJECT_GONE` with reason `DEAD`, `REMOVED` or `MISSING`.

- Ground unit: `Unit.isDead` and `Unit.getLife`, each under `pcall`. `DEAD` when
  `isDead` is true and `getLife` is a number. `REMOVED` when both raise. Any other
  result is `MISSING` and counts in `classifier_anomalies`.
- Static: `StaticObject.getLife`. `DEAD` at 0 or below. `REMOVED` when it raises.
  Anything else is `MISSING` and an anomaly.
- Cargo static, aircraft, ship and any other unit: no read is used. The row leaves as
  `MISSING`, with no anomaly.
- A death event (`S_EVENT_DEAD`, `S_EVENT_UNIT_LOST`, `S_EVENT_CRASH`) that names a row
  in the table puts a mark on it and queues its check. The handler only sets the flag.
  A mark over a gone row gives `DEAD` in every class, ahead of the reads. A mark over a
  `REMOVED` read counts a classifier fault. A mark over `MISSING` or an anomaly counts
  an anomaly. A marked row that still exists has its mark cleared and counted in
  `marks_cleared`.
- The agent keeps the object ID, `entity` and `group_entity` of each row that left as
  `MISSING` or `REMOVED`. The first later death event that names it writes a second
  `OBJECT_GONE` with `DEAD`. When the first record said `REMOVED`, that is a classifier
  fault. A generation change clears the memory.
- A `HIT`, `BDA` or `KILL` that names a row, and a position read that fails, only queue
  the check. They are never facts.
- A destroy on a burning wreck leaves the `DEAD` the reads gave standing. The ID stays
  in the dead set.

Rejected:

- Wait for a death event. It can arrive 103 s late or never.
- Read the reads for aircraft, ships and cargo. Their raises on a kill look like a
  removal, and the evidence is too thin.
- Treat `REMOVED` as proof of a removal. A scripted kill that is removed before the
  60 s life sweep reads as `REMOVED`, and a sample cannot tell the two apart.

## Consequences

- `DEAD` is the only death fact. `MISSING` and `REMOVED` both hold the respawn gate. A
  second `OBJECT_GONE` can follow either. One object can have two records in a
  generation, and the second is always `DEAD`.
- The rules for each class are in `crates/agent-link/src/sampler.rs` (`rule`,
  `classify`) and in "What the reads decide" in
  [spec/records.md](../../spec/records.md).
- A killed aircraft, ship or crate gets its death fact from an event. A crate killed by
  gunfire has none and is held.
- The static rule rests on one type, a fuel depot, and few weapon kills (tank fire
  killed 1 of 5 on Windows and 0 of 5 on Wine). Other types are UNVERIFIED.
- Whether a scripted explosion sends `S_EVENT_HIT` is UNVERIFIED (none of 5 did on
  Wine).
- Slung or carried cargo, and whether the position getter follows it, is UNVERIFIED.
