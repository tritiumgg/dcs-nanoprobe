-- E2: with net.allow_unsafe_api and net.allow_dostring_in set in
-- autoexec.cfg (`cargo xtask gates <target> on`), does the hook still reach
-- the server state, and is the sandbox's call refused? Run once with the
-- keys set; the run without them is the S3 suite. The rule: the bootstrap
-- works with the keys set and the sandbox call fails.
-- states: hook sse

local E = ...

-- `net.dostring_in` returns the chunk's value as a string and whether it ran
-- (LIVE), so a call that reached the state reads "true, reached, true".
local function reached(text)
  return text:find('true, reached', 1, true) == 1
end

local function try(state)
  local r = E.pack(pcall(net.dostring_in, state, 'return "reached"'))
  local parts = {}
  for i = 1, r.n do
    parts[#parts + 1] = tostring(r[i])
  end
  return table.concat(parts, ', ')
end

if E.state == 'hook' then
  for _, state in ipairs({ 'server', 'scripting', 'gui', 'mission', 'export', 'config' }) do
    E.observe('hook_to.' .. state, try(state), { info = true })
  end
  E.observe('hook_reaches_server', reached(try('server')), { windows = true })
  -- The hook opened and configured the agent at start if this state's
  -- agent already has a root before this snippet configures anything. DCS's
  -- io.open cannot read hook.log while the hook holds it open (LIVE).
  local root = lfs.writedir() .. 'nanoprobe/'
  local manifest = io.open(root .. 'manifest.txt', 'rb')
  local build = manifest and manifest:read('*a'):match('active=(%w+)')
  if manifest then
    manifest:close()
  end
  local loader = build and package.loadlib(root .. 'versions/' .. build .. '/nanoprobe_agent_' .. build .. '.dll',
    'luaopen_nanoprobe')
  local loaded = loader ~= nil and loader().status().root ~= nil
  E.observe('agent_loaded_at_start', loaded, { windows = true })
  local ok, value = pcall(net.dostring_in, 'config', 'return tostring(net and net.allow_unsafe_api ~= nil)')
  E.observe('config_sees_unsafe_api_key', ok and value or tostring(value), { info = true })
else
  E.observe('sandbox_to.server', try('server'), { info = true })
  -- The mock has no gates, so its sandbox reaches the server state.
  E.observe('sandbox_refused', not reached(try('server')), { windows = true, mock = false })
end
return 'done'
