# The reconciler under interleaved faults, on the fake sim and on recordings

| | |
|---|---|
| Step | S12 exit test: the reconciler passes a property test over interleaved deaths, restarts and reconnects on the fake sim and on recordings from S9 and S10. |
| Date | 2026-11-13 |
| Platform | Host: macOS 26.6.2 on arm64, Rust 1.99.1. No DCS ran for this step. |
| Recording | `recording.jsonl` of S9's event catalogue run (`cargo xtask catalogue`, `link-probe --record`), SHA-256 `01c05308a5e4c785014287f6e1989a293c7fae2af50e313eb620b4f78d5a4580`: DCS 2.9.31.29434, branch `dcs_server.release`, dedicated server under Wine `wine-10.0 (Debian 10.0~repack-6)`, recorded 2026-11-13 07:43 UTC. It holds server data and is not committed. |
| Script | `crates/sidecar/tests/reconciler.rs`, `crates/sidecar/tests/replay.rs`, `crates/sim-fake/tests/dst.rs`, `crates/sim-fake/tests/conformance.rs`; the harness is `crates/sim-fake/src/harness.rs` |
| Code | The commit that adds this document |
| Result | Met on the fake sim. Partly met on recordings: S9's recording replays as stated below; the object table's records (`OBJECT`, `OBJECT_GONE`, enumerations) come from DCS only with S10's recordings, which do not exist yet. |

## What runs

