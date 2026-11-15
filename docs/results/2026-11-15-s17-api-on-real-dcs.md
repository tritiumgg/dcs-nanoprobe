# The API on real DCS

| | |
|---|---|
| Step | S17 exit test: RealSim passes the conformance suite FakeSim passes; the Rust and the Python hello campaigns run unchanged on the fake sim and on the server; the sidecar runs in its own container sharing the DCS container's network namespace. |
| Date | 2026-11-15. Live runs 1 to 4 |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, run provenance) |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Host rehearsal: macOS on arm64, stock Lua 5.1 with the strict mock. |
| Build | Agent build `7b2a46048187ca03` (`deploy --dev`) in all four runs. The sidecar and the probe are static `x86_64-unknown-linux-musl` programs built from the working tree of each run: runs 2 and 3 ran a sidecar from before the owed mission load and the hold on spawning; run 4 ran both. Changes after run 4 are host-tested only: the owed load set before the activation's own commit, the hold's stamp taken from the `MISSION_LOAD`'s journal row and its end rule, the hold narrowed to spawning, the services' theater pre-check and its order, and the script's `tree`, `sidecar_sha256` and `ipc_mode` records. Run 4 has no recorded tree or sidecar hash. |
| Script | `cargo xtask sidecar <target> [--part all\|conformance\|hello]` (`crates/xtask/src/sidecar.rs`), which runs `conformance-probe` (`crates/fake-agent/src/bin/conformance-probe.rs`) and the image of `deploy/Dockerfile` |
| Rehearsal | `lua/tests/conformance.lua` (RealSim against the agent in the mock), `lua/tests/sidecar.lua` (`nanoprobe-sidecar` and both hellos against the agent in the mock), `examples/hello-python/check.sh` (the Python hello against the fake sidecar), `crates/fake-sidecar` tests (the Rust hello against the fake sidecar), all run by `mise run check` |
| Bundle | `bundles/sidecar-<target>-<stamp>/` (not committed: it holds server data) |
| Result | PASS with open items. |

All figures are LIVE unless tagged. Every run used a server with no players connected.

## How the live test runs

The DCS container's `/dev/shm` is private to it (`IpcMode` private, LIVE: read with
`docker inspect` before run 1; the script records it as `ipc_mode` in `sidecar.json` for
later runs), so a second
container cannot open a mapped file there without a compose change. The test moves the
mapped file to `nanoprobe/dev/sidecar/` in the Saved Games profile, which is on a bind
mount the host can also mount into the sidecar's container, and restarts DCS. The
example overlay in `deploy/` uses a memory-backed volume both containers mount; the test
uses a disk-backed folder in its place.

The sidecar's container is `docker run --network container:<dcs container>` with the
DCS user's uid and gid, the map folder, the agent's token (read-only) and, for the
hello part, `sidecar.toml`, a tokens file holding the hash of a token made for the run,
and the `[missions]` folder (read-only). The image is built on the host from
`deploy/Dockerfile`, `FROM scratch`, so nothing is pulled. The sidecar listens on the
DCS container's address on the Docker network, never on a published port; the hellos
reach it from the development machine through an SSH forward to a loopback port there,
opened for the hellos and closed after.

The simulation is held running with the eval channel while a probe or a hello runs.

## Exit list

| # | Item | Host | Live |
|---|---|---|---|
| 1 | RealSim passes the conformance suite | PASS: 9 of 9 against the agent in the mock, `mispredicted` 0 | PASS, runs 2 and 4: 9 of 9 in 5.9 s and 5.2 s, `mispredicted` 0, in the sidecar's image in the shared namespace |
| 2 | Both hellos run unchanged on the fake sim and on the server | PASS: the Rust and the Python hello against the fake sidecar, and against `nanoprobe-sidecar` over the agent in the mock | PASS, runs 3 and 4: Rust and Python, each to "lead 2 reached the end of its route", lease released, exit 0, DCS loading after the campaign's line |
| 3 | The sidecar in its own container, in the DCS container's network namespace | `deploy/Dockerfile` built and run live; `deploy/docker-compose.nanoprobe.yml` written with placeholders and not applied (UNVERIFIED as a whole) | PASS: attached through the shared loopback in runs 2 to 4; stopped by `docker stop` (SIGTERM) with exit code 0 in runs 3 and 4 (`results.sidecar.exit_code_after_stop`) |
| 4 | This document, with the scripts | | |

## Runs

| Run | Part | Result |
|---|---|---|
| 1 | all | Stopped at its own check, before any container: both hello points at `(-300000, 600000)` and `(-299000, 600000)` on Caucasus are water (`land.getSurfaceType` 3, 3; `surface` in `sidecar.json`). The hellos moved to `(-290000, 680000)` and `(-289000, 680000)`, land in runs 2 and 3 (surface type 1, 1). |
| 2 | all | Conformance PASS. Rust hello PASS. The Python hello was cut off: the run's hold of the simulation waited for 15 s of model time, and the Python campaign's activation loaded the mission, which starts model time again at 0, so the hold timed out after 120 s and the run stopped. The script's hold ends when model time goes back (`hold` in `crates/xtask/src/sidecar.rs`), which run 3 used. |
| 3 | hello | Rust and Python hellos PASS. |
| 4 | all | With the owed mission load and the hold on spawning, before the later changes the Build row lists. Conformance PASS, 9 of 9. Rust and Python hellos PASS: in each, the group stayed unspawned until after DCS stopped, loaded and paused the campaign's mission, then went `materializing` and `live` once (the script's `materialized_after_load` rule, checked against the run's `hello_*.out`). A first attempt of this run stopped at its first SSH call, at the key agent, before it touched the server. |

