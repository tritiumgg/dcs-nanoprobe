# S13: the API on the fake sim

| | |
|---|---|
| Step | S13: gRPC services, leases, tokens, subscriptions, protocol tools, and a hello campaign in Rust |
| Date | 2026-11-13 |
| DCS | None. The server runs over the fake sim (`crates/sim-fake`); no claim here is about DCS. |
| Schema | `proto/nanoprobe/v1alpha1`, unchanged but for the `GetOutcome` comment |
| Programs | `nanoprobe-fake-sidecar` (`crates/fake-sidecar`) and `hello-campaign` (`examples/hello-rust`), through `cargo xtask hello [<target>]` |
| Result | The hello campaign runs on macOS, Linux and Windows. `buf breaking` passes against `main`. |

## Exit test

`cargo xtask hello` builds the two programs for a system, runs the server on a loopback
port with an ephemeral number, runs the hello campaign against it as its child, and
removes what it copied. The hello campaign takes the controller lease, creates and
activates a campaign, puts one ground group with a route in it, lists the world and
watches from the listed revision until the group is `live` and its lead has reached the
route's end, then releases the lease.

| System | Build | How it ran | Group states | Result |
|---|---|---|---|---|
| macOS 26 (Darwin 25.6.0), arm64 | host debug build | `cargo xtask hello` | materializing, live, arrived | passed |
| Linux, x86-64 | static `x86_64-unknown-linux-musl`, linked by `rust-lld` | `cargo xtask hello <docker-wine target>`: on the SSH host, outside the game container, in a `mktemp` folder | materializing, live, arrived | passed |
| Windows 11 (NT 10.0.26200), x86-64 | `x86_64-pc-windows-msvc`, cross-built with `cargo-xwin` | `cargo xtask hello <windows-ssh target>`: as the SSH account, in a folder under its temp folder | materializing, live, arrived | passed |

On both remote systems `cargo xtask hello` checked, after the run, that no process started
from the folder was left, then removed the folder and checked that it was gone. The same flow runs in the tests in-process (an in-memory pipe in place of a
socket) and over a loopback socket (`crates/fake-sidecar/tests/api.rs`).

`sh tools/protobreak.sh main` and `sh tools/protobreak.sh HEAD` pass. The only schema
change is the `GetOutcome` comment (coordinator ruling B).

## What was built

- `crates/proto`: Rust types and tonic stubs generated from the schema by `buf build` and
  `tonic-prost-build` (`cargo xtask proto`), committed with the descriptor set;
  `cargo xtask proto --check` in `mise run check` fails when they are stale.
- `crates/api`: the server over the world store and the reconciler. The `hub::Backend`
  trait is what the real agent's adapter implements in S17.
- The controller lease: one holder, a fencing epoch that is at least the wall clock in
  milliseconds, so it grows across restarts without a store; a stale or expired lease is
  `LEASE_LOST`. Property test over interleaved clients, clock steps and calls.
- Idempotency keys on every changing call: a repeat returns the first response; another
  request under the key is `IDEMPOTENCY_MISMATCH`; a failed call is not remembered.
  Property test against a model.
- Tokens and roles (spec/config.md, "Tokens file"; spec/errors.md, "Roles"), checked on
  every service, reflection included.
- World: list at one revision, then watch from it; upserts conflated per entity between
  wake-ups, and none for a change of position alone; positions in batches at most once
  per period, never under the cap; an area watch sends a group's last state when it
  leaves the area or dies, also between the list and the watch. Events: per-campaign logs and the
  server log, resumable by sequence number, never conflated. A stream that falls
  `stream_buffer` messages behind ends with `CONSUMER_TOO_SLOW`; event history is sent at
  the reader's pace. On shutdown every open stream ends with `UNAVAILABLE`.
- Protocol tools: `buf lint` and the breaking check (`mise run proto`), server reflection,
  and the stream watcher `nanoprobe watch world|events`.

## Not yet covered

Each of these answers or reads as stated, by design, until the step named.

| What | Now | Step |
|---|---|---|
| Effects, `Capture`, `SetOptions`, `RemoveEntity`, airbase owner and warehouse stock, `SetSampleRates` | `UNIMPLEMENTED`, with the method in the error detail's field | S14, S15 |
| `applied_spec_revision`, `applied_order_revision` | 0 | S14 (agent-held watermarks) |
| A `PutGroup` that adds or removes units, or changes category, country, positions or state | `INVALID_FIELD`: a replacement changes labels, skills, extra keys, components and unit types of the same units | S15 |
| Routes with more than one speed or road flag, waypoint extra keys, an orbit radius, attacking a unit or a static | `INVALID_FIELD` | S15 |
| Coalition | `COALITION_UNSPECIFIED` until S15 derives it from the country and the mission's coalitions: a spec takes none, and DCS takes it from the country. A coalition filter passes nothing. | S15 |
| Spawning with a captured life, fuel or ammunition | `INVALID_FIELD`: the spawn does not carry them yet | S15 |
| Unmanaged entities (player aircraft, Mission Editor units) | Not listed or watched; `managed_only` has no effect | S16, S18 (adoption into the store) |
| Players and Extension services | `UNIMPLEMENTED` | S16 |
| Initiator, target, weapon and place of an event | Left out: they are DCS object IDs, which the API never shows, until resolved to entities | S16 |
| A replacement static spec | `INVALID_FIELD` | S15 |
| Finishing, exporting and deleting a campaign; `SetNextMission`, `Reload` | `UNIMPLEMENTED`. Activation loads the fake's one mission. | S18, S19 |
| Persistence: campaigns, stores, event logs and idempotency keys | In memory, for the life of the process. Event logs keep every event, as `event_retention_days = 0` does. | S18 |
| The real agent behind the API | The fake sim only | S17 |
| TLS | Not served | S22 |
| Health from the status block | The fake reports itself healthy while it runs | S17, S19 |

A new controller, an activation, a suspension and a change of a campaign's default lost
policy each rebuild the reconciler from the store and attach again. That is the path of
a sidecar restart, and it starts the new command epoch that spec/commands.md asks for on
a change of controller.
