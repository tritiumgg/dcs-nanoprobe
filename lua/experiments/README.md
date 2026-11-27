# Experiments

Eval snippets for the experiments in step S4. Each file has the probe header of
`spec/eval.md` and runs through the same runner as the probes:

```
cargo xtask probes <target> --dir lua/experiments --only <text>
```

The files in `alone/` end the DCS process on purpose, so the runner never picks
them up. Run each with `cargo xtask eval <target> <state> <file>` on a server with no
players, and let DCS restart.

Observations are recorded with `info` or `timing`, apart from the ones a pass rule
decides, which carry an expected value. The results and their rules are in
`docs/results/`.

## E38

The snippets in `e38/` belong to experiment E38, on reading unit life and ammunition.
The runner does not pick them up: each is run alone with
`cargo xtask eval <target> <state> <file>`, in the state its header names, on a server
with no players. `hook-log.lua` installs a logger on its first run and returns the log
on the next; it runs in the hook state and in `gui`. `store.lua` reads what a fight
left, so it runs after `coverage.lua`. `coverage.lua`, `classify.lua` and `rearm.lua`
each take six to eight minutes.
`lua/tests/e38.lua` rehearses them on the mock. The results are in
`docs/results/2026-11-14-e38-life-and-ammunition.md`.

## S15

The snippets in `s15/` belong to the experiments E6, E8, E14 and E16 of step S15 on
world commands. The runner does not pick them up: each is run alone with
`cargo xtask eval <target> <state> <file>`, in the state its header names, on a server
with no players, and cleans up what it adds. They use `E.lib('s15')`
(`lua/scenarios/s15.lua`: spawns with ammunition, flares and chaff, a capture read, a
text encoder) and the scenario library, which `deploy --dev` installs.

| File | State | Settles | Takes |
|---|---|---|---|
| `e06-warehouse.lua` | `server` | E6: set then read back, on a limited warehouse | a second |
| `e08-capture.lua` | `hook` | E8: an aircraft, a helicopter and a ship captured, destroyed, added again and compared field by field | 25 s of model time |
| `e08-attack.lua` | `server` | E8: whether an attacker engages when its AttackGroup task is issued again with the new group ID | 108 s of model time |
| `e14-ammo.lua` | `server` | E14: which `getAmmo` entry each ground-unit `ammo` slot feeds, and the offset, for ten types | about 80 s of model time |
| `e21b-names.lua` | `server` | E21b: whether platform names may use a base-64 alphabet: case, every character, leading `.` `#` `!` `~`, found and read back byte for byte | 6 s of model time |
| `e-sound-picture.lua` | `hook` | `outSound*` calls and the mission-state `a_out_picture*` calls, each recorded as accepted or raised (client playback and display UNVERIFIED) | about a second |
| `e-zones.lua` | `hook` | Run-time trigger zones: `trigger.misc.addZone`, `getZone`, the zone event and a trigger on it. Recorded; zones are dropped from the platform | timeout 300 s |
| `e-waypoint-scripts.lua` | `hook` | Waypoint `Script` actions: when each fires, in which state, at what cost, while paused, and after a re-route | timeout 300 s |
| `e16-damage.lua` | `hook` | E16: `a_unit_set_life_percentage` at 50 and 0 on aircraft, helicopter, ship, static and a ground control | 20 s of model time |

E6 needs a mission packed with `cargo xtask miz ... --limited-airport ID`, which makes
that airbase's warehouse limited; the sound and picture snippet needs a mission packed
with `--sound-picture`, which adds `np-tone.wav` and `np-picture.png`.

