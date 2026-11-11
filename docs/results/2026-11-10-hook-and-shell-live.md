# Hook and shell mission on a dedicated server

| | |
|---|---|
| Date | 2026-11-10 (02:01–02:16 UTC on 2026-11-11) |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Scripts | `cargo xtask deploy <target> --dev`, `restart`, `load`, `bundle`, `undeploy` (`crates/xtask/src/live.rs`); `lua/hook/nanoprobe-hook.lua`, `lua/agent/hook.lua`, `lua/dev/mission-loader.lua` |
| Builds | `29fa9effb7e6c8ff` and `2fdfd4434e8a106e` (failed), `ada3838fe6ba3b0c` (passed) |
| Result | PASS on the third build |

No player was connected at any restart.

## The hook in a running DCS

Each restart: the hook's session marker reached `dcs.log` 66 s after `docker restart`.
With build `ada3838fe6ba3b0c` the hook logged, in order, `hook loaded` (generation 0),
`mission load begin` (generation 1) 4 s later, and `simulation start` with
`paused: true` 20 s after that. The server's own mission started paused, with no pause
callback.

Two facts about the gui state, both LIVE, found by the two failed builds:

- A file handle from `io.open` has no `seek` method. Build `29fa9effb7e6c8ff` failed at
  `log.lua:125: attempt to call method 'seek' (a nil value)`.
- `file:write` returns nil on success, where stock Lua 5.1 returns true. Build
  `2fdfd4434e8a106e` wrote its first record and then treated the nil as a failure.

The log writer now uses neither, the strict mock reproduces both, and
`lua/tests/hook.lua` passes against the mock.

Also seen: a chunk loaded with `loadfile` in the gui state is named
`[string "<path>"]`, not `@<path>`.

## A generated shell mission

`cargo xtask miz templates/caucasus.miz <out> --start-time 43200` wrote the shell, and
the development loader loaded it with `net.load_mission(<path>)`:

- `net.load_mission` returned `0`. The load began within the same second.
- `Dispatcher: loadMission Done` 15 s after the call. The theatre changed from
  Afghanistan to Caucasus. The mission started paused.
- `dcs.log` showed no error about the mission. It showed the same
  `No suitable driver found to mount ... vfstextures` errors that every start of this
  server shows, and terrain collision warnings.

So the server loads a mission whose `options` member holds only `difficulty` and
`format` (`templates/README.md`).

## Bundle

`cargo xtask bundle` on this session: the `dcs.log` slice began at the session marker
and held 713 lines; one IP address, the server's own public address in
`Registering server as <ip>:10308`, was replaced by a hash.

Afterwards `undeploy` removed every platform file, including `Scripts/Hooks`, which the
first deploy had created, and a last restart returned the server to its own mission.
