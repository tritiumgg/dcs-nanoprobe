# Install

nanoprobe has two parts on a DCS server. The agent runs inside DCS and is installed into
the server's Saved Games profile. The sidecar is a separate program that connects to the
agent and serves the API. Nothing is written into the DCS install.

A release is on the project's GitHub releases page:

| File | For |
|---|---|
| `nanoprobe-<version>-windows-x86_64.zip` | DCS on Windows |
| `nanoprobe-<version>-linux-x86_64.tar.gz` | DCS under Wine on Linux |
| `nanoprobe-<version>-macos-universal.tar.gz` | the fake sidecar on a Mac, to write a controller without DCS |
| `nanoprobe-<version>-schema.tar.gz`, `-schema.binpb` | API clients ([api-clients.md](api-clients.md)) |
| `SHA256SUMS` | checking the files above |

Check the files you downloaded with `sha256sum -c --ignore-missing SHA256SUMS` (Linux)
or `shasum -a 256 -c --ignore-missing SHA256SUMS` (macOS). On Windows, compare the
output of `Get-FileHash` in PowerShell with the file's line in `SHA256SUMS`.

Each archive also holds this guide and the project's other documents, at the paths they
have in the repository, and the skill for AI coding tools in `skills/nanoprobe/`. Their
links to files the archive does not hold open the release's tag on GitHub.

## macOS

No DCS server runs on macOS. The macOS archive holds only `nanoprobe-fake-sidecar`
([controller-guide.md](controller-guide.md), "Test without DCS"), one program for Apple
silicon and Intel Macs, macOS 11 or later. It is not notarized by Apple, so macOS
refuses to open it after a download by a web browser. Download it with the GitHub CLI or
`curl`, which macOS does not mark, check it, and unpack it:

```sh
gh release download v<version> --repo tritiumgg/dcs-nanoprobe -p 'nanoprobe-*-macos-universal.tar.gz' -p SHA256SUMS
shasum -a 256 -c --ignore-missing SHA256SUMS
tar -xzf nanoprobe-<version>-macos-universal.tar.gz
```

With `curl` in place of the GitHub CLI, the first line is two:

```sh
curl -LO https://github.com/tritiumgg/dcs-nanoprobe/releases/download/v<version>/nanoprobe-<version>-macos-universal.tar.gz
curl -LO https://github.com/tritiumgg/dcs-nanoprobe/releases/download/v<version>/SHA256SUMS
```

After a browser download, unpack the archive and clear the mark in the unpacked folder:

```sh
xattr -dr com.apple.quarantine nanoprobe-<version>-macos-universal
```

Or run the program once, let macOS refuse it, and choose "Open Anyway" in System
Settings, Privacy & Security.

## 1. Install the agent

Quit DCS first. Unpack the archive anywhere outside the DCS install and the Saved Games
folder.

**Windows.** Double-click `install.cmd`, or run:

```
nanoprobe.exe install
```

**Linux.** Run as the user who owns the DCS profile:

```sh
./install.sh --wine-prefix <the Wine prefix DCS runs in>
```

`install.sh` runs `./nanoprobe install` with the same arguments.

Without `--wine-prefix` the program uses `WINEPREFIX`, then `~/.wine`. When DCS runs in a
container, run the program on the host against the prefix folder the container mounts.
The program refuses to write into a profile another user owns, and prints the `sudo -u`
command to use.

The program installs into the one DCS profile it finds: a folder named `DCS` or
`DCS.<branch>` in `Saved Games`. When there are several, name one with `--profile DIR`.

Install writes these, all inside the profile:

| Path | What it is |
|---|---|
| `Scripts/Hooks/nanoprobe-hook.lua` | the loader DCS runs at start |
| `nanoprobe/versions/<build>/` | the agent DLL and its Lua, with checksums |
| `nanoprobe/manifest.txt` | which version is active |
| `nanoprobe/install.txt` | the folders install created, which uninstall removes when they are empty |
| `nanoprobe/token` | the token the sidecar presents to the agent |
| `Config/autoexec.cfg` | one marked block, appended |

