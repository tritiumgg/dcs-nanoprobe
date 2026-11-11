# First DLL and spikes on a dedicated server

| | |
|---|---|
| Step | S4: the minimal agent and experiments E2–E5, E10, E13, E18, E20, E21, E23–E27, E29, E30, E33 |
| Date | 2026-11-11 (08:35–09:20 UTC) |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Agent | ABI 1, static CRT, build `f8503cf7fcf2b5d3`, PDB `8A739528CC2F377F4C4C44205044422E1` |
| Scripts | `lua/experiments/*.lua` through `cargo xtask probes <target> --dir lua/experiments --only <name>`; `cargo xtask mapped` with `tools/mapped-peer.py`; `cargo xtask gates`; `lua/experiments/alone/*.lua` through `cargo xtask eval`; `cargo xtask symbolize` |
| Mission | A shell generated from `templates/caucasus.miz`, loaded fresh by `cargo xtask run`; no client connected |
| Result | Every experiment that needs no client has a result. E25 misses the budget: see "E25" and the summary. E18, E30 and the player-facing part of E21 and E23 need a client and are queued. |

Every value is LIVE on this build and platform unless a line says otherwise. No player
was connected at any restart or during any run. Nothing here ran on Windows; each
experiment's Windows run is queued for the first Windows checkpoint (last section).

## The agent

`crates/agent` is the minimal agent the plan's S4 row asks for:

- `luaopen_nanoprobe` is idempotent per state: a second open returns the table kept in
  the registry. The first open in the process pins the DLL and installs the panic hook.
- Every function on the table has the entry shape of `crates/agent/src/entries.rs`: the
  entry owns nothing, `guarded` runs the worker under `catch_unwind`, and only the entry
  calls `lua_error`. `tools/raiseshape.sh` reads the built DLL's assembly and fails if
  an entry has a destructor funclet. A panic disables the agent for the rest of the
  process.
- `configure(root)` opens `logs/agent.log` (spec/log.md) and installs a vectored
  exception handler that appends a record to `logs/crash.log` for an exception raised
  in the DLL, or anywhere while an agent call runs (spec/install.md, "Agent").
- The hook opens the DLL in its own state at start and logs the thread, the pin and the
  PDB signature.
- Probe functions for the experiments: `thread`, `clock`, `qpc_cost`, `crumb`, `raise`,
  `call`, `sink`, `csweep`, `sock_*`, `map_*`, `panic`, `fault`.

The DLL links the C runtime statically (`.cargo/config.toml`), and `tools/checkdll.sh`
fails a build that imports the dynamic one.

## Results

| # | Rule | Result | Verdict |
|---|---|---|---|
| E2 | Bootstrap works with the keys set and the sandbox call fails | Works; sandbox refused | PASS |
| E3 | One thread | One thread for every kind of call | PASS |
| E4 | 50 reloads and a clean exit | 50 of 50; clean exit through the gui state | PASS (Wine) |
| E5 | Adopt C-driven only if at least 2x faster | At most 1.2x | Lua loop with a C sink stays |
| E10 | Recorded | About 111 frames a second, running or paused; no acceleration setter | Recorded |
| E13 | Completes; time recorded | 2,000 units in 836 ms, one frame | PASS |
| E18 | Remote client | Not run: needs a client | Queued |
| E20 | Behaviour recorded; validity rule confirmed or tightened | IDs not reused; a dead unit's ID still reads a position | Rule tightened |
| E21 | Naming rule fixed in S5 | Unit and static names share one namespace; group names are apart | Recorded; visibility queued |
| E23 | Recorded; a change is raised | `getTaintedFiles()` still nil | No change seen on the server; client side queued |
| E24 | Status block works, or the stream fallback | Same pages, no torn read | PASS |
| E25 | Feeds E19 | About 380 µs per frame at p50, 720 µs at p99 | **Misses the budget** |
| E26 | Caught by `lua_pcall`, no crash | 2,000 of 2,000 in each state | PASS (Wine) |
| E27 | Loads; otherwise the dynamic CRT | Loads, pinned | PASS (Wine) |
| E29 | A generated template loads and offers slots | The Mission Editor modules do not load on the server | Manual save stays |
| E30 | A scripted client joins | Not run: needs the Windows PC | Queued |
| E33 | Recorded; above 1 µs is raised | 73 ns | PASS |
| Crash capture | A panic and an access violation each leave a diagnosable bundle | Both do | PASS |

