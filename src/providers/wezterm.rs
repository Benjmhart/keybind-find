//! WezTerm: its keys from `wezterm show-keys`. WezTerm finds and runs its own config (`WEZTERM_CONFIG_FILE`,
//! `~/.wezterm.lua`, its config dirs), so defaults, `disable_default_key_bindings`, `keys`,
//! `key_tables` and `leader` come out as WezTerm resolves them; keybind-find never reads `wezterm.lua` itself. A
//! config that fails to load makes `show-keys` print WezTerm's defaults, with no error. Shown when a WezTerm window
//! has focus.

use super::nvim::single;
use crate::context::{run_for, Context};
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::Context as _;
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    read(command())
}

/// `wezterm show-keys` (the text form: only it prints the leader), with no display, no mise auto-install, from `/`.
fn command() -> Command {
    let mut cmd = Command::new("wezterm");
    cmd.arg("show-keys")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .current_dir("/");
    cmd
}

fn read(cmd: Command) -> anyhow::Result<Sheet> {
    let text = run_for(cmd, TIMEOUT).context("running wezterm show-keys")?;
    let (sheet, skipped) = parse(&text)?;
    if let Some(line) = left_out(skipped) {
        eprintln!("{line}");
    }
    Ok(sheet)
}

/// The one stderr line for `skipped` left-out binds; none when there are none.
fn left_out(skipped: usize) -> Option<String> {
    (skipped > 0).then(|| {
        format!(
            "keybind-find: wezterm: leaving out {skipped} binds (ENHANCED_KEY or an unknown modifier, a key with no X keysym, a key table name that can't be a scope, or a LEADER bind with no leader)"
        )
    })
}

/// The headers of `show_mouse`'s sections (`wezterm-gui/src/inputmap.rs`); each is printed only when it has binds.
const MOUSE: [&str; 4] = [
    "Mouse",
    "Mouse: alt_screen",
    "Mouse: mouse_reporting",
    "Mouse: mouse_reporting + alt_screen",
];

/// The sheet from `show-keys`' text (`wezterm-gui/src/inputmap.rs` `show_keys`), grouped by action name, and how many
/// binds were left out. `Default key table` binds have no scope, a `Key Table: <name>` section's get
/// `key_table=<name>`; the `Mouse…` sections and everything after them are ignored. A bind line is
/// `\t<mods>   <key>   ->   <action>`. Output the config writes to stdout (Lua's `io.write`) is block-buffered into the
/// pipe, so it usually lands at the end, after the `Mouse…` sections, where it is dropped; past ~4 KiB, or with
/// `io.flush()`, it can come first and run into the `Leader:` or `Default key table` line, which is still found. WezTerm lists some defaults under both shift spellings: a bind identical to one
/// already listed (key, scope and action) is listed once.
fn parse(text: &str) -> anyhow::Result<(Sheet, usize)> {
    let mut sheet = Sheet {
        title: "WezTerm".into(),
        provider: "wezterm".into(),
        binds: Default::default(),
    };
    let scope: Name = "key_table".parse()?;
    let mut skipped = 0;
    let mut leader: Option<String> = None;
    // `None` before the first section; `Some(Err(()))` in a table whose name can't be a scope.
    let mut table: Option<Result<Option<Name>, ()>> = None;
    for line in text.lines() {
        if table.is_none() {
            if let Some(i) = line.rfind("Leader: ") {
                leader = leader_chord(&line[i + "Leader: ".len()..]);
            } else if line.ends_with("Default key table") {
                table = Some(Ok(None));
            }
            continue;
        }
        if let Some(name) = line.strip_prefix("Key Table: ") {
            table = Some(name.parse::<Name>().map(Some).map_err(|_| ()));
            continue;
        }
        if MOUSE.contains(&line) {
            break;
        }
        let (Some(table), Some(bind)) = (&table, line.strip_prefix('\t')) else {
            continue;
        };
        let Some((lhs, action)) = bind.split_once("   ->   ") else {
            continue;
        };
        let Ok(table) = table else {
            skipped += 1;
            continue;
        };
        let Some(key) = bind_key(lhs, leader.as_deref()) else {
            skipped += 1;
            continue;
        };
        let action = action.trim();
        let category = action
            .split(['(', ' ', '{'])
            .next()
            .unwrap_or(action)
            .to_string();
        let when: std::collections::BTreeMap<Name, Values> = table
            .clone()
            .map(|t| (scope.clone(), Values::from(t)))
            .into_iter()
            .collect();
        let binds = sheet.binds.entry(category.clone()).or_default();
        if binds
            .iter()
            .any(|b| b.key == key && b.desc == action && b.when == when)
        {
            continue;
        }
        binds.push(Bind {
            key,
            desc: action.to_string(),
            action: category,
            when,
            run: Vec::new(),
        });
    }
    if table.is_none() {
        anyhow::bail!("wezterm show-keys printed no key table");
    }
    Ok((sheet, skipped))
}

