# Aircraft pylons from a payload preset file

`PutGroup` gives a unit the pylons of a Mission Editor payload preset when the unit
names one in `payload_preset`. The sidecar reads the named file once, as data, and
stores the pylons in `extra.payload.pylons`; the field itself is not stored
(spec/world.md, "Payload presets").

## What Eagle Dynamics' files show

Read on 2026-11-24 in the install of a DCS dedicated server on Linux under Wine 10.0
(`wine-10.0 (Debian 10.0~repack-6)`), DCS 2.9.31.29434 (`dcs_server.release`), inside
its container, with `ls`, `find`, `head`, `tail`, `grep`, `sed`, `od` and `wc`. No
file was copied. `LU` is `MissionEditor/modules/me_loadoututils.lua`.

| Fact | Tag | Where |
|---|---|---|
| DCS ships 153 preset files: 62 in `MissionEditor/data/scripts/UnitPayloads/`, the rest in `UnitPayloads/` of modules under `CoreMods/aircraft/` (76), `CoreMods/WWII Units/` (10) and `CoreMods/tech/` (5); the server's `Mods/` holds none | ED-CODE | `find` over the install; docs/results/2026-11-24-real-files.md |
| The editor collects file names from `ConfigHelper.getUnitPayloadsSysPath()` and from `<plugin.dirName>/UnitPayloads` of every applied plugin, keyed by file name | ED-CODE | `LU` `getUnitPayloadFileNames`, `loadUnitPayloads` |
| It runs every file with `loadfile` and `pcall`, and takes the returned table's `unitType`, else its `name`, as the type | ED-CODE | `LU` `loadPayloads_` |
| It then merges the user's files from `ConfigHelper.getWriteDirUnitPayloads()`: a user preset with the name of a shipped one replaces it, others are added; pylons the type does not have are dropped | ED-CODE | `LU` `mergeUserPayloads` |
| `getWriteDirUnitPayloads()` returns `lfs.writedir()..'MissionEditor/UnitPayloads/'`, in the Saved Games profile. The editor saves a type's presets there as the shipped file's name for the type, else `<unitType>.lua`: `local `, the table serialized sorted as `unitPayloads`, then `return unitPayloads`, holding every preset that differs from the shipped one | ED-CODE | `Scripts/ConfigHelper.lua` lines 13 to 17 and 123 to 149; `LU` `saveUnitPayloads` |
| By first line: 140 files start `local unitPayloads =` (136 with the `{` on that line, 4 on the next); 7 start with another `local` statement, Lua code the reader refuses; 6 start with a blank line or a comment. All 153 end with `return unitPayloads`. None has a byte order mark or a carriage return | ED-CODE | first and last lines of all 153 |
| 5 files hold lines with `function`, `require` or `..`: `F-100D.lua`, `MiG-29 Fulcrum.lua`, `F4U-1D.lua`, `F4U-1D_CW.lua`, `F-14BU.lua` | ED-CODE | `grep -c` over all 153 |
| The reader takes 143 of the 153 files; in 2 of those it refuses one preset, as two presets share its name | ED-CODE | docs/results/2026-11-24-real-files.md |
| Keys in all files: `CLSID` (16,602), `num`, `name`, `tasks`, `pylons`, `displayName`, `unitType`, `payloads`, `category`, `settings` (125); in `settings`: `NFP_PRESID`, `NFP_PRESVER`, `NFP_fuze_type_nose`, `NFP_fuze_type_tail`, `NFP_fuze_type`, `GUI_fuze_type`, `laser_code` and weapon-specific keys | ED-CODE | key counts over all 153 |
| `settings` appears in 5 files; of these only `CoreMods/aircraft/F-4E/UnitPayloads/F-4E-45MC.lua`, 223,869 bytes, the largest file, starts with `local unitPayloads =` and holds no line with `function`, `require` or `..`; the reader takes it whole, all 154 presets | ED-CODE | the same; docs/results/2026-11-24-real-files.md |
| A preset gives a unit its pylons only: `getUnitPylons(unitType, payloadName)` builds `pylons[num] = {CLSID, settings}`, the settings merged over the launcher's defaults, matching the preset by `name` | ED-CODE | `LU` `getUnitPylons`, `getAppropriateSettings` |
| Fuel, flares, chaff and gun are set apart from the preset; `fuel` is a mass | ED-CODE | `MissionEditor/modules/me_payload.lua` |
| The server's GUI state has `me_loadoututils` loaded, but its table is filled only by `init` or `initBriefingRoomPayloads`, and no caller of either was found in `MissionEditor/` or `Scripts/` | ED-CODE; filled on the server UNVERIFIED | `lua/mock/api/gui.txt`; `grep` |

