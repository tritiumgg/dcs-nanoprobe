# Turning on CI and cutting a release

For a maintainer with admin rights on the GitHub repository. Run the commands from a
checkout of the repository; `gh` fills in `{owner}/{repo}` from it. The steps go in
order.

Two workflows take part:

| Workflow | Started by | Does |
|---|---|---|
| `ci.yml` | a push to main, a pull request, or by hand | `mise run check` and `mise run dll` on Linux; a build and the checks on Windows |
| `release.yml` | a pushed tag `v<version>`, or by hand as a dry run | builds every released file on one Linux runner with `mise run package` and publishes them as the release; a dry run keeps the files as workflow artifacts and makes no release |

Pushing commits and tags and starting a workflow by hand (`gh workflow run` is a write
to the repository) are the maintainer's. A development agent reads with `gh`, which
includes downloading a run's artifacts, and starts a workflow only on the maintainer's
word.

| Step | Who |
|---|---|
| 1. Turn on Actions (once), 2. push main, 3. the release notes and the dry run, 4. the signed tag | the maintainer |
| 5. Keep the released DLL's PDB | the maintainer or an agent |

## 1. Turn on GitHub Actions

```bash
gh api -X PUT 'repos/{owner}/{repo}/actions/permissions' -F enabled=true -f allowed_actions=all
```

The workflows use actions from outside GitHub's own (`jdx/mise-action`,
`Swatinem/rust-cache`), each pinned to a commit, so the policy must allow them. Check
the result:

```bash
gh api 'repos/{owner}/{repo}/actions/permissions'
```

It shows `"enabled": true` and `"allowed_actions": "all"`. The repository's default
token permission can stay read-only: each workflow names the permissions it needs.

## 2. Push main

A push publishes every commit not yet on GitHub. CI scans the history for secrets
only after the push, so scan it first and push only when it reports no leaks:

```bash
mise exec -- gitleaks git --redact --no-banner .
```

```bash
git push origin main
```

The push starts the CI workflow (`.github/workflows/ci.yml`): the jobs Linux and
Windows. A few seconds after the push, watch the run for the pushed commit; the command
exits nonzero when a job fails:

```bash
gh run watch --exit-status "$(gh run list --workflow ci.yml --commit "$(git rev-parse main)" --limit 1 --json databaseId --jq '.[0].databaseId')"
```

What to expect on the first run:

- Linux runs `mise run check` and then `mise run dll`, as a developer does.
  The first run installs every tool in `mise.toml` and builds from nothing; later runs
  restore caches that pushes to main save. On Linux the job first frees disk space by
  deleting the image's preinstalled toolchains, and installs `libreadline-dev
  libncurses-dev` with apt for mise's Lua build.
- The breaking-change check compares the schema with the commit before the push. When
  that commit holds no schema, as on the first push of the schema, it reports "nothing
  to compare" and passes.
- Windows runs format, clippy, the tests, the virtualization test, the generated-code
  check, the per-crate Windows check, and the agent DLL with its checks, built
  natively. No file it builds is released.

## When a CI job fails

Open the failed step's log:

```bash
gh run view --log-failed
```

Reproduce it on a development machine with the same command: `mise run check` or
`mise run dll` on Linux or macOS. The tools are the same versions as CI's.

- A test that fails only on the runner and passes locally may be a timing limit, or a
  race that the runner's fewer cores expose. Before a rerun, look for a race: slow the
  thread the test waits on and reproduce it locally, as the slow-sidecar variants in
  `crates/fake-agent/tests/misbehave.rs` do. With no race found, rerun the failed jobs
  once (`gh run rerun --failed`); if it fails again, it is a real failure on that
  machine.
- A failure in installing a tool (a download error from mise) is usually transient:
  rerun the job.
- A failure on Windows only cannot be reproduced on Linux or macOS. The log names the
  step; the build, clippy and test steps run there as on any Windows machine with the
  Visual Studio Build Tools and LLVM.
- The breaking-change check compares against the base the event gives, which a local
  `mise run check` does not know. Search the log of the step that runs
  `mise run check` for "comparing the schema against" or "no schema at": the line
  names the base and its merge base. Reproduce it with
  `mise exec -- sh tools/protobreak.sh <commit>`.
- The secret scan of the history: reproduce it with
  `mise exec -- gitleaks git --redact --no-banner .`. A secret it finds is already on
  GitHub: treat it as exposed and rotate it before anything else.

Fix the cause on a branch, merge it to main, and push again. Do not cut a release from
a commit whose CI failed.

## 3. Dry run of the release build

Every release needs a dry run: its files are the ones checked on a Mac below. The
archives carry `docs/release-notes.md`, so finish the version's section and push it to
main first; the check in step 4 fails when the tagged commit differs from the dry run's
in a released file. A dry run does not check the notes, so check them before it with
`sh tools/release-notes.sh <version> /dev/null`. Draft the section from the commits
since the last tag:

```bash
mise run release-notes-draft -- 0.2.0
```

The draft goes to standard output under `## 0.2.0, Draft`. It reads the first-parent
history after the newest `v*` tag (pass another ref as a second argument) and skips
merges, `docs(adr): number ...` and the types docs, test, ci, chore, build, style and
wip. A merge commit stands for the commits of its second parent.

