# Event catalogue (E9)

| | |
|---|---|
| Target kind | docker-wine |
| DCS | 2.9.31.29434 (dcs_server.release) |
| Wine | wine-10.0 (Debian 10.0~repack-6) |
| Run | 2026-11-13T08:25:03Z |
| Script | `cargo xtask catalogue`, scenarios 10-ground, 20-ground-combat, 30-air-defense, 40-airfield, 50-marks in `lua/catalogue` |
| Recording | 830 records, 695 `EVENT`, 0 that did not decode, 0 read twice and counted once, 0 last line cut off |

Agent time per event (`hist_event`, status block at the end of the run): 695 calls, half under 2 µs, 99 % under 16 µs.

Agent time per pump call (`hist_pump`, status block at the end of the run): 58966 calls, half under 2 µs, 99 % under 16 µs.

Events dropped by the agent (`events_dropped`): 0.

Present counts the events of the type that carried the field. A shape is a Lua type, `table` for a plain table, or `object:<class>` for a table with a class's metatable. Read as names the `event_field` the allow-list reads it as (spec/records.md). Forwarded counts the agent's `EVENT` records that carried it.

## S_EVENT_BASE_CAPTURED (10)

1 seen by the recorder, 1 `EVENT` records. Causes: damage 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | damage 1 of 1 |  |  |
| `initiator` | object:Unit 1 | 1 of 1 | damage 1 of 1 |  |  |
| `initiator.id_` | number 1 | 1 of 1 | damage 1 of 1 | INITIATOR | 1 |
| `initiator_data` | table 1 | 1 of 1 | damage 1 of 1 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 1 | damage 1 of 1 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 1 | damage 1 of 1 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 1 | damage 1 of 1 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 1 | 1 of 1 | damage 1 of 1 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 1 | damage 1 of 1 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 1 | damage 1 of 1 |  |  |
| `place` | object:Airbase 1 | 1 of 1 | damage 1 of 1 |  |  |
| `place.id_` | number 1 | 1 of 1 | damage 1 of 1 | PLACE | 1 |
| `subPlace` | number 1 | 1 of 1 | damage 1 of 1 | SUBPLACE | 1 |
| `time` | number 1 | 1 of 1 | damage 1 of 1 | TIME | 1 |

Values of `initiator_data.object_category`: 1: 1. By cause: damage 1: 1.

Largest distance between `time` and model time on arrival: damage 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_BDA (37)

3 seen by the recorder, 3 `EVENT` records. Causes: combat 3.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator` | object:Unit 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator.id_` | number 3 | 3 of 3 | combat 3 of 3 | INITIATOR | 3 |
| `initiator_data` | table 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.coalition` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.mission_id` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.object_category` | number 3 | 3 of 3 | combat 3 of 3 | INITIATOR_CATEGORY | 3 |
| `initiator_data.object_id` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.unit_name` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.unit_type` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target` | object:Unit 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target.id_` | number 3 | 3 of 3 | combat 3 of 3 | TARGET | 3 |
| `target_data` | table 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.coalition` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.mission_id` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.object_category` | number 3 | 3 of 3 | combat 3 of 3 | TARGET_CATEGORY | 3 |
| `target_data.object_id` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.unit_name` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.unit_type` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `time` | number 3 | 3 of 3 | combat 3 of 3 | TIME | 3 |
| `weapon` | object:Weapon 3 | 3 of 3 | combat 3 of 3 |  |  |
| `weapon.id_` | number 3 | 3 of 3 | combat 3 of 3 | WEAPON | 3 |
| `weapon_name` | string 3 | 3 of 3 | combat 3 of 3 | WEAPON_NAME | 3 |

Values of `initiator_data.object_category`: 1: 3. By cause: combat 1: 3.

Values of `target_data.object_category`: 1: 3. By cause: combat 1: 3.

