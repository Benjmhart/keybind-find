//! LibreOffice: the shortcuts of the module in front (Writer, Calc, …) and the global ones, read as LibreOffice reads
//! them: the installed registry's `.xcd` files layered as configmgr layers them, the user's
//! `registrymodifications.xcu` on top, each key as `XCUBasedAcceleratorConfiguration::impl_ts_load` takes it, labelled
//! as `UICommandDescription` names it. Citations are at `libreoffice-26.8.1.1`. Nothing is run: `soffice` on `PATH` is
//! only resolved to find the install.

use super::openbox::printable;
use super::read_capped;
use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::bail;
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const OOR: &str = "http://openoffice.org/2001/registry";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// The largest file read (`main.xcd` is about 2 MB).
const MAX_SIZE: u64 = 16 << 20;
/// roxmltree's node limit per file (`main.xcd` has about 56,000).
const NODES_LIMIT: u32 = 1_000_000;
/// How deep a file's elements may nest (the registry's nest about 8).
const MAX_DEPTH: usize = 64;
/// Rows listed per sheet.
const MAX_ROWS: usize = 5000;
/// Where LibreOffice is looked for when no `soffice` on `PATH` leads to one.
const ROOTS: &[&str] = &["/usr/lib/libreoffice", "/usr/lib64/libreoffice"];

/// A module: the window class LibreOffice gives it, its accelerator module, the sheet's title and its labels
/// component. The class is `titlebarupdate.cxx` `impl_updateApplicationID` 115-167 (X11's `WM_CLASS` class; Wayland's
/// app id is the same name, `IconHelper.cxx` `GetAppIconName`); the labels component is each factory's
/// `ooSetupFactoryCommandConfigRef` in the installed registry.
struct Module {
    class: &'static str,
    id: &'static str,
    title: &'static str,
    labels: &'static str,
}

const MODULES: &[Module] = &[
    Module {
        class: "libreoffice-writer",
        id: "com.sun.star.text.TextDocument",
        title: "LibreOffice Writer",
        labels: "WriterCommands",
    },
    Module {
        class: "libreoffice-calc",
        id: "com.sun.star.sheet.SpreadsheetDocument",
        title: "LibreOffice Calc",
        labels: "CalcCommands",
    },
    Module {
        class: "libreoffice-impress",
        id: "com.sun.star.presentation.PresentationDocument",
        title: "LibreOffice Impress",
        labels: "DrawImpressCommands",
    },
    Module {
        class: "libreoffice-draw",
        id: "com.sun.star.drawing.DrawingDocument",
        title: "LibreOffice Draw",
        labels: "DrawImpressCommands",
    },
    Module {
        class: "libreoffice-math",
        id: "com.sun.star.formula.FormulaProperties",
        title: "LibreOffice Math",
        labels: "MathCommands",
    },
    Module {
        class: "libreoffice-base",
        id: "com.sun.star.sdb.OfficeDatabaseDocument",
        title: "LibreOffice Base",
        labels: "DbuCommands",
    },
    Module {
        class: "libreoffice-startcenter",
        id: "com.sun.star.frame.StartModule",
        title: "LibreOffice Start Center",
        labels: "StartModuleCommands",
    },
];

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A LibreOffice window: one of the seven module classes, any case.
pub(crate) fn is_libreoffice(class: &str) -> bool {
    MODULES.iter().any(|m| m.class.eq_ignore_ascii_case(class))
}

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let (sheet, notes) = read(&|k| std::env::var(k).ok(), ctx.class.as_deref(), ROOTS)?;
    for note in notes {
        eprintln!("keybind-find: libreoffice: {note}");
    }
    Ok(sheet)
}

/// The sheet and the stderr notes (printable). `env`: keybind-find's environment (`PATH`, `HOME`, `XDG_CONFIG_HOME`);
/// `roots`: the installs looked at when `PATH` has none.
fn read(env: Env, class: Option<&str>, roots: &[&str]) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut notes = Vec::new();
    let module = match class.and_then(|c| MODULES.iter().find(|m| m.class.eq_ignore_ascii_case(c)))
    {
        Some(m) => m,
        None => {
            notes.push(format!(
                "the window isn't a LibreOffice module's (class {:?}): listing Writer's keys",
                class.unwrap_or("")
            ));
            &MODULES[0]
        }
    };
    let root = install_root(env, roots)?;
    let sheet = listing(
        &root.join("share").join("registry"),
        user_file(env).as_deref(),
        module,
        NODES_LIMIT,
        &mut notes,
    );
    Ok((sheet, notes.iter().map(|n| printable(n)).collect()))
}

// --- where ----------------------------------------------------------------------------------------------------------

/// The install whose `program/soffice` is the first `soffice` on `PATH` (symlinks resolved), else the first of `roots`
/// that has one. Nothing is run.
fn install_root(env: Env, roots: &[&str]) -> anyhow::Result<PathBuf> {
    let path = env("PATH").unwrap_or_default();
    let on_path = std::env::split_paths(&path)
        .filter(|d| !d.as_os_str().is_empty())
        .filter_map(|d| d.join("soffice").canonicalize().ok())
        .find_map(|f| {
            let program = f.parent()?;
            (f.is_file() && program.file_name()? == "program")
                .then(|| program.parent().map(Path::to_path_buf))?
        });
    if let Some(root) = on_path {
        return Ok(root);
    }
    match roots
        .iter()
        .map(PathBuf::from)
        .find(|r| r.join("program").join("soffice").is_file())
    {
        Some(r) => Ok(r),
        None => bail!(
            "no LibreOffice found: no soffice on PATH under a program/ directory, and no program/soffice in {}",
            roots.join(" or ")
        ),
    }
}

/// `registrymodifications.xcu` under `UserInstallation=$SYSUSERCONFIG/libreoffice/4` (Arch's bootstraprc), with
/// `$SYSUSERCONFIG` as `osl_psz_getConfigDir` (`sal/osl/unx/security.cxx:420-470`) finds it.
/// ponytail: `-env:UserInstallation=` or an edited bootstraprc isn't followed.
fn user_file(env: Env) -> Option<PathBuf> {
    let config = env("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty() && Path::new(d).exists())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(config.join("libreoffice/4/user/registrymodifications.xcu"))
}

// --- the layers -----------------------------------------------------------------------------------------------------

/// The module's sheet from the registry directory `registry` (and its `res/`), with the user file applied.
fn listing(
    registry: &Path,
    user: Option<&Path>,
    module: &Module,
    nodes_limit: u32,
    notes: &mut Vec<String>,
) -> Sheet {
    let mut reg = Registry::new(module.labels);
    load_layer(&mut reg, registry, nodes_limit, notes);
    load_layer(&mut reg, &registry.join("res"), nodes_limit, notes);
    let locale = user
        .and_then(|u| apply_user(&mut reg, u, nodes_limit, notes))
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "en-US".into());
    to_sheet(&reg, module, &locale, MAX_ROWS, notes)
}

/// A file parsed as configmgr's reader needs it: no DTD, at most `nodes_limit` nodes, no deeper than `MAX_DEPTH`, its
/// root `oor:<root>`.
fn parse<'a>(text: &'a str, root: &str, nodes_limit: u32) -> Result<Document<'a>, String> {
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit,
        ..ParsingOptions::default()
    };
    let doc = Document::parse_with_options(text, options).map_err(|e| e.to_string())?;
    let el = doc.root_element();
    if !el.has_tag_name((OOR, root)) {
        return Err(format!(
            "its root element <{}> isn't the registry's <oor:{root}>",
            el.tag_name().name()
        ));
    }
    // The walk follows the registry's own shape (a few levels), so this only keeps hostile nesting out.
    if doc
        .descendants()
        .any(|n| n.ancestors().nth(MAX_DEPTH).is_some())
    {
        return Err(format!("its elements nest deeper than {MAX_DEPTH}"));
    }
    Ok(doc)
}

