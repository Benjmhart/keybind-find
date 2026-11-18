//! niri: its binds from the config file niri reads (KDL v1, through `kdl_v1`), with `include`s read in place and
//! later `binds` blocks replacing earlier binds of the same key, as niri merges them. Line references are to niri
//! v26.04 (commit 8ed0da4).

use super::kdl_v1;
use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use kdl::{KdlNode, KdlValue};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

const SYSTEM: &str = "/etc/niri/config.kdl";

/// niri-config/src/lib.rs:67: an include at this depth is refused (the main file is depth 0).
const RECURSION_LIMIT: usize = 10;

/// keybind-find's own bounds (niri has none): the largest file read, and the most includes followed in one run, so a
/// huge file or a wide include tree can't hold up the picker. A real config is a few KiB in a handful of files.
const MAX_FILE: u64 = 4 << 20;
const MAX_INCLUDES: usize = 256;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let (sheet, notes) = sheet_from(
        &|name: &str| std::env::var(name).ok().filter(|v| !v.is_empty()),
        Path::new(SYSTEM),
    )?;
    if !notes.is_empty() {
        eprintln!("keybind-find: niri: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// The sheet, and the notes for its one stderr line. `system` is niri's system config path (a test seam).
fn sheet_from(env: Env, system: &Path) -> anyhow::Result<(Sheet, Vec<String>)> {
    let paths = config_paths(env, system);
    let path = paths.iter().find(|p| p.is_file()).with_context(|| {
        let tried: Vec<_> = paths.iter().map(|p| p.display().to_string()).collect();
        format!("no niri config (looked for {})", tried.join(", "))
    })?;
    let mut state = State::default();
    let text = read_file(path).with_context(|| format!("reading {}", path.display()))?;
    load(path, &text, env, 0, &mut vec![canonical(path)], &mut state)?;
    let mod_key = state.mod_key.clone().unwrap_or_else(|| "Super".into());
    let mut sheet = Sheet {
        title: "niri".into(),
        provider: "niri".into(),
        binds: Default::default(),
    };
    let mut left_out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut shadowed = 0;
    for raw in state.binds {
        let Some(key) = key(&raw.name, &mod_key) else {
            left_out.push(raw.name);
            continue;
        };
        // Two different niri keys can be one chord here (`Mod+T` and `Super+T` with mod-key Super). niri's
        // `find_configured_bind` (src/input/mod.rs:4443-4472) runs the first in its list: both are listed, and noted.
        // Case-insensitively: niri reads key names that way (`Return` and `return` are one key).
        if !seen.insert(key.to_ascii_lowercase()) {
            shadowed += 1;
        }
        sheet
            .binds
            .entry(raw.action.clone())
            .or_default()
            .push(Bind {
                key,
                desc: raw.desc,
                action: raw.action,
                when: Default::default(),
                run: Vec::new(),
            });
    }
    if shadowed > 0 {
        state.notes.push(format!(
            "{shadowed} bind(s) share a key with an earlier one; niri runs only the first"
        ));
    }
    if !left_out.is_empty() {
        state.notes.push(format!(
            "left out {} bind(s) keybind-find can't send (mouse, wheel or touchpad, Mod3, or an unknown modifier): {}",
            left_out.len(),
            left_out.join(", ")
        ));
    }
    Ok((sheet, state.notes))
}

/// The file niri reads (src/main.rs:327-360): `NIRI_CONFIG` when set (empty counts as unset), else the user's
/// (`ProjectDirs` config dir: `$XDG_CONFIG_HOME` when absolute, else `~/.config`), else the system one. niri's
/// `--config` isn't visible to keybind-find, and niri removes `NIRI_CONFIG` from its children's environment
/// (src/main.rs:150), so only a `NIRI_CONFIG` in keybind-find's own environment is followed.
fn config_paths(env: Env, system: &Path) -> Vec<PathBuf> {
    if let Some(explicit) = env("NIRI_CONFIG").filter(|v| !v.is_empty()) {
        return vec![PathBuf::from(explicit)];
    }
    let xdg = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")));
    let mut paths: Vec<_> = xdg.map(|x| x.join("niri/config.kdl")).into_iter().collect();
    paths.push(system.to_path_buf());
    paths
}

#[derive(Default)]
struct State {
    binds: Vec<RawBind>,
    mod_key: Option<String>,
    notes: Vec<String>,
    /// Includes followed so far (`MAX_INCLUDES`).
    includes: usize,
}

/// A regular file's text (a FIFO or a device would block or never end), up to `MAX_FILE`.
fn read_file(path: &Path) -> anyhow::Result<String> {
    use std::io::Read as _;
    let md = std::fs::metadata(path)?;
    anyhow::ensure!(md.is_file(), "not a regular file");
    let too_big = || anyhow::anyhow!("bigger than {} MiB", MAX_FILE >> 20);
    if md.len() > MAX_FILE {
        return Err(too_big());
    }
    let mut text = String::new();
    // `take` still bounds a file that grows while it is read.
    std::fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_FILE {
        return Err(too_big());
    }
    Ok(text)
}

/// A bind as written; its key is translated once the last `mod-key` is known.
struct RawBind {
    name: String,
    /// What niri compares binds by: the trigger (case-insensitive) and the modifier set as written.
    same: (String, BTreeSet<String>),
    desc: String,
    action: String,
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Reads `path`'s `text` into `state`: its binds (merged), `input { mod-key }`, and its includes in place. A file
/// that doesn't parse, included or not, is an error: niri doesn't load that config, and the provider is dropped.
fn load(
    path: &Path,
    text: &str,
    env: Env,
    depth: usize,
    stack: &mut Vec<PathBuf>,
    state: &mut State,
) -> anyhow::Result<()> {
    let doc = kdl_v1::parse(text).with_context(|| path.display().to_string())?;
    let label = |node: &KdlNode| format!("{}:{}", path.display(), kdl_v1::line(text, node));
    let mut saw_binds = false;
    for node in doc.nodes() {
        match node.name().value() {
            // ponytail: `recent-windows { binds { … } }` (Alt+Tab and its built-in defaults) isn't read: AC #2 lists
            // the top-level `binds` only. Read it if someone misses Alt+Tab in the list.
            "binds" => {
                // A second `binds` node in one file is an error in niri ("duplicate node"); merged here.
                if saw_binds {
                    state
                        .notes
                        .push(format!("{}: a second binds block (merged)", label(node)));
                }
                saw_binds = true;
                let part = binds(node, &label, &mut state.notes);
                // niri-config/src/lib.rs:219-230: a later block's bind replaces any earlier one with the same key.
                state
                    .binds
                    .retain(|b| !part.iter().any(|n| n.same == b.same));
                state.binds.extend(part);
            }
            "input" => {
                let children = node.children().map(|c| c.nodes()).unwrap_or_default();
                for m in children.iter().filter(|n| n.name().value() == "mod-key") {
                    if let Some(v) = first_string(m) {
                        state.mod_key = Some(v.to_string());
                    }
                }
            }
            "include" => include(node, path, env, depth, stack, state, &label(node))?,
            _ => {}
        }
    }
    Ok(())
}

/// One `include "path" [optional=true]` (niri-config/src/lib.rs:297-445): `~` is the home directory, anything else
/// is relative to the including file's folder. A missing optional file is skipped silently; a missing file, a file
/// already being read (a loop), or one `RECURSION_LIMIT` deep is a note and skipped, where niri fails the config.
fn include(
    node: &KdlNode,
    path: &Path,
    env: Env,
    depth: usize,
    stack: &mut Vec<PathBuf>,
    state: &mut State,
    label: &str,
) -> anyhow::Result<()> {
    let Some(target) = first_string(node) else {
        state
            .notes
            .push(format!("{label}: an include with no path"));
        return Ok(());
    };
    let optional = node.entries().iter().any(|e| {
        e.name().is_some_and(|n| n.value() == "optional") && *e.value() == KdlValue::Bool(true)
    });
    // `Path::strip_prefix` matches whole components, as niri's does: `~/x` and `~`, not `~x`.
    let file = match Path::new(target).strip_prefix("~") {
        Ok(rest) => match env("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => {
                state
                    .notes
                    .push(format!("{label}: include {target}: HOME is not set"));
                return Ok(());
            }
        },
        Err(_) => path.parent().unwrap_or(Path::new(".")).join(target),
    };
    if depth + 1 >= RECURSION_LIMIT {
        state.notes.push(format!(
            "{label}: include {target} skipped: includes can't be {RECURSION_LIMIT} levels deep"
        ));
        return Ok(());
    }
    let key = canonical(&file);
    if stack.contains(&key) {
        state.notes.push(format!(
            "{label}: include {target} skipped: it includes itself"
        ));
        return Ok(());
    }
    if optional && std::fs::metadata(&file).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    if state.includes >= MAX_INCLUDES {
        if state.includes == MAX_INCLUDES {
            state.notes.push(format!(
                "{label}: more than {MAX_INCLUDES} includes; the rest are skipped"
            ));
            state.includes += 1;
        }
        return Ok(());
    }
    state.includes += 1;
    let text = match read_file(&file) {
        Ok(text) => text,
        Err(e) => {
            state.notes.push(format!(
                "{label}: include {target} skipped: reading {}: {e:#}",
                file.display()
            ));
            return Ok(());
        }
    };
    stack.push(key);
    let read = load(&file, &text, env, depth + 1, stack, state);
    stack.pop();
    read
}

/// The binds of one `binds { }` block (niri-config/src/binds.rs:767-800, 816-870): each child's name is its key,
/// its first child the action; `hotkey-overlay-title` a string (else the action as written). A key repeated in the
/// block is an error in niri: the first is kept and the repeat noted.
fn binds(
    node: &KdlNode,
    label: &dyn Fn(&KdlNode) -> String,
    notes: &mut Vec<String>,
) -> Vec<RawBind> {
    let mut out: Vec<RawBind> = Vec::new();
    for b in node.children().map(|c| c.nodes()).unwrap_or_default() {
        let name = b.name().value();
        let Some(action) = b.children().and_then(|c| c.nodes().first()) else {
            notes.push(format!("{}: {name} has no action", label(b)));
            continue;
        };
        // More than one action is an error in niri.
        if b.children().is_some_and(|c| c.nodes().len() > 1) {
            notes.push(format!(
                "{}: {name} has more than one action (first shown)",
                label(b)
            ));
        }
        let same = same_key(name);
        if out.iter().any(|o| o.same == same) {
            notes.push(format!(
                "{}: {name} is bound twice in one block (first kept)",
                label(b)
            ));
            continue;
        }
        let title = b
            .entries()
            .iter()
            .find(|e| {
                e.name()
                    .is_some_and(|n| n.value() == "hotkey-overlay-title")
            })
            .and_then(|e| e.value().as_string());
        out.push(RawBind {
            name: name.to_string(),
            same,
            // A title that is empty once its markup is gone says nothing: the action instead.
            desc: title
                .map(plain)
                .filter(|t| t.chars().any(|c| !c.is_whitespace() && !c.is_control()))
                .unwrap_or_else(|| written(action)),
            action: action.name().value().to_string(),
        });
    }
    out
}

/// The action as written: its name, then its arguments and properties (`spawn alacritty`, `screenshot
/// show-pointer=false`).
fn written(action: &KdlNode) -> String {
    let mut parts = vec![action.name().value().to_string()];
    for e in action.entries() {
        let v = match e.value() {
            KdlValue::String(s) => s.clone(),
            KdlValue::Integer(i) => i.to_string(),
            KdlValue::Float(f) => f.to_string(),
            KdlValue::Bool(b) => b.to_string(),
            KdlValue::Null => "null".into(),
        };
        parts.push(match e.name() {
            Some(n) => format!("{}={v}", n.value()),
            None => v,
        });
    }
    parts.join(" ")
}

/// Pango markup as text: known tags dropped and entities decoded (the five XML ones, `&#N;`, `&#xN;`). What Pango
/// can't parse is shown whole, as written, as niri's overlay does (src/ui/hotkey_overlay.rs:143-146, 412-418).
fn plain(markup: &str) -> String {
    markup_text(markup).unwrap_or_else(|| markup.to_string())
}

/// `markup` as Pango shows it, `None` where Pango would fail: an unclosed `<`, a tag it doesn't know, a bare `&`, an
/// unknown or invalid entity. ponytail: tag nesting and attributes aren't checked.
fn markup_text(markup: &str) -> Option<String> {
    const TAGS: &[&str] = &[
        "b", "big", "i", "s", "sub", "sup", "small", "tt", "u", "span",
    ];
    let mut out = String::new();
    let mut rest = markup;
    while let Some(at) = rest.find(['<', '&']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with('<') {
            let end = rest.find('>')?;
            let tag = rest[1..end].trim_start_matches('/');
            let name = tag.split([' ', '\t', '\n', '/']).next().unwrap_or("");
            if !TAGS.contains(&name) {
                return None;
            }
            rest = &rest[end + 1..];
        } else {
            let end = rest.find(';')?;
            out.push(entity(&rest[1..end])?);
            rest = &rest[end + 1..];
        }
    }
    out.push_str(rest);
    Some(out)
}

/// One entity's character, as GMarkup reads it: a lower-case `x` for hex, digits only, never NUL or a surrogate.
fn entity(name: &str) -> Option<char> {
    let named = match name {
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "amp" => Some('&'),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let number = name.strip_prefix('#')?;
    let (digits, radix) = match number.strip_prefix('x') {
        Some(hex) => (hex, 16),
        None => (number, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    char::from_u32(u32::from_str_radix(digits, radix).ok()?).filter(|&c| c != '\0')
}

fn first_string(node: &KdlNode) -> Option<&str> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
}

/// niri's `Key` equality (niri-config/src/binds.rs:943-975): the trigger, matched ignoring case, and the modifier
/// set, each spelling folded to its bit, `Mod` apart from `Super`. ponytail: the trigger is compared by name, not by
/// keysym, so aliases of one keysym (`Prior`, `Page_Up`) aren't merged, and lower-casing merges `XF86Screensaver`
/// with `XF86ScreenSaver`, which niri keeps apart (binds.rs:1005-1030); fold through a keysym table if that matters.
fn same_key(name: &str) -> (String, BTreeSet<String>) {
    let mut parts: Vec<&str> = name.split('+').collect();
    let trigger = parts.pop().unwrap_or_default().to_ascii_lowercase();
    let mods = parts
        .iter()
        .map(|p| {
            let p = p.trim().to_ascii_lowercase();
            match p.as_str() {
                "mod" => p,
                // Mod3 has no notation (keybind-find leaves it out), but niri's bit is one for both spellings.
                "iso_level5_shift" => "mod3".into(),
                _ => modifier(&p).map_or(p.clone(), str::to_string),
            }
        })
        .collect();
    (trigger, mods)
}

/// A modifier as niri spells it (ignoring case) in keybind-find's notation; `None` for Mod3/ISO_Level5_Shift
/// (wtype has no mod3) and anything niri doesn't know. niri-config/src/binds.rs:952-975, input.rs:439-450.
fn modifier(m: &str) -> Option<&'static str> {
    match m.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some("ctrl"),
        "shift" => Some("shift"),
        "alt" => Some("alt"),
        "super" | "win" => Some("super"),
        "iso_level3_shift" | "mod5" => Some("mod5"),
        _ => None,
    }
}

/// niri-config/src/binds.rs:977-1003: these triggers are mouse buttons, wheel and touchpad scrolls, not keys.
const TRIGGERS: &[&str] = &[
    "MouseLeft",
    "MouseRight",
    "MouseMiddle",
    "MouseBack",
    "MouseForward",
    "WheelScrollDown",
    "WheelScrollUp",
    "WheelScrollLeft",
    "WheelScrollRight",
    "TouchpadScrollDown",
    "TouchpadScrollUp",
    "TouchpadScrollLeft",
    "TouchpadScrollRight",
];

/// A bind's name (`Mod+Shift+Slash`) in keybind-find's notation (`super+shift+Slash`), `Mod` being `mod_key`; `None`
/// for one keybind-find can't send. niri resolves the key name ignoring case (`keysym_from_name(…,
/// KEYSYM_CASE_INSENSITIVE)`, binds.rs:1004); like sway's provider, a single letter is lower-cased and any other name
/// kept.
fn key(name: &str, mod_key: &str) -> Option<String> {
    let mut parts: Vec<&str> = name.split('+').collect();
    let keysym = parts.pop()?;
    if keysym.is_empty() || TRIGGERS.iter().any(|t| t.eq_ignore_ascii_case(keysym)) {
        return None;
    }
    let mods = parts
        .iter()
        .map(|p| match p.trim().eq_ignore_ascii_case("mod") {
            true => modifier(mod_key),
            false => modifier(p),
        })
        .collect::<Option<Vec<_>>>()?;
    let lower = keysym.to_ascii_lowercase();
    let keysym = if keysym.len() == 1 && keysym.chars().all(|c| c.is_ascii_alphabetic()) {
        lower.as_str()
    } else {
        keysym
    };
    Some(chord(&mods, keysym))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp directory, removed on drop.
    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("kbf-niri-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn write(d: &Path, name: &str, text: &str) -> PathBuf {
        let p = d.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    /// The sheet of the config at `path` (as `NIRI_CONFIG`), with `HOME` set to `home`.
    fn read(path: &Path, home: &Path) -> anyhow::Result<(Sheet, Vec<String>)> {
        let (path, home) = (path.display().to_string(), home.display().to_string());
        sheet_from(
            &|n: &str| match n {
                "NIRI_CONFIG" => Some(path.clone()),
                "HOME" => Some(home.clone()),
                _ => None,
            },
            Path::new("/nonexistent/niri/config.kdl"),
        )
    }

    fn text(name: &str, kdl: &str) -> anyhow::Result<(Sheet, Vec<String>)> {
        let d = dir(name);
        read(&write(&d.0, "config.kdl", kdl), &d.0)
    }

    /// (category, key, desc), in sheet order.
    fn rows(sheet: &Sheet) -> Vec<(String, String, String)> {
        sheet
            .binds
            .iter()
            .flat_map(|(c, bs)| {
                bs.iter()
                    .map(move |b| (c.clone(), b.key.clone(), b.desc.clone()))
            })
            .collect()
    }

    fn desc<'a>(sheet: &'a Sheet, key: &str) -> Vec<&'a str> {
        let all = sheet.binds.values().flatten();
        all.filter(|b| b.key == key)
            .map(|b| b.desc.as_str())
            .collect()
    }

    #[test]
    fn the_default_config() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/niri/default-config.kdl");
        let (sheet, notes) = read(&fixture, Path::new("/nonexistent")).unwrap();
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str()),
            ("niri", "niri")
        );
        let rows = rows(&sheet);
        let has = |c: &str, k: &str, d: &str| rows.contains(&(c.into(), k.into(), d.into()));
        assert!(
            has("spawn", "super+t", "Open a Terminal: alacritty"),
            "{rows:?}"
        );
        assert!(has(
            "spawn-sh",
            "super+alt+s",
            "spawn-sh pkill orca || exec orca"
        ));
        assert!(has(
            "show-hotkey-overlay",
            "super+shift+Slash",
            "show-hotkey-overlay"
        ));
        assert!(has("focus-column-left", "super+Left", "focus-column-left"));
        assert!(has(
            "move-column-to-workspace",
            "super+ctrl+1",
            "move-column-to-workspace 1"
        ));
        assert!(rows
            .iter()
            .any(|(c, k, _)| c == "spawn-sh" && k == "XF86AudioRaiseVolume"));
        // The 12 wheel binds are left out and counted in the one note; nothing else is noted.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("left out 12 bind(s)"), "{notes:?}");
        assert!(notes[0].contains("Mod+WheelScrollDown"));
        assert!(!rows.iter().any(|(_, k, _)| k.contains("Wheel")));
    }

    #[test]
    fn kdl_through_the_crate() {
        let (sheet, notes) = text(
            "kdl",
            r##"
/-binds { Mod+X { spawn "ghost"; }; }
binds {
    /-Mod+A { spawn "gone"; }
    Mod+B { /-spawn "gone"; spawn "kept"; }
    Mod+C hotkey-overlay-title=null { spawn "c"; }
    Mod+D hotkey-overlay-title=r#"a "raw" title"# { spawn "d"; }
    Mod+E hotkey-overlay-title="<b>Toggle</b> <span foreground='red'>x</span> &amp; &lt;go&gt; &quot;q&quot; &apos;a&apos; &amp;lt;" { spawn "e"; }
    Mod+F repeat=false cooldown-ms=150 allow-when-locked=true allow-inhibiting=false { spawn "f" "--flag"; }
    Mod+G \
        hotkey-overlay-title="continued" { spawn "g"; }
    Mod+H { screenshot show-pointer=false; }
    Mod+I /-{ close-window; }
}
"##,
        )
        .unwrap();
        let rows = rows(&sheet);
        assert_eq!(
            rows,
            [
                ("screenshot", "super+h", "screenshot show-pointer=false"),
                ("spawn", "super+b", "spawn kept"),
                ("spawn", "super+c", "spawn c"),
                ("spawn", "super+d", r#"a "raw" title"#),
                ("spawn", "super+e", r#"Toggle x & <go> "q" 'a' &lt;"#),
                ("spawn", "super+f", "spawn f --flag"),
                ("spawn", "super+g", "continued"),
            ]
            .map(|(c, k, d)| (c.to_string(), k.to_string(), d.to_string()))
        );
        assert_eq!(sheet.binds["spawn"][0].action, "spawn");
        // A bind whose action is slashdashed has none: noted, not listed.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].ends_with("Mod+I has no action"), "{notes:?}");
        // A commented-out binds block: an empty sheet.
        let (sheet, notes) = text("kdl-empty", "/-binds { Mod+T { spawn \"a\"; }; }\n").unwrap();
        assert!(sheet.binds.is_empty() && notes.is_empty());
        // An unterminated string: one error naming the file and the line.
        let d = dir("kdl-bad");
        let path = write(&d.0, "config.kdl", "binds {\n    Mod+T { spawn \"a; }\n}\n");
        let err = format!("{:#}", read(&path, &d.0).unwrap_err());
        assert!(
            err.starts_with(&format!("{}: line ", path.display())),
            "{err}"
        );
    }

    #[test]
    fn keys() {
        let k = |name: &str| key(name, "Super");
        assert_eq!(k("CTRL+X").as_deref(), Some("ctrl+x"));
        assert_eq!(k("Control+Shift+x").as_deref(), Some("ctrl+shift+x"));
        assert_eq!(k("win+T").as_deref(), Some("super+t"));
        assert_eq!(k("ALT+t").as_deref(), Some("alt+t"));
        assert_eq!(k("ISO_Level3_Shift+a").as_deref(), Some("mod5+a"));
        assert_eq!(k("Mod5+a").as_deref(), Some("mod5+a"));
        assert_eq!(k("Mod+Shift+Slash").as_deref(), Some("super+shift+Slash"));
        assert_eq!(k("Mod+Page_Down").as_deref(), Some("super+Page_Down"));
        assert_eq!(
            k("XF86AudioRaiseVolume").as_deref(),
            Some("XF86AudioRaiseVolume")
        );
        assert_eq!(k("Mod+Super+T").as_deref(), Some("super+t"));
        assert_eq!(key("Mod+T", "Alt").as_deref(), Some("alt+t"));
        assert_eq!(key("Mod+T", "ctrl").as_deref(), Some("ctrl+t"));
        assert_eq!(key("Mod+T", "Hyper"), None);
        for out in [
            "Mod3+X",
            "ISO_Level5_Shift+X",
            "Mod+MouseLeft",
            "Mod+mouseback",
            "Mod+TouchpadScrollUp",
            "WheelScrollDown",
            "Hyper+X",
            "Mod+",
        ] {
            assert_eq!(k(out), None, "{out}");
        }
        // `Mod` follows the last `mod-key`; the left-out binds are counted in one note.
        let (sheet, notes) = text(
            "keys",
            r#"
input { mod-key "Ctrl"; }
binds {
    Mod+T { spawn "t"; }
    Mod3+X { spawn "x"; }
    Mod+MouseLeft { spawn "m"; }
    Mod+TouchpadScrollUp { spawn "s"; }
    Hyper+X { spawn "h"; }
}
input { mod-key "Alt"; }
"#,
        )
        .unwrap();
        assert_eq!(desc(&sheet, "alt+t"), ["spawn t"]);
        assert_eq!(rows(&sheet).len(), 1);
        assert_eq!(
            notes,
            ["left out 4 bind(s) keybind-find can't send (mouse, wheel or touchpad, Mod3, or an unknown \
              modifier): Mod3+X, Mod+MouseLeft, Mod+TouchpadScrollUp, Hyper+X"]
        );
    }

    #[test]
    fn includes() {
        let d = dir("inc");
        let root = &d.0;
        // The wiki's example: an included file's bind overridden by the including file's own.
        write(
            root,
            "binds.kdl",
            r#"binds { Mod+T { spawn "alacritty"; }; Mod+Q { close-window; }; Super+T { spawn "super"; }; }
input { mod-key "Super"; }"#,
        );
        write(root, "home.kdl", r#"binds { Mod+H { spawn "home"; }; }"#);
        write(
            root,
            "self.kdl",
            r#"include "self.kdl"
binds { Mod+S { spawn "self"; }; }"#,
        );
        let config = write(
            root,
            "niri/config.kdl",
            r#"
include "../binds.kdl"
include "~/home.kdl"
include optional=true "gone.kdl"
include "missing.kdl"
include "../self.kdl"
binds { Mod+T { spawn "foot"; }; }
"#,
        );
        let (sheet, notes) = read(&config, root).unwrap();
        // `Mod+T` is replaced; `Super+T` is another key in niri, kept.
        assert_eq!(desc(&sheet, "super+t"), ["spawn super", "spawn foot"]);
        assert_eq!(desc(&sheet, "super+q"), ["close-window"]);
        assert_eq!(desc(&sheet, "super+h"), ["spawn home"]);
        assert_eq!(desc(&sheet, "super+s"), ["spawn self"]);
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(
            notes[0].contains("include missing.kdl skipped: reading"),
            "{notes:?}"
        );
        assert!(
            notes[1].ends_with("include self.kdl skipped: it includes itself"),
            "{notes:?}"
        );
        // Both are listed, but niri only ever runs the first.
        assert_eq!(
            notes[2],
            "1 bind(s) share a key with an earlier one; niri runs only the first"
        );
        // A chain 11 files deep: the 11th is skipped with one note, the other ten listed.
        let d = dir("deep");
        for i in 0..11 {
            write(
                &d.0,
                &format!("{i}.kdl"),
                &format!(
                    "include \"{}.kdl\"\nbinds {{ Mod+F{i} {{ spawn \"{i}\"; }}; }}\n",
                    i + 1
                ),
            );
        }
        let (sheet, notes) = read(&d.0.join("0.kdl"), &d.0).unwrap();
        assert_eq!(rows(&sheet).len(), 10);
        assert!(desc(&sheet, "super+F10").is_empty());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with("include 10.kdl skipped: includes can't be 10 levels deep"),
            "{notes:?}"
        );
        // A duplicate key in one block keeps the first; a second binds block in one file merges.
        let (sheet, notes) = text(
            "dup",
            "binds { Mod+A { spawn \"1\"; }; mod+a { spawn \"2\"; }; }\nbinds { Mod+B { spawn \"b\"; }; }\n",
        )
        .unwrap();
        assert_eq!(desc(&sheet, "super+a"), ["spawn 1"]);
        assert_eq!(desc(&sheet, "super+b"), ["spawn b"]);
        assert!(
            notes[0].ends_with("mod+a is bound twice in one block (first kept)"),
            "{notes:?}"
        );
        assert!(
            notes[1].ends_with("a second binds block (merged)"),
            "{notes:?}"
        );
        // An included file that doesn't parse: niri loads nothing, so the provider is dropped.
        let d = dir("inc-bad");
        write(&d.0, "bad.kdl", "binds {\n");
        let config = write(&d.0, "config.kdl", "include \"bad.kdl\"\n");
        let err = format!("{:#}", read(&config, &d.0).unwrap_err());
        assert!(err.contains("bad.kdl: line "), "{err}");
    }

    #[test]
    fn merges_by_niri_key() {
        // Spelling and case don't make another key: the later bind replaces the earlier, one row each.
        let d = dir("merge");
        write(&d.0, "a.kdl", "binds { Ctrl+Shift+T { spawn \"a\"; }; Win+X { spawn \"w\"; }; Mod3+Q { spawn \"3\"; }; }");
        let config = write(
            &d.0,
            "config.kdl",
            "include \"a.kdl\"\nbinds { control+SHIFT+t { spawn \"b\"; }; Super+X { spawn \"s\"; }; ISO_Level5_Shift+Q { spawn \"5\"; }; }\n",
        );
        let (sheet, notes) = read(&config, &d.0).unwrap();
        assert_eq!(desc(&sheet, "ctrl+shift+t"), ["spawn b"]);
        assert_eq!(desc(&sheet, "super+x"), ["spawn s"]);
        // The Mod3 bind is left out once: the two spellings are one key in niri.
        assert_eq!(
            notes,
            ["left out 1 bind(s) keybind-find can't send (mouse, wheel or touchpad, Mod3, or an unknown \
              modifier): ISO_Level5_Shift+Q"]
        );
    }

    #[test]
    fn titles_as_text() {
        assert_eq!(plain("a &#65;&#x42; b"), "a AB b");
        assert_eq!(plain("<b>x</b> <span foreground='red'>y</span>"), "x y");
        assert_eq!(plain("&amp;lt; &quot;&apos;&gt;"), "&lt; \"'>");
        // What Pango can't parse is shown as written.
        for raw in [
            "&#X43;",
            "&#0;",
            "&#xD800;",
            "&#+65;",
            "&#;",
            "&nope;",
            "a & b",
            "1 < 2",
            "<foo>x</foo>",
            "a &amp b",
        ] {
            assert_eq!(plain(raw), raw);
        }
        let (sheet, notes) = text(
            "titles",
            "binds {\n    Mod+A hotkey-overlay-title=\"<b> </b>\" { spawn \"a\"; }\n    Mod+B hotkey-overlay-title=\"\" { spawn \"b\"; }\n    Mod+C { set-column-width 0.5 null; }\n    Mod+D hotkey-overlay-title=\"\\u{7} \" { spawn \"d\"; }\n    Mod+Return { spawn \"r\"; }\n    Super+return { spawn \"s\"; }\n    Mod+E { spawn \"e1\"; spawn \"e2\"; }\n}\n",
        )
        .unwrap();
        // Blank once its markup is gone: the action instead.
        assert_eq!(desc(&sheet, "super+a"), ["spawn a"]);
        assert_eq!(desc(&sheet, "super+b"), ["spawn b"]);
        assert_eq!(desc(&sheet, "super+c"), ["set-column-width 0.5 null"]);
        // Only control characters and spaces: blank too.
        assert_eq!(desc(&sheet, "super+d"), ["spawn d"]);
        // The first action is shown, and the others noted.
        assert_eq!(desc(&sheet, "super+e"), ["spawn e1"]);
        assert!(
            notes[0].ends_with("Mod+E has more than one action (first shown)"),
            "{notes:?}"
        );
        // `Return` and `return` are one key in niri: both listed, noted as sharing it.
        assert_eq!(desc(&sheet, "super+Return"), ["spawn r"]);
        assert_eq!(desc(&sheet, "super+return"), ["spawn s"]);
        assert_eq!(
            notes[1],
            "1 bind(s) share a key with an earlier one; niri runs only the first"
        );
    }

    #[test]
    fn include_edges() {
        let d = dir("edges");
        std::fs::create_dir_all(d.0.join("adir")).unwrap();
        write(&d.0, "late.kdl", "input { mod-key \"Ctrl\"; }\n");
        let config = write(
            &d.0,
            "config.kdl",
            "include \"config.kdl\"\ninclude optional=true \"adir\"\ninclude optional=true \"late.kdl/x\"\ninclude\ninclude \"late.kdl\"\ninput { mod-key \"Alt\"; }\nbinds { Mod+T { spawn \"t\"; }; }\n",
        );
        let (sheet, notes) = read(&config, &d.0).unwrap();
        // The last mod-key read wins, an included one included.
        assert_eq!(desc(&sheet, "alt+t"), ["spawn t"]);
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(
            notes[0].ends_with("include config.kdl skipped: it includes itself"),
            "{notes:?}"
        );
        assert!(notes[1].ends_with("not a regular file"), "{notes:?}");
        // Optional covers a missing file only: a path through a file (ENOTDIR) is noted.
        assert!(
            notes[2].contains("include late.kdl/x skipped: reading"),
            "{notes:?}"
        );
        assert!(notes[3].ends_with("an include with no path"), "{notes:?}");
        // The included mod-key wins when it comes last.
        let config = write(
            &d.0,
            "c2.kdl",
            "input { mod-key \"Alt\"; }\ninclude \"late.kdl\"\nbinds { Mod+T { spawn \"t\"; }; }\n",
        );
        let (sheet, _) = read(&config, &d.0).unwrap();
        assert_eq!(desc(&sheet, "ctrl+t"), ["spawn t"]);
        // `~/` with no HOME: a note, the rest listed.
        let path = write(
            &d.0,
            "c3.kdl",
            "include \"~/x.kdl\"\nbinds { Mod+T { spawn \"t\"; }; }\n",
        )
        .display()
        .to_string();
        let (sheet, notes) = sheet_from(
            &|n: &str| (n == "NIRI_CONFIG").then(|| path.clone()),
            Path::new("/nonexistent"),
        )
        .unwrap();
        assert_eq!(desc(&sheet, "super+t"), ["spawn t"]);
        assert!(
            notes[0].ends_with("include ~/x.kdl: HOME is not set"),
            "{notes:?}"
        );
        // A file over the limit is skipped with a note.
        let big = d.0.join("big.kdl");
        // Not UTF-8 either: the size is what's reported, from the metadata, before any reading.
        std::fs::write(&big, vec![0xff; MAX_FILE as usize + 1]).unwrap();
        let config = write(&d.0, "c4.kdl", "include \"big.kdl\"\n");
        let (_, notes) = read(&config, &d.0).unwrap();
        assert!(notes[0].ends_with("bigger than 4 MiB"), "{notes:?}");
        // Wide trees stop at MAX_INCLUDES, with one note.
        write(&d.0, "leaf.kdl", "\n");
        let config = write(
            &d.0,
            "c5.kdl",
            &"include \"leaf.kdl\"\n".repeat(MAX_INCLUDES + 3),
        );
        let (_, notes) = read(&config, &d.0).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with("more than 256 includes; the rest are skipped"),
            "{notes:?}"
        );
    }

    #[test]
    fn config_path() {
        let paths = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            config_paths(
                &|n: &str| vars.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()),
                Path::new("/etc/niri/config.kdl"),
            )
        };
        let p = |s: &[&str]| s.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            paths(&[("NIRI_CONFIG", "/x/n.kdl"), ("HOME", "/h")]),
            p(&["/x/n.kdl"])
        );
        assert_eq!(
            paths(&[("NIRI_CONFIG", ""), ("HOME", "/h")]),
            p(&["/h/.config/niri/config.kdl", "/etc/niri/config.kdl"])
        );
        assert_eq!(
            paths(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/h")]),
            p(&["/xdg/niri/config.kdl", "/etc/niri/config.kdl"])
        );
        // A relative XDG_CONFIG_HOME doesn't count.
        assert_eq!(
            paths(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")]),
            p(&["/h/.config/niri/config.kdl", "/etc/niri/config.kdl"])
        );
        // Only the system file: it is read.
        let d = dir("path");
        let system = write(
            &d.0,
            "etc/config.kdl",
            "binds { Mod+T { spawn \"etc\"; }; }",
        );
        let home = d.0.join("home").display().to_string();
        let env = |n: &str| (n == "HOME").then(|| home.clone());
        let (sheet, _) = sheet_from(&env, &system).unwrap();
        assert_eq!(desc(&sheet, "super+t"), ["spawn etc"]);
        // None: one error naming every path tried.
        let err = sheet_from(&env, &d.0.join("none.kdl"))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "no niri config (looked for {home}/.config/niri/config.kdl, {})",
                d.0.join("none.kdl").display()
            )
        );
    }
}
