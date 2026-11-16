-- E39 recon: what the `server` state knows about a type's ammunition layout
-- and an aircraft's launchers, and what a default spawn of each type reads.
--
-- The Mission Editor builds a ground unit's `ammo` table from the unit
-- database: `DB.unit_by_type[type].WS[ws].LN[ln].PL[pl]`, keeping only the
-- PL entries with a `virtualStwID`, with `ammo_capacity` and a name from
-- `display_shell_name`, `name_ammunition` or `shell_name[1]` (ED-CODE:
-- MissionEditor/modules/me_loadoututilsVehicles.lua, getAmmoUnitDef). It
-- reads an aircraft launcher's weapon and count with
-- `get_weapon_and_count_from_launcher(CLSID)` (ED-CODE:
-- MissionEditor/modules/me_loadout.lua, callbackSettingsCopy). The `server`
-- state has `db.Units`, `db.Weapons` and that function (LIVE: the API dump
-- in lua/mock/api/server.txt). This snippet records, for each type:
--   1. where its definition sits in `db.Units`, its top-level keys, and
--      every WS/LN/PL entry with its scalar fields and shell names;
--   2. for an aircraft, its pylons and their first launchers' CLSIDs, and
--      what the launcher function and `db.Weapons` say of each;
--   3. a default spawn's `getAmmo` rows, with every scalar of each row's
--      `desc`, and `getLife`, `getLife0`, `getFuel` and `fuelMassMax`.
-- It adds one group a type, reads it after SETTLE_S of model time, and
-- removes it.
-- states: server
-- timeout: 120

local E = ...
local X = E.lib('s15')
local S = X.S

local GROUND = { 'M-113', 'T-72B', 'M-1 Abrams', 'BMP-2', 'M-2 Bradley', 'ZSU-23-4 Shilka', 'Leopard-2' }
local SHIPS = { 'PERRY', 'speedboat' }
local AIR = { { 'F-15C', Group.Category.AIRPLANE }, { 'UH-1H', Group.Category.HELICOPTER } }
local SETTLE_S = 2

local function scalars(t)
  local out = {}
  if type(t) ~= 'table' then
    return out
  end
  for k, v in pairs(t) do
    local kind = type(v)
    if kind == 'number' or kind == 'string' or kind == 'boolean' then
      out[tostring(k)] = v
    end
  end
  return out
end

