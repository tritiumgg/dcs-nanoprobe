# S18 host part: persistence and restore on the fake sim

| | |
|---|---|
| Step | S18, host part: per-campaign store, journal, checkpoints, backups, schema version and migration, capture and restore, the paused restore burst, campaign create, suspend and activate |
| Date | 2026-11-15 |
| DCS | None. Every test runs on the fake sim (`crates/sim-fake`); no claim here is about DCS. |
| Spec | `spec/persistence.md`; `spec/world.md` ("Lifecycle", "Respawn gate") |
| Schema | `campaign.proto`: `Campaign.last_checkpoint`, `CheckpointInfo`, `Backup`, `BackupReason`, `BackupCampaign`, `ListCampaignBackups`; `world.proto`: `Unit.left`, `UnitLeft`. `buf lint` and `tools/protobreak.sh` pass against `main`. |
| Code | Branch `claude/s18-persistence-host-45044c`, on `main` with S15 merged, built with Rust 1.99.1 on macOS 26.6.2, arm64 |
| Result | Every host exit test passes. The live tests are left for a later step (below). |

## Exit tests

All in `crates/sim-fake/tests/persistence.rs` unless named otherwise.

| Exit item | Test | Result |
|---|---|---|
| After a reload and after a DCS kill, the world returns, a convoy continues to its destination, no group is duplicated, no order is issued twice | `the_world_returns_after_a_reload_and_after_dcs_dies`, once with the sidecar alive and once with it restarted from its store file | passed |
| A paused restore that destroys and re-adds groups leaves no duplicate after the resume | `a_paused_restore_burst_leaves_no_duplicate_after_the_resume`: all 10 spawns in one frame under `BURST`; during the pause the fake holds the destroyed objects and the re-added group at once; after the resume one group per entity | passed, on the sidecar's side: the fake lets the pending objects go at the resume by construction, so DCS's own behavior is LIVE only from S15's paused replace |
| Suspend A, activate B, then A: each returns as left, nothing crosses | `suspend_a_activate_b_then_a_and_each_returns_as_left`: each suspended file equals the store as it was left, but for its record and checkpoint | passed |
| A restore with a death pending does not re-create the dead member | `a_restore_with_a_death_pending_does_not_recreate_the_dead_member` (a reload, and a DCS and sidecar restart from the file; `MISSING` and `REMOVED`; report and respawn policies); `an_activation_does_not_recreate_a_held_member` | passed |
| A migration from schema version N to N+1 on a stored campaign | `a_stored_campaign_migrates_from_one_schema_version_to_the_next` | passed |

"No order is issued twice" is checked two ways: no `GROUP_ORDER` is answered
`AT_REVISION` (sent to a group that held it), and DCS applied no order revision twice in
one generation of one agent boot. Each test asserts it found `GROUP_ORDER` results to
check.

## Held members

The held-member rule follows the S18 quorum ruling. Its tests, besides the exit row:
`an_all_held_group_stays_out_of_dead_until_the_controller_removes_it` (a group of dead
and held members goes `lost`, never `dead`, under the report and respawn policies, and
`RemoveEntity` releases the marks); the store test
`only_journalled_marks_outlive_a_generation_and_removal_releases_them`;
`a_unit_shows_how_it_left` in `crates/api/src/convert.rs`; and
`kills_lost_with_a_dcs_restart_settle_under_the_inferred_mark_rule` in
`crates/sidecar/tests/reconciler.rs`.

The rule's costs:

- A group a script removed comes back short of the removed members, after a reload, a
  DCS restart or an activation, until the controller removes the group. The sidecar
  cannot tell such a member from one whose death event is late.
- A kill whose `OBJECT_GONE` the agent never delivered (DCS died first) leaves only a
  mark inferred at an enumeration's end, which clears at the new generation: that
  member is spawned again.
  `kills_lost_with_a_dcs_restart_settle_under_the_inferred_mark_rule` asserts this
  for two members.

## Further tests

- `a_store_reads_back_exactly_as_written`: 12 seeded fault schedules with an
  extension, an airbase owner, a warehouse stock, countries and sample rates set;
  every 25 operations the store is written and a copy read back, and every table, the
  record, the last checkpoint, the route events, the samples' model time and the next
  `EntityId` match.
- `seeded_schedules_hold_with_the_store_read_back_from_its_file`: 32 seeds of the
  deterministic simulation's invariants, with every sidecar restart reading its store
  from the file and nothing else, and writes only where the store makes them plus
  every third operation.
- `backups_are_pruned_numbered_up_and_share_the_history`.
- `crates/sidecar/src/campaigns.rs`: a bad folder listed as an error, a failed create
  or backup leaving no folder, a cut backup removed, activation of a missing or
  finished campaign keeping the active one, and revisions not reused after an unclean
  stop.
- `crates/sidecar/src/store/disk.rs`: a damaged store refused by the check it breaks.
- `crates/sidecar/src/history.rs`: order, reads from a sequence number, a torn last
  record, zeros after a cut write, a bad first or middle record, a bad length,
  out-of-sequence appends, the incremental copy, and compaction keeping the newest
  record.

