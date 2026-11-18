# nanoprobe

nanoprobe lets a program outside DCS World run a persistent campaign on a DCS server.
DCS simulates. Your program, the controller, decides what exists and what it does, over
a gRPC API, in any language. The world survives mission reloads and server restarts.

It has three parts:

- The **agent** is a DLL with a small Lua hook, loaded by DCS from the Saved Games
  profile. It reads state out of DCS and applies commands, inside a fixed time budget.
- The **sidecar** is a separate program on the same machine. It owns the campaign's
  world, stores it, and serves the API.
- **`nanoprobe`** is the command-line tool: installer, uninstaller, `doctor`, and
  inspectors.

Nothing is written into the DCS install, and no DCS binary is hooked or patched. The
integrity check is not affected by files in Saved Games.

The API is `nanoprobe.v1alpha1`. It can change in a way that breaks clients until it
leaves alpha.

## What a controller can do

| Service | Calls |
|---|---|
| Campaign | create, activate, suspend, finish, back up, export and delete campaigns; give a campaign its own mission template |
| Commands | put groups and statics, make a group from a Mission Editor group template, set orders and options, move a group between DCS and the campaign's own simulation (`Materialize`, `Dematerialize`), remove entities, set airbase owners and warehouse stock, clear wrecks |
| World | list the world at a revision, watch its changes, query an area |
| Events | subscribe to events from a sequence number, read history |
| Mission | server status and health, weather and time of the next mission, reload |
| Players | list players and the units they occupy, messages, sounds, chat, F10 marks, drawings and menu items |
| Moderation | kick, ban, unban, move a player |
| Extension | your own data and events; your own Lua, when the server's administrator allows it |

One controller at a time holds the lease and may change things. Any number of observers
read. [docs/api-clients.md](docs/api-clients.md) shows how to generate a client;
[spec/world.md](spec/world.md) defines the world model.

## What is tracked, and how fresh it is

The agent takes at most 0.3 % of real time on the simulation thread by default, and
gives itself at most 250 µs in one frame. It reads what that budget allows. Each value below
has a target period; the periods are settings (`[sampling]` in `agent.toml`).

| Value | For | When it is read | How old it can be |
|---|---|---|---|
| Position | every unit, and cargo statics | every second | 1 s. A unit that has not moved 0.5 m in three reads is read every 5 s, so it is seen up to 5 s late, once, when it starts to move. |
| Position | statics that cannot move | when first seen, and at each full enumeration (60 s) | does not change |
| Life | every unit and static | when first seen; after a hit event names it; in a slow sweep every 60 s; at a capture | about 1 s after a weapon hit. Damage that sends no event shows at the next slow sweep. |
| Life | units players occupy | every 5 s | 5 s |
| Ammunition | units players occupy | once when the player takes the slot, and once at takeoff | not updated in between |
| Ammunition | AI units | never | unknown, not full |
| Fuel | any unit | never | not tracked |
| Coalition and warehouse stock | every airbase | every 60 s | 60 s |
| Which objects exist | every group and static | births and deaths as they happen; a full enumeration every 60 s | a kill is seen at once for ground units and statics (below) |

These are targets, not promises. When the budget cannot meet every target, the server
is not slowed: rates fall in a fixed order. Warehouses fall first, then life, then
positions. Positions a controller subscribed to fall last. The server status reports the
period each tier achieved, so a controller can see how old its data is. The slow sweep
of life took 98 s, not 60, on a loaded server.

Measured on a dedicated server under Wine with 2,000 objects, one ground group in five
moving, across the runs of three results documents: the mean share was 0.19 to 0.23 % of
real time, and 0.19 to 0.27 % of frames went over 250 µs. The median interval between an
object's positions was 0.993 to 1.008 s
([s11](docs/results/2026-11-14-s11-budget-and-baseline.md),
[s14](docs/results/2026-11-14-s14-command-path.md),
[s19](docs/results/2026-11-17-s19-spawn-state.md)).

A controller's commands have their own budget, 2 ms a frame by default. A command is not
cut short: one large spawn can take longer than the budget in its frame, and the server
status counts it.

## Behavior to know

**Deaths.** A killed ground unit or static is reported dead at once, from a read. DCS
sends a ground unit's own death events only when its wreck explodes, which came 0 to
103 s later in tests. An aircraft or a ship is reported dead by its death event, which
came at once in every test. A cargo crate killed by gunfire sends no event and is reported
as missing.

**Removal by script.** DCS sends no event for a unit a script removes. The platform
reports a removed ground unit or static as removed, and anything else as missing.

**Damage without an event.** DCS sends a hit event for weapon hits, splash damage and a
wreck's explosion. A scripted explosion, or a script that sets a unit's life, sends
none; the slow sweep finds the change.

