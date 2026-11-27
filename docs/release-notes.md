# Release notes

## 0.3.0

Tested on DCS 2.9.32.30490 (dedicated server under Wine).

### Added

- `SetNextMission` takes typed weather (`conditions`). Each field set replaces the
  template's, within the Mission Editor's ranges and cross-field checks
  ([ADR 0053](adr/0053-mission-content.md)).
- `SetNextMission` sets the sortie name, the situation and each coalition's task
  (`briefing`), in every language the template holds.
- `SetNextMission` sets each coalition's briefing images and the coalition selection
  screen's images (`briefing_images`).
- `SetMissionFile` and `ListMissionFiles` pack PNG, JPEG, Ogg Vorbis and WAV files into
  a campaign's missions: briefing images, sounds, pictures, broadcasts and kneeboard
  pages. A campaign holds at most 64 files and 64 MiB; a full campaign is refused with
  reason `FILES_FULL`.
- `DoEffect` broadcasts a mission sound file on a radio frequency in Hz, AM or FM
  (`broadcast`), and stops it by name (`broadcast_stop`).
- The release archives include `nanoprobe-fake-sidecar`: the API over a simulated DCS,
  to develop and test a controller without a DCS server. It models DCS from the
  project's results, so a controller that passes against it can still behave
  differently on a real server ([ADR 0054](adr/0054-the-fake-sidecar-ships-in-releases.md),
  [controller guide](controller-guide.md)).
- Guides: [controller-guide.md](controller-guide.md) for writing a controller,
  [operator-guide.md](operator-guide.md) for running a server, and
  [configuration.md](configuration.md) for every setting.
- An Agent Skill in `skills/nanoprobe/` points AI coding tools at the guides and the
  schema. Its README says where each tool looks for it.

### Changed

- **The campaign store is schema 3.** A sidecar migrates an older store when it opens
  it, after taking a `pre_migration` backup. An older sidecar cannot open a migrated
  store: to go back, restore the `pre_migration` backup, which loses what the campaign
  did since. Back the store up before upgrading if you may roll back.
- **The `weather` Struct in `NextMission` is deprecated**; set `conditions`. Setting
  both is `INVALID_FIELD`, and a `weather` table without `clouds` or `season` is
  refused.
- The README is rewritten, with example messages, what nanoprobe adds to DCS,
  performance figures and limitations.
- The configuration reference says `sidecar.toml`'s `[agent] command_queue`,
  `[log] dir` and `[log] level` are accepted and have no effect.
- The `dated_time` field's documentation gives the time a late kill can arrive as up to
  103 s.

## 0.2.0

### Added

- `PutFromStaticTemplate` reads a Mission Editor static template (`.stm`) from a folder
  `[missions] template_dirs` lists and makes every vehicle group and static in it in one
  store commit or none. An anchor and a heading place and turn the layout. Files with
  plane, helicopter, ship or complex groups, linked statics or grass airfields are
  refused
  ([ADR 0003](adr/0003-static-template-spawn-in-one-commit.md),
  [2026-11-23-static-templates.md](results/2026-11-23-static-templates.md)).
- A `PutGroup` unit names a Mission Editor payload preset in `payload_preset`. The
  sidecar reads the preset file from `template_dirs` as data and stores its pylons. A
  file with code other than the data table and its `return` is refused, and so is a
  preset together with `pylons`
  ([ADR 0004](adr/0004-payload-presets-read-at-request-time.md),
  [2026-11-24-payload-presets.md](results/2026-11-24-payload-presets.md)).
- `PutStatic` with a `static_id` revises the static's spec in place, and the sidecar
  spawns it again under its name. A heliport group grows and shrinks as one airbase, and
  the old airbase's owner and stock carry over. A heliport type change is refused with
  `FAILED_PRECONDITION`
  ([ADR 0047](adr/0047-a-statics-spec-is-revised-in-place.md)).
