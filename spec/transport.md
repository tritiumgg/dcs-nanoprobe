# Transport

The agent and the sidecar exchange all data through one memory-mapped file. A loopback
TCP connection carries only the handshake.
The decision and its measurements are in docs/adr/0002-mapped-file-transport.md.

The layouts and code tables in this document, spec/records.md and spec/commands.md are
the source of `spec/transport.golden`. `cargo xtask spec` regenerates it, and a test
fails when the two disagree. Change a table and the golden file in the same commit.

## Conventions

- Every integer and float is little-endian. Floats are IEEE 754.
- Offsets are in bytes from the start of the structure. Every field sits at a multiple
  of its own size.
- A `pad[N]` field is N bytes the writer sets to zero and the reader ignores.
- Each side treats the other side's bytes as untrusted. It reads each length, offset
  and position once into its own memory, checks it, and uses only its copy.
- Neither side forms a Rust reference to memory the other side may write. It copies
  out with raw-pointer reads, then validates.

## The file

| Part | Offset | Size | Writer |
|---|---|---|---|
| Status block | 0 | 4,096 | per field, below |
| Outbound ring | 4,096 | `out_capacity` | agent writes records, sidecar reads |
| Inbound ring | 4,096 + `out_capacity` | `in_capacity` | sidecar writes records, agent reads |

The agent creates the file at the path in `agent.toml` (spec/config.md), sets its size,
and maps it. On Windows the file sits in the user's temp folder. Under Wine it sits on a
memory-backed volume that the sidecar also mounts, reached through a drive letter that
maps to the Linux path. Only the agent's account and the sidecar's may open it.

The file outlives the DCS process. After DCS dies the sidecar still reads the last
heartbeat, lifecycle state and breadcrumb from it. A new DCS process writes the header
again, so a reader never mixes two processes' data (see "Start").

The sidecar may hold the file mapped while a new DCS process opens it, so the agent
never truncates or shrinks it. It opens an existing file as it is and grows it if it
is smaller than the configured size. If it is larger, the agent leaves the size alone:
`file_size` and the ring offsets and capacities in the header describe the part in
use, and the bytes after it are unused.

Ring capacities are powers of two from 256 KiB to 64 MiB. The defaults are 4 MiB
outbound and 1 MiB inbound.

## Status block

Every field has one writer. Nothing in the file is a lock, so a crash on either side
leaves nothing held. Each 128-byte line holds fields of one writer only.

A field the other side reads while it may change is read and written as an atomic
64-bit or 32-bit word. Ring positions, `ready`, the boots and `metrics_seq` are stored
with release ordering and loaded with acquire ordering. The metrics part is written
under a sequence lock (see "Metrics").

The sidecar's fields are in the status block, on their own line, not in the inbound
ring: a heartbeat and a boot are single words that are overwritten, and a ring holds
records that are consumed.

### Layout: status

