#![doc = include_str!("../README.md")]

use anyhow::{bail, Context as _};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

/// The protocol version this crate speaks, in `Request::version` and `Response::version`.
pub const VERSION: u32 = 1;

/// A cheatsheet in DMS's shape: `{"title", "provider", "binds": {category: [bind, ...]}}`. Unknown fields are ignored
/// when reading.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Sheet {
    /// `"title"`: shown to people, e.g. `"nvim (lazy.nvim)"`.
    pub title: String,
    /// `"provider"`: the short name a pick is logged and ranked under, e.g. `"nvim"`.
    pub provider: String,
    /// `"binds"`: category name → its bindings, in the order given. Categories come out sorted by name.
    pub binds: BTreeMap<String, Vec<Bind>>,
}

/// One binding: `{"key", "desc", "action"}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Bind {
    /// `"key"`: in the key notation (see `Key`), e.g. `"ctrl+b c"`. A string, since built-in sheets also hold text
    /// that is typed rather than pressed (espanso's `:date`); a plugin's keys are parsed as `Key` before printing.
    pub key: String,
    /// `"desc"`: what it does, as people search for it.
    pub desc: String,
    /// `"action"`: the program's own name for it; may be `""`. Hand-written DMS sheets may leave it out.
    #[serde(default)]
    pub action: String,
    /// `"when"`: where the binding applies, as scope → the values it applies in, e.g. `{"mode": "insert",
    /// "filetype": ["rust", "toml"]}`. Each is a `Name`; a scope's values are one name or a non-empty list of them
    /// (`Values`). Empty or left out means everywhere (and is left out when written). Report every binding with its
    /// scope and never filter by guessing: keybind-find hides a binding only when a scope contradicts a fact it knows
    /// (`Context::facts`), and shows the rest as tags.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "no_repeated_scopes"
    )]
    pub when: BTreeMap<Name, Values>,
}

/// `when` with each scope once: JSON allows a repeated object key, and a map would keep the last silently.
fn no_repeated_scopes<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<Name, Values>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<Name, Values>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object of scopes")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut when = BTreeMap::new();
            while let Some((scope, values)) = map.next_entry::<Name, Values>()? {
                if when.contains_key(&scope) {
                    return Err(serde::de::Error::custom(format!(
                        "scope {:?} is given twice",
                        scope.as_str()
                    )));
                }
                when.insert(scope, values);
            }
            Ok(when)
        }
    }
    d.deserialize_map(Visitor)
}

/// A scope name or value in `Bind::when` and `Context::facts` (`mode`, `insert`, `rust`): printable ASCII with no
/// spaces, `,`, `=`, `[` or `]`, so a tag (`[filetype=rust,toml]`) reads only one way (no invisible or
/// direction-changing characters either).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(try_from = "String", into = "String")]
pub struct Name(String);

impl Name {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for Name {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Name> {
        Name::try_from(s.to_string())
    }
}

impl TryFrom<String> for Name {
    type Error = anyhow::Error;
    fn try_from(s: String) -> anyhow::Result<Name> {
        if s.is_empty() {
            bail!("a scope name or value can't be empty");
        }
        if let Some(c) = s
            .chars()
            .find(|&c| !c.is_ascii_graphic() || ",=[]".contains(c))
        {
            bail!("{s:?} is not a scope name or value: it has {c:?} (printable ASCII; no spaces, `,`, `=`, `[` or `]`)");
        }
        Ok(Name(s))
    }
}

impl From<Name> for String {
    fn from(n: Name) -> String {
        n.0
    }
}

/// So a map keyed by `Name` can be looked up by `&str`. `Name`'s order is its string's.
impl std::borrow::Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// The values a scope applies in: at least one, each once, in the order given. Written as a string when there is
/// one, else as a list (`"insert"`, `["rust", "toml"]`); read from either, so only from a format that says which it
/// is (JSON does; bincode can't).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Values(Vec<Name>);

impl Values {
    pub fn iter(&self) -> impl Iterator<Item = &Name> {
        self.0.iter()
    }

    pub fn contains(&self, value: &str) -> bool {
        self.0.iter().any(|n| n.as_str() == value)
    }
}

impl From<Name> for Values {
    fn from(n: Name) -> Values {
        Values(vec![n])
    }
}

impl TryFrom<Vec<Name>> for Values {
    type Error = anyhow::Error;
    fn try_from(names: Vec<Name>) -> anyhow::Result<Values> {
        if names.is_empty() {
            bail!("an empty list of scope values would mean nowhere: give at least one, or leave the scope out");
        }
        if let Some((i, n)) = names
            .iter()
            .enumerate()
            .find(|(i, n)| names[..*i].contains(n))
        {
            bail!("scope value {:?} is repeated (at {i})", n.as_str());
        }
        Ok(Values(names))
    }
}

impl Serialize for Values {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => one.serialize(s),
            many => many.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Values {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Values, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Values;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a scope value or a non-empty list of them")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Values, E> {
                v.parse::<Name>().map(Values::from).map_err(E::custom)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Values, A::Error> {
                let mut names = Vec::new();
                while let Some(n) = seq.next_element::<Name>()? {
                    names.push(n);
                }
                Values::try_from(names).map_err(serde::de::Error::custom)
            }
        }
        d.deserialize_any(Visitor)
    }
}