The block in `autoexec.cfg` adds `userhooks` to `net.allow_unsafe_api` and `server` to
`net.allow_dostring_in`, and keeps every name already there. The rest of the file stays
byte for byte as it was. Running install again with the same release changes nothing.
Running it from a newer release switches to that version and keeps the one before it.

## 2. Start DCS and check

Start DCS, then run:

```
nanoprobe doctor
```

On Linux add `--wine-prefix` as for install. Doctor prints one line per check, marked
`ok`, `fail` or `note`, and writes nothing. It exits 0 when no check failed, 1 when one
failed and 2 when it finds no installed profile.

Doctor reads the files, the token and settings, the platform's lines in `Logs/dcs.log`,
the `net.*` keys in `autoexec.cfg`, and the list of DCS functions the agent found. After
a DCS update, a line such as `fail the server state lacks <names>: DCS changed them`
names each DCS function the platform calls and no longer finds.

The server-state check needs a mission to have started once since DCS started.

## 3. Settings for the agent

The agent runs with defaults and needs no settings file on Windows when the sidecar runs
as the same account as DCS. To change a setting, create `nanoprobe/agent.toml` in the
profile; [spec/config.md](../spec/config.md) lists every key. DCS reads it at start.

Two cases need `map_path`, the file the agent and the sidecar share:

- **Wine.** Set it to a path on a memory-backed volume that the sidecar can also open,
  as DCS names it.
- **Windows, sidecar under another account.** Set it to a path both accounts can open.
  The default is in the temp folder of the account that runs DCS.

The key is in the `[transport]` table. The agent refuses a file with a key it does not
know, and then runs without its link:

```toml
[transport]
map_path = 'Z:\dev\shm\nanoprobe.map'
```

Extensions and raw Lua are off unless `agent.toml` has `[lua] raw = true`.

## 4. Run the sidecar

The sidecar is the program `nanoprobe-sidecar` in the archive, beside `nanoprobe`. It is
not installed: run it from the unpacked folder, or on Linux from a container image built
from the archive.

It reads one settings file, `sidecar.toml`. The Windows archive holds an example to copy
and fill in; the Linux archive holds one for a container in `deploy/`. Relative paths in
the file are relative to the folder that holds it. The smallest file names the two files
the sidecar shares with the agent, as the sidecar opens them:

```toml
[agent]
map_path = "<the agent's mapped file>"
token_path = "<profile>/nanoprobe/token"

[missions]
dir = "<the mission folder, as the sidecar opens it>"
dcs_dir = "<the same folder, as DCS opens it>"
```

On Windows the agent's mapped file is `nanoprobe-47310.map` in the temp folder (`%TEMP%`)
of the account that runs DCS, unless `agent.toml` sets `map_path`. Under Wine it is the
Linux path of the file `agent.toml` names.

Start it, and ask a running sidecar for its verdict:

```
nanoprobe-sidecar run --config sidecar.toml
nanoprobe-sidecar health --config sidecar.toml
```

The sidecar keeps its campaign stores under `campaigns/` beside `sidecar.toml`. Keep
that folder outside the DCS profile's `nanoprobe` folder, which uninstall removes. The
sidecar writes its log to standard output and standard error, not to a file: a service
or a scheduled task that runs it should send that output to a file.

The sidecar connects to the agent on loopback, so it runs on the same machine as DCS,
or in a container that shares the DCS container's network. For DCS in a Docker
container, `deploy/` in the Linux archive has the image's `Dockerfile`, a compose
overlay and a `sidecar.toml` to start from; [deploy/README.md](../deploy/README.md) has
the steps. No image is published: build it from the archive.

The sidecar does not start, stop or restart DCS. It reports DCS as healthy, hung or
gone; restarting DCS is the host's job.

When DCS restarts, the sidecar exits with an error once the new DCS does not accept its
connection for `handshake_timeout_s`, so that whatever runs the sidecar starts it again.
Run it under something that does: a service manager on Linux, a service or scheduled
task on Windows, or the compose overlay's restart policy. That the overlay's policy
brings the sidecar back after the DCS container restarts is not yet verified
([acceptance.md](acceptance.md), criterion 3). Started by hand, the sidecar has to be
started again by hand after each DCS restart.

