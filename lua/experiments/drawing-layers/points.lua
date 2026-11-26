-- Drawing layers: how many points a freeform shape (7) takes. Each size is added,
-- read back through world.getMarkPanels and removed at once. A raise, or no panel,
-- marks the size refused. Lua 5.1's unpack stops near 8,000 values, which bounds
-- what the shim can pass.
-- states: server
-- timeout: 60

local E = ...

-- IDs from 900,000,000, below the platform pool (2^30 to 2^31 - 1).
local B = 900000000 + 500
local made = {}
E.track.defer('drawing-layers points', function()
  for id in pairs(made) do
    pcall(trigger.action.removeMark, id)
  end
end)

local function has_panel(id)
  local ok, panels = pcall(world.getMarkPanels)
  if not ok or type(panels) ~= 'table' then
    return nil
  end
  for _, panel in ipairs(panels) do
    if (panel.idx or panel.id) == id then
      return true
    end
  end
  return false
end

local sizes = { 3, 16, 64, 128, 256, 512, 1024, 2048, 4096, 7000, 7990 }
local out = {}
for k, n in ipairs(sizes) do
  local args = {}
  for i = 1, n do
    local a = 2 * math.pi * i / n
    args[i] = { x = 30000 + 5000 * math.cos(a), y = 0, z = 5000 * math.sin(a) }
  end
  for _, v in ipairs({ { 1, 0, 0, 1 }, { 1, 0, 0, 0.2 }, 1, true, '' }) do
    args[#args + 1] = v
  end
  local id = B + k
  local c0 = os.clock()
  local ok, err = pcall(function()
    trigger.action.markupToAll(7, -1, id, unpack(args))
  end)
  local seconds = os.clock() - c0
  made[id] = true
  local panel = has_panel(id)
  pcall(trigger.action.removeMark, id)
  made[id] = nil
  out[#out + 1] = { points = n, ok = ok, error = not ok and tostring(err) or nil, panel = panel,
    cpu_ms = math.floor(seconds * 1e6) / 1e3 }
end
return out