Largest distance between `time` and model time on arrival: combat 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`, `target_data.coalition`, `target_data.mission_id`, `target_data.object_id`, `target_data.unit_name`, `target_data.unit_type`.

## S_EVENT_BIRTH (15)

13 seen by the recorder, 13 `EVENT` records. Causes: air-defense 3, cold 1, combat 4, land 1, runway 1, spawn 3.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `initiator` | object:StaticObject 1, object:Unit 12 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `initiator.id_` | number 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 | INITIATOR | 13 |
| `initiator_data` | table 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `initiator_data.coalition` | number 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `initiator_data.mission_id` | string 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `initiator_data.object_category` | number 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 | INITIATOR_CATEGORY | 13 |
| `initiator_data.object_id` | number 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `initiator_data.unit_name` | string 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `initiator_data.unit_type` | string 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 |  |  |
| `place` | object:Airbase 2 | 2 of 13 | cold 1 of 1, runway 1 of 1 |  |  |
| `place.id_` | number 2 | 2 of 13 | cold 1 of 1, runway 1 of 1 | PLACE | 2 |
| `subPlace` | number 2 | 2 of 13 | cold 1 of 1, runway 1 of 1 | SUBPLACE | 2 |
| `time` | number 13 | 13 of 13 | air-defense 3 of 3, cold 1 of 1, combat 4 of 4, land 1 of 1, runway 1 of 1, spawn 3 of 3 | TIME | 13 |

Values of `initiator_data.object_category`: 1: 12, 3: 1. By cause: air-defense 1: 3; cold 1: 1; combat 1: 4; land 1: 1; runway 1: 1; spawn 1: 2, 3: 1.

Largest distance between `time` and model time on arrival: air-defense 0.000 s, cold 0.000 s, combat 0.000 s, land 0.000 s, runway 0.000 s, spawn 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_DEAD (8)

309 seen by the recorder, 309 `EVENT` records. Causes: air-defense 3, damage 31, explosion 166, static 109.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator` | object:SceneryObject 304, object:StaticObject 1, object:Unit 4 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator.id_` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 | INITIATOR | 309 |
| `initiator_data` | table 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator_data.coalition` | number 5 | 5 of 309 | air-defense 3 of 3, explosion 1 of 166, static 1 of 109 |  |  |
| `initiator_data.mission_id` | string 5 | 5 of 309 | air-defense 3 of 3, explosion 1 of 166, static 1 of 109 |  |  |
| `initiator_data.object_category` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 | INITIATOR_CATEGORY | 309 |
| `initiator_data.object_id` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator_data.unit_name` | string 5 | 5 of 309 | air-defense 3 of 3, explosion 1 of 166, static 1 of 109 |  |  |
| `initiator_data.unit_type` | string 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `time` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 | TIME | 309 |

Values of `initiator_data.object_category`: 1: 4, 3: 1, 5: 304. By cause: air-defense 1: 3; damage 5: 31; explosion 1: 1, 5: 165; static 3: 1, 5: 108.

Largest distance between `time` and model time on arrival: air-defense 0.000 s, damage 0.004 s, explosion 0.000 s, static 0.002 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_ENGINE_STARTUP (18)

4 seen by the recorder, 4 `EVENT` records. Causes: air-defense 1, cold 1, land 1, runway 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `initiator` | object:Unit 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `initiator.id_` | number 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 | INITIATOR | 4 |
| `initiator_data` | table 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.coalition` | number 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.mission_id` | string 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.object_category` | number 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 | INITIATOR_CATEGORY | 4 |
| `initiator_data.object_id` | number 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.unit_name` | string 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.unit_type` | string 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 |  |  |
| `place` | object:Airbase 2 | 2 of 4 | cold 1 of 1, runway 1 of 1 |  |  |
| `place.id_` | number 2 | 2 of 4 | cold 1 of 1, runway 1 of 1 | PLACE | 2 |
| `subPlace` | number 2 | 2 of 4 | cold 1 of 1, runway 1 of 1 | SUBPLACE | 2 |
| `time` | number 4 | 4 of 4 | air-defense 1 of 1, cold 1 of 1, land 1 of 1, runway 1 of 1 | TIME | 4 |

Values of `initiator_data.object_category`: 1: 4. By cause: air-defense 1: 1; cold 1: 1; land 1: 1; runway 1: 1.

Largest distance between `time` and model time on arrival: air-defense 0.000 s, cold 0.000 s, land 0.000 s, runway 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_HIT (2)

