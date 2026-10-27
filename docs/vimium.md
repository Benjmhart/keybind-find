# Vimium

In a browser window, keybind-find lists [Vimium](https://github.com/philc/vimium)'s keys when Vimium is installed, or
when its backup (below) is saved. Installed means: from the Chrome Web Store in Vivaldi, Chrome, Chromium, Brave or Edge under `~/.config`, or in Firefox under `~/.mozilla`. A
disabled Vimium still counts. The defaults come from
Vimium's own source for the installed version, built into keybind-find. Vimium keeps your remaps in the browser's
extension storage, which keybind-find can't read. To list them, save a backup: Vimium → Options → Backup →
"Download backup", then move the file to `~/.config/keybind-find/vimium-options.json` (under `$XDG_CONFIG_HOME` if
set). Save it again after you change your remaps. Its `map`, `unmap` and `unmapAll` lines are applied, and a key
Vimium itself would never reach (`dd` while `d` is mapped) is left out. `mapkey` lines and maps to unknown commands
are skipped, and so are keys keybind-find can't send (`<m-…>`) and spellings Vimium never fires (`<esc>`, `<cr>`,
`<c-s-x>`: Vimium writes these `<escape>`, `<enter>`, `<c-X>`), with a line on stderr for each kind. A `mapkey` changes
which key Vimium sees, so with one the list can be off. Vimium found in any of those browsers counts for every
browser. Vimium's per-site exclusion rules and pass keys aren't applied, so a key Vimium leaves alone on some site is
still listed. Like a web app's keys, Vimium's work only while focus isn't in a text field.
