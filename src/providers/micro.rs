//! micro: its Linux default keybindings pinned per version (`data/micro/`, generated from micro's source), with the
//! user's `bindings.json` laid over them as micro reads it. Shown when micro runs in the focused terminal. Actions
//! (`command:…`, `lua:…`) are shown as text, never run.

use super::nvim::single;
use super::pinned;
use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const REGISTRY: &[(&str, &str)] = &[
    (
        "2.0.11",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/micro/2.0.11.json"
        )),
    ),
    (
        "2.0.14",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/micro/2.0.14.json"
        )),
    ),
    (
        "2.0.15",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/micro/2.0.15.json"
        )),
    ),
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// micro's pane types, as `bindings.json` names them.
const PANES: [&str; 3] = ["buffer", "command", "terminal"];

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(REGISTRY, |_| {}, &env)?;
    if !notes.is_empty() {
        eprintln!("keybind-find: micro: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `sheet` over `registry`, with `setup` applied to `micro -version` (a test seam: `PATH`) and `bindings.json` found
/// through `env`.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut cmd = Command::new("micro");
    cmd.arg("-version")
        .env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let version = run_for(cmd, VERSION_TIMEOUT).context("running micro -version")?;
    let installed = version_of(&version);
    let (v, text, note) = pinned::select("micro", registry, installed.as_deref())
        .context("no pinned micro bindings")?;
    // `pinned::select` names a `.toml` to add; micro's pins are `.json`.
    let installed = installed.unwrap_or_default();
    let mut notes: Vec<String> = note
        .into_iter()
        .map(|n| n.replace(&format!("{installed}.toml"), &format!("{installed}.json")))
        .collect();
    let defaults = defaults(text).with_context(|| format!("reading data/micro/{v}.json"))?;
    let mut keymap = Keymap {
        panes: PANES.iter().map(|p| (p.to_string(), Vec::new())).collect(),
        left_out: 0,
    };
    for (pane, binds) in &defaults {
        for (key, action) in binds {
            keymap.bind(pane, key, action);
        }
    }
    if keymap.left_out > 0 {
        bail!(
            "data/micro/{v}.json has {} keys keybind-find can't read",
            keymap.left_out
        );
    }
    if let Some(path) = bindings_file(env) {
        match read_json(&path) {
            Ok(user) => keymap.overlay(&user, &mut notes),
            Err(e) => {
                // On one line: a parse error can span several.
                let e = format!("{e:#}")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                notes.push(format!("{e}; showing micro's defaults"));
            }
        }
    }
    if keymap.left_out > 0 {
        let n = keymap.left_out;
        let bindings = if n == 1 { "binding" } else { "bindings" };
        notes.push(format!(
            "leaving out {n} bindings.json {bindings}: mouse keys, raw escapes, keys micro or keybind-find can't read, \
             or values that aren't strings"
        ));
    }
    Ok((keymap.into_sheet(), notes))
}

/// The first dotted number in `micro -version` (`Version: 2.0.15`, then the commit and build date); a build without
/// a release version (`0.0.0-unknown`) has none.
fn version_of(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|w| w.trim_matches('.'))
        .find(|w| w.contains('.') && pinned::parse_version(w).is_some())
        .filter(|v| *v != "0.0.0")
        .map(str::to_string)
}

/// `bindings.json` in micro's config dir: `$MICRO_CONFIG_HOME`, else `$XDG_CONFIG_HOME/micro`, else
/// `~/.config/micro` (an empty variable counts as unset); `None` when there's no file (micro would create one; this
/// only reads).
// ponytail: `env` is keybind-find's own; a `MICRO_CONFIG_HOME` set only in micro's shell, or `-config-dir`, isn't seen.
fn bindings_file(env: Env) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = set("MICRO_CONFIG_HOME")
        .or_else(|| set("XDG_CONFIG_HOME").map(|d| d.join("micro")))
        .or_else(|| set("HOME").map(|h| h.join(".config/micro")))?;
    Some(dir.join("bindings.json")).filter(|f| f.exists())
}

fn read_json(path: &std::path::Path) -> anyhow::Result<Value> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&strip_jsonc(&text)).with_context(|| format!("reading {}", path.display()))
}

/// A pane's `key: action` pairs.
type Pairs = Vec<(String, String)>;

/// A pane's bindings: (event, key notation, action), in order.
type Binds = Vec<(Vec<Event>, String, String)>;