3 seen by the recorder, 3 `EVENT` records. Causes: combat 3.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator` | object:Unit 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator.id_` | number 3 | 3 of 3 | combat 3 of 3 | INITIATOR | 3 |
| `initiator_data` | table 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.coalition` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.mission_id` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.object_category` | number 3 | 3 of 3 | combat 3 of 3 | INITIATOR_CATEGORY | 3 |
| `initiator_data.object_id` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.unit_name` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `initiator_data.unit_type` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target` | object:Unit 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target.id_` | number 3 | 3 of 3 | combat 3 of 3 | TARGET | 3 |
| `target_data` | table 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.coalition` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.mission_id` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.object_category` | number 3 | 3 of 3 | combat 3 of 3 | TARGET_CATEGORY | 3 |
| `target_data.object_id` | number 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.unit_name` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `target_data.unit_type` | string 3 | 3 of 3 | combat 3 of 3 |  |  |
| `time` | number 3 | 3 of 3 | combat 3 of 3 | TIME | 3 |
| `weapon` | object:Weapon 3 | 3 of 3 | combat 3 of 3 |  |  |
| `weapon.id_` | number 3 | 3 of 3 | combat 3 of 3 | WEAPON | 3 |
| `weapon_name` | string 3 | 3 of 3 | combat 3 of 3 | WEAPON_NAME | 3 |

Values of `initiator_data.object_category`: 1: 3. By cause: combat 1: 3.

Values of `target_data.object_category`: 1: 3. By cause: combat 1: 3.

Largest distance between `time` and model time on arrival: combat 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`, `target_data.coalition`, `target_data.mission_id`, `target_data.object_id`, `target_data.unit_name`, `target_data.unit_type`.

## S_EVENT_KILL (28)

3 seen by the recorder, 3 `EVENT` records. Causes: air-defense 3.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator` | object:Unit 1, table 2 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator.coalition` | number 2 | 2 of 3 | air-defense 2 of 3 |  |  |
| `initiator.dead` | boolean 2 | 2 of 3 | air-defense 2 of 3 |  |  |
| `initiator.id_` | number 3 | 3 of 3 | air-defense 3 of 3 | INITIATOR | 3 |
| `initiator.misID` | string 2 | 2 of 3 | air-defense 2 of 3 |  |  |
| `initiator.unit_type` | string 2 | 2 of 3 | air-defense 2 of 3 |  |  |
| `initiator_data` | table 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator_data.coalition` | number 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator_data.mission_id` | string 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator_data.object_category` | number 3 | 3 of 3 | air-defense 3 of 3 | INITIATOR_CATEGORY | 3 |
| `initiator_data.object_id` | number 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator_data.unit_name` | string 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator_data.unit_type` | string 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `target` | object:Unit 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `target.id_` | number 3 | 3 of 3 | air-defense 3 of 3 | TARGET | 3 |
| `target_data` | table 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `target_data.coalition` | number 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `target_data.mission_id` | string 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `target_data.object_category` | number 3 | 3 of 3 | air-defense 3 of 3 | TARGET_CATEGORY | 3 |
| `target_data.object_id` | number 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `target_data.unit_name` | string 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `target_data.unit_type` | string 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `time` | number 3 | 3 of 3 | air-defense 3 of 3 | TIME | 3 |
| `weapon` | object:Weapon 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `weapon.id_` | number 3 | 3 of 3 | air-defense 3 of 3 | WEAPON | 3 |
| `weapon_name` | string 3 | 3 of 3 | air-defense 3 of 3 | WEAPON_NAME | 3 |

Values of `initiator_data.object_category`: 1: 3. By cause: air-defense 1: 3.

Values of `target_data.object_category`: 1: 3. By cause: air-defense 1: 3.

Largest distance between `time` and model time on arrival: air-defense 0.000 s.

