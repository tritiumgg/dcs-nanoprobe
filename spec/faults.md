# Faults and chaos

Fault injection breaks the agent or the sidecar on purpose, so a test can check that
each fault is survived or refused with a clear log line. The chaos test kills either
side at random and times each reconnect. Both are development tooling: the agent's
`faults` feature is built only by `cargo xtask deploy --faults` and the host tests, and a
release never carries it.

## Agent faults

`inject(name[, arg])` on the agent's table arms one fault, which fires once. It returns
true, or nil and why: an unknown name, an argument out of range, or for `lose-control`
no open link. A build without the `faults` feature returns nil and `this build has no
fault injection`. The agent logs `fault armed` when it arms one.

`raise`, `stall`, `flood` and `panic` are taken by the next `frame` call, which logs
`injected fault: <name>` as it fires one. `lose-control` is armed on the link and taken
by the next control record the link queues, which may be many frames later; its only
log line is the drop's.

| Name | Argument | What the frame does | Expected outcome |
|---|---|---|---|
| `raise` | none | The `frame` entry raises the Lua error `injected Lua error` instead of pumping. | The hook's guard logs `onSimulationFrame raised`. The next frame pumps. The sidecar stays attached. |
| `stall` | 1 to 10,000 ms | Sleeps for the argument after the link's `begin`, then logs `injected fault: stall ended after <ms> ms`. | The sidecar stays attached: the agent dates the heartbeat's change when it reads it (spec/transport.md, "Detaching"). |
| `flood` | 1 to 100,000 records | Queues that many `LOG` records of about 1 KiB. | The ring fills and the queue overflows. Log records are dropped first, oldest first, so the flood's own record is often among them. The sidecar reads a `GAP` of class 2 and stays attached. |
| `lose-control` | none | The link drops its next control record. | `dropped a control record of kind <k>`, then a second `HELLO` in the same attach (spec/records.md, "Delivery classes"). |
| `panic` | none | Panics after the link's `begin`, holding the driver. | The entry catches it. The agent is disabled and sends `disabled: a caught panic` and a last `LIFECYCLE` of `DISABLED`. The hook logs `the agent is disabled; the hook goes on without it`. DCS runs on. |

## Sidecar faults

| Fault | How | Expected outcome |
|---|---|---|
| Stalled sidecar | `SIGSTOP` for 2 s, then `SIGCONT`, while a mission runs | The agent detaches after 0.5 s with no heartbeat change. The resumed sidecar finds itself detached and attaches again within 2 s. During a mission load the agent runs no frame, so it does not detach, and the resumed sidecar finds itself still attached and goes on (LIVE, 2026-11-13). |
| Command shorter than its header | `link-probe --short-command` | `a command too short for its header; detaching` |
| Broken inbound ring | `link-probe --bad-record <kind>`, below | The agent detaches with `detached: inbound ring: <check>`, and the sidecar attaches again. |

`--bad-record` writes to the mapped file past the sidecar's link, once the first
`HELLO`'s commands are answered. The probe sends nothing more in that session.

| Kind | What the probe writes |
|---|---|
| `past-tail` | A record header of length 64, and a tail 8 bytes past the old one: the record passes the tail. |
| `over-limit` | A record header whose length is the payload limit plus 8, and a tail past the whole record. |
| `misaligned` | A tail 4 bytes past the old one. |
| `past-capacity` | A tail more than the capacity ahead of the head. |
| `padding` | A padding record that does not end at the end of the ring. |

Each fails a different check of spec/transport.md, "Consuming", and the agent's message
names it. A record that passes the end of the ring is not injected: where it lies
depends on the tail, which the probe does not choose. The transport's unit tests cover
that check.

## Chaos

`chaos-runner` runs beside DCS and kills one side per cycle, chosen at random from a
seed, after the pair stayed attached for 0.1 to 2 s.

| Victim | Kill | Reconnect timed from | To |
|---|---|---|---|
| Sidecar | `SIGKILL`, then a new sidecar | The kill | The pair attached with the new sidecar boot |
| DCS | `SIGKILL` | The new agent's first frame, or when it listens if that is later | The pair attached with the new agent boot |

A DCS reconnect is timed from the first frame because the agent attaches only on a
frame, and DCS sends no frame until a mission runs (LIVE,
docs/results/2026-11-11-server-gate.md). The first frame is the first change of the
frame number once the status block shows a loaded mission, generation 1 or later,
running or paused. A DCS server that starts runs a few frames before it loads its first
mission, at generation 0, and none during the load that follows (LIVE,
docs/results/2026-11-13-s8-chaos-and-faults.md: 55 s). When the port the killed DCS
held is still taken, the agent listens only once it is free (spec/transport.md,
"Handshake"), up to about a minute after the kill under Wine. How long DCS takes to
start, load and free its port is not the platform's: the runner times the reconnect
from when the agent listens, and reports the wait for the port apart.

The runner reads the status block as the sidecar does, `ready` and `agent_boot` before
and after the fields. A cycle fails when a reconnect takes 2 s or more; when the pair
detaches, the frame number stops for 3 s, or a new agent boot appears, with no kill;
when one DCS kill is followed by more than one new agent boot; or when a sidecar reports
anything but a session end a kill explains, or `a HELLO of an earlier attach`: a
sidecar that left a `HELLO` unread reads it on its next attach to the same boot and
skips it (spec/transport.md, "Attaching").

After the run, these fail it, each read since the run's start in a log and the file it
rotated into: an error record in the agent's, the hook's or the shim's log; any record
in the agent's crash log; a log that cannot be read; a log that held records before the
run but none from its start, which shows records rotated away; a crash file DCS wrote;
and a count of the hook's starts other than the DCS kills (on the host, the kills and
the first start), which shows a missing log or one rotated away.

On the host, DCS is `lua/chaos/host.lua`, the hook and the shim in the strict mock. On
a docker-wine target, the runner runs in the game container, and the container's own
start loop brings a killed DCS back (LIVE, 2026-11-13: the new agent ran 72 s after a
`pkill -9`). There the runner asks before each DCS kill, and `cargo xtask chaos` lets it go on
only when no player is connected. DCS also stays up for at least 70 s between DCS kills,
so a host watchdog that checks once a minute sees it up between them.
