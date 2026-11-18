# Release notes

## Draft: 0.1.0, the first release

The first release of nanoprobe, 0.1: an agent inside DCS World, a sidecar that owns a
persistent campaign world, and a gRPC API for a controller in any language. The
[README](../README.md) says what the platform tracks and how fresh it is.

### Files

| File | Holds |
|---|---|
| `nanoprobe-<version>-windows-x86_64.zip` | `nanoprobe.exe`, `nanoprobe-sidecar.exe`, `install.cmd`, `uninstall.cmd`, `README.txt`, an example `sidecar.toml`, and the agent |
| `nanoprobe-<version>-linux-x86_64.tar.gz` | static `nanoprobe` and `nanoprobe-sidecar`, `install.sh`, `uninstall.sh`, `README.txt`, `deploy/` for the sidecar's container, and the agent |
| `nanoprobe-<version>-schema.tar.gz`, `nanoprobe-<version>-schema.binpb` | the API schema `nanoprobe.v1alpha1` and its descriptor set |
| `SHA256SUMS` | checksums of the four files |

No client package and no container image is published. Clients generate code from the
schema ([api-clients.md](api-clients.md)); the sidecar's image is built from the Linux
archive ([deploy/README.md](../deploy/README.md)). The files' names carry the workspace
version, `0.1.0`: `nanoprobe-0.1.0-windows-x86_64.zip`, for example.

### How the files were built

The files are built on one Linux runner of GitHub Actions with `mise run package`: the
Windows programs and the agent DLL cross-built with `cargo-xwin`, the Linux programs as
static musl binaries. The acceptance run tested files the same workflow built without
releasing them, from a commit that differs from the tagged one only in files the build
does not read. The published files are that build repeated for the tag, so their bytes,
and the checksums in `SHA256SUMS`, differ from the tested ones; the MSVC CRT and
Windows SDK that `cargo-xwin` downloads for each build may differ too.

The agent's PDB is not published. The project keeps it to turn the agent's crash
records into function names.

### Where it is verified

To be completed from the acceptance run ([acceptance.md](acceptance.md)):

| Platform | Result |
|---|---|
| DCS dedicated server under Wine on Linux | not yet run |
| DCS on Windows, hosting a session from the client | not yet run |
| DCS dedicated server on Windows | unverified: no test has run on one |

The soak runs on the Wine server only.

### Know before you install

- `nanoprobe uninstall` removes the whole `nanoprobe` folder in the DCS profile,
  `nanoprobe/missions/` included: a mission folder kept there, with the templates you
  saved, goes with it. To keep the per-map templates, make the mission folder a folder
  in the profile outside `nanoprobe/`, or copy them out first; keep other template
  files in a folder listed in the sidecar's `[missions] template_dirs`
  ([install.md](install.md), "Uninstall").
- The API is `v1alpha1` and can change in a way that breaks clients.
- The API has no TLS. A listener off loopback needs a tokens file and a trusted network.
- The project ships no mission. Save a template for each map in the Mission Editor
  ([mission-templates.md](mission-templates.md)).
- The platform does not start or restart DCS. It reports DCS as healthy, hung or gone.
- The sidecar exits when DCS restarts. Run it under something that starts it again: a
  service manager, or a Windows service or scheduled task. The compose overlay has a
  restart policy meant for that; that it brings the sidecar back after the DCS container
  restarts is not yet verified.
- The sidecar writes its log to standard output and standard error, not to a file.
- Fuel is not tracked, and ammunition is known only for units players occupy.

### After a DCS update

Run `nanoprobe doctor`. It passes, or it names each DCS function the platform calls and
no longer finds. The conformance suite, which checks the agent's behavior against the
specification on a running DCS, is not in the release: it runs from a source checkout
with the development tooling ([acceptance.md](acceptance.md), criterion 6). The
project's recorded run passed 9 of 9 on 2026-11-15, on DCS 2.9.31.29434 under Wine, with
an earlier agent build than this release's (LIVE,
[2026-11-15-s17-api-on-real-dcs.md](results/2026-11-15-s17-api-on-real-dcs.md)). The
suite must pass in full when `doctor` reports every name present (Open items says what
happens when one is absent).

### Open items

To be completed before the tag is pushed (docs/releasing.md, step 4).

- The conformance suite has no table from each check to the DCS names it needs. When
  `doctor` reports a name absent, a person reads the suite's result.
