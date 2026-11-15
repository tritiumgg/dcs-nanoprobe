-- E8 (S15's part): a re-issued attack task. Rule: captured, restored and
-- compared; here the comparison is whether the attacker engages again.
--
-- Group A, three M-1 Abrams, is ordered to open fire and given an
-- AttackGroup task against group B, two T-72B 1,000 m away that are ordered
-- to hold fire (so B does not shoot back at A, and the shots counted are A's).
-- Phases, each in model seconds:
--   first    A attacks B                                       FIRST_S
--   gap      B is destroyed, and the name is free again        GAP_S
--   control  B is added again under the same name, with a new group ID, and
--            A's task is left as it was (it names B's old ID)  CONTROL_S
--   reissue  A's task is issued again with B's new group ID    REISSUE_S
-- The snippet counts, for each phase, the world events A's units caused:
-- S_EVENT_SHOT (a shot), SHOOTING_START, HIT and KILL, and records A's total
-- ammunition at the end of each phase, the group IDs, whether A has a task,
-- and B's living units. A engages when it shot in the phase. The same
-- terrain is not guaranteed to give A a line of sight to B: when A shot in
-- no phase, the first phase says so, and the rest does not decide anything.
-- The country of B is Russia, of A the USA; ROE option 0 is the ground
-- unit's (ED-CODE: Scripts/UI/RadioCommandDialogPanel/Config/groundUnit.lua),
-- value 2 open fire and 4 hold fire (UNVERIFIED: AI.Option.Ground.val.ROE).
-- states: server
-- timeout: 240

local E = ...
local X = E.lib('s15')
local S = X.S

local FIRST_S, GAP_S, CONTROL_S, REISSUE_S = 45, 3, 15, 45
local DISTANCE_M = 1000
local ANY_WEAPON = 1073741822

local origin = S.origin()
local x0, z0 = origin.x + 3000, origin.z + 12000 + 1200

local names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then
    names[id] = name
  end
end

local phase = 'setup'
local counts, events = {}, {}
local WATCH = { S_EVENT_SHOT = true, S_EVENT_SHOOTING_START = true, S_EVENT_HIT = true, S_EVENT_KILL = true }
E.track.handler({
  onEvent = function(_, event)
    local name = names[event.id]
    if not WATCH[name] then
      return
    end
    local who
    pcall(function() who = event.initiator:getGroup():getName() end)
    local key = phase .. ' ' .. tostring(who) .. ' ' .. name
    counts[key] = (counts[key] or 0) + 1
    if #events < 200 then
      events[#events + 1] = { t = timer.getTime() - (E.store.e08a_t0 or 0), phase = phase, group = who, event = name }
    end
  end,
})

local function a_units()
  local out = {}
  for i = 1, 3 do
    out[i] = { name = 'np-e08a-' .. i, x = x0 + i * 15, z = z0 }
  end
  return out
end
local function b_units()
  local out = {}
  for i = 1, 2 do
    out[i] = { name = 'np-e08b-' .. i, x = x0 + DISTANCE_M + i * 15, z = z0 }
  end
  return out
end

local function ammo_total(group)
  local total, units = 0, 0
  local g = Group.getByName(group)
  for _, unit in ipairs(g and g:getUnits() or {}) do
    units = units + 1
    for _, row in ipairs(X.ammo_rows(unit) or {}) do
      total = total + (row.count or 0)
    end
  end
  return { units = units, rounds = total }
end

local function task_against(group_id)
  return { id = 'AttackGroup', params = { groupId = group_id, expend = 'Auto', weaponType = ANY_WEAPON, groupAttack = true } }
end

local function issue()
  local b = Group.getByName('np-e08b')
  local a = Group.getByName('np-e08a')
  local id = b and b:getID()
  local ok, err = pcall(function()
    a:getController():setTask(task_against(id))
  end)
  return { group_id = id, ok = ok, error = (not ok) and tostring(err) or nil,
    has_task = X.try(function() return a:getController():hasTask() end) }
end

local function shots(label)
  local total = 0
  for key, n in pairs(counts) do
    if key:find('^' .. label .. ' np%-e08a%S* S_EVENT_SHOT$') then
      total = total + n
    end
  end
  return total
end

local function living(group)
  local g = Group.getByName(group)
  return g and #g:getUnits() or 0
end

E.store.e08a_t0 = timer.getTime()
local stages = {}
local function stage(wait_s, fn)
  stages[#stages + 1] = { wait_s, fn }
end

stage(0, function()
  local _, aerr = X.add_ground('np-e08a', 'M-1 Abrams', a_units(), { country = country.id.USA })
  local _, berr = X.add_ground('np-e08b', 'T-72B', b_units(), { country = country.id.RUSSIA })
  E.observe('spawned', { a_error = aerr, b_error = berr }, { info = true })
end)
stage(2, function()
  local a, b = Group.getByName('np-e08a'), Group.getByName('np-e08b')
  E.observe('orders', {
    a_open_fire = S.open_fire(a),
    b_hold_fire = (pcall(function() b:getController():setOption(0, 4) end)),
    a_id = a and a:getID(),
    b_id = b and b:getID(),
    a_ammo = ammo_total('np-e08a'),
  }, { info = true })
  local pa = Group.getByName('np-e08a'):getUnit(1):getPoint()
  local pb = Group.getByName('np-e08b'):getUnit(1):getPoint()
  E.observe('geometry', {
    visible = X.try(function() return land.isVisible({ x = pa.x, y = pa.y + 2, z = pa.z }, { x = pb.x, y = pb.y + 2, z = pb.z }) end),
    surface_a = X.try(function() return land.getSurfaceType({ x = pa.x, y = pa.z }) end),
    surface_b = X.try(function() return land.getSurfaceType({ x = pb.x, y = pb.z }) end),
    height_a = pa.y, height_b = pb.y,
  }, { info = true })
  phase = 'first'
  E.observe('first_task', issue(), { info = true })
end)
stage(FIRST_S, function()
  E.observe('first_phase', { shots = shots('first'), b_living = living('np-e08b'), a_ammo = ammo_total('np-e08a') }, { info = true })
  phase = 'gap'
  local old = Group.getByName('np-e08b')
  E.store.e08a_old_id = old and old:getID()
  S.destroy('np-e08b')
end)
stage(GAP_S, function()
  local gone = Group.getByName('np-e08b') == nil
  local _, err = X.add_ground('np-e08b', 'T-72B', b_units(), { country = country.id.RUSSIA })
  local b = Group.getByName('np-e08b')
  pcall(function() b:getController():setOption(0, 4) end)
  E.observe('re_added', {
    name_free_before_add = gone,
    error = err,
    old_id = E.store.e08a_old_id,
    new_id = b and b:getID(),
    id_changed = b ~= nil and b:getID() ~= E.store.e08a_old_id,
    b_living = living('np-e08b'),
  }, { info = true })
  phase = 'control'
end)
stage(CONTROL_S, function()
  E.observe('control_phase', { shots = shots('control'), b_living = living('np-e08b'), a_ammo = ammo_total('np-e08a') }, { info = true })
  phase = 'reissue'
  E.observe('reissued_task', issue(), { info = true })
end)
stage(REISSUE_S, function()
  local first, control, reissue = shots('first'), shots('control'), shots('reissue')
  E.observe('reissue_phase', { shots = reissue, b_living = living('np-e08b'), a_ammo = ammo_total('np-e08a') }, { info = true })
  E.observe('summary', {
    shots_first = first,
    shots_control = control,
    shots_reissue = reissue,
    engages_after_reissue = reissue > 0,
    engages_with_old_id = control > 0,
    decided = first > 0,
  }, { info = true })
  E.observe('events', events, { info = true })
  E.observe('counts', counts, { info = true })
  E.observe('cleared', tostring(S.clear()), { info = true })
end)

local index, mark = 0, timer.getTime()
return E.wait(function()
  local now = timer.getTime()
  local next_stage = stages[index + 1]
  if not next_stage then
    return 'done'
  end
  if now - mark < next_stage[1] then
    return nil
  end
  index = index + 1
  mark = now
  next_stage[2]()
  return nil
end, 200, 'running')
