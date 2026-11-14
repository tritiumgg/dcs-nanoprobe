# Sampling

How the agent reads the world inside DCS: the frame's time slice, the tiers that share
it, the full enumeration, and what it reports. The records it writes are in
spec/records.md; the object table's rules are in spec/records.md, "The object table".

## The frame

On every `onSimulationFrame` the hook calls the agent's `frame` with DCS's real time,
model time, pause state, and `DCS.getUnitPositionByObjectId` when the hook's state has
it. `frame` does the link's work and the commands, sweeps the positions it owes with
that getter ("The sweep"), and says whether to pump. When it says so, the hook makes one
`net.dostring_in` call into the shim's `pump` in the `server` state, then calls the
agent's `finish`, which publishes the frame's batch and stores the status block. On any
other frame `frame` does that itself and the hook makes one call.

A sampling frame is one where:

- it is not the frame that makes the agent's system calls (spec/transport.md,
  "Handshake");
- a sidecar is attached and the agent has written its `HELLO`;
- the lifecycle state is `running` or `paused`. Sampling goes on while paused, because
  a restore happens before the resume.

Only a sampling frame sweeps, and only a sampling frame with the shim loaded pumps. A
detached agent therefore samples nothing and enumerates nothing. Its event handler
still queues the existence checks that events ask for, but no pump runs them, so no row
leaves the table. The shim keeps at most 4,096 unresolved births; the full enumeration
that starts after the next `HELLO` finds the rest, and checks the rows that left while
the agent was detached.

When the hook's state lacks the getter, `frame` sweeps nothing and the pump reads the
positions on the same schedule ("Tiers").

## Slice

All times are agent time on the sim thread, in microseconds, measured with the agent's
clock (`QueryPerformanceCounter` on Windows).

| Name | Value |
|---|---|
| `I` | Running average of the interval between frames (`frame_interval_us`): each new interval weighs 0.05 |
| `A` | The frame's allowance: `overhead_share` × `I` |
| `C` | System-call cost per second: the agent time of the system-call frames in each second of real time, as a running average over seconds, each new second weighing 0.2 |
| `c` | The per-frame charge for the system-call frames: `C` × `I` / 10⁶ |
| `D` | Debt: agent time spent past the allowances, at most one second's budget (`overhead_share` × 10⁶). Below 0 it is credit: allowance earned on frames that spent less, at most 150 µs |
| `r` | The repayment planned this frame: the smaller of `D` and `A` / 2, when `D` is above 0 |
| `S` | The frame's slice, `slice_us`: the smaller of `A` − `c` − `r` + credit and the cap. The cap is 250 less what the frame's finish takes: the frame's fixed part already spent, and an estimate of closing the last batch and writing its record (`close_us` + the batch tail + 2 × the closing's mean deviation, at least 6 µs). No frame passes 250 µs. The cap binds only below about 12 frames a second. After a frame interval over 2 × `I`, such as a stall, the frame takes no credit |
| `F` | Fixed work: agent time from the start of `frame` to the point of the decision, plus the agent time of the events handled since the last frame. For the pump it includes the frame's sweep |
| `m` | The share of the allowance for births and the enumeration: 0.5, and 0.75 while an enumeration has run longer than `enumeration_s` |
| `R` | The sweep's savings that the pump leaves alone ("The sweep"), `k` times what the sweep has saved. 0 when the pump reads the positions |
| `x` | The running estimate of the cross-state call: the time from `frame`'s decision to the start of the shim's pump, each new call weighing 0.1, one call counting at most 62.5 µs. It decays by 1 % on each pump, and on a frame where 3 × `x` is above the gate's cap and the pump has work worth the call ("The pump's worth"), at most once a frame, so an estimate that is too high cannot keep the pump shut |
| `f` | The running estimate of a pump batch's fixed part ("Tiers"), from the row tiers' batches only. It decays by 1 % on each pump |
| `G` | The gate: 3 × (`x` + `f`), but at most the cap `reach` = 0.8 × (`A` − `c` + 150 − `F` − `R`) and never under 3 × `x` |
| `B` | The pump's sampling budget: (`S` − `F` − `R`) × `k`. Nothing is sampled when it is 0 or less. `k` is 1, except in a build with fault injection, where the `sampling` fault sets it (spec/faults.md) |

