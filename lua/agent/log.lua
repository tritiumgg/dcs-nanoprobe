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
  -- The shortest of 15, 16 or 17 digits that reads back as the same number.
  for digits = 15, 16 do
    local s = string.format('%.' .. digits .. 'g', n)
    if tonumber(s) == n then
      return s
    end
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

-- Call a method on a file handle under pcall. A handle in DCS's gui state
-- lacks `seek` (LIVE, DCS 2.9.31.29434 dedicated server under Wine 10.0), so
-- no method is assumed. Returns ok, then the method's results or the error.
local function call(file, method, ...)
  local args = { n = select('#', ...), ... }
  return pcall(function()
    return file[method](file, unpack(args, 1, args.n))
  end)
end

-- The size of an existing file in bytes, without seek: `size` when the caller
-- gives one (DCS has lfs.attributes), else by reading the file.
local function file_size(path, size)
  if size then
    local ok, n = pcall(size, path)
    if ok and type(n) == 'number' then
      return n
    end
  end
  local file = io.open(path, 'rb')
  if not file then
    return 0
  end
  local ok, data = call(file, 'read', '*a')
  call(file, 'close')
  return ok and type(data) == 'string' and #data or 0
end

-- Open a log. `o` holds:
--   path      the file to append to
--   target    the writer's name, for every record
--   cap       bytes before rotation (default DEFAULT_CAP)
--   real      function returning DCS real time, or nil
--   model     function returning model time, or nil
--   gen       function returning the mission generation, or nil
--   build     the build hash, or nil
--   size      function(path) returning a file's size in bytes, or nil
--   fallback  function(message) called once if the file cannot be written
--   forward   function(level, msg, data) also given every record, or nil; it
--             may be set on the returned log later, as `forward`
function M.open(o)
  local self = {
    path = o.path,
    target = o.target,
    cap = o.cap or M.DEFAULT_CAP,
    clocks = { real = o.real, model = o.model, gen = o.gen },
    build = o.build,
    size_of = o.size,
    fallback = o.fallback,
    forward = o.forward,
    failed = false,
  }

  local function close()
    if self.file then
      call(self.file, 'close')
      self.file = nil
    end
  end

  local function fail(message)
    close()
    if not self.failed then
      self.failed = true
      if self.fallback then
        pcall(self.fallback, 'log ' .. tostring(self.path) .. ': ' .. tostring(message))
      end
    end
  end

  local function reopen()
    self.size = file_size(self.path, self.size_of)
    local file, err = io.open(self.path, 'ab')
    if not file then
      fail(err)
      return false
    end
    self.file = file
    return true
  end

  local function rotate()
    close()
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
    if self.forward then
      pcall(self.forward, level, msg, data)
    end
    if self.failed or not (self.file or reopen()) then
      return
    end
    local encoded, line = pcall(M.encode, {
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
    if not encoded then
      return
    end
    if self.size > 0 and self.size + #line > self.cap and not rotate() then
      return
    end
    -- Stock Lua returns true, or nil and a message. DCS's gui state returns
    -- nil on success (LIVE), so only a raise or a message is a failure.
    local ok, written, err = call(self.file, 'write', line)
    if not ok or (written == nil and err ~= nil) then
      fail(ok and err or written)
      return
    end
    call(self.file, 'flush')
    self.size = self.size + #line
  end

  self.close = close

  return self
end

return M
