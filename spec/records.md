# Outbound records

What the agent writes to the outbound ring. The ring, the record header and the
encodings `str`, `lv` and `tlv` are in spec/transport.md. Offsets below are from the
start of the payload.

DCS call names and what they return are PRIOR in this document unless tagged; steps
S9, S10 and S15 check each against a running DCS.

## Stamps

The first record of every frame's batch is `FRAME`. Every record up to the next `FRAME`
carries its stamps: the mission generation, the frame sequence number and model time.

Every record the agent writes in a DCS process has a number, counted from 1 through
`FRAME` and padding records too. `FRAME` carries the number of the record after it.
With the agent's boot the number names a record for good, which is how a sidecar drops
what it reads twice (spec/transport.md).

**Wall-clock time.** The agent stamps DCS real time, not wall time. The sidecar keeps,
per agent boot, the difference between its own wall clock and the `real_time` of the
`FRAME` records it reads with `replay` 0, and gives every record the wall time of its
`FRAME`'s `real_time` plus that difference. A replayed record therefore gets the time
it was made, not the time it arrived. The difference is measured once the sidecar
attaches, so for records made before that the wall time is an estimate, good to the
drift of the two clocks.

A record made in a callback outside a frame (`onMissionLoadBegin`, `onSimulationStart`,
`onSimulationStop`) carries the frame number of the last frame and the real and model
time of the callback.

A record the agent held in its queue is written after a `FRAME` that repeats the stamps
of the frame it was recorded in, with `replay` set. After the replayed records the
agent writes a `FRAME` for the current frame before any current record. The sidecar
takes the current lifecycle state and campaign only from a `FRAME` with `replay` 0.

## Delivery classes

| Class | When the outbound ring is full or no sidecar is attached |
|---|---|
| sample | Dropped and counted in `samples_dropped` when the ring is full. A detached agent takes no samples (spec/transport.md, "Detaching"). |
| event | Held in the agent's event queue, a fixed number of bytes (spec/config.md). When the queue is full the agent drops the oldest log records first, then the oldest other records, and records the loss in a `GAP` record. |
| control | Held in the agent's control queue, never dropped while there is room. While the queue is full the agent sets `COMMANDS_HELD` and reads no commands, so it produces no more results. If a control record still cannot be queued, the agent drops it and sets `CONTROL_LOST`. |

The agent keeps one order across both queues: it writes queued records in the order
they were made, whichever queue held them. A `RESULT` is therefore never written ahead
of an older `OBJECT`, nor a `LIFECYCLE` ahead of an older event. The two queues only
bound the two classes separately.

The agent holds at most one unwritten `GAP` per class. A further loss of that class
extends it: the count grows and its end moves. Its span runs from the first record
dropped to the last, and may hold records that were kept. A long time without a
sidecar therefore costs two control records, not one per loss.

A `GAP` of events means the sidecar's view of the world may be wrong. The sidecar forces
a full enumeration, and API consumers see the gap as an explicit record.

On `CONTROL_LOST` the agent stops reading commands, and once its control queue is
empty writes a new `HELLO`, clears the flag and reads commands again. The sidecar
resyncs at that `HELLO`: it takes the lifecycle state, mission generation and campaign
from the `FRAME` before it, forces a full enumeration, and resolves its unanswered
commands by the `HELLO`'s `in_head` (spec/commands.md).

`HELLO` is a barrier. The agent writes it only when its queues are empty, so every
record made before it is before it in the ring. The sidecar discards sample-class
records that come before the `HELLO` of its own attach: they were sampled for the
sidecar before it.

### Oversized records

A payload is at most an eighth of the ring (spec/transport.md). The agent never writes
a larger one:

| Record | Written in its place |
|---|---|
| `RESULT` | The same record with a nil `value`, the code `TOO_LARGE`, and what was too large as the message |
| `LOG` | The same record with nil `data` |
| `AUTHOR_EVENT` | Nothing: `emit` refuses the payload first (spec/commands.md) |
| A sample | Nothing. The record is counted in `samples_dropped`. |
| `EVENT` | The same record with each string value cut to 1,024 bytes, at a character boundary where it is UTF-8, each table or other value that is not a scalar replaced by nil, and numbers and booleans kept |
| Any other event-class record | The same record with each string cut to 1,024 bytes and each `lv` value nil |
| Any other | A `LOG` record at `error` naming the kind, and a count in the pending `GAP` of events |

