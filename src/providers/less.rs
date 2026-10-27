//! less: its keys from `less --help`, with the `#command` section of the user's lesskey source file laid over them.
//! Shown when less is anywhere in the terminal's foreground process group (`man`, `git log`, `systemctl status`).

use crate::context::run_for;
use crate::sheet::{chord, Bind, Name, Sheet};
use anyhow::{bail, Context as _};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

/// less 582 reads lesskey source files itself; before, `lesskey` compiled them into a binary `~/.less`.
const READS_SOURCE: u32 = 582;

/// `--help`'s key column in the command sections: two spaces of indent, then keys up to this column.
const KEY_COLUMN: usize = 20;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet() -> anyhow::Result<Sheet> {
    let (sheet, notes) = read(TIMEOUT, |_| {}, &|k| std::env::var(k).ok())?;
    for note in notes {
        eprintln!("keybind-find: less: {note}");
    }
    Ok(sheet)
}

/// Runs `less --version` and `less --help` with `setup` applied (a test seam: `PATH`, `LESS`), and lays the lesskey
/// file found through `env` over the help's keys. Also returns the lines for stderr.
fn read(
    timeout: Duration,
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let run = |arg: &str| {
        let mut cmd = Command::new("less");
        cmd.arg(arg);
        setup(&mut cmd);
        run_for(cmd, timeout).with_context(|| format!("running less {arg}"))
    };
    let version = version(&run("--version")?)?;
    let (mut sheet, mut skipped, _) = help(&strip_overstrike(&run("--help")?));
    if sheet.binds.is_empty() {
        bail!("less --help listed no keys keybind-find can read");
    }
    let mut notes = Vec::new();
    let path = lesskey_path(env);
    // A compiled lesskey file (`lesskey`'s output, which every less reads) is never parsed: only said, to someone
    // who has one.
    let compiled = env("LESSKEY")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| home(env).map(|h| h.join(".less")))
        .filter(|p| p.is_file());
    if version < READS_SOURCE {
        if compiled.is_some() || path.is_some() {
            notes.push(format!(
                "less {version} reads only a compiled lesskey file, which isn't read here; showing its defaults \
                 (less {READS_SOURCE} or newer reads the lesskey source file)"
            ));
        }
    } else if let (None, Some(c)) = (&path, &compiled) {
        notes.push(format!(
            "{} is a compiled lesskey file, which isn't read; showing less's defaults (a lesskey source file is)",
            c.display()
        ));
    }
    if let (true, Some(path)) = (version >= READS_SOURCE, path) {
        // Lossily: one non-UTF-8 byte (a Latin-1 comment) mustn't lose every other key.
        match std::fs::read(&path) {
            Ok(text) => skipped += overlay(&mut sheet, &String::from_utf8_lossy(&text)),
            Err(e) => notes.push(format!(
                "can't read {}: {e}; showing less's defaults",
                path.display()
            )),
        }
    }
    if skipped > 0 {
        let keys = if skipped == 1 { "key" } else { "keys" };
        notes.push(format!("skipping {skipped} {keys} with no keysym we know"));
    }
    Ok((sheet, notes))
}

