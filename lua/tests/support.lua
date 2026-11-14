-- Helpers for the host tests. Not itself a test: luatest.sh runs only
-- lua/tests/*.lua files whose names do not start with "support".

local S = {}

-- The repository root, with a trailing slash, from the running script's path.
function S.root()
  local script = arg and arg[0] or ''
  local root = script:match('^(.*)lua/tests/[^/]+$')
  assert(root, 'run this from the repository, or through tools/luatest.sh')
  return root == '' and './' or root
end

local function quote(path)
  return "'" .. path:gsub("'", "'\\''") .. "'"
end
S.quote = quote

-- Scratch directories this process made, removed when it closes the Lua
-- state, as at a normal exit or an error that ends the script.
local made = {}
S.cleanup = newproxy(true)
getmetatable(S.cleanup).__gc = function()
  for _, path in ipairs(made) do
    os.execute('rm -rf ' .. quote(path))
  end
end

-- A new empty directory, with a trailing slash. Under tools/luatest.sh it
-- goes in the run's own folder, which the run removes at exit.
function S.scratch()
  local path = os.tmpname()
  os.remove(path)
  local root = os.getenv('NANOPROBE_SCRATCH')
  if root then
    path = root .. '/' .. path:match('[^/]+$')
  end
  assert(os.execute('mkdir -p ' .. quote(path)) == 0, 'no scratch directory')
  made[#made + 1] = path
  return path .. '/'
end

function S.remove(path)
  os.execute('rm -rf ' .. quote(path))
end

function S.mkdir(path)
  assert(os.execute('mkdir -p ' .. quote(path)) == 0, 'mkdir ' .. path)
end

function S.copy(from, to)
  assert(os.execute('cp ' .. quote(from) .. ' ' .. quote(to)) == 0, 'cp ' .. from)
end

function S.write(path, text)
  local f = assert(io.open(path, 'wb'))
  f:write(text)
  f:close()
end

function S.read(path)
  local f = io.open(path, 'rb')
  if not f then
    return nil
  end
  local text = f:read('*a')
  f:close()
  return text
end

function S.lines(path)
  local out = {}
  for line in (S.read(path) or ''):gmatch('[^\n]+') do
    out[#out + 1] = line
  end
  return out
end

-- Assert that fn raises an error containing `needle`.
function S.raises(needle, fn, ...)
  local ok, err = pcall(fn, ...)
  assert(not ok, 'expected an error containing ' .. needle)
  assert(tostring(err):find(needle, 1, true),
    'error ' .. tostring(err) .. ' does not contain ' .. needle)
  return err
end

-- Decode JSON text into Lua values. `null` becomes S.null, so an array keeps
-- its length. Raises on text that is not JSON, which makes it the check that
-- an encoder wrote JSON at all.
S.null = setmetatable({}, { __tostring = function() return 'null' end })

function S.json(text)
  local pos = 1
  local value

  local function fail(what)
    error('json: ' .. what .. ' at byte ' .. pos .. ': ' .. text:sub(pos, pos + 20), 0)
  end

  local function space()
    pos = text:find('[^ \t\r\n]', pos) or #text + 1
  end

  local unescape = { b = '\b', f = '\f', n = '\n', r = '\r', t = '\t', ['"'] = '"', ['\\'] = '\\', ['/'] = '/' }

  local function str()
    local out = {}
    pos = pos + 1
    while true do
      local c = text:sub(pos, pos)
      if c == '' then
        fail('unterminated string')
      elseif c == '"' then
        pos = pos + 1
        return table.concat(out)
      elseif c == '\\' then
        local e = text:sub(pos + 1, pos + 1)
        if e == 'u' then
          local code = tonumber(text:sub(pos + 2, pos + 5), 16) or fail('bad \\u escape')
          out[#out + 1] = code < 256 and string.char(code) or '?'
          pos = pos + 6
        else
          out[#out + 1] = unescape[e] or fail('bad escape')
          pos = pos + 2
        end
      elseif c:byte() < 32 then
        fail('control character in a string')
      else
        out[#out + 1] = c
        pos = pos + 1
      end
    end
  end

  function value()
    space()
    local c = text:sub(pos, pos)
    if c == '{' then
      local object = {}
      pos = pos + 1
      space()
      if text:sub(pos, pos) == '}' then
        pos = pos + 1
        return object
      end
      while true do
        space()
        if text:sub(pos, pos) ~= '"' then
          fail('expected a key')
        end
        local key = str()
        space()
        if text:sub(pos, pos) ~= ':' then
          fail('expected a colon')
        end
        pos = pos + 1
        object[key] = value()
        space()
        local d = text:sub(pos, pos)
        pos = pos + 1
        if d == '}' then
          return object
        elseif d ~= ',' then
          fail('expected a comma or a closing brace')
        end
      end
    elseif c == '[' then
      local array = {}
      pos = pos + 1
      space()
      if text:sub(pos, pos) == ']' then
        pos = pos + 1
        return array
      end
      while true do
        array[#array + 1] = value()
        space()
        local d = text:sub(pos, pos)
        pos = pos + 1
        if d == ']' then
          return array
        elseif d ~= ',' then
          fail('expected a comma or a closing bracket')
        end
      end
    elseif c == '"' then
      return str()
    elseif text:sub(pos, pos + 3) == 'true' then
      pos = pos + 4
      return true
    elseif text:sub(pos, pos + 4) == 'false' then
      pos = pos + 5
      return false
    elseif text:sub(pos, pos + 3) == 'null' then
      pos = pos + 4
      return S.null
    end
    local number = text:match('^-?%d+%.?%d*[eE]?[+-]?%d*', pos)
    if not number or not tonumber(number) then
      fail('unexpected text')
    end
    pos = pos + #number
    return tonumber(number)
  end

  local result = value()
  space()
  if pos <= #text then
    fail('text after the value')
  end
  return result
end

function S.ok(message)
  print('ok  ' .. message)
end

return S
