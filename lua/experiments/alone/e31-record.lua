-- E31, S9's part: record the world events a scripted client causes in the
-- server state, from joining through taking a slot, spawning, leaving the
-- slot and leaving the server. Run with `cargo xtask eval <server> server`
-- before the client joins. It returns 20 s of model time after the first
-- S_EVENT_PLAYER_LEAVE_UNIT, or after 290 s of model time.
-- states: server
-- timeout: 330

local E = ...
local R = E.lib('recorder')
R.clear()
R.start()
R.mark('client')

local start, left = timer.getTime(), nil
return E.wait(function()
  if not left and R.count('client', 'S_EVENT_PLAYER_LEAVE_UNIT') > 0 then
    left = timer.getTime()
  end
  if (left and timer.getTime() - left >= 20) or timer.getTime() - start >= 290 then
    E.observe('catalog', R.report(), { info = true })
    return 'done'
  end
  return nil
end, 320)
