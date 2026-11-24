# ADR 0031: Sampler budget: strict tier rank, credit, and a pump gate that cannot lock

## Status

Accepted

## Context

The agent's mean time on the sim thread stays within `overhead_share` of real time
(default 0.003), and each frame's slice is capped at 250 µs. Many readers
draw on that budget: births,
the full enumeration, subscribed rows, positions, life, warehouses and the commands
([ADR 0029](0029-sampling-scope.md)).

Facts that shape the budget (LIVE, DCS 2.9.31.29434 under Wine;
docs/results/2026-11-13-s10-sampling.md,
docs/results/2026-11-14-s11-budget-and-baseline.md):

- The cross-state pump call costs 18.3 µs at the median and 26.7 µs at p99. The
  agent's own estimate of the full path, `xcall_us`, is 25 to 31 µs (PRIOR). At about
  110 frames a second the allowance is about 27 µs a frame, so a pump on every frame
  does not fit ([ADR 0001](0001-per-frame-pump.md)).
- One enumeration step, `coalition.getGroups`, cannot be split. Steps reach about
  1,066 µs in the sampling results and 1,138 µs in the budget results.
- A frame where the agent makes its system calls costs about 165 µs at its p99
  (docs/results/2026-11-13-e25-cpu-idle.md).
- A gate that opens a pump only when the frame has room for it can stay shut for good.
  A pump measures the call. With no pump, its estimate does not move.

The numbers and formulas are in [spec/sampling.md](../../spec/sampling.md) ("Slice",
"Tiers", "The sweep", "The pump's worth").
"Slice" lists the frames that may pass 250 µs.

## Decision

The agent spends a per-frame allowance in a strict tier order, with credit, and calls
the pump only through a gate that has a floor and cannot lock.

- **Allowance and credit.** Each frame has an allowance, `overhead_share` times the
  frame interval. A frame that spends less keeps the rest as credit, up to a cap, and
  repays debt first. A slow frame takes no credit. Budget that a reduced scale
  withholds counts as spent and never returns as credit.
- **System-call frame.** It samples nothing, has no config key, and counts toward the
  mean. Its cost is repaid as a small charge on each frame, never as one lump.
- **Strict rank.** The tiers take rows in the order spec/sampling.md "Tiers" gives:
  births, the enumeration, subscribed positions, positions, attitude when stopped,
  vitals, warehouses. A lower tier gets rows only when every tier above has been paid
  what it owes. A starved tier shows an achieved period that grows, never 0. A
  starvation guard gives a late tier one row, and only when the budget could not
  afford it.
- **Credit caps.** Vitals may hold a full slow cycle of credit. Every other tier holds
  one sweep.
- **Separate accounts.** Births and the enumeration spend from an account of their own.
  Their time is never debt for the row tiers, and the position sweep never waits on
  them.
- **Where the work runs.** The hook's `frame` sweeps positions with a deadline in its
  loop. The pump runs only when it has work worth the call (spec/sampling.md, "The
  pump's worth").
- **The gate.** A pump opens when the frame has room for several calls' worth of work.
  The gate never asks for more than a frame can have. It has a floor on the measured
  call, which does not decay between pumps. A clamp counts, in `clamped_pumps`, the
  case where one call cannot fit in a frame. The pump then runs and never locks.
- **The hold.** After the gate shuts for room while a pump waits, the next sweep
  leaves room, so a frame with full credit has room for the floor.

Rejected:

- Aging, or a lone batch to a lower tier every Nth time. It bends the strict rank,
  has no measured basis, and a lone vitals batch is about 3 rows.
- A vitals credit cap of a few sweeps. A cap of five sweeps fits one measurement,
  and an enumeration plus a stall in one cycle already passes it.
- A documented lock limit. A gate that can stop for good under a stated share or frame
  rate is a defect.

## Consequences

- When demand passes budget, the lowest tiers get nothing, and their periods grow.
  That happens at a tenth of the slice, at very low frame rates, with a saturating
  subscription, and with `overhead_share` near its floor.
- Life can read above its rate after a stall until it repays.
- The first pump before a call is measured has floor 0 and can pass 250 µs.
- The survey of the gate found no lock or stop in 4,000 seeded cases at shares of
  0.003 and above (docs/results/2026-11-22-pump-gate.md). The attitude-row hold is a
  known limit: the sweep waits for a pump of the subscribed attitude rows that the
  gate holds back. 18 of the 4,000 cases at those shares had 5 s with no sweep for
  that reason (same results document).
- The code is `Sampler::gate` and `Sampler::sweep` in
  `crates/agent-link/src/sampler.rs`.
