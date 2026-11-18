# Acceptance checklist

A release is accepted when the six criteria below hold on a DCS dedicated server under
Wine and on a Windows machine that hosts a session from the DCS client, each from a
clean install of the staged release. This page maps each criterion to the evidence the
repository holds and lists what the acceptance run must still show.

A DCS dedicated server on Windows is unverified: no test has run on one. The release
notes say so.

## The run

Both platforms follow the same steps. Every result is committed to `docs/results/` with
its script, the DCS build, the platform and the date.

1. **Stage.** `mise run package` stages the release in `dist/` and checks it. These
   cross-built files are the release (spec/install.md, "Release"), and the run tests
   them and no others: record `SHA256SUMS`, and on each machine check the copied
   archive against its line before unpacking it.
2. **Baseline.** With DCS stopped and no nanoprobe file in the profile, record the
   profile's file list with sizes and checksums, `Config/autoexec.cfg` byte for byte,
   and the DCS install's file list with sizes and modification times.
3. **Install.** One installer run from the unpacked archive, on the machine that holds
   the profile. "How the staged files reach each platform" below says where and as
   whom. Record the name of the version folder it made, `nanoprobe/versions/<build>`:
   it is the build ID that step 8 compares against.
4. **Check.** Start DCS and a mission. `nanoprobe doctor` exits 0 and reports every DCS
   name present in the hook state and in the server state.
5. **Sidecar.** Run the archive's `nanoprobe-sidecar`: on Linux in the image built from
   the archive with `deploy/`, on Windows as a program with the archive's example
   `sidecar.toml` filled in. `nanoprobe doctor --sidecar-config`, or `--missions` and
   `--template-dir` for a container, exits 0.
6. **Criteria on the staged files.** Criteria 4 and 5, the `doctor` half of 6, and the
   sidecar and controller kills of 3, with nothing but the release in the profile.
7. **Uninstall.** One uninstaller run. The profile's file list and `autoexec.cfg` equal
   the baseline, the sidecar's folder is untouched, and the DCS install's file list
   equals its baseline.
8. **Criteria that need the test tooling.** Criterion 1, the DCS kill of 3 and the
   conformance suite of 6, each on its own development deploy of the staged DLL, as
   "What the tools can drive" below describes. To symbolize a crash record from such a
   deploy, copy `dist/private/<signature>/nanoprobe.pdb` to the `nanoprobe.pdb` path the
   deploy prints.
   Afterward the profile is checked against the baseline again.

### How the staged files reach each platform

**Wine server.** Copy the Linux archive to the server and unpack it outside the Wine
prefix. Run `./install.sh --wine-prefix <prefix>` on the machine whose file system
holds the prefix, as the user who owns the profile folder; the program refuses any
other user. With DCS in a container that is one of:

- on the host, against the folder the container mounts as its prefix, as the user ID
  that owns it there (`sudo -u '#<uid>'` when the login account is another user);
- inside the DCS container, as the user DCS runs as, with the unpacked archive on a
  path the container sees. The programs are static and need nothing in the image.

The project's deploy tooling is not used for this step: the step tests the archive's
installer.

**Windows.** The run copies the staged Windows archive to the machine over SSH, into a
folder outside the profile and outside the DCS install, checks it against its line in
`SHA256SUMS`, and unpacks it there. It runs `install.cmd` from the unpacked folder as
the user who owns the profile, with standard input empty so that its closing `pause`
returns. The script's exit code is not the program's: the run keeps the script's
output with the result and judges the install by these checks of the profile:

- `Scripts/Hooks/nanoprobe-hook.lua` equals the archive's `payload/nanoprobe-hook.lua`;
- `nanoprobe/versions/<build>/nanoprobe_agent_<build>.dll` and the folder's
  `lua/hook.lua`, `log.lua` and `shim.lua` equal the payload's files, and the folder's
  `SHA256SUMS` matches them;
- `nanoprobe/manifest.txt` names `<build>` as active, `nanoprobe/token` exists and is
  not empty, and `nanoprobe/install.txt` lists the folders the install created, set
  against the baseline (the results say whether it lists none);
- `Config/autoexec.cfg` holds the install block in the form spec/install.md gives
  ("autoexec.cfg"), and the rest of the file is as it was.