**Rearming.** The platform does not see a rearm. No event came when a truck rearmed an
AI unit; whether DCS sends one when ground crew rearm a player's aircraft is unverified.
A player's ammunition is read again at the next takeoff. Flares and chaff cannot be
read.

**Units that left without a death.** A member that DCS reported missing or removed is
not created again by a reload, a restart or a restore, because its death report may
have been lost. Its group comes back without it. The API marks such a unit `left`; the
controller releases it with `ResolveMember` or removes the group.

**Restarts.** The sidecar checkpoints positions, life and warehouse stock every 5 s. When
DCS comes back after a restart, or a campaign is activated, each group on the loaded map
is spawned from its last stored state and its order continues, so a convoy goes on to
its destination. After a restart DCS runs the mission its own settings name, not the
campaign's; the campaign's next `Reload` loads its own. After a DCS crash the restored
values are up to one checkpoint old. The sidecar exits when DCS restarts, so that
whatever runs it starts it again; the restore happens once it is back
([docs/install.md](docs/install.md)). Ammunition is not
stored. The platform never starts, stops or restarts DCS: it reports DCS as healthy,
hung or gone, and restarting is the host's job.

**Missions.** The sidecar writes each campaign's mission from a template that the
server's administrator saves in the Mission Editor, one per map. The project ships no
mission. The copy changes the date, the start time, the weather, and the dynamic-spawn
and hot-start settings of the airports the controller names. An airport with dynamic
spawn turned on gets a limited aircraft stock, which the controller sets with
`SetWarehouseStock`. Slots, zones, triggers and placed units stay as saved, and placed
units are not managed. See [docs/mission-templates.md](docs/mission-templates.md).

**Ruins.** A campaign static that is destroyed stays destroyed and stays in the world.
After a reload or restart DCS shows a dead static in its place. How a restored ruin
looks depends on its type, and the wreck of the one ground vehicle tested, a BRDM-2,
could not be kept this way ([spec/world.md](spec/world.md), "Ruins").

**Wrecks.** `ClearDebris` removes wrecks, craters and debris in a volume. A `Column` is
a square on the map that clears at every height, so the caller needs no terrain height;
a `Sphere` and a `Box` take altitudes above sea level. The edge is not exact: wrecks
within about 30 m of a small volume go too, and a wreck that is still burning stays.

**Lua.** Extensions and raw Lua are off unless the administrator sets `[lua] raw = true`
in the agent's settings file on the server. No API call can turn them on.

## Limits

- DCS simulates on one core, and the capacity is DCS's. The platform is measured at
  500, 1,200 and 2,000 objects.
- Fuel is not tracked. Ammunition is known only for units players occupy.
- The position of cargo that an aircraft carries is unverified.
- The API has no TLS. Off loopback it needs tokens, and the network must be trusted.
- No client packages are published. Clients generate code from the release's schema.
- Mission templates come from the administrator; the project ships none.
- A group can be made from a Mission Editor group template (`PutGroupFromTemplate`), for
  ground groups only. Static templates and payload presets are not read
  ([docs/mission-templates.md](docs/mission-templates.md), "Group templates").

## Where it is verified

Every claim about DCS in [spec/](spec/) is tagged with how it is known, and every
measurement in [docs/results/](docs/results/) names its script, DCS build, platform and
date.

| Platform | Status |
|---|---|
| DCS dedicated server under Wine on Linux, in a container | The development target. The measurements and live tests ran here: DCS 2.9.31.29434, `wine-10.0`. |
| DCS on Windows, hosting a session from the client | The agent's overhead, mission reloads and the kill switch ran here. |
| DCS dedicated server on Windows | Unverified. No test has run on one. |

[docs/acceptance.md](docs/acceptance.md) maps the project's acceptance criteria to
their evidence and lists what is not yet shown.

## Get started

1. [Install](docs/install.md) the agent into the DCS profile and run the sidecar.
2. Save a [mission template](docs/mission-templates.md) for each map you use.
3. Run your [first campaign](docs/first-campaign.md) with one of the examples in
   [examples/](examples/): Rust, Python and Kotlin.
4. Generate a client for [your language](docs/api-clients.md).

`nanoprobe doctor` checks an install from outside DCS. After a DCS update it names each
DCS function the platform calls and no longer finds.

## Build from source

Every tool comes from `mise.toml`; the Rust toolchain is pinned in
`rust-toolchain.toml`.

```sh
mise install
mise run setup      # once: the secret-scanner hook
mise run check      # what CI checks, on the host
mise run package    # the release archives, in dist/
```

The host tests run on macOS, Linux and Windows. The agent DLL is built for Windows, and
cross-built on macOS and Linux. The development and test tooling
(`cargo xtask`) reads its targets from a file outside the repository and is not part of
a release.

## License

MIT. See [LICENSE](LICENSE).
