//! bash, zsh and fish: the line editor's keys at the prompt, from the shell itself started as `<shell> -ic`, so the
//! user's rc files (and `.inputrc`) apply. bash: `bind -p`, `bind -s` and `bind -X`; zsh: `bindkey -L`, the main
//! keymap; fish: `bind`, every mode, after the prompt event that makes fish load its bindings.

use crate::context::{run_keeping_output, Group};
use crate::sheet::{chord, Bind, Name, Sheet};
use anyhow::Context as _;
use std::process::Command;
use std::time::Duration;

/// `-ic` runs the user's whole rc file first; one that hangs (or `exec`s a program) must not hang the picker.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Marks where the keymap starts on stdout, so a banner from an rc file can't break the parse.
const SENTINEL: &str = "KEYBIND-FIND-KEYMAP";

/// The fixed command line each shell runs: the sentinel, then the bind listing. `builtin` so an rc file's function
/// named `bind` or `bindkey` isn't run instead (a function named `builtin` still would: it's the user's own rc file).
/// Nothing from a config ever goes in here.
fn script(shell: &str) -> Option<&'static str> {
    Some(match shell {
        "bash" => {
            "builtin echo KEYBIND-FIND-KEYMAP; builtin bind -p; builtin bind -s; builtin echo KEYBIND-FIND-COMMANDS; builtin bind -X"
        }
        "zsh" => "builtin echo KEYBIND-FIND-KEYMAP; builtin bindkey -L",
        // fish loads its bindings on the first prompt (`__fish_on_interactive`): plain `fish -c bind` lists none.
        "fish" => "emit fish_prompt; builtin echo KEYBIND-FIND-KEYMAP; builtin bind",
        _ => return None,
    })
}

pub fn sheet(shell: &str) -> anyhow::Result<Sheet> {
    read(shell, TIMEOUT, |_| {})
}

/// Runs `shell` with `setup` applied to its command (a test seam: a fixture HOME, `ZDOTDIR`, ...), and parses what
/// it prints after the last sentinel.
fn read(shell: &str, timeout: Duration, setup: impl FnOnce(&mut Command)) -> anyhow::Result<Sheet> {
    let script =
        script(shell).with_context(|| format!("{shell} is not a shell keybind-find reads"))?;
    // ponytail: the shell on keybind-find's PATH, not the terminal's own (`/proc/<pid>/exe`), which `providers_for`
    // doesn't pass on; take it from there if a second install of a shell turns out to matter.
    // In a new session, without keybind-find's controlling terminal: started from one (a terminal, or a hotkey
    // daemon launched there), `-i` makes bash and zsh take the terminal for job control and stop on SIGTTIN until
    // the timeout. `setsid` (util-linux's or busybox's), found on our own PATH (a test's PATH is for the shell);
    // without it, the shell runs as before. No `--wait` (busybox has none): it only forks when it leads a group,
    // which it never does here, so it is the shell.
    let setsid = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("setsid"))
        .find(|p| p.is_file());
    let (mut cmd, group) = match setsid {
        Some(setsid) => {
            let mut cmd = Command::new(setsid);
            cmd.arg(shell);
            (cmd, Group::Session)
        }
        None => (Command::new(shell), Group::Own),
    };
    // Through a mise shim, a missing shell must not start a download in the middle of a keypress (as nvim.rs does).
    // `KEYBIND_FIND` lets an rc file skip its slow parts; the shell's stdin is closed by `run_keeping_output`.
    cmd.args(["-ic", script])
        .env("KEYBIND_FIND", "1")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let out = run_keeping_output(cmd, timeout, group)
        .with_context(|| format!("starting {shell} -ic, which runs your rc files"))?;
    let at = out.rfind(SENTINEL).with_context(|| {
        format!("{shell} printed no keymap (does an rc file exit, or exec another program?)")
    })?;
    let out = &out[at + SENTINEL.len()..];
    let (sheet, skipped) = match shell {
        "bash" => bash(out),
        "zsh" => zsh(out),
        _ => fish(out),
    };
    if skipped > 0 {
        let keys = if skipped == 1 { "key" } else { "keys" };
        eprintln!("keybind-find: {shell}: skipping {skipped} {keys} with no keysym we know");
    }
    Ok(sheet)
}

fn new_sheet(shell: &str) -> Sheet {
    Sheet {
        title: shell.into(),
        provider: shell.into(),
        binds: Default::default(),
    }
}

/// Adds a bind unless the same key, description and scope is already there (`\e[D` and `\eOD` are both Left).
fn push(sheet: &mut Sheet, category: &str, bind: Bind) {
    let list = sheet.binds.entry(category.into()).or_default();
    if !list.contains(&bind) {
        list.push(bind);
    }
}

fn bind(key: String, desc: String) -> Bind {
    Bind {
        key,
        desc,
        action: String::new(),
        when: Default::default(),
    }
}

/// Marks where `bind -X` starts: bash 5.3 prints its lines as `"\C-xg" "command"`, but 5.2 as `"\C-xg": "command"`,
/// the same shape as a `bind -s` macro.
const COMMANDS: &str = "KEYBIND-FIND-COMMANDS";

/// `bind -p` (`"\C-a": beginning-of-line`) and `bind -s` (`"\C-xh": "text"`) lines, told apart by their shape, then
/// after `COMMANDS` the `bind -X` lines. `# name (not bound)` lines and `self-insert` are left out.
fn bash(out: &str) -> (Sheet, usize) {
    let mut sheet = new_sheet("bash");
    let mut skipped = 0;
    let (keymap, commands) = out.split_once(COMMANDS).unwrap_or((out, ""));
    let lines = keymap
        .lines()
        .map(|l| (false, l))
        .chain(commands.lines().map(|l| (true, l)));
    for (command, line) in lines {
        let Some((seq, rest)) = line.strip_prefix('"').and_then(quoted) else {
            continue;
        };
        let text = rest
            .strip_prefix(':')
            .unwrap_or(rest)
            .strip_prefix(" \"")
            .and_then(quoted);
        let (category, desc) = if command {
            let Some((cmd, _)) = text else { continue };
            ("commands", format!("runs: {}", unquote(&cmd)))
        } else if let Some(text) = rest.strip_prefix(": \"").and_then(quoted) {
            ("macros", format!("macro: {}", unquote(&text.0)))
        } else if let Some(func) = rest.strip_prefix(": ") {
            // A function name is one bare word; `"` opens a macro that wasn't closed.
            // `do-lowercase-version` (`\eA` acts as `\ea`): 51 rows in a stock bash that say nothing new.
            if matches!(func, "self-insert" | "do-lowercase-version" | "")
                || func.contains(['"', ' '])
            {
                continue;
            }
            ("line editing", func.to_string())
        } else {
            continue;
        };
        match decode(&seq, false) {
            Some(raw) if is_report(&raw) => {}
            raw => match raw.as_deref().and_then(keys) {
                Some(key) => push(&mut sheet, category, bind(key, desc)),
                None => skipped += 1,
            },
        }
    }
    (sheet, skipped)
}

