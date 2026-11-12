# Structured log

Every part of the platform writes the same record: the hook and the shim in Lua, the
agent and the sidecar in Rust. One record is one JSON object on one line, UTF-8, ending
in `\n`. A reader merges files from different writers by `wall`.

## Keys

| Key | Type | Required | Meaning |
|---|---|---|---|
| `wall` | string | yes | UTC wall time, RFC 3339, `Z` suffix. Milliseconds where the writer has them (`2026-11-10T14:03:07.123Z`); whole seconds from Lua (`2026-11-10T14:03:07Z`). |
| `level` | string | yes | `error`, `warn`, `info` or `debug` |
| `target` | string | yes | The writer: `hook`, `shim`, `agent` or `sidecar` |
| `msg` | string | yes | What happened, in words |
| `real` | number | no | Seconds of DCS real time, `DCS.getRealTime()`. Monotonic within one DCS process. |
| `model` | number | no | Seconds of model time in the current mission |
| `gen` | integer | no | Mission generation: 0 before the first mission load in this process, then one more on each `onMissionLoadBegin` |
| `thread` | integer | no | OS thread ID. Lua cannot read it; the agent can. |
| `build` | string | no | Build hash of the writer, as in the install's version folder name |
| `data` | object | no | Further values. Each is a string, a number, a boolean or null. |

Writers put the keys in the order above, and leave out an optional key they have no
value for. A number that is not finite is written as `null`.

## Files

Each writer appends to its own file and flushes after every record, so a crash loses at
most the record being written. The agent is the exception: it holds its records, of
every level, for up to a second and writes them on the frame where it makes its other
system calls (spec/transport.md, "Handshake"). A crash can lose that second from
`agent.log`. It does not lose it altogether: the records also go to the sidecar
through the outbound ring, and the crash handler writes `crash.log` itself. When a record would take the file past its cap, and the
file is not empty, the writer renames it to `<name>.1` (replacing an older `<name>.1`)
and starts a new file. A writer therefore keeps at most twice its cap on disk.

In an install, the files live in `<profile>/nanoprobe/logs/`: `hook.log`, `shim.log`,
`agent.log`. The default cap is 4 MiB.

Once the agent's link is open, every record the hook and the shim write, and every
record the agent writes, also goes to the sidecar as a `LOG` record (spec/records.md),
with the writer as its `target`.
