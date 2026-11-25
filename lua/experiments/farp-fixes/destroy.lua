-- FARP fixes, part 2 of 3: destroy what `spawn.lua` added. The FARP and the
-- helipad with radio keys by `StaticObject:destroy()`, as STATIC_DESTROY does;
-- the bare helipad by `Object.destroy` on its airbase. Read in this call and
-- again in `after.lua`.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local G = _G.np_farp_fixes
if not G then
  error('spawn.lua has not run')
end
local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then
    return v
  end
  return 'raised: ' .. tostring(v)
end
-- A call's outcome: `returned` and what it returned, or what it raised.
local function call(f, ...)
  local ok, v = pcall(f, ...)
  return ok and ('returned ' .. tostring(v)) or ('raised: ' .. tostring(v))
end

local events = {}
local handler = {}
function handler:onEvent(e)
  local ok, id = pcall(function()
    return rawget(e.initiator, 'id_')
  end)
  events[#events + 1] = { event = e.id, id = ok and id or nil }
end
world.addEventHandler(handler)

local out = { calls = {} }
for _, name in ipairs({ 'np-ff-farp', 'np-ff-pad' }) do
  local st = StaticObject.getByName(name)
  out.calls[name] = st and call(st.destroy, st) or 'no static'
end
local ab = Airbase.getByName('np-ff-bare')
out.calls['np-ff-bare'] = ab and call(Object.destroy, ab) or 'no airbase'

world.removeEventHandler(handler)
out.events = events
out.now = {}
for _, name in ipairs(G.names) do
  local st, ab0 = G.statics[name], G.bases[name]
  out.now[name] = {
    static_by_name = StaticObject.getByName(name) ~= nil,
    airbase_by_name = Airbase.getByName(name) ~= nil,
    old_static_exists = st and try(st.isExist, st),
    old_airbase_exists = ab0 and try(ab0.isExist, ab0),
  }
end
return out