A frame calls the shim's pump only when the pump has work worth the call ("The pump's
worth") and `S` − `F` − `R` is at least `G`, so the call and one batch's fixed part take
at most a third of what the frame has left, as far as credit can reach. The cap `reach`
leaves out the sweep's reserve `R`, so the gate never asks for more than a frame can
have beside it. Under Wine the
call costs 22–30 µs (PRIOR, S10, docs/results/2026-11-13-s10-sampling.md) against an
allowance of about 27 µs at 110 frames a second, so frames carry their allowance
forward as credit until a sweep or a pump spends it.

At the end of a frame that is not a system-call frame, the agent takes the frame's
agent time `T` (from the start of `frame` to its end, or to the end of `finish`, plus
the events since the last frame) and sets `D` to `D` + `T` + `W` − (`A` − `c`), held
between −150 µs and its cap. `W` is what the scale `k` withheld from the frame: of the
pump's budget, (`S` − `F` − `R`) × (1 − `k`), and of the sweep's, the cost of the rows
`k` kept it from reading. It counts as spent, so it never returns as credit. A frame
that spends less than its allowance repays debt or earns credit; one that spends more
adds to the debt.

A system-call frame samples nothing, earns no credit and leaves `D` alone. Its time goes
into `C`, so the frames after it repay it a little each, about 1.5 µs of a 27 µs slice
at the server's 110 frames a second with the measured 165 µs a second (LIVE,
docs/results/2026-11-13-e25-cpu-idle.md). No frame is skipped to repay it. The cap on
`D` covers a handshake, which brings up to ten system-call frames in a second.

The mean of the agent's time over real time therefore stays at `overhead_share`, less
whatever debt the cap forgave. No frame's slice is more than 250 µs, and with credit at
most 150 µs a frame at the server's frame rate stays under 250 µs with its sweep, its
pump, the closing and `finish`.

## Tiers

Each tier keeps a cursor over the table and a credit: the rows it is owed. Every
sampling frame each tier's credit grows by the rows one of its sweeps reads times the
real time since the last frame over its target period.

The cap is one sweep's rows for every tier except vitals. The cap of vitals is every
row once, a full slow cycle, `max(n, rows)`, with `n` one sweep's rows and `rows` the
rows in the table. A stall or a hold
by a higher tier can take more than one sweep from vitals, a twelfth of its cycle, and
vitals repays it. Until it is repaid, vitals reads above its rate, with ammunition and
warehouses behind it, apart from the starvation guard's one row.

The budget goes to the tiers in this order; each takes the rows its credit allows, and
what is left goes on:

| Order | Tier | Rows | Target period | Calls |
|---|---|---|---|---|
| 1 | Enumeration, while one runs | every group and static in DCS | `enumeration_s` | `coalition.getGroups`, `Group.getUnits`, `coalition.getStaticObjects`; the calls of a birth for an object the table lacks; `getPoint` for a static that cannot move |
| 2 | Births | births waiting to be resolved | each pump | `isExist`, `getName`, `getTypeName`, `getGroup`, `getID`, `getCoalition`, `getCountry`, `getDesc().category`, `getLife0`, `Object.getCategory` for a static, and `getPoint` for one that cannot move |
| 3 | Subscribed positions | the rows `SUBSCRIBE` names | its `period_s` | as for positions, and `getPosition` when it asks for attitude |
| 4 | Positions | every unit, and every static that can move | `position_s`; five times it for a row that has not moved | `DCS.getUnitPositionByObjectId` |
| 5 | Attitude when stopped | units that stopped since their last attitude | each pump | `getPosition` |
| 6 | Vitals | every row | `vitals_s` for a row `SUBSCRIBE` names; twelve times it for any other row, the slow sweep | `getLife`, and `getFuel` for a unit; `isExist` on each row the slow sweep visits |
| 7 | Ammunition | the units `SUBSCRIBE` names, and units that fired | `ammo_s` for a unit `SUBSCRIBE` names; no period for any other unit | `getAmmo` |
| 8 | Warehouses and airbases | every airbase of the mission | `warehouse_s` | `getCoalition`, `getWarehouse():getInventory()` |

