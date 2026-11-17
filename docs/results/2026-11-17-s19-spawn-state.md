# S19 spawn state: life restore, fuel and ammunition removed, player loadouts

| | |
|---|---|
| Step | S19's spawn-state part: a restore or materialize sets each unit's life; unit fuel, the ammunition tier and AI ammunition are removed; a player-occupied unit's ammunition is read at its birth and its takeoff and reaches the API as its loadout |
| Date | 2026-11-17 |
| DCS | Server: 2.9.31.29434, branch `dcs_server.release`, dedicated server. Client: the same build, a client install |
| Platform | Server: Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker. Client: Windows, driven over SSH with no person at the controls |
| Builds | Branch: agent `ff9c364bba4372c6`, built from `d105afd`; the xtask and the snippets are those of the commit that adds this document, whose only xtask change mounts the persistence sidecar's `/missions` writable. Main: agent `1abe4f8714dd4119` (`6f50c62`) for the overhead A/B |
| Scripts | `cargo xtask sampling <target> --part mirror` (A/B); `cargo xtask persistence <target> --only state` (life restore); `cargo xtask client join`, `lua/experiments/alone/e30-take-slot.lua` and `e31-spawn.lua` on the client, then `lua/experiments/loadout/player-name.lua` and `ai-birth.lua` in the server state, with `link-probe --record` attached as the sidecar (loadout) |
| Rehearsal | `lua/tests/loadout_experiments.lua`, under the mock; `cargo xtask persistence rehearse`; the host tests of the reconciler and the sampler |
| Raw results | Bundles `sampling-<target>-20261117T193944Z` (main) and `...T195016Z` (branch), `persistence-<target>-20261117T200746Z`; the loadout recording is server data and stays out of the repository |
| Result | Life restores and holds LIVE. Overhead stays within the owner's targets. A player's birth gives one `AMMO` record with the right rows; an AI birth gives none |

Every figure is LIVE on these builds unless tagged.

## Removals and the Buf exception

The owner dropped unit fuel and the ammunition tier, and kept ammunition only for
player-occupied units, read on events, for a controller's loadout check. The proto fields are
deleted with their numbers and names reserved. `tools/protobreak.sh` fails against main
on exactly these seven deletions, the documented exception; it passes once main has the
change:

- `UnitSpec` 10 `ammo` and the message `UnitSpec.AmmoEntry`
- `UnitSpec` 9 `fuel`
- `SetSampleRatesRequest` 4 `ammo_period`
- `SampleRates` 3 `ammo`
- `Player` 7 `fuel_used`
- `Unit` 10 `fuel`

The E39 open items on bombs, rockets, gun pods and the Ka-50's two gun belts are moot:
nothing restores ammunition.

## Overhead against main

The mirror part at 2,000 objects, main and then the branch, back to back on the same
server, as S14 did (`2026-11-14-s14-command-path.md`, "Overhead against S11").

| Measure | Main | Branch |
|---|---|---|
| Mean share from the 800 spawn | 0.225 % | 0.236 % |
| Mean share at 2,000, settled | 0.209 % | 0.229 % |
| Largest 10 s window | 0.296 % | 0.307 % |
| Frames over 250 µs | 112 of 41,896 (0.27 %) | 92 of 42,694 (0.22 %) |
| Pump p99, histogram bucket bound | 256 µs | 256 µs |
| Median position interval, stall-free | 1.006 s | 1.001 s |
| Worst stall-free window | 3.66 s | 1.08 s |
| Fill-window position | 1.08 s | 0.98 s |
| Fill time, 800 births | 37 s | 35 s |
| Mirror matches at 500, 1,200, 2,000 | yes, yes, yes | yes, yes, no: one position row 3.47 s old |
| Vitals as a rate at 2,000, settled | 4.13 s | 40.2 s |
| Pumps a second at 2,000, settled | 6.95 | 7.84 |
| DCS fps at 2,000 (p5) | 52.2 (0.39) | 65.9 (0.67) |

Both builds meet the owner's targets: mean share under 0.3 %, and pump p99 under 250 µs,
since fewer than 1 % of frames are over 250 µs on each. The target is on the mean; the
branch's largest 10 s window, 0.307 %, is not judged.

The checks each build fails:

