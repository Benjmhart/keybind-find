//! ranger: its browser keys from the default rc.conf pinned at 1.9.4 (`data/ranger/`), then `/etc/ranger/rc.conf`
//! and the user's `rc.conf`, read as ranger reads them into its key tree. Shown when ranger is the program in the
//! focused terminal. ranger is never run, and no `eval` line is.

use crate::sheet::{Bind, Sheet};
use std::path::{Path, PathBuf};

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

const DEFAULTS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/ranger/1.9.4.conf"
));

const SYSTEM: &str = "/etc/ranger/rc.conf";

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = read(&env, Path::new(SYSTEM));
    for note in notes {
        eprintln!("keybind-find: ranger: {note}");
    }
    Ok(sheet)
}

/// The sheet and notes from the files ranger would source (`load_settings`): the defaults unless
/// `RANGER_LOAD_DEFAULT_RC` is `false` (any case) and the system or user file is readable, then `system`, then the
/// user's. A file that can't be read is skipped, as ranger's `os.access(…, R_OK)` check does.
fn read(env: Env, system: &Path) -> (Sheet, Vec<String>) {
    // ranger reads text; a file that isn't UTF-8 is read lossily here.
    let text = |p: &Path| {
        std::fs::read(p)
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    };
    let files: Vec<String> = [Some(system.to_path_buf()), user_conf(env)]
        .into_iter()
        .flatten()
        .filter_map(|p| text(&p))
        .collect();
    let skip_defaults = env("RANGER_LOAD_DEFAULT_RC").is_some_and(|v| v.to_uppercase() == "FALSE")
        && !files.is_empty();
    let texts: Vec<&str> = (!skip_defaults)
        .then_some(DEFAULTS)
        .into_iter()
        .chain(files.iter().map(String::as_str))
        .collect();
    let home = env("HOME").filter(|h| !h.is_empty());
    let load = |name: &str| -> Option<String> {
        let path = match name.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                PathBuf::from(format!("{}{rest}", home.as_ref()?))
            }
            _ => PathBuf::from(name),
        };
        // A relative path (or `~user`) is relative to ranger's directory, unknown here.
        path.is_absolute().then(|| text(&path)).flatten()
    };
    parse(&texts, &load)
}

/// The user's rc.conf: `$XDG_CONFIG_HOME/ranger/rc.conf` when that is absolute, else `~/.config/ranger/rc.conf`.
fn user_conf(env: Env) -> Option<PathBuf> {
    let dir = match env("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(x) if x.is_absolute() => x.join("ranger"),
        _ => PathBuf::from(env("HOME").filter(|h| !h.is_empty())?).join(".config/ranger"),
    };
    Some(dir.join("rc.conf"))
}

/// ranger's key tree for one context (`KeyMaps`): each key code leads to a command or to more keys, in the order
/// they were first bound.
#[derive(Clone, Debug)]
enum Node {
    /// The command, and whether its keys were written with a `<number>` code (bound, but not listed).
    Command(String, bool),
    Keys(Vec<(i64, Node)>),
}

/// Reads a file named by a `source` line (`~` expanded); `None` when it can't be read.
type Load<'a> = &'a dyn Fn(&str) -> Option<String>;

/// How deep `source` lines may nest (ranger has no limit; a file sourcing itself would never end).
const DEPTH: usize = 8;

/// How many `source` files are read in all.
const LOADS: usize = 64;

/// What sourcing the files builds: the key tree, the aliases, and the lines not run.
struct State<'a> {
    tree: Vec<(i64, Node)>,
    /// `alias <name> <command…>`, latest last.
    aliases: Vec<(String, String)>,
    skipped_lines: usize,
    /// `source` files read so far.
    loads: usize,
    load: Load<'a>,
}

