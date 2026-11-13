# Event catalogue: a scripted client (E30, E31)

| | |
|---|---|
| Target kind | docker-wine |
| DCS | 2.9.31.29434 (dcs_server.release) |
| Wine | wine-10.0 (Debian 10.0~repack-6) |
| Run | 2026-11-13, about 08:50-08:55Z, a scripted Windows client of the same DCS build joined to the server above |
| Script | `cargo xtask client join` (`lua/experiments/alone/e30-join.lua`), then `e30-take-slot.lua`, `e31-spawn.lua` and `e31-leave.lua` in the client's hook state; `link-probe --record` and `e31-record.lua` on the server; built with `cargo xtask catalogue --build` |
| Recording | 43 records, 14 `EVENT`, 0 that did not decode, 0 read twice and counted once, 0 last line cut off |

Agent time per event (`hist_event`, status block at the end of the run): 14 calls, half under 4 µs, 99 % under 8 µs.

Agent time per pump call (`hist_pump`, status block at the end of the run): 43559 calls, half under 2 µs, 99 % under 8 µs.

Events dropped by the agent (`events_dropped`): 0.

Present counts the events of the type that carried the field. A shape is a Lua type, `table` for a plain table, or `object:<class>` for a table with a class's metatable. Read as names the `event_field` the allow-list reads it as (spec/records.md). Forwarded counts the agent's `EVENT` records that carried it.

## S_EVENT_BIRTH (15)

1 seen by the recorder, 1 `EVENT` records. Causes: client 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator` | object:Unit 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator.id_` | number 1 | 1 of 1 | client 1 of 1 | INITIATOR | 1 |
| `initiator_data` | table 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 1 | client 1 of 1 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 1 | client 1 of 1 |  |  |
| `place` | object:Airbase 1 | 1 of 1 | client 1 of 1 |  |  |
| `place.id_` | number 1 | 1 of 1 | client 1 of 1 | PLACE | 1 |
| `subPlace` | number 1 | 1 of 1 | client 1 of 1 | SUBPLACE | 1 |
| `time` | number 1 | 1 of 1 | client 1 of 1 | TIME | 1 |

Values of `initiator_data.object_category`: 1: 1. By cause: client 1: 1.

Largest distance between `time` and model time on arrival: client 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_PLAYER_LEAVE_UNIT (21)

1 seen by the recorder, 1 `EVENT` records. Causes: client 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator` | object:Unit 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator.id_` | number 1 | 1 of 1 | client 1 of 1 | INITIATOR | 1 |
| `initiator_data` | table 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 1 | client 1 of 1 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 1 | client 1 of 1 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 1 | client 1 of 1 |  |  |
| `time` | number 1 | 1 of 1 | client 1 of 1 | TIME | 1 |

Values of `initiator_data.object_category`: 1: 1. By cause: client 1: 1.

Largest distance between `time` and model time on arrival: client 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_SIMULATION_FREEZE (57)

1 seen by the recorder, 1 `EVENT` records. Causes: client 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `time` | number 1 | 1 of 1 | client 1 of 1 | TIME | 1 |

Largest distance between `time` and model time on arrival: client 0.000 s.

Seen, read by no allow-list: `id`.

## S_EVENT_SIMULATION_START (46)

0 seen by the recorder, 1 `EVENT` records. Causes: none recorded.

Seen only in the stream: the Lua recorder was not running, so there is no field-presence data for this type.

Forwarded fields: TIME 1.

## S_EVENT_SIMULATION_UNFREEZE (58)

1 seen by the recorder, 1 `EVENT` records. Causes: client 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | client 1 of 1 |  |  |
| `time` | number 1 | 1 of 1 | client 1 of 1 | TIME | 1 |

Largest distance between `time` and model time on arrival: client 0.000 s.

Seen, read by no allow-list: `id`.

## S_EVENT_WEAPON_ADD (34)

