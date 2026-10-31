//! helix: its keys in every mode, from the default keymap pinned per helix version (`data/helix/<version>.toml`, from
//! `keymap/default.rs` at that tag) with the user's `config.toml` `[keys.<mode>]` tables merged as helix merges them.
//! Shown when helix (`hx`; `helix` on Arch) is the program in the focused terminal.

use super::nvim::single;
use super::pinned;
use crate::context::run_for;
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::{anyhow, bail, ensure, Context as _};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// One entry per helix version whose defaults we know, oldest first.
const REGISTRY: &[(&str, &str)] = &[
    (
        "25.01.1",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/helix/25.01.1.toml"
        )),
    ),
    (
        "25.07.1",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/helix/25.07.1.toml"
        )),
    ),
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// helix's modes, with the category of their top-level keys.
const MODES: [(&str, &str); 3] = [
    ("normal", "Normal mode"),
    ("select", "Select mode"),
    ("insert", "Insert mode"),
];

/// The Flatpak's config directory, under `$HOME`.
const FLATPAK_CONFIG: &str = ".var/app/com.helix_editor.Helix/config/helix/config.toml";

#[derive(Deserialize)]
struct Pinned {
    helix_version: String,
    source: String,
    bind: Vec<PinnedBind>,
    node: Vec<PinnedNode>,
    /// Every static command, with its doc.
    commands: BTreeMap<String, String>,
    typable: Vec<Typable>,
}

#[derive(Deserialize)]
struct PinnedBind {
    mode: String,
    keys: String,
    command: String,
}

/// A sub-mode: the keys that lead to it and its infobox label.
#[derive(Deserialize)]
struct PinnedNode {
    mode: String,
    keys: String,
    label: String,
}

#[derive(Deserialize)]
struct Typable {
    name: String,
    aliases: Vec<String>,
    doc: String,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(REGISTRY, |_| {}, &env)?;
    if !notes.is_empty() {
        eprintln!("keybind-find: helix: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `sheet` over `registry`, with `setup` applied to `hx --version` and `helix --version` (a test seam: `PATH`) and
/// `config.toml` found through `env`. Also the notes for stderr.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let installed = ["hx", "helix"].iter().find_map(|program| {
        let mut cmd = Command::new(program);
        cmd.arg("--version")
            .env("LC_ALL", "C")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
        setup(&mut cmd);
        run_for(cmd, VERSION_TIMEOUT)
            .ok()
            .and_then(|t| version_of(&t))
    });
    let (v, text, note) = pinned::select("helix", registry, installed.as_deref())
        .context("no pinned helix keymap")?;
    let pinned: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/helix/{v}.toml"))?;
    ensure!(
        pinned.helix_version == v && !pinned.source.is_empty(),
        "data/helix/{v}.toml names helix {} from {:?}",
        pinned.helix_version,
        pinned.source
    );
    let mut notes: Vec<String> = note.into_iter().collect();
    let config = config_file(env).and_then(|p| match std::fs::read_to_string(&p) {
        Ok(text) => Some((p, text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            notes.push(format!("{}: {e}; using helix's defaults", p.display()));
            None
        }
    });
    let (sheet, left_out, bad) = parse(&pinned, config.as_ref().map(|(_, t)| t.as_str()))?;
    if let (Some(reason), Some((path, _))) = (bad, &config) {
        // helix prints "Bad config: …" and starts with its defaults.
        notes.push(format!(
            "bad config {}: {reason}; using helix's defaults",
            path.display()
        ));
    }
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) with no X keysym (null, media keys, lone modifiers and the like)"
        ));
    }
    Ok((sheet, notes))
}

/// The version in `helix 25.07.1 (a05c151b)`; `None` for another program's line (`hx` is also a hex viewer's name).
fn version_of(text: &str) -> Option<String> {
    let mut words = text.lines().next()?.split_whitespace();
    (words.next()? == "helix").then_some(())?;
    words
        .next()
        .filter(|w| w.contains('.') && pinned::parse_version(w).is_some())
        .map(str::to_string)
}

/// The user's `config.toml`: `$XDG_CONFIG_HOME/helix` (`~/.config/helix` when unset, empty or relative), else the
/// Flatpak's when the host file is absent.
fn config_file(env: Env) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = set("HOME");
    let host = set("XDG_CONFIG_HOME")
        .filter(|d| d.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")))?
        .join("helix/config.toml");
    if host.exists() {
        return Some(host);
    }
    Some(home?.join(FLATPAK_CONFIG)).filter(|p| p.exists())
}

/// A key as helix holds it (`KeyEvent`): `S-` on a lower-case letter is folded into the capital, so `S-r` is `R`.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Key {
    ctrl: bool,
    alt: bool,
    shift: bool,
    sup: bool,
    code: Code,
}