impl State<'_> {
    /// `actions.source`: each line, spaces and line ends stripped; comments and blank lines skipped.
    fn source(&mut self, text: &str, depth: usize) {
        for line in text.lines() {
            let line = line.trim_matches(|c| c == ' ' || c == '\r' || c == '\n');
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            if self.execute(line, depth) != Outcome::Ran {
                self.skipped_lines += 1;
            }
        }
    }

    /// `execute_console` for one command line.
    fn execute(&mut self, line: &str, depth: usize) -> Outcome {
        let words: Vec<&str> = line.split_whitespace().collect();
        // `execute_console('')` raises (an empty `chain` part).
        let Some(&name) = words.first() else {
            return Outcome::Raised;
        };
        // An alias stands for its command, the rest of the line kept (`command_alias_factory`); its command was
        // resolved when it was defined.
        if let Some((_, full)) = self.aliases.iter().rev().find(|(a, _)| a == name) {
            let after = &line[line.find(name).map_or(0, |i| i + name.len())..];
            return self.builtin(&format!("{full}{after}"), depth);
        }
        self.builtin(line, depth)
    }

    /// One command line whose first word is ranger's own command.
    fn builtin(&mut self, line: &str, depth: usize) -> Outcome {
        let words: Vec<&str> = line.split_whitespace().collect();
        let ok = |ran: bool| if ran { Outcome::Ran } else { Outcome::Raised };
        match words.first().copied().unwrap_or("") {
            "map" if words.len() >= 3 => {
                let (keys, numbered) = codes_marked(words[1]);
                bind(
                    &mut self.tree,
                    &keys,
                    Node::Command(rest(line, 2).to_string(), numbered),
                );
                Outcome::Ran
            }
            // Each target in turn; a source that isn't bound stops the line (ranger's `KeyError`). The copy is
            // hidden only when its own keys are written with a `<number>`.
            "copymap" if words.len() >= 3 => ok(words[2..].iter().all(|target| {
                let Some(mut node) = find(&self.tree, &codes(words[1])).cloned() else {
                    return false;
                };
                let (keys, numbered) = codes_marked(target);
                hide(&mut node, numbered);
                bind(&mut self.tree, &keys, node);
                true
            })),
            "unmap" => ok(words[1..]
                .iter()
                .all(|keys| unbind(&mut self.tree, &codes(keys)))),
            // `alias <name> <command…>`: an alias in the command is resolved now, as ranger binds the alias to the
            // command class it names when it is defined.
            "alias" if words.len() >= 3 => {
                let full = rest(line, 2);
                let head = full.split_whitespace().next().unwrap_or("");
                let full = match self.aliases.iter().rev().find(|(a, _)| a == head) {
                    Some((_, target)) => format!("{target}{}", &full[head.len()..]),
                    None => full.to_string(),
                };
                self.aliases.push((words[1].to_string(), full));
                Outcome::Ran
            }
            // `chain <command>; <command>…`: each in turn. `execute_console` catches a command's error, so the chain
            // goes on; only an empty part raises before that (`split()[0]`) and stops it. The line counts once.
            "chain" => {
                let mut all_ran = true;
                for c in rest(line, 1).split(';').map(str::trim) {
                    if c.is_empty() {
                        return Outcome::Raised;
                    }
                    all_ran &= self.execute(c, depth) == Outcome::Ran;
                }
                if all_ran {
                    Outcome::Ran
                } else {
                    Outcome::Raised
                }
            }
            // `source <file>`: its lines, as a config file of their own; one that can't be read raises.
            // ranger's `source` takes one argument (`fm.source(filename)`): more is a TypeError. A budget of loads
            // keeps a file that sources itself many times over from hanging the hotkey.
            "source" if words.len() == 2 => match (depth < DEPTH && self.loads < LOADS)
                .then(|| {
                    self.loads += 1;
                    (self.load)(words[1])
                })
                .flatten()
            {
                Some(text) => {
                    self.source(&text, depth + 1);
                    Outcome::Ran
                }
                None => Outcome::Raised,
            },
            // Never run.
            "eval" => Outcome::NotRun,
            // Other contexts, settings: not browser keys.
            _ => Outcome::Ran,
        }
    }
}

/// What ranger does with one command line, as far as keybind-find follows it.
#[derive(PartialEq)]
enum Outcome {
    Ran,
    /// Not run here (`eval`): counted.
    NotRun,
    /// ranger reports an error: counted.
    Raised,
}

