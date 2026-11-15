-- E21c: when a script spawns a FARP static with a chosen `name`, does DCS use
-- that name as the airbase's name, and what else shows up as its name?
-- Spawns a FARP named 'Bravo Test', one named 'np!k3F2A', and a second one named
-- 'Bravo Test'; after each spawn reads Airbase.getByName, getName, getCallsign,
-- getID, the world.getAirbases() entries near the FARP, and
-- StaticObject.getByName. The FARPs stay in the mission, so a screenshot can be
-- taken afterwards; the mission is thrown away by the runner.
-- Run with `cargo xtask eval <target> server`.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')
local o = S.origin()
local usa = country.id.USA

local function show(v)
  if v == nil then
    return 'nil'
  end
  return tostring(v)
end

local function try(f, ...)
  local ok, a = pcall(f, ...)
  if ok then
    return show(a)
  end
  return 'ERR ' .. tostring(a)
end

local function method(obj, name)
  if obj == nil then
    return 'nil-object'
  end
  local f = obj[name]
  if type(f) ~= 'function' then
    return 'no-method'
  end
  return try(f, obj)
end

local function spawn(name, north, east, callsign_id)
  local ok, res = pcall(coalition.addStaticObject, usa, {
    name = name,
    type = 'FARP',
    category = 'Heliports',
    shape_name = 'FARPS',
    x = o.x + north,
    y = o.z + east,
    heading = 0,
    heliport_callsign_id = callsign_id,
    heliport_frequency = 127.5,
    heliport_modulation = 0,
    heliport_id = nil,
  })
  return ok, res
end

