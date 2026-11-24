# World model

The agent, the sidecar and the public API share one model. DCS spawns, orders and
destroys groups; state lives on units. The model follows that.

## Identities

| Identity | Type | Assigned by | Rules |
|---|---|---|---|
| Campaign ID | `u64` | The sidecar, when a campaign is created | The wall-clock time of creation in milliseconds since the Unix epoch, or one more than the newest campaign's ID if that is larger, then moved forward until its tag is free ("Names"). So an ID is never used twice on a server, even when the stores are lost or in memory only. Never 0. Below 2^53. |
| `EntityId` | `u64` | The sidecar, from one counter per campaign | Starts at 1. Never reused within a campaign: the counter is kept in the store's own row, so an ID stays used after its entity's rows are purged ("Removal"). 0 means none. Below 2^53. |
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
| Static | `EntityId` | yes | A spec and observed state. Spawned and destroyed alone. One that dies stays, as a ruin ("Ruins"). |
| Airbase | theater and DCS name | no | Owner (coalition) |
| Warehouse | theater and DCS name of its airbase | no | Stock |
| Mark, drawing, menu item | `EntityId` | yes | Player-facing objects. Set again after every mission load. |
| Extension | name | yes | A registered Lua chunk (spec/commands.md, "Extensions") |

The platform creates an airbase and its warehouse only as part of a static DCS makes
one of: a FARP. `coalition.addStaticObject` of a FARP makes an airbase under the static's
name, with the static's ID, and its own empty limited warehouse (LIVE,
docs/results/2026-11-16-person-tests-2.md). Such an airbase is listed and sampled like
the mission's from its `STATIC_SPAWN` until the static is destroyed or dies, when the
sidecar drops it. A spawned FARP's airbase is gone at a new mission generation until
its static is spawned again, and its warehouse starts empty each time. The airbase of a
FARP the mission file places goes the same way when its static goes; that it has its
static's ID is UNVERIFIED. An airbase whose ID is not a static's of its name stays. The desired owner and stock the controller set
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

A group with a death fact for a member is never respawned with that member. A static
with a death fact is never spawned alive again: DCS gets its ruin ("Ruins").

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

Removing a managed group or static takes it out of the campaign for good. It goes to
campaign authority, the API no longer reports it, its units leave the area grid, and the
reconciler destroys any object of it that DCS still holds, as drift. A removed static's
ruin goes the same way ("Ruins").

The store keeps a removed entity's rows until it purges them. The purge takes a removed
entity `removed_keep_h` after its removal (spec/config.md) once DCS holds nothing of it:

- no object of it is bound;
- no command written in the loaded mission generation that names it or one of its units
  is unanswered;
- for a dead static, the store's marks say DCS holds no ruin of it in the loaded
  generation: it neither died there nor had a ruin spawn written there, or a destroy of
  it applied since, which clears both marks ("Ruins"). No enumeration finds a dead
  static, so this rests on the `STATIC_DESTROY` the reconciler sent for it;
- for anything else, a full enumeration used by the reconciler began after the removal.

The purge looks once a second and takes at most 1,000 entities in a step, each journalled
as `purged`. A body written without a removal time counts from the first step that reads
it. Removed marks, drawings and menu items are purged by the same time, once DCS holds
them no more in the generation and no command naming them is unanswered.

After the purge:

- **IDs.** The `EntityId` is never given again ("Identities"). A restore keeps the
  larger of the two stores' counters (spec/persistence.md, "Restore a backup").
- **Calls.** Every call whose entity is the ID answers `NOT_FOUND`, `RemoveEntity`
  included; an author treats a `NOT_FOUND` from `RemoveEntity` as done. An attack order
  whose target is removed, purged or unknown is refused with `INVALID_FIELD` on
  `order.attack.target_id` (spec/errors.md). A keyed repeat within
  `idempotency_keep_h` still gets its first answer (spec/errors.md, "Idempotency").
