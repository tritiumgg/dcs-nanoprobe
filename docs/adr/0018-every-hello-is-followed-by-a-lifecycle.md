# ADR 0018: Every HELLO is followed by a LIFECYCLE

## Status

Accepted

## Context

A sidecar learns the world from the records the agent writes. A `FRAME` carries the
mission generation, the campaign and the lifecycle state, but no theater and no
mission name. A `LIFECYCLE` record carries both, and the agent writes one when the
state changes.

A sidecar that starts with no store has not seen the `LIFECYCLE` of a mission that
loaded before it attached. It reads `HELLO` and then waits for a change. A mission that
is already running never produces one. The same holds after a resync, when the agent
lost a control record and writes a new `HELLO` (spec/records.md, "Delivery classes").

`HELLO` is a barrier: the agent writes it only when its queues are empty, so every
record made before it is before it in the ring.

## Decision

After every `HELLO`, the agent writes a `LIFECYCLE` of its current state. It writes the
pair back to back under the same stamps, and writes both or neither.

- The record carries the state, the mission generation, the theater and the mission
  name. The theater and the mission are empty when none is loaded.
- The order after an attach is: the queued records, `HELLO`, `LIFECYCLE`, then the
  `ENUM_BEGIN` of the enumeration the attach starts (spec/transport.md, "Attaching").
- The pair has no `FRAME` between its records. If both do not fit the ring, the agent
  rolls back its write position, keeps `hello_due` set and reads no commands. It tries
  again on the next frame.
- The rule covers the resync `HELLO` after `CONTROL_LOST`.
- The closing `LIFECYCLE` of the kill switch or a caught panic has no `HELLO` before
  it. The agent writes no `HELLO` on the closing path.
- The sidecar changes nothing when the `LIFECYCLE` repeats a mission its store holds:
  no journal entry, no new generation.

## Consequences

- A fresh store gets `FRAME`, `HELLO`, `LIFECYCLE`, and becomes ready with its bindings
  kept.
- Both records keep their layouts. The golden file has no pair rule.
- A ring with room for `HELLO` and not for `LIFECYCLE` delays the attach by a frame. The
  agent reads no commands meanwhile.
