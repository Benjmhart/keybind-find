# keybind-find

Press one hotkey and get a searchable list of the keyboard shortcuts that work **where you are right now**: your
window manager's, the focused app's (nvim, tmux, herdr, alacritty, a web app) and your espanso snippets. Pick one
and keybind-find presses those keys for you, so it works as a command palette that also shows you the key you could
have pressed instead.

The lists are read from the programs' real configuration, your own bindings included, so nothing is kept up by hand.

![keybind-find over nvim: Super+/ opens the list of bindings for the focused window; pressing Space narrows it to the Space bindings; Escape switches to typing; "zen" puts Toggle Zen Mode on top; Enter sends Space u z and nvim enters zen mode.](docs/media/demo.gif)

## Works with

| Where | What is read | Setup |
|---|---|---|
| xmonad | your keymap, with each binding's name, and xmonad's stock keys | a few lines in `xmonad.hs`, incl. `ewmh`: [docs/xmonad.md](docs/xmonad.md) |
| sway | your `bindsym` lines (following `include` and `set $var`), and the focused window from sway's tree | none |
| Hyprland | its binds from the running compositor (`hyprctl -j binds`), with each bind's description, and the focused window from `hyprctl` | none ([below](#hyprland)) |
| KDE Plasma | its global shortcuts (`kglobalshortcutsrc`) and the launch shortcuts of `.desktop` files, and the focused window (on Wayland from `kdotool`) | on Wayland, `kdotool`; picks there are not sent ([below](#kde-plasma)) |
| any other X11 window manager | the focused window, so every app below works; not the window manager's own keys | an EWMH window manager (it sets `_NET_ACTIVE_WINDOW`) |
| other Wayland compositors (niri, GNOME) | espanso only: the focused window isn't read, so no app below | untested; fuzzel needs layer-shell (not GNOME), wtype the virtual-keyboard protocol (not GNOME or KDE: a pick there is refused, [below](#kde-plasma)) |
| alacritty | its keys: alacritty's defaults merged with your `[[keyboard.bindings]]` | none |
| nvim | your maps from a headless nvim with your config, with each map's `desc` | none |
| vim | your maps (`:map`, `:map!`, `:tmap`, `:lmap`) from a headless vim with your vimrc; not vim's built-in keys | none; it runs your vimrc ([below](#vim)) |
| nano | its main-menu keys: nano's defaults for its version (7.2, 8.4 and 9.2 built in) with your nanorc's `bind`/`unbind` on top | none ([below](#nano)) |
| herdr | herdr's defaults for its version, your `config.toml` and plugin commands | none |
| tmux | the running server's `list-keys`, so your prefix and bindings, with each key's note | none |
| bash, zsh, fish | the prompt's line-editing keys from the shell itself: bash's `bind -p`/`-s`/`-X` (with your `.inputrc`), zsh's `bindkey -L`, fish's `bind` (every mode) | none; it runs your rc files: [below](#shells-run-your-rc-files) |
| less | its keys from `less --help`, with your lesskey file's `#command` binds on top; also under `man`, `git log`, `systemctl status` | none; your keys need less 582+ and the lesskey source file ([below](#less)) |
| mpv | the bindings of a headless mpv started with your config: built-ins, your `input.conf` and keys your scripts bind at load, with each one's comment | none; it starts mpv on every `Super+/` ([below](#mpv)) |
| browsers | built-in sheets for Gmail, YouTube and GitHub; any web app you write a sheet for | none; GitHub's needs a URL-in-title extension ([docs/sheets.md](docs/sheets.md)) |
| Firefox / Chromium keys | the browser's own shortcuts (tabs, navigation, find, dev tools) in Firefox, LibreWolf, Zen, Chromium, Chrome, Brave and Edge, from built-in sheets | none ([below](#firefox-and-chromium)) |
| Vimium | Vimium's defaults for its version, and your remaps from a saved backup | none; remaps need the backup ([docs/vimium.md](docs/vimium.md)) |
| espanso | `espanso match list`; picking a match types its trigger | none |
| anything else | a sheet you write, or a script that prints one | [docs/sheets.md](docs/sheets.md) |

nvim, vim, nano, herdr, tmux, the shells and less are read when they are the program in the focused terminal window: alacritty, kitty, foot,
WezTerm, st, xterm/uxterm, Ghostty, GNOME Terminal, Konsole, Xfce Terminal, urxvt or Terminator (by window class, or
sway `app_id`, Hyprland `class`); nvim also inside a herdr or tmux pane. When one terminal process runs several shells
(tabs, splits, or several windows from one process: alacritty's daemon mode, `foot --server`, `urxvtd`; by default
GNOME Terminal, Xfce Terminal, WezTerm, Ghostty and Terminator), keybind-find can't tell which is the focused one: it prints a one-line warning
and shows the list without the program inside. espanso's `word: true` matches (they
wait for a space or punctuation after the trigger) and regex matches (no literal trigger) can't be typed, picks do nothing while the espanso service is stopped, and matches limited by `filter_exec` aren't
listed.

### Shells run your rc files

Over a shell prompt, keybind-find starts your shell as `bash -ic`, `zsh -ic` or `fish -ic` on every `Super+/`, so
**your rc files run each time** (`.bashrc`, `.zshrc`, `config.fish`): whatever they start, they start again. The
shell gets 3 seconds; an rc file that takes longer, or `exec`s another program (`exec tmux`), is killed and the shell
is left out with a one-line warning. keybind-find sets `KEYBIND_FIND=1` for it, so you can skip slow or noisy parts:

```sh
[ -n "$KEYBIND_FIND" ] && return   # bash or zsh: put it after your binds, before the slow part
# fish: wrap the slow part in  if not set -q KEYBIND_FIND; ...; end
```

For fish it also sends the `fish_prompt` event (fish loads its bindings on the first prompt), so your prompt's event
handlers run too.

What it can't see: binds you made by hand in the running shell, binds a plugin adds later (zsh plugins that bind in
a hook or on first use), and an `INPUTRC` or `bind` set only in a login profile. For bash and zsh it shows the keymap
in force when the rc files are done: in vi mode that is insert mode (`viins`, bash's `vi-insert`), not command mode.
The shell it starts is the one on keybind-find's `PATH` (your window manager's), which may not be the one in the
terminal if you installed another. A script running in the terminal (`bash build.sh`) looks like a shell, so the
prompt's keys show then too.

### less

less is shown whenever it is any program in the terminal's foreground job, so it shows under `man`, `git log` and
`systemctl status` too, as well as the program that started it when keybind-find reads that one (a shell running a
script that pipes into less). Your keys are read from the lesskey **source** file less reads: `$LESSKEYIN`, else
`$XDG_CONFIG_HOME/lesskey`, `~/.config/lesskey` or `~/.lesskey`. Only its `#command` section is used; `#version`
lines are ignored. Before less 582, `lesskey` compiled your keys into a binary `~/.less`; a compiled file (`~/.less` or
`$LESSKEY`) is never read, on any version: you get less's defaults and a one-line note. Debian's `man` runs less
as `pager`, which is recognised too. `LESSKEYIN` and `XDG_CONFIG_HOME` come from keybind-find's environment (your
window manager's), not the shell's, so a value set only in an rc file isn't seen.

### mpv

mpv is shown when its window is focused (window class `mpv`, or the Flatpak's `io.mpv.Mpv`), not when mpv plays
inside a terminal. On every `Super+/` keybind-find starts mpv headless (`--vo=null --ao=null --idle=once`: no window,
no sound, no file) with your config, so **your scripts load each time**, as at any mpv start, and a heavy config makes
the list slower; mpv gets 3 seconds. It never takes over your player's `input-ipc-server` socket or `log-file`. The
bindings are those of an idle mpv: keys a script binds later, while a file plays (uosc and similar), exist only in the
running player and aren't listed. Mouse and wheel bindings and keys bound to `ignore` are left out. A Flatpak-only
mpv isn't run (keybind-find needs `mpv` on `PATH`), but when mpv finds no config directory of its own
(`$XDG_CONFIG_HOME/mpv` or `~/.config/mpv`, or `~/.mpv`), the Flatpak's (`~/.var/app/io.mpv.Mpv/config/mpv`) is
used.

### Firefox and Chromium

A Firefox window (also Firefox Developer Edition, LibreWolf and Zen, and their Snap and Flatpak builds) lists
Firefox's own shortcuts; a Chromium, Google Chrome, Brave or Edge window lists Chromium's. Vivaldi and Opera have
keymaps of their own and get neither. The keys are those of the version named in the sheet's title
([data/sheets/firefox.json](data/sheets/firefox.json), [chromium.json](data/sheets/chromium.json)), as the browser's
support page lists them for Linux; another version may differ. They are left
out on a tab where a web-app sheet applies (Gmail, YouTube, GitHub or a `.json` sheet of yours with a `window_title`;
their loose title matches, such as `github.com` on `docs.github.com`, count too), and in an installed Chromium,
Brave or Edge app (PWA) window, on Wayland (by its class) and X11 alike (on X11 by its `WM_CLASS` instance,
`crx_<app id>`). A `--app=URL` window isn't told apart anywhere and still gets them, as does an app window in
XWayland under Hyprland (`hyprctl` doesn't report the instance). Brave and Edge get Chromium's keys, LibreWolf and
Zen Firefox's, which they share; their own extra keys (Zen's workspaces, say) aren't listed. To replace
one, put a `firefox.json` or `chromium.json` in your sheets folder ([docs/sheets.md](docs/sheets.md)): it turns the
built-in sheet off for the whole family, but applies only by its own `provider`, as any sheet does (`firefox` is
Firefox's window class only; LibreWolf or Zen need a sheet with their class).

### vim

Over vim or vi in a terminal, or a gvim window (class `Gvim`), keybind-find starts vim headless (`vim -es`) with your
vimrc (`~/.vimrc`, `~/.vim/vimrc` or `~/.config/vim/vimrc`, the first that exists) on every `Super+/` and lists your
maps, each tagged with its modes. Your vimrc and plugins run, as at any vim start (but not `VimEnter` autocommands,
so maps made there are missing); vim gets 10 seconds. A vimrc that prints or has an error is fine: the maps are read all the same. With no vimrc you get what a
plain vim start sets (`defaults.vim`). vim's own keys aren't listed: they are vim's documentation's (`:help index`),
and your maps are what you forget. Left out: plugins' `<Plug>` and script-local maps, buffer-local maps (a headless
vim has no file of yours open), maps made by hand in the running vim, the system vimrc's (`/etc/vimrc`), `$EXINIT`
and `~/.exrc` (with no vimrc), and a gvim's `.gvimrc` and `has('gui_running')` branches. `~/.config/vim/vimrc` is
read as vim 9.1.0327 and later read it; an older vim ignores it. vim run as `vimdiff`, `view` or `rvim` isn't
recognised. `HOME`, `XDG_CONFIG_HOME` and `VIMINIT` are keybind-find's (your
window manager's environment), not the running vim's; when `VIMINIT` is set, vim reads it instead of a vimrc, and
keybind-find says so and shows nothing for vim. The `vim` on `PATH` is run (else `vi`, Debian's vim-tiny): a `vim`
that is nvim gets nvim's list, and a vim without `+eval` (vim-tiny) is a one-line note. Over a `vi` that is another
editor (ex-vi, busybox), the list is still `vim`'s.

### nano

Over nano or rnano in a terminal, keybind-find lists nano's main-menu keys: the defaults of your nano's version
(from `nano --version`; 7.2, 8.4 and 9.2 are built in, taken from nano's source at each tag; another version uses
the nearest of the same major version, else the nearest older, and says so), with the `bind` and `unbind` lines of
`/etc/nanorc` and then your nanorc (the first readable of `~/.nanorc`, `$XDG_CONFIG_HOME/nano/nanorc`,
`~/.config/nano/nanorc`) laid over them, for the `main` and `all` menus, as nano reads them: a key name or a function
nano doesn't take changes nothing, and a bind to a macro or to another menu's function takes the key away. Macros and
other menus are not listed. `include` lines are checked (a missing file is a one-line note), but nano reads only
syntax definitions from included files, so they change no key. The defaults are a normal build's with no options:
`--modernbindings`, `--rcfile` and the like aren't read, rnano is shown with nano's keys, and a nano built with
another system nanorc than `/etc/nanorc` has that file's binds missed.

### Hyprland

Under Hyprland, keybind-find asks the running compositor for its binds (`hyprctl -j binds`) and never reads your
config (from Hyprland 0.56 it is Lua). A bind shows its `description`, or else its dispatcher and argument
(`exec kitty`); in a 0.56 Lua config without a description that is only the dispatcher's name (`exec_cmd`) or
`lua function`, so give binds a `description` to find them by what they do. Binds are grouped by dispatcher. A bind in
a submap is tagged with it (`[submap=resize]`), since keybind-find can't tell which submap is active. Mouse and wheel
binds, catch-all binds, binds by keycode (`code:NN`), long-press binds, key names that aren't keysym names, and binds
in a submap whose name has spaces or other characters a scope name can't hold can't be sent or scoped, and are left
out, counted in one line. Caps Lock and Num Lock in a bind's modifiers are ignored. The
focused window comes from `hyprctl -j activewindow`, never from X, so a native Wayland window is never mistaken for the
last XWayland one; if `hyprctl` fails, that is a line for the window and one for the binds, and the list goes
on without them.

### KDE Plasma

On Plasma, keybind-find reads the shortcuts kglobalacceld saves in `~/.config/kglobalshortcutsrc` (or under
`$XDG_CONFIG_HOME`), grouped by component (KWin, Plasma, Session Management, …), each shown by its friendly name.
Only shortcuts you have saved there are listed: a component that never wrote its shortcuts to the file isn't, since
the running kglobalaccel (over D-Bus) is not asked. Apps' launch shortcuts (`X-KDE-Shortcuts` in their `.desktop`
files under `kglobalaccel/` and `applications/`) are listed under `launch`, with your changes from the file on top.
Keys keybind-find can't send (keypad keys, a key Qt names but no X keysym matches, non-ASCII keys, a modifier alone
like `Meta`) are left out, counted in one line.

On X11 the focused window is read with `xdotool` as on any X11 window manager. On Wayland it comes from
[`kdotool`](https://github.com/jinliu/kdotool) (0.3 for Plasma 6; not in Arch's or Debian's repositories: AUR or
`cargo install kdotool`); without it, that is one line and the list has only Plasma's shortcuts and espanso.

On Wayland a pick is not sent: KWin doesn't offer the virtual-keyboard protocol that `wtype` needs, so keybind-find
says which keys to press (`press super+e yourself`) instead. `--print` prints as anywhere else. The list shows with
fuzzel, which works on KWin.

## Install

From source, now:

```sh
cargo install --path .
```

This puts `keybind-find` in `~/.cargo/bin`. Your window manager starts it, so that directory must be on the `PATH`
the window manager was started with, not only your shell's. If it isn't, use the full path in the binding below.

Other channels; those still planned are marked:

- a static binary and a `.deb` on the [GitHub release page](https://github.com/Benjmhart/keybind-find/releases)
  (pre-releases for now)
- crates.io: `cargo install keybind-find` *(from 1.0.0)*
- Arch (AUR): `keybind-find`, or `keybind-find-bin` for the prebuilt binary *(from 1.0.0)*
- Nix: `nix run github:Benjmhart/keybind-find` *(from 1.0.0)*
- Gentoo: the GURU overlay *(from 1.0.0)*

## Bind it to `Super+/`

`Super+/` is free in stock xmonad (its help key is `Super+Shift+/`) and in sway, and it is easy to remember as "help".

- **xmonad:** follow [docs/xmonad.md](docs/xmonad.md). There, `Super+/` writes xmonad's keymap for keybind-find to
  read and then starts it.
- **sway:** `bindsym $mod+slash exec keybind-find` (with `$mod` set to `Mod4`).
- **Anything else:** bind `Super+/` to run `keybind-find`.

Prefer another key? Change only that one binding; keybind-find doesn't care which key started it.

Under xmonad, float the picker window: `className =? "keybind-find" --> doCenterFloat` in your `manageHook`
(`doCenterFloat` is from `XMonad.Hooks.ManageHelpers`).

## Using it

On X11 the list opens in fzf, in **key search**: press a binding's keys (Super combinations included) and the list narrows to the
bindings that start with them; Enter runs the highlighted one. **Escape** switches to typing to filter, `ctrl-k`
goes back to pressing keys, and `ctrl-s` picks which column you search. Typing letters straight away presses keys; press Escape first
to filter by text. On Wayland the list opens in fuzzel: type to filter. The bindings you pick most come first.

- [docs/picker.md](docs/picker.md): the picker, key search, and other pickers (`[picker]` in the config: rofi,
  kitty instead of alacritty, dmenu, fuzzel, or a JSON frontend of your own)
- [docs/config.md](docs/config.md): moving keybind-find's own keys in `config.toml`
- [docs/sheets.md](docs/sheets.md): your own sheets, web apps, the built-in sheets and the sheet format
- [docs/vimium.md](docs/vimium.md): Vimium's keys and your remaps
- [docs/ranking.md](docs/ranking.md): most-used first, and the usage log
- [docs/plugins.md](docs/plugins.md): add a program: write a plugin (templates in Rust, Python, Lua and JS)

Other commands:

```sh
keybind-find --print         # the same flow, but print the commands instead of sending keys
keybind-find dump            # every sheet for the focused window, as a JSON array
keybind-find dump nvim       # one provider's sheet (xmonad, sway, espanso, nvim, herdr, tmux, ...)
keybind-find keys            # grab the keyboard, print each key you press in binding notation; Escape stops (X11)
```

## Requirements

<!-- Packages checked 2026-10-25 on archlinux.org/packages and Debian's archive (api.ftp-master.debian.org madison):
     Debian stable is trixie. -->

| Tool | Needed for | Arch | Debian |
|---|---|---|---|
| `xdotool` | X11: the focused window and sending keys | `xdotool` | `xdotool` |
| `xprop` | as `xdotool`: the window's class, title and pid | `xorg-xprop` | `x11-utils` |
| `fzf` 0.60+ (0.66+ for key search) | the list on X11 | `fzf` | `fzf` (stable has 0.60: no key search) |
| `alacritty` | the window fzf runs in | `alacritty` | `alacritty` |
| `xmodmap` | key search, and checking your `config.toml` keys | `xorg-xmodmap` | `x11-xserver-utils` |
| `dmenu` | the list on X11 without fzf or alacritty (or with fzf older than 0.60, via `KEYBIND_FIND_PICKER`) | `dmenu` | `suckless-tools` |
| `fuzzel` | the list on Wayland | `fuzzel` | `fuzzel` |
| `wtype` | Wayland: sending keys | `wtype` | `wtype` |
| `swaymsg` | sway: the focused window | `sway` | `sway` |
| `hyprctl` | Hyprland: its binds and the focused window | `hyprland` | `hyprland` |
| `kdotool` | KDE Plasma 6 on Wayland: the focused window | AUR `kdotool`, or `cargo install kdotool` | not packaged: `cargo install kdotool` |
| `notify-send` | errors shown when run from a hotkey | `libnotify` | `libnotify-bin` |

Each app's provider needs that app (`nvim`, `herdr`, `tmux`, `espanso`), and is only asked when it applies.

## Troubleshooting

- **Nothing happens on the hotkey.** The window manager can't find `keybind-find`: put the full path
  (`~/.cargo/bin/keybind-find`) in the binding, or add `~/.cargo/bin` to the `PATH` your session starts with.
- **No error shown under a hotkey.** Errors that stop keybind-find also go to a desktop notification when stderr isn't
  a terminal, but only if `notify-send` is installed.
- **"no bindings to show".** Nothing applies there: no espanso, xmonad's keymap not written yet (start it with the key
  from [docs/xmonad.md](docs/xmonad.md)), and an app keybind-find doesn't read and has no sheet for. With "every
  provider failed", the focused window wasn't read either: check `xdotool` and `xprop`, and that the window manager
  is EWMH (xmonad needs `ewmh`). On another X11 window manager a stderr line about xmonad's keymap is harmless; a
  `~/.cache/keybind-find/xmonad.keys` left from xmonad is still listed there, so delete it.
- **A provider is missing from the list.** A provider that can't run (not installed, a config error, xmonad's keymap
  not written yet) prints one line to stderr and is left out; the others still show. Run `keybind-find` from a
  terminal to see those lines, or `keybind-find dump <provider>` to see one provider's error.
- **Key search doesn't start.** The list opens in text search instead (type to filter), and the header says why:
  for example no `DISPLAY`, fzf older than 0.66, no `xmodmap`, or another program holding the keyboard.
- **Wayland.** The fzf picker and key search are X11 only; on Wayland the list opens in fuzzel
  (or `KEYBIND_FIND_PICKER`). Outside sway, only
  XWayland windows are read.

## Development

Tool versions (Rust, and the nvim the tests run against) are pinned with [mise](https://mise.jdx.dev):

```sh
mise install
mise run ci      # cargo fmt --check, clippy -D warnings, cargo test
```

Use `mise run ci` rather than bare `cargo test`, which would test against whatever nvim and Rust are on your `PATH`.
CI runs the same task on every pull request.

## License

MIT, see [LICENSE](LICENSE).
