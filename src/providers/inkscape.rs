//! Inkscape 1.x: the keys of the preset Inkscape loads (its own `keys/default.xml`, or the one its preferences name)
//! with the shared and the user's `keys/default.xml` laid over it, read as `Shortcuts::init` reads them
//! (`src/ui/shortcuts.cpp` at INKSCAPE_1_4_4), each labelled and grouped as Inkscape's Keyboard preferences show it,
//! from its action table pinned per version (`data/inkscape/`). Nothing is run but `inkscape --version`.

use super::openbox::{is_keysym, printable};
use super::{pinned, read_capped};
use crate::sheet::{chord, Bind, Sheet, Source};
use anyhow::{ensure, Context as _};
use roxmltree::{Document, Node};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const REGISTRY: &[(&str, &str)] = &[(
    "1.4.4",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/inkscape/1.4.4.toml"
    )),
)];

/// The largest file read (Inkscape's own `inkscape.xml` is about 30 KB).
const MAX_SIZE: u64 = 4 << 20;

/// The XInclude namespaces libxml2 takes: the 2001 one and its old 2003 one.
const XINCLUDE: [&str; 2] = [
    "http://www.w3.org/2001/XInclude",
    "http://www.w3.org/2003/XInclude",
];

/// How many includes deep a file may go.
const MAX_DEPTH: usize = 8;

/// How many includes one lookup takes in all (each `xi:include` met, followed or not), so a file including another
/// many times over can't make the hotkey hang.
const MAX_INCLUDED: usize = 64;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;
/// A file's text; `None` when it doesn't exist; else why it can't be read (`read_capped`, or a test's files).
type Reader<'a> = &'a dyn Fn(&Path) -> Result<Option<String>, String>;

#[derive(Deserialize)]
struct Pin {
    inkscape_version: String,
    action: Vec<PinAction>,
}

#[derive(Deserialize)]
struct PinAction {
    id: String,
    label: String,
    section: String,
}

/// An Inkscape window: X11's class `Inkscape`, or the prgname `org.inkscape.Inkscape` (X11's instance, Wayland's app
/// id), with the `.<INKSCAPE_APP_ID_TAG>` or `.<revision>` some builds append; any case.
pub(crate) fn is_inkscape(class: &str) -> bool {
    const ID: &str = "org.inkscape.Inkscape";
    class.eq_ignore_ascii_case("Inkscape")
        || class.eq_ignore_ascii_case(ID)
        || class
            .get(..ID.len() + 1)
            .is_some_and(|p| p.eq_ignore_ascii_case(&format!("{ID}.")))
}

pub fn sheet() -> anyhow::Result<(Sheet, Option<Source>)> {
    let (sheet, notes, source) = read(&|k| std::env::var(k).ok(), |_| {})?;
    for note in notes {
        eprintln!("keybind-find: inkscape: {note}");
    }
    Ok((sheet, Some(source)))
}

/// The sheet, the stderr notes (printable) and the action table's `Source`. `env`: keybind-find's environment (its
/// `PATH` too); `setup`: the version probe's test seam.
fn read(env: Env, setup: impl Fn(&mut Command)) -> anyhow::Result<(Sheet, Vec<String>, Source)> {
    let mut cmd = Command::new("inkscape");
    cmd.arg("--version");
    // `Inkscape 1.4.4 (dcaf3e7d9e, 2026-05-05)`: the word after `Inkscape `, on the first line that has it.
    let (_, probed) = pinned::probe("inkscape", REGISTRY[0].0, cmd, setup, |t| {
        let v = t
            .lines()
            .find_map(|l| l.strip_prefix("Inkscape "))?
            .split_whitespace()
            .next()?;
        pinned::parse_version(v).map(|_| v.to_string())
    })?;
    let (v, text, note, source) = pinned::select("inkscape", REGISTRY, probed.installed.as_deref())
        .context("no pinned Inkscape actions")?;
    let pin: Pin =
        toml::from_str(text).with_context(|| format!("reading data/inkscape/{v}.toml"))?;
    ensure!(
        pin.inkscape_version == v,
        "data/inkscape/{v}.toml names Inkscape {}",
        pin.inkscape_version
    );
    let mut notes: Vec<String> = note.into_iter().collect();
    let table = Table::new(&pin);
    let keys = lookup(env, &|p| read_capped(p, MAX_SIZE), &table, &mut notes);
    let (sheet, left_out) = to_sheet(&keys, &table);
    notes.extend(left_out);
    Ok((sheet, notes.iter().map(|n| printable(n)).collect(), source))
}

// --- which files ----------------------------------------------------------------------------------------------------

/// `Shortcuts::init` (`shortcuts.cpp:72-140`): the first preset that loads (the preference's, `default.xml`,
/// `inkscape.xml`), then the shared and the user's `keys/default.xml` as user files.
fn lookup(env: Env, read: Reader, table: &Table, notes: &mut Vec<String>) -> Keys {
    let mut keys = Keys::default();
    let profile = profile_dir(env);
    let keys_dir = data_dir(env).join("inkscape").join("keys");
    let (shortcutfile, sharedpath) = profile
        .as_ref()
        .map(|p| preferences(&p.join("preferences.xml"), read, notes))
        .unwrap_or_default();
    let named = shortcutfile
        .filter(|f| !f.is_empty())
        // A relative name is under the keys dir; `join` keeps an absolute one as it is.
        .map(|f| keys_dir.join(f));
    let stock = ["default.xml", "inkscape.xml"].map(|f| keys_dir.join(f));
    // The named preset first, not read twice when it is a stock one.
    let presets = named.iter().map(|p| (p.clone(), true)).chain(
        stock
            .into_iter()
            .filter(|p| Some(p) != named.as_ref())
            .map(|p| (p, false)),
    );
    let mut loaded = false;
    for (path, is_named) in presets {
        match apply_file(&mut keys, table, &path, false, read, notes) {
            Ok(true) => {
                loaded = true;
                break;
            }
            Ok(false) if is_named => notes.push(format!(
                "{}: the preset your preferences name doesn't exist; falling back to the stock presets",
                path.display()
            )),
            Err(why) => notes.push(format!("{}: {why}; this preset is skipped", path.display())),
            Ok(false) => {}
        }
    }
    if !loaded {
        let named_elsewhere = named.filter(|p| !p.starts_with(&keys_dir));
        notes.push(match named_elsewhere {
            Some(p) => format!(
                "no keyboard preset loads ({} nor {}'s stock ones): listing only your own keys",
                p.display(),
                keys_dir.display()
            ),
            None => format!(
                "no keyboard preset loads from {}: listing only your own keys",
                keys_dir.display()
            ),
        });
    }
    // `shared_path()` (`src/io/resource.cpp:487-497`): only a directory.
    let shared = sharedpath
        .map(PathBuf::from)
        .filter(|d| !d.as_os_str().is_empty() && d.is_dir());
    for dir in shared.into_iter().chain(profile) {
        let path = dir.join("keys").join("default.xml");
        if let Err(why) = apply_file(&mut keys, table, &path, true, read, notes) {
            notes.push(format!("{}: {why}; skipped", path.display()));
        }
    }
    keys
}

/// `profile_path()` (`src/io/resource.cpp:410-470`): `$INKSCAPE_PROFILE_DIR`, else GLib's user config dir's
/// `inkscape`. ponytail: the Flatpak's `~/.var/app/org.inkscape.Inkscape/config` isn't followed (keybind-find's env).
fn profile_dir(env: Env) -> Option<PathBuf> {
    let set = |k| env(k).filter(|v: &String| !v.is_empty()).map(PathBuf::from);
    set("INKSCAPE_PROFILE_DIR")
        .or_else(|| set("XDG_CONFIG_HOME").map(|d| d.join("inkscape")))
        .or_else(|| set("HOME").map(|h| h.join(".config").join("inkscape")))
}

