//! `$XDG_CONFIG_HOME/keybind-find/config.toml`: keybind-find's own keys (`[keys]`), checked so key search always has a
//! way out and no two keys clash. A config that fails is one line, and every key falls back to its default.
//! `[[plugin]]` stanzas declare parser plugins (`src/plugins.rs`); a bad one is one line and only it is skipped.
//! `[picker]` chooses the picker (`src/picker.rs`); a bad one is one line and the default is used.

use crate::sheet;
use anyhow::{bail, Context};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::PathBuf;

/// The keys in force, each a list of single chords in the crate's notation (modifiers in crate order, so a capture's
/// chord compares equal). `run`, `text_search` and `undo` are key search's (X chords); `key_search` and `scope` are
/// text search's (fzf `--bind` keys, see `fzf_key`).
#[derive(Debug, Clone, PartialEq)]
pub struct Keys {
    pub run: Vec<String>,
    pub text_search: Vec<String>,
    pub undo: Vec<String>,
    pub key_search: Vec<String>,
    pub scope: Vec<String>,
}

impl Default for Keys {
    fn default() -> Self {
        let v = |keys: &[&str]| keys.iter().map(|k| k.to_string()).collect();
        Keys {
            run: v(&["Return", "KP_Enter"]),
            text_search: v(&["Escape"]),
            undo: v(&["BackSpace"]),
            key_search: v(&["ctrl+k"]),
            scope: v(&["ctrl+s"]),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    keys: KeysFile,
    // Each stanza is checked on its own (`plugins_from`), so one bad stanza doesn't refuse the keys or the others.
    // Any shape: `[plugin]` (one bracket) is `plugins_from`'s note, not a refusal of the keys.
    #[serde(default, rename = "plugin")]
    _plugins: Option<toml::Value>,
    // Checked on its own (`picker_from`), so a bad `[picker]` doesn't refuse the keys.
    #[serde(default, rename = "picker")]
    _picker: Option<toml::Value>,
}

/// `[picker]`: which picker, and how it is run. Every field may be left out.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Picker {
    /// `None`: today's choice (fzf in a terminal on X11 when installed, else the dmenu-style command).
    pub kind: Option<PickerKind>,
    /// The dmenu-style or JSON frontend's argument list.
    pub command: Option<Vec<String>>,
    /// The argument list fzf runs in; keybind-find appends its own command to it.
    pub terminal: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PickerKind {
    Fzf,
    Dmenu,
    Json,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PickerFile {
    kind: Option<String>,
    command: Option<Vec<String>>,
    terminal: Option<Vec<String>>,
}

/// A `[[plugin]]` stanza: the program that turns `inputs` into a sheet, and the windows it applies to.
#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    /// The program column and turn-order name.
    pub name: String,
    /// A name on `PATH` or a path (`~/` expanded).
    pub program: String,
    /// Paths as written: `~` and a `*` in the last component are expanded when it runs.
    pub inputs: Vec<String>,
    /// `None`: no `match`, so it never runs.
    pub matches: Option<Match>,
}

/// `[plugin.match]`: each key given must match, ignoring case. `window_title` is a substring of the title.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Match {
    pub class: Option<Vec<String>>,
    pub program: Option<Vec<String>>,
    pub window_title: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginFile {
    name: String,
    program: String,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(rename = "match")]
    matches: Option<MatchFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchFile {
    class: Option<OneOrMany>,
    program: Option<OneOrMany>,
    window_title: Option<OneOrMany>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct KeysFile {
    run: Option<OneOrMany>,
    text_search: Option<OneOrMany>,
    undo: Option<OneOrMany>,
    key_search: Option<OneOrMany>,
    scope: Option<OneOrMany>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

/// The keys in force, and why the config was refused, if it was. `has_key` says whether the running layout has a
/// keysym (`text_search` must have one); `None` skips that check (no xmodmap: key search can't start anyway).
/// A refused stanza's reason is added to the refusal, so the header shows it too.
pub fn load(has_key: Option<&dyn Fn(&str) -> bool>) -> (Keys, Option<String>) {
    match config_path() {
        Some(path) => load_with_plugins(&path, has_key),
        None => (Keys::default(), None),
    }
}

/// `load_from`, with each skipped plugin stanza's note after the keys' own, then a refused `[picker]`'s.
fn load_with_plugins(
    path: &std::path::Path,
    has_key: Option<&dyn Fn(&str) -> bool>,
) -> (Keys, Option<String>) {
    let (keys, note) = load_from(path, has_key);
    let stanzas = std::fs::read_to_string(path)
        .map(|text| plugins_from(&text).1)
        .unwrap_or_default()
        .into_iter()
        .map(|why| format!("{}: {why}", path.display()));
    // A refused `[picker]` too, so the fzf header shows it (fzf is then the default picker in use).
    let picker = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| picker_from(&text).err())
        .map(|e| picker_refusal(path, &e));
    let notes: Vec<String> = note.into_iter().chain(stanzas).chain(picker).collect();
    (keys, (!notes.is_empty()).then(|| notes.join(" | ")))
}

/// The config's `[picker]`, and why it was refused (the default is then used). No config, or no `[picker]`, is the
/// default.
pub fn picker() -> (Picker, Option<String>) {
    match config_path() {
        Some(path) => picker_at(&path),
        None => (Picker::default(), None),
    }
}

fn picker_at(path: &std::path::Path) -> (Picker, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match picker_from(&text) {
            Ok(p) => (p, None),
            Err(e) => (Picker::default(), Some(picker_refusal(path, &e))),
        },
        // No config, or one that can't be read (`load` reports that): the default.
        Err(_) => (Picker::default(), None),
    }
}

/// A refused `[picker]` as one line (toml's own messages hold line breaks), for stderr and the fzf header.
fn picker_refusal(path: &std::path::Path, e: &anyhow::Error) -> String {
    let why = format!("{e:#}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!("{}: {why}; using the default picker", path.display())
}

/// `[picker]` from a config's text. A config that isn't TOML has no `[picker]` here: `load` reports it.
fn picker_from(text: &str) -> anyhow::Result<Picker> {
    #[derive(Deserialize)]
    struct Only {
        picker: Option<toml::Value>,
    }
    let Some(value) = toml::from_str::<Only>(text).ok().and_then(|f| f.picker) else {
        return Ok(Picker::default());
    };
    let f: PickerFile = value.try_into().context("[picker]")?;
    let kind = match f.kind.as_deref() {
        None => None,
        Some("fzf") => Some(PickerKind::Fzf),
        Some("dmenu") => Some(PickerKind::Dmenu),
        Some("json") => Some(PickerKind::Json),
        Some(k) => bail!("[picker] kind {k:?} is not \"fzf\", \"dmenu\" or \"json\""),
    };
    for (name, list) in [("command", &f.command), ("terminal", &f.terminal)] {
        if list
            .as_ref()
            .is_some_and(|l| l.first().is_none_or(|p| p.trim().is_empty()))
        {
            bail!("[picker] {name} is an empty argument list");
        }
    }
    match kind {
        Some(PickerKind::Json) if f.command.is_none() => {
            bail!("[picker] kind \"json\" needs a command (the frontend to run)")
        }
        Some(PickerKind::Fzf) if f.command.is_some() => {
            bail!("[picker] kind \"fzf\" takes a terminal, not a command")
        }
        _ => {}
    }
    Ok(Picker {
        kind,
        command: f.command,
        terminal: f.terminal,
    })
}

/// The plugins declared in the config, in file order; a missing or malformed config has none (`load` reports it).
pub fn plugins() -> Vec<Plugin> {
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| plugins_from(&text).0)
        .unwrap_or_default()
}

fn config_path() -> Option<PathBuf> {
    path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

/// Names a plugin can't take: the built-in providers' and keybind-find's own (both would mix with its entries).
fn reserved(name: &str) -> bool {
    std::iter::once(&"keybind-find")
        .chain(crate::providers::KNOWN)
        .any(|r| *r == name.to_lowercase())
}

/// The good stanzas, and one note per stanza skipped. Text that isn't TOML has none and no note (`load` says it).
fn plugins_from(text: &str) -> (Vec<Plugin>, Vec<String>) {
    #[derive(Deserialize)]
    struct Stanzas {
        plugin: Option<toml::Value>,
    }
    let Ok(file) = toml::from_str::<Stanzas>(text) else {
        return (Vec::new(), Vec::new());
    };
    let stanzas = match file.plugin {
        None => Vec::new(),
        Some(toml::Value::Array(stanzas)) => stanzas,
        Some(_) => {
            let why =
                "plugin: each plugin is a [[plugin]] stanza (two brackets); none run".to_string();
            return (Vec::new(), vec![why]);
        }
    };
    let (mut plugins, mut notes): (Vec<Plugin>, Vec<String>) = (Vec::new(), Vec::new());
    for (i, value) in stanzas.into_iter().enumerate() {
        let label = value
            .get("name")
            .and_then(|n| n.as_str())
            .map_or(format!("[[plugin]] {}", i + 1), |n| {
                format!("[[plugin]] {n:?}")
            });
        match plugin(value) {
            // Ignoring case: the program column would show both as one.
            Ok(p)
                if plugins
                    .iter()
                    .any(|q: &Plugin| q.name.to_lowercase() == p.name.to_lowercase()) =>
            {
                notes.push(format!("{label}: a second plugin of that name; skipped"))
            }
            Ok(p) => plugins.push(p),
            Err(e) => notes.push(format!(
                "{label}: {}; skipped",
                format!("{e:#}")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
        }
    }
    (plugins, notes)
}

fn plugin(value: toml::Value) -> anyhow::Result<Plugin> {
    let f: PluginFile = value.try_into().context("not valid")?;
    if f.name.trim().is_empty() || f.name.trim() != f.name || f.name.chars().any(char::is_control) {
        bail!("name must be non-empty, with no control characters or surrounding spaces");
    }
    if reserved(&f.name) {
        bail!("name {:?} is keybind-find's own", f.name);
    }
    if f.program.trim().is_empty() {
        bail!("program is empty");
    }
    // A hotkey's working directory is the window manager's: a relative path would depend on it.
    if f.program.contains('/') && !f.program.starts_with('/') && !f.program.starts_with("~/") {
        bail!(
            "program {:?}: a name on PATH, or a path starting with / or ~/",
            f.program
        );
    }
    if let Some(i) = f.inputs.iter().find(|i| {
        let (dir, last) = i.rsplit_once('/').unwrap_or(("", i));
        dir.contains('*') || last.matches('*').count() > 1
    }) {
        bail!("input {i:?}: only one `*`, in the last path component");
    }
    // As for `program`: relative to the window manager's working directory would mean nothing.
    if let Some(i) = f
        .inputs
        .iter()
        .find(|i| !i.starts_with('/') && !i.starts_with("~/"))
    {
        bail!("input {i:?}: a path starting with / or ~/");
    }
    let list = |v: Option<OneOrMany>| {
        v.map(|v| match v {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        })
    };
    let matches = f.matches.map(|m| Match {
        class: list(m.class),
        program: list(m.program),
        window_title: list(m.window_title),
    });
    // An empty list would never match and an empty title would match every window: both are mistakes.
    if let Some(m) = &matches {
        for (key, values) in [
            ("class", &m.class),
            ("program", &m.program),
            ("window_title", &m.window_title),
        ] {
            if values
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.iter().any(|s| s.trim().is_empty()))
            {
                bail!("match {key} is empty");
            }
            // Matched whole: `"kitty "` would never match.
            if let Some(s) = values.iter().flatten().find(|s| s.trim() != s.as_str()) {
                bail!("match {key} {s:?} has surrounding spaces");
            }
        }
    }
    let matches = matches
        // An empty `[plugin.match]` names no window: the same as none.
        .filter(|m| *m != Match::default());
    Ok(Plugin {
        name: f.name,
        program: f.program,
        inputs: f.inputs,
        matches,
    })
}

fn load_from(
    path: &std::path::Path,
    has_key: Option<&dyn Fn(&str) -> bool>,
) -> (Keys, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Keys::default(), None),
        Err(e) => {
            return (
                Keys::default(),
                Some(format!("{}: {e}; using the default keys", path.display())),
            )
        }
    };
    match parse(&text, has_key.unwrap_or(&|_| true)) {
        Ok(keys) => (keys, None),
        Err(e) => {
            let why = format!("{e:#}")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            (
                Keys::default(),
                Some(format!("{}: {why}; using the default keys", path.display())),
            )
        }
    }
}

/// `$XDG_CONFIG_HOME/keybind-find/config.toml` (absolute only, as the XDG spec says), else under `$HOME/.config`.
fn path(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => dir,
        None => PathBuf::from(home.filter(|v| !v.is_empty())?).join(".config"),
    };
    Some(base.join("keybind-find").join("config.toml"))
}

fn parse(text: &str, has_key: &dyn Fn(&str) -> bool) -> anyhow::Result<Keys> {
    let file: File = toml::from_str(text).context("not valid")?;
    let d = Keys::default();
    let take =
        |name: &str, v: Option<OneOrMany>, default: Vec<String>| -> anyhow::Result<Vec<String>> {
            match v {
                None => Ok(default),
                Some(OneOrMany::One(c)) => Ok(vec![
                    normalize(&c).with_context(|| format!("[keys] {name}"))?
                ]),
                Some(OneOrMany::Many(cs)) => {
                    let mut out: Vec<String> = Vec::new();
                    for c in cs {
                        let c = normalize(&c).with_context(|| format!("[keys] {name}"))?;
                        // `K` and `shift+k` are one chord: listed once.
                        if !out.contains(&c) {
                            out.push(c);
                        }
                    }
                    Ok(out)
                }
            }
        };
    let k = file.keys;
    let set = (k.run.is_some(), k.undo.is_some());
    let keys = Keys {
        run: take("run", k.run, d.run)?,
        text_search: take("text_search", k.text_search, d.text_search)?,
        undo: take("undo", k.undo, d.undo)?,
        key_search: take("key_search", k.key_search, d.key_search)?,
        scope: take("scope", k.scope, d.scope)?,
    };
    // A capture reports a layout's base keysyms only (`shift+slash`, never `question`): any other run or undo key would
    // never fire, while the header still offers it. Only the keys set here: a default is what a refusal falls back to.
    for (name, chords, set) in [("run", &keys.run, set.0), ("undo", &keys.undo, set.1)] {
        if let Some(c) = chords
            .iter()
            .find(|c| set && (is_modifier_alone(c) || !has_key(key_of(c))))
        {
            bail!("{name} {c} is not a key this keyboard layout reports (xmodmap -pke), so it would never fire");
        }
    }
    check(&keys, has_key)?;
    Ok(keys)
}

/// The rules that keep the keys apart and key search escapable.
fn check(k: &Keys, has_key: &dyn Fn(&str) -> bool) -> anyhow::Result<()> {
    let capture = [
        ("run", &k.run),
        ("text_search", &k.text_search),
        ("undo", &k.undo),
    ];
    for (i, (a, ak)) in capture.iter().enumerate() {
        for (b, bk) in &capture[i + 1..] {
            if let Some(c) = ak.iter().find(|c| bk.contains(c)) {
                bail!("conflict: {a} and {b} both use {c}");
            }
        }
    }
    if let Some(c) = k.text_search.iter().find(|c| is_modifier_alone(c)) {
        bail!("text_search {c} is a modifier alone, which can't end key search");
    }
    if !k.text_search.iter().any(|c| has_key(key_of(c))) {
        bail!(
            "text_search {} has no key in this keyboard layout (xmodmap -pke), so nothing could end key search",
            k.text_search.join(", ")
        );
    }
    let mut fzf: Vec<(&str, String)> = Vec::new();
    for (name, chords) in [("key_search", &k.key_search), ("scope", &k.scope)] {
        for c in chords {
            let f = fzf_key(c).with_context(|| format!("{name} {c} can't be sent to fzf"))?;
            if f == "enter" || f == "esc" {
                bail!("{name} {c} is text search's own run or close key");
            }
            match fzf.iter().find(|(_, g)| *g == f) {
                Some((other, _)) if *other != name => {
                    bail!("conflict: {other} and {name} both use {c}")
                }
                Some(_) => {}
                None => fzf.push((name, f)),
            }
        }
    }
    Ok(())
}

/// One chord in the crate's notation: known modifiers only, put in crate order; `K` is `shift+k`, as a capture reports
/// it. A sequence (`ctrl+b t`) or an empty part is refused.
fn normalize(chord: &str) -> anyhow::Result<String> {
    if chord.split_whitespace().count() != 1 || chord.trim() != chord {
        bail!("{chord:?} is not one chord");
    }
    let mut parts: Vec<&str> = chord.split('+').collect();
    let key = parts.pop().unwrap_or_default();
    if key.is_empty() || parts.iter().any(|p| p.is_empty()) {
        bail!("{chord:?} has an empty part");
    }
    if let Some(m) = parts.iter().find(|m| !MODS.contains(m)) {
        bail!("{chord:?}: unknown modifier {m:?} (super, ctrl, alt, shift)");
    }
    let lower;
    let key = if key.len() == 1 && key.as_bytes()[0].is_ascii_uppercase() {
        parts.push("shift");
        lower = key.to_ascii_lowercase();
        &lower
    } else {
        key
    };
    Ok(sheet::chord(&parts, key))
}

const MODS: [&str; 4] = ["super", "ctrl", "alt", "shift"];

/// The keysym of a normalized chord (`ctrl+k` → `k`).
pub fn key_of(chord: &str) -> &str {
    chord.rsplit('+').next().unwrap_or(chord)
}

/// A chord a capture never reports: a modifier on its own (`ctrl`, `Shift_L`).
fn is_modifier_alone(chord: &str) -> bool {
    let key = key_of(chord);
    MODS.contains(&key) || crate::capture::MODIFIER_KEYS.contains(&key)
}

/// X keysym → fzf key name, for the keys fzf names.
const FZF_NAMES: &[(&str, &str)] = &[
    ("Return", "enter"),
    ("Escape", "esc"),
    ("Tab", "tab"),
    ("space", "space"),
    ("BackSpace", "bspace"),
    // `del` alone, but fzf takes only `delete` after a modifier (`ctrl-delete`).
    ("Delete", "delete"),
    ("Insert", "insert"),
    ("Up", "up"),
    ("Down", "down"),
    ("Left", "left"),
    ("Right", "right"),
    ("Home", "home"),
    ("End", "end"),
    ("Prior", "page-up"),
    ("Next", "page-down"),
];

/// fzf's keys that take every modifier combination fzf has (`ctrl-alt-shift-up`).
const NAV: [&str; 9] = [
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "delete",
    "page-up",
    "page-down",
];

/// The fzf names of `chords` that have one, each once (`Tab` and `ctrl+i` are both `tab`).
pub fn fzf_keys(chords: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in chords.iter().filter_map(|c| fzf_key(c)) {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// A normalized chord as fzf's `--bind` key name (`ctrl+k` → `ctrl-k`, `F5` → `f5`), or `None` when fzf has no name
/// for it: any `super` chord (a terminal never sees Super), and keys or combinations missing from fzf's
/// "AVAILABLE KEYS" (`man fzf`).
pub fn fzf_key(chord: &str) -> Option<String> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let key = parts.pop()?;
    let has = |m: &str| parts.contains(&m);
    let (ctrl, alt, shift) = (has("ctrl"), has("alt"), has("shift"));
    if has("super") {
        return None;
    }
    let letter = key.len() == 1 && key.as_bytes()[0].is_ascii_lowercase();
    let digit = key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
    let fkey = key
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=12).contains(n));
    let named = FZF_NAMES.iter().find(|(x, _)| *x == key).map(|&(_, f)| f);
    let nav = named.filter(|n| NAV.contains(n));
    let name = match (ctrl, alt, shift) {
        (false, false, false) if letter || digit => key.to_string(),
        (false, false, false) => match fkey {
            Some(n) => format!("f{n}"),
            None => named?.to_string(),
        },
        (false, false, true) if letter => key.to_ascii_uppercase(),
        (false, false, true) if named == Some("tab") => "btab".to_string(),
        (true, false, false) if letter => format!("ctrl-{key}"),
        (true, false, false) => format!(
            "ctrl-{}",
            nav.or(named.filter(|n| ["space", "bspace"].contains(n)))?
        ),
        (false, true, false) if letter || digit => format!("alt-{key}"),
        (false, true, false) => format!(
            "alt-{}",
            nav.or(named.filter(|n| ["enter", "space", "bspace"].contains(n)))?
        ),
        (false, true, true) if letter => format!("alt-{}", key.to_ascii_uppercase()),
        (true, true, false) if letter => format!("ctrl-alt-{key}"),
        (true, true, false) => format!("ctrl-alt-{}", nav.or(named.filter(|&n| n == "bspace"))?),
        (false, false, true) => format!("shift-{}", nav?),
        (false, true, true) => format!("alt-shift-{}", nav?),
        (true, false, true) => format!("ctrl-shift-{}", nav?),
        (true, true, true) => format!("ctrl-alt-shift-{}", nav?),
    };
    // One key under two names: a terminal sends ctrl+m as Return, ctrl+i as Tab and ctrl+h as ctrl+BackSpace (and so
    // with alt). The real name, so the checks see the clash.
    let name = match name.as_str() {
        "ctrl-m" => "enter".to_string(),
        "ctrl-i" => "tab".to_string(),
        "ctrl-alt-h" => "ctrl-alt-bspace".to_string(),
        "ctrl-h" => "ctrl-bspace".to_string(),
        "ctrl-alt-m" => "alt-enter".to_string(),
        _ => name,
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(k: &str) -> bool {
        k != "NoSuchKey"
    }

    fn ok(text: &str) -> Keys {
        parse(text, &layout).unwrap()
    }

    fn refused(text: &str) -> String {
        format!("{:#}", parse(text, &layout).unwrap_err())
    }

    #[test]
    fn plugin_stanzas_parse() {
        let (plugins, notes) = plugins_from(
            r#"
[keys]
undo = "Delete"

[[plugin]]
name = "btop-keys"
program = "~/bin/btop-keys"
inputs = ["~/.config/btop/btop.conf", "~/.config/btop/themes/*.theme"]
match = { class = ["Alacritty", "kitty"], program = "btop" }

[[plugin]]
name = "gimp"
program = "gimp-keys"
[plugin.match]
window_title = "GIMP"

[[plugin]]
name = "unmatched"
program = "x"
"#,
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            plugins[0],
            Plugin {
                name: "btop-keys".into(),
                program: "~/bin/btop-keys".into(),
                inputs: vec![
                    "~/.config/btop/btop.conf".into(),
                    "~/.config/btop/themes/*.theme".into()
                ],
                matches: Some(Match {
                    class: Some(vec!["Alacritty".into(), "kitty".into()]),
                    program: Some(vec!["btop".into()]),
                    window_title: None,
                }),
            }
        );
        assert_eq!(
            plugins[1].matches.as_ref().unwrap().window_title,
            Some(vec!["GIMP".into()])
        );
        assert_eq!(plugins[2].matches, None);
        // The stanzas don't disturb the keys.
        assert_eq!(
            ok("[[plugin]]\nname = \"a\"\nprogram = \"b\"\n"),
            Keys::default()
        );
    }

    #[test]
    fn a_bad_stanza_is_one_note_and_only_it_is_skipped() {
        let (plugins, notes) = plugins_from(
            r#"
[[plugin]]
program = "no-name"

[[plugin]]
name = "no-program"

[[plugin]]
name = "bad-type"
program = 7

[[plugin]]
name = "unknown-key"
program = "x"
args = ["-v"]

[[plugin]]
name = "nvim"
program = "x"

[[plugin]]
name = "two-stars"
program = "x"
inputs = ["~/a*b*"]

[[plugin]]
name = "star-in-dir"
program = "x"
inputs = ["~/*/conf"]

[[plugin]]
name = " spaced"
program = "x"

[[plugin]]
name = "good"
program = "x"
match = { class = "Gimp" }

[[plugin]]
name = "good"
program = "y"

[[plugin]]
name = "empty-match"
program = "x"
[plugin.match]
"#,
        );
        assert_eq!(
            plugins.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["good", "empty-match"]
        );
        // An empty `[plugin.match]` is no match at all.
        assert_eq!(plugins[1].matches, None);
        assert_eq!(notes.len(), 9, "{notes:?}");
        for (note, want) in notes.iter().zip([
            "[[plugin]] 1: not valid",
            "[[plugin]] \"no-program\": not valid",
            "[[plugin]] \"bad-type\": not valid",
            "[[plugin]] \"unknown-key\": not valid",
            "[[plugin]] \"nvim\": name \"nvim\" is keybind-find's own",
            "[[plugin]] \"two-stars\": input",
            "[[plugin]] \"star-in-dir\": input",
            "[[plugin]] \" spaced\": name must be",
            "[[plugin]] \"good\": a second plugin of that name",
        ]) {
            assert!(note.starts_with(want), "{note}");
            assert!(note.ends_with("skipped"), "{note}");
            assert!(!note.contains('\n'), "{note}");
        }
        assert!(notes[0].contains("name"), "{}", notes[0]);
        // Not TOML: no plugins, and the keys' refusal says why.
        assert_eq!(plugins_from("[[plugin"), (vec![], vec![]));
    }

    #[test]
    fn stanza_mistakes_that_would_never_or_always_run() {
        let notes = |stanza: &str| plugins_from(&format!("[[plugin]]\n{stanza}\n")).1;
        for (stanza, want) in [
            (
                "name = \"a\"\nprogram = \"x\"\nmatch = { class = [] }",
                "match class is empty",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\nmatch = { window_title = \"\" }",
                "match window_title is empty",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\nmatch = { program = [\"btop\", \" \"] }",
                "match program is empty",
            ),
            ("name = \"NVIM\"\nprogram = \"x\"", "keybind-find's own"),
            (
                "name = \"Keybind-Find\"\nprogram = \"x\"",
                "keybind-find's own",
            ),
            (
                "name = \"a\"\nprogram = \"bin/x\"",
                "a name on PATH, or a path starting with / or ~/",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\ninputs = [\"btop.conf\"]",
                "a path starting with / or ~/",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\ninputs = [\"*.theme\"]",
                "a path starting with / or ~/",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\ninputs = [\"~user/x\"]",
                "a path starting with / or ~/",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\nmatch = { class = \"kitty \" }",
                "surrounding spaces",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\nmatch = { klass = \"x\" }",
                "not valid",
            ),
            (
                "name = \"a\"\nprogram = \"x\"\ninputs = \"~/one\"",
                "not valid",
            ),
        ] {
            let n = notes(stanza);
            assert!(n.len() == 1 && n[0].contains(want), "{stanza}: {n:?}");
        }
        // Absolute and home paths are fine, as is a bare name for PATH.
        for program in ["/usr/bin/x", "~/bin/x", "x"] {
            assert!(
                notes(&format!("name = \"a\"\nprogram = \"{program}\"")).is_empty(),
                "{program}"
            );
        }
        // Two names that differ only in case: the second is skipped.
        let (p, n) = plugins_from("[[plugin]]\nname = \"btop\"\nprogram = \"x\"\n[[plugin]]\nname = \"Btop\"\nprogram = \"y\"\n");
        assert_eq!((p.len(), n.len()), (1, 1), "{n:?}");
    }

    #[test]
    fn a_single_bracket_plugin_is_a_note_not_a_refusal() {
        for text in [
            "[keys]\nundo = \"Delete\"\n[plugin]\nname = \"a\"\n",
            "plugin = \"x\"\n[keys]\nundo = \"Delete\"\n",
        ] {
            assert_eq!(ok(text).undo, ["Delete"], "{text}");
            let (plugins, notes) = plugins_from(text);
            assert!(plugins.is_empty());
            assert_eq!(notes.len(), 1);
            assert!(
                notes[0].contains("[[plugin]] stanza (two brackets)"),
                "{notes:?}"
            );
        }
    }

    #[test]
    fn a_skipped_stanza_reaches_the_header_note_and_keeps_the_keys() {
        let dir = std::env::temp_dir().join(format!("kbf-config-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[keys]\nundo = \"Delete\"\n[[plugin]]\nname = \"a\"\n",
        )
        .unwrap();
        let (keys, note) = load_with_plugins(&path, None);
        assert_eq!(keys.undo, ["Delete"]);
        let note = note.unwrap();
        assert!(
            note.starts_with(&format!("{}: [[plugin]] \"a\": not valid", path.display())),
            "{note}"
        );
        // A refused [keys] and a bad stanza: both, keys first.
        std::fs::write(
            &path,
            "[keys]\nundo = \"hyper+x\"\n[[plugin]]\nname = \"a\"\n",
        )
        .unwrap();
        let (keys, note) = load_with_plugins(&path, None);
        assert_eq!(keys, Keys::default());
        let note = note.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(note.contains("using the default keys | "), "{note}");
        assert!(note.ends_with("skipped"), "{note}");
    }

    #[test]
    fn defaults_with_no_file_or_an_empty_one() {
        assert_eq!(ok(""), Keys::default());
        assert_eq!(ok("[keys]\n"), Keys::default());
        let d = Keys::default();
        assert_eq!(d.run, ["Return", "KP_Enter"]);
        assert_eq!(d.undo, ["BackSpace"]);
        // The defaults pass their own rules.
        assert!(check(&d, &layout).is_ok());
        // No config dir at all: the defaults, quietly.
        assert_eq!(path(None, None), None);
    }

    #[test]
    fn config_path_follows_xdg() {
        let p = |x: Option<&str>, h: Option<&str>| path(x.map(Into::into), h.map(Into::into));
        assert_eq!(
            p(Some("/x"), Some("/h")),
            Some(PathBuf::from("/x/keybind-find/config.toml"))
        );
        assert_eq!(
            p(Some("rel"), Some("/h")),
            Some(PathBuf::from("/h/.config/keybind-find/config.toml"))
        );
        assert_eq!(p(None, Some("")), None);
    }

    #[test]
    fn partial_keys_keep_the_rest_and_take_a_string_or_a_list() {
        let k = ok("[keys]\nundo = \"ctrl+u\"\nrun = [\"ctrl+j\", \"Return\"]\n");
        assert_eq!(k.undo, ["ctrl+u"]);
        assert_eq!(k.run, ["ctrl+j", "Return"]);
        assert_eq!(k.text_search, Keys::default().text_search);
        assert_eq!(k.key_search, ["ctrl+k"]);
        // The story's example is the defaults.
        let example = "[keys]\nrun = [\"Return\", \"KP_Enter\"]\ntext_search = \"Escape\"\nundo = \"BackSpace\"\n\
                       key_search = \"ctrl+k\"\nscope = \"ctrl+s\"\n";
        assert_eq!(ok(example), Keys::default());
    }

    #[test]
    fn chords_are_normalized_to_capture_order() {
        assert_eq!(normalize("shift+ctrl+x").unwrap(), "ctrl+shift+x");
        assert_eq!(normalize("K").unwrap(), "shift+k");
        assert_eq!(normalize("super+K").unwrap(), "super+shift+k");
        assert_eq!(normalize("BackSpace").unwrap(), "BackSpace");
        assert!(normalize("ctrl+b t").is_err());
        assert!(normalize("ctrl+").is_err());
        assert!(normalize("").is_err());
        assert!(normalize("hyper+k")
            .unwrap_err()
            .to_string()
            .contains("hyper"));
    }

    #[test]
    fn each_refusal_names_the_problem() {
        let e = refused("[keys]\nrun = \"Escape\"\n");
        assert!(
            e.contains("conflict: run and text_search both use Escape"),
            "{e}"
        );
        assert!(refused("[keys]\nundo = [\"KP_Enter\"]\n")
            .contains("conflict: run and undo both use KP_Enter"));
        assert!(refused("[keys]\nundo = \"Escape\"\n")
            .contains("conflict: text_search and undo both use Escape"));
        let e = refused("[keys]\nscope = \"ctrl+k\"\n");
        assert!(
            e.contains("conflict: key_search and scope both use ctrl+k"),
            "{e}"
        );
        // `K` and `shift+k` are one key to fzf.
        assert!(refused("[keys]\nkey_search = \"K\"\nscope = \"shift+k\"\n").contains("conflict:"));
        for k in ["Return", "Escape", "ctrl+m"] {
            for name in ["key_search", "scope"] {
                let e = refused(&format!("[keys]\n{name} = \"{k}\"\n"));
                assert!(e.contains("text search's own run or close key"), "{e}");
            }
        }
        // One key under two names.
        assert!(
            refused("[keys]\nkey_search = \"ctrl+h\"\nscope = \"ctrl+BackSpace\"\n")
                .contains("conflict:")
        );
        assert!(
            refused("[keys]\nkey_search = \"ctrl+alt+m\"\nscope = \"alt+Return\"\n")
                .contains("conflict:")
        );
        assert_eq!(
            fzf_keys(&["Tab".into(), "ctrl+i".into(), "super+k".into()]),
            ["tab"]
        );
        // Defaults aren't held to the layout: only the keys the file sets.
        let no_kp = |k: &str| k != "KP_Enter";
        assert!(parse("[keys]\nundo = \"ctrl+u\"\n", &no_kp).is_ok());
        assert!(parse("[keys]\nrun = [\"Return\", \"KP_Enter\"]\n", &no_kp).is_err());
        assert!(
            refused("[keys]\nkey_search = \"ctrl+alt+h\"\nscope = \"ctrl+alt+BackSpace\"\n")
                .contains("conflict:")
        );
        // A run or undo key no capture reports.
        for bad in [
            "undo = \"ctrl\"",
            "run = \"Control_L\"",
            "undo = \"NoSuchKey\"",
            "run = [\"Return\", \"NoSuchKey\"]",
        ] {
            let e = refused(&format!("[keys]\n{bad}\n"));
            assert!(e.contains("would never fire"), "{bad}: {e}");
        }
        let e = refused("[keys]\nscope = \"super+k\"\n");
        assert!(e.contains("scope super+k can't be sent to fzf"), "{e}");
        assert!(
            refused("[keys]\nkey_search = \"XF86AudioMute\"\n").contains("can't be sent to fzf")
        );
        let e = refused("[keys]\ntext_search = \"ctrl\"\n");
        assert!(e.contains("modifier alone"), "{e}");
        assert!(refused("[keys]\ntext_search = \"Shift_L\"\n").contains("modifier alone"));
        let e = refused("[keys]\ntext_search = \"NoSuchKey\"\n");
        assert!(e.contains("no key in this keyboard layout"), "{e}");
        assert!(refused("[keys]\ntext_search = []\n").contains("no key"));
        // One of several in the layout is enough.
        assert_eq!(
            ok("[keys]\ntext_search = [\"NoSuchKey\", \"F1\"]\n").text_search,
            ["NoSuchKey", "F1"]
        );
    }

    #[test]
    fn a_key_listed_twice_is_listed_once() {
        let k = ok("[keys]\nrun = [\"Return\", \"Return\"]\nkey_search = [\"K\", \"shift+k\"]\n");
        assert_eq!(k.run, ["Return"]);
        assert_eq!(k.key_search, ["shift+k"]);
    }

    #[test]
    fn malformed_toml_is_refused() {
        assert!(refused("[keys\n").contains("not valid"));
        assert!(refused("[keys]\nrun = 3\n").contains("not valid"));
        assert!(refused("[keys]\nudno = \"x\"\n").contains("udno"));
        assert!(refused("[key]\n").contains("not valid"));
        assert!(refused("[keys]\nrun = \"ctrl+b t\"\n").contains("[keys] run"));
    }

    #[test]
    fn a_refused_config_is_one_line_and_the_defaults() {
        let dir = std::env::temp_dir().join(format!("kbf-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        std::fs::write(&file, "[keys]\nrun = \"Escape\"\n").unwrap();
        let (keys, err) = load_from(&file, None);
        assert_eq!(keys, Keys::default());
        let err = err.unwrap();
        assert!(
            !err.contains('\n') && err.contains("conflict: run and text_search both use Escape"),
            "{err}"
        );
        std::fs::write(&file, "[keys]\nundo = \"ctrl+u\"\n").unwrap();
        assert_eq!(
            load_from(&file, None),
            (
                Keys {
                    undo: vec!["ctrl+u".into()],
                    ..Keys::default()
                },
                None
            )
        );
        std::fs::remove_file(&file).unwrap();
        assert_eq!(load_from(&file, None), (Keys::default(), None));
        // A TOML syntax error, which the toml crate prints over several lines: one line.
        std::fs::write(&file, "[keys\nrun = \"x\"\n").unwrap();
        let (keys, err) = load_from(&file, None);
        assert_eq!(keys, Keys::default());
        assert!(err.is_some_and(|e| !e.contains('\n') && e.contains("not valid")));
        std::fs::remove_file(&file).unwrap();
        // Unreadable (a directory): one line, the defaults.
        std::fs::create_dir(&file).unwrap();
        let (keys, err) = load_from(&file, None);
        assert_eq!(keys, Keys::default());
        assert!(err.is_some_and(|e| e.ends_with("using the default keys") && !e.contains('\n')));
        // The layout check is on when a layout is known.
        std::fs::remove_dir(&file).unwrap();
        std::fs::write(&file, "[keys]\ntext_search = \"F1\"\n").unwrap();
        let no_f1 = |k: &str| k != "F1";
        let (keys, err) = load_from(&file, Some(&no_f1));
        assert_eq!(keys, Keys::default());
        assert!(err.is_some_and(|e| e.contains("no key in this keyboard layout")));
        std::fs::remove_file(&file).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fzf_key_names() {
        let f = |c: &str| fzf_key(c);
        for (chord, want) in [
            ("ctrl+k", "ctrl-k"),
            ("alt+x", "alt-x"),
            ("F5", "f5"),
            ("F12", "f12"),
            ("Tab", "tab"),
            ("space", "space"),
            ("shift+Tab", "btab"),
            ("ctrl+space", "ctrl-space"),
            ("ctrl+alt+k", "ctrl-alt-k"),
            ("alt+Return", "alt-enter"),
            ("shift+k", "K"),
            ("alt+shift+k", "alt-K"),
            ("alt+1", "alt-1"),
            ("Prior", "page-up"),
            ("ctrl+shift+Up", "ctrl-shift-up"),
            ("BackSpace", "bspace"),
            ("ctrl+BackSpace", "ctrl-bspace"),
            ("Return", "enter"),
            ("Escape", "esc"),
            ("Delete", "delete"),
            ("ctrl+Delete", "ctrl-delete"),
            ("shift+Delete", "shift-delete"),
            ("ctrl+alt+Delete", "ctrl-alt-delete"),
            ("shift+Up", "shift-up"),
            ("alt+shift+Up", "alt-shift-up"),
            ("ctrl+alt+shift+Up", "ctrl-alt-shift-up"),
            ("ctrl+alt+BackSpace", "ctrl-alt-bspace"),
            ("ctrl+alt+h", "ctrl-alt-bspace"),
            ("alt+BackSpace", "alt-bspace"),
            ("alt+space", "alt-space"),
            ("7", "7"),
            ("Insert", "insert"),
            ("Home", "home"),
            ("End", "end"),
            ("Next", "page-down"),
            ("ctrl+m", "enter"),
            ("ctrl+i", "tab"),
            ("ctrl+h", "ctrl-bspace"),
            ("ctrl+alt+m", "alt-enter"),
        ] {
            assert_eq!(f(chord).as_deref(), Some(want), "{chord}");
        }
        for chord in [
            "super+k",
            "XF86AudioMute",
            "F13",
            "ctrl+shift+k",
            "ctrl+1",
            "slash",
            "shift+F1",
        ] {
            assert_eq!(f(chord), None, "{chord}");
        }
    }

    #[test]
    fn picker_kinds_and_refusals() {
        let v = |l: &[&str]| Some(l.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(picker_from("").unwrap(), Picker::default());
        assert_eq!(
            picker_from("[keys]\nundo = \"Delete\"").unwrap(),
            Picker::default()
        );
        // Not TOML: no [picker] here (`load` says why).
        assert_eq!(picker_from("[keys").unwrap(), Picker::default());
        assert_eq!(
            picker_from("[picker]\nkind = \"fzf\"\nterminal = [\"kitty\", \"--class\", \"keybind-find\", \"-e\"]")
                .unwrap(),
            Picker {
                kind: Some(PickerKind::Fzf),
                command: None,
                terminal: v(&["kitty", "--class", "keybind-find", "-e"]),
            }
        );
        // An argument with a space stays one argument.
        assert_eq!(
            picker_from(
                "[picker]\nkind = \"dmenu\"\ncommand = [\"rofi\", \"-dmenu\", \"-p\", \"a key\"]"
            )
            .unwrap()
            .command,
            v(&["rofi", "-dmenu", "-p", "a key"])
        );
        assert_eq!(
            picker_from("[picker]\nkind = \"json\"\ncommand = [\"my-frontend\"]")
                .unwrap()
                .kind,
            Some(PickerKind::Json)
        );
        // No kind: today's order, with the command or terminal given.
        assert_eq!(
            picker_from("[picker]\nterminal = [\"kitty\", \"-e\"]")
                .unwrap()
                .kind,
            None
        );
        for (text, want) in [
            ("[picker]\nkind = \"rofi\"", "kind \"rofi\" is not"),
            ("[picker]\nkind = \"json\"", "needs a command"),
            (
                "[picker]\nkind = \"fzf\"\ncommand = [\"x\"]",
                "takes a terminal, not a command",
            ),
            (
                "[picker]\ncommand = []",
                "command is an empty argument list",
            ),
            (
                "[picker]\nterminal = [\" \"]",
                "terminal is an empty argument list",
            ),
            ("[picker]\ncommand = \"rofi -dmenu\"", "[picker]"),
            ("[picker]\nkinds = \"fzf\"", "[picker]"),
            ("picker = 3", "[picker]"),
        ] {
            let e = format!("{:#}", picker_from(text).unwrap_err());
            assert!(e.contains(want), "{text}: {e}");
        }
    }

    #[test]
    fn a_refused_picker_keeps_the_keys_and_reaches_the_header_note() {
        let dir = std::env::temp_dir().join(format!("kbf-config-picker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[keys]\nundo = \"Delete\"\n[picker]\nkind = \"nope\"\n",
        )
        .unwrap();
        let (keys, note) = load_with_plugins(&path, None);
        // A string for a list: toml's message has line breaks; the note has none, here or from `picker_at`.
        std::fs::write(&path, "[picker]\ncommand = \"rofi -dmenu\"\n").unwrap();
        let (_, header) = load_with_plugins(&path, None);
        let (picker, line) = picker_at(&path);
        assert_eq!(picker, Picker::default());
        assert_eq!(
            picker_at(&dir.join("missing.toml")),
            (Picker::default(), None)
        );
        for n in [header.unwrap(), line.unwrap()] {
            assert!(
                !n.contains('\n') && n.contains("expected a sequence"),
                "{n}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(keys.undo, ["Delete"]);
        let note = note.unwrap();
        assert!(
            note.starts_with(&format!("{}: [picker] kind \"nope\"", path.display()))
                && note.ends_with("using the default picker"),
            "{note}"
        );
    }
}
