//! yazi: its keys from the default keymap pinned per version (`data/yazi/`), with the user's `keymap.toml` merged
//! over it as yazi merges it. Shown when yazi runs in the focused terminal. `run` text is shown, never run.

use super::nvim::single;
use super::pinned::{self, parse_version};
use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const REGISTRY: &[(&str, &str)] = &[
    (
        "25.4.8",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/yazi/25.4.8.toml"
        )),
    ),
    (
        "25.5.31",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/yazi/25.5.31.toml"
        )),
    ),
    (
        "26.9.1",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/yazi/26.9.1.toml"
        )),
    ),
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A keymap file: sections by name (`mgr`, `input`, …).
type Keymap = BTreeMap<String, Section>;

#[derive(Deserialize)]
struct Section {
    keymap: Option<Vec<Entry>>,
    #[serde(default)]
    prepend_keymap: Vec<Entry>,
    #[serde(default)]
    append_keymap: Vec<Entry>,
}

/// One key (yazi's `Chord`): `on` and `run` are required, as yazi requires them.
#[derive(Deserialize, Clone)]
struct Entry {
    on: OneOrMany,
    run: OneOrMany,
    #[serde(default)]
    desc: String,
    #[serde(rename = "for")]
    platform: Option<String>,
}

#[derive(Deserialize, Clone)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn items(&self) -> Vec<&str> {
        match self {
            OneOrMany::One(s) => vec![s.as_str()],
            OneOrMany::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(REGISTRY, |_| {}, &env)?;
    if !notes.is_empty() {
        eprintln!("keybind-find: yazi: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `sheet` over `registry`, with `setup` applied to `yazi --version` (a test seam: `PATH`) and the config dir found
/// through `env`.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut cmd = Command::new("yazi");
    cmd.arg("--version")
        .env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let version = run_for(cmd, VERSION_TIMEOUT).context("running yazi --version")?;
    let (v, text, note) = pinned::select("yazi", registry, version_of(&version).as_deref())
        .context("no pinned yazi keymap")?;
    let mut notes: Vec<String> = note.into_iter().collect();
    let defaults = keymap(text).with_context(|| format!("reading data/yazi/{v}.toml"))?;
    let names: Vec<String> = defaults.keys().cloned().collect();
    let user = keymap_file(env).and_then(|path| match read_user(&path, v, &names) {
        Ok((user, unknown)) => {
            if !unknown.is_empty() {
                notes.push(format!(
                    "keymap.toml's [{}] isn't a section of yazi {v}'s keymap; ignored, as yazi does",
                    unknown.join("], [")
                ));
            }
            Some(user)
        }Err(e) => {
            // On one line: a TOML error spans several.
            let e = format!("{e:#}")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            notes.push(format!("{e}; showing yazi's defaults"));
            None
        }
    });
    let (sheet, left_out) = merge(v, defaults, user.unwrap_or_default());
    if left_out > 0 {
        let keys = if left_out == 1 { "key" } else { "keys" };
        notes.push(format!(
            "leaving out {left_out} {keys} with no keysym we know"
        ));
    }
    Ok((sheet, notes))
}

/// The first dotted number in `yazi --version` (`    Version: 26.9.1 (Arch Linux …)` on its second line from 25.x;
/// `Yazi 0.4.2 (…)` before).
fn version_of(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|w| w.trim_matches('.'))
        .find(|w| w.contains('.') && parse_version(w).is_some())
        .map(str::to_string)
}

/// `keymap.toml` in yazi's config dir: `$YAZI_CONFIG_HOME`, else `$XDG_CONFIG_HOME/yazi`, else `~/.config/yazi`
/// (an empty or relative value counts as unset); `None` when there's no file (not an error, as in yazi).
fn keymap_file(env: Env) -> Option<PathBuf> {
    let abs = |k: &str| env(k).map(PathBuf::from).filter(|p| p.is_absolute());
    let dir = abs("YAZI_CONFIG_HOME")
        .or_else(|| abs("XDG_CONFIG_HOME").map(|d| d.join("yazi")))
        .or_else(|| abs("HOME").map(|h| h.join(".config/yazi")))?;
    Some(dir.join("keymap.toml")).filter(|f| f.exists())
}

/// A keymap file's sections: its tables (a `"$schema" = …` line beside them is skipped, as yazi skips it).
fn keymap(text: &str) -> anyhow::Result<Keymap> {
    let table: toml::Table = toml::from_str(text)?;
    table
        .into_iter()
        .filter(|(k, v)| v.is_table() || !k.starts_with('$'))
        .map(|(k, v)| Ok((k.clone(), v.try_into().with_context(|| format!("[{k}]"))?)))
        .collect()
}

/// The user's keymap (`sections`: those of the pin), and the other tables in it; refused whole (as yazi refuses it) when it isn't TOML or an entry lacks `on` or `run`.
fn read_user(
    path: &std::path::Path,
    v: &str,
    sections: &[String],
) -> anyhow::Result<(Keymap, Vec<String>)> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let table: toml::Table =
        toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    // Only the pin's sections are read; anything else yazi skips unread (a table is named in a note).
    let mut keymap = Keymap::new();
    let mut unknown = Vec::new();
    for (k, value) in table {
        if !sections.contains(&k) {
            if value.is_table() {
                unknown.push(k);
            }
            continue;
        }
        let section = value
            .try_into()
            .with_context(|| format!("reading {}: [{k}]", path.display()))?;
        keymap.insert(k, section);
    }
    let all = keymap.values().flat_map(|s| {
        s.keymap
            .iter()
            .flatten()
            .chain(&s.prepend_keymap)
            .chain(&s.append_keymap)
    });
    for e in all {
        if e.on.items().is_empty() || e.run.items().is_empty() {
            anyhow::bail!("{}: an entry with an empty `on` or `run`", path.display());
        }
        if let Some(k) = e.on.items().into_iter().find(|k| !yazi_reads(k)) {
            anyhow::bail!("{}: a key yazi refuses: {k:?}", path.display());
        }
        if platform_kept(v, e.platform.as_deref()).is_none() {
            anyhow::bail!(
                "{}: unknown `for` {:?}",
                path.display(),
                e.platform.as_deref().unwrap_or_default()
            );
        }
    }
    Ok((keymap, unknown))
}

