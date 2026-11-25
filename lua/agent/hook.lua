-- The hook, run in the hook state by the loader with the install root and the
-- active build hash.
--
-- It writes a session marker to dcs.log, loads the agent DLL into this state,
-- opens the agent's link to the sidecar (spec/transport.md), and pumps it on
-- every frame: the agent's frame, on a sampling frame one cross-state call
-- into the shim's pump, then the agent's finish. On each simulation start it
-- loads the shim into the server state, once per process, and starts it. It
-- logs the mission lifecycle as JSON lines (spec/log.md), which also reach
-- the sidecar through the agent.
--
-- With the link open, the agent looks for the kill switch and the shim reload
-- request on the frames where it makes its system calls. Without it, the hook
-- looks for the kill switch itself, once a second.
--
-- No callback returns a value: a value returned from an onPlayerTry*
-- callback ends the hook chain for later scripts (ED-CODE,
-- API/Sim_ControlAPI.md).

local root, build = ...
local dir = root .. 'versions/' .. build .. '/'

local logmodule = assert(loadfile(dir .. 'lua/log.lua'))()

-- The agent's interface version this hook was written for.
local ABI = '10'
-- What agent.frame returns.
local FRAME_RELOAD, FRAME_DISABLED = 1, 2

local generation = 0
local disabled = false
local next_check = 0
local agent
local linked = false
local dll = dir .. 'nanoprobe_agent_' .. build .. '.dll'

local function dcslog(level, message)
  pcall(log.write, 'NANOPROBE', level, message)
end

local file = logmodule.open({
  path = root .. 'logs/hook.log',
  target = 'hook',
  real = DCS.getRealTime,
  model = DCS.getModelTime,
  gen = function()
    return generation
  end,
  build = build,
  size = function(path)
    return lfs.attributes(path, 'size')
  end,
  fallback = function(message)
    dcslog(log.ERROR, message)
  end,
})

-- The session marker. A bundle takes the part of dcs.log after the last one.
-- The ID mixes wall time and CPU time; it does not reseed math.random, which
-- every hook in the gui state shares.
local session = string.format('%08x%06x', os.time(), math.floor(os.clock() * 1e6) % 0x1000000)
dcslog(log.INFO, 'session ' .. session .. ' build ' .. build)
file.write('info', 'hook loaded', { session = session })

-- The build this process loaded, so a deploy keeps its folder while its DLL
-- is loaded (spec/install.md, "Operations").
pcall(function()
  local f = io.open(root .. 'running.txt', 'wb')
  if f then
    f:write('build=' .. build .. '\n')
    f:close()
  end
end)

-- The DCS version, read once. A missing global reads as nil.
local dcs_version = ''
do
  local ok, v = pcall(function()
    return __DCS_VERSION__
  end)
  if ok and type(v) == 'string' then
    dcs_version = v
  end
end

-- Whether each dotted name is a function or a value in this state, without
-- raising for a name that is absent.
local function probe(names)
  local found = {}
  for _, name in ipairs(names) do
    local ok, present = pcall(function()
      local v = _G
      for part in name:gmatch('[^.]+') do
        if type(v) ~= 'table' then
          return false
        end
        v = v[part]
      end
      return v ~= nil
    end)
    found[name] = ok and present or false
  end
  return found
end

-- What the platform calls in the hook state (spec/records.md,
-- "Capabilities").
local CAPABILITIES = {
  'DCS.getRealTime',
  'DCS.getModelTime',
  'DCS.getPause',
  'DCS.getMissionFilename',
  'DCS.getMissionName',
  'DCS.getMissionTheatre',
  'DCS.setUserCallbacks',
  'DCS.getAvailableSlots',
  'net.dostring_in',
  'net.load_mission',
  'net.kick',
  'net.banlist_add',
  'net.banlist_add_by_ucid',
  'net.banlist_remove',
  'net.banlist_get',
  'net.force_player_slot',
  'net.get_player_list',
  'net.get_player_info',
  'lfs.writedir',
  'lfs.attributes',
  'log.write',
  'package.loadlib',
}