impl Key {
    fn of(code: Code) -> Key {
        Key {
            ctrl: false,
            alt: false,
            shift: false,
            sup: false,
            code,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Code {
    Char(char),
    /// One of `NAMES`.
    Named(&'static str),
    F(u8),
}

/// helix's key names that aren't a character (`helix-view/src/input.rs` `keys`), with their X keysym; `None` for
/// keys keybind-find can't send.
const NAMES: &[(&str, Option<&str>)] = &[
    ("backspace", Some("BackSpace")),
    ("ret", Some("Return")),
    ("left", Some("Left")),
    ("right", Some("Right")),
    ("up", Some("Up")),
    ("down", Some("Down")),
    ("home", Some("Home")),
    ("end", Some("End")),
    ("pageup", Some("Prior")),
    ("pagedown", Some("Next")),
    ("tab", Some("Tab")),
    ("del", Some("Delete")),
    ("ins", Some("Insert")),
    ("esc", Some("Escape")),
    ("null", None),
    ("capslock", None),
    ("scrolllock", None),
    ("numlock", None),
    ("printscreen", None),
    ("pause", None),
    ("menu", None),
    ("keypadbegin", None),
    ("play", None),
    ("pausemedia", None),
    ("playpause", None),
    ("reverse", None),
    ("stop", None),
    ("fastforward", None),
    ("rewind", None),
    ("tracknext", None),
    ("trackprevious", None),
    ("record", None),
    ("lowervolume", None),
    ("raisevolume", None),
    ("mutevolume", None),
    ("leftshift", None),
    ("leftcontrol", None),
    ("leftalt", None),
    ("leftsuper", None),
    ("lefthyper", None),
    ("leftmeta", None),
    ("rightshift", None),
    ("rightcontrol", None),
    ("rightalt", None),
    ("rightsuper", None),
    ("righthyper", None),
    ("rightmeta", None),
    ("isolevel3shift", None),
    ("isolevel5shift", None),
];

/// A key as `KeyEvent::from_str` reads it, with its errors.
fn parse_key(s: &str) -> anyhow::Result<Key> {
    let mut tokens: Vec<&str> = s.split('-').collect();
    let last = tokens.pop().unwrap_or_default();
    let mut chars = last.chars();
    let one = chars.next().filter(|_| chars.next().is_none());
    let mut code = match last {
        "space" => Code::Char(' '),
        "minus" => Code::Char('-'),
        "lt" => Code::Char('<'),
        "gt" => Code::Char('>'),
        _ if let Some((name, _)) = NAMES.iter().find(|(n, _)| *n == last) => Code::Named(name),
        _ if let Some(c) = one => Code::Char(c),
        f if f.len() > 1 && f.starts_with('F') => {
            let n: u8 = f[1..]
                .parse()
                .map_err(|_| anyhow!("Invalid key code '{f}'"))?;
            ensure!((1..25).contains(&n), "Invalid function key '{n}'");
            Code::F(n)
        }
        "" if s.ends_with('-') => {
            ensure!(
                s == "-",
                "Key '-' cannot be used with modifiers, use 'minus' instead"
            );
            return Ok(Key::of(Code::Char('-')));
        }
        other => bail!("Invalid key code '{other}'"),
    };
    let mut key = Key::of(code);
    for token in tokens {
        let flag = match token {
            "S" => &mut key.shift,
            "A" => &mut key.alt,
            "C" => &mut key.ctrl,
            "Meta" | "Cmd" | "Win" => &mut key.sup,
            _ => bail!("Invalid key modifier '{token}-'"),
        };
        ensure!(!*flag, "Repeated key modifier '{token}-'");
        *flag = true;
    }
    if let Code::Char(c) = code {
        if c.is_ascii_lowercase() && key.shift {
            code = Code::Char(c.to_ascii_uppercase());
            key.shift = false;
        }
    }
    key.code = code;
    Ok(key)
}

/// A key in key notation; `None` for one with no X keysym.
fn notation(key: &Key) -> Option<String> {
    let mut mods = Vec::new();
    for (on, m) in [
        (key.ctrl, "ctrl"),
        (key.alt, "alt"),
        (key.shift, "shift"),
        (key.sup, "super"),
    ] {
        if on {
            mods.push(m);
        }
    }
    let keysym = match key.code {
        Code::Char(c) if c.is_ascii_uppercase() => {
            mods.push("shift");
            single(c.to_ascii_lowercase())?.to_string()
        }
        Code::Char(c) => single(c)?.to_string(),
        Code::Named(n) => NAMES.iter().find(|(name, _)| *name == n)?.1?.to_string(),
        Code::F(n) => format!("F{n}"),
    };
    Some(chord(&mods, &keysym))
}

/// A keymap trie (`KeyTrie`): commands (one, or a list run in turn) or a sub-mode with its label and keys in order.
#[derive(Clone, Debug)]
enum Trie {
    Commands(Vec<String>),
    Node(Node),
}

#[derive(Clone, Debug, Default)]
struct Node {
    /// `""` for a sub-mode the user's config made: helix's infobox then has no title.
    label: String,
    keys: Vec<(Key, Trie)>,
}

impl Node {
    fn get_mut(&mut self, key: &Key) -> Option<&mut Trie> {
        self.keys.iter_mut().find(|(k, _)| k == key).map(|(_, t)| t)
    }

    /// Sets `key` to `trie`, in place if it is bound already.
    fn set(&mut self, key: Key, trie: Trie) {
        match self.get_mut(&key) {
            Some(t) => *t = trie,
            None => self.keys.push((key, trie)),
        }
    }

    /// `KeyTrieNode::merge`: a sub-mode onto a sub-mode merges key by key; anything else replaces the key.
    fn merge(&mut self, other: Node) {
        for (key, trie) in other.keys {
            match (self.get_mut(&key), trie) {
                (Some(Trie::Node(mine)), Trie::Node(theirs)) => mine.merge(theirs),
                (_, trie) => self.set(key, trie),
            }
        }
    }

    /// The sub-mode at `path`, made (with `label`) where missing.
    fn node_at(&mut self, path: &[Key]) -> Option<&mut Node> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };
        if self.get_mut(first).is_none() {
            self.keys.push((*first, Trie::Node(Node::default())));
        }
        match self.get_mut(first)? {
            Trie::Node(n) => n.node_at(rest),
            Trie::Commands(_) => None,
        }
    }
}

fn parse_keys(keys: &str) -> anyhow::Result<Vec<Key>> {
    keys.split(' ').map(parse_key).collect()
}

/// The default keymap of each mode, from the pinned rows.
fn defaults(pinned: &Pinned) -> anyhow::Result<Vec<Node>> {
    let mut modes: Vec<Node> = MODES
        .iter()
        .map(|(_, label)| Node {
            label: label.to_string(),
            keys: Vec::new(),
        })
        .collect();
    let root = |mode: &str| {
        MODES
            .iter()
            .position(|(m, _)| *m == mode)
            .ok_or_else(|| anyhow!("unknown mode {mode:?}"))
    };
    for n in &pinned.node {
        let i = root(&n.mode)?;
        let path = parse_keys(&n.keys)?;
        let node = modes[i]
            .node_at(&path)
            .with_context(|| format!("{} {}: under a command", n.mode, n.keys))?;
        node.label = n.label.clone();
    }
    for b in &pinned.bind {
        let i = root(&b.mode)?;
        let path = parse_keys(&b.keys)?;
        let (last, parent) = path
            .split_last()
            .with_context(|| format!("{} {:?}: no key", b.mode, b.keys))?;
        modes[i]
            .node_at(parent)
            .with_context(|| format!("{} {}: under a command", b.mode, b.keys))?
            .set(*last, Trie::Commands(vec![b.command.clone()]));
    }
    Ok(modes)
}

/// What a command string runs, checked as `MappableCommand::from_str` checks it: a static command, `:typable args`
/// or `@macro` keys.
fn check_command(pinned: &Pinned, command: &str) -> anyhow::Result<()> {
    if let Some(rest) = command.strip_prefix(':') {
        let (name, _) = typable_parts(rest);
        ensure!(!name.is_empty(), "Expected typable command name");
        ensure!(
            typable(pinned, name).is_some(),
            "No TypableCommand named '{command}'"
        );
    } else if let Some(keys) = command.strip_prefix('@') {
        macro_keys(keys)?;
    } else {
        ensure!(
            pinned.commands.contains_key(command),
            "No command named '{command}'"
        );
    }
    Ok(())
}

/// A typable command's name and arguments, split as helix 25.07.1's `command_line::split` splits them: at the first
/// space or tab, so `:  w` has no name.
fn typable_parts(text: &str) -> (&str, &str) {
    let (name, args) = text.split_once([' ', '\t']).unwrap_or((text, ""));
    (name, args.trim())
}

fn typable<'a>(pinned: &'a Pinned, name: &str) -> Option<&'a Typable> {
    pinned
        .typable
        .iter()
        .find(|t| t.name == name || t.aliases.iter().any(|a| a == name))
}