/// `bindkey "^R" widget` and `bindkey -s "^Xh" "text"` lines; `-R` ranges (self-insert) and other flags are left out.
/// zsh prints each string double-quoted for the shell, so `"^\\\\"` is `^\\` to bindkey: ctrl+backslash.
fn zsh(out: &str) -> (Sheet, usize) {
    let mut sheet = new_sheet("zsh");
    let mut skipped = 0;
    for line in out.lines() {
        let Some(rest) = line.strip_prefix("bindkey ") else {
            continue;
        };
        let (macro_, rest) = match rest.strip_prefix("-s ") {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        let Some((seq, rest)) = rest.strip_prefix('"').and_then(quoted) else {
            continue;
        };
        let rest = rest.trim_start();
        let (category, desc) = if macro_ {
            let Some((text, _)) = rest.strip_prefix('"').and_then(quoted) else {
                continue;
            };
            ("macros", format!("macro: {}", unquote(&text)))
        } else {
            if rest.is_empty() || rest.contains(' ') || rest == "self-insert" {
                continue;
            }
            ("line editing", rest.to_string())
        };
        match decode(&unquote(&seq), true) {
            Some(raw) if is_report(&raw) => {}
            raw => match raw.as_deref().and_then(keys) {
                Some(key) => push(&mut sheet, category, bind(key, desc)),
                None => skipped += 1,
            },
        }
    }
    (sheet, skipped)
}

/// The text up to the closing `"` (escapes kept as written) and what follows it; `None` if it isn't closed.
fn quoted(s: &str) -> Option<(String, &str)> {
    let mut chars = s.char_indices();
    let mut text = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((text, &s[i + 1..])),
            '\\' => {
                text.push(c);
                text.push(chars.next()?.1);
            }
            c => text.push(c),
        }
    }
    None
}

/// Removes a shell's double-quote escapes: `\\`, `\"`, `\$` and `` \` `` become the character; other backslashes stay.
fn unquote(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some(&n @ ('\\' | '"' | '$' | '`'))) => {
                out.push(n);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

const ESC: char = '\u{1b}';

/// A readline or zsh key string (`\C-x`, `\M-x`, `\e`, `\\`, `\"`, octal `\177`, `\x41`; with `caret`, zsh's `^X`
/// and `^?`) as the characters the terminal sends. `\M-x` is the byte `x | 0x80`: a run of them that is UTF-8 is that
/// character (zsh prints `é` as `\M-C\M-)`), and otherwise each is ESC then `x`, as terminals send alt.
fn decode(s: &str, caret: bool) -> Option<Vec<char>> {
    let mut out = Vec::new();
    let mut high = Vec::new();
    let flush = |high: &mut Vec<u8>, out: &mut Vec<char>| {
        for chunk in high.utf8_chunks() {
            out.extend(chunk.valid().chars());
            for b in chunk.invalid() {
                out.extend([ESC, char::from(b & 0x7f)]);
            }
        }
        high.clear();
    };
    let mut chars = s.chars().peekable();
    while chars.peek().is_some() {
        match one(&mut chars, caret)? {
            (true, c) if c.is_ascii() => high.push(c as u8 | 0x80),
            (meta, c) => {
                flush(&mut high, &mut out);
                if meta {
                    out.push(ESC);
                }
                out.push(c);
            }
        }
    }
    flush(&mut high, &mut out);
    Some(out)
}

/// What a terminal sends about itself rather than for a key: bracketed paste's start and end, focus in and out, and
/// private-mode replies. Shells bind them; they aren't keys anyone presses, so they're left out without a warning.
fn is_report(raw: &[char]) -> bool {
    let Some(rest) = raw.strip_prefix(&[ESC, '[']) else {
        return false;
    };
    let rest: String = rest.iter().collect();
    matches!(rest.as_str(), "200~" | "201~" | "I" | "O") || rest.starts_with('?')
}

/// One character of a key string, and whether it carries meta.
fn one(chars: &mut std::iter::Peekable<std::str::Chars>, caret: bool) -> Option<(bool, char)> {
    let c = chars.next()?;
    if caret && c == '^' && chars.peek().is_some() {
        let (meta, c) = one(chars, false)?;
        return Some((meta, ctrl(c)?));
    }
    if c != '\\' {
        return Some((false, c));
    }
    let e = chars.next()?;
    let prefixed = |chars: &mut std::iter::Peekable<std::str::Chars>| {
        if chars.peek() == Some(&'-') {
            chars.next();
            one(chars, caret)
        } else {
            None
        }
    };
    Some(match e {
        'C' => {
            let (meta, c) = prefixed(chars)?;
            (meta, ctrl(c)?)
        }
        'M' => (true, prefixed(chars)?.1),
        'e' => (false, ESC),
        'a' => (false, '\u{7}'),
        'b' => (false, '\u{8}'),
        'd' => (false, '\u{7f}'),
        'f' => (false, '\u{c}'),
        'n' => (false, '\n'),
        'r' => (false, '\r'),
        't' => (false, '\t'),
        'v' => (false, '\u{b}'),
        'x' => {
            let hex = std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_hexdigit()))
                .take(2)
                .collect::<String>();
            (false, char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?)
        }
        '0'..='7' => {
            let mut n = e.to_digit(8)?;
            for _ in 0..2 {
                match chars.peek().and_then(|c| c.to_digit(8)) {
                    Some(d) => {
                        n = n * 8 + d;
                        chars.next();
                    }
                    None => break,
                }
            }
            (false, char::from_u32(n)?)
        }
        e => (false, e),
    })
}

/// The control character for `c` (`a` → 0x01, `?` → DEL), `None` if it has none.
fn ctrl(c: char) -> Option<char> {
    match c.to_ascii_uppercase() {
        '?' => Some('\u{7f}'),
        u @ '@'..='_' => char::from_u32(u as u32 - 0x40),
        _ => None,
    }
}

/// Characters a terminal sends as chords in the crate's notation, joined by spaces (`ctrl+x ctrl+g`); `None` if any
/// part has no keysym we know (bracketed paste, focus reports, non-ASCII).
fn keys(raw: &[char]) -> Option<String> {
    let mut out = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        let (mods, sym, used) = key(rest)?;
        out.push(chord(&mods, &sym));
        rest = &rest[used..];
    }
    (!out.is_empty()).then(|| out.join(" "))
}

/// The first key in `raw`: its modifiers, keysym and how many characters it took.
fn key(raw: &[char]) -> Option<(Vec<&'static str>, String, usize)> {
    let c = *raw.first()?;
    if c != ESC {
        let (mods, sym) = plain(c)?;
        return Some((mods, sym.into(), 1));
    }
    match raw.get(1) {
        None => Some((vec![], "Escape".into(), 1)),
        Some('[' | 'O') if raw.len() > 2 => {
            let (mods, sym, used) = csi(&raw[1..])?;
            Some((mods, sym, used + 1))
        }
        // ESC before a key is alt: `\eb`, `\e\C-h`, `\e\e[D` (alt+Left), `\e\e` (alt+Escape).
        Some(_) => {
            let (mut mods, sym, used) = key(&raw[1..])?;
            mods.push("alt");
            Some((mods, sym, used + 1))
        }
    }
}

