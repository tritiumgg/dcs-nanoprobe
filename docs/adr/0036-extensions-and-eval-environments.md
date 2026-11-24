# ADR 0036: Extensions and EVAL run in their own Lua environments

## Status

Accepted

## Context

A controller needs custom logic inside DCS for what DCS does not report by itself.
The agent runs in the `server` state, where one raise or one runaway callback can
stop every other handler and the agent with it.

DCS's event dispatcher has no `pcall`: a handler that raises stops every handler
after it (PRIOR). A callback that DCS runs later, from a timer or a menu, runs outside
the agent's budget and outside the agent's `pcall`. The simulation's own state also
resets on a
mission load (UNVERIFIED), so a chunk cannot keep state in Lua.

## Decision

Raw Lua is off by default, and when `[lua] raw = true` an extension and an EVAL
chunk each run in a fresh environment that reads through to the `server` state's
globals, with the functions that start work outside the agent replaced.

- An extension is a named chunk registered with `EXTENSION_SET`. It may return the
  hooks `invoke`, `on_event`, `on_frame` and `on_stop`. The agent calls every hook
  under `pcall`, charged to the commanded-work budget
  (spec/commands.md, "Extensions"). Hooks run from the pump, except `on_stop` at
  mission end, which runs from `onSimulationStop` through the cross-state call.
- The replaced globals are `world.addEventHandler` and `removeEventHandler` (raise,
  pointing to `on_event`), `timer.scheduleFunction`, `removeFunction` and
  `setFunctionTime`, `missionCommands.addCommand`, `addSubMenu` and `removeItem` in
  their plain, coalition and group forms, and `emit`. Callbacks and menu items go
  through the agent, which removes them when the extension stops.
- An EVAL chunk runs once under `pcall` in a new environment. An assignment to a
  global stays in that environment; state carries to the next chunk only through
  explicit writes to `_G`.
- `emit` never raises. Its Lua wrapper uses `pcall`, and the agent's entry reads the
  payload with raw table access inside the worker that runs under `lua_pcall`.
- After 10 errors in a row an extension's hooks stop until a newer revision or the
  next mission start. A mission start runs every registered extension again in a new
  environment, and the sidecar registers them again after a DCS restart.
- The agent cannot interrupt a hook that loops. `tools/raiseshape.sh` checks that every
  agent entry owns nothing when it can raise, the entries `player`, `menu`,
  `emit`, `extension` and `extension_budget` included.

Rejected:

- Letting an extension register its own event handler: a raise in it would stop the
  agent's handler too.
- A shared environment for all chunks: one chunk could replace a global the shim or
  another extension uses.

## Consequences

- An extension that reaches the real functions another way, such as through `_G`, is
  outside these guarantees.
- Nothing an extension holds in Lua survives a mission load. It reports what must
  last with `emit`, and the controller gives it back by invoking the extension.
- A hook that loops stalls the simulation thread. The budget slows a slow extension
  but cannot stop one.
- Raw Lua gives the caller the power of the `server` state. It needs the `admin` role
  for `EvalLua` and `RegisterLua` (spec/errors.md, "Roles").
