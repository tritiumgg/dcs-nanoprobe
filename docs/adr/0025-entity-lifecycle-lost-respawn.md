# ADR 0025: A lost group respawns at a new spec revision after an enumeration

## Status

Accepted

## Context

The sidecar keeps a lifecycle state for each managed group and static
([spec/world.md](../../spec/world.md), "Lifecycle"). DCS reports some changes late, or
not at all, so the states must stay correct when facts arrive in an unplanned order:

- A death fact can arrive while a spawn's result is lost (`materializing`), during a
  capture-and-destroy, or after the group went `lost`. A group with every member dead
  must never be spawned again.
- The agent holds the applied spec revision of each entity for the whole mission
  generation. A spawn at a revision it already applied is answered `AT_REVISION`,
  which is `applied`, and spawns nothing (spec/errors.md). A respawn at the old
  revision would do nothing.
- While paused, the agent's destroy reports `APPLIED` before DCS lets go of the object
  ([ADR 0001](0001-per-frame-pump.md); LIVE,
  docs/results/2026-11-11-server-gate.md). A destroy can also be refused or lost.
- Removals by scripts send no event, and events can be lost in a `GAP`. The sidecar's
  binding mirror can lack objects DCS still holds until a full enumeration reads them.

## Decision

A `lost` group respawns only after a full enumeration shows DCS holds none of it, and
the respawn is a new spec revision.

- `lost` becomes `materializing` when the author's policy is `respawn`, the authority
  is DCS, the respawn gate is open ([ADR 0024](0024-dead-member-never-respawned.md)),
  and a full enumeration finds no object of the entity. The enumeration must be one
  used since the sidecar started, since the last `HELLO` and since the last `GAP` of
  events.
- The transition bumps `spec_rev` even when the spec is the same. The bump and the
  transition are one journal entry, written before the spawn is sent. A re-send while
  `materializing` keeps the revision.
- `dead` is reachable from `live`, `materializing`, `dematerializing`, `lost` and
  `virtual` when the last member's death fact arrives. `virtual` to `dead` cannot
  happen in order and is listed to keep the rule total. A spawn not yet written is
  never written. One already sent resolves `applied`.
- `dematerializing` lasts until the agent's `APPLIED` for the destroy, or `NOT_BOUND`.
  A capture-and-destroy that is `refused`, `unknown` or `TOO_LARGE` falls back to
  `GROUP_DESTROY`. A refused or unknown destroy is sent again, up to `destroy_retries`
  ([spec/config.md](../../spec/config.md)). Then the entity goes `lost` with the error.
- `unspawnable` becomes `virtual` when the author changes the spec. The error is
  cleared. It then materializes by the level condition.
- `order_rev` also changes when the target of a sent or applied attack order gets new
  DCS object IDs. It does not change on a generation change, after which every order
  is sent again.

Rejected:

- Destroy the group, then spawn it again. It adds a command, and destroys stay pending
  while paused.
- Respawn at the same revision. The agent answers `AT_REVISION` and spawns nothing.
- A durable binding mirror. Persistence can be off, and the read position lives in the
  mapped file, not the store.

## Consequences

- `spec_rev` can change with an identical spec. API clients that watch it must expect
  that.
- A `lost` group waits for an enumeration, so a respawn takes one enumeration cycle
  at least. A `GAP` resets the wait.
- Set-state commands are judged per revision: `applied` means DCS holds that revision
  in this generation. A resend resolves `applied` when a member's death or
  `AT_REVISION` shows it.
- A destroy of an entity DCS should not hold is tried again after the next full
  enumeration. The limit is `destroy_retries`.
- The reconciler code is in `crates/sidecar/src/reconciler.rs` (the `State::Lost`
  arm) and `crates/sidecar/src/store.rs` (`respawn`). The transition table is
  `crates/sidecar/src/lifecycle.rs`.
