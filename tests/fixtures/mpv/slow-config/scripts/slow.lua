-- Test script for keybind-find's mpv provider: about 200 ms of work at load before binding its key.
local start = os.clock()
while os.clock() - start < 0.2 do end
mp.add_key_binding("Ctrl+j", "kbf-slow", function() end)
