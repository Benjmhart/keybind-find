# keybind-find-toy (Lua template)

A complete keybind-find plugin for a toy format, in Lua using the standard library and the bundled json.lua. Copy
this folder, rename the plugin `keybind-find-<name>`, and replace the parser and the manifest. The guide,
[docs/plugins.md](../../docs/plugins.md), says what each part is for. Delete this paragraph and fill in the rest for
your program.

## For users

keybind-find shows toy's key bindings when toy is focused (keybind-find matches it by the program name `toy`).

### 1. Make the keymap readable

*(Part (a): what the user, or their agent, must change so the keymap can be read. Prefer a parser that needs no
change, and say so: "Nothing to change: it reads your existing `~/.config/toy/keys.conf`.")*

Nothing to change: it reads your existing `~/.config/toy/keys.conf`, one `key = description` line per binding.

### 2. Install

Install it: copy `keybind-find-toy` and `json.lua` into one folder, such as `~/.local/bin/` (it finds `json.lua`
beside the path it was run by, so a symlink elsewhere doesn't work). It runs `lua` from PATH: on Debian and Ubuntu,
where the binary is `lua5.4`, change its first line to `#!/usr/bin/env lua5.4`.

Super+/ runs keybind-find with your window manager's `PATH`, which may not have `~/.local/bin` or your `lua` (one
installed by nvm, pyenv or a version manager often isn't). So the stanza names the plugin by its full path, and if
`lua` isn't in `/usr/bin`, change the plugin's first line to its full path.

Then add it to keybind-find's config (`plugin add` writes the full path for you):

```sh
keybind-find plugin add toy
```

or write the stanza yourself, in `~/.config/keybind-find/config.toml`:

```toml
[[plugin]]
name = "toy"
program = "~/.local/bin/keybind-find-toy"
inputs = ["~/.config/toy/keys.conf"]
[plugin.match]
program = ["toy"]
```

### 3. Converting your config (optional)

*(Part (b), only if the keymap can't be read as it is: a conversion script that shows a diff, writes only after
the user confirms, backs up the original, and does nothing the second time. Say what it changes and why. This
template needs none: delete this section, or replace it.)*

## For the plugin's author

- **Test:** `lua test.lua` checks the sample (`tests/keys.conf`) against the sheet it should give
  (`tests/expected.json`).
- **Check it as keybind-find will:** `keybind-find check-plugin ./keybind-find-toy --input tests/keys.conf
  --program toy` runs it the way Super+/ does and prints one line per problem; it exits 0 when there are none.
- **Format:** one binding per line, `key = description`, optionally ending with ` ; scope=value` (spaces around
  the `;`; the name and value are printable ASCII words without `,`, `=`, `[` or `]`). Blank lines and
  `#` comments are skipped.
