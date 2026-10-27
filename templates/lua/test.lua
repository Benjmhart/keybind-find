-- The plugin against its sample: run `lua test.lua` (from this folder or any other).
local here = (arg[0]:match("^(.*)/") or ".")
package.path = here .. "/?.lua;" .. package.path
local json = require("json")

local function slurp(p) local f = assert(io.open(p)); local s = f:read("*a"); f:close(); return s end
-- Single-quoted for the shell, so a `$` or a backtick in a path stays as it is.
local function sh(s) return "'" .. s:gsub("'", "'\\''") .. "'" end
local lua = arg[-1] or "lua"

-- Runs the plugin with `input` on stdin; its stdout, stderr and whether it exited 0.
local function run(args, input)
  local tmp = os.tmpname()
  local f = assert(io.open(tmp, "w")); f:write(input or ""); f:close()
  local errf = os.tmpname()
  -- The status is echoed last: Lua 5.1's (and LuaJIT's) `close` doesn't report it.
  local cmd = ("%s %s %s < %s 2> %s; echo \"exit:$?\""):format(sh(lua), sh(here .. "/keybind-find-toy"), args, sh(tmp),
    sh(errf))
  local p = assert(io.popen(cmd))
  local all = p:read("*a")
  p:close()
  local out, status = all:match("^(.-)exit:(%d+)\n$")
  local err = slurp(errf)
  os.remove(tmp); os.remove(errf)
  return out, err, status == "0"
end

-- Equal as JSON values (keys in any order).
local function same(a, b)
  if type(a) ~= type(b) then return false end
  if type(a) ~= "table" then return a == b end
  for k, v in pairs(a) do if not same(v, b[k]) then return false end end
  for k in pairs(b) do if a[k] == nil then return false end end
  return true
end

local request = { version = 1, context = {}, inputs = json.array({ { path = "keys.conf", text = slurp(here .. "/tests/keys.conf") } }) }
local out, err, ok = run("", json.encode(request))
assert(ok, err)
assert(same(json.decode(out), json.decode(slurp(here .. "/tests/expected.json"))), "the sample's sheet differs:\n" .. out)

out = run("--manifest")
assert(json.decode(out).name == "toy", out)

local bad = { version = 1, context = {}, inputs = json.array({ { path = "k", text = "ctrl+s = Save\nctrl+q\n" } }) }
out, err, ok = run("", json.encode(bad))
assert(not ok and out == "" and err == "k: line 2: not `key = description`\n", err)

out, err, ok = run("", '{"version": 2, "inputs": []}')
assert(not ok and err:find("request version 2 is not 1", 1, true), err)
out, err, ok = run("", '{"version": 1, "context": {}}')
-- `[]`, not `{}`: `same` can't tell them apart, keybind-find can.
assert(ok and out:find('"General":%[%]'), out)
local function bad_line(text, want)
  local req = { version = 1, context = {}, inputs = json.array({ { path = "k", text = text } }) }
  local _, e, good = run("", json.encode(req))
  assert(not good and e == want, e)
end
bad_line("a = b ; mode\n", "k: line 1: a scope is `name=value`\n")
bad_line("a = ; mode=insert\n", "k: line 1: no description\n")
bad_line("a = b ; mode=x,y\n", "k: line 1: a scope's name and value are printable ASCII words without , = [ or ]\n")

-- json.lua: escapes, non-ASCII, surrogate pairs and mixed tables round-trip; malformed input is an error.
local text = 'quote " backslash \\ tab \t newline \n é ✓ 😀'
assert(json.decode(json.encode({ text }))[1] == text)
assert(json.decode('"a\127b"') == "a\127b", "DEL is allowed raw")
assert(json.decode('"\\u00e9\\ud83d\\ude00"') == "é😀")
assert(same(json.decode(json.encode({ [1] = "a", name = "b" })), { ["1"] = "a", name = "b" }))
assert(json.encode(json.array()) == "[]" and json.encode({}) == "{}")
for _, bad in ipairs({ '"\\ud83dA"', '"\\u-001"', "[1-2]", '"a\nb"', "{} x" }) do
  assert(not pcall(json.decode, bad), bad)
end
assert(not pcall(json.encode, 0 / 0))

print("ok")