In a record written in place of another, the agent cuts every `str` to 1,024 bytes at
a character boundary, so the replacement always fits. An `EVENT`'s values are `lv`, not
`str`, and are cut as its row says.

### Codes: record_kind

| Code | Name | Class | Meaning |
|---|---|---|---|
| 0 | PADDING | | Fills the end of the ring |
| 1 | FRAME | control | Stamps for the records that follow |
| 2 | HELLO | control | The agent attached, or resynced |
| 3 | LIFECYCLE | control | The lifecycle state changed |
| 4 | RESULT | control | A command was handled |
| 5 | GAP | control | Records were dropped |
| 16 | OBJECT | event | A unit or static entered the agent's table, or a full enumeration found it |
| 17 | OBJECT_GONE | event | A unit or static left the agent's table |
| 18 | ENUM_BEGIN | event | A full enumeration started |
| 19 | ENUM_END | event | A full enumeration finished |
| 20 | EVENT | event | A DCS world event |
| 21 | AUTHOR_EVENT | event | An event from `emit` or an extension |
| 22 | PLAYER | event | A player connected, left, changed slot or chatted |
| 23 | MENU | event | A player selected a menu item |
| 24 | LOG | event | A log line from the hook, the shim or the agent |
| 32 | POSITIONS | sample | Positions of a batch of objects |
| 33 | VITALS | sample | Life and fuel of a batch of objects |
| 34 | ATTITUDE | sample | Heading, pitch and roll of a batch of objects |
| 35 | AMMO | sample | One unit's ammunition |
| 36 | WAREHOUSE | sample | One warehouse's stock |
| 37 | AIRBASE | sample | One airbase's owner |
| 38 | EXTENSION | sample | One registered extension's run time |

`FRAME` is written with the records it stamps and is never queued alone.

## Control records

### Layout: FRAME

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | frame | Frame sequence number: `frame` in the status block |
| 0x0008 | f64 | model_time | Seconds |
| 0x0010 | f64 | real_time | Seconds |
| 0x0018 | u64 | campaign | Active campaign ID, 0 for none |
| 0x0020 | u64 | number | The number of the record after this one |
| 0x0028 | u32 | mission_gen | Mission generation |
| 0x002C | u32 | lifecycle | A `lifecycle` code |
| 0x0030 | u32 | replay | 1 when the stamps are of an earlier frame, for queued records; else 0 |
| 0x0034 | pad[4] | reserved | |

### Layout: HELLO

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | agent_boot | The agent's boot |
| 0x0008 | u64 | sidecar_boot | The sidecar boot the agent attached to |
| 0x0010 | u64 | build | As in the status block |
| 0x0018 | u64 | campaign | Active campaign ID, 0 for none |
| 0x0020 | u64 | epoch | Command epoch |
| 0x0028 | u64 | last_applied_seq | Highest sequence number handled in `epoch` |
| 0x0030 | u64 | in_head | The inbound read position: every command before it was handled, and its `RESULT` is before this record |
| 0x0038 | u32 | interface | The agent's interface version |
| 0x003C | u32 | flags | `agent_flags` bits |
| + | str | dcs_version | The DCS version string, read once by the hook |

### Layout: LIFECYCLE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | state | A `lifecycle` code |
| 0x0004 | u32 | mission_gen | Mission generation |
| + | str | theater | The loaded mission's map, empty when none is loaded |
| + | str | mission | The loaded mission's file name without its folder, empty when none |

When `mission_gen` changes, every object of the earlier generation has left the agent's
table, and the agent has discarded the binding and every applied revision of the
earlier generation (spec/commands.md). The agent writes no `OBJECT_GONE` for them. The
sidecar ignores an `OBJECT` record stamped with a generation older than its current
one.

### Layout: RESULT

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | seq | The command's sequence number, 0 for a control command |
| 0x0008 | u64 | epoch | The command's epoch |
| 0x0010 | u64 | campaign | The command's campaign |
| 0x0018 | u64 | entity | The command's entity |
| 0x0020 | u64 | rev | The command's revision |
| 0x0028 | u64 | object | The DCS object or group ID the entity is bound to after the command, 0 for none |
| 0x0030 | u32 | cost_us | Time DCS spent on the command, microseconds |
| 0x0034 | u16 | opcode | The command's opcode |
| 0x0036 | u16 | code | A `result` code (spec/commands.md) |
| + | str | message | The error text for a failure, otherwise empty |
| + | lv | value | What the command returned: a captured state, an extension's return value, or nil |

