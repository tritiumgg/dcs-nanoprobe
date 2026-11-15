-- Sound and picture actions. Rule: record, for each call, accepted or error.
-- Client playback and display are UNVERIFIED (no client connected).
--
-- Run it on a mission packed with `cargo xtask miz <template> <out>
-- --sound-picture`: `np-tone.wav` under key ResKey_Action_901, and
-- `np-picture.png` under key ResKey_Action_902 (mapResource format, ED-CODE:
-- Scripts/dictionary.lua setValueToResource).
--
-- Sound: the server state's trigger.action.outSound, outSoundForCoalition,
-- outSoundForCountry, outSoundForGroup and outSoundForUnit, with the file by
-- its in-mission name, with `l10n/DEFAULT/` in front, a name that is not in
-- the mission, and a file under the Saved Games profile (written here, from
-- the hook state, and removed at the end). Each call is run under pcall and
-- reports what it raised and returned.
--
-- Picture: the mission state's a_out_picture, a_out_picture_s, _g, _c, _u and
-- a_out_picture_stop through net.dostring_in('mission', ...). The argument
-- order is the field order of the Mission Editor's rule (ED-CODE:
-- MissionEditor/modules/me_trigrules.lua:963-1042 out_picture_fields, with the
-- target field first for _s _g _u _c at 1576-1613; actionToString in
-- me_predicates.lua:2587-2640 writes the arguments in that order, the file as
-- getValueResourceByKey(key)): file, seconds, clearview, start_delay,
-- horzAlignment, vertAlignment, size, size_units, picture_effect.
-- states: hook
-- timeout: 90

local E = ...

local function cross(source)
  local ok, value = E.cross('server', source)
  if not ok then
    return 'CROSS FAILED ' .. tostring(value)
  end
  return value
end

-- A file under the profile, for the sound path case.
local dir = lfs.writedir() .. 'nanoprobe/dev/'
local rel = 'nanoprobe/dev/np-ext.wav'
local abs = dir .. 'np-ext.wav'
local written = false
do
  local f = io.open(abs, 'wb')
  if f then
    f:write('RIFF$\0\0\0WAVEfmt \16\0\0\0\1\0\1\0\34\86\0\0\68\172\0\0\2\0\16\0data\0\0\0\0')
    f:close()
    written = true
  end
end
E.observe('profile_file_written', { written = written }, { info = true })

