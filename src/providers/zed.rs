//! Zed: its keys from the default keymaps pinned at v1.25.3 (`data/zed/`), for the base keymap and vim or helix mode
//! that `settings.json` chooses, with the user's `keymap.json` on top; each bind's `context` as scopes. Shown when a
//! Zed window has focus. The file's extension, guessed from the window title, is a fact.

use crate::context::Context;
use crate::scope::Facts;
use crate::sheet::{Bind, Name, Sheet, Values};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

macro_rules! pinned {
    ($file:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/zed/1.25.3/",
            $file
        ))
    };
}

const DEFAULT: &str = pinned!("default-linux.json");
const VIM: &str = pinned!("vim.json");
const OVERRIDES: &str = pinned!("specific-overrides.json");

/// `base_keymap` values (as `settings.json` spells them) and their Linux file; `Zed` and `TextMate` have none, `None`
/// loads no defaults at all.
const BASES: [(&str, Option<&str>); 9] = [
    ("Zed", None),
    ("VSCode", Some(pinned!("linux/vscode.json"))),
    ("JetBrains", Some(pinned!("linux/jetbrains.json"))),
    ("SublimeText", Some(pinned!("linux/sublime_text.json"))),
    ("Atom", Some(pinned!("linux/atom.json"))),
    ("TextMate", None),
    ("Emacs", Some(pinned!("linux/emacs.json"))),
    ("Cursor", Some(pinned!("linux/cursor.json"))),
    ("None", None),
];

/// Zed's app ids (Wayland) and X11 classes, by release channel.
const CLASSES: [&str; 4] = [
    "dev.zed.Zed",
    "dev.zed.Zed-Preview",
    "dev.zed.Zed-Nightly",
    "dev.zed.Zed-Dev",
];

pub(crate) fn is_zed(class: &str) -> bool {
    CLASSES.contains(&class)
}

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, facts, notes) = read(ctx.title.as_deref(), &env);
    crate::context::set_facts(facts);
    for note in notes {
        eprintln!("keybind-find: zed: {note}");
    }
    Ok(sheet)
}

/// The sheet, the facts from `title` and the notes for stderr, with the user's files found through `env`.
fn read(title: Option<&str>, env: Env) -> (Sheet, Facts, Vec<String>) {
    let mut notes = Vec::new();
    let dir = config_dir(env);
    let user = |name: &str, notes: &mut Vec<String>| -> Option<Value> {
        let path = dir.as_ref()?.join(name);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                notes.push(format!("{}: {e}; leaving it out", path.display()));
                return None;
            }
        };
        let text = super::micro::strip_jsonc(&text);
        // An empty file (or only comments) is no settings and no binds, as for Zed.
        if text.trim().is_empty() {
            return None;
        }
        serde_json::from_str(&text)
            .map_err(|e| notes.push(format!("{}: {e}; leaving it out", path.display())))
            .ok()
    };
    let settings = user("settings.json", &mut notes);
    let (base, vim) = settings_of(settings.as_ref(), &mut notes);
    let keymap = user("keymap.json", &mut notes);
    let mut keymaps: Vec<(&str, Value)> = Vec::new();
    for (name, text) in load_order(base, vim) {
        match serde_json::from_str(&super::micro::strip_jsonc(text)) {
            Ok(v) => keymaps.push((name, v)),
            Err(e) => notes.push(format!("the pinned {name}: {e}")),
        }
    }
    if let Some(k) = keymap {
        keymaps.push(("keymap.json", k));
    }
    let (sheet, left_out, bad_contexts) = build(&keymaps, &mut notes);
    let mut counted = Vec::new();
    if left_out > 0 {
        counted.push(format!(
            "leaving out {left_out} key(s) keybind-find can't send (fn-, a lone modifier, shift- with a non-letter, \
             or a key with no keysym)"
        ));
    }
    if bad_contexts > 0 {
        counted.push(format!(
            "{bad_contexts} bind(s) whose context didn't parse are shown untagged"
        ));
    }
    if !counted.is_empty() {
        notes.push(counted.join("; "));
    }
    (sheet, facts(title), notes)
}

/// Zed's config directory: `$XDG_CONFIG_HOME/zed` (`~/.config/zed` when unset, empty or relative), else the Flatpak's
/// when the host's is absent.
fn config_dir(env: Env) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let home = set("HOME");
    let config = set("XDG_CONFIG_HOME")
        .filter(|d| d.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")))?;
    let host = config.join("zed");
    if host.is_dir() {
        return Some(host);
    }
    let flatpak = home?.join(".var/app/dev.zed.Zed/config/zed");
    Some(if flatpak.is_dir() { flatpak } else { host })
}

/// `base_keymap` (unset or unknown: `Zed`, an unknown one with a note) and whether vim's keymap loads (`vim_mode` or
/// `helix_mode`).
fn settings_of(settings: Option<&Value>, notes: &mut Vec<String>) -> (&'static str, bool) {
    let Some(settings) = settings else {
        return ("Zed", false);
    };
    if !settings.is_object() {
        notes.push("settings.json isn't an object; using the defaults".into());
        return ("Zed", false);
    }
    let base = match &settings["base_keymap"] {
        Value::Null => "Zed",
        v => match BASES.iter().find(|(n, _)| v.as_str() == Some(n)) {
            Some((n, _)) => n,
            None => {
                notes.push(format!(
                    "settings.json's base_keymap {v} isn't one keybind-find knows; using Zed's"
                ));
                "Zed"
            }
        },
    };
    let on = |k: &str| settings[k].as_bool() == Some(true);
    (base, on("vim_mode") || on("helix_mode"))
}

