-- The shim, run in the server state by the hook (lua/agent/hook.lua) with the
-- agent's table, the install root, its own version folder, and the shim it
-- replaces, or nil. It returns the shim's table, which the hook keeps in the
-- global __nanoprobe_shim.
--
-- The server state outlives missions, so the shim loads once per process.
-- The hook calls start() on every simulation start; a mission load clears
-- what a mission registered, and start() is where the shim registers it
-- again. A reload runs a newer shim.lua over this one without restarting
-- DCS: the new shim gets this one's table and carries its state on. The DLL
-- stays the one loaded.
--
-- Nothing here may raise into DCS: the hook runs it under net.dostring_in,
-- which catches a raise, and every call it makes into DCS is under pcall.

local agent, root, dir, previous = ...

-- The agent's interface version this shim was written for.
local ABI = '2'
if type(agent) ~= 'table' or agent.abi ~= ABI then
  error('the shim needs agent ABI ' .. ABI .. '; the DLL has '
    .. tostring(type(agent) == 'table' and agent.abi or agent), 0)
end

local logmodule = assert(loadfile(dir .. 'lua/log.lua'))()

local M = {
  agent = agent,
  -- Simulation starts this process has seen, across reloads of the shim.
  starts = previous and previous.starts or 0,
  -- Reloads of the shim in this process.
  reloads = previous and (previous.reloads or 0) + 1 or 0,
  dir = dir,
}

M.file = logmodule.open({
  path = root .. 'logs/shim.log',
  target = 'shim',
  model = timer.getTime,
  gen = function()
    return agent.status().gen
  end,
  size = function(path)
    return lfs.attributes(path, 'size')
  end,
  fallback = function(message)
    pcall(env.error, 'nanoprobe shim: ' .. message)
  end,
  forward = function(level, msg, data)
    local args = {}
    for k, v in pairs(data or {}) do
      args[#args + 1] = tostring(k)
      args[#args + 1] = v
    end
    agent.log(level, msg, 'shim', unpack(args))
  end,
})

if previous then
  if previous.file then
    pcall(previous.file.close)
  end
  M.file.write('info', 'shim reloaded', { dir = dir, reloads = M.reloads })
else
  M.file.write('info', 'shim loaded', { dir = dir })
end

-- What the platform calls in the server state (spec/records.md,
-- "Capabilities"). Later steps call more of it; the probe reports each name
-- as present or absent, so a DCS update that removes one is named.
local CAPABILITIES = {
  'timer.getTime',
  'world.addEventHandler',
  'world.removeEventHandler',
  'coalition.addGroup',
  'coalition.addStaticObject',
  'Group.getByName',
  'Unit.getByName',
  'StaticObject.getByName',
  'Airbase.getByName',
  'Object.getPoint',
  'Object.getPosition',
  'Unit.getLife',
  'Unit.getFuel',
  'Unit.getAmmo',
  'Unit.getObjectID',
  'land.getHeight',
  'trigger.action.explosion',
  'trigger.action.outText',
  'trigger.action.markToAll',
  'missionCommands.addCommand',
  'env.mission',
  'env.error',
  'lfs.attributes',
  'package.loadlib',
}

-- Whether each dotted name is present in this state, without raising for a
-- name that is absent.
local function probe(names)
  local found = {}
  for _, name in ipairs(names) do
    local ok, present = pcall(function()
      local v = _G
      for part in name:gmatch('[^.]+') do
        if type(v) ~= 'table' then
          return false
        end
        v = v[part]
      end
      return v ~= nil
    end)
    found[name] = ok and present or false
  end
  return found
end

-- Called by the hook on every simulation start, and after a reload. Returns
-- a short summary for the hook's log.
function M.start()
  M.starts = M.starts + 1
  local ok, err = pcall(function()
    if M.starts == 1 or not M.probed then
      M.file.write('info', 'capabilities', probe(CAPABILITIES))
      M.probed = true
    end
  end)
  if not ok then
    M.file.write('error', 'start raised', { error = tostring(err) })
  end
  return 'start ' .. M.starts .. ' reloads ' .. M.reloads
end

return M
