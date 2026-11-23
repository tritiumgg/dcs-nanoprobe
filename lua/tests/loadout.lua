-- The loadout read (spec/sampling.md, "Loadout reads"), with the real shim
-- and agent on the mock, and `link-probe --record` as the sidecar. A birth
-- in a slot a connected player holds marks its initiator, and so does that
-- unit's takeoff; the next pump asks the unit for its player name and reads
-- `getAmmo`:
--
--   a. the birth of a player unit writes exactly one AMMO record, cause 1;
--   b. the birth of an AI unit writes none, and the unit is never asked for
--      a player name;
--   c. a player unit's takeoff writes exactly one more, cause 2, and an AI
--      unit's takeoff none;
--   d. many pumps with no event write none: nothing reads ammunition on a
--      period;
--   e. a birth in a slot no connected player holds is not read.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/loadout.lua <module> <version> <link-probe>')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local OBJECT, AMMO = 16, 35
local BIRTH, TAKEOFF = 1, 2

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local out = profile .. 'probe.jsonl'
local pidfile = profile .. 'probe.pid'
local recording = profile .. 'recording.jsonl'

-- A little-endian unsigned integer of `n` bytes at byte `off` of a hex string.
local function uint(hex, off, n)
  local v = 0
  for i = n - 1, 0, -1 do
    v = v * 256 + tonumber(hex:sub((off + i) * 2 + 1, (off + i) * 2 + 2), 16)
  end
  return v
end

-- An IEEE 754 double, little-endian, at byte `off` of a hex string.
local function f64(hex, off)
  local b = {}
  for i = 1, 8 do
    b[i] = uint(hex, off + i - 1, 1)
  end
  local sign = b[8] >= 128 and -1 or 1
  local e = (b[8] % 128) * 16 + math.floor(b[7] / 16)
  local m = b[7] % 16
  for i = 6, 1, -1 do
    m = m * 256 + b[i]
  end
  if e == 0 then
    return sign * m * 2 ^ -1074
  end
  return sign * (1 + m / 2 ^ 52) * 2 ^ (e - 1023)
end

-- The `lv` at byte `off` of a hex string (spec/transport.md, "Codes: lv_tag"):
-- the value and the offset after it.
local function lv(hex, off)
  local tag = uint(hex, off, 1)
  off = off + 1
  if tag == 0 then
    return nil, off
  elseif tag == 1 or tag == 2 then
    return tag == 2, off
  elseif tag == 3 then
    return f64(hex, off), off + 8
  elseif tag == 4 then
    local n = uint(hex, off, 4)
    off = off + 4
    local s = hex:sub(off * 2 + 1, (off + n) * 2):gsub('%x%x', function(h)
      return string.char(tonumber(h, 16))
    end)
    return s, off + n
  elseif tag == 5 then
    local n = uint(hex, off, 4)
    off = off + 4
    local t = {}
    for _ = 1, n do
      local k, v
      k, off = lv(hex, off)
      v, off = lv(hex, off)
      t[k] = v
    end
    return t, off
  end
  error('lv tag ' .. tag)
end

-- The recording's AMMO records, in order: object, cause and the rows of the
-- `ammo` array, each with `type` and `count`.
local function ammo_records()
  local list = {}
  for _, line in ipairs(S.lines(recording)) do
    local ok, r = pcall(S.json, line)
    if ok and type(r) == 'table' and r.kind == AMMO then
      list[#list + 1] = {
        object = uint(r.payload, 0, 8),
        cause = uint(r.payload, 8, 4),
        rows = (lv(r.payload, 16)),
      }
    end
  end
  return list
end

local function count(object)
  local n, last = 0, nil
  for _, r in ipairs(ammo_records()) do
    if r.object == object then
      n, last = n + 1, r
    end
  end
  return n, last
end