Each records one observation per step, with `info`. `e06-warehouse.lua` says in
`finding` when every warehouse of the mission is unlimited, which makes no setter
readable. `e08-capture.lua` gives `comparison`, a table per subject with each field's
first read, second read, difference, allowed difference and verdict, and
`reads_for_flares_chaff_pylons`. `e14-ammo.lua` gives `type_<n>_<type>` with `slots`
(each slot's row, count asked and count read) and `verification`.

`cargo xtask routes <target>` is experiment E37, on a docker-wine target. It deploys a
development build, restarts DCS, loads a fresh shell mission, and for each route shape
of `lua/scenarios/routes.lua` adds the same number of ground units (`--units`, 400),
settles 20 s, counts frames for 45 s, and removes them. Everything is a flag: the
shapes, units, sweeps, settle and measure seconds, path length, waypoint step, speed,
unit type, lane spacing and the formation names. It writes `routes.json` and
`routes.txt` under `bundles/routes-<target>-<time>/`: mean and p5 frames per second,
model/real, DCS CPU and the share of groups that moved, per shape.

`lua/tests/s15_experiments.lua` and `lua/tests/routes.lua` rehearse them on the mock.

## S16

Live checks and the exit tests of step S16 on players, moderation and extensions.

`cargo xtask commands <server> --part players --raw-lua on|off` is the exit test
that needs no person. It needs the `[lua] raw` of the profile's `agent.toml` set to
match the flag (it refuses otherwise; the file is not changed), restarts DCS, loads
the shell mission and runs `command-probe --part players` as the sidecar. The server
state is read, and the menu command selected, with `exit-marks-read.lua`,
`exit-menu-pick.lua` and `exit-mission-path.lua` in `s16/`. It writes `players.json`
under `bundles/commands-players-<target>-<time>/`.

`cargo xtask commands <server> --part moderation --client <windows-target> [--miz FILE]`
is the E34 and E35 driver. The server needs a mission with player slots (the
mission it starts with, or `--miz`), and the client is a windows-ssh target at its
main menu with a `--dev` build; `client join` documents what the server profile
needs. It writes `moderation.json`.

`lua/tests/players_exit.lua` rehearses both on the mock, and `lua/tests/s16_experiments.lua`
rehearses every snippet below.

The checks in `s16/` run with `cargo xtask eval <target> <state> <file>`, on a server
with no players, and clean up what they add.

| File | State | Settles | Takes |
|---|---|---|---|
| `e-frame-before-start.lua` | `hook` | whether DCS runs a simulation frame between onMissionLoadBegin and onSimulationStart: the counts over one load of the running mission, which it starts itself | one load |
| `e-emit-objects.lua` | `server` | whether `id_` equals `getID()` for 5 ground units and 1 static, and what `emit` returns for each form | a second |
| `e-handler-survives-load.lua` | `hook` | whether an eval-installed event handler counts after `cargo xtask load`: run it, load, run it again | two runs |
| `e36-chat-hooks.lua` | `hook` | the order and arguments of two extra `onPlayerTrySendChat` sets, one returning a replaced line, as a client chats; run `alone/s16-chat.lua` on the client while it waits | up to 180 s |
| `e-doaction-index.lua` | `server` | which index `missionCommands.doAction` takes for a command added with `addCommand` (-2 to 300), and the error text of a string argument | a second |
| `e-menu-client-item.lua` | `server` | whether the agent reports a command chosen through its callback: `<step>` `add`, choose `np-s16-pick3`, then `read`, which returns the count and removes the item | two runs |
| `e34-ban-restart.lua` | `hook` | whether a ban survives a DCS restart: `<step>` `ban` (bans the lone connected test client), restart DCS, then `check` (finds the ban, removes it) | two runs and a restart |

`DCS.setUserCallbacks` is additive and has no removal, so a callback set a snippet
adds stays until DCS restarts. Each does nothing once its snippet ends or is reset.

The snippets for the Windows client are in `alone/`, run with `cargo xtask eval
<client-target> hook <file>`: `s16-menu-pick.lua` (the input-command route and the
`doAction` fallback; the numbers are read at run time), `s16-observe.lua` (what the
client sees of a kick, a ban or a slot move; arms, then reports on the next run),
`s16-chat.lua` (sends chat lines) and `s16-slot-race.lua` (E35: takes a slot and
asks to spawn in one step). A rejoin is `cargo xtask client join`, which runs
`e30-join.lua`.

## E39

The snippets in `e39/` belong to experiment E39, on spawning with a captured unit
state. The runner does not pick them up: each is run alone with
`cargo xtask eval <target> <state> <file>`, in the state its header names, on a server
with no players, and cleans up what it adds. `recon.lua` (`server`) reads the unit
database and each type's default spawn; `spawn.lua` (`hook`) spawns aircraft, ground
units and ships with a state and sets life in three ways, about 10 s of model time;
`guns.lua` (`server`) reads the full gun counts. `lua/tests/e39.lua` rehearses them on
the mock. The results are in `docs/results/2026-11-16-e39-spawn-state.md`.

## Warehouse

How a player's actions change a warehouse's inventory. The server needs a mission with
limited warehouses, packed with `cargo xtask miz <mission> <out> --limited-airport ID
--limited-warehouse ID`. The client joins with `cargo xtask client join`.

`run.sh <server> <action> [slot] [seconds] [stock]` fills the placeholders of `step.lua`
and runs it in the server's hook state. The action is `list`, `stock`, `slot`, `deslot`,
`kill` or `none`; `stock` is the count the `stock` action sets (default 100), and it copies
names only from a warehouse that lists a count of 1,000,000 or more. A `slot` only assigns
the slot: the client enters its aircraft with `alone/e31-spawn.lua`. `cargo xtask eval
<server> server farp-spawn.lua` adds a FARP 2 km north of the first airdrome whose
inventory lists an aircraft, and reads its warehouse. `lua/tests/warehouse_experiments.lua` rehearses
`step.lua` on the mock. The results are in
`docs/results/2026-11-16-warehouse-player-deltas.md`.

## Player moves

What `net.force_player_slot(player, side, slot)` does with each side, into a slot, against
a slot of another coalition, and when the player changes side just before the call. The
server needs one connected client that holds no slot; it joins with `cargo xtask client
join`.

`run.sh <server> <steps> [settle]` fills the placeholders of `moves.lua` and runs it in
the server's hook state; `moves.lua` describes the step list. A step with a `wait` side
waits for the client to change to that side and calls in the same frame; `run.sh --client
<client> <side>` runs `client-side.lua`, which makes that change with
`net.set_coalition`. `lua/tests/player_moves_experiments.lua` rehearses both on the mock.
The results are in `docs/results/2026-11-24-player-moves.md`.

## Junk

The snippets in `junk/` settle what `world.removeJunk` clears (`junk.lua`) and whether
one wreck can be removed on its own (`junk2.lua`). The runner does not pick them up:
each runs alone in the `server` state with
`cargo xtask eval <target> server <file> --timeout 500`, on a server with no players,
and needs the `s15` and `scenario` libraries in the profile. `junk.lua` takes 160 s of
model time and `junk2.lua` 312 s. Both kill what they spawn with scripted explosions
4 to 14 km from the mission's first airbase and clear their own wrecks at the end.
`lua/tests/junk_experiment.lua` rehearses them on the mock. The results are in
`docs/results/2026-11-16-clear-debris.md`.

`column-origin.lua`, `column-kill.lua` and `column-read.lua` are the reads and the kill
of the clear-debris check, `cargo xtask commands <target> --part debris`, which fills
their placeholders and runs them in the `server` state. `lua/tests/world_exit.lua debris`
runs them on the mock.

## Loadout

The snippets in `loadout/` check the player loadout reads. Each runs alone in the
`server` state with `cargo xtask eval <target> server <file>`. `player-name.lua` reads
`Unit.getPlayerName` and `getAmmo` for every player unit, and `getPlayerName` for up to
40 AI units. `ai-birth.lua` spawns one AI M-113 near the first player, lets 5 s of model
time run, and destroys it, so a recording shows that an AI birth gives no `AMMO` record.
`player-name.lua` runs with a client in an aircraft (`alone/e30-take-slot.lua`,
`alone/e31-spawn.lua`). `ai-birth.lua` spawns near that client when there is one, and
otherwise near the map origin.
`lua/tests/loadout_experiments.lua` rehearses them on the mock. The results are in
`docs/results/2026-11-17-s19-spawn-state.md`.

## FARP pairs

The snippets in `farp-pairs/` read what DCS makes of a static group of two FARPs. The
runner does not pick them up. Load a mission made with
`cargo xtask miz <template> <out> --heliport-group X,Y,2,first|each` with
`cargo xtask load <server> <out>`, then run `read.lua`, `second.lua` (server) and
`slots.lua` (hook) with `cargo xtask eval <server> <state> <file>`, and `parking.lua`
and `make.lua` through `farp-pairs/run.sh <server> parking <airbase>` and
`run.sh <server> make <try>`. Each try of `make.lua` adds FARPs that stay until the next
mission load. `lua/tests/farp_pairs_experiments.lua` rehearses them on the mock. The
results are in `docs/results/2026-11-25-farp-pairs.md`.

## Radio transmission

The snippets in `radio-transmission/` settle the arguments of
`trigger.action.radioTransmission` with a person listening in an aircraft. The runner
does not pick them up. Fill the placeholders of `case.lua` (its header lists them), one
unknown per request, and run it with `cargo xtask eval <server> server <file>` while a
mission with a blue player runs; `stop.lua` stops every case. The mock has no
`coalition.getPlayers`, so they were rehearsed under stock Lua 5.1 with stubs of the two
calls. The results are in `docs/results/2026-11-26-mission-content.md`, run 5.
