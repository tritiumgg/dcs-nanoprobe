# The dead object of a static the lost policy marked dead

| | |
|---|---|
| Step | A static the lost policy marked `dead` while DCS still held its object, whose object then dies in DCS: the reconciler handles the dead object as the static's ruin (spec/world.md, "Ruins") |
| Date | 2026-11-18 |
| DCS | None: host tests on the fake sim |
| Platform | macOS 26.6.2 on arm64; Rust 1.99.1 |
| Script | `mise exec -- cargo test -p nanoprobe-sim-fake --test ruins the_dead_object_of` and `mise exec -- cargo test -p nanoprobe-sidecar --test reconciler the_gate_holds_with_late_deaths_removals_and_revivals` |
| Code | The commit that adds this document. The runs without the fix used it with the call to `ruin_died` in `gone()`'s arm for a `dead` group switched off |
| Tests | `crates/sim-fake/tests/ruins.rs`, `the_dead_object_of_a_static_the_lost_policy_marked_dead_is_destroyed`; `crates/sidecar/tests/reconciler.rs`, `the_gate_holds_with_late_deaths_removals_and_revivals`, with its seed in `crates/sidecar/tests/reconciler.proptest-regressions` |
| Result | Both fail without the fix and pass with it. The other 24 fake-sim ruin tests, the sidecar's own ruin tests and the other 58 reconciler tests pass unchanged |

## The defect

The property test found it. It failed every run on the seed's line, with "settling: DCS
holds a dead static of 26, which is dead and should have none". The sequence:

1. A static is in DCS. Its authority moves to the campaign, and the reconciler sends
   `STATIC_DESTROY`. DCS refuses every destroy.
2. The destroys run out of retries. The static goes `lost`, then `dead` under
   `on_lost = dead`. It has no death fact, and DCS still holds its object.
3. The object dies in DCS. The agent reports `OBJECT_GONE` with reason `DEAD`.
4. `gone()` handled a death of a static already `dead` only by counting it
   (`after_dead`). It did not mark that DCS holds the dead object, so the ruin plan saw
   no ruin: it sent no `STATIC_DESTROY`, and the wreck stayed in DCS for good.

## The fix

In `gone()` (`crates/sidecar/src/reconciler.rs`), a `DEAD` record for a group already
`dead` also calls `ruin_died` when the record is about the object the store knows, as
the stale check of the neighboring arm decides. `gone()` runs only for records of the
loaded generation. For a static, `ruin_died` marks that DCS holds its dead object in
that generation. The ruin plan then destroys it when the static should hold none, and
spawns no second ruin when it should hold one. For a group of units it does nothing.

spec/world.md says the same: a death in DCS of a static already `dead` sets the mark,
and the lost-policy bullet covers a static whose object DCS still holds.

## The regression test

The fake sim refuses every destroy while a static moves to the campaign. After 200
ticks the static is `dead` and its live object is still in DCS. The test kills the
object and, after 5 ticks, asserts the store's mark that DCS holds the dead object in
the loaded generation. It then lets destroys through again, runs 20 ticks, and asserts
a further `STATIC_DESTROY`, no dead object in DCS, no ruin spawn, the state `dead`,
and the harness's settle oracle. Without the fix the mark assert fails: the store
holds no mark.

## Limits

- Host only. Whether DCS refuses a static's destroys repeatedly, and so how often the
  case arises on a server, is not observed.
- The case where the static should hold its ruin (authority back to DCS before the
  destroy) is covered by the property test's oracle, not by a named test.

## Open items

- A second `OBJECT_GONE` `DEAD` for the known object of a dead static, in the same
  generation and after an applied destroy cleared the mark, would set the mark again
  and send one more `STATIC_DESTROY`, bounded by `destroy_retries`. The agent reports a
  row gone once, so the case is likely unreachable.
