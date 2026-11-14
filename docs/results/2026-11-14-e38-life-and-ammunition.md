# E38: reading unit life and ammunition, and telling a death from a removal

| | |
|---|---|
| Step | E38: is there a cheaper or better way to read a unit's life and ammunition than `Unit.getLife` and `Unit.getAmmo`, or a signal that one changed; and is there a read that says at once that an object died |
| Date | 2026-11-14, 00:09 to 01:40 UTC |
| DCS | 2.9.31.29434 on both platforms (LIVE, `cargo xtask target check`). The Wine server runs branch `dcs_server.release`, dedicated server. The Windows machine runs the client, hosting a private session |
| Platform | Windows NT 10.0.26200, rendering at 60 frames a second. Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker, 110 frames a second |
| Scripts | `lua/experiments/e38/`: `recon.lua`, `coverage.lua`, `others.lua`, `classify.lua`, `revive.lua`, `rules.lua`, `cost.lua`, `store.lua`, `hook-reads.lua`, `hook-log.lua`, `rearm.lua`, `trucks.lua`, each through `cargo xtask eval <target> <state> <file>` on a `--dev` build |
| Rehearsal | `lua/tests/e38.lua`, in `mise run check` |
| Mission | A shell generated from `templates/caucasus.miz`. The server had no client connected |
| Result | No candidate replaces today's reads: no read costs half of `Unit.getLife` or less, no bulk read exists, and no signal covers what events miss. A read does tell a killed ground unit from a removed one at the moment it ceases to exist, on both platforms. It does not carry over to statics, cargo, aircraft and ships as it stands, and a script can bring a burning unit back |

Every figure below is LIVE on this build unless tagged otherwise. "Both" means the
Windows client and the Wine server. Each probe ran on both, except `others.lua`
and `trucks.lua`, which the text marks. Delays are in model time, except those of
`revive.lua`, which the text marks. Some Windows results come from earlier versions of
`coverage.lua` and `revive.lua`, which the text marks. The Windows figures come from a
client hosting a session; no Windows dedicated server was run. The server paused and resumed several times in each run
(`S_EVENT_SIMULATION_FREEZE`), as it does with no client.

## The rule and the verdict

A candidate replaces today's read only if it exists in the dedicated server's states
under Wine and on Windows, follows splash damage, and either costs at most half of
today's read a unit or gives a change signal that removes the need to poll.

| Read | Verdict | Why |
|---|---|---|
| Life: `Unit.getLife` | Stays | Every other read of a unit's condition costs between 0.9 and 1.25 times as much. None is a bulk read. `S_EVENT_HIT` announced every change a weapon caused to a ground unit or a static; a cargo crate killed by gunfire sent no event |
| Ammunition: `Unit.getAmmo` | Stays | It is the only read. Shot events announce every fall. Nothing announces a rise |

## Where the F10 map's life readout comes from

ED-CODE, both installs: `Scripts/UI/F10View/AWACSInfo.lua`. The simulation calls the
dialog's `updateDynamicData(alt, speed, hdg, coordsStr, task, mag, life_relative)` with
the life of the one selected unit as a fraction. The value is computed in the
simulation and pushed into Lua; no Lua function reads it. `setLife` and
`setLifeAllowed` in the same module are functions the simulation injects for a
game-master slider. The newer `AWACSInfoNew.lua` has no life argument. The "show life
bar" button on the map's toolbar has no Lua handler.

## Every way found, compared

Cold cost is one batch of 20 units a frame, the read changing from frame to frame, 150
batches, 600 idle units (500 M-113, 100 M-1 Abrams), the median with an empty Lua call
taken off (`cost.lua`). Each read took a plain table `{ id_ = <object ID> }`.

