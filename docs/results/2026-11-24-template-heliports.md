# Heliports in static templates

`PutFromStaticTemplate` builds a static of category `Heliports` as any static, so as
`PutStatic` builds a FARP ([ADR 0043](../adr/0043-static-template-spawn-with-heliports.md);
spec/world.md, "Static templates"). A static of category `GrassAirfields`, or with a
`grassairfield_` key, still refuses the file.

## A file the Mission Editor saved

A static template saved by the Mission Editor of a DCS 2.9.31.29434 client on Windows,
2026-11-24: 3,328 bytes, `theatre` `Caucasus`, under blue country 2 two static groups of
one unit each, an `APFC fuel` (`Fortifications`) and a `SINGLE_HELIPAD` (`Heliports`),
40 m apart. It holds no vehicle group. The file is not in the repository.

| Fact | Tag |
|---|---|
| The editor saves a static group with `heading`, `route` (one point with `alt`, `type`, `name`, `x`, `y`, `speed`, `formation_template`, `action`), `groupId`, `units`, `x`, `y`, `name` and `dead` | LIVE |
| It saves a static unit with `tasks` (empty), `category`, `shape_name`, `type`, `unitId`, `rate` (the fortification only), `x`, `y`, `name` and `heading` | LIVE |
| A helipad's unit also has `heliport_frequency` (the text `"127.5"`), `heliport_modulation` (0) and `heliport_callsign_id` (1); its `shape_name` is `FARP` | LIVE |
| Empty coalitions are saved with `country = {}`; the file has `name`, `requiredModules`, `localization`, `theatre` and `desc` at the top | LIVE |

The reader reads the file, and the conversion makes two statics. It passes `category`,
`shape_name` and `rate`, and drops and logs the rest: `static.groupId`,
`static.heading`, `static.route`, `static.x`, `static.y`,
`static.units[].heliport_callsign_id`, `static.units[].heliport_frequency`,
`static.units[].heliport_modulation` and `static.units[].tasks`. This is the first
static read from a file the editor saved; the form the editor's code gives
(docs/results/2026-11-23-static-templates.md) holds for these keys.

## Live

`cargo xtask sidecar <name> --part template-heliport --file FILE`
(`crates/xtask/src/shell_live/template_heliport.rs`), with FILE the saved file, on a
DCS 2.9.31.29434 dedicated server (`dcs_server.release`) on Linux under Wine 10.0
(`wine-10.0 (Debian 10.0~repack-6)`), 2026-11-24, from this commit's tree before
review: agent build `761f95a6c55d830f`, sidecar binary sha256
`5361e3233978d197ba08ae8d11804cafeae65077699b199c2e5db872d6385d1f`. The sidecar ran in
its own container beside DCS, as the static-template part runs it.

The run puts the file in the listed folder and makes it in place. It also makes the
file's helipad with `PutStatic`, from the spec the conversion gave, 60 m north: the
twin. It reads each static by `StaticObject.getByName` and `Airbase.getByName`, and
which of them `world.getAirbases` lists. It sets 5,000 kg of jet fuel under each
helipad's name with `SetWarehouseStock` and reads `getLiquidAmount(0)` from DCS. It
records each static's state in the World, then removes everything and the file.

| Object | Asked: north, east | Read from DCS: north, east | Airbase |
|---|---|---|---|
| `APFC fuel`, from the file | -356,720.875, 619,153.742 | -356,720.875, 619,153.75 (static) | none |
| `SINGLE_HELIPAD`, from the file | -356,719.635, 619,193.397 | -356,719.625, 619,193.375 (airbase and static) | ID 1000002, category 1 (helipad), listed |
| `SINGLE_HELIPAD`, the `PutStatic` twin | -356,659.635, 619,193.397 | -356,659.625, 619,193.375 (airbase and static) | ID 1000004, category 1 (helipad), listed |

| Check | Template helipad | `PutStatic` twin |
|---|---|---|
| The airbase stands within 1 m, as `SINGLE_HELIPAD` | Yes | Yes |
| `StaticObject.getByName` finds the static there, as `SINGLE_HELIPAD` | Yes | Yes |
| Airbase category and type | 1, `SINGLE_HELIPAD` | 1, `SINGLE_HELIPAD` |
| `world.getAirbases` lists its airbase under the static's platform name | Yes | Yes |
| 5,000 kg of fuel set through the sidecar, read in DCS | 5,000 | 5,000 |
| State in the World | `LOST` | `LOST` |
| After `RemoveEntity`, within 120 s | Static and airbase still in DCS | Static and airbase still in DCS |

The fortification was `LIVE` and was gone after its removal. The answer listed no group
and the two statics at spec revision 1, store revision 9. `land.getSurfaceType` was 1
(land) at the three places. The file was removed. The sidecar's exit code after a stop
was 0. Verdict: PASS (LIVE).

The verdict's rule: every static that is not a heliport stands within 1 m of its place
as its type and country; every heliport's airbase stands there as its type, and so does
its static, as its type and country, when `StaticObject.getByName` finds one; only the heliports make an airbase, and
`world.getAirbases` lists it; the fuel reaches DCS at each heliport; the template's
helipad and its twin have the same state, the same airbase category and type, and the
same outcome of the removal; every static that is not a heliport is removed. After
run 2 the review tightened the rule to judge the heliport's type, its static's place,
type and country, every static's country, the airbase kind and the presence of both
states; run 2's recorded reads meet
the tightened rule, and no further run was made. The tolerances are this step's,
as for the static-template part. The sidecar sends `WAREHOUSE_SET` only for an airbase
whose warehouse the agent sampled (spec/world.md, "Kinds"), so the fuel in DCS shows the
sidecar keeps each helipad's airbase and warehouse. The World service lists no airbase
of any kind (`crates/api/src/convert.rs`), so the World cannot show it.

The mock has no FARP airbase (docs/results/2026-11-16-person-tests-2.md, its rehearsal
row), and the fake sim makes none from a FARP spawn, so the part's DCS reads could not
be rehearsed. What it could cover
was: the conversion and the store write against `PutStatic` (the service test on the
fake sim) and the verdict on good and bad reads (the part's host tests).

Runs used: 2 of 3. Run 1 read the same places, airbase and listing, and failed on the
harness: it waited for the World to list the airbase, which the World never does. Run 2
ran the harness above.

## Open

- A FARP static the platform spawns, by either call, reaches `LOST` and stays in DCS,
  with its airbase, after `RemoveEntity`. That it reaches `LOST` because the sidecar
  sees no object of it is UNVERIFIED; `coalition.getStaticObjects` does not list a FARP
  (LIVE, ADR 0032).
- The FARP's frequency, modulation and callsign from the file are dropped.

## Runs

macOS (aarch64), the working tree of this commit, 2026-11-24.

| Command | Result |
|---|---|
| `cargo test -p nanoprobe-api --lib -- static_template` | 24 passed, the new helipad conversion and the service test against `PutStatic` among them |
| `cargo test -p xtask template_heliport` | 5 passed: the live verdict passes good reads, with and without a found heliport static, and fails each missing, misplaced, wrong-typed or wrong-country object; the template-against-twin comparison passes run 2's recorded results and fails each difference; a file on another theater or without a heliport is refused before the run |
| `mise run check` | Passes: 1,515 tests passed and 0 failed, and every other step |
| `mise run dll` | Passes |
