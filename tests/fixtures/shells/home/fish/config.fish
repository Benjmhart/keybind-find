# A stand-in config: a banner before the keymap, and a bind that shows KEYBIND_FIND (`\cg` reads in fish 3 and 4).
echo 'Welcome!'
bind \cg "echo kbf=$KEYBIND_FIND"
echo 'bind ctrl-q banner-fake'
