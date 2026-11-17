-- Warehouse evidence: why an unlimited airbase reads below 1,000,000 in
-- `getInventory` on one mission while E6 read 1,000,000 on another.
--
-- Hypothesis: `getInventory` lists what the mission file lists for the
-- warehouse (the editor writes no `aircrafts` and no `weapons` for an
-- unlimited airport; ED-CODE, me_mission.lua unloadAirportsEquipment), while
-- `getItemCount` and `getLiquidAmount` of an unlimited warehouse read
-- 1,000,000.
--
-- For every airbase with a warehouse: the mission's unlimited flags where the
-- state exposes them, the number of `getInventory` entries of each kind, the
-- liquid amounts it lists, and `getItemCount` of ITEMS and `getLiquidAmount` of
-- liquids 0 to 3. Then E6's control on the first airbase whose flags say
-- unlimited (else whose by-name reads are 1,000,000): `addItem` 5 of the
-- weapon, both reads, `removeItem` 5, both reads. The count is restored at
-- the end and at reset.
-- Rule: each kind the mission's flags mark unlimited at an airbase is
-- checked on its own: munitions, the weapon's `getItemCount` reads 1,000,000
-- and `getInventory` lists no more weapons than the file; aircraft, the same
-- for the aircraft; fuel, `getLiquidAmount` of liquids 0 to 3 reads
-- 1,000,000. The hypothesis holds when at least one kind was checked and
-- every check passed, and fails on any failed check. With the flags
-- unreadable, or no kind marked unlimited, the result is inconclusive and
-- settles nothing. Recorded as read either way.
-- states: server
-- timeout: 30

local E = ...
local UNLIMITED = 1000000
local ITEMS = { weapon = 'weapons.bombs.Mk_82', aircraft = 'F-15C' }

local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then
    return v
  end
  return nil
end

local function flags_of(id)
  local w = try(function() return env.mission.warehouses end)
  if type(w) ~= 'table' then
    return nil
  end
  -- `airports` is keyed by airdrome ID and `warehouses` by static unit ID,
  -- which can be equal: `airports` comes first.
  local entry, from
  for _, group in ipairs({ 'airports', 'warehouses' }) do
    local list = w[group]
    if not entry and type(list) == 'table' then
      entry = list[id] or list[tostring(id)]
      from = entry and group
    end
  end
  if type(entry) ~= 'table' then
    return nil
  end
  local function count(t)
    local n = 0
    for _ in pairs(type(t) == 'table' and t or {}) do n = n + 1 end
    return n
  end
  return {
    group = from,
    aircraft = entry.unlimitedAircrafts == true,
    munitions = entry.unlimitedMunitions == true,
    fuel = entry.unlimitedFuel == true,
    file_aircrafts = count(entry.aircrafts),
    file_weapons = count(entry.weapons),
  }
end

local function read(wh)
  local r = { inventory = {}, liquids = {}, by_name = {}, liquid_amount = {} }
  local inv = try(wh.getInventory, wh)
  for _, kind in ipairs({ 'weapon', 'aircraft', 'liquids' }) do
    local n = 0
    for k, v in pairs(type(inv) == 'table' and type(inv[kind]) == 'table' and inv[kind] or {}) do
      n = n + 1
      if kind == 'liquids' then
        r.liquids[tostring(k)] = v
      end
    end
    r.inventory[kind] = n
  end
  for kind, name in pairs(ITEMS) do
    r.by_name[kind] = try(wh.getItemCount, wh, name)
    local list = type(inv) == 'table' and inv[kind]
    r.inventory[kind .. '_listed'] = type(list) == 'table' and list[name] or nil
  end
  for i = 0, 3 do
    r.liquid_amount[tostring(i)] = try(wh.getLiquidAmount, wh, i)
  end
  return r
end

local rows, chosen = {}, nil
for _, base in ipairs(world.getAirbases()) do
  local wh = try(base.getWarehouse, base)
  if wh then
    local id = try(base.getID, base)
    local row = { name = try(base.getName, base), id = id, flags = flags_of(id), read = read(wh) }
    rows[#rows + 1] = row
    local unlimited = row.flags and row.flags.munitions
      or (not row.flags and row.read.by_name.weapon == UNLIMITED)
    if unlimited and not chosen then
      chosen = { base = base, wh = wh, row = row }
    end
  end
end
E.observe('airbases', rows, { info = true })

local checked, failed = 0, {}
local function check(row, kind, ok)
  checked = checked + 1
  if not ok then
    failed[#failed + 1] = tostring(row.name) .. ': ' .. kind
  end
end
local readable = false
for _, row in ipairs(rows) do
  local f, r = row.flags, row.read
  if f then
    readable = true
    if f.munitions then
      check(row, 'munitions', r.by_name.weapon == UNLIMITED and r.inventory.weapon <= f.file_weapons)
    end
    if f.aircraft then
      check(row, 'aircraft', r.by_name.aircraft == UNLIMITED and r.inventory.aircraft <= f.file_aircrafts)
    end
    if f.fuel then
      local all = true
      for i = 0, 3 do
        all = all and r.liquid_amount[tostring(i)] == UNLIMITED
      end
      check(row, 'fuel', all)
    end
  end
end
E.observe('flags_readable', readable, { info = true })
E.observe('checked', checked, { info = true })
E.observe('failed', failed, { info = true })

if chosen then
  local wh, name = chosen.wh, ITEMS.weapon
  local start = try(wh.getItemCount, wh, name)
  E.track.defer('unlimited-read restore', function()
    if type(start) == 'number' and start < UNLIMITED then
      pcall(wh.setItem, wh, name, start)
    end
  end)
  local control = { airbase = chosen.row.name, start = start }
  control.add_ok = pcall(wh.addItem, wh, name, 5)
  control.after_add = read(wh)
  control.remove_ok = pcall(wh.removeItem, wh, name, 5)
  control.after_remove = read(wh)
  E.observe('control', control, { info = true })
end
local verdict = 'inconclusive'
if #failed > 0 then
  verdict = 'fails'
elseif checked > 0 then
  verdict = 'holds'
end
E.observe('verdict', verdict, { info = true })
return verdict
