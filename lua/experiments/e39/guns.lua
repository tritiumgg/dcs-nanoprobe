-- E39 guns: where the `server` state holds an aircraft type's full gun
-- count, which turns a captured round count into `payload.gun` (percent of
-- full; spawn.lua found the rounds linear in it). Records, for each type,
-- `db.Units` `Guns[i]` with its scalars and the scalars of its nested
-- tables, and the keys of `Unit.getDescByName(type)`. Reads only.
-- states: server
-- timeout: 30

local E = ...

local TYPES = { 'F-15C', 'UH-1H', 'A-10C', 'Su-25T', 'Ka-50', 'MiG-29A' }

local function scalars(t, depth)
  local out = {}
  if type(t) ~= 'table' then
    return type(t)
  end
  for k, v in pairs(t) do
    local kind = type(v)
    if kind == 'number' or kind == 'string' or kind == 'boolean' then
      out[tostring(k)] = v
    elseif kind == 'table' and depth > 0 then
      out[tostring(k)] = scalars(v, depth - 1)
    end
  end
  return out
end

local function find_def(name)
  for _, ct in pairs(db.Units) do
    if type(ct) == 'table' then
      for _, list in pairs(ct) do
        if type(list) == 'table' then
          for _, def in ipairs(list) do
            if type(def) == 'table' and (def.type == name or def.Name == name) then
              return def
            end
          end
        end
      end
    end
  end
end

for _, t in ipairs(TYPES) do
  local def = find_def(t)
  local r = { found = def ~= nil }
  if def then
    r.guns = scalars(def.Guns, 3)
  end
  local ok, desc = pcall(Unit.getDescByName, t)
  if ok and type(desc) == 'table' then
    local keys = {}
    for k in pairs(desc) do
      keys[#keys + 1] = tostring(k)
    end
    table.sort(keys)
    r.desc_keys = keys
  else
    r.desc_error = tostring(desc)
  end
  E.observe('guns_' .. t, r, { info = true })
end
return 'done'
