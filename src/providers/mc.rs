//! Midnight Commander: its keymap built as mc builds it (`src/keymap.c`), from its compiled-in defaults pinned
//! (`data/mc/`), then `/usr/share/mc/mc.keymap`, the system `mc.keymap` and one user keymap laid over them. Shown when
//! mc, mcedit, mcview or mcdiff is the program in the focused terminal. Files are only read.

use super::nvim::single;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::anyhow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// mc 4.8.33's compiled-in defaults. The installed system keymap (distros ship the full default keymap as
/// `/etc/mc/mc.keymap`) is laid over them, so another mc version's keys win for every action that file names; actions
/// only 4.8.33 compiles in still show.
// ponytail: one pinned version; a registry by `mc --version` if a version without the system file shows wrong keys.
const PINNED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/mc/4.8.33.keymap"
));

/// mc's `DATADIR` and `SYSCONFDIR` as distros build it.
const SHARE: &str = "/usr/share/mc";
const SYSCONF: &str = "/etc/mc";

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A keyfile's sections in order, each with its actions in order and their raw values.
type Keymap = Vec<(String, Vec<(String, String)>)>;

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(PINNED, Path::new(SHARE), Path::new(SYSCONF), &env)?;
    for note in notes {
        eprintln!("keybind-find: mc: {note}");
    }
    Ok(sheet)
}

/// The sheet from `pinned` and the keymap files under `share` and `sysconf` (test seams) and the user's, found
/// through `env`. Also the lines for stderr.
// ponytail: `env` is keybind-find's (from the window manager), not mc's: an `MC_KEYMAP` set only in the shell that ran
// mc isn't seen; reading mc's `/proc/<pid>/environ` needs mc's pid, which the provider isn't given.
fn sheet_with(
    pinned: &str,
    share: &Path,
    sysconf: &Path,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut keymap = keyfile(pinned).map_err(|e| anyhow!("the pinned mc keymap: {e}"))?;
    let mut notes = Vec::new();
    for file in files(share, sysconf, env) {
        // A file that isn't there is skipped, as mc skips it. Bytes that aren't UTF-8 (a comment's) don't stop it.
        match std::fs::read(&file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => notes.push(format!("{}: {e}; not used", file.display())),
            Ok(bytes) => {
                let (read, error) = keyfile_upto(&String::from_utf8_lossy(&bytes));
                merge(&mut keymap, read);
                if let Some(e) = error {
                    notes.push(format!("{}: {e}; the rest of it not used", file.display()));
                }
            }
        }
    }
    let (sheet, keys, sections) = build(&keymap);
    if keys + sections > 0 {
        notes.push(format!(
            "leaving out {keys} key(s) keybind-find can't name and {sections} `:xmap` section(s) no key opens \
             (no ExtendedKeyMap key the section above keeps)"
        ));
    }
    Ok((sheet, notes))
}

/// The keymap files mc merges over its defaults, in its order: `<share>/mc.keymap`, the system file
/// (`$MC_DATADIR/mc.keymap`, else `<sysconf>/mc.keymap`), then one user keymap: `$MC_KEYMAP`, else the `keymap` value
/// in the `ini`'s `[Midnight-Commander]`, else `<config dir>/mc.keymap`. A name that is one of the two system files is
/// passed over, as mc passes it over.
// ponytail: `mc -K <file>` comes first in mc, but there's no mc command line here to read it from.
fn files(share: &Path, sysconf: &Path, env: Env) -> Vec<PathBuf> {
    let share_file = share.join("mc.keymap");
    let system = env("MC_DATADIR")
        .filter(|d| !d.is_empty())
        .map_or_else(|| sysconf.to_path_buf(), PathBuf::from)
        .join("mc.keymap");
    let config = config_dir(env);
    let named = |name: &str| full_name(name, config.as_deref(), share);
    let ini_keymap = || {
        let bytes = std::fs::read(config.as_ref()?.join("ini")).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let (ini, _) = keyfile_upto(&text);
        let (_, keys) = ini.iter().find(|(s, _)| s == "Midnight-Commander")?;
        let (_, value) = keys.iter().find(|(k, _)| k == "keymap")?;
        Some(value.clone()).filter(|v| !v.is_empty())
    };
    let user = env("MC_KEYMAP")
        .and_then(|n| named(&n))
        .filter(|f| *f != system && *f != share_file)
        .or_else(|| {
            ini_keymap()
                .and_then(|n| named(&n))
                .filter(|f| *f != system && *f != share_file)
        })
        .or_else(|| config.map(|c| c.join("mc.keymap")));
    [share_file, system].into_iter().chain(user).collect()
}

