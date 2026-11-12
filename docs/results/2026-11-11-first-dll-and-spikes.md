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
| Result | Every experiment that needs no client has a result. E25 misses the budget: see "E25" and the summary. E18, E21 and E30 were run with the owner as the player. |

Every value is LIVE on this build and platform unless a line says otherwise. No player
was connected at any restart or during any run, except in E18. The probe suite and every
experiment also ran on the Windows client ("On the Windows client").

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
| E2 | Bootstrap works with the keys set and the sandbox call fails | Works; sandbox refused | PASS (Wine and Windows) |
| E3 | One thread | One thread for every kind of call | PASS |
| E4 | 50 reloads and a clean exit | 50 of 50; clean exit through the gui state | PASS (Wine) |
| E5 | Adopt C-driven only if at least 2x faster | At most 1.2x | Lua loop with a C sink stays |
| E10 | Recorded | About 111 frames a second, running or paused; no acceleration setter | Recorded |
| E13 | Completes; time recorded | 2,000 units in 836 ms, one frame | PASS |
| E18 | Client stays connected and sees the result, or each failure is recorded | Connected throughout; burst, menu, mark and reload seen; the damage setter desynced the client | **One failure recorded** |
| E20 | Behaviour recorded; validity rule confirmed or tightened | IDs not reused; a dead unit's ID still reads a position | Rule tightened |
| E21 | Naming rule fixed in S5 | Unit and static names share one namespace; group names are apart; players see names only in the unit dialog | Recorded |
| E23 | Recorded; a change is raised | `getTaintedFiles()` still nil | No change |
| E24 | Status block works, or the stream fallback | Same pages, no torn read | PASS |
| E25 | Feeds E19 | Wine: about 380 µs per frame at p50, 720 µs at p99. Windows: 20 µs and 30 µs | **Misses the budget**; over 10 µs on both |
| E26 | Caught by `lua_pcall`, no crash | 2,000 of 2,000 in each state | PASS (Wine; Windows, hook state) |
| E27 | Loads; otherwise the dynamic CRT | Loads, pinned | PASS (Wine and Windows) |
| E29 | A generated template loads and offers slots | The Mission Editor modules do not load on the server | Manual save stays |
| E30 | A scripted client joins, takes a slot, and reports a menu item and a mark | Joins, takes a slot and spawns; the client's own screens do not follow; reading menus and marks not found | Partly |
| E33 | Recorded; above 1 µs is raised | 73 ns under Wine, 15.5 ns on Windows | PASS |
| Crash capture | A panic and an access violation each leave a diagnosable bundle | Both do; DCS then waits in its crash-report window instead of exiting | PASS (Wine and Windows) |

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
four categories false.

