-- The S15 experiment snippets in lua/experiments/s15 under the strict mock:
-- each runs in the state its header names and finishes, records the
-- observations its rule needs, and after uninstall nothing is left in any
-- state.
--
-- The mock has no trigger zone, no waypoint Script action, no warehouse stock, no per-type unit database, no AI (no
-- shot, no attack) and no honoring of a unit's `ammo` table; the snippets
-- record each refusal as an observation instead of stopping. This test
-- proves the snippets run and record, and prints the refusals it saw, which
-- are the mock's gaps. What the experiments found is in docs/results.

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
for _, lib in ipairs({ 'scenario', 's15', 'routes' }) do
  S.copy(root .. 'lua/scenarios/' .. lib .. '.lua', profile .. 'nanoprobe/dev/lua/' .. lib .. '.lua')
end

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })

local function eval(id, state, source, seconds)
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, seconds * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      return S.json(text), text
    end
  end
  error(id .. ' in ' .. state .. ': no result in ' .. seconds .. ' s')
end

-- file, state, seconds to wait.
local SNIPPETS = {
  { 'e06-warehouse.lua', 'server', 20 },
  { 'e14-ammo.lua', 'server', 400 },
  { 'e16-damage.lua', 'hook', 60 },
  { 'e08-capture.lua', 'hook', 60 },
  { 'e08-attack.lua', 'server', 200 },
  { 'e21b-names.lua', 'server', 60 },
  { 'e-sound-picture.lua', 'hook', 90 },
  { 'e-zones.lua', 'hook', 300 },
  { 'e-waypoint-scripts.lua', 'hook', 300 },
}

