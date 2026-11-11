# Install layout

Everything the platform installs lives in one DCS Saved Games profile, `<profile>`, the
folder `lfs.writedir()` returns. Nothing goes into the DCS install.

```
<profile>/Scripts/Hooks/nanoprobe-hook.lua   loader; the same file in every version
<profile>/nanoprobe/manifest.txt             which version is active
<profile>/nanoprobe/install.txt              folders the install created
<profile>/nanoprobe/disable                  kill switch, created by hand or by a tool
<profile>/nanoprobe/logs/                    JSON-lines logs (spec/log.md)
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

## Kill switch

If `<profile>/nanoprobe/disable` exists when DCS starts, the loader runs nothing. The
hook looks for it once a second of real time; once it appears, the hook stops all work
until DCS restarts.

## Development only

`cargo xtask deploy --dev` also installs `<profile>/Scripts/Hooks/nanoprobe-dev.lua`,
the development mission loader, and `<profile>/nanoprobe/dev/missions/`. A deploy
without `--dev` removes both, and undeploy removes them. A release never carries them.
