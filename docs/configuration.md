# Configuration

nanoprobe reads four things an administrator can set:

| What | Read by | Where | When it is read |
|---|---|---|---|
| `agent.toml` | the agent, inside DCS | `<profile>/nanoprobe/agent.toml` | once, when DCS starts |
| `sidecar.toml` | the sidecar | anywhere; the sidecar's `--config` argument names it | once, when the sidecar starts |
| The tokens file | the sidecar | the path `[api] tokens_path` names | when the sidecar starts |
| The kill switch, `disable` | the agent and its loader | `<profile>/nanoprobe/disable` | while DCS runs |

`<profile>` is the DCS profile folder in Saved Games, such as `Saved Games/DCS` or
`Saved Games/DCS.<branch>`. Every key has a default, except `[agent] map_path` and
`token_path` in `sidecar.toml`, which are required. A key a program does not know is
an error. [spec/config.md](../spec/config.md) has the exact rules.

Most servers set only the keys marked **set this**. Change any other key only for the
reason its section gives.

## `agent.toml`

`agent.toml` sets up the agent inside DCS: the mapped file it shares with the sidecar,
its time budget, how often it reads each value, and whether Lua from the API may run.
It lives at `<profile>/nanoprobe/agent.toml`. Install does not create it; create it when
you need a setting. On Windows, with the sidecar under the same account as DCS, most
servers need none.

The agent runs with every default when the file is missing. A file that does not
parse, or that has an unknown key, stops the agent's link: DCS runs on, and nanoprobe
does not attach. DCS reads the file once at start, so a change needs a DCS restart.

### Overview

