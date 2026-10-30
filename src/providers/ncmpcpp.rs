//! ncmpcpp: its default keys pinned at 0.10.1 (`data/ncmpcpp/`), with the user's bindings files on top, read as
//! ncmpcpp reads them. Shown when ncmpcpp is the program in the focused terminal. ncmpcpp is never run.

use crate::sheet::{Bind, Sheet};
use serde::Deserialize;
use std::path::PathBuf;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

const DEFAULTS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/ncmpcpp/0.10.1.toml"
));

#[derive(Deserialize)]
struct Pinned {
    key: Vec<PinnedKey>,
}

#[derive(Deserialize)]
struct PinnedKey {
    name: String,
    /// Alternatives in order, each a chain of actions.
    actions: Vec<Vec<String>>,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = read(&env)?;
    for note in notes {
        eprintln!("keybind-find: ncmpcpp: {note}");
    }
    Ok(sheet)
}

/// The sheet and the notes for stderr, with the user's files found through `env`.
fn read(env: Env) -> anyhow::Result<(Sheet, Vec<String>)> {
    let files: Vec<(String, String)> = paths(env)
        .into_iter()
        // ncmpcpp opens each with `ifstream` and skips one it can't open.
        // ncmpcpp reads bytes: a file that isn't UTF-8 is read lossily, not skipped.
        .filter_map(|p| {
            let bytes = std::fs::read(&p).ok()?;
            Some((
                p.display().to_string(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        })
        .collect();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    parse(DEFAULTS, &files)
}

/// ncmpcpp's bindings files, in its order: `$XDG_CONFIG_HOME/ncmpcpp/bindings` (`~/.config/` when unset), then
/// `~/.ncmpcpp/bindings`; `~` is `$HOME`. A relative `XDG_CONFIG_HOME` is relative to ncmpcpp's directory, which
/// keybind-find doesn't know: that file is left out.
fn paths(env: Env) -> Vec<PathBuf> {
    let home = env("HOME").filter(|h| !h.is_empty());
    let expand = |p: String| -> Option<PathBuf> {
        match p.strip_prefix('~') {
            Some(rest) => Some(PathBuf::from(format!("{}{rest}", home.as_ref()?))),
            None => Some(PathBuf::from(p)),
        }
    };
    let xdg = match env("XDG_CONFIG_HOME") {
        None => "~/.config/".to_string(),
        Some(x) if x.is_empty() || x.ends_with('/') => x,
        Some(x) => format!("{x}/"),
    };
    [
        format!("{xdg}ncmpcpp/bindings"),
        "~/.ncmpcpp/bindings".into(),
    ]
    .into_iter()
    .filter_map(expand)
    .filter(|p| p.is_absolute())
    .collect()
}

/// One key as bound: its name as written and its alternatives (each a chain of action lines).
struct Key {
    name: String,
    notation: Option<String>,
    alternatives: Vec<Vec<String>>,
}

/// The sheet from the pinned `defaults` and the user's `files` (`(path, text)`, in order), and the notes.
fn parse(defaults: &str, files: &[(&str, &str)]) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut notes = Vec::new();
    let (mut user, mut commands) = (Vec::new(), Vec::new());
    for (path, text) in files {
        if let Err((line, why)) = read_file(text, &mut user, &mut commands) {
            notes.push(format!(
                "{path}:{line}: {why}; ncmpcpp won't start with this file (a running one read it before it changed); keybind-find reads up to there"
            ));
            // As ncmpcpp's `std::all_of` over the files: an error stops the rest.
            break;
        }
    }
    let pinned: Pinned =
        toml::from_str(defaults).map_err(|e| anyhow::anyhow!("data/ncmpcpp/0.10.1.toml: {e}"))?;
    let mut keys = user;
    for k in pinned.key {
        let notation = notation(&k.name);
        // `generateDefaults` binds a default only to a key the user hasn't bound.
        if !keys.iter().any(|u| same(u, &k.name, &notation)) {
            keys.push(Key {
                name: k.name,
                notation,
                alternatives: k.actions,
            });
        }
    }
    let mut sheet = Sheet {
        title: "ncmpcpp".into(),
        provider: "ncmpcpp".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for k in keys {
        if k.alternatives.iter().flatten().all(|a| a == "dummy") {
            continue;
        }
        let Some(key) = k.notation else {
            left_out += 1;
            continue;
        };
        let alternatives = |each: &dyn Fn(&str) -> String| {
            k.alternatives
                .iter()
                .map(|chain| chain.iter().map(|a| each(a)).collect::<Vec<_>>().join(", "))
                .collect::<Vec<_>>()
                .join(" / ")
        };
        sheet.binds.entry("keys".into()).or_default().push(Bind {
            key,
            desc: alternatives(&describe),
            action: alternatives(&|a| a.to_string()),
            when: Default::default(),
        });
    }
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) keybind-find can't send (mouse, a non-ASCII key, a key no press reaches)"
        ));
    }
    Ok((sheet, notes))
}