### E2: the autoexec.cfg gates

`cargo xtask gates <target> on` added, between marker lines, to a new
`Config/autoexec.cfg`:

```lua
net.allow_unsafe_api = { 'userhooks' }
net.allow_dostring_in = { 'server', 'mission', 'gui', 'export', 'config' }
```

After a restart, `e02-gates.lua`:

| Call | Result |
|---|---|
| Hook to `server`, `gui`, `mission`, `export`, `config` | each returned `"reached", true` |
| Hook to `scripting`, not in the list | no values: refused |
| Sandbox (`a_do_script` environment) to `server` | no values: refused |
| The hook opened and configured the agent at start | yes |
| The `config` state sees `net.allow_unsafe_api` | yes |

`userhooks` admits the hook state on the dedicated server. The target list takes
`server`; `scripting` is a separate entry, though both names reach one state. The
installer can therefore set these keys, which also closes the sandbox escape of plan
3.1. `cargo xtask gates <target> off` removed the block and the file it created.

### E3: thread identity

Every call ran on one OS thread: the hook's open of the DLL at start (hook.log's "agent
loaded" record), a hook eval snippet, the hook's `onSimulationFrame`, `net.dostring_in`
into `server`, `gui` and `export`, an event handler and a timer in `server`. The ID was
336 in the first process and 332 or 336 after later restarts, the same for every call
within one process.

### E4: 50 reloads and a clean exit

`e04-reloads.lua` opened the agent in `server`, then reloaded the mission 50 times with
`net.load_mission`. After each reload the server state returned the same agent table,
the same open count (4: hook, server, gui and export), `disabled` false, and a thread
ID. Each load took 1.96–2.21 s (median 2.08 s) on the shell mission.

The clean exit: `DCS.exitProcess()` in the hook state did nothing on the dedicated
server; the same process kept running. Called in the `gui` state through
`net.dostring_in`, it ended the process (`alone/e04-exit.lua`). The previous run's log
ends with the terrain shutdown and `=== Log closed.`, no `.crash` file was written, and
the agent wrote no crash record. The image's AUTOSTART started DCS again.

### E5: getters driven from C

2,000 idle M-113s, best of seven passes, milliseconds per pass:

| Getter | Lua loop alone | Lua loop into `sink` | `csweep` from C | Speed-up |
|---|---|---|---|---|
| `DCS.getUnitPositionByObjectId` | 0.344 | 0.429 | 0.384 | 1.12 |
| `Unit:getLife` | 0.296 | 0.382 | 0.318 | 1.20 |
| `Unit:getFuel` | 0.301 | 0.386 | 0.324 | 1.19 |
| `Unit:getPoint` | 3.96 | 4.31 | 4.44 | 0.97 |

The rule needs 2x; the best is 1.2x. The sampling shape stays the measured one: a Lua
loop calls each getter and passes the scalars to a C sink. The sink costs about 40 ns a
row.

### E10: cadence

| Item | Value |
|---|---|
| Frame interval, running, 1,110 frames | median 8.99 ms, p99 9.98 ms, max 12.3 ms |
| Frame interval, paused, 552 frames | median 9.12 ms, p99 9.68 ms, max 9.88 ms |
| Model time per real time, running | 1.00002 |
| A timer that reschedules itself 1 ms ahead | 3,000 calls in 3 s of model time, every interval exactly 1 ms of model time; in real time a median of 6 µs and a maximum of 9.9 ms apart |
| Time acceleration | No setter in the hook or server states; `Export.LoGetModelTimeAcceleration()` returns 1 |

The timer catches up within a frame: DCS runs every 1 ms step that a frame covers, one
after another. Time acceleration cannot be set on the dedicated server from these
states, so it is not tested further.

### E13: 2,000 units while paused

200 groups of 10 M-113s, spawned with the simulation paused in one cross-state call:
836 ms in all, a median of 4.1 ms and a maximum of 19.3 ms a group; the frame took
844 ms. Every unit existed afterwards, the simulation stayed paused and model time did
not move. After a resume all 200 groups were gone 1.0 s after their destroy.

### E20: dead and removed objects

One unit was removed with `Group:destroy()`, one was killed with an explosion. After 5 s:

