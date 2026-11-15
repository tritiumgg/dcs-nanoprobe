-- E6: do the warehouse setters work on a limited warehouse? Rule: set, then
-- read back.
--
-- The notes of the first live run (PRIOR: setItem and setLiquidAmount raised
-- nothing on an unlimited warehouse, which kept reporting 1,000,000) settle
-- nothing, since an unlimited warehouse ignores a set. So the snippet first
-- reports, for each airbase, whether the mission marks its aircraft,
-- munitions and fuel unlimited. The flags come from the mission's own
-- `warehouses` table, `env.mission.warehouses.airports[<airbase id>]`
-- (ED-CODE: the .miz `warehouses` file, fields unlimitedAircrafts,
-- unlimitedMunitions, unlimitedFuel; UNVERIFIED that the server state
-- exposes `env.mission`). Where the flags are unreadable the snippet says so,
-- and each call's read-back shows the behavior anyway.
--
-- It picks the airbase with the most limited categories, and says in
-- `finding` when every airbase has all three unlimited: then no read-back
-- below can show a set, and the finding is the result. The calls run on that
-- airbase either way, and the snippet sets every value it touched back at
-- the end, and at reset.
--
-- For a weapon item, an aircraft item and each liquid 0 to 3 it reads the
-- count, sets a new one, reads it back with getItemCount or
-- getLiquidAmount and with getInventory, adds 5, removes 3, and removes
-- more than is there. Each step is one entry of its observation: the
-- call, whether it raised, what it returned, and what the reads showed
-- afterward. The weapon and the aircraft come from the airbase's inventory
-- when it lists any, and from a type name the snippet gives when it does not.
-- states: server
-- timeout: 60

local E = ...
local X = E.lib('s15')
local try = X.try

-- Names the snippet uses when the inventory lists no item of a kind.
local FALLBACK_WEAPON = 'weapons.bombs.Mk_82'
local FALLBACK_AIRCRAFT = 'F-15C'
local NEW_ITEMS, NEW_LIQUID = 12, 54321
local LIQUID_NAMES = { [0] = 'jet fuel', [1] = 'aviation gasoline', [2] = 'MW50', [3] = 'diesel' }

-- The warehouse's flags from the mission table, or nil.
local function flags_of(id)
  local warehouses = try(function()
    return env.mission.warehouses
  end)
  if type(warehouses) ~= 'table' then
    return nil
  end
  local entry
  for _, group in ipairs({ 'airports', 'warehouses' }) do
    local list = warehouses[group]
    if type(list) == 'table' then
      entry = list[id] or list[tostring(id)] or entry
    end
  end
  if type(entry) ~= 'table' then
    return nil
  end
  return {
    aircraft = entry.unlimitedAircrafts == true,
    munitions = entry.unlimitedMunitions == true,
    fuel = entry.unlimitedFuel == true,
  }
end

