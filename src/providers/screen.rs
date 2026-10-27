//! GNU screen: its command keys (the escape key, then one key), from defaults pinned per screen version
//! (`data/screen/<version>.toml`, from `process.c` `InitKeytab()` at that release) with `/etc/screenrc` and the
//! user's screenrc laid over them. screen can't print its key table, so this is what the files say. Shown when screen
//! is the program in the focused terminal.
//!
//! ponytail: the session's own command line (`screen -e ^Bb`, `-c file`) and environment (`$SCREENRC`) aren't known
//! here, only this process's; and `/etc/screenrc` stands for the system file a build may have elsewhere
//! (`SYSTEM_SCREENRC`). Read the server's `/proc/<pid>/cmdline` and `environ` if that bites.

use super::nvim::single;
use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// One entry per screen version whose defaults we know, oldest first. Supporting a new screen: add its file and a
/// line.
const REGISTRY: &[(&str, &str)] = &[
    (
        "4.9.1",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/screen/4.9.1.toml"
        )),
    ),
    (
        "5.0.2",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/data/screen/5.0.2.toml"
        )),
    ),
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pinned {
    bind: Vec<PinnedBind>,
}

#[derive(Deserialize)]
struct PinnedBind {
    key: String,
    command: String,
    desc: String,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let mut cmd = Command::new("screen");
    cmd.arg("--version")
        .env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    let version = run_for(cmd, VERSION_TIMEOUT).context("running screen --version");
    let (sheet, notes) = sheet_with(REGISTRY, version, Path::new("/etc/screenrc"), &env)?;
    if !notes.is_empty() {
        eprintln!("keybind-find: screen: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// The sheet and its notes: the pinned defaults picked by `version` (`screen --version`'s output, or why it failed),
/// then `system` and the user's screenrc.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    version: anyhow::Result<String>,
    system: &Path,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut notes = Vec::new();
    let installed = match &version {
        Ok(text) => version_of(text),
        Err(e) => {
            notes.push(format!("{e:#}"));
            None
        }
    };
    let (v, text, note) =
        select(registry, installed.as_deref()).context("no pinned screen defaults")?;
    notes.extend(note);
    let pinned: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/screen/{v}.toml"))?;
    let mut rc = Keytab::new(&pinned)?;
    rc.read(system, false, &mut notes, env);
    let user = env("SCREENRC")
        .filter(|f| !f.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| Path::new(&h).join(".screenrc")));
    if let Some(user) = user {
        rc.read(&user, false, &mut notes, env);
    }
    let (sheet, left_out) = rc.sheet();
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} bindings (bindkey and bind -k lines, keys with no key notation, or a class no key enters)"
        ));
    }
    Ok((sheet, notes))
}

/// The version in `Screen version 4.09.01 (GNU) 20-Aug-23` or `Screen version 5.0.2 (build on …)`, zeros dropped
/// (`4.9.1`).
fn version_of(text: &str) -> Option<String> {
    let mut words = text.lines().next()?.split_whitespace();
    words.find(|w| w.eq_ignore_ascii_case("version"))?;
    let parts = parse_version(words.next()?)?;
    Some(
        parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join("."),
    )
}

/// `4.09.01` → [4, 9, 1]; a part's trailing letters (`01beta`, `2-dirty`) are dropped.
fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.')
        .map(|p| {
            let digits = p.find(|c: char| !c.is_ascii_digit()).unwrap_or(p.len());
            p[..digits].parse().ok()
        })
        .collect()
}

/// The pinned file for `installed`: the newest of the same major version, else the newest; with a note naming the
/// file to add when it isn't the same major.minor, or the version is unknown.
fn select(
    registry: &[(&'static str, &'static str)],
    installed: Option<&str>,
) -> Option<(&'static str, &'static str, Option<String>)> {
    let mut entries: Vec<(Vec<u64>, &'static str, &'static str)> = registry
        .iter()
        .filter_map(|&(v, t)| Some((parse_version(v)?, v, t)))
        .collect();
    entries.sort();
    let &(_, newest, newest_text) = entries.last()?;
    let Some(wanted) = installed.and_then(parse_version) else {
        let note = format!("screen's version is unknown: using the defaults of {newest}");
        return Some((newest, newest_text, Some(note)));
    };
    let (pinned, v, t) = match entries
        .iter()
        .rev()
        .find(|(p, _, _)| p.first() == wanted.first())
    {
        Some((p, v, t)) => (p.get(..2), *v, *t),
        None => (None, newest, newest_text),
    };
    let note = (pinned != wanted.get(..2)).then(|| {
        let installed = installed.unwrap_or_default();
        format!(
            "screen {installed}: using the defaults of {v}; add data/screen/{installed}.toml for its own"
        )
    });
    Some((v, t, note))
}