- **Late events.** World events resolve a gone object from the binding, then from the
  memory of rows that left ("Events and the store"), never from the store, so a late
  kill still names the purged IDs. A death fact for a purged unit is dropped and
  counted (`purged_deaths`).
- **Watches.** A watch that held the entity got its last state and a `Removal` when it
  was removed. One that reads the removal only after the purge gets `removed_id`.
- **Objects of a purged name.** An object in DCS whose platform name carries this
  campaign's tag and names an entity the store does not hold, below the store's next
  `EntityId`, is destroyed as drift: `GROUP_DESTROY` of the group its group's name
  names, or `STATIC_DESTROY` of the static, up to `destroy_retries` times in a
  generation, each counted (`orphan_destroys`). Only a platform name counts ("Names"),
  so an adopted unit, whose `EntityId` no name carries, never makes one; only this
  campaign's tag counts, so another campaign's objects and the server store's player
  records are never read as one. After a restore of an older backup, the objects of
  entities made after the backup are such objects: the restored campaign does not have
  them, and DCS is told to destroy them.

## Expiry

A dead group, not a static, expires `dead_keep_h` after it became `dead`
(spec/config.md; 0 never). The expiry is a removal by the sidecar: the group goes to
campaign authority and leaves the API, it is journalled as `removed` with cause
`expired`, and a watch that held it gets its last state and a `Removal` with cause
`EXPIRED`. Nothing is sent to DCS, neither a destroy nor a `CLEAR_DEBRIS`: wrecks are the
controller's. The purge then takes its rows by the rule of "Removal".

- A group with a member held across a mission generation is `lost`, not `dead`, and
  does not expire ("Respawn gate").
- A ruin never expires: only a `RemoveEntity` takes it ("Ruins").
- The expiry runs in the reconciler's plan of the active campaign, once a second, at most
  1,000 groups in a step: a dead group expires at the first plan after `dead_keep_h`,
  so while DCS is down or not yet ready it waits. A suspended campaign's dead groups
  expire at its next activation.
- A body written without a death time counts from the first step that reads it.

## Lifecycle

A managed group is in one of these states. A managed static follows the same states as
a group of one.

| State | Meaning |
|---|---|
| `virtual` | Simulated by the campaign, not in DCS |
| `materializing` | A spawn was sent and not yet acknowledged |
| `live` | In DCS, at least one member alive |
| `dematerializing` | Leaving DCS: a capture-and-destroy, or for a static a capture and then a destroy, was sent, and the destroy is not yet applied |
| `dead` | Every member has a death fact. Final. A dead static is kept in DCS as a ruin ("Ruins"). |
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
  `dead` entity changes nothing and is counted; the result of a ruin's spawn is read
  ("Ruins"). A destroy result for a `dead` entity
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

## Ruins

A managed static in state `dead` is a *ruin*. The store keeps it. The World service
reports it with `alive` false, state `dead`, and the position and heading it had. An
area query finds it where it stood. No cleanup removes it and it never expires ("Expiry"):
only a `RemoveEntity` does, after which its ruin is destroyed and its rows purged
("Removal"). It is never spawned alive again. A death fact clears a `MISSING` or `REMOVED` mark, so a ruin is never reported as
a removal (`Unit.left`).

The reconciler keeps DCS holding the static's dead object wherever DCS would hold a live
static: the static is not removed, its authority is DCS, and the loaded mission's
theater is its own.

| Case | The reconciler sends |
|---|---|
| The static died in the loaded generation | Nothing. Its own dead object stays in DCS and is the ruin. |
| DCS should hold the ruin and does not: after a mission load, a DCS restart, an activation, or a `Materialize` | `STATIC_SPAWN` with the static's table and `dead = true` (spec/commands.md), at the static's spec revision |
| DCS holds the dead object and should not: after a `Dematerialize` or a `RemoveEntity` | `STATIC_DESTROY` |
| The binding holds a live object of the static: a script revived it, or a spawn of the live static was answered after its death | `STATIC_DESTROY`, then the ruin's `STATIC_SPAWN` |

