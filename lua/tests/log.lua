-- The Lua JSON-lines log writes the record spec/log.md specifies, flushes
-- every line, and rotates at its cap.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local logmodule = dofile(root .. 'lua/agent/log.lua')

-- Encoding.
local line = logmodule.encode({
  wall = '2026-11-10T00:00:00Z',
  level = 'info',
  target = 'hook',
  msg = 'a "quoted"\nline\1',
  real = 1.5,
  model = 0 / 0,
  gen = 3,
  build = 'abc',
  data = { b = true, a = 'x', n = 2 ^ 60, z = 1 / 0 },
})
local expected = '{"wall":"2026-11-10T00:00:00Z","level":"info","target":"hook",'
  .. '"msg":"a \\"quoted\\"\\nline\\u0001","real":1.5,"model":null,"gen":3,'
  .. '"build":"abc","data":{"a":"x","b":true,"n":1.152921504606847e+18,"z":null}}\n'
assert(line == expected, 'encoded\n' .. line .. 'expected\n' .. expected)
S.ok('a record encodes in key order, escaped, with non-finite numbers as null')

local bad = logmodule.encode({ wall = 'w', level = 'loud', target = 't', msg = 1 })
assert(bad == '{"wall":"w","level":"info","target":"t","msg":"1"}\n', bad)
S.ok('an unknown level writes as info and the optional keys are left out')

-- Writing and rotation.
local dir = S.scratch()
local path = dir .. 'hook.log'
local model = 0
local log = logmodule.open({
  path = path,
  target = 'hook',
  cap = 300,
  real = function()
    return 7
  end,
  model = function()
    return model
  end,
  gen = function()
    error('a clock that raises is left out')
  end,
})

log.write('info', 'first', { k = 1 })
local lines = S.lines(path)
assert(#lines == 1, 'a written line is on disk before close: ' .. #lines)
assert(lines[1]:match('^{"wall":"%d%d%d%d%-%d%d%-%d%dT%d%d:%d%d:%d%dZ","level":"info"'),
  lines[1])
assert(lines[1]:find('"real":7,"model":0,"data":{"k":1}}', 1, true), lines[1])
assert(not lines[1]:find('"gen"', 1, true), lines[1])
S.ok('a write is flushed at once, and a raising clock is left out')

for i = 1, 10 do
  model = i
  log.write('debug', 'line ' .. i)
end
log.close()
local live, old = S.read(path), S.read(path .. '.1')
assert(old, 'no rotated file')
assert(#live <= 300 and #old <= 300, 'live ' .. #live .. ', old ' .. #old)
assert(live:find('line 10', 1, true), 'the newest line is in the live file')
S.ok('a file rotates to .1 at its cap, and neither file passes it')

-- A file that cannot be opened reports once and never raises.
local reports = {}
local broken = logmodule.open({
  path = dir .. 'missing/hook.log',
  target = 'hook',
  fallback = function(message)
    reports[#reports + 1] = message
  end,
})
broken.write('info', 'one')
broken.write('info', 'two')
assert(#reports == 1, 'reported ' .. #reports .. ' times')
S.ok('an unwritable file reports once through the fallback and does not raise')

S.remove(dir)
