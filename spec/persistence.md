# Persistence

How the sidecar keeps a campaign, and what outlives a campaign, across a sidecar
restart, a DCS restart, a mission reload and a campaign switch. The world model and the
lifecycle are in spec/world.md; the settings are the `[store]` table of spec/config.md.

## Files

`[store] dir` holds one folder per campaign, named by its campaign ID in decimal, the
server folder `server`, and the lock file `lock`.

| File | Holds |
|---|---|
| `lock` | Nothing. The sidecar holds an exclusive OS lock on it while it runs (below). |
| `<id>/store.sqlite` | The campaign's store: its record, the entity tables, the journal, the commands in flight, the idempotency keys of calls on it, and the last checkpoint. |
| `<id>/events.sqlite` | The campaign's event history. |
| `<id>/backups/<n>/store.sqlite` | Backup `n`, a consistent copy of the store. `n` counts up from 1 and is never reused in the folder. |
| `<id>/backups/<n>/backup.txt` | One line: the time in milliseconds since the Unix epoch, the store's schema version, the store revision, and the reason (`scheduled`, `manual` or `pre_migration`), separated by spaces. Written last: a numbered folder without it is a backup cut short, which is not listed and is removed at the next backup. |
| `<id>/backups/next` | The last backup number given, in decimal, so a number is not reused after its folder is pruned. |
| `<id>/backups/events.sqlite` | One copy of the event history, shared by every backup. |
| `server/store.sqlite` | The server store (below). |
| `server/events.sqlite` | The server event log: events recorded while no campaign is active. |
| `server/backups/...` | The server store's backups, laid out as a campaign's. One is taken with each scheduled backup of the active campaign (reason `scheduled`) and before a migration (`pre_migration`), with the same retention. Its revision in `backup.txt` is the command epoch. |

Every `.sqlite` file runs SQLite with `journal_mode = WAL` and `synchronous = NORMAL`: a
transaction survives a crash of the sidecar process and of DCS. A power loss may lose
the last transactions, never corrupt a file.

A campaign's folder holds all of the campaign's own state, so copying, moving or
deleting the folder is copying, moving or deleting the campaign. It does not hold what
the server derives for it: the command epoch and the player keys are in the server
store, so a campaign folder copied to another server meets other player keys there
(spec/world.md, "Players").

With `[store] enabled = false` there are no files: the stores and logs are in memory,
and the author keeps state through capture and restore (spec/world.md).

### The lock

At start the sidecar creates `lock` if it is missing and takes an exclusive OS lock on
it (`flock` on Linux and macOS, `LockFileEx` on Windows), without waiting. If another
process holds it, the sidecar refuses to start and says so. The lock ends with the
process, so a crash leaves no stale lock. Two sidecars never write one store dir.

## Encodings

In a store file (a campaign's or the server's), every `body` and every `meta` value is
one `lv` value (spec/transport.md, "Encodings"):
a table with string keys, except `schema`, which is a number. A `u64` (an `EntityId`,
a revision, an epoch, an object ID, a time in milliseconds) is written as its decimal
text, because an `lv` number is a double. A list is a table with keys 1 to n. A
nested Lua value (a group's or unit's extra keys, a captured ammunition table, an
option value) is written as its own `lv` encoding inside a string, so a row's nesting
stays fixed. A reader ignores keys it does not know, and a missing key
takes its default (nil, false, 0, the empty string or the empty table). So a field
added in the same schema version reads back from an older file.

The store encodes with a checked writer: a value nested deeper than the codec allows
or a table with more pairs than it allows is an error, and the change that carries it
is refused (`INVALID_FIELD` for an API call). The transport codec's writer, which
writes nil there, is never used for the store. The encoding of every row kind and of
an event body is pinned in `spec/persistence.golden`, which the tests check.

An events file's `meta` values are UTF-8 text. An event `body` is the Protobuf encoding
of the `Event` message of the package named in the events file's `meta`
(`nanoprobe.v1alpha1`), as the Events service sends it.

## The store file

