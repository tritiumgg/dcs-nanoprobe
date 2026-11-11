-- Lists what a Lua state holds: every global, and the members of every table
-- down to three levels, with each one's type. `cargo xtask api-dump` runs it
-- in each state and writes lua/mock/api/<state>.txt, which the strict mock
-- reads as its listing.
--
-- A line is `path : type`, with ` = value` after a number or a boolean. No
-- string's value is written, because a string can hold a path or a name. A
-- key that is not a plain identifier is counted and not named, for the same
-- reason. A table reached a second time is written as `table -> first path`.
-- A table's members are listed when it holds a function or has at most
-- SMALL names; any other table is data, such as the unit database, and is
-- written with its size alone. Tables are walked breadth first, so when the
-- line limit cuts the listing, it cuts the deepest names.

local E = ...
-- Every global is listed; a table below the globals lists its first MAX_KEYS
-- names.
local MAX_LINES, MAX_KEYS, MAX_DEPTH = 40000, 600, 3
local SMALL = 100

-- How many names a table has, and whether any of them is a function.
local function shape(t)
  local names, callable = 0, false
  for key, value in next, t do
    if type(key) == 'string' then
      names = names + 1
      callable = callable or type(value) == 'function'
    end
  end
  return names, callable
end

local lines, truncated = {}, false
local first_path = { [_G] = '_G' }
local queue = { { table = _G, path = '', depth = 1 } }
local head = 1

local function add(line)
  if #lines >= MAX_LINES then
    truncated = true
    return false
  end
  lines[#lines + 1] = line
  return true
end

while queue[head] and not truncated do
  local item = queue[head]
  head = head + 1
  local names, other = {}, 0
  for key in next, item.table do
    if type(key) == 'string' and key:match('^[%a_][%w_]*$') then
      names[#names + 1] = key
    else
      other = other + 1
    end
  end
  table.sort(names)
  if other > 0 then
    add((item.path == '' and '_G' or item.path) .. ' : ' .. other .. ' other keys')
  end
  for i, key in ipairs(names) do
    if i > MAX_KEYS and item.depth > 1 then
      add(item.path .. '<more> : ' .. (#names - MAX_KEYS) .. ' names not listed')
      break
    end
    local value = rawget(item.table, key)
    local kind = type(value)
    local path = item.path .. key
    local line = path .. ' : ' .. kind
    if kind == 'number' or kind == 'boolean' then
      line = line .. ' = ' .. tostring(value)
    elseif kind == 'table' then
      if first_path[value] then
        line = line .. ' -> ' .. first_path[value]
      else
        first_path[value] = path
        local names, callable = shape(value)
        if not callable and names > SMALL then
          line = line .. ', ' .. names .. ' names not listed'
        elseif item.depth < MAX_DEPTH and key ~= '__nanoprobe_eval' then
          queue[#queue + 1] = { table = value, path = path .. '.', depth = item.depth + 1 }
        end
      end
    end
    if key ~= '__nanoprobe_eval' and not add(line) then
      break
    end
  end
end

return { text = table.concat(lines, '\n'), count = #lines, truncated = truncated }
