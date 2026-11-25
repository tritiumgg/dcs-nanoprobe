# Static replace: a static's spec revised in place

`PutStatic` with a `static_id` revises a static's spec, and the sidecar spawns it again
under its name ([ADR 0047](../adr/0047-a-statics-spec-is-revised-in-place.md);
spec/world.md, "Revising a static"). On a running DCS a fortification moves and turns, a
cargo static moves, and a FARP grows to a group of four and shrinks back, paused and
running, as one airbase each time with the old airbase's owner and stock carried over.

## Runs 1 and 2

| | |
|---|---|
| DCS | 2.9.31.29434 dedicated server, `dcs_server.release` |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Build | Both runs: agent `5cc971b680d27307`, sidecar binary sha256 `b991265d690d70b116a690709315fc35fecd59157ceba2625db53db0c883e9d1`, from the step's uncommitted tree over `ee256d3`. Run 2's part differs from run 1's only in the two checks below. Bundles `sidecar-<target>-20261125T104820Z` (run 1) and `sidecar-<target>-20261125T110207Z` (run 2) |
| Date | 2026-11-25, local time |
| Mission | A fresh shell mission on Caucasus the part loads, running; no player but the server |
| Script | `cargo xtask sidecar <name> --part static-replace` (`crates/xtask/src/shell_live/static_replace.rs`) |
| Rehearsal | The API flows in `crates/fake-sidecar/tests/static_replace.rs` and `crates/sim-fake/tests/static_replace.rs`. Every eval snippet of the part ran once without a Lua error under the strict mock, through a script that was not kept; the mock's heading of a turned static and its `world.searchObjects` differ from DCS, so those reads were not rehearsed |

The part makes its objects on land (surface type 1 at every place) near north -289,000,
east 681,000, the paused variant 3 km east of the running one. Each FARP has jet fuel
5,000 kg set with `SetWarehouseStock`, and an item no call set, 10 `Mk_82`, added in DCS.
The running variant sets the red owner with `SetAirbaseOwner`. The paused variant sets
no owner and captures the FARP in DCS (`autoCapture(false)`, `setCoalition(1)`), so its
owner is carried, not desired. All reads below are LIVE.

Run 1 failed two of its own checks, which run 2 fixed:

- It expected the new airbase's `getID()` to change. DCS keeps it across a replace
  (1000005 before and after) and gives the airbase a new object: `id_` 16777729, then
  16778241. Run 2 compares the object.
- Its capture called `setCoalition(1)` alone; DCS kept coalition 2. The agent's
  `AIRBASE_OWNER` handler turns `autoCapture` off first, and so does run 2's capture.

In run 1 every other check of those steps held: the FARP group of four had 16 spots,
every further name found the airbase, and the owner, fuel and `Mk_82` came back.

The tables below are run 2's; run 1 read the same for the fortification and the cargo.
The part as it ran in runs 1 and 2 differs from the committed one: its FARP steps went on
to `FARP` to `SINGLE_HELIPAD` and back (the committed part checks that `PutStatic` refuses
that change), its settled read came 65 s after the owner and fuel came back (130 s in the
committed part), and its show check did not require the read's length or P's own static.
Run 2's reads of the retype step are in its bundle.

### Fortification and cargo

| Revision | Running | Paused |
|---|---|---|
| `Hangar A` moved 50 m north and turned 90 degrees | At the new place and heading (1.5708 rad) within 1 m and 0.01 rad, a new object (`id_` 8000001 to 8000003), `getID()` the same; `LIVE` at spec revision 2; nothing found within 5 m of the old place | The same at once, while paused; after 70 s still paused, past one 60 s enumeration, the same, and the World lists no unmanaged static |
| `ammo_cargo` moved 30 m north | At the new place; the World's position the revised one 10 s later | Not run |

The old fortification did not linger while paused: `world.searchObjects` found no static
within 5 m of its place right after the revision and after 70 s. ADR 0015's claim that a
replaced static stays until the resume does not hold for this case.

### FARP

Each step revises the same static. "Shown" is what DCS read once the new airbase stood;
"settled" is what DCS read 65 s after the owner and fuel came back, past one warehouse
sample.

| Step | Spots | Shown on the new airbase | Settled |
|---|---|---|---|
| 1 `FARP` to a group of 4 | 16 | A new object, coalition 2, fuel 0, `Mk_82` 0; listed once by `world.getAirbases`; each further name finds it | Coalition 1, fuel 5,000, `Mk_82` 10 |
| 4 to 2 | 8 | The same; the 2 dropped names find nothing live | The same |
| 2 to 1 | 4 | The same; the dropped name finds nothing live | The same |
| `FARP` to `SINGLE_HELIPAD` | 4 | `getTypeName` `SINGLE_HELIPAD` on the airbase and the static, a new object (`id_` 16778753 to 16779009), the same `getID()`; the owner, fuel and `Mk_82` already back | Not read: the step waited for 1 spot |

The store's desired owner and stock have no API read. In the running variant the fuel and
the red owner are desired values, which the carry never sends, so DCS reading them
unchanged after the settle shows the new airbase's empty first sample did not lower them.

Both variants gave the same reads. In the paused variant the owner set by the capture,
which no call desired, came back too: the carry restored it.

- A group spawn over a FARP's name, at any size, makes one airbase under the name, with
  the spots of every unit, and leaves the dropped names finding nothing live.
- The new airbase starts with the coalition of the static's country and an empty
  warehouse. Its `getID()` is the old airbase's.
