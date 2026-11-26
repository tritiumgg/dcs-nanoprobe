# Controller guide

A controller is a program that runs a campaign through the gRPC API. This guide gives
the model, every service, the rules for errors and retries, and recipes. It sits on top
of the schema and the specifications. For exact fields read the schema in
[proto/nanoprobe/v1alpha1/](../proto/nanoprobe/v1alpha1/); for exact behavior read
[spec/world.md](../spec/world.md) and [spec/errors.md](../spec/errors.md). Where this
guide and those files differ, they win.

The API is `nanoprobe.v1alpha1`. A release may change it in ways that break generated
code until it leaves alpha. [api-clients.md](api-clients.md) shows how to generate a
client in any language. An AI coding tool can load the skill in
[skills/nanoprobe/](../skills/nanoprobe/), which points at this guide;
[its README](../skills/nanoprobe/README.md) says where each tool looks for it.

## The model

**Server, sidecar and agent.** The agent runs inside DCS. The sidecar runs beside DCS,
stores the world, and serves the API on `127.0.0.1:47311` by default. A controller talks
only to the sidecar.

**Campaign.** A campaign is one persistent world with its own store. Several can exist
on a server; at most one is active. A campaign is `ACTIVE`, `SUSPENDED` or `FINISHED`.
Activating one writes its mission from the map's template, loads it in DCS, restores
the campaign's groups, and suspends the one that was active. Suspending one captures
its groups, checkpoints and closes its store; DCS keeps running, unmanaged. A finished
campaign is read-only.

**Entities.** The world holds groups and their units, statics, airbases with their
warehouses, marks, drawings, menu items and extensions. The sidecar gives each
managed entity an `EntityId` that is unique in its campaign and never reused. The API
never shows DCS object IDs. [spec/world.md](../spec/world.md), "Kinds", says what
each kind carries.

**Managed and unmanaged.** A managed entity was made by the platform from a spec. An
unmanaged one was not: a player's aircraft, a unit placed in the Mission Editor, a
dynamic-spawn aircraft. Unmanaged entities are observed and reported, never ordered,
reconciled or destroyed. The sidecar adopts the units players occupy, and their
groups, so a controller can send them effects, marks, drawings and menu items. A call
that changes the spec or order of an unmanaged entity fails with `UNMANAGED_ENTITY`.

**Desired state and the reconciler.** A set-state call writes desired state to the
store and returns. It does not wait for DCS and it succeeds while DCS is absent. The
sidecar's reconciler then sends DCS what it needs to match the store. It corrects drift
(a group that should exist and does not, an order DCS has not been given). It never
undoes a fact (a death, a kill, a captured airbase, stock DCS used).

**Revisions.** Every change to a campaign's store moves its store revision. The World
service stamps every snapshot and change with it. Each group, static, mark, drawing and
menu item also has a `spec_revision`, and each group an `order_revision`. Beside them
the World service reports `applied_spec_revision` and `applied_order_revision`: what DCS
has confirmed. A controller knows DCS holds its change when the applied revision
reaches the one its call returned. A group or static that DCS refused shows the error
in `last_error`.

**Lifecycle.** A managed group or static is `VIRTUAL`, `MATERIALIZING`, `LIVE`,
`DEMATERIALIZING`, `DEAD`, `LOST` or `UNSPAWNABLE` (DCS refused the spawn; it is not
tried again until the spec changes). `DEAD` is final. `LOST` means DCS no
longer holds it and no death explains why. [spec/world.md](../spec/world.md),
"Lifecycle", has every transition.

**Authority.** Each managed group and static is simulated by DCS (materialized) or by
the campaign (virtual). Only the controller moves it, with `Materialize` and
`Dematerialize`; a `GroupSpec` with `virtual` set starts it virtual. A dematerialize
captures each unit's position, heading and life in the frame that destroys it; a
materialize spawns from the stored state. The sidecar holds no rule on when to move:
that policy is the controller's.

**Mission generations and agent boots.** The mission generation counts mission loads
in one DCS process. The agent boot names one DCS process. A `Stamp` carries both;
together they name one mission run. `frame` and `model_time` order stamps only within
one run. `agent_boot` is opaque: compare it for equality only. A stamp whose
`agent_boot` is 0 is never the same run as another stamp. After a reload or a DCS
restart, the reconciler spawns again every live group on the loaded map, and every
order is sent again.

**Facts that hold a member out.** DCS can lose a unit with no death event. The unit's
`left` then reads `UNIT_LEFT_MISSING` (cause unknown, may be dead) or
`UNIT_LEFT_REMOVED` (the read showed a removal). The member is not spawned again by a
reload, a restart or a restore, because its death report may have been lost. The
controller releases it with `ResolveMember` (`DEAD` or `RESTORE`) or removes the group.
A group whose every remaining member is held is `LOST`.

