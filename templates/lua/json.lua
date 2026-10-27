-- A small JSON module for keybind-find plugins: decode and encode, nothing else. Lua 5.1 to 5.4 and LuaJIT.
-- Objects decode to tables; arrays to sequences, with `json.is_array` set in their metatable so they encode back as
-- arrays (an empty table encodes as `{}` unless marked). `null` decodes to `json.null`.
local json = { null = setmetatable({}, { __tostring = function() return "null" end }) }
local array_mt = { __jsontype = "array" }

function json.array(t) return setmetatable(t or {}, array_mt) end

local function utf8char(cp)
  if cp < 0x80 then return string.char(cp) end
  if cp < 0x800 then return string.char(0xC0 + math.floor(cp / 0x40), 0x80 + cp % 0x40) end
  if cp < 0x10000 then
    return string.char(0xE0 + math.floor(cp / 0x1000), 0x80 + math.floor(cp / 0x40) % 0x40, 0x80 + cp % 0x40)
  end
  return string.char(0xF0 + math.floor(cp / 0x40000), 0x80 + math.floor(cp / 0x1000) % 0x40,
    0x80 + math.floor(cp / 0x40) % 0x40, 0x80 + cp % 0x40)
end

local escapes = { ['"'] = '"', ["\\"] = "\\", ["/"] = "/", b = "\b", f = "\f", n = "\n", r = "\r", t = "\t" }

function json.decode(s)
  local pos = 1
  local function fail(what) error(("JSON: %s at byte %d"):format(what, pos), 0) end
  local function ws() pos = s:find("[^ \t\r\n]", pos) or #s + 1 end
  local value
  local function str()
    local out, i = {}, pos + 1
    while true do
      local c = s:sub(i, i)
      if c == "" then fail("unterminated string") end
      if c == '"' then pos = i + 1; return table.concat(out) end
      if c == "\\" then
        local e = s:sub(i + 1, i + 1)
        if e == "u" then
          local function hex(at)
            local h = s:sub(at, at + 3)
            if not h:match("^%x%x%x%x$") then fail("bad \\u escape") end
            return tonumber(h, 16)
          end
          local cp = hex(i + 2)
          i = i + 6
          if cp >= 0xD800 and cp < 0xDC00 then
            -- A high surrogate needs a low one after it.
            local lo = s:sub(i, i + 1) == "\\u" and hex(i + 2)
            if not lo or lo < 0xDC00 or lo > 0xDFFF then fail("unpaired surrogate") end
            cp = 0x10000 + (cp - 0xD800) * 0x400 + (lo - 0xDC00)
            i = i + 6
          elseif cp >= 0xDC00 and cp < 0xE000 then
            fail("unpaired surrogate")
          end
          out[#out + 1] = utf8char(cp)
        else
          out[#out + 1] = escapes[e] or fail("bad escape")
          i = i + 2
        end
      elseif c:byte() < 0x20 then
        fail("control character in a string")
      else
        -- Up to the next quote, backslash or byte below 0x20 (`%c` would also stop at DEL, 0x7f, which JSON allows
        -- raw, and loop there forever).
        local j = s:find('[%z\1-\31"\\]', i) or #s + 1
        out[#out + 1] = s:sub(i, j - 1)
        i = j
      end
    end
  end
  function value()
    ws()
    local c = s:sub(pos, pos)
    if c == "{" then
      local t = {}
      pos = pos + 1; ws()
      if s:sub(pos, pos) == "}" then pos = pos + 1; return t end
      while true do
        ws()
        if s:sub(pos, pos) ~= '"' then fail("expected a key") end
        local k = str(); ws()
        if s:sub(pos, pos) ~= ":" then fail("expected ':'") end
        pos = pos + 1
        t[k] = value(); ws()
        local d = s:sub(pos, pos); pos = pos + 1
        if d == "}" then return t end
        if d ~= "," then fail("expected ',' or '}'") end
      end
    elseif c == "[" then
      local t = json.array()
      pos = pos + 1; ws()
      if s:sub(pos, pos) == "]" then pos = pos + 1; return t end
      while true do
        t[#t + 1] = value(); ws()
        local d = s:sub(pos, pos); pos = pos + 1
        if d == "]" then return t end
        if d ~= "," then fail("expected ',' or ']'") end
      end
    elseif c == '"' then return str()
    elseif s:sub(pos, pos + 3) == "true" then pos = pos + 4; return true
    elseif s:sub(pos, pos + 4) == "false" then pos = pos + 5; return false
    elseif s:sub(pos, pos + 3) == "null" then pos = pos + 4; return json.null
    else
      local num = s:match("^-?%d+%.?%d*[eE]?[-+]?%d*", pos)
      local n = num and tonumber(num)
      if not n then fail("unexpected character") end
      pos = pos + #num
      return n
    end
  end
  local v = value(); ws()
  if pos <= #s then fail("trailing characters") end
  return v
end

local function encode_string(s)
  return '"' .. s:gsub('[%c"\\]', function(c)
    local named = { ['"'] = '\\"', ["\\"] = "\\\\", ["\n"] = "\\n", ["\r"] = "\\r", ["\t"] = "\\t" }
    return named[c] or ("\\u%04x"):format(c:byte())
  end) .. '"'
end

function json.encode(v)
  local t = type(v)
  if v == json.null or v == nil then return "null" end
  if t == "boolean" then return tostring(v) end
  if t == "number" then
    if v ~= v or v == math.huge or v == -math.huge then error("JSON: can't encode " .. tostring(v), 0) end
    return tostring(v)
  end
  if t == "string" then return encode_string(v) end
  if t ~= "table" then error("JSON: can't encode a " .. t, 0) end
  -- An array: marked, or exactly the keys 1..n.
  local count = 0
  for _ in pairs(v) do count = count + 1 end
  local mt = getmetatable(v)
  if (mt and mt.__jsontype == "array") or (count > 0 and count == #v) then
    local out = {}
    for i = 1, #v do out[i] = json.encode(v[i]) end
    return "[" .. table.concat(out, ",") .. "]"
  end
  -- An object: every key as a string, each with its own value (1 and "1" can't both be kept).
  local entries = {}
  for k, val in pairs(v) do entries[#entries + 1] = { tostring(k), val } end
  table.sort(entries, function(a, b) return a[1] < b[1] end)
  local out = {}
  for i, e in ipairs(entries) do out[i] = encode_string(e[1]) .. ":" .. json.encode(e[2]) end
  return "{" .. table.concat(out, ",") .. "}"
end

return json
