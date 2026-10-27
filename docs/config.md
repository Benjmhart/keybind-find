# Your own keys

`$XDG_CONFIG_HOME/keybind-find/config.toml` (`~/.config/keybind-find/config.toml` when `XDG_CONFIG_HOME` is unset or
relative) can move the five keys keybind-find defines, each as one chord or a list of chords in the key notation
of [sheets.md](sheets.md#format). These are the defaults:

```toml
[keys]
run = ["Return", "KP_Enter"]   # key search: run the highlighted entry
text_search = "Escape"         # key search: switch to text search
undo = "BackSpace"             # key search: take back the last key
key_search = "ctrl+k"          # text search: start key search
scope = "ctrl+s"               # text search: change the search scope
```

A key left out keeps its default; the headers and the listed keys follow what is in force. Text search's own run
(Return) and close (Escape) are fzf's and can't be moved. The config is refused when:

- two of `run`, `text_search` and `undo` share a chord (a conflict);
- a `run` or `undo` key is a modifier alone or a key your keyboard layout doesn't report (a capture reports base keys:
  `shift+slash`, not `question`), so it would never fire;
- `key_search` and `scope` share a key, or either is Return or Escape (`ctrl+m` is Return to a terminal);
- `key_search` or `scope` is a key fzf can't be told about: a `super` chord (a terminal never sees Super), or a key
  fzf has no name for (fzf knows letters, digits, F1–F12, Tab, space, BackSpace, Delete, Insert, the arrows, Home,
  End, Prior and Next, with the modifier combinations listed under "AVAILABLE KEYS" in `man fzf`);
- `text_search` is a modifier alone, or has no key in your keyboard layout, since then nothing could end key search.

Any key but `text_search` can be an empty list (`key_search = []`), which leaves that action with no key; with no
`key_search` key, key search runs only when the list opens.
A `key_search` or `scope` key takes that key away from text search: a plain letter can no longer be typed, and
BackSpace or an arrow no longer edits or moves. A refused or malformed config is one line at the front of the
fzf picker's header, and every key keeps its default.

## The picker

`[picker]` chooses the picker: fzf in another terminal (kitty instead of alacritty), a dmenu-style program such as
rofi, or a JSON frontend of your own. A bad `[picker]` is one line and the default is used; it doesn't refuse
`[keys]`. See [picker.md](picker.md#picker-in-configtoml).

```toml
[picker]
kind = "fzf"
terminal = ["kitty", "--class", "keybind-find", "-e"]
```

## Plugins

A plugin is a program that reads some files (or asks a running program) and prints a sheet of bindings for a program
keybind-find doesn't read itself. Declare each one in `config.toml` with a `[[plugin]]` stanza:

```toml
[[plugin]]
name = "btop"                                  # the program column, and its name in the usage ranking
program = "~/bin/btop-keys"                    # a name on PATH, or a path from / or ~/
inputs = ["~/.config/btop/btop.conf", "~/.config/btop/themes/*.theme"]
match = { class = ["Alacritty", "kitty"], program = "btop" }
```

- `name` and `program` are required. `name` can't be a built-in provider's (`nvim`, `sheets`, ...) or `keybind-find`.
- `inputs` (optional) are files keybind-find reads and passes along: `~/` is your home, and one `*` in the last path
  component matches any run of characters, hidden files included (`themes/*.theme`); matched files go in sorted
  order; a `*` skips anything that isn't a file. Each starts with `/` or `~/`. A missing file is skipped with one line
  on stderr. A file over 1 MiB, inputs over 8 MiB together, and a path given without `*` that is not a regular file,
  can't be read or isn't UTF-8 text, are errors.
- `match` says when it runs. Every key given must match the focused window, ignoring case: `class` (the window's
  class, as for [static sheets](sheets.md)), `program` (the program in the terminal's foreground, e.g. `nvim`) and
  `window_title` (a part of the title). Each is a non-empty string or list of them. A plugin with no `match` never
  runs, and says so on stderr. `program` is the terminal's own foreground program, by its process name (cut to 15
  characters by Linux): inside tmux or herdr it is `tmux: client` or `herdr`, not the program in the pane.

The plugin is run with no arguments and no shell. On stdin it gets one JSON request: `{"version": 1, "context": {"wm",
"class", "title", "program", "facts"}, "inputs": [{"path", "text"}, ...]}`; `wm` is the session (`xmonad`, `sway`,
`i3`, `kde`, ...), any field keybind-find couldn't tell is `null`, and `facts` is empty for now. On stdout it prints a sheet with
`"version": 1` beside its fields (`{"version": 1, "title", "provider", "binds"}`), its keys in the key notation of
[sheets.md](sheets.md#format). The `keybind-find-sheet` crate has these types and a ready-made `main` for plugins
written in Rust. Its sheet is listed under the plugin's `name`, and a pick sends the keys (an `action` is ignored).

### Installing one

A plugin is installed as an executable named `keybind-find-<name>` on `PATH` (`cargo install keybind-find-helix`),
or any executable in `~/.config/keybind-find/plugins/`. Super+/ runs only the plugins a stanza names; `plugin list`
and `plugin add` ask every installed one for `--manifest`.

- `keybind-find plugin list` shows each installed plugin, whether a stanza runs it, and what its manifest says.
- `keybind-find plugin add <name>` appends a commented stanza for it to `config.toml`, with the plugin's default
  `inputs` and `match`, and prints it. It only appends: your comments and order stay as they are. `program` is
  written as the plugin's path (`~/.cargo/bin/keybind-find-helix`), since Super+/ may not have your shell's `PATH`.
  The file is created if missing. A config that already has a stanza of that name (ignoring case) or for that
  program, that isn't valid TOML, or whose `plugin` isn't `[[plugin]]` stanzas, is refused and left as it is.

A stanza that leaves out `inputs` (or gives an empty list) or `match` takes the plugin's defaults; whatever the stanza
sets wins. The defaults come from the plugin's answer to `--manifest`:

```json
{"version": 1, "name": "helix", "description": "Helix's keymap", "inputs": ["~/.config/helix/config.toml"],
 "match": {"program": ["hx"]}, "queries": [], "homepage": "https://example.com/keybind-find-helix"}
```

`description`, `inputs`, `match` (lists of `class`, `program` and `window_title` values), `queries` (programs it asks
while running, rather than files it reads) and `homepage` are optional. keybind-find keeps each manifest in
`~/.cache/keybind-find/manifests.json` until the plugin's file changes, so a normal run asks no plugin for it (a
program with no manifest is remembered too). A manifest is asked for only when a stanza leaves something out and the
cache has none for the plugin's file as it is now: the first run after installing or upgrading it, one plugin at a
time, each within the 3 s limit. A program that couldn't be started or didn't answer in time is one line on stderr
and is asked again an hour later. A keybind-find upgrade asks again too. An empty `match` names no window. The `keybind-find-sheet` crate has a `Manifest` type for it.

### Order and failures

A plugin matched by `program` takes its turn in the list with the programs inside the terminal; any other takes it
with the static sheets. It runs at the same time as the providers; the program gets the same 3 s limit as a sheet parser (reading its inputs
comes before that); if it
crashes, runs out of time, or prints something that isn't a valid version 1 sheet, that is one line on stderr naming it
and the rest of the list still shows. A stanza that can't be read (a missing `name` or `program`, a wrong type, an
unknown key) is one line in the fzf picker's header, and the other plugins and your keys still apply.
