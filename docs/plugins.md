# Writing a plugin

A plugin teaches keybind-find a program it doesn't read itself. It is a small program that reads that program's
config (or asks the running program) and prints its key bindings as a sheet. Users install it, add one stanza to
their config, and from then on Super+/ over that program lists its keys.

Start from a template in [`templates/`](../templates/): Rust, Python, Lua or JavaScript. Each is a complete plugin for
a toy format, with tests and a README to fill in. This page says what each part is for.

A plugin package has four parts:

- (a) user docs;
- (b) optionally, a conversion script;
- (c) the plugin program;
- tests.

## (a) User docs

The README tells a user, or their agent, two things.

1. **What to change so the keymap can be read.** Usually nothing: the best plugin reads the config as it is. When
   something must change, such as a setting that writes the keymap to a file or a comment marking each binding, say
   exactly what, with a before and after.
2. **How to add it to keybind-find:** `keybind-find plugin add <name>`, or the `[[plugin]]` stanza to paste into
   `~/.config/keybind-find/config.toml` (see [config.md](config.md#plugins)):

   ```toml
   [[plugin]]
   name = "toy"
   program = "~/.local/bin/keybind-find-toy"
   inputs = ["~/.config/toy/keys.conf"]
   [plugin.match]
   program = ["toy"]
   ```

   Give `program` as a full path (`~/` is fine). Super+/ runs keybind-find with the window manager's `PATH`, which
   often lacks `~/.cargo/bin` or `~/.local/bin`. `plugin add` writes the path for the user. The same goes for a
   script's interpreter: `#!/usr/bin/env node` finds no node installed by nvm, so say to change that line to the
   interpreter's full path when it isn't in `/usr/bin`.

## (b) A conversion script (optional)

Prefer a parser that needs no config change. When the keymap really can't be read as it is, ship a script that
makes the change for the user. It must:

- **show a diff** of what it would change;
- **write only after the user confirms** (an agent running it confirms the same way);
- **back up the original** next to it, with the date in the name;
- **be safe to run twice:** the second run finds nothing to change and says so.

## (c) The plugin program

**The protocol, version 1.** keybind-find runs the plugin with no arguments and no shell.

- **The request, on stdin,** is one JSON object:

  ```json
  {"version": 1,
   "context": {"wm": "xmonad", "class": "Alacritty", "title": "notes - toy", "program": "toy", "facts": {}},
   "inputs": [{"path": "/home/me/.config/toy/keys.conf", "text": "ctrl+s = Save the file\n"}]}
  ```

  `inputs` are the files the stanza names, already read, with `~` and `*` expanded. A field keybind-find couldn't
  tell is `null`. `facts` holds what keybind-find knows about where the user is, such as nvim's `mode`.
- **The response, on stdout,** is one JSON object, a sheet with the version beside it:

  ```json
  {"version": 1, "title": "Toy", "provider": "toy",
   "binds": {"General": [{"key": "ctrl+s", "desc": "Save the file", "action": ""}]}}
  ```

  Keys are in keybind-find's key notation ([sheets.md](sheets.md#format)): `ctrl+shift+z`, `ctrl+b c` for a
  sequence. Categories group the list. Print nothing else on stdout; diagnostics go to stderr.
- **Errors:** print one line on stderr and exit with status 1. keybind-find shows that line, leaves the plugin out
  of this list, and shows the rest. Name the file and line that broke ("keys.conf: line 2: not `key =
  description`").
- **`--manifest`:** run with that one argument, the plugin prints who it is and its defaults, then exits 0 without
  reading stdin:

  ```json
  {"version": 1, "name": "toy", "description": "The toy format's key file",
   "inputs": ["~/.config/toy/keys.conf"], "match": {"program": ["toy"]}, "queries": [],
   "homepage": "https://example.com/keybind-find-toy"}
  ```

  `plugin add` writes the stanza from it, and a stanza that leaves out `inputs` or `match` takes these. `queries`
  lists programs it asks while running, rather than files it reads. A plugin with no manifest should exit non-zero
  for `--manifest`. Printing anything else there is an error.
- **Version:** the request and response both carry `"version": 1`. Refuse a request of another version with one
  line on stderr, so a later keybind-find gets a clear message rather than a wrong sheet.

**Scopes (`when`).** A binding that applies only somewhere carries `"when": {"mode": "insert"}` (a scope name and
its value, or a list of values). **Report every binding with its scope; never filter by guessing.** keybind-find
decides what applies from what it knows (the running nvim's mode and filetype, say). It hides a binding only when
one of its scopes contradicts a fact, and shows every other scope as a tag. A plugin that drops bindings because it
guessed the mode hides keys the user is looking for. The rules for names and values are in
[sheets.md](sheets.md#format), "Scopes".

**Files or the live program.** Parse files when the files are the keymap. Ask the running program when the files
aren't the whole story. Frameworks such as LazyVim build their maps in code, and plugins add maps at startup, so
only the running program knows them all. List what you ask in the manifest's `queries`. A plugin that asks a
running program usually takes no `inputs`, and has nothing to say when the program isn't running: print an empty
sheet then, not an error.

**Keys only.** A pick sends the binding's keys to the focused window; an `action` is ignored. List what a person
presses, not commands.

**Time limit.** Each run gets 3 seconds, and plugins run at the same time as the built-in providers. A plugin that
takes longer is killed, and that is one stderr line. Parse, print and exit; don't wait on anything slow.

## Tests

Each template has:

- a sample input and the sheet it should give (`tests/`), compared by its own test command (`cargo test`, `python3
  test_plugin.py`, `node test.js`, `lua test.lua`);
- a test that a bad line is one stderr line and exit status 1.

Then check it the way keybind-find will run it:

```sh
keybind-find check-plugin ./keybind-find-toy --input tests/keys.conf --program toy
```

It asks for the manifest, sends the request, and reports each problem on one line with where it is: a key not in
the notation, an empty description, a key twice in one category and scope, a malformed `when`, a wrong version, a
crash, a run over the time limit. It exits 0 only when there are none, and `--json` gives the same report for CI.
In this repository, `templates/check.sh` runs every template's test and `check-plugin` on it.

## Naming and publishing

- **Name:** call the executable `keybind-find-<name>`, where `<name>` is the program it reads (`keybind-find-helix`).
  `keybind-find plugin list` finds every executable of that form on `PATH`, and any executable in
  `~/.config/keybind-find/plugins/`.
- **Publish** wherever your language's users install from: crates.io (`cargo install keybind-find-helix`), PyPI,
  LuaRocks, npm, or a plain repository with a release. Rust plugins depend on
  [`keybind-find-sheet`](../crates/keybind-find-sheet/), which has the types, the key checks, a ready-made `run`
  (and `run_request`, used by the template, for a plugin that joins its inputs itself or reads none), and
  `testing::assert_fixture`.
- **It is code that runs on every Super+/.** Users trust it with their session. Read only the files and programs
  you list, write nothing, open no network connection, and keep it small enough to read.

## Picker frontends

A plugin adds bindings; a picker frontend shows the list. To put keybind-find's list in a UI of your own (rofi, a
launcher, a web view), write a JSON frontend instead: it gets the ranked list on stdin and prints the chosen id. The
protocol is in [picker.md](picker.md#json-frontends), and
[`templates/frontends/rofi/`](../templates/frontends/rofi/) is a complete one in Python, with its test.