/// `get_inkscape_datadir` (`src/path-prefix.cpp:77-99`): `$INKSCAPE_DATADIR`, else `<prefix>/share` of the `inkscape`
/// on `PATH` (`<prefix>/bin/inkscape`, symlinks resolved) when it has an `inkscape` dir, else Arch's `/usr/share`.
/// A wrapper script on `PATH` gives its own prefix, as it would to Inkscape run through it.
/// ponytail: an AppImage's or macOS bundle's datadir isn't followed.
fn data_dir(env: Env) -> PathBuf {
    if let Some(d) = env("INKSCAPE_DATADIR").filter(|d| !d.is_empty()) {
        return d.into();
    }
    let path = env("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join("inkscape"))
        .find(|f| f.is_file())
        .and_then(|f| f.canonicalize().ok())
        .and_then(|f| {
            let bin = f.parent()?;
            (bin.file_name()? == "bin").then(|| bin.parent().map(|p| p.join("share")))?
        })
        .filter(|share| share.join("inkscape").is_dir())
        .unwrap_or_else(|| PathBuf::from("/usr/share"))
}

/// The preferences' `/options/kbshortcuts/shortcutfile` and `/options/resources/sharedpath`, found as
/// `Preferences::_getNode` (`src/preferences.cpp:655-700`) finds them: from the root, the first child with each `id`.
/// A missing file is silent; one that doesn't load is a note.
fn preferences(
    path: &Path,
    read: Reader,
    notes: &mut Vec<String>,
) -> (Option<String>, Option<String>) {
    let parsed = read(path).and_then(|t| {
        t.map(|t| {
            Document::parse(&t).map_err(|e| e.to_string()).map(|doc| {
                let get = |group: &str, attr: &str| {
                    let node =
                        ["options", group]
                            .iter()
                            .try_fold(doc.root_element(), |n, id| {
                                n.children()
                                    .find(|c| c.is_element() && c.attribute("id") == Some(*id))
                            })?;
                    node.attribute(attr).map(String::from)
                };
                (
                    get("kbshortcuts", "shortcutfile"),
                    get("resources", "sharedpath"),
                )
            })
        })
        .transpose()
    });
    match parsed {
        Ok(found) => found.unwrap_or_default(),
        Err(why) => {
            notes.push(format!("{}: {why}; read as no preferences", path.display()));
            Default::default()
        }
    }
}

// --- reading a keys file --------------------------------------------------------------------------------------------

/// `Shortcuts::read` (`shortcuts.cpp:253-282`): the file's binds applied when it loads (read, parsed, its root a
/// `keys` element): `Ok(true)`; `Ok(false)` when it doesn't exist; else why it doesn't load, nothing applied.
fn apply_file(
    keys: &mut Keys,
    table: &Table,
    path: &Path,
    user: bool,
    read: Reader,
    notes: &mut Vec<String>,
) -> Result<bool, String> {
    let Some(text) = read(path)? else {
        return Ok(false);
    };
    let doc = Document::parse(&text).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    if root.tag_name().name() != "keys" {
        return Err(format!(
            "its root element is <{}>, not <keys>",
            root.tag_name().name()
        ));
    }
    let mut walk = Walk {
        keys,
        table,
        user,
        read,
        notes,
        chain: vec![identity(path)],
    };
    walk.children(root, path);
    Ok(true)
}

/// A file as the include chain compares it: its canonical path when it has one.
fn identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

struct Walk<'a> {
    keys: &'a mut Keys,
    table: &'a Table,
    user: bool,
    read: Reader<'a>,
    notes: &'a mut Vec<String>,
    /// The files being read, the outermost first.
    chain: Vec<PathBuf>,
}

impl Walk<'_> {
    /// `Shortcuts::_read` (`shortcuts.cpp:291-373`) over `node`'s children, in document order.
    fn children(&mut self, node: Node, file: &Path) {
        for c in node.children().filter(Node::is_element) {
            self.element(c, file);
        }
    }

    /// One element: a nested `keys` read in place, a `bind` applied, an XInclude spliced; anything else (`modifier`:
    /// mouse modifiers) skipped.
    fn element(&mut self, el: Node, file: &Path) {
        let name = el.tag_name();
        if name.name() == "include" && name.namespace().is_some_and(|ns| XINCLUDE.contains(&ns)) {
            return self.include(el, file);
        }
        match name.name() {
            "keys" => self.children(el, file),
            "bind" => self.bind(el),
            _ => {}
        }
    }

    /// `gaction` and `keys` (`shortcuts.cpp:343-369`): leading spaces trimmed, split on `\s*,\s*`, each key applied.
    /// A bind of Inkscape 0.92's format (`verb`, `action`, `key`, `modifiers`) binds nothing and is counted.
    fn bind(&mut self, el: Node) {
        match (el.attribute("gaction"), el.attribute("keys")) {
            (Some(action), Some(keys)) => {
                for key in split_keys(keys) {
                    self.keys.add(self.table, action, key, self.user);
                }
            }
            _ if ["verb", "action", "key", "modifiers"]
                .iter()
                .any(|a| el.attribute(*a).is_some()) =>
            {
                self.keys.left.old_format += 1;
            }
            _ => {}
        }
    }

    /// libxml2's `xmlXIncludeProcessFlags(…, XML_PARSE_NOXINCNODE)` (`src/xml/repr-io.cpp:274-305`): a whole-file XML
    /// include is replaced by the included file's root element; a failed one is a note and dropped, as are (counted)
    /// one without an `href`, a text include, an `xpointer`, a URL. Every include counts against `MAX_INCLUDED`.
    fn include(&mut self, el: Node, file: &Path) {
        if self.keys.included >= MAX_INCLUDED {
            // One note, then each one past the cap counted.
            if self.keys.included == MAX_INCLUDED {
                self.notes.push(format!(
                    "{}: more than {MAX_INCLUDED} includes in all; the rest are left out",
                    file.display()
                ));
            }
            self.keys.included += 1;
            self.keys.left.includes += 1;
            return;
        }
        self.keys.included += 1;
        let href = el.attribute("href").filter(|h| {
            !h.is_empty()
                && matches!(el.attribute("parse"), None | Some("xml"))
                && el.attribute("xpointer").is_none()
                && !has_scheme(h)
        });
        let Some(href) = href else {
            self.keys.left.includes += 1;
            return;
        };
        let path = file.parent().unwrap_or(Path::new("")).join(href);
        let id = identity(&path);
        let failed = if self.chain.contains(&id) {
            Some("it is already being read (an include cycle)".to_string())
        } else if self.chain.len() > MAX_DEPTH {
            Some(format!("more than {MAX_DEPTH} includes deep"))
        } else {
            match (self.read)(&path) {
                Ok(None) => Some("it doesn't exist".to_string()),
                Err(why) => Some(why),
                Ok(Some(text)) => match Document::parse(&text) {
                    Err(e) => Some(e.to_string()),
                    Ok(doc) => {
                        self.chain.push(id);
                        self.element(doc.root_element(), &path);
                        self.chain.pop();
                        None
                    }
                },
            }
        };
        if let Some(why) = failed {
            self.notes.push(format!(
                "{}: including {}: {why}; its keys are left out",
                file.display(),
                path.display()
            ));
        }
    }
}

/// `scheme:` at the start of an href (RFC 3986): a URL, not a local path.
fn has_scheme(href: &str) -> bool {
    href.split_once(':').is_some_and(|(s, _)| {
        s.starts_with(|c: char| c.is_ascii_alphabetic())
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// `keys` as `_read` splits it: leading spaces (only spaces) removed, then `Glib::Regex::split_simple("\\s*,\\s*")`,
/// which gives nothing for an empty string.
fn split_keys(keys: &str) -> Vec<&str> {
    let keys = keys.trim_start_matches(' ');
    if keys.is_empty() {
        return Vec::new();
    }
    let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c');
    let pieces: Vec<&str> = keys.split(',').collect();
    let last = pieces.len() - 1;
    pieces
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let p = if i > 0 { p.trim_start_matches(ws) } else { p };
            if i < last {
                p.trim_end_matches(ws)
            } else {
                p
            }
        })
        .collect()
}

// --- keys ------------------------------------------------------------------------------------------------------------

const SHIFT: u16 = 1;
const CTRL: u16 = 1 << 1;
const MOD1: u16 = 1 << 2;
const MOD2: u16 = 1 << 3;
const MOD3: u16 = 1 << 4;
const MOD4: u16 = 1 << 5;
const MOD5: u16 = 1 << 6;
const SUPER: u16 = 1 << 7;
const HYPER: u16 = 1 << 8;
const META: u16 = 1 << 9;
const RELEASE: u16 = 1 << 10;
/// The modifiers keybind-find can't send (FR-019).
const UNSENDABLE: u16 = MOD2 | MOD3 | MOD5 | HYPER | META | RELEASE;

/// An accelerator as GTK keeps it: its modifier bits and its keysym's name (a letter lower-cased). Two are the same
/// key when equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Accel {
    mods: u16,
    key: String,
}

