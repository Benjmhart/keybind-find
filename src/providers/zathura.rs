//! zathura: its default shortcuts pinned per version (`data/zathura/`, generated from zathura's and girara's
//! source), with the `map` and `unmap` lines of the zathurarc files zathura reads laid over them. Shown when a zathura
//! window has focus. `exec`/`feedkeys` arguments are shown as text, never run; included files are only read.

use super::nvim::single;
use super::pinned;
use crate::context::{run_for, Context};
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const REGISTRY: &[(&str, &str)] = &[
    (
        "0.5.2",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/zathura/0.5.2.toml"
        )),
    ),
    (
        "0.5.11",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/zathura/0.5.11.toml"
        )),
    ),
    (
        "2026.10.23",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/zathura/2026.10.23.toml"
        )),
    ),
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// The Flatpak's app id, also its window class.
const FLATPAK: &str = "org.pwmt.zathura";

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pinned {
    zathura_version: String,
    modes: Vec<String>,
    functions: Vec<String>,
    arguments: Vec<String>,
    /// girara's key names in `<…>`, as X keysyms.
    keys: BTreeMap<String, String>,
    bind: Vec<Shortcut>,
}

#[derive(Deserialize, Clone, PartialEq)]
struct Shortcut {
    mode: String,
    key: String,
    function: String,
    argument: String,
    #[serde(default)]
    data: String,
    /// A default's argument that has no zathurarc name (`append filepath`).
    #[serde(default)]
    detail: String,
}

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(
        REGISTRY,
        |_| {},
        &env,
        is_flatpak(ctx.class.as_deref()),
        Path::new("/etc"),
    )?;
    if !notes.is_empty() {
        eprintln!("keybind-find: zathura: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// Whether a window of `class` is the Flatpak's zathura (its app id), any case.
fn is_flatpak(class: Option<&str>) -> bool {
    class.is_some_and(|c| c.eq_ignore_ascii_case(FLATPAK))
}

/// `sheet` over `registry`, with `setup` applied to `zathura --version` (a test seam: `PATH`), the config files found
/// through `env` (the Flatpak's when `flatpak`), and `sysconf` standing for `/etc`.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    env: Env,
    flatpak: bool,
    sysconf: &Path,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut cmd = Command::new("zathura");
    cmd.arg("--version")
        .env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let version = run_for(cmd, VERSION_TIMEOUT).context("running zathura --version")?;
    let (v, text, note) = pinned::select("zathura", registry, version_of(&version).as_deref())
        .context("no pinned zathura defaults")?;
    let pinned: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/zathura/{v}.toml"))?;
    if pinned.zathura_version != v {
        bail!(
            "data/zathura/{v}.toml names zathura {}",
            pinned.zathura_version
        );
    }
    let mut rc = Rc {
        pinned: &pinned,
        binds: pinned.bind.clone(),
        seen: HashSet::new(),
        notes: note.into_iter().collect(),
        left_out: 0,
        unsplit: 0,
    };
    for (file, system) in rc_files(env, flatpak, sysconf) {
        rc.read(&file, system);
    }
    let Rc {
        binds,
        mut notes,
        left_out,
        unsplit,
        ..
    } = rc;
    if unsplit > 0 {
        let lines = if unsplit == 1 { "line" } else { "lines" };
        notes.push(format!(
            "{unsplit} zathurarc {lines} that don't split (an unclosed quote?) end their file there, as in zathura"
        ));
    }
    if left_out > 0 {
        let maps = if left_out == 1 { "map" } else { "maps" };
        notes.push(format!(
            "leaving out {left_out} {maps}: mouse binds, and keys girara or keybind-find can't read"
        ));
    }
    Ok((to_sheet(binds), notes))
}

/// The version on `zathura --version`'s first line (`zathura 0.5.11`, `zathura 2026.10.23`).
fn version_of(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    line.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|w| w.trim_matches('.'))
        .find(|w| w.contains('.') && pinned::parse_version(w).is_some())
        .map(str::to_string)
}

