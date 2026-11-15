# Maintenance with no row owed

| | |
|---|---|
| Step | The second ruling on births and the enumeration with no row owed: with no row tier owing rows they take a batch only while their balance is at or above 0; while a row is owed, only when it covers one batch. One helper, `maintenance_pays`, serves the plan, the pump's worth and the gate below a scale of 1. |
| Date | 2026-11-15 |
| DCS | Host: costs from S10 and S11 (PRIOR, docs/results/2026-11-13-s10-sampling.md, docs/results/2026-11-14-s11-budget-and-baseline.md). Live: 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, the run's provenance). |
| Platform | Host: macOS on arm64, the sampler driven by the simulated shim of `crates/agent-link/src/sampler_tests.rs` on a made-up clock, and the agent under stock Lua 5.1 (`lua/tests/dispatch.lua`). Live: the Wine server, Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker; agent builds `97b647342528a7bc` (mirror part, release) and `2ab2cd5f12169478` (tenth part, fault injection). |
| Script | `crates/agent-link/src/sampler_tests.rs` and `lua/tests/dispatch.lua`, run by `mise run check`; figures print with `cargo test -p nanoprobe-agent-link --no-default-features --lib -- <name> --nocapture`. Host time: the ignored test `host_time_of_the_attitude_case`. Live: `cargo xtask sampling <target>`. |
| Result | Live: the tool's verdict is FAIL, on items main fails too or that a DCS freeze explains ("Live run"); the agent's share and tail match main and the S11 baseline. Host: every test passes, every assertion at every start offset. The vitals band holds as a rate (4.99 to 5.19 s). With nothing owed every 10 s window's share is at most 0.254 % and nothing is forgiven; in an injected 30 ms stall's window, the share less the forgiven time stays within 0.3 %. The agent's live share against the S11 baseline is in "Live run". |

## The rule

`Sampler::maintenance_pays` is `maintenance_us >= (rows_owed() ? maintenance_batch() : 0)`.
`rows_owed` is a stopped row, an event mark, or a row tier with a row's credit. The floor
(−1,000 µs), the bound −`L`, forgiveness past −`L`, earning at `m` and the separate
accounts are unchanged (spec/sampling.md, "Tiers").

## Host results

All figures are host simulations on the S10 and S11 costs.

**The vitals band as a rate.** The band, 4.5 to 6.5 s, is judged as the target period ×
the rows due at it / the rows the vitals cursor read, summed over the measure: two minutes
for the attitude case, three for the starvation guard. The agent reports
both in two new `sampling` LOG keys, `due_vitals` and `swept_vitals`; reads an event mark
asked for are in neither. The reported period is printed as readings; their maximum is
held at 15 s, three vitals periods, so a stall that grows still fails. A reading shows an
enumeration's wait (spec/sampling.md, "Achieved period"). Every offset runs every
assertion of its test. `the_vitals_rate_keys_add_up_and_leave_out_marked_reads` checks
that the reports' keys add up, with the open window, to what the sampler counted, and
that 1,200 life marks give 1,200 reads in `rows_vitals` and none in `swept_vitals`.

| Case, start offset | Rate | Rows swept of due | Readings mean, median, max | Pumps a second |
|---|---|---|---|---|
| Attitude, 0 s | 5.01 s | 4,430 of 4,440 | 5.47, 5.11, 13.80 s | 3.36 |
| Attitude, 0.37 s | 4.99 s | 4,453 of 4,440 | 5.75, 5.27, 12.94 s | 3.47 |
| Attitude, 1.91 s | 5.04 s | 4,409 of 4,440 | 6.00, 5.10, 11.12 s | 3.49 |
| Attitude, 3.53 s | 5.19 s | 4,279 of 4,440 | 5.96, 5.05, 13.67 s | 3.43 |
| Starvation guard, 0 s | 5.00 s | 6,660 of 6,660 | 5.84, 5.11, 12.16 s | |
| Starvation guard, 0.37 s | 5.01 s | 6,653 of 6,660 | 5.81, 5.05, 12.75 s | |
| Starvation guard, 1.91 s | 5.10 s | 6,526 of 6,660 | 5.55, 5.08, 10.52 s | |
| Starvation guard, 3.53 s | 5.09 s | 6,540 of 6,660 | 5.97, 5.17, 12.61 s | |

The starvation guard's one-row test passes at the same offsets: 62, 62, 62 and 54
warehouse rows in 180 s with the guard, 0 without.

**No row owed.** `maintenance_with_no_row_owed_keeps_the_share`: 2,000 statics, nothing
owed, enumeration steps of 687 and 1,140 µs or of 2,000 µs, at 110 and 56 frames a
second, 210 s each. No 1 s step ends with a row owed, which the test asserts. Every 10 s
window's share is at most 0.254 % (110 fps, the S10 and S11 steps), and nothing is
forgiven. The lowest balance seen at a 1 s step's end is −2,150 µs; the balance stops at
−`L` = −3,000 µs, and `maintenance_forgiven_us` 0 shows it never reached it. Each
enumeration begins within 61 s of the one before, so each ends within its period plus its
duration; every case judges at least one finished pair. The 30 ms stall variant forgives
about 27,750 µs in the stall's window and nothing in any other.