/// `Components::parseXcdFiles` (`configmgr/source/components.cxx:750-818`): every `.xcd` in `dir`, each after the
/// files its `<dependency file="…"/>` name, else in name order. A missing `dir` is no layer.
/// ponytail: name order where configmgr takes the directory's order; only a file's dependencies order it there too.
fn load_layer(reg: &mut Registry, dir: &Path, nodes_limit: u32, notes: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => return notes.push(format!("{}: {e}; skipped", dir.display())),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| !e.path().is_dir())
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".xcd")
                .map(String::from)
        })
        .collect();
    names.sort();
    let existing: HashSet<&str> = names.iter().map(String::as_str).collect();
    let mut texts = Vec::new();
    for name in &names {
        let path = dir.join(format!("{name}.xcd"));
        match read_capped(&path, MAX_SIZE) {
            Ok(Some(t)) => texts.push((name.as_str(), path, t)),
            Ok(None) => {}
            Err(why) => notes.push(format!("{}: {why}; skipped", path.display())),
        }
    }
    let mut pending = Vec::new();
    for (name, path, text) in &texts {
        match parse(text, "data", nodes_limit) {
            Ok(doc) => {
                let deps: Vec<(String, bool)> = doc
                    .root_element()
                    .children()
                    .filter(|c| c.has_tag_name("dependency"))
                    .filter_map(|c| {
                        Some((
                            c.attribute("file")?.to_string(),
                            c.attribute("optional") == Some("true"),
                        ))
                    })
                    .collect();
                pending.push((*name, path, doc, deps));
            }
            Err(why) => notes.push(format!("{}: {why}; skipped", path.display())),
        }
    }
    let mut done: HashSet<&str> = HashSet::new();
    loop {
        let before = pending.len();
        let mut i = 0;
        while i < pending.len() {
            let ready = pending[i].3.iter().all(|(d, optional)| {
                done.contains(d.as_str()) || (*optional && !existing.contains(d.as_str()))
            });
            if ready {
                let (name, _, doc, _) = pending.remove(i);
                reg.apply_data(doc.root_element());
                done.insert(name);
            } else {
                i += 1;
            }
        }
        if pending.len() == before {
            break;
        }
    }
    for (_, path, _, deps) in pending {
        let waits: Vec<String> = deps
            .iter()
            .filter(|(d, _)| !done.contains(d.as_str()))
            .map(|(d, _)| format!("{d}.xcd"))
            .collect();
        notes.push(format!(
            "{}: needs {}, which doesn't load (missing, or a dependency cycle); skipped",
            path.display(),
            waits.join(", ")
        ));
    }
}

/// The user layer (`parseModificationLayer`, `components.cxx:871-`): each `Accelerators` item applied at the node its
/// path names, in file order. Returns the UI locale its `/org.openoffice.Setup/L10N` item sets, if any. A missing file
/// is silent; one that doesn't load is a note.
fn apply_user(
    reg: &mut Registry,
    path: &Path,
    nodes_limit: u32,
    notes: &mut Vec<String>,
) -> Option<String> {
    let text = match read_capped(path, MAX_SIZE) {
        Ok(t) => t?,
        Err(why) => {
            notes.push(format!("{}: {why}; listing the defaults", path.display()));
            return None;
        }
    };
    let doc = match parse(&text, "items", nodes_limit) {
        Ok(d) => d,
        Err(why) => {
            notes.push(format!("{}: {why}; listing the defaults", path.display()));
            return None;
        }
    };
    let mut locale = None;
    for item in doc
        .root_element()
        .children()
        .filter(|c| c.has_tag_name("item"))
    {
        let Some(p) = item.attribute((OOR, "path")) else {
            continue;
        };
        if p == "/org.openoffice.Setup/L10N" {
            let value = item
                .children()
                .filter(|c| {
                    c.has_tag_name("prop") && c.attribute((OOR, "name")) == Some("ooLocale")
                })
                .flat_map(|c| c.children().filter(|v| v.has_tag_name("value")))
                .last();
            if let Some(v) = value {
                locale = Some(v.text().unwrap_or("").to_string());
            }
        } else if p == "/org.openoffice.Office.Accelerators"
            || p.starts_with("/org.openoffice.Office.Accelerators/")
        {
            // An item whose path doesn't resolve is configmgr's "unknown item" (`xcuparser.cxx` `handleItem`).
            if let Some(segs) = segments(p) {
                reg.apply_item(&segs, item);
            }
        }
    }
    locale
}

/// An item path's segments (`Data::parseSegment`, `configmgr/source/data.cxx`): `NAME` or `TEMPLATE['NAME']` (or
/// `"NAME"`), the name with `&amp;` `&quot;` `&apos;` decoded. `None` when it isn't one.
fn segments(path: &str) -> Option<Vec<String>> {
    let mut s = path.strip_prefix('/')?;
    let mut out = Vec::new();
    while !s.is_empty() {
        let (seg, tail) = match s.find(['/', '[']) {
            Some(i) if s.as_bytes()[i] == b'[' => {
                let quote = s[i + 1..].chars().next()?;
                if quote != '\'' && quote != '"' {
                    return None;
                }
                let body = &s[i + 2..];
                let end = body.find(quote)?;
                (decode(&body[..end])?, body[end + 1..].strip_prefix(']')?)
            }
            Some(i) => (s[..i].to_string(), &s[i..]),
            None => (s.to_string(), ""),
        };
        out.push(seg);
        s = match tail.strip_prefix('/') {
            Some(t) => t,
            None if tail.is_empty() => "",
            None => return None,
        };
    }
    Some(out)
}

/// `&amp;` `&quot;` `&apos;` decoded; any other `&` makes the segment invalid (`data.cxx` `decode`).
fn decode(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (c, n) = [("amp;", '&'), ("quot;", '"'), ("apos;", '\'')]
            .iter()
            .find(|(e, _)| after.starts_with(e))
            .map(|(e, c)| (*c, e.len()))?;
        out.push(c);
        rest = &after[n..];
    }
    out.push_str(rest);
    Some(out)
}

// --- the tree, as `XcuParser` builds it ----------------------------------------------------------------------------

/// What a node is in the schema: a group (fixed children), a set (of keys or labels, or of such sets) or a set member
/// (a key, or a command, holding one localized prop).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Group,
    Set { of_sets: bool },
    Member,
}

#[derive(Debug)]
struct TNode {
    kind: Kind,
    kids: HashMap<String, TNode>,
    /// A member's localized prop (`Command`, `Label`): locale → text.
    vals: HashMap<String, String>,
}

impl TNode {
    fn new(kind: Kind) -> TNode {
        TNode {
            kind,
            kids: HashMap::new(),
            vals: HashMap::new(),
        }
    }

    fn group(kids: Vec<(&str, TNode)>) -> TNode {
        let mut g = TNode::new(Kind::Group);
        g.kids = kids.into_iter().map(|(n, k)| (n.to_string(), k)).collect();
        g
    }

    /// `el`'s children applied to this node (`XcuParser` `handleSetNode` 811-958, `handleLocalizedGroupProp` 699-745):
    /// a set member by its `oor:op`, a group's existing child modified, the member's `prop` values set. Recursion
    /// follows the schema's few levels, never the file's nesting.
    fn apply(&mut self, el: Node, prop: &str) {
        for c in el.children().filter(Node::is_element) {
            if c.has_tag_name("node") {
                let Some(name) = c.attribute((OOR, "name")) else {
                    continue;
                };
                match self.kind {
                    Kind::Group => {
                        if let Some(k) = self.kids.get_mut(name) {
                            k.apply(c, prop);
                        }
                    }
                    Kind::Set { of_sets } => {
                        let member = if of_sets {
                            Kind::Set { of_sets: false }
                        } else {
                            Kind::Member
                        };
                        match c.attribute((OOR, "op")).unwrap_or("modify") {
                            "replace" => {
                                let mut m = TNode::new(member);
                                m.apply(c, prop);
                                self.kids.insert(name.to_string(), m);
                            }
                            "fuse" => self
                                .kids
                                .entry(name.to_string())
                                .or_insert_with(|| TNode::new(member))
                                .apply(c, prop),
                            "remove" => {
                                self.kids.remove(name);
                            }
                            // `modify` of a member that isn't there is ignored (901-910).
                            _ => {
                                if let Some(k) = self.kids.get_mut(name) {
                                    k.apply(c, prop);
                                }
                            }
                        }
                    }
                    Kind::Member => {}
                }
            } else if c.has_tag_name("prop")
                && self.kind == Kind::Member
                && c.attribute((OOR, "name")) == Some(prop)
            {
                if c.attribute((OOR, "op")) == Some("replace") {
                    self.vals.clear();
                }
                values(&mut self.vals, c);
            }
        }
    }
}

/// `handleLocpropValue` (443-547): each `<value>` sets its `xml:lang` (none: `""`), `xsi:nil` to empty,
/// `oor:op="remove"` deletes it.
fn values(vals: &mut HashMap<String, String>, el: Node) {
    for v in el.children().filter(|v| v.has_tag_name("value")) {
        let lang = v
            .attribute((roxmltree::NS_XML_URI, "lang"))
            .unwrap_or("")
            .to_string();
        if v.attribute((OOR, "op")) == Some("remove") {
            vals.remove(&lang);
        } else if v.attribute((XSI, "nil")) == Some("true") {
            vals.insert(lang, String::new());
        } else {
            vals.insert(lang, v.text().unwrap_or("").to_string());
        }
    }
}

/// The three components read: `Accelerators` (`officecfg/.../Accelerators.xcs`), `GenericCommands` and the module's
/// labels.
struct Registry {
    components: HashMap<String, TNode>,
}

