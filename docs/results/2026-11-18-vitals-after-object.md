# A row's life after an attach follows its OBJECT

| | |
|---|---|
| Step | After a sidecar of a new boot attached, the agent wrote some rows' `VITALS` before their first `OBJECT` after its `HELLO`. That sidecar dropped those samples, and the rows had no life for up to 60 s. With the fix the agent writes no `VITALS` row for an object before that `OBJECT` (spec/records.md, "Samples"; spec/sampling.md, "Tiers") |
| Date | 2026-11-18 |
| DCS | Host: none, the sampler on its simulated shim and the agent on the Lua mock. Live: see "Live run" |
| Platform | Host: macOS 26.6.2 on arm64; Rust 1.99.1; Lua 5.1.5 |
| Script | `mise exec -- cargo test -p nanoprobe-agent-link --lib after_a_` runs both order tests; the figures of "Agent time" print with `mise exec -- cargo test -p nanoprobe-agent-link --lib agent_time_around_an_attach_at_live_costs -- --ignored --nocapture` and `... with_live_attitude_costs_every_tier_holds_and_the_pump_bound_holds -- --nocapture`; the sweep part runs as `tools/luatest.sh` runs `lua/tests/sampling.lua ... sweep` |
| Code | The commit that adds this document. "Main" is `75cb671` with this commit's `crates/agent-link/src/sampler_tests.rs` (and, for the sweep part, its `lua/tests/sampling.lua`) and nothing else; the test files build on main unchanged |
| Tests | `crates/agent-link/src/sampler_tests.rs`: `after_a_new_sidecar_attaches_no_vitals_precede_the_object` and `after_a_resync_subscribed_rows_keep_their_life` (new), `agent_time_around_an_attach_at_live_costs` (new, ignored, a measurement); `lua/tests/sampling.lua`, part `sweep`, its new check `vitals_after_object` |
| Result | The new-boot test fails on main (217 rows' `VITALS` before their `OBJECT` at the first attach) and passes with the fix. At live costs, 516 to 552 of 2,000 rows had no kept life 10 s after their `OBJECT` on main, and none with the fix; the agent's share is 0.197 % on both. The resync test passes on both. Every other agent-link test passes unchanged, the band tests at all four start offsets. The sweep part: see "Sweep part" |

## The cause

A new `HELLO` makes `Sampler::check_attach` (`crates/agent-link/src/sampler.rs`)
request an enumeration. On main it also marked every row that is not read on every vitals
sweep for one life read. A row's `OBJECT` after the `HELLO` is queued only when the
enumeration reaches the row (`Sampler::enqueue_known`). `take_marks` hands out a mark
once the row exists and its last read is old enough, so a marked row could be read and
written before the enumeration reached it. A sidecar of a new boot holds no such row and
drops the sample (`samples_unknown_object`, `crates/sidecar/src/mirror.rs`). The mark
is then spent, and the row's next life read is the slow sweep's, up to 60 s later at the
default `vitals_s`.

A second, smaller path is the slow sweep itself. Its cursor reads a row's life when the
row's turn comes, whether or not the row's `OBJECT` after the `HELLO` is queued yet.

The recording from the Lua flake session shows the first path (2026-11-18; the Lua mock
on a macOS host, debug build, main `ac84a53` plus that session's test-only changes; 10 of
120 attaches showed the fault; PRIOR). After the last attach line: `HELLO` (record 17),
`LIFECYCLE`, `POSITIONS`, `ENUM_BEGIN` (20), the enumeration's first `OBJECT` (21), then
a `VITALS` of one row (22) for object 16792832. That object's `OBJECT` is record 87,
14 ms later, and `ENUM_END` is 88. No later `VITALS` names it in the 17 s recorded. A
one-row `VITALS` right after `ENUM_BEGIN` is a mark's batch: the cursor's batches read
several rows.

In the new-boot test on main, the first attach wrote 217 rows' `VITALS` before their
`OBJECT`. With fix 1 alone (below) it wrote 8: the slow sweep's path.

## The fix

All in `crates/agent-link/src/sampler.rs`.

1. `check_attach` marks no row for a life read. `enqueue_known` marks a row when it
   queues the row's first `OBJECT` since the last `HELLO`, unless a subscription reads
   the row on every vitals sweep. The mark's read then comes after that `OBJECT` in the
   ring: the pump flushes queued records before a batch of samples (in `plan`, and in
   the vitals batch's close), and the link writes no sample while records wait after a
   blocked flush.
2. When a vitals batch closes, a row read that the attached sidecar does not hold is
   left out of the `VITALS` (`drop_early_vitals`), and its read time is not noted. A
   row is not held when it has no row left, or when its last `OBJECT` was queued before
   the first `HELLO` of the attached sidecar's boot (`Sampler::known_from`). Its next
   `OBJECT` marks it, as in 1.

A sidecar keeps its table across a later `HELLO` of its own boot from the same agent
boot (`Mirror::hello` keeps its rows; `hello_resync` in `crates/sidecar/src/replay.rs`).
It clears the table at a new agent boot, or when the `HELLO`'s campaign is not the
store's (`Reconciler::hello`). A new agent process starts with no sidecar boot seen, so
its first `HELLO` holds as a new boot's does. At a resync `HELLO` after `CONTROL_LOST`
the agent holds back no row's life. At a same-boot `HELLO` of another campaign the
reconciler drops the life written before each row's next `OBJECT`; that `OBJECT` marks
the row, and a subscribed row is read again within a vitals period. A new boot that
attached since the frame's `check_attach` counts from the link's `HELLO` count at the
close, so a `HELLO` that the flush in the close writes is counted. A row added after the `HELLO` gets its `OBJECT` and its mark
together in `Sampler::add`.

## Tests

The recorder in `sampler_tests.rs` can model the link's queue (`Rec::attach`): records
wait in a queue until a flush, samples go into the ring at once, a due `HELLO` is
written by the next flush, and samples are dropped until then.

- `after_a_new_sidecar_attaches_no_vitals_precede_the_object`: three attaches of a new
  boot to 2,000 rows, each after hits marked 50 rows. The first two `HELLO`s are written
  at a frame's last flush, as the driver writes them. The third is written by a flush
  inside a pump, after five births queued their `OBJECT`; the driver does not pump
  before a new boot's first `HELLO`, so this order is stricter than the driver's, and
  it is the one that runs the second arm of `known_from`. In the records since each
  `HELLO`, every `VITALS` row follows its object's `OBJECT`, and every row has an
  `OBJECT` and a life within 10 s.
- `after_a_resync_subscribed_rows_keep_their_life`: 20 rows subscribed after the first
  frame's attach (which drops an earlier subscription), at live costs, and three
  same-boot `HELLO`s. Over the three, the 20 rows' life is written before their
  `OBJECT` 60 times, so the hold does not apply to a sidecar that keeps its table.

Each part of the fix switched off fails one of the two:

| Tree | New-boot test | Resync test |
|---|---|---|
| Main | Fails: 217 rows early at the first attach | Passes: 60 |
| Fix | Passes | Passes: 60 |
| Fix 1 only (no `drop_early_vitals`) | Fails: 8 rows early at the first attach | Passes: 60 |
| Hold keyed on every `HELLO` | Passes | Fails: 0 |
| `known_from` always `boot_hellos` | Fails: 62 rows early at the third attach | Passes: 60 |

Neither test fails when both flushes before samples are switched off: in these runs a
row's `OBJECT` and its marked read fall in different pumps, and the frame's last flush
writes the `OBJECT` first. The recorder fails any test whose `VITALS` passes an
`OBJECT` of the same object still in its queue. The flush in the vitals close covers
`finish_pump`, which resolves an undecided existence check, and queues its `OBJECT`,
before it closes the pump's last batch. The shim decides every check it is handed, so no
shim input reaches that order today and no test runs it: the flush is checked by
reading the code.

## Agent time

Host, at the live costs of
`at_live_costs_the_fill_the_enumeration_and_no_row_owed_keep_the_pump_bound` (2,000
objects, 100 frames a second, an enumeration item 10 µs, a `coalition.getGroups` step
570 µs, the call 35 µs; LIVE costs from docs/results/2026-11-15-sampler-live-costs.md).
Two minutes to settle, then five attaches of a new boot, each followed by 60 s.

| Measure | Main | Fix |
|---|---|---|
| Agent share of real time | 0.197 % | 0.197 % |
| Frames over 250 µs | 0.34 % | 0.34 % |
| Of those with no enumeration step | 0 | 0 |
| Longest frame | 745 µs | 738 µs |
| Pumps a second | 6.90 | 6.71 |
| `VITALS` rows before their `OBJECT`, per attach | 2,399 to 2,516 | 0 |
| Rows without kept life 10 s after their `OBJECT`, per attach | 516 to 552 | 0 |
| Longest wait from a row's `OBJECT` to its kept life | 54 to 59 s | 1 to 2 s |

The frames over 250 µs are the modeled `coalition.getGroups` steps (570 µs), on both
trees. The steady-state live-cost test,
`with_live_attitude_costs_every_tier_holds_and_the_pump_bound_holds`, prints the same
figures on both trees at every start offset: share 0.270 %, 0.267 %, 0.268 % and
0.269 %; 8.09, 7.73, 7.81 and 7.92 pumps a second.

## Sweep part

`lua/tests/sampling.lua`, part `sweep`, without the `vitals_before_object` allowance
the Lua flake session added. After the "attached again" compare, which fails on any row
without life, `vitals_after_object` reads the recording from the `HELLO` after the last
attach line and fails on any `VITALS` that names an object before its `OBJECT`.

| Set | Passed | Each run | Load average (1 min) | `VITALS` rows checked | What ran beside it |
|---|---|---|---|---|---|
| Beside one check | 10 of 10 | 108 to 136 s | 4.0 to 23.0 | 83 to 91 | `mise run check` of this tree (exit 0, 23 min), from before the first run to after the last |
| Alone | 10 of 10 | 109 to 164 s | 5.8 to 58.7 | 82 to 129 | No check; this session's host tests at times, and other sessions' work on the same machine |
| Main's agent, alone | 10 of 10 | 108 to 111 s | 3.1 to 11.0 | 83 to 85 | No check; other sessions' work on the same machine |

Main's agent passes the same 10 runs, the order check included. The fault came in about
1 of 12 attaches on the mock (the finding: 10 of 120), so 10 runs of one attach each do
not tell main from the fix, and (11/12)^10 is about 0.42. The sweep sets show that the
part passes without its allowance and that the order holds in every run of the fix; the
host test of the new boot is the evidence that tells the two apart.

Each set runs the part as `tools/luatest.sh` does, on the debug host build, one run
after another, each in a fresh scratch folder.

## Live run

LIVE_RESULTS

## Limits

- `POSITIONS` and `ATTITUDE` of a row can still precede its `OBJECT` after an attach.
  The sidecar drops them, and the row's next position sweep carries them again within
  its period.
- A life read left out under fix 2 is spent work, until the enumeration reaches the
  row: once for the slow sweep's read and for each mark taken, and once a vitals sweep
  for a row a new sidecar subscribed before its `OBJECT`. A left-out cursor read counts
  in the tier's credit and the window's `swept_vitals`; a left-out marked read counts
  in neither.
- `Row::sent` is set when an `OBJECT` is queued. When the link's event queue drops that
  `OBJECT`, the row counts as held: its `VITALS` can precede the `OBJECT` the full
  enumeration writes after the `GAP`, and that `OBJECT` does not mark the row, so its
  next kept life is the slow sweep's, up to 60 s. spec/records.md states the exception.
  It needs the event queue to fill while attached, when a blocked ring holds records
  back, with no `HELLO` before the full enumeration. After a detach the re-attach's
  `HELLO` makes that enumeration's `OBJECT` mark the row, and its life follows within
  seconds. Open item for the ring-overflow work.
- A sample the link drops because the ring is full spends the read that made it, as
  any dropped sample does: the row's mark and read time are already settled when the
  batch is written. After an attach this can still leave a row without life until its
  next read. Open item for the ring-overflow work.
