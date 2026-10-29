//! kitty: its keyboard shortcuts, from defaults pinned per kitty version (`data/kitty/<version>.toml`, from
//! `kitty/options/definition.py` at that tag) with the user's kitty.conf `map` lines laid over them the way kitty reads
//! them (`kitty_mod`, `clear_all_shortcuts`, includes, keyboard modes). Shown when a kitty window has focus.

use super::nvim::single;
use super::pinned;
use crate::context::run_for;
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One entry per kitty version whose defaults we know, oldest first. Supporting a new kitty: add its file and a line.
const REGISTRY: &[(&str, &str)] = &[(
    "0.49.2",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/kitty/0.49.2.toml")),
)];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// How deep includes may nest; kitty itself stops only at a file it has already read.
const MAX_DEPTH: usize = 8;

pub(crate) type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pinned {
    kitty_version: String,
    map: Vec<PinnedMap>,
}

#[derive(Deserialize)]
struct PinnedMap {
    title: String,
    /// A `map` line's value: options, shortcut, action.
    map: String,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let version = match program("kitty", &env) {
        Some(kitty) => {
            let mut cmd = Command::new(kitty);
            cmd.arg("--version")
                .env("LC_ALL", "C")
                .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
            run_for(cmd, VERSION_TIMEOUT).context("running kitty --version")
        }
        None => Err(anyhow::anyhow!(
            "kitty isn't on PATH or in ~/.local/kitty.app/bin"
        )),
    };
    let (sheet, notes) = sheet_with(REGISTRY, version, &env)?;
    if !notes.is_empty() {
        eprintln!("keybind-find: kitty: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `name` (`kitty`, `kitten`) on `PATH`, else in `~/.local/kitty.app/bin`, where kitty's installer puts it.
pub(crate) fn program(name: &str, env: Env) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = env("PATH").unwrap_or_default();
    let installer = env("HOME").map(|h| Path::new(&h).join(".local/kitty.app/bin"));
    std::env::split_paths(&path)
        .chain(installer)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The sheet and its notes: the pinned defaults picked by `version` (`kitty --version`'s output, or why it failed),
/// then the user's kitty.conf found through `env`.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    version: anyhow::Result<String>,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut notes = Vec::new();
    let installed = match &version {
        Ok(text) => version_of(text),
        Err(e) => {
            notes.push(format!("{e:#}"));
            None
        }
    };
    let (v, text, note) = pinned::select("kitty", registry, installed.as_deref())
        .context("no pinned kitty defaults")?;
    notes.extend(note);
    let pinned: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/kitty/{v}.toml"))?;
    if pinned.kitty_version != v {
        bail!(
            "data/kitty/{v}.toml names kitty {}",
            pinned.kitty_version
        );
    }
    let mut lines = Vec::new();
    if let Some(conf) = config_file(env) {
        Conf::new(env).read(&conf, 0, &mut lines, &mut notes);
    }
    let (sheet, left_out) = parse(&pinned.map, &lines);
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} maps (a hyper, caps_lock or num_lock modifier, a key with no X keysym, a mode name that can't be a scope, or --when-focus-on)"
        ));
    }
    Ok((sheet, notes))
}

/// The version in `kitty 0.49.2 created by Kovid Goyal` (`kitty 0.49.2 (abc) created by …` for a build from git).
fn version_of(text: &str) -> Option<String> {
    let v = text.split_whitespace().nth(1)?;
    pinned::parse_version(v).map(|_| v.to_string())
}

/// kitty.conf: `$KITTY_CONFIG_DIRECTORY/kitty.conf` when that is set, else the first `kitty/kitty.conf` that exists
/// under `$XDG_CONFIG_HOME`, each `$XDG_CONFIG_DIRS` entry, then `~/.config`. Empty or relative directories are
/// skipped.
fn config_file(env: Env) -> Option<PathBuf> {
    let abs = |d: &str| Some(PathBuf::from(d)).filter(|p| p.is_absolute());
    if let Some(dir) = env("KITTY_CONFIG_DIRECTORY").and_then(|d| abs(&d)) {
        return Some(dir.join("kitty.conf"));
    }
    let dirs = env("XDG_CONFIG_DIRS").unwrap_or_default();
    env("XDG_CONFIG_HOME")
        .and_then(|d| abs(&d))
        .into_iter()
        .chain(dirs.split(':').filter_map(abs))
        .chain(env("HOME").and_then(|h| abs(&h)).map(|h| h.join(".config")))
        .map(|d| d.join("kitty/kitty.conf"))
        .find(|f| f.is_file())
}

