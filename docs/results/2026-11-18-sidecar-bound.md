# Bounding the sidecar's growth over a long campaign

| | |
|---|---|
| Step | Bound the sidecar's memory, rows and step cost over a long campaign (the purge, the expiry, the retentions, the work set, the store's size in the status) |
| Date | 2026-11-18 |
| Scripts | `crates/sidecar/tests/growth_measure.rs` (sizes and step costs, ignored, run by hand in release), `crates/sim-fake/tests/churn.rs`, `crates/sim-fake/tests/purge.rs`, `crates/fake-sidecar/tests/bound.rs`; live: `cargo xtask soak campaign`, `soak fit`, `soak judge` (`crates/xtask/src/persistence/campaign_soak.rs`), `tools/soak-analysis.py`, `tools/soak-deaths.py` |
| Host | macOS on Apple silicon, release build for the measures, debug for the tests |
| Live | The S21 soak's records (before); two live runs below, 2 and 4 hours (after) |

## What grew, before

Host measures (`growth_measure.rs` run on main `acf35be`, the tree before this change,
with this repository's pinned toolchain; release build):

| Item | Memory | Disk |
|---|---|---|
| Group of 4 units, removed | 7.2 KB heap: the store's maps and the disk layer's shadow of each row | 1.65 KB at creation, +75 B at removal |
| Journal row (running reconciler) | drained at each commit for a store opened from its file; kept for a campaign created through the API (below) | 104 B body on average (command 231, transition 93, outcome 82) |
| Event row | 10,000 newest held | body + 44 to 74 B |
| Idempotency row | one index entry, 24 h | 266 B |

Per-step cost with 250 live groups of 4 units and R removed or dead groups (medians
of 50 steps):

"After" is the working tree of this change on `acf35be`, the same host and build.

| R | Plan, before | Checkpoint, before | Plan, after | Checkpoint, after |
|---|---|---|---|---|
| 2,000 | 0.10 ms | 1.0 ms | 0.054 ms | 0.031 ms |
| 10,000 | 0.44 ms | 5.5 ms | 0.051 ms | 0.045 ms |
| 50,000 | 2.8 ms | 38 ms | 0.058 ms | 0.032 ms |

The driver steps every 2 ms, so before the change each 1,000 removed groups cost about
2.8 % of a core in the plan alone. After it the plan walks the work set (spec/persistence.md,
"Size", `work_set`; `crates/sidecar/src/reconciler/bound.rs`) and the checkpoint compares only
the groups marked since the last one (spec/persistence.md, "Writes"); neither depends on
R. The purge and expiry pass runs once a second and looks at every removed and dead
group: with the step clock moving 2 ms a step, the median step stays 0.049 to 0.056 ms
from R = 0 to 50,000, and the step that runs the pass takes at most 0.17, 0.19, 0.55 and
3.7 ms at R = 0, 2,000, 10,000 and 50,000 (removed; dead groups alike). The keeps bound
R, so the pass stays small. Opening a store still reads every row it holds (90 ms and
68 MB at 10,000 removed groups).

The S21 soak on `<target>` (LIVE, in progress when read; DCS 2.9.31.29434 under Wine
10.0, started 2026-11-18 by `cargo xtask soak campaign <target>`; its record file
`records.jsonl` in the soak's bundle, read on 2026-11-18 at about 13:32 EST up to t = 18,840 s (minutes 0 to 314); the
slopes below are fitted from t = 5,400 s to 18,000 s; churn of about 140 removals an
hour): sidecar resident memory 25.5 to 58.7 MB (+3.1 MB
an hour after the first 90 minutes), CPU 22.2 % to 25.8 % of a core at a fixed live
load, entity rows +680 an hour (5 per `PutGroup`, none deleted), journal +8,700 rows an
hour, store file +2.75 MB an hour, events +7,800 an hour. The host figure per group
(7.2 KB) explains about a third of the memory growth; the live runs below show the rest.
The review found one more cause on the host: a campaign created through the API kept
every journal entry it wrote in memory until the sidecar restarted
(`Store::create_on_disk` left the in-memory store's `keep_journal` set), so the soak's 8,700 entries an hour
stayed in memory (UNVERIFIED as the size of the rest). After this change a store linked
to a file holds only what it has not written.

## What bounds it, after

| Kept | Bound | Default |
|---|---|---|
| A removed entity's rows | `removed_keep_h`, once DCS holds nothing of it | 1 h |
| A dead group | `dead_keep_h`, then removed and purged | 24 h |
| The journal | `journal_retention_days` and `journal_max_mb` | 30 days, 1024 MB |
| The event history | `event_retention_days` and `events_max_mb` | 90 days, 1024 MB |
| Ruins | kept until a `RemoveEntity`, by design | |

## Host tests

- `churn.rs`, `a_long_churn_levels_off_on_disk_and_in_memory`: 30 groups and 4 statics
  live, a remove and a put every 2 s, a kill and a replacement every 4 s, an authority
  move every 6 s, 18 simulated minutes (keeps of 1 and 2 minutes, journal 3 minutes), a
  restart in the middle, persistence on and off. Groups held level at 110 and units at
  322 from the fourth minute; the work set at 21 to 31; the groups marked for a checkpoint at
  14 to 18; the journal at about 2,000 rows; the store file at 4.79 MB; the store's own
  heap within 15 % between the middle and last thirds; the reconciler holds 15.8 KB
  beyond a fresh one. A 60-minute run by hand (`NANOPROBE_CHURN_MIN=60`) shows the same
  plateaus; the process heap creeps about 9 KB a simulated minute, all of it in the fake
  sim's own per-generation state (the store and the reconciler measured alone are flat
  between a 20- and a 60-minute run, `NANOPROBE_CHURN_MIN=20` and `=60`). The real agent's per-generation sets are an open
  item.
- `purge.rs`: the purge after the keep and an enumeration; each guard (a refused
  destroy, an unanswered command, no enumeration, the keep) holds it until it clears; a
  late death after the purge is dropped and counted; ruins neither expire nor are purged
  until removed; a dead group expires with no command to DCS and a held group does not;
  an object of a purged name, after a restore of an older backup, is destroyed and no ID
  is given twice; the export and a restore after a purge; three seeded fault schedules
  with removals, puts, the purge and the expiry, under every harness check.
- `bound.rs` (through the gRPC API with the fake agent): a watch gets the last state
  and a `Removal` (`CONTROLLER`, then `EXPIRED` for an expired group); `RemoveEntity`
  of a purged ID answers `NOT_FOUND` and a keyed repeat its first answer; the status's
  `store_size` follows puts, kills and removals.

## Backups with no temporary folder

Both scheduled backups of the S21 soak failed with "store: disk I/O error" (LOG, the
sidecar's log on `<target>`). SQLite puts a temporary file in the first of
`SQLITE_TMPDIR`, `TMPDIR`, `/var/tmp`, `/usr/tmp`, `/tmp` and `.` that is a writable
folder, and fails with `SQLITE_IOERR_GETTEMPPATH` (extended code 6410) when none is
(bundled SQLite 3.54.1, `unixTempFileDir`). The sidecar's image is built `FROM scratch`
and runs as the DCS user, so it has none, in the soak's container and in `deploy/`'s.

The failing step was the events copy (`EventFile::copy_into`), not `VACUUM INTO`. Its
`INSERT ... SELECT` read its own table in its bound, so SQLite held every selected row
in a temporary table, which goes to a file past its cache; inside the copy's
transaction it also kept a statement journal, which goes to a file past 64 KiB when an
extension touches existing pages of the `source` index. The fixed copy reads its bound
first and appends outside a transaction; the byte count is dropped before the append and
written after the delete, so a copy stopped between them counts its rows at its next
open.

Reproduced in a `FROM scratch` amd64 image run as uid 1000 under a local container
runtime, with the sidecar's own `Campaigns::backup` (a reproduction program, not kept;
`no_temp.rs` below is the kept test of the same paths), 2026-11-18:

| Case | Before | After |
|---|---|---|
| Up to 12,000 events of 110 B | backup ok | backup ok |
| 16,000, 25,000, 200,000 events | "store: disk I/O error" | backup ok |
| 25,000 events with a writable `TMPDIR`, a `/tmp` mount, or as root | backup ok | |
| A 19 MB store with no events (`VACUUM INTO` alone) | backup ok | backup ok |

`temp_store = MEMORY` also passed, and was not taken: the copy's peak memory rose 27 MB
for 200,000 events, so a first copy of a full history (`events_max_mb` 1,024) would
spike by about that much again.

The host test `crates/sim-fake/tests/no_temp.rs` runs the store's file paths under a
wrapper of SQLite's default VFS that refuses every temporary file
(`crates/sim-fake/tests/common/notemp.rs`): commits, a checkpoint, the idempotency
retention, the purge, the event history's and the journal's retention by time and by
size (each must delete rows), two scheduled backups with interleaved sources between
them, after which the copy must hold the history's rows and their byte count, the
export, the read copy under `.read/` and a restore. A positive control first checks that
the wrapper is the VFS in use and refuses a temporary table. Before the fix both backups
were refused a transient database; with the bound read first but the append inside the
transaction, the second was refused a statement journal; with the fix nothing is
refused. `churn.rs` runs under the same VFS, so the reconciler's purge and expiry on
disk need no temporary file either.

A failed scheduled backup names its store and, when SQLite failed, its code in the log
("campaign 1: store: disk I/O error (SQLITE_IOERR_GETTEMPPATH, code 6410)"); one skipped
for low free space is reported the same way. `GetServerStatus` reports `last_backup`,
`last_backup_failure` and `backup_error`, not part of health; the health command prints
the error while the last backup failed. A succeeded one logs its number, the store copy's
bytes and the milliseconds of the whole backup.

## Checks

`mise run check` passed on the reviewed tree, its Lua step on a rerun: the first run
failed once in `lua/tests/spawn_bind.lua` (a spawn whose second row the agent wanted
early), a test this change does not touch, with a known failure under load; it passed
alone, and the steps after it passed one by one. One change followed, from the judge:
the removal and death times take the store's clock fallback, as the journal's do. The
sidecar's, the fake sim's, the API's and the fake sidecar's tests, clippy and rustfmt
passed after it; the full check did not run again.

The live part's tree: the full `mise run check` and `mise run dll` passed on `5247061`
with this document and the two `tools/` scripts; the review's later edits touched only
the document and those scripts, which the check does not build or run. `cargo xtask soak
rehearse --minutes 6` against the fake sidecar with its keep and backup flags passed:
119 groups purged at the churn's rate, scheduled backups taken and none failed,
`end.json` written, the sidecar started again over the campaign and attached in 1.1 s.
The fake reads no DCS groups and emits no vitals, so the DCS comparison and the life
counts are shown live only; the comparison has a unit test.

## Live runs

### Method and limits

Two runs on `<target>`, with the same churn and the same command except the length and
one store setting:

| Run | Command | Length |
|---|---|---|
| 1 | `cargo xtask soak campaign <target> --minutes 120 --store-toml 'removed_keep_h = 0' --store-toml 'dead_keep_h = 1' --store-toml 'backup_interval_h = 1' --leave-dead --reattach --detach` | 2 hours |
| 2 | The same with `--minutes 240` and `--store-toml 'idempotency_keep_h = 1'` | 4 hours |

After each: `cargo xtask soak fit <folder>` (`fit.json`; run 2 also with
`--from-minute 100`, `fit-from-100.json`), `soak judge <folder>`, and
`python3 tools/soak-analysis.py <folder>` (`analysis.txt`: memory by half hour and by
window, the knee, CPU by window and against DCS's, deaths by hour, commit times). The
churn is S21's ("Churn and why" in `docs/results/2026-11-17-s21-soak.md`)
with one difference: a killed group is left dead for its expiry and a new group is put
at its place (`--leave-dead`), where S21 removed it and created it again. Both put one
group per killed group. The leave-dead churn's 30 killed groups an hour leave the store
by the expiry, which is a removal, and then the purge, so every removed group is still
purged: 120 removed by the churn and 30 expired, 150 an hour. The per-hour creations,
handovers and kills are S21's, so the S21 soak's sidecar memory growth, about 3 MiB an
hour over its minutes 90 to 300 (above), is the figure to compare with.

Settings: `removed_keep_h = 0` purges a removed group as soon as DCS holds nothing of
it; `dead_keep_h = 1` is the shortest expiry (0 turns it off); `backup_interval_h = 1`
takes a scheduled backup each hour of the sidecar's uptime, inside the run. Run 2's
`idempotency_keep_h = 1` makes the idempotency index reach its steady size after an
hour, where the default of 24 hours keeps it growing through any run shorter than a
day. The journal's and the event history's retentions stay at their defaults (30 and
90 days, 1,024 MB each): neither run reaches them, so both rise by design and are
reported per hour; their bounds are shown on the host.

Judged by `soak fit`, beside `soak judge`: the judge's rules want a segment of 8 hours
and a control, so it says "extended" or fails on projections for a run this short, and
its allowance of 16 MiB an hour of sidecar memory does not answer whether the figures
level off. `soak fit` fits each series by least squares over the medians of 5-minute
blocks from minute 75 (the dead keep of an hour, an enumeration and a margin) to the
end, from minute 0 for a whole-run rate; the noise is twice the slope's standard error.
The limits are absolute where the series' mean is near 0 or large against the rate a
stopped bound would add:

| Series | Rule | Reason |
|---|---|---|
| Sidecar resident memory (`sidecar.rss_kb`) | Slope plus noise at most 1 MiB an hour | A third of the S21 soak's growth of about 3 MiB an hour: the growth is gone by a margin over what it was |
| Sidecar CPU (`sidecar.cpu_percent`) | Slope plus noise at most 1 point of a core an hour | The S21 soak climbed about 1 point an hour (22.2 to 25.8 % in 3.5 hours) |
| Entities held (`store_size.entities_held`) | Slope plus noise at most 10 an hour | A stopped expiry adds 30 groups an hour, a stopped purge 150 |
| Units (`store_size.units`) | Slope plus noise at most 40 an hour | A stopped expiry adds 120 units an hour |
| Work set (`store_size.work_set`) | At most 5 % of its mean an hour | It follows what the plan has to do, not a removal |
| Dead groups (`store_size.groups_dead`) | At most 34 at every record | 30 an hour kept for an hour, one step's kill, and a death or an expiry that lands a step late |
| Awaiting purge (`store_size.awaiting_purge`) | At most 10 at every record | One removal a step, purged after an enumeration |
| Purged, expired (`store_size`) | 150 and 30 an hour, within 20 % | The bound at work |
| Kills (`churn.kills`), groups left dead (`churn.dead_left`) | 30 an hour each, within 20 %, over the whole run | The death path carries the full churn; the S21 soak's kills stopped after 3.7 hours from a tool defect, fixed before these runs |
| Deaths on the event stream (`streams.deaths`) | Reported | Set at minute 37 of run 1, from the stored history read at minute 36: a kill's blasts also destroy scenery (17 of 105 DEAD events in the stored history) and, UNVERIFIED, units of neighboring groups, so the stream counts more than the 4 units of each killed group |
| The agent's pending sets (`pending_destroy`, `pending_deaths`, `loadouts_waiting`, `births`) | At most 1 entry an hour | S21's limit: each drains in steady state |
| Journal and event rows and bytes, the store's files | Reported per hour | They rise by design until their limits |

`soak fit` also requires at least two scheduled backups and no failed or skipped one
(the status's `last_backup` and `last_backup_failure` over the records; the sidecar's
log line gives each one's store copy bytes and the milliseconds of the whole backup),
and the owner's targets over the run: mean share at most 0.3 %, at most 1 % of frames
over 250 µs. `end.json` must show, before and after the sidecar starts again over the
campaign, no object of the campaign in DCS without a row, no live group missing from
DCS and no member of a group left dead in DCS again; and no live unit without a life
value 10 s and 60 s after the restart (the recovery path, after the records, so its
memory starting again does not touch the series).

The CPU of both runs is also judged against DCS's: the sidecar's slope from minute 90
is fitted with DCS's CPU as a second regressor, because on the host the sidecar's work
per commit follows the units that move, which also drive DCS's CPU. A figure after ±
from `soak fit` or `soak judge` is its noise, twice the standard error; every other ± is
one standard error, from `soak-analysis.py`, widened for the residuals' lag-1
autocorrelation.
Memory is in MiB, as the records give KiB.

Four hours show the knee, the slope after it with the idempotency index at its
steady size, and the hourly backup and retention passes four times. They do not show
what only a longer run reaches: the journal's and the event history's limits, the
attribution memory's cap (about hour 8 at this churn), SQLite's page caches full (about
hour 11), the idempotency index at its 24-hour default, and DCS's own growth past the
S21 control's window.

### Run 1: 2 hours

LIVE, `<target>`, 2026-11-18, 18:05 to 20:10 EST (`soak.json`): DCS 2.9.31.29434
(`dcs_server.release`) on Linux under Wine `wine-10.0 (Debian 10.0~repack-6)`, both read
by the soak during the run (`soak.json` provenance); the agent build `62549c694074f77e`
(`--dev`, with the VITALS-after-OBJECT fix). The tool that ran was built from `441c0ed`
before the `soak fit` deaths rule changed; `soak fit` and `soak judge` ran afterwards on
a copy of the run's records with the tool of `5247061`. Bundle
`bundles/soak-<target>-20261118T230523Z` (not committed). 120 records, the run
completed, no segment end, no error, no player.

The bounds hold. From minute 70 the store holds 1,030 groups and 4,120 units, 29 or 30
of the groups dead, 0 or 1 awaiting purge, a work set of 250; each is flat (slope 0 over
minutes 75 to 120; awaiting purge +0.23 ± 0.89 an hour, under its limit of 10 at every
record). The purge took 149.7 groups an hour and the expiry 29.8 an hour,
the churn's 150 and 30. Kills and groups left dead ran at 30.0 an hour each, and the
churn guard found no kind short of its plan. The agent's pending sets stayed at 0.

The sidecar's resident memory and CPU fail their slope limits over minutes 75 to 120
(`fit.json`):

| Series, minutes 75 to 120 | Slope an hour | Limit | This run |
|---|---|---|---|
| Resident memory | +3.01 ± 1.13 MiB | 1 MiB | 41.6 to 44.1 MiB |
| CPU | +0.94 ± 0.34 points of a core | 1 point | 17.4 to 18.7 % |

The memory window straddles a knee. Resident memory rises 6.8 MiB an hour over minutes
60 to 90 and 0.95 ± 0.03 MiB an hour over minutes 92 to 120. The knee is at minute 91,
where the event sequence passes 16,384 (16,179 to 16,870 in one record) and where the
two-segment fit breaks; "What the growth is" explains it. That record also holds the
run's largest deaths jump (+247) and memory steps 0.43 MiB in it, so the knee's minute
is set by that burst of events. Each deaths jump before the ring fills carries a memory
step of 0.3 to 0.5 MiB in one record (run 1, minutes 65 and 85: +0.40 and +0.47).

The sidecar's mean CPU was 17.3 % of a core over minutes 5 to 45, 17.9 % over 60 to 90
and 18.0 % over 92 to 120. From minute 90 it rises +0.56 ± 0.42 points an hour alone and
+0.58 ± 0.45 with DCS's CPU as a second regressor, whose coefficient is 0.03 ± 0.06: DCS's
load does not explain the late slope, and 30 minutes do not settle it. Run 2, 2.5 hours
longer from minute 90, shows the sidecar's CPU flat there (+0.03 ± 0.04 points an hour).
The S21 soak used 22.2 to 25.8 % of a core over its minutes 90 to 300.

Backups: scheduled backups at 60 and 120 minutes of the sidecar's uptime succeeded and
none failed or was skipped, in the sidecar's `FROM scratch` image run as the DCS user,
which failed both backups of the S21 soak.

End checks (`end.json`): no object of the campaign in DCS without a row, no live group
missing from DCS, no member of a group left dead in DCS again, before the sidecar
started again and after. It attached 4.7 s after the restart; 0 of the 970 live units
lacked a life value 10 s and 60 s after it (`before`: 0 of 970). 970 and not 1,000: the
kills' blasts also destroy objects near the target group (scenery in the stored
history); that the missing 30 units are of neighboring groups is UNVERIFIED.

Deaths on the event stream: 206 `S_EVENT_DEAD` in the first hour and 799 in the second,
1,005 in all, for 59 kills. The second hour holds three jumps within one record each, at
the 32nd, 42nd and 45th kill (+130 at minute 65, +123 at 85, +247 at 91). The
campaign's `events.sqlite`, copied from the target at minute 36 and decoded by
`tools/soak-deaths.py` (its output: `deaths-minute-36.txt` in the bundle), held the first
6,294 events, the stream's count at minute 33, with 105 deaths: 88 of the campaign's
M-113 and 17 scenery (trees, one building). 19 of the 105 carry no initiator ID (the 17
scenery and 2 M-113), and no initiator ID appears twice. What died in the jumps is
UNVERIFIED: no history was read after them.

Owner targets: mean share 0.186 %, 0.09 % of frames over 250 µs (whole run). Met.

`soak judge`: FAIL, for reasons of a 2-hour run: `journal_rows` and `store_entity_rows`
have too few export points after its warm-up (every 30 minutes), and the agent's share,
at a slope of +0.0070 ± 0.0118 % an hour, projects to 0.346 % at 24 hours (0.608 % at
its upper bound), over the 0.3 % cap, although the run's mean is 0.186 %. Two series are
called settled: `sidecar_fds` (+2.71 ± 1.42 an hour, 31.8 at the end; in run 2 the
sidecar holds 25 descriptors at the first record, 28 up to the first backup and 31
after it, one record at 32)
and `world_groups`. The DCS series have no control in this run.

### Run 2: 4 hours

LIVE, `<target>`, 2026-11-18 21:12 to 2026-11-19 01:19 EST (`soak.json`): DCS
2.9.31.29434 and Wine as in run 1, read by the soak during the run; the agent build
`00ab0f05f7eff573` (`--dev`, from `5247061`), the tool of `5247061`. Bundle
`bundles/soak-<target>-20261119T021215Z` (not committed). 240 records, the run
completed, no segment end, no error, no player.

Resident memory levels off at a knee, then rises under the limit:

| Minutes | Mean | End | Event rows at the end |
|---|---|---|---|
| 0 to 30 | 30.7 MiB | 34.4 MiB | 5,801 |
| 30 to 60 | 37.7 MiB | 40.0 MiB | 10,415 |
| 60 to 90 | 41.5 MiB | 43.2 MiB | 15,854 |
| 90 to 120 | 43.6 MiB | 43.9 MiB | 20,816 |
| 120 to 150 | 43.9 MiB | 43.8 MiB | 25,694 |
| 150 to 180 | 43.9 MiB | 44.1 MiB | 30,608 |
| 180 to 210 | 44.2 MiB | 44.4 MiB | 35,418 |
| 210 to 240 | 44.5 MiB | 44.7 MiB | 40,196 |

| Window | Slope, MiB an hour |
|---|---|
| Minutes 60 to 90 | 6.47 ± 0.25 |
| Minutes 92 to 120 | 0.93 ± 0.04 (run 1: 0.95 ± 0.03) |
| Minutes 100 to 240 | 0.44 ± 0.06 (`soak fit --from-minute 100`: 0.45 ± 0.04, passed) |
| Minutes 100 to 170 | 0.28 ± 0.15 |
| Minutes 170 to 240 | 0.60 ± 0.01 |
| `soak fit` from minute 75, its default (`fit.json`; the window holds the knee) | 0.67 ± 0.14, passed |

The event sequence passes 16,384 at minute 94 (16,523). Memory rises about 0.07 MiB a
minute up to minute 94 (43.00 to 43.44 MiB over minutes 88 to 94) and about 0.015 a
minute after it, so the knee is at the ring's fill, as in run 1. The two-segment fit
from minute 30 breaks earlier, at minute 88 (15,547 event rows; 8.2 MiB an hour before,
0.47 after), pulled by a one-record step of 0.53 MiB at minute 85 that comes with the
+194 deaths jump; the jump at minute 65 carries a step of 0.32 MiB.

Over minutes 92 to 120 the two runs match (0.93 and 0.95 MiB an hour). The host's
estimate for run 2's settings, with the idempotency index steady, was about 0.7 MiB an
hour after the knee, about 0.3 below run 1's; run 2 is about 6 standard errors above it
and shows no gap from run 1. The host's split of the slope after the knee among the
idempotency index, the attribution memory and the page caches is therefore not
confirmed live (UNVERIFIED). After minute 100 the slope is not steady: 0.28 over the
next 70 minutes, 0.60 over the last 70. Four hours do not show it stop.

CPU is flat after the first hour:

| Measure | Result | Expected |
|---|---|---|
| Mean, minutes 70 to 120 against 190 to 240 | 17.65 % and 17.67 % of a core: +0.02 ± 0.07 points | Within 0.3 points |
| Slope from minute 90, with DCS's CPU as a second regressor | −0.05 ± 0.04 points an hour; 0.12 ± 0.02 sidecar points per DCS point | Flat |
| Slope from minute 90, alone | +0.03 ± 0.04 points an hour | Flat |
| `soak fit` slope from minute 100; from minute 75 | +0.01 ± 0.10 and +0.04 ± 0.08 points an hour, passed | Flat |

DCS's CPU averaged 28.4 % and 29.9 % over the two windows of the first row. From minute
90 the sidecar's CPU and DCS's correlate at 0.38 per record and 0.18 by 5-minute block
median; over the same minutes run 1 gives 0.10 and −0.26. Over the whole run, where both
rise in the first hour, the figures are 0.63 and 0.68 (run 1: 0.73 and 0.78).

Commit time, from the `commit_timings` lines of the sidecar's log (one every 10 s, about
4,990 commits each):

| Minutes | 0 | 30 | 60 | 90 | 120 | 150 | 180 | 210 |
|---|---|---|---|---|---|---|---|---|
| Median commit, µs (median of the half hour's lines) | 122 | 127 | 133 | 129 | 129 | 129 | 126 | 127 |
| p99, µs | 208 | 212 | 223 | 218 | 220 | 218 | 217 | 218 |

In 5-minute blocks the median is 120 to 124 µs up to minute 40, 126 to 129 µs over
minutes 40 to 60, 131 to 135 µs over minutes 60 to 85, and 124 to 134 µs from minute 85
to the end. Run 2 rises in two stages over minutes 40 to 70; run 1 steps once, from 129
at minute 55 to 140 at minute 60, and holds 131 to 140 after. Neither run shows a rise
larger than the block-to-block spread of about 8 µs at the later hourly backups,
retention passes and expiry hours (run 2, minute 120: 127 then 130; minute 180: 126 then
126). What starts the rise is UNVERIFIED.

The bounds hold as in run 1. From minute 100: purged 150.0 an hour, expired 30.0 (90 in
all, from minute 60), kills and groups left dead 30.0 each; dead groups at most 30
(limit 34), entities, units and the work set flat (1,030, 4,120, 250); the churn guard
found no kind short of its plan. The agent's pending sets stayed at 0.

Backups: four scheduled backups, at 60, 120, 180 and 240 minutes of uptime, succeeded,
none failed or was skipped; each took 132 to 161 ms for the store copy (4.3 to 8.8 MB),
the events copy and the server store's.

End checks (`end.json`): clean before and after the restart, as in run 1. It attached
4.9 s after the restart; 0 of the 976 live units lacked a life value before it, 10 s
and 60 s after it.

Deaths on the event stream: 188, 554, 226 and 210 by hour, 1,178 in all, for 119 kills.
The second hour holds two jumps within one record each, at the 32nd and 42nd kill, as
in run 1 (+105 at minute 65, +194 at 85). Hours without a jump run at 188 to 226, about
7 deaths a kill. No stored history was read in this run.

Owner targets: mean share 0.254 %, 0.106 % of frames over 250 µs (whole run). Met.

`soak judge`: FAIL, not conclusive. Over the judge's window from minute 60 the agent's
share rose from 0.251 % to 0.259 % (+0.0029 ± 0.0009 % an hour), which projects to
0.318 % at 24 hours and 0.337 % at its upper bound, over the 0.3 % cap; over the
window's second half its slope is +0.0010 ± 0.0023 % an hour, flat within noise, as the
S21 soak's was over its second half (+0.0011 ± 0.0015 % an hour). The judge has no
control for the share: its control runs without the agent. DCS's own memory grew 151 MiB
an hour (4.8 GiB at the end), and the DCS series have no control in this run. No sidecar
series failed.

### What the growth is

On the host, the sidecar's memory growth is the fill of structures with a fixed size,
each full at its own time. This was found by counting live heap bytes by allocation
site with the fake sim at 120 times the soak's churn and the keeps shortened by the
same factor. The live knee, at the minute the event ring fills in both runs, matches the
main one; the split of the slope after it is the host's and is not confirmed live:

| Structure | Size | Full at this churn |
|---|---|---|
| The event history's window in memory (`EventLog`, `crates/api/src/events.rs`): the newest 10,000 events in a ring of 16,384 slots of 424 bytes | 6.6 MiB of slots, about 3.3 MiB of bodies | Event 16,384: minute 91 in run 1, 94 in run 2 |
| The store's change ring (`world_changes_keep` 100,000 in 131,072 slots of 16 bytes) | 2.0 MiB | About minute 94 (UNVERIFIED: the soak does not record the store revision) |
| The idempotency index | 122 bytes a key plus its table | `idempotency_keep_h` |
| The attribution memory of objects that left DCS | 172 bytes a row, at most 20,000 | About hour 8 |
| SQLite's page caches, 4 connections of 2,000 KiB | 7.8 MiB | About hour 11 |

The ring's head moves through all 16,384 slots while it holds 10,000 events, so each
event touches 424 bytes of memory not touched before until the ring wraps. Before the
knee that is about 4.4 MiB an hour of the 6.8; after it the idempotency index, the
attribution memory and the page caches add up to about 1 MiB an hour on the host. Live,
both runs measure about 0.95 over minutes 92 to 120, although run 2 holds the
idempotency index steady and the host predicts about 0.3 less for it. On the host, from the equivalent of hour 23 to hour 43, every
sidecar structure was flat. Entity rows, the disk shadow, the work set and the
per-stream state are flat; the journal and the event tables cost memory only through
the capped page caches.

On the host the CPU's level comes from two terms bounded by the live groups. The plan
looks up each live group's order every step, and a fresh campaign's groups gain orders
over the first hour or two at the churn's 3 orders a step; that this is the live ramp is
UNVERIFIED, as the soak does not record how many groups hold an order. And each commit
compares each group marked changed with the disk shadow, about 57,000 instructions a
group, where marked groups follow the units that move. Instruction counts on the host
stay flat while the journal, the event history, the store file and the idempotency
index grow tenfold: nothing in the step walks them. On the host the first backup,
retention pass and expiry add no instructions to a commit; whether the live rise in
commit time is wall time only is UNVERIFIED.

## Open items

- What starts the commit-time rise of 5 to 11 µs over minutes 40 to 70 (run 2) and at
  minute 60 (run 1) is UNVERIFIED. It does not come back larger than the blocks' spread
  at the later hourly backups, retention passes or expiries. The `commit_timings` line
  could carry its phases (time in the shadow compare, in the transaction and in its
  commit, groups marked, journal entries written, whether the `world` row was written,
  and the thread's user and system time on Linux), so that one run names the phase.
- The soak records do not hold the store revision, the number of live groups holding
  an order or the groups marked per commit; the status and each soak record could
  carry them. The change ring's fill time and the CPU terms' live sizes are UNVERIFIED
  without them.
- The sidecar's resident memory after the knee is shown live for 2.5 hours only, and its
  slope rose from 0.28 to 0.60 MiB an hour over that time. Run 2's slope over minutes 92
  to 120 matches run 1's although the host predicts about 0.3 MiB an hour less with the
  idempotency index steady, so what makes up the slope after the knee is UNVERIFIED
  live. That it levels off once the
  attribution memory (about hour 8) and SQLite's page caches (about hour 11) are full,
  and with the idempotency index at its 24-hour default, is shown on the host only; a
  live run of at least 12 hours at the defaults would show it.
- The agent's share rose over run 2's first hours (0.251 % to 0.259 % from minute 60,
  +0.003 % an hour) and was flat within noise over the judge window's second half, as in
  the S21 soak; there is no control to separate DCS's part. The owner's target held. A
  longer run with the same agent build settles whether it levels off.
- Deaths on the event stream jump by 105 to 194 within one record at the 32nd and 42nd
  kill in both runs, and by 247 at the 45th in run 1. What died then, and whether any
  object died twice after minute 33, is UNVERIFIED: no stored history was read after the
  jumps.
- With `[store] enabled = false` the event history in memory has no count limit: only
  its 90 days and 1,024 MB of encoded bytes bound it, and decoded events cost several
  times their encoded size (read in the code, UNVERIFIED by a run). A count limit for
  the in-memory history would bound it.
- The fake sim's own tables (its results, junk list and frame tables) grow without
  bound, about 5.6 MiB in 8 minutes at 4 steps a second, so a rehearsal's resident
  memory verdict reads the fake sim's growth, not the sidecar's.
- The backups' events copy keeps every row while the history is empty (its delete bound
  is the history's oldest row); it loses nothing and follows the history again at the
  next event.
- The backup worker opens the live events file for writing (`EventFile::open` makes it
  if missing), which can meet the hub's writer; a lone "database is locked" in a host
  test may be that (UNVERIFIED). A next one names its extended code.
- The agent's per-generation sets (the sampler's `dead` set, the applied revisions set
  to 0 rather than removed) grow within a mission generation; an agent step bounds them.
- The disk layer keeps a second copy of every row in memory (the shadow); halving it is
  an open item.
- The fake sim's destroy refusal covers `GROUP_DESTROY` and `STATIC_DESTROY`, not
  `GROUP_CAPTURE_DESTROY`, so a removed live group whose capture-destroy is refused is
  not exercised; the harness's seeded operations can pick a removed group.