/// The zathurarc files zathura reads, in its order (`config_load_files`), each with whether it's a system file:
/// `<entry>/zathurarc` for each `$XDG_CONFIG_DIRS` entry (else `/etc/xdg`) from the last, `/etc/zathurarc`, then `zathurarc` in the user's config dir: `$XDG_CONFIG_HOME/zathura` (else
/// `~/.config/zathura`), the Flatpak's under `~/.var/app`. An empty variable counts as unset. Native zathura's window
/// class is the Flatpak's app id too (`org.pwmt.zathura`), so for such a window the native file is read when it
/// exists and the Flatpak's otherwise. ponytail: someone with both installs gets the native file over the Flatpak's;
/// tell them apart by the window's process (`/.flatpak-info` in its root) if that bites.
fn rc_files(env: Env, flatpak: bool, sysconf: &Path) -> Vec<(PathBuf, bool)> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    let dirs = set("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into());
    let mut files: Vec<(PathBuf, bool)> = dirs
        .split(':')
        .rev()
        // An empty entry would be keybind-find's working directory, not zathura's.
        .filter(|d| !d.is_empty())
        .map(|d| (Path::new(d).join("zathurarc"), true))
        .collect();
    files.push((sysconf.join("zathurarc"), true));
    let home = set("HOME").map(PathBuf::from);
    let native = set("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.clone().map(|h| h.join(".config")))
        .map(|d| d.join("zathura/zathurarc"));
    let user = match native {
        Some(n) if flatpak && !n.exists() => home.map(|h| {
            h.join(".var/app")
                .join(FLATPAK)
                .join("config/zathura/zathurarc")
        }),
        n => n,
    };
    files.extend(user.map(|f| (f, false)));
    files
}

/// The shortcuts as zathurarc lines change them.
struct Rc<'a> {
    pinned: &'a Pinned,
    binds: Vec<Shortcut>,
    /// Files read so far (canonical): each is read once.
    seen: HashSet<PathBuf>,
    notes: Vec<String>,
    left_out: usize,
    unsplit: usize,
}

impl Rc<'_> {
    /// Applies `file`'s `map`, `unmap` and `include` lines, as girara's `config_parse`. A missing file is skipped
    /// silently; one that can't be read is a note, except the user's (`system` false), which zathura passes over.
    fn read(&mut self, file: &Path, system: bool) {
        let key = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        if !self.seen.insert(key) {
            return;
        }
        let text = match std::fs::read(file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                if system {
                    self.notes.push(format!("reading {}: {e}", file.display()));
                }
                return;
            }
        };
        for line in text.lines() {
            // girara's comment prefixes: `#`, and vim's `"` (`COMMENT_PREFIX`), only in a line's first column.
            if line.is_empty() || line.starts_with(['#', '"']) {
                continue;
            }
            // girara stops reading a file at a line it can't split.
            let Some(words) = split(line) else {
                self.unsplit += 1;
                return;
            };
            match words.first().map(String::as_str) {
                Some("include") if words.len() == 2 => self.include(file, &words[1]),
                Some("map") => self.map(&words[1..], false),
                Some("unmap") => self.map(&words[1..], true),
                _ => {}
            }
        }
    }

    /// `include <path>`: relative to `from`'s directory; a file never includes itself (it's been `seen`).
    fn include(&mut self, from: &Path, path: &str) {
        let target = match Path::new(path) {
            p if p.is_absolute() => p.to_path_buf(),
            p => from.parent().unwrap_or(Path::new("/")).join(p),
        };

        if !target.exists() {
            self.notes.push(format!(
                "{}: include {path}: no file at {}",
                from.display(),
                target.display()
            ));
            return;
        }
        self.read(&target, true);
    }

    /// `map [mode] <key> <function> [argument] [data]` or `unmap [mode] <key>`, as girara's `girara_cmd_map_unmap`:
    /// an unknown mode, key or function changes nothing.
    fn map(&mut self, words: &[String], unmap: bool) {
        let (mode, rest) = match words.first() {
            Some(w) if w.chars().count() >= 3 && w.starts_with('[') && w.ends_with(']') => {
                let m = &w[1..w.len() - 1];
                if !self.pinned.modes.iter().any(|p| p == m) {
                    return;
                }
                (m.to_string(), &words[1..])
            }
            _ => ("normal".to_string(), words),
        };
        let Some(key_word) = rest.first() else {
            return;
        };
        let key = match parse_key(key_word, &self.pinned.keys) {
            Parsed::Key(k) => k,
            Parsed::Mouse | Parsed::Unreadable => {
                if !unmap && rest.len() > 1 {
                    self.left_out += 1;
                }
                return;
            }
        };
        if unmap {
            self.binds.retain(|b| !(b.mode == mode && b.key == key));
            return;
        }
        // `map <key>` alone shows the mapping: nothing to bind.
        let Some(function) = rest.get(1) else {
            return;
        };
        if !self.pinned.functions.iter().any(|f| f == function) {
            return;
        }
        // A known argument name, then data; else the word is the data.
        let (argument, data) = match rest.get(2) {
            Some(a) if self.pinned.arguments.iter().any(|p| p == a) => {
                (a.clone(), rest.get(3).cloned().unwrap_or_default())
            }
            Some(d) => (String::new(), d.clone()),
            None => Default::default(),
        };
        let new = Shortcut {
            mode: mode.clone(),
            key: key.clone(),
            function: function.clone(),
            argument,
            data,
            detail: String::new(),
        };
        match self
            .binds
            .iter_mut()
            .find(|b| b.mode == mode && b.key == key)
        {
            Some(b) => *b = new,
            None => self.binds.push(new),
        }
    }
}

