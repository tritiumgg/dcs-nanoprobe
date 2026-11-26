# Operator guide

This guide is for whoever installs and runs nanoprobe on a DCS server, person or AI
agent. [install.md](install.md) has the install steps in full; this guide adds health,
logs, failures, upgrades and the platform differences.
[configuration.md](configuration.md) lists every setting.

## What runs where

| Part | Where | Started by |
|---|---|---|
| The agent (a DLL and its Lua hook) | inside DCS, from the Saved Games profile | DCS, at start |
| The sidecar, `nanoprobe-sidecar` | beside DCS, on the same machine or in a container on the DCS container's network | you: a service, a scheduled task, or a container restart policy |
| The command-line tool, `nanoprobe` | anywhere that sees the profile | you |

Nothing is written into the DCS install. The platform never starts, stops or restarts
DCS. It reports DCS's health; restarting DCS is the host's job.

## Install

1. Download the archive for your platform and `SHA256SUMS` from the GitHub release, and
   check them.
2. Quit DCS. Unpack the archive outside the DCS install and the Saved Games folder.
3. Install the agent. Windows: `install.cmd`, or `nanoprobe.exe install`. Linux, as the
   profile's owner: `./install.sh --wine-prefix <prefix>`. With several profiles, name
   one with `--profile DIR`.
4. Write `sidecar.toml` from the example in the archive. On Wine, also write
   `nanoprobe/agent.toml` in the profile with `[transport] map_path` (below).
5. Save a mission template for each map in the mission folder
   ([mission-templates.md](mission-templates.md)).
6. Start DCS. Start the sidecar: `nanoprobe-sidecar run --config sidecar.toml`.
7. Check: `nanoprobe doctor --sidecar-config sidecar.toml --theater <map>`.

Install writes only into the profile: the loader `Scripts/Hooks/nanoprobe-hook.lua`, the
`nanoprobe/` folder, and one marked block in `Config/autoexec.cfg` that adds
`userhooks` to `net.allow_unsafe_api` and `server` to `net.allow_dostring_in`. Running
it again with the same release changes nothing.

The API listens on `127.0.0.1:47311` by default (`[api] listen`). Without a tokens file
every caller on loopback is `admin`. A listener on any other address needs `[api]
tokens_path`, and the sidecar refuses to start without it. The API has no TLS: reach a
non-loopback listener only over a network or tunnel you trust.

Extensions and raw Lua stay off unless `agent.toml` sets `[lua] raw = true`. No API
call can turn them on.

## Doctor

`nanoprobe doctor` checks an install from outside DCS and writes nothing. It prints one
line per check, `ok`, `fail` or `note`.

| Exit code | Meaning |
|---|---|
| 0 | No check failed |
| 1 | A check failed |
| 2 | No installed profile found; with `--sidecar-config`, more than one; or a usage error |

It reads the files, the token, the settings, the platform's lines in `Logs/dcs.log`,
the `net.*` keys in `autoexec.cfg`, and the DCS functions the agent found. After a DCS
update, a line such as `fail the server state lacks <names>` names each DCS function
the platform calls and no longer finds. The server-state check needs one mission to
have started since DCS started. Run doctor after every install, upgrade and DCS update.

## Health

The sidecar publishes one verdict about DCS. Read it with
`nanoprobe-sidecar health --config sidecar.toml`, which prints it with the seconds
since the last heartbeat, or with `GetServerStatus` in the API.

| Verdict | Health exit code | Means |
|---|---|---|
| `healthy` | 0 | The heartbeat advances, or DCS is loading within `loading_limit_s` |
| `hung` | 1 | DCS holds the handshake port, but its heartbeat stands still past the limit |
| `gone` | 2 | Nothing listens: no DCS process holds a running agent |
| `disabled` | 3 | The agent stopped itself, and its DCS process is still up |
| no answer | 4 | The sidecar did not answer |

The limits are `heartbeat_stale_s` (10 s) outside a load and `loading_limit_s` (900 s)
during a load. A mission load has no fixed duration; `loading_limit_s` only says when
to call a silent DCS hung. Act on the verdict. The heartbeat's age can pass a limit
while the verdict still reads `healthy`, until the sidecar's next connect finishes.
[spec/errors.md](../spec/errors.md), "Health", has the exact rules.

When `[api] tokens_path` is set, the health command needs a token of the `observer`
role or higher in the `NANOPROBE_TOKEN` environment variable.

`GetServerStatus` also reports whether the sidecar is attached and why the last attach
failed. A DCS whose heartbeat advances is `healthy` even when the sidecar cannot
attach. Low disk space (`store_space_low`), failed backups (`backup_error`) and few DCS
mark IDs left (`dcs_ids_low`) are reported apart and never change the verdict or the
exit code. Do not restart DCS for them.

