# S18 live part: persistence and restore on the Wine server

| | |
|---|---|
| Step | S18, live part: the six live tests of docs/results/2026-11-15-s18-persistence-host.md ("Live tests left for a later step") and a sidecar crash between an activation and its mission load |
| Date | 2026-11-15, live runs 1 to 5 |
| DCS | 2.9.31.29434 dedicated server, branch `dcs_server.release` (LIVE, run provenance) |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Rehearsal: macOS on arm64 against `nanoprobe-fake-sidecar`. |
| Build | Run 5, the run this document reports: commit `c0eaacc`; agent build `51f3d3e925766495` (`deploy --dev`); sidecar a static `x86_64-unknown-linux-musl` program, SHA-256 `128c95ead821fc4869bd657426a7b02b28e15412bb7c4ac1f72d14198cdaa50a` |
| Script | `cargo xtask persistence <target> [--only a,b,...]` (`crates/xtask/src/persistence.rs`, scenarios in `crates/xtask/src/persistence/scenarios.rs`); `cargo xtask persistence rehearse` runs the same scenarios against the fake sidecar |
| Bundle | `bundles/persistence-<target>-<stamp>/` with `persistence.json` and the sidecar's log (not committed: they hold server data) |
| Result | PASS. Every item passed live in run 5. Open items below. |

All figures are LIVE and from run 5 unless tagged. Run 5 records the player count at the
start and the end of each scenario: 0 every time. The test also checks for players before
every reload, kill, removal and pause. `c0eaacc` did not check before an activation's
mission load or before the load after a DCS restart; the tool checks there now.

## How the test runs

The sidecar runs as `cargo xtask sidecar` runs it (docs/results/2026-11-15-s17-api-on-real-dcs.md):
its own container in the DCS container's network namespace, the mapped file in a folder of
the Saved Games profile, `[missions]` holding the shell mission as `Caucasus.miz`. Its store
is a folder of the profile mounted into the container, so a sidecar started again reads
the same files. The test talks to the API through an SSH forward to a loopback port.

| Action | How |
|---|---|
| Reload | `net.load_mission` of the campaign's mission in the hook state, behind the sidecar; done when `hook.log` shows the simulation started. The mission starts paused. |
| DCS kill | The sidecar's container is killed, then `docker restart -t 0` on the DCS container. DCS starts its own mission; the test loads the campaign's mission, as an operator would, and starts the sidecar again from its store. |
| Sidecar kill | `docker kill -s KILL`, then a new container on the same store. |
| Sidecar or DCS stopped | `docker pause` and `docker unpause` of its container, for a few seconds. |
| Model time | The eval channel's hold, which resumes the simulation and ends when model time goes back (a mission load during the hold). |
| What DCS holds | `coalition.getGroups` of every coalition, groups whose name is a platform name, with each unit's `getPoint` and `inAir`. |

"One group per entity" means: every group the store has live is in DCS under its name
exactly once, with one unit per living member that has not left; no group the store does
not have live is in DCS under the campaign's names. Positions and ammunition are judged on
what DCS reports, not on the store.

## Exit list

| # | Item | Result |
|---|---|---|
| 1 | Reload and DCS kill with a convoy under way and an aircraft airborne | PASS. "No order is issued twice" is not visible live: open item. |
| 2 | Paused restore that destroys and re-adds groups: one group per entity | PASS |
| 3 | Suspend A, activate B, then A | PASS |
| 4 | Reload with a death pending: the member is not re-created | PASS, for a death the sidecar reads late and for a member held by a mark |
| 5 | Live write cost at 2,000 units (group commit) | PASS |
| 6 | Capture and restore the tables the restore path carries; no string key looks like an integer | PASS for pylons, callsign and keys; ammunition and options are open items |
| 7 | Sidecar crash between an activation and its mission load | PASS |

### 1. Reload and DCS kill

A campaign with a convoy of two M-113 on a 3 km route north at 10 m/s and an F-15C at
3,000 m with an orbit order. The reload came at 617 m along the route, the DCS kill at
1,518 m. Each "before" is read from DCS while the convoy drives; each "after" is read from
DCS once every group is live again and while the restored mission is still paused, so
nothing has driven since the restore placed it.

| | Reload | DCS kill |
|---|---|---|
| Convoy lead from the start, before (DCS) | 617 m | 1,518 m |
| The same, after the restore (DCS, paused) | 642 m | 1,514 m |
| F-15C altitude before, after (DCS) | 2,999 m, 2,997 m | 2,999 m, 2,997 m |
| F-15C in the air after | yes | yes |
| Paused when the groups were live again | yes | yes |
| One group per entity | yes | yes |

