//! Konsole: its own shortcuts, Konsole 26.08.2's defaults pinned (`data/konsole/`) with what KXMLGUI applies on top:
//! the user's `konsoleui.rc` and `sessionui.rc`, the shortcut scheme and the profile shortcuts of `konsolerc`. Shown
//! when a Konsole window has focus. Files only: nothing is run.

use super::kde;
use crate::sheet::{Bind, Sheet};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

const PINNED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/konsole/26.08.2.toml"
));

/// The KXMLGUI files Konsole's two clients read: `MainWindow` (gui `konsole`) and `SessionController` (gui `session`),
/// both under component `konsole` (Konsole `src/MainWindow.cpp:154`, `src/session/SessionController.cpp:147`).
const UI_FILES: [(&str, &str); 2] = [
    ("konsole", "kxmlgui5/konsole/konsoleui.rc"),
    ("session", "kxmlgui5/konsole/sessionui.rc"),
];

#[derive(Deserialize)]
struct Pinned {
    action: Vec<Action>,
}

#[derive(Deserialize, Clone)]
struct Action {
    action: String,
    /// The KXMLGUI client whose file can change it (`konsole`, `session`); `plugin` and `keytab`: none.
    client: String,
    desc: String,
    /// Qt portable text; none: `[]`.
    keys: Vec<String>,
    category: String,
}

/// What `sheet` read: `konsolerc`'s text (empty when missing), each client's file found (client, path, text), and
/// the scheme file, when the scheme isn't `Default` and one was read.
struct Files<'a> {
    konsolerc: &'a str,
    ui: Vec<(&'a str, String, &'a str)>,
    scheme: Option<(String, &'a str)>,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(&env)?;
    for note in notes {
        eprintln!("keybind-find: konsole: {note}");
    }
    Ok(sheet)
}

/// The files, found through `env`, then `parse`. A missing file is silent; an unreadable one is a note.
fn sheet_with(env: Env) -> anyhow::Result<(Sheet, Vec<String>)> {
    let pinned: Pinned =
        toml::from_str(PINNED).map_err(|e| anyhow::anyhow!("reading data/konsole: {e}"))?;
    let mut notes = Vec::new();
    let mut read = |p: &Path| match std::fs::read(p) {
        Ok(b) => Some(String::from_utf8_lossy(&b).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            notes.push(format!("{}: {e}", p.display()));
            None
        }
    };
    // `KSharedConfig::openConfig()`: the application's `konsolerc`.
    let konsolerc = kde::config_dir(env)
        .and_then(|d| read(&d.join("konsolerc")))
        .unwrap_or_default();
    let dirs = kde::data_dirs(env);
    // `QStandardPaths::locate(GenericDataLocation, …)`: the first that exists.
    let find =
        |rel: &str| -> Option<PathBuf> { dirs.iter().map(|d| d.join(rel)).find(|p| p.is_file()) };
    let ui: Vec<(&str, PathBuf, String)> = UI_FILES
        .iter()
        .filter_map(|(client, rel)| {
            let p = find(rel)?;
            let text = read(&p)?;
            Some((*client, p, text))
        })
        .collect();
    let scheme = current_scheme(&konsolerc);
    let scheme_file = (scheme != "Default")
        .then(|| find(&format!("konsole/shortcuts/{scheme}")))
        .flatten()
        .and_then(|p| Some((p.display().to_string(), read(&p)?)));
    let (sheet, more) = parse(
        &pinned.action,
        &Files {
            konsolerc: &konsolerc,
            ui: ui
                .iter()
                .map(|(c, p, t)| (*c, p.display().to_string(), t.as_str()))
                .collect(),
            scheme: scheme_file.as_ref().map(|(p, t)| (p.clone(), t.as_str())),
        },
    );
    notes.extend(more);
    Ok((sheet, notes))
}

/// `[Shortcut Schemes] Current Scheme`, else `Default` (kxmlgui `kshortcutschemeshelper.cpp:101-104`).
fn current_scheme(konsolerc: &str) -> String {
    kde::ini(konsolerc)
        .into_iter()
        .find(|(path, _)| path.len() == 1 && path[0] == "Shortcut Schemes")
        .and_then(|(_, entries)| entries.into_iter().find(|(k, _)| k == "Current Scheme"))
        .map_or_else(|| "Default".into(), |(_, v)| v)
}

