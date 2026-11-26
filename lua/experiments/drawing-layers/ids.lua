-- Drawing layers: the highest DCS ID a drawing and a mark take. Each ID is added,
-- looked up in world.getMarkPanels by exact ID, removed, and looked up again.
-- A raise, no panel, or a panel left after the remove marks the ID unusable.
-- states: server
-- timeout: 60

local E = ...

local ids = { 2 ^ 31 - 2, 2 ^ 31 - 1, 2 ^ 31, 2 ^ 32 - 1, 2 ^ 32, 2 ^ 40, 2 ^ 52, 2 ^ 53 - 1 }
local made = {}
E.track.defer('drawing-layers ids', function()
  for id in pairs(made) do
    pcall(trigger.action.removeMark, id)
  end
end)

local function find(id)
  local ok, panels = pcall(world.getMarkPanels)
  if not ok or type(panels) ~= 'table' then
    return nil
  end
  local exact, near = false, nil
  for _, panel in ipairs(panels) do
    local idx = panel.idx or panel.id
    if idx == id then
      exact = true
    elseif type(idx) == 'number' and idx >= 2 ^ 30 + 2 ^ 29 then
      near = string.format('%.0f', idx)
    end
  end
  return { exact = exact, other = near }
end

local out = {}
for _, id in ipairs(ids) do
  local row = { id = string.format('%.0f', id) }
  -- Shape 3 with two points: a line on a client (LIVE, run 3 part B).
  local a ={ x = 40000, y = 0, z = 0 }
  local b = { x = 41000, y = 0, z = 1000 }
  local ok, err = pcall(trigger.action.markupToAll, 3, -1, id, a, b, { 1, 0, 0, 1 }, { 1, 0, 0, 0.3 }, 1, true, '')
  made[id] = true
  row.drawing = { ok = ok, error = not ok and tostring(err) or nil, added = find(id) }
  pcall(trigger.action.removeMark, id)
  row.drawing.removed = find(id)
  local mark_ok, mark_err = pcall(trigger.action.markToAll, id, 'np id', a, true)
  row.mark = { ok = mark_ok, error = not mark_ok and tostring(mark_err) or nil, added = find(id) }
  pcall(trigger.action.removeMark, id)
  made[id] = nil
  row.mark.removed = find(id)
  out[#out + 1] = row
end
return out