| Offset | Type | Field | Writer | Meaning |
|---|---|---|---|---|
| 0x0000 | u64 | magic | agent | ASCII `npmap`, then three zero bytes |
| 0x0008 | u32 | layout_version | agent | 1 |
| 0x000C | u32 | interface | agent | The agent DLL's interface version: the ABI between the DLL and its Lua hook and shim, which the hook checks. The sidecar logs it and does not check it. |
| 0x0010 | u64 | layout_hash | agent | The 16 hex digits of the hash line of `spec/transport.golden`, read as one hexadecimal number |
| 0x0018 | u64 | file_size | agent | 4,096 + `out_capacity` + `in_capacity`: the bytes in use, at most the file's length |
| 0x0020 | u64 | agent_boot | agent | A random non-zero value chosen once per DCS process |
| 0x0028 | u64 | build | agent | The first 16 hex digits of the install's build (spec/install.md), as a number |
| 0x0030 | u64 | created_ms | agent | Wall time the header was written, milliseconds since the Unix epoch |
| 0x0038 | u64 | clock_hz | agent | Ticks per second of the agent's clock |
| 0x0040 | u64 | out_offset | agent | 4,096 |
| 0x0048 | u64 | out_capacity | agent | Bytes in the outbound ring |
| 0x0050 | u64 | in_offset | agent | 4,096 + `out_capacity` |
| 0x0058 | u64 | in_capacity | agent | Bytes in the inbound ring |
| 0x0060 | u64 | ready | agent | 0 while the header is written, then 1 |
| 0x0068 | pad[24] | reserved0 | agent | |
| 0x0080 | u64 | frame | agent | Pump calls in this process. The agent's heartbeat. |
| 0x0088 | u64 | clock | agent | The agent's clock at the last pump call |
| 0x0090 | f64 | model_time | agent | Model time at the last pump call, seconds |
| 0x0098 | f64 | real_time | agent | `DCS.getRealTime()` at the last pump call, seconds |
| 0x00A0 | u32 | lifecycle | agent | A `lifecycle` code |
| 0x00A4 | u32 | mission_gen | agent | Mission generation |
| 0x00A8 | u64 | campaign | agent | The active campaign ID, 0 for none |
| 0x00B0 | u64 | out_tail | agent | Outbound write position |
| 0x00B8 | u64 | in_head | agent | Inbound read position |
| 0x00C0 | u64 | epoch | agent | The command epoch the agent accepts (spec/commands.md) |
| 0x00C8 | u64 | last_applied_seq | agent | The highest command sequence number handled in `epoch` |
| 0x00D0 | u64 | sidecar_boot_seen | agent | The sidecar boot the agent is attached to, 0 when detached |
| 0x00D8 | u32 | flags | agent | `agent_flags` bits |
| 0x00DC | u32 | control_queued | agent | Control records waiting in the agent's queue |
| 0x00E0 | u64 | crumb | agent | The breadcrumb: the site of the last risky call |
| 0x00E8 | u64 | crumb_arg | agent | A value for the site, such as an opcode or object ID |
| 0x00F0 | u64 | crumb_frame | agent | `frame` when the breadcrumb was stored |
| 0x00F8 | u64 | crumb_clock | agent | The agent's clock when the breadcrumb was stored |
| 0x0100 | u64 | samples_dropped | agent | Sample records taken and not written because the ring was full |
| 0x0108 | u64 | events_dropped | agent | Event records dropped from the agent's queue |
| 0x0110 | u64 | logs_dropped | agent | Log records dropped from the agent's queue |
| 0x0118 | u64 | gaps | agent | `GAP` records written |
| 0x0120 | u64 | commands_applied | agent | Commands handled, of every result |
| 0x0128 | u64 | commands_over_budget | agent | Commands whose time exceeded the commanded-work budget |
| 0x0130 | u64 | lua_heap_kib | agent | `collectgarbage('count')` in the `server` state, KiB |
| 0x0138 | u64 | event_queue_bytes | agent | Bytes in the agent's event and log queue |
| 0x0140 | pad[64] | reserved1 | agent | |
| 0x0180 | u64 | sidecar_boot | sidecar | A random non-zero value chosen once per sidecar process |
| 0x0188 | u64 | sidecar_heartbeat | sidecar | Incremented every poll interval while attached, on a schedule of its own that the journal does not hold up |
| 0x0190 | u64 | agent_boot_seen | sidecar | The agent boot the sidecar attached to |
| 0x0198 | u64 | in_tail | sidecar | Inbound write position |
| 0x01A0 | u64 | out_head | sidecar | Outbound read position |
| 0x01A8 | u64 | sidecar_ms | sidecar | Wall time of the last heartbeat, milliseconds since the Unix epoch |
| 0x01B0 | u64 | attach_seen | sidecar | The attach ID of the handshake the sidecar finished attaching for |
| 0x01B8 | pad[72] | reserved2 | sidecar | |
| 0x0200 | u64 | metrics_seq | agent | Sequence lock over the metrics fields: odd while they change |
| 0x0208 | f64 | slice_us | agent | The agent's time slice for the current frame, microseconds |
| 0x0210 | f64 | frame_interval_us | agent | Running average of the interval between pump calls, microseconds |
| 0x0218 | f64 | period_position | agent | Achieved period of the position tier, seconds |
| 0x0220 | f64 | period_vitals | agent | Achieved period of the vitals tier, seconds |
| 0x0228 | f64 | period_ammo | agent | Achieved period of the ammunition tier, seconds |
| 0x0230 | f64 | period_warehouse | agent | Achieved period of the warehouse and airbase tier, seconds |
| 0x0238 | f64 | period_enumeration | agent | Achieved period of the full enumeration, seconds |
| 0x0240 | u64 | rows_per_clock | agent | Rows the sampling loop passes between clock reads |
| 0x0248 | pad[56] | reserved3 | agent | |
| 0x0280 | u64[32] | hist_pump | agent | Agent time per pump call |
| 0x0380 | u64[32] | hist_interval | agent | Interval between pump calls |
| 0x0480 | u64[32] | hist_command | agent | DCS time per command |
| 0x0580 | u64[32] | hist_event | agent | Agent time per event handled |
| 0x0680 | u64[32] | hist_syscall | agent | Agent time per system-call frame, also counted in `hist_pump` |
| 0x0780 | pad[2176] | reserved4 | agent | |