/// The sheet from the pinned defaults with `files` applied as KXMLGUI applies them, and the notes for stderr.
fn parse(defaults: &[Action], files: &Files) -> (Sheet, Vec<String>) {
    let mut rows = defaults.to_vec();
    let mut notes = Vec::new();
    let scheme = current_scheme(files.konsolerc);
    if scheme != "Default" {
        // `applyShortcutScheme` (kxmlguifactory.cpp:627-673): every action of each client loses its keys and default,
        // then the scheme file's first `ActionProperties` (exact tag, any `scheme`) applies; none found: still cleared.
        for r in rows.iter_mut().filter(|r| is_ui(&r.client)) {
            r.keys.clear();
        }
        match &files.scheme {
            None => notes.push(format!(
                "shortcut scheme {scheme:?}: no konsole/shortcuts/{scheme} found, so Konsole has no default shortcuts"
            )),
            Some((path, text)) => match xml(text) {
                Ok(doc) => {
                    let props = doc
                        .iter()
                        .position(|(depth, tag, _)| *depth == 1 && tag == "ActionProperties");
                    if let Some(i) = props {
                        apply(&mut rows, None, &doc[i + 1..]);
                    }
                }
                Err(e) => notes.push(format!("{path}: {e}; skipped")),
            },
        }
    }
    for (client, path, text) in &files.ui {
        match xml(text) {
            // `findActionPropertiesElement` (kxmlguifactory.cpp:297-309): the root's first child `ActionProperties`
            // (any case) whose `scheme`, default `Default`, is the current one.
            Ok(doc) => {
                let props = doc.iter().position(|(depth, tag, attrs)| {
                    *depth == 1
                        && tag.eq_ignore_ascii_case("ActionProperties")
                        && attr(attrs, "scheme").unwrap_or("Default") == scheme
                });
                if let Some(i) = props {
                    apply(&mut rows, Some(client), &doc[i + 1..]);
                }
            }
            Err(e) => notes.push(format!("{path}: {e}; using the defaults")),
        }
    }
    // `ProfileManager::loadShortcuts` (src/profile/ProfileManager.cpp:522-543): `<shortcut>=<profile name>` (or the
    // name with `.profile`); triggering it opens a new tab with that profile (`ProfileList::triggered` →
    // `MainWindow::newFromProfile` → `createSession`, src/MainWindow.cpp:638, 741-758, 1002-1005).
    for (path, entries) in kde::ini(files.konsolerc) {
        if path.len() != 1 || path[0] != "Profile Shortcuts" {
            continue;
        }
        for (key, profile) in entries {
            let name = profile
                .strip_suffix(".profile")
                .unwrap_or(&profile)
                .to_string();
            rows.push(Action {
                action: name.clone(),
                client: "profile".into(),
                desc: format!("New tab with profile {name}"),
                keys: vec![key],
                category: "profiles".into(),
            });
        }
    }
    let mut sheet = Sheet {
        title: "Konsole".into(),
        provider: "konsole".into(),
        binds: Default::default(),
    };
    let mut left_out: BTreeMap<&str, usize> = BTreeMap::new();
    for r in rows {
        for k in &r.keys {
            match kde::notation(k).map(|k| x_names(&k)) {
                Ok(key) => sheet
                    .binds
                    .entry(r.category.clone())
                    .or_default()
                    .push(Bind {
                        key,
                        desc: r.desc.clone(),
                        action: r.action.clone(),
                        when: Default::default(),
                        run: Vec::new(),
                    }),
                Err(kind) => *left_out.entry(kind).or_default() += 1,
            }
        }
    }
    if !left_out.is_empty() {
        let kinds: Vec<String> = left_out.iter().map(|(k, n)| format!("{k}: {n}")).collect();
        notes.push(format!(
            "leaving out {} keys keybind-find can't send ({})",
            left_out.values().sum::<usize>(),
            kinds.join(", ")
        ));
    }
    (sheet, notes)
}