/// The pinned keymaps in Zed's order (`load_default_keymap`), each with a name for notes.
fn load_order(base: &str, vim: bool) -> Vec<(&'static str, &'static str)> {
    if base == "None" {
        return Vec::new();
    }
    let mut files = vec![("default-linux.json", DEFAULT)];
    if let Some((_, Some(text))) = BASES.iter().find(|(n, _)| *n == base) {
        files.push(("base keymap", text));
    }
    if vim {
        files.push(("vim.json", VIM));
    }
    files.push(("specific-overrides.json", OVERRIDES));
    files
}

/// One bind as loaded: listed unless a later one with the same key and context text replaces or removes it.
struct Entry {
    /// The key as notation, or as written when it has none (then left out).
    key: Result<String, String>,
    /// The context with spaces removed, for comparing.
    context: String,
    scopes: Option<Scopes>,
    action: String,
    desc: String,
}

/// The sheet from `keymaps` in load order, how many listed keys were left out, and how many listed binds had a context
/// that didn't parse. A keymap that isn't Zed's shape is a note and left out.
fn build(keymaps: &[(&str, Value)], notes: &mut Vec<String>) -> (Sheet, usize, usize) {
    let mut entries: Vec<Entry> = Vec::new();
    for (name, keymap) in keymaps {
        let Some(sections) = sections(keymap) else {
            notes.push(format!(
                "{name} isn't a list of {{\"context\", \"bindings\"}} objects; leaving it out"
            ));
            continue;
        };
        // As Zed's partial load: a binding whose action isn't Zed's shape is left out, the rest kept.
        let invalid = sections
            .iter()
            .map(|(_, u, b)| {
                u.iter().filter(|(_, a)| !valid(a, false)).count()
                    + b.iter().filter(|(_, a)| !valid(a, true)).count()
            })
            .sum::<usize>();
        if invalid > 0 {
            notes.push(format!(
                "{name}: leaving out {invalid} binding(s) whose action isn't a name, [name, args] or null"
            ));
        }
        for (context, unbind, bindings) in sections {
            let unbind: Vec<_> = unbind
                .into_iter()
                .filter(|(_, a)| valid(a, false))
                .collect();
            let bindings: Vec<_> = bindings
                .into_iter()
                .filter(|(_, a)| valid(a, true))
                .collect();
            let mut context_text: String = context.chars().filter(|c| !c.is_whitespace()).collect();
            // Spaces alone are a context (that doesn't parse), not none.
            if context_text.is_empty() {
                context_text = context.to_string();
            }
            // As Zed: only an empty context is none; one of spaces doesn't parse.
            let scopes = if context.is_empty() {
                Some(Scopes::new())
            } else {
                parse(context).map(|p| scopes(&p))
            };
            // An unbind (in `unbind`, or bound to `zed::Unbind`) removes the earlier binds of that key to that action
            // (by name, as Zed's `binding_is_unbound`), in that context (any, with none); it lists nothing.
            let unbinds = unbind
                .iter()
                .map(|(k, a)| (*k, action_of(a).map(|(n, _)| n)));
            let bound = bindings.iter().filter_map(|(k, a)| match action_of(a) {
                Some(("zed::Unbind", Some(Value::String(target)))) => {
                    Some((*k, Some(target.as_str())))
                }
                _ => None,
            });
            for (key, target) in unbinds.chain(bound) {
                let key = notation(key).ok_or_else(|| key.clone());
                entries.retain(|e| {
                    e.key != key
                        || Some(e.action.as_str()) != target
                        || !(context.is_empty() || e.context == context_text)
                });
            }
            for (key, action) in bindings {
                let key = notation(key).ok_or_else(|| key.clone());
                let (action, args) = match action_of(action) {
                    Some(("zed::Unbind", _)) => continue,
                    Some((a, args)) => (a.to_string(), args),
                    // `null`: removes the earlier binds with this key and context, listing nothing.
                    None => {
                        entries.retain(|e| e.key != key || e.context != context_text);
                        continue;
                    }
                };
                entries.retain(|e| e.key != key || e.context != context_text);
                let desc = match args {
                    Some(args) => format!("{action} {args}"),
                    None => action.clone(),
                };
                entries.push(Entry {
                    key,
                    context: context_text.clone(),
                    scopes: scopes.clone(),
                    action,
                    desc,
                });
            }
        }
    }
    let mut sheet = Sheet {
        title: "Zed".into(),
        provider: "zed".into(),
        binds: Default::default(),
    };
    let (mut left_out, mut bad_contexts) = (0, 0);
    for e in entries {
        let Ok(key) = e.key else {
            left_out += 1;
            continue;
        };
        if e.scopes.is_none() {
            bad_contexts += 1;
        }
        let category = e.action.split_once("::").map_or("zed", |(ns, _)| ns);
        sheet
            .binds
            .entry(category.to_string())
            .or_default()
            .push(Bind {
                key,
                desc: e.desc,
                action: e.action,
                when: e.scopes.as_ref().map(scopes_of).unwrap_or_default(),
            });
    }
    (sheet, left_out, bad_contexts)
}

/// An action's name and args; `None` for `null` (and the old `"zed::NoAction"`, its alias).
fn action_of(action: &Value) -> Option<(&str, Option<&Value>)> {
    match action {
        Value::String(a) => Some((a.as_str(), None)),
        Value::Array(a) => Some((a.first()?.as_str()?, a.get(1))),
        _ => None,
    }
    .filter(|(name, _)| *name != "zed::NoAction")
}