Integrity checks look only at the DCS installation; files in the Saved Games profile do
not trigger them (PRIOR: the owner's own repeated testing). The agent and every file
the platform installs live in the profile, so a connecting client sees no difference.

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

### E18 and E21: a player on the server

Run 2026-11-12 (the reload at 00:11 UTC) with the owner as the player, on the
Windows client joined to the Wine server, which ran the owner's own mission with 14
fixed blue slots. `lua/experiments/alone/e18-actions.lua` and `e18-reload.lua` ran on
the server; the owner reported what the client showed. The run used a form of
`e18-actions.lua` that named the player's position, and of `e30-take-slot.lua` that named
the slot; the committed forms find both at run time, so they work with any mission that
has player slots, and have not been run since. Server values are LIVE; client
values are the owner's observations.

| Action on the server | Server | Client |
|---|---|---|
| Burst: 20 groups of 10 M-113 in one call, 1.5 km from the player | 109 ms | All shown; the player stayed connected. A stutter was not watched for. |
| `trigger.action.outText` for 60 s | sent | Not watched for |
| `trigger.action.markToAll` | added | Shown with its text |
| `missionCommands.addCommandForCoalition` (blue) | added | Shown under F10; selected once, and the server's callback ran once |
| `a_unit_set_life_percentage(id, 10)` on `np-e18-1` | The unit died: its group went from 10 to 9, and its three nearest neighbours lost 1–7% of their life, as from a blast | **`np-e18-1` still shown intact** (F11 view), its group with 10 units, its task blank where the others read "Not active". The neighbours' damage was shown. |
| `net.load_mission` of the running mission | Reloaded in 6.6 s; the hook saw `onPlayerStop` for the server's player and the client | Stayed connected; returned to the coalition screen |

The damage setter is the one failure. Setting a ground unit to 10% killed it on the
server, and the death never reached the client, which kept a stale unit. Whether a
higher value avoids the death, and whether a death from weapons replicates, is not
known; S15's E16 must cover damage with a client connected and check what the client
shows.

E21's second half: on the F10 map the player sees only the type ("M-113"). Clicking a
unit opens a dialog with its unit and group names, so platform names are visible to a
player who looks. The naming rule must not put anything in a name that players should
not see.

The test units were removed by the reload, and the eval reset reported no errors.

### E30: a client driven from Lua

Run on the Windows PC on 2026-11-11, by the owner with a Claude Code session there, not
through this repository's tooling. DCS 2.9.31.29434, Windows client; LIVE there, PRIOR
here. With the client at its main menu, this in the client's `gui` state started a
connection to the owner's server, and the owner saw the client join:

```lua
net.set_name('<player name>')
return tostring(net.start_client('<server address>:<port>', '<server password>'))
```

It returned `"0"`. The call matches how ED's own server browser joins
(`MissionEditor/modules/mul_server_list.lua`). `net.start_client` and `net.set_name`
also exist in the hook state.

Taking a slot (LIVE, same session as E18, the owner joined by hand):
`lua/experiments/alone/e30-slots.lua` listed the slots with `DCS.getAvailableSlots`, and
`e30-take-slot.lua` called `net.set_coalition(2, '')` then `net.set_slot(2, '8570', '',
nil)`, as ED's role screen does. Both ran in the hook state; the `gui` state did not
answer `net.dostring_in` while the client was connected. `net.get_slot` and
`DCS.getPlayerUnit()` then reported the slot. The client's screen stayed on the
coalition step, because ED's screens keep their own state and the calls bypass them.
`net.spawn_player()`, which ED's "Fly" path calls, put the player in the cockpit.
`net.stop_game()` in the hook state left the server.

Reading the client's own view of a menu item or a mark from Lua was not found: the hook
state lists the mark event IDs but no call that returns marks or menu items. E30's rule
is therefore met for joining and taking a slot, not for reporting what the client sees;
player-facing checks stay manual.

How the game starts unattended, from the same session: the Start-menu shortcut runs
`bin\DCS_updater.exe`, which runs `bin\DCS.exe` with no arguments, which is ED's
launcher window; its Play button starts `bin\DCS.exe --restarted` and the launcher
exits. Starting `bin\DCS.exe --restarted` directly skips the launcher: a short login
window, then the main menu after about 30 s. Closing the game's window raises a
confirmation dialog, so an unattended stop that cannot reach Lua must end the process.

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

**Then DCS stopped.** The log stopped at that point, the zip stayed incomplete, and the
process stayed alive at about 18% CPU. Five minutes later it had not exited, and
`cargo xtask restart` restarted it. The likely cause is ED's crash-report window
waiting for input (see "On the Windows client"). A host check that only watches the
process or the game port does not see this, because both stay up; the heartbeat
stops.

## On the Windows client

| | |
|---|---|
| Date | 2026-11-11 23:38 UTC to 2026-11-12 00:41 UTC |
| Platform | Windows 10.0.26200, DCS 2.9.31.29434 client, at the main menu, no mission loaded, about 60 frames a second |
| Scripts | `cargo xtask deploy <target> --dev`, `cargo xtask restart <target>` (the windows-ssh target: DCS started by a scheduled task in the desktop session), then `cargo xtask eval <target> hook lua/experiments/<name>.lua`; `cargo xtask undeploy <target>` afterwards |
| Agent | build `843dc82a51e097a1`, PDB `FAC1930B6DBAE4774C4C44205044422E1` |

DCS started through the task, and the hook ran 36 s later. At the main menu the client
has no mission and no `server` state, so the first runs used the hook state.

**Hosting a session from Lua.** `lua/experiments/alone/host-session.lua`, run in the
`gui` state, does what ED's own multiplayer screen does
(`MissionEditor/modules/mul_create_server.lua`). It starts from
`net.get_default_server_settings()`, never reading the profile's `serverSettings.lua`.
It sets a private session on port 10318 (away from a real server's 10308), with a
random unrecorded password and the shell mission in `missionList`, and calls
`net.start_server`. That returned `0`. The client loaded `np-shell` 13 s later as a
multiplayer server (`DCS.isServer()` true), paused at model time 0, and its `server`
state answered.

ED's defaults on this build: `port` 10308 (a number), `isPublic` true, `maxPlayers` 16,
`name` "DCS Server", `mode` 0, `require_pure_clients`, `_models` and `_textures` true,
`require_pure_scripts` false, an empty `advanced` table, no password.

At the main menu the eval hook once took several minutes to take a request, and another
time answered within seconds. Why is not established.

- **E27:** the hook opened the DLL at start, pinned, ABI 1, and the module name matched
  the build; it opened in the hosted session's `server` state too. PASS.
- **E26:** in the hook state and in the hosted session's `server` state, 1,000 raises
  through the entry and 1,000 errors inside the agent's `lua_pcall` all caught, the
  nested case caught, the agent still enabled. PASS.
- **E4, the clean exit:** with the agent open in the hook and `server` states and the
  session running, `DCS.exitProcess()` in the `gui` state ended DCS. The log ends with
  `=== Log closed.` and no `.crash` file was written. The 50 reloads were not run here.
- **E33:** 15.5 ns a counter read in a tight loop, 29 ns through the `clock` entry.
- **E25,** microseconds per frame, 600 frames a size:

| Bytes | send p50 | send p99 | recv p50 | recv p99 | frame p50 | frame p99 |
|---|---|---|---|---|---|---|
| 0 (idle: recv would block) | 2.5 | 3.4 | 1.2 | 1.6 | 3.8 | 4.8 |
| 64 | 17.1 | 22.2 | 1.7 | 2.1 | 18.8 | 24.3 |
| 512 | 17.4 | 24.1 | 1.6 | 2.1 | 19.0 | 26.6 |
| 4,096 | 18.9 | 26.3 | 2.7 | 3.6 | 21.6 | 29.4 |
| 16,384 | 19.7 | 26.9 | 3.0 | 3.8 | 22.6 | 30.3 |

A socket call on Windows costs about a twentieth of one under Wine, so most of the
Wine cost is Wine's. It is still over E19's 10 µs at p99 whenever data is sent, so the
mapped file stays the data path on both platforms (plan revision 7). The client's
numbers come from a different machine than the server's.

**The probe suite** (`cargo xtask probes <target>`), in the hosted session: all 25
probes ran, nothing was left behind, and the only differences from the recorded client
baseline are three the Wine server showed too. The hook state is not the `gui` state
(`hook_is_gui` false), a reload sends `onPlayerStop`, and the scenario's aircraft
reports `hasTask` true. So none of the three is a Wine or dedicated-server trait, and
the S3 classifications stand; `hasTask` comes from the scenario.

**The other experiments,** in the hosted session, each through `cargo xtask probes
<target> --dir lua/experiments --only <name>`:

| # | Windows client | Wine server, above |
|---|---|---|
| E3 | One thread for every kind of call | Same |
| E4 | 50 of 50 reloads, the same table and open count each time; 5.40–6.04 s a load | Same; 1.96–2.21 s |
| E5 | Best speed-up 1.14x (`byObjectId` 0.235 ms C-driven against 0.269 ms Lua into `sink`) | 1.20x |
| E10 | Frames 16.95 ms median running and paused (60 a second, vsync); the 1 ms timer catches up within a frame | About 9 ms; same timer |
| E13 | 2,000 units in 857 ms, one frame; still paused; removed 0.76 s after a resume | 836 ms |
| E20 | The same behaviour; IDs only grew | Same |
| E21 | The same collisions | Same |
| E24 | `cargo xtask mapped <target>` with `tools/mapped-peer.ps1`, both sides mapping a file in the profile: 0 torn reads in 1,290; echo lag 1 frame (2 once in 887) | Same |
| E90 | The panic caught, the agent disabled, the same reason | Same |
| E2 | `cargo xtask gates <target> on` (the client's `autoexec.cfg` saved byte for byte first), then a restart and a hosted session: the hook reached `server`, `gui`, `mission`, `export` and `config`; `scripting` and the sandbox's call were refused; the agent loaded at start. `gates off` put the saved copy back, and its SHA-256 matched the one taken before. | Same |
| E91 | The handler wrote one record: `0xC0000005` in the agent, depth 1, breadcrumb 91. `cargo xtask symbolize` resolved it to `fault` (`crates/agent/src/platform/windows.rs`) through `guarded`. DCS's own handler then wrote and removed a `.crash` and a `.dmp`, left `Logs/dcs.log-<time>.zip` at 0 bytes, and the process hung until the stop task ended it. | Same, hang included |

The profile was undeployed afterwards and DCS stopped; the profile's tree was as before.

On both platforms DCS stopped after its own crash handler ran. On the Windows client
that is ED's crash-report window, which waits for the user to describe the crash and
send it (PRIOR: the owner's experience of DCS crashes without the agent). The Wine
server's container runs a desktop, so the same window may be waiting there
(UNVERIFIED: the desktop was not viewed). Either way the sidecar's health verdict
(S19) must report a process whose heartbeat stopped as hung, so the host's tooling can
end and restart it, and E22 must find a setting that turns the window off or confirm
that the host ending the process is the answer.

## Other findings

- DCS's `io.open` cannot open `hook.log` for reading in the hook state while the hook
  holds it open for appending ("can't open … for 'rb'"). A reader inside DCS must not
  rely on reading a file the platform is writing.
- The DLL carries the build machine's rust-src paths (the home folder) in panic
  location strings from the standard library. `trim-paths` is not stable in this
  toolchain. Release builds need `--remap-path-prefix` (S22).

## What the Windows checkpoint must do

Nothing from S2, S3 or S4 remains for it. Open, and needing a person:

- E30: a way for the client to read its own menu items and marks, if one exists.
