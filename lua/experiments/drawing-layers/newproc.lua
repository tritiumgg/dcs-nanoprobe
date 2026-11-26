-- Drawing layers: does a new DCS process free an ID the last one removed? Step `before`
-- adds a text under ID 900,011,001 and removes it, so this process never draws that ID
-- again (LIVE, reid.lua). Restart DCS. Step `after` adds a text under the same ID and
-- reads whether the server lists its panel, then removes it. Run with
-- `cargo xtask eval <server> server newproc.lua` after putting the step in for <step>.
-- states: server
-- timeout: 30

local STEP = '<step>'
local ID = 900011001
local at = world.getAirbases()[1]:getPoint()
local function add()
  return pcall(trigger.action.textToAll, -1, ID, { x = at.x + 1000, y = 0, z = at.z }, { 0, 0, 0, 1 },
    { 1, 1, 1, 0.6 }, 14, true, 'newproc')
end
local function panel()
  for _, m in ipairs(world.getMarkPanels()) do
    if (m.idx or m.id) == ID then
      return true
    end
  end
  return false
end
local out = { step = STEP, id = ID }
if STEP == 'before' then
  out.added = add()
  out.panel_after_add = panel()
  out.removed = pcall(trigger.action.removeMark, ID)
  out.panel_after_remove = panel()
  out.readded = add()
  out.panel_after_readd = panel()
  pcall(trigger.action.removeMark, ID)
elseif STEP == 'after' then
  out.added = add()
  out.panel = panel()
  out.removed = pcall(trigger.action.removeMark, ID)
  out.panel_after_remove = panel()
else
  error('no step ' .. STEP)
end
return out
