# World model

The agent, the sidecar and the public API share one model. DCS spawns, orders and
destroys groups; state lives on units. The model follows that.

## Identities

| Identity | Type | Assigned by | Rules |
|---|---|---|---|
| Campaign ID | `u64` | The sidecar, when a campaign is created | The wall-clock time of creation in milliseconds since the Unix epoch, or one more than the newest campaign's ID if that is larger, then moved forward until its tag is free ("Names"). So an ID is never used twice on a server, even when the stores are lost or in memory only. Never 0. Below 2^53. |
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
| Unit | `EntityId`, a member of one group | with its group | Observed state: position, attitude, life, and a player unit's ammunition at its last birth or takeoff. Dies alone. |
| Static | `EntityId` | yes | A spec and observed state. Spawned and destroyed alone. |
| Airbase | theater and DCS name | no | Owner (coalition) |
| Warehouse | theater and DCS name of its airbase | no | Stock |
| Mark, drawing, menu item | `EntityId` | yes | Player-facing objects. Set again after every mission load. |
| Extension | name | yes | A registered Lua chunk (spec/commands.md, "Extensions") |

The platform creates an airbase and its warehouse only as part of a static DCS makes
one of: a FARP. `coalition.addStaticObject` of a FARP makes an airbase under the static's
name, with the static's ID, and its own empty limited warehouse (LIVE,
docs/results/2026-11-16-person-tests-2.md). Such an airbase is listed and sampled like
the mission's from its `STATIC_SPAWN` until the static is destroyed or dies, when the
sidecar drops it. The airbase of a FARP the mission file places goes the same way when
its static goes; that it has its static's ID is UNVERIFIED, and an airbase whose ID is
not a static's of its name stays; a new mission generation drops it until the static is spawned again,
and its warehouse starts empty each time. The desired owner and stock the controller set
for it are kept by name, and sent again once a FARP spawned again under the name is
sampled. The platform destroys an airbase only by destroying its FARP. Owner and stock
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
np!<tag><entity>    a group
np.<tag><entity>    a unit
np#<tag><entity>    a static
```

| Part | Value |
|---|---|
| `np` | The platform's marker |
| `!`, `.`, `#` | The DCS namespace: group, unit, static |
| `<tag>` | The campaign's tag: 3 base-62 characters, leading zeros kept |
| `<entity>` | The `EntityId` in base 62, no leading zeros |

Base 62 uses the digits `0-9A-Za-z`, in that order of value: `0` is 0, `A` is 10, `a`
is 36, `z` is 61. There is no separator between the tag and the entity; the tag is
always 3 characters. A name is 7 to 15 characters, all of them in `[0-9A-Za-z!.#]`:
`np`, the marker, the tag and 1 to 9 entity digits (`EntityId` is below 2^53).

Example: in campaign 1,763,000,000,000 (tag `jqW`), `np!jqW1A` is group 72, `np.jqW1B`
is unit 73 and `np#jqW1C` is static 74.

### The campaign tag

The tag is a fixed function of the campaign ID. Stored campaigns depend on it, so it
never changes:

1. Take the campaign ID as an unsigned 64-bit integer `x`.
2. Apply the SplitMix64 output function, all arithmetic modulo 2^64:
   `z = x + 0x9E3779B97F4A7C15`; `z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9`;
   `z = (z ^ (z >> 27)) * 0x94D049BB133111EB`; `z = z ^ (z >> 31)`.
3. Take `v = z mod 238,328` (62^3).
4. Write `v` as 3 base-62 digits, most significant first: `v / 3,844`, `(v / 62) mod 62`,
   `v mod 62`.

