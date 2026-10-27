//! herdr: the effective keymap = a versioned registry of herdr's defaults (`data/herdr/<version>.toml`, taken from
//! herdr's source) ∪ the user's `config.toml` `[keys]` overrides ∪ `[[keys.command]]` bindings (plugin commands
//! included, titled from `plugins.json`). herdr has no command or API that prints its keymap, so the rules below
//! follow herdr 0.9.3's `src/config/keybinds.rs` (`build_keybinds`, `reject_binding`, `parse_binding_string`).

use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;
use toml::{Table, Value};

/// One entry per herdr version whose defaults we know. Supporting a new herdr: add its file and a line here.
const REGISTRY: &[(&str, &str)] = &[(
    "0.9.3",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/herdr/0.9.3.toml"
    )),
)];

/// The only actions that take a `1..9` range, and that take nothing else.
const INDEXED: &[&str] = &["switch_tab", "switch_workspace", "focus_agent"];

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let installed = installed_version();
    let (_, registry, warning) =
        select(installed.as_deref()).context("no herdr defaults registry is built in")?;
    if let Some(w) = warning {
        eprintln!("keybind-find: {w}");
    }
    let dir = config_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
    .context("finding herdr's config: neither XDG_CONFIG_HOME nor HOME is set")?;
    // herdr reads config.toml from HERDR_CONFIG_PATH when it's set; plugins.json always lives in the config dir.
    let config_path = std::env::var_os("HERDR_CONFIG_PATH")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("config.toml"));
    let config = read_or_empty(config_path)?;
    // Plugin titles are only a fallback description; an unreadable plugins.json just means no titles.
    let plugins = read_or_empty(dir.join("plugins.json")).unwrap_or_default();
    parse(&config, registry, &plugins)
}

// ponytail: no timeout; `--version` doesn't touch the server. Add one if it ever hangs.
fn installed_version() -> Option<String> {
    let out = Command::new("herdr").arg("--version").output().ok()?;
    version_from(&String::from_utf8_lossy(&out.stdout))
}

/// The version in `herdr --version` output (`herdr 0.9.3`, `herdr 0.9.4-preview.123`), normalized to `0.9.4`.
fn version_from(output: &str) -> Option<String> {
    output.split_whitespace().find_map(|token| {
        let v = parse_version(token)?;
        Some(v.iter().map(u64::to_string).collect::<Vec<_>>().join("."))
    })
}

/// `0.9.3`, `v0.9.3`, `0.9.4-preview.1`, `0.9` (= 0.9.0) → `[0, 9, 3]`. Suffixes after `-`/`+` are ignored.
fn parse_version(v: &str) -> Option<Vec<u64>> {
    let core = v.trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts: Vec<u64> = core
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if parts.len() > 3 {
        return None;
    }
    parts.resize(3, 0);
    Some(parts)
}

fn select(installed: Option<&str>) -> Option<(&'static str, &'static str, Option<String>)> {
    select_from(REGISTRY, installed)
}

/// The registry entry for `installed`: an exact match silently; otherwise the newest version below it, or the oldest
/// entry if there is none (or herdr's version is unknown), with a warning naming both. `None` for an empty registry.
fn select_from(
    registry: &[(&'static str, &'static str)],
    installed: Option<&str>,
) -> Option<(&'static str, &'static str, Option<String>)> {
    let mut entries: Vec<(Vec<u64>, &'static str, &'static str)> = registry
        .iter()
        .filter_map(|&(v, toml)| Some((parse_version(v)?, v, toml)))
        .collect();
    entries.sort();
    let wanted = installed.and_then(parse_version);
    if let Some(w) = &wanted {
        if let Some(&(_, v, toml)) = entries.iter().find(|(p, _, _)| p == w) {
            return Some((v, toml, None));
        }
    }
    let below = wanted
        .as_ref()
        .and_then(|w| entries.iter().rev().find(|(p, _, _)| p < w));
    let &(_, v, toml) = below.or(entries.first())?;
    let warning = match installed {
        Some(i) => format!("herdr {i}: using defaults registry {v}; add data/herdr/{i}.toml"),
        None => format!("herdr version unknown: using defaults registry {v}"),
    };
    Some((v, toml, Some(warning)))
}