/// Whether an action is Zed's shape: a name or `[name, args]` (`zed::Unbind`'s argument a name); `null` only where
/// `nullable` (a binding, not an unbind target).
fn valid(action: &Value, nullable: bool) -> bool {
    match action {
        Value::Null => nullable,
        Value::String(n) => n != "zed::Unbind",
        Value::Array(a) => {
            a.len() == 2 && a[0].is_string() && (a[0] != "zed::Unbind" || a[1].is_string())
        }
        _ => false,
    }
}

/// One keymap section: its context, `unbind` and `bindings`.
type Section<'a> = (
    &'a str,
    Vec<(&'a String, &'a Value)>,
    Vec<(&'a String, &'a Value)>,
);

/// A keymap's sections; `None` when it isn't Zed's shape (a list of objects with a string `context` and object
/// `bindings` and `unbind`). Actions are checked one by one (`valid`).
fn sections(keymap: &Value) -> Option<Vec<Section<'_>>> {
    keymap
        .as_array()?
        .iter()
        .map(|s| {
            s.as_object()?;
            let context = match &s["context"] {
                Value::Null => "",
                c => c.as_str()?,
            };
            // Both optional: a section may hold only `unbind` (Zed's keymap editor writes those).
            let map = |k: &str| -> Option<Vec<_>> {
                match &s[k] {
                    Value::Null => Some(Vec::new()),
                    b => Some(b.as_object()?.iter().collect()),
                }
            };
            Some((context, map("unbind")?, map("bindings")?))
        })
        .collect()
}

/// A Zed keystroke sequence (`ctrl-shift-p`, `cmd-k cmd-s`, `ctrl--`) as key notation; `None` when a key can't be sent.
fn notation(key: &str) -> Option<String> {
    let strokes: Option<Vec<String>> = key.split_whitespace().map(stroke).collect();
    strokes.filter(|s| !s.is_empty()).map(|s| s.join(" "))
}

fn stroke(stroke: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut rest = stroke;
    'mods: loop {
        for (prefix, m) in [
            ("ctrl-", "ctrl"),
            ("alt-", "alt"),
            ("shift-", "shift"),
            ("super-", "super"),
            ("cmd-", "super"),
            ("win-", "super"),
            // `secondary` is ctrl on Linux.
            ("secondary-", "ctrl"),
            ("fn-", "fn"),
        ] {
            // `ctrl--` is ctrl and the `-` key. Zed reads modifiers in any case (`Ctrl-P`).
            let r = rest
                .get(..prefix.len())
                .filter(|p| p.eq_ignore_ascii_case(prefix))
                .map(|_| &rest[prefix.len()..]);
            if let Some(r) = r.filter(|r| !r.is_empty()) {
                if m == "fn" {
                    return None;
                }
                mods.push(m);
                rest = r;
                continue 'mods;
            }
        }
        break;
    }
    // Named keys in any case too (`Enter`, `F5`); a single letter's case is shift.
    let named = rest.to_ascii_lowercase();
    let keysym = match named.as_str() {
        "enter" => "Return".to_string(),
        "escape" => "Escape".into(),
        "backspace" => "BackSpace".into(),
        "pageup" => "Prior".into(),
        "pagedown" => "Next".into(),
        "space" => "space".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "insert" => "Insert".into(),
        "delete" => "Delete".into(),
        "tab" => "Tab".into(),
        f if f.len() > 1
            && f.starts_with('f')
            && !f[1..].starts_with('0')
            && f[1..].bytes().all(|b| b.is_ascii_digit())
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) =>
        {
            format!("F{}", &f[1..])
        }
        _ => {
            let mut chars = rest.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                // A lone modifier (`shift`) or a name with no keysym.
                return None;
            };
            // Zed matches `shift-` only with letters.
            if mods.contains(&"shift") && !c.is_ascii_alphabetic() {
                return None;
            }
            if c.is_ascii_uppercase() {
                mods.push("shift");
            }
            super::nvim::single(c.to_ascii_lowercase())?.to_string()
        }
    };
    Some(crate::sheet::chord(&mods, &keysym))
}

/// A parsed context predicate (gpui's `KeyBindingContextPredicate`).
#[derive(Debug, Clone, PartialEq)]
enum Pred {
    Id(String),
    Eq(String, String),
    NotEq,
    Not(Box<Pred>),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    /// `A > B`: B in a descendant of A.
    Child(Box<Pred>, Box<Pred>),
}

/// Parses a context as Zed does (`parse_expr` in gpui's `keymap/context.rs`); `None` when it doesn't parse.
fn parse(source: &str) -> Option<Pred> {
    let (pred, rest) = expr(source.trim_start(), 0)?;
    rest.is_empty().then_some(pred)
}

/// Precedence climbing: `>` 1, `||` 2, `&&` 3, `==`/`!=` 4, `!` 5.
fn expr(source: &str, min: u32) -> Option<(Pred, &str)> {
    let (mut pred, mut source) = primary(source)?;
    'parse: loop {
        for (op, precedence) in [(">", 1), ("&&", 3), ("||", 2), ("==", 4), ("!=", 4)] {
            if let (Some(rest), true) = (source.strip_prefix(op), precedence >= min) {
                let (right, rest) = expr(rest.trim_start(), precedence + 1)?;
                pred = match (op, pred, right) {
                    (">", l, r) => Pred::Child(Box::new(l), Box::new(r)),
                    ("&&", l, r) => Pred::And(Box::new(l), Box::new(r)),
                    ("||", l, r) => Pred::Or(Box::new(l), Box::new(r)),
                    ("==", Pred::Id(l), Pred::Id(r)) => Pred::Eq(l, r),
                    ("!=", Pred::Id(_), Pred::Id(_)) => Pred::NotEq,
                    // Operands of `==` and `!=` must be identifiers.
                    _ => return None,
                };
                source = rest;
                continue 'parse;
            }
        }
        return Some((pred, source));
    }
}

