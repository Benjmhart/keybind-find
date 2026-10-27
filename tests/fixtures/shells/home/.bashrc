# A stand-in rc file: a banner (one line shaped like a bind) before the keymap, and a bind that shows KEYBIND_FIND.
echo 'Welcome!'
echo '"\C-xq": banner-fake'
bind -x "\"\C-xk\": echo kbf=$KEYBIND_FIND"
