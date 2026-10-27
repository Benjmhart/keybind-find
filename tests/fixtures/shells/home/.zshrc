# A stand-in rc file: a banner (one line shaped like a bind) before the keymap, a bind, and one that shows KEYBIND_FIND.
echo 'Welcome!'
echo 'bindkey "^Xq" banner-fake'
bindkey "^Xt" transpose-words
bindkey -s "^Xk" "kbf=$KEYBIND_FIND"