Seen, read by no allow-list: `id`, `initiator.coalition`, `initiator.dead`, `initiator.misID`, `initiator.unit_type`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`, `target_data.coalition`, `target_data.mission_id`, `target_data.object_id`, `target_data.unit_name`, `target_data.unit_type`.

## S_EVENT_LAND (4)

1 seen by the recorder, 1 `EVENT` records. Causes: land 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator` | object:Unit 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator.id_` | number 1 | 1 of 1 | land 1 of 1 | INITIATOR | 1 |
| `initiator_data` | table 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 1 | land 1 of 1 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 1 | land 1 of 1 |  |  |
| `place` | object:Airbase 1 | 1 of 1 | land 1 of 1 |  |  |
| `place.id_` | number 1 | 1 of 1 | land 1 of 1 | PLACE | 1 |
| `subPlace` | number 1 | 1 of 1 | land 1 of 1 | SUBPLACE | 1 |
| `time` | number 1 | 1 of 1 | land 1 of 1 | TIME | 1 |

Values of `initiator_data.object_category`: 1: 1. By cause: land 1: 1.

Largest distance between `time` and model time on arrival: land 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_MARK_ADDED (25)

3 seen by the recorder, 3 `EVENT` records. Causes: line-add 1, mark-add 2.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `coalition` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | COALITION | 3 |
| `groupID` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | GROUP_ID | 3 |
| `id` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 |  |  |
| `idx` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | MARK_ID | 3 |
| `pos` | table 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 |  |  |
| `pos.x` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | POS_X | 3 |
| `pos.y` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | POS_Y | 3 |
| `pos.z` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | POS_Z | 3 |
| `text` | string 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | TEXT | 3 |
| `time` | number 3 | 3 of 3 | line-add 1 of 1, mark-add 2 of 2 | TIME | 3 |

Largest distance between `time` and model time on arrival: line-add 28799.131 s, mark-add 28799.139 s.

Allow-listed, never seen: `INITIATOR`.

Seen, read by no allow-list: `id`.

## S_EVENT_MARK_REMOVED (27)

3 seen by the recorder, 3 `EVENT` records. Causes: mark-remove 3.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `coalition` | number 3 | 3 of 3 | mark-remove 3 of 3 | COALITION | 3 |
| `groupID` | number 3 | 3 of 3 | mark-remove 3 of 3 | GROUP_ID | 3 |
| `id` | number 3 | 3 of 3 | mark-remove 3 of 3 |  |  |
| `idx` | number 3 | 3 of 3 | mark-remove 3 of 3 | MARK_ID | 3 |
| `pos` | table 3 | 3 of 3 | mark-remove 3 of 3 |  |  |
| `pos.x` | number 3 | 3 of 3 | mark-remove 3 of 3 | POS_X | 3 |
| `pos.y` | number 3 | 3 of 3 | mark-remove 3 of 3 | POS_Y | 3 |
| `pos.z` | number 3 | 3 of 3 | mark-remove 3 of 3 | POS_Z | 3 |
| `text` | string 3 | 3 of 3 | mark-remove 3 of 3 | TEXT | 3 |
| `time` | number 3 | 3 of 3 | mark-remove 3 of 3 | TIME | 3 |

Largest distance between `time` and model time on arrival: mark-remove 28796.125 s.

Allow-listed, never seen: `INITIATOR`.

Seen, read by no allow-list: `id`.

## S_EVENT_MISSION_END (12)

0 seen by the recorder, 2 `EVENT` records. Causes: none recorded.

Seen only in the stream: the Lua recorder was not running, so there is no field-presence data for this type.

Forwarded fields: TIME 2.

## S_EVENT_MISSION_RESTART (52)

0 seen by the recorder, 2 `EVENT` records. Causes: none recorded.

Seen only in the stream: the Lua recorder was not running, so there is no field-presence data for this type.

Forwarded fields: TIME 2.

## S_EVENT_MISSION_START (11)

0 seen by the recorder, 2 `EVENT` records. Causes: none recorded.

Seen only in the stream: the Lua recorder was not running, so there is no field-presence data for this type.

Forwarded fields: TIME 2.

## S_EVENT_RUNWAY_TAKEOFF (54)

2 seen by the recorder, 2 `EVENT` records. Causes: cold 1, runway 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator` | object:Unit 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator.id_` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | INITIATOR | 2 |
| `initiator_data` | table 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.coalition` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.mission_id` | string 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.object_category` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | INITIATOR_CATEGORY | 2 |
| `initiator_data.object_id` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.unit_name` | string 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.unit_type` | string 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `place` | object:Airbase 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `place.id_` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | PLACE | 2 |
| `subPlace` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | SUBPLACE | 2 |
| `time` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | TIME | 2 |

Values of `initiator_data.object_category`: 1: 2. By cause: cold 1: 1; runway 1: 1.

Largest distance between `time` and model time on arrival: cold 0.000 s, runway 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_RUNWAY_TOUCH (55)

