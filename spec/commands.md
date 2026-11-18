# Commands

What the sidecar writes to the inbound ring. The ring, the record header and the
encodings `str` and `lv` are in spec/transport.md. The record's `kind` is the opcode.
Every payload starts with the command header; an opcode's own layout follows it, with
offsets from the end of the header.

### Layout: command

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | seq | Sequence number, 0 for a control command |
| 0x0008 | u64 | epoch | Command epoch, 0 for a control command |
| 0x0010 | u64 | campaign | The campaign the command was written for, 0 for none |
| 0x0018 | u64 | entity | The `EntityId` the command acts on, 0 when it has none |
| 0x0020 | u64 | rev | The revision the command carries, 0 when it has none |
| 0x0028 | u32 | mission_gen | The mission generation the command was written for |
| 0x002C | pad[4] | reserved | |

A payload shorter than the command header is a failed consuming check: the agent
writes no `RESULT`, logs it, and detaches (spec/transport.md, "Consuming"). The command
was taken, so after the next `HELLO` the sidecar counts it `unknown`.

DCS call names in this document are PRIOR unless tagged; steps S15 and S16 check each
against a running DCS.

## Delivery classes

| Class | Delivery | Rule |
|---|---|---|
| control | in order, not sequenced | Applied when the agent reaches it in the ring. Not counted against the budget. Safe to repeat. |
| set-state | at-least-once, de-duplicated | Applied once per sequence number. One that carries a revision is applied only when the revision is newer than the one the agent applied. Until it has a result, the sidecar derives the command again from the store and sends that under a new sequence number; it never sends an old payload again. |
| effect | at-most-once | Applied once if its sequence number is new. The sidecar never sends it again. |
| query | at-least-once | Reads state and changes nothing. Applied once per sequence number; the sidecar sends it again under a new sequence number if it needs the value. |

The agent reads the inbound ring strictly in order. It never applies a command ahead
of one before it in the ring.

## Sequence, epoch and revisions

The command epoch is a counter the sidecar owns and keeps in the server store
(spec/persistence.md, "The server store"), so it grows across campaigns.
It only grows. The sidecar starts a new epoch when the controller lease changes hands,
when the active campaign changes, when it attaches to a new agent boot, and when it
starts without its journal. A new epoch is one more than the larger of the store's
epoch and the `epoch` in the agent's `HELLO`, so it is above the agent's even when the
store was lost or is in memory only. The sidecar writes no sequenced command before it
has read that `HELLO` (spec/transport.md). The lease's own fencing epoch (the Session
service) fences API callers and never reaches the agent.

If the `HELLO` names a campaign the sidecar's stores do not hold, the sidecar sends
`CAMPAIGN` 0 first, so the agent discards what it kept for it.

The sidecar numbers the commands of an epoch from 1, in the order it writes them. It
journals each command with its epoch, its sequence number and the inbound ring
position just after it, before it writes it.

The agent keeps in its own memory:

| State | Scope | Discarded |
|---|---|---|
| `epoch`, `last_applied_seq` | the DCS process | when DCS exits |
| `applied_spec_rev`, `applied_order_rev` per entity; the binding | the mission generation | when the mission generation changes, on a campaign switch, when DCS exits |
| The applied revision and source of each extension, by name | the campaign | on a campaign switch, when DCS exits |

All of it survives a sidecar restart. An applied revision starts at 0, and a command
that carries a revision carries one of at least 1.

A mission load removes every group, static, mark, drawing and menu item from DCS
(LIVE for groups, docs/results/2026-11-11-first-dll-and-spikes.md, E18; UNVERIFIED for
marks, drawings and menu items, step S16), so the agent forgets their applied
revisions with them. After a load the sidecar sends what DCS no longer has: every
spec; then each group's order and its controller options; then every airbase owner
and warehouse stock that differs from the mission's. The agent applies each as new.
After a spawn of one group the sidecar sends that group's order and options, and
nothing else.

For each command that is not a control command, in ring order, the first row that
matches decides:

| Check | Result | Handler runs |
|---|---|---|
| The command's epoch is lower than `epoch` | `STALE_EPOCH` | no |
| The command's epoch is higher than `epoch` | `epoch` becomes the command's; `last_applied_seq` becomes 0; go on to the next row | |
| `seq` is at or below `last_applied_seq` | `DUPLICATE` | no |
| The command's campaign is not the agent's active campaign | `WRONG_CAMPAIGN` | no |
| No campaign is active and the opcode needs one | `NO_CAMPAIGN` | no |
| The opcode is bound to a generation and `mission_gen` is not the agent's | `WRONG_GENERATION` | no |
| The opcode's Rev column names a revision and `rev` is 0 | `BAD_PAYLOAD` | no |
| The opcode's Rev column names a revision and `rev` is at or below the applied one | `AT_REVISION` | no |
| The opcode needs raw Lua and `agent.toml` has it off | `RAW_LUA_OFF` | no |
| No shim is loaded in the handler's state and the lifecycle state is not `LOADING` | `NOT_READY` | no |
| The opcode is `EXTENSION_INVOKE` and no extension of the name has an applied revision in the campaign | `LUA_ERROR`, message `no extension of that name` | no |
| Otherwise | the handler's result | yes |

The Rev column of the opcode table says which revision a command carries: `spec` is
compared with `applied_spec_rev` of the command's entity, `order` with
`applied_order_rev`, and for `EXTENSION_SET` with the applied revision of the named
extension. A set-state command with an empty Rev column carries no revision and is
de-duplicated by its sequence number only.

After any row from `WRONG_CAMPAIGN` down, `last_applied_seq` becomes the command's
`seq`. The applied revision becomes `rev` only when the handler's result is `APPLIED`
or `TOO_LARGE`; any other result leaves it as it was. Every command that is not a control command gets exactly one `RESULT` record
(spec/records.md).

A command written before a mission load and read after it would act on a world it was
not made for: a spawn compiled for another map, an order for a group that is gone.
The Gen column of the opcode table marks the opcodes this applies to, and the agent
answers them `WRONG_GENERATION`.

A set-state command answered `STALE_EPOCH`, `WRONG_CAMPAIGN`, `WRONG_GENERATION` or
`NOT_READY` is
not lost: the sidecar derives what DCS still needs from the store's desired state and
sends that under the current epoch and generation. It never sends an old payload
again.

### Unanswered commands

The agent stores `in_head` past a command just before it runs it
(spec/transport.md), so a ring position says whether the agent took a command. A
`HELLO` record carries the agent's `in_head`, and every `RESULT` made before the
`HELLO` is before it in the ring. So when the sidecar reads a `HELLO`, each journalled
command with no result is one of two things:

| The command's position | It was | Outcome |
|---|---|---|
| at or before the `HELLO`'s `in_head` | taken, and its result was lost | `unknown` |
| after it | never taken | still `pending` if it is in the ring; otherwise as below |

**After a sidecar restart** the new sidecar sets `in_tail` to `in_head` when it
attaches, so a command after the `HELLO`'s `in_head` is no longer in the ring and was
never applied. Every command that is not a control command resolves as `not_sent`.
An effect or a client's query is not sent again; for a set-state command, and for a
query the sidecar itself needs (the `CAPTURE` of a dematerialization), the sidecar
derives what DCS still needs from the store (spec/errors.md).

**After `CONTROL_LOST`** the ring is untouched: a command after the `HELLO`'s `in_head`
is still there and stays `pending`. A `HELLO` is a resync, and not an attach, when it is
not the first `HELLO` the sidecar read since its handshake.

**After DCS dies** no `HELLO` comes from the old process, and DCS may have taken
commands after the last `in_head` the sidecar read. Every unanswered command of an
`agent_boot` other than the current `HELLO`'s stays `pending` until the first `HELLO` of
a new `agent_boot`, and then resolves `unknown`, whatever its position, whether or not
the sidecar was running when DCS died. The breadcrumb (`crumb_arg`) names the command
that was running when DCS died; it is in `crash.log` (spec/install.md) and in the
status block, which the command-line inspector shows.

**After the kill switch** the agent writes no further `HELLO` until the process exits,
so the sidecar resolves its commands as after DCS dies. `CONTROL_LOST` stays set in
the status block if a control record was lost.

What the sidecar does about `unknown` is in spec/errors.md. After a DCS restart the
agent's memory is empty, and the sidecar starts a new epoch and materializes from the
store (spec/world.md).

