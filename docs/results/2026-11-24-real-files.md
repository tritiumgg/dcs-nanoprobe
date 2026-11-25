# Static templates and payload presets: real files through the readers

The sidecar's static-template and payload-preset readers were built from Eagle
Dynamics' code and checked live with files the test runs wrote. Here every file DCS
ships of either kind goes through them, and so do four files the Mission Editor saved:
three static templates and one payload preset.

## Files the Mission Editor saved

On 2026-11-24 a person saved three static templates and one payload preset from the
Mission Editor of a DCS 2.9.31.29434 client on Windows. The files were read where the
editor wrote them, in the Saved Games profile, with
`cargo xtask real-files <target> profile <path>...`, and made on DCS with
`cargo xtask sidecar <target> --part saved-files --stm <path> --preset <path>
--preset-name <name>` (`crates/xtask/src/shell_live/saved_files.rs`). That run lists
each file's folder in the sidecar's `template_dirs` and names the file in place; it
copies and writes none.

| File | What it holds | Reader |
|---|---|---|
| `StaticTemplate/test0.stm` | Two statics, no group: `APFC fuel` (`Fortifications`) and `SINGLE_HELIPAD` (`Heliports`) | Refused whole, as built: "static group [2] unit [1] is a heliport or a grass airfield". The first static was read with no refusal |
| `StaticTemplate/test1.stm` | Two vehicle groups of one `AAV7` each, country 2, `theatre` Caucasus | Loaded: 2 groups, 2 units, no statics |
| `StaticTemplate/test2.stm` | One static, `HESCO_wallperimeter_1` (`Fortifications`, with `shape_name` and `rate`), country 2, `theatre` Caucasus | Loaded: no group, 1 static |
| `MissionEditor/UnitPayloads/UH-1H.lua` | Two presets of `UH-1H`; "Test Payload 0" is the new one: `M261_MK151` on pylons 2 and 5, `M134_SIDE_L` on 3, `M134_SIDE_R` on 4, no `settings` | Loaded; both presets give pylons |

The editor writes both kinds as Eagle Dynamics' code says (LIVE). The template has the
`coalition`, country and kind lists and the `theatre` of the shipped files. The preset file is
`local unitPayloads = {...}` then `return unitPayloads`, with `unitType`, and holds only
the presets that differ from the shipped ones. The editor's template also writes
`coldAtStart` on each vehicle unit, which the reader drops and logs.

The live run made the template at an anchor 300 m north of where the static-template
check stood, with no turn and under country 2, so a file saved on any map could be made
on the shell mission. The preset went to two UH-1H in the air, 500 m up at 50 m/s: one
from the preset, one with the reader's pylons written in `extra.payload`.

| Run | Result |
|---|---|
| 1 | FAIL, harness: both aircraft read the same ammunition and their stored rows were equal, but the export gives a row's `extra` as a plain string when its bytes are UTF-8, which the harness did not decode. Fixed and tested on the host. The template passed |
| 2 | PASS, with `test1.stm` |
| 3 | PASS, with `test2.stm` and the same preset |

Run 2, LIVE (Windows, DCS 2.9.31.29434 client, 2026-11-24, agent build
`fa5981a651fa40e5`):

- Template: the answer held 2 groups of 1 unit, as the reader found. Both AAV7 stood
  within 2 cm of the places `static_template::specs` gives (-289,452.62 N 680,035.36 E
  and -289,397.38 N 680,034.64 E), heading 0, type `AAV7`, country 2. Both were removed.
- Preset: both aircraft read `weapons.nurs.HYDRA_70_M151` 38 and
  `weapons.shells.M134_7_62_T` 6,400. The preset aircraft's stored pylons equal the
  reader's, and the two stored rows are equal. Fuel read 0.449 of internal on both.
- The profile's file list was the same before and after; DCS closed and the agent was
  undeployed.

Run 3, LIVE (the same client, 2026-11-24, agent build `df3220c98e943d1a`): the answer
held 1 static, and it stood at the anchor itself, -289,425 N 680,035 E, heading 0, type
`HESCO_wallperimeter_1`, country 2. The reader passed `category`, `shape_name` and
`rate` and dropped the static group's `groupId`, `heading`, `route`, `x`, `y` and the
unit's `tasks`. The preset passed as in run 2.

