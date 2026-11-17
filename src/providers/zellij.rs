//! zellij: its keys in every mode, from the installed zellij's own defaults with the user's `config.kdl` applied the
//! way zellij applies it. From 0.46 the defaults are a keybinding preset (`zellij setup --dump-keybinds=<preset>`, the
//! preset and its leader keys chosen in `config.kdl`); before, the `keybinds` of `zellij setup --dump-config`. Each
//! bind is tagged with its mode (`input_mode`): zellij has no query for a client's mode, so every mode is listed.
//! Shown when a terminal's foreground program is zellij.
//!
//! ponytail: the preset is whatever `zellij setup --dump-keybinds=<name>` prints: zellij's built-ins first, then only
//! `<default config dir>/keybinds/<name>.kdl`. A running zellij also looks in `$ZELLIJ_CONFIG_DIR/keybinds` and a
//! `keybinds_dir` option, takes a path as the preset, and lets a user `keybinds/default.kdl` replace the built-in one:
//! those aren't followed (the fallback note says "built-in default"). Nor are a client started with `--config`,
//! `zellij options --keybinds-*` or a layout's keybinds (one session each). keybind-find reads its own environment and
//! `PATH`, not the zellij client's: `ZELLIJ_CONFIG_*` or `XDG_CONFIG_HOME` set only in the shell, a zellij not on the
//! launcher's `PATH`, or a client still running an older zellij after an upgrade, aren't followed. Actions are shown
//! as written, never checked.

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
    sheet_with(
        &|k| std::env::var(k).ok(),
        &|args| run_for(command(args), TIMEOUT),
        &mut std::io::stderr(),
    )
}

