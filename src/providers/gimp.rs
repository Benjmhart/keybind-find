//! GIMP 3: its built-in default shortcuts, pinned per version (`data/gimp/`, from GIMP's own `shortcutsrc` and its
//! source's menu labels), with the user's `shortcutsrc` laid over them as GIMP lays it (`app/menus/shortcuts-rc.c`):
//! GIMP ignores the file's commented lines and takes its defaults from its code. Nothing is run but `gimp --version`.

use super::openbox::is_keysym;
use super::pinned;
use crate::sheet::{chord, Bind, Sheet, Source};
use anyhow::{ensure, Context as _};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One entry per GIMP version whose defaults we know, oldest first (3.2.8's are 3.2.6's: the same file and labels).
const REGISTRY: &[(&str, &str)] = &[
    (
        "3.2.6",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/gimp/3.2.6.toml")),
    ),
    (
        "3.2.8",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/gimp/3.2.8.toml")),
    ),
];

/// The largest `shortcutsrc` read (GIMP's own is about 50 KB).
const MAX_SIZE: u64 = 4 << 20;

/// The window classes: X11's `WM_CLASS` class `Gimp` and the Wayland app id `gimp` (the prgname), and the Flatpak's
/// Wayland app id.
const CLASSES: [&str; 2] = ["gimp", "org.gimp.GIMP"];

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pin {
    gimp_version: String,
    action: Vec<PinAction>,
}

#[derive(Deserialize)]
struct PinAction {
    name: String,
    category: String,
    label: Option<String>,
    keys: Vec<String>,
}

/// A GIMP window, by class or app id, any case.
pub(crate) fn is_gimp(class: &str) -> bool {
    CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class))
}

pub fn sheet() -> anyhow::Result<(Sheet, Option<Source>)> {
    let (sheet, notes, source) = read(&|k| std::env::var(k).ok(), |_| {})?;
    for note in notes {
        eprintln!("keybind-find: gimp: {note}");
    }
    Ok((sheet, Some(source)))
}

/// The sheet, the stderr notes (printable) and the defaults' `Source`. `env`: keybind-find's environment; `setup`:
/// the version probe's test seam.
fn read(env: Env, setup: impl Fn(&mut Command)) -> anyhow::Result<(Sheet, Vec<String>, Source)> {
    let mut cmd = Command::new("gimp");
    cmd.arg("--version");
    // `GNU Image Manipulation Program version 3.2.6`: the last word.
    let (_, probed) = pinned::probe("gimp", REGISTRY[0].0, cmd, setup, |t| {
        let v = t.split_whitespace().last()?;
        pinned::parse_version(v).map(|_| v.to_string())
    })?;
    let installed = probed.installed;
    let (v, text, note, source) = pinned::select("gimp", REGISTRY, installed.as_deref())
        .context("no pinned GIMP defaults")?;
    let pin: Pin = toml::from_str(text).with_context(|| format!("reading data/gimp/{v}.toml"))?;
    ensure!(
        pin.gimp_version == v,
        "data/gimp/{v}.toml names GIMP {}",
        pin.gimp_version
    );
    let mut notes: Vec<String> = note.into_iter().collect();
    let mut keys = Keys::new(defaults(&pin));
    if let Some(path) = gimp_dir(env, installed.as_deref()).map(|d| d.join("shortcutsrc")) {
        match load(&path) {
            Ok(None) => {}
            Err(why) => notes.push(format!(
                "{}: {why}; listing GIMP's defaults",
                path.display()
            )),
            Ok(Some(bytes)) => {
                let (forms, parse_notes) = parse(&bytes);
                for (name, accels) in forms {
                    keys.apply(&name, &accels);
                }
                notes.extend(
                    parse_notes
                        .iter()
                        .map(|n| format!("{}: {n}", path.display())),
                );
            }
        }
    }
    let (sheet, left_out) = to_sheet(&keys.acts);
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) keybind-find can't send: a Hyper, Meta, Mod2, Mod3, Mod5 or Release \
             modifier, a keycode, or a key name with no X keysym"
        ));
    }
    Ok((sheet, notes.iter().map(|n| printable(n)).collect(), source))
}

/// `gimp_directory` (`libgimpbase/gimpenv.c:270-410`): `$GIMP3_DIRECTORY` (relative: under `$HOME`), else
/// `<config home>/GIMP/3.<minor>`; with no GIMP 3 version, the highest `GIMP/3.N` holding a `shortcutsrc`.
/// ponytail: Flatpak's `HOST_XDG_CONFIG_HOME` and Snap's `SNAP_REAL_HOME` aren't followed (keybind-find's own env).
fn gimp_dir(env: Env, version: Option<&str>) -> Option<PathBuf> {
    let home = env("HOME").map(PathBuf::from);
    if let Some(d) = env("GIMP3_DIRECTORY") {
        let d = PathBuf::from(d);
        return if d.is_absolute() {
            Some(d)
        } else {
            Some(home?.join(d))
        };
    }
    let base = env("XDG_CONFIG_HOME")
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")))?
        .join("GIMP");
    // A version of another major (not GIMP 3's directory scheme) is ignored: the scan below.
    if let Some([3, minor, ..]) = version.and_then(pinned::parse_version).as_deref() {
        return Some(base.join(format!("3.{minor}")));
    }
    std::fs::read_dir(&base)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n: u64 = e.file_name().to_str()?.strip_prefix("3.")?.parse().ok()?;
            e.path()
                .join("shortcutsrc")
                .exists()
                .then_some((n, e.path()))
        })
        .max()
        .map(|(_, p)| p)
}