Which rows a sweep reads:

- **Positions.** A unit or a movable static that moves is read on every position sweep.
  One whose position changed by less than 0.5 m on three reads in a row is read once in
  five sweeps; each sweep reads the rows whose slot plus the sweep's count divides by
  five. The first read that finds it moved more than 0.5 m returns it to every sweep,
  so a row that starts to move is seen up to five position periods late, once. A static
  that can move is one whose `Object.getCategory` is 6, cargo; the shim reads that once
  when it adds the row. Whether the getter follows cargo that an aircraft carries is
  UNVERIFIED.
- **A static that cannot move** has no place in the positions tier. The shim reads its
  `getPoint` when it adds the row and each time an enumeration finds it, and the agent
  writes it in a `POSITIONS` record after the `OBJECT` records of that batch.
- **Life and fuel.** Those of a row `SUBSCRIBE` names are read on every vitals sweep.
  Any other row's, with no exception for aircraft, are read once in twelve vitals
  sweeps, the slow sweep (60 s at the default `vitals_s`); once when the row is added; and after an
  `S_EVENT_HIT` names the row as its target, no sooner than 1 s after the row's last
  read. Fuel comes with each of those reads and has no schedule of its own: a ground
  unit's or a ship's reads 1.0 moving or halted (LIVE), and an aircraft's is as old as
  its row's last read. The slow sweep also checks `isExist` on each row it visits, which
  finds a kill that sent no hit event (spec/records.md, "The object table"). A sidecar subscribes the units players occupy, and a capture
  reads fuel itself (spec/commands.md).
- **Ammunition.** That of a unit `SUBSCRIBE` names is read on every ammunition sweep.
  Any other unit's is read on demand only: after an `S_EVENT_SHOT` or
  `S_EVENT_SHOOTING_END` names the unit as its initiator, no sooner than 5 s after its
  last read, and at a capture (spec/commands.md). It has no periodic sweep. A unit that
  has not fired therefore has no `AMMO` record: its load is unknown, not full. A rearm
  sends no event (LIVE for an AI tank), so it is not seen before the unit fires again or
  is captured.
- An event with no target or no initiator, and an event naming an object the table
  lacks, marks nothing. Damage that sends no event, such as a scripted explosion's
  splash on a unit it does not kill (LIVE), shows at the row's next slow sweep of life.
- **After a `HELLO`** the sidecar holds no samples. Every row returns to every position
  sweep until it has again read unmoved three times, and every row that is not read on
  every vitals sweep is marked for one read of its life and fuel.

Rows that an event or an attach marked are read at the start of their tier's next
batch, before the cursor's rows, and do not count against the tier's credit.

When the budget cannot meet every target, the tiers lower in the table get less, so the
rates fall in the order of plan section 4.2: ammunition and warehouses first, then
vitals, then the positions of rows no subscription names. Subscribed positions fall
last. The enumeration keeps its period, within the limit below.

The rank is strict. A lower tier gets rows only when every tier above it has been paid
what it owes. When the budget cannot pay a higher tier in full, the lower tiers get no
rows at all and their periods grow ("Achieved period"). That happens at a tenth of the
slice, and in the release build when the slice is capped at very low frame rates, when a
subscription saturates the budget, or when `overhead_share` is near its floor. With the
sweep in `frame`, the pump reads no tier below the positions while the sweep owes more
than a quarter of a position period's rows and 64.

**Starvation guard.** At a scale of 1, with the sweep not behind, a tier below the
positions that the pumps could not afford is late and goes right after the subscribed
rows, with one row, ahead of owed vitals. A tier is late only when the budget is at a scale
of 1, the pumps could not afford it and the sweep is not behind. The wait is timed from the
tier's last planned batch, or from its first owed row when none was planned. Ammunition and
warehouses are late when two rows are owed and the wait passed the pump's 1 s wait. Vitals
is late when the wait passed 1 vitals period, and takes its rows by the rank. A late tier
takes at most one row per late interval. With the guard on, the vitals period stays inside
the 4.5-6.5 s band of the vitals target. A warehouse read whose cost is unmeasured plans one row, and only when
what is left of the budget exceeds a batch's fixed part by 62.5 µs; warehouse rows are
still read within about 2 × their period or the pump's 1 s wait.