A watchdog may restart DCS on `hung` or `gone`. That policy is yours.

## When DCS is hung

`hung` means a connect to the agent's handshake port is not refused, and the heartbeat
stands still past the limit. The causes are a stall, a load past `loading_limit_s`, or
possibly a crash. A DCS that crashed stays alive in its crash-report window and runs no
frames (LIVE, [results](results/2026-11-11-first-dll-and-spikes.md)). That its port
still accepts a connect, which makes it `hung`, is UNVERIFIED. A crash may read `gone`.

1. Read the end of DCS's own `Logs/dcs.log` in the profile.
2. Read `nanoprobe/logs/crash.log` in the profile. Each line is one native exception,
   with the exception code, the stack as `module+offset`, the breadcrumb, and the PDB
   that symbolizes the build.
3. Run `nanoprobe inspect <map-file>` to read the agent's status block, which keeps the
   breadcrumb after DCS dies.
4. Decide whether to wait or restart DCS. The platform does not decide for you.

## When DCS is gone

`gone` means no DCS process holds a running agent. DCS is not running, it crashed, or
it started with the kill switch file in place.

1. Check whether DCS is running. If not, start it.
2. If it is, look for `nanoprobe/disable` in the profile. Delete it and restart DCS to
   turn the agent on.
3. Run `nanoprobe doctor`.

After a DCS restart, the sidecar waits `handshake_timeout_s` (15 s) for the new DCS
to accept its connection. Then it exits with an error, so that whatever runs it starts
it again. Run the sidecar under something that restarts it. Started by hand, it must be
started again by hand after each DCS restart. That the compose overlay's restart
policy brings it back after the DCS container restarts is UNVERIFIED
([acceptance.md](acceptance.md), criterion 3).

Once the sidecar is back, it restores the active campaign. Each group on the loaded map
is spawned from its last stored state. DCS runs the mission its own settings name. The
campaign's next `Reload` loads the campaign's mission.

## When the agent is disabled

`disabled` means the agent stopped itself and DCS runs on without it. Two things do
that: the kill switch file `nanoprobe/disable`, or a panic the agent caught. Read
`nanoprobe/logs/agent.log` for the cause. The agent does no more work in that DCS
process. Remove the cause, then restart DCS.

## The kill switch

Create an empty file named `disable` in the profile's `nanoprobe` folder. A running
agent stops all work within 2 s of the next frame, and DCS runs on. With the file there
at start, the loader runs nothing. No frame runs while a mission loads, so a file
created during a load takes effect after it (LIVE,
[results](results/2026-11-11-server-gate.md)). Delete the file and restart DCS to turn
the agent back on.

## Logs

| Writer | Where |
|---|---|
| The hook, the shim and the agent | `<profile>/nanoprobe/logs/hook.log`, `shim.log`, `agent.log` |
| Native exceptions | `<profile>/nanoprobe/logs/crash.log` |
| The sidecar | its standard output and standard error |
| DCS | `<profile>/Logs/dcs.log` |

Every platform log is JSON lines, one record per line, with `wall`, `level`, `target`
and `msg` ([spec/log.md](../spec/log.md)). Merge files by `wall`. Each file rolls to
`<name>.1` at 4 MiB. The sidecar writes no file of its own: have its service or
scheduled task send its output to a file. Once the link is open, the agent, hook and
shim also send their records to the sidecar.

Two read-only tools read the mapped file from outside DCS.
`nanoprobe inspect <map-file>` prints the status block, the metrics and the unread
records. `nanoprobe top <map-file>` shows the live state and its rates.
`nanoprobe watch world` and `nanoprobe watch events` print a sidecar's world changes
and events. They read `NANOPROBE_ADDR` and `NANOPROBE_TOKEN`.

## Diagnostics bundles

A release has no bundle command. To report a problem, collect by hand:

- the output of `nanoprobe doctor` and `nanoprobe inspect <map-file>`;
- `<profile>/nanoprobe/logs/`;
- the sidecar's log;
- the relevant part of `<profile>/Logs/dcs.log`.

Leave out secrets. Never copy `serverSettings.lua`, `network.vault`, the token files,
the tokens file, `autoexec.cfg` beyond the two `net.*` keys, or container environment
variables. The repository's development tooling (`cargo xtask bundle`) builds bundles
from an allow-list with redaction; it is built from source and is not in a release.

## Campaign stores and backups

