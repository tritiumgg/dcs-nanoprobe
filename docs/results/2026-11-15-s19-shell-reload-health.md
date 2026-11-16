# Shell, reload and health

| | |
|---|---|
| Step | S19 exit test: weather and time change across a commanded reload; a killed DCS, once the host restarts it, is restored unattended; a DCS that crashed into its report window is reported hung within the configured limit; client behaviour across a reload is recorded; a generated shell offers both kinds of player slot, and a client takes each; a campaign on two theaters switches map and back, with the unloaded map's entities virtual and returning; a finished campaign is readable and refuses changes; a campaign with its own editor-built template loads it with its slots, zones and triggers, and the shell writer changes only the runtime fields. |
| Date | 2026-11-15 |
| Host | macOS on arm64; the fake sim behind the API's hub (`crates/api/src/hub.rs` tests) |
| Script | `cargo xtask sidecar <target> --part shell [--template FILE]` (`crates/xtask/src/shell_live.rs`) |
| Result | Host: PASS. Live: PASS except the killed DCS (open) and the person tests; see "Live" and "Open". |

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
activation fails (`a_create_whose_activation_fails_leaves_no_campaign`), a switch that leaves the old campaign as a suspend does
(`a_switch_suspends_the_old_campaign_with_its_running_clock`,
`a_refused_load_after_a_switch_back_keeps_the_record_clock`), a start outside years 1 to 9999
(`a_start_outside_years_1_to_9999_is_refused`), a start for
another theater kept (`a_start_for_another_theater_survives_a_shell_write`), the clock
in `GetCampaign` (`get_campaign_shows_the_running_clock_before_any_checkpoint`), free space
(`a_full_file_system_refuses_a_copy_and_a_template`;
`a_full_file_system_refuses_a_backup_and_shows_in_the_health` covers a manual backup, a
skipped scheduled backup and `store_space_low`).

## Live

| | |
|---|---|
| Date | 2026-11-16 |
| Target | DCS dedicated server 2.9.31.29434 (`dcs_server.release`) under Wine, `wine-10.0 (Debian 10.0~repack-6)`, in a Docker container; the sidecar in a container of its own sharing its network |
| Template | an editor-built Caucasus mission with a Client AH-64D group, a trigger zone and one trigger rule (`--template`; the mission is not in the repository) |
| Script | `cargo xtask sidecar <target> --part shell --template FILE`; verdict in `bundles/sidecar-<target>-<time>/sidecar.json` |
| Runs | 4. Run 1 passed the reload, then lost its lease to slow evals after the Afghanistan load; run 2 failed `own_template` on the dynamic-slot listing and stopped after the DCS restart; runs 3 and 4 passed every item below but the killed one. |

| Exit item | Live result |
|---|---|
| Weather and time change across a commanded reload | PASS (runs 1 to 3). LIVE: `net.load_mission` loads `shells\<id>.<slot>.miz` under Wine. The mission DCS ran had the requested date (2031-07-04), start (55,800 s), ground wind (7) and temperature (31); the campaign clock read the requested start plus 2 s. |
| Two theaters, switched and back | PASS (runs 2, 3): Afghanistan, Caucasus, Afghanistan, each with its group live and the other map's group `VIRTUAL`. |
| A campaign's own editor-built template | PASS (run 3): every member of the shell is the template's, `l10n/DEFAULT/dictionary`, `mapResource`, `options` and `theatre` byte for byte; `mission` and `warehouses` differ only in the runtime fields; the zone resolves with `trigger.misc.getZone`, the rule count matches, the Client slot is offered, and the shell carries the airport's dynamic-spawn flag. |
| Offered slots | Client slots: offered (`DCS.getAvailableSlots` lists both AH-64D seats). Dynamic-spawn slot: LIVE, `DCS.getAvailableSlots` lists none while the shell carries the flag; whether a client is offered one is the person test below. |
| A finished campaign readable, refusing changes | PASS (runs 2, 3): its group reads `VIRTUAL`; a change is refused `CAMPAIGN_READ_ONLY`. |
| A crashed DCS reported hung within the limit | PASS (runs 2, 3): with `heartbeat_stale_s = 10` and a limit of 25 s, the health verdict went to hung 10.8 s after the deliberate fault, with a lifecycle event. E22: after the fault the `DCS_server.exe` process stays alive under Wine (no exit), so the verdict, not the process, shows the crash. No dialog was looked for; which prompts block an unattended start stays open. |
| A killed DCS, restarted by the host, restored unattended | Open. In runs 3 and 4 the kill worked and the image started DCS again. Run 3's kill command failed on its own shell. In run 4 the sidecar, which exits when DCS restarts, came back with an empty store: the driver kept the store on the container's tmpfs, which a container restart clears. The driver now keeps it in a profile folder; it has not run live. |
| E7: `Airbase.id` from the warehouses keys | Not equal on Afghanistan: 29 airdromes, 26 warehouse airports; FOB Thunder (26), FOB Camp Dubs (27) and FOB Clark (28) have no warehouse entry. `Airbase.id` stays out of `world.proto` (ruling item 3). |

## Open

- A client takes each kind of slot, whether a client is offered a dynamic-spawn slot,
  and client behavior across a reload: person tests on a Windows client with the S19
  sidecar running.
- A killed DCS restored unattended: a live run of the shell part with the store kept
  across sidecar restarts.
- E22: which dialogs block an unattended start, and whether each has a setting.
- E7: `Airbase.id`, since the airdrome IDs and the warehouses keys differ.
- A test left a temporary folder twice in `mise run check` under load; which one is not
  known.
- Whether DCS keeps a loaded `.miz` open on Windows, so a rename over it fails: a
  Windows check, open while the owner uses the PC. The two shell slots never write over
  the file the last load named.
