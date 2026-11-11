-- What each state holds: the base libraries a sandbox removes, the scripting
-- API, and the cross-state calls.
-- states: hook gui server mission export config sse

local E = ...

-- The Windows client's values (LIVE there), by state. A name left out has
-- no recorded value. On the client the hook state and `gui` are one.
local expected = {
  hook = {
    io = 'table', os = 'table', lfs = 'table', debug = 'table', package = 'table', require = 'function',
    Unit = 'nil', world = 'nil', DCS = 'table', Sim = 'table', Export = 'table', net = 'table',
    terrain = 'table', Weather = 'table', a_do_script = 'nil', jit = 'nil',
  },
  gui = {
    io = 'table', os = 'table', lfs = 'table', debug = 'table', package = 'table', require = 'function',
    Unit = 'nil', world = 'nil', DCS = 'table', Sim = 'table', Export = 'table', net = 'table',
    terrain = 'table', Weather = 'table', a_do_script = 'nil', jit = 'nil',
  },
  server = {
    io = 'table', os = 'table', lfs = 'table', debug = 'table', package = 'table', require = 'function',
    Unit = 'table', world = 'table', timer = 'table', coalition = 'table', DCS = 'table', Sim = 'table',
    Export = 'table', net = 'nil', a_do_script = 'nil', jit = 'nil',
  },
  mission = {
    io = 'nil', os = 'nil', package = 'nil', Unit = 'nil', world = 'nil', net = 'nil',
    a_do_script = 'function', a_unit_set_life_percentage = 'function', mission = 'table', db = 'table',
  },
  export = {
    io = 'table', os = 'table', lfs = 'table', package = 'table', require = 'function',
    Unit = 'nil', net = 'nil', LoGetWorldObjects = 'function',
  },
  config = {
    io = 'table', os = 'table', lfs = 'table', package = 'table', require = 'function',
    Unit = 'nil', net = 'table',
  },
  sse = {
    io = 'nil', os = 'nil', lfs = 'nil', package = 'nil', require = 'nil',
    Unit = 'table', world = 'table', timer = 'table', coalition = 'table', net = 'table',
  },
}

local names = {
  'io', 'os', 'lfs', 'debug', 'package', 'require', 'loadstring', 'setfenv', 'jit',
  'Unit', 'Group', 'world', 'timer', 'coalition', 'trigger', 'env', 'land', 'coord',
  'DCS', 'Sim', 'Export', 'net', 'log', 'terrain', 'Weather', 'socket',
  'a_do_script', 'a_unit_set_life_percentage', 'mission', 'db', 'LoGetWorldObjects',
}

local want = expected[E.state] or {}
for _, name in ipairs(names) do
  local kind = type(E.global(name))
  E.observe('has.' .. name, kind, want[name] and { windows = want[name] } or { info = true })
end

-- A member of a table that may be missing, read without raising.
local function member(table_name, key)
  local t = E.global(table_name)
  if type(t) ~= 'table' then
    return 'nil'
  end
  local ok, value = pcall(function()
    return t[key]
  end)
  return ok and type(value) or 'nil'
end

-- package.loadlib is present in every state that is not sandboxed, and
-- net.dostring_in in the hook state, the config state and the sandbox (LIVE).
local loadlib = { hook = 'function', gui = 'function', server = 'function', export = 'function',
  config = 'function', mission = 'nil', sse = 'nil' }
E.observe('package.loadlib', member('package', 'loadlib'), { windows = loadlib[E.state] })
local dostring = { hook = 'function', gui = 'function', sse = 'function', server = 'nil', mission = 'nil',
  export = 'nil' }
E.observe('net.dostring_in', member('net', 'dostring_in'),
  dostring[E.state] and { windows = dostring[E.state] } or { info = true })
E.observe('world.setPersistenceHandler', member('world', 'setPersistenceHandler'),
  E.state == 'server' and { windows = 'function' } or { info = true })
E.observe('coalition.add_dyn_group', member('coalition', 'add_dyn_group'),
  E.state == 'server' and { windows = 'function' } or { info = true })
E.observe('_VERSION', E.global('_VERSION'), { windows = 'Lua 5.1' })
