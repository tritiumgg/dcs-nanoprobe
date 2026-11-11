-- E4: the agent DLL in the server state survives 50 mission reloads: the same
-- table on every open, the same open count, and every call still answers.
-- The clean exit is alone/e04-exit.lua. The rule: 50 reloads and a clean
-- exit.
-- states: hook
-- timeout: 1800

local E = ...
local A = E.lib('agent')
local RELOADS = 50

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    error(state .. ': ' .. tostring(value))
  end
  return value
end

-- One line describing the server state's agent.
local PROBE = [[
  local agent = E.lib('agent').open()
  local status = agent.status()
  return table.concat({ tostring(agent), status.opens, tostring(status.disabled), agent.thread(),
    tostring(agent.status ~= nil) }, ' ')
]]

local file = DCS.getMissionFilename()
if type(file) ~= 'string' then
  error('no mission file to reload')
end

local before = cross('server', PROBE)
local hook_agent = A.open()
local hook_before = tostring(hook_agent)
E.observe('before', before, { info = true })

local done, failures, seq, started_at = 0, {}, nil, nil
local times = {}

return E.wait(function()
  if seq then
    local started = false
    for _, entry in ipairs(E.hook.since(seq)) do
      started = started or entry.name == 'onSimulationStart'
    end
    if not started then
      return nil
    end
    times[#times + 1] = DCS.getRealTime() - started_at
    local after = cross('server', PROBE)
    if after ~= before then
      failures[#failures + 1] = done + 1 .. ': ' .. after
    end
    if tostring(A.open()) ~= hook_before or A.open().thread() == nil then
      failures[#failures + 1] = done + 1 .. ': the hook state agent changed'
    end
    done = done + 1
    seq = nil
  end
  if done < RELOADS and #failures == 0 then
    seq = E.hook.seq()
    started_at = DCS.getRealTime()
    net.load_mission(file)
    return nil
  end
  E.observe('reloads', done, { windows = RELOADS })
  E.observe('failures', failures, { windows = {} })
  local stats = A.stats(times)
  E.observe('load_seconds', { min = stats.min, p50 = stats.p50, max = stats.max }, { info = true })
  E.observe('status', A.open().status(), { info = true })
  return 'reloaded ' .. done
end, 1750)
