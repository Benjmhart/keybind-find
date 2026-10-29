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
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/kitty/0.49.2.toml"
    )),
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
    let (sheet, notes) = sheet_with(
        REGISTRY,
        version,
        Path::new("/etc/xdg/kitty/kitty.conf"),
        &env,
    )?;
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
/// then the system kitty.conf (`system`) and the user's found through `env`, in that order.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    version: anyhow::Result<String>,
    system: &Path,
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
        bail!("data/kitty/{v}.toml names kitty {}", pinned.kitty_version);
    }
    let mut lines = Vec::new();
    // As kitty's `resolve_config`: the system file, then the user's, each with its own memory of what it read (the
    // search can land on the system file itself: read once). No file (an unset `KITTY_CONFIG_DIRECTORY` holds none
    // yet): kitty's defaults, as kitty uses them.
    let canon = |f: &Path| std::fs::canonicalize(f).unwrap_or_else(|_| f.to_path_buf());
    let system = Some(system.to_path_buf()).filter(|f| f.exists());
    let user = config_file(env)
        .filter(|f| f.exists())
        .filter(|f| system.as_deref().map(canon) != Some(canon(f)));
    for conf in system.into_iter().chain(user) {
        Conf::new(env).read(&conf, 0, &mut lines, &mut notes);
    }
    let (sheet, left_out) = parse(&pinned.map, &lines);
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} maps (a hyper, caps_lock or num_lock modifier, a key with no X keysym, a mode no --new-mode defined or whose name can't be a scope, or --when-focus-on)"
        ));
    }
    Ok((sheet, notes))
}

/// The version in `kitty 0.49.2 created by Kovid Goyal` (`kitty 0.49.2 (abc) created by …` for a build from git).
fn version_of(text: &str) -> Option<String> {
    let v = text.split_whitespace().nth(1)?;
    pinned::parse_version(v).map(|_| v.to_string())
}

/// The user's kitty.conf, as kitty's launcher (`get_config_dir`) finds it: `$KITTY_CONFIG_DIRECTORY/kitty.conf` when
/// that is set, else the first `kitty/kitty.conf` that exists under `$XDG_CONFIG_HOME`, `~/.config`, then each
/// `$XDG_CONFIG_DIRS` entry. Empty or relative directories are skipped.
// ponytail: kitty also skips a directory the user can't write to (`access(W_OK)`); that needs `libc`, so a read-only
// config directory is still read here. Add the check with a decision-log entry for `libc` if that ever misleads.
fn config_file(env: Env) -> Option<PathBuf> {
    let abs = |d: &str| Some(PathBuf::from(d)).filter(|p| p.is_absolute());
    if let Some(dir) = env("KITTY_CONFIG_DIRECTORY").and_then(|d| abs(&d)) {
        return Some(dir.join("kitty.conf"));
    }
    let dirs = env("XDG_CONFIG_DIRS").unwrap_or_default();
    env("XDG_CONFIG_HOME")
        .and_then(|d| abs(&d))
        .into_iter()
        .chain(env("HOME").and_then(|h| abs(&h)).map(|h| h.join(".config")))
        .chain(dirs.split(':').filter_map(abs))
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
    fn read(
        &mut self,
        file: &Path,
        depth: usize,
        lines: &mut Vec<String>,
        notes: &mut Vec<String>,
    ) {
        let key = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        if self.seen.contains(&key) {
            notes.push(format!(
                "{} is included again (a cycle?); read once",
                file.display()
            ));
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
            "clear_all_shortcuts"
                if matches!(value.to_ascii_lowercase().as_str(), "y" | "yes" | "true") =>
            {
                maps.clear()
            }
            _ => {}
        }
    }
    let mut left_out = 0;
    let mut out: Vec<Out> = Vec::new();
    // Modes defined so far by `--new-mode`, in order.
    let mut modes: Vec<String> = Vec::new();
    for (title, map) in maps {
        // kitty drops these at parse time: they define no mode either.
        if kitty_drops(&map.shortcut) {
            left_out += usize::from(!map.action.is_empty() || !map.new_mode.is_empty());
            continue;
        }
        if !map.new_mode.is_empty() {
            // A mode defined again starts empty (kitty makes a new one).
            out.retain(|o| o.mode != map.new_mode);
            if !modes.contains(&map.new_mode) {
                modes.push(map.new_mode.clone());
            }
        }
        if map.when_focus_on {
            left_out += 1;
            continue;
        }
        let action = if map.new_mode.is_empty() {
            map.action
        } else {
            format!("push_keyboard_mode {}", map.new_mode)
        };
        // kitty ignores a map in a mode no `--new-mode` before it defined.
        if !map.mode.is_empty() && !modes.contains(&map.mode) {
            left_out += usize::from(!action.is_empty());
            continue;
        }
        let keys = notation(&map.shortcut, &kitty_mod);
        // Keys with no notation are told apart by what was written.
        let same = |o: &Out| {
            o.mode == map.mode && o.keys == keys && (keys.is_some() || o.shortcut == map.shortcut)
        };
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
        if is_kitty_mod(m) {
            // A `kitty_mod` kitty can't read (an unknown name, `none`) is no modifiers at all; one it reads but we
            // can't show leaves the map out.
            let names: Vec<&str> = kitty_mod.split('+').collect();
            if names.iter().all(|k| modifier(k).is_some() || unshowable(k)) {
                for k in names {
                    mods.push(modifier(k)?);
                }
            }
        } else {
            mods.push(modifier(m)?);
        }
    }
    Some(chord(&mods, &keysym(key)?))
}