Births and the enumeration take at most the share `m` of the sampling time while any row
tier owes rows. A balance in µs gains `m` × (`A` − `c` − the frame's fixed work before the
sweep) × `k` on every frame that is not a system-call frame, up to 250 µs. It loses the
time of their batches. While a row tier owes rows they take a batch only when the balance covers one
batch: the smaller of 3 × the measured call and the cost of the rows still owed plus
`batch_us`, at most the most a frame can reach. When no row is owed they take a batch
whatever the balance. The balance gains its share × `k`, and a batch of births or the
enumeration costs it the larger of the batch's time and its batch cost × (1 − `k`), so
at a scale below 1 maintenance yields to the tiers: it falls about as `k`^1.7, to about 2 %
of its rate at `k` = 1 when `k` = 0.1 with rows owed. At `k` = 1 nothing changes. The balance stops at −1,000 µs, so one dear step, such as a `coalition.getGroups`
of 687 µs (PRIOR, S10), keeps them out for a bounded number of pumps. A burst of such
steps does not keep them out for good. While a tier above the positions waits for a
pump, the sweep saves only 1 − `m` of each frame's allowance, so the two shares add up
to the allowance and the pump takes no second share. The row tiers therefore always
keep the rest of the sampling time, so subscribed positions keep a rate even at a tenth
of the slice, where that takes precedence over the enumeration's period. A running
enumeration goes before births and reads their objects itself; the birth of an object
it already added costs one `wants` check and is dropped. While enumerations run back to
back, as at a tenth of the slice, new objects therefore enter through the enumeration,
within one enumeration period plus that enumeration's duration.

### The sweep

`frame` reads the positions it owes itself, in the hook's state, each getter call under
`lua_pcall`: first the subscribed rows read without attitude, then the positions tier.
Nothing else runs between choosing a row and reading it. It writes them in one
`POSITIONS` record, after any `OBJECT` record of the frame.

- **Savings.** On every sampling frame the sweep's savings `R` grow by `A` − `c`, or by
  (1 − `m`) × (`A` − `c`) while births, an enumeration or subscribed rows with attitude
  wait for a pump. They stop at a full sweep's cost over `k`. A full sweep's cost is the
  sweep's fixed part plus the rows owed, at most 256, at the cost of a row; it is at
  most what one frame can ever spend on it: the smaller of `A` − `c` + 150 and 250, less
  `F`, times the same share. The pump leaves `k` × `R` of the frame alone, with one
  exception: on a frame where the pump is wanted for subscribed rows read with their
  attitude ("The pump's worth"), it may spend the savings on the subscribed tier only.
  Other tiers keep the reserve. On such a frame the sweep reads no row of the positions
  tier, which no subscription names, until that pump has run.
- **When.** A sweep's budget is the smaller of `k` × `R` and `S` − `F`. It reads the rows
  that pays for after its fixed part. It waits until that is 64 rows, or as many as a
  frame can ever pay for when that is fewer; or every row owed, once the last sweep is
  a tenth of `position_s` old. A sweep's fixed part, the record and its flush, is then
  paid about ten times a second, not on every frame. The scale `k` applies to the
  savings, so at a tenth a sweep is as large and comes a tenth as often. With full
  savings and no debt, a sweep whose fixed part alone passes its budget reads one row,
  so its costs are measured again.
- **Deadline.** The loop reads the clock every `n` rows, where `n` keeps clock reads
  under 2 % of the rows between them at the sweep's running cost a row, and is at most
  32. It stops at the first read past the budget less the running average of the time
  the sweep takes after its loop. A slow getter or a cost estimate that is too low
  therefore cannot carry a sweep past its budget by more than `n` rows, and the rows it
  did not read stay owed.
