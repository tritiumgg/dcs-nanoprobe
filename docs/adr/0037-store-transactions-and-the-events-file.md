# ADR 0037: The store commits once per driver step, beside an events file and a lock

## Status

Accepted

## Context

The sidecar's store must satisfy three rules. A command is on disk before it reaches
DCS. A result read from DCS is on disk before the agent learns it was read. A change
the API acknowledged survives a sidecar crash.

The store also grows. A restart that read the whole journal to find the commands owed
a result would cost more as a campaign ages. A backup that copied the event history
whole would multiply it by `backup_keep`.

Two sidecars on one store directory would corrupt it.

## Decision

The driver commits at most once per step, a table of commands in flight is the
source of what is owed, events live in their own SQLite file, and an
exclusive OS lock keeps a second sidecar out (spec/persistence.md, "Writes").

- During a step the reconciler decides its commands and writes them to the inbound
  ring, held back from the agent. At the end of the step the sidecar commits the
  events file, then the store, then lets the held commands through and acknowledges the
  records it read. An API call that changes a store commits the same way before it
  answers. There is no writer thread.
- A commit writes the journal entries since the last commit, and only the group, unit
  and extension rows whose spec, order, lifecycle, authority, policy, error, death,
  mark or object ID changed. Positions and life are written only by the checkpoint,
  every `checkpoint_s`. Warehouse desired counts go with the commit that changes them.
- `inflight` holds one row per command sent with no outcome. It is inserted in the
  transaction that journals the command and deleted in the one that journals the
  outcome. A restart reads `inflight`, the entities and the checkpoint row, and never
  reads the journal.
- `events.sqlite` holds `event(seq, wall_ms, source, body)` and a `meta` table, in WAL
  mode with `synchronous = NORMAL`. `source` names the agent record an event came
  from, so a crash between the events commit and the store commit never logs an event
  twice. The server event log uses the same layout.
- Backups run on a worker thread through their own connection. They copy the store
  whole and extend one shared copy of the events file.
- At start the sidecar takes an exclusive lock on `<store dir>/lock` without waiting
  (`flock` on Linux and macOS, `LockFileEx` on Windows) and refuses to start without
  it. The lock ends with the process.

Rejected:

- Reading the journal at start to find owed commands: the cost grows with age.
- Copying the event history into each backup: `backup_keep` would multiply it.

## Consequences

- Each fact is folded in once and each event recorded once, whatever the crash point
  (spec/persistence.md, "A crash between the two commits").
- A write that fails stops the sidecar through its panic path. It never lets a command
  through that it could not journal
  ([0034](0034-stopping-server-refuses-changing-calls.md)).
- A power loss may lose the last transactions, never corrupt a file.
- Sampled state is up to `checkpoint_s` old after a crash and is read again from the
  agent.
- The journal is an audit trail the sidecar never reads back, so its retention cannot
  break a restart ([0038](0038-sidecar-growth-is-bounded.md)).
- The lock covers one store directory. Two sidecars may run with two directories.
