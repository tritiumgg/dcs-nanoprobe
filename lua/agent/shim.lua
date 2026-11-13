-- The shim, run in the server state by the hook (lua/agent/hook.lua) with the
-- agent's table, the install root, its own version folder, and the shim it
-- replaces, or nil. It returns the shim's table, which the hook keeps in the
-- global __nanoprobe_shim.
--
-- The server state outlives missions, so the shim loads once per process.
-- The hook calls start() on every simulation start; a mission load clears
-- what a mission registered, and start() is where the shim registers it
-- again. A reload runs a newer shim.lua over this one without restarting
-- DCS: the new shim gets this one's table and carries its state on. The DLL
-- stays the one loaded.
--
-- Nothing here may raise into DCS: the hook runs it under net.dostring_in,
-- which catches a raise, and every call it makes into DCS is under pcall.

local agent, root, dir, previous = ...

-- The agent's interface version this shim was written for.
local ABI = '3'
if type(agent) ~= 'table' or agent.abi ~= ABI then
  error('the shim needs agent ABI ' .. ABI .. '; the DLL has '
    .. tostring(type(agent) == 'table' and agent.abi or agent), 0)
end

local logmodule = assert(loadfile(dir .. 'lua/log.lua'))()

local M = {
  agent = agent,
  -- Simulation starts this process has seen, across reloads of the shim.
  starts = previous and previous.starts or 0,
  -- Reloads of the shim in this process.
  reloads = previous and (previous.reloads or 0) + 1 or 0,
  dir = dir,
}