/// Whether an entry with `for` = `platform` is kept on Linux by the yazi of pin `v`: through 25.x, `for` is a string
/// that keeps only `linux` and `unix` (any other is dropped); from 26.x an enum, where `all` is kept too and a name
/// it doesn't have refuses the file (`None`).
// ponytail: the enum's version is taken as 26 by the pins; between 25.5.31 and 26.9.1 nothing marks it.
fn platform_kept(v: &str, platform: Option<&str>) -> Option<bool> {
    let enum_ = parse_version(v).is_some_and(|p| p.first().is_some_and(|y| *y >= 26));
    match platform {
        None | Some("linux" | "unix") => Some(true),
        Some("all") if enum_ => Some(true),
        Some("macos" | "windows" | "android") if enum_ => Some(false),
        Some(_) if enum_ => None,
        Some(_) => Some(false),
    }
}

/// Whether yazi's `Key::from_str` takes `key` (else it refuses the whole file): not empty, and inside `<…>` only
/// `S-` `C-` `A-` `D-` before the last part, which isn't one of them. Any last part is a key to yazi (`<Foo>` is its
/// `F`); `notation` is stricter, and leaves out what it can't name.
fn yazi_reads(key: &str) -> bool {
    let Some(inner) = key.strip_prefix('<').and_then(|k| k.strip_suffix('>')) else {
        return !key.is_empty();
    };
    let parts: Vec<String> = inner
        .split_inclusive('-')
        .map(str::to_ascii_lowercase)
        .collect();
    let is_mod = |p: &str| matches!(p, "s-" | "c-" | "a-" | "d-");
    match parts.split_last() {
        Some((last, before)) => !is_mod(last) && before.iter().all(|p| is_mod(p)),
        None => false,
    }
}

