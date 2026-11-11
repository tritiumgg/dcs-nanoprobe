-- Small facts about the hook state and the server state.
-- states: hook server

local E = ...

-- `DCS` and `Sim` are the same table (PRIOR).
E.observe('DCS_is_Sim', rawequal(E.global('DCS'), E.global('Sim')) and E.global('DCS') ~= nil, { windows = true })

if E.state == 'hook' then
  -- The baseline is a client hosting a multiplayer session (LIVE).
  E.observe('isServer', DCS.isServer(), { windows = true })
  E.observe('isMultiplayer', DCS.isMultiplayer(), { windows = true })
  E.observe('mission_name_type', type(DCS.getMissionName()), { windows = 'string' })
  E.observe('mission_file_type', type(DCS.getMissionFilename()), { windows = 'string' })
  E.observe('theatre', DCS.getMissionTheatre(), { info = true })
  -- A hook script leaves the tainted-file list empty (PRIOR).
  local tainted = E.pack(DCS.getTaintedFiles())
  E.observe('tainted_type', type(tainted[1]), { info = true })
  E.observe('tainted_count', type(tainted[1]) == 'table' and #tainted[1] or 0, { windows = 0 })
end
