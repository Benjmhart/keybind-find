# Sheets

For a program keybind-find can't read, put a sheet in `~/.config/keybind-find/sheets/` (under `$XDG_CONFIG_HOME` if
set). A `.json` file in the DMS format (see [Format](#format)) applies when its `provider` is the
focused window's class, ignoring case (`xprop WM_CLASS`, the second name; under sway the `app_id`, or the X class
for an XWayland window). An executable
named after the class (`st-256color` or `st-256color.py`) is run, and what it prints is read as a sheet. It needs a `#!` line, must be your own and not writable by
group or others, and all parsers share one time limit; backups (`.bak`, `~`) are never run. Everything
that matches is merged, in file name order.

**Web apps.** A sheet with a `window_title` applies only while the window's title contains it, ignoring case, so pick
text only that app's tabs have (`"- Gmail"` rather than `"mail"`). `"provider": "browser"` matches Firefox, Chromium,
Chrome, Brave, Vivaldi, Edge, Opera, LibreWolf and Zen, including their other builds (beta, nightly, ESR), snaps and
flatpaks, so one sheet covers them all. For any other browser, use its class as `provider`.

```json
{"title": "Gmail", "provider": "browser", "window_title": "- Gmail",
 "binds": {"mail": [{"key": "c", "desc": "Compose"}, {"key": "g i", "desc": "Go to Inbox"}]}}
```

A web app's keys work only while focus isn't in a text field, and Gmail's only with Settings → "Keyboard shortcuts
on".

**Built-in sheets.** keybind-find ships sheets for Gmail (`gmail.json`, for tabs titled `... - Gmail`; it assumes
"Keyboard shortcuts on"), YouTube (`youtube.json`, `... - YouTube`) and GitHub (`github.json`, `github.com`), in any
browser above. Their source is [`data/sheets/`](../data/sheets/), and they merge after your own. Gmail and YouTube need
no setup. GitHub needs a browser extension that adds the page's address to the window title (search your browser's
add-on store for "URL in title"): GitHub's tab titles don't reliably name it (signed in, a repo tab is just
`owner/repo: description`). The match is plain text, so it can be loose:
- GitHub's sheet applies to any title containing `github.com`, such as `docs.github.com` or a search for it;
- `- YouTube` also matches YouTube Music, Studio and Help tabs, and `- Gmail` Gmail Help pages;
- the YouTube home tab (`YouTube`) gets none (its player keys don't apply there);
- a Google Workspace inbox whose title names your organisation instead of Gmail gets none: copy `gmail.json` into
  your sheets folder with your inbox's title part as `window_title`.

To replace one, put a file with the same name in your sheets
folder (start from the built-in copy). It replaces the built-in sheet even if it fails to load, so an empty sheet
(`{"title": "GitHub", "provider": "browser", "binds": {}}`) turns that one off. A sheet you wrote for one of these
apps under another name (`gh.json`) adds to the built-in one, so its keys show twice: rename it to replace it.

## Format

Sheets use the [DMS cheatsheet format](https://github.com/duxlabs-dev/dux-shell)
(`{title, provider, binds: {category: [{key, desc, action}]}}`). Keys are written like xdotool chords: modifiers in
the order `super`, `ctrl`, `alt`, `shift`, joined with `+` to an X keysym name (`super+shift+Return`); a sequence is
chords separated by spaces (`ctrl+b t`). Write symbols by their keysym name (`question`, `slash`, `less`): a
literal `?` or `/` is listed but refused when picked. An espanso key is its trigger text.

**Scopes.** A binding that applies only somewhere carries `when`: scope names mapped to the value it applies in, or
a list of values (`"when": {"mode": "insert", "filetype": ["rust", "toml"]}`). Left out (or `{}`), it applies
everywhere. Names and values are printable ASCII without spaces, `,`, `=`, `[` or `]`; a scope is given once, and its
list is not empty and doesn't repeat a value. A sheet that breaks this is refused with the reason. Report every
binding with its scope, and never filter by guessing where the user is: keybind-find decides that from what it knows,
its *facts* (such as nvim's `mode`). It hides a binding only when one of its scopes is a fact whose value isn't among
the scope's values. Every other scope stays visible as a tag after the description, `[mode=insert]` or
`[filetype=rust,toml]`, and a scope that matches a fact isn't tagged. A scope keybind-find has no fact for never hides
anything. Today the only facts come from nvim: when Super+/ is pressed over a running nvim, keybind-find asks it
for its `mode` (`normal`, `visual`, `insert`, `o` for operator-pending, `c` for the command line, `t` for the
terminal) and the buffer's `filetype`. The current buffer's own maps carry `buffer=current`, which stays a tag. With
no running nvim (`keybind-find dump nvim` from a shell), there are no facts and every scope shows as a tag.

```json
{"key": "ctrl+r", "desc": "Insert from register", "when": {"mode": "insert"}}
```
