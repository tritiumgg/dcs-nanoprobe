# ADR 0022: `materialize_per_frame` bounds unanswered spawns

## Status

Accepted

## Context

A controller moves groups into and out of DCS as a point moves across a large campaign.
Each materialization is a `GROUP_SPAWN` or a `STATIC_SPAWN`, and each costs agent time
on the simulation thread. A spawn is a command, and the agent applies commands in ring
order under the commanded-work budget (spec/commands.md, "Budget").

The sidecar writes commands to the inbound ring. The ring can hold commands ahead of the
spawns, such as the destroys of groups that leave DCS in the same step. A limit on how
many spawns the sidecar writes per step does not limit how many reach DCS in one frame,
because the agent reads the ring in order and the spawns can queue behind other
commands. The agent publishes `out_tail` once a frame, so the sidecar sees each frame's
results together (spec/transport.md, "Producing").

The restore burst after a mission load has its own rule. It runs while the simulation is
paused, spawns every waiting group and turns the budget off (spec/persistence.md,
"Restore burst").

## Decision

Outside the restore burst, at most `materialize_per_frame` spawns are unanswered at a
time. The agent needs no change.

- A spawn is unanswered from the write to the ring until the sidecar reads its `RESULT`,
  or until the spawn resolves `unknown` or `not_sent`.
- The sidecar writes the next spawn in the step that reads a result.
- Because the agent publishes `out_tail` once a frame, at most that many spawns reach
  DCS in one frame, whatever waits in the ring before them.
- A larger value pipelines spawns. The default is 1 (spec/config.md, `[world]`).
- Destroys have no such limit. The agent's commanded-work budget (`commanded_ms`)
  spreads them over frames like any command.
- The burst turns on while paused, when more groups are `materializing` than the
  budget, as a mission load leaves every live group (spec/world.md, "Materialization
  budget").

Rejected:

- Counting spawns written per step. Spawns queued behind other commands in the ring
  would reach DCS together, more than one frame's share.

## Consequences

- Spawn throughput is at most `materialize_per_frame` per round trip of a result. The
  rate a server reaches is lower than one spawn per frame. On a server, spawns came at
  a median of one every 2 frames, with destroys ahead of them and with none: the pace
  is the round trip of a result, not the ring position (LIVE,
  docs/results/2026-11-17-s20-virtualization.md).
- The host test in `crates/sim-fake/tests/virtualization.rs` writes capture-and-destroys
  ahead of the spawns. No frame outside the burst takes more spawns than the setting.
- The server run of 500 groups kept of 10,000 had at most one spawn per frame (LIVE,
  docs/results/2026-11-17-s20-virtualization.md). The fake sim has no agent, so the
  server runs are the evidence for the agent side.
- The bound depends on the agent storing `out_tail` once a frame (spec/transport.md,
  "Producing"). A change that stores it more often moves the bound.
