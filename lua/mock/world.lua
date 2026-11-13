-- A small simulated world for the strict mock: groups, units, statics and
-- airbases, with the scripting API over them in the `server` state and the
-- sandbox, and the few calls the `hook` and `mission` states have.
--
-- What it reproduces, each with its evidence tag:
--   - Runtime mission IDs start at 1,000,000, and a unit's object ID differs
--     from its mission ID (LIVE).
--   - Adding a group under an existing name replaces it. The units get new
--     object IDs and keep their mission IDs (LIVE).
--   - Group:destroy() is deferred: the group still exists within the same
--     call (LIVE), and its removal emits no event (LIVE). While the
--     simulation is paused the removal waits, and it finishes after a resume
--     (LIVE under Wine). Births still reach handlers while paused (LIVE
--     under Wine).
--   - Every added unit emits S_EVENT_BIRTH (LIVE). A unit killed by an
--     explosion emits S_EVENT_UNIT_LOST and S_EVENT_DEAD, and no
--     S_EVENT_KILL (LIVE).
--   - An event with an initiator carries `initiator_data`, a plain table of
--     coalition, mission_id, object_category, object_id, unit_name and
--     unit_type (LIVE).
--   - A group with a route moves along it and its controller has a task
--     (LIVE).
--   - An aircraft's fuel and gun rounds follow its spawn payload (LIVE).
--   - Unit:getFuel() is 1 for a ground unit (LIVE).
--   - DCS.getUnitPositionByObjectId gives x, y, z for a unit and three
--     zeros for any other ID (LIVE).
--   - Unit:getID() returns the mission ID as a string for a unit added at
--     run time (LIVE under Wine).
--   - a_unit_set_life_percentage in the mission state takes a mission ID and
--     sets the unit's life (LIVE).
--   - A mission load removes every runtime object (LIVE).
-- UNVERIFIED choices: births and deaths reach handlers at the next frame;
-- a getter on a unit that no longer exists raises; an explosion kills
-- within 10 m.

local W = {}

-- Unit types the world models. `life` and `ammo` for the M-113 and the fuel
-- and gun figures for the F-15C are LIVE; the F-15C's life is UNVERIFIED.
local TYPES = {
  ['M-113'] = {
    life = 3,
    ammo = { { 'weapons.shells.M2_12_7_T', 104 }, { 'weapons.shells.M2_12_7_T', 404 } },
  },
  ['F-15C'] = { life = 1, air = true, fuel_max = 6103, gun = { 'weapons.shells.M61_20_HE', 940 } },
  -- The event catalogue's scenarios (lua/catalogue). Their figures are
  -- UNVERIFIED: the mock only needs the types to exist.
  ['M-1 Abrams'] = { life = 1 },
  ['T-72B'] = { life = 1 },
  ['Osa 9A33 ln'] = { life = 1 },
  ['ZSU-23-4 Shilka'] = { life = 1 },
  ['UH-1H'] = { life = 1, air = true, fuel_max = 631 },
}

local Sim = {}
Sim.__index = Sim

function Sim:clear()
  self.units, self.by_name, self.dead = {}, {}, {}
  self.groups, self.statics = {}, {}
  self.marks = {}
  self.mission_ids, self.group_ids = {}, {}
  self.next_object, self.next_mission, self.next_group = 16777472, 1000000, 1000000
  self.queue = {}
end

local function new_sim(host)
  local sim = setmetatable({ host = host }, Sim)
  sim:clear()
  -- Three airbases, with a coalition each that setCoalition changes.
  sim.airbases = {
    { id = 1, name = 'Alpha', x = 0, z = 0, coalition = 0 },
    { id = 2, name = 'Bravo', x = 50000, z = 20000, coalition = 0 },
    { id = 3, name = 'Charlie', x = -30000, z = 60000, coalition = 0 },
  }
  return sim
end

function Sim:object_id()
  self.next_object = self.next_object + 256
  return self.next_object
end

function Sim:remove_unit(unit)
  self.units[unit.id] = nil
  if self.by_name[unit.name] == unit then
    self.by_name[unit.name] = nil
  end
  self.dead[unit.id] = unit
  unit.alive = false
  -- A group whose last unit is gone no longer exists.
  local group = unit.group
  for _, other in ipairs(group.units) do
    if other.alive then
      return
    end
  end
  if self.groups[group.name] == group then
    self.groups[group.name] = nil
  end
  group.alive = false
