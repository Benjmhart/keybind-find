# Changelog

All notable changes to keybind-find are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- foot: its key, search and URL bindings from defaults pinned per version (1.28.0) with your `foot.ini` on top (following `include=`), and its text bindings, when a foot or footclient window has focus.
- GNOME: its window, shell, mutter and media-key shortcuts, your custom shortcuts and the overlay key, from `gsettings`; on GNOME Wayland the list opens in fzf in alacritty (fuzzel needs layer-shell).
- micro: its keys from defaults pinned per version (2.0.11, 2.0.14, 2.0.15) with your `bindings.json` on top, when micro is in the focused terminal.
- zathura: its keys from defaults pinned per version (0.5.2, 0.5.11, 2026.10.23) with your zathurarc's `map`/`unmap` on top, when a zathura window has focus.
- yazi: its keys from pinned default keymaps (25.4.8, 25.5.31, 26.9.1) with your `keymap.toml` merged in, when yazi is in the focused terminal.
- i3: the bindings of the config i3 has loaded, from `i3-msg -t get_config`, read with sway's parser.
- Xfce: xfwm4's window shortcuts and the application shortcuts from `xfconf-query`, as Xfce picks your customized set or its defaults.
- kitty: its keys from pinned defaults (0.49.2) with your kitty.conf on top, and, with kitty's remote control on, the program in its focused tab.
- GNU screen: its command keys from pinned defaults (4.9.1, 5.0.2) with your screenrc on top, and nvim inside screen.
- Ghostty: its keys from `ghostty +list-keybinds` (key tables tagged), when a Ghostty window has focus.
- KDE Plasma: its global shortcuts and apps' launch shortcuts, the focused window on Wayland through `kdotool`, and, on KDE and GNOME under Wayland, an error naming the keys to press instead of a send that can only fail.

### Fixed

- Wayland: `XDG_CURRENT_DESKTOP=KDE` or `GNOME` names KDE or GNOME only while Plasma or GNOME Shell runs, and `SWAYSOCK`, `NIRI_SOCKET` or `HYPRLAND_INSTANCE_SIGNATURE` only while its socket exists, so other compositors no longer have their picks refused.
- mpv: a key your script binds at load no longer goes missing now and then; the sheet is read once the bindings settle.

## [0.1.0-alpha.1] - 2026-10-27

The first public pre-release. 1.0.0 follows once it has been used for a while.

### Added

Everything below is new.

#### Providers

- xmonad: the running keymap, with each binding's name, as `XMonad.Util.NamedActions` reports it.
- sway: `bindsym` lines from the sway config, following `include` and `set $var`; the focused window from sway's tree.
- alacritty: alacritty's defaults merged with your `[[keyboard.bindings]]`.
- nvim: your mappings from a headless nvim with your config, with each map's `desc`.
- herdr: herdr's defaults for its version, your `config.toml` and plugin commands.
- tmux: the running server's key tables, with your prefix and each key's note.
- nvim inside a herdr or tmux pane is found and listed.
- Vimium: its defaults for the installed version, and your remaps from a saved backup.
- espanso: your matches for the focused app; picking one types its trigger.

#### Picker and key search

- fzf in a floating alacritty window on X11, with aligned key, description and program columns.
- Key search (X11, fzf 0.66 or newer): press a binding's keys, Super combinations included, to narrow the list; it is how the list opens.
- `ctrl-s` changes the search scope (all, keys, description, program).
- fuzzel on Wayland (sway; other compositors read only XWayland windows), dmenu on X11 without fzf or alacritty, or any dmenu-style program through `KEYBIND_FIND_PICKER`.
- The picked binding is sent to the focused window (`xdotool` on X11, `wtype` on Wayland); `--print` prints it instead.
- `keybind-find dump [<provider>]` prints the sheets as JSON; `keybind-find keys` prints the keys you press.
- Errors under a hotkey are also shown as a desktop notification.

#### Sheets

- Your own JSON sheets and executable parsers in `~/.config/keybind-find/sheets/`, matched by window class.
- Web-app sheets, matched by window title in any common browser.
- Built-in sheets for Gmail, YouTube and GitHub.

#### Ranking

- The bindings you pick most come first, from a local usage log.
- The rest alternate between programs, the focused app first, then the window manager.

#### Config

- `config.toml` rebinds keybind-find's own picker keys (run, text search, undo, key search, scope); its own keys are listed too.

[Unreleased]: https://github.com/Benjmhart/keybind-find/compare/v0.1.0-alpha.1...staging
[0.1.0-alpha.1]: https://github.com/Benjmhart/keybind-find/releases/tag/v0.1.0-alpha.1
