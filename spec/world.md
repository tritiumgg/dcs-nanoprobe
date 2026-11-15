# World model

The agent, the sidecar and the public API share one model. DCS spawns, orders and
destroys groups; state lives on units. The model follows that.

## Identities

| Identity | Type | Assigned by | Rules |
|---|---|---|---|
| Campaign ID | `u64` | The sidecar, when a campaign is created | The wall-clock time of creation in milliseconds since the Unix epoch, or one more than the newest campaign's ID if that is larger. So an ID is never used twice on a server, even when the stores are lost or in memory only. Never 0. Below 2^53. |
| `EntityId` | `u64` | The sidecar, from one counter per campaign | Starts at 1. Never reused within a campaign. 0 means none. Below 2^53. |
| DCS object ID | `u64` | DCS | The numeric ID of a unit, static or group in one DCS process. The agent and the binding use it; the public API never shows it. |
| Mission generation | `u32` | The agent | 0 before the first mission load in the process, then one more at each load (spec/log.md, `gen`). |
| Player key | string | The sidecar | Derived from the ID DCS gives each client. Constant across sessions, names and campaigns. |

Every number that crosses into Lua is below 2^53, because Lua 5.1 numbers are doubles.

The pair of campaign ID and `EntityId` is unique on the server. The public API names
entities by `EntityId` within a campaign.

## Kinds

| Kind | Identity | Created by the platform | Carries |
|---|---|---|---|
| Group | `EntityId` | yes | The spec: country, category and members; its controller options; and the order. The unit of spawn, order, materialize and destroy. |
| Unit | `EntityId`, a member of one group | with its group | Observed state: position, attitude, life, fuel, ammunition. Dies alone. |
| Static | `EntityId` | yes | A spec and observed state. Spawned and destroyed alone. |
| Airbase | theater and DCS name | no | Owner (coalition) |
| Warehouse | theater and DCS name of its airbase | no | Stock |
| Mark, drawing, menu item | `EntityId` | yes | Player-facing objects. Set again after every mission load. |
| Extension | name | yes | A registered Lua chunk (spec/commands.md, "Extensions") |

The platform never creates or destroys an airbase or a warehouse. Their owner and stock
are settable. The store keeps a desired owner only for an airbase the controller set,
and a desired count only for a warehouse item the controller set. The reconciler sends
`AIRBASE_OWNER` or `WAREHOUSE_SET` when the agent's latest sample differs from it, once
per desired revision and mission generation. A capture by DCS or stock DCS uses is a
fact: a sample replaces a desired value only when the previous sample matched it, the
item (or owner) has no `AIRBASE_OWNER` or `WAREHOUSE_SET` waiting for its result, and
the new value is not one an `AIRBASE_OWNER` or `WAREHOUSE_SET` the sidecar applied set.
An applied `WAREHOUSE_ADJUST` changes the desired counts of the items it names, clamped
at 0, and is never sent again.

## Names

Every group, unit and static the platform creates has a DCS name that encodes its
identity:

```
np.<kind>.<campaign>.<entity>
```

| Part | Value |
|---|---|
| `np` | The platform's marker |
| `<kind>` | `g` group, `u` unit, `s` static |
| `<campaign>` | The campaign ID in base 36, lower case, no leading zeros |
| `<entity>` | The `EntityId` in base 36, lower case, no leading zeros |

Example: `np.u.m3k9x2a1.1z` is unit 71 of the campaign created at that millisecond. A
name is at most 28 characters, all of them in `[a-z0-9.]`.

Why this form (docs/results/2026-11-11-first-dll-and-spikes.md, E21):

- Unit names and static names are one namespace in DCS. A new object with a taken name
  removes the old holder without an event (LIVE). `EntityId` is unique across kinds
  within a campaign, so no two platform objects share a name.
- Group names are a separate namespace (LIVE). The kind letter keeps a group's name
  apart from its units' names anyway.
