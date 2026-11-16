//! zellij: its keys in every mode, from the installed zellij's own defaults with the user's `config.kdl` applied the
//! way zellij applies it. From 0.46 the defaults are a keybinding preset (`zellij setup --dump-keybinds <preset>`, the
//! preset and its leader keys chosen in `config.kdl`); before, the `keybinds` of `zellij setup --dump-config`. Each
//! bind is tagged with its mode (`input_mode`): zellij has no query for a client's mode, so every mode is listed.
//! Shown when a terminal's foreground program is zellij.
//!
//! ponytail: a user preset in `keybinds_dir`, a client started with `--config`, `zellij options --keybinds-*` and a
//! layout's keybinds apply to one session and aren't seen; actions are shown as written, never checked.

use super::kdl_v1;
use super::nvim::single;
use crate::context::run_for;
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::{anyhow, Context as _};
use kdl::{KdlNode, KdlValue};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Each zellij run (`--version`, `setup`): none starts a session.
const TIMEOUT: Duration = Duration::from_secs(2);

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;
type Run<'a> = &'a dyn Fn(&[&str]) -> anyhow::Result<String>;

/// zellij's input modes as config files name them (0.46's `InputMode`, in its order; `InputMode::from_str` also takes
/// the second spelling, and `Entersearch`). 0.45 has all but the last.
const MODES: [(&str, &str); 15] = [
    ("normal", "Normal"),
    ("locked", "Locked"),
    ("resize", "Resize"),
    ("pane", "Pane"),
    ("tab", "Tab"),
    ("scroll", "Scroll"),
    ("entersearch", "EnterSearch"),
    ("search", "Search"),
    ("renametab", "RenameTab"),
    ("renamepane", "RenamePane"),
    ("session", "Session"),
    ("move", "Move"),
    ("prompt", "Prompt"),
    ("tmux", "Tmux"),
    ("renamesession", "RenameSession"),
];

/// The placeholders a 0.46 preset's keys may hold (`LEADER_PLACEHOLDERS`).
const LEADERS: [&str; 3] = ["primary", "secondary", "unlock"];

/// The system-wide config directory, the last zellij looks in.
const SYSTEM_DIR: &str = "/etc/zellij";

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = read(
        &|args| run_for(command(args), TIMEOUT),
        config_file(&env).as_deref(),
    )?;
    for note in notes {
        eprintln!("keybind-find: zellij: {note}");
    }
    Ok(sheet)
}

/// `zellij ARGS`, with no mise auto-install, from `/`.
fn command(args: &[&str]) -> Command {
    let mut cmd = Command::new("zellij");
    cmd.args(args)
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
        .current_dir("/");
    cmd
}

/// The sheet and the notes for stderr, from zellij's version, its defaults and the user's config at `config`.
fn read(run: Run, config: Option<&Path>) -> anyhow::Result<(Sheet, Vec<String>)> {
    let out = run(&["--version"]).context("running zellij --version")?;
    let version = version_of(&out)
        .with_context(|| format!("no version in `zellij --version`'s {:?}", out.trim()))?;
    let new = version >= (0, 46);
    let text = config
        .map(|p| std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display())))
        .transpose()?;
    let in_file = |e: anyhow::Error| match config {
        Some(p) => e.context(p.display().to_string()),
        None => e,
    };
    let doc = text
        .as_deref()
        .map(kdl_v1::parse)
        .transpose()
        .map_err(in_file)?;
    // Only the first: zellij reads it with `get("keybinds")`.
    let user = doc
        .as_ref()
        .and_then(|d| d.nodes().iter().find(|n| n.name().value() == "keybinds"));
    let user_text = text.as_deref().unwrap_or_default();
    let mut notes = Vec::new();
    let mut binds = if new {
        let (preset, leaders) = match user {
            Some(k) => selection(k, user_text).map_err(in_file)?,
            None => ("default".to_string(), BTreeMap::new()),
        };
        let (binds, note) = defaults(&preset, &leaders, run)?;
        notes.extend(note);
        binds
    } else {
        let out = run(&["setup", "--dump-config"]).context("running zellij setup --dump-config")?;
        dump_config_binds(&out).context("reading zellij setup --dump-config")?
    };
    if let Some(user) = user {
        apply(&mut binds, user, user_text, None, new).map_err(in_file)?;
    }
    let (sheet, skipped) = sheet_of(&binds)?;
    notes.extend(left_out(skipped));
    Ok((sheet, notes))
}

/// The version in `zellij --version`'s `zellij 0.46.0`, or a distribution build's `… (zellij 0.46.0)`.
fn version_of(text: &str) -> Option<(u32, u32)> {
    let rest = &text[text.rfind("zellij ")? + "zellij ".len()..];
    // `0.46.0)` in a distribution's form: only the first two parts are read.
    let word = rest.split_whitespace().next()?;
    let mut parts = word.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The user's `config.kdl` as zellij finds it: `$ZELLIJ_CONFIG_FILE`; else `config.kdl` in `$ZELLIJ_CONFIG_DIR`, or in
/// the first of `~/.config/zellij`, `$XDG_CONFIG_HOME/zellij` and `/etc/zellij` that exists (`find_default_config_dir`:
/// `exists`, so a file there stops the search too). `None` when there is no file. An empty variable counts as unset;
/// a relative `XDG_CONFIG_HOME` too (`directories`' rule).
fn config_file(env: Env) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(file) = set("ZELLIJ_CONFIG_FILE") {
        return Some(file);
    }
    let dir = set("ZELLIJ_CONFIG_DIR").or_else(|| {
        let home = set("HOME").map(|h| h.join(".config"));
        // An unset XDG_CONFIG_HOME means ~/.config: already looked in.
        let xdg = set("XDG_CONFIG_HOME").filter(|p| p.is_absolute());
        [home, xdg]
            .into_iter()
            .flatten()
            .map(|d| d.join("zellij"))
            .chain([PathBuf::from(SYSTEM_DIR)])
            .find(|d| d.exists())
    })?;
    Some(dir.join("config.kdl")).filter(|f| f.exists())
}

/// A mode's lower-case name from a config's name for it, among the first `count` of `MODES` (15 from 0.46, 14 before).
fn mode_of(name: &str, count: usize) -> Option<&'static str> {
    MODES[..count]
        .iter()
        .find(|(lower, camel)| {
            name == *lower || name == *camel || (name == "Entersearch" && *lower == "entersearch")
        })
        .map(|(lower, _)| *lower)
}

