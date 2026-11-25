-- FARP groups, run 2, after `one-run.lua`: every name of the four groups again,
-- whether each replaced group's old airbase still exists, and the q replaces'
-- reads made while the mission ran. `one-clean.lua` follows.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local G = _G.np_farp_one
if not G then
  error('one.lua has not run')
end
local function exists(o)
  local ok, v = pcall(function() return o:isExist() end)
  return ok and v or false
end
local out = { airbases = #world.getAirbases(), running = G.q, old = {} }
for tag in pairs(G.rows) do
  out[tag] = G.read(tag)
end
for tag, o in pairs(G.old or {}) do
  out.old[tag] = exists(o)
end
return out