/// Whether kitty drops this shortcut while reading the config (`parse_shortcut`): a modifier it doesn't know, or
/// `none` (its `parse_mods` gives up on it).
/// ponytail: an unknown key name is dropped by kitty too, but can't be told from an XKB name kitty looks up; such a map
/// is only left out (it still defines its `--new-mode`).
fn kitty_drops(shortcut: &str) -> bool {
    shortcut.split('>').any(|part| {
        let part = part
            .strip_suffix('+')
            .filter(|h| !h.is_empty())
            .unwrap_or(part);
        let mods: Vec<&str> = part.split('+').collect();
        mods[..mods.len().saturating_sub(1)]
            .iter()
            .any(|m| !(is_kitty_mod(m) || modifier(m).is_some() || unshowable(m)))
    })
}

fn is_kitty_mod(m: &str) -> bool {
    m.eq_ignore_ascii_case("kitty_mod") || m.eq_ignore_ascii_case("kitty")
}

/// A kitty modifier kitty reads but keybind-find's notation can't show.
fn unshowable(m: &str) -> bool {
    ["hyper", "caps_lock", "num_lock"].contains(&m.to_ascii_lowercase().as_str())
}

/// A kitty modifier as keybind-find's; `None` for one it can't show or kitty doesn't take (`none` included).
fn modifier(m: &str) -> Option<&'static str> {
    Some(match m.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "⌃" => "ctrl",
        "shift" | "⇧" => "shift",
        "alt" | "opt" | "option" | "⌥" => "alt",
        "super" | "cmd" | "command" | "⌘" => "super",
        "meta" => "meta",
        _ => return None,
    })
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
    if let Some(d) = name
        .strip_prefix("KP_")
        .filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitty")
    }

    /// A system kitty.conf that doesn't exist, so the tests don't read the machine's `/etc/xdg/kitty/kitty.conf`.
    fn no_system() -> PathBuf {
        fixtures().join("no-system.conf")
    }

    fn defaults() -> Pinned {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    fn find<'a>(s: &'a Sheet, key: &str) -> Vec<&'a Bind> {
        s.binds
            .values()
            .flatten()
            .filter(|b| b.key == key)
            .collect()
    }

    fn mode(b: &Bind) -> Option<String> {
        b.when
            .get("keyboard_mode")
            .map(|v| v.iter().map(|n| n.to_string()).collect())
    }

    #[test]
    fn pinned_rows_all_parse() {
        let pinned = defaults();
        assert_eq!(pinned.kitty_version, "0.49.2");
        // kitty 0.49.2's definition.py: 134 `map(` entries, 54 of them macOS-only and one a documentation example
        // (`add_to_default=False`): the 79 of types.py's default keymap.
        assert_eq!(pinned.map.len(), 134 - 54 - 1);
        let (sheet, left_out) = parse(&pinned.map, &[]);
        assert_eq!(left_out, 0);
        let binds: Vec<&Bind> = sheet.binds.values().flatten().collect();
        assert_eq!(binds.len(), pinned.map.len());
        assert_eq!(sheet.binds.keys().collect::<Vec<_>>(), ["kitty"]);
        assert!(binds
            .iter()
            .all(|b| b.when.is_empty() && !b.desc.is_empty()));
        // Keys a generator or a name table could mis-spell.
        for (key, desc) in [
            ("ctrl+shift+c", "Copy to clipboard"),
            ("shift+Insert", "Paste from selection"),
            ("ctrl+shift+Prior", "Scroll page up"),
            ("ctrl+shift+bracketright", "Next window"),
            ("ctrl+shift+bracketleft", "Previous window"),
            ("ctrl+shift+grave", "Move window to top"),
            ("ctrl+shift+slash", "Search the scrollback within a pager"),
            ("ctrl+shift+Return", "New window"),
            ("ctrl+shift+plus", "Increase font size"),
            ("ctrl+shift+KP_Add", "Increase font size"),
            ("ctrl+shift+KP_Subtract", "Decrease font size"),
            ("ctrl+shift+BackSpace", "Reset font size"),
            ("ctrl+shift+F11", "Toggle fullscreen"),
            ("ctrl+alt+shift+t", "Set tab title"),
            ("ctrl+shift+p shift+f", "Open selected path"),
            ("ctrl+shift+a m", "Increase background opacity"),
            ("ctrl+Tab", "Next tab"),
            ("ctrl+shift+Escape", "Open the kitty command shell"),
        ] {
            let got = find(&sheet, key);
            assert_eq!(got.len(), 1, "{key}");
            assert_eq!(got[0].desc, desc, "{key}");
        }
        assert_eq!(find(&sheet, "ctrl+shift+c")[0].action, "copy_to_clipboard");
    }

    #[test]
    fn config_over_defaults() {
        let dir = fixtures().join("config/kitty");
        let d = dir.to_string_lossy().into_owned();
        let env = |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(fixtures().join("config").to_string_lossy().into_owned()),
            "KITTY_TEST_DIR" => Some(d.clone()),
            _ => None,
        };
        let (s, notes) = sheet_with(
            REGISTRY,
            Ok("kitty 0.49.2 created by Kovid Goyal\n".into()),
            &no_system(),
            &env,
        )
        .unwrap();
        let one = |key: &str| {
            let got = find(&s, key);
            assert_eq!(got.len(), 1, "{key}: {got:?}");
            got[0].clone()
        };
        // kitty_mod is ctrl+alt for every map, though set last.
        assert!(find(&s, "ctrl+shift+v").is_empty());
        assert_eq!(one("ctrl+alt+v").desc, "Paste from clipboard");
        // Replaced: the action is the description. Same action: the default's title.
        let b = one("ctrl+alt+Return");
        assert_eq!(
            (b.desc.as_str(), b.action.as_str()),
            ("launch --cwd=current", "launch --cwd=current")
        );
        let copy = find(&s, "ctrl+alt+c");
        assert_eq!(copy.len(), 2, "{copy:?}");
        assert_eq!(
            (copy[0].desc.as_str(), mode(copy[0])),
            ("Copy to clipboard", None)
        );
        assert_eq!(
            (copy[1].action.as_str(), mode(copy[1]).as_deref()),
            ("quit", Some("resize"))
        );
        assert_eq!(one("ctrl+alt+equal").desc, "Increase font size");
        // Unmapped (kitty_mod+alt+t lands on the same keys, so the unmap removes it too), and no_op.
        assert!(find(&s, "ctrl+alt+t").is_empty());
        assert!(find(&s, "ctrl+alt+w").is_empty());
        // A sequence over a continuation line.
        assert_eq!(one("ctrl+a n").desc, "combine : new_window : next_layout");
        // The key entering a mode replaces the default on it; the mode's own keys carry it.
        let enter = one("ctrl+alt+r");
        assert_eq!(enter.action, "push_keyboard_mode resize");
        assert_eq!(mode(&enter), None);
        assert_eq!(mode(&one("h")).as_deref(), Some("resize"));
        assert_eq!(one("Escape").action, "pop_keyboard_mode");
        assert_eq!(mode(&one("Escape")).as_deref(), Some("resize"));
        assert_eq!(one("ctrl+alt+m").action, "push_keyboard_mode bad,mode");
        assert!(find(&s, "x").is_empty());
        // Left out: --when-focus-on, hyper, a mode that can't be a scope.
        assert!(find(&s, "ctrl+j").is_empty());
        assert!(s.binds.values().flatten().all(|b| !b.key.contains("hyper")));
        // Key names and modifier aliases.
        assert_eq!(one("Prior").action, "scroll_page_up");
        assert_eq!(one("KP_Add").action, "change_font_size all +2.0");
        assert_eq!(one("ctrl+bracketleft").action, "previous_tab");
        assert_eq!(one("super+ctrl+alt+F35").action, "no_such_action");
        // include (with ${VAR}) and globinclude, sorted; not a file the glob doesn't match.
        assert_eq!(one("F1").action, "show_kitty_doc overview");
        assert_eq!(one("F2").action, "next_tab");
        assert_eq!(one("F3").action, "close_tab");
        assert!(find(&s, "F4").is_empty());
        assert_eq!(s.binds["kitty"].len(), 88);
        // The documentation example kitty doesn't bind.
        assert!(s.binds["kitty"]
            .iter()
            .all(|b| !b.action.contains("Hello World")));
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(
            notes[0].ends_with("kitty.conf is included again (a cycle?); read once"),
            "{notes:?}"
        );
        assert_eq!(
            notes[1],
            "envinclude and geninclude lines aren't followed (geninclude runs a program)"
        );
        assert!(notes[2].starts_with("leaving out 3 maps ("), "{notes:?}");
    }

    #[test]
    fn clear_all_shortcuts_drops_what_came_before() {
        let env = |k: &str| {
            (k == "KITTY_CONFIG_DIRECTORY").then(|| {
                fixtures()
                    .join("clear/kitty")
                    .to_string_lossy()
                    .into_owned()
            })
        };
        let (s, notes) = sheet_with(
            REGISTRY,
            Ok("kitty 0.49.2 created by Kovid Goyal".into()),
            &no_system(),
            &env,
        )
        .unwrap();
        let keys: Vec<&str> = s.binds.values().flatten().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, ["ctrl+y"]);
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn modes_clear_and_kitty_mod_as_kitty_does() {
        let lines = |text: &str| logical_lines(text);
        let keys = |text: &str| {
            let (s, left_out) = parse(&[], &lines(text));
            let mut k: Vec<String> = s
                .binds
                .values()
                .flatten()
                .map(|b| {
                    format!(
                        "{}{}",
                        mode(b).map(|m| format!("{m}:")).unwrap_or_default(),
                        b.key
                    )
                })
                .collect();
            k.sort();
            (k, left_out)
        };
        // A mode used before its --new-mode is ignored (counted); defining it again empties it.
        assert_eq!(
            keys("map --mode m a quit\nmap --new-mode m f1\nmap --mode m b quit\nmap --new-mode m f2\nmap --mode m c quit"),
            (vec!["F1".to_string(), "F2".into(), "m:c".into()], 1)
        );
        // A --new-mode map kitty drops defines (or empties) nothing; one under --when-focus-on still defines its mode.
        assert_eq!(keys("map --new-mode m bogus+x\nmap --mode m h quit").1, 2);
        assert_eq!(
            keys("map --new-mode m f1\nmap --mode m h quit\nmap --new-mode m none+x\nmap --mode m j quit").0,
            ["F1", "m:h", "m:j"]
        );
        assert_eq!(
            keys("map --when-focus-on title:x --new-mode m f1\nmap --mode m h quit"),
            (vec!["m:h".to_string()], 1)
        );
        // Two different keys with no notation stay two (both counted).
        assert_eq!(keys("map hyper+x a\nmap hyper+y b").1, 2);
        // clear_all_shortcuts in any case of yes/y/true; not for no.
        for yes in ["yes", "YES", "y", "True"] {
            assert_eq!(
                keys(&format!("map f1 a\nclear_all_shortcuts {yes}\nmap f2 b")).0,
                ["F2"],
                "{yes}"
            );
        }
        assert_eq!(
            keys("map f1 a\nclear_all_shortcuts no\nmap f2 b").0,
            ["F1", "F2"]
        );
    }

    #[test]
    fn config_paths() {
        let tmp = std::env::temp_dir().join(format!("kbf-kitty-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in ["home/.config/kitty", "xdg/kitty", "dirs/kitty", "kcd"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
            std::fs::write(tmp.join(d).join("kitty.conf"), "").unwrap();
        }
        let t = |p: &str| tmp.join(p).to_string_lossy().into_owned();
        let pick = |vars: &[(&str, String)]| {
            config_file(&|k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone()))
        };
        let home = ("HOME", t("home"));
        assert_eq!(
            pick(std::slice::from_ref(&home)),
            Some(tmp.join("home/.config/kitty/kitty.conf"))
        );
        // ~/.config before XDG_CONFIG_DIRS, as kitty's launcher; an XDG_CONFIG_DIRS entry when neither other has one.
        assert_eq!(
            pick(&[home.clone(), ("XDG_CONFIG_DIRS", t("dirs"))]),
            Some(tmp.join("home/.config/kitty/kitty.conf"))
        );
        assert_eq!(
            pick(&[
                ("HOME", t("nowhere")),
                ("XDG_CONFIG_HOME", t("nowhere")),
                (
                    "XDG_CONFIG_DIRS",
                    format!("/nonexistent::rel:{}", t("dirs"))
                )
            ]),
            Some(tmp.join("dirs/kitty/kitty.conf"))
        );
        assert_eq!(
            pick(&[
                home.clone(),
                ("XDG_CONFIG_HOME", t("xdg")),
                ("XDG_CONFIG_DIRS", t("dirs"))
            ]),
            Some(tmp.join("xdg/kitty/kitty.conf"))
        );
        // KITTY_CONFIG_DIRECTORY wins, even with no file there yet; empty or relative values are unset.
        assert_eq!(
            pick(&[
                home.clone(),
                ("XDG_CONFIG_HOME", t("xdg")),
                ("KITTY_CONFIG_DIRECTORY", t("kcd"))
            ]),
            Some(tmp.join("kcd/kitty.conf"))
        );
        assert_eq!(
            pick(&[
                home.clone(),
                ("KITTY_CONFIG_DIRECTORY", String::new()),
                ("XDG_CONFIG_HOME", "rel".into())
            ]),
            Some(tmp.join("home/.config/kitty/kitty.conf"))
        );
        // KITTY_CONFIG_DIRECTORY with no kitty.conf in it yet: kitty's defaults, no note.
        let empty = tmp.join("empty").to_string_lossy().into_owned();
        let env = |k: &str| (k == "KITTY_CONFIG_DIRECTORY").then(|| empty.clone());
        let (s, notes) = sheet_with(
            REGISTRY,
            Ok("kitty 0.49.2 created by Kovid Goyal".into()),
            &no_system(),
            &env,
        )
        .unwrap();
        assert_eq!(s.binds["kitty"].len(), 79);
        assert!(notes.is_empty(), "{notes:?}");
        // None anywhere: no file, and the defaults alone with no note.
        assert_eq!(pick(&[("HOME", t("nowhere"))]), None);
        let env = |k: &str| (k == "HOME").then(|| t("nowhere"));
        let (s, notes) = sheet_with(
            REGISTRY,
            Ok("kitty 0.49.2 created by Kovid Goyal".into()),
            &no_system(),
            &env,
        )
        .unwrap();
        assert_eq!(s.binds["kitty"].len(), 79);
        assert!(notes.is_empty(), "{notes:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn system_file_underneath() {
        let tmp = std::env::temp_dir().join(format!("kbf-kitty-system-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        for d in ["etc/kitty", "home/.config/kitty"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        let t = |p: &str| tmp.join(p).to_string_lossy().into_owned();
        let shared = t("shared.conf");
        std::fs::write(&shared, "map f9 close_window\n").unwrap();
        let system = tmp.join("etc/kitty/kitty.conf");
        std::fs::write(
            &system,
            format!("kitty_mod ctrl+alt\nmap kitty_mod+f1 show_kitty_doc overview\nmap kitty_mod+f2 next_tab\ninclude {shared}\n"),
        )
        .unwrap();
        let user = tmp.join("home/.config/kitty/kitty.conf");
        let sheet = |vars: &[(&str, String)]| {
            let env = |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
            sheet_with(
                REGISTRY,
                Ok("kitty 0.49.2 created by Kovid Goyal".into()),
                &system,
                &env,
            )
            .unwrap()
        };
        let one = |s: &Sheet, key: &str| {
            let got = find(s, key);
            assert_eq!(got.len(), 1, "{key}: {got:?}");
            got[0].action.clone()
        };
        let home = [("HOME", t("home"))];

        // The user's map wins, the system's untouched map stays, the system's kitty_mod applies, and the include both
        // files read is read for each, with no note.
        std::fs::write(
            &user,
            format!("map kitty_mod+f1 next_layout\ninclude {shared}\n"),
        )
        .unwrap();
        let (s, notes) = sheet(&home);
        assert_eq!(one(&s, "ctrl+alt+F1"), "next_layout");
        assert_eq!(one(&s, "ctrl+alt+F2"), "next_tab");
        assert_eq!(one(&s, "ctrl+alt+v"), "paste_from_clipboard");
        assert_eq!(one(&s, "F9"), "close_window");
        assert!(notes.is_empty(), "{notes:?}");

        // The user's kitty_mod wins over the system's.
        std::fs::write(&user, "kitty_mod ctrl+shift\n").unwrap();
        let (s, _) = sheet(&home);
        assert_eq!(one(&s, "ctrl+shift+F2"), "next_tab");
        assert!(find(&s, "ctrl+alt+F2").is_empty());

        // The user's clear_all_shortcuts drops the system file's maps too.
        std::fs::write(&user, "clear_all_shortcuts yes\nmap f12 quit\n").unwrap();
        let (s, notes) = sheet(&home);
        let keys: Vec<&str> = s.binds.values().flatten().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, ["F12"]);
        assert!(notes.is_empty(), "{notes:?}");

        // The search lands on the system file itself: read once (one --when-focus-on map left out, not two).
        std::fs::write(
            &system,
            "map kitty_mod+f2 next_tab\nmap --when-focus-on title:x f3 quit\n",
        )
        .unwrap();
        let (s, notes) = sheet(&[("HOME", t("nowhere")), ("XDG_CONFIG_DIRS", t("etc"))]);
        assert_eq!(one(&s, "ctrl+shift+F2"), "next_tab");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("leaving out 1 maps ("), "{notes:?}");
        // The system file alone (the user's search finds nothing): its maps, no other note.
        let (s, notes) = sheet(&[("HOME", t("nowhere"))]);
        assert_eq!(one(&s, "ctrl+shift+F2"), "next_tab");
        assert_eq!(s.binds["kitty"].len(), 79);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("leaving out 1 maps ("), "{notes:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn version_and_missing_kitty() {
        assert_eq!(
            version_of("kitty 0.49.2 created by Kovid Goyal").as_deref(),
            Some("0.49.2")
        );
        assert_eq!(
            version_of("kitty 0.50.0 (abc1234567) created by Kovid Goyal").as_deref(),
            Some("0.50.0")
        );
        assert_eq!(version_of("something else"), None);
        let env = |_: &str| None;
        // No kitty: the newest defaults, the reason and the version note.
        let (s, notes) = sheet_with(
            REGISTRY,
            Err(anyhow::anyhow!("kitty isn't on PATH")),
            &no_system(),
            &env,
        )
        .unwrap();
        assert_eq!(s.binds["kitty"].len(), 79);
        assert_eq!(
            notes,
            [
                "kitty isn't on PATH",
                "kitty's version is unknown: using the defaults of 0.49.2"
            ]
        );
        let (_, notes) = sheet_with(
            REGISTRY,
            Ok("kitty 0.50.1 created by Kovid Goyal".into()),
            &no_system(),
            &env,
        )
        .unwrap();
        assert_eq!(
            notes,
            ["kitty 0.50.1: using the defaults of 0.49.2; add data/kitty/0.50.1.toml"]
        );
        let err = sheet_with(
            &[("0.49.2", "map = [")],
            Ok(String::new()),
            &no_system(),
            &env,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("data/kitty/0.49.2.toml"),
            "{err:#}"
        );
    }

    #[test]
    fn program_lookup() {
        let tmp = std::env::temp_dir().join(format!("kbf-kitty-program-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bin = tmp.join(".local/kitty.app/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(tmp.join("path")).unwrap();
        let write = |p: &Path, mode: u32| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(p, "").unwrap();
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        write(&bin.join("kitten"), 0o755);
        write(&tmp.join("path/kitten"), 0o644);
        let home = tmp.to_string_lossy().into_owned();
        let path = format!("rel:{}", tmp.join("path").display());
        let env = |k: &str| match k {
            "HOME" => Some(home.clone()),
            "PATH" => Some(path.clone()),
            _ => None,
        };
        // Not executable on PATH: the installer's place.
        assert_eq!(program("kitten", &env), Some(bin.join("kitten")));
        write(&tmp.join("path/kitten"), 0o755);
        assert_eq!(program("kitten", &env), Some(tmp.join("path/kitten")));
        assert_eq!(program("kitty", &env), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn map_values_and_key_names() {
        let m = split_map("--mode=resize --on-unknown end h resize_window narrower").unwrap();
        assert_eq!(
            (m.mode.as_str(), m.shortcut.as_str(), m.action.as_str()),
            ("resize", "h", "resize_window narrower")
        );
        assert_eq!(split_map("ctrl+x no-op").unwrap().action, "");
        assert_eq!(split_map("ctrl+x noop").unwrap().action, "");
        assert_eq!(split_map("ctrl+x").unwrap().action, "");
        assert!(split_map("--no-such-option x ctrl+x quit").is_none());
        assert!(split_map("").is_none());
        assert!(
            split_map("--when-focus-on 'title:a b' ctrl+x quit")
                .unwrap()
                .when_focus_on
        );
        assert_eq!(
            split_map("--when-focus-on 'title:a b' ctrl+x quit")
                .unwrap()
                .shortcut,
            "ctrl+x"
        );
        let n = |s: &str| notation(s, "ctrl+shift");
        assert_eq!(n("kitty_mod+c").as_deref(), Some("ctrl+shift+c"));
        assert_eq!(n("page_up").as_deref(), Some("Prior"));
        assert_eq!(n("kp_add").as_deref(), Some("KP_Add"));
        assert_eq!(n("[").as_deref(), Some("bracketleft"));
        assert_eq!(n("ctrl++").as_deref(), Some("ctrl+plus"));
        assert_eq!(n("ctrl+a>n").as_deref(), Some("ctrl+a n"));
        assert_eq!(n("control+opt+cmd+C").as_deref(), Some("super+ctrl+alt+c"));
        assert_eq!(n("⌃+⌥+⌘+⇧+x").as_deref(), Some("super+ctrl+alt+shift+x"));
        assert_eq!(n("meta+esc").as_deref(), Some("meta+Escape"));
        assert_eq!(
            n("ctrl+XF86AudioPlay").as_deref(),
            Some("ctrl+XF86AudioPlay")
        );
        assert_eq!(n("ctrl+semicolon").as_deref(), Some("ctrl+semicolon"));
        // `kitty` is kitty_mod too; a kitty_mod kitty can't read is no modifiers; `none` alone drops the map.
        assert_eq!(n("kitty+c").as_deref(), Some("ctrl+shift+c"));
        assert_eq!(notation("kitty_mod+c", "ctrl+bogus").as_deref(), Some("c"));
        assert_eq!(notation("kitty_mod+c", "hyper").as_deref(), None);
        // `none` anywhere, or a modifier kitty doesn't know, and kitty drops the map.
        assert_eq!(notation("kitty_mod+c", "ctrl+none").as_deref(), Some("c"));
        assert_eq!(notation("kitty_mod+c", "hyper+bogus").as_deref(), Some("c"));
        for dropped in [
            "none+a",
            "none+ctrl+a",
            "ctrl+none+a",
            "foo+x",
            "ctrl+a>none+b",
        ] {
            assert_eq!(n(dropped), None, "{dropped}");
            assert!(kitty_drops(dropped), "{dropped}");
        }
        for kept in [
            "hyper+x",
            "kitty_mod+c",
            "kitty+c",
            "ctrl++",
            "x",
            "ctrl+bogus",
        ] {
            assert!(!kitty_drops(kept), "{kept}");
        }
        for left_out in [
            "hyper+x",
            "caps_lock+x",
            "num_lock+x",
            "ctrl+0x61",
            "ctrl+é",
            "ctrl+bogus",
            "foo+x",
        ] {
            assert_eq!(n(left_out), None, "{left_out}");
        }
    }

    #[test]
    fn include_depth_and_expansion() {
        let tmp = std::env::temp_dir().join(format!("kbf-kitty-depth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        // 0.conf includes sub/1.conf, which includes 2.conf from its own directory, and so on.
        for i in 0..=10 {
            let dir = if i == 0 { tmp.clone() } else { tmp.join("sub") };
            std::fs::write(
                dir.join(format!("{i}.conf")),
                format!(
                    "map f{} quit\ninclude {}{}.conf\n",
                    i + 1,
                    if i == 0 { "sub/" } else { "" },
                    i + 1
                ),
            )
            .unwrap();
        }
        let env = |_: &str| None;
        let (mut lines, mut notes) = (Vec::new(), Vec::new());
        Conf::new(&env).read(&tmp.join("0.conf"), 0, &mut lines, &mut notes);
        assert_eq!(lines.len(), 9, "{lines:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("includes nest deeper than 8 at "),
            "{notes:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        let home = |k: &str| match k {
            "HOME" => Some("/h".to_string()),
            "A" => Some("x".to_string()),
            _ => None,
        };
        let c = Conf::new(&home);
        assert_eq!(
            c.expand("~/a/$A/${A}b/$$A/$NOPE/${KITTY_OS}"),
            "/h/a/x/xb/$A/$NOPE/linux"
        );
        assert_eq!(c.expand("~user/${"), "~user/${");
        assert!(
            wildcard("*.conf", "a.conf") && wildcard("a?c", "abc") && !wildcard("*.conf", "a.txt")
        );
    }

    #[test]
    fn real_kitty_version() {
        let env = |k: &str| std::env::var(k).ok();
        let Some(kitty) = program("kitty", &env) else {
            eprintln!("skipping: kitty is not installed");
            return;
        };
        let mut cmd = Command::new(kitty);
        cmd.arg("--version").env("LC_ALL", "C");
        let text = run_for(cmd, Duration::from_secs(10)).unwrap();
        assert!(version_of(&text).is_some(), "{text:?}");
    }
}