/// What keybind-find sends a plugin on stdin: `{"version": 1, "context": {...}, "inputs": [{"path", "text"}, ...]}`.
/// Protocol rule: `context` is an object and `inputs` a list (maybe empty); a missing one reads as empty, a `null`
/// one is an invalid request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Request {
    /// `"version"`: the protocol version, `1`.
    pub version: u32,
    /// `"context"`: the focused window.
    #[serde(default)]
    pub context: Context,
    /// `"inputs"`: the files the plugin's stanza names, already read, in stanza order. Empty for a plugin that reads
    /// none (one that asks a running program instead).
    #[serde(default)]
    pub inputs: Vec<Input>,
}

/// `"context"` in a `Request`. Each field is `null` when keybind-find couldn't tell.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Context {
    /// `"wm"`: the window manager, e.g. `"xmonad"`, `"sway"`.
    #[serde(default)]
    pub wm: Option<String>,
    /// `"class"`: the focused window's class (X `WM_CLASS`, or sway's `app_id`).
    #[serde(default)]
    pub class: Option<String>,
    /// `"title"`: the focused window's title.
    #[serde(default)]
    pub title: Option<String>,
    /// `"program"`: the program in the focused terminal, e.g. `"nvim"`.
    #[serde(default)]
    pub program: Option<String>,
    /// `"facts"`: what keybind-find knows about where the user is, as scope → value (`{"mode": "normal"}`), the same
    /// names as `Bind::when`. An object: missing reads as empty (it knows nothing; nothing is filled in yet), `null`
    /// is invalid, as for the request's `context` and `inputs`.
    #[serde(default)]
    pub facts: BTreeMap<Name, Name>,
}

/// One entry of `"inputs"` in a `Request`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Input {
    /// `"path"`: the file keybind-find read.
    pub path: String,
    /// `"text"`: its contents.
    pub text: String,
}

/// What a plugin prints on stdout: a sheet with `"version": 1` beside its fields,
/// `{"version": 1, "title", "provider", "binds"}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Response {
    /// `"version"`: the protocol version, `1`.
    pub version: u32,
    /// The sheet's own fields (`"title"`, `"provider"`, `"binds"`), at the top level beside `"version"`.
    #[serde(flatten)]
    pub sheet: Sheet,
}

/// What a plugin prints for `--manifest`: who it is and the defaults `keybind-find plugin add` writes into a
/// `[[plugin]]` stanza, `{"version": 1, "name", "description", "inputs", "match", "queries", "homepage"}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Manifest {
    /// `"version"`: the protocol version, `1`.
    pub version: u32,
    /// `"name"`: the stanza's `name`, e.g. `"helix"`.
    pub name: String,
    /// `"description"`: one line on what it reads.
    #[serde(default)]
    pub description: String,
    /// `"inputs"`: the files it reads by default, each starting with `/` or `~/` (`*` in the last component
    /// allowed), e.g. `["~/.config/helix/config.toml"]`.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// `"match"`: when it applies by default. Without one, a stanza must give its own `match` or it never runs.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub matches: Option<Match>,
    /// `"queries"`: programs it asks while running instead of reading files (e.g. `["nvim"]`), for the docs.
    #[serde(default)]
    pub queries: Vec<String>,
    /// `"homepage"`: where it lives, if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

/// `"match"` in a `Manifest`: each list a window must match one of (ignoring case), as in a `[[plugin]]` stanza's
/// `match`; `window_title` is a part of the title. A list left out doesn't constrain.
/// An unknown key is an error, as in a stanza: a misspelled one would otherwise widen the match silently.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Match {
    /// `"class"`: window classes, e.g. `["Alacritty", "kitty"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Vec<String>>,
    /// `"program"`: programs in the terminal's foreground, e.g. `["hx"]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<Vec<String>>,
    /// `"window_title"`: parts of the window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<Vec<String>>,
}

const MOD_ORDER: [&str; 4] = ["super", "ctrl", "alt", "shift"];

/// One chord in the key notation, from strings, as keybind-find's own providers build them (plugins use `Chord`):
/// modifiers in the order super, ctrl, alt, shift, then the X keysym name. Other names keep their input order after
/// those (so `meta` and the rest may differ from `Key`'s order); a repeated modifier appears once.
/// The keysym must be a non-empty X keysym name (`plus`, never `+`), since `+` separates the parts.
pub fn chord(mods: &[&str], keysym: &str) -> String {
    let mut sorted = mods.to_vec();
    sorted.sort_by_key(|m| {
        MOD_ORDER
            .iter()
            .position(|o| o == m)
            .unwrap_or(MOD_ORDER.len())
    });
    // Not `dedup`: unknown modifiers share a sort key, so a repeat of one needn't be next to it.
    let mut seen = Vec::new();
    sorted.retain(|m| {
        let first = !seen.contains(m);
        seen.push(*m);
        first
    });
    sorted.push(keysym);
    sorted.join("+")
}

/// A modifier in the key notation. This is the whole set: any other name is a parse error. Written in lower case, in
/// this order (`Ord`) before the keysym. `win` and X's `mod4` read as `super`, `option` and `mod1` as `alt`; `fn` is the Fn key of
/// keyboards that report it; `mod2`, `mod3` and `mod5` are X's (xmonad binds them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    Super,
    Ctrl,
    Alt,
    Shift,
    Meta,
    Fn,
    Mod2,
    Mod3,
    Mod5,
}

