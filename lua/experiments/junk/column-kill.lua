-- The clear-debris check's kill: each named unit is killed with an explosion
-- of power 30, 6 m beside it. Its handle is kept in `E.store.debris`, in order
-- and by name, so a later read tells a wreck that is left (its reads answer)
-- from one a clear removed (they raise; LIVE,
-- docs/results/2026-11-16-clear-debris.md, run 1). Returns `hit y`: the units
-- found and the height of the first. The caller replaces the placeholder with
-- quoted names.
-- states: server
-- timeout: 60

local E = ...
local y
local hit = { by_name = {} }
for _, n in ipairs({ __NAMES__ }) do
  local u = Unit.getByName(n)
  if u then
    local p = u:getPoint()
    y = y or p.y
    hit[#hit + 1] = u
    hit.by_name[n] = u
    trigger.action.explosion({ x = p.x, y = p.y, z = p.z - 6 }, 30)
  end
end
E.store.debris = hit
return string.format('%d %.1f', #hit, y or 0)
