-- E34: does a ban survive a DCS restart? Run on the server in the hook state,
-- twice, with a restart between:
--   1. `<step>` = `ban`: bans the connected test client (connection 2, the
--      only player) for 600 s with the reason `np-s16-restart`, which
--      disconnects it, and reports that BANS lists that reason.
--   2. `cargo xtask restart <server>` (no player is connected after the ban).
--   3. `<step>` = `check`: reports whether the ban list still holds an entry
--      with that reason, then removes every such entry by its ucid.
-- The ucid is never returned, logged or written: the snippet compares and
-- removes inside Lua and reports counts only.
-- states: hook
-- timeout: 30

local E = ...
local STEP = '<step>'
local REASON = 'np-s16-restart'

local function ours()
  local found = {}
  for _, b in ipairs(net.banlist_get() or {}) do
    if type(b) == 'table' and b.reason == REASON and type(b.ucid) == 'string' then
      found[#found + 1] = b.ucid
    end
  end
  return found
end

if STEP == 'ban' then
  local ids = net.get_player_list() or {}
  local client
  for _, id in ipairs(ids) do
    if id ~= 1 then
      if client then
        error('more than one player is connected; this bans only a lone test client')
      end
      client = id
    end
  end
  if not client then
    error('no client is connected')
  end
  E.observe('banlist_add', tostring((net.banlist_add(client, 600, REASON))), { info = true })
  E.observe('listed_after_ban', #ours(), { info = true })
  return 'banned; restart DCS, then run the check step'
elseif STEP == 'check' then
  local found = ours()
  E.observe('listed_after_restart', #found, { info = true })
  local removed = 0
  for _, ucid in ipairs(found) do
    if net.banlist_remove(ucid) ~= false then
      removed = removed + 1
    end
  end
  E.observe('removed', removed, { info = true })
  E.observe('listed_after_remove', #ours(), { info = true })
  return 'done'
end
error('fill <step> with ban or check')
