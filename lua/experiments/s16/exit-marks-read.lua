-- The S16 exit test: the marks and drawings the shim holds, read back from the
-- server state with `world.getMarkPanels()`. The shim's own mark IDs start at
-- 2^30 (spec/commands.md, "Marks, drawings and menu items in DCS"), so the
-- panels from there up are the ones the agent put on the map. Returns
-- `<count>|<id>=<text>,...` with the panels' texts, ordered by ID, and
-- `panels=<n>` as a third part: every panel DCS lists, so a read that finds
-- none of the agent's can be told from a call that lists nothing. The shape of
-- a panel is UNVERIFIED: the id comes from `id`, `idx` or `markId`, the text
-- from `text` or `message`.
-- states: server
-- timeout: 30

local E = ...
local FIRST = 2 ^ 30
local panels = world.getMarkPanels()
if type(panels) ~= 'table' then
  error('world.getMarkPanels returned ' .. type(panels))
end
local mine, all = {}, 0
for _, panel in pairs(panels) do
  all = all + 1
  if type(panel) == 'table' then
    local id = tonumber(panel.id or panel.idx or panel.markId)
    local text = panel.text or panel.message or ''
    if id and id >= FIRST then
      mine[#mine + 1] = { id = id, text = tostring(text) }
    end
  end
end
table.sort(mine, function(a, b)
  return a.id < b.id
end)
local parts = {}
for i, m in ipairs(mine) do
  parts[i] = string.format('%d=%s', m.id, (m.text:gsub('[|,]', '_')))
end
return string.format('%d|%s|panels=%d', #mine, table.concat(parts, ','), all)
