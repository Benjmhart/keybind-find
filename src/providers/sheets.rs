//! Static sheets and custom parsers in `$XDG_CONFIG_HOME/keybind-find/sheets/` (default `~/.config/...`), matched to
//! the focused window's class (the class part of `xprop WM_CLASS`, e.g. `Vivaldi-stable`; under sway the `app_id`),
//! ignoring case:
//! - `*.json`: a hand-written DMS cheatsheet, matched by its `provider` field (`"provider": "Vivaldi-stable"`).
//!   `"provider": "browser"` matches any browser in `BROWSERS`. A sheet with a `window_title` (a web app's) applies
//!   only while the focused window's title contains it, ignoring case.
//! - any other executable file: a custom parser, matched by its file name with or without one extension
//!   (`st-256color` or `st-256color.py` for class `st-256color`; not backups like `.bak` or `.py~`). It needs a `#!`
//!   line, must be the user's own and not writable by group or others. It is run with no arguments and no shell, and
//!   what it prints is read as a DMS cheatsheet. Non-matching parsers are never run; to disable one, remove its
//!   execute bit. All parsers share one time limit.
//!
//! All matching sheets merge into one, in file name order, then the built-in web-app sheets (`BUILTIN`, which apply
//! with no folder at all), then the browser's own keys (`BROWSER_SHEETS`, Firefox's or Chromium's by `family`) when no
//! web-app sheet (a `.json` sheet with a `window_title`, built in or the user's) applied, and not in a Chromium app
//! window. dwm's keys (`WM_SHEETS`) come next in a dwm session, then st's, surf's and tabbed's (`APP_SHEETS`) by class.
//! A file in the folder with a built-in sheet's name replaces it,
//! even when it fails. A file that fails is one stderr line; only an unreadable directory fails the provider.

use crate::context::{run_for, Context};
use crate::sheet::Sheet;
use anyhow::Context as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

// ponytail: one fixed limit; make it configurable if a real parser needs longer.
const PARSER_TIMEOUT: Duration = Duration::from_secs(3);

/// What `"provider": "browser"` matches, ignoring case: a class that is one of these, or starts with one followed
/// by `-`, `_`, `.` or a space. That covers builds (`Vivaldi-stable`, `firefox-esr`, `Google-chrome-beta`), snaps
/// (`firefox_firefox`) and Chromium app windows under sway (`chrome-<id>-Default`, and in a flatpak
/// `com.google.Chrome.flextop.chrome-<id>-Default`).
/// ponytail: grows by a line per browser.
const BROWSERS: &[&str] = &[
    "firefox",
    "firefoxdeveloperedition",
    "librewolf",
    "zen",
    "chromium",
    "chrome",
    "google-chrome",
    "brave",
    "brave-browser",
    "vivaldi",
    "microsoft-edge",
    "msedge",
    "opera",
    // Flatpak app ids, under sway and as X classes.
    "org.mozilla.firefox",
    "org.chromium.chromium",
    "com.google.chrome",
    "com.brave.browser",
    "com.vivaldi.vivaldi",
    "com.microsoft.edge",
    "com.opera.opera",
    "io.gitlab.librewolf-community",
    "app.zen_browser.zen",
];

/// Web-app sheets built into the binary, by file name, matched like `.json` files in the folder.
const BUILTIN: &[(&str, &str)] = &[
    (
        "github.json",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/sheets/github.json"
        )),
    ),
    (
        "gmail.json",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/sheets/gmail.json"
        )),
    ),
    (
        "youtube.json",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/sheets/youtube.json"
        )),
    ),
];

/// The browsers' own keys, built in, by family (`family`): the file name a user sheet replaces it with, and the text.
const BROWSER_SHEETS: &[(&str, &str, &str)] = &[
    (
        "firefox",
        "firefox.json",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/sheets/firefox.json"
        )),
    ),
    (
        "chromium",
        "chromium.json",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/sheets/chromium.json"
        )),
    ),
];

/// suckless programs' stock keys, built in (each their `config.def.h` at a release): the window classes (ignoring case),
/// the file name a user sheet replaces it with, and the text. A patched `config.h` isn't read.
const APP_SHEETS: &[(&[&str], &str, &str)] = &[
    (
        &["st-256color", "St"],
        "st.json",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/sheets/st.json")),
    ),
    (
        &["Surf"],
        "surf.json",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/sheets/surf.json"
        )),
    ),
    (
        &["tabbed"],
        "tabbed.json",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/sheets/tabbed.json"
        )),
    ),
];

/// Window managers' stock keys, built in, by session (`context::window().wm`), whatever window has focus.
const WM_SHEETS: &[(&str, &str, &str)] = &[(
    "dwm",
    "dwm.json",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/sheets/dwm.json")),
)];

/// The Firefox family, by `BROWSERS`' prefix rule: Firefox and its builds, LibreWolf, Zen, and their Flatpak ids.
const FIREFOX: &[&str] = &[
    "firefox",
    "firefoxdeveloperedition",
    "librewolf",
    "zen",
    "org.mozilla.firefox",
    "io.gitlab.librewolf-community",
    "app.zen_browser.zen",
];

/// The Chromium family: Chromium, Chrome, Brave, Edge, and their Flatpak ids. Not Vivaldi or Opera (own keymaps).
const CHROMIUM: &[&str] = &[
    "chromium",
    "chrome",
    "google-chrome",
    "brave",
    "brave-browser",
    "microsoft-edge",
    "msedge",
    "org.chromium.chromium",
    "com.google.chrome",
    "com.brave.browser",
    "com.microsoft.edge",
];

/// Whether `class` (lower case) is one of `names`, or starts with one followed by `-`, `_`, `.` or a space.
fn named(class: &str, names: &[&str]) -> bool {
    names.iter().any(|b| {
        class
            .strip_prefix(b)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '_', '.', ' ']))
    })
}

pub(crate) fn is_browser(class: &str) -> bool {
    named(&class.to_ascii_lowercase(), BROWSERS)
}

/// The browser family whose own keys apply to a window of `class` (X11 `instance`): `firefox`, `chromium`, or none
/// (another browser, not a browser, or a Chromium app/PWA window, which has no tabs or address bar).
fn family(class: &str, instance: Option<&str>) -> Option<&'static str> {
    let class = class.to_ascii_lowercase();
    if is_app_window(&class) {
        None
    } else if named(&class, FIREFOX) {
        Some("firefox")
    } else if named(&class, CHROMIUM) && !instance.is_some_and(is_app_instance) {
        Some("chromium")
    } else {
        None
    }
}

/// Chromium's app id: 32 letters `a`-`p`.
fn is_app_id(id: &[u8]) -> bool {
    id.len() == 32 && id.iter().all(|b| (b'a'..=b'p').contains(b))
}

/// A Chromium app window's X11 `WM_CLASS` instance: `crx_<id>` (Chromium's app name `_crx_<id>` with the `_`
/// trimmed: `GetWMClassFromAppName`, `chrome/browser/shell_integration_linux.cc` at 149.0.7827.238), also
/// `crx__<id>` (an Edge store app's `_<id>`, unverified), ignoring case.
fn is_app_instance(instance: &str) -> bool {
    let instance = instance.to_ascii_lowercase();
    instance
        .strip_prefix("crx_")
        .map(|id| id.strip_prefix('_').unwrap_or(id))
        .is_some_and(|id| is_app_id(id.as_bytes()))
}

