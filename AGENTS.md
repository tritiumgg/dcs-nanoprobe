# AGENTS.md

Rules for AI agents that change this repository.

This repository builds a platform that lets external programs run a persistent campaign
on a DCS World server. DCS simulates; a Rust DLL inside DCS (the agent) streams state out
and applies commands; a Rust sidecar process owns the world and serves a gRPC API.
`nanoprobe` is the working project name.

Writing a controller: read `docs/controller-guide.md`. Running a server: read
`docs/operator-guide.md` and `docs/configuration.md`.

## Rules that are not negotiable

- **The DCS install is read-only.** Never write, move or execute anything inside it.
  The platform's files go only into the Saved Games profile. A design that needs an
  install edit is a last resort and needs a maintainer's decision.
- **No hooking or patching of DCS binaries.** Calling functions DCS exposes to Lua,
  documented or not, is fine.
- **No DCS community frameworks or bridges as sources**, online or on disk: MOOSE, MIST,
  DCS-gRPC, DCS-BIOS, Olympus, Tacview and SRS scripts, and similar. Eagle Dynamics' own
  shipped files and documents are allowed. Community API reference pages are leads to
  verify, never proof.
- **Nothing specific to one person's setup and no secret goes into this repository.**
  No hosts, users, container names, machine descriptions or personal paths, in code or
  in documents. They live in files outside the repository. Do not commit logs, bundles,
  recordings, missions, or any file copied from a DCS install.
- **Development and test tooling stays generic and out of releases.** It may live in
  this repository, but it reads every target from configuration outside it, and a user
  builds it from source to run it.

## Evidence

- Do not take documentation at its word. Check against Eagle Dynamics' files or a
  running DCS.
- Tag every DCS claim: LIVE (observed on a running build), ED-CODE (read in Eagle
  Dynamics' shipped files), LOG (seen in a DCS log), PRIOR (recorded elsewhere, not
  re-checked) or UNVERIFIED.
- Every result obtained from a running DCS is committed with the script that produced
  it, the DCS build, the platform (Windows, or Wine with its version), and the date.

## Platforms

- The project must build and pass host tests on macOS, Linux and Windows.
- DCS servers on Windows and on Linux under Wine are both first-class. Do not use a
  Windows facility that a native Linux process cannot share with a Wine process: named
  shared memory, named events, named pipes.
- The agent DLL targets `x86_64-pc-windows-msvc` and is cross-built on macOS and Linux.
  Keep Windows-only code behind the platform layer so the agent's logic runs in host
  tests under stock Lua 5.1.

## Code inside DCS

- A crash in the agent takes the server down. Every entry point catches panics.
- A Lua error must never unwind through a Rust frame that owns a value. Entry points
  own nothing and call a separate worker; anything that can raise runs under
  `lua_pcall`. CI checks this shape; do not weaken the check.
- No threads in the agent. All work happens on the frame pump, under the frame budget.
- The event handler must not raise, and never calls a method on an event object.
- Time on the sim thread is the project's main constraint. Measure before and after any
  change to the agent, against the stored baseline.

## Working method

- Every development tool comes from `mise.toml`. Run `mise install`, then
  `mise run setup` once to enable the secret-scanner hook. `mise run check` runs what
  CI runs on the host.
- Rehearse every in-game test against the mock or the fake sim before running it on a
  real server.
- Diagnostics bundles are built from an allow-list and redacted. Never read or copy
  `serverSettings.lua`, `network.vault`, `autoexec.cfg` contents beyond the two `net.*`
  keys, or container environment variables.

## Conventions

- Rust, MIT license. Toolchain pinned in `rust-toolchain.toml`.
- Public API changes go through the Protobuf schema and must pass the Buf breaking check.
- The transport layout and opcode set are specified in `spec/` before they are coded;
  change the spec and the golden file together.

## Commands

| When | Run |
|---|---|
| Once per checkout | `mise install`, then `mise run setup` (the secret-scanner hook) |
| Before `mise run check` | `cargo fmt --all`, then the tests of the crates you changed |
| Before every commit | `mise run check`: what CI runs on the host |
| After a change to a file the DLL build reads (`crates/agent`, `crates/agent-link`, `crates/transport`, `vendor/lua`, the workspace `Cargo.toml` and `Cargo.lock`, the toolchain) | `mise run dll`: cross-builds the DLL and checks its imports, exports and entry-point shape |
| After a change to `proto/` | `mise run proto`: Buf lint, the breaking check, generated code current |
| After a change to the transport tables in `spec/transport.md` | `mise run spec`: rewrites `spec/transport.golden` |
| After an intended change to a store encoding or a drawing's DCS call | `NANOPROBE_BLESS=1 cargo test -p nanoprobe-sidecar -p nanoprobe-api --no-default-features`: rewrites the `store.*` and `event.*` lines of `spec/persistence.golden`, and `spec/drawing.golden`. Review the diff. |
| After an intended change to what the shell writer writes | `NANOPROBE_BLESS=1 cargo test -p nanoprobe-sidecar --lib shell`: rewrites `spec/shell.golden`. Review the diff. |

On Linux, install `libreadline-dev` and `libncurses-dev` before `mise install`.

## Workflow

- Change `spec/` first for a transport layout, opcode, record or store change. Commit
  the spec and its `spec/*.golden` file together.
- Change the API only in `proto/nanoprobe/v1alpha1/`. A new error reason goes into
  `spec/errors.md` and `ErrorReason` together.
- Tag every DCS statement in `spec/`, `docs/` and code comments with an evidence tag.
  Link a LIVE claim to its results document in `docs/results/`.
- Record a decision other parts depend on as an ADR in `docs/adr/`.
- Update every document a change affects, in the same commit: `README.md`, the guides
  in `docs/`, `AGENTS.md` and `skills/nanoprobe/`. Keep each one's audience: the README
  is for people, the guides for people and AI agents, `AGENTS.md` and the skill for AI
  agents.
- Test new behavior against `crates/fake-sidecar`, `crates/fake-agent` and
  `crates/sim-fake` before a DCS server.
- `cargo xtask` (`crates/xtask`) deploys, probes and collects redacted bundles on a
  target named in a file outside the repository. Run its live commands under
  `mise exec --`. Never ship it in a release.

## Commits

- Write Conventional Commits: `type(scope): summary`, imperative, under 72 characters.
- Put one `Release-note:` trailer in the body per user-visible change, worded as the
  release-notes line. Write `Release-note: none` when users see nothing.
  `tools/draft-release-notes.sh` reads them (`docs/releasing.md`).

## Where to look

| Question | File |
|---|---|
| Public API fields | `proto/nanoprobe/v1alpha1/*.proto` |
| Errors, outcomes, roles, health | `spec/errors.md` |
| Entities, lifecycle, revisions | `spec/world.md` |
| Agent commands and opcodes | `spec/commands.md` |
| Settings | `spec/config.md` |
| Stores, backups, migrations | `spec/persistence.md` |
| Mission shells: weather, briefing, files | `spec/persistence.md`, "Mission shells" to "When the content takes effect" |
| Install layout, doctor, kill switch | `spec/install.md` |
| Design decisions | `docs/adr/` |
| Live evidence | `docs/results/` |