A commit's `Release-note:` trailers, written in its body for a user, become the entries
as written; indented lines continue a trailer. `Release-note: none` drops a commit with
nothing a user sees. A commit of a skipped type with a trailer still gives its entries,
and so does a breaking commit of a skipped type, a commit whose subject is not a
Conventional Commit, and a revert; the last three carry the `TODO`.
A `feat` is Added, a `fix` is Fixed, and the rest, with every breaking commit, is
Changed. A commit with no trailer gives its subject, the ADRs and results it names as
links, and `TODO: reword for users`. Merge the draft into the section by hand, reword
the `TODO` entries, and make the heading the version, such as `## 0.2.0`, without
`Draft`. The section is final
when its heading is the version with optional words after it, such as
`## 0.1.0, the first release`, without "Draft", and no line in it says "To be
completed", "not yet run", "TBD" or "TODO".

Then start the dry run:

```bash
gh workflow run release.yml --ref main
```

It runs the job Build on an `ubuntu-24.04` runner. The job first deletes the image's
preinstalled toolchains and tool cache, which it does not use, for the disk space the
mise tools and the builds need, and installs with apt the readline and ncurses headers
that mise's Lua build needs. Then it runs `mise run package` from nothing, with no
cache, and the archive test `tools/stage-release-test.sh`. That test fails when
an archive holds other files than spec/install.md lists, when a program is not static,
when the macOS fake sidecar is not a universal program for macOS 11 that loads only
the system's libraries, with a signed arm64 build, when the archives' documents differ
or one of their relative links resolves outside its archive, when a file names a home
folder or the checkout, when a PDB is among the release's files, or when the Linux
`nanoprobe-fake-sidecar` does not run. The runner cannot run or verify the macOS
program; the step after the download below does that on a Mac. The files
are kept as the workflow artifact `release-files-<commit>` and the PDB folder as
`agent-symbols-<commit>`, each for 14 days, where `<commit>` is the first 12
characters of the commit they were built from. No release is made.

A few seconds after starting it, watch the run; the command exits nonzero when it
fails. Then print the full commit it built, and download its files and PDB folder into
a folder no task writes, named after that commit:

```bash
run=$(gh run list --workflow release.yml --event workflow_dispatch --branch main --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch --exit-status "$run"
gh run view "$run" --json headSha --jq .headSha
commit=$(gh run view "$run" --json headSha --jq .headSha | cut -c1-12)
gh run download "$run" -n "release-files-$commit" -D "dist/runner-$commit"
gh run download "$run" -n "agent-symbols-$commit" -D "dist/runner-$commit/private"
```

`mise run package` rewrites the release files at the top of `dist/` but never this
folder. Keep the full commit with any test of the files.

Before any tag, check the runner's macOS fake sidecar on an Apple silicon Mac, natively
and as x86_64. `gh run download` marks nothing for Gatekeeper. Each command must
succeed, and `--help` must print its usage:

