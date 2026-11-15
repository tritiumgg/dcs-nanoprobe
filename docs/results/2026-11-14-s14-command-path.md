# Command path

| | |
|---|---|
| Step | S14 exit test: 1,000 seeded mixed commands with forced sidecar restarts; every set-state command applied exactly once per revision, no effect applied twice; on host, then on DCS. Overhead compared with S11. |
| Date | 2026-11-14. Live runs 1 and 2, overhead runs 1 and 2, and an A/B against main |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, run provenance) |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Host rehearsal: macOS on arm64. |
| Builds | Runs 1 (live and overhead) ran on the tree before the review fixes, agent build `b06705f22086607b`. Runs 2 ran on the final tree, agent build `88b409424477529a`. The A/B ran main `a6cf0cc` (agent build `355abe04f0424bbd`), then the final tree (agent build `f5e8a17a8e397eb3`; the same agent code as run 2, built after the documentation commits). |
| Script | `cargo xtask commands <target>` (`crates/xtask/src/commands.rs`), which runs `command-probe` (`crates/fake-agent/src/bin/command-probe.rs`) in the game container as the sidecar; `cargo xtask sampling <target> --part mirror` for the overhead |
| Rehearsal | `crates/sim-fake/tests/commands.rs` (seeded, 50 seeds), `lua/tests/commands.lua` (1,000 commands, a restart every 100), `lua/tests/dispatch.lua` (handler dispatch with `command-script`), `crates/sidecar/tests/reconciler.rs`, `crates/agent-link/src/link_admit_tests.rs`, all run by `mise run check` |
| Bundle | `bundles/commands-<target>-<stamp>/`, `bundles/sampling-<target>-<stamp>/` (not committed: they hold server data) |
| Result | PASS with open items. The live exit test passes on both trees. Overhead on the final tree matches S11: mean share and the exact tail meet the owner's targets, and the median position interval meets the adopted measure. The mirror part's window ceiling failed in S14 runs 1 and 2; a back-to-back A/B then failed it on main and passed it on the final tree, so the failures are DCS variance, not S14. |

All figures are LIVE unless tagged.

## Exit test

`command-probe` runs the sidecar's own store and reconciler over the real link. It writes
seeded `EXTENSION_SET` changes (set-state) and `EXTENSION_INVOKE` calls (effects), and starts
a new link session and reconciler from the kept store after every 100 commands, as a sidecar
restart does. Each extension's chunk counts its runs by name and revision, and `invoke`
counts each token, in one table in the `server` state. At the end an audit extension
returns that table. The probe checks the audit against the journalled outcomes. Raw Lua is
on in `agent.toml` for the run only; the run puts the file back as it found it.

| Run | Bundle stamp (UTC) | Written | Restarts (cutting a command in flight) | Revisions run once | Tokens invoked of sent | Twice | Missing | Outcomes | Result |
|---|---|---|---|---|---|---|---|---|---|
| 1 | `commands-<target>-20261114T210724Z` | 1,003 | 9 (not counted) | 458 | 504 of 507 | 0 | 0 | 997 applied, 3 refused, 3 unknown | PASS |
| 2 | `commands-<target>-20261114T230956Z` | 1,002 | 9 (9; 33 commands cut, 36 `AT_REVISION`) | 456 | 505 of 508 | 0 | 0 | 995 applied, 3 refused, 4 unknown; 0 mismatched against the audit | PASS |

A token not invoked was answered `unknown` or refused, and the audit shows it never ran.
Run 1 used the probe before the review added the outcome-against-audit and
cut-restart checks.

## Overhead against S11

Compared with S11 runs 13 and 14 (`2026-11-14-s11-budget-and-baseline.md`, bundles
`sampling-<target>-20261114T143647Z` and `...T144622Z`), the mirror part at 2,000 objects
with a fifth of the ground groups routed.

