-- E38: the mission state's trigger rules as readers of life.
--
-- The mission state holds the functions a mission's trigger conditions
-- compile to: c_unit_damaged, c_unit_hit, c_unit_dead, c_unit_life_less,
-- c_group_life_less, and the action a_unit_set_life_percentage (ED-CODE,
-- MissionEditor/modules/me_predicates.lua and me_trigrules.lua give their
-- arguments). This snippet runs in the hook state and crosses into `server`
-- to add a group of four carriers and to damage it, and into `mission` to
-- call the rules before and after. It records:
--   - whether each rule works for a group added at run time;
--   - the percent at which c_group_life_less and c_unit_life_less turn true,
--     found by bisection, beside the lives read with Unit.getLife;
--   - whether a_unit_set_life_percentage moves getLife down and up again;
--   - what c_unit_dead says of a unit an explosion killed and of one that
--     Unit.destroy removed;
--   - which world events each step sent (a handler in `server` counts them);
--   - the cost of a call, timed from the hook around a loop in the mission
--     state, with an empty loop taken off.
-- states: hook
-- timeout: 120

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    return 'CROSS FAILED ' .. tostring(value)
  end
  return value
end

local SPAWN = [[
local S = E.lib('scenario')
local o = S.origin()
local x, z = o.x + 3000, o.z + 12000
local units = {}
for i = 1, 4 do units[i] = { name = 'np-e38-r' .. i, x = x, z = z + (i - 1) * 8, heading = 0 } end
S.ground('np-e38-r', 'M-113', units)
local names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then names[id] = name end
end
E.store.e38_rules = {}
local seen = E.store.e38_rules
E.track.handler({ onEvent = function(_, event)
  seen[#seen + 1] = names[event.id] or tostring(event.id)
end })
return 'spawned'
]]

-- The events since the last call, as text, and forget them.
local EVENTS = [[
local seen = E.store.e38_rules or {}
local text = table.concat(seen, ' ')
for i = #seen, 1, -1 do seen[i] = nil end
return text
]]

local IDS = [[
local group = Group.getByName('np-e38-r')
if not group then return '' end
local out = { tostring(group:getID()) }
for i = 1, 4 do
  local unit = Unit.getByName('np-e38-r' .. i)
  if not unit then return '' end
  out[#out + 1] = tostring(unit:getID())
end
return table.concat(out, ' ')
]]

local LIVES = [[
local out = {}
for i = 1, 4 do
  local unit = Unit.getByName('np-e38-r' .. i)
  out[i] = unit and string.format('%.4f/%.4f', unit:getLife(), unit:getLife0()) or 'gone'
end
return table.concat(out, ' ')
]]

local EXPLODE = [[
local unit = Unit.getByName('np-e38-r1')
local p = unit:getPoint()
trigger.action.explosion({ x = p.x, y = p.y, z = p.z - 6 }, 30)
return 'exploded'
]]

local DESTROY = "Unit.getByName('np-e38-r3'):destroy() return 'destroyed'"

local CLEAR = "E.lib('scenario').clear() return 'cleared'"

-- In the mission state: every rule once, and the two thresholds by bisection.
local function rules_source(gid, uids)
  return string.format([[
local gid, uids = %s, { %s }
local function call(name, ...)
  local fn = _G[name]
  if type(fn) ~= 'function' then return name .. '=absent' end
  local ok, value = pcall(fn, ...)
  return name .. '=' .. (ok and tostring(value) or 'ERR')
end
local function threshold(fn, id)
  local ok = pcall(fn, id, 50)
  if not ok then return 'ERR' end
  if not fn(id, 1000) then return 'never' end
  local low, high = 0, 1000
  for _ = 1, 40 do
    local mid = (low + high) / 2
    if fn(id, mid) then high = mid else low = mid end
  end
  return string.format('%%.3f', high)
end
local out = {}
out[#out + 1] = 'group_less_threshold=' .. threshold(c_group_life_less, gid)
out[#out + 1] = call('c_group_life_less', 'np-e38-r', 1000) .. '(by name)'
for i, uid in ipairs(uids) do
  out[#out + 1] = string.format('u%%d: %%s %%s %%s less_threshold=%%s', i, call('c_unit_damaged', uid), call('c_unit_hit', uid, 1),
    call('c_unit_dead', uid), threshold(c_unit_life_less, uid))
end
return table.concat(out, '; ')
]], gid, table.concat(uids, ', '))
end

local function timing_source(body, gid, uid, n)
  return string.format('local gid, uid = %s, %s for _ = 1, %d do %s end return 1', gid, uid, n, body)
end

local gid, uids
local step, wait_until = 1, 0
cross('server', SPAWN)

return E.wait(function()
  local now = DCS.getRealTime()
  if now < wait_until then
    return nil
  end
  if step == 1 then
    local text = cross('server', IDS)
    if text == '' or text:find('CROSS FAILED', 1, true) then
      return nil
    end
    uids = {}
    for id in text:gmatch('%S+') do
      uids[#uids + 1] = id
    end
    gid = table.remove(uids, 1)
    E.observe('ids', { group = gid, units = uids }, { info = true })
    E.observe('lives_before', cross('server', LIVES), { info = true })
    E.observe('rules_before', cross('mission', rules_source(gid, uids)), { info = true })
    E.observe('events_at_spawn', cross('server', EVENTS), { info = true })
    E.observe('explode', cross('server', EXPLODE), { info = true })
    step, wait_until = 2, now + 3
  elseif step == 2 then
    E.observe('lives_after_explosion', cross('server', LIVES), { info = true })
    E.observe('rules_after_explosion', cross('mission', rules_source(gid, uids)), { info = true })
    E.observe('events_after_explosion', cross('server', EVENTS), { info = true })
    E.observe('set_50', cross('mission', string.format("a_unit_set_life_percentage(%s, 50) return 'set'", uids[4])), { info = true })
    step, wait_until = 3, now + 2
  elseif step == 3 then
    E.observe('lives_after_set_50', cross('server', LIVES), { info = true })
    E.observe('rules_after_set_50', cross('mission', rules_source(gid, uids)), { info = true })
    E.observe('events_after_set_50', cross('server', EVENTS), { info = true })
    E.observe('set_100', cross('mission', string.format("a_unit_set_life_percentage(%s, 100) return 'set'", uids[4])), { info = true })
    step, wait_until = 4, now + 2
  elseif step == 4 then
    E.observe('lives_after_set_100', cross('server', LIVES), { info = true })
    E.observe('events_after_set_100', cross('server', EVENTS), { info = true })
    E.observe('destroy_unit_3', cross('server', DESTROY), { info = true })
    step, wait_until = 5, now + 2
  elseif step == 5 then
    E.observe('rules_after_destroy', cross('mission', rules_source(gid, uids)), { info = true })
    E.observe('events_after_destroy', cross('server', EVENTS), { info = true })
    -- Cost: 2,000 calls in one chunk, best of five, an empty loop taken off.
    local N = 2000
    local function best(body)
      local source = timing_source(body, gid, uids[4], N)
      local least
      for _ = 1, 5 do
        local t0 = DCS.getRealTime()
        net.dostring_in('mission', source)
        local t = DCS.getRealTime() - t0
        least = (not least or t < least) and t or least
      end
      return least
    end
    local empty = best('')
    local costs = {}
    for name, body in pairs({
      c_group_life_less = 'c_group_life_less(gid, 50)',
      c_unit_life_less = 'c_unit_life_less(uid, 50)',
      c_unit_damaged = 'c_unit_damaged(uid)',
      c_unit_hit = 'c_unit_hit(uid, 1)',
      c_unit_dead = 'c_unit_dead(uid)',
    }) do
      costs[name] = math.floor((best(body) - empty) / N * 1e9 + 0.5)
    end
    E.observe('ns_a_call_warm', costs, { timing = true })
    E.observe('empty_chunk_us', math.floor(empty * 1e6 + 0.5), { timing = true })
    cross('server', CLEAR)
    return 'done'
  end
  return nil
end, 100, 'running')