/// The sheet from the config `env` points at and zellij as `run` runs it, each note a `keybind-find: zellij: ` line on
/// `err`.
fn sheet_with(env: Env, run: Run, err: &mut dyn std::io::Write) -> anyhow::Result<Sheet> {
    let (sheet, notes) = read(run, config_file(env).as_deref())?;
    for note in notes {
        // A note that can't be written is lost; the sheet still is the result.
        let _ = writeln!(err, "keybind-find: zellij: {note}");
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
    let (mut binds, unknown) = if new {
        let (preset, leaders) = match user {
            Some(k) => selection(k, user_text).map_err(in_file)?,
            None => ("default".to_string(), BTreeMap::new()),
        };
        let (binds, unknown, note) = defaults(&preset, &leaders, run)?;
        notes.extend(note);
        (binds, unknown)
    } else {
        let out = run(&["setup", "--dump-config"]).context("running zellij setup --dump-config")?;
        dump_config_binds(&out).context("reading zellij setup --dump-config")?
    };
    if let Some(user) = user {
        Src::user(user_text)
            .apply(&mut binds, user, new)
            .map_err(in_file)?;
    }
    let (sheet, skipped) = sheet_of(&binds)?;
    notes.extend(left_out(skipped, unknown));
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
/// a relative `XDG_CONFIG_HOME` too (`directories`' rule). ponytail: a relative `ZELLIJ_CONFIG_FILE` is read from
/// keybind-find's working directory, not the zellij client's (README says so).
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

/// A KDL text being applied: the text (for line numbers), a preset's leader values (`leaders`) and those of them the
/// user gave (`requested`), and whether it is zellij's own built-in text (`own`: `--dump-config`, the `default` and
/// `unlock-first` presets). Zellij's own text is read leniently: a mode, key, placeholder or instruction this
/// keybind-find doesn't know (a newer zellij's) is left out and counted in `unknown`, not an error; a key holding a
/// placeholder the user gave a value is still strict (that value is the user's). The user's config and presets are
/// strict, as zellij is.
#[derive(Default)]
struct Src<'a> {
    text: &'a str,
    leaders: Option<&'a BTreeMap<String, String>>,
    requested: Option<&'a BTreeMap<String, String>>,
    own: bool,
    unknown: std::cell::Cell<usize>,
}

impl<'a> Src<'a> {
    fn user(text: &'a str) -> Src<'a> {
        Src {
            text,
            ..Default::default()
        }
    }

    /// One more of zellij's own binds left out.
    fn count(&self, n: usize) {
        self.unknown.set(self.unknown.get() + n);
    }

    /// An error at `node`'s line.
    fn at(&self, node: &KdlNode, msg: impl std::fmt::Display) -> anyhow::Error {
        anyhow!("line {}: {msg}", kdl_v1::line(self.text, node))
    }

    /// A node's entries as strings (`kdl_string_arguments!`: properties too); any other value is an error.
    fn strings<'n>(&self, node: &'n KdlNode) -> anyhow::Result<Vec<&'n str>> {
        node.entries()
            .iter()
            .map(|e| {
                e.value().as_string().ok_or_else(|| {
                    self.at(node, format!("{} is not a string", value_text(e.value())))
                })
            })
            .collect()
    }

    /// `clear-defaults` on `node` (its last such property): `true`, `false`, or an error for anything else.
    fn clears(&self, node: &KdlNode) -> anyhow::Result<bool> {
        match node.get("clear-defaults") {
            None => Ok(false),
            Some(v) => v.as_bool().ok_or_else(|| {
                self.at(
                    node,
                    format!(
                        "clear-defaults must be true or false, not {}",
                        value_text(v)
                    ),
                )
            }),
        }
    }

    /// The keys of a `bind` or `unbind` node, each with the leaders' placeholders put in first (a preset's keys; one
    /// with no value for a placeholder is invalid). In zellij's own text an invalid key is skipped, and counted when
    /// `counted` (a bind's), unless every placeholder had a value and the user gave one of them.
    fn keys(&self, node: &KdlNode, counted: bool) -> anyhow::Result<Vec<Key>> {
        let mut keys = Vec::new();
        for raw in self.strings(node)? {
            let k = match self.leaders {
                Some(values) => substitute(raw, values),
                None => Some(raw.to_string()),
            };
            let users = || {
                placeholders_in(raw)
                    .iter()
                    .any(|p| self.requested.is_some_and(|r| r.contains_key(*p)))
            };
            match k.as_deref().and_then(Key::parse) {
                Some(key) => keys.push(key),
                None if self.own && !(k.is_some() && users()) => self.count(usize::from(counted)),
                None => {
                    let k = k.as_deref().unwrap_or(raw);
                    return Err(self.at(node, format!("invalid key {k:?}")));
                }
            }
        }
        Ok(keys)
    }

    /// The binds and unbinds of one mode or `shared*` block into `binds`: every `bind` first, then every `unbind`
    /// (`bind_keys_in_block`); anything else is an error, as is a block with no `{ }`.
    fn bind_block(
        &self,
        binds: &mut BTreeMap<Key, Actions>,
        block: &KdlNode,
    ) -> anyhow::Result<()> {
        let nodes = block
            .children()
            .ok_or_else(|| {
                self.at(
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
                        .ok_or_else(|| self.at(node, "a bind with no { } block of actions"))?
                        .nodes()
                        .iter()
                        .map(action_text)
                        .collect::<Actions>();
                    for key in self.keys(node, true)? {
                        binds.insert(key, actions.clone());
                    }
                }
                "unbind" => {}
                _ if self.own => self.count(1),
                other => {
                    return Err(self.at(node, format!("unknown keybind instruction {other:?}")))
                }
            }
        }
        for node in nodes.iter().filter(|n| n.name().value() == "unbind") {
            for key in self.keys(node, false)? {
                binds.remove(&key);
            }
        }
        Ok(())
    }

    /// How many binds `block` makes in a mode unknown here: they go into a mode of their own, only to be counted.
    fn unknown_binds(&self, block: &KdlNode) -> anyhow::Result<usize> {
        let mut unknown = BTreeMap::new();
        self.bind_block(&mut unknown, block)?;
        Ok(unknown.len())
    }

    /// A mode named in `block` (`count`: how many of `MODES` this zellij has). In zellij's own text an unknown one is
    /// `None`, to be skipped.
    fn mode(
        &self,
        name: &str,
        block: &KdlNode,
        count: usize,
    ) -> anyhow::Result<Option<&'static str>> {
        match mode_of(name, count) {
            Some(m) => Ok(Some(m)),
            None if self.own => Ok(None),
            None => Err(self.at(block, format!("unknown mode {name:?}"))),
        }
    }

    /// One `keybinds` block applied to `binds`, as zellij applies it (0.46 `parse_bind_blocks`, 0.45
    /// `Keybinds::from_kdl`): `clear-defaults=true` empties everything; then every `shared`/`shared_except` (all modes
    /// but those named) and `shared_among` (only those) block in order; then each mode block in order (its
    /// `clear-defaults=true` empties the mode first); then the top-level `unbind`s in every mode (all of them from 0.46,
    /// only the first before). `new`: 0.46 or later. In zellij's own text, a mode block of an unknown mode is skipped,
    /// its binds counted; an unknown mode in a `shared*` list is passed over (keybind-find has no such mode to bind in).
    fn apply(&self, binds: &mut Binds, keybinds: &KdlNode, new: bool) -> anyhow::Result<()> {
        let count = if new { MODES.len() } else { MODES.len() - 1 };
        if self.clears(keybinds)? {
            binds.clear();
        }
        let Some(children) = keybinds.children() else {
            // 0.45 refuses a `keybinds` with no `{ }`; 0.46 takes it.
            return match new {
                true => Ok(()),
                false => Err(self.at(keybinds, "keybinds has no { } block")),
            };
        };
        let nodes = children.nodes();
        for block in nodes {
            let except = match block.name().value() {
                "shared" | "shared_except" => true,
                "shared_among" => false,
                _ => continue,
            };
            let mut named = Vec::new();
            let names = self.strings(block)?;
            for m in &names {
                named.extend(self.mode(m, block, count)?);
            }
            if !except && named.is_empty() && !names.is_empty() {
                // `shared_among` only modes unknown here: its binds, counted.
                self.count(self.unknown_binds(block)?);
                continue;
            }
            for &(mode, _) in &MODES[..count] {
                if named.contains(&mode) != except {
                    self.bind_block(binds.entry(mode).or_default(), block)?;
                }
            }
        }
        for block in nodes {
            let name = block.name().value();
            if ["unbind", "shared", "shared_except", "shared_among"].contains(&name) {
                continue;
            }
            let Some(mode) = self.mode(name, block, count)? else {
                self.count(self.unknown_binds(block)?);
                continue;
            };
            let mode_binds = binds.entry(mode).or_default();
            if self.clears(block)? {
                mode_binds.clear();
            }
            self.bind_block(mode_binds, block)?;
        }
        let unbinds = nodes.iter().filter(|n| n.name().value() == "unbind");
        for node in unbinds.take(if new { usize::MAX } else { 1 }) {
            for key in self.keys(node, false)? {
                for mode_binds in binds.values_mut() {
                    mode_binds.remove(&key);
                }
            }
        }
        Ok(())
    }
}

/// The `{…}` placeholders in `text`, in order (`placeholders_in`).
fn placeholders_in(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some((name, after)) = rest.split_once('{').and_then(|(_, r)| r.split_once('}')) {
        found.push(name);
        rest = after;
    }
    found
}

