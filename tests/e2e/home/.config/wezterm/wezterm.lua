-- keybind-find's e2e WezTerm config: the case's key, a leader with its sequences, a key table, and one bind of each
-- kind wezterm.rs leaves out on purpose, so the wezterm-oracle case checks its known differences
-- (tests/e2e/oracle.py KNOWN).
local wezterm = require 'wezterm'
local act = wezterm.action
local config = wezterm.config_builder()
config.leader = { key = 'a', mods = 'CTRL', timeout_milliseconds = 1500 }
config.keys = {
  { key = 'x', mods = 'CTRL|SHIFT', action = act.CloseCurrentTab { confirm = false } },
  { key = 'c', mods = 'LEADER', action = act.SpawnTab 'CurrentPaneDomain' },
  { key = 'r', mods = 'LEADER|SHIFT', action = act.ActivateKeyTable { name = 'e2e', one_shot = false } },
  { key = 'phys:K', mods = 'CTRL|ALT', action = act.ShowLauncher },
  -- Left out: a non-ASCII key, a named key with no X keysym here, a raw key code.
  { key = 'é', mods = 'CTRL|ALT', action = act.ShowLauncher },
  { key = 'VolumeUp', mods = 'CTRL', action = act.ShowLauncher },
  { key = 'raw:123', mods = 'CTRL', action = act.ShowLauncher },
}
config.key_tables = {
  e2e = {
    { key = 'Escape', action = 'PopKeyTable' },
    { key = 'LeftArrow', action = act.AdjustPaneSize { 'Left', 1 } },
  },
  -- Left out: a table name that can't be a scope.
  ['e2e wide'] = { { key = 'Escape', action = 'PopKeyTable' } },
}
return config