A period is 0 only for a tier that has never owed rows (spec/sampling.md, "Achieved
period").

**Layout version.** `cargo xtask spec` writes `layout_version` into
spec/transport.golden, so it is part of `layout_hash`, and a sidecar refuses an agent
whose hash differs from its own. The golden file changes with every table here, in
spec/records.md and in spec/commands.md. When agent behavior the sidecar reads
changes and no table does, `layout_version` goes up by one.

**Breadcrumbs.** `crumb` 0 means no risky call is in progress. `crumb` 0x10000 plus an
opcode means that command's handler runs, and `crumb_arg` is its `seq`. `crumb`
0x20000 means an extension's hook runs, and the crumb's text is the extension's name
(spec/commands.md, "Extensions"). The agent adds codes for later risky calls here.

### Codes: lifecycle

| Code | Name | Meaning |
|---|---|---|
| 1 | STARTING | The agent is open and no mission has loaded |
| 2 | LOADING | A mission load began (`onMissionLoadBegin`). No frames arrive until it ends (LIVE, docs/results/2026-11-11-server-gate.md). |
| 3 | RUNNING | The simulation runs |
| 4 | PAUSED | The simulation is paused (`DCS.getPause()`) |
| 5 | STOPPING | The mission or DCS is stopping |
| 6 | DISABLED | The kill switch or a caught panic stopped the agent's work |

### Codes: agent_flags

| Code | Name | Meaning |
|---|---|---|
| 1 | ATTACHED | A sidecar is attached |
| 2 | CONTROL_LOST | A control record was dropped; the sidecar resyncs |
| 4 | COMMANDS_HELD | The control queue is full and the agent reads no commands |
| 8 | RAW_LUA | Raw Lua is on in `agent.toml` |
| 16 | NO_HANDSHAKE | The handshake socket is not open: there is no token, or the listen failed and is tried again |

### Metrics

The metrics fields are every field from `slice_us` to `hist_syscall`, the histograms
included. The agent changes them at most once per frame:

1. Store `metrics_seq + 1` (odd), then a release fence, so no field write is seen
   before the odd value.
2. Write the fields.
3. Store `metrics_seq + 2` (even), with release ordering.

The sidecar loads `metrics_seq` (acquire), copies the fields, makes an acquire fence,
and loads `metrics_seq` again. It keeps the copy only if both loads are the same even
value; otherwise it tries again on its next poll.

Histogram bucket 0 counts values under 1 µs. Bucket `i` from 1 to 30 counts values
from 2^(i−1) µs up to 2^i µs. Bucket 31 counts everything from 2^30 µs. Counts only grow
within one DCS process; the sidecar takes differences.

## Rings

Each ring is single-producer and single-consumer. The producer owns the tail and the
consumer owns the head, both in the status block:

| Ring | Producer | Tail | Consumer | Head |
|---|---|---|---|---|
| Outbound | agent | `out_tail` | sidecar | `out_head` |
| Inbound | sidecar | `in_tail` | agent | `in_head` |

Positions are byte counts since the ring was reset, and only grow. A position maps to
the offset `position & (capacity − 1)`. The used space is `tail − head`; the free space
is `capacity − (tail − head)`. Both positions are multiples of 8.

### Layout: record

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | len | Bytes of payload after this header, not counting padding |
| 0x0004 | u16 | kind | A record kind (spec/records.md) or an opcode (spec/commands.md) |
| 0x0006 | u16 | flags | 0. A reader ignores the bits. |

A record takes `8 + len` bytes rounded up to a multiple of 8. Its payload starts at
offset 8. A payload is at most `capacity / 8` bytes.

Every record is contiguous. When a record does not fit between its position and the end
of the ring, the producer first writes a padding record (kind 0) that ends exactly at
the end of the ring, and puts the record at offset 0.

### Producing

1. Load the head (acquire). Check that it is a multiple of 8, at most the tail, and at
   least the tail minus the capacity. Compute the free space.
2. Write records from the tail onward, keeping the new tail in private memory.
3. Store the new tail once (release).

The agent stores `out_tail` once per frame, so the sidecar sees each frame's records
together. The sidecar stores `in_tail` once per batch of commands.

The agent also acts outside the pump, in two callbacks that run when no frame will
follow: `onMissionLoadBegin` and `onSimulationStop`. In each it stores `lifecycle` and
`mission_gen` in the status block, so the block reads `LOADING` for the whole of a
load. It then publishes a batch like a pump's: what its queues hold, in order, ending
with the `LIFECYCLE` record, each behind a `FRAME` with its stamps, then `out_tail`. If
it is detached or owes a `HELLO`, it writes nothing; what the ring has no room for
waits in the queue for the next frame.

### Consuming

1. Load the tail (acquire). Check that `tail − head` is at most the capacity, and that
   both are multiples of 8.
2. For each record from the head: read `len` and `kind` once; check that `len` is at
   most `capacity / 8`, that the whole record lies before the tail, and that it does
   not pass the end of the ring: its offset plus `8 + len` rounded up to 8 is at most
   the capacity. A padding record must end exactly at the end of the ring. Then copy
   the payload out.
3. Store the new head (release).

When each side stores its head:

- The agent takes one command at a time. It copies the command out, stores `in_head`
  past it (release), and only then runs it. So `in_head` is exactly the commands the
  agent took: a command before it was run, or was being run when DCS died; a command
  after it was never touched. The agent does not take a command it will not run in
  this frame.
- The sidecar stores `out_head` only when every event-class and control-class record
  before it is durable in its journal, and only at a `FRAME` record or at the tail, so
  a sidecar that starts there always reads the stamps first. Its heartbeat does not
  wait for the journal.

A sidecar that dies between reading a record and storing the head reads the record
again after it restarts. Every record has a number: `FRAME` carries the number of the
record after it, and each record after that is one more (spec/records.md). The sidecar
drops a record whose agent boot and number it has already journalled.

A record of an unknown kind is skipped by its length; for a command, see
spec/commands.md, "Unknown opcodes". A check that fails, in producing
or consuming, means the other side is broken: the agent detaches and logs it; the
sidecar reports it and attaches again through the handshake. A sidecar whose session
of an agent boot ended on a failed record check does not read from the kept `out_head`
when it attaches to that boot again, which would meet the same record: it sets
`out_head` to `out_tail` and reports the records between as lost.

A record whose payload would be over the limit is never written. What the agent writes
in its place is in spec/records.md, "Oversized records". The sidecar refuses to build
a command over the limit (spec/errors.md).

## Encodings

Variable parts of a payload follow its fixed fields, in the order the layout lists
them, with no padding between them.

| Type | Bytes |
|---|---|
| `str` | A `u16` length, then that many bytes of UTF-8 |
| `T[n]` | `n` values of type `T`, where `n` is a field named earlier in the same payload |
| `tlv[n]` | `n` tagged fields, each a `u16` field code, then an `lv` value |
| `lv` | A Lua value: a `u8` tag from `lv_tag`, then the bytes the tag's row gives |

### Codes: encoding

| Code | Name | Prefix | Meaning |
|---|---|---|---|
| 1 | STR | u16 | The length before a `str` |
| 2 | TLV | u16 | The field code before each `tlv` value |
| 3 | LV | u8 | The tag before each `lv` value |

### Codes: lv_tag

| Code | Name | Bytes | Meaning |
|---|---|---|---|
| 0 | NIL | 0 | nil |
| 1 | FALSE | 0 | false |
| 2 | TRUE | 0 | true |
| 3 | NUMBER | f64 | A number |
| 4 | STRING | u32+n | A `u32` length, then the bytes |
| 5 | TABLE | u32+pairs | A `u32` pair count, then each key `lv` and value `lv` |
| 6 | OTHER | str | The Lua type name of a value that cannot be carried, such as `function` or `userdata` |

**Strings.** A Lua string is any bytes. The agent writes a `str` as valid UTF-8: it
replaces each invalid sequence with U+FFFD, and cuts a string that would pass 65,535
bytes at a character boundary. The sidecar treats a `str` that is not valid UTF-8 as a
failed check. An `lv` string keeps its bytes as they are; the sidecar replaces invalid
sequences with U+FFFD when it turns one into an API value.

A value nests tables at most 16 deep, and each table holds at most 65,536 pairs. A decoder rejects a value
that breaks a limit or runs past the payload, and never calls a method on what it
decodes. A key that is nil or NaN is rejected. The agent decodes an `lv` into a Lua
value inside `lua_pcall`.

## Handshake

The agent listens on `127.0.0.1:<port>` (spec/config.md), so the sidecar must run in
the same network namespace as DCS: on the same Windows machine, or under Docker in a
container that shares the game container's network. The mapped file needs only a
shared volume; the handshake needs the shared network. A container that joins another
container's network keeps the network it joined at its start: when the game container
is restarted, the sidecar's container must be restarted with it. A sidecar that sees
a new `agent_boot` in the file and whose TCP connect is refused for
`handshake_timeout_s` exits with an error, so that whatever runs it starts it again.
A connect that succeeds and gets no answer is a DCS that is loading, and the sidecar
waits on that connection: its `handshake_timeout_s` counts only time in which the
frame number in the file moves. A sidecar that gave up and connected again would leave
connections the agent answers one by one once frames resume, each held up to 2 s for
an attach that never comes (LIVE, 2026-11-13: a reconnect of 9.4 s after a 55 s load).

When the listen fails, the agent sets `NO_HANDSHAKE` and tries again on each
system-call frame, once a second, until it succeeds; then it clears the flag. That try
is the frame's one socket call. Under Wine, the port a killed DCS process held stays
taken for up to about a minute after the kill (LIVE, 2026-11-13: `OS Error 10048` 59 s
after a `pkill -9`), so an agent that starts sooner listens only once it is free.

**All of the agent's system calls happen on one kind of frame.** Under Wine one socket
call costs 23–60 µs at p50 and a file check 38–56 µs. With no sidecar connected that
frame's calls together cost 117 µs at p50 and 165 µs at p99 with nothing to flush, and
188 µs at p99 with one record: two thirds to three quarters of the frame's slice. The
slowest flush of 600 took 4.2 ms. (LIVE, on a host with CPU idle states limited to C1
and the `performance` energy preference, docs/results/2026-11-13-e25-cpu-idle.md.)
With the host's default power saving one socket call costs 141–290 µs (LIVE,
docs/results/2026-11-11-first-dll-and-spikes.md, E25); the file checks and the whole
frame were not measured there, and with file checks costing as much the frame would
exceed the slice (UNVERIFIED). The agent therefore makes at most one socket call on a
frame, on few frames, and on that same frame checks the kill switch and the shim
reload request and flushes its log file (spec/install.md, spec/log.md). It makes no system call on any other
frame, except the one that finds the kill switch, which also closes the connection and
the listener. Outside a handshake such a frame comes once a second, so the kill switch
is seen within a second (spec/install.md).

