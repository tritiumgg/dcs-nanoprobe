# ADR 0038: The sidecar's growth is bounded by purge, expiry and retention

## Status

Accepted

## Context

A campaign runs for weeks. A sidecar that keeps every removed entity, every journal
entry and every event grows in memory, in the rows the driver walks each step, and on
disk.

A soak of a live campaign on a DCS server under Wine showed the growth: entity rows,
journal rows, store file size and sidecar memory all rose while about 140 removals an
hour went on (LIVE, [sidecar bound](../results/2026-11-18-sidecar-bound.md)). On the
host, each removed group of four units held about 7 KB of heap, and the reconciler's
planning pass cost per step rose with the count of removed groups.

A purge is safe only when DCS holds nothing of the entity. A late death can name a
unit after its group is gone (spec/world.md, "Kill attribution").

## Decision

Every kind of record the sidecar keeps has a time limit, the journal and the event
history also have a size limit, and the same limits apply when the store is in memory.

| Kept | Bound | Default |
|---|---|---|
| A removed entity's rows | `removed_keep_h`, once DCS holds nothing of it | 1 hour |
| A dead group | `dead_keep_h`, then it is removed and purged; 0 never | 24 hours |
| The journal | `journal_retention_days` and `journal_max_mb` | 30 days, 1024 MB |
| The event history | `event_retention_days` and `events_max_mb` | 90 days, 1024 MB |
| The server event log | `server_log_days` | 30 days |
| An idempotency key | `idempotency_keep_h` | 24 hours |

- The purge takes a removed entity once DCS holds nothing of it: no bound object and
  no unanswered command naming it. For a dead static it also needs its ruin marks
  cleared and the applied `STATIC_DESTROY`; for anything else, a full enumeration
  since the removal (spec/world.md, "Removal"). An `EntityId` is never given again.
- After a purge every call that names the ID answers `NOT_FOUND`. A late death for a
  purged unit is dropped and counted. A keyed repeat within `idempotency_keep_h` still
  gets its first answer. An object in DCS whose platform name carries this campaign's
  tag and names an entity below the store's next `EntityId` that the store does not
  hold is destroyed as drift.
- A dead group expires without a destroy or a `CLEAR_DEBRIS`: wrecks are the
  controller's. A group with a held member is `lost`, not `dead`, and does not
  expire. Statics never expire.
- Either limit of a log deletes. The delete takes the oldest rows in batches
  of at most 1,000, one per driver step, always keeps the newest row, and skips the
  time limit when the clock is behind the newest row
  (spec/persistence.md, "Journal retention", "Event history").
- The reconciler's plan walks a work set of the groups that need work, and the
  checkpoint compares only the groups marked since the last one.
  `GetServerStatus.store_size` reports what the sidecar holds.
- A finished campaign's journal and history are never deleted.

Rejected:

- No limit by default: the journal is an audit trail, but a default that keeps it for
  ever lets the sidecar grow without bound.
- A separate journal file: it splits one commit across two WAL files.
- `auto_vacuum`: freed pages are reused, so the file levels off.

## Consequences

- Each backup holds the whole journal, so the backups take up to `backup_keep` times
  `journal_max_mb`. A lower retention or fewer backups bound the disk further.
- An author who wants the whole history sets both its days key and its size key to 0.
- Ruins a controller makes and never removes are kept, by design. Their count is in
  the status.
- The sizes count the rows' bodies. The files take more (spec/config.md).
- Host churn tests show plateaus in rows, memory and file size. Whether a live
  sidecar's resident memory levels off is UNVERIFIED beyond the runs in the results
  document. The agent's own per-generation sets are not bounded here.
