//! VS Code, Insiders, VSCodium and Code - OSS: the default keybindings pinned for one release (`data/vscode/`), the
//! installed extensions' keybindings, then the user's `keybindings.json`, as VS Code ranks them. Each bind's `when`
//! clause becomes scopes, and the file in the window title gives the `extension` and `filetype` facts. Files are only
//! read; commands are shown, never run.

use super::micro::strip_jsonc;
use super::nvim::single;
use crate::context::{title_extension, Context};
use crate::scope::Facts;
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::Context as _;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// VS Code 1.143.1's Linux defaults. One version, no probe (running VS Code's CLI on every press is too slow).
// ponytail: one pinned version; a second goes through `pinned::select` by `code --version`.
const PINNED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/vscode/1.143.1.json"
));

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// One product of the family, from its `product.json`.
struct Variant {
    /// `nameLong`: the window title's `appName`.
    name_long: &'static str,
    /// `nameShort`: the user data directory under `~/.config`.
    name_short: &'static str,
    /// `dataFolderName`: `~/<it>/extensions`.
    data_folder: &'static str,
    /// Window classes (X11) and app ids (Wayland), compared ignoring case.
    classes: &'static [&'static str],
    /// The Flatpak's app id and the directory its wrapper passes as `--extensions-dir`, under its data dir.
    flatpak: Option<(&'static str, &'static str)>,
}

/// Classes: Electron names a window after `app.setDesktopName(<linuxDesktopName>.desktop)` when that desktop file is
/// installed (`src/mainImpl.ts`), after `<snap>_<applicationName>` under snap, else after the product (`Code`).
const VARIANTS: [Variant; 4] = [
    Variant {
        name_long: "Visual Studio Code",
        name_short: "Code",
        data_folder: ".vscode",
        classes: &["code", "code_code", "com.visualstudio.code"],
        flatpak: Some(("com.visualstudio.code", "vscode/extensions")),
    },
    Variant {
        name_long: "Visual Studio Code - Insiders",
        name_short: "Code - Insiders",
        data_folder: ".vscode-insiders",
        classes: &[
            "code-insiders",
            "code - insiders",
            "code-insiders_code-insiders",
        ],
        flatpak: None,
    },
    Variant {
        name_long: "VSCodium",
        name_short: "VSCodium",
        data_folder: ".vscode-oss",
        classes: &["vscodium", "codium", "codium_codium", "com.vscodium.codium"],
        flatpak: Some(("com.vscodium.codium", "codium/extensions")),
    },
    Variant {
        name_long: "Code - OSS",
        name_short: "Code - OSS",
        data_folder: ".vscode-oss",
        classes: &["code-oss", "code - oss", "com.visualstudio.codeoss"],
        flatpak: None,
    },
];

/// Is `class` a VS Code family window's?
pub(crate) fn is_vscode(class: &str) -> bool {
    VARIANTS
        .iter()
        .any(|v| v.classes.iter().any(|c| c.eq_ignore_ascii_case(class)))
}

/// One keybinding rule, as `keybindings.json` and `contributes.keybindings` write it.
#[derive(Deserialize, Clone)]
struct Rule {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    linux: Option<String>,
    #[serde(default)]
    command: String,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

/// A rule placed in the list, with the category it shows under.
struct Entry {
    rule: Rule,
    category: Option<String>,
}

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, facts, notes) =
        sheet_with(PINNED, ctx.title.as_deref(), ctx.class.as_deref(), &env)?;
    if !facts.is_empty() {
        crate::context::set_facts(facts);
    }
    for note in notes {
        eprintln!("keybind-find: vscode: {note}");
    }
    Ok(sheet)
}

/// The sheet, the facts from the title, and the lines for stderr, for a window titled `title` of class `class`.
fn sheet_with(
    pinned: &str,
    title: Option<&str>,
    class: Option<&str>,
    env: Env,
) -> anyhow::Result<(Sheet, Facts, Vec<String>)> {
    let defaults: Vec<Rule> = serde_json::from_str(&strip_jsonc(pinned))
        .context("reading the pinned VS Code keybindings")?;
    let mut notes = Vec::new();
    let variant = variant(title, class, env);
    let mut entries: Vec<Entry> = defaults
        .into_iter()
        .map(|rule| Entry {
            rule,
            category: None,
        })
        .collect();
    let mut broken = 0;
    if let Some(v) = variant {
        entries.extend(extension_rules(v, env, &mut broken));
        if let Some(user) = user_rules(v, env, &mut notes) {
            apply_user(&mut entries, user);
        }
    }
    let name = variant.map_or(VARIANTS[0].name_long, |v| v.name_long);
    let (sheet, keys_left, whens_left) = build(name, &entries);
    if keys_left + whens_left + broken > 0 {
        notes.push(format!(
            "leaving out {keys_left} key(s) keybind-find can't name; {whens_left} `when` clause(s) not understood \
             (shown everywhere); {broken} extension package.json file(s) not read"
        ));
    }
    Ok((sheet, title.map(facts).unwrap_or_default(), notes))
}

