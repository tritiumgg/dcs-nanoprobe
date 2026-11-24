# ADR 0013: Command handlers peek the ring head and run in the owning Lua state

## Status

Accepted

## Context

The agent reads commands from the ring in the hook's state, in `frame`. The Lua API a
command needs lives in two other places. Most set-state, effect and query handlers need
the `server` state, which only the shim reaches. `a_unit_set_life_percentage` exists
only in the `mission` state, and `server` and `mission` have no `net`, so the hook is
the only route to it (LIVE; spec/commands.md, "Layout: DAMAGE").

Three rules of the transport and the command contract constrain the design:

- The agent takes one command at a time and stores `in_head` past it before it runs it.
  A command before `in_head` was run or was running when DCS died. A command after it
  was never touched (spec/transport.md, "Consuming").
- Admission checks (epoch, campaign, generation, revision) read state that a mission
  load changes. A command admitted in one frame and run in a later one can be stale.
- Ring order must hold across both states, and each command needs exactly one
  `RESULT`.

## Decision

`frame` peeks the ring head and leaves a command with a Lua handler in the ring. The
state that owns the handler takes it, admits it and runs it under `pcall`. The agent
sends one `RESULT` per command taken.

- `frame` stops reading at a sequenced command with a `server` or `hook` handler. The
  command is not taken or admitted, and `in_head` does not move. `frame` then asks for a
  pump or a hook dispatch, never on the frame that makes the system calls.
- The agent's `command` entry returns only a command at the head whose handler runs in
  the caller's state. It takes it, sets the breadcrumb, and runs the admission table.
  Control commands and admission answers are handled in Rust, in ring order. Nothing is
  held from one frame to the next.
- The State column of the opcode table in spec/commands.md names the state of each
  opcode: `server`, through the shim, or `hook`, with no cross-state call. The hook
  dispatch is one function that any hook callback can call.
- The shim calls the handler under `pcall` and passes the outcome to `command_done`.
  A raise is `LUA_ERROR` with the text. An opcode with no handler is `UNKNOWN_OPCODE`.
  The applied revision moves only in `command_done`.
- A command taken and not answered when the pump ends, or at `finish` or the next
  `frame`, is answered `LUA_ERROR` with the message `no result`. A `command_done` with
  no command outstanding is logged and ignored.
- A pump asked for only for commands runs the generation check and the shim's `reset`,
  then the commands. It does not begin the sampler or sample. Its time, and the time
  of hook commands, count against `commanded_ms`, not agent overhead.
- With no shim loaded, a `server` command waits while the lifecycle state is `LOADING`.
  Otherwise the agent takes it and answers `NOT_READY`. The reconciler holds such a
  command until a lifecycle change, a `HELLO` or 300 frames.
- A `RESULT` message is capped at 1,024 bytes. A handler value over 4,096 table entries
  or 16 KiB of strings makes an `APPLIED` result `TOO_LARGE` with a nil value.

Rejected:

- Take the command in `frame` and hold it for the pump. This breaks the rule that the
  agent takes only a command it will run in this frame. Admission goes stale across a
  load, and a command taken but never run reads `unknown` after a restart.
- A forced pump on the system-call frame. That frame stays free of sampling and of
  cross-state calls (spec/sampling.md).

## Consequences

- Admission runs when the owning state takes the command, so the generation check and
  the handler share one frame. A stale command cannot pass it.
- A command whose handler is in the other state blocks the ring head for the commands
  behind it. Ring order is the price of one outstanding command.
- A pump string that calls a no-op pump costs 18.3 µs at the median and 26.7 µs at
  p99 (LIVE, docs/results/2026-11-13-s10-sampling.md). A commands-only pump pays it
  outside the sampling slice.
- The `command` and `command_done` entries are in the agent's entry table with
  `#[inline(never)]` workers. `tools/raiseshape.sh` checks them. A Lua error cannot
  unwind through a Rust frame that owns a value.
- No frame sees `LOADING`, so the wait for it is a defense. DCS runs no frame
  between `onMissionLoadBegin` and `onSimulationStart` (LIVE,
  docs/results/2026-11-15-s16-players-moderation.md).
- Live results: docs/results/2026-11-14-s14-command-path.md.
