# Probe suite on a dedicated server

| | |
|---|---|
| Step | S2 exit test: the suite runs in full on the Wine server and leaves nothing behind after reset |
| Date | 2026-11-10 (03:05–03:30 UTC on 2026-11-11) |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Scripts | `cargo xtask probes <target>` (`crates/xtask/src/probes.rs`); the 23 probes in `lua/probes/`; `lua/dev/eval-hook.lua`, `lua/dev/eval-prelude.lua`, `lua/scenarios/scenario.lua`, `lua/dev/lib/recorder.lua` |
| Mission | A shell generated from `templates/caucasus.miz`, no client connected |
| Result | PASS on the second and third runs |

No player was connected at any restart or during any run. Everything below is LIVE on
this build and platform unless a line says otherwise. "Client" means the recorded
baseline: DCS 2.9.31.29434 on Windows, a client hosting a multiplayer session and
rendering at 60 fps. The baseline values are PRIOR here: they are written into the
probes as expectations and were not measured again.

## Runs

| Run | Probe runs | Failed | Left behind | Wall time |
|---|---|---|---|---|
| 1 | 31 | 5 | one cleanup error in `server` | 520 s |
| 2 | 31 | 0 | nothing | 98 s |
| 3 | 31 | 0 | nothing | 183 s |

A probe run is one probe in one state. Run 1 found four faults, all in the tooling, and
each is fixed:

- `Unit:getID()` returned a string, and a probe compared it with a number.
- The simulation paused in the middle of the run, and four probes that waited on model
  time timed out. See "Pause" below.
- `timer.removeFunction` raised `Parameter #1 (function reference number) is invalid`
  for a timer that had already run, so the cleanup reported an error.
- One probe waited for a group that an earlier, failed probe had left.

Run 3 observed 469 values: 266 matched their expectation, 202 were recorded with no
expectation, and one timing was out of range (`getVelocity` over 1,000 units, below).

## Nothing left behind

After run 3:

- `uninstall` ran every cleanup in every state and reported no error, no new global in
  any of the seven states, and no change in the number of event handlers in `server` or
  the sandbox. In `server` it removed 3 globals, 2 event handlers, 3 timers and 2
  deferred cleanups.
- A count in the `server` state found 0 groups, 0 units and 0 static objects in the
  mission, and no entry in `world.eventHandlers`.
- `cargo xtask undeploy` removed every platform file. `cargo xtask snapshot`, taken
  before the first deploy and after the undeploy and a restart, differs in two places,
  both written by DCS itself: six new track files in `Tracks/Multiplayer/`, one per
  mission start, and `Config/options.lua`, which DCS writes again at every start.
  `options.lua` was 4,186 bytes before and 3,999 bytes after. Its contents were not
  read, so what the 187 bytes were is UNVERIFIED.

What stays in the DCS process until it exits: the callback table the eval hook gave to
`DCS.setUserCallbacks`, which does nothing once the kill switch is set or the files are
gone.

## Where the server differs from the client

| Observation | Client | Wine server |
|---|---|---|
| A hook's globals are the `gui` state's | yes | no: another table; a global set in one is absent in the other |
| Callbacks on a mission reload | `onSimulationStop`, `onNetMissionChanged`, `onMissionLoadBegin`, `onPlayerStart`, `onMissionLoadEnd`, `onSimulationStart` | the same, with `onPlayerStop` after `onSimulationStop` |
| `package.loadlib` on a missing file | "The specified module could not be found" | "Module not found." |
| `Controller:hasTask()` for an aircraft with a three-point route | false | true. The scenario gives the group the task `CAP`; whether the task or the platform decides it is UNVERIFIED |

Recorded on the server with no client value to compare:

- `Unit:getID()` returns a string for a unit added at run time, and `initiator_data.mission_id`
  in an event is the same string. `Unit:getObjectID()` returns a number.
- `DCS.getUnitPositionByObjectId` returns three values for a static object's ID, the
  first of them 0.
- `net.load_mission` returned one value, 0. The mission started paused.
- `a_do_script('return "x"')` returned one value, nil: a lone value is lost.
- `DCS.getTaintedFiles()` returned nil with the hooks loaded.
- The event handler that survived the reload saw events 52, 12, 11, 46, 58 and 57.
- An explosion on a lone M-113 gave `S_EVENT_UNIT_LOST` and then `S_EVENT_DEAD`, and no
  `S_EVENT_HIT`.