/// Reads a kitty.conf into its logical lines, following `include` and `globinclude` as kitty does.
struct Conf<'a> {
    env: Env<'a>,
    /// Every file read so far: kitty reads a file once and ignores a second include of it.
    seen: Vec<PathBuf>,
    /// Whether `envinclude` or `geninclude` was already noted.
    noted: bool,
}

impl<'a> Conf<'a> {
    fn new(env: Env<'a>) -> Self {
        Conf {
            env,
            seen: Vec::new(),
            noted: false,
        }
    }

    /// Appends `file`'s lines (with continuation lines joined, comments and blank lines dropped) to `lines`, an
    /// include's lines in its place. `depth` is how many includes led here.
    fn read(&mut self, file: &Path, depth: usize, lines: &mut Vec<String>, notes: &mut Vec<String>) {
        let key = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        if self.seen.contains(&key) {
            notes.push(format!("{} is included again (a cycle?); read once", file.display()));
            return;
        }
        // Lossily, as kitty reads with `errors='replace'`.
        let text = match std::fs::read(file) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) => {
                notes.push(format!("reading {}: {e}", file.display()));
                return;
            }
        };
        self.seen.push(key);
        let dir = file.parent().unwrap_or(Path::new("/"));
        for line in logical_lines(&text) {
            let (key, value) = split_word(&line);
            let value = value.trim();
            match key {
                "include" | "globinclude" => {
                    let pattern = self.expand(value);
                    let files = if key == "include" {
                        vec![dir.join(pattern)]
                    } else {
                        glob(dir, &pattern)
                    };
                    for f in files {
                        if depth + 1 > MAX_DEPTH {
                            notes.push(format!(
                                "includes nest deeper than {MAX_DEPTH} at {}; not read",
                                f.display()
                            ));
                            continue;
                        }
                        self.read(&f, depth + 1, lines, notes);
                    }
                }
                "envinclude" | "geninclude" => {
                    if !self.noted {
                        self.noted = true;
                        notes.push(
                            "envinclude and geninclude lines aren't followed (geninclude runs a program)"
                                .into(),
                        );
                    }
                }
                _ => lines.push(line),
            }
        }
    }

    /// An include's value with `~` and `$VAR`/`${VAR}` expanded (`$$` is a `$`; `KITTY_OS` is `linux`; an unset
    /// variable is left as written), as kitty's `expandvars(expanduser(…))`.
    fn expand(&self, value: &str) -> String {
        let home = || (self.env)("HOME").unwrap_or_default();
        let value = match value.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home()),
            _ => value.to_string(),
        };
        let var = |name: &str| match name {
            "KITTY_OS" => Some("linux".to_string()),
            _ => (self.env)(name),
        };
        let mut out = String::new();
        let mut rest = value.as_str();
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            let tail = &rest[i + 1..];
            if let Some(r) = tail.strip_prefix('$') {
                out.push('$');
                rest = r;
                continue;
            }
            let (name, after) = match tail.strip_prefix('{') {
                Some(r) => match r.find('}') {
                    Some(end) if end > 0 => (&r[..end], &r[end + 1..]),
                    _ => ("", tail),
                },
                None => {
                    let end = tail
                        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .unwrap_or(tail.len());
                    (&tail[..end], &tail[end..])
                }
            };
            match Some(name).filter(|n| !n.is_empty()).and_then(var) {
                Some(v) => out.push_str(&v),
                None => {
                    out.push('$');
                    out.push_str(&tail[..tail.len() - after.len()]);
                }
            }
            rest = after;
        }
        out.push_str(rest);
        out
    }
}

/// kitty's logical lines: a line starting (after spaces) with `\` continues the one before; then blank lines and
/// comments are dropped and each line is trimmed.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        match (line.strip_prefix('\\'), out.last_mut()) {
            (Some(more), Some(last)) => last.push_str(more),
            _ => out.push(line.to_string()),
        }
    }
    out.into_iter()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// The first word of `s` and the rest after it (leading spaces skipped). A word starting with a quote runs to the