Test vectors, checked by the shared codec's tests: `x = 0` gives `z =
0xE220A8397B1DCDAF` and tag `nwl`; 1 gives `blp`; 7 gives `hGx`; 1,700,000,000,000 gives
`ncp`; 1,763,000,000,000 gives `jqW`; 1,763,020,000,000 gives `nEd`; 2^53 - 1 gives
`pzz`.

When the sidecar creates a campaign, it takes the ID from the Identities rule, then
moves it forward by one until no campaign in its stores has the same tag. The ID is then
later than the creation millisecond by that many steps.

The agent and the sidecar build and parse names with one codec, in the transport crate.
The agent builds a name from the command header's `campaign` and an `EntityId`; the
sidecar reads the tag from a name and compares it with the active campaign's.

Known limits:

- A tag can equal the tag of a campaign the stores have never held, for example one from
  another server or from lost stores, with probability 1 in 238,328 per pair.
- The check at creation covers the campaigns the stores know: every campaign folder,
  readable or not, and the campaign IDs the server store keeps (deleted campaigns and
  its idempotency index; spec/persistence.md, "The server store"). A campaign folder
  removed by hand, or lost with the server store, leaves no trace, and its tag can be
  given to a new campaign.

In both cases the other campaign's objects, in a mission DCS still runs or one saved
with them, read as the active campaign's.

Why this form (docs/results/2026-11-11-first-dll-and-spikes.md, E21;
docs/results/2026-11-15-s15-world-commands.md, E21b; docs/results/2026-11-15-e21c-farp-name.md,
E21c):

- Unit names and static names are one namespace in DCS. A new object with a taken name
  removes the old holder without an event (LIVE). `EntityId` is unique across kinds
  within a campaign, so no two platform objects share a name.
- Group names are a separate namespace (LIVE). The marker keeps a group's name apart
  from its units' names anyway, and a static's apart from a unit's of the same entity.
- DCS names are case-sensitive, and names in this character set and with a leading
  `.`, `#` or `!` after `np` are accepted, found by name and read back exactly (LIVE,
  E21b on the Wine server, E21c on a Windows client). So base 62 holds a name in about
  half the characters of base 36, and players read shorter names in the unit dialog.
- Players see unit and group names in the unit dialog (LIVE, observed by the owner on
  a client; not read by a script). A name holds only the tag and the entity, never
  a player name, an author's label or anything else a player should not see. An
  author's label is stored in the sidecar and shown through the API.
- After a DCS restart the sidecar rebuilds the binding from names alone.

Rules:

- The shell writer never puts a name in this form into a mission.
- An object found in the world whose name parses with a tag other than the active
  campaign's is never bound to an entity of the active campaign. It is reported as
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
dynamic-spawn aircraft. The agent finds it by events and by the full enumeration. It is
observed and reported, never reconciled, ordered or destroyed by the reconciler.

The sidecar adopts the units players occupy, and the group of each, with a new
`EntityId` and `BIND` (spec/commands.md). A controller may send an adopted unit or
group an effect, and may give an adopted group marks, drawings and menu items. Other
unmanaged objects are observed and reported with no `EntityId`. An adoption lasts for
the agent's boot and the mission generation; after a sidecar restart the sidecar reads
it back from the agent's `OBJECT` records. When an adoption ends, the sidecar removes
from the store every mark, drawing and menu item whose target is that group, as a
journalled removal: a player's group ends with its generation, and so do the items
given to it.

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
(`RemoveEntity`) drops its members' marks, and `ResolveMember` releases one member.
`DEAD` writes a death fact for it; `RESTORE` clears its mark, so the group's next spawn
includes it and the group stays `lost` until that spawn. Either is journalled as
`resolve`, makes no event, and shows in a world watch; it changes only the store, so it
also works on a suspended campaign. A unit that is not held is refused with
`FAILED_PRECONDITION`. The API shows a member's mark as `Unit.left`, whether it was
reported or inferred.

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
Those events go to the server event log (spec/persistence.md, "The server store"), with
its own sequence numbers and the retention of spec/config.md. The Events service reads
it when the request asks for the server log.

**Where player keys are kept.** The player-key secret and the key-to-ucid pairs
("Players") are kept in the server store, in a folder only the sidecar's user may
enter (spec/persistence.md, "The server store"). A
pair learned in a step is written there by the end of the step; a failed write is
tried again at the next. Campaign backups do not include the server store, and no
diagnostics bundle reads it. With persistence off nothing is written, and the keys last
as long as the sidecar process.

**Kill attribution.** DCS sends a weapon kill's `KILL` and `SCORE` with the late death
event, up to 77.1 s after the object stops existing (LIVE,
docs/results/2026-11-14-e38-life-and-ammunition.md), when the
victim and maybe the killer have left the agent's table. The sidecar keeps, per mission
generation, a memory of the rows that left: object ID, `EntityId`, type, coalition, the
player keys that occupied it, and the time it left. It resolves a late event's object
IDs from that memory, stamps the players who occupied them before they left, and
dates the kill from the victim's `S_EVENT_BDA` when one came, else from the time its
row left, never from the late event's own time. The event keeps DCS's time as well.

