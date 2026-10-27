# Changelog

All notable changes to keybind-find are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Ghostty: its keys from `ghostty +list-keybinds` (key tables tagged), when a Ghostty window has focus.
- KDE Plasma: its global shortcuts and apps' launch shortcuts, the focused window on Wayland through `kdotool`, and, on KDE and GNOME under Wayland, an error naming the keys to press instead of a send that can only fail.

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
