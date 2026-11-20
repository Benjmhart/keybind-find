//! Openbox: the keybinds of the rc.xml Openbox loaded, each with its actions, chains as key sequences, grouped under
//! the comments that head them. A port of Openbox 3.6.1's config loading (`openbox/openbox.c`, `obt/paths.c`,
//! `obt/xml.c`), `parse_keyboard`/`parse_key` (`openbox/config.c`), `keyboard_bind`/`tree_find`
//! (`openbox/keyboard.c`, `openbox/keytree.c`) and `translate_key` (`openbox/translate.c`), all at tag
//! `release-3.6.1` (commit 249020d). Openbox reports nothing of its bindings, so the file is the source; nothing in it
//! is run.
//!
//! The XML is read by a small reader for the subset rc.xml needs (no XML crate): declaration, processing instructions
//! and a DOCTYPE without an internal subset are skipped; elements, quoted attributes, text, CDATA, comments, the five
//! predefined entities and character references are read; names compare by local name, as libxml2's `node->name`.

use crate::context::{run, Context};
use crate::sheet::{chord, Bind, Sheet};
use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A file bigger than this isn't read.
const MAX_SIZE: u64 = 4 << 20;

/// Elements nested deeper than this don't load: libxml2's limit without `XML_PARSE_HUGE` (`parser.c:1993` `nodePush`
/// at v2.15.1; Openbox passes `NOBLANKS | RECOVER` only, `obt/xml.c:153-156`).
const MAX_DEPTH: usize = 256;

/// Keys and actions read (each key of a chain counted, each action each time it is bound) before the rest are left
/// out: `key="a b c …"` alternatives multiply down a chain, and each one binds all its keybind's actions.
const MAX_KEYS: usize = 5000;

/// Openbox's own keys when no rc.xml has a `<keyboard>` (or none loads): `bind_default_keyboard`,
/// `openbox/config.c:988-1001` at `release-3.6.1`.
const DEFAULTS: &[(&str, &str)] = &[
    ("A-Tab", "NextWindow"),
    ("S-A-Tab", "PreviousWindow"),
    ("A-F4", "Close"),
];

const CATEGORY: &str = "openbox";

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    // No X, no xprop or no property: the XDG lookup, as Openbox does without `--config-file`.
    let prop = run("xprop", &["-root", "_OB_CONFIG_FILE"]).ok();
    let (sheet, notes) = read(prop.as_deref(), &|k| std::env::var(k).ok());
    for note in notes {
        eprintln!("keybind-find: openbox: {note}");
    }
    Ok(sheet)
}

/// `s` with control and bidirectional-format characters escaped: a note quotes a file's text and a root-window
/// property any X client can set.
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

/// The sheet from the file Openbox picks (`prop`: `xprop -root _OB_CONFIG_FILE`'s output; `env`: keybind-find's
/// environment), and the stderr notes, printable.
fn read(prop: Option<&str>, env: Env) -> (Sheet, Vec<String>) {
    let (sheet, notes) = lookup(prop, env);
    (sheet, notes.iter().map(|n| printable(n)).collect())
}

fn lookup(prop: Option<&str>, env: Env) -> (Sheet, Vec<String>) {
    let mut notes = Vec::new();
    // `openbox/openbox.c:245-256`: the `--config-file` file, or else the first `openbox/rc.xml` of the config dirs.
    let mut candidates = Vec::new();
    match prop.and_then(property_path) {
        Some(p) if p.is_absolute() => candidates.push(p),
        Some(p) => notes.push(format!(
            "_OB_CONFIG_FILE names a relative path ({}); looking in the XDG config dirs",
            p.display()
        )),
        None => {}
    }
    candidates.extend(
        config_dirs(env)
            .into_iter()
            .map(|d| d.join("openbox/rc.xml")),
    );
    for path in candidates {
        match load(&path) {
            Ok(None) => {}
            Ok(Some(root)) => {
                let (sheet, left) = keyboard(&root);
                notes.extend(left.note());
                return (sheet, notes);
            }
            Err(why) => notes.push(format!("{}: {why}; skipped", path.display())),
        }
    }
    notes.push("no rc.xml loads; listing Openbox's built-in keys".into());
    let (sheet, left) = defaults(LeftOut::default());
    notes.extend(left.note());
    (sheet, notes)
}

/// The path in `xprop -root _OB_CONFIG_FILE`'s output: `_OB_CONFIG_FILE(UTF8_STRING) = "…"`; `None` for
/// `_OB_CONFIG_FILE:  not found.` (the property is erased when no `--config-file` was given, `openbox.c:258-266`) or
/// an empty value. xprop prints it as `8u` under `LC_ALL=C.UTF-8`: non-ASCII as it is, `\`, `"`, newline and tab
/// escaped (`\n`, `\t`), other control characters as three octal digits (`\033`); undone here, byte for byte.
fn property_path(out: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let (_, value) = out.lines().next()?.split_once(" = ")?;
    let mut rest = value.strip_prefix('"')?.as_bytes();
    let mut path = Vec::new();
    loop {
        let (b, tail) = rest.split_first()?;
        rest = tail;
        match b {
            b'"' => break,
            b'\\' => {
                let (e, tail) = rest.split_first()?;
                rest = tail;
                path.push(match e {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'0'..=b'3' => match rest {
                        [x @ b'0'..=b'7', y @ b'0'..=b'7', tail @ ..] => {
                            rest = tail;
                            (e - b'0') << 6 | (x - b'0') << 3 | (y - b'0')
                        }
                        _ => *e,
                    },
                    e => *e,
                });
            }
            b => path.push(*b),
        }
    }
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(path)))
}

/// `obt_paths_new` (`obt/paths.c:145-149,164-179`): the config home (non-empty `XDG_CONFIG_HOME`, else
/// `~/.config`), prepended to `XDG_CONFIG_DIRS` (non-empty, split on `:` skipping empty entries; else `/etc/xdg`,
/// which is also Arch's `CONFIGDIR`) unless already in it; duplicates dropped (`slist_path_add`, `:68-81`).
fn config_dirs(env: Env) -> Vec<PathBuf> {
    let set = |k| env(k).filter(|v: &String| !v.is_empty());
    let home = set("XDG_CONFIG_HOME")
        .or_else(|| set("HOME").map(|h| format!("{h}/.config")))
        .map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let listed = set("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into());
    for d in listed.split(':').filter(|d| !d.is_empty()) {
        let d = PathBuf::from(d);
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    // ponytail: no HOME and no XDG_CONFIG_HOME skips the home (glib would ask passwd); read passwd if that matters.
    if let Some(h) = home.filter(|h| !dirs.contains(h)) {
        dirs.insert(0, h);
    }
    dirs
}

/// The file's root element and its `xi:include` count; `None` when it doesn't exist; why it doesn't load otherwise.
fn load(path: &Path) -> Result<Option<Element>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    // Opened non-blocking and checked on the handle, so a FIFO put there can't hang us.
    // ponytail: Linux's O_NONBLOCK on x86 and arm (libc isn't a dependency); alpha, mips, parisc and sparc differ.
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
    let text = String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
    let root = parse_xml(&text)?;
    // `obt/xml.c:160-170`: a document of another type is passed over.
    if root.name != "openbox_config" {
        return Err(format!(
            "its root element is <{}>, not <openbox_config>",
            root.name
        ));
    }
    Ok(Some(root))
}

// --- the XML subset --------------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
struct Element {
    /// The local name (after any `prefix:`).
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug, PartialEq)]
enum Node {
    Element(Element),
    Text(String),
    Comment(String),
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|c| match c {
            Node::Element(e) => Some(e),
            _ => None,
        })
    }

    /// The text when there are no element children (comments ignored); `None` otherwise.
    fn text_only(&self) -> Option<String> {
        let mut s = String::new();
        for c in &self.children {
            match c {
                Node::Element(_) => return None,
                Node::Text(t) => s.push_str(t),
                Node::Comment(_) => {}
            }
        }
        Some(s)
    }
}

