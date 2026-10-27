//! Vimium: its default keys for the installed version (`data/vimium/<version>.toml`, taken from Vimium's source) with
//! the user's `keyMappings` applied on top, read from a Vimium backup saved as
//! `$XDG_CONFIG_HOME/keybind-find/vimium-options.json`. Vimium keeps its settings in the browser's extension storage,
//! which we don't read; the backup (Options → Backup → "Download backup") is the same settings as plain JSON.
//! The rules follow Vimium 2.4.2's `background_scripts/commands.js` (`parseLines`, `KeyMappingsParser`).
//! ponytail: any installed Vimium counts, not necessarily the focused browser's; the Firefox `.xpi` gives no
//! version, so the newest registry is used. Vimium C and Surfingkeys are different extensions, not covered.

use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// One entry per Vimium version whose defaults we know. Supporting a new Vimium: add its file and a line here.
const REGISTRY: &[(&str, &str)] = &[(
    "2.4.2",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/vimium/2.4.2.toml"
    )),
)];

/// Vimium's Chrome Web Store id, and its Firefox add-on file.
const CHROME_ID: &str = "dbepggeogbaibhgnhhndojpepiihcmeb";
const FIREFOX_XPI: &str = "{d7742d87-e61d-4b78-b8a1-b469842139fa}.xpi";

/// Chromium-based browsers' config folders under `~/.config`; each holds one folder per profile.
const CHROMIUM_DIRS: &[&str] = &[
    "vivaldi",
    "google-chrome",
    "google-chrome-beta",
    "chromium",
    "BraveSoftware/Brave-Browser",
    "microsoft-edge",
];

#[derive(Deserialize)]
struct Registry {
    /// Default key (Vimium notation) → command, with any options after it (`reload hard`).
    keys: BTreeMap<String, String>,
    commands: HashMap<String, Command>,
}

#[derive(Deserialize)]
struct Command {
    desc: String,
    group: String,
}

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let home = env("HOME").map(PathBuf::from);
    let backup = backup_path(env("XDG_CONFIG_HOME"), home.clone());
    let (sheet, notes) = build(home.as_deref(), backup.as_deref())?;
    for note in notes {
        eprintln!("keybind-find: vimium: {note}");
    }
    Ok(sheet)
}

/// `$XDG_CONFIG_HOME/keybind-find/vimium-options.json` (absolute only, as the XDG spec says), else under
/// `$HOME/.config`.
fn backup_path(xdg_config_home: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = match xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => dir,
        None => home?.join(".config"),
    };
    Some(base.join("keybind-find").join("vimium-options.json"))
}

/// The sheet, plus a note per thing skipped. Empty, with no note, unless Vimium is installed or the backup exists.
fn build(home: Option<&Path>, backup: Option<&Path>) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut sheet = Sheet {
        title: "vimium".into(),
        provider: "vimium".into(),
        binds: BTreeMap::new(),
    };
    let mut notes = Vec::new();
    let user = match backup {
        Some(path) => key_mappings(path)?,
        None => None,
    };
    let installed = home.and_then(installed);
    let backup_exists = backup.is_some_and(Path::exists);
    if installed.is_none() && !backup_exists {
        return Ok((sheet, notes));
    }
    let (version, text, warning) =
        select(REGISTRY, installed.flatten().as_deref()).context("no Vimium registry built in")?;
    notes.extend(warning);
    let registry: Registry =
        toml::from_str(text).with_context(|| format!("built-in registry {version}"))?;
    let defaults: String = registry
        .keys
        .iter()
        .map(|(key, command)| format!("map {key} {command}\n"))
        .collect();
    let (mapped, skipped_lines) = apply(
        &(defaults + user.as_deref().unwrap_or_default()),
        &registry.commands,
    );
    if skipped_lines > 0 {
        notes.push(format!(
            "skipped {skipped_lines} keyMappings line(s) it can't apply (mapkey, unknown commands)"
        ));
    }
    let mut skipped_keys = 0;
    // Vimium's key tree (`installKeyStateMapping`): the first key to hold a command wins, so a later key equal to it
    // or starting with it never fires, and a later key that is a prefix of earlier ones makes them unreachable.
    let mut live: Vec<(Vec<String>, String)> = Vec::new();
    for (key, command) in mapped {
        let Some(notation) = translate(&key) else {
            skipped_keys += 1;
            continue;
        };
        let chords: Vec<String> = notation.split(' ').map(str::to_string).collect();
        if live.iter().any(|(k, _)| chords.starts_with(k)) {
            continue;
        }
        live.retain(|(k, _)| !k.starts_with(&chords));
        live.push((chords, command));
    }
    for (chords, command) in live {
        let notation = chords.join(" ");
        // `apply` keeps only commands the registry has.
        let Some(info) = registry.commands.get(&command) else {
            continue;
        };
        sheet
            .binds
            .entry(info.group.clone())
            .or_default()
            .push(Bind {
                key: notation,
                desc: info.desc.clone(),
                action: command,
                when: Default::default(),
            });
    }
    if skipped_keys > 0 {
        notes.push(format!(
            "skipped {skipped_keys} key(s) keybind-find can't send (<m-…>, unknown key names)"
        ));
    }
    Ok((sheet, notes))
}