Only a `STATIC_DESTROY` the platform sends takes a ruin out of DCS. `CLEAR_DEBRIS` never
destroys a managed static, and the `world.removeJunk` it calls leaves a dead static,
one an explosion killed and one added dead (LIVE, spec/commands.md, "Layout:
CLEAR_DEBRIS").

- **What the sidecar knows.** The agent holds no row for a dead static: its `isExist`
  reads false (LIVE, docs/results/2026-11-17-person-fixes.md), so no `OBJECT` reports
  it and no enumeration finds it. The store keeps two marks for each dead static
  instead: the agent boot and generation in which a ruin's spawn was written, and those
  in which DCS holds the dead object. A death fact, an `OBJECT_GONE` `DEAD` for the
  object the store knows of a static already `dead`, and an applied ruin spawn set the
  second; an `OBJECT_GONE` `DEAD` for an older object sets nothing. An applied destroy
  clears both. They are written with the step, before its commands reach DCS
  (spec/persistence.md, "Writes"). So a sidecar that restarts sends
  nothing for a ruin the store marks as held, sends again a ruin's spawn that was
  written and not answered ("One ruin a generation"), and still destroys a ruin whose
  spawn it had written.
- **One ruin a generation.** A ruin's spawn carries the static's spec revision. While
  the agent holds that revision in the generation it answers `AT_REVISION`, which is
  `applied`. So the object of a static that died in the generation is never replaced,
  and a spawn sent again after a lost result makes no second ruin. A destroy sets the
  agent's revision to 0, so the spawn that follows a `Materialize` runs.
- **Budget.** A ruin's spawn counts against the materialization budget, and a ruin owed
  a spawn counts as waiting for the restore burst ("Authority";
  spec/persistence.md, "Restore burst").
- **Outcomes.** `refused`: the error is kept as the static's last error until a spawn
  of the ruin applies, and the spawn is not sent again at that revision in that
  generation. The sidecar keeps that in memory: a restarted sidecar tries once more.
  A rejection a resend cannot fix, such as `BAD_PAYLOAD` or a table over the size
  limit, is kept and shown the same way. `NOT_READY` holds it as it holds any spawn. `unknown` and `not_sent`: the next plan
  sends it again. The static stays `dead` whatever the outcome.
- **A destroy whose result is lost** may have run. The store then says only that DCS
  may hold the dead object: a static that should have its ruin gets the spawn again,
  answered `AT_REVISION` if the destroy did not run, and one that should not is sent
  the destroy again, up to `destroy_retries` (spec/config.md).
- **Created destroyed.** `PutStatic` with `dead` creates a static with a death fact, in
  state `dead` from the start: its spec, its death fact and the transition from
  `virtual` to `dead` are journalled together. A controller that wants to keep a unit's
  wreck puts such a static where the unit died. Only the store's state says whether a
  static spawns destroyed: `PutStatic` refuses a `dead` key in `StaticSpec.extra`, and
  a live static's spawn never carries one.
- **`dead` without a death in DCS.** A static the lost policy marks dead is a ruin too,
  and so is one a `ResolveMember` `DEAD` names, once the lifecycle makes it `dead`. In
  the generation the static left in, with no object of it in DCS, the agent still
  holds its revision, so the ruin's spawn is answered `AT_REVISION` and nothing is
  spawned. The next generation spawns the ruin. DCS may still hold the static's live
  object there, when the destroys of a `Dematerialize` were refused until the lost
  policy marked it dead. While that object lives, the binding holds it, and the table's
  row for a live object applies. When it dies, DCS holds the dead object, as for a
  static that died in the generation: it is the ruin, and the table applies.
- **A death the platform never reads.** A static that left as `MISSING` or `REMOVED`
  and whose death event the mission's end cut off has no death fact. It is no ruin: it
  is held like any such member ("Respawn gate").
- **A ruin is not observed.** Nothing tells the sidecar that another script removed a
  ruin. The ruin is spawned again in the next generation.

What a ruin looks like is DCS's choice of model for a dead static of the type (LIVE,
docs/results/2026-11-17-person-fixes.md, on a Windows client of the Wine server):