1 seen by the recorder, 1 `EVENT` records. Causes: land 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator` | object:Unit 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator.id_` | number 1 | 1 of 1 | land 1 of 1 | INITIATOR | 1 |
| `initiator_data` | table 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 1 | land 1 of 1 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 1 | land 1 of 1 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 1 | land 1 of 1 |  |  |
| `place` | object:Airbase 1 | 1 of 1 | land 1 of 1 |  |  |
| `place.id_` | number 1 | 1 of 1 | land 1 of 1 | PLACE | 1 |
| `subPlace` | number 1 | 1 of 1 | land 1 of 1 | SUBPLACE | 1 |
| `time` | number 1 | 1 of 1 | land 1 of 1 | TIME | 1 |

Values of `initiator_data.object_category`: 1: 1. By cause: land 1: 1.

Largest distance between `time` and model time on arrival: land 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_SCORE (29)

3 seen by the recorder, 3 `EVENT` records. Causes: air-defense 3.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `comment` | string 3 | 3 of 3 | air-defense 3 of 3 | COMMENT | 3 |
| `id` | number 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator` | object:Unit 1, table 2 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator.coalition` | number 2 | 2 of 3 | air-defense 2 of 3 |  |  |
| `initiator.dead` | boolean 2 | 2 of 3 | air-defense 2 of 3 |  |  |
| `initiator.id_` | number 3 | 3 of 3 | air-defense 3 of 3 | INITIATOR | 3 |
| `initiator.misID` | string 2 | 2 of 3 | air-defense 2 of 3 |  |  |
| `initiator_data` | table 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 3 | air-defense 1 of 3 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 3 | air-defense 1 of 3 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 3 | air-defense 1 of 3 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 3 | 3 of 3 | air-defense 3 of 3 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 3 | air-defense 1 of 3 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 3 | air-defense 1 of 3 |  |  |
| `time` | number 3 | 3 of 3 | air-defense 3 of 3 | TIME | 3 |

Values of `initiator_data.object_category`: 1: 1. By cause: air-defense 1: 1.

Largest distance between `time` and model time on arrival: air-defense 0.000 s.

Seen, read by no allow-list: `id`, `initiator.coalition`, `initiator.dead`, `initiator.misID`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_SHOOTING_END (24)

1 seen by the recorder, 1 `EVENT` records. Causes: air-defense 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator` | object:Unit 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator.id_` | number 1 | 1 of 1 | air-defense 1 of 1 | INITIATOR | 1 |
| `initiator_data` | table 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 1 | air-defense 1 of 1 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `time` | number 1 | 1 of 1 | air-defense 1 of 1 | TIME | 1 |
| `weapon_name` | string 1 | 1 of 1 | air-defense 1 of 1 | WEAPON_NAME | 1 |

Values of `initiator_data.object_category`: 1: 1. By cause: air-defense 1: 1.

Largest distance between `time` and model time on arrival: air-defense 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_SHOOTING_START (23)

1 seen by the recorder, 1 `EVENT` records. Causes: air-defense 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator` | object:Unit 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator.id_` | number 1 | 1 of 1 | air-defense 1 of 1 | INITIATOR | 1 |
| `initiator_data` | table 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.coalition` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.mission_id` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.object_category` | number 1 | 1 of 1 | air-defense 1 of 1 | INITIATOR_CATEGORY | 1 |
| `initiator_data.object_id` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.unit_name` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `initiator_data.unit_type` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `target` | object:Unit 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `target.id_` | number 1 | 1 of 1 | air-defense 1 of 1 | TARGET | 1 |
| `target_data` | table 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `target_data.coalition` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `target_data.mission_id` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `target_data.object_category` | number 1 | 1 of 1 | air-defense 1 of 1 | TARGET_CATEGORY | 1 |
| `target_data.object_id` | number 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `target_data.unit_name` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `target_data.unit_type` | string 1 | 1 of 1 | air-defense 1 of 1 |  |  |
| `time` | number 1 | 1 of 1 | air-defense 1 of 1 | TIME | 1 |
| `weapon_name` | string 1 | 1 of 1 | air-defense 1 of 1 | WEAPON_NAME | 1 |

Values of `initiator_data.object_category`: 1: 1. By cause: air-defense 1: 1.

Values of `target_data.object_category`: 1: 1. By cause: air-defense 1: 1.