**Positions.** Every `Position` names its theater (map): `north` is DCS `x`, `east` is
DCS `z`, `alt` is meters above sea level (DCS `y`). An entity on a theater that is not
loaded stays virtual until that theater is loaded again.

## Sessions, roles and the lease

Call `SessionService.Hello` first with the schema version you built against. It returns
the server's version and schema, your role, and capabilities: whether raw Lua is on,
whether the sidecar persists campaigns, and the shortest position period it serves.

Roles come from tokens. A loopback listener with no tokens file gives every caller
`admin`. With tokens, send `authorization: Bearer <token>` as gRPC metadata.

| Role | May call |
|---|---|
| `observer` | Every call that changes nothing |
| `controller` | The above, the lease calls, and every changing call except the admin ones |
| `admin` | The above, `EvalLua`, `RegisterLua`, `RemoveLua`, `DeleteCampaign`, `ErasePlayer`, and every Moderation call |

[spec/errors.md](../spec/errors.md), "Roles", has the full list.

One controller at a time holds the lease. Any number of observers connect without one.
`AcquireLease` returns a `Lease` (an epoch and a lease ID) and a TTL, 30 s by default.
Renew it with `RenewLease` at a third of the TTL. A lease not renewed in time is free;
the next `AcquireLease` takes it under a new epoch. A call with an older epoch fails
with `LEASE_LOST`; one with the current epoch and another ID fails with
`NOT_CONTROLLER`. While another controller holds the lease, `AcquireLease` fails with
`LEASE_HELD`.

Every changing call carries the lease and an idempotency key. Most carry them in a
`Write`, with a campaign ID. Campaign 0 means the active campaign, or none when no
campaign is active. The CampaignService calls carry `lease` and `idempotency` as their
own fields, and name their campaign themselves. `RenewLease` and `ReleaseLease` carry
only the lease; `AcquireLease` carries a client name.

## Idempotency

Every changing call except the lease calls carries an idempotency key. An empty key is
`INVALID_FIELD`. Make each key unique to one change, and send the same key when you
send that change again.

- A repeat with the same key and the same request returns the first call's response and
  does nothing else. The lease is left out of the comparison, so a repeat under a new
  lease still matches.
- A repeat with the same key and a different request fails with
  `IDEMPOTENCY_MISMATCH`.
- A key is remembered for `idempotency_keep_h` hours, 24 by default, on the whole
  server: a repeat after the active campaign changed still finds the first call.
- A call that failed is not remembered. Send it again with the same key once the cause
  is fixed.
- Write each key to durable storage before you send the call, or derive it from your
  own durable ID for the change. Then a controller that restarts resends with the same
  key.
- `GetOutcome` reads a remembered call by its key: the campaign it acted on, and for an
  effect or a query its outcome and value. An unknown or expired key is
  `OUTCOME_NOT_FOUND`.

## Three kinds of call

| Kind | Calls | Returns | Sent to DCS |
|---|---|---|---|
| Set-state | `Put*`, `SetOrder`, `SetOptions`, `Materialize`, `Dematerialize`, `RemoveEntity`, `SetAirbaseOwner`, `SetWarehouseStock`, marks, drawings, menu items | when the desired state is stored, with revisions | by the reconciler, again until DCS matches |
| Effect | `DoEffect`, `AdjustWarehouseStock`, messages, sounds, chat, `RemovePlayerMark`, moderation, `InvokeLua`, `EvalLua` | when the agent answers, with a `CommandResult` and any value | at most once, never again |
| Query | `Capture` | when the agent answers, with the entity | again under a new sequence number if the answer was lost |

An effect or a query that gets no answer within `result_wait_s` (10 s by default)
returns the outcome `PENDING`. Read the final outcome later with `GetOutcome` and the
same key, or send the same call again with the same key.

`CommandResult.outcome` says what happened in DCS:

| Outcome | Meaning |
|---|---|
| `APPLIED` | DCS is in the commanded state |
| `REFUSED` | It reached DCS and failed; `message` says why |
| `REJECTED` | The agent did not try it |
| `UNKNOWN` | The agent may have taken it; the result is lost |
| `NOT_SENT` | It did not happen |
| `PENDING` | Not answered yet |

Every outcome, `PENDING`, `REFUSED`, `REJECTED`, `UNKNOWN` and `NOT_SENT` included,
comes in a successful response. `code` carries the agent's own result code beside the
outcome, so a program can tell `WRONG_GENERATION` from `RAW_LUA_OFF` without reading the
message.

## Errors and retries

Every failed call returns a gRPC status with one `ErrorDetail`: a `reason`, a message,
and the entity or field it is about. Match on `reason`; the message text can change.
[spec/errors.md](../spec/errors.md), "Public API", lists every reason, its gRPC code and
when it comes.