/// Whether `k` is the key `name` (by its notation, else as written).
fn same(k: &Key, name: &str, notation: &Option<String>) -> bool {
    match (&k.notation, notation) {
        (Some(a), Some(b)) => a == b,
        _ => k.name == name,
    }
}

/// An action line for people: the action's name with `_` as a space, its argument as written.
fn describe(line: &str) -> String {
    match line.split_once(|c: char| c.is_ascii_whitespace()) {
        Some((name, arg)) => format!(
            "{} {}",
            name.replace('_', " "),
            arg.trim_start_matches(|c: char| c.is_ascii_whitespace())
        ),
        None => line.replace('_', " "),
    }
}

/// Reads one bindings file into `keys` as ncmpcpp's `BindingsConfiguration::read` does: `Err((line, why))` where it
/// stops; the keys bound before that stand, the one being defined is dropped.
fn read_file(
    text: &str,
    keys: &mut Vec<Key>,
    commands: &mut Vec<String>,
) -> Result<(), (usize, String)> {
    #[derive(PartialEq)]
    enum InProgress {
        None,
        Command,
        Key,
    }
    let mut in_progress = InProgress::None;
    let mut actions: Vec<String> = Vec::new();
    let (mut name, mut line_no) = (String::new(), 0);
    // `bind_in_progress`: ends the block being read; an empty one is an error.
    let mut end = |in_progress: &InProgress,
                   name: &str,
                   actions: &mut Vec<String>,
                   commands: &mut Vec<String>,
                   line_no: usize| {
        match in_progress {
            InProgress::None => return Ok(()),
            _ if actions.is_empty() => {
                let what = if *in_progress == InProgress::Key {
                    "key"
                } else {
                    "command"
                };
                return Err((
                    line_no,
                    format!("definition of {what} '{name}' cannot be empty"),
                ));
            }
            InProgress::Command => commands.push(name.to_string()),
            InProgress::Key => {
                let notation = notation(name);
                let chain = std::mem::take(actions);
                match keys.iter_mut().find(|k| same(k, name, &notation)) {
                    Some(k) => k.alternatives.push(chain),
                    None => keys.push(Key {
                        name: name.to_string(),
                        notation,
                        alternatives: vec![chain],
                    }),
                }
            }
        }
        actions.clear();
        Ok(())
    };
    for line in text.split('\n') {
        line_no += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("def_command") {
            end(&in_progress, &name, &mut actions, commands, line_no)?;
            in_progress = InProgress::Command;
            name = enclosed(line, '"', '"');
            if name.is_empty() {
                return Err((line_no, "command must have non-empty name".into()));
            }
            // ncmpcpp's commands are one map across its files.
            if commands.contains(&name) {
                return Err((line_no, format!("redefinition of command '{name}'")));
            }
            let kind = enclosed(line, '[', ']');
            if kind != "immediate" && kind != "deferred" {
                return Err((line_no, format!("invalid type of command: '{kind}'")));
            }
        } else if line.starts_with("def_key") {
            end(&in_progress, &name, &mut actions, commands, line_no)?;
            in_progress = InProgress::Key;
            name = enclosed(line, '"', '"');
            if !valid(&name) {
                return Err((line_no, format!("invalid key: '{name}'")));
            }
        // C's `isspace`: ASCII whitespace only.
        } else if line.starts_with(|c: char| c.is_ascii_whitespace()) {
            let action = line.trim_matches(|c: char| c.is_ascii_whitespace());
            // ncmpcpp checks the action's name; keybind-find doesn't know its list, but none is no action.
            if action.is_empty() {
                return Err((line_no, "unknown action: ''".into()));
            }
            actions.push(action.to_string());
        } else {
            return Err((line_no, format!("invalid line: '{line}'")));
        }
    }
    end(&in_progress, &name, &mut actions, commands, line_no)
}

