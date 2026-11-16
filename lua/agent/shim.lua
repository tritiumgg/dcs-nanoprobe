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
local ABI = '10'
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
  INITIATOR_TYPE = { 19, 'initiator_data', 'unit_type', 'string' },
  TARGET_TYPE = { 20, 'target_data', 'unit_type', 'string' },
  INITIATOR_MISSION_ID = { 21, 'initiator_data', 'mission_id', 'string' },
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

-- The type fields follow the category fields (spec/records.md, "Codes:
-- event_field"): INITIATOR_TYPE wherever INITIATOR_CATEGORY is read,
-- TARGET_TYPE wherever TARGET_CATEGORY is, and INITIATOR_MISSION_ID on a
-- birth only.
for name, list in pairs(M.ALLOW) do
  local out = {}
  for _, field in ipairs(list) do
    out[#out + 1] = field
    if field == 'INITIATOR_CATEGORY' then
      out[#out + 1] = 'INITIATOR_TYPE'
    elseif field == 'TARGET_CATEGORY' then
      out[#out + 1] = 'TARGET_TYPE'
    end
  end
  if name == 'S_EVENT_BIRTH' then
    out[#out + 1] = 'INITIATOR_MISSION_ID'
  end
  M.ALLOW[name] = out
end

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

-- The marks, drawings and menu items the shim set, by EntityId as decimal
-- text (spec/commands.md, "Marks, drawings and menu items in DCS"). They
-- outlive a shim reload and are dropped on a new mission generation.
M.marks = previous and previous.marks or {}
M.menus = previous and previous.menus or {}
-- The next DCS ID for a mark or a drawing, and the next menu order number.
-- DCS's own mark IDs are UNVERIFIED to start low, so these start at 2^30.
M.next_mark = previous and previous.next_mark or 2 ^ 30
M.next_menu = previous and previous.next_menu or 1

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

-- Queue a copy of the event for the extensions that want one; defined with
-- the extensions below.
local queue_event

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
  queue_event(event)
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
    -- the target's life, and a birth, which the next pump resolves.
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
      agent.reads(2, unpack(ids({ 'S_EVENT_BIRTH' })))
      -- A hit, a damage assessment or a kill asks for one check of whether
      -- its target still exists (spec/records.md, "The object table").
      agent.reads(3, unpack(ids({ 'S_EVENT_HIT', 'S_EVENT_BDA', 'S_EVENT_KILL' })))
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
local TIER_STOPPED, TIER_VITALS, TIER_WAREHOUSE = 5, 6, 7
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
    -- A reused slot no longer belongs to the ID it held.
    local old = T.ids[slot]
    if old and T.slot_of[old] == slot then
      T.slot_of[old] = nil
    end
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
  local o = slot and T.ids[slot] == id and T.objs[slot]
  if not o then
    -- No object here: a row the agent holds that this shim does not (its
    -- command destroyed it and the agent kept the row), or an object a
    -- command destroyed that DCS may still hold (pending). `isExist` true
    -- keeps a pending object. False takes the reads of the object-held path
    -- and gives them to `gone`, which ends a pending check and removes a row
    -- the agent still holds; a raise says nothing, so `alive` lets a pending
    -- object go and keeps a row. The class is unknown here, so a failed
    -- `Unit.getLife` falls back to the static's read.
    bare.id_ = id
    local ran, there = pcall(Object.isExist, bare)
    if ran and there == false then
      local dead_ok, dead = pcall(Unit.isDead, bare)
      local life_ok, life = pcall(Unit.getLife, bare)
      if not life_ok then
        life_ok, life = pcall(StaticObject.getLife, bare)
      end
      local freed = agent.gone(id, dead_ok, dead, life_ok, life)
      if freed then
        forget(freed)
      end
      return
    end
    agent.alive(id, ran and there == true)
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
    local life
    if o then
      life = o:getLife()
    end
    if agent.vit(slot, life) then
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
  [TIER_WAREHOUSE] = warehouses,
}

-- A new mission generation: the agent's table was emptied, so the shim
-- drops what it holds and reads the mission's airbases.
local function reset(gen)
  T.gen = gen
  T.ids, T.objs, T.unit, T.births, T.airbases, T.enum = {}, {}, {}, {}, {}, nil
  T.slot_of = {}
  -- A mission load removed every mark, drawing and menu item from DCS.
  M.marks, M.menus = {}, {}
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
-- life; a static's life; an object's point. False when this state
-- lacks one of the classes.
local FN
local function functions()
  local fast = rawget(_G, 'DCS') and rawget(DCS, 'getUnitPositionByObjectId') or false
  return { fast, Unit.getLife, StaticObject.getLife, Object.getPoint }
end

-- Result codes (spec/commands.md, "Codes: result").
local APPLIED, NOT_BOUND, UNKNOWN_OPCODE, DCS_REFUSED, LUA_ERROR = 0, 18, 20, 22, 23

-- Extensions by name (spec/commands.md, "Extensions"). The registry survives
-- a shim reload. Each holds its source, its hooks, the queue of events for
-- `on_event`, its run counters, and what it started through the agent: timers,
-- callbacks waiting to run, and menu items.
M.extensions = previous and previous.extensions or {}
-- The last timer ID an extension's `timer.scheduleFunction` returned.
M.next_timer = previous and previous.next_timer or 0

-- The agent's breadcrumb code for an extension's hook, with its name as the
-- text (spec/transport.md, "Breadcrumbs").
local CRUMB_EXTENSION = 0x20000
-- Events one extension's queue holds, and the most callbacks it holds
-- waiting.
local QUEUE_MAX = 256
-- Errors in a row after which an extension's hooks stop.
local ERRORS_STOP = 10
-- Seconds between one extension's `EXTENSION` records.
local REPORT_EVERY = 10

-- Extension hook time since the last call to the agent's budget, in
-- microseconds. A hook that runs inside a command handler is part of the
-- command's time and charges nothing here.
local charged = 0
local in_command = false

-- A fresh run state for an extension: counters, queues and what it started.
local function reset_extension(e)
  e.hooks = {}
  e.q, e.q_head, e.q_tail = {}, 1, 0
  e.skipped, e.calls, e.total_us, e.max_us = 0, 0, 0, 0
  e.errors, e.stopped, e.dead = 0, false, false
  e.timers, e.pending, e.items = {}, {}, {}
  e.next_report = 0
end

-- Write the extension's `EXTENSION` record. The agent knows the revision.
local function send_stats(e)
  if type(agent.extension) == 'function' then
    pcall(agent.extension, e.name, e.calls, e.total_us, e.max_us, 0, e.skipped, e.stopped)
  end
end

-- Count an error of the extension's hook and stop its hooks at the tenth in
-- a row, with a log record and an `EXTENSION` record.
local function hook_failed(e, what, err)
  e.errors = e.errors + 1
  pcall(function()
    M.file.write('error', 'extension hook raised', { name = e.name, hook = what, error = tostring(err), in_a_row = e.errors })
  end)
  if e.errors >= ERRORS_STOP and not e.stopped then
    e.stopped = true
    pcall(function()
      M.file.write('error', 'extension hooks stopped after errors', { name = e.name, errors = e.errors })
    end)
    send_stats(e)
  end
end

-- Call one hook of the extension under pcall, with the breadcrumb set to it,
-- and count its time. Returns whether it ran without raising, then what it
-- returned. A stopped extension runs nothing.
local function run_hook(e, what, fn, ...)
  if e.stopped then
    return false, 'the extension\'s hooks are stopped'
  end
  if type(agent.crumb) == 'function' then
    pcall(agent.crumb, CRUMB_EXTENSION, e.name)
  end
  local started = agent.clock()
  local ok, value = pcall(fn, ...)
  local us = (agent.clock() - started) * 1e6
  if type(agent.crumb) == 'function' then
    pcall(agent.crumb, 0)
  end
  e.calls = e.calls + 1
  e.total_us = e.total_us + us
  if us > e.max_us then
    e.max_us = us
  end
  if not in_command then
    charged = charged + us
  end
  if ok then
    e.errors = 0
  else
    hook_failed(e, what, value)
  end
  return ok, value
end

-- The queue of callbacks DCS made for the extension (menu commands), run
-- from the pump as hooks.
local function push_pending(e, fn, arg)
  if e.dead then
    return
  end
  if #e.pending >= QUEUE_MAX then
    table.remove(e.pending, 1)
    e.skipped = e.skipped + 1
  end
  e.pending[#e.pending + 1] = { fn, arg }
end

-- A shallow copy of a world event, field by field with no metamethod, queued
-- for each extension with an `on_event` hook. The oldest event goes when the
-- queue is full, and is counted.
queue_event = function(event)
  local queued = false
  for _, e in pairs(M.extensions) do
    if not e.stopped and not e.dead and type(e.hooks.on_event) == 'function' then
      local copy = {}
      local k, v = next(event)
      while k ~= nil do
        copy[k] = v
        k, v = next(event, k)
      end
      if e.q_tail - e.q_head + 1 >= QUEUE_MAX then
        e.q[e.q_head] = nil
        e.q_head = e.q_head + 1
        e.skipped = e.skipped + 1
      end
      e.q_tail = e.q_tail + 1
      e.q[e.q_tail] = copy
      queued = true
    end
  end
  -- The agent asks for a pump of its own while an extension has events.
  if queued and type(agent.extension_budget) == 'function' then
    agent.extension_budget(0, true)
  end
end

-- The `emit` of the spec (spec/commands.md, "emit"): the agent's entry under
-- pcall. It returns true, or false and a reason, and never raises.
local function emit(name, payload, objects)
  local ok, done, reason = pcall(agent.emit, name, payload, objects)
  if not ok then
    return false, tostring(done)
  elseif done == nil then
    return false, 'disabled'
  end
  return done, reason
end
M.emit = emit

-- A table that reads through to the state's global `name` and holds
-- `replaced` over it.
local function over(name, replaced)
  return setmetatable(replaced, {
    __index = function(_, key)
      local real = _G[name]
      return real and real[key]
    end,
  })
end

-- The paths of two menu items are the same when their names are.
local function same_path(a, b)
  if #a ~= #b then
    return false
  end
  for i = 1, #a do
    if a[i] ~= b[i] then
      return false
    end
  end
  return true
end

-- Whether `path` is `prefix` or under it.
local function under(path, prefix)
  if #path < #prefix then
    return false
  end
  for i = 1, #prefix do
    if path[i] ~= prefix[i] then
      return false
    end
  end
  return true
end

-- The globals an extension sees: the state's, through its own table, with the
-- replaced globals of spec/commands.md, "The environment". `e` is the
-- extension, or nil for an EVAL chunk, which has `emit` and nothing else
-- replaced.
local function environment(e)
  local env = { emit = emit }
  if e then
    local function refuse()
      error('an extension does not add event handlers: use on_event', 2)
    end
    env.world = over('world', { addEventHandler = refuse, removeEventHandler = refuse })
    env.timer = over('timer', {
      scheduleFunction = function(fn, arg, time)
        if type(fn) ~= 'function' or type(time) ~= 'number' then
          error('timer.scheduleFunction takes a function, an argument and a time', 2)
        end
        M.next_timer = M.next_timer + 1
        e.timers[M.next_timer] = { fn = fn, arg = arg, time = time }
        return M.next_timer
      end,
      removeFunction = function(id)
        e.timers[id] = nil
      end,
      setFunctionTime = function(id, time)
        local t = e.timers[id]
        if t and type(time) == 'number' then
          t.time = time
        end
      end,
    })
    -- A menu item the extension adds, kept with the call that removes it.
    local mc = {}
    local function track(remove, path, kind, scope)
      if type(path) == 'table' then
        e.items[#e.items + 1] = { remove = remove, path = path, kind = kind, scope = scope }
      end
      return path
    end
    local function callback(fn, arg)
      return function()
        push_pending(e, fn, arg)
      end
    end
    local function add(call, remove, kind)
      return function(...)
        local n = select('#', ...)
        local args = { ... }
        local scope
        if kind ~= 'all' then
          scope = args[1]
        end
        -- The name and the path come after the scope; a command's function
        -- and argument come last.
        local first = kind == 'all' and 1 or 2
        local fn_at = first + 2
        if type(args[fn_at]) == 'function' then
          args[fn_at] = callback(args[fn_at], args[fn_at + 1])
          args[fn_at + 1] = nil
          n = fn_at
        end
        local path = call(unpack(args, 1, n))
        return track(remove, path, kind, scope)
      end
    end
    local function remover(call, kind)
      return function(...)
        local args = { ... }
        local path = args[#args]
        local result = call(...)
        if type(path) == 'table' then
          local scope = kind ~= 'all' and args[1] or nil
          for i = #e.items, 1, -1 do
            local item = e.items[i]
            if item.kind == kind and item.scope == scope and under(item.path, path) then
              table.remove(e.items, i)
            end
          end
        end
        return result
      end
    end
    local real = function(name)
      return function(...)
        return _G.missionCommands[name](...)
      end
    end
    mc.addCommand = add(real('addCommand'), 'removeItem', 'all')
    mc.addSubMenu = add(real('addSubMenu'), 'removeItem', 'all')
    mc.addCommandForCoalition = add(real('addCommandForCoalition'), 'removeItemForCoalition', 'coalition')
    mc.addSubMenuForCoalition = add(real('addSubMenuForCoalition'), 'removeItemForCoalition', 'coalition')
    mc.addCommandForGroup = add(real('addCommandForGroup'), 'removeItemForGroup', 'group')
    mc.addSubMenuForGroup = add(real('addSubMenuForGroup'), 'removeItemForGroup', 'group')
    mc.removeItem = remover(real('removeItem'), 'all')
    mc.removeItemForCoalition = remover(real('removeItemForCoalition'), 'coalition')
    mc.removeItemForGroup = remover(real('removeItemForGroup'), 'group')
    env.missionCommands = over('missionCommands', mc)
  end
  return setmetatable(env, { __index = _G })
end

-- Compile and run an extension's chunk in its environment. Returns its
-- hooks, or nil and why.
local function load_extension(e)
  if type(e.source) ~= 'string' then
    return nil, 'the source is not a string'
  end
  local chunk, err = loadstring(e.source, '=' .. e.name)
  if not chunk then
    return nil, err
  end
  setfenv(chunk, environment(e))
  local ok, hooks = pcall(chunk)
  if not ok then
    return nil, tostring(hooks)
  end
  if hooks ~= nil and type(hooks) ~= 'table' then
    return nil, 'the chunk returned ' .. type(hooks) .. ', not a table'
  end
  return hooks or {}
end

-- Take down what the extension started in DCS: its menu items, newest first,
-- and its timers and waiting work. A call that raises is skipped.
local function clean_extension(e)
  e.dead = true
  for i = #e.items, 1, -1 do
    local item = e.items[i]
    local real = rawget(_G, 'missionCommands')
    if real then
      if item.kind == 'all' then
        pcall(real[item.remove], item.path)
      else
        pcall(real[item.remove], item.scope, item.path)
      end
    end
  end
  e.items, e.timers, e.pending = {}, {}, {}
  e.q, e.q_head, e.q_tail = {}, 1, 0
end

-- `on_stop`, then the extension's leftovers go. An extension whose hooks
-- stopped after errors gets no `on_stop`.
local function stop_extension(e)
  if not e then
    return
  end
  if type(e.hooks.on_stop) == 'function' and not e.stopped and not e.dead then
    local ok, err = run_hook(e, 'on_stop', e.hooks.on_stop)
    if not ok then
      pcall(function()
        M.file.write('warn', 'extension on_stop raised', { name = e.name, error = tostring(err) })
      end)
    end
  end
  clean_extension(e)
end

-- World commands (spec/commands.md, "Handler arguments"). A handler returns
-- the result code, the value, a message, and for a command that removes
-- objects the array of their object IDs (`destroyed`), read before the DCS
-- call and passed only with APPLIED. An object that is missing is NOT_BOUND,
-- except that a destroy of nothing is APPLIED; a DCS call that raises or
-- returns nothing is DCS_REFUSED with the error text.

-- The object IDs of a group's living units, as the agent's table names them.
local function unit_ids(group)
  local ids = {}
  local ok, units = pcall(group.getUnits, group)
  for i = 1, ok and type(units) == 'table' and #units or 0 do
    local id = rawget(units[i], 'id_')
    if type(id) == 'number' then
      ids[#ids + 1] = id
    end
  end
  return ids
end

-- Heading, pitch and roll from the forward and up vectors of a position
-- (the same as the agent's euler).
local function euler(x, y)
  local heading = math.atan2(x.z, x.x)
  if heading < 0 then
    heading = heading + 2 * math.pi
  end
  local pitch = math.asin(math.max(-1, math.min(1, x.y)))
  local rx, rz = -x.z, x.x
  local norm = math.sqrt(rx * rx + rz * rz)
  local roll = 0
  if norm > 1e-9 then
    roll = math.atan2(-(y.x * rx + y.z * rz) / norm, y.y)
  end
  return heading, pitch, roll
end

-- The captured state of a unit or a static (GROUP_CAPTURE_DESTROY, CAPTURE).
-- Each read is its own pcall: one that raises leaves its key out.
local function capture_one(o, is_unit)
  local t = { id = rawget(o, 'id_') }
  local function read(key, method)
    -- The method is looked up inside the pcall: a static has no getLife0.
    local ok, v = pcall(function()
      return o[method](o)
    end)
    if ok and v ~= nil then
      t[key] = v
    end
    return ok and v or nil
  end
  read('name', 'getName')
  read('type', 'getTypeName')
  local ok, pos = pcall(o.getPosition, o)
  if ok and type(pos) == 'table' and type(pos.p) == 'table' then
    t.x, t.y, t.z = pos.p.x, pos.p.y, pos.p.z
    if type(pos.x) == 'table' and type(pos.y) == 'table' then
      t.heading, t.pitch, t.roll = euler(pos.x, pos.y)
    end
  end
  read('life', 'getLife')
  if is_unit then
    read('life0', 'getLife0')
  end
  return t
end

local function capture_group(group)
  local out = {}
  local ok, units = pcall(group.getUnits, group)
  for i = 1, ok and type(units) == 'table' and #units or 0 do
    out[#out + 1] = capture_one(units[i], true)
  end
  return out
end

-- A warehouse of the airbase `name`, or nil and a code and message.
local function warehouse_of(name)
  local ab = Airbase.getByName(name)
  if not ab then
    return nil, NOT_BOUND, 'no airbase ' .. tostring(name)
  end
  local ok, w = pcall(ab.getWarehouse, ab)
  if not ok or not w then
    return nil, NOT_BOUND, 'airbase ' .. tostring(name) .. ' has no warehouse'
  end
  return w
end

-- Apply `delta` or `stock` (WAREHOUSE_ADJUST, WAREHOUSE_SET) to a warehouse.
-- `set` true: setItem and setLiquidAmount; false: add or remove by sign.
local function stock_to(w, stock, set)
  if type(stock) ~= 'table' then
    return
  end
  for _, group in ipairs({ 'weapon', 'aircraft' }) do
    for item, n in pairs(stock[group] or {}) do
      if set then
        w:setItem(item, n)
      elseif n > 0 then
        w:addItem(item, n)
      elseif n < 0 then
        w:removeItem(item, -n)
      end
    end
  end
  for kind, n in pairs(stock.liquids or {}) do
    if set then
      w:setLiquidAmount(kind, n)
    elseif n > 0 then
      w:addLiquid(kind, n)
    elseif n < 0 then
      w:removeLiquid(kind, -n)
    end
  end
end

local function stock_command(a, set)
  local w, code, message = warehouse_of(a.airbase)
  if not w then
    return code, nil, message
  end
  local ok, err = pcall(stock_to, w, set and a.stock or a.delta, set)
  if not ok then
    return DCS_REFUSED, nil, tostring(err)
  end
  return APPLIED
end

-- A group's controller call. `which` is setTask or setOption.
local function controller_call(name, which, ...)
  local g = Group.getByName(name)
  if not g then
    return NOT_BOUND, nil, 'no group ' .. tostring(name)
  end
  local ok, c = pcall(g.getController, g)
  if not ok or not c then
    return DCS_REFUSED, nil, ok and 'the group has no controller' or tostring(c)
  end
  local ran, err = pcall(c[which], c, ...)
  if not ran then
    return DCS_REFUSED, nil, tostring(err)
  end
  return APPLIED
end

-- Destroy a group. Returns the code, the unit IDs read before, a message.
local function destroy_group(g)
  local ids = unit_ids(g)
  local ok, err = pcall(g.destroy, g)
  if not ok then
    return DCS_REFUSED, nil, tostring(err)
  end
  return APPLIED, ids
end

-- The mission ID of a group or a unit by platform name, as a number: nil and
-- a code and message when there is none to name.
local function mission_id(class, name)
  if type(name) ~= 'string' or name == '' then
    return nil, NOT_BOUND, 'no campaign to name the object'
  end
  local o = class.getByName(name)
  if not o then
    return nil, NOT_BOUND, 'no object ' .. name
  end
  local ok, id = pcall(o.getID, o)
  id = ok and tonumber(id)
  if not id then
    return nil, DCS_REFUSED, 'the object has no mission ID'
  end
  return id
end

-- The mission ID of the group or unit a scoped command names. The agent
-- passes what its binding holds for the target: `group_id`, the DCS group ID,
-- or `row`, the slot of the unit's row, whose object handle gives the ID.
-- With neither, the platform name in `name` is looked up.
local function target_id(class, a)
  if type(a.group_id) == 'number' then
    return a.group_id
  end
  if type(a.row) == 'number' then
    local o = T.objs[a.row]
    if not o then
      return nil, NOT_BOUND, 'no object in the row'
    end
    local ok, id = pcall(o.getID, o)
    id = ok and tonumber(id)
    if not id then
      return nil, DCS_REFUSED, 'the object has no mission ID'
    end
    return id
  end
  return mission_id(class, a.name)
end

-- The mission ID of the unit in a row, as text, or nothing: the hook asks
-- for it through the server state.
function M.row_id(slot)
  local id = target_id(Unit, { row = tonumber(slot) })
  return id and string.format('%.0f', id) or ''
end

-- Marks and drawings share DCS's mark ID space. Each gets an ID from the
-- shim's counter; a replacement removes the held mark first and adds the new
-- one under a new ID.
local function new_mark_id()
  local id = M.next_mark
  M.next_mark = id + 1
  return id
end

-- Remove the held mark or drawing of `entity` from DCS and forget it. An
-- entity the shim does not hold is gone already.
local function remove_mark(entity)
  local held = M.marks[entity]
  if not held then
    return APPLIED
  end
  local ok, err = pcall(trigger.action.removeMark, held.id)
  if not ok then
    return DCS_REFUSED, nil, tostring(err)
  end
  M.marks[entity] = nil
  return APPLIED
end

-- Replace the mark or drawing of `entity`: the old one is removed, then
-- `add(id)` calls DCS with the new ID.
local function set_mark(entity, add)
  local held = M.marks[entity]
  if held then
    pcall(trigger.action.removeMark, held.id)
    M.marks[entity] = nil
  end
  local id = new_mark_id()
  local ok, err = pcall(add, id)
  if not ok then
    return DCS_REFUSED, nil, tostring(err)
  end
  M.marks[entity] = { id = id }
  return APPLIED
end

-- The mission ID of a GROUP scope's group, for a command whose scope is 3.
-- Returns nothing for another scope.
local function scope_group(a)
  if a.scope == 3 then
    return target_id(Group, a)
  end
end

-- Remove a held menu item from DCS by its scope. `item.group` is the
-- mission ID of a GROUP scope. Returns what `pcall` returns.
local function menu_remove(item)
  local scope = item.scope
  if scope == 1 then
    return pcall(missionCommands.removeItem, item.path)
  elseif scope == 2 then
    return pcall(missionCommands.removeItemForCoalition, item.target, item.path)
  end
  return pcall(missionCommands.removeItemForGroup, item.group, item.path)
end

-- The command callback: tells the agent which item was chosen. It never
-- raises into DCS, and does nothing until the agent has a `menu` entry.
local function menu_callback(item)
  return function()
    pcall(function()
      if type(agent.menu) == 'function' then
        -- The target the record holds: a coalition number, a group's
        -- `EntityId` (else its platform name), or nothing.
        local target
        if item.scope == 2 then
          target = item.target
        elseif item.scope == 3 then
          if item.target ~= nil then
            target = item.target
          else
            target = item.name
          end
        end
        agent.menu(item.entity, target, item.scope)
      end
    end)
  end
end

-- Add `item` to DCS under its parent's path and keep the path DCS returns.
-- Returns true, or false and the reason.
local function menu_add(item, parent_path)
  local scope = item.scope
  local call, first
  if scope == 1 then
    call = item.submenu and missionCommands.addSubMenu or missionCommands.addCommand
  elseif scope == 2 then
    call = item.submenu and missionCommands.addSubMenuForCoalition or missionCommands.addCommandForCoalition
    first = item.target
  else
    call = item.submenu and missionCommands.addSubMenuForGroup or missionCommands.addCommandForGroup
    first = item.group
  end
  local function make()
    if item.submenu then
      if first ~= nil then
        return call(first, item.text, parent_path)
      end
      return call(item.text, parent_path)
    end
    local callback = menu_callback(item)
    if first ~= nil then
      return call(first, item.text, parent_path, callback)
    end
    return call(item.text, parent_path, callback)
  end
  local ok, path = pcall(make)
  if not ok then
    return false, tostring(path)
  elseif type(path) ~= 'table' then
    return false, 'DCS returned no menu path'
  end
  item.path = path
  return true
end

-- The held items directly under `entity`, in the order they were first set.
local function menu_children(entity)
  local out = {}
  for _, item in pairs(M.menus) do
    if item.parent == entity then
      out[#out + 1] = item
    end
  end
  table.sort(out, function(x, y)
    return x.order < y.order
  end)
  return out
end

-- Forget every item held under `entity`, at every depth, without touching
-- DCS: DCS removed them with their submenu. The entity of each is added to
-- `dropped` when there is one.
local function menu_forget_below(entity, dropped)
  for _, child in ipairs(menu_children(entity)) do
    menu_forget_below(child.entity, dropped)
    M.menus[child.entity] = nil
    if dropped then
      dropped[#dropped + 1] = child.entity
    end
  end
end

-- Add again, under the new path of `entity`, the items held below it, in the
-- order they were first set, at every depth. An item DCS refuses is
-- forgotten with the items below it, and each is added to `dropped`.
local function menu_readd_below(item, dropped)
  for _, child in ipairs(menu_children(item.entity)) do
    local ok, why = menu_add(child, item.path)
    if ok then
      menu_readd_below(child, dropped)
    else
      menu_forget_below(child.entity, dropped)
      M.menus[child.entity] = nil
      dropped[#dropped + 1] = child.entity
      M.file.write('warn', 'menu item not added again', { entity = child.entity, error = why })
    end
  end
end

-- Whether `entity` is `ancestor` or sits under it.
local function menu_under(entity, ancestor)
  local steps = 0
  while entity and entity ~= '0' and steps < 1000 do
    if entity == ancestor then
      return true
    end
    local item = M.menus[entity]
    entity = item and item.parent
    steps = steps + 1
  end
  return false
end

-- Command handlers by opcode. Each returns a result code, then a value, a
-- message, and `destroyed`. The agent has admitted the command and checked
-- raw Lua.
local HANDLERS = {
  -- GROUP_SPAWN: the new group's ID. A group under the name is replaced;
  -- its unit IDs are `destroyed` (spec/commands.md).
  [16] = function(a)
    local old = Group.getByName(a.name)
    local ids = old and unit_ids(old) or {}
    local ok, g = pcall(coalition.addGroup, a.country, a.category, a.group)
    if not ok then
      return DCS_REFUSED, nil, tostring(g)
    elseif not g then
      return DCS_REFUSED, nil, 'addGroup returned no group'
    end
    local got, id = pcall(g.getID, g)
    if not got or type(id) ~= 'number' then
      return DCS_REFUSED, nil, 'the new group has no ID'
    end
    return APPLIED, id, nil, ids
  end,
  -- GROUP_ORDER
  [17] = function(a)
    return controller_call(a.name, 'setTask', a.task)
  end,
  -- GROUP_DESTROY: a group that is not there is destroyed already.
  [18] = function(a)
    local g = Group.getByName(a.name)
    if not g then
      return APPLIED
    end
    local code, ids, message = destroy_group(g)
    return code, nil, message, ids
  end,
  -- GROUP_CAPTURE_DESTROY: the captured state, then the destroy.
  [19] = function(a)
    local g = Group.getByName(a.name)
    if not g then
      return APPLIED
    end
    local value = capture_group(g)
    local code, ids, message = destroy_group(g)
    if code ~= APPLIED then
      return code, nil, message
    end
    return code, value, nil, ids
  end,
  -- GROUP_OPTION
  [20] = function(a)
    return controller_call(a.name, 'setOption', a.option, a.value)
  end,
  -- STATIC_SPAWN: the new static's ID. A static under the name is replaced;
  -- its ID is `destroyed`.
  [21] = function(a)
    local had, old = pcall(StaticObject.getByName, a.name)
    local old_id = had and old and rawget(old, 'id_') or nil
    local ok, s = pcall(coalition.addStaticObject, a.country, a.static)
    if not ok then
      return DCS_REFUSED, nil, tostring(s)
    elseif not s then
      return DCS_REFUSED, nil, 'addStaticObject returned no static'
    end
    local id = rawget(s, 'id_')
    if type(id) ~= 'number' then
      return DCS_REFUSED, nil, 'the new static has no ID'
    end
    local ids = {}
    if type(old_id) == 'number' and old_id ~= id then
      ids[1] = old_id
    end
    return APPLIED, id, nil, ids
  end,
  -- STATIC_DESTROY
  [22] = function(a)
    local s = StaticObject.getByName(a.name)
    if not s then
      return APPLIED
    end
    local id = rawget(s, 'id_')
    local ok, err = pcall(s.destroy, s)
    if not ok then
      return DCS_REFUSED, nil, tostring(err)
    end
    return APPLIED, nil, nil, type(id) == 'number' and { id } or {}
  end,
  -- SOUND: outSound, or the form the scope names. DCS raises nothing for a
  -- file it does not have, so APPLIED says only that DCS took the call.
  [50] = function(a)
    local file, scope = a.file, a.scope
    if type(file) ~= 'string' then
      return DCS_REFUSED, nil, 'the file is not a string'
    end
    local call, first
    if scope == 1 then
      call = trigger.action.outSound
    elseif scope == 2 then
      call, first = trigger.action.outSoundForCoalition, a.target
    elseif scope == 5 then
      call, first = trigger.action.outSoundForCountry, a.target
    elseif scope == 3 or scope == 4 then
      local id, code, message = target_id(scope == 3 and Group or Unit, a)
      if not id then
        return code, nil, message
      end
      call = scope == 3 and trigger.action.outSoundForGroup or trigger.action.outSoundForUnit
      first = id
    else
      return DCS_REFUSED, nil, 'the scope is not 1 to 5'
    end
    local ok, err
    if first ~= nil then
      ok, err = pcall(call, first, file)
    else
      ok, err = pcall(call, file)
    end
    if not ok then
      return DCS_REFUSED, nil, tostring(err)
    end
    return APPLIED
  end,
  -- MARK_SET: the mark to all, a coalition or a group, under a new DCS ID.
  [25] = function(a)
    if a.scope ~= 1 and a.scope ~= 2 and a.scope ~= 3 then
      return DCS_REFUSED, nil, 'the scope is not 1 to 3'
    end
    local group, code, message = scope_group(a)
    if a.scope == 3 and not group then
      return code, nil, message
    end
    local point = { x = a.x, y = a.y, z = a.z }
    local read_only = a.read_only and true or false
    return set_mark(a.entity, function(id)
      if a.scope == 1 then
        trigger.action.markToAll(id, a.text, point, read_only)
      elseif a.scope == 2 then
        trigger.action.markToCoalition(id, a.text, point, a.target, read_only)
      else
        trigger.action.markToGroup(id, a.text, point, group, read_only)
      end
    end)
  end,
  -- MARK_REMOVE
  [26] = function(a)
    return remove_mark(a.entity)
  end,
  -- DRAWING_SET: markupToAll with the coalition, -1 for all, and the array
  -- unpacked after the ID.
  [27] = function(a)
    if a.scope ~= 1 and a.scope ~= 2 then
      return DCS_REFUSED, nil, 'the scope is not 1 or 2'
    end
    if type(a.args) ~= 'table' then
      return DCS_REFUSED, nil, 'the arguments are not an array'
    end
    local coalition = a.scope == 1 and -1 or a.target
    return set_mark(a.entity, function(id)
      trigger.action.markupToAll(a.shape, coalition, id, unpack(a.args, 1, table.maxn(a.args)))
    end)
  end,
  -- DRAWING_REMOVE
  [28] = function(a)
    return remove_mark(a.entity)
  end,
  -- MENU_SET: the item under its parent's path. A replacement removes the
  -- old item and adds a submenu's held items again under the new path.
  [29] = function(a)
    local scope = a.scope
    if scope ~= 1 and scope ~= 2 and scope ~= 3 then
      return DCS_REFUSED, nil, 'the scope is not 1 to 3'
    end
    local group, code, message = scope_group(a)
    if scope == 3 and not group then
      return code, nil, message
    end
    local parent_path
    if a.parent ~= '0' then
      local parent = M.menus[a.parent]
      if not parent or not parent.submenu or not parent.path or parent.scope ~= scope
        or parent.target ~= a.target or parent.group ~= group then
        return NOT_BOUND, nil, 'no submenu ' .. tostring(a.parent) .. ' in this scope'
      end
      if menu_under(a.parent, a.entity) then
        return NOT_BOUND, nil, 'the parent is the item or below it'
      end
      parent_path = parent.path
    end
    local old = M.menus[a.entity]
    local item = {
      entity = a.entity, parent = a.parent, scope = scope, target = a.target, name = a.name,
      group = group, submenu = a.submenu and true or false, text = a.text,
      order = old and old.order,
    }
    if not item.order then
      item.order = M.next_menu
      M.next_menu = item.order + 1
    end
    if old then
      if old.path then
        menu_remove(old)
      end
      -- A command holds no items, and a submenu in another scope cannot hold
      -- the old ones.
      if not item.submenu or old.scope ~= scope or old.target ~= a.target or old.group ~= group then
        menu_forget_below(a.entity)
      end
    end
    local ok, why = menu_add(item, parent_path)
    if not ok then
      M.menus[a.entity] = nil
      menu_forget_below(a.entity)
      return DCS_REFUSED, nil, why
    end
    M.menus[a.entity] = item
    if item.submenu then
      -- The entities of the items DCS would not take again: the value of
      -- the result, so the sidecar sends them again.
      local dropped = {}
      menu_readd_below(item, dropped)
      if #dropped > 0 then
        return APPLIED, dropped
      end
    end
    return APPLIED
  end,
  -- MENU_REMOVE: the item, and every item the shim holds below a submenu.
  [30] = function(a)
    local item = M.menus[a.entity]
    if not item then
      return APPLIED
    end
    if item.path then
      local ok, err = menu_remove(item)
      if not ok then
        return DCS_REFUSED, nil, tostring(err)
      end
    end
    menu_forget_below(a.entity)
    M.menus[a.entity] = nil
    return APPLIED
  end,
  -- EXTENSION_REMOVE: on_stop, then the extension is gone. A name nobody
  -- registered is gone already.
  [32] = function(a)
    stop_extension(M.extensions[a.name])
    M.extensions[a.name] = nil
    return APPLIED
  end,
  -- MESSAGE: outText, or the form the scope names.
  [49] = function(a)
    local text, scope = a.text, a.scope
    if type(text) ~= 'string' then
      return DCS_REFUSED, nil, 'the text is not a string'
    end
    local ok, err
    if scope == 1 then
      ok, err = pcall(trigger.action.outText, text, a.seconds, a.clear)
    elseif scope == 2 or scope == 5 then
      local call = scope == 2 and trigger.action.outTextForCoalition or trigger.action.outTextForCountry
      ok, err = pcall(call, a.target, text, a.seconds, a.clear)
    elseif scope == 3 or scope == 4 then
      local id, code, message = target_id(scope == 3 and Group or Unit, a)
      if not id then
        return code, nil, message
      end
      local call = scope == 3 and trigger.action.outTextForGroup or trigger.action.outTextForUnit
      ok, err = pcall(call, id, text, a.seconds, a.clear)
    else
      return DCS_REFUSED, nil, 'the scope is not 1 to 5'
    end
    if not ok then
      return DCS_REFUSED, nil, tostring(err)
    end
    return APPLIED
  end,
  -- EVAL: the source runs in a new environment that reads through to the
  -- state's globals, so an assignment to a global stays in it. The value is
  -- the first value the chunk returns.
  [58] = function(a)
    if type(a.source) ~= 'string' then
      return LUA_ERROR, nil, 'the source is not a string'
    end
    local chunk, err = loadstring(a.source, '=eval')
    if not chunk then
      return LUA_ERROR, nil, tostring(err)
    end
    local env = environment()
    setfenv(chunk, env)
    local ok, value = pcall(chunk)
    if not ok then
      return LUA_ERROR, nil, tostring(value)
    end
    return APPLIED, value
  end,
  -- AIRBASE_OWNER: automatic capture off, then the owner.
  [23] = function(a)
    local ab = Airbase.getByName(a.airbase)
    if not ab then
      return NOT_BOUND, nil, 'no airbase ' .. tostring(a.airbase)
    end
    local ok, err = pcall(ab.autoCapture, ab, false)
    if ok then
      ok, err = pcall(ab.setCoalition, ab, a.coalition)
    end
    if not ok then
      return DCS_REFUSED, nil, tostring(err)
    end
    return APPLIED
  end,
  -- WAREHOUSE_SET
  [24] = function(a)
    return stock_command(a, true)
  end,
  -- WAREHOUSE_ADJUST
  [59] = function(a)
    return stock_command(a, false)
  end,
  -- CAPTURE: a group's state, or a static's.
  [64] = function(a)
    local g = Group.getByName(a.name)
    if g then
      return APPLIED, capture_group(g)
    end
    local s = StaticObject.getByName(a.static_name)
    if s then
      return APPLIED, { capture_one(s, false) }
    end
    return NOT_BOUND, nil, 'no group or static of that name'
  end,
  -- COUNTRIES: each country's coalition in this mission.
  [65] = function()
    local out = {}
    for _, n in pairs(country.id) do
      local ok, side = pcall(coalition.getCountryCoalition, n)
      if ok and type(side) == 'number' then
        out[n] = side
      end
    end
    return APPLIED, out
  end,
  -- EXTENSION_SET: the old extension keeps running when the new one fails.
  [31] = function(a)
    local e = { name = a.name, source = a.source }
    reset_extension(e)
    local hooks, err = load_extension(e)
    if not hooks then
      clean_extension(e)
      return LUA_ERROR, nil, err
    end
    e.hooks = hooks
    stop_extension(M.extensions[a.name])
    M.extensions[a.name] = e
    return APPLIED
  end,
  -- EXTENSION_INVOKE: the first value `invoke` returns.
  [57] = function(a)
    local e = M.extensions[a.name]
    if not e or type(e.hooks.invoke) ~= 'function' then
      return LUA_ERROR, nil, 'no extension ' .. tostring(a.name) .. ' with invoke'
    end
    local args = type(a.args) == 'table' and a.args or {}
    local ok, value = run_hook(e, 'invoke', e.hooks.invoke, unpack(args, 1, table.maxn(args)))
    if not ok then
      return LUA_ERROR, nil, tostring(value)
    end
    return APPLIED, value
  end,
}

-- Marks, drawings and menu items the shim holds, taken out of DCS.
local function clear_held()
  for _, held in pairs(M.marks) do
    pcall(trigger.action.removeMark, held.id)
  end
  for _, item in pairs(M.menus) do
    if item.parent == '0' and item.path then
      pcall(menu_remove, item)
    end
  end
  M.marks, M.menus = {}, {}
end

-- The campaign the shim last saw, as the agent's decimal text. A switch to
-- another campaign ends every extension (`on_stop`, then gone) and takes the
-- held marks, drawings and menu items out of DCS. The agent's budget call
-- reports the active campaign, and the lifecycle state.
M.campaign = previous and previous.campaign or nil
local function check_campaign()
  if type(agent.extension_budget) ~= 'function' then
    return
  end
  local _, campaign = agent.extension_budget()
  if type(campaign) ~= 'string' then
    return
  end
  if M.campaign ~= nil and M.campaign ~= campaign then
    local old = M.extensions
    M.extensions = {}
    for _, e in pairs(old) do
      stop_extension(e)
    end
    clear_held()
  end
  M.campaign = campaign
end

-- Run the commands the agent hands out, in ring order, until it has none.
local function commands()
  while true do
    local opcode, args = agent.command()
    if not opcode then
      return
    end
    -- A control command the agent ran on the way may have switched the
    -- campaign.
    check_campaign()
    local handler = HANDLERS[opcode]
    if not handler then
      agent.command_done(UNKNOWN_OPCODE, 'no handler in the shim')
    else
      in_command = true
      local ok, code, value, message, destroyed = pcall(handler, args or {})
      in_command = false
      if not ok then
        agent.command_done(LUA_ERROR, tostring(code))
      elseif type(code) ~= 'number' then
        agent.command_done(LUA_ERROR, 'the handler returned no code')
      else
        local gone = code == APPLIED and destroyed or nil
        local taken = agent.command_done(code, message, value, gone)
        -- The agent freed the rows of the IDs it took, in order (at most
        -- READ_IDS, none when its read failed): the shim lets go of those
        -- and keeps the rest, which it still checks by ID.
        if type(gone) == 'table' and type(taken) == 'number' then
          for i = 1, math.min(taken, #gone) do
            local id = gone[i]
            local slot = T.slot_of[id]
            if slot and T.ids[slot] == id then
              forget(slot)
            end
            T.slot_of[id] = nil
          end
        end
      end
    end
  end
end

-- Run every registered extension's chunk again with a new environment, on a
-- mission start: nothing an extension held survives a load, and the counters
-- and a stop after errors start over.
local function restart_extensions()
  for name, e in pairs(M.extensions) do
    reset_extension(e)
    local hooks, err = load_extension(e)
    if hooks then
      e.hooks = hooks
    else
      clean_extension(e)
      M.file.write('error', 'extension did not start again', { name = name, error = err })
    end
  end
end

-- The model time, or nil when the state gives none.
local function model_time()
  local ok, t = pcall(timer.getTime)
  return ok and type(t) == 'number' and t or nil
end

-- The extensions' turn in a pump (spec/commands.md, "Budget"): queued events,
-- the callbacks DCS made and the timers that came due, then `on_frame`, each
-- while the frame's commanded-work budget has time left. The extensions take
-- turns to go first. Hook time is charged to the budget as it is spent.
local running = false
local function run_extensions()
  if type(agent.extension_budget) ~= 'function' then
    return
  end
  local list = {}
  for _, e in pairs(M.extensions) do
    list[#list + 1] = e
  end
  table.sort(list, function(x, y)
    return x.name < y.name
  end)
  local now = agent.clock()
  -- Charge the time spent and read the lifecycle first, so that the first
  -- pump of a loaded or unpaused mission already asks for the next one.
  local _, _, _, lifecycle = agent.extension_budget(charged)
  charged = 0
  running = lifecycle == 3
  local active = false
  for _, e in ipairs(list) do
    if not e.dead and now >= e.next_report then
      e.next_report = now + REPORT_EVERY
      send_stats(e)
    end
    -- Queued events, waiting callbacks and timers, and an `on_frame` hook
    -- while the mission runs ask for the next pump. An `on_event` hook with
    -- an empty queue asks for nothing: `queue_event` asks when it queues.
    if not e.stopped and not e.dead
      and (e.q_head <= e.q_tail or next(e.timers) ~= nil or #e.pending > 0
        or (running and type(e.hooks.on_frame) == 'function')) then
      active = true
    end
  end
  local left = agent.extension_budget(0, active)
  local n = #list
  if n == 0 or left <= 0 then
    return
  end
  M.turn = (M.turn or 0) + 1
  local first = M.turn % n
  -- Charge the hook time spent since the last call and read what is left.
  local function settle()
    local rest = agent.extension_budget(charged)
    charged = 0
    return rest
  end
  local at = model_time()
  for i = 0, n - 1 do
    local e = list[(first + i) % n + 1]
    if not e.stopped and not e.dead then
      local on_event = e.hooks.on_event
      while left > 0 and not e.stopped and e.q_head <= e.q_tail do
        local event = e.q[e.q_head]
        e.q[e.q_head] = nil
        e.q_head = e.q_head + 1
        if type(on_event) == 'function' then
          run_hook(e, 'on_event', on_event, event)
          left = settle()
        end
      end
      -- The callbacks DCS made: the work that does not fit stays in front.
      local waiting = e.pending
      e.pending = {}
      local j = 1
      while j <= #waiting and left > 0 and not e.stopped and not e.dead do
        run_hook(e, 'callback', waiting[j][1], waiting[j][2])
        left = settle()
        j = j + 1
      end
      if j <= #waiting and not e.dead then
        local rest = {}
        for k = j, #waiting do
          rest[#rest + 1] = waiting[k]
        end
        for _, item in ipairs(e.pending) do
          rest[#rest + 1] = item
        end
        e.pending = rest
      end
      -- The timers that came due, earliest first, once each.
      if at and next(e.timers) ~= nil then
        local due = {}
        for id, t in pairs(e.timers) do
          if t.time <= at then
            due[#due + 1] = id
          end
        end
        table.sort(due, function(x, y)
          local tx, ty = e.timers[x].time, e.timers[y].time
          return tx < ty or (tx == ty and x < y)
        end)
        for _, id in ipairs(due) do
          local t = e.timers[id]
          if left <= 0 or e.stopped or e.dead then
            break
          end
          if t then
            local ok, next_time = run_hook(e, 'timer', t.fn, t.arg, t.time)
            if e.timers[id] == t then
              if ok and type(next_time) == 'number' then
                t.time = next_time
              else
                e.timers[id] = nil
              end
            end
            left = settle()
          end
        end
      end
    end
  end
  if lifecycle == 3 and at then
    for i = 0, n - 1 do
      local e = list[(first + i) % n + 1]
      local on_frame = e.hooks.on_frame
      if left <= 0 then
        break
      end
      if not e.stopped and not e.dead and type(on_frame) == 'function' then
        run_hook(e, 'on_frame', on_frame, at)
        left = settle()
      end
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
  local ok, err = pcall(check_campaign)
  if ok then
    ok, err = pcall(commands)
  end
  if not ok then
    in_command = false
    M.file.write('error', 'commands raised', { error = tostring(err) })
  end
  -- Then the extensions, while the frame's commanded-work budget lasts.
  ok, err = pcall(run_extensions)
  if not ok then
    M.file.write('error', 'extensions raised', { error = tostring(err) })
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

-- Called by the hook from `onSimulationStop`, the last callback in which the
-- mission is still there: every extension stops (`on_stop`) and stays
-- stopped until the next mission start runs its chunk again. Never raises.
function M.simulation_stop()
  local ok, err = pcall(function()
    for _, e in pairs(M.extensions) do
      stop_extension(e)
    end
  end)
  charged = 0
  if not ok then
    pcall(function()
      M.file.write('error', 'extensions did not stop', { error = tostring(err) })
    end)
  end
  return ok
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
    -- `emit` is a global only when raw Lua is on; the agent's budget call
    -- says whether it is.
    local raw = type(agent.extension_budget) == 'function' and select(3, agent.extension_budget())
    rawset(_G, 'emit', raw and emit or nil)
  end)
  if not ok then
    M.file.write('error', 'start raised', { error = tostring(err) })
  end
  return 'start ' .. M.starts .. ' reloads ' .. M.reloads
end

return M
