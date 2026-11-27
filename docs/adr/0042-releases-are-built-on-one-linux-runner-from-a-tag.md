# ADR 0042: Releases are built on one Linux runner from a version tag

## Status

Accepted. Amended by [ADR 0054](0054-the-fake-sidecar-ships-in-releases.md) and
[ADR 0055](0055-releases-carry-a-macos-fake-sidecar-and-the-docs.md).

## Context

A release holds Windows files and Linux files. A DCS server runs on Windows, or on
Linux under Wine, so both are first-class (CLAUDE.md, "Platforms"). The agent DLL
targets `x86_64-pc-windows-msvc` and is cross-built from macOS and Linux.

The staging script refuses to run on Windows, and its path remapping handles POSIX
paths only. No DLL built on a Windows host is tested in DCS (UNVERIFIED).

The agent's PDB holds the absolute path of every object file
the linker read, so it must stay out of public files.

## Decision

A pushed tag `v<workspace version>` builds every released file on one Linux runner of
GitHub Actions with `mise run package`, and publishes them at once as the release. The
Windows CI job builds and tests on Windows; no file it builds is released.

- The workflow (`.github/workflows/release.yml`) fails unless the tag is `v` plus the
  workspace version, the tagged commit is on `main`, the version's section of
  `docs/release-notes.md` is final, and no release or draft has the tag.
- A manual run of the same build job is a dry run: it keeps the files as workflow
  artifacts and makes no release.
- The workflow has no acceptance gate. Testing against the project's criteria happens
  before the tag, on files a dry run built.
- The build job has `contents: read`. The publish job has `contents: write`, takes the
  files by artifact, checks them against `SHA256SUMS` and runs only the runner's `gh`.
  Actions are pinned by commit, and the release workflow uses no cache and no secret
  beyond the job's own token.
- A release has five assets: `nanoprobe-<version>-windows-x86_64.zip`,
  `nanoprobe-<version>-linux-x86_64.tar.gz`, `nanoprobe-<version>-schema.tar.gz`,
  `nanoprobe-<version>-schema.binpb` and `SHA256SUMS` (spec/install.md, "Release").
- Each platform archive holds `nanoprobe` (doctor, install, uninstall, inspect, top,
  watch, restore), `nanoprobe-sidecar`, the payload, `README.txt` and `LICENSE`. The
  Windows archive adds `install.cmd`, `uninstall.cmd` and an example `sidecar.toml`.
  The Linux archive adds `install.sh`, `uninstall.sh` and `deploy/`; its two programs
  are static.
- The installer writes no `sidecar.toml` and places no sidecar store or log. The user
  runs the sidecar from the unpacked folder, or from a container image built locally
  from the archive's program.
- The schema is the only code-generation input. Users generate client code from it
  with their language's tools. The release publishes no client package, no container
  image and no Buf Schema Registry module. Every schema file sets
  `java_package = "nanoprobe.v1alpha1"`.
- The agent's PDB is a workflow artifact kept 14 days, never a release file. The
  archive test fails when a PDB, a runner path or an unexpected file is in an archive.
- No fake sidecar, conformance command or test tooling ships. The conformance suite
  and `cargo xtask` run from source.
- There is no macOS sidecar, since a DCS server runs on Windows or under Wine.

Rejected:

- A Windows runner as the builder: the staging and remapping scripts do not run there.
- Publishing the PDB: it names the builder's paths.
- A draft that a second workflow publishes after an acceptance run. Testing against
  the project's criteria happens before the tag, on files a dry run built. The check
  in docs/releasing.md keeps the tagged source equal to the tested source, so the
  released files differ from the tested files only in bytes.

## Consequences

- The tag's build is a rebuild. Its bytes differ from a dry run's: the archives keep
  file times, and cargo-xwin downloads Microsoft's current MSVC CRT and Windows SDK, so
  two builds may link different versions.
- A published version is not built again. The build refuses when a release for the
  tag exists, so the next release raises the version.
- Before a tag, a maintainer checks that the commit differs from the last tested dry
  run only in files the release build does not read (docs/releasing.md).
- Uninstall removes the whole `<profile>/nanoprobe` folder, including `missions/`.
  The install guide says so.
- Unexpired workflow artifacts of a public repository can be downloaded by any
  signed-in GitHub user. The PDB artifact expires after 14 days.
