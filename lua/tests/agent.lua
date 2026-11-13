-- The agent's functions under a stock Lua 5.1: each one runs, returns what
-- the S4 experiments read, and never lets a Lua error or a panic escape
-- except where it is meant to.
--
-- The deliberate access violation is not called: on the host it is a
-- SIGSEGV. The panic is called last, because it disables the agent.
--
-- Run it through tools/luatest.sh, which builds the module and finds it.

local path = ...
assert(path, 'usage: lua lua/tests/agent.lua <module path>')

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')

local function open(module)
  for _, name in ipairs({ 'luaopen_nanoprobe', '_luaopen_nanoprobe' }) do
    local loader = package.loadlib(module, name)
    if loader then
      return loader()
    end
  end
  error('no opener in ' .. module, 0)
end

local agent = open(path)
assert(type(agent) == 'table', 'the opener returned a ' .. type(agent))
assert(agent.abi == '2', 'abi reads ' .. tostring(agent.abi))
assert(type(agent.build) == 'string' and agent.build ~= '', 'build is missing')

-- status works before configure.
local status = agent.status()
assert(status.disabled == false, 'a fresh agent is disabled')
assert(status.opens == 1, 'status.opens reads ' .. tostring(status.opens))
assert(status.root == nil, 'root is set before configure')

-- configure opens logs/agent.log under the root, and a second call with the
-- same root changes nothing.
local root = S.scratch()
assert(agent.configure(root) == true, 'configure failed')
assert(agent.configure(root) == true, 'a second configure failed')
assert(agent.status().root == root, 'status.root reads ' .. tostring(agent.status().root))
agent.log('info', 'host test record', 'agent', 'n', 3, 'b', true, 's', 'x', 'f', print)
-- log is called inside the pump, so its record waits for the next write of
-- the log: the pump's system-call frame, or the end of a call made outside
-- the pump, such as status.
local held = S.read(root .. 'logs/agent.log') or ''
assert(not held:find('host test record', 1, true), 'log wrote its record at once')
agent.status()
local text = assert(S.read(root .. 'logs/agent.log'), 'no logs/agent.log')
assert(text:find('"data":{"n":3,"b":true,"s":"x","f":null}', 1, true), 'the record data reads: ' .. text)
assert(text:find('"msg":"agent configured"', 1, true), 'the configure record is missing')
assert(text:find('"msg":"host test record"', 1, true), 'the logged record is missing')
assert(text:find('"target":"agent"', 1, true), 'records lack the agent target')
local nil_root, err = agent.configure()
assert(nil_root == nil and type(err) == 'string', 'configure without a root did not fail')

-- thread, clock and the clock's cost.
assert(type(agent.thread()) == 'number' and agent.thread() == agent.thread(), 'thread is not stable')
local t0 = agent.clock()
local t1 = agent.clock()
assert(type(t0) == 'number' and t1 >= t0, 'the clock went backwards')
local ns, frequency = agent.qpc_cost(1000)
assert(ns > 0 and ns < 100000, 'qpc_cost reads ' .. tostring(ns))
assert(frequency > 0, 'frequency reads ' .. tostring(frequency))

-- The breadcrumb.
agent.crumb(42, 'host test')
assert(agent.status().crumb_code == 42, 'the crumb code did not stick')

-- raise: a Lua error from the entry, caught by pcall.
local ok, message = pcall(agent.raise, 'from the entry')
assert(ok == false and message == 'from the entry', 'raise gave ' .. tostring(ok) .. ', ' .. tostring(message))
-- The state is usable afterwards.
assert(agent.thread() == agent.thread(), 'the agent is unusable after a raise')

-- inject: the shipping build has no fault injection, and says so.
local injected, refusal = agent.inject('stall', 10)
assert(injected == nil and refusal == 'this build has no fault injection',
  'inject gave ' .. tostring(injected) .. ', ' .. tostring(refusal))

-- call: lua_pcall from C, on success and on error.
local results = { agent.call(function(a, b)
  return a + b, 'x'
end, 2, 3) }
assert(results[1] == true and results[2] == 5 and results[3] == 'x', 'call returned the wrong values')
local called, why = agent.call(function()
  error('inside', 0)
end)
assert(called == false and why == 'inside', 'call did not catch the error: ' .. tostring(why))
assert(pcall(agent.call, 'not a function') == false, 'call accepted a non-function')