## Budget

Each frame the agent applies commands in ring order while the time DCS has spent on
this frame's commands is under the commanded-work budget (spec/config.md). A control
command costs nothing against the budget. The agent always applies one command if one
is waiting. Commands it did not reach stay in the ring, a control command among them.

While the restore burst is on (`BURST`) and the lifecycle state is `PAUSED`, the budget
does not apply.

Each handler runs inside `pcall`, with `crumb` set to 0x10000 plus the opcode and
`crumb_arg` to the command's `seq` (spec/transport.md, "Breadcrumbs").
`cost_us` in the result is the time from the agent taking the command to `command_done`.
A command over the budget is counted in `commands_over_budget`.

### Handlers

The agent reads the ring in the hook's state, in `frame`. Control commands, and every
command the admission rows answer, are handled there in Rust. A command that passes
them runs its handler in the Lua state the State column names.

`frame` looks at the command at the ring's head before it takes it. When that command
is sequenced and has a `server` handler, `frame` stops reading: the command is not
taken, not admitted, and `in_head` does not move. `frame` then asks the hook for a pump,
on a sampling frame or not, but never on the frame that makes the agent's system calls.
In the pump, the shim calls the agent's `command`, which takes commands in ring order
under the same budget. It handles control commands and admission answers in Rust, and
returns the opcode and the decoded arguments of the next admitted command with a
`server` handler. It returns nothing when the ring is empty, the budget is spent, or
the head has a `hook` handler. The shim runs the handler under `pcall` and passes its
result to the agent's `command_done(code, message, value, destroyed)`; a raise is
`LUA_ERROR` with the error text, and an opcode the shim has no handler for is
`UNKNOWN_OPCODE`. No command is held from one frame to the next.

A `hook` handler runs the same way from the hook, with no cross-state call. Each state's
`command` hands out only a command at the ring's head whose handler runs in that state,
so ring order holds, and one command is outstanding at a time. When the head waits for
a `hook` handler, `frame` says so, never on the frame that makes the agent's system
calls, and leaves ending the frame to the hook: the hook runs the pump if `frame` asked
for one, then its own commands, then `finish`. Hook commands share the commanded-work
budget and the `no result` answer, and are excused from the sampling slice as a
commands-only pump is. The hook's dispatch is one function that any hook callback can
call. A `hook` opcode bound to a generation waits while the lifecycle state is
`LOADING`, and is answered `NOT_READY` when no mission is loaded otherwise. `CHAT` and
`MISSION_LOAD` take any campaign and any generation, so they run with no mission
loaded.

A pump begins with the generation check, then the commands, then sampling. A pump that
`frame` asked for only for commands samples nothing, and its time, the cross-state call
included, counts against the commanded-work budget only.

Every command the agent takes gets exactly one `RESULT`. A command taken and not
answered by `command_done` before the pump ends (or, failing that, before `finish` or
the next `frame`) is answered `LUA_ERROR` with the message `no result`, and the
breadcrumb is cleared. A `command_done` with no command outstanding is logged and
ignored. The applied revision moves only in `command_done`.

When no shim is loaded, a command with a `server` handler waits at the head of the
ring while the lifecycle state is `LOADING`. In any other state the agent takes it and
answers `NOT_READY`. A pump that `frame` asked for and that never called `command`, such
as one whose shim is gone or whose `pump_begin` returned nil, also ends with `NOT_READY`
for the `server` command at the head, from `finish`. A command a pump left untaken
because the budget ran out stays in the ring for the next frame.

No frame sees the lifecycle state `LOADING` today: `frame` moves `STARTING` and
`LOADING` to `RUNNING` or `PAUSED` before it reads commands, so the wait is a defense.
DCS runs no frame between `onMissionLoadBegin` and `onSimulationStart` (LIVE,
docs/results/2026-11-15-s16-players-moderation.md).

The agent reads at most 4,096 table entries (key and value pairs, at every depth) and
16 KiB of strings from the value `command_done` receives, 16 tables deep. A string that
would pass the byte limit is not copied. A value that passes either limit is not sent:
an `APPLIED` result becomes `TOO_LARGE`, with a nil `value` and a message naming the
limit; any other code stays, with a nil `value`. The agent cuts every
`RESULT` message to 1,024 bytes at a UTF-8 boundary. A `command_done` with a code that
`result` does not define is answered `LUA_ERROR` with a message naming the code, and the
message is read only when it is a string.

### Handler arguments

The agent decodes the payload into one table and passes it to the handler. A payload
that does not decode is answered `BAD_PAYLOAD` in Rust, and no handler runs. An opcode
whose Entity column names a kind also gets `name`, the entity's platform name
(spec/world.md, "Names") built from the header's `campaign` and `entity`: `np!` for a
group, `np#` for a static, `np.` for a unit. A header whose `entity` is 0 or above 2^53 - 1 gives no
name, and the command is answered `BAD_PAYLOAD`. `CAPTURE` gets both `name` (the group
form) and `static_name`, because its entity is a group or a static.

| Opcode | Fields |
|---|---|
| `GROUP_SPAWN` | `country`, `category`, `name`, `group` |
| `GROUP_ORDER` | `name`, `task` |
| `GROUP_DESTROY`, `GROUP_CAPTURE_DESTROY` | `name` |
| `GROUP_OPTION` | `name`, `option`, `value` |
| `STATIC_SPAWN` | `country`, `name`, `static` |
| `STATIC_DESTROY` | `name` |
| `AIRBASE_OWNER` | `coalition`, `airbase` |
| `WAREHOUSE_SET` | `airbase`, `stock` |
| `WAREHOUSE_ADJUST` | `airbase`, `delta` |
| `DAMAGE` | `name`, `life_percent` |
| `SOUND` | `scope`, `target`, `name`, `file` |
| `PICTURE` | `scope`, `target`, `name`, `file`, `seconds`, `clear_view`, `size`, `horizontal`, `vertical`, `size_units` |
| `PICTURE_STOP` | none |
| `CAPTURE` | `name`, `static_name` |
| `EXTENSION_SET` | `name`, `source` |
| `EXTENSION_REMOVE` | `name` |
| `EXTENSION_INVOKE` | `name`, `args` |
| `MARK_SET` | `entity`, `scope`, `target`, `name`, `read_only`, `x`, `y`, `z`, `text` |
| `DRAWING_SET` | `entity`, `scope`, `target`, `shape`, `args` |
| `MENU_SET` | `entity`, `parent`, `scope`, `target`, `name`, `submenu`, `text` |
| `MARK_REMOVE`, `DRAWING_REMOVE`, `MENU_REMOVE` | `entity` |
| `MESSAGE` | `scope`, `target`, `name`, `clear`, `seconds`, `text` |
| `CHAT` | `player`, `text` |
| `MISSION_LOAD` | `path` |
| `EVAL` | `source` |
| `CLEAR_DEBRIS` | `volume`, `x`, `y`, `z`, `x2`, `y2`, `z2`, `radius` |

For the mark, drawing and menu opcodes, `entity` and `parent` are the header's entity
and the layout's parent as decimal text, so no `EntityId` loses digits in a Lua number;
`parent` is `'0'` for the root. `read_only`, `submenu` and `clear` are booleans. `name`
is the group's platform name for the scope `GROUP` (and the unit's for `MESSAGE`'s
`UNIT`), built from the header's `campaign` and the `target`; it is empty otherwise.
`target` is passed as a number for the scopes `COALITION`, `COUNTRY` and `GROUP` and
is absent otherwise.

When the agent's table holds a row bound to the target entity, the handler gets the
bound target, and `name` is empty for `MARK_SET`, `DRAWING_SET`, `MENU_SET` and
`MESSAGE` and absent for `SOUND` and `PICTURE`: for `GROUP`, `group_id`, the DCS group ID the row
holds, which the scoped DCS calls take as the group's mission ID; for `UNIT`, `row`, the
shim's row slot, whose object the shim asks for `getID()`. This is how a scope names
an adopted unit or group (spec/world.md, "Managed and unmanaged"), which keeps its own
DCS name. `PICTURE` gets both as decimal text, and its hook handler reads the unit's
mission ID through the `server` state. With no bound row, the handler resolves `name`.
A scope the opcode does not take is `BAD_PAYLOAD`, from the agent.

