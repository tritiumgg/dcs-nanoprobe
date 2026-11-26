-- Drawing layers: the sim-thread cost of what the shim calls for a drawing,
-- for the frame budget. Each case is timed with E.bench on a drawing it makes
-- and removes; nothing is left. DCS shows nothing for an add under an ID it
-- held before, so every timed add takes an ID not used before in this run, as
-- the platform does.
-- states: server
-- timeout: 60

local E = ...

-- IDs from 900,000,000, below the platform pool (2^30 to 2^31 - 1).
local B = 900000000 + 3000
local FRESH = 900000000 + 4000
local made = {}
E.track.defer('drawing-layers bench', function()
  for k = 0, 9 do
    pcall(trigger.action.removeMark, B + k)
  end
  for id in pairs(made) do
    pcall(trigger.action.removeMark, id)
  end
end)

local RED, FILL = { 1, 0, 0, 1 }, { 1, 0, 0, 0.3 }
local function p(x, z)
  return { x = x, y = 0, z = z }
end
local function free(n)
  local args = {}
  for i = 1, n do
    local a = 2 * math.pi * i / n
    args[i] = p(50000 + 3000 * math.cos(a), 3000 * math.sin(a))
  end
  for _, v in ipairs({ RED, FILL, 1, true, '' }) do
    args[#args + 1] = v
  end
  return args
end
-- Shape 3 with two points: a line on a client (LIVE, run 3 part B).
local rect = { p(50000, 0), p(51000, 1000), RED, FILL, 1, true, '' }
local f100, f1000 = free(100), free(1000)

-- One remove of the previous drawing and one add under an ID never used before,
-- as the shim does for a set.
local function readd(shape, args)
  local previous
  return function()
    if previous then
      trigger.action.removeMark(previous)
    end
    previous = FRESH
    FRESH = FRESH + 1
    made[previous] = true
    trigger.action.markupToAll(shape, -1, previous, unpack(args))
  end
end

local out = {}
out.readd_rect = E.bench(readd(3, rect), 20, 3)
out.readd_free_100 = E.bench(readd(7, f100), 20, 3)
out.readd_free_1000 = E.bench(readd(7, f1000), 5, 3)
trigger.action.markupToAll(3, -1, B + 3, unpack(rect))
local flip = false
out.set_color = E.bench(function()
  flip = not flip
  trigger.action.setMarkupColor(B + 3, flip and FILL or RED)
end, 50, 3)
out.remove_absent = E.bench(function()
  trigger.action.removeMark(B + 9)
end, 50, 3)
for k = 0, 9 do
  pcall(trigger.action.removeMark, B + k)
end
for id in pairs(made) do
  pcall(trigger.action.removeMark, id)
end
return out
