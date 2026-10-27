# rofi frontend for keybind-find

`keybind-find-rofi` shows keybind-find's list in [rofi](https://github.com/davatorium/rofi): the key, the
description and the program in columns, with the scope tags (`[mode=insert]`) dimmed. It is a JSON picker frontend
(protocol version 1, see [docs/picker.md](../../../docs/picker.md#json-frontends)), in Python with the standard
library only.

Install it somewhere on your `PATH` (or anywhere, and give its full path below; `~` isn't expanded), then in
`~/.config/keybind-find/config.toml`:

```toml
[picker]
kind = "json"
command = ["keybind-find-rofi"]
```

Super+/ then opens rofi. Escape cancels. rofi's own errors (no rofi, a crash, any failure but Escape's exit 1) reach keybind-find's stderr.
On Wayland, use a rofi that runs there (rofi 2.0, or the rofi-wayland fork). The columns line up in a monospace
font: set one in your rofi theme. Ranking, scopes and what is sent are keybind-find's, as with fzf; rofi
has no key search (see the docs for capturing keys with `keybind-find keys`).

The test feeds a sample request through it with a stub `rofi` on `PATH`:

```sh
python3 test_rofi.py
```