Largest distance between `time` and model time on arrival: air-defense 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`, `target_data.coalition`, `target_data.mission_id`, `target_data.object_id`, `target_data.unit_name`, `target_data.unit_type`.

## S_EVENT_SHOT (1)

7 seen by the recorder, 7 `EVENT` records. Causes: air-defense 2, combat 5.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `initiator` | object:Unit 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `initiator.id_` | number 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 | INITIATOR | 7 |
| `initiator_data` | table 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `initiator_data.coalition` | number 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `initiator_data.mission_id` | string 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `initiator_data.object_category` | number 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 | INITIATOR_CATEGORY | 7 |
| `initiator_data.object_id` | number 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `initiator_data.unit_name` | string 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `initiator_data.unit_type` | string 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `target` | object:Unit 2 | 2 of 7 | air-defense 2 of 2 |  |  |
| `target.id_` | number 2 | 2 of 7 | air-defense 2 of 2 | TARGET | 2 |
| `target_data` | table 2 | 2 of 7 | air-defense 2 of 2 |  |  |
| `target_data.coalition` | number 2 | 2 of 7 | air-defense 2 of 2 |  |  |
| `target_data.mission_id` | string 2 | 2 of 7 | air-defense 2 of 2 |  |  |
| `target_data.object_category` | number 2 | 2 of 7 | air-defense 2 of 2 | TARGET_CATEGORY | 2 |
| `target_data.object_id` | number 2 | 2 of 7 | air-defense 2 of 2 |  |  |
| `target_data.unit_name` | string 2 | 2 of 7 | air-defense 2 of 2 |  |  |
| `target_data.unit_type` | string 2 | 2 of 7 | air-defense 2 of 2 |  |  |
| `time` | number 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 | TIME | 7 |
| `weapon` | object:Weapon 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 |  |  |
| `weapon.id_` | number 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 | WEAPON | 7 |
| `weapon_name` | string 7 | 7 of 7 | air-defense 2 of 2, combat 5 of 5 | WEAPON_NAME | 7 |

Values of `initiator_data.object_category`: 1: 7. By cause: air-defense 1: 2; combat 1: 5.

Values of `target_data.object_category`: 1: 2. By cause: air-defense 1: 2.

Largest distance between `time` and model time on arrival: air-defense 0.000 s, combat 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`, `target_data.coalition`, `target_data.mission_id`, `target_data.object_id`, `target_data.unit_name`, `target_data.unit_type`.

## S_EVENT_SIMULATION_FREEZE (57)

8 seen by the recorder, 8 `EVENT` records. Causes: air-defense 5, land 3.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 8 | 8 of 8 | air-defense 5 of 5, land 3 of 3 |  |  |
| `time` | number 8 | 8 of 8 | air-defense 5 of 5, land 3 of 3 | TIME | 8 |

Largest distance between `time` and model time on arrival: air-defense 0.000 s, land 0.000 s.

Seen, read by no allow-list: `id`.

## S_EVENT_SIMULATION_START (46)

0 seen by the recorder, 3 `EVENT` records. Causes: none recorded.

Seen only in the stream: the Lua recorder was not running, so there is no field-presence data for this type.

Forwarded fields: TIME 3.

## S_EVENT_SIMULATION_UNFREEZE (58)

9 seen by the recorder, 9 `EVENT` records. Causes: air-defense 5, land 3, spawn 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 9 | 9 of 9 | air-defense 5 of 5, land 3 of 3, spawn 1 of 1 |  |  |
| `time` | number 9 | 9 of 9 | air-defense 5 of 5, land 3 of 3, spawn 1 of 1 | TIME | 9 |

Largest distance between `time` and model time on arrival: air-defense 0.000 s, land 0.000 s, spawn 0.000 s.

Seen, read by no allow-list: `id`.

## S_EVENT_TAKEOFF (3)

2 seen by the recorder, 2 `EVENT` records. Causes: cold 1, runway 1.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator` | object:Unit 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator.id_` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | INITIATOR | 2 |
| `initiator_data` | table 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.coalition` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.mission_id` | string 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.object_category` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | INITIATOR_CATEGORY | 2 |
| `initiator_data.object_id` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.unit_name` | string 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `initiator_data.unit_type` | string 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `place` | object:Airbase 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 |  |  |
| `place.id_` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | PLACE | 2 |
| `subPlace` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | SUBPLACE | 2 |
| `time` | number 2 | 2 of 2 | cold 1 of 1, runway 1 of 1 | TIME | 2 |

