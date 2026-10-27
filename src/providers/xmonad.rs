//! xmonad: reads the keymap xmonad writes itself (`showKmSimple` via `XMonad.Util.NamedActions`, see docs/xmonad.md).
//!
//! Two key formats exist. xmonad-contrib 0.16 writes `M4-Shift-Return`, `M4-comma`. From 0.17.1, NamedActions uses
//! `keyToString`: `M4-S-<Return>`, `M4-,`, `M4--` (Super+minus), with `M1-`..`M5-`, `C-`, `lock-`, `num-` prefixes
//! (checked against xmonad-contrib 0.18.2 output and `XMonad.Prelude.keyToString`).

use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let path = dump_path(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
        .context("finding the xmonad keymap dump: neither XDG_CACHE_HOME nor HOME is set")?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "{} not found: xmonad writes it when its keybind-find key (Super+/ by default) is pressed; \
             set that up as docs/xmonad.md describes",
            path.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    // GHC's writeFile uses the locale's encoding; don't let one odd byte lose the whole sheet.
    let sheet = parse(&String::from_utf8_lossy(&bytes))?;
    if sheet.binds.is_empty() {
        bail!("{} has no bindings", path.display());
    }
    Ok(sheet)
}

/// `$XDG_CACHE_HOME/keybind-find/xmonad.keys`, else `$HOME/.cache/...`: where `getXdgDirectory XdgCache` writes.
/// A relative `XDG_CACHE_HOME` is ignored, as the XDG spec says.
fn dump_path(xdg_cache_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let cache = match xdg_cache_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => dir,
        None => PathBuf::from(home.filter(|v| !v.is_empty())?).join(".cache"),
    };
    Some(cache.join("keybind-find/xmonad.keys"))
}

fn parse(text: &str) -> anyhow::Result<Sheet> {
    let (sheet, skipped) = parse_reporting(text);
    for line in skipped {
        eprintln!("keybind-find: xmonad: skipping {line:?}: not a key xmonad writes");
    }
    Ok(sheet)
}

/// One `<keys> <description>` line per binding. `showKmSimple` puts a blank line before every `subtitle`, so only a
/// line ending in `:` that follows a blank line (or starts the file) is a heading; a description may end in `:` too.
/// A key bound twice keeps its last entry, as xmonad's `Map.fromList` does, except the help key: the dump lists it
/// first but `addDescrKeys` appends it to the real map, so it wins. Returns the sheet and the lines it skipped.
fn parse_reporting(text: &str) -> (Sheet, Vec<String>) {
    let mut category = "general".to_string();
    let mut skipped = Vec::new();
    // (category, bind) in file order; a repeated key replaces the earlier entry wherever it sat.
    let mut binds: Vec<(String, Bind)> = Vec::new();
    let mut help_key: Option<String> = None;
    let mut after_blank = true;
    let mut first_line = true;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            after_blank = true;
            continue;
        }
        let at_start = std::mem::replace(&mut first_line, false);
        let heading_allowed = std::mem::replace(&mut after_blank, false);
        if heading_allowed {
            if let Some(heading) = line.strip_suffix(':') {
                category = heading.to_string();
                continue;
            }
        }
        let (keys, desc) = line.split_once(' ').unwrap_or((line, ""));
        let Some(key) = key(keys) else {
            skipped.push(line.to_string());
            continue;
        };
        if at_start {
            help_key = Some(key.clone());
        } else if help_key.as_ref() == Some(&key) {
            continue;
        }
        binds.retain(|(_, b)| b.key != key);
        binds.push((
            category.clone(),
            Bind {
                key,
                desc: desc.trim().to_string(),
                action: String::new(),
                when: Default::default(),
            },
        ));
    }
    let mut by_category: BTreeMap<String, Vec<Bind>> = BTreeMap::new();
    for (c, b) in binds {
        by_category.entry(c).or_default().push(b);
    }
    let sheet = Sheet {
        title: "xmonad".into(),
        provider: "xmonad".into(),
        binds: by_category,
    };
    (sheet, skipped)
}

/// Modifier prefixes of both formats. `lock-`/`num-` are dropped: xmonad strips Caps Lock and Num Lock when matching.
const MODS: &[(&str, Option<&str>)] = &[
    ("M4-", Some("super")),
    ("Mod4-", Some("super")),
    ("Shift-", Some("shift")),
    ("S-", Some("shift")),
    ("C-", Some("ctrl")),
    ("Ctrl-", Some("ctrl")),
    ("Control-", Some("ctrl")),
    ("M1-", Some("alt")),
    ("Mod1-", Some("alt")),
    ("M2-", Some("mod2")),
    ("Mod2-", Some("mod2")),
    ("M3-", Some("mod3")),
    ("Mod3-", Some("mod3")),
    ("M5-", Some("mod5")),
    ("Mod5-", Some("mod5")),
    ("lock-", None),
    ("num-", None),
];