While a sidecar is attached, the frame after a system-call frame is never one, however
long ago the last one was. When frames come a second or more apart, as when DCS stalls,
every frame is due its system calls; with this rule every second frame makes them and
the frames between sample. The kill switch and the reload request are then seen within
two frames, and a close waits for the next system-call frame. Before the attach and
during a handshake every frame that is due makes its call.

A system-call frame does no sampling (spec/sampling.md). Its time counts toward the
agent's mean overhead, which never exceeds `overhead_share`: the agent repays it
spread over the frames that follow, never by skipping them (spec/sampling.md,
"Slice"). Its time is counted in `hist_syscall` as well as in `hist_pump`.

| State | Socket call | At most |
|---|---|---|
| No connection | one `accept` | once a second; 100 ms after closing a connection |
| A connection to close | one close | on the next frame |
| A connection, before `OK` or `ERR` | one `recv` or one `send` | once every 100 ms |
| After `OK`, not yet attached | none: the agent watches `attach_seen` in the file | |
| Attached | one `recv`, which shows whether the sidecar closed the connection | once a second, and never on two frames in a row |

The agent closes a connection on the frame after it decides to, as that frame's one
call, whatever the reason: a refused `HELLO`, a timeout, a refused attach, a detach.
Making an accepted connection non-blocking goes with its `accept`.

