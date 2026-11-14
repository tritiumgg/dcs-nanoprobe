# Overhead budget and baseline

| | |
|---|---|
| Step | S11 exit test: the agent's cost on the sim thread against the owner's targets (mean share at most 0.3 %, p99 pump at most 250 µs, positions at 1 Hz, every tier at its target, heap and ID growth flat), and the baseline the later steps measure against |
| Date | 2026-11-14. Runs 1 to 19 |
| DCS | 2.9.31.29434 dedicated server on the Wine server (LIVE, `target check`). The Windows client reports `2.9.31.29434 (unknown branch)` in its bundle. |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker, one simulation core. Windows client: DCS hosting a session on Windows (no Windows dedicated server is available). Host rehearsal: macOS on arm64. |
| Builds | Each run names its tree in "Runs". Runs 1 to 3 ran on the tree before the SEEN record. Runs 4 to 7 ran on the tree with the SEEN record, the vitals credit cap and the k < 1 gate. Runs 8, 9 and 12 ran with the sweep-wrap fix, before the review fixes. Runs 13, 14 and 15 ran on the final tree, agent build `79a10956c7e6a67b`. Run 16 ran on the tree of runs 13 to 15 with an earlier build of the soak tool. Run 17 is a control with no agent deployed. Runs 18 and 19 ran on the final tree with the frame-gap key `gap_max_s` and the positions measure: run 18 on agent build `e2133a84dffc0c4a`, rerun 19 on agent build `8b7d8a9fa030bf65`. Rerun 19 is the measurement on the shipped build, the one with `gap_max_s`. The gap key is the only agent difference between build `79a10956c7e6a67b` (runs 13 to 15) and build `8b7d8a9fa030bf65`. |
| Script | `cargo xtask sampling <target>` (`crates/xtask/src/sampling.rs`, `crates/xtask/src/sampling_live.rs`), `cargo xtask sampling <target> --part tenth`, `cargo xtask sampling <target> --part windows` (Windows client), `cargo xtask soak <target> ...` (`crates/xtask/src/soak.rs`); scenario `lua/scenarios/movers.lua`; frame meter `lua/sampling/frames.lua`; `crates/xtask/src/meter.rs` |
| Rehearsal | `lua/tests/sampling.lua`, `lua/tests/meter.lua`, `crates/agent-link/src/sampler_sweep_tests.rs`, `crates/agent-link/src/sampler_seen_tests.rs`, `crates/sim-fake/tests/seen.rs`, `crates/fake-agent/tests/chaos.rs`, all run by `mise run check` |
| Bundle | `bundles/sampling-<target>-<stamp>/`, `bundles/soak-<target>-<stamp>/` (not committed: they hold server data) |
| Result | PASS with open items. Mean share (0.18 to 0.22 %) and exact p99 (0.14 to 0.21 % of frames over 250 µs) meet the targets on the Wine server, and the Windows client meets both with wide margin. Positions meet the 1 Hz target under the adopted measure (median per-object interval over stall-free windows, limit 1.05 s): runs 13 and 14 by proxy (0.994 to 1.008 s), and the tenth rerun at k = 1 (1.042 s). The forced-collect soak and its control show the live heap growth is DCS's (about 7.2 MB an hour with the agent, 7.8 without), and RSS growth is equal with and without the agent. The fill-window regression of runs 8 and 9 (3.2 to 3.7 s) is gone on the final tree. The open items list what is not met. |

All figures are LIVE unless tagged.

## Result

Targets as written: mean share at most 0.3 %, p99 pump at most 250 µs, positions
at 1 Hz, every tier at its target, heap and ID slopes flat. Figures are from the
latest run of each case (runs 13 and 14 on the Wine server for 2,000 objects, run 9 for the 1,200 and 500 rows, run 15 on the Windows client, runs 16 and 17 for heap, rerun 19 for the tenth part).
Positions are judged as the median per-object interval over stall-free windows (see "Positions").
Share is agent time over real time. p99 is judged on frames over 250 µs: it holds when
at most 1 % of frames pass 250 µs.

