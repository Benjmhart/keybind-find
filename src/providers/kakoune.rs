//! Kakoune: its built-in keys by mode, pinned per version (`data/kakoune/<version>.toml`, from its source), with the
//! mappings on top. The mappings come from the running session when kak is the program in the focused terminal (its
//! `debug mappings`, asked through `kak -p`: kakrc, plugins, filetype hooks, window and buffer maps), else from the
//! files Kakoune sources at start, read as Kakoune parses them (a port of `command_manager.cc` at the pinned tag).
//! No shell command in a kakrc is run.

use super::nvim::single;
use super::pinned::{self, parse_version};
use crate::context::{run_for, run_for_input};
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::{bail, ensure, Context as _};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// One entry per Kakoune version whose built-in keys we know, oldest first.
const REGISTRY: &[(&str, &str)] = &[(
    "2026.05.21",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/kakoune/2026.05.21.toml"
    )),
)];

/// The live read (`kak -p` and the wait for its answer) and `kak -version` each end within this.
const TIMEOUT: Duration = Duration::from_secs(2);

/// Kakoune's nine modes (`commands.cc:1616`); any other mode name is a user mode the config declared.
const MODES: [&str; 9] = [
    "normal", "insert", "menu", "prompt", "goto", "view", "user", "object", "combine",
];

/// How deep commands may nest (`max_command_depth`, `command_manager.cc:525`).
const DEPTH: usize = 100;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pinned {
    kakoune_version: String,
    source: String,
    bind: Vec<Row>,
}

/// A built-in key: its mode, its key as Kakoune writes it, what it does; `map`: one of the global default mappings
/// (`register_keymaps`), running `keys`.
#[derive(Deserialize)]
struct Row {
    mode: String,
    key: String,
    desc: String,
    #[serde(default)]
    map: bool,
    #[serde(default)]
    keys: String,
}

/// One mapping: its mode, its key as written (or as `debug mappings` prints it), the keys it runs, its docstring.
#[derive(Debug, Clone, PartialEq)]
struct Mapping {
    mode: String,
    key: String,
    keys: String,
    doc: String,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let (sheet, notes) = sheet_with(
        REGISTRY,
        crate::context::editor_pid(),
        Path::new("/proc"),
        "kak",
        &std::env::temp_dir(),
        &|k| std::env::var(k).ok(),
    )?;
    for note in notes {
        eprintln!("keybind-find: kakoune: {note}");
    }
    Ok(sheet)
}

/// The sheet for the kak at `pid` (none: `dump kakoune` from a shell), its process read under `proc`, asking its
/// session with the program `kak` through a directory made under `tmp`; `env` is keybind-find's own environment (test
/// seams). Also the notes for stderr.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    pid: Option<u32>,
    proc: &Path,
    kak: &str,
    tmp: &Path,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let is_kak = |d: &Path| {
        std::fs::read_to_string(d.join("comm")).is_ok_and(|c| c.trim_end_matches('\n') == "kak")
    };
    // The editor pid context found is kak's only when kak leads the terminal; check its comm, and again after reading
    // its environ (a pid reused meanwhile, or a zombie's empty environ, isn't kak's).
    let dir = pid.map(|p| proc.join(p.to_string())).filter(|d| is_kak(d));
    let environ = dir
        .as_deref()
        .and_then(environ)
        .filter(|_| dir.as_deref().is_some_and(is_kak));
    let args = dir
        .as_ref()
        .and_then(|d| std::fs::read(d.join("cmdline")).ok())
        .map(|b| nul_split(&b))
        .unwrap_or_default();
    let mut notes = Vec::new();
    let mut answer = None;
    if let (Some(pid), Some(_)) = (pid, &dir) {
        if let Some(session) = session(args.get(1..).unwrap_or_default(), pid) {
            match live(pid, &session, kak, environ.as_ref(), tmp) {
                Ok(a) => answer = Some(a),
                Err(e) => notes.push(format!("{e:#}; reading kakrc instead")),
            }
        }
    }
    let version = match &answer {
        Some((v, _)) => Some(v.clone()),
        None => {
            let ask = |program: &Path| {
                let mut cmd = Command::new(program);
                cmd.arg("-version").env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
                run_for(cmd, TIMEOUT)
                    .ok()
                    .and_then(|t| t.trim().strip_prefix("Kakoune ").map(str::to_string))
            };
            dir.as_ref()
                .and_then(|d| ask(&d.join("exe")))
                .or_else(|| ask(Path::new(kak)))
        }
    };
    let version = version.filter(|v| parse_version(v).is_some());
    let (v, text, note) = pinned::select("kakoune", registry, version.as_deref())
        .context("no pinned Kakoune keys")?;
    let pin: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/kakoune/{v}.toml"))?;
    ensure!(
        pin.kakoune_version == v && !pin.source.is_empty(),
        "data/kakoune/{v}.toml names Kakoune {} from {:?}",
        pin.kakoune_version,
        pin.source
    );
    notes.extend(note);
    let (mappings, left_out) = match answer {
        Some((_, m)) => (m, 0),
        None => {
            let getenv = |k: &str| match &environ {
                Some(e) => e.get(k).cloned(),
                None => env(k),
            };
            fallback(&pin, dir.as_deref(), &getenv, &mut notes)
        }
    };
    let title = format!("Kakoune {}", version.as_deref().unwrap_or(v));
    let (sheet, n) = sheet_of(&pin, &title, &mappings);
    let left_out = left_out + n;
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) keybind-find can't send or read (a focus event, a non-ASCII character, a key \
             that doesn't parse, or a map whose words need Kakoune to expand them)"
        ));
    }
    Ok((sheet, notes))
}

/// A process's environment (`/proc/<pid>/environ`), the first of a name given twice (glibc's `getenv`); `None` when
/// it can't be read or is empty.
fn environ(dir: &Path) -> Option<BTreeMap<String, String>> {
    let b = std::fs::read(dir.join("environ")).ok()?;
    let mut env = BTreeMap::new();
    for kv in nul_split(&b) {
        if let Some((k, v)) = kv.split_once('=') {
            env.entry(k.into()).or_insert_with(|| v.into());
        }
    }
    (!env.is_empty()).then_some(env)
}

