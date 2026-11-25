# Spawned FARPs: tracked, removed, with their radio

A FARP static the platform spawns, by `PutStatic` or `PutFromStaticTemplate`, is `LIVE`
in the World, and `RemoveEntity` removes it with its airbase
([ADR 0045](../adr/0045-spawned-farps-are-tracked-statics.md)). Its radio frequency,
modulation and callsign pass through `StaticSpec.extra` and from a template. Before
this, such a FARP reached `LOST` and stayed in DCS after `RemoveEntity`
([the template-heliports results](2026-11-24-template-heliports.md)).

## Run 1: how DCS exposes a FARP a script adds

Scripts: `lua/experiments/farp-fixes/` `spawn.lua`, `destroy.lua`, `after.lua`,
`running.lua`, `pause-read.lua`, `left.lua`, `frequency.lua`, `reload.lua` and
`count.lua`, each run with `cargo xtask eval <server> <state> FILE`, in that order.
`spawn.lua`, `destroy.lua` and `after.lua` were rehearsed on the mock before the run;
`lua/tests/farp_fixes_experiments.lua` rehearses the server-state ones now.
DCS 2.9.31.29434 dedicated server (`dcs_server.release`) on Linux under Wine 10.0
(`wine-10.0 (Debian 10.0~repack-6)`), 2026-11-24, in a mission paused at model time 0
with no player connected. `spawn.lua` added, 2 km and more north of the first airdrome,
a `FARP` with its radio as numbers, a `SINGLE_HELIPAD` with its frequency as text, a
`SINGLE_HELIPAD` with no radio keys, and a `FARP` with the mission file's warehouse
flags in its unit table. All LIVE.

| Read | Result |
|---|---|
| What `coalition.addStaticObject` returns | An object of class `Airbase`, with the new ID |
| `StaticObject.getByName` | The same object, class `Airbase` |
| `Object.getCategory` | 4, base |
| `getDesc().category` | 1 for the `FARP` and the `SINGLE_HELIPAD` |
| `getLife`, `StaticObject.getLife` by ID | 3600 |
| `isExist`, `getCoalition`, `getCountry`, `getTypeName`, `getPoint` | Read as for a static: true, 2, 2, its type, its place |
| `coalition.getStaticObjects`, every side | Does not list it |
| Birth event | `S_EVENT_BIRTH` (15), initiator category 4, one per static |
| `Airbase.getByName` | The airbase, with the static's ID; `getCategory` 4 |
| `world.getAirbases` | 30 before, 34 after the four spawns |

The shim queues a birth only for initiator categories 1, 3 and 6, and the enumeration
reads statics only from `coalition.getStaticObjects`. So the agent wrote no `OBJECT`
for the FARP, the first enumeration's end moved the entity to `LOST`, and
`RemoveEntity` sent no `STATIC_DESTROY`: the sidecar sends a destroy for an object it no
longer stores only after an `OBJECT` of its name. This is the cause (LIVE, with the code
of `lua/agent/shim.lua` and `crates/sidecar/src/reconciler.rs` before this change).

### Removal

| Step | Result |
|---|---|
| `StaticObject:destroy()` on the `FARP` and the radio helipad, `Object.destroy` on the bare helipad's airbase, mission paused | Each call returned. In the same call and in a later one, every static and airbase was still found, `isExist` true |
| `running.lua` resumed the mission; `pause-read.lua` paused it at model time 7.7 and read; `left.lua` read again | Every airbase was gone from `Airbase.getByName`; `StaticObject.getByName` still returned a handle, `isExist` false, `getLife` raising `Airbase doesn't exist`; `world.getAirbases` still listed each, `isExist` false, 34 in all |
| `reload.lua` loaded the same mission again; `left.lua` and `count.lua` read | 30 airbases; none of the four found |

So `destroy()` removes a FARP and its airbase only when the simulation runs. The
airbase handle and the static are one object, so `Object.destroy` on either does the
same. `running.lua` raised at its control's spawn, which reads the FARP's place: DCS
had taken the earlier destroys as soon as the mission ran, so the control it planned
was not made.

### Radio

| Keys given | `getFrequency` (Hz) | `getModulation` | `getCallsign` |
|---|---|---|---|
| none | 127,500,000 | 0 | London |
| frequency `129.5`, modulation 0, callsign 3 | 129,500,000 | 0 | Paris |
| frequency `"127.5"`, modulation 1, callsign 2 | 127,500,000 | 1 | Dallas |
| frequency `"131.25"`, modulation 1, callsign 4 | 131,250,000 | 1 | Moscow |
| frequency `131.25`, modulation 0, callsign 4 | 131,250,000 | 0 | Moscow |

