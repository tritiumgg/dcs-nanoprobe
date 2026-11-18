# A group from a group template file

`PutGroupFromTemplate` makes a group from one template of a Mission Editor
`templates.lua` file that the administrator provides. The sidecar reads the file once,
as data, and keeps the converted spec (spec/world.md, "Group templates").

## What Eagle Dynamics' files show

Read on 2026-11-18 in the install of a DCS dedicated server on Linux under Wine 10.0,
with `ls`, `wc`, `grep`, `awk` and `head`; nothing was written and no file was copied.
The server's build was 2.9.31.29434 when it was last read, on 2026-11-10 (PRIOR; not
read again on the day). `templates.lua` is dated 19 September.

| Fact | Tag | Where |
|---|---|---|
| The shipped group templates are one file of 185,899 bytes and 5,614 lines, holding 51 templates | ED-CODE | `MissionEditor/data/scripts/templates.lua` |
| The file is one statement, `templates = { ["<name>"] = {...}, ... } -- end of templates`. Every value is a literal. No line starts a second statement, and the file has no call, `local`, `return` or function | ED-CODE | the same |
| The file's only keys are `type`, `name`, `country`, `units`, and per unit `dx`, `dy`, `name`, `skill`, `heading` | ED-CODE | the same |
| `type` is `"vehicle"` in all 51; `country` is a number in all 51, 18 distinct values from 0 to 81 | ED-CODE | the same |
| A unit's `name` is its DCS type; `skill` is text such as `"High"`; the headings in the lines read are `0`, `3.1415926535898`, `4.7123889803847` and `5.8643062867009`, which are radians | ED-CODE | the same, its first 45 and last 8 lines |
| The editor saves a unit as `name = unit.type`, `dx = unit.x - x`, `dy = unit.y - y`, `skill`, `heading = unit.heading`, and a `payload` when the unit has one, with `x` and `y` the group's | ED-CODE | `MissionEditor/modules/me_template.lua`, lines 99 to 105 |
| The editor turns a group about its own place by `x + dx*cos(h) - dy*sin(h)` and `y + dx*sin(h) + dy*cos(h)`, and sets each heading to `math.mod(h + heading, 2π)` | ED-CODE | the same, lines 215 to 228 |
| The editor's script names the group types `"helicopter"` and `"plane"` beside ground groups. The `type` it saves in a template for them is UNVERIFIED | ED-CODE | the same, line 92 |
| A unit table's `x` and `y` are the world's `x` and `z`: a group spawned again from its sampled place stood 25 m further along its route after a reload and 3 m short after DCS was killed | LIVE | docs/results/2026-11-15-s18-persistence-live.md |
| Where the editor saves a user's own templates | UNVERIFIED | |
| A file the editor saved for a user has the shipped file's form | UNVERIFIED | none was read |
| What `type` the editor writes for a ship or a train group | UNVERIFIED | |

So `dx` is on the mission's `x` and `dy` on its `y` (ED-CODE). The API names the
world's `x` `north` and its `z` `east` (spec/world.md, "Positions and theaters"), so
the sidecar adds `dx` to the anchor's `north` and `dy` to its `east`, and its turn is
the editor's own. Where a unit made by the call stands on DCS is UNVERIFIED until the
live part runs.

The commands, run in the install's folder with `T=MissionEditor/data/scripts/templates.lua`
and `M=MissionEditor/modules/me_template.lua`:

| Figure | Command |
|---|---|
| Bytes, lines and date | `wc -lc "$T"`, `ls -l "$T"` |
| The first and last lines | `head -45 "$T"`, `tail -8 "$T"` |
| One statement, no second one | `grep -v '^[[:space:]]' "$T" \| cut -c1-30 \| sort \| uniq -c`: three lines start in column 0, once each: `templates = `, `{` and `} -- end of templates` |
| The keys | `grep -o '\["[A-Za-z_0-9]*"\]' "$T" \| sort \| uniq -c`: `name` 695, `skill`, `heading`, `dx` and `dy` 644 each, `units` 102, `type` and `country` 51 each (`units` counts each closing comment too) |
| No call or keyword | `grep -c -E '[A-Za-z_]\(\|\bfunction\b\|\blocal\b\|\breturn\b\|\bfor\b\|\bif\b\|\.\.' "$T"` gives 27 lines, each a template's name with brackets in it, such as one ending `(Asset Pack Required)`. The same count after `sed -e 's/"[^"]*"//g' -e 's/--.*$//'` removed the quoted strings and the comments is 0 |
| `type` and `country` | `grep -E '\["(type\|country)"\]' "$T" \| sort \| uniq -c` |
| The editor's lines | `grep -n -E '\bdx\b\|\bdy\b\|heading' "$M"`, then `awk 'NR>=88 && NR<=106' "$M"` and `awk 'NR>=208 && NR<=230' "$M"` |
| The other kinds | `ls -l MissionEditor/data/scripts/StaticTemplate`, the same column-0 and key counts over `*.stm`, and `head` of the smallest file of `MissionEditor/data/scripts/UnitPayloads` |

The same look found the other template kinds, which this call does not read:
23 static template files, each one assignment `staticTemplate = {...}` of literals
holding vehicle groups with routes (`MissionEditor/data/scripts/StaticTemplate/`,
ED-CODE), and payload preset files of the form `local unitPayloads = {...}` then
`return unitPayloads` (`MissionEditor/data/scripts/UnitPayloads/`, ED-CODE, one file
read).

## What the call does

| Step | Rule |
|---|---|
| Path | The rules and messages of `SetCampaignTemplate` (docs/results/2026-11-17-template-path.md): absolute for the sidecar's system, resolved into a folder that `[missions] template_dirs` lists. |
| Read | Once, on a worker thread, without the hub's lock, and at most 1 MiB: a larger file is refused by its size, before it is read. |
| Parse | The data-only reader that reads a mission template's members (`crates/sidecar/src/shell/lua.rs`). The root must be `templates`. Any other statement, and any value that is not a literal, refuses the file with its line. |
| Convert | The named template to a `GroupSpec`: a ground group, the file's `country` unless the request gives one, each unit at the anchor plus its offset, turned by the request's `heading`. |
| Write | The spec goes through the code path of `PutGroup`, in the call's store commit. |
| After | A mission load, a reload, a backup, a restore, an export and a restarted sidecar use the store. Nothing opens the path again. |

A refusal changes nothing. Its reason, field and message are in spec/errors.md.

## Decisions made in this step

- **The reader's cap is 1 MiB, not the 16 MiB of a mission template.** The reader
  keeps a node of 72 bytes for every value. `the_parsed_tree_is_bounded_at_the_cap`
  measures the tree of six files at the cap, each built to cost the most in its own
  way: `0,` repeated, `{},`, `{0},`, `{{0}},`, `a=0,`, and a chain of 190 nested
  tables. The largest tree is 37,748,304 bytes, 36 for each byte of source, which is
  about 600 MiB at 16 MiB. The shipped file is under a fifth of the cap.
- **The reader trims each table's list of fields when the table ends**
  (`crates/sidecar/src/shell/lua.rs`). A list that is pushed to keeps room for four
  fields when it holds one: with the trim left out, the chain of nested tables measures
  150,446,592 bytes at the cap, 143 for each byte of source. A mission template's
  members are read by the same reader and get the same trim.
- **The anchor and the heading are checked before the file's refusal is given.** The
  file is read before the call takes the lock, and its refusal is reported inside the
  call, after the lease, the campaign, the anchor and the heading. A repeat under an
  answered idempotency key gets its first answer although the file is gone.
- **A heading of 0 changes no number.** Each offset and heading is then the file's
  own value. With a turn, the heading is the remainder of 2π, as the editor's is.
- **Of two templates with one name, the later is used**, as Lua keeps the later of two
  equal keys in a table constructor.
- **A template with no usable `country` is refused unless the request gives one.** A
  `country` that is not a whole number from 0 to 4,294,967,295 counts as none.
- **A `skill` that is not text is left out; units must be numbered 1 to n.** A hole in
  the numbering is refused, because Lua's own reading of such a list is not defined.
