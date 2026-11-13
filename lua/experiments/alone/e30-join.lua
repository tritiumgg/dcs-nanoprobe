-- E30: join a server from a client's hook state, as ED's server browser does
-- (MissionEditor/modules/mul_server_list.lua: net.start_client). Run by
-- `cargo xtask client join <client-target> <server-target>`, which puts the
-- server's address in ADDRESS and writes the server's join password to
-- SECRET under the profile just before it sends this file. The address is a
-- setup detail and the password a secret, so neither is in the repository.
--
-- The password is read, the file removed, and the password passed to
-- net.start_client inside one block, so nothing holds it afterwards.
-- Nothing here observes, logs or returns it.
-- states: hook
-- timeout: 90

local E = ...
local ADDRESS = '<server>:<port>'
if ADDRESS:find('<', 1, true) then
  error('run this through cargo xtask client join, which fills in ADDRESS')
end

local SECRET = lfs.writedir() .. 'nanoprobe/dev/secret/join'
do
  -- The password lives only in this block.
  local password = ''
  local file = io.open(SECRET, 'rb')
  if file then
    password = file:read('*a') or ''
    file:close()
    os.remove(SECRET)
  end
  E.observe('password_given', file ~= nil and #password > 0, { info = true })
  net.set_name('np-client')
  E.observe('start_client', tostring(net.start_client(ADDRESS, password)), { info = true })
end

local t0 = DCS.getRealTime()
return E.wait(function()
  local blue = #(DCS.getAvailableSlots('blue') or {})
  local red = #(DCS.getAvailableSlots('red') or {})
  if blue + red > 0 or DCS.getRealTime() - t0 > 75 then
    E.observe('joined', {
      id = net.get_my_player_id(),
      multiplayer = DCS.isMultiplayer(),
      blue = blue,
      red = red,
    }, { info = true })
    return 'done'
  end
  return nil
end, 85)