/// `$XDG_CONFIG_HOME/mc` (absolute only), else `~/.config/mc`.
fn config_dir(env: Env) -> Option<PathBuf> {
    env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| {
            env("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| Path::new(&h).join(".config"))
        })
        .map(|d| d.join("mc"))
}

/// A keymap name as mc resolves it (`load_setup_get_full_config_name`): `.keymap` added unless it's there; an
/// absolute name as it is (even when missing: mc then reads no user keymap); else its base name in the config dir,
/// then in `share`, whichever exists.
fn full_name(name: &str, config: Option<&Path>, share: &Path) -> Option<PathBuf> {
    let file = if name.ends_with(".keymap") {
        PathBuf::from(name)
    } else {
        PathBuf::from(format!("{name}.keymap"))
    };
    if file.is_absolute() {
        return Some(file);
    }
    let base = file.file_name()?;
    config
        .into_iter()
        .chain([share])
        .map(|d| d.join(base))
        .find(|f| f.exists())
}

/// A keyfile that must be whole (the pinned one).
fn keyfile(text: &str) -> Result<Keymap, String> {
    let (keymap, error) = keyfile_upto(text);
    error.map_or(Ok(keymap), Err)
}

/// A GLib keyfile as mc reads it: `[section]`, `key = value`, `#` comments and blank lines. A line that is none of
/// those ends it: what came before is kept, as GLib keeps it and mc uses it; the error comes back beside it. A section
/// named twice is one section.
// ponytail: laxer than GLib on odd lines (`[]`, an empty key, a locale `Key[de]`, `\;` escapes in a list).
fn keyfile_upto(text: &str) -> (Keymap, Option<String>) {
    let mut keymap: Keymap = Vec::new();
    let mut current = None;
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .trim_end()
            .strip_prefix('[')
            .and_then(|l| l.strip_suffix(']'))
        {
            current = Some(match keymap.iter().position(|(s, _)| s == name) {
                Some(i) => i,
                None => {
                    keymap.push((name.to_string(), Vec::new()));
                    keymap.len() - 1
                }
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return (
                keymap,
                Some(format!("line {}: not a key, a section or a comment", n + 1)),
            );
        };
        let Some(i) = current else {
            return (
                keymap,
                Some(format!("line {}: a key before any section", n + 1)),
            );
        };
        set(&mut keymap[i].1, key.trim(), value.trim());
    }
    (keymap, None)
}

/// Sets `key` in place, or appends it.
fn set(keys: &mut Vec<(String, String)>, key: &str, value: &str) {
    match keys.iter_mut().find(|(k, _)| k == key) {
        Some((_, v)) => *v = value.to_string(),
        None => keys.push((key.to_string(), value.to_string())),
    }
}

/// `read` laid over `keymap` as `mc_config_read_file(…, remove_empty)` does: each action's value replaced in place
/// (new actions and sections appended), an empty value removing the action.
fn merge(keymap: &mut Keymap, read: Keymap) {
    for (name, keys) in read {
        let i = match keymap.iter().position(|(s, _)| *s == name) {
            Some(i) => i,
            None => {
                keymap.push((name, Vec::new()));
                keymap.len() - 1
            }
        };
        let section = &mut keymap[i].1;
        for (key, value) in keys {
            if value.is_empty() {
                section.retain(|(k, _)| *k != key);
            } else {
                set(section, &key, &value);
            }
        }
    }
}

/// The sheet: a category per section, each action's keys in the section's order; a key already bound earlier in the
/// section isn't listed again (mc's lookup takes the first). A `:xmap` section's keys follow the first key its parent
/// binds to `ExtendedKeyMap` (none when an earlier action has it: mc never reaches that map). Also the keys with no
/// name here, and the `:xmap` sections left out for want of a prefix.
// ponytail: an action name mc doesn't know (it ignores those) is listed as written; there's no action table here.
// ponytail: keys compared in key notation, not mc's key codes: `ctrl-A` and `ctrl-shift-a` (one code in mc) are two
// keys here, and `shift-tab` and `backtab` (two codes) one.
fn build(keymap: &Keymap) -> (Sheet, usize, usize) {
    let mut sheet = Sheet {
        title: "mc".into(),
        provider: "mc".into(),
        binds: Default::default(),
    };
    let (mut keys_left, mut sections_left) = (0, 0);
    for (section, actions) in keymap.iter().filter(|(_, a)| !a.is_empty()) {
        let prefix = match section.strip_suffix(":xmap") {
            None => String::new(),
            Some(parent) => {
                let extended = keymap.iter().find(|(s, _)| s == parent).and_then(|(_, a)| {
                    bound(a)
                        .0
                        .into_iter()
                        // Action names are matched as mc does, in any case.
                        .find(|(_, act)| act.eq_ignore_ascii_case("ExtendedKeyMap"))
                });
                let Some((extended, _)) = extended else {
                    sections_left += 1;
                    continue;
                };
                format!("{extended} ")
            }
        };
        let (binds, left) = bound(actions);
        keys_left += left;
        for (key, action) in binds {
            sheet.binds.entry(section.clone()).or_default().push(Bind {
                key: format!("{prefix}{key}"),
                desc: action.to_string(),
                action: action.to_string(),
                when: Default::default(),
            });
        }
    }
    (sheet, keys_left, sections_left)
}

/// A section's keys in order with the action each is bound to, a key going to the first action that names it; and
/// the number of key names with no translation.
fn bound(actions: &[(String, String)]) -> (Vec<(String, &str)>, usize) {
    let (mut binds, mut left, mut seen) = (Vec::new(), 0, HashSet::new());
    for (action, value) in actions {
        for name in value.split(';').map(str::trim).filter(|n| !n.is_empty()) {
            match notation(name) {
                None => left += 1,
                Some(key) if seen.insert(key.clone()) => binds.push((key, action.as_str())),
                Some(_) => {}
            }
        }
    }
    (binds, left)
}

/// An mc key name (`ctrl-x`, `alt-shift-a`, `f19`, `kpplus`) in key notation, as `tty_keyname_to_keycode` reads it:
/// split on `-`, `+` and spaces, modifiers until the first key, the rest ignored. A one-letter capital is shift and the
/// letter. None for a name with no translation.
fn notation(name: &str) -> Option<String> {
    let mut mods = Vec::new();
    for part in name.split(['-', '+', ' ']).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.push("ctrl"),
            "alt" | "meta" | "ralt" => mods.push("alt"),
            "shift" => mods.push("shift"),
            _ => {
                let (keysym, extra) = key(part)?;
                mods.extend(extra);
                return Some(chord(&mods, &keysym));
            }
        }
    }
    None
}