The frequency is read from the airbase's communicator (`getCommunicator()`). DCS reads
the frequency as a number or as text, the modulation and the callsign ID.

## Warehouses

The owner asked two questions: can a FARP be spawned without a warehouse, or with an
unlimited one; and can two FARPs share one.

- **No warehouse, or unlimited, from a script: no.** A FARP a script adds always has its
  own warehouse, limited and empty: jet fuel 0, `weapons.bombs.Mk_82` 0, `UH-1H` 0
  (LIVE, run 1). `unlimitedFuel`, `unlimitedMunitions` and `unlimitedAircrafts` in its
  unit table change nothing (LIVE). The editor keeps those flags in the mission's
  `warehouses` table under the unit ID, unlimited by default, and writes none of them on
  the static's unit, which carries only the three `heliport_` keys (ED-CODE,
  `MissionEditor/modules/me_mission.lua:9534-9552`, `4465-4467`). The scripting
  `Warehouse` class has no call that makes a warehouse unlimited. Only a FARP in the
  mission file can be unlimited.
- **Shared stock between two script FARPs: no.** 4,321 kg of jet fuel set at one FARP
  left the other at 0 (LIVE). The editor's only link between warehouses is a supplier
  entry, `{Id, type}` with type `airports` or `warehouses`, in the receiving
  warehouse's `suppliers` list, with `speed`, `periodicity` and `size` on it (ED-CODE,
  `me_mission.lua:9906-9921`, `9540-9542`): resupply over time, not shared stock. No
  scripting call makes one. Whether DCS runs the resupply is UNVERIFIED.
- **FARPs that look shared in a mission file.** In a mission an owner saved, 24 of its
  25 `FARP` statics are in 12 static groups of two units. In 7 of those groups only the
  first unit has a `warehouses` entry; every `suppliers` list in the file is empty and
  every FARP warehouse is limited. The editor creates a heliport's warehouse for the
  group's first unit, `addWarehouse(group.units[1].unitId, ...)` (ED-CODE,
  `me_mission.lua:6201-6202`). So the pairs share because they are one static group
  whose warehouse is keyed by its first unit. What airbase DCS makes for the second
  unit is UNVERIFIED. A script cannot build such a group: `coalition.addStaticObject`
  adds one unit. The file is not in the repository.

## The change

- **Agent.** A `STATIC_SPAWN` of a FARP queues the new object as a static birth, so the
  next pump writes its `OBJECT`: kind `STATIC`, category 1. The enumeration's last step
  visits each airbase entry a `STATIC_SPAWN` made in the generation that is not a
  tombstone, through the entry's object, one item each, skipping an entry whose
  `isExist` is not true; a pending-destroy ID is ignored as in every step. With no
  spawned FARP it calls nothing. The event filter stays at categories 1, 3 and 6. The
  airbase entry turns into a tombstone when `isExist` reads false.
- **Sidecar.** No `AIRBASE_OWNER` or `WAREHOUSE_SET` to the airbase of a FARP the store
  no longer holds, which samples until DCS takes the destroy.
- **API.** `PutFromStaticTemplate` passes `heliport_frequency`, `heliport_modulation`
  and `heliport_callsign_id` for a `Heliports` static. `PutStatic` already passed every
  key of `extra`.
- **Mock.** A FARP is absent from `getStaticObjects`, has category 4 and a birth of
  category 4, reads `getDesc().category` 1 and life 3600, and its destroy waits while
  paused; its airbase then stays listed.

## Run 2: through `PutStatic` and a template

`cargo xtask sidecar <name> --part template-heliport --file FILE`
(`crates/xtask/src/shell_live/template_heliport.rs`) on the same server, 2026-11-25,
from this commit's tree before review: agent build `fbf3b07b270b7f3c`, sidecar binary
sha256 `1cac722afd7225c3eae10a274d8b2793c62d2c00c8879a68bbe29a421ac222f4`, in a fresh
shell mission on Caucasus that the part keeps running. FILE was a static template
written for the run in the form the editor saves (the form of
[the template-heliports results](2026-11-24-template-heliports.md)): an `APFC fuel`
(`Fortifications`), a `FARP` with frequency `"129.5"`, modulation 0 and callsign 2, and
a `SINGLE_HELIPAD` with frequency `"127.5"`, modulation 0 and callsign 1. The file is not
in the repository.

