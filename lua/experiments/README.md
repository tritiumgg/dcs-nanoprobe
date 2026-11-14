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