/// `$XDG_CONFIG_HOME/herdr` (absolute only, as the XDG spec says), else `$HOME/.config/herdr`.
fn config_dir(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = match xdg_config_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => dir,
        None => PathBuf::from(home.filter(|v| !v.is_empty())?).join(".config"),
    };
    Some(base.join("herdr"))
}

/// The file's text, or "" if it doesn't exist. Byte-order marks are dropped, as herdr does.
fn read_or_empty(path: PathBuf) -> anyhow::Result<String> {
    match std::fs::read(&path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes)
            .trim_start_matches('\u{feff}')
            .replace("\n\u{feff}", "\n")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn parse(config_toml: &str, registry_toml: &str, plugins_json: &str) -> anyhow::Result<Sheet> {
    let (sheet, warnings) = parse_reporting(config_toml, registry_toml, plugins_json)?;
    for w in warnings {
        eprintln!("keybind-find: herdr: {w}");
    }
    Ok(sheet)
}

/// One binding before translation: herdr key strings, what it does, what it's called.
struct Entry {
    category: &'static str,
    keys: Vec<String>,
    desc: String,
    action: String,
    user: bool,
}

/// The action names in the registry file, in its line order (herdr's `KeysConfig` field order, which decides which
/// of two clashing user bindings herdr keeps). `toml::Table` iterates alphabetically, so read the order from the text.
fn action_order(registry_toml: &str) -> Vec<String> {
    registry_toml
        .lines()
        .skip_while(|l| l.trim() != "[actions]")
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .filter_map(|l| l.split_once(" = ").map(|(name, _)| name.trim().to_string()))
        .filter(|name| !name.is_empty() && !name.starts_with('#'))
        .collect()
}

fn parse_reporting(
    config_toml: &str,
    registry_toml: &str,
    plugins_json: &str,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let registry: Table = registry_toml
        .parse()
        .context("parsing the herdr defaults registry")?;
    let config: Table = config_toml.parse().context("parsing herdr's config.toml")?;
    let actions = registry
        .get("actions")
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut warnings = Vec::new();

    let empty = Table::new();
    let mut keys = match config.get("keys") {
        None => &empty,
        Some(Value::Table(t)) => t,
        Some(_) => {
            warnings.push(
                "keys is not a table; herdr ignores this config and runs on its defaults".into(),
            );
            &empty
        }
    };
    // herdr refuses the whole config on a type error in [keys] and runs on its defaults; show what it runs.
    if let Some(field) = type_error(keys, &actions) {
        warnings.push(format!(
            "keys.{field} has the wrong type; herdr ignores this config and runs on its defaults"
        ));
        keys = &empty;
    }
    let user_value = |name: &str| -> Option<&Value> {
        match name {
            // `fullscreen` is herdr's alias for `zoom`.
            "zoom" => keys.get("zoom").or_else(|| keys.get("fullscreen")),
            _ => keys.get(name),
        }
    };

    let indexed = keys.get("indexed").and_then(Value::as_table);
    if indexed.is_some() {
        warnings.push("[keys.indexed] bindings are not listed".to_string());
    }
    // A non-empty [keys.indexed] target replaces herdr's own default for that action.
    let indexed_replaces = |action: &str| {
        let target = match action {
            "switch_tab" => "tabs",
            "switch_workspace" => "workspaces",
            "focus_agent" => "agents",
            _ => return false,
        };
        indexed
            .and_then(|t| t.get(target))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    };

    // The first prefix herdr accepts. ponytail: herdr also accepts the others (and `extra_prefixes`); only this one
    // is shown, since every binding is reachable through it.
    let registry_prefix = registry
        .get("prefix")
        .and_then(Value::as_str)
        .unwrap_or("ctrl+b");
    let prefix_specs = match keys.get("prefix") {
        Some(v) => strings(v),
        None => vec![registry_prefix.to_string()],
    };
    let prefix = match prefix_specs.iter().find_map(|p| combo(p)) {
        Some(p) => p,
        None => {
            warnings.push(format!(
                "no usable prefix in {prefix_specs:?}; herdr falls back to ctrl+b"
            ));
            "ctrl+b".to_string()
        }
    };

    let mut entries = Vec::new();
    for name in action_order(registry_toml) {
        let Some(spec) = actions.get(&name) else {
            continue;
        };
        let desc = spec
            .get("desc")
            .and_then(Value::as_str)
            .map_or_else(|| name.replace('_', " "), str::to_string);
        let (keys, user) = match user_value(&name) {
            Some(v) => (strings(v), true),
            None if indexed_replaces(&name) => (Vec::new(), false),
            None => (spec.get("keys").map(strings).unwrap_or_default(), false),
        };
        entries.push(Entry {
            category: "actions",
            keys,
            desc,
            action: name,
            user,
        });
    }
    // [keys] names the registry doesn't know (a newer herdr, say), listed so they aren't lost. herdr 0.9.3 itself
    // ignores unknown names.
    for (name, v) in keys {
        let not_an_action = actions.contains_key(name)
            || [
                "prefix",
                "extra_prefixes",
                "fullscreen",
                "command",
                "indexed",
            ]
            .contains(&name.as_str())
            || name.starts_with("navigate_");
        if !not_an_action {
            entries.push(Entry {
                category: "actions",
                keys: strings(v),
                desc: name.replace('_', " "),
                action: name.clone(),
                user: true,
            });
        }
    }
    let titles = plugin_titles(plugins_json);
    for cmd in keys
        .get("command")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let command = cmd
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if command.trim().is_empty() {
            warnings
                .push("a [[keys.command]] with an empty command is disabled by herdr".to_string());
            continue;
        }
        let described = cmd
            .get("description")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty());
        let plugin_title = (cmd.get("type").and_then(Value::as_str) == Some("plugin_action"))
            .then(|| titles.get(command))
            .flatten();
        entries.push(Entry {
            category: "commands",
            keys: cmd.get("key").map(strings).unwrap_or_default(),
            desc: described
                .or(plugin_title.map(String::as_str))
                .unwrap_or(command)
                .to_string(),
            action: command.to_string(),
            user: true,
        });
    }

    // herdr registers user bindings (actions, then commands) before defaults. On a clash it silently drops a default
    // and keeps the first of two user bindings.
    let mut binds: BTreeMap<String, Vec<Bind>> = BTreeMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    entries.sort_by_key(|e| !e.user);
    for e in &entries {
        let indexed_action = e.category == "actions" && INDEXED.contains(&e.action.as_str());
        for spec in e.keys.iter().filter(|k| !k.is_empty()) {
            let rejected =
                |why: &str| format!("{} = {spec:?} is disabled by herdr: {why}", e.action);
            if spec.contains("1..9") != indexed_action {
                if e.user {
                    warnings.push(rejected(if indexed_action {
                        "this action takes only a 1..9 range"
                    } else {
                        "a 1..9 range is only valid for switch_tab, switch_workspace and focus_agent"
                    }));
                }
                continue;
            }
            if printable_direct(spec) {
                warnings.push(rejected("an unmodified printable key outside prefix mode"));
                continue;
            }
            let Some(expanded) = expand(spec, &prefix) else {
                warnings.push(format!(
                    "skipping {} = {spec:?}: no keysym we know",
                    e.action
                ));
                continue;
            };
            for key in expanded {
                if key == format!("{prefix} {prefix}") {
                    warnings.push(rejected("the prefix itself"));
                    continue;
                }
                if !taken.insert(key.clone()) {
                    if e.user {
                        warnings.push(rejected(&format!("{key} is already bound")));
                    }
                    continue;
                }
                binds.entry(e.category.to_string()).or_default().push(Bind {
                    key,
                    desc: e.desc.clone(),
                    action: e.action.clone(),
                    when: Default::default(),
                });
            }
        }
    }
    let version = registry
        .get("herdr_version")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let sheet = Sheet {
        title: format!("herdr (defaults: registry {version})"),
        provider: "herdr".into(),
        binds,
    };
    Ok((sheet, warnings))
}