/// A key as zellij reads one (`KeyWithModifier::from_str`): its modifiers (`[super, ctrl, alt, shift]`) and the bare
/// key. An upper-case ASCII letter is shift and the letter, as zellij compares them.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    mods: [bool; 4],
    bare: Bare,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Bare {
    /// A named key, as its X keysym.
    Named(&'static str),
    Char(char),
}

/// The modifiers' names in keybind-find's notation, in `Key::mods` order.
const MOD_NAMES: [&str; 4] = ["super", "ctrl", "alt", "shift"];

impl Key {
    /// `text` as zellij reads a key (whitespace-split: the last word the bare key, any others modifiers, each case-
    /// insensitive), or `None` where zellij rejects it.
    fn parse(text: &str) -> Option<Key> {
        let mut words: Vec<&str> = text.split_ascii_whitespace().collect();
        let bare = words.pop()?;
        let mut mods = [false; 4];
        for w in words {
            let i = MOD_NAMES.iter().position(|m| w.eq_ignore_ascii_case(m))?;
            mods[i] = true;
        }
        let named = match bare.to_ascii_lowercase().as_str() {
            "pagedown" => "Next",
            "pageup" => "Prior",
            "left" => "Left",
            "right" => "Right",
            "up" => "Up",
            "down" => "Down",
            "home" => "Home",
            "end" => "End",
            "backspace" => "BackSpace",
            "delete" | "del" => "Delete",
            "insert" => "Insert",
            "tab" => "Tab",
            "esc" => "Escape",
            "enter" => "Return",
            "capslock" => "Caps_Lock",
            "scrolllock" => "Scroll_Lock",
            "numlock" => "Num_Lock",
            "printscreen" => "Print",
            "pause" => "Pause",
            "menu" => "Menu",
            "space" => {
                return Some(Key {
                    mods,
                    bare: Bare::Char(' '),
                })
            }
            f => match FKEYS.iter().find(|k| k.eq_ignore_ascii_case(f)) {
                Some(k) => *k,
                None => {
                    let mut chars = bare.chars();
                    let (Some(c), None) = (chars.next(), chars.next()) else {
                        return None;
                    };
                    if c.is_ascii_uppercase() {
                        mods[3] = true;
                    }
                    return Some(Key {
                        mods,
                        bare: Bare::Char(c.to_ascii_lowercase()),
                    });
                }
            },
        };
        Some(Key {
            mods,
            bare: Bare::Named(named),
        })
    }

    /// In keybind-find's notation; `None` for a character with no X keysym (non-ASCII).
    fn notation(&self) -> Option<String> {
        let sym = match self.bare {
            Bare::Named(x) => x,
            Bare::Char(c) => single(c)?,
        };
        let mods: Vec<&str> = MOD_NAMES
            .iter()
            .zip(self.mods)
            .filter_map(|(m, on)| on.then_some(*m))
            .collect();
        Some(chord(&mods, sym))
    }
}

/// zellij's function keys (`BareKey::from_str`: `f1` to `f12`, nothing else).
const FKEYS: [&str; 12] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
];

/// A bind's actions, each as written; the first's name is its category.
type Actions = Vec<String>;
/// Mode → key → actions.
type Binds = BTreeMap<&'static str, BTreeMap<Key, Actions>>;

/// An error at `node`'s line in `text`.
fn at(text: &str, node: &KdlNode, msg: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("line {}: {msg}", kdl_v1::line(text, node))
}

/// A node's entries as strings (`kdl_string_arguments!`: properties too); any other value is an error.
fn strings<'a>(node: &'a KdlNode, text: &str) -> anyhow::Result<Vec<&'a str>> {
    node.entries()
        .iter()
        .map(|e| {
            e.value().as_string().ok_or_else(|| {
                at(
                    text,
                    node,
                    format!("{} is not a string", value_text(e.value())),
                )
            })
        })
        .collect()
}

/// `clear-defaults` on `node` (its last such property): `true`, `false`, or an error for anything else.
fn clears(node: &KdlNode, text: &str) -> anyhow::Result<bool> {
    match node.get("clear-defaults") {
        None => Ok(false),
        Some(v) => v.as_bool().ok_or_else(|| {
            at(
                text,
                node,
                format!(
                    "clear-defaults must be true or false, not {}",
                    value_text(v)
                ),
            )
        }),
    }
}

/// The keys of a `bind` or `unbind` node, each with `leaders`' placeholders put in first (a preset's keys).
fn keys(
    node: &KdlNode,
    text: &str,
    leaders: Option<&BTreeMap<String, String>>,
) -> anyhow::Result<Vec<Key>> {
    strings(node, text)?
        .into_iter()
        .map(|k| {
            let k = match leaders {
                Some(values) => substitute(k, values),
                None => k.to_string(),
            };
            Key::parse(&k).ok_or_else(|| at(text, node, format!("invalid key {k:?}")))
        })
        .collect()
}