| Measure | S11 run 13 | S11 run 14 | S14 run 1 | S14 run 2 (final tree) | A/B: main | A/B: final tree |
|---|---|---|---|---|---|---|
| Mean share from the 800 spawn | 0.22 % | 0.208 % | 0.216 % | 0.201 % | 0.232 % | 0.208 % |
| Mean share at 2,000, settled | 0.211 % | 0.19 % | 0.209 % | 0.193 % | 0.231 % | 0.200 % |
| Largest 10 s window | 0.34 % | 0.30 % | 0.32 % | 0.36 % | not compared | not compared |
| Frames over 250 µs | 85 of 40,523 (0.21 %) | 77 of 41,417 (0.19 %) | 79 of 40,593 (0.19 %) | 104 of 38,998 (0.27 %) | 80 of 41,319 (0.19 %) | 88 of 38,918 (0.23 %) |
| Largest frame | 3,312 µs | 3,426 µs | 3,268 µs | 3,205 µs | 3,407 µs | 3,410 µs |
| Median position interval, stall-free | 1.008 s (proxy) | 0.994 s (proxy) | 1.001 s | 1.006 s | 1.004 s | 0.993 s |
| Worst stall-free window | 1.084 s | 1.012 s | 3.46 s | 3.07 s | 3.99 s | 1.08 s |
| Fill-window position | 1.02 s | 1.05 s | 4.03 s | 2.07 s | 3.20 s | 1.08 s |
| DCS fps at 2,000 (p5) | 55.8 | 59.6 | 65.1 (0.54) | 51.4 (0.40) | 76.5 | 56.0 |

The mirror matches at 500, 1,200 and 2,000 in both S14 runs. Mean share stays under 0.3 %
and frames over 250 µs under 1 %. The median meets the adopted 1.05 s limit. Run 2 fails one
check: a stall-free window's position period of 3.07 s against the 1.25 s ceiling. Run 1
also failed the fill-window check (4.03 s against 3 s); S11 runs 8 and 9 read 3.21 and 3.73 s
with no code cause, and run 2 reads 2.07 s.

To tell a regression from DCS variance, the mirror part ran on main `a6cf0cc` and then on
the final tree, back to back (bundles `sampling-<target>-20261114T234008Z` and
`...T234940Z`). Main failed the window ceiling (3.99 s), the fill-window check (3.20 s) and
the mirror match. The final tree passed every check. The window and fill-window failures
are therefore DCS variance, not S14. The mirror run sends no commands, so S14's command
path does no work in it.

## Behavior settled

- A command whose Lua handler runs in the `server` state waits in the ring for a pump; the
  agent takes it in the pump and answers it there. With no shim loaded outside `LOADING`
  it answers `NOT_READY` (27; Protobuf 28), which the sidecar classes as rejected and
  derives again after a lifecycle change, a `HELLO`, or 300 frames.
- `RAW_LUA_OFF` comes before `NOT_READY` and is final. An invoke of an extension with no
  revision in the campaign answers `LUA_ERROR`.
- Command time counts against the commanded budget only, outside the sampler's debt,
  credit and share.
- A `RESULT` message is capped at 1,024 bytes. A handler value over 4,096 table entries or
  16 KiB of strings, or a `RESULT` over the record limit, is answered `TOO_LARGE` with a nil
  value.
- Health shows the agent's interface version (`ServerStatus.agent_interface`).

## Open items

Every item below is for a later step.

- The mirror part's window ceiling (1.25 s) and fill-window check (3 s) at 2,000 objects
  fail on main and on S14 alike in some runs (3.07 to 3.99 s; 3.20 to 4.03 s). S11's tenth
  part showed the positions sweep waiting on the enumeration for 6 to 27 s; that the same
  wait causes this is UNVERIFIED.
- Main `a6cf0cc` failed the mirror match once in the A/B; S14's runs matched every time.
  Not investigated.
- `lua/tests/sampling.lua` `deaths` failed once in a full `mise run check` (no records,
  `pumps` 0) and passed 4 times alone and in every later full check. Cause not found; not
  compared with main.
- The `LOADING` wait for a `server` command is unreachable today: `frame` moves `LOADING` to
  `RUNNING` or `PAUSED` first. Whether DCS runs a frame before `onSimulationStart` is
  UNVERIFIED.
- The shim keeps the previous campaign's extensions after a campaign switch and runs their
  chunks again at each mission start; invokes of them are refused. Clearing them goes with
  `on_stop`.
- `CHAT` and `MISSION_LOAD` are `hook` state, UNVERIFIED; until hook dispatch exists the agent
  answers hook-state opcodes `UNKNOWN_OPCODE`.
- The sidecar does not read the breadcrumb after DCS dies (the inspector does); a dead or
  kill-switched boot's commands stay pending until a new boot's `HELLO`.
- DCS ran at 51.4 fps at 2,000 objects in overhead run 2, against 55.8 to 65.1 in the
  other runs. No target covers it; a later overhead run watches it.
- S16 inherits `EXTENSION_REMOVE`, the extension API, `on_event`, `on_frame`, `on_stop` on
  campaign switch and simulation stop, the replaced globals of "The environment", `emit`,
  `EXTENSION` records, and hook-state dispatch for `CHAT` and `MISSION_LOAD`.