/// The file's bytes; `None` when it doesn't exist; else why it can't be read.
fn load(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    // Opened non-blocking and checked on the handle, so a FIFO put there can't hang us (as `openbox::load`).
    const O_NONBLOCK: i32 = 0o4000;
    let f = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(path)
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(f) => f,
    };
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    f.take(MAX_SIZE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_SIZE {
        return Err(format!("larger than {} MiB", MAX_SIZE >> 20));
    }
    Ok(Some(bytes))
}

// --- reading shortcutsrc -------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Tok {
    Open,
    Close,
    Ident(String),
    Int(String),
    Str(String),
    /// Any other character (GScanner's `G_TOKEN_CHAR`), or a byte that isn't ASCII.
    Other(u8),
}

/// The GScanner subset `gimp_scanner_new` sets up (`libgimpconfig/gimpscanner.c:283-288`, GLib's defaults otherwise):
/// space, tab, `\r` and newline skipped (GLib's `cset_skip_characters`, `gscanner.c:296`), `#` to the end of the line
/// and `/* … */` comments, symbols matched ignoring case (`case_sensitive` off).
struct Lexer<'a> {
    b: &'a [u8],
    i: usize,
    line: usize,
}

impl Lexer<'_> {
    fn next(&mut self) -> Result<Option<Tok>, String> {
        loop {
            match self.b.get(self.i) {
                Some(b'\n') => self.line += 1,
                Some(b' ' | b'\t' | b'\r') => {}
                Some(b'/') if self.b.get(self.i + 1) == Some(&b'*') => {
                    self.i += 2;
                    // Unclosed: a comment to the end, as GScanner reads it.
                    while self.i < self.b.len() && !self.b[self.i..].starts_with(b"*/") {
                        self.line += usize::from(self.b[self.i] == b'\n');
                        self.i += 1;
                    }
                    self.i += 2;
                    continue;
                }
                Some(b'#') => {
                    while self.b.get(self.i).is_some_and(|&c| c != b'\n') {
                        self.i += 1;
                    }
                    continue;
                }
                _ => break,
            }
            self.i += 1;
        }
        let Some(&c) = self.b.get(self.i) else {
            return Ok(None);
        };
        self.i += 1;
        let run = |s: &mut Self, ok: fn(u8) -> bool| {
            let start = s.i - 1;
            while s.b.get(s.i).copied().is_some_and(ok) {
                s.i += 1;
            }
            String::from_utf8_lossy(&s.b[start..s.i]).into_owned()
        };
        Ok(Some(match c {
            b'(' => Tok::Open,
            b')' => Tok::Close,
            b'"' | b'\'' => Tok::Str(self.string(c)?),
            c if c.is_ascii_alphabetic() => Tok::Ident(run(self, |c| {
                c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
            })),
            c if c.is_ascii_digit() => Tok::Int(run(self, |c| c.is_ascii_digit())),
            c => Tok::Other(c),
        }))
    }

    /// The rest of a string opened by `quote`: `"` with GScanner's escapes (`\` before any other character kept),
    /// `'` with none.
    fn string(&mut self, quote: u8) -> Result<String, String> {
        let mut out = Vec::new();
        let opened = self.line;
        loop {
            let Some(&c) = self.b.get(self.i) else {
                // Named by the line it opened on.
                self.line = opened;
                return Err("a string isn't closed".into());
            };
            self.i += 1;
            match c {
                b'\n' => {
                    self.line += 1;
                    out.push(c);
                }
                c if c == quote => break,
                b'\\' if quote == b'"' => {
                    let Some(&e) = self.b.get(self.i) else {
                        self.line = opened;
                        return Err("a string isn't closed".into());
                    };
                    self.i += 1;
                    match e {
                        b'\\' | b'"' => out.push(e),
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        _ => {
                            // Read again, so a newline is counted.
                            self.i -= 1;
                            out.push(b'\\');
                        }
                    }
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "a string isn't UTF-8".into())
    }
}

/// `shortcuts_rc_parse` (`shortcuts-rc.c:59-157`): the `(action "NAME" "ACCEL"…)` forms in file order, and the notes:
/// a `file-version` other than 1, or where reading stopped (the forms before it still apply, as in GIMP).
fn parse(bytes: &[u8]) -> (Vec<(String, Vec<String>)>, Vec<String>) {
    // NUL ends the input, as GScanner's `g_scanner_get_char` reads it, in a string or comment too.
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    let mut lx = Lexer {
        b: &bytes[..end],
        i: 0,
        line: 1,
    };
    let mut forms = Vec::new();
    let mut notes = Vec::new();
    if let Err(why) = forms_of(&mut lx, &mut forms, &mut notes) {
        notes.push(format!("line {}: {why}; reading stopped there", lx.line));
    }
    (forms, notes)
}

fn forms_of(
    lx: &mut Lexer,
    forms: &mut Vec<(String, Vec<String>)>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let unexpected = |t: Option<Tok>| match t {
        None => "the file ends inside a form".to_string(),
        Some(t) => format!("unexpected {}", describe(&t)),
    };
    while let Some(t) = lx.next()? {
        if t != Tok::Open {
            return Err(unexpected(Some(t)));
        }
        match lx.next()? {
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("file-version") => {
                // `gimp_scanner_parse_int`: an optional `-`, then a number.
                let (sign, t) = match lx.next()? {
                    Some(Tok::Other(b'-')) => ("-", lx.next()?),
                    t => ("", t),
                };
                let Some(Tok::Int(n)) = t else {
                    return Err("file-version isn't followed by a number".into());
                };
                let n = format!("{sign}{n}");
                if n.parse::<i64>() != Ok(1) {
                    notes.push(format!("file-version {n}, not 1; reading it as version 1"));
                }
                match lx.next()? {
                    Some(Tok::Close) => {}
                    t => return Err(unexpected(t)),
                }
            }
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("action") => {
                let Some(Tok::Str(name)) = lx.next()? else {
                    return Err("an action's name isn't a string".into());
                };
                let mut accels = Vec::new();
                let end = loop {
                    match lx.next()? {
                        Some(Tok::Str(a)) => accels.push(a),
                        t => break t,
                    }
                };
                // Set before its `)` is checked, as `shortcuts_action_deserialize` does.
                forms.push((name, accels));
                if end != Some(Tok::Close) {
                    return Err(unexpected(end));
                }
            }
            // "ignoring unknown symbol": skipped to the next `)` token, not by depth (`shortcuts-rc.c:116-124`).
            Some(Tok::Ident(_)) => loop {
                match lx.next()? {
                    Some(Tok::Close) => break,
                    Some(_) => {}
                    None => return Err(unexpected(None)),
                }
            },
            t => return Err(unexpected(t)),
        }
    }
    Ok(())
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Open => "`(`".into(),
        Tok::Close => "`)`".into(),
        Tok::Ident(s) | Tok::Int(s) => format!("`{s}`"),
        Tok::Str(s) => format!("string {s:?}"),
        Tok::Other(c) if c.is_ascii() => format!("{:?}", *c as char),
        Tok::Other(_) => "a non-ASCII byte".into(),
    }
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

/// An accelerator as `gtk_accelerator_parse` leaves it: GTK's modifier bits and the key's name (`gdk_keyval_to_lower`
/// of a letter, `XF86` put back on GDK's names for XF86 keys). Two accelerators are the same chord when equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Accel {
    mods: u16,
    key: String,
}

