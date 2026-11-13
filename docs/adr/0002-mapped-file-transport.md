# ADR 0002: A mapped file carries all data between the agent and the sidecar

## Status

Accepted

## Context

The agent runs on DCS's simulation thread, inside a Windows process that on Linux runs
under Wine. The sidecar is a native process on the same machine. They exchange samples,
events, command results and commands every frame. The agent's whole overhead budget is
0.3% of real time, with at most 250 µs in one frame.

The transport must work between a Wine process and a native Linux process. Windows
named shared memory, named events and named pipes do not cross that boundary.

Measured on DCS 2.9.31.29434 (LIVE): E24 and the Windows client on 2026-11-11
(docs/results/2026-11-11-first-dll-and-spikes.md), E25 on the Wine server on
2026-11-13 with the host's CPU idle states limited to C1 and the `performance` energy
preference (docs/results/2026-11-13-e25-cpu-idle.md):

| Experiment | Wine dedicated server | Windows client |
|---|---|---|
| E25: one non-blocking `send` and one `recv` per frame on a loopback TCP socket | 63–105 µs at p50, 103–141 µs at p99 | 20 µs at p50, 30 µs at p99 |
| E24: a file mapped by the agent and by a native peer | The same pages; 0 torn reads in 64,426; echo lag of one frame | 0 torn reads in 1,290; echo lag of one frame |

E19 asked whether the socket is cheap enough for samples, with a limit of 10 µs per
frame. It is not, on either platform, whenever data is sent. On the Wine server the
socket alone would take about 1.2% of real time, four times the agent's whole budget.
With the host's default power saving the server's cost was about five times higher
(380 µs at p50, 720 µs at p99), so the socket's cost also depends on the host's power
settings.

## Decision

One memory-mapped file carries all data: a status block and two single-producer,
single-consumer rings, specified in spec/transport.md. The agent touches it once per
frame and makes no system call to do so. The sidecar polls it every 1–5 ms.

A loopback TCP connection carries only the handshake, which authenticates the sidecar
with a token. The open connection is the session. Outside a handshake the agent makes
one socket call a second: an `accept` while no sidecar is connected, a `recv` while
one is. It makes none on any other frame.

Every word in the file has one writer, and nothing in it is a lock.

Rejected:

- A socket call per frame: E25.
- Windows named shared memory, events and pipes: they do not cross the Wine boundary.
- Unix sockets from Windows code: experimental in Wine.
- A cross-process wake-up for the sidecar: it needs a named kernel object. The sidecar
  polls instead, which costs the sidecar's CPU and none of DCS's.

## Consequences

- The transport is the same on Windows and under Wine, and its cost to the simulation
  thread does not depend on the platform's socket cost.
- The file outlives a DCS crash, so the sidecar reads the last heartbeat, lifecycle
  state and breadcrumb after DCS dies.
- A Wine server needs a memory-backed volume that both the game and the sidecar mount.
  The path is configuration (spec/config.md).
- The layout is a contract between two builds. The agent and the sidecar must agree on
  a layout hash, generated from the spec into `spec/transport.golden`, or the sidecar
  refuses to attach.
- Reading memory another process writes is outside what Rust's rules model. The code
  that does it is confined to the transport crate, copies before it validates, and is
  checked with Loom, Miri and fuzzing (step S6).
- The sidecar's poll adds up to 5 ms of latency to every record and command.
- A system call under Wine costs tens of microseconds: 23–60 µs for a socket call and
  38–56 µs for a file check at p50 on the Wine server
  (docs/results/2026-11-13-e25-cpu-idle.md). The agent therefore puts all its system
  calls on one kind of frame: the socket call, the kill-switch check, the reload check
  and the log flush. Outside a handshake one frame a second is such a frame. With no
  sidecar connected its calls together cost 117 µs at p50 and 165 µs at p99 with
  nothing to flush, and 188 µs at p99 with one record; the slowest flush of 600 took
  4.2 ms. That is 0.012% of real time, a twenty-fifth of the agent's budget, but at
  p99 two thirds to three quarters of the frame's 250 µs slice. With a sidecar
  connected a `recv` replaces the `accept`; that frame was not measured whole. A
  handshake adds up to ten such frames a second for a second or two. With the host's
  default power saving a socket call costs 141–290 µs on the same server; the file
  checks were not measured then. The system-call frame does no sampling, and step S11
  measures the per-frame p99 with and without it.
- The sidecar's heartbeat and boot are fields of the status block, on a line only the
  sidecar writes, not records in the inbound ring. A heartbeat is one word that is
  overwritten, which a ring of consumed records does not model.
- A sidecar that dies is detected from its heartbeat within 0.5 s, and a new one
  attaches within 2 s.
