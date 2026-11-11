-- The agent opens under a stock Lua 5.1, the way DCS opens it:
-- package.loadlib against an explicit path, never require.
--
-- Every check reads a value, so a run that exercised nothing fails.
--
-- Run it through tools/luatest.sh, which builds the module and finds it.

local path, expected = ...

local usage = 'usage: lua lua/tests/load.lua <module path> <expected version>'
assert(path, usage)
assert(expected, usage)

assert(package.loadlib, 'package.loadlib is missing, so this is not Lua 5.1')

-- Lua 5.1 on macOS, built without LUA_USE_MACOSX, loads through the old dyld
-- API, which wants the Mach-O name with its leading underscore. Every dlopen
-- build, and DCS, takes the bare name. Try the bare name first, so wherever it
-- works the test has used the spelling DCS uses.
local function open(module)
  for _, name in ipairs({ 'luaopen_nanoprobe', '_luaopen_nanoprobe' }) do
    local loader = package.loadlib(module, name)
    if loader then
      return loader, name
    end
  end

  local _, err = package.loadlib(module, 'luaopen_nanoprobe')
  error('no opener in ' .. module .. ': ' .. tostring(err), 0)
end

local loader, symbol = open(path)
local agent = loader()

assert(type(agent) == 'table', 'luaopen_nanoprobe returned a ' .. type(agent))
assert(
  agent.version == expected,
  'version reads ' .. tostring(agent.version) .. ', expected ' .. expected
)
assert(type(agent.abi) == 'string' and agent.abi ~= '', 'abi is missing')
assert(agent.opens == 1, 'the first open reads ' .. tostring(agent.opens))

-- Opening is idempotent: a state that opens the agent again gets its first
-- table back, and the process-wide count of states does not move.
local second = (open(path))()
assert(second == agent, 'a second open in the same state returned another table')
assert(second.opens == 1, 'the second open reads ' .. tostring(second.opens))

print('ok  opened ' .. path)
print('ok  ' .. symbol .. ' returns a table, version ' .. agent.version .. ', abi ' .. agent.abi)
print('ok  a second open in the same state returns the same table')
