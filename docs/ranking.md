# Most-used first

Each binding you pick and send is logged, and the list shows the ones you pick most at the top. The rest take turns by
program, one binding from each in turn, so the first screen isn't all window-manager keys: the innermost program first
(the focused app: nvim, the program inside herdr, or a window's static sheet), then the window manager, then the rest
from the inside out. Over nvim in alacritty that is nvim, xmonad, alacritty, espanso, nvim, xmonad, and so on. The log is
`$XDG_STATE_HOME/keybind-find/usage.tsv` (`~/.local/state/keybind-find/usage.tsv` when
`XDG_STATE_HOME` is unset or relative), one line per pick: the time in unix seconds, the provider and the key, tab-separated. It
holds nothing else: no descriptions, and for espanso the trigger, never its replacement. `--print` and a cancelled
list log nothing. Delete the file to reset the ranking.