/// `getEnclosedString`: the text between the first `open` and the next `close`, with `\\` and `\<close>` unescaped;
/// empty when it isn't closed.
fn enclosed(s: &str, open: char, close: char) -> String {
    let Some(start) = s.find(open) else {
        return String::new();
    };
    let mut out = String::new();
    let mut chars = s[start + open.len_utf8()..].chars();
    while let Some(c) = chars.next() {
        if c == close {
            return out;
        }
        if c == '\\' {
            let rest = chars.as_str();
            if let Some(n) = rest.chars().next().filter(|n| *n == '\\' || *n == close) {
                out.push(n);
                chars.next();
                continue;
            }
        }
        out.push(c);
    }
    String::new()
}

/// Whether ncmpcpp's `stringToKey` accepts `name` (a special name or one character).
fn valid(name: &str) -> bool {
    special(name).is_some() || name.chars().count() == 1
}

/// `stringToSpecialKey` as key notation; `Some(None)` for a key ncmpcpp accepts but keybind-find can't send
/// (`mouse`, or a modifier on one).
fn special(s: &str) -> Option<Option<String>> {
    Some(parts(s)?.map(|(mods, key)| chord(&mods, &key)))
}

/// `chord`, with a `ctrl-<x>` key code's marker as ctrl.
fn chord(mods: &[&str], key: &str) -> String {
    let mods: Vec<&str> = mods
        .iter()
        .map(|m| if *m == CODE { "ctrl" } else { m })
        .collect();
    crate::sheet::chord(&mods, key)
}

/// The modifier `ctrl-<x>` (six characters) carries: a key code (1 for `ctrl-a`), not ncmpcpp's ctrl bit.
const CODE: &str = "ctrl-code";

/// Whether ncmpcpp's `readKey` can report `key` (with `mods`) with the modifier `m` added: the shift and ctrl bits
/// come only with the arrows, insert, delete, home, end and the page keys (and shift with tab), never with a character
/// or a ctrl code; alt with anything but tab (`ESC` tab is read as shift-tab).
fn reaches(m: &str, mods: &[&str], key: &str) -> bool {
    const NAMED: [&str; 10] = [
        "Up", "Down", "Left", "Right", "Insert", "Delete", "Home", "End", "Prior", "Next",
    ];
    match m {
        "alt" => key != "Tab",
        _ => !mods.contains(&CODE) && (NAMED.contains(&key) || (m == "shift" && key == "Tab")),
    }
}