M.file = logmodule.open({
  path = root .. 'logs/shim.log',
  target = 'shim',
  model = timer.getTime,
  gen = function()
    return agent.status().gen
  end,
  size = function(path)
    return lfs.attributes(path, 'size')
  end,
  fallback = function(message)
    pcall(env.error, 'nanoprobe shim: ' .. message)
  end,
  forward = function(level, msg, data)
    local args = {}
    for k, v in pairs(data or {}) do
      args[#args + 1] = tostring(k)
      args[#args + 1] = v
    end
    agent.log(level, msg, 'shim', unpack(args))
  end,
})

if previous then
  if previous.file then
    pcall(previous.file.close)
  end
  M.file.write('info', 'shim reloaded', { dir = dir, reloads = M.reloads })
else
  M.file.write('info', 'shim loaded', { dir = dir })
end

-- What the platform calls in the server state (spec/records.md,
-- "Capabilities"). Later steps call more of it; the probe reports each name
-- as present or absent, so a DCS update that removes one is named.
local CAPABILITIES = {
  'timer.getTime',
  'world.addEventHandler',
  'world.removeEventHandler',
  'coalition.addGroup',
  'coalition.addStaticObject',
  'Group.getByName',
  'Unit.getByName',
  'StaticObject.getByName',
  'Airbase.getByName',
  'Object.getPoint',
  'Object.getPosition',
  'Unit.getLife',
  'Unit.getFuel',
  'Unit.getAmmo',
  'Unit.getObjectID',
  'land.getHeight',
  'trigger.action.explosion',
  'trigger.action.outText',
  'trigger.action.markToAll',
  'missionCommands.addCommand',
  'env.mission',
  'env.error',
  'lfs.attributes',
  'package.loadlib',
}

-- Whether each dotted name is present in this state, without raising for a
-- name that is absent.
local function probe(names)
  local found = {}
  for _, name in ipairs(names) do
    local ok, present = pcall(function()
      local v = _G
      for part in name:gmatch('[^.]+') do
        if type(v) ~= 'table' then
          return false
        end
        v = v[part]
      end
      return v ~= nil
    end)
    found[name] = ok and present or false
  end
  return found
end

-- World events (spec/records.md, "Layout: EVENT" and "Allow-lists").
--
-- ED's dispatcher calls every handler with no pcall (ED-CODE,
-- Scripts/World/EventHandlers.lua), so a raise here would hide the event
-- from every later handler. The handler's whole body runs under pcall. It
-- reads fields with rawget, which calls no metamethod, and never calls a
-- method on the event or on an object in it.

-- Each event_field: its code, the event's key, the key inside an object or a
-- plain table at that key or nil, and the Lua type the value must have.
M.FIELDS = {
  TIME = { 1, 'time', nil, 'number' },
  INITIATOR = { 2, 'initiator', 'id_', 'number' },
  TARGET = { 3, 'target', 'id_', 'number' },
  WEAPON = { 4, 'weapon', 'id_', 'number' },
  WEAPON_NAME = { 5, 'weapon_name', nil, 'string' },
  PLACE = { 6, 'place', 'id_', 'number' },
  SUBPLACE = { 7, 'subPlace', nil, 'number' },
  TEXT = { 8, 'text', nil, 'string' },
  MARK_ID = { 9, 'idx', nil, 'number' },
  POS_X = { 10, 'pos', 'x', 'number' },
  POS_Y = { 11, 'pos', 'y', 'number' },
  POS_Z = { 12, 'pos', 'z', 'number' },
  COALITION = { 13, 'coalition', nil, 'number' },
  GROUP_ID = { 14, 'groupID', nil, 'number' },
  COMMENT = { 15, 'comment', nil, 'string' },
  INITIATOR_CATEGORY = { 17, 'initiator_data', 'object_category', 'number' },
  TARGET_CATEGORY = { 18, 'target_data', 'object_category', 'number' },
}

-- The fields read per event type, by world.event name. A type not named
-- here is forwarded with its ID only. spec/records.md, "Allow-lists", holds
-- the same table; lua/tests/events.lua checks the two agree.
M.ALLOW = {
  S_EVENT_SHOT = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'TARGET', 'TARGET_CATEGORY', 'WEAPON', 'WEAPON_NAME' },
  S_EVENT_HIT = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'TARGET', 'TARGET_CATEGORY', 'WEAPON', 'WEAPON_NAME' },
  S_EVENT_TAKEOFF = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_LAND = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_CRASH = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_EJECTION = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'TARGET', 'TARGET_CATEGORY' },
  S_EVENT_REFUELING = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_DEAD = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_PILOT_DEAD = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_BASE_CAPTURED = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_MISSION_START = { 'TIME' },
  S_EVENT_MISSION_END = { 'TIME' },
  S_EVENT_REFUELING_STOP = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_BIRTH = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_HUMAN_FAILURE = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_ENGINE_STARTUP = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_ENGINE_SHUTDOWN = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_PLAYER_ENTER_UNIT = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_PLAYER_LEAVE_UNIT = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_PLAYER_COMMENT = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'COMMENT' },
  S_EVENT_SHOOTING_START = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'TARGET', 'TARGET_CATEGORY', 'WEAPON_NAME' },
  S_EVENT_SHOOTING_END = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'WEAPON_NAME' },
  S_EVENT_MARK_ADDED = { 'TIME', 'INITIATOR', 'MARK_ID', 'TEXT', 'POS_X', 'POS_Y', 'POS_Z', 'COALITION', 'GROUP_ID' },
  S_EVENT_MARK_CHANGE = { 'TIME', 'INITIATOR', 'MARK_ID', 'TEXT', 'POS_X', 'POS_Y', 'POS_Z', 'COALITION', 'GROUP_ID' },
  S_EVENT_MARK_REMOVED = { 'TIME', 'INITIATOR', 'MARK_ID', 'TEXT', 'POS_X', 'POS_Y', 'POS_Z', 'COALITION', 'GROUP_ID' },
  S_EVENT_KILL = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'TARGET', 'TARGET_CATEGORY', 'WEAPON', 'WEAPON_NAME' },
  S_EVENT_SCORE = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'COMMENT' },
  S_EVENT_UNIT_LOST = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_LANDING_AFTER_EJECTION = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_DISCARD_CHAIR_AFTER_EJECTION = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'TARGET', 'TARGET_CATEGORY' },
  S_EVENT_WEAPON_ADD = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'WEAPON_NAME' },
  S_EVENT_LANDING_QUALITY_MARK = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'COMMENT' },
  S_EVENT_BDA = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'TARGET', 'TARGET_CATEGORY', 'WEAPON', 'WEAPON_NAME' },
  S_EVENT_AI_ABORT_MISSION = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_RUNWAY_TAKEOFF = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_RUNWAY_TOUCH = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY', 'PLACE', 'SUBPLACE' },
  S_EVENT_PLAYER_SELF_KILL_PILOT = { 'TIME', 'INITIATOR', 'INITIATOR_CATEGORY' },
  S_EVENT_SIMULATION_FREEZE = { 'TIME' },
  S_EVENT_SIMULATION_UNFREEZE = { 'TIME' },
  S_EVENT_MISSION_RESTART = { 'TIME' },
  S_EVENT_SIMULATION_START = { 'TIME' },
}

