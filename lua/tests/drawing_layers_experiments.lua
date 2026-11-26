-- The drawing-layers snippets of lua/experiments/drawing-layers under the strict
-- mock: each finishes in its state, returns its reads and leaves no drawing.
-- The mock records setMarkup* changes and lists panels by `id`; it does not
-- reproduce DCS's order or what a change does to a client, so the order and the
-- in-place updates are checked to run, not to match DCS; what DCS does is in
-- docs/results/2026-11-25-drawing-layers.md. The live part's PANELS snippet
-- (crates/xtask/src/shell_live/drawing_layers.rs) runs here too.
-- cam.lua is not run: the mock has no Export.LoGetCameraPosition or
-- LoSetCameraPosition.

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
local function eval(file, state, step, key)
  local source = S.read(root .. 'lua/experiments/drawing-layers/' .. file)
  assert(source:find('%-%- states: ' .. state .. '\n'), file .. ' names the ' .. state .. ' state')
  if type(step) == 'table' then
    for k, v in pairs(step) do
      source = source:gsub('<' .. k .. '>', v)
    end
  elseif step then
    source = source:gsub('<' .. (key or 'step') .. '>', step)
  end
  serial = serial + 1
  local id = 'dl' .. serial
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

local function left()
  local n = 0
  for id in pairs(host.sim.marks) do
    if id >= 900000000 then
      n = n + 1
    end
  end
  return n
end