| Case | Mean share | p99 within 250 µs | Positions (reported / median interval) | Tiers at target | Result |
|---|---|---|---|---|---|
| Wine server, 2,000 objects, a fifth routed | Run 13: 0.211 % settled at 2,000 (max 10 s window 0.30 %); 0.22 % from the 800 spawn on (max 0.34 %). Run 14: 0.19 % settled (max 0.30 %); 0.208 % from the spawn (max 0.30 %) | Run 13: 85 of 40,523 frames over (0.21 %), max 3,312 µs. Run 14: 77 of 41,417 (0.19 %), max 3,426 µs | Under the adopted measure, by proxy: run 13 median 1.008 s, worst stall-free window 1.084 s. Run 14 median 0.994 s, worst 1.012 s. (Old single-reading check, replaced: run 13 0.995 s, run 14 1.072 s.) | Mirror matches in run 13; fill window worst 1.02 s (run 14: 1.05 s) | PASS |
| Wine server, 1,200 objects, a fifth routed | 0.19 % over 30 minutes (soak); steady windows 0.16 to 0.27 % | The run's total: 81 of 40,258 | 0.963 s / 0.964 s | Mirror matches; vitals oldest 61.1 s | PASS |
| Wine server, 500 objects, all moving | Steady windows 0.13 to 0.15 % | The run's total: 81 of 40,258 | 1.024 s / 1.018 s (run 8: 0.955 s / 0.957 s) | Mirror matches; vitals oldest 60.1 s | PASS (median interval 1.018 s, under the 1.05 s limit of the adopted measure) |
| Windows client, 1,200 objects, a fifth moving | 0.043 % (run 15) | 0 of 14,360 frames over; max 212 µs | Recorded median 1.000 s | Vitals oldest 57 s; mirror compare fails on the snapshot lag (see "Windows") | PASS on share and tail; the mirror compare is an open item |
| Windows client, 2,000 objects, a fifth moving | 0.055 to 0.060 %; max window 0.16 % (run 15) | 9 of 13,850 frames over; max 311 µs | Recorded median 1.013 s | Vitals oldest 57 s; mirror compare fails on the snapshot lag | PASS on share and tail; the mirror compare is an open item |
| Wine server, 1,200, churn, forced-collect soak 90 minutes (run 16) and control without the agent, 60 minutes (run 17) | Run 16: 0.184 % | 0.14 % of frames over 250 µs; max 4.6 ms | | Heap floors rise 85.5 to 92.7 MB (about 7.2 MB an hour) with the agent and 84.9 to 91.4 MB (about 7.8 MB an hour) without: the live heap growth is DCS's under churn. RSS growth equal with and without the agent. IDs: 256 per spawned object, so ID growth follows the churn rate. Mirror rows flat. | PASS (heap and RSS growth are DCS's; see "Soak") |
| Tenth part at k = 1 (rerun 19) | 0.287 % | Exact tail at k = 1: 37 of 13,141 frames over (0.28 %) | Median over stall-free windows 1.042 s | Mirror matches; fill 41 s | PASS (enumeration windows reported; see "Tenth part") |

DCS runs at 111.8 fps with 1,200 objects and 113 fps with 500 on the Wine server, 56 to 60
fps (p5 0.40 to 0.45) with 2,000, and at a steady 72 fps on the Windows client. Model time
over real time reads 0.98 to 1.00 in every case.

## Scenario and why

`spawn_mixed(n)` (S10) with `lua/scenarios/movers.lua`: ground groups of 10 in lanes of
their own, driving at 6 m/s. The judged case is 2,000 objects with a fifth of the
ground groups routed, because DCS itself limits the simulation on one core:

| Objects moving | DCS fps (Wine server) |
|---|---|
| A fifth of 2,000 | about 51 |
| All of 1,000 | about 72 |
| All of 2,000 | 1 to 8 |

At 2,000 moving, DCS stalls below 1 fps and the agent's share is measured against a
simulation that barely runs. A fifth routed keeps the object count at 2,000 and the
moving share at 0.31 to 0.33, and DCS holds 50 to 63 fps with p5 near 0.4 (stalls).
The other sizes are 1,200 with a fifth routed and 500 with every group routed (1,000
all routed is the capacity figure above). Every run records fps, model over real time
and the moving share.

## Runs