/// The first `[keys]` field whose value herdr would refuse to deserialize, if any. Unknown names are not checked:
/// herdr ignores them.
fn type_error(keys: &Table, actions: &Table) -> Option<String> {
    let binding = |v: &Value| match v {
        Value::String(_) => true,
        Value::Array(a) => a.iter().all(Value::is_str),
        _ => false,
    };
    keys.iter()
        .find(|(name, v)| match name.as_str() {
            "remote_image_paste" => !v.is_str(),
            "command" => !v.as_array().is_some_and(|a| a.iter().all(Value::is_table)),
            "indexed" => !v.is_table(),
            "prefix" | "extra_prefixes" | "fullscreen" => !binding(v),
            n if n.starts_with("navigate_") || actions.contains_key(n) => !binding(v),
            _ => false,
        })
        .map(|(name, _)| name.clone())
}

/// A key setting is a string or an array of strings; `""` and `[]` mean unbound.
fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// A direct (non-prefix) binding of a lone printable key, which herdr refuses: it would swallow typing.
fn printable_direct(spec: &str) -> bool {
    const PRINTABLE_NAMES: &[&str] = &[
        "space",
        "minus",
        "comma",
        "period",
        "slash",
        "backslash",
        "quote",
        "double_quote",
        "double-quote",
        "semicolon",
        "colon",
        "percent",
        "ampersand",
        "backtick",
        "plus",
    ];
    let s = spec.trim();
    if s.starts_with("prefix+") || s.contains('+') && s != "+" {
        return false;
    }
    s.chars().count() == 1 || PRINTABLE_NAMES.contains(&s.to_ascii_lowercase().as_str())
}

