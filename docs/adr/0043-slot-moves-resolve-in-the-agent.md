# ADR 0043: Slot moves resolve the side and check the slot in the agent

## Status

Accepted

## Context

`MovePlayer` takes a coalition and a slot ID. An operator wants three moves: out of a
slot on the same side, to a given coalition, and into a given slot. DCS has one call
for all of them, `net.force_player_slot(player, side, slot)`. What it does was read on
a server under Wine with a scripted client
([player moves](../results/2026-11-24-player-moves.md), LIVE):

- With an empty slot it sets the side it is given. A player in a slot moved with their
  own side leaves the slot and keeps the side.
- Into a slot of the other coalition it returns `false` and changes nothing.
- Into a slot ID the mission does not have it returns `true`, and the player holds a
  slot with no unit.
- `DCS.getAvailableSlots(coalition)` lists a coalition's slots, taken ones included.
- It applies the side it is given. A move built from a side read before the player
  changed side put the player back, and the client never showed the side it chose.

The sidecar knows each player's side from `SLOT` records, a frame or more old. It has
no table of the mission's slots and their coalitions; only the hook state can read one.

## Decision

The API maps `COALITION_UNSPECIFIED` to the `SLOT` side `KEEP` (0xFFFFFFFF) and the
other coalitions to DCS sides (neutral 0, red 1, blue 2). The agent resolves `KEEP` and
checks the slot in the frame of the call, before it calls `force_player_slot`
(spec/commands.md, "Slot moves").

- `KEEP` with no slot is `net.get_player_info(player, 'side')`; with a slot, the
  coalition whose list holds it.
- The agent reads `DCS.getAvailableSlots` only for the coalition it needs: the one the
  side names; for `KEEP`, red, then blue when red does not list the slot. With red or
  blue it reads the other list only to name a refusal. It keeps no cache.
- It refuses, each with a fixed message, a crew seat (`<unitId>_<n>`), a slot no list
  holds, a slot of the other coalition, a slot another connected player holds, and a
  list, a side or another player's slot DCS does not give. Every such call runs under `pcall`, list entries are
  read with `rawget`, and IDs are compared as text.
- `COALITION_NEUTRAL` with a slot is `INVALID_ARGUMENT`. The decoder refuses a side
  above 2 that is not `KEEP`, `KEEP` with any action but `SLOT`, and side 0 with a slot.
- The record layout is unchanged; `layout_version` is 2, so an agent that would pass
  `KEEP` to DCS does not pair with this sidecar.

Rejected:

- The sidecar sends the roster's side for `COALITION_UNSPECIFIED`. It undid a player's
  own side change (LIVE).
- DCS decides about a slot another player holds. Its behavior there is UNVERIFIED, and
  the list does not tell a taken slot from a free one.
- A cache of the slot lists. A move is rare; a read costs one list per coalition.

## Consequences

- A refusal reaches the caller as a `REFUSED` result with its message, not as a status
  code: the API cannot check a slot before the agent runs.
- The agent's read narrows the race with a player's own side change to a change DCS has
  not applied by the frame of the call. `COALITION_RED` and `COALITION_BLUE` always
  override the player's choice.
- A neutral player slot, if a mission has one, is out of reach of `MovePlayer`.
- Crew seats stay refused until a live run covers them. A move into the slot the player
  already holds goes to DCS, UNVERIFIED.
- A 0.1.0 controller that relied on `COALITION_UNSPECIFIED` meaning spectators must send
  `COALITION_NEUTRAL`.
