# ADR 0016: Dead-boot commands resolve `unknown`; `layout_version` gates behavior

## Status

Accepted

## Context

The sidecar journals every command and resolves it to one outcome. A `HELLO` carries
the agent's `in_head`, which says which commands the agent took (spec/commands.md,
"Unanswered commands"). A `HELLO` from a new DCS process cannot say anything about the
commands of the process before it. DCS may have taken commands after the last `in_head`
the sidecar read. The old process wrote no `HELLO` when it died.

Two questions follow from the contract between the agent and the sidecar:

- What outcome does a command get when its agent boot ended before it answered?
- Which version numbers does the sidecar check? The status block holds
  `layout_version` and `interface`. The hash of the layout tables is checked at attach.

## Decision

Every unanswered command of an `agent_boot` other than the current `HELLO`'s resolves
`unknown` at the first `HELLO` of a new `agent_boot`. The sidecar checks
`layout_version` and the layout hash, and does not check `interface`.

- Until that `HELLO`, the command stays `pending`. Its position in the ring does not
  matter. The rule holds whether or not the sidecar ran when DCS died.
- After the kill switch, the agent writes no further `HELLO` until the process exits.
  The sidecar resolves its commands the same way.
- `unknown` means the agent may have taken the command and its result is lost
  (spec/errors.md, "Command outcomes"). The breadcrumb names the command that was
  running when DCS died. It is in `crash.log` and in the status block, which the
  command-line inspector shows. The sidecar does not read it.
- A `HELLO` that is a resync (not the first one the sidecar read since its handshake)
  resolves nothing as `not_sent`. The ring is untouched, so the agent may still run
  the commands after its `in_head`, and they stay `pending`.
- `interface` is the version of the DLL-to-shim ABI. The hook checks it.
  Health shows it (`agent_interface`; spec/transport.md, status block; spec/records.md,
  `HELLO`).
- `cargo xtask spec` writes the status block's `layout_version` into
  `spec/transport.golden`, so `layout_version` is part of `layout_hash`. A sidecar
  refuses an agent whose hash differs from its own.
- `layout_version` goes up by one when agent behavior the sidecar reads changes and no
  layout table does (spec/transport.md, "Layout version").

Rejected:

- Resolving a dead boot's commands as `not_sent`. The agent may have run them before
  DCS died, so "it did not happen" is wrong.
- Checking `interface` in the sidecar. It is the DLL-to-shim ABI, which the hook
  checks. The golden file gates the agent-to-sidecar contract.

## Consequences

- A dead boot's commands stay `pending` until a new boot sends its `HELLO`. A server
  that stays down shows them as pending.
- A set-state command that resolves `unknown` is derived again from the store under the
  new epoch. An effect that resolves `unknown` is reported and not sent again
  (spec/errors.md).
- The sidecar does not read the breadcrumb after DCS dies. A person reads it in
  `crash.log` or through the inspector.
- A change in agent behavior with no table change costs a bump of `layout_version` and a
  new golden hash. An old sidecar then refuses the new agent, and the reverse.
- A change of `interface` alone does not stop the attach. A hook and DLL that disagree
  stop at the hook's check.