/// One character with no ESC before it: control characters as ctrl, DEL as BackSpace.
fn plain(c: char) -> Option<(Vec<&'static str>, &'static str)> {
    Some(match c {
        '\t' => (vec![], "Tab"),
        '\r' => (vec![], "Return"),
        '\u{7f}' => (vec![], "BackSpace"),
        '\u{0}' => (vec!["ctrl"], "space"),
        c if (c as u32) < 0x20 => (
            vec!["ctrl"],
            super::nvim::single(lower(char::from_u32(c as u32 + 0x40)?))?,
        ),
        c => (vec![], super::nvim::single(c)?),
    })
}

fn lower(c: char) -> char {
    c.to_ascii_lowercase()
}

/// A CSI (`[`) or SS3 (`O`) sequence after the ESC: arrows, Home/End, `~` editing keys, F1-F12, xterm's
/// `27;mod;code~` and the kitty/fixterms `code;mod u`, with xterm's modifier parameter (`1;5C` is ctrl+Right).
fn csi(s: &[char]) -> Option<(Vec<&'static str>, String, usize)> {
    let end = s[1..].iter().position(|c| !matches!(c, '0'..='9' | ';'))? + 1;
    let fin = *s.get(end)?;
    let params: Vec<u32> = s[1..end]
        .iter()
        .collect::<String>()
        .split(';')
        .map(|p| {
            if p.is_empty() {
                Some(1)
            } else {
                p.parse().ok()
            }
        })
        .collect::<Option<_>>()?;
    let (sym, m): (String, u32) = match (fin, params.as_slice()) {
        ('A'..='D' | 'H' | 'F' | 'P'..='S', p) if p.len() <= 2 => {
            let sym = match fin {
                'A' => "Up",
                'B' => "Down",
                'C' => "Right",
                'D' => "Left",
                'H' => "Home",
                'F' => "End",
                'P' => "F1",
                'Q' => "F2",
                'R' => "F3",
                _ => "F4",
            };
            // `\e[1;5C`, and the older one-number `\e[5C` / `\eO3C`.
            (sym.into(), *p.last().unwrap_or(&1))
        }
        ('Z', [_]) if s[0] == '[' => ("Tab".into(), 2),
        ('~', [27, m, code]) if s[0] == '[' => (code_sym(*code)?.into(), *m),
        // The kitty protocol's 8 is super; in xterm's forms it is meta, which terminals send for Alt.
        ('u', [code, m]) if s[0] == '[' => {
            // Caps and Num Lock (64, 128) don't change the key; hyper and meta (16, 32) have no name here.
            let bits = m.checked_sub(1)? & !(64 | 128);
            if bits & (16 | 32) != 0 {
                return None;
            }
            let mut mods = mods(1 + (bits & 7))?;
            if bits & 8 != 0 {
                mods.push("super");
            }
            return Some((mods, code_sym(*code)?.into(), end + 1));
        }
        ('u', [code]) if s[0] == '[' => (code_sym(*code)?.into(), 1),
        ('~', [n, rest @ ..]) if s[0] == '[' && rest.len() <= 1 => {
            let sym = match n {
                1 | 7 => "Home",
                2 => "Insert",
                3 => "Delete",
                4 | 8 => "End",
                5 => "Prior",
                6 => "Next",
                11..=15 => return f_key(*n - 10, rest, end),
                17..=21 => return f_key(*n - 11, rest, end),
                23 | 24 => return f_key(*n - 12, rest, end),
                _ => return None,
            };
            (sym.into(), *rest.first().unwrap_or(&1))
        }
        _ => return None,
    };
    Some((mods(m)?, sym, end + 1))
}

fn f_key(n: u32, rest: &[u32], end: usize) -> Option<(Vec<&'static str>, String, usize)> {
    Some((mods(*rest.first().unwrap_or(&1))?, format!("F{n}"), end + 1))
}

/// A character code from a `u` or `27;m;c~` sequence as a keysym.
fn code_sym(code: u32) -> Option<&'static str> {
    match code {
        9 => Some("Tab"),
        13 => Some("Return"),
        27 => Some("Escape"),
        127 => Some("BackSpace"),
        c => super::nvim::single(char::from_u32(c)?),
    }
}

/// xterm's modifier parameter: 1 plus shift 1, alt 2, ctrl 4, meta 8 (read as alt, as terminals send it for Alt).
fn mods(m: u32) -> Option<Vec<&'static str>> {
    let bits = m.checked_sub(1).filter(|b| *b < 16)?;
    Some(
        [(1, "shift"), (2, "alt"), (4, "ctrl"), (8, "alt")]
            .into_iter()
            .filter(|(b, _)| bits & b != 0)
            .map(|(_, m)| m)
            .collect(),
    )
}

/// `bind` lines, every mode, each tagged `when.mode` (fish prints no `-M` for `default`). A `--preset` bind is dropped
/// when a user bind has the same key and mode. Lines that aren't `bind` (fish 4's `# Defined in` comment, terminal
/// escapes around the prompt) and `self-insert` binds are left out.
fn fish(out: &str) -> (Sheet, usize) {
    let mut sheet = new_sheet("fish");
    let mut skipped = 0;
    let mut found = Vec::new();
    for line in out.lines() {
        let Some(words) = line.strip_prefix("bind ").and_then(fish_words) else {
            continue;
        };
        let (mut preset, mut mode, mut named) = (false, "default".to_string(), false);
        let mut sets_mode = None;
        let mut words = words.into_iter();
        let seq = loop {
            match words.next().as_deref() {
                Some("--preset") => preset = true,
                Some("--user" | "-s" | "--silent") => {}
                Some("-k" | "--key") => named = true,
                Some("-M" | "--mode") => mode = words.next().unwrap_or_default(),
                Some("-m" | "--sets-mode") => sets_mode = words.next(),
                Some("--") => break words.next(),
                Some(w) if w.starts_with('-') && w.len() > 1 => break None,
                w => break w.map(str::to_string),
            }
        };
        let commands: Vec<String> = words.collect();
        let Some(seq) = seq else { continue };
        if seq.is_empty() || commands.is_empty() || commands[0] == "self-insert" {
            continue;
        }
        let Ok(mode) = mode.parse::<Name>() else {
            skipped += 1;
            continue;
        };
        if !named && is_report(&seq.chars().collect::<Vec<_>>()) {
            continue;
        }
        let key = if named {
            terminfo_name(&seq)
        } else {
            fish_key(&seq)
        };
        let Some(key) = key else {
            skipped += 1;
            continue;
        };
        // Commands run in turn; fish 4's `or` / `and` join the ones around them.
        let mut desc = String::new();
        for (i, c) in commands.iter().enumerate() {
            let joins = |w: &str| matches!(w, "or" | "and");
            if i > 0 {
                desc.push_str(if joins(c) || joins(&commands[i - 1]) {
                    " "
                } else {
                    "; "
                });
            }
            desc.push_str(c);
        }
        // vi's `i`, `a`, `Escape`: switching mode is what the key is for.
        if let Some(m) = sets_mode {
            desc = match desc.trim() {
                "" => format!("{m} mode"),
                d => format!("{d}, then {m} mode"),
            };
        }
        if desc.trim().is_empty() {
            continue;
        }
        let desc = printable(&desc);
        found.push((preset, key, mode, desc));
    }
    for (preset, key, mode, desc) in &found {
        let overridden = || found.iter().any(|(p, k, m, _)| !p && k == key && m == mode);
        if *preset && overridden() {
            continue;
        }
        let mut b = bind(key.clone(), desc.clone());
        if let Ok(scope) = "mode".parse::<Name>() {
            b.when.insert(scope, mode.clone().into());
        }
        push(&mut sheet, "line editing", b);
    }
    (sheet, skipped)
}