/// A key after the escape: a byte, or the termcap back-tab key.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Key {
    Byte(u8),
    Backtab,
}

/// One binding: key, command with its arguments, and the pinned description when it is a default command.
type Entry = (Key, String, Option<String>);

/// screen's key tables as the rc files leave them.
struct Keytab {
    main: Vec<Entry>,
    /// The rc files' `bind` lines in order (class, key, command and description, or `None` to remove), replayed over
    /// the defaults once the last `escape` is known.
    binds: Vec<(Option<String>, Key, Option<(String, Option<String>)>)>,
    /// The escape key and the key that sends it (`escape xy`); `None` after `escape ""` (no command key).
    escape: Option<(u8, u8)>,
    /// Pinned descriptions, by command.
    descs: Vec<(String, String)>,
    left_out: usize,
    read: BTreeSet<PathBuf>,
}

impl Keytab {
    fn new(pinned: &Pinned) -> anyhow::Result<Keytab> {
        let mut main = Vec::new();
        for b in &pinned.bind {
            // One character, `^X`, or `backtab`.
            let key = match b.key.as_bytes() {
                b"backtab" => Key::Backtab,
                [c] => Key::Byte(*c),
                [b'^', b'?'] => Key::Byte(0o177),
                [b'^', c] => Key::Byte(c & 0x1f),
                _ => bail!("pinned key {:?} is not one key", b.key),
            };
            main.push((key, b.command.clone(), Some(b.desc.clone())));
        }
        let descs = pinned
            .bind
            .iter()
            .map(|b| (b.command.clone(), b.desc.clone()))
            .collect();
        Ok(Keytab {
            main,
            binds: Vec::new(),
            escape: Some((1, b'a')),
            descs,
            left_out: 0,
            read: BTreeSet::new(),
        })
    }