| Run | Bundle stamp (UTC) | Tree | Part | What it showed |
|---|---|---|---|---|
| 1 | `sampling-<target>-20261114T103010Z` | Before the SEEN record, no vitals cap | Wine, mirror | Mean 0.18 % settled (0.20 % from spawn), max window 0.30 %; 75 of 41,595 frames over 250 µs, max 1,163 µs; positions 0.991 s, median 1.002 s; fps 57 (p5 0.4). FAIL on the mirror: the life sweep took 68 to 131 s and rows were up to 208 s old. |
| 2 | `...T104102Z` | Same | Wine, tenth | See "Tenth part". k = 1 matches; k = 0.1 fails fill, enumeration and the abandon check. |
| 3 | `soak-<target>-20261114T105318Z` | Same | Wine, soak 30 min, 1,200, churn | See "Soak". |
| 4 | `...T113204Z` | SEEN, vitals cap, k < 1 gate | Wine, mirror | Positions 1.001 s, median 1.017 s: FAIL by 1 ms, in a DCS stall window. Mean 0.20 % settled; 69 of 41,149 over 250 µs, max 977 µs. 500 and 1,200 match; at 2,000 one row 146 s old. fps 54 (p5 0.40). |
| 5 | `sampling-<target>-20261114T113337Z` | Tree of run 4 | Windows, windows | 72.0 fps; mean 0.044 % (1,200), 0.060 % (2,000); 0 and 10 of 14,151 frames over 250 µs, max 372 µs; positions median 1.014 s and 1.001 s; fill 4 to 6 s. Mirror compare fails (see "Windows"). |
| 6 | `...T114300Z` | Tree of run 4 | Wine, mirror | Positions 0.992 s, median 0.987 s. Fails only on stale vitals at 2,000 (23 rows). |
| 7 | `...T115232Z` | Tree of run 4 | Wine, mirror | Positions 0.983 s, median 0.997 s. Fails only on stale vitals at 2,000 (14 rows). 0.18 % and 0.24 % of frames over 250 µs in runs 6 and 7, max 769 µs. |
| 8 | `...T120726Z` | Sweep-wrap fix, before review fixes | Wine, mirror | Every size matches; vitals oldest 44 to 61 s. Positions 1.002 s (FAIL by 2 ms), median 0.977 s. Fill window 3.21 s against 3 s. 64 of 39,671 frames over 250 µs, max 764 µs. |
| 9 | `...T121659Z` | Same | Wine, mirror | Every size matches. Positions 0.986 s, median 0.990 s. Fill window 3.73 s against 3 s. 81 of 40,258 over 250 µs, max 2,951 µs. |
| 10 | `soak-<target>-20261114T122626Z` | Sweep-wrap fix | Wine, soak 90 min | See "Soak". Mean share 0.182 %; heap verdict INCONCLUSIVE (one cycle floor). |
| 11 | `soak-<target>-20261114T140035Z` | Sweep-wrap fix; no agent deployed | Wine, soak control 30 min (`--no-sidecar`) | See "Soak". RSS +10.9 %/h without the agent; heap 1.1 MB/min, no cycle. |
| 12 | `sampling-<target>-20261114T120927Z` | Sweep-wrap fix, before review fixes | Windows, windows | `old_vitals` 0 (oldest 58 s). Positions: 140 of 1,200 and 231 of 2,000 rows flagged old, all at one age (see "Windows"). 15 of 32,849 frames over 250 µs, max 394 µs. 72.0 fps. |
| 13 | `...T143647Z` | Final tree, build `79a10956c7e6a67b` | Wine, mirror | PASS. Positions 0.995 s, median 0.983 s, clean windows 1.012 s. Fill window worst 1.02 s. 85 of 40,523 frames over 250 µs, max 3.3 ms. 55.8 fps. |
| 14 | `...T144622Z` | Same | Wine, mirror | FAIL on positions only: 1.072 s, median 0.981 s, clean windows 0.968 s. Two DCS stall windows (308 and 7 frames) lie in the span, at 6.7 and 8.2 s. Fill window worst 1.05 s. 77 of 41,417 over 250 µs. 59.6 fps. |
| 15 | `sampling-<target>-20261114T143646Z` | Same | Windows, windows | 72 fps; recorded medians 1.000 s and 1.013 s; vitals oldest 57 s. 28 of 195 rows flagged old, all at one age (2.07 to 2.22 s). 0 and 9 frames over 250 µs, max 311 µs. See "Windows". |
| 16 | `soak-<target>-20261114T145655Z` | Final tree, earlier soak tool | Wine, forced-collect soak 90 min (`--collect-every 15`) | See "Soak". Share 0.184 %; heap floors 85.5 to 92.7 MB. |
| 17 | `soak-<target>-20261114T163101Z` | Final tree; no agent deployed | Wine, forced-collect control 60 min (`--no-sidecar --collect-every 10`) | See "Soak". Heap floors 84.9 to 91.4 MB. |
| 18 | `sampling-<target>-20261114T173603Z` | Final tree with `gap_max_s` | Wine, tenth | FAIL at k = 1 on positions and fill. See "Tenth part". |
| 19 | `...T175707Z` | Same | Wine, tenth (rerun) | k = 1 passes. See "Tenth part". |