fn local(qname: &str) -> &str {
    qname.rsplit(':').next().unwrap_or(qname)
}

/// The document's root element; an error with its line number when the text isn't well-formed for this subset.
fn parse_xml(text: &str) -> Result<Element, String> {
    // XML's end-of-line handling: CRLF and a lone CR read as LF (lines then count the same).
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let line = |pos: usize| text[..pos].matches('\n').count() + 1;
    let err = |pos: usize, why: String| format!("line {}: {why}", line(pos));
    // Open elements: (qualified name, element, where it opened).
    let mut stack: Vec<(&str, Element, usize)> = Vec::new();
    let mut root: Option<Element> = None;
    let mut pos = 0;
    while pos < text.len() {
        let rest = &text[pos..];
        let until = |end: &str, what: &str| {
            rest.find(end)
                .ok_or_else(|| err(pos, format!("unclosed {what}")))
        };
        if let Some(body) = rest.strip_prefix("<!--") {
            let n = body
                .find("--")
                .ok_or_else(|| err(pos, "unclosed comment".into()))?;
            // XML has no `--` in a comment, nor a comment ending `--->`.
            if !body[n..].starts_with("-->") {
                return Err(err(pos, "`--` inside a comment".into()));
            }
            if let Some((_, top, _)) = stack.last_mut() {
                top.children.push(Node::Comment(body[..n].to_string()));
            }
            pos += 4 + n + 3;
        } else if let Some(body) = rest.strip_prefix("<![CDATA[") {
            let n = body
                .find("]]>")
                .ok_or_else(|| err(pos, "unclosed CDATA section".into()))?;
            let Some((_, top, _)) = stack.last_mut() else {
                return Err(err(pos, "CDATA outside the root element".into()));
            };
            top.children.push(Node::Text(body[..n].to_string()));
            pos += 9 + n + 3;
        } else if rest.starts_with("<?") {
            pos += until("?>", "processing instruction")? + 2;
        } else if rest.starts_with("<!DOCTYPE") {
            // Its end: the first `>` outside a quoted system or public id.
            let mut quote = None;
            let mut end = None;
            for (i, c) in rest.char_indices() {
                match (quote, c) {
                    (Some(q), c) if c == q => quote = None,
                    (Some(_), _) => {}
                    (None, '"' | '\'') => quote = Some(c),
                    (None, '[') => {
                        return Err(err(pos, "a DOCTYPE with an internal subset".into()))
                    }
                    (None, '>') => {
                        end = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            pos += end.ok_or_else(|| err(pos, "unclosed DOCTYPE".into()))? + 1;
        } else if rest.starts_with("<!") {
            return Err(err(
                pos,
                "markup this reader doesn't take (`<!` other than a comment, CDATA or DOCTYPE)"
                    .into(),
            ));
        } else if let Some(body) = rest.strip_prefix("</") {
            let n = body
                .find('>')
                .ok_or_else(|| err(pos, "unclosed end tag".into()))?;
            let name = body[..n].trim_end();
            let Some((open, el, _)) = stack.pop() else {
                return Err(err(pos, format!("</{name}> with no open element")));
            };
            if name != open {
                return Err(err(pos, format!("</{name}> closes <{open}>")));
            }
            close(&mut stack, &mut root, el);
            pos += 2 + n + 1;
        } else if rest.starts_with('<') {
            let start = pos;
            let (qname, el, empty, len) = start_tag(rest).map_err(|why| err(start, why))?;
            if root.is_some() {
                return Err(err(start, format!("<{qname}> after the root element")));
            }
            // Bounded, so dropping the tree and walking chains can't overflow the stack.
            if stack.len() >= MAX_DEPTH {
                return Err(err(
                    start,
                    format!("elements nested deeper than {MAX_DEPTH}"),
                ));
            }
            if empty {
                close(&mut stack, &mut root, el);
            } else {
                stack.push((qname, el, start));
            }
            pos += len;
        } else {
            let n = rest.find('<').unwrap_or(rest.len());
            let raw = &rest[..n];
            match stack.last_mut() {
                Some((_, top, _)) => {
                    let t = unescape(raw).map_err(|(at, why)| err(pos + at, why))?;
                    top.children.push(Node::Text(t));
                }
                None if raw.trim().is_empty() => {}
                None => return Err(err(pos, "text outside the root element".into())),
            }
            pos += n;
        }
    }
    if let Some((name, _, at)) = stack.pop() {
        return Err(err(at, format!("<{name}> is never closed")));
    }
    root.ok_or_else(|| "no root element".to_string())
}

/// A closed element: its parent's child, or the root.
fn close(stack: &mut [(&str, Element, usize)], root: &mut Option<Element>, el: Element) {
    match stack.last_mut() {
        Some((_, parent, _)) => parent.children.push(Node::Element(el)),
        None => *root = Some(el),
    }
}

/// A start tag at the start of `s`: its qualified name, the element, whether it is self-closing, its length.
fn start_tag(s: &str) -> Result<(&str, Element, bool, usize), String> {
    let name_end = |s: &str| {
        s.find(|c: char| c.is_whitespace() || "/>=<\"'&".contains(c))
            .unwrap_or(s.len())
    };
    let n = name_end(&s[1..]) + 1;
    let qname = &s[1..n];
    if qname.is_empty() {
        return Err("a `<` that starts no tag".into());
    }
    let mut el = Element {
        name: local(qname).to_string(),
        ..Element::default()
    };
    let mut i = n;
    loop {
        let rest = &s[i..];
        let trimmed = rest.trim_start();
        i += rest.len() - trimmed.len();
        if trimmed.starts_with("/>") {
            return Ok((qname, el, true, i + 2));
        }
        if trimmed.starts_with('>') {
            return Ok((qname, el, false, i + 1));
        }
        if trimmed.is_empty() {
            return Err(format!("<{qname}> is never closed"));
        }
        if !s[..i].ends_with(char::is_whitespace) {
            return Err(format!("a bad attribute in <{qname}>"));
        }
        let a = name_end(trimmed);
        let aname = &trimmed[..a];
        let after = trimmed[a..].trim_start();
        let value = after.strip_prefix('=').map(str::trim_start);
        let Some((quote, body)) = value.and_then(|v| {
            let q = v.chars().next().filter(|q| *q == '"' || *q == '\'')?;
            Some((q, &v[1..]))
        }) else {
            return Err(format!("a bad attribute in <{qname}>"));
        };
        if aname.is_empty() {
            return Err(format!("a bad attribute in <{qname}>"));
        }
        let end = body
            .find(quote)
            .ok_or_else(|| format!("an unclosed attribute value in <{qname}>"))?;
        if body[..end].contains('<') {
            return Err(format!("a `<` in an attribute value of <{qname}>"));
        }
        // Attribute-value normalisation: a literal tab or newline is a space (a CR is already a newline).
        let value = body[..end].replace(['\t', '\n'], " ");
        el.attrs.push((
            local(aname).to_string(),
            unescape(&value).map_err(|(_, why)| why)?,
        ));
        i = s.len() - body.len() + end + 1;
    }
}

/// `s` with the five predefined entities and character references replaced; an error's offset in `s`.
fn unescape(s: &str) -> Result<String, (usize, String)> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let at = s.len() - rest.len() + amp;
        let after = &rest[amp + 1..];
        let n = after
            .find(|c: char| !(c.is_alphanumeric() || "#_-.:".contains(c)))
            .unwrap_or(after.len());
        if n == 0 || !after[n..].starts_with(';') {
            return Err((at, "a `&` that starts no entity (write `&amp;`)".into()));
        }
        let name = &after[..n];
        let c = match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ if name.starts_with('#') => {
                char_ref(name).ok_or_else(|| (at, format!("a bad character reference &{name};")))?
            }
            _ => return Err((at, format!("unknown entity &{name};"))),
        };
        out.push(c);
        rest = &after[n + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `#N` or `#xN` (digits only) as a character XML allows (its `Char`: tab, newline, CR, and from U+0020 on, without
/// surrogates, U+FFFE and U+FFFF).
fn char_ref(name: &str) -> Option<char> {
    let (digits, radix) = match name.strip_prefix("#x") {
        Some(h) => (h, 16),
        None => (name.strip_prefix('#')?, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let c = char::from_u32(u32::from_str_radix(digits, radix).ok()?)?;
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..)
        .then_some(c)
}

// --- keybinds ----------------------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
struct LeftOut {
    unsendable: usize,
    keycodes: usize,
    unknown: usize,
    conflicts: usize,
    includes: usize,
    /// More than `MAX_KEYS` keys: the rest weren't read.
    capped: bool,
}

impl LeftOut {
    fn note(&self) -> Option<String> {
        let parts: Vec<String> = [
            (
                self.unsendable,
                "with a Meta, Hyper, Mod2, Mod3 or Mod5 modifier (can't be sent on X11)",
            ),
            (self.keycodes, "given as a keycode (0x…)"),
            (
                self.unknown,
                "with an unknown modifier or key name (Openbox binds nothing)",
            ),
            (
                self.conflicts,
                "in conflict with an earlier keybind (Openbox drops it)",
            ),
            (
                self.includes,
                if self.includes == 1 {
                    "xi:include element not followed"
                } else {
                    "xi:include elements not followed"
                },
            ),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, why)| format!("{n} {why}"))
        .chain(
            self.capped
                .then(|| format!("everything after the first {MAX_KEYS} keys and actions")),
        )
        .collect();
        (!parts.is_empty()).then(|| format!("leaving out: {}", parts.join(", ")))
    }
}

/// One bound key sequence: its chords, category and the descriptions of its actions.
struct Bound {
    chords: Vec<String>,
    category: String,
    actions: Vec<String>,
}

/// A node of the key tree, as `keytree.c` builds it: its children by chord, and the bound sequence that ends here
/// (an index in `bound`). A node that is neither a leaf nor has children is a stub (a chain's start whose next key is
/// Openbox's key 0).
#[derive(Default)]
struct KeyNode {
    children: HashMap<String, usize>,
    leaf: Option<usize>,
}

struct Binds {
    bound: Vec<Bound>,
    /// The key tree; node 0 is the root.
    tree: Vec<KeyNode>,
    left: LeftOut,
    /// Keys and actions read so far (`MAX_KEYS`).
    work: usize,
}

impl Binds {
    fn new(left: LeftOut) -> Binds {
        Binds {
            bound: Vec::new(),
            tree: vec![KeyNode::default()],
            left,
            work: 0,
        }
    }

    /// Whether `n` more keys or actions fit under `MAX_KEYS`; counted when they do, noted when they don't.
    fn spend(&mut self, n: usize) -> bool {
        if self.work + n > MAX_KEYS {
            self.left.capped = true;
            return false;
        }
        self.work += n;
        true
    }

    /// The node for `chords`, made as needed; `None` when a proper prefix of it is a bound key (`tree_find`'s
    /// conflict, `openbox/keytree.c:107-136`).
    fn node(&mut self, chords: &[String]) -> Option<usize> {
        let mut at = 0;
        for c in chords {
            if self.tree[at].leaf.is_some() {
                return None;
            }
            at = match self.tree[at].children.get(c) {
                Some(&n) => n,
                None => {
                    self.tree.push(KeyNode::default());
                    let n = self.tree.len() - 1;
                    self.tree[at].children.insert(c.clone(), n);
                    n
                }
            };
        }
        Some(at)
    }

    /// `keyboard_bind` (`openbox/keyboard.c:142-176`) with `tree_find` (`openbox/keytree.c:107-136`): the same
    /// sequence gets the actions appended; one that is a proper prefix of a bound one (or a stub), or has one as a
    /// proper prefix, is dropped.
    fn bind(&mut self, chords: &[String], category: &str, actions: &[String]) {
        let fresh = self.tree.len();
        match self.node(chords) {
            Some(n) if let Some(i) = self.tree[n].leaf => {
                self.bound[i].actions.extend_from_slice(actions)
            }
            // Made just now: nothing was after it.
            Some(n) if n >= fresh => {
                self.tree[n].leaf = Some(self.bound.len());
                self.bound.push(Bound {
                    chords: chords.to_vec(),
                    category: category.into(),
                    actions: actions.to_vec(),
                });
            }
            _ => self.left.conflicts += 1,
        }
    }

    /// A chain whose next key didn't translate: Openbox still binds `chords` then a key-0 node (`tree_build`,
    /// `openbox/keytree.c:47-66`), which never fires or matches (`:116-119`), so `chords` becomes a chain's start and a
    /// later key `chords` conflicts. Dropped, as Openbox drops it, when that is itself a conflict.
    fn stub(&mut self, chords: &[String]) {
        if !chords.is_empty() {
            // A node with no leaf and no children is a stub; a leaf stays a leaf (the stub is the conflict).
            let _ = self.node(chords);
        }
    }

    /// `parse_key` (`openbox/config.c:446-486`): each space-separated key of `key` bound to the same content; nested
    /// keybinds make a chain (their actions win over this one's).
    fn keybind(&mut self, kb: &Element, chords: &mut Vec<String>, category: &str) {
        let Some(keys) = kb.attr("key") else {
            return;
        };
        let nested: Vec<&Element> = kb.elements().filter(|e| e.name == "keybind").collect();
        let actions: Vec<String> = if nested.is_empty() {
            kb.elements()
                .filter(|e| e.name == "action")
                .filter_map(describe)
                .collect()
        } else {
            Vec::new()
        };
        // No action binds nothing (`actions_parse` gives NULL without a name).
        // ponytail: "binds something" is read one level deep; walk the subtree if a chain of empty keybinds matters.
        let binds_any = !nested.is_empty() || !actions.is_empty();
        // `config.c:456`: split on single spaces (a newline in the attribute is one already). An empty piece (two
        // spaces) is Openbox's key 0 too: not listed (AC: skipped), but a stub after a chain's start.
        for k in keys.split(' ') {
            if !self.spend(1) {
                return;
            }
            let c = match translate(k) {
                Ok(c) => c,
                Err(why) => {
                    if !k.is_empty() {
                        *match why {
                            Why::Unsendable => &mut self.left.unsendable,
                            Why::Keycode => &mut self.left.keycodes,
                            Why::Unknown => &mut self.left.unknown,
                        } += 1;
                    }
                    if binds_any {
                        self.stub(chords);
                    }
                    continue;
                }
            };
            chords.push(c);
            if !nested.is_empty() {
                for n in &nested {
                    self.keybind(n, chords, category);
                }
            } else if !actions.is_empty() {
                if !self.spend(actions.len()) {
                    return;
                }
                self.bind(chords, category, &actions);
            }
            chords.pop();
        }
    }

    fn sheet(self) -> (Sheet, LeftOut) {
        let mut sheet = Sheet {
            title: "Openbox".into(),
            provider: "openbox".into(),
            binds: Default::default(),
        };
        for b in self.bound {
            let desc = b.actions.join("; ");
            sheet.binds.entry(b.category).or_default().push(Bind {
                key: b.chords.join(" "),
                action: desc.clone(),
                desc,
                when: Default::default(),
                run: Vec::new(),
            });
        }
        (sheet, self.left)
    }
}

/// The `include` elements that can drop keybinds: the root's own (one may bring a `<keyboard>`) and those inside the
/// read `<keyboard>`.
fn includes(root: &Element, kb: Option<&Element>) -> usize {
    fn under(e: &Element) -> usize {
        e.elements()
            .map(|c| usize::from(c.name == "include") + under(c))
            .sum()
    }
    root.elements().filter(|e| e.name == "include").count() + kb.map_or(0, under)
}

/// `parse_keyboard` (`openbox/config.c:488-510`) on the root's last `<keyboard>` (each one unbinds all before it);
/// with none, the built-in keys (`:1141-1147`: bound before the `keyboard` callback runs).
fn keyboard(root: &Element) -> (Sheet, LeftOut) {
    let kb = root.elements().filter(|e| e.name == "keyboard").last();
    let left = LeftOut {
        includes: includes(root, kb),
        ..LeftOut::default()
    };
    let Some(kb) = kb else {
        return defaults(left);
    };
    let mut binds = Binds::new(left);
    let mut category = CATEGORY.to_string();
    for child in &kb.children {
        match child {
            // A title comment heads the keybinds after it; one with `<` is commented-out XML.
            Node::Comment(c) if !c.contains('<') => {
                let t = collapse(c);
                if !t.is_empty() {
                    category = t;
                }
            }
            Node::Element(e) if e.name == "keybind" => binds.keybind(e, &mut Vec::new(), &category),
            _ => {}
        }
    }
    binds.sheet()
}

fn defaults(left: LeftOut) -> (Sheet, LeftOut) {
    let mut binds = Binds::new(left);
    for (key, action) in DEFAULTS {
        if let Ok(c) = translate(key) {
            binds.bind(&[c], CATEGORY, &[action.to_string()]);
        }
    }
    binds.sheet()
}

/// `s` on one line: control characters as spaces (they would reach the picker), whitespace runs as one space.
fn collapse(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// An action as text: its `name`, then ` child=value` per direct child with text only (collapsed); an Execute command
/// (`command`, or the legacy `execute`, `openbox/actions/execute.c:59-60`) as its value alone. Child elements with
/// elements in them (`finalactions`, `startupnotify`) are skipped. `None` without a name.
fn describe(action: &Element) -> Option<String> {
    let mut out = collapse(action.attr("name")?);
    if out.is_empty() {
        return None;
    }
    for c in action.elements() {
        let Some(v) = c.text_only() else {
            continue;
        };
        let v = collapse(&v);
        if c.name == "command" || c.name == "execute" {
            out.push_str(&format!(" {v}"));
        } else {
            out.push_str(&format!(" {}={v}", c.name));
        }
    }
    Some(out)
}

#[derive(Debug, PartialEq)]
enum Why {
    /// Meta, Hyper, Mod2, Mod3, Mod5: xdotool can't press them (FR-019).
    Unsendable,
    /// `0x…`, a raw keycode.
    Keycode,
    /// A modifier or key name Openbox doesn't know: it binds nothing.
    Unknown,
}

/// The X keysym names `XStringToKeysym` takes (keysymdef.h's and XF86keysym.h's), from keybind-find's table.
/// ponytail: libX11's other forms (`U20AC`, the Sun/DEC/HP vendor names) aren't taken. Add them if a report shows one.
fn is_keysym(name: &str) -> bool {
    static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            crate::portal::KEYSYMS
                .lines()
                .filter(|l| !l.starts_with('#') && !l.starts_with("U+"))
                .filter_map(|l| l.split(' ').next())
                .filter(|n| !n.is_empty())
                .collect()
        })
        .contains(name)
}

/// `translate_key` (`openbox/translate.c:105-160`, modifiers `:29-62`): `-`-separated, the last piece the key, the
/// others modifiers ignoring case; the key a keysym name, exactly. Openbox binds the keysym's keycode, so `W-A` fires
/// as `super+a`.
fn translate(key: &str) -> Result<String, Why> {
    let pieces: Vec<&str> = key.split('-').collect();
    let (name, mods) = pieces.split_last().ok_or(Why::Unknown)?;
    let mut out = Vec::with_capacity(mods.len());
    for m in mods {
        let is = |names: &[&str]| names.iter().any(|n| n.eq_ignore_ascii_case(m));
        out.push(if is(&["S", "Shift"]) {
            "shift"
        } else if is(&["C", "Control"]) {
            "ctrl"
        } else if is(&["A", "Alt", "Mod1"]) {
            "alt"
        } else if is(&["W", "Super", "Mod4"]) {
            "super"
        } else if is(&["M", "Meta", "H", "Hyper", "Mod2", "Mod3", "Mod5"]) {
            return Err(Why::Unsendable);
        } else {
            return Err(Why::Unknown);
        });
    }
    if name
        .as_bytes()
        .get(..2)
        .is_some_and(|b| b.eq_ignore_ascii_case(b"0x"))
    {
        return Err(Why::Keycode);
    }
    if !is_keysym(name) {
        return Err(Why::Unknown);
    }
    // ponytail: a shifted symbol (`W-exclam`) is listed as written; Openbox binds its keycode unshifted, so the
    // pick (xdotool adds shift) may not fire. README says so.
    let sym = match name.as_bytes() {
        [c] if c.is_ascii_uppercase() => name.to_ascii_lowercase(),
        _ => name.to_string(),
    };
    Ok(chord(&out, &sym))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/openbox/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// The sheet and left-out counts of an rc.xml text.
    fn binds(text: &str) -> (Sheet, LeftOut) {
        keyboard(&parse_xml(text).unwrap())
    }

    fn kb(body: &str) -> (Sheet, LeftOut) {
        binds(&format!(
            "<openbox_config><keyboard>{body}</keyboard></openbox_config>"
        ))
    }

    /// (category, key, desc) of every row, in sheet order.
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

    fn desc<'a>(sheet: &'a Sheet, key: &str) -> &'a str {
        let found: Vec<&Bind> = sheet
            .binds
            .values()
            .flatten()
            .filter(|b| b.key == key)
            .collect();
        assert_eq!(found.len(), 1, "{key}");
        &found[0].desc
    }

    fn row(c: &str, k: &str, d: &str) -> (String, String, String) {
        (c.into(), k.into(), d.into())
    }

    /// `rc.xml`: Openbox release-3.6.1's `data/rc.xml`, with a header comment after the declaration.
    #[test]
    fn stock_rc_xml() {
        let (sheet, left) = binds(&fixture("rc.xml"));
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str()),
            ("Openbox", "openbox")
        );
        assert_eq!(sheet.binds.values().flatten().count(), 24);
        assert_eq!(left, LeftOut::default());
        let count = |c: &str| sheet.binds[c].len();
        assert_eq!(count("Keybindings for desktop switching"), 13);
        assert_eq!(count("Keybindings for windows"), 3);
        assert_eq!(count("Keybindings for window switching"), 3);
        assert_eq!(
            count("Keybindings for window switching with the arrow keys"),
            4
        );
        assert_eq!(count("Keybindings for running applications"), 1);
        assert_eq!(sheet.binds.len(), 5);
        for (k, d) in [
            ("ctrl+alt+Left", "GoToDesktop to=left wrap=no"),
            ("super+F1", "GoToDesktop to=1"),
            ("alt+Escape", "Lower; FocusToBottom; Unfocus"),
            ("alt+shift+Tab", "PreviousWindow"),
            ("ctrl+alt+Tab", "NextWindow panels=yes desktop=yes"),
            ("alt+space", "ShowMenu menu=client-menu"),
            ("super+e", "Execute kfmclient openProfile filemanagement"),
        ] {
            assert_eq!(desc(&sheet, k), d, "{k}");
        }
        let all: Vec<&Bind> = sheet.binds.values().flatten().collect();
        assert!(all.iter().all(|b| b.action == b.desc && b.run.is_empty()));
    }

    #[test]
    fn alternatives_chains_and_categories() {
        let (sheet, left) = kb(r#"
            <keybind key="W-z"><action name="Close"/></keybind>
            <!-- Launch -->
            <keybind key="W-a  W-b"><action name="Execute"><command>xterm</command></action></keybind>
            <keybind key="W-x">
              <keybind key="t"><keybind key="x"><action name="Iconify"/></keybind></keybind>
            </keybind>
            <keybind key="W-y">
              <action name="Close"/>
              <keybind key="u"><action name="Raise"/></keybind>
            </keybind>
            <keybind><action name="Close"/></keybind>
            <keybind key="W-k" chroot="yes"><keybind key="j"><action name="Lower"/></keybind></keybind>
            <!-- <keybind key="W-q"><action name="Exit"/></keybind> -->
            <keybind key="W-m"><action name="Maximize"/></keybind>
        "#);
        assert_eq!(
            rows(&sheet),
            [
                row("Launch", "super+a", "Execute xterm"),
                row("Launch", "super+b", "Execute xterm"),
                row("Launch", "super+x t x", "Iconify"),
                row("Launch", "super+y u", "Raise"),
                row("Launch", "super+k j", "Lower"),
                row("Launch", "super+m", "Maximize"),
                row("openbox", "super+z", "Close"),
            ]
        );
        assert_eq!(left, LeftOut::default());
    }

    #[test]
    fn alternatives_and_a_chain() {
        let (sheet, _) = kb(r#"<keybind key="W-a W-b"><action name="Close"/></keybind>"#);
        assert_eq!(
            rows(&sheet),
            [
                row("openbox", "super+a", "Close"),
                row("openbox", "super+b", "Close")
            ]
        );
        let (sheet, _) = kb(r#"<!-- Chains -->
            <keybind key="W-a"><keybind key="t"><keybind key="x"><action name="Iconify"/></keybind></keybind></keybind>"#);
        assert_eq!(rows(&sheet), [row("Chains", "super+a t x", "Iconify")]);
    }

    #[test]
    fn same_key_twice_and_conflicts() {
        let (sheet, left) = kb(r#"
            <keybind key="W-a"><action name="Raise"/></keybind>
            <keybind key="W-a"><action name="Focus"/></keybind>
            <keybind key="W-c"><action name="Close"/></keybind>
            <keybind key="W-c"><keybind key="d"><action name="Lower"/></keybind></keybind>
            <keybind key="W-e"><keybind key="f"><action name="Lower"/></keybind></keybind>
            <keybind key="W-e"><action name="Close"/></keybind>
            <keybind key="Super-a"><action name="Unshade"/></keybind>
        "#);
        assert_eq!(
            rows(&sheet),
            [
                row("openbox", "super+a", "Raise; Focus; Unshade"),
                row("openbox", "super+c", "Close"),
                row("openbox", "super+e f", "Lower"),
            ]
        );
        assert_eq!(left.conflicts, 2);
    }

    #[test]
    fn last_keyboard_wins_and_defaults() {
        let (sheet, _) = binds(
            r#"<openbox_config>
                <keyboard><keybind key="W-a"><action name="Close"/></keybind></keyboard>
                <keyboard><keybind key="W-b"><action name="Lower"/></keybind></keyboard>
            </openbox_config>"#,
        );
        assert_eq!(rows(&sheet), [row("openbox", "super+b", "Lower")]);
        let want = [
            row("openbox", "alt+Tab", "NextWindow"),
            row("openbox", "alt+shift+Tab", "PreviousWindow"),
            row("openbox", "alt+F4", "Close"),
        ];
        let (sheet, left) = binds("<openbox_config><mouse/></openbox_config>");
        assert_eq!(rows(&sheet), want);
        assert_eq!(left, LeftOut::default());
    }

    #[test]
    fn key_notation() {
        for (k, want) in [
            ("W-e", "super+e"),
            ("Super-e", "super+e"),
            ("Mod4-e", "super+e"),
            ("c-a-Delete", "ctrl+alt+Delete"),
            ("S-A-Tab", "alt+shift+Tab"),
            ("Mod1-x", "alt+x"),
            ("W-A", "super+a"),
            ("W-exclam", "super+exclam"),
            ("XF86AudioMute", "XF86AudioMute"),
        ] {
            assert_eq!(translate(k), Ok(want.to_string()), "{k}");
        }
        for (k, why) in [
            ("H-x", Why::Unsendable),
            ("M-x", Why::Unsendable),
            ("Mod3-x", Why::Unsendable),
            ("Mod5-x", Why::Unsendable),
            ("Mod2-x", Why::Unsendable),
            ("W-0x26", Why::Keycode),
            ("W-Return2", Why::Unknown),
            ("Q-x", Why::Unknown),
            ("W-return", Why::Unknown),
            ("W-", Why::Unknown),
        ] {
            assert_eq!(translate(k), Err(why), "{k}");
        }
        let (sheet, left) = kb(r#"
            <keybind key="H-x M-x Mod3-x Mod5-x Mod2-x W-0x26 W-Return2 Q-x W-return W-Return">
              <action name="Close"/>
            </keybind>
        "#);
        assert_eq!(rows(&sheet), [row("openbox", "super+Return", "Close")]);
        assert_eq!(
            left,
            LeftOut {
                unsendable: 5,
                keycodes: 1,
                unknown: 3,
                ..LeftOut::default()
            }
        );
        assert_eq!(
            left.note().unwrap(),
            "leaving out: 5 with a Meta, Hyper, Mod2, Mod3 or Mod5 modifier (can't be sent on X11), 1 given as a \
             keycode (0x…), 3 with an unknown modifier or key name (Openbox binds nothing)"
        );
    }

    #[test]
    fn descriptions() {
        let (sheet, _) = kb(r#"
            <keybind key="W-a"><action name="Execute"><execute>  a   b </execute><prompt>Sure?</prompt></action></keybind>
            <keybind key="W-b"><action name="Execute"><command><![CDATA[x && y <z>]]></command></action></keybind>
            <keybind key="W-c"><action name="GoToDesktop"><to><!-- c -->left</to></action><action/></keybind>
        "#);
        assert_eq!(desc(&sheet, "super+a"), "Execute a b prompt=Sure?");
        assert_eq!(desc(&sheet, "super+b"), "Execute x && y <z>");
        assert_eq!(desc(&sheet, "super+c"), "GoToDesktop to=left");
    }

    #[test]
    fn xml_reader() {
        let root = parse_xml(
            "<?xml version=\"1.0\"?>\r\n<!DOCTYPE openbox_config>\r\n<?pi x?>\r\n\
             <ob:openbox_config xmlns:ob=\"http://openbox.org/3.4/rc\">\r\n\
             <a b='1' c=\"&amp;&#x41;&#65;\"/>\r\n<t>&lt;&amp;&#x41;&#65;&gt;&quot;&apos;</t>\r\n\
             <xi:include href=\"x.xml\"/>\r\n</ob:openbox_config>\r\n",
        )
        .unwrap();
        assert_eq!(root.name, "openbox_config");
        let els: Vec<&Element> = root.elements().collect();
        assert_eq!(
            els[0].attrs,
            [
                ("b".to_string(), "1".to_string()),
                ("c".into(), "&AA".into())
            ]
        );
        assert_eq!(els[1].text_only().unwrap(), "<&AA>\"'");
        assert_eq!(els[2].name, "include");
        for (text, want) in [
            ("<a>\n<b>\n</a>", "line 3: </a> closes <b>"),
            ("<a>\n\n<b></b>", "line 1: <a> is never closed"),
            ("<a>\n&nbsp;</a>", "line 2: unknown entity &nbsp;"),
            (
                "<!DOCTYPE a [\n<!ENTITY x 'y'>]>\n<a/>",
                "line 1: a DOCTYPE with an internal subset",
            ),
            ("<a\nb=1/>", "line 1: a bad attribute in <a>"),
            ("<a/>\n<b/>", "line 2: <b> after the root element"),
            ("\n\n", "no root element"),
        ] {
            assert_eq!(parse_xml(text).unwrap_err(), want, "{text:?}");
        }
    }

    #[test]
    fn xinclude_is_counted_where_it_can_drop_keybinds() {
        // The root's own (one may bring a <keyboard>) and the read <keyboard>'s; not under <menu> or an older
        // <keyboard>.
        let (_, left) = binds(
            r#"<openbox_config xmlns:xi="http://www.w3.org/2001/XInclude">
                <xi:include href="k.xml"/>
                <menu><xi:include href="m.xml"/></menu>
                <keyboard><xi:include href="old.xml"/></keyboard>
                <keyboard><keybind key="W-a"><xi:include href="a.xml"/></keybind></keyboard>
            </openbox_config>"#,
        );
        assert_eq!(left.includes, 2);
        assert_eq!(
            left.note().unwrap(),
            "leaving out: 2 xi:include elements not followed"
        );
        let (_, left) = binds("<openbox_config><xi:include href=\"k.xml\"/></openbox_config>");
        assert_eq!(
            left.note().unwrap(),
            "leaving out: 1 xi:include element not followed"
        );
    }
    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-openbox-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(dir: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir.join("openbox")).unwrap();
        let p = dir.join("openbox/rc.xml");
        std::fs::write(&p, body).unwrap();
        p
    }

    fn rc(key: &str) -> String {
        format!(
            "<openbox_config><keyboard><keybind key=\"{key}\"><action name=\"Close\"/></keybind></keyboard></openbox_config>"
        )
    }

    fn keys(sheet: &Sheet) -> Vec<String> {
        sheet
            .binds
            .values()
            .flatten()
            .map(|b| b.key.clone())
            .collect()
    }

    #[test]
    fn config_dirs_from_env() {
        let dirs = |vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            config_dirs(&move |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()))
        };
        let p = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(dirs(&[("HOME", "/h")]), p(&["/h/.config", "/etc/xdg"]));
        assert_eq!(
            dirs(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/c")]),
            p(&["/c", "/etc/xdg"])
        );
        assert_eq!(
            dirs(&[
                ("HOME", "/h"),
                ("XDG_CONFIG_HOME", ""),
                ("XDG_CONFIG_DIRS", "")
            ]),
            p(&["/h/.config", "/etc/xdg"])
        );
        assert_eq!(
            dirs(&[("XDG_CONFIG_HOME", "/c"), ("XDG_CONFIG_DIRS", "a::b:a")]),
            p(&["/c", "a", "b"])
        );
        // The home already listed keeps its place.
        assert_eq!(
            dirs(&[("XDG_CONFIG_HOME", "/b"), ("XDG_CONFIG_DIRS", "/a:/b")]),
            p(&["/a", "/b"])
        );
    }

    #[test]
    fn lookup() {
        let d = tmp("lookup");
        let (home, etc) = (d.join("home"), d.join("etc/xdg"));
        let lxde = d.join("lxde-rc.xml");
        std::fs::write(&lxde, rc("W-l")).unwrap();
        let vars = [
            ("XDG_CONFIG_HOME".to_string(), home.display().to_string()),
            ("XDG_CONFIG_DIRS".to_string(), etc.display().to_string()),
        ];
        let env = move |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let reply = |p: &Path| format!("_OB_CONFIG_FILE(UTF8_STRING) = \"{}\"\n", p.display());
        const NONE: &str = "_OB_CONFIG_FILE:  not found.\n";
        let go = |prop: Option<&str>| read(prop, &env);

        // Nothing found: the defaults, and a note.
        let (sheet, notes) = go(Some(NONE));
        assert_eq!(keys(&sheet), ["alt+Tab", "alt+shift+Tab", "alt+F4"]);
        assert_eq!(notes, ["no rc.xml loads; listing Openbox's built-in keys"]);
        // The property's file.
        let (sheet, notes) = go(Some(&reply(&lxde)));
        assert_eq!(
            (keys(&sheet), notes.len()),
            (vec!["super+l".to_string()], 0)
        );

        put(&etc, &rc("W-e"));
        // The property's file beats the XDG one.
        assert_eq!(keys(&go(Some(&reply(&lxde))).0), ["super+l"]);
        // No property, no xprop answer, an empty value, or a file that doesn't exist: the XDG lookup, silently.
        let missing = reply(&d.join("missing.xml"));
        let empty = "_OB_CONFIG_FILE(UTF8_STRING) = \"\"\n";
        for prop in [Some(NONE), None, Some(empty), Some(&missing)] {
            let (sheet, notes) = go(prop);
            assert_eq!(
                (keys(&sheet), notes.len()),
                (vec!["super+e".to_string()], 0),
                "{prop:?}"
            );
        }
        // A relative path: a note, then the XDG lookup. Notes are printable: the property is any X client's.
        let (sheet, notes) = go(Some(
            "_OB_CONFIG_FILE(UTF8_STRING) = \"lxde\\033[31m-rc.xml\"\n",
        ));
        assert_eq!(keys(&sheet), ["super+e"]);
        assert_eq!(
            notes,
            ["_OB_CONFIG_FILE names a relative path (lxde\\u{1b}[31m-rc.xml); looking in the XDG config dirs"]
        );
        // xprop's octal escapes are undone: a file named `é.xml`.
        let accent = d.join("é.xml");
        std::fs::write(&accent, rc("W-z")).unwrap();
        let escaped = format!(
            "_OB_CONFIG_FILE(UTF8_STRING) = \"{}/\\303\\251.xml\"\n",
            d.display()
        );
        assert_eq!(keys(&go(Some(&escaped)).0), ["super+z"]);
        // The home's file first; one of another type is noted and passed over.
        let mine = put(&home, &rc("W-h"));
        assert_eq!(keys(&go(Some(NONE)).0), ["super+h"]);
        std::fs::write(&mine, "<openbox_menu/>").unwrap();
        let (sheet, notes) = go(Some(NONE));
        assert_eq!(keys(&sheet), ["super+e"]);
        assert_eq!(
            notes,
            [format!(
                "{}: its root element is <openbox_menu>, not <openbox_config>; skipped",
                mine.display()
            )]
        );
        // A malformed one too, with its line.
        std::fs::write(&mine, "<openbox_config>\n<keyboard>\n</openbox_config>").unwrap();
        let (_, notes) = go(Some(NONE));
        assert_eq!(
            notes,
            [format!(
                "{}: line 3: </openbox_config> closes <keyboard>; skipped",
                mine.display()
            )]
        );
        // The property's file that doesn't load: noted, then the XDG lookup.
        std::fs::remove_file(&mine).unwrap();
        std::fs::write(&lxde, [0xffu8, 0xfe]).unwrap();
        let (sheet, notes) = go(Some(&reply(&lxde)));
        assert_eq!(keys(&sheet), ["super+e"]);
        assert_eq!(notes, [format!("{}: not UTF-8; skipped", lxde.display())]);
        // A directory, or a FIFO (opened without waiting for a writer), where the home's rc.xml would be: noted, and
        // the next dir's is used.
        std::fs::create_dir(&mine).unwrap();
        let (sheet, notes) = go(Some(NONE));
        assert_eq!(keys(&sheet), ["super+e"]);
        assert_eq!(
            notes,
            [format!("{}: not a regular file; skipped", mine.display())]
        );
        std::fs::remove_dir(&mine).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&mine)
            .status()
            .unwrap();
        assert!(made.success());
        let (sheet, notes) = go(Some(NONE));
        assert_eq!(keys(&sheet), ["super+e"]);
        assert_eq!(
            notes,
            [format!("{}: not a regular file; skipped", mine.display())]
        );
    }

    #[test]
    fn property_escapes() {
        let path = |v: &str| property_path(&format!("_OB_CONFIG_FILE(UTF8_STRING) = {v}\n"));
        assert_eq!(
            path(r#""/a\303\251\n\t\033\"\\b\7""#),
            Some(PathBuf::from("/aé\n\t\u{1b}\"\\b7"))
        );
        assert_eq!(path(r#""""#), None);
        assert_eq!(path(r#""/unterminated"#), None);
        assert_eq!(property_path("_OB_CONFIG_FILE:  not found.\n"), None);
    }
    #[test]
    fn config_dirs_in_order_and_the_size_cap() {
        let d = tmp("dirs");
        let (a, b) = (d.join("a"), d.join("b"));
        let vars = [
            (
                "XDG_CONFIG_HOME".to_string(),
                d.join("home").display().to_string(),
            ),
            (
                "XDG_CONFIG_DIRS".to_string(),
                format!("{}:{}", a.display(), b.display()),
            ),
        ];
        let env = move |k: &str| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let go = || read(Some("_OB_CONFIG_FILE:  not found.\n"), &env);
        let first = put(&a, &rc("W-a"));
        put(&b, &rc("W-b"));
        assert_eq!(keys(&go().0), ["super+a"]);
        // A bad first falls through to the second.
        std::fs::write(&first, "<openbox_config>").unwrap();
        let (sheet, notes) = go();
        assert_eq!(keys(&sheet), ["super+b"]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        // Over 4 MiB isn't read; exactly 4 MiB is.
        let body = rc("W-c");
        let pad = |n: usize| format!("{body}{}", " ".repeat(n - body.len()));
        std::fs::write(&first, pad(MAX_SIZE as usize)).unwrap();
        assert_eq!(keys(&go().0), ["super+c"]);
        std::fs::write(&first, pad(MAX_SIZE as usize + 1)).unwrap();
        let (sheet, notes) = go();
        assert_eq!(keys(&sheet), ["super+b"]);
        assert_eq!(
            notes,
            [format!("{}: larger than 4 MiB; skipped", first.display())]
        );
    }

    #[test]
    fn notes_escape_control_and_bidi_characters() {
        assert_eq!(printable("a\u{1b}[31mb\nc é"), "a\\u{1b}[31mb\\nc é");
        assert_eq!(
            printable("x\u{202e}y\u{2066}z\u{200f}\u{2028}"),
            "x\\u{202e}y\\u{2066}z\\u{200f}\\u{2028}"
        );
    }

    #[test]
    fn titles_and_actions_drop_control_characters() {
        let (sheet, _) = kb("<!-- A\u{1b}[2Jtitle -->\
            <keybind key=\"W-a\"><action name=\"Ex\u{7}ecute\"><command>rm\u{1b}x\u{85}y</command></action></keybind>");
        assert_eq!(
            rows(&sheet),
            [row("A [2Jtitle", "super+a", "Ex ecute rm x y")]
        );
    }

    #[test]
    fn repeated_modifiers_are_one() {
        // Openbox ORs the masks (translate.c:126-130); `chord` keeps one of each.
        for k in ["W-W-a", "Super-W-a", "Mod4-super-a"] {
            assert_eq!(translate(k), Ok("super+a".to_string()), "{k}");
        }
        assert_eq!(translate("S-Shift-a"), Ok("shift+a".to_string()));
    }

    #[test]
    fn an_empty_key_piece_is_key_zero() {
        // `g_strsplit(" ")` gives "" for two spaces (config.c:456): not listed, not counted (AC #3), but after a
        // chain's start it is a key-0 node (keytree.c:47-66), so a later key `W-a` conflicts.
        let (sheet, left) = kb(r#"
            <keybind key=" b  c "><action name="Close"/></keybind>
            <keybind key="W-a"><keybind key="t  u"><action name="Lower"/></keybind></keybind>
            <keybind key="W-a"><action name="Raise"/></keybind>
        "#);
        assert_eq!(keys(&sheet), ["b", "c", "super+a t", "super+a u"]);
        assert_eq!(
            left,
            LeftOut {
                conflicts: 1,
                ..LeftOut::default()
            }
        );
    }
    #[test]
    fn non_ascii_keys_are_unknown() {
        assert_eq!(translate("W-€"), Err(Why::Unknown));
        assert_eq!(translate("€"), Err(Why::Unknown));
        assert_eq!(translate("W-0X26"), Err(Why::Keycode));
    }

    #[test]
    fn nesting_is_bounded() {
        let deep = |n: usize| {
            format!(
                "<openbox_config>{}{}</openbox_config>",
                "<keybind>".repeat(n),
                "</keybind>".repeat(n)
            )
        };
        // 256 elements deep, the root among them, load; 257 don't (libxml2's limit).
        assert!(parse_xml(&deep(255)).is_ok());
        assert_eq!(
            parse_xml(&deep(256)).unwrap_err(),
            "line 1: elements nested deeper than 256"
        );
        assert!(parse_xml(&deep(300)).is_err());
        // The deepest chain that loads: openbox_config, keyboard, 253 keybinds and the action make 256.
        let chain = format!(
            "<openbox_config><keyboard>{}<action name=\"Close\"/>{}</keyboard></openbox_config>",
            "<keybind key=\"a\">".repeat(253),
            "</keybind>".repeat(253)
        );
        let (sheet, _) = binds(&chain);
        assert_eq!(keys(&sheet), [vec!["a"; 253].join(" ")]);
    }
    /// `n` keys of the keysym table's letters, F-keys and digits, repeating.
    fn many(n: usize) -> String {
        let names: Vec<String> = ('a'..='l')
            .map(String::from)
            .chain((1..=35).map(|f| format!("F{f}")))
            .collect();
        (0..n)
            .map(|i| names[i % names.len()].as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn actions(sheet: &Sheet) -> usize {
        sheet
            .binds
            .values()
            .flatten()
            .map(|b| b.desc.split("; ").count())
            .sum()
    }

    #[test]
    fn chain_blow_up_is_capped() {
        let level = |inner: &str| format!("<keybind key=\"a b c d e f g h\">{inner}</keybind>");
        let mut body = "<action name=\"Close\"/>".to_string();
        for _ in 0..8 {
            body = level(&body);
        }
        let (sheet, left) = kb(&body);
        // Depth first, each leaf a key and an action, each level above it a key: the 5000 run out at the 2,332nd
        // leaf (worked out by walking the same tree).
        assert!(left.capped);
        assert_eq!(sheet.binds.values().flatten().count(), 2331);
        assert_eq!(
            left.note().unwrap(),
            "leaving out: everything after the first 5000 keys and actions"
        );
    }

    #[test]
    fn the_cap_counts_actions() {
        // 60 keys (47 distinct: repeats append), each binding 100 actions: 49 fit (101 each), the 50th's don't.
        let acts = "<action name=\"Close\"/>".repeat(100);
        let (sheet, left) = kb(&format!("<keybind key=\"{}\">{acts}</keybind>", many(60)));
        assert!(left.capped);
        assert_eq!(
            (sheet.binds.values().flatten().count(), actions(&sheet)),
            (47, 4900)
        );
        // Exactly 5000 isn't capped: 2500 keys of one action each (47 distinct, repeats appended).
        let one = "<action name=\"Close\"/>";
        let (sheet, left) = kb(&format!("<keybind key=\"{}\">{one}</keybind>", many(2500)));
        assert!(!left.capped);
        assert_eq!(
            (sheet.binds.values().flatten().count(), actions(&sheet)),
            (47, 2500)
        );
        let (_, left) = kb(&format!("<keybind key=\"{}\">{one}</keybind>", many(2501)));
        assert!(left.capped);
    }

    #[test]
    fn a_deep_chain_with_many_alternatives() {
        // 252 keybinds of `a`, the last with 4700 alternatives: 252 keys, then 2 per alternative until 5000.
        let chain = format!(
            "<openbox_config><keyboard>{}<keybind key=\"{}\"><action name=\"Close\"/></keybind>{}</keyboard></openbox_config>",
            "<keybind key=\"a\">".repeat(252),
            many(4700),
            "</keybind>".repeat(252)
        );
        let (sheet, left) = binds(&chain);
        assert!(left.capped);
        assert_eq!(
            (sheet.binds.values().flatten().count(), actions(&sheet)),
            (47, 2374)
        );
    }
    #[test]
    fn a_chain_with_a_failed_key_still_starts_a_chain() {
        // keytree.c:47-66: `W-a` then a key-0 node, so a later `W-a` conflicts.
        let (sheet, left) = kb(r#"
            <keybind key="W-a"><keybind key="Q-x"><action name="Close"/></keybind></keybind>
            <keybind key="W-a"><action name="Lower"/></keybind>
            <keybind key="W-a"><keybind key="t"><action name="Raise"/></keybind></keybind>
        "#);
        assert_eq!(rows(&sheet), [row("openbox", "super+a t", "Raise")]);
        assert_eq!((left.unknown, left.conflicts), (1, 1));
        // A bound key first: the failed chain after it changes nothing; a failed first key never conflicts.
        let (sheet, left) = kb(r#"
            <keybind key="W-a"><action name="Lower"/></keybind>
            <keybind key="W-a"><keybind key="Q-x"><action name="Close"/></keybind></keybind>
            <keybind key="Q-y"><keybind key="t"><action name="Close"/></keybind></keybind>
            <keybind key="t"><action name="Raise"/></keybind>
        "#);
        assert_eq!(
            rows(&sheet),
            [
                row("openbox", "super+a", "Lower"),
                row("openbox", "t", "Raise")
            ]
        );
        assert_eq!((left.unknown, left.conflicts), (2, 0));
    }

    #[test]
    fn attribute_newlines_split_alternatives() {
        let (sheet, _) = kb("<keybind key=\"W-a\n   W-b\"><action name=\"Close\"/></keybind>");
        assert_eq!(keys(&sheet), ["super+a", "super+b"]);
        let root = parse_xml("<a b=\"x\ty&#10;z\"/>").unwrap();
        assert_eq!(root.attr("b"), Some("x y\nz"));
    }

    #[test]
    fn character_references() {
        for (text, want) in [
            ("&#65;&#x41;&#x1F600;&#9;", Ok("AA😀\t".to_string())),
            ("&#+65;", Err("a `&` that starts no entity (write `&amp;`)")),
            (
                "&#x+41;",
                Err("a `&` that starts no entity (write `&amp;`)"),
            ),
            ("&#0;", Err("a bad character reference &#0;")),
            ("&#1;", Err("a bad character reference &#1;")),
            ("&#xD800;", Err("a bad character reference &#xD800;")),
            ("&#xFFFE;", Err("a bad character reference &#xFFFE;")),
            ("&#x110000;", Err("a bad character reference &#x110000;")),
            (
                "&#99999999999999999999;",
                Err("a bad character reference &#99999999999999999999;"),
            ),
            ("&#;", Err("a bad character reference &#;")),
            (
                "&averyveryverylongname;",
                Err("unknown entity &averyveryverylongname;"),
            ),
            ("a && b", Err("a `&` that starts no entity (write `&amp;`)")),
            ("a &", Err("a `&` that starts no entity (write `&amp;`)")),
        ] {
            assert_eq!(
                unescape(text).map_err(|(_, why)| why),
                want.map_err(str::to_string),
                "{text}"
            );
        }
    }

    #[test]
    fn xml_error_rows() {
        for (text, want) in [
            ("<a><!-- x", "line 1: unclosed comment"),
            ("<a><!-- x -- y --></a>", "line 1: `--` inside a comment"),
            ("<a><!-- x ---></a>", "line 1: `--` inside a comment"),
            ("<a><![CDATA[x</a>", "line 1: unclosed CDATA section"),
            ("<?xml version='1.0'", "line 1: unclosed processing instruction"),
            ("<!DOCTYPE a", "line 1: unclosed DOCTYPE"),
            ("<!DOCTYPE a SYSTEM \"x[y>\" [\n<!ENTITY e 'f'>]>\n<a/>", "line 1: a DOCTYPE with an internal subset"),
            ("<!ELEMENT a ANY>", "line 1: markup this reader doesn't take (`<!` other than a comment, CDATA or DOCTYPE)"),
            ("<!doctype a>", "line 1: markup this reader doesn't take (`<!` other than a comment, CDATA or DOCTYPE)"),
            ("<a></a", "line 1: unclosed end tag"),
            ("</a>", "line 1: </a> with no open element"),
            ("x<a/>", "line 1: text outside the root element"),
            ("<![CDATA[x]]><a/>", "line 1: CDATA outside the root element"),
            ("<a>\n<command>a && b</command></a>", "line 2: a `&` that starts no entity (write `&amp;`)"),
            ("<a b=\"&nbsp;\"/>", "line 1: unknown entity &nbsp;"),
            ("<a b=\"<\"/>", "line 1: a `<` in an attribute value of <a>"),
            ("<a b=\"x/>", "line 1: an unclosed attribute value in <a>"),
            ("<a =\"x\"/>", "line 1: a bad attribute in <a>"),
            ("<a b=\"1\"c=\"2\"/>", "line 1: a bad attribute in <a>"),
            ("<a>< b/></a>", "line 1: a `<` that starts no tag"),
            ("<a>\r\n\r\n</b>", "line 3: </b> closes <a>"),
        ] {
            assert_eq!(parse_xml(text).unwrap_err(), want, "{text:?}");
        }
        // A BOM, and a DOCTYPE with a quoted `>`, are fine.
        let root = parse_xml("\u{feff}<!DOCTYPE a SYSTEM \"a>b\"><a/>").unwrap();
        assert_eq!(root.name, "a");
    }

    #[test]
    fn names_are_case_sensitive() {
        let (sheet, _) = binds(
            "<openbox_config><Keyboard><keybind key=\"W-a\"><action name=\"Close\"/></keybind></Keyboard></openbox_config>",
        );
        assert_eq!(keys(&sheet), ["alt+Tab", "alt+shift+Tab", "alt+F4"]);
    }

    #[test]
    fn empty_and_partial_keyboards() {
        // An empty last <keyboard/> unbinds everything, the defaults too.
        let (sheet, _) = binds(
            "<openbox_config><keyboard><keybind key=\"W-a\"><action name=\"Close\"/></keybind></keyboard><keyboard/></openbox_config>",
        );
        assert!(sheet.binds.is_empty());
        let (sheet, left) = kb(r#"
            <keybind key="W-n"/>
            <keybind key="W-n"><action name="Close"/></keybind>
            <keybind key="W-c"><keybind key="a b"><action name="Close"/></keybind></keybind>
        "#);
        assert_eq!(keys(&sheet), ["super+n", "super+c a", "super+c b"]);
        assert_eq!(left, LeftOut::default());
    }

    #[test]
    fn category_comments() {
        let (sheet, _) = kb(r#"
            <!-- Window
                 keys -->
            <keybind key="W-a"><!-- inner --><keybind key="b"><!-- not a title --><action name="Close"/></keybind></keybind>
            <!--   -->
            <keybind key="W-c"><action name="Close"/></keybind>
        "#);
        assert_eq!(
            rows(&sheet),
            [
                row("Window keys", "super+a b", "Close"),
                row("Window keys", "super+c", "Close")
            ]
        );
    }

    #[test]
    fn conflict_note() {
        let (_, left) = kb(r#"
            <keybind key="W-c"><action name="Close"/></keybind>
            <keybind key="W-c"><keybind key="d"><action name="Lower"/></keybind></keybind>
        "#);
        assert_eq!(
            left.note().unwrap(),
            "leaving out: 1 in conflict with an earlier keybind (Openbox drops it)"
        );
        let two = LeftOut {
            includes: 2,
            ..LeftOut::default()
        };
        assert_eq!(
            two.note().unwrap(),
            "leaving out: 2 xi:include elements not followed"
        );
    }
}
