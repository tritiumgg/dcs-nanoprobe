-- net.dostring_in from the hook: which state names it reaches, what it
-- returns, and what it does with a chunk that raises.
-- states: hook

local E = ...

-- What a call gave back: how many values, and each one's type and text.
-- A table's address is masked, so two runs compare.
local function call(state, chunk)
  local r = E.pack(pcall(net.dostring_in, state, chunk))
  local out = { raised = not r[1], n = r.n - 1, types = {}, values = {} }
  for i = 2, r.n do
    out.types[i - 1] = type(r[i])
    out.values[i - 1] = tostring(r[i]):gsub('0x%x+', '0x?'):gsub(': %x%x%x%x%x%x+', ': ?'):sub(1, 120)
  end
  return out
end

local function address(state)
  local ok, value = pcall(net.dostring_in, state, 'return tostring(_G)')
  if ok and type(value) == 'string' and value:match('^table') then
    return value
  end
  return nil
end

-- net.dostring_in reached all six names with neither gate key set, and an
-- unknown name returns nil without an error (LIVE).
local reach = {
  gui = true, server = true, scripting = true, mission = true, export = true, config = true,
  userhooks = false, hooks = false, net = false, lobby = false, main = false,
}
local names = {}
for name in pairs(reach) do
  names[#names + 1] = name
end
table.sort(names)
for _, name in ipairs(names) do
  E.observe('reach.' .. name, address(name) ~= nil, { windows = reach[name] })
end
E.observe('unknown_name', call('hooks', 'return 1'), { info = true })

-- `server` and `scripting` are one state (LIVE).
E.observe('server_is_scripting', address('server') == address('scripting'), { windows = true })
-- On a client a hook's globals are the `gui` state's. On a dedicated server
-- they are another table, and a global set in one is absent from the other
-- (LIVE on both).
E.observe('hook_is_gui', tostring(_G) == address('gui'), { windows = true, wine = false })
_G.np_probe_mark = 1
local seen = call('gui', 'return tostring(rawget(_G, "np_probe_mark"))')
_G.np_probe_mark = nil
E.observe('gui_sees_hook_global', seen.values[1] == '1', { windows = true, wine = false })
E.observe('gui_DCS_is_hook_DCS', call('gui', 'return tostring(DCS)').values[1] == tostring(DCS), { info = true })

-- What comes back for each kind of return.
for _, case in ipairs({
  { 'number', 'return 1' },
  { 'fraction', 'return 1.5' },
  { 'string', "return 'text'" },
  { 'two', "return 'a', 'b'" },
  { 'true', 'return true' },
  { 'false', 'return false' },
  { 'nil', 'return nil' },
  { 'nothing', 'return' },
  { 'table', 'return {}' },
  { 'syntax', 'return (' },
  { 'raise', "error('probe raise')" },
  { 'raise_plain', "error('probe raise', 0)" },
  { 'raise_table', 'error({})' },
}) do
  E.observe('return.' .. case[1], call('server', case[2]), { info = true })
end

-- A value comes back as a string and `true`: the text of a string or a
-- number, and an empty string for anything else (LIVE under Wine).
E.observe('number_comes_back_as', call('server', 'return 1'), { wine = {
  raised = false, n = 2, types = { 'string', 'boolean' }, values = { '1', 'true' },
} })
E.observe('table_comes_back_as', call('server', 'return {}'), { wine = {
  raised = false, n = 2, types = { 'string', 'boolean' }, values = { '', 'true' },
} })
-- A chunk that raises returns its error text as an ordinary string (PRIOR),
-- and `false` after it (LIVE under Wine).
local raised = call('server', "error('probe raise')")
E.observe('raise_second_value', raised.values[2], { wine = 'false' })
E.observe('raise_does_not_raise', raised.raised == false, { windows = true })
E.observe('raise_returns_text', raised.types[1] == 'string' and raised.values[1]:find('probe raise', 1, true) ~= nil,
  { windows = true })

-- The chunk gets no arguments (ED-CODE: the signature is state, string).
E.observe('chunk_argument_count', call('server', "return tostring(select('#', ...))").values[1], { windows = '0' })

-- Neither gate key is set in the config state, and the calls above worked
-- (LIVE on an unconfigured install).
local ok, gates = E.cross('config', [[
  return type(rawget(net, 'allow_unsafe_api')) .. ',' .. type(rawget(net, 'allow_dostring_in'))
]])
E.observe('gate_keys', ok and gates or 'unreadable', { windows = 'nil,nil' })