For `SOUND` and `PICTURE` with no bound row, `name` is present for the scope `GROUP` or
`UNIT` only: the platform name built from the header's `campaign` and the `target` (`np!` or `np.`),
empty with campaign 0. `PICTURE`'s hook handler builds Lua source, so the agent formats
its numbers as text, as for `DAMAGE`, and a `file` that is empty, longer than 255 bytes or
holds a character outside letters, digits, space and `_.-/()+,#` is `BAD_PAYLOAD`. So are
`seconds` outside 0 to 86,400 (a day), and `size` below 0.0001 or above 10,000, so the
size never reads 0 in the four decimals the agent writes. The
Mission Editor offers sizes 1 to 100 (ED-CODE); the wider bound leaves room for a
pixel unit whose range is UNVERIFIED, and keeps a number DCS might misread out.
`SOUND`'s `file` is passed to DCS as a value and is any string.

A spawn handler returns the new group's or static's DCS ID as its value. The agent
writes that ID in the `RESULT`'s `object` and sends a nil `value`.

`command_done`'s fourth argument, `destroyed`, is an array of the DCS object IDs a
command removed. The handler reads them before the DCS call: the group's unit IDs for
`GROUP_DESTROY` and `GROUP_CAPTURE_DESTROY`, the static's ID for `STATIC_DESTROY`, for
a `GROUP_SPAWN` under the name of an existing group that group's unit IDs, and for a
`STATIC_SPAWN` under the name of an existing static that static's ID. It passes them
only with `APPLIED`, after the call returned without raising and, for a spawn, returned
an object. The agent also acts on them when it turns that `APPLIED` into `TOO_LARGE`,
since the destroy has happened. It reads `destroyed` only for those five opcodes, under
the same `lua_pcall` that reads the value. It reads at most 4,096 IDs from one `destroyed`; the
rest are counted in `pending_overflow` and leave through the existence checks.
`command_done` returns how many IDs it took from `destroyed`, 0 when the read failed;
the shim forgets its handles for those IDs only. For each ID it holds a row for, it writes
`OBJECT_GONE` with `DESTROYED` before the `RESULT` and drops the row; every named ID,
held or not, enters the pending-destroy set (below). Every `APPLIED` `GROUP_SPAWN` sets
the entity's `applied_order_rev` to 0.

### Unknown opcodes

An agent that reads an opcode it has no handler for cannot know its class. It goes by
`seq`: with `seq` 0 it skips the command and writes a `RESULT` with `UNKNOWN_OPCODE`;
otherwise the command goes through the epoch, duplicate and campaign rows as any
other, is answered `UNKNOWN_OPCODE`, and sets `last_applied_seq`.

### Codes: opcode

| Code | Name | Class | Entity | Rev | Campaign | Gen | State | Payload | Meaning |
| --- | --- | --- | --- | --- | --- | --- |---| --- | --- |
| 0 | PADDING |  |  |  |  |  |  |  | Fills the end of the ring |
| 1 | CAMPAIGN | control |  |  |  |  |  | layout | Announce the active campaign |
| 2 | ENUMERATE | control |  |  |  |  |  | none | Start a full enumeration now, which writes an `OBJECT` for every row |
| 3 | RATES | control |  |  |  |  |  | layout | Set the target period of each sampling tier |
| 4 | SUBSCRIBE | control |  |  |  |  |  | layout | Replace the set of objects sampled at a higher rate |
| 5 | BIND | control |  |  |  |  |  | layout | Give `EntityId`s to unmanaged objects |
| 6 | BURST | control |  |  |  |  |  | layout | Turn the restore burst on or off |
| 16 | GROUP_SPAWN | set-state | group | spec | active | bound | server | layout | Spawn a group |
| 17 | GROUP_ORDER | set-state | group | order | active | bound | server | layout | Give a group its order |
| 18 | GROUP_DESTROY | set-state | group |  | active | bound | server | none | Destroy a group |
| 19 | GROUP_CAPTURE_DESTROY | set-state | group |  | active | bound | server | none | Read a group's final state, return it, and destroy the group, in one frame |
| 20 | GROUP_OPTION | set-state | group |  | active | bound | server | layout | Set one controller option |
| 21 | STATIC_SPAWN | set-state | static | spec | active | bound | server | layout | Spawn a static |
| 22 | STATIC_DESTROY | set-state | static |  | active | bound | server | none | Destroy a static |
| 23 | AIRBASE_OWNER | set-state |  |  | active | bound | server | layout | Set an airbase's owner |
| 24 | WAREHOUSE_SET | set-state |  |  | active | bound | server | layout | Set a warehouse's stock |
| 25 | MARK_SET | set-state | mark | spec | active | bound | server | layout | Add or replace a map mark |
| 26 | MARK_REMOVE | set-state | mark |  | active | bound | server | none | Remove a map mark |
| 27 | DRAWING_SET | set-state | drawing | spec | active | bound | server | layout | Add or replace a map drawing |
| 28 | DRAWING_REMOVE | set-state | drawing |  | active | bound | server | none | Remove a map drawing |
| 29 | MENU_SET | set-state | menu item | spec | active | bound | server | layout | Add or replace an F10 menu item |
| 30 | MENU_REMOVE | set-state | menu item |  | active | bound | server | none | Remove an F10 menu item |
| 31 | EXTENSION_SET | set-state |  | spec | active |  | server | layout | Register or replace a Lua extension |
| 32 | EXTENSION_REMOVE | set-state |  |  | active |  | server | layout | Remove a Lua extension |
| 48 | EXPLOSION | effect |  |  | active | bound | server | layout | An explosion at a point |
| 49 | MESSAGE | effect |  |  | any |  | server | layout | Text on screen |
| 50 | SOUND | effect |  |  | any |  | server | layout | Play a sound file from the mission |
| 51 | SMOKE | effect |  |  | active | bound | server | layout | A smoke marker at a point |
| 52 | FLARE | effect |  |  | active | bound | server | layout | A signal flare at a point |
| 53 | ILLUMINATION | effect |  |  | active | bound | server | layout | An illumination bomb at a point |
| 54 | DAMAGE | effect | unit |  | active | bound | hook | layout | Set a unit's life |
| 55 | CHAT | effect |  |  | any |  | hook | layout | A chat message from the server |
| 56 | MISSION_LOAD | effect |  |  | any |  | hook | layout | Load a mission file |
| 57 | EXTENSION_INVOKE | effect |  |  | active |  | server | layout | Run a registered extension |
| 58 | EVAL | effect |  |  | active |  | server | layout | Run Lua source in the `server` state |
| 59 | WAREHOUSE_ADJUST | effect |  |  | active | bound | server | layout | Add to or take from a warehouse's stock |
| 60 | PICTURE | effect |  |  | any |  | hook | layout | Show a picture from the mission |
| 61 | PICTURE_STOP | effect |  |  | any |  | hook | none | Take every picture off screen |
| 62 | MODERATE | effect |  |  | any |  | hook | layout | Kick, ban, unban, or move a player |
| 63 | CLEAR_DEBRIS | effect |  |  | any | bound | server | layout | Remove wrecks, craters and debris inside a volume |
| 64 | CAPTURE | query | group or static |  | active | bound | server | none | Read an entity's full state and return it |
| 65 | COUNTRIES | query |  |  | any | bound | server | none | Read each country's coalition in the loaded mission |
| 66 | PLAYERS | query |  |  | any |  | hook | none | Read the connected players and their slots |
| 67 | BANS | query |  |  | any |  | hook | none | Read DCS's ban list |

The State column names the Lua state the handler runs in: `server`, the mission
scripting state, through the shim; `hook`, the hook's own state. A control command has
no Lua handler. `CHAT` and `MISSION_LOAD` run in the hook's state (LIVE,
docs/results/2026-11-15-s16-players-moderation.md).

The Campaign column: `active` means the command needs an active campaign; `any` means
it also runs with campaign 0, for a server that runs a plain mission between
campaigns. The Gen column: `bound` means the command acts on the objects of one
mission and is refused in another generation. The Payload column: `layout` means
the opcode has a layout below; `none` means nothing follows the command header.

### Codes: result

