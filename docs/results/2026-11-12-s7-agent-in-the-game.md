# The agent's link in DCS on the Wine server

| | |
|---|---|
| Step | S7 exit test: handshake visible in the inspector; 10 mission reloads with reconnect under 2 s; kill switch stops work within 2 s |
| Date | 2026-11-12, 22:44 to 22:52 UTC |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker; the probe and the inspector run in the game container |
| Build | `73350103b345a38d`, agent ABI 2 |
| Script | `cargo xtask deploy <target> --dev`, `cargo xtask restart <target>`, then `cargo xtask link <target>` (`crates/xtask/src/link.rs`, `lua/live/reloads.lua`, `crates/fake-agent/src/bin/link-probe.rs`) |
| Rehearsal | `lua/tests/link.lua` on macOS, under the strict mock with the host-native agent |
| Code | The commit that adds this document |
| Result | PASS on the Wine server. Windows: queued for the plan's next checkpoint. |

## Setup

The target profile names `map_path = 'Z:\dev\shm\nanoprobe.map'`; deploy wrote it to
`agent.toml` and created the handshake token. DCS mapped the file through Wine's `Z:`
drive, which is the container's root, so the probe and the inspector open
`/dev/shm/nanoprobe.map`. Both are static Linux programs, copied into the container and
run there with `docker exec`, so they share DCS's loopback network and `/dev/shm`. The
test removes them afterwards.

At start the hook logged (LIVE): the agent loaded with ABI 2 and pinned, the link open on
`Z:\dev\shm\nanoprobe.map`, DCS version `2.9.31.29434`, every name in the hook's
capability list present, and the shim started in the `server` state on the first
simulation start. The server's mission starts paused, as a dedicated server with no
client does, so every mission in this run is `PAUSED` rather than `RUNNING`.

## Results

| Check | Rule | Result |
|---|---|---|
| Handshake in the inspector | `nanoprobe inspect` shows `ATTACHED` with the probe's boot | `flags ATTACHED`, `sidecar_boot_seen` = `sidecar_boot`, `attach_seen` set; the `HELLO` names build `73350103b345a38d`, interface 2 |
| Commands | `CAMPAIGN 7`, then a `MESSAGE` for campaign 7 and one for 8 | campaign 7 stored; answers `UNKNOWN_OPCODE` and `WRONG_CAMPAIGN`; `epoch 1`, `last_applied_seq 2` |
| 10 reloads | Each read by the sidecar under 2 s after the mission runs | 10 of 10; 15 to 19 ms |
| Attached throughout | One attach; no detach before the kill switch | 1 attach, 0 detaches |
| Shim reload | The sidecar reads the hook's `shim reloaded` | read |
| Kill switch | `DISABLED` within 2 s of creating `disable`; the pump stops | 250 ms; `frame` stayed at 1973 |
| Kill switch records | The reason and the last `LIFECYCLE` reach the sidecar | both read |
| Restore | `disable` removed, DCS restarted, mission started | done; the test programs removed |

**Reconnect.** The link stays attached through a mission load, so a reload is timed
from the moment the status block shows the new mission paused to the moment the
sidecar reads the agent's `LIFECYCLE` saying so, both on the container's clock. Per
generation 2 to 11: 18, 17, 15, 18, 17, 17, 18, 18, 19 and 17 ms. The time is mostly
the frame that follows `onSimulationStart` (about 9 ms at 110 frames a second) and the
probe's 2 ms poll.

**Loads.** The reloads of the server's mission took 6.24 to 6.40 s each, from
`net.load_mission` to `onSimulationStart` (LIVE). The first load after the restart took
19 s. Neither is a property of the platform; they are recorded only to place the
reconnect times.

**Kill switch.** The file was created at 1794523622953 ms and the status block read
`DISABLED` at 1794523623203 ms, 250 ms later: the agent looks on its system-call frame,
which comes once a second while attached. After it the probe's connects were refused,
the listener being closed.

## Per-frame cost

No stored baseline exists before S11. From the status block at the end of the run
(1,972 pump calls, about 110 frames a second):

| Histogram | Samples | p50 | p99 | max |
|---|---|---|---|---|
| `hist_pump` (agent time per pump call) | 1,972 | under 4 µs | under 256 µs | under 2 ms |
| `hist_command` | 3 | under 1 µs | under 2 µs | under 2 µs |

The p99 reflects the system-call frame, once a second, whose socket call costs 150 to
290 µs under Wine (E25); the maximum is one frame, likely the kill switch's, which also
closes the connection and the listener. S11 measures both against the budget.

## On the host

The rehearsal runs the same steps under the strict mock, with 0.5 s with no frame inside
each load, and adds a command shorter than its header, which makes the agent detach
with a `LOG` to the sidecar. It passes; its slowest reload took 2 to 4 ms. The
1,000-cycle kill-the-peer test (`cargo xtask peerkill`) ran again on the agent's link
code, now shared by the DLL and the fake agent: PASS, reconnect max 1.24 s after an
agent kill and 0.87 s after a sidecar kill.

Building the rehearsal found one defect the live test would also have met: the sidecar
ended its session on a closed connection before reading what the agent had just
published, so the kill switch's last records were lost. The sidecar now reads the ring
once more when the agent closes or detaches it (spec/transport.md, "Detaching").
