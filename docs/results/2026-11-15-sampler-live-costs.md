# Sampler rules at live row costs

| | |
|---|---|
| Step | The rulings of 2026-11-15 on sampler rules at live row costs and on positions held during an enumeration: the unaffordable mark's fixed clock, attitude rows read only in their own pump, the sweep not waiting for the subscribed pump while births or an enumeration wait, and the pump bound in place of "under 5 pumps a second". |
| Date | 2026-11-15 |
| DCS | Live: 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, the run's provenance). Host: live costs read from the enum-rank and maint-fix live runs of the same day and this run (LIVE, below). |
| Platform | Host: macOS on arm64, the sampler driven by the simulated shim of `crates/agent-link/src/sampler_tests.rs` on a made-up clock. Live: the Wine server, Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker; agent builds `854463705b4ba20c` (mirror part, release) and `d1790c6d1fd1158f` (tenth part, fault injection), from commit `e9a065b`. |
| Script | `crates/agent-link/src/sampler_tests.rs`, run by `mise run check`; figures print with `cargo test -p nanoprobe-agent-link --lib <name> -- --nocapture` (`--features faults` for the k = 0.1 test). Live: `cargo xtask sampling <target>` (`crates/xtask/src/sampling_live.rs`). |
| Result | Live: the owner's targets hold in both parts (mean share 0.22 % and 0.29 %, frames over 250 µs 0.24 % and 0.20 %). The tool's verdict is FAIL on ruled bounds: the tenth part's vitals still read no row at k = 1, the mirror's vitals rate is 4.20 s (fast, repaying), and five 10 s windows pass the pump bound by 3 to 8 %. These are open items. Host: every test passes at every start offset. |

## Live row costs

From the `sampling` LOG of the tenth parts (LIVE):

| Figure | Enum-rank and maint-fix runs | This run |
|---|---|---|
| A subscribed row with attitude (`cost_subscribed`) | 28-35 µs | 25-45 µs |
| The call (`xcall_us`, `xcall_seen_us`) | 34-42 µs | 37-57 µs |
| A vitals row (`cost_vitals`) | 1.3 µs | 1.3-1.5 µs |
| A position row in the sweep (`cost_positions`) | 1.0-1.1 µs | 1.0-1.5 µs |

A subscribed row with attitude reads `getPoint` and `getPosition` in the mission state
through the shim, about as dear as the call itself. The host tests model these as a
32 µs row and a 35 µs call.

## Host results

The live-cost repro, `with_live_attitude_costs_every_tier_holds_and_the_pump_bound_holds`:
20 rows subscribed with attitude, 2,000 rows, 100 frames a second, a 687 µs step in
every 100 enumeration items, two minutes after a minute's start.

| Start offset | Pumps a second | Vitals as a rate | Subscribed | Positions | Share |
|---|---|---|---|---|---|
| 0 s | 7.77 | 6.07 s | 1.192 s | 1.02 s | 0.268 % |
| 0.37 s | 7.96 | 5.53 s | 1.201 s | 1.00 s | 0.268 % |
| 1.91 s | 7.94 | 5.56 s | 1.199 s | 1.04 s | 0.269 % |
| 3.53 s | 7.93 | 6.46 s | 1.195 s | 0.99 s | 0.269 % |

No pump planned no row, every 10 s window is within the pump bound, and every frame over
250 µs holds a dear enumeration step. On main the same model reads no vitals row in two
minutes at 6.9 pumps a second.

`at_live_costs_the_fill_the_enumeration_and_no_row_owed_keep_the_pump_bound` (35 µs call):
the 2,000-object enumeration's window nearest the bound is 13.60 pumps a second against
16.28; with no row owed, none comes near. At a tenth,
`at_a_tenth_with_live_attitude_costs_subscribed_rows_keep_the_rank`: subscribed keep
0.52, positions 0.09, 10.45 pumps a second, none empty.

## Live run

`cargo xtask sampling gameserver`, both parts, bundle `sampling-gameserver-20261115T214608Z`.

| Check | Mirror part (2,000) | Tenth part |
|---|---|---|
| Mean share | 0.222 % from the 800 spawn on, 0.210 % settled | 0.290 % at k = 1, spawn included; 0.137 % at k = 0.1 |
| Frames over 250 µs | 99 of 41,886 (0.24 %) | 37 of 18,934 (0.20 %) |
| Positions over stall-free windows | median 1.00 s; no window over 1.25 s | median 1.00 s |
| Vitals as a rate at k = 1 | 4.20 s (3,931 rows of 3,305 due) | no row of 3,702 due |
| Pumps a second at k = 1 | 6.44; 4 windows past the bound (11.19 against 10.73, 11.60 against 10.02, 10.99 against 9.39, 7.69 against 7.30) | 9.20; 1 window past it (10.10 against 9.82) |
| Call share | 0.35 | 0.32 |
| Empty pumps | 1 of 652 | 0 of 921 |
| k = 0.1 rank | | subscribed 0.61 against positions 0.08 |

Against the maint-fix run on main: the sweep now stays within its budget at k = 0.1, and
an enumeration completes at k = 0.1.

The mirror's "position period ≤ 3 s while the table fills" reads 3.01 s in one window. That
window holds a DCS stall at the 800-object spawn: a 1.54 s gap between frames, a
4,470 µs frame, and 781 frames where the others hold about 1,120.

## Open items

- **Tenth part, k = 1: vitals read no row.** The fill of 2,000 births takes the first
  90 s, then one enumeration runs past its 60 s period (`enumeration_s` reaches 134 s),
  so births and the enumeration take most pumps at `m` = 0.75. The share sits at 0.297 %
  and subscribed rows keep about half their rate. The rank is strict, so vitals below
  them gets nothing. The host model does not show this: its enumeration items cost 1 µs
  against 7-17 µs live (`us_enumeration` over `rows_enumeration`), and its enumeration
  ends within its period.
- **Mirror, vitals as a rate 4.20 s**, below the band: vitals repays the rows it owed
  while it waited for an enumeration (readings up to 39.6 s), so the two-minute rate
  reads fast.
- **The pump bound** is passed by 3 to 8 % in five windows, all during fills or
  enumerations, where a maintenance batch may be planned for less than three calls'
  worth when few rows are owed. The host fill at 110 frames a second passes it by under
  0.5 %.
- **The subscribed hold at an enumeration's end** (host): the sweep waits about 0.7 s for
  the subscribed pump to repay the rows that fell behind during the enumeration, so
  moving rows show one gap of 1.6-1.7 s.
