-- The soak control's frame rate (lua/sampling/fps-control.lua) under the
-- strict mock, through the eval channel: with no agent loaded, it counts one
-- frame per poll of its wait, so 2 s at 60 fps and then 2 s at 30 fps read
-- 45 fps over the window, a 5th percentile near 30, and model time equal to
-- real time.

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
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = false })

local source = S.read(root .. 'lua/sampling/fps-control.lua'):gsub('@MEASURE@', '4')
S.write(base .. 'in/fps.hook.lua', source)

-- Frames at 60 fps until the snippet has seen 2 s, then at 30 fps.
local result
local start = host.real
for _ = 1, 60 * 60 do
  host:frame(host.real - start < 2.3 and 1 / 60 or 1 / 30)
  local text = S.read(base .. 'out/fps.json')
  if text then
    result = S.json(text)
    break
  end
end
assert(result, 'no result')
assert(result.ok and result.reply.ok, tostring(result.reply and result.reply.error))
local v = result.reply.values[1]
assert(v.windows >= 3 and v.windows <= 4, 'windows ' .. v.windows)
assert(v.fps > 40 and v.fps < 50, 'fps ' .. v.fps)
assert(v.p5_fps > 28 and v.p5_fps < 33, 'p5 ' .. v.p5_fps)
assert(math.abs(v.model_per_real - 1) < 0.02, 'model per real ' .. v.model_per_real)
-- Each frame of the request polls the snippet once: its count matches the
-- hook's own frame count of the request, within the frames before the
-- snippet's first call.
local counted = v.fps * 4
assert(math.abs(result.hook.frames - counted) <= 3, 'hook frames ' .. result.hook.frames .. ', counted ' .. counted)
-- No agent was opened: the profile holds no agent log.
assert(not S.read(profile .. 'nanoprobe/logs/agent.log'), 'an agent log was written')

print('ok    the control counts frames without the agent: ' .. string.format('%.1f fps, p5 %.1f', v.fps, v.p5_fps))
