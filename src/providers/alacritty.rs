//! alacritty: the user's `[[keyboard.bindings]]` merged over alacritty 0.17.0's defaults (`alacritty_defaults.rs`),
//! by alacritty's own rules (`deserialize_bindings` in `config/ui_config.rs`, `Binding::triggers_match` in
//! `config/bindings.rs`): each user binding removes every default it overlaps, then the user's come first.

use super::alacritty_defaults::DEFAULTS;
use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use std::path::PathBuf;
use toml::{Table, Value};

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    sheet_from(&|name: &str| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

/// No config file, or one alacritty can't read: alacritty runs on its defaults, and the sheet shows them.
fn sheet_from(env: Env) -> anyhow::Result<Sheet> {
    let paths = config_paths(env);
    let mut notes = Vec::new();
    let text = match paths.iter().find(|p| p.is_file()) {
        Some(path) => std::fs::read_to_string(path).unwrap_or_else(|e| {
            notes.push(format!(
                "can't read {} ({e}); showing alacritty's defaults",
                path.display()
            ));
            String::new()
        }),
        None => {
            // alacritty 0.17 still reads an old alacritty.yml when there is no .toml; this sheet doesn't.
            if let Some(yml) = paths
                .iter()
                .map(|p| p.with_extension("yml"))
                .find(|p| p.is_file())
            {
                notes.push(format!(
                    "{} is not read (only alacritty.toml is); showing alacritty's defaults",
                    yml.display()
                ));
            }
            String::new()
        }
    };
    // These notes only come with an empty text, which parses without notes: still one line.
    print_notes(&notes);
    parse(&text)
}

fn print_notes(notes: &[String]) {
    if !notes.is_empty() {
        eprintln!("keybind-find: alacritty: {}", notes.join("; "));
    }
}

/// alacritty's search order (`installed_config` in its `config/mod.rs`, via the `xdg` crate): `alacritty/` under
/// `$XDG_CONFIG_HOME` then `$XDG_CONFIG_DIRS`, the same two without `alacritty/`, then `~/.config/alacritty/`,
/// `~/.alacritty.toml`, `/etc/alacritty/`.
// ponytail: `general.import` files are not read; add them if bindings ever move into one.
fn config_paths(env: Env) -> Vec<PathBuf> {
    let home = env("HOME").map(PathBuf::from);
    let xdg_home = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let xdg_dirs: Vec<PathBuf> = env("XDG_CONFIG_DIRS")
        .unwrap_or_else(|| "/etc/xdg".into())
        .split(':')
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .collect();
    let mut paths = Vec::new();
    for sub in ["alacritty/alacritty.toml", "alacritty.toml"] {
        paths.extend(xdg_home.iter().chain(&xdg_dirs).map(|d| d.join(sub)));
    }
    if let Some(h) = &home {
        paths.push(h.join(".config/alacritty/alacritty.toml"));
        paths.push(h.join(".alacritty.toml"));
    }
    paths.push(PathBuf::from("/etc/alacritty/alacritty.toml"));
    paths
}

/// One binding: its trigger (key as an X keysym, mods in the notation, the modes it needs and the ones it
/// excludes) and what it does.
struct Binding {
    keysym: String,
    key: Key,
    mods: Vec<&'static str>,
    mode: Vec<&'static str>,
    notmode: Vec<&'static str>,
    desc: String,
    /// False for an action alacritty doesn't know (shown, but alacritty skips the binding).
    known: bool,
}

impl Binding {
    /// alacritty's `triggers_match`: same key and mods, modes that can be active together (no modes = any), and
    /// neither needs a mode the other excludes.
    fn overlaps(&self, other: &Binding) -> bool {
        let any_shared = |a: &[&str], b: &[&str]| a.iter().any(|m| b.contains(m));
        self.key.overlaps(&other.key)
            && self.mods == other.mods
            && (self.mode.is_empty()
                || other.mode.is_empty()
                || any_shared(&self.mode, &other.mode))
            && !any_shared(&self.mode, &other.notmode)
            && !any_shared(&other.mode, &self.notmode)
    }

    /// Bindings that need a mode go in a category named after it (`Vi`, `Search`, `Vi|Search`); the rest in
    /// `alacritty`.
    fn category(&self) -> String {
        if self.mode.is_empty() {
            "alacritty".into()
        } else {
            self.mode.join("|")
        }
    }
}

/// Actions that do nothing visible: as a user binding they unbind the defaults they overlap.
const UNBIND: &[&str] = &["None", "ReceiveChar"];

/// The keyboard actions alacritty 0.17.0 accepts by name (`Action` without its `#[config(skip)]` variants, then
/// `ViAction`, `ViMotion` and `SearchAction`), matched ignoring case as alacritty does.
const ACTIONS: &[&str] = &[
    "Paste",
    "Copy",
    "CopySelection",
    "PasteSelection",
    "IncreaseFontSize",
    "DecreaseFontSize",
    "ResetFontSize",
    "ScrollPageUp",
    "ScrollPageDown",
    "ScrollHalfPageUp",
    "ScrollHalfPageDown",
    "ScrollLineUp",
    "ScrollLineDown",
    "ScrollToTop",
    "ScrollToBottom",
    "ClearHistory",
    "Hide",
    "HideOtherApplications",
    "Minimize",
    "Quit",
    "ClearLogNotice",
    "SpawnNewInstance",
    "SelectNextTab",
    "SelectPreviousTab",
    "SelectTab1",
    "SelectTab2",
    "SelectTab3",
    "SelectTab4",
    "SelectTab5",
    "SelectTab6",
    "SelectTab7",
    "SelectTab8",
    "SelectTab9",
    "SelectLastTab",
    "CreateNewWindow",
    "CreateNewTab",
    "ToggleFullscreen",
    "ToggleMaximized",
    "ToggleSimpleFullscreen",
    "ClearSelection",
    "ToggleViMode",
    "ReceiveChar",
    "SearchForward",
    "SearchBackward",
    "None",
    // ViAction
    "ToggleNormalSelection",
    "ToggleLineSelection",
    "ToggleBlockSelection",
    "ToggleSemanticSelection",
    "SearchNext",
    "SearchPrevious",
    "SearchStart",
    "SearchEnd",
    "Open",
    "CenterAroundViCursor",
    "InlineSearchForward",
    "InlineSearchBackward",
    "InlineSearchForwardShort",
    "InlineSearchBackwardShort",
    "InlineSearchNext",
    "InlineSearchPrevious",
    "SemanticSearchForward",
    "SemanticSearchBackward",
    // ViMotion
    "Up",
    "Down",
    "Left",
    "Right",
    "First",
    "Last",
    "FirstOccupied",
    "High",
    "Middle",
    "Low",
    "SemanticLeft",
    "SemanticRight",
    "SemanticLeftEnd",
    "SemanticRightEnd",
    "WordLeft",
    "WordRight",
    "WordLeftEnd",
    "WordRightEnd",
    "Bracket",
    "ParagraphUp",
    "ParagraphDown",
    // SearchAction
    "SearchFocusNext",
    "SearchFocusPrevious",
    "SearchConfirm",
    "SearchCancel",
    "SearchClear",
    "SearchDeleteWord",
    "SearchHistoryPrevious",
    "SearchHistoryNext",
];

/// The notes go to stderr as one line.
fn parse(toml_text: &str) -> anyhow::Result<Sheet> {
    let (sheet, notes) = parse_reporting(toml_text)?;
    print_notes(&notes);
    Ok(sheet)
}

/// The sheet, plus a note for each user binding that isn't shown. A config alacritty can't read leaves it on its
/// defaults, and the sheet too.
fn parse_reporting(toml_text: &str) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut notes = Vec::new();
    let config: Table = toml_text.parse().unwrap_or_else(|e: toml::de::Error| {
        let e = e.to_string();
        let first = e.lines().next().unwrap_or_default();
        notes.push(format!(
            "alacritty.toml doesn't parse ({first}); showing alacritty's defaults"
        ));
        Table::new()
    });
    let keyboard = config.get("keyboard");
    if keyboard.is_some_and(|k| !k.is_table()) {
        notes.push("keyboard is not a table; showing alacritty's defaults".into());
    }
    let user_values = match keyboard.and_then(|k| k.get("bindings")) {
        None => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => {
            notes.push("keyboard.bindings is not an array; showing alacritty's defaults".into());
            Vec::new()
        }
    };
    let mut user = Vec::new();
    for (i, value) in user_values.iter().enumerate() {
        let scancode = value.as_table().and_then(|t| {
            t.iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("key"))
                .and_then(|(_, v)| v.as_integer())
        });
        if let Some(code) = scancode {
            notes.push(format!(
                "keyboard.bindings[{i}]: scancode key {code} not shown"
            ));
            continue;
        }
        match user_binding(value) {
            Ok(b) => user.push(b),
            Err(e) => notes.push(format!("keyboard.bindings[{i}]: {e}; alacritty skips it")),
        }
    }
    let mut defaults = Vec::new();
    for &(key, mods, mode, action) in DEFAULTS {
        let opt = |s: &'static str| (!s.is_empty()).then_some(s);
        defaults.push(
            binding(key, opt(mods), opt(mode), action.to_string(), true).with_context(|| {
                format!("built-in default {key:?} {mods:?} {mode:?} {action:?}")
            })?,
        );
    }
    // A binding whose action alacritty doesn't know is skipped by alacritty, so it unbinds nothing.
    defaults.retain(|d| !user.iter().any(|u| u.known && u.overlaps(d)));

    // Several actions on one trigger run in order (`y` in vi mode copies, then clears): one entry listing them.
    let mut sheet = Sheet {
        title: "alacritty".into(),
        provider: "alacritty".into(),
        binds: Default::default(),
    };
    // Only bindings active in exactly the same modes share an entry: (category, chord, excluded modes) → index.
    let mut entries: Vec<(String, String, Vec<&str>, usize)> = Vec::new();
    for b in user.into_iter().chain(defaults) {
        if UNBIND.contains(&b.desc.as_str()) {
            continue;
        }
        let key = chord(&b.mods, &b.keysym);
        let category = b.category();
        let binds = sheet.binds.entry(category.clone()).or_default();
        let same = entries
            .iter()
            .find(|(c, k, n, _)| *c == category && *k == key && *n == b.notmode)
            .map(|e| e.3);
        match same.map(|i| &mut binds[i]) {
            Some(x) => {
                x.desc = format!("{}, {}", x.desc, b.desc);
                x.action = x.desc.clone();
            }
            None => {
                entries.push((category, key.clone(), b.notmode.clone(), binds.len()));
                binds.push(Bind {
                    key,
                    desc: b.desc.clone(),
                    action: b.desc,
                    when: Default::default(),
                });
            }
        }
    }
    Ok((sheet, notes))
}