## Positions

Positions stay at 1 Hz. The measure (maintainer decision, 2026-11-14) has two checks over
the settled span:

- The median per-object interval over stall-free windows is at most 1.05 s.
- No stall-free 10 s window reports a position period over 1.25 s in the mirror part.

A stall window is a window with a gap between DCS frames over 1 s (agent report key
`gap_max_s`). The window after a stall is excluded too, because the reported period spans
the stall. Stall windows are reported and not judged
(`crates/xtask/src/sampling_positions.rs`). The measure replaces the old check, a single
reading of the reported period against 1 s.

The limits follow from the data. Medians over seven runs read 0.977 to 1.017 s.
Stall-free window readings reach 1.14 s. The sweep is paced to exactly 1.000 s, so a
single reading passes or fails by chance, and a limit of 1 s on it decides nothing.

Re-judge of runs 13 and 14 from their bundles. The bundles predate `gap_max_s`, so stall
windows come from the code's proxy rules (a window with fewer frames than seconds, a window
that runs over 1 s past its nominal 10 s, and a window under 0.7 of the median frames):

| Run | Median interval over stall-free windows (s) | Worst stall-free window (s) | Result |
|---|---|---|---|
| 13 | 1.008 | 1.084 | PASS |
| 14 | 0.994 | 1.012 | PASS |

The tables below keep the old check for the record. The median interval is the median gap
between the recorded samples of one moving object. The split reads the interval over the windows free of DCS stalls ("clean"), and
over windows in which the agent repays credit (it reads extra rows to catch up) against
the others. Clean windows are the windows at 2,000 rows, leaving out each window under
80 % of the median frame count and the window after it.

| Run | Old check (s), replaced | Median interval (s) | Clean windows (s) | Repaying windows (s) | Other windows (s) |
|---|---|---|---|---|---|
| 4 | 1.001 | 1.017 | 1.010 | 1.038 | 0.989 |
| 6 | 0.992 | 0.987 | 0.994 | 0.999 | 0.994 |
| 7 | 0.983 | 0.997 | 1.090 | 0.974 | 1.177 |
| 8 | 1.002 | 0.977 | 1.033 | 1.009 | 1.051 |
| 9 | 0.986 | 0.990 | 1.006 | 0.996 | 1.012 |
| 13 (final) | 0.995 | 0.983 | 1.012 | 1.017 | 1.008 |
| 14 (final) | 1.072 | 0.981 | 0.968 | 0.954 | 0.973 |

Repaying windows are not slower than the others, and clean windows are not faster. The
spread between runs (0.983 to 1.002 s) reads as stall noise (UNVERIFIED).

Run 14's old check read 1.072 s. Its span holds two DCS stall windows (308 and 7 frames,
at 6.7 and 8.2 s), and its clean windows read 0.968 s with a median interval of 0.981 s.
Under the adopted measure run 14 passes (see the re-judge above).

The fill window (800 objects spawned onto 1,160 rows) reads a worst positions period of
1.02 s in run 13 and 1.05 s in run 14, against 3 s. The 3.2 to 3.7 s of runs 8 and 9 does
not recur on the final tree.

## Vitals

The life sweep reads every row's life and fuel once in a cycle of 60 s.

| Run | Life sweep | Oldest vitals sample |
|---|---|---|
| 1 (no credit cap) | 68 to 131 s | 208 s |
| 4 (credit cap) | 59 to 61 s outside the stall window | 146 s at 2,000, one row |
| 6, 7 (credit cap) | 59 to 61 s | Stale at 2,000: 23 and 14 rows |
| 8, 9 (cap and sweep-wrap fix) | Reported vitals period 3.2 to 6.2 s | 44 to 61 s, no row stale |

The credit cap is max(n, rows) for one slow cycle. A stall or an enumeration cannot
bank more than one cycle of credit, so the repayment after it reads at most one cycle
of rows. After a stall the agent repays in bursts of 784 to 1,219 rows a window (run 4).
The vitals band of 4.5 to 6.5 s is unchanged; the reported period dips below it
while the agent repays (3.2 s in run 9).

The sweep-wrap bug. A batch that reaches the end of the row list wraps to the start
and continues in the same pass, so some rows skip cycles. `life_sweep_s` counts passes,
not cycles, so it hid the skips. The fix keeps each row's read to its turn: a row is
read once in its cycle, whatever the batch sizes (`crates/agent-link/src/sampler_sweep_tests.rs`
checks the longest gap between two reads of a row).

