-- The route-shape scenarios of experiment E37 (`cargo xtask routes`): the
-- same number of ground units laid out in one of several shapes of route, so
-- DCS's own cost of moving them can be told apart. Load it with
-- E.lib('routes') in the `server` state.
--
-- Development only. Every group is named `np-<label>-g<n>` and registered
-- with the scenario library, so `E.lib('scenario').clear()` removes it.
--
-- Shapes (`R.SHAPES`):
--   halted         groups of 10 that stay where they are (the floor)
--   offroad-few    groups of 10, two waypoints `length` metres apart, off road
--   offroad-dense  the same path with a waypoint every `step` metres
--   onroad         groups of 10 with two road points (land.getClosestPointOnRoads)
--   formation-off, formation-line, formation-column, formation-diamond
--                  groups of 4 on the offroad-few path with the formation
--                  `R.FORMATIONS` names in the first waypoint
--   size-1, size-4, size-10
--                  groups of 1, 4 and 10 on the offroad-few path
--   held-order     offroad-few groups given the `Hold` task after the spawn
--   held-zero      groups of 10 whose route is one waypoint at speed 0, which
--                  is how the scenario library holds a group
-- Each group drives in a lane of its own, `lane_m` apart, `lanes_per_block`
-- to a block, blocks `block_m` apart westward, so no group drives into
-- another.

local L = ...
local S = L.lib('scenario')
local X = L.lib('s15')
local R = { lane = 0 }

R.SHAPES = {
  'halted', 'offroad-few', 'offroad-dense', 'onroad',
  'formation-off', 'formation-line', 'formation-column', 'formation-diamond',
  'size-1', 'size-4', 'size-10', 'held-order', 'held-zero',
}