**Launch attribution.** At `S_EVENT_SHOT` the sidecar maps the weapon's object ID to
the shooter's occupants. A `HIT` or `KILL` that names that weapon carries those keys in
`launch_players`, with the rule `WEAPON`. A gun's hit names no weapon object; it, and a
weapon the sidecar never saw shot, fall back to the initiator's occupants before it
left, with the rule `INITIATOR`. `initiator_players` is never rewritten. That `SHOT`
and `KILL` carry the same weapon object ID is UNVERIFIED (E31, E32).

With persistence off (spec/config.md) the stores are in memory. They last as long as
the sidecar process. The author reads state with the capture call and writes it back
as specs after a restart.

## Spawn state

Every spawn of a unit, a first materialization, a restore after a mission load or a DCS
restart, and a materialize after a dematerialize, gives it the state the store knows:
its position and heading in the unit table, and its life.

The life is `getLife` over `getLife0` as a percentage: `getLife` from the last sample or
capture, `getLife0` from the capture or the unit's `OBJECT` record. A `PutGroup` may give
a unit's `life_percent` instead, above 0 and up to 100 (at 0 a unit leaves DCS with no
death fact); it stands until DCS reads the unit's life. While the group's respawn is owed,
a sample or capture of the old spawn does not replace it. When the life
is below 99.95 %, the sidecar queues a `DAMAGE` right after the `GROUP_SPAWN`: ring order
runs it after the spawn, which names the unit. It sets the life in the spawn's frame
while the simulation is paused, and later with the simulation running (LIVE, E39,
docs/results/2026-11-16-e39-spawn-state.md); that it sets it in the spawn's frame with
the simulation running is UNVERIFIED. Until that `DAMAGE` is answered, a sample or
capture of the unit does not replace the life the store holds, so a read at the life the
spawn gave cannot overwrite it. The store holds the last life it read, or the
percentage a `PutGroup` asked for. A `DAMAGE` that DCS refuses or rejects, or that the
agent never took, is counted (`lives_lost`), and the sidecar writes a warning to its
standard error naming the unit's entity ID. A `DAMAGE` rejected only for its moment is not
counted: its spawn is sent again with a new `DAMAGE`. A `DAMAGE` that no `RESULT` answers
before a `HELLO` of the same agent boot and mission generation resolves it may have been
applied: the sidecar warns and does not count it, and the next sample shows the unit's
life. A new mission generation drops the hold without a count, because the restore runs
again with the next spawn. The count is in `Reconciler::counters()`; no other operator
surface carries the reconciler's counters. The next sample shows the unit's life.

The setter removes some units at some percentages: a speedboat left DCS at 50 % (LIVE,
E16), while a PERRY kept 50 % and a speedboat kept 80 % (LIVE, E39). A restore `DAMAGE`
that removes a unit leaves the member REMOVED or MISSING like any removal; no death fact
follows and the sidecar does not spawn the group again. Which other ship types and
thresholds remove a unit is UNVERIFIED, an open item.