/// `plugin_id.action_id` → title, from herdr's `plugins.json`. Anything unreadable means no titles.
fn plugin_titles(plugins_json: &str) -> HashMap<String, String> {
    let Ok(serde_json::Value::Array(plugins)) = serde_json::from_str(plugins_json) else {
        return HashMap::new();
    };
    let mut titles = HashMap::new();
    for p in &plugins {
        let Some(id) = p.get("plugin_id").and_then(|v| v.as_str()) else {
            continue;
        };
        for a in p
            .get("actions")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let (Some(action), Some(title)) = (
                a.get("id").and_then(|v| v.as_str()),
                a.get("title").and_then(|v| v.as_str()),
            ) {
                titles.insert(format!("{id}.{action}"), title.to_string());
            }
        }
    }
    titles
}

/// A herdr key string as chord sequences: `prefix+1..9` gives nine, anything else one. `None` if untranslatable.
fn expand(herdr: &str, prefix: &str) -> Option<Vec<String>> {
    if herdr.contains("1..9") {
        return (1..=9)
            .map(|n| key(&herdr.replace("1..9", &n.to_string()), prefix))
            .collect();
    }
    key(herdr, prefix).map(|k| vec![k])
}

/// `prefix+shift+l` with prefix `ctrl+b` → `ctrl+b shift+l`; `ctrl+v` → `ctrl+v`. `prefix+` is case-sensitive,
/// as in herdr.
fn key(herdr: &str, prefix: &str) -> Option<String> {
    let s = herdr.trim();
    match s.strip_prefix("prefix+") {
        Some(rest) => Some(format!("{prefix} {}", combo(rest)?)),
        None => combo(s),
    }
}

