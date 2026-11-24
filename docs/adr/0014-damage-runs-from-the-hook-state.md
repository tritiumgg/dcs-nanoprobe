# ADR 0014: DAMAGE runs from the hook state

## Status

Accepted

## Context

`DAMAGE` sets a unit's life as a percentage of its initial life. DCS exposes the setter
`a_unit_set_life_percentage` only in the `mission` scripting state. The `server` state
and the `mission` state have no `net` table, so neither can call into the other. The
hook state has `net.dostring_in`, which reaches both. The hook is the only route
(LIVE, spec/commands.md, "Layout: DAMAGE"). A handler in the shim, which lives in
`server`, can never reach the setter, so it would answer `DCS_REFUSED` on every call.

The setter revives a dead unit: `isExist` turns true and `getLife` reads the new life
(LIVE, docs/results/2026-11-14-e38-life-and-ammunition.md). It must not run on a unit
that is already dead. At 0 it removes a unit from DCS. At 50 it sets half the initial
life on an aircraft, a helicopter and a ground unit, read back (LIVE,
docs/results/2026-11-15-s15-world-commands.md). It has no effect on a hangar static at
50.

The command dispatch of [ADR 0013](0013-command-handlers-run-in-the-owning-state.md)
already lets a handler run in the hook state.

## Decision

`DAMAGE` has State `hook`. The hook's handler makes one `server` lookup and one
`mission` call, both through `net.dostring_in`, under `pcall`.

- Rust checks that `life_percent` is finite and from 0 to 100, else `BAD_PAYLOAD`. It
  passes the value as text it formats itself, so no Lua locale reads it. It builds the
  unit's platform name from the header (spec/world.md, "Names").
- The handler checks that `name` matches the platform unit form and is at most 15
  characters, and that the life text is digits, a point and digits. A mismatch is
  `DCS_REFUSED`.
- The handler asks the `server` state for the unit's `getID()` by name. A unit that
  does not exist is `NOT_BOUND`, and no `mission` call follows.
- The handler checks that the ID is a non-negative integer. It then calls the
  `mission` state with a fixed template that formats the ID with `%.0f` and guards with
  `c_unit_dead(id)`. A dead unit is `NOT_BOUND`.
- A call that fails is `DCS_REFUSED` with its text. Otherwise the result is `APPLIED`.
- The command takes no unit ID from the agent's rows. The `server` lookup resolves the
  name each time.

Rejected:

- A handler in the shim, as for most opcodes. It cannot reach the setter, so it fails
  on every call.

## Consequences

- A `DAMAGE` costs two `net.dostring_in` calls. The hook commands share the
  `commanded_ms` budget and are excused from the sampling slice like a commands-only
  pump (spec/commands.md, "Handlers").
- The handler builds Lua source for the `mission` state. The checks on the name, the
  life text and the ID keep that source fixed in form.
- A `DAMAGE` on a static has no effect at 50 (LIVE). At 0 it killed a static of one type
  on a dedicated server, and a connected client kept showing it alive (LIVE,
  docs/results/2026-11-17-person-fixes.md). The sidecar sends `DAMAGE` for units only.
- A unit set to 0 leaves DCS without a death event, except an airborne aircraft
  (LIVE, docs/results/2026-11-15-s15-world-commands.md).
- A death caused on a dedicated server by a setting of 10 did not reach a connected
  client (LIVE, docs/results/2026-11-11-first-dll-and-spikes.md). UNVERIFIED for other
  settings and types.
- The hook dispatch is one function that any hook callback can call, so a hook command
  can run outside `onSimulationFrame`.