/// The variant: the one whose `appName` ends the title, else the one the class names, else the first whose config
/// directory exists.
fn variant(title: Option<&str>, class: Option<&str>, env: Env) -> Option<&'static Variant> {
    title
        .and_then(by_title)
        .or_else(|| {
            class.and_then(|c| {
                VARIANTS
                    .iter()
                    .find(|v| v.classes.iter().any(|k| k.eq_ignore_ascii_case(c)))
            })
        })
        .or_else(|| VARIANTS.iter().find(|v| user_dir(v, env).is_some()))
}

fn by_title(title: &str) -> Option<&'static Variant> {
    VARIANTS
        .iter()
        .find(|v| title == v.name_long || title.ends_with(&format!(" - {}", v.name_long)))
}

/// The `extension` and `filetype` facts from a default `window.title` (`● main.rs - project - Visual Studio Code`):
/// its first ` - ` part, after the dirty marker, as a file name. None for a title without a family `appName`.
fn facts(title: &str) -> Facts {
    let mut facts = Facts::new();
    let Some(v) = by_title(title) else {
        return facts;
    };
    let rest = title.strip_suffix(v.name_long).unwrap_or(title);
    let rest = rest.strip_prefix('\u{25cf}').map_or(rest, str::trim_start);
    let file = rest.split(" - ").next().unwrap_or_default();
    let Some(extension) = title_extension(file) else {
        return facts;
    };
    let language = language(extension.as_str()).and_then(|l| l.parse::<Name>().ok());
    for (fact, value) in [("filetype", language), ("extension", Some(extension))] {
        if let (Ok(fact), Some(value)) = (fact.parse::<Name>(), value) {
            facts.insert(fact, value);
        }
    }
    facts
}

/// VS Code's language id for a file extension: the built-in extensions' `contributes.languages[].extensions` at
/// 1.143.1, ignoring case.
// ponytail: the common languages only; add a row when one is missing.
fn language(extension: &str) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("bash", "shellscript"),
        ("bat", "bat"),
        ("c", "c"),
        ("cc", "cpp"),
        ("cjs", "javascript"),
        ("clj", "clojure"),
        ("coffee", "coffeescript"),
        ("cpp", "cpp"),
        ("cs", "csharp"),
        ("css", "css"),
        ("cts", "typescript"),
        ("cxx", "cpp"),
        ("dart", "dart"),
        ("diff", "diff"),
        ("fs", "fsharp"),
        ("go", "go"),
        ("groovy", "groovy"),
        ("h", "cpp"),
        ("hh", "cpp"),
        ("hpp", "cpp"),
        ("htm", "html"),
        ("html", "html"),
        ("ini", "ini"),
        ("java", "java"),
        ("jl", "julia"),
        ("js", "javascript"),
        ("json", "json"),
        ("jsonc", "jsonc"),
        ("jsx", "javascriptreact"),
        ("less", "less"),
        ("lua", "lua"),
        ("markdown", "markdown"),
        ("md", "markdown"),
        ("mjs", "javascript"),
        ("mk", "makefile"),
        ("mts", "typescript"),
        ("php", "php"),
        ("pl", "perl"),
        ("ps1", "powershell"),
        ("py", "python"),
        ("r", "r"),
        ("rb", "ruby"),
        ("rs", "rust"),
        ("rst", "restructuredtext"),
        ("scss", "scss"),
        ("sh", "shellscript"),
        ("sql", "sql"),
        ("swift", "swift"),
        ("tex", "latex"),
        ("ts", "typescript"),
        ("tsx", "typescriptreact"),
        ("vb", "vb"),
        ("xml", "xml"),
        ("yaml", "yaml"),
        ("yml", "yaml"),
        ("zsh", "shellscript"),
    ];
    TABLE
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(extension))
        .map(|(_, l)| *l)
}

/// The variant's `User` directory: `$XDG_CONFIG_HOME/<nameShort>/User` (else `~/.config`), else the Flatpak's, when
/// it exists.
// ponytail: the default profile only, and no `--user-data-dir` or `VSCODE_APPDATA` (no pid or command line here).
fn user_dir(v: &Variant, env: Env) -> Option<PathBuf> {
    let set = |k| env(k).filter(|s| !s.is_empty()).map(PathBuf::from);
    let home = set("HOME");
    let config = set("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config")));
    let host = config.map(|c| c.join(v.name_short).join("User"));
    let flatpak = v.flatpak.zip(home).map(|((id, _), h)| {
        h.join(".var/app")
            .join(id)
            .join("config")
            .join(v.name_short)
            .join("User")
    });
    host.into_iter().chain(flatpak).find(|d| d.is_dir())
}

