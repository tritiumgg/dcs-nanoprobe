# Chaos and faults on the Wine server and the host

| | |
|---|---|
| Step | S8 exit test: 100 kills and restarts of either side: no DCS crash, reconnect under 2 s. Each injected fault is survived or refused with a clear log line. |
| Date | 2026-11-13. Server chaos 01:33 to 03:41 UTC; server faults 04:03 to 04:07 UTC; host chaos 04:07 UTC |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server (LIVE, from `target check` in each run) |
| Platform | Server: Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker, the runner and the probe in the game container. Host: macOS 26.6.2 on arm64, Rust 1.99.1, the strict mock with the host-native agent. |
| Builds | Chaos: `365d33e8c837e512`, the shipping DLL (`deploy --dev`). Faults: `d5c28d45310cfa86`, the same code with fault injection (`deploy --dev --faults`). |
| Script | `cargo xtask chaos gameserver --cycles 100` and `cargo xtask chaos host --cycles 100` (`crates/xtask/src/chaos.rs`, `crates/fake-agent/src/bin/chaos-runner.rs`, `lua/chaos/host.lua`); `cargo xtask faults gameserver` (`crates/xtask/src/faults.rs`) |
| Rehearsal | `lua/tests/faults.lua` and `crates/fake-agent/tests/chaos.rs`, in `mise run check` |
| Code | The commit that adds this document |
| Result | PASS |

## Chaos

Each cycle keeps the agent and the sidecar attached for 0.1 to 2 s, then kills one of
them at random (spec/faults.md, "Chaos"). The sidecar is `link-probe`, killed with
`SIGKILL` and started again. On the server DCS is killed with `pkill -9`, and the
container's own start loop starts it again; the run only checks that no player is
connected before each DCS kill, and keeps DCS up 70 s between DCS kills for the host's
watchdog. A DCS reconnect is timed from the new agent's first frame of a loaded
mission, or from when it listens if that is later; a sidecar reconnect from the kill.

| Run | Seed | Cycles | DCS kills, max | Sidecar kills, max | Failures |
|---|---|---|---|---|---|
| Wine server | `1b5a2feadfd4c2ab` | 100 | 49, 117 ms | 51, 847 ms | 0 |
| Host | `ea97729f20f985c7` | 100 | 47, 1,220 ms | 53, 849 ms | 0 |

On the server, DCS reconnects took 112 to 117 ms (median 115) and sidecar reconnects
349 to 847 ms (median 835). The first frame came 104 to 111 s after each DCS kill; that
time is DCS starting and loading its mission. The agent listened at once after every
kill (wait 0 ms). The hook logged 49 starts for 49 kills; no error record in the
agent's, hook's or shim's log, no crash record, and no crash file from DCS. Every
sidecar report was a session end a kill explains.

The server's DCS reconnect is faster than the host's because the agent already
listens and the sidecar already waits on its connection when the first frame comes;
on the host the sidecar connects after the mock's first frames, and the agent accepts
once a second.

## Faults

On the server, 12 of 12 pass:

| Fault | What happened |
|---|---|
| Lua error from the frame entry | The hook logged `onSimulationFrame raised` with `injected Lua error`; the frame number went on (952, then 1,076); one attach |
| 1.5 s stall in a frame | The agent logged the frame stalled 1,500 ms; the sidecar stayed attached |
| Flood of 10,000 records | The sidecar read a log `GAP` of 8,980 records; stayed attached |
| Lost control record, then a reload | A second `HELLO` in the same attach; the drop logged |
| Sidecar stopped 2 s | `detached: the sidecar's heartbeat stopped for 0.5 s`; attached again 849 ms after resuming |
| Command shorter than its header | `a command too short for its header; detaching`; attached again |
| Record past the tail | `record passes the tail` |
| Record over the limit | `len over an eighth of the ring` |
| Misaligned tail | `not a multiple of 8` |
| Tail past the capacity | `more than the capacity ahead of the head` |
| Padding not at the ring's end | `padding does not end at the end of the ring` |
| Panic in a frame | `disabled: a caught panic` and `DISABLED` read; the hook went on; DCS answered the eval channel |

Each broken ring made the agent detach with `detached: inbound ring:` and the check, and
the sidecar attached again. DCS was restarted after the panic, which leaves the agent
disabled until then. No player was connected during any fault.

## What the server runs found

Five earlier server chaos runs stopped at or near their first DCS kill. Four found
defects in the runner's timing, the agent or the sidecar, now fixed and specified; one
was a fault of the test run itself.

| Finding | Change |
|---|---|
| The agent writes a frame number while a mission loads, and a starting DCS runs about 5 frames at generation 0 before its 55 s first load (LIVE) | The runner times from the first frame of a mission at generation 1 or later |
| Under Wine, the handshake port of a killed DCS stays taken for up to about a minute: `OS Error 10048` 59 s after a kill (LIVE) | The agent tries the listen again once a second until it succeeds (spec/transport.md, "Handshake") |
| The sidecar's 15 s handshake timeout counted load time, so it connected again during the load, and the agent answered each stale connection in turn: a 9.4 s reconnect (LIVE) | The timeout counts only time in which the agent runs frames (spec/transport.md, spec/config.md) |
| A run stopped while its DCS kill was in flight, and the next run read the stale attach | The runner waits for an attach by its own sidecar at its start |

The first live fault run found one more: a sidecar stopped during a mission load is not
detached, because the agent runs no frame then. spec/faults.md records it, and the test
waits for the reloaded mission to run before it stops the sidecar.

## Not covered

Windows: queued for the next Windows checkpoint (PLAN.md section 8). A record that
passes the end of the ring is not injected live; the transport's unit tests cover it.