The bound is this step's: the convoy back within 100 m of where DCS had it, the distance
it drives in 10 s. After the reload it came back 25 m further on, after the DCS kill 3 m
short. At the end DCS had the convoy
lead at its end (within 0.1 m), the F-15C in the air and live, one group per entity.

The order revisions read 1 before and after each restore. The API's
`applied_order_revision` is always 0 (open item), and the order revision is the author's
revision, not a count of what was sent to DCS. So whether DCS was given the order once per
generation is not checked live; `crates/sim-fake/tests/persistence.rs` checks it on the
host.

### 2. Paused restore

Ten groups of two M-113, live. A reload: the mission started paused (`DCS.getPause()`
true) and the restore spawned all ten while paused, one group per entity. Two groups were
dematerialized while paused. They went `virtual`, and DCS still listed both under their
names (DCS does not apply a destroy while paused, as S15 saw). Both were materialized
again, the sidecar was killed at once and started again from its store, and every group
went live while DCS stayed paused: one group per entity during the pause, ten DCS groups.
After 10 s of model time: one group per entity, ten DCS groups.

### 3. Suspend A, activate B, then A

A with three stationary groups and sample rates set, suspended (`SUSPENDED`); B created
and active with two groups; then A activated again. While B ran, DCS held no group of A;
while A ran again, none of B, and B read `SUSPENDED`. DCS held A's three groups 0.0 m from
where it held them when A was suspended, one group per entity.

### 4. Reload with a death pending

Three groups of three M-113, 150 m apart. In each, the middle unit was taken out, 3 s of
model time ran, the unit was gone from DCS, and the mission was loaded again.

| Case | How the member left | Store after | In DCS after |
|---|---|---|---|
| late | Killed by a 300 kg explosion on it, with the sidecar's container paused from before the kill until after the reload started | dead, no mark | not spawned |
| prompt | Killed the same way, the sidecar running | dead, no mark | not spawned |
| removed | `Unit.destroy`, which DCS reports with no death | alive, `left` `REMOVED` | not spawned |

The removed case passes only on `alive` and `REMOVED`; `c0eaacc` accepted any mark, and
run 5 gave `REMOVED`.

In each case the group came back with its two other members, one group per entity.

In the late case the sidecar was paused through the death and the start of the reload,
and the member was still recorded dead, from a death fact. The removed case is the respawn gate (spec/persistence.md, "Restore"): a
member that left with no death fact, held by the agent's `REMOVED` mark across the mission
load and not spawned again. A member DCS lost with no record at all, the case the
inferred mark covers, was not produced live.

### 5. The write cost at 2,000 units

200 groups of ten M-113, one group in five driving north at 5 m/s, all live; then 120 s of
model time. The sidecar printed its write times every 10 s (`NANOPROBE_COMMIT_TIMINGS`,
`crates/api/src/timings.rs`), 14 reports at 2,000 units, each of about 4,980 commits: one
commit per driver step, about 500 a second.

| | Worst report, live: 400 of 2,000 units moving | Host (S18b): all 2,001 units moving, macOS |
|---|---|---|
| A step's commit, median | 0.66 ms | 0.38 ms |
| A step's commit, p99 | 0.97 ms | |
| A step's commit, longest | 28.4 ms | |
| A checkpoint, median of 28 | 7.6 ms | 7.3 ms median |
| A checkpoint, longest | 35.2 ms | |

The bounds are this step's: a commit's p99 within a frame at 60 fps (16.7 ms) and every
checkpoint within a tenth of `checkpoint_s` (500 ms). Both hold. The 2,000 units spawned in
5.6 s of real time.

### 6. Tables