/// `stringToSpecialKey` as modifiers and a keysym, so nested prefixes (`shift-ctrl-a`, `shift-A`) come out canonical.
fn parts(s: &str) -> Option<Option<(Vec<&'static str>, String)>> {
    let b = s.as_bytes();
    let key = |k: &str| Some(Some((Vec::new(), k.to_string())));
    if s.starts_with("ctrl") && s.len() == 6 && b[4] == b'-' {
        let c = b[5] as char;
        return match c {
            // The same key codes as tab, enter and escape in ncmpcpp (and in a terminal).
            'i' => key("Tab"),
            'm' => key("Return"),
            '[' => key("Escape"),
            'a'..='z' | '\\' | ']' | '^' | '_' => {
                Some(super::nvim::single(c).map(|k| (vec![CODE], k.to_string())))
            }
            _ => None,
        };
    }
    for (prefix, m) in [("alt-", "alt"), ("ctrl-", "ctrl"), ("shift-", "shift")] {
        if let Some(rest) = s.strip_prefix(prefix).filter(|r| !r.is_empty()) {
            // `stringToKey`: a special name, else one character.
            let one = rest.chars().count() == 1;
            let inner = parts(rest).or_else(|| one.then(|| single(rest)))?;
            // A key no press reaches (`shift-a`, `ctrl-alt-x`, `shift-enter`): kept apart from the key it would look
            // like (`A`), and not listed.
            return Some(inner.and_then(|(mut mods, k)| {
                reaches(m, &mods, &k).then(|| {
                    mods.push(m);
                    (mods, k)
                })
            }));
        }
    }
    let named = match s {
        "escape" => "Escape",
        "mouse" => return Some(None),
        "up" => "Up",
        "down" => "Down",
        "page_up" => "Prior",
        "page_down" => "Next",
        "home" => "Home",
        "end" => "End",
        "space" => "space",
        "enter" => "Return",
        "insert" => "Insert",
        "delete" => "Delete",
        "left" => "Left",
        "right" => "Right",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        f if (f.len() == 2 || f.len() == 3) && f.starts_with('f') => {
            // `atoi`: the leading digits.
            let digits: String = f[1..].chars().take_while(char::is_ascii_digit).collect();
            let n: u8 = digits.parse().ok().filter(|n| (1..=12).contains(n))?;
            return key(&format!("F{n}"));
        }
        _ => return None,
    };
    key(named)
}

/// One character as modifiers and a keysym: an upper-case letter is shift and the letter; none for one keybind-find
/// can't send.
fn single(c: &str) -> Option<(Vec<&'static str>, String)> {
    let c = c.chars().next()?;
    let key = super::nvim::single(c.to_ascii_lowercase())?.to_string();
    Some((
        if c.is_ascii_uppercase() {
            vec!["shift"]
        } else {
            Vec::new()
        },
        key,
    ))
}