/// `M4-Shift-Return` or `M4-S-<Return>` → `super+shift+Return`; `M4--` → `super+minus`. `None` if it isn't a key.
fn key(xmonad: &str) -> Option<String> {
    let mut rest = xmonad;
    let mut mods = Vec::new();
    'prefix: loop {
        for (prefix, name) in MODS {
            if let Some(r) = rest.strip_prefix(prefix).filter(|r| !r.is_empty()) {
                rest = r;
                mods.extend(*name);
                continue 'prefix;
            }
        }
        break;
    }
    // What's left is one key. A `-` inside it (other than `-` itself) means a modifier we don't know.
    if rest.is_empty() || (rest != "-" && rest.contains('-')) {
        return None;
    }
    Some(chord(&mods, &keysym(rest)))
}

/// The X keysym name for a key as xmonad writes it: `<Return>` → `Return`, `,` → `comma`, `Return` → `Return`.
fn keysym(s: &str) -> String {
    if let Some(name) = s.strip_prefix('<').and_then(|s| s.strip_suffix('>')) {
        if !name.is_empty() {
            return name.to_string();
        }
    }
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => punctuation(c).map_or_else(|| c.to_string(), str::to_string),
        // ponytail: Latin-1 letters (`ö`) stay as the character; add their keysym names if a binding ever needs one.
        _ => s.to_string(),
    }
}

