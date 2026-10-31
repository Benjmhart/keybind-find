# Contributing

Bug reports, sheets and providers are all welcome. Open pull requests against `staging`.

## Setup

Tool versions (Rust, and the nvim the tests run against) are pinned with [mise](https://mise.jdx.dev):

```sh
mise install
mise run ci      # cargo fmt --check, clippy -D warnings, cargo test
```

`mise run ci` is what CI runs on every pull request; run it before you push. Bare `cargo test` tests against
whatever nvim and Rust are on your `PATH`.

## Code rules

- Errors are `anyhow::Result` with a context string saying what was being done (`.context("reading herdr's
  config.toml")`). No custom error enums.
- A provider that fails is one line on stderr and is left out of the list; the other providers still show. Never
  crash, `unwrap`, `expect` or `panic!` outside tests.
- Run other programs only through `std::process::Command` with explicit arguments. Never pass config text through a
  shell.
- The dependencies are `serde`, `serde_json`, `toml`, `anyhow` and `x11rb`. Open an issue to discuss a new crate
  before adding one.

## The easiest contribution: a built-in sheet

A web app's keys are a JSON sheet in [`data/sheets/`](data/sheets/), in the format described in
[docs/sheets.md](docs/sheets.md#format). Copy [`data/sheets/gmail.json`](data/sheets/gmail.json), set `provider` to
`"browser"` and `window_title` to text only that app's tabs have, and list the keys from the app's own help page. Add
the file to `BUILTIN` in [`src/providers/sheets.rs`](src/providers/sheets.rs); its tests load every built-in sheet.

You can try a sheet without building anything: put it in `~/.config/keybind-find/sheets/`, run
`sleep 3; keybind-find dump sheets` in a terminal and focus the app within three seconds.

## Adding a provider

A provider reads one program's real keymap and returns it as a sheet. [`src/providers/tmux.rs`](src/providers/tmux.rs)
(a running program asked for its keys) and [`src/providers/herdr.rs`](src/providers/herdr.rs) (built-in defaults plus
the user's config) are good models.

1. **The sheet model** is in [`src/sheet.rs`](src/sheet.rs): a `Sheet` has a `title`, a `provider` name and `binds`,
   categories of `Bind { key, desc, action }`. Build each key with `sheet::chord` so it is in keybind-find's key
   notation (modifiers `super`, `ctrl`, `alt`, `shift`, then an X keysym name such as `slash`, never `/`; a sequence
   is chords separated by spaces). A key with other characters is listed but refused when picked.
2. **The module:** add `src/providers/<name>.rs` with `pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet>`. Keep
   the parsing in a function that takes text, so tests can feed it fixtures without the program installed.
3. **Register it** in [`src/providers/mod.rs`](src/providers/mod.rs): `mod <name>;`, its name in `KNOWN`, and an arm
   in `providers::sheet`. `keybind-find dump <name>` works from then on.
4. **When it applies:** add it in `providers_for` in [`src/context.rs`](src/context.rs), keyed on the focused window's
   class, or on the program running inside a terminal (any class in `TERMINALS` there). A program found
   inside a herdr or tmux pane is added in `add_nested_nvim` there. `providers_for` lists programs from the outside
   in: the window manager and espanso, then the app and what runs inside it, with `vimium` and `sheets` last; put a
   new app provider inside the class block, before those two. The ranking in [`src/usage.rs`](src/usage.rs) gives the
   innermost program the first turn and the window manager the second; a new window manager provider goes in its
   `"xmonad" | "sway"` match. A provider whose entries are text to type rather than keys to press needs the same
   special case as espanso in [`src/run.rs`](src/run.rs).
5. **Defaults the program doesn't print:** if the program's built-in keys can only be read from its source, pin them
   per version in `data/<name>/<version>.toml`, as [`data/herdr/`](data/herdr/) and [`data/vimium/`](data/vimium/)
   do, and build them in with `include_str!` in a registry (`REGISTRY` in `herdr.rs`). Say in the file which source
   file and version they came from.
6. **Tests:** put real config or output from the program in `tests/fixtures/<name>/` (or one file,
   `tests/fixtures/<name>.<ext>`) and test the parser against it
   in a `#[cfg(test)] mod tests` at the bottom of the module, as the other providers do. Parser tests must pass on a machine
   without the program.
7. **Docs:** add the program to the "Works with" table in [README.md](README.md), the `dump` provider list in
   [`man/keybind-find.1`](man/keybind-find.1) (and its DESCRIPTION) and the bug report template, a line to
   [CHANGELOG.md](CHANGELOG.md), and, if it runs a new program or reads a new file, the lists in
   [SECURITY.md](SECURITY.md).

A provider can also live outside this repo, as a plugin: a small program of its own that prints a sheet. See
[docs/plugins.md](docs/plugins.md). A program few people use, or one you'd rather maintain yourself, is a good fit
for a plugin.

## Support level

Every provider in `src/providers/` is built in, but they are not all tested the same way.

- **Used daily by the maintainer:** xmonad, sway, alacritty, nvim, herdr, tmux, bash and zsh, espanso, less, and the
  browser sheets and Vimium. Their behaviour has been checked against the real programs in daily use.
- **Best-effort (everything else):** helix, vim, Emacs, nano, micro, mc, ranger, ncmpcpp, yazi, zathura, mpv,
  Obsidian, VS Code, Zed, kitty, foot, Ghostty, GNOME Terminal, Xfce Terminal, screen, fish, i3, Hyprland, KDE
  Plasma, Xfce, GNOME and the suckless sheets. Each was written from the program's own source and documentation at a
  pinned version, and is covered by unit tests on fixtures, plus a test against the real program where one is
  installed. None is used day to day, so treat it as a starting point: check its behaviour against the real program
  before relying on it or building on it, and expect the program's newer versions to differ.

To check them against the real programs, run `tests/e2e/run` (it needs only Docker, and about 9 GB of disk for
the image; the first build downloads it). It builds an Arch image pinned to one date (`tests/e2e/Dockerfile`: every
package from the Arch Linux Archive's snapshot), builds keybind-find inside from your checkout, starts each program
for real in a virtual display (Xvfb with i3, and a headless sway) with no network, and checks that `keybind-find dump`
lists the expected providers, each with binds, and a remapped key from a synthetic config (`tests/e2e/home/`).
`tests/e2e/run helix vim-live` runs only the cases named; `E2E_OUT=dir` keeps each case's dump and stderr. Nothing is
installed on your machine and nothing shows on your screen. Run it after changing a best-effort provider; every release
runs it before tagging. The cases are in `tests/e2e/cases`. To test newer programs, move `SNAPSHOT` (and `BASE`) in
the Dockerfile to a later date.

If you use one of the best-effort programs, a bug report or a fix from real use is especially welcome.

## Reporting a bug

Use the bug report template. The two most useful things in it are the output of `keybind-find dump <provider>` for
the provider that is wrong, and the stderr lines. Run them from a terminal with a delay, so the window in question
has focus: `sleep 3; keybind-find dump <provider>` for the sheet and `sleep 3; keybind-find dump > /dev/null` for the
stderr lines, focusing that window within the three seconds.
