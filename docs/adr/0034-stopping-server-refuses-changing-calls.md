# ADR 0034: A stopping server refuses changing calls

## Status

Accepted

## Context

The API server stops on a shutdown signal and after a panic in a driver step, a
changing call or a lease call. After a panic the in-memory state may be half changed.
A call that ran then could write a change the sidecar never meant to keep.

Open streams never end by themselves, so a graceful stop would wait on them.

## Decision

Once the server is stopping, every changing call and every lease call is refused with
`UNAVAILABLE` and no reason, and every open stream ends with the same status.

- The check happens under the hub lock (`Hub::write` and `Hub::guarded` in
  `crates/api/src/hub.rs`), so no call runs after a panic marks the hub as closing.
- A panic in a changing call, a lease call or a driver step poisons the hub: it
  closes, and the driver returns an error to the process.
- A driver panic gives open streams a quarter of the shutdown grace to end with
  `UNAVAILABLE` before the server returns its error (`crates/api/src/server.rs`).
- A panic inside a changing call keeps the call's idempotency key as running, so a
  retry never runs the call twice.
- The client connects again and resumes from its last revision or sequence number
  (spec/errors.md, `UNAVAILABLE` with no reason).

Rejected:

- Serving on after a panic: the state may be half changed, and a later call could
  write on top of it.
- Dropping the connections with no status: a consumer would not know whether to resume
  or to stop.

## Consequences

- A call refused this way changed nothing, and the client may send it again after the
  server returns.
- A store write that fails takes the same path: the sidecar stops and never answers a
  call whose change is not on disk (spec/persistence.md, "Writes").
- The call that panicked answers `INTERNAL`. Calls that arrive after it get
  `UNAVAILABLE`.
- A supervisor must restart the process. The server does not restart itself.