/// The user's `keybindings.json`: none when missing (silently) or when it isn't a JSON array (with a note).
fn user_rules(v: &Variant, env: Env, notes: &mut Vec<String>) -> Option<Vec<Rule>> {
    let file = user_dir(v, env)?.join("keybindings.json");
    let text = match std::fs::read_to_string(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            notes.push(format!("{}: {e}; listing the defaults", file.display()));
            return None;
        }
        Ok(t) => t,
    };
    match serde_json::from_str::<Vec<Value>>(&strip_jsonc(&text)) {
        // An entry that isn't a rule (not an object) is skipped, as VS Code skips it.
        Ok(values) => Some(
            values
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect(),
        ),
        Err(e) => {
            notes.push(format!(
                "{}: not a JSON array of keybindings ({e}); listing the defaults",
                file.display()
            ));
            None
        }
    }
}

/// The installed extensions' `contributes.keybindings`, each under the extension's `displayName` (else `name`). Only
/// the folders `extensions.json` lists (others are old versions left behind); every folder when there is none. A
/// `package.json` that doesn't parse is counted in `broken`.
// ponytail: disabled extensions are listed too: their state is in a SQLite database (`state.vscdb`).
fn extension_rules(v: &Variant, env: Env, broken: &mut usize) -> Vec<Entry> {
    let Some(dir) = extensions_dir(v, env) else {
        return Vec::new();
    };
    let listed: Option<Vec<String>> = std::fs::read_to_string(dir.join("extensions.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .map(|list| list.iter().filter_map(folder).collect());
    let mut folders: Vec<String> = match listed {
        Some(listed) => listed,
        None => std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
    };
    folders.sort();
    let mut entries = Vec::new();
    for folder in folders {
        let Ok(text) = std::fs::read_to_string(dir.join(&folder).join("package.json")) else {
            continue;
        };
        let Ok(package) = serde_json::from_str::<Value>(&text) else {
            *broken += 1;
            continue;
        };
        let category = ["displayName", "name"]
            .iter()
            .find_map(|k| package[k].as_str().filter(|s| !s.is_empty()))
            .unwrap_or(&folder)
            .to_string();
        let rules = match &package["contributes"]["keybindings"] {
            Value::Array(list) => list.clone(),
            one @ Value::Object(_) => vec![one.clone()],
            _ => Vec::new(),
        };
        for rule in rules
            .into_iter()
            .filter_map(|r| serde_json::from_value::<Rule>(r).ok())
        {
            entries.push(Entry {
                rule,
                category: Some(category.clone()),
            });
        }
    }
    entries
}

/// The folder an `extensions.json` entry names: `relativeLocation`, or the old format's `location` path's last part.
fn folder(entry: &Value) -> Option<String> {
    if let Some(r) = entry["relativeLocation"].as_str().filter(|r| !r.is_empty()) {
        return Some(r.to_string());
    }
    let location = entry["location"]["path"]
        .as_str()
        .or_else(|| entry["location"].as_str())?;
    Path::new(location)
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

/// `~/<dataFolderName>/extensions`, else the Flatpak's `--extensions-dir`, when it exists.
fn extensions_dir(v: &Variant, env: Env) -> Option<PathBuf> {
    let home = env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)?;
    let host = home.join(v.data_folder).join("extensions");
    let flatpak = v
        .flatpak
        .map(|(id, dir)| home.join(".var/app").join(id).join("data").join(dir));
    [Some(host), flatpak]
        .into_iter()
        .flatten()
        .find(|d| d.is_dir())
}

/// The user's rules laid over `entries`: a `-command` removes the earlier rules for that command whose key (when
/// given) and `when` text (when given; spaces ignored) match; an empty command is skipped; a rule with the same key and
/// `when` as an earlier one replaces it; the rest are appended.
// ponytail: `when` compared as text, not by implication as VS Code's `whenIsEntirelyIncluded` does: a removal whose
// `when` is narrower than the default's but not equal leaves the default listed.
fn apply_user(entries: &mut Vec<Entry>, user: Vec<Rule>) {
    let when = |w: &Option<String>| w.as_deref().map(|w| w.replace(' ', "")).unwrap_or_default();
    for rule in user {
        if rule.command.is_empty() {
            continue;
        }
        if let Some(command) = rule.command.strip_prefix('-') {
            entries.retain(|e| {
                !(e.rule.command == command
                    && rule
                        .key
                        .as_ref()
                        .is_none_or(|k| same_key(k, key_of(&e.rule)))
                    && rule
                        .when
                        .as_ref()
                        .is_none_or(|_| when(&rule.when) == when(&e.rule.when)))
            });
            continue;
        }
        let place = entries.iter().position(|e| {
            rule.key.is_some()
                && same_key(rule.key.as_deref().unwrap_or_default(), key_of(&e.rule))
                && when(&rule.when) == when(&e.rule.when)
        });
        let entry = Entry {
            rule,
            category: None,
        };
        match place {
            Some(i) => entries[i] = entry,
            None => entries.push(entry),
        }
    }
}

/// A rule's key on Linux: `linux` when given, else `key`.
fn key_of(rule: &Rule) -> &str {
    rule.linux
        .as_deref()
        .or(rule.key.as_deref())
        .unwrap_or_default()
}

/// Two keys alike: the same in key notation, else the same text ignoring case.
fn same_key(a: &str, b: &str) -> bool {
    match (notation(a), notation(b)) {
        (Some(a), Some(b)) => a == b,
        _ => a.trim().eq_ignore_ascii_case(b.trim()),
    }
}

/// The sheet: each rule with a command, its keys in notation, its `when` as scopes. Also the keys with no
/// translation, and the `when` clauses that didn't parse.
fn build(name: &str, entries: &[Entry]) -> (Sheet, usize, usize) {
    let mut sheet = Sheet {
        title: name.into(),
        provider: "vscode".into(),
        binds: Default::default(),
    };
    let (mut keys_left, mut whens_left) = (0, 0);
    for Entry { rule, category } in entries {
        if rule.command.is_empty() || rule.command.starts_with('-') {
            continue;
        }
        let Some(key) = notation(key_of(rule)) else {
            keys_left += 1;
            continue;
        };
        let when = match rule
            .when
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
        {
            None => BTreeMap::new(),
            Some(w) => match parse_when(w) {
                Some(expr) => scopes_of(&scopes(&expr)),
                None => {
                    whens_left += 1;
                    BTreeMap::new()
                }
            },
        };
        let category = category.clone().unwrap_or_else(|| {
            rule.command
                .split_once('.')
                .map_or("general", |(c, _)| c)
                .to_string()
        });
        let desc = match &rule.args {
            Some(args) => format!("{} {args}", rule.command),
            None => rule.command.clone(),
        };
        sheet.binds.entry(category).or_default().push(Bind {
            key,
            desc,
            action: rule.command.clone(),
            when,
        });
    }
    (sheet, keys_left, whens_left)
}

/// A VS Code key (`ctrl+k ctrl+s`, `shift+[KeyA]`) in key notation; None when a part has no translation.
fn notation(key: &str) -> Option<String> {
    let chords: Option<Vec<String>> = key.split_whitespace().map(chord_of).collect();
    chords.filter(|c| !c.is_empty()).map(|c| c.join(" "))
}

fn chord_of(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    // The key is the part after the last `+`, unless the key is `+` itself.
    let (mods, key) = if lower == "+" {
        ("", "+")
    } else if let Some(m) = lower.strip_suffix("++") {
        (m, "+")
    } else {
        lower.rsplit_once('+').unwrap_or(("", lower.as_str()))
    };
    let mods = mods
        .split('+')
        .filter(|m| !m.is_empty())
        .map(|m| match m {
            "ctrl" => Some("ctrl"),
            "shift" => Some("shift"),
            "alt" => Some("alt"),
            "meta" | "win" | "super" => Some("super"),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(chord(&mods, &keysym(key)?))
}

/// A VS Code key name (lower case) as a keysym.
fn keysym(key: &str) -> Option<String> {
    if let Some(code) = key.strip_prefix('[').and_then(|k| k.strip_suffix(']')) {
        // Scan codes: only the letters and digits have a sure name.
        let c = code
            .strip_prefix("key")
            .or_else(|| code.strip_prefix("digit"))?;
        let mut chars = c.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_string()),
            _ => None,
        };
    }
    if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return ((1..=19).contains(&n) && key == format!("f{n}")).then(|| format!("F{n}"));
    }
    if let Some(d) = key.strip_prefix("numpad") {
        if let Some(n) = d.parse::<u8>().ok().filter(|n| *n <= 9 && d.len() == 1) {
            return Some(format!("KP_{n}"));
        }
    }
    let named = match key {
        "enter" => "Return",
        "escape" => "Escape",
        "backspace" => "BackSpace",
        "tab" => "Tab",
        "space" => "space",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Prior",
        "pagedown" => "Next",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "capslock" => "Caps_Lock",
        "numlock" => "Num_Lock",
        "scrolllock" => "Scroll_Lock",
        "pausebreak" => "Pause",
        "contextmenu" => "Menu",
        "printscreen" => "Print",
        "numpad_add" => "KP_Add",
        "numpad_subtract" => "KP_Subtract",
        "numpad_multiply" => "KP_Multiply",
        "numpad_divide" => "KP_Divide",
        "numpad_decimal" => "KP_Decimal",
        "numpad_separator" => "KP_Separator",
        _ => {
            let mut chars = key.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) => single(c).map(str::to_string),
                _ => None,
            };
        }
    };
    Some(named.to_string())
}

