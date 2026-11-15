-- Which index does `missionCommands.doAction` take for a command added with
-- `missionCommands.addCommand`? Run on a server with no players, in the server
-- state. It adds one command with a counting callback, calls `doAction(i)` for
-- i = -2 to 300 under `pcall`, and records which indices ran the callback. It
-- also calls `doAction` with a string and records the error text. It removes
-- the item and its global, and returns the list of indices that hit.
--   cargo xtask eval <target> server lua/experiments/s16/e-doaction-index.lua
-- states: server
-- timeout: 60

local E = ...
local HITS = '__np_s16_doaction_hits'

local current
rawset(_G, HITS, {})
local path = missionCommands.addCommand('np-s16-index', nil, function()
  local hits = rawget(_G, HITS)
  if hits then
    hits[#hits + 1] = current
  end
end)

local errors = 0
for i = -2, 300 do
  current = i
  if not pcall(missionCommands.doAction, i) then
    errors = errors + 1
  end
end

local hit = {}
for _, i in ipairs(rawget(_G, HITS) or {}) do
  hit[#hit + 1] = i
end

local ok, err = pcall(missionCommands.doAction, 'np-s16-index')
E.observe('string_argument_raised', not ok, { info = true })
E.observe('string_argument_error', tostring(err), { info = true })
E.observe('indices_that_raised', errors, { info = true })
E.observe('indices_that_hit', hit, { info = true })

pcall(missionCommands.removeItem, path)
rawset(_G, HITS, nil)
return hit