/// A zathurarc line's words, split as `g_shell_parse_argv` splits them (single quotes literal; in double quotes `\`
/// escapes `"`, `\`, `$`, `` ` ``; outside them it escapes any character; `#` after a space ends the line);
/// `None` when a quote isn't closed or a `\` ends the line.
fn split(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars().peekable();
    // GLib starts a comment at `#` only at the line's start or after a space (not a tab), and splits words only at
    // spaces and tabs.
    let mut prev = None;
    while let Some(c) = chars.next() {
        let at = prev;
        prev = Some(c);
        match c {
            ' ' | '\t' => {
                words.extend(word.take());
            }
            '#' if matches!(at, None | Some(' ')) => break,
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => w.push(c),
                    }
                }
            }
            '"' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                            w.extend(chars.next());
                        }
                        c => w.push(c),
                    }
                }
            }
            // A `\` ending the line is an error to `g_shell_parse_argv`, as an unclosed quote is.
            '\\' => {
                // The escaped character is the one GLib looks back at (`a\ #b`: a comment).
                let next = chars.next()?;
                prev = Some(next);
                word.get_or_insert_with(String::new).push(next);
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Some(words)
}

enum Parsed {
    Key(String),
    Mouse,
    /// girara refuses it, or keybind-find has no keysym for it.
    Unreadable,
}

/// The mouse names girara takes in `<…>` (buttons and event types).
const MOUSE: &[&str] = &[
    "Button1",
    "Button2",
    "Button3",
    "Button4",
    "Button5",
    "Button6",
    "Button7",
    "Button8",
    "Button9",
    "motion",
    "scroll_up",
    "scroll_down",
    "scroll_left",
    "scroll_right",
];

/// A zathurarc key in keybind-find's notation, as girara reads it: `<X-k>` with one modifier (`S`, `A`/`M`, `C`)
/// and one character or a name from `keys`; `<Name>`; one character (`J` is shift+j); anything longer a sequence of
/// characters (`gg`, `zO`).
fn parse_key(word: &str, keys: &BTreeMap<String, String>) -> Parsed {
    let n = word.chars().count();
    let Some(inner) = word
        .strip_prefix('<')
        .and_then(|w| w.strip_suffix('>'))
        .filter(|_| n >= 3)
    else {
        if n == 1 {
            return char_key(word.chars().next(), &[]);
        }
        let parts: Option<Vec<String>> = word
            .chars()
            .map(|c| match char_key(Some(c), &[]) {
                Parsed::Key(k) => Some(k),
                _ => None,
            })
            .collect();
        return parts.map_or(Parsed::Unreadable, |p| Parsed::Key(p.join(" ")));
    };
    let mut chars = inner.chars();
    let (mods, name): (Vec<&str>, &str) = match (chars.next(), chars.next()) {
        (Some(m), Some('-')) if inner.chars().count() > 2 => {
            let m = match m {
                'S' => "shift",
                'A' | 'M' => "alt",
                'C' => "ctrl",
                _ => return Parsed::Unreadable,
            };
            // The modifier and `-` are ASCII.
            let rest = &inner[2..];
            if rest.chars().count() == 1 {
                return char_key(rest.chars().next(), &[m]);
            }
            (vec![m], rest)
        }
        _ => (vec![], inner),
    };
    match keys.get(name) {
        Some(keysym) => Parsed::Key(chord(&mods, keysym)),
        None if MOUSE.contains(&name) => Parsed::Mouse,
        None => Parsed::Unreadable,
    }
}

