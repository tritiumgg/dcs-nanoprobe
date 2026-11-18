# Person-test fixes: dynamic-spawn shell, spawned FARP, ruins, unlimited reads, dead statics

| | |
|---|---|
| Step | The fixes to what the person tests found: PICTURE's coalition name, the dynamic-spawn shell, a spawned FARP's airbase, ruin clearing, what an unlimited warehouse reads; and the owner's question whether a static added dead shows as a ruin |
| Date | 2026-11-17 |
| DCS | Server: 2.9.31.29434, branch `dcs_server.release`, dedicated server. Client: the same build, a client install, with the owner at the controls |
| Platform | Server: Linux, Wine `wine-10.0 (Debian 10.0~repack-6)` in Docker. Client: Windows |
| Builds | Branch: agent `9813a3d3bf40fc71`, built from `5126ca6`; its shim lacks the `isExist` guard of `16e3cac`. Main: agent `40cb4700d957857f` (`e05737d`) for the overhead A/B |
| Scripts | `cargo xtask sampling <target> --part mirror` (A/B, run 1 main and run 2 branch). `NP_SHELL_PERSON=<stop> cargo xtask sidecar <target> --part shell --template tests/fixtures/templates/afghanistan.miz` (runs 3 and 4, the hold). `cargo xtask eval <target> server lua/experiments/warehouse/unlimited-read.lua` during run 4. The scripts in `lua/experiments/person-fixes/` during run 4, at the owner's request |
| Rehearsal | `lua/tests/warehouse_experiments.lua` (unlimited-read), `lua/tests/dispatch.lua` (`clear-debris`, `world`), the shell and sim-fake host tests. The `person-fixes/` scripts ran only live: they were written during the hold |
| Raw results | Bundles `sampling-<target>-20261118T001245Z` (main), `...T002330Z` (branch), `sidecar-<target>-20261118T003402Z` (run 3), `...T004112Z` (run 4) |
| Result | The shell offers dynamic spawn at the stocked airport and at the spawned FARP. An unlimited warehouse lists no item and 100,000 of each liquid in `getInventory`. A dead static nobody manages is cleared from clients by `Object.destroy`. Statics and aircraft added with `dead = true` show destroyed; a BRDM-2 does not. Overhead is unchanged |

Every figure is LIVE on these builds unless tagged.

## Overhead

`sampling --part mirror` on the same server, main then branch:

| | Main | Branch |
|---|---|---|
| Pump p50 / p99 | under 16 µs / under 256 µs | under 16 µs / under 256 µs |
| Pump maximum | under 2,048 µs | under 4,096 µs |
| Frame rate at 500 / 1,200 / 2,000 rows | 113.7 / 111.5 / 73.5 | 112.9 / 111.9 / 74.2 |
| Pump work at 2,000 rows | 639 µs a second at 8.61 pumps a second | 658 µs a second at 9.78 pumps a second |
| Checks failed | 3: position period while filling, vitals band, pump rate bound | 1: pump rate bound |

The branch's changes run in command handlers and the warehouse tier; the test mission
has no FARP, so the tier's added reads did not run. Both builds fail the pump rate bound
at 2,000 rows on this server.

## The dynamic-spawn shell

The hold made Herat (airport 1) blue with `SetAirbaseOwner`, stocked it with 4
`AH-64D_BLK_II` and 50,000 kg of jet fuel, and spawned a FARP 2 km north with
`PutStatic` and the four keys of spec/commands.md, stocked the same way.

- `DCS.getAirdromesCoalition()` gave Herat coalition 2 after `SetAirbaseOwner`, so the
  owner the platform sets is the one the dialog reads.
- The owner's slot screen listed Herat (Airfield) and the FARP `np#4XF1` (Heliport) under
  dynamic slots, each with 4 AH-64D. Herat offered no hot start; the FARP offered it.
  After his spawn Herat showed 1,000 t of each fuel (fuel stays unlimited) and the FARP
  49.8 t of jet fuel and no other fuel.
- The FARP's stock read 0 just after `SetWarehouseStock` and 4 AH-64D and 50,000 kg a
  little later, once the reconciler had sent it.
- A spawned FARP is not in `coalition.getStaticObjects`; `world.getAirbases` lists its
  airbase, and `StaticObject.getByName` finds its static, alive, with the airbase's ID.
  Run 3 failed on the first point: the hold looked for the FARP among the statics.

## What an unlimited warehouse reads

