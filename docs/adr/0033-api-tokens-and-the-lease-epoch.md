# ADR 0033: API tokens are stored as hashes, and the lease epoch never goes down

## Status

Accepted

## Context

The API needs a way to tell callers apart and to stop a stale controller from acting.
Two facts shape both.

A tokens file sits on disk next to the sidecar. A leaked file must not hand out
working tokens. The sidecar can then not know what a token looks like, only its hash.

A controller can pause, lose its lease, and return after another controller took
over. The sidecar may also restart, with or without a store. The fencing value must
still rise, or an old lease could match a new one.

## Decision

The tokens file holds the SHA-256 of each token and a role, and every lease grant has
an epoch that is above all earlier ones, from a clock when no store remembers them.

- The file is TOML with one `[[token]]` per caller: `name` (for logs), `role`
  (`observer`, `controller` or `admin`) and `sha256` (64 lower-case hex digits). An
  unknown key or role, a malformed hash, or two entries with one hash stop the sidecar
  at start (spec/config.md, "Tokens file").
- A caller sends `authorization: Bearer <token>`. The sidecar hashes it and compares
  the hash with every entry in constant time, with no early exit.
- A token is at least 32 printable ASCII characters with no spaces. The sidecar
  refuses a shorter presented token with `TOKEN_REQUIRED`. 64 hex digits from 32
  random bytes, the form the installer uses for the agent's handshake token, is the
  intended form. The operator makes each API token and puts its hash in the file.
- The lease epoch is the larger of the last epoch plus one and the wall clock in
  milliseconds. It also never falls below the clock reading taken when the server
  started. It therefore grows across sidecar restarts with no store
  (`crates/api/src/lease.rs`).
- A lease past `lease_ttl_s` is `LEASE_LOST` when presented, before any other
  controller takes it.
- A lease grant starts a new command epoch and, while a campaign is active,
  reattaches the reconciler (spec/commands.md, "Sequence, epoch and revisions"). The
  lease epoch itself fences API callers and never reaches the agent.

Rejected:

- Plain tokens in the file: a leaked file would work as a credential.
- A counter kept only in memory: a restart would hand out epochs that an old
  controller already holds.

## Consequences

- A lost token cannot be read back. The operator makes a new one and swaps in its hash.
- The length rule is checked on the presented token, so a short guess is refused
  before the comparison.
- The epoch relies on the clock. A clock set far forward raises every later epoch; a
  clock set back is covered by the last-epoch term.
- A failed call's idempotency key is not remembered, so a repeat after a new lease
  runs as a new call (spec/errors.md, "Public API").
- Each grant restarts the reconciler while a campaign is active. A controller that
  renews a lease keeps the epoch and causes no restart.
