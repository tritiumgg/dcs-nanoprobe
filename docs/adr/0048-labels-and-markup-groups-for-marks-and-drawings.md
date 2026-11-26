# ADR 0048: Labels and markup groups for marks and drawings

## Status

Accepted.

## Context

A controller that keeps long-lived objective drawings, a front line in many segments,
campaign sectors and short-lived player-triggered drawings needs to:

- find an item by its own key;
- show and hide sets of items;
- replace a set at once;
- let items expire while the controller is down;
- know which drawing is drawn on top.

Without platform support, the controller keeps its own map of entity IDs and hides an
item by removing it and creating it again under a new ID.

Before this change, the shim gave each mark and drawing a DCS mark ID from its own counter
starting at 2^30. A replacement removed the old ID and added a new one, so an update
moved the drawing to the end of DCS's list. The order after a mission load followed
`EntityId`. Marks and drawings were not served by `ListWorld` or `WatchWorld`, and no
marker was kept in the store file, so a sidecar restart lost them.

What DCS does (LIVE, DCS 2.9.31.29434 on the server under Wine,
[the drawing-layers results](../results/2026-11-25-drawing-layers.md)):

- `world.getMarkPanels` lists markup in ascending ID order, whatever the creation order.
- `markupToAll` under an ID DCS holds changes nothing a server can read and emits no
  event. This was tested for drawings only; for marks it is UNVERIFIED.
- An add under an ID removed earlier in the process, for a drawing or a mark, shows no
  panel and emits no event, at once or after real time. It lasts until the next mission
  load. An add under a never-used ID works. A mission load frees every ID, removed ones included (one kind of load, a reload of the same mission).
  The platform reuses no ID within a DCS process.
- `setMarkupColor`, `setMarkupColorFill`, `setMarkupTypeLine`, `setMarkupFontSize` and
  `setMarkupText` do not raise, emit no event and keep the order. `setMarkupText`
  changes the panel's text.
- A freeform takes at least 7,990 points. At the default 1 MiB inbound ring, the command
  limit binds first.

A client draws by ascending ID, connected or joining later, across shape kinds (LIVE).
The style and text setters change what a client draws, and a position setter moves it.

## Decision

- **Labels.** A mark or drawing carries a label, unique among the campaign's live marks
  and drawings and free again when the item is removed. A put with ID 0 and a held label
  changes that item.
- **Markup groups.** Flat, named sets of marks and drawings with `visible`, an optional
  default expiry and an optional cap. `PutMarkupGroup`, `RemoveMarkupGroup` (which removes the items),
  `ReplaceMarkupGroup` (one write that changes, adds and removes by label) and `ListMarkupGroups`.
  A hidden markup group's items leave DCS and read applied revision 0 ([ADR 0017](0017-set-state-applied-means-dcs-holds-the-revision.md)).
- **Expiry and caps.** Expiry counts campaign model time: model seconds summed over the
  campaign's mission generations. A cap removes a markup group's oldest items in the same write.
  Expired and evicted items are journalled removals with their own cause, and follow
  the existing purge ([ADR 0038](0038-sidecar-growth-is-bounded.md)).
