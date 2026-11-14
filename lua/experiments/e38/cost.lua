-- E38: what each way of reading life and ammunition costs a unit, cold.
--
-- 500 idle carriers and 100 tanks. On every frame the snippet reads one
-- batch of 20 units with one read, and times the batch; the read changes
-- from frame to frame and the batch moves on through the units, so a whole
-- simulation frame lies between two batches and no unit is read twice in a
-- row. That is how a sampler meets a row: never warm. Each read gets 150
-- batches; the figure is the median batch over 20, with the median of an
-- empty Lua call taken off. A back-to-back pass over every unit, best of
-- seven, is recorded beside it for comparison with earlier figures.
--
-- Every read takes a plain table `{ id_ = <object ID> }`, as the agent's
-- sampler passes, not an object from Unit.getByName.
-- states: server
-- timeout: 300

local E = ...
local S = E.lib('scenario')
local BATCH, BATCHES = 20, 150

local function exists(table_name, name)
  return (pcall(function()
    assert(type(_G[table_name][name]) == 'function')
  end))
end

-- name, the units it reads ('all' or 'tanks'), and the call.
local READS = {
  { 'empty', 'all', function() end },
  { 'Unit.getLife', 'all', function(h) return Unit.getLife(h) end },
  { 'Unit.getFuel', 'all', function(h) return Unit.getFuel(h) end },
  { 'Unit.getLife0', 'all', function(h) return Unit.getLife0(h) end },
  { 'Unit.isAlive', 'all', function(h) return Unit.isAlive(h) end },
  { 'Unit.isBroken', 'all', function(h) return Unit.isBroken(h) end },
  { 'Unit.isDead', 'all', function(h) return Unit.isDead(h) end },
  { 'Unit.isEffective', 'all', function(h) return Unit.isEffective(h) end },
  { 'Unit.isActive', 'all', function(h) return Unit.isActive(h) end },
  { 'Object.isExist', 'all', function(h) return Object.isExist(h) end },
  { 'Unit.getDrawArgumentValue', 'all', function(h) return Unit.getDrawArgumentValue(h, 0) end },
  { 'Unit.getAmmo carrier', 'carriers', function(h) return Unit.getAmmo(h) end },
  { 'Unit.getAmmo tank', 'tanks', function(h) return Unit.getAmmo(h) end },
  { 'DCS.getUnitPositionByObjectId', 'all', function(h) return DCS.getUnitPositionByObjectId(h.id_) end },
  { 'Export.LoGetObjectById', 'all', function(h) return Export.LoGetObjectById(h.id_) end },
}

S.spawn_grid(500, { label = 'e38k', offset = 2000, kind = 'M-113' })
S.spawn_grid(100, { label = 'e38m', offset = 9000, kind = 'M-1 Abrams' })

local sets = { all = {}, carriers = {}, tanks = {} }
local function collect()
  for _, set in pairs(sets) do
    for i = #set, 1, -1 do
      set[i] = nil
    end
  end
  for i = 1, 500 do
    local unit = Unit.getByName('np-e38k-' .. i)
    if unit then
      local handle = { id_ = rawget(unit, 'id_') }
      sets.all[#sets.all + 1] = handle
      sets.carriers[#sets.carriers + 1] = handle
    end
  end
  for i = 1, 100 do
    local unit = Unit.getByName('np-e38m-' .. i)
    if unit then
      local handle = { id_ = rawget(unit, 'id_') }
      sets.all[#sets.all + 1] = handle
      sets.tanks[#sets.tanks + 1] = handle
    end
  end
  return #sets.all
end

local function median(list)
  table.sort(list)
  return list[math.ceil(#list / 2)]
end

local clock = E.clock
local times, cursors, usable = {}, {}, {}
local frame, settle_until
local started = clock()

return E.wait(function()
  if not settle_until then
    if collect() < 600 then
      return clock() - started > 60 and error('the units did not appear') or nil
    end
    -- Leave the spawn's own work behind.
    settle_until = clock() + 5
    for i, read in ipairs(READS) do
      times[i], cursors[i] = {}, 0
      usable[i] = pcall(read[3], sets[read[2]][1])
    end
    frame = 0
    return nil
  end
  if clock() < settle_until then
    return nil
  end
  frame = frame + 1
  local index = (frame - 1) % #READS + 1
  local read = READS[index]
  if usable[index] and #times[index] < BATCHES then
    local set, fn, cursor = sets[read[2]], read[3], cursors[index]
    local n = #set
    local t0 = clock()
    for i = 1, BATCH do
      fn(set[(cursor + i - 1) % n + 1])
    end
    local t = clock() - t0
    cursors[index] = (cursor + BATCH) % n
    times[index][#times[index] + 1] = t
  end
  if frame < #READS * BATCHES then
    return nil
  end

  local empty = median(times[1]) / BATCH
  local cold, warm, absent = {}, {}, {}
  for i, read in ipairs(READS) do
    if usable[i] then
      local sorted = times[i]
      local mid = median(sorted) / BATCH
      cold[read[1]] = {
        median_ns = math.floor((mid - empty) * 1e9 + 0.5),
        p10_ns = math.floor((sorted[math.ceil(#sorted / 10)] / BATCH - empty) * 1e9 + 0.5),
        p90_ns = math.floor((sorted[math.ceil(#sorted * 9 / 10)] / BATCH - empty) * 1e9 + 0.5),
      }
      local set, fn = sets[read[2]], read[3]
      local best
      for _ = 1, 7 do
        local t0 = clock()
        for j = 1, #set do
          fn(set[j])
        end
        local t = (clock() - t0) / #set
        best = (not best or t < best) and t or best
      end
      warm[read[1]] = math.floor(best * 1e9 + 0.5)
    else
      absent[#absent + 1] = read[1]
    end
  end
  warm.empty_is_included = true
  E.observe('cold_a_call', cold, { timing = true })
  E.observe('back_to_back_ns_a_call', warm, { timing = true })
  E.observe('raised_or_absent', absent, { info = true })
  E.observe('units', { all = #sets.all, tanks = #sets.tanks, batch = BATCH, batches = BATCHES, exists_getLife = exists('Unit', 'getLife') }, { info = true })
  S.clear()
  return 'done'
end, 280, 'running')