do
  local out, text = eval('order.lua', 'server')
  assert(#out.steps == 8, 'order: eight steps ' .. text)
  local first = out.steps[1]
  assert(#first.results == 3 and first.results[1] == 'ok', text)
  assert(#first.after.order == 3, 'order: three drawings listed ' .. text)
  assert(out.removed and #out.removed.order == 0, 'order: all removed ' .. text)
  assert(#out.events > 0, 'order: events seen ' .. text)
  assert(left() == 0, 'order: no drawing left')
  S.ok('order: eight steps read, events ' .. #out.events .. ', nothing left')
end

do
  local out, text = eval('points.lua', 'server')
  assert(#out == 11 and out[1].points == 3 and out[1].ok and out[1].panel, 'points: ' .. text)
  assert(left() == 0, 'points: no drawing left')
  S.ok('points: 11 sizes tried, largest ok ' .. tostring(out[11].ok))
end

do
  for _, step in ipairs({ 'place', 'readd', 'newid', 'over', 'inplace', 'geometry', 'flicker', 'highids', 'watch' }) do
    local out, text = eval('client.lua', 'server', step)
    assert(out.step == step and type(out.order) == 'table', step .. ': ' .. text)
    if step == 'readd' then
      for _, key in ipairs({ 'readd_now', 'readd_later' }) do
        local r = out[key]
        -- DCS shows nothing for an add under an ID removed earlier in the process
        -- (LIVE); the mock does the same, so six of the seven panels are listed.
        assert(r and not r.exists and not r.text_ok and r.count == 6, 'readd: ' .. key .. ' ' .. text)
      end
    end
    if step == 'flicker' then
      -- Ten cycles: each swaps the ID, so the last quad is 70 and the earlier IDs are gone.
      assert(out.cycles == 10, 'flicker: ten cycles ' .. text)
      assert(host.sim.marks[900000000 + 1000 + 70], 'flicker: the last ID is held ' .. text)
      assert(not host.sim.marks[900000000 + 1000 + 69] and not host.sim.marks[900000000 + 1000 + 51],
        'flicker: earlier IDs are gone ' .. text)
    end
    if step == 'inplace' then
      for _, id in ipairs({ 51, 53 }) do
        local set = host.sim.marks[900000000 + 1000 + id].set
        assert(set and set.color and set.fill and set.line_type and set.font_size, 'inplace: setters recorded for ' .. id .. ' ' .. text)
      end
      assert(host.sim.marks[900000000 + 1000 + 53].text == 'np changed', 'inplace: the text changed')
    end
  end
  local out, text = eval('client.lua', 'server', 'read')
  assert(type(out.marks) == 'table' and type(out.panels) == 'table', 'read: ' .. text)
  -- The re-added line (ID 10) and the add over a held ID show nothing, as in DCS.
  -- Held: 5, 20, 50, 52, 53 and the last flicker quad, 70; and four high IDs.
  assert(#out.order == 6 and left() == 10, 'client: six drawings held before clear ' .. text)
  local cleared = eval('client.lua', 'server', 'clear')
  assert(left() == 0, 'client: no drawing left')
  assert(cleared.round == 1, 'client: the round counter moved on')
  -- The next place takes the round's own block of IDs, so every add draws again.
  local again, again_text = eval('client.lua', 'server', 'place')
  assert(#again.order == 7 and host.sim.marks[900000000 + 2000 + 52], 'client: round 1 draws ' .. again_text)
  eval('client.lua', 'server', 'clear')
  assert(left() == 0, 'client: no drawing left after round 1')
  S.ok('client: every step ran, a new round drew, nothing left')
end

do
  -- shapes, kinds, styles and loadfree: a first run draws, a second removes. The mock
  -- takes rectToAll, textToAll, circleToAll and markupToAll of any shape.
  -- loadfree: no load runs between the client run and loadfree, so the mock keeps
  -- the two IDs the client run removed dead, and one of three holds a panel.
  local expected = { shapes = 10, kinds = 6, styles = 24, loadfree = 1 }
  for _, name in ipairs({ 'shapes', 'kinds', 'styles', 'loadfree' }) do
    local out, text = eval(name .. '.lua', 'server')
    assert(left() == expected[name], name .. ': drawings held, ' .. left() .. ' ' .. text)
    if name == 'loadfree' then
      assert(out.held['900009999'] and not out.held['900001010'] and not out.held['900001030'],
        'loadfree: only the never-used ID holds a panel ' .. text)
    end
    if name == 'shapes' then
      for _, row in ipairs(out) do
        assert(row.ok, 'shapes: ' .. row.name .. ' ' .. text)
      end
    end
    local gone, gone_text = eval(name .. '.lua', 'server')
    assert(gone.removed, name .. ': second run removes ' .. gone_text)
    assert(left() == 0, name .. ': no drawing left')
  end
  local out, text = eval('panels.lua', 'server')
  assert(type(out.panels) == 'table', 'panels: ' .. text)
  S.ok('shapes, kinds, styles, loadfree, panels: ran, nothing left')
end

do
  -- partc: every step runs, list names the setters, sweep and rsweep take 11 steps per
  -- bar, clear leaves nothing. The mock shows no drawing; what DCS draws is read by a person.
  local function id(round, n)
    return 900020000 + 1000 * round + n
  end
  local list, list_text = eval('partc.lua', 'server', 'list')
  assert(#list.trigger_action > 0 and #list.world > 0 and #list.drawing > 0, 'partc: list ' .. list_text)
  for _, step in ipairs({ 'draw', 'move', 'refresh', 'other', 'bars' }) do
    local out, text = eval('partc.lua', 'server', step)
    assert(out.step == step, 'partc: ' .. text)
    for _, row in ipairs(out.timings or {}) do
      assert(not row.error and row.us >= 0, 'partc: ' .. step .. ' ' .. text)
    end
    if step == 'draw' then
      assert(out.held.Q1 and out.held.P1 and #out.panels == 20, 'partc: draw holds 10 drawings and 10 labels ' .. text)
    end
    if step == 'other' then
      assert(#out.tried >= 1 and out.tried[1].ok, 'partc: other tried a setter ' .. text)
    end
  end
  local sweep, sweep_text = eval('partc.lua', 'server', 'sweep')
  assert(sweep.step_count == 11, 'partc: sweep steps ' .. sweep_text)
  for _, name in ipairs({ 'SEG', 'FLUID', 'READD' }) do
    assert(#sweep.steps[name] == 11 and sweep.cost[name].steps1to10_us.n == 10, 'partc: sweep ' .. name .. ' ' .. sweep_text)
  end
  assert(#sweep.steps.FLUID[2].calls == 3, 'partc: FLUID makes three calls a step')
  -- READD: a fresh ID at every step and the old one removed, so one rectangle is held.
  local readd = 0
  for _, mark in pairs(host.sim.marks) do
    readd = readd + (mark.drawing and 1 or 0)
  end
  assert(readd > 0)
  for _, step in ipairs({ 'shapes2', 'rbars' }) do
    local out, text = eval('partc.lua', 'server', step)
    assert(out.step == step, 'partc: ' .. text)
    if step == 'shapes2' then
      assert(#out.shape2 == 4, 'partc: four shape 2 layouts ' .. text)
    end
  end
  local rs, rs_text = eval('partc.lua', 'server', 'rsweep')
  assert(rs.step_count == 11, 'partc: rsweep steps ' .. rs_text)
  for _, name in ipairs({ 'FLAT', 'RND-C', 'RND-P' }) do
    assert(#rs.steps[name] == 11, 'partc: rsweep ' .. name .. ' ' .. rs_text)
  end
  assert(rs.rndp_position_end and not rs.rndp_position_end.call.error, 'partc: position end tried ' .. rs_text)
  local half, half_text = eval('partc.lua', 'server', 'rhalf')
  for _, name in ipairs({ 'FLAT', 'RND-C', 'RND-P' }) do
    assert(half.half[name] and #half.half[name] >= 1, 'partc: rhalf ' .. name .. ' ' .. half_text)
  end
  local cleared = eval('partc.lua', 'server', 'clear')
  assert(left() == 0 and cleared.round == 1, 'partc: no drawing left')
  eval('partc.lua', 'server', 'draw')
  eval('partc.lua', 'server', 'clear')
  assert(left() == 0, 'partc: no drawing left after a second round')
  S.ok('partc: every step ran, sweeps took 11 steps, nothing left')
end

do
  -- corners: four shape-3 drawings and four labels; a second run removes them.
  local out, text = eval('corners.lua', 'server')
  assert(#out == 8 and left() == 8, 'corners: ' .. text)
  for _, row in ipairs(out) do
    assert(row.ok, 'corners: ' .. row.name .. ' ' .. text)
  end
  assert(eval('corners.lua', 'server').removed and left() == 0, 'corners: no drawing left')
  -- unknown: one case per run, then clear. The mock takes any shape; DCS may crash.
  for _, case in ipairs({ { shape = 8, layout = 'oval' }, { shape = 12, layout = 'points' } }) do
    local one, one_text = eval('unknown.lua', 'server', case)
    assert(one.shape == case.shape and one.layout == case.layout and one.ok and one.panel, 'unknown: ' .. one_text)
  end
  assert(left() == 4, 'unknown: two cases and two labels held')
  local cleared, cleared_text = eval('unknown.lua', 'server', { shape = 0, layout = 'clear' })
  assert(cleared.removed == 4 and left() == 0, 'unknown: clear ' .. cleared_text)
  local drawn, drawn_text = eval('startbar.lua', 'server', 'draw')
  assert(#drawn.timings == 2 and not drawn.timings[1].error and not drawn.timings[2].error, 'startbar: ' .. drawn_text)
  assert(drawn.sr_points == 33 and left() == 4, 'startbar: draw ' .. drawn_text)
  for _, pct in ipairs({ '25', '50', '75' }) do
    local moved, moved_text = eval('startbar.lua', 'server', pct)
    assert(#moved.timings == 2 and not moved.timings[1].error and not moved.timings[2].error, 'startbar: ' .. moved_text)
  end
  assert(eval('startbar.lua', 'server', 'clear').removed == 4 and left() == 0, 'startbar: clear')
  -- newproc: the mock keeps a removed ID dead (as one DCS process does) and has no
  -- restart, so `before` shows the re-add ignored and `after` runs.
  local before, before_text = eval('newproc.lua', 'server', 'before')
  assert(before.added and before.panel_after_add and not before.panel_after_remove, 'newproc: ' .. before_text)
  assert(before.readded and not before.panel_after_readd, 'newproc: a removed ID is dead ' .. before_text)
  local after, after_text = eval('newproc.lua', 'server', 'after')
  assert(after.added and after.removed and left() == 0, 'newproc: ' .. after_text)
  -- startfluid: a first run draws three bars and three labels, a second removes them.
  eval('startfluid.lua', 'server')
  assert(left() == 6, 'startfluid: three bars and three labels, ' .. left())
  assert(eval('startfluid.lua', 'server').removed and left() == 0, 'startfluid: no drawing left')
  -- shape3: kept as the record of a crashed run; it refuses to run and calls nothing.
  local calls = 0
  local real = host.states.server.trigger.action.markupToAll
  host.states.server.trigger.action.markupToAll = function(...)
    calls = calls + 1
    return real(...)
  end
  local s3, s3_text = eval('shape3.lua', 'server')
  host.states.server.trigger.action.markupToAll = real
  assert(s3.error and calls == 0 and left() == 0, 'shape3: refuses to run ' .. s3_text)
  S.ok('corners, unknown, startbar, startfluid, shape3: ran one case per run, nothing left')
end

do
  local out, text = eval('ids.lua', 'server')
  assert(#out == 8 and out[1].drawing.ok and out[1].drawing.added.exact, 'ids: ' .. text)
  assert(left() == 0, 'ids: no drawing left')
  S.ok('ids: 8 IDs tried')
end

do
  local out, text = eval('reid.lua', 'server')
  assert(#out.a == 4 and #out.b == 4 and #out.c == 3 and #out.d == 3, 'reid: cases read ' .. text)
  -- An add under a removed ID shows no panel (LIVE); an add under a fresh ID does.
  assert(out.a[1].panel.exists and not out.a[2].panel.exists and not out.a[3].panel.exists, 'reid: a ' .. text)
  assert(not out.b[3].panel.exists, 'reid: b ' .. text)
  assert(not out.d[3].panel.exists, 'reid: d ' .. text)
  assert(out.c[3].panel.exists and not out.c[3].panel_C.exists, 'reid: c ' .. text)
  assert(#out.mark_later == 2 and out.events.A.added >= 1, 'reid: later and events ' .. text)
  for name, row in pairs(out.final) do
    assert(not row.exists, 'reid: ' .. name .. ' left ' .. text)
  end
  assert(left() == 0, 'reid: no drawing left')
  S.ok('reid: five cases read, nothing left')
end

do
  local out, text = eval('bench.lua', 'server')
  for _, key in ipairs({ 'readd_rect', 'readd_free_100', 'readd_free_1000', 'set_color', 'remove_absent' }) do
    assert(out[key], 'bench: ' .. key .. ' ' .. text)
  end
  assert(left() == 0, 'bench: no drawing left')
  S.ok('bench: five cases timed')
end

do
  -- The live part's PANELS snippet, read out of the Rust source, against drawings
  -- the mock holds: it lists the panels from 2^30 by ID, in ascending order, with
  -- their position, and counts every panel.
  local rust = S.read(root .. 'crates/xtask/src/shell_live/drawing_layers.rs')
  local snippet = rust:match('const PANELS: &str = r#"(.-)"#;')
  assert(snippet, 'panels: the PANELS snippet is in the Rust source')
  local function run(source)
    serial = serial + 1
    local id = 'dl' .. serial
    S.write(base .. 'in/' .. id .. '.server.lua', source)
    for _ = 1, 20 * 60 do
      host:frame(1 / 60)
      local text = S.read(base .. 'out/' .. id .. '.json')
      if text then
        local result = S.json(text)
        assert(result.ok and result.reply.ok, 'panels: ' .. text)
        return result.reply.values[1], text
      end
    end
    error('panels: no result')
  end
  run([[
local function p(x, z) return { x = x, y = 0, z = z } end
for _, id in ipairs({ 1073741825, 1073741824, 5 }) do
  trigger.action.markupToAll(3, -1, id, p(1000, 2000), p(1400, 2400), { 1, 0, 0, 1 }, { 1, 0, 0, 0.2 }, 1, true, '')
end
return true
]])
  local out, text = run('local E = ...\n' .. snippet)
  assert(out.total == 3, 'panels: three held ' .. text)
  assert(#out.panels == 2, 'panels: two from 2^30 ' .. text)
  assert(out.panels[1].idx == 1073741824 and out.panels[2].idx == 1073741825, 'panels: ascending by ID ' .. text)
  assert(out.panels[1].x and out.panels[1].z, 'panels: positions read ' .. text)
  run([[
for _, id in ipairs({ 1073741825, 1073741824, 5 }) do trigger.action.removeMark(id) end
return true
]])
  local gone = run('local E = ...\n' .. snippet)
  assert(gone.total == 0 and #gone.panels == 0, 'panels: none left')
  assert(left() == 0, 'panels: no drawing left')
  S.ok('panels: the live snippet lists 2 of 3 panels by ID')
end

do
  local out, text = eval('reload.lua', 'hook')
  assert(out.paused and out.model < 5, 'reload: paused at the start ' .. text)
  S.ok('reload: loaded again and paused')
end

do
  -- shot.lua lists the folder, sets the extension without a dot and takes one
  -- screenshot; shots.lua then lists it, and with a later <since> lists none.
  local shot, text = eval('shot.lua', 'hook')
  assert(shot.ok and shot.ext_ok and #host.screenshots == 1, 'shot: one call ' .. text)
  assert(host.shot_ext == 'png', 'shot: extension given without a dot')
  assert(type(shot.before) == 'number' and #shot.called == 2, 'shot: reported ' .. text)
  local all = eval('shots.lua', 'hook')
  assert(not all.error and #all.files == 1, 'shots: one file ')
  assert(all.files[1].name == 'Screen_1.png' and all.files[1].size == 4, 'shots: name and size')
  local none = eval('shots.lua', 'hook', tostring(os.time() + 1000), 'since')
  assert(#none.files == 0, 'shots: since filters')
  S.ok('shot, shots: one screenshot taken and listed')
end
