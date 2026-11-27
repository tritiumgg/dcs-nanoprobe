# ADR 0053: Mission content: typed weather, briefings and files

## Status

Accepted.

## Context

A campaign's mission is a shell the sidecar writes from a template before each load
(spec/persistence.md, "Mission shells"). Controllers need to change more of it than the
date, time and weather: each coalition's briefing, the sortie name and description,
briefing images, the coalition selection screen's image, sounds the template's
triggers play, kneeboard pages, and files a controller plays or shows at run time. The weather was an untyped `Struct` that replaced
the template's whole table, unchecked.

What a mission holds (ED-CODE, DCS 2.9.31.29434, `MissionEditor/modules/me_mission.lua`,
`me_weather.lua`, `Scripts/dictionary.lua`, `Config/Effects/clouds.lua`):

- The weather is one table with fixed keys, metric units and the editor's ranges, and
  cross-field rules: a preset fixes the cloud base range, precipitation needs density and
  a temperature side.
- The briefing texts and the sortie name are `DictKey_` keys into a dictionary per
  locale; images and sounds are `ResKey_` keys in `mapResource`, numbered with the
  dictionary keys under `maxDictId`.
- A trigger plays a sound through its key, so replacing the file under the same name
  changes what it plays without touching the trigger.
- No Lua the editor or the sim ships changes a briefing after the load.

## Decision

- **Typed weather** (`NextMission.conditions`) merges over the template's table: only the
  fields set change, every cross-field rule is checked on the merged table, and any field
  makes the weather static. The `Struct` stays, deprecated, and must hold `clouds` and
  `season`; setting both forms is refused.
- **Briefing text** goes into every locale dictionary the template holds, under the key
  the template's field names, or a new `DictKey_` above `maxDictId`. No key is removed
  and no plain text is written into `mission`.
- **Files** are named by path under `[missions] template_dirs` and copied into the
  campaign store at set time, as templates are ([ADR 0006](0006-templates-are-files-copied-at-set-time.md)).
  Each is parsed past its magic bytes and bounded: 8 MiB, 64 files and 64 MiB per
  campaign. A file named as one the template holds replaces it, in every locale folder;
  a new one gets a `ResKey_np_` key.
- **Everything takes effect at the next load.** `SetNextMission` renders the shell when
  the template is at hand and refuses what would not render.
- **A radio broadcast is an effect**: the `Effect` cases `broadcast` and
  `broadcast_stop`, sent with `DoEffect` as `sound` and `picture` are, carried by the
  opcodes `BROADCAST` (69) and `BROADCAST_STOP` (70). DCS plays the file only by its
  path in the mission (LIVE, `docs/results/2026-11-26-mission-content.md`, run 5), so the
  sidecar sends a file name as that path; that a client hears a broadcast sent so is
  UNVERIFIED. That a load ends a broadcast is UNVERIFIED; the platform does not start
  it again.

## Consequences

- A controller changes one weather field without knowing the rest; the template author
  keeps control of everything else.
- A template with dynamic weather loses it once a typed field is set.
- Every shell carries the campaign's files: a shell can be 64 MiB larger than its
  template.
- The schema is 3: a sidecar of schema 2 refuses a migrated store.
