# S20 virtualization: 500 of 10,000 on the server, 2,000 of 10,000 on the fake sim

| | |
|---|---|
| Step | PLAN S20: authority, materialize and dematerialize, the state handoff, interest queries, the materialization budget; the store measured at 10,000 entities |
| Date | 2026-11-17 |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Builds | Run 4: agent `8c6e96ed7f1fdf0a` and the sidecar image, built from the tree of the commit that adds this document (main `e05737d` plus this step's changes). Runs 1 to 3 ran earlier versions of the same work: run 1 agent `db81d28c04d50b7e` before the tail fix, run 2 `ed9afd7e50da1632` after it, run 3 the round-1 review fixes |
| Scripts | `cargo xtask persistence <target> --only virtual` (`crates/xtask/src/persistence/virtualization.rs`); `cargo test -p nanoprobe-sim-fake --release --test virtualization -- --ignored` (`mise run virtual`); `cargo test --release -p nanoprobe-sidecar --test store_scale -- --ignored --nocapture` |
| Rehearsal | `cargo xtask persistence rehearse --only virtual`: the same controller against `nanoprobe-fake-sidecar`, 2,000 of 10,000 |
| Raw results | Bundles `persistence-<target>-20261117T231351Z` (run 1), `...T232301Z` (run 2), `20261118T012813Z` (run 3), `...T015148Z` (run 4); rehearsal `persistence-rehearse-20261118T014335Z` |
| Result | Host: pass. Server, run 4: every check passed but one: one of 100 route groups was captured 0.28 rad from the heading read from DCS before its dematerialize, against this test's 0.2 rad (see "Server: 500 of 10,000"). Every ID kept, 200 hit units and 240 entering units handed over exactly, no group duplicated or lost, at most one spawn per frame, owner targets held (mean agent share 0.276 %, 0.50 % of frames over 250 µs) |

Every DCS figure is LIVE on these builds. Host figures were taken on a development Mac
under heavy load from other work, so their absolute times are high and noisy.

## The test controller

Both tests run one controller. The world is 10,000 groups, created virtual. One unit in
five starts at 60 % life. Every unit has its own heading. One group in ten has a route,
so its units move while they are in DCS. A point moves along a path of six positions
across the world and back. At each position the controller asks the interest query for
the groups near the point, takes the nearest `KEEP`, and materializes and dematerializes
the difference. It then waits until the sidecar holds exactly those groups live and the
rest virtual.

| | Host (fake sim, in process) | Host (fake sidecar, API) | Server (API) |
|---|---|---|---|
| World | 10,000 groups of 2 units | 10,000 groups of 1 unit | 10,000 groups of 1 M-113 on a land grid 300 m apart |
| Kept materialized | 2,000 | 2,000 | 500 |
| Interest query | the store's grid | `QueryArea` | `QueryArea` |
| Checked | every frame | every 2 s poll | every 2 s poll, and DCS read through the eval channel |

Checks:

- **Authority.** The in-process test compares every group's authority with the one
  the controller last set, after every frame. Through the API, which reports states
  and not authority, a group the controller did not move must keep its state, and no
  group may become `lost`, `unspawnable` or `dead`.
- **IDs.** At the end every group and unit has the `EntityId` it was created with.
- **No duplicate, nothing lost.** No unit entity is in DCS twice. After each move DCS
  holds each live group once and no other group of the campaign (server: every DCS
  group with the campaign's tag read through the eval channel).
- **Handover out.** DCS changes units while they are in it, so a capture that kept the
  stored state fails. In process: after each move one live unit in seven turns by 1 rad
  and is hit to 35 % (80 % if already at 35 %) in the fake; each unit of a group that
  left DCS was captured where DCS held it at the end of the frame before, within one
  frame's movement, with DCS's heading and life. On the server: 40 stationary leaving
  groups per move are hit the same way through `DoEffect` and read from DCS just before
  the dematerialize, and 20 leaving route groups, which turn as they drive, are read the
  same way; the sidecar's state after the handover must match the read. On the server
  a slow vitals sweep between the hit and the dematerialize can also give the sidecar
  the hit life, and the run does not record for how many of the 200 that happened, so
  the in-process test is the evidence that the capture carries life.
- **Handover in.** In process: each unit entering DCS spawns at the position and
  heading the store held, and has the store's life once its `DAMAGE` applies. On the
  server: 40 stationary entering groups per move are read from DCS 5 s after they are
  live.
- **Budget.** In process: at most `materialize_per_frame` (1) spawns reach DCS in one
  frame, counted each frame. On the server every group has one unit, so a unit's birth
  event is its group's spawn: per move the campaign's births must number the groups
  that entered DCS, and no frame may hold more than one, by the births' frame numbers.
  The run also fails on any command over the commanded-work budget.
- **Owner targets.** On the server only, from the agent's `sampling` reports: mean
  share at most 0.3 % and at most 1 % of frames over 250 µs. The fake sim has no agent,
  so the host tests do not measure them; the server runs are the evidence.
- **Every sample checked.** In process the units checked leaving and entering equal
  the members of every group dematerialized and materialized. On the server a sampled
  group with no DCS read or no state in the sidecar fails the run.

Tolerances, this test's own: position 5 m north and east (DCS sets a ground unit on the
terrain); heading 0.01 rad; life 0.5 percentage points. A route group: heading 0.2 rad
(it may turn at a waypoint between the read and the capture), position the distance it
can drive at 5 m/s from the read to the end of the handover, plus 5 m. In process: one frame's
movement (10 m) for position, 1e-9 rad for heading, 1e-4 percentage points for life
(the store keeps life as an f32).

## Host: 2,000 of 10,000

In process (`a_controller_keeps_2000_of_10000_materialized_around_a_moving_point`):
pass. Six moves: 7,498 materializations and 5,498 dematerializations over 8,877
frames; 14,996 units checked entering DCS and 10,996 leaving, every one of them; 2,625 of
those leaving had been turned and hit in DCS; every position and heading handed over
exactly (largest gap 0 m); route units captured up to 2,828 m from where they were
created; never more than one spawn in a frame. The normal test run keeps 200
of 1,000 with the same checks; the full size runs in release in `mise run check` and CI.

Through the API against the fake sidecar (the rehearsal): every group and unit kept its
ID, no group went `lost`, `unspawnable` or `dead`, no unmoved group changed state.
In the last rehearsal (`persistence-rehearse-20261118T014335Z`) the first 2,000 took
49 s and later moves 15 to 63 s for 442 to 1,742 groups each way; the fake sidecar was
a debug build.

## Server: 500 of 10,000

`persistence <target> --only virtual`, four runs, at the cap. Run 1 failed the owner's
p99 target: commanded work counted in the frame tail (see "Agent overhead and commanded
work"). Run 2, after the tail fix, passed the checks of its version, which had no hits
and no route-group checks. Run 3 stopped at move 3: a hit was refused because
the sidecar held no DCS object yet for the unit (see "Unbound units"). Run 4, the
final test code:

| Move | Entering | Leaving | Handover, s | `QueryArea`, ms | Births, frames spanned | Most births in a frame |
|---|---|---|---|---|---|---|
| 0 | 500 | 0 | 10.0 | 40 | 1,007 | 1 |
| 1 | 246 | 246 | 6.4 | 42 | 495 | 1 |
| 2 | 494 | 494 | 9.9 | 44 | 1,002 | 1 |
| 3 | 500 | 500 | 12.3 | 38 | 1,017 | 1 |
| 4 | 500 | 500 | 9.4 | 42 | 1,001 | 1 |
| 5 | 500 | 500 | 9.4 | 49 | 1,013 | 1 |

- Putting the 10,000 groups through `PutGroup`, one call each over the SSH port
  forward: 71 s.
- IDs: every group and unit kept its ID. States: no group went `lost`, `unspawnable` or
  `dead`, and no group the controller did not move changed state.
- DCS held each live group once and no other group of the campaign after every move.
- Births: every entering group was born once, never more than one in a frame, at a
  median of one every 2 frames.
- Handover out, stationary: 200 units hit to 35 % or 80 % before they left, each held by
  the sidecar afterwards with DCS's position, heading and new life.
- Handover out, route groups: 100 checked, 79 of them turned more than 0.2 rad from the
  heading they spawned with. 99 were captured within 0.2 rad of the heading read from
  DCS before the dematerialize, and within the distance they could drive. Group 6761
  was captured 0.284 rad from the read, 4.65 m from the read position, while the bound
  on its driving was 79.5 m. That it had slowed and turned at a waypoint between the read
  and its capture is UNVERIFIED: the run records no speed.
  The read precedes the capture by a few seconds, so for a turning unit it is a proxy;
  the 0.2 rad bound was set before the run and the run fails on it. The stationary
  units, whose read is not a proxy, matched exactly.
- Handover in: 240 units, every life within 0.5 points, including units hit before an
  earlier dematerialize. Over both directions the largest position gap was 0.00006 m and
  the largest heading gap of an entering unit 3e-7 rad.
- Owner targets, from the agent's 14 `sampling` reports over the 138 s of moves: mean
  share 0.276 % (largest report 0.298 %), 78 of 15,538 frames over 250 µs (0.50 %, the
  p99 within 250 µs), the longest agent time of one frame 1.26 ms.
- Commands over the commanded-work budget: 0.
- The sidecar's commits, over its 14 timing reports while the store held 10,000 groups
  (the bundle's `sidecar_timings` also holds reports from while the world was being put):
  medians 132 to 161 µs, p99 295 to 567 µs, largest 52.8 ms (its cause is not recorded);
  checkpoints 4.8 to 34.0 ms, 25 of 28 under 12 ms.

### Unbound units

A group is `live` when its spawn is applied; its units are bound to DCS objects when
their `OBJECT` records come from the agent's births tier. In run 4, of the stationary
leaving units the test tried to hit, 90 (move 3), 31 (move 4) and 35 (move 5) were not
yet bound, and the sidecar refused their effects. Their groups had gone live at the
move before, roughly 10 to 20 s earlier (UNVERIFIED: an estimate from the handover times
and the 5 s hold; the run records no per-unit times). A controller that acts on a unit right after materializing it meets the same
refusal. Why the `OBJECT` records lag is open; the AI birth loadout marks priced below
their cost (S19) are a candidate.

## The materialization budget

The first host run found more than one spawn reaching DCS in a frame. The reconciler
sent at most `materialize_per_frame` spawns per step, but a step comes every 2 ms and a
frame about every 9 ms, and spawns written behind a few hundred capture-and-destroys
waited in the ring and then ran together: three in one frame at the first move.
Unanswered spawns now count against the budget outside the restore burst, so at most
`materialize_per_frame` spawns are in the ring at once (spec/world.md, "Authority";
spec/config.md; quorum ruling of 2026-11-17). Destroys keep no sidecar limit; the
agent's commanded-work budget spreads them. Host tests: with 300 capture-and-destroys
written ahead of the spawns, no frame takes more than the budget at 1 and at 3, and at 3
some frame takes 3; a paused restore still spawns more than the budget in a frame.

The agent runs a frame's commands in its pump and publishes `out_tail` once, at the end
of the frame (`finish`, `crates/agent/src/driver.rs`), so a spawn's `RESULT` reaches the
sidecar after the frame that ran it. On the server, spawns came at a median of one
every 2 frames, with 500 destroys ahead of them (moves 3 to 5) and with none (move 0)
alike: the pace is the round trip, a result published at the end of one frame and the
next spawn taken at the start of a later one, not the ring position. A larger
`materialize_per_frame` pipelines spawns.

## Agent overhead and commanded work

PLAN section 6 keeps DCS's time executing commands out of the agent-overhead budget,
and spec/sampling.md "Slice" defines a frame's agent time without its commanded work.
The sampler took it out; the agent's count of frames over 250 µs did not, so every
frame that ran a spawn or a destroy counted. In the first server run 2,888 of 13,284
frames (21.7 %) were over 250 µs, about one for each command, while the mean share was
0.275 %. The sampler now hands each frame's commanded time to the link, and the count
leaves it out. The pump histogram keeps the whole span.

Before and after, the same test back to back on the server (runs 1 and 2):

| Measure | Before (`db81d28c`) | After (`ed9afd7e`) |
|---|---|---|
| Mean share | 0.275 % | 0.281 % |
| Largest report | 0.301 % | 0.347 % |
| Frames over 250 µs | 2,888 of 13,284 (21.7 %) | 70 of 14,413 (0.49 %) |
| `frame_max_us` (before: the frame span with commanded work; after: agent time) | 19.8 ms | 8.4 ms |

The change adds one store per frame; the shares differ by run-to-run variance.

## Store at 10,000

`store_scale` in release: 10,000 groups, with 1 and with 3 units each, in a store file.
Before the dirty set, every commit compared every row with the copy of what the file
held, so a step's commit cost 3.4 ms at 10,000 groups of 1 unit and 6.0 ms at 3 units,
with nothing changed. The driver steps every 2 ms and commits at the end of each step,
so the comparison alone would have used the driver thread.

Before is main `e05737d`; after is the tree of the commit that adds this document.

| Measure, ms (median, largest) | 10,000 × 1 unit, before | after | 10,000 × 3 units, before | after |
|---|---|---|---|---|
| Commit, nothing changed | 3.4, 4.3 | 0.001, 0.02 | 6.0, 7.6 | 0.001, 0.02 |
| Commit, one authority change | 3.4, 3.6 | 0.08, 0.59 | 6.6, 8.1 | 0.03, 0.21 |
| Commit, positions of 2,000 groups moving | 3.9, 4.4 | 1.3, 1.9 | 6.9, 8.1 | 3.2, 4.5 |
| Checkpoint, 2,000 groups moved | 8.7, 11.3 | 13.7, 16.6 | 27.5, 51.8 | 42.8, 81.0 |
| Checkpoint, every unit moved | 43.3, 46.2 | 46.6, 59.7 | 131, 134 | 138, 205 |

The checkpoint path is the same before and after: it compares every row. Its slower
second column is put down to the Mac's load during that run, UNVERIFIED: the two runs
were not interleaved on an idle machine. On the server see "Server: 500 of 10,000".

**Dirty set: added.** A change to a group or one of its units marks the group, and a
commit compares only the marked groups and their members. A checkpoint, a new file and
the first commit after a new mission generation compare every row. Debug builds compare
every row at every commit as well and fail when the marks missed a change, so the
workspace tests check the marks (spec/persistence.md, "Writes").

**`unit_state` split: not needed.** A checkpoint writes each moved unit's whole row. At
the S20 sizes, with 2,000 groups moving, it takes 8.7 to 16.6 ms every 5 s on the Mac at
1 unit a group and 27 to 81 ms at 3 units; on the server, at 500 materialized groups,
4.8 to 34.0 ms, mostly under 12 ms. The worst case measured, 30,000 units all moving,
took 131 to 205 ms. A pause of that size every 5 s delays the driver's step by as much
and misses no owner target, which are on the agent. Neither is a target the split would meet that the current
schema misses; schema 1 keeps one unit row.

**Interest queries.** `QueryArea` over 10,000 groups answered in 38 to 49 ms in run 4
on the server's sidecar, through the SSH port forward, with a 6.7 km radius that found
at least the 500 groups needed in one query each time (the count found is not recorded).

## Open items

- AI birth loadout marks cost about 22 µs each and are priced at 1 µs (S19). Not
  addressed: it did not block the budget here.
- Units stay unbound for some time after their group goes live on the server, an
  estimated 10 to 20 s (UNVERIFIED, "Unbound units"); a controller's effect on such a
  unit is refused.
- The route-group heading check compares the capture with an earlier read; a unit
  turning at a waypoint in between can exceed it (run 4, one of 100).
- Store checkpoint times before and after the dirty set, interleaved on an idle machine.
- The API reports a group's state, not its authority, so an API client cannot tell a
  `materializing` group the controller is withdrawing from one it asked for. The
  in-process test checks authority directly.
- A sidecar step is still linear in the number of groups (the reconciler's plan walks
  every group). At 10,000 it was not measured apart from the fake's own cost.