local rows = {}
local best, best_limited
for _, base in ipairs(world.getAirbases()) do
  local id = try(base.getID, base)
  local row = { name = try(base.getName, base), id = id, flags = flags_of(id) }
  local desc = try(base.getDesc, base)
  row.category = type(desc) == 'table' and desc.category or nil
  row.has_warehouse = try(base.getWarehouse, base) ~= nil
  local limited = 0
  if row.flags then
    for _, key in ipairs({ 'aircraft', 'munitions', 'fuel' }) do
      limited = limited + (row.flags[key] and 0 or 1)
    end
  end
  row.limited_categories = row.flags and limited or nil
  rows[#rows + 1] = row
  if row.has_warehouse and (not best or limited > best_limited) then
    best, best_limited = base, limited
  end
end
E.observe('airbases', rows, { info = true })
E.observe('mission_warehouses_readable', rows[1] ~= nil and rows[1].flags ~= nil, { info = true })
if not best then
  E.observe('finding', 'no airbase with a warehouse', { info = true })
  return 'no warehouse'
end
if best_limited == 0 then
  E.observe('finding',
    rows[1] and rows[1].flags
        and 'every airbase has aircraft, munitions and fuel unlimited: no setter can be read back here'
      or 'the flags are unreadable: judge each step by its read-back',
    { info = true })
end
E.observe('chosen_airbase', { name = best:getName(), id = best:getID(), limited_categories = best_limited }, { info = true })

local warehouse = best:getWarehouse()
local methods = {}
-- The methods the object answers to: its metatable's index, or the
-- `Warehouse` class table when the object has no such metatable.
for _, source in ipairs({ function() return getmetatable(warehouse).__index end, function() return Warehouse end }) do
  local ok, t = pcall(source)
  if ok and type(t) == 'table' and #methods == 0 then
    pcall(function()
      for name, value in pairs(t) do
        if type(value) == 'function' then
          methods[#methods + 1] = name
        end
      end
    end)
  end
end
table.sort(methods)
E.observe('warehouse_methods', methods, { info = true })

local inventory = try(warehouse.getInventory, warehouse)
local function section(name)
  local t = type(inventory) == 'table' and inventory[name]
  return type(t) == 'table' and t or {}
end
-- The inventory's keys of a kind with a count above zero, in name order.
local function names_of(kind)
  local out = {}
  for key, count in pairs(section(kind)) do
    if type(key) == 'string' and type(count) == 'number' and count > 0 then
      out[#out + 1] = key
    end
  end
  table.sort(out)
  return out
end
local weapons, aircraft = names_of('weapon'), names_of('aircraft')
E.observe('inventory_sizes', {
  weapon = #weapons,
  aircraft = #aircraft,
  liquids = section('liquids'),
}, { info = true })

-- What the reads say about `key` of `kind` now.
local function read(kind, key)
  local r = {}
  if kind == 'liquid' then
    r.amount = try(warehouse.getLiquidAmount, warehouse, key)
    local inv = try(warehouse.getInventory, warehouse)
    r.inventory = type(inv) == 'table' and type(inv.liquids) == 'table' and inv.liquids[key] or nil
  else
    r.count = try(warehouse.getItemCount, warehouse, key)
    local inv = try(warehouse.getInventory, warehouse)
    local list = type(inv) == 'table' and inv[kind] or nil
    r.inventory = type(list) == 'table' and list[key] or nil
  end
  return r
end

-- One call, as an entry: its name, whether it raised, and what it returned.
local function step(label, kind, key, fn)
  local before = read(kind, key)
  local ok, value = pcall(fn)
  return {
    step = label,
    raised = not ok or nil,
    error = (not ok) and tostring(value) or nil,
    returned = ok and value or nil,
    before = before,
    after = read(kind, key),
  }
end

local function exercise(label, kind, key, source)
  local steps = {}
  local original = read(kind, key)
  local base = (kind == 'liquid' and original.amount or original.count) or 0
  local target = kind == 'liquid' and NEW_LIQUID or NEW_ITEMS
  if target == base then
    target = target + 1
  end
  local get = kind == 'liquid' and 'amount' or 'count'
  local set, add, remove
  if kind == 'liquid' then
    set = function(n) return warehouse:setLiquidAmount(key, n) end
    add = function(n) return warehouse:addLiquid(key, n) end
    remove = function(n) return warehouse:removeLiquid(key, n) end
  else
    set = function(n) return warehouse:setItem(key, n) end
    add = function(n) return warehouse:addItem(key, n) end
    remove = function(n) return warehouse:removeItem(key, n) end
  end
  E.track.defer('warehouse ' .. label, function()
    pcall(set, base)
  end)
  steps[#steps + 1] = { step = 'read', key = key, source = source, read = original }
  steps[#steps + 1] = step('set ' .. target, kind, key, function() return set(target) end)
  local now = steps[#steps].after[get]
  steps[#steps + 1] = step('add 5', kind, key, function() return add(5) end)
  steps[#steps + 1] = step('remove 3', kind, key, function() return remove(3) end)
  local left = steps[#steps].after[get]
  steps[#steps + 1] = step('remove past zero (' .. ((left or 0) + 10) .. ')', kind, key, function()
    return remove((left or 0) + 10)
  end)
  steps[#steps + 1] = step('set back ' .. base, kind, key, function() return set(base) end)
  local verdict = {
    set_read_back = now == target,
    set_requested = target,
    set_read = now,
    original = base,
    restored = steps[#steps].after[get] == base,
  }
  E.observe(label, { verdict = verdict, steps = steps }, { info = true })
end

if #weapons > 0 then
  exercise('item_weapon', 'weapon', weapons[1], 'inventory')
else
  exercise('item_weapon', 'weapon', FALLBACK_WEAPON, 'fallback')
end
if #aircraft > 0 then
  exercise('item_aircraft', 'aircraft', aircraft[1], 'inventory')
else
  exercise('item_aircraft', 'aircraft', FALLBACK_AIRCRAFT, 'fallback')
end
for liquid = 0, 3 do
  exercise('liquid_' .. liquid, 'liquid', liquid, LIQUID_NAMES[liquid])
end
return 'done'
