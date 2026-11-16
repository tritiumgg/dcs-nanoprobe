-- S19 person test: the coalition and listed aircraft of the shell's flagged
-- airport (12, the Caucasus template's first) and the template's client base
-- (22), read in the server state while the shell is held.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local out = {}
for _, b in ipairs(world.getAirbases()) do
  local id = tonumber(b:getID())
  if id == 12 or id == 22 then
    local inv = b:getWarehouse():getInventory()
    local n = 0
    for _ in pairs(inv.aircraft or {}) do
      n = n + 1
    end
    out[tostring(id)] = { name = b:getName(), coalition = b:getCoalition(), aircraft_types = n }
  end
end
return out
