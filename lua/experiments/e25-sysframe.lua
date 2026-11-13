-- E25, the system-call frame: the cost inside DCS of the calls the pump makes
-- once a second with no sidecar connected: an accept with nothing waiting,
-- the kill-switch and reload checks, and the log flush with one record held
-- and with none. With a sidecar connected a recv replaces the accept;
-- e25-socket measures it at size 0. Recorded; it feeds S11's frame budget.
-- states: hook
-- timeout: 30

local E = ...
local A = E.lib('agent')
local agent = A.open()

assert(agent.sys_open())
local FRAMES = 600
local NAMES = { 'accept', 'kill_switch', 'reload', 'flush_one', 'flush_none' }
local lists, idle, logged = {}, {}, {}
for i = 1, #NAMES do
  lists[i] = {}
end
local frame, accepted = 0, 0

local function us(list)
  local s = A.stats(list)
  return { n = s.n, p50 = s.p50 / 1000, p90 = s.p90 / 1000, p99 = s.p99 / 1000, max = s.max / 1000 }
end

return E.wait(function()
  local v = { agent.sys_frame() }
  if v[1] == nil then
    agent.sys_close()
    error('sys_frame: ' .. tostring(v[2]))
  end
  for i = 1, #NAMES do
    lists[i][#lists[i] + 1] = v[i]
  end
  -- A frame as the pump makes it with nothing held for the log, and with
  -- one record held.
  idle[#idle + 1] = v[1] + v[2] + v[3] + v[5]
  logged[#logged + 1] = v[1] + v[2] + v[3] + v[4]
  if v[6] ~= -1 then
    accepted = accepted + 1
  end
  frame = frame + 1
  if frame < FRAMES then
    return nil
  end
  for i = 1, #NAMES do
    E.observe(NAMES[i] .. '_us', us(lists[i]), { timing = true })
  end
  E.observe('frame_idle_us', us(idle), { timing = true })
  E.observe('frame_with_log_us', us(logged), { timing = true })
  E.observe('accept_did_not_block', accepted, { info = true })
  agent.sys_close()
  return 'done'
end, 50)
