# ADR 0001: One per-frame pump from the hook into the server state

## Status

Accepted

## Context

The agent's world work runs in the `server` Lua state, which has the scripting API. It
needs one ordered place to run each frame: sampling, events to publish, commands to
apply. That place must also run while the simulation is paused. A restore spawns groups
before players are released, and a dedicated server with no client is paused and must
still answer.

Measured on DCS 2.9.31.29434, on the dedicated server under Wine
(docs/results/2026-11-11-server-gate.md, E17) and on a Windows client
(docs/results/2026-11-11-first-dll-and-spikes.md). Every item is LIVE:

- `onSimulationFrame` in the hook runs about 110 times a second, running or paused, in
  all three pause conditions: at model time 0, paused mid-mission, and paused for no
  clients.
- A trivial `net.dostring_in` call from the hook into `server` costs 5.3 µs under Wine
  and 2–3 µs on the client.
- `timer.scheduleFunction` in the `server` state does not run while paused.
- Every callback of both states runs on one OS thread (E3).
- No frame arrives during a mission load.

## Decision

The hook's `onSimulationFrame` makes one cross-state call per frame into a pump
function in the `server` state. All of the agent's per-frame work runs inside that
call, in a fixed order, under the frame budget.

Rejected:

- A timer in the `server` state: it stops while paused.
- Two independent pumps, one per state: no single order between sampling, events and
  commands.
- The DLL calling into the stored `server` state pointer from the hook's callback. It
  was ruled out unless every callback ran on one thread, and E3 showed that they do,
  so it is possible. It is not adopted: it saves about 5 µs a frame, and it enters a
  Lua state from another state's callback by a path DCS does not provide, where
  `net.dostring_in` is the one DCS does. Revisit it if step S11 shows the cross-state
  call matters to the budget.

## Consequences

- The agent pays one cross-state call per pump, charged to its overhead budget. A
  trivial call cost about 5 µs under Wine (E17); the real pump call, from the hook
  through `net.dostring_in` to the shim's pump, costs 22–30 µs (LIVE, S10,
  docs/results/2026-11-13-s10-sampling.md), about the whole 0.3 % slice at 110 frames
  a second. So the agent does not pump every frame: it carries unspent allowance
  forward as credit and pumps when the call is at most a third of what it can spend
  (spec/sampling.md, "Slice"). Calling the shim from the DLL through the stored
  `server` state pointer (candidate C) would remove the call; S11 decides it from a
  breakdown of the call's cost.
- The pump is the one path into the `server` state, and positions do not take it. The
  hook's own state has `DCS.getUnitPositionByObjectId`, so the agent's `frame` reads the
  units' positions there, with no cross-state call, and the pump runs only when
  births, an enumeration or the tiers below the positions have work worth its call
  (spec/sampling.md, "The sweep", "The pump's worth"). LIVE at 1,960 objects the pump
  then runs about 3 times a second, against about 27 with the positions in it (S10).
- The pump runs while paused, so spawning, sampling and command handling work before a
  resume. A removal requested while paused finishes only after a resume (E17), and the
  reconciler must expect the group to exist until then.
- The agent does no work during a mission load. It publishes `LOADING` from
  `onMissionLoadBegin` and `RUNNING` or `PAUSED` once frames return, and the sidecar
  judges health from the lifecycle state, never from a load duration.
- One thread means no locking inside the agent, and the agent starts no thread of its
  own.
- The frame rate differs by host (about 110 a second on the server, 60 on a rendering
  client), so every budget is a share of real time, not a count of frames.