| Type added with `dead = true` | A client sees |
|---|---|
| `FARP Fuel Depot`, `Shelter`, `FARP Ammo Dump Coating`, `F-15C`, `UH-1H` | Destroyed |
| `Windsock` | Intact |
| `BRDM-2` | Alive; nothing after the client rejoins |
| `speedboat` | Not added: DCS refuses it |

So the ruin of a type with no destroyed model looks intact after a restore, and a ground
vehicle's wreck cannot be kept as a static spawned dead. Which other types behave as
the Windsock or the BRDM-2 does is UNVERIFIED. In the generation a static dies in, a
client sees the wreck DCS made of it: LIVE for a BRDM-2 killed by an explosion (the
same results) and for a FARP Fuel Depot (docs/results/2026-11-16-person-tests-2.md),
UNVERIFIED for other types. Whether a FARP added dead
makes its airbase is UNVERIFIED.

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

## Authority

Each managed group and static is simulated by DCS (materialized) or by the campaign
(virtual): its authority. Only the controller changes it, with `Materialize` and
`Dematerialize`; a `PutGroup` with `virtual` starts it under the campaign. The sidecar
never changes it and holds no rule on when to: hysteresis and every other policy are the
controller's. The lifecycle above carries a change out.

**Interest queries.** `QueryArea`, or a `WorldFilter` with a center and a radius, gives
every group with a unit within the radius, and every static within it, virtual or not,
from the store's grid. A virtual unit is where the store last held it. A dead unit is
left out; a dead static is a ruin and is found where it stood.

**Handover.** A dematerialize captures each unit in the frame that destroys it
(`GROUP_CAPTURE_DESTROY`; for a static, a `CAPTURE`, then a destroy), so the store holds
the position, heading and life DCS had at that moment. A materialize spawns from the
state the store holds (see "Spawn state"). A capture that fails takes the last
checkpointed state instead (spec/commands.md, `GROUP_CAPTURE_DESTROY`).

**Materialization budget.** Outside the restore burst (spec/persistence.md), at most
`materialize_per_frame` spawns (`GROUP_SPAWN`, `STATIC_SPAWN`) are unanswered at a
time: from the write to the ring until the sidecar reads the spawn's `RESULT`, or the
spawn resolves `unknown` or `not_sent`. The next spawn is written in the step that reads
a result. So at most that many reach DCS in one frame, whatever waits in the ring before
them, and a larger value pipelines spawns. Destroys have no such limit: the agent's
commanded-work budget (`commanded_ms`, spec/config.md) spreads them over frames like any
command. The burst turns on when more groups are already `materializing` than the
budget, as a mission load leaves every live group; a group the controller moves to DCS
while paused leaves `virtual` within the budget.

**Bound units.** A unit is bound while the sidecar holds a DCS object for it: from its
`OBJECT` record until its `OBJECT_GONE`. `live` says the spawn was applied, not that
every unit is bound. The agent writes the `OBJECT` record of each unit of a spawned
group that it can read before the spawn's `RESULT` (spec/commands.md, "Layout:
GROUP_SPAWN"), so those units are bound when the group turns `live`. A unit it could not
read then binds when its birth resolves (spec/sampling.md, "Tiers") or at the next full
enumeration, and so does a unit whose `OBJECT` the agent's event queue dropped: the
`GAP` forces that enumeration (spec/records.md, "Delivery classes"). A static binds when
its birth resolves; one spawned dead has no `OBJECT` and is never bound ("Ruins"). What
a call on a unit that is not bound does:

| Call | On an entity DCS holds no object of |
|---|---|
| `DoEffect` with a unit or a group as its scope, a damage effect, `Capture` | Refused: `FAILED_PRECONDITION`, reason `ENTITY_NOT_FOUND`. A group counts as bound when one of its units is |
| An attack order whose target is not bound | Kept: the order is compiled and sent when the target binds |
| A mark, a drawing or a menu item whose scope is a group with no bound unit | Kept: it is sent when one of the group's units binds |
| The capture of every group that a campaign operation makes (spec/persistence.md, "Campaign operations") | Leaves out a group with no bound unit: the store keeps the state it holds for it |
| Any other order or option of the group | Sent: the agent finds the group by its name |

The sidecar queues none of the refused calls; the controller sends them again.

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
a player unit before its first read, and a player unit the agent does not mark: the
aircraft of a player who hosts the server from the client, and the other cases that
spec/sampling.md, "Loadout reads", lists. A player unit read with no ammunition rows has
an empty `ammo` and an `ammo_read`. Nothing sees a rearm. No event named an AI M-1 rearmed by a truck (LIVE, E38,
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

## Group templates

`PutGroupFromTemplate` makes a group from a group template of the DCS Mission Editor.
DCS ships its group templates in one Lua file,
`MissionEditor/data/scripts/templates.lua`: one assignment
`templates = { ["<name>"] = {...}, ... }` of literal values (ED-CODE,
docs/results/2026-11-18-spawn-template.md). That the editor saves a user's own
templates in a file of the same form is UNVERIFIED. The administrator provides the
file; the
call names it by its path as the sidecar opens it, in a folder that
`[missions] template_dirs` lists (spec/config.md), under the path rules of
`SetCampaignTemplate` (spec/persistence.md, "Set a template").

The sidecar reads the file once, converts the named template to a group spec, and
journals the group as `PutGroup` journals one of that spec. The store keeps the spec
alone: a mission load, a restore, an export and a restarted sidecar never read the file,
and the group is changed afterwards with `PutGroup`. The path and the template's name
are written to the sidecar's standard error. A repeat under an answered idempotency key
returns the first answer whatever has become of the file.

The file is read as data, by the reader that reads a mission template's members. No
code from it runs. The reader takes one assignment of a name to a value and nothing
after it. A value is a table constructor, with `[key] = v`, `name = v` and positional
fields separated by `,` or `;`; a double-quoted, single-quoted or long-bracket string;
a number; `true`, `false` or `nil`. Tables nest at most 200 deep, and `--` comments are
skipped. Anything else refuses the file with the line it is on: a second statement,
`local`, `return`, a call, a function, an operator, a name used as a value. So is a
file over 1 MiB. The file Eagle Dynamics ships is 185,899 bytes (ED-CODE). A file the editor
saved for a user has not been read; one in another form is refused.

| In the file | In the spec |
|---|---|
| The template's key | The template the request's `template` names. Of two with one name, the later |
| `type` | Must be `"vehicle"`, which gives a ground group. The shipped file has no other value (ED-CODE); a template with another is refused. Which values the editor saves for other groups is UNVERIFIED |
| `country` | The DCS country number, unless the request gives `country` |
| `name` | Not used. An empty `label` in the request takes the template's key |
| `units[i].name` | The unit's DCS type |
| `units[i].dx`, `dy` | The unit's place: `dx` meters north and `dy` meters east of the request's `anchor`, on the anchor's theater and at its `alt` |
| `units[i].heading` | The unit's heading, radians |
| `units[i].skill` | The unit's skill. One that is not text is left out |
| Any other key | Dropped, and named in the sidecar's standard error |

The request's `heading`, radians clockwise from north, turns the layout about the
anchor and is added to each unit's heading. The heading is then the truncated remainder
of the sum by 2π, as the editor's `math.mod` gives it (ED-CODE,
docs/results/2026-11-18-spawn-template.md): it keeps the sum's sign, so a turn to the
left can give a negative heading. At 0 each offset and heading is the file's own
number. A unit gets the platform's own name, as every unit does ("Names"); the file
names none.