Host test figures for the wrap (`crates/agent-link/src/sampler_sweep_tests.rs`). Before
the fix a row waited 24 passes and 120.85 s between reads while the slow period read
60 s. After the fix a row waits at most 13 passes.

Windows evidence. Run 5 (before the fix) flagged `old_vitals` 2 of 13 rows, oldest 322 s.
Run 12 (after the fix) flagged 0, oldest 58 s.

## Exact p99

The pump histogram's bucket edges put p99 at "under 256 µs" for both runs of the S10
tree, so that figure cannot tell 240 µs from 255 µs. The sampling LOG carries exact
counts: `frames_over_250` (frames whose agent time passed 250 µs),
`syscall_over_250` (those that ran the system-call work) and `frame_max_us`. The
xtask summary reads p99 as within 250 µs when frames over 250 µs are at most 1 % of
frames (`tail.p99_within_250`).

| Run | Frames | Over 250 µs | Share | Max (µs) |
|---|---|---|---|---|
| 1 | 41,595 | 75 | 0.18 % | 1,163 |
| 4 | 41,149 | 69 | 0.17 % | 977 |
| 8 | 39,671 | 64 | 0.16 % | 764 |
| 9 | 40,258 | 81 | 0.20 % | 2,951 (7 with system calls) |
| 12 | 32,849 | 15 | 0.05 % | 394 |
| 13 | 40,523 | 85 | 0.21 % | 3,312 (4 with system calls) |
| 14 | 41,417 | 77 | 0.19 % | 3,426 (7 with system calls) |
| 10 (soak) | 613,747 | 878 | 0.14 % | 3,300 |

Soak (1,200 objects, 30 minutes): 445 of 206,913 frames over 250 µs (0.22 %), with
one minute holding 143 of its 6,676 frames. That minute holds a 3.0 ms frame, the
longest of the run. The other minutes read about 0.1 %. The minute coincides with the
heap's collection (see "Soak"), so the long frame is a garbage-collection step that
ran inside agent-timed calls (UNVERIFIED as the cause; see "Open items").

## SEEN record

The SEEN record is a per-object mark kept in the agent's sampler. Host figures are as
measured.

| Measure | Before | After |
|---|---|---|
| Host, cost an item (µs), three scenarios | 2.03 / 2.97 / 2.92 | 1.79 / 2.91 / 1.93 |
| Host, enumeration (ms), three scenarios | 15.7 / 14.9 / 15.6 | 14.3 / 14.3 / 15.6 |
| Agent-side `seen()` (release, ns) | 23 | 10 |
| LIVE, cost an item (µs) | 6.24 | 6.09 |

The host saving is a few tenths of a microsecond on one scenario and none on the other
two. The LIVE saving is 0.15 µs of 6.2. The SEEN record saves nothing that matters. The
cost of an item is DCS's own calls and the shim's, not the agent's bookkeeping.

## Candidate C

Candidate C changes how the agent makes its cross-state call (`net.dostring_in`). It is
not adopted. An empty `net.dostring_in('server', 'return')` costs 7.0 µs
median (17 µs p99), under the 10 µs rule; a pump string calling a no-op costs 18.3 µs
median (27 µs p99). The call is about 63 µs a second of about 1,700 µs of agent time a
second, 4 %, under the 20 % rule. Neither threshold is met, so the call stays.

## Tenth part and the k < 1 gates

At a budget scale k below 1 the agent's rows are fewer. The k < 1 gate stops a pump
from running when it cannot pay a step, so a pump that does nothing is rare.

Rehearsal, five runs with the gate against three without (mock, debug build):

| | With the gate (5 runs) | Without (3 runs) |
|---|---|---|
| Empty pumps | 8 to 10 % | 41 to 45 % |
| Overrun frames | 41 to 69 % | 41 to 69 % |
| Subscribed positions kept | about 1.0 | about 1.0 |
| Positions kept | 0.39 to 0.45 | 0.39 to 0.45 |

The mock asserts empty pumps at most 20 %. The overrun count is reported and not
failed. Overruns read 41 to 69 % of row frames with and without the gate, so the
count does not separate the two. At k = 0.1 a batch's fixed part alone passes k·B, so
a row frame passes its slice by construction. The rule is the bound on the sum of
overrun time, not on the number of frames. The live part judges the bound.

