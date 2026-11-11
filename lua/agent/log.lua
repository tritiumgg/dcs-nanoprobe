-- JSON-lines log for the hook and the shim. The record is specified in
-- spec/log.md.
--
-- Lua 5.1, no DCS dependency: the caller passes clocks in. Writing never
-- raises. A failure to write disables the file and reports itself once
-- through the caller's fallback.

local M = {}

M.DEFAULT_CAP = 4 * 1024 * 1024

local escapes = {
  ['"'] = '\\"',
  ['\\'] = '\\\\',
  ['\b'] = '\\b',
  ['\f'] = '\\f',
  ['\n'] = '\\n',
  ['\r'] = '\\r',
  ['\t'] = '\\t',
}

local function quote(s)
  s = s:gsub('[%c"\\]', function(c)
    return escapes[c] or string.format('\\u%04x', c:byte())
  end)
  return '"' .. s .. '"'
end

-- A finite number, an integer without a fraction. Anything else is null.
local function number(n)
  if n ~= n or n == math.huge or n == -math.huge then
    return 'null'
  end
  if n == math.floor(n) and math.abs(n) < 2 ^ 53 then
    return string.format('%d', n)
  end
  return string.format('%.17g', n)
end

local function scalar(v)
  local t = type(v)
  if t == 'string' then
    return quote(v)
  elseif t == 'number' then
    return number(v)
  elseif t == 'boolean' then
    return tostring(v)
  elseif v == nil then
    return 'null'
  end
  return quote(tostring(v))
end
M.scalar = scalar

local LEVELS = { error = true, warn = true, info = true, debug = true }

-- One record as a line, ending in a newline. `r` has the keys of spec/log.md.
-- `data` keys are written in sorted order, so a line is the same every run.
function M.encode(r)
  local parts = {
    '"wall":' .. quote(r.wall),
    '"level":' .. quote(LEVELS[r.level] and r.level or 'info'),
    '"target":' .. quote(r.target),
    '"msg":' .. quote(tostring(r.msg)),
  }
  for _, key in ipairs({ 'real', 'model', 'gen', 'thread' }) do
    if r[key] ~= nil then
      parts[#parts + 1] = '"' .. key .. '":' .. scalar(r[key])
    end
  end
  if r.build ~= nil then
    parts[#parts + 1] = '"build":' .. quote(r.build)
  end
  if r.data ~= nil then
    local fields = {}
    for k, v in pairs(r.data) do
      fields[#fields + 1] = quote(tostring(k)) .. ':' .. scalar(v)
    end
    table.sort(fields)
    parts[#parts + 1] = '"data":{' .. table.concat(fields, ',') .. '}'
  end
  return '{' .. table.concat(parts, ',') .. '}\n'
end

-- Open a log. `o` holds:
--   path      the file to append to
--   target    the writer's name, for every record
--   cap       bytes before rotation (default DEFAULT_CAP)
--   real      function returning DCS real time, or nil
--   model     function returning model time, or nil
--   gen       function returning the mission generation, or nil
--   build     the build hash, or nil
--   fallback  function(message) called once if the file cannot be written
function M.open(o)
  local self = {
    path = o.path,
    target = o.target,
    cap = o.cap or M.DEFAULT_CAP,
    clocks = { real = o.real, model = o.model, gen = o.gen },
    build = o.build,
    fallback = o.fallback,
    failed = false,
  }

  local function fail(message)
    if self.file then
      pcall(self.file.close, self.file)
      self.file = nil
    end
    if not self.failed then
      self.failed = true
      if self.fallback then
        pcall(self.fallback, 'log ' .. tostring(self.path) .. ': ' .. tostring(message))
      end
    end
  end

  local function reopen()
    local file, err = io.open(self.path, 'ab')
    if not file then
      fail(err)
      return false
    end
    self.file = file
    self.size = file:seek('end') or 0
    return true
  end

  local function rotate()
    self.file:close()
    self.file = nil
    local old = self.path .. '.1'
    os.remove(old)
    local ok, err = os.rename(self.path, old)
    if not ok then
      fail(err)
      return false
    end
    return reopen()
  end

  local function clock(f)
    if f then
      local ok, v = pcall(f)
      if ok then
        return v
      end
    end
    return nil
  end

  -- Write one record. Never raises.
  function self.write(level, msg, data)
    if self.failed or not (self.file or reopen()) then
      return
    end
    local line = M.encode({
      wall = os.date('!%Y-%m-%dT%H:%M:%SZ'),
      level = level,
      target = self.target,
      msg = msg,
      real = clock(self.clocks.real),
      model = clock(self.clocks.model),
      gen = clock(self.clocks.gen),
      build = self.build,
      data = data,
    })
    if self.size > 0 and self.size + #line > self.cap and not rotate() then
      return
    end
    local ok, err = self.file:write(line)
    if not ok then
      fail(err)
      return
    end
    self.file:flush()
    self.size = self.size + #line
  end

  function self.close()
    if self.file then
      pcall(self.file.close, self.file)
      self.file = nil
    end
  end

  return self
end

return M
