# Persistence

How the sidecar keeps a campaign across a sidecar restart, a DCS restart, a mission
reload and a campaign switch. The world model and the lifecycle are in spec/world.md;
the settings are the `[store]` table of spec/config.md.

## Files

Each campaign has a folder in `[store] dir`, named by its campaign ID in decimal.

| File | Holds |
|---|---|
| `<id>/store.sqlite` | The campaign's store: its record, the entity tables, the journal and the checkpoint log. SQLite in WAL mode. |
| `<id>/history.log` | The event history, append-only. |
| `<id>/backups/<n>/store.sqlite` | Backup `n`, a consistent copy of the store. `n` counts up from 1 and is never reused in the folder. |
| `<id>/backups/<n>/backup.txt` | One line: the time in milliseconds since the Unix epoch, the store's schema version, and the reason (`scheduled`, `manual` or `pre_migration`), separated by spaces. Written last: a numbered folder without it is a backup cut short, which is not listed and is removed at the next backup. |
| `<id>/backups/next` | The last backup number given, in decimal, so a number is not reused after its folder is pruned. |
| `<id>/backups/history.log` | One copy of the event history, shared by every backup. |

Nothing else of a campaign lives outside its folder, so copying, moving or deleting the
folder is copying, moving or deleting the campaign. The server-scoped store
(spec/world.md, "Events and the store") is not specified here.

With `[store] enabled = false` there are no files: the store is in memory, and the
author keeps state through capture and restore (spec/world.md).

## The store file

| Table | Columns | Rows |
|---|---|---|
| `meta` | `key TEXT PRIMARY KEY`, `value BLOB` | One per key below |
| `entity` | `id INTEGER`, `kind TEXT`, `body BLOB`; the key is `id` and `kind` | One per group or static (`kind` is `group`) and one per unit (`unit`). A static is its own unit, so its `EntityId` has both rows. |
| `extension` | `name TEXT PRIMARY KEY`, `body BLOB` | One per registered Lua extension, and one tombstone per removed one |
| `journal` | `seq INTEGER PRIMARY KEY`, `body BLOB` | One per journal entry, in order, from 1 |
| `checkpoint` | `seq INTEGER PRIMARY KEY`, `body BLOB` | One per checkpoint, in order, from 1 |

A removed extension keeps its `extension` row as a tombstone: `removed` is true, the
body holds no source, and `spec_rev` is the highest revision the extension had. A
registration of the name after a reopen gets a revision above it.

Every `body` and every `meta` value is one `lv` value (spec/transport.md, "Encodings"):
a table with string keys, except `schema`, which is a number. A `u64` (an `EntityId`,
a revision, an epoch, an object ID, a time in milliseconds) is written as its decimal
text, because an `lv` number is a double. A list is a table with keys 1 to n. A
nested Lua value (a group's or unit's extra keys, a captured ammunition table, an
option value) is written as its own `lv` encoding inside a string, so a row's nesting
stays fixed. A reader ignores keys it does not know, and a missing key
takes its default (nil, false, 0, the empty string or the empty table). So a field
added in the same schema version reads back from an older file.

`meta` keys:

| Key | Value |
|---|---|
| `schema` | The schema version, a number. 1 is this document. |
| `campaign_id` | The campaign ID. It must equal the folder's name. |
| `campaign` | The campaign record: ID, name, state (`active`, `suspended`, `finished`), theaters, the theater to load, clock, created time, the author's component schema versions, and the outcome of a finished campaign. Its `state` key keeps its layout in every schema version, so the state is read before a migration. The platform schema version is `schema`. |
| `world` | The next `EntityId`, the store revision, the command epoch, the loaded mission (agent boot, mission generation, theater), the desired airbase owners and warehouse stocks with their revisions, the sample rates the controller set, the agent's country-to-coalition map, the model time of the last samples, and the route events not yet published. |

An entity body carries every field of its row in spec/world.md: for a group or static
its spec, order and revisions, lifecycle state, authority, lost policy, error, and the
reconciler's per-generation bookkeeping (spawned, spawn unknown, order sent, destroy
failures); for a unit its spec, last observed state, death fact, missing or removed
mark, whether the agent reported that mark in an `OBJECT_GONE` (only a reported mark
outlives a mission generation, spec/world.md "Respawn gate"), and DCS object ID. A group's body records whether it is a `group` or a
`static`.

The journal holds the entries spec/world.md says are journalled, each with its `kind`
and its fields. The journal is append-only: no row is changed or removed.

## Writes

The sidecar writes the store in one SQLite transaction at a time. A transaction writes
every entity row that differs from the file, the extensions that changed, the journal
entries made since the last one, and `meta`. So the file always holds one state the
sidecar had, never part of one.