-- The route-point action (ED's AI.Task.VehicleFormation) for each formation
-- name. DCS has no formation called column: `Cone` is the narrowest of its
-- wedges, so the mapping is UNVERIFIED, and `o.formations` replaces any entry.
R.FORMATIONS = { off = 'Off Road', line = 'Rank', column = 'Cone', diamond = 'Diamond' }

-- Per shape: group size, how the route is laid, and what else.
local CONFIG = {
  ['halted'] = { per = 10, route = 'none' },
  ['offroad-few'] = { per = 10, route = 'few' },
  ['offroad-dense'] = { per = 10, route = 'dense' },
  ['onroad'] = { per = 10, route = 'road' },
  ['formation-off'] = { per = 4, route = 'few', formation = 'off' },
  ['formation-line'] = { per = 4, route = 'few', formation = 'line' },
  ['formation-column'] = { per = 4, route = 'few', formation = 'column' },
  ['formation-diamond'] = { per = 4, route = 'few', formation = 'diamond' },
  ['size-1'] = { per = 1, route = 'few' },
  ['size-4'] = { per = 4, route = 'few' },
  ['size-10'] = { per = 10, route = 'few' },
  ['held-order'] = { per = 10, route = 'few', hold = true },
  ['held-zero'] = { per = 10, route = 'zero' },
}

-- The number of waypoints of the dense shape for a path of `length` metres
-- with one every `step` metres, the two ends included.
function R.dense_points(length, step)
  return math.floor(length / step + 0.5) + 1
end

-- The route points `{ x, z, speed, action }` of a straight path from
-- (x, z) east for `length` metres: two points, or a point every `step`.
function R.path(x, z, length, speed, dense, step, action)
  local points = {}
  local n = dense and R.dense_points(length, step) or 2
  for i = 0, n - 1 do
    points[#points + 1] = { x = x + length * i / (n - 1), z = z, speed = speed, action = action }
  end
  return points
end

-- Road points: up to `count` points on a road, 300 m or more apart, found by
-- asking land.getClosestPointOnRoads for the nearest road to each point of a
-- grid around the origin. Returns the list, and the error text when the call
-- is not there.
function R.road_points(count, origin, grid_m, reach_m)
  local found = {}
  local reach = reach_m or 30000
  local grid = grid_m or 1000
  for ring = 0, math.floor(reach / grid) do
    for ix = -ring, ring do
      for iz = -ring, ring do
        if math.max(math.abs(ix), math.abs(iz)) == ring then
          local ok, px, pz = pcall(land.getClosestPointOnRoads, 'roads', origin.x + ix * grid, origin.z + iz * grid)
          if not ok then
            return found, tostring(px)
          end
          if px and pz then
            local fresh = true
            for _, p in ipairs(found) do
              if (p.x - px) ^ 2 + (p.z - pz) ^ 2 < 300 ^ 2 then
                fresh = false
                break
              end
            end
            if fresh then
              found[#found + 1] = { x = px, z = pz }
              if #found >= count then
                return found
              end
            end
          end
        end
      end
    end
  end
  return found
end

-- The end of a road route from `start`: the road point nearest to a point
-- `length` metres off in the first of four directions that lands at least
-- half that far away. Nil when none does.
local function road_end(start, length)
  for _, angle in ipairs({ 0, math.pi / 2, math.pi, 3 * math.pi / 2 }) do
    local ok, px, pz = pcall(land.getClosestPointOnRoads, 'roads', start.x + math.cos(angle) * length, start.z + math.sin(angle) * length)
    if ok and px and pz and (px - start.x) ^ 2 + (pz - start.z) ^ 2 >= (length / 2) ^ 2 then
      return { x = px, z = pz }
    end
  end
  return nil
end

-- Add `n` ground units of shape `shape`. `o` holds: label (default `r`),
-- kind (default M-113), speed (m/s, 8), length (m, 4000), step (m, 50),
-- lane_m (100), lanes_per_block (60), block_m (6000), unit_gap_m (15),
-- formations (a table of formation name to action) and country. Returns a
-- summary: shape, groups (names), units added, per, route_points, and when
-- the shape cannot be laid out `skipped` with the reason. `seconds` is the
-- time the adds took.
function R.spawn(shape, n, o)
  o = o or {}
  local cfg = CONFIG[shape]
  if not cfg then
    error('routes.spawn: unknown shape ' .. tostring(shape), 2)
  end
  local label = o.label or 'r'
  local speed, length, step = o.speed or 8, o.length or 4000, o.step or 50
  local lane_m, per_block, block_m, gap = o.lane_m or 100, o.lanes_per_block or 60, o.block_m or 6000, o.unit_gap_m or 15
  local formations = {}
  for k, v in pairs(R.FORMATIONS) do
    formations[k] = v
  end
  for k, v in pairs(o.formations or {}) do
    formations[k] = v
  end
  local origin = S.origin()
  local per = cfg.per
  local groups_n = math.ceil(n / per)
  local out = { shape = shape, groups = {}, units = 0, per = per, route = cfg.route }
  local roads, road_error
  if cfg.route == 'road' then
    roads, road_error = R.road_points(groups_n, origin)
    out.road_points = #roads
    if #roads == 0 then
      out.skipped = road_error and ('land.getClosestPointOnRoads: ' .. road_error) or 'no road within 30 km of the origin'
      return out
    end
  end
  if cfg.route == 'dense' then
    out.route_points = R.dense_points(length, step)
  elseif cfg.route == 'few' or cfg.route == 'road' then
    out.route_points = 2
  elseif cfg.route == 'zero' then
    out.route_points = 1
  else
    out.route_points = 1
  end
  local t0 = L.clock() or 0
  local added = 0
  for g = 1, groups_n do
    local count = math.min(per, n - added)
    local lane = R.lane
    R.lane = lane + 1
    local x0 = origin.x + 2000 - math.floor(lane / per_block) * block_m
    local z0 = origin.z + 2000 + (lane % per_block) * lane_m
    local route
    if cfg.route == 'few' or cfg.route == 'dense' then
      route = R.path(x0, z0, length, speed, cfg.route == 'dense', step, nil)
      if cfg.formation then
        route[1].action = formations[cfg.formation]
      end
    elseif cfg.route == 'zero' then
      route = { { x = x0, z = z0, speed = 0 } }
    elseif cfg.route == 'road' then
      local start = roads[(g - 1) % #roads + 1]
      local finish = road_end(start, length)
      if finish then
        x0, z0 = start.x, start.z
        route = { { x = start.x, z = start.z, speed = speed, action = 'On Road' },
          { x = finish.x, z = finish.z, speed = speed, action = 'On Road' } }
      end
    end
    if cfg.route == 'road' and not route then
      out.road_without_end = (out.road_without_end or 0) + 1
    else
      local units = {}
      for i = 1, count do
        units[i] = { name = string.format('np-%s-u%d-%d', label, g, i), x = x0 - (i - 1) * gap, z = z0 }
      end
      local name = string.format('np-%s-g%d', label, g)
      local group, err = X.add_ground(name, o.kind or 'M-113', units, { route = route, country = o.country })
      if group then
        out.groups[#out.groups + 1] = name
        out.units = out.units + count
        if cfg.hold then
          local ok, herr = pcall(function()
            group:getController():setTask({ id = 'Hold', params = {} })
          end)
          if not ok then
            out.hold_error = tostring(herr)
          end
        end
      else
        out.add_errors = (out.add_errors or 0) + 1
        out.first_add_error = out.first_add_error or tostring(err)
      end
    end
    added = added + count
  end
  out.seconds = (L.clock() or 0) - t0
  if cfg.route == 'road' and out.units == 0 then
    out.skipped = 'no road point has a road route of ' .. length .. ' m'
  end
  return out
end

-- Remove everything the scenario library added, and start the lanes again.
-- Returns the number of groups removed.
function R.clear()
  R.lane = 0
  local groups = S.clear()
  return groups
end

return R
