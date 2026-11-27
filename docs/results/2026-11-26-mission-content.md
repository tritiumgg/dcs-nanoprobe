# Mission content: typed weather, briefing, files and broadcasts

## Run 1: server side

| | |
|---|---|
| DCS | 2.9.32.30490 (`dcs_server.release`, ProtocolVersion 433): the dedicated server under Wine 10.0 (`wine-10.0 (Debian 10.0~repack-6)`). It updated itself from 2.9.31.29434 when the run restarted it. This is the platform's first run on 2.9.32.30490 |
| Date | 2026-11-26, local time |
| Build | Agent `7af5109d224245fc` with the eval channel; the sidecar built from the same tree |
| Script | `cargo xtask sidecar <server> --part mission-content` (`crates/xtask/src/shell_live/mission_content.rs`) |
| Rehearsal | `crates/fake-sidecar/tests/mission_files.rs`, `crates/fake-sidecar/tests/world_commands.rs` (`broadcast_effects`), `lua/tests/dispatch.lua` (`broadcast`), the shell writer's tests and `spec/shell.golden` |
| Result | setup, next, reload, read back and cleanup PASS. The effects step stopped at its gate: no client could join (below) |

The run made a campaign on Caucasus with its own template (the shared one with a WAV and a
PNG packed and keyed), set six files with `SetMissionFile`, set the next mission's typed
weather, briefing and images, reloaded, and read the mission back through the eval
channel, each read its own request. All results are LIVE unless tagged.

### The agent on 2.9.32.30490

`target check` reads 2.9.32.30490. The hook loaded, the agent attached, and the sidecar
ran setup, `SetNextMission`, `Reload` and the eval reads on it. The hook and shim logs
have no warn or error line since the restart, the shim's capabilities record has no
false entry, and `dcs.log` has no nanoprobe warn or error line (LOG).

### Weather

| Set | `env.mission.weather` | The sim (`atmosphere`, at Anapa-Vityazevo, ground 43 m) |
|---|---|---|
| `temperature_c` -5 | `season.temperature` -5 | 281.27 K (8.1 °C) at the ground, on the template's date, 21 June |
| `qnh_mmhg` 750 | `qnh` 750 | 99,471 Pa at 43 m: 750 mmHg is 99,992 Pa at sea level |
| `visibility_m` 30,000 | `visibility.distance` 30,000 | |
| wind at the ground, 8 m/s toward 270 | `atGround` `{speed = 8, dir = 270}` | 10 m up: north 0.0, east -8.46 m/s |
| wind at 2000 m, 15 m/s toward 300 | `at2000` `{speed = 15, dir = 300}` | north 7.50, east -12.99: 15.0 m/s toward 300 degrees |
| wind at 8000 m, 25 m/s toward 320 | `at8000` `{speed = 25, dir = 320}` | north 19.15, east -16.07: 25.0 m/s toward 320 degrees |
| `ground_turbulence_mps` 2 | `groundTurbulence` 20 | |
| clouds: no preset, base 1200, thickness 800, density 9, snow | `clouds` `{base = 1200, thickness = 800, density = 9, iprecptns = 3}`, no `preset` | |
| fog off, dust off, halo auto | no `fog2`, `enable_fog` false, `enable_dust` false, `halo.preset` `auto` | |
| any field | `atmosphere_type` 0 | |

So `qnh` is mmHg, wind `speed` is m/s, and wind `dir` is the direction the wind blows
toward, measured from north: the API names it `toward_deg`. The temperature the sim
reports is not the one set. The Mission Editor bounds the temperature by the map's range
for the date (ED-CODE, `me_weather.lua` `applyTempRestrictions`,
`Terrain.getTempratureRangeByDate`). Runs 3 and 4, on a January date, show the sim does
the same (below, "Temperature").

### Briefing and images

