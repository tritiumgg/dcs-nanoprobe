# 0.1 acceptance on a Windows DCS client hosting a session

| | |
|---|---|
| Step | S22's acceptance run (docs/acceptance.md), the half on a Windows machine whose DCS client hosts a session: a clean install of a release build's files, what of criteria 3 to 6 the route reaches, and criterion 1 on the same DLL |
| Date | 2026-11-22 |
| DCS | 2.9.31.29434, client (LIVE, the hook's `link open` line in `nanoprobe/logs/hook.log`) |
| Platform | Windows; DCS started and stopped in the logged-in desktop session through two scheduled tasks; every command from a Mac over SSH, as a standard account that can reach the profile folder and not the desktop user's temp folder |
| Files tested | The manual run of the release workflow the Wine half tested: run ID 45006509810, commit `00642c4ec65664f6c137f700f13efe51628f6803`, artifacts `release-files-00642c4ec656` and `agent-symbols-00642c4ec656`, downloaded with `gh run download 45006509810` into an empty folder, checked against the run's `SHA256SUMS` on the Mac, and the Windows archive checked again on the machine |
| `SHA256SUMS` | `7bea1a7afd4b370da61195319893b87ef04579d696442a9ac102f70fe289627a  nanoprobe-0.1.0-linux-x86_64.tar.gz`; `b9a0dbc4e0ee1b6fa00b24475c9982c1695e44ff429a0490bd87cf2503ac7d08  nanoprobe-0.1.0-schema.binpb`; `9aa445ced46da260ecf06eb2c641966b6933f297bd882226f5256e2af2dc1634  nanoprobe-0.1.0-schema.tar.gz`; `3507ee572dbbc444b65e063a1f3744d2033cb4eb2a6c8ef3b03bd254e2cb6335  nanoprobe-0.1.0-windows-x86_64.zip` |
| Agent | Build `67d8161d780bba2e`; DLL SHA-256 `d8f4d8d6fc4e389e810f6f2e18f54be9ce4389cd34832a7deac97b70d2e37dee`; loader SHA-256 `f02d0cfb7ae4ab6c32345b7496f8b83407e742e29a60dd0bb478d1bac2605400`; PDB signature `B611D9D3D74982194C4C44205044422E1` |
| Scripts | The PowerShell commands below, with the machine's names replaced; the measuring tool is `cargo xtask` at the files' commit |
| Raw results | Bundle `sampling-<target>-20261122T203018Z`; it is not committed |
| Result | Criterion 5 passes. The `doctor` half of criterion 6 passes, its server-state check with the eval channel's gates beside the release. The sidecar kill of criterion 3 passes. Criterion 1's owner targets hold; the tool's mirror check fails on one row at 2,000 (open item). Criterion 4, the controller kill and the DCS kill are not shown on Windows (below) |

Every DCS figure is LIVE on this build and platform unless tagged.

## Steps 2 to 7: the release's files alone

The profile held no nanoprobe file, and DCS was stopped. The baseline: the profile's
files outside `Logs/` with sizes and MD5 sums, 6,305 files; `Config/autoexec.cfg`'s
SHA-256 and length (25 bytes); and a SHA-256 over the DCS install's file list with
sizes and modification times, 92,882 entries.

```powershell
# on the machine, in a folder outside the profile and the install
$want = (Get-Content SHA256SUMS | Where-Object { $_ -match 'windows-x86_64.zip' }).Split(' ')[0]
(Get-FileHash nanoprobe-0.1.0-windows-x86_64.zip -Algorithm SHA256).Hash.ToLower() -eq $want
Expand-Archive nanoprobe-0.1.0-windows-x86_64.zip -DestinationPath .
cd nanoprobe-0.1.0-windows-x86_64
cmd /c 'install.cmd --profile "<profile>" < NUL 2>&1'
.\nanoprobe.exe doctor --profile '<profile>' --theater Caucasus
.\nanoprobe.exe doctor --profile '<profile>' --sidecar-config <sidecar folder>\sidecar.toml --theater Caucasus
cmd /c 'uninstall.cmd --profile "<profile>" < NUL 2>&1'
```

The SSH account is not the profile's owner, so `install.cmd` and `uninstall.cmd` ran
with `--profile`. Both are the archive's scripts; standard input was empty, so the
closing `pause` returned.

| Step | Result |
|---|---|
| 3. Install | `install.cmd` printed "installed version 67d8161d780bba2e", "created the handshake token", "added the nanoprobe block to Config/autoexec.cfg"; the script returned 0. `Scripts/Hooks/nanoprobe-hook.lua`, the DLL and `lua/hook.lua`, `log.lua`, `shim.lua` equal the payload's, the version folder's `SHA256SUMS` matches them, `manifest.txt` reads `active=67d8161d780bba2e`, the token is 64 bytes. `install.txt` lists no folder: `Scripts/Hooks` was already in the profile. `autoexec.cfg`'s first 25 bytes equal the baseline's file, followed by a line break the file lacked and the block spec/install.md gives |
| 4. Check | At the main menu, with the release alone: `doctor` exit 0, the hook started the build, the hook state has all 21 DCS names; the shim had not started, as no mission ran. With a hosted session (below): `doctor` exit 0, the hook state 21 of 21 and the server state 47 of 47 |
| 5. Sidecar | The archive's `nanoprobe-sidecar.exe run --config sidecar.toml`, the archive's example with its placeholders filled in, in a folder outside the profile, run as a program by the SSH account. It listened on loopback, and the agent attached 4 s after its start. `doctor --sidecar-config` exit 0 |
| 6. Kills | Sidecar: killed; the agent logged "detached: the sidecar's heartbeat stopped for 0.5 s" 0.51 s after the kill; started again 6.4 s after the kill; the agent attached 0.76 s after the start. Controller kill and the hellos: not run (below) |
| 7. Uninstall | `uninstall.cmd` printed "removed nanoprobe"; the script returned 0. Against the baseline: no nanoprobe file, `autoexec.cfg` equal byte for byte (SHA-256 and length), the install's file list equal (same count and digest). The only differences are files DCS and another add-on write while DCS runs: `Config/InstantActionGeneratorParams.lua`, `Config/authdata.bin`, `MissionEditor/news.lua`, and one state file of another add-on's hook. The same four differ after the restore at the end of step 8. The sidecar's folder was untouched |

A client starts at its main menu, where no server state exists, and the release has no
way to host a session. So step 4's second `doctor`, step 5 and step 6 ran with the
eval channel added beside the release's files, as the Wine half did for its hellos:
`cargo xtask --dll <the archive's payload DLL> deploy <target> --dev` (the same build;
"version folder already in place, manifest unchanged"), and `cargo xtask gates <target>
on`, which appends `net.allow_dostring_in = { 'server', 'mission', 'gui', 'export',
'config' }` to `autoexec.cfg` so the eval channel reaches the `gui` state. The eval
channel hosted a private session with a shell mission
(`lua/experiments/alone/host-session.lua`). Because those gates also allow `server`,
the second `doctor` does not show on Windows that the release's block alone reaches
the server state; the Wine half shows that form. Before step 7, `gates <target> off`
put back `autoexec.cfg` as the install left it, and the development hooks and
`nanoprobe/dev` were deleted; the profile's listing then equaled the one taken after
the install.

Criterion 4 and the controller kill were not run. The machine's SSH server refuses
port forwarding ("Session open refused by peer"), so a hello on the Mac cannot reach
the sidecar's loopback API. Running a hello on the machine needs a hello program copied
onto it, which this run did not do, and the machine has no Python. The DCS kill of
criterion 3 was not run either: reading the restored convoy needs an API client.

## Step 8: criterion 1 on the release's DLL

`cargo xtask --dll <the archive's payload DLL> sampling <target> --part windows`, with
`gates <target> on` before and `off` after. The deploy printed DLL SHA-256
`d8f4d8d6…7dee`, build `67d8161d780bba2e` and loader SHA-256 `f02d0cfb…5400`, equal to
the archive's DLL, the install's build and the archive's loader.

The tool started DCS through its start task, hosted a private session, and spawned the
mixed scenario to 1,200 and then 2,000 objects, about one ground group in three moving
(the tool's own count: 45 of 135 groups, then 69 of 225). It failed on one check; the
owner's targets hold.

| | 1,200 | 2,000 |
|---|---|---|
| Mean share, settled (limit 0.3 %) | 0.045 % | 0.057 % |
| Highest 10 s window's share | 0.074 % | 0.107 % |
| Frames over 250 µs (limit 1 %) | 0 of 14,359 | 7 of 13,813 (0.05 %) |
| Median position interval | 1.000 s | 0.986 s |
| Vitals period the agent reports | 5.03 s | 5.40 s |
| Mirror against the oracle | matches | one row of 2,000 with a position 2.19 s old |
| Frames a second (p5) | 72.0 (71.9) | 72.0 (71.9) |

Over the whole run, 10 of 33,385 frames were over 250 µs, the longest pump 2.64 ms;
the pump's 99th percentile was under 128 µs. Births took at most 84 µs, and
enumeration 259 µs at most.

After the run the tool closed DCS, removed its probe and undeployed, and found the
profile's listing equal to the one before its run.

## Open items

- At 2,000 objects one mirror row was 2.19 s old when the oracle was read, in a run
  with no stall (frames a second at p5 71.9). The Wine half's first run had one such
  row, 4.0 s old.
- The release has no route for a client to host a session, so `doctor`'s server-state
  check on a client needs a person to host one, or the development tooling.
- Criterion 4, the controller kill and the DCS kill of criterion 3 are not shown on
  Windows: no API client could reach the sidecar from the run's machine.

## Limits

- One machine, one DCS build, a client hosting a session. A dedicated server on Windows
  is not covered.
- Criterion 2 is the 8-hour soak's result on the Wine server, cited and not rerun.
- The conformance suite does not run on Windows.
