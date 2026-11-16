# Shell, reload and health

| | |
|---|---|
| Step | S19 exit test: weather and time change across a commanded reload; a killed DCS, once the host restarts it, is restored unattended; a DCS that crashed into its report window is reported hung within the configured limit; client behaviour across a reload is recorded; a generated shell offers both kinds of player slot, and a client takes each; a campaign on two theaters switches map and back, with the unloaded map's entities virtual and returning; a finished campaign is readable and refuses changes; a campaign with its own editor-built template loads it with its slots, zones and triggers, and the shell writer changes only the runtime fields. |
| Date | 2026-11-15 |
| Host | macOS on arm64; the fake sim behind the API's hub (`crates/api/src/hub.rs` tests) |
| Script | `cargo xtask sidecar <target> --part shell [--template FILE]` (`crates/xtask/src/shell_live.rs`) |
| Result | Host: PASS. Live: see "Live". |

## Host exit tests

| Exit item | Test | Result |
|---|---|---|
| Weather and time across a reload | `activation_writes_the_shell_from_the_next_mission_and_the_clock_follows`; `shell::tests::only_the_runtime_fields_change` | PASS: the shell holds the set start and the dynamic-spawn flags; the start is used once; the clock runs from the shell's start |
| Two theaters there and back | `a_reload_switches_theater_and_back_with_the_other_map_s_entities_virtual` | PASS: each reload leaves the other map's group `virtual` and the loaded map's `live`; shells alternate between the two slots |
| A campaign's own template | `a_campaign_template_is_checked_kept_and_written_into_its_shell`; `shell::tests::no_change_keeps_every_member_byte_for_byte` | PASS: refused for a theater not the campaign's, a file that is not a `.miz`, and a placed name that reads as a platform name; the shell carries the template's own weather |
| A finished campaign readable, refusing changes | `a_finished_campaign_reads_through_a_copy_and_refuses_changes`; `campaigns::tests::a_finished_campaign_reads_through_a_copy_that_goes_away` | PASS: read through a migrated copy under `.read/`, removed after 60 s unused; changes refused as read-only |
| Hung verdict | `a_hung_verdict_is_a_lifecycle_event_on_entering_and_leaving` | PASS on the host for the event; the verdict itself is the S17 health rule, checked live |
| Resolving a held member | `store::tests::a_held_member_resolves_dead_or_restored_once` | PASS |
| Offline restore | `campaigns::tests::a_restore_keeps_ids_and_revisions_above_both_copies`; `restore::tests::refuses_a_held_store_dir_and_restores_a_free_one` | PASS |
| Export | `export::tests::exports_every_table_one_json_object_per_line`; `export_reload_and_delete_through_the_services` | PASS |

## Live

Filled in by the live runs.