/// Control characters (from a command's own `\e`) written visibly, so a description stays one readable line.
fn printable(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ESC => "\\e".to_string(),
            c if c.is_control() => format!("\\x{:02x}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

/// A fish key: fish 4's names (`ctrl-x`, `alt-left`, `enter`, `ctrl-x,ctrl-g`), or the characters fish 3 (and fish 4
/// for raw sequences) writes with escapes (`\cx`, `\ex`, `\e\[A`, `\;`), already unescaped by `fish_words`.
fn fish_key(seq: &str) -> Option<String> {
    let chars: Vec<char> = seq.chars().collect();
    if chars.len() == 1 || chars.iter().any(|c| c.is_control()) {
        return keys(&chars);
    }
    let named = chords(seq)
        .into_iter()
        .map(|part| {
            let mut mods = Vec::new();
            let mut rest = part;
            while let Some((m @ ("ctrl" | "alt" | "shift" | "super"), r)) =
                rest.split_once('-').filter(|(_, r)| !r.is_empty())
            {
                mods.push(m);
                rest = r;
            }
            let mut cs = rest.chars();
            let sym = match (cs.next(), cs.next()) {
                (Some(c), None) => super::nvim::single(c)?,
                _ => named_sym(rest)?,
            };
            Some(chord(&mods, sym))
        })
        .collect::<Option<Vec<_>>>();
    match named {
        Some(chords) => Some(chords.join(" ")),
        // fish 3 writes a sequence as typed (vi's `gg`, `d$`, `d0`, an insert-mode `jk`); fish 4 would write `g,g`.
        None if !seq.contains(['-', ',']) => keys(&chars),
        None => None,
    }
}

/// fish 4's `,`-separated chords. A `,` right after a modifier's `-` (or first in a chord) is the comma key: fish
/// writes `alt-\,`, which `fish_words` has already unescaped.
fn chords(seq: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in seq.char_indices() {
        let part = &seq[start..i];
        if c == ',' && !part.is_empty() && !part.ends_with('-') {
            parts.push(part);
            start = i + 1;
        }
    }
    parts.push(&seq[start..]);
    parts
}

/// fish 4's key names.
fn named_sym(name: &str) -> Option<&'static str> {
    Some(match name {
        "enter" => "Return",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "escape" => "Escape",
        "space" => "space",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Prior",
        "pagedown" => "Next",
        "insert" => "Insert",
        "minus" => "minus",
        "comma" => "comma",
        f => *FKEYS.iter().find(|k| k.eq_ignore_ascii_case(f))?,
    })
}

/// X has keysyms up to F35.
const FKEYS: [&str; 35] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "F25", "F26", "F27", "F28",
    "F29", "F30", "F31", "F32", "F33", "F34", "F35",
];

/// fish 3's `bind -k` terminfo names.
fn terminfo_name(name: &str) -> Option<String> {
    let (mods, sym): (&[&str], &str) = match name {
        "ppage" => (&[], "Prior"),
        "npage" => (&[], "Next"),
        "btab" => (&["shift"], "Tab"),
        "sright" => (&["shift"], "Right"),
        "sleft" => (&["shift"], "Left"),
        "dc" => (&[], "Delete"),
        "sdc" => (&["shift"], "Delete"),
        "ic" => (&[], "Insert"),
        "nul" => (&["ctrl"], "space"),
        "sr" => (&["shift"], "Up"),
        "sf" => (&["shift"], "Down"),
        "shome" => (&["shift"], "Home"),
        "send" => (&["shift"], "End"),
        n => (&[], named_sym(n)?),
    };
    Some(chord(mods, sym))
}

/// A `bind` line's words with fish's quoting removed: `'...'` (only `\'` and `\\` escape), `"..."` (`\"`, `\$`,
/// `\\`) and bare words with fish's backslash escapes (`\e`, `\cx`, `\x1f`, `\n`, octal, `\;`). `None` if a quote
/// isn't closed.
fn fish_words(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| *c == ' ').is_some() {}
        if chars.peek().is_none() {
            return Some(words);
        }
        let mut word = String::new();
        while let Some(c) = chars.next_if(|c| *c != ' ') {
            match c {
                '\'' => loop {
                    match chars.next()? {
                        '\'' => break,
                        '\\' if matches!(chars.peek(), Some('\'' | '\\')) => {
                            word.extend(chars.next())
                        }
                        c => word.push(c),
                    }
                },
                '"' => loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '$' | '\\')) => {
                            word.extend(chars.next())
                        }
                        c => word.push(c),
                    }
                },
                '\\' => word.push(fish_escape(&mut chars)?),
                c => word.push(c),
            }
        }
        words.push(word);
    }
}

