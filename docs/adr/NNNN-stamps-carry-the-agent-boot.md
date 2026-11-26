# ADR NNNN: Stamps carry the agent boot

## Status

Accepted.

## Context

The API's `Stamp` had wall time, model time, mission generation and frame. The
generation and the frame start over in each DCS process, and the wall time is an
estimate ([records](../../spec/records.md), "Stamps"). So two stamps from two DCS
processes could be equal, and a controller that keyed anything on the generation and
the frame got a false match after a DCS restart. Inside the sidecar every check already
pairs the agent boot with the generation, and `RemovePlayerMark` checks both through the
event's source key.

## Decision

- `Stamp` gains `agent_boot`, set by the server: opaque, compared for equality only,
  with no order. 0 means no agent boot is known: no agent attached to the sidecar
  process since it started or restarted, or an event body stored without it and not
  made from an agent record. While attached, a stamp takes the boot of the record it was
  read with. A 0 is never the same run as another stamp.
- The agent boot and the mission generation together name one mission run. `frame` and
  `model_time` order stamps only within one run. No request takes a `Stamp` today; one
  that does checks the boot together with the generation.
- The API server builds every `Stamp` through one helper that takes the boot. A test
  rejects a `Stamp` struct literal anywhere else in the crate.
- An event made from an agent record takes the boot of its source key, when it is logged
  and when it is read back, never the boot of the agent running at the read.
- A checkpoint keeps the agent boot of the generation it was taken in, for its stamp.

## Consequences

- A controller tells a DCS restart apart from any stamp, with no `Lifecycle` watch.
- The field is additive and passes the breaking check. Event bodies and checkpoints
  written before it read 0, except events with a source key, which read its boot.
- No agent or transport change: the boot is in the status block already.