| Code | Name | Class | Meaning |
|---|---|---|---|
| 0 | APPLIED | success | The handler ran and DCS accepted the call |
| 2 | AT_REVISION | success | The agent already applied this revision or a newer one |
| 3 | DUPLICATE | | The sequence number was already handled. The first result says what happened. |
| 16 | STALE_EPOCH | failure | The command's epoch is older than the agent's |
| 17 | NO_CAMPAIGN | failure | No campaign is active and the opcode needs one |
| 18 | NOT_BOUND | failure | The entity has no object in DCS |
| 19 | BAD_PAYLOAD | failure | The payload did not decode, or a revision is missing |
| 20 | UNKNOWN_OPCODE | failure | The agent has no handler for the opcode |
| 21 | RAW_LUA_OFF | failure | The command needs raw Lua, which `agent.toml` has off |
| 22 | DCS_REFUSED | failure | The DCS call raised, or returned no object |
| 23 | LUA_ERROR | failure | The handler or the extension raised |
| 24 | WRONG_CAMPAIGN | failure | The command was written for a campaign that is not the active one |
| 25 | TOO_LARGE | | The command ran and its value does not fit a record (spec/records.md). Whether that is a failure depends on the command's class (spec/errors.md). |
| 26 | WRONG_GENERATION | failure | The command was written for another mission generation |
| 27 | NOT_READY | failure | The Lua state its handler runs in has no shim loaded outside a mission load, or the pump asked for it never called `command` |

## Control commands

### Layout: CAMPAIGN

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | campaign | The active campaign ID, 0 for none |

When the ID differs from the agent's, the agent first removes from DCS every menu item,
mark and drawing it set and stops every extension. It then discards what it holds for
the old campaign: the binding, the applied revisions, the registered extensions, and
its record of menu items, marks and drawings. It keeps `epoch` and `last_applied_seq`;
the sidecar starts a new epoch with the switch.

The object table stays. It mirrors what is in DCS, not what a campaign owns, and a
switch that changes the world goes through a mission load, which empties it. Until
that load the old campaign's objects are reported with no `EntityId`.

With campaign 0 the agent samples and reports, and applies only the opcodes whose
Campaign column says `any`.

`ENUMERATE` has no payload after the header.

### Layout: RATES

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | position_s | Target period of the position tier, seconds |
| 0x0008 | f64 | vitals_s | Target period of the vitals tier |
| 0x0010 | pad[8] | reserved | |
| 0x0018 | f64 | warehouse_s | Target period of the warehouse and airbase tier |
| 0x0020 | f64 | enumeration_s | Period of the full enumeration |

A value of 0 keeps the default of spec/config.md. Rates are targets: the agent holds
its time slice and lets the rates fall, in this order: warehouses, then
vitals, then positions of objects no subscription names. Subscribed positions fall
last. The full enumeration keeps its period.

### Layout: SUBSCRIBE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | period_s | Target period for the listed entities' positions, seconds |
| 0x0008 | u32 | attitude | 1 to sample attitude with each position, else 0 |
| 0x000C | u32 | n | Entities that follow |
| + | u64[n] | entity | `EntityId`s. An ID the binding does not hold is ignored. |

The sidecar caps the period (spec/config.md). The agent's time slice holds whatever
the period: when the slice cannot meet it, the rate falls.

### Layout: BIND

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | n | Pairs that follow |
| 0x0004 | u32 | kind | `UNIT`, `STATIC` or `GROUP` (`object_kind`, spec/records.md): what every pair binds |
| + | u64[n] | entity | `EntityId`s |
| + | u64[n] | object | DCS object IDs, or DCS group IDs for `GROUP` |

A pair whose object is not in the agent's table, or whose group has no unit in it, is
ignored. The binding of an object or group with a platform name comes from the name
and cannot be changed. A group binding lets a `scope` of `GROUP` name a player's
group, which is unmanaged.

### Layout: BURST

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | on | 1 on, 0 off |
| 0x0004 | pad[4] | reserved | |

The burst also turns off when the lifecycle state leaves `PAUSED`.

## Set-state commands

### Layout: GROUP_SPAWN

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | country | DCS country number |
| 0x0004 | u32 | category | DCS group category number |
| + | str | name | The group's platform name (spec/world.md) |
| + | lv | group | The group table for `coalition.addGroup`, with platform names for the group and its units |

The sidecar compiles the table; the agent does not change it. The revision check has
already answered `AT_REVISION` for a `rev` at or below the applied one, so a group
that exists at this revision is never spawned again: that is the exists-guard.

A spawn that reaches the handler spawns, whether or not a group has the name. Under an
existing name it replaces the group. The agent then treats the old group as it does a
destroyed one: it writes `OBJECT_GONE` with `DESTROYED` for each old row and keeps the
old object IDs in the pending-destroy set (below). It sets the entity's
`applied_order_rev` to 0, because the new group has no task, and the sidecar sends the
order and the options again.

While the simulation is paused, `coalition.addGroup` under an existing name leaves
exactly the new group after a resume, with its new units: the old units are gone and
were written `DESTROYED` once (LIVE, step S15 exit test, agent build
`9590d6f6771c67a1`, DCS 2.9.31.29434 dedicated server under Wine 10.0, 2026-11-15).
The same replace while the simulation runs is PRIOR; step S18's restore checks it.

The result's `object` is the group's DCS ID, and an `OBJECT` record follows for each
unit.

### Layout: GROUP_ORDER

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | lv | task | The task table for the group's controller |

An order that resolves `refused` is not sent again at the same revision while DCS holds
none of the group's members. It is sent again when DCS gets back a member it lacked: a
revived member, a `MISSING` or `REMOVED` member found again, or a `lost` group found.

`GROUP_DESTROY`, `GROUP_CAPTURE_DESTROY`, `STATIC_DESTROY`, `MARK_REMOVE`,
`DRAWING_REMOVE` and `MENU_REMOVE` have no payload after the header. Destroying or
removing something that does not exist is `APPLIED`. Every destroy and every remove,
`EXTENSION_REMOVE` included, sets the applied revisions of what it removed to 0.

**A destroy while paused is pending.** `Group:destroy()` called while the simulation
is paused does not finish until it runs again; the group then goes within about a
second, with no event (LIVE, docs/results/2026-11-11-server-gate.md, E17). So after
its destroy call the agent:

- writes `OBJECT_GONE` with `DESTROYED` for each row and removes the rows, at once;
- keeps the object IDs in a pending-destroy set until `isExist()` is false for each, or
  raises, read through the existing per-pump existence checks;
- ignores a pending-destroy object in births and in a full enumeration. A `DEAD` event
  that names one is still written as an `EVENT`, removes nothing, and is counted in `pending_deaths`.

The set is cleared on a mission generation change and survives a resync `HELLO`. It
holds at most 65,536 IDs; an ID over that is not kept, and is counted in
`pending_overflow`.
An ID still existing after 60 s of unpaused model time is dropped and counted in `pending_expired`, so a
later enumeration can add the object back. A unit killed and then destroyed before an
existence check read its death is reported `DESTROYED`, not `DEAD`.

`GROUP_CAPTURE_DESTROY` returns the captured state as the result's `value`: a list
with one table for each living unit, or for the static, with these keys. A read that
raises leaves its key out.

| Key | Value |
|---|---|
| `name` | The unit's or static's DCS name |
| `id` | Its DCS object ID |
| `type` | Its type name |
| `x`, `y`, `z` | Position, meters |
| `heading`, `pitch`, `roll` | Attitude, radians, from `getPosition()` |
| `life`, `life0` | `getLife()` and `getLife0()` |

For a group that does not exist the value is nil. The sidecar never sends this command again
after a `refused`, `unknown` or `TOO_LARGE` outcome: it takes the group's last
checkpointed state and sends `GROUP_DESTROY`.

A static has no capture-and-destroy opcode. The sidecar dematerializes a static with
`CAPTURE` and, once it has the result, `STATIC_DESTROY`. If the capture fails it uses
the last checkpointed state.

### Layout: GROUP_OPTION

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | option | The DCS option ID |
| 0x0004 | pad[4] | reserved | |
| + | lv | value | The option's value |

The handler calls `setOption(option, value)` on the group's controller. A group that
does not exist is `NOT_BOUND`. The sidecar keeps each group's options with the group, not in its spec
(spec/world.md), and sends every option again after a spawn, as it does the order.

