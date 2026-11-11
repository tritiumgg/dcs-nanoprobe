-- E3: the OS thread each kind of call into the agent runs on. The rule:
-- one thread, or the agent design is revised.
-- states: hook
-- timeout: 60

local E = ...
local A = E.lib('agent')
local agent = A.open()

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    error(state .. ': ' .. tostring(value))
  end
  return value
end

local threads = {}
threads.hook_snippet = agent.thread()

-- The thread the hook opened the agent on is in hook.log's "agent loaded"
-- record. DCS's io.open cannot read hook.log while the hook holds it open
-- (LIVE), so the bundle carries that value.

threads.server_call = tonumber(cross('server', "return E.lib('agent').open().thread()"))
threads.gui_call = tonumber(cross('gui', "return E.lib('agent').open().thread()"))
local export_ok, export = E.cross('export', "return E.lib('agent').open().thread()")
threads.export_call = export_ok and tonumber(export) or nil
E.observe('export_opened', export_ok, { info = true })

-- An event handler and a timer in the server state. Spawning two units fires
-- S_EVENT_BIRTH at once; the timer needs model time.
cross('server', [[
  local agent = E.lib('agent').open()
  E.track.global('np_e3', {})
  E.track.handler({ onEvent = function(_, event)
    if np_e3.event == nil then
      np_e3.event = agent.thread()
    end
  end })
  E.track.timer(function()
    np_e3.timer = agent.thread()
  end, nil, timer.getTime() + 0.5)
  E.lib('scenario').spawn_grid(2, { label = 'e3' })
  return 'armed'
]])

return E.wait(function()
  threads.hook_frame = threads.hook_frame or agent.thread()
  local timer_thread = tonumber(cross('server', 'return np_e3.timer'))
  if not timer_thread then
    return nil
  end
  threads.server_timer = timer_thread
  threads.server_event = tonumber(cross('server', 'return np_e3.event'))
  cross('server', "E.lib('scenario').clear() return 'cleared'")

  local first, same = nil, true
  for name, id in pairs(threads) do
    E.observe('thread.' .. name, id, { info = true })
    first = first or id
    same = same and id == first
  end
  E.observe('one_thread', same, { windows = true })
  return 'done'
end, 50, 'running')
