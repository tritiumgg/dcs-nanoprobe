-- Open the active version's agent DLL in the snippet's state. Load it with
-- E.lib('agent'); call .open() for the agent's table.
--
-- Development only. Opening is idempotent per state, so a snippet in the
-- hook state gets the table the hook opened, and a state the agent was never
-- opened in gets a new one. Needs io, lfs and package.loadlib in the state:
-- hook, gui, server and export have them.

local M = {}

-- The install root, the active build, and the DLL's path.
function M.paths()
  local root = lfs.writedir() .. 'nanoprobe/'
  local file = assert(io.open(root .. 'manifest.txt', 'rb'), 'no manifest at ' .. root)
  local text = file:read('*a')
  file:close()
  local build = assert(text:match('active=(%w+)'), 'no active build in the manifest')
  return root, build, root .. 'versions/' .. build .. '/nanoprobe_agent_' .. build .. '.dll'
end

-- The agent's table in this state, configured with the install root.
function M.open()
  local root, _, path = M.paths()
  local loader, err = package.loadlib(path, 'luaopen_nanoprobe')
  if not loader then
    error('agent: ' .. tostring(err), 2)
  end
  local agent = loader()
  if type(agent) ~= 'table' then
    error('agent: the opener returned a ' .. type(agent), 2)
  end
  -- A disabled agent returns nothing from configure; status() still answers.
  local configured, err = agent.configure(root)
  if configured == nil and err ~= nil then
    error('agent: ' .. tostring(err), 2)
  end
  return agent
end

-- Percentiles of a list of numbers: a table with p50, p90, p99, max, min,
-- mean and n.
function M.stats(list)
  local sorted = {}
  local sum = 0
  for i = 1, #list do
    sorted[i] = list[i]
    sum = sum + list[i]
  end
  table.sort(sorted)
  local n = #sorted
  if n == 0 then
    return { n = 0 }
  end
  local function at(p)
    return sorted[math.max(1, math.ceil(n * p))]
  end
  return { n = n, min = sorted[1], p50 = at(0.5), p90 = at(0.9), p99 = at(0.99), max = sorted[n], mean = sum / n }
end

return M
