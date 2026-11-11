-- E21: what happens when names collide: unit, group and static names reused
-- across kinds. Where names are visible to players needs a client and is
-- not covered here. The rule: the naming rule is fixed in S5 from this.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')
local o = S.origin()
local row = 0

local function ground(group, unit_names)
  row = row + 1
  local units = {}
  for i, name in ipairs(unit_names) do
    units[i] = { name = name, x = o.x + 5000 + i * 20, z = o.z + row * 60 }
  end
  return pcall(S.ground, group, 'M-113', units)
end

local function exists()
  local out = {}
  for _, name in ipairs({ 'np-e21-g1', 'np-e21-g2', 'np-e21-g3', 'np-e21-g4', 'np-e21-u1', 'np-e21-s1' }) do
    local group = Group.getByName(name)
    local unit = Unit.getByName(name)
    local static = StaticObject.getByName(name)
    local parts = {}
    if group then
      parts[#parts + 1] = 'group(' .. group:getSize() .. ')'
    end
    if unit then
      parts[#parts + 1] = 'unit(of ' .. unit:getGroup():getName() .. ')'
    end
    if static then
      parts[#parts + 1] = 'static'
    end
    if #parts > 0 then
      out[#out + 1] = name .. '=' .. table.concat(parts, '+')
    end
  end
  return table.concat(out, ' ')
end

ground('np-e21-g1', { 'np-e21-u1', 'np-e21-u2' })
E.observe('0.start', exists(), { info = true })
-- A second group with a unit named like one in the first group.
local ok, err = ground('np-e21-g2', { 'np-e21-u1', 'np-e21-u3' })
E.observe('1.unit_name_in_two_groups', tostring(ok) .. ' ' .. tostring(not ok and err or '') .. ' -> ' .. exists(),
  { info = true })
-- A static named like a group.
ok, err = pcall(S.spawn_static, 'np-e21-g1', 5000, row * 60 + 200)
E.observe('2.static_named_like_group', tostring(ok) .. ' -> ' .. exists(), { info = true })
-- A static, then a group with its name.
pcall(S.spawn_static, 'np-e21-s1', 5000, row * 60 + 300)
ok, err = ground('np-e21-s1', { 'np-e21-u4' })
E.observe('3.group_named_like_static', tostring(ok) .. ' -> ' .. exists(), { info = true })
-- A group whose unit has the group's own name.
ok, err = ground('np-e21-g3', { 'np-e21-g3' })
E.observe('4.unit_named_like_its_group', tostring(ok) .. ' -> ' .. exists(), { info = true })
-- A unit named like a static.
ok, err = ground('np-e21-g4', { 'np-e21-s1' })
E.observe('5.unit_named_like_static', tostring(ok) .. ' -> ' .. exists(), { info = true })
-- Names at the limits: long, and with characters outside ASCII letters.
ok, err = ground('np-e21-' .. string.rep('x', 200), { 'np-e21-long-1' })
E.observe('6.long_group_name', tostring(ok) .. ' ' .. tostring(Group.getByName('np-e21-' .. string.rep('x', 200)) ~= nil),
  { info = true })
ok, err = ground('np-e21-~!@#$%^&*()', { 'np-e21-sym-1' })
E.observe('7.symbol_group_name', tostring(ok) .. ' ' .. tostring(Group.getByName('np-e21-~!@#$%^&*()') ~= nil),
  { info = true })

local cleared_groups, cleared_statics = S.clear()
E.observe('cleared', { groups = cleared_groups, statics = cleared_statics }, { info = true })
return 'done'