/// The pinned defaults: each pane's bindings, in the file's (`bindings.json`) shape.
fn defaults(text: &str) -> anyhow::Result<Vec<(String, Pairs)>> {
    let value: Value = serde_json::from_str(&strip_jsonc(text))?;
    PANES
        .iter()
        .map(|pane| {
            let binds = value
                .get(pane)
                .and_then(Value::as_object)
                .with_context(|| format!("no {pane:?} object"))?
                .iter()
                .map(|(k, v)| {
                    let action = v
                        .as_str()
                        .with_context(|| format!("{k:?} isn't a string"))?;
                    Ok((k.clone(), action.to_string()))
                })
                .collect::<anyhow::Result<_>>()?;
            Ok((pane.to_string(), binds))
        })
        .collect()
}

/// JSON with comments, as JSON: `//` and `/* */` comments and trailing commas (before `]` or `}`) removed outside
/// strings. Other JSON5 (single quotes, unquoted keys) is left for the JSON parser to refuse.
pub(crate) fn strip_jsonc(text: &str) -> String {
    // Comments first: each becomes a space, keeping its newlines, so a parse error's line number is the file's.
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            plain.push(c);
            match c {
                '\\' => plain.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                plain.push(c);
            }
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        plain.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if c == '\n' {
                        plain.push('\n');
                    }
                    if last == '*' && c == '/' {
                        break;
                    }
                    last = c;
                }
                plain.push(' ');
            }
            _ => plain.push(c),
        }
    }
    // Then a comma whose next non-space character closes the array or object.
    let mut out = String::with_capacity(plain.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in plain.char_indices() {
        if in_string {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if c == ',' && plain[i + 1..].trim_start().starts_with([']', '}']) {
            continue;
        }
        out.push(c);
    }
    out
}

/// A key as micro tells keys apart (its `Event`): a tcell key code or a typed character, with modifiers; a sequence
/// is several.
#[derive(Clone, PartialEq, Debug)]
struct Event {
    code: Code,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

#[derive(Clone, PartialEq, Debug)]
enum Code {
    /// tcell's codes below 128 (`CtrlA` is 1, `Enter` is CR 13, `Backspace` is DEL 127, `OldBackspace` BS 8).
    Num(u32),
    /// A named key with a code of its own (`Up`, `F12`).
    Name(&'static str),
    Rune(char),
}

enum Parsed {
    Key(Vec<Event>),
    /// A mouse key or a raw escape.
    NotKey,
    Unknown,
}

/// micro's `keyEvents` names as tcell codes (`internal/action/bindings.go`), aliases joined as tcell joins them.
fn key_code(name: &str) -> Option<Code> {
    const NAMED: &[&str] = &[
        "Up",
        "Down",
        "Right",
        "Left",
        "UpLeft",
        "UpRight",
        "DownLeft",
        "DownRight",
        "Center",
        "PageUp",
        "PageDown",
        "Home",
        "End",
        "Insert",
        "Delete",
        "Help",
        "Exit",
        "Clear",
        "Cancel",
        "Print",
        "Pause",
        "Backtab",
    ];
    const CTRL: [&str; 32] = [
        "Space",
        "A",
        "B",
        "C",
        "D",
        "E",
        "F",
        "G",
        "H",
        "I",
        "J",
        "K",
        "L",
        "M",
        "N",
        "O",
        "P",
        "Q",
        "R",
        "S",
        "T",
        "U",
        "V",
        "W",
        "X",
        "Y",
        "Z",
        "LeftSq",
        "Backslash",
        "RightSq",
        "Carat",
        "Underscore",
    ];
    if let Some(rest) = name.strip_prefix("Ctrl") {
        return CTRL
            .iter()
            .position(|c| *c == rest)
            .map(|n| Code::Num(n as u32));
    }
    Some(match name {
        "Tab" => Code::Num(9),
        "Esc" | "Escape" => Code::Num(27),
        "Enter" => Code::Num(13),
        "Backspace" => Code::Num(127),
        "OldBackspace" => Code::Num(8),
        "PgUp" => Code::Name("PageUp"),
        "PgDown" => Code::Name("PageDown"),
        f if f
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=64).contains(&n) && !f[1..].starts_with('0')) =>
        {
            // The `&'static str` for this F-key: micro's names are F1 to F64.
            Code::Name(FKEYS.iter().find(|k| **k == f)?)
        }
        n => Code::Name(NAMED.iter().find(|k| **k == n)?),
    })
}

const FKEYS: [&str; 64] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "F25", "F26", "F27", "F28",
    "F29", "F30", "F31", "F32", "F33", "F34", "F35", "F36", "F37", "F38", "F39", "F40", "F41",
    "F42", "F43", "F44", "F45", "F46", "F47", "F48", "F49", "F50", "F51", "F52", "F53", "F54",
    "F55", "F56", "F57", "F58", "F59", "F60", "F61", "F62", "F63", "F64",
];