/// A bind's `<mods>   <key>` (mods padded, empty for none) in keybind-find's notation, the leader first for a
/// `LEADER` bind; `None` when it is left out.
fn bind_key(lhs: &str, leader: Option<&str>) -> Option<String> {
    let lhs = lhs.trim_end();
    // The key comes after the last run of padding; a key holds at most single spaces (`A (Physical)`).
    let (mods, key) = lhs
        .rfind("   ")
        .map_or(("", lhs), |i| (&lhs[..i], &lhs[i + 3..]));
    match notation(mods, key.trim_start())? {
        (key, true) => Some(format!("{} {key}", leader?)),
        (key, false) => Some(key),
    }
}

/// `mods` (as `modifiers` reads them) and `key` (as `keysym` does) as one chord, and whether `LEADER` is among the mods.
fn notation(mods: &str, key: &str) -> Option<(String, bool)> {
    let (mut mods, is_leader) = modifiers(mods)?;
    let (shift, sym) = keysym(key)?;
    if shift {
        mods.push("shift");
    }
    Some((chord(&mods, &sym), is_leader))
}

/// Modifiers as WezTerm's `Debug` prints them (`SHIFT | CTRL`): keybind-find's names, and whether `LEADER` is one;
/// `None` for `ENHANCED_KEY` or a name not known here.
fn modifiers(mods: &str) -> Option<(Vec<&'static str>, bool)> {
    let mut out = Vec::new();
    let mut leader = false;
    for m in mods.split('|').map(str::trim).filter(|m| !m.is_empty()) {
        let m = m
            .strip_prefix("LEFT_")
            .or_else(|| m.strip_prefix("RIGHT_"))
            .unwrap_or(m);
        match m {
            "SHIFT" => out.push("shift"),
            "ALT" => out.push("alt"),
            "CTRL" => out.push("ctrl"),
            "SUPER" => out.push("super"),
            "LEADER" => leader = true,
            "NONE" => {}
            _ => return None,
        }
    }
    Some((out, leader))
}

/// The leader chord from the `Leader: <KeyCode debug> <mods> <duration>` line (`Char('a') CTRL 1.5s`); `None` when it
/// doesn't read, so its `LEADER` binds are left out.
fn leader_chord(line: &str) -> Option<String> {
    // The duration is the last word; a `Char(' ')` key holds a space.
    let line = line.trim_end();
    let rest = &line[..line.rfind(' ')?];
    let (key, mods) = match rest.strip_prefix("Char('") {
        Some(c) => {
            let end = c.find("')")?;
            // `Debug`'s escapes, then the names `human_key` gives those characters.
            let key = match &c[..end] {
                "\\'" => "'",
                "\\\\" => "\\",
                "\\r" => "Enter",
                "\\t" => "Tab",
                "\\u{1b}" => "Escape",
                "\\u{8}" => "Backspace",
                // `Debug` tells Delete apart, unlike `human_key` (which prints it as Escape).
                "\\u{7f}" => "Delete",
                s if s.chars().count() == 1 => s,
                _ => return None,
            };
            (key.to_string(), &c[end + 2..])
        }
        None => {
            let (key, mods) = rest.split_once(' ').unwrap_or((rest, ""));
            let key = match key.strip_prefix("Function(") {
                Some(n) => format!("F{}", n.strip_suffix(')')?),
                None => match key.strip_prefix("Numpad(") {
                    Some(n) => format!("Numpad{}", n.strip_suffix(')')?),
                    None => match key.strip_prefix("Physical(") {
                        Some(p) => format!("{} (Physical)", p.strip_suffix(')')?),
                        None => key.to_string(),
                    },
                },
            };
            (key, mods)
        }
    };
    match notation(mods, &key)? {
        (key, false) => Some(key),
        (_, true) => None,
    }
}

