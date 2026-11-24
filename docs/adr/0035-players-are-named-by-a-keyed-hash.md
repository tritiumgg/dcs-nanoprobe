# ADR 0035: Players are named by a keyed hash, and ucids stay inside the sidecar

## Status

Accepted

## Context

DCS gives each client a ucid. It is the same on every server, and a ban list is keyed
by it. An API that carried ucids would put a stable identifier into every event, log,
metric and diagnostics bundle, and a consumer could join players across servers.

Moderation still needs the ucid: `net.banlist_remove` takes one, and a connection ID
can be reused by another player between the sidecar's read and the agent's call.
The server-scoped store holds what the server derives
(spec/persistence.md, "The server store"), so it can hold the secret and the map.

## Decision

The API, the event stream, logs, metrics and bundles name a player by a player key, an
HMAC of the ucid under a secret made once per server; the ucid stays in the sidecar
and the agent.

- The key is the first 16 bytes of HMAC-SHA-256 of the ucid, keyed with a 32-byte
  random secret, written as 32 lower-case hex digits (spec/world.md, "Players";
  `Keyring` in `crates/sidecar/src/players.rs`).
- The secret and the key-to-ucid map live in the server store, with mode 0700 on the
  folder, on Linux and macOS. Campaign backups and bundles never hold them. With the
  store off they last as long as the process.
- An operator may give several servers one secret, so a player has one key on all of
  them. With different secrets the same ucid has unrelated keys.
- A raw ucid is an admin input only: `Unban` takes one, and `Ban` takes one for a
  connected player. A ban of a player with no connection is refused until it is
  verified. `ListBans` and every other output carry keys.
- `MODERATE` carries the connection ID and the ucid. The hook reads
  `net.get_player_info(player, 'ucid')` first, and a missing player or a different
  ucid is `NOT_BOUND` (spec/commands.md, "Moderation").
- A bundle redacts any text of 32 hex digits, which covers a ucid and a key.
- Seat occupancy joins a `SLOT` record to a `BIRTH` whose mission ID equals the slot
  ID without its `_N` seat suffix, in either order. A held slot attaches to every
  later birth with that ID, so a
  respawn is covered. A slot that matches no birth leaves the unit unknown, counted,
  never guessed.
- A menu selection that names no player is reported with attribution `AUDIENCE`: the
  keys are the candidates by occupancy at the record's time, and the sidecar never
  picks one (spec/records.md, "Layout: MENU").

Rejected:

- An unsalted hash of the ucid: anyone holding a ucid could compute its key on any
  server.
- Carrying ucids in the API: every consumer and log would hold the identifier.

## Consequences

- A campaign folder copied to another server meets other player keys there, unless the
  operator shares the secret (spec/persistence.md, "Files").
- Losing the secret changes every key. Keys in old events do not match new ones.
- A redaction pattern of 32 hex digits also hides other 32-digit hex text in a bundle.
- Whether a client's slot ID equals the mission ID for a dynamic-spawn slot is
  UNVERIFIED. For a mission's client slots it holds (LIVE,
  [bind lag](../results/2026-11-17-bind-lag.md)).
- A controller that needs the exact chooser of a menu item uses a group-scoped item on a
  group with one player seat.
