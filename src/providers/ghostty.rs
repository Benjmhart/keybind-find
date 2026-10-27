//! ghostty: its keys from `ghostty +list-keybinds --plain`. ghostty reads its own config files (and their
//! `config-file` includes), so this is the effective list, as on disk: a config edited but not reloaded shows the new
//! keys. Shown when a ghostty window has focus.

use super::nvim::single;
use crate::context::{run_for, Context};
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::Context as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (cmd, note) = command(&env, &version);
    if let Some(note) = note {
        eprintln!("keybind-find: ghostty: {note}");
    }
    read(cmd)
}

/// The running ghostty's major and minor version, from `ghostty --version`.
fn version() -> Option<(u32, u32)> {
    let mut cmd = Command::new("ghostty");
    cmd.arg("--version");
    isolate(&mut cmd);
    version_of(&run_for(cmd, TIMEOUT).ok()?)
}

/// `ghostty +list-keybinds --plain`, with `--default` when ghostty would find no config file: then it would write a
/// template config, and keybind-find never creates files. `config.ghostty` counts only from ghostty 1.3 (`version`, asked
/// only then); before, ghostty reads only `config`. `XDG_CONFIG_HOME` is set to the directory checked, so ghostty
/// reads the same place (it would take a relative one as relative to `/`). Also a note when `config.ghostty` is passed
/// over because the version can't be read.
fn command(env: Env, version: &dyn Fn() -> Option<(u32, u32)>) -> (Command, Option<String>) {
    let dir = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            env("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        });
    let mut note = None;
    let has_config = dir.as_ref().is_some_and(|d| {
        d.join("ghostty/config").exists()
            || d.join("ghostty/config.ghostty").exists()
                && match version() {
                    Some(v) => v >= (1, 3),
                    None => {
                        note = Some(
                            "can't read `ghostty --version`, so not trusting config.ghostty (ghostty before 1.3 \
                             would write a template config); showing ghostty's default keys"
                                .to_string(),
                        );
                        false
                    }
                }
    });
    let mut cmd = Command::new("ghostty");
    cmd.arg("+list-keybinds");
    if !has_config {
        cmd.arg("--default");
    }
    cmd.arg("--plain");
    if let Some(dir) = dir {
        cmd.env("XDG_CONFIG_HOME", dir);
    }
    isolate(&mut cmd);
    (cmd, note)
}

/// No display (ghostty must not open a window), no mise auto-install, run from `/`.
fn isolate(cmd: &mut Command) {
    cmd.env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET")
        .current_dir("/");
}

/// Major and minor from `ghostty --version`'s `  - version: 1.4.0` line.
fn version_of(text: &str) -> Option<(u32, u32)> {
    let v = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("- version:"))?
        .trim();
    let mut parts = v.split(['.', '-', '+']);
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn read(cmd: Command) -> anyhow::Result<Sheet> {
    let text = run_for(cmd, TIMEOUT).context("running ghostty +list-keybinds")?;
    let (sheet, skipped) = parse(&text)?;
    if skipped > 0 {
        eprintln!(
            "keybind-find: ghostty: leaving out {skipped} binds (catch_all, a non-ASCII key, a key with no X keysym, a key table name that can't be a scope, or a line ghostty printed garbled)"
        );
    }
    Ok(sheet)
}

/// The sheet from `+list-keybinds --plain`'s `keybind = <trigger>=<action>` lines, grouped by action, and how many
/// binds were left out. A `chain=<action>` line adds its action to the bind before it.
///
/// ghostty (1.4.0 `Set.Value.formatEntries`) writes `chain=` over the start of its buffer, so the binds after a chained
/// one in the same sequence come out with the start of that line in place of their prefix (`ctrl+a>c=new_tab` after
/// `chain=quit` prints `chain=>c=new_tab`; `g>w=…` prints `c>w=…`): such a line is left out, as its first keys are
/// lost (see `garbled`).
fn parse(text: &str) -> anyhow::Result<(Sheet, usize)> {
    let mut sheet = Sheet {
        title: "ghostty".into(),
        provider: "ghostty".into(),
        binds: Default::default(),
    };
    let scope: Name = "key_table".parse()?;
    let mut skipped = 0;
    // Where the last bind went, for a `chain=` line; `None` after a left-out one.
    let mut last: Option<(String, usize)> = None;
    // The last `chain=` line, while ghostty's buffer may still start with it.
    let mut chain: Option<&str> = None;
    for line in text.lines() {
        // Only the start is trimmed: an action's argument may end in a space.
        let Some(entry) = line
            .trim_start()
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("keybind")
            .and_then(|l| l.trim_start().strip_prefix('='))
            .map(str::trim_start)
        else {
            continue;
        };
        if entry.is_empty() {
            continue;
        }
        if chain.is_some_and(|c| garbled(entry, c)) {
            skipped += 1;
            last = None;
            continue;
        }
        if let Some(action) = entry.strip_prefix("chain=") {
            chain = Some(entry);
            if let Some((category, i)) = &last {
                if let Some(b) = sheet.binds.get_mut(category).and_then(|v| v.get_mut(*i)) {
                    b.desc = format!("{}, then {action}", b.desc);
                }
            }
            continue;
        }
        // A whole new line: ghostty wrote it from the start of its buffer.
        chain = None;
        let Some((table, chords, action)) = split(entry) else {
            skipped += 1;
            last = None;
            continue;
        };
        let table = match table.map(|t| t.parse::<Name>()) {
            None => None,
            Some(Ok(t)) => Some(t),
            Some(Err(_)) => {
                skipped += 1;
                last = None;
                continue;
            }
        };
        let Some(keys) = chords
            .iter()
            .map(|(mods, key)| keysym(key).map(|k| chord(mods, &k)))
            .collect::<Option<Vec<_>>>()
        else {
            skipped += 1;
            last = None;
            continue;
        };
        let category = action.split(':').next().unwrap_or(action).to_string();
        let binds = sheet.binds.entry(category.clone()).or_default();
        binds.push(Bind {
            key: keys.join(" "),
            desc: action.to_string(),
            action: category.clone(),
            when: table
                .map(|t| (scope.clone(), Values::from(t)))
                .into_iter()
                .collect(),
        });
        last = Some((category, binds.len() - 1));
    }
    Ok((sheet, skipped))
}

/// Whether `entry` is a bind ghostty printed over the `chain` line before it: the start of `chain` (all of it, then
/// what was left of the bind's own prefix), `>`, then a trigger and an action.
/// ponytail: a real root bind starting with the chain line's first characters and `>` (`c>x=…` right after a chain
/// line) reads as garbled too; ghostty's output can't tell them apart.
fn garbled(entry: &str, chain: &str) -> bool {
    entry.match_indices('>').any(|(i, _)| {
        let k = i.min(chain.len());
        i > 0 && entry.as_bytes()[..k] == chain.as_bytes()[..k] && split(&entry[i + 1..]).is_some()
    })
}

/// Each chord's modifiers and key name.
type Chords<'a> = Vec<(Vec<&'static str>, &'a str)>;

/// Splits `[<table>/]<trigger>=<action>` into the table, the chords and the action. A key may itself be `=`, `>`, `+`
/// or `/`, so the trigger is read chord by chord: modifiers, then one key (a name, or one character), then `>`
/// (another chord) or `=` (the action). `None` when it doesn't read.
fn split(entry: &str) -> Option<(Option<&str>, Chords<'_>, &str)> {
    // A table name comes before the first `/`, and can't hold a trigger's `+`, `>` or `=`.
    let (table, mut rest) = match entry.split_once('/') {
        Some((t, rest)) if !t.is_empty() && !t.contains(['+', '>', '=']) => (Some(t), rest),
        _ => (None, entry),
    };
    let mut chords = Vec::new();
    loop {
        let mut mods = Vec::new();
        while let Some(m) = ["super", "ctrl", "alt", "shift"]
            .into_iter()
            .find(|m| rest.strip_prefix(m).is_some_and(|r| r.starts_with('+')))
        {
            mods.push(m);
            rest = &rest[m.len() + 1..];
        }
        rest = rest.strip_prefix("physical:").unwrap_or(rest);
        let name = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let len = match name {
            0 => rest.chars().next()?.len_utf8(),
            n => n,
        };
        chords.push((mods, &rest[..len]));
        rest = &rest[len..];
        match rest.chars().next()? {
            '>' => rest = &rest[1..],
            '=' => return Some((table, chords, &rest[1..])),
            _ => return None,
        }
    }
}

/// A ghostty key name (1.2+'s W3C names or 1.0/1.1's) or character as an X keysym; `None` for `catch_all`, a
/// non-ASCII character, or a name with no keysym.
fn keysym(key: &str) -> Option<String> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_lowercase().to_string());
        }
        return single(c).map(str::to_string);
    }
    if let Some(c) = key.strip_prefix("key_").filter(|c| c.len() == 1) {
        return Some(c.to_string());
    }
    if let Some(d) = key.strip_prefix("digit_").filter(|d| d.len() == 1) {
        return Some(d.to_string());
    }
    if let Some(n) = key
        .strip_prefix('f')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=25).contains(n))
    {
        return Some(format!("F{n}"));
    }
    if let Some(k) = key
        .strip_prefix("numpad_")
        .or_else(|| key.strip_prefix("kp_"))
    {
        if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() {
            return Some(format!("KP_{k}"));
        }
        let kp = match k {
            "add" => "KP_Add",
            "subtract" => "KP_Subtract",
            "multiply" => "KP_Multiply",
            "divide" => "KP_Divide",
            "decimal" => "KP_Decimal",
            "enter" => "KP_Enter",
            "equal" => "KP_Equal",
            "separator" => "KP_Separator",
            "up" => "KP_Up",
            "down" => "KP_Down",
            "left" => "KP_Left",
            "right" => "KP_Right",
            "home" => "KP_Home",
            "end" => "KP_End",
            "insert" => "KP_Insert",
            "delete" => "KP_Delete",
            "page_up" => "KP_Prior",
            "page_down" => "KP_Next",
            "begin" => "KP_Begin",
            _ => return None,
        };
        return Some(kp.to_string());
    }
    let x = match key {
        // 1.0/1.1 digits.
        "zero" => "0",
        "one" => "1",
        "two" => "2",
        "three" => "3",
        "four" => "4",
        "five" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" => "9",
        "backquote" | "grave_accent" => "grave",
        "quote" | "apostrophe" => "apostrophe",
        "bracket_left" | "left_bracket" => "bracketleft",
        "bracket_right" | "right_bracket" => "bracketright",
        "backslash" => "backslash",
        "comma" => "comma",
        "equal" => "equal",
        "minus" => "minus",
        "plus" => "plus",
        "period" => "period",
        "semicolon" => "semicolon",
        "slash" => "slash",
        "space" => "space",
        "tab" => "Tab",
        "enter" => "Return",
        "backspace" => "BackSpace",
        "escape" => "Escape",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "page_up" => "Prior",
        "page_down" => "Next",
        "arrow_up" | "up" => "Up",
        "arrow_down" | "down" => "Down",
        "arrow_left" | "left" => "Left",
        "arrow_right" | "right" => "Right",
        "caps_lock" => "Caps_Lock",
        "num_lock" => "Num_Lock",
        "scroll_lock" => "Scroll_Lock",
        "print_screen" => "Print",
        "pause" => "Pause",
        "context_menu" => "Menu",
        "help" => "Help",
        "alt_left" | "left_alt" => "Alt_L",
        "alt_right" | "right_alt" => "Alt_R",
        "control_left" | "left_control" => "Control_L",
        "control_right" | "right_control" => "Control_R",
        "meta_left" | "left_super" => "Super_L",
        "meta_right" | "right_super" => "Super_R",
        "shift_left" | "left_shift" => "Shift_L",
        "shift_right" | "right_shift" => "Shift_R",
        "convert" => "Henkan",
        "non_convert" => "Muhenkan",
        "kana_mode" => "Hiragana_Katakana",
        "browser_back" => "XF86Back",
        "browser_forward" => "XF86Forward",
        "browser_refresh" => "XF86Refresh",
        "browser_stop" => "XF86Stop",
        "browser_search" => "XF86Search",
        "browser_favorites" => "XF86Favorites",
        "browser_home" => "XF86HomePage",
        "eject" => "XF86Eject",
        "launch_mail" => "XF86Mail",
        "launch_app_1" => "XF86MyComputer",
        "launch_app_2" => "XF86Calculator",
        "media_play_pause" => "XF86AudioPlay",
        "media_stop" => "XF86AudioStop",
        "media_track_next" => "XF86AudioNext",
        "media_track_previous" => "XF86AudioPrev",
        "media_select" => "XF86AudioMedia",
        "power" => "XF86PowerOff",
        "sleep" => "XF86Sleep",
        "wake_up" => "XF86WakeUp",
        "audio_volume_down" => "XF86AudioLowerVolume",
        "audio_volume_mute" => "XF86AudioMute",
        "audio_volume_up" => "XF86AudioRaiseVolume",
        "copy" => "XF86Copy",
        "cut" => "XF86Cut",
        "paste" => "XF86Paste",
        _ => return None,
    };
    Some(x.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/ghostty/{name}",
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

    #[test]
    fn parses_1_4_0() {
        let (sheet, skipped) = parse(&fixture("list-keybinds-1.4.0.txt")).unwrap();
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str()),
            ("ghostty", "ghostty")
        );
        let rows = rows(&sheet);
        for want in [
            "ctrl+shift+c = copy_to_clipboard:mixed",
            "ctrl+shift+v = paste_from_clipboard",
            "super+ctrl+alt+shift+a = select_all",
            "ctrl+1 = goto_tab:1",
            "ctrl+shift+Up = jump_to_prompt:-1",
            "shift+Prior = scroll_page_up",
            "ctrl+bracketleft = previous_tab",
            "ctrl+plus = increase_font_size:1",
            "ctrl+equal = increase_font_size:1",
            "ctrl+equal = decrease_font_size:1",
            "ctrl+shift+o = new_split:right",
            "ctrl+a n = new_window",
            "ctrl+a c = new_tab",
            "alt+Left = goto_split:left, then toggle_fullscreen",
            "ctrl+q = quit",
            "F11 = toggle_fullscreen",
            "KP_Add = increase_font_size:2",
            "ctrl+slash = text:\\x15",
            "h = resize_split:left,10 [key_table=resize]",
            "Escape = deactivate_key_table [key_table=resize]",
            "ctrl+a l = resize_split:right,10 [key_table=resize]",
        ] {
            assert!(has(&rows, want), "{want} not in {rows:#?}");
        }
        // catch_all, `ö`, `intl_ro` (no keysym) and the table name with a space.
        assert_eq!(skipped, 4);
        assert_eq!(rows.len(), 21, "{rows:#?}");
        // Grouped by action; the action's name is the bind's action.
        let split = &sheet.binds["new_split"][0];
        assert_eq!(split.action, "new_split");
        assert!(sheet.binds["new_window"][0].when.is_empty());
    }

    #[test]
    fn parses_1_1_3() {
        let (sheet, skipped) = parse(&fixture("list-keybinds-1.1.3.txt")).unwrap();
        let rows = rows(&sheet);
        assert_eq!(
            rows.len(),
            9,
            "every name in the fixture has a keysym: {rows:#?}"
        );
        for want in [
            "ctrl+shift+c = copy_to_clipboard",
            "super+a = select_all",
            "ctrl+shift+Up = jump_to_prompt:-1",
            "ctrl+bracketleft = previous_tab",
            "ctrl+0 = reset_font_size",
            "alt+1 = goto_tab:1",
            "ctrl+shift+grave = toggle_quick_terminal",
            "shift+Next = scroll_page_down",
            "KP_Enter = ignore",
        ] {
            assert!(has(&rows, want), "{want} not in {rows:#?}");
        }
        assert_eq!(skipped, 0);
    }

    #[test]
    fn chains() {
        // Real shapes: after a chained bind in a sequence, ghostty prints its siblings over the start of the chain line
        // (a 6-byte `ctrl+a` prefix keeps `chain=`, a 1-byte `g` keeps `c`, a 12-byte one keeps the whole line and
        // then the rest of its own prefix).
        let text = "keybind = ctrl+a>n=new_window\nkeybind = chain=toggle_fullscreen\nkeybind = chain=quit\n\
                    keybind = chain=>c=new_tab\nkeybind = chain=>d=x\n\
                    keybind = g>t=new_tab\nkeybind = chain=toggle_fullscreen\nkeybind = c>w=new_window\n\
                    keybind = ctrl+shift+a>x=y\nkeybind = chain=quit\nkeybind = chain=quit+a>z=w\n\
                    keybind = ctrl+b=esc:a\nkeybind = chain=csi:>4\n\
                    keybind = t/k=a\nkeybind = chain=b\nkeybind = ctrl+a=text:x \n";
        let (sheet, skipped) = parse(text).unwrap();
        assert_eq!(
            rows(&sheet),
            [
                "k = a, then b [key_table=t]",
                // A chained action holding `>` is still a chain.
                "ctrl+b = esc:a, then csi:>4",
                "g t = new_tab, then toggle_fullscreen",
                "ctrl+a n = new_window, then toggle_fullscreen, then quit",
                // Only the start of a line is trimmed.
                "ctrl+a = text:x ",
                "ctrl+shift+a x = y, then quit",
            ]
        );
        // The four garbled lines.
        assert_eq!(skipped, 4);
    }

    #[test]
    fn version_from_ghostty_version() {
        let out =
            "Ghostty 1.4.0\n\nVersion\n  - version: 1.4.0\n  - channel: stable\nBuild Config\n";
        assert_eq!(version_of(out), Some((1, 4)));
        assert_eq!(version_of("  - version: 1.3.0-dev+abc\n"), Some((1, 3)));
        assert_eq!(version_of("Ghostty 1.1.3\n"), None);
    }

    #[test]
    fn odd_lines() {
        // No binds at all (`keybind = `), other config lines, a chain after a left-out bind, an unreadable trigger.
        let (sheet, skipped) =
            parse("keybind = \nfont-size = 12\nkeybind = catch_all=ignore\nkeybind = chain=quit\nkeybind = ctrl+\n")
                .unwrap();
        assert!(sheet.binds.is_empty());
        assert_eq!(skipped, 2);
        // Uppercase letters are the key, lowercased; a `>` key and a `/` key.
        let (sheet, _) =
            parse("keybind = ctrl+C=copy\nkeybind = ctrl+>=x\nkeybind = /=y\n").unwrap();
        assert_eq!(
            rows(&sheet),
            ["ctrl+c = copy", "ctrl+greater = x", "slash = y"]
        );
    }

    fn command_args(env: Env, version: &dyn Fn() -> Option<(u32, u32)>) -> Command {
        command(env, version).0
    }

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn default_keys_when_there_is_no_config_file() {
        let dir = std::env::temp_dir().join(format!("kbf-ghostty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ghostty")).unwrap();
        let config = dir.to_string_lossy().into_owned();
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(config.clone()),
            _ => None,
        };
        let v14 = || Some((1, 4));
        // No config file: nothing to decide, the version isn't asked.
        let none = args(&command_args(&env, &|| panic!("version asked")));
        std::fs::write(dir.join("ghostty/config"), "").unwrap();
        // `config` is read by every ghostty: the version isn't asked.
        let legacy = args(&command_args(&env, &|| panic!("version asked")));
        std::fs::remove_file(dir.join("ghostty/config")).unwrap();
        std::fs::write(dir.join("ghostty/config.ghostty"), "").unwrap();
        let current = args(&command_args(&env, &v14));
        // Before 1.3 ghostty doesn't read `config.ghostty` (and would write a template); an unknown version is as safe.
        let old = args(&command_args(&env, &|| Some((1, 2))));
        let at_1_3 = args(&command_args(&env, &|| Some((1, 3))));
        let (unknown, note) = command(&env, &|| None);
        let unknown = args(&unknown);
        // An empty or relative XDG_CONFIG_HOME counts as unset: HOME's .config, passed on to ghostty.
        let home = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some("relative".to_string()),
            "HOME" => Some(config.clone()),
            _ => None,
        };
        let (cmd, _) = command(&home, &v14);
        let empty = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(String::new()),
            "HOME" => Some(config.clone()),
            _ => None,
        };
        let (empty, _) = command(&empty, &v14);
        // HOME's .config holding a config: ghostty's own keys.
        std::fs::create_dir_all(dir.join(".config/ghostty")).unwrap();
        std::fs::write(dir.join(".config/ghostty/config"), "").unwrap();
        let home_config = args(&command_args(&home, &|| panic!("version asked")));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(home_config, ["+list-keybinds", "--plain"]);
        assert_eq!(none, ["+list-keybinds", "--default", "--plain"]);
        assert_eq!(legacy, ["+list-keybinds", "--plain"]);
        assert_eq!(current, ["+list-keybinds", "--plain"]);
        assert_eq!(old, ["+list-keybinds", "--default", "--plain"]);
        assert_eq!(at_1_3, ["+list-keybinds", "--plain"]);
        assert_eq!(unknown, ["+list-keybinds", "--default", "--plain"]);
        assert!(note.is_some_and(|n| n.contains("ghostty --version")));
        assert!(command(&env, &v14).1.is_none());
        let xdg_of = |cmd: &Command| {
            cmd.get_envs()
                .find(|(k, _)| *k == "XDG_CONFIG_HOME")
                .and_then(|(_, v)| v)
                .map(|v| v.to_string_lossy().into_owned())
        };
        assert_eq!(args(&empty), args(&cmd));
        assert_eq!(xdg_of(&empty), Some(format!("{config}/.config")));
        let xdg = cmd
            .get_envs()
            .find(|(k, _)| *k == "XDG_CONFIG_HOME")
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert_eq!(xdg, Some(format!("{config}/.config")));
        assert_eq!(args(&cmd), ["+list-keybinds", "--default", "--plain"]);
        // No HOME either: the defaults.
        assert_eq!(
            args(&command_args(&|_| None, &v14)),
            ["+list-keybinds", "--default", "--plain"]
        );
        // The display is never passed on, and it runs from `/`.
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for k in ["DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET"] {
            assert!(removed.iter().any(|r| r == k), "{k}");
        }
        assert_eq!(cmd.get_current_dir(), Some(std::path::Path::new("/")));
        let mise = cmd
            .get_envs()
            .find(|(k, _)| *k == "MISE_NOT_FOUND_AUTO_INSTALL")
            .and_then(|(_, v)| v);
        assert_eq!(mise, Some(std::ffi::OsStr::new("0")));
    }

    #[test]
    fn missing_ghostty_is_an_error_naming_it() {
        let (mut cmd, _) = command(&|_| None, &|| None);
        cmd.env("PATH", "/nonexistent");
        let e = read(cmd).unwrap_err();
        // Not found, and said so with ghostty's name.
        let io = e.root_cause().downcast_ref::<std::io::Error>();
        assert_eq!(
            io.map(|e| e.kind()),
            Some(std::io::ErrorKind::NotFound),
            "{e:#}"
        );
        assert!(format!("{e:#}").contains("running ghostty"), "{e:#}");
    }

    #[test]
    fn real_ghostty_defaults_create_no_file() {
        let have = Command::new("ghostty")
            .arg("--version")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !have {
            eprintln!("skipping: ghostty is not installed");
            return;
        }
        let dir = std::env::temp_dir().join(format!("kbf-ghostty-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let home = dir.to_string_lossy().into_owned();
        let env = |k: &str| match k {
            "HOME" => Some(home.clone()),
            "XDG_CONFIG_HOME" => Some(format!("{home}/config")),
            _ => None,
        };
        // The real `--version` output reads.
        let v = version();
        assert!(v.is_some(), "ghostty --version didn't read");
        // Every XDG directory in the temp dir, so a file ghostty writes anywhere is seen.
        let run = |env: Env| {
            let (mut cmd, _) = command(env, &version);
            cmd.env("HOME", &dir);
            for (k, sub) in [
                ("XDG_STATE_HOME", "state"),
                ("XDG_CACHE_HOME", "cache"),
                ("XDG_DATA_HOME", "data"),
            ] {
                cmd.env(k, dir.join(sub));
            }
            read(cmd)
        };
        let defaults = run(&env);
        let after_defaults = walk(&dir);
        // With a config file: ghostty reads it (the directory passed on), and still writes nothing.
        std::fs::create_dir_all(dir.join("config/ghostty")).unwrap();
        // `global:` binds are listed, without the prefix.
        let config = "keybind = ctrl+shift+f12=quit\nkeybind = global:ctrl+shift+f11=quit\n";
        std::fs::write(dir.join("config/ghostty/config"), config).unwrap();
        let configured = run(&env);
        let after_config = walk(&dir);
        // From 1.3, `config.ghostty` alone is read too.
        std::fs::remove_file(dir.join("config/ghostty/config")).unwrap();
        std::fs::write(dir.join("config/ghostty/config.ghostty"), config).unwrap();
        let current = v.is_some_and(|v| v >= (1, 3)).then(|| run(&env));
        let after_current = walk(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!defaults.unwrap().binds.is_empty());
        assert_eq!(after_defaults, [dir.join("config")], "ghostty wrote files");
        let configured = configured.unwrap();
        assert!(
            configured
                .binds
                .values()
                .flatten()
                .any(|b| b.key == "ctrl+shift+F12" && b.desc == "quit"),
            "the config's bind is missing"
        );
        assert!(
            configured
                .binds
                .values()
                .flatten()
                .any(|b| b.key == "ctrl+shift+F11"),
            "the global: bind is missing"
        );
        if let Some(current) = current {
            assert!(
                current
                    .unwrap()
                    .binds
                    .values()
                    .flatten()
                    .any(|b| b.key == "ctrl+shift+F12"),
                "config.ghostty's bind is missing"
            );
        }
        assert_eq!(
            after_current,
            [
                dir.join("config"),
                dir.join("config/ghostty"),
                dir.join("config/ghostty/config.ghostty")
            ],
            "ghostty wrote files"
        );
        assert_eq!(
            after_config,
            [
                dir.join("config"),
                dir.join("config/ghostty"),
                dir.join("config/ghostty/config")
            ],
            "ghostty wrote files"
        );
    }

    /// Every path under `dir`.
    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            out.push(e.path());
            if e.path().is_dir() {
                out.extend(walk(&e.path()));
            }
        }
        out
    }
}
