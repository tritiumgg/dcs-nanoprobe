# Configuration

Two files, both TOML. Every key has the default shown, except the two marked
required. An unknown key
is an error: the reader logs it and refuses to start, so a misspelled key is never
silently ignored.

## Agent: `<profile>/nanoprobe/agent.toml`

Read once, when the agent starts in a DCS process. A change needs a DCS restart. The
sidecar cannot change this file. It may ask for other sampling targets with `RATES`.

```toml
[transport]
map_path = ""          # the mapped file; empty means the default below
port = 47310           # loopback TCP port of the handshake
out_ring_kib = 4096    # outbound ring, a power of two from 256 to 65536
in_ring_kib = 1024     # inbound ring, a power of two from 256 to 65536
event_queue_kib = 1024 # the agent's queue for events and logs
control_queue = 1024   # the agent's queue for control records, in records

[budget]
overhead_share = 0.003 # agent overhead as a share of real time
commanded_ms = 2.0     # commanded work per frame, milliseconds

[sampling]
position_s = 1.0       # target period of each tier, seconds
vitals_s = 5.0
ammo_s = 30.0
warehouse_s = 60.0
enumeration_s = 60.0

[lua]
raw = false            # allow extensions, EVAL and emit
```

| Key | Rule |
|---|---|
| `map_path` | A path as DCS opens it. Default: `nanoprobe-<port>.map` in the folder `GetTempPathW` returns. Under Wine, set it to a path on a memory-backed volume the sidecar also mounts, such as `Z:\dev\shm\nanoprobe.map`. On Windows, when the sidecar runs as another account than DCS, set it to a path both accounts can open, such as `<profile>\nanoprobe\nanoprobe.map`: the default temp folder is per user (LIVE, docs/results/2026-11-12-s7-agent-in-the-game.md). A file in `<profile>/nanoprobe/` goes with the folder on undeploy. |
| `port` | The agent listens on `127.0.0.1` only, so the sidecar runs in the same network namespace as DCS. A sidecar container that joins the game container's network is restarted whenever the game container is (spec/transport.md). |
| `overhead_share`, `commanded_ms` | The two budget settings. The slice of a frame is `overhead_share` times the running average frame interval, and never more than 250 µs, which is fixed. `overhead_share` is above 0 and at most 1; `commanded_ms` is above 0 and at most 1,000. The agent's fixed work on every frame and the cross-state call set a floor under `overhead_share`: below it nothing is left for sampling and the mean exceeds the share. The floor is about (`F` + `x`) × frames per second / 10⁶, with `F` the fixed work and `x` the cross-state call of spec/sampling.md, "Slice": about 0.0028 at 113 frames a second with `F` about 3 µs and `x` 22–24 µs (PRIOR, S10, docs/results/2026-11-13-s10-sampling.md). S11 measures both. The agent logs `fixed work reached the overhead allowance` at `warn` in any 10 s window with a frame whose fixed work reached the allowance. |
| `[sampling]` | Defaults for the tiers. The sidecar may ask for other targets with `RATES` (spec/commands.md); the budget still holds. `vitals_s` and `ammo_s` are the periods of the rows a `SUBSCRIBE` names; any other row's life and fuel are read every 12 × `vitals_s`, and any other unit's ammunition only on demand (spec/sampling.md, "Tiers"). |
| `raw` | Off by default. Only this file turns it on. |

### Token

`<profile>/nanoprobe/token` holds 64 lower-case hex digits and nothing else; a reader
accepts one line ending after them. The
installer creates it from 32 random bytes, readable only by the account DCS runs as and
the sidecar's. The agent reads it at start. The sidecar presents it in the handshake
(spec/transport.md). It never leaves the server, and no log or bundle holds it.

### Kill switch

`<profile>/nanoprobe/disable` (spec/install.md). When it appears the agent sets the
lifecycle state to `DISABLED` within 2 s of the first frame after it, and does no more
work until DCS restarts. No frame runs during a mission load (spec/install.md, "Kill
switch").

## Sidecar: `sidecar.toml`

The sidecar takes the file's path as an argument. It reads the file at start.

```toml
[agent]
map_path = ""               # the mapped file as the sidecar opens it; required
handshake = "127.0.0.1:47310"
token_path = ""             # the token file as the sidecar opens it; required
poll_ms = 2                 # how often the sidecar reads the file, 1 to 5
command_queue = 4096        # effects and queries waiting for the inbound ring; more is QUEUE_FULL

[api]
listen = "127.0.0.1:47311"  # gRPC listener
tokens_path = ""            # tokens and roles; required for a listener that is not loopback
tls_cert = ""               # with tls_key, serve TLS
tls_key = ""
lease_ttl_s = 30            # a controller lease not renewed for this long is free
stream_buffer = 1024        # messages a stream may fall behind before it is ended
result_wait_s = 10          # how long an effect or a query call waits for its result

[store]
dir = "campaigns"           # one SQLite file per campaign, and the server-scoped store
enabled = true              # false: the author stores state through capture and restore
checkpoint_s = 5            # positions, fuel and life
backup_interval_h = 24      # consistent copies of the active store
backup_keep = 7
event_retention_days = 0    # event history; 0 keeps it for the life of the campaign
server_log_days = 30        # events recorded while no campaign is active
idempotency_keep_h = 24     # how long an idempotency key is remembered
world_changes_keep = 100000 # changes kept for a watch to resume from

[health]
heartbeat_stale_s = 10      # no heartbeat for this long outside a load: hung or gone (spec/errors.md)
loading_limit_s = 900       # no heartbeat for this long while loading: hung
handshake_timeout_s = 15    # how long one handshake attempt waits, counting only time in which the agent runs frames; after a DCS restart, how long before the sidecar exits

[world]
position_period_min_s = 0.1 # the cap: the shortest position period a subscription gets
materialize_per_frame = 1   # groups spawned per frame outside the restore burst
on_lost = "report"          # report, respawn or dead: the default policy for a lost group
destroy_retries = 3         # refused or unknown destroys of one entity before the sidecar stops

[log]
dir = "logs"
level = "info"
```

| Key | Rule |
|---|---|
| `[agent] map_path`, `token_path` | Paths on the sidecar's side. Under Wine with a native sidecar they are Linux paths to the same files the agent names with Windows paths. |
| `[api] listen` | Loopback by default. A listener on any other address refuses to start without `tokens_path`. |
| `tokens_path` | A TOML file of tokens, each with a role: `observer`, `controller` or `admin` (spec/errors.md, "Roles"). Kept outside any repository. |
| `[store] enabled` | With `false` the stores are in memory and last as long as the sidecar process (spec/world.md). |
| `position_period_min_s` | The only cap on subscription rates. A request for a shorter period gets this one. The agent's time slice holds whatever the period. |
| `[health]` | Limits, not expectations (spec/errors.md, "Health"). A mission load has no fixed duration; `loading_limit_s` is only when to call a silent DCS hung. |
| `on_lost` | An author may override it per group through the API. |
| `destroy_retries` | At least 1. A dematerializing entity whose destroy is refused or unknown this many times goes `lost` with the error (spec/world.md). A destroy of an entity DCS should not hold is tried again after the next full enumeration. |

Relative paths are relative to the folder that holds `sidecar.toml`.

### Health command

`nanoprobe-sidecar health --config <path>` asks the running sidecar for its verdict
(spec/errors.md), prints it with the seconds since the last heartbeat, and exits with:

| Exit code | Verdict |
|---|---|
| 0 | `healthy` |
| 1 | `hung` |
| 2 | `gone` |
| 3 | `disabled` |
| 4 | The sidecar did not answer |