| Reason | What to do |
|---|---|
| `INVALID_FIELD`, `UNMANAGED_ENTITY`, `CONFIRMATION_REQUIRED`, `CAMPAIGN_EXISTS` | Fix the request. Do not retry it as is. |
| `IDEMPOTENCY_MISMATCH` | The key was used for another change. Use a new key. |
| `TOKEN_REQUIRED`, `ROLE_REQUIRED` | Send a valid token with a role that allows the call. Do not retry as is. |
| none (`UNSPECIFIED`), gRPC `FAILED_PRECONDITION` | Read `field` and the message. [spec/errors.md](../spec/errors.md) lists each case, such as a theater with no template. |
| `ENTITY_NOT_FOUND` and the other `*_NOT_FOUND` | The thing does not exist, or was removed and purged. A `NOT_FOUND` from `RemoveEntity` means done. |
| `LEASE_LOST`, `NOT_CONTROLLER` | Take the lease again, then send the call again with the same key. |
| `LEASE_HELD` | Another controller is running. Wait, or stop. |
| `NO_ACTIVE_CAMPAIGN`, `CAMPAIGN_NOT_ACTIVE` | Activate the campaign, or name the active one. |
| `CAMPAIGN_READ_ONLY` | The campaign is finished. |
| `THEATER_NOT_LOADED` | Load the entity's theater first. |
| `QUEUE_FULL` | The command queue to DCS is full. Back off, then send again with the same key. |
| `DCS_NOT_READY` | No DCS is attached, it is loading, or it is hung. Wait for a healthy `GetServerStatus`, then send again. |
| `CONSUMER_TOO_SLOW` | Your stream fell behind and ended. Open it again from your last revision or sequence number. |
| `REVISION_EXPIRED` | The store no longer keeps changes since your revision. List again, then watch. |
| `SEQUENCE_EXPIRED` | The event history no longer holds your sequence number. Read from the oldest kept, and mark the span as incomplete. |
| `UNAVAILABLE` with no reason | The server is stopping. Connect again and resume from your last revision or sequence number. |
| `SPACE_LOW`, `DCS_IDS_FULL` | The server is short of disk space or of DCS mark IDs. Tell the operator. |
| `RAW_LUA_OFF` | The operator has Lua off. No call turns it on. |
| `UNVERIFIED`, or none with gRPC `UNIMPLEMENTED` | The call or path is not verified on DCS, or not built. `field` names it. |
| `INTERNAL` | A fault in the sidecar, or a campaign that has used every entity ID. Report it. |

No layer retries what the layer below refused, and no layer turns a failure into a
success. A retry is always the client's, and always with the same idempotency key.

## Services

Each service is one file in [proto/nanoprobe/v1alpha1/](../proto/nanoprobe/v1alpha1/).

### SessionService (`session.proto`)

| Call | Purpose |
|---|---|
| `Hello` | Versions, capabilities and your role |
| `AcquireLease`, `RenewLease`, `ReleaseLease` | The controller lease |

### CampaignService (`campaign.proto`)

| Call | Purpose |
|---|---|
| `CreateCampaign` | Make a campaign, with its theaters and clock; optionally activate it |
| `ListCampaigns`, `GetCampaign` | Read campaigns |
| `ActivateCampaign` | Load the campaign's mission and restore it; suspends the active one |
| `SuspendCampaign` | Capture, checkpoint and close; DCS keeps running unmanaged |
| `FinishCampaign` | Suspend, then make the store read-only with an outcome |
| `ExportCampaign` | The store as line-per-record text |
| `DeleteCampaign` | Delete one that is not active; needs `confirm_name` |
| `BackupCampaign`, `ListCampaignBackups` | Take and list backups. Restoring is offline (operator guide) |
| `SetCampaignTemplate` | Give the campaign its own mission template for one theater |

An activation, a `CreateCampaign` that activates, or a `Reload` fails with
`FAILED_PRECONDITION` and the field `theater` when the theater has no template, the
template does not parse, or the mission does not render. Nothing changes then. A
mission that renders but cannot be saved fails the same way, after the groups are
captured. An airport in the next-mission settings that the template lacks fails with
`INVALID_FIELD` on `next.dynamic_spawn`.

### CommandsService (`commands.proto`)

