# Per-map mission templates

The sidecar writes each campaign's mission from a template: an ordinary mission saved
in the DCS Mission Editor. The server's administrator provides one per map that a
campaign uses. nanoprobe ships none.

A template lives in the mission folder that `[missions] dir` names (spec/config.md),
as `<theater>.miz`. A campaign with neither this template nor its own for its map
("A campaign's own template") cannot be activated or reloaded there.

## Save a template

1. Start DCS with the map installed and open the Mission Editor.
2. Create a new mission on the map. Give it the coalitions, date, start time and
   weather a campaign should start from when it sets none of its own.
3. Optionally set an airport's dynamic spawn and hot start in its warehouse. A campaign
   can change both per mission. The sidecar writes them as `dynamicSpawn` and
   `allowHotStart` of the airport in the `warehouses` member (LIVE,
   docs/results/2026-11-15-s19-shell-reload-health.md).
4. Add nothing else unless every campaign on the map should have it. Units placed in a
   template are left to DCS, and the platform never manages them. Do not give a group,
   unit or static a name in the platform's own format, `np` followed by `!`, `.` or
   `#` (spec/world.md, "Names").
5. Save the mission as `<theater>.miz` in the mission folder. The name is the
   theater's name as DCS writes it in the mission, matching case, such as
   `Caucasus.miz` or `Afghanistan.miz`.

A `.miz` file is a ZIP archive. Its `theatre` member holds the theater's name (LIVE: the
files the Mission Editor saved in `tests/fixtures/templates/`); the file name must be that name followed by `.miz`. A theater name is 1 to 64 characters
of `A`-`Z`, `a`-`z`, `0`-`9`, `_` and `-`.

## What the sidecar changes

Each campaign's mission is a copy of the template with only these values changed: the
date and start time, the weather, and each named airport's dynamic spawn and hot start
(spec/persistence.md, "Mission shells"). An airport with dynamic spawn turned on gets a
limited aircraft stock, which the controller sets with `SetWarehouseStock`. Airbase
owners and warehouse stocks are set after the mission starts. Every other part of the
template is kept.

## A campaign's own template

A campaign can carry its own template for a map, which it uses in place of the shared
one. The administrator saves the mission in a folder that `[missions] template_dirs`
lists in `sidecar.toml` (spec/config.md); no folder is listed by default, so the list
decides which files a controller can name. The controller calls `SetCampaignTemplate`
with the file's path as the sidecar opens it. For a sidecar in a container that is the
path inside the container (deploy/sidecar.toml). For a native sidecar beside DCS under
Wine it is the Linux path. It is never the path DCS has for the file.

The sidecar checks the file as it checks a shared template, with one more rule: no
group, unit or static in it has a name in the platform's own format. It then copies the
file into the campaign's store. The campaign uses the copy from then on:

- A reload, an activation and a restart of the sidecar never read the file again.
- A backup, a restore and an export carry the copy.
- An edit of the file takes effect when the controller sets the template again.
- The file can be moved or deleted after the call.

A call with no path removes the campaign's template, and the campaign uses the shared
one again.

## Group templates

The Mission Editor saves a group's layout as a group template: the unit types, each
unit's offset and heading, and a country. DCS ships its own in one file,
`MissionEditor/data/scripts/templates.lua` in its install (ED-CODE,
docs/results/2026-11-18-spawn-template.md). Where the editor saves a user's own, and
that the file has the same form, is UNVERIFIED; look for a `templates.lua` under
`MissionEditor` in the Saved Games profile.

A controller makes a group from one with `PutGroupFromTemplate`:

1. The administrator copies the file into a folder that `[missions] template_dirs`
   lists, as for a campaign's own template. Nothing in the DCS install is changed.
2. The controller names the file's path as the sidecar opens it, the template's name
   in the file, and an anchor: the point on the map where the template's origin goes.
3. Optionally it gives a heading, which turns the whole layout about the anchor; a
   country, which replaces the template's own; and a label.

The sidecar reads the file once and keeps the group it made. The file can be changed,
moved or deleted after the call, and the group is changed afterwards with `PutGroup`.
Only ground groups are built: a template whose `type` is not `"vehicle"` is refused.
The file is read as data and never run. A file over 1 MiB, or one that holds anything
but the one `templates = {...}` table of plain values, is refused
(spec/world.md, "Group templates").

## Static templates

The Mission Editor saves the objects of the open mission as a static template, a
`.stm` file in `StaticTemplate/` of the Saved Games profile. DCS ships its own in
`MissionEditor/data/scripts/StaticTemplate/` in its install (ED-CODE,
docs/results/2026-11-23-static-templates.md). A static template holds absolute places
on one map, and the editor loads it where it was saved.

A controller makes every ground group and static of one with `PutFromStaticTemplate`:

1. The administrator copies the file into a folder that `[missions] template_dirs`
   lists. Nothing in the DCS install is changed.
2. The controller names the file's path as the sidecar opens it.
3. Without an anchor, every object goes where the file has it, on the file's map, which
   must be the campaign's. With an anchor, the center of the layout goes to the
   anchor, on its map.
4. Optionally it gives a heading, which turns the whole layout about its center; a
   country, which replaces the file's countries; and a label, which is put in front of
   each object's name from the file.

The call makes all of the file's objects or none. The answer lists the groups, each
with its units, and the statics, in the order the file holds them. The sidecar keeps
what it made; the file can be changed, moved or deleted after the call. Give a group
its orders afterwards with `SetOrder`: the routes and tasks in the file are not used.

Only ground groups and statics are built. A file that holds planes, helicopters, ships,
a complex, a static linked to a unit, a heliport or a grass airfield is refused whole.
While the mission runs, the objects spawn on DCS at the platform's usual pace, by
default one at a time. While it is paused, they spawn together in the restore burst
(spec/persistence.md, "Restore burst"). Each shows its own error if DCS refuses it
(spec/world.md, "Static templates").

## Check the templates

`nanoprobe doctor` checks every `<theater>.miz` in the mission folder. It fails when the
folder holds none, when a template is one the sidecar cannot use, and when a map named
with `--theater` has none. Without `--missions` or `--theater`, a profile with no
`nanoprobe/missions` folder gets only a note. Name the folder with
`--missions DIR` when it is not `nanoprobe/missions` in the profile, and each map your
campaigns use with `--theater NAME`:

```sh
nanoprobe doctor --missions /path/to/missions --theater Caucasus --theater Afghanistan
```
