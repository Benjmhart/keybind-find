//! KDE Plasma: the global shortcuts kglobalacceld keeps in `kglobalshortcutsrc`, and the launch shortcuts of the
//! `.desktop` files it takes defaults from. The files are read, not kglobalaccel's D-Bus (no D-Bus crate).

use super::nvim::single;
use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(&env)?;
    for note in notes {
        eprintln!("keybind-find: kde: {note}");
    }
    Ok(sheet)
}

/// The sheet and the notes to print. A missing `kglobalshortcutsrc` is no error (nothing was ever changed); an
/// unreadable one is.
fn sheet_with(env: Env) -> anyhow::Result<(Sheet, Vec<String>)> {
    let rc = config_dir(env)
        .context("neither XDG_CONFIG_HOME nor HOME is set")?
        .join("kglobalshortcutsrc");
    let text = match std::fs::read(&rc) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", rc.display())),
    };
    let rc_text = text.as_deref().unwrap_or_default();
    // Files named in `[services]`, read for their `Name` even without a default shortcut.
    let wanted = ini(rc_text)
        .into_iter()
        .filter_map(|(path, _)| match path.as_slice() {
            [services, file] if services == "services" => Some(file.clone()),
            _ => None,
        })
        .collect();
    let (sheet, left_out) = parse(rc_text, &desktops(&data_dirs(env), &wanted));
    let mut notes = Vec::new();
    if text.is_none() && sheet.binds.is_empty() {
        notes.push(format!("no {} and no launch shortcuts", rc.display()));
    }
    if !left_out.is_empty() {
        let kinds: Vec<String> = left_out.iter().map(|(k, n)| format!("{k}: {n}")).collect();
        notes.push(format!(
            "leaving out {} keys keybind-find can't send ({})",
            left_out.values().sum::<usize>(),
            kinds.join(", ")
        ));
    }
    Ok((sheet, notes))
}

/// An absolute path from `k`; empty or relative counts as unset.
fn absolute(env: Env, k: &str) -> Option<PathBuf> {
    env(k).map(PathBuf::from).filter(|p| p.is_absolute())
}

fn config_dir(env: Env) -> Option<PathBuf> {
    absolute(env, "XDG_CONFIG_HOME").or_else(|| absolute(env, "HOME").map(|h| h.join(".config")))
}

/// `$XDG_DATA_HOME` (else `~/.local/share`), then each `$XDG_DATA_DIRS` entry (else `/usr/local/share:/usr/share`).
fn data_dirs(env: Env) -> Vec<PathBuf> {
    let home = absolute(env, "XDG_DATA_HOME")
        .or_else(|| absolute(env, "HOME").map(|h| h.join(".local/share")));
    let dirs = env("XDG_DATA_DIRS")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(
            dirs.split(':')
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
        )
        .collect()
}

/// A `.desktop` file's global shortcut defaults.
#[derive(Debug, PartialEq)]
struct Desktop {
    /// The file name, the `[services]` group's name: `org.kde.konsole.desktop`.
    file: String,
    /// Its `Name` (the file name without `.desktop` when it has none).
    name: String,
    /// `_launch` or an action name, its description, and its keys (comma-separated, as written).
    shortcuts: Vec<(String, String, String)>,
}

/// The `.desktop` files kglobalacceld reads, by file name: `kglobalaccel/` in every data dir, then `applications/`,
/// the first file of each name winning even when it has no shortcut. Only files mentioning `X-KDE-Shortcuts`, or
/// `wanted`, are parsed.
/// ponytail: `applications/` subdirectories (`kde4/`, whose ids get a prefix) are not walked.
fn desktops(dirs: &[PathBuf], wanted: &BTreeSet<String>) -> Vec<Desktop> {
    let mut seen: BTreeMap<String, Option<Desktop>> = BTreeMap::new();
    for sub in ["kglobalaccel", "applications"] {
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
                continue;
            };
            let mut files: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "desktop") && p.is_file())
                .collect();
            files.sort();
            for path in files {
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };
                if seen.contains_key(&name) {
                    continue;
                }
                let text = std::fs::read(&path)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let desktop = (text.contains("X-KDE-Shortcuts") || wanted.contains(&name))
                    .then(|| desktop(&name, &text, sub == "kglobalaccel"))
                    .flatten();
                seen.insert(name, desktop);
            }
        }
    }
    seen.into_values().flatten().collect()
}