/// A key name as key notation; `None` when it can't be sent or isn't a key.
fn notation(name: &str) -> Option<String> {
    match special(name) {
        Some(k) => k,
        None if name.chars().count() == 1 => single(name).map(|(m, k)| chord(&m, &k)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/ncmpcpp")
                .join(name),
        )
        .unwrap()
    }

    fn get<'a>(s: &'a Sheet, key: &str) -> Option<&'a str> {
        s.binds["keys"]
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.desc.as_str())
    }

    #[test]
    fn pinned_defaults() {
        assert!(
            DEFAULTS.contains("tag 0.10.1")
                && DEFAULTS.contains("3e6d992b938de9ba3019302027bd64dbcab6f8c6")
        );
        assert!(DEFAULTS.contains("BindingsConfiguration::generateDefaults()"));
        let pinned: Pinned = toml::from_str(DEFAULTS).unwrap();
        assert_eq!(pinned.key.len(), 96);
        // Every key translates, but `mouse`.
        let untranslated: Vec<&str> = pinned
            .key
            .iter()
            .filter(|k| notation(&k.name).is_none())
            .map(|k| k.name.as_str())
            .collect();
        assert_eq!(untranslated, ["mouse"]);
        let (s, notes) = parse(DEFAULTS, &[]).unwrap();
        assert_eq!(s.binds["keys"].len(), 95);
        assert_eq!(
            notes,
            ["leaving out 1 key(s) keybind-find can't send (mouse, a non-ASCII key, a key no press reaches)"]
        );
        assert_eq!(get(&s, "p"), Some("pause"));
        assert_eq!(get(&s, "q"), Some("quit"));
        assert_eq!(
            get(&s, "Return"),
            Some("enter directory / toggle output / run action / play item")
        );
        assert_eq!(get(&s, "shift+Up"), Some("select item, scroll up"));
        for b in &s.binds["keys"] {
            assert!(
                b.key.parse::<keybind_find_sheet::Key>().is_ok(),
                "{}",
                b.key
            );
            assert!(b.when.is_empty());
        }
    }

    #[test]
    fn bindings_over_defaults() {
        let (xdg, home) = (fixture("bindings"), fixture("home-bindings"));
        let (s, notes) = parse(
            DEFAULTS,
            &[("xdg/bindings", &xdg), ("home/bindings", &home)],
        )
        .unwrap();
        // The invalid key on line 30 stops ncmpcpp's reading, of this file and (as `std::all_of`) the next.
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(
            notes[0],
            "xdg/bindings:30: invalid key: 'bogus'; ncmpcpp won't start with this file (a running one read it before it changed); keybind-find reads up to there"
        );
        // `z` after the error keeps its default; the second file's `Q` isn't read.
        assert_eq!(get(&s, "z"), Some("toggle random"));
        assert_eq!(get(&s, "shift+q"), None);
        // Replaced; a chain; alternatives; `dummy` left out; the command skipped; read before the error, kept.
        assert_eq!(get(&s, "p"), Some("play"));
        assert_eq!(get(&s, "ctrl+r"), Some("select item, scroll down"));
        assert_eq!(
            get(&s, "x"),
            Some("toggle output / run external command \"echo hi\"")
        );
        assert!(get(&s, "s").is_none());
        assert_eq!(get(&s, "alt+l"), Some("show lyrics"));
        let x = s.binds["keys"].iter().find(|b| b.key == "x").unwrap();
        assert_eq!(x.action, "toggle_output / run_external_command \"echo hi\"");
        // Both files, XDG first, when neither stops: a key in both gets both.
        let clean = xdg.replace("def_key \"bogus\"", "def_key \"w\"");
        let (s, notes) = parse(
            DEFAULTS,
            &[("xdg/bindings", &clean), ("home/bindings", &home)],
        )
        .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(
            get(&s, "x"),
            Some("toggle output / run external command \"echo hi\" / stop")
        );
        assert_eq!(get(&s, "shift+q"), Some("quit"));
        assert_eq!(get(&s, "z"), Some("quit"));
        // A user's `dummy` hides the default too (it is bound).
        let (s, _) = parse(DEFAULTS, &[("b", "def_key \"q\"\n  dummy\n")]).unwrap();
        assert!(get(&s, "q").is_none());
    }

    #[test]
    fn errors_as_ncmpcpp_sees_them() {
        let note = |text: &str| parse(DEFAULTS, &[("b", text)]).unwrap().1[0].clone();
        assert!(note("def_key \"p\"\ndef_key \"q\"\n  quit\n")
            .starts_with("b:2: definition of key 'p' cannot be empty"));
        assert!(note("def_key \"p\"\n").starts_with("b:2: definition of key 'p' cannot be empty"));
        assert!(note("bind p pause\n").starts_with("b:1: invalid line: 'bind p pause'"));
        assert!(note("def_key \"p\"\n   \n").starts_with("b:2: unknown action: ''"));
        assert!(note("def_key \"ctrl-A\"\n  quit\n").starts_with("b:1: invalid key: 'ctrl-A'"));
        assert!(note("def_key \"p\n  quit\n").starts_with("b:1: invalid key: ''"));
        assert!(note("def_command \"\" [deferred]\n  quit\n")
            .starts_with("b:1: command must have non-empty name"));
        assert!(note("def_command \"c\" [later]\n  quit\n")
            .starts_with("b:1: invalid type of command: 'later'"));
        let twice = "def_command \"c\" [deferred]\n  quit\ndef_command \"c\" [immediate]\n  quit\n";
        assert!(note(twice).starts_with("b:3: redefinition of command 'c'"));
        assert!(
            note("def_command \"c\" [deferred]\ndef_key \"p\"\n  quit\n")
                .starts_with("b:2: definition of command 'c' cannot be empty")
        );
        // The command map spans the files: the second file's `c` is a redefinition.
        let c = "def_command \"c\" [deferred]\n  quit\n";
        let notes = parse(DEFAULTS, &[("one", c), ("two", c)]).unwrap().1;
        assert!(
            notes[0].starts_with("two:1: redefinition of command 'c'"),
            "{notes:?}"
        );
        // `\\` in a key name is a backslash.
        let (s3, _) = parse(DEFAULTS, &[("b", "def_key \"\\\\\"\n  quit\n")]).unwrap();
        assert_eq!(get(&s3, "backslash"), Some("quit"));
        // A key with no notation is matched by name: the user's `mouse` replaces the default (still left out, once).
        let notes = parse(DEFAULTS, &[("b", "def_key \"mouse\"\n  quit\n")])
            .unwrap()
            .1;
        assert_eq!(notes, ["leaving out 1 key(s) keybind-find can't send (mouse, a non-ASCII key, a key no press reaches)"]);
        // `shift-a` is a key no press reaches: it doesn't replace `A`'s default.
        let (s2, _) = parse(DEFAULTS, &[("b", "def_key \"shift-a\"\n  quit\n")]).unwrap();
        let (s2, notes2) = (
            s2,
            parse(DEFAULTS, &[("b", "def_key \"shift-a\"\n  quit\n")])
                .unwrap()
                .1,
        );
        assert_eq!(get(&s2, "shift+a"), Some("add"));
        assert_eq!(notes2, ["leaving out 2 key(s) keybind-find can't send (mouse, a non-ASCII key, a key no press reaches)"]);
        // A non-ASCII key is a key to ncmpcpp but can't be sent: counted.
        let (_, notes) = parse(DEFAULTS, &[("b", "def_key \"é\"\n  quit\n")]).unwrap();
        assert_eq!(
            notes,
            ["leaving out 2 key(s) keybind-find can't send (mouse, a non-ASCII key, a key no press reaches)"]
        );
        // Only the action's name gets `_` as a space; its argument is as written.
        let (s, _) = parse(
            DEFAULTS,
            &[("b", "def_key \"p\"\n  push_characters \"a_b\"\n")],
        )
        .unwrap();
        assert_eq!(get(&s, "p"), Some("push characters \"a_b\""));
        // An action line must start with ASCII whitespace (C's `isspace`).
        assert!(note("def_key \"p\"\n\u{a0}quit\n").starts_with("b:2: invalid line: "));
        // `ctrl-m` is enter: binding it replaces enter's defaults; `tab` and `ctrl-i` are one key's alternatives.
        let same =
            "def_key \"ctrl-m\"\n  quit\ndef_key \"tab\"\n  stop\ndef_key \"ctrl-i\"\n  play\n";
        let (s, _) = parse(DEFAULTS, &[("b", same)]).unwrap();
        assert_eq!(get(&s, "Return"), Some("quit"));
        assert_eq!(get(&s, "Tab"), Some("stop / play"));
        // What was read before the error stands; the key being defined is dropped.
        let (s, _) = parse(
            DEFAULTS,
            &[(
                "b",
                "def_key \"p\"\n  play\ndef_key \"q\"\n  stop\nnonsense\n",
            )],
        )
        .unwrap();
        assert_eq!(get(&s, "p"), Some("play"));
        assert_eq!(get(&s, "q"), Some("quit"));
        // Escapes in the key, as `getEnclosedString`.
        let (s, notes) = parse(DEFAULTS, &[("b", "def_key \"\\\"\"\n  quit\n")]).unwrap();
        assert_eq!((get(&s, "quotedbl"), notes.len()), (Some("quit"), 1));
    }

    #[test]
    fn the_files() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            paths(&env(&[("HOME", "/h")])),
            [
                PathBuf::from("/h/.config/ncmpcpp/bindings"),
                "/h/.ncmpcpp/bindings".into()
            ]
        );
        assert_eq!(
            paths(&env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/x")])),
            [
                PathBuf::from("/x/ncmpcpp/bindings"),
                "/h/.ncmpcpp/bindings".into()
            ]
        );
        assert_eq!(
            paths(&env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "~/conf/")]))[0],
            PathBuf::from("/h/conf/ncmpcpp/bindings")
        );
        // Empty or relative: relative to ncmpcpp's directory, unknown here.
        for xdg in ["", "conf"] {
            let pairs: &'static [(&'static str, &'static str)] = if xdg.is_empty() {
                &[("HOME", "/h"), ("XDG_CONFIG_HOME", "")]
            } else {
                &[("HOME", "/h"), ("XDG_CONFIG_HOME", "conf")]
            };
            assert_eq!(paths(&env(pairs)), [PathBuf::from("/h/.ncmpcpp/bindings")]);
        }
        assert!(paths(&env(&[])).is_empty());
        // A file that isn't UTF-8 is read (lossily), as ncmpcpp reads bytes.
        let latin = std::env::temp_dir().join(format!("kbf-ncmpcpp-latin-{}", std::process::id()));
        std::fs::create_dir_all(latin.join(".ncmpcpp")).unwrap();
        std::fs::write(
            latin.join(".ncmpcpp/bindings"),
            b"# caf\xe9\ndef_key \"p\"\n  play\n",
        )
        .unwrap();
        let home_l = latin.display().to_string();
        let (s, _) = read(&|k: &str| (k == "HOME").then(|| home_l.clone())).unwrap();
        let _ = std::fs::remove_dir_all(&latin);
        assert_eq!(get(&s, "p"), Some("play"));
        // Missing and unreadable files: skipped silently.
        let dir = std::env::temp_dir().join(format!("kbf-ncmpcpp-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".ncmpcpp/bindings")).unwrap();
        let home = dir.display().to_string();
        let (s, notes) = read(&|k: &str| (k == "HOME").then(|| home.clone())).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((s.binds["keys"].len(), notes.len()), (95, 1));
    }

    #[test]
    fn keys() {
        for (name, want) in [
            ("ctrl-_", "ctrl+underscore"),
            ("ctrl-a", "ctrl+a"),
            ("ctrl-[", "Escape"),
            ("ctrl-i", "Tab"),
            ("ctrl-m", "Return"),
            ("ctrl-]", "ctrl+bracketright"),
            ("alt-l", "alt+l"),
            ("alt-L", "alt+shift+l"),
            ("ctrl-up", "ctrl+Up"),
            ("shift-tab", "shift+Tab"),
            ("shift-up", "shift+Up"),
            ("page_up", "Prior"),
            ("page_down", "Next"),
            ("f1", "F1"),
            ("f12", "F12"),
            ("A", "shift+a"),
            ("~", "asciitilde"),
            ("enter", "Return"),
            ("backspace", "BackSpace"),
            ("escape", "Escape"),
            ("up", "Up"),
            ("down", "Down"),
            ("left", "Left"),
            ("right", "Right"),
            ("home", "Home"),
            ("end", "End"),
            ("insert", "Insert"),
            ("delete", "Delete"),
            ("space", "space"),
            ("tab", "Tab"),
            // Nested prefixes come out in canonical order, shift once.
            ("shift-ctrl-up", "ctrl+shift+Up"),
            ("alt-shift-left", "alt+shift+Left"),
            ("alt-ctrl-x", "ctrl+alt+x"),
            ("shift-ctrl-i", "shift+Tab"),
            ("ctrl-page_up", "ctrl+Prior"),
            ("ctrl-\\", "ctrl+backslash"),
        ] {
            assert_eq!(notation(name).as_deref(), Some(want), "{name}");
            assert!(valid(name), "{name}");
            assert!(want.parse::<keybind_find_sheet::Key>().is_ok(), "{want}");
        }
        // Valid to ncmpcpp, but not sent: counted.
        for name in [
            "mouse",
            "é",
            "alt-mouse",
            "shift-a",
            "shift-A",
            "shift-1",
            "shift-ctrl-a",
            "shift-alt-a",
            // Keys no press reaches: ctrl or shift on anything but the named navigation keys, alt on tab.
            "ctrl-alt-x",
            "ctrl-ctrl-a",
            "ctrl-enter",
            "ctrl-tab",
            "shift-enter",
            "shift-f1",
            "alt-tab",
        ] {
            assert_eq!(notation(name), None, "{name}");
            assert!(valid(name), "{name}");
        }
        // Not keys at all.
        for name in ["f13", "f0", "bogus", "ctrl-A", "alt-", ""] {
            assert!(!valid(name), "{name}");
        }
    }
}
