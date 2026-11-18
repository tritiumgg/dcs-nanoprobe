# The ruin oracle and a death read after the spawn

| | |
|---|---|
| Step | The fake sim's check that no ruin spawn replaces a static's own dead object in its generation (spec/world.md, "Ruins", "One ruin a generation") counted a death the sidecar read only after it wrote the spawn |
| Date | 2026-11-18 |
| DCS | None: host tests on the fake sim |
| Platform | macOS 26.6.2 on arm64; Rust 1.99.1 |
| Script | `mise exec -- cargo test -p nanoprobe-sidecar --test reconciler` with and without `--no-default-features`, and `mise exec -- cargo test -p nanoprobe-sim-fake`, which runs `ruins` and `ruin_oracle` |
| Code | The commit that adds this document: test tooling in `crates/sim-fake`, and tests in `crates/sidecar/tests` for the seed line and its plain replay. No sidecar source and no spec changed. The run with the old check was the parent commit with the seed line added |
| Tests | `crates/sidecar/tests/reconciler.rs`: `the_gate_holds_with_late_deaths_removals_and_revivals` with its seed in `reconciler.proptest-regressions`, and `a_ruin_spawned_before_a_late_death_is_read_replaces_the_dead_object`, the seed's op list as a plain test. `crates/sim-fake/tests/ruin_oracle.rs`: three tests that plant a ruin spawn by hand |
| Result | The seed fails with the old check and passes with the new one, on both feature sets. The plain replay reads fields the old code lacks, so it ran only with the new check. A planted spawn written after the death was read is still reported. The 25 fake-sim ruin tests and the other reconciler tests pass unchanged |

## The failure

The property test failed every run on the seed's line: "settling: a ruin was spawned
over static 5, which died in that generation". The sequence, traced on the fake sim:

1. A foreign campaign takes the agent. The agent drops its applied revisions.
2. The sidecar restarts and attaches. Its first step switches the agent back to its
   campaign. The agent still holds no applied revision.
3. Static 5 dies with a late death event: its row leaves as `MISSING`, and the
   `OBJECT_GONE` `DEAD` is due later. DCS holds the dead object.
4. In the settle phase, the `on_lost = dead` policy makes the static `dead` with no
   death fact. The ruin plan writes the ruin's spawn. The agent holds no revision for
   it, so it answers `APPLIED` and replaces the dead object with a dead static of the
   same name.
5. The death record arrives, and the sidecar reads it.

The check looked up the death in the set of records written to the ring when it ran,
after step 5. It counted a death the sidecar had not read when it wrote the spawn.

The late death in step 3 is not a record that should have been dropped at the
campaign switch. The switch back runs in step 2, before the kill, and clears the
agent's pending death events as it should. The death comes after it, in the
sidecar's own campaign.

## The reconciler is right

When the sidecar wrote the spawn, the store had no death fact for static 5 and no
mark that DCS held its dead object. spec/world.md, "Ruins", says a static the lost
policy marks dead is a ruin and that DCS should hold it. Nothing told the sidecar the
object was there. The spawn was answered `APPLIED` because the campaign switch cleared
the agent's revisions, as spec/commands.md says the agent does on a campaign switch. DCS
ends with one dead static of that name, and the store holds the static as `dead`.

## The fix

The fake sim keeps a link clock that counts the sidecar's reads and command writes.
For each reported death it keeps the clock of its first read that a flush
acknowledged (`Ledger::deaths_read`), and for each command the clock it was written
at. Each replaced ruin in `Ledger::ruins_replaced` carries a flag: the sidecar had
read the static's death before the spawn was written. The check in
`World::check_safety`, run after every tick, reports a replacement only when that
flag is set. A read that no flush acknowledged does not count: the record is read
again after the next attach. For the same reason, the deaths a sidecar with another
store takes as already read (`World::sidecar_of`) are those acknowledged reads.

This rule is narrower than "the death was read before the spawn ran". A command can
wait in the ring for frames after it was written. A death read in that gap reached
the sidecar too late to stop the spawn, so it is not counted either.

## Tests

- The plain replay asserts that settling passes, one ruin was replaced with the flag
  clear, the death was read afterwards, and the static is `dead` with its ruin in DCS.
- One planted test kills a static, waits until the sidecar has the death, writes a
  ruin spawn into the ring by hand, and asserts the check reports it.
- The other two drive the fake frame by frame with no reconciler step. A spawn that
  runs while the death record is in the ring and unread gets the flag clear. So does
  one written before a read and a flush that come before it runs. One written after a
  read and a flush gets it set. A read with no flush is not counted.

## Limits

- Host only. The sequence needs a foreign campaign's sidecar to take the agent and
  hand it back, a late death, and the dead policy. It is not observed on a server.

## Open items

- Three statements in spec/world.md, "Ruins", do not hold in this sequence: the
  table's row for a static that died in the loaded generation ("Nothing"), "the object
  of a static that died in the generation is never replaced" ("One ruin a
  generation"), and the agent still holding the revision in the generation the static
  left in (the lost-policy bullet). The campaign switch cleared the agent's revisions,
  and the ruin spawn written before the death was read replaced the static's dead
  object. The outcome is still one dead static of that name. The spec text can say
  that these hold while the agent holds the revision.