/// `$HOME`, unless unset or empty.
fn home(env: Env) -> Option<PathBuf> {
    env("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// The number after `less ` on `less --version`'s first line (`less 668 (PCRE2 regular expressions)`).
fn version(text: &str) -> anyhow::Result<u32> {
    let line = text.lines().next().unwrap_or_default();
    line.strip_prefix("less ")
        .map(|r| {
            r.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("less --version printed {line:?}, not a version"))
}

/// The lesskey source file less reads: `$LESSKEYIN` if set, else the first of `$XDG_CONFIG_HOME/lesskey`,
/// `~/.config/lesskey`, `~/.lesskey` that exists. `None`: no user keys.
fn lesskey_path(env: Env) -> Option<PathBuf> {
    if let Some(p) = env("LESSKEYIN").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    // An empty variable is unset, as less takes it.
    let home = home(env);
    let xdg = env("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(|d| PathBuf::from(d).join("lesskey"));
    let in_home = [".config/lesskey", ".lesskey"].map(|f| home.as_ref().map(|h| h.join(f)));
    [xdg]
        .into_iter()
        .chain(in_home)
        .flatten()
        .find(|p| p.is_file())
}

/// less's help with the terminal's overstrike undone: `c` backspace `c` (bold) and `_` backspace `c` (underline)
/// are both `c`. A backspace at a line's start has nothing to undo.
fn strip_overstrike(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c != '\u{8}' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('\n') {
            out.pop();
        }
    }
    out
}

/// The help's sections as categories (`MOVING` → `Moving`), one bind per key on each line, every bind of a line
/// with the line's description; `LINE EDITING` binds carry `mode=line-editing`, `OPTIONS` is left out. Also
/// returns the keys skipped, and each line that gave no bind with why (for the test that no line is lost).
fn help(text: &str) -> (Sheet, usize, Vec<(&'static str, &str)>) {
    let mut sheet = Sheet {
        title: "less".into(),
        provider: "less".into(),
        binds: Default::default(),
    };
    let (mut skipped, mut unused, mut section) = (0, Vec::new(), String::new());
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if indent >= 8 && trimmed.chars().all(|c| c.is_ascii_uppercase() || c == ' ') {
            section = trimmed.to_string();
            unused.push(("heading", line));
            continue;
        }
        if trimmed.starts_with("---") {
            unused.push(("divider", line));
            continue;
        }
        let line_edit = section == "LINE EDITING";
        if section == "OPTIONS" {
            unused.push(("options", line));
            continue;
        }
        let columns = match (line_edit, indent) {
            (true, 1) => Some(line_edit_columns(line)),
            (false, 2) => line.get(..KEY_COLUMN).zip(line.get(KEY_COLUMN..)),
            // The notes under a section's divider, and its introduction.
            _ => {
                unused.push(("note", line));
                continue;
            }
        };
        // A line too short for the description column, or cut inside a character: not a key line we know.
        let Some((keys, desc)) = columns else {
            unused.push(("no key", line));
            continue;
        };
        // `*`: a count N may be typed first, which the description already says.
        let desc = desc.trim().trim_start_matches('*');
        let desc = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        if desc.is_empty() {
            unused.push(("no key", line));
            continue;
        }
        let tokens: Vec<&str> = if line_edit {
            keys.split_whitespace()
                .filter(|t| !t.chars().all(|c| c == '.'))
                .collect()
        } else {
            // Keys are two spaces apart; after one space come the key's arguments (`s file`, `ESC-^F <c1> <c2>`).
            keys.split("  ")
                .filter_map(|t| t.split_whitespace().next())
                .collect()
        };
        let mut any = false;
        for token in tokens {
            let Some(key) = help_key(strip_argument(token)) else {
                skipped += 1;
                continue;
            };
            let mut b = Bind {
                key,
                desc: desc.clone(),
                action: String::new(),
                when: Default::default(),
            };
            if line_edit {
                if let (Ok(scope), Ok(mode)) =
                    ("mode".parse::<Name>(), "line-editing".parse::<Name>())
                {
                    b.when.insert(scope, mode.into());
                }
            }
            push(&mut sheet, &sentence_case(&section), b);
            any = true;
        }
        if !any {
            unused.push(("no key", line));
        }
    }
    (sheet, skipped, unused)
}

/// A `LINE EDITING` line (` RightArrow ....... ESC-l ... Move cursor right one character.`): the keys, and the
/// description after the last dot leader (or, with none, after the last run of three spaces).
fn line_edit_columns(line: &str) -> (&str, &str) {
    let at = [("... ", 4), ("   ", 3)]
        .iter()
        .filter_map(|(sep, n)| line.rfind(sep).map(|i| i + n))
        .max()
        .unwrap_or(line.len());
    // `ESC (MS-DOS only)` deletes the line only on MS-DOS.
    let keys = line[..at]
        .split(" ESC (MS-DOS only)")
        .next()
        .unwrap_or_default();
    (keys, &line[at..])
}

/// A help key without its placeholder: `/pattern` → `/`, `m<letter>` → `m`, `||XYcommand` → `||`.
fn strip_argument(token: &str) -> &str {
    const ARGS: [&str; 8] = [
        "XYcommand",
        "Xcommand",
        "command",
        "pattern",
        "cmd",
        "<letter>",
        "<flag>",
        "<name>",
    ];
    ARGS.iter()
        .find_map(|a| token.strip_suffix(a).filter(|k| !k.is_empty()))
        .unwrap_or(token)
}

/// A key in `--help`'s notation as chords: `^F` → `ctrl+f`, `ESC-v` → `alt+v`, `^X^X` → `ctrl+x ctrl+x`, `:q` →
/// `colon q`, `ESC-RArrow` → `alt+Right`, and the line editor's `ctrl-RightArrow`, `SHIFT-TAB`.
fn help_key(token: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut rest = token;
    while !rest.is_empty() {
        let (key, used) = help_unit(rest)?;
        out.push(key);
        rest = &rest[used..];
    }
    (!out.is_empty()).then(|| out.join(" "))
}

/// The first chord in `s` and the bytes it took. A name (`SPACE`, `RightArrow`) is the whole rest of the token.
fn help_unit(s: &str) -> Option<(String, usize)> {
    for (prefix, m) in [
        ("ESC-", "alt"),
        ("ctrl-", "ctrl"),
        ("SHIFT-", "shift"),
        ("^", "ctrl"),
    ] {
        if let Some(r) = s.strip_prefix(prefix).filter(|r| !r.is_empty()) {
            let (inner, used) = help_unit(r)?;
            return Some((with_mod(m, &inner), prefix.len() + used));
        }
    }
    let named = match s {
        "SPACE" => "space",
        "CR" => "Return",
        "HOME" => "Home",
        "END" => "End",
        "RightArrow" | "RArrow" => "Right",
        "LeftArrow" | "LArrow" => "Left",
        "UpArrow" => "Up",
        "DownArrow" => "Down",
        "BACKSPACE" => "BackSpace",
        "DELETE" => "Delete",
        "TAB" => "Tab",
        "ESC" => "Escape",
        _ => "",
    };
    if !named.is_empty() {
        return Some((named.into(), s.len()));
    }
    let c = s.chars().next()?;
    Some((super::nvim::single(c)?.into(), c.len_utf8()))
}

/// `chord` with modifier `m` added; ctrl takes the lowercase letter (`^F` is ctrl+f).
fn with_mod(m: &str, key: &str) -> String {
    let mut parts: Vec<&str> = key.split('+').collect();
    let sym = parts.pop().unwrap_or_default().to_string();
    let sym = if m == "ctrl" && sym.len() == 1 {
        sym.to_ascii_lowercase()
    } else {
        sym
    };
    parts.push(m);
    chord(&parts, &sym)
}

/// `SEARCHING` → `Searching`.
fn sentence_case(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut c = lower.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Adds a bind unless the same key and description is already in the category.
fn push(sheet: &mut Sheet, category: &str, bind: Bind) {
    let list = sheet.binds.entry(category.into()).or_default();
    if !list.contains(&bind) {
        list.push(bind);
    }
}

/// Lays a lesskey source file's `#command` section over the defaults (the line editor's are left alone): a key
/// bound there replaces the default for that key, in category `lesskey`; `invalid`, or `noaction` with no extra
/// string, unbinds it; `#stop` drops every default command. `#line-edit` and `#env` are ignored, and so are
/// comments and `#version` lines. Returns the keys skipped.
/// ponytail: `#version` conditions aren't evaluated; read them if someone relies on one.
fn overlay(sheet: &mut Sheet, text: &str) -> usize {
    let (mut command, mut stop, mut skipped, mut user) = (true, false, 0, Vec::new());
    for line in text.lines() {
        let t = line.trim();
        match t {
            "#command" => command = true,
            "#line-edit" | "#env" => command = false,
            "#stop" if command => stop = true,
            _ if !command || t.is_empty() || t.starts_with('#') => {}
            _ => match lesskey_line(t) {
                Some((Some(key), action, extra)) => user.push((key, action, extra)),
                Some((None, ..)) => skipped += 1,
                None => {}
            },
        }
    }
    let defaults = |c: &str| c != "Line editing" && c != "lesskey";
    if stop {
        sheet.binds.retain(|c, _| !defaults(c));
    }
    for (key, action, extra) in user {
        for (_, list) in sheet.binds.iter_mut().filter(|(c, _)| defaults(c)) {
            list.retain(|b| b.key != key);
        }
        sheet.binds.retain(|_, list| !list.is_empty());
        let list = sheet.binds.entry("lesskey".into()).or_default();
        list.retain(|b| b.key != key);
        if action == "invalid" || (action == "noaction" && extra.is_empty()) {
            continue;
        }
        let desc = if extra.is_empty() {
            action.clone()
        } else {
            format!("{action} {extra}")
        };
        list.push(Bind {
            key,
            desc,
            action,
            when: Default::default(),
        });
    }
    sheet.binds.retain(|_, list| !list.is_empty());
    skipped
}

/// A `#command` line, `string action [extra]`: the key as chords (`None` if a part has no keysym), the action and
/// the extra string as written. `None` for a line with no action.
fn lesskey_line(line: &str) -> Option<(Option<String>, String, String)> {
    let mut chars = line.chars().peekable();
    let (mut keys, mut ok, mut alt) = (Vec::new(), true, false);
    while let Some(c) = chars.next() {
        let key: Option<(Vec<&str>, String)> = match c {
            ' ' | '\t' => break,
            '#' => return None,
            // `^[` is ESC, as `\e` is. ESC ESC x is Escape, then alt+x.
            '^' if chars.peek() == Some(&'[')
                && !matches!(chars.clone().nth(1), None | Some(' ' | '\t')) =>
            {
                chars.next();
                if std::mem::replace(&mut alt, true) {
                    keys.push("Escape".into());
                }
                continue;
            }
            // less's `CONTROL(c)`: `c & 037`, so `^?` is ctrl+_. Only before an ASCII character: less works on
            // bytes, and a space ends the key.
            '^' => match chars.peek().copied() {
                Some(c) if c.is_ascii_graphic() => {
                    chars.next();
                    control(c.to_ascii_uppercase() as u32 & 0x1f)
                }
                _ => None,
            },
            '\\' => match chars.next() {
                Some('e') => {
                    // ESC then a key is that key with alt; a lone ESC is Escape.
                    if matches!(chars.peek(), None | Some(' ' | '\t')) {
                        Some((vec![], "Escape".into()))
                    } else {
                        if std::mem::replace(&mut alt, true) {
                            keys.push("Escape".into());
                        }
                        continue;
                    }
                }
                Some('b') => control(0x08),
                Some('n') => control(0x0a),
                Some('r') => control(0x0d),
                Some('t') => control(0x09),
                Some('k') => k_key(&mut chars),
                Some(d @ '0'..='7') => {
                    let mut n = d.to_digit(8).unwrap_or_default();
                    for _ in 0..2 {
                        match chars.peek().and_then(|c| c.to_digit(8)) {
                            Some(v) => {
                                n = n * 8 + v;
                                chars.next();
                            }
                            None => break,
                        }
                    }
                    control(n)
                }
                Some(c) => plain(c),
                None => None,
            },
            c => plain(c),
        };
        match key {
            Some((mut mods, sym)) => {
                if std::mem::take(&mut alt) {
                    mods.push("alt");
                }
                keys.push(chord(&mods, &sym));
            }
            None => ok = false,
        }
    }
    let rest: String = chars.collect();
    // Text after an unescaped `#` is a comment.
    let (mut end, mut escaped) = (rest.len(), false);
    for (i, c) in rest.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '#' => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let mut words = rest[..end].trim().splitn(2, [' ', '\t']);
    let action = words.next().filter(|a| !a.is_empty())?.to_string();
    let extra = words.next().unwrap_or_default().trim().to_string();
    let key = (ok && !keys.is_empty()).then(|| keys.join(" "));
    Some((key, action, extra))
}

/// A character as a chord: printable ones by keysym, control characters as ctrl (`\b`, 0x08, is ctrl+h), except
/// Tab, Return, Escape and DEL (BackSpace) by name.
fn control(n: u32) -> Option<(Vec<&'static str>, String)> {
    match n {
        0x09 => Some((vec![], "Tab".into())),
        0x0d => Some((vec![], "Return".into())),
        0x7f => Some((vec![], "BackSpace".into())),
        0x1b => Some((vec![], "Escape".into())),
        0 => Some((vec!["ctrl"], "space".into())),
        // `^]` is 0x1d: ctrl and `]`, as the help's `^]` reads.
        1..0x20 => Some((
            vec!["ctrl"],
            super::nvim::single(char::from_u32(n + 0x40)?.to_ascii_lowercase())?.into(),
        )),
        _ => plain(char::from_u32(n)?),
    }
}

fn plain(c: char) -> Option<(Vec<&'static str>, String)> {
    Some((vec![], super::nvim::single(c)?.into()))
}

/// The key after `\k`: one letter (`\kd` Down, `\kD` Page Down, `\kL` ctrl+Left), or `^` or `+` and a letter
/// (`\k^d` ctrl+Down, `\k+d` shift+Down). The keypad (`\kp1`) has no keysym we read.
fn k_key(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<(Vec<&'static str>, String)> {
    let base = |c: char| {
        Some(match c {
            'b' => "BackSpace",
            'd' => "Down",
            'D' => "Next",
            'e' => "End",
            'h' => "Home",
            'i' => "Insert",
            'l' => "Left",
            'r' => "Right",
            'u' => "Up",
            'U' => "Prior",
            'x' => "Delete",
            '1' => "F1",
            _ => return None,
        })
    };
    let c = chars.next()?;
    let (mods, sym) = match c {
        '^' | '+' => {
            let m = if c == '^' { "ctrl" } else { "shift" };
            (vec![m], base(chars.next()?)?)
        }
        't' => (vec!["shift"], "Tab"),
        'B' | 'E' | 'H' | 'L' | 'R' | 'X' => (vec!["ctrl"], base(c.to_ascii_lowercase())?),
        'F' => (vec!["shift"], "End"),
        'I' => (vec!["shift"], "Home"),
        'M' => (vec!["shift"], "Left"),
        'S' => (vec!["shift"], "Right"),
        c => (vec![], base(c)?),
    };
    Some((mods, sym.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/less")
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(dir().join(name)).unwrap()
    }

    fn help_710() -> (Sheet, usize, Vec<(&'static str, String)>) {
        let text = strip_overstrike(&fixture("help-710.txt"));
        let (s, n, unused) = help(&text);
        let unused = unused
            .into_iter()
            .map(|(w, l)| (w, l.to_string()))
            .collect();
        (s, n, unused)
    }

    /// The descriptions bound to `key` in `category`.
    fn descs(s: &Sheet, category: &str, key: &str) -> Vec<String> {
        s.binds
            .get(category)
            .into_iter()
            .flatten()
            .filter(|b| b.key == key)
            .map(|b| b.desc.clone())
            .collect()
    }

    fn desc(s: &Sheet, category: &str, key: &str) -> String {
        let d = descs(s, category, key);
        assert_eq!(d.len(), 1, "{category} {key}: {d:?}");
        d[0].clone()
    }

    #[test]
    fn overstrike_is_undone() {
        assert_eq!(strip_overstrike("h\u{8}hi _\u{8}x"), "hi x");
        assert_eq!(strip_overstrike("\u{8}a\n\u{8}b"), "a\nb");
        assert!(
            fixture("help-710.txt").contains('\u{8}'),
            "the fixture keeps less's overstrike"
        );
    }

    #[test]
    fn help_keys_from_the_fixture() {
        let (s, skipped, _) = help_710();
        let window = "Forward one window (or N lines).";
        for k in ["f", "ctrl+f", "ctrl+v", "space"] {
            assert_eq!(desc(&s, "Moving", k), window, "{k}");
        }
        assert_eq!(desc(&s, "Summary of less commands", "q"), "Exit.");
        assert_eq!(desc(&s, "Summary of less commands", "colon q"), "Exit.");
        assert_eq!(desc(&s, "Summary of less commands", "Z Z"), "Exit.");
        assert_eq!(
            desc(&s, "Searching", "slash"),
            "Search forward for (N-th) matching line."
        );
        assert_eq!(
            desc(&s, "Jumping", "G"),
            "Go to last line in file (or line N)."
        );
        assert_eq!(
            desc(&s, "Jumping", "End"),
            "Go to last line in file (or line N)."
        );
        assert_eq!(
            desc(&s, "Moving", "alt+v"),
            "Backward one window (or N lines)."
        );
        assert_eq!(
            desc(&s, "Moving", "Return"),
            "Forward one line (or N lines)."
        );
        assert_eq!(
            desc(&s, "Moving", "Right"),
            "Right one half screen width (or N positions)."
        );
        assert_eq!(
            desc(&s, "Moving", "alt+Right"),
            "Right like ESC-) but stop at longest line."
        );
        assert_eq!(
            desc(&s, "Moving", "ctrl+Right"),
            "Right to last column displayed."
        );
        assert_eq!(
            desc(&s, "Moving", "alt+space"),
            "Forward one window, but don't stop at end-of-file."
        );
        assert_eq!(desc(&s, "Jumping", "ctrl+x ctrl+x"), "Same as '.");
        assert_eq!(
            desc(&s, "Jumping", "ctrl+alt+f"),
            "Find close bracket <c2>."
        );
        assert_eq!(
            desc(&s, "Jumping", "m"),
            "Mark the top line (or line N) with <letter>."
        );
        assert_eq!(
            desc(&s, "Searching", "ctrl+o n"),
            "Search forward for (N-th) OSC8 hyperlink."
        );
        assert_eq!(desc(&s, "Changing files", "colon e"), "Examine a new file.");
        assert_eq!(
            desc(&s, "Miscellaneous commands", "bar bar"),
            "Pipe file between mark X and mark Y to shell command."
        );
        assert_eq!(
            desc(&s, "Miscellaneous commands", "s"),
            "Save input to a file."
        );
        assert_eq!(skipped, 0, "every key in the help translates");
        assert!(!s.binds.contains_key("Options"));
        // The help's binds carry no scope, except the line editor's.
        for (c, list) in &s.binds {
            for b in list {
                assert_eq!(b.when.is_empty(), c != "Line editing", "{c}: {b:?}");
            }
        }
    }

    #[test]
    fn line_editing_keys_are_scoped_to_the_prompt() {
        let (s, ..) = help_710();
        let le = "Line editing";
        assert_eq!(desc(&s, le, "Right"), "Move cursor right one character.");
        assert_eq!(desc(&s, le, "alt+l"), "Move cursor right one character.");
        assert_eq!(desc(&s, le, "ctrl+Right"), "Move cursor right one word.");
        assert_eq!(desc(&s, le, "ctrl+u"), "Delete entire line.");
        assert!(
            descs(&s, le, "Escape").is_empty(),
            "ESC deletes the line only on MS-DOS"
        );
        assert_eq!(
            desc(&s, le, "shift+Tab"),
            "Complete filename & reverse cycle."
        );
        assert_eq!(
            desc(&s, le, "alt+Tab"),
            "Complete filename & reverse cycle."
        );
        assert_eq!(desc(&s, le, "BackSpace"), "Delete char to left of cursor.");
        let b = &s.binds[le][0];
        assert_eq!(b.when.len(), 1);
        let (scope, mode) = b.when.iter().next().unwrap();
        assert_eq!(
            (scope.as_str(), mode.iter().next().unwrap().as_str()),
            ("mode", "line-editing")
        );
    }

    #[test]
    fn no_help_line_is_lost() {
        let (s, _, unused) = help_710();
        let lost: Vec<_> = unused.iter().filter(|(w, _)| *w == "no key").collect();
        assert!(lost.is_empty(), "{lost:?}");
        // What is left out: headings, dividers, the options, and the notes indented under a section.
        for (why, line) in &unused {
            match *why {
                // A key line is indented two spaces (one in LINE EDITING); notes are indented six or more.
                "note" => assert!(line.starts_with("      "), "{line:?}"),
                "heading" | "divider" | "options" => {}
                w => panic!("{w}: {line:?}"),
            }
        }
        let headings: Vec<_> = unused
            .iter()
            .filter(|(w, _)| *w == "heading")
            .map(|(_, l)| l.trim())
            .collect();
        assert_eq!(
            headings,
            [
                "SUMMARY OF LESS COMMANDS",
                "MOVING",
                "SEARCHING",
                "JUMPING",
                "CHANGING FILES",
                "MISCELLANEOUS COMMANDS",
                "OPTIONS",
                "LINE EDITING"
            ]
        );
        // Every command-shaped line (two spaces, then a key) outside OPTIONS gave a bind.
        let commands = strip_overstrike(&fixture("help-710.txt"))
            .split("OPTIONS")
            .next()
            .unwrap()
            .lines()
            .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
            .count();
        assert!(commands > 60, "{commands}");
        assert!(s.binds.values().flatten().count() > commands);
    }

    #[test]
    fn debian_bookworms_less_590_parses_the_same_way() {
        let text = strip_overstrike(&fixture("help-590.txt"));
        let (s, skipped, unused) = help(&text);
        assert_eq!(skipped, 0);
        let lost: Vec<_> = unused.iter().filter(|(w, _)| *w == "no key").collect();
        assert!(lost.is_empty(), "{lost:?}");
        assert_eq!(desc(&s, "Summary of less commands", "q"), "Exit.");
        assert_eq!(
            desc(&s, "Moving", "space"),
            "Forward one window (or N lines)."
        );
        // 590 names the clear-mark key `ESC-M<letter>`; 710 says `ESC-m`.
        assert_eq!(desc(&s, "Jumping", "alt+M"), "Clear a mark.");
        assert_eq!(s.binds["Line editing"].len(), 31);
    }

    #[test]
    fn help_notation() {
        for (raw, want) in [
            ("^F", Some("ctrl+f")),
            ("ESC-v", Some("alt+v")),
            ("SPACE", Some("space")),
            ("CR", Some("Return")),
            ("RightArrow", Some("Right")),
            ("^X^X", Some("ctrl+x ctrl+x")),
            (":q", Some("colon q")),
            ("ESC-^F", Some("ctrl+alt+f")),
            ("ESC-SPACE", Some("alt+space")),
            ("ctrl-BACKSPACE", Some("ctrl+BackSpace")),
            ("SHIFT-TAB", Some("shift+Tab")),
            ("ESC-<", Some("alt+less")),
            ("ESC-\u{e9}", None),
        ] {
            assert_eq!(help_key(raw).as_deref(), want, "{raw}");
        }
        assert_eq!(strip_argument("/pattern"), "/");
        assert_eq!(strip_argument("||XYcommand"), "||");
        assert_eq!(strip_argument("ESC-m<letter>"), "ESC-m");
        assert_eq!(strip_argument("command"), "command");
    }

    #[test]
    fn an_unknown_key_in_the_help_is_skipped_and_counted() {
        let text = "\n                MOVING\n\n  j  ESC-\u{e9}           Forward one line.\n";
        let (s, skipped, _) = help(text);
        assert_eq!(skipped, 1);
        assert_eq!(desc(&s, "Moving", "j"), "Forward one line.");
    }

    #[test]
    fn lesskey_notation() {
        let key = |l: &str| lesskey_line(l).and_then(|(k, ..)| k);
        assert_eq!(key("\\ev back-screen").as_deref(), Some("alt+v"));
        assert_eq!(key("^X^V examine").as_deref(), Some("ctrl+x ctrl+v"));
        assert_eq!(key("\\n forw-line").as_deref(), Some("ctrl+j"));
        assert_eq!(key("\\r forw-line").as_deref(), Some("Return"));
        assert_eq!(key("\\t forw-line").as_deref(), Some("Tab"));
        assert_eq!(key("\\kd forw-line").as_deref(), Some("Down"));
        assert_eq!(key("\\kD forw-screen").as_deref(), Some("Next"));
        assert_eq!(key("\\k^r x").as_deref(), Some("ctrl+Right"));
        assert_eq!(key("\\k+u x").as_deref(), Some("shift+Up"));
        assert_eq!(key("\\kL x").as_deref(), Some("ctrl+Left"));
        assert_eq!(key("\\\\ x").as_deref(), Some("backslash"));
        assert_eq!(key("\\40 forw-screen").as_deref(), Some("space"));
        assert_eq!(key("\\^ x").as_deref(), Some("asciicircum"));
        assert_eq!(key("\\e x").as_deref(), Some("Escape"));
        assert_eq!(key("\\kp1 x"), None, "the keypad");
        // Control characters past `Z`: `^]` is ctrl+bracketright, as the help's `^]` reads; `^?` is DEL.
        assert_eq!(key("^] x").as_deref(), Some("ctrl+bracketright"));
        assert_eq!(key("^\\ x").as_deref(), help_key("^\\").as_deref());
        assert_eq!(key("^? x").as_deref(), Some("ctrl+underscore"));
        assert_eq!(key("^[v x").as_deref(), Some("alt+v"), "as \\ev");
        assert_eq!(key("^[ x").as_deref(), Some("Escape"));
        assert_eq!(
            key("\\e^[x x").as_deref(),
            Some("Escape alt+x"),
            "ESC ESC x"
        );
        // `^` before a non-ASCII character, or before the space that ends the key: no keysym, counted.
        assert_eq!(lesskey_line("^\u{e9} x").map(|(k, ..)| k), Some(None));
        assert_eq!(lesskey_line("^\u{fffd} x").map(|(k, ..)| k), Some(None));
        assert_eq!(lesskey_line("j^ x").map(|(k, ..)| k), Some(None));
        // `\\#`: an escaped backslash, then a comment.
        let (_, action, extra) = lesskey_line("q quit a\\\\# gone").unwrap();
        assert_eq!((action.as_str(), extra.as_str()), ("quit", "a\\\\"));
        assert_eq!(key("\\35 x").as_deref(), Some("ctrl+bracketright"));
        let (_, action, extra) = lesskey_line("B set-mark b/next\\n  # a comment").unwrap();
        assert_eq!((action.as_str(), extra.as_str()), ("set-mark", "b/next\\n"));
        assert!(lesskey_line("x").is_none(), "no action");
        assert!(lesskey_line("# a comment").is_none());
    }

    #[test]
    fn lesskey_overlays_the_defaults() {
        let (mut s, ..) = help_710();
        let skipped = overlay(&mut s, &fixture("lesskey"));
        assert_eq!(skipped, 1, "the keypad key");
        // A user bind replaces the default for that key.
        assert_eq!(desc(&s, "lesskey", "j"), "back-line");
        assert!(descs(&s, "Moving", "j").is_empty());
        assert_eq!(desc(&s, "lesskey", "alt+j"), "forw-line");
        assert_eq!(desc(&s, "lesskey", "ctrl+x ctrl+g"), "goto-end");
        assert_eq!(desc(&s, "lesskey", "Down"), "forw-screen");
        assert_eq!(desc(&s, "lesskey", "B"), "set-mark b/next\\n");
        // `invalid` and a bare `noaction` unbind; `noaction` with an extra string types it.
        assert!(descs(&s, "Summary of less commands", "Z Z").is_empty());
        assert!(descs(&s, "lesskey", "Z Z").is_empty());
        assert!(descs(&s, "Moving", "r").is_empty());
        assert_eq!(desc(&s, "lesskey", "backslash"), "noaction q");
        // `#line-edit` and `#env` lines aren't read as commands; the line editor's defaults stay.
        assert!(descs(&s, "lesskey", "ctrl+a").is_empty());
        assert_eq!(
            desc(&s, "Line editing", "Right"),
            "Move cursor right one character."
        );
        // Untouched defaults stay, the line editor's too though `\kd` was rebound.
        assert_eq!(desc(&s, "Summary of less commands", "q"), "Exit.");
        assert_eq!(
            desc(&s, "Line editing", "Down"),
            "Retrieve next command line."
        );
        // Exactly the file's commands: no `#env` line (`LESS = -R`), malformed line (`x`, `^`) or `#version` line.
        let mut keys: Vec<_> = s.binds["lesskey"].iter().map(|b| b.key.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "B",
                "Down",
                "alt+j",
                "backslash",
                "ctrl+bracketright",
                "ctrl+x ctrl+g",
                "j"
            ]
        );
    }

    #[test]
    fn a_key_bound_twice_keeps_the_last_and_stop_counts_only_in_commands() {
        let (mut s, ..) = help_710();
        overlay(&mut s, "q quit\nq back-line\n#line-edit\n#stop\n");
        assert_eq!(desc(&s, "lesskey", "q"), "back-line");
        assert_eq!(
            desc(&s, "Moving", "space"),
            "Forward one window (or N lines)."
        );
    }

    #[test]
    fn stop_drops_every_default_command() {
        let (mut s, ..) = help_710();
        overlay(&mut s, "#command\nq quit\n#stop\n");
        let cats: Vec<_> = s.binds.keys().map(String::as_str).collect();
        assert_eq!(cats, ["Line editing", "lesskey"]);
        assert_eq!(desc(&s, "lesskey", "q"), "quit");
    }

    #[test]
    fn lesskey_file_choice() {
        let tmp = std::env::temp_dir().join(format!("kbf-lesskey-{}", std::process::id()));
        let home = tmp.join("home");
        let xdg = tmp.join("xdg");
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::create_dir_all(&xdg).unwrap();
        let env_of = |vars: Vec<(&'static str, PathBuf)>| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.display().to_string())
            }
        };
        let all = || {
            vec![
                ("LESSKEYIN", tmp.join("in")),
                ("XDG_CONFIG_HOME", xdg.clone()),
                ("HOME", home.clone()),
            ]
        };
        // None present: no overlay.
        assert_eq!(lesskey_path(&env_of(all()[1..].to_vec())), None);
        for f in [
            home.join(".lesskey"),
            home.join(".config/lesskey"),
            xdg.join("lesskey"),
            tmp.join("in"),
        ] {
            std::fs::write(&f, "").unwrap();
        }
        assert_eq!(lesskey_path(&env_of(all())), Some(tmp.join("in")));
        assert_eq!(
            lesskey_path(&env_of(all()[1..].to_vec())),
            Some(xdg.join("lesskey"))
        );
        assert_eq!(
            lesskey_path(&env_of(all()[2..].to_vec())),
            Some(home.join(".config/lesskey"))
        );
        std::fs::remove_file(home.join(".config/lesskey")).unwrap();
        assert_eq!(
            lesskey_path(&env_of(all()[2..].to_vec())),
            Some(home.join(".lesskey"))
        );
        // An empty variable is unset: `XDG_CONFIG_HOME=""` doesn't look for `./lesskey`, `LESSKEYIN=""` falls through.
        let mut empty = all();
        empty[0].1 = PathBuf::new();
        empty[1].1 = PathBuf::new();
        assert_eq!(lesskey_path(&env_of(empty)), Some(home.join(".lesskey")));
        // `XDG_CONFIG_HOME` set but without the file falls through to `~/.config/lesskey`.
        std::fs::write(home.join(".config/lesskey"), "").unwrap();
        let mut no_xdg_file = all()[1..].to_vec();
        no_xdg_file[0].1 = tmp.join("elsewhere");
        assert_eq!(
            lesskey_path(&env_of(no_xdg_file)),
            Some(home.join(".config/lesskey"))
        );
        // A `LESSKEYIN` that names no file is no user keys (less reads only that one).
        let mut missing = all();
        missing[0].1 = tmp.join("nope");
        assert_eq!(lesskey_path(&env_of(missing)), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn versions() {
        assert_eq!(
            version("less 668 (PCRE2 regular expressions)\nCopyright").unwrap(),
            668
        );
        assert_eq!(version("less 590x (GNU regular expressions)").unwrap(), 590);
        assert!(version("BusyBox v1.36").is_err());
        assert!(version("").is_err());
    }

    /// A stand-in `less` on PATH printing `version` for `--version` and the 710 fixture for `--help`; given the
    /// argument `slow`, it sleeps 5 s first. Returns its directory.
    fn fake_less(name: &str, version: &str, slow: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!("kbf-less-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = '{slow}' ] && sleep 5\ncase \"$1\" in --version) echo 'less {version} (POSIX regular expressions)' ;; \
             --help) cat '{}' ;; *) exit 3 ;; esac\n",
            dir().join("help-710.txt").display()
        );
        // Written by `sh`, not by this process: a write handle another test thread's fork inherited would make
        // running the script fail with ETXTBSY (as `plugins.rs`'s `write_script` says).
        use std::io::Write;
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(bin.join("less"))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(sh.wait().unwrap().success());
        bin
    }

    fn with_path(bin: &Path) -> impl Fn(&mut Command) + '_ {
        move |c: &mut Command| {
            c.env("PATH", format!("{}:/usr/bin:/bin", bin.display()));
        }
    }

    #[test]
    fn user_keys_need_less_582() {
        let env = |k: &str| (k == "LESSKEYIN").then(|| dir().join("lesskey").display().to_string());
        for (v, read_source) in [
            ("668", true),
            ("590", true),
            ("582", true),
            ("581", false),
            ("487", false),
        ] {
            let bin = fake_less(v, v, "");
            let (s, notes) = read(TIMEOUT, with_path(&bin), &env).unwrap();
            let _ = std::fs::remove_dir_all(&bin);
            assert_eq!(s.binds.contains_key("lesskey"), read_source, "less {v}");
            assert!(!s.binds["Moving"].is_empty());
            // Old less: one line saying the keys aren't read; new less: the fixture's one keypad key skipped.
            assert_eq!(notes.len(), 1, "less {v}: {notes:?}");
            let old = notes[0].contains("isn't read");
            assert_eq!(old, !read_source, "less {v}: {notes:?}");
            assert_eq!(
                notes[0].contains("skipping 1 key "),
                read_source,
                "{notes:?}"
            );
        }
        // Old less with no user keys anywhere: nothing to say.
        let bin = fake_less("old", "487", "");
        let (_, notes) = read(TIMEOUT, with_path(&bin), &|_| None).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(notes.is_empty(), "{notes:?}");
        // An unreadable lesskey file (a directory here) is a note, and the defaults still show.
        let bin = fake_less("dir", "668", "");
        let lesskeyin = |k: &str| (k == "LESSKEYIN").then(|| dir().display().to_string());
        let (s, notes) = read(TIMEOUT, with_path(&bin), &lesskeyin).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(!s.binds.contains_key("lesskey"));
        assert!(
            notes.is_empty(),
            "a directory isn't a file less reads: {notes:?}"
        );
        // A non-UTF-8 byte (a Latin-1 comment) loses nothing else.
        let tmp = std::env::temp_dir().join(format!("kbf-lesskey-bytes-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("latin1"), b"# caf\xe9\nj back-line\n").unwrap();
        let bin = fake_less("bytes", "668", "");
        let latin1 = |k: &str| (k == "LESSKEYIN").then(|| tmp.join("latin1").display().to_string());
        let (s, _) = read(TIMEOUT, with_path(&bin), &latin1).unwrap();
        assert_eq!(desc(&s, "lesskey", "j"), "back-line");
        // A compiled `~/.less` and no source file: one note on any less, never parsed.
        std::fs::write(tmp.join(".less"), b"\0M+G\x01binary").unwrap();
        let home = |k: &str| (k == "HOME").then(|| tmp.display().to_string());
        let (s, notes) = read(TIMEOUT, with_path(&bin), &home).unwrap();
        assert!(!s.binds.contains_key("lesskey"));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("compiled lesskey file"), "{notes:?}");
        let _ = std::fs::remove_dir_all(&bin);
        let bin = fake_less("old-compiled", "487", "");
        let (_, notes) = read(TIMEOUT, with_path(&bin), &home).unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("487 reads only a compiled"), "{notes:?}");
        let _ = std::fs::remove_dir_all(&bin);
        // A source file that can't be read: a note, and the defaults show. (Root reads anything: skip then.)
        std::fs::write(tmp.join("locked"), "j back-line\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        if std::fs::read(tmp.join("locked")).is_err() {
            let bin = fake_less("locked", "668", "");
            let locked =
                |k: &str| (k == "LESSKEYIN").then(|| tmp.join("locked").display().to_string());
            let (s, notes) = read(TIMEOUT, with_path(&bin), &locked).unwrap();
            let _ = std::fs::remove_dir_all(&bin);
            assert!(!s.binds.contains_key("lesskey"));
            assert!(
                notes.len() == 1 && notes[0].starts_with("can't read"),
                "{notes:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
        let bin = fake_less("bad", "x", "");
        let e = read(TIMEOUT, with_path(&bin), &env).unwrap_err();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(format!("{e:#}").contains("not a version"), "{e:#}");
    }

    #[test]
    fn a_help_with_no_keys_is_an_error() {
        let bin = std::env::temp_dir().join(format!("kbf-less-empty-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        // `--help` prints nothing. Written by `sh`, as in `fake_less`.
        let script = "case \"$1\" in --version) echo 'less 668' ;; esac";
        let mut sh = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("less"))
            .arg(script)
            .spawn()
            .unwrap();
        assert!(sh.wait().unwrap().success());
        let e = read(TIMEOUT, with_path(&bin), &|_| None).unwrap_err();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(e.to_string().contains("listed no keys"), "{e:#}");
    }

    #[test]
    fn missing_or_hanging_less_is_an_error() {
        let none = |_: &str| None;
        let e = read(
            TIMEOUT,
            |c| {
                c.env("PATH", "/nonexistent");
            },
            &none,
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("running less --version"), "{e:#}");
        for slow in ["--version", "--help"] {
            let bin = fake_less("slow", "668", slow);
            let start = std::time::Instant::now();
            let e = read(Duration::from_millis(300), with_path(&bin), &none).unwrap_err();
            let _ = std::fs::remove_dir_all(&bin);
            let e = format!("{e:#}");
            assert!(
                e.contains(&format!("running less {slow}")) && e.contains("did not answer"),
                "{e}"
            );
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }

    /// Skips (with a message) when less isn't on PATH.
    fn have_less() -> bool {
        let found = Command::new("sh")
            .args(["-c", "command -v less"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            eprintln!("skipping: less is not installed");
        }
        found
    }

    #[test]
    fn real_less_help_parses() {
        if !have_less() {
            return;
        }
        let none = |_: &str| None;
        let (plain, _) = read(TIMEOUT, |_| {}, &none).unwrap();
        let all: Vec<_> = plain
            .binds
            .values()
            .flatten()
            .map(|b| b.key.as_str())
            .collect();
        assert!(all.contains(&"q") && all.contains(&"space"), "{all:?}");
        // The environment's `LESS` doesn't change the help, so not the sheet either.
        for odd in ["-R -F -X", "--use-color -Dd+r", "-?", "+G"] {
            let (s, _) = read(
                TIMEOUT,
                |c| {
                    c.env("LESS", odd);
                },
                &none,
            )
            .unwrap();
            assert_eq!(s, plain, "LESS={odd}");
        }
    }
}
