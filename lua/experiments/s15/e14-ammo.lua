-- E14: which `getAmmo()` entry does each `ammo` slot of a ground unit feed,
-- and by how much does the count read differ from the count asked for?
--
-- A ground unit's table takes `ammo = { WS = { [ws] = { LN = { [ln] = { PL =
-- { [pl] = { ammo_capacity = n, virtualStwID = pl, name = ... } } } } } } }`
-- (ED-CODE: the mission format, MissionEditor/modules/me_loadout_vehicles.lua
-- as read in the research notes; PRIOR: a T-72B's main gun read one more
-- than the capacity asked for). The per-type layout is in no table a script
-- can read, so the snippet finds it by trying. For each of ten types it:
--   1. adds the type with no `ammo` and records the rows getAmmo lists, the
--      full counts;
--   2. adds one unit per slot of WS 1-2, LN 1-4, PL 1-2, each asking for 1
--      round in that slot alone, and reads getAmmo of each: the row whose
--      count changed is the one the slot feeds. The slot's row and the
--      count read give the offset, read less asked;
--   3. when no slot changed anything, tries again with `name` set to the
--      tail of each row's type name, one slot per row (LN = row, PL 1);
--   4. adds one unit asking for about half of every found row's full count
--      through every found slot at once, and records the rows, so the offset
--      shows at a second count and for all rows together.
-- A type the mission cannot add is recorded and skipped. The type names are
-- those of ED's unit database; those marked UNVERIFIED have not been added
-- on a live server (the others appear in earlier live runs).
-- states: server
-- timeout: 600

local E = ...
local X = E.lib('s15')
local S = X.S

-- ED type names. UNVERIFIED: BMP-2, M-2 Bradley, Leopard-2,
-- M1126 Stryker ICV, Ural-375 ZU-23.
local TYPES = {
  'T-72B', 'M-1 Abrams', 'BMP-2', 'M-2 Bradley', 'ZSU-23-4 Shilka',
  'Osa 9A33 ln', 'Leopard-2', 'BTR-80', 'M1126 Stryker ICV', 'Ural-375 ZU-23',
}
local MAX_WS, MAX_LN, MAX_PL = 2, 4, 2
local PROBE_COUNT = 1
local SETTLE_S = 2

local origin = S.origin()

-- The ammo table for a list of { ws, ln, pl, n, name }.
local function ammo_table(slots)
  local ammo = { WS = {} }
  for _, s in ipairs(slots) do
    local ws = ammo.WS[s.ws] or { LN = {} }
    ammo.WS[s.ws] = ws
    local ln = ws.LN[s.ln] or { PL = {} }
    ws.LN[s.ln] = ln
    ln.PL[s.pl] = { ammo_capacity = s.n, virtualStwID = s.pl, name = s.name }
  end
  return ammo
end

local function add(label, kind, x, z, ammo)
  local group, err = X.add_ground(label, kind, { { name = label .. '-1', x = x, z = z, ammo = ammo } })
  return group ~= nil, err
end

local function rows_of(label)
  local unit = Unit.getByName(label .. '-1')
  if not unit then
    return nil
  end
  return X.ammo_rows(unit) or {}
end

-- Row indices whose count differs between two reads of the same type, and a
-- note when the rows themselves differ.
local function changed(base, cand)
  if #base ~= #cand then
    return nil, 'baseline ' .. #base .. ' rows, this unit ' .. #cand
  end
  local out = {}
  for i, row in ipairs(base) do
    if row.type ~= cand[i].type then
      return nil, 'row ' .. i .. ' is ' .. tostring(row.type) .. ' in the baseline and ' .. tostring(cand[i].type)
    end
    if row.count ~= cand[i].count then
      out[#out + 1] = i
    end
  end
  return out
end

local results = {}
local steps = {}
local index, due = 1, 0

local function push(fn)
  steps[#steps + 1] = fn
end

local function plan(t, kind)
  local st = { type = kind, n = t, probes = {}, slots = {}, errors = {} }
  local x0 = origin.x + 3000
  local z0 = origin.z + 12000 + t * 300
  local base_label = 'np-e14-' .. t .. 'b'

  push(function()
    local ok, err = add(base_label, kind, x0, z0, nil)
    st.added = ok
    if not ok then
      st.errors[#st.errors + 1] = 'baseline add: ' .. tostring(err)
    end
    local k = 0
    -- One unit per slot, asking for PROBE_COUNT rounds through it alone.
    for ws = 1, MAX_WS do
      for ln = 1, MAX_LN do
        for pl = 1, MAX_PL do
          k = k + 1
          local label = string.format('np-e14-%dw%dl%dp%d', t, ws, ln, pl)
          local slot = { ws = ws, ln = ln, pl = pl, n = PROBE_COUNT }
          local added, aerr = add(label, kind, x0 + k * 30, z0 + 40, ammo_table({ slot }))
          st.probes[#st.probes + 1] = { label = label, slot = slot, added = added, error = aerr }
        end
      end
    end
    return SETTLE_S
  end)

  push(function()
    st.default = rows_of(base_label)
    if not st.default then
      st.errors[#st.errors + 1] = 'the baseline unit does not exist'
      st.default = {}
    end
    local nothing = true
    for _, p in ipairs(st.probes) do
      local cand = rows_of(p.label)
      if cand then
        local rows, note = changed(st.default, cand)
        if note then
          st.errors[#st.errors + 1] = string.format('w%dl%dp%d: %s', p.slot.ws, p.slot.ln, p.slot.pl, note)
        else
          for _, i in ipairs(rows) do
            nothing = false
            st.slots[#st.slots + 1] = {
              ws = p.slot.ws, ln = p.slot.ln, pl = p.slot.pl, variant = 'plain', row = i, type = cand[i].type,
              requested = p.slot.n, read = cand[i].count, offset = cand[i].count - p.slot.n, default = st.default[i].count,
            }
          end
        end
      end
      S.destroy(p.label)
    end
    S.destroy(base_label)
    st.unchanged_plain = nothing
    if nothing and #st.default > 0 then
      -- Name each slot after its row, one unit per row.
      local named = {}
      for i, row in ipairs(st.default) do
        local label = string.format('np-e14-%dn%d', t, i)
        local slot = { ws = 1, ln = i, pl = 1, n = PROBE_COUNT, name = X.tail(row.type) }
        local added, aerr = add(label, kind, x0 + i * 30, z0 + 80, ammo_table({ slot }))
        named[#named + 1] = { label = label, slot = slot, added = added, error = aerr }
      end
      st.named = named
      return SETTLE_S
    end
    return 0
  end)

  push(function()
    for _, p in ipairs(st.named or {}) do
      local cand = rows_of(p.label)
      if cand then
        local rows, note = changed(st.default, cand)
        if note then
          st.errors[#st.errors + 1] = 'named ln' .. p.slot.ln .. ': ' .. note
        else
          for _, i in ipairs(rows) do
            st.slots[#st.slots + 1] = {
              ws = p.slot.ws, ln = p.slot.ln, pl = p.slot.pl, variant = 'named', row = i, type = cand[i].type,
              requested = p.slot.n, read = cand[i].count, offset = cand[i].count - p.slot.n, default = st.default[i].count,
            }
          end
        end
      end
      S.destroy(p.label)
    end
    -- Every found slot at once, asking for about half the full count.
    local asked, seen = {}, {}
    for _, s in ipairs(st.slots) do
      if not seen[s.row] then
        seen[s.row] = true
        local n = math.max(1, math.floor(s.default / 2))
        asked[#asked + 1] = { ws = s.ws, ln = s.ln, pl = s.pl, n = n, name = s.variant == 'named' and X.tail(s.type) or nil, row = s.row }
      end
    end
    st.asked = asked
    if #asked > 0 then
      local ok, err = add('np-e14-' .. t .. 'v', kind, x0, z0 + 120, ammo_table(asked))
      if not ok then
        st.errors[#st.errors + 1] = 'verification add: ' .. tostring(err)
      end
      return SETTLE_S
    end
    return 0
  end)

  push(function()
    local verify = {}
    local rows = rows_of('np-e14-' .. t .. 'v')
    if rows then
      for _, a in ipairs(st.asked) do
        local row = rows[a.row]
        verify[#verify + 1] = {
          row = a.row, ws = a.ws, ln = a.ln, pl = a.pl, requested = a.n,
          read = row and row.count, offset = row and (row.count - a.n),
        }
      end
    end
    S.destroy('np-e14-' .. t .. 'v')
    local mapped = {}
    for _, s in ipairs(st.slots) do
      mapped[s.row] = true
    end
    local unmapped = {}
    for i = 1, #st.default do
      if not mapped[i] then
        unmapped[#unmapped + 1] = i
      end
    end
    local entry = {
      type = kind,
      added = st.added,
      default_rows = st.default,
      slots = st.slots,
      verification = verify,
      unmapped_rows = unmapped,
      errors = st.errors,
    }
    results[#results + 1] = { type = kind, rows = #st.default, slots = #st.slots, unmapped = #unmapped }
    E.observe('type_' .. t .. '_' .. kind, entry, { info = true })
    return 1
  end)
end

for t, kind in ipairs(TYPES) do
  plan(t, kind)
end

return E.wait(function()
  if timer.getTime() < due then
    return nil
  end
  local step = steps[index]
  if not step then
    E.observe('summary', results, { info = true })
    return 'done'
  end
  index = index + 1
  local ok, wait_s = pcall(step)
  if not ok then
    E.observe('step_failed_' .. index, tostring(wait_s), { info = true })
    wait_s = 0
  end
  due = timer.getTime() + (wait_s or 0)
  return nil
end, 540, 'running')