`code` and `opcode` come from spec/commands.md. A `RESULT` for a command of an epoch
older than the agent's still carries that command's epoch.

A control command gets a `RESULT` only when it fails.

### Layout: GAP

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | class | A `gap_class` code |
| 0x0004 | u32 | count | Records dropped |
| 0x0008 | f64 | from_real | Real time of the first dropped record, seconds |
| 0x0010 | f64 | to_real | Real time of the last dropped record |
| 0x0018 | f64 | from_model | Model time of the first dropped record |
| 0x0020 | f64 | to_model | Model time of the last dropped record |
| 0x0028 | u32 | from_gen | Mission generation of the first dropped record |
| 0x002C | u32 | to_gen | Mission generation of the last dropped record |

### Codes: gap_class

| Code | Name | Meaning |
|---|---|---|
| 1 | EVENTS | Event-class records other than logs |
| 2 | LOGS | Log records |

## The object table

The agent keeps a table of the units and statics in the mission, fed by events, by its
own spawns and destroys, and by a full enumeration that is the authority when they
disagree. The sidecar mirrors it from these records.

A row leaves the table on a death or removal event, on the agent's own destroy, or when
a full enumeration does not find it. A position read never removes a row (LIVE,
docs/results/2026-11-11-first-dll-and-spikes.md, E20):

- A dead unit's ID keeps returning the wreck's position, so a position cannot show a
  death.
- A removed object's ID returns `0, 0, 0`. The agent does not publish a position of
  exactly `0, 0, 0`; it leaves the row out of the batch until an event or the next
  full enumeration settles it.

An object the agent destroyed stays in a pending-destroy set until DCS has removed it
(spec/commands.md); the agent ignores it in events and enumerations meanwhile.

Object IDs were not reused within a DCS process (LIVE, E20). The agent still keeps a
generation number on each row, in its own memory, and attributes a sample only to the
row it was read under, in case DCS does reuse one. The transport carries object IDs
only: a reused ID is an `OBJECT_GONE` and then an `OBJECT`.

### Layout: OBJECT

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | object | DCS object ID |
| 0x0008 | u64 | group | DCS ID of the unit's group, 0 for a static |
| 0x0010 | u64 | entity | The `EntityId` the agent's binding holds for the object, 0 when it holds none |
| 0x0018 | u64 | group_entity | The `EntityId` the binding holds for the group, 0 when it holds none |
| 0x0020 | u64 | enumeration | The enumeration that found it, 0 when an event or a spawn did |
| 0x0028 | u32 | kind | `UNIT` or `STATIC` (`object_kind`) |
| 0x002C | u32 | category | DCS's category number for the unit or static |
| 0x0030 | u32 | coalition | DCS coalition number |
| 0x0034 | u32 | country | DCS country number |
| 0x0038 | f32 | life0 | `getLife0()`: the life the object started with, in the scale of `VITALS` |
| 0x003C | pad[4] | reserved | |
| + | str | name | The object's DCS name |
| + | str | type | The DCS type name |
| + | str | group_name | The group's DCS name, empty for a static |

The agent fills `entity` and `group_entity` from platform names (spec/world.md) or from
`BIND` commands.
A full enumeration writes one `OBJECT` for every row, so a sidecar that attaches reads
the whole binding from it.

### Codes: object_kind

| Code | Name |
|---|---|
| 1 | UNIT |
| 2 | STATIC |
| 3 | GROUP |

`GROUP` is used only by `BIND` (spec/commands.md); the table holds units and statics.

### Layout: OBJECT_GONE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | object | DCS object ID |
| 0x0008 | u32 | reason | A `gone_reason` code |
| 0x000C | pad[4] | reserved | |

### Codes: gone_reason

| Code | Name | Meaning |
|---|---|---|
| 1 | DEAD | A death event named it |
| 2 | DESTROYED | The agent destroyed it for a command. Written when the destroy is called, which while paused is before DCS removes the object. |
| 3 | MISSING | A full enumeration did not find it |
| 4 | REMOVED | A removal event named it |

Only `DEAD` is a death fact. `MISSING` and `REMOVED` on a managed group lead to `lost`
(spec/world.md).

### Layout: ENUM_BEGIN

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | enumeration | A number that grows with each enumeration in the process, never 0 |