| Read or signal | State | Client | Server | Cold, Windows | Cold, Wine | Bulk | Verdict |
|---|---|---|---|---|---|---|---|
| `Unit.getLife` | `server` | yes | yes | 0.28 µs | 0.74 µs | no | Today's read |
| `Unit.getFuel` | `server` | yes | yes | 0.23 | 0.70 | no | For comparison |
| `Unit.isDead` (undocumented) | `server` | yes | yes | 0.25 | 0.68 | no | Same cost; never changes while the unit exists. Useful after it ceases to exist, see below |
| `Unit.isAlive`, `isBroken`, `isEffective` (undocumented) | `server` | yes | yes | 0.26 to 0.30 | 0.71 to 0.75 | no | As `isDead` |
| `Unit.isActive`, `Object.isExist` | `server` | yes | yes | 0.26, 0.31 | 0.67, 0.79 | no | Same cost |
| `Unit.getDrawArgumentValue` | `server` | yes | yes | 0.32 | 0.91 | no | One animation argument a call; no known argument is a life value |
| `Unit.getLife0` | `server` | yes | yes | raises on a plain table | | no | Defined in Lua as `getDesc().life` (ED-CODE, `Scripts/ScriptingSystem.lua`) |
| `Unit.getAmmo`, M-113 (2 rows) | `server` | yes | yes | 3.1 | 10.6 | no | Today's read |
| `Unit.getAmmo`, M-1 Abrams (6 rows) | `server` | yes | yes | 6.9 | 25.4 | no | Each row carries a full description table (`recon.lua`) |
| `DCS.getUnitPositionByObjectId` | hook, `server` | yes | yes | 0.22 | 0.70 | no | For comparison |
| `Export.LoGetObjectById` | hook, `server` | yes | yes | 10.5 | 41.8 | `LoGetWorldObjects` | 14 fields, none of life or ammunition (`recon.lua`; ED-CODE `Scripts/Export.lua`) |
| `DCS.getUnitProperty` | hook, `server` | yes | yes | 0.17 warm | 1.0 warm | no | By mission ID, properties 0 to 19 answer or are empty and 20 to 40 are invalid (`recon.lua`). All are the mission's data. No life, no ammunition |
| `c_unit_life_less(id, percent)` | `mission` | yes | yes | 0.33 warm | 1.2 warm | no | True below `(life - 1) / (life0 - 1)` as a percent for an M-113. Needs a cross-state call |
| `c_unit_damaged(id)`, `c_unit_dead(id)`, `c_unit_hit(id, n)` | `mission` | yes | yes | 0.31 to 0.38 warm | 1.1 to 1.2 warm | no | `damaged` is life below full. `dead` is true for a killed unit and for a removed one. `hit` stayed false after an explosion |
| `c_group_life_less(id, percent)` | `mission` | yes | yes | 1.4 warm | 4.7 warm | a group | A count of units, not life; see below |
| `DebriefingEventsData` (undocumented) | `server` | yes | yes | `getEvents` 19.5 µs at 234 entries | 62 µs at 243 entries | all events | Fed by the simulation while the mission runs. The same events as the world handler, at the same time. `getEvents` copies the whole list |
| `onGameEvent('kill', ...)` | hook | yes | yes | | | | Came once for each `S_EVENT_KILL`. Names unit types, not units |
| `onGameBdaEvent`, `onDebriefingEvent` | `gui` | yes | not as a user callback | | | | Handlers of ED's GUI script (ED-CODE, `MissionEditor/GameGUI.lua`), not user callbacks. See below |
| `AWACSInfo.updateDynamicData` | `server` | selected unit | UNVERIFIED | | | no | The F10 readout; one unit, only while a person has it selected (ED-CODE) |
| `LoGetPayloadInfo`, `LoGetSnares` | export | own aircraft | no | | | | The player's own aircraft only (ED-CODE) |

The client's and the server's states hold the same `DCS.*`, `Export.*`, scripting and
`mission` functions: `cargo xtask api-dump` on the client against `lua/mock/api/`
differs only in GUI dialogs. A back-to-back pass over every unit reads 0.11 µs for
`getLife` on Windows and 0.51 under Wine, with the empty call of 0.02 and 0.09 µs
included, which is why the cold figure is the one to budget.

### The trigger rules

`rules.lua`, both. The `mission` state holds the functions a mission's trigger
conditions compile to (ED-CODE, `MissionEditor/modules/me_predicates.lua` and
`me_trigrules.lua`). They answered for a group added at run time, by mission ID.

- `a_unit_set_life_percentage(id, 50)` set an M-113's life from 2.85 to 1.5, and `100`
  set it to 3. `Unit.getLife` followed both and no world event came, on both.