/// The sheet from rc.conf `texts` in the order ranger sources them, and the notes for stderr.
fn parse(texts: &[&str], load: Load) -> (Sheet, Vec<String>) {
    let mut st = State {
        tree: Vec::new(),
        aliases: Vec::new(),
        skipped_lines: 0,
        loads: 0,
        load,
    };
    for text in texts {
        st.source(text, 0);
    }
    let (tree, skipped_lines) = (st.tree, st.skipped_lines);
    let mut sheet = Sheet {
        title: "ranger".into(),
        provider: "ranger".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    let mut path = Vec::new();
    walk(&tree, &mut path, &mut |keys, command, hidden| {
        let Some(key) = notation(keys).filter(|_| !hidden) else {
            left_out += 1;
            return;
        };
        let category = command.split_whitespace().next().unwrap_or("ranger");
        let binds = sheet.binds.entry(category.to_string()).or_default();
        // `<backspace>` and `<backspace2>` are both BackSpace: one entry when they run the same command.
        if binds.iter().any(|b| b.key == key && b.desc == command) {
            return;
        }
        binds.push(Bind {
            key,
            desc: command.to_string(),
            action: command.to_string(),
            when: Default::default(),
        });
    });
    let mut notes = Vec::new();
    if left_out + skipped_lines > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) keybind-find doesn't list (<any>, <bg>, a <number>, a key with no keysym) and \
             {skipped_lines} line(s) it doesn't run (eval) or ranger stops on with an error"
        ));
    }
    (sheet, notes)
}

/// `Command.rest(n)`: the line from its `n`th word on.
fn rest(line: &str, n: usize) -> &str {
    let mut words = 0;
    let mut in_space = true;
    for (i, c) in line.char_indices() {
        if c.is_whitespace() {
            if !in_space {
                in_space = true;
                words += 1;
            }
        } else if in_space {
            in_space = false;
            if words == n {
                return &line[i..];
            }
        }
    }
    ""
}

/// `KeyMaps.bind`: a key that held a command becomes a prefix, and a prefix bound as a whole is replaced.
fn bind(map: &mut Vec<(i64, Node)>, keys: &[i64], node: Node) {
    let Some((&key, more)) = keys.split_first() else {
        return;
    };
    let at = map.iter().position(|(k, _)| *k == key);
    if more.is_empty() {
        match at {
            Some(i) => map[i].1 = node,
            None => map.push((key, node)),
        }
        return;
    }
    let i = match at {
        Some(i) => {
            if matches!(map[i].1, Node::Command(..)) {
                map[i].1 = Node::Keys(Vec::new());
            }
            i
        }
        None => {
            map.push((key, Node::Keys(Vec::new())));
            map.len() - 1
        }
    };
    if let Node::Keys(inner) = &mut map[i].1 {
        bind(inner, more, node);
    }
}

/// What `keys` lead to, as `KeyMaps.copy` finds it; `None` when it isn't bound.
fn find<'a>(map: &'a [(i64, Node)], keys: &[i64]) -> Option<&'a Node> {
    let (key, more) = keys.split_first()?;
    let node = &map.iter().find(|(k, _)| k == key)?.1;
    match (more.is_empty(), node) {
        (true, n) => Some(n),
        (false, Node::Keys(inner)) => find(inner, more),
        (false, Node::Command(..)) => None,
    }
}

/// `_unbind_traverse`: removes `keys` and any prefix left empty; `false` where ranger raises (a command where more
/// keys were to follow).
fn unbind(map: &mut Vec<(i64, Node)>, keys: &[i64]) -> bool {
    let Some((key, more)) = keys.split_first() else {
        return true;
    };
    let Some(i) = map.iter().position(|(k, _)| k == key) else {
        return true;
    };
    if more.is_empty() {
        map.remove(i);
        return true;
    }
    let Node::Keys(inner) = &mut map[i].1 else {
        return false;
    };
    let ok = unbind(inner, more);
    if inner.is_empty() {
        map.remove(i);
    }
    ok
}

/// Marks every command under `node` as bound with a `<number>` code, or not.
fn hide(node: &mut Node, numbered: bool) {
    match node {
        Node::Command(_, hidden) => *hidden = numbered,
        Node::Keys(inner) => inner.iter_mut().for_each(|(_, n)| hide(n, numbered)),
    }
}