-- The two sampling shapes give the same sum.
agent.sink_take()
local ids = {}
for i = 1, 100 do
  ids[i] = i
end
local function getter(id)
  return id, id * 2, id * 3
end
for i = 1, #ids do
  agent.sink(getter(ids[i]))
end
local count_lua, sum_lua = agent.sink_take()
local rows, errors = agent.csweep(getter, ids, 3)
local count_c, sum_c = agent.sink_take()
assert(count_lua == 100 and count_c == 100 and rows == 100 and errors == 0, 'row counts differ')
assert(sum_lua == sum_c and sum_c == 6 * 5050, 'sums differ: ' .. sum_lua .. ' and ' .. sum_c)
-- A getter that raises is counted and does not stop the sweep.
local _, raised = agent.csweep(function(id)
  if id % 10 == 0 then
    error('dead')
  end
  return id
end, ids, 1)
assert(raised == 10, 'csweep counted ' .. tostring(raised) .. ' errors')
agent.sink_take()

-- The socket pair moves bytes, one send and one receive a frame.
assert(agent.sock_open() == true, 'sock_open failed')
local received = 0
for _ = 1, 20 do
  local send_ns, recv_ns, sent, got = agent.sock_frame(512)
  assert(send_ns >= 0 and recv_ns >= 0, 'negative timings')
  assert(sent == 512, 'sent ' .. tostring(sent))
  if got > 0 then
    received = received + got
  end
end
assert(received > 0, 'nothing was received')
agent.sock_close()
local nothing, socket_error = agent.sock_frame(1)
assert(nothing == nil and socket_error, 'sock_frame without a pair did not fail')

-- The system-call frame's other calls: an accept that would block, two
-- checks for files that are absent, and two log flushes.
assert(agent.sys_open() == true, 'sys_open failed')
local before = #S.read(root .. 'logs/agent.log')
for _ = 1, 5 do
  local values = { agent.sys_frame() }
  assert(#values == 6, 'sys_frame returned ' .. #values .. ' values')
  local sum = 0
  for i = 1, 5 do
    assert(values[i] >= 0, 'a negative timing')
    sum = sum + values[i]
  end
  assert(sum > 0, 'every timing is zero')
  assert(values[4] > 0, 'the flush of one record took no time')
  assert(values[6] == -1, 'the accept did not report would-block')
end
assert(#S.read(root .. 'logs/agent.log') > before, 'the flush wrote nothing')
agent.sys_close()
nothing, socket_error = agent.sys_frame()
assert(nothing == nil and socket_error, 'sys_frame without a listener did not fail')

-- The mapped file: the payload under the sequence lock, and the peer's words.
local map = root .. 'status.map'
assert(agent.map_open(map) == true, 'map_open failed')
assert(agent.map_load(0) > 0, 'no magic')
local echo, peer = agent.map_frame(7)
assert(echo == 0 and peer == 0, 'a fresh peer area is not zero')
assert(agent.map_load(1) == 2, 'the sequence reads ' .. tostring(agent.map_load(1)))
for word = 2, 9 do
  assert(agent.map_load(word) == 7, 'payload word ' .. word .. ' reads ' .. tostring(agent.map_load(word)))
end
assert(agent.map_load(64) == nil, 'an index past the end read a value')
agent.map_close()

-- panic: caught by the entry, the agent disabled, status still answers.
assert(select('#', agent.panic()) == 0, 'panic returned values')
local after = agent.status()
assert(after.disabled == true, 'a panic did not disable the agent')
assert(after.reason:find('deliberate panic', 1, true), 'the reason reads ' .. tostring(after.reason))
assert(agent.thread() == nil, 'a disabled agent still answers thread()')
text = S.read(root .. 'logs/agent.log')
assert(text:find('"msg":"panic"', 1, true), 'the panic is not in the log')
assert(text:find('"msg":"disabled after a panic"', 1, true), 'the disable is not in the log')

print('ok  status, configure and the log')
print('ok  thread, clock, qpc_cost ' .. string.format('%.1f', ns) .. ' ns, crumb')
print('ok  raise and call keep Lua errors inside Lua')
print('ok  csweep and the Lua loop with sink give the same sum')
print('ok  the socket pair and the mapped file')
print('ok  a panic is caught and disables the agent')
