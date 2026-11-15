-- E36: what several callback sets do with one chat line. The agent's hook has
-- an `onPlayerTrySendChat` that never returns a value, because a returned
-- value decides the matter for every other hook (ED-CODE,
-- API/Sim_ControlAPI.md). Rule: record the order DCS calls two more sets in,
-- what each sees, and whether a set that returns a replacement line stops the
-- later set.
--
-- It installs two sets with DCS.setUserCallbacks (additive, no removal), in
-- the order the `<order>` placeholder names: empty or `replacer-first`
-- installs the replacer, then the observer; `observer-first` the other way
-- round. DCS calls the later set first (LIVE, S16), so `observer-first` puts
-- the replacer ahead of the observer. The run line below leaves `<order>`
-- empty; fill it with `observer-first` for the second order.
--   replacer  returns `replaced:<line>` for a line that starts with `np-s16-r`,
--             and nothing for any other
--   observer  returns nothing and records every call
-- Both record (id, message, all, the arguments' types) and the order of
-- calls, in `log`. Both stop doing anything when the snippet ends or is reset,
-- though DCS keeps the empty callback sets until it restarts.
--
-- Run it on the server, then send the lines from a client with
-- `lua/experiments/alone/s16-chat.lua`, which sends `np-s16-plain` and
-- `np-s16-r one`. The snippet ends after both, or after 180 s. The agent's own
-- view of the same lines is the `PLAYER` `CHAT` records on the link, which the
-- caller compares with this log.
--   cargo xtask eval <target> hook lua/experiments/s16/e36-chat-hooks.lua --timeout 200
-- states: hook
-- timeout: 200

local E = ...
local live = true
local log = {}
local lines = {}

local function note(who, id, message, all, ...)
  if #log < 60 then
    log[#log + 1] = {
      who = who,
      id = id,
      message = tostring(message),
      all = all,
      types = type(id) .. ',' .. type(message) .. ',' .. type(all),
      extra_args = select('#', ...),
    }
  end
end

local replacer = {
  onPlayerTrySendChat = function(id, message, all, ...)
    if not live then
      return
    end
    note('replacer', id, message, all, ...)
    if type(message) == 'string' and message:sub(1, 8) == 'np-s16-r' then
      lines.replaced = (lines.replaced or 0) + 1
      return 'replaced:' .. message
    end
  end,
}
local observer = {
  onPlayerTrySendChat = function(id, message, all, ...)
    if not live then
      return
    end
    note('observer', id, message, all, ...)
    lines.observed = (lines.observed or 0) + 1
  end,
}
-- The install order: `observer-first` installs the observer first, any other
-- value (empty or `replacer-first`) installs the replacer first.
local ORDER = '<order>'
if ORDER == 'observer-first' then
  DCS.setUserCallbacks(observer)
  DCS.setUserCallbacks(replacer)
else
  DCS.setUserCallbacks(replacer)
  DCS.setUserCallbacks(observer)
end
E.track.defer('chat callbacks off', function()
  live = false
end)
E.observe('installed', ORDER == 'observer-first' and 'observer then replacer' or 'replacer then observer',
  { info = true })

local started = DCS.getRealTime()
return E.wait(function()
  local seen_plain, seen_replace = false, false
  for _, entry in ipairs(log) do
    if entry.who == 'observer' and entry.message == 'np-s16-plain' then
      seen_plain = true
    end
    if entry.who == 'replacer' and entry.message:sub(1, 8) == 'np-s16-r' then
      seen_replace = true
    end
  end
  if not (seen_plain and seen_replace) and DCS.getRealTime() - started < 170 then
    return nil
  end
  live = false
  E.observe('log', log, { info = true })
  local replacer_calls = 0
  for _, e in ipairs(log) do
    replacer_calls = replacer_calls + (e.who == 'replacer' and 1 or 0)
  end
  E.observe('replacer_calls', replacer_calls, { info = true })
  E.observe('observer_calls', lines.observed or 0, { info = true })
  -- The observer saw the replaced line after the replacer returned a value
  -- when the chain went on.
  local after_replace = false
  for i, entry in ipairs(log) do
    if entry.who == 'replacer' and entry.message:sub(1, 8) == 'np-s16-r' then
      local nxt = log[i + 1]
      after_replace = nxt ~= nil and nxt.who == 'observer' and nxt.message == entry.message
    end
  end
  E.observe('observer_called_after_a_replacement', after_replace, { info = true })
  return 'done'
end, 190)