fn walk(map: &[(i64, Node)], path: &mut Vec<i64>, each: &mut dyn FnMut(&[i64], &str, bool)) {
    for (key, node) in map {
        path.push(*key);
        match node {
            Node::Command(c, hidden) => each(path, c, *hidden),
            Node::Keys(inner) => walk(inner, path, each),
        }
        path.pop();
    }
}

// ranger's markers, numbered past curses' keys.
const ANY: i64 = 9001;
const PASSIVE: i64 = 9002;
const ALT: i64 = 9003;
const QUANTIFIER: i64 = 9004;

/// `parse_keybinding`: a key sequence as key codes. `<name>` is looked up lower-cased; a number in brackets is that
/// code (marked, and never sent); any other `<…>` is its characters. `_clean_input` first turns the text into its
/// UTF-8 bytes (`encode('utf-8').decode('latin-1')`), so a non-ASCII character is several codes.
fn codes(keys: &str) -> Vec<i64> {
    codes_marked(keys).0
}

/// `codes`, and whether a `<number>` was among them: that code is the key itself in ranger's tree (`<10>` is
/// Return), but the binding isn't listed.
fn codes_marked(keys: &str) -> (Vec<i64>, bool) {
    let mut numbered = false;
    let bytes = keys.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = bytes[i + 1..].iter().position(|b| *b == b'>') {
                let inner = &bytes[i + 1..i + 1 + end];
                // ranger lower-cases the bytes as Latin-1 text: ASCII only matters for the names.
                let name = String::from_utf8_lossy(inner).to_ascii_lowercase();
                match special(&name) {
                    Some(k) => out.extend(k),
                    // A key code: that key in the tree, but its binding left out (the AC).
                    None if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) => {
                        numbered = true;
                        out.push(name.parse::<u32>().map_or(-2, i64::from));
                    }
                    None => {
                        out.push(b'<' as i64);
                        out.extend(inner.iter().map(|b| *b as i64));
                        out.push(b'>' as i64);
                    }
                }
                i += end + 2;
                continue;
            }
        }
        out.push(bytes[i] as i64);
        i += 1;
    }
    (out, numbered)
}

/// `special_keys`, by lower-cased name.
fn special(name: &str) -> Option<Vec<i64>> {
    let base = |n: &str| -> Option<i64> {
        Some(match n {
            "bs" | "backspace" => 263,
            "backspace2" => 127,
            "delete" => 330,
            "s-delete" => 383,
            "insert" => 331,
            "cr" | "enter" | "return" => 10,
            "space" => 32,
            "esc" | "escape" => 27,
            "down" => 258,
            "up" => 259,
            "left" => 260,
            "right" => 261,
            "pagedown" => 338,
            "pageup" => 339,
            "home" => 262,
            "end" => 360,
            "tab" => 9,
            "s-tab" => 353,
            "lt" => 60,
            "gt" => 62,
            _ => return None,
        })
    };
    if let Some(k) = base(name) {
        return Some(vec![k]);
    }
    if let Some(rest) = name.strip_prefix("a-") {
        if let Some(k) = base(rest) {
            return Some(vec![ALT, k]);
        }
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            // Lower-cased already, so only lower-case letters match, as in ranger.
            if c.is_ascii_alphanumeric() || "_!{}".contains(c) {
                return Some(vec![ALT, c as i64]);
            }
        }
    }
    if let Some(rest) = name.strip_prefix("c-") {
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_lowercase() || c == '_' {
                return Some(vec![c as i64 - 96]);
            }
        }
        if rest == "space" {
            return Some(vec![0]);
        }
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<i64>().ok()) {
        if (0..64).contains(&n) && name[1..] == n.to_string() {
            return Some(vec![264 + n]);
        }
    }
    Some(vec![match name {
        "any" => ANY,
        "alt" => ALT,
        "bg" => PASSIVE,
        "allow_quantifiers" => QUANTIFIER,
        _ => return None,
    }])
}

/// A key-code sequence as key notation; `None` when a key can't be sent (`<any>`, `<bg>`, a code with no keysym).
fn notation(keys: &[i64]) -> Option<String> {
    let mut chords = Vec::new();
    let mut alt = false;
    for &k in keys {
        if k == ALT {
            if alt {
                return None;
            }
            alt = true;
            continue;
        }
        let (mut mods, keysym) = key(k)?;
        if std::mem::take(&mut alt) {
            mods.push("alt");
        }
        chords.push(crate::sheet::chord(&mods, &keysym));
    }
    (!alt && !chords.is_empty()).then(|| chords.join(" "))
}

