-- The shim's event handler in the strict mock's server state, with a stub
-- agent that records each call to agent.event:
--
-- - the shim's allow-lists and field table are the ones in spec/records.md;
-- - every allow-listed field is forwarded with its code when it has the
--   right type, and left out when it has any other;
-- - no event shape makes the handler raise, whatever the event, its fields
--   or the agent do, so a later handler always sees the event;
-- - a shim reload replaces the handler, and the event is forwarded once.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

---------------------------------------------------------------------------
-- spec/records.md: the event_field codes and the allow-lists.

local spec = S.read(root .. 'spec/records.md')

local function section(title)
  local start = spec:find('\n### ' .. title .. '\n', 1, true)
  assert(start, 'spec/records.md has no section ' .. title)
  local stop = spec:find('\n##', start + 1, true) or #spec
  return spec:sub(start, stop)
end

local spec_fields = {}
for code, name, value, from in section('Codes: event_field'):gmatch('\n| (%d+) | ([%u_]+) | (%a+) | ([^|]-) |') do
  spec_fields[name] = { code = tonumber(code), value = value, from = from }
end

local spec_allow, spec_order = {}, {}
for name, list in section('Allow-lists'):gmatch('\n| (S_EVENT_[%u_]+) | ([^|]-) |') do
  local fields = {}
  for field in list:gmatch('[%u_]+') do
    fields[#fields + 1] = field
  end
  spec_allow[name] = fields
  spec_order[#spec_order + 1] = name
end
assert(#spec_order > 0, 'spec/records.md lists no allow-list')

---------------------------------------------------------------------------
-- The shim, loaded into the mock's server state with a stub agent.

local profile = S.scratch()
local dir = profile .. 'nanoprobe/versions/test/'
S.mkdir(dir .. 'lua')
S.mkdir(profile .. 'nanoprobe/logs')
S.copy(root .. 'lua/agent/log.lua', dir .. 'lua/log.lua')

local host = mock.new({ writedir = profile })
local server = host.states.server
host:load_mission('events')

local calls = {}
local agent = {
  abi = '6',
  status = function()
    return { gen = 1 }
  end,
  log = function() end,
  event = function(...)
    calls[#calls + 1] = { n = select('#', ...), ... }
  end,
}

local function load_shim(previous)
  local chunk = assert(loadfile(root .. 'lua/agent/shim.lua'))
  setfenv(chunk, server)
  return chunk(agent, profile .. 'nanoprobe/', dir, previous)
end

local shim = load_shim(nil)
shim.start()

---------------------------------------------------------------------------
-- The shim's tables match the spec.

for name, f in pairs(shim.FIELDS) do
  local s = spec_fields[name]
  assert(s, 'the shim reads ' .. name .. ', which spec/records.md does not list')
  assert(s.code == f[1], name .. ': the shim has code ' .. f[1] .. ', the spec ' .. s.code)
  assert(s.value == f[4], name .. ': the shim takes a ' .. f[4] .. ', the spec a ' .. s.value)
  local from = '`event.' .. f[2] .. (f[3] and ('.' .. f[3]) or '') .. '`'
  assert(s.from == from, name .. ': the shim reads ' .. from .. ', the spec ' .. s.from)
end

local shim_count = 0
for name, list in pairs(shim.ALLOW) do
  shim_count = shim_count + 1
  local s = spec_allow[name]
  assert(s, 'the shim has an allow-list for ' .. name .. ', the spec has none')
  assert(table.concat(s, ',') == table.concat(list, ','),
    name .. ': the shim reads ' .. table.concat(list, ',') .. ', the spec ' .. table.concat(s, ','))
  for _, field in ipairs(list) do
    assert(shim.FIELDS[field], name .. ' allows ' .. field .. ', which the shim cannot read')
  end
  assert(server.world.event[name], name .. ' is not a world.event name')
end
assert(shim_count == #spec_order, 'the spec has ' .. #spec_order .. ' allow-lists, the shim ' .. shim_count)
S.ok('the shim\'s field codes and allow-lists are the ones in spec/records.md')

---------------------------------------------------------------------------
-- Shapes.

-- An object as DCS hands one out: `id_` in the table, everything else
-- through a metatable. This one raises on any other key.
local function object(id)
  return setmetatable({ id_ = id }, {
    __index = function(_, key)
      error('a method was called: ' .. tostring(key))
    end,
  })
end

-- A value of each Lua type the handler may meet.
local function values()
  return {
    number = 7,
    string = 'np',
    boolean = true,
    table = { np = 1 },
    object = object(5),
    ['function'] = function() end,
  }
end

-- The value of the right type for a field.
local function good(f)
  if f[3] == 'id_' then
    return object(1000 + f[1])
  elseif f[3] ~= nil then
    return { [f[3]] = f[1] + 0.5 }
  elseif f[4] == 'number' then
    return f[1] + 0.25
  end
  return 'text ' .. f[1]
end

-- A later handler, which must see every event the shim's handler saw.
local later = 0
server.world.addEventHandler({
  onEvent = function()
    later = later + 1
  end,
})

local function fire(event)
  calls = {}
  local before = later
  server.world.onEvent(event)
  assert(later == before + 1, 'a later handler did not see the event')
  return calls
end

-- The pairs of one call, as code -> value, checking each code is known and
-- each value has the type the spec gives it.
local by_code = {}
for name, f in pairs(shim.FIELDS) do
  by_code[f[1]] = { name = name, f = f }
end
local function pairs_of(call)
  local out, order = {}, {}
  assert(type(call[1]) == 'number', 'the first argument is not the event ID')
  assert(call.n % 2 == 1, 'agent.event took an odd field list')
  for i = 2, call.n, 2 do
    local code, value = call[i], call[i + 1]
    local known = by_code[code]
    assert(known, 'an unknown code ' .. tostring(code))
    assert(type(value) == known.f[4], known.name .. ' carries a ' .. type(value))
    assert(out[code] == nil, known.name .. ' twice')
    out[code] = value
    order[#order + 1] = known.name
  end
  return out, order
end

for name, list in pairs(shim.ALLOW) do
  local id = server.world.event[name]
  -- Every field of the right type: all forwarded, in the allow-list order.
  local event = { id = id }
  for _, field in ipairs(list) do
    local f = shim.FIELDS[field]
    local v = good(f)
    if f[2] == 'pos' then
      event.pos = event.pos or {}
      event.pos[f[3]] = f[1] + 0.5
    else
      event[f[2]] = v
    end
  end
  local c = fire(event)
  assert(#c == 1 and c[1][1] == id, name .. ': not forwarded once')
  local got, order = pairs_of(c[1])
  assert(table.concat(order, ',') == table.concat(list, ','),
    name .. ': forwarded ' .. table.concat(order, ',') .. ' of ' .. table.concat(list, ','))
  for _, field in ipairs(list) do
    local f = shim.FIELDS[field]
    local want = f[3] == 'id_' and 1000 + f[1] or (f[3] and f[1] + 0.5) or (f[4] == 'number' and f[1] + 0.25)
      or ('text ' .. f[1])
    assert(got[f[1]] == want, name .. '.' .. field .. ' is ' .. tostring(got[f[1]]))
  end

  -- Each field at every other type, and absent: left out, the rest kept.
  for _, field in ipairs(list) do
    local f = shim.FIELDS[field]
    for kind, v in pairs(values()) do
      local e = { id = id, time = 1 }
      if f[3] ~= nil then
        -- The container of the wrong kind, and the inner value of the wrong kind.
        e[f[2]] = v
        local c1 = fire(e)
        local g1 = pairs_of(c1[1])
        assert(g1[f[1]] == nil or (kind == 'object' and f[3] == 'id_'),
          name .. '.' .. field .. ' taken from a ' .. kind)
        local inner = {}
        inner[f[3]] = v
        e[f[2]] = (f[3] == 'id_') and setmetatable(inner, { __index = error }) or inner
        local c2 = fire(e)
        local g2 = pairs_of(c2[1])
        assert((kind == f[4]) == (g2[f[1]] ~= nil), name .. '.' .. field .. ' from an inner ' .. kind)
      else
        e[f[2]] = v
        local c1 = fire(e)
        local g1 = pairs_of(c1[1])
        assert((kind == f[4]) == (g1[f[1]] ~= nil), name .. '.' .. field .. ' from a ' .. kind)
      end
    end
  end
end
S.ok('each allow-listed field is forwarded at its type and left out at any other')

-- An event of a type with no allow-list: its ID only.
local c = fire({ id = server.world.event.S_EVENT_UNIT_TASK_STAGE, time = 3, initiator = object(9), text = 'x' })
assert(#c == 1 and c[1].n == 1 and c[1][1] == server.world.event.S_EVENT_UNIT_TASK_STAGE, 'a type with no list was not ID only')
c = fire({ id = 9999, time = 3 })
assert(#c == 1 and c[1].n == 1 and c[1][1] == 9999, 'an unknown ID was not forwarded alone')
S.ok('an event with no allow-list, or of an unknown type, is forwarded with its ID only')

-- Shapes that are not events at all, or events whose parts raise when read
-- other than raw. None raises; a non-table or an event with no numeric ID
-- does not reach the agent, and a raising part is left out.
local raising = setmetatable({}, {
  __index = function()
    error('read through a metamethod')
  end,
})
local ev = server.world.event
-- Each shape, how many calls reach the agent, and what the one call must
-- or must not carry, by field name.
local weird = {
  { 'not a table', 0 },
  { 42, 0 },
  { true, 0 },
  { function() end, 0 },
  { {}, 0 },
  { { id = 'S_EVENT_DEAD' }, 0 },
  { { id = 0 / 0 }, 1 },
  { { id = ev.S_EVENT_HIT, initiator = raising, target = raising, weapon = raising }, 1,
    absent = { 'INITIATOR', 'TARGET', 'WEAPON' } },
  { setmetatable({ id = ev.S_EVENT_DEAD }, { __index = error }), 1, absent = { 'TIME', 'INITIATOR' } },
  { { id = ev.S_EVENT_MARK_ADDED, pos = raising, text = string.rep('x', 100000) }, 1,
    absent = { 'POS_X', 'POS_Y', 'POS_Z' }, present = { 'TEXT' } },
  { { id = ev.S_EVENT_SHOT, time = 0 / 0, initiator = object(0 / 0) }, 1, present = { 'TIME', 'INITIATOR' } },
}
local errors_before = shim.event_errors
for i, case in ipairs(weird) do
  calls = {}
  local before = later
  server.world.onEvent(case[1])
  assert(later == before + 1, 'shape ' .. i .. ': a later handler did not see the event')
  assert(#calls == case[2], 'shape ' .. i .. ': ' .. #calls .. ' calls reached the agent')
  if case[2] == 1 then
    local got = pairs_of(calls[1])
    for _, name in ipairs(case.absent or {}) do
      assert(got[shim.FIELDS[name][1]] == nil, 'shape ' .. i .. ': ' .. name .. ' was read')
    end
    for _, name in ipairs(case.present or {}) do
      assert(got[shim.FIELDS[name][1]] ~= nil, 'shape ' .. i .. ': ' .. name .. ' is missing')
    end
  end
end
-- Every read was raw: no shape made the handler catch an error.
assert(shim.event_errors == errors_before, 'a shape raised inside the handler')
S.ok('no event shape makes the handler raise, and a later handler sees every event')

-- An agent that raises: the handler catches it, counts it, and a later
-- handler still sees the event.
local real_event = agent.event
agent.event = function()
  error('the agent raised')
end
local errors = shim.event_errors
fire({ id = server.world.event.S_EVENT_DEAD, time = 1, initiator = object(3) })
assert(shim.event_errors == errors + 1, 'the error was not counted')
agent.event = real_event
S.ok('an agent that raises is caught and counted')

-- A reload: the new shim's handler replaces the old one.
local reloaded = load_shim(shim)
reloaded.start()
c = fire({ id = server.world.event.S_EVENT_DEAD, time = 1, initiator = object(3) })
assert(#c == 1, 'a reload left ' .. #c .. ' handlers forwarding')
assert(server.world.eventHandlers == nil or server.world.eventHandlers[shim.handler] == nil,
  'the replaced handler is still registered')
-- A second start, as on the next mission, adds nothing.
reloaded.start()
c = fire({ id = server.world.event.S_EVENT_DEAD, time = 1, initiator = object(3) })
assert(#c == 1, 'a second start left ' .. #c .. ' handlers forwarding')
S.ok('a shim reload replaces the handler, and another start adds none')
