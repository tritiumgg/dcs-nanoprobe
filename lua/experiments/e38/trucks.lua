-- E38: which vehicles ED's unit database calls trucks, and what it says of
-- supply.
--
-- The `gui` state holds ED's unit database as `db` (LIVE). This snippet
-- lists every vehicle whose attributes name a truck or whose type name
-- suggests supply, with its display name and attributes, and counts the vehicle fields whose
-- names mention supply, rearming, ammunition or reloading.
-- states: gui

local E = ...
local trucks, fields = {}, {}
for _, car in ipairs(db.Units.Cars.Car) do
  local name = tostring(car.type)
  local attributes = {}
  for _, attribute in ipairs(car.attribute or {}) do
    if type(attribute) == 'string' then
      attributes[#attributes + 1] = attribute
    end
  end
  local text = table.concat(attributes, ', ')
  local lower = name:lower()
  if text:find('Truck') or lower:find('ammo') or lower:find('suppl') or lower:find('m939') or lower:find('818') then
    trucks[#trucks + 1] = name .. ' (' .. tostring(car.DisplayName) .. ') [' .. text .. ']'
  end
  for key in pairs(car) do
    local k = tostring(key):lower()
    if k:find('suppl') or k:find('rearm') or k:find('ammo') or k:find('reload') then
      fields[tostring(key)] = (fields[tostring(key)] or 0) + 1
    end
  end
end
table.sort(trucks)
E.observe('trucks', trucks, { info = true })
E.observe('vehicle_fields_about_supply', fields, { info = true })
return #trucks