| Read | Value |
|---|---|
| `env.mission.sortie`, `descriptionText`, the three tasks | `DictKey_sortie_5`, `DictKey_descriptionText_1`, `DictKey_descriptionBlueTask_3`, `DictKey_descriptionRedTask_2`, `DictKey_descriptionNeutralsTask_4`: the template's keys |
| `env.getValueDictByKey` of the sortie and description keys | `NP content check`; `Line one\nLine two: Привет, ñ` |
| `DCS.getMissionDescription()`, hook state | the string `Line one\nLine two: Привет, ñ` |
| `pictureFileNameB`, `R`, `N` | `{ResKey_np_6, ResKey_Action_902}`, `{ResKey_np_8}`, `{}`: the new image's key, and the template image's own key |
| `maxDictId` | 8 |

The text, new lines and non-ASCII letters included, reaches the mission through the
dictionary.

### Refusal

`SetNextMission` with rain at -5 degrees was refused `INVALID_ARGUMENT`, `INVALID_FIELD`,
before the accepted request.

### No client

The owner's client could not join: it ran 2.9.31.29434 and the server 2.9.32.30490, so
the server was not in its browser. The server registered as usual (LOG, `dcs.log`:
"Registering server as ...:10308"). The client checks move to run 2.

## Agent cost on 2.9.32.30490, idle

| | |
|---|---|
| DCS | 2.9.32.30490, the dedicated server under Wine 10.0 (`wine-10.0 (Debian 10.0~repack-6)`) |
| Date | 2026-11-26, local time |
| Case | DCS restarted into the server's default mission, paused at model 0, no players, no sidecar; 600 s of the agent's 10 s sampling records |
| Script | `bench.sh` (kept with the run's private records): `cargo xtask restart`, then the `sampling` and `fixed work reached the overhead allowance` records of `agent.log` |

| Run | Build | Median share | Frames over 250 µs | Max frame | Allowance warnings (frames) |
|---|---|---|---|---|---|
| A | 0.2.0, `027aa9b24a24ce9e` | 0.0322 % | 0 of 68,185 | 210 µs | 0 |
| A2 | 0.2.0, again | 0.0339 % | 1 of 68,174 | 486 µs | 2 (4) |
| B | this step, `7af5109d224245fc` | 0.0355 % | 2 of 68,300 | 1,050 µs | 1 (1) |

The released build logs the allowance warning on this DCS build too, and the two 0.2.0
runs differ by 5 %, as much as B differs from A2: this step adds no measured idle cost.
Every run is about a tenth of the 0.3 % share target, and the frames over 250 µs are far
under 1 %. In run 1, with the sidecar attached and the mission running, the agent logged
30 such warnings of 1 to 3 frames; whether the warning is new on 2.9.32.30490 is not
known, as no sampling record from a 2.9.31 run is kept on the server.

## Runs 2 to 4: with a client

| | |
|---|---|
| DCS | 2.9.32.30490 on the server, the dedicated server under Wine 10.0 (`wine-10.0 (Debian 10.0~repack-6)`), and on the Windows client |
| Date | 2026-11-26, local time |
| Build | Agent, sidecar and harness from this step's tree at each run: agent `7af5109d224245fc` in runs 2 and 3 and `d76ef4907ac87866` in run 4. The harness waits before `Reload` from run 3; the sidecar sends a picture's file name as its key from run 4. The client ran a development build for screenshots |
| Script | `cargo xtask sidecar <server> --part mission-content`, with `NP_CONTENT_GATE_DIR` holding each visible step for the person at the client. The broadcast by key was one eval request of the same call with the key as its file, as `lua/experiments/radio-transmission/case.lua` makes with `<file>` set to the key; that request's own file was not kept. Run 4's template adds a blue (USA) and a red (Russia) UH-1H with skill `Client`, hot start on the ground at Anapa-Vityazevo |
| Result | Run 2 failed in the harness: its `Reload` came before DCS applied the activation's load and was refused (`a mission load is owed already`), as specified; the part now waits. Runs 3 and 4 PASS on every step |

What the client showed (LIVE, with screenshots):

| Check | Seen |
|---|---|
| Weather | snow under low cloud on 15/01/2016; the briefing reads -5°, "999.92 hPa / 750 mmHg", cloud base 1200, "At GRND 8 m/s, 270° Meteo 90°" (and 300/120, 320/140 aloft), turbulence 2 m/s |
| Temperature | with the January date the sim reads -5.3 °C at the ground (267.87 K), against 8.1 °C on run 1's June date: the date bounds the temperature |
| Coalition screen and spectator briefing | the sortie and the situation, "Привет, ñ" included; no coalition task or image |
| Blue slot briefing | "My Side: USA", objective "Blue task text", the blue image as 1 of 2 and the test image as 2 of 2. Once it showed only after Fly; once (red) before it |
| Red slot briefing | "Red task text" and one red image |
| Kneeboard, UH-1H | the all-aircraft page (`KNEEBOARD/IMAGES/`) is page 1 of 60, the UH-1H page (`KNEEBOARD/UH-1H/IMAGES/`) page 60 |
| Sound by file name | heard (runs 3 and 4) |
| Picture by bare file name | run 3: not shown; by its key `ResKey_np_6`: shown. Run 4, with the sidecar sending a file name as its key from the loaded shell's `mapResource`: shown |
| Broadcast, 251,000,000 Hz AM, 1000 W, looping, by file name; again by key `ResKey_np_7` (eval) | APPLIED, but not heard on the UH-1H's UHF radio tuned to 251.000 AM. Run 5 settles the file form |
| Broadcast again under the same name, stop, stop of an unknown name | APPLIED; what DCS does with them is not observed |

## Run 5: the broadcast's form and the coalition selection image

| | |
|---|---|
| DCS | 2.9.32.30490 on the server, under Wine 10.0, and on the Windows client |
| Date | 2026-11-26, local time |
| Build | Agent `72e9ae02f582eb9d` with the eval channel; the sidecar from the same tree |
| Script | `cargo xtask sidecar <server> --part mission-content` with `NP_CONTENT_GATE_DIR`; each broadcast case is `lua/experiments/radio-transmission/case.lua` with its placeholders filled, one case per eval request in the `server` state, then `stop.lua` |
| Result | PASS on every step. The person at the client sat in the blue UH-1H, hot on the ground at Anapa-Vityazevo. The part's own broadcast step still sent the bare file name (APPLIED, not listened to): this build did not yet send the path |

`NextMission.briefing_images.coalition_selection` set to the blue PNG wrote
`pictureFileNameServer`, and the coalition selection screen showed the blue image
(LIVE). In the client's shipped Lua only the Mission Editor reads the field (ED-CODE,
`MissionEditor/modules/me_mission.lua`, `me_setImage.lua`).