```bash
mkdir "dist/runner-$commit/macos" && tar -xzf "dist/runner-$commit/nanoprobe-"*-macos-universal.tar.gz -C "dist/runner-$commit/macos"
fake=$(echo "dist/runner-$commit/macos/"*/nanoprobe-fake-sidecar)
codesign -v --strict --arch arm64 "$fake"
"$fake" --help
perl -e 'alarm 60; exec @ARGV' "$fake" --listen 127.0.0.1:0 --run true
```

The x86_64 build runs under Rosetta. Install it first if `arch -x86_64 /usr/bin/true`
fails (`softwareupdate --install-rosetta`), then run:

```bash
arch -x86_64 "$fake" --help
perl -e 'alarm 60; exec @ARGV' arch -x86_64 "$fake" --listen 127.0.0.1:0 --run true
```

If one fails, do not tag: the macOS build moves to a macOS runner job
([ADR 0055](adr/0055-releases-carry-a-macos-fake-sidecar-and-the-docs.md)). The 0.1
acceptance run, which ran once, also tested a dry run's files (docs/acceptance.md).

The first dry run is also the first time `mise run package` runs on a Linux host. If it
fails, the log names the step. The tools come from `mise.toml`: zig as the C compiler
for the static programs, cargo-xwin and LLVM for the Windows files, cargo-zigbuild and
zig for the macOS fake sidecar, LLVM's `llvm-lipo` and `llvm-objdump` for its check,
buf for the schema.
mise builds Lua 5.1 from source, which needs the readline and ncurses development
headers (`libreadline-dev libncurses-dev` on Debian and Ubuntu); the job installs them
with apt. `gh` is pinned in `mise.toml` too. The runner supplies rustup, which installs
the toolchain `rust-toolchain.toml` names, the C compiler and linker for programs built
for the runner itself and for the `cargo:` tools mise installs, `zip`, `unzip`, git and
the shell tools.

## 4. Tag the version

The tag is `v` and the workspace version in `Cargo.toml` (`0.1.0` for the first
release), on a commit on main whose CI passed.

The commit to tag must differ from the tested dry run's commit only in files the
release build does not read: the test results and the ADRs,
the documents for maintainers (`docs/acceptance.md`, this file, `AGENTS.md`), and the
test tooling (`crates/xtask`, the fake agent, `examples/`, and every `lua/` folder but
`lua/hook` and `lua/agent`). The fake sidecar is released, and so are the documents
`tools/release/docs.txt` lists, so they are not among them. This prints nothing when
that holds:

```bash
git diff --stat <dry-run commit> main -- . ':(exclude)docs/results' ':(exclude)docs/adr' \
    ':(exclude)docs/acceptance.md' ':(exclude)docs/releasing.md' ':(exclude)AGENTS.md' \
    ':(exclude)crates/xtask' ':(exclude)crates/fake-agent' \
    ':(exclude)examples' ':(exclude,glob)lua/[!ah]*/**'
```

If it lists a file, the tested files are not the source being tagged: run a dry run of
main and test again. Check that main's CI passed (the watch command in step 2), and tag
the same commit:

```bash
git tag -s v0.1.0 -m 'nanoprobe 0.1.0' main
git push origin v0.1.0
```

The tag push starts `release.yml`. A few seconds later, watch it:

```bash
gh run watch --exit-status "$(gh run list --workflow release.yml --branch v0.1.0 --limit 1 --json databaseId --jq '.[0].databaseId')"
```

The commit in the tag run's artifact names is `git rev-parse 'v0.1.0^{commit}' | cut -c1-12`. The workflow that runs is the one in the tagged
commit, so tag a commit that holds `.github/workflows/release.yml`. Before it builds,
the job Build checks that the tag is `v` plus the workspace version, that its commit is
on main, that the version's section of `docs/release-notes.md` in that commit is final
(`tools/release-notes.sh`), and that no release has the tag. Then it builds as in the
dry run. The job Publish checks the files against their `SHA256SUMS`, refuses if any
release or draft has the tag, and publishes the release: the three archives, the schema's
two files and `SHA256SUMS`, with the notes from the tagged commit, relative links made
absolute to the tag. It is marked the latest release. Check it:

```bash
gh release view v0.1.0 --json isDraft,assets --jq '{isDraft, assets: [.assets[].name]}'
```

A correct release shows `"isDraft": false` and exactly six assets:
`nanoprobe-0.1.0-windows-x86_64.zip`, `nanoprobe-0.1.0-linux-x86_64.tar.gz`,
`nanoprobe-0.1.0-macos-universal.tar.gz`, `nanoprobe-0.1.0-schema.tar.gz`,
`nanoprobe-0.1.0-schema.binpb` and `SHA256SUMS`. On
the release page, the notes' links open files under `blob/v0.1.0`. If any of this is
wrong, delete the release and the tag ("When a release step fails") and fix the cause
before tagging again.

A dry run never runs these parts, so the first tag is their first run: the tag check
against the runner's origin/main, the notes check, the release lookups through the
GitHub API, the checksum check of the downloaded files, `gh release create`, and the
notes' links on a real release page.

The tag's build is a rebuild. Its bytes differ from a dry run's: the archives keep file
times, the builds are not reproducible, and cargo-xwin downloads Microsoft's current
MSVC CRT and Windows SDK for each build, so the two builds may link different versions
of them.

## 5. Keep the released DLL's PDB

The PDB of the released DLL is the tag run's `agent-symbols-<commit>` artifact, kept
14 days. Copy it to the development machine before it expires, beside the folders
`mise run package` keeps:

```bash
run=$(gh run list --workflow release.yml --branch v0.1.0 --limit 1 --json databaseId --jq '.[0].databaseId')
gh run download "$run" -n "agent-symbols-$(git rev-parse 'v0.1.0^{commit}' | cut -c1-12)" -D dist/private
```

`dist/private/<signature>/` then holds `nanoprobe.dll` and `nanoprobe.pdb`. Copied into
`target/symbols/`, the folder lets `cargo xtask symbolize` turn a crash record of the
released DLL into function names (spec/install.md, "Release").

## When a release step fails

Nothing is public until the job Publish succeeds.

- **The dry run fails.** Fix the cause on main, push, and run it again.
- **The tag check fails.** The message says why: another version, or a commit off
  main. Delete the tag and tag the right commit:
  `git push origin :refs/tags/v0.1.0 && git tag -d v0.1.0`, then step 4.
- **The notes check fails.** The message names the placeholder or the heading. Finish
  the notes on main, push, delete the tag as above, and go back to step 3: the archives
  carry the notes, so the new commit needs its own dry run and Mac check before the tag.
- **The build fails.** No release is made. Fix the cause on main, push, delete the tag
  as above, and go back to step 4: after a tested dry run, a fix in a file the release
  build reads needs a new dry run and a new test before the tag.
- **A release for the tag exists.** The build refuses, so a second run cannot replace
  published files. A published version is not built again: raise the workspace version
  for the next release. Only a release that should not exist, such as one left by a
  failed run, is deleted (`gh release delete v0.1.0 --yes`); then delete the tag as
  above and go back to step 4.
- **The job Publish fails after Build passed.** Read the log. If `gh release create`
  left a partial release or draft, delete it as above. Within a day, while the run's
  `release-notes-<commit>` artifact exists, rerun only the failed job:
  `gh run rerun <run id> --failed`. Later, or when the fix is in a workflow or a
  script, delete the tag and go back to step 4.

## Before the repository goes public

Workflow artifacts of a public repository can be downloaded by any signed-in GitHub
user until they expire: the release workflow's `release-files-<commit>` and
`agent-symbols-<commit>`, which holds the agent's PDB (14 days each), and
`release-notes-<commit>` (1 day). CI keeps no
artifact. List the unexpired ones:

```bash
gh api --paginate 'repos/{owner}/{repo}/actions/artifacts' --jq '.artifacts[] | select(.expired | not) | [.id, .name, .expires_at] | @tsv'
```

Delete any that should not become public, by its ID:

```bash
gh api -X DELETE 'repos/{owner}/{repo}/actions/artifacts/<id>'
```

Run logs become public too. They hold the runner's paths and the tools' output, and no
secret: the workflows use only the token GitHub gives each run.
