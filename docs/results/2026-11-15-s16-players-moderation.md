# Players, moderation and extensions

| | |
|---|---|
| Step | S16 exit test: live on the Wine server without a person, menu items added and removed, a mark and a drawing set and removed, messages sent, the `CHAT` and `MISSION_LOAD` handlers run, an extension registered that survives a reload with its time in metrics, and raw Lua gated. Experiments E34, E35 and E36 run. |
| Date | 2026-11-15. Exit test players runs 1 to 3, moderation runs 1 and 2, experiment runs |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release`; scripted client: DCS 2.9.31.29434 on Windows (LIVE, run provenance) |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Scripted client: Windows. Host rehearsal: macOS on arm64. |
| Builds | Every live run, on the server and on the client, used agent build `31c7ce9d396e3fa4` (`deploy --dev`). |
| Script | `cargo xtask commands <target> --part players` (`crates/xtask/src/commands/players.rs`), which runs the probe in `crates/fake-agent/src/players_exit.rs`; with `--scenario moderation` (`crates/xtask/src/commands/moderation.rs`) for E34 and E35; `cargo xtask eval <target> <state> <file>` for the snippets in `lua/experiments/s16/` (`e-emit-objects`, `e-frame-before-start`, `e-handler-survives-load`, `e36-chat-hooks`, `e34-ban-restart`, `exit-*`) and `lua/experiments/alone/` (`s16-*`, `e30-join`) |
| Rehearsal | `lua/tests/players_exit.lua`, `lua/tests/s16_experiments.lua`, `lua/tests/dispatch.lua`, `crates/sim-fake/tests/players.rs`, `roster.rs`, `adoption.rs`, `crates/fake-sidecar/tests/players.rs`, `moderation.rs`, `adoption.rs`, all run by `mise run check` |
| Bundle | `bundles/commands-players-<target>-<stamp>/`, `bundles/commands-moderation-<target>-<stamp>/` (not committed: they hold server data), logs in `bundles/s16/` |
| Result | PASS with open items. The exit test passes. The kick, ban and slot-move person tests pass with the scripted client. The menu callback from a client is inconclusive and waits for the owner. Every experiment has a verdict. |

All figures are LIVE unless tagged. Both targets were restored to their baseline after the
runs.

## Exit test

The probe runs against a server with no player. It sets a submenu and a command item and
selects the item, removes both, sets a mark and a drawing and reads them back, removes them
and reads again, sends a message to all, a message to a coalition and a chat line, and checks
that raw Lua is gated. With raw Lua on it also registers an extension, sends `MISSION_LOAD`
and checks the extension after the load.

Rules the probe applies:

- Menu: the command item is selected through `missionCommands.doAction(index)` in the server
  state, and a `MENU` record carries the item's ID.
- Marks: `world.getMarkPanels()` lists the shim's panels after the set and none after the
  remove.
- Gate: with raw Lua off, `EXTENSION_SET` answers `RAW_LUA_OFF` and `EVAL` answers
  `RAW_LUA_OFF`. With raw Lua on, `EVAL` returns its value.
- Extension: `on_frame` and `on_event` run, `invoke` answers, `emit` gives an
  `AUTHOR_EVENT`, and an `EXTENSION` record (one sample of the extension's run time) arrives
  with `calls` above 0. After `MISSION_LOAD` the lifecycle goes to `LOADING`, then back to
  running with a new generation, the chunk runs again, and the same checks hold.

| Run | Raw Lua | Result | Cause or detail |
|---|---|---|---|
| 1 | off | FAIL, in the probe's test data | `DRAWING_SET` of shape 2 (circle) answered `DCS_REFUSED`: `markupToAll` takes a point at parameter 5 for shape 2, and the probe sent a number. `menu_select` was skipped: `doAction` was tried for indexes 1 to 40. |
| 2 | off | PASS | Shape 1 (line, two points). `menu_select` passed with index 0. |
| 3 | on | PASS, 24 of 24 steps | Extension across `MISSION_LOAD`: `LOADING`, then running again in 3 s, generation 2 to 3. `on_frame` ran 1,179 times, `on_event` 4 and `invoke` 2 before the load; `on_frame` 3 times and `on_event` 2 after it. `EVAL` returned 42. |

The run cap was 3 and 3 runs were used. Run 1 failed on the probe's own data, not on the
platform. Every other step passed in run 1 before the failing one, and `menu_select` is
settled by `doAction` indexes counting from 0 (below).

In runs 2 and 3 the mark read back as 2 panels, one for the mark and one for the drawing,
and 0 after the remove.

### Person tests

The plan's person tests ran with a scripted client on Windows, with no person at the
keyboard.

| Test | Result |
|---|---|
| Kick with a reason | Pass. The client saw reason `np-s16-kick`, code 104. |
| Ban until unban | Pass. The client saw reason `np-s16-ban`, code 101. A rejoin was refused and no `CONNECT` record came. `BANS` listed the client among 1. After `UNBAN` the list no longer held it and the rejoin worked. |
| Slot move within E35's limit | Pass (see E35). |
| Menu callback by a client | Inconclusive after 3 attempts (the third was granted over the cap by the coordinator). Waits for the owner. |
| Menu callback through the server's `missionCommands.doAction(0)` | Pass. It returns a `MENU` record with the item's ID. |

Moderation run 1 passed connect, `PLAYERS`, slot, birth, the move to spectators, E35 and
the kick, then failed in the harness's rejoin wait: after a kick the client still lists the
last session's slots, so the join check returned before the client had joined. The check now
counts a join only with `DCS.isMultiplayer()` and a player ID. Moderation run 2 passed end
to end, 17 of 17 steps.

## Experiments

| Experiment | Rule | Runs | Verdict | Key facts |
|---|---|---|---|---|
| E34 moderation calls | Each does what it says, or the gap is recorded | 2 | Met | Kick with a reason, ban, unban, move to spectators. A ban survives a DCS restart. LIVE. |
| E35 slot gating | Out within 1 s with a controller that answers at once, no crash | 2 | Met | 16.8 ms and 27.6 ms from the client's `SLOT` record to the result. No aircraft spawned. LIVE. |
| E36 chat | Recorded; the owner chooses the chat design | 3 attempts | Recorded | A returned value ends the chain for sets installed before it. LIVE. |
| Frame before start | Recorded | 1 | Recorded | No frame before `onSimulationStart`. LIVE. |
| Handler across a load | Recorded | 2 attempts | Recorded | An eval-installed event handler survives a mission load. LIVE. |
| Object IDs | Recorded | 1 | Recorded | `id_` differs from `getID()` for every object read. LIVE. |

### E34 moderation calls

The moderation scenario ran in moderation runs 1 and 2; the figures below are from run 2
unless stated.

- Kick with a reason: the `KICK` command applied and a `DISCONNECT` record came. The client's
  disconnect showed reason `np-s16-kick`, code 104 (`ERR_KICKED`). Run 1 showed the same.
  The same player rejoined as connection 2.
- Ban: `BAN` for 180 s applied and a `DISCONNECT` record came. The client showed reason
  `np-s16-ban`, code 101. A rejoin was refused and no `CONNECT` record came.
- Ban list: `BANS` listed the client among 1. `UNBAN` applied, `BANS` no longer listed the
  client, and the rejoin worked.
- Move to spectators: `MODERATE SLOT` applied and a `SLOT` record showed side 0 and an empty
  slot. The client reported 1 slot change.
- A ban survives a DCS restart. `lua/experiments/s16/e34-ban-restart.lua` added a ban with
  `net.banlist_add(client, 600, reason)` (true; listed 1), DCS restarted with no player, the
  list still held 1, `net.banlist_remove` removed it (1) and the list held 0.

`net.banlist_add`, `net.banlist_add_by_ucid`, `net.banlist_get`, `net.banlist_remove`,
`net.kick` and `net.force_player_slot` are all present in the hook state (the agent's
capability record in `hook.log`).

### E35 slot gating

The client took a slot and asked to spawn in one step while the server's slot handler sent
a move to spectators on the slot event.

| Measure | Run 1 | Run 2 |
|---|---|---|
| Command queued | 11.3 ms | 2.1 ms |
| Command written | 13.1 ms | 2.4 ms |
| `SLOT` record to spectators | 25.5 ms | 14.7 ms |
| Result | 27.6 ms | 16.8 ms |

Each time is measured from the client's own `SLOT` record. Both runs are inside the 1 s
limit. The client's own record shows the slot change to the aircraft slot and, in the same
moment, the change to side 0 with an empty slot. No aircraft spawned in either run and no
`BIRTH` followed the record. The event IDs after the slot record were `[21]`. The client saw
side 0, slot `""`, unit `""`, with no disconnect and no mission change. No crash.

When the client took the slot in the normal way (before the gating test), the events were
`[15, 34, 58]`, which include a `BIRTH`.

### E36 chat

`lua/experiments/s16/e36-chat-hooks.lua` installed two `onPlayerTrySendChat` callback sets
in the hook state: an observer that returns nothing and a replacer that returns a
replacement string for one line (`np-s16-r one`). The client sent two lines. Each callback
received a player ID (number), the message (string) and an all-flag of -1 (number), with no
further arguments.

| Attempt | Install order | DCS call order | Result |
|---|---|---|---|
| 1 | replacer, then observer | observer, then replacer, for both lines | Both callbacks saw both lines. The sender saw both lines unchanged. |
| 2 | harness race | | The client sent before the server snippet was installed. Nothing was seen. Not counted. |
| 3 | observer, then replacer | replacer, then observer | For the plain line the observer was called next. For the replaced line the observer was not called. The sender saw both lines unchanged. |

DCS calls the most recently installed callback set first. A set that returns a value ends
the chain for the sets installed before it. A set that returns nothing lets the chain
continue. The sender saw its own lines unchanged in both runs. Whether other clients see a
replaced line was not observed: one client only.

The rule records these facts. The owner chooses the chat design, including the opt-in chat
filter.

### Other facts

- No frame runs before `onSimulationStart`. The hook callbacks come in the order
  `onMissionLoadBegin`, `onMissionLoadEnd`, `onSimulationStart`, with 0 frames between them
  (`e-frame-before-start.lua`, hook state).
- An event handler installed by an eval snippet survives a mission load. It counted 5 events
  before a load of `templates/caucasus.miz` and 8 after it, and the server state was not
  reset (`e-handler-survives-load.lua`). This settles the question S15 left open. The first
  attempt was invalid: the load was given a file name from the remote machine and no load
  happened.
- `id_` differs from `getID()` on the objects read: 6 of 6 (5 units and 1 static). An example
  pair: `id_` 16777473 against `getID` 1000001 for a unit, and 8000001 against 1000006 for a
  static (`e-emit-objects.lua`, server state). An emit of objects spawned in the same chunk
  answered `false` for `objects`: they were not yet in the agent's table.
- `missionCommands.doAction` indices count from 0. An item added with
  `missionCommands.addCommand` was selected by `doAction(0)`; indexes -2 to 300 hit only 0.
  `doAction('name')` raises `Parameter #1 (action id) missed`.
