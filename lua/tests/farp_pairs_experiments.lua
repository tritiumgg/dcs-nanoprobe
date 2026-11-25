-- The FARP-pair snippets of lua/experiments/farp-pairs under the strict mock:
-- each finishes in its state and returns its reads, and the stock it sets is put
-- back. The mission's pair stands in as two `coalition.addStaticObject` FARPs,
-- each with its own stock, so the shared-stock reads are checked to run and put
-- back, not to share; what DCS does with a static group of two is in
-- docs/results/2026-11-25-farp-pairs.md.

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

local server = host.states.server
for k = 1, 2 do
  server.coalition.addStaticObject(server.country.id.USA, { name = 'np-hg-' .. k, category = 'Heliports',
    type = 'FARP', shape_name = 'FARPS', x = 200 * (k - 1), y = 0, heading = 0 })
end
local farps = {}
for _, b in ipairs(host.sim.airbases) do
  if b.name:find('^np%-hg%-') then
    farps[b.name] = b
    b.stock.liquids[0] = b.name == 'np-hg-1' and 10000 or 20000
  end
end
local hook = host.states.hook
rawset(hook.DCS, 'getFarpsAndCarriersMissionData', function()
  return { farps = { [farps['np-hg-1'].id] = { coalition = 2 } }, carriers = {} }
end)
rawset(hook.DCS, 'getDynamicSpawnSettings', function()
  return { dynamicSpawnAvailable = true }
end)

local serial = 0
-- `fill` is put in as run.sh puts it: `<try>` in make.lua, `<name>` in parking.lua.
local function eval(file, state, fill)
  local source = S.read(root .. 'lua/experiments/farp-pairs/' .. file)
  assert(source:find('%-%- states: ' .. state .. '\n'), file .. ' names the ' .. state .. ' state')
  if fill then
    source = source:gsub('<try>', fill):gsub('<name>', fill)
  end
  serial = serial + 1
  local id = 'fp' .. serial
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, 20 * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.ok, file .. ': ' .. tostring(result.error))
      assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error) .. ' ' .. text)
      return result.reply.values[1], text
    end
  end
  error(file .. ': no result in 20 s')
end

do
  local out, text = eval('read.lua', 'server')
  assert(type(out.bases['np-hg-1'].id) == 'number' and type(out.bases['np-hg-2'].id) == 'number', text)
  assert(out.shared, 'read: the shared-stock reads ran ' .. text)
  assert(out.shared.after_mk82_set_7_at_one.one.mk82 == 7, text)
  assert(out.shared.after_jet_fuel_set_4321_at_two.two.jet_fuel == 4321, text)
  -- The mock's two FARPs are apart, as two statics are.
  assert(out.shared.after_mk82_set_7_at_one.two.mk82 == 0, text)
  assert(out.restored.one.mk82 == 0 and out.restored.two.jet_fuel == 20000 and out.restored.one.jet_fuel == 10000,
    'read: the sets are put back ' .. text)
  S.ok('read: two pads read, stock set and put back')
end

do
  local out, text = eval('slots.lua', 'hook')
  assert(out.settings[tostring(farps['np-hg-1'].id)], text)
  assert(out['airbase_id_np-hg-2'] ~= nil, text)
  S.ok('slots: the FARP list and settings read')
end

do
  local out, text = eval('second.lua', 'server')
  assert(type(out) == 'table' and out.second_static ~= nil, 'second: ' .. text)
  S.ok('second: finished, second name read')
end

do
  local out, text = eval('parking.lua', 'server', 'np-hg-1')
  assert(type(out.parking) == 'table' and #out.parking >= 1, 'parking: ' .. text)
  S.ok('parking: ' .. #out.parking .. ' spot(s) of np-hg-1 read')
end

for _, try in ipairs({ 'group-static', 'static-units', 'same-group', 'group-ground', 'group-dynamic' }) do
  local out, text = eval('make.lua', 'server', try)
  assert(out.try == try and out.names, text)
  S.ok('make ' .. try .. ': airbases ' .. tostring(out.airbases_before) .. ' -> ' .. tostring(out.airbases_after))
end
