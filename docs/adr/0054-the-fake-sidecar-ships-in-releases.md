# ADR 0054: The fake sidecar ships in the release archives

## Status

Accepted. Amends [ADR 0042](0042-releases-are-built-on-one-linux-runner-from-a-tag.md):
its "No fake sidecar, conformance command or test tooling ships" does not apply to the
fake sidecar, and each platform archive holds three programs. Amended by
[ADR 0055](0055-releases-carry-a-macos-fake-sidecar-and-the-docs.md): the macOS archive
holds the fake sidecar alone.

## Context

`nanoprobe-fake-sidecar` runs the real API server over the fake simulation
(`crates/sim-fake`). The hello examples and the API tests use it. Built from
source only, it needs a checkout and a Rust toolchain, which a controller author in
another language does not otherwise need. It reads no
target and no configuration outside its arguments, so it holds nothing specific to one
setup.

## Decision

- Both platform archives hold `nanoprobe-fake-sidecar[.exe]` beside `nanoprobe` and
  `nanoprobe-sidecar`, built the same way: a Windows executable, or a static Linux one
  (spec/install.md, "Release"). The archive test checks it as it checks the others.
- It counts as a tool for controller authors. Its `--help` is written for them; the
  options only the project's own tests use are listed under their own heading.
- The conformance command and `cargo xtask` still do not ship and run from source.

## Consequences

- A change to `crates/fake-sidecar` or `crates/sim-fake` changes the release's files,
  so the release dry run's comparison no longer excludes the fake sidecar
  (docs/releasing.md).
- The fake simulation models DCS from the project's results. Passing against it does
  not prove behavior on a real server; the controller guide says so
  (docs/controller-guide.md, "Test without DCS").
- Each archive grows by one program.