The run made FILE in place, then the `FARP` again with `PutStatic`, 60 m north, with
frequency `"131.25"`, modulation 1 and callsign 4: the twin. After the checks it added
four more helipads like the twin with `PutStatic`, then removed every entity. The part
reads the agent's last sampling report from its log before the spawns and after the
four.

| Check | Result |
|---|---|
| Each static stands within 1 m as its type and country; each heliport's airbase stands there, category 1, listed by `world.getAirbases` | Yes |
| State in the World, the template's three statics and the twin | `LIVE` (3), all four |
| Radio read from DCS, template `FARP` | 129.5 MHz, AM, Dallas |
| Radio read from DCS, template `SINGLE_HELIPAD` | 127.5 MHz, AM, London |
| Radio read from DCS, `PutStatic` twin | 131.25 MHz, FM, Moscow |
| 5,000 kg of jet fuel set through the sidecar at each of the three heliports | Read in DCS, all three |
| After `RemoveEntity`, the mission running | Every static and every airbase gone: neither `getByName` finds a live object, and `world.getAirbases` lists none that exists. All 8 entities |
| The agent's airbase entry of each of the 7 heliports | A tombstone |
| The template's `FARP` and the twin | The same state, airbase category and type, and removal |
| `land.getSurfaceType` at the four places | 1, land |

Verdict: PASS (LIVE). The rule: as the template-heliports part's, and further every
static `LIVE`, each heliport's frequency, modulation and callsign as its keys give or
DCS's defaults, every heliport and its airbase gone after the removal, its entry a
tombstone, and the four further helipads made. The tolerances are this step's.

### Sim-thread time

The agent's sampling report (10 s windows) in the same mission and process, once with
no FARP and once with the 7 heliports standing, the window after the four further
spawns. Both are reports of the changed agent; the comparison is within the run.

| Report | No FARP | 7 FARPs |
|---|---|---|
| Mean share | 0.077 % | 0.089 % |
| Frames over 250 µs | 1 of 1,117 | 0 of 1,113 |
| Largest frame | 297.8 µs | 197.8 µs |
| Enumeration: time, rows | 79.4 µs, 18 | 150.3 µs, 12 |
| Enumeration step: largest in the window; p99 bucket's upper edge since the agent started | 11 µs; 16 µs | 17.8 µs; 32 µs |
| Births: time, rows | 0 µs, 0 | 156.3 µs, 4 |
| Warehouses: time, rows | 170.1 µs, 3 | 219.3 µs, 4 |
| Airbase entries | 21 | 28 |

The p99 figures are the upper edges of histogram buckets over the agent's life and the
largest step is the window's (spec/sampling.md, "Reporting"), so the p99 can exceed it.
A window holds as much of an enumeration as fell in it: 18 rows in one, 12 in the
other, so the rows are not a full count of the mission. The window with FARPs holds the
four further births and their warehouse reads, which account for most of the rise in
share. Both windows are under the owner's target of 0.3 % mean share. They are not a
measure at the load of the stored baseline (0.18 to 0.22 % with 2,000 objects,
[S11](2026-11-14-s11-budget-and-baseline.md)), and no run of the agent from before this
change was made in this mission.

What the change adds per enumeration: one `isExist` and one visit for each spawned FARP
that is not a tombstone, and a Lua pass over the airbase entries with no DCS call for
an entry that is not a spawned FARP. With no spawned FARP it makes no DCS call (the
`farps` host test). The pass grows with the mission's airbase count. Here, with 21 and
28 entries, the enumeration took 79.4 µs and 150.3 µs in the two windows, which hold
different parts of an enumeration (18 and 12 rows); its largest step was 11 µs and
17.8 µs.

## Run 3: the final tree, with an owner

The same part and FILE on the same server, 2026-11-25, from this commit's tree after
review: agent build `79dc5148799a9930`, sidecar binary sha256
`45a9cf465f51f111ba7b8cf24e0cef08cb13668c0fbd5826e331f2fc326bf15a`. The part now also sets
each heliport's owner to red with `SetAirbaseOwner` beside the fuel, and requires DCS to
read both. The review changed how the agent knows a spawned FARP (by the object
`addStaticObject` returns), how it judges one gone, and how owner and stock commands
find one (by that object), after run 2.

| Check | Result |
|---|---|
| Statics and airbases stand, each heliport's airbase listed, category 1 | Yes |
| State in the World, the four statics | `LIVE`, all four |
| Radio read from DCS, template `FARP`, template `SINGLE_HELIPAD`, twin | 129.5 MHz AM Dallas; 127.5 MHz AM London; 131.25 MHz FM Moscow |
| Owner red and 5,000 kg of jet fuel set through the sidecar at the three heliports | Read in DCS at all three: coalition 1, 5,000 kg |
| After `RemoveEntity`, the mission running | All 8 entities and every airbase gone |
| The agent's airbase entry of each of the 7 heliports | A tombstone |

