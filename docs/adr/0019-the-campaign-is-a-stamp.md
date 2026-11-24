# ADR 0019: The campaign is a stamp; records of another campaign do not count

## Status

Accepted

## Context

A server can run more than one campaign over time. The agent has one active campaign.
A sidecar of another campaign can attach and switch the agent to its own, and the first
sidecar switches it back when it next attaches. The mission generation does not change
on a switch, because DCS does not load a mission.

The agent queues records in its own memory while no sidecar is attached, or while the
sidecar has not read them. A death queued while campaign A was active can reach a
sidecar of campaign B. The record carries the DCS object ID, and an entity binding for
it is the binding of A. The generation rule passes it, because the generation is the
same. B would count it as a death fact of its own unit.

The read position of the outbound ring lives in the mapped file, not in the store, and
a sidecar can run with persistence off. A fix has to work in those cases.

## Decision

The campaign is a stamp on every record, and the sidecar counts a record's entity
fields only when its stamp names the campaign of the store.

- Every `FRAME` carries the campaign. A record has the stamps of its `FRAME`: the
  mission generation, the campaign, the lifecycle state, the frame number, and model and
  real time (spec/records.md, "Stamps").
- The agent writes a new `FRAME` before a record whose stamps differ from the last
  `FRAME`, even in the middle of a frame. A `CAMPAIGN` command that changes the campaign
  therefore puts a `FRAME` with the new campaign before the next record. Records made
  before it keep the old campaign.
- A queued record is written after a `FRAME` that repeats the stamps it was made with,
  the campaign included, with `replay` set when it was made in an earlier frame.
- The sidecar keeps the agent boot, the generation and the campaign of every `FRAME`,
  replayed ones too. A replayed `FRAME` with a stamp the sidecar already applied is
  applied again.
- An `OBJECT` binding, an `OBJECT_GONE` death fact and an `EVENT` object ID count only
  when the campaign stamp equals the store's. Otherwise the entity fields count as 0 and
  the sidecar counts the record as dropped (`other_campaign`).
- Campaign 0 never resolves.
- An enumeration that holds a record of another campaign is not used.

Rejected:

- A campaign field on `OBJECT_GONE`. It repeats the stamp and covers one record kind.
- `EntityId` values unique across campaigns. It changes the public ID rule.
- The agent clearing its queue on a switch. The `CAMPAIGN` command arrives after the
  queue is flushed, and the clear would lose the facts of campaign A.

## Consequences

- The rule needs no wire change and works with persistence off and with a sidecar that
  restarts mid-replay.
- A record made before a switch and read after it is dropped and counted. B does not
  take a death of campaign A as its own.
- A queued record repeats its stamps, so a replayed queue needs a `FRAME` wherever any
  stamp changes, not only the frame number.
- The agent boot is not a stamp. The queue ends with the DCS process, so every record in
  one ring has the boot of the status block.
- The sidecar tests cover A's queued death read by B with persistence off and with the
  agent detached, and the same with a restart during replay
  (docs/results/2026-11-13-s12-sidecar-core.md).
