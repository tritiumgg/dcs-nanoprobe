# ADR 0029: Sampling scope: positions, life on hit and slow sweep, no fuel, player ammo

## Status

Accepted

## Context

The agent's time on the sim thread is the main constraint, and every read costs some of
it ([ADR 0001](0001-per-frame-pump.md)). The agent cannot read everything about every
object at a high rate. A controller of a campaign needs to see where units are, how
damaged they are, and what a player flies with.

What DCS gives (LIVE, DCS 2.9.31.29434,
docs/results/2026-11-14-e38-life-and-ammunition.md):

- No read costs half of `Unit.getLife` or less, no bulk read exists, and no signal
  covers what events miss.
- A hit event names its target. Damage with no event, such as a scripted explosion's
  splash on a unit it does not kill, shows only at the next read of life.
- Every fall in ammunition has a shot event. No event named an AI M-1 that a truck
  rearmed.
- A static that cannot move has a fixed position. Cargo can move: slung or carried
  cargo is a static.

## Decision

Positions are periodic for every unit and for cargo. Life is read on a hit and on a slow
sweep. The agent never reads fuel, and reads ammunition only for a unit a player
occupies.

- **Positions.** The agent reads every unit and every static that can move on a
  period, `position_s` (1 s). A row that has not moved is read less often, and the
  first read that shows movement returns it to the full rate. A static that cannot
  move has its position read at birth and at each full enumeration. A static that can
  move is one whose `Object.getCategory` is 6, cargo. Positions are never on demand
  only.
- **Life.** A row `SUBSCRIBE` names is read on every vitals sweep. Any other row,
  aircraft included, is read once in twelve sweeps: the slow sweep. It is also read once
  when the row is added, and after a `HIT` names it as target, no sooner than 1 s after
  its last read. The slow sweep checks `isExist` on each row it visits. A sidecar
  subscribes the units players occupy.
- **Fuel.** The agent reads none. The store and the API carry none, and the proto
  fields are reserved.
- **Ammunition.** The agent reads `getAmmo` once for a unit a player occupies, at its
  birth and at its takeoff, and writes one `AMMO` record with the cause. It has no tier
  and no period. The agent reads no AI unit. A rearm is not read.
- A restore keeps only life: `DAMAGE` after the spawn. It restores no fuel or
  ammunition.

Rejected:

- Positions on demand only. A controller needs the movement of units without asking.
- A periodic ammunition sweep for AI units. Ammunition matters less than health, and
  no cheaper read exists (docs/results/2026-11-14-e38-life-and-ammunition.md). Shot
  events cover the falls.
- Periodic fuel reads. Fuel does not matter for AI units. Dropping it frees sampling
  budget.
- A restore of ammunition or fuel. A restore with state restored them in part only
  (docs/results/2026-11-16-e39-spawn-state.md).

## Consequences

- AI life is as fresh as the last hit or slow sweep (60 s at the default `vitals_s`).
  After a DCS crash, life falls back to the last read.
- Damage with no event waits for the slow sweep.
- A player aircraft's ammunition shows the load at its birth or its takeoff. A rearm
  by ground crew is not read. Whether DCS sends an event for it is UNVERIFIED, and the
  load of a dynamic-spawn slot may be missed (spec/sampling.md, "Loadout reads").
- The `AMMO` record names its cause, birth or takeoff, so a controller can tell which
  load it shows.
- The rules are in [spec/sampling.md](../../spec/sampling.md) ("Tiers", "Loadout
  reads"). The budget that shares these reads is in
  [ADR 0031](0031-sampler-budget-and-pump-gate.md).
