-- The oracle's early read: where every unit and cargo static stands, and how
-- fast it moves, a few seconds before the oracle itself
-- (lua/sampling/oracle.lua), which adds what this read to its rows and
-- forgets it. `cargo xtask sampling compare` passes a position sample that
-- is late for a moving object only when this read shows the object halted
-- at the sampled position (crates/xtask/src/sampling.rs).
-- states: server
-- timeout: 30
local E = ...
local rows, n = {}, 0

local function note(o, speed)
  local p = o:getPoint()
  rows[o.id_] = { p.x, p.y, p.z, speed }
  n = n + 1
end

for side = 0, 2 do
  for _, group in ipairs(coalition.getGroups(side) or {}) do
    for _, unit in ipairs(group:getUnits() or {}) do
      pcall(function()
        if unit:isExist() then
          local v = unit:getVelocity()
          note(unit, math.sqrt(v.x * v.x + v.y * v.y + v.z * v.z))
        end
      end)
    end
  end
  for _, static in ipairs(coalition.getStaticObjects(side) or {}) do
    pcall(function()
      if static:isExist() and Object.getCategory(static) == 6 then
        local ok, v = pcall(static.getVelocity, static)
        note(static, ok and type(v) == 'table' and math.sqrt(v.x * v.x + v.y * v.y + v.z * v.z) or 0)
      end
    end)
  end
end
E.store.s10_early = { model = timer.getTime(), rows = rows }
return n
