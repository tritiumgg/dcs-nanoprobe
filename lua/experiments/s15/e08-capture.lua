-- E8 (S15's part): what can be captured from an aircraft, a helicopter and a
-- ship, destroyed, added again and compared? Rule: each captured, restored
-- and compared.
--
-- Three subjects: an airborne F-15C added with pylon stores, 60 flares, 60
-- chaff, 3,000 kg of fuel and half its gun rounds; a UH-1H in the air with
-- flares and chaff; and a speedboat when the map has water within 60 km. The
-- CLSIDs of the stores are ED's for the AIM-9M and the AIM-120C; which
-- pylons of the F-15C take them is UNVERIFIED, and the rows getAmmo lists
-- afterward show what stuck.
--
-- Each is damaged to 70 % through the mission state's
-- a_unit_set_life_percentage, so life is a value worth restoring. Then the
-- snippet reads every key a capture keeps (name, ID, type, x, y, z, heading,
-- pitch, roll, life, life0, fuel, and the rows of getAmmo, with the speed)
-- and records what reads exist for flares, chaff and pylons: every function
-- on Unit, Object, Group, Controller and Airbase whose name holds one of the
-- words flare, chaff, pylon, payload, loadout, countermeasure, dispenser or
-- store, the keys of getDesc(), and whether getAmmo lists a flare or a chaff
-- row at all (LIVE, E38: it lists the gun only).
--
-- It destroys each group, waits for the name to be free, adds the group again
-- from the captured values (position, altitude, heading, speed, fuel in
-- kg, the gun's share of its rounds) and, for what has no read, from the
-- spawn record (pylons, flares, chaff) as the platform's own record would
-- hold them. It sets life back to the captured share, waits, reads the same
-- keys, and compares field by field: the first read, the second, their
-- difference, the allowed difference and whether it is within it. A moving
-- object has flown on between the add and the second read, so a position's
-- allowed difference is the distance its speed covers in that time, plus 25 m
-- and a third of that distance.
-- states: hook
-- timeout: 180

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    return 'CROSS FAILED ' .. tostring(value)
  end
  return value
end

local PRE = "local X = E.lib('s15')\nlocal S = X.S\nlocal o = S.origin()\n"

-- The F-15C's gun holds 940 rounds (LIVE, lua/probes/51-aircraft.lua).
local GUN_ROUNDS = 940
local DAMAGE_PERCENT = 70

-- Add the subjects and keep their spawn records. Returns text.
local SPAWN = PRE .. [[
local out = {}
local store = { specs = {}, caps = {}, labels = {} }
E.store.e08 = store
local AIM9M, AIM120C = '{6CEB49FC-DED8-4DED-B053-E1F033FF72D3}', '{40EF17B7-F508-45de-8566-6FFECC0C1AB8}'
local function air(label, opts)
  opts.name = label
  local group, err = X.add_air(opts)
  if group then
    store.specs[label] = opts
    store.labels[#store.labels + 1] = label
    out[#out + 1] = label .. '=added'
  else
    out[#out + 1] = label .. '=failed:' .. tostring(err)
  end
end
air('np-e08-air', {
  x = o.x, z = o.z + 20000, alt = 5000, speed = 200, heading = 0, fuel = 3000, gun = 50, flare = 60, chaff = 60,
  pylons = {
    [1] = { CLSID = AIM9M }, [2] = { CLSID = AIM120C }, [3] = { CLSID = AIM120C },
    [9] = { CLSID = AIM120C }, [10] = { CLSID = AIM120C }, [11] = { CLSID = AIM9M },
  },
})
air('np-e08-heli', {
  kind = 'UH-1H', category = Group.Category.HELICOPTER, x = o.x - 6000, z = o.z + 20000, alt = 400, speed = 40,
  fuel = 300, gun = 100, flare = 30, chaff = 30, task = 'Transport',
})
local water = S.water()
if water then
  local group, err = X.add_ground('np-e08-ship', 'speedboat', { { name = 'np-e08-ship-1', x = water.x, z = water.z, heading = 0 } },
    { category = Group.Category.SHIP, route = { { x = water.x, z = water.z, speed = 0 } } })
  if group then
    store.specs['np-e08-ship'] = { ship = true, x = water.x, z = water.z }
    store.labels[#store.labels + 1] = 'np-e08-ship'
    out[#out + 1] = 'np-e08-ship=added'
  else
    out[#out + 1] = 'np-e08-ship=failed:' .. tostring(err)
  end
else
  out[#out + 1] = 'np-e08-ship=skipped:no water within 60 km'
end
return table.concat(out, ' ')
]]

local IDS = PRE .. [[
local out = {}
for _, label in ipairs(E.store.e08.labels) do
  local unit = Unit.getByName(label .. '-1')
  out[#out + 1] = label .. '=' .. tostring(unit and unit:getID())
end
return table.concat(out, ' ')
]]

-- Read every key a capture keeps, keep the read as the `stage`'s, and return
-- it as text. `stage` is replaced below.
local CAPTURE = PRE .. [[
local store = E.store.e08
local out = {}
for _, label in ipairs(store.labels) do
  local unit = Unit.getByName(label .. '-1')
  if unit then
    local c = X.capture(unit)
    c.at = timer.getTime()
    store.caps[label] = store.caps[label] or {}
    store.caps[label]['@STAGE@'] = c
    out[label] = c
  else
    out[label] = 'no unit'
  end
end
return X.enc(out)
]]

-- What can be read about flares, chaff and pylons.
local READS = PRE .. [[
local store = E.store.e08
local out = { methods = X.candidate_methods() }
for _, label in ipairs(store.labels) do
  local unit = Unit.getByName(label .. '-1')
  if unit then
    local rows = X.ammo_rows(unit) or {}
    local flare_or_chaff = 0
    for _, row in ipairs(rows) do
      local name = tostring(row.type) .. ' ' .. tostring(row.display)
      if name:lower():find('flare') or name:lower():find('chaff') or name:lower():find('cm_') then
        flare_or_chaff = flare_or_chaff + 1
      end
    end
    out[label] = { ammo_rows = #rows, flare_or_chaff_rows = flare_or_chaff, desc_keys = X.desc_keys(unit) }
  end
end
return X.enc(out)
]]

-- Destroy every group and report which still exist.
local DESTROY = PRE .. [[
for _, label in ipairs(E.store.e08.labels) do S.destroy(label) end
return 'destroyed'
]]

local GONE = PRE .. [[
local out = {}
for _, label in ipairs(E.store.e08.labels) do
  out[#out + 1] = label .. '=' .. tostring(Group.getByName(label) ~= nil)
end
return table.concat(out, ' ')
]]

-- Add every group again from the first capture and the spawn records.
local RESPAWN = PRE .. [[
local store = E.store.e08
local out = {}
store.spawned_at = timer.getTime()
for _, label in ipairs(store.labels) do
  local c, spec = store.caps[label].first, store.specs[label]
  local group, err
  if spec.ship then
    group, err = X.add_ground(label, 'speedboat', { { name = label .. '-1', x = c.x, z = c.z, heading = c.heading } },
      { category = Group.Category.SHIP, route = { { x = c.x, z = c.z, speed = 0 } } })
  else
    local gun = spec.gun
    if spec.kind == nil and c.ammo then
      for _, row in ipairs(c.ammo) do
        if tostring(row.type):find('M61', 1, true) then gun = row.count / @GUN_ROUNDS@ * 100 end
      end
    end
    local opts = {
      name = label, kind = spec.kind, category = spec.category, task = spec.task,
      x = c.x, z = c.z, alt = c.y, heading = c.heading, speed = c.speed,
      fuel = c.fuel_kg, gun = gun, flare = spec.flare, chaff = spec.chaff, pylons = spec.pylons,
    }
    group, err = X.add_air(opts)
  end
  out[#out + 1] = label .. '=' .. (group and 'added' or ('failed:' .. tostring(err)))
end
return table.concat(out, ' ')
]]

-- The captured life over the captured life0, as a percentage, per subject.
local LIFE = PRE .. [[
local out = {}
for _, label in ipairs(E.store.e08.labels) do
  local c = E.store.e08.caps[label].first
  out[#out + 1] = label .. '=' .. string.format('%.4f', (c.life or 0) / (c.life0 or 1) * 100)
end
return table.concat(out, ' ')
]]

-- The comparison, field by field.
local COMPARE = PRE .. [[
local store = E.store.e08
local out = {}
local function number(a, b, tol)
  if a == nil or b == nil then return { first = a, second = b, ok = false, note = 'missing' } end
  return { first = a, second = b, delta = b - a, tol = tol, ok = math.abs(b - a) <= tol }
end
for _, label in ipairs(store.labels) do
  local a, b = store.caps[label].first, store.caps[label].second
  if not (a and b) then
    out[label] = 'no second read'
  else
    local f = {}
    local fly = (b.at - store.spawned_at)
    local expected = (a.speed or 0) * fly
    local moved = math.sqrt((b.x - a.x) ^ 2 + (b.z - a.z) ^ 2)
    f.name = { first = a.name, second = b.name, ok = a.name == b.name }
    f.type = { first = a.type, second = b.type, ok = a.type == b.type }
    f.id = { first = a.id, second = b.id, note = 'a new object ID is expected', changed = a.id ~= b.id }
    f.mission_id = { first = a.mission_id, second = b.mission_id, changed = a.mission_id ~= b.mission_id }
    f.horizontal_move_m = { moved = moved, expected = expected, ok = math.abs(moved - expected) <= expected / 3 + 25 }
    f.y = number(a.y, b.y, 150)
    f.heading = number(a.heading, b.heading, 0.1)
    f.pitch = number(a.pitch, b.pitch, 0.2)
    f.roll = number(a.roll, b.roll, 0.2)
    f.life = number(a.life, b.life, 0.02 * (a.life0 or 1))
    f.life0 = number(a.life0, b.life0, 1e-6)
    f.fuel_kg = number(a.fuel_kg, b.fuel_kg, 0.01 * (a.fuel_max_kg or 1000) + 5)
    local rows = {}
    local n = math.max(#(a.ammo or {}), #(b.ammo or {}))
    for i = 1, n do
      local r1, r2 = (a.ammo or {})[i], (b.ammo or {})[i]
      rows[i] = {
        type_first = r1 and r1.type, type_second = r2 and r2.type,
        count_first = r1 and r1.count, count_second = r2 and r2.count,
        ok = r1 ~= nil and r2 ~= nil and r1.type == r2.type and r1.count == r2.count,
        offset = (r1 and r2) and (r2.count - r1.count) or nil,
      }
    end
    f.ammo_rows = rows
    out[label] = f
  end
end
return X.enc(out)
]]

local function set_life(ids, percent_of)
  local report = {}
  for label, id in pairs(ids) do
    local numeric = tonumber(id)
    if numeric then
      report[#report + 1] = label .. ':' .. cross('mission',
        'local ok, err = pcall(a_unit_set_life_percentage, ' .. numeric .. ', ' .. percent_of(label)
          .. ') return ok and "ok" or tostring(err)')
    else
      report[#report + 1] = label .. ': no ID (' .. tostring(id) .. ')'
    end
  end
  table.sort(report)
  return report
end

local function parse(text)
  local ids = {}
  for label, id in text:gmatch('(%S+)=(%S+)') do
    ids[label] = id
  end
  return ids
end

E.observe('spawned', cross('server', SPAWN), { info = true })

local ids
local STAGES = {
  { 3, function()
    ids = parse(cross('server', IDS))
    E.observe('ids', ids, { info = true })
    E.observe('damage_set', set_life(ids, function() return DAMAGE_PERCENT end), { info = true })
  end },
  { 1.5, function()
    E.observe('captured', cross('server', (CAPTURE:gsub('@STAGE@', 'first'))), { info = true })
    E.observe('reads_for_flares_chaff_pylons', cross('server', READS), { info = true })
  end },
  { 0.1, function()
    cross('server', DESTROY)
  end },
  { 3, function()
    E.observe('groups_after_destroy', cross('server', GONE), { info = true })
  end },
  { 0.1, function()
    E.observe('respawned', cross('server', (RESPAWN:gsub('@GUN_ROUNDS@', tostring(GUN_ROUNDS)))), { info = true })
  end },
  { 2, function()
    ids = parse(cross('server', IDS))
    local percent = parse(cross('server', LIFE))
    E.observe('life_percent_captured', percent, { info = true })
    E.observe('life_restored', set_life(ids, function(label) return percent[label] end), { info = true })
  end },
  { 1, function()
    E.observe('captured_again', cross('server', (CAPTURE:gsub('@STAGE@', 'second'))), { info = true })
    E.observe('comparison', cross('server', COMPARE), { info = true })
  end },
  { 0, function()
    E.observe('cleared', cross('server', "local E = ...\nreturn tostring(E.lib('scenario').clear())\n"), { info = true })
  end },
}

local stage, mark = 0, DCS.getModelTime()
return E.wait(function()
  local now = DCS.getModelTime()
  local info = STAGES[stage + 1]
  if not info then
    return 'done'
  end
  if now - mark < info[1] then
    return nil
  end
  info[2]()
  stage = stage + 1
  mark = now
  return nil
end, 120, 'running')