### Layout: STATIC_SPAWN

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | country | DCS country number |
| 0x0004 | pad[4] | reserved | |
| + | str | name | The static's platform name |
| + | lv | static | The table for `coalition.addStaticObject` |

The exists-guard applies as for a group.

A FARP static (`category` `Heliports`) also makes an airbase of its name with its own
warehouse, which the shim samples (spec/sampling.md; spec/world.md, "Kinds"). The table
takes these keys from `StaticSpec.extra`; the sidecar adds none of them:

| Key | Effect |
|---|---|
| `category` | `"Heliports"`, which makes the static a FARP. `StaticSpec` has no category field, so it comes from `extra`. Whether DCS makes the airbase without it is UNVERIFIED |
| `dynamicSpawn` | `true`: players may spawn aircraft at the FARP from its warehouse. Read only at spawn time; setting it later changes nothing (LIVE) |
| `allowHotStart` | `true`: those spawns may start hot. Whether dynamic spawn needs it is UNVERIFIED; the LIVE run set it |
| `dynamicCargo` | `true`: dynamic cargo at the FARP. Set in the LIVE run that offered a spawn; whether dynamic spawn needs it is UNVERIFIED |
| `hidden` | `false`: the static shows on the clients' F10 map. A static added by script without it is hidden there (LIVE) |

A dynamic spawn there takes an aircraft and fuel from the FARP's warehouse, which starts
empty: the controller stocks it with `SetWarehouseStock` under the static's name (LIVE,
a stocked FARP was offered and a spawn worked).

### Layout: AIRBASE_OWNER

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | coalition | DCS coalition number |
| 0x0004 | pad[4] | reserved | |
| + | str | airbase | The airbase's DCS name |

The handler calls `autoCapture(false)` and then `setCoalition(coalition)` on the
airbase (PRIOR; step S18's restore checks it): with automatic capture on, DCS
can hand the airbase back to the units around it. An airbase the platform set keeps
its owner until the next `AIRBASE_OWNER`. An airbase that does not exist is
`NOT_BOUND`.

### Layout: WAREHOUSE_SET

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | airbase | The airbase's DCS name |
| + | lv | stock | Names and counts to set. An item not named keeps its count. |

`stock` has the shape of a `WAREHOUSE` record's (spec/records.md): `weapon` and
`aircraft` map item names to counts, set with `setItem`; `liquids` maps the liquid
number (0 jet fuel, 1 gasoline, 2 MW-50, 3 diesel) to an amount, set with
`setLiquidAmount`. An airbase with no warehouse, or a name DCS does not have, is
`NOT_BOUND`. A call that raises is `DCS_REFUSED`.

On a limited warehouse each set reads back as set, for items and liquids (LIVE, E6,
step S15). An unlimited warehouse ignores a set without an error (LIVE, E6), so `APPLIED`
says only that DCS took the call. What it reads depends on the call (LIVE,
docs/results/2026-11-17-person-fixes.md, on the 26 airports the template file marks
unlimited): `getItemCount` of `weapons.bombs.Mk_82` and of `F-15C`, and
`getLiquidAmount` of liquids 0 to 3, read 1,000,000, while `getInventory`, which the
sampler reads, lists no weapon or aircraft of an unlimited kind and each liquid of
unlimited fuel at 100,000. Other items are UNVERIFIED. An `addItem` on an unlimited warehouse
adds an entry of its count to `getInventory` that a `removeItem` of the same count does
not take away; `getItemCount` stays at 1,000,000. Whether a `setItem` does the same is
UNVERIFIED. The reconciler then sees a sample that differs from the desired count; it
sends the set once per revision and mission generation, and never again for that
difference.

### Layout: WAREHOUSE_ADJUST

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | airbase | The airbase's DCS name |
| + | lv | delta | Names and amounts to add, in the shape of `WAREHOUSE_SET`'s `stock`. A negative amount takes away. |

An effect: applied at most once, never sent again. A positive amount uses `addItem` or
`addLiquid`, a negative one `removeItem` or `removeLiquid`. Taking more than a limited
warehouse holds leaves 0, with no error (LIVE, E6, step S15). A call that raises
stops the handler there: the items before it stay changed and the answer is
`DCS_REFUSED`; the sidecar takes those changes from the next sample, as facts.

### Layout: MARK_SET

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | scope | A `scope` code |
| 0x0004 | u32 | read_only | 1 if players cannot change it |
| 0x0008 | u64 | target | The coalition number or group `EntityId`, by scope. The scope is `ALL`, `COALITION` or `GROUP`. |
| 0x0010 | f64 | x | DCS `x`, meters |
| 0x0018 | f64 | y | DCS `y`, meters |
| 0x0020 | f64 | z | DCS `z`, meters |
| + | str | text | The mark's text |

### Layout: DRAWING_SET

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | scope | `ALL` or `COALITION` |
| 0x0004 | u32 | shape | The DCS shape ID |
| 0x0008 | u64 | target | The coalition number, by scope |
| + | lv | args | The shape's arguments after the coalition and the ID, as an array |

### Layout: MENU_SET

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u64 | parent | The parent menu item's `EntityId`, 0 for the root |
| 0x0008 | u64 | target | The coalition number or group `EntityId`, by scope |
| 0x0010 | u32 | scope | `ALL`, `COALITION` or `GROUP` |
| 0x0014 | u32 | submenu | 1 for a submenu, 0 for a command |
| + | str | text | The item's text |

Selecting a command item writes a `MENU` record (spec/records.md).

### Marks, drawings and menu items in DCS

The shim holds every mark, drawing and menu item it set, by `EntityId`. They are gone
after a mission load: the shim forgets them on a new generation, and the sidecar sets
them again, as it spawns groups again. A `GROUP` scope names the group by its platform
name; the handler reads the group's mission ID with `getID()`, and a group DCS does not
have is `NOT_BOUND`. DCS returns nothing from these calls, so `APPLIED` says only that
the call did not raise.

**Marks and drawings** share DCS's mark ID space (ED-CODE, `trigger.action.removeMark`
removes both). The shim gives each a DCS ID from its own counter, which starts at
2^30 so it stays clear of the IDs DCS gives the marks players make (UNVERIFIED: that
those start low). A replacement removes the held DCS mark first and adds the new one
under a new ID.

- `MARK_SET` calls `trigger.action.markToAll(id, text, point, read_only)`,
  `markToCoalition(id, text, point, coalition, read_only)` or
  `markToGroup(id, text, point, group, read_only)`.
- `DRAWING_SET` calls `trigger.action.markupToAll(shape, coalition, id, ...)` with the
  coalition number, -1 for `ALL`, and the array `args` unpacked after the ID. Points in
  `args` are tables with `x`, `y` and `z`, as DCS takes them. DCS checks the arguments
  for the shape: shape 1 with two points, colors, a line type, read only and a message
  is drawn, and shape 2 with a number after its first point is refused with
  `DCS_REFUSED` (LIVE, docs/results/2026-11-15-s16-players-moderation.md).
- `MARK_REMOVE` and `DRAWING_REMOVE` call `trigger.action.removeMark` on the held ID.
  An entity the shim does not hold is `APPLIED`: it is gone already.

**Menu items** use `missionCommands.addSubMenu` or `addCommand`, or their
`ForCoalition` or `ForGroup` form by scope. A submenu's path is what DCS returned for
it. An item with a parent is added under the parent's path; a parent the shim does not
hold, or one that is not a submenu or has another scope or target, is `NOT_BOUND`. A
command's callback is a function of the shim that calls the agent's `menu` with the
item's `EntityId` under `pcall`: it never raises into DCS.

- A replacement removes the old item first. DCS removes a submenu's items with it, so
  the shim adds the items it holds under that submenu again under the new path, in the
  order they were first set, at every depth. An item DCS refuses to add again is
  forgotten with every item below it, and the result, still `APPLIED`, has as its value
  the array of their `EntityId`s as decimal text; the sidecar sends those items again.
- `MENU_REMOVE` removes the item; for a submenu the shim also forgets every item it
  holds under it. The sidecar removes items before their submenu, so this happens only
  when the store and DCS disagree.
- A `GROUP`-scoped item stays on the group's DCS ID. When the sidecar spawns that group
  again, it sets the group's marks and menu items again.

### Codes: scope

