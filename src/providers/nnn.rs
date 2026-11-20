//! nnn: its stock keys pinned per version (`data/nnn/`), plus the plugin (`;` + key) and bookmark (`b` + key) keys of
//! `NNN_PLUG` and `NNN_BMS`, read from the environment of the nnn in the focused terminal. nnn's keys are compiled in
//! (`src/nnn.h` `bindings[]`), so a patched build isn't seen. Plugin commands and paths are shown, never run.

use super::nvim::single;
use super::pinned::{self, parse_version};
use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use serde::Deserialize;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const REGISTRY: &[(&str, &str)] = &[(
    "5.3",
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/nnn/5.3.toml")),
)];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// A variable's raw value from keybind-find's own environment (a test seam).
type Env<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;

#[derive(Deserialize)]
struct Pinned {
    bind: Vec<Entry>,
}

/// One `bindings[]` entry: its key as nnn.h writes it, its `SEL_…` action, what it does and its help screen section.
#[derive(Deserialize)]
struct Entry {
    sym: String,
    action: String,
    desc: String,
    category: String,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| {
        use std::os::unix::ffi::OsStringExt;
        std::env::var_os(k).map(OsStringExt::into_vec)
    };
    let (sheet, notes) = sheet_with(
        REGISTRY,
        |_| {},
        crate::context::editor_pid(),
        Path::new("/proc"),
        &env,
    )?;
    for note in notes {
        eprintln!("keybind-find: nnn: {note}");
    }
    Ok(sheet)
}

/// The sheet for the nnn at `pid` (none: `dump nnn` from a shell), its process read under `proc`; `setup` is applied
/// to `nnn -V` and `env` is keybind-find's own environment (test seams). Also the notes.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    pid: Option<u32>,
    proc: &Path,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let is_nnn = |dir: &Path| {
        std::fs::read_to_string(dir.join("comm")).is_ok_and(|c| c.trim_end_matches('\n') == "nnn")
    };
    // The editor pid context found is nnn's only when nnn leads the terminal; check its comm.
    let dir = pid.map(|p| proc.join(p.to_string())).filter(|d| is_nnn(d));
    // The focused nnn's own binary, else `nnn` from PATH; without either, the newest pinned file with a note.
    let ask = |program: &Path| {
        let mut cmd = Command::new(program);
        cmd.arg("-V")
            .env("LC_ALL", "C")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
        setup(&mut cmd);
        run_for(cmd, VERSION_TIMEOUT).ok()
    };
    let version = dir
        .as_ref()
        .and_then(|d| ask(&d.join("exe")))
        .or_else(|| ask(Path::new("nnn")));
    let version = version
        .as_deref()
        .map(str::trim)
        .filter(|v| parse_version(v).is_some());
    let (v, text, note) = pinned::select("nnn", registry, version).context("no pinned nnn keys")?;
    let pin: Pinned = toml::from_str(text).with_context(|| format!("reading data/nnn/{v}.toml"))?;
    let mut notes: Vec<String> = note.into_iter().collect();

    // nnn reads both with `getenv` once, from the environment it started with: its `/proc/<pid>/environ`. Its comm
    // is checked again after the read (a pid reused meanwhile, or a zombie's empty environ, isn't nnn's).
    let environ = dir.as_ref().and_then(|d| {
        let b = std::fs::read(d.join("environ"))
            .ok()
            .filter(|b| !b.is_empty())?;
        is_nnn(d).then_some(b)
    });
    if environ.is_none() {
        notes.push(match dir {
            Some(_) => "can't read nnn's environment; NNN_PLUG and NNN_BMS from keybind-find's".into(),
            None => "no nnn in the focused terminal; NNN_PLUG and NNN_BMS from keybind-find's environment".into(),
        });
    }
    let getenv = |k: &str| match &environ {
        Some(e) => environ_var(e, k),
        None => env(k),
    };

    let mut sheet = Sheet {
        title: "nnn".into(),
        provider: "nnn".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for e in pin.bind {
        let Some(key) = notation(&e.sym) else {
            left_out += 1;
            continue;
        };
        sheet.binds.entry(e.category).or_default().push(Bind {
            key,
            desc: e.desc,
            action: e.action,
            when: Default::default(),
            run: Vec::new(),
        });
    }
    // `lazy_parse_plug` (nnn.c:6191-6201): a bad NNN_PLUG runs no plugin; a bad NNN_BMS stops nnn (nnn.c:10503-10506).
    // `plug`: NNN_PLUG's entries, whose leading `-` is nnn's no-refresh flag; NNN_BMS's `,` is never read: after `b`,
    // `,` is nnn's marked directory (`handle_bookmark`, nnn.c:6235).
    for (name, plug, lead, category, what, bad) in [
        (
            "NNN_PLUG",
            true,
            "semicolon",
            "PLUGIN KEYS",
            "plugin",
            "no plugin keys (nnn runs none)",
        ),
        (
            "NNN_BMS",
            false,
            "b",
            "BOOKMARKS",
            "bookmark",
            "no bookmark keys (nnn won't start with it)",
        ),
    ] {
        let value = getenv(name).unwrap_or_default();
        let Some(entries) = parse_kv(&value) else {
            notes.push(format!("{name} isn't nnn's key:value;… list: {bad}"));
            continue;
        };
        for (k, value) in entries {
            let Some(key) = byte_key(k).filter(|_| plug || k != b',') else {
                left_out += 1;
                continue;
            };
            // nnn's no-refresh flag (`-`, nnn.c:9625-9627) isn't part of the plugin's name.
            let shown = match value.strip_prefix('-') {
                Some(rest) if plug && !rest.is_empty() => rest,
                _ => &value,
            };
            sheet.binds.entry(category.into()).or_default().push(Bind {
                key: format!("{lead} {key}"),
                desc: format!("{what}: {shown}"),
                action: value.clone(),
                when: Default::default(),
                run: Vec::new(),
            });
        }
    }
    if left_out > 0 {
        let keys = if left_out == 1 { "key" } else { "keys" };
        notes.push(format!(
            "leaving out {left_out} {keys} that can't be sent or that nnn never reads (no keysym, such as a control or non-ASCII byte; or a `,` bookmark, which `b ,` never reaches)"
        ));
    }
    Ok((sheet, notes))
}