/// The backup's `keyMappings` text: `None` if the file or the field is missing (Vimium leaves it out until it's
/// changed). A file that isn't JSON, or a `keyMappings` that isn't text, is an error.
fn key_mappings(path: &Path) -> anyhow::Result<Option<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let value: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a Vimium backup", path.display()))?;
    match &value["keyMappings"] {
        _ if !value.is_object() => anyhow::bail!("{} is not a Vimium backup", path.display()),
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s.clone())),
        _ => anyhow::bail!("{}: its keyMappings is not text", path.display()),
    }
}

/// `Some(version)` if Vimium is installed in a Chromium profile (the newest found), `Some(None)` if only in Firefox
/// (or its version folder doesn't parse), `None` if not found.
fn installed(home: &Path) -> Option<Option<String>> {
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    };
    let mut found = false;
    let mut versions: Vec<(Vec<u64>, String)> = Vec::new();
    for browser in CHROMIUM_DIRS {
        for profile in subdirs(&home.join(".config").join(browser)) {
            let ext = profile.join("Extensions").join(CHROME_ID);
            if !ext.is_dir() {
                continue;
            }
            found = true;
            for dir in subdirs(&ext) {
                // `2.4.2_0` → `2.4.2`.
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                let version = name.as_deref().and_then(|n| n.split('_').next());
                if let Some(v) = version {
                    if let Some(parsed) = parse_version(v) {
                        versions.push((parsed, v.to_string()));
                    }
                }
            }
        }
    }
    for profile in subdirs(&home.join(".mozilla").join("firefox")) {
        found |= profile.join("extensions").join(FIREFOX_XPI).is_file();
    }
    versions.sort();
    found.then(|| versions.pop().map(|(_, v)| v))
}

/// `2.4.2` → `[2, 4, 2]`; anything that isn't dot-separated numbers is `None`.
fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|p| p.parse().ok()).collect()
}

/// The registry entry for `installed`: an exact match silently; otherwise the newest, with a warning naming both.
fn select(
    registry: &[(&'static str, &'static str)],
    installed: Option<&str>,
) -> Option<(&'static str, &'static str, Option<String>)> {
    if let Some(&(v, text)) = registry.iter().find(|(v, _)| Some(*v) == installed) {
        return Some((v, text, None));
    }
    let &(v, text) = registry
        .iter()
        .filter_map(|e| Some((parse_version(e.0)?, e)))
        .max_by(|a, b| a.0.cmp(&b.0))?
        .1;
    let warning = match installed {
        Some(i) => format!("Vimium {i}: using key registry {v}; add data/vimium/{i}.toml"),
        None => format!("Vimium version unknown: using key registry {v}"),
    };
    Some((v, text, Some(warning)))
}

/// Applies a `keyMappings` text the way Vimium does, returning (key, command) in the order mapped (the defaults first, sorted; none shadows another) and the number of
/// lines skipped. Keys stay in Vimium's notation; `unmap` names a key exactly as it was mapped.
fn apply(text: &str, commands: &HashMap<String, Command>) -> (Vec<(String, String)>, usize) {
    let mut mapped: Vec<(String, String)> = Vec::new();
    let mut skipped = 0;
    for line in text.replace("\\\n", "").split('\n') {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', '"']) {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        match (tokens[0].to_lowercase().as_str(), &tokens[1..]) {
            // Options after the command are ignored.
            ("map", [key, command, ..]) if commands.contains_key(*command) => {
                match mapped.iter_mut().find(|(k, _)| k == key) {
                    Some(entry) => entry.1 = command.to_string(),
                    None => mapped.push((key.to_string(), command.to_string())),
                }
            }
            ("unmap", [key]) => mapped.retain(|(k, _)| k != key),
            ("unmapall", _) => mapped.clear(),
            _ => skipped += 1,
        }
    }
    (mapped, skipped)
}

