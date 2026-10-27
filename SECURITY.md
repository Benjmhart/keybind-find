# Security policy

## Reporting a vulnerability

Please report security problems privately, through GitHub's private vulnerability reporting: the repository's
**Security** tab, then **Report a vulnerability**. Don't open a public issue for them.

## What keybind-find does that matters for security

- **It sends keystrokes to the focused window.** The binding you pick is sent with `xdotool` (X11) or `wtype`
  (Wayland). Keys come from the sheets it read, so a sheet decides what can be sent; keys outside keybind-find's key
  notation are refused, except espanso triggers, which are typed as written.
- **It runs executables from your sheets folder.** An executable file in `~/.config/keybind-find/sheets/` named after
  the focused window's class is run to print a sheet. It is run only if it is owned by you and not writable by group
  or others, and the folder must be yours and not writable by group or others too. It runs with no arguments and no
  shell, under a time limit.
- **It runs other programs**, always with explicit arguments:
  - the programs it reads: nvim (headless, with your config), `tmux`, `herdr`, `espanso`, `swaymsg`, `xdotool`,
    `xprop`, `xmodmap`;
  - the picker: `alacritty` with `fzf`, `fuzzel` or `dmenu`, or whatever program `KEYBIND_FIND_PICKER` names;
  - `notify-send` for errors under a hotkey.

  Config text never goes through a shell. Fixed commands do: `/bin/sh` kills a timed-out child's process group (the
  group id is an argument), and fzf's bindings run through `sh -c` (the scope switch, `cat` of the run directory's
  key list, and keybind-find's own path). Those paths are quoted, and one holding `'`, `{` or `}` is refused.
- **It grabs the keyboard** while key search is on (the list opens in it) and during `keybind-find keys`, so the
  window manager's bindings don't fire. The grab ends on Escape, a run key, an error, 30 seconds with no key, or
  exit. Pressed keys are only matched against the list, never stored.
- **It reads config files:** its own `config.toml`, sheets and Vimium backup, the configs of the programs it lists
  (sway, alacritty, herdr and its `plugins.json`, Vimium's extension folder), xmonad's keymap dump, and `/proc` to
  find the program inside a terminal.
- **It writes:** the usage log, `$XDG_STATE_HOME/keybind-find/usage.tsv` (default `~/.local/state/...`), which holds
  the time, provider and key of each pick (an espanso trigger, never its replacement); and, while the fzf picker is
  open, a `0700` directory under `$XDG_RUNTIME_DIR` (else the temporary directory) with the list, the key-search list,
  the pick and two local Unix sockets, removed when it closes.
- **It makes no network calls itself.** Programs it starts with your config can: a nvim plugin manager may try to
  install missing plugins.