local function keys(t)
  local out = {}
  if type(t) ~= 'table' then
    return out
  end
  for k, v in pairs(t) do
    out[#out + 1] = tostring(k) .. ':' .. type(v)
  end
  table.sort(out)
  return out
end

local function first(t, n)
  local out = {}
  if type(t) ~= 'table' then
    return out
  end
  for i = 1, math.min(#t, n) do
    out[i] = t[i]
  end
  return out
end

E.observe('apis', {
  db = type(db),
  db_units = type(db) == 'table' and type(db.Units) or 'no db',
  db_weapons = type(db) == 'table' and type(db.Weapons) or 'no db',
  launcher_fn = type(get_weapon_and_count_from_launcher),
  display_by_wstype = type(get_weapon_display_name_by_wstype),
  display_by_clsid = type(get_weapon_display_name_by_clsid),
  getDescByName = type(Unit.getDescByName),
  db_units_keys = keys(type(db) == 'table' and db.Units),
  db_weapons_keys = keys(type(db) == 'table' and db.Weapons),
}, { info = true })

-- The definition of a type: db.Units.<cat>.<sub>[i] whose `type` or `Name`
-- is the type name.
local function find_def(name)
  if type(db) ~= 'table' or type(db.Units) ~= 'table' then
    return nil, 'no db.Units'
  end
  for cat, ct in pairs(db.Units) do
    if type(ct) == 'table' then
      for sub, list in pairs(ct) do
        if type(list) == 'table' then
          for i, def in ipairs(list) do
            if type(def) == 'table' and (def.type == name or def.Name == name) then
              return def, cat .. '.' .. sub .. '[' .. i .. ']'
            end
          end
        end
      end
    end
  end
  return nil, 'not found'
end

local function ws_layout(def)
  local rows = {}
  if type(def.WS) ~= 'table' then
    return rows
  end
  for ws, w in pairs(def.WS) do
    if type(w) == 'table' and type(w.LN) == 'table' then
      for ln, l in pairs(w.LN) do
        if type(l) == 'table' then
          local lscal = scalars(l)
          if type(l.PL) == 'table' then
            for pl, p in pairs(l.PL) do
              if type(p) == 'table' then
                rows[#rows + 1] = {
                  ws = ws, ln = ln, pl = pl, ln_scalars = lscal, pl_scalars = scalars(p),
                  shell_name = first(p.shell_name, 4), pl_keys = keys(p),
                }
              end
            end
          else
            rows[#rows + 1] = { ws = ws, ln = ln, ln_scalars = lscal, no_pl = true, ln_keys = keys(l) }
          end
        end
      end
    end
  end
  return rows
end

local function launchers(def)
  local out = {}
  if type(def.Pylons) ~= 'table' then
    return out
  end
  for i, pylon in ipairs(def.Pylons) do
    local entry = { i = i, scalars = scalars(pylon), launchers = {} }
    for j, launcher in ipairs(type(pylon.Launchers) == 'table' and pylon.Launchers or {}) do
      if j > 4 then
        break
      end
      local clsid = type(launcher) == 'table' and launcher.CLSID
      local l = { clsid = clsid }
      if clsid then
        local ok, weapon, count = pcall(get_weapon_and_count_from_launcher, clsid)
        l.call_ok, l.weapon, l.count = ok, weapon, count
        if ok and weapon ~= nil then
          local dok, display = pcall(get_weapon_display_name_by_wstype, weapon)
          l.display_by_wstype = dok and display or ('x: ' .. tostring(display))
        end
        local cok, cdisplay = pcall(get_weapon_display_name_by_clsid, clsid)
        l.display_by_clsid = cok and cdisplay or ('x: ' .. tostring(cdisplay))
        local by = type(db.Weapons) == 'table' and type(db.Weapons.ByCLSID) == 'table' and db.Weapons.ByCLSID[clsid]
        if by then
          l.by_clsid = scalars(by)
          l.by_clsid_keys = keys(by)
        end
      end
      entry.launchers[#entry.launchers + 1] = l
    end
    out[#out + 1] = entry
  end
  return out
end

local subjects = {}
for _, t in ipairs(GROUND) do
  subjects[#subjects + 1] = { type = t, kind = 'ground' }
end
for _, t in ipairs(SHIPS) do
  subjects[#subjects + 1] = { type = t, kind = 'ship' }
end
for _, a in ipairs(AIR) do
  subjects[#subjects + 1] = { type = a[1], kind = 'air', category = a[2] }
end

for i, s in ipairs(subjects) do
  local def, where = find_def(s.type)
  local d = { where = where }
  if def then
    d.top_keys = keys(def)
    d.scalars = scalars(def)
    d.ws = ws_layout(def)
    if s.kind == 'air' then
      d.pylons = launchers(def)
      d.passivCounterm = scalars(def.passivCounterm)
      d.passivCounterm_flare = scalars(type(def.passivCounterm) == 'table' and def.passivCounterm.flare)
      d.passivCounterm_chaff = scalars(type(def.passivCounterm) == 'table' and def.passivCounterm.chaff)
      d.Guns = type(def.Guns) == 'table' and #def.Guns or type(def.Guns)
    end
  end
  E.observe('def_' .. i .. '_' .. s.type, d, { info = true })
end

-- Spawn one default unit of each type.
local origin = S.origin()
local water = S.water()
local added = {}
for i, s in ipairs(subjects) do
  local label = 'np-e39r-' .. i
  local group, err
  if s.kind == 'ground' then
    group, err = X.add_ground(label, s.type, { { name = label .. '-1', x = origin.x + 3000 + i * 60, z = origin.z + 12000 } })
  elseif s.kind == 'ship' then
    if water then
      group, err = X.add_ground(label, s.type, { { name = label .. '-1', x = water.x + i * 400, z = water.z } },
        { category = Group.Category.SHIP })
    else
      err = 'no water within 60 km'
    end
  else
    group, err = X.add_air({ name = label, kind = s.type, category = s.category, x = origin.x - i * 3000,
      z = origin.z + 20000, alt = s.category == Group.Category.HELICOPTER and 500 or 6000,
      speed = s.category == Group.Category.HELICOPTER and 40 or 220, fuel = 1000, gun = 100,
      task = s.category == Group.Category.HELICOPTER and 'Transport' or 'CAP' })
  end
  added[i] = { label = label, ok = group ~= nil, error = err }
end
E.observe('added', added, { info = true })

local due = timer.getTime() + SETTLE_S
return E.wait(function()
  if timer.getTime() < due then
    return nil
  end
  for i, s in ipairs(subjects) do
    local label = added[i].label
    local unit = Unit.getByName(label .. '-1')
    local r = { found = unit ~= nil }
    if unit then
      local rows = X.try(unit.getAmmo, unit)
      r.ammo = {}
      for j, row in ipairs(type(rows) == 'table' and rows or {}) do
        r.ammo[j] = { count = row.count, desc = scalars(row.desc), desc_keys = keys(row.desc) }
      end
      r.life = X.try(unit.getLife, unit)
      r.life0 = X.try(unit.getLife0, unit)
      r.fuel = X.try(unit.getFuel, unit)
      local desc = X.try(unit.getDesc, unit)
      r.fuelMassMax = type(desc) == 'table' and desc.fuelMassMax or nil
      r.in_air = X.try(unit.inAir, unit)
    end
    E.observe('read_' .. i .. '_' .. s.type, r, { info = true })
    S.destroy(label)
  end
  return 'done'
end, 60, 'running')