/// The mouse names micro takes (`mouseEvents`), with an optional `Drag`/`Release` suffix.
const MOUSE: [&str; 7] = [
    "MouseLeft",
    "MouseMiddle",
    "MouseRight",
    "MouseWheelUp",
    "MouseWheelDown",
    "MouseWheelLeft",
    "MouseWheelRight",
];

/// A `bindings.json` key as micro's `findEvent` reads it: `<…>` groups are a sequence; else one key, its modifier
/// prefixes (`Ctrl`, `Alt`, `Shift`, optional `-`) then a `keyEvents` name (after `Ctrl`, the rest upper-cased and
/// tried as `Ctrl<k>` first), a mouse name, or one character.
fn parse_key(key: &str) -> Parsed {
    // micro's `<(.+?)>`, unanchored: each match is one key of the sequence, and text between matches is skipped.
    // When the first search finds none, or a later one finds none with text left, the key is read whole.
    let mut events = Vec::new();
    let mut rest = key;
    while !rest.is_empty() {
        // `.+?`: at least one character, up to the first `>`, never across a newline.
        let found = rest.match_indices('<').find_map(|(i, _)| {
            let after = &rest[i + 1..];
            let first = after.chars().next().filter(|c| *c != '\n')?;
            let tail = &after[first.len_utf8()..];
            let close = tail
                .find(['>', '\n'])
                .filter(|&j| tail[j..].starts_with('>'))?;
            Some((i, i + 1 + first.len_utf8() + close))
        });
        let Some((open, close)) = found else {
            return parse_single(key);
        };
        match parse_single(&rest[open + 1..close]) {
            Parsed::Key(mut e) => events.append(&mut e),
            Parsed::NotKey => return Parsed::NotKey,
            Parsed::Unknown => return Parsed::Unknown,
        }
        rest = &rest[close + 1..];
    }
    if events.is_empty() {
        return parse_single(key);
    }
    Parsed::Key(events)
}

fn parse_single(key: &str) -> Parsed {
    let mut k = key;
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    loop {
        if k.starts_with('-') && k != "-" {
            k = &k[1..];
        } else if k.starts_with("Ctrl") && k != "CtrlH" {
            k = &k[4..];
            ctrl = true;
        } else if let Some(r) = k.strip_prefix("Alt") {
            k = r;
            alt = true;
        } else if let Some(r) = k.strip_prefix("Shift") {
            k = r;
            shift = true;
        } else if k.starts_with('\u{1b}') {
            return Parsed::NotKey;
        } else {
            break;
        }
    }
    if k.is_empty() {
        return Parsed::Unknown;
    }
    let event = |code| {
        Parsed::Key(vec![Event {
            code,
            ctrl,
            alt,
            shift,
        }])
    };
    // With ctrl, micro upper-cases the key's first letter and keeps it so (`Ctrl-up` is ctrl+Up).
    let upper;
    if ctrl {
        upper = match k.get(..1) {
            Some(first) => first.to_uppercase() + &k[1..],
            None => k.to_string(),
        };
        k = &upper;
        if let Some(code) = key_code(&format!("Ctrl{k}")) {
            return event(code);
        }
    }
    if let Some(code) = key_code(k) {
        return event(code);
    }
    // micro strips the suffix from the key itself: `aDrag` is `a`.
    let k = k
        .strip_suffix("Drag")
        .or_else(|| k.strip_suffix("Release"))
        .unwrap_or(k);
    if MOUSE.contains(&k) {
        return Parsed::NotKey;
    }
    match k.as_bytes() {
        [b] => event(Code::Rune(char::from(*b))),
        _ => Parsed::Unknown,
    }
}