-- The agent DLL, opened in this state. Opening pins it, and configuring it
-- opens logs/agent.log and the crash record logs/crash.log. Starting it opens
-- the link. A failure is one record, and the hook carries on without the
-- agent or without the link.
do
  local loaded, result = pcall(function()
    local loader, err = package.loadlib(dll, 'luaopen_nanoprobe')
    if not loader then
      error(err, 0)
    end
    local opened = loader()
    if type(opened) ~= 'table' then
      error('the opener returned a ' .. type(opened), 0)
    end
    if opened.abi ~= ABI then
      error('the DLL has ABI ' .. tostring(opened.abi) .. '; this hook needs ' .. ABI, 0)
    end
    local configured, why = opened.configure(root)
    if not configured then
      error(why, 0)
    end
    return opened
  end)
  if loaded then
    agent = result
    local status = agent.status()
    file.write('info', 'agent loaded', {
      abi = agent.abi,
      pinned = status.pinned,
      pdb = status.pdb,
      thread = status.thread,
    })
    local started, path = agent.start(dcs_version)
    if started then
      linked = true
      -- Every later hook record also goes to the sidecar.
      file.forward = function(level, msg, data)
        local args = {}
        for k, v in pairs(data or {}) do
          args[#args + 1] = tostring(k)
          args[#args + 1] = v
        end
        agent.log(level, msg, 'hook', unpack(args))
      end
      file.write('info', 'link open', { map = path, dcs = dcs_version })
    else
      file.write('error', 'link not opened', { error = tostring(path) })
    end
  else
    file.write('error', 'agent not loaded', { error = tostring(result) })
  end
end

file.write('info', 'capabilities', probe(CAPABILITIES))

local function exists(path)
  local f = io.open(path, 'rb')
  if f then
    f:close()
    return true
  end
  return false
end

local function stop(message, path)
  file.write('warn', message, { path = path })
  dcslog(log.WARNING, 'disabled by ' .. path)
  disabled = true
  file.close()
end

local function guard(name, fn)
  return function(...)
    if disabled then
      return
    end
    local ok, err = pcall(fn, ...)
    if not ok then
      file.write('error', name .. ' raised', { error = tostring(err) })
    end
  end
end

-- The mission's file name without its folder, or ''.
local function mission_file()
  local path = DCS.getMissionFilename()
  if type(path) ~= 'string' then
    return ''
  end
  return path:match('([^/\\]+)$') or path
end

-- Load the shim into the server state behind a guard, so it loads once per
-- process, and start it. `reload` runs the shim at `shim` again over the one
-- loaded, keeping the agent it holds. Returns what the chunk returned and
-- whether it ran.
local BOOT = [[
local dll, shim, root, dir = %q, %q, %q, %q
if not __nanoprobe_shim then
  local loader, err = package.loadlib(dll, 'luaopen_nanoprobe')
  if not loader then
    error(err, 0)
  end
  local body = assert(loadfile(shim))
  __nanoprobe_shim = body(loader(), root, dir, nil)
end
return __nanoprobe_shim.start()
]]

local RELOAD = [[
local shim, root, dir = %q, %q, %q
local old = __nanoprobe_shim
if not old then
  error('no shim is loaded', 0)
end
local body = assert(loadfile(shim))
local new = body(old.agent, root, dir, old)
__nanoprobe_shim = new
return new.start()
]]

-- Whether a shim is loaded in the server state, for the pump.
local shim_loaded = false

-- The pump, one cross-state call on each sampling frame
-- (spec/sampling.md, "The frame"). The string is fixed, so no frame builds
-- one, and short, because net.dostring_in compiles it on every call: a
-- chunk with a local and a test costs about 10 µs more (LIVE, S10). The hook
-- calls it only while a shim is loaded.
local PUMP = '__nanoprobe_shim.pump()'
-- The simulation stop: the shim stops the extensions.
local STOP = '__nanoprobe_shim.simulation_stop()'

local function shim_start()
  local chunk = string.format(BOOT, dll, dir .. 'lua/shim.lua', root, dir)
  local text, ok = net.dostring_in('server', chunk)
  if ok then
    shim_loaded = true
    file.write('info', 'shim started', { result = text })
  else
    file.write('error', 'shim not started', { error = tostring(text) })
  end
end

-- Reload the shim of the version the manifest names now, which may be a
-- newer deploy than this hook's. The DLL stays the one loaded.
local function shim_reload()
  local m = io.open(root .. 'manifest.txt', 'rb')
  local active = m and m:read('*a'):match('active=([%w]+)')
  if m then
    m:close()
  end
  if not active then
    file.write('error', 'shim not reloaded', { error = 'no active version in the manifest' })
    return
  end
  local shimdir = root .. 'versions/' .. active .. '/'
  local chunk = string.format(RELOAD, shimdir .. 'lua/shim.lua', root, shimdir)
  local text, ok = net.dostring_in('server', chunk)
  if ok then
    file.write('info', 'shim reloaded', { version = active, result = text })
  else
    file.write('error', 'shim not reloaded', { version = active, error = tostring(text) })
  end
end

-- The position getter by object ID, when this DCS has it in this state: the
-- agent sweeps the units' positions with it inside `frame`, with no
-- cross-state call (spec/sampling.md, "The frame").
local BY_ID
do
  local ok, f = pcall(function()
    return DCS.getUnitPositionByObjectId
  end)
  if ok and type(f) == 'function' then
    BY_ID = f
  end
end

-- Commands whose handler runs in this state (spec/commands.md, "Handlers").
-- Each returns a result code, a value, and a message, as the shim's do, and
-- runs under pcall. The agent has admitted the command.
local APPLIED, NOT_BOUND, DCS_REFUSED, LUA_ERROR = 0, 18, 22, 23

-- The platform's unit name form, at most 15 characters (spec/world.md, "Names").
local NAME_MAX = 15
local UNIT_NAME = '^np%.[0-9A-Za-z]+$'
-- The agent formats the life: digits, a point and digits.
local LIFE_TEXT = '^%d+%.%d+$'

-- PICTURE's arguments, as the agent formats them: a number is digits, with a
-- point and digits for a real; a platform name is a group's or a unit's.
local INT_TEXT = '^%d+$'
local GROUP_NAME = '^np![0-9A-Za-z]+$'
-- A picture's file name goes into Lua source for the mission state, inside
-- double quotes. The agent rejects any name outside this set, which has no
-- quote, backslash or control character, so the name cannot leave the
-- string; the hook checks again because it builds the source. The set is
-- allowed, and the rest is rejected, because no escaping is needed then and
-- none can differ from what the mission state reads.
local FILE_TEXT = '^[%w _%.%-/%(%)%+,#]+$'
-- The mission state's picture function by scope: ALL, COALITION, COUNTRY,
-- GROUP and UNIT (spec/commands.md, "Layout: PICTURE").
local PICTURE_CALL = {
  [1] = 'a_out_picture', [2] = 'a_out_picture_s', [5] = 'a_out_picture_c',
  [3] = 'a_out_picture_g', [4] = 'a_out_picture_u',
}
-- a_out_picture_s takes the coalition by the Mission Editor's name for it,
-- and shows nothing for a number (LIVE, docs/results/2026-11-16-person-tests.md;
-- ED-CODE, MissionEditor/modules/Mission/CoalitionData.lua). a_out_picture_c
-- takes the DCS country number (ED-CODE, me_trigrules.lua countryLister).
local COALITION_NAME = { [0] = 'neutrals', [1] = 'red', [2] = 'blue' }

-- One call into another state. Returns its text, or nil and the reason.
local function cross(state, chunk)
  local text, ok = net.dostring_in(state, chunk)
  if not ok then
    return nil, tostring(text)
  end
  return text
end

-- The path of a MISSION_LOAD the dispatch handed back: the hook calls
-- `net.load_mission` with it after `finish`, and takes no further command in
-- the frame.
local pending_load


-- A DCS call's error text with every occurrence of `ucid` removed: a ucid
-- never goes into a RESULT message (spec/commands.md, "Player and ban
-- queries").
local function scrub(text, ucid)
  text = tostring(text)
  if type(ucid) ~= 'string' or ucid == '' then
    return text
  end
  local out, from = {}, 1
  while true do
    local a, b = text:find(ucid, from, true)
    if not a then
      break
    end
    out[#out + 1] = text:sub(from, a - 1)
    out[#out + 1] = '<ucid>'
    from = b + 1
  end
  out[#out + 1] = text:sub(from)
  return table.concat(out)
end

-- A value of a player's or a ban's fields the agent can carry: a string, a
-- number or a boolean, else nothing.
local function plain(v)
  local t = type(v)
  if t == 'string' or t == 'number' or t == 'boolean' then
    return v
  end
end

-- A slot as text: a number is its decimal text, anything but a string is ''.
local function slot_text(v)
  if type(v) == 'number' then
    return string.format('%d', v)
  end
  return type(v) == 'string' and v or ''
end

-- The connection ID of the server itself, which has no player record.
local SERVER = 1

-- MODERATE's actions (spec/commands.md, "Codes: moderation").
local KICK, BAN, UNBAN, SLOT = 1, 2, 3, 4

-- SLOT's side for the side the agent resolves, and the coalition names
-- DCS.getAvailableSlots takes (spec/commands.md, "Slot moves").
local KEEP = 4294967295
local COALITIONS = { [1] = 'red', [2] = 'blue' }

-- SLOT's refusals, each a fixed message (spec/commands.md, "Slot moves").
local NO_SIDE = 'DCS gave no side for the player'
local SEAT = 'a crew seat is not a slot the agent moves players into'
local NO_LIST = 'DCS gave no slot list'
local NOT_LISTED = 'the mission lists no slot with this ID'
local OTHER = 'the slot belongs to the other coalition'
local HELD = 'another player holds the slot'
local NO_PLAYERS = 'DCS gave no player list'
local NO_HOLDER = 'DCS gave no slot for another player'

-- Whether DCS lists `slot` for coalition `side` (1 or 2): true or false, or
-- nil when DCS gives no list. Entries are read with rawget and their
-- unitId compared as text.
local function lists_slot(side, slot)
  local ok, list = pcall(DCS.getAvailableSlots, COALITIONS[side])
  if not ok or type(list) ~= 'table' then
    return nil
  end
  local i = 1
  while true do
    local entry = rawget(list, i)
    if entry == nil then
      return false
    end
    if type(entry) == 'table' and slot_text(rawget(entry, 'unitId')) == slot then
      return true
    end
    i = i + 1
  end
end

-- Whether a connected player other than `player` holds `slot`: true or
-- false, or nil and a refusal when DCS gives no player list or a player's
-- slot read raises. A slot read as nil is no slot: the player left.
local function slot_held(player, slot)
  local ok, ids = pcall(net.get_player_list)
  if not ok or type(ids) ~= 'table' then
    return nil, NO_PLAYERS
  end
  local i = 1
  while true do
    local q = rawget(ids, i)
    if q == nil then
      return false
    end
    if q ~= SERVER and q ~= player then
      local got, held = pcall(net.get_player_info, q, 'slot')
      if not got then
        return nil, NO_HOLDER
      end
      if slot_text(held) == slot then
        return true
      end
    end
    i = i + 1
  end
end

-- The side a SLOT move goes to, or nil and a refusal message.
local function slot_side(player, side, slot)
  if slot == '' then
    if side ~= KEEP then
      return side
    end
    local ok, own = pcall(net.get_player_info, player, 'side')
    if ok and (own == 0 or own == 1 or own == 2) then
      return own
    end
    return nil, NO_SIDE
  end
  if slot:find('^.+_%d+$') then
    return nil, SEAT
  end
  local first = side == KEEP and 1 or side
  local listed = lists_slot(first, slot)
  if listed == nil then
    return nil, NO_LIST
  end
  local to = first
  if not listed then
    local other = lists_slot(3 - first, slot)
    if other == nil then
      return nil, NO_LIST
    elseif not other then
      return nil, NOT_LISTED
    elseif side ~= KEEP then
      return nil, OTHER
    end
    to = 3 - first
  end
  local held, why = slot_held(player, slot)
  if held == nil then
    return nil, why
  elseif held then
    return nil, HELD
  end
  return to
end

local HANDLERS = {
  -- CHAT: everyone for player 0, else one connection. DCS returns nothing
  -- useful, so APPLIED says only that the call did not raise.
  [55] = function(a)
    local text, player = a.text, a.player
    if type(text) ~= 'string' or text == '' or #text > 1024 then
      return DCS_REFUSED, nil, 'the text is not a string of 1 to 1,024 bytes'
    end
    if type(player) ~= 'number' or player < 0 or player ~= math.floor(player) then
      return DCS_REFUSED, nil, 'the player is not a connection ID'
    end
    local ok, err
    if player == 0 then
      ok, err = pcall(net.send_chat, text, true)
    else
      ok, err = pcall(net.send_chat_to, text, player)
    end
    if not ok then
      return DCS_REFUSED, nil, tostring(err)
    end
    return APPLIED
  end,
  -- MISSION_LOAD: answers APPLIED and hands the path to `run_commands`; the
  -- load happens after `finish`.
  [56] = function(a)
    local path = a.path
    if type(path) ~= 'string' or path == '' or #path > 1024 then
      return DCS_REFUSED, nil, 'the path is not a string of 1 to 1,024 bytes'
    end
    pending_load = path
    return APPLIED
  end,
  -- MODERATE: the player is checked by ucid first, since a connection ID
  -- can come to belong to someone else. A call that raises or returns false
  -- is DCS_REFUSED.
  [62] = function(a)
    local action, player, ucid = a.action, a.player, a.ucid
    local names_player = action == KICK or action == SLOT or (action == BAN and player ~= 0)
    if names_player then
      local ok, held = pcall(net.get_player_info, player, 'ucid')
      if not ok then
        return DCS_REFUSED, nil, 'DCS could not read the player: ' .. scrub(held, ucid)
      end
      if held == nil or held ~= ucid then
        return NOT_BOUND, nil, 'no connected player with that ucid'
      end
    end
    local ok, returned
    if action == KICK then
      ok, returned = pcall(net.kick, player, a.reason)
    elseif action == BAN and player ~= 0 then
      ok, returned = pcall(net.banlist_add, player, a.seconds, a.reason)
    elseif action == BAN then
      ok, returned = pcall(net.banlist_add_by_ucid, ucid, a.seconds, a.reason)
    elseif action == UNBAN then
      ok, returned = pcall(net.banlist_remove, ucid)
    elseif action == SLOT then
      local side, refused = slot_side(player, a.side, a.slot)
      if not side then
        return DCS_REFUSED, nil, refused
      end
      ok, returned = pcall(net.force_player_slot, player, side, a.slot)
    else
      return DCS_REFUSED, nil, 'not a moderation action'
    end
    if not ok then
      return DCS_REFUSED, nil, scrub(returned, ucid)
    elseif returned == false then
      return DCS_REFUSED, nil, 'DCS returned false'
    end
    return APPLIED
  end,
  -- PLAYERS: every connected player but the server.
  [66] = function()
    local ok, ids = pcall(net.get_player_list)
    if not ok or type(ids) ~= 'table' then
      return DCS_REFUSED, nil, 'DCS gave no player list'
    end
    local out = {}
    for _, id in ipairs(ids) do
      if id ~= SERVER then
        local got, info = pcall(net.get_player_info, id)
        if got and type(info) == 'table' then
          out[#out + 1] = {
            id = id, ucid = plain(info.ucid), name = plain(info.name),
            side = plain(info.side), slot = slot_text(info.slot),
          }
        end
      end
    end
    return APPLIED, out
  end,
  -- BANS: the ban list, each entry with its ucid, name, banned_until and
  -- reason only.
  [67] = function()
    local ok, list = pcall(net.banlist_get)
    if not ok or type(list) ~= 'table' then
      return DCS_REFUSED, nil, 'DCS gave no ban list'
    end
    local out = {}
    for _, entry in ipairs(list) do
      if type(entry) == 'table' then
        out[#out + 1] = {
          ucid = plain(entry.ucid), name = plain(entry.name),
          banned_until = plain(entry.banned_until), reason = plain(entry.reason),
        }
      end
    end
    return APPLIED, out
  end,
  -- PICTURE: the target's mission ID from the server state for a group or a
  -- unit, then the mission state's picture function from a fixed template
  -- whose every part was checked above or is a number checked here.
  [60] = function(a)
    local scope = type(a.scope) == 'string' and a.scope:find(INT_TEXT) and tonumber(a.scope)
    local call = scope and PICTURE_CALL[scope]
    if not call then
      return DCS_REFUSED, nil, 'the scope is not 1 to 5'
    end
    local file = a.file
    if type(file) ~= 'string' or #file > 255 or not file:find(FILE_TEXT) then
      return DCS_REFUSED, nil, 'the file name is not safe'
    end
    for _, key in ipairs({ 'seconds', 'size' }) do
      if type(a[key]) ~= 'string' or #a[key] > 12 or not a[key]:find(LIFE_TEXT) then
        return DCS_REFUSED, nil, 'the ' .. key .. ' is not formatted'
      end
    end
    for _, key in ipairs({ 'target', 'horizontal', 'vertical', 'size_units' }) do
      if type(a[key]) ~= 'string' or #a[key] > 10 or not a[key]:find(INT_TEXT) then
        return DCS_REFUSED, nil, 'the ' .. key .. ' is not formatted'
      end
    end
    if a.clear_view ~= 'true' and a.clear_view ~= 'false' then
      return DCS_REFUSED, nil, 'clear_view is not true or false'
    end
    -- The target the call names: none, a coalition name, a number, or a
    -- mission ID.
    local first = ''
    if scope == 2 then
      local side = COALITION_NAME[tonumber(a.target)]
      if not side then
        return DCS_REFUSED, nil, 'the coalition is not 0 to 2'
      end
      first = '"' .. side .. '", '
    elseif scope == 5 then
      first = a.target .. ', '
    elseif scope == 3 and a.group_id ~= nil then
      -- The agent's binding holds the group: its DCS group ID is the mission
      -- ID the picture function takes.
      if type(a.group_id) ~= 'string' or #a.group_id > 10 or not a.group_id:find(INT_TEXT) then
        return DCS_REFUSED, nil, 'the group ID is not formatted'
      end
      first = a.group_id .. ', '
    elseif scope == 4 and a.row ~= nil then
      -- The agent's binding holds the unit: the shim reads the ID of the
      -- object in the row.
      if type(a.row) ~= 'string' or #a.row > 10 or not a.row:find(INT_TEXT) then
        return DCS_REFUSED, nil, 'the row is not formatted'
      end
      local text, err = cross('server', 'return __nanoprobe_shim.row_id(' .. a.row .. ')')
      if not text then
        return DCS_REFUSED, nil, err
      end
      if text == '' then
        return NOT_BOUND, nil, 'no object in the row'
      end
      local id = tonumber(text)
      if not id or id ~= math.floor(id) or id < 0 or id >= 2 ^ 53 then
        return DCS_REFUSED, nil, 'the object has no integer mission ID'
      end
      first = string.format('%.0f, ', id)
    elseif scope == 3 or scope == 4 then
      local name = a.name
      if name == '' then
        return NOT_BOUND, nil, 'no campaign to name the object'
      end
      if type(name) ~= 'string' or #name > NAME_MAX
        or not name:find(scope == 3 and GROUP_NAME or UNIT_NAME) then
        return DCS_REFUSED, nil, 'not a platform name'
      end
      local class = scope == 3 and 'Group' or 'Unit'
      local text, err = cross('server', "local o = " .. class .. ".getByName('" .. name .. "') if o then return o:getID() end")
      if not text then
        return DCS_REFUSED, nil, err
      end
      if text == '' then
        return NOT_BOUND, nil, 'no object ' .. name
      end
      local id = tonumber(text)
      if not id or id ~= math.floor(id) or id < 0 or id >= 2 ^ 53 then
        return DCS_REFUSED, nil, 'the object has no integer mission ID'
      end
      first = string.format('%.0f, ', id)
    end
    local text, err = cross('mission', string.format(
      'if not %s then return "none" end %s(%sgetValueResourceByKey("%s"), %s, %s, 0, %s, %s, %s, %s) return "shown"',
      call, call, first, file, a.seconds, a.clear_view,
      a.horizontal, a.vertical, a.size, a.size_units))
    if not text then
      return DCS_REFUSED, nil, err
    elseif text == 'none' then
      return DCS_REFUSED, nil, 'the mission state has no ' .. call
    elseif text ~= 'shown' then
      return DCS_REFUSED, nil, 'the mission state answered ' .. text
    end
    return APPLIED
  end,
  -- PICTURE_STOP
  [61] = function()
    local text, err = cross('mission', 'a_out_picture_stop() return "stopped"')
    if not text then
      return DCS_REFUSED, nil, err
    elseif text ~= 'stopped' then
      return DCS_REFUSED, nil, 'the mission state answered ' .. text
    end
    return APPLIED
  end,
  -- DAMAGE: the unit's mission ID from the server state, then
  -- a_unit_set_life_percentage in the mission state. A unit that is not
  -- there is NOT_BOUND with no mission call: the setter revives a dead unit
  -- (LIVE, E38). A dead unit is NOT_BOUND too.
  [54] = function(a)
    local name, life = a.name, a.life_percent
    if type(name) ~= 'string' or #name > NAME_MAX or not name:find(UNIT_NAME) then
      return DCS_REFUSED, nil, 'not a platform unit name'
    end
    if type(life) ~= 'string' or not life:find(LIFE_TEXT) then
      return DCS_REFUSED, nil, 'the life is not formatted'
    end
    local text, err = cross('server', "local u = Unit.getByName('" .. name .. "') if u then return u:getID() end")
    if not text then
      return DCS_REFUSED, nil, err
    end
    if text == '' then
      return NOT_BOUND, nil, 'no unit ' .. name
    end
    local id = tonumber(text)
    if not id or id ~= math.floor(id) or id < 0 or id >= 2 ^ 53 then
      return DCS_REFUSED, nil, 'the unit has no integer mission ID'
    end
    text, err = cross('mission', string.format(
      'if c_unit_dead(%.0f) then return "dead" end a_unit_set_life_percentage(%.0f, %s) return "set"', id, id, life))
    if not text then
      return DCS_REFUSED, nil, err
    elseif text == 'dead' then
      return NOT_BOUND, nil, 'the unit is dead'
    elseif text ~= 'set' then
      return DCS_REFUSED, nil, 'the mission state answered ' .. text
    end
    return APPLIED
  end,
}

-- Run the commands the agent hands out for this state, in ring order, until
-- it has none. Any hook callback can call it; `onSimulationFrame` does when
-- `frame` says a command waits. A command taken and not answered is answered
-- `LUA_ERROR` "no result" by the agent.
local function run_commands()
  while true do
    local opcode, args = agent.command(true)
    if not opcode then
      return
    end
    local handler = HANDLERS[opcode]
    if not handler then
      agent.command_done(20, 'no handler in the hook')
    else
      local ok, code, value, message = pcall(handler, args or {})
      if not ok then
        agent.command_done(LUA_ERROR, tostring(code))
      elseif type(code) ~= 'number' then
        agent.command_done(LUA_ERROR, 'the handler returned no code')
      else
        agent.command_done(code, message, value)
      end
    end
    -- A mission load takes no further command in this frame.
    if pending_load then
      return
    end
  end
end

-- Start the load a MISSION_LOAD handed back. `net.load_mission` returns at
-- once and DCS loads on a later frame; what it returned is logged, because a
-- load DCS cannot do shows nowhere else.
local function start_load()
  local path = pending_load
  pending_load = nil
  if not path then
    return
  end
  local ok, returned = pcall(net.load_mission, path)
  file.write(ok and 'info' or 'error', 'mission load requested', { path = path, returned = tostring(returned) })
end

-- What the hook last held for each connected player, by connection ID: the
-- fields `net.get_player_info` gave, for a DISCONNECT, whose callback is not
-- relied on to answer it.
local players = {}

-- The player events (spec/records.md, "Layout: PLAYER").
local CONNECT, DISCONNECT, CHANGE_SLOT, CHAT = 1, 2, 3, 4

-- The seat of a slot ID: N - 1 for a `_N` suffix, else 0.
local function seat_of(slot)
  local n = type(slot) == 'string' and slot:match('_(%d+)$')
  n = n and tonumber(n)
  if n and n >= 1 then
    return n - 1
  end
  return 0
end

-- A player's fields from DCS, or nil when it has none to give.
local function read_player(id)
  local ok, info = pcall(net.get_player_info, id)
  if not ok or type(info) ~= 'table' or type(info.ucid) ~= 'string' or info.ucid == '' then
    return nil
  end
  return {
    client = info.ucid,
    name = type(info.name) == 'string' and info.name or '',
    side = type(info.side) == 'number' and info.side or 0,
    slot = slot_text(info.slot),
  }
end

-- Write the player record. The server's own connection writes none.
local function send_player(event, id, p, text)
  if not linked or type(id) ~= 'number' or id == SERVER then
    return
  end
  agent.player(event, id, p.side, seat_of(p.slot), p.client, p.name, p.slot, text or '')
end

local callbacks = {}

callbacks.onMissionLoadBegin = guard('onMissionLoadBegin', function()
  generation = generation + 1
  if linked then
    generation = agent.lifecycle('loading', '', '', DCS.getRealTime(), DCS.getModelTime()) or generation
  end
  file.write('info', 'mission load begin', { mission = DCS.getMissionName() })
end)

callbacks.onSimulationStart = guard('onSimulationStart', function()
  local paused = DCS.getPause()
  if linked then
    local ok, theatre = pcall(DCS.getMissionTheatre)
    theatre = ok and type(theatre) == 'string' and theatre or ''
    agent.lifecycle(paused and 'paused' or 'running', theatre, mission_file(), DCS.getRealTime(), DCS.getModelTime())
  end
  file.write('info', 'simulation start', { paused = paused })
  if agent then
    shim_start()
  end
end)

callbacks.onSimulationStop = guard('onSimulationStop', function()
  if linked then
    agent.lifecycle('stopping', '', mission_file(), DCS.getRealTime(), DCS.getModelTime())
  end
  -- The last callback in which the mission is still there: the extensions
  -- stop here, since no frame follows (spec/commands.md, "Running").
  if shim_loaded then
    local text, ok = net.dostring_in('server', STOP)
    if not ok then
      file.write('error', 'extensions not stopped', { error = tostring(text) })
    end
  end
  file.write('info', 'simulation stop')
end)

callbacks.onPlayerConnect = guard('onPlayerConnect', function(id)
  if id == SERVER then
    return
  end
  local p = read_player(id)
  if p then
    players[id] = p
    send_player(CONNECT, id, p)
  end
end)

callbacks.onPlayerDisconnect = guard('onPlayerDisconnect', function(id, err_code)
  -- What the hook held; a player who was connected before the hook loaded is
  -- read now, which DCS may no longer answer.
  local p = players[id] or read_player(id)
  players[id] = nil
  if p then
    send_player(DISCONNECT, id, p, type(err_code) == 'number' and string.format('%d', err_code) or '')
  end
end)

callbacks.onPlayerChangeSlot = guard('onPlayerChangeSlot', function(id)
  if id == SERVER then
    return
  end
  local p = read_player(id)
  if p then
    players[id] = p
    send_player(CHANGE_SLOT, id, p)
  end
end)

-- Never returns a value: a returned value would decide the matter for every
-- other hook (ED-CODE, API/Sim_ControlAPI.md).
callbacks.onPlayerTrySendChat = guard('onPlayerTrySendChat', function(id, message)
  if id == SERVER then
    return
  end
  local p = read_player(id)
  if p then
    players[id] = p
    send_player(CHAT, id, p, type(message) == 'string' and message or '')
  end
end)

callbacks.onSimulationFrame = guard('onSimulationFrame', function()
  if linked then
    local code, pump, hook = agent.frame(DCS.getRealTime(), DCS.getModelTime(), DCS.getPause(), BY_ID, shim_loaded)
    -- A frame that needs neither a pump nor the hook's commands ends inside
    -- `frame`. Otherwise the pump runs, then the hook's commands, then
    -- `finish`.
    if pump or hook then
      if pump and shim_loaded then
        net.dostring_in('server', PUMP)
      end
      if hook then
        local ran, err = pcall(run_commands)
        if not ran then
          file.write('error', 'hook commands raised', { error = tostring(err) })
        end
      end
      agent.finish()
      start_load()
    end
    if code == nil then
      -- A caught panic disabled the agent: every entry returns nothing. The
      -- hook goes on without the link.
      linked = false
      file.write('error', 'the agent is disabled; the hook goes on without it', agent.status())
    elseif code == FRAME_DISABLED then
      stop('kill switch found; stopping', root .. 'disable')
    elseif code == FRAME_RELOAD then
      shim_reload()
    end
    return
  end
  local now = DCS.getRealTime()
  if now < next_check then
    return
  end
  next_check = now + 1
  if exists(root .. 'disable') then
    stop('kill switch found; stopping', root .. 'disable')
  end
end)

DCS.setUserCallbacks(callbacks)