fn punctuation(c: char) -> Option<&'static str> {
    Some(match c {
        '!' => "exclam",
        '"' => "quotedbl",
        '#' => "numbersign",
        '$' => "dollar",
        '%' => "percent",
        '&' => "ampersand",
        '\'' => "apostrophe",
        '(' => "parenleft",
        ')' => "parenright",
        '*' => "asterisk",
        '+' => "plus",
        ',' => "comma",
        '-' => "minus",
        '.' => "period",
        '/' => "slash",
        ':' => "colon",
        ';' => "semicolon",
        '<' => "less",
        '=' => "equal",
        '>' => "greater",
        '?' => "question",
        '@' => "at",
        '[' => "bracketleft",
        '\\' => "backslash",
        ']' => "bracketright",
        '^' => "asciicircum",
        '_' => "underscore",
        '`' => "grave",
        '{' => "braceleft",
        '|' => "bar",
        '}' => "braceright",
        '~' => "asciitilde",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/xmonad.keys"
    ));

    /// Real `showKmSimple` output from Arch's xmonad-contrib 0.18.2 (runghc probe, 2026-10-21), keyToString format.
    const NEW_FORMAT: &str = "M1-M2-M3-M4-M5-S-C-lock-a all\n, comma\nM4-S-<Return> sret\nM4-- m4minus\n- minus\n\
<XF86AudioRaiseVolume> vol\n<F1> f1\n<KP_Subtract> kp\n<KP_Enter> kpe\nISO_Left_Tab isolt\n\u{f6} \u{f6}\nM4-a ends with colon:\nM4-c\n\
M4-d multi\nline\nM4-e \u{dc}n\u{ef}code \u{2713}\nM4-f\nM4-g s\n\nhead:\nM4-h after heading\n";

    fn find<'a>(s: &'a Sheet, key: &str) -> Vec<(&'a str, &'a Bind)> {
        s.binds
            .iter()
            .flat_map(|(c, bs)| bs.iter().map(move |b| (c.as_str(), b)))
            .filter(|(_, b)| b.key == key)
            .collect()
    }

    fn one<'a>(s: &'a Sheet, key: &str) -> (&'a str, &'a str) {
        let found = find(s, key);
        assert_eq!(found.len(), 1, "{key}: {found:?}");
        (found[0].0, found[0].1.desc.as_str())
    }

    #[test]
    fn fixture_title_and_count() {
        let (s, skipped) = parse_reporting(FIXTURE);
        assert_eq!(
            (s.title.as_str(), s.provider.as_str()),
            ("xmonad", "xmonad")
        );
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 53);
        assert!(s.binds.values().flatten().all(|b| b.action.is_empty()));
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    #[test]
    fn key_notation() {
        for (x, want) in [
            ("M4-b", "super+b"),
            ("M4-Shift-Return", "super+shift+Return"),
            ("M4-comma", "super+comma"),
            ("Print", "Print"),
            ("M4-slash", "super+slash"),
            ("Shift-M4-k", "super+shift+k"),
            ("C-M1-Delete", "ctrl+alt+Delete"),
            ("Mod4-S-minus", "super+shift+minus"),
            // keyToString format (xmonad-contrib 0.17.1+)
            ("M4-S-<Return>", "super+shift+Return"),
            ("M4--", "super+minus"),
            ("-", "minus"),
            ("M4-,", "super+comma"),
            ("M4-/", "super+slash"),
            ("<XF86AudioRaiseVolume>", "XF86AudioRaiseVolume"),
            ("<KP_Subtract>", "KP_Subtract"),
            ("lock-num-M4-x", "super+x"),
            ("M3-a", "mod3+a"),
            (
                "M1-M2-M3-M4-M5-S-C-a",
                "super+ctrl+alt+shift+mod2+mod3+mod5+a",
            ),
        ] {
            assert_eq!(key(x).as_deref(), Some(want), "{x}");
        }
        for bad in ["Hyper-x", "M4-", "", "Super-Hyper-k"] {
            assert_eq!(key(bad), None, "{bad}");
        }
    }

    #[test]
    fn last_entry_wins() {
        let s = parse(FIXTURE).unwrap();
        assert_eq!(
            one(&s, "super+m"),
            ("custom", "Example: overrides Focus the master")
        );
        assert_eq!(one(&s, "super+p").1, "Example: overrides Launch dmenu");
        assert_eq!(
            one(&s, "super+shift+k"),
            ("custom", "Example: overrides Swap up")
        );
        assert_eq!(one(&s, "super+x"), ("custom", "Example: open a browser"));
        assert_eq!(one(&s, "F9"), ("custom", "Example: take a screenshot"));
        let s = parse("x:\nM4-a old\n\ny:\nM4-a new\n").unwrap();
        assert_eq!(one(&s, "super+a"), ("y", "new"));
    }

    #[test]
    fn help_key_wins_over_a_later_duplicate() {
        let s = parse("M4-slash Search keybindings\n\nmine:\nM4-slash Something else\n").unwrap();
        assert_eq!(one(&s, "super+slash"), ("general", "Search keybindings"));
    }

    #[test]
    fn categories() {
        let s = parse(FIXTURE).unwrap();
        assert_eq!(
            one(&s, "super+shift+Return").0,
            "launching and killing programs"
        );
        assert_eq!(one(&s, "super+n").0, "changing layouts");
        assert_eq!(one(&s, "super+slash").0, "general");
        let screens: Vec<&str> = s.binds["switching screens"]
            .iter()
            .map(|b| b.key.as_str())
            .collect();
        assert_eq!(
            screens,
            [
                "super+w",
                "super+e",
                "super+r",
                "super+shift+w",
                "super+shift+e",
                "super+shift+r"
            ]
        );
    }

    #[test]
    fn new_format() {
        let (s, skipped) = parse_reporting(NEW_FORMAT);
        assert_eq!(one(&s, "super+shift+Return"), ("general", "sret"));
        assert_eq!(one(&s, "super+minus").1, "m4minus");
        assert_eq!(one(&s, "minus").1, "minus");
        assert_eq!(one(&s, "comma").1, "comma");
        assert_eq!(one(&s, "XF86AudioRaiseVolume").1, "vol");
        // A description ending in `:` is not a heading; only `head:` after a blank line is.
        assert_eq!(one(&s, "super+a"), ("general", "ends with colon:"));
        assert_eq!(one(&s, "super+h"), ("head", "after heading"));
        assert_eq!(one(&s, "super+c").1, "");
        assert_eq!(one(&s, "super+e").1, "\u{dc}n\u{ef}code \u{2713}");
        assert_eq!(one(&s, "super+ctrl+alt+shift+mod2+mod3+mod5+a").1, "all");
        // `multi\nline`: the continuation `line` is read as a bare keysym; harmless, and nothing is skipped.
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    #[test]
    fn unknown_modifier_is_skipped_and_reported() {
        let (s, skipped) = parse_reporting("Hyper-x Foo\nM4-a Bar\n");
        let all: Vec<&Bind> = s.binds.values().flatten().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(
            (all[0].key.as_str(), all[0].desc.as_str()),
            ("super+a", "Bar")
        );
        assert_eq!(skipped, ["Hyper-x Foo"]);
    }

    #[test]
    fn edge_cases() {
        // Heading with no bindings, an empty description, CRLF, trailing spaces, no final newline.
        let s = parse("empty:\n\nfull: \r\n\nM4-q\r\nM4-r  Run r \nnext:\nM4-x Run x").unwrap();
        assert!(!s.binds.contains_key("empty"));
        assert_eq!(one(&s, "super+q"), ("full", ""));
        assert_eq!(one(&s, "super+r"), ("full", "Run r"));
        // `next:` follows a binding, not a blank line: a binding line (an odd one), not a heading.
        assert_eq!(one(&s, "super+x").0, "full");
        assert!(parse("").unwrap().binds.is_empty());
    }

    #[test]
    fn dump_path_rules() {
        let p = |x: Option<&str>, h: Option<&str>| {
            dump_path(x.map(Into::into), h.map(Into::into)).map(|p| p.display().to_string())
        };
        assert_eq!(
            p(Some("/c"), Some("/h")).as_deref(),
            Some("/c/keybind-find/xmonad.keys")
        );
        assert_eq!(
            p(None, Some("/h")).as_deref(),
            Some("/h/.cache/keybind-find/xmonad.keys")
        );
        assert_eq!(
            p(Some(""), Some("/h")).as_deref(),
            Some("/h/.cache/keybind-find/xmonad.keys")
        );
        assert_eq!(
            p(Some("rel"), Some("/h")).as_deref(),
            Some("/h/.cache/keybind-find/xmonad.keys")
        );
        assert_eq!(p(None, None), None);
        assert_eq!(p(None, Some("")), None);
    }
}