- A `SINGLE_HELIPAD` over a `FARP` reads 4 spots (`#getParking()`), where a new
  `SINGLE_HELIPAD` has 1 (docs/results/2026-11-25-farp-groups.md). The type took and the
  parking did not, so `PutStatic` refuses a heliport type change in place (spec/world.md,
  "Revising a static"). Run 3's probe read the same on an unmanaged name (below). The
  shape (`shape_name`) was not read. `SINGLE_HELIPAD` back to `FARP` was not tried.

### Sampling

| | Airbases | Sim-thread share | Longest frame (µs) | Frames over 250 µs |
|---|---|---|---|---|
| Before the objects | 21 | 0.060 % | 393.8 | 2 |
| With both FARPs standing | 23 | 0.074 % | 191.1 | 0 |

### Removal

`RemoveEntity` of every entity, with the mission running: every static, further and
marker name found nothing live, no airbase of the names was listed, and the World was
empty.

## Run 3

| | |
|---|---|
| DCS | 2.9.31.29434 dedicated server, `dcs_server.release` |
| Platform | Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker |
| Build | Agent `2b02e44249f76438` (DLL sha256 `8521504878ccad99d1637d9f0e12f2276b441462990eee818d2280735380bb83`), sidecar binary sha256 `fe5fc4da1951ceaf0e623bc9d3a00d091e160b7c8910eeafd9da0d83b2266df6`, from the step's tree over `ee256d3` after the last review fixes. Bundle `sidecar-<target>-20261125T140857Z` |
| Date | 2026-11-25, local time |
| Script | `cargo xtask sidecar <name> --part static-replace` (`crates/xtask/src/shell_live/static_replace.rs`) |
| Rehearsal | As runs 1 and 2. The retype probe's snippets ran under the strict mock without a Lua error, through a script that was not kept |

Every ruled step passed in both variants, and the removal passed. The part failed on its
retype probe's cleanup check, described below; its reads stand.

| Step | Running | Paused |
|---|---|---|
| `Hangar A` moved and turned | Spec revision 2; a new object (`id_` 8000001 to 8000003), `getID()` the same; nothing within 5 m of the old place; no unmanaged static | The same (`id_` 8000012 to 8000014); still so after 70 s paused |
| `ammo_cargo` moved 30 m north | At the new place, and the World's position the revised one | Not run |
| `FARP` 1 to 4, 4 to 2, 2 to 1 | 16, 8 and 4 spots; each a new airbase object, `getID()` 1000005 throughout, listed once; shown with coalition 2, fuel 0, `Mk_82` 0; then coalition 1, fuel 5,000, `Mk_82` 10, and the same 130 s later | The same, `getID()` 1000022 throughout; the captured owner came back |
| `FARP` to `SINGLE_HELIPAD` | `FAILED_PRECONDITION`, "a heliport's type does not change in place"; spec revision 4 before and after; the same `FARP` object with 4 spots | The same |

The removal left no static, further or marker name finding anything live, and no
airbase of the names listed.

### Retype probe

On the unmanaged name `np-sr-retype-probe`, north -289,500, east 681,300, the probe
spawned a one-unit `FARP` group with `coalition.addGroup(2, -1, ...)`, then a one-unit
`SINGLE_HELIPAD` group over the same name.

| Read | Airbase type | Spots | Object `id_` | `getID()` | Listed |
|---|---|---|---|---|---|
| `FARP`, at once and 10 s later | `FARP` | 4 | 16780033 | 1000035 | 1 |
| `SINGLE_HELIPAD`, at once and 10 s later | `SINGLE_HELIPAD`, the static too | 4 | 16780289 | 1000035 | 1 |

This repeats run 2's finding: the type took, the 4 spots stayed, and DCS gave a new object
under the same `getID()`.

The probe's cleanup check failed. Its destroy returned without error, yet after 120 s
the name still found an existing static and airbase. The mission read running before the
second sampling report and paused when the removal began. What paused it is not known.
DCS takes a destroy only while the simulation runs (docs/results/2026-11-25-farp-groups.md),
so the probe most likely ran paused. After the removal resumed the mission, both objects
read `isExist` false. The probe's place read surface type 3, water; 1 km north of the
`FARP` place reads 1, land.
The committed probe stands there, and resumes the mission before it spawns and before it
destroys. That version has not run live.

### Sampling

| | Airbases | Sim-thread share | Longest frame (µs) | Frames over 250 µs |
|---|---|---|---|---|
| Before the objects | 21 | 0.062 % | 426.5 | 2 |
| With both FARPs standing | 23 | 0.071 % | 184.7 | 0 |

## Open

- The part's eval snippets have no kept rehearsal under the mock.
- What paused the mission between the second sampling report and run 3's removal.
- The committed retype probe, which resumes the mission and stands on land, has not run
  live.
- A static DCS reports as cargo whose spec lacks `extra.category` `Cargos` keeps its spec
  place; whether DCS makes one is UNVERIFIED.
- After a sidecar restart the reconciler takes a `live` entity's applied revision as its
  store revision, so a revision made but not yet spawned before the restart reaches DCS
  only at the next generation. This holds for groups too.
- What DCS offers players on a revised airbase's pads, and after a heliport type change.
  Needs a client.
- A heliport type change in place, refused until a live check shows DCS changes the
  parking with the type.

## Runs and host checks

| Run | Build | Result |
|---|---|---|
| Live 1 | Agent `5cc971b680d27307` | Two of the part's own checks wrong (see above); every other check held |
| Live 2 | Agent `5cc971b680d27307` | Pass |
| Live 3 | Agent `2b02e44249f76438`, the final build | Every ruled step and the removal pass; the retype probe's cleanup check failed (above) |
| `mise run check` | The final tree, before the probe's place and resume fix | Pass, 1,613 tests |
| `mise run dll` | The same | Pass |

Live runs used: 3 of 3.