/// `gtk_accelerator_parse` (GTK 3.24.52 `gtk/gtkaccelgroup.c:1198-1432`): `<…>` modifiers ignoring case, then an X
/// keysym name, exactly. `None`: GTK's invalid accelerator (no key, a keycode, an unknown name, an unclosed `<`).
fn parse_accel(s: &str) -> Option<Accel> {
    let mut rest = s;
    let mut mods = 0;
    while let Some(r) = rest.strip_prefix('<') {
        let (m, after) = r.split_once('>')?;
        mods |= match m.to_ascii_lowercase().as_str() {
            "primary" | "control" | "ctrl" | "ctl" => CTRL,
            "shift" | "shft" => SHIFT,
            "alt" | "mod1" => MOD1,
            "mod2" => MOD2,
            "mod3" => MOD3,
            "mod4" => MOD4,
            "mod5" => MOD5,
            "super" => SUPER,
            "hyper" => HYPER,
            "meta" => META,
            "release" => RELEASE,
            // ponytail: skipped, as AC #6 pins it; GTK 3.24.52 stops at the `>` and fails the whole accelerator.
            _ => 0,
        };
        rest = after;
    }
    if !is_keysym(rest) {
        return None;
    }
    // `gdk_keyval_to_lower` (`:1429`), for the letters. ponytail: other cased keysyms (`Aacute`) keep their case.
    // GTK compares keyvals: a name sharing its keyval with another is `XKeysymToString`'s (keysymdef.h's first),
    // the name a pressed key gets (`xmodmap -pke`), so key search matches it.
    // ponytail: the aliases presets use; keysymdef.h has more (`Henkan`, `Hangul_switch`, …).
    let rest = match rest {
        "Page_Up" => "Prior",
        "Page_Down" => "Next",
        "KP_Page_Up" => "KP_Prior",
        "KP_Page_Down" => "KP_Next",
        "script_switch" => "Mode_switch",
        "quoteright" => "apostrophe",
        "quoteleft" => "grave",
        r => r,
    };
    let key = if rest.len() == 1 {
        rest.to_ascii_lowercase()
    } else {
        rest.to_string()
    };
    Some(Accel { mods, key })
}

/// The accelerator in keybind-find's notation; `None`: a modifier keybind-find can't send.
fn notation(a: &Accel) -> Option<String> {
    if a.mods & UNSENDABLE != 0 {
        return None;
    }
    let mods: Vec<&str> = [
        (CTRL, "ctrl"),
        (SHIFT, "shift"),
        (MOD1, "alt"),
        (SUPER | MOD4, "super"),
    ]
    .iter()
    .filter(|(bits, _)| a.mods & bits != 0)
    .map(|&(_, name)| name)
    .collect();
    Some(chord(&mods, &a.key))
}

// --- the actions ----------------------------------------------------------------------------------------------------

/// The pinned action table: each detailed id's label and section, and the base names (Inkscape's action check).
struct Table {
    rows: HashMap<String, (String, String)>,
    bases: HashSet<String>,
}

impl Table {
    fn new(pin: &Pin) -> Table {
        let mut rows = HashMap::new();
        let mut bases = HashSet::new();
        for a in &pin.action {
            // The first row of an id kept, as `unordered_map::emplace` keeps it.
            rows.entry(normalise(&a.id))
                .or_insert_with(|| (a.label.clone(), a.section.clone()));
            bases.insert(base(&a.id).to_string());
        }
        Table { rows, bases }
    }

    /// `add_shortcut`'s check (`shortcuts.cpp:531-540`): an action of the table by its name without the parameter,
    /// or an extension's (`app.<id>`, registered at run time; dotted, as extension ids are).
    fn knows(&self, name: &str) -> bool {
        self.bases.contains(base(name)) || is_extension(name)
    }

    /// The label and section Inkscape's preferences show (`inkscape-preferences.cpp:3571-3625`): the detailed id's,
    /// else its base id's label with the parameter, else the id; an empty section is `Misc`.
    fn describe(&self, name: &str, id: &str) -> (String, String) {
        let b = base(name);
        let (label, section) = match self.rows.get(name) {
            Some((l, s)) => (l.clone(), s.clone()),
            None => match self.rows.get(b) {
                Some((l, s)) => (format!("{l} {}", &name[b.len()..]), s.clone()),
                None if is_extension(name) => (id.to_string(), "Extensions".to_string()),
                None => (id.to_string(), String::new()),
            },
        };
        let section = if section.is_empty() {
            "Misc".to_string()
        } else {
            section
        };
        (label, section)
    }
}

/// A detailed action name without its parameter: `app.transform-rotate(90.0)` → `app.transform-rotate`.
fn base(name: &str) -> &str {
    name.split('(').next().unwrap_or(name)
}

fn is_extension(name: &str) -> bool {
    base(name)
        .strip_prefix("app.")
        .is_some_and(|r| r.contains('.'))
}

/// GTK's `gtk_normalise_detailed_action_name` as far as whitespace goes: none outside quotes in the parameter, so
/// `( 1.0)` is `(1.0)`. ponytail: not GVariant's printing, so `(1.0)` and `(1.00)` stay two actions.
fn normalise(name: &str) -> String {
    let Some(open) = name.find('(') else {
        return name.to_string();
    };
    let mut out = name[..open].to_string();
    // The quote open, and where it opened in `out`.
    let mut quote = None;
    for c in name[open..].chars() {
        match quote {
            Some((q, at)) if c == q => {
                quote = None;
                // `("Select")` is GVariant's `('Select')`.
                if q == '"' && !out[at + 1..].contains('\'') {
                    out.replace_range(at..at + 1, "'");
                    out.push('\'');
                    continue;
                }
            }
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some((c, out.len())),
            None if c.is_whitespace() => continue,
            None => {}
        }
        out.push(c);
    }
    out
}

/// What was left out, by reason.
#[derive(Debug, Default, PartialEq)]
struct LeftOut {
    unparsable: usize,
    unknown: usize,
    old_format: usize,
    includes: usize,
}

/// The actions and their keys, as GTK holds them after Inkscape's `add_shortcut` calls.
#[derive(Default)]
struct Keys {
    /// In the order each first got a key: the id as first written, and its keys.
    acts: Vec<(String, Vec<Accel>)>,
    /// A normalised name's place in `acts`.
    by_name: HashMap<String, usize>,
    /// Which action holds each key: one at most, since Inkscape takes a key from its holder before giving it out.
    holder: HashMap<Accel, usize>,
    left: LeftOut,
    /// The includes met so far (`MAX_INCLUDED`).
    included: usize,
}

impl Keys {
    /// `add_shortcut` (`shortcuts.cpp:523-563`): the key taken from whichever action holds it, then given to
    /// `action` when Inkscape knows it: appended in a preset, replacing the action's keys in a user file. A key GTK
    /// can't parse changes nothing (`gtk_application_accels_set_accels_for_action` refuses the whole list).
    fn add(&mut self, table: &Table, action: &str, key: &str, user: bool) {
        let Some(a) = parse_accel(key) else {
            self.left.unparsable += 1;
            return;
        };
        if let Some(i) = self.holder.remove(&a) {
            self.acts[i].1.retain(|k| *k != a);
        }
        let name = normalise(action);
        if !table.knows(&name) {
            // "No Action for".
            self.left.unknown += 1;
            return;
        }
        let i = *self.by_name.entry(name).or_insert_with(|| {
            self.acts.push((action.to_string(), Vec::new()));
            self.acts.len() - 1
        });
        if user {
            for k in std::mem::take(&mut self.acts[i].1) {
                self.holder.remove(&k);
            }
        }
        self.acts[i].1.push(a.clone());
        self.holder.insert(a, i);
    }
}