/// One escape after a bare `\`.
fn fish_escape(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<char> {
    let e = chars.next()?;
    let digits = |chars: &mut std::iter::Peekable<std::str::Chars>, radix: u32, max: usize| {
        let mut n = 0;
        for _ in 0..max {
            match chars.peek().and_then(|c| c.to_digit(radix)) {
                Some(d) => {
                    n = n * radix + d;
                    chars.next();
                }
                None => break,
            }
        }
        char::from_u32(n)
    };
    Some(match e {
        'a' => '\u{7}',
        'b' => '\u{8}',
        'e' => ESC,
        'f' => '\u{c}',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\u{b}',
        'x' | 'X' => digits(chars, 16, 2)?,
        'u' => digits(chars, 16, 4)?,
        'U' => digits(chars, 16, 8)?,
        'c' => ctrl(chars.next()?)?,
        '0'..='7' => {
            // Up to three digits in all.
            let rest = std::iter::from_fn(|| chars.next_if(|c| c.is_digit(8)))
                .take(2)
                .collect::<String>();
            char::from_u32(u32::from_str_radix(&format!("{e}{rest}"), 8).ok()?)?
        }
        e => e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shells")
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(dir().join(name)).unwrap()
    }

    fn get<'a>(s: &'a Sheet, category: &str, key: &str) -> Vec<&'a Bind> {
        s.binds
            .get(category)
            .map(|l| l.iter().filter(|b| b.key == key).collect())
            .unwrap_or_default()
    }

    fn desc(s: &Sheet, category: &str, key: &str) -> String {
        match get(s, category, key).as_slice() {
            [b] => b.desc.clone(),
            other => panic!("{key}: {other:?}"),
        }
    }

    fn mode(b: &Bind) -> String {
        b.when
            .values()
            .flat_map(|v| v.iter())
            .map(|n| n.to_string())
            .collect()
    }

    /// Skips (with a message) when `shell` isn't on PATH, as the xterm test in `context.rs` does.
    fn have(shell: &str) -> bool {
        let found = Command::new("sh")
            .args(["-c", "command -v \"$1\"", "sh", shell])
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            eprintln!("skipping: {shell} is not installed");
        }
        found
    }

    #[test]
    fn readline_key_translation() {
        for (seq, want) in [
            (r"\C-a", "ctrl+a"),
            (r"\C-x\C-g", "ctrl+x ctrl+g"),
            (r"\e\C-g", "ctrl+alt+g"),
            (r"\M-f", "alt+f"),
            (r"\eb", "alt+b"),
            (r"\e", "Escape"),
            (r"\e\e", "alt+Escape"),
            (r"\e[A", "Up"),
            (r"\eOD", "Left"),
            (r"\e[H", "Home"),
            (r"\eOF", "End"),
            (r"\e[3~", "Delete"),
            (r"\e[2~", "Insert"),
            (r"\e[5~", "Prior"),
            (r"\e[6~", "Next"),
            (r"\e[1~", "Home"),
            (r"\e[1;5C", "ctrl+Right"),
            (r"\e[1;3D", "alt+Left"),
            (r"\e[5D", "ctrl+Left"),
            (r"\e\e[D", "alt+Left"),
            (r"\e[3;5~", "ctrl+Delete"),
            (r"\e[15~", "F5"),
            (r"\eOP", "F1"),
            (r"\C-m", "Return"),
            (r"\C-i", "Tab"),
            (r"\C-?", "BackSpace"),
            (r"\177", "BackSpace"),
            (r"\e\C-?", "alt+BackSpace"),
            (r"\C-]", "ctrl+bracketright"),
            (r"\C-_", "ctrl+underscore"),
            (r"\C-@", "ctrl+space"),
            (r"\\", "backslash"),
            (r#"\""#, "quotedbl"),
            (r"\e.", "alt+period"),
            (r"\e<", "alt+less"),
            (r"\C-xe", "ctrl+x e"),
            (r"\e[", "alt+bracketleft"),
            (r"\x41", "A"),
            // An ESC before a sequence that has alt already is still one alt.
            (r"\e\e[1;3D", "alt+Left"),
            (r"\e[1;11C", "alt+Right"),
            (r"\e[97;5u", "ctrl+a"),
            // In the kitty protocol, 8 is super.
            (r"\e[97;9u", "super+a"),
            (r"\e[97;8u", "ctrl+alt+shift+a"),
            // Caps Lock (64) is ignored.
            (r"\e[97;65u", "a"),
            (r"\x1b[A", "Up"),
            (r"\M-\C-x", "ctrl+alt+x"),
        ] {
            assert_eq!(
                decode(seq, false).as_deref().and_then(keys).as_deref(),
                Some(want),
                "{seq}"
            );
        }
        for seq in [
            r"\e[200~",
            r"\e[?1004h",
            r"\e[I",
            r"\200",
            "ö",
            r"\e[99~",
            r"\x",
            r"\e[97;17u",
            r"\e[97;33u",
        ] {
            assert_eq!(decode(seq, false).as_deref().and_then(keys), None, "{seq}");
        }
    }

    #[test]
    fn zsh_key_translation() {
        for (seq, want) in [
            ("^R", "ctrl+r"),
            ("^[b", "alt+b"),
            ("^[^D", "ctrl+alt+d"),
            ("^?", "BackSpace"),
            ("^[^?", "alt+BackSpace"),
            ("^X^U", "ctrl+x ctrl+u"),
            (r"\M-x", "alt+x"),
            (r"\C-x", "ctrl+x"),
            ("^[[3~", "Delete"),
            ("^[OA", "Up"),
            ("^[[1;5C", "ctrl+Right"),
            // As zsh prints them, double-quoted for the shell.
            (r"\\\\", "backslash"),
            (r"^\\\\", "ctrl+backslash"),
            (r"^[\$", "alt+dollar"),
            (r#"^[\""#, "alt+quotedbl"),
            ("^[ ", "alt+space"),
            ("^@", "ctrl+space"),
            ("^I", "Tab"),
            ("^M", "Return"),
            ("^J", "ctrl+j"),
            (r"^[\M-a", "alt+a"),
        ] {
            assert_eq!(
                decode(&unquote(seq), true)
                    .as_deref()
                    .and_then(keys)
                    .as_deref(),
                Some(want),
                "{seq}"
            );
        }
        // zsh prints a UTF-8 character as meta bytes: `é` is `\M-C\M-)`, a character with no keysym, not two alt keys.
        assert_eq!(decode(r"\M-C\M-)", true), Some(vec!['é']));
        // A stray meta byte after a character stays alt; the character is still one character.
        assert_eq!(decode(r"\M-C\M-)\M-a", true), Some(vec!['é', ESC, 'a']));
        let (s, skipped) = zsh("bindkey \"\\M-C\\M-)\" w\n");
        assert!(s.binds.is_empty());
        assert_eq!(skipped, 1);
        assert_eq!(
            decode(&unquote("^[[200~"), true).as_deref().and_then(keys),
            None
        );
        // bash has no caret notation: `^` is a key there.
        assert_eq!(
            decode("^R", false).as_deref().and_then(keys).as_deref(),
            Some("asciicircum R")
        );
    }

    #[test]
    fn fish_key_translation() {
        let w = |s: &str| fish_words(s).unwrap().remove(0);
        for (seq, want) in [
            // fish 4
            ("ctrl-x", "ctrl+x"),
            ("alt-left", "alt+Left"),
            ("enter", "Return"),
            ("ctrl-shift-z", "ctrl+shift+z"),
            ("alt-.", "alt+period"),
            ("ctrl-/", "ctrl+slash"),
            (r"ctrl-\[", "ctrl+bracketleft"),
            (r"alt-\<", "alt+less"),
            ("alt-#", "alt+numbersign"),
            ("ctrl-alt-h", "ctrl+alt+h"),
            ("shift-tab", "shift+Tab"),
            ("pageup", "Prior"),
            ("f1", "F1"),
            ("ctrl-x,ctrl-e", "ctrl+x ctrl+e"),
            ("alt--", "alt+minus"),
            ("-", "minus"),
            ("f13", "F13"),
            (r"alt-\,", "alt+comma"),
            (r"ctrl-x,alt-\,", "ctrl+x alt+comma"),
            // fish 3's typed letters: vi's `gg` and an insert-mode `jk`.
            ("gg", "g g"),
            ("jk", "j k"),
            (r"\u001b\[A", "Up"),
            (r"\X1b\[B", "Down"),
            (r"\e\[1\;9C", "alt+Right"),
            (r"\;", "semicolon"),
            // fish 3
            (r"\cx", "ctrl+x"),
            (r"\ex", "alt+x"),
            (r"\e\[A", "Up"),
            (r"\e\eOC", "alt+Right"),
            (r"\e\x7f", "alt+BackSpace"),
            (r"\x1f", "ctrl+underscore"),
            (r"\e\[13\;5u", "ctrl+Return"),
            (r"\e\[27\;2\;13\~", "shift+Return"),
            (r"\e\[32\;2u", "shift+space"),
            (r"\r", "Return"),
            (r"\t", "Tab"),
            (r"\e", "Escape"),
            ("';'", "semicolon"),
            (r"\e\#", "alt+numbersign"),
        ] {
            assert_eq!(fish_key(&w(seq)).as_deref(), Some(want), "{seq}");
        }
        // fish 3 vi sequences with digits and symbols.
        assert_eq!(fish_key("d$").as_deref(), Some("d dollar"));
        assert_eq!(fish_key("d0").as_deref(), Some("d 0"));
        for seq in [r"\e\[I", r"\e\[200\~", "hyper-x", "ctrl-", "ctrl-x,é"] {
            assert_eq!(fish_key(&w(seq)), None, "{seq}");
        }
        assert_eq!(terminfo_name("btab").as_deref(), Some("shift+Tab"));
        assert_eq!(terminfo_name("ppage").as_deref(), Some("Prior"));
        assert_eq!(terminfo_name("f1").as_deref(), Some("F1"));
        // A comma first in a chord, or right after a modifier, is the comma key.
        assert_eq!(chords(",,a"), [",", "a"]);
        assert_eq!(chords("alt-,,ctrl-x"), ["alt-,", "ctrl-x"]);
        assert_eq!(terminfo_name("sr").as_deref(), Some("shift+Up"));
        assert_eq!(terminfo_name("send").as_deref(), Some("shift+End"));
        assert_eq!(terminfo_name("nosuch"), None);
    }

    #[test]
    fn fish_words_unquote() {
        assert_eq!(
            fish_words(r#"--preset alt-d 'if test "$(commandline; printf .)" = \\n.; end'"#)
                .unwrap(),
            [
                "--preset",
                "alt-d",
                r#"if test "$(commandline; printf .)" = \n.; end"#
            ]
        );
        assert_eq!(
            fish_words(r#"x "a \"b\" \$c" d\ e \101"#).unwrap(),
            ["x", r#"a "b" $c"#, "d e", "A"]
        );
        assert_eq!(fish_words(r"\0").unwrap(), ["\0"]);
        assert_eq!(fish_words(r"\177").unwrap(), ["\u{7f}"]);
        assert_eq!(fish_words("a 'open"), None);
        assert_eq!(fish_words(""), Some(vec![]));
    }

    #[test]
    fn bash_fixtures() {
        let out = [
            fixture("bash-bind-p.txt"),
            fixture("bash-bind-s.txt"),
            format!("{COMMANDS}\n"),
            fixture("bash-bind-X.txt"),
        ]
        .concat();
        let (s, skipped) = bash(&out);
        assert_eq!(desc(&s, "line editing", "ctrl+a"), "beginning-of-line");
        assert_eq!(desc(&s, "line editing", "ctrl+r"), "reverse-search-history");
        // bash names readline's yank-last-arg `insert-last-argument` too, and `bind -p` lists the key under both.
        let dot: Vec<&str> = get(&s, "line editing", "alt+period")
            .iter()
            .map(|b| b.desc.as_str())
            .collect();
        assert_eq!(dot, ["insert-last-argument", "yank-last-arg"]);
        assert_eq!(desc(&s, "line editing", "Delete"), "delete-char");
        assert_eq!(desc(&s, "line editing", "alt+f"), "forward-word");
        assert_eq!(desc(&s, "macros", "ctrl+x h"), r"macro: hello world\C-j");
        assert_eq!(desc(&s, "commands", "ctrl+x g"), "runs: echo hi");
        let all: Vec<&Bind> = s.binds.values().flatten().collect();
        assert!(
            all.iter().all(|b| b.desc != "self-insert"),
            "self-insert is left out"
        );
        assert!(all.iter().all(|b| b.desc != "do-lowercase-version"));
        assert!(!all.iter().any(|b| b.desc.contains("not bound")));
        assert!(
            all.iter().all(|b| b.when.is_empty()),
            "bash carries no scope"
        );
        // `\e[D` and `\eOD` are one row.
        assert_eq!(get(&s, "line editing", "Left").len(), 1);
        // Bracketed paste is bound, but is a terminal report, not a key: left out without a warning.
        assert_eq!(skipped, 0);
        assert!(all.iter().all(|b| !b.desc.starts_with("bracketed-paste")));
        assert_eq!(s.provider, "bash");
    }

    #[test]
    fn bash_5_2_commands_have_a_colon() {
        // bash 5.2.15 (Debian bookworm) prints `bind -X` with a colon, the shape of a `bind -s` macro.
        let (s, _) = bash(&format!(
            "\"\\C-xh\": \"hello\"\n{COMMANDS}\n\"\\C-xg\": \"echo hi\"\n"
        ));
        assert_eq!(desc(&s, "commands", "ctrl+x g"), "runs: echo hi");
        assert_eq!(desc(&s, "macros", "ctrl+x h"), "macro: hello");
        assert!(!s.binds["macros"].iter().any(|b| b.key == "ctrl+x g"));
    }

    #[test]
    fn bash_lines_malformed_and_untranslatable() {
        let (s, skipped) = bash(concat!(
            "# abort (not bound)\n",
            "\"\\C-a\": self-insert\n",
            "\"\\e[200~\": bracketed-paste-begin\n",
            "\"\\e[99~\": untranslatable\n",
            "\"\\C-q\n",
            "\"\\C-q\" junk\n",
            "\"\\C-q\": \"open\n",
            "garbage\n",
            "\n",
            "\"\\C-b\": backward-char\n",
        ));
        assert_eq!(skipped, 1);
        assert_eq!(s.binds.values().flatten().count(), 1, "{:?}", s.binds);
    }

    #[test]
    fn zsh_fixture() {
        let (s, skipped) = zsh(&fixture("zsh-bindkey-L.txt"));
        assert_eq!(
            desc(&s, "line editing", "ctrl+r"),
            "history-incremental-search-backward"
        );
        assert_eq!(desc(&s, "line editing", "alt+b"), "backward-word");
        assert_eq!(desc(&s, "line editing", "alt+period"), "insert-last-word");
        assert_eq!(desc(&s, "line editing", "ctrl+x ctrl+u"), "undo");
        assert_eq!(desc(&s, "macros", "ctrl+x h"), r#"macro: echo "hi"^J"#);
        assert!(s.binds.values().flatten().all(|b| b.desc != "self-insert"));
        assert_eq!(skipped, 0, "bracketed paste is a report, not a key");
        let (s, skipped) = zsh("bindkey -R \" \"-\"~\" self-insert\nbindkey \"^[[I\" x\nbindkey \"^[[99~\" x\nbindkey -s \"^A\"\nbindkey\nbindkey \"^A\" a b\n");
        assert!(s.binds.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn fish_fixtures() {
        // fish 4.9.3 (Arch), from a neutral HOME, with the prompt event; raw, banner and terminal escapes included.
        let out = fixture("fish4-bind.txt");
        let at = out.rfind(SENTINEL).unwrap();
        let (s, skipped) = fish(&out[at + SENTINEL.len()..]);
        let one = |key: &str| {
            let b = get(&s, "line editing", key);
            assert_eq!(b.len(), 1, "{key}: {b:?}");
            (b[0].desc.clone(), mode(b[0]))
        };
        assert_eq!(one("ctrl+r"), ("history-pager".into(), "default".into()));
        assert_eq!(
            one("alt+Left"),
            ("prevd-or-backward-word".into(), "default".into())
        );
        assert_eq!(one("ctrl+bracketleft"), ("cancel".into(), "default".into()));
        assert_eq!(
            one("ctrl+l").0,
            "status test-terminal-feature scroll-content-up && commandline -f scrollback-push; clear-screen"
        );
        assert!(
            get(&s, "line editing", "space").is_empty(),
            "self-insert is left out"
        );
        assert_eq!(skipped, 0);
        // fish 3.6.0 (Debian bookworm): the escaped spellings, `-k` names and the paste mode.
        let out = fixture("fish3-bind.txt");
        let at = out.rfind(SENTINEL).unwrap();
        let (s, skipped) = fish(&out[at + SENTINEL.len()..]);
        assert_eq!(desc(&s, "line editing", "ctrl+r"), "history-pager");
        assert_eq!(desc(&s, "line editing", "shift+Tab"), "complete-and-search");
        assert_eq!(
            desc(&s, "line editing", "alt+period"),
            "history-token-search-backward"
        );
        assert_eq!(desc(&s, "line editing", "ctrl+underscore"), "undo");
        assert_eq!(desc(&s, "line editing", "Prior"), "beginning-of-history");
        assert_eq!(
            desc(&s, "line editing", "ctrl+l"),
            r#"echo -n (clear | string replace \e\[3J ""); commandline -f repaint"#
        );
        let paste = get(&s, "line editing", "Return");
        assert!(paste.iter().any(|b| mode(b) == "paste"), "{paste:?}");
        assert!(paste
            .iter()
            .any(|b| mode(b) == "default" && b.desc == "execute"));
        // Bracketed paste start/end, the two focus reports and a mode reply are reports: left out, not counted.
        assert_eq!(skipped, 0);
        assert!(get(&s, "line editing", "Escape")
            .iter()
            .all(|b| b.desc == "cancel"));
    }

    #[test]
    fn fish_user_binds_replace_presets_per_mode() {
        let (s, skipped) = fish(concat!(
            "bind --preset ctrl-g cancel\n",
            "bind --preset -M insert ctrl-g cancel\n",
            "bind ctrl-g 'echo hi'\n",
            "bind --preset -M visual ctrl-g cancel\n",
            "bind -M insert -m default ctrl-o 'echo two' repaint\n",
            "bind -M default -m insert i repaint-mode\n",
            "bind -M insert -m default jk ''\n",
            "bind ctrl-e ''\n",
            "bind --preset '' self-insert\n",
            "bind --bogus x y\n",
            "bind --preset hyper-x y\n",
            "bind -M 'bad mode' ctrl-a y\n",
            "bind \\e\\[200\\~ __fish_start_bracketed_paste\n",
            "bind 'open\n",
            "bind ctrl-a\n",
            "\u{1b}[?2004l\n",
        ));
        let g = get(&s, "line editing", "ctrl+g");
        let got: Vec<(String, String)> = g.iter().map(|b| (b.desc.clone(), mode(b))).collect();
        assert_eq!(
            got,
            [
                ("cancel".into(), "insert".into()),
                ("echo hi".into(), "default".into()),
                ("cancel".into(), "visual".into())
            ]
        );
        let o = get(&s, "line editing", "ctrl+o");
        assert_eq!(
            (o[0].desc.as_str(), mode(o[0])),
            ("echo two; repaint, then default mode", "insert".into())
        );
        // `hyper-x` and the mode that isn't a name.
        assert_eq!(skipped, 2);
        let i = get(&s, "line editing", "i");
        assert_eq!(i[0].desc, "repaint-mode, then insert mode");
        assert_eq!(desc(&s, "line editing", "j k"), "default mode");
        assert!(
            get(&s, "line editing", "ctrl+e").is_empty(),
            "an empty command is no row"
        );
        assert_eq!(s.binds.values().flatten().count(), 6);
    }

    fn home(cmd: &mut Command, dir: &Path) {
        cmd.env("HOME", dir)
            .env("ZDOTDIR", dir)
            .env("XDG_CONFIG_HOME", dir)
            .env_remove("INPUTRC")
            .env_remove("BASH_ENV")
            .env_remove("ENV");
    }

    #[test]
    fn real_bash_reads_inputrc_and_bashrc() {
        if !have("bash") {
            return;
        }
        let h = dir().join("home");
        let s = read("bash", TIMEOUT, |c| home(c, &h)).unwrap();
        // `~/.inputrc` replaces the default `beginning-of-line`.
        assert_eq!(desc(&s, "line editing", "ctrl+a"), "end-of-line");
        assert_eq!(desc(&s, "macros", "ctrl+x h"), r"macro: hello world\C-j");
        // `KEYBIND_FIND` reached the rc file, which wrote it into a bind; the banner before the sentinel didn't count.
        assert_eq!(desc(&s, "commands", "ctrl+x k"), "runs: echo kbf=1");
        assert!(
            get(&s, "line editing", "ctrl+x q").is_empty(),
            "the banner's fake bind"
        );
        // `INPUTRC` names another file, which wins over `~/.inputrc`.
        let s = read("bash", TIMEOUT, |c| {
            home(c, &h);
            c.env("INPUTRC", dir().join("inputrc-alt"));
        })
        .unwrap();
        assert_eq!(desc(&s, "line editing", "ctrl+a"), "kill-line");
    }

    #[test]
    fn real_zsh_reads_zshrc() {
        if !have("zsh") {
            return;
        }
        let h = dir().join("home");
        let s = read("zsh", TIMEOUT, |c| home(c, &h)).unwrap();
        assert_eq!(desc(&s, "line editing", "ctrl+x t"), "transpose-words");
        assert_eq!(desc(&s, "macros", "ctrl+x k"), "macro: kbf=1");
        assert!(
            get(&s, "line editing", "ctrl+x q").is_empty(),
            "the banner's fake bind"
        );
    }

    #[test]
    fn real_fish_reads_config_fish() {
        assert!(
            script("fish").unwrap().starts_with("emit fish_prompt;"),
            "plain `fish -c bind` lists nothing"
        );
        if !have("fish") {
            return;
        }
        let h = dir().join("home");
        let s = read("fish", TIMEOUT, |c| home(c, &h)).unwrap();
        // The user's bind replaced fish 4's preset `ctrl-g cancel`; the banner's fake bind isn't read.
        assert_eq!(desc(&s, "line editing", "ctrl+g"), "echo kbf=1");
        assert_eq!(mode(get(&s, "line editing", "ctrl+g")[0]), "default");
        assert!(get(&s, "line editing", "ctrl+q").is_empty());
        assert!(
            !get(&s, "line editing", "ctrl+r").is_empty(),
            "the presets are loaded"
        );
    }

    /// Whether any process has `arg` in its argv.
    fn running(arg: &str) -> bool {
        std::fs::read_dir("/proc").unwrap().flatten().any(|p| {
            std::fs::read(p.path().join("cmdline"))
                .is_ok_and(|c| c.split(|b| *b == 0).any(|a| a == arg.as_bytes()))
        })
    }

    #[test]
    fn a_hanging_rc_file_times_out_and_is_killed() {
        let h = dir().join("sleep");
        // `.bashrc` runs `sleep 30.25`; `.zshrc` does `exec sleep 30.5`.
        for (shell, arg, h) in [
            ("bash", "30.25", h.clone()),
            ("zsh", "30.5", h),
            // `.bashrc` there does `exec sleep 30.75`.
            ("bash", "30.75", dir().join("exec-sleep")),
        ] {
            if !have(shell) {
                continue;
            }
            let start = std::time::Instant::now();
            let err = format!(
                "{:#}",
                read(shell, Duration::from_millis(500), |c| home(c, &h)).unwrap_err()
            );
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{shell}: {:?}",
                start.elapsed()
            );
            assert!(
                err.contains(&format!("starting {shell} -ic"))
                    && err.contains("did not answer within 0.5 s"),
                "{err}"
            );
            // SIGKILL is sent before the error returns; give the kernel a moment to finish the processes off.
            let gone = (0..100).any(|_| {
                std::thread::sleep(Duration::from_millis(10));
                !running(arg)
            });
            assert!(gone, "{shell}: the rc file's sleep outlived the timeout");
        }
    }

    #[test]
    fn a_background_job_holding_stdout_keeps_the_keymap_and_is_killed() {
        let h = dir().join("bg");
        // Each rc file starts `sleep <arg> &` with its output still on the shell's stdout.
        for (shell, arg) in [("bash", "30.125"), ("zsh", "30.375")] {
            if !have(shell) {
                continue;
            }
            let s = read(shell, TIMEOUT, |c| home(c, &h)).unwrap();
            assert_eq!(
                desc(&s, "line editing", "ctrl+x b"),
                "backward-word",
                "{shell}"
            );
            let gone = (0..100).any(|_| {
                std::thread::sleep(Duration::from_millis(10));
                !running(arg)
            });
            assert!(gone, "{shell}: the background job outlived the read");
        }
    }

    #[test]
    fn missing_or_unknown_shells_are_errors_naming_them() {
        let e = read("bash", TIMEOUT, |c| {
            c.env("PATH", "/nonexistent");
        })
        .unwrap_err();
        let err = format!("{e:#}");
        // Through setsid, its own message; without it, the spawn's.
        let not_found = err.contains("failed to execute bash: No such file")
            || e.chain().any(|c| {
                c.downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            });
        assert!(err.contains("bash") && not_found, "{err}");
        assert!(sheet("csh").unwrap_err().to_string().contains("csh"));
        // A shell that prints no sentinel (an rc file that `exec`s something quick) names the shell.
        if have("bash") {
            let err = read("bash", TIMEOUT, |c| {
                home(c, &dir().join("exec"));
            })
            .unwrap_err()
            .to_string();
            assert!(err.starts_with("bash printed no keymap"), "{err}");
        }
    }

    /// A stand-in shell on PATH that prints `fixture` (after a sentinel, fake binds and a second sentinel: only the *last*
    /// one counts), so `read`'s whole path runs for every shell, fish included, wherever the tests run.
    fn fake(shell: &str, fixture: &str) -> anyhow::Result<Sheet> {
        let bin = std::env::temp_dir().join(format!("kbf-shells-{shell}-{}", std::process::id()));
        std::fs::create_dir_all(&bin)?;
        let path = bin.join(shell);
        // Written by `sh`, not by this process: a write handle another test thread's fork inherited would make
        // running the script fail with ETXTBSY (as `plugins.rs`'s `write_script` says).
        use std::io::Write;
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        sh.stdin
            .take()
            .context("no stdin pipe")?
            .write_all("#!/bin/sh\n[ \"$1\" = -ic ] && [ \"$KEYBIND_FIND\" = 1 ] && [ \"$MISE_NOT_FOUND_AUTO_INSTALL\" = 0 ] || exit 3\ncase \"$2\" in *KEYBIND-FIND-KEYMAP*bind*) ;; *) exit 4 ;; esac\n(: </dev/tty) 2>/dev/null && exit 5\nprintf '%s\\n' KEYBIND-FIND-KEYMAP '\"\\C-xq\": fake' 'bindkey \"^Xq\" fake' 'bind ctrl-q fake' KEYBIND-FIND-KEYMAP\ncat \"$KBF_FIXTURE\"\n".as_bytes())?;
        anyhow::ensure!(sh.wait()?.success(), "writing the fake shell");
        let s = read(shell, TIMEOUT, |c| {
            c.env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env("KBF_FIXTURE", dir().join(fixture));
        });
        let _ = std::fs::remove_dir_all(&bin);
        s
    }

    #[test]
    fn every_shell_through_read() {
        let s = fake("fish", "fish4-bind.txt").unwrap();
        assert_eq!(desc(&s, "line editing", "ctrl+r"), "history-pager");
        assert!(
            get(&s, "line editing", "ctrl+q").is_empty(),
            "read after the last sentinel"
        );
        let s = fake("bash", "bash-bind-p.txt").unwrap();
        assert_eq!(desc(&s, "line editing", "ctrl+a"), "beginning-of-line");
        assert!(get(&s, "line editing", "ctrl+x q").is_empty());
        let s = fake("zsh", "zsh-bindkey-L.txt").unwrap();
        assert_eq!(
            desc(&s, "line editing", "ctrl+r"),
            "history-incremental-search-backward"
        );
        assert!(get(&s, "line editing", "ctrl+x q").is_empty());
    }

    #[test]
    fn zsh_vi_mode_shows_the_main_keymap_only() {
        if !have("zsh") {
            return;
        }
        let s = read("zsh", TIMEOUT, |c| home(c, &dir().join("zsh-vi"))).unwrap();
        assert_eq!(desc(&s, "line editing", "ctrl+x i"), "undo");
        assert!(
            get(&s, "line editing", "ctrl+x v").is_empty(),
            "vicmd isn't shown"
        );
    }

    #[test]
    fn cut_lines_never_panic() {
        for name in [
            "bash-bind-p.txt",
            "bash-bind-s.txt",
            "bash-bind-X.txt",
            "zsh-bindkey-L.txt",
            "fish3-bind.txt",
            "fish4-bind.txt",
        ] {
            for line in fixture(name).lines() {
                for (i, _) in line.char_indices().chain([(line.len(), ' ')]) {
                    let cut = &line[..i];
                    let _ = (bash(cut), zsh(cut), fish(cut));
                }
            }
        }
        // Long runs of what the decoders recurse on.
        for run in ["\\e", "\\M-", "^", "\\C-", "\\", "\\x", "\\c"] {
            let seq = run.repeat(200);
            let _ = (
                bash(&format!("\"{seq}\": x")),
                zsh(&format!("bindkey \"{seq}\" x")),
                fish(&format!("bind {seq} x")),
            );
        }
    }
}
