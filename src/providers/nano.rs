//! nano: its main-menu keys, from defaults pinned per nano version (`data/nano/<version>.toml`, from nano's
//! `shortcut_init()` at that tag) with the nanorc files' `bind` and `unbind` lines laid over them. Shown when nano
//! or rnano is the program in the focused terminal.

use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One entry per nano version whose defaults we know, oldest first. Supporting a new nano: add its file and a line.
const REGISTRY: &[(&str, &str)] = &[
    (
        "7.2",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/nano/7.2.toml")),
    ),
    (
        "8.4",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/nano/8.4.toml")),
    ),
    (
        "9.2",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/nano/9.2.toml")),
    ),
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pinned {
    nano_version: String,
    source: String,
    bind: Vec<PinnedBind>,
    /// Every function `bind` takes in the main menu, with its description.
    functions: BTreeMap<String, String>,
    other_menus: OtherMenus,
}

/// The functions `bind` takes in other menus only.
#[derive(Deserialize)]
struct OtherMenus {
    functions: Vec<String>,
}

#[derive(Deserialize, Clone)]
struct PinnedBind {
    key: String,
    function: String,
    desc: String,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(REGISTRY, |_| {}, Path::new("/etc/nanorc"), &env)?;
    for note in notes {
        eprintln!("keybind-find: nano: {note}");
    }
    Ok(sheet)
}

/// `sheet` over `registry`, with `setup` applied to `nano --version` (a test seam: `PATH`).
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    system: &Path,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut cmd = Command::new("nano");
    cmd.arg("--version")
        // The version line is translated.
        .env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let version = run_for(cmd, VERSION_TIMEOUT).context("running nano --version")?;
    read(registry, version_of(&version).as_deref(), system, env)
}

/// The version in `nano --version`'s first line: its first number with a dot (` GNU nano, version 8.4`,
/// ` GNU nano from git, v8.4-12-g…`).
fn version_of(text: &str) -> Option<String> {
    let line = text.lines().next()?;
    line.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|w| w.trim_matches('.'))
        .find(|w| w.contains('.') && parse_version(w).is_some())
        .map(str::to_string)
}

fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|p| p.parse().ok()).collect()
}

/// The pinned file for `installed`: its own silently; else, with a note, the nearest of the same major version
/// (older first: nano 8 moved `^F`/`^B`), else the newest older one, else the oldest; the newest when the version is
/// unknown.
fn select(
    registry: &[(&'static str, &'static str)],
    installed: Option<&str>,
) -> Option<(&'static str, &'static str, Option<String>)> {
    let mut entries: Vec<(Vec<u64>, &'static str, &'static str)> = registry
        .iter()
        .filter_map(|&(v, t)| Some((parse_version(v)?, v, t)))
        .collect();
    entries.sort();
    let &(_, newest, newest_text) = entries.last()?;
    let Some(wanted) = installed.and_then(parse_version) else {
        let note = format!("nano's version is unknown: using the defaults of {newest}");
        return Some((newest, newest_text, Some(note)));
    };
    if let Some(&(_, v, t)) = entries.iter().find(|(p, _, _)| *p == wanted) {
        return Some((v, t, None));
    }
    let same = |p: &Vec<u64>| p.first() == wanted.first();
    let older = entries.iter().rev().find(|(p, _, _)| *p < wanted);
    let &(_, v, t) = older
        .filter(|(p, _, _)| same(p))
        .or_else(|| entries.iter().find(|(p, _, _)| same(p) && *p > wanted))
        .or(older)
        .or(entries.first())?;
    let i = installed.unwrap_or_default();
    Some((
        v,
        t,
        Some(format!(
            "nano {i}: using the defaults of {v}; add data/nano/{i}.toml"
        )),
    ))
}

