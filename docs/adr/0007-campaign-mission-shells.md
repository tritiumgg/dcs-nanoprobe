# ADR 0007: Campaign mission shells: the missions folder pair and two shell slots

## Status

Accepted

## Context

A campaign needs a mission for DCS to run. The sidecar writes it from a template saved
in the Mission Editor ([ADR 0006](0006-templates-are-files-copied-at-set-time.md)) and
loads it with a `MISSION_LOAD` command that carries a path as DCS opens it.

The sidecar and DCS can see one folder under different paths, such as a native sidecar
beside DCS under Wine. A path the sidecar can check is not a path DCS can open. DCS may
still hold the last mission file it loaded (UNVERIFIED), so the sidecar must not
rewrite that file.
A campaign may name its own template for a map, in place of the shared one.

`net.load_mission` loads `shells\<id>.<slot>.miz` under Wine (LIVE, DCS 2.9.31.29434
dedicated server under Wine 10.0,
[the shell and reload results](../results/2026-11-15-s19-shell-reload-health.md)).

## Decision

The sidecar writes a campaign's shell into a folder named once per side, in one of two
slots, and rewrites only the runtime fields of the template.

- `[missions] dir` is the folder as the sidecar opens it. `[missions] dcs_dir` is the
  same folder as DCS opens it. Both lie inside the Saved Games profile. The shared
  template of a theater is `<dir>/<theater>.miz`, the theater's name as `LIFECYCLE`
  carries it, with no case change. `MISSION_LOAD` carries `dcs_dir` joined to the
  shell's file name.
- A campaign has two slots, `shells/<campaign ID>.a.miz` and `.b.miz`. A write goes to
  the slot that the owed load, else the last applied load, did not name. It goes
  through a temporary file that is flushed and renamed. A load whose outcome is
  unknown, or that a suspend dropped after it was sent, still holds its slot. The
  sidecar removes stale temporary files at start.
- The shell is a copy of the template with these values changed: `date` and
  `start_time`, `weather`, and the dynamic-spawn and hot-start flags of the airports
  that `SetNextMission` names, keyed by airport ID, and, for an airport with dynamic
  spawn on, `unlimitedAircrafts` false and an empty `aircrafts` list when it has none.
  Every other byte is kept. Airbase owners and warehouse stocks are set after the
  mission starts.
- The campaign's own template, when it has one, replaces the shared one for its
  theater. Units placed in a template stay unmanaged.
- A theater with no template, a template that does not parse, a shell that does not
  render, and an airport the template lacks refuse the call before anything is created,
  suspended, opened or sent. A shell that cannot be saved refuses after the active
  campaign's groups are captured. Nothing is suspended, opened or sent.
- The sidecar refuses to start when `dcs_dir` joined to `shells/`, 20 digits and
  `.a.miz` is over 1,024 bytes, the agent's path limit.
- `Reload` captures every group DCS holds, writes the shell, and commits a checkpoint
  and the owed load together. It answers `PENDING`. The load completes later, as an
  activation's does. The campaign clock takes a load's `start` when the load is
  `APPLIED`, so a refused load leaves the clock where it was.

Spec: [persistence](../../spec/persistence.md), "Mission shells" and "Campaign
operations"; [config](../../spec/config.md), `[missions]`.

Rejected:

- A table of paths as DCS opens them. A native sidecar cannot check a Wine path, and
  the sidecar needs a folder it can write.
- One shell file per campaign, rewritten in place. DCS may hold the file it loaded.

## Consequences

- Two files per campaign stay in `shells/` until the campaign is deleted.
- A reload on a theater change leaves entities of a theater that is not loaded
  `virtual`. They return when it is loaded again.
- After a DCS restart into a mission the campaign did not load, the sidecar does not
  reload the shell. The clock stays at the value it reached until the campaign's next
  load.
