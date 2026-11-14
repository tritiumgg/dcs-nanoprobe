-- DCS's frame meter for the benchmarks: settle for SETTLE seconds of real
-- time with the simulation running, then count frames for MEASURE seconds.
-- Returns the frames (the intervals between the frames of the window, so the
-- first frame starts the count and is not in it), the frames in each whole second, the longest frame,
-- the paused frames, and real and model time at the first and last frame.
--
-- The window is tied to the wall clock by dcs.log, because os.time() has
-- whole-second resolution. The
-- first and last frame each write one line,
--   NANOPROBE-METER <tag> start real=<s> model=<s>
--   NANOPROBE-METER <tag> end real=<s> model=<s> frames=<n>
-- and dcs.log stamps each line to the millisecond. A host reads the two
-- stamps (`window_from_log` in crates/xtask/src/meter.rs) and has the
-- window's start and length in wall-clock time to a frame. The frame count is this script's own, so the
-- agent need not be loaded.
-- states: hook
-- timeout: 300

local E = ...
local SETTLE, MEASURE = tonumber('@SETTLE@') or 30, tonumber('@MEASURE@') or 60
local TAG = '@TAG@'
if TAG:find('@', 1, true) then
  TAG = 'meter'
end

local function note(text)
  pcall(log.write, 'NANOPROBE-METER', log.INFO, text)
end

local t0 = DCS.getRealTime()
local first_real, first_model, last_real, last_model
local previous = t0
local frames, paused, longest = 0, 0, 0
local buckets = {}
for i = 1, math.ceil(MEASURE) do
  buckets[i] = 0
end
return E.wait(function()
  local real = DCS.getRealTime()
  local frame = real - previous
  previous = real
  if real - t0 < SETTLE then
    return nil
  end
  if not first_real then
    first_real, first_model = real, DCS.getModelTime()
    note(string.format('%s start real=%.3f model=%.3f', TAG, first_real, first_model))
  else
    longest = math.max(longest, frame)
    frames = frames + 1
    local b = math.floor(real - first_real) + 1
    if buckets[b] then
      buckets[b] = buckets[b] + 1
    end
  end
  last_real, last_model = real, DCS.getModelTime()
  if DCS.getPause() then
    paused = paused + 1
  end
  if real - first_real >= MEASURE then
    note(string.format('%s end real=%.3f model=%.3f frames=%d', TAG, last_real, last_model, frames))
    return {
      tag = TAG,
      frames = frames,
      buckets = buckets,
      longest = longest,
      paused = paused,
      first_real = first_real,
      last_real = last_real,
      first_model = first_model,
      last_model = last_model,
    }
  end
  return nil
end, SETTLE + MEASURE + 120, 'running')