/// A Chromium app window's class (lower case), as Wayland compositors see it: `chrome-<id>-<profile>`, `brave-…` or
/// `msedge-…` (Edge's store apps: `msedge-_<id>-…`), also inside a Flatpak's `….flextop.chrome-…`; the id is 32
/// letters `a`-`p`. Under X11 an app window has the browser's own class; its instance tells it apart
/// (`is_app_instance`).
fn is_app_window(class: &str) -> bool {
    ["chrome-", "brave-", "msedge-"].iter().any(|p| {
        class.match_indices(p).any(|(i, _)| {
            let id = &class[i + p.len()..];
            let id = id.strip_prefix('_').unwrap_or(id);
            id.len() > 32
                && is_app_id(&id.as_bytes()[..32])
                && id.as_bytes()[32] == b'-'
                && (i == 0 || class[..i].ends_with('.'))
        })
    })
}

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    // No folder (no $HOME): "" reads as missing, so only the built-in sheets apply.
    let dir = sheets_dir(&env).unwrap_or_default();
    parse(&dir, ctx, crate::context::window().wm.as_deref())
}

/// `$XDG_CONFIG_HOME/keybind-find/sheets` (absolute only, as the XDG spec says), else under `$HOME/.config`.
fn sheets_dir(env: Env) -> Option<PathBuf> {
    let base = match env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => dir,
        None => PathBuf::from(env("HOME")?).join(".config"),
    };
    Some(base.join("keybind-find").join("sheets"))
}

fn empty() -> Sheet {
    Sheet {
        title: "static sheets".into(),
        provider: "sheets".into(),
        binds: Default::default(),
    }
}

/// The merged sheet for the window in `ctx` (its class, instance and title); each file that failed is one stderr line.
/// `wm` is the session (`context::window().wm`), for `WM_SHEETS`.
fn parse(dir: &Path, ctx: &Context, wm: Option<&str>) -> anyhow::Result<Sheet> {
    let (sheet, notes) = parse_reporting(
        dir,
        ctx.class.as_deref(),
        ctx.instance.as_deref(),
        ctx.title.as_deref(),
        wm,
        PARSER_TIMEOUT,
    )?;
    for note in notes {
        eprintln!("keybind-find: sheets: {note}");
    }
    Ok(sheet)
}

/// The merged sheet, plus a note per file skipped. A missing directory has only the built-in sheets.
fn parse_reporting(
    dir: &Path,
    class: Option<&str>,
    instance: Option<&str>,
    title: Option<&str>,
    wm: Option<&str>,
    timeout: Duration,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut sheet = empty();
    let mut notes = Vec::new();
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    paths.sort();
    // Parsers run only from a folder nobody else can add to (a planted file or symlink can't be checked later).
    let parsers_allowed = if paths.is_empty() {
        Ok(())
    } else {
        trusted(&std::fs::metadata(dir).context("reading the sheets folder")?)
            .map_err(|why| format!("{}: parsers not run: {why}", dir.display()))
    };
    // The user's sheet files, which replace built-in sheets of the same name.
    let mut user_sheets = Vec::new();
    // Whether a web-app sheet (one with a `window_title`) applied: then the browser's own keys are left out.
    let mut web_app = false;
    let deadline = std::time::Instant::now() + timeout;
    for path in paths {
        let json = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        // Follows symlinks. Anything that isn't a regular file is ignored, but a sheet that can't be reached says so.
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) if json => {
                notes.push(format!("skipping {}: {e}", path.display()));
                user_sheets.extend(path.file_name().map(|n| n.to_string_lossy().into_owned()));
                continue;
            }
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        if json {
            user_sheets.extend(path.file_name().map(|n| n.to_string_lossy().into_owned()));
        }
        let found = if json {
            static_sheet(&path, class, title)
        } else if meta.permissions().mode() & 0o111 != 0 && parser_matches(&path, class) {
            match &parsers_allowed {
                Ok(()) => run_parser(&path, &meta, deadline).map(|o| o.map(|s| (s, false))),
                Err(why) => {
                    if !notes.contains(why) {
                        notes.push(why.clone());
                    }
                    continue;
                }
            }
        } else {
            Ok(None)
        };
        web_app |= matches!(found, Ok(Some((_, true))));
        merge(&mut sheet, &mut notes, &path.display().to_string(), found);
    }
    let replaced = |name: &str| user_sheets.iter().any(|u| u.eq_ignore_ascii_case(name));
    for (name, text) in BUILTIN {
        if !replaced(name) {
            let found = sheet_from(text, class, title);
            web_app |= matches!(found, Ok(Some((_, true))));
            merge(&mut sheet, &mut notes, &format!("built-in {name}"), found);
        }
    }
    let built_in = |text: &str| {
        serde_json::from_str(text)
            .context("not a DMS cheatsheet")
            .map(|s| Some((s, false)))
    };
    for (session, name, text) in WM_SHEETS {
        if wm.is_some_and(|w| w.eq_ignore_ascii_case(session)) && !replaced(name) {
            merge(
                &mut sheet,
                &mut notes,
                &format!("built-in {name}"),
                built_in(text),
            );
        }
    }
    for (classes, name, text) in APP_SHEETS {
        if class.is_some_and(|c| classes.iter().any(|k| k.eq_ignore_ascii_case(c)))
            && !replaced(name)
        {
            merge(
                &mut sheet,
                &mut notes,
                &format!("built-in {name}"),
                built_in(text),
            );
        }
    }
    let browser = class.and_then(|c| family(c, instance));
    for (fam, name, text) in BROWSER_SHEETS {
        if browser == Some(*fam) && !web_app && !replaced(name) {
            merge(
                &mut sheet,
                &mut notes,
                &format!("built-in {name}"),
                built_in(text),
            );
        }
    }
    Ok((sheet, notes))
}

/// Adds a found sheet's binds to `sheet`, or a one-line note naming `what` failed.
fn merge(
    sheet: &mut Sheet,
    notes: &mut Vec<String>,
    what: &str,
    found: anyhow::Result<Option<(Sheet, bool)>>,
) {
    match found {
        Ok(Some((s, _))) => {
            for (category, binds) in s.binds {
                sheet.binds.entry(category).or_default().extend(binds);
            }
        }
        Ok(None) => {}
        Err(e) => notes.push(format!(
            "skipping {what}: {}",
            format!("{e:#}")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        )),
    }
}

/// The sheet in a `.json` file if it applies to the window: its `provider` is `class` (or `browser` and `class` is a
/// browser), and its `window_title`, if any, is in `title`. Every file is read (a broken one is reported whatever
/// window has focus). `window_title` is read beside the DMS fields, so it never reaches the merged sheet. With the
/// sheet, whether it applied by a `window_title` (a web app's).
fn static_sheet(
    path: &Path,
    class: Option<&str>,
    title: Option<&str>,
) -> anyhow::Result<Option<(Sheet, bool)>> {
    let text = std::fs::read_to_string(path).context("can't read it")?;
    sheet_from(&text, class, title)
}

/// `static_sheet` for a sheet's text.
fn sheet_from(
    text: &str,
    class: Option<&str>,
    title: Option<&str>,
) -> anyhow::Result<Option<(Sheet, bool)>> {
    let value: serde_json::Value = serde_json::from_str(text).context("not a DMS cheatsheet")?;
    let window_title = match &value["window_title"] {
        serde_json::Value::Null => None,
        serde_json::Value::String(t) => Some(t.clone()),
        _ => anyhow::bail!("its window_title is not text"),
    };
    let s: Sheet = serde_json::from_value(value).context("not a DMS cheatsheet")?;
    let by_title = window_title
        .as_deref()
        .is_some_and(|w| !w.trim().is_empty());
    Ok(applies(&s.provider, window_title.as_deref(), class, title).then_some((s, by_title)))
}