/// `key` with its placeholders put in as zellij's `substitute_placeholders` does: for each placeholder in the original
/// key, in order, every `{name}` in the text so far is replaced by its value (so a value holding a later placeholder's
/// `{…}` is replaced again, and one holding an earlier one's stays); then whitespace collapsed. `None` when one has no
/// value.
fn substitute(key: &str, values: &BTreeMap<String, String>) -> Option<String> {
    let mut out = key.to_string();
    for name in placeholders_in(key) {
        out = out.replace(&format!("{{{name}}}"), values.get(name)?);
    }
    Some(out.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// The preset and leader values the user's `keybinds` node selects (`KeybindsSelection::from_kdl`): `preset` (default
/// `default`) and the `primary`, `secondary` and `unlock` properties, each a string.
fn selection(keybinds: &KdlNode, text: &str) -> anyhow::Result<(String, BTreeMap<String, String>)> {
    let src = Src::user(text);
    let string = |name: &str| -> anyhow::Result<Option<String>> {
        keybinds
            .get(name)
            .map(|v| {
                v.as_string().map(str::to_string).ok_or_else(|| {
                    src.at(
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

/// zellij's built-in presets (`BUILTIN_KEYBIND_PRESETS`): `--dump-keybinds` prints these from zellij itself, and any
/// other name from the user's `keybinds/<name>.kdl`.
const BUILT_IN: [&str; 2] = ["default", "unlock-first"];

/// What `zellij setup --dump-keybinds` says (exit 1) when it has no preset of that name (`dump_specified_keybinds`).
const NOT_FOUND: &str = "Keybinding preset not found: ";

/// zellij's defaults from 0.46: the preset named `preset` as `zellij setup --dump-keybinds=<preset>` prints it (zellij's
/// built-in presets, then `<default config dir>/keybinds/<preset>.kdl`), with the user's `leaders`; the binds, how
/// many of zellij's own were unknown here, and a note. When zellij has no such preset, or the preset doesn't apply (a
/// leader empty, missing or equal to another, a key the leaders make invalid): the `default` preset with its own
/// leaders, which is what zellij falls back to, and a note saying why. Any other failure of the run (a timeout, no
/// zellij) is the provider's error.
fn defaults(
    preset: &str,
    leaders: &BTreeMap<String, String>,
    run: Run,
) -> anyhow::Result<(Binds, usize, Option<String>)> {
    let dump = |name: &str| {
        run(&["setup", &format!("--dump-keybinds={name}")])
            .with_context(|| format!("running zellij setup --dump-keybinds={name}"))
    };
    let note = |e: &anyhow::Error| {
        format!(
            "can't use the keybinding preset {preset:?} ({e:#}); showing the built-in default preset with its own \
             leader keys"
        )
    };
    let text = match dump(preset) {
        Ok(text) => text,
        // The run was for this preset (and `run_for` may cut or respace the name).
        Err(e) if format!("{e:#}").contains(NOT_FOUND) => {
            let (binds, unknown) = preset_binds(&dump("default")?, &BTreeMap::new(), true)
                .context("reading the default preset")?;
            return Ok((binds, unknown, Some(note(&e))));
        }
        Err(e) => return Err(e),
    };
    match preset_binds(&text, leaders, BUILT_IN.contains(&preset)) {
        Ok((binds, unknown)) => Ok((binds, unknown, None)),
        Err(e) => {
            let text = match preset {
                "default" => text,
                _ => dump("default")?,
            };
            let (binds, unknown) = preset_binds(&text, &BTreeMap::new(), true)
                .context("reading the default preset")?;
            Ok((binds, unknown, Some(note(&e))))
        }
    }
}

/// A 0.46 preset's binds (`KeybindPreset::from_kdl`, `leader_values`, `keybinds`) and how many of its binds were
/// unknown here: each placeholder its keys use takes the user's value, else the preset's `preset { defaults { … } }`
/// one, whitespace collapsed; an empty value, a placeholder with none, or `primary` and `secondary` with the same
/// modifiers is an error. `own`: a built-in preset, read leniently (`Src`); a user's preset is strict, its header
/// checked as zellij checks it. `mousebinds` is never read.
fn preset_binds(
    text: &str,
    requested: &BTreeMap<String, String>,
    own: bool,
) -> anyhow::Result<(Binds, usize)> {
    let doc = kdl_v1::parse(text)?;
    let mut defaults = BTreeMap::new();
    if let Some(header) = doc.get("preset").and_then(KdlNode::children) {
        if !own {
            for example in header
                .nodes()
                .iter()
                .filter(|n| n.name().value() == "example")
            {
                let strings = example
                    .entries()
                    .iter()
                    .filter(|e| e.name().is_none() && e.value().is_string())
                    .count();
                if strings != 2 {
                    anyhow::bail!("an example in the preset header needs two strings");
                }
            }
            if let Some(mode) = header.get("default_mode").and_then(first) {
                if mode_of(mode, MODES.len()).is_none() {
                    anyhow::bail!("invalid default_mode in the preset: {mode:?}");
                }
            }
        }
        for node in header
            .get("defaults")
            .and_then(KdlNode::children)
            .map(|d| d.nodes())
            .unwrap_or_default()
        {
            let name = node.name().value();
            match first(node) {
                _ if !own && !LEADERS.contains(&name) => {
                    anyhow::bail!("unknown default {name:?} in the preset header")
                }
                Some(value) => _ = defaults.insert(name.to_string(), value.to_string()),
                None if own => {}
                None => anyhow::bail!("the default for {name} must be a string"),
            }
        }
    }
    let keybinds = doc
        .get("keybinds")
        .ok_or_else(|| anyhow!("the preset has no keybinds block"))?;
    let mut used = Vec::new();
    placeholders(keybinds, &mut used, own)?;
    let mut values = BTreeMap::new();
    for name in used {
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
        // As sets of words: `Ctrl Ctrl` is `Ctrl`.
        let set = |v: &str| {
            v.split_whitespace()
                .map(str::to_lowercase)
                .collect::<std::collections::BTreeSet<_>>()
        };
        if set(p) == set(s) {
            anyhow::bail!("primary and secondary are both {p:?}");
        }
    }
    let mut binds = Binds::new();
    let src = Src {
        text,
        leaders: Some(&values),
        requested: Some(requested),
        own,
        ..Default::default()
    };
    src.apply(&mut binds, keybinds, true)?;
    Ok((binds, src.unknown.get()))
}

/// `node`'s first unnamed argument, if a string (`first_string_argument`).
fn first(node: &KdlNode) -> Option<&str> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
}

/// The leader placeholders in `node`'s `bind` and `unbind` keys, and its children's (`collect_placeholders`, which
/// reads arguments only: a placeholder only in a property gets no value, and that key is then invalid, as in zellij).
/// One not in `LEADERS` is an error, but in a built-in preset (`own`) it is passed over: its keys are then left out.
fn placeholders(node: &KdlNode, found: &mut Vec<String>, own: bool) -> anyhow::Result<()> {
    if ["bind", "unbind"].contains(&node.name().value()) {
        for k in node
            .entries()
            .iter()
            .filter(|e| e.name().is_none())
            .filter_map(|e| e.value().as_string())
        {
            for name in placeholders_in(k) {
                if !LEADERS.contains(&name) {
                    match own {
                        true => continue,
                        false => anyhow::bail!("unknown placeholder {{{name}}} in the key {k:?}"),
                    }
                }
                if !found.iter().any(|f| f == name) {
                    found.push(name.to_string());
                }
            }
        }
        return Ok(());
    }
    for child in node.children().map(|c| c.nodes()).unwrap_or_default() {
        placeholders(child, found, own)?;
    }
    Ok(())
}

/// zellij's defaults before 0.46: the `keybinds` of `--dump-config` (its default config, no placeholders), and how many
/// of its binds were unknown here.
fn dump_config_binds(text: &str) -> anyhow::Result<(Binds, usize)> {
    let doc = kdl_v1::parse(text)?;
    let keybinds = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "keybinds")
        .ok_or_else(|| anyhow!("no keybinds block"))?;
    let mut binds = Binds::new();
    let src = Src {
        text,
        own: true,
        ..Default::default()
    };
    src.apply(&mut binds, keybinds, false)?;
    Ok((binds, src.unknown.get()))
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

/// The sheet of `binds`, grouped by each bind's first action, and how many binds were left out (a key with no X
/// keysym). A bind with no action isn't listed, nor counted.
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
            // `bind "x" { }` binds nothing (it only hides a default); nothing to list.
            let Some(first) = actions.first() else {
                continue;
            };
            let Some(k) = key.notation() else {
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

/// The note for left-out binds: `skipped` keybind-find can't send (a key with no X keysym) and `unknown` of zellij's own
/// with a mode, key, placeholder or instruction keybind-find doesn't know (a newer zellij's); none when there are none.
fn left_out(skipped: usize, unknown: usize) -> Option<String> {
    let mut why = Vec::new();
    if skipped > 0 {
        why.push(format!("{skipped} with a key with no X keysym"));
    }
    if unknown > 0 {
        why.push(format!(
            "{unknown} of zellij's own that this keybind-find doesn't know"
        ));
    }
    (!why.is_empty()).then(|| {
        format!(
            "leaving out {} binds ({})",
            skipped + unknown,
            why.join("; ")
        )
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
            true => preset_binds(defaults, &BTreeMap::new(), true)?.0,
            false => dump_config_binds(defaults)?.0,
        };
        let doc = kdl_v1::parse(user)?;
        if let Some(k) = doc.nodes().iter().find(|n| n.name().value() == "keybinds") {
            Src::user(user).apply(&mut binds, k, new)?;
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
        let (binds, unknown) =
            preset_binds(&fixture("0.46.0/default.kdl"), &BTreeMap::new(), true).unwrap();
        assert_eq!(unknown, 0);
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
                &sheet_of(
                    &preset_binds(&fixture("0.46.0/default.kdl"), &leaders(l), true)
                        .unwrap()
                        .0,
                )
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
                &sheet_of(
                    &preset_binds(&fixture("0.46.0/unlock-first.kdl"), &leaders(l), true)
                        .unwrap()
                        .0,
                )
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
            let e = format!(
                "{:#}",
                preset_binds(&preset, &leaders(&l), true).unwrap_err()
            );
            assert!(e.contains(why), "{l:?}: {e}");
        }
        // A placeholder with no value, an unknown one, no keybinds.
        let e = |t: &str| {
            format!(
                "{:#}",
                preset_binds(t, &BTreeMap::new(), false).unwrap_err()
            )
        };
        assert!(e("keybinds { normal { bind \"{unlock}\" { Quit; }; }; }")
            .contains("gives no value for it"));
        assert!(
            e("keybinds { normal { bind \"{tertiary} x\" { Quit; }; }; }")
                .contains("unknown placeholder {tertiary} in the key \"{tertiary} x\"")
        );
        assert!(e("preset { }").contains("no keybinds block"));
        assert!(e("keybinds {").starts_with("line 1: "));
    }

    #[test]
    fn the_default_config_before_0_46() {
        let (binds, unknown) = dump_config_binds(&fixture("0.45.1/default.kdl")).unwrap();
        assert_eq!(unknown, 0);
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
                ["setup", "--dump-keybinds=default"] => Ok(fixture("0.46.0/default.kdl")),
                ["setup", "--dump-keybinds=unlock-first"] => Ok(fixture("0.46.0/unlock-first.kdl")),
                ["setup", "--dump-keybinds=slow"] => anyhow::bail!("zellij timed out after 2s"),
                // zellij's own message for a preset it doesn't have, as `run_for` reports it.
                ["setup", p] if p.starts_with("--dump-keybinds=") => anyhow::bail!(
                    "zellij failed (exit status: 1): Keybinding preset not found: {}",
                    &p["--dump-keybinds=".len()..]
                ),
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
        assert_eq!(calls, ["--version", "setup --dump-keybinds=default"]);
        let (_, calls) = read_with("0.45.1", None);
        assert_eq!(calls, ["--version", "setup --dump-config"]);
        let (f, _g) = config("preset", "keybinds preset=\"unlock-first\" {\n}\n");
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(calls, ["--version", "setup --dump-keybinds=unlock-first"]);
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
                "setup --dump-keybinds=mine",
                "setup --dump-keybinds=default"
            ]
        );
        let (sheet, notes) = out.unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("can't use the keybinding preset \"mine\" (running zellij setup --dump-keybinds=mine: zellij failed (exit status: 1): Keybinding preset not found: mine); showing the built-in default preset with its own leader keys"), "{notes:?}");
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
            // The default preset's text, dumped once, read again with its own leaders.
            ["--version", "setup --dump-keybinds=default"]
        );
        let (sheet, notes) = out.unwrap();
        assert_eq!(
            notes,
            ["can't use the keybinding preset \"default\" (primary and secondary are both \"Alt\"); showing the built-in default preset with its own leader keys"]
        );
        let rows = rows(&sheet);
        assert!(has(&rows, "normal ctrl+p = SwitchToMode \"Pane\""));
        // The user's own binds still apply on top.
        assert!(has(&rows, "normal alt+y = NewTab"));
    }

    #[test]
    fn a_run_failure_is_no_fallback() {
        // A timeout (or a crash, or no zellij) isn't a missing preset: the provider's one error, no second run.
        let (f, _g) = config("slow", "keybinds preset=\"slow\" {\n}\n");
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(calls, ["--version", "setup --dump-keybinds=slow"]);
        assert_eq!(
            format!("{:#}", out.unwrap_err()),
            "running zellij setup --dump-keybinds=slow: zellij timed out after 2s"
        );
        // Not found: whatever name zellij's message gives (`run_for` collapses whitespace in it).
        let run = |args: &[&str]| -> anyhow::Result<String> {
            match args {
                ["setup", "--dump-keybinds=my  preset"] => anyhow::bail!(
                    "zellij failed (exit status: 1): Keybinding preset not found: my preset"
                ),
                ["setup", "--dump-keybinds=default"] => Ok(fixture("0.46.0/default.kdl")),
                _ => anyhow::bail!("unexpected {args:?}"),
            }
        };
        let (binds, _, note) = defaults("my  preset", &BTreeMap::new(), &run).unwrap();
        assert!(note
            .unwrap()
            .contains("Keybinding preset not found: my preset"));
        assert!(binds["normal"].contains_key(&Key::parse("Ctrl p").unwrap()));
        // A preset that isn't `default` with bad leaders: the default preset is dumped.
        let (f, _g) = config(
            "uf",
            "keybinds preset=\"unlock-first\" primary=\"Super\" unlock=\"\" {\n}\n",
        );
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(
            calls,
            [
                "--version",
                "setup --dump-keybinds=unlock-first",
                "setup --dump-keybinds=default"
            ]
        );
        let (sheet, notes) = out.unwrap();
        assert!(
            notes[0].contains("the value for unlock is empty"),
            "{notes:?}"
        );
        assert!(has(
            &rows(&sheet),
            "locked ctrl+g = SwitchToMode \"Normal\""
        ));
        assert!(has(&rows(&sheet), "normal ctrl+p = SwitchToMode \"Pane\""));
    }

    #[test]
    fn newer_versions_read_presets() {
        for v in ["0.47.0", "1.0.0"] {
            let (out, calls) = read_with(v, None);
            assert_eq!(calls, ["--version", "setup --dump-keybinds=default"], "{v}");
            assert!(out.is_ok());
        }
    }

    #[test]
    fn zellijs_own_text_is_lenient() {
        let preset = "keybinds {\n  newmode {\n    bind \"a\" \"b\" { Quit; }\n  }\n\
                      shared_among \"newmode\" \"pane\" {\n    bind \"x\" { Detach; }\n  }\n\
                      pane {\n    bind \"F13\" \"c\" { Quit; }\n    unbind \"F14\"\n  }\n}\n";
        let (binds, unknown) = preset_binds(preset, &BTreeMap::new(), true).unwrap();
        // newmode's two binds and F13.
        assert_eq!(unknown, 3);
        assert_eq!(
            rows(&sheet_of(&binds).unwrap().0),
            ["pane c = Quit", "pane x = Detach"]
        );
        let (binds, unknown) = dump_config_binds(preset).unwrap();
        assert_eq!((unknown, binds["pane"].len()), (3, 2));
        // The count reaches read()'s note.
        let run = |args: &[&str]| -> anyhow::Result<String> {
            match args {
                ["--version"] => Ok("zellij 0.46.0".into()),
                _ => Ok(preset.into()),
            }
        };
        let (_, notes) = read(&run, None).unwrap();
        assert_eq!(
            notes,
            ["leaving out 3 binds (3 of zellij's own that this keybind-find doesn't know)"]
        );
        // A user's key keybind-find can't send: one note.
        let (f, _g) = config("e", "keybinds {\n  normal { bind \"é\" { Quit; }; }\n}\n");
        let (out, _) = read_with("0.46.0", Some(&f));
        assert_eq!(
            out.unwrap().1,
            ["leaving out 1 binds (1 with a key with no X keysym)"]
        );
        // A key a user's leader makes invalid is still an error (then the default preset).
        let with_leader = "keybinds {\n  pane {\n    bind \"{primary} F13\" { Quit; }\n  }\n}\n";
        let e = preset_binds(with_leader, &leaders(&[("primary", "Ctrl")]), true).unwrap_err();
        assert_eq!(format!("{e:#}"), "line 3: invalid key \"Ctrl F13\"");
        // The user's config stays strict.
        let (f, _g) = config("strict", "keybinds {\n  newmode { }\n}\n");
        assert!(
            format!("{:#}", read_with("0.46.0", Some(&f)).0.unwrap_err())
                .ends_with("line 2: unknown mode \"newmode\"")
        );
    }

    #[test]
    fn placeholders_as_zellij_collects_them() {
        // Only in an unbind: still collected, so the key reads.
        let p = "preset { defaults { primary \"Ctrl\"; }; }\nkeybinds {\n  pane {\n    bind \"x\" { Quit; }\n\
                 bind \"Ctrl y\" { Quit; }\n    unbind \"{primary} y\"\n  }\n}\n";
        assert_eq!(
            rows(
                &sheet_of(&preset_binds(p, &BTreeMap::new(), false).unwrap().0)
                    .unwrap()
                    .0
            ),
            ["pane x = Quit"]
        );
        // Only in a property: zellij doesn't collect it (`collect_placeholders` skips named entries), so it has no
        // value and the key is invalid, as zellij finds it ("No value for the placeholder").
        let p = "preset { defaults { primary \"Ctrl\"; }; }\nkeybinds {\n  pane {\n    bind k=\"{primary} x\" { Quit; }\n  }\n}\n";
        let e = format!(
            "{:#}",
            preset_binds(p, &BTreeMap::new(), false).unwrap_err()
        );
        assert_eq!(e, "line 4: invalid key \"{primary} x\"");
        // Also in an argument: collected there, and the property's key reads too.
        let p = "preset { defaults { primary \"Ctrl\"; }; }\nkeybinds {\n  pane {\n    bind \"{primary} a\" k=\"{primary} x\" { Quit; }\n  }\n}\n";
        assert_eq!(
            rows(
                &sheet_of(&preset_binds(p, &BTreeMap::new(), false).unwrap().0)
                    .unwrap()
                    .0
            ),
            ["pane ctrl+a = Quit", "pane ctrl+x = Quit"]
        );
    }

    #[test]
    fn top_level_unbind_comes_last() {
        let user = "keybinds clear-defaults=true {\n  unbind \"Ctrl g\"\n  normal { bind \"Ctrl g\" { Quit; }; }\n\
                    shared { bind \"Ctrl g\" { Detach; }; }\n}\n";
        for new in [true, false] {
            let defaults = match new {
                true => PRESET,
                false => "keybinds {\n  normal { bind \"x\" { Quit; }; }\n}\n",
            };
            assert_eq!(
                merged(defaults, user, new).unwrap(),
                Vec::<String>::new(),
                "{new}"
            );
        }
    }

    #[test]
    fn the_sheet_from_the_environment() {
        // The config `env` points at is read, and each note is a `keybind-find: zellij: ` line.
        let (f, _g) = config(
            "seam",
            "keybinds {\n  normal { bind \"Alt y\" { NewTab; }; bind \"é\" { Quit; }; }\n}\n",
        );
        let path = f.display().to_string();
        let env = |k: &str| (k == "ZELLIJ_CONFIG_FILE").then(|| path.clone());
        let run = |args: &[&str]| -> anyhow::Result<String> {
            match args {
                ["--version"] => Ok("zellij 0.46.0\n".into()),
                ["setup", "--dump-keybinds=default"] => Ok(fixture("0.46.0/default.kdl")),
                _ => anyhow::bail!("unexpected {args:?}"),
            }
        };
        let mut err = Vec::new();
        let sheet = sheet_with(&env, &run, &mut err).unwrap();
        assert!(has(&rows(&sheet), "normal alt+y = NewTab"));
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "keybind-find: zellij: leaving out 1 binds (1 with a key with no X keysym)\n"
        );
        // Two notes, two lines.
        let (f2, _g2) = config(
            "seam2",
            "keybinds primary=\"Alt\" {\n  normal { bind \"é\" { Quit; }; }\n}\n",
        );
        let path = f2.display().to_string();
        let env = |k: &str| (k == "ZELLIJ_CONFIG_FILE").then(|| path.clone());
        let mut err = Vec::new();
        sheet_with(&env, &run, &mut err).unwrap();
        let err = String::from_utf8(err).unwrap();
        let lines: Vec<&str> = err.lines().collect();
        assert_eq!(lines.len(), 2, "{err}");
        assert!(lines
            .iter()
            .all(|l| l.starts_with("keybind-find: zellij: ")));
    }

    #[test]
    fn user_presets_are_strict() {
        let strict = |t: &str| {
            format!(
                "{:#}",
                preset_binds(t, &BTreeMap::new(), false).unwrap_err()
            )
        };
        let lenient = |t: &str| preset_binds(t, &BTreeMap::new(), true).map(|(_, n)| n);
        // What a built-in preset may hold (a newer zellij's), a user's preset may not.
        let newer = "keybinds {\n  newmode { bind \"a\" { Quit; }; }\n}\n";
        assert_eq!(strict(newer), "line 2: unknown mode \"newmode\"");
        assert_eq!(lenient(newer).unwrap(), 1);
        let unknown_default = "preset { defaults { tertiary \"x\"; }; }\nkeybinds { }\n";
        assert_eq!(
            strict(unknown_default),
            "unknown default \"tertiary\" in the preset header"
        );
        assert_eq!(lenient(unknown_default).unwrap(), 0);
        // The header as `KeybindPreset::from_kdl` checks it.
        for (header, why) in [
            (
                "example \"a\"",
                Some("an example in the preset header needs two strings"),
            ),
            (
                "example \"a\" \"b\" \"c\"",
                Some("an example in the preset header needs two strings"),
            ),
            ("example \"a\" \"b\" 1 k=\"c\"", None),
            (
                "default_mode \"Bogus\"",
                Some("invalid default_mode in the preset: \"Bogus\""),
            ),
            ("default_mode \"Locked\"", None),
            ("default_mode 1", None),
            (
                "defaults { primary k=\"x\" \"Ctrl\"; secondary 1; }",
                Some("the default for secondary must be a string"),
            ),
        ] {
            let t = format!("preset {{ {header}; }}\nkeybinds {{ }}\n");
            match why {
                Some(why) => assert_eq!(strict(&t), why, "{header}"),
                None => assert!(
                    preset_binds(&t, &BTreeMap::new(), false).is_ok(),
                    "{header}"
                ),
            }
            assert!(lenient(&t).is_ok(), "{header}");
        }
        // The first unnamed string argument is the default.
        let p = "preset { defaults { primary k=\"x\" \"Ctrl\"; }; }\nkeybinds {\n  pane { bind \"{primary} x\" { Quit; }; }\n}\n";
        assert_eq!(
            rows(
                &sheet_of(&preset_binds(p, &BTreeMap::new(), false).unwrap().0)
                    .unwrap()
                    .0
            ),
            ["pane ctrl+x = Quit"]
        );
        // A bad user preset falls back to the built-in default with a note; the same text as a built-in doesn't.
        let run = |args: &[&str]| -> anyhow::Result<String> {
            match args {
                ["setup", "--dump-keybinds=default"] => Ok(fixture("0.46.0/default.kdl")),
                _ => Ok(newer.into()),
            }
        };
        let (binds, _, note) = defaults("mine", &BTreeMap::new(), &run).unwrap();
        assert!(note.unwrap().contains("unknown mode \"newmode\""));
        assert!(binds["normal"].contains_key(&Key::parse("Ctrl p").unwrap()));
        let (_, unknown, note) = defaults("unlock-first", &BTreeMap::new(), &run).unwrap();
        assert_eq!((unknown, note), (1, None));
    }

    #[test]
    fn built_in_text_left_out() {
        let p = "preset { defaults { primary \"Ctrl\"; }; }\nkeybinds {\n\
                 pane {\n    bind \"{hyper} x\" { Quit; }\n    bind \"{primary} F13\" { Quit; }\n    bind \"y\" { Quit; }\n    bnid \"z\" { }\n  }\n\
                 newmode {\n    bind \"a\" \"b\" { Quit; }\n    unbind \"a\"\n  }\n\
                 newmode2 { bnid \"x\" { }; }\n\
                 shared_among \"newmode\" { bind \"s\" \"t\" { Quit; }; }\n\
                 shared_among \"newmode\" \"pane\" { bind \"u\" { Quit; }; }\n}\n";
        let (binds, unknown) = preset_binds(p, &BTreeMap::new(), true).unwrap();
        assert_eq!(
            rows(&sheet_of(&binds).unwrap().0),
            ["pane u = Quit", "pane y = Quit"]
        );
        // {hyper} x, {primary} F13, bnid, newmode's b (a unbound), newmode2's bnid, s and t.
        assert_eq!(unknown, 7);
        // The same in a user's preset: errors.
        let strict = |t: &str| {
            format!(
                "{:#}",
                preset_binds(t, &BTreeMap::new(), false).unwrap_err()
            )
        };
        assert_eq!(
            strict(p),
            "unknown placeholder {hyper} in the key \"{hyper} x\""
        );
        let bnid = "keybinds {\n  pane { bnid \"z\" { }; }\n}\n";
        assert_eq!(strict(bnid), "line 2: unknown keybind instruction \"bnid\"");
        // A key holding the user's own leader value stays strict, in a built-in preset too.
        let user = leaders(&[("primary", "Alt")]);
        let e = format!("{:#}", preset_binds(p, &user, true).unwrap_err());
        assert_eq!(e, "line 5: invalid key \"Alt F13\"");
        // ... but not one whose other placeholder is unknown here (a newer zellij's): left out and counted.
        let newer = "keybinds {\n  pane { bind \"{primary} {hyper} x\" { Quit; }; bind \"y\" { Quit; }; }\n}\n";
        let (binds, unknown) = preset_binds(newer, &user, true).unwrap();
        assert_eq!(
            (rows(&sheet_of(&binds).unwrap().0), unknown),
            (vec!["pane y = Quit".to_string()], 1)
        );
        // `shared_except` naming only an unknown mode binds in every known one, nothing counted; `shared_among` naming
        // none binds nowhere and counts nothing.
        let shared = "keybinds {\n  shared_except \"newmode\" { bind \"q\" { Quit; }; }\n\
                      shared_among { bind \"w\" { Quit; }; }\n}\n";
        let (binds, unknown) = preset_binds(shared, &BTreeMap::new(), true).unwrap();
        let r = rows(&sheet_of(&binds).unwrap().0);
        assert_eq!((r.len(), unknown), (MODES.len(), 0));
        assert!(r.iter().all(|r| r.ends_with(" q = Quit")));
        // In the user's config, no row and no note either.
        let (f, _g) = config(
            "among",
            "keybinds {\n  shared_among { bind \"w\" { Quit; }; }\n}\n",
        );
        let (sheet, notes) = read_with("0.46.0", Some(&f)).0.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!rows(&sheet).iter().any(|r| r.ends_with(" w = Quit")));
    }

    #[test]
    fn placeholders_put_in_once() {
        let values = leaders(&[("primary", "{secondary}"), ("secondary", "Alt")]);
        assert_eq!(
            substitute("{primary} x", &values).as_deref(),
            Some("{secondary} x")
        );
        assert_eq!(
            substitute("{primary}  {secondary} x", &values).as_deref(),
            Some("Alt Alt x")
        );
        assert_eq!(substitute("{unlock} x", &values), None);
        // So a leader holding a placeholder makes the preset's keys invalid: the default preset, as in zellij.
        let (f, _g) = config("ph", "keybinds primary=\"{secondary}\" {\n}\n");
        let (out, _) = read_with("0.46.0", Some(&f));
        let (sheet, notes) = out.unwrap();
        assert!(
            notes[0].contains("invalid key \"{secondary} g\""),
            "{notes:?}"
        );
        assert!(has(&rows(&sheet), "normal ctrl+p = SwitchToMode \"Pane\""));
    }

    #[test]
    fn leader_and_default_rules() {
        // Modifier sets: `Ctrl Ctrl` is `Ctrl` (both placeholders used, else only one is checked).
        let both = "keybinds {\n  pane { bind \"{primary} x\" \"{secondary} y\" { Quit; }; }\n}\n";
        let e = preset_binds(
            both,
            &leaders(&[("primary", "Ctrl Ctrl"), ("secondary", "ctrl")]),
            true,
        );
        assert_eq!(
            format!("{:#}", e.unwrap_err()),
            "primary and secondary are both \"Ctrl Ctrl\""
        );
        assert!(preset_binds(
            both,
            &leaders(&[("primary", "Ctrl"), ("secondary", "Ctrl Alt")]),
            true
        )
        .is_ok());
        // The last `clear-defaults` counts.
        assert!(merged(
            PRESET,
            "keybinds clear-defaults=false clear-defaults=true {\n}\n",
            true
        )
        .unwrap()
        .is_empty());
        // A bind with no actions hides the default and lists nothing, with no note.
        assert!(
            !merged(PRESET, "keybinds {\n  pane { bind \"x\" { }; }\n}\n", true)
                .unwrap()
                .iter()
                .any(|r| r.starts_with("pane x "))
        );
        let (f, _g) = config(
            "noaction",
            "keybinds {\n  normal { bind \"Ctrl p\" { }; }\n}\n",
        );
        let (sheet, notes) = read_with("0.46.0", Some(&f)).0.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert!(!rows(&sheet).iter().any(|r| r.starts_with("normal ctrl+p ")));
    }

    #[test]
    fn the_synthetic_config() {
        let f = PathBuf::from(format!(
            "{}/tests/fixtures/zellij/config.kdl",
            env!("CARGO_MANIFEST_DIR")
        ));
        let (out, calls) = read_with("0.46.0", Some(&f));
        assert_eq!(calls, ["--version", "setup --dump-keybinds=default"]);
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
            (
                "keybinds primary=1 {\n}\n",
                "line 1: primary must be a string, not 1",
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
        assert_eq!(calls, ["--version", "setup --dump-keybinds=unlock-first"]);
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
            Src::user(user)
                .apply(&mut binds, &doc.nodes()[0], true)
                .unwrap();
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
        // `é` and `Alt ü`; the bind with no action isn't listed, nor counted.
        assert_eq!(skipped, 2);
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
        assert_eq!(left_out(0, 0), None);
        assert!(left_out(1, 0).is_some() && left_out(0, 1).is_some());
        assert_eq!(
            left_out(2, 0).as_deref(),
            Some("leaving out 2 binds (2 with a key with no X keysym)")
        );
        assert_eq!(
            left_out(0, 3).as_deref(),
            Some("leaving out 3 binds (3 of zellij's own that this keybind-find doesn't know)")
        );
        assert_eq!(
            left_out(1, 2).as_deref(),
            Some("leaving out 3 binds (1 with a key with no X keysym; 2 of zellij's own that this keybind-find doesn't know)")
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
        // A ZELLIJ_CONFIG_DIR without config.kdl: none, though ~/.config/zellij has one (no fall-through).
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(home.join(".config/zellij")).unwrap();
        std::fs::write(home.join(".config/zellij/config.kdl"), "").unwrap();
        assert_eq!(
            find(vec![("ZELLIJ_CONFIG_DIR", s(&empty)), ("HOME", s(&home))]),
            None
        );
        assert!(find(vec![("HOME", s(&home))]).is_some());
        std::fs::remove_dir_all(home.join(".config/zellij")).unwrap();
        // A plain file at ~/.config/zellij stops the search (zellij's `exists()`), though
        // $XDG_CONFIG_HOME/zellij/config.kdl exists. (AC #3 says "exists as a directory"; zellij doesn't check.)
        std::fs::write(home.join(".config/zellij"), "").unwrap();
        let xdg_x = vec![("HOME", s(&home)), ("XDG_CONFIG_HOME", s(&home.join("x")))];
        assert_eq!(find(xdg_x.clone()), None);
        std::fs::remove_file(home.join(".config/zellij")).unwrap();
        assert!(find(xdg_x).is_some());
        // No HOME, no XDG: only /etc/zellij.
        assert_eq!(find(vec![]), none);
    }

    #[test]
    fn the_command() {
        // AC #2: each run is given 2 s.
        assert_eq!(TIMEOUT, Duration::from_secs(2));
        let cmd = command(&["setup", "--dump-keybinds=default"]);
        assert_eq!(cmd.get_program(), "zellij");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["setup", "--dump-keybinds=default"]);
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
                "case \"$*\" in --version) echo 'zellij 0.46.0';; 'setup --dump-keybinds=default') cat '{preset}';; *) exit 9;; esac"
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
            e.starts_with("running zellij setup --dump-keybinds=default: ") && e.contains("nope"),
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
            // 2 s: zellij's runs start no session, and a picker can't wait long.
            took >= Duration::from_millis(1900) && took < Duration::from_secs(4),
            "{took:?}"
        );
    }
}