The sidecar adds `dx` to the anchor's `north` and `dy` to its `east`, on these facts:

- The Mission Editor saves a unit's `dx` as its mission `x` less the group's and its
  `dy` as its `y` less the group's, and turns a group by the same rotation (ED-CODE,
  the same document).
- A unit table's `x` and `y` are the world's `x` and `z`: a group spawned again from
  its sampled place stood within 25 m of it (LIVE,
  docs/results/2026-11-15-s18-persistence-live.md).
- The API names DCS `x` `north` and DCS `z` `east` ("Positions and theaters").

A unit made by this call stood on DCS within 3 mm of the place the request and the
file give, without a turn and with a quarter turn (LIVE, DCS 2.9.31.29434 dedicated
server under Wine 10.0, docs/results/2026-11-18-spawn-template.md).

The editor also saves a `payload` for a unit that has one, and its script names the
group types `"plane"` and `"helicopter"` (ED-CODE); the `type` it saves for them is
UNVERIFIED. A `payload` is dropped as any other key.

A country the loaded mission does not hold, or a unit type DCS does not have, is DCS's
refusal: it shows as the group's last error, as with `PutGroup`.

## Static templates

`PutFromStaticTemplate` makes every vehicle group and static of a static template of the
DCS Mission Editor, a `.stm` file, in one store commit or none. The editor saves the
objects of the mission it has open as one assignment `staticTemplate = {...}` of literal
values, in `StaticTemplate/<name>.stm` of the Saved Games profile; DCS ships 23 in
`MissionEditor/data/scripts/StaticTemplate/` (ED-CODE,
docs/results/2026-11-23-static-templates.md). The file is the mission's own tree:
`coalition`, then each coalition's `country` list, each country's DCS number `id` and
its kinds (`vehicle`, `static`, `ship`, `plane`, `helicopter`, `complex`), each kind's
`group` list. Every place is an absolute mission `x` and `y` on the file's `theatre`.
The editor loads a static template where it was saved, with no anchor and no turn
(ED-CODE).

The file is named, read once and kept as `PutGroupFromTemplate` names, reads and keeps
its file ("Group templates"), by the same reader with the root `staticTemplate` and the
same 1 MiB cap; the largest shipped file is 147,286 bytes (ED-CODE). The store keeps
the converted specs alone. Each entity is then an ordinary group or static: a group's
orders are set afterwards with `SetOrder`.

The file's order is the order of its text: the answer lists the vehicle groups in the
order they appear in the file, across its coalitions and countries, each with its unit
IDs in the file's unit order, then the statics likewise. Each entry carries its spec
revision, and `revision` is the store revision of the call's one commit.

| In the file | In the specs |
|---|---|
| `theatre` | The theater of every entity when the request has no `anchor`. It must be present and be the campaign's theater. With an anchor it is not used |
| `coalition.<c>.country[i].id` | The DCS country number of every entity under that country, unless the request gives `country`. One that is not a whole number from 0 to 4,294,967,295 refuses the file. The country's `name` is not used |
| `vehicle.group[j]` | A ground group. Its `name` is its label; every other group key, `route`, `task`, `tasks`, timing and activation keys included, is dropped and named in the sidecar's standard error |
| `vehicle.group[j].units[k]` | A unit: `type` its DCS type, `x` meters north, `y` meters east, `heading` radians, `skill` when it is text. `unitId` and `name` are not used; every other key is dropped and logged. The unit's label is empty |
| `static.group[j].units[k]` | One static per unit: `type`, `x`, `y` and the unit's `heading` as for a vehicle unit; `category`, `shape_name`, `rate`, `mass`, `canCargo` and `livery_id` go to the static's further keys; the group's `dead = true` creates it destroyed ("Ruins"); its label is the group's `name`. Other keys are dropped and logged |
| `ship`, `plane`, `helicopter`, `complex` | A file with any group of these kinds is refused whole |
| A static with `linkOffset` true or a route point with `linkUnit` | Refused: the link to a unit is not built |
| A static of category `Heliports` or `GrassAirfields`, or with a `heliport_` or `grassairfield_` key | Refused: such a static makes an airbase, which the world model has not seen |
| `name`, `desc`, `localization`, `requiredModules`, `customFormations` | Not used |