Verdict: PASS (LIVE), by run 2's rule with the owner added. Its sampling report with
the FARPs standing fell in a window with no enumeration in it (0 rows), so the
sim-thread figures above are run 2's: mean share 0.057 % with no FARP and 0.069 % with
7 in this run.

Runs used: 3 of 3.

## Open

- A FARP the mission file places is not read as a static. Whether
  `coalition.getStaticObjects` lists one is UNVERIFIED.
- A FARP killed in play, and its ruin spawned again with `dead = true` (ADR 0026), are
  UNVERIFIED. If DCS reports a killed FARP with `isExist` false, `STATIC_DESTROY`
  applies with nothing destroyed, as for a removed one; that step must revisit it.
- On a paused server a removed FARP's airbase stays until the mission resumes (LIVE).
  Whether a player is offered a slot there meanwhile is UNVERIFIED.
- Callsign IDs other than 1 to 4 are UNVERIFIED.
- What `StaticObject.getByName` and `Airbase.getByName` return for a name while an
  older FARP of it waits for its destroy and a newer one stands is UNVERIFIED. The
  agent does not depend on it: it keeps the object each spawn returned.
- The World serves no airbases (`crates/api/src/convert.rs`).

## Runs

macOS (aarch64), the working tree of this commit, 2026-11-25. The Lua scripts run as
`tools/luatest.sh` runs them: `lua lua/tests/<script>.lua <module> <version> <link-probe>
<nanoprobe> <xtask> <scenario>`, with the module, programs and version that
`tools/luatest.sh` builds and reads; `mise run lua` runs all of them.

| Command | Result |
|---|---|
| `lua/tests/dispatch.lua`, scenario `farps` | Pass, eight times in a row: with no FARP the FARP step reads nothing of any airbase entry; a spawned FARP gets a row and an `OBJECT` (kind 2, category 1, `life0` 3600); an enumeration writes its `OBJECT` with the enumeration's number and nothing gone; a same-name spawn writes the old ID `DESTROYED` and the new one takes the entry; a destroy while paused writes one `OBJECT_GONE DESTROYED`, keeps the airbase sampled, and an enumeration meanwhile reads nothing of the ID; a resume makes the tombstone, and no second `OBJECT_GONE`; a second destroy finds only the removed FARP's handle and is `APPLIED` with nothing gone; a spawn after the tombstone finds the removed FARP by name (`isExist` false), names its ID destroyed again, and nothing is written gone twice; a destroy and a same-name spawn while paused leave the new FARP its row and entry after the resume, the old ID is written gone once, and an owner and a stock sent meanwhile reach the new FARP although the mock's old one still answers lookups of the name; a FARP spawned while an enumeration runs, before the FARP step and (a same-name spawn, with 150 further FARPs behind it) behind its cursor, keeps its row and is written gone by nothing, the enumeration's place recorded when DCS got each spawn; a FARP whose birth read adds no row is added by the next enumeration (kind 2, category 1, `life0` 3600); a new generation drops the spawned entries. Against the shim before this change it fails at the spawn |
| `lua/tests/dispatch.lua`, scenario `world` | Pass; the FARP destroy resumes the mission before it checks the tombstone |
| `lua/tests/farp_fixes_experiments.lua` | Pass: `spawn.lua`, `destroy.lua`, `after.lua`, `frequency.lua`, `left.lua` and `count.lua` on the mock, whose removed FARP is still found by name with `isExist` false and stays listed: the count ends six above where it began. The hook-state snippets that resume, pause or load the mission (`running.lua`, `pause-read.lua`, `reload.lua`) are not rehearsed |
| `cargo test -p nanoprobe-sim-fake --test world_commands` | 25 passed, among them: a FARP the store holds gets its owner and stock, and once removed, while the fake still samples its airbase, gets no more, nor does the airbase of a static the store holds nothing of, as after a purge (fails without the sidecar change); a FARP's radio keys reach its `STATIC_SPAWN` unchanged |
| `cargo test -p nanoprobe-api --lib -- static_template` | 24 passed: a helipad keeps its radio keys, the frequency as text; a `heliport_` key on another static is dropped; the template's helipad equals the `PutStatic` FARP with the same radio |
| `cargo test -p xtask template_heliport` | 6 passed, the radio check among them |
| `mise run check` | Pass |
| `mise run dll` | Pass |
