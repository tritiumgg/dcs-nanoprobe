-- a_do_script in the mission state hands back what a chunk returned,
-- shifted by one.
-- states: mission

local E = ...

local function shown(...)
  local r = E.pack(...)
  local out = { n = r.n, values = {} }
  for i = 1, r.n do
    out.values[i] = tostring(r[i])
  end
  return out
end

-- `return 1, 2, 3` arrives as nil, 1, 2 (LIVE). Only numbers and strings
-- are returned here: a table returned through a_do_script corrupted DCS's
-- allocator (PRIOR).
E.observe('three_values', shown(a_do_script('return 1, 2, 3')), { windows = { n = 3, values = { 'nil', '1', '2' } } })
E.observe('one_value', shown(a_do_script('return "x"')), { info = true })
E.observe('no_value', shown(a_do_script('local x = 1')), { info = true })
