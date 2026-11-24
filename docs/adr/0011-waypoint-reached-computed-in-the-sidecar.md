# ADR 0011: Waypoint reached is computed in the sidecar; no waypoint Lua

## Status

Accepted

## Context

A controller wants an event when a group reaches a waypoint. DCS can run a `Script`
action in a waypoint's combo task, so the agent could attach a platform script to each
waypoint and report from it.

Two runs on DCS 2.9.31.29434, a dedicated server under Wine 10.0
([the world-command results](../results/2026-11-15-s15-world-commands.md), "Waypoint
Script actions"):

- A waypoint `Script` fires once per group, not once per unit, just after the spawn
  for the first waypoint (LIVE).
- The script runs in a separate state that has `world`, `net` and `trigger` but is
  not the `mission` state. A global defined in the `server` state does not exist
  there, so the script cannot reach the agent's shim (LIVE). Which state it is is
  UNVERIFIED.
- The first run raised `attempt to call global 'np_wp' (a nil value)` in that state.
  DCS then stopped answering, with its process alive, its ports open and no log line
  after that second (LIVE, LOG). The cause is unknown.
- Whether a `Script` fires on arrival or on passing a later point was not settled.

The sidecar already follows each group's route from its position samples.

## Decision

The sidecar computes `WAYPOINT_REACHED` and `ROUTE_COMPLETE` from route progress. The
platform runs no Lua on a waypoint: no platform script and no controller script.

- A waypoint counts as reached when a sample of the group's lead unit lies within 50 m
  of it on the map plane. It also counts when the segment between two consecutive
  samples of the same lead unit passes within 50 m of it, or when the group is seen at
  a later waypoint.
- A segment counts only when the two samples are at most 60 s of model time apart,
  with no `HELLO`, new mission, death or new order between them. The lead unit is the
  first member that is not dead. Progress never goes back.
- The event carries the group, the order revision, the waypoint's index from 0, the
  model time of the sample that showed it, and whether the position was observed or
  written by the controller. `ROUTE_COMPLETE` follows the last waypoint.
- The route compiler emits the controller's waypoints and attaches no action to them.

Spec: [world](../../spec/world.md), "Route progress".

Rejected:

- A standard platform `Script` on every waypoint. It cannot reach the shim, and an
  unhandled error in it was followed by DCS hanging.
- Controller scripts on waypoints, gated by the raw-Lua setting.
  They run in the same state, out of the shim's reach, and a script error there was
  followed by a hang.

## Consequences

- The event is as current as the position samples: up to one position period after
  DCS reaches the waypoint ([sampling](../../spec/sampling.md)).
- A fast group can pass a waypoint between samples. The segment test counts it.
  A segment over 60 s of model time apart does not count, so a long gap in samples
  can leave a waypoint to be reached by the later-waypoint rule.
- Nothing runs in DCS for the event, so it adds no work to the frame budget.
