-- E38: reading ED's debriefing event store a little at a time.
--
-- The `server` state holds DebriefingEventsData (ED-CODE:
-- Scripts/UI/DebriefingEventsData.lua), which DCS feeds while a mission runs.
-- This snippet records what a reader that wants only the new entries has to
-- pay and can rely on:
--   - whether getEvents returns the same table on every call, so entries
--     past the last index read are the new ones;
--   - the cost of getEventCount and of getEvents, one call a frame;
--   - the cost of copying the five fields a sampler needs from one entry;
--   - the store's size, by type, and one entry of each type with its fields;
--   - what each `end shooting` entry holds as `ammo_consumption`.
-- Run it after a probe that fought, such as coverage.lua.
-- states: server
-- timeout: 60

local E = ...
local store = DebriefingEventsData
local clock = E.clock

local first = store.getEvents()
local same_table = first == store.getEvents()
local by, sample = {}, {}
for _, event in ipairs(first) do
  local kind = tostring(event.type)
  by[kind] = (by[kind] or 0) + 1
  if not sample[kind] then
    local fields = {}
    for key, value in pairs(event) do
      fields[tostring(key)] = type(value)
    end
    sample[kind] = fields
  end
end
E.observe('same_table_each_call', same_table, { info = true })
E.observe('entries', { count = store.getEventCount(), length = #first, by_type = by }, { info = true })
E.observe('fields_by_type', sample, { info = true })
local bursts = {}
for _, event in ipairs(first) do
  if event.type == 'end shooting' and #bursts < 60 then
    bursts[#bursts + 1] = string.format('%s %s %s', tostring(event.initiator_unit_type), tostring(event.weapon), tostring(event.ammo_consumption))
  end
end
E.observe('end_shooting_unit_weapon_consumption', bursts, { info = true })

local function median(list)
  table.sort(list)
  return list[math.ceil(#list / 2)]
end

local counts, lists, empties, copies = {}, {}, {}, {}
local frame = 0
return E.wait(function()
  frame = frame + 1
  local t0 = clock()
  local t1 = clock()
  local n = store.getEventCount()
  local t2 = clock()
  local events = store.getEvents()
  local t3 = clock()
  local event = events[(frame - 1) % math.max(n, 1) + 1]
  local a, b, c, d, e
  if event then
    a, b, c, d, e = event.type, event.t, event.initiator_object_id, event.target_object_id, event.weapon
  end
  local t4 = clock()
  empties[frame], counts[frame], lists[frame], copies[frame] = t1 - t0, t2 - t1, t3 - t2, t4 - t3
  if frame < 300 then
    return a and b and c and d and e and nil
  end
  local empty = median(empties)
  E.observe('ns_a_call', {
    clock_pair = math.floor(empty * 1e9 + 0.5),
    getEventCount = math.floor((median(counts) - empty) * 1e9 + 0.5),
    getEvents = math.floor((median(lists) - empty) * 1e9 + 0.5),
    copy_five_fields = math.floor((median(copies) - empty) * 1e9 + 0.5),
  }, { timing = true })
  return 'done'
end, 50, 'running')