**Unchanged tests.** The 2,000-row enumeration test (positions windows 1.02 to 1.12 s at
110 fps), the 2,000-object positions and check-queue tests, the 250 µs frame tests at
half a frame a second, the k = 0.1 tests and `begin-only-pump` (nothing forgiven, share
within 0.3 % plus the 2 % carry across a window's edges) pass.

## Host time

The sampler and its simulated world for two minutes of the attitude case (13,200
frames at 110 fps), release build, best of five, four runs of each tree on the same
machine while other work ran: main `df88953` 4.79, 4.80, 4.88 and 4.93 µs a frame; this
tree 4.80, 4.90, 5.07 and 5.10 µs a frame. The best runs are equal; what makes the spread
is not measured. Main was benched by adding `host_time_of_the_attitude_case`, unchanged,
to a checkout of `df88953` and running `cargo test --release -p nanoprobe-agent-link
--no-default-features --lib -- --ignored --nocapture host_time` in each tree. The figure
includes the simulated world's time. The agent's live share against the S11 baseline is
in "Live run".

## Live run

One run of `cargo xtask sampling <target>` on 2026-11-15, both parts, the bundle kept
outside the repository. All figures are LIVE. The comparison is the enum-rank live trace
on main (`df88953`'s agent), run the same day with the same tool and scenario, read from
its stored bundle.

| Measure | This tree | Main | S11 baseline |
|---|---|---|---|
| Mean share at 2,000, settled | 0.223 % | 0.226 % | 0.19-0.21 % (runs 13, 14) |
| Mean share at k = 1, tenth, spawn included | 0.292 % | 0.289 % | 0.287 % (rerun 19) |
| Frames over 250 µs, mirror part | 103 of 42,028 (0.25 %) | | 0.19-0.21 % |
| Frames over 250 µs at k = 1, tenth | 21 of 17,480 (0.12 %) | | 37 of 13,141 (0.28 %) |
| Positions median interval at 2,000, settled | 1.003 s | 0.999 s | 0.994-1.008 s |
| Positions median interval at k = 1, tenth | 1.001 s | | 1.042 s |
| `maintenance_forgiven_us` | 0 in every window | 0 | |
| Vitals rows a second at 2,000, settled | 42.5 | 30.9 | |
| Pumps a second at 2,000, settled | 6.8 | 6.5 | |
| Pumps a second at k = 1, tenth, settled | 13.1 | 12.9 | |
| Subscribed rows kept at k = 0.1 | 0.38; 0.51 outside the freeze | 0.52; 0.54 outside its freeze | 0.80 (S11) |

The tool's reported checks at k = 1: at 2,000, settled, the vitals period as a rate is
4.35 s (4,239 rows swept of 3,684 due in 113 s; readings mean 26.4 s, median 23.9 s,
maximum 59.5 s), below the band while vitals repays the rows the fill left; 7.25 pumps a
second. In the tenth part at k = 1, settled, vitals swept 0 of 3,702 rows due in 100 s
(readings mean 119 s) and the pump came 13.1 times a second. Main's run shows the same:
0 vitals rows a second and a reported vitals period of 163 s in that part. Neither the
pumps a second nor the vitals band is met live on either tree.

The checks that fail:

- No stall-free window's positions period over 1.25 s at 2,000: one window reads 3.33 s.
  It is the window that ends as a DCS freeze begins: its 831 frames run at 83 a second,
  and the next window has 5 frames and a 2.97 s frame gap. A gap that spans a report
  boundary counts in the window where it ends, so the freeze's start leaves this window
  marked stall-free. Main's run had a freeze at the same step, and there the gap fell in
  the window itself. The per-object median, 1.003 s, passes.
- At k = 0.1: subscribed positions keep 0.38 of their rows, no enumeration completes, and
  2 of 9 sweeps run past their budget by up to 12.6 µs. DCS froze for about 35 s in this
  phase: two windows of 2 frames with 9.7 and 9.0 s frame gaps, the agent's share 0.002 %
  in them; DCS's own frame rate over the phase was 9.7 a second against 64.8 on main.
  Outside the windows with a frame gap over 1 s the subscribed rows keep 0.51, against
  0.54 on main. The enumeration failed to complete at k = 0.1 in S10, S11 and on main.

## Server

The server was restored after the run: build `365d33e8c837e512` active, the manifest,
versions and `agent.toml` checksum as before, no copies left in the container, no probe
running. Only directory times and nanoprobe's own logs differ.

## Open

- Live, at k = 1 in the tenth part (2,000 rows, 20 subscribed with their attitude), vitals
  reads no row for 100 s while about 1,900 life marks wait, and the pump comes 13 times a
  second; at 2,000 in the mirror part it comes 6.8 times a second. Main does the same. The
  host's attitude case does not reproduce it.
- A frame gap that spans a report boundary counts in the next window, so a window that
  ends as DCS freezes is judged stall-free.
- During an enumeration, maintenance takes the whole pump in most pumps while vitals are
  owed, so a vitals pass can wait 11 to 14 s (the readings above). The rate holds; the
  rank within a pump is left for a later sampler session.
