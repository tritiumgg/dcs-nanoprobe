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
local ABI = '8'
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
  'Object.isExist',
  'Object.getName',
  'Object.getTypeName',
  'Object.getCategory',
  'Unit.getDesc',
  'Unit.getLife0',
  'Unit.getGroup',
  'Unit.getCoalition',
  'Unit.getCountry',
  'Group.getID',
  'Group.getName',
  'Group.getUnits',
  'StaticObject.getLife',
  'StaticObject.getDesc',
  'coalition.getGroups',
  'coalition.getStaticObjects',
  'world.getAirbases',
  'Airbase.getName',
  'Airbase.getCoalition',
  'Airbase.getWarehouse',
  'Warehouse.getInventory',
  'DCS.getUnitPositionByObjectId',
  'Unit.getLife',
  'Unit.isDead',
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

-- Sampling state (spec/sampling.md). The agent keeps the object table; the
-- shim keeps, by the slot plus one the agent gives each row, the row's object
-- ID, its DCS object, and whether it is a unit. A reloaded shim carries them
-- on, since the agent's table outlives it.
local T = previous and previous.T or {}
M.T = T
T.gen = T.gen or -1
T.ids = T.ids or {}
T.objs = T.objs or {}
T.unit = T.unit or {}
-- Each unit row's slot by object ID, for the positions fallback when DCS
-- lacks getUnitPositionByObjectId.
T.slot_of = T.slot_of or {}
T.births = T.births or {}
T.airbases = T.airbases or {}
-- Births the handler keeps at most (spec/sampling.md, "The frame").
local MAX_BIRTHS = 4096

-- The event IDs that matter to the table, from this state's world.event.
local BIRTH

-- Clear what the shim holds for a row the agent removed.
local function forget(slot)
  local id = T.ids[slot]
  if id then
    T.slot_of[id] = nil
  end
  T.ids[slot], T.objs[slot], T.unit[slot] = nil, nil, nil