/// A `[[keyboard.bindings]]` entry: `key`, optional `mods` and `mode`, and exactly one of `action`, `chars` and
/// `command` (a string, or `{ program, args }`).
fn user_binding(value: &Value) -> anyhow::Result<Binding> {
    // alacritty matches field names ignoring case, and refuses unknown or repeated ones.
    let mut t = Table::new();
    for (name, v) in value.as_table().context("not a table")? {
        let lower = name.to_ascii_lowercase();
        if !["key", "mods", "mode", "action", "chars", "command"].contains(&lower.as_str()) {
            anyhow::bail!("unknown field {name:?}");
        }
        if t.insert(lower, v.clone()).is_some() {
            anyhow::bail!("field {name:?} given twice");
        }
    }
    let text = |field: &str| -> anyhow::Result<Option<&str>> {
        match t.get(field) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => anyhow::bail!("{field} is not a string"),
        }
    };
    let key = text("key")?.context("no key")?;
    let mut known = true;
    let desc = match (text("action")?, text("chars")?, t.get("command")) {
        (Some(action), None, None) => match ACTIONS.iter().find(|a| a.eq_ignore_ascii_case(action))
        {
            Some(a) => a.to_string(),
            // The story shows an unknown action verbatim; it says so, since alacritty skips the binding.
            None => {
                known = false;
                let why = if action.eq_ignore_ascii_case("ExpandSelection") {
                    "a mouse-only action"
                } else {
                    "not an alacritty 0.17.0 action"
                };
                format!("{action} ({why}; alacritty skips it)")
            }
        },
        (None, Some(chars), None) => format!("chars: {}", escape(chars)),
        (None, None, Some(command)) => format!("command: {}", program(command)?),
        _ => anyhow::bail!("needs exactly one of action, chars and command"),
    };
    let mut b = binding(key, text("mods")?, text("mode")?, desc, false)?;
    b.known = known;
    Ok(b)
}