/// `name`'s value in a NUL-separated environment block: the first `name=` entry, as glibc's `getenv` finds it.
fn environ_var(environ: &[u8], name: &str) -> Option<Vec<u8>> {
    environ
        .split(|&b| b == 0)
        .find_map(|kv| kv.strip_prefix(name.as_bytes())?.strip_prefix(b"="))
        .map(<[u8]>::to_vec)
}

/// nnn's `parsekvpair` (nnn.c:4493-4541) over a `NNN_PLUG`/`NNN_BMS` value: each entry one key byte, `:`, then a
/// non-empty value up to the next `;`. `None` when nnn refuses the value; unset or empty is no entries. Of two
/// entries with one key the first counts (`get_kv_val`, nnn.c:4550-4571).
fn parse_kv(v: &[u8]) -> Option<Vec<(u8, String)>> {
    let mut out: Vec<(u8, String)> = Vec::new();
    let mut rest = v;
    let mut count = 0;
    while let Some((&key, after)) = rest.split_first() {
        let after = after.strip_prefix(b":")?;
        if after.first().is_none_or(|&b| b == b';') {
            return None;
        }
        count += 1;
        // nnn stops at its 100th key before cutting its value at a `;`: that value runs to the end, and the loop with it.
        let end = match count {
            100 => None,
            _ => after.iter().position(|&b| b == b';'),
        };
        let (value, next) = match end {
            Some(i) => (&after[..i], &after[i + 1..]),
            None => (after, &after[after.len()..]),
        };
        if out.iter().all(|(k, _)| *k != key) {
            out.push((key, String::from_utf8_lossy(value).into_owned()));
        }
        rest = next;
    }
    Some(out)
}

/// A `NNN_PLUG`/`NNN_BMS` key byte as the key that types it; `None` for a control byte or one ≥ 0x80.
fn byte_key(b: u8) -> Option<String> {
    (b.is_ascii_graphic() || b == b' ')
        .then(|| printable(b as char))
        .flatten()
}

/// A printable character as typed: upper case is shift and the letter.
fn printable(c: char) -> Option<String> {
    if c.is_ascii_uppercase() {
        return Some(chord(&["shift"], single(c.to_ascii_lowercase())?));
    }
    single(c).map(str::to_string)
}