### API listener and tokens

The API listens on `127.0.0.1:47311`. Without a tokens file every caller on loopback
has the `admin` role, so any program on the machine can control the server. For a
controller on
another machine, set `[api] listen` to an address it can reach and `tokens_path` to a
tokens file; the sidecar refuses a listener that is not loopback without one. The file
holds the SHA-256 of each token and a role (`observer`, `controller` or `admin`), never
the token itself. [spec/config.md](../spec/config.md), "Tokens file", has the layout.

The API is plain gRPC without TLS: the sidecar refuses to start with `tls_cert` or
`tls_key` set. A token sent to a listener on another address crosses the network in the
clear, so reach it only over a network or a tunnel you trust.

### Mission templates

A campaign's mission is written from a template you save in the Mission Editor, one per
map, in the mission folder. The project ships none.
[mission-templates.md](mission-templates.md) explains how to save them and how a
campaign uses its own. A controller may name a file by path, a campaign's own mission
template, a group template file, a static template file or a payload preset file, only
inside a folder listed in `[missions] template_dirs`, which is empty by default.

## 5. Check the whole setup

Give doctor the sidecar's settings to check both sides together:

```
nanoprobe doctor --sidecar-config sidecar.toml --theater Caucasus
```

Doctor then checks that the sidecar accepts the file, that the token it names is the
profile's, that the mission folder holds a template for each `--theater`, and that each
folder in `template_dirs` is a folder. On Linux and macOS it also fails a template
folder that every user can write in; it does not read Windows access lists.

A sidecar serves one DCS. With more than one installed profile, name the sidecar's with
`--profile`; otherwise doctor checks nothing and exits 2.

When the sidecar runs in a container, the paths in `sidecar.toml` are the container's.
Name the folders as the host sees them instead:

```sh
./nanoprobe doctor --wine-prefix <prefix> --missions <mission folder> \
    --template-dir <templates folder> --theater Caucasus
```

## The kill switch

Create an empty file named `disable` in the profile's `nanoprobe` folder. A running
agent stops all work within 2 seconds of the next frame, and DCS runs on without it. With
the file in place at start, the loader runs nothing. Delete the file and restart DCS to
turn the agent back on.

## Uninstall

> **Uninstall deletes the profile's `nanoprobe` folder and everything in it,
> `nanoprobe/missions/` included.** If your mission folder is there, as the examples
> suggest, the templates you saved in the Mission Editor and the campaigns' missions are
> deleted with it. The per-map templates (`<theater>.miz`) must be in the mission
> folder: to keep them through an uninstall, make the mission folder (`[missions] dir`
> and `dcs_dir`) a folder in the profile outside `nanoprobe/`, or copy them out first.
> Keep a campaign's own templates, group and static template files and payload preset
> files in a folder outside the profile, listed in the sidecar's
> `[missions] template_dirs`. Payload preset files may also stay in the profile's
> folder where the Mission Editor saves them, `MissionEditor/UnitPayloads`
> (UNVERIFIED), which uninstall does not touch
> ([mission-templates.md](mission-templates.md), "Payload presets").

Quit DCS, then double-click `uninstall.cmd` on Windows, or on Linux run:

```sh
./uninstall.sh --wine-prefix <prefix>
```

Uninstall removes the loader, the block in `autoexec.cfg`, and the profile's `nanoprobe`
folder: versions, token, settings, logs, and a mission folder kept there. A folder that
install created is removed if it is empty. The profile is then as it was before the
first install. The sidecar's own folder is not touched.

## Other commands

| Command | What it does |
|---|---|
| `nanoprobe inspect <map-file>` | prints the agent's status block, metrics and unread records |
| `nanoprobe top <map-file>` | shows the live state and rates of the link |
| `nanoprobe watch world\|events` | prints a sidecar's world changes or events as they arrive |
| `nanoprobe restore` | restores a campaign's backup, with the sidecar stopped |

`nanoprobe help` prints every option.