## Mission load on activation

Activation loads the campaign's mission with `MISSION_LOAD` (coordinator ruling,
quorum on RealSim's `load_mission`). The run put the shell mission in the profile as
`nanoprobe/missions/Caucasus.miz` and named the folder in `sidecar.toml` as
`dir = "/missions"` (the sidecar's mount) and `dcs_dir` as the same folder on Wine's
`Z:` drive, `Z:\<profile as the container sees it>\nanoprobe\missions`.

- `net.load_mission` takes a `Z:` path under Wine: each hello's activation was
  followed by `Stopping`, `Loading` and `Paused` on `Caucasus` in the event stream, and
  the group went `virtual` and back to `live` in the new generation (LIVE, runs 2 and
  3; the script passes a hello only when its output shows DCS loading after the
  campaign's line). The conformance suite's own `MISSION_LOAD` of the same path was
  `APPLIED` and a new generation ran (LIVE, run 2).
- On the host, `lua/tests/sidecar.lua` checks that each activation through
  `nanoprobe-sidecar` (RealSim over the real link to the agent in the mock) loads
  `<dir>/Caucasus.miz` once. The refusal cases (no file, an earlier campaign still
  active and its store unchanged, an unknown `[missions]` key) are host tests of the
  hub (`crates/api/src/hub.rs`) over the fake sim with a mission folder, and of
  `sidecar.toml`, since the transport has no part in them; `CreateCampaign` and
  `ActivateCampaign` through the services refuse before anything is captured or
  created.
- In runs 2 and 3 the hello's group was spawned once in the mission that ran before
  the load, then again after it. Activation's load is owed in the campaign's store
  from the activation's commit until its result, and no group of the campaign is
  spawned while it is owed, or after its `APPLIED` until the store records a mission
  other than the one the load was sent in
  (coordinator ruling, quorum on an activation's `MISSION_LOAD` across a sidecar
  crash; spec/persistence.md, "Campaign operations"). Host tests in
  `crates/api/src/hub.rs`: an owed load outlives a crash and is sent once, and never
  again on a later start; a sent load is not sent twice across a crash; a suspend
  clears it; nothing materializes before the load's generation; a load that never
  reached the agent is sent again. Run 4 ran the owed load and the hold live, before
  the later changes the Build row lists.

## Health

Docker showed the live container `unhealthy`: the image's `HEALTHCHECK` runs
`nanoprobe-sidecar health` with no token, and the run's listener needs one. On the host,
`nanoprobe-sidecar health` against a loopback listener with no tokens file printed
`healthy` and exited 0 (`lua/tests/sidecar.lua`).

## Machines left as found

The run removes its containers, its image, the build cache it made (the cache was
empty before), its host folder, the map folder and the mission folder, and puts
`agent.toml` back. The server was then restored to the build and files it had before
the runs and restarted, and its profile listing and the host's images, containers and
build cache were compared with a listing taken before run 1: the same manifest,
versions, `agent.toml`, logs and mission (the one it ran before, paused at model time
0); only the nanoprobe logs grew. The listing and restore scripts name the target, so
they are kept outside the repository with the other session records.

## Open items

- `[agent] command_queue` is read and not enforced: RealSim's queue of commands for the
  inbound ring has no limit, and no call is refused `QUEUE_FULL` for it.
- In the overlay the sidecar shares the DCS container's network namespace. When the DCS
  container restarts, the sidecar exits once the new agent does not answer
  (`handshake_timeout_s`), and `restart: unless-stopped` starts it in the restarted
  container's namespace; when the DCS container is created again, `docker compose up
  -d` must create the sidecar again too. UNVERIFIED: the overlay was not applied.
- An activation's `MISSION_LOAD` that the agent refused, or whose result was lost, is
  only written to the sidecar's standard error, and the campaign runs on the mission
  DCS holds. No API call or event shows it. The refused case has no host test: the
  fake sim refuses only a path the sidecar never sends.
- After a crash between the `APPLIED` commit and the new generation's `LIFECYCLE`, the
  hold on spawning is not restored: it lives in memory once the load is applied.
- `[api] tls_cert` and `tls_key` are refused at start: TLS is not built.
- `[log] dir` and `level` are read and not used: the sidecar logs to standard output.
- The Dockerfile's health check needs `NANOPROBE_TOKEN` in the container's environment
  when the listener has a tokens file.
- The hellos reach a sidecar on the server through an SSH forward to a loopback port on
  the development machine, open only while they run.
- A reattach in the conformance suite is a new link session, which the agent sees as a
  new sidecar boot.