local SOUND = [[
local out = {}
local function try(label, fn)
  local ok, err = pcall(fn)
  out[#out + 1] = label .. '=' .. (ok and 'ok' or ('ERR ' .. tostring(err)))
end
local S = E.lib('scenario')
local o = S.origin()
S.ground('np-snd-g', 'M-113', { { name = 'np-snd-u', x = o.x + 6000, z = o.z + 6000 } })
local g = Group.getByName('np-snd-g')
local u = Unit.getByName('np-snd-u')
local gid, uid = g:getID(), u:getID()
local names = {
  mission = 'np-tone.wav',
  prefixed = 'l10n/DEFAULT/np-tone.wav',
  missing = 'np-not-there.wav',
  relative = ']] .. rel .. [[',
  absolute = ']] .. abs:gsub('\\', '/') .. [[',
}
local order = { 'mission', 'prefixed', 'missing', 'relative', 'absolute' }
for _, k in ipairs(order) do
  local f = names[k]
  try('outSound/' .. k, function() trigger.action.outSound(f) end)
  try('outSoundForCoalition/' .. k, function() trigger.action.outSoundForCoalition(coalition.side.BLUE, f) end)
  try('outSoundForCountry/' .. k, function() trigger.action.outSoundForCountry(country.id.USA, f) end)
  try('outSoundForGroup/' .. k, function() trigger.action.outSoundForGroup(gid, f) end)
  try('outSoundForUnit/' .. k, function() trigger.action.outSoundForUnit(uid, f) end)
end
try('outSoundStop', function() trigger.action.outSoundStop() end)
-- a call with the wrong argument types
try('outSound/number', function() trigger.action.outSound(5) end)
try('outSoundForGroup/unknown group', function() trigger.action.outSoundForGroup(999999, 'np-tone.wav') end)
S.clear()
return gid .. ' ' .. uid .. ' | ' .. table.concat(out, '; ')
]]
E.observe('sound', cross(SOUND), { info = true })

-- Picture, from the hook state into the mission state.
local function mission(code)
  local ok, a, b = pcall(net.dostring_in, 'mission', code)
  return { pcall_ok = ok, result = tostring(a), flag = b }
end

E.observe('picture_functions_exist', mission(
  "return tostring(a_out_picture ~= nil) .. ' ' .. tostring(a_out_picture_s ~= nil) .. ' ' .. tostring(a_out_picture_g ~= nil) .. ' ' .. tostring(a_out_picture_c ~= nil) .. ' ' .. tostring(a_out_picture_u ~= nil) .. ' ' .. tostring(a_out_picture_stop ~= nil) .. ' ' .. tostring(getValueResourceByKey ~= nil)"
), { info = true })
E.observe('resource_by_key', mission(
  "return tostring(getValueResourceByKey and getValueResourceByKey('ResKey_Action_902')) .. ' | ' .. tostring(getValueResourceByKey and getValueResourceByKey('ResKey_Action_901')) .. ' | ' .. tostring(getValueResourceByKey and getValueResourceByKey('ResKey_missing'))"
), { info = true })

-- A group and unit to aim at.
local ids = cross([[
local S = E.lib('scenario')
local o = S.origin()
S.ground('np-pic-g', 'M-113', { { name = 'np-pic-u', x = o.x + 6000, z = o.z + 6300 } })
return Group.getByName('np-pic-g'):getID() .. ' ' .. Unit.getByName('np-pic-u'):getID()
]])
local gid, uid = tostring(ids):match('(%d+) (%d+)')
E.observe('picture_targets', ids, { info = true })

local TAIL = ', 5, false, 0, "1", "1", 50, "0", 0'
local calls = {
  { 'all/key', 'a_out_picture(getValueResourceByKey("ResKey_Action_902")' .. TAIL .. ')' },
  { 'all/name', 'a_out_picture("np-picture.png"' .. TAIL .. ')' },
  { 'all/missing_key', 'a_out_picture(getValueResourceByKey("ResKey_missing")' .. TAIL .. ')' },
  { 'all/missing_name', 'a_out_picture("np-not-there.png"' .. TAIL .. ')' },
  { 'coalition/red', 'a_out_picture_s("red", getValueResourceByKey("ResKey_Action_902")' .. TAIL .. ')' },
  { 'coalition/blue', 'a_out_picture_s("blue", getValueResourceByKey("ResKey_Action_902")' .. TAIL .. ')' },
  { 'coalition/1', 'a_out_picture_s(1, getValueResourceByKey("ResKey_Action_902")' .. TAIL .. ')' },
  { 'group/id', 'a_out_picture_g(' .. tostring(gid) .. ', getValueResourceByKey("ResKey_Action_902")' .. TAIL .. ')' },
  { 'country/id', 'a_out_picture_c(2, getValueResourceByKey("ResKey_Action_902")' .. TAIL .. ')' },
  { 'unit/id', 'a_out_picture_u(' .. tostring(uid) .. ', getValueResourceByKey("ResKey_Action_902")' .. TAIL .. ')' },
  { 'swapped_args', 'a_out_picture_s(getValueResourceByKey("ResKey_Action_902"), "blue"' .. TAIL .. ')' },
  { 'stop', 'a_out_picture_stop()' },
}
local rows = {}
for _, c in ipairs(calls) do
  local m = mission('local ok, err = pcall(function() ' .. c[2] .. ' end); return tostring(ok) .. " " .. tostring(err)')
  rows[#rows + 1] = c[1] .. ' -> ' .. tostring(m.result) .. (m.pcall_ok and '' or ' (dostring_in raised)') .. ' flag=' .. tostring(m.flag)
end
E.observe('picture_calls', rows, { info = true })

cross([[ E.lib('scenario').clear() ]])
if written then
  os.remove(abs)
end
E.observe('profile_file_removed', { still_there = io.open(abs, 'rb') ~= nil }, { info = true })
return 'done'
