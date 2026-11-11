# Install layout

Everything the platform installs lives in one DCS Saved Games profile, `<profile>`, the
folder `lfs.writedir()` returns. Nothing goes into the DCS install.

```
<profile>/Scripts/Hooks/nanoprobe-hook.lua   loader; the same file in every version
<profile>/nanoprobe/manifest.txt             which version is active
<profile>/nanoprobe/install.txt              folders the install created
<profile>/nanoprobe/disable                  kill switch, created by hand or by a tool
<profile>/nanoprobe/logs/                    JSON-lines logs (spec/log.md), and crash.log
<profile>/nanoprobe/versions/<build>/
    nanoprobe_agent_<build>.dll              the agent DLL
    lua/hook.lua                             the hook body the loader runs
    lua/log.lua                              the Lua log writer
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
  every version folder other than the two named is removed. Deploying the active
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

## Kill switch

If `<profile>/nanoprobe/disable` exists when DCS starts, the loader runs nothing. The
hook looks for it once a second of real time; once it appears, the hook stops all work
until DCS restarts.

## Development only

`cargo xtask deploy --dev` also installs these files in `<profile>`:

```
Scripts/Hooks/nanoprobe-dev.lua    the development mission loader
Scripts/Hooks/nanoprobe-eval.lua   the eval hook (spec/eval.md)
nanoprobe/dev/lua/prelude.lua      the prelude the eval hook sends to each state
nanoprobe/dev/lua/<name>.lua       every Lua file in lua/dev/lib/ and lua/scenarios/
nanoprobe/dev/missions/            missions the mission loader loads
nanoprobe/dev/eval/in/             eval requests
nanoprobe/dev/eval/taken/          eval requests the hook has taken
nanoprobe/dev/eval/out/            eval results
```

A file is written only when its content differs, and a file in `nanoprobe/dev/lua/`
that the install does not hold is removed. The files are not part of the version and
do not change the build. A deploy without `--dev` removes both hooks and the
`nanoprobe/dev` folder, and undeploy removes both hooks. A release never carries them.