/// One herdr combo (`shift+tab`, `ctrl+b`, `?`, `N`) as a chord, following herdr's `parse_key_combo`.
fn combo(s: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut key = None;
    for part in s.split('+').map(str::trim) {
        let modifier = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some("ctrl"),
            "shift" => Some("shift"),
            "alt" | "option" | "meta" => Some("alt"),
            "cmd" | "command" | "super" => Some("super"),
            "hyper" | "" => return None,
            _ => None,
        };
        match modifier {
            Some(m) => mods.push(m),
            None if key.is_none() => key = Some(part),
            None => return None,
        }
    }
    let key = key?;
    let lower = key.to_ascii_lowercase();
    let named = match lower.as_str() {
        "space" => "space",
        "enter" | "return" => "Return",
        "esc" | "escape" => "Escape",
        "tab" => "Tab",
        "backspace" | "bs" => "BackSpace",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "minus" => "minus",
        "comma" => "comma",
        "period" => "period",
        "slash" => "slash",
        "backslash" => "backslash",
        "quote" => "apostrophe",
        "double_quote" | "double-quote" => "quotedbl",
        "semicolon" => "semicolon",
        "colon" => "colon",
        "percent" => "percent",
        "ampersand" => "ampersand",
        "backtick" => "grave",
        "plus" => "plus",
        _ => "",
    };
    let keysym = if !named.is_empty() {
        named.to_string()
    } else {
        let mut chars = key.chars();
        match (chars.next(), chars.next()) {
            // An uppercase letter is shift + the letter, as herdr reads it.
            (Some(c), None) if c.is_ascii_uppercase() => {
                mods.push("shift");
                c.to_ascii_lowercase().to_string()
            }
            (Some(c), None) if c.is_ascii_alphanumeric() => c.to_string(),
            // ponytail: non-ASCII keys (`ö`), which herdr accepts, are skipped; add keysym names if one is ever used.
            (Some(c), None) => punctuation(c)?.to_string(),
            _ if lower.starts_with('f') && lower[1..].parse::<u8>().is_ok() => {
                format!("F{}", &lower[1..])
            }
            _ => return None,
        }
    };
    Some(chord(&mods, &keysym))
}