/// A parsed `when` clause.
#[derive(Debug, PartialEq)]
enum When {
    Key(String),
    Not(Box<When>),
    Eq(String, String),
    /// `!=`, `=~`, `in`, `not in`, `<`, `<=`, `>`, `>=`: no scope.
    Other,
    And(Box<When>, Box<When>),
    Or(Box<When>, Box<When>),
}

/// Parses a `when` clause (`&&` binds tighter than `||`); None when it doesn't parse.
fn parse_when(text: &str) -> Option<When> {
    let mut p = Parser { s: text, i: 0 };
    let expr = p.or()?;
    p.ws();
    (p.i == p.s.len()).then_some(expr)
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.i..]
    }

    fn ws(&mut self) {
        let skipped = self.rest().len() - self.rest().trim_start().len();
        self.i += skipped;
    }

    fn eat(&mut self, token: &str) -> bool {
        self.ws();
        let found = self.rest().starts_with(token);
        if found {
            self.i += token.len();
        }
        found
    }

    fn or(&mut self) -> Option<When> {
        let mut left = self.and()?;
        while self.eat("||") {
            left = When::Or(Box::new(left), Box::new(self.and()?));
        }
        Some(left)
    }

    fn and(&mut self) -> Option<When> {
        let mut left = self.unary()?;
        while self.eat("&&") {
            left = When::And(Box::new(left), Box::new(self.unary()?));
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<When> {
        self.ws();
        if self.rest().starts_with('!') && !self.rest().starts_with("!=") {
            self.i += 1;
            return Some(When::Not(Box::new(self.unary()?)));
        }
        if self.eat("(") {
            let inner = self.or()?;
            return self.eat(")").then_some(inner);
        }
        let key = self.word()?;
        for op in ["==", "!=", "=~", "<=", ">=", "<", ">", "not in", "in "] {
            let save = self.i;
            if self.eat(op) {
                if op == "=~" {
                    self.regex()?;
                    return Some(When::Other);
                }
                let value = self.value()?;
                return Some(if op == "==" {
                    When::Eq(key, value)
                } else {
                    When::Other
                });
            }
            self.i = save;
        }
        Some(When::Key(key))
    }

    /// A key or a bare value: up to whitespace or an operator character.
    fn word(&mut self) -> Option<String> {
        self.ws();
        let len = self
            .rest()
            .find(|c: char| c.is_whitespace() || "()!=<>&|~'\"".contains(c))
            .unwrap_or(self.rest().len());
        if len == 0 {
            return None;
        }
        let word = self.rest()[..len].to_string();
        self.i += len;
        Some(word)
    }

    fn value(&mut self) -> Option<String> {
        self.ws();
        let quote = self
            .rest()
            .chars()
            .next()
            .filter(|c| *c == '\'' || *c == '"');
        let Some(q) = quote else {
            return self.word();
        };
        let end = self.rest()[1..].find(q)? + 1;
        let value = self.rest()[1..end].to_string();
        self.i += end + 1;
        Some(value)
    }

    /// A `/…/flags` regex (escapes honoured), or a quoted string.
    fn regex(&mut self) -> Option<()> {
        self.ws();
        if !self.rest().starts_with('/') {
            return self.value().map(|_| ());
        }
        let mut escaped = false;
        let end = self.rest()[1..].char_indices().find_map(|(i, c)| {
            let close = c == '/' && !escaped;
            escaped = c == '\\' && !escaped;
            close.then_some(i + 1)
        })?;
        self.i += end + 1;
        let flags = self.rest().len()
            - self
                .rest()
                .trim_start_matches(|c: char| c.is_ascii_alphabetic())
                .len();
        self.i += flags;
        Some(())
    }
}

/// The scopes a clause keeps, by name, each with its values: only ever wider than VS Code's condition, so a fact
/// can't hide a bind VS Code would run.
type Scopes = BTreeMap<String, BTreeSet<String>>;

fn scopes(when: &When) -> Scopes {
    let one = |k: &str, v: &str| Scopes::from([(k.to_string(), BTreeSet::from([v.to_string()]))]);
    match when {
        When::Key(k) => one(k, "true"),
        When::Not(inner) => match inner.as_ref() {
            When::Key(k) => one(k, "false"),
            _ => Scopes::new(),
        },
        When::Eq(k, v) => match k.as_str() {
            "editorLangId" | "resourceLangId" => one("filetype", v),
            "resourceExtname" => one("extension", v.strip_prefix('.').unwrap_or(v)),
            _ => one(k, v),
        },
        When::Other => Scopes::new(),
        When::And(a, b) => {
            let (mut a, b) = (scopes(a), scopes(b));
            for (k, v) in b {
                match a.get_mut(&k) {
                    // Both sides name it: the values both allow; none means it can't hold, so no scope (wider).
                    Some(mine) => {
                        let both: BTreeSet<_> = mine.intersection(&v).cloned().collect();
                        if both.is_empty() {
                            a.remove(&k);
                        } else {
                            *mine = both;
                        }
                    }
                    None => {
                        a.insert(k, v);
                    }
                }
            }
            a
        }
        When::Or(a, b) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/vscode")
            .join(name)
    }

    /// An env with only `HOME` (and the given others) set.
    fn env_at(home: &Path, vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let mut vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        vars.push(("HOME".into(), home.display().to_string()));
        move |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn binds<'a>(s: &'a Sheet, command: &str) -> Vec<&'a Bind> {
        s.binds
            .values()
            .flatten()
            .filter(|b| b.action == command)
            .collect()
    }

    fn keys(s: &Sheet, command: &str) -> Vec<String> {
        binds(s, command).iter().map(|b| b.key.clone()).collect()
    }

    /// A `when` as `k=v,w` text, scopes joined by spaces.
    fn when_text(w: &BTreeMap<Name, Values>) -> String {
        w.iter()
            .map(|(k, v)| {
                let vs: Vec<&str> = v.iter().map(Name::as_str).collect();
                format!("{k}={}", vs.join(","))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn scoped(when: &str) -> Option<String> {
        parse_when(when).map(|w| when_text(&scopes_of(&scopes(&w))))
    }

    fn pinned_entries() -> Vec<Entry> {
        let rules: Vec<Rule> = serde_json::from_str(&strip_jsonc(PINNED)).unwrap();
        rules
            .into_iter()
            .map(|rule| Entry {
                rule,
                category: None,
            })
            .collect()
    }

    #[test]
    fn pinned_file() {
        assert!(
            PINNED.contains("d2bbb8a815a1c45fc361a5a88e55528ad2e9ca52")
                && PINNED.contains("e646868")
        );
        let entries = pinned_entries();
        assert_eq!(entries.len(), 1202);
        let (s, keys_left, whens_left) = build("Visual Studio Code", &entries);
        // `[IntlBackslash]` (6) and the browser back and forward keys (2 each).
        assert_eq!(keys_left, 10);
        assert_eq!(whens_left, 0, "every default `when` parses");
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 1202 - 10);
        let mods = ["super", "ctrl", "alt", "shift"];
        for b in s.binds.values().flatten() {
            for chord in b.key.split(' ') {
                let parts: Vec<&str> = chord.split('+').collect();
                let (key, mods_used) = parts.split_last().unwrap();
                assert!(mods_used.iter().all(|m| mods.contains(m)), "{}", b.key);
                assert!(
                    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                    "{}",
                    b.key
                );
            }
        }
        assert_eq!(
            keys(&s, "workbench.action.showCommands"),
            ["F1", "ctrl+shift+p"]
        );
        assert_eq!(
            keys(&s, "workbench.action.terminal.toggleTerminal"),
            ["ctrl+grave"]
        );
        let comment = &binds(&s, "editor.action.commentLine")[0];
        assert_eq!(comment.key, "ctrl+slash");
        assert_eq!(
            when_text(&comment.when),
            "editorReadonly=false editorTextFocus=true"
        );
        assert!(s.binds.contains_key("workbench") && s.binds.contains_key("editor"));
        // `args` in the description.
        assert_eq!(
            binds(&s, "cursorEnd")[0].desc,
            r#"cursorEnd {"sticky":false}"#
        );
        assert!(s.binds["general"].iter().any(|b| b.action == "cursorEnd"));
    }

    #[test]
    fn user_file_over_the_defaults() {
        let env = env_at(&fixture("noindex-missing"), &[]);
        let home = fixture("home");
        let env_home = env_at(&home, &[]);
        let (s, facts, notes) = sheet_with(
            PINNED,
            Some("main.rs - project - Visual Studio Code"),
            None,
            &env_home,
        )
        .unwrap();
        assert_eq!(s.title, "Visual Studio Code");
        // Removals: key and `when`; key only; neither; a `when` that differs leaves the default.
        assert!(keys(&s, "editor.action.commentLine").is_empty());
        assert_eq!(keys(&s, "workbench.action.showCommands"), ["ctrl+shift+p"]);
        assert!(keys(&s, "workbench.action.quickOpen").is_empty());
        assert_eq!(keys(&s, "editor.action.rename"), ["F2"]);
        // Same key and `when`: replaced in place.
        assert!(keys(&s, "workbench.action.files.save").is_empty());
        assert_eq!(keys(&s, "workbench.action.files.saveAll"), ["ctrl+s"]);
        // An empty command isn't listed; a later user rule replaces an earlier one.
        assert!(s.binds.values().flatten().all(|b| b.key != "ctrl+alt+0"));
        assert!(keys(&s, "first.command").is_empty());
        assert_eq!(keys(&s, "second.command"), ["ctrl+alt+y"]);
        // `args` (with a `//` inside a string) in the description; `when` as scopes.
        let send = binds(&s, "workbench.action.terminal.sendSequence");
        let send = send.iter().find(|b| b.key == "ctrl+alt+t").unwrap();
        assert_eq!(
            send.desc,
            r#"workbench.action.terminal.sendSequence {"text":"echo // hi\n"}"#
        );
        assert_eq!(when_text(&send.when), "filetype=rust");
        // The user removed an extension's bind; the rest of the extensions' are there.
        assert!(keys(&s, "sample.hello").is_empty());
        assert_eq!(keys(&s, "sample.run"), ["ctrl+alt+r"]);
        // The title's file: `extension` and `filetype`.
        assert_eq!(facts.get("extension").map(Name::as_str), Some("rs"));
        assert_eq!(facts.get("filetype").map(Name::as_str), Some("rust"));
        // The broken extension and the defaults' untranslatable keys, in one line.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("leaving out 10 key(s)") && notes[0].contains("1 extension"),
            "{notes:?}"
        );
        // No user file, no extensions: the defaults, silently except the count.
        let (s, _, notes) = sheet_with(PINNED, Some("Visual Studio Code"), None, &env).unwrap();
        assert_eq!(keys(&s, "workbench.action.quickOpen"), ["ctrl+e", "ctrl+p"]);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn user_files_that_are_not_rules() {
        let home = std::env::temp_dir().join(format!("kbf-vscode-bad-{}", std::process::id()));
        let user = home.join(".config/Code/User");
        std::fs::create_dir_all(&user).unwrap();
        let env = env_at(&home, &[]);
        for (text, what) in [("{}", "an object"), ("[ nope", "not JSON")] {
            std::fs::write(user.join("keybindings.json"), text).unwrap();
            let (s, _, notes) = sheet_with(PINNED, Some("Visual Studio Code"), None, &env).unwrap();
            assert_eq!(notes.len(), 2, "{what}: {notes:?}");
            assert!(notes[0].contains("not a JSON array"), "{what}: {notes:?}");
            assert_eq!(keys(&s, "workbench.action.quickOpen"), ["ctrl+e", "ctrl+p"]);
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn when_table() {
        for (when, want) in [
            ("editorTextFocus", Some("editorTextFocus=true")),
            ("!editorReadonly", Some("editorReadonly=false")),
            ("a && b", Some("a=true b=true")),
            ("editorLangId == rust", Some("filetype=rust")),
            ("resourceLangId == 'prompt'", Some("filetype=prompt")),
            ("resourceExtname == .md", Some("extension=md")),
            ("editorLangId == a || editorLangId == b", Some("filetype=a,b")),
            (
                "inReferenceSearchEditor && !config.editor.stablePeek || referenceSearchVisible && !config.editor.stablePeek",
                Some("config.editor.stablePeek=false"),
            ),
            ("(a || b) && c", Some("c=true")),
            ("!(a && b)", Some("")),
            ("a != b", Some("")),
            ("a =~ /x\\/y/i && b", Some("b=true")),
            ("a in b", Some("")),
            ("a not in b", Some("")),
            ("a > 2", Some("")),
            ("a <= 2 && c", Some("c=true")),
            ("a == x && a == y", Some("")),
            ("a == 'has space'", Some("")),
            ("a && (b", None),
            ("a &&", None),
            ("== x", None),
            ("a == ", None),
        ] {
            assert_eq!(scoped(when).as_deref(), want, "{when}");
        }
    }

    #[test]
    fn variant_and_paths() {
        let none = |_: &str| None;
        for (title, name) in [
            ("main.rs - p - Visual Studio Code", "Visual Studio Code"),
            (
                "main.rs - p - Visual Studio Code - Insiders",
                "Visual Studio Code - Insiders",
            ),
            ("Welcome - p - VSCodium", "VSCodium"),
            ("x - Code - OSS", "Code - OSS"),
        ] {
            assert_eq!(
                variant(Some(title), None, &none).map(|v| v.name_long),
                Some(name),
                "{title}"
            );
        }
        // The class when the title doesn't say; the title over the class.
        for (class, name) in [
            ("Code", "Visual Studio Code"),
            ("code-oss", "Code - OSS"),
            ("VSCodium", "VSCodium"),
            ("com.visualstudio.code", "Visual Studio Code"),
            ("code-insiders", "Visual Studio Code - Insiders"),
        ] {
            assert_eq!(
                variant(Some("main.rs"), Some(class), &none).map(|v| v.name_long),
                Some(name),
                "{class}"
            );
        }
        assert_eq!(
            variant(Some("a - VSCodium"), Some("Code"), &none).map(|v| v.name_long),
            Some("VSCodium")
        );
        assert!(variant(None, Some("Alacritty"), &none).is_none());
        // The first variant whose config directory exists.
        let home = std::env::temp_dir().join(format!("kbf-vscode-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/VSCodium/User")).unwrap();
        let env = env_at(&home, &[]);
        assert_eq!(
            variant(None, None, &env).map(|v| v.name_long),
            Some("VSCodium")
        );
        // `XDG_CONFIG_HOME` set, empty, unset.
        std::fs::create_dir_all(home.join("xdg/Code/User")).unwrap();
        let xdg = home.join("xdg").display().to_string();
        let code = &VARIANTS[0];
        assert_eq!(
            user_dir(code, &env_at(&home, &[("XDG_CONFIG_HOME", &xdg)])),
            Some(home.join("xdg/Code/User"))
        );
        assert_eq!(
            user_dir(code, &env_at(&home, &[("XDG_CONFIG_HOME", "")])),
            None
        );
        std::fs::create_dir_all(home.join(".config/Code/User")).unwrap();
        assert_eq!(user_dir(code, &env), Some(home.join(".config/Code/User")));
        // The Flatpak's only when the host directory is absent.
        let flat = home.join(".var/app/com.visualstudio.code/config/Code/User");
        std::fs::create_dir_all(&flat).unwrap();
        assert_eq!(user_dir(code, &env), Some(home.join(".config/Code/User")));
        std::fs::remove_dir_all(home.join(".config/Code")).unwrap();
        assert_eq!(user_dir(code, &env), Some(flat));
        // The extensions directory the same way.
        let flat_ext = home.join(".var/app/com.visualstudio.code/data/vscode/extensions");
        std::fs::create_dir_all(&flat_ext).unwrap();
        assert_eq!(extensions_dir(code, &env), Some(flat_ext));
        std::fs::create_dir_all(home.join(".vscode/extensions")).unwrap();
        assert_eq!(
            extensions_dir(code, &env),
            Some(home.join(".vscode/extensions"))
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn title_facts() {
        let ext = |f: &str| title_extension(f).map(|n| n.to_string());
        for (file, want) in [
            ("main.rs", Some("rs")),
            ("foo.test.ts", Some("ts")),
            ("Makefile", None),
            (".bashrc", None),
            ("Untitled-1", None),
            ("x.has space", None),
            ("x.abcdefghijklmnopq", None),
        ] {
            assert_eq!(ext(file).as_deref(), want, "{file}");
        }
        let text = |title: &str| {
            facts(title)
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            text("\u{25cf} main.rs - project - Visual Studio Code"),
            "extension=rs filetype=rust"
        );
        assert_eq!(text("Welcome - project - VSCodium"), "");
        assert_eq!(text("my.project - Visual Studio Code"), "extension=project");
        assert_eq!(
            text("notes.MD - Code - OSS"),
            "extension=MD filetype=markdown"
        );
        assert_eq!(text("main.rs - project"), "");
        assert_eq!(
            text("main.rs - project - Visual Studio Code - Insiders"),
            "extension=rs filetype=rust"
        );
    }

    #[test]
    fn key_notation() {
        for (key, want) in [
            ("ctrl+shift+alt+meta+a", Some("super+ctrl+alt+shift+a")),
            ("ctrl+[KeyA]", Some("ctrl+a")),
            ("shift+[Digit1]", Some("shift+1")),
            ("ctrl+[IntlBackslash]", None),
            ("ctrl+numpad_add", Some("ctrl+KP_Add")),
            ("numpad7", Some("KP_7")),
            ("ctrl+oem_102", None),
            ("ctrl+;", Some("ctrl+semicolon")),
            ("ctrl+\\", Some("ctrl+backslash")),
            ("ctrl+k ctrl+s", Some("ctrl+k ctrl+s")),
            ("ctrl++", Some("ctrl+plus")),
            ("f19", Some("F19")),
            ("f20", None),
            ("cmd+a", None),
            ("browserback", None),
            ("enter", Some("Return")),
            ("pagedown", Some("Next")),
            ("", None),
        ] {
            assert_eq!(notation(key).as_deref(), want, "{key:?}");
        }
    }

    #[test]
    fn installed_extensions() {
        let env = env_at(&fixture("home"), &[]);
        let mut broken = 0;
        let entries = extension_rules(&VARIANTS[0], &env, &mut broken);
        // Listed in extensions.json (new and old formats); 0.9.0, left behind, isn't; the broken one is counted.
        assert_eq!(broken, 1);
        let (s, _, _) = build("Visual Studio Code", &entries);
        assert_eq!(keys(&s, "sample.hello"), ["ctrl+alt+e"]);
        assert!(keys(&s, "sample.old").is_empty());
        // `linux` over `key`; a single object; category from `displayName`, else `name`.
        assert_eq!(keys(&s, "single.go"), ["ctrl+alt+l"]);
        assert!(s.binds["Sample Hello"]
            .iter()
            .any(|b| b.action == "sample.run"));
        assert!(s.binds["single"].iter().any(|b| b.action == "single.go"));
        // No extensions.json: every folder.
        let env = env_at(&fixture("noindex"), &[]);
        let (s, _, _) = build("x", &extension_rules(&VARIANTS[0], &env, &mut broken));
        assert_eq!(keys(&s, "one.go"), ["ctrl+alt+1"]);
        assert_eq!(keys(&s, "two.go"), ["ctrl+alt+2"]);
    }

    #[test]
    fn bad_pinned_file() {
        assert!(sheet_with("{", None, None, &|_| None).is_err());
    }
}