The Afghanistan template marks 26 airports unlimited in munitions and fuel. On each of
the 26, `getItemCount` of `weapons.bombs.Mk_82` and `getLiquidAmount` of liquids 0 to 3
read 1,000,000, while `getInventory` listed no weapon and 4 liquids at 100,000 each.
Kandahar, unlimited in aircraft, read 1,000,000 for `getItemCount("F-15C")` and listed no
aircraft. These are the 4 entries below 1,000,000 that
docs/results/2026-11-16-warehouse-player-deltas.md saw.

The control on Herat (munitions unlimited): `addItem` of 5 Mk-82 made `getInventory` list
5 while `getItemCount` stayed 1,000,000; `removeItem` of 5 left the listing at 5.

The experiment's rule returned `inconclusive`: `env.mission.warehouses` is not readable
in the server state, so it checked no flag. The spec text rests on the reads and on the
template's own flags.

## Ruins and CLEAR_DEBRIS

- A `FARP Fuel Depot` added by script and killed with an explosion of power 1,000 read
  life 0 and `isExist` false, as do statics added with `dead = true`. The hold's reads
  printed a false `isExist` as `raised` (`ok and e or 'raised'`); the reads in
  `person-fixes/` and the fixed hold print it as read.
- The hold's first clear failed: its 30 m sphere was at altitude 0, and Herat lies about
  900 m above sea level, so the sphere held nothing (`debris_removed` 0, cost 43 µs). The
  hold now centers its spheres at the ground's height.
- A 250 km box clear around the FARP took 161 µs and removed 8 objects. After it, the
  ruin and the five statics added dead were gone by name. The shim's static step takes
  each of the six as dead, and 8 fits six statics and two other junk objects; which call
  removed which is UNVERIFIED.
- A second ruin, at 360 m east of the FARP, was destroyed by `ruin-clear.lua`, the
  shim's own check and destroy: one destroyed. The owner saw it disappear from his
  client. `StaticObject.getByName` still found it in the same call.

## Statics added dead

Added with `coalition.addStaticObject`, `dead = true` and `hidden = false`, in a line east
of the FARP. Each read life 0 and `isExist` false on the server. What the owner saw:

| Type | First look | After leaving and rejoining | F10 |
|---|---|---|---|
| FARP Fuel Depot | destroyed, burning | destroyed | not shown |
| F-15C | ruin, burning | the same | not shown |
| UH-1H | destroyed, burning | the same | not shown |
| Windsock | intact, not burning | the same | not shown |
| Shelter | destroyed | destroyed | not checked |
| FARP Ammo Dump Coating | destroyed | destroyed | not checked |
| BRDM-2 | intact, not burning | not shown at all | shown, labeled, before the rejoin |
| speedboat | refused: `addStaticObject` returned nil | | |
| `.Ammunition depot` | refused as a type name | | |

Three BRDM-2s, to see how the death reaches a client:

| BRDM-2 | How it died | Server | Client | After a rejoin |
|---|---|---|---|---|
| A | added with `dead = true` | life 0, `isExist` false | alive, on F10 | gone |
| B | added alive (life 3), an explosion of power 100 | life 0, `isExist` false | dead, burning | dead, burning |
| C | added alive, `a_unit_set_life_percentage(id, 0)` in the mission state | life 0, `isExist` false | alive | gone |

So structures and aircraft can be restored as ruins with `dead = true`; the one ground
vehicle tried cannot, and its death reaches a client only from an explosion. Whether
every ground vehicle behaves as the BRDM-2 does, and whether a `category` in the table
changes it, is UNVERIFIED. The setter killed static C, which spec/commands.md
("Layout: DAMAGE") records as doing nothing to a static (E16); E16's case was not
repeated here.

## Screenshots

`net.screenshot_request(2)` in the server's hook state returned nil and no image reached
the server. ED's web GUI calls it the same way; a server setting allows it and a client
option `allow_server_screenshots` defaults to false. The server setting is in
`serverSettings.lua`, which is not read.

## Open items

- Whether other ground vehicles, and ships, can be added dead and shown as wrecks; a
  `category` in the static table may matter.
- E16 against the setter killing a static at 0.
- The warehouse tier's added `StaticObject.getByName` and `isExist` for a FARP were not in
  the A/B; price them on a mission with FARPs.
- `env.mission.warehouses` is not readable in the server state; unlimited-read needs
  another source for the flags.