| Code | Name | Target |
|---|---|---|
| 1 | ALL | none |
| 2 | COALITION | DCS coalition number |
| 3 | GROUP | group `EntityId` |
| 4 | UNIT | unit `EntityId` |
| 5 | COUNTRY | DCS country number; `SOUND`, `PICTURE` and `MESSAGE` only |

### Layout: EXTENSION_SET

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | name | The extension's name |
| + | lv | source | Lua source, a string |

Needs raw Lua. The applied revision is kept per
name. How an extension runs is in "Extensions" below.

### Layout: EXTENSION_REMOVE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | name | The extension's name |

## Effects

Positions are DCS `x`, `y`, `z` in meters.

### Layout: EXPLOSION

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | x | |
| 0x0008 | f64 | y | |
| 0x0010 | f64 | z | |
| 0x0018 | f64 | power | DCS explosion power |

### Layout: MESSAGE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | scope | A `scope` code |
| 0x0004 | u32 | clear | 1 to clear earlier messages |
| 0x0008 | u64 | target | By scope |
| 0x0010 | f64 | seconds | How long the text stays |
| + | str | text | The text |

The handler calls `trigger.action.outText(text, seconds, clear)`, or the
`outTextForCoalition`, `outTextForCountry`, `outTextForGroup` or `outTextForUnit` form
the scope names, with the coalition, country or mission ID first. A `GROUP` or `UNIT`
target DCS does not have is `NOT_BOUND`. The agent checks that `seconds` is finite and
from 0 to 86,400, else `BAD_PAYLOAD`.

### Layout: SOUND

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | scope | A `scope` code |
| 0x0004 | pad[4] | reserved | |
| 0x0008 | u64 | target | By scope |
| + | str | file | The sound file's name in the mission |

The handler calls `trigger.action.outSound`, or the `outSoundForCoalition`,
`outSoundForCountry`, `outSoundForGroup` or `outSoundForUnit` form the scope names,
with `file` as given. A `GROUP` or `UNIT` target is resolved by its platform name in
the command's campaign; with campaign 0, or when DCS does not have it, the answer is
`NOT_BOUND`. DCS accepts any file name without an error, a missing one included (LIVE,
step S15), so `APPLIED` says only that DCS took the call. Which form of the name plays
on a client, and whether a file outside the mission plays, is UNVERIFIED.

### Layout: PICTURE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | scope | A `scope` code |
| 0x0004 | u32 | clear_view | 1 to clear the view, the Mission Editor's "clear view" |
| 0x0008 | u64 | target | By scope |
| 0x0010 | f64 | seconds | How long the picture stays; 0 until `PICTURE_STOP` |
| 0x0018 | f64 | size | The picture's size, in `size_units` |
| 0x0020 | u32 | horizontal | The Mission Editor's horizontal alignment value |
| 0x0024 | u32 | vertical | The Mission Editor's vertical alignment value |
| 0x0028 | u32 | size_units | The Mission Editor's size-units value |
| 0x002C | pad[4] | reserved | |
| + | str | file | The picture's resource key in the mission, or its file name |

`PICTURE` and `PICTURE_STOP` run in the hook's state, because `a_out_picture` and its
scoped forms exist only in the `mission` state (LIVE, step S15), and are native, with no
Lua definition in ED's files. The handler resolves a `GROUP` or `UNIT` target as
`DAMAGE` does, with one `net.dostring_in('server', ...)` call (`NOT_BOUND` when DCS
does not have it, or with campaign 0), then makes one `net.dostring_in('mission', ...)`
call from a fixed template: `a_out_picture`, `a_out_picture_s`, `_c`, `_g` or `_u` by
scope, the target first for a scoped form, then `getValueResourceByKey(file)`, seconds,
clear view, a start delay of 0, horizontal, vertical, size and size units, the order of
the Mission Editor's fields (ED-CODE, `me_trigrules.lua`). A key DCS does not know
resolves to itself, and DCS raises nothing for a missing picture (LIVE), so `APPLIED`
says only that DCS took the call. A `COALITION` target goes as the Mission Editor's
coalition name, `"neutrals"`, `"red"` or `"blue"` for 0, 1 and 2 (ED-CODE,
`MissionEditor/modules/Mission/CoalitionData.lua`): `a_out_picture_s` shows nothing for a
number and shows the picture for `"blue"` (LIVE, a client). The handler answers
`DCS_REFUSED` for a coalition above 2, with no call. A `COUNTRY` target goes as the DCS
country number, the value the Mission Editor's country list writes (ED-CODE,
`me_trigrules.lua`, `countryLister`); a client showing it is UNVERIFIED. The alignment
and unit values are UNVERIFIED.

`PICTURE_STOP` has no payload after the header and calls `a_out_picture_stop()`.

### Layout: SMOKE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | x | |
| 0x0008 | f64 | y | |
| 0x0010 | f64 | z | |
| 0x0018 | u32 | color | DCS smoke color number |
| 0x001C | pad[4] | reserved | |

### Layout: FLARE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | x | |
| 0x0008 | f64 | y | |
| 0x0010 | f64 | z | |
| 0x0018 | f64 | azimuth | Radians |
| 0x0020 | u32 | color | DCS flare color number |
| 0x0024 | pad[4] | reserved | |

### Layout: ILLUMINATION

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | x | |
| 0x0008 | f64 | y | |
| 0x0010 | f64 | z | |
| 0x0018 | f64 | power | DCS illumination power |

### Layout: CLEAR_DEBRIS

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | volume | A `volume` code |
| 0x0004 | pad[4] | reserved | |
| 0x0008 | f64 | x | A sphere's center, or a box's lowest corner |
| 0x0010 | f64 | y | |
| 0x0018 | f64 | z | |
| 0x0020 | f64 | x2 | A box's highest corner; 0 for a sphere |
| 0x0028 | f64 | y2 | |
| 0x0030 | f64 | z2 | |
| 0x0038 | f64 | radius | A sphere's radius in meters; 0 for a box |

### Codes: volume

| Code | Name | Meaning |
|---|---|---|
| 1 | SPHERE | `x`, `y`, `z` and `radius` |
| 2 | BOX | From `x`, `y`, `z` to `x2`, `y2`, `z2`, aligned with the axes |

The handler calls `world.removeJunk` with a DCS volume of the code's shape. It then
lists the statics in the same volume with `world.searchObjects` and collects each whose
`getLife()` reads 0 or less and whose `isExist()` reads false, as a dead static's do
(LIVE), or raises (UNVERIFIED in DCS; host-tested), and whose name reads and does not
start with `np#`; a static whose name cannot be read stays (spec/world.md, "Names"). After the
search returns it calls `destroy` on each, under `pcall`: `world.removeJunk` leaves a
destroyed static, and a client keeps showing its ruin until `Object.destroy` removes it
(LIVE, docs/results/2026-11-16-person-tests-2.md). A destroy that raises is skipped and
not counted. A static the platform manages, dead or alive, is never destroyed, so a
controller can keep its wreck or ruin on purpose: its record and death fact are
unchanged. Only the dead statics nobody manages, placed by the mission or by a script,
go.

The value is the number `world.removeJunk` returns, how many junk objects it removed
(craters and debris as well as wrecks), plus the statics destroyed. A static a destroy
left where it was is destroyed again by each later clear and counted once a mission
generation. The agent sends it as the `RESULT`'s value. A `world.removeJunk` call that
raises is `DCS_REFUSED` with its text; otherwise `APPLIED`.

The agent checks the payload, else `BAD_PAYLOAD`: a known `volume`, every number
finite and within 10,000 km of 0, a sphere's `radius` above 0, and a box's `x`, `y`, `z`
each at most its `x2`, `y2`, `z2`.

What DCS removes (LIVE, docs/results/2026-11-16-clear-debris.md): the wrecks of ground
units that have stopped burning. A clear on open ground where an explosion struck
returns 1, so a crater counts as junk (inferred from the count). `world.removeJunk`
leaves every live unit and static, a wreck that is still burning, a dead static's object
(which the handler then destroys), and destroyed scenery. A clear around a dead aircraft
or a dead static returns a count above 0; what it removes there is UNVERIFIED. A burning wreck it leaves still explodes and sends its death
events, so a clear changes nothing in spec/records.md. A wreck it removes reads as a
removed object: `isDead`, `getLife` and `getPoint` raise. The volume's edge is not exact
to an object's position. A 2,000 km sphere removed the wrecks of a whole run in one
call, the 250 km boxes below removed them too, and DCS accepts a 10,000 km sphere and a
2,000 km box.

