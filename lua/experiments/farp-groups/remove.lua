-- FARP groups, part 3, mission paused: destroy g3, g10, h3, fh, f2, ff, ff2 and fv,
-- as STATIC_DESTROY does: `StaticObject.getByName(first name):destroy()`.
-- f2, ff, ff2 and fv are destroyed under every unit name that still finds a
-- static, and fv's vehicle by `Unit.getByName` if it is one,
-- since what DCS made of them is read in `spawn.lua`. Read at once;
-- `after.lua` reads again once the simulation has run.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local G = _G.np_farp_groups
if not G then
  error('spawn.lua has not run')
end
local function call(f, ...)
  local ok, v = pcall(f, ...)
  return ok and ('returned ' .. tostring(v)) or ('raised: ' .. tostring(v))
end
local out = { calls = {}, now = {} }
for _, tag in ipairs({ 'g3', 'g10', 'h3', 'fh', 'f2', 'ff', 'ff2', 'fv' }) do
  local n = #G.groups[tag].units
  local every = tag == 'f2' or tag == 'ff' or tag == 'ff2' or tag == 'fv'
  for k = 1, every and n or 1 do
    local name = 'np-fg-' .. tag .. '-' .. k
    local st = StaticObject.getByName(name)
    out.calls[name] = st and call(st.destroy, st) or 'no static'
    local un = Unit.getByName(name)
    if un then
      out.calls[name .. ' unit'] = call(un.destroy, un)
    end
  end
  out.now[tag] = G.read(tag, n)
end
return out
