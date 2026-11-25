-- FARP groups, run 2, mission paused: does a one-unit `addGroup(-1)` replace a
-- live group under its first name, and a group a one-unit one? Adds, east of
-- the first airdrome, a 2-FARP group `np-fg-o2` and a one-FARP group
-- `np-fg-o1`, then spawns a one-FARP group under `np-fg-o2-1` and a 2-FARP
-- group under `np-fg-o1-1`/`-2`, and reads every name at once. The same again
-- under `np-fg-q2`/`np-fg-q1` is left for `one-run.lua` to replace while the
-- mission runs; `one-after.lua` reads.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then
    return v
  end
  return 'raised: ' .. tostring(v)
end
local anchor
for _, base in ipairs(world.getAirbases()) do
  local ok, d = pcall(base.getDesc, base)
  if not anchor and ok and type(d) == 'table' and d.category == 0 then
    anchor = base
  end
end
if not anchor then
  error('no airdrome')
end
local p = anchor:getPoint()
local G = { rows = { o2 = 1, o1 = 2, q2 = 3, q1 = 4 } }
_G.np_farp_one = G

function G.group(tag, n)
  local units = {}
  for k = 1, n do
    units[k] = { category = 'Heliports', type = 'FARP', shape_name = 'FARPS', name = 'np-fg-' .. tag .. '-' .. k,
      x = p.x + 200 * (k - 1), y = p.z + 3000 + 600 * G.rows[tag], heading = 0, rate = 100 }
  end
  return try(coalition.addGroup, country.id.USA, -1, { name = 'np-fg-' .. tag, units = units })
end

function G.read(tag)
  local r = {}
  for k = 1, 2 do
    local name = 'np-fg-' .. tag .. '-' .. k
    local st = try(StaticObject.getByName, name)
    local ab = try(Airbase.getByName, name)
    r[name] = {
      static = type(st) == 'table' and { id = rawget(st, 'id_'), exists = try(st.isExist, st) } or tostring(st),
      airbase = type(ab) == 'table' and rawget(ab, 'id_') or tostring(ab),
      parking = type(ab) == 'table' and #(try(ab.getParking, ab) or {}) or nil,
    }
  end
  return r
end

-- Replace `tag`'s group with one of `n` units; the old airbase's handle is
-- kept to read its `isExist` later.
function G.replace(tag, n)
  local old = StaticObject.getByName('np-fg-' .. tag .. '-1')
  G.old = G.old or {}
  G.old[tag] = old
  local made = G.group(tag, n)
  local r = G.read(tag)
  r.new = type(made) == 'table' and rawget(made, 'id_') or tostring(made)
  r.old = old and rawget(old, 'id_')
  r.old_exists = old and try(old.isExist, old)
  return r
end

local out = { before = #world.getAirbases() }
for tag, n in pairs({ o2 = 2, o1 = 1, q2 = 2, q1 = 1 }) do
  local made = G.group(tag, n)
  out[tag .. '_made'] = type(made) == 'table' and { id = rawget(made, 'id_'), category = try(Object.getCategory, made) }
    or tostring(made)
end
out.o2 = G.replace('o2', 1)
out.o1 = G.replace('o1', 2)
out.after = #world.getAirbases()
return out