/// One `.desktop` file: `_launch` from `[Desktop Entry]` (unless `X-KDE-GlobalShortcutType=Service`), and each
/// action listed in `Actions=` (its `[Desktop Action <a>]`), when it has `X-KDE-Shortcuts`. `None` for a `Hidden`
/// (deleted) file, and for a `NoDisplay` one when `hidable`.
fn desktop(file: &str, text: &str, hidable: bool) -> Option<Desktop> {
    let groups = ini(text);
    let get = |group: &str, key: &str| {
        groups
            .iter()
            .find(|(path, _)| path.len() == 1 && path[0] == group)
            .and_then(|(_, entries)| entries.iter().find(|(k, _)| k == key))
            .map(|(_, v)| v.as_str())
    };
    let entry = "Desktop Entry";
    if get(entry, "Hidden") == Some("true") || hidable && get(entry, "NoDisplay") == Some("true") {
        return None;
    }
    let name = get(entry, "Name").unwrap_or(file.trim_end_matches(".desktop"));
    let mut shortcuts = Vec::new();
    let service = get(entry, "X-KDE-GlobalShortcutType") == Some("Service");
    if let Some(keys) = get(entry, "X-KDE-Shortcuts").filter(|_| !service) {
        shortcuts.push(("_launch".into(), format!("launch {name}"), keys.into()));
    }
    let actions = get(entry, "Actions").unwrap_or_default();
    for action in actions.split(';').filter(|a| !a.is_empty()) {
        let group = format!("Desktop Action {action}");
        if let Some(keys) = get(&group, "X-KDE-Shortcuts") {
            let desc = get(&group, "Name").unwrap_or(action);
            shortcuts.push((action.into(), desc.into(), keys.into()));
        }
    }
    Some(Desktop {
        file: file.into(),
        name: name.into(),
        shortcuts,
    })
}

/// The sheet from `kglobalshortcutsrc`'s text and the `.desktop` defaults, and how many keys of each kind were left
/// out. Each top-level group is a component (Plasma 5's `[<app>.desktop]` groups too); nested groups (other contexts,
/// `$InverseAction`) and `$`-groups are skipped. `[services][<file>]` replaces that file's defaults, and so does a
/// Plasma 5 top-level `[<file>]` group (listed as a component).
fn parse(rc: &str, desktops: &[Desktop]) -> (Sheet, BTreeMap<&'static str, usize>) {
    let mut sheet = Sheet {
        title: "kde".into(),
        provider: "kde".into(),
        binds: Default::default(),
    };
    let mut left_out = BTreeMap::new();
    let mut add = |category: &str, desc: &str, action: &str, keys: &str| {
        for key in keys.split('\t').map(str::trim) {
            if key.is_empty() || key == "none" {
                continue;
            }
            match notation(key) {
                Ok(key) => sheet.binds.entry(category.into()).or_default().push(Bind {
                    key,
                    desc: desc.into(),
                    action: action.into(),
                    when: Default::default(),
                }),
                Err(kind) => *left_out.entry(kind).or_default() += 1,
            }
        }
    };
    let mut overrides: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut components = BTreeSet::new();
    let groups = ini(rc);
    for (path, entries) in &groups {
        match path.as_slice() {
            [services, file] if services == "services" => {
                for (action, keys) in entries {
                    overrides.insert((file.clone(), action.clone()), keys.clone());
                }
            }
            [component] if component != "services" && !component.starts_with('$') => {
                components.insert(component.as_str());
                let category = entries
                    .iter()
                    .find(|(k, _)| k == "_k_friendly_name")
                    .map_or(component.as_str(), |(_, v)| v.as_str());
                for (action, value) in entries.iter().filter(|(k, _)| k != "_k_friendly_name") {
                    // `current,default,friendly name`.
                    let fields = list(value);
                    let desc = fields.get(2).filter(|d| !d.is_empty()).unwrap_or(action);
                    add(
                        category,
                        desc,
                        action,
                        fields.first().map_or("", String::as_str),
                    );
                }
            }
            _ => {}
        }
    }
    for d in desktops
        .iter()
        .filter(|d| !components.contains(d.file.as_str()))
    {
        for (action, desc, keys) in &d.shortcuts {
            match overrides.remove(&(d.file.clone(), action.clone())) {
                Some(keys) => add("launch", desc, action, &keys),
                None => add("launch", desc, action, &keys.replace(',', "\t")),
            }
        }
    }
    // An override with no default (an app given a shortcut in System Settings) still is a shortcut.
    for ((file, action), keys) in overrides {
        let desc = match action.as_str() {
            "_launch" => {
                let found = desktops
                    .iter()
                    .find(|d| d.file == file)
                    .map(|d| d.name.as_str());
                format!(
                    "launch {}",
                    found.unwrap_or(file.trim_end_matches(".desktop"))
                )
            }
            a => a.to_string(),
        };
        add("launch", &desc, &action, &keys);
    }
    (sheet, left_out)
}

