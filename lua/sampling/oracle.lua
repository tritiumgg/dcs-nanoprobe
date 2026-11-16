-- The mirror oracle of step S10: every unit and static in the mission, read
-- by brute force in one frame, for `cargo xtask sampling compare` to set
-- against the sidecar's mirror (spec/sampling.md).
-- states: server
-- timeout: 30
--
-- Returns { model = model time, chunks = { { row, ... }, ... } }, one row
-- per object that exists, in chunks of 1,000 rows because the eval channel
-- encodes at most 2,000 entries of a table (spec/eval.md): its object ID,
-- name, kind (1 unit, 2 static), position, speed, top speed (`getDesc().speedMax`),
-- life; and `cargo` for a static of the cargo category, which can move.
-- A getter that raises leaves its object out and counts it in `failed`.
-- After an early read (lua/sampling/oracle-early.lua) a unit's or cargo
-- static's row also holds where it stood then and how fast it moved (`ex`,
-- `ey`, `ez`, `es`), and the reply the early read's model time.

local E = ...
local rows, failed = {}, 0
local early = E.store and E.store.s10_early
local was = early and early.rows or {}

-- Add the early read's figures for the object to its row.
local function with_early(row)
  local e = was[row.id]
  if e then
    row.ex, row.ey, row.ez, row.es = e[1], e[2], e[3], e[4]
  end
  return row
end

local function unit_row(u)
  if not u:isExist() then
    return
  end
  local p, v = u:getPoint(), u:getVelocity()
  local desc = u:getDesc()
  rows[#rows + 1] = with_early({
    id = u.id_,
    name = u:getName(),
    kind = 1,
    x = p.x,
    y = p.y,
    z = p.z,
    speed = math.sqrt(v.x * v.x + v.y * v.y + v.z * v.z),
    top = type(desc) == 'table' and tonumber(desc.speedMax) or 0,
    life = u:getLife(),
  })
end

local function static_row(s)
  if not s:isExist() then
    return
  end
  local p = s:getPoint()
  rows[#rows + 1] = with_early({
    id = s.id_,
    name = s:getName(),
    kind = 2,
    x = p.x,
    y = p.y,
    z = p.z,
    speed = 0,
    top = 0,
    life = s:getLife(),
    cargo = Object.getCategory(s) == 6,
  })
end

local model = timer.getTime()
for side = 0, 2 do
  for _, group in ipairs(coalition.getGroups(side) or {}) do
    for _, unit in ipairs(group:getUnits() or {}) do
      if not pcall(unit_row, unit) then
        failed = failed + 1
      end
    end
  end
  for _, static in ipairs(coalition.getStaticObjects(side) or {}) do
    if not pcall(static_row, static) then
      failed = failed + 1
    end
  end
end
E.observe('rows', #rows, { info = true })
local chunks = {}
for i, row in ipairs(rows) do
  local c = math.floor((i - 1) / 1000) + 1
  chunks[c] = chunks[c] or {}
  chunks[c][#chunks[c] + 1] = row
end
if early then
  E.store.s10_early = nil
end
return { model = model, chunks = chunks, rows_n = #rows, failed = failed, early_model = early and early.model }
