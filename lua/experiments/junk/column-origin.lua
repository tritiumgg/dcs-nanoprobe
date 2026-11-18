-- The clear-debris check's origin: the first airbase whose ground is at least
-- __MIN_GROUND__ m above sea level. Returns `x z ground name`, or `none` with
-- the highest ground found. The caller replaces the placeholder.
-- states: server
-- timeout: 60

local MIN = __MIN_GROUND__
local highest = -math.huge
for _, airbase in ipairs(world.getAirbases()) do
  local ok, p = pcall(airbase.getPoint, airbase)
  if ok and type(p) == 'table' then
    local read, ground = pcall(land.getHeight, { x = p.x, y = p.z })
    if read and type(ground) == 'number' then
      if ground >= MIN then
        local named, name = pcall(airbase.getName, airbase)
        return string.format('%.1f %.1f %.1f %s', p.x, p.z, ground, named and tostring(name) or '?')
      end
      if ground > highest then
        highest = ground
      end
    end
  end
end
return string.format('none %.1f', highest)