- Ground units added beside an airbase captured it: `dcs.log` showed a `base captured`
  event with an M-113 as its initiator (LOG).

## What matched the client

States and gates: `net.dostring_in` reached `gui`, `server`, `scripting`, `mission`,
`export` and `config` with neither gate key set, and an unknown name returned nothing.
`server` and `scripting` are one state. The sandbox reached the `server` state through
`net.dostring_in`. `DCS` and `Sim` are one table. `package.loadlib` is absent only in
`mission` and the sandbox.

Pause: while paused, `onSimulationFrame` kept arriving, model time stood still and a
timer did not run. `DCS.setPause(false)` took effect on a later frame, 56 to 58 frames
after the call.

Reload: no frame during the load, no `onMissionLoadProgress`, the theatre readable at
`onMissionLoadBegin` and nil at `onSimulationStop`. The `server` and `mission` states
kept their globals; the sandbox and the `export` state were made again. The handler saw
the mission end (12) and start (11); the timer scheduled before the reload did not run.

Timers and events: a timer that asked for 0.1 ms ran once per millisecond of model time
(2,004 ticks in 2 s). Four spawned units gave four `S_EVENT_BIRTH`, each with
`initiator_data` holding `coalition`, `mission_id`, `object_category`, `object_id`,
`unit_name` and `unit_type`. `Group:destroy()` left the group existing within the call
and emitted no event.

Objects: an M-113 has life 3, fuel 1 and ammunition counts 104 and 404. A convoy that
was destroyed and added again in one call kept its mission IDs, got new object IDs, and
moved on along its route. An F-15C spawned with 3,000 kg of fuel and 50 % gun read
fuel 0.4914 and 470 rounds; added again with half of each, it flew 3,291 m in 15 s and
read fuel 0.2441 and 235 rounds. `a_unit_set_life_percentage(id, 40)` in the `mission`
state changed `getLife()` from 3 to 1.2. `Airbase:autoCapture(false)` and
`setCoalition` changed an airbase's coalition. `terrain.convertMetersToLatLon` in the
hook state and `coord.LOtoLL` in the `server` state agreed to 6.4e-9 degrees at three
points. `a_do_script('return 1, 2, 3')` returned nil, 1, 2.

## Pause

A dedicated server with no client starts its mission paused. `DCS.setPause(false)`
resumed it. It then paused again by itself: `dcs.log` showed `adding client[2]`,
`disconnecting client[2]`, `remove: client[2]` and `simulation paused` within 20 ms,
once every 60 s. Something opened the game port and closed it, DCS counted it as a
client, and DCS pauses when its last client leaves (LOG). A TCP check from a monitoring
tool is enough to do this.

The eval hook therefore resumes the simulation for a probe that waits on model time
(`E.wait(fn, seconds, 'running')`). It resumed 4 times in run 2 and 5 times in run 3.

## Timings

Nanoseconds per call, best of three rounds, one M-113 in the mission. The server was
resumed and ran no other load.

| Call | Client | Server, run 2 | Server, run 3 |
|---|---|---|---|
| `net.dostring_in('server', 'return 1')` | 2,000 | 5,474 | 4,990 |
| `net.dostring_in('server', 'return tostring(timer.getTime())')` | 3,000 | 7,220 | 6,628 |
| `DCS.getModelTime` (hook) | 30 | 20 | 20 |
| `DCS.getRealTime` (hook) | not recorded | 98 | 97 |
| `timer.getTime` | 40 | 21 | 34 |
| `Unit:getLife` | 85 | 130 | 170 |
| `Unit:getFuel` | 85 | 143 | 147 |
| `Unit:getName` | 85 | 157 | 149 |
| `Unit:isExist` | 135 | 236 | 231 |
| `Unit:inAir` | 135 | 229 | 228 |
| `Unit:getPoint` | 800 | 1,935 | 1,953 |
| `Unit:getVelocity` | 760 | 1,884 | 2,014 |
| `Unit:getPosition` | 3,300 | 8,885 | 8,302 |
| `DCS.getUnitPositionByObjectId` | 60 | 149 | 149 |
| `Export.LoGetObjectById` | 9,700 | 20,126 | 19,921 |
| `Export.LoGetWorldObjects`, one unit | 7,000 | 20,439 | 22,773 |