const ACCELERATORS: &str = "org.openoffice.Office.Accelerators";
const GENERIC: &str = "org.openoffice.Office.UI.GenericCommands";

impl Registry {
    fn new(labels: &str) -> Registry {
        let keys = || {
            TNode::group(vec![
                ("Global", TNode::new(Kind::Set { of_sets: false })),
                ("Modules", TNode::new(Kind::Set { of_sets: true })),
            ])
        };
        let commands = || {
            TNode::group(vec![(
                "UserInterface",
                TNode::group(vec![("Commands", TNode::new(Kind::Set { of_sets: false }))]),
            )])
        };
        let mut components = HashMap::new();
        components.insert(
            ACCELERATORS.to_string(),
            TNode::group(vec![("PrimaryKeys", keys()), ("SecondaryKeys", keys())]),
        );
        components.insert(GENERIC.to_string(), commands());
        components.insert(format!("org.openoffice.Office.UI.{labels}"), commands());
        Registry { components }
    }

    /// An `.xcd`'s `oor:component-data` children for the components read, in document order.
    fn apply_data(&mut self, data: Node) {
        for c in data
            .children()
            .filter(|c| c.has_tag_name((OOR, "component-data")))
        {
            let (Some(name), Some(package)) =
                (c.attribute((OOR, "name")), c.attribute((OOR, "package")))
            else {
                continue;
            };
            let full = format!("{package}.{name}");
            let prop = if full == ACCELERATORS {
                "Command"
            } else {
                "Label"
            };
            if let Some(comp) = self.components.get_mut(&full) {
                comp.apply(c, prop);
            }
        }
    }

    /// A user item at `segs` (its first the component): its children applied at that node, or as values when it names
    /// a key's `Command`. Unresolved: skipped.
    fn apply_item(&mut self, segs: &[String], item: Node) {
        let Some((first, rest)) = segs.split_first() else {
            return;
        };
        if first != ACCELERATORS {
            return;
        }
        let Some(mut node) = self.components.get_mut(first) else {
            return;
        };
        for (i, seg) in rest.iter().enumerate() {
            if i + 1 == rest.len() && node.kind == Kind::Member && seg == "Command" {
                return values(&mut node.vals, item);
            }
            match node.kids.get_mut(seg) {
                Some(k) => node = k,
                None => return,
            }
        }
        node.apply(item, "Command");
    }

    fn node(&self, path: &[&str]) -> Option<&TNode> {
        let (first, rest) = path.split_first()?;
        rest.iter()
            .try_fold(self.components.get(*first)?, |n, s| n.kids.get(*s))
    }

    /// `UICommandDescription` (`uicommanddescription.cxx` `getInfoFromCommand` 398-420, `fillInfoFromResult` 254-262):
    /// the module's labels, else the generic ones; the locale, its language, `en-US`, `""`; then the product name, no
    /// trailing dots, no mnemonics. `None`: no label.
    fn label(&self, labels: &str, command: &str, locale: &str) -> Option<String> {
        let language = locale.split('-').next().unwrap_or(locale);
        let module = format!("org.openoffice.Office.UI.{labels}");
        let vals = &[module.as_str(), GENERIC]
            .iter()
            .find_map(|c| self.node(&[c, "UserInterface", "Commands", command]))?
            .vals;
        let text = [locale, language, "en-US", ""]
            .iter()
            .find_map(|l| vals.get(*l))?;
        let text = text.replace("%PRODUCTNAME", "LibreOffice");
        Some(
            erase_mnemonics(text.trim_end_matches('.'))
                .trim_end()
                .to_string(),
        )
        .filter(|t| !t.is_empty())
    }
}

/// `MnemonicGenerator::EraseAllMnemonicChars` (`vcl/source/window/mnemonic.cxx:315-349`): a whole `(~x)` for an
/// ASCII letter `x`, else each `~`.
fn erase_mnemonics(s: &str) -> String {
    let mut c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '~' {
            i += 1;
            continue;
        }
        if i > 0
            && i + 2 < c.len()
            && c[i - 1] == '('
            && c[i + 2] == ')'
            && c[i + 1].is_ascii_alphabetic()
        {
            c.drain(i - 1..i + 3);
            i -= 1;
        } else {
            c.remove(i);
        }
    }
    c.into_iter().collect()
}

// --- the keys -------------------------------------------------------------------------------------------------------

/// Why a key is left out.
#[derive(Debug, PartialEq)]
enum Out {
    Unknown,
    Invalid,
}

/// A key event: LibreOffice's key name (`KEY_` dropped) and modifier bits (SHIFT 1, MOD1 2, MOD2 4, MOD3 8).
type Event = (String, u8);

/// `impl_ts_load` (`acceleratorconfiguration.cxx:941-1047`): `B_SHIFT_MOD1` → `("B", SHIFT|MOD1)`; an unknown key name
/// (`mapIdentifierToCode` throws), an empty or unknown modifier piece, or four of them: left out.
fn event(name: &str) -> Result<Event, Out> {
    let mut pieces = name.split('_');
    let key = pieces.next().unwrap_or("");
    keysym(key).map_err(|_| Out::Unknown)?;
    let mut mods = 0;
    for (k, p) in pieces.enumerate() {
        mods |= match p {
            _ if k == 3 => return Err(Out::Invalid),
            "SHIFT" => 1,
            "MOD1" => 2,
            "MOD2" => 4,
            "MOD3" => 8,
            _ => return Err(Out::Invalid),
        };
    }
    Ok((key.to_string(), mods))
}

/// The X keysym LibreOffice's GTK backend turns into key code `KEY_<key>` (`keymapping.cxx:31-140`, `gtkframe.cxx`
/// `GetKeyCode` 126-262): `Ok(None)` when none does, `Err` for a name `KeyMapping` doesn't know.
fn keysym(key: &str) -> Result<Option<String>, ()> {
    const NAMED: &[(&str, &str)] = &[
        ("DOWN", "Down"),
        ("UP", "Up"),
        ("LEFT", "Left"),
        ("RIGHT", "Right"),
        ("HOME", "Home"),
        ("END", "End"),
        ("PAGEUP", "Page_Up"),
        ("PAGEDOWN", "Page_Down"),
        ("RETURN", "Return"),
        ("ESCAPE", "Escape"),
        ("TAB", "Tab"),
        ("BACKSPACE", "BackSpace"),
        ("SPACE", "space"),
        ("INSERT", "Insert"),
        ("DELETE", "Delete"),
        ("ADD", "KP_Add"),
        ("SUBTRACT", "minus"),
        ("MULTIPLY", "KP_Multiply"),
        ("DIVIDE", "slash"),
        ("POINT", "period"),
        ("COMMA", "comma"),
        ("LESS", "less"),
        ("GREATER", "greater"),
        ("EQUAL", "equal"),
        ("DECIMAL", "KP_Decimal"),
        ("TILDE", "asciitilde"),
        ("QUOTELEFT", "grave"),
        ("QUOTERIGHT", "apostrophe"),
        ("BRACKETLEFT", "bracketleft"),
        ("BRACKETRIGHT", "bracketright"),
        ("SEMICOLON", "semicolon"),
        ("RIGHTCURLYBRACKET", "braceright"),
        ("NUMBERSIGN", "numbersign"),
        ("COLON", "colon"),
        ("FIND", "Find"),
        ("CONTEXTMENU", "Menu"),
        ("HELP", "Help"),
        ("UNDO", "Undo"),
        ("REPEAT", "Redo"),
    ];
    const NONE: &[&str] = &[
        "OPEN",
        "CUT",
        "COPY",
        "PASTE",
        "PROPERTIES",
        "FRONT",
        "MENU",
        "HANGUL_HANJA",
    ];
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_digit() || c.is_ascii_uppercase() {
            return Ok(Some(c.to_ascii_lowercase().to_string()));
        }
    }
    if let Some(n) = key.strip_prefix('F').and_then(|n| n.parse::<u8>().ok()) {
        if (1..=26).contains(&n) && key[1..] == n.to_string() {
            return Ok(Some(key.to_string()));
        }
    }
    if let Some((_, s)) = NAMED.iter().find(|(k, _)| *k == key) {
        return Ok(Some(s.to_string()));
    }
    // `HANGUL_HANJA` holds a `_`, so a key name never reaches here as it: it is split as `HANGUL` + `HANJA` first,
    // and `KEY_HANGUL` is unknown, as it is to LibreOffice.
    if NONE.contains(&key) {
        return Ok(None);
    }
    Err(())
}

/// The left-out counts.
#[derive(Default, Debug, PartialEq)]
struct LeftOut {
    no_keysym: usize,
    unknown: usize,
    invalid: usize,
    capped: bool,
}