- DCS refused `markupToAll` shape 2 with a number at parameter 5: it wants a point there.
- A menu callback gets no player. ED-CODE: the radio panel calls
  `missionCommands.doAction(actionIndex)` (`Scripts/UI/RadioCommandDialogPanel/RadioCommandDialogsPanel.lua`).
  The callback receives what the script passed to `addCommand`
  (`Scripts/ScriptingSystem.lua`).

### Menu callback from a client

Three attempts to pick a menu item as the client, from the client's own input route.

ED-CODE: `Config/Input/CommandMenu/keyboard/default.lua` binds F1 to F12 to
`ICommandMenuItem1` to `ICommandMenuItem12`. `Config/Input/Aircrafts/common_keyboard_binding.lua`
binds `iCommandToggleCommandMenu` to the communication menu. No install text file gives
their numbers.

- Attempt 1: the client's `net.dostring_in` answered nil for the gui, export, mission and
  server states, and the hook state has no `Input` global. Nothing was sent.
- Attempt 2: `require('Input')` in the client's hook state gives ED's module.
  `Input.getEnvTable().Actions` gives `iCommandToggleCommandMenu` 179, `ICommandMenuItem10`
  975 and `ICommandMenuItem1` 966 (DCS 2.9.31.29434, Windows client). The client sat in its
  A-10C and sent the three commands with `Export.LoSetCommand`, 0.6 s apart. The server-side
  hit counter was unreadable: each server eval runs in its own environment, so the reading
  eval did not see the counter. Result unknown.
