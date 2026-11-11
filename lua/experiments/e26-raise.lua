-- E26: a Lua error raised through the agent's entry shape, and one raised
-- inside a lua_pcall the agent makes, under the C runtime of the platform.
-- The rule: caught by lua_pcall, no crash.
-- states: hook server

local E = ...
local agent = E.lib('agent').open()
local N = 1000

local caught = 0
for i = 1, N do
  local ok, message = pcall(agent.raise, 'raise ' .. i)
  if not ok and message == 'raise ' .. i then
    caught = caught + 1
  end
end
E.observe('raise_caught', caught, { windows = N })

local inner = 0
for i = 1, N do
  local ok, message = agent.call(function()
    error('inner ' .. i, 0)
  end)
  if ok == false and message == 'inner ' .. i then
    inner = inner + 1
  end
end
E.observe('call_caught', inner, { windows = N })

-- A raise from a nested call: Lua calls the agent, which calls Lua, which
-- calls the agent's raise.
local nested_ok, nested = agent.call(function()
  return pcall(agent.raise, 'nested')
end)
E.observe('nested', { nested_ok, nested }, { windows = { true, false } })
E.observe('still_enabled', agent.status().disabled, { windows = false })
E.observe('thread_after', type(agent.thread()), { windows = 'number' })
return 'done'
