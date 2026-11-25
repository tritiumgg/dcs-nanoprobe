-- FARP groups, run 2, last: destroy every object under the names of
-- `one.lua`'s groups and each replaced group's old airbase, and clear the
-- global. A mission reload follows.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local G = _G.np_farp_one
if not G then
  return 'nothing to clean'
end
local function exists(o)
  local ok, v = pcall(function() return o:isExist() end)
  return ok and v or false
end
local out = { old = {}, destroyed = 0 }
for tag, o in pairs(G.old) do
  out.old[tag] = exists(o)
  if exists(o) then
    pcall(function() o:destroy() end)
    out.destroyed = out.destroyed + 1
  end
end
for tag in pairs(G.rows) do
  for k = 1, 2 do
    local st = StaticObject.getByName('np-fg-' .. tag .. '-' .. k)
    if st and exists(st) then
      pcall(function() st:destroy() end)
      out.destroyed = out.destroyed + 1
    end
  end
end
_G.np_farp_one = nil
return out
