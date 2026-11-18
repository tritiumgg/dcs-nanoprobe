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

## Checks

`mise run check` passed on the reviewed tree, its Lua step on a rerun: the first run
failed once in `lua/tests/spawn_bind.lua` (a spawn whose second row the agent wanted
early), a test this change does not touch, with a known failure under load; it passed
alone, and the steps after it passed one by one. One change followed, from the judge:
the removal and death times take the store's clock fallback, as the journal's do. The
sidecar's, the fake sim's, the API's and the fake sidecar's tests, clippy and rustfmt
passed after it; the full check did not run again.

## Live run

To be run after the S21 soak releases `<target>`, on the coordinator's grant.

## Open items

- The agent's per-generation sets (the sampler's `dead` set, the applied revisions set
  to 0 rather than removed) grow within a mission generation; an agent step bounds them.
- The disk layer keeps a second copy of every row in memory (the shadow); halving it is
  an open item.
- The fake sim's destroy refusal covers `GROUP_DESTROY` and `STATIC_DESTROY`, not
  `GROUP_CAPTURE_DESTROY`, so a removed live group whose capture-destroy is refused is
  not exercised; the harness's seeded operations can pick a removed group.
