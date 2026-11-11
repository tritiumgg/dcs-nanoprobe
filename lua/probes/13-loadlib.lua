-- package.loadlib in the server state reaches the system's library loader.
-- The probe asks for a file that does not exist, so nothing is loaded.
-- states: server

local E = ...
local path = lfs.writedir() .. 'nanoprobe/dev/no-such-library.dll'
local r = E.pack(package.loadlib(path, 'luaopen_none'))

E.observe('returns', r.n, { windows = 3 })
E.observe('first_is_nil', r[1] == nil, { windows = true })
-- Stock Lua 5.1 names the step that failed: `open` for a file it could not
-- load.
E.observe('failed_step', r[3], { windows = 'open' })
-- The message is the system's own: "The specified module could not be
-- found" on Windows, "Module not found" under Wine (LIVE on both).
local message = tostring(r[2])
E.observe('message_is_module_not_found', message:find('module could not be found', 1, true) ~= nil,
  { windows = true, wine = false })
E.observe('message_is_wine_text', message:find('Module not found', 1, true) ~= nil,
  { windows = false, wine = true, mock = false })
E.observe('message', (message:gsub(path, '<path>', 1):gsub('[A-Za-z]:[\\/][^\n]*%.dll', '<path>'):sub(1, 200)),
  { info = true })