/// A key as `human_key` prints it, as an X keysym, and whether it means shift (an upper-case letter); `None` for a
/// key with no translation (another named key, a control character, `Composed(…)`, non-ASCII).
///
/// WezTerm prints Delete (`Char('\x7f')`) as `Escape` too (`human_key`): such a bind is listed as Escape.
fn keysym(key: &str) -> Option<(bool, String)> {
    if let Some(phys) = key.strip_suffix(" (Physical)") {
        return physical(phys).map(|k| (false, k.to_string()));
    }
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_uppercase() {
            return Some((true, c.to_ascii_lowercase().to_string()));
        }
        return single(c).map(|k| (false, k.to_string()));
    }
    named(key).map(|k| (false, k))
}

/// A named key in `human_key`'s spelling (a `KeyCode` variant's name, `F<n>`, `Numpad<n>`) as an X keysym.
fn named(key: &str) -> Option<String> {
    if let Some(n) = key
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=35).contains(n))
    {
        return Some(format!("F{n}"));
    }
    if let Some(n) = key
        .strip_prefix("Numpad")
        .filter(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
    {
        return Some(format!("KP_{n}"));
    }
    let x = match key {
        "Enter" => "Return",
        "Escape" => "Escape",
        // Only from a `Leader:` line or a physical key: `human_key` prints Delete as `Escape`.
        "Delete" => "Delete",
        "Backspace" => "BackSpace",
        "Space" => "space",
        "Tab" => "Tab",
        "PageUp" => "Prior",
        "PageDown" => "Next",
        "Home" => "Home",
        "End" => "End",
        "Insert" => "Insert",
        "LeftArrow" => "Left",
        "RightArrow" => "Right",
        "UpArrow" => "Up",
        "DownArrow" => "Down",
        "Copy" => "XF86Copy",
        "Cut" => "XF86Cut",
        "Paste" => "XF86Paste",
        _ => return None,
    };
    Some(x.to_string())
}