fn punctuation(c: char) -> Option<&'static str> {
    Some(match c {
        '?' => "question",
        '[' => "bracketleft",
        ']' => "bracketright",
        '{' => "braceleft",
        '}' => "braceright",
        '-' => "minus",
        ',' => "comma",
        '.' => "period",
        '/' => "slash",
        '\\' => "backslash",
        '\'' => "apostrophe",
        '"' => "quotedbl",
        ';' => "semicolon",
        ':' => "colon",
        '%' => "percent",
        '&' => "ampersand",
        '`' => "grave",
        '=' => "equal",
        '!' => "exclam",
        '@' => "at",
        '#' => "numbersign",
        '$' => "dollar",
        '^' => "asciicircum",
        '*' => "asterisk",
        '(' => "parenleft",
        ')' => "parenright",
        '<' => "less",
        '>' => "greater",
        '|' => "bar",
        '~' => "asciitilde",
        '_' => "underscore",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/herdr.toml"
    ));
    const PLUGINS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/herdr-plugins.json"
    ));
    const REG: &str = REGISTRY[0].1;

    fn all(s: &Sheet) -> Vec<(&str, &Bind)> {
        s.binds
            .iter()
            .flat_map(|(c, bs)| bs.iter().map(move |b| (c.as_str(), b)))
            .collect()
    }

    fn keys_of<'a>(s: &'a Sheet, action: &str) -> Vec<&'a str> {
        all(s)
            .into_iter()
            .filter(|(_, b)| b.action == action)
            .map(|(_, b)| b.key.as_str())
            .collect()
    }

    fn on<'a>(s: &'a Sheet, key: &str) -> Vec<(&'a str, &'a Bind)> {
        all(s).into_iter().filter(|(_, b)| b.key == key).collect()
    }

    fn report(config: &str) -> (Sheet, Vec<String>) {
        parse_reporting(config, REG, "").unwrap()
    }

    #[test]
    fn fixture_title_and_provider() {
        let s = parse(CONFIG, REG, PLUGINS).unwrap();
        assert_eq!(s.provider, "herdr");
        assert_eq!(s.title, "herdr (defaults: registry 0.9.3)");
    }

    #[test]
    fn prefix_expansion_and_overrides() {
        let s = parse(CONFIG, REG, PLUGINS).unwrap();
        assert_eq!(keys_of(&s, "new_tab"), ["ctrl+b t", "ctrl+b c"]);
        assert_eq!(keys_of(&s, "zoom"), ["ctrl+b f", "ctrl+b z"]);
        assert_eq!(
            keys_of(&s, "split_vertical"),
            ["ctrl+b shift+v", "ctrl+b v"]
        );
    }

    #[test]
    fn unbinding_removes_actions() {
        let s = parse(CONFIG, REG, PLUGINS).unwrap();
        assert!(keys_of(&s, "resize_mode").is_empty());
        assert!(keys_of(&s, "toggle_sidebar").is_empty());
        assert!(all(&s).iter().all(|(_, b)| !b.key.is_empty()));
        let (s, _) = report("[keys]\nhelp = []\n");
        assert!(keys_of(&s, "help").is_empty());
    }

    #[test]
    fn commands_and_displacement() {
        let (s, warnings) = parse_reporting(CONFIG, REG, PLUGINS).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let commands: Vec<(&str, &str, &str)> = s.binds["commands"]
            .iter()
            .map(|b| (b.key.as_str(), b.desc.as_str(), b.action.as_str()))
            .collect();
        assert_eq!(
            commands,
            [
                ("ctrl+b u", "open the example", "example-plugin.open"),
                ("ctrl+b shift+k", "go back", "example-plugin.back"),
            ]
        );
        assert_eq!(on(&s, "ctrl+b shift+k").len(), 1);
        assert!(keys_of(&s, "swap_pane_up").is_empty());
        assert_eq!(keys_of(&s, "swap_pane_left"), ["ctrl+b shift+h"]);
        // A [keys] override also displaces another action's default.
        let (s, _) = report("[keys]\nhelp = \"prefix+l\"\n");
        assert_eq!(keys_of(&s, "help"), ["ctrl+b l"]);
        assert!(keys_of(&s, "focus_pane_right").is_empty());
    }

    #[test]
    fn user_clashes_keep_the_first() {
        // An action field wins over a command on the same key; a repeated key is listed once.
        let config = "[keys]\nhelp = \"prefix+t\"\ngoto = [\"prefix+f\", \"prefix+f\"]\n\n[[keys.command]]\nkey = \"prefix+t\"\ntype = \"shell\"\ncommand = \"htop\"\n";
        let (s, warnings) = report(config);
        assert_eq!(on(&s, "ctrl+b t").len(), 1);
        assert_eq!(on(&s, "ctrl+b t")[0].1.action, "help");
        assert_eq!(keys_of(&s, "goto"), ["ctrl+b f"]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        // Between two actions, herdr's field order decides: `help` comes before `goto`.
        let (s, _) = report("[keys]\ngoto = \"prefix+y\"\nhelp = \"prefix+y\"\n");
        assert_eq!(on(&s, "ctrl+b y")[0].1.action, "help");
    }

    #[test]
    fn command_description_fallbacks() {
        let config = r#"
[[keys.command]]
key = "prefix+y"
type = "plugin_action"
command = "example-plugin.open-side"

[[keys.command]]
key = "prefix+u"
type = "shell"
command = "htop"

[[keys.command]]
key = "prefix+i"
type = "shell"
command = ""
"#;
        let s = parse(config, REG, PLUGINS).unwrap();
        assert_eq!(on(&s, "ctrl+b y")[0].1.desc, "Example: side pane");
        assert_eq!(on(&s, "ctrl+b u")[0].1.desc, "htop");
        assert!(on(&s, "ctrl+b i").is_empty());
        for plugins in ["", "not json", "{\"plugins\": []}"] {
            let s = parse(config, REG, plugins).unwrap();
            assert_eq!(on(&s, "ctrl+b y")[0].1.desc, "example-plugin.open-side");
        }
    }

    #[test]
    fn prefix_override_and_ranges() {
        let (s, _) = report("[keys]\nprefix = \"ctrl+a\"\n");
        assert_eq!(keys_of(&s, "help"), ["ctrl+a question"]);
        let tabs = keys_of(&s, "switch_tab");
        assert_eq!(tabs.len(), 9);
        assert_eq!((tabs[0], tabs[8]), ("ctrl+a 1", "ctrl+a 9"));
        let (s, _) = report("[keys]\nprefix = [\"f12\", \"ctrl+b\"]\n");
        assert_eq!(keys_of(&s, "help"), ["F12 question"]);
        let (s, _) = report("");
        assert_eq!(keys_of(&s, "switch_tab")[0], "ctrl+b 1");
        assert_eq!(keys_of(&s, "remote_image_paste"), ["ctrl+v"]);
    }

    #[test]
    fn bad_prefixes_fall_back() {
        for config in [
            "[keys]\nprefix = \"\"\n",
            "[keys]\nprefix = []\n",
            "[keys]\nprefix = \"hyper+a\"\n",
        ] {
            let (s, warnings) = report(config);
            assert_eq!(keys_of(&s, "help"), ["ctrl+b question"], "{config}");
            assert_eq!(warnings.len(), 1, "{config}: {warnings:?}");
        }
        let (s, _) = report("[keys]\nprefix = [\"hyper+a\", \"ctrl+a\"]\n");
        assert_eq!(keys_of(&s, "help"), ["ctrl+a question"]);
    }

    #[test]
    fn missing_and_invalid_config() {
        let (s, _) = report("");
        assert_eq!(keys_of(&s, "help"), ["ctrl+b question"]);
        assert!(!s.binds.contains_key("commands"));
        assert!(parse("[keys\n", REG, "").is_err());
    }

    #[test]
    fn type_errors_mean_defaults() {
        for config in [
            "[keys]\nhelp = 5\ngoto = \"prefix+f\"\n",
            "[keys]\nremote_image_paste = [\"ctrl+v\"]\ngoto = \"prefix+f\"\n",
            "[keys]\ngoto = [\"prefix+f\", 1]\n",
            "keys = 5\n",
        ] {
            let (s, warnings) = report(config);
            assert_eq!(keys_of(&s, "goto"), ["ctrl+b g"], "{config}");
            assert!(warnings[0].contains("runs on its defaults"), "{warnings:?}");
        }
        // An unknown name with an odd type is ignored, as herdr does.
        let (s, warnings) = report("[keys]\nsomething_new = 5\ngoto = \"prefix+f\"\n");
        assert_eq!(keys_of(&s, "goto"), ["ctrl+b f"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn aliases_and_non_actions() {
        let (s, warnings) = report(
            "[keys]\nfullscreen = \"prefix+f\"\nextra_prefixes = [\"ctrl+a\"]\nfuture_thing = \"prefix+y\"\nnavigate_pane_left = \"a\"\n",
        );
        assert_eq!(keys_of(&s, "zoom"), ["ctrl+b f"]);
        assert!(keys_of(&s, "fullscreen").is_empty());
        assert!(keys_of(&s, "extra_prefixes").is_empty());
        assert_eq!(on(&s, "ctrl+b y")[0].1.desc, "future thing");
        assert!(keys_of(&s, "navigate_pane_left").is_empty());
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn bindings_herdr_disables() {
        let config = "[keys]\nhelp = \"?\"\nsettings = \"prefix+ctrl+b\"\nzoom = \"Prefix+z\"\nswitch_tab = \"prefix+x\"\ngoto = \"prefix+1..9\"\n\n[[keys.command]]\nkey = \"prefix+1..9\"\ncommand = \"htop\"\n";
        let (s, warnings) = report(config);
        for action in ["help", "settings", "zoom", "switch_tab", "goto", "htop"] {
            assert!(keys_of(&s, action).is_empty(), "{action}");
        }
        assert_eq!(warnings.len(), 6, "{warnings:?}");
    }

    #[test]
    fn indexed_tabs_replace_the_switch_tab_default() {
        let (s, warnings) = report("[keys.indexed]\ntabs = \"prefix+1..9\"\n");
        assert!(keys_of(&s, "switch_tab").is_empty());
        assert_eq!(warnings, ["[keys.indexed] bindings are not listed"]);
        let (s, _) = report("[keys.indexed]\nagents = \"prefix+alt+1..9\"\n");
        assert_eq!(keys_of(&s, "switch_tab").len(), 9);
    }

    #[test]
    fn combo_translation() {
        for (herdr, want) in [
            ("ctrl+b", "ctrl+b"),
            ("prefix+?", "P question"),
            ("prefix+shift+tab", "P shift+Tab"),
            ("prefix+N", "P shift+n"),
            ("prefix+minus", "P minus"),
            ("prefix+[", "P bracketleft"),
            ("prefix+ctrl+alt+x", "P ctrl+alt+x"),
            ("super+enter", "super+Return"),
            ("f12", "F12"),
            ("prefix+quote", "P apostrophe"),
        ] {
            assert_eq!(key(herdr, "P").as_deref(), Some(want), "{herdr}");
        }
        for bad in [
            "hyper+x",
            "ctrl+",
            "ctrl+a+b",
            "prefix+",
            "Prefix+space",
            "é",
        ] {
            assert_eq!(key(bad, "P"), None, "{bad}");
        }
    }

    #[test]
    fn versions() {
        assert_eq!(version_from("herdr 0.9.3\n").as_deref(), Some("0.9.3"));
        assert_eq!(
            version_from("herdr 0.9.4-preview.123").as_deref(),
            Some("0.9.4")
        );
        assert_eq!(version_from("herdr v0.10 (abc)").as_deref(), Some("0.10.0"));
        assert_eq!(version_from("garbage"), None);
        assert_eq!(parse_version("0.9"), parse_version("0.9.0"));
        assert!(parse_version("0.10.0") > parse_version("0.9.3"));
    }

    #[test]
    fn registry_selection() {
        assert_eq!(select(Some("0.9.3")), Some(("0.9.3", REG, None)));
        let fake: &[(&str, &str)] = &[("0.10.0", "c"), ("0.9.1", "a"), ("0.9.3", "b")];
        let pick = |i: Option<&str>| select_from(fake, i).unwrap();
        assert_eq!(pick(Some("0.9.3")), ("0.9.3", "b", None));
        assert_eq!(
            pick(Some("0.9.5")),
            (
                "0.9.3",
                "b",
                Some(
                    "herdr 0.9.5: using defaults registry 0.9.3; add data/herdr/0.9.5.toml".into()
                )
            )
        );
        assert_eq!(pick(Some("0.10.2")).0, "0.10.0");
        assert_eq!(pick(Some("0.9.0")).0, "0.9.1");
        assert_eq!(
            pick(None),
            (
                "0.9.1",
                "a",
                Some("herdr version unknown: using defaults registry 0.9.1".into())
            )
        );
        assert_eq!(select_from(&[], Some("0.9.3")), None);
    }

    #[test]
    fn registry_file() {
        assert!(REGISTRY.iter().all(|(v, _)| parse_version(v).is_some()));
        let reg: Table = REG.parse().unwrap();
        let actions = reg["actions"].as_table().unwrap();
        assert_eq!(actions.len(), 53);
        assert_eq!(action_order(REG).len(), 53);
        assert_eq!(
            action_order(REG)[..3],
            ["help", "settings", "new_workspace"]
        );
        assert!(actions.keys().all(|k| !k.starts_with("navigate_")));
        assert_eq!(reg["prefix"].as_str(), Some("ctrl+b"));
        // Spot checks against herdr v0.9.3 `impl Default for KeysConfig`.
        for (action, keys) in [
            ("swap_pane_right", vec!["prefix+shift+l"]),
            ("copy_mode", vec!["prefix+["]),
            ("zoom", vec!["prefix+z"]),
            ("switch_tab", vec!["prefix+1..9"]),
            ("remote_image_paste", vec!["ctrl+v"]),
            ("last_pane", vec![]),
        ] {
            assert_eq!(strings(&actions[action]["keys"]), keys, "{action}");
        }
    }

    #[test]
    fn config_locations_and_reading() {
        let dir =
            |x: Option<&str>, h: Option<&str>| config_dir(x.map(Into::into), h.map(Into::into));
        assert_eq!(dir(Some("/x"), Some("/h")), Some("/x/herdr".into()));
        assert_eq!(dir(None, Some("/h")), Some("/h/.config/herdr".into()));
        assert_eq!(
            dir(Some("rel"), Some("/h")),
            Some("/h/.config/herdr".into())
        );
        assert_eq!(dir(None, None), None);
        let tmp = std::env::temp_dir().join(format!("kbf-herdr-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(read_or_empty(tmp.join("missing")).unwrap(), "");
        assert!(read_or_empty(tmp.clone()).is_err());
        std::fs::write(
            tmp.join("bom.toml"),
            "\u{feff}[keys]\n\u{feff}goto = \"prefix+f\"\n",
        )
        .unwrap();
        let text = read_or_empty(tmp.join("bom.toml")).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(keys_of(&report(&text).0, "goto"), ["ctrl+b f"]);
    }
}