/// An event in keybind-find's notation; `None` when it has no X keysym (`F36`, `Center`).
fn notation(e: &Event) -> Option<String> {
    const CTRL_CHARS: [&str; 32] = [
        "space",
        "a",
        "b",
        "c",
        "d",
        "e",
        "f",
        "g",
        "h",
        "i",
        "j",
        "k",
        "l",
        "m",
        "n",
        "o",
        "p",
        "q",
        "r",
        "s",
        "t",
        "u",
        "v",
        "w",
        "x",
        "y",
        "z",
        "bracketleft",
        "backslash",
        "bracketright",
        "asciicircum",
        "underscore",
    ];
    let mut mods = Vec::new();
    let mut ctrl = e.ctrl;
    let key = match &e.code {
        Code::Num(127) => "BackSpace".to_string(),
        Code::Num(9) if !ctrl => "Tab".into(),
        Code::Num(13) if !ctrl => "Return".into(),
        Code::Num(27) if !ctrl => "Escape".into(),
        Code::Num(n) => {
            // A control character: typed with ctrl (`CtrlH`, `OldBackspace`: BS, which ctrl+h types).
            ctrl = true;
            CTRL_CHARS.get(*n as usize)?.to_string()
        }
        Code::Name("Backtab") => {
            mods.push("shift");
            "Tab".into()
        }
        Code::Name("PageUp") => "Page_Up".into(),
        Code::Name("PageDown") => "Page_Down".into(),
        Code::Name(f) if f.starts_with('F') => {
            // X has F1 to F35.
            let n: u8 = f[1..].parse().ok()?;
            (n <= 35).then(|| f.to_string())?
        }
        Code::Name(
            n @ ("Up" | "Down" | "Left" | "Right" | "Home" | "End" | "Insert" | "Delete" | "Help"
            | "Cancel" | "Print" | "Pause" | "Clear"),
        ) => n.to_string(),
        Code::Name(_) => return None,
        Code::Rune(c) => {
            if c.is_ascii_uppercase() {
                mods.push("shift");
            }
            single(c.to_ascii_lowercase())?.to_string()
        }
    };
    if ctrl {
        mods.push("ctrl");
    }
    if e.alt {
        mods.push("alt");
    }
    if e.shift {
        mods.push("shift");
    }
    Some(chord(&mods, &key))
}

/// Each pane's bindings: (event, key notation, action), in the order they were bound.
struct Keymap {
    panes: Vec<(String, Binds)>,
    left_out: usize,
}

impl Keymap {
    /// Binds `key` to `action` in `pane`, replacing the binding of the same event (micro keeps one action per event);
    /// `None` unbinds it.
    // ponytail: two spellings of one key in the same file have no defined winner in micro (a Go map); here the one
    // serde_json gives last (its maps are sorted by key) wins.
    fn bind(&mut self, pane: &str, key: &str, action: &str) {
        let events = match parse_key(key) {
            Parsed::Key(e) => e,
            Parsed::NotKey | Parsed::Unknown => {
                self.left_out += 1;
                return;
            }
        };
        let Some(notation) = events.iter().map(notation).collect::<Option<Vec<_>>>() else {
            self.left_out += 1;
            return;
        };
        let Some((_, binds)) = self.panes.iter_mut().find(|(p, _)| p == pane) else {
            return;
        };
        let at = binds.iter().position(|(e, _, _)| *e == events);
        if action == "None" {
            if let Some(i) = at {
                binds.remove(i);
            }
            return;
        }
        let new = (events, notation.join(" "), action.to_string());
        match at {
            Some(i) => binds[i] = new,
            None => binds.push(new),
        }
    }

    /// `bindings.json` as micro's `InitBindings` reads it: a string value binds in `buffer`; an object named after a
    /// pane binds in that pane; another object name is a note; anything else is skipped and counted.
    fn overlay(&mut self, user: &Value, notes: &mut Vec<String>) {
        let Some(top) = user.as_object() else {
            notes.push("bindings.json isn't a JSON object; showing micro's defaults".into());
            return;
        };
        let mut unknown = Vec::new();
        for (k, v) in top {
            match v {
                Value::String(action) => self.bind("buffer", k, action),
                Value::Object(binds) if PANES.contains(&k.as_str()) => {
                    for (key, action) in binds {
                        match action.as_str() {
                            Some(a) => self.bind(k, key, a),
                            None => self.left_out += 1,
                        }
                    }
                }
                Value::Object(_) => unknown.push(k.clone()),
                _ => self.left_out += 1,
            }
        }
        if !unknown.is_empty() {
            let names: Vec<String> = unknown.iter().map(|n| format!("{n:?}")).collect();
            let is = if names.len() == 1 {
                "isn't a pane"
            } else {
                "aren't panes"
            };
            notes.push(format!(
                "bindings.json's {} {is} (buffer, command, terminal); ignored",
                names.join(", ")
            ));
        }
    }

