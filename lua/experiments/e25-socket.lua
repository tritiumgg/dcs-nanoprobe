-- E25: the cost of one non-blocking send and one recv per frame inside DCS,
-- on a loopback TCP pair the agent holds, at the batch sizes the stream
-- would carry. Size 0 is an idle frame: a send of nothing and a receive
-- that would block. Recorded; it feeds E19 (p99 at most 10 us per frame).
-- states: hook
-- timeout: 60

local E = ...
local A = E.lib('agent')
local agent = A.open()

assert(agent.sock_open())
local SIZES = { 0, 64, 512, 4096, 16384 }
local FRAMES = 600
local size_index, frame = 1, 0
local send, recv, total, blocked, empty = {}, {}, {}, 0, 0

local function us(list)
  local s = A.stats(list)
  return { n = s.n, p50 = s.p50 / 1000, p90 = s.p90 / 1000, p99 = s.p99 / 1000, max = s.max / 1000 }
end

return E.wait(function()
  local send_ns, recv_ns, sent, got = agent.sock_frame(SIZES[size_index])
  if got < 0 then
    empty = empty + 1
  end
  send[#send + 1], recv[#recv + 1], total[#total + 1] = send_ns, recv_ns, send_ns + recv_ns
  if sent < 0 then
    blocked = blocked + 1
  end
  frame = frame + 1
  if frame < FRAMES then
    return nil
  end
  local label = 'bytes' .. SIZES[size_index]
  E.observe(label .. '.send_us', us(send), { timing = true })
  E.observe(label .. '.recv_us', us(recv), { timing = true })
  E.observe(label .. '.frame_us', us(total), { timing = true })
  E.observe(label .. '.send_would_block', blocked, { info = true })
  E.observe(label .. '.recv_would_block', empty, { info = true })
  send, recv, total, blocked, empty, frame = {}, {}, {}, 0, 0, 0
  size_index = size_index + 1
  if SIZES[size_index] then
    return nil
  end
  agent.sock_close()
  return 'done'
end, 50)
