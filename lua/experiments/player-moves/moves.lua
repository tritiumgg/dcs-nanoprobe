-- Player moves: what net.force_player_slot(player, side, slot) does with each
-- side, into a slot, against a slot of another side, and when the player
-- changes side before the command.
--
-- `run.sh` fills the placeholders and runs this with
-- `cargo xtask eval <server> hook` on a server with one client connected.
-- STEPS is a list separated by `;`. Each step is `side,slot` or
-- `side,slot,wait`:
--   side  the number passed to force_player_slot;
--   slot  empty, a literal slot ID, or `@red` / `@blue` for the first slot
--         DCS.getAvailableSlots offers that coalition;
--   wait  a side: the step first waits, up to WAIT_LIMIT s, until the
--         client's own side becomes it (a person or a client snippet changes
--         it), then calls force_player_slot in that same frame.
-- Each step records the client's side and slot before the call, what the
-- call returned, the first change the server reads and when, and the side
-- and slot after SETTLE s.
-- Rule: recorded as read; the results doc sets the verdict per step.
-- states: hook
-- timeout: 900

local E = ...
local STEPS = '<steps>'
local SETTLE = tonumber('<settle>') or 6
local WAIT_LIMIT = 300

local function client()
  local me = net.get_my_player_id()
  for _, id in ipairs(net.get_player_list() or {}) do
    if id ~= me then
      return id
    end
  end
end

local id = client()
if not id then
  error('no client connected')
end

-- The slots each coalition offers: the first three, with the ID's Lua type.
local offered = {}
for _, name in ipairs({ 'red', 'blue' }) do
  local list = {}
  for i, s in ipairs(DCS.getAvailableSlots(name) or {}) do
    if i > 3 then
      break
    end
    list[i] = { unitId = tostring(s.unitId), id_type = type(s.unitId), type = tostring(s.type),
      role = tostring(s.role) }
  end
  offered[name] = list
end
E.observe('offered', offered, { info = true })

local function resolve(slot)
  local name = slot:match('^@(%a+)$')
  if not name then
    return slot
  end
  local s = (DCS.getAvailableSlots(name) or {})[1]
  if not s then
    error('no slot offered to ' .. name)
  end
  return tostring(s.unitId)
end

local function where()
  local side = net.get_player_info(id, 'side')
  local slot = net.get_player_info(id, 'slot')
  return { side = side, slot = tostring(slot), slot_type = type(slot) }
end

local steps = {}
for text in STEPS:gmatch('[^;]+') do
  local side, slot, wait = text:match('^(%-?%d+),([^,]*),?(%-?%d*)$')
  if not side then
    error('bad step: ' .. text)
  end
  steps[#steps + 1] = { side = tonumber(side), slot = slot, wait = tonumber(wait) }
end

local n, step, phase, mark, record = 0, nil, 'next', 0, nil
return E.wait(function()
  local now = DCS.getRealTime()
  if phase == 'next' then
    n = n + 1
    step = steps[n]
    if not step then
      return 'done'
    end
    phase, mark = step.wait and 'waiting' or 'call', now
  end
  if phase == 'waiting' then
    if where().side ~= step.wait then
      if now - mark > WAIT_LIMIT then
        error('step ' .. n .. ': the client did not change to side ' .. step.wait)
      end
      return nil
    end
    phase = 'call'
  end
  if phase == 'call' then
    local slot = resolve(step.slot)
    record = { side = step.side, slot = slot, waited = step.wait and (now - mark) or nil, before = where() }
    local ok, returned = pcall(net.force_player_slot, id, step.side, slot)
    record.ok, record.returned = ok, ok and tostring(returned) or nil
    record.error = (not ok) and tostring(returned) or nil
    record.same_frame = where()
    phase, mark = 'settle', now
    return nil
  end
  -- settle
  local at = where()
  if not record.changed_at and (at.side ~= record.before.side or at.slot ~= record.before.slot) then
    record.changed_at = now - mark
    record.first_change = at
  end
  if now - mark < SETTLE then
    return nil
  end
  record.after = at
  E.observe('step_' .. n, record, { info = true })
  phase = 'next'
  return nil
end, WAIT_LIMIT * 3)