- Players see unit and group names in the unit dialog (LIVE, observed by the owner on
  a client; not read by a script). A name holds only numbers, never
  a player name, an author's label or anything else a player should not see. An
  author's label is stored in the sidecar and shown through the API.
- After a DCS restart the sidecar rebuilds the binding from names alone.

Rules:

- The shell writer never puts a name in this form into a mission.
- An object found in the world whose name parses with a campaign ID other than the
  active one is never bound to an entity of the active campaign. It is reported as
  unmanaged with the warning `foreign_name`. This happens after a campaign is
  suspended while DCS keeps running its mission, between a campaign switch and the
  mission load that follows it, and when a mission was saved with platform objects in
  it.
- While no campaign is active there is no store to adopt into. The sidecar mirrors the
  objects the agent reports, in memory only, gives them no `EntityId`, and does not
  serve them through the World service.

## Managed and unmanaged

A *managed* entity was created by the platform from a spec in the store. The reconciler
keeps it as the spec says.

An *unmanaged* entity was not: a player aircraft, a Mission Editor unit, a
dynamic-spawn aircraft. The agent finds it by events and by the full enumeration, and
the sidecar adopts it with a new `EntityId`. It is observed and reported, never
reconciled, ordered or destroyed by the reconciler. A controller may still send it an
effect.

## Positions and theaters

Every position carries the theater (map) it is on, because DCS coordinates mean nothing
across maps. The transport sends DCS's own coordinates, and the sidecar stamps the
theater of the mission that produced them. The public API names the axes:

| API field | DCS | Meaning |
|---|---|---|
| `theater` | the mission's theater | Map name as DCS gives it, such as `Caucasus` |
| `north` | `x` | Meters, map-local |
| `east` | `z` | Meters, map-local |
| `alt` | `y` | Meters above sea level |

An entity on a theater that is not loaded stays under campaign authority (virtual)
until its theater is loaded again. Moving an entity to another theater is a spec change
by the author.

## Facts and drift

A *fact* happened and the platform cannot undo it: a death, a kill, an expended round, a
captured airbase. Facts are journalled when they arrive and change the desired state.

A *drift* is a difference the platform corrects: a managed group that should exist and
does not, or an order DCS has not been given. The reconciler corrects drift only.

A group with a death fact for a member is never respawned with that member.

## Revisions

