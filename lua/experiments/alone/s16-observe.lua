-- S16: what a scripted client sees while the server kicks it, bans it or moves
-- it to another slot. It installs a second callback set that records
-- onNetDisconnect (the reason text and the error code), onNetMissionChanged
-- and onPlayerChangeSlot, and samples the connection once a second: the
-- player ID, the multiplayer flag, the slot `net.get_slot` reports and the
-- unit `DCS.getPlayerUnit` reads. DCS.setUserCallbacks has no removal, so the
-- set does nothing once the snippet has reported.
--
-- Two ways to run it, for the same report:
--   * `<wait>` filled in with a number of seconds, or unset (30): it waits
--     that long, then returns what it saw.
--   * `<wait>` filled in with 0: it returns `armed` at once. The next run
--     returns the report. `cargo xtask commands <server> --part moderation`
--     runs it this way, around the server's action.
-- Run by hand: `cargo xtask eval <client-target> hook lua/experiments/alone/s16-observe.lua`.
-- states: hook
-- timeout: 90

local E = ...
local WAIT = tonumber('<wait>') or 30
local NAME = '__np_s16_observe'

local function now()
  return DCS.getRealTime()
end

local function try(fn, ...)
  local ok, a, b = pcall(fn, ...)
  if ok then
    return a, b
  end
  return 'raised ' .. tostring(a)
end

local function report(state)
  state.live = false
  rawset(_G, NAME, nil)
  return state
end

local function sample(state)
  local id = try(net.get_my_player_id)
  local side, slot = try(net.get_slot, id)
  local row = {
    t = math.floor((now() - state.start) * 10) / 10,
    id = id,
    multiplayer = try(DCS.isMultiplayer),
    side = side,
    slot = slot ~= nil and tostring(slot) or nil,
    unit = tostring(try(DCS.getPlayerUnit)),
  }
  local last = state.samples[#state.samples]
  if not last or last.id ~= row.id or last.multiplayer ~= row.multiplayer or last.side ~= row.side
    or last.slot ~= row.slot or last.unit ~= row.unit then
    state.samples[#state.samples + 1] = row
  end
end

local state = rawget(_G, NAME)
if state then
  sample(state)
  state.finished = true
  return E.observe('report', {
    seconds = math.floor((now() - state.start) * 10) / 10,
    disconnects = state.disconnects,
    slot_changes = state.slot_changes,
    mission_changes = state.mission_changes,
    samples = state.samples,
  }, { info = true }) and report(state) and 'reported'
end

state = {
  live = true,
  start = now(),
  next_sample = now(),
  disconnects = {},
  slot_changes = {},
  mission_changes = {},
  samples = {},
}
rawset(_G, NAME, state)
local function stamp()
  return math.floor((now() - state.start) * 10) / 10
end
DCS.setUserCallbacks({
  onNetDisconnect = function(reason, code)
    if state.live then
      state.disconnects[#state.disconnects + 1] = { t = stamp(), reason = tostring(reason), code = tostring(code) }
    end
  end,
  onNetMissionChanged = function(name)
    if state.live then
      state.mission_changes[#state.mission_changes + 1] = { t = stamp(), mission = tostring(name) }
    end
  end,
  onPlayerChangeSlot = function(id)
    if state.live then
      local side, slot = try(net.get_slot, id)
      state.slot_changes[#state.slot_changes + 1] = {
        t = stamp(), player = tostring(id), side = side, slot = slot ~= nil and tostring(slot) or nil,
      }
    end
  end,
})
E.track.defer('observer off', function()
  state.live = false
  rawset(_G, NAME, nil)
end)
sample(state)
E.observe('armed_at_real_time', now(), { info = true })

if WAIT <= 0 then
  -- Polling continues in the hook's frames only while a snippet waits, so
  -- the samples between runs come from the callbacks; the second run takes
  -- one more.
  return 'armed'
end
return E.wait(function()
  if now() >= state.next_sample then
    state.next_sample = now() + 1
    sample(state)
  end
  if now() - state.start < WAIT then
    return nil
  end
  sample(state)
  E.observe('report', {
    seconds = WAIT,
    disconnects = state.disconnects,
    slot_changes = state.slot_changes,
    mission_changes = state.mission_changes,
    samples = state.samples,
  }, { info = true })
  report(state)
  return 'reported'
end, WAIT + 10)