end

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
  local freed = agent.event(unpack(args, 1, n))
  if freed then
    forget(freed)
  end
  -- A birth of a unit or static: keep the object for the next pump, which
  -- reads it (spec/records.md, "The object table"). Nothing is called on it
  -- here.
  if id == BIRTH then
    local data = rawget(event, 'initiator_data')
    local category = type(data) == 'table' and rawget(data, 'object_category')
    local object = rawget(event, 'initiator')
    -- 1 is a unit, 3 a static, 6 a cargo static (LIVE, S10).
    if (category == 1 or category == 3 or category == 6) and type(object) == 'table' and #T.births < MAX_BIRTHS then
      T.births[#T.births + 1] = { object, category == 1 }
    end
  end
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
  local events = rawget(_G, 'world') and rawget(world, 'event')
  if type(events) == 'table' then
    BIRTH = rawget(events, 'S_EVENT_BIRTH')
    -- The death events that remove a row (spec/records.md).
    local deaths = {}
    for _, name in ipairs({ 'S_EVENT_DEAD', 'S_EVENT_UNIT_LOST', 'S_EVENT_CRASH' }) do
      local id = rawget(events, name)
      if type(id) == 'number' then
        deaths[#deaths + 1] = id
      end
    end
    if type(agent.deaths) == 'function' then
      agent.deaths(unpack(deaths))
    end
    -- The events that ask for a read (spec/sampling.md, "Tiers"): a hit for
    -- the target's life, a shot for the shooter's ammunition, and a birth,
    -- which the next pump resolves.
    local function ids(names)
      local out = {}
      for _, name in ipairs(names) do
        local id = rawget(events, name)
        if type(id) == 'number' then
          out[#out + 1] = id
        end
      end
      return out
    end
    if type(agent.reads) == 'function' then
      agent.reads(1, unpack(ids({ 'S_EVENT_HIT' })))
      agent.reads(2, unpack(ids({ 'S_EVENT_SHOT', 'S_EVENT_SHOOTING_END' })))
      agent.reads(3, unpack(ids({ 'S_EVENT_BIRTH' })))
      -- A hit, a damage assessment or a kill asks for one check of whether
      -- its target still exists (spec/records.md, "The object table").
      agent.reads(4, unpack(ids({ 'S_EVENT_HIT', 'S_EVENT_BDA', 'S_EVENT_KILL' })))
    end
  end
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

-- The sampling pump (spec/sampling.md). The hook calls it through one
-- net.dostring_in on each sampling frame. The agent plans each batch; the
-- shim calls the getters and hands each row's values to the agent, which
-- says when the frame's budget is spent. The shim calls a sink for every
-- row of a batch, in order, with nils for a value it could not read, so the
-- agent counts rows right. A getter that raises stops its batch: the agent
-- leaves that row out until an event or an enumeration settles it.

local TIER_BIRTHS, TIER_ENUM, TIER_SUB, TIER_POS = 1, 2, 3, 4
local TIER_STOPPED, TIER_VITALS, TIER_AMMO, TIER_WAREHOUSE = 5, 6, 7, 8
local OBJECT_UNIT, OBJECT_STATIC = 1, 2
-- DCS Object.Category.CARGO: a static that can move (LIVE, S10).
local CATEGORY_CARGO = 6

-- The batch buffer: 256 entries, so the agent writes only entries that exist.
local buf = {}
for i = 1, 256 do
  buf[i] = 0
end
-- Arguments for the sinks that take a list, reused.
local list = {}
-- The row being read, for a getter that raises.
local current

local function position_of(slot)
  if T.unit[slot] then
    local f = rawget(_G, 'DCS') and rawget(DCS, 'getUnitPositionByObjectId')
    if f then
      return f(T.ids[slot])
    end
  end
  local o = T.objs[slot]
  if o then
    local p = o:getPoint()
    return p.x, p.y, p.z
  end
end

local function attitude(slot)
  local o = T.objs[slot]
  if o then
    local p = o:getPosition()
    agent.att(slot, p.x.x, p.x.y, p.x.z, p.y.x, p.y.y, p.y.z)
  end
end

-- Read a unit or static and add its row. `isunit` comes from where the
-- object was found: a birth's object category (LIVE, S9), or the list an
-- enumeration read it from. Returns nothing; a getter that raises leaves the
-- object for the next enumeration.
local function read(o, enumerated, isunit)
  local id = rawget(o, 'id_')
  if type(id) ~= 'number' then
    return
  end
  local wants, died = agent.wants(id)
  if not wants or not o:isExist() then
    return
  end
  -- An object that died in this generation is added again only when it
  -- lives: a script can set a burning unit's life (LIVE, E38).
  if died and (isunit and o:isDead() or not isunit and not (o:getLife() > 0)) then
    return
  end
  local gid, gname = 0, ''
  if isunit then
    local g = o:getGroup()
    if g then
      gid, gname = g:getID(), g:getName()
    end
  end
  local desc = o:getDesc()
  local life0 = isunit and o:getLife0() or o:getLife()
  -- A static of the cargo category can move: its position is read with the
  -- units' (LIVE, S10: Object.getCategory gives 6 for one).
  local movable = not isunit and Object.getCategory(o) == CATEGORY_CARGO
  local slot = agent.object(id, tonumber(gid) or 0, isunit and OBJECT_UNIT or OBJECT_STATIC,
    type(desc) == 'table' and desc.category or 0, o:getCoalition(), o:getCountry(), life0,
    o:getName(), o:getTypeName(), gname, enumerated, movable)
  if slot and slot > 0 then
    T.ids[slot], T.objs[slot], T.unit[slot] = id, o, isunit
    T.slot_of[id] = slot
    if not isunit then
      -- A static's position is read here and at each enumeration.
      local p = o:getPoint()
      agent.spot(id, p.x, p.y, p.z)
    end
  end
end

-- Reads that raised in this process, and how many the shim logs.
M.read_errors = previous and previous.read_errors or 0

-- Read an object under pcall, logging the first few failures.
local function try_read(o, enumerated, isunit)
  local ok, err = pcall(read, o, enumerated, isunit)
  if not ok then
    M.read_errors = M.read_errors + 1
    if M.read_errors <= LOGGED_ERRORS then
      pcall(function()
        M.file.write('error', 'reading an object raised', { error = tostring(err) })
      end)
    end
  end
end

local function births(rows)
  for _ = 1, rows do
    local n = #T.births
    if n == 0 then
      return
    end
    local birth = T.births[n]
    T.births[n] = nil
    try_read(birth[1], false, birth[2])
    if agent.item() then
      return true
    end
  end
end

-- The enumeration's steps: every coalition and group category, then every
-- coalition's statics.
local STEPS = {}
for side = 0, 2 do
  for category = 0, 4 do
    STEPS[#STEPS + 1] = { false, side, category }
  end
end
for side = 0, 2 do
  STEPS[#STEPS + 1] = { true, side }
end

-- One enumeration item. A known object costs one call into the agent,
-- which also counts the item; a new one is read, then counted. Returns
-- whether to stop.
local function visit(o, isunit)
  local id = type(o) == 'table' and rawget(o, 'id_')
  if type(id) ~= 'number' then
    return agent.item()
  end
  local slot, stop = agent.seen(id)
  if slot == 0 then
    try_read(o, true, isunit)
    return agent.item()
  end
  if slot and not isunit then
    -- A static's position is read at each enumeration.
    local ok, p = pcall(o.getPoint, o)
    if ok and type(p) == 'table' then
      agent.spot(id, p.x, p.y, p.z)
    end
  end
  return stop
end

local function enumerate(rows, start)
  if start then
    T.enum = { step = 1, li = 0, ui = 0 }
  end
  local e = T.enum
  if not e then
    return
  end
  local n = 0
  while n < rows do
    if e.units then
      e.ui = e.ui + 1
      local u = e.units[e.ui]
      if u == nil then
        e.units = nil
      else
        n = n + 1
        if visit(u, true) then
          return true
        end
      end
    elseif e.list then
      e.li = e.li + 1
      local item = e.list[e.li]
      if item == nil then
        e.list = nil
      else
        n = n + 1
        if e.statics then
          if visit(item, false) then
            return true
          end
        else
          local ok, units = pcall(item.getUnits, item)
          if ok and type(units) == 'table' then
            e.units, e.ui = units, 0
          end
          if agent.item() then
            return true
          end
        end
      end
    else
      local step = STEPS[e.step]
      if step == nil then
        T.enum = nil
        agent.enum_done()
        return
      end
      -- A coalition's static list is one call whose time grows with the
      -- statics: it runs in a batch of its own, so the batch holds no
      -- other enumeration work and `enum_step_max_us` reports its time.
      if step[1] and n > 0 then
        return
      end
      e.step = e.step + 1
      local ok, l
      if step[1] then
        ok, l = pcall(coalition.getStaticObjects, step[2])
      else
        ok, l = pcall(coalition.getGroups, step[2], step[3])
      end
      e.list, e.li, e.statics = ok and type(l) == 'table' and l or nil, 0, step[1]
      n = n + 1
      if agent.item() then
        return true
      end
      if step[1] then
        return
      end
    end
  end
end

-- An existence check (spec/records.md, "The object table"). When the object
-- no longer exists, the reads that tell a death from a removal are made at
-- once, in this pump, on a plain table holding the object ID: a killed
-- unit answers them and a removed one raises (LIVE, E38). A static has one
-- read, its life. The agent classifies the row from what they gave.
local bare = { id_ = 0 }
local function check(id)
  local slot = T.slot_of[id]
  local o = slot and T.objs[slot]
  if not o then
    agent.alive(id, false)
    return
  end
  local ok, exists = pcall(o.isExist, o)
  -- A raise says nothing about the object: the row stays until a later
  -- check finds it gone. The agent is told which: a survivor, or unknown.
  if not ok or exists then
    agent.alive(id, ok and true or false)
    return
  end
  bare.id_ = id
  local dead_ok, dead, life_ok, life = true, nil, nil, nil
  if T.unit[slot] then
    dead_ok, dead = pcall(Unit.isDead, bare)
    life_ok, life = pcall(Unit.getLife, bare)
  else
    life_ok, life = pcall(StaticObject.getLife, bare)
  end
  local freed = agent.gone(id, dead_ok, dead, life_ok, life)
  if freed then
    forget(freed)
  end
end

-- `current` while a batch reads by object ID: a getter that raises is on
-- the batch's next row, which the agent knows.
local NEXT = {}

local function positions(rows, flag)
  local pos = agent.pos
  if flag then
    -- With attitude the buffer holds slots plus one.
    for i = 1, rows do
      local slot = buf[i]
      current = slot
      local stop = pos(position_of(slot))
      attitude(slot)
      if stop then
        return true
      end
    end
    return
  end
  -- A unit's object ID, or minus its slot plus one for a static
  -- (spec/sampling.md, "Tiers"); the getter is looked up once a batch.
  local f = rawget(_G, 'DCS') and rawget(DCS, 'getUnitPositionByObjectId')
  local objs = T.objs
  current = NEXT
  for i = 1, rows do
    local v = buf[i]
    local stop
    if v > 0 and f then
      stop = pos(f(v))
    else
      local o = objs[v > 0 and T.slot_of[v] or -v]
      local p = o:getPoint()
      stop = pos(p.x, p.y, p.z)
    end
    if stop then
      return true
    end
  end
end

local function stopped(rows)
  for i = 1, rows do
    local slot = buf[i]
    current = slot
    attitude(slot)
    if agent.stopped() then
      return true
    end
  end
end

local function vitals(rows)
  for i = 1, rows do
    -- A static's entry is minus its slot plus one.
    local slot = buf[i]
    if slot < 0 then
      slot = -slot
    end
    current = slot
    local o = T.objs[slot]
    local life, fuel
    if o then
      life = o:getLife()
      if T.unit[slot] then
        fuel = o:getFuel()
      end
    end
    if agent.vit(slot, life, fuel) then
      return true
    end
  end
end

local function ammo(rows)
  for i = 1, rows do
    local slot = buf[i]
    current = slot
    list[1] = slot
    local n = 1
    local o = T.objs[slot]
    local a = o and o:getAmmo()
    if type(a) == 'table' then
      for _, entry in ipairs(a) do
        local desc = type(entry) == 'table' and rawget(entry, 'desc')
        local name = type(desc) == 'table' and rawget(desc, 'typeName')
        local count = type(entry) == 'table' and rawget(entry, 'count')
        if type(name) == 'string' and type(count) == 'number' and n < 128 then
          list[n + 1], list[n + 2] = name, count
          n = n + 2
        end
      end
    end
    if agent.ammo(unpack(list, 1, n)) then
      return true
    end
  end
end

-- The stock of a warehouse as triples, from getInventory().
local function stock(base)
  local inv = base:getWarehouse():getInventory()
  local n = 0
  if type(inv) == 'table' then
    for category, items in pairs(inv) do
      if type(category) == 'string' and type(items) == 'table' then
        for key, value in pairs(items) do
          if type(value) == 'number' and n < 3 * 4096 then
            list[n + 1], list[n + 2], list[n + 3] = category, key, value
            n = n + 3
          end
        end
      end
    end
  end
  return n
end

local function warehouses(rows)
  for i = 1, rows do
    local index = buf[i]
    local base = T.airbases[index]
    local stop
    if base then
      current = nil
      local coalition_of = base.object:getCoalition()
      local ok, n = pcall(stock, base.object)
      if ok then
        stop = agent.airbase(index, base.id, coalition_of, base.name, true, unpack(list, 1, n))
      else
        stop = agent.airbase(index, base.id, coalition_of, base.name, false)
      end
    else
      stop = agent.airbase(index, 0, 0, '', false)
    end
    if stop then
      return true
    end
  end
end

local RUN = {
  [TIER_BIRTHS] = births,
  [TIER_ENUM] = enumerate,
  [TIER_SUB] = positions,
  [TIER_POS] = positions,
  [TIER_STOPPED] = stopped,
  [TIER_VITALS] = vitals,
  [TIER_AMMO] = ammo,
  [TIER_WAREHOUSE] = warehouses,
}

-- A new mission generation: the agent's table was emptied, so the shim
-- drops what it holds and reads the mission's airbases.
local function reset(gen)
  T.gen = gen
  T.ids, T.objs, T.unit, T.births, T.airbases, T.enum = {}, {}, {}, {}, {}, nil
  T.slot_of = {}
  local ok, bases = pcall(world.getAirbases)
  if ok and type(bases) == 'table' then
    for _, base in ipairs(bases) do
      local got, name = pcall(base.getName, base)
      local id = rawget(base, 'id_')
      if got and type(name) == 'string' and type(id) == 'number' then
        T.airbases[#T.airbases + 1] = { object = base, id = id, name = name }
      end
    end
  end
end

-- The getters the agent calls itself, in the order it knows them: the
-- position getter by object ID, or false when this DCS lacks it; a unit's
-- life and fuel; a static's life; an object's point. False when this state
-- lacks one of the classes.
local FN
local function functions()
  local fast = rawget(_G, 'DCS') and rawget(DCS, 'getUnitPositionByObjectId') or false
  return { fast, Unit.getLife, Unit.getFuel, StaticObject.getLife, Object.getPoint }
end

-- Result codes (spec/commands.md, "Codes: result").
local APPLIED, UNKNOWN_OPCODE, LUA_ERROR = 0, 20, 23

-- Extensions by name: the source, the environment's hooks, and the chunk's
-- revision is the agent's (spec/commands.md, "Extensions"). The registry
-- survives a shim reload.
M.extensions = previous and previous.extensions or {}

-- The globals an extension sees: the state's, through its own table. The
-- replaced globals of spec/commands.md, "The environment", are step S16's.
local function environment()
  return setmetatable({}, { __index = _G })
end

-- Compile and run an extension's chunk. Returns its hooks, or nil and why.
local function load_extension(name, source)
  if type(source) ~= 'string' then
    return nil, 'the source is not a string'
  end
  local chunk, err = loadstring(source, '=' .. name)
  if not chunk then
    return nil, err
  end
  setfenv(chunk, environment())
  local ok, hooks = pcall(chunk)
  if not ok then
    return nil, tostring(hooks)
  end
  if hooks ~= nil and type(hooks) ~= 'table' then
    return nil, 'the chunk returned ' .. type(hooks) .. ', not a table'
  end
  return hooks or {}
end

local function stop_extension(e)
  if e and type(e.hooks.on_stop) == 'function' then
    local ok, err = pcall(e.hooks.on_stop)
    if not ok then
      M.file.write('warn', 'extension on_stop raised', { name = e.name, error = tostring(err) })
    end
  end
end

-- Command handlers by opcode. Each returns a result code, then a value or a
-- message. The agent has admitted the command and checked raw Lua.
local HANDLERS = {
  -- EXTENSION_SET: the old extension keeps running when the new one fails.
  [31] = function(a)
    local hooks, err = load_extension(a.name, a.source)
    if not hooks then
      return LUA_ERROR, nil, err
    end
    stop_extension(M.extensions[a.name])
    M.extensions[a.name] = { name = a.name, source = a.source, hooks = hooks }
    return APPLIED
  end,
  -- EXTENSION_INVOKE: the first value `invoke` returns.
  [57] = function(a)
    local e = M.extensions[a.name]
    if not e or type(e.hooks.invoke) ~= 'function' then
      return LUA_ERROR, nil, 'no extension ' .. tostring(a.name) .. ' with invoke'
    end
    local args = type(a.args) == 'table' and a.args or {}
    local ok, value = pcall(e.hooks.invoke, unpack(args, 1, table.maxn(args)))
    if not ok then
      return LUA_ERROR, nil, tostring(value)
    end
    return APPLIED, value
  end,
}

-- Run the commands the agent hands out, in ring order, until it has none.
local function commands()
  while true do
    local opcode, args = agent.command()
    if not opcode then
      return
    end
    local handler = HANDLERS[opcode]
    if not handler then
      agent.command_done(UNKNOWN_OPCODE, 'no handler in the shim')
    else
      local ok, code, value, message = pcall(handler, args or {})
      if not ok then
        agent.command_done(LUA_ERROR, tostring(code))
      elseif type(code) ~= 'number' then
        agent.command_done(LUA_ERROR, 'the handler returned no code')
      else
        agent.command_done(code, message, value)
      end
    end
  end
end

-- Run every registered extension's chunk again with a new environment, on a
-- mission start: nothing an extension held survives a load.
local function restart_extensions()
  for name, e in pairs(M.extensions) do
    local hooks, err = load_extension(name, e.source)
    if hooks then
      e.hooks = hooks
    else
      e.hooks = {}
      M.file.write('error', 'extension did not start again', { name = name, error = err })
    end
  end
end

local function pump()
  local gen, sample = agent.pump_begin(#T.births, #T.airbases, collectgarbage('count'))
  if gen == nil then
    return
  end
  if gen ~= T.gen then
    reset(gen)
    -- The airbases count for this frame's plan comes on the next pump.
  end
  -- Commands go before sampling (spec/commands.md, "Handlers").
  local ok, err = pcall(commands)
  if not ok then
    M.file.write('error', 'commands raised', { error = tostring(err) })
  end
  if not sample then
    return
  end
  if FN == nil then
    local ok, fns = pcall(functions)
    FN = ok and fns or false
    M.file.write('info', 'row reads', { by = FN and 'agent' or 'shim', by_id = FN and FN[1] ~= false or false })
  end
  -- The rows to check for existence go first: a row that is gone leaves
  -- before a tier plans a read of it.
  local checks = agent.checks(buf)
  for i = 1, checks or 0 do
    check(buf[i])
  end
  while true do
    -- With the object array and the function list, the agent reads the
    -- batches of positions without attitude and of vitals itself.
    local tier, rows, flag = agent.plan(buf, T.objs, FN or nil)
    if not tier then
      return
    end
    local run = RUN[tier]
    current = nil
    local ok, stop = pcall(run, rows, flag)
    if not ok then
      if current == NEXT then
        agent.suspect()
      elseif current then
        agent.suspect(current)
      end
    elseif stop then
      return
    end
  end
end

-- Errors the pump caught in this process, and how many it logs.
M.pump_errors = previous and previous.pump_errors or 0

-- Called by the hook on each sampling frame. Never raises.
function M.pump()
  local ok, err = pcall(pump)
  pcall(agent.pump_end)
  if not ok then
    M.pump_errors = M.pump_errors + 1
    if M.pump_errors <= LOGGED_ERRORS then
      pcall(function()
        M.file.write('error', 'pump raised', { error = tostring(err) })
      end)
    end
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
    if M.starts > 1 then
      restart_extensions()
    end
  end)
  if not ok then
    M.file.write('error', 'start raised', { error = tostring(err) })
  end
  return 'start ' .. M.starts .. ' reloads ' .. M.reloads
end

return M