A coalition or country with two entries under one key, a list with two entries under
one number, and a static value the store cannot encode refuse the file.

The shipped files hold only vehicle groups (ED-CODE). The form of a static is taken
from the editor's code that saves one, and is UNVERIFIED against a saved file. That the
editor writes x as north and y as east follows from "Group templates": a unit table's
`x` and `y` are the world's `x` and `z` (LIVE). Two units and two `Fortifications`
statics made by this call stood on DCS within 15 µm of the places the file and the
request give, in place and at an anchor with a quarter turn (LIVE, DCS 2.9.31.29434
dedicated server under Wine 10.0, docs/results/2026-11-23-static-templates.md).

The layout's origin is the center of the box that bounds every unit's and static's `x`
and `y`. Without an `anchor` every entity stands where the file has it, at altitude 0.
With one, the origin goes to the anchor: every entity keeps its offset from the origin,
on the anchor's theater and at its altitude. Either way the request's `heading` turns
the layout about the origin and is added to every unit's and static's heading, by the
rotation and the remainder of "Group templates"; at 0 every heading is the file's own
number, and without an anchor every place too. A non-empty `label` is prefixed to each
entity's label as `<label>/<name>`.

Every check of every entity runs before the first store write: the checks of `PutGroup`
and `PutStatic` on each converted spec, the size of each spawn command, each value's
encoding, and that the campaign has an entity ID left for every group, unit and static.
A refusal makes no entity.

One store commit is not one DCS spawn. The entities materialize as every entity does:
outside the restore burst at most `materialize_per_frame` spawns are unanswered at a
time (spec/config.md), and a campaign that is paused with more entities waiting than
that spawns them in the restore burst (spec/persistence.md, "Restore burst"). Each
entity can fail on its own in DCS, a country the mission does not hold or a type DCS
does not have, and shows it in its own last error.

## Payload presets

A unit of a `PutGroup` spec can name a payload preset of the DCS Mission Editor in
`payload_preset`, instead of writing its pylons in `extra.payload.pylons`. The sidecar
reads the preset's pylons into `extra.payload.pylons` and journals the group as
`PutGroup` journals one with those pylons written by hand. The field is a request
field: the store never keeps it, so a spec read back from the World service or a
capture has none. The file's path and the preset's name are written to the sidecar's
standard error. Only `PutGroup` reads the field; the template calls build their specs
themselves and never set it.

**Files.** The editor gathers presets by aircraft type from
`MissionEditor/data/scripts/UnitPayloads/` and from `UnitPayloads/` of every module in
the install, then merges the user's presets over them by preset name, from the folder
`ConfigHelper.getWriteDirUnitPayloads()` names, which is `MissionEditor/UnitPayloads/`
of the Saved Games profile (the path is UNVERIFIED). Each file returns one table: the
type in `unitType`, else `name`, and the presets in `payloads`, each with a `name`, an
optional `displayName`, `pylons` (each `{CLSID, num, settings?}`) and `tasks`
(ED-CODE, `MissionEditor/modules/me_loadoututils.lua`,
docs/results/2026-11-24-payload-presets.md). The sidecar does not merge: a request
names one file, by its path as the sidecar opens it, in a folder that
`[missions] template_dirs` lists (spec/config.md), under the path rules of
`SetCampaignTemplate` (spec/persistence.md, "Set a template"). The sidecar never lists
a folder of the DCS install; an administrator copies the files to use, or lists the
profile's `MissionEditor/UnitPayloads` folder.