The cost: the `world.removeJunk` call alone takes about 4 us per object it removes and
4 to 6 us when it removes nothing, at any size. A `CLEAR_DEBRIS` command's `cost_us`
includes the pump around it: 20 us at the median for a box that removes little, 37 us
at p99, 981 us at most, and 6.4 ms in all for a whole-map clear of 217 objects, spread
over 4 frames (LIVE, run 3 of the results). These figures were measured before the
handler searched the volume for dead statics. The search, a `getLife` and `isExist` on
each static in the volume and a destroy of each dead one add a cost that is UNVERIFIED.

A clear of the whole map is not one command, so that no single command removes the junk
of the whole map at once. The sidecar sends it as boxes 250 km square that tile x and z
from -2,000 km to 2,000 km, 256 commands, each from 10 km below sea level to 100 km above
it. That this reach covers every theater is UNVERIFIED: only Caucasus and Afghanistan
were run, and junk outside it is left without an error. They go through the ring under the commanded-work budget like any other command. The
call has one outcome, settled when every box sent has answered: the outcome of the first
box, by sequence number, that was not `APPLIED`, carrying the sum of the applied boxes'
counts when it has a result; otherwise `not_sent` when a box was never sent, because the
agent detached; otherwise `APPLIED` with the sum of the counts. The sidecar keeps what it
needs to fold the boxes in memory, so a clear in flight when the sidecar restarts
settles as `unknown`, a clear of one volume as well.

### Layout: DAMAGE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | life_percent | The life to set, as a percentage of the unit's initial life, 0 to 100 |

`DAMAGE` runs in the hook's state, because `a_unit_set_life_percentage` exists only in
the `mission` state and neither the `server` nor the `mission` state has `net` (LIVE).
The agent checks that `life_percent` is finite and from 0 to 100, else `BAD_PAYLOAD`,
and passes it to the handler as text it formats itself. The handler checks `name`
against the platform unit form, then makes one `net.dostring_in('server', ...)` call
that returns the unit's `getID()`; a unit that does not exist is `NOT_BOUND`, with no
`mission` call, because the setter revives a dead unit (LIVE, E38). It then makes one
`net.dostring_in('mission', ...)` call from a fixed template, with the ID formatted as
an integer and a `c_unit_dead` guard: a dead unit is `NOT_BOUND`. A call that fails is
`DCS_REFUSED` with its text; otherwise `APPLIED`.

What the setter does (LIVE, E16, step S15): at 50 % an aircraft, a helicopter and a
ground unit read back half their initial life. At 0 they leave DCS; only an airborne
aircraft logged an event (`UNIT_LOST`, `EJECTION`). The one ship type tried, a
speedboat, left DCS at 50 % and stayed at 80 %; a PERRY frigate read back 80 % and 50 %
(LIVE, E39). At 50 % the setter did nothing to a static (LIVE, E16), which is why
`DAMAGE` takes only a unit. At 0 it killed a `BRDM-2` static on the server, and a
connected client kept showing it alive (LIVE, docs/results/2026-11-17-person-fixes.md);
what it does to a static at other settings and for other types is UNVERIFIED. The sidecar also queues a `DAMAGE` right after a `GROUP_SPAWN` to give a unit
its stored life (spec/world.md, "Spawn state"); E39 found it applies in the spawn's
frame while paused, and later with the simulation running; the running same-frame case
is UNVERIFIED. On a dedicated server a setting of 10 killed a ground unit and the
death did not reach a connected client (LIVE,
docs/results/2026-11-11-first-dll-and-spikes.md, E18).

### Layout: CHAT

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | player | The DCS connection ID to send to, 0 for everyone |
| 0x0004 | pad[4] | reserved | |
| + | str | text | The message |

The handler calls `net.send_chat(text, true)` for player 0 and
`net.send_chat_to(text, player)` otherwise. DCS returns nothing useful, so `APPLIED`
says only that the call did not raise; a player who is not connected gets nothing.
The agent refuses an empty `text` or one over 1,024 bytes with `BAD_PAYLOAD`.

### Layout: MISSION_LOAD

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | path | The mission file's path as DCS opens it |

The agent queues the result, publishes the frame's records, and only then calls the
load. It handles no further command in that frame. The lifecycle then goes to
`LOADING`.

The agent refuses an empty `path` or one over 1,024 bytes with `BAD_PAYLOAD`. The
hook's handler checks the same again, else `DCS_REFUSED`, as for `PICTURE`, and answers
`APPLIED`; it hands the path back to the dispatch, which takes no further command, and
the hook calls `net.load_mission(path)` after `finish`. `net.load_mission` returns at
once and DCS loads on a later frame (LIVE, docs/results/2026-11-11-server-gate.md). A
load DCS cannot do is not in the result: it shows as no `LOADING`, and as the hook's
log line with what `net.load_mission` returned. The command is an effect and is never
sent again.

### Layout: EXTENSION_INVOKE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | name | The extension's name |
| + | lv | args | An array of arguments |

Calls the extension's `invoke` with the arguments. The result's `value` is the first
value it returns. An extension with no `invoke` answers `LUA_ERROR`.

### Layout: EVAL

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | lv | source | Lua source, a string |

Needs raw Lua. The result's `value` is the first value the chunk returns.

The shim compiles the source with `loadstring` and runs it under `pcall` in a new
environment whose reads fall through to the `server` state's globals, so an assignment
to a global stays in that environment and cannot replace one of the shim's. State
carries from one chunk to the next only through explicit writes to `_G`. `emit` is in
it. A chunk that does not compile, or raises, is `LUA_ERROR` with the error text. Its
time is charged to the commanded-work budget like any handler; a chunk that loops is
not interrupted.

## Queries

`CAPTURE` has no payload after the header. The result's `value` is the state
`GROUP_CAPTURE_DESTROY` returns, and nothing is destroyed. A group or static that does
not exist is `NOT_BOUND`.

`COUNTRIES` has no payload after the header. The result's `value` maps each DCS
country number to its coalition number in the loaded mission, from
`coalition.getCountryCoalition` for each value of `country.id`, each read under `pcall`.
A country whose read raises or returns no number is left out. The sidecar sends it
whenever it holds no map for the current mission generation and a mission is loaded: a
new generation, a sidecar restart, or a lost answer. A resync `HELLO` within one
generation keeps the map. The API reports a materialized object's coalition from its
`OBJECT` records and uses the map for groups and statics that are not materialized;
before the answer their coalition is unspecified. A disagreement between the two is
counted.

## Extensions

An extension is a named Lua chunk that runs in the `server` state. It needs raw Lua.
Custom logic that must run inside DCS goes here: anything DCS does not report by
itself, built from its events and its scripting API.

### Running

When `EXTENSION_SET` is applied the agent compiles the source and runs the chunk once,
in its own environment. The chunk may return a table of hooks:

| Hook | Called | Arguments |
|---|---|---|
| `invoke(...)` | by `EXTENSION_INVOKE` | the command's arguments |
| `on_event(event)` | from the pump, once for each world event since the last call | a copy of the event table |
| `on_frame(model_time)` | from the pump, at most once per frame, while the lifecycle state is `RUNNING` | model time, seconds |
| `on_stop()` | before the extension is replaced or removed, on a campaign switch, and when the simulation stops | none |

Every hook is optional. The agent calls every hook under `pcall`, with the breadcrumb
set to the extension, and from its pump. `on_stop` at the end of a mission is the one
exception: no frame follows, so the agent calls it from `onSimulationStop`, the last
callback in which the mission is still there, through the cross-state call.

**Events.** The agent's own handler does what it always does: it reads plain fields
and calls no method. For extensions it also makes a shallow copy of the event table,
field by field, and queues the copy. The copy holds the same objects DCS put in the
event: `initiator`, `target`, `weapon`. `on_event` runs later, from the pump, and may
call methods on those objects. An object may be gone by then, and a method may raise;
the `pcall` contains a raise. Each extension's queue holds 256 events; when it is full
the oldest is dropped and counted in `events_skipped` (spec/records.md).

