# Install layout

Everything the platform installs lives in one DCS Saved Games profile, `<profile>`, the
folder `lfs.writedir()` returns. Nothing goes into the DCS install.

```
<profile>/Scripts/Hooks/nanoprobe-hook.lua   loader; the same file in every version
<profile>/Config/autoexec.cfg                one block merged in by install
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
- **Undeploy** removes the `autoexec.cfg` block (below), the loader, the `nanoprobe`
  folder, and each folder listed in `install.txt`, deepest first, if it is empty.
  `install.txt` lists, one per line and relative to `<profile>`, the folders on the
  loader's path that did not exist before the first deploy, and `Config` when install
  created it. After undeploy the profile is as it was before the first deploy.
- **Install**, the release's `nanoprobe install`, is deploy, then the token (as
  "Development only" describes for deploy), then the `autoexec.cfg` merge. Running it
  again with the same release changes nothing. **Uninstall** is undeploy.

## autoexec.cfg

DCS runs `<profile>/Config/autoexec.cfg` at start in its `config` state, as plain Lua
(ED-CODE). Its `net.allow_unsafe_api` and `net.allow_dostring_in` keys gate
`net.dostring_in` (LIVE, docs/results/2026-11-11-first-dll-and-spikes.md, E2). Install
appends one block that adds `userhooks` to the first and `server` to the second when
the file runs, and keeps every name earlier lines put there:

```lua
-- nanoprobe: begin. `nanoprobe uninstall` removes this block.
do
  local function allow(list, name)
    if type(list) ~= 'table' then list = {} end
    for i = 1, #list do
      if list[i] == name then return list end
    end
    list[#list + 1] = name
    return list
  end
  net.allow_unsafe_api = allow(net.allow_unsafe_api, 'userhooks')
  net.allow_dostring_in = allow(net.allow_dostring_in, 'server')
end
-- nanoprobe: end
```

With the keys set, the hook reaches `server` and the sandboxed mission environment does
not (LIVE, E2, with the two keys assigned as plain lists). That the block's form has the
same effect is UNVERIFIED until the release's acceptance run.

The begin line carries `[new file]` when install created the file, and `[added line
break]` when install added a line break before the block because the file did not end
with one. Uninstall removes the block, the line break it added, and the file if it
created it and nothing else is left. Every other byte of the file stays as it was. An
edited block, or a begin line without an end line, is refused by both and left for the
user. The tools handle the file's bytes only to find and cut the block. `doctor` reports
the block, and each later assignment to either key that drops the name the platform
needs or that it cannot read, by line number and key, and nothing else from the file.

## Release

A release is two archives, the agent's PDB (`nanoprobe-<version>-agent.pdb`, which
symbolizes its crash records and is not installed), the API schema, and `SHA256SUMS`
over the five files, built by `mise run package` (`tools/stage-release.sh`) on macOS or
Linux. The schema
is `nanoprobe-<version>-schema.tar.gz`, one folder `nanoprobe-<version>-schema/` with
`buf.yaml`, `LICENSE` and `proto/`, and `nanoprobe-<version>-schema.binpb`, the file
descriptor set `buf build` makes of it, with its imports and comments. Users generate
client code from them (docs/api-clients.md). The GitHub release is the only place
they are published. Each archive holds one folder,
`nanoprobe-<version>-<platform>/`:

```
nanoprobe[.exe]                 doctor, install, uninstall, inspect, top, watch, restore
nanoprobe-sidecar[.exe]         the sidecar (spec/config.md)
install.cmd, uninstall.cmd      Windows only: run the program on a double click
install.sh, uninstall.sh        Linux only: run the program with their arguments
README.txt                      what the folder holds and the first steps
sidecar.toml                    Windows only: an example for a sidecar beside DCS
deploy/                         Linux only: the repository's deploy/ folder
LICENSE
payload/nanoprobe-hook.lua      the loader
payload/nanoprobe_agent.dll     the agent DLL
payload/lua/hook.lua, log.lua, shim.lua
```

`install.sh` and `uninstall.sh` run `nanoprobe install` and `nanoprobe uninstall` with
the program beside them, and pass on every argument and the program's exit code.
`deploy/` holds the sidecar's `Dockerfile`, a compose overlay and an example
`sidecar.toml` for a DCS server in a container. The image is built from the archive's
`nanoprobe-sidecar`, where it runs; no image is published. Each example `sidecar.toml`
is one the sidecar accepts once its placeholders are replaced. The Windows archive's
`README.txt` and `sidecar.toml` end their lines with CR LF.

No file in an archive holds a path of the machine that built it: the builds map the
checkout, cargo's registry and the toolchain to `nanoprobe`, `cargo` and `rust`
(`tools/remap.sh`), and the archive test fails on the builder's home folder. The PDB is
not covered: the linker writes into it the absolute path of every object file and
library it read.

The sidecar is not installed: the user runs it from the unpacked folder or from the
image, with a `sidecar.toml` of their own. `nanoprobe install` and `uninstall` write no
`sidecar.toml`, and place and remove nothing of the sidecar: its stores are in its own
folder (spec/config.md) and stay. The exception is a folder the user puts
inside `<profile>/nanoprobe`, the mission folder included, which undeploy removes with
the rest.

The platforms are `windows-x86_64` (a zip) and `linux-x86_64` (a tar.gz whose two
programs are static). The payload is the same in both: a Linux server runs DCS under Wine.
`nanoprobe install` reads `payload/` beside the program. It installs into the one DCS
profile it finds, or the one `--profile` names: a folder named `DCS` or `DCS.<branch>`
in `%USERPROFILE%\Saved Games` on Windows, or in `<prefix>/drive_c/users/*/Saved Games`
for a Wine prefix (`--wine-prefix`, `WINEPREFIX`, or `~/.wine`). On Linux and macOS,
install and uninstall run as the user who owns the profile folder and refuse to run as
another: DCS runs as that user and must read what install writes. On Windows the
folder's access list decides.

## Doctor

`nanoprobe doctor` checks an install from outside DCS and writes nothing. It prints one
line per check, marked `ok`, `fail` or `note`, and exits 0 when nothing failed, 1 when a
check failed, and 2 when it has no profile to check: none found, or, without
`--profile`, none that holds the loader. Without `--profile`
it checks every profile found, as install finds them, that holds the loader. It also checks
the per-map templates in the mission folder: `--missions DIR` names the folder the
sidecar's `[missions] dir` names, as the machine doctor runs on sees it, and defaults to
`nanoprobe/missions` in the profile. Each `--theater NAME` names a theater whose
template must be there.

| Check | Fails when | A note when |
|---|---|---|
| Loader | it is missing | it differs from this release's |
| Kill switch | `nanoprobe/disable` exists | |
| Manifest and active version | no `active`; `SHA256SUMS` missing; a file missing, changed or unlisted; the folder's build does not match its `SHA256SUMS` | `previous` is not installed |
| Token and `agent.toml` | the token is missing or empty, or (Linux, macOS) owned by another user than the profile; `agent.toml` does not parse | |
| `Logs/dcs.log`, newest start | the newest start has no platform line and began after the loader was written; a platform `ERROR` line | no log; a newest start from before the install; the hook runs another build than `active` |
| `logs/hook.log` and `logs/shim.log`, newest start | an `error` record; a `capabilities` record with a name absent; no `hook loaded` record of the newest session and no `hook.log.1` | no shim record yet in that start; the start record rotated out; the newest start ran no hook, and the logs are not checked |
| `autoexec.cfg` | the block is edited or cut short; an assignment after the block, or anywhere without one, sets a list that lacks the needed name | no block; a value doctor cannot read |
| Per-map templates | the folder holds no `<theater>.miz`, or none for a `--theater`; a template the sidecar cannot write a shell from (spec/persistence.md, "Mission shells"); the mission folder does not read, except when `nanoprobe/missions` is absent and neither `--missions` nor `--theater` is given | a `.miz` whose name is no theater's; no `nanoprobe/missions` in the profile and no `--missions` or `--theater` |

The newest start is the part of `dcs.log` after its last `=== Log opened UTC <time>`
line, which also says when it began (the log's modification time when there is no such
line). Its `session <id> build <build>` line names the hook's `hook loaded` record of that
session in `hook.log`; doctor reads the hook's records from there and the shim's from that
record's `wall` on, across each log and its `.1`. Without a `dcs.log` it reads them from
the last `hook loaded` record.

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
which comes at least once a second, also while a sidecar is attaching, or on every
second frame when frames are over a second apart and a sidecar is attached
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

`cargo xtask deploy --faults` builds the agent with its `faults` feature, which adds
fault injection (spec/faults.md), into `target/faults`, and installs that DLL as a new
version with its own build hash. A release never carries it, and `tools/checkdll.sh`
refuses it. A deploy without `--faults` installs the shipping DLL again.

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