/// A Vimium key (`gg`, `<c-e>`, `<a-f>`, `<c-X>`, `<f12>`) in the crate's notation (`g g`, `ctrl+e`, ...). `None`
/// for what can't be sent: meta (`<m-x>`), unknown names and modifiers, characters with no keysym name.
fn translate(key: &str) -> Option<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut chords = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match group(&chars[i..]) {
            Some((len, mods, name)) => {
                chords.push(group_chord(&mods, &name)?);
                i += len;
            }
            None => {
                chords.push(super::nvim::single(chars[i])?.to_string());
                i += 1;
            }
        }
    }
    (!chords.is_empty()).then(|| chords.join(" "))
}

/// A `<…>` group at the start, as Vimium's `specialKeyRegexp` reads one: modifiers (`x-`), then one character or a
/// name of two or more letters and digits starting with a letter, then `>`. Returns (length, modifiers, key).
fn group(chars: &[char]) -> Option<(usize, Vec<char>, String)> {
    if chars.first() != Some(&'<') {
        return None;
    }
    let mut i = 1;
    let mut mods = Vec::new();
    while chars.get(i).is_some_and(char::is_ascii_alphabetic) && chars.get(i + 1) == Some(&'-') {
        mods.push(chars[i].to_ascii_lowercase());
        i += 2;
    }
    // A name: a letter, then letters and digits, up to `>`.
    let end = chars[i..].iter().position(|&c| c == '>').map(|p| i + p);
    if let Some(end) = end {
        let name: String = chars[i..end].iter().collect();
        let mut it = name.chars();
        let is_name = name.len() >= 2
            && it.next().is_some_and(|c| c.is_ascii_alphabetic())
            && it.all(|c| c.is_ascii_alphanumeric());
        if is_name {
            return Some((end + 1, mods, name.to_lowercase()));
        }
    }
    // One character, only after modifiers (`<c->>` is ctrl and `>`).
    if !mods.is_empty() && chars.get(i + 1) == Some(&'>') {
        return Some((i + 2, mods, chars[i].to_string()));
    }
    None
}

