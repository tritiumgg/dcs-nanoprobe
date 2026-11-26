-- Drawing layers, client check: take a screenshot of the client's screen, as
-- e21c did. Lists the screenshots folder first, so a later shots.lua shows which
-- file is new. Returns {before, called, ok, error, listed}: `before` is os.time()
-- just before the call, `called` the calls made, `listed` the file names in the
-- folder before the call (nil when it cannot be read).
-- State: hook (a client's GameGUI hook state).
-- Evidence: DCS.makeScreenShot's arguments are UNVERIFIED; it is called with
-- none, as e21c did (it returned and a file appeared, LIVE). DCS.setScreenShotExt
-- is given 'png' with no dot, because '.png' made a double dot in the file name
-- (LIVE, e21c). The screenshots folder, lfs.writedir() .. 'Screenshots', is
-- UNVERIFIED.
-- Run: cargo xtask eval <target> hook lua/experiments/drawing-layers/shot.lua
-- Then list again with shots.lua, giving `before` as its <since>.
-- states: hook
-- timeout: 30
local E = ...
local out = { called = {} }
local listed = {}
local dir = lfs.writedir() .. 'Screenshots'
local ok, iter, state, first = pcall(lfs.dir, dir)
if ok then
  for name in iter, state, first do
    if name ~= '.' and name ~= '..' then
      listed[#listed + 1] = name
    end
  end
  table.sort(listed)
  out.listed = listed
end
out.before = os.time()
local s, why = pcall(DCS.setScreenShotExt, 'png')
out.called[#out.called + 1] = "DCS.setScreenShotExt('png')"
out.ext_ok, out.ext_error = s, (not s) and tostring(why) or nil
local m, err = pcall(DCS.makeScreenShot)
out.called[#out.called + 1] = 'DCS.makeScreenShot()'
out.ok, out.error = m, (not m) and tostring(err) or nil
return out