impl Modifier {
    /// Every modifier, in order (the assertion below fails to compile if one is missing).
    pub const ALL: [Modifier; 9] = [
        Modifier::Super,
        Modifier::Ctrl,
        Modifier::Alt,
        Modifier::Shift,
        Modifier::Meta,
        Modifier::Fn,
        Modifier::Mod2,
        Modifier::Mod3,
        Modifier::Mod5,
    ];

    /// Its name in the notation.
    pub fn name(self) -> &'static str {
        match self {
            Modifier::Super => "super",
            Modifier::Ctrl => "ctrl",
            Modifier::Alt => "alt",
            Modifier::Shift => "shift",
            Modifier::Meta => "meta",
            Modifier::Fn => "fn",
            Modifier::Mod2 => "mod2",
            Modifier::Mod3 => "mod3",
            Modifier::Mod5 => "mod5",
        }
    }

    /// A name or alias, exactly as written (lower case).
    fn named(s: &str) -> Option<Modifier> {
        match s {
            "win" | "mod4" => Some(Modifier::Super),
            "option" | "mod1" => Some(Modifier::Alt),
            _ => Modifier::ALL.into_iter().find(|m| m.name() == s),
        }
    }

    /// Also matches `Ctrl`, `WIN`: for errors and for refusing a modifier as a keysym.
    fn named_any_case(s: &str) -> bool {
        Modifier::named(&s.to_ascii_lowercase()).is_some()
    }
}

const _: () = assert!(Modifier::ALL.len() == Modifier::Mod5 as usize + 1);

impl std::fmt::Display for Modifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::str::FromStr for Modifier {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Modifier> {
        if let Some(m) = Modifier::named(s) {
            return Ok(m);
        }
        let names: Vec<&str> = Modifier::ALL.iter().map(|m| m.name()).collect();
        if Modifier::named_any_case(s) {
            bail!("modifier {s:?} must be written in lower case");
        }
        bail!(
            "{s:?} is not a modifier ({}; win or mod4 for super, option or mod1 for alt)",
            names.join(", ")
        )
    }
}

/// An X keysym name: ASCII letters, digits and `_` (`Return`, `plus`, `F5`, `XF86AudioRaiseVolume`), never a modifier
/// name, and never a lone capital letter: a shifted letter is `shift+k` (a `Chord` reads `K` that way). Only the
/// spelling is checked, not X's keysym table (`Retrun` parses); a non-ASCII character is refused (X names `ö`
/// `odiaeresis`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Keysym(String);

impl Keysym {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Keysym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for Keysym {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Keysym> {
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!("{s:?} is not an X keysym name (letters, digits and _; write `plus` for +)");
        }
        if Modifier::named_any_case(s) {
            bail!("{s:?} is a modifier, not a keysym");
        }
        if s.len() == 1 && s.as_bytes()[0].is_ascii_uppercase() {
            bail!(
                "{s:?} is a shifted letter: shift+{}",
                s.to_ascii_lowercase()
            );
        }
        Ok(Keysym(s.to_string()))
    }
}

/// One chord: a set of modifiers and a keysym, written `super+shift+k` (modifiers in `Modifier` order).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    pub mods: std::collections::BTreeSet<Modifier>,
    pub keysym: Keysym,
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for m in &self.mods {
            write!(f, "{m}+")?;
        }
        write!(f, "{}", self.keysym)
    }
}

impl std::str::FromStr for Chord {
    type Err = anyhow::Error;
    /// Modifiers in any order; a repeated one (also through an alias, `win+super`) is an error. A lone capital letter
    /// is that letter with shift (`K` and `shift+K` are `shift+k`).
    fn from_str(s: &str) -> anyhow::Result<Chord> {
        let in_s = |e: anyhow::Error| anyhow::anyhow!("{s:?}: {e}");
        let parts: Vec<&str> = s.split('+').collect();
        if parts.iter().any(|p| p.is_empty()) {
            bail!("{s:?} has an empty part (write `plus` for +)");
        }
        // `split` yields at least one part.
        let Some((keysym, mods)) = parts.split_last() else {
            bail!("{s:?} has no keysym");
        };
        let mut set = std::collections::BTreeSet::new();
        let shifted = keysym.len() == 1 && keysym.as_bytes()[0].is_ascii_uppercase();
        let keysym: Keysym = if shifted {
            set.insert(Modifier::Shift);
            keysym.to_ascii_lowercase().parse().map_err(in_s)?
        } else {
            keysym.parse().map_err(in_s)?
        };
        // Repeats among the modifiers as written; `shift+K` repeats nothing (the capital's shift is implied).
        let mut written = std::collections::BTreeSet::new();
        for part in mods {
            let m: Modifier = part.parse().map_err(in_s)?;
            set.insert(m);
            if !written.insert(m) {
                let alias = if *part == m.name() {
                    String::new()
                } else {
                    format!(" ({part})")
                };
                bail!("{s:?}: modifier {m}{alias} appears twice");
            }
        }
        Ok(Chord { mods: set, keysym })
    }
}

/// A key: one chord, or a sequence pressed one after another (`ctrl+b c`). Never empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    first: Chord,
    rest: Vec<Chord>,
}

impl Key {
    pub fn new(first: Chord, rest: Vec<Chord>) -> Key {
        Key { first, rest }
    }

    pub fn chords(&self) -> impl Iterator<Item = &Chord> {
        std::iter::once(&self.first).chain(&self.rest)
    }
}

impl From<Chord> for Key {
    fn from(c: Chord) -> Key {
        Key::new(c, Vec::new())
    }
}

