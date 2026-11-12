# Review workflow

`review-round.js` is a Claude Code workflow that reviews a finished step before it is
committed. Run it as `/review-round`, or from a session with
`Workflow({ name: "review-round", args: {...} })`.

## The loop

1. Finish the step and get `mise run check` to pass.
2. Run a round (`mode: "round"`): 3 reviewers with different lenses, 1 agent that
   merges duplicate issues, and 3 voters. It returns `accepted` (two votes or more)
   and `rejected`.
3. Read each accepted issue and check it against the code before fixing it; a vote
   does not prove an issue. Fix the real ones and run `mise run check`.
4. Run the next round with what was fixed (`fixed`) and what was rejected
   (`rejected`), so the reviewers check the fixes and do not raise the same issues.
5. Stop early when a round accepts nothing.
6. When the planned rounds are done, and not after an early stop, run the judge
   (`mode: "judge"`): one agent that reads all of the work and rules whether it is
   complete and mergeable. Fix what it calls blocking, then commit.

## How many rounds

Choose before the first round:

| Work | Rounds |
|---|---|
| A specification, a wire format, the public API, or code that runs inside DCS | 3 |
| Host-only code and tooling of ordinary size | 2 |
| A small change: a fix, a test, a document edit of a few dozen lines | 1 |

Add a round, up to 5, when the last planned round still accepted a High issue, or
when its fixes changed the design and not only the wording.

## Arguments

| Key | Meaning |
|---|---|
| `mode` | `"round"` (default) or `"judge"` |
| `root` | The repository's absolute path |
| `step` | The step, such as `S6 Transport and inspector` |
| `work` | What to review: the files, how to see the change (`git status`, `git diff`), the exit test, and what belongs to a later step |
| `context` | What the work must satisfy, with absolute paths: plan sections, specs, results documents |
| `lenses` | Optional. Three `{key, prompt}` for the reviewers. The defaults are conformance, correctness, and tests and evidence. Write others when the work is not code. |
| `round`, `of` | The round's number and the planned total |
| `fixed` | Short lines: what was fixed after earlier rounds |
| `rejected` | Short lines: what was rejected, by the vote or after reading |
| `intended` | Known and intended differences, which reviewers do not report |
| `history` | Judge only: one line per round |

## Notes

- A round runs 7 agents and takes 10 to 15 minutes.
- Later rounds mostly find defects in what earlier fixes added. Keep fixes small.
- Say in `work` what is out of scope and in `intended` what is deliberate, or the
  reviewers report it again.
- The agents are read-only. Tell them in `work` not to start a long run, such as the
  1,000-cycle `cargo xtask peerkill`, while one is in progress.
