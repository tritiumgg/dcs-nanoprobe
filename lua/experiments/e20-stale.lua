-- E20: getters on the object of a unit that was removed, one that was
-- killed, and their old object IDs; and whether a later spawn reuses an ID.
-- The rule: behaviour recorded, and the validity rule (every sampled row is
-- checked against the agent's table generation) confirmed or tightened.
-- states: hook
-- timeout: 180

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    error(state .. ': ' .. tostring(value))
  end
  return value
end

local was_paused = DCS.getPause()

cross('server', [[
  local S = E.lib('scenario')
  local o = S.origin()
  E.track.global('np_e20', {})
  local function add(name)
    local group = S.ground(name, 'M-113', {
      { name = name .. '-1', x = o.x + 2500, z = o.z + #np_e20 * 40 },
      { name = name .. '-2', x = o.x + 2520, z = o.z + #np_e20 * 40 },
    })
    local unit = group:getUnit(1)
    np_e20[#np_e20 + 1] = { name = name, unit = unit, unit_name = unit:getName(), id = unit.id_ }
  end
  add('np-e20-removed')
  add('np-e20-killed')
  Group.getByName('np-e20-removed'):destroy()
  E.lib('scenario').kill('np-e20-killed-1', 500)
  return 'armed'
]])

local READ = [[
  local out = {}
  for _, row in ipairs(np_e20) do
    local unit = row.unit
    local function try(fn, ...)
      local r = { pcall(fn, ...) }
      local parts = {}
      for i = 1, select('#', unpack(r)) do
        local v = r[i]
        parts[#parts + 1] = type(v) == 'table' and 'table' or tostring(v)
      end
      return table.concat(parts, ',')
    end
    out[#out + 1] = row.name .. ': isExist=' .. try(unit.isExist, unit)
      .. ' getLife=' .. try(unit.getLife, unit)
      .. ' getPoint=' .. try(unit.getPoint, unit)
      .. ' getName=' .. try(unit.getName, unit)
      .. ' byObjectId=' .. try(DCS.getUnitPositionByObjectId, row.id)
      .. ' getByName=' .. tostring(Unit.getByName(row.unit_name) ~= nil)
  end
  return table.concat(out, ' | ')
]]

local step, wait_until = 'settle', nil

return E.wait(function()
  if DCS.getPause() then
    DCS.setPause(false)
    return nil
  end
  wait_until = wait_until or DCS.getRealTime() + 5
  if DCS.getRealTime() < wait_until then
    return nil
  end
  if step == 'settle' then
    E.observe('stale', cross('server', READ), { info = true })
    -- Spawn 500 units, and the removed group again under its old name.
    E.observe('reuse', cross('server', [[
      local S = E.lib('scenario')
      local old = {}
      for _, row in ipairs(np_e20) do old[row.id] = row.name end
      local names = S.spawn_grid(500, { label = 'e20', offset = 4000 })
      local o = S.origin()
      S.ground('np-e20-removed', 'M-113', {
        { name = 'np-e20-removed-1', x = o.x + 2500, z = o.z },
        { name = 'np-e20-removed-2', x = o.x + 2520, z = o.z },
      })
      local reused, min, max = {}, nil, nil
      local function seen(unit)
        local id = unit.id_
        if old[id] then reused[#reused + 1] = old[id] .. '=' .. id end
        min = math.min(min or id, id)
        max = math.max(max or id, id)
      end
      for _, name in ipairs(names) do
        for _, unit in ipairs(Group.getByName(name):getUnits()) do seen(unit) end
      end
      local again = Group.getByName('np-e20-removed'):getUnit(1)
      seen(again)
      local old_ids = {}
      for _, row in ipairs(np_e20) do old_ids[#old_ids + 1] = row.id end
      return string.format('old ids %s; new ids %d..%d; reused [%s]; re-added unit id %d',
        table.concat(old_ids, ','), min, max, table.concat(reused, ','), again.id_)
    ]]), { info = true })
    E.observe('stale_after_reuse', cross('server', READ), { info = true })
    cross('server', "E.lib('scenario').clear() return 'cleared'")
    step = 'clear'
    wait_until = DCS.getRealTime() + 3
    return nil
  end
  DCS.setPause(was_paused)
  return 'done'
end, 170)
