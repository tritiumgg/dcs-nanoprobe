-- emit's objects: is the `id_` of a unit the same number as `getID()`?
-- spec/commands.md ("emit") takes a unit object or the number in its `id_`
-- field, and says that `id_` differs from `getID()` is UNVERIFIED. Rule: for 5
-- ground units and 1 static, record `id_`, `getID()` and whether they are
-- equal, and when raw Lua is on, what `emit` returns for the object and for the
-- `id_` number, and for the `getID()` number. Spawns and removes its own
-- objects. Run on a server with no player:
-- `cargo xtask eval <target> server lua/experiments/s16/e-emit-objects.lua`.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')
local o = S.origin()

local units = {}
for i = 1, 5 do
  units[i] = { name = 'np-s16-id-u' .. i, x = o.x + 4000 + i * 40, z = o.z + 4000 }
end
local group = S.ground('np-s16-id-g', 'M-113', units)
local static = S.spawn_static('np-s16-id-s', 4200, 4000)

local function field(obj, key)
  local ok, v = pcall(function()
    return obj[key]
  end)
  return ok and v or nil
end

local function call(obj, name)
  local ok, v = pcall(function()
    return obj[name](obj)
  end)
  return ok and v or ('raised ' .. tostring(v))
end

local has_emit = type(rawget(_G, 'emit')) == 'function' or type(emit) == 'function'
E.observe('emit_is_a_function', has_emit, { info = true })

local function emit_result(arg)
  if not has_emit then
    return 'no emit'
  end
  local ok, a, b = pcall(emit, 'np.s16.ids', nil, { arg })
  return ok and (tostring(a) .. (b ~= nil and (' ' .. tostring(b)) or '')) or ('raised ' .. tostring(a))
end

local rows, equal = {}, 0
local function row(kind, name, obj)
  local id_ = field(obj, 'id_')
  local get_id = call(obj, 'getID')
  local same = id_ ~= nil and id_ == get_id
  if same then
    equal = equal + 1
  end
  rows[#rows + 1] = {
    kind = kind,
    name = name,
    id_ = id_,
    getID = get_id,
    equal = same,
    emit_object = emit_result(obj),
    emit_id_ = emit_result(id_),
    emit_getID = emit_result(get_id),
  }
end

for i = 1, 5 do
  local unit = Unit.getByName('np-s16-id-u' .. i)
  if unit then
    row('unit', 'np-s16-id-u' .. i, unit)
  else
    rows[#rows + 1] = { kind = 'unit', name = 'np-s16-id-u' .. i, found = false }
  end
end
row('static', 'np-s16-id-s', static)

E.observe('rows', rows, { info = true })
E.observe('equal_count', equal, { info = true })
E.observe('rows_count', #rows, { info = true })
E.observe('group_added', group ~= nil, { info = true })
E.note('compare id_ with getID in rows; emit_* is "true" when emit accepted the entry')
S.clear()
E.observe('cleared', S.remaining() == 0, { info = true })
return 'done'
