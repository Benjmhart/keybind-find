# Security policy

## Reporting a vulnerability

Please report security problems privately, through GitHub's private vulnerability reporting: the repository's
**Security** tab, then **Report a vulnerability**. Don't open a public issue for them.

## What keybind-find does that matters for security

- **It sends keystrokes to the focused window.** The binding you pick is sent with `xdotool` (X11) or `wtype`
  (Wayland), and under KDE Plasma and GNOME on Wayland through the desktop's RemoteDesktop portal
  (`org.freedesktop.portal.RemoteDesktop` on the session bus, keyboard only). The desktop asks you once; the approval
  is kept as a restore token in `~/.local/state/keybind-find/portal-token` (`$XDG_STATE_HOME/keybind-find/`), which
  lets keybind-find press keys without asking again until you delete that file. The portal ties the token to the
  calling app's id: empty for a program run outside a desktop-launched app's scope, otherwise that app's (a
  terminal's, say), so a token granted from one app won't restore from another and the dialog shows again. Any
  program of yours with the same id that can read the file could use it; its mode, 0600, keeps it from other users. Keys come from the sheets it read, so a sheet decides what
  can be sent; keys outside keybind-find's key notation are refused, except espanso triggers, which are typed as
  written.
- **It runs executables from your sheets folder.** An executable file in `~/.config/keybind-find/sheets/` named after
  the focused window's class is run to print a sheet. It is run only if it is owned by you and not writable by group
  or others, and the folder must be yours and not writable by group or others too. It runs with no arguments and no
  shell, under a time limit.
