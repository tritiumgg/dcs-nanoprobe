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
