-- FARP fixes, after running.lua: pause the mission and read, by name, which
-- of the five FARP-fixes statics DCS still finds as a static and as an airbase,
-- and how often world.getAirbases lists each. Run with
-- `cargo xtask eval <target> hook`.
-- Rule: recorded as read.
-- states: hook
local E = ...
DCS.setPause(true)
local ok, v = E.cross('server', [[
  local out = {}
  for _, n in ipairs({ 'np-ff-farp', 'np-ff-pad', 'np-ff-bare', 'np-ff-unl', 'np-ff-ctl' }) do
    local s, a = StaticObject.getByName(n), Airbase.getByName(n)
    local listed = 0
    for _, b in ipairs(world.getAirbases()) do if b:getName() == n then listed = listed + 1 end end
    out[#out + 1] = n .. ' static=' .. tostring(s ~= nil) .. ' airbase=' .. tostring(a ~= nil) .. ' listed=' .. listed
  end
  return table.concat(out, '; ') .. '; airbases=' .. #world.getAirbases()
]])
return { paused = DCS.getPause(), model = DCS.getModelTime(), read = v }