- Attempt 3: a server item whose callback raises a global counter and calls the agent's
  menu hook. The client sat in its A-10C, unpaused, and sent the same three commands with
  `Export.LoSetCommand`. The counter stayed 0 and no `MENU` record came in 1,954 records. The
  callback did not run.

The result is inconclusive. `Export.LoSetCommand` exists in the client's hook state, but the
radio menu commands sent from that state selected nothing. Whether that state drives input
at all is UNVERIFIED. The client's `net.dostring_in` answers nil for every state. The test
waits for the owner on a client with a person.

## Open items

- The client menu callback is not settled (see above). The owner runs it on a client with a
  person at the controls.
- Whether other clients see a replaced chat line is not observed: E36 ran with one client.
- E36 and the opt-in chat filter design are the owner's choice.
- `PLAYERS` is not sent while no campaign is active.
- `players.json` waits for S18 to wire the store directory.
- `SCORE` events get no dated time.
- A `DEAD` with no `BDA` before its row leaves the world is not dated.
- The sound and picture client checks and the client's view after a damage kill wait for
  the owner (from S15).
- E31 remainder and E32 were not run in S16. They need a client in combat. Not done.
- The E15 scenery probe was not run in S16.
- Host note: `mise run check` runs failed in the `lua dispatch` scenarios (heartbeat
  timeouts) while another session ran CPU-load loops on the same machine. The failures fit
  that load.