A unit spawns with the fuel, ammunition, flares and chaff its payload or type gives
(owner's decision, after E39).

### Ammunition

`Unit.ammo` is the ammunition of a unit a player occupies, by `getAmmo` type name, as
the agent read it at the unit's birth or at its takeoff. `Unit.ammo_read` names the
event that asked for the read, `AMMO_READ_CAUSE_BIRTH` or `AMMO_READ_CAUSE_TAKEOFF`, and
carries the stamps of the read. Rows with the same type name add up. The newest read
replaces the one before. A controller reads it for a loadout check.

An AI unit has no ammunition read: its `ammo` is empty and it has no `ammo_read`. So has
a player unit before its first read. A player unit read with no ammunition rows has an
empty `ammo` and an `ammo_read`. Nothing sees a
rearm. No event named an AI M-1 rearmed by a truck (LIVE, E38,
docs/results/2026-11-14-e38-life-and-ammunition.md); whether DCS sends one for a player
aircraft rearmed by ground crew is UNVERIFIED, an open item for a live check. The
loadout is the one at the last birth or takeoff. A read whose record the ring drops is
made again on a later pump (spec/sampling.md, "Loadout reads"). The sidecar keeps it in memory with the unit. It is not stored on
disk, not part of a capture and not carried by a restore, and a new mission generation
clears it. A unit the store holds shows it on its `Unit`. The units the sidecar adopts
have no entity in the store (above, "Managed and unmanaged"), so the API shows no `Unit`
for them, and the World service lists store units only.

**Players.** A player's aircraft is usually an adopted unit, so the loadout also reaches
the API through the player. `Player.ammo` and `Player.ammo_read` (fields 8 and 9) carry
the loadout of the unit in `Player.unit_id`: the same type-name counts and the same read
as `Unit.ammo` and `Unit.ammo_read`, summed by type name when a record repeats a type.
They are empty and absent when no read was made and when `unit_id` is 0. They clear when
the player leaves the unit and at a new mission generation or agent boot. A managed unit
a player occupies shows the same loadout on its `Unit` and on the `Player`. The sidecar
keeps the table by DCS object ID in memory. Nothing of it is stored on disk.

**Loadout event.** Each `AMMO` record of a unit that players occupy, managed or adopted,
makes one `LoadoutEvent` (field 12 of `Event.body`). It carries the unit's `EntityId`,
its group's, the unit's type name, the keys of its occupants by seat, the counts and the
read. It is journalled after the `S_EVENT_BIRTH` or `S_EVENT_TAKEOFF` world event that
caused the read, and it is not made when the roster names no player in the unit. An
`EventFilter` passes it on `player_key`, when one of its `player_keys` matches, and on
`dcs_ids`, by the cause: `S_EVENT_BIRTH` (15) for a birth read, `S_EVENT_TAKEOFF` (3) for
a takeoff read.

**Reads before the bind.** An `AMMO` record can reach the sidecar before the sidecar
holds an `EntityId` for its object, such as one read before the player's slot joins the
birth that adopts the unit. The sidecar holds the read by DCS object ID until the
object is bound in the same agent boot and mission generation, then applies it: the
`Player` fields, the store's unit when it has one, and the `LoadoutEvent`. It holds at
most 64 reads. A newer read of an object replaces its held one, a full table drops its
oldest read, and a new mission generation or agent boot drops them all.

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
`"5"`. A string key that looks like an integer cannot be sent. None of these DCS tables
has one: `getDesc` and `getPosition` of an F-15C and an M-113, and
`getInventory` of three Caucasus airbases' warehouses (LIVE, DCS 2.9.31.29434 dedicated
server under Wine 10.0, docs/results/2026-11-15-s18-persistence-live.md). That no other
DCS table the platform handles has one, of these or other unit types and warehouses, is
UNVERIFIED.

## Players

| Record | Contents |
|---|---|
| Player | Player key, display names with the time each was first seen |
| Seat occupancy | Player key, unit `EntityId`, seat, from, to. A multi-crew aircraft has several. |

Every event is stamped with the player keys occupying its initiator and its target at
that moment.

**Player key.** The key is the first 16 bytes of HMAC-SHA-256 of the client's ucid,
keyed with a 32-byte random secret, written as 32 lowercase hex digits. The sidecar
makes the secret once per server and keeps it in the server-scoped store and its
backups, never in a bundle. An operator may give several servers the same secret so a
player has one key on all of them. Without the secret a key does not tell which ucid it
came from, and the same ucid has unrelated keys on servers with different secrets.

The sidecar keeps the map from key to ucid in the server-scoped store, so the
Moderation service can name a player by key. A raw ucid is accepted only as an admin's
input, to ban or unban a ucid. The API, the event stream, logs, metrics and
bundles carry keys and never a ucid; a bundle redacts any text with a ucid's form, 32 hex
digits.

**Seat occupancy.** A `SLOT` record (spec/records.md) gives a player's slot ID; an
`S_EVENT_BIRTH` gives a unit's object ID and its mission ID (`INITIATOR_MISSION_ID`).
The sidecar joins a slot to a birth whose mission ID equals the slot ID without its
`_N` seat suffix, in either order: a slot held when the birth comes, or a birth seen
before the slot. A held slot attaches to every later birth with its mission ID, so a
respawn in the same slot is covered. A slot change or a disconnect ends the occupancy;
so does the unit leaving the table. A slot that matches no birth leaves the player's
unit unknown: the sidecar counts it and does not guess. The sidecar subscribes each
occupied unit to the higher sampling rate (spec/sampling.md).

**Menu attribution.** A menu selection names the players who could have made it
(spec/records.md, "Layout: MENU").