/// matching quote and loses them, as kitty's `Shlex` reads a map's options.
fn split_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    if let Some(q) = s.chars().next().filter(|c| *c == '\'' || *c == '"') {
        if let Some(end) = s[1..].find(q) {
            return (&s[1..1 + end], &s[end + 2..]);
        }
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// The files `pattern` (`*` and `?` in any part; no `**` or `[…]`) names under `dir`, sorted, as kitty's
/// `globinclude`.
/// ponytail: `*` and `?` only; a `[…]` class or `**` matches nothing. Add them if a config needs them.
fn glob(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut found = vec![dir.to_path_buf()];
    for part in pattern.split('/').filter(|p| !p.is_empty() && *p != ".") {
        found = found
            .into_iter()
            .flat_map(|d| {
                if !part.contains(['*', '?']) {
                    return vec![d.join(part)];
                }
                let mut m: Vec<PathBuf> = std::fs::read_dir(&d)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| wildcard(part, &e.file_name().to_string_lossy()))
                    .map(|e| e.path())
                    .collect();
                m.sort();
                m
            })
            .collect();
    }
    found.retain(|f| f.is_file());
    found.sort();
    found
}

/// Whether `name` matches `pattern`, where `*` is any run and `?` any one character.
fn wildcard(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // The classic two-pointer match with backtracking to the last `*`.
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|c| *c == '*')
}

/// One `map` line's parts.
#[derive(Debug, Default, PartialEq)]
struct Map {
    mode: String,
    new_mode: String,
    when_focus_on: bool,
    shortcut: String,
    action: String,
}

/// A `map` value (`[--option[=]value…] <shortcut> [action…]`), as kitty's `parse_map`: `None` for an unknown option or
/// no shortcut (kitty ignores the line). A `noop` action is no action.
fn split_map(value: &str) -> Option<Map> {
    let mut map = Map::default();
    let mut rest = value.trim_start();
    while rest.starts_with("--") {
        let (word, after) = split_word(rest);
        let (name, val, after) = match word[2..].split_once('=') {
            Some((n, v)) => (n, v, after),
            None => {
                let (v, a) = split_word(after);
                (&word[2..], v, a)
            }
        };
        match name.replace('-', "_").as_str() {
            "mode" => map.mode = val.to_string(),
            "new_mode" => map.new_mode = val.to_string(),
            "when_focus_on" => map.when_focus_on = true,
            "on_unknown" | "on_action" | "timeout" | "allow_fallback" => {}
            _ => return None,
        }
        rest = after.trim_start();
    }
    let (shortcut, action) = split_word(rest);
    map.shortcut = shortcut.trim_matches('>').to_string();
    let action = action.trim();
    map.action = if ["noop", "no-op", "no_op"].contains(&action) {
        String::new()
    } else {
        action.to_string()
    };
    (!map.shortcut.is_empty()).then_some(map)
}

/// One bind on the way out: its mode, key notation, action and title.
struct Out {
    mode: String,
    keys: Option<String>,
    shortcut: String,
    action: String,
    title: Option<String>,
}

/// The sheet from the pinned `defaults` and the config's logical `lines`, and how many maps were left out. Like kitty
/// (`finalize_keys`): `clear_all_shortcuts yes` drops every map before it, `kitty_mod`'s last value applies to every
/// map, and a later map of the same keys in the same mode replaces the earlier one (with no action, removes it).
fn parse(defaults: &[PinnedMap], lines: &[String]) -> (Sheet, usize) {
    let mut maps: Vec<(Option<&str>, Map)> = defaults
        .iter()
        .filter_map(|d| Some((Some(d.title.as_str()), split_map(&d.map)?)))
        .collect();
    let mut kitty_mod = "ctrl+shift".to_string();
    for line in lines {
        let (key, value) = split_word(line);
        let value = value.trim();
        match key {
            "map" => maps.extend(split_map(value).map(|m| (None, m))),
            "kitty_mod" => kitty_mod = value.to_string(),
            "clear_all_shortcuts" if matches!(value, "y" | "yes" | "true") => maps.clear(),
            _ => {}
        }
    }
    let mut left_out = 0;
    let mut out: Vec<Out> = Vec::new();
    for (title, map) in maps {
        if map.when_focus_on {
            left_out += 1;
            continue;
        }
        let action = if map.new_mode.is_empty() {
            map.action
        } else {
            format!("push_keyboard_mode {}", map.new_mode)
        };
        let keys = notation(&map.shortcut, &kitty_mod);
        // Keys with no notation are told apart by what was written.
        let same = |o: &Out| o.mode == map.mode && o.keys == keys && (keys.is_some() || o.shortcut == map.shortcut);
        let at = out.iter().position(same);
        if action.is_empty() {
            if let Some(i) = at {
                out.remove(i);
            }
            continue;
        }
        let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        // A default's title stays while the action is the one it titles.
        let title = title.map(str::to_string).or_else(|| {
            at.and_then(|i| (words(&out[i].action) == words(&action)).then(|| out[i].title.clone()))
                .flatten()
        });
        let o = Out {
            mode: map.mode,
            keys,
            shortcut: map.shortcut,
            action,
            title,
        };
        match at {
            Some(i) => out[i] = o,
            None => out.push(o),
        }
    }
    let mut sheet = Sheet {
        title: "kitty".into(),
        provider: "kitty".into(),
        binds: Default::default(),
    };
    let scope: Option<Name> = "keyboard_mode".parse().ok();
    for o in out {
        let Some(key) = o.keys else {
            left_out += 1;
            continue;
        };
        let mut when = std::collections::BTreeMap::new();
        if !o.mode.is_empty() {
            match (scope.clone(), o.mode.parse::<Name>()) {
                (Some(s), Ok(m)) => {
                    when.insert(s, Values::from(m));
                }
                _ => {
                    left_out += 1;
                    continue;
                }
            }
        }
        sheet.binds.entry("kitty".into()).or_default().push(Bind {
            key,
            desc: o.title.unwrap_or_else(|| o.action.clone()),
            action: o.action,
            when,
        });
    }
    (sheet, left_out)
}

