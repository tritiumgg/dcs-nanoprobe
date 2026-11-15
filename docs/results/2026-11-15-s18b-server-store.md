# S18b: the server store, group commit and the hub's stores, on the fake sim

Host only, 2026-11-15, macOS. No DCS run: every result here is from the fake sim and
host tests. The persistence review's ruling changed schema 1 in place; no store had
shipped.

## What the sidecar does

- **The API hub runs on the campaign stores** (`crates/api/src/hub.rs`). A campaign's
  store opens when a call first names it and closes at suspend and finish; listing reads
  records and checkpoints without opening. Suspend, finish and a switch capture every
  bound group first and wait up to `result_wait_s`. Timers take a checkpoint every
  `checkpoint_s` (skipped when the revision did not move) and a backup every
  `backup_interval_h`. `FinishCampaign`, `BackupCampaign` and `ListCampaignBackups` are
  built. A clean stop closes every store; the active campaign resumes at the next start.
- **Group commit.** One store transaction per driver step: commands are held in the
  adapter (`SimAdapter::flush`) until the step's events and store commit, and the
  agent's records are acknowledged only then. A changing API call commits before it
  answers. Every tool that drives an adapter directly flushes after its sends
  (`command-script`, `command-probe`, `conformance`, `world_exit`, `players_exit`). The
  journal is write-only (`at_ms`, `kind`, `entity` columns); an `inflight` table feeds a
  restarted reconciler. Sampled state is written only by a checkpoint, one row replaced
  in place.
- **`events.sqlite`** holds a campaign's events and the server log: rows by sequence
  number with a unique `source` key (an agent record's first event, its further events
  numbered, or a route event's key), retention by DELETE from the newest event's time
  (skipped when the clock is behind it), run at open and hourly, and an incremental
  backup copy.
- **The server store** (`crates/sidecar/src/server.rs`, `<store dir>/server/`, mode
  0700): the command epoch, the player secret and key-to-ucid pairs, the idempotency
  index and keys of calls on no campaign, server-scoped extension data, the server event
  log, backups on the campaign schedule, and migration with a pre-migration backup. An
  exclusive OS lock on `<store dir>/lock` refuses a second sidecar.
- **Player keys.** S16's `Keyring` is in memory and read from the server store at start.
  A `players.json` from before the server store is read in once and renamed
  `players.json.migrated` (mode 0600). A key learned in a step is written by the end of
  the step; a failed write is tried again at the next.
- **Idempotency keys** are written in the acted-on campaign's store in the change's
  transaction, then indexed in the server store; a campaign store's recent keys are
  copied into the index when it opens. Every changing method's answer reads back from a
  store, checked by a test that reads the method names from the services.
- **Backups** copy through their own connection on a worker thread. `backup.txt` records
  the revision. The newest pre-migration backup of each schema version is kept outside
  `backup_keep`; a pre-migration backup is skipped when the newest backup has the same
  schema and revision.
- **Checked encoder.** The store refuses an `lv` value past the codec's depth or pair
  limits (`INVALID_FIELD` through the API); row and event-body bytes are pinned in
  `spec/persistence.golden`.
- **Crash between the commits.** Events commit before the store, and commands are
  released after both. Event rows carry their source; the store keeps the last record
  folded in and the stamp it was read under. A record the agent delivers again is folded
  in once and its events logged once.
- **Builds without SQLite.** The server store and the events file are behind the
  sidecar's `disk` feature, so `link-probe` and `nanoprobe` build for
  `x86_64-unknown-linux-musl` with no C toolchain for the target.

## Exit tests (host, fake sim)

| Exit item | Test |
|---|---|
| The epoch only grows across a campaign switch and a sidecar restart | `crates/fake-sidecar/tests/persistence.rs` `the_epoch_only_grows_across_a_switch_and_a_restart` |
| A player's key is the same in two campaigns | `crates/fake-sidecar/tests/adoption.rs` `keys_are_stable_across_a_restart_with_persistence` (through `ListPlayers` in campaigns a and b and after a restart); `crates/fake-sidecar/tests/persistence.rs` `player_keys_are_the_servers_across_campaigns_and_restarts`, `a_players_json_is_migrated_at_start_and_its_keys_kept` |
| A repeated call is recognized after the active campaign changes | `a_repeat_is_recognized_after_a_switch_and_a_restart`, `a_repeat_is_recognized_from_the_campaign_store_when_the_server_row_is_gone`; `crates/api/src/idem.rs` `every_service_method_round_trips_through_a_store` |
| The server event log survives a restart | `the_server_event_log_survives_a_restart` |
| A backup and a migration of the store pass | `crates/sidecar/src/server.rs` `a_migration_backs_up_the_server_store_first`; `the_timers_take_checkpoints_and_backups_and_keep_backup_keep`; `crates/sim-fake/tests/persistence.rs` `pre_migration_backups_are_kept_apart_and_not_repeated` |
| Part 1: suspend and capture, timers, Events over the history, compaction | `suspend_captures_the_live_groups_then_closes_the_store`, `the_timers_take_checkpoints_and_backups_and_keep_backup_keep`, `a_sidecar_restart_resumes_the_active_campaign_its_world_and_events`, `compaction_at_open_keeps_the_newest_event_and_ids_skip_unreadable_folders` |
| The crash point between the two commits | `a_crash_between_the_two_commits_folds_and_logs_each_record_once`; `crates/fake-sidecar/tests/adoption.rs` `one_record_with_two_events_is_logged_with_two_sources` |
| The player secret stays private | `crates/sidecar/src/server.rs` `the_server_folder_and_the_migrated_file_are_private`; `crates/xtask/src/bundle.rs` `any_32_hex_digit_token_is_redacted_and_the_keyring_file_is_never_collected` |

## Cost of a write

`crates/sim-fake/tests/persistence.rs` `the_cost_of_a_write_with_two_thousand_units`,
release build, macOS, 2,001 units all moved between writes: a step's commit 0.38 ms
median, a checkpoint 7.3 ms median. S18 wrote every moved row at each commit (7.5 ms,
docs/results/2026-11-15-s18-persistence-host.md).

## Review

Two adversarial rounds and a judge, with `.claude/workflows/review-round.js`. Round 1
accepted 20 issues (5 High) and round 2 accepted 7 (2 High: methods whose stored answers
did not read back, and one record's several events sharing a unique source key). All
were fixed, each with a test.

## Live tests left

S18's six (docs/results/2026-11-15-s18-persistence-host.md), plus:

7. A sidecar restart over a running server: the active campaign resumes, the epoch is
   read back from the server store, and records the agent delivers again are not folded
   twice.
8. Commands held until the step's commit: measure the delay a command waits with the
   server's unit count.

## Open items

- A finished campaign's world reads as empty until S19's read-only open; a read of a
  suspended campaign opens its store.
- The SuspendCampaign and FinishCampaign answers show the checkpoint before the final
  one; `GetCampaign` shows the final one.
- `SampleRates achieved` on `CaptureResponse` is not added.
- A reconciler rebuilt after a step whose `BIND`s were released but not yet applied
  removes the markers of that adoption: the adoption is not read back. Seen in
  `crates/sidecar/tests/reconciler.rs`
  `a_rebuilt_reconciler_removes_the_markers_of_an_adoption_that_ended`, which waits a
  frame for the agent to apply them. For S19, with the restart and recovery work.
