# Shell, reload and health

| | |
|---|---|
| Step | S19 exit test: weather and time change across a commanded reload; a killed DCS, once the host restarts it, is restored unattended; a DCS that crashed into its report window is reported hung within the configured limit; client behaviour across a reload is recorded; a generated shell offers both kinds of player slot, and a client takes each; a campaign on two theaters switches map and back, with the unloaded map's entities virtual and returning; a finished campaign is readable and refuses changes; a campaign with its own editor-built template loads it with its slots, zones and triggers, and the shell writer changes only the runtime fields. |
| Date | 2026-11-15 |
| Host | macOS on arm64; the fake sim behind the API's hub (`crates/api/src/hub.rs` tests) |
| Script | `cargo xtask sidecar <target> --part shell [--template FILE]` (`crates/xtask/src/shell_live.rs`) |
| Result | Host: PASS. Live: see "Live". |

## Host exit tests

Run with `cargo test -p nanoprobe-api -p nanoprobe-sidecar -p nanoprobe-cli` on the
branch; `mise run check` runs them all.

| Exit item | Host test | Result |
|---|---|---|
| Weather and time change across a commanded reload | `a_reload_writes_the_requested_weather_and_start_then_the_clock` (a `NextMission` with a start and a weather Struct, the reload's shell in the second slot, then a reload with no start carrying the clock); `shell::tests::only_the_runtime_fields_change` | PASS on the host; that DCS applies them: live |
| The campaign clock | `the_clock_follows_loads_refusals_and_restarts` (an applied load whose mission starts late, a refused load, a DCS restart that loads no shell) | PASS |
| A killed DCS, restarted by the host, restored unattended | live only (the fake's restart is covered by S18's tests) | live |
| A crashed DCS reported hung within the limit | `a_hung_verdict_is_a_lifecycle_event_on_entering_and_leaving` for the event; the verdict is S17's rule | live |
| Client behaviour across a reload is recorded | none | open: person test |
| A generated shell offers both kinds of player slot, a client takes each | `a_campaign_template_is_checked_kept_and_written_into_its_shell` (the dynamic-spawn flags in the shell); offered slots live | offered: live; taken: open, person test |
| Two theaters, switched and back, the unloaded map's entities virtual | `a_reload_switches_theater_and_back_with_the_other_map_s_entities_virtual` | PASS |
| A finished campaign readable, refusing changes | `a_finished_campaign_reads_through_a_copy_and_refuses_changes`; `campaigns::tests::a_finished_campaign_reads_through_a_copy_that_goes_away` | PASS |
| A campaign's own editor-built template, only runtime fields changed | `a_campaign_template_is_checked_kept_and_written_into_its_shell`; `shell::tests::no_change_keeps_every_member_byte_for_byte` | PASS on the host; with the owner's editor-built mission: live |

Other built items and their tests: `ResolveMember`
(`resolve_member_works_on_a_suspended_campaign`, `store::tests::a_held_member_resolves_dead_or_restored_once`),
the offline restore (`campaigns::tests::a_restore_keeps_ids_and_revisions_above_both_copies`,
`a_restored_finished_backup_stays_finished`, `restore::tests::refuses_a_held_store_dir_and_restores_a_free_one`),
export (`export::tests::exports_every_table_one_json_object_per_line`,
`export_reload_and_delete_through_the_services`), delete refusals
(`a_delete_without_a_name_needs_a_confirmation`,
`a_delete_that_cannot_remove_the_folder_keeps_the_campaign`), a create whose
activation fails (`a_create_whose_activation_fails_leaves_no_campaign`), a start for
another theater kept (`a_start_for_another_theater_survives_a_shell_write`), the clock
in `GetCampaign` (`get_campaign_shows_the_running_clock_before_any_checkpoint`), free space
(`a_full_file_system_refuses_a_copy_and_a_template`;
`a_full_file_system_refuses_a_backup_and_shows_in_the_health` covers a manual backup, a
skipped scheduled backup and `store_space_low`).

## Live

Filled in by the live runs.

## Open

- A client takes each kind of slot, and client behaviour across a reload: person tests
  on the Windows client, which the owner is using during this step.
- Whether DCS keeps a loaded `.miz` open on Windows, so a rename over it fails: a
  Windows check, open while the owner uses the PC. The two shell slots never write over
  the file the last load named.