| Table | Columns | Rows |
|---|---|---|
| `meta` | `key TEXT PRIMARY KEY`, `value BLOB` | One per key below |
| `entity` | `id INTEGER`, `kind TEXT`, `body BLOB`; the key is `id` and `kind` | One per group or static (`kind` is `group`) and one per unit (`unit`). A static is its own unit, so its `EntityId` has both rows. |
| `extension` | `name TEXT PRIMARY KEY`, `body BLOB` | One per registered Lua extension, and one tombstone per removed one |
| `journal` | `seq INTEGER PRIMARY KEY`, `at_ms INTEGER`, `kind TEXT`, `entity INTEGER`, `body BLOB` | One per journal entry, in order, from 1 |
| `inflight` | `epoch INTEGER`, `seq INTEGER`, `body BLOB`; the key is `epoch` and `seq` | One per command sent with no outcome yet |
| `idempotency` | `key TEXT PRIMARY KEY`, `at_ms INTEGER`, `request_hash BLOB`, `response BLOB` | One per changing call answered on this campaign |
| `checkpoint` | `seq INTEGER PRIMARY KEY`, `body BLOB` | The last checkpoint, one row |
| `template` | `theater TEXT PRIMARY KEY`, `body BLOB` | One per theater the campaign has its own template for: the `.miz` file's bytes (plan revision 35). A schema 1 file without the table gains it at open. |

A removed extension keeps its `extension` row as a tombstone: `removed` is true, the
body holds no source, and `spec_rev` is the highest revision the extension had. A
registration of the name after a reopen gets a revision above it.

`meta` keys:

| Key | Value |
|---|---|
| `schema` | The schema version, a number. 1 is this document. |
| `campaign_id` | The campaign ID. It must equal the folder's name. |
| `campaign` | The campaign record: ID, name, state (`active`, `suspended`, `finished`), theaters, the theater to load, clock, created time, the author's component schema versions, and the outcome of a finished campaign. Its `state` key keeps its layout in every schema version, so the state is read before a migration. The platform schema version is `schema`. |
| `world` | The next `EntityId`, the store revision, whether the store is open, the loaded mission (agent boot, mission generation, theater), the last agent record folded in (agent boot and record number) with the stamp it was read under (agent boot, mission generation, campaign), which records delivered again before their next `FRAME` keep, the desired airbase owners and warehouse stocks with their revisions, the sample rates the controller set, the mission load an activation or a reload owes (the shell as DCS opens it, the start written into it, and the command's epoch and sequence number once sent; absent when none is owed), the start written into the running mission's shell, the model time of that generation's first sample with its agent boot and generation, the shell slot DCS may hold (the last load applied, or one sent whose outcome is unknown, left in flight, or dropped by a suspend), the next mission's settings (`SetNextMission`), the agent's country-to-coalition map, the model time of the last samples, and the route events not yet published. |

An entity body carries every field of its row in spec/world.md: for a group or static
its spec, order and revisions, lifecycle state, authority, lost policy, error, and the
reconciler's per-generation bookkeeping (spawned, spawn unknown, order sent, destroy
failures); for a unit its spec, last observed state, death fact, missing or removed
mark, whether the agent reported that mark in an `OBJECT_GONE` (only a reported mark
outlives a mission generation, spec/world.md "Respawn gate"), and DCS object ID. A
group's body records whether it is a `group` or a `static`.

The journal holds the entries spec/world.md says are journalled, each with its `kind`
(`command`, `outcome`, `death`, and so on), the entity it names (0 for none), the wall
time it was made and its fields. The journal is an audit trail: the sidecar appends to
it and never reads it back. An `inflight` row is inserted with the `command` entry that
sends it and deleted with the entry that gives its outcome, in the same transaction; it
is what a restarted reconciler reads to learn which commands it is owed a result for.

