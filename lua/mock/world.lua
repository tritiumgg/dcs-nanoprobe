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
--   - Every added unit and every added static emits S_EVENT_BIRTH (LIVE).
--     The event's object_category is 3 for a static and 6 for a cargo
--     static (LIVE).
--   - A unit killed by a weapon or an explosion is doomed: isExist() is false
--     at once and its group stops listing it (LIVE). Its death events come
--     later, when the wreck explodes, 0 to 84 s afterward (LIVE). The
--     mock's `sim.death_delay` is that wait in seconds of model time, 0 by
--     default. Model time stands still while paused, so a delayed death waits.
--   - A doomed unit, named by its object ID in a plain table, still answers
--     Unit.isDead (true), Unit.getLife (a number), Object.getPoint,
--     Unit.getDesc and Object.getCategory (1), and
--     DCS.getUnitPositionByObjectId gives the wreck's position (LIVE).
--   - A unit removed by Unit.destroy or Group.destroy emits no event. Its
--     isDead, getLife, getPoint and getDesc raise, Object.getCategory gives 0
--     and the position getter gives three zeros (LIVE). A unit that exists
--     is not dead (LIVE).
--   - A static killed by a weapon (Sim:shoot) stops existing at once, reads
--     StaticObject.getLife 0 and sends the same events as a unit; a static
--     removed by Object.destroy reads nothing (getLife raises) and sends none
--     (LIVE, E38). Object.destroy on a doomed unit or static, a burning wreck,
--     takes it out of the world: its reads then raise, its death events still
--     come.
--   - A script that sets a doomed unit's life (a_unit_set_life_percentage
--     above 0, or Sim:revive) revives it: same object ID, isExist true,
--     isDead false, listed by its group again, no event (LIVE, E38).
--   - A weapon kill (Sim:shoot) emits S_EVENT_HIT and S_EVENT_BDA naming the
--     unit as target at once, then S_EVENT_KILL, S_EVENT_UNIT_LOST and
--     S_EVENT_DEAD together at the death. An explosion kill emits only
--     S_EVENT_UNIT_LOST and S_EVENT_DEAD, and no S_EVENT_KILL, HIT or BDA
--     (LIVE).
--   - An event with an initiator carries `initiator_data`, a plain table of
--     coalition, mission_id, object_category, object_id, unit_name and
--     unit_type (LIVE). An event with a target carries `target_data`, the
--     same table for the target.
--   - A group with a route moves along it and its controller has a task
--     (LIVE).
--   - An aircraft's fuel and gun rounds follow its spawn payload (LIVE).
--   - Unit:getFuel() is 1 for a ground unit (LIVE).
--   - DCS.getUnitPositionByObjectId gives x, y, z for a unit and for a
--     cargo static, and three zeros for any other ID (LIVE).
--   - Object.getCategory gives 1 for a unit, 3 for a static and 6 for a
--     cargo static (LIVE).
--   - Unit:getID() returns the mission ID as a string for a unit added at
--     run time (LIVE under Wine).
--   - a_unit_set_life_percentage in the mission state takes a mission ID and
--     sets the unit's life (LIVE).
--   - A mission load removes every runtime object (LIVE).
--   - An airbase's warehouse keeps counts: setItem and setLiquidAmount set,
--     addItem and addLiquid add, removeItem and removeLiquid take away and
--     stop at zero with no error (LIVE, E6, step S15), and
--     getInventory reads them back. A weapon's name starts with `weapons.`.
--   - Airbase:autoCapture(flag) records the flag; setCoalition sets the owner.
--   - Controller:setOption records each option's value by group.
--   - trigger.action.markToAll, markToCoalition, markToGroup and markupToAll
--     add to `sim.marks` by ID and send S_EVENT_MARK_ADDED; removeMark takes
--     one out. outText and its Coalition, Country, Group and Unit forms record
--     each call in `sim.messages`. missionCommands keeps one menu tree per
--     scope; `sim:menu_list(key)` and `sim:menu_select(key, path)` read it.
--     What DCS returns for a path is UNVERIFIED beyond a table of names.
--   - trigger.action.outSound and its Coalition, Country, Group and Unit
--     forms (server state) take a file and, for a form, a number first; the
--     mock records each call in `sim.sounds` as { call, target, file }, for any
--     file, a missing one included (LIVE: DCS raises nothing). The mission
--     state's a_out_picture, _s, _c, _g and _u take the target (not for the
--     plain one; _s takes the coalition's name, the others a number), a
--     resource key, the seconds, clear view as a boolean, the
--     start delay, horizontal, vertical, size and size units, and record each
--     call in `sim.pictures`; a_out_picture_stop counts in
--     `sim.picture_stops`; getValueResourceByKey returns a key it does not
--     know as it is (LIVE). The shapes of the arguments are ED-CODE; their
--     values on screen are UNVERIFIED.
--   - A unit added with a `player` name has a player in it: Unit.getPlayerName
--     returns the name, and nil for an AI unit (UNVERIFIED). `sim:takeoff(unit)`
--     sends S_EVENT_TAKEOFF with the unit as initiator.
-- UNVERIFIED choices: births reach handlers at the next frame; the other
-- getters on a unit that no longer exists raise; a static's initiator_data
-- has no mission_id; an explosion kills within 10 m.

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
  -- The mixed scenario's ship (S10). UNVERIFIED figures.
  ['speedboat'] = { life = 1 },
  -- The death probe (lua/sampling/deaths.lua). UNVERIFIED figures.
  ['A-10A'] = { life = 1, air = true, fuel_max = 5029 },
  ['Ural-375'] = { life = 1 },
  -- Types the world commands' tests spawn (S15). UNVERIFIED figures.
  ['BTR-80'] = { life = 2, ammo = { { 'weapons.shells.2A72_30', 500 } } },
  ['Mi-8MT'] = { life = 2, air = true, fuel_max = 1929, gun = { 'weapons.shells.PKT_7_62', 1000 } },
  ['Su-27'] = { life = 1, air = true, fuel_max = 9400, gun = { 'weapons.shells.GSH301_30_HE', 150 } },
  -- Many ammo rows, for the capture size test (S15). UNVERIFIED figures.
  ['T-90'] = {
    life = 2,
    ammo = {
      { 'weapons.shells.125_HE', 10 }, { 'weapons.shells.125_AP', 10 }, { 'weapons.shells.125_HEAT', 10 },
      { 'weapons.missiles.9M119', 4 }, { 'weapons.shells.PKT_7_62', 2000 }, { 'weapons.shells.NSVT_12_7', 300 },
      { 'weapons.shells.125_SM1', 4 }, { 'weapons.shells.125_SM2', 4 }, { 'weapons.shells.125_SM3', 4 },
      { 'weapons.shells.125_SM4', 4 }, { 'weapons.shells.125_SM5', 4 }, { 'weapons.shells.125_SM6', 4 },
    },
  },
  ['MOSCOW'] = { life = 8, ammo = { { 'weapons.missiles.S_300F_48N6', 64 } } },
}

