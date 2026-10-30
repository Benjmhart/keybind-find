//! Obsidian: its default hotkeys pinned for one release (`data/obsidian/`, read from the release's app bundle), with
//! the focused vault's `.obsidian/hotkeys.json` laid over them. Shown when an Obsidian window has focus. Obsidian is
//! a GUI app and is never run: only `obsidian.json` (the vault list) and the vault's `hotkeys.json` are read.

use super::nvim::single;
use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The newest release at the time; there is no version probe, so every Obsidian gets these defaults.
const PINNED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/obsidian/1.14.5.toml"
));

/// The Flatpak's app id.
const FLATPAK: &str = "md.obsidian.Obsidian";

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pinned {
    obsidian_version: String,
    command: Vec<Command>,
}

#[derive(Deserialize)]
struct Command {
    id: String,
    name: String,
    hotkeys: Vec<Hotkey>,
}

/// The API's `Hotkey`, in the pinned file and in `hotkeys.json`.
#[derive(Deserialize)]
struct Hotkey {
    modifiers: Vec<String>,
    key: String,
}

#[derive(Deserialize)]
struct VaultList {
    #[serde(default)]
    vaults: BTreeMap<String, Vault>,
}

#[derive(Deserialize)]
struct Vault {
    path: PathBuf,
}

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(PINNED, ctx.title.as_deref(), &env)?;
    for note in notes {
        eprintln!("keybind-find: obsidian: {note}");
    }
    Ok(sheet)
}

/// The sheet for the window titled `title`, with `env` for the config dirs. Also the lines for stderr.
fn sheet_with(pinned: &str, title: Option<&str>, env: Env) -> anyhow::Result<(Sheet, Vec<String>)> {
    let pinned: Pinned = toml::from_str(pinned).context("reading the pinned Obsidian hotkeys")?;
    let mut notes = Vec::new();
    let custom = vault(title, env, &mut notes)
        .map(|v| custom_hotkeys(&v, &mut notes))
        .unwrap_or_default();
    let (sheet, left_out) = parse(&pinned, &custom);
    if left_out > 0 {
        notes.push(format!(
            "{left_out} hotkey(s) left out: a key or modifier keybind-find can't name"
        ));
    }
    Ok((sheet, notes))
}

/// Where Obsidian keeps `obsidian.json`, in the order looked at: the native (and AppImage) dir, the Flatpak's, the
/// Snap's. A relative or empty `XDG_CONFIG_HOME` is unset.
fn config_dirs(env: Env) -> Vec<PathBuf> {
    let home = env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let config = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let sandboxed = home.iter().flat_map(|h| {
        [
            h.join(".var/app").join(FLATPAK).join("config/obsidian"),
            h.join("snap/obsidian/current/.config/obsidian"),
        ]
    });
    config
        .map(|c| c.join("obsidian"))
        .into_iter()
        .chain(sandboxed)
        .collect()
}

/// The focused window's vault, from the first readable `obsidian.json`: the one the title names, else the only one.
/// None when there is no list, or several and the title names none (with a note).
fn vault(title: Option<&str>, env: Env, notes: &mut Vec<String>) -> Option<PathBuf> {
    let (file, text) = config_dirs(env)
        .into_iter()
        .map(|d| d.join("obsidian.json"))
        .find_map(|f| std::fs::read_to_string(&f).ok().map(|t| (f, t)))?;
    let list: VaultList = match serde_json::from_str(&text) {
        Ok(list) => list,
        Err(e) => {
            notes.push(format!("{}: {e}; listing the defaults", file.display()));
            return None;
        }
    };
    let paths: Vec<PathBuf> = list.vaults.into_values().map(|v| v.path).collect();
    if let Some(p) = title.and_then(|t| by_title(t, &paths)) {
        return Some(p.clone());
    }
    match paths.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        _ => {
            notes.push(format!(
                "no vault in {} matches the window title; listing the defaults",
                file.display()
            ));
            None
        }
    }
}