    /// Reads one rc file's lines in order. A file is read once; one that can't be read is a note (a missing top-level
    /// file is no note: most systems have no `/etc/screenrc` or `~/.screenrc`).
    fn read(&mut self, path: &Path, sourced: bool, notes: &mut Vec<String>, env: Env) {
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !self.read.insert(key) {
            return;
        }
        let text = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if !sourced && e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                notes.push(format!("can't read {}: {e}", path.display()));
                return;
            }
        };
        for line in text.lines() {
            let words = words(line).unwrap_or_default();
            let Some((first, args)) = words.split_first() else {
                continue;
            };
            match first.as_slice() {
                b"bind" => self.bind(args),
                b"bindkey" => self.left_out += 1,
                b"escape" | b"defescape" => match args {
                    [w] if w.is_empty() => self.escape = None,
                    [w] if w.len() == 2 => self.escape = Some((w[0], w[1])),
                    _ => {}
                },
                b"source" => {
                    if let Some(file) = args.first() {
                        let file = expand(&String::from_utf8_lossy(file), env);
                        let file = match path.parent() {
                            Some(dir) if file.is_relative() => dir.join(file),
                            _ => file,
                        };
                        self.read(&file, true, notes, env);
                    }
                }
                _ => {}
            }
        }
    }

    /// `bind [-c class] key [command [args]]`: adds or replaces the key, or with no command removes it. `-k` (a
    /// termcap key) is left out.
    fn bind(&mut self, mut args: &[Vec<u8>]) {
        let mut class = None;
        loop {
            match args {
                [flag, name, rest @ ..] if flag == b"-c" && !rest.is_empty() => {
                    class = Some(String::from_utf8_lossy(name).into_owned());
                    args = rest;
                }
                [flag, ..] if flag == b"-k" => {
                    self.left_out += 1;
                    return;
                }
                _ => break,
            }
        }
        let Some((key, command)) = args.split_first() else {
            return;
        };
        let [byte] = key.as_slice() else {
            return;
        };
        let key = Key::Byte(*byte);
        let command = command
            .iter()
            .map(|w| String::from_utf8_lossy(w).into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        let desc = self
            .descs
            .iter()
            .find(|(c, _)| *c == command)
            .map(|(_, d)| d.clone());
        self.binds
            .push((class, key, (!command.is_empty()).then_some((command, desc))));
    }

    /// The sheet, and how many keys were left out (with the bindkey and termcap lines counted while reading).
    /// The defaults get `other` and `meta` on the last `escape`'s keys (as `InitKeytab` ends), then the `bind` lines
    /// are replayed over them in order.
    fn sheet(mut self) -> (Sheet, usize) {
        let mut sheet = Sheet {
            title: "screen".into(),
            provider: "screen".into(),
            binds: Default::default(),
        };
        // `escape ""`: no command key, so no command keys.
        let Some((x, y)) = self.escape else {
            return (sheet, self.left_out);
        };
        set(
            &mut self.main,
            Key::Byte(x),
            Some((
                "other".into(),
                Some("Toggle to the window displayed previously".into()),
            )),
        );
        set(
            &mut self.main,
            Key::Byte(y),
            Some((
                "meta".into(),
                Some("Send the command character to the window".into()),
            )),
        );
        let mut classes: Vec<(String, Vec<Entry>)> = Vec::new();
        for (class, key, action) in std::mem::take(&mut self.binds) {
            let table = match class {
                None => &mut self.main,
                Some(name) => {
                    let i = match classes.iter().position(|(c, _)| *c == name) {
                        Some(i) => i,
                        None => {
                            classes.push((name, Vec::new()));
                            classes.len() - 1
                        }
                    };
                    &mut classes[i].1
                }
            };
            set(table, key, action);
        }
        let mut left_out = self.left_out;
        let Some(escape) = notation(Key::Byte(x)) else {
            // No notation for the escape key itself: nothing can be sent.
            let all = self.main.len() + classes.iter().map(|(_, t)| t.len()).sum::<usize>();
            return (sheet, left_out + all);
        };
        // Each class behind the first key that enters it (`bind r command -c resize`), from the main table or from a
        // class already reached.
        let mut prefixes: Vec<(String, String)> = Vec::new();
        let enters = |table: &[Entry], prefix: &str, prefixes: &mut Vec<(String, String)>| {
            for (k, command, _) in table {
                let (Some(name), Some(k)) = (command.strip_prefix("command -c "), notation(*k))
                else {
                    continue;
                };
                if !prefixes.iter().any(|(n, _)| n == name) {
                    prefixes.push((name.to_string(), format!("{prefix} {k}")));
                }
            }
        };
        enters(&self.main, &escape, &mut prefixes);
        for _ in 0..classes.len() {
            for (name, table) in &classes {
                if let Some(prefix) = prefixes
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, p)| p.clone())
                {
                    enters(table, &prefix, &mut prefixes);
                }
            }
        }
        let mut add = |category: &str, prefix: &str, table: &[Entry]| {
            for (key, command, desc) in table {
                let Some(k) = notation(*key) else {
                    left_out += 1;
                    continue;
                };
                sheet.binds.entry(category.into()).or_default().push(Bind {
                    key: format!("{prefix} {k}"),
                    desc: desc.clone().unwrap_or_else(|| command.clone()),
                    action: command.split(' ').next().unwrap_or_default().into(),
                    when: Default::default(),
                });
            }
        };
        add("screen", &escape, &self.main);
        let mut orphans = 0;
        for (name, table) in &classes {
            match prefixes.iter().find(|(n, _)| n == name) {
                Some((_, prefix)) => add(&format!("class {name}"), prefix, table),
                None => orphans += table.len(),
            }
        }
        (sheet, left_out + orphans)
    }
}

/// Sets `key` in `table` to `action` (in place, keeping the order), or removes it with `None`.
fn set(table: &mut Vec<Entry>, key: Key, action: Option<(String, Option<String>)>) {
    let at = table.iter().position(|(k, _, _)| *k == key);
    match (at, action) {
        (Some(i), Some((c, d))) => table[i] = (key, c, d),
        (None, Some((c, d))) => table.push((key, c, d)),
        (Some(i), None) => {
            table.remove(i);
        }
        (None, None) => {}
    }
}

/// `source`'s path with `~` and `$VAR`/`${VAR}` expanded (an unset variable is empty).
fn expand(path: &str, env: Env) -> PathBuf {
    let path = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", env("HOME").unwrap_or_default())
        }
        _ => path.to_string(),
    };
    let mut out = String::new();
    let mut rest = path.as_str();
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, tail) = match after.strip_prefix('{').and_then(|a| a.split_once('}')) {
            Some((name, tail)) => (name, tail),
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        if name.is_empty() {
            out.push('$');
        } else {
            out.push_str(&env(name).unwrap_or_default());
        }
        rest = tail;
    }
    out.push_str(rest);
    PathBuf::from(out)
}