-- An ammunition row's desc: DCS gives the full descriptor, not just the name.
local function ammo_desc(type_name)
  return {
    typeName = type_name, displayName = type_name, category = 0, life = 1, box = { min = {}, max = {} },
    warhead = { caliber = 30, explosiveMass = 0.1, mass = 0.4, type = 1 },
    a = 1, b = 2, c = 3, d = 4,
  }
end

local Sim = {}
Sim.__index = Sim

function Sim:clear()
  -- `dead` holds killed units, `removed` units that Unit.destroy or
  -- Group.destroy took out.
  self.units, self.by_name, self.dead, self.removed = {}, {}, {}, {}
  -- `dead_statics` holds killed statics, `gone_statics` removed ones.
  self.dead_statics, self.gone_statics = {}, {}
  self.groups, self.statics = {}, {}
  self.marks = {}
  -- F10 menu trees by scope key (`all`, `c<coalition>`, `g<group ID>`), and
  -- the text calls of trigger.action.outText and its forms.
  self.menus, self.messages = {}, {}
  self.actions, self.next_action = {}, -1
  self.orders = {}
  -- Controller options by group ID, then option number.
  self.options = {}
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
  for _, airbase in ipairs(sim.airbases) do
    -- `auto_capture` is nil until a script sets it.
    airbase.stock = { aircraft = {}, weapon = {}, liquids = { [0] = 100000, [1] = 100000, [2] = 100000, [3] = 100000 } }
  end
  return sim
end

function Sim:object_id()
  self.next_object = self.next_object + 256
  return self.next_object
end

-- Take a unit out of the world: it no longer exists and its group no longer
-- lists it. A group whose last unit is gone no longer exists.
local function detach(self, unit)
  self.units[unit.id] = nil
  if self.by_name[unit.name] == unit then
    self.by_name[unit.name] = nil
  end
  unit.alive = false
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

-- Remove a unit with no event (Unit.destroy, Group.destroy, a replaced group).
function Sim:remove_unit(unit)
  if unit.alive then
    self.removed[unit.id] = unit
    detach(self, unit)
  elseif self.dead[unit.id] then
    -- A burning wreck: its reads raise from now on.
    self.dead[unit.id] = nil
    self.removed[unit.id] = unit
  end
end

-- Take a static out of the world with no event (Object.destroy). A killed
-- static that is destroyed too has nothing left to read.
function Sim:remove_static(static)
  static.alive = false
  if self.statics[static.name] == static then
    self.statics[static.name] = nil
  end
  self.dead_statics[static.id] = nil
  self.gone_statics[static.id] = static
end

-- Whether a point lies inside a world.removeJunk volume: a sphere (id 2) or
-- a box (id 1). Any other shape holds nothing.
local function inside(volume, p)
  local params = type(volume) == 'table' and volume.params
  if type(params) ~= 'table' then
    error('mock: removeJunk takes a volume', 3)
  end
  if volume.id == 2 then
    local c, r = params.point, params.radius
    local dx, dy, dz = p.x - c.x, p.y - c.y, p.z - c.z
    return dx * dx + dy * dy + dz * dz <= r * r
  elseif volume.id == 1 then
    local lo, hi = params.min, params.max
    return p.x >= lo.x and p.x <= hi.x and p.y >= lo.y and p.y <= hi.y and p.z >= lo.z and p.z <= hi.z
  end
  return false
end

-- world.removeJunk: the wrecks of killed units inside the volume leave the
-- world, as Object.destroy on a wreck does. A killed static stays, dead, and
-- counts once for the junk around it (LIVE,
-- docs/results/2026-11-16-clear-debris.md). Returns how many. A test sets
-- `sim.junk_raises` to make the call raise.
function Sim:remove_junk(volume)
  if self.junk_raises then
    error('mock: removeJunk raises', 3)
  end
  local n = 0
  for id, unit in pairs(self.dead) do
    if inside(volume, unit) then
      self.dead[id] = nil
      self.removed[id] = unit
      n = n + 1
    end
  end
  for _, static in pairs(self.dead_statics) do
    if inside(volume, static) then
      n = n + 1
    end
  end
  return n
end

-- Bring a killed unit back with its full life: the same object ID, listed by
-- its group again, no event. Returns whether it was dead.
function Sim:revive(unit)
  if self.dead[unit.id] ~= unit then
    return false
  end
  self.dead[unit.id] = nil
  self.units[unit.id] = unit
  self.by_name[unit.name] = unit
  unit.alive = true
  unit.life = unit.kind.life
  local group = unit.group
  group.alive = true
  if not self.groups[group.name] then
    self.groups[group.name] = group
  end
  return true