/// An nnn.h key (`'h'`, `'\''`, `'\r'`, `CONTROL('D')`, `KEY_LEFT`) in keybind-find's notation, as curses delivers it
/// under nnn's `nonl()` and `keypad(TRUE)` (nnn.c:2424-2428): Return is `\r`, so `CONTROL('J')` (10) is Ctrl+J.
fn notation(sym: &str) -> Option<String> {
    let named = match sym {
        "KEY_LEFT" => "Left",
        "KEY_RIGHT" => "Right",
        "KEY_UP" => "Up",
        "KEY_DOWN" => "Down",
        "KEY_NPAGE" => "Next",
        "KEY_PPAGE" => "Prior",
        "KEY_HOME" => "Home",
        "KEY_END" => "End",
        "KEY_BTAB" => return Some(chord(&["shift"], "Tab")),
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.into());
    }
    if let Some(c) = sym
        .strip_prefix("CONTROL(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let key = match char_lit(c)? {
            c if c.is_ascii_alphabetic() => single(c.to_ascii_lowercase())?,
            // 0x1f: the byte terminals send for Ctrl+/ (the help writes `^/`).
            '_' => "slash",
            // ponytail: other punctuation as its own keysym (`]` is 5.3's only one); `[` would be Escape.
            c => single(c)?,
        };
        return Some(chord(&["ctrl"], key));
    }
    match char_lit(sym)? {
        '\r' => Some("Return".into()),
        '\t' => Some("Tab".into()),
        c => printable(c),
    }
}

