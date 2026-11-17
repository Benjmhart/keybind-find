-- keybind-find's e2e config: awesome's stock rc.lua, then keys of our own: the remapped key the `awesome` case checks,
-- and one key per kind the awesome provider leaves out, so each of the oracle's KNOWN entries has a key to match.
dofile("/etc/xdg/awesome/rc.lua")
local awful, gears = require("awful"), require("gears")
local function nop() end
local function e2e(mods, key, description)
  return awful.key(mods, key, nop, { description = description, group = "e2e" })
end
root.keys(gears.table.join(
  root.keys(),
  awful.key({ "Mod4", "Shift" }, "x", nop, { description = "e2e quit", group = "awesome" }),
  e2e({ "Mod4", "Mod3" }, "x", "e2e mod3"),
  e2e({ "Mod4", "Mod5" }, "x", "e2e mod5"),
  e2e({ "Any" }, "F13", "e2e any"),
  e2e({ "Mod4" }, "#94", "e2e keycode"),
  e2e({ "Mod4" }, "é", "e2e accent")
))
