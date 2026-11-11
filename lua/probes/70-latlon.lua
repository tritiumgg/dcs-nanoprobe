-- terrain.convertMetersToLatLon in the hook state against coord.LOtoLL in
-- the server state, at three points.
-- states: hook

local E = ...
local ok, text = E.cross('server', [[
  local out = {}
  for i, base in ipairs(world.getAirbases()) do
    if i > 3 then
      break
    end
    local p = base:getPoint()
    local lat, lon = coord.LOtoLL({ x = p.x, y = 0, z = p.z })
    out[i] = string.format('%.3f,%.3f,%.12f,%.12f', p.x, p.z, lat, lon)
  end
  return table.concat(out, ';')
]])
if not ok then
  error(text)
end

local worst, points = 0, 0
for x, z, lat, lon in text:gmatch('([^,;]+),([^,;]+),([^,;]+),([^,;]+)') do
  local here_lat, here_lon = terrain.convertMetersToLatLon(tonumber(x), tonumber(z))
  worst = math.max(worst, math.abs(here_lat - tonumber(lat)), math.abs(here_lon - tonumber(lon)))
  points = points + 1
end
E.observe('points', points, { info = true })
-- The two conversions agree (PRIOR).
E.observe('largest_difference_deg', worst, { windows = 0, within = 1e-7 })