-- Every airbase within 300 m of a point, as "name|callsign|id|category".
local function near(north, east)
  local px, pz = o.x + north, o.z + east
  local found = {}
  for _, ab in ipairs(world.getAirbases()) do
    local ok, p = pcall(ab.getPoint, ab)
    if ok and p then
      local dx, dz = p.x - px, p.z - pz
      if dx * dx + dz * dz < 300 * 300 then
        found[#found + 1] = method(ab, 'getName') .. '|' .. method(ab, 'getCallsign') .. '|' .. method(ab, 'getID')
          .. '|' .. method(ab, 'getCategory')
      end
    end
  end
  return table.concat(found, ' ; ')
end

local function views(tag, name, north, east)
  local ab = Airbase.getByName(name)
  local st = StaticObject.getByName(name)
  E.observe(tag .. '.Airbase.getByName', ab and 'found' or 'nil', { info = true })
  E.observe(tag .. '.airbase.getName', method(ab, 'getName'), { info = true })
  E.observe(tag .. '.airbase.getCallsign', method(ab, 'getCallsign'), { info = true })
  E.observe(tag .. '.airbase.getID', method(ab, 'getID'), { info = true })
  E.observe(tag .. '.airbase.isExist', method(ab, 'isExist'), { info = true })
  E.observe(tag .. '.airbase.getCategory', method(ab, 'getCategory'), { info = true })
  E.observe(tag .. '.airbase.getDesc', try(function()
    local d = ab:getDesc()
    local keys = {}
    for k, v in pairs(d) do
      keys[#keys + 1] = tostring(k) .. '=' .. tostring(v)
    end
    table.sort(keys)
    return table.concat(keys, ',')
  end), { info = true })
  E.observe(tag .. '.world.getAirbases.near', near(north, east), { info = true })
  E.observe(tag .. '.StaticObject.getByName', st and 'found' or 'nil', { info = true })
  E.observe(tag .. '.static.getName', method(st, 'getName'), { info = true })
  E.observe(tag .. '.static.isExist', method(st, 'isExist'), { info = true })
  E.observe(tag .. '.static.getTypeName', method(st, 'getTypeName'), { info = true })
  E.observe(tag .. '.static.getCategory', method(st, 'getCategory'), { info = true })
  E.observe(tag .. '.static.getID', method(st, 'getID'), { info = true })
  return ab, st
end

-- Parts 1 to 3 run once per mission: a rerun in the same mission starts at part 4.
if Airbase.getByName('np!k3F2A') == nil then
E.observe('0.airbases_before', #world.getAirbases(), { info = true })

-- 1. A name with spaces.
local ok, res = spawn('Bravo Test', 400, 0, 1)
E.observe('1.spawn', tostring(ok) .. ' ' .. show(res and (ok and method(res, 'getName') or res)), { info = true })
local ab1, st1 = views('1', 'Bravo Test', 400, 0)

-- 2. A platform-style name.
ok, res = spawn('np!k3F2A', 400, 300, 2)
E.observe('2.spawn', tostring(ok) .. ' ' .. show(res and (ok and method(res, 'getName') or res)), { info = true })
local ab2, st2 = views('2', 'np!k3F2A', 400, 300)

E.observe('2.airbases_after', #world.getAirbases(), { info = true })

-- 3. A second FARP with the first one's name.
ok, res = spawn('Bravo Test', 400, 600, 3)
E.observe('3.spawn', tostring(ok) .. ' ' .. show(res and (ok and method(res, 'getName') or res)), { info = true })
E.observe('3.first.static.isExist', method(st1, 'isExist'), { info = true })
E.observe('3.first.airbase.isExist', method(ab1, 'isExist'), { info = true })
E.observe('3.first.airbase.getName', method(ab1, 'getName'), { info = true })
E.observe('3.first.static.getName', method(st1, 'getName'), { info = true })
E.observe('3.second_np.airbase.isExist', method(ab2, 'isExist'), { info = true })
local ab3 = Airbase.getByName('Bravo Test')
local st3 = StaticObject.getByName('Bravo Test')
local function where(obj)
  if obj == nil then
    return 'nil'
  end
  return try(function()
    local p = obj:getPoint()
    return string.format('north %.0f east %.0f', p.x - o.x, p.z - o.z)
  end)
end
E.observe('3.Airbase.getByName.where', where(ab3), { info = true })
E.observe('3.StaticObject.getByName.where', where(st3), { info = true })
E.observe('3.Airbase.getByName.same_object_as_first', tostring(ab3 ~= nil and ab1 ~= nil and ab3 == ab1), { info = true })
E.observe('3.near_first_site', near(400, 0), { info = true })
E.observe('3.near_dup_site', near(400, 600), { info = true })
E.observe('3.airbases_after', #world.getAirbases(), { info = true })
local names = {}
for _, ab in ipairs(world.getAirbases()) do
  local n = method(ab, 'getName')
  if n:find('Bravo', 1, true) or n:find('np!', 1, true) then
    names[#names + 1] = n .. '|' .. method(ab, 'getID')
  end
end
E.observe('3.world.getAirbases.matching', table.concat(names, ' ; '), { info = true })
end

-- 4. The idea naming scheme (base 62, campaign tag 'k3F') and today's spec form,
-- each read back by name and checked byte for byte.
local function ll(north, east)
  local metres = string.format('x %.0f z %.0f', o.x + north, o.z + east)
  local ok, lat, lon = pcall(function()
    return coordinate.LOtoLL({ x = o.x + north, y = 0, z = o.z + east })
  end)
  if ok and type(lat) == 'number' then
    return string.format('lat %.5f lon %.5f ; %s', lat, lon, metres)
  end
  return metres
end

local function ground(group, unit_names, north, east)
  local units = {}
  for i, name in ipairs(unit_names) do
    units[i] = { name = name, x = o.x + north, z = o.z + east + (i - 1) * 25 }
  end
  return pcall(S.ground, group, 'M-113', units)
end

local function check(tag, kind, name)
  local got
  if kind == 'group' then
    got = Group.getByName(name)
  elseif kind == 'unit' then
    got = Unit.getByName(name)
  elseif kind == 'static' then
    got = StaticObject.getByName(name)
  else
    got = Airbase.getByName(name)
  end
  local n = method(got, 'getName')
  E.observe(tag .. '.' .. kind .. '.' .. name, (got and 'found' or 'nil') .. ' getName=' .. n
    .. ' identical=' .. tostring(n == name) .. ' exist=' .. method(got, 'isExist'), { info = true })
end

if Unit.getByName('np.k3F19') == nil then
local g_ok, g_err = ground('np#k3F7', { 'np.k3F19', 'np.k3F1A' }, 700, 0)
E.observe('4.spawn.np#k3F7', tostring(g_ok) .. ' ' .. (g_ok and '' or tostring(g_err)), { info = true })
local ok, res = spawn('np!k3F2B', 700, 300, 4)
E.observe('4.spawn.np!k3F2B', tostring(ok) .. ' ' .. (ok and method(res, 'getName') or tostring(res)), { info = true })
g_ok, g_err = ground('np.g.m3k9x2a1.7', { 'np.u.m3k9x2a1.1z' }, 700, 600)
E.observe('4.spawn.np.g.m3k9x2a1.7', tostring(g_ok) .. ' ' .. (g_ok and '' or tostring(g_err)), { info = true })

check('4', 'group', 'np#k3F7')
check('4', 'unit', 'np.k3F19')
check('4', 'unit', 'np.k3F1A')
check('4', 'static', 'np!k3F2B')
check('4', 'airbase', 'np!k3F2B')
check('4', 'static', 'np!k3F2A')
check('4', 'group', 'np.g.m3k9x2a1.7')
check('4', 'unit', 'np.u.m3k9x2a1.1z')
E.observe('4.shared_namespace.survivors', table.concat({
  tostring(Unit.getByName('np.k3F19') ~= nil),
  tostring(Unit.getByName('np.k3F1A') ~= nil),
  tostring(StaticObject.getByName('np!k3F2B') ~= nil),
  tostring(StaticObject.getByName('np!k3F2A') ~= nil),
  tostring(Group.getByName('np#k3F7') ~= nil),
}, ' '), { info = true })

-- Map positions, for finding each object on the F10 map.
E.observe('5.position.Bravo Test (first, gone)', ll(400, 0), { info = true })
E.observe('5.position.np!k3F2A', ll(400, 300), { info = true })
E.observe('5.position.Bravo Test (second)', ll(400, 600), { info = true })
E.observe('5.position.np#k3F7 / np.k3F19 / np.k3F1A', ll(700, 0), { info = true })
E.observe('5.position.np!k3F2B', ll(700, 300), { info = true })
E.observe('5.position.np.g.m3k9x2a1.7 / np.u.m3k9x2a1.1z', ll(700, 600), { info = true })
end

-- 6. Case: names that differ only by case, in the same mission. Runs once.
if Unit.getByName('np.K3F1C') == nil then
  local function pair(tag, kind_a, name_a, kind_b, name_b)
    local function get(kind, name)
      if kind == 'group' then
        return Group.getByName(name)
      elseif kind == 'unit' then
        return Unit.getByName(name)
      end
      return StaticObject.getByName(name)
    end
    local a, b = get(kind_a, name_a), get(kind_b, name_b)
    E.observe(tag, string.format('%s: exist=%s id=%s getName=%s | %s: exist=%s id=%s getName=%s | same_object=%s',
      name_a, method(a, 'isExist'), method(a, 'getID'), method(a, 'getName'),
      name_b, method(b, 'isExist'), method(b, 'getID'), method(b, 'getName'),
      tostring(a ~= nil and b ~= nil and a == b)), { info = true })
  end
  ground('np#K3F7', { 'np.K3F1C' }, 1000, 0)
  ground('np#q1', { 'np.k3f19' }, 1000, 300)
  local ok6, res6 = spawn('np!K3F2B', 1000, 600, 5)
  E.observe('6.spawn.np!K3F2B', tostring(ok6) .. ' ' .. (ok6 and method(res6, 'getName') or tostring(res6)), { info = true })
  ground('np#x9', { 'np.x9Z1' }, 1300, 0)
  ok6, res6 = spawn('np!X9z1', 1300, 300, 6)
  E.observe('6.spawn.np!X9z1', tostring(ok6) .. ' ' .. (ok6 and method(res6, 'getName') or tostring(res6)), { info = true })
  pair('6.unit_case.np.k3F19/np.k3f19', 'unit', 'np.k3F19', 'unit', 'np.k3f19')
  pair('6.static_case.np!k3F2B/np!K3F2B', 'static', 'np!k3F2B', 'static', 'np!K3F2B')
  pair('6.group_case.np#k3F7/np#K3F7', 'group', 'np#k3F7', 'group', 'np#K3F7')
  E.observe('6.group_sizes', method(Group.getByName('np#k3F7'), 'getSize') .. ' ' .. method(Group.getByName('np#K3F7'), 'getSize'),
    { info = true })
  pair('6.unit_vs_static.np.x9Z1/np!X9z1', 'unit', 'np.x9Z1', 'static', 'np!X9z1')
  E.observe('6.airbases.np!k3F2B,np!K3F2B', method(Airbase.getByName('np!k3F2B'), 'getName') .. ' '
    .. method(Airbase.getByName('np!K3F2B'), 'getName') .. ' ids ' .. method(Airbase.getByName('np!k3F2B'), 'getID')
    .. ' ' .. method(Airbase.getByName('np!K3F2B'), 'getID'), { info = true })
  E.observe('6.position.np#K3F7 / np.K3F1C', ll(1000, 0), { info = true })
  E.observe('6.position.np#q1 / np.k3f19', ll(1000, 300), { info = true })
  E.observe('6.position.np!K3F2B', ll(1000, 600), { info = true })
  E.observe('6.position.np#x9 / np.x9Z1', ll(1300, 0), { info = true })
  E.observe('6.position.np!X9z1', ll(1300, 300), { info = true })
end
return 'done'