An F-15C in the air with a payload by pylon number (`payload.pylons` keyed `"1"`, `"2"`,
`"6"`, `"10"`, `"11"`, CLSIDs from ED's `UnitPayloads/F-15C.lua`, ED-CODE), a callsign
table keyed `"1"`, `"2"`, `"3"` and `name`, a board number, and the weapon-hold ROE option;
an M-113 with the ground weapon-hold ROE option (option IDs read from `AI.Option` in the
run). DCS's own reads of each unit, taken after the spawn, after a capture then a
dematerialize and a materialize, and after a reload:

| Unit | Ammunition (`getAmmo`) | Callsign (`getCallsign`) |
|---|---|---|
| F-15C | `M61_20_HE` 940, `AIM_120` 2 | `Enfield11` |
| M-113 | `M2_12_7_T` 104, `M2_12_7` 404 | empty |

All three reads were equal, and the capture was `APPLIED`. The payload names three
AIM-120B pylons and DCS loads two AIM-120. The control is an F-15C spawned outside the
platform by `coalition.addGroup` with the same payload as a Lua table keyed by number: DCS
loaded it with the same rows, `M61_20_HE` 940 and `AIM_120` 2. The conversion of the
decimal-string keys is not the cause; why DCS loads two is UNVERIFIED.

No string key looked like an integer in 218 keys of 55 tables: the F-15C's and the
M-113's `getAmmo`, `getDesc` and `getPosition`, and the inventories of three Caucasus
airbases' warehouses (Anapa-Vityazevo, Krasnodar-Center, Novorossiysk). spec/world.md
("Lua tables in the API") is LIVE for these tables only. Run 5 recorded only the totals;
the tool now records each read's key count and fails a read with none.

Ammunition: the restore path carries no ammunition count. A `PutGroup` with a unit's `ammo`
set is refused (`spawning with a captured life, fuel or ammunition is not built yet`), and
the API's `Unit.ammo` is never filled, so a capture's answer shows none although the store
keeps what DCS read. A respawn gives each unit its type's full load.

Options: DCS has no getter for a controller option, so their sending again after a respawn
is not observed live (UNVERIFIED); host tests check that the sidecar sends them.

### 7. Sidecar crash between an activation and its mission load

Campaign X active with one group; campaign Y created inactive with one group. The DCS
container was paused, Y activated (the call answered `ACTIVE`), the sidecar killed, DCS
unpaused, and the sidecar started again from its store. DCS was read twice before it
started Y's mission, and no group of Y was in X's mission. The test now fails when it
reads DCS no time before that start, or never sees the start; run 5 meets both. DCS loaded Y's mission once: one
simulation start in `hook.log` from before the pause to the end, after 30 s more of model
time, and one `LOADING` lifecycle event in Y's log. Y's group was live once, one group per
entity, and no group of X was in DCS.

Which branch of the owed load ran at the restart (never sent, or sent and waited for) is
not recorded: the sidecar does not log it, and the store is not read by the test.

## Earlier runs

Runs 1 to 4 ran before review round 1 and are not the results above.

| Run | Tree | What happened |
|---|---|---|
| 1 | `1a98240` with this step's tooling uncommitted | Stopped at its own check before it spawned anything: no straight route on land east of the start. The route search now tries north first, the hello's direction. |
| 2 | the same | Item 1 passed. Item 2 stopped in the test's hold: the campaign's activation loaded its mission during the hold, model time started again at 0, and the hold waited for it to pass its start. The hold now ends when model time goes back. |
| 3 | the same | Items 2 to 5 and 7 passed. Item 6 failed on the test's read of ammunition through the API, which does not carry it. |
| 4 | `7040654` | Item 6 alone, reading DCS: passed. |

Run 5 judges on DCS's positions where runs 2 and 3 used the store's, adds the removed case
to item 4 and the pre-load reads to item 7, and compares the payload with the control.
Each item ran live at most three times.

## Rehearsal

`cargo xtask persistence rehearse` ran the scenarios against `nanoprobe-fake-sidecar` with a
store folder: items 1, 2, 3, 5, 6 and 7 passed, item 4 was skipped (a kill needs DCS). There
a reload, a DCS kill and a sidecar kill are each a restart of the fake sidecar's process,
and the DCS-side checks are not judged, so item 6 there checks only the API's answers. The
rehearsal found two faults in the fake sidecar, both fixed: every process started as the
same agent boot, so a restarted fake looked like the boot its store last saw and left the
campaign `virtual`; and it started with no mission, which a restarted DCS does not
(`--theater` starts it on one).

## Machines left as found

The run removes its container, its image, the build cache when it was empty before, its
host folder and the three profile folders, and puts `agent.toml` back. It refuses to start
when any of the three folders exists. The server was restored after runs 4 and 5 to the
build, manifest, logs and mission it had before run 1, and restarted; its profile listing
equals the one taken before run 1 but for file times and the nanoprobe logs, and it runs the
mission it ran before, paused at model time 0. The listing and restore scripts name the
target, so they are kept outside the repository.

## Open items

- The API's `applied_spec_revision` and `applied_order_revision` are always 0
  (`crates/api/src/convert.rs`), so whether DCS was given an order once per generation is
  not visible through the API.
- Spawning with a captured life, fuel or ammunition is not built, and `Unit.ammo` is not
  filled from the store's captured ammunition: a restore gives each unit its type's full
  load.
- Controller options sent again after a respawn are not observable in DCS; only host tests
  check them.
- After a DCS restart DCS runs its own mission; the sidecar does not load the active
  campaign's. The test loaded it.
- A member DCS lost with no record (the inferred mark) was not produced live.
- From S18b's live list: item 7 (a sidecar restart over a running server) was run only as
  far as the active campaign resuming (items 2 and 7 here); that the command epoch is read
  back from the server store and that records delivered again are not folded twice was not
  checked live. Item 8, the delay a command waits for its step's commit, was not measured
  directly; the commit itself is 0.66 ms at the median at 2,000 units.
