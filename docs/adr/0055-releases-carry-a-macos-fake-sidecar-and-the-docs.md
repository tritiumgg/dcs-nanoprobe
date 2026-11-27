# ADR 0055: Releases carry a macOS fake sidecar and the documents

## Status

Accepted. Amends [ADR 0042](0042-releases-are-built-on-one-linux-runner-from-a-tag.md):
a release has six assets, not five, and its "There is no macOS sidecar" does not apply to
the fake sidecar. Amends [ADR 0054](0054-the-fake-sidecar-ships-in-releases.md): the
macOS archive holds the fake sidecar alone.

## Context

A controller author on a Mac runs the fake sidecar to develop without DCS. Built from
source it needs a checkout and a Rust toolchain. No DCS server runs on macOS, so the
agent, the sidecar and `nanoprobe` have no use there.

The documents and the skill live only in the repository. An archive without them sends
a user to GitHub's `main` branch, which can describe a later version than the one
unpacked.

Builds of `nanoprobe-fake-sidecar` for `aarch64-apple-darwin` and `x86_64-apple-darwin`
link no framework. The only C code is the bundled SQLite. `cargo-zigbuild` links both
against the macOS library stubs zig ships. In Linux containers for x86_64 (the release
runner's platform) and for arm64, with no macOS SDK on the machine, it built a universal
program that, on an arm64 Mac, passes `codesign -v --strict` for its arm64 build, loads
only `libSystem`, and serves and runs a program natively and under Rosetta (2026-11-27,
Rust 1.99.1, zig 0.15.2, cargo-zigbuild 0.24.1, the `rust:1.99.1-bookworm` image). The
Linux runner itself has not built it yet.

With `debug = "line-tables-only"`, the macOS program's debug map names each object file
the linker read by its absolute path, the toolchain's under the home folder. The path
remapping of `tools/remap.sh` does not reach it.

## Decision

- The release has a third archive, `nanoprobe-<version>-macos-universal.tar.gz`, built
  on the same Linux runner by `mise run package` (spec/install.md, "Release"). It holds
  `nanoprobe-fake-sidecar`, `README.txt`, `LICENSE` and the documents.
- The program is one universal file of an arm64 and an x86_64 build, each for macOS 11
  and later. `cargo-zigbuild` is pinned in `mise.toml`, and both Apple targets are in
  `rust-toolchain.toml`. The macOS build alone strips debug information, through
  `--config`; the Windows and Linux programs keep their line tables.
- The arm64 build carries only its linker's ad-hoc signature. The program is not
  notarized. The install guide tells users to download with `gh release download` or
  `curl` and check `SHA256SUMS`, and otherwise to clear the quarantine attribute or use
  "Open Anyway".
- Every archive carries the documents `tools/release/docs.txt` lists, at their
  repository paths and with the same bytes. `tools/release-links.awk` rewrites their
  links: a link to a file the archive holds stays; any other relative link, and a link
  to the repository's `main`, names the release's tag. `tools/release-notes.sh` uses the
  same file.
- The archive test checks the macOS program with LLVM's `llvm-lipo` and `llvm-objdump`
  on any host, and verifies and runs it on a macOS host. Before a tag, a maintainer runs
  the runner's build on a Mac (docs/releasing.md).
- The path chosen is the cross-build on the release's `ubuntu-24.04` x86_64 runner. Its
  first build is a dry run, checked on a Mac before the tag. If a runner build fails
  that step, or the build ever needs a downloaded macOS SDK, the macOS build moves to a
  macOS runner job with `contents: read`, no secret and its tools from mise, whose
  output the Linux job checks by hash, tests with the full archive test, and alone
  lists in `SHA256SUMS`. A change of path updates this ADR.

Rejected:

- A macOS runner job from the start: a second builder whose Xcode the image updates, and
  an artifact hand-off into the publish path, when the Linux runner can build it.
- An arm64-only or a per-architecture archive: an Intel Mac would get nothing, or a
  user would pick between two downloads.
- A separate documents archive: a Windows user would need a tar.gz tool, and a program
  archive would not carry its own documents.
- Notarization: it needs an Apple Developer account and its credentials as workflow
  secrets, and the release workflow holds no secret beyond its own token. The project
  does not notarize; ad-hoc signing and the documented quarantine steps are how the
  program runs.

## Consequences

- A change to a listed document changes the release's files, so the dry run's
  comparison excludes only the documents the release does not carry (docs/releasing.md).
  The archives carry `docs/release-notes.md`, so the version's notes are final before
  the dry run, and every release has a dry run.
- Each archive grows by the documents. The release has a third archive to download and
  check.
- A user who downloads the macOS archive in a browser has one manual step before the
  program runs.
- The runner cannot run the macOS program. A failure only a Mac shows is found in the
  maintainer's step before the tag, not in the release workflow.
- The macOS program has no line tables. A panic message still names the file and line,
  which rustc writes as text.
