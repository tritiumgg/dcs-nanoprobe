-- FARP groups, run 2, after `one.lua`: resume the mission, after 2 s of
-- model time do the q replaces (q2 to one FARP, q1 to two) while it runs,
-- and pause 10 s later. `one-after.lua` reads. The wait asks for `running`,
-- so the eval hook keeps the mission running while it waits. Run with
-- `cargo xtask eval <target> hook --timeout 60`.
-- Rule: recorded as read.
-- states: hook
-- timeout: 60

local E = ...
if #net.get_player_list() > 1 then
  error('players connected')
end
local t0 = DCS.getModelTime()
local out = { from = t0 }
return E.wait(function()
  local now = DCS.getModelTime()
  if not out.replaced and now >= t0 + 2 then
    if #net.get_player_list() > 1 then
      DCS.setPause(true)
      error('players connected')
    end
    local ok, v = E.cross('server', [[
      local G = _G.np_farp_one
      G.q = { q2 = G.replace('q2', 1), q1 = G.replace('q1', 2), model = timer.getTime() }
      return 'done'
    ]])
    out.replaced = ok and v or ('raised: ' .. tostring(v))
  elseif out.replaced and now >= t0 + 12 then
    DCS.setPause(true)
    out.to = now
    return out
  end
end, 50, 'running')