The checkpoint row holds `seq`, `at_ms` (wall time), `revision`, `model_time` and
`mission_gen` (the samples' model time and generation), and `achieved` (below). A new
checkpoint replaces it, with `seq` one more.

## Writes

The sidecar writes the store in one SQLite transaction at a time, so the file always
holds one state the sidecar had, never part of one.

**Group commit.** The driver commits at most once per step. During a step the
reconciler folds in what it read and decides its commands; the commands are written to
the inbound ring but held back from the agent. At the end of the step the sidecar
commits the events file, then the store, then lets the held commands through and
acknowledges the records it read. An API call that changes a store commits the same
way before it answers. So a command is on disk before it reaches DCS, a result or fact
read from DCS is on disk before the agent learns it was read, and a change the API
acknowledged survives a sidecar crash. A loaded mission goes to disk with the
transitions it causes.

A commit writes:

| Written | When |
|---|---|
| The journal entries made since the last commit, and the `inflight` rows they insert or delete | Every commit |
| Every group, unit or extension row whose spec, order, lifecycle, authority, policy, error, death, mark or object ID changed | Every commit |
| `meta.world` and `meta.campaign` when they changed | Every commit |
| The idempotency rows of calls answered | Every commit |
| Positions, fuel, life, ammunition, warehouse stock: the sampled state | Only at a checkpoint |

A checkpoint is a commit that also writes every row whose sampled state changed, and
replaces the checkpoint row. It is taken every `checkpoint_s` while the campaign is
active, skipped when the store revision did not move since the last one, and taken at
suspend and finish and at a clean stop. `Campaign.last_checkpoint` shows the last one.

Writes happen on the driver thread, under the hub lock. A write that fails stops the
sidecar through its panic path (spec/errors.md, `UNAVAILABLE`): the sidecar never lets a
command through it could not journal, and never answers a call whose change is not on
disk.

What each failure loses:

| Data | Written | Lost if DCS crashes | Lost if the sidecar crashes |
|---|---|---|---|
| Specs, orders, components, extensions | in the step or call that changes them, before any command that carries them reaches DCS and before the call answers | nothing | nothing |
| Results, deaths, removals, revivals | at the end of the step that reads them, before the agent learns they were read | what DCS did in its last frames before the crash and had not yet written | nothing: unacknowledged records are read again |
| Positions, fuel, life, ammunition, warehouse stock | every `checkpoint_s` | up to `checkpoint_s` | up to `checkpoint_s`; read again from the agent |
| Events | at the end of the step that makes them, before they are published | nothing published | nothing published |
| Effects | journalled as sent | may or may not have applied; never sent again | never sent again |
| Owed mission load | in the activation's or the reload's commit (with a checkpoint for a reload); cleared in the commit that journals its outcome | nothing | sent again at start if it never reached DCS |

### A crash between the two commits

The events file commits before the store. Each event made from an agent record carries
that record's identity (agent boot and record number) in its row. The store keeps the
last agent record it folded in (`meta.world`); the sidecar acknowledges records to the
agent only after both commits, and after a restart the agent's link delivers again
every record it was not told was read.

- A crash before the events commit loses nothing: both commits are missing, and every
  record of the step is read again.
- A crash between the commits leaves the events of the step in the events file and its
  facts out of the store. The records are read again: their facts are folded in, and an
  event whose record identity is already in the events file is not appended again.
- A crash after the store commit, before the acknowledgement: the records are read
  again, and a record at or below the store's last folded record is dropped.

So each fact is folded in once and each event is recorded once, whatever the crash
point.

### Achieved periods

The agent samples positions, vitals, ammunition and warehouse stock in tiers whose
period stretches when the frame budget is short (spec/sampling.md). A value read in a
starved tier can be much older than its target period. The checkpoint therefore
records, per tier, the period achieved at that moment, in seconds, next to the target.
A restore that uses a checkpointed value can tell how old it may be.

`achieved` is a list of `{tier, target_s, achieved_s}`, where `tier` is `position`,
`vitals`, `ammo`, `warehouse` or `enumeration`, the field names of the API's
`SampleRates`. `Campaign.last_checkpoint` shows the checkpoint as a `CheckpointInfo`:
`taken` from `at_ms`, `sim` from `model_time` and `mission_gen`, and `achieved` from
the achieved periods; the targets are not shown.

## Opening a store

1. Read `meta.schema`. A version newer than the sidecar's is refused: the sidecar does
   not open the campaign and reports the version it found.
2. An older version is migrated (below).
3. Load `meta`, the entities, the extensions, the `inflight` rows and the checkpoint row
   into memory. The journal is not read. A tombstone is not an extension: it sets the
   name's revision floor and the agent's pending removal, so `EXTENSION_REMOVE` is sent
   until the agent confirms it.
4. Copy the idempotency rows younger than `idempotency_keep_h` into the server store's
   key index (below).

The spatial grid and the change list a watch resumes from are not stored. The grid is
rebuilt from the units' last positions. A watch that resumes from before the open gets
a resync (spec/errors.md).

`meta.world` records whether the store is open: true from an open, false after a
clean close (suspend, finish, a clean stop). An open that finds it true follows an
unclean stop: the store revision may have been handed out past the last write, so the
open moves it forward by 2^32 and writes that at once. A revision handed out before the
stop is never handed out again, and a watch that resumes from one gets a resync.

A new store file is made under a temporary name, filled in one transaction (tables,
`schema`, `campaign_id`, the record and an empty world), flushed and renamed into
place. A crash leaves either no file or a whole one.

## Schema version and migration

The schema version is a number, one more at each change to this document's tables or
bodies that an older sidecar would read wrongly. A migration is a list of steps, one
per version. Each step keeps its own decoder for the version it reads, frozen when the
step is written: a later change to the current decoders never changes what an old step
reads. On open, a store at version N below the current version M:

1. Is backed up (below) with reason `pre_migration`, whatever the backup schedule,
   unless the newest complete backup already has schema N and the store's current
   revision. Listing campaigns reads each store's record and version without opening
   it, so it migrates nothing.
2. Has steps N+1 to M applied in one transaction, and `meta.schema` set to M in it.
3. Is opened.

A step that fails rolls back the transaction: the store stays at version N, the backup
is kept, and the campaign is not opened. Migrating the author's components is the
author's step, run through the Extension service after the platform's steps; the
campaign record keeps each component schema's version for it.

## Backups

A backup is a consistent copy of the store, taken with SQLite's `VACUUM INTO` through a
connection of its own, on a worker thread: the driver starts it and goes on stepping,
and collects the result in a later step. `BackupCampaign` answers when the worker is
done. The copy is flushed to disk before its `backup.txt` is written and flushed. A
backup is taken every `backup_interval_h` while the campaign is active (reason
`scheduled`), on request through the API (`manual`), and before a migration
(`pre_migration`). Suspend and finish take none: their final checkpoint is in the store
itself. One backup of a campaign runs at a time; a request while one runs waits for it.

Retention: after each backup the sidecar keeps the newest `backup_keep` scheduled and
manual backups of the campaign and deletes the older ones. It keeps, in addition, the
newest `pre_migration` backup of each schema version, outside `backup_keep`; older
`pre_migration` backups of the same version are deleted. A backup's number counts up
from 1 and is never reused in the folder, so a deleted number does not come back.

There is no restore call; a backup is restored offline (see "Restore a backup").

The event history is not copied into each backup: `backups/events.sqlite` is one copy,
extended at each backup with the events it lacks (`INSERT ... WHERE seq > ` its newest
sequence number). Events the retention deleted from the history since are deleted from
the copy too, so `backup_keep` does not multiply the history.

## Event history

`events.sqlite` holds a campaign's events:

| Table | Columns | Rows |
|---|---|---|
| `meta` | `key TEXT PRIMARY KEY`, `value BLOB` | `format` (1, a number), `campaign_id` (decimal text; 0 for the server log), `package` (the Protobuf package of the bodies, `nanoprobe.v1alpha1`) |
| `event` | `seq INTEGER PRIMARY KEY`, `wall_ms INTEGER`, `source TEXT`, `body BLOB` | One per event |

`source` names what the event was made from, so an event made again after a crash is
not logged twice. It is unique where it is not null:

- An event made from an agent record: `<agent boot>:<record number>` for the record's
  first event, and `<agent boot>:<record number>:<i>` for its event at index `i`
  (from 1) when one record makes several. A record's events are logged in one
  transaction, so the first one's key stands for all of them.
- A route event (spec/world.md), which the sidecar makes again from samples read
  again: `route:<group EntityId>:<order revision>:<waypoint index or end>`.
- Any other event the sidecar makes: null.

Sequence numbers grow by one from row to row. The driver commits the events of a step
in one transaction before it publishes them (above), so a number a client has seen is
on disk and is never given to another event. The Events service reads rows by `seq`.

`event_retention_days` above 0 deletes the events older than the retention, at open
and once an hour: the cutoff is the earlier of now and the newest event's `wall_ms`,
less the retention. The newest event is always kept, so the sequence numbers go on from
it after a restart. When the clock is behind the newest event's `wall_ms` the deletion
is skipped and a warning logged: a clock set back must not delete the history. A
finished campaign's history is not deleted. The Events service serves the log from the
first sequence number it still holds; a request to read on from before it is refused
with `SEQUENCE_EXPIRED` (spec/errors.md).

## The server store

`server/store.sqlite` holds what outlives a campaign. It is built as a campaign store:
`meta.schema`, migration, backups and the same `lv` bodies.

| Table | Columns | Rows |
|---|---|---|
| `meta` | `key TEXT PRIMARY KEY`, `value BLOB` | `schema`; `epoch`, the command epoch (spec/commands.md); `open`, as a campaign store's |
| `player_secret` | `id INTEGER PRIMARY KEY`, `secret BLOB` | One row: the secret player keys are derived with (spec/world.md, "Players") |
| `player` | `key TEXT PRIMARY KEY`, `ucid TEXT`, `body BLOB` | One per player key the server has given |
| `idempotency` | `key TEXT PRIMARY KEY`, `at_ms INTEGER`, `campaign INTEGER`, `request_hash BLOB`, `response BLOB` | One per changing call answered: for a call on no campaign, the whole answer; for a call on a campaign, the index (`campaign` set, `request_hash` and `response` null) |
| `extension` | `name TEXT PRIMARY KEY`, `body BLOB` | Server-scoped extension data, by name |
| `deleted_campaign` | `id INTEGER PRIMARY KEY`, `at_ms INTEGER` | One per campaign deleted through the API, so its tag is never given again (spec/world.md, "Names") |

The command epoch is written before any command of the new epoch reaches DCS. It
grows across a campaign switch and a sidecar restart.

The server event log is `server/events.sqlite`, laid out as a campaign's, with
`campaign_id` 0 and its own sequence numbers. `server_log_days` is its retention, by
the rule above.

A player's key is the same in every campaign of the server. An existing
`players.json` in `[store] dir` (written before the server store) is read once into the
server store and renamed `players.json.migrated`.

The `server` folder is made with mode 0700 on Linux and macOS, and set to it at every
open: it holds the player secret, the key-to-ucid pairs and their backups.
`players.json.migrated` is set to mode 0600 before it is renamed and is kept as the
record of the migration. Nothing in the `server` folder, `players.json` or
`players.json.migrated` is ever put in a diagnostics bundle or a campaign backup.

## Idempotency keys

A changing call's key (spec/errors.md, "Idempotency") is written with its change:

- A call that acted on a campaign writes its `idempotency` row in that campaign's
  store, in the same transaction as the change; a call that suspends or finishes the
  campaign writes it in the closing transaction. The server store's index row (`key`,
  `at_ms`, `campaign`) is written before that transaction. An index row whose
  campaign store never got the key finds nothing, and the repeat runs as a new call.
