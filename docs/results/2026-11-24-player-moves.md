# Player moves with net.force_player_slot

| | |
|---|---|
| Question | What `net.force_player_slot(player, side, slot)` does with each side and an empty slot, with the player's own side, into a slot, against a slot of the other coalition and into a slot ID the mission does not have; the slot ID's form; and what happens when the player changes side just before a move. Then, on the final build, whether `MovePlayer`'s agent handler (spec/commands.md, "Slot moves") does what the ruling says, and what a move costs |
| Date | 2026-11-24, three live runs of the snippets and one of the final build |
| DCS | Server: 2.9.31.29434 dedicated server. Client: DCS 2.9.31.29434 on Windows. Both read with `cargo xtask target check`. |
| Platform | Server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Client: Windows, driven over SSH with no person at the controls. Host rehearsal: macOS on arm64. |
| Builds | Snippet runs: server `365d33e8c837e512`, `--dev`, unchanged; client `d03d9ce1dcd7d03b`, `--dev`. Final build: `c14fa8e9ff32edc0`, `--dev`, on both. Each was removed or rolled back after its runs. |
| Mission | The server's own mission, paused before the client joined. It offers 14 blue slots and no red slot, so no step moves a player into a red slot. |
| Script | `lua/experiments/player-moves/run.sh <server> <steps> [settle]` runs `moves.lua` in the server's hook state; `run.sh --client <client> <side>` runs `client-side.lua` on the client; `cargo xtask eval` runs `slots.lua` on the server and `client-read.lua` on the client. The client joined with `cargo xtask client join` and held no slot. Final build: `cargo xtask commands <server> --part moves --client <client>` (`crates/xtask/src/commands/moves.rs`, probe scenario `moves` in `crates/fake-agent/src/players_exit.rs`). |
| Rehearsal | `lua/tests/player_moves_experiments.lua`; for the final build `lua/tests/players_exit.lua` scenarios `moves` and `moves-occupied`, and `lua/tests/dispatch.lua` scenario `slot-moves`; all run by `mise run check` |
| Result | Every question has an answer, LIVE. DCS refuses a slot of the other coalition by returning `false`, and accepts a slot ID the mission does not have. A move with a side read before the player's own change undoes that change. The final build passes all 13 steps in one run: `KEEP` keeps the player's own change, and the agent refuses the missing slot and the other coalition's slot before DCS. A slot move costs at most 350 µs of commanded work. |

## Method

Each step of `moves.lua` reads the client's side and slot with `net.get_player_info`, calls
`net.force_player_slot` under `pcall`, reads again in the same frame, records the first
change and its time, and reads a last time after the settle time (6 s in runs 1 and 2, 3 s
in run 3). No step needs a person: the client is a scripted DCS client.

## Results

Run 1, steps `1,;1,;2,;2,@blue;2,;2,@blue;1,;2,@blue;1,@blue;2,99999999;0,`. `@blue` is the
first blue slot, `8580`. Every change was seen in the same frame as the call.

| Step | Call | Before | Returned | After |
|---|---|---|---|---|
| 1 | `(p, 1, '')` | side 0, no slot | `true` | side 1, no slot |
| 2 | `(p, 1, '')` | side 1, no slot | `true` | side 1, no slot, no change |
| 3 | `(p, 2, '')` | side 1, no slot | `true` | side 2, no slot |
| 4 | `(p, 2, '8580')` | side 2, no slot | `true` | side 2, slot `8580` |
| 5 | `(p, 2, '')` | side 2, slot `8580` | `true` | side 2, no slot |
| 6 | `(p, 2, '8580')` | side 2, no slot | `true` | side 2, slot `8580` |
| 7 | `(p, 1, '')` | side 2, slot `8580` | `true` | side 1, no slot |
| 8 | `(p, 2, '8580')` | side 1, no slot | `true` | side 2, slot `8580` |
| 9 | `(p, 1, '8580')` | side 2, slot `8580` | `false` | side 2, slot `8580`, no change |
| 10 | `(p, 2, '99999999')` | side 2, slot `8580` | `true` | side 2, slot `99999999` |
| 11 | `(p, 0, '')` | side 2, slot `99999999` | `true` | side 0, no slot |

Run 2, the race. The server ran steps `2,;2,,1;0,`: it put the client on blue with no slot,
then waited for the client's own side to become 1 and called `(p, 2, '')` in that frame, the
move a controller would send with the side it read before the change. The client ran
`client-side.lua`, which called `net.set_coalition(1, '')` 21 s into the wait.