Values of `initiator_data.object_category`: 1: 2. By cause: cold 1: 1; runway 1: 1.

Largest distance between `time` and model time on arrival: cold 0.000 s, runway 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## S_EVENT_UNIT_LOST (30)

309 seen by the recorder, 309 `EVENT` records. Causes: air-defense 3, damage 31, explosion 166, static 109.

| Field | Shapes | Present | By cause | Read as | Forwarded |
|---|---|---|---|---|---|
| `id` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator` | object:SceneryObject 304, object:StaticObject 1, object:Unit 4 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator.id_` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 | INITIATOR | 309 |
| `initiator_data` | table 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator_data.coalition` | number 5 | 5 of 309 | air-defense 3 of 3, explosion 1 of 166, static 1 of 109 |  |  |
| `initiator_data.mission_id` | string 5 | 5 of 309 | air-defense 3 of 3, explosion 1 of 166, static 1 of 109 |  |  |
| `initiator_data.object_category` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 | INITIATOR_CATEGORY | 309 |
| `initiator_data.object_id` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `initiator_data.unit_name` | string 5 | 5 of 309 | air-defense 3 of 3, explosion 1 of 166, static 1 of 109 |  |  |
| `initiator_data.unit_type` | string 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 |  |  |
| `time` | number 309 | 309 of 309 | air-defense 3 of 3, damage 31 of 31, explosion 166 of 166, static 109 of 109 | TIME | 309 |

Values of `initiator_data.object_category`: 1: 4, 3: 1, 5: 304. By cause: air-defense 1: 3; damage 5: 31; explosion 1: 1, 5: 165; static 3: 1, 5: 108.

Largest distance between `time` and model time on arrival: air-defense 0.000 s, damage 0.000 s, explosion 0.000 s, static 0.000 s.

Seen, read by no allow-list: `id`, `initiator_data.coalition`, `initiator_data.mission_id`, `initiator_data.object_id`, `initiator_data.unit_name`, `initiator_data.unit_type`.

## Facts the scenarios observed

- 50-marks: mark_added_time = 29294
- 50-marks: model_time = 503.875

## Not reached

S_EVENT_CRASH, S_EVENT_EJECTION, S_EVENT_REFUELING, S_EVENT_PILOT_DEAD, S_EVENT_TOOK_CONTROL, S_EVENT_REFUELING_STOP, S_EVENT_HUMAN_FAILURE, S_EVENT_DETAILED_FAILURE, S_EVENT_ENGINE_SHUTDOWN, S_EVENT_PLAYER_ENTER_UNIT, S_EVENT_PLAYER_LEAVE_UNIT, S_EVENT_PLAYER_COMMENT, S_EVENT_MARK_CHANGE, S_EVENT_LANDING_AFTER_EJECTION, S_EVENT_PARATROOPER_LENDING, S_EVENT_DISCARD_CHAIR_AFTER_EJECTION, S_EVENT_WEAPON_ADD, S_EVENT_TRIGGER_ZONE, S_EVENT_LANDING_QUALITY_MARK, S_EVENT_AI_ABORT_MISSION, S_EVENT_DAYNIGHT, S_EVENT_FLIGHT_TIME, S_EVENT_PLAYER_SELF_KILL_PILOT, S_EVENT_PLAYER_CAPTURE_AIRFIELD, S_EVENT_EMERGENCY_LANDING, S_EVENT_UNIT_CREATE_TASK, S_EVENT_UNIT_DELETE_TASK, S_EVENT_WEAPON_REARM, S_EVENT_WEAPON_DROP, S_EVENT_UNIT_TASK_COMPLETE, S_EVENT_UNIT_TASK_STAGE, S_EVENT_MAC_EXTRA_SCORE, S_EVENT_MISSION_WINNER, S_EVENT_MAC_LMS_RESTART, S_EVENT_HUMAN_AIRCRAFT_REPAIR_START, S_EVENT_HUMAN_AIRCRAFT_REPAIR_FINISH, S_EVENT_GROUP_CHANGE_OPTION.