/// `"htop"` or `{ program = "htop", args = ["-d", "5"] }` → `htop -d 5`.
fn program(command: &Value) -> anyhow::Result<String> {
    match command {
        Value::String(p) => Ok(p.clone()),
        Value::Table(t) => {
            let mut words = vec![t
                .get("program")
                .and_then(Value::as_str)
                .context("command has no program")?
                .to_string()];
            let args = match t.get("args") {
                None => &Vec::new(),
                Some(Value::Array(a)) => a,
                Some(_) => anyhow::bail!("command args is not an array"),
            };
            for arg in args {
                words.push(
                    arg.as_str()
                        .context("command args must be strings")?
                        .to_string(),
                );
            }
            Ok(words.join(" "))
        }
        _ => anyhow::bail!("command is neither a string nor a table"),
    }
}

/// `default`: a row of the built-in table, whose named keys match anywhere on the keyboard (alacritty's
/// `KeyLocation::Any`).
fn binding(
    key: &str,
    mods: Option<&str>,
    mode: Option<&str>,
    desc: String,
    default: bool,
) -> anyhow::Result<Binding> {
    let (keysym, mut location) = keysym(key).with_context(|| format!("unknown key {key:?}"))?;
    if default && location == Location::Standard {
        location = Location::Any;
    }
    let mut b = Binding {
        key: Key {
            base: base(&keysym),
            location,
        },
        keysym,
        mods: Vec::new(),
        mode: Vec::new(),
        notmode: Vec::new(),
        desc,
        known: true,
    };
    for m in split(mods)? {
        let m = match m.to_lowercase().as_str() {
            "command" | "super" => "super",
            "shift" => "shift",
            "alt" | "option" => "alt",
            "control" => "ctrl",
            "none" => continue,
            _ => anyhow::bail!("unknown modifier {m:?}"),
        };
        if !b.mods.contains(&m) {
            b.mods.push(m);
        }
    }
    b.mods.sort_by_key(|m| {
        ["super", "ctrl", "alt", "shift"]
            .iter()
            .position(|o| o == m)
    });
    for m in split(mode)? {
        let lower = m.to_lowercase();
        let (set, name) = match lower.strip_prefix('~') {
            Some(n) => (&mut b.notmode, n),
            None => (&mut b.mode, lower.as_str()),
        };
        let name = MODES
            .iter()
            .find(|(n, _)| *n == name)
            .with_context(|| format!("unknown mode {m:?}"))?
            .1;
        if !set.contains(&name) {
            set.push(name);
        }
    }
    b.mode
        .sort_by_key(|m| MODES.iter().position(|(_, n)| n == m));
    Ok(b)
}

/// alacritty's mode names (lowercase, as it matches them) and how a category shows them, in category order.
const MODES: &[(&str, &str)] = &[
    ("vi", "Vi"),
    ("search", "Search"),
    ("alt", "Alt"),
    ("appcursor", "AppCursor"),
    ("appkeypad", "AppKeypad"),
];

/// `Control | Shift` → `["Control", "Shift"]`; absent → none. An empty part (`""`, `Vi|`) is an error, as in
/// alacritty.
fn split(s: Option<&str>) -> anyhow::Result<Vec<&str>> {
    let parts: Vec<&str> = s
        .into_iter()
        .flat_map(|s| s.split('|'))
        .map(str::trim)
        .collect();
    if parts.contains(&"") {
        anyhow::bail!("empty name in {:?}", s.unwrap_or_default());
    }
    Ok(parts)
}

/// Where on the keyboard a key is (alacritty's `KeyLocation`): `Any` matches both others.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Location {
    Any,
    Standard,
    Numpad,
}

/// A key as alacritty compares triggers: the key ignoring the keypad (`KP_Add` → `plus`), and where it is.
struct Key {
    base: String,
    location: Location,
}

impl Key {
    fn overlaps(&self, other: &Key) -> bool {
        self.base == other.base
            && (self.location == other.location
                || self.location == Location::Any
                || other.location == Location::Any)
    }
}