/// The sheet for nano `installed`: the pinned defaults, then `/etc/nanorc` (`system`) and the user's nanorc found
/// through `env`. Also the lines for stderr.
fn read(
    registry: &[(&'static str, &'static str)],
    installed: Option<&str>,
    system: &Path,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let (version, text, note) =
        select(registry, installed).context("no nano defaults are built in")?;
    let pinned: Pinned =
        toml::from_str(text).with_context(|| format!("data/nano/{version}.toml"))?;
    if pinned.nano_version != version || pinned.source.is_empty() {
        bail!(
            "data/nano/{version}.toml names nano {} from {:?}",
            pinned.nano_version,
            pinned.source
        );
    }
    let mut notes: Vec<String> = note.into_iter().collect();
    // The main menu: key identity → bind, in the defaults' order, user additions after.
    let mut keys: Vec<(String, PinnedBind)> = pinned
        .bind
        .iter()
        .map(|b| (identity(&b.key), b.clone()))
        .collect();
    let modern = parse_version(version).is_some_and(|v| v >= vec![9]);
    for path in [Some(system.to_path_buf()), user_nanorc(env)]
        .into_iter()
        .flatten()
    {
        match std::fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                apply(&mut keys, &pinned, modern, &text, &path, env, &mut notes);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => notes.push(format!(
                "can't read {}: {e}; showing nano's defaults for it",
                path.display()
            )),
        }
    }
    let mut sheet = Sheet {
        title: format!("nano {version}"),
        provider: "nano".into(),
        binds: Default::default(),
    };
    let mut unknown = Vec::new();
    for (_, b) in keys {
        let Some(key) = notation(&b.key) else {
            unknown.push(b.key);
            continue;
        };
        sheet.binds.entry("main".into()).or_default().push(Bind {
            key,
            desc: b.desc,
            action: b.function,
            when: Default::default(),
        });
    }
    if !unknown.is_empty() {
        notes.push(format!(
            "skipping keys with no keysym we know: {}",
            unknown.join(" ")
        ));
    }
    Ok((sheet, notes))
}

