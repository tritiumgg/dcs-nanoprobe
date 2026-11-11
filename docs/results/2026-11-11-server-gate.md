# The server gate: dedicated server under Wine against the Windows client

| | |
|---|---|
| Step | S3 gate, experiments E1 and E17 |
| Date | 2026-11-11 (07:40–08:00 UTC) |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Scripts | `cargo xtask run <target> suite` and `cargo xtask run <target> pause` (`crates/xtask/src/main.rs`); the probes in `lua/probes/`; the pause matrix `lua/dev/lib/pause.lua` with `01-pause-start.lua`, `80-pause-mid.lua`, `81-pause-idle.lua` |
| Mission | A shell generated from `templates/caucasus.miz`, loaded fresh by the runner; no client connected |
| Result | PASS. E17: pump candidate A works in all three pause conditions. E1: every difference from the client is classified below. |

"Client" means the recorded baseline: DCS 2.9.31.29434 on Windows, a client hosting a
multiplayer session and rendering at a vsync-locked 60 fps. Its values are PRIOR here.
Every server value is LIVE on this build and platform unless a line says otherwise.

## Runs

The runner deployed the development build, restarted DCS, waited for the hook and the
mission, loaded a fresh shell mission, ran the probes, cleaned up and collected a bundle.

| Run | Test | Probe runs | Failed | Left behind |
|---|---|---|---|---|
| 1 | `suite` | 33 | 0 | 2 groups, 3 units: the idle matrix's removal was still waiting in the paused mission |
| 2 | `pause` | 3 | 0 | nothing: 0 groups, 0 units, every cleanup ran |

The pause matrix was changed after run 1 to resume until its removal finishes and then
pause again. Run 2 is that version.

## E17: the pause matrix

The matrix drives the `server` state the way candidate A would: the hook's
`onSimulationFrame` makes one `net.dostring_in` call into `server` on every frame. It
holds the pause for 2 s of real time.

| Item | At model time 0 | Mid-mission, `DCS.setPause(true)` | Paused for no clients |
|---|---|---|---|
| Pause held for the whole check | yes | yes | yes |
| Frame callback while paused | 220 frames | 222 | 222 |
| Cross-state call on every frame | 220 of 220 | 222 of 222 | 222 of 222 |
| Model time stood still | yes | yes | yes |
| Timer scheduled 10 ms ahead ran | no | no | no |
| Getters (`getPoint`, `getLife`, `getFuel`, `isExist`, `getVelocity`, `DCS.getUnitPositionByObjectId`) | all returned values | all | all |
| `getUnitPositionByObjectId` agrees with `getPoint` | yes | yes | yes |
| Ground group of 2 M-113 spawned | yes, 6.2 ms | yes, 1.9 ms | yes, 1.4 ms |
| Airborne F-15C spawned | yes, 19.5 ms | yes, 11.8 ms | yes, 9.0 ms |
| The aircraft stayed in place while paused | yes | yes | yes |
| `S_EVENT_BIRTH` reached a handler while paused | 2 | 2 | 2 |
| `Group:destroy()` finished while paused | no | no | no |
| It finished after a resume | 0.51 s later | 0.50 s later | 0.51 s later |

Values are from run 2; run 1 gave the same verdicts.

The idle condition came about as before: DCS paused by itself 36.5 s after the probe
resumed it (58.2 s in run 1). The only hook callback with it was `onSimulationPause`.
No `onPlayerConnect` or `onPlayerDisconnect` arrived.

**Decision: pump candidate A.** The rule was "candidate A works in all three, or B is
chosen and restore redesigned". A worked in all three. B fails it: a timer never ran
while paused.

What this does not cover:

- The median cross-state call in the matrix took 136–139 µs. That is the eval channel's
  own call, which compiles the snippet and frames the reply. A trivial
  `net.dostring_in('server', 'return 1')` costs 5.3 µs (below). Candidate A's real cost
  is measured with the agent in S4 and S7.
- Removal is deferred until the simulation runs. A capture-and-destroy while paused, then
  a re-add under the same name, is untested. S15 and S18 need that case.
- A connected client during the pause is untested (E18 with a remote client).

## E1: differences from the client, classified

Classes:

- **Same**: the server matches the client.
- **Wording**: a text differs; nothing may depend on it.
- **Plan text**: the plan's description changes; the design does not.
- **Design**: the design must take it into account.
- **Unexplained**: the cause is not known; the probe stays and the Windows checkpoint
  re-runs it.
- **Timing**: compared loosely; the baseline is a rendering client on other hardware.

### Behaviour

