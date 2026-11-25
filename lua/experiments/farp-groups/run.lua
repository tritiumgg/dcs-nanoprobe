-- FARP groups, part 4: resume the mission; after 3 s of model time, do the
-- replace tries on r3, r2 and r1 while it runs, and set off an explosion at
-- the place of d2's second FARP; pause after 10 s more. `after.lua` reads.
-- Run with `cargo xtask eval <target> hook`. Its wait does not ask for
-- `running`, so the eval hook paused the mission again while it waited: in
-- run 1 the mission ran about 2 s after the explosion, the call timed out on
-- the Mac at 30 s, and `settle.lua` ran the 10 s.
-- Rule: recorded as read.
-- states: hook
-- timeout: 90

local E = ...

local function cross(source)
  local ok, value = E.cross('server', source)
  if not ok then
    error('server: ' .. tostring(value))
  end
  return value
end

if #net.get_player_list() > 1 then
  error('players connected')
end
-- The server part stores its reads in the global, for `after.lua`.
local RUNNING = [[
  local G = _G.np_farp_groups
  local r = { model = timer.getTime() }
  for _, tag in ipairs({ 'r3', 'r2', 'r1' }) do
    local ok, v = pcall(G.replace, tag)
    r[tag] = ok and v or ('raised: ' .. tostring(v))
  end
  local at = G.places['np-fg-d2-2']
  local p = { x = at.x, y = land.getHeight({ x = at.x, y = at.z }), z = at.z }
  local ok, v = pcall(trigger.action.explosion, p, 3000)
  r.explosion = ok and 'returned' or ('raised: ' .. tostring(v))
  G.running = r
  return 'done'
]]
local out = { paused_before = DCS.getPause(), model_before = DCS.getModelTime() }
local step, t0 = 0, nil
return E.wait(function()
  local now = DCS.getModelTime()
  if step == 0 then
    DCS.setPause(false)
    t0, step = now, 1
  elseif step == 1 and now >= t0 + 3 then
    -- Nobody but the server before the damage death.
    if #net.get_player_list() > 1 then
      DCS.setPause(true)
      error('players connected before the explosion')
    end
    out.running = cross(RUNNING)
    t0, step = now, 2
  elseif step == 2 and now >= t0 + 10 then
    DCS.setPause(true)
    step = 3
  elseif step == 3 and DCS.getPause() then
    out.paused, out.model = true, now
    return out
  end
end, 80)