| Call on the old object | Removed | Killed |
|---|---|---|
| `isExist()` | false | false |
| `getLife()` | raises "Unit doesn't exist" | 0 |
| `getPoint()` | raises "Object doesn't exist" | a point (the wreck) |
| `getName()` | the old name | the old name |
| `DCS.getUnitPositionByObjectId(old id)` | 0, 0, 0 | the wreck's position |
| `Unit.getByName(old name)` | nil | nil |

Then 500 units were spawned and the removed group added again under its old name. No
new unit took an old object ID: IDs only grew (old 17801985 and 17802497; new
17803009 to 17931009). The re-added unit got a new ID; the old object still reads as
not existing, and `Unit.getByName` finds the new unit.

The validity rule is tightened: a position read cannot detect a death, because a dead
unit's ID keeps returning the wreck's position. A row leaves the table on a death or
removal event, or when the full enumeration does not find it. `0, 0, 0` from the
position call marks a removed object. Object IDs were not reused in this process, so
the generation check guards against a rule that may change, not one seen.

### E21: name collisions

| Case | Result |
|---|---|
| A second group with a unit named like a unit of the first | Accepted. The first group loses that unit (2 to 1); the name now finds the new unit. |
| A static named like a group | Both exist |
| A group named like a static | Both exist |
| A unit named like its own group | Both exist |
| A unit named like a static | Accepted; the static is gone |
| A 207-character group name; a name with `~!@#$%^&*()` | Both accepted and found by name |

Unit names and static names are one namespace: a new object takes the name, and the old
holder is removed without an event. Group names are a separate namespace. S5 fixes the
naming rule from this: every platform name is unique across units and statics, which
an encoding of the `EntityId` with a kind prefix gives. Where names show to players
needs a client (queued).

### E23: the tainted-files list

With the DLL open in the hook and server states, `DCS.getTaintedFiles()` returned nil,
as it did with the hooks and no DLL in S3, and `DCS.getTaintedCategories()` returned all
four categories false. What a connecting client sees is not covered (queued).

### E24: a mapped file across the Wine boundary

`cargo xtask mapped <target>`: the agent mapped `Z:\dev\shm\nanoprobe-e24.map`, and
`tools/mapped-peer.py` mapped `/dev/shm/nanoprobe-e24.map` in the container with
Python's `mmap`. Each frame the agent wrote a counter into eight words under a sequence
lock and read back the peer's echo. Two runs, 15 s of agent frames each:

| Item | Run 1 | Run 2 |
|---|---|---|
| Peer reads | 32,121 | 32,305 |
| Torn reads (payload words disagree under an unchanged even sequence) | 0 | 0 |
| Reads retried during a write | 16 | 18 |
| Agent values the peer saw | 1,665 of 1,669 | 1,666 of 1,670 |
| Echo lag seen by the agent | exactly 1 frame, every time | same |

The pages are shared both ways and whole words are seen whole. The status block works
under Wine. `/dev/shm` in the game container stands in for the memory-backed volume a
sibling container would share; the mechanism is the same.

### E25: socket cost per frame

The agent held a non-blocking loopback TCP pair (`sock_open`) and made one `send` and
one `recv` each frame for 600 frames per size. Microseconds:

| Bytes | send p50 | send p99 | recv p50 | recv p99 | frame p50 | frame p99 |
|---|---|---|---|---|---|---|
| 0 (idle: recv would block) | 221 | 341 | 141 | 322 | 377 | 646 |
| 64 | 276 | 379 | 217 | 353 | 479 | 718 |
| 512 | 283 | 381 | 205 | 353 | 481 | 717 |
| 4,096 | 228 | 391 | 157 | 350 | 403 | 717 |
| 16,384 | 290 | 386 | 216 | 351 | 486 | 719 |

The cost is per call, not per byte. One call costs 150–290 µs under this Wine. That is
far over E19's 10 µs, and one send and one recv per frame alone exceed the agent's
whole per-frame budget (plan 6: p99 250 µs). At about 111 frames a second it would be
about 4–5% of real time against a budget of 0.3%. **This misses a budget target and
changes the transport design; it is raised with the owner.** Why a call costs this much
under Wine is not established; it is UNVERIFIED that the cost is Wine's server round
trip for socket calls.

### E26: a Lua error through the entry shape