- **The sidecar chooses every DCS mark ID**, carried in `MARK_SET` and `DRAWING_SET`.
  Marks and drawings share one pool, 2^30 to 2^31 - 1, about 2^30 adds per DCS process
  for every campaign the process runs. Every add takes an ID never used before in the
  pool, including a re-add after a mission load or a sidecar restart (LIVE: DCS draws
  nothing for an add under a removed ID). A style-only change stays in place, takes no
  ID and keeps the item's. A geometry change is a remove and an add under a new ID.
  After every load the sidecar adds marks and drawings again together, ascending by a
  persisted draw rank, each under a new ID, so the order is kept (spec/world.md, "Redraw
  order").
- **High-water mark.** The pool has a high-water mark per DCS process, the next ID to
  give. The campaign's store commits it in the same transaction as the command that
  carries the ID, before the sidecar sends the command. The campaign's closing
  transaction writes it to the server store, and an activation raises the campaign's
  mark to it. A new DCS process starts the mark again. There is no repack inside a
  process. After a crash the shim can refuse an add with `id <ID> at or below the process
  mark <N>`; the sidecar then raises the mark past N and retries once under a fresh ID.
- **Shim guards.** The shim answers `APPLIED` without calling DCS for a set it already
  holds under the same ID, which covers retries and resends. It refuses with
  `DCS_REFUSED` any add at or below its own per-process high-water mark. It does not
  call `removeMark` for an ID it does not hold: that could only remove a mission
  script's mark and kill the ID. A removal emits `S_EVENT_MARK_REMOVED`, which carries
  the platform's `mark_id`.
- **Capacity.** A full pool refuses new adds with `RESOURCE_EXHAUSTED`, reason
  `DCS_IDS_FULL`. Items already shown stay, and the sidecar keeps working. The server
  status reports the DCS IDs left in the running process and warns when few are left.
  Capacity is a rate: 2^30 (1.07 billion) adds per DCS process. One item redrawn every
  second lasts 34 years, 100 items a second about 4 months, and 1,000 items a second
  about 12 days. A new DCS process refills the pool.
- **Mission scripts.** A mission script that uses an ID in the pool can collide with the
  platform. If the script holds the ID, DCS ignores the platform's add over it
  (UNVERIFIED for a mark); if the script removed it, the add draws nothing until the next
  mission load (LIVE). The platform does not detect either. Scripts should use IDs below
  2^30.
- **Own marks.** `WorldEvent.platform_mark` is set at intake, against the IDs issued in
  that event's boot, and tells a controller which mark events the platform caused.
  `Mark` and `Drawing` carry `dcs_id`, the ID of the last add the sidecar sent: 0 while
  the item is hidden, new, or not sent since the agent boot or mission load.
- **In place.** A change of only colors, line type, font size or a text's text is
  `DRAWING_UPDATE`, applied with the `setMarkup*` calls.
- **Player marks.** `RemovePlayerMark` is an effect (`MARK_CLEAR`) that removes a mark
  whose ID the platform did not issue in the running DCS process, named by the sequence number of the mark event that carries its ID.
  The sidecar refuses an event the log no longer holds and one from another DCS process or
  mission generation. `WorldEvent.mark_id`
  carries the ID of the three mark events.
- `ListWorld` and `WatchWorld` serve marks and drawings, with a markup group filter.
- **Persistence.** The store file holds markers and markup groups in tables of their own, from
  schema 2, with the campaign model time. A schema 1 sidecar refuses the file.

The API names them markup groups. A client draws by DCS ID, and a group has no order of
its own: lifting a group above another would mean adding every higher item again on each
change, which the frame budget does not allow. The name makes no claim about draw order.
DCS's own term for drawings is markup. `Group` is taken by unit groups.

## Consequences

- The ID order is the draw order. Every add takes a higher ID, so the last add draws on
  top. A redraw after a mission load adds items again in persisted draw-rank order, and
  the order a client shows after a DCS restart is UNVERIFIED. spec/world.md, "Redraw
  order", has one row for each kind of redraw.
- A drawing changed in place keeps its place in the ID order. A geometry change is a
  remove and an add under a new ID, and draws on top.
- Showing a markup group adds its items again under new IDs. It goes through the
  reconciler's 32 marker commands a step and uses up IDs of the pool.
- The pool holds a fixed number of adds per DCS process. A controller that redraws many
  items at a high rate can fill it; new adds then fail with `RESOURCE_EXHAUSTED` until
  DCS restarts. A style change uses no ID.
- The transport layout changes: `MARK_SET` and `DRAWING_SET` gain `dcs_id`, and two
  opcodes are added. An agent of another layout hash is refused.
- Marks, drawings, menu items and markup groups are rows of the store file, schema 2. A sidecar
  restart in the same DCS process sends the removals the old process may not have sent,
  then sets the rest again.