/// NUL-terminated strings, empty ones kept.
fn nul_split(b: &[u8]) -> Vec<String> {
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    if b.is_empty() {
        return Vec::new();
    }
    b.split(|&c| c == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

// --- the session ---------------------------------------------------------------------------------------------------

/// kak's switches (`main.cc:1023-1041`): whether each takes a value.
fn kak_switch(name: &str) -> Option<bool> {
    match name {
        "c" | "C" | "e" | "E" | "s" | "p" | "f" | "i" | "ui" | "debug" => Some(true),
        "n" | "d" | "q" | "l" | "clear" | "version" | "ro" | "help" => Some(false),
        _ => None,
    }
}

/// The session of the kak run with `args` (after its name) as pid `pid`: `-c`, `-C` or `-s`'s value, else the pid
/// (`main.cc:1153-1199`, `run_server` 699). Switches anywhere, each value the next argument, `--` ends them
/// (`parameters_parser.cc:29-90`). `None` for a command line kak refuses or one that isn't a UI (`-p`).
fn session(args: &[String], pid: u32) -> Option<String> {
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    let mut only_pos = false;
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if !only_pos && a == "--" {
            only_pos = true;
        } else if let Some(name) = a.strip_prefix('-').filter(|_| !only_pos) {
            let value = match kak_switch(name)? {
                true => {
                    i += 1;
                    args.get(i - 1)?.as_str()
                }
                false => "",
            };
            if seen.insert(name, value).is_some() {
                return None;
            }
        }
    }
    if seen.contains_key("p") {
        return None;
    }
    let named: Vec<&str> = ["c", "C", "s"]
        .iter()
        .filter_map(|s| seen.get(s).copied())
        .collect();
    match named[..] {
        [] => Some(pid.to_string()),
        [s] => Some(s.to_string()),
        _ => None,
    }
}

/// Asks the session `session` for the mappings of its client whose pid is `pid`, with `kak -p` (`main.cc:959-971`).
/// The server runs the script in an empty context (`remote.cc:820-836`); it runs a command named after the pid in
/// every client, which exists only for the one whose `%val{client_pid}` it is (a plain kak's is the server's own pid,
/// a `kak -c` client's its own: `main.cc:810-812`, `remote.cc:662-670`). That client writes a marker and its `debug
/// mappings` to `*debug*` (`commands.cc:1702-1717`), copies `*debug*` to `maps` and then its version to `done` in a
/// private directory. `kak -p` finds the socket by `XDG_RUNTIME_DIR` or `TMPDIR` (`file.cc:127-134`,
/// `remote.cc:609-633`): kak's own, when its environ could be read. The version and the mappings.
fn live(
    pid: u32,
    session: &str,
    kak: &str,
    environ: Option<&BTreeMap<String, String>>,
    tmp: &Path,
) -> anyhow::Result<(String, Vec<Mapping>)> {
    let start = Instant::now();
    // ponytail: mpv's helper, so the directory is named `keybind-find-mpv-…`; give it a prefix parameter if that
    // ever confuses anyone.
    let dir = super::mpv::private_dir(tmp)?;
    let out = ask(pid, session, kak, environ, &dir, start);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn ask(
    pid: u32,
    session: &str,
    kak: &str,
    environ: Option<&BTreeMap<String, String>>,
    dir: &Path,
    start: Instant,
) -> anyhow::Result<(String, Vec<Mapping>)> {
    let quote = |p: &Path| -> anyhow::Result<String> {
        let s = p
            .to_str()
            .context("the temporary directory's path isn't UTF-8")?;
        // The script's `%{…}` counts braces even inside quotes (`parse_quoted_balanced`, command_manager.cc:162-180).
        ensure!(
            !s.contains(['{', '}']),
            "the temporary directory's path has a brace: {s}"
        );
        Ok(format!("'{}'", s.replace('\'', "''")))
    };
    let (maps, done) = (quote(&dir.join("maps"))?, quote(&dir.join("done"))?);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let marker = format!("KEYBIND-FIND-{}-{nanos}", std::process::id());
    // `write` isn't used for the copy: it runs BufWritePre/BufWritePost hooks (commands.cc:599-629).
    let script = format!(
        "define-command -override -hidden keybind-find-report-{pid} %{{
    echo -debug {marker}
    debug mappings
    evaluate-commands -no-hooks -buffer *debug* %{{
        execute-keys '%'
        echo -to-file {maps} %val{{selection}}
    }}
    echo -to-file {done} %val{{version}}
}}
evaluate-commands -no-hooks -client %sh{{ printf %s \"$kak_client_list\" | tr ' ' , }} %{{
    try %{{ evaluate-commands \"keybind-find-report-%val{{client_pid}}\" }}
}}
"
    );
    let mut cmd = Command::new(kak);
    cmd.args(["-p", session])
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    for var in ["XDG_RUNTIME_DIR", "TMPDIR"] {
        match environ.map(|e| e.get(var)) {
            Some(Some(v)) => cmd.env(var, v),
            Some(None) => cmd.env_remove(var),
            None => &mut cmd,
        };
    }
    run_for_input(cmd, TIMEOUT, Some(script.into_bytes()))
        .with_context(|| format!("asking kak's session {session} ({kak} -p {session})"))?;
    // `done` is written after `maps`; it can be seen created and still empty.
    let version = loop {
        if let Ok(v) = std::fs::read_to_string(dir.join("done")) {
            if !v.trim().is_empty() {
                break v.trim().to_string();
            }
        }
        if start.elapsed() >= TIMEOUT {
            bail!(
                "no answer from kak's session {session} within {} s",
                TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = std::fs::read_to_string(dir.join("maps")).context("reading kak's answer")?;
    let maps = parse_debug(&text, &marker).context("kak's answer")?;
    Ok((version, maps))
}

/// The mappings `debug mappings` wrote after the last `marker` line of `*debug*`'s text: `Mappings:`, then one
/// ` * <mode> <key>: '<keys>' <docstring>` per mapping, a docstring's further lines being the lines up to the next
/// ` * `. The keys never hold a `'`: `to_string` writes it `<quote>` (keys.cc:186-224).
fn parse_debug(text: &str, marker: &str) -> anyhow::Result<Vec<Mapping>> {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .rposition(|l| *l == marker)
        .context("no marker in *debug*")?;
    let mut rest = lines[at + 1..].iter();
    ensure!(
        rest.next() == Some(&"Mappings:"),
        "no `Mappings:` after the marker"
    );
    let mut out: Vec<Mapping> = Vec::new();
    for line in rest {
        if let Some(entry) = line.strip_prefix(" * ") {
            let parsed = entry.split_once(' ').and_then(|(mode, rest)| {
                let (key, rest) = rest.split_once(": '")?;
                let (keys, doc) = rest.split_once('\'')?;
                let doc = if doc.is_empty() {
                    doc
                } else {
                    doc.strip_prefix(' ')?
                };
                Some(Mapping {
                    mode: mode.into(),
                    key: key.into(),
                    keys: keys.into(),
                    doc: doc.trim().into(),
                })
            });
            out.push(parsed.with_context(|| format!("unreadable line {line:?}"))?);
        } else if let Some(last) = out.last_mut() {
            if !line.trim().is_empty() {
                last.doc = format!("{} {}", last.doc, line.trim()).trim().into();
            }
        } else {
            bail!("unreadable line {line:?}");
        }
    }
    Ok(out)
}

// --- the config ----------------------------------------------------------------------------------------------------

/// The mappings from the files `share/kak/kakrc` (30-50) sources at start, in its order: every `*.kak` under
/// `<config>/autoload` when that is a directory, else under `<runtime>/autoload`, each in a `try`; then
/// `<runtime>/kakrc.local` and `<config>/kakrc` (the second not read when the first fails, as they share one
/// `evaluate-commands`). Over the global default maps. `dir`: kak's `/proc/<pid>` (none: no kak), whose `exe` places
/// the runtime as `runtime_directory` does (main.cc:110-131). Also the count of maps left out.
/// ponytail: a kak run with `-n` sources none of them, but they are read anyway; check its args if that matters.
fn fallback(
    pin: &Pinned,
    dir: Option<&Path>,
    getenv: Env,
    notes: &mut Vec<String>,
) -> (Vec<Mapping>, usize) {
    let set = |k: &str| getenv(k).filter(|v| !v.is_empty());
    let config = set("KAKOUNE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| set("XDG_CONFIG_HOME").map(|x| Path::new(&x).join("kak")))
        .or_else(|| set("HOME").map(|h| Path::new(&h).join(".config/kak")));
    let runtime = getenv("KAKOUNE_RUNTIME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let exe = dir
                .and_then(|d| std::fs::canonicalize(d.join("exe")).ok())
                .or_else(|| on_path(getenv));
            exe.and_then(|e| Some(e.parent()?.join("../share/kak")))
                .filter(|r| r.is_dir())
                .and_then(|r| std::fs::canonicalize(r).ok())
                .unwrap_or_else(|| PathBuf::from("/usr/share/kak"))
        });
    let mut ev = Eval {
        maps: pin
            .bind
            .iter()
            .filter(|r| r.map)
            .map(|r| Mapping {
                mode: r.mode.clone(),
                key: r.key.clone(),
                keys: r.keys.clone(),
                doc: r.desc.clone(),
            })
            .collect(),
        user_modes: Vec::new(),
        left_out: 0,
    };
    let autoload = match config.as_ref().map(|c| c.join("autoload")) {
        Some(a) if a.is_dir() => a,
        _ => runtime.join("autoload"),
    };
    let mut files = Vec::new();
    kak_files(&autoload, &mut BTreeSet::new(), &mut files);
    files.sort();
    for f in files {
        let _ = ev.file(&f, notes);
    }
    let local = runtime.join("kakrc.local");
    let ok = !local.is_file() || ev.file(&local, notes);
    match config.map(|c| c.join("kakrc")) {
        Some(rc) if ok && rc.is_file() => {
            ev.file(&rc, notes);
        }
        _ => {}
    }
    (ev.maps, ev.left_out)
}

/// `kak` on `PATH`, its links resolved (Kakoune finds its runtime from its own binary).
fn on_path(getenv: Env) -> Option<PathBuf> {
    getenv("PATH")?
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join("kak"))
        .find(|p| p.is_file())
        .and_then(|p| std::fs::canonicalize(p).ok())
}

/// Every file named `*.kak` under `dir`, links followed (`find -L … -type f -name '*\.kak'`); a directory seen
/// before (a link loop) is skipped.
fn kak_files(dir: &Path, seen: &mut BTreeSet<PathBuf>, out: &mut Vec<PathBuf>) {
    let Ok(real) = std::fs::canonicalize(dir) else {
        return;
    };
    if !seen.insert(real) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() => kak_files(&p, seen, out),
            Ok(m) if m.is_file() && p.extension().is_some_and(|x| x == "kak") => out.push(p),
            _ => {}
        }
    }
}

/// What reading kakrc files does to the global mappings.
struct Eval {
    maps: Vec<Mapping>,
    user_modes: Vec<String>,
    left_out: usize,
}

/// Why a command failed: Kakoune stops reading the file (or runs a `try`'s `catch`).
type Fail = String;

impl Eval {
    /// Reads one file; false when it stopped on an error (one note naming it).
    fn file(&mut self, path: &Path, notes: &mut Vec<String>) -> bool {
        match std::fs::read(path) {
            Ok(b) => match self.run(&String::from_utf8_lossy(&b), 0) {
                Ok(()) => true,
                Err(e) => {
                    notes.push(format!(
                        "{}: {e}; Kakoune reads no further in it",
                        path.display()
                    ));
                    false
                }
            },
            Err(e) => {
                notes.push(format!("can't read {}: {e}", path.display()));
                true
            }
        }
    }

    /// Runs `text` as `CommandManager::execute` does (command_manager.cc:549-589): each command as soon as its
    /// separator is read, so a parse error stops only what follows.
    fn run(&mut self, text: &str, depth: usize) -> Result<(), Fail> {
        let mut p = Parser { s: text, pos: 0 };
        let mut words = Vec::new();
        loop {
            match p.token()? {
                Some(Tok::Word(w)) => words.push(w),
                tok => {
                    self.command(&words, depth)?;
                    words.clear();
                    if tok.is_none() {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn command(&mut self, words: &[Word], depth: usize) -> Result<(), Fail> {
        let Some(name) = words.first() else {
            return Ok(());
        };
        if depth > DEPTH {
            return Err("maximum nested command depth hit".into());
        }
        // A word Kakoune expands at run time (`%sh{}`, `%val{}`, …): the command isn't read.
        if words.iter().any(|w| w.typed) {
            if !name.typed && name.text == "map" {
                self.left_out += 1;
            }
            return Ok(());
        }
        let params: Vec<&str> = words[1..].iter().map(|w| w.text.as_str()).collect();
        let what = |e: Fail| format!("'{}': {e}", name.text);
        match name.text.as_str() {
            "map" => self.map(&params).map_err(what),
            "unmap" => self.unmap(&params).map_err(what),
            "declare-user-mode" => self.declare_user_mode(&params).map_err(what),
            "try" => self.try_(&params, depth),
            "evaluate-commands" | "eval" => self.eval(&params, depth),
            // `fail` (commands.cc:2665-2677) always fails: a `try`'s way to its `catch`.
            "fail" => Err(params.join(" ")),
            _ => Ok(()),
        }
    }

    /// `map [-docstring S] <scope> <mode> <key> <keys>` (commands.cc:1993-2016).
    fn map(&mut self, params: &[&str]) -> Result<(), Fail> {
        let (switches, pos) = parameters(params, &[("docstring", true)], false)?;
        let [scope, mode, key, keys] = pos[..] else {
            return Err("wrong argument count".into());
        };
        global(scope)?;
        let mode = self.mode(mode)?;
        let k = one_key(key).ok_or("only a single key can be mapped")?;
        parse_keys(keys).ok_or_else(|| format!("unable to parse {keys:?}"))?;
        let m = Mapping {
            mode,
            key: key.into(),
            keys: keys.into(),
            doc: switches
                .get("docstring")
                .copied()
                .unwrap_or_default()
                .into(),
        };
        match self.find(&m.mode, k) {
            Some(i) => self.maps[i] = m,
            None => self.maps.push(m),
        }
        Ok(())
    }

    /// `unmap <scope> <mode> [<key> [<expected>]]` (commands.cc:2018-2047).
    fn unmap(&mut self, params: &[&str]) -> Result<(), Fail> {
        let (_, pos) = parameters(params, &[], false)?;
        if !(2..=4).contains(&pos.len()) {
            return Err("wrong argument count".into());
        }
        global(pos[0])?;
        let mode = self.mode(pos[1])?;
        let Some(&key) = pos.get(2) else {
            self.maps.retain(|m| m.mode != mode);
            return Ok(());
        };
        let k = one_key(key).ok_or("only a single key can be unmapped")?;
        if let Some(i) = self.find(&mode, k) {
            let same = match pos.get(3) {
                Some(&want) => {
                    let want =
                        parse_keys(want).ok_or_else(|| format!("unable to parse {want:?}"))?;
                    parse_keys(&self.maps[i].keys) == Some(want)
                }
                None => true,
            };
            if same {
                self.maps.remove(i);
            }
        }
        Ok(())
    }

    /// `declare-user-mode <name>` (commands.cc:2679-2691, keymap_manager.cc:64-77).
    fn declare_user_mode(&mut self, params: &[&str]) -> Result<(), Fail> {
        let (_, pos) = parameters(params, &[], false)?;
        let [name] = pos[..] else {
            return Err("wrong argument count".into());
        };
        if MODES.contains(&name) {
            return Err(format!("'{name}' is already a regular mode"));
        }
        if self.user_modes.iter().any(|m| m == name) {
            return Err(format!("user mode '{name}' already defined"));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("invalid mode name: '{name}'"));
        }
        self.user_modes.push(name.into());
        Ok(())
    }

    /// `try <cmds> [catch <cmds>]…` (commands.cc:2443-2489): the first body that doesn't fail; the last `catch`'s
    /// failure is the command's, and a `try` with no `catch` swallows its body's.
    fn try_(&mut self, params: &[&str], depth: usize) -> Result<(), Fail> {
        let (_, pos) = parameters(params, &[], false)?;
        if pos.is_empty() || pos.len() % 2 != 1 {
            return Err("'try': wrong argument count".into());
        }
        if pos.iter().skip(1).step_by(2).any(|c| *c != "catch") {
            return Err("'try': usage: try <commands> [catch <on error commands>]...".into());
        }
        for (i, body) in pos.iter().enumerate().step_by(2) {
            if i == 0 || i < pos.len() - 1 {
                if self.run(body, depth + 1).is_ok() {
                    return Ok(());
                }
            } else {
                return self.run(body, depth + 1);
            }
        }
        Ok(())
    }

    /// `evaluate-commands` (alias `eval`, commands.cc:2253-2276): its arguments joined by spaces, run in place. Not
    /// read with a switch other than `-no-hooks`: they run it elsewhere (a client, a buffer, a draft) or verbatim.
    fn eval(&mut self, params: &[&str], depth: usize) -> Result<(), Fail> {
        let switches = [
            ("client", true),
            ("try-client", true),
            ("buffer", true),
            ("draft", false),
            ("itersel", false),
            ("save-regs", true),
            ("no-hooks", false),
            ("verbatim", false),
        ];
        let (sw, pos) =
            parameters(params, &switches, true).map_err(|e| format!("'evaluate-commands': {e}"))?;
        if pos.is_empty() {
            return Err("'evaluate-commands': wrong argument count".into());
        }
        if sw.keys().any(|s| *s != "no-hooks") {
            return Ok(());
        }
        self.run(&pos.join(" "), depth + 1)
    }

    /// `parse_keymap_mode` (commands.cc:1596-1614): a prefix of a regular mode, in Kakoune's order, else a declared
    /// user mode.
    fn mode(&self, s: &str) -> Result<String, Fail> {
        MODES
            .iter()
            .find(|m| m.starts_with(s))
            .map(|m| m.to_string())
            .or_else(|| self.user_modes.iter().find(|m| *m == s).cloned())
            .ok_or_else(|| format!("no such keymap mode: '{s}'"))
    }

    fn find(&self, mode: &str, key: Key) -> Option<usize> {
        self.maps
            .iter()
            .position(|m| m.mode == mode && one_key(&m.key) == Some(key))
    }
}

/// The scope of a `map` at startup: `get_scope_ifp` (commands.cc:427-440) takes a prefix of `global`; `buffer`,
/// `window` and `local` need a context the kakrc doesn't run in, so they fail there.
/// ponytail: `buffer=NAME` fails too, though Kakoune finds `*debug*` (the one buffer at startup) and maps there.
fn global(scope: &str) -> Result<(), Fail> {
    if "global".starts_with(scope) {
        Ok(())
    } else {
        Err(format!("scope '{scope}' doesn't exist at startup"))
    }
}

/// A command's switches and positionals, as `ParametersParser` reads them (parameters_parser.cc:29-90): `-name`
/// anywhere (only before the first positional with `at_start`), its value the next argument, `--` ends them; an
/// unknown or repeated switch, or a missing value, fails.
fn parameters<'a>(
    params: &[&'a str],
    switches: &[(&'static str, bool)],
    at_start: bool,
) -> Result<(BTreeMap<&'static str, &'a str>, Vec<&'a str>), Fail> {
    let (mut sw, mut pos) = (BTreeMap::new(), Vec::new());
    let mut only_pos = false;
    let mut it = params.iter();
    while let Some(&p) = it.next() {
        if !only_pos && p == "--" {
            only_pos = true;
        } else if let Some(name) = p.strip_prefix('-').filter(|_| !only_pos) {
            let &(name, takes) = switches
                .iter()
                .find(|(s, _)| *s == name)
                .ok_or_else(|| format!("unknown option '{p}'"))?;
            let value = if takes {
                *it.next()
                    .ok_or_else(|| format!("missing value for switch '-{name}'"))?
            } else {
                ""
            };
            if sw.insert(name, value).is_some() {
                return Err(format!("switch '-{name}' specified more than once"));
            }
        } else {
            pos.push(p);
            only_pos |= at_start;
        }
    }
    Ok((sw, pos))
}

// --- words ---------------------------------------------------------------------------------------------------------

/// A word of a command; `typed`: a typed expansion (`%sh{…}`, `%val{…}`, …, also inside `"…"`) Kakoune expands when
/// it runs the command.
struct Word {
    text: String,
    typed: bool,
}

enum Tok {
    Word(Word),
    Separator,
}

/// The expansions `token_type` knows (command_manager.cc:214-238) besides the raw `%{…}`.
const TYPED: [&str; 7] = ["sh", "reg", "opt", "val", "arg", "file", "exp"];

/// Kakoune's command parser (command_manager.cc:110-260, 449-485) over one file or body.
struct Parser<'a> {
    s: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s[self.pos..].chars().next()
    }

    /// `read_token`: the next word or separator, `None` at the end; an unterminated string is an error.
    fn token(&mut self) -> Result<Option<Tok>, Fail> {
        self.skip_blanks_and_comments();
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        match c {
            '"' | '\'' => {
                self.pos += 1;
                let (text, done) = self.quoted(c);
                if !done {
                    return Err(format!("parse error: unterminated string {c}...{c}"));
                }
                Ok(Some(Tok::Word(if c == '"' {
                    expand(&text)
                } else {
                    Word { text, typed: false }
                })))
            }
            '%' => {
                self.pos += 1;
                self.percent().map(|w| Some(Tok::Word(w)))
            }
            ';' | '\n' => {
                self.pos += 1;
                Ok(Some(Tok::Separator))
            }
            _ => {
                if c == '\\'
                    && matches!(
                        self.s[self.pos + 1..].chars().next(),
                        Some('%' | '\'' | '"')
                    )
                {
                    self.pos += 1;
                }
                Ok(Some(Tok::Word(Word {
                    text: self.unquoted(),
                    typed: false,
                })))
            }
        }
    }

    fn skip_blanks_and_comments(&mut self) {
        let b = self.s.as_bytes();
        while let Some(&c) = b.get(self.pos) {
            match c {
                b' ' | b'\t' | 0x0c => self.pos += 1,
                b'\\' if b.get(self.pos + 1) == Some(&b'\n') => self.pos += 2,
                b'#' => {
                    while b.get(self.pos).is_some_and(|&c| c != b'\n') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// `parse_quoted`: up to `delim`, a doubled one being one literal; whether it was found.
    fn quoted(&mut self, delim: char) -> (String, bool) {
        let mut out = String::new();
        let mut beg = self.pos;
        while let Some(c) = self.peek() {
            let cur = self.pos;
            self.pos += c.len_utf8();
            if c == delim {
                if self.peek() != Some(delim) {
                    out.push_str(&self.s[beg..cur]);
                    return (out, true);
                }
                out.push_str(&self.s[beg..self.pos]);
                self.pos += delim.len_utf8();
                beg = self.pos;
            }
        }
        out.push_str(&self.s[beg..]);
        (out, false)
    }

    /// `parse_quoted_balanced`: up to the `close` that ends the nesting of `open`, with no escape.
    fn balanced(&mut self, open: u8, close: u8) -> (String, bool) {
        let b = self.s.as_bytes();
        let beg = self.pos;
        let mut level = 1;
        while let Some(&c) = b.get(self.pos) {
            self.pos += 1;
            if c == open {
                level += 1;
            } else if c == close {
                level -= 1;
                if level == 0 {
                    return (self.s[beg..self.pos - 1].into(), true);
                }
            }
        }
        (self.s[beg..].into(), false)
    }

    /// `parse_percent_token`, after the `%`: a type name of `a`-`z`, then a delimiter that isn't a letter.
    fn percent(&mut self) -> Result<Word, Fail> {
        let start = self.pos;
        let b = self.s.as_bytes();
        while b.get(self.pos).is_some_and(u8::is_ascii_lowercase) {
            self.pos += 1;
        }
        let kind = &self.s[start..self.pos];
        let delim = match self.peek() {
            Some(d) if !d.is_alphabetic() => d,
            _ => {
                return Err(format!(
                    "parse error: expected a string delimiter after '%{kind}'"
                ))
            }
        };
        self.pos += delim.len_utf8();
        let typed = TYPED.contains(&kind);
        if !typed && !kind.is_empty() {
            return Err(format!("parse error: unknown expand '{kind}'"));
        }
        let (text, done) = match delim {
            '(' => self.balanced(b'(', b')'),
            '[' => self.balanced(b'[', b']'),
            '{' => self.balanced(b'{', b'}'),
            '<' => self.balanced(b'<', b'>'),
            d => self.quoted(d),
        };
        if !done {
            return Err(format!(
                "parse error: unterminated string '%{kind}{delim}...'"
            ));
        }
        Ok(Word { text, typed })
    }

    /// `parse_unquoted`: up to a blank or a separator, which a `\` before it keeps in the word.
    fn unquoted(&mut self) -> String {
        let b = self.s.as_bytes();
        let mut out = String::new();
        let mut beg = self.pos;
        while let Some(&c) = b.get(self.pos) {
            if matches!(c, b';' | b'\n' | b' ' | b'\t' | 0x0c) {
                out.push_str(&self.s[beg..self.pos]);
                if self.pos != beg && b[self.pos - 1] == b'\\' {
                    out.pop();
                    out.push(c as char);
                    beg = self.pos + 1;
                } else {
                    return out;
                }
            }
            self.pos += 1;
        }
        out.push_str(&self.s[beg..]);
        out
    }
}

/// A `"…"` word's text after `expand` (command_manager.cc:487-511): `%%` is one `%`; a typed expansion in it makes
/// the word typed.
/// ponytail: `%{…}` (raw) inside `"…"` is kept as written, not replaced by its content; no config of a map uses it.
fn expand(text: &str) -> Word {
    let mut out = String::new();
    let mut typed = false;
    let mut rest = text;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(r) = after.strip_prefix('%') {
            out.push('%');
            rest = r;
            continue;
        }
        let kind: String = after.chars().take_while(char::is_ascii_lowercase).collect();
        typed |= TYPED.contains(&kind.as_str())
            && after[kind.len()..]
                .chars()
                .next()
                .is_some_and(|d| !d.is_alphabetic());
        out.push('%');
        rest = after;
    }
    out.push_str(rest);
    Word { text: out, typed }
}

// --- keys ----------------------------------------------------------------------------------------------------------

/// A key as `parse_keys` reads it (keys.cc:91-165): a character, or a named key as its X keysym (`focus_in` and
/// `focus_out` as themselves, which no keysym sends).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Base {
    Char(char),
    Sym(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    ctrl: bool,
    alt: bool,
    shift: bool,
    base: Base,
}

/// `keynamemap` (keys.cc:63-89).
const NAMES: [(&str, Base); 25] = [
    ("ret", Base::Sym("Return")),
    ("space", Base::Sym("space")),
    ("tab", Base::Sym("Tab")),
    ("lt", Base::Char('<')),
    ("gt", Base::Char('>')),
    ("backspace", Base::Sym("BackSpace")),
    ("esc", Base::Sym("Escape")),
    ("up", Base::Sym("Up")),
    ("down", Base::Sym("Down")),
    ("left", Base::Sym("Left")),
    ("right", Base::Sym("Right")),
    ("pageup", Base::Sym("Prior")),
    ("pagedown", Base::Sym("Next")),
    ("home", Base::Sym("Home")),
    ("end", Base::Sym("End")),
    ("ins", Base::Sym("Insert")),
    ("del", Base::Sym("Delete")),
    ("plus", Base::Char('+')),
    ("minus", Base::Char('-')),
    ("semicolon", Base::Char(';')),
    ("percent", Base::Char('%')),
    ("quote", Base::Char('\'')),
    ("dquote", Base::Char('"')),
    ("focus_in", Base::Sym("focus_in")),
    ("focus_out", Base::Sym("focus_out")),
];

const F_KEYS: [&str; 12] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
];

/// `parse_keys`: `None` where Kakoune throws a key_parse_error.
fn parse_keys(s: &str) -> Option<Vec<Key>> {
    let plain = |base| Key {
        ctrl: false,
        alt: false,
        shift: false,
        base,
    };
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let close = rest.find('>').filter(|_| c == '<');
        let Some(close) = close else {
            out.push(plain(match c {
                '\n' | '\r' => Base::Sym("Return"),
                '\u{8}' => Base::Sym("BackSpace"),
                '\t' => Base::Sym("Tab"),
                ' ' => Base::Sym("space"),
                '\u{1b}' => Base::Sym("Escape"),
                c => Base::Char(c),
            }));
            rest = &rest[c.len_utf8()..];
            continue;
        };
        let mut desc = &rest[1..close];
        let mut key = plain(Base::Char(' '));
        while let Some(dash) = desc.find('-') {
            if dash != 1 {
                return None;
            }
            match desc.as_bytes()[0].to_ascii_lowercase() {
                b'c' => key.ctrl = true,
                b'a' => key.alt = true,
                b's' => key.shift = true,
                _ => return None,
            }
            desc = &desc[2..];
        }
        let mut chars = desc.chars();
        key = if let Some(&(_, base)) = NAMES.iter().find(|(n, _)| *n == desc) {
            canonical(Key { base, ..key })?
        } else if let (Some(c), None) = (chars.next(), chars.next()) {
            canonical(Key {
                base: Base::Char(c),
                ..key
            })?
        } else if desc.starts_with('F') && desc.len() <= 3 {
            let n: usize = desc[1..].parse().ok()?;
            Key {
                base: Base::Sym(F_KEYS.get(n.checked_sub(1)?)?),
                ..key
            }
        } else {
            return None;
        };
        out.push(key);
        rest = &rest[close + 1..];
    }
    Some(out)
}

/// `canonicalize_ifn` (keys.cc:19-44): a control character is ctrl + its letter; shift on an ASCII letter is the
/// upper-case letter, on another character an error.
fn canonical(mut k: Key) -> Option<Key> {
    if let Base::Char(c) = k.base {
        if ('\u{1}'..'\u{1b}').contains(&c) {
            k.ctrl = true;
            k.base = Base::Char((c as u8 - 1 + b'a') as char);
        }
    }
    if k.shift {
        match k.base {
            Base::Char(c) if c.is_ascii_alphabetic() => {
                k.shift = false;
                k.base = Base::Char(c.to_ascii_uppercase());
            }
            Base::Char(_) => return None,
            Base::Sym(_) => {}
        }
    }
    Some(k)
}

/// The one key of `s`, as `map` and `unmap` take it.
fn one_key(s: &str) -> Option<Key> {
    match parse_keys(s)?[..] {
        [k] => Some(k),
        _ => None,
    }
}

/// A key in keybind-find's notation: `c-` ctrl, `a-` alt, `s-` shift, an upper-case letter shift + its letter.
/// `None`: a focus event, or a character with no keysym.
fn chord_of(k: Key) -> Option<String> {
    let mut mods: Vec<&str> = [(k.ctrl, "ctrl"), (k.alt, "alt"), (k.shift, "shift")]
        .iter()
        .filter(|(on, _)| *on)
        .map(|&(_, m)| m)
        .collect();
    let sym = match k.base {
        Base::Sym(s) if s.starts_with("focus_") => return None,
        Base::Sym(s) => s,
        Base::Char(c) => {
            if c.is_ascii_uppercase() {
                mods.push("shift");
            }
            single(c.to_ascii_lowercase())?
        }
    };
    Some(chord(&mods, sym))
}

/// Kakoune keys (`<c-x>f`) in keybind-find's notation (`ctrl+x f`).
fn notation(s: &str) -> Option<String> {
    let chords = parse_keys(s)?
        .into_iter()
        .map(chord_of)
        .collect::<Option<Vec<_>>>()?;
    (!chords.is_empty()).then(|| chords.join(" "))
}

// --- the sheet -----------------------------------------------------------------------------------------------------

/// The keys that reach a mode from normal mode, and the mode its keys are typed in (AC: a sub-mode's keys are a
/// sequence typed in normal mode).
fn reached(mode: &str) -> (&'static [&'static str], &str) {
    match mode {
        "goto" => (&["g"], "normal"),
        "view" => (&["v"], "normal"),
        "object" => (&["alt+i", "alt+a"], "normal"),
        "user" => (&["space"], "normal"),
        m => (&[""], m),
    }
}

/// The sheet: the pinned built-in keys (not the default maps: `mappings` holds those still mapped), each replaced by a
/// mapping of its mode and key, then the other mappings. Also how many keys were left out.
fn sheet_of(pin: &Pinned, title: &str, mappings: &[Mapping]) -> (Sheet, usize) {
    // (mode, chord) → (desc, action), in order.
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    let mut left_out = 0;
    let builtins = pin
        .bind
        .iter()
        .filter(|r| !r.map)
        .map(|r| (&r.mode, &r.key, r.desc.clone(), String::new()));
    let mapped = mappings.iter().map(|m| {
        let doc = m.doc.lines().map(str::trim).filter(|l| !l.is_empty());
        let doc = doc.collect::<Vec<_>>().join(" ");
        let desc = if doc.is_empty() { m.keys.clone() } else { doc };
        (&m.mode, &m.key, desc, m.keys.clone())
    });
    for (mode, key, desc, action) in builtins.chain(mapped) {
        let Some(chord) = notation(key) else {
            left_out += 1;
            continue;
        };
        let row = (mode.clone(), chord, desc, action);
        match rows.iter().position(|r| r.0 == row.0 && r.1 == row.1) {
            Some(i) => rows[i] = row,
            None => rows.push(row),
        }
    }
    let mut sheet = Sheet {
        title: title.into(),
        provider: "kakoune".into(),
        binds: Default::default(),
    };
    for (mode, chord, desc, action) in rows {
        let (leads, typed_in) = reached(&mode);
        let mut when = BTreeMap::new();
        if let (Ok(scope), Ok(v)) = ("mode".parse::<Name>(), typed_in.parse::<Name>()) {
            when.insert(scope, Values::from(v));
        }
        for lead in leads {
            let key = if lead.is_empty() {
                chord.clone()
            } else {
                format!("{lead} {chord}")
            };
            sheet.binds.entry(mode.clone()).or_default().push(Bind {
                key,
                desc: desc.clone(),
                action: action.clone(),
                when: when.clone(),
                run: Vec::new(),
            });
        }
    }
    (sheet, left_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin() -> Pinned {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/kakoune")
                .join(name),
        )
        .unwrap()
    }

    /// A fresh directory for one test.
    fn fresh(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-kak-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn mapping(mode: &str, key: &str, keys: &str, doc: &str) -> Mapping {
        Mapping {
            mode: mode.into(),
            key: key.into(),
            keys: keys.into(),
            doc: doc.into(),
        }
    }

    /// The binds of `s` with this key, as (category, mode tag, desc, action).
    fn find<'a>(s: &'a Sheet, key: &str) -> Vec<(&'a str, String, &'a str, &'a str)> {
        s.binds
            .iter()
            .flat_map(|(c, bs)| bs.iter().map(move |b| (c, b)))
            .filter(|(_, b)| b.key == key)
            .map(|(c, b)| {
                let mode = b
                    .when
                    .get("mode")
                    .map(|v| v.iter().map(Name::as_str).collect::<Vec<_>>().join(","));
                (
                    c.as_str(),
                    mode.unwrap_or_default(),
                    b.desc.as_str(),
                    b.action.as_str(),
                )
            })
            .collect()
    }

    /// The desc of the normal-mode key `key`.
    fn normal<'a>(s: &'a Sheet, key: &str) -> Option<&'a str> {
        find(s, key)
            .into_iter()
            .find(|b| b.0 == "normal")
            .map(|b| b.2)
    }

    fn count(s: &Sheet) -> usize {
        s.binds.values().map(Vec::len).sum()
    }

    /// AC #1: Kakoune's built-in keys, counted against the source.
    #[test]
    fn the_pinned_keys() {
        let p = pin();
        assert_eq!(p.kakoune_version, "2026.05.21");
        let n = |mode: &str| p.bind.iter().filter(|r| r.mode == mode && !r.map).count();
        // `keymap` has 150 entries at v2026.05.21 (normal.cc:2451-2639; ctrl-l "force redraw" is the last).
        assert_eq!(
            (
                n("normal"),
                n("goto"),
                n("view"),
                n("object"),
                n("insert"),
                n("prompt")
            ),
            (150, 13, 11, 28, 22, 42)
        );
        let maps: Vec<(&str, &str)> = p
            .bind
            .iter()
            .filter(|r| r.map)
            .map(|r| (r.key.as_str(), r.keys.as_str()))
            .collect();
        assert_eq!(maps.len(), 12);
        assert!(maps.contains(&("<left>", "h")) && maps.contains(&("<s-home>", "<a-H>")));
        assert!(p
            .bind
            .iter()
            .filter(|r| r.map)
            .all(|r| r.mode == "normal" && r.desc.is_empty()));
        let desc = |mode: &str, key: &str| {
            let r = p
                .bind
                .iter()
                .find(|r| r.mode == mode && r.key == key && !r.map);
            r.map(|r| r.desc.as_str()).unwrap_or("(none)")
        };
        assert_eq!(desc("normal", "x"), "extend selections to whole lines");
        assert_eq!(desc("normal", "<a-i>"), "select inner object");
        assert_eq!(desc("normal", "<space>"), "user mappings");
        assert_eq!(desc("normal", "<c-l>"), "force redraw");
        assert_eq!(desc("goto", "g"), "buffer top");
        assert_eq!(desc("goto", "k"), "buffer top");
        assert_eq!(
            desc("object", "<a-semicolon>"),
            "run command in object context"
        );
        // Two keys sharing the docstring "select nested objects": worded apart as keys.asciidoc does.
        assert_eq!(
            desc("normal", "<a-I>"),
            "select each inner nested object in selections"
        );
        assert_eq!(
            desc("normal", "<a-A>"),
            "select each whole nested object in selections"
        );
        assert_eq!(
            desc("insert", "<c-x>W"),
            "explicit word completion (all buffers)"
        );
        assert_eq!(desc("prompt", "<a-backspace>"), desc("prompt", "<c-w>"));
        // Every key parses and translates: nothing left out, and every bind lands in the sheet.
        let (s, left_out) = sheet_of(&p, "Kakoune 2026.05.21", &[]);
        assert_eq!(left_out, 0);
        // object's keys twice (after alt+i and alt+a); no map rows without mappings.
        assert_eq!(count(&s), 150 + 13 + 11 + 28 * 2 + 22 + 42);
        assert_eq!(s.title, "Kakoune 2026.05.21");
        assert_eq!(s.provider, "kakoune");
    }

    /// AC #2.
    #[test]
    fn kakoune_keys_in_notation() {
        for (kak, want) in [
            ("<c-a-w>", "ctrl+alt+w"),
            ("X", "shift+x"),
            ("<s-x>", "shift+x"),
            ("<s-tab>", "shift+Tab"),
            ("<lt>", "less"),
            ("<gt>", "greater"),
            ("<quote>", "apostrophe"),
            ("<dquote>", "quotedbl"),
            ("<a-;>", "alt+semicolon"),
            ("<a-semicolon>", "alt+semicolon"),
            ("<pageup>", "Prior"),
            ("<pagedown>", "Next"),
            ("<F12>", "F12"),
            ("<F1>", "F1"),
            ("<space>", "space"),
            ("<ret>", "Return"),
            ("<backspace>", "BackSpace"),
            ("<esc>", "Escape"),
            ("<ins>", "Insert"),
            ("<del>", "Delete"),
            ("<plus>", "plus"),
            ("<minus>", "minus"),
            ("<percent>", "percent"),
            ("<a-A>", "alt+shift+a"),
            ("<c-W>", "ctrl+shift+w"),
            ("<C-x>", "ctrl+x"),
            (":", "colon"),
            ("<c-x>f", "ctrl+x f"),
            ("<", "less"),
        ] {
            assert_eq!(notation(kak).as_deref(), Some(want), "{kak}");
        }
        // Left out: no keysym, a focus event, keys that don't parse (keys.cc:91-165).
        for kak in [
            "é",
            "<focus_in>",
            "<focus_out>",
            "<bogus>",
            "<F13>",
            "<F0>",
            "<s-;>",
            "<x-a>",
            "<ab-c>",
            "<>",
            "",
        ] {
            assert_eq!(notation(kak), None, "{kak}");
        }
        assert_eq!(parse_keys("<a-;>"), parse_keys("<a-semicolon>"));
        assert_eq!(parse_keys("<s-x>"), parse_keys("X"));
        assert_eq!(parse_keys(" "), parse_keys("<space>"));
        assert_eq!(one_key("<c-x>f"), None);
    }

    /// AC #4: `debug mappings` as kak 2026.05.21 wrote it in the e2e image.
    #[test]
    fn debug_mappings_as_kak_wrote_them() {
        let text = fixture("debug.txt");
        let m = parse_debug(&text, "KEYBIND-FIND-NEW").unwrap();
        let get = |mode: &str, key: &str| m.iter().find(|x| x.mode == mode && x.key == key);
        // The twelve default maps but `<home>` (unmapped by the e2e kakrc), the config's, the window's.
        assert_eq!(m.len(), 11 + 1 + 1 + 1 + 3 + 4 + 1, "{m:#?}");
        assert_eq!(
            get("normal", "<left>"),
            Some(&mapping("normal", "<left>", "h", ""))
        );
        assert_eq!(get("normal", "<home>"), None);
        assert_eq!(get("goto", "q").unwrap().keys, "<esc>gj");
        assert_eq!(get("insert", "<c-y>").unwrap().doc, "e2e insert");
        assert_eq!(
            get("user", ":"),
            Some(&mapping(
                "user",
                ":",
                ":write<ret>",
                "e2e write on two lines"
            ))
        );
        assert_eq!(get("user", "b").unwrap().doc, "e2e window");
        assert_eq!(get("e2e", "q").unwrap().keys, ":quit<ret>");
        assert_eq!(get("man", "m").unwrap().doc, "Look up a man page");
        // `user n` was mapped under the old marker only.
        assert_eq!(get("user", "n"), None);
        assert!(parse_debug(&text, "KEYBIND-FIND-OLD")
            .unwrap()
            .iter()
            .any(|x| x.key == "n"));
        // The sheet: every mode's keys tagged, the custom mode alone.
        let (s, left_out) = sheet_of(&pin(), "Kakoune 2026.05.21", &m);
        assert_eq!(left_out, 1, "é");
        assert_eq!(
            find(&s, "space b"),
            [(
                "user",
                "normal".into(),
                "e2e window",
                ":echo<space>window<ret>"
            )]
        );
        assert_eq!(find(&s, "space colon")[0].2, "e2e write on two lines");
        assert_eq!(find(&s, "q").iter().filter(|b| b.0 == "e2e").count(), 1);
        assert_eq!(
            normal(&s, "Left"),
            Some("h"),
            "an empty docstring: its keys"
        );
        assert_eq!(normal(&s, "Home"), None);
        for bad in [
            "no marker",
            "KEYBIND-FIND-NEW\nnot mappings\n",
            "KEYBIND-FIND-NEW\nMappings:\n * user\n",
        ] {
            assert!(parse_debug(bad, "KEYBIND-FIND-NEW").is_err(), "{bad}");
        }
    }

    /// AC #3.
    #[test]
    fn where_each_key_goes() {
        let maps = [
            mapping("normal", "x", ":echo x<ret>", "my x"),
            mapping("user", "k", ":quit<ret>", ""),
            mapping("goto", "q", "gj", "two\n  lines"),
            mapping("object", "q", "c", "obj"),
            mapping("insert", "<c-y>", "<esc>", "ins"),
            mapping("prompt", "<c-z>", "<esc>", "pr"),
            mapping("menu", "<c-j>", "<down>", "menu"),
            mapping("combine", "<c-u>", "u", "comb"),
            mapping("view", "z", "vt", "view"),
            mapping("e2e", "q", ":quit<ret>", "custom"),
            mapping("normal", "é", "x", "left out"),
        ];
        let (s, left_out) = sheet_of(&pin(), "Kakoune 2026.05.21", &maps);
        assert_eq!(left_out, 1);
        // A mapping replaces the built-in key of its mode.
        assert_eq!(
            find(&s, "x"),
            [("normal", "normal".into(), "my x", ":echo x<ret>")]
        );
        assert_eq!(
            find(&s, "space k"),
            [("user", "normal".into(), ":quit<ret>", ":quit<ret>")]
        );
        assert_eq!(
            find(&s, "g q"),
            [("goto", "normal".into(), "two lines", "gj")]
        );
        assert_eq!(
            find(&s, "g g"),
            [("goto", "normal".into(), "buffer top", "")]
        );
        assert_eq!(find(&s, "alt+i q").len(), 1);
        assert_eq!(find(&s, "alt+a q")[0].2, "obj");
        assert_eq!(find(&s, "alt+i w")[0].2, "word");
        assert_eq!(find(&s, "v z")[0].1, "normal");
        assert_eq!(find(&s, "ctrl+y")[0].1, "insert");
        assert_eq!(find(&s, "ctrl+z")[0].1, "prompt");
        assert_eq!(find(&s, "ctrl+j")[0].0, "menu");
        // One key in four modes: each its own bind, tagged.
        let modes: Vec<String> = find(&s, "ctrl+u").into_iter().map(|b| b.1).collect();
        assert_eq!(modes, ["combine", "insert", "normal", "prompt"]);
        assert_eq!(
            find(&s, "q")
                .into_iter()
                .filter(|b| b.0 == "e2e")
                .collect::<Vec<_>>(),
            [("e2e", "e2e".into(), "custom", ":quit<ret>")]
        );
        assert_eq!(s.title, "Kakoune 2026.05.21");
    }

    /// The words of `text` (one command per line), as Kakoune reads them.
    fn words(text: &str) -> Vec<Vec<String>> {
        let mut p = Parser { s: text, pos: 0 };
        let (mut out, mut cur) = (Vec::new(), Vec::new());
        loop {
            match p.token().unwrap() {
                Some(Tok::Word(w)) => cur.push(if w.typed {
                    format!("${}", w.text)
                } else {
                    w.text
                }),
                tok => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    if tok.is_none() {
                        return out;
                    }
                }
            }
        }
    }

    /// AC #6.
    #[test]
    fn words_as_kakoune_parses_them() {
        let w = |t: &str| words(t);
        assert_eq!(w("a 'a''b' \"x\"\"y\""), [["a", "a'b", "x\"y"]]);
        assert_eq!(
            w("a %|a||b| %{a{b}c} %(x) %[y] %<z>"),
            [["a", "a|b", "a{b}c", "x", "y", "z"]]
        );
        assert_eq!(w(r"a b\;c d\ e"), [["a", "b;c", "d e"]]);
        assert_eq!(w("a #c\nb x#y"), [vec!["a"], vec!["b", "x#y"]]);
        assert_eq!(w("a; b\nc"), [vec!["a"], vec!["b"], vec!["c"]]);
        assert_eq!(w("a \\\n b"), [["a", "b"]]);
        assert_eq!(w("a \\%x \\'y \\\"z"), [["a", "%x", "'y", "\"z"]]);
        assert_eq!(
            w("a %sh{echo}; b %val{x} \"%opt{y} 100%%\""),
            [vec!["a", "$echo"], vec!["b", "$x", "$%opt{y} 100%"]]
        );
        assert_eq!(w("a \"50%% %{x}\""), [["a", "50% %{x}"]]);
        assert_eq!(w("a %¹b¹¹c¹ d"), [["a", "b¹c", "d"]]);
        assert_eq!(w("a\tb\x0cc"), [["a", "b", "c"]]);
        for bad in [
            "a %{b",
            "a 'b",
            "a \"b",
            "a %sh(",
            "a %foo{x}",
            "a %",
            "a %ab",
        ] {
            let mut p = Parser { s: bad, pos: 0 };
            let r = (0..5).try_for_each(|_| p.token().map(|_| ()));
            assert!(r.is_err(), "{bad}");
        }
    }

    /// The global mappings after `text`, with no default maps; also the left-out count and the failure that stopped
    /// it.
    fn eval(text: &str) -> (Vec<Mapping>, usize, Option<Fail>) {
        let mut ev = Eval {
            maps: Vec::new(),
            user_modes: Vec::new(),
            left_out: 0,
        };
        let r = ev.run(text, 0).err();
        (ev.maps, ev.left_out, r)
    }

    fn keys_of(maps: &[Mapping]) -> Vec<String> {
        maps.iter()
            .map(|m| format!("{} {} {}", m.mode, m.key, m.keys))
            .collect()
    }

    /// AC #5's commands.
    #[test]
    fn map_unmap_and_the_rest() {
        let (m, _, err) = eval(
            "map -docstring 'd' global normal x y\nmap global normal z w -docstring e\nmap g n a b\n\
             map global normal x y2",
        );
        assert_eq!(err, None);
        assert_eq!(keys_of(&m), ["normal x y2", "normal z w", "normal a b"]);
        assert_eq!(m[1].doc, "e");
        // A prefix of a mode, in Kakoune's order: `i` is insert, `u` user, `o` object, `c` combine, `m` menu.
        let (m, _, _) = eval("map global i a b\nmap global u a b\nmap global o a b\nmap global c a b\nmap global m a b");
        assert_eq!(
            m.iter().map(|m| m.mode.as_str()).collect::<Vec<_>>(),
            ["insert", "user", "object", "combine", "menu"]
        );
        // unmap: one key, a whole mode, or only with the expected keys.
        let (m, _, err) = eval(
            "map global normal <left> h\nmap global normal <right> l\nmap global user a b\nmap global user c d\n\
             unmap global normal <left>\nunmap global user\nunmap global normal <right> wrong-keys\n\
             unmap global normal <up>",
        );
        assert_eq!(err, None);
        assert_eq!(keys_of(&m), ["normal <right> l"]);
        let (m, _, _) = eval("map global normal <a-;> x\nunmap global normal <a-semicolon> x");
        assert!(m.is_empty());
        // A declared user mode; then a map to it.
        let (m, _, err) = eval("declare-user-mode e2e\nmap global e2e q ':quit<ret>'");
        assert_eq!(
            (keys_of(&m), err),
            (vec!["e2e q :quit<ret>".to_string()], None)
        );
        // try runs its body; catch only when the body fails.
        let (m, _, err) = eval("try %{ map global user a b } catch %{ map global user c d }");
        assert_eq!((keys_of(&m), err), (vec!["user a b".to_string()], None));
        let (m, _, err) = eval("try %{ map window user a b } catch %{ map global user c d }");
        assert_eq!((keys_of(&m), err), (vec!["user c d".to_string()], None));
        let (m, _, err) = eval("try %{ map window user a b }\nmap global user e f");
        assert_eq!((keys_of(&m), err), (vec!["user e f".to_string()], None));
        // eval and evaluate-commands, with no switch but -no-hooks: read in place.
        let (m, _, _) = eval(
            "eval %{ map global user a b }\nevaluate-commands -no-hooks %{ map global user c d }\n\
             eval -draft %{ map global user e f }\neval -client c %{ map global user g h }\neval map global user i j",
        );
        assert_eq!(keys_of(&m), ["user a b", "user c d", "user i j"]);
        // Not read: hook and define-command bodies, %sh{}; a map with a typed expansion is counted.
        let (m, left_out, err) = eval(
            "hook global WinCreate .* %{ map window user a b }\ndefine-command x %{ map global user c d }\n\
             map global user e %sh{echo f}\nmap global user g \"%val{session}\"\nnop %sh{ map }\nsource x.kak",
        );
        assert_eq!((m.len(), left_out, err), (0, 2, None));
    }

    /// A command Kakoune refuses at startup stops its file there (as `source` does): `map window …`, a map to an
    /// undeclared mode, a key that is not one key, a bad switch.
    #[test]
    fn a_failing_command_stops_its_file() {
        for (bad, says) in [
            (
                "map window user a b",
                "'map': scope 'window' doesn't exist at startup",
            ),
            (
                "map global nosuch a b",
                "'map': no such keymap mode: 'nosuch'",
            ),
            (
                "map global user ab c",
                "'map': only a single key can be mapped",
            ),
            (
                "map global user <bogus> c",
                "'map': only a single key can be mapped",
            ),
            ("map -x global user a c", "'map': unknown option '-x'"),
            ("map global user a", "'map': wrong argument count"),
            (
                "declare-user-mode normal",
                "'declare-user-mode': 'normal' is already a regular mode",
            ),
            (
                "declare-user-mode a\ndeclare-user-mode a",
                "'declare-user-mode': user mode 'a' already defined",
            ),
            (
                "declare-user-mode 'a b'",
                "'declare-user-mode': invalid mode name: 'a b'",
            ),
            (
                "try %{ fail } catch %{ map window user a b }",
                "'map': scope 'window' doesn't exist at startup",
            ),
            (
                "try a b c",
                "'try': usage: try <commands> [catch <on error commands>]...",
            ),
            (
                "map global user c %{d",
                "parse error: unterminated string '%{...'",
            ),
        ] {
            let (m, _, err) = eval(&format!("map global user z y\n{bad}\nmap global user q r"));
            assert_eq!(keys_of(&m), ["user z y"], "{bad}");
            assert_eq!(err.as_deref(), Some(says), "{bad}");
        }
    }

    /// The fallback over files in `root`, with `vars` as the environment (no kak process).
    fn read_config(root: &Path, vars: &[(&str, String)]) -> (Vec<Mapping>, usize, Vec<String>) {
        let env = |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let mut notes = Vec::new();
        let _ = root;
        let (m, n) = fallback(&pin(), None, &env, &mut notes);
        (m, n, notes)
    }

    fn doc_of(m: &[Mapping], key: &str) -> String {
        m.iter()
            .find(|m| m.mode == "user" && m.key == key)
            .map(|m| m.doc.clone())
            .unwrap_or_default()
    }

    /// AC #5's files.
    #[test]
    fn the_files_kakoune_sources() {
        let d = fresh("files");
        let (cfg, rt, home) = (d.join("cfg"), d.join("rt"), d.join("home"));
        let map =
            |doc: &str| format!("map global user k x -docstring {doc}\nmap global user {doc} x\n");
        write(&rt.join("autoload/b.kak"), &map("b"));
        write(&rt.join("autoload/a.kak"), &map("a"));
        write(&rt.join("autoload/sub/c.kak"), &map("c"));
        write(&rt.join("autoload/not-kak.txt"), &map("t"));
        write(&rt.join("kakrc.local"), &map("l"));
        write(&cfg.join("kakrc"), &map("r"));
        let s = |p: &Path| p.display().to_string();
        let runtime = ("KAKOUNE_RUNTIME", s(&rt));
        let vars = [("KAKOUNE_CONFIG_DIR", s(&cfg)), runtime.clone()];
        let (m, _, notes) = read_config(&d, &vars);
        assert_eq!(doc_of(&m, "k"), "r", "{notes:?}");
        // Over the default maps; the runtime's autoload in sorted order, then kakrc.local, then kakrc.
        assert_eq!(m.len(), 12 + 6);
        let order: Vec<&str> = m[12..].iter().map(|m| m.key.as_str()).collect();
        assert_eq!(order, ["k", "a", "b", "c", "l", "r"]);
        std::fs::remove_file(cfg.join("kakrc")).unwrap();
        assert_eq!(doc_of(&read_config(&d, &vars).0, "k"), "l");
        std::fs::remove_file(rt.join("kakrc.local")).unwrap();
        assert_eq!(doc_of(&read_config(&d, &vars).0, "k"), "c");
        // A config autoload directory replaces the runtime's; links are followed.
        std::os::unix::fs::symlink(rt.join("autoload/sub"), cfg.join("autoload")).unwrap_or_else(
            |_| {
                std::fs::create_dir_all(cfg.join("autoload")).unwrap();
            },
        );
        let (m, _, _) = read_config(&d, &vars);
        assert_eq!(
            m[12..].iter().map(|m| m.key.as_str()).collect::<Vec<_>>(),
            ["k", "c"]
        );
        // `XDG_CONFIG_HOME` empty: `$HOME/.config/kak`.
        write(&home.join(".config/kak/kakrc"), &map("h"));
        let vars = [
            ("KAKOUNE_CONFIG_DIR", String::new()),
            ("XDG_CONFIG_HOME", String::new()),
            ("HOME", s(&home)),
            runtime.clone(),
        ];
        assert_eq!(doc_of(&read_config(&d, &vars).0, "k"), "h");
        write(&d.join("xdg/kak/kakrc"), &map("x"));
        let vars = [
            ("XDG_CONFIG_HOME", s(&d.join("xdg"))),
            ("HOME", s(&home)),
            runtime.clone(),
        ];
        assert_eq!(doc_of(&read_config(&d, &vars).0, "k"), "x");
        // A failing kakrc.local: Kakoune sources no kakrc after it; an unterminated string stops one file, with a note.
        write(&rt.join("kakrc.local"), "map window user a b\n");
        let vars = [("XDG_CONFIG_HOME", s(&d.join("xdg"))), runtime.clone()];
        let (m, _, notes) = read_config(&d, &vars);
        assert_eq!(doc_of(&m, "k"), "c", "{notes:?}");
        assert!(
            notes[0].contains("kakrc.local: 'map': scope 'window'"),
            "{notes:?}"
        );
        std::fs::remove_file(rt.join("kakrc.local")).unwrap();
        write(
            &d.join("xdg/kak/kakrc"),
            "map global user a b\nmap global user c %{d\nmap global user e f\n",
        );
        let (m, _, notes) = read_config(&d, &vars);
        assert_eq!(doc_of(&m, "a"), "");
        assert!(m.iter().any(|m| m.key == "a") && !m.iter().any(|m| m.key == "e"));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with("Kakoune reads no further in it"),
            "{notes:?}"
        );
        // An unreadable kakrc is one note; the others still count.
        std::fs::remove_file(d.join("xdg/kak/kakrc")).unwrap();
        std::fs::create_dir_all(d.join("xdg/kak/kakrc")).unwrap();
        let (m, _, notes) = read_config(&d, &vars);
        assert_eq!(m.len(), 12 + 4, "the runtime's a, b, c and k");
        assert!(
            notes.is_empty(),
            "a directory isn't a file Kakoune sources: {notes:?}"
        );
    }

    /// Kakoune's own `rc/tools/man.kak`: its four `man` maps, not the `map window` in its hooks.
    #[test]
    fn runtime_man_kak() {
        let text = fixture("man.kak");
        assert!(text.starts_with("# rc/tools/man.kak of mawww/kakoune tag v2026.05.21"));
        let (m, left_out, err) = eval(&text);
        assert_eq!((left_out, err), (0, None));
        assert_eq!(
            keys_of(&m),
            [
                "man g :man-jump<ret>",
                "man j :man-link-next<ret>",
                "man k :man-link-prev<ret>",
                "man m :man<space>"
            ]
        );
        assert_eq!(m[3].doc, "Look up a man page");
    }

    #[test]
    fn session_from_the_command_line() {
        let s = |args: &[&str]| {
            session(
                &args.iter().map(|a| a.to_string()).collect::<Vec<_>>(),
                4242,
            )
        };
        assert_eq!(s(&["f.c"]).as_deref(), Some("4242"));
        assert_eq!(s(&[]).as_deref(), Some("4242"));
        assert_eq!(s(&["-s", "s", "f.c"]).as_deref(), Some("s"));
        assert_eq!(s(&["-c", "s"]).as_deref(), Some("s"));
        assert_eq!(s(&["f", "-C", "s"]).as_deref(), Some("s"));
        assert_eq!(
            s(&["-e", "-s", "f"]).as_deref(),
            Some("4242"),
            "-s is -e's value"
        );
        assert_eq!(s(&["-n", "-ui", "dummy", "-s", "x"]).as_deref(), Some("x"));
        assert_eq!(s(&["--", "-s"]).as_deref(), Some("4242"));
        assert_eq!(s(&["-p", "s"]), None);
        assert_eq!(s(&["-c", "a", "-s", "b"]), None);
        assert_eq!(s(&["-s", "a", "-s", "b"]), None);
        assert_eq!(s(&["-bogus"]), None);
        assert_eq!(s(&["-s"]), None);
    }

    /// A fake `kak` (written through `sh`: ETXTBSY, see `plugins.rs`'s `write_script`): `-version` prints Kakoune's
    /// line; `-p S` logs `S` and its `XDG_RUNTIME_DIR` to `log`, then runs `body` with `$dir` (the directory named in
    /// the script on its stdin) and `$marker`. It exits 7 without `MISE_NOT_FOUND_AUTO_INSTALL=0`.
    fn fake_kak(d: &Path, body: &str) -> PathBuf {
        use std::io::Write;
        let log = d.join("log");
        let script = format!(
            "#!/bin/sh\n[ \"$MISE_NOT_FOUND_AUTO_INSTALL\" = 0 ] || exit 7\n\
             [ \"$1\" = -version ] && {{ echo 'Kakoune 2026.05.21'; exit 0; }}\n\
             in=$(cat)\nprintf '%s %s\\n' \"$2\" \"${{XDG_RUNTIME_DIR-unset}}\" >> '{}'\n\
             dir=$(printf '%s\\n' \"$in\" | sed -n \"s/.*echo -to-file '\\(.*\\)\\/done'.*/\\1/p\")\n\
             marker=$(printf '%s\\n' \"$in\" | sed -n 's/.*echo -debug \\(KEYBIND-FIND-[0-9-]*\\).*/\\1/p')\n{body}\n",
            log.display()
        );
        let path = d.join("kak");
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(sh.wait().unwrap().success());
        path
    }

    /// A stand-in `/proc` with a kak at pid 4242 run with `cmdline`, its environ `environ`.
    fn fake_proc(d: &Path, cmdline: &[&str], environ: &[String]) -> PathBuf {
        let proc = d.join("proc");
        let dir = proc.join("4242");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("comm"), "kak\n").unwrap();
        std::fs::write(dir.join("cmdline"), cmdline.join("\0") + "\0").unwrap();
        std::fs::write(dir.join("environ"), environ.join("\0") + "\0").unwrap();
        proc
    }

    /// AC #4: the live read through a fake `kak`, and each way it falls back.
    #[test]
    fn the_live_read() {
        let d = fresh("live");
        let (cfg, tmp) = (d.join("cfg"), d.join("tmp"));
        write(
            &cfg.join("kakrc"),
            "map global user r x -docstring 'from kakrc'\n",
        );
        std::fs::create_dir_all(&tmp).unwrap();
        let environ = [
            format!("KAKOUNE_CONFIG_DIR={}", cfg.display()),
            format!("KAKOUNE_RUNTIME={}", d.join("no-runtime").display()),
            "XDG_RUNTIME_DIR=/run/kak-test".into(),
        ];
        let proc = fake_proc(&d, &["kak", "-s", "sess", "f.c"], &environ);
        let none = |_: &str| None;
        let run = |kak: &Path| {
            let started = Instant::now();
            let out = sheet_with(
                REGISTRY,
                Some(4242),
                &proc,
                &kak.display().to_string(),
                &tmp,
                &none,
            )
            .unwrap();
            assert!(started.elapsed() < Duration::from_secs(3));
            // The private directory is gone on every path.
            assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
            out
        };
        let answer = "printf 'junk\\n%s\\nMappings:\\n * user w: '\"'\"':echo<space>window<ret>'\"'\"' e2e window\\n' \
                      \"$marker\" > \"$dir/maps\"; printf '2026.05.21\\n' > \"$dir/done\"";
        let kak = fake_kak(&d, answer);
        let (s, notes) = run(&kak);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(find(&s, "space w")[0].2, "e2e window");
        // The session's list is all there is: no kakrc, no default map it doesn't list.
        assert!(find(&s, "space r").is_empty() && normal(&s, "Left").is_none());
        assert_eq!(s.title, "Kakoune 2026.05.21");
        let log = std::fs::read_to_string(d.join("log")).unwrap();
        assert_eq!(log, "sess /run/kak-test\n");
        // Each failure falls back to the kakrc with one note.
        for (name, body, says) in [
            ("exit", "exit 1", "asking kak's session sess"),
            (
                "silent",
                "exit 0",
                "no answer from kak's session sess within 2 s",
            ),
            (
                "garbage",
                "echo garbage > \"$dir/maps\"; echo 2026.05.21 > \"$dir/done\"",
                "no marker",
            ),
        ] {
            let kak = fake_kak(&d, body);
            let (s, notes) = run(&kak);
            assert_eq!(notes.len(), 1, "{name}: {notes:?}");
            assert!(
                notes[0].contains(says) && notes[0].ends_with("; reading kakrc instead"),
                "{name}: {notes:?}"
            );
            assert_eq!(find(&s, "space r")[0].2, "from kakrc", "{name}");
            assert_eq!(normal(&s, "Left"), Some("h"), "{name}");
        }
        let (_, notes) = run(&d.join("no-such-kak"));
        assert!(notes[0].contains("asking kak's session"), "{notes:?}");
        assert!(notes[1].contains("version is unknown"), "{notes:?}");
        // A `-p` kak is no UI, and a pid that isn't kak's is never asked: the kakrc, silently.
        let kak = fake_kak(&d, "exit 1");
        std::fs::write(proc.join("4242/cmdline"), "kak\0-p\0sess\0").unwrap();
        assert!(run(&kak).1.is_empty());
        std::fs::write(proc.join("4242/cmdline"), "kak\0").unwrap();
        std::fs::write(proc.join("4242/comm"), "zsh\n").unwrap();
        let (s, notes) = sheet_with(
            REGISTRY,
            Some(4242),
            &proc,
            &kak.display().to_string(),
            &tmp,
            &none,
        )
        .unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert!(
            find(&s, "space r").is_empty(),
            "keybind-find's own (empty) environment"
        );
    }

    /// The real kak, where installed (the e2e image): a session with one map in its kakrc and a client beside it; the
    /// live read for the client's pid lists the map, and a pid no client has times out into the fallback.
    #[test]
    fn real_kakoune() {
        let mut v = Command::new("kak");
        v.arg("-version");
        if run_for(v, TIMEOUT).is_err() {
            eprintln!("skipping: kak is not installed");
            return;
        }
        let d = fresh("real");
        let (cfg, tmp) = (d.join("cfg"), d.join("tmp"));
        write(
            &cfg.join("kakrc"),
            "map global user k ':quit<ret>' -docstring 'real test map'\n",
        );
        std::fs::create_dir_all(&tmp).unwrap();
        let name = format!("kbf-test-{}", std::process::id());
        let null = std::process::Stdio::null;
        let mut server = Command::new("kak")
            .args(["-d", "-s", &name])
            .env("KAKOUNE_CONFIG_DIR", &cfg)
            .stdin(null())
            .stdout(null())
            .stderr(null())
            .spawn()
            .unwrap();
        let listed = || {
            let mut l = Command::new("kak");
            l.arg("-l");
            run_for(l, TIMEOUT).is_ok_and(|t| t.lines().any(|s| s == name))
        };
        for _ in 0..100 {
            if listed() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut client = Command::new("kak")
            .args(["-c", &name, "-ui", "dummy"])
            .stdin(null())
            .stdout(null())
            .stderr(null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        let none = |_: &str| None;
        let got = sheet_with(
            REGISTRY,
            Some(client.id()),
            Path::new("/proc"),
            "kak",
            &tmp,
            &none,
        );
        let proc = fake_proc(&d, &["kak", "-c", &name], &[]);
        let timed_out = sheet_with(REGISTRY, Some(4242), &proc, "kak", &tmp, &none);
        let mut kill = Command::new("kak");
        kill.args(["-p", &name]);
        let _ = run_for_input(kill, TIMEOUT, Some(b"kill!".to_vec()));
        let _ = client.kill();
        let _ = client.wait();
        let _ = server.kill();
        let _ = server.wait();
        let (s, notes) = got.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(find(&s, "space k")[0].2, "real test map");
        assert_eq!(normal(&s, "Left"), Some("h"));
        let (_, notes) = timed_out.unwrap();
        assert!(
            notes[0].contains("no answer from kak's session"),
            "{notes:?}"
        );
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
    }
}