- Main: the pump bound; the vitals band (4.13 s, under the 4.5 to 6.5 s band); one
  stall-free window (3.66 s against 1.25 s). These vary from run to run on this server.
- Branch: the pump bound, variance as on main; the vitals band (40.2 s, over the band),
  from loadout marks, below; the mirror at 2,000, one position row 3.47 s old against
  2 s, in a window with 2 pumps in 10 s, a stall of DCS.

The branch's vitals rate has a cause. Every AI birth leaves a loadout mark, up to the
1,024 the list holds; 152 were dropped at the 1,200 spawn. Each mark costs about 22 µs in
the pump, a `getPlayerName` call and its bookkeeping. The pump hands out as many marks as
the budget left after the tiers pays for at a vitals row's cost, about 1 µs
(spec/sampling.md, "Loadout reads"). The marks then took the budget the vitals tier
would have had. After the 1,200 spawn the list emptied in about 40 s. After the 2,000
spawn it took about 60 s, most of the 103 s settled window that starts when the table
filled. The vitals period stayed at 55 to 59 s until the list emptied, while main's
caught up within 20 s. Positions were not affected.

## Life restore

`persistence --only state`: an F-15C put at 60 % and an M-113 at 40 % through `PutGroup`,
spawned with the simulation running.

| Read | F-15C | M-113 |
|---|---|---|
| After the spawn | 12.0 of 20 (60 %) | 1.2 of 3 (40 %) |
| After a capture, a dematerialize and a materialize | 12.0 of 20 | 1.2 of 3 |
| After a mission reload | 12.0 of 20 | 1.2 of 3 |

The restore `DAMAGE` applies with the simulation running. Whether it lands in the
spawn's own frame while running stays UNVERIFIED (spec/world.md, "Spawn state").

## Player loadouts

The client joined the server's own mission, took the first slot offered (an A-10C_2),
and entered it with `net.spawn_player`. The recorder ran from before the slot-in to after
the AI spawn.

- One `AMMO` record, cause birth, for the A-10C_2: `GAU8_30_AP` 1,150, `HYDRA_70_M151`
  42, `GBU_12` 2, `Mk_82` 6, `AIM_9` 2. `getAmmo` read in the server state gave the same
  rows.
- `Unit.getPlayerName` returned the client's player name (a string) for the A-10C_2, and
  nil for each of 40 AI units read.
- An AI M-113 spawned with the recorder attached (`ai-birth.lua`) was born and got no
  `AMMO` record. The recording holds two `S_EVENT_BIRTH` records: the A-10C_2's at the
  slot-in and the M-113's.

## Open items

- Takeoff: not run. A scripted client cannot fly, so the takeoff `AMMO` is checked only
  on the fake sim.
- Ground-crew rearm of a player aircraft: not run, it needs a person at the client's
  radio menu. Whether any event fires stays UNVERIFIED.
- AI birth marks starve the vitals tier after a large spawn (above), against
  spec/sampling.md "Starvation guard". A mark is priced at a vitals row and costs about
  22 times that.
- Loadout-mark pumps are outside the 5-per-second pump bound (spec/sampling.md, "The
  pump's worth").
- The 1,024-mark loadout list can drop a player's birth mark when more than 1,024 births
  follow it before a pump reads it; `loadouts_dropped` counts it, and the takeoff read
  recovers the loadout.
- `life_held` is kept in memory: a sidecar restart between a restore `DAMAGE`'s send and
  its result loses the hold.
- A respawn before a restore `DAMAGE` resolves files the old result as an unread effect.
- `agent.toml` keeps `deny_unknown_fields`, so a hand-written `[sampling] ammo_s` fails to
  load (no release has shipped).
- The reconciler counters `lives_restored` and `lives_lost` reach no operator surface; a
  lost life logs with `eprintln!`.
- The removal of a speedboat at 50 % by `DAMAGE` (E16), and other ship types: UNVERIFIED.
- Host-test flakes under load, in main's tests: `crates/fake-agent/tests/linksim.rs`
  `predictions_hold_across_wraps_and_a_restart_drops_the_unwritten` failed once in a full
  check and passed 3 of 3 alone. `lua/tests/dispatch.lua` scenario `marks` waited for a
  pump that the hook does not run once the sidecar is gone; it now runs the pump itself.