/// One typed character with `mods`: an upper-case letter adds shift (its keyval is the capital); punctuation by
/// keysym name.
fn char_key(c: Option<char>, mods: &[&'static str]) -> Parsed {
    let Some(c) = c else {
        return Parsed::Unreadable;
    };
    let mut mods = mods.to_vec();
    if c.is_ascii_uppercase() {
        mods.push("shift");
    }
    match single(c.to_ascii_lowercase()) {
        Some(k) => Parsed::Key(chord(&mods, k)),
        None => Parsed::Unreadable,
    }
}

/// One category per mode. A fullscreen shortcut identical to normal's isn't listed again. The description is the
/// function with its argument and data, as a zathurarc line says them.
fn to_sheet(binds: Vec<Shortcut>) -> Sheet {
    let normal: Vec<&Shortcut> = binds.iter().filter(|b| b.mode == "normal").collect();
    let mut sheet = Sheet {
        title: "zathura".into(),
        provider: "zathura".into(),
        binds: Default::default(),
    };

    for b in &binds {
        if b.mode == "fullscreen"
            && normal.iter().any(|n| {
                Shortcut {
                    mode: b.mode.clone(),
                    ..(*n).clone()
                } == *b
            })
        {
            continue;
        }
        let detail = (!b.detail.is_empty()).then(|| format!("({})", b.detail));
        let desc = [b.function.as_str(), b.argument.as_str(), b.data.trim()]
            .into_iter()
            .chain(detail.as_deref())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        sheet.binds.entry(b.mode.clone()).or_default().push(Bind {
            key: b.key.clone(),
            desc: desc.clone(),
            action: desc,
            when: Default::default(),
        });
    }
    sheet
}

#[cfg(test)]
mod tests {
    use super::*;
    use keybind_find_sheet::Key;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zathura")
    }

    fn pinned(v: &str) -> Pinned {
        toml::from_str(REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1).unwrap()
    }

    fn find<'a>(s: &'a Sheet, mode: &str, key: &str) -> Vec<&'a str> {
        s.binds
            .get(mode)
            .map(|b| {
                b.iter()
                    .filter(|b| b.key == key)
                    .map(|b| b.desc.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The sheet for pin `v` with the zathurarc files from `env` laid over its defaults.
    fn read(
        v: &str,
        env: Env,
        flatpak: bool,
        sysconf: &Path,
    ) -> (Sheet, Vec<String>, usize, usize) {
        // Never the machine's /etc/xdg: XDG_CONFIG_DIRS is somewhere empty unless the test sets it.
        let env = |k: &str| match env(k) {
            None if k == "XDG_CONFIG_DIRS" => Some("/nonexistent-kbf".into()),
            v => v,
        };
        let env: Env = &env;
        let pinned = pinned(v);
        let mut rc = Rc {
            pinned: &pinned,
            binds: pinned.bind.clone(),
            seen: HashSet::new(),
            notes: Vec::new(),
            left_out: 0,
            unsplit: 0,
        };
        for (file, system) in rc_files(env, flatpak, sysconf) {
            rc.read(&file, system);
        }
        (to_sheet(rc.binds), rc.notes, rc.left_out, rc.unsplit)
    }

    #[test]
    fn pinned_files() {
        for (v, commit) in [
            ("0.5.2", "bccbe933c663a63ccd30adfea74869d2751fe4b8"),
            ("0.5.11", "9c9380e57d83eb3638348710965fc896f7f71709"),
            ("2026.10.23", "839f3192e96cc811033ebe500dab4ac11449d59d"),
        ] {
            let text = REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1;
            assert!(
                text.lines()
                    .next()
                    .unwrap()
                    .contains(&format!("at tag {v} (commit {commit})")),
                "{v}"
            );
            assert!(text.contains("zlib licensed"), "{v}");
            let p = pinned(v);
            assert_eq!(p.zathura_version, v);
            for b in &p.bind {
                assert!(b.key.parse::<Key>().is_ok(), "{v}: {}", b.key);
                assert!(p.functions.contains(&b.function), "{v}: {}", b.function);
                assert!(
                    b.argument.is_empty() || p.arguments.contains(&b.argument),
                    "{v}: {}",
                    b.argument
                );
                assert!(p.modes.contains(&b.mode), "{v}: {}", b.mode);
            }
            for keysym in p.keys.values() {
                assert!(keysym.parse::<Key>().is_ok(), "{v}: {keysym}");
            }
        }
        // As the source has them (2026.10.23: zathura/config.c config_load_default).
        let (s, _, _, _) = read("2026.10.23", &|_| None, false, Path::new("/nonexistent"));
        assert_eq!(find(&s, "normal", "shift+j"), ["navigate next"]);
        assert_eq!(find(&s, "normal", "j"), ["scroll down"]);
        assert_eq!(find(&s, "normal", "g g"), ["goto top"]);
        assert_eq!(find(&s, "normal", "z shift+o"), ["zoom out"]);
        assert_eq!(find(&s, "normal", "o"), ["focus_inputbar :open"]);
        assert_eq!(
            find(&s, "normal", "shift+o"),
            ["focus_inputbar :open (append filepath)"]
        );
        assert_eq!(find(&s, "normal", "z 0"), ["zoom (original)"]);
        assert_eq!(find(&s, "normal", "XF86Forward"), ["navigate next"]);
        assert_eq!(find(&s, "normal", "ctrl+n"), ["toggle_statusbar"]);
        assert_eq!(find(&s, "index", "Escape"), ["toggle_index"]);
        assert_eq!(find(&s, "presentation", "space"), ["navigate next"]);
        assert_eq!(find(&s, "insert", "q"), ["quit"]);
        // girara 0.x's own normal-mode keys: `<C-q>` quit (gone in 2026.x), `<C-[>` girara's abort.
        let (s, _, _, _) = read("0.5.2", &|_| None, false, Path::new("/nonexistent"));
        assert_eq!(find(&s, "normal", "ctrl+q"), ["quit"]);
        assert_eq!(find(&s, "normal", "ctrl+bracketleft"), ["abort"]);

        assert!(pinned("0.5.2").keys.len() < pinned("0.5.11").keys.len());
    }

    #[test]
    fn version_pick() {
        let pick = |out: &str| {
            let (v, _, note) =
                pinned::select("zathura", REGISTRY, version_of(out).as_deref()).unwrap();
            (v, note)
        };
        assert_eq!(
            pick("zathura 0.5.11\ngirara 0.4.5 (runtime: 0.4.5)\n"),
            ("0.5.11", None)
        );
        assert_eq!(
            pick("zathura 0.5.6"),
            (
                "0.5.2",
                Some(
                    "zathura 0.5.6: using the defaults of 0.5.2; add data/zathura/0.5.6.toml"
                        .into()
                )
            )
        );
        assert_eq!(
            pick("zathura 2026.07.18\ngirara 2026.07.18"),
            (
                "2026.10.23",
                Some("zathura 2026.07.18: using the defaults of 2026.10.23; add data/zathura/2026.07.18.toml".into())
            )
        );
        assert_eq!(
            pick("garbage"),
            (
                "2026.10.23",
                Some("zathura's version is unknown: using the defaults of 2026.10.23".into())
            )
        );
        // Only the first line: girara's version below isn't zathura's.
        assert_eq!(version_of("zathura\ngirara 0.4.5"), None);
    }

    #[test]
    fn zathurarc_over_defaults() {
        let home = fixtures().join("user").to_string_lossy().into_owned();
        let env = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| home.clone());
        let (s, notes, left_out, unsplit) =
            read("2026.10.23", &env, false, Path::new("/nonexistent"));
        // Replaced in place; added in a mode; removed.
        assert_eq!(find(&s, "normal", "shift+j"), ["scroll down"]);
        assert_eq!(find(&s, "index", "shift+q"), ["quit"]);
        assert_eq!(find(&s, "index", "q"), ["quit"]);
        assert!(find(&s, "presentation", "space").is_empty());
        assert_eq!(find(&s, "normal", "space"), ["scroll full-down"]);
        // An unknown function or mode changes nothing; nor does an `include` with two paths.
        assert!(find(&s, "normal", "x").is_empty());
        assert!(!s.binds.contains_key("nosuchmode"));
        assert!(find(&s, "normal", "F12").is_empty());
        // A quoted argument: the data, shown trimmed.
        assert_eq!(find(&s, "normal", "ctrl+o"), ["focus_inputbar :open"]);
        // A buffer command.
        assert_eq!(find(&s, "normal", "g x"), ["goto top"]);
        // Included: relative, and each of a loop once; a file including itself is skipped.
        assert_eq!(find(&s, "normal", "alt+Right"), ["navigate previous"]);
        assert_eq!(find(&s, "normal", "F2"), ["quit"]);
        assert_eq!(find(&s, "normal", "F3"), ["quit"]);
        // The mouse bind and the key girara refuses (`<C-S-x>`): counted.
        assert_eq!((left_out, unsplit), (2, 0));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("include missing.rc: no file at"),
            "{notes:?}"
        );
    }

    #[test]
    fn fullscreen_duplicates() {
        let (s, _, _, _) = read("2026.10.23", &|_| None, false, Path::new("/nonexistent"));
        // Every default fullscreen shortcut is normal's too (F11 toggles fullscreen in both): none listed again.
        assert!(!s.binds.contains_key("fullscreen"));
        assert_eq!(find(&s, "normal", "F11"), ["toggle_fullscreen"]);
        // A user map that differs in fullscreen is listed there.
        let home = fixtures().join("user").to_string_lossy().into_owned();
        let env = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| home.clone());
        let (s, _, _, _) = read("2026.10.23", &env, false, Path::new("/nonexistent"));
        assert_eq!(find(&s, "fullscreen", "w"), ["scroll up"]);
        assert_eq!(find(&s, "normal", "w"), ["toggle_single_page_mode"]);
    }

    #[test]
    fn files_in_zathuras_order() {
        let tmp = std::env::temp_dir().join(format!("kbf-zathura-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for (dir, line) in [
            ("a", "map <F4> quit\nmap <F5> quit"),
            ("b", "map <F4> follow\nmap <F6> follow"),
            (
                "etc",
                "map <F5> goto top\nmap <F6> goto top\nmap <F7> goto top",
            ),
            ("home/.config/zathura", "map <F7> recolor"),
            (
                "home/.var/app/org.pwmt.zathura/config/zathura",
                "map <F8> reload",
            ),
        ] {
            std::fs::create_dir_all(tmp.join(dir)).unwrap();
            std::fs::write(tmp.join(dir).join("zathurarc"), line).unwrap();
        }
        let t = |p: &str| tmp.join(p).to_string_lossy().into_owned();
        let vars = [
            ("XDG_CONFIG_DIRS", format!("{}:{}", t("a"), t("b"))),
            ("HOME", t("home")),
            ("XDG_CONFIG_HOME", String::new()),
        ];
        let env = move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        // b, then a, then /etc, then the user's: each later file wins.
        let (s, _, _, _) = read("2026.10.23", &env, false, &tmp.join("etc"));
        assert_eq!(find(&s, "normal", "F4"), ["quit"]);
        // /etc after every XDG directory.
        assert_eq!(find(&s, "normal", "F5"), ["goto top"]);
        assert_eq!(find(&s, "normal", "F6"), ["goto top"]);
        assert_eq!(find(&s, "normal", "F7"), ["recolor"]);
        assert!(find(&s, "normal", "F8").is_empty());
        // 0.5.2 reads /etc/zathurarc too (`GLOBAL_RC`).
        let (s, _, _, _) = read("0.5.2", &env, false, &tmp.join("etc"));
        assert_eq!(find(&s, "normal", "F6"), ["goto top"]);
        // The Flatpak's class is native zathura's too: its native file when there is one (found by an e2e run:
        // native zathura 2026.10.4's windows are `org.pwmt.zathura`), else the Flatpak's.
        assert!(is_flatpak(Some("org.pwmt.zathura")) && is_flatpak(Some("ORG.PWMT.ZATHURA")));
        assert!(!is_flatpak(Some("Zathura")) && !is_flatpak(None));
        let (s, _, _, _) = read("2026.10.23", &env, true, &tmp.join("etc"));
        assert_eq!(find(&s, "normal", "F7"), ["recolor"]);
        assert!(find(&s, "normal", "F8").is_empty());
        std::fs::rename(
            tmp.join("home/.config/zathura/zathurarc"),
            tmp.join("home/.config/zathura/zathurarc.away"),
        )
        .unwrap();
        let (s, _, _, _) = read("2026.10.23", &env, true, &tmp.join("etc"));
        assert_eq!(find(&s, "normal", "F8"), ["reload"]);
        assert_eq!(find(&s, "normal", "F7"), ["goto top"]);
        std::fs::rename(
            tmp.join("home/.config/zathura/zathurarc.away"),
            tmp.join("home/.config/zathura/zathurarc"),
        )
        .unwrap();
        // An absolute include, and an unclosed quote: the rest of that file isn't read, as in zathura.
        std::fs::write(
            tmp.join("home/.config/zathura/zathurarc"),
            format!(
                "include {}\nmap <F9> quit \"unclosed\nmap <F10> quit\n",
                tmp.join("a/zathurarc").display()
            ),
        )
        .unwrap();
        let only_user = {
            let home = t("home");
            move |k: &str| match k {
                "HOME" => Some(home.clone()),
                "XDG_CONFIG_DIRS" => Some("/nonexistent".into()),
                _ => None,
            }
        };
        let (s, _, _, unsplit) = read("2026.10.23", &only_user, false, Path::new("/nonexistent"));
        assert_eq!(find(&s, "normal", "F5"), ["quit"]);
        assert!(find(&s, "normal", "F9").is_empty() && find(&s, "normal", "F10").is_empty());
        assert_eq!(unsplit, 1);
        // An unreadable user file is passed over silently; a system one is a note.
        use std::os::unix::fs::PermissionsExt;
        let user = tmp.join("home/.config/zathura/zathurarc");
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(
            tmp.join("etc/zathurarc"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        if std::fs::read(&user).is_err() {
            let (_, notes, _, _) = read("2026.10.23", &env, false, &tmp.join("etc"));
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].contains("etc/zathurarc"), "{notes:?}");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn splitting() {
        let s = |l: &str| split(l).map(|w| w.join("|"));
        assert_eq!(
            s("map <C-o> focus_inputbar \":open \"").as_deref(),
            Some("map|<C-o>|focus_inputbar|:open ")
        );
        assert_eq!(
            s("map x exec 'a b' # comment").as_deref(),
            Some("map|x|exec|a b")
        );
        assert_eq!(
            s(r#"map x exec "say \"hi\"""#).as_deref(),
            Some(r#"map|x|exec|say "hi""#)
        );
        assert_eq!(s(r"map a\ b quit").as_deref(), Some("map|a b|quit"));
        assert_eq!(s("map x 'open").as_deref(), None);
        assert_eq!(s("map x quit \\").as_deref(), None);
        // After a tab `#` is a word; with an apostrophe it's an unclosed quote, as in GLib.
        assert_eq!(s("map x quit\t#c").as_deref(), Some("map|x|quit|#c"));
        assert_eq!(s("map x quit\t# don't").as_deref(), None);
        assert_eq!(s("map a\\ #b quit").as_deref(), Some("map|a "));
        assert_eq!(s("map\x0bx quit").as_deref(), Some("map\x0bx|quit"));
        assert_eq!(s("  ").as_deref(), Some(""));
    }

    #[test]
    fn keys() {
        let keys = pinned("2026.10.23").keys;
        for (zathura, want) in [
            ("a", Some("a")),
            ("J", Some("shift+j")),
            ("/", Some("slash")),
            ("?", Some("question")),
            ("<C-r>", Some("ctrl+r")),
            ("<A-Right>", Some("alt+Right")),
            ("<M-k>", Some("alt+k")),
            ("<S-Space>", Some("shift+space")),
            ("<KPUp>", Some("KP_Up")),
            ("<F11>", Some("F11")),
            ("<Esc>", Some("Escape")),
            ("<Return>", Some("Return")),
            ("<BackSpace>", Some("BackSpace")),
            ("<Tab>", Some("Tab")),
            ("<PageUp>", Some("Page_Up")),
            ("<C-J>", Some("ctrl+shift+j")),
            ("gg", Some("g g")),
            ("zO", Some("z shift+o")),
            ("<Button1>", None),
            ("<scroll_up>", None),
            ("<C-S-x>", None),
            ("<c-x>", None),
            ("<Nope>", None),
            ("é", None),
        ] {
            let got = match parse_key(zathura, &keys) {
                Parsed::Key(k) => Some(k),
                _ => None,
            };
            assert_eq!(got.as_deref(), want, "{zathura}");
        }
        assert!(matches!(parse_key("<Button1>", &keys), Parsed::Mouse));
        // girara 0.3.7 has no keypad names.
        assert!(matches!(
            parse_key("<KPUp>", &pinned("0.5.2").keys),
            Parsed::Unreadable
        ));
    }

    /// A stand-in `zathura` printing `out`, in its own directory; returns the directory.
    fn fake_zathura(name: &str, out: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!("kbf-zathura-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process: a write handle another test thread's fork inherited would make
        // running the script fail with ETXTBSY (as `plugins.rs`'s `write_script` says).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("zathura"))
            .arg(format!(
                "[ \"$*|$LC_ALL\" = '--version|C' ] || exit 9; printf '{out}'"
            ))
            .status()
            .unwrap();
        assert!(ok.success());
        bin
    }

    #[test]
    fn whole_sheet_and_failures() {
        let path = |bin: &Path| {
            let bin = bin.to_path_buf();
            move |c: &mut Command| {
                c.env("PATH", &bin);
            }
        };
        let none = |k: &str| (k == "XDG_CONFIG_DIRS").then(|| "/nonexistent".to_string());
        let bin = fake_zathura("ok", "zathura 0.5.11\\ngirara 0.4.5\\n");
        let (s, notes) = sheet_with(
            REGISTRY,
            path(&bin),
            &none,
            false,
            Path::new("/nonexistent"),
        )
        .unwrap();
        assert_eq!(s.title, "zathura");
        assert_eq!(find(&s, "normal", "ctrl+q"), ["quit"]);
        assert!(notes.is_empty(), "{notes:?}");
        let _ = std::fs::remove_dir_all(&bin);
        // A zathura printing nothing: the newest, with a note.
        let bin = fake_zathura("silent", "");
        let (_, notes) = sheet_with(
            REGISTRY,
            path(&bin),
            &none,
            false,
            Path::new("/nonexistent"),
        )
        .unwrap();
        assert_eq!(
            notes,
            ["zathura's version is unknown: using the defaults of 2026.10.23"]
        );
        // A malformed pinned file: an error naming it.
        let err = sheet_with(
            &[("2026.10.23", "bind = [")],
            path(&bin),
            &none,
            false,
            Path::new("/x"),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("data/zathura/2026.10.23.toml"),
            "{err:#}"
        );
        let _ = std::fs::remove_dir_all(&bin);
        // No zathura: an error.
        let empty = std::env::temp_dir().join(format!("kbf-zathura-none-{}", std::process::id()));
        let err = sheet_with(REGISTRY, path(&empty), &none, false, Path::new("/x")).unwrap_err();
        assert!(
            format!("{err:#}").contains("running zathura --version"),
            "{err:#}"
        );
    }

    #[test]
    fn real_zathura_version() {
        let ok = Command::new("zathura")
            .arg("--version")
            .env("LC_ALL", "C")
            .output()
            .ok()
            .filter(|o| o.status.success());
        let Some(out) = ok else {
            eprintln!("skipping: zathura --version doesn't run here");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let v = version_of(&text).unwrap_or_else(|| panic!("no version in {text:?}"));
        assert!(pinned::select("zathura", REGISTRY, Some(&v)).is_some());
    }
}