| When | Why |
|---|---|
| When a command, a result, a fact or a command epoch is journalled: before every command is sent, and when a result or a fact arrives | A transition and its command are on disk before the command reaches DCS (spec/world.md, "Lifecycle"). A result, a death, a removal or a revival read from DCS is on disk before anything else happens, so a sidecar that restarts never takes a result it read for `unknown`. A new loaded mission is not written alone: it goes to disk with the transitions it causes, so a restarted sidecar never sees the new mission without them. |
| Before an API call that changes the store answers | A change the API acknowledged survives a sidecar crash. |
| Every `checkpoint_s` | Positions, fuel, life and the rest of the observed state. A checkpoint row records the wall time, the store revision, the model time and mission generation of its samples, and the period each sampling tier achieved (below); the API shows the last one as `Campaign.last_checkpoint`. |
| At suspend and finish | The final state. |

Writes happen on the sidecar's driver thread, under the hub lock, before the command
or the answer they protect. A write that fails stops the sidecar through its panic path
(spec/errors.md, `UNAVAILABLE`): the sidecar never sends a command it could not
journal, and never answers a call whose change is not on disk.

SQLite runs with `journal_mode = WAL` and `synchronous = NORMAL`: a transaction
survives a crash of the sidecar process and of DCS. A power loss may lose the last
transactions, never corrupt the file.

What each failure loses:

| Data | Written | Lost if DCS crashes | Lost if the sidecar crashes |
|---|---|---|---|
| Specs, orders, components, extensions | before the command that carries them is sent, and before an API call that changed them answers | nothing | nothing |
| Results, deaths, removals, revivals | when they arrive | what DCS did in its last frames before the crash and had not yet written | nothing already read |
| Positions, fuel, life, ammunition, warehouse stock | every `checkpoint_s` | up to `checkpoint_s` | up to `checkpoint_s`; read again from the agent |
| Events | before they are published | nothing published | nothing published |
| Effects | journalled as sent | may or may not have applied; never sent again | never sent again |

### Achieved periods

The agent samples positions, vitals, ammunition and warehouse stock in tiers whose
period stretches when the frame budget is short (spec/sampling.md). A value read in a
starved tier can be much older than its target period. Each checkpoint row therefore
records, per tier, the period achieved at that moment, in seconds, next to the target.
A restore that uses a checkpointed value can tell how old it may be.

A checkpoint body holds `seq`, `at_ms` (wall time), `revision`, `model_time` and
`mission_gen` (the samples' model time and generation), and `achieved`: a list of
`{tier, target_s, achieved_s}`, where `tier` is `position`, `vitals`, `ammo`,
`warehouse` or `enumeration`, the field names of the API's `SampleRates`.
`Campaign.last_checkpoint` shows the last one as a `CheckpointInfo`: `taken` from
`at_ms`, `sim` from `model_time` and `mission_gen`, and `achieved` from the achieved
periods; the targets are not shown.

## Opening a store

1. Read `meta.schema`. A version newer than the sidecar's is refused: the sidecar does
   not open the campaign and reports the version it found.
2. An older version is migrated (below).
3. Load `meta`, the entities, the extensions and the journal into memory. A tombstone
   is not an extension: it sets the name's revision floor and the agent's pending
   removal, so `EXTENSION_REMOVE` is sent until the agent confirms it.

The spatial grid and the change list a watch resumes from are not stored. The grid is
rebuilt from the units' last positions. A watch that resumes from before the open gets
a resync (spec/errors.md).

`meta.world` records whether the store is open: true from an open, false after a
clean close (suspend, finish). An open that finds it true follows an unclean stop: the
store revision may have been handed out past the last write, so the open moves it
forward by 2^32 and writes that at once. A revision handed out before the stop is
never handed out again, and a watch that resumes from one gets a resync.

## Schema version and migration

The schema version is a number, one more at each change to this document's tables or
bodies that an older sidecar would read wrongly. A migration is a list of steps, one
per version. On open, a store at version N below the current version M:

1. Is backed up (below), always, whatever the backup schedule. Listing campaigns reads
   each store's record and version without opening it, so it migrates nothing.
2. Has steps N+1 to M applied in one transaction, and `meta.schema` set to M in it.
3. Is opened.

A step that fails rolls back the transaction: the store stays at version N, the backup
is kept, and the campaign is not opened. Migrating the author's components is the
author's step, run through the Extension service after the platform's steps; the
campaign record keeps each component schema's version for it.

## Backups

A backup is a consistent copy of the store, taken with SQLite's `VACUUM INTO` while the
store is open. The copy is flushed to disk before its `backup.txt` is written and
flushed. It is taken every `backup_interval_h` while the campaign is active
(reason `scheduled`), on request through the API (`manual`), and before every
migration (`pre_migration`). Suspend and finish take none: their final checkpoint is
in the store itself.

Retention: after each backup the sidecar keeps the newest `backup_keep` backups of the
campaign, whatever their reason, and deletes the older ones. A backup's number counts
up from 1 and is never reused in the folder, so a deleted number does not come back.

There is no restore call. To restore a backup: suspend the campaign; stop the sidecar;
delete the campaign's `store.sqlite-wal` and `store.sqlite-shm` if they are there (an
unclean stop leaves them, and SQLite would replay them onto the copied file); copy
`backups/<n>/store.sqlite` over the campaign's `store.sqlite`; start the sidecar; and
activate the campaign. The event history is not rolled back: `history.log` keeps the
events after the backup, which the restored store does not know.

