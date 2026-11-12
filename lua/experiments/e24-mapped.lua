-- E24: a file mapped by the DCS process under Wine and by a native Linux
-- process: the same pages, and stores seen whole. `cargo xtask mapped`
-- starts the peer (tools/mapped-peer.py) in the container and then runs this.
-- Each frame the agent writes a counter into eight words under a sequence
-- lock and reads back the peer's echo of it. The rule: the status block
-- works, or the stream fallback of plan 4.3 is used.
-- states: hook
-- timeout: 60

local E = ...
local A = E.lib('agent')
local agent = A.open()

-- Under Wine, /dev/shm in the container through the Z: drive, which the peer
-- reaches as /dev/shm. On Windows, and in the mock rehearsal, a file in the
-- profile, which the peer's account can reach.
local function wine_shm()
  local ok, attributes = pcall(lfs.attributes, 'Z:\\dev\\shm')
  return ok and type(attributes) == 'table' and attributes.mode == 'directory'
end
local PATH = package.config:sub(1, 1) == '\\' and wine_shm() and 'Z:\\dev\\shm\\nanoprobe-e24.map'
  or lfs.writedir() .. 'nanoprobe-e24.map'
local SECONDS = 15

local opened, err = agent.map_open(PATH)
E.observe('opened', opened == true, { wine = true })
if not opened then
  E.note(tostring(err))
  return 'not opened'
end

local n, lags, echoes, start = 0, {}, 0, DCS.getRealTime()
local last_echo, peer_first, peer_last = 0, nil, nil

return E.wait(function()
  n = n + 1
  local echo, peer = agent.map_frame(n)
  if peer > 0 then
    peer_first = peer_first or peer
    peer_last = peer
    if echo ~= last_echo then
      echoes = echoes + 1
      lags[#lags + 1] = n - echo
      last_echo = echo
    end
  end
  if DCS.getRealTime() - start < SECONDS then
    return nil
  end
  agent.map_close()
  E.observe('frames', n, { info = true })
  E.observe('peer_seen', peer_last ~= nil, { wine = true })
  E.observe('peer_advanced', (peer_last or 0) - (peer_first or 0), { info = true })
  E.observe('echoes_seen', echoes, { info = true })
  E.observe('echo_lag_frames', A.stats(lags), { info = true })
  return 'done'
end, 50)
