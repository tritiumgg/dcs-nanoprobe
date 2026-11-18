-- What DCS holds of each static named: whether `StaticObject.getByName` finds
-- it, its life, and whether it exists. Run by `cargo xtask persistence`
-- (scenario `ruins`) in the server state through the eval channel; the
-- scenario sets NAMES. `lua/tests/ruins_live.lua` rehearses it on the mock.
-- states: server

local NAMES = {}

local out = {}
for _, name in ipairs(NAMES) do
  local got, static = pcall(StaticObject.getByName, name)
  local row = { found = got and static ~= nil }
  if row.found then
    local read, life = pcall(static.getLife, static)
    if read then
      row.life = life
    else
      row.life_raised = true
    end
    local known, exists = pcall(static.isExist, static)
    if known then
      row.exists = exists
    else
      row.exists_raised = true
    end
  end
  out[name] = row
end
return out
