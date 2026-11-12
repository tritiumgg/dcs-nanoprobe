# Install layout

Everything the platform installs lives in one DCS Saved Games profile, `<profile>`, the
folder `lfs.writedir()` returns. Nothing goes into the DCS install.

```
<profile>/Scripts/Hooks/nanoprobe-hook.lua   loader; the same file in every version
<profile>/nanoprobe/manifest.txt             which version is active
<profile>/nanoprobe/install.txt              folders the install created
<profile>/nanoprobe/running.txt              the build the running hook loaded
<profile>/nanoprobe/agent.toml               the agent's settings (spec/config.md)
<profile>/nanoprobe/token                    the handshake token (spec/config.md)
<profile>/nanoprobe/disable                  kill switch, created by hand or by a tool
<profile>/nanoprobe/reload                   shim reload request, created by a tool
<profile>/nanoprobe/logs/                    JSON-lines logs (spec/log.md), and crash.log
<profile>/nanoprobe/versions/<build>/
    nanoprobe_agent_<build>.dll              the agent DLL
    lua/hook.lua                             the hook body the loader runs
    lua/log.lua                              the Lua log writer
    lua/shim.lua                             the shim, run in the server state
    SHA256SUMS                               sha256 of every other file in the folder
```

`SHA256SUMS` holds one `<sha256>  <path>` line per file, sorted by path. `<build>` is
the first 16 hex digits of the SHA-256 of that text with the DLL listed as `agent.dll`,
so it changes when any file in the version changes. The DLL carries the build in its
name because Windows does not let a loaded DLL be overwritten.

## Manifest

`manifest.txt` is ASCII lines of `key=value`:

```
active=<build>
previous=<build>
```

`previous` is absent after the first install. The loader reads only `active`.

## Operations

- **Deploy** writes the version folder (skipped when it exists and its checksums match),
  the loader, and the manifest, in that order. The old `active` becomes `previous`, and
  every version folder other than the two named, and the one `running.txt` names, is
  removed. The hook writes `running.txt` (`build=<build>`) when it starts, so the folder
  of a DLL that is still loaded stays after shim reloads of later deploys. Deploying the active
  version again changes nothing.
- **Rollback** swaps `active` and `previous`. It refuses when there is no `previous`.
- **Undeploy** removes the loader, the `nanoprobe` folder, and each folder listed in
  `install.txt`, deepest first, if it is empty. `install.txt` lists, one per line and
  relative to `<profile>`, the folders on the loader's path that did not exist before
  the first deploy. After undeploy the profile is as it was before the first deploy.

## Agent

The hook opens the DLL in its own state with `package.loadlib` and the absolute path,
checks that its ABI is the one the hook was written for, and calls `configure` with
`<profile>/nanoprobe/`. The first open in the process pins the DLL, so it stays loaded
until DCS exits. Opening again in the same state returns the same table.

`configure` opens `logs/agent.log` and registers a handler that appends one JSON line to
`logs/crash.log` for each native exception raised inside the DLL, or anywhere while an
agent call runs, eight at most per process. The handler records and passes the
exception on. Its `data` holds the exception code and address, `module+offset` for the
address and for each frame of the stack, the breadcrumb, and `pdb`: the GUID and age of
the PDB that symbolizes this build.

The hook then calls `start` with the DCS version. `start` reads `agent.toml` and the
token, creates and maps the transport file, writes its header, and listens for the
handshake (spec/transport.md). It runs once per process. A missing `agent.toml` is
every default; one that does not parse, or has an unknown key, stops `start`, and the
hook carries on without the link. Without a usable token the agent writes the file but
does not listen, and sets `NO_HANDSHAKE`.

On every `onSimulationFrame` the hook passes DCS real time, model time and
`DCS.getPause()` to the agent's pump. It tells the agent each lifecycle change:
`onMissionLoadBegin`, `onSimulationStart` and `onSimulationStop`.

### Shim

On every `onSimulationStart` the hook makes one `net.dostring_in('server', …)` call. The
first one in a process opens the DLL in the server state with `package.loadlib` and
runs the version's `lua/shim.lua` there, behind a guard, the global
`__nanoprobe_shim`, so that later calls only start it. The server state outlives
missions (LIVE, docs/results/2026-11-11-first-dll-and-spikes.md, E4), so the shim loads
once per process. Each start re-registers what a mission
load cleared.

The shim's chunk takes the agent's table, the install root, its own version folder
and the shim it replaces, and returns its own table. It refuses an agent of another
ABI.

**Shim reload.** When `<profile>/nanoprobe/reload` exists, the agent removes it and the
hook runs the `lua/shim.lua` of the version the manifest names as active, over the
loaded shim, and starts it. The new shim is given the old one and carries its state
on. The DLL stays the one loaded: a new DLL needs a DCS restart. A shim that raises
while it loads leaves the old one in place.

### Capabilities

The hook in the hook state and the shim in the server state each check, once, that
every DCS function and table the platform calls in that state is present, and write
the result as one `capabilities` record (spec/records.md, "Capabilities"). A missing
name is reported, not fatal.

## Kill switch

If `<profile>/nanoprobe/disable` exists when DCS starts, the loader runs nothing.

While the agent's link is open, the agent looks for the file on its system-call frame,
which comes at least once a second, also while a sidecar is attaching
(spec/transport.md, "Handshake"). When it appears the agent stores `DISABLED` in the
status block, sends the reason and a last `LIFECYCLE` record if a sidecar is attached,
closes the connection and the listener on that frame, and makes no further call. The
last `LIFECYCLE` passes the control queue's limit, and no `HELLO` follows it. If the
ring has no room for these records they are not sent, and the agent logs it; the
status block's `DISABLED` is the authority. Every
agent call but `status` then does nothing, and the hook stops all work. Without the
link, the hook looks for the file itself once a second of real time.

Either way the work stops within 2 s of the first frame after the file appears. No
frame runs while a mission loads (LIVE, docs/results/2026-11-11-server-gate.md), so a
file created during a load is seen after it.

## Development only

`cargo xtask deploy --dev` also installs these files in `<profile>`:

```
Scripts/Hooks/nanoprobe-dev.lua    the development mission loader
Scripts/Hooks/nanoprobe-eval.lua   the eval hook (spec/eval.md)
nanoprobe/dev/lua/prelude.lua      the prelude the eval hook sends to each state
nanoprobe/dev/lua/<name>.lua       every Lua file in lua/dev/lib/ and lua/scenarios/
nanoprobe/dev/missions/            missions the mission loader or a test session loads
nanoprobe/dev/link/                test programs and their output, written and removed
                                   by `cargo xtask link`
nanoprobe/dev/eval/in/             eval requests
nanoprobe/dev/eval/taken/          eval requests the hook has taken
nanoprobe/dev/eval/out/            eval results
```

Deploy also creates `nanoprobe/token` from 32 random bytes when there is none, readable
only by the profile's owner on Linux and macOS; on Windows the profile folder's access
list decides. When the target profile names a `map_path`, deploy sets that key in
`agent.toml`: it creates the file, or keeps every other key of the one there. It prints
neither file. `cargo xtask reload` requests a shim reload and waits for the hook's
answer.

A file is written only when its content differs, and a file in `nanoprobe/dev/lua/`
that the install does not hold is removed. The files are not part of the version and
do not change the build. A deploy without `--dev` removes both hooks and the
`nanoprobe/dev` folder, and undeploy removes both hooks. A release never carries them.