/// The vault a window title names: `<vault> - Obsidian <version>`, or `<note> - <vault> - Obsidian <version>`, by
/// the vault folder's name. The longest name wins when two match (vaults `Notes` and `My Notes`).
fn by_title<'a>(title: &str, paths: &'a [PathBuf]) -> Option<&'a PathBuf> {
    let (head, version) = title.rsplit_once(" - Obsidian ")?;
    if !version.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    paths
        .iter()
        .filter_map(|p| Some((p, p.file_name()?.to_str()?)))
        .filter(|(_, name)| {
            head == *name || head.strip_suffix(name).is_some_and(|h| h.ends_with(" - "))
        })
        .max_by_key(|(_, name)| name.len())
        .map(|(p, _)| p)
}

/// `<vault>/.obsidian/hotkeys.json`: command id → its custom hotkeys. None there is none; a vault that is gone, or a
/// file that can't be read or parsed, is a note and none.
fn custom_hotkeys(vault: &Path, notes: &mut Vec<String>) -> BTreeMap<String, Vec<Hotkey>> {
    if let Err(e) = std::fs::read_dir(vault) {
        notes.push(format!(
            "vault {}: {e}; listing the defaults",
            vault.display()
        ));
        return BTreeMap::new();
    }
    // ponytail: only `.obsidian`; a vault's own config folder name is kept inside the app, out of sight.
    let file = vault.join(".obsidian/hotkeys.json");
    let parsed = match std::fs::read_to_string(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(e) => Err(e.to_string()),
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
    };
    parsed.unwrap_or_else(|e| {
        notes.push(format!("{}: {e}; listing the defaults", file.display()));
        BTreeMap::new()
    })
}

/// The sheet: each pinned command with its default hotkeys, or its `custom` ones when it has an entry there (even
/// an empty one, as in Obsidian); ids only in `custom` (a plugin's) are described by their id. Category is the id's
/// prefix. Also the number of hotkeys left out because a key or modifier has no name here.
fn parse(pinned: &Pinned, custom: &BTreeMap<String, Vec<Hotkey>>) -> (Sheet, usize) {
    let mut sheet = Sheet {
        // The defaults' release: the installed one may differ (there's no version probe).
        title: format!("Obsidian {}", pinned.obsidian_version),
        provider: "obsidian".into(),
        binds: Default::default(),
    };
    let names: BTreeMap<&str, &str> = pinned
        .command
        .iter()
        .map(|c| (c.id.as_str(), c.name.as_str()))
        .collect();
    let defaults = pinned
        .command
        .iter()
        .filter(|c| !custom.contains_key(&c.id))
        .map(|c| (c.id.as_str(), &c.hotkeys));
    let mut left_out = 0;
    for (id, hotkeys) in defaults.chain(custom.iter().map(|(id, h)| (id.as_str(), h))) {
        let category = id.split_once(':').map_or(id, |(c, _)| c);
        for hotkey in hotkeys {
            let Some(key) = notation(hotkey) else {
                left_out += 1;
                continue;
            };
            sheet
                .binds
                .entry(category.to_string())
                .or_default()
                .push(Bind {
                    key,
                    desc: names.get(id).copied().unwrap_or(id).to_string(),
                    action: id.to_string(),
                    when: Default::default(),
                });
        }
    }
    (sheet, left_out)
}