/// The keypad keysym's main-keyboard twin, for matching: `KP_7` → `7`, `KP_Add` → `plus`.
fn base(keysym: &str) -> String {
    let twin = match keysym {
        "KP_Enter" => "Return",
        "KP_Add" => "plus",
        "KP_Separator" => "comma",
        "KP_Decimal" => "period",
        "KP_Divide" => "slash",
        "KP_Equal" => "equal",
        "KP_Subtract" => "minus",
        "KP_Multiply" => "asterisk",
        k => k.strip_prefix("KP_").unwrap_or(k),
    };
    twin.to_string()
}

/// An alacritty key name as an X keysym. A single character is lowercased, as alacritty matches it (`"N"` is n;
/// shift comes from `mods`); punctuation goes by its keysym name. Multi-character names are winit's `NamedKey`s
/// (mostly X's names already) or alacritty's legacy names.
/// The location follows alacritty: characters anywhere, its legacy main-keyboard names (`Return`, `Plus`, `Key1`)
/// standard, `Numpad*` on the keypad, other named keys anywhere.
fn keysym(key: &str) -> Option<(String, Location)> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        // alacritty takes `" "` or `"\t"` as a character no key press ever produces (Space and Tab are named keys).
        if c.is_whitespace() || c.is_control() {
            return None;
        }
        let lower: String = c.to_lowercase().collect();
        let mut one = lower.chars();
        let sym = match (one.next(), one.next()) {
            (Some(l), None) => punctuation(l).map_or(lower.clone(), str::to_string),
            _ => lower,
        };
        return Some((sym, Location::Any));
    }
    let standard = [
        "Enter", "Return", "Plus", "Comma", "Slash", "Equals", "Minus", "Asterisk",
    ];
    let location = if standard.contains(&key) || digit(key, "Key").is_some() {
        Location::Standard
    } else if key.starts_with("Numpad") {
        Location::Numpad
    } else {
        Location::Any
    };
    let named = match key {
        "ArrowUp" | "Up" => "Up",
        "ArrowDown" | "Down" => "Down",
        "ArrowLeft" | "Left" => "Left",
        "ArrowRight" | "Right" => "Right",
        "Enter" | "Return" => "Return",
        "PageUp" => "Prior",
        "PageDown" => "Next",
        "Space" => "space",
        "Backspace" | "Back" => "BackSpace",
        "Copy" => "XF86Copy",
        "Paste" => "XF86Paste",
        "Cut" => "XF86Cut",
        "At" => "at",
        "Colon" => "colon",
        "Period" => "period",
        "LBracket" => "bracketleft",
        "RBracket" => "bracketright",
        "Semicolon" => "semicolon",
        "Backslash" => "backslash",
        "Plus" => "plus",
        "Comma" => "comma",
        "Slash" => "slash",
        "Equals" => "equal",
        "Minus" => "minus",
        "Asterisk" => "asterisk",
        "NumpadEnter" => "KP_Enter",
        "NumpadAdd" => "KP_Add",
        "NumpadComma" => "KP_Separator",
        "NumpadDecimal" => "KP_Decimal",
        "NumpadDivide" => "KP_Divide",
        "NumpadEquals" => "KP_Equal",
        "NumpadSubtract" => "KP_Subtract",
        "NumpadMultiply" => "KP_Multiply",
        "CapsLock" => "Caps_Lock",
        "NumLock" => "Num_Lock",
        "ScrollLock" => "Scroll_Lock",
        "PrintScreen" => "Print",
        "ContextMenu" => "Menu",
        _ => {
            if let Some(d) = digit(key, "Key") {
                return Some((d.to_string(), location));
            }
            if let Some(d) = digit(key, "Numpad") {
                return Some((format!("KP_{d}"), location));
            }
            // Home, End, Insert, Delete, Tab, Escape, F1..F35, ...: X uses winit's names. winit's names start
            // with a capital, so `escape` is a typo alacritty refuses.
            // ponytail: other typos (`Escpae`) pass; check against winit's NamedKey list if that bites.
            let named = key.starts_with(|c: char| c.is_ascii_uppercase())
                && key.chars().all(|c| c.is_ascii_alphanumeric());
            return named.then(|| (key.to_string(), location));
        }
    };
    Some((named.to_string(), location))
}

/// `Key7` with prefix `Key` → `'7'`.
fn digit(key: &str, prefix: &str) -> Option<char> {
    let rest = key.strip_prefix(prefix)?;
    let mut c = rest.chars();
    match (c.next(), c.next()) {
        (Some(d), None) if d.is_ascii_digit() => Some(d),
        _ => None,
    }
}

// ponytail: a third copy of this table (xmonad.rs, nvim.rs have one each); move it to sheet.rs when a story owns all three.
fn punctuation(c: char) -> Option<&'static str> {
    Some(match c {
        ' ' => "space",
        '!' => "exclam",
        '"' => "quotedbl",
        '#' => "numbersign",
        '$' => "dollar",
        '%' => "percent",
        '&' => "ampersand",
        '\'' => "apostrophe",
        '(' => "parenleft",
        ')' => "parenright",
        '*' => "asterisk",
        '+' => "plus",
        ',' => "comma",
        '-' => "minus",
        '.' => "period",
        '/' => "slash",
        ':' => "colon",
        ';' => "semicolon",
        '<' => "less",
        '=' => "equal",
        '>' => "greater",
        '?' => "question",
        '@' => "at",
        '[' => "bracketleft",
        '\\' => "backslash",
        ']' => "bracketright",
        '^' => "asciicircum",
        '_' => "underscore",
        '`' => "grave",
        '{' => "braceleft",
        '|' => "bar",
        '}' => "braceright",
        '~' => "asciitilde",
        _ => return None,
    })
}