/// `{primary}`-style placeholders replaced by their values, then whitespace collapsed (`substitute_placeholders`). Every
/// placeholder in a preset has a value by now (`preset_binds` checks).
fn substitute(key: &str, values: &BTreeMap<String, String>) -> String {
    let mut key = key.to_string();
    for (name, value) in values {
        key = key.replace(&format!("{{{name}}}"), value);
    }
    key.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The binds and unbinds of one mode or `shared*` block into `binds`: every `bind` first, then every `unbind`
/// (`bind_keys_in_block`); anything else is an error, as is a block with no `{ }`.
fn bind_block(
    binds: &mut BTreeMap<Key, Actions>,
    block: &KdlNode,
    text: &str,
    leaders: Option<&BTreeMap<String, String>>,
) -> anyhow::Result<()> {
    let nodes = block
        .children()
        .ok_or_else(|| {
            at(
                text,
                block,
                format!("{} has no {{ }} block", block.name().value()),
            )
        })?
        .nodes();
    for node in nodes {
        match node.name().value() {
            "bind" => {
                let actions = node
                    .children()
                    .ok_or_else(|| at(text, node, "a bind with no { } block of actions"))?
                    .nodes()
                    .iter()
                    .map(action_text)
                    .collect::<Actions>();
                for key in keys(node, text, leaders)? {
                    binds.insert(key, actions.clone());
                }
            }
            "unbind" => {}
            other => {
                return Err(at(
                    text,
                    node,
                    format!("unknown keybind instruction {other:?}"),
                ))
            }
        }
    }
    for node in nodes.iter().filter(|n| n.name().value() == "unbind") {
        for key in keys(node, text, leaders)? {
            binds.remove(&key);
        }
    }
    Ok(())
}

/// One `keybinds` block applied to `binds`, as zellij applies it (0.46 `parse_bind_blocks`, 0.45 `Keybinds::from_kdl`):
/// `clear-defaults=true` empties everything; then every `shared`/`shared_except` (all modes but those named) and
/// `shared_among` (only those) block in order; then each mode block in order (its `clear-defaults=true` empties the mode
/// first); then the top-level `unbind`s in every mode (all of them from 0.46, only the first before). `new`: 0.46 or
/// later.
fn apply(
    binds: &mut Binds,
    keybinds: &KdlNode,
    text: &str,
    leaders: Option<&BTreeMap<String, String>>,
    new: bool,
) -> anyhow::Result<()> {
    let count = if new { MODES.len() } else { MODES.len() - 1 };
    if clears(keybinds, text)? {
        binds.clear();
    }
    let Some(children) = keybinds.children() else {
        // 0.45 refuses a `keybinds` with no `{ }`; 0.46 takes it.
        return match new {
            true => Ok(()),
            false => Err(at(text, keybinds, "keybinds has no { } block")),
        };
    };
    let nodes = children.nodes();
    for block in nodes {
        let except = match block.name().value() {
            "shared" | "shared_except" => true,
            "shared_among" => false,
            _ => continue,
        };
        let named = strings(block, text)?
            .into_iter()
            .map(|m| {
                mode_of(m, count).ok_or_else(|| at(text, block, format!("unknown mode {m:?}")))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        for &(mode, _) in &MODES[..count] {
            if named.contains(&mode) != except {
                bind_block(binds.entry(mode).or_default(), block, text, leaders)?;
            }
        }
    }
    for block in nodes {
        let name = block.name().value();
        if ["unbind", "shared", "shared_except", "shared_among"].contains(&name) {
            continue;
        }
        let mode = mode_of(name, count)
            .ok_or_else(|| at(text, block, format!("unknown mode {name:?}")))?;
        let mode_binds = binds.entry(mode).or_default();
        if clears(block, text)? {
            mode_binds.clear();
        }
        bind_block(mode_binds, block, text, leaders)?;
    }
    let unbinds = nodes.iter().filter(|n| n.name().value() == "unbind");
    for node in unbinds.take(if new { usize::MAX } else { 1 }) {
        for key in keys(node, text, leaders)? {
            for mode_binds in binds.values_mut() {
                mode_binds.remove(&key);
            }
        }
    }
    Ok(())
}

/// The preset and leader values the user's `keybinds` node selects (`KeybindsSelection::from_kdl`): `preset` (default
/// `default`) and the `primary`, `secondary` and `unlock` properties, each a string.
fn selection(keybinds: &KdlNode, text: &str) -> anyhow::Result<(String, BTreeMap<String, String>)> {
    let string = |name: &str| -> anyhow::Result<Option<String>> {
        keybinds
            .get(name)
            .map(|v| {
                v.as_string().map(str::to_string).ok_or_else(|| {
                    at(
                        text,
                        keybinds,
                        format!("{name} must be a string, not {}", value_text(v)),
                    )
                })
            })
            .transpose()
    };
    let preset = string("preset")?.unwrap_or_else(|| "default".to_string());
    let mut leaders = BTreeMap::new();
    for name in LEADERS {
        if let Some(v) = string(name)? {
            leaders.insert(name.to_string(), v);
        }
    }
    Ok((preset, leaders))
}

/// zellij's defaults from 0.46: the preset named `preset`, as `zellij setup --dump-keybinds` prints it, with the user's
/// `leaders`. A preset zellij can't dump or that doesn't apply (a leader empty, missing or equal to another): the
/// `default` preset with its own leaders, as zellij does, and a note saying why.
fn defaults(
    preset: &str,
    leaders: &BTreeMap<String, String>,
    run: Run,
) -> anyhow::Result<(Binds, Option<String>)> {
    let dump = |name: &str| {
        run(&["setup", "--dump-keybinds", name])
            .with_context(|| format!("running zellij setup --dump-keybinds {name}"))
    };
    match dump(preset).and_then(|t| preset_binds(&t, leaders)) {
        Ok(binds) => Ok((binds, None)),
        Err(e) => {
            let note = format!(
                "can't use the keybinding preset {preset:?} ({e:#}); showing the default preset with its own leader \
                 keys, as zellij does"
            );
            let text = dump("default")?;
            let binds =
                preset_binds(&text, &BTreeMap::new()).context("reading the default preset")?;
            Ok((binds, Some(note)))
        }
    }
}

/// A 0.46 preset's binds (`KeybindPreset::from_kdl`, `leader_values`, `keybinds`): each placeholder its keys use takes
/// the user's value, else the preset's `preset { defaults { … } }` one, whitespace collapsed; an empty value, a
/// placeholder with none, or `primary` and `secondary` with the same modifiers is an error. `mousebinds` is never read.
fn preset_binds(text: &str, requested: &BTreeMap<String, String>) -> anyhow::Result<Binds> {
    let doc = kdl_v1::parse(text)?;
    let first = |name: &str| doc.nodes().iter().find(|n| n.name().value() == name);
    let mut defaults = BTreeMap::new();
    let header = first("preset").and_then(KdlNode::children);
    if let Some(d) = header
        .and_then(|h| h.get("defaults"))
        .and_then(KdlNode::children)
    {
        for node in d.nodes() {
            let value = node
                .entries()
                .iter()
                .find(|e| e.name().is_none())
                .and_then(|e| e.value().as_string());
            defaults.insert(
                node.name().value().to_string(),
                value.unwrap_or_default().to_string(),
            );
        }
    }
    let keybinds = first("keybinds").ok_or_else(|| anyhow!("the preset has no keybinds block"))?;
    let mut used = Vec::new();
    placeholders(keybinds, &mut used);
    let mut values = BTreeMap::new();
    for name in used {
        if !LEADERS.contains(&name.as_str()) {
            anyhow::bail!("unknown placeholder {{{name}}}");
        }
        let value = requested
            .get(&name)
            .or_else(|| defaults.get(&name))
            .ok_or_else(|| anyhow!("the preset uses {{{name}}} and gives no value for it"))?;
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if value.is_empty() {
            anyhow::bail!("the value for {name} is empty");
        }
        values.insert(name, value);
    }
    if let (Some(p), Some(s)) = (values.get("primary"), values.get("secondary")) {
        let set = |v: &str| {
            let mut words: Vec<String> = v.split_whitespace().map(str::to_lowercase).collect();
            words.sort();
            words.dedup();
            words
        };
        if set(p) == set(s) {
            anyhow::bail!("primary and secondary are both {p:?}");
        }
    }
    let mut binds = Binds::new();
    apply(&mut binds, keybinds, text, Some(&values), true)?;
    Ok(binds)
}

/// The `{name}` placeholders in `node`'s `bind` and `unbind` keys, and its children's (`collect_placeholders`).
fn placeholders(node: &KdlNode, found: &mut Vec<String>) {
    if ["bind", "unbind"].contains(&node.name().value()) {
        for k in node
            .entries()
            .iter()
            .filter(|e| e.name().is_none())
            .filter_map(|e| e.value().as_string())
        {
            let mut rest = k;
            while let Some((name, after)) =
                rest.split_once('{').and_then(|(_, r)| r.split_once('}'))
            {
                if !found.iter().any(|f| f == name) {
                    found.push(name.to_string());
                }
                rest = after;
            }
        }
        return;
    }
    for child in node.children().map(|c| c.nodes()).unwrap_or_default() {
        placeholders(child, found);
    }
}

/// zellij's defaults before 0.46: the `keybinds` of `--dump-config` (its default config, no placeholders).
fn dump_config_binds(text: &str) -> anyhow::Result<Binds> {
    let doc = kdl_v1::parse(text)?;
    let keybinds = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "keybinds")
        .ok_or_else(|| anyhow!("no keybinds block"))?;
    let mut binds = Binds::new();
    apply(&mut binds, keybinds, text, None, false)?;
    Ok(binds)
}

/// One action as written: its name, then each argument (a property as `name=value`), and `{…}` for a children block.
fn action_text(node: &KdlNode) -> String {
    let mut parts = vec![node.name().value().to_string()];
    for e in node.entries() {
        let v = value_text(e.value());
        parts.push(match e.name() {
            Some(name) => format!("{}={v}", name.value()),
            None => v,
        });
    }
    if node.children().is_some() {
        parts.push("{…}".to_string());
    }
    parts.join(" ")
}

/// A value as KDL v1 writes it: a string quoted (Rust's escapes, close to KDL's), `true`, `null`, a number.
fn value_text(v: &KdlValue) -> String {
    match v {
        KdlValue::String(s) => format!("{s:?}"),
        KdlValue::Integer(i) => i.to_string(),
        KdlValue::Float(f) => format!("{f:?}"),
        KdlValue::Bool(b) => b.to_string(),
        KdlValue::Null => "null".to_string(),
    }
}

/// The sheet of `binds`, grouped by each bind's first action, and how many binds were left out (a key with no X keysym,
/// or a bind with no action).
fn sheet_of(binds: &Binds) -> anyhow::Result<(Sheet, usize)> {
    let mut sheet = Sheet {
        title: "zellij".into(),
        provider: "zellij".into(),
        binds: Default::default(),
    };
    let scope: Name = "input_mode".parse()?;
    let mut skipped = 0;
    for (mode, keys) in binds {
        let value: Name = mode.parse()?;
        for (key, actions) in keys {
            let (Some(k), Some(first)) = (key.notation(), actions.first()) else {
                skipped += 1;
                continue;
            };
            let category = first.split(' ').next().unwrap_or(first).to_string();
            sheet.binds.entry(category.clone()).or_default().push(Bind {
                key: k,
                desc: actions.join("; "),
                action: category,
                when: [(scope.clone(), Values::from(value.clone()))].into(),
                run: Vec::new(),
            });
        }
    }
    Ok((sheet, skipped))
}

/// The one stderr line for `skipped` left-out binds; none when there are none.
fn left_out(skipped: usize) -> Option<String> {
    (skipped > 0).then(|| {
        format!("leaving out {skipped} binds (a key with no X keysym, or a bind with no action)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/zellij/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// Every bind as `mode key = desc`, sorted.
    fn rows(sheet: &Sheet) -> Vec<String> {
        let mut rows: Vec<String> = sheet
            .binds
            .values()
            .flatten()
            .map(|b| {
                let modes: Vec<String> = b
                    .when
                    .values()
                    .flat_map(|v| v.iter().map(Name::to_string))
                    .collect();
                format!("{} {} = {}", modes.join("|"), b.key, b.desc)
            })
            .collect();
        rows.sort();
        rows
    }

    fn has(rows: &[String], row: &str) -> bool {
        rows.iter().any(|r| r == row)
    }

    /// How many binds each mode has.
    fn counts(binds: &Binds) -> Vec<(&'static str, usize)> {
        binds.iter().map(|(m, k)| (*m, k.len())).collect()
    }

    fn leaders(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The sheet of `defaults` (a 0.46 preset with no user leaders) with the user config `user` on top.
    fn merged(defaults: &str, user: &str, new: bool) -> anyhow::Result<Vec<String>> {
        let mut binds = match new {
            true => preset_binds(defaults, &BTreeMap::new())?,
            false => dump_config_binds(defaults)?,
        };
        let doc = kdl_v1::parse(user)?;
        if let Some(k) = doc.nodes().iter().find(|n| n.name().value() == "keybinds") {
            apply(&mut binds, k, user, None, new)?;
        }
        Ok(rows(&sheet_of(&binds)?.0))
    }

    /// A tiny 0.46 preset: `shared_except "locked"` binds and one mode block.
    const PRESET: &str = "preset { defaults { primary \"Ctrl\"; secondary \"Alt\"; }; }\nkeybinds {\n\
        locked { bind \"{primary} g\" { SwitchToMode \"Normal\"; }; }\n\
        shared_except \"locked\" { bind \"{primary} q\" { Quit; }; bind \"Shift h\" { MoveFocus \"Left\"; }; }\n\
        pane { bind \"x\" { CloseFocus; }; }\n}\n";

    #[test]
    fn the_default_preset() {
        let binds = preset_binds(&fixture("0.46.0/default.kdl"), &BTreeMap::new()).unwrap();
        assert_eq!(
            counts(&binds),
            [
                ("entersearch", 31),
                ("locked", 1),
                ("move", 41),
                ("normal", 28),
                ("pane", 58),
                ("prompt", 30),
                ("renamepane", 31),
                ("renamesession", 31),
                ("renametab", 31),
                ("resize", 45),
                ("scroll", 50),
                ("search", 52),
                ("session", 47),
                ("tab", 58),
                ("tmux", 50),
            ]
        );
        let (sheet, skipped) = sheet_of(&binds).unwrap();
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str(), skipped),
            ("zellij", "zellij", 0)
        );
        let rows = rows(&sheet);
        for want in [
            "normal ctrl+g = SwitchToMode \"Locked\"",
            "locked ctrl+g = SwitchToMode \"Normal\"",
            "normal alt+h = MoveFocusOrTab \"Left\"",
            "normal alt+Left = MoveFocusOrTab \"Left\"",
            "normal alt+shift+p = ToggleGroupMarking",
            "pane d = NewPane \"Down\"; SwitchToMode \"Normal\"",
            "pane c = SwitchToMode \"RenamePane\"; PaneNameInput 0",
            "resize shift+h = Resize \"Decrease Left\"",
            "tab shift+space = PreviousSwapLayout",
            "tmux quotedbl = NewPane \"Down\"; SwitchToMode \"Normal\"",
            "tmux percent = NewPane \"Right\"; SwitchToMode \"Normal\"",
            "scroll Next = PageScrollDown",
            "scroll Prior = PageScrollUp",
            "entersearch Escape = SwitchToMode \"Scroll\"",
            "entersearch Return = SwitchToMode \"Search\"",
            "session w = LaunchOrFocusPlugin \"session-manager\" {…}; SwitchToMode \"Normal\"",
            "session Tab = ToggleSession; SwitchToMode \"Normal\"",
        ] {
            assert!(has(&rows, want), "{want} not in {rows:#?}");
        }
        // `mousebinds` contributes nothing: none of its actions or keys.
        assert!(!rows
            .iter()
            .any(|r| ["Click", "Double", "ContextMenu", "GroupToggle"]
                .iter()
                .any(|m| r.contains(m))));
        // The category is the first action's name.
        assert_eq!(
            sheet.binds["NewPane"]
                .iter()
                .find(|b| b.key == "d")
                .unwrap()
                .action,
            "NewPane"
        );
        assert!(sheet.binds.contains_key("LaunchOrFocusPlugin"));
    }

    #[test]
    fn leaders_from_the_user() {
        let binds = |l: &[(&str, &str)]| {
            rows(
                &sheet_of(&preset_binds(&fixture("0.46.0/default.kdl"), &leaders(l)).unwrap())
                    .unwrap()
                    .0,
            )
        };
        let swapped = binds(&[("primary", "Alt"), ("secondary", "Ctrl")]);
        assert!(
            has(&swapped, "normal alt+p = SwitchToMode \"Pane\""),
            "{swapped:#?}"
        );
        assert!(has(&swapped, "normal ctrl+h = MoveFocusOrTab \"Left\""));
        assert!(has(&swapped, "normal ctrl+p = TogglePaneInGroup"));
        // A multi-word leader, whitespace collapsed; `unlock` is unused by this preset.
        let wide = binds(&[("primary", "  Ctrl   Shift "), ("unlock", "")]);
        assert!(
            has(&wide, "normal ctrl+shift+p = SwitchToMode \"Pane\""),
            "{wide:#?}"
        );
        // unlock-first: `{unlock}` from its own defaults (Ctrl g), or the user's.
        let uf = |l| {
            rows(
                &sheet_of(&preset_binds(&fixture("0.46.0/unlock-first.kdl"), &leaders(l)).unwrap())
                    .unwrap()
                    .0,
            )
        };
        assert!(has(&uf(&[]), "locked ctrl+g = SwitchToMode \"Normal\""));
        assert!(has(
            &uf(&[("unlock", "Alt u")]),
            "locked alt+u = SwitchToMode \"Normal\""
        ));
        assert!(has(&uf(&[]), "pane Tab = SwitchFocus"));
    }

    #[test]
    fn leaders_that_dont_apply() {
        let preset = fixture("0.46.0/default.kdl");
        for (l, why) in [
            (
                vec![("primary", "Alt")],
                "primary and secondary are both \"Alt\"",
            ),
            (
                vec![("primary", "ctrl alt"), ("secondary", "Alt Ctrl")],
                "primary and secondary are both",
            ),
            (vec![("primary", "  ")], "the value for primary is empty"),
            (vec![("primary", "Hyper")], "invalid key \"Hyper g\""),
        ] {
            let e = format!("{:#}", preset_binds(&preset, &leaders(&l)).unwrap_err());
            assert!(e.contains(why), "{l:?}: {e}");
        }
        // A placeholder with no value, an unknown one, no keybinds.
        let e = |t: &str| format!("{:#}", preset_binds(t, &BTreeMap::new()).unwrap_err());
        assert!(e("keybinds { normal { bind \"{unlock}\" { Quit; }; }; }")
            .contains("gives no value for it"));
        assert!(
            e("keybinds { normal { bind \"{tertiary} x\" { Quit; }; }; }")
                .contains("unknown placeholder {tertiary}")
        );
        assert!(e("preset { }").contains("no keybinds block"));
        assert!(e("keybinds {").starts_with("line 1: "));
    }

    #[test]
    fn the_default_config_before_0_46() {
        let binds = dump_config_binds(&fixture("0.45.1/default.kdl")).unwrap();
        // No renamesession mode before 0.46.
        assert_eq!(binds.len(), 14);
        assert!(!binds.contains_key("renamesession"));
        // As tests/e2e/oracle.py's reader counts them over 0.45's 14 modes.
        let total: usize = binds.values().map(BTreeMap::len).sum();
        assert_eq!(total, 537);
        let (sheet, skipped) = sheet_of(&binds).unwrap();
        assert_eq!(skipped, 0);
        let rows = rows(&sheet);
        assert!(
            has(&rows, "normal ctrl+g = SwitchToMode \"Locked\""),
            "{rows:#?}"
        );
        assert!(has(&rows, "normal alt+n = NewPane"));
        assert!(!rows.iter().any(|r| r.contains("Click")));
        assert!(dump_config_binds("theme \"x\"\n")
            .unwrap_err()
            .to_string()
            .contains("no keybinds block"));
    }

    #[test]
    fn version() {
        assert_eq!(version_of("zellij 0.46.0\n"), Some((0, 46)));
        assert_eq!(version_of("zellij 0.45.1\n"), Some((0, 45)));
        assert_eq!(version_of("foo 1.0 (zellij 0.46.0)\n"), Some((0, 46)));
        assert_eq!(version_of("zellij 1.2.3-rc1"), Some((1, 2)));
        // The last `zellij `: the version of zellij, not of a distribution named after it.
        assert_eq!(version_of("zellij 1.0 (zellij 0.45.1)"), Some((0, 45)));
        for bad in [
            "",
            "garbage",
            "zellij",
            "zellij x.y",
            "zellij 0",
            "tmux 3.5",
        ] {
            assert_eq!(version_of(bad), None, "{bad}");
        }
    }

    /// `read` with a run that answers `--version` with `version` and the `setup` dumps from the fixtures, recording
    /// every call.
    fn read_with(
        version: &str,
        config: Option<&Path>,
    ) -> (anyhow::Result<(Sheet, Vec<String>)>, Vec<String>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let run = |args: &[&str]| -> anyhow::Result<String> {
            calls.borrow_mut().push(args.join(" "));
            match args {
                ["--version"] => Ok(format!("zellij {version}\n")),
                ["setup", "--dump-keybinds", "default"] => Ok(fixture("0.46.0/default.kdl")),
                ["setup", "--dump-keybinds", "unlock-first"] => {
                    Ok(fixture("0.46.0/unlock-first.kdl"))
                }
                ["setup", "--dump-keybinds", p] => {
                    anyhow::bail!("Keybinding preset not found: {p}")
                }
                ["setup", "--dump-config"] => Ok(fixture("0.45.1/default.kdl")),
                _ => anyhow::bail!("unexpected {args:?}"),
            }
        };
        let out = read(&run, config);
        (out, calls.into_inner())
    }

    /// A temp dir removed when dropped, failed asserts included.
    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmp(name: &str) -> (PathBuf, Cleanup) {
        let dir = crate::test_tmp().join(format!("kbf-zellij-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), Cleanup(dir))
    }

    /// A `config.kdl` holding `text` in a temp dir.
    fn config(name: &str, text: &str) -> (PathBuf, Cleanup) {
        let (dir, guard) = tmp(name);
        let f = dir.join("config.kdl");
        std::fs::write(&f, text).unwrap();
        (f, guard)
    }

    #[test]
    fn commands_for_each_version() {
        let (_, calls) = read_with("0.46.0", None);
        assert_eq!(calls, ["--version", "setup --dump-keybinds default"]);
        let (_, calls) = read_with("0.45.1", None);
        assert_eq!(calls, ["--version", "setup --dump-config"]);
        let (f, _g) = config("preset", "keybinds preset=\"unlock-first\" {\n}\n");
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(calls, ["--version", "setup --dump-keybinds unlock-first"]);
        assert!(has(
            &rows(&out.unwrap().0),
            "locked ctrl+g = SwitchToMode \"Normal\""
        ));
        // A preset zellij can't dump: one note, the default preset.
        let (f, _g) = config("missing", "keybinds preset=\"mine\" primary=\"Alt\" {\n}\n");
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(
            calls,
            [
                "--version",
                "setup --dump-keybinds mine",
                "setup --dump-keybinds default"
            ]
        );
        let (sheet, notes) = out.unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("can't use the keybinding preset \"mine\" (running zellij setup --dump-keybinds mine: Keybinding preset not found: mine); showing the default preset"), "{notes:?}");
        // The user's leaders were for `mine`: the default preset keeps its own.
        assert!(has(&rows(&sheet), "normal ctrl+p = SwitchToMode \"Pane\""));
        // A 0.45 config's preset properties mean nothing to 0.45.
        let (out, calls) = read_with("0.45.1", Some(&f));
        assert_eq!(calls, ["--version", "setup --dump-config"]);
        assert!(out.unwrap().1.is_empty());
        // A version that doesn't read: one error, nothing else run.
        let (out, calls) = read_with("unknown", None);
        assert_eq!(calls, ["--version"]);
        let e = format!("{:#}", out.unwrap_err());
        assert_eq!(e, "no version in `zellij --version`'s \"zellij unknown\"");
    }

    #[test]
    fn equal_leaders_fall_back_with_a_note() {
        let (f, _g) = config(
            "equal",
            "keybinds primary=\"Alt\" {\n  normal { bind \"Alt y\" { NewTab; }; }\n}\n",
        );
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(
            calls,
            [
                "--version",
                "setup --dump-keybinds default",
                "setup --dump-keybinds default"
            ]
        );
        let (sheet, notes) = out.unwrap();
        assert_eq!(
            notes,
            ["can't use the keybinding preset \"default\" (primary and secondary are both \"Alt\"); showing the default preset with its own leader keys, as zellij does"]
        );
        let rows = rows(&sheet);
        assert!(has(&rows, "normal ctrl+p = SwitchToMode \"Pane\""));
        // The user's own binds still apply on top.
        assert!(has(&rows, "normal alt+y = NewTab"));
    }

    #[test]
    fn the_synthetic_config() {
        let f = PathBuf::from(format!(
            "{}/tests/fixtures/zellij/config.kdl",
            env!("CARGO_MANIFEST_DIR")
        ));
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(calls, ["--version", "setup --dump-keybinds default"]);
        let (sheet, notes) = out.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        let rows = rows(&sheet);
        for want in [
            "normal alt+p = SwitchToMode \"Pane\"",
            "normal ctrl+shift+n = NewPane",
            "normal alt+y = NewTab",
            "pane F5 = Detach",
            "tab F5 = Detach",
        ] {
            assert!(has(&rows, want), "{want} not in {rows:#?}");
        }
        // `Alt q` (Quit under primary=Alt) unbound everywhere; F5 only where shared_among put it.
        assert!(!rows.iter().any(|r| r.contains(" alt+q ")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.starts_with("normal F5 ")));
    }

    #[test]
    fn config_errors_name_the_file_and_line() {
        for (text, want) in [
            (
                "keybinds {\n  normal {\n    bind \"Hyper x\" { Quit; }\n  }\n}\n",
                "line 3: invalid key \"Hyper x\"",
            ),
            (
                "keybinds {\n  nosuch { }\n}\n",
                "line 2: unknown mode \"nosuch\"",
            ),
            (
                "keybinds {\n  shared_except \"nosuch\" { }\n}\n",
                "line 2: unknown mode \"nosuch\"",
            ),
            (
                "keybinds {\n  shared_among \"Normal\" \"bogus\" { }\n}\n",
                "line 2: unknown mode \"bogus\"",
            ),
            (
                "keybinds clear-defaults=\"yes\" {\n}\n",
                "line 1: clear-defaults must be true or false, not \"yes\"",
            ),
            (
                "keybinds {\n  normal clear-defaults=1 { }\n}\n",
                "line 2: clear-defaults must be true or false, not 1",
            ),
            (
                "keybinds {\n\n  normal {\n    bnid \"x\" { Quit; }\n  }\n}\n",
                "line 4: unknown keybind instruction \"bnid\"",
            ),
            (
                "keybinds {\n  normal\n}\n",
                "line 2: normal has no { } block",
            ),
            (
                "keybinds {\n  normal {\n    bind \"x\"\n  }\n}\n",
                "line 3: a bind with no { } block of actions",
            ),
            (
                "keybinds {\n  normal {\n    bind 1 { Quit; }\n  }\n}\n",
                "line 3: 1 is not a string",
            ),
            (
                "keybinds {\n  unbind \"x y\"\n}\n",
                "line 2: invalid key \"x y\"",
            ),
            (
                "keybinds preset=1 {\n}\n",
                "line 1: preset must be a string, not 1",
            ),
            ("keybinds {\n  normal {\n", "line 2: Expected closing '}'"),
        ] {
            let (f, _g) = config("errors", text);
            let (out, _) = read_with("0.46.0", Some(&f));
            let e = format!("{:#}", out.unwrap_err());
            assert_eq!(
                e.split_once(": line").map(|(p, _)| p),
                Some(f.display().to_string().as_str()),
                "{e}"
            );
            assert!(e.contains(want), "{text:?}: {e}");
        }
        // 0.45 knows no renamesession mode, and wants a `{ }` on `keybinds`.
        let (f, _g) = config("old", "keybinds {\n  renamesession { }\n}\n");
        assert!(
            format!("{:#}", read_with("0.45.1", Some(&f)).0.unwrap_err())
                .ends_with("line 2: unknown mode \"renamesession\"")
        );
        let (f, _g) = config("old2", "keybinds clear-defaults=true\n");
        assert!(
            format!("{:#}", read_with("0.45.1", Some(&f)).0.unwrap_err())
                .ends_with("line 1: keybinds has no { } block")
        );
        // 0.46 takes both.
        assert!(read_with("0.46.0", Some(&f)).0.unwrap().0.binds.is_empty());
        // A file that can't be read.
        let (dir, _g) = tmp("unreadable");
        let e = format!(
            "{:#}",
            read_with("0.46.0", Some(&dir.join("nosuch.kdl")))
                .0
                .unwrap_err()
        );
        assert!(
            e.starts_with(&format!("reading {}: ", dir.join("nosuch.kdl").display())),
            "{e}"
        );
    }

    #[test]
    fn merge() {
        let p = |user: &str| merged(PRESET, user, true).unwrap();
        // keybinds clear-defaults: only the user's.
        assert_eq!(
            p("keybinds clear-defaults=true { normal { bind \"a\" { Quit; }; }; }"),
            ["normal a = Quit"]
        );
        // A mode's clear-defaults after a shared_except that bound into it: the shared bind is gone too.
        assert_eq!(
            p("keybinds clear-defaults=true { shared_except \"normal\" { bind \"z\" { Quit; }; }\n\
               pane clear-defaults=true { bind \"y\" { Detach; }; }; }")
                .iter()
                .filter(|r| r.starts_with("pane "))
                .collect::<Vec<_>>(),
            ["pane y = Detach"]
        );
        // A mode block after a shared block, and only the named modes for shared_among, in either spelling.
        let rows = p("keybinds clear-defaults=true { pane { bind \"a\" { Write 1; }; }\n\
                      shared_among \"Pane\" \"entersearch\" \"Entersearch\" { bind \"a\" \"b\" { Write 2; }; }; }");
        assert_eq!(
            rows,
            [
                "entersearch a = Write 2",
                "entersearch b = Write 2",
                "pane a = Write 1",
                "pane b = Write 2"
            ]
        );
        // `shared` is every mode.
        assert_eq!(
            p("keybinds clear-defaults=true { shared { bind \"F1\" { Quit; }; }; }").len(),
            15
        );
        // A block binds, then unbinds, whatever the order.
        assert_eq!(p("keybinds clear-defaults=true { pane { unbind \"x\"; bind \"x\" \"y\" { Quit; }; }; }"), ["pane y = Quit"]);
        // Top-level unbinds: every one from 0.46.
        let rows = p("keybinds { unbind \"Ctrl q\"\n unbind \"Ctrl g\"; }");
        assert!(
            !rows
                .iter()
                .any(|r| r.contains("ctrl+q") || r.contains("ctrl+g")),
            "{rows:#?}"
        );
        // `H` is `Shift h`: it replaces it, and unbinds it.
        let rows = p("keybinds { pane { bind \"H\" { Detach; }; }; }");
        assert!(
            has(&rows, "pane shift+h = Detach") && !has(&rows, "pane shift+h = MoveFocus \"Left\""),
            "{rows:#?}"
        );
        let rows = p("keybinds { unbind \"H\"; }");
        assert!(!rows.iter().any(|r| r.contains("shift+h")), "{rows:#?}");
        let rows = p("keybinds { tab { unbind \"shift H\"; }; }");
        assert!(
            !has(&rows, "tab shift+h = MoveFocus \"Left\"")
                && has(&rows, "pane shift+h = MoveFocus \"Left\"")
        );
        // Case doesn't matter for modifiers and named keys.
        assert_eq!(
            p("keybinds clear-defaults=true { pane { bind \"cTRL pAGEdOWN\" { Quit; }; }; }"),
            ["pane ctrl+Next = Quit"]
        );
        // A user bind with a placeholder: an invalid key, as in zellij (only presets have placeholders).
        assert!(merged(
            PRESET,
            "keybinds { pane { bind \"{primary} x\" { Quit; }; }; }",
            true
        )
        .is_err());
        // An unbind of a key nowhere bound, and of an empty mode: no change.
        assert_eq!(p("keybinds { unbind \"F9\"; }"), p(""));
        // A property's string is a key too (zellij reads every entry).
        assert_eq!(
            p("keybinds clear-defaults=true { pane { bind \"a\" k=\"b\" { Quit; }; }; }"),
            ["pane a = Quit", "pane b = Quit"]
        );
    }

    #[test]
    fn only_the_first_keybinds() {
        let (f, _g) = config(
            "two",
            "keybinds preset=\"unlock-first\" {\n  normal { bind \"Alt y\" { NewTab; }; }\n}\n\
             keybinds preset=\"nosuch\" clear-defaults=true {\n  normal { bind \"Alt z\" { Quit; }; }\n}\n",
        );
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(calls, ["--version", "setup --dump-keybinds unlock-first"]);
        let rows = rows(&out.unwrap().0);
        assert!(has(&rows, "normal alt+y = NewTab") && !rows.iter().any(|r| r.contains("alt+z")));
        assert!(has(&rows, "locked ctrl+g = SwitchToMode \"Normal\""));
    }

    #[test]
    fn merge_before_0_46() {
        let old = "keybinds {\n  normal { bind \"Ctrl q\" { Quit; }; bind \"Ctrl g\" { SwitchToMode \"Locked\"; }; }\n}\n";
        // Only the first top-level unbind.
        let rows = merged(
            old,
            "keybinds {\n  unbind \"Ctrl q\"\n  unbind \"Ctrl g\"\n}\n",
            false,
        )
        .unwrap();
        assert_eq!(rows, ["normal ctrl+g = SwitchToMode \"Locked\""]);
        let rows = merged(
            old,
            "keybinds {\n  unbind \"Ctrl q\"\n  unbind \"Ctrl g\"\n}\n",
            true,
        );
        // Both from 0.46.
        assert_eq!(rows.unwrap(), Vec::<String>::new());
        // shared covers 14 modes before 0.46.
        assert_eq!(
            merged(
                old,
                "keybinds clear-defaults=true { shared { bind \"F1\" { Quit; }; }; }",
                false
            )
            .unwrap()
            .len(),
            14
        );
    }

    #[test]
    fn keys() {
        let one = |k: &str| Key::parse(k).and_then(|k| k.notation());
        for (k, want) in [
            ("PageDown", "Next"),
            ("PageUp", "Prior"),
            ("Left", "Left"),
            ("Right", "Right"),
            ("Up", "Up"),
            ("Down", "Down"),
            ("Home", "Home"),
            ("End", "End"),
            ("Backspace", "BackSpace"),
            ("Delete", "Delete"),
            ("Del", "Delete"),
            ("Insert", "Insert"),
            ("F1", "F1"),
            ("f12", "F12"),
            ("Tab", "Tab"),
            ("Esc", "Escape"),
            ("Enter", "Return"),
            ("Space", "space"),
            ("CapsLock", "Caps_Lock"),
            ("ScrollLock", "Scroll_Lock"),
            ("NumLock", "Num_Lock"),
            ("PrintScreen", "Print"),
            ("Pause", "Pause"),
            ("Menu", "Menu"),
            ("a", "a"),
            ("A", "shift+a"),
            ("7", "7"),
            ("\"", "quotedbl"),
            ("{", "braceleft"),
            ("Ctrl Alt a", "ctrl+alt+a"),
            ("alt ctrl  a", "ctrl+alt+a"),
            ("Super Shift Tab", "super+shift+Tab"),
            ("SHIFT shift x", "shift+x"),
            ("Ctrl F", "ctrl+shift+f"),
        ] {
            assert_eq!(one(k).as_deref(), Some(want), "{k:?}");
        }
        // Keys zellij rejects.
        for k in [
            "", "  ", " ", "F13", "F0", "f01", "F+1", "Hyper a", "Ctrl", "ab", "Ctrl-a", "Meta x",
        ] {
            assert_eq!(Key::parse(k), None, "{k:?}");
        }
        // A key zellij takes but keybind-find can't send: parsed, no notation.
        assert!(Key::parse("é").is_some() && one("é").is_none());
        assert_eq!(Key::parse("H"), Key::parse("Shift h"));
        assert_ne!(Key::parse("ctrl h"), Key::parse("Shift h"));
    }

    #[test]
    fn descriptions_and_left_out() {
        let rows = |user: &str| {
            let doc = kdl_v1::parse(user).unwrap();
            let mut binds = Binds::new();
            apply(&mut binds, &doc.nodes()[0], user, None, true).unwrap();
            let (sheet, skipped) = sheet_of(&binds).unwrap();
            (rows(&sheet), skipped, sheet)
        };
        let (r, skipped, sheet) = rows(
            "keybinds {\n  normal {\n    bind \"a\" { NewPane \"Down\"; SwitchToMode \"Normal\"; }\n\
             bind \"b\" {\n      LaunchOrFocusPlugin \"zellij:about\" {\n        floating true\n      }\n      SwitchToMode \"Normal\"\n    }\n\
             bind \"c\" { Run \"git\" \"log\" cwd=\"/tmp\" close_on_exit=true; }\n\
             bind \"d\" { GoToTab 1; Resize 1.5; Write null; }\n\
             bind \"é\" \"Alt ü\" { Quit; }\n    bind \"e\" { }\n  }\n}\n",
        );
        assert_eq!(
            r,
            [
                "normal a = NewPane \"Down\"; SwitchToMode \"Normal\"",
                "normal b = LaunchOrFocusPlugin \"zellij:about\" {…}; SwitchToMode \"Normal\"",
                "normal c = Run \"git\" \"log\" cwd=\"/tmp\" close_on_exit=true",
                "normal d = GoToTab 1; Resize 1.5; Write null",
            ]
        );
        // `é`, `Alt ü` and the bind with no action.
        assert_eq!(skipped, 3);
        assert_eq!(sheet.binds["NewPane"][0].action, "NewPane");
        assert_eq!(
            sheet.binds["LaunchOrFocusPlugin"][0].action,
            "LaunchOrFocusPlugin"
        );
        let when = &sheet.binds["Run"][0].when;
        assert_eq!(when.len(), 1);
        assert_eq!(when.keys().next().unwrap().to_string(), "input_mode");
    }

    #[test]
    fn every_mode_name() {
        let names: Vec<&str> = MODES.iter().map(|(l, _)| *l).collect();
        assert_eq!(
            names,
            [
                "normal",
                "locked",
                "resize",
                "pane",
                "tab",
                "scroll",
                "entersearch",
                "search",
                "renametab",
                "renamepane",
                "session",
                "move",
                "prompt",
                "tmux",
                "renamesession"
            ]
        );
        for (lower, camel) in MODES {
            assert_eq!(mode_of(lower, 15), Some(lower));
            assert_eq!(mode_of(camel, 15), Some(lower));
            assert!(lower.parse::<Name>().is_ok());
        }
        assert_eq!(mode_of("Entersearch", 15), Some("entersearch"));
        assert_eq!(mode_of("RenameSession", 14), None);
        assert_eq!(mode_of("NORMAL", 15), None);
        assert_eq!(mode_of("renameSession", 15), None);
    }

    #[test]
    fn left_out_line() {
        assert_eq!(left_out(0), None);
        assert!(left_out(1).is_some());
        assert_eq!(
            left_out(2).as_deref(),
            Some("leaving out 2 binds (a key with no X keysym, or a bind with no action)")
        );
    }

    #[test]
    fn config_path() {
        let (root, _g) = tmp("paths");
        let home = root.join("home");
        let xdg = root.join("xdg");
        let env = |pairs: Vec<(&'static str, String)>| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
        };
        let s = |p: &Path| p.display().to_string();
        let find = |pairs: Vec<(&'static str, String)>| config_file(&env(pairs));
        let system = Path::new(SYSTEM_DIR).join("config.kdl");
        let none = system.exists().then(|| system.clone());
        // Nothing there yet.
        assert_eq!(
            find(vec![("HOME", s(&home)), ("XDG_CONFIG_HOME", s(&xdg))]),
            none
        );
        // ZELLIJ_CONFIG_FILE is taken as is, even missing (reading it then fails, as zellij does).
        assert_eq!(
            find(vec![
                ("ZELLIJ_CONFIG_FILE", "/x/my.kdl".into()),
                ("HOME", s(&home))
            ]),
            Some("/x/my.kdl".into())
        );
        // ZELLIJ_CONFIG_DIR: config.kdl in it, if there.
        let dir = root.join("dir");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(find(vec![("ZELLIJ_CONFIG_DIR", s(&dir))]), None);
        std::fs::write(dir.join("config.kdl"), "").unwrap();
        assert_eq!(
            find(vec![("ZELLIJ_CONFIG_DIR", s(&dir)), ("HOME", s(&home))]),
            Some(dir.join("config.kdl"))
        );
        assert_eq!(
            find(vec![
                ("ZELLIJ_CONFIG_FILE", String::new()),
                ("ZELLIJ_CONFIG_DIR", s(&dir))
            ]),
            Some(dir.join("config.kdl"))
        );
        // $XDG_CONFIG_HOME/zellij when ~/.config/zellij doesn't exist.
        std::fs::create_dir_all(xdg.join("zellij")).unwrap();
        std::fs::write(xdg.join("zellij/config.kdl"), "").unwrap();
        assert_eq!(
            find(vec![("HOME", s(&home)), ("XDG_CONFIG_HOME", s(&xdg))]),
            Some(xdg.join("zellij/config.kdl"))
        );
        // ~/.config/zellij first, once it exists: without a config.kdl, none (the search stops at the directory).
        std::fs::create_dir_all(home.join(".config/zellij")).unwrap();
        assert_eq!(
            find(vec![("HOME", s(&home)), ("XDG_CONFIG_HOME", s(&xdg))]),
            None
        );
        std::fs::write(home.join(".config/zellij/config.kdl"), "").unwrap();
        assert_eq!(
            find(vec![("HOME", s(&home)), ("XDG_CONFIG_HOME", s(&xdg))]),
            Some(home.join(".config/zellij/config.kdl"))
        );
        // An empty or relative XDG_CONFIG_HOME is ~/.config.
        std::fs::remove_dir_all(home.join(".config/zellij")).unwrap();
        // `tests/fixtures/zellij/config.kdl` exists relative to the tests' working directory, the crate root.
        assert!(Path::new("tests/fixtures/zellij/config.kdl").exists());
        for bad in ["", "relative", "tests/fixtures"] {
            assert_eq!(
                find(vec![("HOME", s(&home)), ("XDG_CONFIG_HOME", bad.into())]),
                none,
                "{bad:?}"
            );
        }
        std::fs::create_dir_all(home.join("x/zellij")).unwrap();
        std::fs::write(home.join("x/zellij/config.kdl"), "").unwrap();
        assert_eq!(
            find(vec![
                ("HOME", s(&home)),
                ("XDG_CONFIG_HOME", s(&home.join("x")))
            ]),
            Some(home.join("x/zellij/config.kdl"))
        );
        // No HOME, no XDG: only /etc/zellij.
        assert_eq!(find(vec![]), none);
    }

    #[test]
    fn the_command() {
        assert_eq!(TIMEOUT, Duration::from_secs(2));
        let cmd = command(&["setup", "--dump-keybinds", "default"]);
        assert_eq!(cmd.get_program(), "zellij");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["setup", "--dump-keybinds", "default"]);
        let mise = cmd
            .get_envs()
            .find(|(k, _)| *k == "MISE_NOT_FOUND_AUTO_INSTALL")
            .and_then(|(_, v)| v);
        assert_eq!(mise, Some(std::ffi::OsStr::new("0")));
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/")));
    }

    /// A run of `command(args)` with `PATH` holding a fake `zellij` running `body` first (written through `sh`:
    /// ETXTBSY), and the dir's guard.
    fn fake(name: &str, body: &str) -> (impl Fn(&[&str]) -> anyhow::Result<String>, Cleanup) {
        let (bin, guard) = tmp(&format!("bin-{name}"));
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("zellij"))
            .arg(body)
            .status()
            .unwrap();
        assert!(ok.success());
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let run = move |args: &[&str]| {
            let mut cmd = command(args);
            cmd.env("PATH", &path);
            run_for(cmd, TIMEOUT)
        };
        (run, guard)
    }

    #[test]
    fn a_fake_zellij() {
        let preset = format!(
            "{}/tests/fixtures/zellij/0.46.0/default.kdl",
            env!("CARGO_MANIFEST_DIR")
        );
        let (run, _bin) = fake(
            "ok",
            &format!(
                "case \"$*\" in --version) echo 'zellij 0.46.0';; 'setup --dump-keybinds default') cat '{preset}';; *) exit 9;; esac"
            ),
        );
        let (f, _g) = config(
            "fake",
            "keybinds {\n  normal { bind \"Alt y\" { NewTab; }; }\n  unbind \"Ctrl q\"\n}\n",
        );
        let (sheet, notes) = read(&run, Some(&f)).unwrap();
        assert!(notes.is_empty());
        let rows = rows(&sheet);
        assert!(has(&rows, "normal alt+y = NewTab"), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("ctrl+q")));
        // The preset's 584, less `Ctrl q` in the 14 modes but locked, plus `Alt y`.
        assert_eq!(rows.len(), 584 - 14 + 1);
        // One exiting 1: one error naming the command and zellij's reason.
        let (run, _bin) = fake("fails", "echo 'boom' >&2; exit 1");
        let e = format!("{:#}", read(&run, None).unwrap_err());
        assert!(
            e.starts_with("running zellij --version: ") && e.contains("boom"),
            "{e}"
        );
        // A dump that fails after the version: one error too (the default preset itself can't be had).
        let (run, _bin) = fake(
            "dumpfails",
            "[ \"$1\" = --version ] && { echo 'zellij 0.46.0'; exit 0; }; echo nope >&2; exit 1",
        );
        let e = format!("{:#}", read(&run, None).unwrap_err());
        assert!(
            e.starts_with("running zellij setup --dump-keybinds default: ") && e.contains("nope"),
            "{e}"
        );
        // Not installed.
        let run = |args: &[&str]| {
            let mut cmd = command(args);
            cmd.env("PATH", "/nonexistent");
            run_for(cmd, TIMEOUT)
        };
        let e = read(&run, None).unwrap_err();
        assert_eq!(
            e.root_cause()
                .downcast_ref::<std::io::Error>()
                .map(|e| e.kind()),
            Some(std::io::ErrorKind::NotFound),
            "{e:#}"
        );
        // A hang is killed at the timeout: waited for (so it ran), then given up.
        let (run, _bin) = fake("hangs", "sleep 10");
        let started = std::time::Instant::now();
        let e = format!("{:#}", read(&run, None).unwrap_err());
        let took = started.elapsed();
        assert!(e.starts_with("running zellij --version"), "{e}");
        assert!(
            took >= TIMEOUT - Duration::from_millis(100) && took < TIMEOUT + Duration::from_secs(2),
            "{took:?}"
        );
    }
}