- Marks and drawings carry a label, unique among the live ones, and belong to a markup
  group with visibility, expiry and a cap: `PutMarkupGroup`, `ReplaceMarkupGroup`,
  `RemoveMarkupGroup` and `ListMarkupGroups`. A put with ID 0 and a held label changes
  that item. `ListWorld` and `WatchWorld` filter by markup group. The sidecar chooses
  every DCS mark ID from one pool; a full pool refuses an add with `RESOURCE_EXHAUSTED`
  and reason `DCS_IDS_FULL`. `Mark` and `Drawing` carry `dcs_id`, and
  `WorldEvent.platform_mark` tells a controller which mark events the platform caused.
  A persisted draw order survives a mission load or a restart
  ([ADR 0048](adr/0048-labels-and-markup-groups-for-marks-and-drawings.md)).
- `RemovePlayerMark` removes a mark the platform did not issue, named by the sequence
  number of the mark event that carries its ID. Mark events carry `WorldEvent.mark_id`.
- `Stamp` has an `agent_boot` field: opaque, compared for equality only, and 0 when no
  boot is known. The boot and the mission generation together name one mission run
  ([ADR 0050](adr/0050-stamps-carry-the-agent-boot.md)).
- `PutFromStaticTemplate` builds a heliport static, such as a FARP or a helipad, as
  `PutStatic` does: DCS makes its airbase, and the platform samples its warehouse
  ([ADR 0044](adr/0044-static-template-spawn-with-heliports.md)).
- A FARP's radio: `heliport_frequency`, `heliport_modulation` and `heliport_callsign_id`
  in `StaticSpec.extra` set its frequency, modulation and callsign, and
  `PutFromStaticTemplate` passes them from the file
  ([2026-11-25-farp-fixes.md](results/2026-11-25-farp-fixes.md)).
- Heliport groups: `StaticSpec.further_units` lists further heliports, and DCS makes one
  airbase of the static and them, with one warehouse and every unit's parking.
  `PutFromStaticTemplate` builds a static group whose every unit is a heliport the same
  way, and answers it as one static
  ([ADR 0046](adr/0046-heliport-groups-spawn-as-one-airbase.md)).
- Typed drawings: `PutDrawingRequest.typed_shape`, `MarkupGroupDrawing.typed_shape` and
  `Drawing.typed_shape` take a `DrawingShape`: a line, polygon, arrow, text box, quad,
  freeform, circle, rectangle at any heading, oval or rounded rectangle, with colors, a
  `LineType`, a fill and a font size. The sidecar draws an oval or a rounded rectangle as
  a freeform with a point every 10 degrees. A circle's radius changes in place
  ([ADR 0052](adr/0052-typed-drawing-shapes.md), spec/world.md, "Drawings").
- A `WorldFilter` with a center and a radius keeps the drawings it meets.

### Changed

- **The store file is schema 2.** The sidecar takes a `pre_migration` backup and migrates
  a 0.1.0 store on its first open. A 0.1.0 sidecar refuses the migrated file, and there
  is no downgrade: to go back, restore the `pre_migration` backup, which loses what the
  campaign did since. Back the store up before upgrading if you may roll back
  ([ADR 0048](adr/0048-labels-and-markup-groups-for-marks-and-drawings.md),
  `spec/persistence.md`).
- `PutDrawing` and the agent accept only the `markupToAll` shapes 1, 3, 4, 5, 6 and 7
  and the `circleToAll` circle, with their known argument layouts, and refuse anything
  else before DCS is called.
- `PutStatic` and `PutFromStaticTemplate` refuse a `units` key in a static's `extra`
  with `INVALID_FIELD`. List a heliport group's further units in
  `StaticSpec.further_units`.
- **`MovePlayer` keeps the player's coalition when `coalition` is
  `COALITION_UNSPECIFIED`.** In 0.1.0 it moved the player to spectators. With no `slot`,
  the player leaves their slot and stays on their side; with a `slot`, they go to that
  slot on its own coalition. Use `COALITION_NEUTRAL` to move a player to spectators.
  This changes `v1alpha1` behavior a 0.1.0 controller may rely on.
- **`MovePlayer` moves a player into a slot.** `slot` is the slot's unit ID, as the
  mission lists it. `COALITION_RED` or `COALITION_BLUE` with a slot of the other
  coalition is refused, and `COALITION_NEUTRAL` with a slot is `INVALID_ARGUMENT`.
