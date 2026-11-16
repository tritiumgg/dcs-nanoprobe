# S18 live part: persistence and restore on the Wine server

| | |
|---|---|
| Step | S18, live part: the six live tests of docs/results/2026-11-15-s18-persistence-host.md ("Live tests left for a later step") and a sidecar crash between an activation and its mission load |
| Date | 2026-11-15, live runs 1 to 4 |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, run provenance) |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Rehearsal: macOS on arm64 against `nanoprobe-fake-sidecar`. |
| Build | Agent build `51f3d3e925766495` (`deploy --dev`) in every run. Sidecar: static `x86_64-unknown-linux-musl`, SHA-256 `128c95ea…aa50a` in runs 2 to 4 (the sidecar code did not change between them). Runs 2 and 3 ran the working tree on `1a98240` with this step's tooling uncommitted; run 4 ran commit `7040654`. Between run 3 and `7040654` only the test tool changed (its hold and item 6). |
| Script | `cargo xtask persistence <target> [--only a,b,...]` (`crates/xtask/src/persistence.rs`, scenarios in `crates/xtask/src/persistence/scenarios.rs`); `cargo xtask persistence rehearse` runs the same scenarios against the fake sidecar |
| Bundle | `bundles/persistence-<target>-<stamp>/` with `persistence.json` and the sidecar's log (not committed: they hold server data) |
| Result | PASS. Every item passed live. Open items below. |

All figures are LIVE unless tagged. No player was connected in any run.

## How the test runs

The sidecar runs as `cargo xtask sidecar` runs it (docs/results/2026-11-15-s17-api-on-real-dcs.md):
its own container in the DCS container's network namespace, the mapped file in a folder of
the Saved Games profile, `[missions]` holding the shell mission as `Caucasus.miz`. Its store
is a folder of the profile mounted into the container, so a sidecar started again reads
the same files. The test talks to the API through an SSH forward to a loopback port.

The test does these things to DCS and the sidecar:

| Action | How |
|---|---|
| Reload | `net.load_mission` of the campaign's mission in the hook state, behind the sidecar; done when `hook.log` shows the simulation started. The mission starts paused. |
| DCS kill | The sidecar's container is killed, then `docker restart -t 0` on the DCS container, which DCS does not survive cleanly. DCS starts its own mission; the test loads the campaign's mission, as an operator would, and starts the sidecar again from its store. |
| Sidecar kill | `docker kill -s KILL`, then a new container on the same store. |
| Sidecar or DCS stopped | `docker pause` and `docker unpause` of its container, for a few seconds. |
| Model time | The eval channel's hold, which resumes the simulation and ends when model time goes back (a mission load during the hold). |
| What DCS holds | `coalition.getGroups` of every coalition, groups whose name is a platform name. |

"One group per entity" means: every group the store has live is in DCS under its name
exactly once, with one unit per living member that has not left; no group the store does
not have live is in DCS under the campaign's names.

## Exit list

| # | Item | Result | Runs |
|---|---|---|---|
| 1 | Reload and DCS kill with a convoy under way and an aircraft airborne | PASS | 1 (stopped at its own check), 2 |
| 2 | Paused restore that destroys and re-adds groups: one group per entity | PASS | 2 (stopped: test hold), 3 |
| 3 | Suspend A, activate B, then A | PASS | 3 |
| 4 | Reload with a death pending: the member is not re-created | PASS | 3 |
| 5 | Live write cost at 2,000 units (group commit) | PASS | 3 |
| 6 | Capture and restore the tables the restore path carries; no string key looks like an integer | PASS, with the ammunition open item | 3 (failed: test read), 4 |
| 7 | Sidecar crash between an activation and its mission load | PASS | 3 |

### 1. Reload and DCS kill

A campaign with a convoy of two M-113 on a 3 km route north at 10 m/s and an F-15C at
3,000 m with an orbit order. The reload came at 654 m along the route, the DCS kill at
1,542 m.

| | Reload | DCS kill |
|---|---|---|
| Convoy lead from the start, before | 654 m | 1,542 m |
| The same, after the restore | 674 m | 1,531 m |
| F-15C altitude before, after | 2,999 m, 2,961 m | 2,999 m, 2,960 m |
| F-15C in the air after | yes | yes |
| One group per entity | yes | yes |
| Order revision before, after | 1, 1 (both groups) | 1, 1 |

The convoy reached its end (within 4 m) and the F-15C was still in the air and live at the
end, one group per entity. The convoy came back where it was, not at its start: after the
DCS kill, 11 m short of where it was, the distance it drove since the last checkpoint
(positions are written only by a checkpoint, every 5 s).

"No order is issued twice" was checked by what the API shows: the order revision is the
same after each restore, and the convoy carried on from its place to its end. The API's
`applied_order_revision` is always 0 (open item), so how many times DCS was given the
order in each generation is not visible live; the host test checks it.

Run 1 stopped at its own check before it spawned anything: it looked for a route east of
the start, and the land there is not. The route search now tries north first, the
direction of the hello's route.

### 2. Paused restore

Ten groups of two M-113, live. A reload: the mission started paused (`DCS.getPause()`
true) and the restore spawned all ten while paused, one group per entity. Two groups were
dematerialized while paused. They went `virtual`, and DCS still listed both under their
names (DCS does not apply a destroy while paused, as S15 saw). Both were materialized
again, the sidecar was killed at once and started again from its store, and every group
went live while DCS stayed paused: one group per entity during the pause, ten DCS groups.
After 10 s of model time: one group per entity, ten DCS groups.

Run 2 stopped in the test's own hold: the campaign's activation loaded its mission during
the hold, model time started again at 0 and the hold waited for it to pass its start.
The hold now ends when model time goes back.

