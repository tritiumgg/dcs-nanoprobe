-- Radio transmission, one case: stop every earlier case, then start one
-- looped transmission above the first blue player's aircraft, so the person
-- in it can say whether it is heard.
-- Returns {ok, error, at, unit, args}: `ok` and `error` are pcall's, `at` the
-- point used, `unit` the aircraft's type, `args` what was passed.
-- State: server (the mission scripting state).
-- Evidence: trigger.action.radioTransmission(file, point, modulation, loop,
-- frequency, power, name) is the argument order the platform assumes (PRIOR);
-- its units and file form are what the cases settle. coalition.getPlayers and
-- Unit:getPoint are PRIOR. Run 5 of docs/results/2026-11-26-mission-content.md
-- settles the order, Hz, AM 0, FM 1 and the path form (LIVE).
-- Placeholders, each replaced before the eval (one unknown per request):
--   <file>   the file argument, as a Lua string body
--   <freq>   the frequency argument, a number
--   <mod>    the modulation argument, 0 or 1
--   <power>  the power argument, a number
--   <above>  meters above the aircraft
--   <name>   this case's name, `np-rt-c<N>`
-- Run: sed -e 's|<file>|l10n/DEFAULT/np-new.wav|' -e 's/<freq>/251000000/' ...
--   case.lua > /path/to/c1.lua, then cargo xtask eval <target> server c1.lua
-- stop.lua stops every case.
-- states: server
-- timeout: 30
local E = ...
for i = 1, 16 do
  pcall(trigger.action.stopRadioTransmission, 'np-rt-c' .. i)
end
local u = (coalition.getPlayers(2) or {})[1]
if not u then
  return { ok = false, error = 'no blue player' }
end
local p = u:getPoint()
local at = { x = p.x, y = p.y + tonumber('<above>'), z = p.z }
local args = {
  file = '<file>',
  modulation = tonumber('<mod>'),
  frequency = tonumber('<freq>'),
  power = tonumber('<power>'),
  name = '<name>',
}
local ok, err = pcall(
  trigger.action.radioTransmission,
  args.file,
  at,
  args.modulation,
  true,
  args.frequency,
  args.power,
  args.name
)
return { ok = ok, error = err and tostring(err), at = at, unit = u:getTypeName(), args = args }
