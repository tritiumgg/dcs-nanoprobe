-- Catalog: ground units and a static. Spawn, a weak explosion that
-- damages, a strong one that kills, a static killed, and a scripted removal.
-- states: server
-- timeout: 90

local E = ...
local S = E.lib('scenario')
local R = E.lib('recorder')
R.clear()
R.start()

local origin = S.origin()
local step, mark = 1, timer.getTime()
local function after(seconds)
  return timer.getTime() - mark >= seconds
end
local function next_step(cause)
  step, mark = step + 1, timer.getTime()
  if cause then
    R.mark(cause)
  end
end

R.mark('spawn')
S.ground('np-cg', 'M-113', {
  { name = 'np-cg-1', x = origin.x + 400, z = origin.z - 300 },
  { name = 'np-cg-2', x = origin.x + 450, z = origin.z - 300 },
})
S.spawn_static('np-cs', 400, -400, 'FARP Fuel Depot', 'Fortifications')

return E.wait(function()
  if step == 1 then
    if after(3) then
      -- A weak explosion 12 m from the second unit, to damage it.
      next_step('damage')
      local u = Unit.getByName('np-cg-2')
      local p = u:getPoint()
      trigger.action.explosion({ x = p.x + 12, y = p.y, z = p.z }, 2)
    end
  elseif step == 2 then
    if after(5) then
      next_step('explosion')
      if Unit.getByName('np-cg-1') then
        S.kill('np-cg-1', 500)
      end
    end
  elseif step == 3 then
    if after(8) then
      next_step('static')
      local st = StaticObject.getByName('np-cs')
      if st then
        trigger.action.explosion(st:getPoint(), 2000)
      end
    end
  elseif step == 4 then
    if after(8) then
      next_step('destroy')
      S.destroy('np-cg')
    end
  elseif step == 5 then
    if after(3) then
      E.observe('catalog', R.report(), { info = true })
      -- The static, if the explosion left it, goes before the next
      -- scenario. A scripted removal emits no event (LIVE,
      -- lua/probes/32-events.lua).
      S.clear()
      return 'done'
    end
  end
  return nil
end, 60, 'running')