end

function Sim:remove_group(group)
  for _, unit in ipairs(group.units) do
    if unit.alive then
      self:remove_unit(unit)
    end
  end
  if self.groups[group.name] == group then
    self.groups[group.name] = nil
  end
  group.alive = false
end

function Sim:add_group(country, category, data)
  if type(data) ~= 'table' or type(data.name) ~= 'string' or type(data.units) ~= 'table' then
    error('mock: coalition.addGroup needs a table with a name and units', 3)
  end
  local old = self.groups[data.name]
  if old then
    self:remove_group(old)
  end
  self.group_ids[data.name] = self.group_ids[data.name] or self.next_group
  self.next_group = self.next_group + 1
  local group = {
    name = data.name,
    id = self.group_ids[data.name],
    category = category,
    country = country,
    units = {},
    alive = true,
    route = {},
    leg = 1,
  }
  for _, point in ipairs(data.route and data.route.points or {}) do
    group.route[#group.route + 1] = { x = point.x, z = point.y, speed = point.speed or 0 }
  end
  for _, u in ipairs(data.units) do
    local kind = TYPES[u.type]
    if not kind then
      error('mock: the unit type ' .. tostring(u.type) .. ' is not modelled', 3)
    end
    if not self.mission_ids[u.name] then
      self.mission_ids[u.name] = self.next_mission
      self.next_mission = self.next_mission + 1
    end
    local previous = self.by_name[u.name]
    if previous then
      self:remove_unit(previous)
    end
    local unit = {
      id = self:object_id(),
      mission_id = self.mission_ids[u.name],
      name = u.name,
      type = u.type,
      kind = kind,
      group = group,
      x = u.x,
      y = kind.air and (u.alt or 0) or 0,
      z = u.y,
      heading = u.heading or 0,
      speed = kind.air and (u.speed or 0) or 0,
      life = kind.life,
      fuel = kind.air and ((u.payload and u.payload.fuel or kind.fuel_max) / kind.fuel_max) or 1,
      gun = u.payload and u.payload.gun or 100,
      alive = true,
    }
    self.units[unit.id] = unit
    self.by_name[unit.name] = unit
    group.units[#group.units + 1] = unit
    self.queue[#self.queue + 1] = { event = 'S_EVENT_BIRTH', unit = unit }
  end
  self.groups[group.name] = group
  return group
end

function Sim:add_static(country, data)
  if type(data) ~= 'table' or type(data.name) ~= 'string' then
    error('mock: coalition.addStaticObject needs a table with a name', 3)
  end
  local static = {
    id = self:object_id(),
    name = data.name,
    type = data.type,
    country = country,
    x = data.x,
    y = 0,
    z = data.y,
    alive = true,
  }
  self.statics[static.name] = static
  return static
end

-- Kill every unit within 10 m of the point.
function Sim:explode(point)
  for _, unit in pairs(self.units) do
    local dx, dz = unit.x - point.x, unit.z - point.z
    if unit.alive and not unit.dying and dx * dx + dz * dz <= 100 then
      unit.dying = true
      unit.life = 0
      self.queue[#self.queue + 1] = { event = 'S_EVENT_UNIT_LOST', unit = unit }
      self.queue[#self.queue + 1] = { event = 'S_EVENT_DEAD', unit = unit, remove = true }
    end
  end
end

-- Deliver queued events and carry out deferred removals. Runs every frame,
-- paused or not; a group's removal waits while `paused`.
function Sim:flush(paused)
  local queue = self.queue
  self.queue = {}
  for _, item in ipairs(queue) do
    if paused and item.destroy_group then
      self.queue[#self.queue + 1] = item
    else
      if item.event then
        self.host:unit_event(item.event, item.unit)
      end
      if item.remove then
        self:remove_unit(item.unit)
      elseif item.destroy_group then
        self:remove_group(item.destroy_group)
      elseif item.destroy_static then
        item.destroy_static.alive = false
        if self.statics[item.destroy_static.name] == item.destroy_static then
          self.statics[item.destroy_static.name] = nil
        end
      end
    end
  end
end

-- Move every group along its route for `dt` seconds of model time.
function Sim:step(dt)
  for _, group in pairs(self.groups) do
    local lead = group.units[1]
    local target = group.route[group.leg + 1]
    local air = lead and lead.kind.air
    for _, unit in ipairs(group.units) do
      if air then
        unit.fuel = math.max(0, unit.fuel - 1e-4 * dt)
      end
    end
    if lead and lead.alive and (target or air) then
      local speed = air and lead.speed or target.speed
      local heading = lead.heading
      if target then
        heading = math.atan2(target.z - lead.z, target.x - lead.x)
      end
      local step = speed * dt
      if target and (target.x - lead.x) ^ 2 + (target.z - lead.z) ^ 2 <= step * step then
        group.leg = group.leg + 1
      end
      for _, unit in ipairs(group.units) do
        if unit.alive then
          unit.x = unit.x + math.cos(heading) * step
          unit.z = unit.z + math.sin(heading) * step
          unit.heading = heading
          unit.speed = speed
        end
      end
    elseif lead then
      for _, unit in ipairs(group.units) do
        unit.speed = 0
      end
    end
  end
end

local function side_of(state, country)
  local id, side = state.country.id, state.coalition.side
  if country == id.USA then
    return side.BLUE
  elseif country == id.RUSSIA then
    return side.RED
  end
  return side.NEUTRAL
end

-- The scripting API over the world, for the `server` state or the sandbox.
local function scripting(host, name)
  local state = host.states[name]
  local sim = host.sim
  local ns = host.namespace
  local names = host.listing.states[name]

  -- An object handle: a table holding `id_` whose methods come from its
  -- class and then the class's parents, as the listing names them.
  local function handle(chain, id)
    return setmetatable({ id_ = id }, {
      __index = function(_, key)
        for _, class in ipairs(chain) do
          if names.kinds[class .. '.' .. tostring(key)] then
            return state[class][key]
          end
        end
        error('mock: ' .. name .. ' ' .. chain[1] .. '.' .. tostring(key) .. ' is not in the listing', 2)
      end,
    })
  end

  local UNIT, GROUP, STATIC, AIRBASE = { 'Unit', 'Object' }, { 'Group' }, { 'StaticObject', 'Object' },
    { 'Airbase', 'Object' }

  -- The thing a handle names, alive or not.
  local function find(self)
    local id = self.id_
    return sim.units[id] or sim.dead[id]
  end

  local function static_of(self)
    for _, static in pairs(sim.statics) do
      if static.id == self.id_ then
        return static
      end
    end
  end

  local function airbase_of(self)
    for _, airbase in ipairs(sim.airbases) do
      if airbase.id == self.id_ then
        return airbase
      end
    end
  end

  -- A live unit, or an error: what a getter does on a unit that is gone is
  -- UNVERIFIED.
  local function live(self, method)
    local unit = sim.units[self.id_]
    if not unit then
      error('mock: UNVERIFIED: ' .. method .. ' on a unit that no longer exists', 3)
    end
    return unit
  end

  local function any(self)
    return sim.units[self.id_] or static_of(self) or airbase_of(self)
  end

  state.Object = ns(name, 'Object', {
    isExist = function(self)
      local thing = sim.units[self.id_] or static_of(self)
      return thing ~= nil and thing.alive == true
    end,
    getName = function(self)
      local thing = find(self) or static_of(self) or airbase_of(self)
      return thing and thing.name
    end,
    getTypeName = function(self)
      local thing = find(self) or static_of(self)
      return thing and thing.type
    end,
    getPoint = function(self)
      local thing = any(self) or error('mock: UNVERIFIED: getPoint on an object that no longer exists', 2)
      return { x = thing.x, y = thing.y or 0, z = thing.z }
    end,
    getPosition = function(self)
      local unit = live(self, 'getPosition')
      local c, s = math.cos(unit.heading), math.sin(unit.heading)
      return {
        p = { x = unit.x, y = unit.y, z = unit.z },
        x = { x = c, y = 0, z = s },
        y = { x = 0, y = 1, z = 0 },
        z = { x = -s, y = 0, z = c },
      }
    end,
    getVelocity = function(self)
      local unit = live(self, 'getVelocity')
      return { x = math.cos(unit.heading) * unit.speed, y = 0, z = math.sin(unit.heading) * unit.speed }
    end,
    inAir = function(self)
      local unit = live(self, 'inAir')
      return unit.kind.air == true and unit.y > 0
    end,
    destroy = function(self)
      local unit = sim.units[self.id_]
      if unit then
        sim.queue[#sim.queue + 1] = { remove = true, unit = unit }
        return
      end
      local static = static_of(self)
      if static then
        sim.queue[#sim.queue + 1] = { destroy_static = static }
      end
    end,
  })

  state.Unit = ns(name, 'Unit', {
    getByName = function(unit_name)
      local unit = sim.by_name[unit_name]
      return unit and handle(UNIT, unit.id) or nil
    end,
    getName = function(self)
      local unit = find(self)
      return unit and unit.name
    end,
    getTypeName = function(self)
      local unit = find(self)
      return unit and unit.type
    end,
    -- A string for a unit added at run time (LIVE under Wine).
    getID = function(self)
      return tostring(live(self, 'getID').mission_id)
    end,
    getObjectID = function(self)
      return self.id_
    end,
    getLife = function(self)
      return live(self, 'getLife').life
    end,
    getFuel = function(self)
      return live(self, 'getFuel').fuel
    end,
    getAmmo = function(self)
      local unit = live(self, 'getAmmo')
      local out = {}
      for i, entry in ipairs(unit.kind.ammo or {}) do
        out[i] = { count = entry[2], desc = { typeName = entry[1] } }
      end
      if unit.kind.gun then
        out[#out + 1] = {
          count = math.floor(unit.kind.gun[2] * unit.gun / 100),
          desc = { typeName = unit.kind.gun[1] },
        }
      end
      return #out > 0 and out or nil
    end,
    getDesc = function(self)
      local unit = live(self, 'getDesc')
      return { typeName = unit.type, life = unit.kind.life, fuelMassMax = unit.kind.fuel_max }
    end,
    getGroup = function(self)
      local unit = live(self, 'getGroup')
      return handle(GROUP, unit.group.id)
    end,
    getCoalition = function(self)
      return side_of(state, live(self, 'getCoalition').group.country)
    end,
  })

  local function group_of(self)
    for _, group in pairs(sim.groups) do
      if group.id == self.id_ then
        return group
      end
    end
  end

  state.Controller = ns(name, 'Controller', {
    -- True for a group following a route (LIVE under Wine, for a ground
    -- group and for an aircraft whose group task is CAP).
    hasTask = function(self)
      local group = group_of(self)
      return group ~= nil and #group.route > 1
    end,
  })

  state.Group = ns(name, 'Group', {
    getByName = function(group_name)
      local group = sim.groups[group_name]
      return group and handle(GROUP, group.id) or nil
    end,
    getName = function(self)
      local group = group_of(self)
      return group and group.name
    end,
    getID = function(self)
      return self.id_
    end,
    isExist = function(self)
      return group_of(self) ~= nil
    end,
    getUnits = function(self)
      local out = {}
      local group = group_of(self)
      for _, unit in ipairs(group and group.units or {}) do
        if unit.alive then
          out[#out + 1] = handle(UNIT, unit.id)
        end
      end
      return out
    end,
    getUnit = function(self, index)
      return state.Group.getUnits(self)[index]
    end,
    getSize = function(self)
      return #state.Group.getUnits(self)
    end,
    getCategory = function(self)
      local group = group_of(self)
      return group and group.category
    end,
    getCoalition = function(self)
      local group = group_of(self)
      return group and side_of(state, group.country)
    end,
    getController = function(self)
      return handle({ 'Controller' }, self.id_)
    end,
    destroy = function(self)
      local group = group_of(self)
      if group then
        sim.queue[#sim.queue + 1] = { destroy_group = group }
      end
    end,
  })

  state.StaticObject = ns(name, 'StaticObject', {
    getByName = function(static_name)
      local static = sim.statics[static_name]
      return static and handle(STATIC, static.id) or nil
    end,
    getName = function(self)
      local static = static_of(self)
      return static and static.name
    end,
    getID = function(self)
      return self.id_
    end,
  })

  state.Warehouse = ns(name, 'Warehouse', {
    getInventory = function()
      return { aircraft = {}, weapon = {}, liquids = { [0] = 100000, [1] = 100000, [2] = 100000, [3] = 100000 } }
    end,
  })

  state.Airbase = ns(name, 'Airbase', {
    getByName = function(airbase_name)
      for _, airbase in ipairs(sim.airbases) do
        if airbase.name == airbase_name then
          return handle(AIRBASE, airbase.id)
        end
      end
    end,
    getName = function(self)
      return airbase_of(self).name
    end,
    getID = function(self)
      return self.id_
    end,
    getCoalition = function(self)
      return airbase_of(self).coalition
    end,
    -- Every mock airbase is an airdrome with one free fighter spot, in the
    -- fields a parking entry has (PRIOR).
    getDesc = function()
      return { category = state.Airbase.Category.AIRDROME }
    end,
    getParking = function(self)
      local a = airbase_of(self)
      return {
        { Term_Index = 1, Term_Type = 104, TO_AC = false, vTerminalPos = { x = a.x + 200, y = 0, z = a.z + 50 } },
      }
    end,
    setCoalition = function(self, side)
      airbase_of(self).coalition = side
    end,
    autoCapture = function() end,
    getWarehouse = function(self)
      return handle({ 'Warehouse' }, self.id_)
    end,
  })

  state.coalition = ns(name, 'coalition', {
    addGroup = function(country, category, data)
      return handle(GROUP, sim:add_group(country, category, data).id)
    end,
    addStaticObject = function(country, data)
      return handle(STATIC, sim:add_static(country, data).id)
    end,
    getCountryCoalition = function(country)
      return side_of(state, country)
    end,
    getGroups = function(side, category)
      local out = {}
      for _, group in pairs(sim.groups) do
        if side_of(state, group.country) == side and (category == nil or group.category == category) then
          out[#out + 1] = group
        end
      end
      table.sort(out, function(a, b)
        return a.name < b.name
      end)
      for i, group in ipairs(out) do
        out[i] = handle(GROUP, group.id)
      end
      return out
    end,
    getStaticObjects = function(side)
      local out = {}
      for _, static in pairs(sim.statics) do
        if side_of(state, static.country) == side then
          out[#out + 1] = handle(STATIC, static.id)
        end
      end
      return out
    end,
  })

  -- A mark event, dispatched to every state's handlers once: only the
  -- server state's trigger table sends it.
  local function mark_event(event_name, id, text, point, side, removed)
    if not removed then
      sim.marks[id] = { text = text, point = point, coalition = side }
    end
    if name ~= 'server' then
      return
    end
    host:event({
      id = state.world.event[event_name],
      idx = id,
      text = text,
      pos = { x = point.x, y = point.y, z = point.z },
      coalition = side,
      groupID = -1,
      time = host.start_time + host.model,
    })
  end

  state.trigger = ns(name, 'trigger', {
    action = {
      explosion = function(point)
        sim:explode(point)
      end,
      -- A scripted mark or drawing emits S_EVENT_MARK_ADDED, and its
      -- removal S_EVENT_MARK_REMOVED, with no initiator, coalition 255 and
      -- group -1 for one shown to all, and a time that is not model time
      -- (LIVE, the S9 event catalogue). The mock's time is the time of day.
      markToAll = function(id, text, point)
        mark_event('S_EVENT_MARK_ADDED', id, text, point, 255)
      end,
      markToCoalition = function(id, text, point, side)
        mark_event('S_EVENT_MARK_ADDED', id, text, point, side)
      end,
      lineToAll = function(side, id, from)
        mark_event('S_EVENT_MARK_ADDED', id, '', from, side == -1 and 255 or side)
      end,
      removeMark = function(id)
        local mark = sim.marks[id]
        if mark then
          sim.marks[id] = nil
          mark_event('S_EVENT_MARK_REMOVED', id, mark.text, mark.point, mark.coalition, true)
        end
      end,
    },
  })

  state.land = ns(name, 'land', {
    getHeight = function()
      return 0
    end,
  })

  state.coord = ns(name, 'coord', {
    LOtoLL = function(point)
      return W.latlon(point.x, point.z)
    end,
  })

  local function airbases()
    local out = {}
    for i, airbase in ipairs(sim.airbases) do
      out[i] = handle(AIRBASE, airbase.id)
    end
    return out
  end

  -- Call `fn` for each unit inside the sphere, until it returns false.
  local function search(_, volume, fn, data)
    local centre, radius = volume.params.point, volume.params.radius
    for _, unit in pairs(sim.units) do
      local dx, dy, dz = unit.x - centre.x, unit.y - centre.y, unit.z - centre.z
      if dx * dx + dy * dy + dz * dz <= radius * radius and fn(handle(UNIT, unit.id), data) == false then
        return
      end
    end
  end

  if name == 'server' then
    rawset(state.world, 'getAirbases', airbases)
    rawset(state.world, 'searchObjects', search)
    rawset(state.DCS, 'getUnitPositionByObjectId', W.position(sim))
    state.Export = ns(name, 'Export', {
      LoGetWorldObjects = function()
        local out = {}
        for id, unit in pairs(sim.units) do
          out[id] = W.exported(unit)
        end
        return out
      end,
      LoGetObjectById = function(id)
        local unit = sim.units[id]
        return unit and W.exported(unit) or nil
      end,
    })
  else
    -- The sandbox has its own handler table (ED-CODE: the dispatcher is
    -- loaded into two states).
    local handlers, order = {}, {}
    state.world = ns(name, 'world', {
      eventHandlers = handlers,
      addEventHandler = function(h)
        if not handlers[h] then
          order[#order + 1] = h
        end
        handlers[h] = h
      end,
      removeEventHandler = function(h)
        handlers[h] = nil
        for i, other in ipairs(order) do
          if other == h then
            table.remove(order, i)
            return
          end
        end
      end,
      onEvent = function(event)
        for _, h in ipairs({ unpack(order) }) do
          h:onEvent(event)
        end
      end,
      getAirbases = airbases,
      searchObjects = search,
    })
  end

  -- The event for a unit, as this state's handlers see it.
  return function(event_name, unit)
    return {
      id = state.world.event[event_name],
      time = host.model,
      initiator = handle(UNIT, unit.id),
      initiator_data = {
        coalition = side_of(state, unit.group.country),
        mission_id = unit.mission_id,
        object_category = state.Object.Category.UNIT,
        object_id = unit.id,
        unit_name = unit.name,
        unit_type = unit.type,
      },
    }
  end
end

-- A made-up projection, the same in every state that converts.
function W.latlon(x, z)
  return 42 + x / 111000, 41 + z / 80000, 0
end

function W.position(sim)
  return function(id)
    local unit = sim.units[id]
    if not unit then
      return 0, 0, 0
    end
    return unit.x, unit.y, unit.z
  end
end

function W.exported(unit)
  local lat, lon = W.latlon(unit.x, unit.z)
  return {
    Name = unit.type,
    UnitName = unit.name,
    GroupName = unit.group.name,
    Position = { x = unit.x, y = unit.y, z = unit.z },
    LatLongAlt = { Lat = lat, Long = lon, Alt = unit.y },
    Heading = unit.heading,
  }
end

-- Add the world's part of `name`'s API to the host's state of that name.
function W.install(host, name)
  if not host.sim then
    host.sim = new_sim(host)
    host.makers = {}
    -- Send an event about a unit to the server state and the sandbox.
    function host:unit_event(event_name, unit)
      for _, state in ipairs({ 'server', 'sse' }) do
        if self.makers[state] and self.states[state] then
          self.states[state].world.onEvent(self.makers[state](event_name, unit))
        end
      end
    end
  end
  local state = host.states[name]
  local sim = host.sim
  if name == 'hook' then
    rawset(state.DCS, 'getUnitPositionByObjectId', W.position(sim))
    state.terrain = host.namespace(name, 'terrain', {
      convertMetersToLatLon = function(x, z)
        local lat, lon = W.latlon(x, z)
        return lat, lon
      end,
    })
  elseif name == 'mission' then
    state.a_unit_set_life_percentage = function(mission_id, percent)
      for _, unit in pairs(sim.units) do
        if unit.mission_id == tonumber(mission_id) then
          unit.life = unit.kind.life * percent / 100
        end
      end
    end
  else
    host.makers[name] = scripting(host, name)
  end
end

return W