/// A C character literal (`'h'`, `'\''`, `'\t'`) as its character.
fn char_lit(s: &str) -> Option<char> {
    let inner = s.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut cs = inner.chars();
    let c = match (cs.next()?, cs.next(), cs.next()) {
        ('\\', Some(e), None) => match e {
            'r' => '\r',
            't' => '\t',
            'n' => '\n',
            '\'' | '\\' => e,
            _ => return None,
        },
        // `'\'` is no literal: a lone backslash escapes the closing quote.
        (c, None, _) if c != '\\' => c,
        _ => return None,
    };
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-nnn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// PATH without nnn: the version is unknown, so the newest pinned file (5.3) is used.
    fn no_nnn(c: &mut Command) {
        c.env("PATH", "/nonexistent");
    }

    fn env_of(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<Vec<u8>> {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.as_bytes().to_vec())
        }
    }

    fn pinned_file() -> Pinned {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    #[test]
    fn the_pinned_file_is_nnn_h() {
        let fixture = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/nnn/bindings-5.3.h"
        ))
        .unwrap();
        assert!(
            fixture.contains("tag v5.3")
                && fixture.contains("5f1c78b")
                && fixture.contains("src/nnn.h")
        );
        assert!(fixture.contains("BSD 2-Clause"));
        let table: Vec<(String, String)> = fixture
            .lines()
            .filter_map(|l| {
                let l = l.trim().strip_prefix("{ ")?.strip_suffix(" },")?;
                let (sym, act) = l.rsplit_once(',')?;
                Some((sym.trim().to_string(), act.trim().to_string()))
            })
            .filter(|(sym, _)| sym != "KEY_MOUSE" && sym != "KEY_ENTER")
            .collect();
        let pin = pinned_file();
        let ours: Vec<(String, String)> = pin
            .bind
            .iter()
            .map(|e| (e.sym.clone(), e.action.clone()))
            .collect();
        assert_eq!(ours, table);
        assert_eq!(ours.len(), 91);
        for e in &pin.bind {
            assert!(notation(&e.sym).is_some(), "{}", e.sym);
            assert!(!e.desc.is_empty(), "{}", e.sym);
            assert!(
                ["NAVIGATION", "FILTER & PROMPT", "FILES", "MISC"].contains(&e.category.as_str()),
                "{}",
                e.category
            );
        }
        let header: String = REGISTRY[0]
            .1
            .lines()
            .take_while(|l| l.starts_with('#'))
            .collect();
        assert!(
            header.contains("v5.3")
                && header.contains("5f1c78b")
                && header.contains("BSD-2-Clause")
        );
        let desc = |a: &str| {
            pin.bind
                .iter()
                .find(|e| e.action == a)
                .unwrap()
                .desc
                .clone()
        };
        assert_eq!(desc("SEL_YOUNG"), "Next young");
        assert_eq!(desc("SEL_BMOPEN"), "Select bookmark");
        assert_eq!(desc("SEL_CDHOME"), "Go HOME");
        assert_eq!(desc("SEL_CTX3"), "Go to/create context 3");
        // Each desc is the comment above its entry in nnn.h, but where the comment only names a key (the help's or
        // nnn.1's words) and for X, which shares x's comment.
        let overrides = [
            ("SEL_CTRL_D", "Page down"),
            ("SEL_CTRL_U", "Page up"),
            ("SEL_YOUNG", "Next young"),
            ("SEL_BMOPEN", "Select bookmark"),
            ("SEL_CDHOME", "Go HOME"),
            ("SEL_RM_RF", "Delete (rm -rf)"),
        ];
        let mut comment = "";
        let mut above = Vec::new();
        for l in fixture.lines() {
            if let Some(c) = l
                .trim()
                .strip_prefix("/* ")
                .and_then(|c| c.strip_suffix(" */"))
            {
                comment = c;
            } else if l.trim().starts_with("{ ")
                && !l.contains("KEY_MOUSE")
                && !l.contains("KEY_ENTER")
            {
                above.push(comment);
            }
        }
        assert_eq!(above.len(), 91);
        for (e, comment) in pin.bind.iter().zip(above) {
            let want = match overrides.iter().find(|(a, _)| *a == e.action) {
                Some((_, d)) => d.to_string(),
                None => match e.action.strip_prefix("SEL_CTX") {
                    Some(n) => format!("Go to/create context {n}"),
                    None => comment.to_string(),
                },
            };
            assert_eq!(e.desc, want, "{}", e.sym);
        }
        assert_eq!(desc("SEL_TRASH"), "Delete from selection buffer");
        // KEY_HOME and KEY_END aren't on the help screen: NAVIGATION, beside g and G.
        let category = |sym: &str| {
            pin.bind
                .iter()
                .find(|e| e.sym == sym)
                .unwrap()
                .category
                .clone()
        };
        for (sym, want) in [
            ("KEY_HOME", "NAVIGATION"),
            ("KEY_END", "NAVIGATION"),
            ("'/'", "FILTER & PROMPT"),
            ("'x'", "FILES"),
            ("'?'", "MISC"),
        ] {
            assert_eq!(category(sym), want, "{sym}");
        }
        // The file's version is its registry key.
        let v: toml::Value = toml::from_str(REGISTRY[0].1).unwrap();
        assert_eq!(v["nnn_version"].as_str(), Some(REGISTRY[0].0));
    }

    #[test]
    fn notation_as_curses_delivers() {
        for (sym, want) in [
            ("'h'", "h"),
            ("'G'", "shift+g"),
            ("'\\''", "apostrophe"),
            ("'`'", "grave"),
            ("'~'", "asciitilde"),
            ("' '", "space"),
            ("'?'", "question"),
            ("'\\r'", "Return"),
            ("'\\t'", "Tab"),
            ("CONTROL('D')", "ctrl+d"),
            ("CONTROL('J')", "ctrl+j"),
            ("CONTROL('_')", "ctrl+slash"),
            ("CONTROL(']')", "ctrl+bracketright"),
            ("KEY_BTAB", "shift+Tab"),
            ("KEY_NPAGE", "Next"),
            ("KEY_PPAGE", "Prior"),
            ("KEY_LEFT", "Left"),
            ("KEY_HOME", "Home"),
        ] {
            assert_eq!(notation(sym).as_deref(), Some(want), "{sym}");
        }
        for sym in [
            "KEY_MOUSE",
            "KEY_F(1)",
            "'ab'",
            "''",
            "'\\'",
            "CONTROL('é')",
        ] {
            assert_eq!(notation(sym), None, "{sym}");
        }
    }

    #[test]
    fn parse_kv_as_nnn() {
        let ok = |v: &str| parse_kv(v.as_bytes()).unwrap();
        assert_eq!(
            ok("f:finder;o:fzopen"),
            [(b'f', "finder".into()), (b'o', "fzopen".into())]
        );
        assert!(ok("").is_empty());
        assert_eq!(ok("f:x;"), [(b'f', "x".into())]);
        for bad in ["f:", "f:;o:x", "fo:x", "é:x", "f", ";"] {
            assert_eq!(parse_kv(bad.as_bytes()), None, "{bad}");
        }
        assert_eq!(ok("f:a;f:b"), [(b'f', "a".into())]);
        assert_eq!(
            ok("p:!git log --format=%h:%s"),
            [(b'p', "!git log --format=%h:%s".into())]
        );
        // 101 entries: nnn keeps 100, and its 100th value runs to the end (the loop stops before cutting it).
        let keys: Vec<u8> = (b'!'..).take(101).collect();
        let v: Vec<u8> = keys.iter().flat_map(|&k| [k, b':', b'x', b';']).collect();
        let got = parse_kv(&v).unwrap();
        assert_eq!(got.len(), 100);
        let tail = String::from_utf8_lossy(&[b'x', b';', keys[100], b':', b'x', b';']).into_owned();
        assert_eq!(got[99], (keys[99], tail));
        assert_eq!(
            parse_kv(b"\x01:x;\xe9:y"),
            Some(vec![(1, "x".into()), (0xe9, "y".into())])
        );
        assert_eq!(byte_key(b'F').as_deref(), Some("shift+f"));
        assert_eq!(byte_key(b';').as_deref(), Some("semicolon"));
        assert_eq!(byte_key(1), None);
        assert_eq!(byte_key(0xe9), None);
        assert_eq!(byte_key(0x7f), None);
    }

    fn sheet_of(env: &'static [(&'static str, &'static str)]) -> (Sheet, Vec<String>) {
        sheet_with(
            REGISTRY,
            no_nnn,
            None,
            Path::new("/nonexistent"),
            &env_of(env),
        )
        .unwrap()
    }

    fn find<'a>(s: &'a Sheet, key: &str) -> Option<&'a Bind> {
        s.binds.values().flatten().find(|b| b.key == key)
    }

    fn count(s: &Sheet) -> usize {
        s.binds.values().map(Vec::len).sum()
    }

    #[test]
    fn plugin_and_bookmark_keys() {
        let (s, notes) = sheet_of(&[
            ("NNN_PLUG", "x:!echo e2e quit;p:-preview-tui"),
            ("NNN_BMS", "d:~/Docs;u:/media/usb"),
        ]);
        assert_eq!(count(&s), 95);
        let x = find(&s, "semicolon x").unwrap();
        assert_eq!(
            (x.desc.as_str(), x.action.as_str()),
            ("plugin: !echo e2e quit", "!echo e2e quit")
        );
        let p = find(&s, "semicolon p").unwrap();
        assert_eq!(
            (p.desc.as_str(), p.action.as_str()),
            ("plugin: preview-tui", "-preview-tui")
        );
        assert_eq!(find(&s, "b d").unwrap().desc, "bookmark: ~/Docs");
        assert_eq!(find(&s, "b u").unwrap().desc, "bookmark: /media/usb");
        assert!(s.binds["PLUGIN KEYS"].len() == 2 && s.binds["BOOKMARKS"].len() == 2);
        assert_eq!(find(&s, "question").unwrap().action, "SEL_HELP");
        // Version unknown (no nnn on PATH) and no nnn in front: those two notes only.
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("version is unknown"), "{notes:?}");

        let (s, notes) = sheet_of(&[("NNN_PLUG", "xy:z"), ("NNN_BMS", "d:~/Docs;u:/media/usb")]);
        assert_eq!(count(&s), 93);
        assert!(!s.binds.contains_key("PLUGIN KEYS"));
        assert_eq!(
            notes.iter().filter(|n| n.starts_with("NNN_PLUG")).count(),
            1,
            "{notes:?}"
        );
        let (s, notes) = sheet_of(&[("NNN_PLUG", "F:x;m:-"), ("NNN_BMS", "d:")]);
        assert_eq!(count(&s), 93);
        assert_eq!(find(&s, "semicolon shift+f").unwrap().desc, "plugin: x");
        // A lone `-` isn't the no-refresh flag (`tmp[1]` is checked).
        assert_eq!(find(&s, "semicolon m").unwrap().desc, "plugin: -");
        assert_eq!(
            notes.iter().filter(|n| n.starts_with("NNN_BMS")).count(),
            1,
            "{notes:?}"
        );
        // A key byte with no keysym, and a `,` bookmark (`b ,` is nnn's marked directory): left out and counted. A
        // `,` plugin is read.
        let (s, notes) = sheet_of(&[
            ("NNN_PLUG", "\x01:x;\x7f:y;a:z;,:c"),
            ("NNN_BMS", ",:/tmp;e:/etc"),
        ]);
        assert_eq!(count(&s), 94);
        assert!(find(&s, "semicolon comma").is_some() && find(&s, "b comma").is_none());
        assert!(
            notes.iter().any(|n| n.starts_with("leaving out 3 keys")),
            "{notes:?}"
        );
        let (_, notes) = sheet_of(&[]);
        assert!(
            !notes.iter().any(|n| n.starts_with("leaving out")),
            "{notes:?}"
        );
    }

    /// A stand-in `/proc` with a process at pid 4242 named `comm` and, if given, its `environ`.
    /// The sheet's notes for an nnn at pid 4242 whose `exe` prints `version` for `-V`, with no nnn on PATH.
    fn exe_notes(version: &str) -> Vec<String> {
        let proc = fake_proc(&format!("exe-{version}"), "nnn", Some(b"A=1\0"));
        // Written by `sh`, not by this process (ETXTBSY: see yazi's `fake_yazi`).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n[ \"$*\" = -V ] || exit 9; echo %s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(proc.join("4242/exe"))
            .arg(version)
            .status()
            .unwrap();
        assert!(ok.success());
        sheet_with(REGISTRY, no_nnn, Some(4242), &proc, &env_of(&[]))
            .unwrap()
            .1
    }

    #[test]
    fn version_from_the_focused_binary() {
        assert_eq!(
            exe_notes("5.4"),
            ["nnn 5.4: using the defaults of 5.3; add data/nnn/5.4.toml"]
        );
        assert!(exe_notes("5.3").is_empty());
    }

    fn fake_proc(name: &str, comm: &str, environ: Option<&[u8]>) -> PathBuf {
        let proc = tmp(name);
        let dir = proc.join("4242");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        if let Some(e) = environ {
            std::fs::write(dir.join("environ"), e).unwrap();
        }
        proc
    }

    #[test]
    fn environment_of_the_running_nnn() {
        let own = env_of(&[("NNN_PLUG", "o:own")]);
        let run = |proc: &Path| sheet_with(REGISTRY, no_nnn, Some(4242), proc, &own).unwrap();
        let environ =
            b"HOME=/h\0NNN_PLUGX=z:no\0NNN_PLUG=n:\xffnnn's\0NNN_PLUG=s:second\0NNN_BMS=d:/tmp\0";
        let proc = fake_proc("env", "nnn", Some(environ));
        let (s, notes) = run(&proc);
        assert_eq!(
            find(&s, "semicolon n").unwrap().desc,
            "plugin: \u{fffd}nnn's"
        );
        assert!(find(&s, "semicolon o").is_none() && find(&s, "semicolon z").is_none());
        assert!(find(&s, "semicolon s").is_none());
        assert_eq!(find(&s, "b d").unwrap().desc, "bookmark: /tmp");
        assert!(
            !notes.iter().any(|n| n.contains("environment")),
            "{notes:?}"
        );
        // Not nnn any more, or no environ: keybind-find's own, with the note.
        for proc in [
            fake_proc("zsh", "zsh", Some(environ)),
            fake_proc("noenv", "nnn", None),
            fake_proc("zombie", "nnn", Some(b"")),
        ] {
            let (s, notes) = run(&proc);
            assert_eq!(find(&s, "semicolon o").unwrap().desc, "plugin: own");
            assert!(find(&s, "semicolon n").is_none());
            assert_eq!(
                notes.iter().filter(|n| n.contains("environment")).count(),
                1,
                "{notes:?}"
            );
        }
        assert_eq!(environ_var(b"A=1\0AB=2\0", "AB"), Some(b"2".to_vec()));
        assert_eq!(environ_var(b"AB=2\0", "A"), None);
    }

    #[test]
    fn version_from_nnn_v() {
        let bin = tmp("bin");
        // Written by `sh`, not by this process (ETXTBSY: see yazi's `fake_yazi`).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n[ \"$*\" = -V ] || exit 9; echo 5.4\\n' > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("nnn"))
            .status()
            .unwrap();
        assert!(ok.success());
        let path = move |c: &mut Command| {
            c.env("PATH", &bin);
        };
        let (s, notes) = sheet_with(
            REGISTRY,
            path,
            None,
            Path::new("/nonexistent"),
            &env_of(&[]),
        )
        .unwrap();
        assert_eq!(count(&s), 91);
        assert_eq!(
            notes[0],
            "nnn 5.4: using the defaults of 5.3; add data/nnn/5.4.toml"
        );
    }

    #[test]
    fn real_nnn_version() {
        let mut cmd = Command::new("nnn");
        cmd.arg("-V");
        let Ok(text) = run_for(cmd, VERSION_TIMEOUT) else {
            eprintln!("skipping: nnn is not installed");
            return;
        };
        assert!(parse_version(text.trim()).is_some(), "{text:?}");
    }
}