9 seen by the recorder, 9 `EVENT` records. Causes: client 9.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 9 | 9 of 9 | client 9 of 9 |  |  |
| `initiator` | object:Unit 9 | 9 of 9 | client 9 of 9 |  |  |
| `initiator.id_` | number 9 | 9 of 9 | client 9 of 9 | INITIATOR | 9 |
| `initiator_data` | table 9 | 9 of 9 | client 9 of 9 |  |  |
| `initiator_data.coalition` | number 9 | 9 of 9 | client 9 of 9 |  |  |
| `initiator_data.mission_id` | string 9 | 9 of 9 | client 9 of 9 |  |  |
| `initiator_data.object_category` | number 9 | 9 of 9 | client 9 of 9 | INITIATOR_CATEGORY | 9 |
| `initiator_data.object_id` | number 9 | 9 of 9 | client 9 of 9 |  |  |
| `initiator_data.unit_name` | string 9 | 9 of 9 | client 9 of 9 |  |  |
| `initiator_data.unit_type` | string 9 | 9 of 9 | client 9 of 9 |  |  |
| `time` | number 9 | 9 of 9 | client 9 of 9 | TIME | 9 |
| `weapon_name` | string 9 | 9 of 9 | client 9 of 9 | WEAPON_NAME | 9 |

Values of `initiator_data.object_category`: 1: 9. By cause: client 1: 9.

Largest distance between `time` and model time on arrival: client 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## Not reached

S_EVENT_SHOT, S_EVENT_HIT, S_EVENT_TAKEOFF, S_EVENT_LAND, S_EVENT_CRASH, S_EVENT_EJECTION, S_EVENT_REFUELING, S_EVENT_DEAD, S_EVENT_PILOT_DEAD, S_EVENT_BASE_CAPTURED, S_EVENT_MISSION_START, S_EVENT_MISSION_END, S_EVENT_TOOK_CONTROL, S_EVENT_REFUELING_STOP, S_EVENT_HUMAN_FAILURE, S_EVENT_DETAILED_FAILURE, S_EVENT_ENGINE_STARTUP, S_EVENT_ENGINE_SHUTDOWN, S_EVENT_PLAYER_ENTER_UNIT, S_EVENT_PLAYER_COMMENT, S_EVENT_SHOOTING_START, S_EVENT_SHOOTING_END, S_EVENT_MARK_ADDED, S_EVENT_MARK_CHANGE, S_EVENT_MARK_REMOVED, S_EVENT_KILL, S_EVENT_SCORE, S_EVENT_UNIT_LOST, S_EVENT_LANDING_AFTER_EJECTION, S_EVENT_PARATROOPER_LENDING, S_EVENT_DISCARD_CHAIR_AFTER_EJECTION, S_EVENT_TRIGGER_ZONE, S_EVENT_LANDING_QUALITY_MARK, S_EVENT_BDA, S_EVENT_AI_ABORT_MISSION, S_EVENT_DAYNIGHT, S_EVENT_FLIGHT_TIME, S_EVENT_PLAYER_SELF_KILL_PILOT, S_EVENT_PLAYER_CAPTURE_AIRFIELD, S_EVENT_EMERGENCY_LANDING, S_EVENT_UNIT_CREATE_TASK, S_EVENT_UNIT_DELETE_TASK, S_EVENT_WEAPON_REARM, S_EVENT_WEAPON_DROP, S_EVENT_UNIT_TASK_COMPLETE, S_EVENT_UNIT_TASK_STAGE, S_EVENT_MAC_EXTRA_SCORE, S_EVENT_MISSION_RESTART, S_EVENT_MISSION_WINNER, S_EVENT_RUNWAY_TAKEOFF, S_EVENT_RUNWAY_TOUCH, S_EVENT_MAC_LMS_RESTART, S_EVENT_HUMAN_AIRCRAFT_REPAIR_START, S_EVENT_HUMAN_AIRCRAFT_REPAIR_FINISH, S_EVENT_GROUP_CHANGE_OPTION.
