# The release without the agent's PDB

| | |
|---|---|
| Step | The release is two archives, the API schema's two files and `SHA256SUMS`; the agent's PDB is kept by the builder, beside the DLL it belongs to, and not released (spec/install.md, "Release") |
| Date | 2026-11-18 |
| DCS | None: host only |
| Platform | macOS 26.6.2 on arm64; Rust 1.99.1, cargo-xwin 0.23.1, zig 0.15.2 |
| Script | `mise run package` (`tools/stage-release.sh`, `tools/stage-release-test.sh`, `tools/pdbsig.sh`); then a copy of the kept PDB placed beside the archives as `nanoprobe-0.1.0-agent.pdb` and `sh tools/stage-release-test.sh` run again, the copy removed after |
| Code | The commit that adds this document |
| Result | Pass. `mise run package` exits 0 with every test line `ok`. With the planted PDB the archive test exits 1, and its one `FAIL` line is the planted file |

## What the package run showed

The staged release in `dist/` holds `nanoprobe-0.1.0-windows-x86_64.zip`,
`nanoprobe-0.1.0-linux-x86_64.tar.gz`, `nanoprobe-0.1.0-schema.tar.gz`,
`nanoprobe-0.1.0-schema.binpb` and `SHA256SUMS`. The archive names carry the workspace
version, `0.1.0`. The lines of the archive test this change adds or rewrites:

```text
ok    the kept PDB is the released DLL's, under its signature 4D0E7D424D3FA8424C4C44205044422E1
ok    no PDB is in the release
ok    SHA256SUMS lists the two archives and the schema's two files
ok    SHA256SUMS matches
```

`dist/private/4D0E7D424D3FA8424C4C44205044422E1/` holds `nanoprobe.dll`, equal to the
archives' `payload/nanoprobe_agent.dll`, and `nanoprobe.pdb`. The signature is the one
form a crash record's `pdb` carries: on an earlier build of the same DLL,
`tools/pdbsig.sh` gave the folder name `cargo xtask` gave when it kept that build under
`target/symbols/`. The test checks that the kept PDB holds the DLL's
GUID and names `luaopen_nanoprobe`.

## The negative check

With the kept PDB copied to `dist/nanoprobe-0.1.0-agent.pdb`, the archive test printed

```text
FAIL  a PDB is in the release: <checkout>/dist/nanoprobe-0.1.0-agent.pdb
```

and exited 1. That was its only `FAIL` line.

## Limits

- A PDB inside an archive is checked by the same test, and was not planted.
- `tools/pdbsig.sh` reads the CodeView record as `crates/xtask/src/symbols.rs` does; it
  was compared with xtask's signature on one build of the DLL.

## The check

One full `mise run check` on this change failed in `tools/luatest.sh`, at
`lua/tests/faults.lua:251`: "the sidecar attached again 2301 ms after resuming", against
the test's limit of 2,000 ms, with "a poll came 2.011373417s late" in its log. The load
average was 34.8 at the time, from another project's test program on the same machine.
The change touches no Lua, agent or sidecar code. The steps before `tools/luatest.sh`
had passed. The check was then completed step by step, under the check lock, with the
load average falling from 18.6 to 2.1: `lua/tests/faults.lua` alone passed on its first
run (attached again 895 ms after resuming), then `tools/luatest.sh` whole, `buf lint`,
`tools/protobreak.sh`, `tools/protobreak-test.sh`, `cargo xtask proto --check`,
`tools/stage-schema-test.sh` and `tools/secretscan-test.sh` each passed. The 2,000 ms
limit is recorded as an open timing limit.