fn primary(source: &str) -> Option<(Pred, &str)> {
    let id = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let vim_operator = |c: char| ">~<\"?".contains(c);
    let next = source.chars().next()?;
    if next == '(' {
        let (pred, rest) = expr(source[1..].trim_start(), 0)?;
        return Some((pred, rest.strip_prefix(')')?.trim_start()));
    }
    if next == '!' {
        let (pred, rest) = expr(source[1..].trim_start(), 5)?;
        return Some((Pred::Not(Box::new(pred)), rest));
    }
    let len = if id(next) {
        source
            .find(|c: char| !id(c) && !vim_operator(c))
            .unwrap_or(source.len())
    } else if vim_operator(next) {
        next.len_utf8()
    } else {
        return None;
    };
    Some((Pred::Id(source[..len].into()), source[len..].trim_start()))
}

/// The scopes a context keeps, by name, each with its values: only ever wider than Zed's predicate (as 6.25's `when`),
/// so a fact can't hide a bind Zed would run.
type Scopes = BTreeMap<String, BTreeSet<String>>;

fn scopes(pred: &Pred) -> Scopes {
    let one = |k: &str, v: &str| Scopes::from([(k.to_string(), BTreeSet::from([v.to_string()]))]);
    match pred {
        Pred::Id(k) => one(k, "true"),
        Pred::Not(inner) => match inner.as_ref() {
            Pred::Id(k) => one(k, "false"),
            _ => Scopes::new(),
        },
        // Vim's modes share nvim's fact name; Zed's own `mode` (the editor kind) is renamed so it never meets it.
        Pred::Eq(k, v) => match k.as_str() {
            "vim_mode" => one("mode", v),
            "mode" => one("editor_mode", v),
            _ => one(k, v),
        },
        Pred::NotEq => Scopes::new(),
        // The focused node is the child's.
        Pred::Child(_, child) => scopes(child),
        Pred::And(a, b) => {
            let (mut a, b) = (scopes(a), scopes(b));
            for (k, v) in b {
                match a.get_mut(&k) {
                    // Both sides name it: the values both allow; none means it can't hold, so no scope (wider).
                    Some(mine) => {
                        mine.retain(|x| v.contains(x));
                        if mine.is_empty() {
                            a.remove(&k);
                        }
                    }
                    None => {
                        a.insert(k, v);
                    }
                }
            }
            a
        }
        Pred::Or(a, b) => {
            let (a, b) = (scopes(a), scopes(b));
            a.into_iter()
                .filter_map(|(k, mut v)| {
                    v.extend(b.get(&k)?.iter().cloned());
                    Some((k, v))
                })
                .collect()
        }
    }
}

/// Scopes as a bind's `when`: a name or value that isn't a `Name` drops that scope.
fn scopes_of(scopes: &Scopes) -> BTreeMap<Name, Values> {
    scopes
        .iter()
        .filter_map(|(k, vs)| {
            let names: Option<Vec<Name>> = vs.iter().map(|v| v.parse().ok()).collect();
            Some((k.parse().ok()?, Values::try_from(names?).ok()?))
        })
        .collect()
}

