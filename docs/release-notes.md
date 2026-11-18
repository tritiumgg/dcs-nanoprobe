# Release notes

## Draft: the first release (version not set)

The first release of nanoprobe: an agent inside DCS World, a sidecar that owns a
persistent campaign world, and a gRPC API for a controller in any language. The
[README](../README.md) says what the platform tracks and how fresh it is.

### Files

| File | Holds |
|---|---|
| `nanoprobe-<version>-windows-x86_64.zip` | `nanoprobe.exe`, `nanoprobe-sidecar.exe`, `install.cmd`, `uninstall.cmd`, `README.txt`, an example `sidecar.toml`, and the agent |
| `nanoprobe-<version>-linux-x86_64.tar.gz` | static `nanoprobe` and `nanoprobe-sidecar`, `install.sh`, `uninstall.sh`, `README.txt`, `deploy/` for the sidecar's container, and the agent |
| `nanoprobe-<version>-schema.tar.gz`, `nanoprobe-<version>-schema.binpb` | the API schema `nanoprobe.v1alpha1` and its descriptor set |
| `nanoprobe-<version>-agent.pdb` | symbols for the agent's crash records |
| `SHA256SUMS` | checksums of the five files |

No client package and no container image is published. Clients generate code from the
schema ([api-clients.md](api-clients.md)); the sidecar's image is built from the Linux
archive ([deploy/README.md](../deploy/README.md)).

### How the files were built

The files are cross-built on a development machine with `mise run package`: the Windows
programs and the agent DLL with `cargo-xwin`, the Linux programs as static musl
binaries. They are not built on a Windows runner. `SHA256SUMS` identifies the exact
files the acceptance run tested.

### Where it is verified

To be completed from the acceptance run ([acceptance.md](acceptance.md)):

| Platform | Result |
|---|---|
| DCS dedicated server under Wine on Linux | not yet run |
| DCS on Windows, hosting a session from the client | not yet run |
| DCS dedicated server on Windows | unverified: no test has run on one |

The soak runs on the Wine server only.

### Know before you install

- `nanoprobe uninstall` removes the whole `nanoprobe` folder in the DCS profile. A
  mission folder kept there, with the templates you saved, goes with it. Copy your
  templates out first.
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
with the development tooling ([acceptance.md](acceptance.md), criterion 6). It must pass
in full when `doctor` reports every name present. When `doctor` reports a name absent,
the suite has no list of which checks need that name, and a person reads its result.

### Open items

To be completed when the release is cut.

- The agent's PDB names the folders of the machine that built it: the linker writes the
  path of each object file and library into it. Publish a PDB only from a build whose
  paths may be public.