## Seed sweep

`seed_sweep_compares_file_and_memory` (ignored by default) runs 1,000 seeds of the
persistent schedule, each twice: once with restarts from the store file, once with
the store kept in memory.

```
NANOPROBE_SEEDS=1000 cargo test --release -p nanoprobe-sim-fake --test persistence -- --ignored --nocapture seed_sweep
```

No seed fails with the file that passes in memory. Two seeds fail in both, and are
recorded as open items:

| Seed | Failure | On `main` |
|---|---|---|
| 358 (schedule seed `358 ^ 0x5eed`, late deaths) | `group 13 respawned with member 13, which left DCS unexplained in this generation`, at a `RemoveSilently` after a static was killed and revived | fails the same way |
| 989 (same) | `5 holds order 0 in DCS, store has 8` at settling: an order sent after members were removed and one revived is not held by DCS | passes; the schedule reaches this state only because a held member is no longer re-created at a DCS restart |

The "On `main`" column comes from the in-memory half of the schedule
(`World::new_late(3, 1, policy, seed)`, `Schedule::new(seed ^ 0x5eed)`, 300 `op_late`
operations, then `settle`), built in a scratch worktree of `main`, which has no
persistence test to run it.

The sweep found two durability faults, both fixed: a loaded mission written alone
before the transitions it causes, and a missing mark that was not written at once.

## Cost of a write

`the_cost_of_a_write_with_two_thousand_units`, release build, 2,001 units, median of 10:

```
cargo test --release -p nanoprobe-sim-fake --test persistence -- --nocapture the_cost_of_a_write
```

| Case | Time |
|---|---|
| Every unit moved since the last write | 7.5 ms |
| Nothing changed | 0.22 ms |

The no-change cost is the comparison of every row with the copy of what the file holds.
It is paid on each write before a command, on the sidecar's thread, not DCS's.

## Design points decided here

- A suspend writes a final checkpoint, closes the store cleanly, and takes no backup;
  backups are scheduled, manual, or before a migration (ruling Q2). The capture of
  live groups and the agent's `CAMPAIGN` 0 at a suspend are the driver's, built with
  the hub wiring. The suspend test sets sample rates on each campaign just before it
  is suspended, a change no other write catches, and finds them after the switch.
- An open after an unclean stop moves the store revision forward by 2^32, so a
  revision a watcher saw but the file never got is not handed out again.
- Rows are written by comparing the store with a copy of what the file holds, so no
  mutator of the store marks what it changed. The codec destructures every struct
  without `..`: a new store field does not compile until it is persisted.
- A store writes itself when a command, a result, a fact or a command epoch is
  journalled, and when a missing mark is set or cleared. A loaded mission is written
  with the transitions it causes, never alone.
- Writes run on the sidecar's driver thread, not a writer thread of their own: they
  must finish before the command they protect is sent.
- The fake sim models pause, the pending-destroy set and `BURST`; the burst lifts the
  per-frame limit from the command after it.

## Live tests left for a later step

Run on a DCS dedicated server (Linux under Wine first), with the sidecar's
persistence on:

1. Reload the mission and kill DCS with a convoy under way and an aircraft airborne:
   the world returns, the convoy arrives, the aircraft continues, no group is
   duplicated, no order is issued twice.
2. A paused restore through the sidecar that destroys and re-adds groups under the
   same names: exactly one group per entity after the resume. S15's paused replace
   showed DCS doing this for one command (LIVE); this checks the sidecar's restore
   path over it.
3. Suspend A, activate B, then A on the server.
4. A reload with a death pending (a unit killed whose death event is late): the member
   is not re-created.
5. The live write cost with the server's unit count.
6. Capture and restore every table the restore path carries (ammunition, pylons,
   options, extra keys) and confirm that no string key looks like an integer, the
   UNVERIFIED claim spec/world.md ("Lua tables in the API") gives to S18. Tag the
   result LIVE with the build and platform.

## Open items

- The API hub (`crates/api/src/hub.rs`) still keeps stores in memory. Wiring it to
  `Campaigns` (create, activate, suspend with its capture and `CAMPAIGN` 0,
  `BackupCampaign`, `ListCampaignBackups`, `last_checkpoint`), the scheduled backup,
  the checkpoint timer, a write before each changing call answers, and the Events
  service over `history.log` (appended and flushed before each event is published),
  and compacting the history at open (`event_retention_days`). `Campaigns::create`
  takes the campaign ID as given; the hub applies the naming ruling's ID rule.
- Seed 989 goes with the per-member resolve call (S19).
- A call that resolves one held member as dead or restores it (S19 exit item).
- The optional `SampleRates achieved` on `CaptureResponse` (commands.proto), left out.
- A read-only open for reading finished and suspended campaigns through the World and
  Events services.
- Seeds 358 and 989 above.