/// `kde::notation`'s `Page_Up`/`Page_Down` as the X keysyms `Prior`/`Next`, as the other terminals' sheets write them.
fn x_names(key: &str) -> String {
    key.split(' ')
        .map(|chord| {
            let (mods, sym) = chord.rsplit_once('+').map_or(("", chord), |(m, s)| (m, s));
            let sym = match sym {
                "Page_Up" => "Prior",
                "Page_Down" => "Next",
                s => s,
            };
            if mods.is_empty() {
                sym.to_string()
            } else {
                format!("{mods}+{sym}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A client whose actions KXMLGUI files change (the plugins' actions aren't in a collection; the keytab isn't KXMLGUI).
fn is_ui(client: &str) -> bool {
    client == "konsole" || client == "session"
}

/// `applyActionProperties` and `configureAction` (kxmlguifactory.cpp:553-620) over the elements after an
/// `ActionProperties` (its children: depth 2, until the next depth-1 element): each `action` (any case) with a `name`
/// sets that action's keys from `shortcut`, or `accel` (any case) as its old name, the last of them in the element.
/// `client` `None` (a scheme file): every client's action of that name. A name no pinned action has is listed as it
/// is.
fn apply(rows: &mut Vec<Action>, client: Option<&str>, after: &[Element]) {
    for (_, tag, attrs) in after.iter().take_while(|(d, _, _)| *d > 1) {
        if !tag.eq_ignore_ascii_case("action") {
            continue;
        }
        let Some(name) = attr(attrs, "name") else {
            continue;
        };
        let Some(value) = attrs
            .iter()
            .rev()
            .find(|(k, _)| k == "shortcut" || k.eq_ignore_ascii_case("accel"))
            .map(|(_, v)| v)
        else {
            continue;
        };
        // `QKeySequence::listFromString` splits on "; " (qtbase qkeysequence.cpp:1553); an empty part is no key.
        let keys: Vec<String> = value
            .split("; ")
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(String::from)
            .collect();
        let mut found = false;
        for r in rows
            .iter_mut()
            .filter(|r| r.action == name && client.map_or(is_ui(&r.client), |c| r.client == c))
        {
            r.keys = keys.clone();
            found = true;
        }
        if !found {
            rows.push(Action {
                action: name.into(),
                client: client.unwrap_or("konsole").into(),
                desc: name.into(),
                keys,
                category: "konsole".into(),
            });
        }
    }
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// An XML element: its depth (the root: 0), tag and attributes in file order.
type Element = (usize, String, Vec<(String, String)>);

/// The elements of an XML document in order, read as far as KXMLGUI needs: tags and quoted attributes (the five
/// predefined entities and character references decoded); comments, `<?…?>` and `<!DOCTYPE …>` skipped; text ignored.
/// An error for an unclosed or mismatched tag, an unquoted value, an unknown entity, or no single root.
fn xml(text: &str) -> Result<Vec<Element>, String> {
    let mut out = Vec::new();
    let mut open: Vec<&str> = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = &r[r.find("-->").ok_or("unclosed comment")? + 3..];
        } else if let Some(r) = rest.strip_prefix("<?") {
            rest = &r[r.find("?>").ok_or("unclosed <?")? + 2..];
        } else if let Some(r) = rest.strip_prefix("<!") {
            // `<!DOCTYPE …>`, an internal subset in `[…]` included.
            let mut depth = 0i32;
            let end = r
                .char_indices()
                .find(|&(_, c)| {
                    depth += i32::from(c == '[') - i32::from(c == ']');
                    c == '>' && depth == 0
                })
                .ok_or("unclosed <!")?;
            rest = &r[end.0 + 1..];
        } else if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').ok_or("unclosed end tag")?;
            let name = r[..end].trim();
            if open.pop() != Some(name) {
                return Err(format!("</{name}> closes no open element"));
            }
            rest = &r[end + 1..];
        } else {
            if open.is_empty() && !out.is_empty() {
                return Err("a second root element".into());
            }
            let r = &rest[1..];
            let n = r
                .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
                .ok_or("unclosed tag")?;
            let (name, mut r) = (&r[..n], &r[n..]);
            if name.is_empty() {
                return Err("a `<` with no tag name".into());
            }
            let mut attrs = Vec::new();
            let depth = open.len();
            loop {
                r = r.trim_start();
                if let Some(x) = r.strip_prefix("/>") {
                    rest = x;
                    break;
                }
                if let Some(x) = r.strip_prefix('>') {
                    open.push(name);
                    rest = x;
                    break;
                }
                let eq = r
                    .find('=')
                    .ok_or_else(|| format!("<{name}> is not closed"))?;
                let key = r[..eq].trim();
                if key.is_empty()
                    || key.contains(|c: char| c.is_whitespace() || "<>/\"'".contains(c))
                {
                    return Err(format!("a bad attribute in <{name}>"));
                }
                let v = r[eq + 1..].trim_start();
                let q = v
                    .chars()
                    .next()
                    .filter(|c| *c == '"' || *c == '\'')
                    .ok_or_else(|| format!("{key}: an unquoted value"))?;
                let close = v[1..]
                    .find(q)
                    .ok_or_else(|| format!("{key}: an unclosed value"))?;
                attrs.push((key.to_string(), entities(&v[1..1 + close])?));
                r = &v[close + 2..];
            }
            out.push((depth, name.to_string(), attrs));
        }
    }
    match (open.last(), out.is_empty()) {
        (Some(name), _) => Err(format!("<{name}> is not closed")),
        (None, true) => Err("no root element".into()),
        (None, false) => Ok(out),
    }
}

/// `&lt; &gt; &amp; &apos; &quot;`, `&#N;` and `&#xH;` decoded.
fn entities(s: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let r = &rest[i + 1..];
        let end = r.find(';').ok_or("a `&` with no `;`")?;
        let e = &r[..end];
        let num = |digits: &str, radix| {
            (!digits.starts_with('+'))
                .then(|| u32::from_str_radix(digits, radix).ok())
                .flatten()
                .and_then(char::from_u32)
        };
        out.push(match e {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "apos" => '\'',
            "quot" => '"',
            _ => e
                .strip_prefix("#x")
                .map(|h| num(h, 16))
                .or_else(|| e.strip_prefix('#').map(|d| num(d, 10)))
                .flatten()
                .ok_or_else(|| format!("unknown entity &{e};"))?,
        });
        rest = &r[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/konsole")
            .join(name)
    }

    fn read(name: &str) -> String {
        std::fs::read_to_string(fixture(name)).unwrap()
    }

    fn pinned() -> Vec<Action> {
        toml::from_str::<Pinned>(PINNED).unwrap().action
    }

    /// Every bind of `action` as (key, desc).
    fn get<'a>(s: &'a Sheet, action: &str) -> Vec<(&'a str, &'a str)> {
        s.binds
            .values()
            .flatten()
            .filter(|b| b.action == action)
            .map(|b| (b.key.as_str(), b.desc.as_str()))
            .collect()
    }

    fn files<'a>(konsolerc: &'a str, ui: &[(&'a str, &'a str)]) -> Files<'a> {
        Files {
            konsolerc,
            ui: ui
                .iter()
                .map(|(c, t)| (*c, format!("{c}ui.rc"), *t))
                .collect(),
            scheme: None,
        }
    }

    #[test]
    fn pinned_defaults() {
        for s in [
            "tag v26.08.2 (commit c21993d0b73b6fe5d4c52da50b25370b40389e4e)",
            "GPL-2.0-or-later",
            "kconfig tag v6.31.0",
            "kxmlgui tag v6.31.0",
            "src/session/SessionController.cpp",
            "default.keytab",
        ] {
            assert!(PINNED.contains(s), "{s}");
        }
        let p = pinned();
        // 140 actions; 81 with a default key (86 keys): 61 of Konsole's setDefaultShortcut(s) calls off macOS
        // (switch-to-tab's two give ten), Ctrl+Shift+B, the plugins' four, SaveAs, KeyBindings and Preferences from
        // KStandardShortcut, open_kcommand_bar, help_whats_this, hamburger_menu and the keytab's eight.
        assert_eq!(p.len(), 140);
        assert_eq!(p.iter().filter(|a| !a.keys.is_empty()).count(), 81);
        // Every key reads, so nothing is left out.
        let (s, notes) = parse(&p, &files("", &[]));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 86);
        assert_eq!(get(&s, "new-tab"), [("ctrl+shift+t", "New Tab")]);
        assert_eq!(
            get(&s, "next-tab"),
            [("shift+Right", "Next Tab"), ("ctrl+Next", "Next Tab")]
        );
        assert_eq!(get(&s, "switch-to-tab-9"), [("alt+0", "Switch to Tab 10")]);
        assert!(get(&s, "switch-to-tab-10").is_empty());
        assert_eq!(get(&s, "edit_find"), [("ctrl+shift+f", "Find…")]);
        assert_eq!(
            get(&s, "file_save_as"),
            [("ctrl+shift+s", "Save Output As...")]
        );
        assert_eq!(get(&s, "hamburger_menu"), [("shift+F10", "Open Menu")]);
        assert!(get(&s, "help_contents").is_empty());
        assert_eq!(get(&s, "close-window"), [("ctrl+shift+q", "Close Window")]);
        assert_eq!(
            get(&s, "split-view-left-right"),
            [("ctrl+parenleft", "Split View Left/Right")]
        );
        assert_eq!(
            get(&s, "scrollPageUp"),
            [(
                "shift+Prior",
                "Scroll up one page (not in full-screen programs)"
            )]
        );
        assert_eq!(s.binds["scrolling"].len(), 8);
        for b in s.binds.values().flatten() {
            assert!(
                b.key.parse::<keybind_find_sheet::Key>().is_ok(),
                "{}",
                b.key
            );
        }
        assert_eq!(
            (s.title.as_str(), s.provider.as_str()),
            ("Konsole", "konsole")
        );
        // One row per action and client; every client one the provider knows.
        let mut seen = std::collections::BTreeSet::new();
        for a in &p {
            assert!(
                seen.insert((a.client.as_str(), a.action.as_str())),
                "{}",
                a.action
            );
            assert!(["konsole", "session", "plugin", "keytab"].contains(&a.client.as_str()));
            assert!(!a.desc.contains('&'), "{}", a.desc);
        }
    }

    #[test]
    fn the_users_konsoleui_rc() {
        let ui = read("konsoleui.rc");
        let session = read("sessionui.rc");
        let (s, notes) = parse(
            &pinned(),
            &files("", &[("konsole", &ui), ("session", &session)]),
        );
        // The first Default ActionProperties; the "Mine" one before it and the second Default one after: ignored.
        assert_eq!(get(&s, "close-window"), [("ctrl+shift+x", "Close Window")]);
        // Two keys; none; `accel`; an entity; a character reference in a lower-case `action`.
        assert_eq!(
            get(&s, "new-tab"),
            [("ctrl+shift+t", "New Tab"), ("ctrl+t", "New Tab")]
        );
        assert!(get(&s, "new-window").is_empty());
        assert_eq!(
            get(&s, "detach-tab"),
            [("ctrl+shift+j", "Detach Current Tab")]
        );
        assert_eq!(
            get(&s, "split-view-auto"),
            [("ctrl+ampersand", "Split View Automatically")]
        );
        assert_eq!(get(&s, "clone-tab"), [("ctrl+alt+c", "Clone Tab")]);
        // An action the defaults don't have: its name, category konsole.
        assert_eq!(
            s.binds["konsole"]
                .iter()
                .map(|b| (b.key.as_str(), b.desc.as_str()))
                .collect::<Vec<_>>(),
            [("super+k", "some-plugin-action")]
        );
        // A keypad key: left out and counted; the defaults it replaced are gone.
        assert!(get(&s, "focus-view-next").is_empty());
        assert_eq!(
            notes,
            ["leaving out 1 keys keybind-find can't send (keypad: 1)"]
        );
        // sessionui.rc changes the session's actions only: the window's split-view-left-right keeps its key.
        assert_eq!(get(&s, "edit_copy"), [("ctrl+alt+c", "Copy")]);
        assert_eq!(
            get(&s, "split-view-left-right"),
            [
                ("ctrl+alt+l", "Split View Left-Right"),
                ("ctrl+parenleft", "Split View Left/Right")
            ]
        );
        // Untouched defaults stay.
        assert_eq!(
            get(&s, "close-session"),
            [("ctrl+shift+w", "Close Session")]
        );
    }

    #[test]
    fn a_malformed_file_keeps_the_defaults() {
        let bad = read("malformed.rc");
        let session = read("sessionui.rc");
        let (s, notes) = parse(
            &pinned(),
            &files("", &[("konsole", &bad), ("session", &session)]),
        );
        assert_eq!(
            notes,
            ["konsoleui.rc: <gui> is not closed; using the defaults"]
        );
        assert_eq!(get(&s, "close-window"), [("ctrl+shift+q", "Close Window")]);
        assert_eq!(get(&s, "edit_copy"), [("ctrl+alt+c", "Copy")]);
    }

    #[test]
    fn the_xml_reader() {
        let doc = xml("\u{feff}<?xml version='1.0'?>\n<!DOCTYPE gui [ <!ENTITY x 'y'> ]>\n<!-- <a b='c'/> -->\n<gui a = \"1\" b='&lt;&gt;&amp;&apos;&quot;&#65;&#x42;'><x/><y z=\"\">t</y></gui>\n").unwrap();
        let attrs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        assert_eq!(
            doc,
            [
                (0, "gui".into(), attrs(&[("a", "1"), ("b", "<>&'\"AB")])),
                (1, "x".into(), vec![]),
                (1, "y".into(), attrs(&[("z", "")])),
            ]
        );
        for (bad, why) in [
            ("<gui>", "<gui> is not closed"),
            ("<gui></x>", "</x> closes no open element"),
            ("<gui a=b/>", "a: an unquoted value"),
            ("<gui a='b/>", "a: an unclosed value"),
            ("<gui a='&nbsp;'/>", "unknown entity &nbsp;"),
            ("<gui a='&#xD800;'/>", "unknown entity &#xD800;"),
            ("<gui a='&amp'/>", "a `&` with no `;`"),
            ("<gui/><gui/>", "a second root element"),
            ("<!-- x", "unclosed comment"),
            ("", "no root element"),
            ("<gui a/>", "<gui> is not closed"),
            ("< gui/>", "a `<` with no tag name"),
        ] {
            assert_eq!(xml(bad), Err(why.to_string()), "{bad}");
        }
    }

    #[test]
    fn a_shortcut_scheme() {
        let ui = read("konsoleui.rc");
        let scheme = read("share/konsole/shortcuts/Mine");
        let rc = read("konsolerc");
        let mut f = files(&rc, &[("konsole", &ui)]);
        f.scheme = Some(("Mine".into(), &scheme));
        let (s, notes) = parse(&pinned(), &f);
        assert!(notes.is_empty(), "{notes:?}");
        // Only the scheme's keys and the user's scheme="Mine" changes, beside what KXMLGUI doesn't own (the plugins,
        // the keytab) and the profile shortcuts.
        let ui_keys: Vec<(&str, &str)> = s
            .binds
            .iter()
            .filter(|(c, _)| !["scrolling", "profiles"].contains(&c.as_str()))
            .flat_map(|(_, b)| b)
            .filter(|b| {
                !b.action.starts_with("quickcommands") && !b.action.starts_with("sshmanager")
            })
            .map(|b| (b.key.as_str(), b.action.as_str()))
            .collect();
        let mut ui_keys = ui_keys;
        ui_keys.sort();
        assert_eq!(
            ui_keys,
            [
                ("ctrl+Insert", "edit_copy"),
                ("ctrl+alt+q", "close-window"),
                ("ctrl+c", "edit_copy"),
                ("ctrl+t", "new-tab"),
            ]
        );
        assert_eq!(s.binds["scrolling"].len(), 8);
        // Without the scheme file: cleared, and one note.
        let f = files(&rc, &[]);
        let (s, notes) = parse(&pinned(), &f);
        assert_eq!(
            notes,
            ["shortcut scheme \"Mine\": no konsole/shortcuts/Mine found, so Konsole has no default shortcuts"]
        );
        assert!(get(&s, "new-tab").is_empty() && get(&s, "close-window").is_empty());
        assert_eq!(s.binds["scrolling"].len(), 8);
    }

    #[test]
    fn profile_shortcuts() {
        let (s, notes) = parse(
            &pinned(),
            &files(
                "[Profile Shortcuts]\nCtrl+Alt+1=Work\nCtrl+Alt+2=Root.profile\nNum+1=Pad\n",
                &[],
            ),
        );
        assert_eq!(
            s.binds["profiles"]
                .iter()
                .map(|b| (b.key.as_str(), b.desc.as_str(), b.action.as_str()))
                .collect::<Vec<_>>(),
            [
                ("ctrl+alt+1", "New tab with profile Work", "Work"),
                ("ctrl+alt+2", "New tab with profile Root", "Root"),
            ]
        );
        assert_eq!(
            notes,
            ["leaving out 1 keys keybind-find can't send (keypad: 1)"]
        );
        // The Default scheme, named: nothing cleared.
        let (s, _) = parse(
            &pinned(),
            &files("[Shortcut Schemes]\nCurrent Scheme=Default\n", &[]),
        );
        assert_eq!(get(&s, "new-tab"), [("ctrl+shift+t", "New Tab")]);
    }

    /// A fresh directory for one test.
    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-konsole-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn env_of(pairs: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
    }

    const CLOSE: &str = "<gui><ActionProperties><Action name='close-window' shortcut='Ctrl+Alt+";

    fn close_window(env: Env) -> (Vec<String>, Vec<String>) {
        let (s, notes) = sheet_with(env).unwrap();
        (
            get(&s, "close-window")
                .iter()
                .map(|(k, _)| k.to_string())
                .collect(),
            notes,
        )
    }

    #[test]
    fn the_paths() {
        let d = tmp("paths");
        let (home, xdg, sys1, sys2) = (
            d.join("home"),
            d.join("xdg"),
            d.join("sys1"),
            d.join("sys2"),
        );
        let ui = "kxmlgui5/konsole/konsoleui.rc";
        let dirs = format!("{}:{}", sys1.display(), sys2.display());
        let h = home.display().to_string();
        let base = || vec![("HOME", h.clone()), ("XDG_DATA_DIRS", dirs.clone())];
        // Nothing anywhere: the defaults, silently.
        assert_eq!(
            close_window(&env_of(base())),
            (vec!["ctrl+shift+q".into()], vec![])
        );
        // A file only in an XDG_DATA_DIRS entry; the first entry before the second.
        put(&sys2, ui, &format!("{CLOSE}2'/></ActionProperties></gui>"));
        assert_eq!(close_window(&env_of(base())).0, ["ctrl+alt+2"]);
        put(&sys1, ui, &format!("{CLOSE}1'/></ActionProperties></gui>"));
        assert_eq!(close_window(&env_of(base())).0, ["ctrl+alt+1"]);
        // The user's ~/.local/share before them; XDG_DATA_HOME instead of it; empty or relative: unset.
        put(
            &home,
            &format!(".local/share/{ui}"),
            &format!("{CLOSE}h'/></ActionProperties></gui>"),
        );
        assert_eq!(close_window(&env_of(base())).0, ["ctrl+alt+h"]);
        put(&xdg, ui, &format!("{CLOSE}x'/></ActionProperties></gui>"));
        let mut e = base();
        e.push(("XDG_DATA_HOME", xdg.display().to_string()));
        assert_eq!(close_window(&env_of(e)).0, ["ctrl+alt+x"]);
        for v in ["", "rel"] {
            let mut e = base();
            e.push(("XDG_DATA_HOME", v.to_string()));
            assert_eq!(close_window(&env_of(e)).0, ["ctrl+alt+h"]);
        }
        // konsolerc from XDG_CONFIG_HOME, else ~/.config; its scheme's file from the data dirs.
        put(
            &home,
            ".config/konsolerc",
            "[Shortcut Schemes]\nCurrent Scheme=Mine\n",
        );
        let (keys, notes) = close_window(&env_of(base()));
        assert!(keys.is_empty());
        assert_eq!(notes.len(), 1);
        put(
            &sys2,
            "konsole/shortcuts/Mine",
            &format!("{CLOSE}m'/></ActionProperties></gui>"),
        );
        assert_eq!(
            close_window(&env_of(base())),
            (vec!["ctrl+alt+m".into()], vec![])
        );
        let cfg = d.join("cfg");
        put(&cfg, "konsolerc", "[Profile Shortcuts]\nCtrl+Alt+9=Work\n");
        let mut e = base();
        e.push(("XDG_CONFIG_HOME", cfg.display().to_string()));
        let (s, _) = sheet_with(&env_of(e)).unwrap();
        assert_eq!(get(&s, "close-window"), [("ctrl+alt+h", "Close Window")]);
        assert_eq!(
            get(&s, "Work"),
            [("ctrl+alt+9", "New tab with profile Work")]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
