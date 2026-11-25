-- FARP pairs, part 1c: the second unit's name as a way in. `StaticObject.getByName`
-- and `Warehouse.getByName` for each unit name, then 7 Mk-82 set through the
-- warehouse reached from the second name and read through `Airbase.getByName` of
-- the first; put back after.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local MK82 = 'weapons.bombs.Mk_82'
local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then return v end
  return 'raised: ' .. tostring(v)
end
local function id(v)
  return type(v) == 'table' and rawget(v, 'id_') or tostring(v)
end
local out = {}
for _, n in ipairs({ 'np-hg-1', 'np-hg-2' }) do
  out['warehouse_by_name_' .. n] = id(try(Warehouse.getByName, n))
end
local s2 = try(StaticObject.getByName, 'np-hg-2')
local w2 = type(s2) == 'table' and try(function() return s2:getWarehouse() end)
local b1 = try(Airbase.getByName, 'np-hg-1')
local w1 = type(b1) == 'table' and try(function() return b1:getWarehouse() end)
out.second_static = id(s2)
out.second_warehouse = id(w2)
out.first_warehouse = id(w1)
if type(w1) == 'table' and type(w2) == 'table' then
  local before1 = try(function() return w1:getItemCount(MK82) end)
  local before2 = try(function() return w2:getItemCount(MK82) end)
  out.mk82_before = { first = before1, second = before2 }
  try(function() return w2:setItem(MK82, 7) end)
  out.mk82_read_at_first_after_set_7_at_second = try(function() return w1:getItemCount(MK82) end)
  -- Put back each warehouse's own count, the second's last, so a shared
  -- warehouse ends at the count it read.
  if type(before1) == 'number' then try(function() return w1:setItem(MK82, before1) end) end
  if type(before2) == 'number' then try(function() return w2:setItem(MK82, before2) end) end
  out.mk82_put_back = { first = try(function() return w1:getItemCount(MK82) end),
    second = try(function() return w2:getItemCount(MK82) end) }
end
return out