Three cases ran, numbered as they were prepared; c2 to c6 (kHz, MHz, FM on UHF, 10,000 W,
500 m) were not needed once c1 was heard. Each case stops every earlier one, then calls `trigger.action.radioTransmission(file,
point, modulation, true, frequency, power, name)` 50 m above the aircraft, at 1000 W:

| Case | File | Frequency | Modulation | Radio | Heard |
|---|---|---|---|---|---|
| c1 | `l10n/DEFAULT/np-new.wav` | 251,000,000 | 0 | UHF, 251.000 AM | yes |
| c7 | `np-new.wav` | 251,000,000 | 0 | UHF, 251.000 AM | no |
| c8 | `l10n/DEFAULT/np-new.wav` | 40,000,000 | 1 | FM, 40.00 | yes |

So DCS takes the frequency in Hz, modulation 0 is AM and 1 is FM, and the file is given
by its path in the mission; given by its bare name it is not heard, as in run 4. c7's
stop of c1 silenced it: `stopRadioTransmission` stops a playing broadcast.

From this result the sidecar sends a broadcast's file name as its `l10n/DEFAULT/` path
from the applied shell, so the agent makes c1's call. A host test checks the helper
that maps the name (`Inner::sound_path`); a client has not yet heard a broadcast sent
through `DoEffect` with it (UNVERIFIED).
