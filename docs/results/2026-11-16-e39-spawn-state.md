# E39: spawning with a captured unit state

| | |
|---|---|
| Step | E39, for S19's restore and S20's materialize: what a `coalition.addGroup` spawn sets per unit, and what can be set right after it, for life, fuel, per-pylon weapons, gun rounds and countermeasures, on aircraft (air start), a helicopter, ground units and ships |
| Date | 2026-11-16 |
| DCS | 2.9.31.29434, branch `dcs_server.release`, dedicated server |
| Platform | Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker. No player connected |
| Scripts | `lua/experiments/e39/`: `recon.lua` (`server`), `spawn.lua` (`hook`), `guns.lua` (`server`), each through `cargo xtask eval <target> <state> <file>` on a `--dev` build |
| Rehearsal | `lua/tests/e39.lua`, under the mock |
| Mission | A shell from `templates/caucasus.miz` (`cargo xtask miz`, `cargo xtask load`) |
| Raw results | `2026-11-16-e39-spawn-state.json`, trimmed from the three replies |
| Result | Life, aircraft fuel, gun rounds and per-pylon missiles restore. Ground and ship ammunition, flares and chaff, ground and ship fuel do not |

Every figure is LIVE on this build unless tagged.

## The rule

Written in `spawn.lua` before the run. A mechanism restores a value when the read after
the spawn, 1 s and 5 s of model time later, equals the target:

- life: `getLife / getLife0` within 0.5 percentage points of the percent asked, the unit
  existing and not dead;
- fuel: `getFuel` within 0.005 of the fraction asked;
- gun: the gun row within 1 round of the full count times the percent;
- pylons: every `getAmmo` row equal to the summed counts of the pylons whose launcher's
  weapon has that row's `displayName`;
- ground ammunition: every row exact from a table built only from data DCS's Lua can
  read, else it needs a per-type map.

The experiment fails its rule only when it cannot record a verdict, or DCS stops.

## Verdicts

| Value | Aircraft and helicopter | Ground unit | Ship |
|---|---|---|---|
| Life | Restores, after the spawn | Restores, after the spawn | Restores, after the spawn |
| Fuel | Restores at spawn (`payload.fuel`); a drop tank spawns full | No fuel model: `getFuel` reads 1 | No fuel model: `getFuel` reads 1 |
| Gun rounds | Restores at spawn (`payload.gun`) | Cannot (see below) | Not tested; the unit table has no ammunition key a ship uses (ED-CODE) |
| Missiles by pylon | Restores at spawn (`payload.pylons`), a whole pylon at a time | n/a | n/a |
| Other ammunition | Cannot | Cannot | Cannot |
| Flares, chaff | Set at spawn, never read (E8): cannot be captured | n/a | n/a |

No post-spawn setter exists for fuel or ammunition in the `server`, `sse`, `mission` or
`hook` state: the only setter of a unit's condition is `a_unit_set_life_percentage`, in
`mission` (LIVE: the API dumps in `lua/mock/api/`, same build). The Mission Editor's unit
table has no life key; its payload keys are `fuel`, `gun`, `flare`, `chaff`, `ammo_type`
and `pylons` (ED-CODE: `MissionEditor/modules/me_payload.lua`).

## Life

`a_unit_set_life_percentage(<mission ID>, <percent>)` set every subject exactly, read at
once, 1 s and 5 s later, at three moments: in the frame of the `addGroup` with the
simulation paused, a second later still paused, and with it running.

| Subject | Set | When | Read |
|---|---|---|---|
| F-15C, air start | 60 | addGroup's frame, paused | 12 of 20 |
| F-15C, air start | 60 | paused, 1 s later | 12 of 20 |
| F-15C, air start | 30 | running | 6 of 20 |
| UH-1H | 50 | addGroup's frame, paused | 6 of 12 |
| T-72B | 40 | addGroup's frame, paused | 10 of 25 |
| M-113 | 40 | paused, 1 s later | 1.2 of 3 |
| M-1 Abrams | 70 | running | 22.4 of 32 |
| PERRY | 80 | addGroup's frame, paused | 1,680 of 2,100 |
| PERRY | 50 | running | 1,050 of 2,100 |
| Speedboat | 80 | paused, 1 s later | 1.28 of 1.6 |

