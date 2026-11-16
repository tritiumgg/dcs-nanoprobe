-- Warehouse evidence: one step of a player's life at an airbase or FARP, and
-- how each limited warehouse's inventory changes over it.
--
-- `run.sh` fills the placeholders and runs this with
-- `cargo xtask eval <server> hook`. The snippet:
--   1. reads every warehouse whose getInventory counts are all below
--      UNLIMITED, and the player's unit, fuel and ammunition. On the one live
--      mission every warehouse read so, the ones the mission marks unlimited
--      included (docs/results/2026-11-16-warehouse-player-deltas.md; E6 read
--      1,000,000 on another mission);
--   2. does ACTION: `none` (a person acts), `slot` (net.force_player_slot to
--      SLOT), `deslot` (to spectators), `kill` (an explosion at the player's
--      unit), `list` (slots, players and warehouses only, no watch), or
--      `stock` (setItem STOCK of every weapon and aircraft the first
--      warehouse with a count at or above UNLIMITED lists, into each other
--      warehouse; LIVE, no warehouse listed such a count, so it set nothing);
--   3. reads them again once a second for SECONDS of real time, recording
--      each change with its real time from the start, the model time and
--      whether the simulation was paused;
--   4. reports each warehouse's net change and the player's state at the end.
-- Rule: recorded as read; the results doc sets the pass rule per step.
-- states: hook
-- timeout: 900

local E = ...
local ACTION = '<action>'
local SLOT = '<slot>'
local SECONDS = tonumber('<seconds>') or 60
local STOCK = tonumber('<stock>') or 100
local UNLIMITED = 1000000

local function server(source)
  local ok, value = E.cross('server', source)
  if not ok then
    error('cross failed: ' .. tostring(value))
  end
  return value
end