### Layout: ENUM_END

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | enumeration | The enumeration that finished |
| 0x0008 | u32 | objects | `OBJECT` records it wrote |
| 0x000C | pad[4] | reserved | |

An enumeration is sliced over frames, and the world changes while it runs. Only a row
that was in the table at `ENUM_BEGIN` and that the enumeration did not find is
`MISSING`. A row added after `ENUM_BEGIN`, by an event or a spawn, is never removed by
that enumeration. The sidecar applies the same rule to its mirror.

The sidecar uses an enumeration only if it read `ENUM_BEGIN`, `ENUM_END` with a
matching count of `OBJECT` records, and no `GAP` of events between them. Otherwise it
asks for another.

## Events

### Layout: EVENT

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | id | The DCS event ID |
| 0x0004 | u32 | n | Fields that follow |
| + | tlv[n] | fields | Each an `event_field` code and a value |

The shim's handler runs wholly inside `pcall`. For each event type it reads only the
plain fields on that type's allow-list and never calls a method on an event object. An
event of a type with no allow-list is written with `n` 0. Every field is optional: an
event of one type does not always carry the same fields.

The shim reads every field with `rawget`, which calls no metamethod. A field read
from an object, such as `event.initiator.id_`, is taken as the object's own `id_` key.
Every path in "Read from" but `ROUNDS`'s was present on some event type in the event
catalogue, and every object carried its ID as its own `id_` number. On a kill whose
killer was already dead, `event.initiator` was a plain table, not an object, with
`id_`, `coalition`, `dead`, `misID` and `unit_type`, and the shim still reads `id_`
from it (LIVE, `docs/results/2026-11-13-event-catalogue.md`). A value
whose Lua type is not the type in "Value" below is left out, as is a missing one. The
agent writes the fields in the order of the allow-list.

The agent counts its time for each event, from the call into the agent to the record
queued, in `hist_event` (spec/transport.md). The agent takes at most 16 fields per
event, so a cut `EVENT` always fits (see "Oversized records").

### Codes: event_field

| Code | Name | Value | Read from |
|---|---|---|---|
| 1 | TIME | number | `event.time` |
| 2 | INITIATOR | number | `event.initiator.id_` |
| 3 | TARGET | number | `event.target.id_` |
| 4 | WEAPON | number | `event.weapon.id_` |
| 5 | WEAPON_NAME | string | `event.weapon_name` |
| 6 | PLACE | number | `event.place.id_` |
| 7 | SUBPLACE | number | `event.subPlace` |
| 8 | TEXT | string | `event.text` |
| 9 | MARK_ID | number | `event.idx` |
| 10 | POS_X | number | `event.pos.x` |
| 11 | POS_Y | number | `event.pos.y` |
| 12 | POS_Z | number | `event.pos.z` |
| 13 | COALITION | number | `event.coalition` |
| 14 | GROUP_ID | number | `event.groupID` |
| 15 | COMMENT | string | `event.comment` |
| 16 | ROUNDS | number | The rounds expended, on a shooting-end event |
| 17 | INITIATOR_CATEGORY | number | `event.initiator_data.object_category` |
| 18 | TARGET_CATEGORY | number | `event.target_data.object_category` |

`INITIATOR_CATEGORY` and `TARGET_CATEGORY` are DCS's `Object.Category` numbers. In the
event catalogue (LIVE, `docs/results/2026-11-13-event-catalogue.md`) a spawned unit's
events carried 1, the spawned static's 3, and the objects that died around an
explosion 5, which is DCS's number for a scenery object (PRIOR). They say what kind of
object the ID in `INITIATOR` or `TARGET` names: an explosion that kills a unit also
kills scenery near it, and each scenery object is a death event of its own; in that
run 304 of 309 `S_EVENT_DEAD` events were of category 5 (LIVE, same report).

`TIME` is model time on every type the catalogue's recorder saw but the mark events:
it was within 0.004 s of model time when the event arrived (LIVE,
`docs/results/2026-11-13-event-catalogue.md`). On `S_EVENT_MARK_ADDED` it was 28,799.1 s
ahead of model time, which matches the time of day for a mission that starts at
28,800 s after midnight, as the shell does (`start_time` in `templates/caucasus.miz`).
On `S_EVENT_MARK_REMOVED` it was at most 28,796.1 s ahead, 3 s less, and the scenario
removed the line 3 s after adding it: the removal seems to carry the removed mark's add
time, not the time of the removal (an inference from the report's per-cause maxima;
UNVERIFIED).