/// Whether a sheet for `provider` (and, if set, a window title containing `window_title`) applies to the window.
fn applies(
    provider: &str,
    window_title: Option<&str>,
    class: Option<&str>,
    title: Option<&str>,
) -> bool {
    let Some(class) = class else { return false };
    let class_matches = if provider.eq_ignore_ascii_case("browser") {
        is_browser(class)
    } else {
        provider.eq_ignore_ascii_case(class)
    };
    class_matches
        && match window_title.map(str::trim).filter(|w| !w.is_empty()) {
            None => true,
            Some(w) => title.is_some_and(|t| t.to_lowercase().contains(&w.to_lowercase())),
        }
}

/// Runs a parser that is the user's own and writable only by them, within what's left of `deadline`.
fn run_parser(
    path: &Path,
    meta: &std::fs::Metadata,
    deadline: std::time::Instant,
) -> anyhow::Result<Option<Sheet>> {
    if let Err(why) = trusted(meta) {
        anyhow::bail!("not run: {why}");
    }
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        anyhow::bail!("not run: earlier parsers used up the time limit");
    }
    let out = run_for(Command::new(path), left)?;
    let sheet = serde_json::from_str(&out).context("its output is not a DMS cheatsheet")?;
    Ok(Some(sheet))
}

/// Ours, and writable only by us. Our uid without libc: the owner of our own /proc entry.
fn trusted(meta: &std::fs::Metadata) -> Result<(), String> {
    let me = std::fs::metadata("/proc/self")
        .map_err(|e| format!("can't find our user id ({e})"))?
        .uid();
    trust(meta.uid(), meta.mode(), me)
}

fn trust(owner: u32, mode: u32, me: u32) -> Result<(), String> {
    if owner != me {
        return Err("owned by another user".into());
    }
    if mode & 0o022 != 0 {
        return Err("writable by group or others".into());
    }
    Ok(())
}

/// Backup and disabled copies keep the class in their stem but aren't parsers.
const NOT_PARSERS: &[&str] = &[
    "bak", "old", "orig", "disabled", "swp", "rej", "save", "tmp", "new", "dist", "prev", "copy",
];