## What the call does

| Step | Rule |
|---|---|
| Path | Each distinct `payload_preset.path` of the request: the rules and messages of `SetCampaignTemplate`. |
| Read | Once per distinct path, on a worker thread, without the hub's lock, at most 1 MiB, by `lua::parse_returned`: exactly `local N = <value>` then `return N`. `lua::parse` is unchanged and still refuses both words. |
| Convert | The preset of that `name` in a file whose `unitType` (else `name`) is the unit's type: each pylon to `extra.payload.pylons["<num>"] = {CLSID, settings?}`. |
| Check | Every refusal before the converted spec reaches `PutGroup`'s own checks; then `PutGroup` as written. |
| After | The store holds the pylons. Nothing opens the path again; a replace that names a preset reads it again. |

## Decisions made in this step

- **The ruling, by an Opus quorum** (the coordinator's record, "Payload presets
  quorum"): a file in `template_dirs` read as data, `UnitSpec.payload_preset`
  converted and not stored, a code file refused whole, exact type match, preset and
  pylons together refused, the 1 MiB cap, settings copied only if DCS accepts them.
- **A pylon number is a whole number from 1 to 1,000.** DCS numbers pylons from 1, and
  no shipped aircraft has more than a few dozen; the bound keeps a key in the range the
  API's integer keys take.
- **A request names at most 16 distinct preset paths, and each file is dropped once
  its presets are converted.** Every file is read and parsed, up to 1 MiB and its
  tree, before the call takes the lock; the cap bounds that work, and a real group
  needs one file per aircraft type.
- **Two entries under one key in `payloads` or `pylons` are refused**, as the
  static-template reader refuses them: Lua keeps the later one, and which the author
  meant is not knowable.
- **A preset with no `pylons` table is refused**, while an empty one is a clean
  loadout. The editor's `getUnitPylons` iterates `pylons` and would fail on a missing
  one.
- **`settings` must be a table that comes back from the API form unchanged.** A
  non-integer number key, a string that is not UTF-8 or a duplicated key would change
  on the way to DCS, so they are refused instead.
- **`settings` are copied.** The live check spawned two aircraft with them and DCS took
  both; see below.
- **A local's name may not be a Lua reserved word**, and `return N;` with a semicolon
  is refused: neither is in a shipped file, and the form allows comments and white
  space only.

## Live check

Script: `cargo xtask sidecar <target> --part payload-preset`
(`crates/xtask/src/shell_live/payload_preset.rs`). DCS 2.9.31.29434
(`dcs_server.release`), dedicated server on Linux under Wine 10.0
(`wine-10.0 (Debian 10.0~repack-6)`), Caucasus, 2026-11-24. The run's agent build was
`5eb6e3f41ba036fd`, its sidecar binary had SHA-256
`e748d06faa040c066caf88ff25c2a46bbcfa813204a9f9fb6dc8388aff8af074`, and its tree was
`v0.1.0-18-g5e6a217-dirty`, this step's work before review.

The run writes a preset file of its own in the data form, `np-live-presets.lua`, into
the folder its `sidecar.toml` lists on the profile side, with one preset of three
pylons of an F-4E-45MC, taken from a shipped preset of that type: Mk-84 bombs on
pylons 1 and 13, each with the shipped fuze `settings`, and an AIM-7E-2 on pylon 9. It
makes one aircraft from the preset and one with the same pylons written by hand in
`extra.payload`, both in the air at 3,000 m, with no `fuel`, `flare`, `chaff` or `gun`.
It reads both by `Unit.getByName` (`getAmmo`, `getFuel`, `getDesc().fuelMassMax`),
reads the stored unit rows from `ExportCampaign`, then removes both groups and the
file.

Rule: both aircraft present and of the type, the same ammunition with one weapon for
each of the three pylons (gun rounds not counted), the same stored `extra`, whose
pylons are the ones written by hand; a preset sent with `pylons` also is refused; the
file and the groups are gone after. The weapon count was added to the script after
run 3, which asked only for the same non-empty ammunition; run 3's reads hold 3
weapons each and meet it.

| Item | Result |
|---|---|
| Ammunition, preset | `weapons.bombs.Mk_84` 2, `weapons.missiles.HB-AIM-7E-2` 1, `supply.shells.1` 639 |
| Ammunition, by hand | the same |
| Stored `extra` | the same for both, decoded from the export: the hand-written pylons with their `settings`, plus `speed` and `alt_type` |
| Fuel | `getFuel` 0.45 for both, of a `fuelMassMax` of 5,510.5 kg |
| Preset and `pylons` together | refused: "a unit that names a payload preset carries no `pylons`; remove them to use the preset" |
| File and groups | removed |
| Verdict | PASS |

DCS spawned both aircraft with the pylons' `settings`. The scripting API reads no fuze,
so whether DCS applied them is UNVERIFIED. With no `fuel` the aircraft had 45 % of
internal fuel; the API's comment says the controller supplies `fuel`.

Runs used: 3 of 3. The first stopped on the Mac before it reached the server: the
sidecar's Linux build found no `zig` through the shim outside `mise exec`. The second
reached the server and read the result above, but its verdict failed on its own
comparison: the export gives a unit's `extra` as the store's encoding in base 64, not
as JSON. The harness decodes it, with a host test, and the third run passed.

**Changed after run 3, from the review.** In the script: the weapon count above; the
refusal probe removes at once a group it makes, should the call be accepted. In the
sidecar: presets are converted on the worker thread, each file dropped before the next;
a call names at most 16 preset files; a file's presets are found through a name index;
two entries under one key in `payloads` or `pylons`, and `settings` holding `nil`, are
refused; a unit with no type is refused for that before its preset is looked up. None
changes a step or a read of the run that passed: its file had one preset under one key,
no `nil` and one path, every unit had a type, and its probe was refused. Run 3's reads
pass the current verdict, which the host test `the_verdict_passes_a_good_read_and_fails_each_fault`
holds to the same values. On Linux under Wine the later sidecar code is covered by host
tests only. The Windows run below ran it live.

## Live check on Windows

Script: `cargo xtask sidecar <target> --part payload-preset` on a windows-ssh target
(`crates/xtask/src/sidecar_windows.rs`, driving the same
`crates/xtask/src/shell_live/payload_preset.rs`). DCS 2.9.31.29434 client on Windows,
hosting a private session, Caucasus, 2026-11-24. The table is run 3's, on this
commit's tree: agent build `59c32c8a9b1bb30f`, sidecar binary (`x86_64-pc-windows-msvc`)
SHA-256 `2e54ce242313e450185b24c5ed3d231f9113ee221f0e0c111ccd5a2e0bda227a`, the merged
sidecar code.

The client has no dedicated server. The run refuses when DCS already runs, starts DCS
through the profile's start task and hosts a private session with the shell mission.
The sidecar is a Windows program in the profile's `nanoprobe/dev/sidecar/`, with a
loopback listener and native paths in its `sidecar.toml`, held by an SSH session; the
API is reached through an SSH local forward. At the end the run stops the sidecar,
closes DCS through the stop task, removes its folders, undeploys, and fails unless the
profile's top folders list what they listed before.

The rule is the Wine run's, with one item left out. On Windows the sidecar stops
cleanly only on Ctrl-C, which a process in another SSH session cannot be sent, so the
run ends its process with `Stop-Process -Id` and records its exit code, -1; "exit code
0 after a stop" is not part of the Windows verdict.

| Item | Result |
|---|---|
| Ammunition, preset | `weapons.bombs.Mk_84` 2, `weapons.missiles.HB-AIM-7E-2` 1, `supply.shells.1` 639 |
| Ammunition, by hand | the same |
| One weapon for each pylon | yes |
| Stored `extra` | the same for both: the hand-written pylons with their `settings`, plus `speed` and `alt_type` |
| Fuel | `getFuel` 0.45 for both, of a `fuelMassMax` of 5,510.5 kg |
| Preset and `pylons` together | refused: "a unit that names a payload preset carries no `pylons`; remove them to use the preset" |
| File and groups | removed |
| Profile after the run | the top folders as before; DCS closed |
| Verdict | PASS (LIVE) |

Runs used: 3 of 3, each a PASS with the reads above. Run 1 ran the harness before
review. Runs 2 and 3 ran it after the review's fixes (the start script writes the
sidecar's process ID and exit code, the stop ends that process by its ID, the run
refuses a profile with a `nanoprobe` folder or a client where a sidecar runs, and the
restore verdict is a tested function); run 3 also takes the process handle that lets
the exit code be read, which run 2 recorded as empty.