Milliseconds for one pass over every unit, best of five passes, M-113s in groups of 10.
`addGroup` is the median for one group of 10. The server columns are run 2 / run 3.

| Measurement | 500: client | server | 1,000: client | server | 2,000: client | server |
|---|---|---|---|---|---|---|
| `coalition.addGroup` | 3.80 | 3.52 / 3.57 | 5.00 | 4.34 / 4.44 | 5.30 | 5.88 / 5.37 |
| enumerate groups and units | 0.19 | 0.58 / 0.63 | 0.44 | 1.18 / 1.43 | 1.00 | 3.14 / 2.88 |
| `DCS.getUnitPositionByObjectId` | 0.046 | 0.083 / 0.084 | 0.106 | 0.171 / 0.175 | 0.316 | 0.392 / 0.756 |
| `Unit:getPoint` | 0.35 | 0.97 / 1.05 | 0.77 | 1.94 / 3.19 | 1.55 | 4.84 / 4.73 |
| `Unit:getVelocity` | 0.33 | 0.93 / 1.00 | 0.72 | 1.87 / 28.9 | 1.47 | 4.41 / 4.03 |
| `Unit:getPosition` | 1.31 | 4.09 / 4.49 | 2.88 | 8.29 / 8.75 | 5.86 | 19.4 / 17.2 |
| `Unit:getLife` | 0.046 | 0.081 / 0.081 | 0.10 | 0.167 / 0.168 | 0.20 | 0.338 / 0.341 |
| `Unit:getFuel` | 0.047 | 0.082 / 0.083 | 0.14 | 0.171 / 0.172 | 0.21 | 0.345 / 0.350 |
| `Unit:isExist` | 0.075 | 0.136 / 0.137 | 0.16 | 0.276 / 0.279 | 0.32 | 0.564 / 0.568 |
| `Unit:getName` | 0.053 | 0.100 / 0.101 | 0.17 | 0.207 / 0.229 | 0.31 | 0.423 / 0.425 |
| `Unit:getAmmo` | 0.78 | 2.13 / 2.31 | 1.92 | 4.33 / 4.34 | 3.45 | 10.6 / 10.0 |
| `Export.LoGetWorldObjects` | 3.10 | 9.84 / 11.1 | 7.60 | 20.2 / 19.8 | 14.1 | 40.3 / 42.7 |
| `world.searchObjects`, sphere | 0.16 | 0.52 / 0.55 | 0.35 | 1.16 / 1.03 | 0.71 | 2.05 / 2.20 |

- The cheap getters cost 1.5 to 2 times the client's figure, and the ones that build
  tables 2.5 to 3 times.
- The client figures and these runs come from different machines. Each ratio mixes the
  hardware with Wine and with the dedicated server, and these runs do not separate
  them. Slower hardware alone could account for the ratios (UNVERIFIED).
- Run 3's `getVelocity` pass over 1,000 units took 28.9 ms in every one of its five
  passes, 15 times run 2's figure. The cause is not known, and no other sweep in either
  run shows it.
- `Group:destroy()` returned in 2.5 and 2.9 µs per group (client: 0.5 µs). All 200
  groups were gone 0.52 s after the calls.
- One airborne F-15C took 18.2 ms and 7.4 ms to add (client: 6.6 ms). One static
  object took 0.93 ms and 0.87 ms at the median of 100 (client: 0.12 ms, another type).
- `os.clock` steps in 1 ms and `DCS.getRealTime` in 200 ns (client: 1 ms and about
  100 ns).
- The `server` state's Lua heap read 140 MB and 198 MB with 500 units, and 100 MB and
  158 MB with 2,000, in the two runs. A collection ran in between.

## Not covered

- The Windows client. The suite runs against a local profile with the same commands;
  that run is queued for the first Windows checkpoint.
- Whether `net.load_mission` does nothing on a single-player host. It needs a client.
- Deaths caused by a unit, and the events they emit. The scenario library has no
  engagement scenario.
- The pause matrix and the classification of each difference above. Both belong to the
  next step.
