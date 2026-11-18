-- The clear-debris check's read round the column clear, run before it and
-- after it. Returns `inside outside alive static all`: how many of the
-- column's wrecks and of the outside group's wrecks are left (a left wreck's
-- reads answer, a removed one's raise), how many control units exist alive,
-- the managed static as `exists:life:y`, or `gone` when no static has its
-- name, and how many of all the killed units' wrecks are left.
-- The caller replaces the placeholders with quoted names.
-- states: server
-- timeout: 60

local E = ...
local handles = E.store.debris and E.store.debris.by_name or {}
local function left(names)
  local n = 0
  for _, name in ipairs(names) do
    local u = handles[name]
    if u and pcall(Unit.getLife, u) then
      n = n + 1
    end
  end
  return n
end
local alive = 0
for _, name in ipairs({ __CONTROL__ }) do
  local u = Unit.getByName(name)
  if u and u:isExist() and u:getLife() > 0 then
    alive = alive + 1
  end
end
local static = 'gone'
local s = StaticObject.getByName(__STATIC__)
if s then
  local known, exists = pcall(s.isExist, s)
  local got, life = pcall(s.getLife, s)
  local placed, p = pcall(s.getPoint, s)
  static = string.format('%s:%s:%s', known and tostring(exists) or 'raised',
    got and string.format('%.1f', life) or 'raised',
    placed and type(p) == 'table' and string.format('%.1f', p.y) or 'raised')
end
local all = 0
for _, u in ipairs(E.store.debris or {}) do
  if pcall(Unit.getLife, u) then
    all = all + 1
  end
end
return string.format('%d %d %d %s %d', left({ __INSIDE__ }), left({ __OUTSIDE__ }), alive, static, all)
