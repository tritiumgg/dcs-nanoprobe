# ADR 0017: A set-state command is applied when DCS holds its revision

## Status

Accepted

## Context

A controller sets desired state. The sidecar derives commands from the store and sends
them to the agent. A command can be lost in several ways: the sidecar restarts, DCS
restarts, a mission loads, or a result never reaches the sidecar. The sidecar then
sends again what DCS still needs.

If `applied` meant "the agent took sequence number N", a resend with a new sequence
number would run the spawn a second time. Judged by sequence number, a resend after a
lost result answers `AT_REVISION` and would never count as applied, though DCS holds
the revision. A client watching a call would also see an outcome that has no meaning
once the store drives the state.

The agent keeps an applied revision per entity (spec/commands.md, "Sequence, epoch and
revisions"). A command that carries a revision at or below it is answered `AT_REVISION`
and does not run.

## Decision

For a set-state command, `applied` means DCS holds that revision in this generation.
The sidecar judges set-state commands per revision. It judges effects per sequence
number.

- A pending resend resolves `applied` when the revision is shown applied: the agent
  answers `AT_REVISION`, or the death of a member shows the spawn ran. A spawn that is
  not yet answered when the last death fact arrives resolves `applied` (spec/world.md,
  "Lifecycle").
- `AT_REVISION` reads "the agent applied this revision or a newer one in this
  generation".
- `not_sent` covers any non-control command the agent never took. For a set-state
  command, and for a query the sidecar needs itself, the sidecar derives what DCS needs
  from the store and sends it as a new command. An effect or a client's query that
  resolves `not_sent` is not sent again.
- The call that sets state succeeds when the desired state is journalled. It does not
  wait for DCS (spec/errors.md, "Public API").
- `GetOutcome` on the key of a set-state call returns the `campaign_id` and no
  `result`. The call has no command outcome. The client follows the entity's applied
  revisions and last error in the World service. Repeating the call with the same key
  returns its first response, revisions included. An unknown or expired key is
  `OUTCOME_NOT_FOUND`. `not_sent` stays for effects and queries.

Rejected:

- Judging set-state commands by sequence number. Judged by sequence number, a resend
  after a lost result answers `AT_REVISION` and would never count as applied, though
  DCS holds the revision.
- Reporting `not_sent` from `GetOutcome` for a set-state call. The store drives the
  state whether or not a command was sent, so the value says nothing the client can use.

## Consequences

- The exactly-once check for set-state commands counts revisions, not sequence numbers.
  An effect is checked per sequence number, and an effect that resolves `unknown` is
  not sent again (spec/errors.md).
- A spawn sent again after a lost result can answer `AT_REVISION` for a group that is
  new in DCS. The reconciler treats that as applied and sends the order again, which
  costs at most an `AT_REVISION` (crates/sidecar/src/reconciler.rs).
- A client that wants to know what DCS did reads the entity, not the call. A set-state
  call has no result to wait for.
- A destroy sets the applied revisions of what it removed to 0, so the spawn that
  follows runs (spec/commands.md, "Set-state commands").
- The agent's applied revision moves only in `command_done`, and only with `APPLIED` or
  `TOO_LARGE` (spec/commands.md, "Sequence, epoch and revisions").
- Live results of the command path: docs/results/2026-11-14-s14-command-path.md.
