# Bounding the sidecar's growth over a long campaign

| | |
|---|---|
| Step | Bound the sidecar's memory, rows and step cost over a long campaign (the purge, the expiry, the retentions, the work set, the store's size in the status) |
| Date | 2026-11-18 |
| Scripts | `crates/sidecar/tests/growth_measure.rs` (sizes and step costs, ignored, run by hand in release), `crates/sim-fake/tests/churn.rs`, `crates/sim-fake/tests/purge.rs`, `crates/fake-sidecar/tests/bound.rs` |
| Host | macOS on Apple silicon, release build for the measures, debug for the tests |
| Live | The S21 soak's records (before); the live run below (after) |

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
(7.2 KB) explains about a third of the memory growth; the live run below shows the rest.
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

The live part's tree: the full `mise run check` passed on the committed host part
(`20a1999`) before its changes. With them, rustfmt, the workspace's clippy with
warnings denied, the proto check and the tests of the sidecar, the API, the fake sim
(`no_temp.rs`, `churn.rs`), the fake sidecar and xtask passed. `cargo xtask soak
rehearse --minutes 6` against the fake sidecar with its keep and backup flags passed:
119 groups purged at the churn's rate, scheduled backups taken and none failed,
`end.json` written, the sidecar started again over the campaign and attached in 1.1 s.
The fake reads no DCS groups and emits no vitals, so the DCS comparison and the life
counts are shown live only; the comparison has a unit test.

## Live run

### Method and limits

Command: `cargo xtask soak campaign <target> --minutes 120 --store-toml 'removed_keep_h = 0'
--store-toml 'dead_keep_h = 1' --store-toml 'backup_interval_h = 1' --leave-dead
--reattach --detach`, then `cargo xtask soak fit <folder>` (`fit.json`) and `soak judge
<folder>`. The churn is S21's ("Churn and why" in `docs/results/2026-11-17-s21-soak.md`)
with one difference: a killed group is left dead for its expiry and a new group is put
at its place (`--leave-dead`), where S21 removed it and created it again. Both put one
group per killed group. The leave-dead churn's 30 killed groups an hour leave the store
by the expiry, which is a removal, and then the purge, so every removed group is still
purged: 120 removed by the churn and 30 expired, 150 an hour. The per-hour creations,
handovers and deaths are S21's, so the S21 soak's 3.1 MB an hour of sidecar memory is
the figure to compare with.

Settings: `removed_keep_h = 0` purges a removed group as soon as DCS holds nothing of
it; `dead_keep_h = 1` is the shortest expiry (0 turns it off); `backup_interval_h = 1`
takes scheduled backups at 60 and 120 minutes of the sidecar's uptime, inside the run.
The retentions stay at their defaults: a 2-hour run reaches neither the 30 days nor the
1,024 MB, so the journal and the event history rise by design and are reported per hour;
their bounds are shown on the host.

Judged by `soak fit`, beside `soak judge`: the judge's rules want a segment of 8 hours
and a control, so it says "extended" for a 2-hour run, and its allowance of 16 MiB an
hour of sidecar memory does not answer whether the figures level off. `soak fit` fits
each series by least squares over the medians of 5-minute blocks from minute 75 (the
dead keep of an hour, an enumeration and a margin) to the end, from minute 0 for a
whole-run rate; the noise is twice the slope's standard error. The limits are absolute
where the series' mean is near 0 or large against the rate a stopped bound would add:

| Series | Rule | Reason |
|---|---|---|
| Sidecar resident memory (`sidecar.rss_kb`) | Slope plus noise at most 1 MiB an hour | A third of the S21 soak's 3.1 MB an hour: the growth is gone by a margin over what it was |
| Sidecar CPU (`sidecar.cpu_percent`) | At most 1 point of a core an hour | The S21 soak climbed about 1 point an hour (22.2 to 25.8 % in 3.5 hours) |
| Entities held (`store_size.entities_held`) | At most 10 an hour | A stopped expiry adds 30 groups an hour, a stopped purge 150 |
| Units (`store_size.units`) | At most 40 an hour | A stopped expiry adds 120 units an hour |
| Work set (`store_size.work_set`) | At most 5 % of its mean an hour | It follows what the plan has to do, not a removal |
| Dead groups (`store_size.groups_dead`) | At most 34 at every record | 30 an hour kept for an hour, one step's kill, and a death or an expiry that lands a step late |
| Awaiting purge (`store_size.awaiting_purge`) | At most 10 at every record | One removal a step, purged after an enumeration |
| Purged, expired (`store_size`) | 150 and 30 an hour, within 20 % | The bound at work |
| Kills (`churn.kills`), groups left dead (`churn.dead_left`) | 30 an hour each, within 20 %, over the whole run | The death path carries the full churn; the S21 soak's kills stopped after 3.7 hours from a tool defect, fixed before this run |
| Deaths on the event stream (`streams.deaths`) | Reported; from the stored history, at least 4 deaths of the campaign's units a kill and no unit dead twice | Set at minute 37 of the run, from the stored history read then: a kill's blasts also destroy scenery (17 of 105 DEAD events) and units of neighboring groups, so the stream's count runs at about 175 an hour, not 120; the 8-hour soak had the same ratio |
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

Two hours show a plateau that forms inside them and the death path at its full rate
for two hours. They do not show what only a longer run reaches: the journal's and the
event history's limits, the idempotency index past its 24 hours, a slope under the
noise of two hours, and DCS's own growth past the S21 control's window.

## Open items

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