| Counter | Scope | Changes when |
|---|---|---|
| Store revision | campaign | Any change to the store. Stamps every change the World service reports. |
| `spec_rev` | group, static, mark, drawing, menu item, extension | Its spec changes, or a `lost` group or static is respawned, whose spec may be the same |
| `order_rev` | group | Its order changes, or the target of a sent or applied attack order (a group, a unit's group, or a static) gets new DCS object IDs. The new revision is taken when the target's spawn resolves `applied`, and the order is compiled after it. Not on a mission generation change, after which every order is sent again. |

A group's spec is its country, coalition, category, extra keys of its DCS table, and
its units: each one's type, skill and extra keys. A static's is its country,
coalition, type and extra keys. Positions and state are not spec; a spawn takes the
last known ones. The author's label and components are not spec either: they never
reach DCS, and a change to them moves only the store revision.

A group's controller options, by DCS option ID, are kept with the group and are not
spec: a change moves only the store revision. The reconciler sends `GROUP_OPTION` for
each option the group's current spawn has not applied, after the group's order
(spec/commands.md).

The agent holds `applied_spec_rev` and `applied_order_rev` per entity it applied
(spec/commands.md). The sidecar re-sends a spec or an order only when the agent's
applied revision is behind.

## Removal

Removing a managed group or static takes it out of the campaign for good. The store
keeps a removed entity, marked removed, under campaign authority: the API no longer
reports it, its `EntityId` is never given again, and the reconciler destroys any object
of it that DCS still holds, as drift.

## Lifecycle

A managed group is in one of these states. A managed static follows the same states as
a group of one.

| State | Meaning |
|---|---|
| `virtual` | Simulated by the campaign, not in DCS |
| `materializing` | A spawn was sent and not yet acknowledged |
| `live` | In DCS, at least one member alive |
| `dematerializing` | Leaving DCS: a capture-and-destroy, or for a static a capture and then a destroy, was sent, and the destroy is not yet applied |
| `dead` | Every member has a death fact. Final. |
| `lost` | Missing from DCS with no death fact, or held: every member without a death fact is held across a mission generation (see "Respawn gate") |
| `unspawnable` | DCS refused the spawn. Not retried. |

| From | To | When |
|---|---|---|
| `virtual` | `materializing` | The authority is DCS and the entity's theater is loaded. A level condition, checked on every plan. |
| `materializing` | `live` | The spawn's outcome is `applied` |
| `materializing` | `unspawnable` | The spawn's outcome is `refused` (spec/errors.md) |
| `materializing` | `materializing` | The spawn's outcome is `unknown` or `not_sent`, or the mission generation changes: the spawn is sent again |
| `materializing` | `lost` | Every member without a death fact is held (see "Respawn gate"), so no member may be spawned. Never `dead`: only death facts, or the author's lost policy `dead`, make a group `dead`. |
| `materializing` | `virtual` | The authority is the campaign, or the entity's theater is not loaded, no spawn of it is unanswered, and none was `unknown` in this generation (DCS may hold it; the spawn is sent again, answered `AT_REVISION`, and the group leaves through `live`) |
| `live`, `materializing`, `dematerializing`, `lost`, `virtual` | `dead` | The last member's death fact arrives (see the conditions below) |
| `live` | `dematerializing` | The authority moves to the campaign |
| `dematerializing` | `virtual` | The destroy is applied (the agent's `APPLIED`, which while paused comes before DCS lets go), or the agent answers `NOT_BOUND`: a capture-and-destroy that ran in full, or the `GROUP_DESTROY` or `STATIC_DESTROY` sent after a capture, or after a capture-and-destroy that was `refused`, `unknown` or `TOO_LARGE`. The entity keeps the captured state, or else its last checkpointed state. Also when the mission generation changes first. |
| `dematerializing` | `lost` | The destroy was `refused` or `unknown` `destroy_retries` times (spec/config.md). The error is kept as the entity's last error. |
| `live`, `materializing` | `virtual` | The mission generation changes and the new mission's theater is not the entity's, or the authority is the campaign |
| `live` | `lost` | A full enumeration finds no member of the group, and no death fact explains it |
| `live` | `materializing` | The mission generation changes (a reload or a DCS restart), the theater is still the entity's and the authority is DCS |
| `lost` | `live` | A later full enumeration finds the group |
| `lost` | `materializing` or `virtual` | The mission generation changes: `materializing` if the new mission's theater is the entity's and the authority is DCS, else `virtual` |
| `lost` | `materializing` | The author's policy is respawn, the authority is DCS, a full enumeration used since the sidecar started, since the last `HELLO` and since the last `GAP` of events finds no object of the entity, and the respawn gate is open (see "Respawn gate"). Until the enumeration the binding mirror may lack objects DCS holds, such as those of an entity that went `lost` after its destroys failed. The spawn carries a new spec revision: the agent still holds the old one for this generation and would answer `AT_REVISION`. |
| `lost` | `dead` | The author's policy is mark dead |
| `unspawnable` | `virtual` | The author changes the spec; its last error is cleared. It materializes from `virtual` by the level condition above. |

- `lost` covers removals DCS reports no event for, and events lost because another
  script's handler raised first. The default policy is to report and wait for the next
  full enumeration.
- A member that is missing without a death fact, while another member is still found,
  is reported as missing and the group stays `live`. No spawn includes it while it is
  marked: a mark the agent reported (`OBJECT_GONE` `MISSING` or `REMOVED`) holds the
  member across mission generations until a death fact arrives or the controller
  removes the group (see "Respawn gate").
- After a DCS restart every `live` group is spawned again from its last captured state,
  on the same path as a first materialization.
- A spawn sent again at a revision the agent applied in this mission generation is
  answered `AT_REVISION`, which is `applied` (spec/errors.md): sending a spawn twice
  never makes two groups.
- A death fact comes from an `OBJECT_GONE` with reason `DEAD` (spec/records.md), so
  only units and statics in the agent's table have one, never scenery. The agent may
  write it after the object left the table as `MISSING` or `REMOVED`: DCS sends a death
  event up to about a minute after the object stops existing. It counts only
  when it is stamped with the loaded generation and the store's campaign, and its
  binding names a unit of the group (or the static). A death of an earlier
  generation, of another campaign (campaign 0 included), or for an object the
  binding does not tie to the group, changes nothing. The same stamp condition holds
  for an `OBJECT`'s binding, and for an `EVENT`'s object IDs where the sidecar
  resolves them. The sidecar counts what it drops. An enumeration with a record
  stamped with another campaign is not used.
- `dead` is final. A spawn or capture result, or an enumeration, that arrives for a
  `dead` entity changes nothing and is counted. A destroy result for a `dead` entity
  whose objects DCS still holds counts toward `destroy_retries` like any destroy of an
  entity DCS should not hold (spec/config.md).
- When the last death fact arrives while the group is `materializing`, the spawn
  that is not yet answered ran: it resolves `applied`.
- `lost` -> `dead` by death facts happens in order: the members left as `MISSING` or
  `REMOVED`, the group went `lost`, and the death events that follow make each a
  death fact. `virtual` -> `dead` cannot happen in order, because an entity becomes
  `virtual` only once DCS no longer holds it. The row keeps the rule total.
- A death fact for a member that left as `MISSING` or `REMOVED` counts as any other.
  When it corrects a `REMOVED`, the sidecar also counts a classifier fault, journals
  it and reports it. A death fact that makes the last member dead moves the group to
  `dead`, and a spawn it has not yet written is never written.
- The sidecar keeps, per member, the DCS object ID it was last bound to or left with.
  A death fact (and a `MISSING` or `REMOVED`) for another object ID is about an object
  from before a respawn: the sidecar journals and counts a death fact, and it marks
  no member.
- A death can be undone in DCS: a script that sets a unit's life revives it under the
  same object ID and entity, and the agent writes an `OBJECT` for it. When the object
  ID is the one the member's death fact named, the sidecar keeps the death journalled
  and counted, journals a `revived` fact, binds the object again and marks the member
  alive. It holds nothing. A group that is already `dead` stays `dead`. A revival is
  not a spawn. A later death is a new death fact.
- Every transition is journalled before the command that causes it is sent.

## Respawn gate

The respawn policy (`on_lost`, spec/config.md) does not respawn a `lost` group while
any member that is not found is marked `MISSING` or `REMOVED` and no death fact
explains it: a mark from this mission generation, or one the agent reported in an
`OBJECT_GONE` in an earlier generation (below).

`MISSING` means DCS no longer has the object and the cause is unknown, so a death
event may still follow. `REMOVED` means DCS no longer has it and the read shows a
removal, not a kill. A scripted kill that is removed before the slow sweep can read as
`REMOVED`, so `REMOVED` holds the gate exactly as `MISSING` does, for good: no timer
and no enumeration opens it. A death fact for the member releases its hold: the member
is dead, and the spawn leaves it out (a dead member is never spawned again). A member
that is found again holds nothing either. A group that the gate holds stays `lost`,
appears in the reconciler's held set and is counted.

A mission generation change keeps every mark the agent reported in an `OBJECT_GONE`
(`MISSING` or `REMOVED`) and resets only the member's DCS object ID: a death event can
no longer arrive for an object of the old generation (UNVERIFIED: that the
agent's memory of rows that left ends with the mission; the fake sim models it so), so the member may be dead. Such
a member is *held*: no spawn includes it and it is never the lead of an order
(spec/persistence.md, "Restore"). A mark inferred at an enumeration's end, with no
`OBJECT_GONE`, clears at the generation change: it can name a member DCS never bound,
such as one whose `OBJECT` was lost in a `GAP`. A group whose members are all dead or
held, with at least one held, goes from `materializing` to `lost` and stays there; the
platform never makes it `dead` without death facts, and only the author's lost policy
`dead` moves it on. Only the controller releases a hold: removing the group
(`RemoveEntity`) drops its members' marks. A call that resolves one member as dead or
restores it is step S19's. The API shows a member's mark as `Unit.left`, whether it
was reported or inferred.

A weapon kill is never respawned as a `MISSING` or `REMOVED` member. A group that a
script removed is not re-created by the policy or by a mission change; the policy then
acts as report. A member with a death fact never holds the gate, and a revived member
holds nothing.

## Route progress

The sidecar follows each group's route from its position samples. A waypoint counts as
reached when a sample of the group's lead unit lies within 50 m of it on the map plane,
when the segment between two consecutive samples passes within 50 m of it, or when the
group is seen at a later waypoint, which reaches every waypoint before it. A segment
counts only between two samples of the same lead unit at most 60 s of model time
apart, with no `HELLO`, new mission, death or new order between them. The lead unit
is the first member that is not dead. Progress never goes back. For each waypoint reached the sidecar writes a `WAYPOINT_REACHED`
event with the group, the order revision, the waypoint's index, the model time of the
sample that showed it, and whether that position was observed or written by the
controller. When the last waypoint is reached it also writes one `ROUTE_COMPLETE`
event with the group, the order revision, the sample's model time and the same flag.
A route belongs to a group, so both events are per group, never per unit.

The index counts the waypoints of the order as the controller gave them, from 0, and
stays the same when the remaining route is compiled again after a restore or a
re-route at the same order revision. A new order revision starts again from 0. The
event is as current as the position samples: one position period after DCS reaches
the waypoint (spec/sampling.md). No script runs in DCS for it.

## Events and the store

Events are numbered per campaign and kept in the campaign's event history
(spec/persistence.md, "Event history"). While no campaign
is active, events still happen: players connect and chat, and a plain mission runs.
Those events go to a log in the server-scoped store, with its own sequence numbers and
the retention of spec/config.md. The Events service reads it when the request asks for
the server log.

With persistence off (spec/config.md) the stores are in memory. They last as long as
the sidecar process. The author reads state with the capture call and writes it back
as specs after a restart.

## Lua tables in the API

The API carries DCS tables as JSON-like values (`google.protobuf.Struct`), whose keys
are strings and whose numbers are doubles. The sidecar converts both ways by one rule:

- To Lua: a key that is a decimal integer without leading zeros, such as `"3"` or
  `"-1"`, becomes that number key. Any other key stays a string. A list becomes a
  table with keys 1 to n.
- From Lua: a table whose keys are exactly 1 to n becomes a list. Any other table
  becomes an object, with each number key written in decimal.
- These do not survive a round trip and come back as strings: a number key that is
  not an integer, a number that is not finite (`"nan"`, `"inf"`, `"-inf"`), and a value
  Lua cannot send, which is its type name.

So a sparse table such as pylons 1, 3 and 5 is an object with the keys `"1"`, `"3"` and
`"5"`. A string key that looks like an integer cannot be sent. That no DCS table the
platform handles has one is UNVERIFIED (step S18 checks the tables it restores).

## Players

| Record | Contents |
|---|---|
| Player | Player key, display names with the time each was first seen |
| Seat occupancy | Player key, unit `EntityId`, seat, from, to. A multi-crew aircraft has several. |

Every event is stamped with the player keys occupying its initiator and its target at
that moment. How the player key is derived is fixed in S16 (E31).