/// A group's path and its entries.
type Group = (Vec<String>, Vec<(String, String)>);

/// KConfig's INI: each group (`[a][b]` is the path `a`, `b`; a last `[$i]`, the immutable marker, dropped) with its
/// entries in file order, a repeated group merged and a repeated key's last value kept. Names and values have the file
/// escapes undone, keys their `[$e]`-style options cut off. Lines before the first group and comments are skipped.
fn ini(text: &str) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut current: Option<usize> = None;
    for line in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let mut path = Vec::new();
            let mut rest = line;
            while let Some((name, after)) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
                path.push(unescape(name));
                rest = after;
            }
            if path.len() > 1 && path.last().is_some_and(|p| p == "$i") {
                path.pop();
            }
            current = Some(match groups.iter().position(|(p, _)| *p == path) {
                Some(i) => i,
                None => {
                    groups.push((path, Vec::new()));
                    groups.len() - 1
                }
            });
        } else if let (Some(i), Some((k, v))) = (current, line.split_once('=')) {
            let k = unescape(k.split_once("[$").map_or(k, |(k, _)| k).trim());
            let entries = &mut groups[i].1;
            entries.retain(|(old, _)| *old != k);
            entries.push((k, unescape(v.trim())));
        }
    }
    groups
}

/// Undoes KConfig's file escapes (`\s \t \n \r \\ \xHH`); `\,` and `\;` stay, for `list`.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hex: String = chars.clone().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    // KConfig writes `\xHH` only for control bytes, so a byte is a char.
                    Ok(b) if hex.len() == 2 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                        out.push(char::from(b));
                        chars.nth(1);
                    }
                    _ => out.push_str("\\x"),
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A KConfig list: split on unescaped commas, with `\,` → `,` and `\\` → `\`.
fn list(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut item = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => item.extend(chars.next()),
            ',' => items.push(std::mem::take(&mut item)),
            c => item.push(c),
        }
    }
    items.push(item);
    items
}

/// A Qt PortableText key sequence (`Meta+K, Meta+L`) in the notation, or the kind of key it can't give. Names are
/// matched ignoring case, as `QKeySequence::fromString` does for a hand-written `X-KDE-Shortcuts`.
fn notation(seq: &str) -> Result<String, &'static str> {
    let chords = seq
        .split(", ")
        .map(chord_of)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(chords.join(" "))
}

fn chord_of(c: &str) -> Result<String, &'static str> {
    let (mods, key) = match c.strip_suffix("++") {
        Some(mods) => (mods, "+"),
        None if c == "+" => ("", "+"),
        None => c.rsplit_once('+').unwrap_or(("", c)),
    };
    let mut out = Vec::new();
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        out.push(match m.to_ascii_lowercase().as_str() {
            "meta" => "super",
            "ctrl" => "ctrl",
            "alt" => "alt",
            "shift" => "shift",
            "num" => return Err("keypad"),
            _ => return Err("unknown Qt name"),
        });
    }
    let mut chars = key.chars();
    let sym = match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_ascii() => return Err("non-ASCII"),
        (Some(c), None) if c.is_ascii_alphabetic() => c.to_ascii_lowercase().to_string(),
        (Some(c), None) => single(c).ok_or("unknown Qt name")?.to_string(),
        _ if key
            .strip_prefix(['F', 'f'])
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=35).contains(&n)) =>
        {
            key.to_ascii_uppercase()
        }
        _ if key.eq_ignore_ascii_case("Backtab") => {
            out.push("shift");
            "Tab".to_string()
        }
        _ => QT_KEYS
            .iter()
            .find(|(qt, _)| qt.eq_ignore_ascii_case(key))
            .map(|(_, x)| x.to_string())
            .ok_or("unknown Qt name")?,
    };
    Ok(chord(&out, &sym))
}

/// Qt's PortableText key names (qtbase `qkeysequence.cpp` `keyname[]`) as X keysyms. A name with no clear keysym is
/// left out (and counted).
const QT_KEYS: &[(&str, &str)] = &[
    ("Space", "space"),
    ("Esc", "Escape"),
    ("Tab", "Tab"),
    ("Backspace", "BackSpace"),
    ("Return", "Return"),
    ("Enter", "KP_Enter"),
    ("Ins", "Insert"),
    ("Del", "Delete"),
    ("Pause", "Pause"),
    ("Print", "Print"),
    ("SysReq", "Sys_Req"),
    ("Home", "Home"),
    ("End", "End"),
    ("Left", "Left"),
    ("Up", "Up"),
    ("Right", "Right"),
    ("Down", "Down"),
    ("PgUp", "Page_Up"),
    ("PgDown", "Page_Down"),
    ("CapsLock", "Caps_Lock"),
    ("NumLock", "Num_Lock"),
    ("ScrollLock", "Scroll_Lock"),
    ("Menu", "Menu"),
    ("Help", "Help"),
    ("Back", "XF86Back"),
    ("Forward", "XF86Forward"),
    ("Stop", "XF86Stop"),
    ("Refresh", "XF86Refresh"),
    ("Volume Down", "XF86AudioLowerVolume"),
    ("Volume Mute", "XF86AudioMute"),
    ("Volume Up", "XF86AudioRaiseVolume"),
    ("Media Play", "XF86AudioPlay"),
    ("Media Stop", "XF86AudioStop"),
    ("Media Previous", "XF86AudioPrev"),
    ("Media Next", "XF86AudioNext"),
    ("Media Record", "XF86AudioRecord"),
    ("Media Pause", "XF86AudioPause"),
    ("Home Page", "XF86HomePage"),
    ("Favorites", "XF86Favorites"),
    ("Search", "XF86Search"),
    ("Standby", "XF86Standby"),
    ("Open URL", "XF86OpenURL"),
    ("Launch Mail", "XF86Mail"),
    ("Launch Media", "XF86AudioMedia"),
    ("Launch (0)", "XF86Launch0"),
    ("Launch (1)", "XF86Launch1"),
    ("Launch (2)", "XF86Launch2"),
    ("Launch (3)", "XF86Launch3"),
    ("Launch (4)", "XF86Launch4"),
    ("Launch (5)", "XF86Launch5"),
    ("Launch (6)", "XF86Launch6"),
    ("Launch (7)", "XF86Launch7"),
    ("Launch (8)", "XF86Launch8"),
    ("Launch (9)", "XF86Launch9"),
    ("Launch (A)", "XF86LaunchA"),
    ("Launch (B)", "XF86LaunchB"),
    ("Launch (C)", "XF86LaunchC"),
    ("Launch (D)", "XF86LaunchD"),
    ("Launch (E)", "XF86LaunchE"),
    ("Launch (F)", "XF86LaunchF"),
    ("Monitor Brightness Up", "XF86MonBrightnessUp"),
    ("Monitor Brightness Down", "XF86MonBrightnessDown"),
    ("Keyboard Light On/Off", "XF86KbdLightOnOff"),
    ("Keyboard Brightness Up", "XF86KbdBrightnessUp"),
    ("Keyboard Brightness Down", "XF86KbdBrightnessDown"),
    ("Power Off", "XF86PowerOff"),
    ("Wake Up", "XF86WakeUp"),
    ("Eject", "XF86Eject"),
    ("Screensaver", "XF86ScreenSaver"),
    ("WWW", "XF86WWW"),
    ("Sleep", "XF86Sleep"),
    ("Media Rewind", "XF86AudioRewind"),
    ("Calculator", "XF86Calculator"),
    ("Calendar", "XF86Calendar"),
    ("Close", "XF86Close"),
    ("Copy", "XF86Copy"),
    ("Cut", "XF86Cut"),
    ("Display", "XF86Display"),
    ("Documents", "XF86Documents"),
    ("Browser", "XF86Explorer"),
    ("Game", "XF86Game"),
    ("Go", "XF86Go"),
    ("Logoff", "XF86LogOff"),
    ("Paste", "XF86Paste"),
    ("Phone", "XF86Phone"),
    ("Reply", "XF86Reply"),
    ("Reload", "XF86Reload"),
    ("Rotate Windows", "XF86RotateWindows"),
    ("Save", "XF86Save"),
    ("Send", "XF86Send"),
    ("Split Screen", "XF86SplitScreen"),
    ("Terminal", "XF86Terminal"),
    ("Tools", "XF86Tools"),
    ("Video", "XF86Video"),
    ("Zoom In", "XF86ZoomIn"),
    ("Zoom Out", "XF86ZoomOut"),
    ("Messenger", "XF86Messenger"),
    ("WebCam", "XF86WebCam"),
    ("Pictures", "XF86Pictures"),
    ("Music", "XF86Music"),
    ("Battery", "XF86Battery"),
    ("Bluetooth", "XF86Bluetooth"),
    ("Wireless", "XF86WLAN"),
    ("Media Fast Forward", "XF86AudioForward"),
    ("Audio Repeat", "XF86AudioRepeat"),
    ("Audio Random Play", "XF86AudioRandomPlay"),
    ("Time", "XF86Time"),
    ("Hibernate", "XF86Hibernate"),
    ("Suspend", "XF86Suspend"),
    ("Microphone Mute", "XF86AudioMicMute"),
    ("Undo", "Undo"),
    ("Redo", "Redo"),
    ("Find", "Find"),
    ("Select", "Select"),
    ("Clear", "Clear"),
    ("Kanji", "Kanji"),
    ("Muhenkan", "Muhenkan"),
    ("Henkan", "Henkan"),
    ("Romaji", "Romaji"),
    ("Hiragana", "Hiragana"),
    ("Katakana", "Katakana"),
    ("Hiragana Katakana", "Hiragana_Katakana"),
    ("Zenkaku", "Zenkaku"),
    ("Hankaku", "Hankaku"),
    ("Zenkaku Hankaku", "Zenkaku_Hankaku"),
    ("Kana Lock", "Kana_Lock"),
    ("Kana Shift", "Kana_Shift"),
    ("Eisu Shift", "Eisu_Shift"),
    ("Eisu toggle", "Eisu_toggle"),
    ("Hangul", "Hangul"),
    ("Cancel", "Cancel"),
    ("Execute", "Execute"),
    ("Power Down", "XF86PowerDown"),
    ("Touchpad Toggle", "XF86TouchpadToggle"),
    ("Touchpad On", "XF86TouchpadOn"),
    ("Touchpad Off", "XF86TouchpadOff"),
    // A modifier pressed alone (Plasma's launcher is `Meta`).
    ("Meta", "Super_L"),
    ("Shift", "Shift_L"),
    ("Control", "Control_L"),
    ("Alt", "Alt_L"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/kde/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn share(dirs: &[&str]) -> Vec<PathBuf> {
        dirs.iter()
            .map(|d| {
                PathBuf::from(format!(
                    "{}/tests/fixtures/kde/{d}",
                    env!("CARGO_MANIFEST_DIR")
                ))
            })
            .collect()
    }

    /// Each category's rows as `key = desc`.
    fn rows(sheet: &Sheet) -> BTreeMap<String, Vec<String>> {
        sheet
            .binds
            .iter()
            .map(|(c, binds)| {
                (
                    c.clone(),
                    binds
                        .iter()
                        .map(|b| format!("{} = {}", b.key, b.desc))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn plasma6_components_entries_and_escapes() {
        let (sheet, left_out) = parse(&fixture("kglobalshortcutsrc-plasma6"), &[]);
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str()),
            ("kde", "kde")
        );
        let rows = rows(&sheet);
        assert_eq!(
            rows["KWin"],
            [
                "super+comma = Comma Key",
                "super+shift+c = Zoom, then pan",
                "super+e = Toggle Present Windows (Current desktop)",
                "XF86Launch1 = Toggle Present Windows (Current desktop)",
                "super+k super+l = Two Chords",
                "super+ctrl+Left = Switch to Window to the Left",
                "XF86AudioRaiseVolume = Increase Volume",
                "alt+shift+Tab = Walk Through Windows (Reverse)",
                "alt+F4 = Close Window",
            ]
        );
        assert_eq!(
            rows["Session Management"],
            ["super+l = Lock Session", "XF86ScreenSaver = Lock Session"]
        );
        // `[services]` with no `.desktop` file found: still listed, by file name; `none` lists nothing.
        assert_eq!(
            rows["launch"],
            [
                "super+f = launch org.example.named",
                "super+shift+n = new",
                "ctrl+alt+k = launch org.kde.konsole"
            ]
        );
        // No `_k_friendly_name`: the group name is the category.
        assert_eq!(rows["org.kde.kmix"], ["XF86AudioMute = Mute"]);
        // Nested groups (`$InverseAction`, another context) and `[$Version]` give nothing.
        assert_eq!(rows.len(), 4);
        let action = |key: &str| {
            sheet
                .binds
                .values()
                .flatten()
                .find(|b| b.key == key)
                .unwrap()
                .action
                .clone()
        };
        assert_eq!(action("alt+F4"), "Window Close");
        assert_eq!(
            left_out,
            BTreeMap::from([("keypad", 1), ("non-ASCII", 1), ("unknown Qt name", 1)])
        );
    }

    #[test]
    fn plasma5_desktop_groups_are_components() {
        let (sheet, left_out) = parse(&fixture("kglobalshortcutsrc-plasma5"), &[]);
        assert_eq!(
            rows(&sheet)["Spectacle"],
            [
                "super+shift+Print = Capture Rectangular Region",
                "Print = Spectacle"
            ]
        );
        assert!(left_out.is_empty());
        // Plasma 5 keeps a `.desktop` component's state in its own group: its file's defaults aren't listed again.
        let found = desktops(&share(&["share"]), &BTreeSet::new());
        assert!(found.iter().any(|d| d.file == "org.kde.spectacle.desktop"));
        let (sheet, _) = parse(&fixture("kglobalshortcutsrc-plasma5"), &found);
        let rows = rows(&sheet);
        assert!(
            !rows["launch"].iter().any(|r| r.contains("Spectacle")),
            "{rows:?}"
        );
        assert_eq!(rows["Spectacle"].len(), 2);
    }

    #[test]
    fn desktop_defaults_and_services_overrides() {
        let wanted = BTreeSet::from(["org.example.named.desktop".to_string()]);
        let found = desktops(&share(&["share", "share-system", "missing"]), &wanted);
        let files: Vec<&str> = found.iter().map(|d| d.file.as_str()).collect();
        // Left out: the `NoDisplay` file in `kglobalaccel/` and its twin in `applications/` (the first file of a name
        // wins across both), a `Hidden` (deleted) file, and one without `X-KDE-Shortcuts` that `[services]` doesn't
        // name. `NoDisplay` in `applications/` is no reason to leave one out.
        assert_eq!(
            files,
            [
                "org.example.named.desktop",
                "org.example.nodisplay.desktop",
                "org.example.noname.desktop",
                "org.example.notes.desktop",
                "org.example.service.desktop",
                "org.example.term.desktop",
                "org.kde.dolphin.desktop",
                "org.kde.konsole.desktop",
                "org.kde.spectacle.desktop",
            ]
        );
        let (sheet, _) = parse(&fixture("kglobalshortcutsrc-plasma6"), &found);
        assert_eq!(
            rows(&sheet)["launch"],
            [
                "super+y = launch No Display",
                // No `Name`: the file name.
                "super+u = launch org.example.noname",
                // An action's override; an action without `Name` is described by its id (and its keys, written
                // in lower case, still read); an action missing from `Actions=` isn't listed.
                "super+shift+n = New Note",
                "super+shift+o = open",
                // `X-KDE-GlobalShortcutType=Service`: its actions only.
                "super+r = Run It",
                "super+shift+r = Run It",
                // The first data dir's file wins over the system one.
                "ctrl+alt+t = launch Example Terminal",
                // Dolphin is `none` in `[services]`; Konsole's default is replaced.
                "ctrl+alt+k = launch Konsole",
                "Print = launch Spectacle",
                // A shortcut given in System Settings to an app with none: described by the app's `Name`.
                "super+f = launch Named App",
            ]
        );
        // No `kglobalshortcutsrc`: the defaults alone.
        let (sheet, _) = parse("", &found);
        assert!(rows(&sheet)["launch"].contains(&"super+e = launch Dolphin".to_string()));
        assert!(rows(&sheet)["launch"].contains(&"ctrl+alt+y = launch Konsole".to_string()));
    }

    #[test]
    fn key_table() {
        for (qt, want) in [
            ("Meta+A", "super+a"),
            ("Ctrl+Alt+Shift+Z", "ctrl+alt+shift+z"),
            ("Meta+1", "super+1"),
            ("Meta+F12", "super+F12"),
            ("F35", "F35"),
            ("Esc", "Escape"),
            ("Backtab", "shift+Tab"),
            ("Ctrl+Backtab", "ctrl+shift+Tab"),
            ("Meta+PgUp", "super+Page_Up"),
            ("Ins", "Insert"),
            ("Del", "Delete"),
            ("Meta+Space", "super+space"),
            ("Media Play", "XF86AudioPlay"),
            ("Launch (0)", "XF86Launch0"),
            ("Volume Up", "XF86AudioRaiseVolume"),
            ("Meta+/", "super+slash"),
            ("Ctrl++", "ctrl+plus"),
            ("+", "plus"),
            ("Meta+,", "super+comma"),
            ("Meta+K, Meta+L", "super+k super+l"),
            ("Meta+,, Meta+L", "super+comma super+l"),
            // A modifier alone, as Plasma's launcher.
            ("Meta", "Super_L"),
            ("Alt", "Alt_L"),
            ("Touchpad Toggle", "XF86TouchpadToggle"),
            // Hand-written in a `.desktop` file, any case.
            ("ctrl+alt+t", "ctrl+alt+t"),
            ("META+esc", "super+Escape"),
            ("meta+f5", "super+F5"),
        ] {
            assert_eq!(notation(qt), Ok(want.to_string()), "{qt}");
        }
        for (qt, kind) in [
            ("Num+5", "keypad"),
            ("Meta+Num+Enter", "keypad"),
            ("Meta+É", "non-ASCII"),
            ("Toggle Media Play/Pause", "unknown Qt name"),
            ("Hyper+K", "unknown Qt name"),
            ("F36", "unknown Qt name"),
            ("Meta+K, Num+1", "keypad"),
        ] {
            assert_eq!(notation(qt), Err(kind), "{qt}");
        }
        // Every key the table gives is one `run` will send.
        for (_, x) in QT_KEYS {
            assert!(
                x.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "{x}"
            );
        }
    }

    #[test]
    fn kconfig_escapes() {
        assert_eq!(
            unescape(r"a\sb\tc\nd\re\\f\x41\,\;"),
            "a b\tc\nd\re\\fA\\,\\;"
        );
        assert_eq!(unescape(r"bad\xZZ end\"), "bad\\xZZ end\\");
        assert_eq!(
            list(r"Meta+\,,none,Zoom\, then\\pan"),
            ["Meta+,", "none", "Zoom, then\\pan"]
        );
        assert_eq!(list(""), [""]);
        // Only two hex digits make an `\xHH`.
        assert_eq!(unescape(r"\x+4"), "\\x+4");
    }

    #[test]
    fn ini_groups_and_keys_as_kconfig_reads_them() {
        let text = "\u{feff}[kwin]\nA=1\nB[$e]=2\nA=3\n[kwin][$i]\nC=4\n[a\\x5db]\nD=5\n[kwin][Ctx]\nE=6\nno equals sign\n";
        let groups = ini(text);
        let paths: Vec<Vec<String>> = groups.iter().map(|(p, _)| p.clone()).collect();
        // A BOM is no part of the first group; `[$i]` marks the group immutable, not nested; names are unescaped.
        assert_eq!(
            paths,
            [
                vec!["kwin".to_string()],
                vec!["a]b".into()],
                vec!["kwin".into(), "Ctx".into()]
            ]
        );
        // A repeated group is merged and a repeated key keeps its last value; `[$e]` is cut off the key.
        let pairs = |i: usize| -> Vec<(&str, &str)> {
            groups[i]
                .1
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect()
        };
        assert_eq!(pairs(0), [("B", "2"), ("A", "3"), ("C", "4")]);
        assert_eq!(pairs(2), [("E", "6")]);
    }

    #[test]
    fn paths_from_the_environment() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let e = env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "rel")]);
        assert_eq!(config_dir(&e), Some(PathBuf::from("/h/.config")));
        let e = env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "")]);
        assert_eq!(config_dir(&e), Some(PathBuf::from("/h/.config")));
        let e = env(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/c")]);
        assert_eq!(config_dir(&e), Some(PathBuf::from("/c")));
        let e = env(&[
            ("HOME", "/h"),
            ("XDG_DATA_HOME", ""),
            ("XDG_DATA_DIRS", "/a::rel:/b"),
        ]);
        assert_eq!(
            data_dirs(&e),
            [PathBuf::from("/h/.local/share"), "/a".into(), "/b".into()]
        );
        let e = env(&[("XDG_DATA_HOME", "/d")]);
        assert_eq!(
            data_dirs(&e),
            [
                PathBuf::from("/d"),
                "/usr/local/share".into(),
                "/usr/share".into()
            ]
        );
        assert_eq!(config_dir(&env(&[])), None);
    }

    #[test]
    fn missing_file_is_a_note_not_an_error() {
        let root = format!("{}/tests/fixtures/kde", env!("CARGO_MANIFEST_DIR"));
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(format!("{root}/no-such-dir")),
            "XDG_DATA_HOME" => Some(format!("{root}/no-such-share")),
            "XDG_DATA_DIRS" => Some(format!("{root}/no-such-system")),
            _ => None,
        };
        let (sheet, notes) = sheet_with(&env).unwrap();
        assert!(sheet.binds.is_empty());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("kglobalshortcutsrc"), "{notes:?}");
        // With launch defaults found, the missing file is not worth a note.
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(format!("{root}/no-such-dir")),
            "XDG_DATA_HOME" => Some(format!("{root}/share")),
            "XDG_DATA_DIRS" => Some(format!("{root}/no-such-system")),
            _ => None,
        };
        let (sheet, notes) = sheet_with(&env).unwrap();
        assert!(!sheet.binds.is_empty());
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn left_out_keys_are_one_note() {
        let dir = std::env::temp_dir().join(format!("kbf-kde-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("kglobalshortcutsrc"),
            fixture("kglobalshortcutsrc-plasma6"),
        )
        .unwrap();
        let config = dir.to_string_lossy().into_owned();
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(config.clone()),
            "XDG_DATA_HOME" | "XDG_DATA_DIRS" => Some(format!("{config}/no-such-share")),
            _ => None,
        };
        let result = sheet_with(&env);
        let _ = std::fs::remove_dir_all(&dir);
        let (sheet, notes) = result.unwrap();
        assert!(!sheet.binds.is_empty());
        assert_eq!(
            notes,
            ["leaving out 3 keys keybind-find can't send (keypad: 1, non-ASCII: 1, unknown Qt name: 1)"]
        );
    }
}
