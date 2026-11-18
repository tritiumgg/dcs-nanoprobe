-- The control run's churn library (lua/scenarios/soak.lua) under the strict
-- mock, through the eval channel: the live count holds on every tick, names
-- are never reused, the handover window moves round the ring, the counts
-- the ticks report add up to the totals in the stats, kills are reported
-- only on kill ticks, and a ring with no free place reports an error and
-- does not raise.

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
for _, lib in ipairs({ 'scenario', 'soak' }) do
  S.copy(root .. 'lua/scenarios/' .. lib .. '.lua', profile .. 'nanoprobe/dev/lua/' .. lib .. '.lua')
end

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = false })

local serial = 0
local function eval(source)
  serial = serial + 1
  local id = 'r' .. serial
  S.write(base .. 'in/' .. id .. '.server.lua', source)
  for _ = 1, 30 * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.ok, id .. ': ' .. tostring(result.error))
      assert(result.reply.ok, id .. ': ' .. tostring(result.reply.error))
      return result.reply.values[1]
    end
  end
  error(id .. ': no result')
end
local function frames(n)
  for _ = 1, n do
    host:frame(1 / 60)
  end
end

local function places(n)
  local out = {}
  for i = 1, n do
    out[i] = string.format('{%d, 0}', (i - 1) * 100)
  end
  return '{ ' .. table.concat(out, ', ') .. ' }'
end
local function setup(n, live)
  return eval(string.format(
    "local E = ...\nreturn E.lib('soak').setup{ places = %s, live = %d, units = 4 }\n", places(n), live))
end
local function names()
  return eval([[
    local st = _G.__nanoprobe_soak
    local out = {}
    for _, e in ipairs(st.live) do out[#out + 1] = e.place .. ':' .. e.name end
    return out
  ]])
end
local function count(t)
  local n = 0
  for _ in pairs(t) do
    n = n + 1
  end
  return n
end

-- 30 places: ten ticks move the window 20 places, short of a full turn.
local first = setup(30, 8)
frames(2)
assert(first.groups == 8 and first.units == 32, 'setup: ' .. first.groups .. ' groups, ' .. first.units .. ' units')
assert(first.max_id and first.max_id > 0, 'setup: no max_id')
assert(first.spawned_units_total == 32, 'setup total')
S.ok('setup spawns 8 groups of 4 units')

local seen, sums = {}, { spawned = 32, destroyed = 0, killed = 0 }
for _, entry in ipairs(names()) do
  seen[entry:match(':(.*)$')] = true
end
local top_place = 0
local last
for i = 1, 10 do
  local kill = i % 4 == 0
  last = eval(string.format(
    "local E = ...\nreturn E.lib('soak').tick{ handover = 2, replace = 1, kill = %s, orders = 2 }\n", tostring(kill)))
  assert(#last.errors == 0, 'tick ' .. i .. ': ' .. tostring(last.errors[1]))
  frames(3)
  local live = names()
  assert(#live == 8, 'tick ' .. i .. ': ' .. #live .. ' live groups')
  for _, entry in ipairs(live) do
    local place, name = entry:match('^(%d+):(.*)$')
    top_place = math.max(top_place, tonumber(place))
    if not seen[name] then
      seen[name] = true
    end
  end
  sums.spawned = sums.spawned + last.spawned_units
  sums.destroyed = sums.destroyed + last.destroyed_units
  sums.killed = sums.killed + last.killed_units
  if kill then
    assert(last.killed_units == 4, 'tick ' .. i .. ': killed ' .. last.killed_units)
  else
    assert(last.killed_units == 0, 'tick ' .. i .. ' reported kills')
  end
  assert(last.orders == 2, 'tick ' .. i .. ': ' .. last.orders .. ' orders')
end
S.ok('ten ticks keep 8 live groups')

-- A name seen twice would have one entry less than the groups spawned.
local spawned_groups = sums.spawned / 4
assert(count(seen) == spawned_groups, 'a group name was reused: ' .. count(seen) .. ' names for ' .. spawned_groups)
S.ok('no group name is reused')

local live = names()
local at = {}
for _, entry in ipairs(live) do
  at[tonumber(entry:match('^(%d+):'))] = true
end
assert(top_place > 8 and not at[1], 'the window did not move: top place ' .. top_place)
local above = false
for p in pairs(at) do
  above = above or p > 8
end
assert(above, 'no group above place 8')
S.ok('the handover window moves round the ring')

local stats = eval("local E = ...\nreturn E.lib('soak').stats()\n")
assert(stats.spawned_units_total == sums.spawned, 'spawned total ' .. stats.spawned_units_total .. ' vs ' .. sums.spawned)
assert(stats.destroyed_units_total == sums.destroyed, 'destroyed total')
assert(stats.killed_units_total == sums.killed and sums.killed == 8, 'killed total ' .. sums.killed)
assert(stats.ticks == 10, 'ticks')
assert(stats.units == stats.spawned_units_total - stats.destroyed_units_total - stats.killed_units_total,
  'units that exist do not match the totals: ' .. stats.units)
assert(stats.max_id > first.max_id, 'max_id did not grow')
S.ok('the tick reports add up to the stats')

-- Orders reached the controller as a Mission task of four square laps.
local task
for _, order in ipairs(host.sim.orders) do
  task = order.task or task
end
assert(task and task.id == 'Mission' and #task.params.route.points == 17, 'order shape')
assert(task.params.route.points[1].action == 'Off Road' and task.params.route.points[1].speed == 5, 'order point')
S.ok('orders are Mission tasks with a 17-point route')

local cleared = eval("local E = ...\nreturn E.lib('soak').clear()\n")
frames(5)
assert(cleared >= 8, 'clear destroyed ' .. cleared)
for name in pairs(host.sim.groups) do
  assert(not name:find('^ctl%-'), 'a group is left: ' .. name)
end
S.ok('clear leaves no group of ours')

-- A ring with no free place cannot hand over: an error string, no raise.
setup(8, 8)
frames(2)
local short = eval("local E = ...\nreturn E.lib('soak').tick{ handover = 2, replace = 0, orders = 0 }\n")
assert(#short.errors == 1 and short.errors[1]:find('handover', 1, true), 'short ring: no error')
assert(short.spawned_units == 0 and short.destroyed_units == 0, 'short ring changed groups')
eval("local E = ...\nreturn E.lib('soak').clear()\n")
S.ok('a ring too short to hand over reports an error')
assert(#host.timer_errors == 0, 'a timer raised: ' .. tostring(host.timer_errors[1]))
