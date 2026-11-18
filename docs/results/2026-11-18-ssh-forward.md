# The xtask port forward on the shared SSH connection

`cargo xtask` reaches a target over SSH. Its plain calls share one connection per
destination (`ControlMaster=auto`, a control socket, `ControlPersist=60`). The port
forward that the sidecar tests use (`Forward` in `crates/xtask/src/ssh.rs`) is a
forward of that connection, so starting one asks the key agent for nothing when a
connection is up. Host tooling only. No DCS claim is made here.

## What the forward does

| Step | Command | Rule |
|---|---|---|
| Find the connection | `ssh -O check` | A control request talks to the control socket only. It cannot open a connection. |
| Open one if none is up | a plain call, `ssh ... -- <target> exit` | The only step that can open a connection. Bounded at 30 s. |
| Add the forward | `ssh -O forward -L <spec>` | A refusal is the error "the SSH forward did not come up: ..." |
| Keep it | `ssh -O forward -L <spec>` every 15 s | Each request is a client of the master, so the master's idle timer starts again. It also puts the forward back on a connection that a plain call had to open. |
| Remove it | `ssh -O cancel -L <spec>` | On `stop` and on drop, after the keeper has ended. The connection stays. |

Where SSH has no connection sharing (Windows OpenSSH), the forward is an `ssh -N -L`
process with a connection of its own.

A process that is killed does not remove its forward. The forward stays, unused,
until the shared connection closes.

## OpenSSH behavior this rests on

Observed on OpenSSH_10.3p1, client and server, macOS (Darwin 25.6.0), 2026-11-18,
against an sshd on the loopback address started for the test, with the command lines
above and `ControlPersist=20`:

| Observation | Result |
|---|---|
| `-O check` and `-O forward` with no connection up | Exit 255, "Control socket connect ... No such file or directory". sshd accepts no connection. |
| `-O forward` of a forward that is there | Exit 0. |
| `-O forward` on a local port in use | Exit 255, "Port forwarding failed". |
| A forward with no client request | The master closes within 30 s of the last client: a forward does not hold the connection. |
| `-O forward` every 8 s for 56 s, no plain call | The master stays up and the forward carries a connection. |
| `-O cancel` | Exit 0 and the local port refuses connections. Exit 0 also when the forward is absent. |
| After the last request | The master closes by its own timer. |
| Connections accepted by sshd for the whole sequence | 1. |

## The live test

`a_forward_on_a_real_destination_carries_a_connection` is an ignored test in
`crates/xtask/src/ssh.rs`. It starts two forwards to a service that sends first, reads
its greeting through each, drops one, holds the other with no plain call for longer
than `ControlPersist`, stops it, starts it again on the same local port, drops it, and
checks that the shared connection is still up.

```sh
NANOPROBE_FORWARD_DEST=<target> NANOPROBE_FORWARD_TO=127.0.0.1:22 \
  cargo test -p xtask -- --ignored --nocapture a_forward_on_a_real
```

| Run | Connection before the test | Hold | Result |
|---|---|---|---|
| Loopback sshd, OpenSSH_10.3p1, macOS, 2026-11-18 | None: the test's first forward opened it with `ControlPersist=60` | 90 s | Pass. sshd accepted 1 connection. One master process. The master closed by its own timer within 70 s of the test's end. |
| A Linux target over the network, client OpenSSH_10.3p1 on macOS, 2026-11-18 | Up, opened beforehand with a long `ControlPersist` | 75 s | Pass. The master kept its process ID and was the only `ssh` process with a TCP connection before, during and after. The test made 16 `ssh` calls, all control requests: 5 `check`, 8 `forward`, 3 `cancel`. |

The master process is the local `ssh` client, so the server's OpenSSH version does not
enter into the control requests or the idle timer.