The event history is not copied into each backup: `backups/history.log` is one copy,
extended at each backup by the bytes the history gained since the last one. So
`backup_keep` does not multiply the history. If the history was compacted since (its
first record changed), the copy is made again in full.

## Event history

`history.log` holds the campaign's events in sequence order, one record each:

| Offset | Type | Field |
|---|---|---|
| 0x00 | u32 | `len`, the length of `body` |
| 0x04 | u32 | `crc`, CRC-32 (IEEE) of the bytes from 0x08 to the end of `body` |
| 0x08 | u64 | `seq`, the event's sequence number |
| 0x10 | u64 | `wall_ms`, milliseconds since the Unix epoch |
| 0x18 | bytes[len] | `body`, the Protobuf encoding of the event as the Events service sends it |

Little-endian throughout. Sequence numbers grow by one from record to record. The
sidecar appends an event's record and flushes it before it publishes the event, so a
number a client has seen is on disk and is never given to another event. An append
that fails is cut back to the last whole record and stops the sidecar, as a failed
store write does.

On open the sidecar reads the file to its end, and cuts a last record that is short or
whose `crc` does not match: that is a write cut by a crash, and zeros may follow it. A
bad record anywhere else stops the open and is reported: a record is not last when a
whole record numbered past it follows anywhere, or when the bytes right after it start
a later-numbered record, whole or cut short.

`event_retention_days` (spec/config.md) above 0 compacts the history at open: the
records older than the retention are dropped by writing the rest to a new file and
renaming it over the old one. The newest record is always kept, so the sequence
numbers go on from it after a restart. The Events service reports the first sequence
number it still holds, and a request before it gets a gap record.

## Campaign operations

What each operation of the Campaign service does to the files:

| Operation | Files |
|---|---|
| Create | A new folder and store, with the campaign record (state `suspended`, or `active` when the call activates it) and an empty world. |
| Activate | A finished campaign is refused (`FAILED_PRECONDITION`) before anything is opened or migrated. The store is opened (and migrated). Then the active campaign, if any, is suspended, the store's state is set to `active`, and its mission is loaded. If the open fails, the active campaign stays active; if the suspend fails, no campaign is active. Exactly one campaign is active per DCS instance. |
| Suspend | The driver captures every live group (`CAPTURE`) into the store and tells the agent there is no active campaign (`CAMPAIGN` 0). Then a final checkpoint, the state set to `suspended`, and the store closed cleanly. DCS keeps running whatever it has, unmanaged. |
| Finish | As suspend, with state `finished` and the outcome record. A finished store is read-only: it is never activated or migrated again, only read. |
| Delete | The folder is removed. Never automatic. |

Every open of a store writes it (it marks the store open), so an open that is not
followed by a close counts as an unclean stop. A read-only open, for reading a finished
or suspended campaign through the World and Events services, is not built yet; the
sidecar opens no finished store.

### Restore

Activation and the return of DCS after a restart take the same path: the reconciler
reads the store and every entity it should hold in DCS is `materializing` once the new
mission generation is seen (spec/world.md, "Lifecycle"). Each group is spawned from
its last captured or checkpointed state, and its order is compiled for what remains of
it (`compile(intent, status)`), so a convoy continues to its destination and an order
is not issued twice in one generation.

A member the agent reported gone as `MISSING` or `REMOVED`, with no death fact, is
not re-created by a restore: its group is spawned without it, and the member keeps its
mark across the new generation (spec/world.md, "Respawn gate"). A late death event can
no longer arrive for it (UNVERIFIED: that the agent's memory of rows that left ends with
the mission; the fake sim models it so), so the mark stays until the controller removes the group. A
group whose members are all dead or held goes `lost`, never `dead`. The sidecar cannot
tell a member a script removed from one whose death event is late, so neither comes
back. A kill whose `OBJECT_GONE` was lost with DCS (a crash before the sidecar read it)
leaves no reported mark, and that member is spawned again.

### Restore burst

While the lifecycle state is `PAUSED` and more groups are waiting to materialize than
`materialize_per_frame`, the reconciler sends `BURST` on (spec/commands.md) and spawns
every waiting group in the next frame, without the per-frame limit. It sends `BURST`
off when no group is waiting or the state leaves `PAUSED`; the agent also turns it off
by itself when the state leaves `PAUSED`.

A destroy sent while paused is pending (spec/commands.md, "A destroy while paused is
pending"): the agent writes `OBJECT_GONE` `DESTROYED` at once and DCS lets the objects
go at the resume. A spawn under the same name in the same pause is a new group with new
object IDs. After the resume DCS holds exactly one group per entity: LIVE on the Wine
server, docs/results/2026-11-15-s15-world-commands.md ("Paused replace"). The
sidecar's restore path over it is checked on the fake sim only.