fn group_chord(mods: &[char], name: &str) -> Option<String> {
    let mut names = Vec::new();
    for m in mods {
        names.push(match m {
            'a' => "alt",
            'c' => "ctrl",
            's' => "shift",
            _ => return None,
        });
    }
    let keysym = match name {
        "space" => "space".to_string(),
        "tab" => "Tab".to_string(),
        // Not `cr` or `esc`: Vimium names keys by `event.key` (`enter`, `escape`), so those never fire.
        "enter" => "Return".to_string(),
        "backspace" => "BackSpace".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        // Vimium names other keys by their `event.key`, lower-cased.
        "escape" => "Escape".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" => "Page_Up".to_string(),
        "pagedown" => "Page_Down".to_string(),
        "delete" => "Delete".to_string(),
        "insert" => "Insert".to_string(),
        f if f.starts_with('f')
            && !f[1..].starts_with('0')
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) =>
        {
            f.to_uppercase()
        }
        _ => {
            let mut it = name.chars();
            let c = it.next()?;
            if it.next().is_some() {
                return None;
            }
            // Vimium shows shift on a character by its case and never as `s-`, so `<c-s-x>` never fires.
            if names.contains(&"shift") {
                return None;
            }
            // `<c-A>` is ctrl+shift+a, as the crate writes a shifted letter with another modifier.
            if c.is_ascii_uppercase() && !names.is_empty() {
                names.push("shift");
            }
            super::nvim::single(c.to_ascii_lowercase())?.to_string()
        }
    };
    Some(chord(&names, &keysym))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/vimium")
            .join(name)
    }

    /// A fresh temp folder (a leftover from a crashed run with the same pid is cleared first).
    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kbf-vimium-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A HOME with Vimium 2.4.2 in Vivaldi's Default profile.
    fn vivaldi_home(name: &str) -> PathBuf {
        let home = temp(name);
        std::fs::create_dir_all(
            home.join(".config/vivaldi/Default/Extensions")
                .join(CHROME_ID)
                .join("2.4.2_0"),
        )
        .unwrap();
        home
    }

    fn find<'a>(sheet: &'a Sheet, key: &str) -> Vec<(&'a str, &'a Bind)> {
        sheet
            .binds
            .iter()
            .flat_map(|(c, binds)| binds.iter().map(move |b| (c.as_str(), b)))
            .filter(|(_, b)| b.key == key)
            .collect()
    }

    fn registry() -> Registry {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    fn mappings(text: &str) -> (Vec<(String, String)>, usize) {
        apply(text, &registry().commands)
    }

    #[test]
    fn the_registry_parses_and_every_key_translates() {
        let r = registry();
        assert!(r.keys.len() > 60, "{}", r.keys.len());
        for (key, command) in &r.keys {
            assert!(translate(key).is_some(), "{key}");
            let name = command.split_whitespace().next().unwrap();
            assert!(r.commands.contains_key(name), "{key} {command}");
        }
    }

    #[test]
    fn defaults() {
        let home = vivaldi_home("defaults");
        let (sheet, notes) = build(Some(&home), None).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(sheet.provider, "vimium");
        assert_eq!(sheet.title, "vimium");
        let gg = find(&sheet, "g g");
        assert_eq!(gg.len(), 1);
        assert_eq!(gg[0].0, "navigation");
        assert_eq!(gg[0].1.desc, "Scroll to the top of the page");
        assert_eq!(gg[0].1.action, "scrollToTop");
        assert_eq!(find(&sheet, "ctrl+e")[0].1.action, "scrollDown");
        // `reload hard`: the command is the first word.
        assert_eq!(find(&sheet, "R")[0].1.action, "reload");
        assert!(sheet.binds.contains_key("tabs") && sheet.binds.contains_key("find"));
        // A backup without keyMappings gives the defaults.
        let (same, notes) = build(Some(&home), Some(&fixture("no-mappings.json"))).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(same, sheet);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn the_backup_applies_over_the_defaults() {
        let home = vivaldi_home("backup");
        let (sheet, notes) = build(Some(&home), Some(&fixture("mappings.json"))).unwrap();
        // unmap x, map X removeTab (over restoreTab), a continued line, a comment.
        assert!(find(&sheet, "x").is_empty());
        let x = find(&sheet, "X");
        assert_eq!(x.len(), 1);
        assert_eq!(x[0].1.desc, "Close current tab");
        assert_eq!(find(&sheet, "ctrl+d")[0].1.action, "scrollPageDown");
        // mapkey and an unknown command: one note, counted.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("skipped 2 keyMappings line"),
            "{}",
            notes[0]
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn mapping_rules() {
        let (m, skipped) =
            mappings("map j scrollUp\nmap gg scrollToBottom\nunmapAll\nmap q scrollDown\n");
        assert_eq!(m, [("q".to_string(), "scrollDown".to_string())]);
        assert_eq!(skipped, 0);
        // Comments, blank lines, `\` continuation, case of the action, options ignored.
        let (m, skipped) = mappings(
            "  # a comment\n\" another\n\n  MAP a \\\nscrollUp\nmap z0 zoomReset count=3\nunmap\nmap b\n",
        );
        assert_eq!(
            m,
            [
                ("a".to_string(), "scrollUp".to_string()),
                ("z0".to_string(), "zoomReset".to_string())
            ]
        );
        // `unmap` without a key and `map` without a command.
        assert_eq!(skipped, 2);
        let (_, skipped) = mappings("mapkey a b\nmap q noSuchCommand\nfrobnicate x\n");
        assert_eq!(skipped, 3);
        // A re-map keeps the key's place; unmap then map moves it to the end.
        let (m, _) =
            mappings("map a scrollUp\nmap b scrollDown\nmap a reload\nunmap b\nmap b reload\n");
        assert_eq!(
            m.iter()
                .map(|(k, c)| format!("{k}={c}"))
                .collect::<Vec<_>>(),
            ["a=reload", "b=reload"]
        );
    }

    #[test]
    fn shadowed_keys_are_left_out() {
        let home = vivaldi_home("shadow");
        // No default shadows another: all of them are listed.
        let (sheet, _) = build(Some(&home), None).unwrap();
        assert_eq!(
            sheet.binds.values().flatten().count(),
            registry().keys.len()
        );
        let dir = temp("shadow-backup");
        let path = dir.join("vimium-options.json");
        let run = |mappings: &str| {
            std::fs::write(
                &path,
                serde_json::json!({ "keyMappings": mappings }).to_string(),
            )
            .unwrap();
            build(Some(&home), Some(&path)).unwrap().0
        };
        // The same key spelled differently: the first (the default) wins.
        let sheet = run("map <C-e> scrollUp");
        let e = find(&sheet, "ctrl+e");
        assert!(e.len() == 1 && e[0].1.action == "scrollDown", "{e:?}");
        // A longer key behind a mapped one never fires.
        let sheet = run("map dd removeTab");
        assert!(find(&sheet, "d d").is_empty());
        assert_eq!(find(&sheet, "d")[0].1.action, "scrollPageDown");
        // A shorter key makes the longer ones under it unreachable.
        let sheet = run("map y copyCurrentUrl");
        assert_eq!(find(&sheet, "y")[0].1.action, "copyCurrentUrl");
        for gone in ["y y", "y f", "y t"] {
            assert!(find(&sheet, gone).is_empty(), "{gone}");
        }
        // A spelling Vimium never fires can't hide the one that does.
        let sheet = run(
            "map <c-s-a> reload\nmap <c-A> removeTab\nmap <esc> reload\nmap <escape> createTab",
        );
        assert_eq!(find(&sheet, "ctrl+shift+a")[0].1.action, "removeTab");
        assert_eq!(find(&sheet, "Escape")[0].1.action, "createTab");
        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_backup_fails_the_provider() {
        let home = vivaldi_home("broken");
        let err = build(Some(&home), Some(&fixture("broken.json"))).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a Vimium backup"),
            "{err:#}"
        );
        let dir = temp("not-text");
        let path = dir.join("vimium-options.json");
        std::fs::write(&path, r#"{"keyMappings": 3}"#).unwrap();
        let err = build(Some(&home), Some(&path)).unwrap_err();
        assert!(
            format!("{err:#}").contains("keyMappings is not text"),
            "{err:#}"
        );
        // JSON, but not a backup.
        std::fs::write(&path, "[]").unwrap();
        let err = build(Some(&home), Some(&path)).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a Vimium backup"),
            "{err:#}"
        );
        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn translation() {
        for (vimium, ours) in [
            ("gg", "g g"),
            ("<c-e>", "ctrl+e"),
            ("<a-f>", "alt+f"),
            ("<c-X>", "ctrl+shift+x"),
            ("<s-space>", "shift+space"),
            ("<c-s-left>", "ctrl+shift+Left"),
            ("<c-A>", "ctrl+shift+a"),
            ("#", "numbersign"),
            ("`", "grave"),
            ("g$", "g dollar"),
            ("<<", "less less"),
            (">>", "greater greater"),
            ("[[", "bracketleft bracketleft"),
            ("<space>", "space"),
            ("<tab>", "Tab"),
            ("<enter>", "Return"),
            ("<backspace>", "BackSpace"),
            ("<left>", "Left"),
            ("<right>", "Right"),
            ("<up>", "Up"),
            ("<down>", "Down"),
            ("<f1>", "F1"),
            ("<f12>", "F12"),
            ("<f13>", "F13"),
            ("<escape>", "Escape"),
            ("<Home>", "Home"),
            ("<end>", "End"),
            ("<pageup>", "Page_Up"),
            ("<pagedown>", "Page_Down"),
            ("<delete>", "Delete"),
            ("<insert>", "Insert"),
            ("<c-left>", "ctrl+Left"),
            ("<c->>", "ctrl+greater"),
            ("g<space>x", "g space x"),
        ] {
            assert_eq!(translate(vimium).as_deref(), Some(ours), "{vimium}");
        }
        for vimium in [
            "<m-x>", "<esc>", "<cr>", "<c-s-x>", "<s-a>", "<f25>", "<f01>", "<foo>", "<x-a>", "é",
            "",
        ] {
            assert_eq!(translate(vimium), None, "{vimium}");
        }
    }

    #[test]
    fn untranslatable_keys_are_counted() {
        let home = vivaldi_home("meta");
        let dir = temp("meta-backup");
        let path = dir.join("vimium-options.json");
        std::fs::write(
            &path,
            r#"{"keyMappings": "map <m-x> removeTab\nmap <f25> reload"}"#,
        )
        .unwrap();
        let (_, notes) = build(Some(&home), Some(&path)).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("skipped 2 key(s)"), "{}", notes[0]);
        std::fs::remove_dir_all(&home).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn installed_check() {
        let home = vivaldi_home("installed");
        assert_eq!(installed(&home), Some(Some("2.4.2".to_string())));
        // Several profiles and browsers: the newest version wins, and one the registry lacks gets a note naming both.
        let newer = home
            .join(".config/chromium/Profile 1/Extensions")
            .join(CHROME_ID);
        std::fs::create_dir_all(newer.join("2.10.0_0")).unwrap();
        std::fs::create_dir_all(newer.join("odd-name")).unwrap();
        // A version folder that doesn't parse alone: installed, version unknown.
        let odd = temp("odd");
        let ext = odd
            .join(".config/vivaldi/Default/Extensions")
            .join(CHROME_ID);
        std::fs::create_dir_all(ext.join("odd-name")).unwrap();
        assert_eq!(installed(&odd), Some(None));
        std::fs::remove_dir_all(&odd).unwrap();
        assert_eq!(installed(&home), Some(Some("2.10.0".to_string())));
        let (sheet, notes) = build(Some(&home), None).unwrap();
        assert!(!sheet.binds.is_empty());
        assert!(
            notes.len() == 1 && notes[0].contains("Vimium 2.10.0") && notes[0].contains("2.4.2"),
            "{notes:?}"
        );
        // Firefox only: installed, no version, so the newest registry with a note.
        let ff = temp("firefox");
        let ext = ff.join(".mozilla/firefox/abc.default-release/extensions");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join(FIREFOX_XPI), "").unwrap();
        assert_eq!(installed(&ff), Some(None));
        let (sheet, notes) = build(Some(&ff), None).unwrap();
        assert!(!sheet.binds.is_empty());
        assert!(
            notes.len() == 1 && notes[0].contains("version unknown"),
            "{notes:?}"
        );
        // Neither, and no backup: an empty sheet, no note. A missing backup path counts as no backup.
        let none = temp("none");
        assert_eq!(installed(&none), None);
        let missing = none.join("vimium-options.json");
        for backup in [None, Some(missing.as_path())] {
            let (sheet, notes) = build(Some(&none), backup).unwrap();
            assert!(sheet.binds.is_empty() && notes.is_empty(), "{notes:?}");
        }
        assert!(build(None, None).unwrap().0.binds.is_empty());
        // Only the backup: it applies, with the newest registry and a note.
        let (sheet, notes) = build(Some(&none), Some(&fixture("mappings.json"))).unwrap();
        assert_eq!(find(&sheet, "X")[0].1.action, "removeTab");
        assert!(
            notes.iter().any(|n| n.contains("version unknown")),
            "{notes:?}"
        );
        for d in [home, ff, none] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn version_selection() {
        let reg: &[(&str, &str)] = &[("2.4.2", "a"), ("2.10.0", "b"), ("2.3.0", "c")];
        assert_eq!(select(reg, Some("2.4.2")), Some(("2.4.2", "a", None)));
        let (v, _, w) = select(reg, Some("2.5.0")).unwrap();
        assert_eq!(v, "2.10.0");
        assert!(w.unwrap().contains("Vimium 2.5.0"));
        let (v, _, w) = select(reg, None).unwrap();
        assert_eq!(v, "2.10.0");
        assert!(w.unwrap().contains("unknown"));
        assert_eq!(select(&[], None), None);
    }

    #[test]
    fn backup_path_follows_xdg() {
        let home = Some(PathBuf::from("/h"));
        assert_eq!(
            backup_path(Some("/xdg".into()), home.clone()),
            Some(PathBuf::from("/xdg/keybind-find/vimium-options.json"))
        );
        assert_eq!(
            backup_path(Some("rel".into()), home.clone()),
            Some(PathBuf::from("/h/.config/keybind-find/vimium-options.json"))
        );
        assert_eq!(backup_path(None, None), None);
    }
}