| Observation | Client | Wine server | Class |
|---|---|---|---|
| A hook's globals are the `gui` state's | yes | no: the hook state is a table of its own; a global set in one is absent in the other | Plan text. The hook state and `gui` are named apart everywhere; the bootstrap loads the DLL in the hook's own state and never reaches it through `net.dostring_in('gui', …)`. |
| Callbacks on a mission reload | `onSimulationStop`, `onNetMissionChanged`, `onMissionLoadBegin`, `onPlayerStart`, `onMissionLoadEnd`, `onSimulationStart` | the same, with `onPlayerStop` after `onSimulationStop` | Design, small. Lifecycle comes from the simulation and load callbacks only; player callbacks never drive it. Why the server sends `onPlayerStop` is UNVERIFIED. |
| `package.loadlib` on a missing file | "The specified module could not be found" | "Module not found." | Wording. The agent reports the text and never matches on it. |
| `Controller:hasTask()` on an aircraft with a three-point route | false (plain route) | true (the scenario's group task is `CAP`) | Unexplained. The scenarios may differ rather than the platforms. The platform never reads tasks back, so no design depends on it. |
| A dedicated server with no client starts its mission paused, and pauses again whenever a connection to the game port opens and closes | not applicable: the client is a player | yes, with `onSimulationPause` and no player callback | Design. Already in the plan: the pump runs while paused (E17), and nothing assumes the simulation runs. |
| `Group:destroy()` while paused | not recorded | the removal waits until the simulation runs | Design. Restore and despawn while paused must expect the group to exist until a resume. |
| `Unit:getID()` for a unit added at run time | a mission ID; its type not recorded | a string; `initiator_data.mission_id` is the same string | Design, small. IDs are read with `tonumber`. |
| Frame rate | 60 fps, vsync | about 110 frames a second, running or paused | Design, small. The per-frame pump runs nearly twice as often; the budget is a share of real time, so it holds. |

Everything else the suite compares matched the client exactly: the states `net.dostring_in`
reaches with neither gate key set and nil for unknown names; `server` and `scripting` as
one state; the sandbox reaching `server`; `DCS` and `Sim` as one table; where
`package.loadlib` exists; no frame and no `onMissionLoadProgress` during a load; the
theatre readable at `onMissionLoadBegin` and nil at `onSimulationStop`; which states keep
their globals over a reload and which are made again; event handlers kept and timers
cleared by a reload; the 1 ms timer quantum; `S_EVENT_BIRTH` with `initiator_data`;
`Group:destroy()` deferred and silent; the M-113, convoy, F-15C, damage, airbase and
static results; `a_do_script` shifting results by one; `DCS.setPause(false)` acting on a
later frame (56 to 58 frames here); and the lat/lon conversion.

Recorded on the server with no client value, and consistent with the client's rules:

- `DCS.getUnitPositionByObjectId` on a static object's ID returns three values, the first
  0. The client recorded 0.
- `a_do_script('return "x"')` returns nil: the lone value is shifted out.
- `DCS.getTaintedFiles()` returns nil with the hooks loaded.
- `net.load_mission` returns one value, 0, and the mission starts paused.

### Timing

Run 1. Nanoseconds per call, best of three, or milliseconds per pass over every unit,
best of five.

| Call | Client | Server | Ratio |
|---|---|---|---|
| `net.dostring_in('server', 'return 1')` | 2,000 ns | 5,273 ns | 2.6 |
| `DCS.getModelTime` | 30 ns | 20 ns | 0.7 |
| `timer.getTime` | 40 ns | 35 ns | 0.9 |
| `Unit:getLife`, `getFuel`, `getName` | 85 ns | 133–155 ns | 1.6–1.8 |
| `Unit:isExist`, `inAir` | 135 ns | 231–232 ns | 1.7 |
| `Unit:getPoint`, `getVelocity` | 760–800 ns | 1,855–1,919 ns | 2.4 |
| `Unit:getPosition` | 3,300 ns | 9,233 ns | 2.8 |
| `DCS.getUnitPositionByObjectId` | 60 ns | 160 ns | 2.7 |
| `Export.LoGetObjectById` | 9,700 ns | 22,821 ns | 2.4 |
| 2,000 units: `getUnitPositionByObjectId` sweep | 0.316 ms | 0.359 ms | 1.1 |
| 2,000 units: enumerate groups and units | 1.0 ms | 2.53 ms | 2.5 |
| 2,000 units: `getPoint` sweep | 1.55 ms | 4.22 ms | 2.7 |
| 2,000 units: `getPosition` sweep | 5.86 ms | 19.9 ms | 3.4 |
| 2,000 units: `getAmmo` sweep | 3.45 ms | 10.8 ms | 3.1 |
| 2,000 units: `Export.LoGetWorldObjects` | 14.1 ms | 48.7 ms | 3.5 |
| `coalition.addGroup`, 10 M-113, at 2,000 units | 5.3 ms | 5.86 ms | 1.1 |
| One airborne F-15C | 6.6 ms | 12.6 ms | 1.9 |
| One static object | 0.12 ms | 0.86 ms | 7 |
| `Group:destroy()` per group | 0.5 µs | 2.2 µs | 4.4 |

Class: Timing, for every row. Each ratio mixes other hardware with Wine and with the
dedicated server, and these runs do not separate them. The position tier's call,
`getUnitPositionByObjectId`, is the one that scales best: 0.36 ms for 2,000 units. The
static object compares one type on the client with another here. The 28.9 ms
`getVelocity` pass over 1,000 units seen once before did not recur (1.89 ms).

## What the Windows checkpoint must do

Re-run `cargo xtask probes` on the Windows client with a freshly loaded mission, so the
matrix's model-time-0 condition runs, and reopen any row above whose client value it
changes. `cargo xtask run` needs a target with a restart command, which a local profile
has not. `hasTask` and the reload's
`onPlayerStop` are the two rows it may resolve.