The verdict judges the template by place, type and country, within 1 m, as the
static-template check does. It judges the preset by these: both aircraft present and of
the file's type; both read the same ammunition; when the reader gives the preset a
pylon, the preset aircraft has at least one weapon that is not gun rounds
(`weapons.shells.`), so a preset of only tanks or pods would fail it; the stored pylons
equal the reader's; and both stored rows are equal. Fuel and headings are recorded and
not judged. Runs 2 and 3 were judged by a weapon count that also counted gun rounds;
the gun-round exclusion came after them, and their recorded ammunition,
`weapons.nurs.HYDRA_70_M151` 38, passes it.

## Shipped files

Read on 2026-11-24 in the install of a DCS dedicated server on Linux under Wine 10.0
(`wine-10.0 (Debian 10.0~repack-6)`), DCS 2.9.31.29434 (`dcs_server.release`), with
`cargo xtask real-files <target> shipped`. One SSH session runs `find` and `cat` in the
container and streams every file into the tool, which runs the sidecar's readers on the
bytes in memory. Nothing was written on the server and no file was kept. Each file is
ED-CODE.

The tool applies to each file what the API calls apply:

- A static template (`.stm`): `static_template::read`, then `static_template::specs`
  in place with no anchor and no country override, as `PutFromStaticTemplate` converts
  a file without an anchor. The counts are the converted specs'.
- A preset file (`.lua`): `payload_preset::read`, then `File::pylons` for every
  distinct preset `name` in the file against the file's own type, as `PutGroup`
  matches a `payload_preset`. Entries counts every entry of `payloads`, Names the
  distinct names, Matched the names that give pylons.

Result: 176 files. Of 23 static templates, 20 load (37 groups, 346 units, no statics)
and 3 are refused. Of 153 preset files, 143 load and 10 are refused whole; in 2 of the
143 one preset is refused. Of the 2,069 preset entries in the 143 files that load, 2,065
give pylons. "Loaded" in the
table's last line counts a file the reader took, whatever its presets gave.

The first run refused 5 more files, which the readers take after the coordinator's
ruling (D266): a bare name as a value in a preset's `tasks` (`[1] = CAS`, in
`A6E.lua`, `F-4E-45MC.lua` and `Mirage-F1C.lua`), and a pylon entry with
`["CLSID"] = ""` (in `Tornado GR4.lua` and `MosquitoFBMkVI.lua`), which now leaves its
station empty: the table's last column counts those pylons, 4 and 2. spec/world.md, "Payload presets", has both rules.

### Refusals by cause

| Cause | Files | Why |
|---|---|---|
| A static template with no object: its countries hold only `id` and `name` | `testx4.stm`, `testx5.stm`, `testx6.stm` | A file with nothing to make is refused |
| A preset file of Lua code: locals other than the one table, `function`, `require`, `..` | 9: the five F-14 files, `JF-17.lua`, `AH-64D_BLK_II.lua`, `MiG-29 Fulcrum.lua`, `F-100D.lua` | A code file is refused whole (the payload-presets ruling) |
| Two entries under one key in `payloads` (`[4]`) | `SA342L.lua` | Two entries under one key refuse the file, as in a static template |
| Two presets with one `name` (that name only) | `AV8BNA.lua`, `Su-30.lua` | The payload-presets ruling |

17 preset files load with no preset: their `payloads` table is empty.

### Every file