| Call | Purpose |
|---|---|
| `PutGroup` | Create a group (ID 0) or replace its spec; optional first order |
| `PutGroupFromTemplate` | Create a ground group from a Mission Editor group template |
| `PutFromStaticTemplate` | Create every vehicle group and static of a Mission Editor static template, in one commit |
| `PutStatic` | Create or revise a static |
| `SetOrder` | Give a group a route, attack, hold or orbit order |
| `SetOptions` | Set a group's controller options by DCS option ID |
| `Materialize`, `Dematerialize` | Move groups and statics between DCS and the campaign |
| `RemoveEntity` | Remove a managed group, static, mark, drawing or menu item for good |
| `ResolveMember` | Release a held member: `DEAD` or `RESTORE` |
| `SetLostPolicy` | What to do with a `LOST` group: report, respawn or mark dead; per group or the campaign default |
| `SetAirbaseOwner`, `SetWarehouseStock` | Desired owner and stock of an airbase |
| `AdjustWarehouseStock` | Add to or take from stock now; an effect |
| `Capture` | Read an entity's full state from DCS; a query |
| `DoEffect` | Explosion, smoke, flare, illumination, damage, sound, picture, picture stop, or `ClearDebris` |
| `SetSampleRates` | Ask for other sampling periods, within the agent's budget |
| `GetOutcome` | The outcome of an earlier call, by its idempotency key |

An order is stored as an intent. The sidecar compiles what remains of it into a DCS
task whenever DCS must be told again, so a convoy restored after a restart drives on to
its destination. An attack order on a target that is not bound in DCS is kept and sent
when the target binds.

`ClearDebris` removes wrecks, craters and debris in a volume. A `Column` is a square on
the map that clears at every height and needs no terrain height. A `Sphere` and a
`Box` take altitudes above sea level. The edge is not exact: wrecks just outside a small
volume can go too, and a wreck that is still burning stays (LIVE,
[clear-debris](results/2026-11-16-clear-debris.md),
[column](results/2026-11-18-clear-debris-column.md)). A `WholeMap` is sent as several
boxes; that it covers every theater is UNVERIFIED.

### WorldService (`world.proto`)

| Call | Purpose |
|---|---|
| `ListWorld` | A snapshot at one revision, streamed |
| `WatchWorld` | Changes after a revision: upserts, position batches and removals |
| `QueryArea` | Entities within a radius of a point, virtual ones included |

`WatchWorld` needs the active campaign. `ListWorld` and `QueryArea` also read a
suspended or finished campaign by its ID
([spec/persistence.md](../spec/persistence.md), "Reading a campaign that is not open").
With campaign 0 and no active campaign, they fail with `NO_ACTIVE_CAMPAIGN`. A
`WorldFilter` narrows by campaign,
coalition, kind, area, managed only, and markup group. Units are listed inside their
groups. A watch that names campaign 0 follows the campaign active when it starts and
ends with `CAMPAIGN_NOT_ACTIVE` when another becomes active.

### EventsService (`events.proto`)

| Call | Purpose |
|---|---|
| `SubscribeEvents` | Events after a sequence number, then live |
| `ReadEvents` | A page of history |

Events are numbered per campaign, one more per event. While no campaign is active,
events go to the server log, which has its own numbers; set `server_log` to read it.
Every field of an event but `sequence` and `stamp` is optional, because DCS does not
always send the same fields for one event type.

### MissionService (`mission.proto`)

| Call | Purpose |
|---|---|
| `GetServerStatus` | Health verdict, sim state, loaded theater, achieved sampling periods, overhead, queues, store size, backups, DCS IDs left |
| `SetNextMission` | Theater, start time, weather and airports with dynamic spawn for the next load |
| `Reload` | Write the campaign's mission and load it; the campaign is restored after the load |

### PlayersService (`players.proto`)

| Call | Purpose |
|---|---|
| `ListPlayers`, `ListOccupancy` | Players, and who sat in which unit when |
| `SendMessage`, `PlaySound`, `SendChat` | Messages to an audience |
| `PutMark`, `PutDrawing` | F10 map marks and typed drawings |
| `PutMenuItem` | F10 radio menu items; selecting a command gives a `MenuEvent` |
| `PutMarkupGroup`, `RemoveMarkupGroup`, `ReplaceMarkupGroup`, `ListMarkupGroups` | Markup groups |
| `RemovePlayerMark` | Remove a mark the platform does not hold, such as a player's; an effect |
| `ErasePlayer` | Erase one player's records, or move them to a new key; admin |

A player key is derived from the ID DCS gives each client. It stays the same across
sessions, names and campaigns.

### ModerationService (`moderation.proto`), admin only