Steps 4 to 6 follow with the archive's `nanoprobe.exe` and `nanoprobe-sidecar.exe`.
Step 7 runs `uninstall.cmd` the same way, and the copied archive and folder are removed
afterward. This shows criterion 5 on Windows. Only when the installer cannot be run
over SSH does a person at the machine run `install.cmd` and `uninstall.cmd`, and the
results say which route ran.

### What the tools can drive

The release holds no test channel. What can be shown with nothing but the staged files
in the profile: the install and the uninstall, `doctor`, the archive's sidecar, the
hello campaigns through it, and what happens when the sidecar or a controller is killed.

The measuring tools are development tooling, and each one deploys an agent before it
runs: `cargo xtask sampling` (criterion 1), `cargo xtask persistence` (the DCS kill of
criterion 3) and `cargo xtask sidecar --part conformance` (criterion 6). They need the
development mission loader and the eval channel in the profile, which are Lua files
beside the agent. The run gives each tool the staged DLL with xtask's top-level option,
from the release's commit:

```
cargo xtask --dll <the archive's payload/nanoprobe_agent.dll> sampling <target> --part mirror
```

xtask refuses the option for a command that deploys no agent, and fails a command that
was given it and deployed none. The tenth part of `sampling` deploys a build with fault
injection that it builds itself, so it cannot run on the staged DLL: with the option,
`sampling` refuses that part and its default of both parts. Name one part, `--part
mirror` on the Wine server and `--part windows` on Windows.

Each deploy prints the DLL's SHA-256, the installed version's build ID and the loader's
SHA-256. The run keeps each command's console output with its result, and the results
compare three strings:

- the DLL's SHA-256 with the archive's `payload/nanoprobe_agent.dll`;
- the build ID with the one step 3 recorded. It covers the DLL and the version folder's
  three Lua files, and not the development files, so it matches when the checkout's
  Lua is the release's;
- the loader's SHA-256 with the archive's `payload/nanoprobe-hook.lua`. The build ID
  does not cover the loader.

What those three results show is therefore exactly this: the released DLL, loader and
Lua, with the development files beside them in the profile, placed by the tooling's
deploy. They are not the archive as its installer installs it. `cargo xtask
persistence` also runs its own sidecar, built from the checkout and run in a container
it starts itself, not the archive's `nanoprobe-sidecar`; the conformance suite attaches
in the sidecar's place.

`cargo xtask persistence` and `cargo xtask sidecar` run only against a DCS server in a
container under Wine. `cargo xtask sampling` has a part for a Windows machine
(`--part windows`). So on Windows the tooling drives criterion 1 alone; the DCS kill of
criterion 3 is done there by hand with the staged files, and the conformance suite is
not run.

## 1. Overhead

*The agent's own work takes at most 0.3 % of real time on average and at most 250 µs in
the 99th-percentile frame, at 2,000 moving mixed units on a dedicated server, with the
achieved sampling rates reported. A controller's commands never take more than the
configured budget plus one command in a frame, and every frame over the budget is
attributed.*

| Evidence | Platform | Shows |
|---|---|---|
| [2026-11-14-s11-budget-and-baseline.md](results/2026-11-14-s11-budget-and-baseline.md) | Wine server, DCS 2.9.31.29434 | 2,000 objects, one ground group in five moving, runs 13 and 14: mean share 0.211 and 0.19 %; 0.21 and 0.19 % of frames over 250 µs; positions at a median 1.008 and 0.994 s (by proxy) |
| [2026-11-14-s14-command-path.md](results/2026-11-14-s14-command-path.md), [2026-11-17-s19-spawn-state.md](results/2026-11-17-s19-spawn-state.md) | Wine server | the same measurement on later builds of the agent: mean share 0.193 to 0.231 %; 0.19 to 0.27 % of frames over 250 µs; positions at a median 0.993 to 1.006 s, measured directly |
| [2026-11-14-s11-budget-and-baseline.md](results/2026-11-14-s11-budget-and-baseline.md), run 15 | Windows client hosting a session | 2,000 objects: mean share 0.055 to 0.060 %; 9 of 13,850 frames over 250 µs |
| [2026-11-15-s15-world-commands.md](results/2026-11-15-s15-world-commands.md) | Wine server | no frame held more than the budget plus one command; every over-budget frame attributed to its command |
| [2026-11-17-s20-virtualization.md](results/2026-11-17-s20-virtualization.md) | Wine server | 500 of 10,000 entities in DCS: mean share 0.276 %; 0.50 % of frames over 250 µs |