Whether a shooting-end event carries a count of rounds, and under what name, is
UNVERIFIED (E32, step S16); where it does not, the sidecar fills the value from the
ammunition difference. No allow-list holds `ROUNDS` until then. Codes are added and
never renumbered.

### Allow-lists

The fields the shim reads, by event type. The type is the `world.event` name, whose ID
the shim reads from `world.event` in the `server` state when it registers its handler; a
name the running DCS lacks has no allow-list. The lists come from the event catalogue
(experiment E9, `docs/results/2026-11-13-event-catalogue.md`, and the scripted
client of experiments E30 and E31, `docs/results/2026-11-13-e31-event-catalogue.md`). "Evidence" says
whether the catalogue saw the type with its fields on the Wine server (LIVE), saw it
only as an agent record with no field data (stream only), saw it only in an earlier
run of the same scenarios whose report is not committed (PRIOR), or did not reach it
(UNVERIFIED: a lead, to be checked when a step reaches the type). A type with no row,
which includes every type no run reached, is forwarded with its ID only.

| Event | Fields | Evidence |
|---|---|---|
| S_EVENT_SHOT | TIME, INITIATOR, INITIATOR_CATEGORY, TARGET, TARGET_CATEGORY, WEAPON, WEAPON_NAME | LIVE |
| S_EVENT_HIT | TIME, INITIATOR, INITIATOR_CATEGORY, TARGET, TARGET_CATEGORY, WEAPON, WEAPON_NAME | LIVE |
| S_EVENT_TAKEOFF | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | LIVE |
| S_EVENT_LAND | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | LIVE |
| S_EVENT_CRASH | TIME, INITIATOR, INITIATOR_CATEGORY | PRIOR: an earlier run of the same scenarios on the same server |
| S_EVENT_EJECTION | TIME, INITIATOR, INITIATOR_CATEGORY, TARGET, TARGET_CATEGORY | PRIOR: an earlier run of the same scenarios on the same server |
| S_EVENT_REFUELING | TIME, INITIATOR, INITIATOR_CATEGORY | UNVERIFIED |
| S_EVENT_DEAD | TIME, INITIATOR, INITIATOR_CATEGORY | LIVE |
| S_EVENT_PILOT_DEAD | TIME, INITIATOR, INITIATOR_CATEGORY | PRIOR: an earlier run of the same scenarios on the same server |
| S_EVENT_BASE_CAPTURED | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | LIVE |
| S_EVENT_MISSION_START | TIME | LIVE, stream only |
| S_EVENT_MISSION_END | TIME | LIVE, stream only |
| S_EVENT_REFUELING_STOP | TIME, INITIATOR, INITIATOR_CATEGORY | UNVERIFIED |
| S_EVENT_BIRTH | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | LIVE |
| S_EVENT_HUMAN_FAILURE | TIME, INITIATOR, INITIATOR_CATEGORY | UNVERIFIED |
| S_EVENT_ENGINE_STARTUP | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | LIVE |
| S_EVENT_ENGINE_SHUTDOWN | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | PRIOR: an earlier run of the same scenarios on the same server |
| S_EVENT_PLAYER_ENTER_UNIT | TIME, INITIATOR, INITIATOR_CATEGORY | UNVERIFIED: not sent for a scripted client that took a slot with `net.set_slot` and spawned with `net.spawn_player` (`docs/results/2026-11-13-e31-event-catalogue.md`) |
| S_EVENT_PLAYER_LEAVE_UNIT | TIME, INITIATOR, INITIATOR_CATEGORY | LIVE |
| S_EVENT_PLAYER_COMMENT | TIME, INITIATOR, INITIATOR_CATEGORY, COMMENT | UNVERIFIED |
| S_EVENT_SHOOTING_START | TIME, INITIATOR, INITIATOR_CATEGORY, TARGET, TARGET_CATEGORY, WEAPON_NAME | LIVE |
| S_EVENT_SHOOTING_END | TIME, INITIATOR, INITIATOR_CATEGORY, WEAPON_NAME | LIVE |
| S_EVENT_MARK_ADDED | TIME, INITIATOR, MARK_ID, TEXT, POS_X, POS_Y, POS_Z, COALITION, GROUP_ID | LIVE for a scripted mark, which has no initiator; not seen: INITIATOR |
| S_EVENT_MARK_CHANGE | TIME, INITIATOR, MARK_ID, TEXT, POS_X, POS_Y, POS_Z, COALITION, GROUP_ID | UNVERIFIED |
| S_EVENT_MARK_REMOVED | TIME, INITIATOR, MARK_ID, TEXT, POS_X, POS_Y, POS_Z, COALITION, GROUP_ID | LIVE for a scripted mark, which has no initiator; not seen: INITIATOR |
| S_EVENT_KILL | TIME, INITIATOR, INITIATOR_CATEGORY, TARGET, TARGET_CATEGORY, WEAPON, WEAPON_NAME | LIVE |
| S_EVENT_SCORE | TIME, INITIATOR, INITIATOR_CATEGORY, COMMENT | LIVE |
| S_EVENT_UNIT_LOST | TIME, INITIATOR, INITIATOR_CATEGORY | LIVE |
| S_EVENT_LANDING_AFTER_EJECTION | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | PRIOR: an earlier run of the same scenarios on the same server |
| S_EVENT_DISCARD_CHAIR_AFTER_EJECTION | TIME, INITIATOR, INITIATOR_CATEGORY, TARGET, TARGET_CATEGORY | PRIOR: an earlier run of the same scenarios on the same server |
| S_EVENT_WEAPON_ADD | TIME, INITIATOR, INITIATOR_CATEGORY, WEAPON_NAME | LIVE |
| S_EVENT_LANDING_QUALITY_MARK | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, COMMENT | UNVERIFIED |
| S_EVENT_BDA | TIME, INITIATOR, INITIATOR_CATEGORY, TARGET, TARGET_CATEGORY, WEAPON, WEAPON_NAME | LIVE |
| S_EVENT_AI_ABORT_MISSION | TIME, INITIATOR, INITIATOR_CATEGORY | PRIOR: an earlier run of the same scenarios on the same server |
| S_EVENT_RUNWAY_TAKEOFF | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | LIVE |
| S_EVENT_RUNWAY_TOUCH | TIME, INITIATOR, INITIATOR_CATEGORY, PLACE, SUBPLACE | LIVE |
| S_EVENT_PLAYER_SELF_KILL_PILOT | TIME, INITIATOR, INITIATOR_CATEGORY | UNVERIFIED |
| S_EVENT_SIMULATION_FREEZE | TIME | LIVE |
| S_EVENT_SIMULATION_UNFREEZE | TIME | LIVE |
| S_EVENT_MISSION_RESTART | TIME | LIVE, stream only |
| S_EVENT_SIMULATION_START | TIME | LIVE, stream only |

