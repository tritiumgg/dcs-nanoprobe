-- Clear-debris person test: does a static's ruin disappear on a client after
-- `world.removeJunk` runs over it? A person at the client watches the static.
-- ACTION (filled by the caller with sed):
--   spawn: add a `FARP Fuel Depot` static, `hidden = false`, DISTANCE m ahead
--          of the first player unit's position (its heading), named np-ruin;
--   kill:  an explosion of power POWER at np-ruin, kept small so the player's
--          own aircraft is out of its reach;
--   clear: `world.removeJunk` with a RADIUS m sphere on np-ruin's point;
--   destroy: `Object.destroy` on the dead np-ruin, when found by name;
--   read:  np-ruin and the player unit's life, changing nothing.
-- Each action reads np-ruin: whether `getByName` finds it, isExist, life and
-- point. `clear` also records the count
-- removeJunk gives, on the point `spawn` kept in a server-state global.
-- Rule: recorded as the person at the client reports it.
-- states: server
-- timeout: 30

local E = ...
local ACTION = '<action>'
local NAME = 'np-ruin'
local DISTANCE = tonumber('<distance>') or 300
local POWER = tonumber('<power>') or 200
local RADIUS = tonumber('<radius>') or 30

local function player()
  for _, side in ipairs({ coalition.side.BLUE, coalition.side.RED }) do
    local u = (coalition.getPlayers(side) or {})[1]
    if u then
      return u
    end
  end
end

local function read(label)
  local s = StaticObject.getByName(NAME)
  local out = { found = s ~= nil, exists = s ~= nil and s:isExist() }
  if s then
    local okl, life = pcall(s.getLife, s)
    out.life = okl and life or nil
    local okp, p = pcall(s.getPoint, s)
    out.point = okp and { x = math.floor(p.x), z = math.floor(p.z) } or nil
  end
  E.observe(label, out, { info = true })
  return s
end

if ACTION == 'spawn' then
  local u = player()
  if not u then
    error('no player unit')
  end
  local pos = u:getPosition()
  local x = pos.p.x + pos.x.x * DISTANCE
  local z = pos.p.z + pos.x.z * DISTANCE
  local ok, err = pcall(coalition.addStaticObject, u:getCountry(), {
    category = 'Fortifications', type = 'FARP Fuel Depot', name = NAME,
    x = x, y = z, heading = 0, hidden = false,
  })
  -- Kept for `clear`: a destroyed static may no longer be found by name.
  rawset(_G, 'np_ruin_point', { x = x, y = land.getHeight({ x = x, y = z }), z = z })
  E.observe('spawn', { ok = ok, error = (not ok) and tostring(err) or nil, player = u:getTypeName() }, { info = true })
  read('after_spawn')
elseif ACTION == 'kill' then
  local s = read('before_kill')
  if not s then
    error('np-ruin not found')
  end
  trigger.action.explosion(s:getPoint(), POWER)
elseif ACTION == 'clear' then
  local p = rawget(_G, 'np_ruin_point')
  if not p then
    error('no np-ruin point; run spawn first')
  end
  read('before_clear')
  local n = world.removeJunk({ id = world.VolumeType.SPHERE, params = { point = p, radius = RADIUS } })
  E.observe('removed', n, { info = true })
  read('after_clear')
elseif ACTION == 'destroy' then
  local s = read('before_destroy')
  local ok, err = false, 'np-ruin not found by name'
  if s then
    ok, err = pcall(s.destroy, s)
  end
  E.observe('destroyed', { ok = ok, error = (not ok) and tostring(err) or nil }, { info = true })
  read('after_destroy')
elseif ACTION == 'read' then
  read('now')
  local u = player()
  E.observe('player_life', u and u:getLife() or -1, { info = true })
else
  error('unknown action ' .. ACTION)
end
return 'done'