### 3. Suspend A, activate B, then A

A with three stationary groups and sample rates set, suspended; B created and active with
two groups; then A activated again. While B ran, DCS held no group of A; while A ran
again, none of B. A's three groups came back 0.0 m from where they were left, one group
per entity. Both suspended campaigns read `SUSPENDED`.

### 4. Reload with a death pending

Two groups of three M-113, 150 m apart. In each, the middle unit was killed with a
300 kg explosion on it, 3 s of model time ran, the unit was gone from DCS, and the mission
was loaded again.

| Case | How | Member in the store | Member in DCS after |
|---|---|---|---|
| late | The sidecar's container paused from before the kill until after the reload had started | dead | not spawned |
| prompt | The sidecar running | dead | not spawned |

In both cases the group came back with its two other members, one group per entity, and
the member was recorded dead, not `missing`.

### 5. The write cost at 2,000 units

200 groups of ten M-113, one group in five driving north at 5 m/s, all live; then 120 s of
model time. The sidecar printed its write times every 10 s (`NANOPROBE_COMMIT_TIMINGS`,
`crates/api/src/timings.rs`), 14 reports at 2,000 units:

| | Worst report | Host (S18b, 2,001 units, macOS) |
|---|---|---|
| A step's commit, median | 0.68 ms | 0.38 ms |
| A step's commit, p99 | 0.96 ms | |
| A step's commit, longest | 25.8 ms | |
| A checkpoint | 0.6 to 14.4 ms; 7.6 to 8.8 ms in all but one of the last 20 | 7.3 ms median |

About 500 commits a second: one per driver step. The bounds are this step's: a commit's
p99 within a frame at 60 fps (16.7 ms) and every checkpoint within a tenth of
`checkpoint_s` (500 ms). Both hold. The 2,000 units spawned in 5.9 s of real time.

### 6. Tables

An F-15C in the air with a payload by pylon number (`payload.pylons` keyed `"1"`, `"2"`,
`"6"`, `"10"`, `"11"`, with CLSIDs from ED's `UnitPayloads/F-15C.lua`, ED-CODE), a
callsign table keyed `"1"`, `"2"`, `"3"` and `name`, a board number, and the
weapon-hold ROE option; an M-113 with the ground weapon-hold ROE option (option IDs read
from `AI.Option` in the run). DCS's own reads of each unit, taken after the spawn, after a
capture then a dematerialize and a materialize, and after a reload:

| Unit | Ammunition (`getAmmo`) | Callsign (`getCallsign`) |
|---|---|---|
| F-15C | `M61_20_HE` 940, `AIM_120` 2 | `Enfield11` |
| M-113 | `M2_12_7_T` 104, `M2_12_7` 404 | empty |

All three reads were equal, and the capture was `APPLIED`. The payload names three
AIM-120B pylons and DCS loaded two AIM-120 (UNVERIFIED why; the same after each restore).

No string key looked like an integer in 218 keys of 55 tables: each unit's `getAmmo`,
`getDesc` and `getPosition`, and the inventory of three airbases' warehouses. This settles
spec/world.md's claim for these tables (LIVE, the build and platform above).

The restore path carries no ammunition count: a `PutGroup` with a unit's `ammo` set is
refused (`spawning with a captured life, fuel or ammunition is not built yet`), and the
API's `Unit.ammo` is never filled, so a capture's answer shows none although the store
keeps what DCS read. A respawn gives each unit its type's full load. Run 3's item 6 read
ammunition through the API and failed on that; run 4 reads it from DCS.

Options: DCS has no getter for a controller option, so their re-application after a
respawn is not observed live (UNVERIFIED); the host tests check that the sidecar sends
them again.

### 7. Sidecar crash between an activation and its mission load

Campaign X active with one group; campaign Y created inactive with one group. The DCS
container was paused, Y activated (the call answered `ACTIVE`), the sidecar killed, DCS
unpaused, and the sidecar started again from its store. DCS loaded Y's mission once: one
simulation start in `hook.log` from before the pause to the end, after 30 s more of model
time, and one `LOADING` lifecycle event in Y's log. Y's group was live once, one group per
entity, and no group of X was in DCS.

## Rehearsal

`cargo xtask persistence rehearse` passed all seven against `nanoprobe-fake-sidecar` with a
store folder. There a reload, a DCS kill and a sidecar kill are each a restart of the fake
sidecar's process, and the DCS-side checks are not judged. The rehearsal found two faults in
the fake sidecar, both fixed: every process started as the same agent boot, so a restarted
fake looked like the boot its store last saw and left the campaign `virtual`; and it started
with no mission, which a restarted DCS does not (`--theater` starts it on one).

## Machines left as found

The run removes its container, its image, the build cache when it was empty before, its
host folder and the three profile folders, and puts `agent.toml` back. The server was then
restored to the build, manifest, logs and mission it had before run 1 and restarted; its
profile listing equals the one taken before run 1 but for file times and the nanoprobe
logs, and the mission it runs is the one it ran before, paused at model time 0. The listing
and restore scripts name the target, so they are kept outside the repository.

## Open items

- The API's `applied_spec_revision` and `applied_order_revision` are always 0
  (`crates/api/src/convert.rs`), so whether DCS was given an order once per generation is
  not visible through the API.
- Spawning with a captured life, fuel or ammunition is not built, and `Unit.ammo` is not
  filled from the store's captured ammunition: a restore gives each unit its type's full
  load.
- After a DCS restart DCS runs its own mission; the sidecar does not load the active
  campaign's. The test loaded it.
- From S18b: a sidecar restart over a running server resuming its campaign was run here
  (items 2 and 7); the delay a command waits for its step's commit was not measured
  directly (S18b item 8). The commit itself is 0.7 ms at the median at 2,000 units.