/// One bind per key, under its action's section, in the order the actions got their first key; and the note on what
/// was left out, when anything was.
fn to_sheet(keys: &Keys, table: &Table) -> (Sheet, Option<String>) {
    let mut sheet = Sheet {
        title: "Inkscape".into(),
        provider: "inkscape".into(),
        binds: Default::default(),
    };
    let mut unsendable = 0;
    for (id, accels) in &keys.acts {
        let (label, section) = table.describe(&normalise(id), id);
        for a in accels {
            let Some(key) = notation(a) else {
                unsendable += 1;
                continue;
            };
            // An id can come from the user's file: shown, so escaped.
            sheet.binds.entry(section.clone()).or_default().push(Bind {
                key,
                desc: printable(&label),
                action: printable(id),
                when: Default::default(),
                run: Vec::new(),
            });
        }
    }
    let l = &keys.left;
    let reasons: Vec<String> = [
        (unsendable, "key(s) with a Meta, Hyper, Mod2, Mod3, Mod5 or Release modifier keybind-find can't send"),
        (l.unparsable, "key(s) GTK can't parse"),
        (l.unknown, "key(s) bound to an action Inkscape doesn't have"),
        (l.old_format, "bind(s) in Inkscape 0.92's format (verb=, key=), which Inkscape 1 ignores"),
        (l.includes, "include(s) keybind-find doesn't follow (parse=\"text\", an xpointer, a URL)"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    let note = (!reasons.is_empty()).then(|| format!("leaving out {}", reasons.join("; ")));
    (sheet, note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::pinned::{assert_data, fake_bin, path_of};

    fn table() -> Table {
        Table::new(&toml::from_str(REGISTRY[0].1).unwrap())
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/inkscape")
            .join(name)
    }

    fn disk(p: &Path) -> Result<Option<String>, String> {
        read_capped(p, MAX_SIZE)
    }

    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-inkscape-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Files from disk applied in order (path, user): the keys, and the notes.
    fn apply(files: &[(&Path, bool)]) -> (Keys, Vec<String>) {
        let (t, mut keys, mut notes) = (table(), Keys::default(), Vec::new());
        for (path, user) in files {
            assert_eq!(
                apply_file(&mut keys, &t, path, *user, &disk, &mut notes),
                Ok(true),
                "{}",
                path.display()
            );
        }
        (keys, notes)
    }

    /// Keys files given as their `<keys>` element's content (text, user), applied in order through a reader seam.
    fn texts(files: &[(&str, bool)]) -> (Keys, Vec<String>) {
        let path = |i: usize| PathBuf::from(format!("/v/{i}.xml"));
        let map: HashMap<PathBuf, String> = files
            .iter()
            .enumerate()
            .map(|(i, (body, _))| (path(i), format!("<keys>{body}</keys>")))
            .collect();
        let read = |p: &Path| Ok(map.get(p).cloned());
        let (t, mut keys, mut notes) = (table(), Keys::default(), Vec::new());
        for (i, (_, user)) in files.iter().enumerate() {
            assert_eq!(
                apply_file(&mut keys, &t, &path(i), *user, &read, &mut notes),
                Ok(true)
            );
        }
        (keys, notes)
    }

    /// An action's keys in keybind-find's notation (`!` for one left out).
    fn keys_of(keys: &Keys, id: &str) -> Vec<String> {
        keys.by_name
            .get(&normalise(id))
            .map(|&i| {
                keys.acts[i]
                    .1
                    .iter()
                    .map(|a| notation(a).unwrap_or_else(|| "!".into()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// (section, key, desc, action).
    type Row = (String, String, String, String);

    /// The sheet's rows, and the left-out note.
    fn rows(keys: &Keys) -> (Vec<Row>, Option<String>) {
        let (sheet, note) = to_sheet(keys, &table());
        let rows = sheet
            .binds
            .iter()
            .flat_map(|(c, bs)| {
                bs.iter()
                    .map(move |b| (c.clone(), b.key.clone(), b.desc.clone(), b.action.clone()))
            })
            .collect();
        (rows, note)
    }

    fn row(rows: &[Row], key: &str) -> Vec<(String, String)> {
        rows.iter()
            .filter(|r| r.1 == key)
            .map(|r| (r.0.clone(), r.2.clone()))
            .collect()
    }

    fn pair(section: &str, desc: &str) -> Vec<(String, String)> {
        vec![(section.to_string(), desc.to_string())]
    }

    #[test]
    fn keys_as_gtk_parses_them() {
        let k = |s: &str| parse_accel(s).map(|a| notation(&a).unwrap_or_else(|| "!".into()));
        for (input, want) in [
            ("<Primary>z", Some("ctrl+z")),
            ("<primary>Z", Some("ctrl+z")),
            ("<CONTROL>z", Some("ctrl+z")),
            ("<Ctl>z", Some("ctrl+z")),
            ("<shift><alt>F1", Some("alt+shift+F1")),
            ("<Mod1>F1", Some("alt+F1")),
            ("<Mod4>x", Some("super+x")),
            ("<super>x", Some("super+x")),
            ("<Meta>x", Some("!")),
            ("<Hyper>x", Some("!")),
            ("<Mod3>x", Some("!")),
            ("<Release>x", Some("!")),
            ("<Foo>x", Some("x")),
            ("KP_Add", Some("KP_Add")),
            ("ISO_Left_Tab", Some("ISO_Left_Tab")),
            ("<primary>ampersand", Some("ctrl+ampersand")),
            ("<primary><shift>a", Some("ctrl+shift+a")),
            ("<primary>", None),
            ("return", None),
            ("0x26", None),
            ("b ", None),
            ("", None),
            ("<primary", None),
            ("<primary>alt>m", None),
            ("Page_Up", Some("Prior")),
            ("<ctrl>Page_Down", Some("ctrl+Next")),
            ("KP_Page_Up", Some("KP_Prior")),
            ("KP_Page_Down", Some("KP_Next")),
            ("script_switch", Some("Mode_switch")),
            ("<primary>quoteright", Some("ctrl+apostrophe")),
            ("quoteleft", Some("grave")),
            ("Prior", Some("Prior")),
        ] {
            assert_eq!(k(input).as_deref(), want, "{input:?}");
        }
        assert_eq!(split_keys("  a,b ,  c"), ["a", "b", "c"]);
        assert_eq!(split_keys("x  "), ["x  "]);
        assert_eq!(split_keys(" \tx"), ["\tx"]);
        assert!(split_keys("   ").is_empty());
    }

    #[test]
    fn the_stock_preset() {
        let (keys, notes) = apply(&[(&fixture("inkscape.xml"), false)]);
        assert!(notes.is_empty(), "{notes:?}");
        // `<primary>alt>m` (app.object-set-inverse-clip) is a typo GTK refuses.
        assert_eq!(
            keys.left,
            LeftOut {
                unparsable: 1,
                ..LeftOut::default()
            }
        );
        let (rows, note) = rows(&keys);
        assert_eq!(rows.len(), 269);
        assert_eq!(
            note.as_deref(),
            Some(
                "leaving out 3 key(s) with a Meta, Hyper, Mod2, Mod3, Mod5 or Release modifier keybind-find can't \
                 send; 1 key(s) GTK can't parse"
            )
        );
        assert_eq!(row(&rows, "q"), pair("Tools", "Quick Zoom"));
        assert_eq!(row(&rows, "ctrl+shift+e"), pair("Dialog", "Open Export"));
        assert_eq!(
            row(&rows, "ctrl+ampersand"),
            pair("Dialog", "Open Live Path Effect")
        );
        assert_eq!(
            row(&rows, "ctrl+shift+7"),
            pair("Dialog", "Open Live Path Effect")
        );
        assert_eq!(
            row(&rows, "ctrl+bracketright"),
            pair("Transform", "Rotate (90.0)")
        );
        assert_eq!(
            row(&rows, "bracketright"),
            pair("Transform", "Rotate Step (180.0)")
        );
        assert_eq!(row(&rows, "ctrl+Tab"), pair("View", "Next Window"));
        // `<primary><shift>Page_Up`: by XKeysymToString's name, as a pressed key is named.
        assert_eq!(
            keys_of(&keys, "win.layer-raise"),
            ["ctrl+shift+Prior", "ctrl+shift+KP_Prior"]
        );
        assert_eq!(keys_of(&keys, "win.window-next"), ["ctrl+Tab", "!"]);
        // The action as written in the file.
        assert!(rows
            .iter()
            .any(|r| r.1 == "ctrl+bracketright" && r.3 == "app.transform-rotate(90.0)"));
    }

    #[test]
    fn xara_includes_the_stock_preset() {
        let (keys, notes) = apply(&[(&fixture("xara.xml"), false)]);
        assert!(notes.is_empty(), "{notes:?}");
        let (rows, _) = rows(&keys);
        assert_eq!(row(&rows, "alt+s"), pair("Tool Switch", "Selector Tool"));
        assert_eq!(row(&rows, "F2"), pair("Tool Switch", "Selector Tool"));
        assert_eq!(
            keys_of(&keys, "win.tool-switch('Select')"),
            ["s", "F1", "F2", "alt+s"]
        );
        // `<ctrl>n, <ctrl>N`: one key.
        assert_eq!(keys_of(&keys, "win.document-new"), ["ctrl+n"]);
        // F2 was the Node tool's; xara gives it F4, n, N (one n).
        assert_eq!(keys_of(&keys, "win.tool-switch('Node')"), ["F4", "n"]);
    }

    #[test]
    fn the_overlay() {
        let preset = r#"<bind gaction="doc.undo" keys="&lt;primary&gt;z, F9"/><bind gaction="doc.redo" keys="F7"/>"#;
        // A user bind replaces all the preset's keys of its action.
        let (keys, _) = texts(&[
            (preset, false),
            (r#"<bind gaction="doc.undo" keys="F8"/>"#, true),
        ]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["F8"]);
        // `keys="a, b"` in a user file leaves only b; a is no one's.
        let (keys, _) = texts(&[
            (preset, false),
            (r#"<bind gaction="doc.undo" keys="a, b"/>"#, true),
        ]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["b"]);
        assert!(!keys.holder.contains_key(&parse_accel("a").unwrap()));
        // A user key steals the key from a preset action, which is no longer listed.
        let (keys, _) = texts(&[
            (preset, false),
            (r#"<bind gaction="doc.undo" keys="F7"/>"#, true),
        ]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["F7"]);
        assert!(keys_of(&keys, "doc.redo").is_empty());
        assert!(!rows(&keys).0.iter().any(|r| r.3 == "doc.redo"));
        // `keys=""`, a bind without keys: nothing changes.
        let (keys, _) = texts(&[
            (preset, false),
            (
                r#"<bind gaction="doc.undo" keys=""/><bind gaction="doc.redo"/>"#,
                true,
            ),
        ]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["ctrl+z", "F9"]);
        assert_eq!(keys_of(&keys, "doc.redo"), ["F7"]);
        assert_eq!(keys.left, LeftOut::default());
        // Unparsable pieces change nothing and are counted.
        let (keys, _) = texts(&[
            (preset, false),
            (
                r#"<bind gaction="doc.undo" keys="x  "/><bind gaction="doc.redo" keys="0x26"/>"#,
                true,
            ),
        ]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["ctrl+z", "F9"]);
        assert_eq!(keys_of(&keys, "doc.redo"), ["F7"]);
        assert_eq!(keys.left.unparsable, 2);
        // An unknown action takes the key from its holder, and gets nothing.
        let (keys, _) = texts(&[
            (preset, false),
            (r#"<bind gaction="app.no-such" keys="F7"/>"#, true),
        ]);
        assert!(keys_of(&keys, "doc.redo").is_empty());
        assert_eq!(keys.left.unknown, 1);
        assert!(!rows(&keys).0.iter().any(|r| r.1 == "F7"));
        // An extension's action: its id, under Extensions.
        let (keys, _) = texts(&[(
            r#"<bind gaction="app.org.inkscape.color.brighter" keys="F6"/>"#,
            true,
        )]);
        assert_eq!(
            rows(&keys).0,
            [(
                "Extensions".to_string(),
                "F6".to_string(),
                "app.org.inkscape.color.brighter".to_string(),
                "app.org.inkscape.color.brighter".to_string()
            )]
        );
        // Whitespace in the parameter: one action.
        let (keys, _) = texts(&[(
            r#"<bind gaction="win.path-inset-screen(1.0)" keys="F5"/><bind gaction="win.path-inset-screen( 1.0)" keys="F6"/>"#,
            false,
        )]);
        assert_eq!(keys_of(&keys, "win.path-inset-screen(1.0)"), ["F5", "F6"]);
        assert_eq!(
            row(&rows(&keys).0, "F6"),
            pair("Path", "Inset Screen (1.0)")
        );
        // Nested <keys> read in place; <modifier> and other elements ignored; 0.92's format counted.
        let (keys, _) = texts(&[(
            r#"<modifier action="canvas-rotate" modifiers="Shift"/><keys><bind gaction="doc.undo" keys="F3"/></keys>
               <other/><bind key="a" modifiers="Ctrl" verb="EditCopy"/>"#,
            false,
        )]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["F3"]);
        assert_eq!(
            keys.left,
            LeftOut {
                old_format: 1,
                ..LeftOut::default()
            }
        );
        let (_, note) = rows(&keys);
        assert!(note.is_some_and(|n| n.contains("1 bind(s) in Inkscape 0.92's format")));
    }

    #[test]
    fn xinclude() {
        let d = tmp("xinclude");
        let xi = r#"xmlns:xi="http://www.w3.org/2001/XInclude""#;
        // A cycle: noted once, the rest read.
        write(
            &d.join("a.xml"),
            &format!(
                r#"<keys {xi}><xi:include href="b.xml"/><bind gaction="doc.undo" keys="F1"/></keys>"#
            ),
        );
        write(
            &d.join("b.xml"),
            &format!(
                r#"<keys {xi}><xi:include href="a.xml"/><bind gaction="doc.redo" keys="F2"/></keys>"#
            ),
        );
        let (keys, notes) = apply(&[(&d.join("a.xml"), false)]);
        assert_eq!(
            (keys_of(&keys, "doc.undo"), keys_of(&keys, "doc.redo")),
            (vec!["F1".into()], vec!["F2".into()])
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("an include cycle"), "{notes:?}");
        // Nine deep: the ninth include is dropped, the eight above it read.
        for i in 0..10 {
            write(
                &d.join(format!("d{i}.xml")),
                &format!(
                    r#"<keys {xi}><bind gaction="doc.undo" keys="F{}"/><xi:include href="d{}.xml"/></keys>"#,
                    i + 1,
                    i + 1
                ),
            );
        }
        let (keys, notes) = apply(&[(&d.join("d0.xml"), false)]);
        assert_eq!(
            keys_of(&keys, "doc.undo"),
            (1..=9).map(|i| format!("F{i}")).collect::<Vec<_>>()
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("more than 8 includes deep"), "{notes:?}");
        // A missing file: noted. parse="text" and an xpointer: dropped and counted. An absolute href: followed.
        write(
            &d.join("abs.xml"),
            r#"<bind gaction="doc.redo" keys="F12"/>"#,
        );
        write(
            &d.join("m.xml"),
            &format!(
                r#"<keys {xi}><xi:include href="missing.xml"/><xi:include href="a.xml" parse="text"/>
                   <xi:include href="a.xml" xpointer="x"/><xi:include href="{}"/></keys>"#,
                d.join("abs.xml").display()
            ),
        );
        let (keys, notes) = apply(&[(&d.join("m.xml"), false)]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("missing.xml: it doesn't exist"),
            "{notes:?}"
        );
        assert_eq!(keys.left.includes, 2);
        assert_eq!(keys_of(&keys, "doc.redo"), ["F12"]);
        // The old 2003 namespace too; another namespace's `include` is no include.
        write(
            &d.join("ns.xml"),
            r#"<keys xmlns:a="http://www.w3.org/2003/XInclude" xmlns:o="urn:other"><a:include href="abs.xml"/><o:include href="missing.xml"/></keys>"#,
        );
        let (keys, notes) = apply(&[(&d.join("ns.xml"), false)]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            (keys_of(&keys, "doc.redo"), keys.left.includes),
            (vec!["F12".into()], 0)
        );
    }

    #[test]
    fn xml_inkscape_reads_and_doesnt() {
        let d = tmp("xml");
        let (t, mut keys, mut notes) = (table(), Keys::default(), Vec::new());
        let mut load = |name: &str, text: &[u8]| {
            std::fs::write(d.join(name), text).unwrap();
            apply_file(&mut keys, &t, &d.join(name), false, &disk, &mut notes)
        };
        assert!(load("dtd.xml", b"<!DOCTYPE keys><keys/>").is_err());
        assert!(load("prefix.xml", b"<keys><xi:include href=\"x.xml\"/></keys>").is_err());
        assert_eq!(
            load("latin1.xml", b"<keys name=\"\xe9\"/>"),
            Err("not UTF-8".to_string())
        );
        assert_eq!(
            load("root.xml", b"<config/>"),
            Err("its root element is <config>, not <keys>".to_string())
        );
        assert_eq!(
            load(
                "ns.xml",
                br#"<k:keys xmlns:k="urn:x"><k:bind gaction="doc.undo" keys="F1"/></k:keys>"#
            ),
            Ok(true)
        );
        assert_eq!(keys_of(&keys, "doc.undo"), ["F1"]);
    }

    /// `lookup` with env `vars`, from disk.
    fn look(vars: &[(&str, &Path)]) -> (Keys, Vec<String>) {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.display().to_string()))
            .collect();
        let mut notes = Vec::new();
        let keys = lookup(&|k| vars.get(k).cloned(), &disk, &table(), &mut notes);
        (keys, notes)
    }

    /// A data dir with `inkscape.xml` and `xara.xml` (the fixtures) and, when `default`, a `default.xml` binding
    /// only `z` to Quick Zoom.
    fn data(d: &Path, default: bool) -> PathBuf {
        let keys = d.join("inkscape/keys");
        std::fs::create_dir_all(&keys).unwrap();
        for f in ["inkscape.xml", "xara.xml"] {
            std::fs::copy(fixture(f), keys.join(f)).unwrap();
        }
        if default {
            write(
                &keys.join("default.xml"),
                r#"<keys><bind gaction="tool.all.quick-zoom" keys="z"/></keys>"#,
            );
        }
        d.to_path_buf()
    }

    fn prefs(profile: &Path, options: &str) {
        write(
            &profile.join("preferences.xml"),
            &format!(r#"<inkscape><group id="options">{options}</group></inkscape>"#),
        );
    }

    #[test]
    fn which_files() {
        let d = tmp("lookup");
        let data_dir = data(&d.join("data"), true);
        let (p, dd) = (d.join("profile"), &data_dir as &Path);
        let quick = |k: &Keys| keys_of(k, "tool.all.quick-zoom");
        // No preferences: default.xml.
        let (keys, notes) = look(&[("INKSCAPE_DATADIR", dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        assert_eq!((quick(&keys), notes), (vec!["z".to_string()], vec![]));
        // The preferences' preset (the decoy kbshortcuts not used); the user's file over it.
        std::fs::create_dir_all(&p).unwrap();
        std::fs::copy(fixture("preferences.xml"), p.join("preferences.xml")).unwrap();
        write(
            &p.join("keys/default.xml"),
            &std::fs::read_to_string(fixture("user.xml")).unwrap(),
        );
        let (keys, notes) = look(&[("INKSCAPE_DATADIR", dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            keys_of(&keys, "win.tool-switch('Select')"),
            ["s", "F1", "F2", "alt+s"]
        );
        assert_eq!(
            keys_of(&keys, "win.dialog-open('Export')"),
            ["ctrl+alt+F12"]
        );
        assert_eq!(keys_of(&keys, "doc.undo"), ["F4"]);
        // An absolute preset path.
        prefs(
            &p,
            &format!(
                r#"<group id="kbshortcuts" shortcutfile="{}"/>"#,
                fixture("xara.xml").display()
            ),
        );
        let (keys, _) = look(&[("INKSCAPE_DATADIR", dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        assert_eq!(
            keys_of(&keys, "win.tool-switch('Select')"),
            ["s", "F1", "F2", "alt+s"]
        );
        // A preset that doesn't load: a note, then default.xml.
        prefs(&p, r#"<group id="kbshortcuts" shortcutfile="nope.xml"/>"#);
        let (keys, notes) = look(&[("INKSCAPE_DATADIR", dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        assert_eq!(quick(&keys), ["z"]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("nope.xml: the preset your preferences name doesn't exist"),
            "{notes:?}"
        );
        // No default.xml: inkscape.xml.
        let dd2 = data(&d.join("data2"), false);
        let (keys, notes) = look(&[
            ("INKSCAPE_DATADIR", &dd2),
            ("INKSCAPE_PROFILE_DIR", &d.join("none")),
        ]);
        assert_eq!((quick(&keys), notes), (vec!["q".to_string()], vec![]));
        // No preset at all: a note, and the user's keys alone.
        let (keys, notes) = look(&[
            ("INKSCAPE_DATADIR", &d.join("empty")),
            ("INKSCAPE_PROFILE_DIR", &p),
        ]);
        assert_eq!(rows(&keys).0.len(), 2);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[1].starts_with("no keyboard preset loads from"),
            "{notes:?}"
        );
        // XDG_CONFIG_HOME's inkscape, and HOME's when it is empty.
        let x = d.join("xdg");
        write(
            &x.join("inkscape/keys/default.xml"),
            r#"<keys><bind gaction="doc.undo" keys="F5"/></keys>"#,
        );
        let (keys, _) = look(&[
            ("INKSCAPE_DATADIR", dd),
            ("XDG_CONFIG_HOME", &x),
            ("HOME", &d),
        ]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["F5"]);
        write(
            &d.join(".config/inkscape/keys/default.xml"),
            r#"<keys><bind gaction="doc.undo" keys="F6"/></keys>"#,
        );
        let (keys, _) = look(&[
            ("INKSCAPE_DATADIR", dd),
            ("XDG_CONFIG_HOME", Path::new("")),
            ("HOME", &d),
        ]);
        assert_eq!(keys_of(&keys, "doc.undo"), ["F6"]);
        // The shared dir's file before the user's.
        let shared = d.join("shared");
        write(
            &shared.join("keys/default.xml"),
            r#"<keys><bind gaction="doc.undo" keys="F7"/><bind gaction="doc.redo" keys="F8"/></keys>"#,
        );
        let p2 = d.join("profile2");
        prefs(
            &p2,
            &format!(
                r#"<group id="resources" sharedpath="{}"/>"#,
                shared.display()
            ),
        );
        write(
            &p2.join("keys/default.xml"),
            r#"<keys><bind gaction="doc.undo" keys="F9"/></keys>"#,
        );
        let (keys, notes) = look(&[("INKSCAPE_DATADIR", dd), ("INKSCAPE_PROFILE_DIR", &p2)]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            (keys_of(&keys, "doc.undo"), keys_of(&keys, "doc.redo")),
            (vec!["F9".into()], vec!["F8".into()])
        );
        // A user file that doesn't load: a note, the preset kept.
        write(&p2.join("keys/default.xml"), "<!DOCTYPE keys><keys/>");
        let (keys, notes) = look(&[("INKSCAPE_DATADIR", dd), ("INKSCAPE_PROFILE_DIR", &p2)]);
        assert_eq!(quick(&keys), ["z"]);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("profile2/keys/default.xml") && n.ends_with("skipped")),
            "{notes:?}"
        );
    }

    #[test]
    fn the_data_dir_of_the_inkscape_on_path() {
        let d = tmp("prefix");
        let prefix = d.join("opt/ink");
        write(&prefix.join("bin/inkscape"), "");
        data(&prefix.join("share"), true);
        std::fs::create_dir_all(d.join("path")).unwrap();
        std::os::unix::fs::symlink(prefix.join("bin/inkscape"), d.join("path/inkscape")).unwrap();
        let (keys, _) = look(&[
            ("PATH", &d.join("path")),
            ("INKSCAPE_PROFILE_DIR", &d.join("none")),
        ]);
        assert_eq!(keys_of(&keys, "tool.all.quick-zoom"), ["z"]);
        assert_eq!(data_dir(&|_| None), PathBuf::from("/usr/share"));
    }

    #[test]
    fn files_that_cant_be_read_are_skipped() {
        let d = tmp("fifo");
        std::fs::create_dir_all(d.join("keys")).unwrap();
        let made = Command::new("mkfifo")
            .arg(d.join("keys/default.xml"))
            .status()
            .unwrap();
        assert!(made.success());
        let (_, notes) = look(&[("INKSCAPE_DATADIR", &d), ("INKSCAPE_PROFILE_DIR", &d)]);
        assert!(
            notes
                .iter()
                .any(|n| n.ends_with("not a regular file; skipped")),
            "{notes:?}"
        );
        std::fs::remove_file(d.join("keys/default.xml")).unwrap();
        write(
            &d.join("keys/default.xml"),
            &format!("<keys>{}</keys>", " ".repeat(MAX_SIZE as usize)),
        );
        let (_, notes) = look(&[("INKSCAPE_DATADIR", &d), ("INKSCAPE_PROFILE_DIR", &d)]);
        assert!(
            notes
                .iter()
                .any(|n| n.ends_with("larger than 4 MiB; skipped")),
            "{notes:?}"
        );
    }

    #[test]
    fn version_picks_the_table() {
        assert_data("inkscape", &format!("{}.toml", REGISTRY[0].0));
        let d = tmp("version");
        let run = |body: &str| {
            let bin = fake_bin("inkscape", body);
            let env = |k: &str| match k {
                "INKSCAPE_DATADIR" => Some(data(&d, false).display().to_string()),
                "INKSCAPE_PROFILE_DIR" => Some(d.join("none").display().to_string()),
                "PATH" => Some(bin.display().to_string()),
                _ => None,
            };
            read(&env, path_of(&bin)).unwrap()
        };
        let (sheet, notes, source) = run("echo 'Inkscape 1.4.4 (dcaf3e7d9e, 2026-05-04)'");
        assert_eq!(source, pinned::source("1.4.4", Some("1.4.4"), false));
        assert!(!notes.iter().any(|n| n.contains("version")), "{notes:?}");
        assert_eq!(sheet.provider, "inkscape");
        let (_, notes, source) = run("echo 'Inkscape 1.5.0 (0123456789, 2027-01-01)'");
        assert_eq!(source, pinned::source("1.4.4", Some("1.5.0"), true));
        assert_eq!(
            notes[0],
            "inkscape 1.5.0: using the defaults of 1.4.4; add data/inkscape/1.5.0.toml"
        );
        let (sheet, notes, source) = run("exit 127");
        assert_eq!(source, pinned::source("1.4.4", None, true));
        assert_eq!(
            notes[0],
            "inkscape's version is unknown: using the defaults of 1.4.4"
        );
        assert!(!sheet.binds.is_empty());
    }

    #[test]
    fn included_files_are_capped_in_all() {
        // Ten files each including the next one ten times: 10^9 reads without the cap.
        let d = tmp("fanout");
        let xi = r#"xmlns:xi="http://www.w3.org/2001/XInclude""#;
        for i in 0..8 {
            let inc = format!(r#"<xi:include href="f{}.xml"/>"#, i + 1).repeat(10);
            write(
                &d.join(format!("f{i}.xml")),
                &format!("<keys {xi}>{inc}</keys>"),
            );
        }
        write(&d.join("f8.xml"), r#"<bind gaction="doc.undo" keys="F1"/>"#);
        let (keys, notes) = apply(&[(&d.join("f0.xml"), false)]);
        // 64 taken; each include met past the cap counted (the files above f8 hold 10 each).
        assert!(keys.included > MAX_INCLUDED);
        assert_eq!(keys.left.includes, keys.included - MAX_INCLUDED);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with("more than 64 includes in all; the rest are left out"),
            "{notes:?}"
        );
        // The first chain down reached f8 before the cap.
        assert_eq!(keys_of(&keys, "doc.undo"), ["F1"]);
    }

    #[test]
    fn more_includes() {
        let d = tmp("includes");
        let xi = r#"xmlns:xi="http://www.w3.org/2001/XInclude""#;
        // A URL, a missing href, a file that doesn't parse.
        write(&d.join("bad.xml"), "<keys><bind></keys>");
        write(
            &d.join("a.xml"),
            &format!(
                r#"<keys {xi}><xi:include href="http://example.org/k.xml"/><xi:include/><xi:include href=""/><xi:include href="bad.xml"/></keys>"#
            ),
        );
        let (keys, notes) = apply(&[(&d.join("a.xml"), false)]);
        // The URL, no href and an empty one: counted; the file that doesn't parse: a note.
        assert_eq!(keys.left.includes, 3);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("including ")
                && notes[0].contains("bad.xml: ")
                && notes[0].ends_with("its keys are left out"),
            "{notes:?}"
        );
        assert_eq!(
            rows(&keys).1.as_deref(),
            Some("leaving out 3 include(s) keybind-find doesn't follow (parse=\"text\", an xpointer, a URL)")
        );
        assert!(
            has_scheme("file:///x.xml")
                && has_scheme("https://x")
                && !has_scheme("dir/x.xml")
                && !has_scheme("1a:b")
        );
        // A nested include is resolved from the included file's own directory.
        write(
            &d.join("sub/y.xml"),
            &format!(r#"<keys {xi}><xi:include href="z.xml"/></keys>"#),
        );
        write(
            &d.join("sub/z.xml"),
            r#"<bind gaction="doc.redo" keys="F2"/>"#,
        );
        write(
            &d.join("x.xml"),
            &format!(r#"<keys {xi}><xi:include href="sub/y.xml"/></keys>"#),
        );
        let (keys, notes) = apply(&[(&d.join("x.xml"), false)]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(keys_of(&keys, "doc.redo"), ["F2"]);
    }

    #[test]
    fn labels_sections_and_notes() {
        // A known base with a parameter the table lacks: the id, under Misc. A clean file: no note.
        let (keys, _) = texts(&[(
            r#"<bind gaction="win.canvas-display-mode(9)" keys="F3"/>"#,
            false,
        )]);
        assert_eq!(
            row(&rows(&keys).0, "F3"),
            pair("Misc", "win.canvas-display-mode(9)")
        );
        assert_eq!(rows(&keys).1, None);
        // The unknown reason's text.
        let (keys, _) = texts(&[(r#"<bind gaction="app.no-such" keys="F3"/>"#, false)]);
        assert_eq!(
            rows(&keys).1.as_deref(),
            Some("leaving out 1 key(s) bound to an action Inkscape doesn't have")
        );
        // A <meta> key takes its key from another action, then is left out.
        let (keys, _) = texts(&[(
            r#"<bind gaction="doc.undo" keys="&lt;meta&gt;z"/><bind gaction="doc.redo" keys="&lt;Meta&gt;Z"/>"#,
            false,
        )]);
        assert!(keys_of(&keys, "doc.undo").is_empty());
        assert_eq!(keys_of(&keys, "doc.redo"), ["!"]);
        let (listed, note) = rows(&keys);
        assert!(listed.is_empty());
        assert!(note.is_some_and(|n| n.starts_with("leaving out 1 key(s) with a Meta")));
        // Keysym aliases are one key: a user <ctrl>Prior takes <primary>Page_Up.
        let (keys, _) = texts(&[
            (
                r#"<bind gaction="doc.undo" keys="&lt;primary&gt;Page_Up"/>"#,
                false,
            ),
            (
                r#"<bind gaction="doc.redo" keys="&lt;ctrl&gt;Prior"/>"#,
                true,
            ),
        ]);
        assert!(keys_of(&keys, "doc.undo").is_empty());
        assert_eq!(keys_of(&keys, "doc.redo"), ["ctrl+Prior"]);
        // ("Select") is ('Select').
        assert_eq!(
            normalise(r#"win.tool-switch("Select")"#),
            "win.tool-switch('Select')"
        );
        assert_eq!(normalise(r#"win.x( "it's" )"#), r#"win.x("it's")"#);
        let (keys, _) = texts(&[(
            r#"<bind gaction="win.tool-switch(&quot;Select&quot;)" keys="F3"/>"#,
            false,
        )]);
        assert_eq!(
            row(&rows(&keys).0, "F3"),
            pair("Tool Switch", "Selector Tool")
        );
    }

    #[test]
    fn env_precedence() {
        let d = tmp("env");
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            profile_dir(&env(&[
                ("INKSCAPE_PROFILE_DIR", "/p"),
                ("XDG_CONFIG_HOME", "/x")
            ])),
            Some(PathBuf::from("/p"))
        );
        assert_eq!(
            profile_dir(&env(&[
                ("INKSCAPE_PROFILE_DIR", ""),
                ("XDG_CONFIG_HOME", "/x")
            ])),
            Some(PathBuf::from("/x/inkscape"))
        );
        let prefix = d.join("opt");
        write(&prefix.join("bin/inkscape"), "");
        std::fs::create_dir_all(prefix.join("share/inkscape/keys")).unwrap();
        let path = prefix.join("bin").display().to_string();
        let both = |datadir: &str| {
            let path = path.clone();
            let datadir = datadir.to_string();
            data_dir(&move |k: &str| match k {
                "INKSCAPE_DATADIR" => Some(datadir.clone()),
                "PATH" => Some(path.clone()),
                _ => None,
            })
        };
        assert_eq!(both("/data"), PathBuf::from("/data"));
        assert_eq!(both(""), prefix.join("share"));
        // A wrapper script gives its own prefix when that has an inkscape dir, else /usr/share.
        let wrapper = d.join("home/.local");
        write(&wrapper.join("bin/inkscape"), "");
        let p = wrapper.join("bin").display().to_string();
        let on_path = move || {
            let p = p.clone();
            data_dir(&move |k: &str| (k == "PATH").then(|| p.clone()))
        };
        assert_eq!(on_path(), PathBuf::from("/usr/share"));
        std::fs::create_dir_all(wrapper.join("share/inkscape")).unwrap();
        assert_eq!(on_path(), wrapper.join("share"));
        // An inkscape in a folder not named bin: /usr/share.
        write(&d.join("tools/inkscape"), "");
        std::fs::create_dir_all(d.join("share/inkscape")).unwrap();
        let t = d.join("tools").display().to_string();
        assert_eq!(
            data_dir(&move |k: &str| (k == "PATH").then(|| t.clone())),
            PathBuf::from("/usr/share")
        );
    }

    #[test]
    fn preferences_and_presets_that_dont_load() {
        let d = tmp("prefs");
        let dd = data(&d.join("data"), true);
        let p = d.join("profile");
        let look_p = || look(&[("INKSCAPE_DATADIR", &dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        let quick = |k: &Keys| keys_of(k, "tool.all.quick-zoom");
        // Malformed preferences: one note, read as none.
        write(
            &p.join("preferences.xml"),
            "<inkscape><group id=\"options\">",
        );
        let (keys, notes) = look_p();
        assert_eq!(quick(&keys), ["z"]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("preferences.xml: ")
                && notes[0].ends_with("; read as no preferences"),
            "{notes:?}"
        );
        // An empty shortcutfile, and a sharedpath that isn't a directory: no note, default.xml.
        prefs(
            &p,
            &format!(
                r#"<group id="kbshortcuts" shortcutfile=""/><group id="resources" sharedpath="{}"/>"#,
                p.join("preferences.xml").display()
            ),
        );
        let (keys, notes) = look_p();
        assert_eq!((quick(&keys), notes), (vec!["z".to_string()], vec![]));
        // The named preset doesn't parse: a note, default.xml.
        write(&dd.join("inkscape/keys/broken.xml"), "<keys>");
        prefs(&p, r#"<group id="kbshortcuts" shortcutfile="broken.xml"/>"#);
        let (keys, notes) = look_p();
        assert_eq!(quick(&keys), ["z"]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("broken.xml: ") && notes[0].ends_with("; this preset is skipped"),
            "{notes:?}"
        );
        // default.xml doesn't load either: a note, inkscape.xml.
        write(&dd.join("inkscape/keys/default.xml"), "<config/>");
        let (keys, notes) = look_p();
        assert_eq!(quick(&keys), ["q"]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(
            notes[1].ends_with(
                "default.xml: its root element is <config>, not <keys>; this preset is skipped"
            ),
            "{notes:?}"
        );
    }

    #[test]
    fn the_include_cap_is_one_budget() {
        // Counted reads: exactly 64 includes followed, across the preset and the user's file.
        let n = std::cell::Cell::new(0);
        let inc = |k: usize| r#"<xi:include href="a.xml"/>"#.repeat(k);
        let xi = r#"xmlns:xi="http://www.w3.org/2001/XInclude""#;
        let preset = format!("<keys {xi}>{}</keys>", inc(40));
        let user = format!("<keys {xi}>{}</keys>", inc(40));
        let read = |p: &Path| {
            Ok(Some(match p.to_str() {
                Some("/v/preset.xml") => preset.clone(),
                Some("/v/user.xml") => user.clone(),
                _ => {
                    n.set(n.get() + 1);
                    r#"<bind gaction="doc.undo" keys="F1"/>"#.to_string()
                }
            }))
        };
        let (t, mut keys, mut notes) = (table(), Keys::default(), Vec::new());
        for (f, user) in [("/v/preset.xml", false), ("/v/user.xml", true)] {
            assert_eq!(
                apply_file(&mut keys, &t, Path::new(f), user, &read, &mut notes),
                Ok(true)
            );
        }
        assert_eq!(n.get(), MAX_INCLUDED);
        assert_eq!(keys.left.includes, 16);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("/v/user.xml: more than 64 includes"),
            "{notes:?}"
        );
    }

    #[test]
    fn round_two_corners() {
        assert_eq!(normalise("x( 'a b' )"), "x('a b')");
        assert_eq!(profile_dir(&|_| None), None);
        let d = tmp("round2");
        let dd = data(&d.join("data"), true);
        // No profile at all: the preset only, no note.
        let (keys, notes) = look(&[("INKSCAPE_DATADIR", &dd)]);
        assert_eq!(
            (keys_of(&keys, "tool.all.quick-zoom"), notes),
            (vec!["z".to_string()], vec![])
        );
        // Two options groups: the first wins.
        let p = d.join("profile");
        write(
            &p.join("preferences.xml"),
            r#"<inkscape><group id="options"><group id="kbshortcuts" shortcutfile="xara.xml"/></group>
               <group id="options"><group id="kbshortcuts" shortcutfile="nope.xml"/></group></inkscape>"#,
        );
        let (keys, notes) = look(&[("INKSCAPE_DATADIR", &dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            keys_of(&keys, "win.tool-switch('Select')"),
            ["s", "F1", "F2", "alt+s"]
        );
        // A named preset that is default.xml is read once; a missing one falls back.
        prefs(
            &p,
            r#"<group id="kbshortcuts" shortcutfile="default.xml"/>"#,
        );
        let reads = std::cell::RefCell::new(Vec::new());
        let counting = |path: &Path| {
            reads.borrow_mut().push(path.to_path_buf());
            disk(path)
        };
        std::fs::write(dd.join("inkscape/keys/default.xml"), "<config/>").unwrap();
        let vars: HashMap<&str, String> = [
            ("INKSCAPE_DATADIR", dd.display().to_string()),
            ("INKSCAPE_PROFILE_DIR", p.display().to_string()),
        ]
        .into();
        let mut notes = Vec::new();
        lookup(&|k| vars.get(k).cloned(), &counting, &table(), &mut notes);
        let default = dd.join("inkscape/keys/default.xml");
        assert_eq!(reads.borrow().iter().filter(|r| **r == default).count(), 1);
        assert_eq!(notes.len(), 1, "{notes:?}");
        prefs(
            &p,
            &format!(
                r#"<group id="kbshortcuts" shortcutfile="{}"/>"#,
                d.join("gone.xml").display()
            ),
        );
        std::fs::remove_dir_all(dd.join("inkscape/keys")).unwrap();
        let (_, notes) = look(&[("INKSCAPE_DATADIR", &dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        assert!(notes[0].ends_with("gone.xml: the preset your preferences name doesn't exist; falling back to the stock presets"), "{notes:?}");
        assert!(
            notes[1].starts_with(&format!(
                "no keyboard preset loads ({} nor ",
                d.join("gone.xml").display()
            )),
            "{notes:?}"
        );
        // The shared folder's broken keys/default.xml: a note.
        let shared = d.join("shared");
        write(&shared.join("keys/default.xml"), "<keys>");
        prefs(
            &p,
            &format!(
                r#"<group id="resources" sharedpath="{}"/>"#,
                shared.display()
            ),
        );
        let (_, notes) = look(&[("INKSCAPE_DATADIR", &dd), ("INKSCAPE_PROFILE_DIR", &p)]);
        assert!(
            notes
                .iter()
                .any(|n| n.contains("shared/keys/default.xml: ") && n.ends_with("; skipped")),
            "{notes:?}"
        );
    }

    #[test]
    fn control_characters_are_escaped() {
        let d = tmp("ctrl");
        let keys = d.join("inkscape/keys");
        write(
            &keys.join("default.xml"),
            r#"<keys xmlns:xi="http://www.w3.org/2001/XInclude"><bind gaction="app.org.x&#9;y" keys="F1"/><xi:include href="no&#9;such.xml"/></keys>"#,
        );
        let bin = fake_bin("inkscape", "echo 'Inkscape 1.4.4 (x)'");
        let env = |k: &str| match k {
            "INKSCAPE_DATADIR" => Some(d.display().to_string()),
            "INKSCAPE_PROFILE_DIR" => Some(d.join("none").display().to_string()),
            _ => None,
        };
        let (sheet, notes, _) = read(&env, path_of(&bin)).unwrap();
        let b = &sheet.binds["Extensions"][0];
        assert_eq!(
            (b.desc.as_str(), b.action.as_str()),
            ("app.org.x\\ty", "app.org.x\\ty")
        );
        assert!(
            notes.iter().any(|n| n.contains("no\\tsuch.xml")),
            "{notes:?}"
        );
        assert!(!notes.iter().any(|n| n.contains('\t')), "{notes:?}");
        // The version from the first line that starts with `Inkscape `.
        let bin = fake_bin(
            "inkscape",
            "echo 'Gtk-WARNING: x'; echo 'Inkscape 1.4.4 (x)'",
        );
        let (_, _, source) = read(&env, path_of(&bin)).unwrap();
        assert_eq!(source.installed.as_deref(), Some("1.4.4"));
    }

    #[test]
    fn window_classes() {
        for c in [
            "Inkscape",
            "inkscape",
            "org.inkscape.Inkscape",
            "org.inkscape.Inkscape.e2e",
            "ORG.INKSCAPE.INKSCAPE.x",
        ] {
            assert!(is_inkscape(c), "{c}");
        }
        for c in ["org.inkscape.InkscapeX", "inkscape-x", "org.inkscape", "é"] {
            assert!(!is_inkscape(c), "{c}");
        }
    }
}
