-- The FARP-group snippets of lua/experiments/farp-groups under the strict
-- mock, in their order: each finishes in its state and returns its reads, and
-- `cleanup.lua` leaves nothing behind. The mock's heliport group is one FARP
-- airbase under the first unit's name, and makes every other unit of a
-- static group a static of its own, as DCS did (LIVE,
-- docs/results/2026-11-25-farp-groups.md).

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

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })

local hook = host.states.hook
rawset(hook.DCS, 'getFarpsAndCarriersMissionData', function()
  local farps = {}
  for _, b in ipairs(host.sim.airbases) do
    if b.farp then
      farps[b.id] = { name = b.name, type = b.farp.type }
    end
  end
  return { farps = farps, carriers = {} }
end)
-- Dynamic spawn reads as DCS read it: from the first unit's `dynamicSpawn`
-- (LIVE, docs/results/2026-11-25-farp-groups.md).
rawset(hook.DCS, 'getDynamicSpawnSettings', function(key)
  for _, b in ipairs(host.sim.airbases) do
    if b.id == key and b.farp then
      return { dynamicSpawnAvailable = b.farp.dynamic == true, allowHotSpawn = false }
    end
  end
  return { dynamicSpawnAvailable = false }
end)

local serial = 0
local function eval(file, state, frames)
  local source = S.read(root .. 'lua/experiments/farp-groups/' .. file)
  assert(source, 'no ' .. file)
  assert(source:find('%-%- states: ' .. state .. '\n'), file .. ' names the ' .. state .. ' state')
  serial = serial + 1
  local id = 'fg' .. serial
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, (frames or 30) * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.ok, file .. ': ' .. tostring(result.error))
      assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error) .. ' ' .. text)
      return result.reply.values[1], text
    end
  end
  error(file .. ': no result')
end

local server = host.states.server

do
  local out, text = eval('spawn.lua', 'server')
  local g3 = out.groups.g3
  assert(g3.returned.category == 4 and g3.new_airbases == 1, 'g3 is one airbase: ' .. text)
  assert(g3.parking == 12, 'g3 has 12 spots: ' .. text)
  for k = 1, 3 do
    assert(g3.names['np-fg-g3-' .. k].static.id == g3.returned.id, 'g3 unit ' .. k .. ' finds the airbase: ' .. text)
  end
  assert(out.groups.g10.parking == 40, 'g10 has 40 spots: ' .. text)
  assert(out.groups.f2.returned.category == 3 and out.groups.f2.new_airbases == 0, 'f2 is two statics: ' .. text)
  assert(out.groups.ff2.parking == 8 and out.groups.ff2.names['np-fg-ff2-3'].static.class ~= 'Airbase', text)
  S.ok('spawn.lua: 17 groups added and read')
end

do
  local out, text = eval('slots.lua', 'hook')
  assert(out['np-fg-g3-1'] and out['np-fg-g3-1'].dynamic == false, 'g3 (group key only): ' .. text)
  assert(out['np-fg-g2u-1'] and out['np-fg-g2u-1'].dynamic == true, 'g2u (unit 1 key): ' .. text)
  S.ok('slots.lua: the FARP list read')
end

do
  local out, text = eval('replace.lua', 'server')
  assert(out.p3.returned.category == 4 and out.p1.returned.category == 4, text)
  assert(out.p3.names['np-fg-p3-2'].static.id == out.p3.returned.id, 'p3 unit 2 finds the new airbase: ' .. text)
  S.ok('replace.lua: three replace tries read')
end

do
  local out, text = eval('remove.lua', 'server')
  assert(tostring(out.calls['np-fg-g3-1']):find('^returned'), text)
  -- Paused: the destroy waits for the simulation (LIVE).
  assert(out.now.g3.names['np-fg-g3-2'].static.exists == true, text)
  S.ok('remove.lua: groups destroyed while paused')
end

do
  local out, text = eval('run.lua', 'hook', 60)
  assert(out.paused == true and out.running == 'done', 'run.lua: ' .. text)
  S.ok('run.lua: resumed, replaced while running, paused')
end

do
  local out, text = eval('settle.lua', 'hook', 60)
  assert(out.paused == true and out.to - out.from >= 10, 'settle.lua: ' .. text)
  S.ok('settle.lua: ran 10 s and paused')
end

do
  local out, text = eval('after.lua', 'server')
  local g3 = out.groups.g3.names
  assert(g3['np-fg-g3-1'].airbase == 'nil', 'g3 airbase gone after the run: ' .. text)
  assert(type(g3['np-fg-g3-2'].static) ~= 'table' or g3['np-fg-g3-2'].static.exists == false, text)
  S.ok('after.lua: read after the run')
end

do
  local out, text = eval('cleanup.lua', 'server')
  assert(type(out) == 'table', text)
  host:set_pause(false)
  host:frame(1 / 60)
  host:set_pause(true)
  for _, tag in ipairs({ 'g2u', 'h2', 'p3', 'r1', 'd2' }) do
    local left = server.StaticObject.getByName('np-fg-' .. tag .. '-1')
    assert(not left or not left:isExist(), tag .. ' left after the cleanup')
  end
  assert(rawget(server, 'np_farp_groups') == nil, 'the global is left')
  S.ok('cleanup.lua: nothing left')
end

-- Run 2's one-unit replaces.
do
  local out, text = eval('one.lua', 'server')
  assert(out.o2.new ~= out.o2.old and out.o2.old_exists == false, 'o2 replaced at once: ' .. text)
  assert(out.o1.old_exists == false, 'o1 replaced at once: ' .. text)
  local ran, rtext = eval('one-run.lua', 'hook', 60)
  assert(ran.replaced == 'done', 'one-run.lua: ' .. rtext)
  local after, atext = eval('one-after.lua', 'server')
  assert(after.old.q2 == false and after.old.q1 == false, atext)
  local clean, ctext = eval('one-clean.lua', 'server')
  assert(type(clean) == 'table' and rawget(server, 'np_farp_one') == nil, ctext)
  S.ok('one.lua, one-run.lua, one-after.lua and one-clean.lua: one-unit replaces read')
end
