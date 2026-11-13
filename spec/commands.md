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

The command epoch is a counter the sidecar owns and keeps in the server-scoped store.
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

A set-state command answered `STALE_EPOCH`, `WRONG_CAMPAIGN` or `WRONG_GENERATION` is
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
An effect or a client's query is not sent again; for a set-state command the sidecar
derives what DCS still needs from the store (spec/errors.md).

**After `CONTROL_LOST`** the ring is untouched: a command after the `HELLO`'s `in_head`
is still there and stays `pending`.

**After DCS dies** no `HELLO` comes from the old process. The sidecar uses the
largest `in_head` it read from the status block for that `agent_boot`: a command at or
before it is `unknown`, one after it was never taken. `in_head` moves before each
command runs, so a command that was running when DCS died, and may be why it died, is
`unknown` and never `not_sent`. The breadcrumb names it (`crumb_arg`), and the sidecar
reports it as the command DCS died in. If the sidecar was not running when DCS died
and finds a new `agent_boot`, every unanswered command is `unknown`.

**After the kill switch** the agent writes no further `HELLO`, so the sidecar resolves
its commands as after DCS dies, by the largest `in_head` it read for that
`agent_boot`. `CONTROL_LOST` stays set in the status block if a control record was
lost.

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
`cost_us` in the result is the time around the handler. A command over the budget is
counted in `commands_over_budget`.

### Unknown opcodes

An agent that reads an opcode it has no handler for cannot know its class. It goes by
`seq`: with `seq` 0 it skips the command and writes a `RESULT` with `UNKNOWN_OPCODE`;
otherwise the command goes through the epoch, duplicate and campaign rows as any
other, is answered `UNKNOWN_OPCODE`, and sets `last_applied_seq`.

### Codes: opcode

| Code | Name | Class | Entity | Rev | Campaign | Gen | Payload | Meaning |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 0 | PADDING |  |  |  |  |  |  | Fills the end of the ring |
| 1 | CAMPAIGN | control |  |  |  |  | layout | Announce the active campaign |
| 2 | ENUMERATE | control |  |  |  |  | none | Start a full enumeration now |
| 3 | RATES | control |  |  |  |  | layout | Set the target period of each sampling tier |
| 4 | SUBSCRIBE | control |  |  |  |  | layout | Replace the set of objects sampled at a higher rate |
| 5 | BIND | control |  |  |  |  | layout | Give `EntityId`s to unmanaged objects |
| 6 | BURST | control |  |  |  |  | layout | Turn the restore burst on or off |
| 16 | GROUP_SPAWN | set-state | group | spec | active | bound | layout | Spawn a group |
| 17 | GROUP_ORDER | set-state | group | order | active | bound | layout | Give a group its order |
| 18 | GROUP_DESTROY | set-state | group |  | active | bound | none | Destroy a group |
| 19 | GROUP_CAPTURE_DESTROY | set-state | group |  | active | bound | none | Read a group's final state, return it, and destroy the group, in one frame |
| 20 | GROUP_OPTION | set-state | group |  | active | bound | layout | Set one controller option |
| 21 | STATIC_SPAWN | set-state | static | spec | active | bound | layout | Spawn a static |
| 22 | STATIC_DESTROY | set-state | static |  | active | bound | none | Destroy a static |
| 23 | AIRBASE_OWNER | set-state |  |  | active | bound | layout | Set an airbase's owner |
| 24 | WAREHOUSE_SET | set-state |  |  | active | bound | layout | Set a warehouse's stock |
| 25 | MARK_SET | set-state | mark | spec | active | bound | layout | Add or replace a map mark |
| 26 | MARK_REMOVE | set-state | mark |  | active | bound | none | Remove a map mark |
| 27 | DRAWING_SET | set-state | drawing | spec | active | bound | layout | Add or replace a map drawing |
| 28 | DRAWING_REMOVE | set-state | drawing |  | active | bound | none | Remove a map drawing |
| 29 | MENU_SET | set-state | menu item | spec | active | bound | layout | Add or replace an F10 menu item |
| 30 | MENU_REMOVE | set-state | menu item |  | active | bound | none | Remove an F10 menu item |
| 31 | EXTENSION_SET | set-state |  | spec | active |  | layout | Register or replace a Lua extension |
| 32 | EXTENSION_REMOVE | set-state |  |  | active |  | layout | Remove a Lua extension |
| 48 | EXPLOSION | effect |  |  | active | bound | layout | An explosion at a point |
| 49 | MESSAGE | effect |  |  | any |  | layout | Text on screen |
| 50 | SOUND | effect |  |  | any |  | layout | Play a sound file from the mission |
| 51 | SMOKE | effect |  |  | active | bound | layout | A smoke marker at a point |
| 52 | FLARE | effect |  |  | active | bound | layout | A signal flare at a point |
| 53 | ILLUMINATION | effect |  |  | active | bound | layout | An illumination bomb at a point |
| 54 | DAMAGE | effect | unit |  | active | bound | layout | Set a unit's life |
| 55 | CHAT | effect |  |  | any |  | layout | A chat message from the server |
| 56 | MISSION_LOAD | effect |  |  | any |  | layout | Load a mission file |
| 57 | EXTENSION_INVOKE | effect |  |  | active |  | layout | Run a registered extension |
| 58 | EVAL | effect |  |  | active |  | layout | Run Lua source in the `server` state |
| 64 | CAPTURE | query | group or static |  | active | bound | none | Read an entity's full state and return it |

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
| 0x0010 | f64 | ammo_s | Target period of the ammunition tier |
| 0x0018 | f64 | warehouse_s | Target period of the warehouse and airbase tier |
| 0x0020 | f64 | enumeration_s | Period of the full enumeration |