/// One key code (curses' numbering) as modifiers and a keysym.
fn key(k: i64) -> Option<(Vec<&'static str>, String)> {
    let plain = |s: &str| Some((Vec::new(), s.to_string()));
    match k {
        0 => Some((vec!["ctrl"], "space".into())),
        9 => plain("Tab"),
        10 => plain("Return"),
        // `<c-_>` is `ord('_') - 96`, -1: no key sends it (ctrl+_ is 31), so it isn't listed.
        1..=26 => Some((vec!["ctrl"], ((k + 96) as u8 as char).to_string())),
        27 => plain("Escape"),
        32..=126 => {
            let c = k as u8 as char;
            let keysym = super::nvim::single(c.to_ascii_lowercase())?.to_string();
            Some((
                if c.is_ascii_uppercase() {
                    vec!["shift"]
                } else {
                    Vec::new()
                },
                keysym,
            ))
        }
        127 | 263 => plain("BackSpace"),
        330 => plain("Delete"),
        383 => Some((vec!["shift"], "Delete".into())),
        331 => plain("Insert"),
        258 => plain("Down"),
        259 => plain("Up"),
        260 => plain("Left"),
        261 => plain("Right"),
        338 => plain("Next"),
        339 => plain("Prior"),
        262 => plain("Home"),
        360 => plain("End"),
        353 => Some((vec!["shift"], "Tab".into())),
        265..=299 => plain(&format!("F{}", k - 264)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ranger")
            .join(name)
    }

    /// `parse` with no file to `source`.
    fn parse_t(texts: &[&str]) -> (Sheet, Vec<String>) {
        parse(texts, &|_| None)
    }

    fn text(name: &str) -> String {
        std::fs::read_to_string(fixture(name)).unwrap()
    }

    fn get<'a>(s: &'a Sheet, key: &str) -> Vec<&'a str> {
        s.binds
            .values()
            .flatten()
            .filter(|b| b.key == key)
            .map(|b| b.desc.as_str())
            .collect()
    }

    #[test]
    fn pinned_defaults() {
        assert!(
            DEFAULTS.contains("tag v1.9.4")
                && DEFAULTS.contains("55fc2839f693aed9486d211694d7317d78161341")
        );
        assert!(DEFAULTS.contains("ranger/config/rc.conf"));
        let (s, notes) = parse_t(&[DEFAULTS]);
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 280);
        // Only the `<any>`/`<bg>` maps are left out; no line is skipped.
        assert_eq!(
            notes,
            ["leaving out 11 key(s) keybind-find doesn't list (<any>, <bg>, a <number>, a key with no keysym) and \
              0 line(s) it doesn't run (eval) or ranger stops on with an error"]
        );
        assert_eq!(get(&s, "g g"), ["move to=0"]);
        assert_eq!(get(&s, "q"), ["quit"]);
        assert_eq!(get(&s, "plus u x"), ["shell -f chmod u+x %s"]);
        assert_eq!(get(&s, "k"), ["move up=1"]);
        assert_eq!(get(&s, "Up"), ["move up=1"]);
        // A `map … eval …` default shows its text, under `eval`; nothing is run.
        assert!(s.binds["eval"]
            .iter()
            .any(|b| b.key == "shift+a" && b.desc.starts_with("eval fm.open_console(")));
        assert_eq!(get(&s, "alt+1"), ["tab_open 1"]);
        // The category is the command's first word.
        assert!(s.binds["quit"].iter().any(|b| b.key == "q"));
        for b in s.binds.values().flatten() {
            assert!(
                b.key.parse::<keybind_find_sheet::Key>().is_ok(),
                "{}",
                b.key
            );
            assert!(b.when.is_empty());
        }
        // Each key once (BackSpace's two codes run the same command).
        let mut keys: Vec<&str> = s.binds.values().flatten().map(|b| b.key.as_str()).collect();
        keys.sort();
        let n = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), n);
    }

    #[test]
    fn rules() {
        let (user, system) = (text("rc.conf"), text("system-rc.conf"));
        let (s, notes) = parse_t(&[DEFAULTS, &system, &user]);
        assert_eq!(get(&s, "q"), ["quitall"]);
        assert!(get(&s, "shift+q").is_empty());
        // `copymap` copies the binding the source has at that line: the user's.
        assert_eq!(get(&s, "ctrl+q"), ["quitall"]);
        assert_eq!(get(&s, "shift+z shift+q"), ["quitall"]);
        assert_eq!(get(&s, "shift+j"), ["move down=5"]);
        assert_eq!(get(&s, "alt+j"), ["scroll_preview 3"]);
        assert_eq!(get(&s, "ctrl+f"), ["move down=1 pages=True"]);
        // Other contexts skipped: the console's `<C-x>` isn't a browser key, the pager's `q` didn't replace it.
        assert!(get(&s, "ctrl+x").is_empty() && get(&s, "ctrl+y").is_empty());
        // The system file's `ZZ` stands; its `w` lost to the user's later lines.
        assert_eq!(get(&s, "shift+z shift+z"), ["quit"]);
        assert_eq!(get(&s, "w"), ["console tab_new"]);
        // `eval` counted; `x<any>` left out with the defaults' 11.
        assert!(
            notes[0].starts_with("leaving out 12 key(s)") && notes[0].contains(" and 1 line(s)"),
            "{notes:?}"
        );
    }

    #[test]
    fn the_key_tree() {
        // Binding a prefix replaces what was under it; binding under a key that held a command makes it a prefix.
        let (s, _) = parse_t(&["map gg top\nmap g go\n"]);
        assert_eq!((get(&s, "g"), get(&s, "g g")), (vec!["go"], vec![]));
        let (s, _) = parse_t(&["map g go\nmap gx x\n"]);
        assert_eq!((get(&s, "g"), get(&s, "g x")), (vec![], vec!["x"]));
        // `copymap` of a prefix copies everything under it.
        let (s, _) = parse_t(&["map ga a\nmap gb b\ncopymap g h\n"]);
        assert_eq!((get(&s, "h a"), get(&s, "h b")), (vec!["a"], vec!["b"]));
        // `unmap` removes the key and a prefix left empty; several at once.
        let (s, _) = parse_t(&["map ga a\nmap gb b\nmap c c\nunmap ga gb c\n"]);
        assert!(s.binds.is_empty());
        // A missing source stops the line (the targets after it too); so does unmapping under a command.
        let (s, notes) = parse_t(&["map a a\ncopymap zz b\nunmap aa\n"]);
        assert_eq!(get(&s, "a"), ["a"]);
        assert!(
            notes[0]
                .ends_with("and 2 line(s) it doesn't run (eval) or ranger stops on with an error"),
            "{notes:?}"
        );
        // Unmapping a key not bound is nothing (not counted); a list stops where ranger raises.
        let (s, notes) = parse_t(&["map a a\nmap b b\nunmap zz\nunmap b aa a\n"]);
        assert_eq!((get(&s, "a"), get(&s, "b")), (vec!["a"], vec![]));
        assert!(notes[0].contains(" and 1 line(s)"), "{notes:?}");
        // `source` reads the named file (here through the test's loader), `chain` runs each command, an alias stands
        // for its command; a `source` of a file that can't be read is counted.
        let files =
            |name: &str| (name == "/keys.conf").then(|| "map k up\nalias m map\n".to_string());
        let (s, notes) = parse(
            &["source /keys.conf\nchain map a 1; map b 2\nm c three\nsource /none.conf\n"],
            &files,
        );
        assert_eq!(
            (get(&s, "k"), get(&s, "a"), get(&s, "b")),
            (vec!["up"], vec!["1"], vec!["2"])
        );
        assert_eq!(get(&s, "c"), ["three"]);
        assert!(notes[0].contains(" and 1 line(s)"), "{notes:?}");
        // A file sourcing itself stops at the depth limit; one sourcing itself many times over, at the budget.
        let me = |_: &str| Some("map x y\nsource /me\n".to_string());
        let (s, notes) = parse(&["source /me\n"], &me);
        assert_eq!(
            (get(&s, "x"), notes[0].contains(" and 1 line(s)")),
            (vec!["y"], true),
            "{notes:?}"
        );
        let many = |_: &str| Some("source /m\n".repeat(20));
        let (_, notes) = parse(&["source /m\n"], &many);
        assert!(!notes.is_empty());
        // `source` takes one argument: a path with a space is ranger's TypeError.
        let any = |_: &str| Some("map z z\n".to_string());
        let (s, _) = parse(&["source /my dir/keys.conf\n"], &any);
        assert!(get(&s, "z").is_empty());
        // `chain` goes on past an `eval` (not run here) and past a command's error (ranger catches it), and stops at
        // an empty part; each such line counts once.
        let (s, notes) = parse_t(&[
            "chain eval x; map c d\nchain copymap zz q; map e f\nchain map g h;; map i j\n",
        ]);
        assert_eq!(
            (get(&s, "c"), get(&s, "e"), get(&s, "g"), get(&s, "i")),
            (vec!["d"], vec!["f"], vec!["h"], vec![])
        );
        assert!(notes[0].contains(" and 3 line(s)"), "{notes:?}");
        // An alias's command is resolved when it is defined: redefining `m` later doesn't change `n`.
        let (s, _) = parse_t(&["alias m map\nalias n m\nalias m unmap\nn k up\n"]);
        assert_eq!(get(&s, "k"), ["up"]);
        // A `<number>` is that key in the tree (`<10>` is Return, `<100>` is `d`), but its binding isn't listed.
        let (s, notes) =
            parse_t(&["map <CR> open\nmap <10> run\nmap <100> x\ncopymap <CR> <101>\n"]);
        assert!(get(&s, "Return").is_empty() && get(&s, "d").is_empty() && get(&s, "e").is_empty());
        assert!(notes[0].starts_with("leaving out 3 key(s)"), "{notes:?}");
        let (s, _) = parse_t(&["map <100> x\nmap d shown\n"]);
        assert_eq!(get(&s, "d"), ["shown"]);
        // The hiding follows the keys as written: a copy to a named key is listed.
        let (s, _) = parse_t(&["map <100> x\ncopymap d e\n"]);
        assert_eq!(get(&s, "e"), ["x"]);
        // Two names for one code are one key: `<C-j>` is `<CR>`.
        let (s, _) = parse_t(&["map <CR> open\nmap <C-j> down\n"]);
        assert_eq!(get(&s, "Return"), ["down"]);
        // `map` with no command, and blank or comment lines: nothing.
        let (s, notes) = parse_t(&["map x\n\n   # note\n  map  y   cmd   arg\n"]);
        assert_eq!((get(&s, "y"), notes.len()), (vec!["cmd   arg"], 0));
    }

    #[test]
    fn files_and_order() {
        let dir = std::env::temp_dir().join(format!("kbf-ranger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let home = dir.join("home");
        std::fs::create_dir_all(home.join(".config/ranger")).unwrap();
        std::fs::copy(fixture("rc.conf"), home.join(".config/ranger/rc.conf")).unwrap();
        let system = dir.join("system-rc.conf");
        let h = home.display().to_string();
        let env = |extra: &'static [(&'static str, &'static str)]| {
            let h = h.clone();
            move |k: &str| {
                if k == "HOME" {
                    return Some(h.clone());
                }
                extra
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        // An unreadable file is skipped, silently (the defaults stand).
        let locked = dir.join("locked.conf");
        std::fs::write(&locked, "map q x\n").unwrap();
        std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .unwrap();
        if std::fs::read(&locked).is_err() {
            let (s, notes) = read(&|_| None, &locked);
            assert_eq!((get(&s, "q"), notes.len()), (vec!["quit"], 1));
        }
        // `source ~/…` in the user's file, through `read`'s loader (`HOME`); a relative one isn't read.
        std::fs::write(home.join("extra.conf"), "map <F7> reload_cwd\n").unwrap();
        std::fs::write(
            home.join(".config/ranger/rc.conf"),
            format!(
                "{}source ~/extra.conf\nsource extra.conf\n",
                text("rc.conf")
            ),
        )
        .unwrap();
        let (s, notes) = read(&env(&[]), &system);
        assert_eq!(get(&s, "F7"), ["reload_cwd"]);
        assert!(notes[0].contains(" and 2 line(s)"), "{notes:?}");
        // Defaults, then the user's file (no system file).
        let (s, _) = read(&env(&[]), &system);
        assert_eq!(
            (get(&s, "q"), get(&s, "g g")),
            (vec!["quitall"], vec!["move to=0"])
        );
        // RANGER_LOAD_DEFAULT_RC=false (any case) with a readable user file: no defaults.
        let (s, _) = read(&env(&[("RANGER_LOAD_DEFAULT_RC", "false")]), &system);
        assert!(get(&s, "g g").is_empty());
        assert_eq!(get(&s, "q"), ["quitall"]);
        // … but kept when neither the user's nor the system file is readable.
        let (s, _) = read(
            &env(&[
                ("RANGER_LOAD_DEFAULT_RC", "FALSE"),
                ("XDG_CONFIG_HOME", "/nonexistent"),
            ]),
            &system,
        );
        assert_eq!(get(&s, "g g"), ["move to=0"]);
        // The system file counts as readable, and comes before the user's.
        std::fs::copy(fixture("system-rc.conf"), &system).unwrap();
        let (s, _) = read(
            &env(&[
                ("RANGER_LOAD_DEFAULT_RC", "False"),
                ("XDG_CONFIG_HOME", "/nonexistent"),
            ]),
            &system,
        );
        assert_eq!((get(&s, "w"), get(&s, "g g")), (vec!["tab_close"], vec![]));
        let _ = std::fs::remove_dir_all(&dir);
        // XDG_CONFIG_HOME absolute: its ranger/; relative or empty: ~/.config/ranger.
        assert_eq!(
            user_conf(&env(&[("XDG_CONFIG_HOME", "/x")])),
            Some(PathBuf::from("/x/ranger/rc.conf"))
        );
        for xdg in ["", "rel"] {
            let pairs: &'static [(&'static str, &'static str)] = if xdg.is_empty() {
                &[("XDG_CONFIG_HOME", "")]
            } else {
                &[("XDG_CONFIG_HOME", "rel")]
            };
            assert_eq!(
                user_conf(&env(pairs)),
                Some(home.join(".config/ranger/rc.conf"))
            );
        }
        assert_eq!(user_conf(&|_| None), None);
    }

    #[test]
    fn keys() {
        for (ranger, want) in [
            ("<C-f>", "ctrl+f"),
            ("<a-j>", "alt+j"),
            ("<A-J>", "alt+j"),
            ("<CR>", "Return"),
            ("<enter>", "Return"),
            ("<esc>", "Escape"),
            ("<space>", "space"),
            ("<tab>", "Tab"),
            ("<s-tab>", "shift+Tab"),
            ("<bs>", "BackSpace"),
            ("<backspace2>", "BackSpace"),
            ("<delete>", "Delete"),
            ("<insert>", "Insert"),
            ("<up>", "Up"),
            ("<LEFT>", "Left"),
            ("<home>", "Home"),
            ("<end>", "End"),
            ("<PAGEDOWN>", "Next"),
            ("<pageup>", "Prior"),
            ("<lt>", "less"),
            ("<gt>", "greater"),
            ("<F5>", "F5"),
            ("<c-space>", "ctrl+space"),
            ("<a-left>", "alt+Left"),
            ("<s-delete>", "shift+Delete"),
            ("<a-5>", "alt+5"),
            ("<a-{>", "alt+braceleft"),
            ("<alt>j", "alt+j"),
            ("<f35>", "F35"),
            ("J", "shift+j"),
            ("gg", "g g"),
            ("+ux", "plus u x"),
            ("<foo>", "less f o o greater"),
            ("<x", "less x"),
        ] {
            assert_eq!(notation(&codes(ranger)).as_deref(), Some(want), "{ranger}");
        }
        for ranger in [
            "p`<any>",
            "<allow_quantifiers>",
            "m<bg>",
            "<f0>",
            "<f40>",
            "<c-_>",
            "<999999999999999999999>",
            "é",
            "<alt>",
        ] {
            assert_eq!(notation(&codes(ranger)), None, "{ranger}");
        }
    }
}
