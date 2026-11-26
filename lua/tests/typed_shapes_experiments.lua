-- The typed-shapes snippets of lua/experiments/typed-shapes under the strict mock: each
-- step finishes in its state, every call returns without error, and clear leaves no
-- drawing. The mock does not draw, so what a client shows is in the results doc.

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

local serial = 0
-- `step` is put in as run.sh puts it.
local function eval(file, step)
  local source = S.read(root .. 'lua/experiments/typed-shapes/' .. file)
  assert(source:find('%-%- states: server\n'), file .. ' names the server state')
  source = source:gsub('<step>', step)
  serial = serial + 1
  local id = 'ts' .. serial
  S.write(base .. 'in/' .. id .. '.server.lua', source)
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

local function left()
  local n = 0
  for id in pairs(host.sim.marks) do
    if id >= 900000000 then
      n = n + 1
    end
  end
  return n
end

local function no_errors(out, text)
  for _, row in ipairs(out.timings) do
    assert(not row.error and row.us >= 0, 'rotated: ' .. row.call .. ' ' .. text)
  end
end

do
  local draw, text = eval('rotated.lua', 'draw')
  no_errors(draw, text)
  -- Eight drawings and eight labels.
  assert(#draw.timings == 16 and #draw.held == 8 and #draw.panels == 16, 'rotated: draw ' .. text)
  local points = {}
  for _, row in ipairs(draw.timings) do
    local name, n = row.call:match('^(%w+) shape %d %((%d+) points%)$')
    if name then
      points[name] = tonumber(n)
    end
  end
  assert(points.R0 == 4 and points.R30 == 4 and points.QK == 4 and points.F30 == 4, 'rotated: quads ' .. text)
  assert(points.OV == 36 and points.RR == 40 and points.RC == 38, 'rotated: point counts ' .. text)
  local radius, rtext = eval('rotated.lua', 'radius')
  no_errors(radius, rtext)
  assert(#radius.timings == 1, 'rotated: radius ' .. rtext)
  local clear, ctext = eval('rotated.lua', 'clear')
  assert(clear.cleared and #clear.panels == 0 and left() == 0, 'rotated: clear ' .. ctext)
  S.ok('rotated: 8 drawings and 8 labels, 36, 40 and 38 points, a radius change, nothing left')
end
