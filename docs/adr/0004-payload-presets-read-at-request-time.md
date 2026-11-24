# ADR 0004: Payload presets are read at request time, never stored

## Status

Accepted

## Context

An aircraft's loadout is a list of pylons, each with a `CLSID`. The Mission Editor
keeps named loadouts per aircraft type as payload preset files. DCS ships 153 of
them, and the editor merges a user's presets over them by name (ED-CODE,
[the payload-preset results](../results/2026-11-24-payload-presets.md)). A controller
should name a preset, not copy its pylons by hand.

The files are Lua code. All 153 end with `return unitPayloads`. 140 start with
`local unitPayloads =`, 7 with another `local` statement and 6 with a blank line or a
comment. Five hold lines with `function`, `require` or `..` (ED-CODE). A preset gives
pylons only. Fuel, flares, chaff and gun are set apart from it.

The sidecar has no access to the editor's merged table. Whether the server's GUI state
fills it is UNVERIFIED. The sidecar reads files from the administrator's folders, as
for [group templates](0005-spawn-group-from-template-file.md).

## Decision

A `PutGroup` unit names a preset file and a preset in `payload_preset`; the sidecar
reads the file at the request, converts the preset into `extra.payload.pylons`, and
never stores the field.

- `UnitSpec.payload_preset` is a message of `path` and `name`. It is a request field:
  a spec read back from the World service or a capture has none. Every other call
  that takes a spec refuses a set `payload_preset`.
- The file must lie in a folder that `[missions] template_dirs` lists. One request
  names at most 16 distinct files, each read once, under the template path rules.
- A separate reader entry point takes exactly `local N = <value>` then `return N`, with
  only white space and comments around them, and returns the same parse tree as the
  data reader. A file with other code is refused whole, with the line of the first
  statement that is not data.
- The preset's `unitType`, else `name`, must equal the unit's `type` exactly. The
  preset is picked by `name`, never `displayName`. Two presets with one `name` refuse
  the request.
- The converted spec equals one written by hand in `extra.payload.pylons` and runs the
  same checks. A preset together with `extra.payload.pylons` is refused.
- `settings` of a pylon are copied. `tasks`, `displayName` and unknown keys are dropped
  and logged.
- The sidecar reads one named file and never merges the editor's folders. It never
  lists a folder of the DCS install.

Spec: [world](../../spec/world.md), "Payload presets".

Rejected:

- A new agent opcode or an agent-side reader: it needs a new query and opcode, depends
  on the GUI state's merged table (UNVERIFIED), and filling that table would run every
  preset file on the sim thread.
- Storing the preset name with the unit: as with templates, the store keeps no
  provenance. The sidecar's standard error names the files and presets.
- Running the file as Lua: no code from a file runs in the sidecar.

## Consequences

- A repeat under an answered idempotency key returns the first answer, whatever has
  become of the file. A replace that names a preset reads the file as it is then.
- A shipped file that holds code is refused. The administrator saves the preset from
  the editor, or the controller writes the pylons by hand. How many shipped files the
  reader takes whole is UNVERIFIED. Whether the editor saves a user's preset in the
  data form is UNVERIFIED. The folder the editor writes user presets to is UNVERIFIED.
- A pylons-only spawn started with 45 % of the internal fuel (LIVE, F-4E-45MC,
  DCS 2.9.31.29434, dedicated server under Wine 10.0). The controller supplies
  `extra.payload.fuel` in kilograms.
- Two aircraft, one from a preset with `settings` and one with the same pylons by
  hand, read the same ammunition and stored the same spec (LIVE, same document). DCS
  accepted `settings`. Whether it applies fuze settings is UNVERIFIED, because the
  scripting API reads no fuze.
- A controller that sends a read-back spec again with a preset removes its pylons
  first.