- A call on no campaign writes its whole row in the server store.

A repeat looks in the index, then in that campaign's store; with no index row, in the
store of the campaign the call names, or the active one. A finished campaign's store
is read for it without being opened. When a campaign's store is opened, its rows
younger than `idempotency_keep_h` are copied into the index. A stored key older than
`idempotency_keep_h` is forgotten: the repeat runs as a new call. Rows older than
`idempotency_keep_h` are deleted from every open store once an hour.

## Campaign operations

What each operation of the Campaign service does to the files:

| Operation | Files |
|---|---|
| Create | A new folder with its store (the campaign record, state `suspended`, or `active` when the call activates it, and an empty world) and its events file. A call that activates a campaign whose theater has no template is refused before the folder is made. |
| Activate | A finished campaign is refused (`FAILED_PRECONDITION`) before anything is opened or migrated. The campaign's shell is written first (see "Mission shells"); a theater with no template, a template that does not parse, a shell that does not render, or an airport the template lacks is refused before anything is created, suspended, opened or sent: the active campaign stays active. A shell that cannot be saved is refused after the active campaign's groups are captured; nothing is suspended, opened or sent. The active campaign, if any, is suspended first. Then the target's store is opened (and migrated), its state set to `active`, and its mission loaded: the load is owed in `meta.world` from the activation's commit, before the call answers, and the `MISSION_LOAD` is sent once the agent is attached and knows the campaign. The commit that folds in its result clears it: `APPLIED` clears it; `NOT_SENT` keeps it owed and it is sent again with the same file; any other outcome is logged and clears it. If the open fails, no campaign is active. Exactly one campaign is active per DCS instance. |
| Reload | `Reload` on the active campaign. Refused before the capture: no active campaign, an owed load, a theater with no template, a template that does not parse, a shell that does not render, and an airport the template lacks. A shell that cannot be saved is refused after the capture: the groups are captured, and nothing is suspended, opened or sent. So is a `SetNextMission` or `SetCampaignTemplate` that lands during the capture and makes the shell fail. The driver captures every group DCS holds, as Suspend does; then writes the shell for the next mission's theater (the loaded one when it names none; the campaign's own theater when the loaded one is not one of the campaign's, as after a DCS restart into a foreign mission), and commits a checkpoint and the owed load together. The call answers `PENDING`; the load then runs as an activation's does, with the same spawning hold. A theater change is a reload whose shell is on another theater: entities on a theater that is not loaded stay `virtual` and return when it is loaded again. |
| Suspend | The driver captures every group and static DCS holds (`CAPTURE`) and waits up to `result_wait_s` for the results, which the store takes as they arrive. Then it tells the agent there is no active campaign (`CAMPAIGN` 0), writes the campaign clock into the record, takes a final checkpoint, sets the state to `suspended`, clears an owed mission load, and closes the store cleanly. DCS keeps running whatever it has, unmanaged. A repeat of the call under its answered key captures nothing. |
| Finish | As suspend, with state `finished` and the outcome record. A finished store is read-only: it is never activated or migrated again, only read. |
| Set a template | `SetCampaignTemplate` writes or removes a row of the `template` table. Refused: a theater that is not one of the campaign's (`INVALID_FIELD`), a file over 16 MiB, one whose `theatre` member does not name the theater exactly, one whose `mission` or `warehouses` does not parse with the fields the shell writer changes, one that places a group, unit or static whose name reads as a platform name (spec/world.md, "Names"), and, under "Free space", one the file system has no room for. |
| Backup | A backup with reason `manual` (`BackupCampaign`), refused under "Free space". `ListCampaignBackups` reads the folder without opening the store. |
| Export | `ExportCampaign` streams the campaign's store and event history as JSON lines (see "Export"). It reads the files and makes no copy. |
| Delete | `DeleteCampaign` (admin), with the campaign's name to confirm, on a campaign that is not active and has no backup running: its store is closed and its folder and shells removed. Never automatic. |