/// A shortcut (`kitty_mod+c`, `ctrl+a>n`) in keybind-find's notation, `kitty_mod` standing for its value; `None` for
/// a hyper, caps_lock, num_lock or unknown modifier, or a key with no X keysym.
fn notation(shortcut: &str, kitty_mod: &str) -> Option<String> {
    let keys = shortcut
        .split('>')
        .map(|part| single_key(part, kitty_mod))
        .collect::<Option<Vec<_>>>()?;
    Some(keys.join(" "))
}

fn single_key(sc: &str, kitty_mod: &str) -> Option<String> {
    // kitty reads a trailing `+` as the plus key (`ctrl++`).
    let sc = match sc.strip_suffix('+') {
        Some(head) if !head.is_empty() => format!("{head}plus"),
        _ => sc.to_string(),
    };
    let parts: Vec<&str> = sc.split('+').collect();
    let (key, mod_names) = parts.split_last()?;
    let mut mods = Vec::new();
    for m in mod_names {
        if m.eq_ignore_ascii_case("kitty_mod") {
            for k in kitty_mod.split('+') {
                mods.push(modifier(k)?);
            }
        } else {
            mods.push(modifier(m)?);
        }
    }
    let mods: Vec<&str> = mods.into_iter().flatten().collect();
    Some(chord(&mods, &keysym(key)?))
}

/// A kitty modifier as keybind-find's: `Some(None)` for `none`, `None` for one it can't show (or doesn't know).
fn modifier(m: &str) -> Option<Option<&'static str>> {
    Some(Some(match m.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "⌃" => "ctrl",
        "shift" | "⇧" => "shift",
        "alt" | "opt" | "option" | "⌥" => "alt",
        "super" | "cmd" | "command" | "⌘" => "super",
        "meta" => "meta",
        "none" => return Some(None),
        _ => return None,
    }))
}