/// The user's nanorc, as nano picks it: the first readable of `~/.nanorc`, `$XDG_CONFIG_HOME/nano/nanorc` and
/// `~/.config/nano/nanorc`.
fn user_nanorc(env: Env) -> Option<PathBuf> {
    let home = env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let xdg = env("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    [
        home.as_ref().map(|h| h.join(".nanorc")),
        xdg.map(|d| d.join("nano/nanorc")),
        home.as_ref().map(|h| h.join(".config/nano/nanorc")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file() && std::fs::File::open(p).is_ok())
}

/// Lays one nanorc's `bind` and `unbind` lines for the main menu (`main` or `all`) over `keys`, as nano does:
/// later lines win; a key name nano doesn't take, or a function it doesn't know, changes nothing; a function of
/// another menu, or a string (a macro), takes the key away from the main menu (the macro isn't listed). Words after
/// the menu are ignored. `include` files are checked for a note when missing, but nano reads only syntax definitions
/// from them (it refuses `bind` there), so their lines change no key.
fn apply(
    keys: &mut Vec<(String, PinnedBind)>,
    pinned: &Pinned,
    modern: bool,
    text: &str,
    file: &Path,
    env: Env,
    notes: &mut Vec<String>,
) {
    for line in text.lines() {
        let words = split(line);
        match words.as_slice() {
            [cmd, pattern, ..] if cmd == "include" => {
                if included(pattern, env) == Some(false) {
                    notes.push(format!(
                        "{}: no file matches include {pattern}",
                        file.display()
                    ));
                }
            }
            [cmd, key, function, menu, ..]
                if cmd == "bind" && main(menu) && valid_key(key, modern) =>
            {
                let id = identity(key);
                if function.starts_with('"') {
                    keys.retain(|(k, _)| *k != id);
                } else if let Some(desc) = pinned.functions.get(function.as_str()) {
                    keys.retain(|(k, _)| *k != id);
                    let bind = PinnedBind {
                        key: key.clone(),
                        function: function.clone(),
                        desc: desc.clone(),
                    };
                    keys.push((id, bind));
                } else if pinned.other_menus.functions.contains(function) {
                    keys.retain(|(k, _)| *k != id);
                }
            }
            [cmd, key, menu, ..] if cmd == "unbind" && main(menu) && valid_key(key, modern) => {
                let id = identity(key);
                keys.retain(|(k, _)| *k != id);
            }
            _ => {}
        }
    }
}

fn main(menu: &str) -> bool {
    menu == "main" || menu == "all"
}

/// A nanorc line's words, split on blanks, as nano's `parse_argument` reads them: a word starting with `"` runs to
/// the line's last `"` (a macro string); a `#` at the start ends the line.
fn split(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut rest = line.trim();
    if rest.starts_with('#') {
        return words;
    }
    while !rest.is_empty() {
        let end = if rest.starts_with('"') {
            rest.rfind('"')
                .filter(|&e| e > 0)
                .map_or(rest.len(), |e| e + 1)
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };
        words.push(rest[..end].to_string());
        rest = rest[end..].trim_start();
    }
    words
}

/// Whether an `include` pattern matches a file: quotes and a leading `~/` undone; a pattern with a `*` in its file
/// name matched against the directory's entries (not hidden ones). `None`: a pattern this doesn't check (`?`, `[`,
/// a `*` in a directory), so no note is given.
fn included(pattern: &str, env: Env) -> Option<bool> {
    let p = pattern.trim_matches('"');
    let p = match (p.strip_prefix("~/"), env("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(p),
    };
    let name = p.file_name()?.to_string_lossy().into_owned();
    let dir = p.parent().unwrap_or(Path::new("."));
    if dir.to_string_lossy().contains(['*', '?', '[']) || name.contains(['?', '[']) {
        return None;
    }
    if !name.contains('*') {
        return Some(p.is_file());
    }
    let parts: Vec<&str> = name.split('*').collect();
    let matches = |n: &str| {
        let mut rest = n;
        for (i, part) in parts.iter().enumerate() {
            match rest.find(part) {
                Some(at) if i > 0 || at == 0 => rest = &rest[at + part.len()..],
                _ => return false,
            }
        }
        parts.last().is_none_or(|l| n.ends_with(l))
    };
    Some(
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .any(|n| !n.starts_with('.') && matches(&n)),
    )
}

/// The named keys nano's `bind` takes, in its spelling (matched ignoring case).
const NAMES: [&str; 3] = ["Space", "Ins", "Del"];

/// Whether `bind` and `unbind` take `key`, as nano's `keycode_from_string`: `^c`, `^Space`, `M-c`, `M-Space`,
/// `Sh-M-` and a letter, `F1`-`F24`, `Ins`, `Del`; from nano 9, `M-` with `Left`, `Right`, `Up`, `Down`, `Ins`,
/// `Del` too. Case doesn't count.
fn valid_key(key: &str, modern: bool) -> bool {
    let lower = key.to_ascii_lowercase();
    let one = |r: &str| r.chars().count() == 1;
    if let Some(r) = lower.strip_prefix("sh-m-") {
        return one(r) && r.chars().all(|c| c.is_ascii_alphabetic());
    }
    if let Some(r) = lower.strip_prefix('^') {
        // `^@`…`^_` with the letters, `` ^` ``, `^/`, `^-`; not `^[` (Escape, which `bind` refuses).
        return r == "space"
            || matches!(
                r.as_bytes(),
                [b'/' | b'-' | b'`' | b'@' | b'\\' | b']' | b'^' | b'_' | b'a'..=b'z']
            );
    }
    if let Some(r) = lower.strip_prefix("m-") {
        return one(r)
            || r == "space"
            || (modern && ["left", "right", "up", "down", "ins", "del"].contains(&r));
    }
    lower
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
        || lower == "ins"
        || lower == "del"
}

/// A nano key's identity: its spelling with the modifiers and a letter's case normalised (nano reads `^x` as `^X`,
/// `M-x` as `M-X`, `m-x` and `sh-m-x` too), and spellings of one keycode made one (`^I` is `Tab`, `^/` and `^-` are
/// `^_`, `` ^` `` and `^Space` are `^@`).
fn identity(key: &str) -> String {
    let (prefix, rest) = split_mods(key);
    let rest = if rest.chars().count() == 1 {
        rest.to_ascii_uppercase()
    } else {
        NAMES
            .iter()
            .chain(&[
                "Left", "Right", "Up", "Down", "Tab", "Enter", "Bsp", "Home", "End", "PgUp", "PgDn",
            ])
            .find(|n| n.eq_ignore_ascii_case(rest))
            .map_or_else(|| rest.to_string(), |n| n.to_string())
    };
    let id = format!("{prefix}{rest}");
    match id.as_str() {
        "^I" => "Tab".into(),
        "^/" | "^-" => "^_".into(),
        "^`" | "^Space" => "^@".into(),
        _ => id,
    }
}

/// The modifier prefixes (`Sh-`, `M-`, `^`, in any order and any case) as `Sh-`, `M-`, `^`, and the key after them.
fn split_mods(key: &str) -> (String, &str) {
    let mut prefix = String::new();
    let mut rest = key;
    loop {
        let lower = rest.to_ascii_lowercase();
        if lower.starts_with("sh-") && rest.len() > 3 {
            prefix.push_str("Sh-");
            rest = &rest[3..];
        } else if lower.starts_with("m-") && rest.len() > 2 {
            prefix.push_str("M-");
            rest = &rest[2..];
        } else if rest.starts_with('^') && rest.len() > 1 {
            prefix.push('^');
            rest = &rest[1..];
        } else {
            return (prefix, rest);
        }
    }
}

/// A nano key in the key notation: `^X` → `ctrl+x`, `M-x` → `alt+x`, `Sh-M-x` → `alt+shift+x`, `^Del` →
/// `ctrl+Delete`, `F1`, `Ins` → `Insert`. `None`: no keysym we know.
fn notation(key: &str) -> Option<String> {
    let (prefix, rest) = split_mods(key);
    let mut mods = Vec::new();
    if prefix.contains("Sh-") {
        mods.push("shift");
    }
    if prefix.contains("M-") {
        mods.push("alt");
    }
    if prefix.contains('^') {
        mods.push("ctrl");
    }
    let named = match rest.to_ascii_lowercase().as_str() {
        "bsp" => "BackSpace",
        "del" => "Delete",
        "ins" => "Insert",
        "enter" => "Return",
        "tab" => "Tab",
        "space" => "space",
        "home" => "Home",
        "end" => "End",
        "pgup" => "Prior",
        "pgdn" => "Next",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        _ => "",
    };
    let keysym = if !named.is_empty() {
        named.to_string()
    } else if let Some(n) = rest
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        format!("F{n}")
    } else {
        let mut chars = rest.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        // A letter with ctrl or alt is case-blind in nano; alone, a capital is a shifted letter.
        let c = if mods.is_empty() {
            c
        } else {
            c.to_ascii_lowercase()
        };
        if c.is_ascii_uppercase() {
            mods.push("shift");
        }
        super::nvim::single(c.to_ascii_lowercase())?.to_string()
    };
    Some(chord(&mods, &keysym))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nano")
    }

    fn pinned(version: &str) -> Pinned {
        let (_, text) = REGISTRY.iter().find(|(v, _)| *v == version).unwrap();
        toml::from_str(text).unwrap()
    }

    fn function(version: &str, key: &str) -> Option<String> {
        pinned(version)
            .bind
            .into_iter()
            .find(|b| b.key == key)
            .map(|b| b.function)
    }

    #[test]
    fn pinned_files() {
        for (version, text) in REGISTRY {
            let p: Pinned = toml::from_str(text).unwrap();
            assert_eq!(p.nano_version, *version);
            assert!(p.source.starts_with("nano@"), "{version}");
            assert!(
                text.contains(&format!("tag v{version} (commit ")),
                "{version}: names its tag and commit"
            );
            let mut seen = std::collections::HashSet::new();
            for b in &p.bind {
                // nano lists some keycodes under two spellings (`^I` and `Tab`); a spelling only once.
                assert!(seen.insert(b.key.clone()), "{version}: {} twice", b.key);
                assert!(notation(&b.key).is_some(), "{version}: {}", b.key);
                assert!(!b.desc.is_empty(), "{version}: {}", b.key);
                assert!(
                    p.functions.contains_key(&b.function),
                    "{version}: {}",
                    b.function
                );
            }
            // Keys every version has, by function (the extractor once lost `^T` and `M-9`).
            for (key, f) in [
                ("^X", "exit"),
                ("^O", "writeout"),
                ("^T", "execute"),
                ("^K", "cut"),
                ("M-9", "beginpara"),
                // nano's own names, the first of each pair in strtosc (`up`, not `prevline`).
                ("Up", "up"),
                ("PgUp", "pageup"),
            ] {
                assert_eq!(
                    function(version, key).as_deref(),
                    Some(f),
                    "{version} {key}"
                );
            }
            assert!(
                p.other_menus.functions.contains(&"cancel".to_string()),
                "{version}"
            );
        }
        // nano 8 moved ^F and ^B to searching.
        assert_eq!(function("7.2", "^F").as_deref(), Some("right"));
        assert_eq!(function("8.4", "^F").as_deref(), Some("whereis"));
        assert_eq!(function("8.4", "^B").as_deref(), Some("wherewas"));
    }

    #[test]
    fn version_pick() {
        let pick = |v: Option<&str>| select(REGISTRY, v).unwrap();
        assert_eq!(pick(Some("8.4")), ("8.4", REGISTRY[1].1, None));
        let (v, _, note) = pick(Some("8.6"));
        assert_eq!(v, "8.4");
        assert!(note.unwrap().contains("add data/nano/8.6.toml"));
        // 8.0-8.3 keep nano 8's keys: 8.4, not 7.2.
        assert_eq!(pick(Some("8.1")).0, "8.4");
        let (v, _, note) = pick(Some("10.0"));
        assert_eq!(v, "9.2");
        assert!(note.unwrap().contains("add data/nano/10.0.toml"));
        assert_eq!(pick(Some("7.0")).0, "7.2");
        assert_eq!(pick(Some("5.4")).0, "7.2");
        let (v, _, note) = pick(None);
        assert_eq!(v, "9.2");
        assert!(note.unwrap().contains("unknown"));
        assert!(select(&[], Some("8.4")).is_none());
        assert_eq!(
            version_of(" GNU nano, version 8.4\n (C) 2024").as_deref(),
            Some("8.4")
        );
        assert_eq!(
            version_of(" GNU nano from git, v8.4-12-gabc").as_deref(),
            Some("8.4")
        );
        assert_eq!(
            version_of("GNU nano, version 9.2.1").as_deref(),
            Some("9.2.1")
        );
        assert_eq!(version_of("something else"), None);
    }

    #[test]
    fn bad_registry_is_an_error() {
        let env = |_: &str| None;
        let err = read(
            &[("9.2", "not toml [")],
            Some("9.2"),
            Path::new("/nonexistent"),
            &env,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("data/nano/9.2.toml"), "{err:#}");
        let err = read(
            &[("9.2", REGISTRY[1].1)],
            Some("9.2"),
            Path::new("/nonexistent"),
            &env,
        )
        .unwrap_err();
        assert!(err.to_string().contains("names nano 8.4"), "{err}");
        assert!(read(&[], None, Path::new("/nonexistent"), &env).is_err());
    }

    #[test]
    fn no_nano_drops_the_provider() {
        let env = |_: &str| None;
        let err = sheet_with(
            REGISTRY,
            |c| {
                c.env("PATH", "/nonexistent-kbf");
            },
            Path::new("/x"),
            &env,
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains("running nano --version") && !err.contains('\n'),
            "{err}"
        );
    }

    #[test]
    fn key_notation_and_identity() {
        for (nano, want) in [
            ("^X", Some("ctrl+x")),
            ("^x", Some("ctrl+x")),
            ("M-x", Some("alt+x")),
            ("m-x", Some("alt+x")),
            ("Sh-M-x", Some("alt+shift+x")),
            ("sh-m-x", Some("alt+shift+x")),
            ("M-/", Some("alt+slash")),
            ("M-<", Some("alt+less")),
            ("M-^", Some("alt+asciicircum")),
            ("^^", Some("ctrl+asciicircum")),
            ("^Space", Some("ctrl+space")),
            ("^space", Some("ctrl+space")),
            ("Sh-^Del", Some("ctrl+shift+Delete")),
            ("M-Bsp", Some("alt+BackSpace")),
            ("PgUp", Some("Prior")),
            ("F1", Some("F1")),
            ("f13", Some("F13")),
            ("Ins", Some("Insert")),
            ("del", Some("Delete")),
            ("Enter", Some("Return")),
            ("F99", None),
            ("Nonesuch", None),
        ] {
            assert_eq!(notation(nano).as_deref(), want, "{nano}");
        }
        assert_eq!(identity("m-x"), identity("M-X"));
        assert_ne!(identity("Sh-M-x"), identity("M-x"));
        assert_eq!(identity("^I"), identity("Tab"));
        assert_eq!(identity("^/"), identity("^_"));
        assert_eq!(identity("^space"), identity("^@"));
        assert!(
            valid_key("^space", false) && valid_key("sh-m-x", false) && valid_key("f24", false)
        );
        assert!(
            !valid_key("Enter", true)
                && !valid_key("^Del", true)
                && !valid_key("Sh-M-1", true)
                && ["^1", "^?", "^{", "^["].iter().all(|k| !valid_key(k, true))
        );
        assert!(valid_key("M-Left", true) && !valid_key("M-Left", false));
        assert_eq!(
            split(r#"bind M-1 "a" b" main"#),
            ["bind", "M-1", r#""a" b""#, "main"]
        );
        assert_eq!(split("  # a comment"), Vec::<String>::new());
        assert_eq!(
            split("bind\t^S  savefile main"),
            ["bind", "^S", "savefile", "main"]
        );
    }

    /// The sheet for nano `version` with the fixture system nanorc and `home` as HOME (and `xdg` if any).
    fn run(version: &str, home: &Path, xdg: Option<&Path>) -> (Sheet, Vec<String>) {
        let (h, x) = (
            home.display().to_string(),
            xdg.map(|x| x.display().to_string()),
        );
        read(
            REGISTRY,
            Some(version),
            &fixtures().join("etc-nanorc"),
            &|k| match k {
                "HOME" => Some(h.clone()),
                "XDG_CONFIG_HOME" => x.clone(),
                _ => None,
            },
        )
        .unwrap()
    }

    fn get<'a>(s: &'a Sheet, key: &str) -> Option<&'a Bind> {
        s.binds.get("main")?.iter().find(|b| b.key == key)
    }

    #[test]
    fn nanorc_over_defaults() {
        let (s, notes) = run("9.2", &fixtures().join("home"), None);
        let action = |k: &str| get(&s, k).map(|b| b.action.clone());
        // /etc's own bind; a user bind replacing a default; `all` (in lower case) for main; the user's ^K over /etc's;
        // words after the menu ignored.
        assert_eq!(action("alt+2").as_deref(), Some("copy"));
        assert_eq!(action("ctrl+s").as_deref(), Some("writeout"));
        assert_eq!(action("alt+w").as_deref(), Some("whereis"));
        assert_eq!(action("ctrl+k").as_deref(), Some("cut"));
        assert_eq!(action("alt+shift+x").as_deref(), Some("copy"));
        assert_eq!(action("ctrl+l").as_deref(), Some("savefile"));
        // A function by its documented name; a `^` key nano doesn't have is ignored.
        assert_eq!(action("alt+k").as_deref(), Some("up"));
        assert!(get(&s, "ctrl+1").is_none());
        // The description is the function's own.
        assert_eq!(
            get(&s, "alt+shift+x").unwrap().desc,
            pinned("9.2").functions["copy"]
        );
        // unbind removes (`^_` is also `^/`); a macro and another menu's function take the key away.
        for gone in [
            "ctrl+j",
            "alt+u",
            "ctrl+t",
            "ctrl+underscore",
            "ctrl+slash",
            "alt+1",
        ] {
            assert!(get(&s, gone).is_none(), "{gone}");
        }
        // An unknown function, an invalid key name, another menu, `set`, syntax and color lines, and the included
        // files' binds change nothing.
        for (key, nano) in [
            ("ctrl+e", "^E"),
            ("ctrl+h", "^H"),
            ("ctrl+a", "^A"),
            ("ctrl+y", "^Y"),
            ("Return", "Enter"),
        ] {
            assert_eq!(action(key), function("9.2", nano), "{key}");
        }
        // A missing include is a note; a pattern this doesn't check (`[ab]`) is none.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("missing"), "{notes:?}");
        assert_eq!(s.title, "nano 9.2");
    }

    #[test]
    fn xdg_nanorc_and_no_nanorc() {
        let empty = std::env::temp_dir().join(format!("kbf-nano-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        // No ~/.nanorc: the XDG one.
        let (s, _) = run("9.2", &empty, Some(&fixtures().join("xdg")));
        assert_eq!(get(&s, "alt+3").unwrap().action, "writeout");
        // Neither, and no /etc file: the defaults alone, no notes.
        let e = empty.display().to_string();
        let (s, notes) = read(REGISTRY, Some("9.2"), &empty.join("nanorc"), &|k| {
            (k == "HOME").then(|| e.clone())
        })
        .unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.binds["main"].len(), pinned("9.2").bind.len());
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn unreadable_files() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("kbf-nano-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let rc = home.join(".nanorc");
        std::fs::write(&rc, "bind M-4 writeout main\n").unwrap();
        std::fs::set_permissions(&rc, std::fs::Permissions::from_mode(0o000)).unwrap();
        let etc = home.join("etc-nanorc");
        std::fs::write(&etc, "bind M-5 writeout main\n").unwrap();
        std::fs::set_permissions(&etc, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&rc).is_ok() {
            eprintln!("skipping: running as root, the files are readable");
            let _ = std::fs::remove_dir_all(&home);
            return;
        }
        // An unreadable ~/.nanorc is passed over for the XDG one, as nano does; an unreadable /etc file is a note.
        let (h, x) = (
            home.display().to_string(),
            fixtures().join("xdg").display().to_string(),
        );
        let (s, notes) = read(REGISTRY, Some("9.2"), &etc, &|k| match k {
            "HOME" => Some(h.clone()),
            "XDG_CONFIG_HOME" => Some(x.clone()),
            _ => None,
        })
        .unwrap();
        assert_eq!(get(&s, "alt+3").unwrap().action, "writeout");
        assert!(get(&s, "alt+4").is_none() && get(&s, "alt+5").is_none());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("can't read") && notes[0].contains("etc-nanorc"),
            "{notes:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn real_nano_version() {
        let Ok(out) = Command::new("nano")
            .arg("--version")
            .env("LC_ALL", "C")
            .output()
        else {
            eprintln!("skipping: nano is not installed");
            return;
        };
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(version_of(&text).is_some(), "{text}");
    }
}