The hook loads the shim on the first simulation start of the process, and the shim
registers its handler then and again on every later simulation start; a shim reload
registers the new handler at once. An event DCS sends before the handler is registered
is not seen, and no `GAP` records it. `S_EVENT_MISSION_START` and
`S_EVENT_SIMULATION_START` of a load arrived as agent records (LIVE,
`docs/results/2026-11-13-event-catalogue.md`), so they come after the registration;
whether any other event of a mission load comes before it is UNVERIFIED.
ED's registry is keyed by
the handler, so a second add changes nothing (ED-CODE,
`Scripts/World/EventHandlers.lua`). A shim reload removes the replaced shim's handler
before it adds its own.

### Layout: AUTHOR_EVENT

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | n | Objects that follow |
| 0x0004 | pad[4] | reserved | |
| + | u64[n] | object | DCS object IDs of the units the event is about, from `emit` |
| + | str | name | The event's name |
| + | lv | payload | The author's value |

### Layout: PLAYER

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | event | A `player_event` code |
| 0x0004 | u32 | player | The ID DCS gives the connection |
| 0x0008 | u32 | side | DCS coalition number |
| 0x000C | u32 | seat | The seat in a multi-crew aircraft, 0 for the first or when DCS gives none |
| + | str | client | The ID DCS gives the client, from which the sidecar derives the player key |
| + | str | name | The player's name |
| + | str | slot | The slot ID |
| + | str | text | The chat text, for `CHAT` |

Player records come from the hook's callbacks, in the hook's own state. On a dedicated
server that state is not the `gui` state (LIVE,
docs/results/2026-11-11-server-gate.md). Which callbacks and
fields DCS provides for a human player is UNVERIFIED (E31, steps S9 and S16), and so is
how a slot ID names a unit and a seat. Step S16 fixes both and may add fields.