- **The log names at most 16 unused keys**, so a hostile file cannot flood it.
- **A request whose order is refused makes no group**: the order is checked by
  `PutGroup`'s code before the group is created.

## Live

Not run. The live part is built: `cargo xtask sidecar <name> --part template` writes a
one-unit template file into the listed folder, with `dx` 120 and `dy` 50, makes two
groups from it at one anchor, the second turned a quarter turn, reads each unit back
with `Unit.getByName`, and removes the groups and the file, also when a step fails.
Its verdict is tested on the host: a unit at the place the axes exchanged would give,
a `dy` dropped or of the wrong sign, and a turn to the left each fail it. It shows
where the call puts a unit on DCS; the meaning of `dx` and `dy` comes from the editor's
script above.

## Runs

macOS (aarch64), the working tree of this commit, 2026-11-18.

| Command | Result |
|---|---|
| `cargo test -p nanoprobe-api --lib -- group_template template_file` | 23 passed: the converter's 14, three of them property tests of 2,000 cases each, the service test, and the file read's 8. |
| `cargo test -p nanoprobe-fake-sidecar --test api` | 11 passed. |
| `cargo test -p xtask template` | 6 passed, 2 of them the live part's file and verdict. |
| `buf lint`, `sh tools/protobreak.sh`, `cargo xtask proto --check` | Pass: the call and its two messages are additions. |

## Tests

| Rule | Test |
|---|---|
| A file in the editor's form, written for the tests; offsets; the turn, to the left, and past a full turn; the country from the file, from the request, and 0; the label | `crates/api/src/group_template.rs`, `tests` |
| Reader refusals, each construct on a line of its own and the refusal naming that line: `local`, `return`, a second statement, a call after the table, a call as a value, a function, an operator, a name as a value, 300 nested tables; a wrong root, an empty file, a file over the cap | `a_file_that_is_not_a_group_template_file_is_refused`, `a_file_over_the_cap_is_refused_before_it_is_parsed` |
| Template refusals, each with its field and message | `a_template_the_conversion_cannot_use_is_refused`, `a_place_that_is_not_finite_is_refused` |
| Names ignored, unknown keys dropped and listed, the later duplicate | `names_are_ignored_unknown_keys_are_dropped_and_the_last_duplicate_wins` |
| Arbitrary bytes and shuffled template text are read or refused, never a panic | `arbitrary_bytes_are_read_or_refused`, `shuffled_template_text_is_read_or_refused` |
| Whole generated templates of 1 to 5 units, with plain and extreme numbers, the string forms and repeated keys: one of plain numbers is always read with the file's numbers and always converts to finite places; one with an extreme number converts to finite places or is refused for that number | `generated_templates_convert_or_are_refused_for_a_number` |
| The parsed tree at the cap | `the_parsed_tree_is_bounded_at_the_cap` |
| Through the services on the fake sim: 26 refusals that leave the store's revision as it was, among them a template of 300 units that is over one command to the agent, and an anchor, a theater and a heading refused although the file would be refused too; no lease, with a file that is there and one that is not, and a wrong lease; the answer; a group written by hand through `PutGroup` equal in the store and, once live, in what DCS reports; the repeat under an answered key with the file deleted; a new key refused; another request under the key; a reload; a restore of a backup and a new sidecar with a new DCS | `crates/api/src/hub/tests/group_template.rs` |
| An observer is refused before the path is looked at | `crates/fake-sidecar/tests/api.rs`, `tokens_give_roles` |

## Open

- The live part has not run on a server.
- The reader has not run on a file from a DCS install: none may be copied into the
  repository. The tests' file is written in the shipped form.
- A file saved by a user's editor has not been read. One in another form is refused.
- A template whose `type` is not `"vehicle"` is refused. The `type` the editor saves
  for air, ship and train groups is UNVERIFIED. Static templates and payload presets
  are not read.
- A mission template's members go through the same reader under the 16 MiB cap of a
  mission template, where a hostile member can make a tree of about 600 MiB.
- The path's open items of docs/results/2026-11-17-template-path.md hold for this call
  too.
