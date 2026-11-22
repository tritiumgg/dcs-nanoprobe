# 0.1 acceptance on a DCS dedicated server under Wine

| | |
|---|---|
| Step | S22's acceptance run (docs/acceptance.md), the half on a DCS dedicated server under Wine: a clean install of a release build's files, criteria 3 to 6, and criterion 1 on the same DLL |
| Date | 2026-11-22 |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server (LIVE, `cargo xtask target check <target>`) |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)`, DCS in a Docker container that relaunches DCS and runs DCS's updater at each start |
| Files tested | A manual run of the release workflow, run ID 45006509810, commit `00642c4ec65664f6c137f700f13efe51628f6803`, artifacts `release-files-00642c4ec656` and `agent-symbols-00642c4ec656`, downloaded with docs/releasing.md step 3 into `dist/runner-00642c4ec656/` and checked against the run's `SHA256SUMS` on the Mac and, for the Linux archive, again on the server |
| `SHA256SUMS` | `7bea1a7afd4b370da61195319893b87ef04579d696442a9ac102f70fe289627a  nanoprobe-0.1.0-linux-x86_64.tar.gz`; `b9a0dbc4e0ee1b6fa00b24475c9982c1695e44ff429a0490bd87cf2503ac7d08  nanoprobe-0.1.0-schema.binpb`; `9aa445ced46da260ecf06eb2c641966b6933f297bd882226f5256e2af2dc1634  nanoprobe-0.1.0-schema.tar.gz`; `3507ee572dbbc444b65e063a1f3744d2033cb4eb2a6c8ef3b03bd254e2cb6335  nanoprobe-0.1.0-windows-x86_64.zip` |
| Agent | Build `67d8161d780bba2e`; DLL SHA-256 `d8f4d8d6fc4e389e810f6f2e18f54be9ce4389cd34832a7deac97b70d2e37dee`; PDB signature `B611D9D3D74982194C4C44205044422E1` |
| Scripts | The commands below, with the machine's names replaced; the measuring tools are `cargo xtask` at the files' commit |
| Raw results | Bundles `persistence-<target>-20261122T103637Z`, `sidecar-<target>-20261122T105010Z`, `sampling-<target>-20261122T102628Z` (run 1) and `sampling-<target>-20261122T105734Z` (run 2); they are not committed |
| Result | Criteria 3, 4, 5 and 6 pass. Criterion 1's owner targets hold in both runs; the tool's checks fail on the pump bound at k = 1 and on one window that holds the tool's own oracle read, both ruled open items (below) |

Every DCS figure is LIVE on this build and platform unless tagged.

## Steps 2 to 7: the release's files alone

The server's development install was copied aside and removed, so the profile held no
nanoprobe file. DCS kept running throughout: the container relaunches it, so it cannot
be stopped for the baseline. The baseline: the profile's files outside `Logs/` with
modes, sizes and MD5 sums; `Config/autoexec.cfg`, absent; and a digest of the DCS
install's file list with sizes and modification times, 24,239 entries.

```sh
# on the server: the archive checked, unpacked, copied into the DCS container
grep linux-x86_64 SHA256SUMS | sha256sum -c
tar -xzf nanoprobe-0.1.0-linux-x86_64.tar.gz
docker cp nanoprobe-0.1.0-linux-x86_64 <container>:/tmp/<folder>
docker exec --user <dcs user> <container> /tmp/<folder>/install.sh --wine-prefix <prefix>
docker exec --user <dcs user> <container> /tmp/<folder>/nanoprobe doctor --wine-prefix <prefix> --theater Caucasus
```

| Step | Result |
|---|---|
| 3. Install | `install.sh` exit 0: version `67d8161d780bba2e`, the token, and the block in a new `Config/autoexec.cfg`. The loader, the DLL and `lua/hook.lua`, `log.lua`, `shim.lua` equal the archive's payload, the version folder's `SHA256SUMS` matches, `manifest.txt` names the build, and `install.txt` lists `Scripts` and `Scripts/Hooks`, which the profile lacked |
| 4. Check | After a DCS restart, `doctor` exit 0: the hook started the build, the hook state has all 21 DCS names the platform calls and the server state all 47, and the block allows `userhooks` and `server`. The release's `autoexec.cfg` block reaches the server state |
| 5. Sidecar | The image built from the archive with `deploy/Dockerfile` ran in the DCS container's network namespace, as `cargo xtask sidecar` runs its own; `agent.toml` names the mapped file in a profile folder (deploy/README.md, step 3), the store is in a folder outside the profile, and the listener takes a tokens file. Health `healthy`; the agent attached. `doctor --missions <profile>/nanoprobe/missions --theater Caucasus` exit 0 |
| 6. Kills | Sidecar: `docker kill`, `docker start` 5 s later: the agent detached 0.35 s after the kill and attached 0.82 s after the start; health `healthy`. Controller: hello A held the lease and was killed; hello B, started at once, was refused "another controller holds the lease"; hello C, after the 30 s lease time, was granted a higher epoch and made its campaign. After a DCS restart the sidecar exits ("the agent is gone after a DCS restart"), and the run started it again |
| 7. Uninstall | `uninstall.sh` exit 0, "removed nanoprobe", with the sidecar stopped. Against the baseline: no nanoprobe file, `Scripts` gone again, no `autoexec.cfg`; the only differences are DCS's own `Tracks/Multiplayer/*.trk` of the run's missions, `Config/authdata.bin` and `dcs.lock`. The install's list differs only in `autoupdate.dat`, `autoupdate.cfg`, `autoupdate_log.txt` and `bin/DCS_updater.exe`, written by DCS's updater at each container start, with the entry count unchanged. The sidecar's store folder was untouched |

Criterion 4, the hello campaigns, ran on a development deploy: the server pauses while
nobody is connected, and the release cannot unpause DCS. `cargo xtask --dll <the
archive's payload DLL> deploy <target> --dev` added the eval channel beside the
release's files (the same build, `67d8161d780bba2e`), and an eval hold kept the
simulation running. Against the archive's sidecar, the Rust and the Python hello each
reached "lead 2 reached the end of its route" and "hello campaign done", and released
the lease (2 min 25 s and 2 min 24 s). The first attempt of each failed on the run's own
hold script and on the lease time between them, not on the platform. The development
files were removed before step 7.

## Step 8: the tooling criteria on the release's DLL

Each command ran `cargo xtask --dll <the archive's payload DLL> ...`. Each deploy printed
DLL SHA-256 `d8f4d8d6…7dee`, build `67d8161d780bba2e` and loader SHA-256 `f02d0cfb…5400`,
equal to the archive's DLL, the install's build and the archive's loader.

| Command | Result |
|---|---|
| `persistence <target> --only reload` (the DCS kill of criterion 3, with a moving convoy) | PASS |
| `sidecar <target> --part conformance` (criterion 6) | PASS, 9 of 9 checks |
| `sampling <target> --part mirror` (criterion 1), run 1 | FAIL on the tool's checks; the owner's targets hold |
| the same, run 2 | FAIL on the tool's checks; the owner's targets hold |

Criterion 1 at 2,000 objects, one ground group in five moving:

| | Run 1 | Run 2 |
|---|---|---|
| Mean share, from the 800 spawn on and at 2,000 settled | within 0.3 % | within 0.3 % |
| Frames over 250 µs | 68 of 42,804 (0.16 %) | 63 of 43,061 (0.15 %) |
| Median position interval, stall-free windows | 0.997 s | 0.997 s |
| Mirror against the oracle at 500, 1,200, 2,000 | one row of 1,960 with a position 4.0 s old | matches |
| Worst stall-free window's position period (limit 1.25 s) | 1.49 s | 2.05 s |
| Pumps a second at k = 1 within the bound | fails in 5 windows | fails in 7 windows |
| Frames a second at 2,000 (p5) | 68.2 (0.70) | 66.8 (0.45) |

The coordinator's rulings on the failed checks:

- **The long window is the tool's oracle read.** In both runs the worst window is the
  last report window, and the tool wrote `oracle-2000.json` inside it. That window alone
  has a frame gap of about 0.5 s (0.52 and 0.57 s), under the 1 s stall threshold, so
  the check counts it as free; the agent's share in it is 0.0017, and every earlier
  settled window reads 0.93 to 1.06 s. The release's DLL is not implicated.
- **The stale mirror row** of run 1 fell in a run with a DCS stall and did not recur.
- **The pump bound at k = 1** fails on main and on every live run of the pump gate too
  (docs/results/2026-11-22-pump-gate.md). Run 1: 13.39 against 11.38, 11.40 against
  10.10, 11.69 against 9.93, 10.09 against 9.76, 6.99 against 6.40. Run 2: 13.89 against
  12.23, 11.19 against 9.95, 10.99 against 9.75, 10.40 against 9.86, 10.89 against
  10.31, 6.90 against 6.53, 6.49 against 6.27.

## Open items

- The sampling tool's settled window check must leave out the window that holds the
  oracle read.
- The pump bound at k = 1 at 2,000 objects.
- Run 1's mirror row 4.0 s old, in a run with a DCS stall.
- The release cannot unpause DCS: a server that pauses while nobody is connected keeps
  a campaign still, and the hellos needed the eval channel (release notes, "Know before
  you install").
- The overlay's restart policy after a DCS restart is still not shown: the run started
  the sidecar itself.
- The server's Docker build cache holds the layers of the run's image build; whether it
  was empty before is not recorded, so it was left for the server's administrator.

## Limits

- One server, one DCS build. The Windows half of the acceptance is not in this document.
- DCS ran during the baseline and the compare, so files DCS writes itself differ.
- Criterion 2 is the 8-hour soak's result, cited and not rerun.
