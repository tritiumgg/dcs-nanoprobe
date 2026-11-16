-- Person tests: set the life of `np-person-kill-1` to 0 with the damage
-- setter the platform's DAMAGE command uses (a_unit_set_life_percentage, in
-- the mission state; ED-CODE, MissionEditor/modules/me_trigrules.lua), then
-- read back isExist and the unit by name at 1, 3 and 10 s. The person says
-- what the client shows: a burning wreck, a wreck, nothing, or the live unit.
-- Rule: recorded as the person reports.
-- states: hook
-- timeout: 30

local E = ...
local function server(source)
  local ok, value = E.cross('server', source)
  return ok and value or ('cross failed: ' .. tostring(value))
end
local READ = [[
local u = Unit.getByName('np-person-kill-1')
if not u then return 'by name: none' end
local ok, exists = pcall(function() return u:isExist() end)
local okl, life = pcall(function() return u:getLife() end)
return 'by name: found, isExist=' .. tostring(ok and exists) .. ', life=' .. tostring(okl and life)
]]
local raw = server([[
local u = Unit.getByName('np-person-kill-1')
return u and u:getID() or 'none'
]])
local id = tonumber(raw)
if not id then
  error('no np-person-kill-1 (' .. tostring(raw) .. '); run kill-spawn.lua first')
end
E.observe('before', server(READ), { info = true })
local text = net.dostring_in('mission',
  'local ok, err = pcall(a_unit_set_life_percentage, ' .. id .. ', 0) return ok and "ok" or tostring(err)')
E.observe('set_life_0', tostring(text), { info = true })
local marks, seen = { 1, 3, 10 }, {}
local t0 = DCS.getRealTime()
return E.wait(function()
  local elapsed = DCS.getRealTime() - t0
  for i, mark in ipairs(marks) do
    if not seen[i] and elapsed >= mark then
      seen[i] = true
      E.observe('after_' .. mark .. 's', server(READ), { info = true })
    end
  end
  if seen[#marks] then
    return 'done'
  end
end, 20, 'running')
