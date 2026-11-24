# ADR 0012: The platform holds no authority policy

## Status

Accepted

## Context

Each managed group and static is simulated by DCS (materialized) or by the campaign
(virtual). A controller decides which units DCS simulates and when. The rules depend
on the campaign: distance to players, mission phase, a budget on live units. One
campaign's rule is wrong for another, and a unit near a threshold needs a hysteresis
rule of its own.

## Decision

The controller is the only part that changes a group's authority. The sidecar executes
the change, hands the state over, answers interest queries, and spreads
materializations within a budget. It holds no rule on when to change authority.

- `Materialize` and `Dematerialize` change authority. A `PutGroup` with `virtual`
  starts a group under the campaign.
- `QueryArea`, or a `WorldFilter` with a center and a radius, answers which entities
  lie near a point, from the store's grid, virtual or not
  ([ADR 0010](0010-no-trigger-zones.md)).
- A dematerialize captures each unit in the frame that destroys it, so the store holds
  the position, heading and life DCS had. A materialize spawns from that state.
- The materialization budget, `materialize_per_frame`, bounds the spawns unanswered
  at a time ([ADR 0022](0022-materialize-per-frame-bounds-unanswered-spawns.md)). It
  limits pace and does not choose units.
- The project builds no authority helper library. Hysteresis and every other authority
  policy belong to the controller.

Spec: [world](../../spec/world.md), "Authority".

Rejected:

- A distance rule or hysteresis in the sidecar. It would fix one policy for every
  campaign.
- An optional authority helper library shipped with the platform.
  When and why a group changes authority is campaign logic.

## Consequences

- A controller author writes the policy: when to dematerialize, how far apart to set
  the thresholds, and how to avoid a unit that flips at the edge.
- The sidecar's tests check the primitives, with a test controller that sends the
  calls.
- A controller that does nothing leaves every group where it is.
- The platform's behavior at the edge of an area is the controller's. The platform
  gives no event for crossing it.