- `c_group_life_less` turned true below 100 for a fresh group of four, 75 after one
  died and 50 after a second was removed. A group added again under a name used earlier
  in the mission read 62.5, 37.5 and 25 (Windows): units of the earlier group still
  count. The cause is UNVERIFIED. In that run the group's first unit also got the
  object ID of the earlier, killed unit of the same name (1000056) and read full life
  (3, `c_unit_dead` false), and the group read 1.4 µs warm. So adding a group under a
  dead unit's name is a second way a dead ID reads alive again; the mechanism is
  UNVERIFIED.
- A rule costs at least what `getLife` costs and needs a cross-state call of 13 µs
  (Windows) or 46 µs (Wine) for the chunk, so none is a cheaper read.

### The debriefing store

`store.lua` and `coverage.lua`, both. `DebriefingEventsData` (ED-CODE,
`Scripts/UI/DebriefingEventsData.lua`) is loaded in the `server` state and the
simulation feeds it through `addEvent`, on the dedicated server too. Types seen: hit,
bda, dead, kill, shot, start shooting, end shooting, score, crash, eject, pilot dead,
land, engine startup, engine shutdown, ai abort mission, mission start. An entry
carries `type`, `t`, `event_id`, `initiator_object_id`, `target_object_id`, the mission
IDs, unit types, coalitions and `weapon`. Repeated hits merge into one entry: UNVERIFIED, the counts in `coverage.lua` matched the world hits one for one (56 of 56 on Windows, 64 of 64 on Wine). For every
object that ceased to exist, each entry arrived within the same 0.1 s as the world
event of the same kind, so the store is no earlier and adds no event. `getEvents`
returns a new table on each call. `end shooting` carries `ammo_consumption`, which was
0 on all 33 machine-gun bursts of M-1 Abrams tanks in each store, so it is no round
count for a ground unit.

### The GUI's handlers

`hook-log.lua`, run in the hook state and in the `gui` state. On the Windows client DCS
called the GUI script's own global `onDebriefingEvent` in the `gui` state once for each
world event (for example 56 for 56 `S_EVENT_HIT`) and also 21 times for group option changes, which have no world event, with a table holding `id`, `type`,
`typeName`, `time` and `linked_event_id`. `onGameBdaEvent`, whose "damage" event
carries a percent (ED-CODE, `MissionEditor/modules/mul_bda.lua`), was not called in a
fight between AI units. Neither arrived as a user callback on either platform. The
dedicated server's GUI script defines neither (ED-CODE,
`MissionEditor/dedicatedServerGUI.lua`); whether DCS would call one defined there is
UNVERIFIED.

## What announces a change

`coverage.lua`, 360 s of model time, three runs on Windows and two on Wine, one of the committed script on each platform
(the last), the others earlier versions; the table is the last run of each. Ten times a second it read each unit's life, the four flags and, for the
shooters, the sum of `getAmmo`'s counts. It matched each change with the world events
that named the unit from 1 s before the sample that saw the change to 0.1 s after it.
Every event that came was logged, with no filter by type.

| Cause | Changes, Windows | Changes, Wine | Event naming the unit |
|---|---|---|---|
| Tank shells and machine guns on three BTR-80 | 11 | 22 | `S_EVENT_HIT`, target, every one |
| Howitzer shells among nine parked trucks (splash) | 30 | 20 | `S_EVENT_HIT`, target, every one |
| A burning wreck's explosion beside four M-113 | 4 | 4 | `S_EVENT_HIT`, target, every one; the wreck is the initiator |
| `trigger.action.explosion`, power 30, 6 m from a row of five M-113 | 5 | 5 | none |
| `Unit.destroy`, `Group.destroy` | 2 | 2 | none |
| `a_unit_set_life_percentage`, down and up (`rules.lua`) | 2 | 2 | none |
| Ammunition falling: a tank, three howitzers | 54 | 53 | A shot event as initiator, every one |
| Ammunition rising: the tank rearmed | 5 | 5 | none |

- `Unit.getLife` followed every one of these, splash included.
- A hit event was logged within about 0.1 s of the sample that saw the change, before
  or after it.
- A main-gun or howitzer round came with `S_EVENT_SHOT`. A machine-gun burst falls
  between its `S_EVENT_SHOOTING_START` and `S_EVENT_SHOOTING_END`; a fall late in a
  burst was matched by one of those two. The match relies on bursts shorter than the
  1 s window.
- `getAmmo` for an F-15C added with 60 flares and 60 chaff listed the gun only
  (`others.lua`, Windows). Countermeasures are not readable through it.