Still to show, on both platforms, with `cargo xtask --dll <staged DLL> sampling <target>
--part mirror` (on Windows `--part windows`), on the released DLL, loader and Lua with
the development files beside them ("What the tools can drive"): the mean share, the
share of frames over
250 µs, and the achieved period of each sampling tier at 2,000 objects; no frame over
the commanded budget plus one command.

Limits of the evidence:

- "2,000 moving" is measured with one ground group in five on a route. With all 2,000
  moving, DCS itself ran at 1 to 8 frames a second on the test server, and the agent's
  share is then measured against a simulation that barely runs.
- The 99th percentile is judged as at most 1 % of frames over 250 µs. The pump's
  histogram does not give the exact percentile.

## 2. Soak

*A soak of at least 8 hours, with a control run without the agent, shows no memory
growth or drift that the platform causes. The run is extended when a slope the platform
causes is unresolved or rising at its end.*

| Evidence | Platform | Shows |
|---|---|---|
| [2026-11-14-s11-budget-and-baseline.md](results/2026-11-14-s11-budget-and-baseline.md) | Wine server | 90 minutes with churn against a 60-minute control without the agent: heap floors grew about 7.2 MB an hour with the agent and 7.8 without; process memory grew equally in both |
| [2026-11-17-s21-soak.md](results/2026-11-17-s21-soak.md) | host | the soak tool, its verdict rules and its rehearsal; no result from a running DCS |

The criterion is met by the long soak's own results document, which the acceptance run
cites and does not repeat:

| Evidence | Platform | Shows |
|---|---|---|
| (the long soak's results document, to be named here when it is committed) | Wine server | the 8-hour soak, its control without the agent, and the tool's verdict |

The soak ran on an agent built before the path maps: the same code, with another build
ID ([2026-11-18-s22-release-host.md](results/2026-11-18-s22-release-host.md), "What the
path remapping changes in the agent"). A soak on Windows is not planned; the release
notes say which platform the soak ran on.

## 3. Recovery

*Killing the sidecar or a client leads to automatic recovery. Killing DCS, where the
host restarts it, leads to a restore without a person: the world comes back and a convoy
resumes its route. Until DCS is back, the platform reports it as hung or gone.*

| Evidence | Platform | Shows |
|---|---|---|
| [2026-11-13-s8-chaos-and-faults.md](results/2026-11-13-s8-chaos-and-faults.md) | Wine server | 100 kill cycles, 0 failures: the link between the agent and a test peer in the sidecar's place (`link-probe`) came back after 49 DCS kills and 51 kills of the peer. The sidecar itself was not in this test. |
| [2026-11-15-s18-persistence-live.md](results/2026-11-15-s18-persistence-live.md) | Wine server | after a sidecar kill, a mission reload and a DCS kill, a convoy came back 25 m further on after the reload and 3 m short after the DCS kill, inside the test's 100 m bound, and reached the end of its route. The test loaded the campaign's mission after the DCS kill. |
| [2026-11-15-s19-shell-reload-health.md](results/2026-11-15-s19-shell-reload-health.md) | Wine server | a DCS that stopped answering was reported hung 10.8 s after the fault. The unattended restore after a DCS kill is open there. |
| [2026-11-12-s7-agent-in-the-game.md](results/2026-11-12-s7-agent-in-the-game.md) | Wine server and Windows client | the link stayed attached through 10 of 10 mission reloads; the kill switch took effect in 250 ms on Wine and 271 ms on Windows |

Still to show:

- A killed sidecar and a killed controller: the campaign carries on, and a new
  controller takes the lease. Both platforms.
- A killed DCS that the host restarts, with `cargo xtask --dll <staged DLL> persistence`
  on the released DLL, loader and Lua with the development files beside them, and the
  tool's own sidecar ("What the tools can drive"): the status reads hung or gone until
  DCS is back, then the world is restored and the convoy resumes, with no person and no
  test step loading a mission. The sidecar exits when DCS restarts and must be started
  again: on the Wine server the container restarts DCS, and the tool restarts the
  sidecar's container, not the overlay's restart policy. The results record that. The
  overlay's restart policy after a DCS restart stays unverified. On Windows the tool
  does not run, and nothing restarts a client-hosted DCS or a sidecar run as a program: the run kills DCS, starts DCS and the staged
  sidecar again by hand, and reads the restored convoy through the API. It records that
  the restart was by hand.
- The verdict `gone`, which no result shows yet.

## 4. Hello campaign

*A hello campaign works in Rust and in one other language.*

| Evidence | Platform | Shows |
|---|---|---|
| [2026-11-13-s13-api-on-the-fake-sim.md](results/2026-11-13-s13-api-on-the-fake-sim.md) | macOS, Linux, Windows; no DCS | the Rust hello passes against the fake sim on all three |
| [2026-11-15-s17-api-on-real-dcs.md](results/2026-11-15-s17-api-on-real-dcs.md) | Wine server | the Rust and the Python hello each ran to "lead reached the end of its route" against the sidecar in its container |
| `mise run check` | host | the Python and the Kotlin hello pass against the fake sidecar; the Rust and the Python hello pass through the real sidecar against the agent under stock Lua with the mock DCS (`lua/tests/sidecar.lua`) |

Still to show: the Rust hello and one other, against the staged sidecar, on both
platforms.

## 5. Install

*Install is one installer run, with no edit to the DCS install.*

| Evidence | Platform | Shows |
|---|---|---|
| [2026-11-18-s22-release-host.md](results/2026-11-18-s22-release-host.md) (`mise run package`) | host | the unpacked Linux payload installs into a made-up Wine profile, passes `doctor`, and uninstalls; the profile, its `autoexec.cfg` and a sidecar folder beside it are then as before |
| [2026-11-18-s22-release-without-pdb.md](results/2026-11-18-s22-release-without-pdb.md) (`mise run package`) | host | the release is the two archives and the schema's two files; the agent's PDB is kept beside its DLL in `dist/private/` and is in no release file |
| `crates/install/tests/install.rs`, `crates/cli/tests/setup.rs` | host | install, the `autoexec.cfg` merge and its removal, byte for byte |
| [2026-11-10-deploy-roundtrip.md](results/2026-11-10-deploy-roundtrip.md) | Wine server | the development deploy and undeploy leave an identical profile tree |
| [2026-11-11-first-dll-and-spikes.md](results/2026-11-11-first-dll-and-spikes.md) | Wine server | with the two `net.*` keys set as plain lists, the hook reaches the server state |

Still to show: steps 2, 3 and 7 of the run with the staged archive, on the Wine server
and on Windows ("How the staged files reach each platform").
No result yet holds a release install on a running DCS. The install block in
`autoexec.cfg` adds to the two keys with a function; that this form has the same effect
as the plain lists is unverified until step 4 passes (spec/install.md, "autoexec.cfg").

## 6. After a DCS update

*After a DCS update, `nanoprobe doctor` passes or names each failed capability, and the
conformance suite passes for every capability `doctor` reports as present.*

| Evidence | Platform | Shows |
|---|---|---|
| `crates/cli/tests/doctor.rs` | host | with a DCS name absent from a capabilities record, `doctor` fails and names it |
| [2026-11-15-s17-api-on-real-dcs.md](results/2026-11-15-s17-api-on-real-dcs.md) | Wine server | the conformance suite passed 9 of 9 through the real adapter |

Still to show:

- `doctor` exits 0 on the staged install with every name present. Both platforms.
- The conformance suite passes in full. Wine server only, for now.

How the suite is run: from a source checkout, with the development tooling
(`cargo xtask --dll <staged DLL> sidecar <target> --part conformance`). The suite
attaches in place of the sidecar, spawns and destroys groups and loads a mission, so it
needs a server nobody is flying on. The command does more than run the suite: it
deploys the agent with the eval channel, points the agent's `map_path` elsewhere and
restarts DCS. So it runs in step 8, on its own deploy, and not on the clean install that
steps 3 to 7 check. The suite therefore tests the released DLL, loader and Lua with the
development files beside them, not the archive as its installer installs it ("What the
tools can drive"). Afterward the run undeploys and checks the profile and
`autoexec.cfg` against the baseline of step 2 again.

The command runs only against a DCS server in a container under Wine. The suite has no
route to a Windows machine yet, so this half of the criterion is not shown on Windows.

Limits:

- No DCS update happened during development, so the criterion is shown on one DCS
  build. It is checked again at each DCS update.
- The suite has no table from a check to the DCS names it needs. When `doctor` reports a
  name absent, a person reads the suite's result against the absent names.

## Not covered by the run

- A DCS dedicated server on Windows.
- A soak on Windows.
- The conformance suite on Windows.
- Criterion 1, the DCS kill of criterion 3 and the conformance suite against the archive
  as its installer installs it: they run on the released DLL, loader and Lua with the
  development files beside them.