- **It runs other programs**, always with explicit arguments:
  - the programs it reads: nvim (headless, with your config), `tmux`, `herdr`, `espanso`, `swaymsg`, `niri` (`msg
    --json focused-window` under niri; niri's config is only read, its `spawn` commands shown, never run), `xdotool`,
    `xprop` (also `xprop -root _OB_CONFIG_FILE` under Openbox, the file Openbox names), `xmodmap`, `wezterm` (`show-keys` runs your `wezterm.lua`, as WezTerm does on every start; `cli
    list-clients`, without your config, asks the running WezTerm for its focused pane), `zellij` (`--version`, and
    `setup --dump-keybinds=<preset>` or `setup --dump-config`, which print zellij's built-in keys and start no session; zellij
    creates its cache directory and a log under `/tmp/zellij-<uid>` as on any start; and `action list-clients`, which
    asks the one running session for its focused pane's command and starts none), `dbus-send` (under awesome: it asks
    the running awesome for its hotkeys through `org.awesomewm.awful.Remote.Eval`, as `awesome-client` does; the Lua it
    sends is a fixed snippet in keybind-find that only reads `awful.key.hotkeys`, with nothing of yours put into it, and
    the reply is hex-decoded text, never run; with no `DBUS_SESSION_BUS_ADDRESS`, some libdbus builds start a session
    bus of their own for it), `lf` (`-remote "query <pid> maps"` and `cmaps`, which ask the focused lf for its key
    list through lf's own server; keybind-find sends lf nothing), `lazygit` (`--version`, which runs `git --version`
    and starts no session: the focused lazygit's own binary, `/proc/<pid>/exe`, else `lazygit` from PATH) and `git` (`rev-parse` in the focused lazygit's repo, to find the repo's lazygit files), `nnn` (`-V`, which prints
    its version and exits: the focused nnn's own binary, `/proc/<pid>/exe`, else `nnn` from PATH; that binary is
    picked by its comm `nnn`, which any process of yours can set), `kak` (`-version`, which prints its version and
    exits: the focused kak's own binary, `/proc/<pid>/exe`, else `kak` from PATH; and, the same binary, `-p
    <session>`, which sends a fixed script to the session of the kak in the focused terminal, yours: in the client with
    that kak's pid it writes `debug mappings` to `*debug*` and copies `*debug*` and the version into a `0700` directory
    keybind-find made with a random name in kak's `XDG_RUNTIME_DIR`, else keybind-find's own, whichever is yours and
    `0700` (an answer arriving after keybind-find gave up can't be redirected there; with neither, nothing is sent),
    removed after; the script also defines one hidden command, `keybind-find-report-<pid>`, emptied once it ran, and
    runs no shell; nothing else in the session is changed), qutebrowser's Python (the interpreter on the `#!` line of
    the `qutebrowser` on PATH, run only when that line is a Python one: an absolute `…/python*`, or `…/env python*`
    or `…/env -S python*` (then with PATH's absolute entries only), with flags only; `-I -c` with a fixed script that prints qutebrowser's version and your `autoconfig.yml`
    as PyYAML loads it), and the version checks of programs whose defaults are built in, each from PATH with a 2-second
    limit and only a version read from what it prints: `konsole --version` (with Qt's offscreen platform, so it needs
    no display; it prints its version and exits before any window or D-Bus, though, as on every Konsole start, it may
    first rename old keys in `konsolerc`), `mc --version`, `ranger --version` (before any config is read), `ncmpcpp
    --version` (before connecting to MPD), `lf -version`, `gimp --version` (it prints its version and exits before
    any window or plug-in) and `inkscape --version` (it prints its version and exits before any window or document);
  - the picker: `alacritty` with `fzf`, `fuzzel` or `dmenu`, or whatever program `KEYBIND_FIND_PICKER` names;
  - `notify-send` for errors under a hotkey.
- **It has your qutebrowser run a script of its own.** Over a qutebrowser window, keybind-find writes a fixed Python
  script (only its directory's path filled in, as a string literal) into a `0700` directory with a random name that it
  creates beside qutebrowser's IPC socket, in qutebrowser's runtime directory (only if that directory is yours and
  `0700`), and sends `:debug-pyeval --file` with it over that socket, which runs it inside the browser. The script
  only reads qutebrowser's bindings and current mode and writes them to one file in that directory; keybind-find
  removes the directory afterwards, answer or not, and treats the answer as data (commands are shown, never run).

  Config text never goes through a shell. Fixed commands do: `/bin/sh` kills a timed-out child's process group (the
  group id is an argument), and fzf's bindings run through `sh -c` (the scope switch, `cat` of the run directory's
  key list, and keybind-find's own path). Those paths are quoted, and one holding `'`, `{` or `}` is refused.
- **It grabs the keyboard** while key search is on (the list opens in it) and during `keybind-find keys`, so the
  window manager's bindings don't fire. The grab ends on Escape, a run key, an error, 30 seconds with no key, or
  exit. Pressed keys are only matched against the list, never stored.
- **It reads config files:** its own `config.toml`, sheets and Vimium backup, the configs of the programs it lists
  (sway, alacritty, herdr and its `plugins.json`, zellij's `config.kdl`, sxhkd's `sxhkdrc` and extra configs, Konsole's `konsolerc`,
  `kxmlgui5/konsole/*ui.rc` and shortcut scheme files, lf's `lfrc` files and the files they `source`, lazygit's `config.yml` files and the repo's `.lazygit.yml` and
  `.git/lazygit.yml` (only their `keybinding:` section is parsed; nothing in them is run), Kakoune's kakrc, `kakrc.local`
  and autoload `*.kak` files (only their `map`, `unmap`, `declare-user-mode`, `try` and `evaluate-commands` are read;
  nothing in them is run), Openbox's `rc.xml` (the file in the root window's `_OB_CONFIG_FILE` property, else the
  first `openbox/rc.xml` of your XDG config dirs; only its `<keyboard>` keybinds are read, their actions shown as text,
  never run), Vimium's extension folder), xmonad's keymap dump, and `/proc` to find the program inside a terminal and your own sxhkd's
  and the focused lf's and lazygit's config files (`/proc/<pid>/{cmdline,environ,cwd}` of your `sxhkd` processes and
  of that `lf` or `lazygit` only, and that lazygit's `/proc/<pid>/exe`), and `/proc/<pid>/{comm,environ}` of the nnn in
  the focused terminal (its `NNN_PLUG` and `NNN_BMS` are shown, never run), and `/proc/<pid>/{comm,cmdline,environ,exe}`
  of the kak in the focused terminal (its session name and socket directory, and its config and runtime paths), and
  `/proc/<pid>/{comm,fd,net/unix}` of the focused qutebrowser window's process (to find its IPC socket); the first 256
  bytes of the `qutebrowser` on PATH (its `#!` line); qutebrowser's `autoconfig.yml` (read by qutebrowser's own Python; a `config.py` beside it is only checked for, never read); GIMP's
  `shortcutsrc` (its `GIMP3_DIRECTORY` or `GIMP/3.N` config directory; the file is read whole, at most 4 MiB and a regular
  file only, and only its `action` forms are used); Inkscape's `preferences.xml` and keys files (the preset in its
  `share/inkscape/keys/`, the shared and your profile's `keys/default.xml`, and the local files they `xi:include`, at
  most 8 deep and never one already being read; each read whole, at most 4 MiB and a regular file only; only `bind`
  elements are used, their actions shown as text, never run).
- **It writes:** the usage log, `$XDG_STATE_HOME/keybind-find/usage.tsv` (default `~/.local/state/...`), which holds
  the time, provider and key of each pick (an espanso trigger, never its replacement); and, while the fzf picker is
  open, a `0700` directory under `$XDG_RUNTIME_DIR` (else the temporary directory) with the list, the key-search list,
  the pick and two local Unix sockets, removed when it closes.
- **It makes no network calls itself.** Programs it starts with your config can: a nvim plugin manager may try to
  install missing plugins.
