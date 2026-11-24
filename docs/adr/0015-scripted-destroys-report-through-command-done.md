# ADR 0015: Scripted destroys are reported through `command_done`

## Status

Accepted

## Context

A command that removes objects from DCS leaves the agent's object table out of date.
`Group:destroy()` produces no death event, and while the simulation is paused it does
not finish until the simulation runs again (LIVE,
docs/results/2026-11-11-server-gate.md). `Unit.destroy` is reported with no death
(LIVE, docs/results/2026-11-15-s18-persistence-live.md). The agent has to report each
destroy itself, or the removed object reads as a script removal.

The shim can name objects that the agent does not hold, such as units whose births are
not yet resolved. A snapshot of the agent's rows at the time it takes the command cannot
name them. The handler can read the IDs before it calls DCS, because it holds the group
or static handle then.

A spawn under the name of an existing group or static replaces it. The old objects are
replaced in the same call; while paused, DCS lets them go at the resume. An in-place
static respawn needs the old static's ID, or the replaced static reads as a script
removal.

## Decision

The handler of a removing command reads the object IDs it will remove before the DCS
call. It passes them as the fourth argument of `command_done`, only with `APPLIED`.

- `command_done(code, message, value, destroyed)` takes `destroyed`, an array of object
  IDs. No separate entry reports a destroy.
- The handler reads the IDs before the DCS call: the group's unit IDs for
  `GROUP_DESTROY` and `GROUP_CAPTURE_DESTROY`, the static's ID for `STATIC_DESTROY`,
  the existing group's unit IDs for a `GROUP_SPAWN` under an existing name, and the
  existing static's ID for a `STATIC_SPAWN` under an existing name.
- The handler passes the IDs only after the DCS call returned without raising and, for a
  spawn, returned an object. The agent never reports before the call.
- The agent reads `destroyed` only for those five opcodes, in the same `lua_pcall` that
  reads the value. It reads at most 4,096 IDs from one array. It acts on the array
  when it turns `APPLIED` into `TOO_LARGE`, because the destroy has happened.
- For each ID it holds a row for, the agent writes `OBJECT_GONE` with `DESTROYED`
  before the `RESULT` and drops the row. Every named ID, held or not, enters the
  pending-destroy set (spec/commands.md, "A destroy while paused is pending").
- Enumeration and births ignore IDs in the pending set. A `DEAD` event naming one is
  written as an `EVENT`, removes nothing, and is counted. An ID leaves the set when
  `isExist` is false or raises. The set holds at most 65,536 IDs and drops an ID that
  still exists after 60 s of unpaused model time, counted in `pending_expired`.
- Every `APPLIED` `GROUP_SPAWN` sets the entity's `applied_order_rev` to 0.

Rejected:

- A separate `agent.destroyed` entry called after the DCS call. It needs a new entry,
  and the fourth argument needs none.
- A snapshot of the agent's held rows at the time it takes the command. It cannot name
  objects the agent does not hold.

## Consequences

- The shim forgets its handles only for the IDs the agent took; `command_done` returns
  how many it took from the start of the array. The rest stay, and the existence checks
  resolve them.
- A unit killed and then destroyed before an existence check read its death is reported
  `DESTROYED`, not `DEAD` (spec/commands.md).
- The backstop of 60 s lets a later enumeration add back an object that a destroy left
  in DCS. The counters `pending_deaths`, `pending_overflow` and `pending_expired` show
  how often each path runs.
- A destroy of a missing object is `APPLIED` with no IDs, so a repeated destroy is
  safe.
- Any command added later that removes objects must read its IDs the same way and join
  the list of five opcodes. The agent ignores the argument for other opcodes.