/// `st-256color` or `st-256color.py` for class `st-256color`, ignoring case. Not `st-256color.py~`, `.bak`, ...
fn parser_matches(path: &Path, class: Option<&str>) -> bool {
    let Some(class) = class else { return false };
    let named = |p: Option<&std::ffi::OsStr>| {
        p.is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(class))
    };
    let script_extension = path.extension().is_some_and(|e| {
        let e = e.to_string_lossy();
        e.chars().all(|c| c.is_ascii_alphanumeric())
            && !e.chars().all(|c| c.is_ascii_digit())
            && !NOT_PARSERS.iter().any(|n| e.eq_ignore_ascii_case(n))
    });
    named(path.file_name()) || (script_extension && named(path.file_stem()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture folder, with the modes the parser checks want whatever the checkout's umask was (chmod opens
    /// nothing for writing, so it can't race a parallel exec).
    fn fixtures() -> PathBuf {
        let dir = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sheets"
        ));
        let mode =
            |p: &Path, m| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
        mode(&dir, 0o755);
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let m = e.metadata().unwrap().permissions().mode();
            mode(&e.path(), if m & 0o111 != 0 { 0o755 } else { 0o644 });
        }
        dir
    }

    /// A fresh private temp folder (a leftover from a crashed run with the same pid is cleared first).
    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("keybind-find-sheets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn keys(sheet: &Sheet, category: &str) -> Vec<String> {
        sheet
            .binds
            .get(category)
            .map_or(Vec::new(), |b| b.iter().map(|b| b.key.clone()).collect())
    }

    #[test]
    fn class_picks_its_sheets_and_merges_them_in_file_order() {
        let (sheet, notes) = parse_reporting(
            &fixtures(),
            Some("Vivaldi"),
            None,
            None,
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert_eq!(sheet.provider, "sheets");
        assert_eq!(sheet.title, "static sheets");
        // File name order: vivaldi-extra.json ('-' sorts before '.'), then vivaldi.json.
        assert_eq!(keys(&sheet, "tabs"), ["ctrl+shift+t", "ctrl+t", "ctrl+w"]);
        assert_eq!(keys(&sheet, "panels"), ["ctrl+shift+b"]);
        // No GIMP binds; the broken file is the only note, and it names the file.
        assert!(!sheet.binds.contains_key("tools"));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("broken.json"), "{}", notes[0]);
        // Case doesn't matter.
        let (upper, _) = parse_reporting(
            &fixtures(),
            Some("VIVALDI"),
            None,
            None,
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert_eq!(upper, sheet);
    }

    #[test]
    fn no_class_or_no_match_is_empty() {
        for class in [None, Some("xterm")] {
            let (sheet, notes) =
                parse_reporting(&fixtures(), class, None, None, None, PARSER_TIMEOUT).unwrap();
            assert!(sheet.binds.is_empty(), "{class:?}");
            // Only the broken JSON: the failing parser isn't run for these classes.
            assert_eq!(notes.len(), 1, "{notes:?}");
        }
        let gimp = parse(&fixtures(), &window(Some("gimp"), None), None).unwrap();
        assert_eq!(keys(&gimp, "tools"), ["m", "r"]);
    }

    #[test]
    fn a_missing_directory_is_empty() {
        let (sheet, notes) = parse_reporting(
            Path::new("/nonexistent-kbf/sheets"),
            Some("Vivaldi"),
            None,
            None,
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert!(sheet.binds.is_empty() && notes.is_empty());
    }

    #[test]
    fn a_custom_parser_runs_for_its_class() {
        // Class case doesn't matter for parsers either.
        let (sheet, notes) = parse_reporting(
            &fixtures(),
            Some("ST-256Color"),
            None,
            None,
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        // st-256color.json, then the parser's output (file name order); the .bak copy isn't run. The built-in
        // st.json follows.
        assert_eq!(
            keys(&sheet, "clipboard")[..3],
            ["shift+Insert", "ctrl+shift+c", "ctrl+shift+v"]
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("broken.json"), "{}", notes[0]);
        // A parser that exits non-zero: one note naming it, with its stderr.
        let (sheet, notes) = parse_reporting(
            &fixtures(),
            Some("failing-app"),
            None,
            None,
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert!(sheet.binds.is_empty());
        let failed: Vec<_> = notes
            .iter()
            .filter(|n| n.contains("failing-app.sh"))
            .collect();
        assert_eq!(failed.len(), 1, "{notes:?}");
        assert!(failed[0].contains("no config.h here"), "{}", failed[0]);
    }

    fn parser_notes(class: &str, timeout: Duration) -> Vec<String> {
        let (_, notes) =
            parse_reporting(&fixtures(), Some(class), None, None, None, timeout).unwrap();
        assert!(notes.iter().all(|n| !n.contains('\n')), "{notes:?}");
        notes
            .into_iter()
            .filter(|n| !n.contains("broken.json"))
            .collect()
    }

    /// Whether a process whose command line contains `needle` is running (the fixtures' odd sleep lengths).
    fn running(needle: &str) -> bool {
        let found = || {
            std::fs::read_dir("/proc").unwrap().flatten().any(|e| {
                std::fs::read(e.path().join("cmdline")).is_ok_and(|c| {
                    String::from_utf8_lossy(&c)
                        .replace('\0', " ")
                        .contains(needle)
                })
            })
        };
        // A killed process can linger a moment before it leaves /proc.
        let until = std::time::Instant::now() + Duration::from_secs(1);
        while found() {
            if std::time::Instant::now() > until {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn parsers_that_misbehave_are_skipped() {
        let short = Duration::from_millis(300);
        let notes = parser_notes("garbage", short);
        assert!(
            notes.len() == 1 && notes[0].contains("not a DMS cheatsheet"),
            "{notes:?}"
        );
        // `slow` and `slow.sh` share one time limit: the second isn't started.
        let started = std::time::Instant::now();
        let notes = parser_notes("slow", short);
        assert!(started.elapsed() < short * 3, "{:?}", started.elapsed());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("did not answer"), "{}", notes[0]);
        assert!(notes[1].contains("used up the time limit"), "{}", notes[1]);
        // Exits at once but leaves `sleep` holding stdout: the group is killed, the note says why.
        let started = std::time::Instant::now();
        let notes = parser_notes("forks", short);
        assert!(started.elapsed() < short * 3, "{:?}", started.elapsed());
        assert!(
            notes.len() == 1 && notes[0].contains("left a process holding its output"),
            "{notes:?}"
        );
        // Nothing they started is left running.
        assert!(!running("sleep 4.71") && !running("sleep 4.73"));
        // stdin is closed: a parser reading it gets EOF at once.
        let (sheet, _) =
            parse_reporting(&fixtures(), Some("reads-stdin"), None, None, None, short).unwrap();
        assert_eq!(keys(&sheet, "misc"), ["F1"]);
        // Not executable: never run, though its name matches.
        assert_eq!(parser_notes("plain", short), Vec::<String>::new());
    }

    #[test]
    fn only_the_users_own_parsers_run() {
        let dir = temp("own");
        let p = dir.join("shared.sh");
        // Would fail if run; it's never executed, so no exec-while-writing race.
        std::fs::write(&p, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o775)).unwrap();
        let (_, notes) =
            parse_reporting(&dir, Some("shared"), None, None, None, PARSER_TIMEOUT).unwrap();
        assert!(
            notes.len() == 1 && notes[0].contains("writable by group or others"),
            "{notes:?}"
        );
        // A folder others can write to: no parser runs (one note), but static sheets still load.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            dir.join("shared.json"),
            r#"{"title":"s","provider":"shared","binds":{"x":[{"key":"F2","desc":"d"}]}}"#,
        )
        .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let (sheet, notes) =
            parse_reporting(&dir, Some("shared"), None, None, None, PARSER_TIMEOUT).unwrap();
        assert!(
            notes.len() == 1 && notes[0].contains("parsers not run: writable by group or others"),
            "{notes:?}"
        );
        assert_eq!(keys(&sheet, "x"), ["F2"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn symlinked_sheets_and_dotted_parser_names() {
        let dir = temp("ln");
        std::os::unix::fs::symlink(fixtures().join("gimp.json"), dir.join("gimp.json")).unwrap();
        let (sheet, notes) =
            parse_reporting(&dir, Some("Gimp"), None, None, None, PARSER_TIMEOUT).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(keys(&sheet, "tools"), ["m", "r"]);
        // A parser named after a dotted class, no extension, symlinked into the folder.
        std::os::unix::fs::symlink(fixtures().join("st-256color.sh"), dir.join("org.app.Name"))
            .unwrap();
        let (sheet, notes) =
            parse_reporting(&dir, Some("org.app.name"), None, None, None, PARSER_TIMEOUT).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(keys(&sheet, "clipboard"), ["ctrl+shift+c", "ctrl+shift+v"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn web_app_sheets_match_by_title_in_any_browser() {
        let dir = temp("web");
        let gmail = r#"{"title":"Gmail","provider":"browser","window_title":"gmail","binds":{"mail":[{"key":"c","desc":"Compose"}]}}"#;
        std::fs::write(dir.join("gmail.json"), gmail).unwrap();
        std::fs::write(
            dir.join("vivaldi.json"),
            r#"{"title":"V","provider":"Vivaldi-stable","binds":{"tabs":[{"key":"ctrl+t","desc":"New tab"}]}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("empty-title.json"),
            r#"{"title":"E","provider":"Vivaldi-stable","window_title":"","binds":{"tabs":[{"key":"ctrl+w","desc":"Close"}]}}"#,
        )
        .unwrap();
        let run = |class, title| {
            let (sheet, notes) =
                parse_reporting(&dir, class, None, title, None, PARSER_TIMEOUT).unwrap();
            assert!(notes.is_empty(), "{notes:?}");
            sheet
        };
        let gmail_tab = run(
            Some("Vivaldi-stable"),
            Some("Inbox (3) - you@example.com - Gmail - Vivaldi"),
        );
        assert_eq!(keys(&gmail_tab, "mail"), ["c"]);
        // The plain Vivaldi sheet and the empty window_title (same as none) apply on any tab.
        assert_eq!(keys(&gmail_tab, "tabs"), ["ctrl+w", "ctrl+t"]);
        let other_tab = run(Some("Vivaldi-stable"), Some("GitHub - Vivaldi"));
        assert!(!other_tab.binds.contains_key("mail"));
        assert_eq!(keys(&other_tab, "tabs"), ["ctrl+w", "ctrl+t"]);
        // Any browser, any case; not a terminal titled Gmail; never without a title.
        assert_eq!(
            keys(
                &run(Some("firefox"), Some("Gmail — Mozilla Firefox")),
                "mail"
            ),
            ["c"]
        );
        assert_eq!(
            keys(&run(Some("Google-chrome"), Some("GMAIL")), "mail"),
            ["c"]
        );
        assert!(run(Some("Alacritty"), Some("Gmail")).binds.is_empty());
        assert!(!run(Some("Vivaldi-stable"), None).binds.contains_key("mail"));
        assert!(run(None, Some("Gmail")).binds.is_empty());
        // window_title stays out of what dump prints.
        let json = serde_json::to_string(&gmail_tab).unwrap();
        assert!(!json.contains("window_title"), "{json}");
        // A window_title that isn't text is reported, like any broken sheet.
        std::fs::write(
            dir.join("bad.json"),
            r#"{"title":"B","provider":"browser","window_title":3,"binds":{}}"#,
        )
        .unwrap();
        let (_, notes) =
            parse_reporting(&dir, Some("firefox"), None, None, None, PARSER_TIMEOUT).unwrap();
        assert!(
            notes.len() == 1 && notes[0].contains("window_title"),
            "{notes:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn browser_families() {
        let app = "abcdefghijklmnopabcdefghijklmnop";
        for (class, want) in [
            ("firefox", Some("firefox")),
            ("Firefox", Some("firefox")),
            ("firefox-esr", Some("firefox")),
            ("firefox-nightly", Some("firefox")),
            ("firefoxdeveloperedition", Some("firefox")),
            ("firefox_firefox", Some("firefox")),
            ("org.mozilla.firefox", Some("firefox")),
            ("librewolf", Some("firefox")),
            ("io.gitlab.librewolf-community", Some("firefox")),
            ("zen", Some("firefox")),
            ("zen-browser", Some("firefox")),
            ("app.zen_browser.zen", Some("firefox")),
            ("chromium", Some("chromium")),
            ("Chromium-browser", Some("chromium")),
            ("org.chromium.Chromium", Some("chromium")),
            ("Google-chrome", Some("chromium")),
            ("Google-chrome-beta", Some("chromium")),
            ("com.google.Chrome", Some("chromium")),
            ("brave-browser", Some("chromium")),
            ("Brave-browser-nightly", Some("chromium")),
            ("com.brave.Browser", Some("chromium")),
            ("microsoft-edge", Some("chromium")),
            ("microsoft-edge-beta", Some("chromium")),
            ("com.microsoft.Edge", Some("chromium")),
            // Own keymaps, not browsers, or nothing.
            ("Vivaldi-stable", None),
            ("com.vivaldi.Vivaldi", None),
            ("opera", None),
            ("Opera beta", None),
            ("Alacritty", None),
            ("chromebook-tool", None),
            ("", None),
            // Chromium app and PWA windows: no tabs, no address bar.
            (&format!("chrome-{app}-Default"), None),
            (&format!("chrome-{app}-Profile_1"), None),
            (&format!("msedge-{app}-Default"), None),
            (
                &format!("com.google.Chrome.flextop.chrome-{app}-Default"),
                None,
            ),
            (
                &format!("com.microsoft.Edge.flextop.msedge-{app}-Default"),
                None,
            ),
            (&format!("brave-{app}-Default"), None),
            (
                &format!("com.brave.Browser.flextop.brave-{app}-Default"),
                None,
            ),
            (&format!("msedge-_{app}-Default"), None),
            ("msedge", Some("chromium")),
            // Not an app id: too short, a letter past `p`, no profile after it, or not at the start or after a `.`.
            (&format!("chrome-{app}q-Default"), Some("chromium")),
            (&format!("google-chrome-{app}-Default"), Some("chromium")),
            ("chrome-abcdefghijklmnop-Default", Some("chromium")),
            (&format!("chrome-{}z-Default", &app[..31]), Some("chromium")),
        ] {
            assert_eq!(family(class, None), want, "{class}");
            assert_eq!(
                family(&class.to_uppercase(), None),
                want,
                "{class} in upper case"
            );
            if want.is_some() {
                assert!(is_browser(class), "{class}");
            }
        }
        // Each family's names are browsers.
        for name in FIREFOX.iter().chain(CHROMIUM) {
            assert!(BROWSERS.contains(name), "{name}");
        }
        // Every class `is_browser` knows has the family its name says (none for Vivaldi and Opera).
        for class in BROWSER_CLASSES {
            let lower = class.to_lowercase();
            let want = if lower.contains("vivaldi") || lower.contains("opera") {
                None
            } else if ["fox", "wolf", "zen"].iter().any(|f| lower.contains(f)) {
                Some("firefox")
            } else {
                Some("chromium")
            };
            assert_eq!(family(class, None), want, "{class}");
        }
    }

    /// Which browser sheet a merged sheet holds, by a bind only that sheet has: Firefox goes to tab 1 with alt+1,
    /// Chromium with ctrl+1.
    fn browser_sheet(sheet: &Sheet) -> Option<&'static str> {
        let has = |key: &str| {
            sheet
                .binds
                .get("tabs")
                .is_some_and(|b| b.iter().any(|b| b.key == key && b.desc == "Go to tab 1"))
        };
        match (has("alt+1"), has("ctrl+1")) {
            (true, false) => Some("firefox"),
            (false, true) => Some("chromium"),
            (false, false) => None,
            (true, true) => Some("both"),
        }
    }

    #[test]
    fn x11_app_windows_by_instance() {
        let app = "abcdefghijklmnopabcdefghijklmnop";
        let none = Path::new("/nonexistent-kbf/sheets");
        let run = |class: &str, instance: Option<&str>, title: &str| {
            parse_reporting(
                none,
                Some(class),
                instance,
                Some(title),
                None,
                PARSER_TIMEOUT,
            )
            .unwrap()
            .0
        };
        let crx = format!("crx_{app}");
        for class in [
            "Google-chrome",
            "Brave-browser",
            "Microsoft-edge",
            "Chromium",
        ] {
            // An installed app: no Chromium sheet, in any case, and with Edge's `_<id>` too.
            for instance in [crx.clone(), crx.to_uppercase(), format!("crx__{app}")] {
                assert_eq!(
                    browser_sheet(&run(class, Some(&instance), "x")),
                    None,
                    "{class} {instance}"
                );
            }
            // The browser's own window, no instance, or not an app id: the Chromium sheet.
            for instance in [
                Some(class.to_lowercase()),
                None,
                Some(format!("crx_{}", &app[..31])),
                Some(format!("crx_{}z", &app[..31])),
                Some(format!("crx_{app}a")),
                Some(app.to_string()),
            ] {
                assert_eq!(
                    browser_sheet(&run(class, instance.as_deref(), "x")),
                    Some("chromium"),
                    "{class} {instance:?}"
                );
            }
        }
        // Only the Chromium family is narrowed: Firefox with a `crx_` instance keeps its sheet.
        assert_eq!(
            browser_sheet(&run("firefox", Some(&crx), "x")),
            Some("firefox")
        );
        // A web-app sheet still applies by title on an app window.
        let gmail = run(
            "Google-chrome",
            Some(&crx),
            "Inbox - you@example.com - Gmail",
        );
        assert!(gmail.binds.contains_key("go to"), "Gmail's");
        assert_eq!(browser_sheet(&gmail), None);
        // The window's instance reaches the lookup.
        let chrome =
            |instance| parse(none, &window(Some("Google-chrome"), instance), None).unwrap();
        assert_eq!(browser_sheet(&chrome(Some(&crx))), None);
        assert_eq!(browser_sheet(&chrome(None)), Some("chromium"));
    }

    fn window(class: Option<&str>, instance: Option<&str>) -> Context {
        Context {
            window_id: None,
            class: class.map(str::to_string),
            instance: instance.map(str::to_string),
            title: None,
            providers: vec![],
        }
    }

    #[test]
    fn browser_sheets_merge_unless_a_web_app_applies() {
        let run = |dir: &Path, class: Option<&str>, title: Option<&str>| {
            parse_reporting(dir, class, None, title, None, PARSER_TIMEOUT).unwrap()
        };
        let none = Path::new("/nonexistent-kbf/sheets");
        let (s, notes) = run(none, Some("firefox"), Some("Mozilla Firefox"));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(browser_sheet(&s), Some("firefox"));
        assert!(keys(&s, "tabs").contains(&"ctrl+t".to_string()));
        // No folder at all (no $HOME).
        let (s, notes) = run(
            Path::new(""),
            Some("Google-chrome"),
            Some("New Tab - Google Chrome"),
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(browser_sheet(&s), Some("chromium"));
        // A web-app sheet applies: it alone.
        let (s, _) = run(
            none,
            Some("firefox"),
            Some("Inbox - you@example.com - Gmail - Mozilla Firefox"),
        );
        assert_eq!(browser_sheet(&s), None);
        assert!(s.binds.contains_key("go to"), "Gmail's");
        // On Chromium's family too, and by a loose title match (`github.com` in `docs.github.com`).
        for (class, title) in [
            ("brave-browser", "Inbox - you@example.com - Gmail - Brave"),
            ("Google-chrome", "Actions - docs.github.com - Google Chrome"),
        ] {
            assert_eq!(
                browser_sheet(&run(none, Some(class), Some(title)).0),
                None,
                "{title}"
            );
        }
        // Neither family, an app window, or no class: no browser sheet.
        for class in [
            Some("Vivaldi-stable"),
            Some("chrome-abcdefghijklmnopabcdefghijklmnop-Default"),
            None,
        ] {
            assert_eq!(
                browser_sheet(&run(none, class, Some("x")).0),
                None,
                "{class:?}"
            );
        }
        let dir = temp("browser-merge");
        // A user `browser` sheet with no `window_title` doesn't hide the browser's keys; one with a matching
        // `window_title` does.
        std::fs::write(
            dir.join("mine.json"),
            r#"{"title":"M","provider":"browser","binds":{"mine":[{"key":"F9","desc":"Mine"}]}}"#,
        )
        .unwrap();
        let (s, notes) = run(&dir, Some("firefox"), Some("Mozilla Firefox"));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(keys(&s, "mine"), ["F9"]);
        assert_eq!(browser_sheet(&s), Some("firefox"));
        std::fs::write(
            dir.join("app.json"),
            r#"{"title":"A","provider":"browser","window_title":"my app","binds":{"app":[{"key":"F8","desc":"App"}]}}"#,
        )
        .unwrap();
        let (s, _) = run(&dir, Some("firefox"), Some("My App - Mozilla Firefox"));
        assert_eq!(keys(&s, "app"), ["F8"]);
        assert_eq!(browser_sheet(&s), None);
        // A `window_title` of spaces is no filter: the sheet applies everywhere, like one without, and hides nothing.
        std::fs::write(
            dir.join("app.json"),
            r#"{"title":"A","provider":"browser","window_title":"  ","binds":{"app":[{"key":"F8","desc":"App"}]}}"#,
        )
        .unwrap();
        let (s, _) = run(&dir, Some("firefox"), Some("Mozilla Firefox"));
        assert_eq!(keys(&s, "app"), ["F8"]);
        assert_eq!(browser_sheet(&s), Some("firefox"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_user_file_replaces_a_browser_sheet() {
        let dir = temp("browser-replace");
        let run = || {
            parse_reporting(&dir, Some("firefox"), None, Some("x"), None, PARSER_TIMEOUT).unwrap()
        };
        // Any case; the user's applies by its own `provider`.
        std::fs::write(
            dir.join("Firefox.json"),
            r#"{"title":"F","provider":"firefox","binds":{"mine":[{"key":"F9","desc":"Mine"}]}}"#,
        )
        .unwrap();
        let (s, notes) = run();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.binds.keys().collect::<Vec<_>>(), ["mine"]);
        // A broken one: one note, and the built-in one isn't used instead.
        std::fs::write(dir.join("Firefox.json"), "{").unwrap();
        let (s, notes) = run();
        assert!(s.binds.is_empty());
        assert!(
            notes.len() == 1 && notes[0].contains("Firefox.json"),
            "{notes:?}"
        );
        // Chromium's is untouched by it.
        let (s, _) = parse_reporting(
            &dir,
            Some("chromium"),
            None,
            Some("x"),
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert_eq!(browser_sheet(&s), Some("chromium"));
        // It replaces the built-in sheet for the whole family, but applies only by its own `provider` (README): a
        // LibreWolf window gets neither.
        std::fs::write(
            dir.join("Firefox.json"),
            r#"{"title":"F","provider":"firefox","binds":{"mine":[{"key":"F9","desc":"Mine"}]}}"#,
        )
        .unwrap();
        let (s, notes) = parse_reporting(
            &dir,
            Some("librewolf"),
            None,
            Some("x"),
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert!(notes.is_empty() && s.binds.is_empty(), "{notes:?} {s:?}");
        // The same for Chromium's, broken: one note, no fallback, on Brave too.
        std::fs::write(dir.join("chromium.json"), "{").unwrap();
        for class in ["chromium", "brave-browser"] {
            let (s, notes) =
                parse_reporting(&dir, Some(class), None, Some("x"), None, PARSER_TIMEOUT).unwrap();
            assert!(s.binds.is_empty(), "{class}");
            assert!(
                notes.len() == 1 && notes[0].contains("chromium.json"),
                "{notes:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every browser class `is_browser` must know.
    const BROWSER_CLASSES: &[&str] = &[
        "firefox",
        "firefox-esr",
        "librewolf",
        "zen",
        "zen-browser",
        "chromium",
        "Chromium-browser",
        "Google-chrome",
        "brave-browser",
        "Vivaldi-stable",
        "microsoft-edge",
        "opera",
        // Other builds, snaps, flatpaks; app-window shapes with a short stand-in id (a real one is 32 letters).
        "Google-chrome-beta",
        "Brave-browser-nightly",
        "Vivaldi-snapshot",
        "firefox-nightly",
        "firefoxdeveloperedition",
        "firefox_firefox",
        "org.mozilla.firefox",
        "org.chromium.Chromium",
        "com.brave.Browser",
        "chrome-abcdefghijklmnop-Default",
        "msedge-abcdefghijklmnop-Default",
        "com.google.Chrome.flextop.chrome-abcdefghijklmnop-Default",
        "com.google.Chrome",
        "com.vivaldi.Vivaldi",
        "com.microsoft.Edge",
        "com.opera.Opera",
        "io.gitlab.librewolf-community",
        "app.zen_browser.zen",
        "Opera beta",
    ];

    #[test]
    fn browser_classes() {
        for class in BROWSER_CLASSES {
            let class = *class;
            assert!(is_browser(class), "{class}");
            assert!(
                applies("browser", Some("Gmail"), Some(class), Some("x - Gmail")),
                "{class}"
            );
        }
        for class in [
            "Alacritty",
            "chromebook-tool",
            "operator",
            "zenity",
            "firefoxy",
            "",
        ] {
            assert!(!is_browser(class), "{class}");
        }
        // A named provider with a window_title needs both.
        assert!(applies(
            "Alacritty",
            Some("vim"),
            Some("alacritty"),
            Some("vim notes.md")
        ));
        assert!(!applies(
            "Alacritty",
            Some("vim"),
            Some("alacritty"),
            Some("zsh")
        ));
        assert!(!applies(
            "Alacritty",
            Some("vim"),
            Some("firefox"),
            Some("vim notes.md")
        ));
        // Spaces around window_title don't count; spaces alone are no filter.
        assert!(applies(
            "browser",
            Some(" Gmail "),
            Some("firefox"),
            Some("Inbox - Gmail")
        ));
        assert!(applies("browser", Some("  "), Some("firefox"), None));
        assert!(applies("Browser", None, Some("firefox"), None));
    }

    #[test]
    fn parsers_ignore_the_title() {
        let (sheet, _) = parse_reporting(
            &fixtures(),
            Some("st-256color"),
            None,
            Some("anything"),
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            keys(&sheet, "clipboard")[..3],
            ["shift+Insert", "ctrl+shift+c", "ctrl+shift+v"]
        );
    }

    #[test]
    fn app_sheets_are_valid() {
        let f_keys: Vec<String> = (1..=12).map(|n| format!("F{n}")).collect();
        let names: Vec<&str> = typed_keys()
            .into_iter()
            .chain([
                "Break", "Print", "Prior", "Next", "Insert", "Num_Lock", "Tab", "Return", "Escape",
                "space",
            ])
            .chain(f_keys.iter().map(String::as_str))
            .collect();
        let sheets = APP_SHEETS
            .iter()
            .map(|(_, name, text)| (*name, *text))
            .chain(WM_SHEETS.iter().map(|(_, name, text)| (*name, *text)));
        for (name, text) in sheets {
            let value: serde_json::Value = serde_json::from_str(text).expect(name);
            assert!(value.get("window_title").is_none(), "{name}");
            let s: Sheet = serde_json::from_value(value).expect(name);
            // The title names the program, its release, the tag's commit and `config.def.h`.
            let program = name.trim_end_matches(".json");
            assert_eq!(s.provider, program, "{name}");
            assert!(
                s.title.starts_with(&format!("{program} "))
                    && s.title.contains("defaults (config.def.h at "),
                "{name}: {}",
                s.title
            );
            assert_valid_keys(name, &s, &names);
        }
        // dwm: `TAGKEYS` for 1-9 under its four modifiers, and the two all-tags keys.
        let (_, _, dwm) = WM_SHEETS[0];
        let s: Sheet = serde_json::from_str(dwm).unwrap();
        let keys: Vec<&str> = s.binds.values().flatten().map(|b| b.key.as_str()).collect();
        for n in 1..=9 {
            for m in ["alt", "ctrl+alt", "alt+shift", "ctrl+alt+shift"] {
                assert!(keys.contains(&format!("{m}+{n}").as_str()), "{m}+{n}");
            }
        }
        assert!(keys.contains(&"alt+0") && keys.contains(&"alt+shift+0"));
        assert_eq!(keys.len(), 24 + 36);
    }

    #[test]
    fn suckless_sheets_by_class_and_session() {
        let none = Path::new("/nonexistent/keybind-find-sheets");
        let get = |class: &str, wm: Option<&str>| {
            let (sheet, notes) =
                parse_reporting(none, Some(class), None, None, wm, PARSER_TIMEOUT).unwrap();
            assert!(notes.is_empty(), "{notes:?}");
            sheet
                .binds
                .values()
                .flatten()
                .map(|b| b.key.clone())
                .collect::<Vec<_>>()
        };
        for class in ["st-256color", "St", "ST-256COLOR"] {
            assert!(
                get(class, None).contains(&"ctrl+shift+c".to_string()),
                "{class}"
            );
        }
        assert!(get("Surf", None).contains(&"ctrl+g".to_string()));
        assert!(get("tabbed", None).contains(&"ctrl+shift+Return".to_string()));
        let firefox = get("firefox", None);
        assert!(!firefox.contains(&"ctrl+shift+Return".to_string()));
        assert!(!firefox.contains(&"alt+p".to_string()));
        // dwm by session, whatever the class, with the class's own sheet; no dwm keys in another session.
        let dwm = get("firefox", Some("dwm"));
        assert!(dwm.contains(&"alt+p".to_string()) && dwm.contains(&"ctrl+t".to_string()));
        assert!(!get("firefox", Some("i3")).contains(&"alt+p".to_string()));
        // `parse`, which `sheet` calls with `context::window().wm`, passes it through.
        let ctx = window(Some("St"), None);
        let all = |s: Sheet| {
            s.binds
                .into_values()
                .flatten()
                .map(|b| b.key)
                .collect::<Vec<_>>()
        };
        assert!(all(parse(none, &ctx, Some("dwm")).unwrap()).contains(&"alt+p".to_string()));
        assert!(!all(parse(none, &ctx, None).unwrap()).contains(&"alt+p".to_string()));
    }

    #[test]
    fn a_user_file_replaces_a_suckless_sheet() {
        let dir = std::env::temp_dir().join(format!("kbf-sheets-suckless-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let keys = |class: &str, wm: Option<&str>| {
            let (sheet, notes) =
                parse_reporting(&dir, Some(class), None, None, wm, PARSER_TIMEOUT).unwrap();
            let keys: Vec<String> = sheet.binds.into_values().flatten().map(|b| b.key).collect();
            (keys, notes)
        };
        // Same name, any provider: it replaces the built-in (here it applies to no window).
        std::fs::write(
            dir.join("st.json"),
            r#"{"title": "mine", "provider": "nothing", "binds": {"x": [{"key": "ctrl+x", "desc": "x"}]}}"#,
        )
        .unwrap();
        let (k, notes) = keys("st-256color", None);
        assert!(k.is_empty() && notes.is_empty(), "{k:?} {notes:?}");
        // A broken one too: a note, and no st keys.
        std::fs::write(dir.join("st.json"), "{").unwrap();
        let (k, notes) = keys("st-256color", None);
        assert!(k.is_empty(), "{k:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        // dwm's the same way.
        std::fs::write(dir.join("DWM.json"), "{").unwrap();
        let (k, _) = keys("firefox", Some("dwm"));
        assert!(!k.contains(&"alt+p".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trust_rules() {
        assert_eq!(trust(1000, 0o100755, 1000), Ok(()));
        assert_eq!(
            trust(0, 0o100755, 1000),
            Err("owned by another user".into())
        );
        assert!(trust(1000, 0o100775, 1000).is_err());
        assert!(trust(1000, 0o100757, 1000).is_err());
    }

    #[test]
    fn which_names_are_parsers() {
        let m = |name: &str, class: &str| parser_matches(Path::new(name), Some(class));
        assert!(m("st-256color", "st-256color"));
        assert!(m("st-256color.py", "ST-256color"));
        assert!(m("org.gnome.Nautilus", "org.gnome.Nautilus"));
        assert!(!m("st-256color.py~", "st-256color"));
        assert!(!m("st-256color.bak", "st-256color"));
        assert!(!m("st-256color.ORIG", "st-256color"));
        assert!(!m("st-256color.py", "st"));
        assert!(!parser_matches(Path::new("st"), None));
    }

    #[test]
    fn edge_files() {
        let dir = temp("edge");
        std::fs::create_dir_all(dir.join("sub.json")).unwrap();
        std::fs::write(
            dir.join("a.json"),
            r#"{"title":"A","provider":"VIVALDI-stable","binds":{}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("c.json"),
            r#"{"title":"C","provider":"VIVALDI-stable","binds":{"tabs":[{"key":"ctrl+t","desc":"New tab"}]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "not a sheet").unwrap();
        std::fs::write(dir.join("B.JSON"), "not json").unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("dangling.json")).unwrap();
        let (sheet, notes) = parse_reporting(
            &dir,
            Some("vivaldi-stable"),
            None,
            None,
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        // Empty binds, a directory named *.json and a non-JSON file: nothing. `B.JSON` is a sheet (any case) and
        // broken; a dangling `dangling.json` is reported too (B.JSON sorts first).
        // `VIVALDI-stable` matches class `vivaldi-stable`; a.json's empty binds add nothing.
        assert_eq!(sheet.binds.keys().collect::<Vec<_>>(), ["tabs"]);
        assert_eq!(keys(&sheet, "tabs"), ["ctrl+t"]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("B.JSON") && notes[0].contains("not a DMS cheatsheet"),
            "{}",
            notes[0]
        );
        assert!(notes[1].contains("dangling.json"), "{}", notes[1]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every bind of a built-in sheet has a description and a key in the notation, from the keysyms in `names`.
    fn assert_valid_keys(name: &str, sheet: &Sheet, names: &[&str]) {
        assert!(!sheet.binds.is_empty(), "{name}");
        for bind in sheet.binds.values().flatten() {
            assert!(!bind.desc.is_empty(), "{name}: {}", bind.key);
            for chord in bind.key.split(' ') {
                let parts: Vec<&str> = chord.split('+').collect();
                let (keysym, mods) = parts.split_last().unwrap();
                assert!(names.contains(keysym), "{name}: {}", bind.key);
                assert!(
                    mods.iter()
                        .all(|m| ["super", "ctrl", "alt", "shift"].contains(m)),
                    "{name}: {}",
                    bind.key
                );
                assert_eq!(crate::sheet::chord(mods, keysym), chord, "{name}");
            }
        }
    }

    fn typed_keys() -> Vec<&'static str> {
        (' '..='~')
            .filter_map(super::super::nvim::single)
            .chain(["Left", "Right", "Up", "Down", "Home", "End"])
            .collect()
    }

    #[test]
    fn builtin_sheets_are_valid() {
        let names = typed_keys();
        for (name, text) in BUILTIN {
            let value: serde_json::Value = serde_json::from_str(text).expect(name);
            let window_title = value["window_title"].as_str().unwrap_or_default();
            assert!(!window_title.trim().is_empty(), "{name}");
            // It applies on its own tabs, through the same parse as a file.
            let tab = format!("x - {window_title} - Mozilla Firefox");
            let (s, by_title) = sheet_from(text, Some("firefox"), Some(&tab))
                .expect(name)
                .expect(name);
            assert!(by_title, "{name}");
            assert_eq!(s.provider, "browser", "{name}");
            assert_valid_keys(name, &s, &names);
        }
    }

    #[test]
    fn browser_sheets_are_valid() {
        let f_keys: Vec<String> = (1..=12).map(|n| format!("F{n}")).collect();
        let names: Vec<&str> = typed_keys()
            .into_iter()
            .chain([
                "Tab",
                "Prior",
                "Next",
                "Escape",
                "Return",
                "BackSpace",
                "Delete",
                "space",
            ])
            .chain(f_keys.iter().map(String::as_str))
            .collect();
        for (family, name, text) in BROWSER_SHEETS {
            let value: serde_json::Value = serde_json::from_str(text).expect(name);
            assert!(value.get("window_title").is_none(), "{name}");
            let s: Sheet = serde_json::from_value(value).expect(name);
            assert_eq!(s.provider, *family, "{name}");
            assert_eq!(*name, format!("{family}.json"));
            // The title names the browser and its version, then the date the keys were checked.
            let mut words = s.title.split_whitespace();
            assert!(
                matches!(words.next(), Some("Firefox" | "Chrome")),
                "{name}: {}",
                s.title
            );
            assert!(
                words.next().and_then(|v| v.parse::<u32>().ok()).is_some(),
                "{name}: {}",
                s.title
            );
            assert!(s.title.contains("checked 20"), "{name}: {}", s.title);
            assert_valid_keys(name, &s, &names);
        }
    }

    #[test]
    fn builtin_sheets_apply_with_no_folder() {
        // With a URL-in-title extension: signed-in GitHub tabs don't name GitHub.
        let github =
            "Benjmhart/keybind-find - https://github.com/Benjmhart/keybind-find — Mozilla Firefox";
        // A missing folder, and no folder at all (no $HOME).
        for dir in ["/nonexistent-kbf/sheets", ""] {
            let (sheet, notes) = parse_reporting(
                Path::new(dir),
                Some("firefox"),
                None,
                Some(github),
                None,
                PARSER_TIMEOUT,
            )
            .unwrap();
            assert!(notes.is_empty(), "{notes:?}");
            let g_c = sheet.binds["repository"].iter().find(|b| b.key == "g c");
            assert_eq!(g_c.map(|b| b.desc.as_str()), Some("Go to Code"));
            // Only GitHub's: not Gmail's or YouTube's.
            assert!(!sheet.binds.contains_key("player") && !sheet.binds.contains_key("go to"));
        }
        // The same tab without the extension: no GitHub keys.
        let (sheet, _) = parse_reporting(
            Path::new(""),
            Some("firefox"),
            None,
            Some("Benjmhart/keybind-find — Mozilla Firefox"),
            None,
            PARSER_TIMEOUT,
        )
        .unwrap();
        assert!(!sheet.binds.contains_key("repository"));
        // Which built-in sheets a Vivaldi tab gets, by a category only that sheet has.
        let apps = |title: &str| {
            let (sheet, _) = parse_reporting(
                Path::new(""),
                Some("Vivaldi-stable"),
                None,
                Some(title),
                None,
                PARSER_TIMEOUT,
            )
            .unwrap();
            [
                ("github", "repository"),
                ("gmail", "go to"),
                ("youtube", "player"),
            ]
            .into_iter()
            .filter(|(_, c)| sheet.binds.contains_key(*c))
            .map(|(app, _)| app)
            .collect::<Vec<_>>()
        };
        assert_eq!(
            apps("Inbox (3) - you@example.com - Gmail - Vivaldi"),
            ["gmail"]
        );
        // An app named elsewhere in a title doesn't count.
        assert_eq!(
            apps("[GitHub] A new SSH key - you@example.com - Gmail - Vivaldi"),
            ["gmail"]
        );
        assert_eq!(
            apps("Learn Gmail in 5 minutes - YouTube - Vivaldi"),
            ["youtube"]
        );
        assert!(apps("Benjmhart/keybind-find: a keybinding finder - Vivaldi").is_empty());
        assert!(apps("github - Google Search - Vivaldi").is_empty());
    }

    #[test]
    fn a_user_file_replaces_a_builtin_sheet() {
        let dir = temp("builtin");
        let github = Some("Issues - https://github.com/Benjmhart/keybind-find/issues");
        // Vivaldi: a browser with no built-in keys of its own, so only the web-app sheets are in play.
        let run = || {
            parse_reporting(
                &dir,
                Some("Vivaldi-stable"),
                None,
                github,
                None,
                PARSER_TIMEOUT,
            )
            .unwrap()
        };
        // A sheet for another app leaves the built-in one alone; theirs merges first.
        std::fs::write(
            dir.join("gitlab.json"),
            r#"{"title":"L","provider":"browser","binds":{"repository":[{"key":"F6","desc":"Mine"}]}}"#,
        )
        .unwrap();
        let (sheet, notes) = run();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(keys(&sheet, "repository")[..2], ["F6", "g c"]);
        std::fs::remove_file(dir.join("gitlab.json")).unwrap();
        // The same name, in any case, replaces it.
        std::fs::write(
            dir.join("GitHub.json"),
            r#"{"title":"G","provider":"browser","window_title":"github.com","binds":{"mine":[{"key":"F5","desc":"Mine"}]}}"#,
        )
        .unwrap();
        let (sheet, notes) = run();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(sheet.binds.keys().collect::<Vec<_>>(), ["mine"]);
        // One that doesn't match this tab, or has no binds (the README's way to turn one off), still replaces it.
        for user in [
            r#"{"title":"G","provider":"browser","window_title":"nowhere","binds":{"mine":[{"key":"F5","desc":"Mine"}]}}"#,
            r#"{"title": "GitHub", "provider": "browser", "binds": {}}"#,
        ] {
            std::fs::write(dir.join("GitHub.json"), user).unwrap();
            let (sheet, notes) = run();
            assert!(
                notes.is_empty() && sheet.binds.is_empty(),
                "{user} {notes:?}"
            );
            // In Firefox, with GitHub's sheet turned off, the tab gets Firefox's own keys instead.
            let (sheet, _) =
                parse_reporting(&dir, Some("firefox"), None, github, None, PARSER_TIMEOUT).unwrap();
            assert_eq!(browser_sheet(&sheet), Some("firefox"), "{user}");
        }
        // A broken one is reported, and the built-in one isn't used instead.
        std::fs::remove_file(dir.join("GitHub.json")).unwrap();
        std::fs::write(dir.join("github.json"), "{").unwrap();
        let (sheet, notes) = run();
        assert!(sheet.binds.is_empty());
        assert!(
            notes.len() == 1 && notes[0].contains("github.json"),
            "{notes:?}"
        );
        // So is one that can't be reached.
        std::fs::remove_file(dir.join("github.json")).unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("github.json")).unwrap();
        let (sheet, notes) = run();
        assert!(sheet.binds.is_empty());
        assert!(
            notes.len() == 1 && notes[0].contains("github.json"),
            "{notes:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sheets_dir_follows_xdg() {
        let env = |n: &str| match n {
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            sheets_dir(&env),
            Some(PathBuf::from("/xdg/keybind-find/sheets"))
        );
        let env = |n: &str| match n {
            "XDG_CONFIG_HOME" => Some("rel".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            sheets_dir(&env),
            Some(PathBuf::from("/h/.config/keybind-find/sheets"))
        );
        assert_eq!(sheets_dir(&|_: &str| None), None);
    }
}