-- The allow-lists by event ID, as lists of FIELDS entries, from this
-- state's world.event. A name this DCS lacks is left out.
local function by_id()
  local out = {}
  local ok, events = pcall(function()
    return world.event
  end)
  if not ok or type(events) ~= 'table' then
    return out
  end
  for name, list in pairs(M.ALLOW) do
    local id = rawget(events, name)
    if type(id) == 'number' then
      local fields = {}
      for i, field in ipairs(list) do
        fields[i] = M.FIELDS[field]
      end
      out[id] = fields
    end
  end
  return out
end

-- Filled when the handler is registered, once world.event exists.
local BY_ID = {}
-- The arguments to agent.event, reused for every event.
local args = {}

local function forward(event)
  if type(event) ~= 'table' then
    return
  end
  local id = rawget(event, 'id')
  if type(id) ~= 'number' then
    return
  end
  args[1] = id
  local n = 1
  local fields = BY_ID[id]
  if fields then
    for i = 1, #fields do
      local f = fields[i]
      local v = rawget(event, f[2])
      if f[3] ~= nil then
        if type(v) == 'table' then
          v = rawget(v, f[3])
        else
          v = nil
        end
      end
      if type(v) == f[4] then
        args[n + 1] = f[1]
        args[n + 2] = v
        n = n + 2
      end
    end
  end
  agent.event(unpack(args, 1, n))
end

-- Errors the handler caught in this process, and how many it logs.
M.event_errors = previous and previous.event_errors or 0
local LOGGED_ERRORS = 5

M.handler = {}
function M.handler:onEvent(event)
  local ok, err = pcall(forward, event)
  if not ok then
    M.event_errors = M.event_errors + 1
    if M.event_errors <= LOGGED_ERRORS then
      -- tostring and the table are built inside pcall too.
      pcall(function()
        M.file.write('error', 'event handler raised', { error = tostring(err) })
      end)
    end
  end
end

-- Register the handler once: ED's registry is keyed by the handler, so
-- adding it again is harmless (ED-CODE). The handler of the shim this one
-- replaced is removed first.
local function register()
  BY_ID = by_id()
  if previous and previous.handler and not M.replaced then
    pcall(world.removeEventHandler, previous.handler)
    M.replaced = true
  end
  local ok, err = pcall(world.addEventHandler, M.handler)
  if not ok then
    M.file.write('error', 'could not add the event handler', { error = tostring(err) })
  end
end

-- Register at load too, when this state has a world, so a shim reloaded in
-- the middle of a mission replaces the old handler at once. The hook loads
-- the shim only on a simulation start, so the events of a mission load that
-- come before it are not seen (spec/records.md, "Allow-lists").
if rawget(_G, 'world') ~= nil then
  local ok, err = pcall(register)
  if not ok then
    M.file.write('error', 'could not add the event handler', { error = tostring(err) })
  end
end

-- Called by the hook on every simulation start, and after a reload. Returns
-- a short summary for the hook's log.
function M.start()
  M.starts = M.starts + 1
  local ok, err = pcall(function()
    if M.starts == 1 or not M.probed then
      M.file.write('info', 'capabilities', probe(CAPABILITIES))
      M.probed = true
    end
    register()
  end)
  if not ok then
    M.file.write('error', 'start raised', { error = tostring(err) })
  end
  return 'start ' .. M.starts .. ' reloads ' .. M.reloads
end

return M
