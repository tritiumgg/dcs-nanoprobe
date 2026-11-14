-- Whether events name every unit whose life or ammunition changes, and
-- what fuel does over time. Two blue and two red tanks 600 m apart open
-- fire; three more vehicles stand by an explosion that damages without
-- killing. Five times a second the probe reads each one's life and
-- ammunition itself, and it keeps every event that names one of them. For
-- each change of life or ammunition it reports the nearest event naming the
-- unit: as a hit's target for life, as a shooter for ammunition. It also
-- reads the fuel of one unit of each kind in the scenario at the start and
-- at the end. It destroys what it added.
-- states: server
-- timeout: 240
local E = ...
local S = E.lib('scenario')

local EVENTS = {}
for _, name in ipairs({
  'S_EVENT_SHOT', 'S_EVENT_HIT', 'S_EVENT_KILL', 'S_EVENT_DEAD', 'S_EVENT_UNIT_LOST', 'S_EVENT_BDA',
  'S_EVENT_SHOOTING_START', 'S_EVENT_SHOOTING_END', 'S_EVENT_WEAPON_ADD', 'S_EVENT_WEAPON_REARM',
  'S_EVENT_CRASH', 'S_EVENT_DAMAGE',
}) do
  local ok, id = pcall(function()
    return world.event[name]
  end)
  if ok and type(id) == 'number' then
    EVENTS[id] = name
  end
end

-- Fuel of one unit of each kind that exists now.
local KIND = { [0] = 'airplane', [1] = 'helicopter', [2] = 'ground', [3] = 'ship' }
local fuel = {}
for s = 0, 2 do
  for _, g in ipairs(coalition.getGroups(s) or {}) do
    for _, u in ipairs(g:getUnits() or {}) do
      local ok, d = pcall(u.getDesc, u)
      local kind = KIND[ok and type(d) == 'table' and d.category or -1] or 'other'
      local v = u:getVelocity()
      local moving = v.x * v.x + v.y * v.y + v.z * v.z > 0.25
      local key = kind .. (moving and '_moving' or '_halted')
      if not fuel[key] then
        fuel[key] = { unit = u, type = u:getTypeName(), first = u:getFuel(), at = timer.getTime() }
      end
    end
  end
end

local origin = S.origin()
local x, z = origin.x + 1500, origin.z - 2500
local blue = S.ground('np-design-blue', 'M-1 Abrams', {
  { name = 'np-design-b1', x = x, z = z, heading = 0 },
  { name = 'np-design-b2', x = x, z = z + 40, heading = 0 },
})
local red = S.ground('np-design-red', 'T-72B', {
  { name = 'np-design-r1', x = x + 600, z = z, heading = math.pi },
  { name = 'np-design-r2', x = x + 600, z = z + 40, heading = math.pi },
}, nil, country.id.RUSSIA)
-- Three vehicles 12 m apart, far from the fight, for the explosion.
S.ground('np-design-near', 'M-113', {
  { name = 'np-design-n1', x = x - 800, z = z, heading = 0 },
  { name = 'np-design-n2', x = x - 800, z = z + 12, heading = 0 },
  { name = 'np-design-n3', x = x - 800, z = z + 24, heading = 0 },
})
local fire = { blue = S.open_fire(blue), red = S.open_fire(red) }

local NAMES = {
  'np-design-b1', 'np-design-b2', 'np-design-r1', 'np-design-r2', 'np-design-n1', 'np-design-n2', 'np-design-n3',
}
-- Each watched unit by object ID.
local watched, by_name = {}, {}
for _, name in ipairs(NAMES) do
  local u = Unit.getByName(name)
  if u then
    local w = { name = name, unit = u, life = {}, ammo = {}, events = {}, alive = true }
    watched[rawget(u, 'id_')] = w
    by_name[name] = w
  end
end

local function rounds(u)
  local ok, a = pcall(u.getAmmo, u)
  if not ok or type(a) ~= 'table' then
    return 0
  end
  local n = 0
  for _, entry in ipairs(a) do
    n = n + (type(entry) == 'table' and tonumber(entry.count) or 0)
  end
  return n
end