The hook registers an `onPlayerTry*` callback only where it needs one, and such a
callback never returns a value: a returned value would decide the matter for every
other hook.

The sidecar builds the API's player events from these records:

| API event | From |
|---|---|
| connect, disconnect, chat | `CONNECT`, `DISCONNECT`, `CHAT` |
| enter unit, leave unit | `SLOT`: leaving the earlier slot, entering the new one; and `DISCONNECT` |
| rename | `RENAME`, or a `CONNECT` or `SLOT` whose name differs from the last one seen for the client |

### Codes: player_event

| Code | Name |
|---|---|
| 1 | CONNECT |
| 2 | DISCONNECT |
| 3 | SLOT |
| 4 | CHAT |
| 5 | RENAME |

### Layout: MENU

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | entity | The menu item's `EntityId` |
| 0x0008 | u64 | target | What the item was added for, by `scope`: 0, a coalition number or a group `EntityId` |
| 0x0010 | u32 | scope | The item's `scope` code (spec/commands.md) |
| 0x0014 | u32 | player | The ID DCS gives the connection of the player who selected it, 0 when DCS does not say |

Whether DCS tells a menu callback which player selected the item is UNVERIFIED (step
S16). When it does not, the sidecar attributes the selection to the players the item
was shown to: for a group-scoped item, the occupants of that group.

### Layout: LOG

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | level | 1 error, 2 warn, 3 info, 4 debug |
| 0x0004 | pad[4] | reserved | |
| + | str | target | The writer (spec/log.md) |
| + | str | msg | The message |
| + | lv | data | The record's `data` table, or nil |

The writer also appends the line to its own log file (spec/log.md).

### Capabilities

The hook and the shim each write one `LOG` record at `info` with the message
`capabilities` when they start (spec/install.md, "Capabilities"): the hook once per
process, the shim once per load or reload. Its `data` has one key per DCS name the
platform calls in that state, such as `DCS.getPause` or `world.addEventHandler`, and
the value `true` when the name is present or `false` when it is absent. The `target`
says which state: `hook` or `shim` (the server state).

## Samples

A sample batch is columnar: `n`, then one array per column. Row `i` of every array
describes the same object.

### Layout: POSITIONS

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | n | Rows |
| 0x0004 | pad[4] | reserved | |
| + | u64[n] | object | DCS object IDs |
| + | f64[n] | x | DCS `x`, meters |
| + | f64[n] | y | DCS `y`, meters |
| + | f64[n] | z | DCS `z`, meters |

### Layout: VITALS

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | n | Rows |
| 0x0004 | pad[4] | reserved | |
| + | u64[n] | object | DCS object IDs |
| + | f32[n] | life | `getLife()`: DCS's absolute value, whose scale depends on the type |
| + | f32[n] | fuel | `getFuel()`, a fraction of internal fuel |

### Layout: ATTITUDE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | n | Rows |
| 0x0004 | pad[4] | reserved | |
| + | u64[n] | object | DCS object IDs |
| + | f32[n] | heading | Radians |
| + | f32[n] | pitch | Radians |
| + | f32[n] | roll | Radians |

Attitude is not sampled periodically. The agent writes it on a capture, for a
subscribed object, and once when an object stops.

### Layout: AMMO

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | object | DCS object ID |
| + | lv | ammo | An array of tables, each with `type` (string) and `count` (number) |

### Layout: WAREHOUSE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | airbase | The airbase's DCS name |
| + | lv | stock | The inventory as plain tables of names and counts |

### Layout: AIRBASE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | object | The airbase's DCS object ID, which an event's `PLACE` field names |
| 0x0008 | u32 | coalition | DCS coalition number of the owner |
| 0x000C | pad[4] | reserved | |
| + | str | airbase | The airbase's DCS name |

### Layout: EXTENSION

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | calls | Hook calls since the extension was installed in this mission |
| 0x0008 | u64 | total_us | Time in those runs, microseconds |
| 0x0010 | u64 | max_us | The longest run, microseconds |
| 0x0018 | u64 | rev | The extension's applied revision |
| 0x0020 | u64 | events_skipped | Events its `on_event` queue dropped |
| 0x0028 | u32 | stopped | 1 when its hooks are stopped after errors |
| 0x002C | pad[4] | reserved | |
| + | str | name | The extension's name |

The agent writes one `EXTENSION` record per registered extension every 10 s, and one
when an extension's hooks are stopped.