That is one such frame a second, outside a handshake: under 1% of frames above 100
frames a second, so inside the p99 target there. A handshake adds up to ten such
frames a second for a second or two. Step S11 measures it.

The agent serves one connection at a time and does not accept while it has one. The
operating system still completes a TCP connect to the listening socket, into its
backlog, when the agent calls no `accept`: that a connect succeeds shows only that the
process is alive. It
closes a connection that has not sent a complete `HELLO` line within 2 s, and one that
has not attached within 2 s of `OK`. A later connection waits in the listen backlog.

Messages are UTF-8 lines ending in `\n`, at most 1,024 bytes, with ASCII keywords and
fields separated by one space. Numbers are lower-case hex without a prefix.

| From | Line | Meaning |
|---|---|---|
| sidecar | `HELLO 1 <token> <sidecar_boot>` | Protocol version 1, the token from the token file, the sidecar's boot |
| agent | `OK <layout_hash> <agent_boot> <attach_id> <path>` | The layout hash, the agent's boot, a random non-zero number for this attach, and the file's path as the agent opened it. `<path>` is the rest of the line and may hold spaces. |
| agent | `ERR <reason>` | Refused. Reasons: `version`, `token`, `disabled`, `no_file`. The agent closes the connection. A line that is not a well-formed `HELLO`, or one with a zero boot, is answered `ERR version`. The agent never sends `disabled` or `no_file`: a disabled agent has closed its listener, and an agent without its file does not listen. A sidecar then meets a refused connect, and a disabled agent's file shows `DISABLED`. |

