-- The churn of the soak test's control run, which has no agent loaded: it
-- does to DCS with plain scripting calls what the run with the agent does
-- through the platform's API. Load it with E.lib('soak') in the `server`
-- state.
--
-- Development only. Groups are named `ctl-<place>-<generation>`, units
-- `<group>-<i>`; a name is never reused. The state is one global, so it
-- survives between snippets in the same Lua state.

local L = ...
local S = L.lib('scenario')
local K = {}
local KEY = '__nanoprobe_soak'

-- DCS's country ID for the USA is 2 (PRIOR: the model index of a running DCS,
-- lua/mock/api/server.txt).
local DEFAULT_COUNTRY = 2

local function state()
  return _G[KEY]
end

-- A route point with the fields scenario.lua's `waypoint` gives a ground
-- point (ED-CODE: the route points of any saved mission file).
local function point(x, z, speed)
  return {
    x = x,
    y = z,
    type = 'Turning Point',
    action = 'Off Road',
    speed = speed,
    speed_locked = true,
    ETA = 0,
    ETA_locked = false,
    formation_template = '',
    task = { id = 'ComboTask', params = { tasks = {} } },
  }
end

local function units_of(name)
  local group = Group.getByName(name)
  if not group then
    return {}
  end
  return group:getUnits() or {}
end