The sidecar keeps each campaign's store under `[store] dir`, by default `campaigns/`
beside `sidecar.toml`. Keep
that folder outside the profile's `nanoprobe` folder, which uninstall deletes.

The sidecar takes a backup every `backup_interval_h` while a campaign is active, one on
`BackupCampaign`, and one before a store migration. A backup is refused with
`SPACE_LOW` when free space is short. To restore one, stop the sidecar and run:

```sh
nanoprobe restore --store <store dir> --campaign <id> --backup <n>
```

It keeps the store it replaces as `restore-<time>/store.sqlite` in the campaign's
folder, suspends the campaign unless the backup is finished, and keeps entity IDs and
revisions above both copies'. The event history is not rolled back.
`ListCampaignBackups` lists the backups ([spec/persistence.md](../spec/persistence.md),
"Backups" and "Restore a backup").

## DCS updates

A DCS update needs no reinstall. Install writes only into the Saved Games profile, and a
DCS update does not touch the profile. After the update, start DCS, let one mission
start, and run `nanoprobe doctor`. A line such as `fail the server state lacks <names>`
names each DCS function the platform calls that the new DCS no longer has. A missing
function is reported and does not stop the agent. No DCS update has happened during
testing. This follows from the design; no test has seen it yet
([acceptance.md](acceptance.md), "After a DCS update").

## Upgrades

1. Back up each campaign: `BackupCampaign`, or copy the `[store] dir` folder with the
   sidecar stopped.
2. Quit DCS and stop the sidecar.
3. Run `install` from the new release. It makes the new version active and keeps the
   one before it as `previous` in `nanoprobe/manifest.txt`.
4. Replace the sidecar program with the new release's.
5. Start DCS, start the sidecar, and run `nanoprobe doctor`.

A sidecar opens a store of an older schema by taking a `pre_migration` backup, then
migrating it in one transaction. A failed migration rolls back, keeps the backup, and
does not open the campaign. There is no downgrade: an older sidecar refuses a newer
store. To go back, run `install` from the older release, then restore the
`pre_migration` backup with `nanoprobe restore`. That loses every change made since the
migration.

**0.2.0.** The store is schema 2. A 0.2.0 sidecar migrates a 0.1.0 store on its first
open; a 0.1.0 sidecar refuses the migrated file. Back up before you upgrade if you may
go back. [release-notes.md](release-notes.md) lists every change.

## Uninstall

Uninstall deletes the profile's `nanoprobe` folder and everything in it, a mission
folder kept there included. Move per-map templates to a mission folder outside
`nanoprobe/`, and other template files to a folder `[missions] template_dirs` lists,
before you uninstall ([install.md](install.md), "Uninstall").

Quit DCS, then run `uninstall.cmd` on Windows or `./uninstall.sh --wine-prefix
<prefix>` on Linux. It removes the loader, the `autoexec.cfg` block and the `nanoprobe`
folder, and folders install created if they are empty. The sidecar's folder is not
touched.

## Windows and Wine

| | Windows | Linux, DCS under Wine |
|---|---|---|
| Archive | `nanoprobe-<version>-windows-x86_64.zip` | `nanoprobe-<version>-linux-x86_64.tar.gz` |
| Install | `install.cmd` or `nanoprobe.exe install` | `./install.sh --wine-prefix <prefix>`, as the profile's owner. For DCS in a container, run it on the host against the prefix folder the container mounts. |
| Mapped file | Default `nanoprobe-47310.map` in the `%TEMP%` of the account that runs DCS. Set `map_path` when the sidecar runs as another account. | Set `[transport] map_path` in `agent.toml` to a path on a memory-backed volume, as DCS names it (such as `Z:\dev\shm\nanoprobe.map`). The sidecar opens the same file by its Linux path. |
| Sidecar | `nanoprobe-sidecar.exe`, as a service or scheduled task | a native Linux program, or a container built from `deploy/` in the archive ([deploy/README.md](../deploy/README.md)); no image is published |
| Health probe | waits up to 5 s for a refused connect; Windows reports a refusal only after a delay (PRIOR) | waits 100 ms |
| Doctor | does not read Windows access lists | fails a template folder that every user can write in |

The agent listens on `127.0.0.1` only, so the sidecar must share DCS's network
namespace: the same machine, or a container on the DCS container's network.

What has been tested:

| Platform | Status |
|---|---|
| DCS dedicated server under Wine on Linux, in a container | The development target. DCS 2.9.31.29434, `wine-10.0`. |
| DCS on Windows, hosting from the client | The agent's overhead, mission reloads and the kill switch. |
| DCS dedicated server on Windows | UNVERIFIED. No test has run on one. |