| Where | What was read |
|---|---|
| Server, step 2 | Before: side 1, no slot (the client's change had arrived). Returned `true`. Same frame and after: side 2. |
| Client | `set_coalition` returned. Its own side read 2 at once and stayed 2, then 0 at 6.0 s (the server's step 3). It never read 1. |

Run 3: `(p, 2, '8580')`, then `slots.lua` on the server and `client-read.lua` on the client;
`(p, 2, '99999999')` and the client read; `(p, 2, '')` and the client read; `(p, 0, '')`.

| Read | Value |
|---|---|
| Server, `DCS.getAvailableSlots('blue')` with the client in `8580` | 14 slots, `8580` among them |
| Client in `8580` | `net.get_slot`: side 2, slot `8580`; `DCS.getPlayerUnit()`: `8580` |
| Client after `(p, 2, '99999999')` | `net.get_slot`: side 2, slot `99999999`; `DCS.getPlayerUnit()`: empty |
| Client after `(p, 2, '')` | side 2, no slot |

## Findings

- With an empty slot, `force_player_slot` sets the side it is given, 0, 1 or 2, and the
  player holds no slot. A move to the side the player is on changes nothing and returns
  `true`. LIVE.
- A player in a slot moved with their own side and an empty slot leaves the slot and keeps
  the side. LIVE.
- A move into a slot of the given side works from no slot and from the other side, in the
  frame of the call. LIVE.
- A move into a slot of the other coalition returns `false` and changes nothing. LIVE.
- A slot ID the mission does not have is accepted: the call returns `true`, and both the
  server and the client then report that slot with no unit. A later move recovers the
  player. LIVE.
- A slot ID is a string, the slot's `unitId` from `DCS.getAvailableSlots(coalition)`; on
  this mission each was decimal digits. On the server the list holds every slot of the
  coalition, a slot a player holds included, so it does not tell a free slot from a taken
  one. LIVE. The `_N` seat suffix of a multi-crew slot is PRIOR (ADR 0035), not seen here.
- `force_player_slot` applies the side it is given, whatever the player chose since. A move
  built from a side read before the player's own change puts the player back, and the client
  never shows the side it chose. LIVE.
- The client's `net.get_slot` and the server's `net.get_player_info` agree after every move.
  LIVE.

Not shown: a move into a red slot (the mission has none), a move into a slot another player
holds (one client), a move with side 0 and a slot, and a multi-crew seat.

## Final build

One run of `cargo xtask commands <server> --part moves --client <client>` on build
`c14fa8e9ff32edc0`, 2026-11-25 01:31 to 01:35 UTC, the server's own mission. The probe
is the sidecar: it sends each `MODERATE SLOT` over the real link and reads the client's
side and slot from `PLAYERS` until they match. Verdict PASS, 13 of 13 steps. Bundle
`bundles/commands-moves-<server>-20261125T013132Z/moves.json`, not committed.

| Step | Move (side, slot) | Answer | Client after | `cost_us` |
|---|---|---|---|---|
| `players_query` | | no player connected | | |
| `players_query_client` | | the client alone | | |
| `blue_no_slot` | 2, none | `APPLIED` | blue, no slot | 86 |
| `blue_slot` | 2, `8580` | `APPLIED` | blue, `8580` | 350 |
| `keep_leaves_slot` | `KEEP`, none | `APPLIED` | blue, no slot | 188 |
| `other_coalition_refused` | 1, `8580` | `DCS_REFUSED` `the slot belongs to the other coalition` | blue, no slot | 265 |
| `missing_slot_refused` | 2, `99999999` | `DCS_REFUSED` `the mission lists no slot with this ID` | blue, no slot | 279 |
| `red_no_slot` | 1, none | `APPLIED` | red, no slot | 82 |
| `keep_takes_slot_coalition` | `KEEP`, `8580` | `APPLIED` | blue, `8580` | 330 |
| `keep_race` | `KEEP`, none, every 100 ms | 60 × `APPLIED` | red, no slot | |
| `spectators` | 0, none | `APPLIED` | spectators | 77 |

In `keep_race` the client called `net.set_coalition(1, '')` itself while the probe sent
`KEEP` moves: 55 went out before the probe read the client's change and 5 after. The
client read red 0.1 s after its call and stayed red to the end. Finding 7's move with
the side read before the change put the player back on blue; `KEEP` did not. LIVE.

`keep_takes_slot_coalition` starts from red, so the agent can reach blue only from the
slot's list. The two refusals left the client where it was, so `force_player_slot` was
not called with the other coalition or the missing ID. LIVE.

**Cost.** The handler's `cost_us` is 77 to 86 µs for a move with no slot and 265 to
350 µs for a move with a slot, which reads one or two slot lists and the player list.
`KEEP` with a slot read red, where the mission lists no slot, then blue. The agent's
commanded work has its own budget, `commanded_ms`, 2 ms a frame by default
(spec/config.md), apart from the sampling slice of 250 µs that the S11 baseline
measures (docs/results/2026-11-14-s11-budget-and-baseline.md). One slot move uses at
most 18 % of that budget in one frame, and a move is an admin action, not a per-frame
cost. Rule set here: a slot move must fit in one frame's commanded-work budget; it does.
The server's mission is the largest test mission with player slots, 14 blue and none
red; the test templates in `tests/fixtures/templates/` hold none. A mission with more
slots costs more per list read, UNVERIFIED.

Not shown on the final build: a move into a red slot and `KEEP` with a red slot (the
mission has none), the refusal of a slot another player holds (one client), a move into
the slot the player holds, and a crew seat. Each is covered on the mock only.

## Restore

After the snippet runs the server's mission was reloaded from its own file and is paused
at model time 0, as before. After the final-build run the server's build, manifest,
development folder, logs and `agent.toml` were put back, and DCS restarted twice; the
mission is paused at model time 0. The client's build was undeployed and its `Logs/`
put back after each.
