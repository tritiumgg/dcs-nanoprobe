-- S16: send chat lines from a scripted client with `net.send_chat(text, true)`,
-- for the server's chat callbacks (E36, `lua/experiments/s16/e36-chat-hooks.lua`)
-- and for `PLAYER` `CHAT` records. Run with `cargo xtask eval <client-target>
-- hook` on a client that has joined a server. It sends `np-s16-plain`, then
-- `np-s16-r one` a second later, and listens for 4 s after the last for the
-- lines DCS shows the client (`onChatMessage`, if the callback exists in this
-- state). Fill `<text>` with one line to send only that line.
-- states: hook
-- timeout: 30

local E = ...
local ONE = '<text>'
local LINES = { 'np-s16-plain', 'np-s16-r one' }
if not ONE:find('<', 1, true) then
  LINES = { ONE }
end

local seen = {}
local live = true
DCS.setUserCallbacks({
  onChatMessage = function(message, from)
    if live and #seen < 20 then
      seen[#seen + 1] = { message = tostring(message), from = tostring(from) }
    end
  end,
})
E.track.defer('chat listener off', function()
  live = false
end)

local sent, at, index = {}, DCS.getRealTime(), 0
local done_at
return E.wait(function()
  local now = DCS.getRealTime()
  if now < at then
    return nil
  end
  if index < #LINES then
    index = index + 1
    local ok, err = pcall(net.send_chat, LINES[index], true)
    sent[#sent + 1] = LINES[index] .. ' ' .. (ok and 'sent' or ('raised ' .. tostring(err)))
    at = now + 1
    done_at = now + 4
    return nil
  end
  if now < done_at then
    return nil
  end
  live = false
  E.observe('sent', sent, { info = true })
  E.observe('shown_to_the_client', seen, { info = true })
  return 'done'
end, 25)
