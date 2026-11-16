-- The E-junk snippet (lua/experiments/junk/junk.lua) under the strict mock:
-- it runs in the `server` state, finishes, records every observation its rule
-- needs, and after uninstall nothing is left in any state. The mock models no
-- aircraft crash, scenery or crater, so those observations hold the mock's
-- answers; what DCS does is in docs/results/2026-11-16-clear-debris.md.

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
for _, lib in ipairs({ 'scenario', 's15' }) do
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

local source = S.read(root .. 'lua/experiments/junk/junk.lua')
assert(source:match('%-%- states: server\n'), 'junk.lua does not name the server state')
local result, text = eval('junk1', 'server', source, 200)
assert(result.ok, tostring(result.error))
assert(result.reply.ok, tostring(result.reply.error))
local seen = {}
for _, o in ipairs(result.reply.observations) do
  seen[o.name] = o
end
for _, name in ipairs({ 'removeJunk_type', 'volume_types', 'base_offset', 'spawned', 'call_empty_small',
  'call_empty_map', 'read_alive', 'read_burning_before', 'call_burning_sphere', 'read_burning_after', 'read_before',
  'call_many_one', 'call_sphere', 'call_box', 'call_segment', 'call_pyramid', 'call_static', 'read_after',
  'call_map_sphere', 'read_after_map', 'call_map_sphere_again', 'call_map_box', 'call_map_sphere_1e7', 'read_end',
  'events' }) do
  assert(seen[name], 'no observation named ' .. name)
end
assert(result.reply.values[1] == 'done', 'the snippet returned ' .. tostring(result.reply.values[1]))
-- The mock's removeJunk takes the wrecks of a sphere: the sphere cluster's
-- four, and not the live unit beside them.
local sphere = text:match('"name":"call_sphere","value":(%b{})')
assert(sphere and sphere:find('"value":4[,}]'), 'the sphere call did not count four wrecks: ' .. tostring(sphere))
local after = text:match('"name":"read_after_map","value":(%b{})') or ''
assert(after:find('"sphere_live":{"exist=true dead=false', 1, true), 'a clear removed the live unit: ' .. after)
if os.getenv('JUNK_DUMP') then
  for _, o in ipairs(result.reply.observations) do
    io.write(o.name, ' = ', text:match('"name":"' .. o.name .. '","value":(%b{})') or tostring(o.value), '\n')
  end
end
S.ok(string.format('junk.lua@server: %d observation(s)', #result.reply.observations))

-- E-junk2: per-wreck removal. The mock's destroy on a wreck takes it out, so
-- the destroyed subjects' spheres count nothing.
local result2, text2 = eval('junk2', 'server', S.read(root .. 'lua/experiments/junk/junk2.lua'), 400)
assert(result2.ok, tostring(result2.error))
assert(result2.reply.ok, tostring(result2.reply.error))
assert(result2.reply.values[1] == 'done', 'junk2.lua returned ' .. tostring(result2.reply.values[1]))
local seen2 = {}
for _, o in ipairs(result2.reply.observations) do
  seen2[o.name] = o
end
for _, name in ipairs({ 'read_alive', 'read_dead_1s', 'destroy_1s', 'read_after_destroy_1s', 'read_dead_300s',
  'census_statics_before', 'destroy_300s', 'read_after_destroy_300s', 'census_statics_after', 'sphere_counts',
  'small_counts', 'read_after_clear', 'events', 'cleanup' }) do
  assert(seen2[name], 'junk2.lua: no observation named ' .. name)
end
if os.getenv('JUNK_DUMP') then
  for _, o in ipairs(result2.reply.observations) do
    io.write(o.name, ' = ', text2:match('"name":"' .. o.name .. '","value":(%b{})') or tostring(o.value), '\n')
  end
end
S.ok(string.format('junk2.lua@server: %d observation(s)', #result2.reply.observations))

local report = eval('junkclean', 'uninstall', '', 10)
assert(report.ok, tostring(report.error))
for _, state in ipairs(report.reply) do
  assert(#state.errors == 0, state.state .. ': ' .. tostring(state.errors[1]))
end
assert(#host.timer_errors == 0, 'a timer raised: ' .. tostring(host.timer_errors[1]))
S.ok('uninstall leaves nothing')
