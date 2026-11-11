-- The DCS API names the strict mock knows exist, per Lua state and namespace.
--
-- A call to a name not listed here raises "not in the listing". A listed name
-- the mock does not implement raises "not mocked". The two errors differ so a
-- test failure says whether the code under test is wrong or the mock is short.
--
-- Version 0 is written by hand and covers only the namespaces the mock
-- implements. Every name is PRIOR: recorded in an in-process listing of DCS
-- 2.9.31.29434 on a Windows client hosting a multiplayer session, not
-- re-checked here. The S2 api-dump probe regenerates this listing with its
-- script, build and date.

local M = {}

M.build = '2.9.31.29434'

M.gui = {
  DCS = {
    'getModelTime', 'getRealTime', 'getPause', 'setPause', 'setUserCallbacks',
    'getMissionName', 'getMissionFilename', 'isServer', 'isMultiplayer',
    'getUnitPositionByObjectId', 'getUnitProperty', 'restartMission',
    'stopMission', 'startMission', 'exitProcess', 'exportToMiz', 'saveMissionTo',
    'getTaintedFiles',
  },
  net = {
    'dostring_in', 'log', 'load_mission', 'get_player_list', 'get_player_info',
    'get_my_player_id', 'get_server_id', 'send_chat', 'lua2json', 'json2lua',
  },
  log = {
    'write', 'info', 'warning', 'error', 'debug', 'alert',
    'set_output', 'set_output_rules',
  },
  lfs = { 'writedir', 'currentdir', 'attributes', 'dir', 'mkdir', 'rmdir', 'tempdir' },
}

M.server = {
  timer = {
    'getTime', 'getAbsTime', 'getTime0', 'getPause', 'scheduleFunction',
    'removeFunction', 'setFunctionTime',
  },
  world = {
    'addEventHandler', 'removeEventHandler', 'onEvent', 'getAirbases',
    'getMarkPanels', 'getPlayer', 'removeJunk', 'searchObjects',
    'runPersistenceHandlers', 'setPersistenceHandler',
  },
  env = {
    'info', 'warning', 'error', 'getMissionName', 'getValueDictByKey', 'getMode',
  },
  log = {
    'write', 'info', 'warning', 'error', 'debug', 'alert',
    'set_output', 'set_output_rules',
  },
  DCS = { 'getModelTime', 'getRealTime', 'getPause', 'getUnitPositionByObjectId' },
}

-- `scripting` names the same state as `server` (LIVE, notes on states).
M.scripting = M.server

return M
