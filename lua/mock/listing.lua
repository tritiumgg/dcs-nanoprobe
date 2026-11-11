-- What each DCS Lua state holds, for the strict mock: read from the listings
-- in lua/mock/api/<state>.txt.
--
-- `cargo xtask api-dump` writes those files from a running DCS with
-- lua/probes/tools/api-dump.lua, and each file's header gives the DCS build,
-- the platform and the date. Every name in them is LIVE for that build and
-- platform.
--
-- A read of a name that is not listed raises "not in the listing". A call of a
-- listed function the mock does not implement raises "not mocked". The two
-- errors differ so a test failure says whether the code under test is wrong or
-- the mock is short.

local here = debug.getinfo(1, 'S').source:match('^@(.*[/\\])') or './'

local M = { states = {} }

-- `hook` is the state DCS runs Scripts/Hooks files in, and `sse` the sandboxed
-- mission scripting environment. Neither has a name in DCS.
M.names = { 'hook', 'gui', 'server', 'mission', 'export', 'config', 'sse' }

for _, state in ipairs(M.names) do
  -- kinds[path] is the type of the value at a dotted path; values[path] its
  -- value when it is a number or a boolean; members[path] the names directly
  -- under a table, and globals the names at the top.
  -- alias[path] is the path of the same table where it was listed first.
  local listing = { kinds = {}, values = {}, members = {}, globals = {}, alias = {}, header = {} }
  for line in io.lines(here .. 'api/' .. state .. '.txt') do
    local key, text = line:match('^# (%a+) (.*)$')
    if key then
      listing.header[key] = text
    else
      local path, kind, rest = line:match('^([%a_][%w_.]*) : (%a+)(.*)$')
      if path and kind ~= 'other' then
        listing.kinds[path] = kind
        local value = rest:match('^ = (.*)$')
        listing.alias[path] = rest:match('^ %-> (.*)$')
        if kind == 'number' then
          listing.values[path] = tonumber(value)
        elseif kind == 'boolean' then
          listing.values[path] = value == 'true'
        end
        local parent, name = path:match('^(.*)%.([^.]+)$')
        if parent then
          listing.members[parent] = listing.members[parent] or {}
          table.insert(listing.members[parent], name)
        else
          listing.globals[path] = kind
        end
      end
    end
  end
  M.states[state] = listing
end

-- `scripting` names the same state as `server` (LIVE).
M.states.scripting = M.states.server

M.build = M.states.server.header.dcs or 'unknown'
M.platform = M.states.server.header.platform or 'unknown'

-- The numbers and booleans directly under `path` in `state`, by name.
function M.constants(state, path)
  local listing = M.states[state]
  local out = {}
  for _, name in ipairs(listing.members[path] or {}) do
    local value = listing.values[path .. '.' .. name]
    if value ~= nil then
      out[name] = value
    end
  end
  return out
end

return M