local seen = {}
local handler = {}
function handler:onEvent(event)
  pcall(function()
    local id = rawget(event, 'id')
    local name = EVENTS[id] or tostring(id)
    seen[name] = (seen[name] or 0) + 1
    local t = timer.getTime()
    local i, g = rawget(event, 'initiator'), rawget(event, 'target')
    local ii = type(i) == 'table' and rawget(i, 'id_') or nil
    local gi = type(g) == 'table' and rawget(g, 'id_') or nil
    local wi, wg = ii and watched[ii], gi and watched[gi]
    if wi and #wi.events < 400 then
      wi.events[#wi.events + 1] = { t = t, event = name, as = 'initiator', target = gi ~= nil }
    end
    if wg and #wg.events < 400 then
      wg.events[#wg.events + 1] = { t = t, event = name, as = 'target', initiator = ii ~= nil }
    end
  end)
end
world.addEventHandler(handler)

local function sample()
  local t = timer.getTime()
  for _, w in pairs(watched) do
    if w.alive then
      local ok, exists = pcall(w.unit.isExist, w.unit)
      if not ok or not exists then
        w.alive = false
        w.gone_at = t
      else
        local life, ammo = w.unit:getLife(), rounds(w.unit)
        if w.last_life ~= life and #w.life < 200 then
          w.life[#w.life + 1] = { t = t, from = w.last_life, to = life }
        end
        if w.last_ammo ~= ammo and #w.ammo < 400 then
          w.ammo[#w.ammo + 1] = { t = t, from = w.last_ammo, to = ammo }
        end
        w.last_life, w.last_ammo = life, ammo
      end
    end
  end
end
sample()

-- For each change after the first read, the nearest event of `names` that
-- names the unit `as`: how many changes have one within a second before or
-- 0.3 s after, and the changes that have none.
local function judge(changes, events, names, as)
  local matched, unmatched = 0, {}
  for k = 2, #changes do
    local c = changes[k]
    local best
    for _, e in ipairs(events) do
      if e.as == as and names[e.event] then
        local d = c.t - e.t
        if d >= -0.3 and d <= 1.0 and (not best or math.abs(d) < math.abs(best)) then
          best = d
        end
      end
    end
    if best then
      matched = matched + 1
    elseif #unmatched < 20 then
      unmatched[#unmatched + 1] = c
    end
  end
  return { changes = math.max(#changes - 1, 0), matched = matched, unmatched = unmatched }
end

-- The time of the last change, and of the last event of `names` naming the
-- unit `as`: an event at or after the change means a read on that event
-- holds the final value.
local function last(changes, events, names, as)
  local change = #changes > 1 and changes[#changes].t or nil
  local event
  for _, e in ipairs(events) do
    if e.as == as and names[e.event] then
      event = e.t
    end
  end
  return { last_change = change, last_event = event }
end

local started, next_sample, exploded, ended = timer.getTime(), 0, false, nil
return E.wait(function()
  local t = timer.getTime()
  if t >= next_sample then
    sample()
    next_sample = t + 0.2
  end
  if not exploded and t - started >= 20 then
    exploded = true
    local n1 = by_name['np-design-n1']
    if n1 and n1.alive then
      local p = n1.unit:getPoint()
      -- Beside the row's first vehicle, so the others take splash only.
      trigger.action.explosion({ x = p.x, y = p.y, z = p.z - 6 }, 30)
    end
  end
  local b, r = Group.getByName('np-design-blue'), Group.getByName('np-design-red')
  local over = not b or not r or b:getSize() == 0 or r:getSize() == 0
  if over and not ended then
    ended = t
  end
  if not ((ended and t - ended >= 10 and t - started >= 40) or t - started >= 150) then
    return nil
  end
  sample()
  world.removeEventHandler(handler)
  local LIFE = { S_EVENT_HIT = true }
  local LIFE_ANY = { S_EVENT_HIT = true, S_EVENT_KILL = true, S_EVENT_BDA = true, S_EVENT_DAMAGE = true }
  local SHOTS = { S_EVENT_SHOT = true, S_EVENT_SHOOTING_START = true, S_EVENT_SHOOTING_END = true }
  local ENDS = { S_EVENT_SHOT = true, S_EVENT_SHOOTING_END = true }
  local units = {}
  for _, name in ipairs(NAMES) do
    local w = by_name[name]
    if w then
      local counts = {}
      for _, e in ipairs(w.events) do
        local key = e.event .. ':' .. e.as
        counts[key] = (counts[key] or 0) + 1
      end
      units[name] = {
        alive = w.alive,
        gone_after_s = w.gone_at and w.gone_at - started or nil,
        life_first = w.life[1] and w.life[1].to,
        life_last = w.last_life,
        ammo_first = w.ammo[1] and w.ammo[1].to,
        ammo_last = w.last_ammo,
        events = counts,
        life_by_hit = judge(w.life, w.events, LIFE, 'target'),
        life_by_any = judge(w.life, w.events, LIFE_ANY, 'target'),
        ammo_by_shot = judge(w.ammo, w.events, SHOTS, 'initiator'),
        life_final = last(w.life, w.events, LIFE, 'target'),
        ammo_final = last(w.ammo, w.events, ENDS, 'initiator'),
      }
    end
  end
  local fuels = {}
  for key, f in pairs(fuel) do
    local ok, exists = pcall(f.unit.isExist, f.unit)
    fuels[key] = {
      type = f.type,
      first = f.first,
      last = ok and exists and f.unit:getFuel() or nil,
      seconds = t - f.at,
    }
  end
  for _, name in ipairs({ 'np-design-blue', 'np-design-red', 'np-design-near' }) do
    S.destroy(name)
  end
  return { seconds = t - started, open_fire = fire, events = seen, units = units, fuel = fuels }
end, 220, 'running')
