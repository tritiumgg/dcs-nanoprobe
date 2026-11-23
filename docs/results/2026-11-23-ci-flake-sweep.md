# The CI flake sweep after 0.1.0

| | |
|---|---|
| Step | Run the host suite repeatedly on GitHub's shared runners, collect every test that fails by timing, and fix them in one batch. CI runs Linux and Windows; a macOS host checks macOS with `mise run check` |
| Date | 2026-11-23 |
| DCS | None. Every test here runs on the host against the fake agent or the fake sim |
| Platform | GitHub `ubuntu-latest` and `windows-latest` runners; host check on macOS 26.6.2, arm64 |
| Script | `.github/workflows/ci.yml`, started by hand with `gh workflow run CI --ref <branch>` |
| Code | `73dd99e`, this branch's fixes (last one `f82642b`) merged with main at `49474ec`; the commit after it changes only this document |
| Result | Six tests failed by timing on the runners or under load on the host; all are fixed in test code (and in the Rust hello example), none in the product. Three consecutive CI runs on `73dd99e` passed on Linux and Windows (45170990880, 45176319046, 45180794833) |

## Flakes fixed

| Test | Platform | Run | Cause | Fix |
|---|---|---|---|---|
| `compaction_at_open_keeps_the_newest_event_and_ids_skip_unreadable_folders` (`crates/fake-sidecar/tests/persistence.rs`) | Windows, Linux | 45079543930 (`19e4fe5`), 45083127779 (`db76db7`) | The read after the restart found events `[1, 2, 3]` where only the newest, 3, should survive the 20 ms retention. The hub stamps an event with the wall-clock time at which the driver read the last live frame (`Link::stamp`, `crates/api/src/events.rs`), so the stamp stands still while no frame is read. The test slept 50 ms between its publishes; on a loaded runner the driver read no frame in that time, so all three events had one stamp and none was older than the cut. The unreadable-folder scan was correct: that assertion passed in both runs. No product defect | The test waits until the hub's stamp is 100 ms past the first publish, and until the wall clock passes the newest stamp before the restart (a clock behind the newest event skips the time limit) |
| `hello_campaign_in_process` (`crates/fake-sidecar/tests/api.rs`, `examples/hello-rust/src/lib.rs`) | macOS | 45079543930 (`19e4fe5`) | The test required the states `[Materializing, Live]`; it saw `[Live]`. The hello kept only the revision of its `ListWorld` and recorded the states its watch saw after it. On a loaded runner the group was already Materializing at the listed revision, so the watch showed only Live. Had the group been Live at the list, the watch would have shown no state and the hello would have waited out its 60 s limit. The race exists on every platform. First recorded in `docs/results/2026-11-18-ruins.md` (open items) | The hello records the group's state from the `ListWorld` pages, then each change its watch sees. The test accepts any tail of `[Virtual, Materializing, Live]` |
| `extension-budget` (`lua/tests/dispatch.lua`) | macOS (the local check; the Lua tests also run in CI on Linux) | none: `mise run check` on `a48b10c` | "The queued events were not all run later". The scenario's last invoke waited 400 ms of wall time for the 20 queued calls, one a frame. Other checks loaded the host at the time, and it ran too few frames in 400 ms | The invoke waits for a file the test writes 200 frames after the burst (`wait_file`, as the other extension scenarios do) |
| `lua/tests/loadout.lua` | Linux | 45140479872 (`894693e`) | "No AMMO record for the player unit after its birth". The test waited 20 s of wall time for the record. The agent spends a small share of each frame, and on that runner one pass of its enumeration took 18 s (`enumeration_s` in its sampling log) | The waits for the birth and takeoff records end on the record; their limit is 120 s, only to stop a hang |
| `the_timers_take_checkpoints_and_backups_and_keep_backup_keep` (`crates/fake-sidecar/tests/persistence.rs`) | Windows | 45153287278 (`7e8ccea`) | The manual backup failed with ABORTED: the lease was no longer current. The test acquires one lease and never renews it. On that runner each scheduled backup took up to 5.6 s, and the test reached the manual backup after the 30 s default TTL had run out | The test sets the lease TTL to ten minutes; each of its waits keeps its own limit |
| `lua/tests/births.lua` | Linux | 45157825891 (`05805eb`) | "Only 108 of 120 births have an OBJECT record after the queue emptied". The test waited 30 s of wall time for the records, the same deadline as `loadout.lua` | The two waits for OBJECT records end on the records; their limit is 120 s, only to stop a hang |

The other failures in the recent main runs were fixed before this sweep:

- `a_long_detach_keeps_the_event_queue_within_its_cap` and `a_str_that_is_not_utf8_is_found` (`crates/fake-agent/tests/misbehave.rs`; macOS, 45073338249, `3526524`): fixed in `a20494b`.
- `hub::tests::a_full_file_system_refuses_a_backup_and_shows_in_the_health` (`crates/api/src/hub.rs`; macOS, 45034128289, `0e325e8`): SQLITE_BUSY from the backup read, the product defect fixed in `bd54951`.
- `lua/tests/world_exit.lua` ("go holds no ground height"; Linux, 45034128289, `0e325e8`): the `go` file race, fixed in `c4d2414`.
- Run 45029909859 (`8812cf3`) failed every job before its first step; no test ran.

## Sweep runs

Each run is the whole CI workflow on Linux and Windows, started by hand.

| Run | Commit | Linux | Windows |
|---|---|---|---|
| 45121378884 | `400a027` (CI change only) | pass | pass |
| 45124550610 | `4edbc07` | pass | pass |
| 45128430805 | `4edbc07` | pass | pass |
| 45131742451 | `a48b10c` | pass | pass |
| 45136193170 | `a48b10c` | pass | pass |
| 45140479872 | `894693e` | `loadout.lua` | pass |
| 45144095881 | `7e8ccea` | pass | pass |
| 45148895185 | `7e8ccea` | pass | pass |
| 45153287278 | `7e8ccea` | pass | backup-timer lease |
| 45157825891 | `05805eb` | `births.lua` | pass |
| 45161245631 | `f82642b` | pass | pass |
| 45166181570 | `f82642b` | pass | pass |
| 45170990880 | `73dd99e` (main merged in) | pass | pass |
| 45176319046 | `73dd99e` | pass | pass |
| 45180794833 | `73dd99e` | pass | pass |

`mise run check` and `mise run dll` pass on the host on `73dd99e`.

## Open items

- Other Lua tests wait on wall-clock deadlines of 30 s or less in the way `loadout.lua` and `births.lua` did (`faults.lua`, `link.lua`, `sampling.lua`, `samplingcheck.lua`). Some of those deadlines check the project's timing targets and must stay; the others are candidates for a hang limit if they fail.
- The Python and Kotlin hellos (`examples/hello-python/hello.py`, `examples/hello-kotlin/src/main/kotlin/Hello.kt`) list and watch the same way the Rust hello did: a group already Live at the list leaves them waiting until their limit. Their checks run in `mise run check`, so in CI on Linux; no run failed on it.