/// The sheet of `defaults` with `user` merged over it per section (yazi's `deserialize_over_hook`): `keymap`
/// replaces the defaults; `prepend_keymap` goes first and drops each default with the same first two keys;
/// `append_keymap` goes last, less those whose first two keys a default has. Then, as yazi, entries for another
/// platform and `noop` ones are dropped, and of two with the same keys the first stays. Also returns how many keys
/// had no notation.
fn merge(v: &str, defaults: Keymap, mut user: Keymap) -> (Sheet, usize) {
    let mut sheet = Sheet {
        title: "yazi".into(),
        provider: "yazi".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;

    let two = |e: &Entry| {
        let keys: Vec<String> = e.on.items().iter().map(|k| same_key(k)).collect();
        [keys.first().cloned(), keys.get(1).cloned()]
    };
    for (name, section) in defaults {
        let mut base = section.keymap.unwrap_or_default();
        let (prepend, append) = match user.remove(&name) {
            Some(u) => {
                if let Some(keymap) = u.keymap {
                    base = keymap;
                }
                (u.prepend_keymap, u.append_keymap)
            }
            None => Default::default(),
        };
        let first: Vec<_> = prepend.iter().map(two).collect();
        let kept: Vec<_> = base.iter().map(two).collect();
        let entries: Vec<Entry> = prepend
            .iter()
            .cloned()
            .chain(base.iter().filter(|e| !first.contains(&two(e))).cloned())
            .chain(append.iter().filter(|e| !kept.contains(&two(e))).cloned())
            .filter(|e| platform_kept(v, e.platform.as_deref()) == Some(true))
            .filter(|e| !matches!(e.run.items()[..], [run] if run.trim() == "noop"))
            .collect();
        let mut seen: Vec<Vec<String>> = Vec::new();
        for e in entries {
            let on: Vec<String> = e.on.items().iter().map(|k| same_key(k)).collect();
            if seen.contains(&on) {
                continue;
            }
            seen.push(on);
            let Some(key) =
                e.on.items()
                    .iter()
                    .map(|k| notation(k))
                    .collect::<Option<Vec<_>>>()
            else {
                left_out += 1;
                continue;
            };
            let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
            let action = collapse(&e.run.items().join("; "));
            let desc = Some(collapse(&e.desc))
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| action.clone());
            sheet.binds.entry(name.clone()).or_default().push(Bind {
                key: key.join(" "),
                desc,
                action,
                when: Default::default(),
            });
        }
    }
    (sheet, left_out)
}

/// A key as yazi compares keys: its notation, or as written when it has none.
fn same_key(key: &str) -> String {
    notation(key).unwrap_or_else(|| key.to_string())
}

/// One yazi key (`q`, `Q`, `~`, `<C-A>`, `<S-Tab>`, `<PageDown>`) in keybind-find's notation; `None` for a name
/// yazi doesn't know. As yazi's `Key::from_str`: outside `<…>` the key is the first character; inside, `S-` `C-`
/// `A-` `D-` (any case) then a name or one character; an upper-case letter adds shift.
fn notation(key: &str) -> Option<String> {
    let mut mods = Vec::new();
    let Some(inner) = key.strip_prefix('<').and_then(|k| k.strip_suffix('>')) else {
        return char_key(key.chars().next()?, &mut mods).map(|k| chord(&mods, &k));
    };
    let mut parts = inner.split_inclusive('-').peekable();
    let mut name = None;
    while let Some(part) = parts.next() {
        let last = parts.peek().is_none();
        match part.to_ascii_lowercase().as_str() {
            "s-" if !last => mods.push("shift"),
            "c-" if !last => mods.push("ctrl"),
            "a-" if !last => mods.push("alt"),
            "d-" if !last => mods.push("super"),
            _ if !last => return None,
            lower => {
                name = Some(match lower {
                    "space" => "space".to_string(),
                    "backspace" => "BackSpace".into(),
                    "enter" => "Return".into(),
                    "left" => "Left".into(),
                    "right" => "Right".into(),
                    "up" => "Up".into(),
                    "down" => "Down".into(),
                    "home" => "Home".into(),
                    "end" => "End".into(),
                    "pageup" => "Page_Up".into(),
                    "pagedown" => "Page_Down".into(),
                    "tab" => "Tab".into(),
                    "delete" => "Delete".into(),
                    "insert" => "Insert".into(),
                    "esc" => "Escape".into(),
                    f if f
                        .strip_prefix('f')
                        .filter(|n| !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()))
                        .and_then(|n| n.parse::<u8>().ok())
                        .is_some_and(|n| (1..=19).contains(&n)) =>
                    {
                        format!("F{}", &f[1..])
                    }
                    _ => {
                        let mut chars = part.chars();
                        match (chars.next(), chars.next()) {
                            (Some(c), None) => char_key(c, &mut mods)?,
                            _ => return None,
                        }
                    }
                });
            }
        }
    }
    Some(chord(&mods, &name?))
}