impl std::fmt::Display for Key {
    /// The canonical spelling: chords joined by single spaces.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.first)?;
        for c in &self.rest {
            write!(f, " {c}")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for Key {
    type Err = anyhow::Error;
    /// Chords separated by whitespace; the error says what is wrong.
    fn from_str(s: &str) -> anyhow::Result<Key> {
        let mut chords = s.split_whitespace().map(str::parse::<Chord>);
        let Some(first) = chords.next() else {
            bail!("the key is empty");
        };
        Ok(Key::new(first?, chords.collect::<anyhow::Result<_>>()?))
    }
}

/// Parses every key in `sheet` and writes it back in its canonical spelling (`shift+ctrl+k` → `ctrl+shift+k`,
/// `win+k` → `super+k`); the error names the category and the binding.
pub fn normalize_sheet(sheet: &mut Sheet) -> anyhow::Result<()> {
    for (category, binds) in &mut sheet.binds {
        for b in binds {
            let key: Key = b
                .key
                .parse()
                .with_context(|| format!("category {category:?}, {:?}", b.desc))?;
            b.key = key.to_string();
        }
    }
    Ok(())
}

/// A plugin's `main` for a parser: reads the `Request` from stdin, calls `parse` on each input's text (their sheets
/// joined into one, named by the first; later inputs add their binds), parses the keys (see `normalize_sheet`) and
/// prints the `Response` as
/// one line. On an error, or a panic in `parse`, it prints one line to stderr and exits with status 1. Stdout is the
/// response: print anything else with `eprintln!`.
pub fn run(parse: impl Fn(&str) -> anyhow::Result<Sheet>) -> ! {
    run_request(|req| join(req, &parse))
}

/// As `run`, with the whole `Request` for plugins that read several inputs together, or none. Both replace the panic
/// hook: a panic on any thread of the plugin then prints one line and exits 1.
pub fn run_request(f: impl Fn(&Request) -> anyhow::Result<Sheet>) -> ! {
    // A panic anywhere (also under `panic = "abort"`) is one stderr line and exit 1, instead of the default message.
    // Written to the stderr handle, not with `eprintln!`, which a test harness would capture.
    std::panic::set_hook(Box::new(|info| {
        let msg = info.payload_as_str().unwrap_or("(no message)");
        let _ = writeln!(std::io::stderr(), "the plugin panicked: {}", one_line(msg));
        std::process::exit(1)
    }));
    let code = serve(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
        f,
    );
    std::process::exit(code)
}

fn serve(
    input: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
    f: impl Fn(&Request) -> anyhow::Result<Sheet>,
) -> i32 {
    let done = respond(input, f).and_then(|json| {
        // A failing stdout may still have taken part of the line: the reader goes by the exit status.
        out.write_all(format!("{json}\n").as_bytes())
            .and_then(|()| out.flush())
            .context("writing the response")
    });
    match done {
        Ok(()) => 0,
        Err(e) => {
            // `{:#}` joins the context chain with ": "; a message may still hold a line break.
            let _ = writeln!(err, "{}", one_line(&format!("{e:#}")));
            1
        }
    }
}

fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

fn respond(
    input: &mut dyn Read,
    f: impl Fn(&Request) -> anyhow::Result<Sheet>,
) -> anyhow::Result<String> {
    let mut text = String::new();
    input
        .read_to_string(&mut text)
        .context("reading the request")?;
    // The version first, so a later version's request says so instead of failing to parse as this one.
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let v: Versioned = serde_json::from_str(&text).context("the request is not valid")?;
    if v.version != VERSION {
        bail!(
            "request version {} is not {VERSION}, the version this plugin speaks",
            v.version
        );
    }
    let req: Request = serde_json::from_str(&text).context("the request is not valid")?;
    response(f(&req)?)
}

/// The response JSON for `sheet`, its keys parsed and written canonically.
fn response(mut sheet: Sheet) -> anyhow::Result<String> {
    normalize_sheet(&mut sheet)?;
    serde_json::to_string(&Response {
        version: VERSION,
        sheet,
    })
    .context("encoding the response")
}

fn join(req: &Request, parse: impl Fn(&str) -> anyhow::Result<Sheet>) -> anyhow::Result<Sheet> {
    let mut out: Option<Sheet> = None;
    for i in &req.inputs {
        let s = parse(&i.text).with_context(|| i.path.clone())?;
        match &mut out {
            None => out = Some(s),
            Some(o) => {
                for (category, binds) in s.binds {
                    o.binds.entry(category).or_default().extend(binds);
                }
            }
        }
    }
    out.context("the request has no inputs (a plugin that reads none uses run_request)")
}

/// For a plugin's own tests.
pub mod testing {
    use super::Sheet;
    use std::path::Path;

    /// Asserts that `parse` turns the file at `input_path` into exactly the response JSON in `expected_json_path`
    /// (`{"version": 1, "title", "provider", "binds"}`, compared as JSON values), keys checked. On a mismatch the
    /// message holds the response it got, ready to paste into the expected file once it looks right.
    pub fn assert_fixture(
        parse: impl Fn(&str) -> anyhow::Result<Sheet>,
        input_path: impl AsRef<Path>,
        expected_json_path: impl AsRef<Path>,
    ) {
        let (input, expected) = (input_path.as_ref(), expected_json_path.as_ref());
        let read = |p: &Path| {
            std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        };
        let got = parse(&read(input))
            .and_then(super::response)
            .unwrap_or_else(|e| panic!("{}: {e:#}", input.display()));
        let got: serde_json::Value = serde_json::from_str(&got).unwrap_or_else(|e| panic!("{e}"));
        let want: serde_json::Value = serde_json::from_str(&read(expected))
            .unwrap_or_else(|e| panic!("{}: {e}", expected.display()));
        assert!(
            got == want,
            "{} doesn't give {}; it gives:\n{}",
            input.display(),
            expected.display(),
            serde_json::to_string_pretty(&got).unwrap_or_default()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines of `<key> <desc>`, all in category `c`; a line `fail` is an error.
    fn parse(text: &str) -> anyhow::Result<Sheet> {
        let mut binds = Vec::new();
        for l in text.lines() {
            if l == "fail" {
                bail!("line {l:?} is not a binding");
            }
            let (key, desc) = l.split_once(' ').unwrap_or((l, ""));
            binds.push(Bind {
                key: key.into(),
                desc: desc.into(),
                action: String::new(),
                when: Default::default(),
            });
        }
        Ok(Sheet {
            title: "Test".into(),
            provider: "test".into(),
            binds: BTreeMap::from([("c".to_string(), binds)]),
        })
    }

    fn request(texts: &[&str]) -> String {
        serde_json::to_string(&Request {
            version: 1,
            context: Context::default(),
            inputs: texts
                .iter()
                .enumerate()
                .map(|(n, t)| Input {
                    path: format!("/in{n}"),
                    text: t.to_string(),
                })
                .collect(),
        })
        .unwrap()
    }

    /// (exit code, stdout, stderr) for `input` through `run`'s parse path.
    fn served(input: &str) -> (i32, String, String) {
        served_with(input, |r| join(r, parse))
    }

    fn served_with(
        input: &str,
        f: impl Fn(&Request) -> anyhow::Result<Sheet>,
    ) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = serve(&mut input.as_bytes(), &mut out, &mut err, f);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn dms_json_both_ways() {
        let json = r#"{"title":"xmonad title","provider":"xmonad","binds":{"general":[{"key":"super+b","desc":"Browser","action":""}]}}"#;
        let s: Sheet = serde_json::from_str(json).unwrap();
        let want = Sheet {
            title: "xmonad title".into(),
            provider: "xmonad".into(),
            binds: BTreeMap::from([(
                "general".to_string(),
                vec![Bind {
                    key: "super+b".into(),
                    desc: "Browser".into(),
                    action: String::new(),
                    when: Default::default(),
                }],
            )]),
        };
        assert_eq!(s, want);
        // Exactly DMS's fields, in DMS's shape, with an empty action kept.
        assert_eq!(serde_json::to_string(&s).unwrap(), json);
    }

    #[test]
    fn empty_binds_is_an_empty_object() {
        let s = Sheet {
            title: "x title".into(),
            provider: "x".into(),
            binds: BTreeMap::new(),
        };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"title":"x title","provider":"x","binds":{}}"#
        );
    }

    fn name(s: &str) -> Name {
        s.parse().unwrap()
    }

    #[test]
    fn when_is_a_string_or_a_list_and_left_out_when_empty() {
        let json = r#"{"key":"i","desc":"d","action":"","when":{"filetype":["rust","toml"],"mode":"insert"}}"#;
        let b: Bind = serde_json::from_str(json).unwrap();
        assert_eq!(
            b.when,
            BTreeMap::from([
                (
                    name("filetype"),
                    vec![name("rust"), name("toml")].try_into().unwrap()
                ),
                (name("mode"), name("insert").into()),
            ])
        );
        // One value is written as a string, several as a list.
        assert_eq!(serde_json::to_string(&b).unwrap(), json);
        let b: Bind =
            serde_json::from_str(r#"{"key":"i","desc":"d","when":{"mode":["insert"]}}"#).unwrap();
        assert_eq!(b.when["mode"], name("insert").into());
        // Absent or empty means everywhere, and isn't written.
        let b: Bind = serde_json::from_str(r#"{"key":"i","desc":"d","when":{}}"#).unwrap();
        assert!(b.when.is_empty());
        assert_eq!(
            serde_json::to_string(&b).unwrap(),
            r#"{"key":"i","desc":"d","action":""}"#
        );
        // What can't be a scope is a parse error that says why.
        for (bad, why) in [
            ("[]", "empty list of scope values"),
            (r#"["a","a"]"#, r#""a" is repeated"#),
            (r#"["a","b","a"]"#, r#""a" is repeated (at 2)"#),
            (r#""""#, "can't be empty"),
            (r#""a b""#, "no spaces"),
            (r#"["a,b"]"#, "no spaces"),
            (r#""a]""#, "no spaces"),
            ("1", "a scope value or a non-empty list of them"),
            ("[1]", "expected a string"),
            ("null", "a scope value or a non-empty list of them"),
        ] {
            let json = format!(r#"{{"key":"i","desc":"d","when":{{"mode":{bad}}}}}"#);
            let e = serde_json::from_str::<Bind>(&json).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
        for bad_scope in ["", "mo de", "mode="] {
            let json = format!(r#"{{"key":"i","desc":"d","when":{{"{bad_scope}":"x"}}}}"#);
            assert!(
                serde_json::from_str::<Bind>(&json).is_err(),
                "{bad_scope:?}"
            );
        }
        assert!(serde_json::from_str::<Bind>(r#"{"key":"i","desc":"d","when":null}"#).is_err());
        let e = serde_json::from_str::<Bind>(
            r#"{"key":"i","desc":"d","when":{"mode":"insert","mode":"normal"}}"#,
        )
        .unwrap_err();
        assert!(
            e.to_string().contains(r#"scope "mode" is given twice"#),
            "{e}"
        );
        // Values keep the order given.
        let json = r#"{"key":"i","desc":"d","action":"","when":{"filetype":["toml","rust"]}}"#;
        let b: Bind = serde_json::from_str(json).unwrap();
        assert_eq!(serde_json::to_string(&b).unwrap(), json);
    }

    #[test]
    fn names_and_values_refuse_what_a_tag_cant_show() {
        for ok in ["mode", "insert", "c++", "x-y_z.1"] {
            assert_eq!(name(ok).as_str(), ok);
        }
        for bad in [
            "",
            " ",
            "a b",
            "a\tb",
            "a,b",
            "a=b",
            "[a",
            "a]",
            "a\u{7}",
            "ö",
            "insert\u{200b}",
            "a\u{202e}b",
            "a\u{a0}b",
        ] {
            assert!(bad.parse::<Name>().is_err(), "{bad:?}");
        }
        assert!(Values::try_from(vec![]).is_err());
        let v = Values::try_from(vec![name("rust"), name("toml")]).unwrap();
        assert!(v.contains("toml") && !v.contains("lua"));
        assert_eq!(
            v.iter().map(Name::as_str).collect::<Vec<_>>(),
            ["rust", "toml"]
        );
    }

    #[test]
    fn reads_dms_without_action_and_ignores_extra_fields() {
        let json =
            r#"{"title":"t","provider":"p","extra":1,"binds":{"c":[{"key":"k","desc":"d"}]}}"#;
        let s: Sheet = serde_json::from_str(json).unwrap();
        assert_eq!(s.binds["c"][0].action, "");
        assert!(serde_json::to_string(&s)
            .unwrap()
            .contains(r#""action":"""#));
    }

    #[test]
    fn chord_canonical_order() {
        assert_eq!(chord(&["shift", "super"], "k"), "super+shift+k");
        assert_eq!(chord(&["alt", "ctrl"], "Tab"), "ctrl+alt+Tab");
        assert_eq!(
            chord(&["shift", "alt", "ctrl", "super"], "Return"),
            "super+ctrl+alt+shift+Return"
        );
        assert_eq!(chord(&[], "Print"), "Print");
    }

    #[test]
    fn chord_unknown_mods_keep_order_and_repeats_collapse() {
        assert_eq!(
            chord(&["hyper", "shift", "meta"], "k"),
            "shift+hyper+meta+k"
        );
        assert_eq!(chord(&["shift", "super", "shift"], "k"), "super+shift+k");
        assert_eq!(chord(&["hyper", "meta", "hyper"], "k"), "hyper+meta+k");
    }

    #[test]
    fn request_and_response_json_contract() {
        let r: Request = serde_json::from_str(
            r#"{"version":1,"context":{"wm":"sway","class":"Alacritty","title":"t","program":"nvim",
                "facts":{"mode":"normal"}},"inputs":[{"path":"/a","text":"x"}]}"#,
        )
        .unwrap();
        assert_eq!(r.context.wm.as_deref(), Some("sway"));
        assert_eq!(r.context.class.as_deref(), Some("Alacritty"));
        assert_eq!(r.context.title.as_deref(), Some("t"));
        assert_eq!(r.context.program.as_deref(), Some("nvim"));
        assert_eq!(
            r.context.facts,
            BTreeMap::from([(name("mode"), name("normal"))])
        );
        assert!(
            serde_json::from_str::<Request>(r#"{"version":1,"context":{"facts":null}}"#).is_err()
        );
        assert!(serde_json::from_str::<Request>(
            r#"{"version":1,"context":{"facts":{"mode":""}}}"#
        )
        .is_err());
        assert_eq!(
            r.inputs,
            [Input {
                path: "/a".into(),
                text: "x".into()
            }]
        );
        // Unknown fields and nulls: a missing value is `None`, no inputs or facts is an empty list or map.
        let r: Request =
            serde_json::from_str(r#"{"version":1,"context":{"wm":null},"later":true}"#).unwrap();
        assert_eq!(r.context, Context::default());
        assert!(r.inputs.is_empty());
        // Unknown fields inside context and inputs are ignored too.
        let r: Request = serde_json::from_str(
            r#"{"version":1,"context":{"class":"x","later":1},"inputs":[{"path":"/a","text":"","later":1}]}"#,
        )
        .unwrap();
        assert_eq!(r.context.class.as_deref(), Some("x"));
        // What keybind-find sends, field for field.
        let sent = Request {
            version: 1,
            context: Context {
                wm: Some("xmonad".into()),
                class: Some("Alacritty".into()),
                title: None,
                program: Some("nvim".into()),
                facts: BTreeMap::new(),
            },
            inputs: vec![Input {
                path: "/a".into(),
                text: "x".into(),
            }],
        };
        assert_eq!(
            serde_json::to_string(&sent).unwrap(),
            r#"{"version":1,"context":{"wm":"xmonad","class":"Alacritty","title":null,"program":"nvim","facts":{}},"inputs":[{"path":"/a","text":"x"}]}"#
        );
        let resp = Response {
            version: 1,
            sheet: parse("k K").unwrap(),
        };
        let json = r#"{"version":1,"title":"Test","provider":"test","binds":{"c":[{"key":"k","desc":"K","action":""}]}}"#;
        assert_eq!(serde_json::to_string(&resp).unwrap(), json);
        assert_eq!(serde_json::from_str::<Response>(json).unwrap(), resp);
    }

    #[test]
    fn keys_parse_to_their_canonical_spelling() {
        for (key, canonical) in [
            ("k", "k"),
            ("super+shift+Return", "super+shift+Return"),
            ("shift+super+Return", "super+shift+Return"),
            ("ctrl+b c", "ctrl+b c"),
            (" ctrl+b \t c ", "ctrl+b c"),
            ("space f f", "space f f"),
            ("F5", "F5"),
            ("fn+F5", "fn+F5"),
            ("XF86AudioRaiseVolume", "XF86AudioRaiseVolume"),
            ("KP_7", "KP_7"),
            ("K", "shift+k"),
            ("shift+K", "shift+k"),
            ("ctrl+K", "ctrl+shift+k"),
            ("shift+k", "shift+k"),
            ("mod4+mod1+k", "super+alt+k"),
            ("Retrun", "Retrun"),
            ("ctrl+plus", "ctrl+plus"),
            ("win+k", "super+k"),
            ("option+shift+k", "alt+shift+k"),
            ("meta+ctrl+k", "ctrl+meta+k"),
            ("mod5+mod2+shift+a", "shift+mod2+mod5+a"),
            (
                "super+ctrl+alt+shift+meta+fn+mod2+mod3+mod5+a",
                "super+ctrl+alt+shift+meta+fn+mod2+mod3+mod5+a",
            ),
        ] {
            let k: Key = key.parse().unwrap_or_else(|e| panic!("{key:?}: {e:#}"));
            assert_eq!(k.to_string(), canonical, "{key:?}");
        }
        for (key, why) in [
            ("", "empty"),
            ("  ", "empty"),
            ("ctrl++", "empty part"),
            ("ctrl+", "empty part"),
            ("+", "empty part"),
            ("+k", "empty part"),
            ("ö", "not an X keysym"),
            ("$(x)", "not an X keysym"),
            ("crtl+k", "\"crtl\" is not a modifier"),
            ("hyper+k", "not a modifier"),
            ("k+ctrl", "is a modifier, not a keysym"),
            ("Return+k", "not a modifier"),
            ("Ctrl+k", "lower case"),
            ("WIN+k", "lower case"),
            ("ctrl", "is a modifier, not a keysym"),
            ("shift+super", "is a modifier, not a keysym"),
            ("ctrl+Alt", "is a modifier, not a keysym"),
            ("ctrl+ctrl+k", "appears twice"),
            ("win+super+k", "modifier super appears twice"),
            ("super+win+k", "modifier super (win) appears twice"),
            ("shift+shift+K", "appears twice"),
            ("ctrl+b crtl+t", "not a modifier"),
        ] {
            let e = format!("{:#}", key.parse::<Key>().unwrap_err());
            assert!(e.contains(why), "{key:?}: {e}");
        }
    }

    #[test]
    fn typed_keys_build_what_they_print() {
        let c = Chord {
            mods: [Modifier::Shift, Modifier::Super].into(),
            keysym: "k".parse().unwrap(),
        };
        let key = Key::new(c.clone(), vec!["t".parse().unwrap()]);
        assert_eq!(key.to_string(), "super+shift+k t");
        assert_eq!(key.chords().count(), 2);
        assert_eq!(
            Key::from(c).to_string().parse::<Key>().unwrap().to_string(),
            "super+shift+k"
        );
        assert_eq!(
            Modifier::ALL.map(|m| m.name().parse::<Modifier>().unwrap()),
            Modifier::ALL
        );
    }

    #[test]
    fn the_response_holds_canonical_keys() {
        let (code, out, err) = served(&request(&["shift+ctrl+k Kill", "win+Return Term"]));
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(
            out.contains(r#""key":"ctrl+shift+k""#) && out.contains(r#""key":"super+Return""#),
            "{out}"
        );
    }

    #[test]
    fn a_request_in_a_response_out() {
        let (code, out, err) = served(&request(&["ctrl+k Kill line", "k Up"]));
        assert_eq!((code, err.as_str()), (0, ""));
        // Both inputs' binds, joined into the first sheet, one JSON line.
        assert_eq!(
            out,
            r#"{"version":1,"title":"Test","provider":"test","binds":{"c":[{"key":"ctrl+k","desc":"Kill line","action":""},{"key":"k","desc":"Up","action":""}]}}"#.to_string() + "\n"
        );
    }

    #[test]
    fn later_inputs_add_categories_and_keep_the_first_name() {
        let two = |text: &str| -> anyhow::Result<Sheet> {
            let mut s = parse(text)?;
            s.title = format!("from {text}");
            s.provider = format!("p {text}");
            let moved = s.binds.remove("c").unwrap_or_default();
            s.binds
                .insert(text.split(' ').nth(1).unwrap_or("").to_string(), moved);
            Ok(s)
        };
        let (code, out, _) = served_with(&request(&["a A", "b B", "c A"]), |r| join(r, two));
        assert_eq!(code, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["title"], "from a A");
        assert_eq!(v["provider"], "p a A");
        assert_eq!(v["binds"]["A"].as_array().unwrap().len(), 2);
        assert_eq!(v["binds"]["B"][0]["key"], "b");
    }

    /// `run` itself, in a child process (this test binary again): real stdin, stdout, exit status.
    #[test]
    fn run_prints_the_response_or_exits_1() {
        // `<parent pid>:<key>`: a value leaked into the parent's own run doesn't take it over.
        let child = std::env::var("KEYBIND_FIND_SHEET_CHILD")
            .ok()
            .and_then(|v| {
                let (pid, key) = v.split_once(':')?;
                (pid == std::os::unix::process::parent_id().to_string()).then(|| key.to_string())
            });
        if let Some(key) = child {
            run(move |_| {
                if key == "panic" {
                    panic!("out of bounds");
                }
                parse(&format!("{key} Up"))
            });
        }
        for (key, code) in [("ctrl+k", 0), ("crtl+k", 1), ("panic", 1)] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                // `-q`: the harness's per-test line would otherwise share a line with the response when it runs
                // one test at a time (`RUST_TEST_THREADS=1`).
                .args(["-q", "--exact", "tests::run_prints_the_response_or_exits_1"])
                .env(
                    "KEYBIND_FIND_SHEET_CHILD",
                    format!("{}:{key}", std::process::id()),
                )
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(request(&["x"]).as_bytes())
                .unwrap();
            let o = child.wait_with_output().unwrap();
            let (out, err) = (
                String::from_utf8(o.stdout).unwrap(),
                String::from_utf8(o.stderr).unwrap(),
            );
            assert_eq!(o.status.code(), Some(code), "{out}{err}");
            // The test harness prints its own "running 1 test" header first; the response is the last line.
            let last = out.lines().last().unwrap_or("");
            if code == 0 {
                assert!(
                    last.starts_with(r#"{"version":1,"#) && last.contains(r#""key":"ctrl+k""#),
                    "{out}"
                );
                assert_eq!(err, "");
            } else {
                assert!(!out.contains("version"), "{out}");
                // Only `run`'s line: the default panic message would add more.
                assert_eq!(err.lines().count(), 1, "{err}");
                let want = if key == "panic" {
                    "the plugin panicked: out of bounds"
                } else {
                    "not a modifier"
                };
                assert!(err.contains(want), "{err}");
            }
        }
    }

    #[test]
    fn unreadable_stdin_and_a_failing_stdout_are_one_stderr_line() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut err = Vec::new();
        let code = serve(
            &mut request(&["k Up"]).as_bytes(),
            &mut Broken,
            &mut err,
            |r| join(r, parse),
        );
        let err = String::from_utf8(err).unwrap();
        assert_eq!(code, 1);
        assert!(
            err.starts_with("writing the response: ") && err.lines().count() == 1,
            "{err}"
        );

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = serve(&mut &b"\xff"[..], &mut out, &mut err, |r| join(r, parse));
        let err = String::from_utf8(err).unwrap();
        assert_eq!((code, out.len()), (1, 0));
        assert!(
            err.starts_with("reading the request: ") && err.lines().count() == 1,
            "{err}"
        );
    }

    #[test]
    fn errors_are_one_stderr_line_and_exit_1() {
        for (input, want) in [
            (request(&["k Up", "fail"]), "/in1: line \"fail\""),
            (request(&["crtl+k Up"]), "category \"c\", \"Up\""),
            (request(&[]), "no inputs"),
            ("{".to_string(), "not valid"),
            (
                request(&["k"]).replace(r#""version":1"#, r#""version":2"#),
                "version 2 is not 1",
            ),
        ] {
            let (code, out, err) = served(&input);
            assert_eq!(code, 1, "{input}");
            assert_eq!(out, "", "{input}");
            assert_eq!(err.lines().count(), 1, "{err}");
            assert!(err.contains(want), "{input}: {err}");
        }
        // A message with a line break still makes one line.
        let (_, _, err) = served_with(&request(&[]), |_| bail!("two\r\nlines"));
        assert_eq!(err, "two  lines\n");
        // A later version says so, even when the rest doesn't fit this one.
        let (code, _, err) = served(r#"{"version":2,"inputs":"changed"}"#);
        assert_eq!(code, 1);
        assert!(err.contains("version 2 is not 1"), "{err}");
    }

    #[test]
    fn run_request_sees_the_context_and_may_have_no_inputs() {
        let req = r#"{"version":1,"context":{"program":"nvim"},"inputs":[]}"#;
        let (code, out, _) = served_with(req, |r| {
            parse(&format!(
                "k {}",
                r.context.program.as_deref().unwrap_or("-")
            ))
        });
        assert_eq!(code, 0);
        assert!(out.contains(r#""desc":"nvim""#), "{out}");
    }

    #[test]
    fn assert_fixture_passes_on_a_match() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        testing::assert_fixture(
            parse,
            format!("{dir}/sample.txt"),
            format!("{dir}/sample.json"),
        );
    }

    #[test]
    #[should_panic(expected = "it gives:")]
    fn assert_fixture_fails_on_a_mismatch() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        testing::assert_fixture(
            |t| parse(&format!("{t}extra one")),
            format!("{dir}/sample.txt"),
            format!("{dir}/sample.json"),
        );
    }

    #[test]
    #[should_panic(expected = "not a modifier")]
    fn assert_fixture_checks_keys() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        testing::assert_fixture(
            |_| parse("crtl+k Up"),
            format!("{dir}/sample.txt"),
            format!("{dir}/sample.json"),
        );
    }
}