**Form.** The editor runs each file as Lua. Of the 153 files Eagle Dynamics ships, 140
start with `local unitPayloads =`, 7 start with another `local` statement and 6 start
with a blank line or a comment; all end with `return unitPayloads`, and 5 hold lines
with `function`, `require` or `..` (ED-CODE, the same document). How many the reader
takes whole is UNVERIFIED. The sidecar reads the file as data and runs none of it. It
takes exactly `local <name> = <value>`, then `return <name>` with the same name, with
only white space and comments around them: `<name>` is not a Lua reserved word, and no
`;` follows the `return`. The value is read as a group template's is ("Group
templates"). Anything else refuses the file whole, with the line of the first
statement that is not this form and the advice to save the preset from the editor.
That the editor saves a user's preset in this form is UNVERIFIED. A file over 1 MiB is
refused; the largest shipped file is 223,869 bytes (ED-CODE).

| In the file | In the spec |
|---|---|
| `unitType`, else `name` | Must equal the unit's `type` exactly, case included |
| `payloads` | Two entries under one key refuse the file |
| `payloads[i].name` | The preset the request's `name` names. Not `displayName`. Two presets with one `name` refuse the request |
| `payloads[i].pylons` | A table, which may be empty; none refuses the request, and so do two entries under one key |
| `payloads[i].pylons[j].num` | The pylon: the key of `extra.payload.pylons`, as a decimal string ("Lua tables in the API"). A whole number from 1 to 1,000; two entries for one pylon refuse the request |
| `payloads[i].pylons[j].CLSID` | The pylon's `CLSID`, a non-empty string |
| `payloads[i].pylons[j].settings` | The pylon's `settings`, as the file has it. A value that is not a table, a table holding `nil` (Lua keeps no such key), a table the store cannot encode, or one the API cannot carry unchanged refuses the request |
| `displayName`, `tasks`, any other key | Dropped, and named in the sidecar's standard error |

A preset with an empty `pylons` table gives the unit no pylons. When the editor applies
a preset, it merges each pylon's `settings` over the launcher's defaults (ED-CODE,
`MissionEditor/modules/me_loadoututils.lua`, `getUnitPylons` and
`getAppropriateSettings`); the sidecar passes the file's `settings` as they are, and
none for a pylon that has none. A preset
holds no `fuel`, `flare`, `chaff` or `gun`; the request's `extra.payload` keeps those it
gives. A unit spawned with pylons and no `fuel` had 45 % of its internal fuel: an
F-4E-45MC read `getFuel` 0.45 of a `fuelMassMax` of 5,510.5 kg (LIVE, the same
document). A controller that wants another amount gives `extra.payload.fuel` in
kilograms, as the editor saves it (ED-CODE, `MissionEditor/modules/me_payload.lua`).

A request that names a preset and also carries `extra.payload.pylons` is refused, and
so is one whose `extra.payload` is not an object. A controller that sends a read-back
spec again with a preset removes its pylons first.

Each distinct path of a request is read and parsed once, before the call takes the
store's lock, and each preset asked of it is converted then; a parsed file is dropped
before the next is read. A file's presets are indexed by name when it is parsed, so a
lookup does not scan it. A request names at most 16 distinct paths; one with more is
refused, field `spec.units`, and no file is read. Every refusal comes before the first
store write and makes no group. A repeat under an answered idempotency key returns the
first answer, whatever has become of the file. A replacement (`group_id` not 0) that
names a preset reads the file as it is then.

Two F-4E-45MC aircraft spawned in the air, one from a preset of three pylons, two of
them with fuze `settings`, and one with the same pylons written by hand, read the same
ammunition from DCS (2 Mk-84, 1 AIM-7E-2 and 639 gun rounds), and their stored `extra`
was the same, with the hand-written pylons and their `settings` (LIVE, DCS
2.9.31.29434 dedicated server under Wine 10.0,
docs/results/2026-11-24-payload-presets.md). DCS spawned both with `settings` in their
pylons; whether it applied the fuze settings is UNVERIFIED, since the scripting API
reads no fuze.

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