/// A screenrc line's words, as screen's `Parse` splits them: whitespace between words, `"…"` and `'…'` quoting,
/// `\n \r \t \' \" \\ \$ \# \^` and `\ooo` (octal) escapes and `^x` (control characters, `^?` is DEL) outside single
/// quotes; `#` at the start of a word ends the line. `!cmd` is `exec cmd`. `$VAR` is left as is (only `source` paths
/// expand). `None` for a line screen drops (a quote left open).
fn words(line: &str) -> Option<Vec<Vec<u8>>> {
    let b = line.as_bytes();
    let mut words = Vec::new();
    let mut i = 0;
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if words.is_empty() && b.get(i) == Some(&b'!') {
            words.push(b"exec".to_vec());
            i += 1;
            continue;
        }
        if i >= b.len() || b[i] == b'#' || b[i] == b'\n' {
            return Some(words);
        }
        let mut word = Vec::new();
        let mut delim = 0;
        while i < b.len() {
            let c = b[i];
            let next = b.get(i + 1).copied();
            if c == delim {
                delim = 0;
            } else if delim != b'\''
                && c == b'\\'
                && next.is_some_and(|n| b"nrt'\"\\$#^01234567".contains(&n))
            {
                i += 1;
                match b[i] {
                    d @ b'0'..=b'7' => {
                        let mut v = u32::from(d - b'0');
                        for _ in 0..2 {
                            match b.get(i + 1) {
                                Some(d @ b'0'..=b'7') => {
                                    v = v << 3 | u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        word.push(v as u8);
                    }
                    b'n' => word.push(b'\n'),
                    b'r' => word.push(b'\r'),
                    b't' => word.push(b'\t'),
                    c => word.push(c),
                }
            } else if delim != b'\'' && c == b'^' && next.is_some() {
                i += 1;
                word.push(if b[i] == b'?' { 0o177 } else { b[i] & 0x1f });
            } else if delim == 0 && (c == b'\'' || c == b'"') {
                delim = c;
            } else if delim == 0 && (c == b' ' || c == b'\t') {
                break;
            } else {
                word.push(c);
            }
            i += 1;
        }
        // screen drops a line with a quote left open.
        if delim != 0 {
            return None;
        }
        words.push(word);
    }
}

/// A key after the escape in the notation; `None` for one it can't spell (a byte above 127).
fn notation(key: Key) -> Option<String> {
    let Key::Byte(b) = key else {
        return Some("shift+Tab".into());
    };
    Some(match b {
        0 => "ctrl+space".into(),
        9 => "Tab".into(),
        27 => "Escape".into(),
        127 => "BackSpace".into(),
        1..=26 => chord(&["ctrl"], &char::from(b'a' + b - 1).to_string()),
        28 => "ctrl+backslash".into(),
        29 => "ctrl+bracketright".into(),
        30 => "ctrl+asciicircum".into(),
        31 => "ctrl+underscore".into(),
        b'A'..=b'Z' => chord(&["shift"], &char::from(b.to_ascii_lowercase()).to_string()),
        32..=126 => single(char::from(b))?.to_string(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(format!(
            "{}/tests/fixtures/screen/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
    }

    /// Each category's rows as `key = desc`.
    fn rows(sheet: &Sheet, category: &str) -> Vec<String> {
        sheet
            .binds
            .get(category)
            .map(|b| {
                b.iter()
                    .map(|b| format!("{} = {}", b.key, b.desc))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn has(rows: &[String], row: &str) -> bool {
        rows.iter().any(|r| r == row)
    }

    #[test]
    fn pinned_files_parse_and_every_key_translates() {
        for (v, text) in REGISTRY {
            assert!(text.contains(&format!("screen_version = \"{v}\"")), "{v}");
            assert!(
                text.contains(&format!("https://ftp.gnu.org/gnu/screen/screen-{v}.tar.gz")),
                "{v}"
            );
            let pinned: Pinned = toml::from_str(text).unwrap();
            let rc = Keytab::new(&pinned).unwrap();
            for (key, command, _) in &rc.main {
                assert!(notation(*key).is_some(), "{v}: {key:?} {command}");
            }
            let (sheet, left_out) = rc.sheet();
            assert_eq!(left_out, 0, "{v}");
            let rows = rows(&sheet, "screen");
            assert!(has(
                &rows,
                "ctrl+a c = Create a new window with a shell and switch to that window"
            ));
            assert!(has(
                &rows,
                "ctrl+a ctrl+a = Toggle to the window displayed previously"
            ));
            assert!(has(
                &rows,
                "ctrl+a a = Send the command character to the window"
            ));
            assert!(has(
                &rows,
                "ctrl+a shift+Tab = Switch the input focus to the previous region"
            ));
            assert!(has(
                &rows,
                "ctrl+a Tab = Switch the input focus to the next region"
            ));
            assert!(has(
                &rows,
                "ctrl+a BackSpace = Switch to the previous window (opposite of C-a n)"
            ));
        }
        let keys = |v: &str| {
            let text = REGISTRY.iter().find(|(r, _)| *r == v).unwrap().1;
            let pinned: Pinned = toml::from_str(text).unwrap();
            pinned
                .bind
                .into_iter()
                .map(|b| format!("{} {}", b.key, b.command))
                .collect::<Vec<_>>()
        };
        let old = keys("4.9.1");
        let new = keys("5.0.2");
        assert!(old.contains(&"t time".into()) && !old.iter().any(|k| k.starts_with("u ")));
        assert!(new.contains(&"u parent".into()) && !new.iter().any(|k| k.starts_with("t ")));
        assert!(new.contains(&"k kill --confirm".into()) && old.contains(&"k kill".into()));
    }

    #[test]
    fn pinned_rows_match_init_keytab() {
        // Spot checks against `InitKeytab()`, keys whose spelling the generator could get wrong.
        for (v, text) in REGISTRY {
            let pinned: Pinned = toml::from_str(text).unwrap();
            let cmd = |k: &str| {
                let hits: Vec<&str> = pinned
                    .bind
                    .iter()
                    .filter(|b| b.key == k)
                    .map(|b| b.command.as_str())
                    .collect();
                assert!(hits.len() <= 1, "{v}: {k:?} twice");
                hits.first().copied()
            };
            for (key, want) in [
                (" ", "next"),
                ("^@", "next"),
                ("{", "history"),
                ("}", "history"),
                ("^I", "focus"),
                ("i", "info"),
                ("backtab", "focus prev"),
                (
                    "^\\",
                    if *v == "4.9.1" {
                        "quit"
                    } else {
                        "quit --confirm"
                    },
                ),
                ("\"", "windowlist -b"),
                ("'", "select"),
                ("|", "split -v"),
                ("9", "select 9"),
                ("L", "login"),
            ] {
                assert_eq!(cmd(key), Some(want), "{v}: {key:?}");
            }
            // The escape keys get `other` and `meta` from the reader, not the data.
            assert_eq!((cmd("^A"), cmd("a")), (None, None), "{v}");
            assert_eq!(pinned.bind.len(), 87, "{v}");
        }
    }

    #[test]
    fn version_pick() {
        let pick = |text: &str| {
            let (v, _, note) = select(REGISTRY, version_of(text).as_deref()).unwrap();
            (v, note.is_some())
        };
        assert_eq!(
            pick("Screen version 4.09.01 (GNU) 20-Aug-23"),
            ("4.9.1", false)
        );
        assert_eq!(
            pick("Screen version 4.09.00 (GNU) 30-Jan-22"),
            ("4.9.1", false)
        );
        assert_eq!(
            pick("Screen version 4.06.02 (GNU) 23-Oct-17"),
            ("4.9.1", true)
        );
        assert_eq!(
            pick("Screen version 5.00.01 (build on x)"),
            ("5.0.2", false)
        );
        assert_eq!(
            pick("Screen version 5.0.1 (build on 2025-05-01)"),
            ("5.0.2", false)
        );
        assert_eq!(
            pick("Screen version 5.0.2 (build on 2025-05-01)"),
            ("5.0.2", false)
        );
        assert_eq!(pick("Screen version 6.0.0"), ("5.0.2", true));
        assert_eq!(pick("garbage"), ("5.0.2", true));
        // A build suffix doesn't hide the version.
        assert_eq!(
            pick("Screen version 4.09.01beta (GNU) 20-Aug-23"),
            ("4.9.1", false)
        );
        assert_eq!(
            pick("Screen version 5.0.2-dirty (build on x)"),
            ("5.0.2", false)
        );
        let (_, _, note) = select(REGISTRY, Some("4.6.2")).unwrap();
        assert!(note.unwrap().contains("data/screen/4.6.2.toml"));
        assert_eq!(
            version_of("Screen version 4.09.01 (GNU) 20-Aug-23").as_deref(),
            Some("4.9.1")
        );
    }

    /// An environment of `vars`.
    fn env<'a>(vars: &'a [(&'a str, String)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
    }

    fn v5() -> anyhow::Result<String> {
        Ok("Screen version 5.0.2 (build on 2025-05-01)\n".into())
    }

    #[test]
    fn screenrc_over_defaults() {
        let home = fixture("home").to_string_lossy().into_owned();
        let vars = [
            ("HOME", home),
            (
                "SCREENRC",
                fixture("screenrc").to_string_lossy().into_owned(),
            ),
        ];
        let (sheet, notes) =
            sheet_with(REGISTRY, v5(), &fixture("etcscreenrc-5.0.2"), &env(&vars)).unwrap();
        let main = rows(&sheet, "screen");
        // `escape ^Bb`: every key behind ctrl+b; `b` and `^B` become meta and other.
        assert!(main.iter().all(|r| r.starts_with("ctrl+b ")), "{main:#?}");
        assert!(has(
            &main,
            "ctrl+b ctrl+b = Toggle to the window displayed previously"
        ));
        assert!(has(
            &main,
            "ctrl+b b = Send the command character to the window"
        ));
        // `^A` and `a` are back to nothing.
        assert!(!main
            .iter()
            .any(|r| r.starts_with("ctrl+b ctrl+a ") || r.starts_with("ctrl+b a ")));
        // /etc/screenrc is upstream's `etc/etcscreenrc` from https://ftp.gnu.org/gnu/screen/screen-5.0.2.tar.gz:
        // `bind ^k` and `bind ^\` remove, `bind \\ quit` and `bind K kill` add.
        assert!(!main
            .iter()
            .any(|r| r.starts_with("ctrl+b ctrl+k ") || r.starts_with("ctrl+b ctrl+backslash ")));
        // A command that isn't a pinned default as written (5.0.2's is `quit --confirm`) shows as it is.
        assert!(has(&main, "ctrl+b backslash = quit"));
        assert!(has(&main, "ctrl+b shift+k = kill"));
        assert!(has(&main, "ctrl+b shift+i = login on"));
        // The user file: a replaced key shows its command (quotes kept as one word), a bare bind removes.
        assert!(has(&main, "ctrl+b c = screen -t my shell 1"));
        assert!(!main.iter().any(|r| r.starts_with("ctrl+b x ")));
        assert!(!main.iter().any(|r| r.starts_with("ctrl+b shift+n ")));
        // A comment after a command; an octal key.
        assert!(has(&main, "ctrl+b question = Show key bindings"));
        assert!(has(&main, "ctrl+b Escape = Show a list of window"));
        // `source`: relative to the including file, `~`, a loop read once.
        assert!(has(
            &main,
            "ctrl+b shift+z = Reset the virtual terminal to its power-on values"
        ));
        assert!(has(
            &main,
            "ctrl+b shift+v = Display the version and compilation date"
        ));
        // A class entered by `bind r command -c resize`; the one nothing enters is counted.
        assert_eq!(
            rows(&sheet, "class resize"),
            ["ctrl+b r 1 = resize +1", "ctrl+b r 2 = resize -1"]
        );
        assert!(!sheet.binds.contains_key("class orphan"));
        // Notes: a missing source; left out: bindkey, bind -k, the orphan class's key.
        assert!(
            notes.iter().any(|n| n.contains("missing.screenrc")),
            "{notes:?}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("leaving out 3 bindings")),
            "{notes:?}"
        );
        assert_eq!(notes.len(), 2, "{notes:?}");
    }

    #[test]
    fn user_file_and_failures() {
        // `$SCREENRC` beats `~/.screenrc`; without it, `~/.screenrc`.
        let home = fixture("home").to_string_lossy().into_owned();
        let vars = [("HOME", home.clone())];
        let none = Path::new("/nonexistent/screenrc");
        let (sheet, notes) = sheet_with(REGISTRY, v5(), none, &env(&vars)).unwrap();
        assert!(has(&rows(&sheet, "screen"), "ctrl+a shift+y = stuff yank"));
        assert!(notes.is_empty(), "{notes:?}");
        let vars = [("HOME", home.clone()), ("SCREENRC", String::new())];
        let (sheet, _) = sheet_with(REGISTRY, v5(), none, &env(&vars)).unwrap();
        assert!(has(&rows(&sheet, "screen"), "ctrl+a shift+y = stuff yank"));
        let vars = [
            ("HOME", home),
            (
                "SCREENRC",
                fixture("home/home.screenrc").to_string_lossy().into_owned(),
            ),
        ];
        let (sheet, _) = sheet_with(REGISTRY, v5(), none, &env(&vars)).unwrap();
        assert!(!has(&rows(&sheet, "screen"), "ctrl+a shift+y = stuff yank"));
        // No screen: the newest defaults, with the reason as a note; an unreadable file is a note.
        let (sheet, notes) = sheet_with(
            REGISTRY,
            Err(anyhow::anyhow!("running screen --version: not found")),
            fixture("home").as_path(),
            &|_| None,
        )
        .unwrap();
        assert!(has(
            &rows(&sheet, "screen"),
            "ctrl+a u = Switch to the current window's parent window"
        ));
        assert!(notes[0].contains("not found"), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("unknown")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("can't read")), "{notes:?}");
        // A malformed pinned file drops the provider.
        let bad = [("5.0.2", "bind = 3")];
        assert!(sheet_with(&bad, v5(), none, &|_| None).is_err());
    }

    #[test]
    fn defescape_and_line_syntax() {
        let dir = std::env::temp_dir().join(format!("kbf-screen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rc = dir.join("rc");
        std::fs::write(
            &rc,
            "defescape `e\nbind 'q' \"stuff x\"\n  # all comment\n\nbind ^[ windows\n",
        )
        .unwrap();
        let path = rc.to_string_lossy().into_owned();
        let vars = [("SCREENRC", path)];
        let result = sheet_with(REGISTRY, v5(), Path::new("/nonexistent"), &env(&vars));
        let _ = std::fs::remove_dir_all(&dir);
        let (sheet, _) = result.unwrap();
        let main = rows(&sheet, "screen");
        assert!(
            has(
                &main,
                "grave grave = Toggle to the window displayed previously"
            ),
            "{main:#?}"
        );
        assert!(has(&main, "grave q = stuff x"));
        assert!(has(
            &main,
            "grave e = Send the command character to the window"
        ));
        assert!(has(&main, "grave Escape = Show a list of window"));
        // `^A` lost `other`; nothing else was bound to it.
        assert!(!main.iter().any(|r| r.starts_with("grave ctrl+a ")));
    }

    /// The sheet from the defaults and `rc` (one user file).
    fn with_rc(rc: &str) -> (Sheet, Vec<String>) {
        let dir =
            std::env::temp_dir().join(format!("kbf-screen-rc-{}-{}", std::process::id(), rc.len()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rc"), rc).unwrap();
        let vars = [("SCREENRC", dir.join("rc").to_string_lossy().into_owned())];
        let result = sheet_with(REGISTRY, v5(), Path::new("/nonexistent"), &env(&vars));
        let _ = std::fs::remove_dir_all(&dir);
        result.unwrap()
    }

    #[test]
    fn binds_after_escape_win() {
        // `escape` sets the defaults' `other`/`meta` keys; a later `bind` replaces them, and so does an earlier one.
        let (sheet, _) = with_rc("escape ^Bb\nbind ^B windows\n");
        let main = rows(&sheet, "screen");
        assert!(
            has(&main, "ctrl+b ctrl+b = Show a list of window"),
            "{main:#?}"
        );
        assert!(has(
            &main,
            "ctrl+b b = Send the command character to the window"
        ));
        let (sheet, _) = with_rc("bind ^A\n");
        assert!(!rows(&sheet, "screen")
            .iter()
            .any(|r| r.starts_with("ctrl+a ctrl+a ")));
        // Only the last escape counts.
        let (sheet, _) = with_rc("escape ^Bb\nescape ^Tt\n");
        let main = rows(&sheet, "screen");
        assert!(
            has(&main, "ctrl+t ctrl+b = Send a break to window"),
            "{main:#?}"
        );
        assert!(has(
            &main,
            "ctrl+t ctrl+t = Toggle to the window displayed previously"
        ));
        // `escape ""`: no command key, so nothing is listed; a malformed escape changes nothing.
        let (sheet, _) = with_rc("escape \"\"\n");
        assert!(sheet.binds.is_empty());
        let (sheet, _) = with_rc("escape ^B\nescape abc\n");
        assert!(has(
            &rows(&sheet, "screen"),
            "ctrl+a c = Create a new window with a shell and switch to that window"
        ));
        // An escape key with no notation: every key left out.
        let (sheet, notes) = with_rc("escape \\351x\n");
        assert!(sheet.binds.is_empty());
        assert!(
            notes.iter().any(|n| n.starts_with("leaving out 8")),
            "{notes:?}"
        );
    }

    #[test]
    fn classes_reached_through_classes_and_dropped_lines() {
        let (sheet, notes) = with_rc(
            "bind r command -c a\nbind -c a x command -c b\nbind -c b 1 resize +1\nbind q stuff \"open\n",
        );
        assert_eq!(rows(&sheet, "class b"), ["ctrl+a r x 1 = resize +1"]);
        // The line with the open quote is dropped, as screen drops it: `q` keeps its default.
        assert!(has(
            &rows(&sheet, "screen"),
            "ctrl+a q = Send a control-q to the current window"
        ));
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn source_paths_expand() {
        let env = |k: &str| match k {
            "HOME" => Some("/h".to_string()),
            "D" => Some("/d".to_string()),
            _ => None,
        };
        assert_eq!(expand("~/x", &env), PathBuf::from("/h/x"));
        assert_eq!(expand("~", &env), PathBuf::from("/h"));
        assert_eq!(expand("$D/x", &env), PathBuf::from("/d/x"));
        assert_eq!(expand("${D}x", &env), PathBuf::from("/dx"));
        assert_eq!(expand("$UNSET/x", &env), PathBuf::from("/x"));
        assert_eq!(expand("a$", &env), PathBuf::from("a$"));
        assert_eq!(expand("~user/x", &env), PathBuf::from("~user/x"));
    }

    #[test]
    fn words_as_screen_parses_them() {
        let w = |l: &str| {
            words(l)
                .unwrap()
                .into_iter()
                .map(|w| String::from_utf8_lossy(&w).into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            w("bind c screen -t \"a b\" 1"),
            ["bind", "c", "screen", "-t", "a b", "1"]
        );
        assert_eq!(w("bind ^A other # x"), ["bind", "\u{1}", "other"]);
        assert_eq!(w("bind \\033 x"), ["bind", "\u{1b}", "x"]);
        assert_eq!(w("bind \\\\ x"), ["bind", "\\", "x"]);
        assert_eq!(w("bind \\^ x"), ["bind", "^", "x"]);
        assert_eq!(w("bind ^? x"), ["bind", "\u{7f}", "x"]);
        assert_eq!(w("bind '^A' x"), ["bind", "^A", "x"]);
        assert_eq!(w("a#b"), ["a#b"]);
        assert_eq!(w("!ls -l"), ["exec", "ls", "-l"]);
        assert!(w("   ").is_empty());
        assert_eq!(words("bind x \"open"), None);
        assert_eq!(words("bind x 'open"), None);
    }

    #[test]
    fn key_notation() {
        let n = |l: &str| {
            let w = words(l).unwrap();
            notation(Key::Byte(w[0][0]))
        };
        for (spelled, want) in [
            ("c", "c"),
            ("N", "shift+n"),
            ("^A", "ctrl+a"),
            ("^[", "Escape"),
            ("^?", "BackSpace"),
            ("\\177", "BackSpace"),
            ("\\033", "Escape"),
            ("\\\\", "backslash"),
            ("'\"'", "quotedbl"),
            ("\"'\"", "apostrophe"),
            ("' '", "space"),
            ("|", "bar"),
            ("-", "minus"),
            ("\\t", "Tab"),
            ("^I", "Tab"),
            ("^@", "ctrl+space"),
        ] {
            assert_eq!(n(spelled).as_deref(), Some(want), "{spelled}");
        }
        assert_eq!(notation(Key::Backtab).as_deref(), Some("shift+Tab"));
        assert_eq!(notation(Key::Byte(0o351)), None);
    }

    #[test]
    fn untranslatable_key_is_counted() {
        let pinned: Pinned = toml::from_str(REGISTRY[1].1).unwrap();
        let mut rc = Keytab::new(&pinned).unwrap();
        rc.bind(&[vec![0o351], b"windows".to_vec()]);
        let (_, left_out) = rc.sheet();
        assert_eq!(left_out, 1);
    }

    #[test]
    fn real_screen_version() {
        let mut cmd = Command::new("screen");
        cmd.arg("--version")
            .env("LC_ALL", "C")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
        let Ok(out) = run_for(cmd, VERSION_TIMEOUT) else {
            eprintln!("skipping: screen is not installed");
            return;
        };
        assert!(version_of(&out).is_some(), "{out:?}");
    }
}