A `World` holds the fake sim (DCS and the agent at the level of the agent's protocol),
the store and the reconciler. An operation is a fault or some frames:

| Operation | What it does |
|---|---|
| Frames | DCS runs 1 to 8 frames; the reconciler steps after each |
| Kill | A unit or static dies: a death event, `OBJECT_GONE` `DEAD` with its binding, and 50 scenery death events (S9's catalogue: 300 of 306 deaths were scenery, LIVE) |
| Remove, silently or with an event | An object leaves DCS with no record, or with `OBJECT_GONE` `REMOVED` |
| Detach, reattach | The link drops; the agent queues its records (6 event records, then a `GAP`); the same sidecar attaches again |
| Lose a result | The next `RESULT` is lost and the link drops with it |
| Refuse destroys | The next 1 to 5 destroys raise in DCS and are answered `DCS_REFUSED` |
| Another campaign | The link drops and a sidecar of another campaign switches the agent to it; that campaign's group, with the same `EntityId`s, is in DCS. This campaign's sidecar switches back when it next attaches. |
| Sidecar restart | The reconciler is rebuilt from the store and attaches as a new sidecar boot |
| Sidecar crash in a write | The sidecar stops (a panic, caught) in the middle of writing a command, after 0 to 3 others, and restarts from what it journalled |
| DCS crash, DCS restart | DCS runs a frame and dies before the sidecar reads it; a new agent boot with empty memory |
| Mission load | A new generation, on the groups' theater or on another |
| Authority, orders, spec | A group moves between campaign and DCS authority; a route or an attack order; a unit type DCS refuses, or one it accepts |

Safety, checked on every command and after every frame:

- A sequenced command is journalled, with its inbound position, before it is
  written, and the transition that led to it before that. Every command DCS took is
  in the journal, including when the sidecar stops in the middle of the write.
- A spawn never includes a unit with a death fact.
- No spawn replaces a group DCS holds.
- Every death fact is a death of this campaign's unit that happened; `dead` is final.
- Every outcome matches what the agent did: `not_sent` was never taken; `unknown` was
  taken unless its agent boot is gone; `applied`, `refused` and `rejected` are the
  outcome of the result code the agent gave. The one exception is a spawn resolved
  `applied` without a result, when the group's last member died while it was
  unanswered (spec/world.md): DCS applied that revision in that generation.
- One epoch never spans two agent boots.

Convergence, after the faults stop, a reattach and 400 frames more: every entity is
`live`, `dead`, `unspawnable`, `lost` or `virtual`, each with a cause (every member
died, or the group was `lost` and the `dead` policy marked it; a refused type; a member
removed, its death record dropped by a `GAP`, or its destroys failed to the limit;
campaign authority or another theater loaded). A `live` group is in
DCS with exactly its members that have neither a death fact nor a missing mark,
holding its order, and an attack order names its target's current DCS group. No entity
outside `dead` has every member dead. Nothing else of the campaign is in DCS, and no
command is unanswered.

## Results

| Test | Runs | Failures |
|---|---|---|
| Seeded schedules, 300 operations, 6 groups and 2 statics, the three lost policies in turn | 20,000 seeds | 0 |
| Proptest, 1 to 199 operations, 1 to 6 groups, 0 to 2 statics, any lost policy | 30,000 cases | 0 |
| Replay of a recorded fake-sim run, the sidecar restarting at up to 5 points the property picks, persistence on: the same death facts as the live run | 500 cases | 0 |
| The same with persistence off (a fresh store at each restart): exactly the deaths read after the last restart | 500 cases | 0 |
| A death queued while detached, read by a sidecar restarted without its store | 1 | 0 |
| `materializing` -> `dead` (a lost `RESULT`, then every member dies; the spawn resolves `applied`), `lost` -> `dead`, and a death with a wrong binding | 1 each | 0 |
| A fixed `unspawnable` group materializes; a refused destroy stops at the limit and the entity goes `lost`, then leaves DCS once destroys work; an attack order follows its target when the target respawns in the same generation; a route advances with no new order; a `materializing` group under campaign authority is withdrawn with no spawn; a death while `dematerializing` | 1 each | 0 |
| A death queued under another campaign and read by a sidecar of a campaign with the same `EntityId`s, without its store: live, and replayed with 5 sets of restarts; and the window between a campaign switch and the next load | 1 each | 0 |
| The seeds reach `live` -> `dead`, and never `virtual` -> `dead` | 200 seeds | 0 |
| A restart after the last `FRAME`, with persistence off, leaves no death fact | the seeds of 64 whose recording ends after its last `FRAME` | 0 |
| Conformance suite on the fake sim (9 checks) | 1 | 0 |
| S9's recording, 841 records (93 `FRAME`, 1 `HELLO`, 34 `LIFECYCLE`, 2 `RESULT`, 694 `EVENT`, 17 `LOG`), the sidecar restarting at 1 to 7 points, with persistence on and off | 64 | 0 |

On S9's recording every record decodes, every `EVENT` is read, the only kind the
reconciler does not read is `LOG`, and no death fact is journalled, since no object of
the recording is the platform's. With persistence on the store ends on the generation
and theater of the last `LIFECYCLE`; with it off, on that generation, taken from the
stamps read after the last restart, and on that theater when a `LIFECYCLE` follows the
restart. Its deaths reach the reconciler as `EVENT` hints only;
death facts on real records need S10's object-table records.

`mise run check` runs 64 seeds, 96 proptest cases and 48 cases of each replay
property. The larger runs above: `NANOPROBE_SEEDS=20000 cargo test --release -p
nanoprobe-sim-fake --test dst`, `PROPTEST_CASES=30000 cargo test --release -p
nanoprobe-sidecar --test reconciler` and `PROPTEST_CASES=500 cargo test --release -p
nanoprobe-sidecar --test replay`. `PROPTEST_CASES` sets the cases of every property in
`crates/sidecar/tests`. A seed replays with `NANOPROBE_SEED=<hex>`. The recordings test reads
`NANOPROBE_RECORDINGS`, a folder of `*.jsonl` files, and skips when it is unset.

## The tests detect defects

Each change below was made to the reconciler or the fake alone. The named test failed
on it, with the seeds `mise run check` runs unless a count is given:

| Change | Failed |
|---|---|
| A command is journalled after it is written | Seeds: a command DCS took is not journalled (the crash in a write) |
| A spawn includes units with a death fact | Seeds |
| A new generation is ignored | Seeds |
| Every unanswered command resolves `not_sent` | Seeds |
| No new epoch for a new agent boot | Seeds |
| No group is ever `lost` | Seeds |
| A dematerializing entity is never destroyed after its capture | Seeds |
| Only rows the mirror bound go missing at an enumeration | 3,000 seeds |
| A target's respawn in the same generation does not move its attackers' order revision | `an_attack_order_follows_its_target_when_the_target_respawns` |
| A refused destroy is sent again without a limit | `a_refused_destroy_stops_at_the_limit` |
| A `materializing` group under campaign authority is never withdrawn | `a_materializing_group_under_campaign_authority_is_withdrawn` |
| `OBJECT_GONE` stamped with another campaign counts | Seeds, and the three campaign tests |
| The fake answers a spawn or order at its revision as new | Conformance |
| An attach keeps the commands it did not take | Conformance: `a_reattach_answers_what_was_taken` |

Ignoring `OBJECT_GONE` `MISSING` and `REMOVED` is not detected: the enumeration marks
the same members missing, so the end state is the same.

## Found and fixed by these tests

- A restarted sidecar resolved every unanswered command `unknown`: the journal held no
  inbound position. The adapter gives the position before the write, and the
  reconciler journals it first (spec/commands.md).
- An order refused with `NOT_BOUND` was never sent to the group spawned again.
- A respawn of a `lost` group at its old spec revision is answered `AT_REVISION`, so
  nothing spawns. It takes a new revision (spec/world.md).
- A death the agent queued while no sidecar was attached could not be tied to its unit
  after a restart. `OBJECT_GONE` carries the row's binding (the coordinator's ruling,
  written into the spec by S10).
- A member whose `OBJECT` was lost to a `GAP` was never marked missing.
- A sidecar that lost its store dropped the deaths queued in a generation before the
  one it first saw named.
- A route's progress stayed put when the group passed a waypoint between two position
  samples. A group seen at a later waypoint has passed the ones before it.
- A `lost` entity whose destroys had failed to the limit, and which DCS still held,
  was spawned again by the respawn policy over the object DCS held.
- After a spawn answered `AT_REVISION` to a resend, the reconciler kept believing the
  agent held the group's order, so the respawned group never got it.
- A death the agent queued under another campaign counted against this campaign's
  entity of the same `EntityId`. The stamps carry the campaign, and such records are
  dropped.

## Not covered

The fake does not model pause and the pending-destroy set, `CONTROL_LOST` and its
resync `HELLO`, the commanded-work budget in time, or effects. A spec change to a
`live` group is not reconciled: the lifecycle has no transition that spawns it again.
S10's recordings, with `OBJECT`, `OBJECT_GONE` and enumerations from DCS, are needed
for the part of the exit test on real object-table records.

A command of an agent boot that died resolves `unknown`, also when DCS never took it:
the sidecar does not read the status block's `in_head`, and the rule of
spec/commands.md, "After DCS dies", is not yet safe for commands taken after the last
read. Step S14 settles it.

The fake writes a `FRAME` right after a `CAMPAIGN` that changes the campaign, and in a
replayed queue wherever any stamp changes. The agent's doing the same is step S10's.