LIVE (run 2, fault-injection build, 2,000 objects, 20 units subscribed):

| | k = 1 | k = 0.1 |
|---|---|---|
| Mirror matches | yes | no: 3 objects missing, as in S10 |
| Share | 0.287 % (max window 0.357 %) | 0.19 % |
| Fill | 45 s for 2,000 births | 122 s for 50 births (twice-expected limit missed) |
| Positions | median interval 1.19 s; worst fill period 28 s (S10: 72 s) | Subscribed rows at most 4.22 s apart |
| Enumeration | | Not completed in 296 s |
| Row frames past k·B | | 13 of 55, largest 52.5 µs |

In the last 7 windows of the k = 0.1 part, 50 births and 1,931 life marks were pending,
the cross-state call cost about 110 µs a pump, and 8 % of frames passed 250 µs (max
about 500 µs); the whole part had 1.5 % over 250 µs. Nothing in the call grows with
the backlog. A pump that only resolves births at k < 1 cannot pay a step. The gate
covers births-only and maintenance pumps for that reason. The 110 µs call cost is
inferred as a garbage-collection step (UNVERIFIED).

### Final tree (runs 18 and 19)

Run 18 (`sampling-<target>-20261114T173603Z`) read worse than run 2 at k = 1: mirror
positions 9.6 s against 1.2 s, fill 79 s against 45 s, worst fill-window period 71.8 s
against 28 s. Two causes explain it, and neither lies in the agent (a trace found no agent
cause). DCS varies: births cost 27.6 to 42 µs a row against 25 to 27.5 in run 2, and the
mirror runs before and after S11 show the same cost a birth. The measure had two artifacts:
the last window was an enumeration window, and the fill-from-empty period equals the fill
duration by definition. DCS ran at 24.9 fps with p5 0.10 in the run.

Run 18 at k = 0.1: one enumeration completed, births drained to 0, 50 births took 123 s,
one extra row sat in the mirror. A row batch passed k·B by 1,484 µs, against 52.5 µs in
run 2. That batch fell in a window holding an enumeration. The "key missing" of the
ENUM_END-hold check was tooling: the reports were read after the probe's files were
removed. The tool reads the local copy of the reports.

Rerun 19 (`...T175707Z`), final tree:

| | k = 1 | k = 0.1 |
|---|---|---|
| Mirror matches | yes | |
| Share | 0.287 % | |
| Fill | 41 s | 50 births in 122 s |
| Positions | median per-object interval over stall-free windows 1.042 s (PASS); no stall windows | Subscribed rows kept 0.80 of their rate |
| Exact tail | 37 of 13,141 frames over 250 µs | 7 of 18,722 (fault injection, reported) |
| Enumeration | windows of 5.9, 15.9 and 6.7 s with zero position rows (reported) | None completed |
| Row frames past k·B | | Largest 47.1 µs; 10 of 67 sweeps past budget, up to 20.4 µs |
| ENUM_END hold | 0 abandoned | |

The judged tail count is the in-run figure: 37 of 13,141 frames over 250 µs at k = 1, and
the longest k = 1 frame is 5,601 µs. `--rejudge` reads 38 of 14,278, because it takes every
k = 1 report in the probe log, including one that the in-run judgement skips.

The stored run 19 bundle's `sampling.json` marks the stall-free window ceiling check
`ok: false` (worst window 15.9 s), because the tool that wrote it predates the
reported-only ruling for enumeration windows. The judgement in this document follows the
ruling.

Rulings of 2026-11-14 on the tenth part. The exact tail is judged over k = 1 spans; k = 0.1
spans are fault injection and are reported. Enumeration windows at k = 1 are reported and not
judged: the host's positions sweep waits while an enumeration is due or running, and at
k = 1 here the budget sits at its cap, so the sweep waits through the enumeration. Run 2
shows the same (17 and 27 s).

## Chaos leak

Cause: the chaos runner killed only the shell that ran `dcs.sh`. The agent process
that the shell started in the background lived on, and could hold the next test's
pipe of another test.

Fix: the runner starts each run in its own process group and kills the group;
`assert_nothing_left` fails a test that leaves a process or a temp folder. The misbehave
test pair and the agent-link test folder guard remove their folders on drop.

Proof: the leak reproduces on commit 693fcb1 and with the group kill disabled, and
does not reproduce with the fix.

## Soak