/// A typed character as a keysym, an upper-case letter as shift and the letter.
fn char_key(c: char, mods: &mut Vec<&'static str>) -> Option<String> {
    if c.is_ascii_uppercase() {
        mods.push("shift");
    }
    single(c.to_ascii_lowercase()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yazi")
    }

    fn defaults(v: &str) -> Keymap {
        let text = REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1;
        keymap(text).unwrap()
    }

    fn user(name: &str) -> Keymap {
        user_for("26.9.1", name)
    }

    fn user_for(v: &str, name: &str) -> Keymap {
        let names: Vec<String> = defaults(v).into_keys().collect();
        read_user(&fixtures().join(name), v, &names).unwrap().0
    }

    fn rows<'a>(s: &'a Sheet, section: &str) -> Vec<(&'a str, &'a str)> {
        s.binds[section]
            .iter()
            .map(|b| (b.key.as_str(), b.action.as_str()))
            .collect()
    }

    fn find<'a>(s: &'a Sheet, section: &str, key: &str) -> Vec<&'a Bind> {
        s.binds
            .get(section)
            .map(|b| b.iter().filter(|b| b.key == key).collect())
            .unwrap_or_default()
    }

    #[test]
    fn pinned_files_parse_and_translate() {
        for (v, commit) in [
            ("25.4.8", "99ea3b74c4260a724b43af812df0f68ef59395b7"),
            ("25.5.31", "b65a88075a824e4304dca5428ba05de1404fa635"),
            ("26.9.1", "8dd895c695a5950330c2623eb43debf323b60654"),
        ] {
            let text = REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1;
            let header = text.lines().next().unwrap();
            assert!(
                header.contains(&format!("at tag v{v} (commit {commit})")),
                "{header}"
            );
            assert!(
                text.contains("MIT licensed: Copyright (c) 2023 - sxyazi"),
                "{v}"
            );
            let (s, left_out) = merge(v, defaults(v), Keymap::new());
            assert_eq!(left_out, 0, "{v}");
            // Every entry is listed: none merged away, none lost (the presets have no duplicate and no `for`).
            let entries = text.matches("{ on").count();
            assert_eq!(
                s.binds.values().map(Vec::len).sum::<usize>(),
                entries,
                "{v}"
            );
            // And the whole file is there: its last section's last entry is the last bind listed for it.
            let section = text.lines().rfind(|l| l.starts_with('[')).unwrap();
            let section = section.trim_matches(['[', ']']);
            let on = text
                .rsplit("{ on = \"")
                .next()
                .unwrap()
                .split('"')
                .next()
                .unwrap();
            assert_eq!(
                s.binds[section].last().unwrap().key,
                notation(on).unwrap(),
                "{v}"
            );
        }
        let names = |v: &str| defaults(v).into_keys().collect::<Vec<_>>();
        assert_eq!(
            names("26.9.1"),
            ["cmp", "confirm", "help", "input", "mgr", "pick", "spot", "tasks"]
        );
        assert!(names("25.4.8").contains(&"manager".to_string()));
        assert!(!names("25.5.31").contains(&"manager".to_string()));
        let (s, _) = merge("26.9.1", defaults("26.9.1"), Keymap::new());
        let q = find(&s, "mgr", "q");
        assert_eq!(
            (q.len(), q[0].desc.as_str(), q[0].action.as_str()),
            (1, "Quit the process", "quit")
        );
        assert_eq!(find(&s, "mgr", "shift+q")[0].action, "quit --no-cwd-file");
        assert_eq!(find(&s, "mgr", "g g")[0].action, "arrow top");
    }

    #[test]
    fn version_pick() {
        let pick = |v: &str| {
            let (p, _, note) = pinned::select("yazi", REGISTRY, version_of(v).as_deref()).unwrap();
            (p, note)
        };
        assert_eq!(
            pick("Yazi\n    Version: 26.9.1 (Arch Linux 2026-09-04)\n    Rustc  : 1.98.1 (48a229ce 2026-09-01)\n"),
            ("26.9.1", None)
        );
        assert_eq!(
            version_of("Yazi 0.4.2 (abcdef 2024-12-01)").as_deref(),
            Some("0.4.2")
        );
        // Before the `[mgr]` rename (25.5.28), the `[manager]` pin; 25.5.28-25.5.30 too (a note asks for their pin).
        for (v, want) in [
            ("25.5.10", "25.4.8"),
            ("25.5.28", "25.4.8"),
            ("25.12.29", "25.5.31"),
            ("25.2.26", "25.4.8"),
            ("0.4.2", "25.4.8"),
            ("27.1.1", "26.9.1"),
        ] {
            let (p, note) = pick(&format!("Yazi {v}"));
            assert_eq!(p, want, "{v}");
            assert_eq!(
                note.as_deref(),
                Some(
                    format!("yazi {v}: using the defaults of {want}; add data/yazi/{v}.toml")
                        .as_str()
                )
            );
        }
        let (p, note) = pick("Yazi");
        assert_eq!(p, "26.9.1");
        assert_eq!(
            note.as_deref(),
            Some("yazi's version is unknown: using the defaults of 26.9.1")
        );
    }

    #[test]
    fn prepend_append_and_replace() {
        // prepend: `g g`'s default goes, `g h` (same first key, other second) stays.
        let (s, _) = merge("26.9.1", defaults("26.9.1"), user("prepend.toml"));
        let gg = find(&s, "mgr", "g g");
        assert_eq!(gg.len(), 1);
        assert_eq!(gg[0].action, "arrow bot");
        assert_eq!(s.binds["mgr"][0].key, "g g");
        assert_eq!(find(&s, "mgr", "g h")[0].action, "cd ~");
        // A prepended `noop` unbinds a default: both are gone.
        assert!(find(&s, "mgr", "shift+q").is_empty());
        // By the first two keys: a prepended `g e x` drops the default `g e`.
        assert!(find(&s, "mgr", "g e").is_empty());
        assert_eq!(find(&s, "mgr", "g e x")[0].action, "shell gex");
        // append: one clashing with a default's first two keys is dropped, a new one goes last.
        let (s, _) = merge("26.9.1", defaults("26.9.1"), user("append.toml"));
        assert_eq!(find(&s, "mgr", "q")[0].action, "quit");
        assert!(find(&s, "mgr", "g h x").is_empty());
        assert_eq!(s.binds["mgr"].last().unwrap().key, "ctrl+alt+y");
        // keymap replaces the section's defaults; the other sections keep theirs.
        let (s, _) = merge("26.9.1", defaults("26.9.1"), user("replace.toml"));
        assert_eq!(
            rows(&s, "input"),
            [("Escape", "close"), ("Return", "close --submit")]
        );
        assert_eq!(
            s.binds["mgr"].len(),
            merge("26.9.1", defaults("26.9.1"), Keymap::new()).0.binds["mgr"].len()
        );
    }

    #[test]
    fn platforms_and_duplicates() {
        let (s, _) = merge("26.9.1", defaults("26.9.1"), user("platform.toml"));
        assert!(find(&s, "mgr", "ctrl+alt+m").is_empty());
        assert!(find(&s, "mgr", "ctrl+alt+w").is_empty());
        assert_eq!(find(&s, "mgr", "ctrl+alt+u")[0].action, "shell unix");
        assert_eq!(find(&s, "mgr", "ctrl+alt+l")[0].action, "shell linux");
        // Two with the same keys: the first, as yazi runs the first.
        let d = find(&s, "mgr", "ctrl+alt+d");
        assert_eq!((d.len(), d[0].action.as_str()), (1, "shell first"));
        // A run array, joined; a desc's whitespace collapsed.
        let r = &find(&s, "mgr", "ctrl+alt+r")[0];
        assert_eq!(
            (r.action.as_str(), r.desc.as_str()),
            ("plugin a; plugin b", "two plugins")
        );
    }

    #[test]
    fn platforms_by_version() {
        for (platform, new, old) in [
            (None, Some(true), Some(true)),
            (Some("linux"), Some(true), Some(true)),
            (Some("unix"), Some(true), Some(true)),
            (Some("all"), Some(true), Some(false)),
            (Some("macos"), Some(false), Some(false)),
            (Some("Linux"), None, Some(false)),
        ] {
            assert_eq!(platform_kept("26.9.1", platform), new, "{platform:?}");
            assert_eq!(platform_kept("25.5.31", platform), old, "{platform:?}");
        }
        // A `for` 26.x doesn't know refuses the file there; 25.x drops the entry.
        let path = fixtures().join("bad-for.toml");
        let new: Vec<String> = vec!["manager".into()];
        assert!(read_user(&path, "26.9.1", &new).is_err());
        let user = user_for("25.4.8", "bad-for.toml");
        let (s, _) = merge("25.4.8", defaults("25.4.8"), user);
        assert!(find(&s, "manager", "ctrl+alt+b").is_empty());
        assert_eq!(find(&s, "manager", "ctrl+alt+k")[0].action, "shell kept");
    }

    #[test]
    fn sections_another_version_lacks() {
        // `[manager]` against 26.9.1: not read (so a bad entry in it refuses nothing), named.
        let path = fixtures().join("old-section.toml");
        let names: Vec<String> = defaults("26.9.1").into_keys().collect();
        let (user, unknown) = read_user(&path, "26.9.1", &names).unwrap();
        assert_eq!(unknown, ["manager"]);
        assert!(user.is_empty());
        // Against 25.4.8, where it's the section: applied.
        let (s, _) = merge(
            "25.4.8",
            defaults("25.4.8"),
            user_for("25.4.8", "old-section.toml"),
        );
        assert_eq!(find(&s, "manager", "ctrl+alt+o")[0].action, "shell old");
        // A section that isn't a table is refused only where it's a section; other keys are skipped.
        let path = fixtures().join("not-a-table/keymap.toml");
        assert!(read_user(&path, "26.9.1", &names).is_err());
        let old: Vec<String> = defaults("25.4.8").into_keys().collect();
        assert!(read_user(&path, "25.4.8", &old).unwrap().0.is_empty());
    }

    #[test]
    fn keys() {
        for (yazi, want) in [
            ("<C-a>", Some("ctrl+a")),
            ("<C-A>", Some("ctrl+shift+a")),
            ("<A-k>", Some("alt+k")),
            ("<S-Tab>", Some("shift+Tab")),
            ("<S-a>", Some("shift+a")),
            ("<D-x>", Some("super+x")),
            ("<c-s-x>", Some("ctrl+shift+x")),
            ("<C-[>", Some("ctrl+bracketleft")),
            ("<Esc>", Some("Escape")),
            ("<Enter>", Some("Return")),
            ("<Space>", Some("space")),
            ("<Backspace>", Some("BackSpace")),
            ("<Delete>", Some("Delete")),
            ("<PageDown>", Some("Page_Down")),
            ("<Home>", Some("Home")),
            ("<F19>", Some("F19")),
            ("<C-->", Some("ctrl+minus")),
            ("<->", Some("minus")),
            ("Q", Some("shift+q")),
            ("q", Some("q")),
            ("~", Some("asciitilde")),
            (".", Some("period")),
            ("1", Some("1")),
            ("<Tab>", Some("Tab")),
            ("<Insert>", Some("Insert")),
            ("<Left>", Some("Left")),
            ("<Right>", Some("Right")),
            ("<Up>", Some("Up")),
            ("<Down>", Some("Down")),
            ("<End>", Some("End")),
            ("<PageUp>", Some("Page_Up")),
            ("<F1>", Some("F1")),
            // Modifiers in any order and case, shift once.
            ("<S-A>", Some("shift+a")),
            ("<S-C-x>", Some("ctrl+shift+x")),
            ("<C-S-x>", Some("ctrl+shift+x")),
            ("<F01>", None),
            ("<F+1>", None),
            ("<Foo>", None),
            ("<F20>", None),
            ("<X-a>", None),
            ("<>", None),
            ("", None),
        ] {
            assert_eq!(notation(yazi).as_deref(), want, "{yazi}");
        }
        let gg = Entry {
            on: OneOrMany::Many(vec!["g".into(), "g".into()]),
            run: OneOrMany::One("arrow top".into()),
            desc: String::new(),
            platform: None,
        };
        let user: Keymap = [(
            "mgr".to_string(),
            Section {
                keymap: Some(vec![
                    gg.clone(),
                    Entry {
                        on: OneOrMany::One("<Foo>".into()),
                        ..gg
                    },
                ]),
                prepend_keymap: vec![],
                append_keymap: vec![],
            },
        )]
        .into();
        let (s, left_out) = merge("26.9.1", defaults("26.9.1"), user);
        assert_eq!(rows(&s, "mgr"), [("g g", "arrow top")]);
        // No desc: the run is the description.
        assert_eq!(s.binds["mgr"][0].desc, "arrow top");
        assert!(s.binds["mgr"][0].when.is_empty());
        assert_eq!(left_out, 1);
        // What yazi refuses (the whole file) apart from what it reads but keybind-find can't name.
        for (key, reads) in [
            ("q", true),
            ("<Foo>", true),
            ("<c-a>", true),
            ("<X-a>", false),
            ("<C->", false),
            ("<>", false),
            ("", false),
        ] {
            assert_eq!(yazi_reads(key), reads, "{key}");
        }
    }

    /// An env with `vars` only.
    fn env_of(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
        move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn config_dir() {
        let tmp = std::env::temp_dir().join(format!("kbf-yazi-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in ["yazi-home", "xdg/yazi", "home/.config/yazi"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
            std::fs::write(tmp.join(d).join("keymap.toml"), "").unwrap();
        }
        let t = |p: &str| tmp.join(p).to_string_lossy().into_owned();
        let pick = |vars| keymap_file(&env_of(vars));
        let home = ("HOME", t("home"));
        assert_eq!(
            pick(vec![
                home.clone(),
                ("XDG_CONFIG_HOME", t("xdg")),
                ("YAZI_CONFIG_HOME", t("yazi-home"))
            ]),
            Some(tmp.join("yazi-home/keymap.toml"))
        );
        assert_eq!(
            pick(vec![
                home.clone(),
                ("XDG_CONFIG_HOME", t("xdg")),
                ("YAZI_CONFIG_HOME", "rel".into())
            ]),
            Some(tmp.join("xdg/yazi/keymap.toml"))
        );
        assert_eq!(
            pick(vec![home.clone(), ("XDG_CONFIG_HOME", String::new())]),
            Some(tmp.join("home/.config/yazi/keymap.toml"))
        );
        assert_eq!(pick(vec![("HOME", t("nowhere"))]), None);
        // YAZI_CONFIG_HOME set but holding no keymap.toml: none, not the next directory's.
        assert_eq!(
            pick(vec![home.clone(), ("YAZI_CONFIG_HOME", t("nowhere"))]),
            None
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A stand-in `yazi` printing `out`, in its own directory; returns the directory.
    fn fake_yazi(name: &str, out: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!("kbf-yazi-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process: a write handle another test thread's fork inherited would make
        // running the script fail with ETXTBSY (as `plugins.rs`'s `write_script` says).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("yazi"))
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
        let bin = fake_yazi("ok", "Yazi\\n    Version: 26.9.1 (x)\\n");
        // The user file: through YAZI_CONFIG_HOME, merged.
        let env = env_of(vec![(
            "YAZI_CONFIG_HOME",
            fixtures().join("dir").to_string_lossy().into_owned(),
        )]);
        let (s, notes) = sheet_with(REGISTRY, path(&bin), &env).unwrap();
        assert_eq!(find(&s, "mgr", "ctrl+alt+z")[0].action, "shell from-dir");
        assert!(notes.is_empty(), "{notes:?}");
        // No user file: the defaults, no line.
        let none = env_of(vec![("HOME", "/nonexistent-kbf".into())]);
        let (s, notes) = sheet_with(REGISTRY, path(&bin), &none).unwrap();
        assert_eq!(s.binds.len(), 8);
        assert!(notes.is_empty(), "{notes:?}");
        // Invalid TOML, or an entry yazi refuses: the defaults and one line.
        for bad in ["bad", "no-run", "bad-key", "not-a-table"] {
            let env = env_of(vec![(
                "YAZI_CONFIG_HOME",
                fixtures().join(bad).to_string_lossy().into_owned(),
            )]);
            let (s, notes) = sheet_with(REGISTRY, path(&bin), &env).unwrap();
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(notes[0].ends_with("; showing yazi's defaults"), "{notes:?}");
            assert!(!notes[0].contains('\n'), "{notes:?}");
            assert_eq!(s.binds.len(), 8);
        }
        // An old section and an untranslatable key: one line each.
        let env = env_of(vec![(
            "YAZI_CONFIG_HOME",
            fixtures().join("odd").to_string_lossy().into_owned(),
        )]);
        let (_, notes) = sheet_with(REGISTRY, path(&bin), &env).unwrap();
        assert_eq!(
            notes,
            [
                "keymap.toml's [manager] isn't a section of yazi 26.9.1's keymap; ignored, as yazi does",
                "leaving out 1 key with no keysym we know"
            ]
        );
        let _ = std::fs::remove_dir_all(&bin);
        // A yazi printing nothing: the newest, with a note.
        let bin = fake_yazi("silent", "");
        let (_, notes) = sheet_with(REGISTRY, path(&bin), &none).unwrap();
        assert_eq!(
            notes,
            ["yazi's version is unknown: using the defaults of 26.9.1"]
        );
        let _ = std::fs::remove_dir_all(&bin);
        // No yazi: an error.
        let empty = std::env::temp_dir().join(format!("kbf-yazi-none-{}", std::process::id()));
        let err = sheet_with(REGISTRY, path(&empty), &none).unwrap_err();
        assert!(
            format!("{err:#}").contains("running yazi --version"),
            "{err:#}"
        );
        // A pinned file that doesn't parse: an error naming it.
        let bin = fake_yazi("bad-pin", "Yazi 26.9.1");
        let err = sheet_with(&[("26.9.1", "[mgr\n")], path(&bin), &none).unwrap_err();
        assert!(
            format!("{err:#}").contains("data/yazi/26.9.1.toml"),
            "{err:#}"
        );
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn real_yazi_version() {
        let Ok(out) = Command::new("yazi")
            .arg("--version")
            .env("LC_ALL", "C")
            .output()
        else {
            eprintln!("skipping: yazi is not installed");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let v = version_of(&text).unwrap_or_else(|| panic!("no version in {text:?}"));
        assert!(pinned::select("yazi", REGISTRY, Some(&v)).is_some());
    }
}
