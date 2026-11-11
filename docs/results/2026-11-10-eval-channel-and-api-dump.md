# Eval channel and API dump on a dedicated server

| | |
|---|---|
| Date | 2026-11-10 (02:45–03:30 UTC on 2026-11-11) |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Scripts | `cargo xtask deploy <target> --dev`, `restart`, `load`, `eval`, `api-dump` (`crates/xtask/src/eval.rs`, `probes.rs`); `lua/dev/eval-hook.lua`, `lua/dev/eval-prelude.lua`, `lua/probes/tools/api-dump.lua`, `lua/probes/11-surface.lua` |
| Mission | A shell generated from `templates/caucasus.miz`, no client connected |
| Result | The channel reached every state. The listings are in `lua/mock/api/`. |

No player was connected at any restart. Everything below is LIVE on this build and
platform.

## The states

`cargo xtask eval <target> <state> <file>` ran a snippet in each state and read back its
result.

| State | How it was reached | `io`, `os`, `lfs` | `debug` | `package` | `net` | `DCS` | `world` | Best clock |
|---|---|---|---|---|---|---|---|---|
| `hook` | the eval hook's own state | table | table | table | table | table | nil | `DCS.getRealTime` |
| `gui` | `net.dostring_in` | table | table | table | table | table | nil | `DCS.getRealTime` |
| `server` | `net.dostring_in` | table | table | table | nil | table | table | `DCS.getRealTime` |
| `scripting` | `net.dostring_in` | the same state as `server` | | | | | | |
| `mission` | `net.dostring_in` | nil | nil | nil | nil | nil | nil | none |
| `export` | `net.dostring_in` | table | table | table | nil | nil | nil | `os.clock` |
| `config` | `net.dostring_in` | table | table | table | table | nil | nil | `os.clock` |
| sandbox | `a_do_script` in `mission` | nil | table | nil | table | nil | table | none |

- **A hook's globals are not the `gui` state's.** `tostring(_G)` in the hook differs
  from `net.dostring_in('gui', 'return tostring(_G)')`. A global set in the hook is
  absent in `gui`, and a global set in `gui` is absent in the hook. `tostring(DCS)`
  differs too. The channel therefore names the hook's own state `hook` and keeps it
  apart from `gui`.
- A request to `scripting` found the prelude that a request to `server` had installed:
  the two names reach one state.
- `a_do_script` is a function in the `mission` state and nowhere else. `mission` has no
  clock at all.
- The sandbox was reached while the mission was paused at model time 0. The chunk
  returned its reply and a spare value, and the reply came back in the second position.
- `onSimulationFrame` kept arriving while the server was paused with no client: 1,549
  frames in the first 76 s of real time, all of them paused. No frame arrived during
  any mission load.
- A reply of 1.3 MB crossed `net.dostring_in` in 16,384-byte pieces with its length
  intact. The largest string that crosses whole was not measured.

## What `net.dostring_in` returns

From the hook, into `server` (`lua/probes/10-dostring.lua`). Every call returned two
values, a string and a boolean, and none raised.

| Chunk | First value | Second value |
|---|---|---|
| `return 1` | `1` | true |
| `return 1.5` | `1.5` | true |
| `return 'text'` | `text` | true |
| `return 'a', 'b'` | `a` | true |
| `return true`, `return false`, `return nil`, `return`, `return {}` | empty string | true |
| `return (` | the compiler's message | false |
| `error('probe raise')` | the message with its position | false |
| `error('probe raise', 0)` | `probe raise` | false |
| `error({})` | empty string | false |

An unknown state name (`hooks`, `userhooks`, `net`, `lobby`, `main`) returned no value
and raised nothing. The chunk received no arguments. Neither `net.allow_unsafe_api` nor
`net.allow_dostring_in` was set in the `config` state.

## The API dump

`cargo xtask api-dump <target>` ran `lua/probes/tools/api-dump.lua` in each state and
wrote one listing per state. A line is `path : type`, with the value after a number or a
boolean. No string's value is written. Two dumps of the `hook` state, taken either side
of two DCS restarts, were identical.

| State | Lines |
|---|---|
| `hook` | 8,032 |
| `gui` | 12,003 |
| `server` | 9,732 |
| `mission` | 654 |
| `export` | 311 |
| `config` | 490 |
| sandbox (`sse`) | 1,365 |

What the listings show:

- `Sim` is the same table as `DCS`, in the `hook` state and in the `server` state.
- `Unit` has no `getPoint`, `getPosition`, `getVelocity`, `inAir`, `isExist` or
  `destroy` of its own. They are members of `Object`, which `Unit.parentClass_` names.
- `world.eventHandlers` and `world.onEvent` are ordinary members of `world` in the
  `server` state and in the sandbox.
- `terrain.convertMetersToLatLon` is in the `hook` and `gui` states. The `server` state
  has `coord` and no `terrain`.
- The `server` state holds 1,650 globals and the `hook` state 1,571. Most are tables of
  the unit database, which the dump writes with their size alone.

The strict mock reads these files as its listing (`lua/mock/listing.lua`), so a name the
mock accepts is a name this build has.
