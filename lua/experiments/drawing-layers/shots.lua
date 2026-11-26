-- Drawing layers, client check: list the client's screenshots folder, so the
-- file a screenshot made (shot.lua) can be found by name, size and time.
-- Returns {dir, now, files = {{name, size, modified}, ...}} in name order, or
-- {dir, now, error} when the folder cannot be read. `modified` and `now` are
-- os.time() seconds.
-- State: hook (a client's GameGUI hook state, where lfs and os exist).
-- Evidence: the folder name `Screenshots` under lfs.writedir() is UNVERIFIED;
-- lfs.attributes' `modification` field is UNVERIFIED in this state. e21c
-- (docs/results/2026-11-15-e21c-farp-name.md) saw screenshots written, not
-- where.
-- Run: cargo xtask eval <target> hook lua/experiments/drawing-layers/shots.lua
-- With a time, so only newer files are listed (the number is shot.lua's `before`):
--   sed -e 's/<since>/1764000000/' shots.lua > /path/to/snippet.lua, then eval that file.
-- With <since> left in, every file is listed.
-- states: hook
-- timeout: 30
local E = ...
local since = tonumber('<since>')
local dir = lfs.writedir() .. 'Screenshots'
local out = { dir = dir, now = os.time(), files = {} }
local ok, iter, state, first = pcall(lfs.dir, dir)
if not ok then
  out.error = tostring(iter)
  return out
end
for name in iter, state, first do
  if name ~= '.' and name ~= '..' then
    local path = dir .. '/' .. name
    local size = lfs.attributes(path, 'size')
    local modified = lfs.attributes(path, 'modification')
    if not since or (modified or 0) >= since then
      out.files[#out.files + 1] = { name = name, size = size, modified = modified }
    end
  end
end
table.sort(out.files, function(a, b)
  return a.name < b.name
end)
return out