/// A kitty key name or character (kitty 0.49.2 `key_names.py` aliases, `GLFW_FKEY_*` names) as an X keysym; `None`
/// for a native code (`0x…`), a non-ASCII character, or a name kitty would look up in XKB.
/// ponytail: XKB names kitty looks up itself are left out, except `XF86…` ones, kept as written.
fn keysym(key: &str) -> Option<String> {
    let upper = key.to_ascii_uppercase();
    let alias = match upper.as_str() {
        "SPC" | "SPACE" => Some(' '),
        "STAR" | "MULTIPLY" => Some('*'),
        "PLUS" => Some('+'),
        "MINUS" | "HYPHEN" => Some('-'),
        "BAR" | "PIPE" => Some('|'),
        "EQUAL" => Some('='),
        "UNDERSCORE" => Some('_'),
        "COMMA" => Some(','),
        "PERIOD" | "DOT" => Some('.'),
        "SLASH" => Some('/'),
        "BACKSLASH" => Some('\\'),
        "TILDE" => Some('~'),
        "GRAVE" | "GRAVE_ACCENT" => Some('`'),
        "APOSTROPHE" => Some('\''),
        "SEMICOLON" => Some(';'),
        "COLON" => Some(':'),
        "LEFT_BRACKET" => Some('['),
        "RIGHT_BRACKET" => Some(']'),
        _ => None,
    };
    let mut chars = key.chars();
    let c = alias.or(match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_ascii_lowercase()),
        _ => None,
    });
    if let Some(c) = c {
        return single(c).map(str::to_string);
    }
    let name = match upper.as_str() {
        "ESC" => "ESCAPE",
        "PGUP" | "PAGEUP" => "PAGE_UP",
        "PGDN" | "PAGEDOWN" => "PAGE_DOWN",
        "RETURN" => "ENTER",
        "ARROWUP" => "UP",
        "ARROWDOWN" => "DOWN",
        "ARROWRIGHT" => "RIGHT",
        "ARROWLEFT" => "LEFT",
        "DEL" => "DELETE",
        "KP_PLUS" => "KP_ADD",
        "KP_MINUS" => "KP_SUBTRACT",
        other => other,
    };
    if let Some(n) = name
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=35).contains(n))
    {
        return Some(format!("F{n}"));
    }
    if let Some(d) = name.strip_prefix("KP_").filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit()) {
        return Some(format!("KP_{d}"));
    }
    let x = match name {
        "ESCAPE" => "Escape",
        "ENTER" => "Return",
        "TAB" => "Tab",
        "BACKSPACE" => "BackSpace",
        "INSERT" => "Insert",
        "DELETE" => "Delete",
        "LEFT" => "Left",
        "RIGHT" => "Right",
        "UP" => "Up",
        "DOWN" => "Down",
        "PAGE_UP" => "Prior",
        "PAGE_DOWN" => "Next",
        "HOME" => "Home",
        "END" => "End",
        "CAPS_LOCK" => "Caps_Lock",
        "SCROLL_LOCK" => "Scroll_Lock",
        "NUM_LOCK" => "Num_Lock",
        "PRINT_SCREEN" => "Print",
        "PAUSE" => "Pause",
        "MENU" => "Menu",
        "KP_DECIMAL" => "KP_Decimal",
        "KP_DIVIDE" => "KP_Divide",
        "KP_MULTIPLY" => "KP_Multiply",
        "KP_SUBTRACT" => "KP_Subtract",
        "KP_ADD" => "KP_Add",
        "KP_ENTER" => "KP_Enter",
        "KP_EQUAL" => "KP_Equal",
        "KP_SEPARATOR" => "KP_Separator",
        "KP_LEFT" => "KP_Left",
        "KP_RIGHT" => "KP_Right",
        "KP_UP" => "KP_Up",
        "KP_DOWN" => "KP_Down",
        "KP_PAGE_UP" => "KP_Prior",
        "KP_PAGE_DOWN" => "KP_Next",
        "KP_HOME" => "KP_Home",
        "KP_END" => "KP_End",
        "KP_INSERT" => "KP_Insert",
        "KP_DELETE" => "KP_Delete",
        "KP_BEGIN" => "KP_Begin",
        "MEDIA_PLAY" | "MEDIA_PLAY_PAUSE" => "XF86AudioPlay",
        "MEDIA_PAUSE" => "XF86AudioPause",
        "MEDIA_STOP" => "XF86AudioStop",
        "MEDIA_FAST_FORWARD" => "XF86AudioForward",
        "MEDIA_REWIND" => "XF86AudioRewind",
        "MEDIA_TRACK_NEXT" => "XF86AudioNext",
        "MEDIA_TRACK_PREVIOUS" => "XF86AudioPrev",
        "MEDIA_RECORD" => "XF86AudioRecord",
        "LOWER_VOLUME" => "XF86AudioLowerVolume",
        "RAISE_VOLUME" => "XF86AudioRaiseVolume",
        "MUTE_VOLUME" => "XF86AudioMute",
        "LEFT_SHIFT" => "Shift_L",
        "LEFT_CONTROL" => "Control_L",
        "LEFT_ALT" => "Alt_L",
        "LEFT_SUPER" => "Super_L",
        "LEFT_HYPER" => "Hyper_L",
        "LEFT_META" => "Meta_L",
        "RIGHT_SHIFT" => "Shift_R",
        "RIGHT_CONTROL" => "Control_R",
        "RIGHT_ALT" => "Alt_R",
        "RIGHT_SUPER" => "Super_R",
        "RIGHT_HYPER" => "Hyper_R",
        "RIGHT_META" => "Meta_R",
        "ISO_LEVEL3_SHIFT" => "ISO_Level3_Shift",
        "ISO_LEVEL5_SHIFT" => "ISO_Level5_Shift",
        _ if key.starts_with("XF86") => key,
        _ => return None,
    };
    Some(x.to_string())
}
