-- E39: what a `coalition.addGroup` spawn can set per unit, and what can be
-- set right after it, for life, fuel, per-pylon weapons, gun rounds and
-- countermeasures, on aircraft (air start), a helicopter, ground units and
-- ships.
--
-- Rule, written before the run. A mechanism restores a value when the read
-- after the spawn, 1 s and 5 s of model time later, equals the target:
--   life: getLife / getLife0 within 0.5 percentage points of the percent
--         asked, with the unit existing and not dead;
--   fuel: getFuel within 0.005 of the fraction asked (payload.fuel in kg is
--         the fraction times getDesc().fuelMassMax);
--   gun:  the gun row within 1 round of the full count times the percent;
--   pylons: every getAmmo row's count equal to the sum of the counts of the
--         pylons whose launcher's weapon (get_weapon_and_count_from_launcher,
--         get_weapon_display_name_by_wstype) has that row's displayName;
--   ground ammunition: every row exact from a table built only from data
--         DCS's Lua can read; otherwise it needs a per-type map (E14).
-- Flares and chaff are set but have no read (E8), so they cannot be
-- captured and get no verdict. A value that reads back unchanged is
-- ignored; a call that raises errors. The experiment fails its rule only
-- when it cannot record a verdict for a subject, or DCS stops.
--
-- Life is set with a_unit_set_life_percentage, in the `mission` state only
-- (ED-CODE: MissionEditor/modules/me_trigrules.lua, as in E16). It is set
-- three ways: in the same frame as the addGroup, with the simulation
-- paused; a second later, still paused; and running.
-- states: hook
-- timeout: 240

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    return 'CROSS FAILED ' .. tostring(value)
  end
  return value
end

local PRE = "local X = E.lib('s15')\nlocal S = X.S\nlocal o = S.origin()\n"

-- CLSIDs of the F-15C's launchers (LIVE: recon.lua, db.Units.Planes).
local AIM9M = '{6CEB49FC-DED8-4DED-B053-E1F033FF72D3}'
local AIM7M = '{8D399DDA-FF81-4F14-904D-099B34FE7918}'
local AIM120B = '{C8E06185-7CD6-4C90-959F-044679E90751}'
local TANK = '{E1F29B21-F291-4589-9FD8-3272EEC69506}'

