# ADR 0008: An activation owes its mission load across a sidecar crash

## Status

Accepted

## Context

`ActivateCampaign` suspends the active campaign, opens the target's store, and sends a
`MISSION_LOAD` to DCS once the agent is attached. The call acknowledges the activation
before DCS loads the mission. The store promises that an acknowledged activation
survives a crash ([persistence](../../spec/persistence.md), "Writes").

If the sidecar stops between the acknowledgment and the load, no existing record tells
a restarted sidecar whether the load was missed or DCS restarted for another reason.
Without that record, the new campaign would spawn into the previous mission.
A load that was sent must not be sent twice, because an effect is never repeated.

A crash between an activation and its mission load left DCS loading the new mission
once, with no group of the new campaign in the old mission and none of the old in DCS
(LIVE, DCS 2.9.31.29434 dedicated server under Wine 10.0,
[the persistence results](../results/2026-11-15-s18-persistence-live.md), item 7).

## Decision

The activation's own commit records the mission load it owes in the store's
`meta.world`, and the sidecar sends it, once, after any restart.

- The record holds the shell path as DCS opens it, the start written into the shell,
  and the command's epoch and sequence number once the load is sent. It is written
  before the call answers.
- The commit that folds in the load's result clears it. `APPLIED` clears it.
  `NOT_SENT` keeps it owed and the load is sent again with the same file. Any other
  outcome is logged and clears it. Suspend and finish also clear it.
- At start, an owed load with no command is sent once the agent is ready. One whose
  command is in `inflight` waits for its result and is never sent twice. One whose
  command is not in `inflight` is logged and cleared. A clean stop keeps an owed load.
- While a load is owed, and after its `APPLIED` until the store records a mission
  other than the one the load was sent in, no group of the campaign is spawned. The
  rest of the reconciler's work goes on.
- With no owed load, the sidecar sends no mission load.
- A reload reuses the same record and the same spawning hold.

Spec: [persistence](../../spec/persistence.md), "The store file", "Campaign
operations" and "Sidecar start and stop".

Rejected:

- Reading the journal at start to find a missed load. The journal is never read back.

## Consequences

- A missing key reads as no owed load, so the store schema has no column for it.
- A campaign never spawns into the mission that came before its own.
- A DCS restart is not a missed load. The sidecar does not reload a shell after a
  restart: the owed record is the only trigger.
