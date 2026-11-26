# ADR 0051: An order refused NOT_BOUND waits for any member

## Status

Accepted. Refines the resend rule for `GROUP_ORDER` in spec/commands.md, "Layout:
GROUP_ORDER".

## Context

An order that resolves `refused` is not sent again at the same revision. It is sent again
when DCS gets back a member it lacked: a revived member, a `MISSING` or `REMOVED` member
found again, or a `lost` group found. The reconciler recognizes each of those from what
it read of the member: a death fact, or a `MISSING` or `REMOVED` mark.

The property search on the fake sim found this sequence. A script removed one living
member with no record. The group's other living member then left as `REMOVED`, and DCS
held none of the group. The group's attack order ran and the agent answered
`NOT_BOUND`. A script then revived the silently removed member. The reconciler had
never read that the member left, so its `OBJECT` matched none of the three cases. The
order stayed refused, and the attacker in DCS held no task. UNVERIFIED: whether DCS can
revive an object a script removed under its old object ID; the fake sim allows it.

## Decision

A `GROUP_ORDER` that resolves `refused` with `NOT_BOUND` is marked as refused while
DCS held none of the group. After that answer, any `OBJECT` record of a member clears
the refusal, and the order is sent again at its revision. A refusal with any other code
keeps the existing rule.

## Consequences

- The answer `NOT_BOUND` stands in for the leave record the reconciler never read. A
  member DCS holds again with no record that it left brings back the group's order.
- The agent writes an `OBJECT` only for an object DCS holds. While DCS holds none of the
  group no such record comes, so the rule sends no order in a loop.
- No change to the agent, the transport or the store.