`cargo xtask soak <target>`: 1,200 objects, a fifth routed, a churn of 25 units every
30 s, 30 minutes (bundle `soak-<target>-20261114T105318Z`, 10:53 to
11:27 UTC). The run fills in 29.8 s and the share holds the limit: mean 0.193 %, 1,774
objects destroyed and 2,974 spawned in all, the mirror holding 1,200 rows.

| Measure | Value |
|---|---|
| Agent share, whole run | 0.193 % |
| Frames over 250 µs | 445 of 206,913; about 0.1 % a minute, 143 of 6,676 in the GC minute |
| Longest frame | 3.0 ms (the GC minute) |
| Lowest heap | 113 MB at minute 1, 156 MB at minute 24, then a cycle to 89 MB |
| RSS | 4.27 GB at the start, 4.70 GB near the end, slowing after minute 20 |
| IDs | 256 per spawned object |
| fps | 111.6 |

The tool's verdict on heap and RSS is inconclusive: the fitted span (from minute 15)
holds the collection, so the heap slope's noise (113 % an hour) exceeds the 1 % an
hour threshold. The RSS slope reads 9.9 % an hour, with noise 1.9 %, and is
unresolved. The ID slope is resolved at 5.3 % an hour and follows the churn:
256 IDs per spawned object is DCS's allocation, not the agent's.

### 90-minute soak and the control (cycle-length evidence)

LIVE, 2026-11-14, DCS 2.9.31.29434 under Wine 10.0. Run 10 is a 90-minute soak (bundle
`soak-<target>-20261114T122626Z`) and run 11 is a 30-minute control without the sidecar
(`--no-sidecar`, bundle `soak-<target>-20261114T140035Z`). Both run 1,200 objects with churn.

| Measure | Run 10 (90 min, agent) | Run 11 (30 min, control) |
|---|---|---|
| Agent share, whole run | 0.182 % | no agent |
| Frames over 250 µs | 878 of 613,747 (0.14 %) | no agent |
| Longest frame | 3.3 ms, in minute 26 | no agent |
| fps | 112 | 112 |
| IDs | 256 per spawned object | not read (no mirror) |
| Mirror rows | flat | no mirror |
| RSS slope | +12.2 % an hour | +10.9 % an hour (unresolved) |
| RSS in the first 25 minutes | +0.40 GB | +0.40 GB |
| Heap slope before the first cycle end | 1.5 MB a minute | 1.1 MB a minute |
| Heap | 117 MB at the start, 155 MB, cycle end to 91 MB near minute 26 to 27, then 188 MB at minute 90 | no cycle |

RSS grows by the same 0.40 GB in the first 25 minutes with and without the agent, so
the RSS growth is DCS's.

The GC minute. The longest frame, 3.3 ms, falls in minute 26, the minute of the heap's
cycle end. The debt cap forgave 176 µs there once, so the span check reads false.

Cycle-floor verdict: INCONCLUSIVE. The heap reaches one floor (91 MB, near minute 26 to
27) and then 188 MB at minute 90, and no second cycle end appears 63 minutes later. A
cycle longer than 60 minutes gives one floor, and one floor fixes no slope. The soak tool
takes `--collect-every N`, which runs a forced full collect in the server state and
reads exact floors.

### Forced-collect soak and its control

LIVE, 2026-11-14, DCS 2.9.31.29434 under Wine 10.0, 1,200 objects with churn. Run 16 is a
90-minute soak with a forced full collect every 15 minutes (bundle
`soak-<target>-20261114T145655Z`). Run 17 is a 60-minute control without the agent, with a
forced collect every 10 minutes (bundle `soak-<target>-20261114T163101Z`). Each collect
reads an exact heap floor. The collect eval takes 1.07 to 1.23 s round trip.

| Measure | Run 16 (90 min, agent) | Run 17 (60 min, control) |
|---|---|---|
| Heap floors | 85.5, 87.4, 89.4, 91.0, 92.7 MB at minutes 15 to 75 | 84.9 to 91.4 MB at minutes 10 to 60 |
| Heap floor slope | about 7.2 MB an hour | about 7.8 MB an hour |
| RSS slope | +9.9 % an hour | +11.5 % an hour |
| Agent share, whole run | 0.184 % | no agent |
| Frames over 250 µs | 0.14 % | no agent |
| Longest frame | 4.6 ms, minute 56 (no collect in that minute) | no agent |
| IDs | 256 per spawned object | not read |
| Mirror rows | flat | no mirror |

