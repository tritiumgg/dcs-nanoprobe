-- nanoprobe loader. DCS runs every file in <profile>/Scripts/Hooks once per
-- process, in the gui state. This file is the same in every version: it reads
-- the manifest and runs the active version's hook. The layout is in
-- spec/install.md.
--
-- It must not raise into DCS, so the whole body runs under pcall, and a
-- failure is one line in dcs.log.

local function report(message)
  pcall(log.write, 'NANOPROBE', log.ERROR, message)
end

local ok, err = pcall(function()
  local root = lfs.writedir() .. 'nanoprobe/'

  local disable = io.open(root .. 'disable', 'rb')
  if disable then
    disable:close()
    log.write('NANOPROBE', log.INFO, 'disabled by ' .. root .. 'disable; nothing loaded')
    return
  end

  local manifest = io.open(root .. 'manifest.txt', 'rb')
  if not manifest then
    error('no manifest at ' .. root .. 'manifest.txt', 0)
  end
  local text = manifest:read('*a')
  manifest:close()

  local build = text:match('active=([%w]+)')
  if not build then
    error('no active version in ' .. root .. 'manifest.txt', 0)
  end

  local dir = root .. 'versions/' .. build .. '/'
  local body, loaderr = loadfile(dir .. 'lua/hook.lua')
  if not body then
    error(loaderr, 0)
  end
  body(root, build)
end)

if not ok then
  report('loader: ' .. tostring(err))
end
