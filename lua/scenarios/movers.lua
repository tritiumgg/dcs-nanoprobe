-- The mover count, for the benchmarks. Load it with E.lib('movers') in the
-- `server` state.
--
-- Development only. A frame rate means little without knowing how many
-- ground groups and ships were driving while it was measured, since a
-- parked object costs DCS far less than one that moves. `count` reads the
-- lead unit of every ground group and ship whose name starts with a prefix
-- and sets it against where it stood at the previous call: a group moved
-- when its lead unit is more than `M.THRESHOLD` metres from there.
--
-- The first call only records positions. A group that is new since the
-- last call has no earlier position and is counted as `new`, not as moved
-- or still, so churn does not read as parked groups.

local M = { THRESHOLD = 20, last = {} }

-- Count the groups whose name starts with `o.prefix` (default `np-`).
-- Returns `groups` (alive and measured), `moved`, `still`, `new`, and
-- `share`, moved over moved plus still (nil when none had an earlier
-- position). Groups gone since the last call are forgotten.
function M.count(o)
  o = o or {}
  local prefix = o.prefix or 'np-'
  local wanted = { [Group.Category.GROUND] = true, [Group.Category.SHIP] = true }
  local seen, moved, still, new = {}, 0, 0, 0
  for side = 0, 2 do
    for _, group in ipairs(coalition.getGroups(side) or {}) do
      local name = group:getName()
      if name:sub(1, #prefix) == prefix and wanted[group:getCategory()] then
        local unit = group:getUnit(1)
        if unit and unit:isExist() then
          local p = unit:getPoint()
          local before = M.last[name]
          if before then
            local d = math.sqrt((p.x - before.x) ^ 2 + (p.z - before.z) ^ 2)
            if d > M.THRESHOLD then
              moved = moved + 1
            else
              still = still + 1
            end
          else
            new = new + 1
          end
          seen[name] = { x = p.x, z = p.z }
        end
      end
    end
  end
  M.last = seen
  local measured = moved + still
  return {
    groups = measured + new,
    moved = moved,
    still = still,
    new = new,
    share = measured > 0 and moved / measured or nil,
  }
end

-- Forget every position, so the next call is a first call.
function M.reset()
  M.last = {}
end

return M