**Budget.** The time of every hook is charged to the commanded-work budget and
reported per extension in `EXTENSION` records. After the frame's commands, the agent
calls hooks only while the frame's budget has time left: first queued events, then
`on_frame`. The extensions take turns to go first, so a slow extension slows itself
before the others. An `on_frame` call that does not fit is skipped and not made up. A
hook must return: the agent cannot interrupt one that loops.

Hooks run only in a pump. While any extension has queued events, waiting callbacks or
an `on_frame` hook, the shim tells the agent so through `extension_budget`, and the
agent asks for a pump on every frame of a `RUNNING` or `PAUSED` mission, never on the
frame that makes its system calls. Such a pump samples only on a sampling frame; its
cross-state call is charged to the commanded-work budget. With no extension holding
work, the pump runs as before.

**Errors.** A hook that raises is logged and counted. After 10 errors in a row from one
extension the agent stops calling its hooks and writes a log record and an `EXTENSION`
record that say so. Only `EXTENSION_SET` with a newer revision, or the next mission
start, starts them again.

**The environment.** The chunk's global table reads through to the state's globals,
with these replaced, so that nothing an extension starts runs outside `pcall`, outside
the budget, or after the extension is gone:

| Global | In an extension |
|---|---|
| `world.addEventHandler`, `world.removeEventHandler` | Raise, with a message that points to `on_event`. DCS's dispatcher has no `pcall`: a handler that raises stops every handler after it, the agent's included (PRIOR). |
| `timer.scheduleFunction`, `timer.removeFunction`, `timer.setFunctionTime` | Go through the agent, which runs the callback from the pump as a hook and cancels it when the extension stops |
| `missionCommands.addCommand`, `addCommandForCoalition`, `addCommandForGroup` | Go through the agent, which runs the callback as a hook and removes the item when the extension stops |
| `missionCommands.addSubMenu`, `addSubMenuForCoalition`, `addSubMenuForGroup` | Go through the agent, which tracks the submenu and removes it when the extension stops |
| `missionCommands.removeItem`, `removeItemForCoalition`, `removeItemForGroup` | Go through the agent, which forgets the item and its children |
| `emit` | Bound to the extension: its time is charged to the extension's hook |

Every other global reads through to the state's own. These are all the replaced
globals. An extension that reaches the real functions another way, such as through
`_G`, is outside these guarantees.

### State

| When | What happens |
|---|---|
| `EXTENSION_SET` with a newer revision | The agent compiles the new source and runs its chunk. If either fails, the result is `LUA_ERROR`, and the old extension keeps running at its revision. Otherwise `on_stop` of the old one, and the new one takes its place. |
| The simulation stops (a mission ends or is about to be reloaded) | `on_stop` |
| A mission starts | Every registered extension's chunk runs again with a new environment. Nothing an extension held in Lua survives a mission load. |
| A campaign switch, `EXTENSION_REMOVE` | `on_stop`, then the extension is gone |
| A sidecar restart | Nothing: the extensions keep running |
| DCS restarts | The agent holds none. The sidecar registers them again from the store. |

Lua holds no lasting state. An extension reports what must last with `emit`, and the
controller gives it back after a load by invoking the extension with it.

### emit

`emit(name, payload, objects)` is a global in the `server` state when raw Lua is on. It
queues an `AUTHOR_EVENT` record (spec/records.md), which is event class: it is held in
the agent's queue when the ring is full, and a loss is a `GAP`, never silent.

| Argument | Type | Rule |
|---|---|---|
| `name` | string | 1 to 128 bytes |
| `payload` | nil, boolean, number, string or table | Encoded as `lv`. A function, userdata or thread inside it becomes its type name. At most 16 KiB encoded, and 16 tables deep. |
| `objects` | nil, or an array of at most 16 units | Each entry is a unit object, or the number in its `id_` field, which is the object ID the agent's table uses. `id_` differs from what `getID()` returns (LIVE, docs/results/2026-11-15-s16-players-moderation.md). The sidecar stamps the event with the player keys occupying those units, as it does for a world event. |

`emit` never raises. It returns `true`, or `false` and a reason: `name`, `too_large`,
`too_deep` (a table in itself counts), `objects` (not an array of at most 16, or an
entry that is not in the agent's table) or `disabled`. Its time is charged to the hook
that called it, or to the agent's overhead when no hook is running.

`emit` is a Lua function of the shim that calls the agent's `emit` entry under `pcall`
and turns a raise into `false` and the error text. The entry reads `payload` with raw
table access only, inside the worker that runs under `lua_pcall`, so no metamethod runs
and no Lua error crosses a Rust frame that owns a value.

## Moderation

`MODERATE` runs in the hook's state, where `net` is. `net.kick`, `net.banlist_add`,
`net.banlist_add_by_ucid`, `net.banlist_remove`, `net.force_player_slot` and
`net.get_player_info` are functions there (LIVE, `lua/mock/api/hook.txt`, written by
`lua/probes/tools/api-dump.lua` on DCS 2.9.31.29434 under Wine, 2026-11-11).
`net.kick(player, reason)`, `net.banlist_add(player, seconds, reason)`,
`net.banlist_remove(ucid)` and `net.force_player_slot(player, 0, '')` do what the
table below says, and a ban survives a DCS restart (LIVE, E34,
docs/results/2026-11-15-s16-players-moderation.md). `net.banlist_add_by_ucid`'s
argument order is UNVERIFIED. The sidecar names a player to the agent by connection ID
and ucid; the API names a player by player key (spec/world.md, "Players"). The API
refuses a ban of a player with no connection, a ban of seconds 0 and a move into a slot
with `UNIMPLEMENTED` and reason `UNVERIFIED` until E34 verifies them.

### Layout: MODERATE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | action | A `moderation` code |
| 0x0004 | u32 | player | The DCS connection ID, 0 for none |
| 0x0008 | u32 | side | The coalition to move to, for `SLOT`; 0 for spectators |
| 0x000C | pad[4] | reserved | |
| 0x0010 | f64 | seconds | How long a `BAN` lasts, 0 for DCS's default |
| + | str | ucid | The client's ucid |
| + | str | slot | The slot ID to move to, for `SLOT`; empty for spectators |
| + | str | reason | Shown to the player, for `KICK` and `BAN` |

The handler gets `action`, `player`, `side`, `seconds`, `ucid`, `slot` and `reason`.
The agent refuses with `BAD_PAYLOAD` an action it does not know, an empty `ucid`, a
`seconds` that is not finite or is below 0, and player 1, which is the server itself.

For every action that names a player (`KICK`, `SLOT`, and `BAN` with a player) the
handler first reads `net.get_player_info(player, 'ucid')`. A player who is not
connected, or whose ucid differs from `ucid`, is `NOT_BOUND`: the connection ID now
belongs to someone else, or to no one.

### Codes: moderation

| Code | Name | Call |
|---|---|---|
| 1 | KICK | `net.kick(player, reason)` |
| 2 | BAN | `net.banlist_add(player, seconds, reason)`; with player 0, `net.banlist_add_by_ucid(ucid, seconds, reason)` |
| 3 | UNBAN | `net.banlist_remove(ucid)` |
| 4 | SLOT | `net.force_player_slot(player, side, slot)` |

A call that raises is `DCS_REFUSED`; one that returns `false` is `DCS_REFUSED` with the
message `DCS returned false`. Otherwise `APPLIED`. `MODERATE` is an effect: a result
that is lost is unknown, and the command is never sent again. Each `APPLIED` one is an
event in the API's stream with the token that asked for it.

### Player and ban queries

`PLAYERS` has no payload after the header. Its value is an array with one table for each
connected player but the server, from `net.get_player_list()` and
`net.get_player_info(id)`: `id`, `ucid`, `name`, `side` and `slot`. `slot` is text,
as in a `PLAYER` record: a number DCS gives is written in decimal, and a spectator's is
empty. While a campaign is active, the sidecar sends it after every `HELLO` and every
new mission generation, so a controller that attaches reads the slot changes it missed.
With no campaign active it is not sent; the roster then follows the `PLAYER` records
alone.

`BANS` has no payload after the header. Its value is what `net.banlist_get()` returns,
each entry with its `ucid`, `name`, `banned_until` and `reason` fields only; any other
field DCS returns, such as an address, is left out.

A ucid travels from the agent to the sidecar and no further: the API, the event stream,
logs, metrics and bundles carry the player key instead (spec/world.md, "Players"). No
handler writes a ucid into a `RESULT` message or a log record.