A switch of the active campaign (`ActivateCampaign`, or `CreateCampaign` that
activates) captures and suspends the active one as Suspend does.

### Sidecar start and stop

At start the sidecar takes the lock, opens the server store, and lists the folders. It removes the read copies under each campaign's `.read/` and the temporary files in `[missions] dir`'s `shells/` that a stopped sidecar left. It
reads each campaign's record and checkpoint without opening its store, and opens its
events file, applying the retention. A folder whose store or events file cannot be read
is skipped; its ID is never given to a new campaign. A campaign whose record is
`active` was active when the sidecar stopped: its store is opened and it is the active
campaign again. If its `meta.world` owes a mission load that was never sent (no command
named), it is sent once the agent is ready. If the owed load names its command and that
command is in `inflight`, the sidecar waits for its result and never sends it twice. If
the command is not in `inflight`, the load is logged and cleared. A clean stop keeps an
owed load. With no owed load there is no mission load. While a load
is owed, and after its `APPLIED` until the store records a mission other than the one
the load was sent in, no group of the campaign is spawned (the rest of the reconciler's work goes on), so a campaign never spawns into the mission before
its own. More than one such campaign stops the start.

A store is opened when a call first names its campaign, to read or to change it, and
when it is activated. It stays open until the campaign is suspended or finished. Every
open of a store writes it (it marks the store open), so an open that is not followed by
a close counts as an unclean stop. A clean stop of the sidecar closes every open store;
the active one takes a checkpoint first and keeps its state `active`. A stop after a
panic closes nothing. A read opens no store (see "Reading a campaign that is not open").

### Mission shells

A campaign's mission is a shell the sidecar writes from a template (plan 4.4): the
campaign's own template for the theater (the `template` table), else the shared one,
`<theater>.miz` in `[missions] dir` (spec/config.md). The shell is a copy of the
template with only these values changed, every other byte of every member kept:

| Member | Value | From |
|---|---|---|
| `mission` | `date` (`Day`, `Month`, `Year`) and `start_time` | `NextMission.start` when set, else the campaign clock, else the template's own |
| `mission` | `weather` | `NextMission.weather` when set, else the template's |
| `warehouses` | `airports[<id>].dynamicSpawn`, `allowHotStart` | `NextMission.dynamic_spawn`, by the airport ID in the template's `warehouses`; airports not named keep the template's flags. An ID the template does not hold is `INVALID_FIELD` |

Airbase owners and warehouse stocks are not written into the file: the reconciler sets
them after the new mission starts, as after any generation change. Units placed in a
template stay unmanaged (spec/world.md, "Managed and unmanaged").

Each campaign has two shell slots, `shells/<campaign ID>.a.miz` and `.b.miz`. A shell
goes to the slot the owed load, else the last applied load, did not name, through a
temporary file that is flushed and renamed, so a file DCS may still hold is never
rewritten. The owed load names its slot, and a resend after `NOT_SENT` loads the file
already written.

`NextMission` is kept in `meta.world` until `SetNextMission` replaces it whole. It
applies to a shell for its theater, or for any theater when it names none; a shell for
another theater is written with the template's weather and flags and the campaign clock.
Its `start` is used once: the commit that owes the load clears it, and the clock takes it
when the load is applied (below); a load refused or rejected leaves the clock where it
was. The campaign clock is the start written into the running mission's shell
plus the model time since that generation's first sample; with no mission of its own
running it is the clock in the campaign record, which suspend, finish, checkpoints and
every shell write update. A load's start takes over at its `APPLIED`, counted from the
next generation's first sample; a load refused or rejected keeps the running mission's
start, or with none the record's clock. A DCS restart runs a mission the campaign did not load: the clock reached before
it is written to the record, and the clock stays there until the campaign's next load.
A load whose outcome is `UNKNOWN`, or that left the commands in flight, and one a suspend
dropped after it was sent, count as holding their slot. Dates and times are the mission's own calendar time, carried
in Timestamp fields with no time-zone conversion.

### Reading a campaign that is not open

A read of a campaign whose store is not open, finished or suspended, opens no store: the
sidecar takes a consistent copy of it with `VACUUM INTO` under `.read/` in the
campaign's folder, migrates the copy if it is older, opens it, and serves the World
lists, the area query and the Lua extension list from it. The other reads (the campaign
record, events, occupancy, export) read the files and make no copy. The copy is dropped once no read named the campaign for 60 s, or when the
campaign's own store opens. A copy is refused under "Free space". Watches stay on the
active campaign.

### Export

`ExportCampaign` streams JSON lines, each a whole object with a `kind`: a `header` line
(export format 1, the campaign ID, the store's schema version, and the Protobuf package
of the events), then `meta` (each key, its value decoded), `entity` (each row, kind and
body decoded), `extension`, `journal`, `inflight`, `template` (theater, size and SHA-256
of the file) and `checkpoint` rows, then `event`
rows with the event's Protobuf encoding in base 64 (`event_b64`). A stored body is
decoded to JSON by the rule of spec/world.md, "Lua tables in the API"; a string that is
not UTF-8 is `{"bytes_b64": ...}`. The store is read in one read transaction on a
connection of its own, so the lines come from one snapshot while the driver goes on
writing; the events run up to the newest at the export's start. It works on a campaign
in any state.

### Free space

The status reports the free bytes on the file system that holds `[store] dir`. A
campaign holds its store, the store's WAL and its events file there. A backup, a read
copy, and `SetCampaignTemplate` (with the template's size added) are refused with
`SPACE_LOW` when the free bytes are fewer than that; a scheduled backup is then skipped,
logged, and tried one interval later. A failed store write still stops the sidecar.

### Restore a backup

`nanoprobe restore --store <dir> --campaign <id> --backup <n>`, with the sidecar
stopped, restores backup `n` over a campaign's store. It takes the store dir's lock, so
it refuses while a sidecar runs. It copies the current store, with its WAL applied, to
`restore-<unix ms>/store.sqlite` in the campaign's folder before it removes anything;
removes the store's WAL files; copies the backup in under a temporary name and renames
it over the store. A crash after the WAL files go and before the rename leaves the store
without its last commits; the copy aside holds them. Then
the restored store's next `EntityId` is the larger of the two copies', and its revision
the larger of the two plus 2^32, so no ID or revision handed out before is given again;
it is marked closed, its record is `suspended` unless the backup is `finished`, and its
commands in flight, its owed mission load and a start set for the next shell are
cleared; its shell slot is the replaced store's, since DCS may hold that file. The event
history is not rolled back: `events.sqlite` keeps the events after the backup.

### Timers

While a campaign is active, the driver takes a checkpoint every `checkpoint_s` and
starts a backup (`scheduled`) `backup_interval_h` after the newest backup kept, or
after the activation when there is none. A checkpoint that fails stops the sidecar, as
a failed store write does. A backup that fails is logged and taken again one interval
later.

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
sidecar's restore path over it is LIVE on the same server,
docs/results/2026-11-15-s18-persistence-live.md (item 2).
