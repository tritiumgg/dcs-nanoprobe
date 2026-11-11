-- Development only; never part of a release. `cargo xtask deploy --dev`
-- installs this as <profile>/Scripts/Hooks/nanoprobe-dev.lua, and a plain
-- deploy or an undeploy removes it.
--
-- It loads a mission on request, so a test can load a generated shell with no
-- person at the server. Once a second of real time, while a mission runs, it
-- looks for <profile>/nanoprobe/dev/load.txt. That file names one .miz in
-- <profile>/nanoprobe/dev/missions/. The request is renamed to load.taken
-- before the load is attempted, so a mission that crashes DCS is not loaded
-- again at the next start. The result goes to dcs.log under NANOPROBE-DEV.
--
-- It honours the kill switch, and no callback returns a value.

local ok, err = pcall(function()
  local root = lfs.writedir() .. 'nanoprobe/'
  local separator = lfs.writedir():find('\\', 1, true) and '\\' or '/'
  local next_check = 0
  local stopped = false

  local function dcslog(level, message)
    pcall(log.write, 'NANOPROBE-DEV', level, message)
  end

  local function exists(path)
    local f = io.open(path, 'rb')
    if f then
      f:close()
      return true
    end
    return false
  end

  local function check()
    if exists(root .. 'disable') then
      stopped = true
      return
    end
    local request = io.open(root .. 'dev/load.txt', 'rb')
    if not request then
      return
    end
    local name = (request:read('*a') or ''):match('^%s*(.-)%s*$')
    request:close()
    os.remove(root .. 'dev/load.taken')
    local renamed, rename_err = os.rename(root .. 'dev/load.txt', root .. 'dev/load.taken')
    if not renamed then
      dcslog(log.ERROR, 'could not take the request: ' .. tostring(rename_err))
      stopped = true
      return
    end
    if not name:match('^[%w%-_.]+%.miz$') then
      dcslog(log.ERROR, 'refused a request for ' .. string.format('%q', name))
      return
    end
    local path = (root .. 'dev/missions/' .. name):gsub('/', separator)
    dcslog(log.INFO, 'loading ' .. name)
    local result = net.load_mission(path)
    dcslog(log.INFO, 'net.load_mission returned ' .. tostring(result))
  end

  DCS.setUserCallbacks({
    onSimulationFrame = function()
      if stopped then
        return
      end
      local now = DCS.getRealTime()
      if now < next_check then
        return
      end
      next_check = now + 1
      local checked, check_err = pcall(check)
      if not checked then
        dcslog(log.ERROR, 'check raised: ' .. tostring(check_err))
        stopped = true
      end
    end,
  })
  dcslog(log.INFO, 'mission loader ready')
end)

if not ok then
  pcall(log.write, 'NANOPROBE-DEV', log.ERROR, 'loader failed: ' .. tostring(err))
end