After `OK` neither side sends anything. The open connection is the session: closing it
ends the session. The sidecar's heartbeat in the file shows that it is alive.

The token is compared in constant time. The sidecar maps the path it is configured
with, not the one the agent sends: under Wine the agent's path is a Windows path. The
sidecar refuses to attach when the file's `layout_hash` or `agent_boot` differs from
the agent's `OK` line.

## Attaching

The agent accepts a `HELLO` only while detached, so nothing reads or writes the rings
while a sidecar attaches. The attach ID fences the attach: the agent attaches only
when the sidecar has stored the ID of the open connection's `OK` in `attach_seen`,
which the sidecar does last. Values a sidecar left in the file from an earlier attach
never match a new ID.

The sidecar, after `OK`:

1. Maps the file and checks `magic`, `layout_version`, `layout_hash`, that `file_size`
   is 4,096 plus the two capacities and at most the length of the file, the ring
   offsets and capacities, and that `ready` is 1.
2. Loads `out_tail` (acquire). If `agent_boot_seen` differs from the file's
   `agent_boot`, this DCS process is new to it: it sets `out_head` and `in_tail` to 0.
   Otherwise a sidecar was attached to the same process before: it keeps `out_head`,
   so it reads what was left unread, and sets `in_tail` to `in_head`, so a command
   written and not yet handled is never applied. A kept `out_head` that fails the
   producer's check against that `out_tail` (step 1 of "Producing") was left by a late
   store; the sidecar sets it to `out_tail` and reports the records between as lost.
3. Stores `agent_boot_seen` and `sidecar_boot`, starts its heartbeat, and last stores
   `attach_seen` (release).

The agent, on each frame after it sent `OK`, checks that `attach_seen` is the attach
ID it sent, and that `sidecar_boot` and `agent_boot_seen` match the `HELLO` and its own
boot. When they do, it checks the sidecar's positions: `out_head` by step 1 of
"Producing", and `in_tail`, which must equal its `in_head`. If either fails, it refuses
the attach: it logs it and closes the connection. Otherwise:

1. If the sidecar boot differs from the one it last served, it drops the position
   subscriptions and sample rates the old sidecar set; it keeps everything else.
2. Stores `sidecar_boot_seen` and sets `ATTACHED`, and reads the inbound ring on from
   `in_head`, with no tail kept from before.
3. Writes its queued records in the order they were made, with their `GAP` records
   (spec/records.md). This may take several frames. It reads no command meanwhile.
4. Writes a `HELLO` record and, right after it under the same stamps, a `LIFECYCLE` of
   its current state (spec/records.md, "Delivery classes"); both or neither. It then
   starts a full enumeration and reads commands again.

The sidecar writes only control commands until it has read that `HELLO`. Its first
command is `CAMPAIGN` (spec/commands.md). The `HELLO` of this attach is the first
`HELLO` naming this sidecar's boot at or after the `out_tail` the sidecar loaded in
step 2: the agent writes nothing while detached, so a `HELLO` before it is an earlier
attach's, left unread by a sidecar of the same boot.

## Detaching

The agent detaches when `sidecar_heartbeat` has not changed for 0.5 s of real time, or
when its `recv` shows the connection closed. The heartbeat is what finds a dead sidecar
quickly: a sidecar polls every 1–5 ms, so 0.5 s without a change is not a slow poll.
The agent counts the 0.5 s from the time it last saw the value change, taking that time
when it reads the value, not at the start of the frame. Otherwise a frame in which the
agent itself stalled dates the change too early and detaches a live sidecar.
A new sidecar is then attached within 2 s of the old one's death: 0.5 s to detach, the
close on the next frame and the `accept` 100 ms after it, and a handshake of three
calls. The agent then:

- closes the connection and forgets the accepted `HELLO` and its attach ID, so
  attaching again needs a new handshake;
- stores 0 in `sidecar_boot_seen` and clears `ATTACHED`;
- writes nothing to the outbound ring and reads nothing from the inbound ring;
- stops sampling, and keeps its tables, watermarks and applied revisions;
- records events and logs into its queue, oldest dropped first (spec/records.md).

A sidecar stops writing to the file when its connection closes, when `sidecar_boot` or
`attach_seen` in the file is not its own, or when `sidecar_boot_seen` changes from its
own boot to anything else, 0 included. It then starts again from the handshake. When the
connection closed or `sidecar_boot_seen` changed while `sidecar_boot` and `attach_seen`
were still its own, it first reads the outbound ring once more, to its tail, and stores
nothing: the agent publishes its last records, such as the kill switch's `LIFECYCLE`,
before it closes the connection. It
checks these:

- on every poll, before it writes;
- again after every store of `in_tail` or `out_head`;
- first of all, before any write, when more than 0.25 s passed since its last poll.
  A sidecar that was stopped for a while (a paused container, a debugger) was detached
  after 0.5 s, and another may hold the file.

Because the agent serves one connection and attaches only on a new attach ID, a
sidecar that was detached cannot be attached again on what it wrote before. The checks
are not a lock: a sidecar stopped between a check and a store can still make that one
store after another sidecar attached. The agent's position checks catch a head or
tail that this leaves impossible, and it detaches; a store that leaves them possible
can lose or repeat records once. This is accepted: it needs a sidecar frozen for over
half a second at one instruction while a second sidecar starts.

The sidecar reads the status block as a snapshot: it loads `ready` and `agent_boot`,
copies the fields it needs, makes an acquire fence, and loads `ready` and `agent_boot`
again. It keeps the copy only when both loads agree and `ready` is 1. It never takes a
smaller `in_head` or `out_tail` for the same `agent_boot`. When `agent_boot` changes,
DCS has restarted: the sidecar stops, handshakes again, and maps the file again in
case its size changed.

The mission keeps running whether or not a sidecar is attached.

## Start

When the agent opens in a new DCS process it:

1. Stores 0 in `ready`, then a release fence.
2. Writes every header field, `out_tail` and `in_head` as 0, and zeros the other agent
   fields. It does not touch a sidecar field.
3. Stores 1 in `ready` (release).
