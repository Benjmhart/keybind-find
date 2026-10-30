//! Midnight Commander: its keymap built as mc builds it (`src/keymap.c`), from its compiled-in defaults pinned
//! (`data/mc/`), then `/usr/share/mc/mc.keymap`, the system `mc.keymap` and one user keymap laid over them. Shown when
//! mc, mcedit, mcview or mcdiff is the program in the focused terminal. Files are only read.

use super::nvim::single;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::anyhow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// mc 4.8.33's compiled-in defaults. The installed system keymap (distros ship the full default keymap as
/// `/etc/mc/mc.keymap`) is laid over them, so an older or newer mc's defaults win wherever that file is there.
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
fn sheet_with(
    pinned: &str,
    share: &Path,
    sysconf: &Path,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut keymap = keyfile(pinned).map_err(|e| anyhow!("the pinned mc keymap: {e}"))?;
    let mut notes = Vec::new();
    for file in files(share, sysconf, env) {
        // A file that isn't there is skipped, as mc skips it.
        match std::fs::read_to_string(&file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => notes.push(format!("{}: {e}; not used", file.display())),
            Ok(text) => match keyfile(&text) {
                Ok(read) => merge(&mut keymap, read),
                Err(e) => notes.push(format!("{}: {e}; not used", file.display())),
            },
        }
    }
    let (sheet, keys, sections) = build(&keymap);
    if keys + sections > 0 {
        notes.push(format!(
            "leaving out {keys} key(s) keybind-find can't name and {sections} `:xmap` section(s) with no \
             ExtendedKeyMap"
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
        let text = std::fs::read_to_string(config.as_ref()?.join("ini")).ok()?;
        let ini = keyfile(&text).ok()?;
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

/// A GLib keyfile as mc reads it: `[section]`, `key = value`, `#` comments and blank lines; anything else makes the
/// whole file unreadable (as `g_key_file_load_from_file` refuses it). A section named twice is one section.
fn keyfile(text: &str) -> Result<Keymap, String> {
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
            return Err(format!("line {}: not a key, a section or a comment", n + 1));
        };
        let Some(i) = current else {
            return Err(format!("line {}: a key before any section", n + 1));
        };
        set(&mut keymap[i].1, key.trim(), value.trim());
    }
    Ok(keymap)
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
/// section isn't listed again (mc's lookup takes the first). A `:xmap` section's keys follow its parent's
/// `ExtendedKeyMap`. Also the keys with no name here, and the `:xmap` sections without a prefix.
// ponytail: an action name mc doesn't know (it ignores those) is listed as written; there's no action table here.
// ponytail: keys compared in key notation, not mc's key codes, so `tab` and `ctrl-i` (one code in mc) are both listed.
fn build(keymap: &Keymap) -> (Sheet, usize, usize) {
    let mut sheet = Sheet {
        title: "mc".into(),
        provider: "mc".into(),
        binds: Default::default(),
    };
    let (mut keys_left, mut sections_left) = (0, 0);
    for (section, actions) in keymap {
        let prefix = match section.strip_suffix(":xmap") {
            None => String::new(),
            Some(parent) => {
                let extended = keymap
                    .iter()
                    .find(|(s, _)| s == parent)
                    .and_then(|(_, a)| a.iter().find(|(k, _)| k == "ExtendedKeyMap"))
                    .and_then(|(_, v)| v.split(';').map(str::trim).find(|n| !n.is_empty()))
                    .and_then(notation);
                let Some(extended) = extended else {
                    sections_left += 1;
                    continue;
                };
                format!("{extended} ")
            }
        };
        let mut seen = HashSet::new();
        for (action, value) in actions {
            for name in value.split(';').map(str::trim).filter(|n| !n.is_empty()) {
                let Some(key) = notation(name) else {
                    keys_left += 1;
                    continue;
                };
                let key = format!("{prefix}{key}");
                if seen.insert(key.clone()) {
                    sheet.binds.entry(section.clone()).or_default().push(Bind {
                        key,
                        desc: action.clone(),
                        action: action.clone(),
                        when: Default::default(),
                    });
                }
            }
        }
    }
    (sheet, keys_left, sections_left)
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
        "kpslash" => ("KP_Divide", None),
        "kpasterisk" => ("KP_Multiply", None),
        "kpminus" => ("KP_Subtract", None),
        "kpplus" => ("KP_Add", None),
        "kpleft" => ("KP_Left", None),
        "kpright" => ("KP_Right", None),
        "kpup" => ("KP_Up", None),
        "kpdown" => ("KP_Down", None),
        "kphome" => ("KP_Home", None),
        "kpend" => ("KP_End", None),
        "kpnpage" => ("KP_Next", None),
        "kpppage" => ("KP_Prior", None),
        "kpinsert" => ("KP_Insert", None),
        "kpdelete" => ("KP_Delete", None),
        "kpenter" => ("KP_Enter", None),
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
                "slash" => '/',
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
            ("System", "alt+y"),
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
        assert!(keys(&s, "filemanager", "System").is_empty());
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
            ["leaving out 1 key(s) keybind-find can't name and 0 `:xmap` section(s) with no ExtendedKeyMap"]
        );
        assert!(keys(&s, "filemanager", "UserDefault").is_empty());
        assert!(!s.binds["editor"].is_empty());
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
        // A new action comes after the defaults.
        let fm = &s.binds["filemanager"];
        let mine = fm.iter().position(|b| b.action == "MyAction").unwrap();
        assert_eq!(fm[mine].key, "ctrl+alt+q");
        assert!(fm[..mine].iter().all(|b| b.action != "MyAction"));
        assert_eq!(fm.last().unwrap().key, "ctrl+alt+w");
        // `[editor:xmap]` with the editor's ExtendedKeyMap removed: left out, counted; an unknown key counted.
        assert!(!s.binds.contains_key("editor:xmap"));
        assert_eq!((keys_left, sections_left), (1, 1));
        // A section named twice in one file is one section.
        let twice = keyfile("[a]\nx = f1\n[b]\ny = f2\n[a]\nz = f3\n").unwrap();
        assert_eq!(twice.len(), 2);
        assert_eq!(twice[0].1.len(), 2);
    }

    #[test]
    fn failures() {
        let none = fixture("nowhere");
        // No files at all: the pinned sheet, no note.
        let (s, notes) = sheet_with(PINNED, &none, &none, &|_| None).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.binds, build(&keyfile(PINNED).unwrap()).0.binds);
        // A malformed user file: a note, the rest used.
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
                .contains("malformed.keymap: line 3: not a key, a section or a comment; not used"),
            "{notes:?}"
        );
        assert_eq!(keys(&s, "filemanager", "Copy"), ["F5"]);
        assert_eq!(keys(&s, "filemanager", "Help"), ["F12"]);
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
        // A malformed pinned file is an error.
        assert!(sheet_with("nonsense", &none, &none, &|_| None).is_err());
    }

    /// The installed system keymap, when there is one, reads with no note.
    #[test]
    fn real_system_keymap() {
        let etc = Path::new(SYSCONF);
        if !etc.join("mc.keymap").exists() {
            eprintln!("skipping: no {}/mc.keymap", etc.display());
            return;
        }
        let none = fixture("nowhere");
        let (s, notes) = sheet_with(PINNED, &none, etc, &|_| None).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!s.binds["filemanager"].is_empty());
    }
}