Every subject still existed 5 s later. E16's speedboat removed at 50 is not contradicted:
this run set the speedboat to 80 only.

## Aircraft fuel

`payload.fuel` is kilograms; `getFuel` is the fraction of `getDesc().fuelMassMax`
(F-15C 6,103 kg, UH-1H 631 kg). Asked 0.37, 0.8 and 0.2 of an F-15C and 0.5 of a UH-1H,
`getFuel` read them to 4 decimals. A UH-1H given 1,000 kg read 0.9997: DCS caps at the
internal maximum. An F-15C with a 610-gallon tank on pylon 6 and 0.5 internal read
0.8057: the tank spawns full and `getFuel` counts it, 1,865.5 kg, the tank launcher's
`Weight` less `Weight_Empty` in `db.Weapons.ByCLSID` (1,865.9). A tank cannot be part
filled, so a captured state below the tanks' fuel cannot be restored.

## Gun rounds

The F-15C's M61 row read 940, 470 and 235 at `payload.gun` 100, 50 and 25; at 0 the row
is absent. The full count is in the `server` state as
`db.Units.Planes.Plane[i].Guns[k].supply.count`: F-15C 940, A-10C 1,150, Su-25T 250,
MiG-29A 150. The Ka-50 has `count1` 240 and `count2` 220, two belts, whose split by
percent is UNVERIFIED. The UH-1H has no `Guns`. A gun row's `typeName` starts with
`weapons.shells.`.

## Missiles by pylon

`get_weapon_and_count_from_launcher(CLSID)` returns the launcher's weapon as a wsType
array and a count (AIM-120B `{4,4,7,24}`, 1), and `get_weapon_display_name_by_wstype`
names it. Both and `db.Weapons.ByCLSID` exist in the `server` state. An F-15C with
AIM-120B on pylons 1 and 11, AIM-9M on 3 and 9, AIM-7M on 4, 5, 7 and 8 read AIM-120B 2,
AIM-9M 2, AIM-7M 4; with pylons 4, 5, 9 and 11 left empty it read 1, 1 and 2. Each
`getAmmo` row's `displayName` equals the launcher's weapon name. The row's `typeName` is
not uniform (`weapons.missiles.AIM_120`, `AIM_9`, `weapons.missiles.AIM_7`); its last
part names a global whose value is the weapon's fourth wsType number (`AIM_120` 24,
`AIM_9` 22, `AIM_7` 21, from the API dump). A tank or smoke pylon has no row.

A pylon carries all of its launcher's count or nothing, so a partly fired rack or pod
cannot be restored exactly.

## Ground and ship ammunition

The `server` state's `db.Units.Cars.Car[i].WS[...].LN[...].PL[...]` entries are not the
Mission Editor's: of the T-72B's 20 PL entries only two carry `ammo_capacity` (7 and 10,
`2A46M_125_HE`), and none carries `virtualStwID`. An `ammo` table setting those two slots
to 3 and 4 read 8 HE rounds; setting them to 0 removed the HE row. With the default the
row reads 18. The other rows were unchanged. The offset is not constant, and the layout of
the other rows is not readable, so ground ammunition needs a per-type map measured live
(E14) and no general restore is built. Entries with `ammo_capacity`, of all PL entries:
M-113 0 of 5, M-1 Abrams 5 of 10, BMP-2 1 of 4, M-2 Bradley 2 of 8, ZSU-23-4 1 of 1,
Leopard-2 4 of 24, PERRY 2 of 39, speedboat 0 of 10.

## Not verified

- Bombs, rockets and gun pods by pylon: only air-to-air missiles were spawned.
- A Ka-50's two belts under `payload.gun`.
- The life setter on a unit that is part way through a mission load.
- Windows: no Windows server was run (the Windows PC is in use).
