# ADR 0039: The health verdict holds its last value until a probe finishes

## Status

Accepted

## Context

The sidecar cannot see the DCS process. It judges from the mapped file and from its
own connects to the agent's handshake port (spec/errors.md, "Health"). A DCS that
runs no frames still holds its socket (UNVERIFIED). A crashed DCS stays alive in its
crash-report window (LIVE, [first DLL](../results/2026-11-11-first-dll-and-spikes.md),
"Crash capture").

A connect to the port takes time to finish. Windows reports a refusal only after
about 2 seconds (PRIOR). While a connect is unfinished, the stalled heartbeat gives
no evidence whether DCS is hung or gone. A verdict guessed from it would flip between
two states and publish an event each time.

## Decision

The verdict has the states `healthy`, `hung`, `gone` and `disabled`. While no connect
of the current stall has finished and no session is attached, the verdict keeps its
last value. Operators act on the verdict.

- A stall begins when the sidecar reads an advanced heartbeat or finds a session
  attached, and lasts until the next such read.
- A connect finishes when it is refused, accepted or reaches the probe's timeout. A
  timeout counts as not refused. A try whose connect was never made is no result: the
  sidecar tries again a second later and logs repeated failures.
- The first matching row of spec/errors.md, "Health", decides. A file whose lifecycle
  state is `DISABLED`, with a connect that is not refused, is `disabled`. A `LOADING`
  state is judged by `loading_limit_s`, not `heartbeat_stale_s`.
- Otherwise a refused connect gives `gone`. A connect that finishes any other way, with
  a stale heartbeat, gives `hung`.
- The probe's timeout is 100 ms on Unix and 5 s on Windows, whatever
  `handshake_timeout_s` says.
- The sidecar's first verdict is `gone`, and `gone` at start publishes no event.
- `SIM_STATE_HUNG` is a lifecycle state. When the verdict enters `hung`, the sidecar
  publishes one lifecycle event with it. When the verdict leaves `hung`, it publishes
  one with the state the status then reports. A dead DCS goes from `healthy` to `gone`
  with no event.
- The rule is the same on every platform. Only the timeout differs
  (`crates/sidecar/src/sim/real.rs`).

Rejected:

- Guessing `hung` or `gone` from an unanswered probe: it publishes a pair of events
  that carry no evidence.
- Treating a try that never connected as "not refused": the verdict would claim
  `hung` with no connect behind it. The cost is a held verdict if threads run out,
  which the sidecar logs.

## Consequences

- The verdict can read `healthy` while the heartbeat's age is past
  `heartbeat_stale_s`, until a connect of the new stall finishes.
- The sidecar takes no action on a verdict. Starting and restarting DCS belong to the
  host (spec/errors.md, "Health").
- Free space and failed backups are not part of the verdict, so a full disk does not
  make a watchdog restart DCS.
- That a crashed DCS whose handshake port still accepts connects reads `hung` and not
  `gone` is UNVERIFIED.
