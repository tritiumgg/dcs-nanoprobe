-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): net.screenshot_request for each connected client, as ED's web GUI calls it.
-- states: hook
local out = {}
for _, id in ipairs(net.get_player_list() or {}) do
  if id ~= net.get_my_player_id() then
    local ok, r = pcall(net.screenshot_request, id)
    out[#out + 1] = { id = id, ok = ok, result = tostring(r) }
  end
end
return out
