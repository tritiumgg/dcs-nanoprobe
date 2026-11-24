# ADR 0005: Spawn a group from a group-template file

## Status

Accepted

## Context

The Mission Editor saves a group's layout as a group template: unit types, each unit's
offset and heading, and a country. DCS ships its templates in one file,
`MissionEditor/data/scripts/templates.lua`: 51 templates in one assignment of literal
values, every one of type `"vehicle"` (ED-CODE,
[the group-template results](../results/2026-11-18-spawn-template.md)). A controller
that wants a convoy or a battery should not compute each unit's place.

The editor saves a unit's `dx` and `dy` as its mission `x` and `y` less the group's,
and turns a group by the same rotation (ED-CODE, same document). A unit table's `x`
and `y` are the world's `x` and `z` (LIVE,
[the persistence results](../results/2026-11-15-s18-persistence-live.md)). Where the
editor saves a user's own templates, and that the file has the shipped form, is
UNVERIFIED.

The file lives on the administrator's side. The API names it by path under the
folder allow-list of [ADR 0006](0006-templates-are-files-copied-at-set-time.md).

## Decision

`PutGroupFromTemplate` reads one named template from a file under
`[missions] template_dirs` and creates one ground group from it.

- The call creates only. It has the role, lease and idempotency rules of `PutGroup`,
  and a change goes through `PutGroup`. The request carries `path`, `template`,
  `anchor`, `heading`, an optional `country`, a `label` and an optional first `order`.
- The store keeps the converted `GroupSpec` alone. A restore, a reload, a restart and
  an export never read the file. The path and the template's name go to the sidecar's
  standard error. No field records them.
- The file is read once, as data, by the reader that reads a mission template's
  members, with the root `templates` and a 1 MiB cap. No code from the file runs.
  Anything but one assignment of plain values refuses the file with its line.
- `anchor` is required. Each unit goes `dx` meters north and `dy` meters east of it,
  on the anchor's theater and at its altitude. `heading`, in radians clockwise from
  north, turns the layout about the anchor and is added to each unit's heading. At 0
  every number is the file's own.
- `country` is optional because 0 is a country. It replaces the template's country.
  A country the mission does not hold shows as DCS's refusal in the group's last error.
- A template whose `type` is not `"vehicle"` is refused. The shipped file has no other
  value, and the values the editor saves for other groups are UNVERIFIED.
- The unit's DCS type is the template unit's `name`. Names in the file are not used.
  An empty `label` takes the template's key. The sidecar drops other keys and names
  them in its standard error.
- Every refusal comes before any store write. Path refusals name the field `path`; a
  missing template, a template with no units, and a bad `dx`, `dy` or `heading` name
  `template`.

Spec: [world](../../spec/world.md), "Group templates". Administrator steps:
[mission templates](../mission-templates.md).

Rejected:

- A convert-only call that returns the converted spec. It would let a controller read
  any table file under the listed folders.
- Reading the file again at restore or reload. The store would then depend on a file
  that can change or vanish.

## Consequences

- A unit made by the call stood on DCS within 3 mm of the requested place, with no
  turn and with a quarter turn (LIVE, DCS 2.9.31.29434, dedicated server under Wine
  10.0, same results document).
- A repeat under an answered idempotency key returns the first answer, whatever has
  become of the file.
- Static templates ([ADR 0003](0003-static-template-spawn-in-one-commit.md)) and
  payload presets ([ADR 0004](0004-payload-presets-read-at-request-time.md)) reuse the
  reader and the path rules.
- A file the editor saved for a user has not been read. One in another form is
  refused.
