# keybind-find-toy (Rust template)

A complete keybind-find plugin for a toy format, in Rust using `keybind-find-sheet`'s `run_request` and
`testing::assert_fixture`. Copy this folder, rename the plugin `keybind-find-<name>`, and replace the parser and the
manifest. The guide, [docs/plugins.md](../../docs/plugins.md), says what each part is for. Delete this paragraph and
fill in the rest for your program.

## For users

keybind-find shows toy's key bindings when toy is focused (keybind-find matches it by the program name `toy`).

### 1. Make the keymap readable

*(Part (a): what the user, or their agent, must change so the keymap can be read. Prefer a parser that needs no
change, and say so: "Nothing to change: it reads your existing `~/.config/toy/keys.conf`.")*

Nothing to change: it reads your existing `~/.config/toy/keys.conf`, one `key = description` line per binding.

### 2. Install

Install it: `cargo install --path .` (it goes to `~/.cargo/bin/keybind-find-toy`).

Then add it to keybind-find's config (`plugin add` writes the plugin's full path, since Super+/ runs keybind-find
with your window manager's `PATH`, which may not have `~/.cargo/bin`):

```sh
keybind-find plugin add toy
```

or write the stanza yourself, in `~/.config/keybind-find/config.toml`:

```toml
[[plugin]]
name = "toy"
program = "~/.cargo/bin/keybind-find-toy"
inputs = ["~/.config/toy/keys.conf"]
[plugin.match]
program = ["toy"]
```

### 3. Converting your config (optional)

*(Part (b), only if the keymap can't be read as it is: a conversion script that shows a diff, writes only after
the user confirms, backs up the original, and does nothing the second time. Say what it changes and why. This
template needs none: delete this section, or replace it.)*

## For the plugin's author

- **Test:** `cargo test` checks the sample (`tests/fixtures/keys.conf`) against the sheet it should give
  (`tests/fixtures/expected.json`), with `keybind_find_sheet::testing::assert_fixture`.
- **Check it as keybind-find will:** `keybind-find check-plugin ./target/debug/keybind-find-toy --input
  tests/fixtures/keys.conf
  --program toy` runs it the way Super+/ does and prints one line per problem; it exits 0 when there are none.
- **Format:** one binding per line, `key = description`, optionally ending with ` ; scope=value` (spaces around
  the `;`; the name and value are printable ASCII words without `,`, `=`, `[` or `]`). Blank lines and
  `#` comments are skipped.
