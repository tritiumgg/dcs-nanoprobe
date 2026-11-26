-- The route-shape generator of E37 (lua/scenarios/routes.lua), two ways.
--
-- First under the strict mock, through the eval channel: every shape adds
-- the unit count asked for, in the group size the shape names, with the
-- route the shape names, and groups do not start on one another. The mock
-- has no roads (land.getClosestPointOnRoads is not mocked), so the on-road
-- shape must come back skipped with the reason.
--
-- Then against stubs, where the test controls the library calls: the road
-- shape on a stub road, the formation in the first waypoint, the dense
-- path's waypoint spacing, and the Hold order of the held shape. The mock
-- does not keep a route point's action, so the stubs are the only check of
-- it here.

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
for _, lib in ipairs({ 'scenario', 's15', 'routes', 'movers' }) do
  S.copy(root .. 'lua/scenarios/' .. lib .. '.lua', profile .. 'nanoprobe/dev/lua/' .. lib .. '.lua')
end

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = false })

local serial = 0
local function eval(state, source, seconds)
  serial = serial + 1
  local id = 'r' .. serial
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

local function spawn(shape, n, extra)
  return eval('server', string.format(
    "local E = ...\nreturn E.lib('routes').spawn('%s', %d, { label = 'r', kind = 'M-113'%s })\n", shape, n, extra or ''), 30)
end
local function clear()
  eval('server', "local E = ...\nreturn E.lib('routes').clear()\n", 30)
  for _ = 1, 120 do
    host:frame(1 / 60)
  end
end

-- shape: groups, units per group, route points.
local EXPECT = {
  ['halted'] = { 40, 10, 1 },
  ['offroad-few'] = { 40, 10, 2 },
  ['offroad-dense'] = { 40, 10, 81 },
  ['formation-off'] = { 100, 4, 2 },
  ['formation-line'] = { 100, 4, 2 },
  ['formation-column'] = { 100, 4, 2 },
  ['formation-diamond'] = { 100, 4, 2 },
  ['size-1'] = { 400, 1, 2 },
  ['size-4'] = { 100, 4, 2 },
  ['size-10'] = { 40, 10, 2 },
  ['held-order'] = { 40, 10, 2 },
  ['held-zero'] = { 40, 10, 1 },
}
local SHAPES = { 'halted', 'offroad-few', 'offroad-dense', 'formation-off', 'formation-line', 'formation-column',
  'formation-diamond', 'size-1', 'size-4', 'size-10', 'held-order', 'held-zero' }

