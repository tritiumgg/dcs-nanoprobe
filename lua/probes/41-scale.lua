-- Sweeps over 500, 1,000 and 2,000 idle ground units, and what spawning and
-- removing them costs.
-- states: server
-- timeout: 300

local E = ...
local S = E.lib('scenario')
local read = E.clock

-- Milliseconds for one pass of `fn`, best of five.
local function ms(fn)
  local best
  for _ = 1, 5 do
    local t0 = read()
    fn()
    local t = read() - t0
    if not best or t < best then
      best = t
    end
  end
  return best * 1000
end

local function median(list)
  local sorted = { unpack(list) }
  table.sort(sorted)
  return sorted[math.ceil(#sorted / 2)]
end

-- Baselines: M-113s, 10 per group, on a rendering client at 60 fps, with
-- one more unit in the mission (LIVE there). Milliseconds.
local baseline = {
  [500] = { addGroup = 3.8, enumerate = 0.19, byObjectId = 0.046, getPoint = 0.35, getVelocity = 0.33,
    getPosition = 1.31, getLife = 0.046, getFuel = 0.047, isExist = 0.075, getName = 0.053, getAmmo = 0.78,
    LoGetWorldObjects = 3.1, searchObjects = 0.16 },
  [1000] = { addGroup = 5.0, enumerate = 0.44, byObjectId = 0.106, getPoint = 0.77, getVelocity = 0.72,
    getPosition = 2.88, getLife = 0.10, getFuel = 0.14, isExist = 0.16, getName = 0.17, getAmmo = 1.92,
    LoGetWorldObjects = 7.6, searchObjects = 0.35 },
  [2000] = { addGroup = 5.3, enumerate = 1.0, byObjectId = 0.316, getPoint = 1.55, getVelocity = 1.47,
    getPosition = 5.86, getLife = 0.20, getFuel = 0.21, isExist = 0.32, getName = 0.31, getAmmo = 3.45,
    LoGetWorldObjects = 14.1, searchObjects = 0.71 },
}

local spawned, groups = 0, {}

local function spawn(target)
  local names, costs = S.spawn_grid(target - spawned, { label = 'scale', start = spawned, offset = 1500 })
  for _, name in ipairs(names) do
    groups[#groups + 1] = name
  end
  spawned = target
  E.observe('n' .. target .. '.addGroup_ms', median(costs) * 1000, { windows = baseline[target].addGroup, timing = true })
end

local function measure(target)
  local base = baseline[target]
  local prefix = 'n' .. target .. '.'
  local units, ids = {}, {}
  local function enumerate()
    units, ids = {}, {}
    for _, group in ipairs(coalition.getGroups(coalition.side.BLUE, Group.Category.GROUND)) do
      for _, unit in ipairs(group:getUnits()) do
        units[#units + 1] = unit
        ids[#ids + 1] = unit.id_
      end
    end
  end
  local function sweep(name, fn)
    E.observe(prefix .. name .. '_ms', ms(fn), { windows = base[name], timing = true })
  end
  sweep('enumerate', enumerate)
  E.observe(prefix .. 'units', #units, { windows = target })
  local function each(method)
    return function()
      for i = 1, #units do
        local unit = units[i]
        unit[method](unit)
      end
    end
  end
  local position = DCS.getUnitPositionByObjectId
  sweep('byObjectId', function()
    for i = 1, #ids do
      position(ids[i])
    end
  end)
  for _, method in ipairs({ 'getPoint', 'getVelocity', 'getPosition', 'getLife', 'getFuel', 'isExist', 'getName',
    'getAmmo' }) do
    sweep(method, each(method))
  end
  sweep('LoGetWorldObjects', Export.LoGetWorldObjects)
  local origin = S.origin()
  local volume = { id = world.VolumeType.SPHERE, params = { point = origin, radius = 20000 } }
  local found = 0
  sweep('searchObjects', function()
    found = 0
    world.searchObjects(Object.Category.UNIT, volume, function()
      found = found + 1
      return true
    end)
  end)
  E.observe(prefix .. 'searchObjects_found', found, { info = true })
  E.observe(prefix .. 'lua_heap_mb', collectgarbage('count') / 1024, { info = true })
end

local plan = {}
for _, target in ipairs({ 500, 1000, 2000 }) do
  plan[#plan + 1] = function() spawn(target) end
  plan[#plan + 1] = false
  plan[#plan + 1] = function() measure(target) end
end
local step, gone_since = 0, nil

return E.wait(function()
  step = step + 1
  if plan[step] then
    plan[step]()
    return nil
  elseif plan[step] == false then
    return nil
  end
  if not gone_since then
    -- Group:destroy() returns in about 0.5 us per group, and the work is
    -- done later (LIVE).
    local t0 = read()
    for _, name in ipairs(groups) do
      S.destroy(name)
    end
    E.observe('destroy_us_per_group', (read() - t0) / #groups * 1e6, { windows = 0.5, timing = true })
    gone_since = read()
    return nil
  end
  for _, name in ipairs(groups) do
    if Group.getByName(name) then
      return nil
    end
  end
  E.observe('seconds_until_gone', read() - gone_since, { info = true })
  return 'done'
end, 240, 'running')
