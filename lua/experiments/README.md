# Experiments

Eval snippets for the experiments in step S4. Each file has the probe header of
`spec/eval.md` and runs through the same runner as the probes:

```
cargo xtask probes <target> --dir lua/experiments --only <text>
```

The files in `alone/` end the DCS process on purpose, so the runner never picks
them up. Run each with `cargo xtask eval <target> <state> <file>` on a server with no
players, and let DCS restart.

Observations are recorded with `info` or `timing`, apart from the ones a pass rule
decides, which carry an expected value. The results and their rules are in
`docs/results/`.

## E38

The snippets in `e38/` belong to experiment E38, on reading unit life and ammunition.
The runner does not pick them up: each is run alone with
`cargo xtask eval <target> <state> <file>`, in the state its header names, on a server
with no players. `hook-log.lua` installs a logger on its first run and returns the log
on the next; it runs in the hook state and in `gui`. `store.lua` reads what a fight
left, so it runs after `coverage.lua`. `coverage.lua`, `classify.lua` and `rearm.lua`
each take six to eight minutes.
`lua/tests/e38.lua` rehearses them on the mock. The results are in
`docs/results/2026-11-14-e38-life-and-ammunition.md`.

## S15

The snippets in `s15/` belong to the experiments E6, E8, E14 and E16 of step S15 on
world commands. The runner does not pick them up: each is run alone with
`cargo xtask eval <target> <state> <file>`, in the state its header names, on a server
with no players, and cleans up what it adds. They use `E.lib('s15')`
(`lua/scenarios/s15.lua`: spawns with ammunition, flares and chaff, a capture read, a
text encoder) and the scenario library, which `deploy --dev` installs.

| File | State | Settles | Takes |
|---|---|---|---|
| `e06-warehouse.lua` | `server` | E6: set then read back, on a limited warehouse | a second |
| `e08-capture.lua` | `hook` | E8: an aircraft, a helicopter and a ship captured, destroyed, added again and compared field by field | 25 s of model time |
| `e08-attack.lua` | `server` | E8: whether an attacker engages when its AttackGroup task is issued again with the new group ID | 108 s of model time |
| `e14-ammo.lua` | `server` | E14: which `getAmmo` entry each ground-unit `ammo` slot feeds, and the offset, for ten types | about 80 s of model time |
| `e21b-names.lua` | `server` | E21b: whether platform names may use a base-64 alphabet: case, every character, leading `.` `#` `!` `~`, found and read back byte for byte | 6 s of model time |
| `e-sound-picture.lua` | `hook` | `outSound*` calls and the mission-state `a_out_picture*` calls, each recorded as accepted or raised (client playback and display UNVERIFIED) | about a second |
| `e-zones.lua` | `hook` | Run-time trigger zones: `trigger.misc.addZone`, `getZone`, the zone event and a trigger on it. Recorded; zones are dropped from the platform | timeout 300 s |
| `e-waypoint-scripts.lua` | `hook` | Waypoint `Script` actions: when each fires, in which state, at what cost, while paused, and after a re-route | timeout 300 s |
| `e16-damage.lua` | `hook` | E16: `a_unit_set_life_percentage` at 50 and 0 on aircraft, helicopter, ship, static and a ground control | 20 s of model time |

E6 needs a mission packed with `cargo xtask miz ... --limited-airport ID`, which makes
that airbase's warehouse limited; the sound and picture snippet needs a mission packed
with `--sound-picture`, which adds `np-tone.wav` and `np-picture.png`.

Each records one observation per step, with `info`. `e06-warehouse.lua` says in
`finding` when every warehouse of the mission is unlimited, which makes no setter
readable. `e08-capture.lua` gives `comparison`, a table per subject with each field's
first read, second read, difference, allowed difference and verdict, and
`reads_for_flares_chaff_pylons`. `e14-ammo.lua` gives `type_<n>_<type>` with `slots`
(each slot's row, count asked and count read) and `verification`.

`cargo xtask routes <target>` is experiment E37, on a docker-wine target. It deploys a
development build, restarts DCS, loads a fresh shell mission, and for each route shape
of `lua/scenarios/routes.lua` adds the same number of ground units (`--units`, 400),
settles 20 s, counts frames for 45 s, and removes them. Everything is a flag: the
shapes, units, sweeps, settle and measure seconds, path length, waypoint step, speed,
unit type, lane spacing and the formation names. It writes `routes.json` and
`routes.txt` under `bundles/routes-<target>-<time>/`: mean and p5 frames per second,
model/real, DCS CPU and the share of groups that moved, per shape.

`lua/tests/s15_experiments.lua` and `lua/tests/routes.lua` rehearse them on the mock.