/// One key of an mc name: its keysym, and a modifier the name implies (`backtab`, `complete`, a capital).
fn key(part: &str) -> Option<(String, Option<&'static str>)> {
    let mut chars = part.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return if c.is_ascii_uppercase() {
            Some((c.to_ascii_lowercase().to_string(), Some("shift")))
        } else {
            Some((single(c)?.to_string(), None))
        };
    }
    let lower = part.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return ((1..=24).contains(&n) && lower == format!("f{n}"))
            .then(|| (format!("F{n}"), None));
    }
    // `lib/tty/key.c`'s `key_name_conv_tab`, case-insensitive as mc's lookup.
    let (keysym, extra) = match lower.as_str() {
        "escape" | "esc" => ("Escape", None),
        "complete" => ("Tab", Some("alt")),
        "backtab" => ("Tab", Some("shift")),
        "backspace" | "bs" => ("BackSpace", None),
        "up" => ("Up", None),
        "down" => ("Down", None),
        "left" => ("Left", None),
        "right" => ("Right", None),
        "insert" | "ins" => ("Insert", None),
        "delete" | "del" => ("Delete", None),
        "home" => ("Home", None),
        "end" => ("End", None),
        "pgup" => ("Prior", None),
        "pgdn" => ("Next", None),
        // Only these three are keypad codes of their own in mc; its other `kp*` names are the plain keys.
        "kpasterisk" => ("KP_Multiply", None),
        "kpminus" => ("KP_Subtract", None),
        "kpplus" => ("KP_Add", None),
        "kpslash" | "slash" => ("slash", None),
        "kpleft" => ("Left", None),
        "kpright" => ("Right", None),
        "kpup" => ("Up", None),
        "kpdown" => ("Down", None),
        "kphome" => ("Home", None),
        "kpend" => ("End", None),
        "kpnpage" => ("Next", None),
        "kpppage" => ("Prior", None),
        "kpinsert" => ("Insert", None),
        "kpdelete" => ("Delete", None),
        "kpenter" => ("Return", None),
        // curses' keypad corners: 7 and 1 with Num Lock off.
        "a1" => ("KP_Home", None),
        "c1" => ("KP_End", None),
        "enter" => ("Return", None),
        "tab" => ("Tab", None),
        _ => {
            let c = match lower.as_str() {
                "asterisk" => '*',
                "minus" => '-',
                "plus" => '+',
                "dot" => '.',
                "lt" => '<',
                "gt" => '>',
                "equal" => '=',
                "comma" => ',',
                "apostrophe" => '\'',
                "colon" => ':',
                "semicolon" => ';',
                "exclamation" => '!',
                "question" => '?',
                "ampersand" => '&',
                "dollar" => '$',
                "quota" => '"',
                "percent" => '%',
                "caret" => '^',
                "tilda" => '~',
                "prime" => '`',
                "underline" | "understrike" => '_',
                "pipe" => '|',
                "lparenthesis" => '(',
                "rparenthesis" => ')',
                "lbracket" => '[',
                "rbracket" => ']',
                "lbrace" => '{',
                "rbrace" => '}',
                "space" => ' ',
                "backslash" => '\\',
                "number" | "hash" => '#',
                "at" => '@',
                _ => return None,
            };
            return Some((single(c)?.to_string(), None));
        }
    };
    Some((keysym.to_string(), extra))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mc")
            .join(name)
    }

    fn keys<'a>(s: &'a Sheet, section: &str, action: &str) -> Vec<&'a str> {
        s.binds
            .get(section)
            .into_iter()
            .flatten()
            .filter(|b| b.action == action)
            .map(|b| b.key.as_str())
            .collect()
    }

    /// An env with `HOME` at the fixture home, and the given extra variables.
    fn env_with(vars: &[(&str, String)]) -> impl Fn(&str) -> Option<String> {
        let mut vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        vars.push(("HOME".into(), fixture("home").display().to_string()));
        move |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn files_for(vars: &[(&str, String)]) -> Vec<PathBuf> {
        files(&fixture("share"), &fixture("etc"), &env_with(vars))
    }

    #[test]
    fn pinned_keymap() {
        assert!(PINNED.contains("mc-4.8.33.tar.xz") && PINNED.contains("src/keymap.c"));
        let keymap = keyfile(PINNED).unwrap();
        assert_eq!(keymap.len(), 15);
        let (s, keys_left, sections_left) = build(&keymap);
        assert_eq!((keys_left, sections_left), (0, 0), "every key translates");
        assert_eq!(keys(&s, "filemanager", "Copy"), ["F5"]);
        assert_eq!(keys(&s, "filemanager", "ChangePanel"), ["Tab", "ctrl+i"]);
        let xmap = &s.binds["filemanager:xmap"];
        assert!(!xmap.is_empty());
        assert!(
            xmap.iter().all(|b| b.key.starts_with("ctrl+x ")),
            "{xmap:?}"
        );
        assert_eq!(keys(&s, "filemanager:xmap", "ChangeMode"), ["ctrl+x c"]);
    }

    #[test]
    fn key_notation() {
        for (name, want) in [
            ("ctrl-x", Some("ctrl+x")),
            ("alt-shift-a", Some("alt+shift+a")),
            ("meta-c", Some("alt+c")),
            ("ctrl+space", Some("ctrl+space")),
            ("ctrl space", Some("ctrl+space")),
            ("f19", Some("F19")),
            ("F10", Some("F10")),
            ("f24", Some("F24")),
            ("kpplus", Some("KP_Add")),
            ("kpenter", Some("Return")),
            ("kpasterisk", Some("KP_Multiply")),
            ("kpminus", Some("KP_Subtract")),
            ("a1", Some("KP_Home")),
            ("c1", Some("KP_End")),
            ("esc", Some("Escape")),
            ("bs", Some("BackSpace")),
            ("ins", Some("Insert")),
            ("del", Some("Delete")),
            ("apostrophe", Some("apostrophe")),
            ("question", Some("question")),
            ("backslash", Some("backslash")),
            ("kpleft", Some("Left")),
            ("kpslash", Some("slash")),
            ("alt-question", Some("alt+question")),
            ("alt-prime", Some("alt+grave")),
            ("ctrl-backslash", Some("ctrl+backslash")),
            ("shift-tab", Some("shift+Tab")),
            ("backtab", Some("shift+Tab")),
            ("complete", Some("alt+Tab")),
            ("A", Some("shift+a")),
            ("a", Some("a")),
            ("enter", Some("Return")),
            ("pgdn", Some("Next")),
            ("Control-Home", Some("ctrl+Home")),
            ("ralt-dot", Some("alt+period")),
            ("exclamation", Some("exclam")),
            // mc reads up to the first key.
            ("alt-gt ctrl-e", Some("alt+greater")),
            ("f25", None),
            ("f0", None),
            ("no-such-key", None),
            ("ctrl", None),
            ("", None),
        ] {
            assert_eq!(notation(name).as_deref(), want, "{name:?}");
        }
    }

    #[test]
    fn file_order() {
        let home = fixture("home/.config/mc");
        let (share, etc) = (fixture("share/mc.keymap"), fixture("etc/mc.keymap"));
        // No user keymap named: the default one in the config dir.
        assert_eq!(
            files_for(&[]),
            [share.clone(), etc.clone(), home.join("mc.keymap")]
        );
        // `MC_DATADIR` replaces /etc/mc.
        let datadir = fixture("datadir").display().to_string();
        assert_eq!(
            files_for(&[("MC_DATADIR", datadir)])[1],
            fixture("datadir/mc.keymap")
        );
        // `MC_KEYMAP`: absolute (kept even when missing), relative (in the config dir), without `.keymap`, and in
        // the share dir when the config dir hasn't it.
        let abs = fixture("merge.keymap").display().to_string();
        assert_eq!(
            files_for(&[("MC_KEYMAP", abs.clone())])[2],
            fixture("merge.keymap")
        );
        assert_eq!(
            files_for(&[("MC_KEYMAP", "/nowhere/x".into())])[2],
            PathBuf::from("/nowhere/x.keymap")
        );
        assert_eq!(
            files_for(&[("MC_KEYMAP", "mine".into())])[2],
            home.join("mine.keymap")
        );
        assert_eq!(
            files_for(&[("MC_KEYMAP", "sub/emacs.keymap".into())])[2],
            home.join("emacs.keymap")
        );
        assert_eq!(
            files_for(&[("MC_KEYMAP", "mc".into())])[2],
            fixture("home/.config/mc/mc.keymap")
        );
        // Not in the config dir: the share dir.
        assert_eq!(
            files_for(&[("MC_KEYMAP", "shared".into())])[2],
            fixture("share/shared.keymap")
        );
        // Naming the share dir's own mc.keymap: passed over, as the system file is.
        let share_name = share.display().to_string();
        assert_eq!(
            files_for(&[("MC_KEYMAP", share_name)])[2],
            home.join("mc.keymap")
        );
        // Not found anywhere: the next source.
        assert_eq!(
            files_for(&[("MC_KEYMAP", "nosuch".into())])[2],
            home.join("mc.keymap")
        );
        // Naming a system file: passed over.
        let etc_name = etc.display().to_string();
        assert_eq!(
            files_for(&[("MC_KEYMAP", etc_name)])[2],
            home.join("mc.keymap")
        );
        // The `ini`'s `keymap`, under `XDG_CONFIG_HOME`; `MC_KEYMAP` before it.
        let ini = fixture("ini-config").display().to_string();
        let ini_home = fixture("ini-config/mc");
        assert_eq!(
            files_for(&[("XDG_CONFIG_HOME", ini.clone())])[2],
            ini_home.join("mine.keymap")
        );
        assert_eq!(
            files_for(&[("XDG_CONFIG_HOME", ini), ("MC_KEYMAP", abs)])[2],
            fixture("merge.keymap")
        );
        // A relative or empty `XDG_CONFIG_HOME` is unset.
        let xdg = fixture("xdg").display().to_string();
        assert_eq!(
            files_for(&[("XDG_CONFIG_HOME", xdg)])[2],
            fixture("xdg/mc/mc.keymap")
        );
        for relative in ["xdg", ""] {
            assert_eq!(
                files_for(&[("XDG_CONFIG_HOME", relative.into())])[2],
                home.join("mc.keymap")
            );
        }
        // No home and no XDG: the two system files only.
        assert_eq!(
            files(&fixture("share"), &fixture("etc"), &|_| None).len(),
            2
        );
    }

    #[test]
    fn files_laid_over_in_order() {
        let (s, notes) =
            sheet_with(PINNED, &fixture("share"), &fixture("etc"), &env_with(&[])).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        // The system file over the share one, both over the defaults; the user's after.
        assert_eq!(keys(&s, "filemanager", "Help"), ["F12"]);
        for (action, key) in [
            ("Share", "F13"),
            ("System", "alt+w"),
            ("UserDefault", "alt+u"),
        ] {
            assert_eq!(keys(&s, "filemanager", action), [key], "{action}");
        }
        let datadir = fixture("datadir").display().to_string();
        let (s, _) = sheet_with(
            PINNED,
            &fixture("share"),
            &fixture("etc"),
            &env_with(&[("MC_DATADIR", datadir)]),
        )
        .unwrap();
        assert_eq!(keys(&s, "filemanager", "DataDir"), ["alt+z"]);
        // /etc/mc's file isn't read: its Help = f12 is gone.
        assert_eq!(keys(&s, "filemanager", "Help"), ["F11"]);
        // The upstream emacs keymap as the user's: it parses and lays over.
        let (s, notes) = sheet_with(
            PINNED,
            &fixture("share"),
            &fixture("etc"),
            &env_with(&[("MC_KEYMAP", "emacs".into())]),
        )
        .unwrap();
        // Upstream's `[menu]` has `ctr-a`, which mc can't read either.
        assert_eq!(
            notes,
            ["leaving out 1 key(s) keybind-find can't name and 0 `:xmap` section(s) no key opens (no ExtendedKeyMap key the section above keeps)"]
        );
        assert!(keys(&s, "filemanager", "UserDefault").is_empty());
        assert_eq!(keys(&s, "editor", "Store"), ["alt+w"]);
        // Its Paste = ctrl-y loses to DeleteLine, earlier in mc's [editor]: as in mc.
        assert!(keys(&s, "editor", "Paste").is_empty());
        // It moves WordRight off ctrl-x, so the editor's extended map is reached.
        assert_eq!(keys(&s, "editor", "WordRight"), ["ctrl+Right", "alt+f"]);
        assert_eq!(keys(&s, "editor:xmap", "EditNew"), ["ctrl+x k"]);
    }

    #[test]
    fn merging() {
        let mut keymap = keyfile(PINNED).unwrap();
        let read = std::fs::read_to_string(fixture("merge.keymap")).unwrap();
        merge(&mut keymap, keyfile(&read).unwrap());
        let (s, keys_left, sections_left) = build(&keymap);
        // Replaced, removed.
        assert_eq!(keys(&s, "filemanager", "Copy"), ["alt+5", "shift+F5"]);
        assert!(keys(&s, "filemanager", "Move").is_empty());
        // f9 on Menu and Quit: Menu, earlier in the section, keeps it.
        assert_eq!(keys(&s, "filemanager", "Menu"), ["F9"]);
        assert_eq!(keys(&s, "filemanager", "Quit"), ["F10"]);
        // Replaced in place: Help, before Delete, takes f8 from it.
        assert_eq!(keys(&s, "filemanager", "Help"), ["F1", "F8"]);
        assert!(keys(&s, "filemanager", "Delete").is_empty());
        // A new action comes after the defaults.
        let fm = &s.binds["filemanager"];
        let mine = fm.iter().position(|b| b.action == "MyAction").unwrap();
        assert_eq!(fm[mine].key, "ctrl+alt+q");
        assert_eq!(fm.last().unwrap().key, "ctrl+alt+w");
        let quit_quiet = fm.iter().position(|b| b.action == "QuitQuiet").unwrap();
        assert!(quit_quiet < mine, "a default after the new action");
        // `[editor:xmap]` with the editor's ExtendedKeyMap removed: left out, counted; an unknown key counted.
        assert!(!s.binds.contains_key("editor:xmap"));
        assert_eq!((keys_left, sections_left), (1, 1));
        // A section named twice in one file is one section.
        let twice = keyfile("[a]\nx = f1\n[b]\ny = f2\n[a]\nz = f3\n").unwrap();
        assert_eq!(twice.len(), 2);
        assert_eq!(twice[0].1.len(), 2);
    }

    #[test]
    fn xmap_prefix() {
        let mut keymap = keyfile(PINNED).unwrap();
        let read = std::fs::read_to_string(fixture("xmap.keymap")).unwrap();
        merge(&mut keymap, keyfile(&read).unwrap());
        let (s, keys_left, sections_left) = build(&keymap);
        // The first key the parent binds to ExtendedKeyMap: not `bogus` (no name), not f2 (UserMenu's).
        assert_eq!(keys(&s, "filemanager:xmap", "Hello"), ["ctrl+y z"]);
        let xmap = &s.binds["filemanager:xmap"];
        assert!(
            xmap.iter().all(|b| b.key.starts_with("ctrl+y ")),
            "{xmap:?}"
        );
        // The editor's ExtendedKeyMap loses ctrl-x to WordRight: its xmap is left out, counted; an empty one isn't.
        assert!(!s.binds.contains_key("editor:xmap"));
        assert!(!s.binds.contains_key("viewer:xmap"));
        assert_eq!((keys_left, sections_left), (1, 1));
        // The action name in any case, as mc reads it.
        let (s, _, _) = build(&keyfile("[a]\nextendedKEYmap = ctrl-y\n[a:xmap]\nX = z\n").unwrap());
        assert_eq!(keys(&s, "a:xmap", "X"), ["ctrl+y z"]);
    }

    #[test]
    fn failures() {
        let none = fixture("nowhere");
        // No files at all: the pinned sheet, no note.
        let (s, notes) = sheet_with(PINNED, &none, &none, &|_| None).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.binds, build(&keyfile(PINNED).unwrap()).0.binds);
        // A malformed user file: a note; its lines before the bad one are used, as mc uses them.
        let bad = fixture("malformed.keymap").display().to_string();
        let (s, notes) = sheet_with(
            PINNED,
            &fixture("share"),
            &fixture("etc"),
            &env_with(&[("MC_KEYMAP", bad)]),
        )
        .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0]
                .contains("malformed.keymap: line 3: not a key, a section or a comment; the rest of it not used"),
            "{notes:?}"
        );
        assert_eq!(keys(&s, "filemanager", "Copy"), ["F9"]);
        assert_eq!(keys(&s, "filemanager", "Help"), ["F12"]);
        // Bytes that aren't UTF-8 in a comment don't stop a file.
        let latin1 =
            std::env::temp_dir().join(format!("kbf-mc-latin1-{}.keymap", std::process::id()));
        std::fs::write(&latin1, b"# caf\xe9\n[filemanager]\nCopy = f9\n").unwrap();
        let named = latin1.display().to_string();
        let (s, notes) =
            sheet_with(PINNED, &none, &none, &env_with(&[("MC_KEYMAP", named)])).unwrap();
        let _ = std::fs::remove_file(&latin1);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(keys(&s, "filemanager", "Copy"), ["F9"]);
        // An unreadable one (a directory named `.keymap`): a note.
        let dir = std::env::temp_dir().join(format!("kbf-mc-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("d.keymap")).unwrap();
        let named = dir.join("d.keymap").display().to_string();
        let (_, notes) =
            sheet_with(PINNED, &none, &none, &env_with(&[("MC_KEYMAP", named)])).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("d.keymap: ") && notes[0].ends_with("; not used"),
            "{notes:?}"
        );
        assert_eq!(
            keyfile("x = 1"),
            Err("line 1: a key before any section".into())
        );
        let (kept, error) = keyfile_upto("[a]\nx = f1\nbad line\nz = f3\n");
        assert_eq!(
            kept,
            [("a".to_string(), vec![("x".to_string(), "f1".to_string())])]
        );
        assert_eq!(
            error.as_deref(),
            Some("line 3: not a key, a section or a comment")
        );
        // A malformed pinned file is an error.
        assert!(sheet_with("nonsense", &none, &none, &|_| None).is_err());
    }

    /// The installed system keymap, when there is one, is read and used.
    #[test]
    fn real_system_keymap() {
        let etc = Path::new(SYSCONF);
        if !etc.join("mc.keymap").exists() {
            eprintln!("skipping: no {}/mc.keymap", etc.display());
            return;
        }
        let none = fixture("nowhere");
        let (s, notes) = sheet_with(PINNED, &none, etc, &|_| None).unwrap();
        // Read, not passed over; a count of keys with no name is fine (4.8.33's own file has `ctr-a`).
        assert!(notes.iter().all(|n| !n.ends_with("not used")), "{notes:?}");
        assert!(!s.binds["filemanager"].is_empty());
    }
}