The live heap grows by the same amount with and without the agent, so the growth is
DCS's under churn. RSS growth is equal with and without the agent (+0.40 GB in the first
25 minutes in runs 10 and 11). The agent's share is 0.184 % and its tail is 0.14 %.

Run 16's tool verdict reads INCONCLUSIVE. The run predates the last-record fix of the soak
tool, which left 2 floors after minute 45 for the fit. The paired comparison of floors
with and without the agent is the result.

### Growing slowest enumeration step

The longest agent frame of a minute grows by about 8 µs a minute, from 450 to 1,200 µs over
90 minutes, in runs 3, 10 and 16 (also before the SEEN record). The cause is the slowest
step of the once-a-minute enumeration (`enum_step_max_us`): 484 µs grows to 1,138 µs over
90 minutes under churn and flattens at minutes 80 to 90, with the same curve in all three
runs. The typical enumeration step is flat (p50 4 µs, p99 64 µs), and the enumeration's
total cost is flat. The slowest birth step grows from 125 to 268 µs. No agent structure that
grows is found in the code. The cause is inferred to be one DCS call that slows with churn
(UNVERIFIED). It is an open item.

## Windows

`cargo xtask sampling <target> --part windows` against a DCS client hosting a session
(runs 5, 12 and 15; DCS 2.9.31.29434, branch unknown in the bundle). The machine is
restored after each run. DCS runs at a steady 72.0 fps (p5 71.9); the agent's share
is 0.044 to 0.062 %.

The mirror compare fails on both runs, from the probe and not from sampling. Run 5
read the oracle 1.6 s after the dump, which put every position 1.6 s older than the
reported one; the oracle is read first. Run 12 flags 140 of 1,200 and 231 of 2,000 rows
old, and every flagged row has one identical age (2.168 s and 2.193 s). The probe's
mirror snapshot lags the ring by about 1.2 s on this machine. Recorded intervals read
0.999 s and 1.000 s (median), which are the sampling rate. The vitals check passes after
the sweep-wrap fix (0 stale, oldest 58 s).

Run 15 (final tree, oracle read first): 72 fps; recorded interval medians 1.000 s and
1.013 s; vitals oldest 57 s. 28 of 195 rows are flagged old, all at one age (2.07 to
2.22 s). Reading the oracle first does not remove the age. A single age for every row
is a lag in the mirror snapshot in the probe's run path on the client-hosted session,
not in the agent (UNVERIFIED beyond this pattern). The run path of the probe is an open
item.

## Open items

Every item below is for a later step.

- DCS stalls under 1 fps at 2,000 objects (p5 0.4 to 0.5 fps), so the positions rate
  there is judged over stall-free windows (maintainer decision, 2026-11-14).
- Fill-window positions read 3.21 s and 3.73 s in runs 8 and 9 against 3 s, and
  1.77 to 1.83 s in runs 4, 6 and 7. A trace finds no code cause: laps read nearly
  all rows (1,090 to 1,240 against about 700) while DCS ran at 754 to 820 frames
  in a window against 932 to 942. The final tree reads 1.02 s and 1.05 s (runs 13
  and 14).
- The Windows probe's mirror snapshot lags by one age on the client-hosted session
  (run 15). The probe's run path needs a fix.
- GC steps run inside agent-timed calls, so a collector step counts as agent time.
  The 3.0 ms frame of the soak and the 110 µs pumps at k = 0.1 are consistent with
  it (UNVERIFIED).
- The positions tier loses credit on idle rows, and rows are read twice after a HELLO.
- The sidecar has no check of the interface version at HELLO (a HELLO interface gate).
- Enumeration rank against positions and births is one design question. At the budget cap
  the positions sweep waits on a due or running enumeration, for 6 to 27 s at k = 1 (rerun
  19: 5.9, 15.9 and 6.7 s; run 2: 17 and 27 s). A periodic enumeration pre-empts pending
  births for about 30 s.
- Subscribed rows keep 0.80 of their rate at k = 0.1 (rerun 19), against the ruled 90 %.
- No enumeration completes at k = 0.1 in rerun 19 (run 18: one).
- 50 births take 122 s at k = 0.1 (run 18: 123 s).
- 10 of 67 sweeps pass the budget by up to 20.4 µs at k = 0.1.
- The sampler's `dead` set grows by one per destroyed object and is cleared only on
  reset. It is never iterated. It is a bounded memory leak to prune.
- The slowest enumeration step grows from 484 to 1,138 µs over 90 minutes under churn
  (see "Growing slowest enumeration step"). A per-step-kind maximum in the report would
  name the DCS call.