/// `"\u001B\r"` → `\e\r`: control characters as escapes, everything else as it is.
fn escape(chars: &str) -> String {
    chars
        .chars()
        .map(|c| match c {
            '\x1b' => "\\e".to_string(),
            '\r' => "\\r".to_string(),
            '\n' => "\\n".to_string(),
            '\t' => "\\t".to_string(),
            '\\' => "\\\\".to_string(),
            c if c.is_control() => format!("\\x{:02x}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/alacritty.toml"
    ));

    fn desc<'a>(sheet: &'a Sheet, category: &str, key: &str) -> Option<&'a str> {
        sheet
            .binds
            .get(category)?
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.desc.as_str())
    }

    #[test]
    fn fixture_adds_overrides_and_unbinds() {
        let (sheet, notes) = parse_reporting(FIXTURE).unwrap();
        assert_eq!(notes, Vec::<String>::new());
        // Added: "N" with Control|Alt.
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+alt+n"),
            Some("SpawnNewInstance")
        );
        // Overridden: the default ctrl+shift+c is Copy.
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+shift+c"),
            Some("ToggleFullscreen")
        );
        // Unbound: ctrl+shift+f (SearchForward) is gone, and nothing replaces it.
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+f"), None);
        // chars, escaped (Control+Return).
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+Return"),
            Some("chars: \\e\\r")
        );
        // A Vi-mode binding goes in category Vi (`x`, new); one replaces the default ctrl+y there.
        assert_eq!(desc(&sheet, "Vi", "x"), Some("ClearSelection"));
        assert_eq!(desc(&sheet, "Vi", "ctrl+y"), Some("Copy"));
        // command, both forms.
        assert_eq!(
            desc(&sheet, "alacritty", "super+t"),
            Some("command: alacritty")
        );
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+alt+h"),
            Some("command: alacritty -e htop -d 5")
        );
        // An unknown action name is shown as it is, flagged; `mods` with spaces around `|`.
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+shift+F12"),
            Some("FrobnicateWidgets (not an alacritty 0.17.0 action; alacritty skips it)")
        );
        // Action names match ignoring case and show alacritty's spelling.
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+shift+F11"),
            Some("ToggleViMode")
        );
        // Defaults on the same keys with other mods are still there.
        assert_eq!(desc(&sheet, "Vi", "ctrl+f"), Some("ScrollPageDown"));
        assert_eq!(desc(&sheet, "Vi", "shift+f"), Some("InlineSearchBackward"));
        assert_eq!(desc(&sheet, "Search", "ctrl+n"), Some("SearchHistoryNext"));
        assert_eq!(desc(&sheet, "Vi", "n"), Some("SearchNext"));
        assert_eq!(desc(&sheet, "Vi", "ctrl+c"), Some("ToggleViMode"));
        // The user's come before the defaults (the fixture's first two bindings lead the list).
        let first: Vec<&str> = sheet.binds["alacritty"][..2]
            .iter()
            .map(|b| b.key.as_str())
            .collect();
        assert_eq!(first, ["ctrl+alt+n", "ctrl+Return"]);
        // Untouched defaults are still there.
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+v"), Some("Paste"));
        assert_eq!(desc(&sheet, "Search", "Escape"), Some("SearchCancel"));
    }

    #[test]
    fn defaults_alone() {
        for text in ["", "[font]\nsize = 12\n", "[keyboard]\n"] {
            let sheet = parse(text).unwrap();
            let count: usize = sheet.binds.values().map(Vec::len).sum();
            // 97 rows; same-trigger rows share an entry (`y`/Vi: Copy, ClearSelection; ...) and ctrl+l's
            // ReceiveChar half is not shown.
            assert_eq!(count, 91);
            assert_eq!(
                desc(&sheet, "alacritty", "ctrl+shift+space"),
                Some("ToggleViMode")
            );
            assert_eq!(desc(&sheet, "alacritty", "ctrl+l"), Some("ClearLogNotice"));
            assert_eq!(
                desc(&sheet, "alacritty", "shift+Prior"),
                Some("ScrollPageUp")
            );
            assert_eq!(
                desc(&sheet, "alacritty", "ctrl+KP_Add"),
                Some("IncreaseFontSize")
            );
            assert_eq!(desc(&sheet, "Vi", "y"), Some("Copy, ClearSelection"));
            assert_eq!(desc(&sheet, "Vi", "shift+question"), Some("SearchBackward"));
            assert_eq!(desc(&sheet, "Vi", "comma"), Some("InlineSearchPrevious"));
            assert_eq!(desc(&sheet, "Vi|Search", "Return"), Some("SearchConfirm"));
            assert_eq!(desc(&sheet, "Search", "Return"), Some("SearchFocusNext"));
        }
    }

    #[test]
    fn every_default_parses() {
        for &(key, mods, mode, action) in DEFAULTS {
            let opt = |s: &'static str| (!s.is_empty()).then_some(s);
            binding(key, opt(mods), opt(mode), action.into(), true).unwrap();
            assert!(ACTIONS.contains(&action), "{action}");
        }
        assert_eq!(DEFAULTS.len(), 97);
    }

    #[test]
    fn unbinding_follows_alacritty_mode_overlap() {
        // No mode overlaps every mode: ctrl+shift+space is ToggleViMode (~Search) and ScrollToBottom (Vi|~Search).
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"Space\"\nmods = \"Control|Shift\"\naction = \"None\"\n",
        )
        .unwrap();
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+space"), None);
        assert_eq!(desc(&sheet, "Vi", "ctrl+shift+space"), None);
        // A Search binding leaves the ~Search defaults on the same key alone.
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"Space\"\nmods = \"Control|Shift\"\nmode = \"Search\"\naction = \"ReceiveChar\"\n",
        )
        .unwrap();
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+shift+space"),
            Some("ToggleViMode")
        );
        assert_eq!(
            desc(&sheet, "Vi", "ctrl+shift+space"),
            Some("ScrollToBottom")
        );
    }

    #[test]
    fn invalid_bindings_are_skipped_with_a_note() {
        let text = r#"
[[keyboard.bindings]]
key = "a"
mods = "Hyper"
action = "Copy"
[[keyboard.bindings]]
key = 38
action = "Copy"
[[keyboard.bindings]]
key = "b"
action = "Copy"
chars = "x"
[[keyboard.bindings]]
mods = "Control"
action = "Copy"
[[keyboard.bindings]]
key = "c"
mode = "Insert"
action = "Copy"
[[keyboard.bindings]]
key = "NoSuchKey!"
action = "Copy"
[[keyboard.bindings]]
key = "v"
mods = "Control|Shift|"
action = "None"
[[keyboard.bindings]]
key = "c"
mods = "Control|Shift"
acton = "None"
[[keyboard.bindings]]
key = "c"
mods = "Control|Shift"
command = { program = "x", args = "-e" }
[[keyboard.bindings]]
key = "escape"
action = "Copy"
[[keyboard.bindings]]
key = "e"
Key = "e"
action = "Copy"
[[keyboard.bindings]]
key = "e"
mods = 3
action = "Copy"
[[keyboard.bindings]]
key = "e"
command = { args = ["x"] }
[[keyboard.bindings]]
key = "e"
command = { program = "x", args = [1] }
[[keyboard.bindings]]
key = "e"
command = 7
[[keyboard.bindings]]
Key = "d"
MODS = "Alt"
Action = "paste"
"#;
        let (sheet, notes) = parse_reporting(text).unwrap();
        let want = [
            "[0]: unknown modifier \"Hyper\"",
            "[1]: scancode key 38 not shown",
            "[2]: needs exactly one",
            "[3]: no key",
            "[4]: unknown mode",
            "[5]: unknown key",
            "[6]: empty name in \"Control|Shift|\"",
            "[7]: unknown field \"acton\"",
            "[8]: command args is not an array",
            "[9]: unknown key \"escape\"",
            "[10]: field \"key\" given twice",
            "[11]: mods is not a string",
            "[12]: command has no program",
            "[13]: command args must be strings",
            "[14]: command is neither a string nor a table",
        ];
        assert_eq!(notes.len(), want.len(), "{notes:?}");
        for (note, w) in notes.iter().zip(want) {
            assert!(note.contains(w), "{note} lacks {w}");
        }
        // Field names ignore case; the skipped bindings left the ctrl+shift+v/c defaults alone.
        assert_eq!(desc(&sheet, "alacritty", "alt+d"), Some("Paste"));
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+v"), Some("Paste"));
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+c"), Some("Copy"));
    }

    #[test]
    fn unknown_actions_and_unbind_case() {
        // An unknown action is shown but, like alacritty, removes no default.
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"C\"\nmods = \"Control|Shift\"\naction = \"ToggleFulscreen\"\n",
        )
        .unwrap();
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+shift+c"),
            Some("ToggleFulscreen (not an alacritty 0.17.0 action; alacritty skips it), Copy")
        );
        // `none` unbinds like `None`, and isn't listed.
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"C\"\nmods = \"Control|Shift\"\naction = \"none\"\n",
        )
        .unwrap();
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+c"), None);
    }

    #[test]
    fn mode_overlap_both_ways() {
        let one = |key: &str, mode: &str| {
            parse(&format!(
                "[[keyboard.bindings]]\nkey = \"{key}\"\nmode = \"{mode}\"\naction = \"None\"\n"
            ))
            .unwrap()
        };
        // Disjoint required modes: an AppCursor binding leaves the Vi `y` alone.
        assert_eq!(
            desc(&one("y", "AppCursor"), "Vi", "y"),
            Some("Copy, ClearSelection")
        );
        // A binding that excludes Vi leaves Vi's Escape alone, but takes Search's.
        let sheet = one("Escape", "~Vi");
        assert_eq!(desc(&sheet, "Vi", "Escape"), Some("ClearSelection"));
        assert_eq!(desc(&sheet, "Search", "Escape"), None);
        // A Vi binding takes a default that has no mode (ctrl+shift+c Copy).
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"C\"\nmods = \"Control|Shift\"\nmode = \"Vi\"\naction = \"None\"\n",
        )
        .unwrap();
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+c"), None);
        // A plain `Vi` binding replaces the `Vi|~Search` default.
        assert_eq!(desc(&one("g", "Vi"), "Vi", "g"), None);
    }

    #[test]
    fn same_trigger_user_bindings_share_an_entry() {
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"a\"\nmods = \"Alt|Alt\"\naction = \"Copy\"\n\
             [[keyboard.bindings]]\nkey = \"a\"\nmods = \"Alt\"\nmode = \"~Vi|~vi\"\naction = \"Paste\"\n",
        )
        .unwrap();
        // Active in different modes (the second not in Vi): two entries.
        let got: Vec<(&str, &str)> = sheet.binds["alacritty"][..2]
            .iter()
            .map(|b| (b.key.as_str(), b.desc.as_str()))
            .collect();
        assert_eq!(got, [("alt+a", "Copy"), ("alt+a", "Paste")]);
        // Same modes: one entry, both actions in order, action kept in step with desc.
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"a\"\nmods = \"Alt\"\nmode = \"~Vi\"\naction = \"Copy\"\n\
             [[keyboard.bindings]]\nkey = \"a\"\nmods = \"Alt\"\nmode = \"~vi|~Vi\"\naction = \"Paste\"\n",
        )
        .unwrap();
        let b = &sheet.binds["alacritty"][0];
        assert_eq!((b.key.as_str(), b.desc.as_str()), ("alt+a", "Copy, Paste"));
        assert_eq!(b.action, b.desc);
    }

    #[test]
    fn sheet_from_reads_the_first_config_or_none() {
        let dir =
            std::env::temp_dir().join(format!("keybind-find-alacritty-{}", std::process::id()));
        let xdg = dir.join("xdg");
        std::fs::create_dir_all(xdg.join("alacritty")).unwrap();
        std::fs::create_dir_all(dir.join("home/.config/alacritty")).unwrap();
        let env = |n: &str| match n {
            "XDG_CONFIG_HOME" => Some(xdg.display().to_string()),
            "XDG_CONFIG_DIRS" => Some(dir.join("nodirs").display().to_string()),
            "HOME" => Some(dir.join("home").display().to_string()),
            _ => None,
        };
        let has = |s: &Sheet, key: &str| desc(s, "alacritty", key).is_some();
        // No file anywhere under these dirs (the /etc fallbacks aside): defaults, no error.
        if !std::path::Path::new("/etc/alacritty/alacritty.toml").exists() {
            let s = sheet_from(&env).unwrap();
            assert!(has(&s, "ctrl+shift+v") && !has(&s, "ctrl+shift+n"));
        }
        // The XDG file wins over the ~/.config one.
        let bind = |key: &str| {
            format!("[[keyboard.bindings]]\nkey = \"{key}\"\nmods = \"Alt\"\naction = \"Copy\"\n")
        };
        std::fs::write(dir.join("home/.config/alacritty/alacritty.toml"), bind("h")).unwrap();
        std::fs::write(xdg.join("alacritty/alacritty.toml"), bind("x")).unwrap();
        let s = sheet_from(&env).unwrap();
        assert!(has(&s, "alt+x") && !has(&s, "alt+h"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn odd_keys_and_actions() {
        let text = r#"
[[keyboard.bindings]]
KEY = 30
action = "Copy"
[[keyboard.bindings]]
key = " "
mods = "Control|Shift"
action = "Copy"
[[keyboard.bindings]]
key = "z"
mods = "Alt"
action = "ExpandSelection"
"#;
        let (sheet, notes) = parse_reporting(text).unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[0].contains("scancode key 30 not shown"),
            "{}",
            notes[0]
        );
        // `" "` is a character no key press gives; it leaves the Space default alone.
        assert!(notes[1].contains("unknown key \" \""), "{}", notes[1]);
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+shift+space"),
            Some("ToggleViMode")
        );
        assert_eq!(
            desc(&sheet, "alacritty", "alt+z"),
            Some("ExpandSelection (a mouse-only action; alacritty skips it)")
        );
    }

    #[test]
    fn a_binding_that_is_not_a_table() {
        let (sheet, notes) = parse_reporting("[keyboard]\nbindings = [1]\n").unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("[0]: not a table"), "{}", notes[0]);
        assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+v"), Some("Paste"));
    }

    #[test]
    fn unreadable_config_and_old_yml() {
        let dir =
            std::env::temp_dir().join(format!("keybind-find-alacritty-odd-{}", std::process::id()));
        let conf = dir.join("alacritty");
        std::fs::create_dir_all(&conf).unwrap();
        let env = |n: &str| match n {
            "XDG_CONFIG_HOME" => Some(dir.display().to_string()),
            "XDG_CONFIG_DIRS" => Some(dir.join("none").display().to_string()),
            "HOME" => Some(dir.join("home").display().to_string()),
            _ => None,
        };
        // Only the old YAML file: defaults (the note goes to stderr).
        std::fs::write(conf.join("alacritty.yml"), "key_bindings: []\n").unwrap();
        let s = sheet_from(&env).unwrap();
        assert_eq!(desc(&s, "alacritty", "ctrl+shift+v"), Some("Paste"));
        // Invalid UTF-8: unreadable, so defaults rather than an error.
        std::fs::write(conf.join("alacritty.toml"), [0xff, 0xfe, b'[']).unwrap();
        let s = sheet_from(&env).unwrap();
        assert_eq!(desc(&s, "alacritty", "ctrl+shift+v"), Some("Paste"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_broken_config_shows_the_defaults() {
        for text in ["not toml [", "[keyboard]\nbindings = 3", "keyboard = 3"] {
            let (sheet, notes) = parse_reporting(text).unwrap();
            assert_eq!(notes.len(), 1, "{notes:?}");
            assert!(
                notes[0].contains("showing alacritty's defaults"),
                "{}",
                notes[0]
            );
            assert_eq!(desc(&sheet, "alacritty", "ctrl+shift+v"), Some("Paste"));
        }
    }

    #[test]
    fn keypad_and_main_keys_match_as_alacritty_does() {
        let unbind = |key: &str| {
            parse(&format!(
                "[[keyboard.bindings]]\nkey = \"{key}\"\nmods = \"Control\"\naction = \"None\"\n"
            ))
            .unwrap()
        };
        // The default ctrl+`+` is anywhere on the keyboard: NumpadAdd unbinds it, and ctrl+KP_Add too.
        let sheet = unbind("NumpadAdd");
        assert_eq!(desc(&sheet, "alacritty", "ctrl+plus"), None);
        assert_eq!(desc(&sheet, "alacritty", "ctrl+KP_Add"), None);
        // `Plus` is the main keyboard's: it leaves the keypad default.
        let sheet = unbind("Plus");
        assert_eq!(desc(&sheet, "alacritty", "ctrl+plus"), None);
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+KP_Add"),
            Some("IncreaseFontSize")
        );
        // The character `+` is anywhere: it takes the keypad default too.
        let sheet = unbind("+");
        assert_eq!(desc(&sheet, "alacritty", "ctrl+KP_Add"), None);
        // `Minus` is the main keyboard's; `-` is anywhere.
        let sheet = unbind("Minus");
        assert_eq!(desc(&sheet, "alacritty", "ctrl+minus"), None);
        assert_eq!(
            desc(&sheet, "alacritty", "ctrl+KP_Subtract"),
            Some("DecreaseFontSize")
        );
        assert_eq!(desc(&unbind("-"), "alacritty", "ctrl+KP_Subtract"), None);
        // Numpad0 unbinds the default ctrl+0 (a character: anywhere).
        assert_eq!(desc(&unbind("Numpad0"), "alacritty", "ctrl+0"), None);
        // NumpadEnter matches the default Enter bindings (named: anywhere).
        let sheet = parse(
            "[[keyboard.bindings]]\nkey = \"NumpadEnter\"\nmode = \"Vi|~Search\"\naction = \"None\"\n",
        )
        .unwrap();
        assert_eq!(desc(&sheet, "Vi", "Return"), None);
    }

    #[test]
    fn keys_and_mods() {
        let k = |key: &str, mods: &str| {
            let mods = (!mods.is_empty()).then_some(mods);
            let b = binding(key, mods, None, String::new(), false).unwrap();
            chord(&b.mods, &b.keysym)
        };
        assert_eq!(k("N", "Control|Shift"), "ctrl+shift+n");
        assert_eq!(k("n", " Shift | Control "), "ctrl+shift+n");
        assert_eq!(k("Return", "Shift"), "shift+Return");
        assert_eq!(k("Enter", ""), "Return");
        assert_eq!(k("PageUp", "Super|Alt"), "super+alt+Prior");
        assert_eq!(k("PageDown", "Command"), "super+Next");
        assert_eq!(k("Home", "Option"), "alt+Home");
        assert_eq!(k("F5", "None"), "F5");
        assert_eq!(k("Key1", ""), "1");
        assert_eq!(k("Numpad7", ""), "KP_7");
        assert_eq!(k("LBracket", "Control"), "ctrl+bracketleft");
        assert_eq!(k("/", ""), "slash");
        assert_eq!(k("Back", ""), "BackSpace");
        assert_eq!(k("ö", ""), "ö");
        assert_eq!(k("Ö", ""), "ö");
        assert_eq!(k("PrintScreen", "Shift"), "shift+Print");
        assert_eq!(k("CapsLock", "None"), "Caps_Lock");
        assert_eq!(k("Copy", ""), "XF86Copy");
        assert_eq!(k("Equals", "Control"), "ctrl+equal");
        assert_eq!(k("Backslash", ""), "backslash");
        assert_eq!(k("ContextMenu", ""), "Menu");
        assert_eq!(k("NumpadComma", ""), "KP_Separator");
        assert!(binding("Not a key", None, None, String::new(), false).is_err());
        assert!(binding("a", Some(""), None, String::new(), false).is_err());
    }

    #[test]
    fn modes_and_categories() {
        let cat = |mode: Option<&str>| {
            binding("a", None, mode, String::new(), false)
                .unwrap()
                .category()
        };
        assert_eq!(cat(None), "alacritty");
        assert_eq!(cat(Some("~Vi|~Alt")), "alacritty");
        assert_eq!(cat(Some("Vi")), "Vi");
        assert_eq!(cat(Some("vi | ~search")), "Vi");
        assert_eq!(cat(Some("Search|Vi")), "Vi|Search");
        assert_eq!(cat(Some("Vi|vi")), "Vi");
        assert_eq!(cat(Some("AppKeypad")), "AppKeypad");
        assert_eq!(cat(Some("AppCursor")), "AppCursor");
    }

    #[test]
    fn chars_are_escaped() {
        assert_eq!(escape("\u{1b}\r"), "\\e\\r");
        assert_eq!(escape("a\\b\u{7f}\u{1}\t\n"), "a\\\\b\\x7f\\x01\\t\\n");
    }

    #[test]
    fn config_paths_in_alacrittys_order() {
        let env = |n: &str| match n {
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "XDG_CONFIG_DIRS" => Some("/d1:relative:/d2".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        let want: Vec<PathBuf> = [
            "/xdg/alacritty/alacritty.toml",
            "/d1/alacritty/alacritty.toml",
            "/d2/alacritty/alacritty.toml",
            "/xdg/alacritty.toml",
            "/d1/alacritty.toml",
            "/d2/alacritty.toml",
            "/h/.config/alacritty/alacritty.toml",
            "/h/.alacritty.toml",
            "/etc/alacritty/alacritty.toml",
        ]
        .map(PathBuf::from)
        .to_vec();
        assert_eq!(config_paths(&env), want);
        // Defaults: ~/.config and /etc/xdg; a relative XDG_CONFIG_HOME is ignored, as the xdg crate does.
        let env = |n: &str| match n {
            "HOME" => Some("/h".to_string()),
            "XDG_CONFIG_HOME" => Some("rel".to_string()),
            _ => None,
        };
        let paths = config_paths(&env);
        assert_eq!(
            paths[0],
            PathBuf::from("/h/.config/alacritty/alacritty.toml")
        );
        assert_eq!(paths[1], PathBuf::from("/etc/xdg/alacritty/alacritty.toml"));
    }
}