- The agent refuses these moves before DCS sees them, with outcome `REFUSED` and one of
  these messages: `the mission lists no slot with this ID`, `the slot belongs to the
  other coalition`, `another player holds the slot`, `a crew seat is not a slot the
  agent moves players into`, `DCS gave no slot list`, `DCS gave no player list`, `DCS
  gave no slot for another player` and `DCS gave no side for the player`. DCS itself
  accepts a slot ID the mission does not have and leaves the player in a slot with no
  aircraft.
- The transport's `layout_version` is 2. A 0.1.0 agent and a sidecar of this version
  refuse each other; install both from the same release.
- **`PutDrawingRequest.shape` and `args` are deprecated**; set `typed_shape`. A put sets
  one form, not both and not neither. A drawing put through them reads back with its
  `typed_shape` and an empty theater, and passes no area filter.
- **The 0.1.0 drawing fields are checked more tightly.** A put through `shape` and `args`
  is `INVALID_ARGUMENT` where 0.1.0 took it when a color or fill part is not from 0 to 1,
  the line type is not a whole number from 0 to 6, a text box's font size is not a whole
  number from 1 to 100, a point is more than 10,000 km from 0, or a polygon or freeform
  has more than 7,990 points. The agent refuses the same colors, line types, font sizes
  and point counts. DCS was never checked with those values, except line type 7, which
  DCS draws solid.
- A drawing put through `shape` and `args` keeps no `y`: it is sent and read back with
  `y` 0, and DCS fills in the ground height.
- `Drawing.shape` and `args` read as the `markupToAll` call a drawing is sent as: an oval
  reads as shape 7 and its points, and a circle as 0 and an empty list.

### Fixed

- **A drawing can no longer crash the server.** 0.1.0 passed any `markupToAll` shape
  number and arguments a controller sent straight to DCS, and an unknown shape, such as
  8, crashes and hangs the DCS server. Upgrade if a controller you do not control can
  put drawings.
- A group or static revised and not yet spawned before a sidecar restart is spawned in
  place after it. A group lost to failed destroys waits for a full enumeration that began
  after the loss before the respawn policy acts, and a member a script removed with no
  record is not respawned in that mission generation
  ([ADR 0049](adr/0049-confirmed-revisions-and-lost-groups-after-a-restart.md)).
- A FARP the platform spawns, by either call, is `LIVE` in the World, and
  `RemoveEntity` removes it with its airbase. On a paused server DCS removes it when the
  mission resumes ([2026-11-25-farp-fixes.md](results/2026-11-25-farp-fixes.md)).
- A group's order that DCS refused because it held none of the group is sent again
  once DCS reports one of its members, even a member no record said had left.
  UNVERIFIED: whether DCS can hold such a member again; the fake sim models it as a
  script's removal and revival
  ([ADR 0051](adr/0051-an-order-refused-not-bound-waits-for-any-member.md)).

## 0.1.0, the first release

The first release of nanoprobe, 0.1: an agent inside DCS World, a sidecar that owns a
persistent campaign world, and a gRPC API for a controller in any language. The
[README](../README.md) says what the platform tracks and how fresh it is.

### Files

| File | Holds |
|---|---|
| `nanoprobe-<version>-windows-x86_64.zip` | `nanoprobe.exe`, `nanoprobe-sidecar.exe`, `install.cmd`, `uninstall.cmd`, `README.txt`, an example `sidecar.toml`, and the agent |
| `nanoprobe-<version>-linux-x86_64.tar.gz` | static `nanoprobe` and `nanoprobe-sidecar`, `install.sh`, `uninstall.sh`, `README.txt`, `deploy/` for the sidecar's container, and the agent |
| `nanoprobe-<version>-schema.tar.gz`, `nanoprobe-<version>-schema.binpb` | the API schema `nanoprobe.v1alpha1` and its descriptor set |
| `SHA256SUMS` | checksums of the four files |

No client package and no container image is published. Clients generate code from the
schema ([api-clients.md](api-clients.md)); the sidecar's image is built from the Linux
archive ([deploy/README.md](../deploy/README.md)). The files' names carry the workspace
version, `0.1.0`: `nanoprobe-0.1.0-windows-x86_64.zip`, for example.

### How the files were built

