-- The pause matrix (E17) at model time 0: a mission that starts paused, as
-- a server with no client does. Runs only on a freshly loaded mission that
-- has not run yet; `cargo xtask run` loads one first.
-- states: hook
-- timeout: 60

local E = ...
if not DCS.getPause() then
  E.note('the simulation is not paused; nothing observed')
  return 'skipped'
end
local ok, model = E.cross('server', 'return timer.getTime()')
if not ok then
  error('server: ' .. tostring(model))
end
E.observe('model_time', tonumber(model), { info = true })
if tonumber(model) ~= 0 then
  E.note('the mission has already run; nothing observed')
  return 'skipped'
end

return E.wait(E.lib('pause').start(E, 'start'), 20)