-- Spawn a group at place `p` and record it. Returns the unit count.
local function spawn(st, p)
  st.gen[p] = (st.gen[p] or 0) + 1
  local name = string.format('ctl-%d-%d', p, st.gen[p])
  -- A place is a map point, north and east in meters: the soak with the
  -- agent puts its groups at the same points.
  local units = {}
  for i = 1, st.units do
    units[i] = {
      name = name .. '-' .. i,
      x = st.places[p][1],
      z = st.places[p][2] + (i - 1) * st.spacing,
      heading = 0,
    }
  end
  S.ground(name, st.type, units, nil, st.country)
  st.seq = st.seq + 1
  local entry = { place = p, name = name, seq = st.seq, tick = st.ticks }
  st.live[#st.live + 1] = entry
  st.occupied[p] = entry
  st.spawned_total = st.spawned_total + st.units
  return st.units
end

-- Destroy a group and drop it from the bookkeeping. Returns the units that
-- existed when it went.
local function destroy(st, entry)
  local n = #units_of(entry.name)
  S.destroy(entry.name)
  for i, e in ipairs(st.live) do
    if e == entry then
      table.remove(st.live, i)
      break
    end
  end
  st.occupied[entry.place] = nil
  st.destroyed_total = st.destroyed_total + n
  return n
end

function K.stats()
  local st = state()
  local out = { groups = 0, units = 0, max_id = 0, live = 0, spawned_units_total = 0, destroyed_units_total = 0,
    killed_units_total = 0, ticks = 0 }
  if not st then
    return out
  end
  out.live = #st.live
  for _, entry in ipairs(st.live) do
    local ok, units = pcall(units_of, entry.name)
    if ok then
      if Group.getByName(entry.name) then
        out.groups = out.groups + 1
      end
      for _, u in ipairs(units) do
        out.units = out.units + 1
        -- The runtime object ID is the object's `id_` field, which DCS steps
        -- by 256 per object (UNVERIFIED). Unit.getID is the mission's ID.
        local id = tonumber(u.id_)
        if id and id > out.max_id then
          out.max_id = id
        end
      end
    end
  end
  out.spawned_units_total, out.destroyed_units_total = st.spawned_total, st.destroyed_total
  out.killed_units_total, out.ticks = st.killed_total, st.ticks
  return out
end

function K.clear()
  local st = state()
  local names = {}
  for name in pairs(S.groups) do
    if name:find('^ctl%-') then
      names[#names + 1] = name
    end
  end
  local n = 0
  for _, name in ipairs(names) do
    if Group.getByName(name) then
      n = n + 1
    end
    pcall(S.destroy, name)
  end
  if st then
    for _, entry in ipairs(st.live) do
      pcall(S.destroy, entry.name)
    end
  end
  _G[KEY] = nil
  return n
end

function K.setup(o)
  K.clear()
  local st = {
    places = o.places, live_count = o.live or 250, units = o.units or 4, type = o.type or 'M-113',
    country = o.country or DEFAULT_COUNTRY, spacing = o.spacing or 12, loop = o.loop or 200, speed = o.speed or 5,
    gen = {}, live = {}, occupied = {}, seq = 0, ticks = 0, cursor = 1,
    spawned_total = 0, destroyed_total = 0, killed_total = 0,
  }
  _G[KEY] = st
  for p = 1, math.min(st.live_count, #st.places) do
    pcall(spawn, st, p)
    st.cursor = p % #st.places + 1
  end
  return K.stats()
end

function K.tick(o)
  o = o or {}
  local st = state()
  local r = { spawned_units = 0, destroyed_units = 0, killed_units = 0, orders = 0, errors = {} }
  if not st then
    r.errors[1] = 'no setup'
    r.stats = K.stats()
    return r
  end
  st.ticks = st.ticks + 1
  -- Run one step under pcall; a failure becomes a string and the tick goes on.
  local function step(label, fn)
    local ok, err = pcall(fn)
    if not ok then
      r.errors[#r.errors + 1] = label .. ': ' .. tostring(err)
    end
  end
  local function replace(entry)
    local place = entry.place
    step('destroy ' .. entry.name, function()
      r.destroyed_units = r.destroyed_units + destroy(st, entry)
    end)
    step('spawn at place ' .. place, function()
      r.spawned_units = r.spawned_units + spawn(st, place)
    end)
  end

  -- 1. The group the last tick killed: what is left of it goes, and a new
  -- group takes its place.
  if st.doomed then
    local entry = st.doomed
    st.doomed = nil
    entry.doomed = nil
    replace(entry)
  end

  -- 2. Handover: the oldest groups leave and groups appear at free places
  -- further round the ring. Free places are counted before anything goes, so a
  -- ring with no room reports it and leaves the groups alone.
  local handover = o.handover or 0
  if handover > 0 then
    local free = {}
    local n = #st.places
    for k = 0, n - 1 do
      local p = (st.cursor - 1 + k) % n + 1
      if not st.occupied[p] and #free < handover then
        free[#free + 1] = p
      end
    end
    if #free < handover then
      r.errors[#r.errors + 1] = string.format('handover: %d free places for %d groups', #free, handover)
    else
      local old = {}
      for i = 1, math.min(handover, #st.live) do
        old[i] = st.live[i]
      end
      for _, entry in ipairs(old) do
        step('destroy ' .. entry.name, function()
          r.destroyed_units = r.destroyed_units + destroy(st, entry)
        end)
      end
      for _, p in ipairs(free) do
        step('spawn at place ' .. p, function()
          r.spawned_units = r.spawned_units + spawn(st, p)
        end)
        st.cursor = p % n + 1
      end
    end
  end

  -- 3. Replace: the next oldest groups are replaced where they stand.
  local old = {}
  for i = 1, math.min(o.replace or 0, #st.live) do
    old[i] = st.live[i]
  end
  for _, entry in ipairs(old) do
    replace(entry)
  end

  -- 4. Orders: a square loop of `loop` meters from the group's place, four
  -- laps, off road, given through the group's controller.
  local given = 0
  for _, entry in ipairs(st.live) do
    if given >= (o.orders or 0) then
      break
    end
    if not entry.routed and not entry.doomed then
      step('order ' .. entry.name, function()
        local x, z = st.places[entry.place][1], st.places[entry.place][2]
        local side = st.loop
        local points = { point(x, z, st.speed) }
        for _ = 1, 4 do
          points[#points + 1] = point(x + side, z, st.speed)
          points[#points + 1] = point(x + side, z + side, st.speed)
          points[#points + 1] = point(x, z + side, st.speed)
          points[#points + 1] = point(x, z, st.speed)
        end
        Group.getByName(entry.name):getController():setTask({ id = 'Mission', params = { route = { points = points } } })
        entry.routed = true
        r.orders = r.orders + 1
        given = given + 1
      end)
    end
  end

  -- 5. Kill: explode every unit of the oldest group that this tick did not
  -- spawn and that has no route, as the soak with the agent picks a group
  -- that stands. It stays in the live list until the next tick replaces it.
  if o.kill then
    for _, entry in ipairs(st.live) do
      if not entry.doomed and not entry.routed and entry.tick ~= st.ticks then
        step('kill ' .. entry.name, function()
          for _, u in ipairs(units_of(entry.name)) do
            if u:isExist() then
              trigger.action.explosion(u:getPoint(), 300)
              r.killed_units = r.killed_units + 1
            end
          end
        end)
        entry.doomed = true
        st.doomed = entry
        st.killed_total = st.killed_total + r.killed_units
        break
      end
    end
  end

  r.stats = K.stats()
  return r
end

return K
