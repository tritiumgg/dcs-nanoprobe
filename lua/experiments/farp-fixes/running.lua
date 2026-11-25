-- FARP fixes, after `destroy.lua` and `after.lua`, whose destroys DCS had not
-- taken on the paused server: whether `destroy` takes the heliports while the
-- simulation runs. It resumes, then adds a fortification as the control and
-- destroys it, the FARP static and the radio helipad's airbase, reads after 3 s
-- of model time, and pauses again. Run with `cargo xtask eval <target> hook`.
-- In run 1 it raised at the control's spawn, which reads the FARP's place: DCS
-- had already taken the earlier destroys once the mission ran. The mission
-- stayed running until `pause-read.lua` paused it at model time 7.7;
-- `left.lua` and `count.lua` read what was left.
-- Rule: recorded as read.
-- states: hook
-- timeout: 60

local E = ...

local function cross(source)
  local ok, value = E.cross('server', source)
  if not ok then
    error('server: ' .. tostring(value))
  end
  return value
end

-- Reads each object by name, as a text line.
local READ = [[
  local out = {}
  for _, n in ipairs({ 'np-ff-farp', 'np-ff-pad', 'np-ff-bare', 'np-ff-unl', 'np-ff-ctl' }) do
    local s, a = StaticObject.getByName(n), Airbase.getByName(n)
    local ex = s and select(2, pcall(s.isExist, s))
    out[#out + 1] = n .. ' static=' .. tostring(s ~= nil) .. ' exists=' .. tostring(ex)
      .. ' airbase=' .. tostring(a ~= nil)
  end
  local count = #world.getAirbases()
  return table.concat(out, '; ') .. '; airbases=' .. count
]]

local out = { paused_at_start = DCS.getPause() }
local step, at = 'resume', nil
return E.wait(function()
  if step == 'resume' then
    DCS.setPause(false)
    step = 'destroy'
    return nil
  end
  if step == 'destroy' then
    if DCS.getPause() then
      return nil
    end
    out.before = cross(READ)
    out.calls = cross([[
      local r = {}
      local base = Airbase.getByName('np-ff-farp'):getPoint()
      local ok, c = pcall(coalition.addStaticObject, country.id.USA, { category = 'Fortifications',
        type = 'FARP Fuel Depot', name = 'np-ff-ctl', x = base.x, y = base.z - 300, heading = 0 })
      r[#r + 1] = 'control added=' .. tostring(ok and c ~= nil)
      for _, n in ipairs({ 'np-ff-ctl', 'np-ff-farp' }) do
        local s = StaticObject.getByName(n)
        r[#r + 1] = n .. ' destroy=' .. (s and tostring(pcall(s.destroy, s)) or 'none')
      end
      local a = Airbase.getByName('np-ff-pad')
      r[#r + 1] = 'np-ff-pad airbase destroy=' .. (a and tostring(pcall(Object.destroy, a)) or 'none')
      return table.concat(r, '; ')
    ]])
    out.right_after = cross(READ)
    at = DCS.getModelTime()
    step = 'read'
    return nil
  end
  if step == 'read' then
    if DCS.getModelTime() - at < 3 then
      return nil
    end
    out.after_3s = cross(READ)
    out.model = DCS.getModelTime()
    DCS.setPause(true)
    return out
  end
end, 50)