/// A hotkey in key notation: `Mod` is Ctrl off macOS, and the key is an MDN key value. None for a modifier or key
/// with no name here.
fn notation(hotkey: &Hotkey) -> Option<String> {
    let mods = hotkey
        .modifiers
        .iter()
        .map(|m| match m.as_str() {
            "Mod" | "Ctrl" => Some("ctrl"),
            "Alt" => Some("alt"),
            "Shift" => Some("shift"),
            "Meta" => Some("super"),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(chord(&mods, &keysym(&hotkey.key)?))
}

/// An MDN key value as an X keysym: a letter in either case is that (lower-case) key, punctuation by name.
fn keysym(key: &str) -> Option<String> {
    let named = match key {
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "Enter" => "Return",
        "PageUp" => "Prior",
        "PageDown" => "Next",
        "Backspace" => "BackSpace",
        "Tab" | "Escape" | "Delete" | "Home" | "End" => key,
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_string());
    }
    let function = key
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n) && key == format!("F{n}"));
    if function {
        return Some(key.to_string());
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => single(c.to_ascii_lowercase()).map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/obsidian")
    }

    fn pinned() -> Pinned {
        toml::from_str(PINNED).unwrap()
    }

    fn keys(s: &Sheet, id: &str) -> Vec<String> {
        s.binds
            .values()
            .flatten()
            .filter(|b| b.action == id)
            .map(|b| b.key.clone())
            .collect()
    }

    fn hotkey(modifiers: &[&str], key: &str) -> Hotkey {
        Hotkey {
            modifiers: modifiers.iter().map(|m| m.to_string()).collect(),
            key: key.into(),
        }
    }

    /// An empty dir under the temp dir, unique to `name` and this test run.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kbf-obsidian-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes an `obsidian.json` listing `vaults` into `dir`.
    fn vault_list(dir: &Path, vaults: &[&Path]) {
        let vaults: serde_json::Map<String, serde_json::Value> = vaults
            .iter()
            .enumerate()
            .map(|(i, p)| (format!("id{i}"), serde_json::json!({ "path": p, "ts": i })))
            .collect();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("obsidian.json"),
            serde_json::json!({ "vaults": vaults }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn pinned_file() {
        assert!(PINNED.contains("obsidian-1.14.5.asar.gz"), "release asset");
        let sha = PINNED.split("SHA-256 ").nth(1).unwrap();
        assert!(sha.len() > 64 && sha[..64].bytes().all(|b| b.is_ascii_hexdigit()));
        let p = pinned();
        assert_eq!(p.obsidian_version, "1.14.5");
        let mut ids: Vec<_> = p.command.iter().map(|c| &c.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), p.command.len(), "duplicate id");
        let (sheet, left_out) = parse(&p, &BTreeMap::new());
        assert_eq!(left_out, 0, "every key translates");
        assert_eq!(sheet.title, "Obsidian 1.14.5");
        assert_eq!(keys(&sheet, "command-palette:open"), ["ctrl+p"]);
        let palette = &sheet.binds["command-palette"][0];
        assert_eq!(palette.desc, "Open command palette");
        assert_eq!(
            keys(&sheet, "workspace:next-tab"),
            ["ctrl+Tab", "ctrl+Next"]
        );
        assert_eq!(keys(&sheet, "app:go-back"), ["ctrl+alt+Left"]);
        assert_eq!(keys(&sheet, "workspace:goto-tab-8"), ["ctrl+8"]);
    }

    #[test]
    fn key_table() {
        for (mods, key, want) in [
            (&["Mod"][..], "p", Some("ctrl+p")),
            (&["Ctrl"], "P", Some("ctrl+p")),
            (&["Meta"], "a", Some("super+a")),
            (&["Mod", "Ctrl", "Shift"], "A", Some("ctrl+shift+a")),
            (&["Mod", "Alt"], "ArrowLeft", Some("ctrl+alt+Left")),
            (&["Alt"], "Enter", Some("alt+Return")),
            (&["Mod"], ",", Some("ctrl+comma")),
            (&["Mod"], "/", Some("ctrl+slash")),
            (&[], "F2", Some("F2")),
            (&[], "F24", Some("F24")),
            (&[], " ", Some("space")),
            (&[], "PageUp", Some("Prior")),
            (&[], "Backspace", Some("BackSpace")),
            (&[], "F25", None),
            (&[], "F02", None),
            (&[], "MediaPlayPause", None),
            (&[], "é", None),
            (&[], "", None),
            (&["Hyper"], "a", None),
        ] {
            assert_eq!(
                notation(&hotkey(mods, key)).as_deref(),
                want,
                "{mods:?} {key:?}"
            );
        }
    }

    #[test]
    fn vault_choice_by_title() {
        let text = std::fs::read_to_string(fixtures().join("obsidian.json")).unwrap();
        let list: VaultList = serde_json::from_str(&text).unwrap();
        let both: Vec<PathBuf> = list.vaults.into_values().map(|v| v.path).collect();
        let pick = |title: &str, paths: &[PathBuf]| by_title(title, paths).cloned();
        let my = Some(PathBuf::from("/home/user/My Notes"));
        assert_eq!(pick("x - My Notes - Obsidian 1.14.4", &both), my);
        assert_eq!(pick("My Notes - Obsidian 1.14.4", &both), my);
        assert_eq!(
            pick("Notes - Obsidian 1.14.4", &both),
            Some(PathBuf::from("/home/user/Notes"))
        );
        let work = [PathBuf::from("/v/Work"), PathBuf::from("/v/Other")];
        let at_work = Some(work[0].clone());
        assert_eq!(pick("Note - Work - Obsidian 1.14.4", &work), at_work);
        assert_eq!(pick("Work - Obsidian 1.14.4", &work), at_work);
        // A note named `a - Work` in vault `Work`.
        assert_eq!(pick("a - Work - Work - Obsidian 1.14.4", &work), at_work);
        // Not a vault name: a suffix inside a word, a pop-out's `Obsidian`, no version.
        for title in [
            "Homework - Obsidian 1.14.4",
            "Obsidian",
            "Work - Obsidian",
            "Work - Obsidian beta",
            "",
        ] {
            assert_eq!(pick(title, &work), None, "{title:?}");
        }
    }

    #[test]
    fn vault_from_the_list() {
        let dir = scratch("list");
        let env_for =
            |d: PathBuf| move |k: &str| (k == "XDG_CONFIG_HOME").then(|| d.display().to_string());
        let one = [Path::new("/v/Solo")];
        vault_list(&dir.join("one/obsidian"), &one);
        let env = env_for(dir.join("one"));
        let mut notes = vec![];
        // One vault and a title that names none: that vault.
        assert_eq!(
            vault(Some("Obsidian"), &env, &mut notes),
            Some("/v/Solo".into())
        );
        assert_eq!(vault(None, &env, &mut notes), Some("/v/Solo".into()));
        assert!(notes.is_empty(), "{notes:?}");
        // Two and no match: none, with the note.
        vault_list(
            &dir.join("two/obsidian"),
            &[Path::new("/v/A"), Path::new("/v/B")],
        );
        let env = env_for(dir.join("two"));
        assert_eq!(
            vault(Some("B - Obsidian 1.14.5"), &env, &mut notes),
            Some("/v/B".into())
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(vault(Some("Obsidian"), &env, &mut notes), None);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("no vault"), "{notes:?}");
        // No list, or an empty one: none, no note.
        notes.clear();
        assert_eq!(vault(None, &env_for(dir.join("none")), &mut notes), None);
        std::fs::create_dir_all(dir.join("empty/obsidian")).unwrap();
        std::fs::write(dir.join("empty/obsidian/obsidian.json"), r#"{"vaults":{}}"#).unwrap();
        assert_eq!(vault(None, &env_for(dir.join("empty")), &mut notes), None);
        assert!(notes.is_empty(), "{notes:?}");
        // A malformed list: a note.
        std::fs::create_dir_all(dir.join("bad/obsidian")).unwrap();
        std::fs::write(dir.join("bad/obsidian/obsidian.json"), "{").unwrap();
        assert_eq!(vault(None, &env_for(dir.join("bad")), &mut notes), None);
        assert_eq!(notes.len(), 1, "{notes:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_dirs_in_order() {
        let home = scratch("home");
        let h = home.display().to_string();
        let env = |xdg: Option<&'static str>| {
            let h = h.clone();
            move |k: &str| match k {
                "HOME" => Some(h.clone()),
                "XDG_CONFIG_HOME" => xdg.map(str::to_string),
                _ => None,
            }
        };
        assert_eq!(
            config_dirs(&env(Some("/xdg"))),
            [
                PathBuf::from("/xdg/obsidian"),
                home.join(".var/app/md.obsidian.Obsidian/config/obsidian"),
                home.join("snap/obsidian/current/.config/obsidian"),
            ]
        );
        for relative in [Some("rel"), Some(""), None] {
            assert_eq!(
                config_dirs(&env(relative))[0],
                home.join(".config/obsidian")
            );
        }
        assert!(config_dirs(&|_: &str| None).is_empty());
        // Flatpak, then Snap, when `~/.config` has no list.
        let flatpak = home.join(".var/app/md.obsidian.Obsidian/config/obsidian");
        let snap = home.join("snap/obsidian/current/.config/obsidian");
        vault_list(&snap, &[Path::new("/v/Snap")]);
        let mut notes = vec![];
        assert_eq!(vault(None, &env(None), &mut notes), Some("/v/Snap".into()));
        vault_list(&flatpak, &[Path::new("/v/Flat")]);
        assert_eq!(vault(None, &env(None), &mut notes), Some("/v/Flat".into()));
        vault_list(&home.join(".config/obsidian"), &[Path::new("/v/Native")]);
        assert_eq!(
            vault(None, &env(None), &mut notes),
            Some("/v/Native".into())
        );
        assert!(notes.is_empty(), "{notes:?}");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn hotkeys_json_over_the_defaults() {
        let mut notes = vec![];
        let custom = custom_hotkeys(&fixtures().join("vaults/Work"), &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        let (sheet, left_out) = parse(&pinned(), &custom);
        // Replaced.
        assert_eq!(keys(&sheet, "editor:toggle-bold"), ["ctrl+shift+b"]);
        // Emptied: gone.
        assert!(keys(&sheet, "workspace:close").is_empty());
        // A plugin's: its id is the description and its prefix the category.
        let plugin = &sheet.binds["dataview"][0];
        assert_eq!(
            (plugin.key.as_str(), plugin.desc.as_str()),
            ("ctrl+alt+r", "dataview:refresh-views")
        );
        // A core command without a default: also its id; a key with no name is counted.
        assert_eq!(keys(&sheet, "editor:toggle-strikethrough"), ["alt+Up"]);
        assert_eq!(left_out, 1);
        // The rest are the defaults.
        assert_eq!(keys(&sheet, "command-palette:open"), ["ctrl+p"]);
        // 43 defaults, less `workspace:close`, plus the plugin's and strikethrough's.
        let total: usize = sheet.binds.values().map(Vec::len).sum();
        assert_eq!(total, 44);
    }

    #[test]
    fn missing_malformed_or_gone() {
        let defaults = parse(&pinned(), &BTreeMap::new()).0;
        let mut notes = vec![];
        // No `.obsidian/hotkeys.json`: no custom hotkeys, no note.
        assert!(custom_hotkeys(&fixtures().join("vaults/Plain"), &mut notes).is_empty());
        assert!(notes.is_empty(), "{notes:?}");
        // Malformed, or the vault is gone: a note each, and the defaults.
        assert!(custom_hotkeys(&fixtures().join("vaults/Broken"), &mut notes).is_empty());
        assert!(custom_hotkeys(&fixtures().join("vaults/Gone"), &mut notes).is_empty());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("hotkeys.json") && notes[1].contains("vault "),
            "{notes:?}"
        );
        // End to end: the title picks the vault, its file is laid over; a malformed pinned file is an error.
        let dir = scratch("e2e");
        let work = fixtures().join("vaults/Work");
        vault_list(
            &dir.join("obsidian"),
            &[&work, &fixtures().join("vaults/Broken")],
        );
        let d = dir.display().to_string();
        let env = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| d.clone());
        let (sheet, notes) =
            sheet_with(PINNED, Some("Note - Work - Obsidian 1.14.5"), &env).unwrap();
        assert_eq!(keys(&sheet, "editor:toggle-bold"), ["ctrl+shift+b"]);
        assert_eq!(notes.len(), 1, "the left-out count: {notes:?}");
        let (sheet, notes) = sheet_with(PINNED, Some("Obsidian"), &env).unwrap();
        assert_eq!(sheet.binds, defaults.binds);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(sheet_with("command = 1", None, &env).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