`Kick`, `Ban`, `Unban`, `ListBans` and `MovePlayer`. `MovePlayer` returns the outcome
`REFUSED` for some slots: one the mission does not list, one of the other coalition, one
another player holds, and a crew seat ([spec/commands.md](../spec/commands.md), "Slot
moves").

### ExtensionService (`extension.proto`)

| Call | Purpose |
|---|---|
| `RegisterSchema` | Declare your own event type or component, with a schema version |
| `PublishEvent` | Write your own event into the event log |
| `PutData`, `GetData` | Your own data in the campaign store |
| `RegisterLua`, `RemoveLua`, `ListLua`, `InvokeLua` | Named Lua chunks installed at every mission start; admin to register |
| `EvalLua` | Run Lua once in the server state; admin |

Lua calls need `[lua] raw = true` in the agent's settings on the server; otherwise they
fail with `RAW_LUA_OFF`. No API call can turn it on.

## World: list, then watch

Read the world with `ListWorld`, then `WatchWorld` from the revision the list returned
(`ListWorldResponse.revision`, the same in every message of one list). A watch sends
`WorldChange` messages:

- `upsert`: an entity appeared or changed anything but a position;
- `positions`: a columnar batch of unit positions with the stamp they were sampled at;
- `removal`: an entity you held was removed, after an upsert of its last state, with
  the cause `CONTROLLER`, `EXPIRED` or `EVICTED`;
- `removed_id`: an entity you held left the store before your watch read its removal.

Revisions are per campaign. Watch with the `campaign_id` the list returned, and store
it with the revision. After `CAMPAIGN_NOT_ACTIVE`, list the new active campaign again.

`rate` and `min_period` ask for faster or slower positions. A period under the
server's cap is served at the cap, which is not an error. Keep the last revision you
processed; on `CONSUMER_TOO_SLOW` or a dropped connection, watch again from it. On
`REVISION_EXPIRED`, list again.

A dead group expires and leaves the API `dead_keep_h` after it died, 24 hours by
default. A dead static stays as a ruin until the controller removes it.

## Events: sequence numbers and gaps

Subscribe with `after_sequence` set to the last sequence you processed, or 0 for the
oldest kept, or `live_only` for the newest. Sequence numbers are per campaign. Store the
sequence of each event you handle with its `campaign_id`. On reconnect, subscribe to
that campaign after it.

A `Gap` event says events in a span of time were lost; `lost` counts them. Mark what
overlaps the span as incomplete, and read the world again for what matters. The
agent's dropped events also force a full enumeration in DCS, so the world catches up on
its own. A `LifecycleEvent` reports a sim state change, including `SIM_STATE_HUNG` when
the health verdict enters `hung`. Gap and lifecycle events pass every filter.

Some events do not always carry the data you expect, depending on how they were
caused. Code for missing fields.

## Markup groups and drawings

A mark or a drawing may carry a label and belong to a markup group
([spec/world.md](../spec/world.md), "Labels and markup groups").

- **Labels.** 1 to 128 bytes, unique among the campaign's live marks and drawings. A
  put with ID 0 and a label an item of the same kind holds changes that item (an
  upsert). A put always sets the label it carries, and an empty label clears it, so
  send the label on every put. A label that another item holds fails with gRPC
  `ALREADY_EXISTS`, reason `INVALID_FIELD`, field `label`: an item of the other kind,
  an item in another markup group, or another item when the put names an ID.
- **Markup groups.** A name with `visible`, an optional `expire_after_s` and an optional
  `cap`. A name is 1 to 64 bytes with no control character. A put always sets the markup
  group it carries; an empty one takes the item out of its group. A put of an item that
  names a markup group the campaign lacks makes it, visible. `PutMarkupGroup` sets every
  field as given: a call that leaves out `visible` hides the group. Send `visible: true`
  whenever you set a cap or an expiry. Hiding a markup group removes its items from DCS
  and keeps them in the store; showing it adds them again.
- **Replace.** `ReplaceMarkupGroup` makes a markup group hold exactly the items given,
  by label, in one write. Every item needs a label, unique in the call and held by no
  item of another markup group. Items whose spec is unchanged keep their revision and
  DCS ID. Use it to redraw a layer each tick.
- **Expiry and cap.** An item expires after `expire_after_s` of campaign model time;
  paused time does not count. A put of one item over the cap removes the markup
  group's oldest items. A `ReplaceMarkupGroup` with more items than the cap fails with
  `INVALID_ARGUMENT` and changes nothing.
- **DCS IDs.** DCS ignores an add under a removed mark ID until the next mission load
  (LIVE, [results](results/2026-11-25-drawing-layers.md)). So the platform picks every
  ID and never reuses one within a DCS process. Every new item, change of geometry,
  show of a hidden markup group, and re-add after a mission load or a sidecar restart
  takes a new ID.
  A change of only a drawing's colors, line type, font size or text uses none, so
  change items in place. Only a new DCS process frees IDs. A put fails with
  `DCS_IDS_FULL` when the pool cannot cover its adds. `GetServerStatus` reports
  `dcs_ids_left`, and sets `dcs_ids_low` when under 1 % are left.

A drawing is a `DrawingShape`: a theater, a style, a text, and one kind with its
geometry. The kinds are `Line`, `Polygon`, `Arrow`, `TextBox`, `Quad`, `Freeform`,
`Circle`, `Rectangle`, `Oval` and `RoundedRectangle`. Headings are radians clockwise
from north. The sidecar lowers each kind to one DCS call; no caller-made argument
reaches DCS. Use `typed_shape` in `PutDrawing`; the `shape` and `args` fields are
deprecated. [spec/world.md](../spec/world.md), "Drawings", has each kind's limits and
what a client draws (LIVE on DCS 2.9.31.29434).

## Templates and presets

A call that names a file may name one only in a folder the operator lists in
`[missions] template_dirs`. The sidecar reads that file once, at the call, and stores
the result. The shared per-map templates are in `[missions] dir`, and each activation
or reload writes the mission from the theater's template there.
[mission-templates.md](mission-templates.md) explains each kind:

- **Mission templates.** One per map, saved in the Mission Editor. The sidecar writes
  each campaign's mission from it, changing the date, start time, weather, and the
  dynamic-spawn and hot-start settings of the airports the controller names.
  `SetCampaignTemplate` gives a campaign its own. Units placed in the template stay
  unmanaged.
- **Group templates.** `PutGroupFromTemplate` makes a ground group from one entry of a
  `templates.lua` file, at an anchor and heading.
- **Static templates.** `PutFromStaticTemplate` makes every vehicle group and static of
  an `.stm` file, in one store commit or not at all.
- **Payload presets.** A unit's `payload_preset` in `PutGroup` gives an aircraft the
  pylons of a Mission Editor preset file in the data form. A preset file with Lua code
  is refused.

## What the sidecar adds to DCS's events

DCS gives a script raw events and object handles. The sidecar adds these
([spec/world.md](../spec/world.md), "Events and the store" and "Players"):

- **Kill attribution.** DCS sends a weapon kill's `KILL` with the late death event, which
  can come long after the object stopped existing (LIVE,
  [results](results/2026-11-16-clear-debris.md)). By then the victim and maybe the
  killer have left the agent's table. The sidecar keeps, per mission generation, a
  memory of the rows that left. It resolves the late event's objects from it, names the
  players who were in them in `initiator_players` and `target_players`, and sets
  `dated_time` from the victim's BDA event, else from when its row left. DCS's own time
  stays in `extra.time`.
- **Launch attribution.** At each shot the sidecar maps the weapon to the shooter's
  players. A `HIT` or `KILL` naming that weapon carries them in `launch_players` with
  `launch_rule` `LAUNCH_RULE_WEAPON`. A gun hit names no weapon; it, and a weapon never
  seen shot, fall back to the initiator's players before it left, with
  `LAUNCH_RULE_INITIATOR`. `initiator_players` is never rewritten. That `SHOT` and
  `KILL` carry the same weapon object ID is UNVERIFIED.
- **Player keys.** Every event is stamped with the keys of the players in its
  initiator and target. A key is an HMAC of the player's DCS ID under a per-server
  secret. The API, events and logs never carry the DCS ID itself.
- **Seat occupancy.** `ListOccupancy` and `PlayerEvent` say who sat in which unit and
  seat, and when.
- **Route progress.** `RouteEvent` reports `ROUTE_EVENT_KIND_WAYPOINT_REACHED` per
  waypoint and `ROUTE_EVENT_KIND_ROUTE_COMPLETE` once per route, worked out from
  position samples.
- **Loadouts.** `LoadoutEvent` gives the ammunition of a unit players occupy, read at
  its birth and at takeoff, by weapon type.
- **Menu attribution.** A `MenuEvent` names the player who chose the item, or the
  players who could have.
- **Mark ownership.** A mark event's `platform_mark` says whether the mark is one of
  the platform's own, so a controller can tell them from a player's.
- **Sequence numbers and gaps.** Events are numbered and stored, and a `Gap` event
  marks a span where events were lost.

## Freshness

The agent aims to take at most 0.3 % of real time on the simulation thread by default
(`[budget] overhead_share`). Its reads get at most 250 µs of one frame. Commands have
their own budget, 2 ms a frame (`commanded_ms`). At 2,000 objects under Wine, about
0.2 % of frames went over 250 µs, the worst at 3.4 ms (LIVE,
[s11](results/2026-11-14-s11-budget-and-baseline.md)). The agent reads what the budget
allows. Each value has a target
period (`[sampling]` in `agent.toml`, or `SetSampleRates`):

| Value | For | When it is read | How old it can be |
|---|---|---|---|
| Position | every unit, and cargo statics | every second | 1 s. A unit that has not moved 0.5 m in three reads is read every 5 s, so it is seen up to 5 s late once when it starts to move. |
| Position | statics that cannot move | when first seen, and at each full enumeration (60 s) | does not change |
| Life | every unit and static | when first seen; after a hit event names it; in a slow sweep every 60 s; at a capture | about 1 s after a weapon hit. Damage that sends no event shows at the next slow sweep. |
| Life | units players occupy | every 5 s | 5 s |
| Ammunition | units players occupy | when the player takes the slot, and at takeoff | not updated in between |
| Ammunition | AI units | never | unknown |
| Fuel | any unit | never | not tracked |
| Owner and stock | every airbase | every 60 s | 60 s |
| Which objects exist | every group and static | births and deaths as they happen; a full enumeration every 60 s | see "DCS behavior" |

These are targets. When the budget cannot meet them, rates fall in a fixed order:
warehouses first, then life, then positions. Positions a controller subscribed to fall
last. `GetServerStatus.achieved` reports the period each tier achieved. The position of
cargo an aircraft carries is UNVERIFIED.

## DCS behavior to code for

These hold on DCS 2.9.31.29434. Each item links its results, or the spec section that
cites them.

- **Deaths.** A killed ground unit or static is reported dead at once, from a read. DCS
  sends a ground unit's own death event when its wreck explodes, which can be long
  after the unit stopped existing (LIVE, [results](results/2026-11-16-clear-debris.md)). A cargo
  static killed by gunfire sends no death event (LIVE) and is reported missing.
  Aircraft and ships are reported missing when they leave, and dead when their death
  event arrives ([spec/records.md](../spec/records.md), "The object table").
- **Removal by script.** DCS sends no event for an object a script removes (LIVE). The
  platform reports a removed ground unit or static as removed, and anything else as
  missing.
- **Damage without an event.** A scripted explosion, or a script that sets a unit's
  life, sends no hit event. The slow sweep finds the change
  ([spec/sampling.md](../spec/sampling.md)).
- **Rearming.** The platform does not see a rearm. No event came when a truck rearmed
  an AI unit (LIVE, [E38](results/2026-11-14-e38-life-and-ammunition.md)). Whether DCS
  sends one when ground crew rearm a player's aircraft is UNVERIFIED. A player's
  ammunition is read again at the next takeoff. Flares and chaff cannot be read.
- **Restarts.** The sidecar saves positions and life every 5 s, and stock as each change
  is read. After a DCS restart, each group on the loaded map is spawned from its last
  stored state, and its order continues. After a crash the restored values are up to one
  checkpoint old. Ammunition is not stored. After a restart, DCS runs the mission its
  own settings name. The campaign's next `Reload` loads the campaign's mission.
- **Ruins.** A destroyed campaign static stays dead and stays in the world. After a
  reload or restart, DCS shows a dead static in its place. How a restored ruin looks
  depends on its type ([spec/world.md](../spec/world.md), "Ruins").
- **Capacity.** DCS simulates on one core. The platform is measured at 500, 1,200 and
  2,000 objects (LIVE, [s11](results/2026-11-14-s11-budget-and-baseline.md)).

## Example messages

These were captured from a run of the hello flow against the fake sidecar, then
trimmed. They are Protobuf JSON: 64-bit integers are strings, enum values are names,
and a field at its default value (0, false, empty) is left out. A field marked
`optional` in the schema, and a message field, appear when set, even at 0 or empty:
`waypointIndex: 0` is waypoint 0. The lease ID is
replaced with `"…"`.

**Put a group.** The controller holds the lease, has created and activated campaign
`1795722999663`, and puts a convoy with a route order. `PutGroupRequest`:

```json
{
  "write": {
    "lease": { "epoch": "1795722999660", "leaseId": "…" },
    "idempotency": { "key": "hello/hello-1795722999661/convoy" },
    "campaignId": "1795722999663"
  },
  "spec": {
    "label": "convoy",
    "country": 2,
    "category": "GROUP_CATEGORY_GROUND",
    "units": [
      { "type": "M-113", "label": "lead",
        "position": { "theater": "Caucasus", "north": -290000.0, "east": 680000.0 } },
      { "type": "M-113", "label": "second",
        "position": { "theater": "Caucasus", "north": -290000.0, "east": 680020.0 } }
    ]
  },
  "order": {
    "route": {
      "waypoints": [
        { "position": { "theater": "Caucasus", "north": -289000.0, "east": 680000.0 },
          "speed": 10.0 }
      ]
    }
  }
}
```

`alt` is 0 and so is left out. `PutGroupResponse` comes back once the store holds the
group; DCS has not spawned it yet:

```json
{ "groupId": "1", "unitIds": ["2", "3"], "specRevision": "1", "revision": "2" }
```

**See it in the world.** A later `ListWorldResponse` holds the group, `LIVE`, with its
units where they are now (trimmed to the group and one unit):

```json
{
  "revision": "54",
  "campaignId": "1795722999663",
  "entities": [
    { "group": {
        "id": "1", "label": "convoy", "managed": true,
        "state": "ENTITY_STATE_LIVE",
        "coalition": "COALITION_BLUE", "country": 2,
        "category": "GROUP_CATEGORY_GROUND", "theater": "Caucasus",
        "specRevision": "1", "orderRevision": "1",
        "units": [
          { "id": "2", "groupId": "1", "type": "M-113", "label": "lead", "alive": true,
            "position": { "theater": "Caucasus", "north": -289000.0, "east": 680000.0 },
            "lifeMax": 1.0 }
        ] } }
  ]
}
```

Watch from `revision` 54 on campaign `1795722999663` to get every change after it.

**Route events.** When the convoy reaches its one waypoint, the event stream delivers
two events: `WAYPOINT_REACHED` for waypoint 0, then `ROUTE_COMPLETE`. The second:

```json
{
  "event": {
    "sequence": "2",
    "campaignId": "1795722999663",
    "stamp": {
      "wall": "2026-11-26T19:56:41.720769882Z",
      "modelTime": 25.700000000000095,
      "missionGeneration": 2,
      "frame": "259",
      "agentBoot": "1795722996410"
    },
    "route": {
      "kind": "ROUTE_EVENT_KIND_ROUTE_COMPLETE",
      "groupId": "1",
      "orderRevision": "1",
      "modelTime": 25.800000000000097,
      "observed": true
    }
  }
}
```

`waypointIndex` is set only on `WAYPOINT_REACHED`. `observed` is true because the
position came from DCS.

**An effect.** The controller sets the lead's life to 0 with a `Damage` effect.
`life_percent` is 0, so JSON leaves it out:

```json
{
  "write": {
    "lease": { "epoch": "1795722999660", "leaseId": "…" },
    "idempotency": { "key": "hello/hello-1795722999661/damage" },
    "campaignId": "1795722999663"
  },
  "effect": { "damage": { "unitId": "2" } }
}
```

`DoEffectResponse` carries the outcome once the agent answers:

```json
{ "result": { "outcome": "OUTCOME_APPLIED", "costUs": 10, "code": "RESULT_CODE_APPLIED" } }
```

## Recipes

Each recipe names the calls. The hello examples run recipes 1 and 2 in full:
[hello-rust](../examples/hello-rust), [hello-python](../examples/hello-python),
[hello-kotlin](../examples/hello-kotlin). [first-campaign.md](first-campaign.md)
runs them against the fake sidecar, with no DCS.

**1. Connect and take control.** `Hello` with your schema version. Check `role`.
`AcquireLease`; renew at a third of `ttl` on a timer. On `LEASE_LOST` or
`NOT_CONTROLLER`, acquire again and resend in-flight calls with their keys.
`ReleaseLease` on exit.

**2. Start a campaign and spawn a group.** `CreateCampaign` with `activate`. `PutGroup`
with ID 0, a `GroupSpec` and a route order; keep the returned `group_id` and
`spec_revision`. `ListWorld`, then `WatchWorld` from its revision, until the group's
state is `LIVE` and `applied_spec_revision` reaches the returned `spec_revision`.

**3. Survive a disconnect.** Store, durably, the campaign ID, the last world revision
and event sequence you processed, and every in-flight call's idempotency key. On
reconnect: `Hello`, take the lease, and resend in-flight calls with the same keys. Then
`WatchWorld` from the revision and `SubscribeEvents` after the sequence, both on that
campaign ID. On `REVISION_EXPIRED`, list again.

**4. Fire an effect and read its outcome.** `DoEffect` with a fresh key. If the outcome
is `PENDING`, call `GetOutcome` with that key later. Never send an effect again with a
new key unless you want it twice.

**5. Keep far groups out of DCS.** Periodically `QueryArea` around each player.
`Materialize` groups that come within your radius; `Dematerialize` those that leave a
larger radius, so they do not flap. The hysteresis is yours to choose.

**6. Redraw a map layer.** Each tick, build the whole layer as labeled items and call
`ReplaceMarkupGroup`. Unchanged items keep their DCS IDs, so the map does not flicker.
Hide the layer with `PutMarkupGroup` and `visible` false.

**7. Release a held member.** Watch for a unit whose `left` is `UNIT_LEFT_MISSING` or
`UNIT_LEFT_REMOVED`. Decide: `ResolveMember` with `DEAD` to record a death, or
`RESTORE` to let its group's next spawn include it.

**8. Change the weather and reload.** `SetNextMission` with theater, start, weather and
dynamic-spawn airports, then `Reload`. Give dynamic-spawn airports their aircraft with
`SetWarehouseStock`.

**9. Offer a menu.** `PutMenuItem` with `submenu` for a folder and without for a
command. Subscribe to events and act on each `MenuEvent` by `item_id`.