-- Server-state helpers, sent with each read. Lines are tab separated:
-- W name category id | I kind key count | P name type fuel life x z | A name count
local LIB = [[
local UNLIMITED = ]] .. UNLIMITED .. [[

local function inv_of(base)
  local ok, wh = pcall(base.getWarehouse, base)
  if not ok or not wh then return nil end
  local ok2, inv = pcall(wh.getInventory, wh)
  if not ok2 or type(inv) ~= 'table' then return nil end
  return inv, wh
end
local function limited(inv)
  for _, kind in ipairs({ 'weapon', 'aircraft', 'liquids' }) do
    for _, n in pairs(inv[kind] or {}) do
      if type(n) == 'number' and n >= UNLIMITED then return false end
    end
  end
  return true
end
local function cat(base)
  local ok, d = pcall(base.getDesc, base)
  return ok and type(d) == 'table' and d.category or -1
end
local function dump(out, base, inv)
  out[#out + 1] = table.concat({ 'W', base:getName(), tostring(cat(base)), tostring(base:getID()) }, '\t')
  for _, kind in ipairs({ 'weapon', 'aircraft', 'liquids' }) do
    for key, n in pairs(inv[kind] or {}) do
      out[#out + 1] = table.concat({ 'I', kind, tostring(key), tostring(n) }, '\t')
    end
  end
end
local function players(out)
  for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
    for _, u in ipairs(coalition.getPlayers(side) or {}) do
      local p = u:getPoint()
      local okf, fuel = pcall(u.getFuel, u)
      local okl, life = pcall(u.getLife, u)
      local okd, desc = pcall(u.getDesc, u)
      local fmax = okd and type(desc) == 'table' and desc.fuelMassMax or -1
      out[#out + 1] = table.concat({ 'P', u:getName(), u:getTypeName(), tostring(okf and fuel),
        tostring(okl and life), string.format('%.0f', p.x), string.format('%.0f', p.z),
        tostring(fmax), tostring(select(2, pcall(u.inAir, u))) }, '\t')
      local oka, ammo = pcall(u.getAmmo, u)
      for _, a in ipairs(oka and ammo or {}) do
        local name = type(a.desc) == 'table' and (a.desc.typeName or a.desc.displayName) or '?'
        out[#out + 1] = table.concat({ 'A', tostring(name), tostring(a.count) }, '\t')
      end
    end
  end
end
]]

local READ_LIMITED = LIB .. [[
local out = { 'T\t' .. tostring(timer.getTime()) }
for _, base in ipairs(world.getAirbases()) do
  local inv = inv_of(base)
  if inv and limited(inv) then dump(out, base, inv) end
end
players(out)
return table.concat(out, '\n')
]]

local LIST = LIB .. [[
local out = {}
for _, base in ipairs(world.getAirbases()) do
  local inv = inv_of(base)
  local n = 0
  for _, kind in ipairs({ 'weapon', 'aircraft', 'liquids' }) do
    for _ in pairs(inv and inv[kind] or {}) do n = n + 1 end
  end
  local p = base:getPoint()
  out[#out + 1] = table.concat({ 'B', base:getName(), tostring(cat(base)), tostring(base:getID()),
    tostring(inv ~= nil), tostring(inv and limited(inv)), tostring(n),
    string.format('%.0f', p.x), string.format('%.0f', p.z) }, '\t')
end
players(out)
return table.concat(out, '\n')
]]

local STOCK_ALL = LIB .. [[
local names, limited_bases = {}, {}
for _, base in ipairs(world.getAirbases()) do
  local inv = inv_of(base)
  if inv then
    if limited(inv) then
      limited_bases[#limited_bases + 1] = base
    elseif not next(names) then
      for _, kind in ipairs({ 'weapon', 'aircraft' }) do
        for key in pairs(inv[kind] or {}) do names[#names + 1] = key end
      end
    end
  end
end
local set = 0
for _, base in ipairs(limited_bases) do
  local wh = base:getWarehouse()
  for _, key in ipairs(names) do
    if pcall(wh.setItem, wh, key, ]] .. STOCK .. [[) then set = set + 1 end
  end
end
return #limited_bases .. ' limited, ' .. #names .. ' item names, ' .. set .. ' sets'
]]

-- Parse a read into { t = model time, wh = { [name] = { [kind\tkey] = n } },
-- cat = { [name] = c }, players = { lines }, ammo = { lines } }.
local function parse(text)
  local r = { wh = {}, cat = {}, players = {}, ammo = {} }
  local current
  for line in tostring(text):gmatch('[^\n]+') do
    local f = {}
    for field in (line .. '\t'):gmatch('([^\t]*)\t') do
      f[#f + 1] = field
    end
    if f[1] == 'T' then
      r.t = tonumber(f[2])
    elseif f[1] == 'W' then
      current = {}
      r.wh[f[2]] = current
      r.cat[f[2]] = f[3] .. ' id ' .. f[4]
    elseif f[1] == 'I' and current then
      current[f[2] .. '\t' .. f[3]] = tonumber(f[4])
    elseif f[1] == 'P' then
      r.players[#r.players + 1] = line
    elseif f[1] == 'A' then
      r.ammo[#r.ammo + 1] = line
    end
  end
  return r
end

-- Changes from a to b, as `warehouse kind key: before -> after (delta)`.
local function diff(a, b)
  local out = {}
  local names = {}
  for name in pairs(a.wh) do names[name] = true end
  for name in pairs(b.wh) do names[name] = true end
  for name in pairs(names) do
    local wa, wb = a.wh[name], b.wh[name]
    if not wa or not wb then
      out[#out + 1] = name .. ': ' .. (wa and 'no longer limited or gone' or 'newly seen')
    else
      local keys = {}
      for k in pairs(wa) do keys[k] = true end
      for k in pairs(wb) do keys[k] = true end
      for k in pairs(keys) do
        local x, y = wa[k] or 0, wb[k] or 0
        if x ~= y then
          out[#out + 1] = string.format('%s %s: %s -> %s (%+g)', name, (k:gsub('\t', ' ')), tostring(x), tostring(y), y - x)
        end
      end
    end
  end
  table.sort(out)
  return out
end

local function paused()
  local ok, p = pcall(DCS.getPause)
  return ok and p or nil
end

if ACTION == 'list' then
  local slots = {}
  for _, side in ipairs({ 'red', 'blue' }) do
    for _, s in ipairs(DCS.getAvailableSlots(side) or {}) do
      local fields = {}
      for k, v in pairs(s) do
        fields[#fields + 1] = tostring(k) .. '=' .. tostring(v)
      end
      table.sort(fields)
      slots[#slots + 1] = side .. ' ' .. table.concat(fields, ' ')
    end
  end
  E.observe('slots', slots, { info = true })
  local list = {}
  for _, id in ipairs(net.get_player_list() or {}) do
    local side, slot = net.get_slot(id)
    list[#list + 1] = string.format('id=%s side=%s slot=%s', tostring(id), tostring(side), tostring(slot))
  end
  E.observe('players', list, { info = true })
  E.observe('airbases', server(LIST), { info = true })
  return 'listed'
end

if ACTION == 'stock' then
  E.observe('stock', server(STOCK_ALL), { info = true })
  return 'stocked'
end

-- The one connected client: the first player who is not the server.
local function client()
  local me = net.get_my_player_id()
  for _, id in ipairs(net.get_player_list() or {}) do
    if id ~= me then
      return id
    end
  end
end

local t0 = DCS.getRealTime()
local first = parse(server(READ_LIMITED))
local last = first
E.observe('start', { model = first.t, paused = paused(), players = first.players, ammo = first.ammo,
  limited = first.cat }, { info = true })
-- The start inventory of each watched warehouse that lists more than the four
-- liquids, as `kind key = count` lines.
do
  local inventory = {}
  for name, items in pairs(first.wh) do
    local lines = {}
    for k, n in pairs(items) do
      lines[#lines + 1] = (k:gsub('\t', ' ')) .. ' = ' .. tostring(n)
    end
    if #lines > 4 then
      table.sort(lines)
      inventory[name] = lines
    end
  end
  E.observe('start_inventory', inventory, { info = true })
end

if ACTION == 'slot' or ACTION == 'deslot' then
  local id = client()
  if not id then
    error('no client connected')
  end
  local side = 0
  if ACTION == 'slot' then
    for name, n in pairs({ red = 1, blue = 2 }) do
      for _, s in ipairs(DCS.getAvailableSlots(name) or {}) do
        if tostring(s.unitId) == SLOT then
          side = n
        end
      end
    end
    if side == 0 then
      error('slot ' .. SLOT .. ' is not offered')
    end
  end
  local ok, err = pcall(net.force_player_slot, id, side, ACTION == 'slot' and SLOT or '')
  E.observe('action', { action = ACTION, player = id, side = side, slot = SLOT, ok = ok,
    error = (not ok) and tostring(err) or nil, at = DCS.getRealTime() - t0 }, { info = true })
elseif ACTION == 'kill' then
  local text = server([[
local u = (coalition.getPlayers(coalition.side.BLUE) or {})[1] or (coalition.getPlayers(coalition.side.RED) or {})[1]
if not u then return 'no player unit' end
local p = u:getPoint()
trigger.action.explosion(p, 200)
return 'explosion at ' .. u:getName()
]])
  E.observe('action', { action = ACTION, result = text, at = DCS.getRealTime() - t0 }, { info = true })
else
  E.observe('action', { action = ACTION, at = 0 }, { info = true })
end

local changes, nextread = {}, 1
return E.wait(function()
  local now = DCS.getRealTime() - t0
  if now < nextread then
    return nil
  end
  nextread = nextread + 1
  -- A failed read is recorded and skipped, so the changes seen so far are kept.
  local ok, text = pcall(server, READ_LIMITED)
  local r = last
  if ok then
    r = parse(text)
    local d = diff(last, r)
    if #d > 0 then
      changes[#changes + 1] = { real = string.format('%.1f', now), model = r.t, paused = paused(), changes = d,
        players = r.players }
    end
    last = r
  else
    changes[#changes + 1] = { real = string.format('%.1f', now), failed = tostring(text) }
  end
  if now >= SECONDS then
    E.observe('changes', changes, { info = true })
    E.observe('net', diff(first, r), { info = true })
    E.observe('end', { model = r.t, paused = paused(), players = r.players, ammo = r.ammo }, { info = true })
    return 'done'
  end
end, SECONDS + 30)
