//! foot: its default key, search and URL bindings pinned per version (`data/foot/`, generated from foot's source),
//! with the user's `foot.ini` laid over them as foot's `config.c` reads it. `pipe-*` commands are shown, never run.

use super::pinned;
use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const REGISTRY: &[(&str, &str)] = &[(
    "1.28.0",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/foot/1.28.0.toml"
    )),
)];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// How deep `include=` may nest before reading stops.
const MAX_DEPTH: usize = 8;

/// The binding sections read, the section their bindings live in (foot keeps `[text-bindings]` with the key
/// bindings), and the category each is listed under.
const SECTIONS: [(&str, &str, &str); 4] = [
    ("key-bindings", "key-bindings", "foot"),
    ("search-bindings", "search-bindings", "search"),
    ("url-bindings", "url-bindings", "url"),
    ("text-bindings", "key-bindings", "text"),
];

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `data/foot/<version>.toml`: each section's actions with their descriptions, and the default bindings.
#[derive(Deserialize)]
struct Pinned {
    actions: BTreeMap<String, BTreeMap<String, String>>,
    bind: Vec<PinnedBind>,
}

#[derive(Deserialize)]
struct PinnedBind {
    section: String,
    action: String,
    combo: String,
}

/// One combo bound to an action. `aux` is a `pipe-*` command, a `regex-*` name or a text binding's text: foot
/// replaces and compares bindings by action and aux together.
#[derive(Clone, Debug, PartialEq)]
struct Binding {
    action: String,
    aux: Option<String>,
    /// The modifier names as written, sorted (foot compares them as strings).
    mods: Vec<String>,
    key: String,
    text: bool,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(REGISTRY, |_| {}, &env)?;
    if !notes.is_empty() {
        eprintln!("keybind-find: foot: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `sheet` over `registry`, with `setup` applied to `foot --version` (a test seam: `PATH`) and `foot.ini` found
/// through `env`. Without `foot`, the newest defaults and the note that says so.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut cmd = Command::new("foot");
    cmd.arg("--version")
        .env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let installed = run_for(cmd, VERSION_TIMEOUT)
        .ok()
        .and_then(|t| version_of(&t));
    let (v, text, note) = pinned::select("foot", registry, installed.as_deref())
        .context("no pinned foot bindings")?;
    let pinned: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/foot/{v}.toml"))?;
    let home = env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let (sheet, mut notes) = parse(&pinned, config_file(env).as_deref(), home.as_deref());
    notes.splice(0..0, note);
    Ok((sheet, notes))
}

/// The version in `foot --version`'s `foot version: 1.28.0 +pgo +ime …`; a git build's `1.28.0-12-gabc1234` is
/// its release.
fn version_of(text: &str) -> Option<String> {
    let v = text
        .trim()
        .strip_prefix("foot version: ")?
        .split_whitespace()
        .next()?;
    let v = v.split('-').next()?;
    pinned::parse_version(v).map(|_| v.to_string())
}

/// Where foot looks for `foot.ini`, in order: `$XDG_CONFIG_HOME` (else `~/.config`), then each `$XDG_CONFIG_DIRS`
/// entry (else `/etc/xdg`); an empty variable counts as unset (`config.c` `open_config`).
fn candidates(env: Env) -> Vec<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    let home = set("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|h| PathBuf::from(h).join(".config")));
    let dirs = set("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into());
    home.into_iter()
        .chain(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from))
        .map(|d| d.join("foot/foot.ini"))
        .collect()
}

/// The first `foot.ini` foot would open. Only that one is read (with its includes).
fn config_file(env: Env) -> Option<PathBuf> {
    candidates(env)
        .into_iter()
        .find(|p| std::fs::File::open(p).is_ok_and(|f| f.metadata().is_ok_and(|m| m.is_file())))
}

/// The sheet from `defaults` with `root` (and what it includes) applied, and the notes for stderr. `home` expands an
/// `include=~/…`.
fn parse(defaults: &Pinned, root: Option<&Path>, home: Option<&Path>) -> (Sheet, Vec<String>) {
    let mut lists: BTreeMap<&str, Vec<Binding>> = BTreeMap::new();
    for b in &defaults.bind {
        let Some((section, _, _)) = SECTIONS.iter().find(|(s, _, _)| *s == b.section) else {
            continue;
        };
        if let Some(binding) = combo(&b.action, None, &b.combo, false) {
            lists.entry(*section).or_default().push(binding);
        }
    }
    let mut reader = Reader {
        actions: &defaults.actions,
        lists,
        notes: Vec::new(),
        malformed: 0,
        home,
        open: Vec::new(),
    };
    if let Some(root) = root {
        reader.file(root);
    }
    let Reader {
        mut lists,
        mut notes,
        malformed,
        ..
    } = reader;
    let mut left_out = malformed;
    let mut sheet = Sheet {
        title: "foot".into(),
        provider: "foot".into(),
        binds: Default::default(),
    };
    for (section, list) in &mut lists {
        notes.extend(resolve_collisions(section, list));
        for b in list.iter() {
            let (_, _, category) = SECTIONS
                .iter()
                .find(|(s, l, _)| *l == *section && (b.text == (*s == "text-bindings")))
                .copied()
                .unwrap_or(("", "", "foot"));
            let Some(key) = notation(b) else {
                left_out += 1;
                continue;
            };
            let desc = if b.text {
                format!("send `{}`", b.aux.as_deref().unwrap_or_default())
            } else {
                let desc = defaults
                    .actions
                    .get(*section)
                    .and_then(|a| a.get(&b.action))
                    .cloned()
                    .unwrap_or_else(|| b.action.replace('-', " "));
                match &b.aux {
                    Some(aux) => format!("{desc} [{aux}]"),
                    None => desc,
                }
            };
            sheet.binds.entry(category.into()).or_default().push(Bind {
                key,
                desc,
                action: if b.text {
                    "text-binding".into()
                } else {
                    b.action.clone()
                },
                when: Default::default(),
            });
        }
    }
    if left_out > 0 {
        let s = if left_out == 1 { "" } else { "s" };
        notes.push(format!(
            "leaving out {left_out} foot.ini binding{s} keybind-find can't read or send (a modifier other than \
             Control, Shift, Mod1/Alt and Mod4/Super, a key name that isn't a keysym, or a malformed line)"
        ));
    }
    (sheet, notes)
}

/// foot's own rule after reading the config (`resolve_key_binding_collisions`): a combo already bound to another
/// action (or another command or text) earlier in the list keeps that binding, and the later one is dropped with an
/// error. The user's lines come after the defaults they don't replace, so a default wins over them.
// ponytail: keys compared by name, as written; foot compares keysyms, so aliases (`Prior`/`Page_Up`) aren't seen as one.
fn resolve_collisions(section: &str, list: &mut Vec<Binding>) -> Vec<String> {
    let mut notes = Vec::new();
    let mut i = 1;
    while i < list.len() {
        let b = &list[i];
        let earlier = list[..i].iter().rev().find(|e| {
            !(e.action == b.action && e.aux == b.aux) && e.mods == b.mods && e.key == b.key
        });
        match earlier {
            Some(e) => {
                let what = |b: &Binding| {
                    if b.text {
                        format!("text {}", b.aux.as_deref().unwrap_or_default())
                    } else {
                        b.action.clone()
                    }
                };
                notes.push(format!(
                    "[{section}] {}: {} is already mapped to {}; foot keeps that",
                    what(b),
                    combo_text(b),
                    what(e)
                ));
                list.remove(i);
            }
            None => i += 1,
        }
    }
    notes
}

fn combo_text(b: &Binding) -> String {
    b.mods
        .iter()
        .map(String::as_str)
        .chain([b.key.as_str()])
        .collect::<Vec<_>>()
        .join("+")
}

/// One `mods+key` combo as foot splits it (at the last `+`; `none` is no modifiers); `None` for a key that isn't
/// shaped like a keysym name (foot then refuses the whole line).
// ponytail: no keysym table; a single letter or digit, or a letter then letters, digits and `_`, passes as a keysym.
fn combo(action: &str, aux: Option<&str>, combo: &str, text: bool) -> Option<Binding> {
    let (mods, key) = combo.rsplit_once('+').unwrap_or(("", combo));
    let mut mods: Vec<String> = if mods == "none" {
        Vec::new()
    } else {
        mods.split('+')
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .collect()
    };
    mods.sort();
    let mut chars = key.chars();
    let keysym = match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_ascii_alphanumeric(),
        (Some(c), Some(_)) => {
            c.is_ascii_alphabetic() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    keysym.then(|| Binding {
        action: action.into(),
        aux: aux.map(str::to_string),
        mods,
        key: key.into(),
        text,
    })
}

/// A binding in keybind-find's notation; `None` for a modifier keybind-find can't send.
fn notation(b: &Binding) -> Option<String> {
    let mut mods = b
        .mods
        .iter()
        .map(|m| match m.as_str() {
            "Control" => Some("ctrl"),
            "Shift" => Some("shift"),
            "Mod1" | "Alt" => Some("alt"),
            "Mod4" | "Super" => Some("super"),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let key = match b.key.as_str() {
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_uppercase()) => {
            mods.push("shift");
            k.to_ascii_lowercase()
        }
        k => k.to_string(),
    };
    Some(chord(&mods, &key))
}

struct Reader<'a> {
    actions: &'a BTreeMap<String, BTreeMap<String, String>>,
    lists: BTreeMap<&'static str, Vec<Binding>>,
    notes: Vec<String>,
    malformed: usize,
    home: Option<&'a Path>,
    /// The files being read, outermost first: an include of one of them is a cycle.
    open: Vec<PathBuf>,
}

impl Reader<'_> {
    /// Reads `path` as foot's `parse_config_file` does, starting in `[main]`.
    fn file(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if self.open.contains(&canonical) {
            self.notes.push(format!(
                "{}: included again inside itself; not read again",
                path.display()
            ));
            return;
        }
        if self.open.len() > MAX_DEPTH {
            self.notes.push(format!(
                "{}: includes nest deeper than {MAX_DEPTH}; not read",
                path.display()
            ));
            return;
        }
        let text = match std::fs::read(path) {
            Ok(t) => String::from_utf8_lossy(&t).into_owned(),
            Err(e) => {
                self.notes.push(format!("{}: {e}", path.display()));
                return;
            }
        };
        self.open.push(canonical);
        // `None`: a section that isn't read (or an invalid one): its keys are skipped.
        let mut section = Some("main");
        for line in text.lines() {
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // A comment is a `#` after a blank, from the second character on.
            let cut = line
                .char_indices()
                .skip(1)
                .find(|&(i, c)| c == '#' && line[..i].ends_with([' ', '\t']))
                .map_or(line.len(), |(i, _)| i);
            let line = line[..cut].trim_end();
            if let Some(name) = line.strip_prefix('[') {
                section = name
                    .strip_suffix(']')
                    .filter(|n| !n.contains(']'))
                    .and_then(|n| {
                        ["main"]
                            .into_iter()
                            .chain(SECTIONS.iter().map(|(s, _, _)| *s))
                            .find(|s| *s == n)
                    });
                continue;
            }
            let Some(section) = section else {
                continue;
            };
            let kv = line
                .split_once('=')
                .map(|(k, v)| (k.trim_end(), v.trim_start()))
                .filter(|(k, v)| !k.is_empty() && !v.is_empty());
            match (section, kv) {
                ("main", Some(("include", value))) => self.include(path, value),
                ("main", _) => {}
                (_, None) => self.malformed += 1,
                (_, Some((key, value))) => {
                    if !self.binding(section, key, value) {
                        self.malformed += 1;
                    }
                }
            }
        }
        self.open.pop();
    }

    fn include(&mut self, from: &Path, value: &str) {
        let path = match value.strip_prefix("~/") {
            Some(rest) => match self.home {
                Some(home) => home.join(rest),
                None => {
                    self.notes.push(format!(
                        "{}: include={value}: no HOME to expand ~",
                        from.display()
                    ));
                    return;
                }
            },
            None => PathBuf::from(value),
        };
        if !path.is_absolute() {
            self.notes.push(format!(
                "{}: include={value}: not an absolute path",
                from.display()
            ));
            return;
        }
        self.file(&path);
    }

    /// One `key=value` line of a binding section, applied as foot's `value_to_key_combos` does: the action's (and
    /// aux's) bindings are replaced by the new combos, or removed by `none`. False for a line foot refuses.
    fn binding(&mut self, section: &'static str, key: &str, value: &str) -> bool {
        let (_, list, _) = SECTIONS
            .iter()
            .find(|(s, _, _)| *s == section)
            .copied()
            .unwrap_or(("", section, ""));
        let text = section == "text-bindings";
        let (action, aux, value) = if text {
            if !valid_text(key) {
                return false;
            }
            ("", Some(key), value)
        } else {
            if !self
                .actions
                .get(section)
                .is_some_and(|a| a.contains_key(key))
            {
                return false;
            }
            // `pipe-*` take a command and `regex-*` a regex name, in `[…]` up to the last `]`.
            if section == "key-bindings" && (key.starts_with("pipe-") || key.starts_with("regex-"))
            {
                let Some((aux, rest)) = value.strip_prefix('[').and_then(|v| v.rsplit_once(']'))
                else {
                    return false;
                };
                (key, Some(aux), rest.trim_start())
            } else {
                (key, None, value)
            }
        };
        let list = self.lists.entry(list).or_default();
        let same = |b: &Binding| b.action == action && b.aux.as_deref() == aux && b.text == text;
        if value.eq_ignore_ascii_case("none") {
            list.retain(|b| !same(b));
            return true;
        }
        let Some(new) = value
            .split(' ')
            .filter(|c| !c.is_empty())
            .map(|c| combo(action, aux, c, text))
            .collect::<Option<Vec<_>>>()
            .filter(|n| !n.is_empty())
        else {
            return false;
        };
        list.retain(|b| !same(b));
        list.extend(new);
        true
    }
}

/// A `[text-bindings]` text as foot accepts it: a `\` only as `\xHH`.
fn valid_text(text: &str) -> bool {
    let mut rest = text;
    while let Some(i) = rest.find('\\') {
        let esc = &rest[i + 1..];
        let Some(hex) = esc.strip_prefix('x').and_then(|h| h.get(..2)) else {
            return false;
        };
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        rest = &esc[3..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/foot")
            .join(name)
    }

    fn pinned() -> Pinned {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    fn rows<'a>(s: &'a Sheet, category: &str) -> Vec<(&'a str, &'a str)> {
        s.binds
            .get(category)
            .map(|b| {
                b.iter()
                    .map(|b| (b.key.as_str(), b.desc.as_str()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn keys<'a>(s: &'a Sheet, category: &str, action: &str) -> Vec<&'a str> {
        s.binds
            .get(category)
            .map(|b| {
                b.iter()
                    .filter(|b| b.action == action)
                    .map(|b| b.key.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn pinned_rows_all_parse() {
        let p = pinned();
        let count = |s: &str| p.bind.iter().filter(|b| b.section == s).count();
        assert_eq!(
            (
                count("key-bindings"),
                count("search-bindings"),
                count("url-bindings")
            ),
            (22, 44, 5)
        );
        for b in &p.bind {
            assert!(
                p.actions[&b.section].contains_key(&b.action),
                "{} {}",
                b.section,
                b.action
            );
            let binding = combo(&b.action, None, &b.combo, false).unwrap();
            assert!(notation(&binding).is_some(), "{}", b.combo);
        }
        let (s, notes) = parse(&p, None, None);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            [&s.binds["foot"], &s.binds["search"], &s.binds["url"]].map(Vec::len),
            [22, 44, 5]
        );
        assert_eq!(
            keys(&s, "foot", "clipboard-copy"),
            ["ctrl+shift+c", "XF86Copy"]
        );
        assert_eq!(
            keys(&s, "foot", "scrollback-up-page"),
            ["shift+Prior", "shift+KP_Prior"]
        );
        assert_eq!(
            keys(&s, "search", "cursor-left-word"),
            ["ctrl+Left", "alt+b"]
        );
        assert_eq!(keys(&s, "url", "toggle-url-visible"), ["t"]);
        assert!(rows(&s, "foot").contains(&("ctrl+plus", "Increases the font size by 0.5pt.")));
    }

    #[test]
    fn foot_ini_over_the_defaults() {
        let home = fixture("home");
        let (s, notes) = parse(
            &pinned(),
            Some(&home.join(".config/foot/foot.ini")),
            Some(&home),
        );
        // Replaced: every default combo of the action goes.
        assert_eq!(keys(&s, "foot", "clipboard-copy"), ["ctrl+alt+c"]);
        // Removed.
        assert!(keys(&s, "foot", "search-start").is_empty());
        // Control+Shift+r, freed by `search-start=none`, moved to fullscreen.
        assert_eq!(keys(&s, "foot", "fullscreen"), ["ctrl+shift+r"]);
        // The command and the regex name are kept in the description.
        assert_eq!(
            rows(&s, "foot")
                .into_iter()
                .filter(|(k, _)| *k == "ctrl+Print" || *k == "ctrl+shift+q")
                .map(|(_, d)| d)
                .collect::<Vec<_>>(),
            [
                "Pipes the currently visible text, the entire scrollback, the currently selected text, or the last \
                 command's output to an external tool. [sh -c \"xurls | fuzzel | xargs -r firefox\"]",
                "Enter regex mode. [hashes]",
            ]
        );
        // An upper-case letter is shift and the letter.
        assert_eq!(keys(&s, "foot", "spawn-terminal"), ["ctrl+shift+n"]);
        // Text bindings, shown escaped.
        assert_eq!(rows(&s, "text"), [("super+k", "send `\\x1b[A`")]);
        // The include: its [search-bindings] line applies, and the including file's next key stays in [main]
        // (`font-reset` there is no binding), then its own [url-bindings].
        assert_eq!(keys(&s, "search", "cancel"), ["Escape"]);
        assert_eq!(keys(&s, "foot", "font-reset"), ["ctrl+0", "ctrl+KP_0"]);
        assert_eq!(keys(&s, "url", "cancel"), ["ctrl+q"]);
        // The default kept Control+Shift+v when the user gave it to another action: foot keeps the earlier binding.
        assert_eq!(
            keys(&s, "foot", "clipboard-paste"),
            ["ctrl+shift+v", "XF86Paste"]
        );
        assert!(keys(&s, "foot", "primary-paste").is_empty());
        assert!(
            notes.iter().any(|n| n
                == "[key-bindings] primary-paste: Control+Shift+v is already mapped to clipboard-paste; foot keeps that"),
            "{notes:?}"
        );
        // The cycle back to foot.ini: a note, read once.
        assert!(
            notes
                .iter()
                .any(|n| n.ends_with("foot.ini: included again inside itself; not read again")),
            "{notes:?}"
        );
        // Mod3+x (an unsendable modifier), `bogus-action`, a key that isn't a keysym (the whole line refused), a
        // line with no `=`, an empty value, and a bad `\` in a text: counted.
        assert_eq!(
            notes.last().unwrap(),
            "leaving out 6 foot.ini bindings keybind-find can't read or send (a modifier other than Control, Shift, \
             Mod1/Alt and Mod4/Super, a key name that isn't a keysym, or a malformed line)"
        );
        // Mod3+x replaced unicode-input's default: listed nowhere.
        assert!(keys(&s, "foot", "unicode-input").is_empty());
        // The refused line left prompt-prev's default as it was.
        assert_eq!(keys(&s, "foot", "prompt-prev"), ["ctrl+shift+z"]);
        assert_eq!(notes.len(), 3, "{notes:?}");
    }

    #[test]
    fn includes_nest_up_to_a_depth() {
        let tmp = std::env::temp_dir().join(format!("kbf-foot-deep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        for i in 0..12 {
            std::fs::write(
                tmp.join(format!("{i}.ini")),
                format!(
                    "include={}\n[key-bindings]\nquit=Control+F{}\n",
                    tmp.join(format!("{}.ini", i + 1)).display(),
                    i + 1
                ),
            )
            .unwrap();
        }
        let (s, notes) = parse(&pinned(), Some(&tmp.join("0.ini")), None);
        let _ = std::fs::remove_dir_all(&tmp);
        // Files 0-8 are read (each sets quit; the outermost, read last, wins); 9 is too deep.
        assert_eq!(keys(&s, "foot", "quit"), ["ctrl+F1"]);
        assert!(
            notes[0].ends_with("9.ini: includes nest deeper than 8; not read"),
            "{notes:?}"
        );
        // A relative include, a missing one and `~/` without HOME: notes.
        let dir = std::env::temp_dir().join(format!("kbf-foot-inc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("foot.ini"),
            "include=rel.ini\ninclude=/nonexistent/x.ini\ninclude=~/x.ini\n",
        )
        .unwrap();
        let (_, notes) = parse(&pinned(), Some(&dir.join("foot.ini")), None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes[0].ends_with("include=rel.ini: not an absolute path"));
        assert!(notes[1].starts_with("/nonexistent/x.ini: "));
        assert!(notes[2].ends_with("include=~/x.ini: no HOME to expand ~"));
    }

    #[test]
    fn where_foot_ini_is_looked_for() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let paths = |e: &dyn Fn(&str) -> Option<String>| {
            candidates(e)
                .into_iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            paths(&env(&[
                ("XDG_CONFIG_HOME", "/x"),
                ("HOME", "/h"),
                ("XDG_CONFIG_DIRS", "/a::/b")
            ])),
            ["/x/foot/foot.ini", "/a/foot/foot.ini", "/b/foot/foot.ini"]
        );
        // Empty variables are unset: ~/.config, then /etc/xdg.
        assert_eq!(
            paths(&env(&[
                ("XDG_CONFIG_HOME", ""),
                ("HOME", "/h"),
                ("XDG_CONFIG_DIRS", "")
            ])),
            ["/h/.config/foot/foot.ini", "/etc/xdg/foot/foot.ini"]
        );
        assert_eq!(paths(&env(&[])), ["/etc/xdg/foot/foot.ini"]);
        // The first that exists: here the XDG_CONFIG_DIRS one.
        let home = fixture("home");
        let dirs = fixture("dirs");
        let (h, d) = (
            fixture("nowhere").display().to_string(),
            dirs.display().to_string(),
        );
        let e = move |k: &str| match k {
            "XDG_CONFIG_HOME" => Some(h.clone()),
            "XDG_CONFIG_DIRS" => Some(d.clone()),
            _ => None,
        };
        assert_eq!(config_file(&e), Some(dirs.join("foot/foot.ini")));
        let h = home.join(".config").display().to_string();
        let e = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| h.clone());
        assert_eq!(config_file(&e), Some(home.join(".config/foot/foot.ini")));
    }

    #[test]
    fn version_line() {
        assert_eq!(
            version_of("foot version: 1.28.0 +pgo +ime +graphemes -assertions\n").as_deref(),
            Some("1.28.0")
        );
        assert_eq!(
            version_of("foot version: 1.28.0-12-gabc1234 (Oct 27 2026, branch 'master') +pgo")
                .as_deref(),
            Some("1.28.0")
        );
        for junk in ["", "foot 1.28.0", "foot version: x.y", "version: 1.28.0"] {
            assert_eq!(version_of(junk), None, "{junk}");
        }
    }

    /// A stand-in `foot` printing `out`, in its own directory; returns the directory.
    fn fake(name: &str, out: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!("kbf-foot-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process (ETXTBSY: see `xfce.rs`'s `fake`).
        let body = format!("[ \"$*|$LC_ALL\" = '--version|C' ] || exit 9; echo '{out}'");
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("foot"))
            .arg(body)
            .status()
            .unwrap();
        assert!(ok.success());
        bin
    }

    #[test]
    fn version_picks_the_defaults() {
        let path = |bin: &Path| {
            let bin = bin.to_path_buf();
            move |c: &mut Command| {
                c.env("PATH", &bin);
            }
        };
        let none = |_: &str| None;
        let bin = fake("exact", "foot version: 1.28.0 +pgo +ime");
        let (s, notes) = sheet_with(REGISTRY, path(&bin), &none).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.title, "foot");
        let bin = fake("newer", "foot version: 1.29.1 +pgo");
        let (_, notes) = sheet_with(REGISTRY, path(&bin), &none).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert_eq!(
            notes,
            ["foot 1.29.1: using the defaults of 1.28.0; add data/foot/1.29.1.toml"]
        );
        // No foot: the newest defaults, the user's file, and the note.
        let missing = std::env::temp_dir().join(format!("kbf-foot-none-{}", std::process::id()));
        let home = fixture("home").join(".config").display().to_string();
        let env = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| home.clone());
        let (s, notes) = sheet_with(REGISTRY, path(&missing), &env).unwrap();
        assert_eq!(
            notes[0],
            "foot's version is unknown: using the defaults of 1.28.0"
        );
        assert_eq!(keys(&s, "foot", "clipboard-copy"), ["ctrl+alt+c"]);
    }

    #[test]
    fn real_foot_version() {
        let Ok(out) = Command::new("foot")
            .arg("--version")
            .env("LC_ALL", "C")
            .output()
        else {
            eprintln!("skipping: foot is not installed");
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(version_of(&text).is_some(), "{text}");
    }
}