/// `os=linux`, and the file's `extension` from a title in Zed's default format (`project — main.rs`: the last segment
/// after ` — `); no extension without the separator.
fn facts(title: Option<&str>) -> Facts {
    let mut facts = Facts::new();
    // Zed sets `os` in every context: `os == windows` binds never run here.
    if let (Ok(k), Ok(v)) = ("os".parse::<Name>(), "linux".parse::<Name>()) {
        facts.insert(k, v);
    }
    let file = title
        .and_then(|t| t.rsplit_once(" \u{2014} "))
        .map(|(_, f)| f);
    if let (Some(ext), Ok(name)) = (
        file.and_then(crate::context::title_extension),
        "extension".parse::<Name>(),
    ) {
        facts.insert(name, ext);
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/zed")
            .join(name)
    }

    /// The pinned files for `base`/`vim`, then `user` keymaps.
    fn sheet_of(base: &str, vim: bool, user: &[&str]) -> (Sheet, usize, usize, Vec<String>) {
        let mut keymaps: Vec<(&str, Value)> = load_order(base, vim)
            .into_iter()
            .map(|(n, t)| {
                (
                    n,
                    serde_json::from_str(&super::super::micro::strip_jsonc(t)).unwrap(),
                )
            })
            .collect();
        for u in user {
            keymaps.push((
                "keymap.json",
                serde_json::from_str(&super::super::micro::strip_jsonc(u)).unwrap(),
            ));
        }
        let mut notes = Vec::new();
        let (s, l, b) = build(&keymaps, &mut notes);
        (s, l, b, notes)
    }

    fn all(s: &Sheet) -> Vec<&Bind> {
        s.binds.values().flatten().collect()
    }

    /// The binds for `key` whose scopes print as `scopes` (`k=v,w k2=v`), in order.
    fn find<'a>(s: &'a Sheet, key: &str, scopes: &str) -> Vec<&'a str> {
        all(s)
            .into_iter()
            .filter(|b| b.key == key && tags(b) == scopes)
            .map(|b| b.desc.as_str())
            .collect()
    }

    fn tags(b: &Bind) -> String {
        b.when
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}={}",
                    v.iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn pinned_files() {
        for (text, name) in [(DEFAULT, "default"), (VIM, "vim"), (OVERRIDES, "overrides")]
            .into_iter()
            .chain(BASES.iter().filter_map(|(n, t)| Some(((*t)?, *n))))
        {
            let v: Value = serde_json::from_str(&super::super::micro::strip_jsonc(text)).unwrap();
            assert!(sections(&v).is_some(), "{name}");
            for (c, ..) in sections(&v).unwrap() {
                assert!(c.trim().is_empty() || parse(c).is_some(), "{name}: {c}");
            }
        }
        // Binds listed and keys left out, per base keymap, without and with vim mode.
        for (base, plain, with_vim, left) in [
            ("Zed", 917, 1795, 45),
            ("VSCode", 928, 1806, 45),
            ("JetBrains", 993, 1871, 46),
            ("SublimeText", 957, 1835, 45),
            ("Atom", 962, 1839, 45),
            ("TextMate", 917, 1795, 45),
            ("Emacs", 1014, 1892, 45),
            ("Cursor", 945, 1823, 45),
            ("None", 0, 0, 0),
        ] {
            for (vim, want) in [(false, plain), (true, with_vim)] {
                let (s, l, bad, notes) = sheet_of(base, vim, &[]);
                assert_eq!((all(&s).len(), l, bad), (want, left, 0), "{base} vim={vim}");
                assert!(notes.is_empty(), "{notes:?}");
                for b in all(&s) {
                    assert!(
                        b.key.parse::<keybind_find_sheet::Key>().is_ok(),
                        "{base}: {}",
                        b.key
                    );
                }
            }
        }
        let (s, ..) = sheet_of("Zed", false, &[]);
        assert_eq!((s.title.as_str(), s.provider.as_str()), ("Zed", "zed"));
        // Category by namespace; args as compact JSON.
        assert!(s.binds["workspace"]
            .iter()
            .any(|b| b.action == "workspace::Save"));
        assert_eq!(
            find(&s, "ctrl+shift+p", "Workspace=true"),
            ["command_palette::Toggle"]
        );
        assert!(all(&s)
            .iter()
            .any(|b| b.desc.starts_with(&format!("{} {{", b.action))));
    }

    #[test]
    fn load_order_and_user_binds() {
        assert!(load_order("None", true).is_empty());
        let names = |b, v| {
            load_order(b, v)
                .into_iter()
                .map(|(n, _)| n)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("Zed", false),
            ["default-linux.json", "specific-overrides.json"]
        );
        assert_eq!(
            names("TextMate", false),
            ["default-linux.json", "specific-overrides.json"]
        );
        assert_eq!(
            names("VSCode", true),
            [
                "default-linux.json",
                "base keymap",
                "vim.json",
                "specific-overrides.json"
            ]
        );
        // VSCode's file comes after the defaults: its bind replaces the default's for the same key and context.
        let (s, ..) = sheet_of("VSCode", false, &[]);
        assert_eq!(find(&s, "F5", "Workspace=true"), ["debugger::Start"]);
        let user = std::fs::read_to_string(fixture("keymap.json")).unwrap();
        let (s, _, _, notes) = sheet_of("Zed", false, &[&user]);
        assert!(notes.is_empty(), "{notes:?}");
        // Replaced, not added: one bind for the key in that context.
        assert_eq!(
            find(&s, "ctrl+shift+p", "Workspace=true"),
            ["file_finder::Toggle"]
        );
        assert_eq!(
            find(&s, "ctrl+alt+q", ""),
            ["workspace::SendKeystrokes \"ctrl-s ctrl-w\""]
        );
        assert_eq!(
            find(&s, "ctrl+b", "Editor=true extension=md"),
            ["markdown::OpenPreview"]
        );
        // `null` removed the default `ctrl-w` in `Workspace`.
        let (plain, ..) = sheet_of("Zed", false, &[]);
        assert!(!find(&plain, "ctrl+w", "Workspace=true").is_empty());
        assert!(find(&s, "ctrl+w", "Workspace=true").is_empty());
        // Context text compared with spaces removed.
        let spaced = r#"[{"context": "Editor&&extension==md", "bindings": {"ctrl-b": null}}]"#;
        let (s, ..) = sheet_of("Zed", false, &[&user, spaced]);
        assert!(find(&s, "ctrl+b", "Editor=true extension=md").is_empty());
        // helix_mode alone loads vim.json; the vim keymap's binds are tagged by mode.
        let mut notes = Vec::new();
        let helix: Value = serde_json::json!({"helix_mode": true});
        assert_eq!(settings_of(Some(&helix), &mut notes), ("Zed", true));
        let (s, ..) = sheet_of("Zed", true, &[]);
        assert!(all(&s)
            .iter()
            .any(|b| tags(b).split(' ').any(|t| t == "mode=normal")));
    }

    /// Temp dirs for `env`: a fresh HOME.
    fn home(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kbf-zed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_files() {
        let h = home("files");
        let env_of = |xdg: Option<String>| {
            let h = h.clone();
            move |k: &str| match k {
                "HOME" => Some(h.display().to_string()),
                "XDG_CONFIG_HOME" => xdg.clone(),
                _ => None,
            }
        };
        // The pinned defaults' left-out keys are always one line; the rest are the files'.
        let others = |notes: Vec<String>| -> Vec<String> {
            assert_eq!(
                notes
                    .iter()
                    .filter(|n| n.starts_with("leaving out 45 key(s)"))
                    .count(),
                1,
                "{notes:?}"
            );
            notes
                .into_iter()
                .filter(|n| !n.starts_with("leaving out "))
                .collect()
        };
        // No files at all: the defaults, silently.
        let (s, facts, notes) = read(None, &env_of(None));
        assert_eq!(all(&s).len(), 917);
        assert!(!facts.contains_key("extension") && others(notes).is_empty());
        // Unset and empty XDG_CONFIG_HOME: `~/.config/zed`; set: that.
        let dot = h.join(".config/zed");
        std::fs::create_dir_all(&dot).unwrap();
        std::fs::copy(fixture("settings.json"), dot.join("settings.json")).unwrap();
        for xdg in [None, Some(String::new()), Some("relative".into())] {
            assert_eq!(
                config_dir(&env_of(xdg.clone())),
                Some(dot.clone()),
                "{xdg:?}"
            );
            assert_eq!(all(&read(None, &env_of(xdg)).0).len(), 928);
        }
        let xdg = h.join("xdg");
        assert_eq!(
            config_dir(&env_of(Some(xdg.display().to_string()))),
            Some(xdg.join("zed"))
        );
        // The Flatpak's only when the host directory is absent.
        let flatpak = h.join(".var/app/dev.zed.Zed/config/zed");
        std::fs::create_dir_all(&flatpak).unwrap();
        assert_eq!(config_dir(&env_of(None)), Some(dot.clone()));
        std::fs::remove_dir_all(&dot).unwrap();
        assert_eq!(config_dir(&env_of(None)), Some(flatpak.clone()));
        // A keymap.json that isn't JSON: the defaults, one note. One of the wrong shape: the same.
        std::fs::write(flatpak.join("keymap.json"), "not json").unwrap();
        let (s, _, notes) = read(None, &env_of(None));
        let notes = others(notes);
        assert_eq!((all(&s).len(), notes.len()), (917, 1), "{notes:?}");
        assert!(notes[0].ends_with("; leaving it out"), "{notes:?}");
        std::fs::write(flatpak.join("keymap.json"), r#"{"bindings": {}}"#).unwrap();
        let (s, _, notes) = read(None, &env_of(None));
        let notes = others(notes);
        assert_eq!((all(&s).len(), notes.len()), (917, 1), "{notes:?}");
        assert!(
            notes[0].starts_with("keymap.json isn't a list"),
            "{notes:?}"
        );
        // An unknown base keymap: Zed's, one note.
        std::fs::remove_file(flatpak.join("keymap.json")).unwrap();
        std::fs::write(flatpak.join("settings.json"), r#"{"base_keymap": "Vim"}"#).unwrap();
        let (s, _, notes) = read(None, &env_of(None));
        let notes = others(notes);
        assert_eq!((all(&s).len(), notes.len()), (917, 1), "{notes:?}");
        assert!(notes[0].contains("base_keymap \"Vim\""), "{notes:?}");
        // `None`: no defaults, so nothing left out and no line; the user's keymap still loads.
        std::fs::write(flatpak.join("settings.json"), r#"{"base_keymap": "None"}"#).unwrap();
        let (s, _, notes) = read(None, &env_of(None));
        assert!(s.binds.is_empty() && notes.is_empty(), "{notes:?}");
        std::fs::copy(fixture("keymap.json"), flatpak.join("keymap.json")).unwrap();
        let (s, _, notes) = read(None, &env_of(None));
        assert_eq!(
            find(&s, "ctrl+shift+p", "Workspace=true"),
            ["file_finder::Toggle"]
        );
        assert!(notes.is_empty(), "{notes:?}");
        // A settings.json that doesn't parse, isn't an object, or has a base_keymap that isn't a string: Zed's
        // defaults, one note each.
        std::fs::remove_file(flatpak.join("keymap.json")).unwrap();
        for (text, note) in [
            ("{base_keymap", "settings.json: "),
            ("[1]", "settings.json isn't an object"),
            (r#"{"base_keymap": 5}"#, "settings.json's base_keymap 5 "),
        ] {
            std::fs::write(flatpak.join("settings.json"), text).unwrap();
            let (s, _, notes) = read(None, &env_of(None));
            let notes = others(notes);
            assert_eq!((all(&s).len(), notes.len()), (917, 1), "{text}: {notes:?}");
            assert!(notes[0].contains(note), "{text}: {notes:?}");
        }
        // An action of the wrong shape: that binding left out (nothing removed), one note.
        std::fs::write(flatpak.join("settings.json"), "{}").unwrap();
        std::fs::write(
            flatpak.join("keymap.json"),
            r#"[{"context": "Workspace", "bindings": {"ctrl-w": []}}]"#,
        )
        .unwrap();
        let (s, _, notes) = read(None, &env_of(None));
        assert!(!find(&s, "ctrl+w", "Workspace=true").is_empty());
        assert!(others(notes)[0].starts_with("keymap.json: leaving out 1 binding(s)"));
        // Only bad contexts, nothing left out: the line names only them.
        std::fs::write(flatpak.join("settings.json"), r#"{"base_keymap": "None"}"#).unwrap();
        std::fs::write(
            flatpak.join("keymap.json"),
            r#"[{"context": "Editor &&", "bindings": {"q": "x::Y"}}]"#,
        )
        .unwrap();
        let (_, _, notes) = read(None, &env_of(None));
        assert_eq!(
            notes,
            ["1 bind(s) whose context didn't parse are shown untagged"]
        );
        std::fs::write(flatpak.join("settings.json"), "{}").unwrap();
        let (_, _, notes) = read(None, &env_of(None));
        assert!(
            notes[0].starts_with("leaving out 45 key(s)")
                && notes[0].ends_with("; 1 bind(s) whose context didn't parse are shown untagged"),
            "{notes:?}"
        );
        // A keymap.json that can't be read (a directory): one note.
        std::fs::remove_file(flatpak.join("keymap.json")).unwrap();
        std::fs::create_dir(flatpak.join("keymap.json")).unwrap();
        let (_, _, notes) = read(None, &env_of(None));
        assert_eq!(others(notes).len(), 1);
        std::fs::remove_dir(flatpak.join("keymap.json")).unwrap();
        // An empty or comment-only keymap.json: nothing, silently. A section without `bindings` is valid.
        std::fs::write(flatpak.join("keymap.json"), "// nothing yet\n").unwrap();
        assert!(others(read(None, &env_of(None)).2).is_empty());
        std::fs::write(
            flatpak.join("keymap.json"),
            r#"[{"context": "Editor", "unbind": {"ctrl-w": "x::Y"}}, {"bindings": {"ctrl-alt-q": "x::Y"}}]"#,
        )
        .unwrap();
        let (s, _, notes) = read(None, &env_of(None));
        assert!(others(notes).is_empty());
        assert_eq!(find(&s, "ctrl+alt+q", ""), ["x::Y"]);
        std::fs::remove_file(flatpak.join("keymap.json")).unwrap();
        // XDG_CONFIG_HOME set: its zed/ is read.
        let xdg_zed = h.join("xdg/zed");
        std::fs::create_dir_all(&xdg_zed).unwrap();
        std::fs::copy(fixture("settings.json"), xdg_zed.join("settings.json")).unwrap();
        let xdg_env = env_of(Some(h.join("xdg").display().to_string()));
        assert_eq!(all(&read(None, &xdg_env).0).len(), 928);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn contexts() {
        let s = |c: &str| {
            parse(c).map(|p| {
                scopes(&p)
                    .iter()
                    .map(|(k, v)| {
                        format!("{k}={}", v.iter().cloned().collect::<Vec<_>>().join(","))
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
        };
        for (context, want) in [
            ("Editor", "Editor=true"),
            ("!Terminal", "Terminal=false"),
            ("Editor && mode == full", "Editor=true editor_mode=full"),
            ("Editor && extension == md", "Editor=true extension=md"),
            (
                "vim_mode == normal || vim_mode == visual || vim_mode == operator",
                "mode=normal,operator,visual",
            ),
            (
                "(vim_mode == normal || vim_mode == helix_normal) && !menu",
                "menu=false mode=helix_normal,normal",
            ),
            ("BufferSearchBar && !in_replace > Editor", "Editor=true"),
            ("Picker || menu", ""),
            ("vim_operator == g~", "vim_operator=g~"),
            ("a != b", ""),
            ("Editor && a != b", "Editor=true"),
            ("!(a && b)", ""),
            ("a && a == x", ""),
        ] {
            assert_eq!(s(context).as_deref(), Some(want), "{context}");
        }
        // `!` binds tightest: `!a == b` compares a negation, which Zed refuses.
        for bad in [
            "Editor &&",
            "(Editor",
            "a == !b",
            "!a == b",
            "Editor ^ x",
            "a b",
        ] {
            assert_eq!(s(bad), None, "{bad}");
        }
        // A context that doesn't parse: listed untagged, counted.
        let broken = r#"[{"context": "Editor &&", "bindings": {"ctrl-b": "x::Y"}}]"#;
        let (s, _, bad, _) = sheet_of("None", false, &[broken]);
        assert_eq!((find(&s, "ctrl+b", ""), bad), (vec!["x::Y"], 1));
        // A value that isn't a `Name` (non-ASCII) drops that scope; `vim_operator == "` is a `Name` and kept.
        let odd = r#"[{"context": "Editor && lang == é", "bindings": {"q": "x::Y"}},
                      {"context": "Editor && vim_operator == \"", "bindings": {"w": "x::Y"}}]"#;
        let (s, ..) = sheet_of("None", false, &[odd]);
        assert_eq!(find(&s, "q", "Editor=true"), ["x::Y"]);
        assert_eq!(find(&s, "w", "Editor=true vim_operator=\""), ["x::Y"]);
        // Unbinds: by key and action name, in that context or (with none) any; `zed::NoAction` is `null`; a
        // one-element action array isn't Zed's shape.
        let base = r#"[{"context": "Editor", "bindings": {"ctrl-q": "a::A", "w": "b::B", "e": "c::C"}},
                       {"context": "Pane", "bindings": {"ctrl-q": "a::A", "r": "d::D", "t": "f::F"}},
                       {"bindings": {"y": "g::G"}}]"#;
        let user = r#"[{"context": "Editor", "unbind": {"CTRL-q": "a::A", "w": "other::X"}},
                       {"bindings": {"e": ["zed::Unbind", "c::C"]}},
                       {"context": "Pane", "bindings": {"r": "zed::NoAction", "t": ["zed::NoAction", {}]}},
                       {"context": "  ", "bindings": {"y": null}}]"#;
        let (s, _, bad, notes) = sheet_of("None", false, &[base, user]);
        assert!(notes.is_empty(), "{notes:?}");
        // The unbind in `Editor` leaves `Pane`'s `ctrl-q` (and `CTRL-q` is the same key).
        assert!(find(&s, "ctrl+q", "Editor=true").is_empty());
        assert_eq!(find(&s, "ctrl+q", "Pane=true"), ["a::A"]);
        // By action name: `w` is bound to another action, kept.
        assert_eq!(find(&s, "w", "Editor=true"), ["b::B"]);
        // With no context, any context.
        assert!(find(&s, "e", "Editor=true").is_empty());
        // `zed::NoAction`, as a string or an array, is `null`.
        assert!(find(&s, "r", "Pane=true").is_empty() && find(&s, "t", "Pane=true").is_empty());
        // A context of spaces doesn't parse (counted) and isn't the no-context `y`.
        assert_eq!((find(&s, "y", ""), bad), (vec!["g::G"], 0));
        // An action not of Zed's shape: that binding left out (nothing removed), the rest kept, one note.
        for bad in [
            r#"["a::A"]"#,
            r#"["a::A", 1, 2]"#,
            r#"[5, {}]"#,
            r#"["zed::Unbind", ["b::B", {}]]"#,
            r#""zed::Unbind""#,
            "true",
        ] {
            let plain = r#"[{"bindings": {"w": "b::B", "q": "c::C"}}]"#;
            let user = format!(r#"[{{"bindings": {{"w": {bad}, "q": null, "e": "d::D"}}}}]"#);
            let (s, _, _, notes) = sheet_of("None", false, &[plain, &user]);
            assert_eq!(notes, ["keymap.json: leaving out 1 binding(s) whose action isn't a name, [name, args] or null"], "{bad}");
            assert_eq!(find(&s, "w", ""), ["b::B"], "{bad}");
            assert!(find(&s, "q", "").is_empty(), "{bad}");
            assert_eq!(find(&s, "e", ""), ["d::D"], "{bad}");
        }
        let (_, _, _, notes) = sheet_of("None", false, &[r#"[{"unbind": {"q": null}}]"#]);
        assert_eq!(notes.len(), 1);
        // Not Zed's shape at all: the whole file refused.
        for bad in [r#"[{"bindings": []}]"#, "[1]", r#"[{"context": 5}]"#] {
            let (_, _, _, notes) = sheet_of("None", false, &[bad]);
            assert!(notes[0].starts_with("keymap.json isn't a list"), "{bad}");
        }
        // An action without `::` is filed under `zed`.
        let bare = r#"[{"bindings": {"q": "NoNamespace"}}]"#;
        assert_eq!(
            sheet_of("None", false, &[bare]).0.binds["zed"][0].action,
            "NoNamespace"
        );
    }

    #[test]
    fn title_facts() {
        let ext = |t: &str| facts(Some(t)).get("extension").map(|e| e.to_string());
        assert_eq!(ext("project — main.rs").as_deref(), Some("rs"));
        assert_eq!(ext("project"), None);
        assert_eq!(ext("a — b — notes.md").as_deref(), Some("md"));
        assert_eq!(ext("project - main.rs"), None);
        // `os` is always linux: `os == windows` binds are hidden.
        let none = facts(None);
        assert_eq!(none.len(), 1);
        assert_eq!(
            none.get("os").map(|o| o.to_string()).as_deref(),
            Some("linux")
        );
        let (s, ..) = sheet_of("Zed", true, &[]);
        assert!(all(&s)
            .iter()
            .any(|b| tags(b).split(' ').any(|t| t == "os=windows")));
        // The title reaches the facts through `read`.
        let (_, facts, _) = read(Some("keybind-find — zed.rs"), &|_| None);
        assert_eq!(
            facts.get("extension").map(|e| e.to_string()).as_deref(),
            Some("rs")
        );
    }

    #[test]
    fn keys() {
        for (zed, want) in [
            ("ctrl-shift-p", "ctrl+shift+p"),
            ("secondary-s", "ctrl+s"),
            ("cmd-k cmd-s", "super+k super+s"),
            ("win-a", "super+a"),
            ("Ctrl-Shift-P", "ctrl+shift+p"),
            ("CTRL-p", "ctrl+p"),
            ("Enter", "Return"),
            ("F5", "F5"),
            ("Shift-Tab", "shift+Tab"),
            ("ctrl-P", "ctrl+shift+p"),
            ("super-a", "super+a"),
            ("shift-G", "shift+g"),
            ("ctrl--", "ctrl+minus"),
            ("-", "minus"),
            ("shift-g", "shift+g"),
            ("G", "shift+g"),
            ("pageup", "Prior"),
            ("pagedown", "Next"),
            ("f13", "F13"),
            ("f24", "F24"),
            ("enter", "Return"),
            ("escape", "Escape"),
            ("backspace", "BackSpace"),
            ("shift-tab", "shift+Tab"),
            ("alt-delete", "alt+Delete"),
            ("space", "space"),
            ("g g", "g g"),
            ("ctrl-k ctrl-[", "ctrl+k ctrl+bracketleft"),
        ] {
            assert_eq!(notation(zed).as_deref(), Some(want), "{zed}");
        }
        for zed in [
            "shift-(",
            "ctrl-shift-0",
            "fn-f1",
            "shift shift",
            "shift",
            "f25",
            "f01",
            "f+1",
            "copy",
            "ctrl-",
            "",
        ] {
            assert_eq!(notation(zed), None, "{zed}");
        }
    }
}