end

-- Kill a unit: it stops existing at once, keeps its position and has `life`
-- (LIVE). Its death events are the caller's to queue.
function Sim:doom(unit)
  self.dead[unit.id] = unit
  unit.life = 0
  detach(self, unit)
end

-- Kill a static: it stops existing at once and reads life 0, as a unit does.
function Sim:doom_static(static)
  static.alive = false
  if self.statics[static.name] == static then
    self.statics[static.name] = nil
  end
  self.dead_statics[static.id] = static
end

-- Seconds of model time from a kill to its death events.
Sim.death_delay = 0

-- The model time at which a kill made now reaches its death events.
local function death_time(self)
  return (self.host.model or 0) + self.death_delay
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
      -- The name of the player in the unit, nil for an AI unit.
      player = u.player,
      alive = true,
    }
    self.units[unit.id] = unit
    self.by_name[unit.name] = unit
    group.units[#group.units + 1] = unit
    self.queue[#self.queue + 1] = { event = 'S_EVENT_BIRTH', initiator = unit }
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
    static = true,
    -- A static of the `Cargos` category can move (LIVE).
    cargo = data.category == 'Cargos',
  }
  -- A new object under a taken name removes the old holder with no event
  -- (LIVE for a live holder, docs/results/2026-11-15-s15-world-commands.md;
  -- UNVERIFIED for a dead one).
  for id, old in pairs(self.dead_statics) do
    if old.name == static.name then
      self.dead_statics[id] = nil
      self.gone_statics[id] = old
    end
  end
  if data.dead == true then
    -- Added dead: it reads life 0 and `isExist` false from the start, and
    -- `StaticObject.getByName` finds it (LIVE,
    -- docs/results/2026-11-17-person-fixes.md). Whether DCS sends a birth
    -- event for it is UNVERIFIED; the mock sends one, the harder case.
    static.alive = false
    self.statics[static.name] = nil
    self.dead_statics[static.id] = static
  else
    self.statics[static.name] = static
  end
  self.queue[#self.queue + 1] = { event = 'S_EVENT_BIRTH', initiator = static }
  return static
end

-- Queue the death events of a doomed unit, for `death_delay` from now. A
-- weapon kill (`weapon`) also sends S_EVENT_KILL.
local function queue_deaths(self, unit, weapon, shooter)
  local at = death_time(self)
  local q = self.queue
  if weapon then
    q[#q + 1] = { event = 'S_EVENT_KILL', initiator = shooter, target = unit, at = at }
  end
  q[#q + 1] = { event = 'S_EVENT_UNIT_LOST', initiator = unit, at = at }
  q[#q + 1] = { event = 'S_EVENT_DEAD', initiator = unit, at = at }
end

-- Kill every unit and static within 10 m of the point. It sends nothing
-- until the death events (LIVE).
function Sim:explode(point)
  local hit = {}
  for _, unit in pairs(self.units) do
    local dx, dz = unit.x - point.x, unit.z - point.z
    if dx * dx + dz * dz <= 100 then
      hit[#hit + 1] = unit
    end
  end
  table.sort(hit, function(a, b)
    return a.id < b.id
  end)
  for _, unit in ipairs(hit) do
    self:doom(unit)
    queue_deaths(self, unit)
  end
  local statics = {}
  for _, static in pairs(self.statics) do
    local dx, dz = static.x - point.x, static.z - point.z
    if dx * dx + dz * dz <= 100 then
      statics[#statics + 1] = static
    end
  end
  table.sort(statics, function(a, b)
    return a.id < b.id
  end)
  for _, static in ipairs(statics) do
    self:doom_static(static)
    queue_deaths(self, static)
  end
end

-- Put a player in a unit, or take the player out with a nil name. No event
-- follows: DCS sends S_EVENT_BIRTH when the unit is added.
function Sim:set_player(unit, name)
  unit.player = name
end

-- An S_EVENT_TAKEOFF with `unit` as initiator on the next flush.
function Sim:takeoff(unit)
  self.queue[#self.queue + 1] = { event = 'S_EVENT_TAKEOFF', initiator = unit }
end

-- Send a HIT that kills nothing, from `shooter` (or nobody) to `unit`.
function Sim:hit(unit, shooter)
  self.queue[#self.queue + 1] = { event = 'S_EVENT_HIT', initiator = shooter, target = unit }
end

-- Kill a unit with a weapon, optionally from the unit `shooter`: S_EVENT_HIT
-- and S_EVENT_BDA on the next flush, the death events after `death_delay`
-- (LIVE).
function Sim:shoot(unit, shooter)
  if unit.static then
    if not unit.alive then
      return
    end
  elseif not self.units[unit.id] then
    return
  end
  self.queue[#self.queue + 1] = { event = 'S_EVENT_HIT', initiator = shooter, target = unit }
  self.queue[#self.queue + 1] = { event = 'S_EVENT_BDA', initiator = shooter, target = unit }
  if unit.static then
    self:doom_static(unit)
  else
    self:doom(unit)
  end
  queue_deaths(self, unit, true, shooter)
end

-- Deliver queued events and carry out deferred removals. Runs every frame,
-- paused or not; a group's removal waits while `paused`, and an item with a
-- model time `at` waits for it.
function Sim:flush(paused)
  local queue = self.queue
  local now = self.host.model or 0
  self.queue = {}
  for _, item in ipairs(queue) do
    if (paused and item.destroy_group) or (item.at and item.at > now + 1e-9) then
      self.queue[#self.queue + 1] = item
    else
      if item.event then
        self.host:unit_event(item.event, item.initiator, item.target)
      end
      if item.remove then
        self:remove_unit(item.unit)
      elseif item.destroy_group then
        self:remove_group(item.destroy_group)
      elseif item.destroy_static then
        self:remove_static(item.destroy_static)
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
    return sim.units[id] or sim.dead[id] or sim.removed[id]
  end

  -- A unit that exists or was killed, or an error: a unit that Unit.destroy
  -- or Group.destroy removed has no state to read (LIVE).
  local function known(self, method)
    local unit = sim.units[self.id_] or sim.dead[self.id_]
    if not unit then
      error('mock: ' .. method .. ' on a unit that was destroyed or never existed', 3)
    end
    return unit
  end

  local function static_of(self)
    for _, static in pairs(sim.statics) do
      if static.id == self.id_ then
        return static
      end
    end
    return sim.dead_statics[self.id_]
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
    return sim.units[self.id_] or sim.dead[self.id_] or static_of(self) or airbase_of(self)
  end

  state.Object = ns(name, 'Object', {
    isExist = function(self)
      -- A test sets `sim.exist_raises[id] = true` to make the call raise for
      -- one object, as it can in DCS for an object in transition.
      if sim.exist_raises and sim.exist_raises[self.id_] then
        error('mock: Object.isExist raises for ' .. tostring(self.id_), 2)
      end
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
      if sim.removed[self.id_] then
        error('mock: getPoint on a unit that was destroyed', 2)
      end
      local thing = any(self) or error('mock: UNVERIFIED: getPoint on an object that no longer exists', 2)
      return { x = thing.x, y = thing.y or 0, z = thing.z }
    end,
    getCategory = function(self)
      if sim.units[self.id_] or sim.dead[self.id_] then
        return 1
      end
      if sim.removed[self.id_] then
        return 0
      end
      local static = static_of(self) or error('mock: UNVERIFIED: getCategory on an object that no longer exists', 2)
      return static.cargo and 6 or 3
    end,
    getPosition = function(self)
      local unit = sim.units[self.id_] or static_of(self)
      if not unit or unit.alive == false then
        unit = live(self, 'getPosition')
      end
      local heading = unit.heading or 0
      local c, s = math.cos(heading), math.sin(heading)
      return {
        p = { x = unit.x, y = unit.y or 0, z = unit.z },
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
      local unit = sim.units[self.id_] or sim.dead[self.id_]
      if unit then
        sim.queue[#sim.queue + 1] = { remove = true, unit = unit }
        return
      end
      local static = static_of(self)
      if static then
        -- A test sets `sim.destroy_raises` to make a static's destroy raise.
        if sim.destroy_raises then
          error('mock: Object.destroy raises', 2)
        end
        if sim.searching then
          sim.destroyed_in_search = (sim.destroyed_in_search or 0) + 1
        end
        sim.destroy_calls = (sim.destroy_calls or 0) + 1
        -- A test sets `sim.destroy_keeps` for a destroy that leaves the
        -- static where it is.
        if sim.destroy_keeps then
          return
        end
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
    -- False for a unit that exists, true for a killed one (LIVE).
    isDead = function(self)
      known(self, 'isDead')
      return sim.units[self.id_] == nil
    end,
    getLife = function(self)
      return known(self, 'getLife').life
    end,
    getFuel = function(self)
      return live(self, 'getFuel').fuel
    end,
    -- The player's name, nil for an AI unit (UNVERIFIED: no run of DCS has
    -- read it).
    getPlayerName = function(self)
      return live(self, 'getPlayerName').player
    end,
    getAmmo = function(self)
      local unit = live(self, 'getAmmo')
      local out = {}
      for i, entry in ipairs(unit.kind.ammo or {}) do
        out[i] = { count = entry[2], desc = ammo_desc(entry[1]) }
      end
      if unit.kind.gun then
        out[#out + 1] = {
          count = math.floor(unit.kind.gun[2] * unit.gun / 100),
          desc = ammo_desc(unit.kind.gun[1]),
        }
      end
      return #out > 0 and out or nil
    end,
    getDesc = function(self)
      local unit = known(self, 'getDesc')
      -- `category` is the unit's: its group's category number, which
      -- Unit.Category numbers the same way (UNVERIFIED).
      return {
        typeName = unit.type,
        life = unit.kind.life,
        fuelMassMax = unit.kind.fuel_max,
        category = unit.group.category,
      }
    end,
    getLife0 = function(self)
      return live(self, 'getLife0').kind.life
    end,
    getCountry = function(self)
      return live(self, 'getCountry').group.country
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
    -- The world has no AI: orders are kept, in `sim.orders`, and obeyed by
    -- nothing. A test that wants a kill makes it (Sim:shoot).
    setTask = function(self, task)
      sim.orders[#sim.orders + 1] = { group = self.id_, task = task }
    end,
    setOption = function(self, option, value)
      if type(option) ~= 'number' then
        error('mock: Controller.setOption needs a number for the option, got ' .. type(option), 2)
      end
      sim.orders[#sim.orders + 1] = { group = self.id_, option = option, value = value }
      sim.options[self.id_] = sim.options[self.id_] or {}
      sim.options[self.id_][option] = value
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
      if not static then
        -- A dead static is found by its name too (LIVE,
        -- docs/results/2026-11-17-person-fixes.md).
        for _, dead in pairs(sim.dead_statics) do
          if dead.name == static_name then
            static = dead
            break
          end
        end
      end
      return static and handle(STATIC, static.id) or nil
    end,
    getName = function(self)
      local static = static_of(self)
      return static and static.name
    end,
    getID = function(self)
      return self.id_
    end,
    getTypeName = function(self)
      local static = static_of(self)
      return static and static.type
    end,
    -- A static's life, country, coalition and description (UNVERIFIED
    -- figures: a static has life 1 here, or the `life` a test sets).
    getLife = function(self)
      local static = static_of(self) or error('mock: UNVERIFIED: getLife on a static that no longer exists', 2)
      if static.alive and static.life ~= nil then
        return static.life
      end
      return static.alive and 1 or 0
    end,
    getCountry = function(self)
      local static = static_of(self) or error('mock: getCountry on a static that no longer exists', 2)
      return static.country
    end,
    getCoalition = function(self)
      local static = static_of(self) or error('mock: getCoalition on a static that no longer exists', 2)
      return side_of(state, static.country)
    end,
    getDesc = function(self)
      local static = static_of(self) or error('mock: getDesc on a static that no longer exists', 2)
      return { typeName = static.type, category = 0 }
    end,
  })

  -- The stock of the airbase a warehouse handle names.
  local function stock_of(self)
    return airbase_of(self).stock
  end

  local function bucket(stock, item)
    if type(item) ~= 'string' then
      error('mock: a warehouse item is a name, got ' .. type(item), 3)
    end
    return item:find('^weapons%.') and stock.weapon or stock.aircraft
  end

  local function count(n, method)
    if type(n) ~= 'number' or n ~= n or n < 0 then
      error('mock: Warehouse.' .. method .. ' takes a count of 0 or more, got ' .. tostring(n), 3)
    end
    return n
  end

  local function liquid(stock, kind)
    if type(kind) ~= 'number' or stock.liquids[kind] == nil then
      error('mock: Warehouse liquid ' .. tostring(kind) .. ' is not 0 to 3', 3)
    end
    return kind
  end

  state.Warehouse = ns(name, 'Warehouse', {
    getInventory = function(self)
      local stock = stock_of(self)
      local out = { aircraft = {}, weapon = {}, liquids = {} }
      for key, items in pairs({ aircraft = stock.aircraft, weapon = stock.weapon, liquids = stock.liquids }) do
        for k, v in pairs(items) do
          out[key][k] = v
        end
      end
      return out
    end,
    getItemCount = function(self, item)
      return bucket(stock_of(self), item)[item] or 0
    end,
    setItem = function(self, item, n)
      bucket(stock_of(self), item)[item] = count(n, 'setItem')
    end,
    addItem = function(self, item, n)
      local items = bucket(stock_of(self), item)
      items[item] = (items[item] or 0) + count(n, 'addItem')
    end,
    removeItem = function(self, item, n)
      local items = bucket(stock_of(self), item)
      items[item] = math.max(0, (items[item] or 0) - count(n, 'removeItem'))
    end,
    getLiquidAmount = function(self, kind)
      local stock = stock_of(self)
      return stock.liquids[liquid(stock, kind)]
    end,
    setLiquidAmount = function(self, kind, n)
      local stock = stock_of(self)
      stock.liquids[liquid(stock, kind)] = count(n, 'setLiquidAmount')
    end,
    addLiquid = function(self, kind, n)
      local stock = stock_of(self)
      stock.liquids[liquid(stock, kind)] = stock.liquids[kind] + count(n, 'addLiquid')
    end,
    removeLiquid = function(self, kind, n)
      local stock = stock_of(self)
      stock.liquids[liquid(stock, kind)] = math.max(0, stock.liquids[kind] - count(n, 'removeLiquid'))
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
      -- A test sets `sim.coalition_raises` to make the call raise.
      if sim.coalition_raises then
        error('mock: Airbase.getCoalition raises', 2)
      end
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
      -- UNVERIFIED: DCS refuses a side that is not 0 (neutral), 1 (red) or 2 (blue).
      if side ~= 0 and side ~= 1 and side ~= 2 then
        error('mock: Airbase.setCoalition needs 0, 1 or 2, got ' .. tostring(side), 2)
      end
      airbase_of(self).coalition = side
    end,
    -- Records the flag, in `auto_capture` on the airbase (the test's view).
    autoCapture = function(self, flag)
      if type(flag) ~= 'boolean' then
        error('mock: Airbase.autoCapture needs a boolean, got ' .. type(flag), 2)
      end
      airbase_of(self).auto_capture = flag
    end,
    getWarehouse = function(self)
      return handle({ 'Warehouse' }, self.id_)
    end,
  })

  state.coalition = ns(name, 'coalition', {
    addGroup = function(country, category, data)
      return handle(GROUP, sim:add_group(country, category, data).id)
    end,
    addStaticObject = function(country, data)
      local static = sim:add_static(country, data)
      -- A FARP (category `Heliports`) is also an airbase of its name, with
      -- the static's ID and its own empty limited warehouse (LIVE,
      -- docs/results/2026-11-16-person-tests-2.md). A FARP spawned again
      -- under the name replaces the airbase.
      if data.category == 'Heliports' then
        local base = {
          id = static.id, name = static.name, coalition = side_of(state, country), x = static.x, z = static.z,
          stock = { aircraft = {}, weapon = {}, liquids = { [0] = 0, [1] = 0, [2] = 0, [3] = 0 } },
        }
        local at = #sim.airbases + 1
        for i, b in ipairs(sim.airbases) do
          if b.name == static.name then
            at = i
          end
        end
        sim.airbases[at] = base
      end
      return handle(STATIC, static.id)
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
  local function mark_event(event_name, id, text, point, side, removed, group, extra)
    if not removed then
      sim.marks[id] = { text = text, point = point, coalition = side, group = group }
      for k, v in pairs(extra or {}) do
        sim.marks[id][k] = v
      end
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
      groupID = group or -1,
      time = host.start_time + host.model,
    })
  end

  -- A mark call checks the argument types the way DCS takes them. A test
  -- sets `sim.mark_raises` to make one raise.
  local function mark_check(call, id, text, point)
    if type(id) ~= 'number' or type(text) ~= 'string' or type(point) ~= 'table' then
      error('mock: ' .. call .. ' takes an ID, a text and a point', 3)
    end
    if sim.mark_raises then
      error('mock: ' .. call .. ' raises', 3)
    end
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
      markToAll = function(id, text, point, read_only)
        mark_check('markToAll', id, text, point)
        mark_event('S_EVENT_MARK_ADDED', id, text, point, 255, false, nil, { read_only = read_only })
      end,
      markToCoalition = function(id, text, point, side, read_only)
        mark_check('markToCoalition', id, text, point)
        mark_event('S_EVENT_MARK_ADDED', id, text, point, side, false, nil, { read_only = read_only })
      end,
      markToGroup = function(id, text, point, group, read_only)
        mark_check('markToGroup', id, text, point)
        if type(group) ~= 'number' then
          error('mock: markToGroup takes a group ID', 2)
        end
        mark_event('S_EVENT_MARK_ADDED', id, text, point, 255, false, group, { read_only = read_only })
      end,
      -- A drawing: the shape, the coalition (-1 for all) and its arguments.
      markupToAll = function(shape, side, id, ...)
        if type(shape) ~= 'number' or type(side) ~= 'number' or type(id) ~= 'number' then
          error('mock: markupToAll takes a shape, a coalition and an ID', 2)
        end
        if sim.mark_raises then
          error('mock: markupToAll raises', 2)
        end
        local first = ...
        local point = type(first) == 'table' and first.x and first or { x = 0, y = 0, z = 0 }
        mark_event('S_EVENT_MARK_ADDED', id, '', point, side == -1 and 255 or side, false, nil,
          { shape = shape, args = { ... }, drawing = true })
      end,
      lineToAll = function(side, id, from)
        mark_event('S_EVENT_MARK_ADDED', id, '', from, side == -1 and 255 or side)
      end,
      removeMark = function(id)
        local mark = sim.marks[id]
        if mark then
          sim.marks[id] = nil
          mark_event('S_EVENT_MARK_REMOVED', id, mark.text, mark.point, mark.coalition, true, mark.group)
        end
      end,
    },
  })

  -- A text call: records each call in `sim.messages` as
  -- { call, target, text, seconds, clear }. A test sets
  -- `sim.message_raises` to make one raise.
  local function message(call, targeted)
    return function(...)
      local target, text, seconds, clear = ...
      if not targeted then
        target, text, seconds, clear = nil, ...
      end
      if type(text) ~= 'string' or type(seconds) ~= 'number' or type(clear) ~= 'boolean'
        or (targeted and type(target) ~= 'number') then
        error('mock: ' .. call .. ' takes ' .. (targeted and 'a number, ' or '') .. 'a text, seconds and a boolean', 2)
      end
      if sim.message_raises then
        error('mock: ' .. call .. ' raises', 2)
      end
      sim.messages[#sim.messages + 1] = { call = call, target = target, text = text, seconds = seconds, clear = clear }
    end
  end
  state.trigger.action.outText = message('outText', false)
  state.trigger.action.outTextForCoalition = message('outTextForCoalition', true)
  state.trigger.action.outTextForCountry = message('outTextForCountry', true)
  state.trigger.action.outTextForGroup = message('outTextForGroup', true)
  state.trigger.action.outTextForUnit = message('outTextForUnit', true)

  -- F10 menus: one tree for each scope key. A path is the array of item
  -- names from the root, as DCS returns one; its form beyond that is
  -- UNVERIFIED. A command's function runs through `sim:menu_select`. A test
  -- sets `sim.menu_raises` to make an add raise.
  if names.kinds.missionCommands then
    local function root(key)
      sim.menus[key] = sim.menus[key] or { children = {} }
      return sim.menus[key]
    end
    local function find(key, path)
      local node = root(key)
      for _, part in ipairs(path or {}) do
        local found
        for _, child in ipairs(node.children or {}) do
          if child.name == part then
            found = child
            break
          end
        end
        if not found then
          return nil
        end
        node = found
      end
      return node
    end
    local function add(call, key, item_name, path, submenu, func, arg)
      if type(item_name) ~= 'string' or (path ~= nil and type(path) ~= 'table') then
        error('mock: ' .. call .. ' takes a name and a path', 3)
      end
      if not submenu and type(func) ~= 'function' then
        error('mock: ' .. call .. ' takes a function', 3)
      end
      if sim.menu_raises then
        error('mock: ' .. call .. ' raises', 3)
      end
      local parent = find(key, path)
      if not parent or not parent.children then
        error('mock: ' .. call .. ' has no submenu at that path', 3)
      end
      local node = { name = item_name, func = func, arg = arg, children = submenu and {} or nil }
      parent.children[#parent.children + 1] = node
      -- DCS numbers the actions the radio panel selects by index, from 0
      -- (LIVE, docs/results/2026-11-15-s16-players-moderation.md). The mock
      -- counts the commands in the order they were added.
      if not submenu then
        sim.actions = sim.actions or {}
        sim.next_action = (sim.next_action or -1) + 1
        node.index = sim.next_action
        sim.actions[node.index] = node
      end
      local out = {}
      for i, part in ipairs(path or {}) do
        out[i] = part
      end
      out[#out + 1] = item_name
      return out
    end
    local function remove(call, key, path)
      if type(path) ~= 'table' or #path == 0 then
        error('mock: ' .. call .. ' takes a path', 3)
      end
      local parent_path = { unpack(path, 1, #path - 1) }
      local parent = find(key, parent_path)
      for i, child in ipairs(parent and parent.children or {}) do
        if child.name == path[#path] then
          table.remove(parent.children, i)
          child.removed = true
          return
        end
      end
    end
    local function number(call, v)
      if type(v) ~= 'number' then
        error('mock: ' .. call .. ' takes a number first', 3)
      end
      return v
    end
    state.missionCommands = ns(name, 'missionCommands', {
      addSubMenu = function(n, path)
        return add('addSubMenu', 'all', n, path, true)
      end,
      addSubMenuForCoalition = function(side, n, path)
        return add('addSubMenuForCoalition', 'c' .. number('addSubMenuForCoalition', side), n, path, true)
      end,
      addSubMenuForGroup = function(group, n, path)
        return add('addSubMenuForGroup', 'g' .. number('addSubMenuForGroup', group), n, path, true)
      end,
      addCommand = function(n, path, func, arg)
        return add('addCommand', 'all', n, path, false, func, arg)
      end,
      addCommandForCoalition = function(side, n, path, func, arg)
        return add('addCommandForCoalition', 'c' .. number('addCommandForCoalition', side), n, path, false, func, arg)
      end,
      addCommandForGroup = function(group, n, path, func, arg)
        return add('addCommandForGroup', 'g' .. number('addCommandForGroup', group), n, path, false, func, arg)
      end,
      removeItem = function(path)
        remove('removeItem', 'all', path)
      end,
      removeItemForCoalition = function(side, path)
        remove('removeItemForCoalition', 'c' .. number('removeItemForCoalition', side), path)
      end,
      removeItemForGroup = function(group, path)
        remove('removeItemForGroup', 'g' .. number('removeItemForGroup', group), path)
      end,
    })
    -- The action the radio panel runs for an index: ED's panel calls
    -- missionCommands.doAction(index) (ED-CODE). An index with no live
    -- command does nothing.
    rawset(state.missionCommands, 'doAction', function(index)
      if type(index) ~= 'number' then
        error('mock: missionCommands.doAction takes a number', 2)
      end
      local node = sim.actions and sim.actions[index]
      if node and not node.removed then
        node.func(node.arg)
      end
    end)
    -- The items of a scope key, depth first, as `Name/Name` strings.
    function sim:menu_list(key)
      local out = {}
      local function walk(node, prefix)
        for _, child in ipairs(node.children or {}) do
          local label = prefix .. child.name
          out[#out + 1] = label
          walk(child, label .. '/')
        end
      end
      walk(root(key), '')
      return out
    end
    -- Select the command at `path`, as a player does: runs its function.
    function sim:menu_select(key, path)
      local node = find(key, path)
      assert(node and node.func, 'mock: no command at that path')
      return node.func(node.arg)
    end
  end

  -- A sound call: records and checks the argument types the way DCS takes
  -- them. A test sets `sim.sound_raises` to make the call raise.
  local function sound(call, targeted)
    return function(...)
      local n = select('#', ...)
      local target, file = ...
      if not targeted then
        target, file = nil, ...
      end
      if n ~= (targeted and 2 or 1) or type(file) ~= 'string' or (targeted and type(target) ~= 'number') then
        error('mock: ' .. call .. ' takes ' .. (targeted and 'a number and ' or '') .. 'a file name', 2)
      end
      if sim.sound_raises then
        error('mock: ' .. call .. ' raises', 2)
      end
      sim.sounds = sim.sounds or {}
      sim.sounds[#sim.sounds + 1] = { call = call, target = target, file = file }
    end
  end
  state.trigger.action.outSound = sound('outSound', false)
  state.trigger.action.outSoundForCoalition = sound('outSoundForCoalition', true)
  state.trigger.action.outSoundForCountry = sound('outSoundForCountry', true)
  state.trigger.action.outSoundForGroup = sound('outSoundForGroup', true)
  state.trigger.action.outSoundForUnit = sound('outSoundForUnit', true)

  state.land = ns(name, 'land', {
    SurfaceType = { LAND = 1, SHALLOW_WATER = 2, WATER = 3, ROAD = 4, RUNWAY = 5 },
    getHeight = function()
      return 0
    end,
    -- Water (3) from 20 km south of the origin, land (1) elsewhere: a
    -- made-up coast, so the mixed scenario finds water. UNVERIFIED.
    getSurfaceType = function(point)
      return (point.x or 0) < -20000 and 3 or 1
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

  -- Call `fn` for each object of the categories (a number or a list) inside
  -- the sphere or box, until it returns false: live units (1), and live and
  -- dead statics (3; LIVE, a dead static is listed).
  local function search(categories, volume, fn, data)
    local wanted = {}
    for _, c in ipairs(type(categories) == 'table' and categories or { categories }) do
      wanted[c] = true
    end
    local found = {}
    if wanted[1] then
      for _, unit in pairs(sim.units) do
        if inside(volume, unit) then
          found[#found + 1] = handle(UNIT, unit.id)
        end
      end
    end
    if wanted[3] then
      for _, list in ipairs({ sim.statics, sim.dead_statics }) do
        for _, static in pairs(list) do
          if inside(volume, static) then
            found[#found + 1] = handle(STATIC, static.id)
          end
        end
      end
    end
    -- A destroy inside the callback is a fault: DCS's search walks a list
    -- the destroy changes. `sim.destroyed_in_search` counts them.
    sim.searching = true
    for _, h in ipairs(found) do
      local ok, go_on = pcall(fn, h, data)
      if not ok then
        sim.searching = false
        error(go_on, 0)
      end
      if go_on == false then
        break
      end
    end
    sim.searching = false
  end

  if name == 'server' then
    rawset(state.world, 'getAirbases', airbases)
    rawset(state.world, 'searchObjects', search)
    rawset(state.world, 'removeJunk', function(volume)
      return sim:remove_junk(volume)
    end)
    -- The marks and drawings on the map, as a list of panels. The field
    -- names are UNVERIFIED: the mock gives id, text, pos and coalition.
    rawset(state.world, 'getMarkPanels', function()
      local out = {}
      for id, mark in pairs(sim.marks) do
        out[#out + 1] = { id = id, text = mark.text, pos = mark.point, coalition = mark.coalition }
      end
      table.sort(out, function(a, b)
        return a.id < b.id
      end)
      return out
    end)
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
      removeJunk = function(volume)
        return sim:remove_junk(volume)
      end,
    })
  end

  -- A handle to a unit or a static.
  local function handle_of(thing)
    return thing.static and handle(STATIC, thing.id) or handle(UNIT, thing.id)
  end

  -- The plain table an event carries for an object. A static has no mission
  -- ID (UNVERIFIED); its object_category is 3, or 6 for a cargo static (LIVE).
  local function data_of(thing)
    if thing.static then
      return {
        coalition = side_of(state, thing.country),
        object_category = thing.cargo and state.Object.Category.CARGO or state.Object.Category.STATIC,
        object_id = thing.id,
        unit_name = thing.name,
        unit_type = thing.type,
      }
    end
    return {
      coalition = side_of(state, thing.group.country),
      -- A string in DCS (LIVE, docs/results/2026-11-13-event-catalogue.md).
      mission_id = tostring(thing.mission_id),
      object_category = state.Object.Category.UNIT,
      object_id = thing.id,
      unit_name = thing.name,
      unit_type = thing.type,
    }
  end

  -- The event for an initiator and a target, either of them a unit or a
  -- static or nil, as this state's handlers see it.
  return function(event_name, initiator, target)
    local event = { id = state.world.event[event_name], time = host.model }
    if initiator then
      event.initiator = handle_of(initiator)
      event.initiator_data = data_of(initiator)
    end
    if target then
      event.target = handle_of(target)
      event.target_data = data_of(target)
    end
    return event
  end
end

-- A made-up projection, the same in every state that converts.
function W.latlon(x, z)
  return 42 + x / 111000, 41 + z / 80000, 0
end

function W.position(sim)
  return function(id)
    local unit = sim.units[id] or sim.dead[id]
    if not unit then
      for _, static in pairs(sim.statics) do
        if static.id == id and static.cargo then
          return static.x, static.y, static.z
        end
      end
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
    -- Send an event about a unit or static, and optionally a target, to the
    -- server state and the sandbox.
    function host:unit_event(event_name, initiator, target)
      for _, state in ipairs({ 'server', 'sse' }) do
        if self.makers[state] and self.states[state] then
          self.states[state].world.onEvent(self.makers[state](event_name, initiator, target))
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
    -- True for a unit that is not alive: killed, removed or unknown. A test
    -- sets `sim.dead_ids[mission_id]` to make a live unit read dead.
    -- `sim.dead_checks` counts the calls.
    state.c_unit_dead = function(mission_id)
      sim.dead_checks = (sim.dead_checks or 0) + 1
      if sim.dead_ids and sim.dead_ids[tonumber(mission_id)] then
        return true
      end
      for _, unit in pairs(sim.units) do
        if unit.mission_id == tonumber(mission_id) then
          return false
        end
      end
      return true
    end
    state.getValueResourceByKey = function(key)
      if type(key) ~= 'string' then
        error('mock: getValueResourceByKey takes a key', 2)
      end
      return key
    end
    -- The picture functions record { call, target, file, seconds, clear_view,
    -- delay, horizontal, vertical, size, size_units } in `sim.pictures`. A
    -- test sets `sim.picture_raises` to make a call raise.
    local function picture(call, targeted)
      return function(...)
        local args = { ... }
        local n = targeted and 9 or 8
        local at = targeted and 2 or 1
        -- a_out_picture_s takes 'red', 'blue' or 'neutrals'; a number shows
        -- nothing (LIVE), so the mock raises on one to catch the old form.
        local target_ok = not targeted or type(args[1]) == 'number'
        if call == 'a_out_picture_s' then
          target_ok = args[1] == 'red' or args[1] == 'blue' or args[1] == 'neutrals'
        end
        local ok = select('#', ...) == n and target_ok
          and type(args[at]) == 'string' and type(args[at + 2]) == 'boolean'
        for _, i in ipairs({ at + 1, at + 3, at + 4, at + 5, at + 6, at + 7 }) do
          ok = ok and type(args[i]) == 'number'
        end
        if not ok then
          error('mock: ' .. call .. ' takes the arguments of the Mission Editor action', 2)
        end
        if sim.picture_raises then
          error('mock: ' .. call .. ' raises', 2)
        end
        sim.pictures = sim.pictures or {}
        sim.pictures[#sim.pictures + 1] = {
          call = call, target = targeted and args[1] or nil, file = args[at], seconds = args[at + 1],
          clear_view = args[at + 2], delay = args[at + 3], horizontal = args[at + 4],
          vertical = args[at + 5], size = args[at + 6], size_units = args[at + 7],
        }
      end
    end
    state.a_out_picture = picture('a_out_picture', false)
    state.a_out_picture_s = picture('a_out_picture_s', true)
    state.a_out_picture_c = picture('a_out_picture_c', true)
    state.a_out_picture_g = picture('a_out_picture_g', true)
    state.a_out_picture_u = picture('a_out_picture_u', true)
    state.a_out_picture_stop = function()
      sim.picture_stops = (sim.picture_stops or 0) + 1
    end
    state.a_unit_set_life_percentage = function(mission_id, percent)
      sim.life_calls = (sim.life_calls or 0) + 1
      -- A test sets `sim.life_raises` to make the call raise.
      if sim.life_raises then
        error('mock: a_unit_set_life_percentage raises', 2)
      end
      for _, unit in pairs(sim.units) do
        if unit.mission_id == tonumber(mission_id) then
          unit.life = unit.kind.life * percent / 100
        end
      end
      for _, unit in pairs(sim.dead) do
        if unit.mission_id == tonumber(mission_id) and percent > 0 then
          sim:revive(unit)
          unit.life = unit.kind.life * percent / 100
        end
      end
    end
  else
    host.makers[name] = scripting(host, name)
  end
end

return W