## Dead, burning or removed: ground units

`coverage.lua` and `classify.lua`, both.

A ground unit that is killed ceases to exist before its fatal events are delivered.
The handler read `Object.isExist` on the target as each event arrived: the target was
already gone for 12 of 12 `S_EVENT_BDA` and 12 of 12 `S_EVENT_KILL` on each platform,
and for the 12 fatal ones of 56 (Windows) and 64 (Wine) `S_EVENT_HIT`. Some units then
burn. Their death events come when the burning wreck explodes.

| Last run of `coverage.lua` | Windows | Wine |
|---|---|---|
| Kills (12 in the fight, by shells and by a burning wreck's explosion: 4 on Windows, 1 on Wine; 1 by the scripted explosion) | 13 | 13 |
| `UNIT_LOST` and `DEAD` within 0.1 s of ceasing to exist | 5 | 5 |
| The same events later | 8: 11.7 to 67.1 s | 8: 27.3 to 77.1 s |
| `HIT` and `BDA` naming the unit as target, within 0.2 s of ceasing to exist | 12 of 12 weapon kills | 12 of 12 |
| Any event at the moment the explosion's victim ceased to exist | none | none |

A weapon kill also sent `KILL` with the death events; the explosion's victim got
`UNIT_LOST` and `DEAD` only. The late events are not a property of the dedicated
server or of Wine.

What each read returns for the object ID, on both platforms:

| Read on `{ id_ = <object ID> }` | Killed, burning | Killed, death events sent | Removed by `Unit.destroy`, `Group.destroy` or `trigger.action.deactivateGroup` |
|---|---|---|---|
| `Object.isExist` | false | false | false |
| `Unit.getLife` | 1 | 0 | raises |
| `Unit.isDead` | true | true | raises |
| `Unit.isAlive`, `Unit.isEffective` | false | false | raises |
| `Unit.isBroken` | true | true | raises |
| `Object.getPoint`, `Unit.getDesc` | a table | a table | raises |
| `Object.getCategory` | 1 | 1 | 0 |
| `DCS.getUnitPositionByObjectId` | the wreck's position | the wreck's position | 0 |
| `Unit.getByName(name)` | nil | nil | nil |
| `Export.LoGetObjectById` | nil | nil | nil |

- On each platform 31 kills (24 in the fights, 7 by an explosion beside the unit) and 17
  removals (7 `Unit.destroy`, 7 `Group.destroy`, 3 `deactivateGroup`) read as the table
  says at the first sample after the unit ceased to exist. `classify.lua` read
  `isExist`, `isDead`, `getLife`, `getPoint`, `getCategory`, the position getter and
  `getByName`; only `coverage.lua` read `isAlive`, `isEffective`, `isBroken`, `getDesc`
  and `LoGetObjectById`, so those cells for the `deactivateGroup` rows are not read
  (ED-CODE: `deactivateGroup` is `group:destroy()`). The counts add both
  `coverage.lua` runs on each platform, including earlier versions (two on Windows, one on
  Wine) that lack the per-event existence read; the committed script's run alone gives
  fewer. `deactivateGroup` is
  `group:destroy()` (ED-CODE, `Scripts/ScriptingSystem.lua`).
- A life of 1 went with death events still to come and a life of 0 with events already
  sent, in every kill. Life turned from 1 to 0 at the death events.
- Read once a second for 325 s, a dead unit's reads stayed as the table says, and no
  ID was seen to exist again (`classify.lua`).
- The four flags never changed while a unit existed. They are not an early sign of
  damage.
- A removal sent no event and left no entry in the debriefing store.
- `Unit.destroy` on a burning wreck's object ID, 15 s after it ceased to exist: all
  reads raise from then on and no death event came in the 310 s that followed, 3 of 3 on each platform.
- From the hook state (`hook-reads.lua`, both), `DCS.getUnitPositionByObjectId`
  returns a position for a dead unit and 0 for a removed one, so a position sweep sees
  a removal and does not see a death. `Export.LoGetObjectById` is nil for both and a
  table for a living unit. `DCS.getUnitProperty` returns the mission's data for all
  three.

### A script can bring a burning unit back

`revive.lua`, both. Its delays are real seconds, and the Wine server paused during the
watch, so they cannot be set against the model times elsewhere. The Windows run used an
earlier version with the first three units only. Its log holds event names without unit
IDs, so which unit an event named is inferred from which unit's life turned to 0. Three M-113, each with an explosion beside it. Three seconds later
all three read `isExist` false, `isDead` true, `getLife` 1, and `Unit.getByName`
returned nil. Then `a_unit_set_life_percentage(id, 100)` on the first and `(id, 50)` on
the second, in the `mission` state. Two seconds later, and for the 120 s watched, the
first read `isExist` true, `isDead` false and `getLife` 3, and `Unit.getByName` found
it; the second the same with a life of 1.5. The object ID was the same. No event came
at the revival or after. The third, left alone, stayed dead, and `UNIT_LOST` and `DEAD`
came between 30 and 60 s after.

A fourth M-113 got an explosion of power 300 on it (Wine only). Three seconds later it
read `isExist` false, `isDead` true and `getLife` 0, and its `UNIT_LOST` and `DEAD` had
come. The setter with 100 brought it back in the same way: `isExist` true, `getLife` 3,
no event. So neither the dead read nor a `DEAD` event is final while a script may set
the unit's life.

## Other kinds of object

`classify.lua`, both, read once a second for 325 s; `others.lua` on Windows. "Dead
read" is `Unit.isDead` true and `Unit.getLife` a number; "raise" is both raising.

| Case | Windows | Wine | When it ceased to exist | Later | Death event |
|---|---|---|---|---|---|
| Static (fuel depot), explosion | 6 | 5 | Both `Unit` reads raise. `StaticObject.getLife` 0, `getPoint` a table, `getCategory` 3, `StaticObject.getByName` still finds it, the position getter 0 | unchanged | `UNIT_LOST`, `DEAD` at once |
| Static, tank fire | 1 of 5 died | 0 of 5 died | as above | unchanged | `HIT`, `KILL`, `UNIT_LOST`, `DEAD` at once |
| Static, `StaticObject.destroy` | 4 | 3 | `getLife`, `getPoint`, `getDesc` and `Unit.isDead` raise; `getByName` nil, `getCategory` 0, the position getter 0 | same | none |
| Cargo crate (`ammo_cargo`), explosion of power 3000 on it | 5 | 5 | not harmed | | |
| Cargo crate, tank fire | 3 of 3 died | 1 of 3 died | `isExist` false, `StaticObject.getLife` negative (-0.57 to -56.7), `getByName` still finds it, `getCategory` 6, the position getter returns its position | unchanged | none: no `HIT`, no `DEAD`, no event in 300 s |
| Cargo, `StaticObject.destroy` | 3 | 3 | every read raises | same | none |
| Aircraft (F-15C), explosion in flight | 4 | 3 | dead read, life 1; the position getter follows the falling aircraft | raise from the crash on, 17 to 21 s later | `AI_ABORT_MISSION` and `ENGINE_SHUTDOWN` at once (not death events), `UNIT_LOST` within 2 s, `PILOT_DEAD` or `EJECTION`, `CRASH` at the ground; no `DEAD` |
| Aircraft, out of fuel, flies into the ground | 2 | 2 | raise at once | raise | `CRASH` in the same second |
| Aircraft, `Unit.destroy` | 3 | 3 | raise | raise | none |
| Ship (speedboat), explosion | 5 | 5 | Windows: dead read, life 0. Wine: raise already at the first sample | raise within 1 s | `UNIT_LOST`, `DEAD` at once |
| Ship, `Unit.destroy` | 3 | 3 | raise | raise | none |

- For a static the two `Unit` reads raise whether it was killed or removed. Its own
  `StaticObject.getLife` separates the two: a number for a kill, a raise for a removal.
- A crate killed by gunfire sent no event at all.
- An aircraft that crashes with no kill reads as removed at once. A ship reads as
  removed within 1 s, and at once under Wine. Their death events came at once.
- The Windows `others.lua` fighter died from the power-5 explosion at 15 s, so its
  planned kill step had nothing left to act on.
- Not obtained: an aircraft shot down by air defense (the battery did not fire), an AI
  aircraft that lands and despawns (not despawned at 325 s), cargo delivered or dropped
  from a sling.

## Rearm

`rearm.lua` and `trucks.lua`. Eight M-1 Abrams 1.5 km apart, each ordered to fire eight
rounds at a point on the ground and then left standing, each with a different neighbor.
ED's unit database, read in the client's `gui` state, lists 72 vehicles with the
attribute `Trucks` (and five `vap_ammo_*` props without it) and no vehicle field about supply or rearming. The type `M 818` has
the display name "Truck M939 Heavy".

| Neighbor | Windows | Wine |
|---|---|---|
| None within 1.4 km | No rise in 400 s | No rise in 400 s |
| M 818 at 25 m | First rise 48 s after the last shot: 29 at once, then 1 and 1 within 21 s | First rise 60 s after the last shot: 1, then 30 at once, then 1, within 18 s |
| M 818 at 100 m | First rise 66 s after the last shot: 30 at once, then 1 and 1 within 17 s | First rise 47 s after the last shot: 29 at once, then 1 after 17 s |
| M 818 at 400 m | No rise in 278 s after its last shot | No rise in 277 s after its last shot |
| M 818 at 200 m, M978 HEMTT Tanker at 25 m, Ural-375 at 25 m | Not obtained: these tanks did not fire | Not obtained: the same three tanks did not fire |
| M 818 of the other coalition at 25 m | Not obtained: another tank killed the truck | Not obtained: another tank killed the truck |

- A rearmed tank ended 3 rounds above its start on both platforms. On Windows each of
  three rows was one above (`M256_120_HE` 18 to 19, `M2_12_7` 404 to 405, `M2_12_7_T`
  104 to 105).
- In `coverage.lua` the tank, with an M 818 at 25 m, rearmed in all five runs (three are earlier
  versions of the script): 49 to 65 rounds in one step and single rounds 15
  to 19 s apart. The first rise came 18, 34, 47, 61 and 70 s after its last shot.
- The truck's life, fuel and `getAmmo` (no rows) did not change, and no event named
  it. No `S_EVENT_WEAPON_REARM`, `S_EVENT_WEAPON_ADD` or other event named the tank.
- UNVERIFIED: a moving unit, a unit under fire, other supply types.

## Recommendation

**Life.** Keep `Unit.getLife`, read after `S_EVENT_HIT` names the unit and on the slow
sweep. The sweep stays, because a scripted explosion and the scripted life setter
change life with no event. At 2,000 units a 60 s sweep is 33 reads a second: about
25 µs a second under Wine at 0.74 µs a read (computed from the cold figures above).

**Death of a ground unit.** `Unit.getLife` alone does not show a death: 1 is also a
living unit's life. The check that finds it is `Object.isExist` (0.79 µs under Wine),
which is already false when the fatal `HIT` and `BDA` arrive. For a ground unit found
not to exist, `Unit.isDead` and `Unit.getLife` on its object ID, each under
`lua_pcall`, classify it at once: true and a number mean killed; two raises mean
removed by script. The classification holds for what was tested: weapon kills,
explosions, `Unit.destroy`, `Group.destroy` and `deactivateGroup`, read up to 325 s
after. Two limits: a script that sets a dead unit's life brings it back, before or
after its death events, and a script that destroys a burning wreck cancels its death
events. A kill with no event is
found by the slow sweep or by a pass of `Object.isExist`, 1.6 ms for 2,000 units under
Wine (computed from the cold figures above).

**Other kinds.** The same two reads do not classify a static, a crate, an aircraft or
a ship. A static and a crate need `StaticObject.getLife`. An aircraft and a ship are
announced by their own events at once.

**Ammunition.** Keep `Unit.getAmmo`, read after a shot event names the unit. A rearm
sends no event, so a rise is seen only by a later read. A sweep of 2,000 armed units
costs 21 to 51 ms a pass under Wine (computed from the cold figures above).

## Not verified

- Splash from an aircraft's bomb. Howitzer shells are the real weapon tested.
- A collision, an aircraft's turnaround, repair by DCS itself, a jettison, and the use
  of flares and chaff.
- Removal by DCS itself: an aircraft that lands and despawns, a wreck cleared later
  than 325 s.
- The life setter on a unit whose death events had already come, on Windows.
- Whether a life of 1 marks a burning unit for every type. Seen for the M-113, the
  Ural-375, the BTR-80 and the F-15C.
- Static types other than the fuel depot, ship types other than the speedboat.
- Whether `Object.isExist` is false in the same frame as the fatal hit for kinds other
  than ground units.