/// One set's events and commands: `PrimaryKeys` then `SecondaryKeys`, each in configmgr's order (shorter names first,
/// then by UTF-16 code units: `config_map.hxx`), the command for `locale` exactly, else `en-US`; the first of an event
/// wins (`getCommandByKeyEvent` 500-516: primary before secondary).
fn set_events(
    reg: &Registry,
    set: &[&str],
    locale: &str,
    left: &mut LeftOut,
) -> Vec<(Event, String)> {
    let mut taken = HashSet::new();
    let mut out = Vec::new();
    for group in ["PrimaryKeys", "SecondaryKeys"] {
        let path: Vec<&str> = [ACCELERATORS, group].iter().chain(set).copied().collect();
        let Some(node) = reg.node(&path) else {
            continue;
        };
        let mut names: Vec<&String> = node.kids.keys().collect();
        names.sort_by_key(|n| {
            (
                n.encode_utf16().count(),
                n.encode_utf16().collect::<Vec<_>>(),
            )
        });
        for name in names {
            let vals = &node.kids[name].vals;
            let Some(command) = vals.get(locale).or_else(|| vals.get("en-US")) else {
                continue;
            };
            if command.is_empty() {
                continue;
            }
            match event(name) {
                Ok(ev) => {
                    if taken.insert(ev.clone()) {
                        out.push((ev, command.clone()));
                    }
                }
                Err(Out::Unknown) => left.unknown += 1,
                Err(_) => left.invalid += 1,
            }
        }
    }
    out
}

/// `AcceleratorExecute::impl_ts_findCommand` (`svtools/source/misc/acceleratorexecute.cxx:268-311`): the module's
/// keys, then each global key on an event the module doesn't bind.
/// ponytail: a document's own accelerators aren't read.
/// ponytail: at most about 100 keys × 16 modifier sets are distinct events, so `MAX_ROWS` is a guard, never met.
fn to_sheet(
    reg: &Registry,
    module: &Module,
    locale: &str,
    max_rows: usize,
    notes: &mut Vec<String>,
) -> Sheet {
    let mut left = LeftOut::default();
    let own = set_events(reg, &["Modules", module.id], locale, &mut left);
    let global = set_events(reg, &["Global"], locale, &mut left);
    let bound: HashSet<&Event> = own.iter().map(|(e, _)| e).collect();
    let category = module.title.trim_start_matches("LibreOffice ");
    let rows = own.iter().map(|r| (category, r)).chain(
        global
            .iter()
            .filter(|(e, _)| !bound.contains(e))
            .map(|r| ("LibreOffice", r)),
    );
    let mut sheet = Sheet {
        title: module.title.to_string(),
        provider: "libreoffice".into(),
        binds: Default::default(),
    };
    let mut count = 0;
    for (cat, ((key, mods), command)) in rows {
        let Ok(Some(sym)) = keysym(key) else {
            left.no_keysym += 1;
            continue;
        };
        if count == max_rows {
            left.capped = true;
            break;
        }
        count += 1;
        let names: Vec<&str> = [(1, "shift"), (2, "ctrl"), (4, "alt"), (8, "super")]
            .iter()
            .filter(|(b, _)| mods & b != 0)
            .map(|(_, n)| *n)
            .collect();
        let desc = reg
            .label(module.labels, command, locale)
            .unwrap_or_else(|| command.clone());
        sheet.binds.entry(cat.to_string()).or_default().push(Bind {
            key: chord(&names, &sym),
            desc: plain(&desc),
            action: plain(command),
            when: Default::default(),
            run: Vec::new(),
        });
    }
    let parts: Vec<String> = [
        (
            left.no_keysym,
            "on OPEN, CUT, COPY, PASTE, PROPERTIES, FRONT, MENU or HANGUL_HANJA (no X keysym GTK turns into them)",
        ),
        (left.unknown, "with a key name LibreOffice doesn't know"),
        (left.invalid, "with an invalid name"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, why)| format!("{n} keys {why}"))
    .chain(left.capped.then(|| format!("everything after the first {max_rows} keys")))
    .collect();
    if !parts.is_empty() {
        notes.push(format!("leaving out: {}", parts.join(", ")));
    }
    sheet
}