/// `gtk_accelerator_parse` (GTK 3.24.52 `gtk/gtkaccelgroup.c:1198-1432`): `<…>` modifiers ignoring case (an unknown
/// one skipped), then the key name. `None`: GTK's invalid accelerator (no key, an unclosed `<`, a name no keysym can
/// have), which GIMP drops.
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
            _ => 0,
        };
        rest = after;
    }
    if rest.is_empty() || !rest.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
        return None;
    }
    let key = if rest.len() == 1 {
        rest.to_ascii_lowercase()
    } else if !is_keysym(rest) && is_keysym(&format!("XF86{rest}")) {
        format!("XF86{rest}")
    } else {
        rest.to_string()
    };
    Some(Accel { mods, key })
}

/// The accelerator in keybind-find's notation; `None`: left out (an unsendable modifier, a keycode, a key name with
/// no X keysym).
fn notation(a: &Accel) -> Option<String> {
    if a.mods & UNSENDABLE != 0 || !is_keysym(&a.key) {
        return None;
    }
    let mut mods = Vec::new();
    for (bits, name) in [
        (CTRL, "ctrl"),
        (SHIFT, "shift"),
        (MOD1, "alt"),
        (SUPER | MOD4, "super"),
    ] {
        if a.mods & bits != 0 {
            mods.push(name);
        }
    }
    Some(chord(&mods, &a.key))
}

// --- the overlay -----------------------------------------------------------------------------------------------------

struct Act {
    /// One of the pin's, not a name only the user's file has.
    known: bool,
    category: String,
    label: Option<String>,
    defaults: Vec<Accel>,
    current: Vec<Accel>,
}

/// The pin's actions, each on its defaults.
fn defaults(pin: &Pin) -> BTreeMap<String, Act> {
    pin.action
        .iter()
        .map(|a| {
            let keys: Vec<Accel> = a.keys.iter().filter_map(|k| parse_accel(k)).collect();
            let act = Act {
                known: true,
                category: a.category.clone(),
                label: a.label.clone(),
                defaults: keys.clone(),
                current: keys,
            };
            (a.name.clone(), act)
        })
        .collect()
}

/// The actions' keys, with an index of which pin actions hold each chord.
struct Keys {
    acts: BTreeMap<String, Act>,
    /// The pin's actions only: a name GIMP doesn't have never gets keys there ("not existing action … ignored"), so
    /// it never holds a chord.
    holders: HashMap<Accel, BTreeSet<String>>,
}

impl Keys {
    fn new(acts: BTreeMap<String, Act>) -> Keys {
        let mut holders: HashMap<Accel, BTreeSet<String>> = HashMap::new();
        for (name, act) in &acts {
            for a in &act.current {
                holders.entry(a.clone()).or_default().insert(name.clone());
            }
        }
        Keys { acts, holders }
    }

    /// A pin action's keys replaced, the index kept up to date.
    fn set(&mut self, name: &str, keys: Vec<Accel>) {
        let Some(act) = self.acts.get_mut(name) else {
            return;
        };
        for a in &act.current {
            if let Some(h) = self.holders.get_mut(a) {
                h.remove(name);
            }
        }
        for a in &keys {
            self.holders
                .entry(a.clone())
                .or_default()
                .insert(name.to_string());
        }
        act.current = keys;
    }

    /// `shortcuts_action_deserialize` (`shortcuts-rc.c:242-342`): each accelerator that parses is kept unless another
    /// action holding it is off its defaults (then it is dropped); one on its defaults loses all its keys
    /// (`gimp_action_use_default_accels`, `app/widgets/gimpaction.c:675-712`). The kept ones replace the action's. A
    /// name the pin lacks (a plug-in's) is listed under `Other` but holds no chord; an empty name (NULL in GIMP) only
    /// runs the conflict pass.
    fn apply(&mut self, name: &str, accels: &[String]) {
        let mut kept = Vec::new();
        for a in accels.iter().filter_map(|s| parse_accel(s)) {
            let others: Vec<String> = self
                .holders
                .get(&a)
                .map(|h| h.iter().filter(|o| *o != name).cloned().collect())
                .unwrap_or_default();
            let mut add = true;
            for other in others {
                let Some(act) = self.acts.get(&other) else {
                    continue;
                };
                if act.current == act.defaults {
                    self.set(&other, Vec::new());
                } else {
                    add = false;
                    break;
                }
            }
            // A key given twice in one line is kept once.
            if add && !kept.contains(&a) {
                kept.push(a);
            }
        }
        match self.acts.get(name) {
            _ if name.is_empty() => {}
            Some(act) if act.known => self.set(name, kept),
            _ => {
                let act = Act {
                    known: false,
                    category: "Other".into(),
                    label: None,
                    defaults: Vec::new(),
                    current: kept,
                };
                self.acts.insert(name.to_string(), act);
            }
        }
    }
}

