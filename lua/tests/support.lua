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

-- A new empty directory, with a trailing slash.
function S.scratch()
  local path = os.tmpname()
  os.remove(path)
  assert(os.execute('mkdir -p ' .. quote(path)) == 0, 'no scratch directory')
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

function S.ok(message)
  print('ok  ' .. message)
end

return S