| Key | Default | In short |
|---|---|---|
| [`[transport] map_path`](#transport-map_path) | temp folder | The mapped file the agent and the sidecar share. **Set this** under Wine. |
| [`[transport] port`](#transport-port) | `47310` | The agent's handshake port |
| [`[transport] out_ring_kib`, `in_ring_kib`](#transport-out_ring_kib-and-in_ring_kib) | `4096`, `1024` | Sizes of the two rings in the mapped file |
| [`[transport] event_queue_kib`, `control_queue`](#transport-event_queue_kib-and-control_queue) | `1024`, `1024` | The agent's queues for events and control records |
| [`[budget] overhead_share`](#budget-overhead_share) | `0.003` | The agent's share of real time |
| [`[budget] commanded_ms`](#budget-commanded_ms) | `2.0` | Time per frame for commands |
| [`[sampling]` periods](#sampling-periods) | `1`, `5`, `60`, `60` s | How often each value is read |
| [`[lua] raw`](#lua-raw) | `false` | Allow Lua from the API |

### Settings

#### `[transport] map_path`

**Does:** names the mapped file, the file the agent and the sidecar share to pass
records both ways. The path is the one DCS opens.

**Default:** `nanoprobe-<port>.map` in the temp folder of the account that runs DCS.

**Change it when** DCS runs under Wine: set it to a path on a memory-backed volume the
sidecar can also open, such as `Z:\dev\shm\nanoprobe.map`. Also change it on Windows
when the sidecar runs as another account than DCS, because each account has its own temp
folder: use a path both can open. Set `[agent] map_path` in `sidecar.toml` to the same
file.

#### `[transport] port`

**Does:** sets the loopback TCP port where the agent waits for the sidecar's handshake.
The agent listens on `127.0.0.1` only, so the sidecar must share DCS's network.

**Default:** `47310`. The default mapped file name contains it.

**Change it when** another program uses the port, or two DCS servers run on one machine.
Set `[agent] handshake` in `sidecar.toml` to match.

#### `[transport] out_ring_kib` and `in_ring_kib`

**Does:** sets the sizes, in KiB, of the two rings in the mapped file. The out ring
carries records from the agent to the sidecar. The in ring carries commands to the
agent. Each is a power of two from 256 to 65536.

**Defaults:** `4096` and `1024`.

**Change them when** `nanoprobe inspect` shows a ring often full. The sidecar refuses to
build a command larger than an eighth of the in ring, so a large spawn may need a larger
`in_ring_kib`.

#### `[transport] event_queue_kib` and `control_queue`

**Does:** sizes the agent's own queues: events and logs in KiB, and control records in
records. They hold records while the out ring is full.

**Defaults:** `1024` and `1024`. Each is from 1 to 1,048,576.

**Change them when** gap events appear often under load. A gap can mean the agent's
event queue dropped events.

#### `[budget] overhead_share`

**Does:** sets the share of real time the agent may spend on the simulation thread for
its own reads. Each frame's slice is this share of the average frame time, and never
more than 250 µs.

**Default:** `0.003`, which is 0.3 %. Above 0, at most 1.

**Change it when** you accept more cost on the server for fresher values, or want even
less cost. The agent's fixed work per frame sets a floor under the share, and the floor
rises with frame rate (PRIOR). Below it the fixed work takes the whole
share and nothing is left for reads. The agent logs a warning when that happens.

#### `[budget] commanded_ms`

**Does:** sets the milliseconds per frame for running the controller's commands. A
command is never cut short, so one large spawn can run past it in its frame.

**Default:** `2.0`. Above 0, at most 1,000.

**Change it when** commands queue up and the server has frame time to spare, or when
command bursts cause stutter you want to spread out.

#### `[sampling]` periods

**Does:** sets the target period of each kind of read, in seconds:

| Key | Default | Reads |
|---|---|---|
| `position_s` | `1.0` | unit positions |
| `vitals_s` | `5.0` | life of units a subscription names, such as those players occupy; other units every 12 × this |
| `warehouse_s` | `60.0` | airbase owners and warehouse stock |
| `enumeration_s` | `60.0` | the full list of objects in DCS |

Each period is above 0.

**Change them when** a controller needs values fresher or can live with older ones. The
budget still holds: a shorter period the budget cannot meet is not met. A controller
can also ask for other periods with `SetSampleRates`.

#### `[lua] raw`

**Does:** allows the API's Lua calls: `EvalLua`, `RegisterLua`, `InvokeLua`, and Lua
sent by extensions.

**Default:** `false`. Only this file turns it on. No API call can.

**Change it when** you trust the controller with code that runs inside DCS. A Lua error
is caught, but Lua that never returns hangs the server.

## `sidecar.toml`

`sidecar.toml` sets up the sidecar: how it reaches the agent, where the API listens and
who may call it, where campaigns are stored and for how long, when DCS counts as hung,
and where mission templates are. It lives wherever you choose. Start the sidecar with
its path: `nanoprobe-sidecar run --config sidecar.toml`. The sidecar reads it once, at
start, so a change needs a sidecar restart.

The sidecar refuses to start when the file has an unknown key or a bad value. Relative
paths are relative to the folder that holds the file. Each release archive holds an
example to start from.

### Overview

| Key | Default | In short |
|---|---|---|
| [`[agent] map_path`, `token_path`](#agent-map_path-and-token_path) | required | The mapped file and the token, as the sidecar sees them. **Set this.** |
| [`[agent] handshake`](#agent-handshake) | `127.0.0.1:47310` | The agent's handshake address |
| [`[agent] poll_ms`](#agent-poll_ms) | `2` | How often the sidecar reads the mapped file |
| [`[agent] command_queue`](#agent-command_queue) | `4096` | No effect in this release |
| [`[api] listen`](#api-listen) | `127.0.0.1:47311` | The API's address |
| [`[api] tokens_path`](#api-tokens_path) | empty | The tokens file |
| [`[api] tls_cert`, `tls_key`](#api-tls_cert-and-tls_key) | empty | Must stay empty |
| [`[api] lease_ttl_s`](#api-lease_ttl_s) | `30` | How long a controller lease lasts |
| [`[api] stream_buffer`](#api-stream_buffer) | `1024` | How far a stream may fall behind |
| [`[api] result_wait_s`](#api-result_wait_s) | `10` | How long an effect waits for DCS |
| [`[store] dir`](#store-dir) | `campaigns` | Where campaigns are stored |
| [`[store] enabled`](#store-enabled) | `true` | Store campaigns on disk |
| [`[store] checkpoint_s`](#store-checkpoint_s) | `5` | How often positions and life are saved |
| [`[store] backup_interval_h`, `backup_keep`](#store-backup_interval_h-and-backup_keep) | `24`, `7` | Scheduled backups |
| [`[store]` history limits](#store-history-limits) | 90 days, 30 days, 1,024 MB | How much event and journal history is kept |
| [`[store] idempotency_keep_h`](#store-idempotency_keep_h) | `24` | How long a call's key is remembered |
| [`[store] world_changes_keep`](#store-world_changes_keep) | `100000` | How far back a watch can resume |
| [`[store] removed_keep_h`, `dead_keep_h`](#store-removed_keep_h-and-dead_keep_h) | `1`, `24` | When removed and dead groups leave the store |
| [`[health]` limits](#health-limits) | `10`, `900`, `15` s | When DCS counts as hung, and the handshake wait |
| [`[world] position_period_min_s`](#world-position_period_min_s) | `0.1` | The fastest position updates a controller gets |
| [`[world] materialize_per_frame`](#world-materialize_per_frame) | `1` | Spawns sent to DCS at a time |
| [`[world] on_lost`](#world-on_lost) | `"report"` | What to do with a group DCS lost |
| [`[world] destroy_retries`](#world-destroy_retries) | `3` | Destroy attempts before giving up |
| [`[log] dir`, `level`](#log-dir-and-level) | `logs`, `info` | No effect in this release |
| [`[missions] dir`, `dcs_dir`](#missions-dir-and-dcs_dir) | empty | The mission folder. **Set this.** |
| [`[missions] template_dirs`](#missions-template_dirs) | `[]` | Folders a controller may name files in |

### Settings

#### `[agent] map_path` and `token_path`

**Does:** names the mapped file and the agent's token file, as the sidecar opens them.
The token is `<profile>/nanoprobe/token`, which install creates.

**Default:** none. Both are required.

**Set them** always. Under Wine with a native sidecar, these are Linux paths to the
files `agent.toml` names with Windows paths. In a container, they are the container's
paths.

#### `[agent] handshake`

**Does:** names the address of the agent's handshake.

**Default:** `127.0.0.1:47310`.

**Change it when** you changed `[transport] port` in `agent.toml`.

#### `[agent] poll_ms`

**Does:** sets how often, in milliseconds, the sidecar reads the mapped file.

**Default:** `2`. From 1 to 5.

**Change it when** the sidecar's own CPU use matters more than latency (raise it), or
the reverse.

#### `[agent] command_queue`

Accepted, and has no effect in this release.

#### `[api] listen`

**Does:** sets the address and port of the gRPC API.

**Default:** `127.0.0.1:47311`: only programs on the same machine can connect.

**Change it when** the controller runs on another machine, or the sidecar runs in a
container whose port you publish. Any address other than loopback needs
`tokens_path`; the sidecar refuses to start without one. The API has no encryption, so
reach it only over a network or tunnel you trust.

#### `[api] tokens_path`

**Does:** names the tokens file, which gives each caller a role ("The tokens file",
below).

**Default:** empty. Every caller on loopback then has the `admin` role.

**Set it when** the API listens off loopback, or when you want to limit what programs
on the machine may do.

#### `[api] tls_cert` and `tls_key`

Leave them empty. TLS is not built, and the sidecar refuses to start with either set.

#### `[api] lease_ttl_s`

**Does:** sets how many seconds a controller lease lasts without a renewal. After that,
another controller may take the lease.

**Default:** `30`.

**Change it when** controllers fail over too slowly after a crash (lower it), or a slow
network makes a working controller lose its lease (raise it).

#### `[api] stream_buffer`

**Does:** sets how many messages a stream may fall behind before the sidecar ends it
with `CONSUMER_TOO_SLOW`.

**Default:** `1024`.

**Change it when** a slow consumer is ended often and has the memory to buffer more.

#### `[api] result_wait_s`

**Does:** sets how many seconds an effect or a query call waits for DCS's answer. After
that it returns the outcome `PENDING`, and the controller reads the result later with
`GetOutcome`.

**Default:** `10`.

**Change it when** controllers would rather wait longer, or get `PENDING` sooner.

#### `[store] dir`

**Does:** names the folder that holds one folder per campaign and the server's own
store.

**Default:** `campaigns`, beside `sidecar.toml`.

**Change it when** you want the stores on another disk. Keep it outside the profile's
`nanoprobe` folder, which uninstall deletes.

#### `[store] enabled`

**Does:** with `false`, keeps every store in memory. Nothing survives a sidecar restart.

**Default:** `true`.

**Change it** only for tests, or for a controller that saves state itself with
`Capture` and puts it back after a restart.

#### `[store] checkpoint_s`

**Does:** sets how many seconds pass between saves of positions and life. Warehouse
stock is saved as each change is read.

**Default:** `5`.

**Change it when** you accept losing more after a crash for less disk writing (raise
it), or the reverse. After a DCS crash, restored values are up to one checkpoint old.

#### `[store] backup_interval_h` and `backup_keep`

**Does:** sets the hours between scheduled backups of the active campaign, and how many
scheduled and manual backups to keep per campaign. Backups before a store migration are
kept apart.

**Defaults:** `24` and `7`.

**Change them when** you want more restore points, or less disk use. Each backup holds a
full copy of the store, journal included.

#### `[store]` history limits

**Does:** limits how much history each campaign keeps:

| Key | Default | Limits |
|---|---|---|
| `event_retention_days` | `90` | days of event history; 0 is no time limit |
| `events_max_mb` | `1024` | megabytes of event history; 0 is no limit |
| `journal_retention_days` | `30` | days of the store's journal of changes; 0 is no time limit |
| `journal_max_mb` | `1024` | megabytes of journal; 0 is no limit |
| `server_log_days` | `30` | days of events recorded while no campaign is active |

Whichever limit is reached first deletes the oldest rows.

**Change them when** disk use matters, or you want to keep a whole history: set both a
days key and its megabyte key to 0. At a high command rate, 30 days of journal can be
gigabytes in each backup.

#### `[store] idempotency_keep_h`

**Does:** sets how many hours the sidecar remembers a call's idempotency key. A repeat
within that time returns the first answer and does nothing again.

**Default:** `24`.

**Change it when** your controllers may resend calls after a longer outage.

#### `[store] world_changes_keep`

**Does:** sets how many world changes the store keeps for a watch to resume from. A
watch that starts further back fails with `REVISION_EXPIRED` and must list again.

**Default:** `100000`.

**Change it when** controllers reconnect after long gaps on a busy world.

#### `[store] removed_keep_h` and `dead_keep_h`

**Does:** `removed_keep_h` sets how many hours a removed entity's rows stay before they
may be purged. `dead_keep_h` sets how many hours a dead group stays in the world before
it expires and leaves the API. A dead static stays as a ruin until it is removed.

**Defaults:** `1` and `24`. `dead_keep_h = 0` never expires dead groups.

**Change them when** controllers need dead groups for longer, or the store grows with
dead and removed entities.

#### `[health]` limits

**Does:**

| Key | Default | Sets |
|---|---|---|
| `heartbeat_stale_s` | `10` | Seconds without a heartbeat, outside a mission load, before DCS can be judged `hung` or `gone` |
| `loading_limit_s` | `900` | Seconds without a heartbeat during a mission load before DCS is judged `hung` |
| `handshake_timeout_s` | `15` | Seconds one handshake attempt waits. Only time in which DCS runs frames counts, so a mission load does not use it up. After a DCS restart, how long the sidecar waits for the new DCS before it exits, so that its supervisor starts it again. On Windows a connect waits at least 5 s. |

**Change them when** your server's loads take longer than `loading_limit_s`, or the
verdict turns `hung` on short stalls you want to ignore. A mission load has no fixed
duration; these values only bound it. The sidecar takes no action on a verdict.

#### `[world] position_period_min_s`

**Does:** sets the shortest position period any controller's subscription gets. A
request for a shorter one gets this.

**Default:** `0.1`.

**Change it when** controllers' requests for fast positions cost too much. The agent's
budget holds either way.

#### `[world] materialize_per_frame`

**Does:** sets how many spawns may wait for DCS's answer at a time, outside the restore
after a load. So at most this many spawns reach DCS in one frame.

**Default:** `1`. At least 1.

**Change it when** controllers move many groups into DCS at once and the server has
frame time to spare. A larger value spawns faster and costs more per frame.

#### `[world] on_lost`

**Does:** sets what happens to a group DCS lost with no death: `report` waits for the
next full list of objects, `respawn` spawns it again once that is safe, and `dead`
marks it dead.

**Default:** `"report"`. A controller can set it per group with `SetLostPolicy`.

**Change it when** your campaign should always respawn lost groups, or always count them
dead.

#### `[world] destroy_retries`

**Does:** sets how many refused or unknown destroys of one entity the sidecar sends
before it stops. An entity leaving DCS then goes `lost` with the error.

**Default:** `3`. At least 1.

**Change it** rarely: raise it if destroys fail on a busy server and later succeed.

#### `[log] dir` and `level`

Accepted, and have no effect in this release. The sidecar writes its log to standard
output and standard error.

#### `[missions] dir` and `dcs_dir`

**Does:** names the mission folder twice: `dir` as the sidecar opens it, `dcs_dir` as
DCS opens it. The folder holds one template per map as `<theater>.miz`, and the
missions the sidecar writes under `shells/`.

**Default:** empty. Activating or reloading a campaign fails while either is empty.

**Set them** always. Keep the folder outside the profile's `nanoprobe` folder, such as
`<profile>/Missions/nanoprobe`, so uninstall does not delete your templates.
[mission-templates.md](mission-templates.md) explains the templates.

#### `[missions] template_dirs`

**Does:** lists the folders a controller may name a file in: a campaign's own mission
template, a group template file, a static template file, a payload preset file, or an
image or sound file a campaign packs into its missions (`SetMissionFile`).
Subfolders are allowed. Links that lead outside a listed folder are refused.

**Default:** `[]`, which refuses every path.

**Set it when** controllers use those calls. Only the administrator should be able to
write in these folders: the sidecar reads any file there that a controller names.

## The tokens file

A tokens file gives callers of the API their roles. It holds the SHA-256 of each
token, never the token:

```toml
[[token]]
name = "campaign-host"   # for logs
role = "controller"      # observer, controller or admin
sha256 = "<64 lower-case hex digits>"
```

A token is at least 32 characters of printable ASCII with no spaces. The intended form
is 64 hex digits from 32 random bytes. A caller sends the token as
`authorization: Bearer <token>`. An unknown key or role, a malformed hash, or two
entries with one hash stops the sidecar from starting. Install makes no API tokens; make
them yourself. The hash covers the token's text exactly, with no newline.

On Linux and macOS:

```sh
TOKEN=$(openssl rand -hex 32)
printf %s "$TOKEN" | shasum -a 256
```

In PowerShell on Windows:

```powershell
$b = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($b)
$token = -join ($b | ForEach-Object { $_.ToString('x2') })
$hash = [Security.Cryptography.SHA256]::Create().ComputeHash([Text.Encoding]::ASCII.GetBytes($token))
-join ($hash | ForEach-Object { $_.ToString('x2') })
```

Give the token to the program that calls the API. Put the hash in `sha256`.
[controller-guide.md](controller-guide.md), "Sessions, roles and the lease", says what
each role may call.

## The kill switch

Create an empty file named `disable` in the profile's `nanoprobe` folder. A running
agent stops all work within 2 s of the next frame, and DCS runs on. With the file there
when DCS starts, the loader runs nothing. Delete it and restart DCS to turn the agent
back on.

## Environment variables

| Variable | Read by | What it does |
|---|---|---|
| `NANOPROBE_ADDR` | `nanoprobe watch`, the hello examples | The API address, such as `http://127.0.0.1:47311` |
| `NANOPROBE_TOKEN` | `nanoprobe watch`, `nanoprobe-sidecar health`, the hello examples | The API token to send |
| `WINEPREFIX` | `nanoprobe install`, `uninstall` and `doctor` on Linux | The Wine prefix, when `--wine-prefix` is not given |
