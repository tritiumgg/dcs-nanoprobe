# Groups and statics from a static template file

`PutFromStaticTemplate` makes every vehicle group and static of a Mission Editor static
template (`.stm`) that the administrator provides, in one store commit or none. The
sidecar reads the file once, as data, and keeps the converted specs (spec/world.md,
"Static templates").

## What Eagle Dynamics' files show

Read on 2026-11-23 in the install of a DCS dedicated server on Linux under Wine 10.0
(`wine-10.0 (Debian 10.0~repack-6)`), DCS 2.9.31.29434 (`dcs_server.release`), inside
its container, with `ls`, `wc`, `grep`, `sed`, `awk` and `cat`. Nothing was written and
no file was copied. `MS` is `MissionEditor/modules/me_staticTemplate.lua` and `MM` is
`MissionEditor/modules/me_mission.lua`.

| Fact | Tag | Where |
|---|---|---|
| DCS ships 23 static templates, 5,857 to 147,286 bytes, dated 19 September | ED-CODE | `MissionEditor/data/scripts/StaticTemplate/*.stm` |
| Each file is one statement: three lines start in column 0, `staticTemplate = `, `{` and `} -- end of staticTemplate`, apart from the continuation lines of a `desc` string written with `\` and a newline | ED-CODE | the same |
| The top keys are `coalition`, `name`, `desc`, `theatre`, `requiredModules` and `localization`; `theatre` is `"Caucasus"` in 4 files and `"TheChannel"` in 19 | ED-CODE | the same |
| `coalition` holds `blue`, `red` and `neutrals`; each a `name` and a `country` list; each country an `id`, its DCS number, a `name`, and its kinds | ED-CODE | the same; `MS` lines 45 to 85 |
| The editor saves the open mission's objects with the mission's serializer, `U.saveInFile(um, 'staticTemplate', file)`, into `lfs.writedir()..'StaticTemplate/'`, the Saved Games profile, as `<name>.stm` | ED-CODE | `MS` lines 92 to 111; `me_staticTemplateSave.lua` |
| The kinds it writes are `plane`, `helicopter`, `ship`, `vehicle`, `static` and `complex`, each a `group` list | ED-CODE | `MS` lines 56 to 73; `MM` lines 292 to 299 |
| Every place is an absolute mission `x` and `y`. A static template has no origin and no offsets | ED-CODE | the shipped files; `MM` lines 4252 to 4300 and 4419 to 4500 |
| The editor loads a static template where it was saved: no anchor and no turn. It renumbers the IDs and skips a country the mission does not hold | ED-CODE | `MS` lines 149 to 233; `MM` lines 3112 to 3200 |
| The 23 shipped files hold only `vehicle` groups: 44 groups, 346 units. No `static`, `ship`, `plane`, `helicopter` or `complex` | ED-CODE | key counts over all 23 |
| A vehicle group has `groupId`, `name`, `x`, `y`, `start_time`, `task`, `taskSelected`, `tasks`, `route`, `hidden`, `visible` and `uncontrollable`; the editor also writes `lateActivation`, `condition`, `probability`, `manualHeading`, `hiddenOnPlanner` and `hiddenOnMFD`. The shipped routes are one point each | ED-CODE | the shipped files; `MM` lines 4252 to 4275 |
| A vehicle unit has `unitId`, `name`, `type`, `skill`, `x`, `y`, `heading`, `playerCanDrive`, `transportable` and `AddPropVehicle`; the editor also writes `ammo`, `livery_id`, `coldAtStart`, `wagons`, `modulation`, `frequency` and `linksSources` | ED-CODE | the same, lines 4277 to 4300 |
| A static is a group of `groupId`, `name`, `heading`, `hidden`, `dead`, `linkOffset`, `route` (one point, which may carry `linkUnit`), `x`, `y` and `units`; each unit `unitId`, `name`, `type`, `category`, `shape_name`, `heading`, `x`, `y`, `rate`, `mass`, `canCargo`, `livery_id`, heliport and grass-airfield keys, and `resourcePayload` | ED-CODE | `MM` lines 4419 to 4500 |
| A static's `category` is one of `Planes`, `Helicopters`, `Ships`, `Fortifications`, `Heliports`, `GrassAirfields`, `Warehouses`, `Cargos`, `LTAvehicles`, `WWII structures`, `Effects`, `Animal` and `Personnel`, by the type's own category | ED-CODE | `MissionEditor/modules/me_crutches.lua` lines 67 to 81 |
| `FARP Tent` and `FARP Fuel Depot` are in the list `default_Fortifications` | ED-CODE | `Scripts/Database/db_countries.lua`, from line 45 |
| What a static looks like in a file the editor saved | LIVE for the keys a saved file with a fortification and a helipad holds (docs/results/2026-11-24-template-heliports.md); `hidden`, `linkOffset`, `linkUnit`, `mass`, `canCargo`, `livery_id`, `resourcePayload` and the grass-airfield keys stay ED-CODE |

## What the call does

| Step | Rule |
|---|---|
| Path | The rules and messages of `SetCampaignTemplate`, as `PutGroupFromTemplate` takes them. |
| Read | Once, on a worker thread, without the hub's lock, and at most 1 MiB, by the data-only reader with the root `staticTemplate`. |
| Convert | Every vehicle group to a ground `GroupSpec` and every static unit to a `StaticSpec`, placed in place or at the anchor and turned about the layout's origin. |
| Check | Every check of `PutGroup` and `PutStatic` on every spec, the size of every spawn command, the encoding of every value, and the entity IDs left, before the first write. |
| Write | Every entity, in the call's one store commit. |
| After | A mission load, a reload, a backup, a restore, an export and a restarted sidecar use the store. Nothing opens the path again. |

## Decisions made in this step

- **The ruling, by an Opus quorum** (the coordinator's record, "Static templates
  quorum"): the RPC name, in place without an anchor, the bounding-box origin, one
  country override, the label prefix, the kinds refused, the keys dropped, the answer's
  shape.
- **Two entries under one number in a list, or under one key in `coalition` or a
  country, refuse the file.** Lua would keep the later of two equal keys; the editor
  never writes two, and which one the author meant is not knowable.
- **A static value the store cannot encode, such as a table nested deeper than the
  agent's encoding allows, is refused by the reader**, with its place in the file. The
  call checks every value again before the first write.
- **A list's numbers need not run 1 to n.** The editor writes a coalition's country list
  with the numbers the mission had, and skips a country with no objects
  (`MS` lines 53 to 84). The order is the text's.
- **The later of two equal string keys is used**, as Lua keeps it, also for a static
  key passed to DCS.
- **A static's heading is its unit's `heading`.** The editor writes the group's and the
  unit's; the unit's is the object's own.
- **The checks that a store write can still fail, an over-large value and the entity
  IDs, run before the first write.** `Store::ids_left` is new for the second. A write
  that fails after every check panics, which stops the sidecar as any half-done change
  does; no store is written half.
- **The log names at most 16 unused keys**, as for group templates.
- **The cap is 1 MiB**, the group-template reader's: seven times the largest shipped
  file, and the parsed-tree bound measured for that reader holds.

## Live

`cargo xtask sidecar <name> --part static-template`
(`crates/xtask/src/shell_live/static_template.rs`) on a DCS 2.9.31.29434 dedicated
server (`dcs_server.release`) on Linux under Wine 10.0 (`wine-10.0 (Debian
10.0~repack-6)`), 2026-11-23, from this commit's tree: agent build `5a90d060af06118a`,
sidecar binary sha256 `0010435823e3a5fe8411bdcc743fbae8ca83c968cca71018273b7d253d11358d`.
The sidecar ran in its own container beside DCS, as the group-template part runs it.

The run writes a static template of its own into the listed folder, in the editor's
form: under country 2, one vehicle group of two `M-113` and two static groups of one
`Fortifications` static each, `FARP Tent` and `FARP Fuel Depot`. Every offset from the
layout's origin (north -289,725, east 680,035) differs on its two axes. It makes the
layout twice: in place, and at an anchor 300 m north of the origin turned a quarter turn
clockwise. It reads every unit with `Unit.getByName` and every static with
`StaticObject.getByName`, by its platform name, then removes everything and the file,
also when a step fails.

| Call | Object | Asked: north, east, heading | Read from DCS: north, east, heading |
|---|---|---|---|
| In place | M-113 | -289,700, 680,050, 0 | -289,699.999999, 680,049.999999, 0 |
| In place | M-113 | -289,690, 680,080, 1.0 | -289,689.999999, 680,079.999999, 1.0000000 |
| In place | FARP Tent | -289,760, 680,020, 0.5 | -289,760, 680,020, 0.4999998 |
| In place | FARP Fuel Depot | -289,730, 679,990, 0 | -289,730, 679,990, 0 |
| Anchor, π/2 | M-113 | -289,440, 680,060, 1.5707963 | -289,439.999986, 680,059.999996, 1.5707964 |
| Anchor, π/2 | M-113 | -289,470, 680,070, 2.5707963 | -289,469.999986, 680,069.999996, 2.5707963 |
| Anchor, π/2 | FARP Tent | -289,410, 680,000, 2.0707963 | -289,410, 680,000, 2.0707963 |
| Anchor, π/2 | FARP Fuel Depot | -289,380, 680,030, 1.5707963 | -289,380, 680,030, 1.5707963 |

| Check | Result |
|---|---|
| Each object within 1 m of its place | Yes: within 15 µm. The file's `x` went north and `y` east; the turn put north to the east and east to the south. No object stood where exchanged axes would put it |
| Type and country, from `getTypeName` and `getCountry` | As asked, all eight |
| Heading within 0.1 rad, recorded and not part of the verdict | Yes, all eight |
| Every entity answered and listed after its call | Each answer held one group of two units and two statics, each at spec revision 1. The World service listed every one at once after its call: `[[1,1],[4,1],[5,1]]`, then those and `[6,1],[9,1],[10,1]`. The answers' store revisions were 10 and 23. This does not show one commit; the host tests show that a refused call writes nothing, a refusal found after earlier entities were planned included |
| IDs in file order | Group 1 with units 2 and 3, statics 4 and 5; then group 6 with 7 and 8, statics 9 and 10 |
| A path outside the listed folder, the sidecar's own `sidecar.toml` | Refused: `is not a file in a folder that [missions] template_dirs lists` |
| The log | `made 1 groups and 2 statics`, keys not used: `static.groupId`, `static.heading`, `static.hidden`, `static.x`, `static.y`, `vehicle.groupId`, `vehicle.units[].playerCanDrive`, `vehicle.x`, `vehicle.y` |
| `land.getSurfaceType` at the eight places, before the calls | 1 (land), all |
| Everything removed, the file removed | No object of any name left; the file gone |
| The sidecar's exit code after a stop | 0 |

The verdict's rule is the place, the type and the country; its tolerances are this
step's, as for group templates. It is tested on the host: a missing object, a place
2 m off, a wrong type or country, the axes exchanged and a turn to the left each fail
it. This run is the first read of a static spawned from a static template's form
(LIVE). The form of a static in a file the editor saved is LIVE for the keys
docs/results/2026-11-24-template-heliports.md read; this run's file
was written by the run, after the editor's code.

## Live on Windows

`cargo xtask sidecar <name> --part static-template` on a windows-ssh target
(`crates/xtask/src/sidecar_windows.rs`, driving the same
`crates/xtask/src/shell_live/static_template.rs`) on a DCS 2.9.31.29434 client on
Windows, hosting a private session, 2026-11-24: agent build `59c32c8a9b1bb30f`, sidecar
binary (`x86_64-pc-windows-msvc`) sha256
`2e54ce242313e450185b24c5ed3d231f9113ee221f0e0c111ccd5a2e0bda227a`, tree
`v0.1.0-20-g79e3310-dirty`: the merged sidecar code, with the Windows branch of the
harness before review.

The client has no dedicated server. The run refuses when DCS already runs, starts DCS
through the profile's start task and hosts a private session with the shell mission.
The sidecar is a Windows program in the profile's `nanoprobe/dev/sidecar/`, with a
loopback listener and native paths in its `sidecar.toml`, held by an SSH session; the
API is reached through an SSH local forward. The path outside the listed folder is
that `sidecar.toml`. At the end the run stops the sidecar, closes DCS through the stop
task, removes its folders, undeploys, and fails unless the profile's top folders list
what they listed before.

The rule is the Wine run's, with one item left out. On Windows the sidecar stops
cleanly only on Ctrl-C, which a process in another SSH session cannot be sent, so the
run ends it with `Stop-Process`; "the sidecar's exit code after a stop" is not part of
the Windows verdict.

| Call | Object | Asked: north, east, heading | Read from DCS: north, east, heading |
|---|---|---|---|
| In place | M-113 | -289,700, 680,050, 0 | -289,699.999999, 680,049.999999, 0 |
| In place | M-113 | -289,690, 680,080, 1.0 | -289,689.999999, 680,079.999999, 1.0000000 |
| In place | FARP Tent | -289,760, 680,020, 0.5 | -289,760, 680,020, 0.4999998 |
| In place | FARP Fuel Depot | -289,730, 679,990, 0 | -289,730, 679,990, 0 |
| Anchor, π/2 | M-113 | -289,440, 680,060, 1.5707963 | -289,439.999986, 680,059.999996, 1.5707964 |
| Anchor, π/2 | M-113 | -289,470, 680,070, 2.5707963 | -289,469.999986, 680,069.999996, 2.5707963 |
| Anchor, π/2 | FARP Tent | -289,410, 680,000, 2.0707963 | -289,410, 680,000, 2.0707963 |
| Anchor, π/2 | FARP Fuel Depot | -289,380, 680,030, 1.5707963 | -289,380, 680,030, 1.5707963 |

| Check | Result |
|---|---|
| Each object within 1 m of its place | Yes: within 15 µm, the same reads as on Wine. No object stood where exchanged axes would put it |
| Type and country | As asked, all eight |
| Heading within 0.1 rad, recorded and not part of the verdict | Yes, all eight |
| Every entity answered and listed after its call | Yes: `[[1,1],[4,1],[5,1]]`, then those and `[6,1],[9,1],[10,1]`; store revisions 7 and 11 |
| IDs in file order | Group 1 with units 2 and 3, statics 4 and 5; then group 6 with 7 and 8, statics 9 and 10 |
| A path outside the listed folder, the sidecar's own `sidecar.toml` | Refused: `is not a file in a folder that [missions] template_dirs lists` |
| The log | `made 1 groups and 2 statics`, the same unused keys as on Wine |
| `land.getSurfaceType` at the eight places | 1 (land), all |
| Everything removed, the file removed | Yes |
| Profile after the run | The top folders as before; DCS closed |
| Verdict | PASS (LIVE) |

The table holds run 3's reads. Runs used: 3 of 3. Run 1 read eight objects at their
places but failed on the harness. Its outside-path probe was the container's
`/etc/nanoprobe/sidecar.toml`, which is not an absolute path on Windows. It also tried to
remove the mission folder while DCS still held the loaded mission open, and that removal
failed; the undeploy removed the folder after DCS closed. The harness of runs 2 and 3
asks the target for the sidecar's own config path, and removes the folders after DCS
closes. Run 2 stopped before the API: the hosted session did not start the shim within
the harness's 300 s, where run 1's session had taken two minutes after the mission
started. Run 3 waited up to 900 s, the limit the API parts give a mission load; load
time has no fixed duration. Run 3 passed.

**Changed after run 3, from the review.** The start script writes the sidecar's process
ID and exit code to files and exits 0 (a process ended by `Stop-Process` exits with -1,
which SSH reported as its own failure); the stop ends that process by its ID; the run
refuses a profile that holds a `nanoprobe` folder or a client where a sidecar runs; the
restore verdict is a function with a host test; the bundle records the harness's
SHA-256. None changes a step or a read the verdict uses: the part's driver is unchanged,
and the restore still requires every step and an unchanged listing. The payload-preset
part's Windows run 3 ran the final harness end to end.

## Runs

macOS (aarch64), the working tree of this commit, 2026-11-23.

| Command | Result |
|---|---|
| `cargo test -p nanoprobe-api --lib -- static_template` | 21 passed: the reader's and the conversion's 20, three of them property tests of 2,000 cases each, and the service test on the fake sim. |
| `cargo test -p xtask static_template` | 4 passed: the live part's file and its places, the verdict, and the check that every entity is listed after its call. |
| `mise run check` | Passes: the workspace tests, 1,459 passed and 0 failed, and every other step, `buf lint`, `tools/protobreak.sh` (no breaking change against `origin/main`) and `cargo xtask proto --check` among them. |
| `mise run dll` | Passes. |