| Static template | Bytes | Result | Groups | Units | Statics | Theatre |
|---|---:|---|---:|---:|---:|---|
| `MissionEditor/data/scripts/StaticTemplate/German Radar Site - Channel Map - BULLDOGGE - Near Boulogne..stm` | 17768 | loaded | 1 | 13 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/German Radar Site - Channel Map - DICKHÄUTER - Near Dieppe..stm` | 25904 | loaded | 1 | 8 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/German Radar Site - Channel Map - KRABBE - Near Dunkirk..stm` | 15912 | loaded | 1 | 11 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/German Radar Site - Channel Map - PFERD- Near Prédefin..stm` | 25831 | loaded | 1 | 8 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/German Radar Site - Channel Map - SKORPION - Near baie de Somme..stm` | 16874 | loaded | 1 | 12 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/German Radar Site - Channel Map - WELS - Near Wissant..stm` | 15901 | loaded | 1 | 11 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/German Radar Sites - Channel Map.stm` | 84812 | loaded | 6 | 63 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/NASAMS_TEMP.stm` | 51284 | loaded | 3 | 26 | 0 | Caucasus |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch SIte - Siracourt - Channel Map.stm` | 26481 | loaded | 1 | 9 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Crécy Forest - Channel Map.stm` | 26522 | loaded | 1 | 9 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Flixecourt - Channel Map.stm` | 25324 | loaded | 1 | 8 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Herbouville - Channel Map.stm` | 26105 | loaded | 1 | 8 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Ligescourt - Channel Map.stm` | 26503 | loaded | 1 | 9 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Maisoncelle - Channel Map.stm` | 27685 | loaded | 1 | 10 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Neuville - Channel Map.stm` | 26457 | loaded | 1 | 9 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Vacquerie le Bouc - Channel Map.stm` | 26595 | loaded | 1 | 9 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Vacqueriette - Channel Map.stm` | 26546 | loaded | 1 | 9 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Val Ygot - Channel Map.stm` | 25326 | loaded | 1 | 8 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Site - Wallon-Cappel - Channel Map.stm` | 26557 | loaded | 1 | 9 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/V1 Launch Sites - Channel Map.stm` | 147286 | loaded | 11 | 97 | 0 | TheChannel |
| `MissionEditor/data/scripts/StaticTemplate/testx4.stm` | 5857 | refused: read, path: the file is not a static template file: it holds no vehicle group and no static | | | | |
| `MissionEditor/data/scripts/StaticTemplate/testx5.stm` | 5857 | refused: read, path: the file is not a static template file: it holds no vehicle group and no static | | | | |
| `MissionEditor/data/scripts/StaticTemplate/testx6.stm` | 5857 | refused: read, path: the file is not a static template file: it holds no vehicle group and no static | | | | |

| Preset file | Bytes | Result | Type | Entries | Names | Matched | Empty pylons |
|---|---:|---|---|---:|---:|---:|---:|
| `CoreMods/WWII Units/UnitPayloads/A-20G.lua` | 429 | loaded | `A-20G` | 1 | 1 | 1 | 0 |
| `CoreMods/WWII Units/UnitPayloads/Bf-109K-4.lua` | 908 | loaded | `Bf-109K-4` | 3 | 3 | 3 | 0 |
| `CoreMods/WWII Units/UnitPayloads/FW-190A8.lua` | 2467 | loaded | `FW-190A8` | 13 | 13 | 13 | 0 |
| `CoreMods/WWII Units/UnitPayloads/FW-190D9.lua` | 1123 | loaded | `FW-190D9` | 4 | 4 | 4 | 0 |
| `CoreMods/WWII Units/UnitPayloads/MosquitoFBMkVI.lua` | 6398 | loaded | `MosquitoFBMkVI` | 6 | 6 | 6 | 2 |
| `CoreMods/WWII Units/UnitPayloads/P-47D-30.lua` | 1283 | loaded | `P-47D-30` | 4 | 4 | 4 | 0 |
| `CoreMods/WWII Units/UnitPayloads/P-47D-30bl1.lua` | 686 | loaded | `P-47D-30bl1` | 2 | 2 | 2 | 0 |
| `CoreMods/WWII Units/UnitPayloads/P-47D-40.lua` | 2171 | loaded | `P-47D-40` | 6 | 6 | 6 | 0 |
| `CoreMods/WWII Units/UnitPayloads/P-51D-30-NA.lua` | 3500 | loaded | `P-51D-30-NA` | 7 | 7 | 7 | 0 |
| `CoreMods/WWII Units/UnitPayloads/P-51D.lua` | 3488 | loaded | `P-51D` | 7 | 7 | 7 | 0 |
| `CoreMods/aircraft/A-10/UnitPayloads/A-10A.lua` | 11906 | loaded | `A-10A` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/A-10/UnitPayloads/A-10C.lua` | 53542 | loaded | `A-10C` | 61 | 61 | 61 | 0 |
| `CoreMods/aircraft/A-10/UnitPayloads/A-10C_2.lua` | 69906 | loaded | `A-10C_2` | 78 | 78 | 78 | 0 |
| `CoreMods/aircraft/A-6E/UnitPayloads/A6E.lua` | 17489 | loaded | `A6E` | 24 | 24 | 24 | 0 |
| `CoreMods/aircraft/AH-64D/UnitPayloads/AH-64D_BLK_II.lua` | 15262 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 2: expected `return ExtFuelTankID`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/AJS37/UnitPayloads/AJS37.lua` | 16026 | loaded | `AJS37` | 32 | 32 | 32 | 0 |
| `CoreMods/aircraft/AV8BNA/UnitPayloads/AV8BNA.lua` | 24328 | loaded; refused presets: "H-L-H: Mk-82SEx6, GAU-12": name: the file has more than one preset named "H-L-H: Mk-82SEx6, GAU-12" | `AV8BNA` | 31 | 30 | 29 | 0 |
| `CoreMods/aircraft/C-101/UnitPayloads/C-101CC.lua` | 12397 | loaded | `C-101CC` | 26 | 26 | 26 | 0 |
| `CoreMods/aircraft/C-101/UnitPayloads/C-101EB.lua` | 919 | loaded | `C-101EB` | 3 | 3 | 3 | 0 |
| `CoreMods/aircraft/ChinaAssetPack/UnitPayloads/H-6J.lua` | 5058 | loaded | `H-6J` | 11 | 11 | 11 | 0 |
| `CoreMods/aircraft/ChinaAssetPack/UnitPayloads/J-11A.lua` | 38598 | loaded | `J-11A` | 25 | 25 | 25 | 0 |
| `CoreMods/aircraft/ChinaAssetPack/UnitPayloads/JF-17.lua` | 50877 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 3: expected `return tIntercept`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/ChinaAssetPack/UnitPayloads/WingLoong-I.lua` | 433 | loaded | `WingLoong-I` | 1 | 1 | 1 | 0 |
| `CoreMods/aircraft/Christen Eagle II/UnitPayloads/Christen Eagle II.lua` | 366 | loaded | `Christen Eagle II` | 1 | 1 | 1 | 0 |
| `CoreMods/aircraft/F-100D/UnitPayloads/F-100D.lua` | 5314 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 14: expected `local <name> = {`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/F-15E/UnitPayloads/F-15ESE.lua` | 17464 | loaded | `F-15ESE` | 18 | 18 | 18 | 0 |
| `CoreMods/aircraft/F-16C/UnitPayloads/F-16C_50.lua` | 54128 | loaded | `F-16C_50` | 48 | 48 | 48 | 0 |
| `CoreMods/aircraft/F-4E/UnitPayloads/F-4E-45MC.lua` | 223869 | loaded | `F-4E-45MC` | 154 | 154 | 154 | 0 |
| `CoreMods/aircraft/F-5E/UnitPayloads/F-5E-3.lua` | 20284 | loaded | `F-5E-3` | 34 | 34 | 34 | 0 |
| `CoreMods/aircraft/F-5E/UnitPayloads/F-5E-3_FC.lua` | 19129 | loaded | `F-5E-3_FC` | 32 | 32 | 32 | 0 |
| `CoreMods/aircraft/F-5E/UnitPayloads/F-5E.lua` | 19578 | loaded | `F-5E` | 33 | 33 | 33 | 0 |
| `CoreMods/aircraft/F-86/UnitPayloads/F-86F Sabre.lua` | 3990 | loaded | `F-86F Sabre` | 10 | 10 | 10 | 0 |
| `CoreMods/aircraft/F-86/UnitPayloads/F-86F_FC.lua` | 3982 | loaded | `F-86F_FC` | 10 | 10 | 10 | 0 |
| `CoreMods/aircraft/F14/UnitPayloads/F-14A-135-GR-Early.lua` | 37835 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 1: expected `=`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/F14/UnitPayloads/F-14A-135-GR.lua` | 37834 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 1: expected `=`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/F14/UnitPayloads/F-14A-95-GR.lua` | 21098 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 1: expected `=`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/F14/UnitPayloads/F-14B.lua` | 37831 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 1: expected `=`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/F14/UnitPayloads/F-14BU.lua` | 34156 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 1: expected `=`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/F4U-1D/UnitPayloads/F4U-1D.lua` | 9819 | loaded | `F4U-1D` | 10 | 10 | 10 | 0 |
| `CoreMods/aircraft/F4U-1D/UnitPayloads/F4U-1D_CW.lua` | 9825 | loaded | `F4U-1D_CW` | 10 | 10 | 10 | 0 |
| `CoreMods/aircraft/FA-18C/UnitPayloads/FA-18A.lua` | 18081 | loaded | `F/A-18A` | 19 | 19 | 19 | 0 |
| `CoreMods/aircraft/FA-18C/UnitPayloads/FA-18C.lua` | 24485 | loaded | `F/A-18C` | 25 | 25 | 25 | 0 |
| `CoreMods/aircraft/FA-18C/UnitPayloads/FA-18C_hornet.lua` | 17779 | loaded | `FA-18C_hornet` | 25 | 25 | 25 | 0 |
| `CoreMods/aircraft/I-16/UnitPayloads/I-16.lua` | 3118 | loaded | `I-16` | 5 | 5 | 5 | 0 |
| `CoreMods/aircraft/Ka-50/UnitPayloads/Ka-50.lua` | 16395 | loaded | `Ka-50` | 37 | 37 | 37 | 0 |
| `CoreMods/aircraft/Ka-50/UnitPayloads/Ka-50_3.lua` | 9669 | loaded | `Ka-50_3` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/L-39/UnitPayloads/L-39C.lua` | 1729 | loaded | `L-39C` | 6 | 6 | 6 | 0 |
| `CoreMods/aircraft/L-39/UnitPayloads/L-39ZA.lua` | 7228 | loaded | `L-39ZA` | 18 | 18 | 18 | 0 |
| `CoreMods/aircraft/La-7/UnitPayloads/La-7.lua` | 965 | loaded | `La-7` | 2 | 2 | 2 | 0 |
| `CoreMods/aircraft/M-2000C/UnitPayloads/M-2000C.lua` | 9179 | loaded | `M-2000C` | 18 | 18 | 18 | 0 |
| `CoreMods/aircraft/MB-339/UnitPayloads/MB-339A.lua` | 14406 | loaded | `MB-339A` | 18 | 18 | 18 | 0 |
| `CoreMods/aircraft/MB-339/UnitPayloads/MB-339APAN.lua` | 7023 | loaded | `MB-339APAN` | 9 | 9 | 9 | 0 |
| `CoreMods/aircraft/Mi-24P/UnitPayloads/Mi-24P.lua` | 12379 | loaded | `Mi-24P` | 20 | 20 | 20 | 0 |
| `CoreMods/aircraft/MiG-15bis/UnitPayloads/MiG-15bis.lua` | 1991 | loaded | `MiG-15bis` | 7 | 7 | 7 | 0 |
| `CoreMods/aircraft/MiG-15bis/UnitPayloads/MiG-15bis_FC.lua` | 1992 | loaded | `MiG-15bis_FC` | 7 | 7 | 7 | 0 |
| `CoreMods/aircraft/MiG-19P/UnitPayloads/MiG-19P.lua` | 4541 | loaded | `MiG-19P` | 11 | 11 | 11 | 0 |
| `CoreMods/aircraft/MiG-21bis/UnitPayloads/MiG-21bis.lua` | 14996 | loaded | `MiG-21Bis` | 27 | 27 | 27 | 0 |
| `CoreMods/aircraft/MiG-29-Fulcrum/UnitPayloads/MiG-29 Fulcrum.lua` | 33353 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: line 2: expected `return Missile_R60_ID`; a file with Lua code is not read: save the preset from the Mission Editor, or write its pylons in `extra.payload.pylons` | | | | | |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1B.lua` | 9648 | loaded | `Mirage-F1B` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1BD.lua` | 8954 | loaded | `Mirage-F1BD` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1BE.lua` | 11914 | loaded | `Mirage-F1BE` | 21 | 21 | 21 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1BQ.lua` | 8954 | loaded | `Mirage-F1BQ` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1C-200.lua` | 9656 | loaded | `Mirage-F1C-200` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1C.lua` | 9713 | loaded | `Mirage-F1C` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CE.lua` | 11917 | loaded | `Mirage-F1CE` | 21 | 21 | 21 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CG.lua` | 10204 | loaded | `Mirage-F1CG` | 17 | 17 | 17 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CH.lua` | 9650 | loaded | `Mirage-F1CH` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CJ.lua` | 9650 | loaded | `Mirage-F1CJ` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CK.lua` | 9650 | loaded | `Mirage-F1CK` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CR.lua` | 7366 | loaded | `Mirage-F1CR` | 12 | 12 | 12 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CT.lua` | 8954 | loaded | `Mirage-F1CT` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1CZ.lua` | 9650 | loaded | `Mirage-F1CZ` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1DDA.lua` | 8956 | loaded | `Mirage-F1DDA` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1ED.lua` | 9637 | loaded | `Mirage-F1ED` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1EDA.lua` | 8956 | loaded | `Mirage-F1EDA` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1EE.lua` | 12511 | loaded | `Mirage-F1EE` | 22 | 22 | 22 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1EH.lua` | 9650 | loaded | `Mirage-F1EH` | 16 | 16 | 16 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1EQ.lua` | 8959 | loaded | `Mirage-F1EQ` | 15 | 15 | 15 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1JA.lua` | 8892 | loaded | `Mirage-F1JA` | 14 | 14 | 14 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1M-CE.lua` | 11395 | loaded | `Mirage-F1M-CE` | 20 | 20 | 20 | 0 |
| `CoreMods/aircraft/Mirage-F1/UnitPayloads/Mirage-F1M-EE.lua` | 11395 | loaded | `Mirage-F1M-EE` | 20 | 20 | 20 | 0 |
| `CoreMods/aircraft/OH-58D/UnitPayloads/OH58D.lua` | 5182 | loaded | `OH58D` | 12 | 12 | 12 | 0 |
| `CoreMods/aircraft/SA342/UnitPayloads/SA342L.lua` | 3241 | refused: read, path: the file is not a payload preset file in the data form `local <name> = {...}` then `return <name>`: `payloads` has two entries under [4] | | | | | |
| `CoreMods/aircraft/SA342/UnitPayloads/SA342M.lua` | 1363 | loaded | `SA342M` | 3 | 3 | 3 | 0 |
| `CoreMods/aircraft/SA342/UnitPayloads/SA342Minigun.lua` | 756 | loaded | `SA342Minigun` | 2 | 2 | 2 | 0 |
| `CoreMods/aircraft/SA342/UnitPayloads/SA342Mistral.lua` | 1678 | loaded | `SA342Mistral` | 3 | 3 | 3 | 0 |
| `CoreMods/tech/Currenthill Assets Pack/UnitPayloads/CHAP_TigerUHT.lua` | 4166 | loaded | `CHAP_TigerUHT` | 9 | 9 | 9 | 0 |
| `CoreMods/tech/Currenthill Assets Pack/UnitPayloads/Mi-28N.lua` | 15186 | loaded | `Mi-28N` | 37 | 37 | 37 | 0 |
| `CoreMods/tech/Currenthill Assets Pack/UnitPayloads/Tu-95MS.lua` | 4496 | loaded | `Tu-95MS` | 6 | 6 | 6 | 0 |
| `CoreMods/tech/HeavyMetalCore/UnitPayloads/B-1B.lua` | 3525 | loaded | `B-1B` | 10 | 10 | 10 | 0 |
| `CoreMods/tech/HeavyMetalCore/UnitPayloads/B-52H.lua` | 1524 | loaded | `B-52H` | 4 | 4 | 4 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/A-50.lua` | 111 | loaded | `A-50` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/AH-1W.lua` | 6755 | loaded | `AH-1W` | 16 | 16 | 16 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/AH-64A.lua` | 3142 | loaded | `AH-64A` | 7 | 7 | 7 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/AH-64D.lua` | 3142 | loaded | `AH-64D` | 7 | 7 | 7 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/An-26B.lua` | 115 | loaded | `An-26B` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/An-30M.lua` | 115 | loaded | `An-30M` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/C-130.lua` | 113 | loaded | `C-130` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/C-17A.lua` | 113 | loaded | `C-17A` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/CH-47D.lua` | 115 | loaded | `CH-47D` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/CH-53E.lua` | 115 | loaded | `CH-53E` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/E-2C.lua` | 111 | loaded | `E-2C` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/E-3A.lua` | 111 | loaded | `E-3A` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-117A.lua` | 997 | loaded | `F-117A` | 3 | 3 | 3 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-14A.lua` | 2261 | loaded | `F-14A` | 3 | 3 | 3 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-15C.lua` | 12238 | loaded | `F-15C` | 11 | 11 | 11 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-15E.lua` | 42266 | loaded | `F-15E` | 31 | 31 | 31 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-16A MLU.lua` | 20608 | loaded | `F-16A MLU` | 23 | 23 | 23 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-16A.lua` | 17387 | loaded | `F-16A` | 20 | 20 | 20 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-16C bl.50.lua` | 16233 | loaded | `F-16C bl.50` | 16 | 16 | 16 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-16C.lua` | 22559 | loaded | `F-16C bl.52d` | 25 | 25 | 25 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/F-4E.lua` | 11585 | loaded | `F-4E` | 15 | 15 | 15 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/IL-76MD.lua` | 117 | loaded | `IL-76MD` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/IL-78M.lua` | 115 | loaded | `IL-78M` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/KC-10A.lua` | 115 | loaded | `KC-10A` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/KC-135.lua` | 115 | loaded | `KC-135` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Ka-27.lua` | 113 | loaded | `Ka-27` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Ka-52.lua` | 4304 | loaded | `Ka-52` | 9 | 9 | 9 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MQ-9 Reaper.lua` | 1833 | loaded | `MQ-9 Reaper` | 4 | 4 | 4 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Mi-24V.lua` | 14097 | loaded | `Mi-24V` | 29 | 29 | 29 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Mi-26.lua` | 113 | loaded | `Mi-26` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Mi-8MT.lua` | 3795 | loaded | `Mi-8MT` | 7 | 7 | 7 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-23MLD.lua` | 6419 | loaded | `MiG-23MLD` | 12 | 12 | 12 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-25PD.lua` | 1672 | loaded | `MiG-25PD` | 3 | 3 | 3 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-25RBT.lua` | 792 | loaded | `MiG-25RBT` | 2 | 2 | 2 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-27K.lua` | 8415 | loaded | `MiG-27K` | 14 | 14 | 14 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-29A.lua` | 14328 | loaded | `MiG-29A` | 19 | 19 | 19 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-29G.lua` | 8020 | loaded | `MiG-29G` | 11 | 11 | 11 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-29S.lua` | 16660 | loaded | `MiG-29S` | 22 | 22 | 22 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/MiG-31.lua` | 3023 | loaded | `MiG-31` | 4 | 4 | 4 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Mirage 2000-5.lua` | 6070 | loaded | `Mirage 2000-5` | 8 | 8 | 8 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/OH-58D.lua` | 1749 | loaded | `OH-58D` | 6 | 6 | 6 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/RQ-1A Predator.lua` | 427 | loaded | `RQ-1A Predator` | 1 | 1 | 1 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/S-3B.lua` | 4538 | loaded | `S-3B` | 7 | 7 | 7 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/SH-3W.lua` | 113 | loaded | `SH-3W` | 0 | 0 | 0 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/SH-60B.lua` | 312 | loaded | `SH-60B` | 1 | 1 | 1 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-17M4.lua` | 15191 | loaded | `Su-17M4` | 19 | 19 | 19 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-24M.lua` | 18944 | loaded | `Su-24M` | 33 | 33 | 33 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-24MR.lua` | 2415 | loaded | `Su-24MR` | 5 | 5 | 5 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-25.lua` | 31299 | loaded | `Su-25` | 30 | 30 | 30 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-25T.lua` | 37417 | loaded | `Su-25T` | 35 | 35 | 35 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-25TM.lua` | 34071 | loaded | `Su-25TM` | 32 | 32 | 32 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-27.lua` | 28162 | loaded | `Su-27` | 28 | 28 | 28 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-30.lua` | 31796 | loaded; refused presets: "Kh-31P*2,Kh-31A*2,R-73*2,R-77*2,ECM": name: the file has more than one preset named "Kh-31P*2,Kh-31A*2,R-73*2,R-77*2,ECM" | `Su-30` | 31 | 30 | 29 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-33.lua` | 26472 | loaded | `Su-33` | 23 | 23 | 23 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Su-34.lua` | 21481 | loaded | `Su-34` | 20 | 20 | 20 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Tornado GR4.lua` | 4850 | loaded | `Tornado GR4` | 5 | 5 | 5 | 4 |
| `MissionEditor/data/scripts/UnitPayloads/Tornado IDS.lua` | 7021 | loaded | `Tornado IDS` | 8 | 8 | 8 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Tu-142.lua` | 312 | loaded | `Tu-142` | 1 | 1 | 1 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Tu-160.lua` | 409 | loaded | `Tu-160` | 1 | 1 | 1 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/Tu-22M3.lua` | 2243 | loaded | `Tu-22M3` | 6 | 6 | 6 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/UH-1H.lua` | 534 | loaded | `UH-1H` | 1 | 1 | 1 | 0 |
| `MissionEditor/data/scripts/UnitPayloads/UH-60A.lua` | 115 | loaded | `UH-60A` | 0 | 0 | 0 | 0 |

files 176: static templates 20 loaded of 23; preset files 143 loaded of 153, 2 of them with a refused preset