for _, shape in ipairs(SHAPES) do
  local want = EXPECT[shape]
  local out = spawn(shape, 400)
  assert(out.units == 400, shape .. ': units ' .. tostring(out.units))
  assert(#out.groups == want[1], shape .. ': groups ' .. #out.groups)
  assert(out.per == want[2], shape .. ': per ' .. tostring(out.per))
  assert(not out.skipped and not out.add_errors, shape .. ': ' .. tostring(out.skipped or out.first_add_error))
  -- What the mock holds: the groups, the units, and each group's route.
  local groups, units, positions = 0, 0, {}
  for name, group in pairs(host.sim.groups) do
    if name:find('^np%-r%-g') then
      groups = groups + 1
      units = units + #group.units
      assert(#group.route == want[3], shape .. ': ' .. name .. ' has ' .. #group.route .. ' route points')
      local lead = group.units[1]
      local key = string.format('%.0f,%.0f', lead.x, lead.z)
      assert(not positions[key], shape .. ': two groups start at ' .. key)
      positions[key] = true
    end
  end
  assert(groups == want[1] and units == 400, shape .. ': the mock holds ' .. groups .. ' groups, ' .. units .. ' units')
  if shape == 'held-order' then
    local holds = 0
    for _, order in ipairs(host.sim.orders) do
      if type(order.task) == 'table' and order.task.id == 'Hold' then
        holds = holds + 1
      end
    end
    assert(holds == 40, 'held-order: Hold orders ' .. holds)
  end
  clear()
  S.ok(string.format('%s: %d groups of %d, %d route points', shape, want[1], want[2], want[3]))
end

-- A count that is not a multiple of the group size ends with a short group.
local odd = spawn('offroad-few', 95)
assert(odd.units == 95 and #odd.groups == 10, 'a short last group: ' .. tostring(odd.units) .. ' units')
clear()
S.ok('95 units make nine groups of ten and one of five')

-- No roads in the mock: the shape is skipped, and says why.
local road = spawn('onroad', 400)
assert(road.skipped and road.units == 0, 'the on-road shape without a road: ' .. tostring(road.skipped))
S.ok('onroad without roads is skipped: ' .. road.skipped:sub(1, 60))
clear()

-- A shape the generator does not know is an error naming it.
local ok, err = pcall(eval, 'server', "local E = ...\nreturn E.lib('routes').spawn('nope', 10, {})\n", 5)
assert(not ok and tostring(err):find('unknown shape nope', 1, true), 'unknown shape: ' .. tostring(err))
S.ok('an unknown shape is refused by name')

-- Nothing is left behind.
local result
do
  serial = serial + 1
  local id = 'r' .. serial
  S.write(base .. 'in/' .. id .. '.uninstall.lua', '')
  for _ = 1, 600 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      result = S.json(text)
      break
    end
  end
end
assert(result and result.ok, 'uninstall')
for _ = 1, 5 do
  host:frame(1 / 60)
end
assert(next(host.sim.groups) == nil, 'a group is left')
assert(#host.timer_errors == 0, 'a timer raised: ' .. tostring(host.timer_errors[1]))

---------------------------------------------------------------------------
-- Part two: the generator against stubs.

local calls, orders = {}, {}
local function stub_group()
  return {
    getController = function()
      return {
        setTask = function(_, task)
          orders[#orders + 1] = task
        end,
      }
    end,
  }
end
local lib_stub = {
  scenario = {
    origin = function() return { x = 0, y = 0, z = 0 } end,
    air_point = function(x, z, speed, alt)
      return { x = x, y = z, speed = speed, alt = alt, action = alt and 'Turning Point' or 'Off Road' }
    end,
    groups = {},
    clear = function() return 0 end,
  },
  s15 = {
    add_ground = function(name, kind, units, o)
      calls[#calls + 1] = { name = name, kind = kind, units = units, route = o.route }
      return stub_group()
    end,
  },
}
local env = setmetatable({
  -- A straight road along z = 5000: the closest road point to (x, z) is
  -- (x, 5000), whatever z is, within 30 km.
  land = {
    getClosestPointOnRoads = function(kind, x, z)
      assert(kind == 'roads', 'road type ' .. tostring(kind))
      return x, 5000
    end,
  },
}, { __index = _G })
local chunk = assert(loadfile(root .. 'lua/scenarios/routes.lua'))
setfenv(chunk, env)
local R = chunk({ lib = function(name) return lib_stub[name] end, clock = function() return 0 end })

-- The dense path has a waypoint every `step` meters, the ends included.
assert(R.dense_points(4000, 50) == 81, 'dense points ' .. R.dense_points(4000, 50))
local path = R.path(100, 7, 4000, 8, true, 50, nil)
assert(#path == 81 and path[1].x == 100 and path[81].x == 4100 and path[2].x - path[1].x == 50, 'the dense path')
assert(#R.path(0, 0, 4000, 8, false, 50, nil) == 2, 'the short path')
S.ok('the dense path holds a waypoint every 50 m, ends included')

-- The formation sits in the first waypoint only, and `formations` replaces it.
R.spawn('formation-column', 8, { label = 't' })
assert(#calls == 2 and calls[1].route[1].action == 'Cone' and calls[1].route[2].action == nil, 'the column formation')
R.lane = 0
calls = {}
R.spawn('formation-column', 4, { label = 't', formations = { column = 'Vee' } })
assert(calls[1].route[1].action == 'Vee', 'a formation set by the caller')
R.lane = 0
calls = {}
R.spawn('formation-line', 4, { label = 't' })
assert(calls[1].route[1].action == 'Rank', 'the line formation')
R.spawn('formation-diamond', 4, { label = 't' })
assert(calls[2].route[1].action == 'Diamond', 'the diamond formation')
R.spawn('formation-off', 4, { label = 't' })
assert(calls[3].route[1].action == 'Off Road', 'the off-road formation')
S.ok('the formation action is in the first waypoint, from the table or the caller')

-- Lanes do not overlap: successive groups start `lane_m` apart.
calls = {}
R.lane = 0
R.spawn('offroad-few', 30, { label = 'l', lane_m = 100 })
assert(calls[2].units[1].z - calls[1].units[1].z == 100 and calls[1].units[1].x == calls[2].units[1].x, 'lanes')
S.ok('groups start in lanes 100 m apart')

-- The road shape: two road points per group, both On Road, on the stub road.
calls = {}
R.lane = 0
local summary = R.spawn('onroad', 30, { label = 'o', length = 4000 })
assert(not summary.skipped and summary.units == 30 and #calls == 3, 'road groups: ' .. tostring(summary.skipped))
for _, call in ipairs(calls) do
  assert(#call.route == 2 and call.route[1].action == 'On Road' and call.route[2].action == 'On Road', 'On Road points')
  assert(call.route[1].z == 5000 and call.route[2].z == 5000, 'the points are on the stub road')
  assert(math.abs(call.route[2].x - call.route[1].x) >= 2000, 'the route is long enough')
  assert(call.units[1].x == call.route[1].x and call.units[1].z == 5000, 'the units start on the road')
end
assert(summary.road_points >= 1, 'road points found')
S.ok('the road shape gives On Road points on the road, each group on its own road point where there are several')

-- No road within reach: skipped with the reason.
env.land.getClosestPointOnRoads = function() return nil end
calls = {}
local none = R.spawn('onroad', 20, { label = 'n' })
assert(none.skipped and #calls == 0, 'no road: ' .. tostring(none.skipped))
env.land.getClosestPointOnRoads = nil
local missing = R.spawn('onroad', 20, { label = 'm' })
assert(missing.skipped and missing.skipped:find('getClosestPointOnRoads', 1, true), 'no land call: ' .. tostring(missing.skipped))
S.ok('the road shape is skipped when no road is found or the call is missing')

-- The held shape: Hold orders after the spawn.
orders, calls = {}, {}
R.lane = 0
R.spawn('held-order', 20, { label = 'h' })
assert(#orders == 2 and orders[1].id == 'Hold', 'Hold orders')
S.ok('the held shape orders Hold on every group')

S.remove(profile)