The files are built on one Linux runner of GitHub Actions with `mise run package`: the
Windows programs and the agent DLL cross-built with `cargo-xwin`, the Linux programs as
static musl binaries. The acceptance run tested files the same workflow built without
releasing them, from commit `00642c4`. The tagged commit differs from it in one place the
build reads: the sidecar opens a campaign's events file for a backup with an immediate
transaction, so a backup taken while events are written waits for the write lock
instead of failing with "database is locked". The agent is the same code. The published
files are the build repeated for the tag, so their bytes, and the checksums in
`SHA256SUMS`, differ from the tested ones; the MSVC CRT and Windows SDK that
`cargo-xwin` downloads for each build may differ too.

The agent's PDB is not published. The project keeps it to turn the agent's crash
records into function names.

### Where it is verified

From the acceptance run ([acceptance.md](acceptance.md)):

| Platform | Result |
|---|---|
| DCS dedicated server under Wine on Linux | criteria 3 to 6 pass; criterion 1's owner targets hold, with open items ([2026-11-22-s22-acceptance-wine.md](results/2026-11-22-s22-acceptance-wine.md)) |
| DCS on Windows, hosting a session from the client | install, `doctor`, the sidecar and its kill, and uninstall pass; criterion 1's owner targets hold, with an open item; the hello campaigns and the controller and DCS kills are not shown ([2026-11-22-s22-acceptance-windows.md](results/2026-11-22-s22-acceptance-windows.md)) |
| DCS dedicated server on Windows | unverified: no test has run on one |

The soak runs on the Wine server only.

### Know before you install

- `nanoprobe uninstall` removes the whole `nanoprobe` folder in the DCS profile,
  `nanoprobe/missions/` included: a mission folder kept there, with the templates you
  saved, goes with it. To keep the per-map templates, make the mission folder a folder
  in the profile outside `nanoprobe/`, or copy them out first; keep other template
  files in a folder listed in the sidecar's `[missions] template_dirs`
  ([install.md](install.md), "Uninstall").
- The API is `v1alpha1` and can change in a way that breaks clients.
- The API has no TLS. A listener off loopback needs a tokens file and a trusted network.
- The project ships no mission. Save a template for each map in the Mission Editor
  ([mission-templates.md](mission-templates.md)).
- The platform does not start or restart DCS. It reports DCS as healthy, hung or gone.
- The sidecar exits when DCS restarts. Run it under something that starts it again: a
  service manager, or a Windows service or scheduled task. The compose overlay has a
  restart policy meant for that; that it brings the sidecar back after the DCS container
  restarts is not yet verified.
- The sidecar writes its log to standard output and standard error, not to a file.
- Fuel is not tracked, and ammunition is known only for units players occupy.
- The platform cannot unpause DCS. A dedicated server that pauses while nobody is
  connected stays paused, and a campaign's groups do not move until a player joins or
  the server is set not to pause.
- Static templates and payload presets are not yet supported.
- The released agent differs from the one the 8-hour soak ran by one fix: after an
  attach, a row's `VITALS` record is no longer written before its `OBJECT` record.

### After a DCS update

Run `nanoprobe doctor`. It passes, or it names each DCS function the platform calls and
no longer finds. The conformance suite, which checks the agent's behavior against the
specification on a running DCS, is not in the release: it runs from a source checkout
with the development tooling ([acceptance.md](acceptance.md), criterion 6). The
project's recorded run passed 9 of 9 on 2026-11-22, on DCS 2.9.31.29434 under Wine, with
the agent of the tested files (LIVE,
[2026-11-22-s22-acceptance-wine.md](results/2026-11-22-s22-acceptance-wine.md)). The
suite must pass in full when `doctor` reports every name present (Open items says what
happens when one is absent).

### Open items

- The conformance suite has no table from each check to the DCS names it needs. When
  `doctor` reports a name absent, a person reads the suite's result.
- At 2,000 objects the agent's pump rate at k = 1 goes past the sampling tool's bound
  in some windows. The overhead targets hold
  ([2026-11-22-s22-acceptance-wine.md](results/2026-11-22-s22-acceptance-wine.md)).
- The status block's achieved position period is the length of the last sweep pass,
  so one slow pass moves it; it is not an average.