    fn into_sheet(self) -> Sheet {
        let mut sheet = Sheet {
            title: "micro".into(),
            provider: "micro".into(),
            binds: Default::default(),
        };
        for (pane, binds) in self.panes {
            for (_, key, action) in binds {
                sheet.binds.entry(pane.clone()).or_default().push(Bind {
                    key,
                    desc: action.clone(),
                    action,
                    when: Default::default(),
                });
            }
        }
        sheet
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/micro")
    }

    fn keymap(v: &str) -> Keymap {
        let text = REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1;
        let mut k = Keymap {
            panes: PANES.iter().map(|p| (p.to_string(), Vec::new())).collect(),
            left_out: 0,
        };
        for (pane, binds) in defaults(text).unwrap() {
            for (key, action) in binds {
                k.bind(&pane, &key, &action);
            }
        }
        k
    }

    fn find<'a>(s: &'a Sheet, pane: &str, key: &str) -> Vec<&'a str> {
        s.binds
            .get(pane)
            .map(|b| {
                b.iter()
                    .filter(|b| b.key == key)
                    .map(|b| b.desc.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn pinned_files() {
        for (v, commit) in [
            ("2.0.11", "225927b9a25f0d50ea63ea18bc7bb68e404c0cfd"),
            ("2.0.14", "04c577049ca898f097cd6a2dae69af0b4d4493e1"),
            ("2.0.15", "6a62575bcfdf4965f187eedafceb3400316e612b"),
        ] {
            let text = REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1;
            assert!(text.contains(&format!("at tag v{v}")), "{v}");
            assert!(text.contains(&format!("(commit {commit})")), "{v}");
            assert!(text.contains("MIT licensed"), "{v}");
            for file in [
                "internal/action/defaults_other.go",
                "internal/action/defaults.go",
            ] {
                assert!(text.contains(file), "{v}: {file}");
            }
            assert!(
                defaults(text)
                    .unwrap()
                    .iter()
                    .all(|(_, b)| b.iter().all(|(k, _)| !k.contains("Mouse"))),
                "{v}"
            );
            let k = keymap(v);
            assert_eq!(k.left_out, 0, "{v}");
            // Every key listed, less the spellings of one event (`CtrlH` and `OldBackspace` are both BS).
            let keys: usize = defaults(text).unwrap().iter().map(|(_, b)| b.len()).sum();
            let listed: usize = k.panes.iter().map(|(_, b)| b.len()).sum();
            assert_eq!(listed, keys - 2, "{v}");
        }
        let s = keymap("2.0.15").into_sheet();
        assert_eq!(find(&s, "buffer", "ctrl+c"), ["Copy|CopyLine"]);
        assert_eq!(
            find(&keymap("2.0.14").into_sheet(), "buffer", "ctrl+c"),
            ["CopyLine|Copy"]
        );
        assert_eq!(find(&s, "buffer", "ctrl+h"), ["Backspace"]);
        assert_eq!(find(&s, "buffer", "BackSpace"), ["Backspace"]);
        assert_eq!(find(&s, "buffer", "Return"), ["InsertNewline"]);
        assert_eq!(
            find(&s, "buffer", "shift+Tab"),
            ["CycleAutocompleteBack|OutdentSelection|OutdentLine"]
        );
        assert_eq!(find(&s, "buffer", "alt+shift+f"), ["FindLiteral"]);
        assert_eq!(find(&s, "buffer", "alt+braceleft"), ["ParagraphPrevious"]);
        assert_eq!(find(&s, "terminal", "ctrl+q ctrl+q"), ["Exit"]);
        assert_eq!(
            s.binds.keys().collect::<Vec<_>>(),
            ["buffer", "command", "terminal"]
        );
        // 2.0.11 lacks Alt-[ and ShiftPageUp.
        let old = keymap("2.0.11").into_sheet();
        assert!(find(&old, "buffer", "alt+bracketleft").is_empty());
        assert_eq!(
            find(&s, "buffer", "alt+bracketleft"),
            ["DiffPrevious|CursorStart"]
        );
    }

    #[test]
    fn version_pick() {
        let pick = |out: &str| {
            let (v, _, note) =
                pinned::select("micro", REGISTRY, version_of(out).as_deref()).unwrap();
            (v, note)
        };
        assert_eq!(
            pick("Version: 2.0.15\nCommit hash: 6a62575b\nCompiled on October 01, 2025\n"),
            ("2.0.15", None)
        );
        assert_eq!(
            pick("Version: 2.0.13"),
            (
                "2.0.11",
                Some(
                    "micro 2.0.13: using the defaults of 2.0.11; add data/micro/2.0.13.toml".into()
                )
            )
        );
        assert_eq!(pick("Version: 2.0.16").0, "2.0.15");
        assert_eq!(pick("Version: 0.0.0-unknown").0, "2.0.15");
        assert_eq!(
            pick("garbage"),
            (
                "2.0.15",
                Some("micro's version is unknown: using the defaults of 2.0.15".into())
            )
        );
    }

    #[test]
    fn jsonc() {
        assert_eq!(
            strip_jsonc(r#"{"a": "x // y"} // c"#).trim(),
            r#"{"a": "x // y"}"#
        );
        assert_eq!(
            strip_jsonc("{\"a\": 1 /* x\n y */}").replace(char::is_whitespace, ""),
            "{\"a\":1}"
        );
        assert_eq!(
            strip_jsonc("[1, 2, /* c */\n ]").replace(char::is_whitespace, ""),
            "[1,2]"
        );
        assert_eq!(strip_jsonc(r#"{"a": "q\" ,}", }"#), r#"{"a": "q\" ,}" }"#);
        let v: Value = serde_json::from_str(&strip_jsonc("{\"a\": [1, 2,], // x\n}")).unwrap();
        assert_eq!(v["a"][1], 2);
    }

    #[test]
    fn keys() {
        for (micro, want) in [
            ("Ctrl-x", Some("ctrl+x")),
            ("CtrlX", Some("ctrl+x")),
            ("Ctrl-X", Some("ctrl+x")),
            ("CtrlG", Some("ctrl+g")),
            ("Alt-g", Some("alt+g")),
            ("Alt-G", Some("alt+shift+g")),
            ("AltShiftLeft", Some("alt+shift+Left")),
            ("CtrlShiftUp", Some("ctrl+shift+Up")),
            ("Backtab", Some("shift+Tab")),
            ("F12", Some("F12")),
            ("F35", Some("F35")),
            ("CtrlSpace", Some("ctrl+space")),
            ("CtrlLeftSq", Some("ctrl+bracketleft")),
            ("CtrlBackslash", Some("ctrl+backslash")),
            ("CtrlRightSq", Some("ctrl+bracketright")),
            ("CtrlCarat", Some("ctrl+asciicircum")),
            ("CtrlUnderscore", Some("ctrl+underscore")),
            ("<Ctrl-q><Ctrl-q>", Some("ctrl+q ctrl+q")),
            ("Alt-{", Some("alt+braceleft")),
            ("Enter", Some("Return")),
            ("Esc", Some("Escape")),
            ("Escape", Some("Escape")),
            ("PgUp", Some("Page_Up")),
            ("Alt-CtrlH", Some("ctrl+alt+h")),
            ("q", Some("q")),
            ("Q", Some("shift+q")),
            ("Ctrl-m", Some("ctrl+m")),
        ] {
            let got = match parse_key(micro) {
                Parsed::Key(e) => e
                    .iter()
                    .map(notation)
                    .collect::<Option<Vec<_>>>()
                    .map(|n| n.join(" ")),
                _ => None,
            };
            assert_eq!(got.as_deref(), want, "{micro}");
        }
        for not in ["MouseLeft", "MouseWheelUpDrag", "\u{1b}ctrlback"] {
            assert!(matches!(parse_key(not), Parsed::NotKey), "{not}");
        }
        // Ctrl keeps the upper-cased name; micro's `<…>` is unanchored; `<` alone is a character.
        for (k, want) in [
            ("Ctrl-up", "ctrl+Up"),
            ("x<Ctrl-a>", "ctrl+a"),
            ("<", "less"),
            ("aDrag", "a"),
        ] {
            let Parsed::Key(e) = parse_key(k) else {
                panic!("{k}")
            };
            assert_eq!(
                e.iter()
                    .map(|e| notation(e).unwrap())
                    .collect::<Vec<_>>()
                    .join(" "),
                want,
                "{k}"
            );
        }
        // A group can't start at a multi-byte character's middle, nor span a newline.
        assert!(matches!(parse_key("<é><CtrlA>"), Parsed::Unknown));
        let Parsed::Key(e) = parse_key("<\n><a>") else {
            panic!()
        };
        assert_eq!(notation(&e[0]).as_deref(), Some("a"));
        for unknown in ["Nonsense", "", "<Ctrl-q", "<Nope>", "<Ctrl-q><Nope>"] {
            assert!(matches!(parse_key(unknown), Parsed::Unknown), "{unknown}");
        }
        // F64 is a key to micro, with no X keysym.
        let Parsed::Key(f64) = parse_key("F64") else {
            panic!()
        };
        assert_eq!(notation(&f64[0]), None);
        // Spellings micro binds as one event, and ones it keeps apart (as tcell reports them).
        let same = |a: &str, b: &str| match (parse_key(a), parse_key(b)) {
            (Parsed::Key(x), Parsed::Key(y)) => x == y,
            _ => false,
        };
        assert!(
            same("CtrlS", "Ctrl-s") && same("Ctrl-s", "Ctrl-S") && same("CtrlH", "OldBackspace")
        );
        assert!(same("Esc", "Escape") && same("PgUp", "PageUp"));
        assert!(!same("Ctrl-m", "Enter") && !same("Ctrl-i", "Tab") && !same("CtrlLeftSq", "Esc"));
        assert!(!same("Backspace", "OldBackspace") && !same("Ctrl-h", "CtrlH"));
    }

    #[test]
    fn bindings_json_over_defaults() {
        let mut k = keymap("2.0.15");
        let mut notes = Vec::new();
        k.overlay(
            &read_json(&fixtures().join("config/bindings.json")).unwrap(),
            &mut notes,
        );
        let left_out = k.left_out;
        let s = k.into_sheet();
        assert_eq!(find(&s, "buffer", "ctrl+z"), ["Undo|Redo"]);
        // Another spelling of one key replaces it.
        assert_eq!(find(&s, "buffer", "ctrl+s"), ["SaveAll"]);
        assert!(find(&s, "buffer", "ctrl+a").is_empty());
        // `Ctrl-m` is its own event in micro, not Enter's.
        assert_eq!(find(&s, "buffer", "ctrl+m"), ["command:pwd"]);
        assert_eq!(find(&s, "buffer", "Return"), ["InsertNewline"]);
        assert_eq!(find(&s, "buffer", "alt+shift+g"), ["lua:foo.bar"]);
        assert_eq!(find(&s, "buffer", "ctrl+x ctrl+c"), ["Save,Quit"]);
        // In command only.
        assert_eq!(find(&s, "command", "ctrl+w"), ["WordLeft"]);
        assert_eq!(
            find(&s, "buffer", "ctrl+w"),
            find(&keymap("2.0.15").into_sheet(), "buffer", "ctrl+w")
        );
        // Mouse, raw, unknown, a key with no keysym, `http://x`, and two values that aren't strings: counted.
        assert_eq!(left_out, 7);
        assert_eq!(
            notes,
            [r#"bindings.json's "bogus" isn't a pane (buffer, command, terminal); ignored"#]
        );
    }

    #[test]
    fn config_dir() {
        let tmp = std::env::temp_dir().join(format!("kbf-micro-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in ["mch", "xdg/micro", "home/.config/micro"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
            std::fs::write(tmp.join(d).join("bindings.json"), "{}").unwrap();
        }
        let t = |p: &str| tmp.join(p).to_string_lossy().into_owned();
        let pick = |vars: Vec<(&str, String)>| {
            bindings_file(&|k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone()))
        };
        let home = ("HOME", t("home"));
        assert_eq!(
            pick(vec![
                home.clone(),
                ("XDG_CONFIG_HOME", t("xdg")),
                ("MICRO_CONFIG_HOME", t("mch"))
            ]),
            Some(tmp.join("mch/bindings.json"))
        );
        assert_eq!(
            pick(vec![home.clone(), ("XDG_CONFIG_HOME", t("xdg"))]),
            Some(tmp.join("xdg/micro/bindings.json"))
        );
        assert_eq!(
            pick(vec![
                home.clone(),
                ("XDG_CONFIG_HOME", String::new()),
                ("MICRO_CONFIG_HOME", String::new())
            ]),
            Some(tmp.join("home/.config/micro/bindings.json"))
        );
        assert_eq!(pick(vec![("HOME", t("nowhere"))]), None);
        // MICRO_CONFIG_HOME set with no file there: none, as micro (not the XDG one).
        assert_eq!(
            pick(vec![
                home.clone(),
                ("XDG_CONFIG_HOME", t("xdg")),
                ("MICRO_CONFIG_HOME", t("nowhere"))
            ]),
            None
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A stand-in `micro` printing `out`, in its own directory; returns the directory.
    fn fake_micro(name: &str, out: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!("kbf-micro-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process: a write handle another test thread's fork inherited would make
        // running the script fail with ETXTBSY (as `plugins.rs`'s `write_script` says).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("micro"))
            .arg(format!(
                "[ \"$*|$LC_ALL\" = '-version|C' ] || exit 9; printf '{out}'"
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
        let bin = fake_micro("ok", "Version: 2.0.14\\nCommit hash: x\\n");
        let dir = |d: &str| {
            let d = fixtures().join(d).to_string_lossy().into_owned();
            move |k: &str| (k == "MICRO_CONFIG_HOME").then(|| d.clone())
        };
        let (s, notes) = sheet_with(REGISTRY, path(&bin), &dir("config")).unwrap();
        assert_eq!(find(&s, "buffer", "ctrl+s"), ["SaveAll"]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[1].starts_with("leaving out 7 bindings.json bindings"),
            "{notes:?}"
        );
        // Not JSON: the defaults and one line.
        let (s, notes) = sheet_with(REGISTRY, path(&bin), &dir("bad")).unwrap();
        assert_eq!(find(&s, "buffer", "ctrl+c"), ["CopyLine|Copy"]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with("; showing micro's defaults") && !notes[0].contains('\n'),
            "{notes:?}"
        );
        // Not an object: the defaults and one line.
        let (_, notes) = sheet_with(REGISTRY, path(&bin), &dir("array")).unwrap();
        assert_eq!(
            notes,
            ["bindings.json isn't a JSON object; showing micro's defaults"]
        );
        // No file: the defaults, no line.
        let (_, notes) = sheet_with(REGISTRY, path(&bin), &dir("nowhere")).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        let _ = std::fs::remove_dir_all(&bin);
        // Another version: the note names the `.json` to add.
        let bin2 = fake_micro("newer", "Version: 2.0.16\\n");
        let (_, notes) = sheet_with(REGISTRY, path(&bin2), &dir("nowhere")).unwrap();
        assert_eq!(
            notes,
            ["micro 2.0.16: using the defaults of 2.0.15; add data/micro/2.0.16.json"]
        );
        let _ = std::fs::remove_dir_all(&bin2);
        // A micro printing nothing: the newest, with a note.
        let bin = fake_micro("silent", "");
        let (_, notes) = sheet_with(REGISTRY, path(&bin), &dir("nowhere")).unwrap();
        assert_eq!(
            notes,
            ["micro's version is unknown: using the defaults of 2.0.15"]
        );
        // A malformed pinned file: an error naming it.
        let err = sheet_with(&[("2.0.15", "{")], path(&bin), &dir("nowhere")).unwrap_err();
        assert!(
            format!("{err:#}").contains("data/micro/2.0.15.json"),
            "{err:#}"
        );
        let _ = std::fs::remove_dir_all(&bin);
        // No micro: an error.
        let empty = std::env::temp_dir().join(format!("kbf-micro-none-{}", std::process::id()));
        let err = sheet_with(REGISTRY, path(&empty), &dir("nowhere")).unwrap_err();
        assert!(
            format!("{err:#}").contains("running micro -version"),
            "{err:#}"
        );
    }

    #[test]
    fn real_micro_version() {
        let Ok(out) = Command::new("micro")
            .arg("-version")
            .env("LC_ALL", "C")
            .output()
        else {
            eprintln!("skipping: micro is not installed");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        // A build without a release version (`0.0.0-unknown`) has none: newest defaults, as unknown.
        if !text.contains("0.0.0") {
            let v = version_of(&text).unwrap_or_else(|| panic!("no version in {text:?}"));
            assert!(pinned::parse_version(&v).is_some(), "{v}");
        }
    }
}