- **Cost.** The sweep plans with two running averages, each new sweep weighing 0.1: the
  cost of one row inside its loop, and its fixed part, the sweep's time outside the
  loop. The flush of `OBJECT` records queued before the sweep is not the sweep's time.
  The tier's reported cost a row is the sweep's whole time over its rows: each new
  sweep weighs 0.1, and one that is cheaper weighs 0.5.
- **A read that raises** ends the sweep. The agent leaves that row out of every tier
  until the next full enumeration finds the object again, or its existence check removes
  the row.
- **A read that gives no position**, or three zeros, counts as an unmoved read, and
  the row's existence is checked on the next pump (spec/records.md, "The object
  table"). The row is soon read less often until the check settles it.

### The pump's worth

With the sweep in `frame` the pump is called only when it has work worth its call:

- a batch's worth of subscribed rows with attitude is owed: the smaller of three calls'
  worth of rows and the rows subscribed, or the oldest owed row is past its period. A
  smaller number waits, so a pump is not called for a few rows; or births or an
  enumeration that is due or running wait and may take a batch ("Tiers": their balance
  is above 0, or no row is owed);
- or an existence check that a `S_EVENT_HIT`, `S_EVENT_BDA`, `S_EVENT_KILL` or a death
  event asked for, or a check of a row the last enumeration missed, is queued. It is
  urgent only when it was actually queued, and the pump releases the sweep's reserve for it
  unless the sweep is behind;
- or the sweep is not behind (above), and the tiers below the positions owe rows that
  cost at least three calls, or as much as a frame could spend on them. A tier (vitals,
  ammunition or warehouses) whose owed rows the last pump planned none of for lack of
  budget leaves that sum until the last pump is 1 s old; a planned row of the tier clears
  the mark, and rows skipped because the sweep is behind or the rank is strict do not
  set it;
- or they owe anything and the last pump is 1 s old.

The agent counts births itself from the birth events, so an event that is no birth
calls no pump. The call's cost here is the running average of the calls measured, with
no decay between pumps, and 20 µs before the first. A pump that plans no row is counted
(`empty_pumps`).

Each pump makes as many existence checks as its budget pays for at the cost of a vitals
row, at least one and at most 20. The rest carry over, and the checks an event
asked for and the checks of the enumeration's missed rows share one urgent queue, in
arrival order, ahead of the slow sweep's checks.

### Batches in the pump