In each of the hook and server states: 1,000 `pcall(agent.raise, …)` caught with the
right message; 1,000 errors raised inside the agent's own `lua_pcall` returned as
`false, message`; a raise nested inside the agent's `lua_pcall` caught; the agent still
enabled and answering afterwards. No crash. `tools/raiseshape.sh` finds no destructor
in any of the 21 entries.

### E27: the static-CRT DLL in the real prefix

The hook opened the DLL at start in every process this session (hook.log "agent
loaded"), pinned. It opened in `server`, `gui` and `export` as well. The DLL imports
only `kernel32`, `ntdll`, `ws2_32`, one `api-ms-win-core-synch` set and `lua.dll`.

### E29: an empty mission without a person

On the dedicated server `me_mission` is not a global in the `gui` or hook state.
`require('me_mission')` fails in both: in `gui` inside `me_draw_panel.lua`, in the hook
state on a missing `DcsWeb` module. The failed require left Mission Editor globals in
both states until the next restart. The Mission Editor route does not work on the
server, so by the rule the one-time manual save stays; the two installed maps already
have templates. Writing a shell from the documented key list was not tried.

### E33: the clock

`QueryPerformanceCounter` at 10 MHz: 73 ns a read in a tight loop, in both states; 105 ns
through the `clock` entry from Lua. Under 1 µs. With a getter at about 160 ns a row, a
clock read every 32 rows costs about 1.4% of a sweep, under the 2% of plan 6.

### Crash capture

**Panic** (`e90-panic.lua`, server state): the entry caught it and returned nothing; the
agent disabled itself; `status().reason` read
`panic at crates/agent/src/entries.rs:311: deliberate panic for the crash capture test`.
The bundle's `logs/agent.log` holds a `panic` record with the message, location and
breadcrumb 90, then `disabled after a panic`.

**Access violation** (`alone/e91-fault.lua`, server state): the handler appended one
record to `logs/crash.log`: code `0xC0000005`, `in_agent` true, depth 1, the faulting
offset, the breadcrumb, the PDB signature, and 48 frames. `cargo xtask symbolize` on the
bundle resolved them with the kept PDB to `work_fault` (`crates/agent/src/entries.rs:318`),
through `guarded` and the `fault` entry, into `lua.dll`, `edCore.dll` and
`DCS_server.exe`. DCS's own handler ran after ours: it logged the modules, including
the agent DLL, wrote a `.crash` and a `.dmp`, and began packing them into
`Logs/dcs.log-<time>.zip`.

**Then DCS hung.** The log stopped at that point, the zip stayed incomplete, and the
process stayed alive at about 18% CPU. Five minutes later it had not exited, and
`cargo xtask restart` restarted it. A crash under Wine can therefore leave a live,
hung process. The supervisor (S19) must treat a stalled heartbeat as a crash, not wait
for the process to exit, and `dcs-watchdog` will not see it while the port stays open.

## Other findings

- DCS's `io.open` cannot open `hook.log` for reading in the hook state while the hook
  holds it open for appending ("can't open … for 'rb'"). A reader inside DCS must not
  rely on reading a file the platform is writing.
- The DLL carries the build machine's rust-src paths (the home folder) in panic
  location strings from the standard library. `trim-paths` is not stable in this
  toolchain. Release builds need `--remap-path-prefix` (S22).

## What the Windows checkpoint must do

Windows runs are queued, as the owner decided (2026-11-10). At the first checkpoint, on
the Windows client hosting a session:

- E4, E26, E27, E33: deploy, then `cargo xtask probes <local target> --dir
  lua/experiments --only e26` (and `e27`, `e33`, `e04`), and the clean exit.
- E25 on Windows: `--only e25`. Its numbers decide whether the socket cost is Wine's.
- E3, E5, E10, E13, E20, E21 again, for the client's values.
- Crash capture: `e90-panic.lua`, then `alone/e91-fault.lua`; check whether DCS on
  Windows exits or hangs after its crash report.

Needing a person or the Windows PC over SSH:

- E18: with one remote client connected, a burst spawn, the damage setter, menus and
  marks, and a reload; whether the client stays connected and sees each.
- E21's second half: where unit, group and static names show to a player.
- E23's second half: whether a connecting client sees anything different with the DLL
  loaded.
- E30: a scripted client through `net.start_client` and `net.set_slot`.
