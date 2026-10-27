# Picker and key search

On X11, the list opens in **fzf** inside a small alacritty window (class and title `keybind-find`), in three aligned
columns: key, description, program. It opens in **key search** (below): press a binding's keys, then Enter to run it.
**Escape** switches to text search: type to filter; Enter runs the highlighted binding, Escape again closes the list.
**`ctrl-s`** changes what you search: all columns → keys → description → program → all, shown in the prompt
(`all>`, `keys>`, `description>`, `program>`). The most-used bindings come first, and among equally good matches the
list keeps that order.

**Key search** is on when the list opens, and **`ctrl-k`** starts it again from text search: you search by pressing
the keys themselves, Super combinations included (X11). Typing letters straight away presses keys rather than
searching text; press Escape first for that. The keyboard is grabbed
while you press, so xmonad's own bindings don't fire, and the header shows what you have pressed so far. After each
key the list shows only the bindings whose key **starts with** those keys, compared chord by chord: holding Super+Shift
shows every `super+shift+…` binding, `ctrl+b` shows all of herdr's prefix bindings, and `ctrl+b` then `t` narrows to
`ctrl+b t`. `super+s` never matches `super+shift+Return`, and `shift+k` matches `K`. **Enter** (Return or keypad
Enter, with no modifier) runs the highlighted binding. **Escape** ends key search and keeps the narrowed list in text
search; type to filter within it, `ctrl-k` to press keys again (`ctrl-k` then Escape brings back the whole list), or
Escape again to close the list. **BackSpace** takes back the last key pressed and the list widens to match what is
left; with nothing pressed it does nothing, and `ctrl+BackSpace` is a key like any other. Because a bare Return,
Escape or BackSpace is used by key search, a binding on one of them can't be searched for by pressing it (with a
modifier it can: `shift+Escape`, `super+Return`); move that key in the config ([config.md](config.md)) and it becomes searchable. With
no key pressed for 30 s, key search stops on its own and the header says `ctrl-k` starts it again. If it can't start
(no X display, another program holding the keyboard, `xmodmap` missing, fzf older than 0.66), the list opens in text
search and the header says why.

keybind-find's own keys are in the list too, after everything else, under program `keybind-find`: in key search,
run (Return, KP_Enter), text search (Escape) and undo (BackSpace); in text search, key search (`ctrl-k`), search scope
(`ctrl-s`), run (Return) and close (Escape). Each shows the key in force; picking one runs nothing. They are listed
only in the fzf picker, since the other pickers don't have these keys.

xmonad tiles a new window unless told otherwise, so float this one. In your `manageHook`:

```haskell
className =? "keybind-find" --> doCenterFloat
```

(`doCenterFloat` is from `XMonad.Hooks.ManageHelpers`.)

The picker is chosen in this order:

1. `KEYBIND_FIND_PICKER`, if set and not empty: any dmenu-style program, which reads lines on stdin and prints the
   chosen one. It overrides `[picker]` (below), as `kind = "dmenu"` with that command.
   ```sh
   KEYBIND_FIND_PICKER="rofi -dmenu -i" keybind-find
   ```
   The value is split on whitespace and run without a shell, so no quotes and no spaces inside one argument (a dmenu
   font name with a space won't work; use `[picker] command` for that).
2. `[picker] kind` in the config, if set (below).
3. fzf in a terminal (alacritty, or `[picker] terminal`), on X11 when both are installed. It needs fzf 0.60 or newer
   (for `--accept-nth`), and 0.66 for key search (`--listen` on a Unix socket; with an older fzf, the list opens in
   text search and says so). With an fzf older than 0.60, choose dmenu.
4. The dmenu-style command: `[picker] command`, else fuzzel on Wayland (`fuzzel --dmenu`), dmenu on X11
   (`dmenu -i -l 25`).

## `[picker]` in config.toml

```toml
[picker]
kind = "fzf"                                           # or "dmenu" or "json"; leave out for the order above
terminal = ["kitty", "--class", "keybind-find", "-e"]  # what fzf runs in
# command = ["rofi", "-dmenu", "-i", "-p", "a prompt"] # the dmenu-style or JSON frontend
```

Every key may be left out.

- **`kind`:** `"fzf"` uses fzf, also on Wayland (in a terminal that runs there, such as kitty or foot); there it
  opens in text search, since key search is X11 only (it is off whenever `WAYLAND_DISPLAY` is set). `"dmenu"` uses the dmenu-style `command` (default as in step 4). `"json"` runs `command` as a
  JSON frontend (below), and needs one.
- **`command`:** an argument list, one TOML string per argument, so an argument may hold spaces. It is run without
  a shell, so `~` isn't expanded: give a name on `PATH` or a full path. It is the dmenu-style picker's (with no `kind`, used where fzf isn't) or the JSON frontend; `kind =
  "fzf"` takes no command.
- **`terminal`:** the argument list fzf runs in; keybind-find appends `keybind-find __fzf <list> <out>` to it. The
  default is alacritty with class and title `keybind-find`, sized for the columns:
  `["alacritty", "--class", "keybind-find,keybind-find", "--title", "keybind-find", "-o",
  "window.dimensions.columns=140", "-o", "window.dimensions.lines=30", "-e"]`. Give your terminal the same class, so
  your window manager floats it. Only fzf uses it. When fzf or the terminal isn't installed, the dmenu-style picker
  is used (with `kind = "fzf"`, one stderr line says why).

  The terminal must stay open until its command exits: keybind-find reads the pick when the terminal returns, so a
  terminal that hands the command to a running server and returns at once loses it. kitty and foot wait (not
  `kitty --single-instance`, not `footclient`); use `wezterm start --always-new-process` and
  `gnome-terminal --wait --`.

A bad `[picker]` (an unknown kind or key, an empty list, `json` without a command) is one line, shown in the fzf
header or on stderr, and the default order is used; your `[keys]` still apply.

## JSON frontends

With `kind = "json"`, keybind-find runs `command` with one JSON object on stdin (protocol version 1) and waits for
the person to choose (up to a day):

```json
{
  "version": 1,
  "context": {"wm": "xmonad", "class": "Alacritty", "title": "main.rs - nvim", "program": "nvim",
              "facts": {"filetype": "rust"}},
  "entries": [
    {"id": 0, "key": "space f f", "desc": "Find files", "program": "nvim", "when": {}, "tags": []},
    {"id": 1, "key": "ctrl+w v", "desc": "Split vertically", "program": "nvim",
     "when": {"mode": ["normal", "visual"]}, "tags": ["[mode=normal,visual]"]}
  ]
}
```

- `context` is the focused window, each field `null` when keybind-find couldn't tell, and `facts`, what the running
  nvim reported (see [sheets.md](sheets.md), "Scopes").
- `entries` are in ranked order (most used first), already without what the facts rule out. `program` is the
  program column; `when` is what is left of each binding's scope once the facts are applied (each scope's value is
  one string, or a list of several), and `tags` is how the fzf picker shows it.

The frontend prints the chosen entry's `id` on stdout (its first non-blank line counts), or `<id> print` to have
keybind-find print the keys (as `--print` does: to its stdout, so seen only when keybind-find runs from a shell)
instead of sending them. Printing nothing, or exiting non-zero, is a cancel; a non-zero exit with something on
stderr, or by a signal, is also one stderr line, so a frontend that crashed doesn't pass for Escape. Anything else
printed is one stderr line, and nothing is sent. keybind-find still decides what is listed and sends only that entry's keys, and
a pick is logged for ranking exactly as an fzf pick is (a `print` pick, like `--print`, is not).

The frontend runs without a shell, in keybind-find's process group, so one that draws on the terminal (fzf on
`/dev/tty`) works when keybind-find is run from a shell. The request's `version` is the picker protocol's, 1.

**Key search** stays in the fzf picker. A frontend that wants it can run `keybind-find keys` itself: it grabs the
keyboard and prints each chord pressed, one per line in keybind-find's notation (`super+shift+Return`, `ctrl+b`),
until Escape or 30 s with no key (X11 only). Match those against the start of each entry's `key`, chord by chord.

**rofi:** [`templates/frontends/rofi/`](../templates/frontends/rofi/) is a Python (stdlib only) frontend that shows
the list in rofi, with the key, description, program and dimmed tags in columns:

```toml
[picker]
kind = "json"
command = ["keybind-find-rofi"]
```

## dmenu-style pickers

The dmenu-style pickers get the same columns, padded with spaces (`<key>  <description>  [<program>]`), so they line
up in a monospace font. Keys wider than 28 characters and descriptions wider than 60 overflow their own line only.

`KEYBIND_FIND_PICKER="head -1" keybind-find --print` runs the whole flow with no UI at all.
