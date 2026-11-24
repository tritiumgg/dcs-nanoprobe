# ADR 0006: Administrator templates are files by path, copied at set time

## Status

Accepted

## Context

The sidecar writes each campaign's mission from a template: a `.miz` saved in the
Mission Editor. Each map needs one. A campaign may want its own for a map. Group,
static and payload-preset files are templates of the same kind of source: files an
administrator saves in the editor.

The sidecar and the DCS server can run on different sides of a boundary, such as a
container or Wine. The path DCS has for a file is not the path the sidecar has. A
file that a controller can name by any path is a file the sidecar will read for it.

## Decision

The administrator provides every template as a file. The API names a file by its path
as the sidecar opens it, and the sidecar copies a campaign's template into the
campaign's store when the call is made.

- The project ships no `.miz`. The administrator saves one per map, as
  `<theater>.miz`, in the folder that `[missions] dir` names. `nanoprobe doctor`
  reports a missing one.
- `SetCampaignTemplate` takes `theater` and `path`. An empty path removes the
  campaign's template. The call reads the file once, checks it, and writes its bytes
  to the campaign's row of the `template` table in one store commit.
- The campaign uses the copy from then on. An activation, a reload, a backup, a
  restore, an export and a restarted sidecar use the row and never open the path.
  A change to the file takes effect when the controller sets the template again.
- A path is allowed only if it is absolute and resolves, after symbolic links and
  `..`, to a file inside a folder that `[missions] template_dirs` lists. An empty
  list, the default, refuses every path. `[missions] dir` is allowed only if listed.
  A missing file, a folder, a path outside the listed folders and an empty list get the
  same refusal text.
- Template conversion lives in the sidecar. Calls that read group, static and
  payload-preset files take paths under the same allow-list
  ([ADR 0005](0005-spawn-group-from-template-file.md)).
- The copy lives in a row, not in a file beside the store. A write that fails changes
  nothing, and backup, restore and export carry the row with no further code.

Spec: [persistence](../../spec/persistence.md), "Set a template";
[config](../../spec/config.md), `[missions] template_dirs`.

Rejected:

- Upload of the file's bytes through the API. The administrator provides the files,
  so the API names them.
- Reading the file at each activation or reload. An edited or deleted file would
  change a campaign without a call, and a backup would not carry its template.

## Consequences

- Only the administrator should be able to write in a listed folder: the sidecar
  reads any file there that a controller names.
- A file can be moved or deleted after the call. A repeat of an answered call returns
  its first answer.
- A call is checked once: the `theatre` member, the members the shell writer changes,
  no platform-format names, free space, and a 16 MiB cap.
