# S9: events and the event catalogue

| | |
|---|---|
| Step | S9: event forwarding with allow-lists, the stream recorder, the catalogue builder, wider scenarios; experiments E9 and E30, and S9's part of E31 |
| Date | 2026-11-13 |
| DCS | 2.9.31.29434 (`dcs_server.release`) under wine-10.0 (Debian 10.0~repack-6), docker-wine; the client part on a Windows client of the same DCS build |
| Scripts | `cargo xtask catalogue <target>` with `lua/catalogue/*.lua`; for E30 and E31, `cargo xtask restart <windows-target>` (the profile's start and stop tasks), `cargo xtask client join <windows-target> <server-target>` (`lua/experiments/alone/e30-join.lua`), then `e30-take-slot.lua`, `e31-spawn.lua` and `e31-leave.lua` through `cargo xtask eval`, with `link-probe --record` and `e31-record.lua` on the server |
| Result | Exit test met for every event type reached. E30 partly meets its rule: a scripted client joined, took a slot and spawned, but reading a menu item or a mark from the client is still not found, so player-facing menu and mark tests stay manual. S9's slot events recorded; `S_EVENT_PLAYER_ENTER_UNIT` did not come. |

## Exit test

The plan's exit test is a field-presence report committed for every event type
reached, and a decoder that survives every catalogued and fuzzed shape.

- **The report** is `docs/results/2026-11-13-event-catalogue.md`, with the catalogue
  it was built from in `2026-11-13-event-catalogue.json`. 24 event types reached: 20
  seen by the Lua recorder with field presence per type and cause, and 4
  (`S_EVENT_MISSION_START`, `MISSION_END`, `MISSION_RESTART`, `SIMULATION_START`) seen
  only as agent records, which the report marks. The run took about 12 minutes, of
  which DCS's restart and first load took about 3.
- **The decoder** read every one of the run's 695 `EVENT` records (LIVE). The test
  `every_catalogued_shape_decodes` (crates/xtask/src/catalogue.rs) builds an `EVENT`
  from every shape the committed catalogue and the allow-lists name, at its own type
  and at eight others, and decodes each record, its cut form and every prefix of it.
  The fuzz runs are below.

## Fuzzing

`mise run fuzz` with `FUZZ_SECONDS=600`, on the Mac, nightly toolchain, 2026-11-13.
The `records` corpus was seeded with the 785 distinct payloads of the first live
recording. No target found a crash.

| Target | Executions | Coverage (edges) | Features | Corpus |
|---|---|---|---|---|
| `lv` | 35,247,533 | 250 | 1,261 | 217 |
| `ring` | 224,933,865 | 90 | 192 | 41 |
| `records` (every record decoder, `EVENT` included) | 51,493,840 | 714 | 2,612 | 466 |
| `handshake` | 8,597,938 | 128 | 556 | 155 |
| `header` | 19,307,344 | 281 | 360 | 25 |

## What the catalogue changed

All LIVE on the run of the committed report unless tagged.

- **Category codes.** An explosion that kills a unit also kills the scenery around
  it, and each scenery object is a `S_EVENT_DEAD` and an `S_EVENT_UNIT_LOST` of its own:
  304 of 309 deaths were scenery. `event_field` 17 `INITIATOR_CATEGORY` and 18
  `TARGET_CATEGORY` carry `initiator_data.object_category` and
  `target_data.object_category` so a consumer can tell them apart.
- **A dead killer.** On 2 of 3 `S_EVENT_KILL` events the killer was already dead, and
  `event.initiator` was a plain table with `id_`, `coalition`, `dead`, `misID` and
  `unit_type`, not an object. The shim reads `id_` with `rawget`, so `INITIATOR` is
  still forwarded.
- **Mark time.** Scripted marks emit `S_EVENT_MARK_ADDED` and `S_EVENT_MARK_REMOVED`
  with no initiator. On `S_EVENT_MARK_ADDED` the `time` was 28,799.1 s ahead of model
  time, the time of day for the shell's start at 28,800 s. On `S_EVENT_MARK_REMOVED` it
  was at most 28,796.1 s ahead, 3 s less, as if a removal carries the removed mark's add
  time (UNVERIFIED inference). Every other type's `time` was within 0.004 s of model
  time on arrival.
- **Allow-lists** were rewritten from the catalogue, with an Evidence column
  (spec/records.md, "Allow-lists"). New lists: `S_EVENT_BDA`, `S_EVENT_SCORE`,
  `S_EVENT_AI_ABORT_MISSION`, `S_EVENT_SIMULATION_FREEZE`, `S_EVENT_SIMULATION_UNFREEZE`,
  `S_EVENT_MISSION_RESTART`, `S_EVENT_SIMULATION_START`. `S_EVENT_SHOT` carries a target.
  `S_EVENT_BASE_CAPTURED` had `subPlace` and no `coalition`; a spawned M-113 near an
  airbase captured it.
- **Agent cost.** Agent time per event (`hist_event`): 695 events, half under 2 µs,
  99 % under 16 µs. No event was dropped.

Two earlier runs of the same scenarios on the same server, the same night, reached
types the final run did not: the first `S_EVENT_CRASH`, `EJECTION`, `PILOT_DEAD`,
`LANDING_AFTER_EJECTION`, `DISCARD_CHAIR_AFTER_EJECTION`, `AI_ABORT_MISSION` and
`ENGINE_SHUTDOWN`, the second `ENGINE_SHUTDOWN`. Their reports are not committed; the spec tags those rows PRIOR.
In the first run the SA-8 of the air-defense scenario outlived it and shot down the
airfield scenario's aircraft; every scenario removes its units when it ends.

## E30: a scripted client

LIVE, 2026-11-13, a Windows client and the Wine server above, both on DCS 2.9.31.29434.

1. `cargo xtask restart <windows-target>` ran the profile's stop and start tasks
   (`schtasks /run` over SSH), which started DCS in the logged-in desktop session. The
   hook with the eval channel ran 31 s later.
2. A first join, with no password, reached the server and was refused: the client's
   `dcs.log` read `Session was closed with ERR_INVALID_PASSWORD: Invalid password`.
3. `cargo xtask client join` then joined with the password from the server profile's
   `client_password_file`: `net.start_client` returned `"0"`, and the client was in a
   multiplayer session as player 2, offered 14 blue slots. Neither side's `dcs.log`
   held the password; the command checks both.
4. `e30-take-slot.lua`: `net.set_coalition(2, '')` and `net.set_slot(2, '8580', '',
   nil)`, as ED's role screen does, took a slot of type `A-10C_2`. `e31-spawn.lua`:
   `net.spawn_player()` put the client in the cockpit; `DCS.getPlayerUnit()` read 8580.
5. `e31-leave.lua`: `net.stop_game()` left the server.

E30's rule asks for a client that joins, takes a slot and reports a menu item and a
mark. Joining and taking a slot pass. Reading the client's own view of a menu item or
a mark from Lua is still not found (S4), so that part fails, and as the rule says,
player-facing menu and mark tests stay manual.

The command found the password in neither side's `dcs.log`. After DCS exited, a
one-off script, not committed, searched every file under the client's Saved Games
whose time had changed since the run began and found the password in none; it did not
read DCS's own `Config/authdata.bin` (PRIOR: the script is not in the repository).

## E31, S9's part: a player's world events

LIVE, the same run; the report is `docs/results/2026-11-13-e31-event-catalogue.md`.
The server's Lua recorder and the agent's records agree on every type both saw.

- Taking the slot and spawning: `S_EVENT_BIRTH` of the aircraft, with `place` and
  `subPlace`, and 9 `S_EVENT_WEAPON_ADD`, one per store, with `weapon_name`.
- Leaving: `S_EVENT_PLAYER_LEAVE_UNIT` with the initiator and `initiator_data`.
- `S_EVENT_SIMULATION_FREEZE` and `S_EVENT_SIMULATION_UNFREEZE`, which both sources
  saw; that they came from the server pausing and resuming as the player came and
  went is UNVERIFIED. `S_EVENT_SIMULATION_START` came before the recorder started, so
  only the agent's records have it.
- No `S_EVENT_PLAYER_ENTER_UNIT` arrived for this client, which took a slot and
  spawned through `net.set_slot` and `net.spawn_player`. Whether a client that takes a
  slot through ED's screens gets one is UNVERIFIED.
- Agent time per event: 14 events, half under 4 µs, 99 % under 8 µs.

Of the events E31 lists, this run did not see these for the player: enter, takeoff,
landing, shot, hit, kill, crash, ejection and death. They, the player records
(`PLAYER`, from the hook's callbacks) and whether a stable client ID can be read are
step S16's part of E31.
