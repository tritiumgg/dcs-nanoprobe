-- The benchmark meters under the strict mock: the mover count
-- (lua/scenarios/movers.lua) and the frame meter (lua/sampling/frames.lua),
-- through the eval channel. The mock has no terrain: ships are skipped by
-- scenarios that find no water, so the scenario is ground groups only.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local profile = S.scratch()
S.mkdir(profile .. 'Scripts/Hooks')
S.mkdir(profile .. 'nanoprobe/dev/lua')
for _, folder in ipairs({ 'in', 'taken', 'out' }) do
  S.mkdir(profile .. 'nanoprobe/dev/eval/' .. folder)
end
S.copy(root .. 'lua/dev/eval-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
S.copy(root .. 'lua/dev/eval-prelude.lua', profile .. 'nanoprobe/dev/lua/prelude.lua')
S.copy(root .. 'lua/scenarios/scenario.lua', profile .. 'nanoprobe/dev/lua/scenario.lua')
S.copy(root .. 'lua/scenarios/movers.lua', profile .. 'nanoprobe/dev/lua/movers.lua')

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = false })

local serial = 0
local function eval(state, source, seconds)
  serial = serial + 1
  local id = 'm' .. serial
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, seconds * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.ok, id .. ': ' .. tostring(result.error))
      assert(result.reply.ok, id .. ': ' .. tostring(result.reply.error))
      return result.reply.values[1]
    end
  end
  error(id .. ' in ' .. state .. ': no result in ' .. seconds .. ' s')
end

-- 60 ground units: 6 groups of 10, the first and the fifth of the lanes
-- moving at a share of 0.5 (every second one).
local spawned = eval('server',
  "local E = ...\nreturn E.lib('scenario').spawn_mixed(60, { label = 'm', moving = 0.5, ground_kind = 'M-113' })\n", 30)
assert(spawned.ground > 0, 'nothing spawned')

local COUNT = "local E = ...\nreturn E.lib('movers').count({ prefix = 'np-m-' })\n"
local first = eval('server', COUNT, 10)
assert(first.new == first.groups and first.moved == 0 and first.still == 0 and first.share == nil,
  'the first call only records positions')
S.ok(string.format('movers: first call records %d groups', first.groups))

-- Drive the mock for a minute of model time: the routed groups move.
for _ = 1, 60 * 60 do
  host:frame(1 / 60)
end
local second = eval('server', COUNT, 10)
assert(second.new == 0 and second.moved + second.still == first.groups, 'every group was measured')
assert(second.moved > 0 and second.still > 0, 'some groups move and some stay: ' .. second.moved .. '/' .. second.still)
assert(second.share > 0 and second.share < 1, 'share ' .. tostring(second.share))
S.ok(string.format('movers: %d moved, %d still, share %.2f', second.moved, second.still, second.share))

-- A group added since the last call is new; a group destroyed is forgotten.
eval('server', "local E = ...\nreturn E.lib('scenario').spawn_mixed(10, { label = 'n', moving = 0 })\n", 30)
local third = eval('server', "local E = ...\nreturn E.lib('movers').count({ prefix = 'np-' })\n", 10)
assert(third.new >= 1, 'a new group is counted as new')
S.ok('movers: a group spawned since the last call counts as new')

-- The frame meter: the mock runs 60 frames a second. Its two log lines are
-- the host's wall-clock tie.
local source = S.read(root .. 'lua/sampling/frames.lua'):gsub('@SETTLE@', '2'):gsub('@MEASURE@', '5'):gsub('@TAG@', 'unit')
local before = #host.dcslog
local m = eval('hook', source, 30)
assert(m.tag == 'unit', 'tag ' .. tostring(m.tag))
-- The frames are the intervals between frames: a count that includes the
-- first frame is one more than the span holds.
local span_frames = math.floor((m.last_real - m.first_real) * 60 + 0.5)
assert(m.frames == span_frames and span_frames >= 299, 'intervals in 5 s at 60 fps: ' .. m.frames .. ' of ' .. span_frames)
local rate = m.frames / (m.last_real - m.first_real)
assert(math.abs(rate - 60) < 0.5, 'intervals over the real span: ' .. rate)
assert(#m.buckets == 5, 'buckets: ' .. #m.buckets)
for i, b in ipairs(m.buckets) do
  assert(b >= 59 and b <= 61, 'bucket ' .. i .. ': ' .. b)
end
assert(m.paused == 0, 'paused frames: ' .. m.paused)
local ratio = (m.last_model - m.first_model) / (m.last_real - m.first_real)
assert(math.abs(ratio - 1) < 0.01, 'model/real ' .. ratio)
local start, finish
for i = before + 1, #host.dcslog do
  local line = host.dcslog[i]
  if line.subsystem == 'NANOPROBE-METER' then
    if line.message:find('^unit start real=') then
      start = line.message
    elseif line.message:find('^unit end real=') then
      finish = line.message
    end
  end
end
assert(start and finish, 'the meter wrote no start and end lines')
assert(finish:find('frames=' .. m.frames, 1, true), 'the end line does not carry the frame count: ' .. finish)
S.ok(string.format('frames: %d frames in 5 s, model/real %.3f, start and end logged', m.frames, ratio))

S.remove(profile)