A value of 0 keeps the default of spec/config.md. Rates are targets: the agent holds
its time slice and lets the rates fall, in this order: ammunition and warehouses, then
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

That `coalition.addGroup` under an existing name replaces the group is PRIOR; step S15
checks it, running and paused.

The result's `object` is the group's DCS ID, and an `OBJECT` record follows for each
unit.

### Layout: GROUP_ORDER

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | lv | task | The task table for the group's controller |

`GROUP_DESTROY`, `GROUP_CAPTURE_DESTROY`, `STATIC_DESTROY`, `MARK_REMOVE`,
`DRAWING_REMOVE` and `MENU_REMOVE` have no payload after the header. Destroying or
removing something that does not exist is `APPLIED`. Every destroy and every remove,
`EXTENSION_REMOVE` included, sets the applied revisions of what it removed to 0.

**A destroy while paused is pending.** `Group:destroy()` called while the simulation
is paused does not finish until it runs again; the group then goes within about a
second, with no event (LIVE, docs/results/2026-11-11-server-gate.md, E17). So after
its destroy call the agent:

- writes `OBJECT_GONE` with `DESTROYED` for each row and removes the rows, at once;
- keeps the object IDs in a pending-destroy set until `isExist()` is false for each;
- ignores a pending-destroy object in events and in a full enumeration.

Whether a spawn under the name of a group whose destroy is pending leaves exactly the
new group after a resume is UNVERIFIED; step S15 checks it.

`GROUP_CAPTURE_DESTROY` returns the captured state as the result's `value`: for each
living unit its position, attitude, life, initial life, fuel and ammunition. For a
group that does not exist the value is nil. The sidecar never sends this command again
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

### Layout: STATIC_SPAWN

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | country | DCS country number |
| 0x0004 | pad[4] | reserved | |
| + | str | name | The static's platform name |
| + | lv | static | The table for `coalition.addStaticObject` |

The exists-guard applies as for a group.

### Layout: AIRBASE_OWNER

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | coalition | DCS coalition number |
| 0x0004 | pad[4] | reserved | |
| + | str | airbase | The airbase's DCS name |

### Layout: WAREHOUSE_SET

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | airbase | The airbase's DCS name |
| + | lv | stock | Names and counts to set. An item not named keeps its count. |

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

The agent holds every mark, drawing and menu item it set. They are gone after a
mission load; the sidecar sets them again, as it spawns groups again.

### Codes: scope

| Code | Name | Target |
|---|---|---|
| 1 | ALL | none |
| 2 | COALITION | DCS coalition number |
| 3 | GROUP | group `EntityId` |
| 4 | UNIT | unit `EntityId` |

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

### Layout: SOUND

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | scope | A `scope` code |
| 0x0004 | pad[4] | reserved | |
| 0x0008 | u64 | target | By scope |
| + | str | file | The sound file's name in the mission |

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

### Layout: DAMAGE

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | f64 | life_percent | The life to set, as a percentage of the unit's initial life, 0 to 100 |

Damage goes through `a_unit_set_life_percentage` in the `mission` state with one
cross-state call. On a dedicated server a setting of 10 killed a ground unit and the
death did not reach a connected client (LIVE,
docs/results/2026-11-11-first-dll-and-spikes.md, E18). Step S15 (E16) decides what the
platform offers.

### Layout: CHAT

| Offset | Type | Field | Meaning |
|---|---|---|---|
| 0x0000 | u32 | player | The DCS connection ID to send to, 0 for everyone |
| 0x0004 | pad[4] | reserved | |
| + | str | text | The message |

### Layout: MISSION_LOAD

| Offset | Type | Field | Meaning |
|---|---|---|---|
| + | str | path | The mission file's path as DCS opens it |

The agent queues the result, publishes the frame's records, and only then calls the
load. It handles no further command in that frame. The lifecycle then goes to
`LOADING`.

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

## Queries

`CAPTURE` has no payload after the header. The result's `value` is the state
`GROUP_CAPTURE_DESTROY` returns, and nothing is destroyed.

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
| `timer.scheduleFunction`, `timer.removeFunction` | Go through the agent, which runs the callback from the pump as a hook and cancels it when the extension stops |
| `missionCommands` functions that take a callback | Go through the agent, which runs the callback as a hook and removes the item when the extension stops |

The full list of replaced globals is fixed in step S16. An extension that reaches the
real functions another way, such as through `_G`, is outside these guarantees.

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
| `objects` | nil, or an array of at most 16 units | Each entry is a unit object, or the number in its `id_` field, which is the object ID the agent's table uses. That `id_` differs from what `getID()` returns is UNVERIFIED (step S16). The sidecar stamps the event with the player keys occupying those units, as it does for a world event. |

`emit` never raises. It returns `true`, or `false` and a reason: `name`, `too_large`,
`too_deep` (a table in itself counts), `objects` (not an array of at most 16, or an
entry that is not in the agent's table) or `disabled`. Its time is charged to the hook
that called it, or to the agent's overhead when no hook is running.