/// One bind per chord of each action, by category then action name; and how many chords were left out.
fn to_sheet(acts: &BTreeMap<String, Act>) -> (Sheet, usize) {
    let mut sheet = Sheet {
        title: "GIMP".into(),
        provider: "gimp".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for (name, act) in acts {
        for a in &act.current {
            let Some(key) = notation(a) else {
                left_out += 1;
                continue;
            };
            // A name can come from the user's file: shown, so escaped.
            let name = printable(name);
            sheet
                .binds
                .entry(act.category.clone())
                .or_default()
                .push(Bind {
                    key,
                    desc: act.label.clone().unwrap_or_else(|| name.clone()),
                    action: name,
                    when: Default::default(),
                    run: Vec::new(),
                });
        }
    }
    (sheet, left_out)
}

/// `s` with control and bidirectional-format characters escaped: a note or a name can quote the user's file.
/// ponytail: a copy of `openbox::printable` (as qutebrowser's); share it when a story owns them.
fn printable(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => c.escape_unicode().to_string(),
            c if c.is_control() => c.escape_debug().to_string(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::pinned::{assert_data, fake_bin, path_of};

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/gimp/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn pin() -> Pin {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-gimp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The pin with `text` laid over it.
    fn overlay(text: &[u8]) -> BTreeMap<String, Act> {
        let mut keys = Keys::new(defaults(&pin()));
        for (name, accels) in parse(text).0 {
            keys.apply(&name, &accels);
        }
        keys.acts
    }

    /// Each action's current chords in keybind-find's notation (`!` for a left-out one).
    fn keys(acts: &BTreeMap<String, Act>, name: &str) -> Vec<String> {
        acts[name]
            .current
            .iter()
            .map(|a| notation(a).unwrap_or_else(|| "!".into()))
            .collect()
    }

    fn binds(s: &Sheet) -> Vec<(String, String, String, String)> {
        s.binds
            .iter()
            .flat_map(|(c, bs)| {
                bs.iter()
                    .map(move |b| (c.clone(), b.action.clone(), b.key.clone(), b.desc.clone()))
            })
            .collect()
    }

    #[test]
    fn tokenizer_and_parse() {
        let (forms, notes) = parse(
            b"# a comment (action \"x\" \"a\")\n(action \"a\\\"b\" 'x' \"\\q\") # to the end\n(action 'n')",
        );
        assert_eq!(
            forms,
            [
                ("a\"b".to_string(), vec!["x".to_string(), "\\q".to_string()]),
                ("n".to_string(), vec![]),
            ]
        );
        assert!(notes.is_empty(), "{notes:?}");
        // An unclosed string: a note with its line, the earlier actions kept.
        let (forms, notes) = parse(b"(action \"a\" \"b\")\n\n(action \"c\" \"d)\n");
        assert_eq!(forms, [("a".to_string(), vec!["b".to_string()])]);
        assert_eq!(
            notes,
            ["line 3: a string isn't closed; reading stopped there"]
        );
        // Another file-version: a note, and reading goes on.
        let (forms, notes) = parse(b"(file-version 2)\n(action \"a\")");
        assert_eq!((forms.len(), notes.len()), (1, 1));
        assert_eq!(notes[0], "file-version 2, not 1; reading it as version 1");
        // An unknown form is skipped to its `)`.
        let (forms, notes) = parse(b"(frobnicate 1 \"x\" y)\n(action \"a\")");
        assert_eq!((forms.len(), notes.len()), (1, 0));
        // A non-string in an action: what came before it applies (as GIMP sets it before its `)`).
        let (forms, notes) = parse(b"(action \"a\" \"b\" 5)\n(action \"c\")");
        assert_eq!(forms, [("a".to_string(), vec!["b".to_string()])]);
        assert_eq!(notes, ["line 1: unexpected `5`; reading stopped there"]);
        // Invalid UTF-8 in a string; an unclosed form.
        let (forms, notes) = parse(b"(action \"a\")\n(action \"\xff\")");
        assert_eq!(
            (forms.len(), notes[0].as_str()),
            (1, "line 2: a string isn't UTF-8; reading stopped there")
        );
        assert_eq!(
            parse(b"(action \"a\"").1,
            ["line 1: the file ends inside a form; reading stopped there"]
        );
        // file-version as a number; NUL ends the input, in a string or a comment too.
        for (v, noted) in [
            ("01", false),
            ("-1", true),
            ("2", true),
            ("99999999999999999999", true),
        ] {
            let (forms, notes) = parse(format!("(file-version {v})\n(action \"a\")").as_bytes());
            assert_eq!((forms.len(), notes.len()), (1, usize::from(noted)), "{v}");
        }
        assert_eq!(parse(b"(action \"a\")\0(action \"b\")").0.len(), 1);
        assert_eq!(parse(b"(action \"a\")# x\0\n(action \"b\")").0.len(), 1);
        assert_eq!(
            parse(b"(action \"a\0\")\n").1,
            ["line 1: a string isn't closed; reading stopped there"]
        );
        // CRLF: GLib skips `\r` too, so GIMP reads the whole file.
        let (forms, notes) = parse(b"# GIMP shortcutsrc\r\n(action \"a\")\r\n(action \"b\")\r\n");
        assert_eq!((forms.len(), notes.len()), (2, 0));
        // Symbols ignoring case; `/* */` comments, one over lines.
        let (forms, notes) =
            parse(b"(FILE-VERSION 1)\n/* a\n(action \"x\") */ (Action \"a\" /* k */ \"b\")\n");
        assert_eq!(forms, [("a".to_string(), vec!["b".to_string()])]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            parse(b"(action \"a\")\n\n(x\"").1,
            ["line 3: a string isn't closed; reading stopped there"]
        );
        // The file ending inside an unknown form.
        assert_eq!(
            parse(b"(action \"a\")\n(frobnicate 1 2").1,
            ["line 2: the file ends inside a form; reading stopped there"]
        );
        // The other escapes; `\` before any other character kept (AC #3).
        let (forms, _) = parse(b"(action \"\\\\ \\n\\t\\r\\b\\f\\101\")");
        assert_eq!(forms[0].0, "\\ \n\t\r\u{8}\u{c}\\101");
    }

    #[test]
    fn keys_as_gtk_parses_them() {
        let n = |s: &str| parse_accel(s).map(|a| notation(&a).unwrap_or_else(|| "!".into()));
        for (accel, want) in [
            ("<Primary>z", "ctrl+z"),
            ("<Primary>Z", "ctrl+z"),
            ("<primary><shift>V", "ctrl+shift+v"),
            ("<CTRL>k", "ctrl+k"),
            ("<Control>k", "ctrl+k"),
            ("<Ctl>k", "ctrl+k"),
            ("<Alt>Tab", "alt+Tab"),
            ("<Mod1>Tab", "alt+Tab"),
            ("<Super>r", "super+r"),
            ("<Mod4>r", "super+r"),
            ("<Shift>KP_5", "shift+KP_5"),
            ("slash", "slash"),
            ("percent", "percent"),
            ("<Primary>less", "ctrl+less"),
            ("Cut", "XF86Cut"),
            ("XF86ZoomIn", "XF86ZoomIn"),
            ("<Foo>x", "x"),
            // Left out: unsendable modifiers, a keycode, a name with no keysym (case-sensitive).
            ("<Hyper>r", "!"),
            ("<Meta>r", "!"),
            ("<Mod3>r", "!"),
            ("<Release>q", "!"),
            ("0x26", "!"),
            ("NoSuchKey", "!"),
            ("cut", "!"),
        ] {
            assert_eq!(n(accel).as_deref(), Some(want), "{accel}");
        }
        // Invalid: dropped, not counted.
        for accel in ["bogus+", "", "<Primary>", "<Primary"] {
            assert_eq!(parse_accel(accel), None, "{accel}");
        }
        // Compared after parsing: `Cut` is `XF86Cut`, `<Ctrl>` is `<Primary>`; `<Mod4>` isn't `<Super>` in GTK.
        assert_eq!(parse_accel("Cut"), parse_accel("XF86Cut"));
        assert_eq!(parse_accel("<Ctrl>KP_Add"), parse_accel("<Primary>KP_Add"));
        assert_ne!(parse_accel("<Mod4>r"), parse_accel("<Super>r"));
    }

    #[test]
    fn the_overlay_as_gimp_applies_it() {
        let acts = overlay(&fixture("shortcutsrc-user"));
        assert_eq!(keys(&acts, "edit-undo"), ["ctrl+u"]);
        // Its `<Primary>v` taken by edit-copy while on its defaults: all its keys go (`Paste` too).
        assert!(acts["edit-paste"].current.is_empty());
        assert_eq!(keys(&acts, "edit-copy"), ["ctrl+v"]);
        // `<Primary>u` is edit-undo's, which is off its defaults now: dropped.
        assert_eq!(keys(&acts, "edit-redo"), ["ctrl+y"]);
        assert_eq!(keys(&acts, "view-zoom-in"), ["ctrl+KP_Add", "XF86ZoomIn"]);
        assert_eq!(
            keys(&acts, "tools-rect-select"),
            ["super+r", "!", "super+e"]
        );
        assert!(acts["edit-cut"].current.is_empty());
        assert_eq!(keys(&acts, "file-quit"), ["ctrl+q"]);
        assert_eq!(acts["plug-in-e2e"].category, "Other");
        let (sheet, left_out) = to_sheet(&acts);
        assert_eq!(left_out, 1);
        let b = binds(&sheet);
        let of = |name: &str| -> Vec<_> { b.iter().filter(|r| r.1 == name).cloned().collect() };
        assert!(of("edit-paste").is_empty() && of("edit-cut").is_empty());
        let row = |c: &str, a: &str, k: &str, d: &str| (c.into(), a.into(), k.into(), d.into());
        assert_eq!(
            of("plug-in-e2e"),
            [row("Other", "plug-in-e2e", "ctrl+alt+F12", "plug-in-e2e")]
        );
        assert_eq!(
            of("tools-rect-select"),
            [
                row("Tools", "tools-rect-select", "super+r", "Rectangle Select"),
                row("Tools", "tools-rect-select", "super+e", "Rectangle Select"),
            ]
        );
        // Categories in order, entries by action name.
        let order: Vec<_> = b.iter().map(|r| (r.0.clone(), r.1.clone())).collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted);
        // Order matters: the same two lines the other way round give GIMP's other result.
        let acts = overlay(b"(action \"edit-redo\" \"<Primary>u\" \"<Primary>y\")\n(action \"edit-undo\" \"<Primary>u\")");
        assert_eq!(keys(&acts, "edit-redo"), ["ctrl+u", "ctrl+y"]);
        // edit-redo, off its defaults, holds `<Primary>u`: edit-undo loses it and is left with none.
        assert!(acts["edit-undo"].current.is_empty());
        // A line equal to its defaults is still "on defaults" for a later conflict.
        let acts = overlay(
            b"(action \"edit-undo\" \"<Primary>z\")\n(action \"edit-copy\" \"<Primary>z\")",
        );
        assert!(acts["edit-undo"].current.is_empty());
        assert_eq!(keys(&acts, "edit-copy"), ["ctrl+z"]);
    }

    #[test]
    fn the_pinned_defaults() {
        for (v, _) in REGISTRY {
            assert_data("gimp", &format!("{v}.toml"));
        }
        // 3.2.8 changed no default: its pin is 3.2.6's but for the version.
        let p328: Pin = toml::from_str(REGISTRY[1].1).unwrap();
        assert_eq!(p328.gimp_version, "3.2.8");
        let pin = pin();
        assert_eq!(pin.gimp_version, "3.2.6");
        let names = |p: &Pin| -> Vec<_> {
            p.action
                .iter()
                .map(|a| {
                    (
                        a.name.clone(),
                        a.category.clone(),
                        a.label.clone(),
                        a.keys.clone(),
                    )
                })
                .collect()
        };
        assert_eq!(names(&p328), names(&pin));
        assert_eq!(pin.action.len(), 1132);
        let keyed: Vec<_> = pin.action.iter().filter(|a| !a.keys.is_empty()).collect();
        assert_eq!(keyed.len(), 136);
        for a in &keyed {
            assert!(a.label.is_some(), "{} has no label", a.name);
            for k in &a.keys {
                let parsed = parse_accel(k);
                assert!(
                    parsed.as_ref().and_then(notation).is_some(),
                    "{}: {k}",
                    a.name
                );
            }
        }
        // With no shortcutsrc: the pin's keyed actions, nothing left out.
        let (sheet, left_out) = to_sheet(&defaults(&pin));
        assert_eq!(left_out, 0);
        let b = binds(&sheet);
        assert_eq!(
            b.iter()
                .map(|r| r.1.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            136
        );
        let of = |name: &str| -> Vec<_> {
            b.iter()
                .filter(|r| r.1 == name)
                .map(|r| (r.0.as_str(), r.2.as_str(), r.3.as_str()))
                .collect()
        };
        assert_eq!(of("edit-undo"), [("Edit", "ctrl+z", "Undo")]);
        assert_eq!(
            of("tools-rect-select"),
            [("Tools", "r", "Rectangle Select")]
        );
        assert_eq!(
            of("dialogs-action-search")
                .iter()
                .map(|r| r.1)
                .collect::<Vec<_>>(),
            ["slash", "KP_Divide"]
        );
        assert_eq!(
            of("edit-cut").iter().map(|r| r.1).collect::<Vec<_>>(),
            ["ctrl+x", "XF86Cut"]
        );
        // GIMP's own file with no user changes is all comments: nothing changes.
        let (over, _) = to_sheet(&overlay(&fixture("shortcutsrc-default")));
        assert_eq!(over, sheet);
    }

    #[test]
    fn conflicts_in_detail() {
        // A left-out key (hyper) still holds its chord against a later action.
        let acts =
            overlay(b"(action \"edit-undo\" \"<Hyper>r\")\n(action \"edit-copy\" \"<Hyper>r\")");
        assert_eq!(keys(&acts, "edit-undo"), ["!"]);
        assert!(acts["edit-copy"].current.is_empty());
        // An action's own keys don't conflict with it; a key given twice in a line is kept once.
        // (edit-undo is off its defaults, holding `<Primary>u`, when its second line is read.)
        let acts = overlay(
            b"(action \"edit-undo\" \"<Primary>u\")\n(action \"edit-undo\" \"<Primary>u\" \"<Primary>y\" \"<Ctrl>u\")",
        );
        assert_eq!(keys(&acts, "edit-undo"), ["ctrl+u", "ctrl+y"]);
        // Defaults compared after parsing: `<Ctrl>Z` is edit-undo's `<Primary>z`, so it is still on its defaults.
        let acts =
            overlay(b"(action \"edit-undo\" \"<Ctrl>Z\")\n(action \"edit-copy\" \"<Primary>z\")");
        assert!(acts["edit-undo"].current.is_empty());
        let acts = overlay(b"(action \"edit-copy\" \"<Ctrl>z\")");
        assert!(acts["edit-undo"].current.is_empty());
        assert_eq!(keys(&acts, "edit-copy"), ["ctrl+z"]);
        // A name from the file is shown escaped.
        let acts = overlay("(action \"x\u{202e}y\\n\" \"<Primary>F12\")".as_bytes());
        let b = binds(&to_sheet(&acts).0);
        let row = b.iter().find(|r| r.2 == "ctrl+F12").unwrap();
        assert_eq!(
            (row.1.as_str(), row.3.as_str()),
            ("x\\u{202e}y\\n", "x\\u{202e}y\\n")
        );
    }

    #[test]
    fn names_gimp_lacks() {
        // A name the pin lacks never holds a key against a real action, but still takes one from an action on its
        // defaults; it is listed under Other.
        let acts =
            overlay(b"(action \"old-name\" \"<Primary>k\")\n(action \"edit-undo\" \"<Primary>k\")");
        assert_eq!(keys(&acts, "edit-undo"), ["ctrl+k"]);
        assert_eq!(keys(&acts, "old-name"), ["ctrl+k"]);
        assert_eq!(acts["old-name"].category, "Other");
        let acts = overlay(b"(action \"old-name\" \"<Primary>z\")");
        assert!(acts["edit-undo"].current.is_empty());
        // An empty name runs the conflict pass only.
        let acts = overlay(b"(action \"\" \"<Primary>z\")");
        assert!(acts["edit-undo"].current.is_empty() && !acts.contains_key(""));
        // Many forms: a lookup each, not a pass over every action.
        let many: String = (0..20_000)
            .map(|i| format!("(action \"plug-in-{i}\" \"<Primary>F{}\")\n", i % 12 + 1))
            .collect();
        let t = std::time::Instant::now();
        assert_eq!(overlay(many.as_bytes()).len(), 1132 + 20_000);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn the_pin_matches_gimps_own_file() {
        // GIMP 3.2.6's file with no user changes: its commented lines are its defaults, the pin's keys.
        let text = String::from_utf8(fixture("shortcutsrc-default")).unwrap();
        let uncommented: String = text
            .lines()
            .filter_map(|l| l.strip_prefix("# (action "))
            .map(|l| format!("(action {l}\n"))
            .collect();
        let (forms, notes) = parse(uncommented.as_bytes());
        assert!(notes.is_empty(), "{notes:?}");
        let pin = pin();
        assert_eq!(forms.len(), pin.action.len());
        for (name, accels) in forms {
            let a = pin.action.iter().find(|a| a.name == name).unwrap();
            assert_eq!(accels, a.keys, "{name}");
        }
        // Each category is its action group's label (`app/actions/actions.c` action_groups[], the longest
        // `<group>-` prefix), else Plug-ins.
        const GROUPS: &[(&str, &str)] = &[
            ("brush-editor", "Brush Editor"),
            ("brushes", "Brushes"),
            ("buffers", "Buffers"),
            ("channels", "Channels"),
            ("colormap", "Colormap"),
            ("context", "Context"),
            ("cursor-info", "Pointer Information"),
            ("dashboard", "Dashboard"),
            ("debug", "Debug"),
            ("dialogs", "Dialogs"),
            ("dock", "Dock"),
            ("dockable", "Dockable"),
            ("documents", "Document History"),
            ("drawable", "Drawable"),
            ("dynamics", "Paint Dynamics"),
            ("dynamics-editor", "Paint Dynamics Editor"),
            ("edit", "Edit"),
            ("error-console", "Error Console"),
            ("file", "File"),
            ("filters", "Filters"),
            ("fonts", "Fonts"),
            ("gradient-editor", "Gradient Editor"),
            ("gradients", "Gradients"),
            ("tool-presets", "Tool Presets"),
            ("tool-preset-editor", "Tool Preset Editor"),
            ("help", "Help"),
            ("image", "Image"),
            ("images", "Images"),
            ("layers", "Layers"),
            ("mypaint-brushes", "MyPaint Brushes"),
            ("palette-editor", "Palette Editor"),
            ("palettes", "Palettes"),
            ("patterns", "Patterns"),
            ("plug-in", "Plug-ins"),
            ("quick-mask", "Quick Mask"),
            ("sample-points", "Sample Points"),
            ("select", "Select"),
            ("templates", "Templates"),
            ("text-tool", "Text Tool"),
            ("text-editor", "Text Editor"),
            ("tool-options", "Tool Options"),
            ("tool-path", "Tool Path"),
            ("tools", "Tools"),
            ("paths", "Paths"),
            ("view", "View"),
            ("windows", "Windows"),
        ];
        for a in &pin.action {
            let group = GROUPS
                .iter()
                .filter(|(g, _)| a.name.starts_with(&format!("{g}-")))
                .max_by_key(|(g, _)| g.len());
            assert_eq!(a.category, group.map_or("Plug-ins", |g| g.1), "{}", a.name);
        }
        // The data file: no mnemonic left, sorted by category then name.
        assert!(pin
            .action
            .iter()
            .all(|a| !a.label.as_deref().unwrap_or("").contains('_')));
        let order: Vec<_> = pin.action.iter().map(|a| (&a.category, &a.name)).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]));
    }

    /// `read` under `vars`, with `gimp` on `PATH` printing `version` (`None`: no gimp).
    fn run(vars: &[(&str, &Path)], version: Option<&str>) -> (Sheet, Vec<String>, Source) {
        let bin = match version {
            Some(v) => fake_bin("gimp", &format!("echo '{v}'")),
            None => fake_bin("not-gimp", "exit 1"),
        };
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.display().to_string()))
            .collect();
        let env = move |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        read(&env, path_of(&bin)).unwrap()
    }

    fn put(dir: &Path, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("shortcutsrc"), text).unwrap();
    }

    const V326: &str = "GNU Image Manipulation Program version 3.2.6";
    const UNDO_U: &str = "(action \"edit-undo\" \"<Primary>u\")\n";

    fn undo(s: &Sheet) -> String {
        binds(s)
            .into_iter()
            .find(|r| r.1 == "edit-undo")
            .map(|r| r.2)
            .unwrap_or_default()
    }

    #[test]
    fn where_the_file_is() {
        let d = tmp("lookup");
        let home = d.join("home");
        put(&home.join(".config/GIMP/3.2"), UNDO_U);
        // The version's directory, under ~/.config.
        let (s, notes, source) = run(&[("HOME", &home)], Some(V326));
        assert_eq!((undo(&s).as_str(), notes.len()), ("ctrl+u", 0));
        assert_eq!(
            source,
            Source {
                defaults: "3.2.6".into(),
                installed: Some("3.2.6".into()),
                checked: true,
                mismatch: false,
            }
        );
        // 3.2.8: its own pin, no note.
        let (s, notes, source) = run(
            &[("HOME", &home)],
            Some("GNU Image Manipulation Program version 3.2.8"),
        );
        assert_eq!((undo(&s).as_str(), notes.len()), ("ctrl+u", 0));
        assert_eq!(
            (source.defaults.as_str(), source.mismatch),
            ("3.2.8", false)
        );
        // XDG_CONFIG_HOME set: there; empty: ~/.config.
        let xdg = d.join("xdg");
        put(
            &xdg.join("GIMP/3.2"),
            "(action \"edit-undo\" \"<Primary>w\")\n",
        );
        assert_eq!(
            undo(&run(&[("HOME", &home), ("XDG_CONFIG_HOME", &xdg)], Some(V326)).0),
            "ctrl+w"
        );
        assert_eq!(
            undo(
                &run(
                    &[("HOME", &home), ("XDG_CONFIG_HOME", Path::new(""))],
                    Some(V326)
                )
                .0
            ),
            "ctrl+u"
        );
        // GIMP3_DIRECTORY: absolute as is, relative under $HOME.
        put(&d.join("abs"), "(action \"edit-undo\" \"<Primary>a\")\n");
        put(&home.join("rel"), "(action \"edit-undo\" \"<Primary>r\")\n");
        assert_eq!(
            undo(
                &run(
                    &[("HOME", &home), ("GIMP3_DIRECTORY", &d.join("abs"))],
                    Some(V326)
                )
                .0
            ),
            "ctrl+a"
        );
        assert_eq!(
            undo(
                &run(
                    &[("HOME", &home), ("GIMP3_DIRECTORY", Path::new("rel"))],
                    Some(V326)
                )
                .0
            ),
            "ctrl+r"
        );
        // Another version: its own directory, the nearest pin and its note.
        put(
            &home.join(".config/GIMP/3.4"),
            "(action \"edit-undo\" \"<Primary>4\")\n",
        );
        let (s, notes, source) = run(
            &[("HOME", &home)],
            Some("GNU Image Manipulation Program version 3.4.0"),
        );
        assert_eq!(undo(&s), "ctrl+4");
        assert_eq!(
            notes,
            ["gimp 3.4.0: using the defaults of 3.2.8; add data/gimp/3.4.0.toml"]
        );
        assert_eq!(
            (source.installed.as_deref(), source.mismatch),
            (Some("3.4.0"), true)
        );
        // No gimp: the highest GIMP/3.N holding a shortcutsrc (3.4 has one: remove it), and the unknown-version note.
        std::fs::remove_file(home.join(".config/GIMP/3.4/shortcutsrc")).unwrap();
        put(
            &home.join(".config/GIMP/3.0"),
            "(action \"edit-undo\" \"<Primary>0\")\n",
        );
        let (s, notes, source) = run(&[("HOME", &home)], None);
        assert_eq!(undo(&s), "ctrl+u");
        assert_eq!(
            notes,
            ["gimp's version is unknown: using the defaults of 3.2.8"]
        );
        assert_eq!((source.installed, source.mismatch), (None, true));
        // A gimp printing something else: no version.
        let (_, _, source) = run(&[("HOME", &home)], Some("GIMP, the next one"));
        assert_eq!(source.installed, None);
        // Missing: the defaults, no note.
        let (s, notes, _) = run(&[("HOME", &d.join("nobody"))], Some(V326));
        assert_eq!((undo(&s).as_str(), notes.len()), ("ctrl+z", 0));
        // Both kinds of note through `read`: the parse note under the file's path, then the left-out count.
        let notes_dir = d.join("notes");
        put(
            &notes_dir,
            "(action \"edit-undo\" \"<Hyper>u\")\n(action \"x\" 5)\n",
        );
        let (_, notes, _) = run(&[("GIMP3_DIRECTORY", &notes_dir)], Some(V326));
        assert_eq!(
            notes,
            [
                format!("{}/shortcutsrc: line 2: unexpected `5`; reading stopped there", notes_dir.display()),
                "leaving out 1 key(s) keybind-find can't send: a Hyper, Meta, Mod2, Mod3, Mod5 or Release modifier, \
                 a keycode, or a key name with no X keysym"
                    .to_string(),
            ]
        );
        // A version of another major: its directory isn't GIMP 3's, the scan is used. 3.10 is above 3.9.
        let two = run(
            &[("HOME", &home)],
            Some("GNU Image Manipulation Program version 2.10.38"),
        );
        assert_eq!(undo(&two.0), "ctrl+u");
        put(
            &home.join(".config/GIMP/3.9"),
            "(action \"edit-undo\" \"<Primary>9\")\n",
        );
        put(
            &home.join(".config/GIMP/3.10"),
            "(action \"edit-undo\" \"<Primary>t\")\n",
        );
        assert_eq!(undo(&run(&[("HOME", &home)], None).0), "ctrl+t");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_that_cant_be_read() {
        let d = tmp("unreadable");
        // A FIFO: never opened for a blocking read.
        let fifo = d.join("fifo");
        std::fs::create_dir_all(&fifo).unwrap();
        let ok = Command::new("mkfifo")
            .arg(fifo.join("shortcutsrc"))
            .status()
            .unwrap();
        assert!(ok.success());
        let (s, notes, _) = run(&[("GIMP3_DIRECTORY", &fifo)], Some(V326));
        assert_eq!(undo(&s), "ctrl+z");
        assert_eq!(
            notes,
            [format!(
                "{}/shortcutsrc: not a regular file; listing GIMP's defaults",
                fifo.display()
            )]
        );
        // Over 4 MiB.
        let big = d.join("big");
        put(&big, &format!("{UNDO_U}{}", " ".repeat(4 << 20)));
        let (s, notes, _) = run(&[("GIMP3_DIRECTORY", &big)], Some(V326));
        assert_eq!(undo(&s), "ctrl+z");
        assert_eq!(
            notes,
            [format!(
                "{}/shortcutsrc: larger than 4 MiB; listing GIMP's defaults",
                big.display()
            )]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
