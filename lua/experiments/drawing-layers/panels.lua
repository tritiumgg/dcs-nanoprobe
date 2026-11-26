-- Drawing layers, player marks: every field world.getMarkPanels() returns for each
-- panel below 900,000,000 (players' marks), keys and value types, with strings and
-- numbers as they are. Run with `cargo xtask eval <server> server panels.lua`.
-- states: server
-- timeout: 30

local out = {}
for _, panel in ipairs(world.getMarkPanels()) do
  local id = panel.idx or panel.id
  if type(id) == 'number' and id < 900000000 then
    local row = {}
    for k, v in pairs(panel) do
      local t = type(v)
      if t == 'table' then
        local keys = {}
        for kk, vv in pairs(v) do keys[#keys + 1] = tostring(kk) .. '=' .. tostring(vv) end
        table.sort(keys)
        row[tostring(k)] = '{' .. table.concat(keys, ',') .. '}'
      else
        if t == 'string' or t == 'number' or t == 'boolean' then
          row[tostring(k)] = v
        else
          row[tostring(k)] = '<' .. t .. '>'
        end
      end
    end
    out[#out + 1] = row
  end
end
return { panels = out }
