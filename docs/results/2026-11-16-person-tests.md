# Person tests

| | |
|---|---|
| Step | The client checks S15 and S16 left for a person: sound playback, picture display, the client view after a damage kill, a radio menu item picked by hand, and the player name the server sees. |
| Date | 2026-11-16 |
| DCS | Server: 2.9.31.29434 dedicated server, branch `dcs_server.release`. Client: DCS 2.9.31.29434 on Windows (LOG, the client's `dcs.log`). |
| Platform | Wine server: Linux, `wine-10.0 (Debian 10.0~repack-6)` in Docker. Client: Windows, with a person at the controls who joined from the server browser. Host rehearsal: macOS on arm64. |
| Build | Agent build `365d33e8c837e512` on the server. Nothing was deployed to the client. |
| Mission | The server's own mission with the two test files of `cargo xtask miz <mission> <out> --sound-picture` packed in, loaded with `cargo xtask load`. The person flew a UH-1H at a FARP. |
| Script | `cargo xtask eval <server> <state> <file>` for each snippet in `lua/experiments/person/` |
| Rehearsal | `lua/tests/person_experiments.lua`, run by `mise run check` |
| Result | Every test has a verdict. The menu callback runs from a person's pick with only the script's argument, so by spec/records.md the record's `player` is 0 (AUDIENCE); the record was not read. A coalition picture given the coalition as a number shows nothing, which is the form the agent's `PICTURE` sends (open item). |

All results are LIVE unless tagged. What the client showed or played is the person's report.
What the server read is the snippet's eval output.

## Sound

`np-tone.wav` (0.5 s, 440 Hz, packed in the mission) played 3 times, 1 s apart, with each
form. The file was given by its name in the mission.

| Snippet | Call | Heard |
|---|---|---|
| `sound-all.lua` | `trigger.action.outSound(file)` | Yes, 3 tones |
| `sound-coalition.lua` | `outSoundForCoalition(2, file)` | Yes |
| `sound-group.lua` | `outSoundForGroup(<group ID>, file)` | Yes |

## Picture

`np-picture.png` (8 by 8, packed in the mission) was shown through
`getValueResourceByKey("ResKey_Action_902")`. The arguments came in the agent's `PICTURE`
order, centered, at 30 % of the window: file, seconds, clear view, start delay 0,
alignment 1 and 1, size 30, size units 1. The Mission Editor's last field, picture effect,
was not passed, as the agent does not pass it.

| Snippet | Call | Seen |
|---|---|---|
| `picture-all.lua` | `a_out_picture(file, 10, false, ...)` | Yes, centered: a square with red and blue squares inside. It cleared after a short time; the person did not time it. |
| `picture-coalition.lua` | `a_out_picture_s(2, file, 10, false, ...)` | Nothing |
| `picture-coalition-name.lua` | `a_out_picture_s("blue", file, 10, false, ...)` | Yes. It cleared after about 10 s. |
| `picture-group.lua` | `a_out_picture_g(<group ID>, file, 10, false, ...)` | Yes |
| `picture-hold.lua` | `a_out_picture(file, 0, false, ...)` | Yes, and still up after about 15 s |
| `picture-stop.lua` | `a_out_picture_stop()` | The held picture cleared |
| `picture-clearview.lua` | `a_out_picture(file, 10, true, ...)` | Yes, with no visible difference from clear view off |

`a_out_picture_s` showed the picture for `"blue"` and nothing for 2, one run each. The
call with 2 returned without error. `"red"` was not tried. The agent's `PICTURE` was not
sent in this test. `lua/agent/hook.lua` passes the coalition target as a number (scope 2)
and the country target as a number (scope 5), so a coalition-scoped `PICTURE` is expected
to show nothing (ED-CODE reading plus the LIVE result above). The sound and stop snippets
called `trigger.action` and `a_out_picture_stop` directly, not through the agent's `SOUND`
and `PICTURE_STOP`, which make the same calls.

## Damage kill

`kill-spawn.lua` added an M-113 in front of the player's aircraft. The first run had
`DISTANCE = 150`, the only difference from the committed file; that unit sat under the FARP
pad, out of sight, so `kill-clear.lua` removed it and a second run at 400 m added one the
person saw. `kill-damage.lua` set its life to 0 with `a_unit_set_life_percentage`, the
setter of the `DAMAGE` command. It read `getLife()` 3 (the raw value, not a percentage) before and was gone by name 1 s
after. On
the client the unit vanished. No wreck was left, and no kill or death message appeared.
The server may hide death messages in its settings, which this test did not read
(UNVERIFIED).

## Menu callback

`menu-add.lua` added `np-person-all` (`missionCommands.addCommand`) and `np-person-group`
(`addCommandForGroup`, the player's group). The person saw both under F10 Other in the
radio menu and picked each by hand. `menu-read.lua` found that each callback ran once.
Each received one argument, the string the script gave `addCommand`, and nothing that
names the player. The MENU record's `player` is therefore 0, and the sidecar reports
the selection with the attribution `AUDIENCE` (spec/records.md, "Layout: MENU"). The
callbacks also called the agent's `menu` entry under `pcall`, as the shim's own callback
does; that call's outcome was not recorded (UNVERIFIED). The record itself was not read,
since no sidecar was attached.

In S16's third attempt, the commands sent with `Export.LoSetCommand` from the client's hook
state ran no callback. The first attempt sent nothing, and the second attempt's result is
unknown (docs/results/2026-11-15-s16-players-moderation.md). A person's pick runs the
callback, so the scripted input route is the likely cause. Whether that route drives input
at all remains UNVERIFIED.

## Player name

`player-name.lua` read `net.get_player_info(id, 'name')` on the server. It gives the
client's name as set in that client's DCS. It does not read `np-client` or any other
name the scripted client uses.

## Open items

- The agent's `PICTURE` with coalition scope passes the coalition as a number, the form
  that showed nothing. `a_out_picture_s` showed the picture for `"blue"` (LIVE, one run);
  `"red"` is UNVERIFIED. The mock (`lua/mock/world.lua`) takes the number and changes with
  the agent. For the step that fixes `PICTURE`.
- The country form (`a_out_picture_c`, scope 5, sent as a number), the unit form
  (`a_out_picture_u`, scope 4) and the sound unit and country forms were not tried on a
  client. Whether the country form needs a name is UNVERIFIED. For the same step.
- Targeting is not shown: each targeted sound and picture went to the person's own
  coalition or group. Whether a client outside the target is left out is UNVERIFIED; it
  needs a call aimed at the other coalition or another group.
- What `clear_view` changes is UNVERIFIED: a single picture showed no difference.
- Whether a client plays a sound or shows a picture whose file is not packed in the
  mission (a profile path, a missing name) is UNVERIFIED on a client.
- The MENU record and its attribution through a running sidecar, from a person's pick,
  were not read: the attribution above follows from the callback's arguments and the spec.