/// A macro's keys as `parse_macro` reads them: characters, or names in `<…>`.
fn macro_keys(text: &str) -> anyhow::Result<Vec<Key>> {
    let mut keys = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        match c {
            '>' => bail!("Unmatched '>'"),
            '<' => {
                let end = rest.find('>').context("'>' expected")?;
                keys.push(parse_key(&rest[1..end])?);
                rest = &rest[end + 1..];
            }
            _ => {
                let one = &rest[..c.len_utf8()];
                keys.push(parse_key(if one == "-" { "minus" } else { one })?);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    Ok(keys)
}

/// A value under `[keys.<mode>]` as helix's `KeyTrie` deserializer takes it.
fn user_trie(pinned: &Pinned, value: &toml::Value) -> anyhow::Result<Trie> {
    Ok(match value {
        toml::Value::String(command) => {
            check_command(pinned, command)?;
            Trie::Commands(vec![command.clone()])
        }
        toml::Value::Array(list) => {
            let mut commands = Vec::new();
            for c in list {
                let command = c
                    .as_str()
                    .context("a list of commands holds strings only")?;
                check_command(pinned, command)?;
                ensure!(
                    !command.starts_with('@'),
                    "macro keybindings may not be used in command sequences"
                );
                commands.push(command.to_string());
            }
            Trie::Commands(commands)
        }
        toml::Value::Table(table) => {
            let mut node = Node::default();
            for (key, value) in table {
                node.set(parse_key(key)?, user_trie(pinned, value)?);
            }
            Trie::Node(node)
        }
        other => bail!(
            "invalid type: {}, expected a command, list of commands, or sub-keymap",
            other.type_str()
        ),
    })
}

/// The user's `[keys.<mode>]` tables, in `MODES` order, checked as helix's `ConfigRaw` checks the file.
fn user_keys(pinned: &Pinned, config: &str) -> anyhow::Result<Vec<Option<Node>>> {
    let table: toml::Table = toml::from_str(config).map_err(|e| anyhow!("{}", e.message()))?;
    let mut modes: Vec<Option<Node>> = vec![None; MODES.len()];
    for (field, value) in &table {
        match (field.as_str(), value) {
            // ponytail: `[editor]`'s own options aren't checked (helix refuses an unknown one too); add when a
            // typo there fools someone.
            ("theme", toml::Value::String(_)) | ("editor", toml::Value::Table(_)) => {}
            ("editor", other) => bail!("invalid type: {}, expected a map", other.type_str()),
            ("theme", other) => bail!("invalid type: {}, expected a string", other.type_str()),
            ("keys", toml::Value::Table(keys)) => {
                for (mode, value) in keys {
                    let i = MODES
                        .iter()
                        .position(|(m, _)| m == mode)
                        .ok_or_else(|| anyhow!("Invalid mode '{mode}'"))?;
                    // helix itself stops on a mode that isn't a table (it unwraps a node).
                    let Trie::Node(node) = user_trie(pinned, value)? else {
                        bail!("keys.{mode} is not a table of keys");
                    };
                    modes[i] = Some(node);
                }
            }
            ("keys", other) => bail!("invalid type: {}, expected a map", other.type_str()),
            (other, _) => {
                bail!("unknown field `{other}`, expected one of `theme`, `keys`, `editor`")
            }
        }
    }
    Ok(modes)
}

/// The sheet from the pinned defaults with `config` (a `config.toml`) merged; also the keys left out and, for a
/// config helix would refuse, why (the defaults are then used alone).
fn parse(pinned: &Pinned, config: Option<&str>) -> anyhow::Result<(Sheet, usize, Option<String>)> {
    let mut modes = defaults(pinned)?;
    let bad = match config.map(|c| user_keys(pinned, c)).transpose() {
        Ok(user) => {
            for (mode, node) in modes.iter_mut().zip(user.into_iter().flatten()) {
                if let Some(node) = node {
                    mode.merge(node);
                }
            }
            None
        }
        // One stderr line: toml's messages run over several.
        Err(e) => Some(format!("{e:#}").lines().collect::<Vec<_>>().join(": ")),
    };
    let mut sheet = Sheet {
        title: format!("helix {}", pinned.helix_version),
        provider: "helix".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for ((mode, _), root) in MODES.iter().zip(&modes) {
        let mut path = Vec::new();
        walk(
            pinned,
            mode,
            root,
            &root.label,
            &mut path,
            &mut sheet,
            &mut left_out,
        );
    }
    Ok((sheet, left_out, bad))
}

/// Adds `node`'s binds under `path` (each key's notation, `None` for one with no keysym) to `sheet`, filed under the
/// innermost labelled sub-mode (`category`); a bind with a key that has no keysym is counted in `left_out`.
fn walk(
    pinned: &Pinned,
    mode: &str,
    node: &Node,
    category: &str,
    path: &mut Vec<Option<String>>,
    sheet: &mut Sheet,
    left_out: &mut usize,
) {
    for (key, trie) in &node.keys {
        path.push(notation(key));
        match trie {
            Trie::Node(child) => {
                let category = if child.label.is_empty() {
                    category
                } else {
                    &child.label
                };
                walk(pinned, mode, child, category, path, sheet, left_out);
            }
            // `no_op` disables a key.
            // `no_op` disables a key; an empty list does nothing.
            Trie::Commands(c) if c.is_empty() || (c.len() == 1 && c[0] == "no_op") => {}
            Trie::Commands(_) if path.iter().any(Option::is_none) => *left_out += 1,
            Trie::Commands(commands) => {
                let mut b = Bind {
                    key: path.iter().flatten().cloned().collect::<Vec<_>>().join(" "),
                    desc: commands
                        .iter()
                        .map(|c| describe(pinned, c))
                        .collect::<Vec<_>>()
                        .join(", "),
                    action: commands.join(", "),
                    when: Default::default(),
                };
                if let (Ok(scope), Ok(value)) = ("mode".parse::<Name>(), mode.parse::<Name>()) {
                    b.when.insert(scope, Values::from(value));
                }
                sheet.binds.entry(category.to_string()).or_default().push(b);
            }
        }
        path.pop();
    }
}

/// What a command does: a static command's doc; a typable command's doc, or `:name args` with arguments; a macro's
/// text.
fn describe(pinned: &Pinned, command: &str) -> String {
    if let Some(rest) = command.strip_prefix(':') {
        let (name, args) = typable_parts(rest);
        return match typable(pinned, name) {
            Some(t) if args.is_empty() => t.doc.clone(),
            Some(t) => format!(":{} {args}", t.name),
            None => command.to_string(),
        };
    }
    if command.starts_with('@') {
        return command.to_string();
    }
    pinned
        .commands
        .get(command)
        .cloned()
        .unwrap_or_else(|| command.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/helix")
                .join(name),
        )
        .unwrap()
    }

    fn pinned(version: &str) -> Pinned {
        let text = REGISTRY.iter().find(|(v, _)| *v == version).unwrap().1;
        toml::from_str(text).unwrap()
    }

    /// (category, key, desc, action) of the binds in `mode`.
    fn binds<'a>(s: &'a Sheet, mode: &str) -> Vec<(&'a str, &'a str, &'a str, &'a str)> {
        s.binds
            .iter()
            .flat_map(|(c, bs)| bs.iter().map(move |b| (c.as_str(), b)))
            .filter(|(_, b)| {
                b.when
                    .iter()
                    .any(|(k, v)| k.as_str() == "mode" && v.iter().any(|n| n.as_str() == mode))
            })
            .map(|(c, b)| (c, b.key.as_str(), b.desc.as_str(), b.action.as_str()))
            .collect()
    }

    /// The bind on `key` in `mode`: (category, desc, action).
    fn get<'a>(s: &'a Sheet, mode: &str, key: &str) -> Option<(&'a str, &'a str, &'a str)> {
        binds(s, mode)
            .into_iter()
            .find(|(_, k, _, _)| *k == key)
            .map(|(c, _, d, a)| (c, d, a))
    }

    #[test]
    fn pinned_files() {
        for (v, commit, binds_total, typables) in [
            (
                "25.01.1",
                "e7ac2fcdecfdbf43a4f772e7f7c163b43b3d0b9b",
                665,
                88,
            ),
            (
                "25.07.1",
                "a05c151bb6e8e9c65ec390b0ae2afe7a5efd619b",
                671,
                89,
            ),
        ] {
            let text = REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1;
            assert!(
                text.contains(&format!("tag {v} (commit {commit})")) && text.contains("MPL-2.0"),
                "{v}"
            );
            let p = pinned(v);
            assert_eq!(p.helix_version, v);
            assert_eq!(p.bind.len(), binds_total, "{v}");
            // Every `TypableCommand` in typed.rs.
            assert_eq!(p.typable.len(), typables, "{v}");
            for b in &p.bind {
                assert!(p.commands.contains_key(&b.command), "{v} {}", b.command);
            }
            let (s, left_out, bad) = parse(&p, None).unwrap();
            assert_eq!((left_out, bad), (0, None), "{v}");
            assert_eq!(s.binds.values().flatten().count(), binds_total, "{v}");
            for b in s.binds.values().flatten() {
                assert!(
                    b.key.parse::<keybind_find_sheet::Key>().is_ok(),
                    "{v} {}",
                    b.key
                );
            }
            assert_eq!(s.title, format!("helix {v}"));
        }
        // 25.07.1 added the file explorer under space, and `g |`.
        let (old, _, _) = parse(&pinned("25.01.1"), None).unwrap();
        let (new, _, _) = parse(&pinned("25.07.1"), None).unwrap();
        assert_eq!(get(&old, "normal", "space e"), None);
        assert_eq!(
            get(&new, "normal", "space e"),
            Some((
                "Space",
                "Open file explorer in workspace root",
                "file_explorer"
            ))
        );
        assert_eq!(get(&old, "normal", "g bar"), None);
        assert_eq!(
            get(&new, "normal", "g bar"),
            Some(("Goto", "Goto column", "goto_column"))
        );
    }

    #[test]
    fn the_defaults_by_mode() {
        let (s, _, _) = parse(&pinned("25.07.1"), None).unwrap();
        for b in s.binds.values().flatten() {
            assert_eq!(b.when.len(), 1, "{}", b.key);
        }
        assert_eq!(binds(&s, "normal").len(), 322);
        assert_eq!(binds(&s, "select").len(), 322);
        assert_eq!(binds(&s, "insert").len(), 27);
        assert_eq!(
            get(&s, "normal", "g d"),
            Some(("Goto", "Goto definition", "goto_definition"))
        );
        // Two keys for one command; nested sub-modes.
        assert_eq!(get(&s, "normal", "h").map(|b| b.2), Some("move_char_left"));
        assert_eq!(
            get(&s, "normal", "Left").map(|b| b.2),
            Some("move_char_left")
        );
        assert_eq!(
            get(&s, "normal", "space shift+g shift+e").map(|b| (b.0, b.2)),
            Some(("Debug (experimental)", "dap_disable_exceptions"))
        );
        assert_eq!(
            get(&s, "normal", "ctrl+w n v").map(|b| b.0),
            Some("New split scratch buffer")
        );
        // Select: normal's keys, with select's own over them, its top level filed as select's.
        assert_eq!(
            get(&s, "select", "d"),
            Some(("Select mode", "Delete selection", "delete_selection"))
        );
        assert_eq!(
            get(&s, "select", "w").map(|b| b.2),
            Some("extend_next_word_start")
        );
        assert_eq!(
            get(&s, "select", "g g").map(|b| b.2),
            Some("extend_to_file_start")
        );
        assert_eq!(
            get(&s, "select", "g d").map(|b| b.2),
            Some("goto_definition")
        );
        assert_eq!(
            get(&s, "insert", "ctrl+x"),
            Some(("Insert mode", "Invoke completion popup", "completion"))
        );
    }

    #[test]
    fn the_users_config() {
        let (s, left_out, bad) = parse(&pinned("25.07.1"), Some(&fixture("config.toml"))).unwrap();
        assert_eq!(bad, None);
        // `null` and `C-leftshift`.
        assert_eq!(left_out, 2);
        let n = |k| get(&s, "normal", k);
        assert_eq!(
            n("ctrl+s"),
            Some((
                "Normal mode",
                "Write changes to disk. Accepts an optional path (:write some/path.txt)",
                ":w"
            ))
        );
        assert_eq!(n("shift+r").map(|b| b.2), Some("redo"));
        assert_eq!(
            n("shift+k"),
            Some((
                "Normal mode",
                "Enter selection extend mode, Goto line end",
                "select_mode, goto_line_end"
            ))
        );
        assert_eq!(n("x"), None);
        // A new sub-mode has no label: its keys go with its parent's.
        assert_eq!(
            n("plus a"),
            Some(("Normal mode", ":open file", ":open file"))
        );
        assert_eq!(n("plus m"), Some(("Normal mode", "@miw", "@miw")));
        // Merged sub-modes keep their other keys.
        assert_eq!(
            n("g z"),
            Some(("Goto", "Goto definition", "goto_definition"))
        );
        assert_eq!(n("g d").map(|b| b.2), Some("goto_definition"));
        assert_eq!(
            n("space f").map(|b| b.2),
            Some("file_picker_in_current_directory")
        );
        assert_eq!(n("space b").map(|b| b.2), Some("buffer_picker"));
        // Select's Goto replaced by a command; normal's Goto untouched.
        assert_eq!(
            get(&s, "select", "g"),
            Some(("Select mode", "Goto definition", "goto_definition"))
        );
        assert!(binds(&s, "select").iter().all(|b| !b.1.starts_with("g ")));
        assert_eq!(
            get(&s, "insert", "j k"),
            Some(("Insert mode", "Enter normal mode", "normal_mode"))
        );
        // The rest of the defaults stay.
        assert_eq!(n("w").map(|b| b.2), Some("move_next_word_start"));
    }

    #[test]
    fn a_config_helix_refuses_gives_the_defaults() {
        let p = pinned("25.07.1");
        let (defaults, _, _) = parse(&p, None).unwrap();
        for (config, reason) in [
            (fixture("bad-toml.toml"), "invalid table header"),
            (
                fixture("unknown-command.toml"),
                "No command named 'frobnicate'",
            ),
            (fixture("unknown-key.toml"), "Invalid key code 'foo'"),
            (
                "[keys.normal]\nx = \":frob\"\n".into(),
                "No TypableCommand named ':frob'",
            ),
            ("[keys.normal]\nx = \"@<C-a\"\n".into(), "'>' expected"),
            (
                "[keys.normal]\nx = [\"undo\", \"@u\"]\n".into(),
                "macro keybindings",
            ),
            ("[keys.normal]\nx = 1\n".into(), "invalid type: integer"),
            (
                "[keys.normal]\nC-S-C-x = \"undo\"\n".into(),
                "Repeated key modifier 'C-'",
            ),
            ("[keys.normal]\n\"A--\" = \"undo\"\n".into(), "use 'minus'"),
            (
                "[keys.visual]\nx = \"undo\"\n".into(),
                "Invalid mode 'visual'",
            ),
            ("[keys]\nnormal = \"undo\"\n".into(), "not a table"),
            ("color = 1\n".into(), "unknown field `color`"),
            ("theme = 1\n".into(), "expected a string"),
            ("editor = 5\n".into(), "expected a map"),
            (
                "[keys.normal]\nx = \":  w\"\n".into(),
                "Expected typable command name",
            ),
        ] {
            let (s, _, bad) = parse(&p, Some(&config)).unwrap();
            let bad = bad.unwrap_or_default();
            assert!(bad.contains(reason), "{config:?}: {bad}");
            assert!(!bad.contains('\n'), "{bad}");
            assert_eq!(s, defaults, "{config:?}");
        }
        // Valid edge cases: an empty file, `:w` with spaces, `-` alone, an F key, a sub-mode under a new key.
        let (s, _, bad) = parse(
            &p,
            Some("[keys.normal]\n\"-\" = \":w\"\nE = []\nC-o = \":o a  b\"\nF24 = \"undo\"\n\"A-minus\" = { \"S-tab\" = \"redo\" }\n"),
        )
        .unwrap();
        assert_eq!(bad, None);
        assert_eq!(
            get(&s, "normal", "minus"),
            Some((
                "Normal mode",
                "Write changes to disk. Accepts an optional path (:write some/path.txt)",
                ":w"
            ))
        );
        // An empty list does nothing: not listed.
        assert_eq!(get(&s, "normal", "shift+e"), None);
        // An alias with arguments is described by the command's own name.
        assert_eq!(get(&s, "normal", "ctrl+o").map(|b| b.1), Some(":open a  b"));
        assert_eq!(get(&s, "normal", "F24").map(|b| b.2), Some("undo"));
        assert_eq!(
            get(&s, "normal", "alt+minus shift+Tab").map(|b| b.2),
            Some("redo")
        );
        assert_eq!(parse(&p, Some("")).unwrap().0, defaults);
    }

    #[test]
    fn keys() {
        let n = |k: &str| notation(&parse_key(k).unwrap());
        for (helix, ours) in [
            ("C-s", Some("ctrl+s")),
            ("A-.", Some("alt+period")),
            ("C-S-esc", Some("ctrl+shift+Escape")),
            ("G", Some("shift+g")),
            ("S-g", Some("shift+g")),
            ("A-S-down", Some("alt+shift+Down")),
            ("Cmd-s", Some("super+s")),
            ("Meta-s", Some("super+s")),
            ("Win-s", Some("super+s")),
            ("C-G", Some("ctrl+shift+g")),
            ("Cmd-G", Some("super+shift+g")),
            ("esc", Some("Escape")),
            ("home", Some("Home")),
            ("end", Some("End")),
            ("ret", Some("Return")),
            ("backspace", Some("BackSpace")),
            ("del", Some("Delete")),
            ("ins", Some("Insert")),
            ("pageup", Some("Prior")),
            ("pagedown", Some("Next")),
            ("space", Some("space")),
            ("tab", Some("Tab")),
            ("minus", Some("minus")),
            ("lt", Some("less")),
            ("gt", Some("greater")),
            ("F12", Some("F12")),
            ("\"", Some("quotedbl")),
            ("|", Some("bar")),
            ("null", None),
            ("leftshift", None),
            ("capslock", None),
            ("playpause", None),
            ("é", None),
        ] {
            assert_eq!(n(helix).as_deref(), ours, "{helix}");
        }
        for bad in ["F25", "F0", "foo", "X-a", "", "C-"] {
            assert!(parse_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_version() {
        assert_eq!(
            version_of("helix 25.07.1 (a05c151b)\n").as_deref(),
            Some("25.07.1")
        );
        for junk in ["", "helix", "helix x.y (z)", "hx 0.6.0"] {
            assert_eq!(version_of(junk), None, "{junk}");
        }
        let none = |_: &str| None;
        let run = |bin: &Path| {
            let path = bin.display().to_string();
            sheet_with(
                REGISTRY,
                move |c: &mut Command| {
                    c.env("PATH", &path);
                },
                &none,
            )
            .unwrap()
        };
        // No binary: the newest file and the note.
        let (s, notes) = run(Path::new("/nonexistent"));
        assert_eq!(s.title, "helix 25.07.1");
        assert_eq!(
            notes,
            ["helix's version is unknown: using the defaults of 25.07.1"]
        );
        // `hx`, or Arch's `helix`; an unknown version gets the nearest with a note.
        // A hex viewer named `hx` is passed over for Arch's `helix`.
        let bin = fake("hx", "hx 0.6.0");
        let helix = fake("helix", "helix 25.01.1 (e7ac2fcd)");
        let both = std::env::join_paths([&bin, &helix]).unwrap();
        let path = both.to_string_lossy().into_owned();
        let (s, notes) = sheet_with(
            REGISTRY,
            move |c: &mut Command| {
                c.env("PATH", &path);
            },
            &none,
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        let _ = std::fs::remove_dir_all(&helix);
        assert_eq!((s.title.as_str(), notes.len()), ("helix 25.01.1", 0));
        for (name, out, title, note) in [
            ("hx", "helix 25.01.1 (e7ac2fcd)", "helix 25.01.1", None),
            ("helix", "helix 25.07.1 (a05c151b)", "helix 25.07.1", None),
            (
                "hx",
                "helix 25.10.0 (0123abcd)",
                "helix 25.07.1",
                Some("helix 25.10.0: using the defaults of 25.07.1; add data/helix/25.10.0.toml"),
            ),
        ] {
            let bin = fake(name, out);
            let (s, notes) = run(&bin);
            let _ = std::fs::remove_dir_all(&bin);
            assert_eq!(s.title, title, "{name} {out}");
            assert_eq!(notes.first().map(String::as_str), note, "{name} {out}");
        }
    }

    fn fake(name: &str, version: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!(
            "kbf-helix-bin-{name}-{}-{}",
            version.len(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process (ETXTBSY: see `xfce.rs`'s `fake`).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\necho \"%s\"\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join(name))
            .arg(version)
            .status()
            .unwrap();
        assert!(ok.success());
        bin
    }

    #[test]
    fn the_config_file_and_its_notes() {
        let dir = std::env::temp_dir().join(format!("kbf-helix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (home, xdg) = (dir.join("home"), dir.join("xdg"));
        let flatpak = home.join(FLATPAK_CONFIG);
        let host = home.join(".config/helix/config.toml");
        let in_xdg = xdg.join("helix/config.toml");
        for f in [&flatpak, &host, &in_xdg] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        }
        let env_of = |xdg_home: Option<&Path>| {
            let h = home.display().to_string();
            let x = xdg_home.map(|p| p.display().to_string());
            move |k: &str| match k {
                "HOME" => Some(h.clone()),
                "XDG_CONFIG_HOME" => x.clone(),
                _ => None,
            }
        };
        // Nothing anywhere: none.
        assert_eq!(config_file(&env_of(None)), None);
        // Only the Flatpak's.
        std::fs::write(&flatpak, "").unwrap();
        assert_eq!(config_file(&env_of(None)), Some(flatpak.clone()));
        // The host file wins; `XDG_CONFIG_HOME` empty or relative is unset.
        std::fs::write(&host, "").unwrap();
        for x in [None, Some(Path::new("")), Some(Path::new("rel"))] {
            assert_eq!(config_file(&env_of(x)), Some(host.clone()), "{x:?}");
        }
        std::fs::write(&in_xdg, "").unwrap();
        assert_eq!(config_file(&env_of(Some(&xdg))), Some(in_xdg.clone()));
        // Through `sheet_with`: a bad file is one note naming it; the defaults stand.
        std::fs::write(&in_xdg, fixture("unknown-key.toml")).unwrap();
        let no_binary = |c: &mut Command| {
            c.env("PATH", "/nonexistent");
        };
        let (s, notes) = sheet_with(REGISTRY, no_binary, &env_of(Some(&xdg))).unwrap();
        assert_eq!(binds(&s, "normal").len(), 322);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[1].contains(&in_xdg.display().to_string())
                && notes[1].contains("Invalid key code 'foo'")
                && notes[1].contains("using helix's defaults"),
            "{notes:?}"
        );
        // A good one: merged, and the left-out count.
        std::fs::write(&in_xdg, fixture("config.toml")).unwrap();
        let (s, notes) = sheet_with(REGISTRY, no_binary, &env_of(Some(&xdg))).unwrap();
        assert_eq!(get(&s, "normal", "x"), None);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[1].starts_with("leaving out 2 key(s)"), "{notes:?}");
        // A file that can't be read (a directory): a note, the defaults.
        std::fs::remove_file(&in_xdg).unwrap();
        std::fs::create_dir(&in_xdg).unwrap();
        let (s, notes) = sheet_with(REGISTRY, no_binary, &env_of(Some(&xdg))).unwrap();
        assert_eq!(binds(&s, "normal").len(), 322);
        assert!(notes[1].contains("using helix's defaults"), "{notes:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC: the real `hx --version` (or `helix --version`) parses. Skipped (with a message) when neither is installed.
    #[test]
    fn real_helix() {
        let Some(program) = ["hx", "helix"].into_iter().find(|p| {
            Command::new(p)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .output()
                .is_ok_and(|o| o.status.success())
        }) else {
            eprintln!("skipping: neither hx nor helix is installed");
            return;
        };
        let mut cmd = Command::new(program);
        cmd.arg("--version").env("LC_ALL", "C");
        let out = run_for(cmd, VERSION_TIMEOUT).unwrap();
        assert!(version_of(&out).is_some(), "{out}");
    }
}