local function main()
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(version .. 'lua')
  S.mkdir(profile .. 'nanoprobe/logs')
  S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
  S.copy(root .. 'lua/agent/shim.lua', version .. 'lua/shim.lua')
  S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
  S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
  local port = 20000 + math.floor(os.clock() * 1e6 + os.time()) % 20000
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 4096\nin_ring_kib = 256\n\n"
      .. "[sampling]\nenumeration_s = 5.0\n", map, port))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  assert(package.loadlib(version .. 'nanoprobe_agent_' .. BUILD .. '.dll', 'luaopen_nanoprobe'))

  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('sampling')
  local sim = host.sim
  -- Count the `getAmmo` calls on each unit.
  local Unit = host.states.server.Unit
  local real_getAmmo = Unit.getAmmo
  local ammo_calls = {}
  rawset(Unit, 'getAmmo', function(self, ...)
    ammo_calls[self.id_] = (ammo_calls[self.id_] or 0) + 1
    return real_getAmmo(self, ...)
  end)
  -- And the `getPlayerName` calls: a unit no player occupies gets none.
  local real_getPlayerName = Unit.getPlayerName
  local name_calls = {}
  rawset(Unit, 'getPlayerName', function(self, ...)
    name_calls[self.id_] = (name_calls[self.id_] or 0) + 1
    return real_getPlayerName(self, ...)
  end)

  assert(os.execute(string.format(
    '%s --map %s --port %d --token-file %s --seconds 600 --record %s >> %s 2>&1 & echo $! > %s',
    S.quote(PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), S.quote(recording),
    S.quote(out), S.quote(pidfile))) == 0, 'could not start link-probe')

  -- Frames at about 100 a second until `done` is true, or `seconds` pass.
  local function run(seconds, done)
    local deadline = now() + seconds
    local next_frame = now()
    while now() < deadline do
      if now() >= next_frame then
        next_frame = next_frame + 0.01
        host:frame(0.01)
        if done and done() then
          return true
        end
      end
    end
    return done == nil
  end

  assert(run(15, function()
    for _, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' and e.ev == 'hello' then
        return true
      end
    end
    return false
  end), 'the probe did not attach:\n' .. (S.read(out) or ''))
  -- The attach's enumeration settles before the test adds units.
  run(2)

  local usa = host.states.server.country.id.USA
  local function add(name, x, player)
    sim:add_group(usa, 0, { name = name, units = { { name = name .. '-1', type = 'F-15C', x = x, y = 0,
      alt = 1000, player = player } } })
    return sim.by_name[name .. '-1']
  end
  -- The player takes the slot before the unit is born in it, as in DCS, and
  -- the birth's mission ID is the slot ID (LIVE, spec/records.md). The agent
  -- marks a birth only in a slot a connected player holds.
  host:connect(2, { ucid = 'ucid-pilot', name = 'Pilot', side = 2, slot = '' })
  host:change_slot(2, 2, tostring(sim.next_mission))
  local pilot = add('np-ld-player', 41000, 'Pilot')
  assert(tostring(pilot.mission_id) == host.players[2].slot, 'the mock gave the unit another mission ID')
  local ai = add('np-ld-ai', 41100, nil)
  -- (e) A unit a player sits in whose slot no connected player holds: the
  -- agent has no way to know of the player without a read, and makes none.
  local stray = add('np-ld-stray', 41200, 'Stray')

  -- (a, b) The player's birth writes one AMMO record with cause 1; the AI
  -- unit's birth writes none. The waits for a record end on the record; their
  -- 120 s only stops a hang. The agent spends a small share of each frame, and
  -- on a loaded runner one pass of its enumeration took 18 s
  -- (`enumeration_s` in its sampling log).
  assert(run(120, function()
    return count(pilot.id) >= 1
  end), 'no AMMO record for the player unit after its birth')
  -- Let the AI unit's birth resolve too, and every later pump run.
  run(3)
  local n, first = count(pilot.id)
  assert(n == 1, 'the player unit has ' .. n .. ' AMMO records after its birth, wanted 1')
  assert(first.cause == BIRTH, 'the birth read has cause ' .. first.cause)
  assert(count(ai.id) == 0, 'the AI unit has an AMMO record after its birth')
  -- The rows are the ones the mock's getAmmo gives, by type name and count.
  local want = real_getAmmo(Unit.getByName('np-ld-player-1'))
  assert(want and #want > 0, 'the mock gives the player unit no ammunition')
  assert(#first.rows == #want, 'the AMMO record has ' .. #first.rows .. ' rows, wanted ' .. #want)
  for i, row in ipairs(want) do
    assert(first.rows[i].type == row.desc.typeName, 'row ' .. i .. ' has type ' .. tostring(first.rows[i].type))
    assert(first.rows[i].count == row.count, 'row ' .. i .. ' has count ' .. tostring(first.rows[i].count))
  end
  assert((ammo_calls[pilot.id] or 0) >= 1, 'the shim did not read the player unit\'s ammunition')
  assert((ammo_calls[ai.id] or 0) == 0, 'the shim called getAmmo on the AI unit after its birth')
  assert((name_calls[ai.id] or 0) == 0, 'the shim asked the AI unit for a player name after its birth')
  assert(count(stray.id) == 0 and (name_calls[stray.id] or 0) == 0,
    'a birth in a slot no connected player holds was read')
  S.ok('a player unit\'s birth gives exactly one AMMO record with cause birth; an AI unit\'s gives none and costs no read')

  -- (c) The takeoffs: one more record for the player, none for the AI unit.
  sim:takeoff(pilot)
  sim:takeoff(ai)
  assert(run(120, function()
    return count(pilot.id) >= 2
  end), 'no AMMO record for the player unit after its takeoff')
  run(3)
  local m, last = count(pilot.id)
  assert(m == 2, 'the player unit has ' .. m .. ' AMMO records after its takeoff, wanted 2')
  assert(last.cause == TAKEOFF, 'the takeoff read has cause ' .. last.cause)
  assert(count(ai.id) == 0, 'the AI unit has an AMMO record after its takeoff')
  assert((ammo_calls[ai.id] or 0) == 0, 'the shim called getAmmo on the AI unit after its takeoff')
  assert((name_calls[ai.id] or 0) == 0, 'the shim asked the AI unit for a player name after its takeoff')
  S.ok('a player unit\'s takeoff gives exactly one AMMO record with cause takeoff; an AI unit\'s gives none')

  -- (d) Nothing reads ammunition on a period: many pumps, no new record.
  local total = #ammo_records()
  run(10)
  assert(#ammo_records() == total, 'an AMMO record came with no event')
  S.ok('pumps with no event write no AMMO record')
end

local ok, err = pcall(main)
local pid = S.read(pidfile)
if pid then
  os.execute('kill ' .. pid:match('%d+') .. ' 2>/dev/null')
end
if not ok then
  io.stderr:write(tostring(err), '\n')
  io.stderr:write('probe output:\n', (S.read(out) or ''):sub(-3000), '\n')
  io.stderr:write('agent log:\n', (S.read(profile .. 'nanoprobe/logs/agent.log') or ''):sub(-2000), '\n')
  os.exit(1)
end