-- Subjects: name, how it is added, the life percent to set and when.
-- `life_at`: 'frame' sets it in the addGroup's frame (paused), 'paused' a
-- second later while still paused, 'running' after the resume.
local SPAWN = PRE .. [[
local AIM9M, AIM7M, AIM120B, TANK = '%s', '%s', '%s', '%s'
local cases = {}
E.store.e39 = { cases = cases }
local function air(name, opts, extra)
  opts.name = name
  local group, err = X.add_air(opts)
  extra.name, extra.label, extra.added, extra.error = name .. '-1', name, group ~= nil, err
  extra.pylons = opts.pylons
  cases[#cases + 1] = extra
end
local function ground(name, kind, at, ammo, category, extra)
  local group, err = X.add_ground(name, kind, { { name = name .. '-1', x = at.x, z = at.z, ammo = ammo } },
    { category = category })
  extra.name, extra.label, extra.added, extra.error, extra.kind = name .. '-1', name, group ~= nil, err, kind
  cases[#cases + 1] = extra
end
local full = {
  [1] = { CLSID = AIM120B }, [3] = { CLSID = AIM9M }, [4] = { CLSID = AIM7M }, [5] = { CLSID = AIM7M },
  [6] = { CLSID = TANK }, [7] = { CLSID = AIM7M }, [8] = { CLSID = AIM7M }, [9] = { CLSID = AIM9M },
  [11] = { CLSID = AIM120B },
}
local reduced = {
  [1] = { CLSID = AIM120B }, [3] = { CLSID = AIM9M }, [7] = { CLSID = AIM7M }, [8] = { CLSID = AIM7M },
}
local fmax = 6103
air('np-e39-full', { x = o.x, z = o.z + 20000, alt = 6000, fuel = 0.5 * fmax, gun = 100, flare = 60, chaff = 120,
  pylons = full }, { kind = 'F-15C', fuel = 0.5, gun = 100, life = 60, life_at = 'frame' })
air('np-e39-reduced', { x = o.x - 4000, z = o.z + 20000, alt = 6000, fuel = 0.37 * fmax, gun = 50, flare = 30,
  chaff = 60, pylons = reduced }, { kind = 'F-15C', fuel = 0.37, gun = 50, life = 60, life_at = 'paused' })
air('np-e39-gun25', { x = o.x - 8000, z = o.z + 20000, alt = 6000, fuel = 0.8 * fmax, gun = 25 },
  { kind = 'F-15C', fuel = 0.8, gun = 25, life = 30, life_at = 'running' })
air('np-e39-gun0', { x = o.x - 12000, z = o.z + 20000, alt = 6000, fuel = 0.2 * fmax, gun = 0 },
  { kind = 'F-15C', fuel = 0.2, gun = 0 })
air('np-e39-heli', { kind = 'UH-1H', category = Group.Category.HELICOPTER, x = o.x - 5000, z = o.z + 24000, alt = 400,
  speed = 40, fuel = 0.5 * 631, gun = 100, task = 'Transport' }, { kind = 'UH-1H', fuel = 0.5, life = 50, life_at = 'frame' })
local g = { x = o.x + 3000, z = o.z + 12000 }
ground('np-e39-t72', 'T-72B', g, nil, nil, { life = 40, life_at = 'frame' })
ground('np-e39-t72ammo', 'T-72B', { x = g.x + 60, z = g.z }, { WS = { [1] = { LN = { [2] = { PL = {
  [1] = { ammo_capacity = 3 }, [2] = { ammo_capacity = 4 } } } } } } }, nil, { ammo_asked = { ['2A46M_125_HE'] = 7 } })
ground('np-e39-t72zero', 'T-72B', { x = g.x + 120, z = g.z }, { WS = { [1] = { LN = { [2] = { PL = {
  [1] = { ammo_capacity = 0 }, [2] = { ammo_capacity = 0 } } } } } } }, nil, { ammo_asked = { ['2A46M_125_HE'] = 0 } })
ground('np-e39-m113', 'M-113', { x = g.x + 180, z = g.z }, nil, nil, { life = 40, life_at = 'paused' })
ground('np-e39-abrams', 'M-1 Abrams', { x = g.x + 240, z = g.z }, nil, nil, { life = 70, life_at = 'running' })
local water = S.water()
if water then
  ground('np-e39-perry', 'PERRY', water, nil, Group.Category.SHIP, { life = 80, life_at = 'frame', ship = true })
  ground('np-e39-perry50', 'PERRY', { x = water.x + 1500, z = water.z }, nil, Group.Category.SHIP,
    { life = 50, life_at = 'running', ship = true })
  ground('np-e39-boat', 'speedboat', { x = water.x + 3000, z = water.z }, nil, Group.Category.SHIP,
    { life = 80, life_at = 'paused', ship = true })
end
local out = {}
for _, c in ipairs(cases) do
  out[#out + 1] = { label = c.label, added = c.added, error = c.error }
end
return X.enc(out)
]]
SPAWN = SPAWN:gsub("'%%s', '%%s', '%%s', '%%s'", string.format("'%s', '%s', '%s', '%s'", AIM9M, AIM7M, AIM120B, TANK), 1)

-- The mission ID of each case, as `label=id` pairs.
local IDS = PRE .. [[
local out = {}
for _, c in ipairs(E.store.e39.cases) do
  local unit = Unit.getByName(c.name)
  out[#out + 1] = c.label .. '=' .. tostring(unit and unit:getID())
end
return table.concat(out, ' ')
]]

-- What each launcher of the two loaded aircraft carries, by pylon.
local LAYOUTS = PRE .. [[
local out = {}
for _, c in ipairs(E.store.e39.cases) do
  if c.pylons then
    local p = {}
    for n, pylon in pairs(c.pylons) do
      local ok, weapon, count = pcall(get_weapon_and_count_from_launcher, pylon.CLSID)
      local dok, display = pcall(get_weapon_display_name_by_wstype, weapon)
      p[tostring(n)] = { clsid = pylon.CLSID, ok = ok, count = count, display = dok and display or nil }
    end
    out[c.label] = p
  end
end
E.store.e39.layouts = out
return X.enc(out)
]]

-- Every case's reads now, with the rule's verdict where the case has a target.
local READ = PRE .. [[
local out = {}
local layouts = E.store.e39.layouts or {}
for _, c in ipairs(E.store.e39.cases) do
  local r = {}
  local unit = Unit.getByName(c.name)
  r.found = unit ~= nil
  if unit then
    r.exists = X.try(unit.isExist, unit)
    r.dead = X.try(unit.isDead, unit)
    r.life = X.try(unit.getLife, unit)
    r.life0 = X.try(unit.getLife0, unit)
    r.fuel = X.try(unit.getFuel, unit)
    local rows = X.try(unit.getAmmo, unit)
    r.ammo = {}
    for i, row in ipairs(type(rows) == 'table' and rows or {}) do
      local desc = type(row.desc) == 'table' and row.desc or {}
      r.ammo[i] = { type = desc.typeName, display = desc.displayName, count = row.count }
    end
    if c.life and r.life and r.life0 and r.life0 > 0 then
      r.life_percent = 100 * r.life / r.life0
      r.life_ok = math.abs(r.life_percent - c.life) <= 0.5
    end
    if c.fuel and r.fuel then
      r.fuel_ok = math.abs(r.fuel - c.fuel) <= 0.005
    end
    if c.gun then
      for _, row in ipairs(r.ammo) do
        if row.type and row.type:find('M61_20', 1, true) then
          r.gun_rounds = row.count
          r.gun_ok = math.abs(row.count - 940 * c.gun / 100) <= 1
        end
      end
    end
    local lay = layouts[c.label]
    if lay then
      local want = {}
      for _, p in pairs(lay) do
        if p.display then
          want[p.display] = (want[p.display] or 0) + (p.count or 0)
        end
      end
      local match = {}
      for _, row in ipairs(r.ammo) do
        if row.display and want[row.display] then
          match[row.display] = { want = want[row.display], read = row.count, ok = want[row.display] == row.count }
          want[row.display] = nil
        end
      end
      for display, n in pairs(want) do
        match[display] = { want = n, read = 'no row' }
      end
      r.pylons = match
    end
    if c.ammo_asked then
      r.ammo_asked = c.ammo_asked
    end
  end
  out[c.label] = r
end
return X.enc(out)
]]

local ids = {}
local cases_life = {}

-- Set the life of every case whose `life_at` is `when`.
local function set_life(when)
  local report = {}
  for label, spec in pairs(cases_life) do
    if spec.at == when then
      local id = tonumber(ids[label])
      if id then
        report[label] = cross('mission', string.format(
          'local ok, err = pcall(a_unit_set_life_percentage, %.0f, %d) return ok and "ok" or tostring(err)', id, spec.life))
      else
        report[label] = 'no ID (' .. tostring(ids[label]) .. ')'
      end
    end
  end
  return report
end

local function read(name)
  E.observe(name, cross('server', READ), { info = true })
end

-- Which cases set life, and when: read once from the server state.
local LIFE = PRE .. [[
local out = {}
for _, c in ipairs(E.store.e39.cases) do
  if c.life then out[#out + 1] = c.label .. '=' .. c.life .. '@' .. c.life_at end
end
return table.concat(out, ' ')
]]

local was_paused = DCS.getPause()
E.observe('start', { paused = was_paused, model = DCS.getModelTime() }, { info = true })
DCS.setPause(true)

-- In one frame, paused: spawn, read the IDs, set the `frame` lives, read.
E.observe('spawned', cross('server', SPAWN), { info = true })
for label, id in cross('server', IDS):gmatch('(%S+)=(%S+)') do
  ids[label] = id
end
for label, life, at in cross('server', LIFE):gmatch('(%S+)=(%d+)@(%a+)') do
  cases_life[label] = { life = tonumber(life), at = at }
end
E.observe('ids', ids, { info = true })
E.observe('layouts', cross('server', LAYOUTS), { info = true })
E.observe('same_frame_life', set_life('frame'), { info = true })
read('read_same_frame')

local real0 = DCS.getRealTime()
local mark
local stage = 0
local STAGES = {
  -- Still paused, a second of real time later.
  function()
    if DCS.getRealTime() - real0 < 1 then return false end
    read('read_paused_1_s')
    E.observe('paused_life', set_life('paused'), { info = true })
    return true
  end,
  function()
    if DCS.getRealTime() - real0 < 2 then return false end
    read('read_paused_2_s')
    DCS.setPause(false)
    mark = DCS.getModelTime()
    return true
  end,
  function()
    if DCS.getModelTime() - mark < 1 then return false end
    read('read_running_1_s')
    E.observe('running_life', set_life('running'), { info = true })
    mark = DCS.getModelTime()
    return true
  end,
  function()
    if DCS.getModelTime() - mark < 1 then return false end
    read('read_life_1_s')
    return true
  end,
  function()
    if DCS.getModelTime() - mark < 5 then return false end
    read('read_life_5_s')
    return true
  end,
  function()
    E.observe('cleared', cross('server', "local E = ...\nreturn tostring(E.lib('scenario').clear())\n"), { info = true })
    DCS.setPause(was_paused)
    return true
  end,
}

return E.wait(function()
  local step = STAGES[stage + 1]
  if not step then
    return 'done'
  end
  if stage >= 2 and stage < #STAGES - 1 and DCS.getPause() then
    DCS.setPause(false)
  end
  if step() then
    stage = stage + 1
  end
  return nil
end, 200)