/// A physical key's name (`PhysKeyCode`'s, as WezTerm prints it) as an X keysym. A letter is the key itself, no
/// shift: WezTerm prints it in either case (its name table holds both, read back through a hash map).
fn physical(name: &str) -> Option<String> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return c
            .is_ascii_alphanumeric()
            .then(|| c.to_ascii_lowercase().to_string());
    }
    let x = match name {
        "Return" => "Return",
        "Backslash" => "backslash",
        "Comma" => "comma",
        "Equal" => "equal",
        "Grave" => "grave",
        "LeftBracket" => "bracketleft",
        "RightBracket" => "bracketright",
        "Minus" => "minus",
        "Period" => "period",
        "Quote" => "apostrophe",
        "Semicolon" => "semicolon",
        "Slash" => "slash",
        _ => return named(name),
    };
    Some(x.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/wezterm/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// Every bind as `key = desc`, with ` [key_table=…]` when scoped, in category order.
    fn rows(sheet: &Sheet) -> Vec<String> {
        sheet
            .binds
            .values()
            .flatten()
            .map(|b| {
                let table = b
                    .when
                    .values()
                    .map(|v| {
                        let names: Vec<String> = v.iter().map(Name::to_string).collect();
                        format!(" [key_table={}]", names.join("|"))
                    })
                    .collect::<String>();
                format!("{} = {}{table}", b.key, b.desc)
            })
            .collect()
    }

    fn has(rows: &[String], row: &str) -> bool {
        rows.iter().any(|r| r == row)
    }

    /// `run::check`'s rule for a key keybind-find sends: `+`-joined `[A-Za-z0-9_]` parts, modifiers xdotool can press.
    fn sendable(key: &str) -> bool {
        key.split_whitespace().all(|chord| {
            let parts: Vec<&str> = chord.split('+').collect();
            parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                && parts[..parts.len() - 1]
                    .iter()
                    .all(|m| ["super", "ctrl", "alt", "shift"].contains(m))
        })
    }

    /// The parse's rows, each key sendable and none repeated with the same scope and description.
    fn checked(text: &str) -> (Sheet, Vec<String>, usize) {
        let (sheet, skipped) = parse(text).unwrap();
        let rows = rows(&sheet);
        for b in sheet.binds.values().flatten() {
            assert!(sendable(&b.key), "{:?}", b.key);
        }
        let mut seen = std::collections::BTreeSet::new();
        for r in &rows {
            assert!(seen.insert(r), "{r} listed twice");
        }
        (sheet, rows, skipped)
    }

    #[test]
    fn parses_the_e2e_config() {
        let (sheet, rows, skipped) = checked(&fixture("show-keys.txt"));
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str()),
            ("WezTerm", "wezterm")
        );
        for want in [
            // The config's key (`x` with CTRL|SHIFT, printed `CTRL X`).
            "ctrl+shift+x = CloseCurrentTab { confirm: false }",
            // Leader sequences, from the `Leader: Char('a') CTRL 1.5s` line.
            "ctrl+a c = SpawnTab(CurrentPaneDomain)",
            "ctrl+a shift+r = ActivateKeyTable { name: \"e2e\", timeout_milliseconds: None, replace_current: false, one_shot: false, until_unknown: false, prevent_fallback: false }",
            "ctrl+alt+k = ShowLauncher",
            "ctrl+shift+space = QuickSelect",
            "alt+Return = ToggleFullScreen",
            "ctrl+shift+Tab = ActivateTabRelative(-1)",
            "XF86Copy = CopyTo(Clipboard)",
            "shift+Prior = ScrollByPage(-1.0)",
            "ctrl+shift+w = CloseCurrentTab { confirm: true }",
            "super+w = CloseCurrentTab { confirm: true }",
            // A user table, and the table WezTerm always adds.
            "Left = AdjustPaneSize(Left, 1) [key_table=e2e]",
            "Escape = PopKeyTable [key_table=e2e]",
            "ctrl+c = Multiple([ScrollToBottom, CopyMode(Close)]) [key_table=copy_mode]",
            "ctrl+n = CopyMode(NextMatch) [key_table=search_mode]",
        ] {
            assert!(has(&rows, want), "{want} not in {rows:#?}");
        }
        // `é`, `RawCode(123)`, `VolumeUp`, and the bind of the `e2e wide` table.
        assert_eq!(skipped, 4);
        assert_eq!(rows.len(), 177, "{rows:#?}");
        // Grouped by action name; the mouse sections are ignored.
        assert_eq!(sheet.binds["SpawnTab"][0].action, "SpawnTab");
        assert!(sheet.binds["SpawnTab"][0].when.is_empty());
        assert!(!sheet.binds.contains_key("SelectTextAtMouseCursor"));
        assert!(!rows
            .iter()
            .any(|r| r.contains("Drag") || r.contains("streak")));
    }

    #[test]
    fn parses_the_defaults() {
        let (_, rows, skipped) = checked(&fixture("defaults.txt"));
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 172, "{rows:#?}");
        // No leader: no sequences.
        assert!(!rows
            .iter()
            .any(|r| r.split(" = ").next().unwrap().contains(' ')));
        assert!(has(&rows, "ctrl+shift+x = ActivateCopyMode"));
    }

    /// One `Default key table` holding `lines` (`<mods>   <key>   ->   <action>`), after `head` (a `Leader:` line).
    fn table(head: &str, lines: &[&str]) -> String {
        let binds: String = lines.iter().map(|l| format!("\t{l}\n")).collect();
        format!("{head}Default key table\n-----------------\n\n{binds}\n")
    }

    #[test]
    fn notation() {
        let one = |mods: &str, key: &str| {
            let (sheet, skipped) =
                parse(&table("", &[&format!("{mods:12}   {key:14}   ->   Nop")])).unwrap();
            (rows(&sheet).first().cloned(), skipped)
        };
        for (mods, key, want) in [
            ("SHIFT | CTRL", "C", "ctrl+shift+c"),
            ("SHIFT | CTRL", "c", "ctrl+shift+c"),
            // An upper-case letter is WezTerm's ctrl+shift spelling.
            ("CTRL", "P", "ctrl+shift+p"),
            ("", "A", "shift+a"),
            ("NONE", "x", "x"),
            ("LEFT_ALT | RIGHT_CTRL", "Tab", "ctrl+alt+Tab"),
            ("SUPER | LEFT_SHIFT", "Enter", "super+shift+Return"),
            ("CTRL", "A (Physical)", "ctrl+a"),
            ("CTRL", "a (Physical)", "ctrl+a"),
            ("CTRL", "1 (Physical)", "ctrl+1"),
            ("CTRL", "Equal (Physical)", "ctrl+equal"),
            ("CTRL", "Space (Physical)", "ctrl+space"),
            ("CTRL", "LeftArrow (Physical)", "ctrl+Left"),
            ("", "Numpad5", "KP_5"),
            ("", "LeftArrow", "Left"),
            ("ALT", "UpArrow", "alt+Up"),
            ("", "PageDown", "Next"),
            ("", "Copy", "XF86Copy"),
            ("", "Cut", "XF86Cut"),
            ("SHIFT", "Paste", "shift+XF86Paste"),
            ("", "Backspace", "BackSpace"),
            ("", "Space", "space"),
            ("CTRL", "F12", "ctrl+F12"),
            ("", "RightArrow", "Right"),
            ("", "DownArrow", "Down"),
            ("CTRL", "Quote (Physical)", "ctrl+apostrophe"),
            ("CTRL", "Backslash (Physical)", "ctrl+backslash"),
            ("", "Home", "Home"),
            ("", "End", "End"),
            ("", "Insert", "Insert"),
            ("", "Escape", "Escape"),
            ("CTRL", "-", "ctrl+minus"),
            ("CTRL", ">", "ctrl+greater"),
        ] {
            assert_eq!(
                one(mods, key),
                (Some(format!("{want} = Nop")), 0),
                "{mods} {key}"
            );
        }
        // Left out and counted: ENHANCED_KEY or an unknown modifier, a key with no translation.
        for (mods, key) in [
            ("ENHANCED_KEY", "x"),
            ("HYPER", "x"),
            ("CTRL", "Composed(\"a b\")"),
            ("CTRL", "é"),
            ("CTRL", "\u{1}"),
            ("CTRL", "VolumeUp"),
            ("CTRL", "RawCode(123)"),
            ("CTRL", "Numpad10"),
            ("CTRL", "F0"),
            ("CTRL", "Keypad5 (Physical)"),
            ("CTRL", "F36"),
        ] {
            assert_eq!(one(mods, key), (None, 1), "{mods} {key}");
        }
    }

    #[test]
    fn leader() {
        let lines = [
            "LEADER         c   ->   SpawnTab(CurrentPaneDomain)",
            "LEADER | SHIFT   R   ->   X",
        ];
        let keys = |head: &str| {
            let (sheet, skipped) = parse(&table(head, &lines)).unwrap();
            let mut keys: Vec<String> = sheet
                .binds
                .values()
                .flatten()
                .map(|b| b.key.clone())
                .collect();
            keys.sort();
            (keys, skipped)
        };
        assert_eq!(
            keys("Leader: Char('a') CTRL 1.5s\n"),
            (vec!["ctrl+a c".into(), "ctrl+a shift+r".into()], 0)
        );
        assert_eq!(
            keys("Leader: Char(' ') CTRL 1.5s\n"),
            (vec!["ctrl+space c".into(), "ctrl+space shift+r".into()], 0)
        );
        assert_eq!(
            keys("Leader: Char('B') SHIFT | ALT 1s\n").0[0],
            "alt+shift+b c"
        );
        assert_eq!(
            keys("Leader: Char('\\'') CTRL 500ms\n").0[0],
            "ctrl+apostrophe c"
        );
        assert_eq!(keys("Leader: Function(5) NONE 1s\n").0[0], "F5 c");
        assert_eq!(keys("Leader: Physical(A) SUPER 1s\n").0[0], "super+a c");
        assert_eq!(keys("Leader: Char('\\r') CTRL 1s\n").0[0], "ctrl+Return c");
        assert_eq!(keys("Leader: Char('\\u{1b}') NONE 1s\n").0[0], "Escape c");
        assert_eq!(
            keys("Leader: Char(')') CTRL 1s\n").0[0],
            "ctrl+parenright c"
        );
        assert_eq!(keys("Leader: Numpad(5) CTRL 1s\n").0[0], "ctrl+KP_5 c");
        // `Debug` tells Delete apart (`human_key` prints it as Escape).
        assert_eq!(
            keys("Leader: Char('\\u{7f}') CTRL 1s\n").0[0],
            "ctrl+Delete c"
        );
        assert_eq!(keys("Leader: Char('\\t') CTRL 1s\n").0[0], "ctrl+Tab c");
        assert_eq!(
            keys("Leader: Char('\\\\') CTRL 1s\n").0[0],
            "ctrl+backslash c"
        );
        assert_eq!(keys("Leader: Char('\\u{8}') NONE 1s\n").0[0], "BackSpace c");
        // Text the config wrote to stdout with no newline runs into the line.
        assert_eq!(keys("junkLeader: Char('a') CTRL 1s\n").0[0], "ctrl+a c");
        assert_eq!(
            keys("xLeader: fooLeader: Char('a') CTRL 1s\n").0[0],
            "ctrl+a c"
        );
        // No `Leader:` line, or one that doesn't read: the LEADER binds are left out.
        assert_eq!(keys(""), (vec![], 2));
        assert_eq!(keys("Leader: Char('é') CTRL 1s\n"), (vec![], 2));
        assert_eq!(keys("Leader: Char('a') LEADER 1s\n"), (vec![], 2));
        assert_eq!(keys("Leader: garbled\n"), (vec![], 2));
    }

    #[test]
    fn sections_dedupe_and_odd_lines() {
        let text = "stray outputDefault key table\n-----------------\n\n\
                    \tCTRL           W   ->   CloseCurrentTab { confirm: true }\n\
                    \tCTRL           X   ->   CloseCurrentTab { confirm: false }\n\
                    \t               q   ->   CopyMode(Close)\n\
                    \t               s   ->   ShowLauncher\n\
                    \tSHIFT | CTRL   W   ->   CloseCurrentTab { confirm: true }\n\
                    \tSHIFT | CTRL   w   ->   CloseCurrentTab { confirm: true }\n\
                    \tSHIFT | CTRL   w   ->   Other\n\
                    \tno arrow here\n\n\
                    Key Table: copy_mode\n-------------------\n\n\
                    \t   w   ->   CloseCurrentTab { confirm: true }\n\
                    \t   q   ->   CopyMode(Close)\n\n\
                    Key Table: wide name\n--------------------\n\n\
                    \t   q   ->   PopKeyTable\n\n\
                    Key Table: Mouse\n-----------------\n\n\
                    \t   m   ->   Nop\n\n\
                    Mouse\n-----\n\n\
                    \t   Down { streak: 1, button: Left }   ->   SelectTextAtMouseCursor(Cell)\n\n\
                    Key Table: after_mouse\n\n\t   z   ->   Nop\n";
        let (sheet, rows, skipped) = checked(text);
        assert_eq!(
            rows,
            [
                "ctrl+shift+w = CloseCurrentTab { confirm: true }",
                "ctrl+shift+x = CloseCurrentTab { confirm: false }",
                "w = CloseCurrentTab { confirm: true } [key_table=copy_mode]",
                // The same key and action in another table is another bind.
                "q = CopyMode(Close)",
                "q = CopyMode(Close) [key_table=copy_mode]",
                // A key table named `Mouse` isn't the mouse section.
                "m = Nop [key_table=Mouse]",
                "ctrl+shift+w = Other",
                "s = ShowLauncher",
            ]
        );
        // The category is the action's name, before `(`, ` {` or a space.
        assert_eq!(
            sheet.binds.keys().collect::<Vec<_>>(),
            [
                "CloseCurrentTab",
                "CopyMode",
                "Nop",
                "Other",
                "ShowLauncher"
            ]
        );
        assert_eq!(sheet.binds["CloseCurrentTab"].len(), 3);
        // The `wide name` table's bind.
        assert_eq!(skipped, 1);
        assert_eq!(sheet.binds["CopyMode"][0].action, "CopyMode");
        // No section at all: not show-keys' output.
        assert!(parse("Usage: wezterm\n").is_err());
        assert!(parse("").is_err());
    }

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn each_mouse_header_ends_the_tables() {
        for header in &MOUSE[1..] {
            let text = format!(
                "Default key table\n-----------------\n\n\tCTRL   x   ->   Nop\n\n{header}\n{}\n\n\
                 \t   Down {{ streak: 1, button: Left }}   ->   SelectTextAtMouseCursor(Cell)\n",
                "-".repeat(header.len())
            );
            let (sheet, skipped) = parse(&text).unwrap();
            assert_eq!(
                (rows(&sheet), skipped),
                (vec!["ctrl+x = Nop".to_string()], 0),
                "{header}"
            );
        }
    }

    #[test]
    fn left_out_line() {
        assert_eq!(left_out(0), None);
        assert_eq!(
            left_out(4).as_deref(),
            Some("keybind-find: wezterm: leaving out 4 binds (ENHANCED_KEY or an unknown modifier, a key with no X keysym, a key table name that can't be a scope, or a LEADER bind with no leader)")
        );
    }

    #[test]
    fn the_command() {
        assert_eq!(TIMEOUT, Duration::from_secs(3));
        let cmd = command();
        assert_eq!(cmd.get_program(), "wezterm");
        assert_eq!(args(&cmd), ["show-keys"]);
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for k in ["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"] {
            assert!(removed.iter().any(|r| r == k), "{k}");
        }
        let mise = cmd
            .get_envs()
            .find(|(k, _)| *k == "MISE_NOT_FOUND_AUTO_INSTALL")
            .and_then(|(_, v)| v);
        assert_eq!(mise, Some(std::ffi::OsStr::new("0")));
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
    }

    /// `command()` with `PATH` holding only a fake `wezterm` running `body` (written through `sh`: ETXTBSY).
    fn fake(name: &str, body: &str) -> (Command, Cleanup) {
        let bin = crate::test_tmp().join(format!("kbf-wezterm-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("wezterm"))
            .arg(body)
            .status()
            .unwrap();
        assert!(ok.success());
        let mut cmd = command();
        cmd.env("PATH", &bin);
        (cmd, Cleanup(bin))
    }

    /// Removes a test's directory when dropped, failed asserts included.
    struct Cleanup(std::path::PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn failures_are_one_error() {
        let (cmd, _dir) = fake("fails", "echo 'no such thing' >&2; exit 1");
        let e = format!("{:#}", read(cmd).unwrap_err());
        assert!(
            e.starts_with("running wezterm show-keys: ") && e.contains("no such thing"),
            "{e}"
        );
        // Not installed.
        let mut cmd = command();
        cmd.env("PATH", "/nonexistent");
        let e = read(cmd).unwrap_err();
        assert_eq!(
            e.root_cause()
                .downcast_ref::<std::io::Error>()
                .map(|e| e.kind()),
            Some(std::io::ErrorKind::NotFound),
            "{e:#}"
        );
        // A hang is killed at the timeout: waited for (so it ran), then given up.
        let (cmd, _hangs) = fake("hangs", "PATH=/usr/bin:/bin; sleep 10");
        let started = std::time::Instant::now();
        let e = format!("{:#}", read(cmd).unwrap_err());
        let took = started.elapsed();
        assert!(e.starts_with("running wezterm show-keys"), "{e}");
        assert!(
            took >= TIMEOUT - Duration::from_millis(100) && took < TIMEOUT + Duration::from_secs(2),
            "{took:?}"
        );
        // The fake's output reaches the parser, from `show-keys`.
        let (cmd, _ok) = fake(
            "ok",
            "[ \"$*\" = show-keys ] || exit 9\nprintf 'Default key table\\n\\n\\tCTRL   x   ->   Nop\\n'",
        );
        assert_eq!(rows(&read(cmd).unwrap()), ["ctrl+x = Nop"]);
    }

    #[test]
    fn real_wezterm() {
        let have = Command::new("wezterm")
            .arg("--version")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !have {
            eprintln!("skipping: wezterm is not installed");
            return;
        }
        // A home of its own, so the user's wezterm.lua is never read.
        let home = crate::test_tmp().join(format!("kbf-wezterm-real-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let _home = Cleanup(home.clone());
        let config = home.join("wezterm.lua");
        let run = |lua: &str| {
            std::fs::write(&config, lua).unwrap();
            let mut cmd = command();
            cmd.env("HOME", &home)
                .env("XDG_CONFIG_HOME", home.join("config"))
                .env("WEZTERM_CONFIG_FILE", &config);
            read(cmd).map(|s| rows(&s))
        };
        let configured = run(
            "local wezterm = require 'wezterm'\nreturn { leader = { key = 'b', mods = 'CTRL' }, keys = { \
             { key = 'F12', mods = 'CTRL|SHIFT', action = wezterm.action.ShowLauncher }, \
             { key = 'n', mods = 'LEADER', action = wezterm.action.ShowLauncher } } }\n",
        )
        .unwrap();
        // A config that fails to load: WezTerm's defaults, no error.
        let broken = run("return {").unwrap();
        assert!(
            has(&configured, "ctrl+shift+F12 = ShowLauncher"),
            "{configured:#?}"
        );
        assert!(
            has(&configured, "ctrl+b n = ShowLauncher"),
            "{configured:#?}"
        );
        assert!(
            has(&broken, "ctrl+shift+c = CopyTo(Clipboard)"),
            "{broken:#?}"
        );
        assert!(
            !broken.iter().any(|r| r.contains("ShowLauncher")),
            "{broken:#?}"
        );
    }
}