/// `s` with control and bidirectional-format characters as spaces (they would reach the picker).
fn plain(s: &str) -> String {
    s.chars()
        .map(|c| {
            let one = c.to_string();
            if printable(&one) == one {
                c
            } else {
                ' '
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/libreoffice")
    }

    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-libreoffice-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, text: impl AsRef<[u8]>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn module(class: &str) -> &'static Module {
        MODULES.iter().find(|m| m.class == class).unwrap()
    }

    /// The fixture registry's sheet for `class` with `user` (a file's text) as the user file: (category, key) →
    /// (desc, action), and the notes.
    type Rows = HashMap<(String, String), (String, String)>;

    fn sheet_of(class: &str, user: Option<&str>) -> (Rows, Vec<String>) {
        let user_path = user.map(|u| {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = tmp(&format!("user-{n}")).join("registrymodifications.xcu");
            write(&p, u);
            p
        });
        let mut notes = Vec::new();
        let s = listing(
            &fixtures().join("registry"),
            user_path.as_deref(),
            module(class),
            NODES_LIMIT,
            &mut notes,
        );
        (flat(&s), notes)
    }

    fn flat(s: &Sheet) -> HashMap<(String, String), (String, String)> {
        let mut out = HashMap::new();
        for (cat, binds) in &s.binds {
            for b in binds {
                let dup = out.insert(
                    (cat.clone(), b.key.clone()),
                    (b.desc.clone(), b.action.clone()),
                );
                assert!(dup.is_none(), "{cat} {} twice", b.key);
            }
        }
        out
    }

    fn get<'a>(
        m: &'a HashMap<(String, String), (String, String)>,
        cat: &str,
        key: &str,
    ) -> Option<(&'a str, &'a str)> {
        m.get(&(cat.to_string(), key.to_string()))
            .map(|(d, a)| (d.as_str(), a.as_str()))
    }

    fn counts(m: &HashMap<(String, String), (String, String)>) -> Vec<(String, usize)> {
        let mut c: HashMap<String, usize> = HashMap::new();
        for (cat, _) in m.keys() {
            *c.entry(cat.clone()).or_default() += 1;
        }
        let mut c: Vec<_> = c.into_iter().collect();
        c.sort();
        c
    }

    fn items(items: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="{OOR}" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="{XSI}">
{items}
</oor:items>
"#
        )
    }

    fn locale_item(l: &str) -> String {
        format!(
            r#"<item oor:path="/org.openoffice.Setup/L10N"><prop oor:name="ooLocale" oor:op="fuse"><value>{l}</value></prop></item>"#
        )
    }

    const WRITER: &str = "/org.openoffice.Office.Accelerators/PrimaryKeys/Modules/org.openoffice.Office.Accelerators:Module['com.sun.star.text.TextDocument']";

    #[test]
    fn writer_from_the_installed_registry() {
        let (m, notes) = sheet_of("libreoffice-writer", None);
        assert_eq!(get(&m, "Writer", "ctrl+b"), Some(("Bold", ".uno:Bold")));
        assert_eq!(get(&m, "Writer", "ctrl+i"), Some(("Italic", ".uno:Italic")));
        // WriterCommands' `Table` over GenericCommands' `~Table...`.
        assert_eq!(
            get(&m, "Writer", "ctrl+F12"),
            Some(("Table", ".uno:InsertTable"))
        );
        assert_eq!(
            get(&m, "Writer", "ctrl+KP_Add"),
            Some(("Calculate", ".uno:CalculateSel"))
        );
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+c"),
            Some(("Copy", ".uno:Copy"))
        );
        // Global secondary.
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+Insert"),
            Some(("Copy", ".uno:Copy"))
        );
        // From the second, `unxwnt` fragment (op-less nodes).
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+h"),
            Some(("Find and Replace", ".uno:SearchDialog"))
        );
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+q"),
            Some(("Exit", ".uno:Quit"))
        );
        // Global B_MOD1 has only an empty Command at en-US: Writer's ctrl+b is the only one.
        assert!(get(&m, "LibreOffice", "ctrl+b").is_none());
        // GenericCommands labels the find bar's command `~Find...` (story correction: Testing expected the command).
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+f"),
            Some(("Find", "vnd.sun.star.findbar:FocusToFindbar"))
        );
        // Arguments are part of the command looked up; this one has no label: described by itself.
        let (d, a) = get(&m, "Writer", "ctrl+0").unwrap();
        assert_eq!(d, a);
        assert!(
            a.starts_with(".uno:StyleApply?Style:string=Text body&FamilyName"),
            "{a}"
        );
        assert_eq!(
            counts(&m),
            [("LibreOffice".into(), GLOBAL_ROWS), ("Writer".into(), 142)]
        );
        assert_eq!(notes, [LEFT_OUT]);
    }

    /// Global keys listed for Writer (some of the 37 Global events Writer binds itself).
    const GLOBAL_ROWS: usize = 34;

    #[test]
    fn calc_start_center_and_base() {
        let (m, _) = sheet_of("libreoffice-calc", None);
        assert_eq!(
            get(&m, "Calc", "F2"),
            Some(("Cell Edit Mode", ".uno:SetInputMode"))
        );
        let (d, a) = get(&m, "Calc", "ctrl+KP_Add").unwrap();
        assert_eq!((d, a), ("Insert Cells", ".uno:InsertCell"));
        // No StartModuleCommands: GenericCommands' labels.
        let (m, _) = sheet_of("libreoffice-startcenter", None);
        assert!(m.keys().any(|(c, _)| c == "Start Center"), "{m:?}");
        assert!(
            m.iter()
                .filter(|((c, _), _)| c == "Start Center")
                .all(|(_, (d, a))| d != a),
            "{m:?}"
        );
        assert_eq!(get(&m, "Start Center", "ctrl+o").map(|x| x.0), Some("Open"));
        // Base binds nothing of its own: the global keys.
        let (m, _) = sheet_of("libreoffice-base", None);
        assert!(m.keys().all(|(c, _)| c == "LibreOffice"));
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+c"),
            Some(("Copy", ".uno:Copy"))
        );
        let rows: Vec<_> = MODULES
            .iter()
            .map(|md| {
                let (m, notes) = sheet_of(md.class, None);
                (md.class, m.len(), notes.join(" | "))
            })
            .collect();
        let want: Vec<_> = MODULE_ROWS
            .iter()
            .map(|(c, n, why)| (*c, *n, why.to_string()))
            .collect();
        assert_eq!(rows, want);
    }

    /// The defaults' bare OPEN, CUT, COPY and PASTE keys, once each per sheet (module or Global).
    const LEFT_OUT: &str = "leaving out: 4 keys on OPEN, CUT, COPY, PASTE, PROPERTIES, FRONT, MENU or HANGUL_HANJA (no X keysym GTK turns into them)";

    const MODULE_ROWS: [(&str, usize, &str); 7] = [
        ("libreoffice-writer", 176, LEFT_OUT),
        ("libreoffice-calc", 152, LEFT_OUT),
        ("libreoffice-impress", 110, LEFT_OUT),
        ("libreoffice-draw", 88, LEFT_OUT),
        ("libreoffice-math", 53, LEFT_OUT),
        ("libreoffice-base", 44, LEFT_OUT),
        ("libreoffice-startcenter", 45, LEFT_OUT),
    ];

    #[test]
    fn the_user_file_libreoffice_wrote() {
        let captured =
            std::fs::read_to_string(fixtures().join("registrymodifications.xcu")).unwrap();
        let (m, notes) = sheet_of("libreoffice-writer", Some(&captured));
        assert_eq!(
            get(&m, "Writer", "ctrl+alt+shift+q"),
            Some(("Exit", ".uno:Quit"))
        );
        assert_eq!(
            get(&m, "Writer", "ctrl+i"),
            Some(("Underline", ".uno:Underline"))
        );
        // LibreOffice moved Underline's own ctrl+u to the secondary keys.
        assert_eq!(
            get(&m, "Writer", "ctrl+u"),
            Some(("Underline", ".uno:Underline"))
        );
        assert!(get(&m, "Writer", "ctrl+b").is_none());
        assert!(get(&m, "LibreOffice", "ctrl+b").is_none());
        assert_eq!(
            get(&m, "LibreOffice", "alt+shift+F12"),
            Some(("About LibreOffice", ".uno:About"))
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        // A removed module key lets the global key on its event through, as `impl_ts_findCommand` falls through.
        let global_b = r#"<item oor:path="/org.openoffice.Office.Accelerators/PrimaryKeys/Global"><node oor:name="B_MOD1" oor:op="replace"><prop oor:name="Command" oor:op="fuse"><value xml:lang="en-US">.uno:Open</value></prop></node></item>"#;
        let with = captured.replace("</oor:items>", &format!("{global_b}\n</oor:items>"));
        let (m, _) = sheet_of("libreoffice-writer", Some(&with));
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+b").map(|x| x.1),
            Some(".uno:Open")
        );
    }

    #[test]
    fn the_ui_locale() {
        let de = items(&locale_item("de"));
        let (m, _) = sheet_of("libreoffice-writer", Some(&de));
        assert_eq!(
            get(&m, "Writer", "ctrl+b"),
            Some(("Blocksatz", ".uno:JustifyPara"))
        );
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+c"),
            Some(("Kopieren", ".uno:Copy"))
        );
        // main.xcd's `de` value for Global C_MOD2 (and the language pack's).
        assert_eq!(
            get(&m, "LibreOffice", "alt+c").map(|x| x.1),
            Some(".uno:UnicodeNotationToggle")
        );
        // An empty value for the locale is no key (X_MOD2 at `de`), not the en-US one.
        assert!(get(&m, "LibreOffice", "alt+x").is_none());
        // Commands by the exact locale only; labels fall back to the language.
        let ch = items(&locale_item("de-CH"));
        let (m, _) = sheet_of("libreoffice-writer", Some(&ch));
        assert_eq!(get(&m, "Writer", "ctrl+b"), Some(("Bold", ".uno:Bold")));
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+c"),
            Some(("Kopieren", ".uno:Copy"))
        );
        assert_eq!(
            get(&m, "LibreOffice", "alt+x").map(|x| x.1),
            Some(".uno:UnicodeNotationToggle")
        );
        // Empty: en-US.
        let (m, _) = sheet_of("libreoffice-writer", Some(&items(&locale_item(""))));
        assert_eq!(get(&m, "Writer", "ctrl+b"), Some(("Bold", ".uno:Bold")));
    }

    // --- layering, on synthetic files --------------------------------------------------------------------------------

    fn data(inner: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><oor:data xmlns:oor="{OOR}" xmlns:xsi="{XSI}">{inner}</oor:data>"#
        )
    }

    fn global(keys: &str) -> String {
        format!(
            r#"<oor:component-data oor:name="Accelerators" oor:package="org.openoffice.Office"><node oor:name="PrimaryKeys"><node oor:name="Global">{keys}</node></node></oor:component-data>"#
        )
    }

    fn key(name: &str, op: &str, values: &str) -> String {
        let op = if op.is_empty() {
            String::new()
        } else {
            format!(r#" oor:op="{op}""#)
        };
        format!(r#"<node oor:name="{name}"{op}><prop oor:name="Command">{values}</prop></node>"#)
    }

    fn v(lang: &str, text: &str) -> String {
        format!(r#"<value xml:lang="{lang}">{text}</value>"#)
    }

    /// Files (name, text) in a registry directory loaded with `limit`: the registry and notes.
    fn load(files: &[(&str, &[u8])], limit: u32) -> (Registry, Vec<String>) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let d = tmp(&format!(
            "reg-{}",
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        for (name, text) in files {
            write(&d.join(name), text);
        }
        let mut reg = Registry::new("WriterCommands");
        let mut notes = Vec::new();
        load_layer(&mut reg, &d, limit, &mut notes);
        (reg, notes)
    }

    /// The Global primary keys: (name, en-US command), by name.
    fn globals(reg: &Registry) -> Vec<(String, String)> {
        let node = reg.node(&[ACCELERATORS, "PrimaryKeys", "Global"]).unwrap();
        let mut out: Vec<_> = node
            .kids
            .iter()
            .map(|(k, n)| (k.clone(), n.vals.get("en-US").cloned().unwrap_or_default()))
            .collect();
        out.sort();
        out
    }

    fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn set_operations() {
        let base = data(&global(
            &[
                key("A_MOD1", "replace", &v("en-US", ".uno:A")),
                key(
                    "B_MOD1",
                    "replace",
                    &(v("en-US", ".uno:B") + &v("de", ".uno:Bde")),
                ),
                key("C_MOD1", "replace", &v("en-US", ".uno:C")),
                key("D_MOD1", "replace", &v("en-US", ".uno:D")),
            ]
            .concat(),
        ));
        let over = data(&global(
            &[
                // replace: a new member; its values only.
                key("A_MOD1", "replace", &v("de", ".uno:Ade")),
                // modify: kept values, one changed.
                key("B_MOD1", "", &v("en-US", ".uno:B2")),
                // modify of an unknown member: ignored.
                key("E_MOD1", "", &v("en-US", ".uno:E")),
                // fuse: created when absent.
                key("F_MOD1", "fuse", &v("en-US", ".uno:F")),
                r#"<node oor:name="C_MOD1" oor:op="remove"/><node oor:name="Z_MOD1" oor:op="remove"/>"#.into(),
                // A value removed, and xsi:nil.
                key("D_MOD1", "fuse", r#"<value xml:lang="en-US" oor:op="remove"/><value xml:lang="de" xsi:nil="true"/>"#),
            ]
            .concat(),
        ));
        let (reg, notes) = load(
            &[("a.xcd", base.as_bytes()), ("b.xcd", over.as_bytes())],
            NODES_LIMIT,
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            globals(&reg),
            pairs(&[
                ("A_MOD1", ""),
                ("B_MOD1", ".uno:B2"),
                ("D_MOD1", ""),
                ("F_MOD1", ".uno:F")
            ])
        );
        let g = reg.node(&[ACCELERATORS, "PrimaryKeys", "Global"]).unwrap();
        assert_eq!(g.kids["A_MOD1"].vals["de"], ".uno:Ade");
        assert_eq!(g.kids["B_MOD1"].vals["de"], ".uno:Bde");
        assert_eq!(g.kids["D_MOD1"].vals["de"], "");
        assert!(!g.kids["D_MOD1"].vals.contains_key("en-US"));
        // A prop `oor:op="replace"` empties it first; a value without xml:lang is locale "".
        let rep = data(&global(
            r#"<node oor:name="B_MOD1"><prop oor:name="Command" oor:op="replace"><value>.uno:Plain</value></prop></node>"#,
        ));
        let (reg, _) = load(
            &[
                ("a.xcd", base.as_bytes()),
                ("b.xcd", over.as_bytes()),
                ("c.xcd", rep.as_bytes()),
            ],
            NODES_LIMIT,
        );
        let g = reg.node(&[ACCELERATORS, "PrimaryKeys", "Global"]).unwrap();
        assert_eq!(
            g.kids["B_MOD1"].vals,
            HashMap::from([(String::new(), ".uno:Plain".to_string())])
        );
    }

    #[test]
    fn file_order_by_dependencies() {
        let dep = |f: &str, opt: bool| {
            format!(
                r#"<dependency file="{f}"{}/>"#,
                if opt { r#" optional="true""# } else { "" }
            )
        };
        let k = |cmd: &str| global(&key("A_MOD1", "fuse", &v("en-US", cmd)));
        // `a` names `main` before it: read after it.
        let a = data(&(dep("main", false) + &k(".uno:FromA")));
        let main = data(&k(".uno:FromMain"));
        // An optional dependency that is absent doesn't count.
        let o = data(&(dep("main", false) + &dep("absent", true) + &k(".uno:FromO")));
        let (reg, notes) = load(
            &[
                ("a.xcd", a.as_bytes()),
                ("main.xcd", main.as_bytes()),
                ("o.xcd", o.as_bytes()),
            ],
            NODES_LIMIT,
        );
        assert!(notes.is_empty(), "{notes:?}");
        // configmgr's first pass reads `main` then `o` (its dependency now read); `a` waits for the second.
        assert_eq!(globals(&reg), pairs(&[("A_MOD1", ".uno:FromA")]));
        // A cycle, and a missing dependency: one note each, the files skipped, the rest read.
        let x = data(&(dep("y", false) + &k(".uno:X")));
        let y = data(&(dep("x", false) + &k(".uno:Y")));
        let m = data(&(dep("gone", false) + &k(".uno:M")));
        let (reg, notes) = load(
            &[
                ("main.xcd", main.as_bytes()),
                ("x.xcd", x.as_bytes()),
                ("y.xcd", y.as_bytes()),
                ("m.xcd", m.as_bytes()),
            ],
            NODES_LIMIT,
        );
        assert_eq!(globals(&reg), pairs(&[("A_MOD1", ".uno:FromMain")]));
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes[0].ends_with(
            "m.xcd: needs gone.xcd, which doesn't load (missing, or a dependency cycle); skipped"
        ));
        assert!(notes[1].ends_with(
            "x.xcd: needs y.xcd, which doesn't load (missing, or a dependency cycle); skipped"
        ));
        // A malformed file: one note, the others still read; a directory named .xcd and other names are skipped.
        let (reg, notes) = load(
            &[
                (
                    "bad.xcd",
                    b"<oor:data xmlns:oor=\"http://openoffice.org/2001/registry\"><x></oor:data>",
                ),
                ("main.xcd", main.as_bytes()),
                ("main.xcd.sample", b"junk"),
                ("sub.xcd/z.xcd", b"junk"),
            ],
            NODES_LIMIT,
        );
        assert_eq!(globals(&reg), pairs(&[("A_MOD1", ".uno:FromMain")]));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("bad.xcd: ") && notes[0].contains("1:"),
            "{notes:?}"
        );
    }

    #[test]
    fn xml_through_roxmltree() {
        // Another prefix for the registry namespace reads the same.
        let r = format!(
            r#"<r:data xmlns:r="{OOR}"><r:component-data r:name="Accelerators" r:package="org.openoffice.Office"><node r:name="PrimaryKeys"><node r:name="Global"><node r:name="A_MOD1" r:op="replace"><prop r:name="Command"><value xml:lang="en-US">.uno:R</value></prop></node></node></node></r:component-data></r:data>"#
        );
        // An un-namespaced `name` isn't `oor:name`.
        let plain = data(&global(
            r#"<node name="B_MOD1" oor:op="replace"><prop oor:name="Command"><value xml:lang="en-US">.uno:B</value></prop></node>"#,
        ));
        let (reg, notes) = load(
            &[("p.xcd", plain.as_bytes()), ("r.xcd", r.as_bytes())],
            NODES_LIMIT,
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(globals(&reg), pairs(&[("A_MOD1", ".uno:R")]));
        // A DOCTYPE, too many nodes, not UTF-8, a mismatched tag, a root that isn't oor:data, deep nesting: a note
        // each, the file skipped.
        let doctype = format!("<!DOCTYPE oor:data SYSTEM \"x.dtd\">{}", data(""));
        let deep = data(&global(&format!(
            r#"<node oor:name="A_MOD1" oor:op="replace">{}{}</node>"#,
            "<x>".repeat(70),
            "</x>".repeat(70)
        )));
        let many = data(&global(
            &key("A_MOD1", "replace", &v("en-US", ".uno:A")).repeat(40),
        ));
        let (reg, notes) = load(
            &[
                ("a.xcd", doctype.as_bytes()),
                ("b.xcd", b"\xff\xfe"),
                ("c.xcd", b"<oor:data xmlns:oor=\"http://openoffice.org/2001/registry\">\n  <a></b></oor:data>"),
                ("d.xcd", b"<data/>"),
                ("e.xcd", deep.as_bytes()),
                ("f.xcd", many.as_bytes()),
            ],
            100,
        );
        assert!(globals(&reg).is_empty(), "{:?} {notes:?}", globals(&reg));
        assert_eq!(notes.len(), 6, "{notes:?}");
        // Read errors first, then parse errors, each in name order.
        assert!(
            notes[0].ends_with("b.xcd: not UTF-8; skipped"),
            "{}",
            notes[0]
        );
        assert!(
            notes[1].ends_with("a.xcd: XML with DTD detected; skipped"),
            "{}",
            notes[1]
        );
        assert!(
            notes[2].ends_with("c.xcd: expected 'a' tag, not 'b' at 2:6; skipped"),
            "{}",
            notes[2]
        );
        assert!(
            notes[3].ends_with(
                "d.xcd: its root element <data> isn't the registry's <oor:data>; skipped"
            ),
            "{}",
            notes[3]
        );
        assert!(
            notes[4].ends_with("e.xcd: its elements nest deeper than 64; skipped"),
            "{}",
            notes[4]
        );
        assert!(
            notes[5].contains("f.xcd: ") && notes[5].contains("nodes limit"),
            "{}",
            notes[5]
        );
    }

    // --- the user file -----------------------------------------------------------------------------------------------

    #[test]
    fn user_items() {
        let module = |name: &str, keys: &str| {
            format!(
                r#"<item oor:path="/org.openoffice.Office.Accelerators/PrimaryKeys/Modules"><node oor:name="{name}" oor:op="replace">{keys}</node></item>"#
            )
        };
        let odd = "it's & \"odd\"";
        let user = items(
            &[
                module("it's &amp; &quot;odd&quot;", &key("A_MOD1", "replace", &v("en-US", ".uno:Odd"))),
                // The same module by an escaped path, a key added.
                format!(
                    r#"<item oor:path="/org.openoffice.Office.Accelerators/PrimaryKeys/Modules/org.openoffice.Office.Accelerators:Module['it&amp;apos;s &amp;amp; &amp;quot;odd&amp;quot;']">{}</item>"#,
                    key("B_MOD1", "replace", &v("en-US", ".uno:OddB"))
                ),
                // Unknown paths and other components: skipped.
                format!(r#"<item oor:path="{WRITER}/Nope['x']">{}</item>"#, key("C_MOD1", "replace", &v("en-US", ".uno:No"))),
                r#"<item oor:path="/org.openoffice.Office.UI.GenericCommands/UserInterface/Commands"><node oor:name=".uno:Bold"><prop oor:name="Label"><value xml:lang="en-US">Fat</value></prop></node></item>"#.to_string(),
                format!(r#"<item oor:path="{WRITER}/Key['I_MOD1']/Command"><value xml:lang="en-US">.uno:Underline</value></item>"#),
            ]
            .concat(),
        );
        let p = tmp("items").join("registrymodifications.xcu");
        write(&p, &user);
        let mut reg = Registry::new("WriterCommands");
        let mut notes = Vec::new();
        load_layer(
            &mut reg,
            &fixtures().join("registry"),
            NODES_LIMIT,
            &mut notes,
        );
        assert_eq!(apply_user(&mut reg, &p, NODES_LIMIT, &mut notes), None);
        assert!(notes.is_empty(), "{notes:?}");
        let odd_keys = reg
            .node(&[ACCELERATORS, "PrimaryKeys", "Modules", odd])
            .unwrap();
        let mut got: Vec<_> = odd_keys
            .kids
            .iter()
            .map(|(k, n)| (k.clone(), n.vals["en-US"].clone()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            pairs(&[("A_MOD1", ".uno:Odd"), ("B_MOD1", ".uno:OddB")])
        );
        let writer = |k: &str| {
            reg.node(&[
                ACCELERATORS,
                "PrimaryKeys",
                "Modules",
                "com.sun.star.text.TextDocument",
                k,
            ])
            .map(|n| n.vals["en-US"].clone())
        };
        assert_eq!(writer("I_MOD1").as_deref(), Some(".uno:Underline"));
        assert_eq!(
            reg.label("WriterCommands", ".uno:Bold", "en-US").as_deref(),
            Some("Bold")
        );
        // A file that doesn't load: a note, the defaults; a missing one: nothing.
        let (m, notes) = sheet_of("libreoffice-writer", Some("<oor:items"));
        assert_eq!(get(&m, "Writer", "ctrl+b").map(|x| x.1), Some(".uno:Bold"));
        assert!(
            notes
                .iter()
                .any(|n| n.contains("registrymodifications.xcu: ")
                    && n.ends_with("; listing the defaults")),
            "{notes:?}"
        );
        let mut notes = Vec::new();
        assert_eq!(
            apply_user(
                &mut reg,
                &tmp("none").join("x.xcu"),
                NODES_LIMIT,
                &mut notes
            ),
            None
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn item_paths() {
        assert_eq!(
            segments("/a/T['x/y']/U[\"q&apos;&amp;&quot;\"]/b"),
            Some(vec!["a".into(), "x/y".into(), "q'&\"".into(), "b".into()])
        );
        assert_eq!(segments("/a"), Some(vec!["a".into()]));
        assert_eq!(segments("a"), None);
        assert_eq!(segments("/T['x"), None);
        assert_eq!(segments("/T[x]"), None);
        assert_eq!(segments("/T['x']y"), None);
        assert_eq!(segments("/T['&lt;']"), None);
    }

    #[test]
    fn where_the_user_file_is() {
        let d = tmp("xdg");
        let env = |vars: &'static [(&'static str, &str)]| {
            let vars: Vec<(String, String)> = vars
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            user_file(&move |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()))
        };
        let xdg = d.to_str().unwrap().to_string();
        let leaked: &'static str = Box::leak(xdg.into_boxed_str());
        assert_eq!(
            env(Box::leak(Box::new([
                ("XDG_CONFIG_HOME", leaked),
                ("HOME", "/h")
            ]))),
            Some(d.join("libreoffice/4/user/registrymodifications.xcu"))
        );
        let home = Some(PathBuf::from(
            "/h/.config/libreoffice/4/user/registrymodifications.xcu",
        ));
        assert_eq!(
            env(&[("XDG_CONFIG_HOME", "/no/such/dir"), ("HOME", "/h")]),
            home
        );
        assert_eq!(env(&[("XDG_CONFIG_HOME", ""), ("HOME", "/h")]), home);
        assert_eq!(env(&[]), None);
    }

    // --- keys and labels ---------------------------------------------------------------------------------------------

    #[test]
    fn keys_as_gtk_sends_them() {
        let rows = [
            ("0", "0"),
            ("9", "9"),
            ("A", "a"),
            ("Z", "z"),
            ("F1", "F1"),
            ("F26", "F26"),
            ("DOWN", "Down"),
            ("UP", "Up"),
            ("LEFT", "Left"),
            ("RIGHT", "Right"),
            ("HOME", "Home"),
            ("END", "End"),
            ("PAGEUP", "Page_Up"),
            ("PAGEDOWN", "Page_Down"),
            ("RETURN", "Return"),
            ("ESCAPE", "Escape"),
            ("TAB", "Tab"),
            ("BACKSPACE", "BackSpace"),
            ("SPACE", "space"),
            ("INSERT", "Insert"),
            ("DELETE", "Delete"),
            ("ADD", "KP_Add"),
            ("SUBTRACT", "minus"),
            ("MULTIPLY", "KP_Multiply"),
            ("DIVIDE", "slash"),
            ("POINT", "period"),
            ("COMMA", "comma"),
            ("LESS", "less"),
            ("GREATER", "greater"),
            ("EQUAL", "equal"),
            ("DECIMAL", "KP_Decimal"),
            ("TILDE", "asciitilde"),
            ("QUOTELEFT", "grave"),
            ("QUOTERIGHT", "apostrophe"),
            ("BRACKETLEFT", "bracketleft"),
            ("BRACKETRIGHT", "bracketright"),
            ("SEMICOLON", "semicolon"),
            ("RIGHTCURLYBRACKET", "braceright"),
            ("NUMBERSIGN", "numbersign"),
            ("COLON", "colon"),
            ("FIND", "Find"),
            ("CONTEXTMENU", "Menu"),
            ("HELP", "Help"),
            ("UNDO", "Undo"),
            ("REPEAT", "Redo"),
        ];
        for (code, sym) in rows {
            assert_eq!(keysym(code), Ok(Some(sym.to_string())), "{code}");
            assert!(super::super::openbox::is_keysym(sym), "{sym}");
        }
        for code in [
            "OPEN",
            "CUT",
            "COPY",
            "PASTE",
            "PROPERTIES",
            "FRONT",
            "MENU",
            "HANGUL_HANJA",
        ] {
            assert_eq!(keysym(code), Ok(None), "{code}");
        }
        for code in ["F0", "F27", "F01", "a", "ZZZ", "", "AA", "KEY_A"] {
            assert_eq!(keysym(code), Err(()), "{code}");
        }
        assert_eq!(event("1_SHIFT_MOD1"), Ok(("1".into(), 3)));
        assert_eq!(event("A_MOD3"), Ok(("A".into(), 8)));
        assert_eq!(event("A_MOD1_SHIFT_MOD2"), Ok(("A".into(), 7)));
        assert_eq!(event("A_FOO"), Err(Out::Invalid));
        assert_eq!(event("A__MOD1"), Err(Out::Invalid));
        assert_eq!(event("A_"), Err(Out::Invalid));
        assert_eq!(event("A_SHIFT_MOD1_MOD2_MOD3"), Err(Out::Invalid));
        assert_eq!(event("ZZZ_MOD1"), Err(Out::Unknown));
        assert_eq!(event("HANGUL_HANJA"), Err(Out::Unknown));
        assert_eq!(event("COPY_MOD1"), Ok(("COPY".into(), 2)));
    }

    #[test]
    fn events_counts_and_order() {
        let keys = [
            key("A_SHIFT_MOD1", "replace", &v("en-US", ".uno:Long")),
            key("A_MOD1_SHIFT", "replace", &v("en-US", ".uno:LongToo")),
            key("A_MOD1_MOD2", "replace", &v("en-US", ".uno:Third")),
            key("A_MOD2_MOD1", "replace", &v("en-US", ".uno:Fourth")),
            key("A_FOO", "replace", &v("en-US", ".uno:Bad")),
            key("ZZZ_MOD1", "replace", &v("en-US", ".uno:Unknown")),
            key("COPY_MOD1", "replace", &v("en-US", ".uno:Copy")),
            key("MOD3_TEST", "replace", ""),
            key("Q_MOD3", "replace", &v("en-US", ".uno:Super")),
        ]
        .concat();
        let secondary = r#"<node oor:name="SecondaryKeys"><node oor:name="Global"><node oor:name="Q_MOD3" oor:op="replace"><prop oor:name="Command"><value xml:lang="en-US">.uno:Second</value></prop></node><node oor:name="W_MOD3" oor:op="replace"><prop oor:name="Command"><value xml:lang="en-US">.uno:W</value></prop></node></node><node oor:name="Modules"><node oor:name="com.sun.star.text.TextDocument" oor:op="replace"><node oor:name="W_MOD3" oor:op="replace"><prop oor:name="Command"><value xml:lang="en-US">.uno:ModW</value></prop></node></node></node></node>"#;
        let text = data(&global(&keys).replace(
            "</oor:component-data>",
            &format!("{secondary}</oor:component-data>"),
        ));
        let (reg, notes) = load(&[("a.xcd", text.as_bytes())], NODES_LIMIT);
        assert!(notes.is_empty(), "{notes:?}");
        let mut notes = Vec::new();
        let s = to_sheet(
            &reg,
            module("libreoffice-writer"),
            "en-US",
            MAX_ROWS,
            &mut notes,
        );
        let m = flat(&s);
        // Equal length: by code units (`A_MOD1_SHIFT` < `A_SHIFT_MOD1`); shorter first (`A_MOD1_MOD2` before both).
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+shift+a").map(|x| x.1),
            Some(".uno:LongToo")
        );
        assert_eq!(
            get(&m, "LibreOffice", "ctrl+alt+a").map(|x| x.1),
            Some(".uno:Third")
        );
        // Primary over secondary; module over global.
        assert_eq!(
            get(&m, "LibreOffice", "super+q").map(|x| x.1),
            Some(".uno:Super")
        );
        assert_eq!(get(&m, "Writer", "super+w").map(|x| x.1), Some(".uno:ModW"));
        assert!(get(&m, "LibreOffice", "super+w").is_none());
        assert_eq!(m.len(), 4, "{m:?}");
        assert_eq!(
            notes,
            ["leaving out: 1 keys on OPEN, CUT, COPY, PASTE, PROPERTIES, FRONT, MENU or HANGUL_HANJA (no X keysym GTK turns into them), 1 keys with a key name LibreOffice doesn't know, 1 keys with an invalid name"]
        );
    }

    #[test]
    fn the_row_cap() {
        let keys: String = ["A", "B", "C"]
            .iter()
            .map(|k| key(&format!("{k}_MOD1"), "replace", &v("en-US", ".uno:X")))
            .collect();
        let (reg, _) = load(&[("a.xcd", data(&global(&keys)).as_bytes())], NODES_LIMIT);
        let mut notes = Vec::new();
        let s = to_sheet(&reg, module("libreoffice-writer"), "en-US", 2, &mut notes);
        assert_eq!(flat(&s).len(), 2);
        assert_eq!(notes, ["leaving out: everything after the first 2 keys"]);
        let mut notes = Vec::new();
        assert_eq!(
            flat(&to_sheet(
                &reg,
                module("libreoffice-writer"),
                "en-US",
                3,
                &mut notes
            ))
            .len(),
            3
        );
        assert!(notes.is_empty());
    }

    #[test]
    fn labels() {
        assert_eq!(erase_mnemonics("Cop~y"), "Copy");
        assert_eq!(erase_mnemonics("~Options"), "Options");
        assert_eq!(erase_mnemonics("Fichier (~F)"), "Fichier ");
        assert_eq!(erase_mnemonics("Zoom (~1)"), "Zoom (1)");
        assert_eq!(erase_mnemonics("~"), "");
        let text = data(
            r#"<oor:component-data oor:name="GenericCommands" oor:package="org.openoffice.Office.UI"><node oor:name="UserInterface"><node oor:name="Commands"><node oor:name=".uno:Options" oor:op="replace"><prop oor:name="Label"><value xml:lang="en-US">~Options...</value></prop></node><node oor:name=".uno:About" oor:op="replace"><prop oor:name="Label"><value xml:lang="en-US">~About %PRODUCTNAME</value></prop></node><node oor:name=".uno:F" oor:op="replace"><prop oor:name="Label"><value xml:lang="en-US">File</value><value xml:lang="fr">Fichier (~F)</value></prop></node><node oor:name=".uno:L" oor:op="replace"><prop oor:name="Label"><value xml:lang="de">Deutsch</value><value xml:lang="de-CH">Schwiiz</value><value xml:lang="">Root</value><value xml:lang="en-US">English</value></prop></node><node oor:name=".uno:Root" oor:op="replace"><prop oor:name="Label"><value xml:lang="">Root</value></prop></node><node oor:name=".uno:Empty" oor:op="replace"><prop oor:name="Label"><value xml:lang="en-US"></value></prop></node><node oor:name=".uno:Both" oor:op="replace"><prop oor:name="Label"><value xml:lang="en-US">Generic</value></prop></node></node></node></oor:component-data><oor:component-data oor:name="WriterCommands" oor:package="org.openoffice.Office.UI"><node oor:name="UserInterface"><node oor:name="Commands"><node oor:name=".uno:Both" oor:op="replace"><prop oor:name="Label"><value xml:lang="en-US">Writer's</value></prop></node></node></node></oor:component-data>"#,
        );
        let (reg, notes) = load(&[("a.xcd", text.as_bytes())], NODES_LIMIT);
        assert!(notes.is_empty(), "{notes:?}");
        let l = |c: &str, loc: &str| reg.label("WriterCommands", c, loc);
        assert_eq!(l(".uno:Options", "en-US").as_deref(), Some("Options"));
        assert_eq!(
            l(".uno:About", "en-US").as_deref(),
            Some("About LibreOffice")
        );
        assert_eq!(l(".uno:F", "fr").as_deref(), Some("Fichier"));
        assert_eq!(l(".uno:F", "fr-CA").as_deref(), Some("Fichier"));
        assert_eq!(l(".uno:L", "de-CH").as_deref(), Some("Schwiiz"));
        assert_eq!(l(".uno:L", "de-AT").as_deref(), Some("Deutsch"));
        assert_eq!(l(".uno:L", "fr").as_deref(), Some("English"));
        assert_eq!(l(".uno:Root", "fr").as_deref(), Some("Root"));
        assert_eq!(l(".uno:Empty", "en-US"), None);
        assert_eq!(l(".uno:None", "en-US"), None);
        assert_eq!(l(".uno:Both", "en-US").as_deref(), Some("Writer's"));
        assert_eq!(
            reg.label("CalcCommands", ".uno:Both", "en-US").as_deref(),
            Some("Generic")
        );
        assert_eq!(plain("a\tb\u{202e}c\u{1b}"), "a b c ");
    }

    // --- where LibreOffice is ----------------------------------------------------------------------------------------

    #[test]
    fn the_install_root() {
        let d = tmp("root");
        write(&d.join("opt/lo/program/soffice"), "");
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::os::unix::fs::symlink("../opt/lo/program/soffice", d.join("bin/soffice")).unwrap();
        // Not under program/: skipped.
        write(&d.join("other/soffice"), "");
        write(&d.join("usr/lib/libreoffice/program/soffice"), "");
        let fallback = d.join("usr/lib/libreoffice");
        let roots = [fallback.to_str().unwrap()];
        let path = |p: String| move |k: &str| (k == "PATH").then(|| p.clone());
        let p = format!(":{}:{}", d.join("other").display(), d.join("bin").display());
        assert_eq!(
            install_root(&path(p), &roots).unwrap(),
            d.join("opt/lo").canonicalize().unwrap()
        );
        let p = format!("{}:", d.join("other").display());
        assert_eq!(install_root(&path(p.clone()), &roots).unwrap(), fallback);
        let err = install_root(&path(p), &["/no/a", "/no/b"])
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "no LibreOffice found: no soffice on PATH under a program/ directory, and no program/soffice in /no/a or /no/b"
        );
        // The whole read fails then; with an install, a class that isn't LibreOffice's lists Writer, with a note.
        assert!(read(&|_| None, Some("libreoffice-writer"), &["/no/a"]).is_err());
        let fake = tmp("fake");
        write(&fake.join("program/soffice"), "");
        std::fs::create_dir_all(fake.join("share")).unwrap();
        std::os::unix::fs::symlink(fixtures().join("registry"), fake.join("share/registry"))
            .unwrap();
        let roots = [fake.to_str().unwrap()];
        let (s, notes) = read(&|_| None, Some("XTerm"), &roots).unwrap();
        assert_eq!(s.title, "LibreOffice Writer");
        assert_eq!(s.provider, "libreoffice");
        assert_eq!(
            notes[0],
            "the window isn't a LibreOffice module's (class \"XTerm\"): listing Writer's keys"
        );
        let (s, notes) = read(&|_| None, Some("LIBREOFFICE-CALC"), &roots).unwrap();
        assert_eq!(s.title, "LibreOffice Calc");
        assert!(!notes.iter().any(|n| n.contains("isn't")), "{notes:?}");
    }

    #[test]
    fn classes() {
        for m in MODULES {
            assert!(is_libreoffice(m.class) && is_libreoffice(&m.class.to_uppercase()));
        }
        for c in [
            "libreoffice-x",
            "LibreOffice",
            "soffice",
            "libreoffice-writer2",
        ] {
            assert!(!is_libreoffice(c), "{c}");
        }
    }
}
