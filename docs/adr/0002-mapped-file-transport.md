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

Measured on 2026-11-11 on DCS 2.9.31.29434 (LIVE,
docs/results/2026-11-11-first-dll-and-spikes.md):

| Experiment | Wine dedicated server | Windows client |
|---|---|---|
| E25: one non-blocking `send` and one `recv` per frame on a loopback TCP socket | 380 µs at p50, 720 µs at p99 | 20 µs at p50, 30 µs at p99 |
| E24: a file mapped by the agent and by a native peer | The same pages; 0 torn reads in 64,426; echo lag of one frame | 0 torn reads in 1,290; echo lag of one frame |

E19 asked whether the socket is cheap enough for samples, with a limit of 10 µs per
frame. It is not, on either platform. Under Wine the socket alone would take 4–5% of
real time.

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
- A socket call under Wine costs more than a whole frame's slice (150–290 µs against a
  cap of 250 µs), and a file call may cost as much (UNVERIFIED). The agent therefore
  puts all its system calls on one kind of frame: the socket call, the kill-switch
  check and the log flush. Outside a handshake one frame a second is such a frame. If
  the three calls cost 300 µs each, that is 0.09% of real time, a third of the
  agent's budget, and each such frame is over the cap: one frame in 110 at 110 frames
  a second, which is inside the p99 target only above 100 frames a second. A
  handshake adds up to ten such frames a second for a second or two. Step S11
  measures all of it.
- The sidecar's heartbeat and boot are fields of the status block, on a line only the
  sidecar writes, not records in the inbound ring. A heartbeat is one word that is
  overwritten, which a ring of consumed records does not model.
- A sidecar that dies is detected from its heartbeat within 0.5 s, and a new one
  attaches within 2 s.