-- Observations each snippet must record.
local REQUIRED = {
  ['e06-warehouse.lua'] = { 'airbases', 'chosen_airbase', 'warehouse_methods', 'item_weapon', 'item_aircraft',
    'liquid_0', 'liquid_1', 'liquid_2', 'liquid_3' },
  ['e14-ammo.lua'] = { 'type_1_T-72B', 'type_2_M-1 Abrams', 'type_3_BMP-2', 'type_4_M-2 Bradley', 'type_5_ZSU-23-4 Shilka',
    'type_6_Osa 9A33 ln', 'type_7_Leopard-2', 'type_8_BTR-80', 'type_9_M1126 Stryker ICV', 'type_10_Ural-375 ZU-23', 'summary' },
  ['e16-damage.lua'] = { 'spawned', 'ids', 'before', 'set_50', 'after_50_plus_1_s', 'set_0', 'after_0_plus_1_s',
    'after_0_plus_3_s', 'after_0_plus_10_s', 'events', 'cleared' },
  ['e08-capture.lua'] = { 'spawned', 'ids', 'damage_set', 'captured', 'reads_for_flares_chaff_pylons', 'groups_after_destroy',
    'respawned', 'life_restored', 'captured_again', 'comparison', 'cleared' },
  ['e08-attack.lua'] = { 'spawned', 'orders', 'first_task', 'first_phase', 're_added', 'control_phase', 'reissued_task',
    'reissue_phase', 'summary', 'events', 'cleared' },
  -- The mock models no zone, no waypoint Script action, no `outSoundStop`
  -- and no picture action: e-zones and e-waypoint-scripts here prove only
  -- that the steps run and record (their zone, flag and Script-line
  -- observations hold the mock's refusals or zero counts), and
  -- e-sound-picture records the mock's refusals for the picture calls.
  -- What fires, when and at what cost is settled on a running DCS.
  ['e21b-names.lua'] = { 'cases', 'case_pairs', 'failures', 'events', 'cleared', 'leftovers' },
  ['e-sound-picture.lua'] = { 'profile_file_written', 'sound', 'picture_functions_exist', 'resource_by_key',
    'picture_targets', 'picture_calls', 'profile_file_removed' },
  ['e-zones.lua'] = { 'start_paused', 'init', 'spawn', 'pause_confirmed_in_hook', 'getZone_while_paused',
    'read_10s', 'events_and_cleanup' },
  ['e-waypoint-scripts.lua'] = { 'start_paused', 'init', 'spawn', 'read_5s', 'read_22s_before_pause',
    'pause_in_hook_3s_after', 'read_paused_a', 'read_resumed_10s', 'reroute', 'read_after_reroute_10s',
    'mission_state_table', 'log_events_cleanup' },
}

-- Phrases that mark an observation as a refusal by the mock.
local GAPS = {}
local function scan(file, name, text)
  for phrase in text:gmatch('mock: [^"\\]+') do
    GAPS[file .. ' / ' .. phrase:sub(1, 90)] = true
  end
  for phrase in text:gmatch('[%w%.:_]+ is in the DCS API but not mocked') do
    GAPS[file .. ' / ' .. phrase] = true
  end
end

for serial, probe in ipairs(SNIPPETS) do
  local file, state, seconds = probe[1], probe[2], probe[3]
  local source = S.read(root .. 'lua/experiments/s15/' .. file)
  assert((source:match('%-%- states: ([%a ]+)\n') or ''):find(state, 1, true), file .. ' does not name the state ' .. state)
  local result, text = eval('s15x' .. serial, state, source, seconds)
  assert(result.ok, file .. ': ' .. tostring(result.error))
  assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error))
  local seen = {}
  for _, o in ipairs(result.reply.observations) do
    seen[o.name] = o
  end
  for _, name in ipairs(REQUIRED[file]) do
    assert(seen[name], file .. ': no observation named ' .. name)
  end
  scan(file, '', text)
  -- S15_DUMP=1 prints what each snippet recorded, for reading the rehearsal.
  if os.getenv('S15_DUMP') then
    io.write('--- ', file, '\n')
    for _, o in ipairs(result.reply.observations) do
      local value = o.value
      if type(value) == 'table' then
        value = text:match('"name":"' .. o.name .. '","value":(%b{})') or '<table>'
      end
      io.write('  ', o.name, ' = ', tostring(value):sub(1, 400), '\n')
    end
  end
  S.ok(string.format('%s@%s: %d observation(s)', file, state, #result.reply.observations))
end
assert(#host.timer_errors == 0, 'a timer raised: ' .. tostring(host.timer_errors[1]))

-- E8 compares a unit with itself: whatever the mock does, the comparison
-- of an aircraft it can respawn must name every field.
do
  local result = eval('s15cmp', 'hook', S.read(root .. 'lua/experiments/s15/e08-capture.lua'), 60)
  local comparison
  for _, o in ipairs(result.reply.observations) do
    if o.name == 'comparison' then
      comparison = o.value
    end
  end
  assert(type(comparison) == 'string' and comparison:find('np-e08-air', 1, true), 'no comparison of the aircraft: ' .. tostring(comparison))
  for _, field in ipairs({ 'name', 'type', 'id', 'horizontal_move_m', 'y', 'heading', 'pitch', 'roll', 'life', 'life0', 'fuel_kg', 'ammo_rows' }) do
    assert(comparison:find('"' .. field .. '"', 1, true), 'the comparison has no ' .. field)
  end
  S.ok('e08-capture.lua: the comparison names every field')
end

-- The mock gaps the snippets met.
local gaps = {}
for gap in pairs(GAPS) do
  gaps[#gaps + 1] = gap
end
table.sort(gaps)
for _, gap in ipairs(gaps) do
  io.write('  mock gap: ', gap, '\n')
end

-- Nothing is left behind.
local result = eval('cleanup', 'uninstall', '', 10)
assert(result.ok, tostring(result.error))
for _, report in ipairs(result.reply) do
  assert(#report.errors == 0, report.state .. ': ' .. tostring(report.errors[1]))
  assert(#report.new_globals == 0, report.state .. ' kept the global ' .. tostring(report.new_globals[1]))
  assert(report.handlers == S.null or report.handlers == 0, report.state .. ' kept an event handler')
end
for _ = 1, 5 do
  host:frame(1 / 60)
end
assert(#host.handlers == 0 and host:timer_count() == 0, 'a handler or a timer is left')
assert(next(host.sim.groups) == nil and next(host.sim.statics) == nil, 'a group or a static is left')
S.ok('the S15 experiment snippets run under the mock and leave nothing after uninstall')

S.remove(profile)