A batch costs a fixed part `f` (planning it, the shim's dispatch and its return) plus
its rows. The sink that takes each row reads the clock at the first and the last planned
row, and every `rows_per_clock` rows between. A row tier's cost per row is measured
inside the batch, from the first row's clock read to the last; the rest of the batch's
time is its fixed part. A row tier's batch without two clock reads leaves the cost as it
is, unless the tier has none yet; a batch of one row lowers a cost that is too high.
Births and the enumeration time each step: their cost per row is the mean of the steps
after the batch's first. Steps over 62.5 µs, such as one `coalition.getGroups`, stay out
of it. Each tier keeps a running average of its cost per row, and the agent keeps one of
`f` and of `t`, the part of `f` after a batch's last clock read; each new batch weighs
0.1.

What is left of `B` for rows keeps room for closing the batch: the running average of
a close, which decays by 1 % on each pump, twice its mean deviation, and `t`. A batch plans (what is left − (`f` − `t`)) /
cost rows, so a batch rarely overruns and a pump pays `f` once per tier. The loop stops
at the first clock read that finds nothing left. A row that costs more than the whole
sampling budget `B`, such as a large warehouse or one enumeration call, is taken alone
at its tier's turn. A row that costs more than what is left of `B` is taken alone when
it is the frame's first batch. Both happen only on a frame where `D` is 0, and the
overrun becomes debt. When `f` and the room for closing pass what is left, as at a
tenth of the slice, the frame's first batch takes the rows `B` allows, at least one, on
the same terms. Its planned rows bound it, not the clock. The report counts all of these
as `lone_rows`.

`rows_per_clock` is the smallest whole number that keeps clock reads under 2% of a
sweep: the cost of one clock read, measured once when the link opens as the mean of
64 reads, over 2% of the cheapest tier's cost per row.

A unit moved when its position changed by more than 0.5 m since its last sample. A
unit that moved and then reads less than 0.5 m from its last sample has stopped: the
agent writes its attitude once.

The DLL reads the rows of vitals, and of positions without attitude when the pump reads
them, inside the pump: the plan gives it the object IDs, and it calls each getter under
`lua_pcall`. The shim's own loops read the other tiers under `pcall`, one call per
batch. A batch that raises stops at the row that raised: the agent leaves that row out
of every tier until the next full enumeration settles it, and goes on with the next
batch.

### Achieved period

A tier's sweep completes when its cursor has passed every row the table held when the
sweep began, or when no row after the cursor is due on this sweep. The achieved period
is the real time between the last two completions, and for the first sweep the time
from the tier's start to its completion; or, when it is longer, the time since the last
completion, so a tier that stalls shows its stall as it grows. A tier that has owed
rows and never completed a sweep reports the time since the first sampling frame on
which it owed them, so a tier that gets no rows shows a period that grows. A period is 0
only for a tier that has never owed rows. A tier with no rows completes once per target
period. The enumeration's period runs from one `ENUM_BEGIN` to the next of two
enumerations that both ended, and before the first ends, from when the first was due;
while an enumeration runs it grows from the last ended one's `ENUM_BEGIN`, so it reads
up to the period plus the enumeration's duration. The agent writes each in the status
block (spec/transport.md) and in the `sampling` LOG.

The slow sweep, of the life and fuel of the rows read once in twelve vitals sweeps,
completes when the vitals tier has completed twelve sweeps. Its achieved period is
reported in the `sampling` LOG in the same way: the longer of the last slow sweep's time
and the time the running one has taken, and 0 before any such row was owed. Ammunition
has no slow sweep, and its key reads 0.

A tier that reads a row only on its turn, in one of every `every` of its sweeps (an idle
unit's position, a row's slow life and fuel), decides each place once per sweep. A batch
that passes the end of the list reads the places after the end in the next sweep: the
batch's close completes the sweep, and those places are decided against the new count.
So every row is read once in each `every` consecutive sweeps, and the slow sweep's period
is the time `every` sweeps took, in which every row was read once.

The status block has no period for subscribed positions. A sidecar measures it from
the samples of the subscribed objects. The agent bounds the
age of every subscribed row at 1.2 s plus one pump wait.

## Full enumeration

An enumeration starts:

- on the first sampling frame of a mission generation;
- on the first sampling frame after a `HELLO`;
- on an `ENUMERATE` command;
- when `enumeration_s` has passed since the last one began.

One that is due while another runs, or while its `ENUM_END` is held for missed rows being
checked (spec/records.md, "Layout: ENUM_END"), waits for it. A running enumeration ends
with no `ENUM_END` when the mission generation changes. A held end is dropped, with no
`ENUM_END`, when the generation changes, the agent detaches or a new `HELLO` arrives, and
is abandoned when the hold lasts one enumeration period (`enum_end_abandoned`). A
detach or a `HELLO` does not stop an enumeration still running: it carries on and writes
its rows and its `ENUM_END`, and the enumeration the `HELLO` queued starts after it. A
new sidecar that never read that `ENUM_BEGIN` counts the `ENUM_END` refused and uses
nothing from it (spec/records.md). The hold counts in the enumeration's duration. The
sidecar then has an `ENUM_BEGIN` with no end, and uses nothing from it (spec/records.md).

It reads `coalition.getGroups(side, category)` for each coalition and group category,
then `coalition.getStaticObjects(side)` for each coalition, one call at a time, then
each group's units. Each call is one row of the tier's cost, and each unit one more.
For each unit and static it reports the row: a new row it adds as a birth would gets an
`OBJECT`; a row the table has gets an `OBJECT` with the enumeration's number or its ID
in a `SEEN` record (spec/records.md, "Layout: SEEN"). A known object costs the shim one
call into the agent, which also counts the item and reads the clock every
`rows_per_clock` items. The agent keeps each row's `OBJECT` payload encoded, and patches
the enumeration's number into it; it encodes the payload again when the row's entities
change, and an empty payload makes the row's next report a full `OBJECT`.

An enumeration that took the `full` flag writes `OBJECT` for every row. The flag is set
by an `ENUMERATE` command, by every `HELLO` and by every event-class record other than a
`LOG` that the link drops, at the moment of the drop, and the next `ENUM_BEGIN` takes it. A `HELLO` that
arrives while an enumeration runs does not change the enumeration's mode, and the
running enumeration continues: it writes `OBJECT`, not `SEEN`, for each row it has not
sent in the new attach, because no `OBJECT` for the row was queued since that `HELLO`.
The enumeration the `HELLO` queued starts after it and takes the flag.

## Commands

| Opcode | Effect |
|---|---|
| `ENUMERATE` | Starts an enumeration, or one more after the one in progress |
| `RATES` | Sets the tiers' target periods; 0 keeps the `agent.toml` default |
| `SUBSCRIBE` | Replaces the subscribed set: the bound rows of the listed entities, their period and the attitude flag |
| `BIND` | Sets `entity` or `group_entity` on table rows (spec/commands.md) |

Rates and subscriptions belong to the sidecar that set them: the agent drops them when
a sidecar of another boot attaches (spec/transport.md, "Attaching").

## Reporting

Every 10 s of real time while attached, the agent writes a `LOG` at `info` with the
message `sampling` and these `data` keys:

| Key | Meaning |
|---|---|
| `share` | The agent's time over real time in the window, system-call frames and events included |
| `events_us` | The agent time of the events handled in the window, µs, which `share` includes: a spawn's births arrive as events in the frame that spawns |
| `window_s` | The window's real time, seconds. Over a span, the agent's mean is the sum of `share` × `window_s` over the sum of `window_s` |
| `syscall_per_s` | `C` |
| `debt_us`, `credit_us` | `D` when above 0, and the credit when `D` is below 0 |
| `pumps`, `frames` | Frames in the window that called the shim's pump, and all frames |
| `frames_over_250`, `syscall_over_250`, `frame_max_us` | Frames whose agent time passed 250 µs, the per-frame cap, counted exactly since the last report, and of those the system-call frames; and the longest frame, µs. The histograms' bucket edge is 256 µs, so they cannot place a p99 against 250 µs. A report counts frames up to the one before it |
| `gap_max_s` | The longest real time, s, between the starts of two consecutive frames the agent pumped in the window. A gap that spans a report boundary counts in the window where it ends; the first frame after attach adds none. The positions check uses it to find stall windows, which a frame count cannot show |
| `empty_pumps` | Pumps in the window that planned no row. They are in no frame count below |
| `check_pumps`, `check_overrun_us` | Of those, the pumps that made existence checks, and the most one ran past `B`. At least one check goes out whatever the budget, so such a pump may pass it |
| `slice_us` | `S` of the last frame |
| `rows` | Rows in the table |
| `births` | Births the shim holds, waiting to be resolved |
| `position_s`, `vitals_s`, `ammo_s`, `warehouse_s`, `enumeration_s` | The achieved periods |
| `forgiven_us` | Debt dropped by the cap in the window |
| `scale` | `k` |
| `overrun_us` | The most a frame's sampling ran past `B` in the window, among frames that swept rows (tiers 3 to 8), a frame with checks included |
| `row_frames`, `overrun_frames` | Frames in the window that swept rows, and of those the frames whose sampling ran past `B` by more than `clock_interval_us`. A frame that made existence checks and also swept rows is a row frame, and its time includes the checks'; only a frame that made checks and no row is a check pump |
| `overrun_sum_us` | What those overrun frames ran past `B` in all, µs |
| `lone_rows` | Frames in the window that took a batch past what was left of `B`: one dear row, or a first batch whose fixed part alone passed it ("Tiers") |
| `maintenance_frames`, `maintenance_overrun_us` | Frames in the window that resolved births or ran the enumeration, and the most one ran past `B`. Their steps are single DCS calls, such as one `coalition.getGroups`, which the clock check cannot split |
| `clock_interval_us` | What `rows_per_clock` rows of the cheapest tier cost: the most sampling can run past `B` between two clock reads |
| `cost_births`, `cost_enumeration`, `cost_subscribed`, `cost_positions`, `cost_stopped`, `cost_vitals`, `cost_ammo`, `cost_warehouses` | Each tier's running cost per row, µs |
| `birth_step_p50_us`, `birth_step_p99_us`, `enum_step_p50_us`, `enum_step_p99_us` | The upper edge of the histogram bucket (spec/transport.md, "Metrics") holding the p50 and p99 of one birth step (one object's reads) and one enumeration step (one DCS call, or one object), since the agent started |
| `birth_step_max_us`, `enum_step_max_us` | The longest such step in the window |
| `batch_us` | The running estimate of a batch's fixed part: planning it, the shim's dispatch and its return, apart from its rows |
| `xcall_us`, `close_us` | The running estimates of the cross-state call and of closing a batch. Each takes at most a quarter of 250 µs from one measurement and decays by 1 % on each pump, so one slow call cannot stop sampling. `xcall_us` also decays on a frame where the gate's cap is below 3 × `x` ("Slice") |
| `starved` | Frames in the window whose fixed work reached the allowance `A`; when above 0 the agent also logs a warning (spec/config.md) |
| `us_fixed`, `us_xcall`, `us_pump`, `us_syscall` | Where the window's agent time went, µs: from the start of `frame` to the slice on frames that are not system-call frames; the cross-state calls; the pumps; the system-call frames. With the tiers' times they do not add up to the window's agent time: the rest is the end of each frame |
| `us_births`, `us_enumeration`, `us_subscribed`, `us_positions`, `us_stopped`, `us_vitals`, `us_ammo`, `us_warehouses` | Each tier's time in the window, µs: its batches from plan to close, and the sweeps for positions |
| `rows_births`, `rows_enumeration`, `rows_subscribed`, `rows_positions`, `rows_stopped`, `rows_vitals`, `rows_ammo`, `rows_warehouses` | Each tier's rows in the window |
| `idle` | Units and movable statics read once in five position sweeps |
| `marked_life`, `marked_ammo` | Rows marked for a read of life, and of ammunition, that still wait |
| `life_sweep_s`, `ammo_sweep_s` | The achieved period of the slow sweep of life and fuel ("Achieved period"); `ammo_sweep_s` is 0, ammunition having none |
| `sweeps` | Sweeps of positions in `frame` in the window |
| `sweep_row_us`, `sweep_batch_us` | The running cost of one row inside the sweep's loop, and of a sweep outside its loop. `cost_positions` is the sweep's whole time over its rows |
| `sweep_cut` | Sweeps in the window that their deadline stopped |
| `sweep_overruns`, `sweep_overrun_us` | Sweeps in the window that ran past their budget by more than the rows between two clock reads cost, and the most a sweep ran past it |
| `sweep_forced` | Sweeps in the window that read one row because their fixed part alone passed their budget ("The sweep"). They are in neither of the two counts above |
| `gone_dead`, `gone_removed`, `gone_missing` | Rows that left the table after an existence check, since the agent started, by the reason the reads gave (spec/records.md, "The object table") |
| `classifier_anomalies`, `classifier_faults` | Since the agent started: `classifier_anomalies` counts existence checks whose reads gave no decision, including a death-event mark over a `MISSING` or anomalous result. `classifier_faults` counts rows that left as `REMOVED` and that a death event named, whether by a mark at the check or by a later event (spec/records.md, "Death events") |
| `enum_end_abandoned` | Since the agent started: enumerations whose hold on pending misses lasted one enumeration period and ended with no `ENUM_END` (spec/records.md, "Layout: ENUM_END") |
| `marks_cleared` | Since the agent started: death-event marks cleared because `isExist` returned true at its check |
| `revivals` | Since the agent started: objects of the dead set that were found alive and added again (spec/records.md, "The object table") |
