-- The E39 snippets in lua/experiments/e39 under the strict mock: each runs
-- in the state its header names and finishes, records the observations its
-- rule needs, and after uninstall nothing is left in any state.
--
-- The mock has no unit database (`db.Units`), no launcher function, no
-- `a_unit_set_life_percentage` that changes life, and does not honor a
-- unit's `ammo` table or payload; the snippets record each gap as an
-- observation instead of stopping. This proves the snippets run and record.
-- What the experiment found is in docs/results.

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

-- file, state, seconds to wait, observations it must record.
local SNIPPETS = {
  { 'recon.lua', 'server', 30, { 'apis', 'def_1_M-113', 'def_10_F-15C', 'added', 'read_1_M-113', 'read_10_F-15C' } },
  { 'spawn.lua', 'hook', 120, { 'start', 'spawned', 'ids', 'layouts', 'same_frame_life', 'read_same_frame',
    'read_paused_1_s', 'paused_life', 'read_paused_2_s', 'read_running_1_s', 'running_life', 'read_life_1_s',
    'read_life_5_s', 'cleared' } },
}

for serial, probe in ipairs(SNIPPETS) do
  local file, state, seconds, required = probe[1], probe[2], probe[3], probe[4]
  local source = S.read(root .. 'lua/experiments/e39/' .. file)
  assert((source:match('%-%- states: ([%a ]+)\n') or ''):find(state, 1, true), file .. ' does not name the state ' .. state)
  local result, text = eval('e39x' .. serial, state, source, seconds)
  assert(result.ok, file .. ': ' .. tostring(result.error))
  assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error))
  local seen = {}
  for _, o in ipairs(result.reply.observations) do
    seen[o.name] = o
  end
  for _, name in ipairs(required) do
    assert(seen[name], file .. ': no observation named ' .. name)
  end
  -- E39_DUMP=1 prints what each snippet recorded.
  if os.getenv('E39_DUMP') then
    io.write('--- ', file, '\n', text:sub(1, 6000), '\n')
  end
  S.ok(string.format('%s@%s: %d observation(s)', file, state, #result.reply.observations))
end
assert(#host.timer_errors == 0, 'a timer raised: ' .. tostring(host.timer_errors[1]))

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
S.ok('the E39 snippets run under the mock and leave nothing after uninstall')

S.remove(profile)
