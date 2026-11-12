# Kill-the-peer cycles between the fake agent and the stub sidecar

| | |
|---|---|
| Step | S6 exit test: 1,000 kill-the-peer cycles between the fake agent and the stub: reconnect under 2 s, no record corrupted, rings and queues bounded |
| Date | 2026-11-12, 06:34 to 06:57 UTC |
| DCS | None. Host only: the agent's side is the fake agent (`crates/fake-agent`). |
| Platform | macOS 26.6.2 on arm64, Rust 1.99.1 |
| Script | `cargo xtask peerkill --cycles 1000 --keep <dir>` (`crates/xtask/src/peerkill.rs`), seed `ad4cd873af85c94c` |
| Code | The commit that adds this document |
| Result | PASS |

The fake agent and the stub sidecar run as two processes and share the mapped file and
the loopback handshake as spec/transport.md describes. The agent pumps 100 frames a
second. Both rings are 256 KiB, the smallest the spec allows. The agent's event queue
is 2 KiB, so that it overflows while a peer is down and the `GAP` path runs; the control
queue holds 64 records.

Each cycle runs the pair attached for 100 to 600 ms, then kills one peer chosen at
random with SIGKILL and starts it again. A reconnect is timed from the kill until the
file shows the agent attached to the new sidecar (or the new agent attached to the
sidecar). The pair must stay attached through every steady phase.

The stub journals every record it reads and every command before it makes it visible.
After the run, `crates/fake-agent/src/verify.rs` checks the journals: per agent boot,
record numbers run from 1 with no gap, padding records included; every record's bytes
are the values the fake agent derives from its boot and sequence numbers; a record read
again after a sidecar restart has the bytes first journalled; every lost event or log
line is counted by a `GAP`; every result is `APPLIED`, answers a command that was sent,
with that command's digest, at most once; every command the next `HELLO`'s `in_head`
shows taken has a result; no session ended on a failed check. `agent.log` must hold no
failed-check line, and the agent may detach only after a sidecar kill.

```
kills of the agent: 480, reconnect p50 1.227742292s p99 1.240225s max 1.246553875s
kills of the sidecar: 520, reconnect p50 842.775375ms p99 868.638958ms max 871.211167ms
records 357300 (256 padding, 27 journalled twice, 55 read again, 0 lines cut by a kill), agent boots 481, attaches 1001, events 53790 (11938 accounted by GAP), samples 62541, commands sent 158864 (1020 after inbound padding), taken 81800 (0 unknown after a resync), results 151172 (151172 applied)
largest resident memory: fake agent 2512 KiB, stub 3488 KiB
most held: outbound ring 178624 of 262144 B, inbound ring 53672 of 262144 B, event queue 2048 of 2048 B, control queue 1 of 64
peerkill: pass
```

The ring and queue fills are the largest values the harness saw, sampling the status
block once a millisecond; the rings are bounded by construction and the queues by their
caps, counted from the queue's contents. Resident memory is sampled with `ps` before
each kill.

After an agent kill the reconnect takes about 1.2 s: the new agent calls `accept` on its
first frame and then once a second, and the stub's first connect comes after that first
call. After a sidecar kill it takes about 0.85 s: the agent detaches when the heartbeat
has not changed for 0.5 s, closes the connection on the next frame, and accepts 100 ms
later.

## The other checks of the step

| Check | Command | Result |
|---|---|---|
| Host tests | `mise run check` | Pass |
| Loom: the ring (with a wrap and a padding record) and the metrics sequence lock | `mise run loom` | Pass. Each model fails when the tail's store is made relaxed or a seqlock fence is removed. |
| Miri: the memory, ring, status, codec, record and handshake tests, a two-thread ring test among them | `mise run miri` | Pass, 36 tests |
| Fuzzing: `lv`, the ring consumer, the record decoders, the handshake lines, the header check | `FUZZ_SECONDS=30 mise run fuzz` | 30 s per target, 0.5 to 12.6 million runs each, no failure |
| Misbehaviors | `cargo test -p nanoprobe-fake-agent --test misbehave` | 28 tests pass; each refused or survived with a log line |
