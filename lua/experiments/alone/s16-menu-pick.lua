-- S16: select an F10 menu item from the scripted client, two routes.
-- Run with `cargo xtask eval <client-target> hook` on a client that is in its
-- aircraft in a mission whose menu holds a command item (the S16 exit test's
-- `np-s16` submenu, or any mission command).
--
-- Route 1, as a player does: read the numbers of the input commands at run
-- time from the gui state (`Input.getEnvTable().Actions`: iCommandToggleCommandMenu,
-- ICommandMenuItem10, ICommandMenuItem1; ED-CODE,
-- Scripts/Input/Data.lua:410) and send them with `Export.LoSetCommand`, with a
-- delay between: open the communication menu, F10 (Other), then F1 (the first
-- item there). No number is written in this file. The caller says which item
-- is first.
--
-- Route 2, the index fallback ED's own panel uses
-- (Scripts/UI/RadioCommandDialogPanel/RadioCommandDialogsPanel.lua calls
-- `missionCommands.doAction(actionIndex)`): reads what the gui state offers
-- (which of `missionCommands`, `RadioCommandDialogsPanel` exist and any menu
-- tree a reader function returns) and, when ROUTE2_INDEX is set, calls
-- doAction(index) in the gui state. DCS's numbering is UNVERIFIED; edit the
-- constant to try an index.
--
-- Every step is recorded; a step that cannot run says why. The snippet changes
-- no setting and writes no file.
-- states: hook
-- timeout: 60

local E = ...
local ROUTE2_INDEX = nil
local DELAY = 0.6

local function gui(code)
  local text, ran = net.dostring_in('gui', code)
  return text, ran
end

-- Route 1: read the numbers.
local ACTIONS = gui([[
local env = Input.getEnvTable()
local a = env and env.Actions
if type(a) ~= 'table' then return 'no Actions: ' .. type(a) end
local function n(k) return tostring(a[k]) end
return n('iCommandToggleCommandMenu') .. '|' .. n('ICommandMenuItem10') .. '|' .. n('ICommandMenuItem1')
]])
E.observe('actions_raw', ACTIONS, { info = true })
local toggle, other, first = tostring(ACTIONS):match('^([^|]+)|([^|]+)|([^|]+)$')
local numbers = { tonumber(toggle), tonumber(other), tonumber(first) }
if not (numbers[1] and numbers[2] and numbers[3]) then
  local hint = gui([[
local env = Input.getEnvTable()
local out = {}
for k in pairs(env and env.Actions or {}) do
  if type(k) == 'string' and k:lower():find('menu', 1, true) and #out < 30 then out[#out + 1] = k end
end
table.sort(out)
return table.concat(out, ',')
]])
  E.observe('route1', 'the numbers were not found', { info = true })
  E.observe('menu_action_names', hint, { info = true })
else
  E.observe('route1_numbers', { toggle = numbers[1], item10 = numbers[2], item1 = numbers[3] }, { info = true })
  E.observe('has_LoSetCommand', type(Export) == 'table' and type(Export.LoSetCommand) == 'function', { info = true })
end

-- Route 2: what the gui state offers.
E.observe('route2_gui', gui([[
local function t(name) return name .. '=' .. type(rawget(_G, name)) end
local out = { t('missionCommands'), t('RadioCommandDialogsPanel'), t('radioMenu'), t('RadioCommandDialogPanel') }
local mc = rawget(_G, 'missionCommands')
if type(mc) == 'table' then
  for _, name in ipairs({ 'getMenu', 'getCommands', 'getItems', 'getTree' }) do
    out[#out + 1] = name .. '=' .. type(mc[name])
  end
end
return table.concat(out, ' ')
]]), { info = true })
E.observe('route2_tree', gui([[
local mc = rawget(_G, 'missionCommands')
if type(mc) ~= 'table' then return 'no missionCommands in gui' end
local function dump(v, depth)
  if type(v) ~= 'table' or depth > 3 then return tostring(v) end
  local parts = {}
  for k, x in pairs(v) do
    if #parts < 20 then parts[#parts + 1] = tostring(k) .. '=' .. dump(x, depth + 1) end
  end
  return '{' .. table.concat(parts, ',') .. '}'
end
for _, name in ipairs({ 'getMenu', 'getCommands', 'getItems', 'getTree' }) do
  if type(mc[name]) == 'function' then
    local ok, value = pcall(mc[name])
    return name .. ' ' .. (ok and dump(value, 0) or ('raised ' .. tostring(value)))
  end
end
return 'no reader function'
]]), { info = true })
if ROUTE2_INDEX then
  E.observe('route2_doAction', gui('return tostring(pcall(missionCommands.doAction, ' .. tonumber(ROUTE2_INDEX) .. '))'),
    { info = true })
end

if not (numbers[1] and numbers[2] and numbers[3]) then
  return 'route 1 not run'
end

-- Route 1: send the three commands with a delay between.
local steps = { { 'toggle', numbers[1] }, { 'item10', numbers[2] }, { 'item1', numbers[3] } }
local at, index = DCS.getRealTime(), 0
local sent = {}
return E.wait(function()
  if DCS.getRealTime() < at then
    return nil
  end
  index = index + 1
  local step = steps[index]
  if not step then
    E.observe('route1_sent', sent, { info = true })
    return 'done'
  end
  local ok, err = pcall(Export.LoSetCommand, step[2])
  sent[#sent + 1] = step[1] .. ' ' .. tostring(step[2]) .. ' ' .. (ok and 'sent' or ('raised ' .. tostring(err)))
  at = DCS.getRealTime() + DELAY
  return nil
end, 20)
