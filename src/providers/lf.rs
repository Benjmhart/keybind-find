//! lf: its keys in normal, visual and command-line mode. Asked of the running lf (`lf -remote "query <pid> maps"`)
//! when it can say; else lf r42's defaults (`data/lf/r42.lfrc`), then the lfrc files and `-command`s lf loaded, read as
//! lf reads them (a port of its scanner and parser at r42). Shown when lf is the program in the focused terminal. lf
//! is never sent a command, and no shell command in an lfrc is run.

use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet, Values};
use anyhow::{bail, Context as _};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// ponytail: one version, no probe; a second goes through `pinned::select` by `lf -version` (`42` on Arch, `r42`
/// upstream).
const DEFAULTS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/lf/r42.lfrc"));

const SYSTEM: &str = "/etc/lf/lfrc";

/// Each `lf -remote` run must end within this.
const TIMEOUT: Duration = Duration::from_secs(1);

/// How deep `source` may nest (lf has no limit; a file sourcing itself would never end), and how many files it reads.
const DEPTH: usize = 8;
const LOADS: usize = 64;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet() -> anyhow::Result<Sheet> {
    let (sheet, notes) = sheet_with(
        crate::context::editor_pid(),
        Path::new("/proc"),
        "lf",
        Path::new(SYSTEM),
        &|k| std::env::var(k).ok(),
    );
    for note in notes {
        eprintln!("keybind-find: lf: {note}");
    }
    Ok(sheet)
}

/// The sheet for the lf at `pid` (none: `dump lf` from a shell), its process read under `proc`, asking it with the
/// program `lf`; `system` is `/etc/lf/lfrc` and `env` keybind-find's own environment (test seams). Also the notes.
fn sheet_with(
    pid: Option<u32>,
    proc: &Path,
    lf: &str,
    system: &Path,
    env: Env,
) -> (Sheet, Vec<String>) {
    // The editor pid context found is lf's only when lf leads the terminal; check its comm.
    let pid = pid.filter(|p| {
        std::fs::read_to_string(proc.join(format!("{p}/comm")))
            .is_ok_and(|c| c.trim_end_matches('\n') == "lf")
    });
    let mut notes = Vec::new();
    let dir = pid.map(|p| proc.join(p.to_string()));
    let environ = dir.as_deref().and_then(environ);
    if let Some(pid) = pid {
        match live(pid, lf, environ.as_ref()) {
            Ok(Some(table)) => return finish(&table, notes),
            // lf has its maps only once it has run a shell command: nothing to say.
            Ok(None) => {}
            Err(e) => notes.push(format!("{e:#}; reading lfrc instead")),
        }
    }
    let table = fallback(dir, environ.as_ref(), system, env, &mut notes);
    finish(&table, notes)
}

type Environ = BTreeMap<String, String>;

/// A process's environment (`/proc/<pid>/environ`), `None` when it can't be read.
fn environ(dir: &Path) -> Option<Environ> {
    let b = std::fs::read(dir.join("environ")).ok()?;
    Some(
        nul_split(&b)
            .into_iter()
            .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.into(), v.into())))
            .collect(),
    )
}

/// lf's keys by mode: lf's key string → the bound expression's text (`String()`).
#[derive(Default, Debug, PartialEq)]
struct Table {
    normal: BTreeMap<String, String>,
    visual: BTreeMap<String, String>,
    command: BTreeMap<String, String>,
}

// --- live ---------------------------------------------------------------------------------------------------------

/// The running lf's own answer (`listBinds`, ui.go:1220-1254), or `None` when it has none yet (it fills it only in
/// `runShell`, app.go:566-579). lf finds its server's socket by `XDG_RUNTIME_DIR` (os.go:115-134): lf's own, when its
/// `environ` could be read.
/// ponytail: `lf` from PATH, not the focused lf's executable as vim's provider does; do that if versions ever differ.
fn live(pid: u32, lf: &str, environ: Option<&Environ>) -> anyhow::Result<Option<Table>> {
    let ask = |what: &str| -> anyhow::Result<String> {
        let query = format!("query {pid} {what}");
        let mut cmd = Command::new(lf);
        cmd.args(["-remote", &query])
            .env("LC_ALL", "C.UTF-8")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
        match environ.map(|e| e.get("XDG_RUNTIME_DIR")) {
            Some(Some(dir)) => cmd.env("XDG_RUNTIME_DIR", dir),
            Some(None) => cmd.env_remove("XDG_RUNTIME_DIR"),
            None => &mut cmd,
        };
        run_for(cmd, TIMEOUT).with_context(|| format!("asking lf ({lf} -remote \"{query}\")"))
    };
    let Some(maps) = rows(&ask("maps")?).context("lf's maps")? else {
        return Ok(None);
    };
    let cmaps = rows(&ask("cmaps")?)
        .context("lf's cmaps")?
        .unwrap_or_default();
    let mut table = Table::default();
    for (modes, key, text) in maps.into_iter().chain(cmaps) {
        for m in modes.chars() {
            let t = match m {
                'n' => &mut table.normal,
                'v' => &mut table.visual,
                'c' => &mut table.command,
                _ => bail!("lf's answer has an unknown mode {modes:?}"),
            };
            t.insert(key.clone(), text.clone());
        }
    }
    Ok(Some(table))
}

/// One `query` answer's rows (mode letters, key, command): a tabwriter table padded with tabs under the header `mode
/// key command`. `None` for an empty answer, or a header with no rows.
fn rows(answer: &str) -> anyhow::Result<Option<Vec<(String, String, String)>>> {
    let mut lines = answer.lines().filter(|l| !l.trim().is_empty());
    let Some(head) = lines.next() else {
        return Ok(None);
    };
    // The server's own errors (server.go: a missing client, a bad command) come back as text, with exit 0.
    if head.starts_with("listen:") || head.starts_with("remote command:") {
        bail!("lf answered {:?}", head.trim());
    }
    if fields(head) != ["mode", "key", "command"] {
        bail!(
            "lf's answer has no `mode key command` header: {:?}",
            head.trim()
        );
    }
    let mut out = Vec::new();
    for line in lines {
        let f = fields(line);
        let [mode, key, rest @ ..] = f.as_slice() else {
            bail!("unreadable row in lf's answer: {line:?}");
        };
        if rest.is_empty() {
            bail!("unreadable row in lf's answer: {line:?}");
        }
        out.push((mode.to_string(), key.to_string(), rest.join(" ")));
    }
    Ok((!out.is_empty()).then_some(out))
}

fn fields(line: &str) -> Vec<&str> {
    line.split('\t').filter(|f| !f.is_empty()).collect()
}

// --- fallback -----------------------------------------------------------------------------------------------------

/// lf's flags that take no value (main.go:238-312); every other flag takes one.
const BOOL_FLAGS: &[&str] = &[
    "doc",
    "help",
    "version",
    "server",
    "single",
    "print-last-dir",
    "print-selection",
];

/// `-config` and the `-command`s from lf's arguments, read as Go's `flag` package reads them: `-name value`,
/// `-name=value`, one or two dashes; it stops at `--`, `-` or the first non-flag.
fn flags(args: &[String]) -> (Option<String>, Vec<String>) {
    let (mut config, mut commands) = (None, Vec::new());
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        let Some(name) = arg.strip_prefix('-').filter(|n| !n.is_empty()) else {
            break;
        };
        let name = match name.strip_prefix('-') {
            Some("") => break,
            Some(n) => n,
            None => name,
        };
        // Go refuses `---x` and `-=x` (and lf exits): nothing more is a flag.
        if name.starts_with('-') || name.starts_with('=') {
            break;
        }
        let (name, value) = match name.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (name, None),
        };
        if BOOL_FLAGS.contains(&name) {
            continue;
        }
        let value = match value {
            Some(v) => v,
            None => {
                let Some(v) = args.get(i) else { break };
                i += 1;
                v.clone()
            }
        };
        match name {
            "config" => config = Some(value),
            "command" => commands.push(value),
            _ => {}
        }
    }
    (config, commands)
}

/// lf's keys from its config, as `run()` loads it (app.go:280-303): the defaults, then `-config`'s file alone or
/// `/etc/lf/lfrc` and `<config home>/lf/lfrc`, then each `-command`. `dir` is lf's `/proc/<pid>` (none: no lf).
fn fallback(
    dir: Option<PathBuf>,
    environ: Option<&Environ>,
    system: &Path,
    env: Env,
    notes: &mut Vec<String>,
) -> Table {
    let args: Vec<String> = dir
        .as_ref()
        .and_then(|d| std::fs::read(d.join("cmdline")).ok())
        .map(|b| nul_split(&b))
        .unwrap_or_default();
    let (config, commands) = flags(args.get(1..).unwrap_or_default());
    // An empty `-config` is none (app.go:280).
    let config = config.filter(|c| !c.is_empty());
    if dir.is_some() && environ.is_none() {
        notes.push("can't read lf's environment; using keybind-find's for its config paths".into());
    }
    let getenv = |k: &str| match environ {
        Some(m) => m.get(k).cloned(),
        None => env(k),
    };
    let mut ev = Eval {
        table: Table::default(),
        home: getenv("HOME").filter(|h| !h.is_empty()),
        cwd: dir
            .as_ref()
            .and_then(|d| std::fs::read_link(d.join("cwd")).ok()),
        loads: 0,
        notes,
    };
    ev.run(DEFAULTS, "the pinned defaults", 0);
    let files = match config {
        Some(c) => ev.resolve(&c, "-config").into_iter().collect(),
        None => {
            // `cmp.Or`: the first that is set and not empty (os.go:86-90).
            let home = ["LF_CONFIG_HOME", "XDG_CONFIG_HOME"]
                .iter()
                .find_map(|k| getenv(k).filter(|v| !v.is_empty()))
                .and_then(|h| ev.resolve(&h, "config home"))
                .or_else(|| ev.home.as_ref().map(|h| Path::new(h).join(".config")));
            [Some(system.to_path_buf()), home.map(|h| h.join("lf/lfrc"))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
        }
    };
    for f in files {
        // lf `Stat`s first: a missing file is skipped without a word.
        match std::fs::read(&f) {
            Ok(b) => ev.run(&String::from_utf8_lossy(&b), &f.display().to_string(), 0),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => ev.notes.push(format!("can't read {}: {e}", f.display())),
        }
    }
    for c in commands {
        ev.run(&c, "-command", 0);
    }
    ev.table
}

/// NUL-terminated strings, empty ones kept (`lf -config ""` has an empty argument).
fn nul_split(b: &[u8]) -> Vec<String> {
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    if b.is_empty() {
        return Vec::new();
    }
    b.split(|&c| c == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// What evaluating lfrc text does to the key tables (eval.go).
struct Eval<'a> {
    table: Table,
    home: Option<String>,
    /// lf's working directory, for a relative path.
    cwd: Option<PathBuf>,
    /// `source` files read so far.
    loads: usize,
    notes: &'a mut Vec<String>,
}

impl Eval<'_> {
    /// `readFile`'s loop (app.go:98-116): each expression in turn; a parse error ends the text, with one note.
    fn run(&mut self, text: &str, name: &str, depth: usize) {
        let mut p = Parser::new(text.as_bytes());
        while let Some(e) = p.parse_expr() {
            self.eval(&e, depth);
        }
        if let Some(err) = p.err {
            self.notes.push(format!("{name}: {err}"));
        }
    }

    fn eval(&mut self, e: &Expr, depth: usize) {
        match e {
            Expr::Map { kind, keys, expr } => {
                let t = &mut self.table;
                let tables: Vec<&mut BTreeMap<String, String>> = match *kind {
                    "map" => vec![&mut t.normal, &mut t.visual],
                    "nmap" => vec![&mut t.normal],
                    "vmap" => vec![&mut t.visual],
                    _ => vec![&mut t.command],
                };
                for table in tables {
                    match expr {
                        Some(x) => table.insert(keys.clone(), x.text()),
                        None => table.remove(keys),
                    };
                }
            }
            // eval.go:1666-1670: `:` stays bound so the user can still `:quit`.
            Expr::Call { name, .. } if name == "clearmaps" => {
                for t in [&mut self.table.normal, &mut self.table.visual] {
                    t.clear();
                    t.insert(":".into(), "read".into());
                }
            }
            Expr::Call { name, args } if name == "source" => match args.as_slice() {
                [path] => self.source(path, depth),
                _ => self.notes.push("source: requires an argument".into()),
            },
            Expr::List(items) => {
                for e in items {
                    self.eval(e, depth);
                }
            }
            // `cmd`, `set`, other commands and shell commands change no key.
            _ => {}
        }
    }

    /// `source PATH` (eval.go:1605-1610): `~` is the home directory; read in place.
    fn source(&mut self, path: &str, depth: usize) {
        if depth >= DEPTH || self.loads >= LOADS {
            self.notes.push(format!(
                "source {path}: not read (more than {DEPTH} deep or {LOADS} files)"
            ));
            return;
        }
        let Some(file) = self.resolve(path, "source") else {
            return;
        };
        self.loads += 1;
        match std::fs::read(&file) {
            Ok(b) => self.run(
                &String::from_utf8_lossy(&b),
                &file.display().to_string(),
                depth + 1,
            ),
            // lf reports a file it can't open (`readFile`).
            Err(e) => self.notes.push(format!("source {path}: {e}")),
        }
    }

    /// A path as lf opens it: `~` replaced by the home directory (`replaceTilde`, misc.go:34-39) for `source`; a
    /// relative path is relative to lf's directory, unknown without a running lf.
    fn resolve(&mut self, path: &str, what: &str) -> Option<PathBuf> {
        let path = match (what, path.strip_prefix('~')) {
            ("source", Some(rest)) => {
                let Some(home) = &self.home else {
                    self.notes.push(format!("{what} {path}: no home directory"));
                    return None;
                };
                format!("{home}{rest}")
            }
            _ => path.to_string(),
        };
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Some(path);
        }
        match &self.cwd {
            Some(cwd) => Some(cwd.join(path)),
            None => {
                self.notes.push(format!(
                    "{what} {}: a relative path, and lf's directory is unknown",
                    path.display()
                ));
                None
            }
        }
    }
}

// --- lfrc: lf r42's scanner (scan.go) and parser (parse.go) ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tok {
    Eof,
    Ident,
    Colon,
    Prefix,
    LBraces,
    RBraces,
    Command,
    Semicolon,
}

/// scan.go's `scanner`, byte for byte: its flags insert semicolons at line ends (`sem`, `nln`), read `push`'s keys as
/// one word (`key`), and read shell commands to the line end or inside `{{ }}` (`cmd`, `blk`).
struct Scanner<'a> {
    buf: &'a [u8],
    off: usize,
    chr: u8,
    sem: bool,
    nln: bool,
    eof: bool,
    key: bool,
    blk: bool,
    cmd: bool,
    typ: Tok,
    tok: String,
}

fn is_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ')
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl<'a> Scanner<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Scanner {
            buf,
            off: 0,
            chr: buf.first().copied().unwrap_or(0),
            sem: false,
            nln: false,
            eof: buf.is_empty(),
            key: false,
            blk: false,
            cmd: false,
            typ: Tok::Eof,
            tok: String::new(),
        }
    }

    fn next(&mut self) {
        if self.off + 1 < self.buf.len() {
            self.off += 1;
            self.chr = self.buf[self.off];
            return;
        }
        self.off = self.buf.len();
        self.chr = 0;
        self.eof = true;
    }

    fn peek(&self) -> u8 {
        self.buf.get(self.off + 1).copied().unwrap_or(0)
    }

    fn set(&mut self, typ: Tok, tok: impl Into<String>) {
        self.typ = typ;
        self.tok = tok.into();
    }

    /// scan.go:102-321.
    fn scan(&mut self) -> bool {
        loop {
            if self.eof {
                self.next();
                if self.sem {
                    self.set(Tok::Semicolon, "\n");
                    self.sem = false;
                    return true;
                }
                if self.nln {
                    self.set(Tok::Semicolon, "\n");
                    self.nln = false;
                    return true;
                }
                self.set(Tok::Eof, "EOF");
                return false;
            } else if self.key {
                let beg = self.off;
                while !self.eof && !is_space(self.chr) {
                    self.next();
                }
                self.set(Tok::Ident, text(&self.buf[beg..self.off]));
                self.key = false;
            } else if self.blk {
                if !self.cmd {
                    self.next();
                    self.next();
                    self.set(Tok::RBraces, "}}");
                    self.blk = false;
                    self.sem = true;
                    return true;
                }
                let beg = self.off;
                while !self.eof {
                    self.next();
                    if self.chr == b'}' && !self.eof && self.peek() == b'}' {
                        self.set(Tok::Command, text(&self.buf[beg..self.off]));
                        self.cmd = false;
                        return true;
                    }
                }
                self.set(Tok::Eof, "EOF");
                return false;
            } else if self.cmd {
                while !self.eof && is_space(self.chr) {
                    self.next();
                }
                if !self.eof && self.chr == b'{' && self.peek() == b'{' {
                    self.next();
                    self.next();
                    self.set(Tok::LBraces, "{{");
                    self.blk = true;
                    return true;
                }
                let beg = self.off;
                while !self.eof && self.chr != b'\n' {
                    self.next();
                }
                self.set(Tok::Command, text(&self.buf[beg..self.off]));
                self.cmd = false;
                self.sem = true;
            } else if self.chr == b'\n' {
                if self.sem {
                    self.set(Tok::Semicolon, "\n");
                    self.sem = false;
                    return true;
                }
                self.next();
                if self.nln {
                    self.set(Tok::Semicolon, "\n");
                    self.nln = false;
                    return true;
                }
                continue;
            } else if is_space(self.chr) {
                while !self.eof && is_space(self.chr) && self.chr != b'\n' {
                    self.next();
                }
                continue;
            } else if self.chr == b';' {
                self.set(Tok::Semicolon, ";");
                self.sem = false;
                self.next();
            } else if self.chr == b'#' {
                while !self.eof && self.chr != b'\n' {
                    self.next();
                }
                continue;
            } else if self.chr == b'"' {
                self.next();
                let mut buf = Vec::new();
                while !self.eof && self.chr != b'"' {
                    if self.chr == b'\\' {
                        self.next();
                        match self.chr {
                            b'"' | b'\\' => buf.push(self.chr),
                            b'a' => buf.push(0x07),
                            b'b' => buf.push(0x08),
                            b'f' => buf.push(0x0c),
                            b'n' => buf.push(b'\n'),
                            b'r' => buf.push(b'\r'),
                            b't' => buf.push(b'\t'),
                            b'v' => buf.push(0x0b),
                            c if c.is_ascii_digit() => {
                                let mut oct = Vec::new();
                                while self.chr.is_ascii_digit() {
                                    oct.push(self.chr);
                                    self.next();
                                }
                                buf.push(octal(&oct));
                                continue;
                            }
                            c => buf.extend([b'\\', c]),
                        }
                        self.next();
                        continue;
                    }
                    buf.push(self.chr);
                    self.next();
                }
                self.set(Tok::Ident, text(&buf));
                self.next();
            } else if self.chr == b'\'' {
                self.next();
                let beg = self.off;
                while !self.eof && self.chr != b'\'' {
                    self.next();
                }
                self.set(Tok::Ident, text(&self.buf[beg..self.off]));
                self.next();
            } else if self.chr == b':' {
                self.set(Tok::Colon, ":");
                self.nln = true;
                self.next();
            } else if self.chr == b'{' && self.peek() == b'{' {
                self.next();
                self.next();
                self.set(Tok::LBraces, "{{");
                self.sem = false;
                self.nln = false;
            } else if self.chr == b'}' && self.peek() == b'}' {
                self.next();
                self.next();
                self.set(Tok::RBraces, "}}");
                self.sem = true;
            } else if matches!(self.chr, b'$' | b'%' | b'!' | b'&') {
                self.set(Tok::Prefix, text(&[self.chr]));
                self.cmd = true;
                self.next();
            } else {
                let mut buf = Vec::new();
                while !self.eof && !is_space(self.chr) && self.chr != b';' && self.chr != b'#' {
                    if self.chr == b'\\' {
                        self.next();
                        buf.push(self.chr);
                        self.next();
                        continue;
                    }
                    buf.push(self.chr);
                    self.next();
                }
                self.set(Tok::Ident, text(&buf));
                self.sem = true;
                if self.tok == "push" {
                    self.key = true;
                    while !self.eof && is_space(self.chr) && self.chr != b'\n' {
                        self.next();
                    }
                }
            }
            return true;
        }
    }
}

/// `strconv.ParseUint(oct, 8, 8)` as scan.go uses it: a digit 8 or 9 is a syntax error (0), too big is 255.
fn octal(digits: &[u8]) -> u8 {
    if digits.iter().any(|&d| d > b'7') {
        return 0;
    }
    u8::from_str_radix(&text(digits), 8).unwrap_or(u8::MAX)
}

/// parse.go's expressions; `text` is their `String()`.
#[derive(Debug, PartialEq)]
enum Expr {
    Set {
        opt: String,
        val: String,
    },
    SetLocal {
        dir: String,
        opt: String,
        val: String,
    },
    /// `map`, `nmap`, `vmap` or `cmap`; no expression deletes the key.
    Map {
        kind: &'static str,
        keys: String,
        expr: Option<Box<Expr>>,
    },
    Cmd {
        name: String,
        expr: Option<Box<Expr>>,
    },
    Call {
        name: String,
        args: Vec<String>,
    },
    Exec {
        prefix: String,
        value: String,
    },
    List(Vec<Expr>),
}

impl Expr {
    /// parse.go:56-202.
    fn text(&self) -> String {
        let with = |head: String, e: &Option<Box<Expr>>| match e {
            Some(e) => format!("{head} {}", e.text()),
            None => head,
        };
        match self {
            Expr::Set { opt, val } if val.is_empty() => format!("set {opt}"),
            Expr::Set { opt, val } => format!("set {opt} {val}"),
            Expr::SetLocal { dir, opt, val } if val.is_empty() => format!("setlocal {dir} {opt}"),
            Expr::SetLocal { dir, opt, val } => format!("setlocal {dir} {opt} {val}"),
            Expr::Map { kind, keys, expr } => with(format!("{kind} {keys}"), expr),
            Expr::Cmd { name, expr } => with(format!("cmd {name}"), expr),
            Expr::Call { name, args } => std::iter::once(name)
                .chain(args)
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
            Expr::Exec { prefix, value } => {
                let lines: Vec<&str> = value.split('\n').collect();
                let first = lines
                    .iter()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty())
                    .map(|l| format!("{l}{}", if lines.len() > 1 { " ..." } else { "" }))
                    .unwrap_or_default();
                format!("{prefix}{{{{ {first} }}}}")
            }
            Expr::List(items) => {
                let inner: String = items.iter().map(|e| format!("{}; ", e.text())).collect();
                format!(":{{{{ {inner}}}}}")
            }
        }
    }
}

struct Parser<'a> {
    s: Scanner<'a>,
    err: Option<String>,
}

impl<'a> Parser<'a> {
    fn new(buf: &'a [u8]) -> Self {
        let mut s = Scanner::new(buf);
        s.scan();
        Parser { s, err: None }
    }

    /// The `map`/`nmap`/`vmap`/`cmap`/`cmd` arms: a name, then an expression unless the line ends there.
    fn named(&mut self) -> (String, Option<Box<Expr>>) {
        self.s.scan();
        let name = self.s.tok.clone();
        self.s.scan();
        let expr = if self.s.typ != Tok::Semicolon {
            self.parse_expr().map(Box::new)
        } else {
            self.s.scan();
            None
        };
        (name, expr)
    }

    /// `set` and `setlocal`'s value: the next token unless the line ends.
    fn value(&mut self) -> String {
        self.s.scan();
        let mut val = String::new();
        if self.s.typ != Tok::Semicolon {
            val = self.s.tok.clone();
            self.s.scan();
        }
        self.s.scan();
        val
    }

    /// parse.go:205-414.
    fn parse_expr(&mut self) -> Option<Expr> {
        match self.s.typ {
            Tok::Eof => None,
            Tok::Ident => Some(match self.s.tok.as_str() {
                "set" => {
                    self.s.scan();
                    if self.s.typ != Tok::Ident {
                        self.err = Some(format!("expected identifier: {:?}", self.s.tok));
                    }
                    let opt = self.s.tok.clone();
                    let val = self.value();
                    Expr::Set { opt, val }
                }
                "setlocal" => {
                    self.s.scan();
                    if self.s.typ != Tok::Ident {
                        self.err = Some(format!("expected directory: {:?}", self.s.tok));
                    }
                    let dir = self.s.tok.clone();
                    self.s.scan();
                    if self.s.typ != Tok::Ident {
                        self.err = Some(format!("expected identifier: {:?}", self.s.tok));
                    }
                    let opt = self.s.tok.clone();
                    let val = self.value();
                    Expr::SetLocal { dir, opt, val }
                }
                kind @ ("map" | "nmap" | "vmap" | "cmap") => {
                    let kind = match kind {
                        "map" => "map",
                        "nmap" => "nmap",
                        "vmap" => "vmap",
                        _ => "cmap",
                    };
                    let (keys, expr) = self.named();
                    Expr::Map { kind, keys, expr }
                }
                "cmd" => {
                    let (name, expr) = self.named();
                    Expr::Cmd { name, expr }
                }
                _ => {
                    let name = self.s.tok.clone();
                    let mut args = Vec::new();
                    while self.s.scan() && self.s.typ != Tok::Semicolon {
                        args.push(self.s.tok.clone());
                    }
                    self.s.scan();
                    Expr::Call { name, args }
                }
            }),
            Tok::Colon => {
                self.s.scan();
                let mut exprs = Vec::new();
                if self.s.typ == Tok::LBraces {
                    self.s.scan();
                    loop {
                        exprs.push(self.parse_expr()?);
                        if self.s.typ == Tok::RBraces {
                            break;
                        }
                    }
                    self.s.scan();
                } else {
                    loop {
                        exprs.push(self.parse_expr()?);
                        if self.s.tok == "\n" {
                            break;
                        }
                    }
                }
                self.s.scan();
                Some(Expr::List(exprs))
            }
            Tok::Prefix => {
                let prefix = self.s.tok.clone();
                self.s.scan();
                let value = if self.s.typ == Tok::LBraces {
                    self.s.scan();
                    let v = self.s.tok.clone();
                    self.s.scan();
                    v
                } else {
                    self.s.tok.clone()
                };
                self.s.scan();
                self.s.scan();
                Some(Expr::Exec { prefix, value })
            }
            _ => {
                self.err = Some(format!(
                    "unexpected token: {:?}; the rest is not read",
                    self.s.tok
                ));
                None
            }
        }
    }
}

// --- keys ---------------------------------------------------------------------------------------------------------

/// One key lf reads (`readKey`, key.go:164-182), as its bind string writes it.
#[derive(Debug, PartialEq)]
struct Key {
    /// Where it ends in the bind string (a shorter bind is a prefix up to here).
    end: usize,
    chord: String,
    /// A character lf's command line inserts rather than looks up (no alt; `readCmdEvent`, ui.go:1558-1569).
    rune: bool,
    /// A digit with no modifier: a count in normal and visual mode (ui.go:1399-1401).
    digit: bool,
    /// `<esc>`: after the first key it cancels (ui.go:1394-1398).
    esc: bool,
}

/// lf's named keys (key.go:11-127, `gKeyVal`, and `readKey`'s `<lt>` `<gt>` `<space>`) as X keysyms, with the
/// modifier a name carries and whether it is a character. `None`: one keybind-find can't send.
fn named(name: &str) -> Option<(Option<&'static str>, String, bool)> {
    let sym = match name {
        "enter" => "Return",
        "backspace" => "BackSpace",
        "tab" => "Tab",
        "backtab" => return Some((Some("shift"), "Tab".into(), false)),
        "esc" => "Escape",
        "delete" => "Delete",
        "insert" => "Insert",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pgup" => "Prior",
        "pgdn" => "Next",
        "clear" => "Clear",
        "pause" => "Pause",
        "print" => "Print",
        "space" => return Some((None, "space".into(), true)),
        "lt" => return Some((None, "less".into(), true)),
        "gt" => return Some((None, "greater".into(), true)),
        _ => {
            // F1-F35 have X keysyms; lf's `<f-36>`…`<f-64>` don't.
            let n: u8 = name.strip_prefix("f-")?.parse().ok()?;
            return (1..=35)
                .contains(&n)
                .then(|| (None, format!("F{n}"), false));
        }
    };
    Some((None, sym.into(), false))
}

/// A character's keysym: an upper-case letter as `shift` + its letter (yazi's `char_key`).
fn char_sym(c: char, mods: &mut Vec<&'static str>) -> Option<String> {
    if c.is_ascii_uppercase() {
        mods.push("shift");
    }
    // A raw space, `<` or `>` is never what lf reads (it writes `<space>`, `<lt>`, `<gt>`).
    if matches!(c, ' ' | '<' | '>') {
        return None;
    }
    super::nvim::single(c.to_ascii_lowercase()).map(str::to_string)
}

/// A bind string's keys, or `None` when one of them is a key lf never reads or keybind-find can't send: a pointer
/// event (`<m-…>`, ui.go:1440-1486), an unknown `<name>`, a raw `<` or `>`, a character with no keysym.
fn keys(s: &str) -> Option<Vec<Key>> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let at = s.len() - rest.len();
        let (len, key) = if c == '<' {
            let close = rest.find('>')?;
            (close + 1, angle(&rest[1..close])?)
        } else {
            let mut mods = Vec::new();
            let sym = char_sym(c, &mut mods)?;
            (
                c.len_utf8(),
                Key {
                    end: 0,
                    chord: chord(&mods, &sym),
                    rune: true,
                    digit: c.is_ascii_digit(),
                    esc: false,
                },
            )
        };
        out.push(Key {
            end: at + len,
            ..key
        });
        rest = &rest[len..];
    }
    Some(out)
}

/// The inside of `<…>`: a name, or `c-`/`s-`/`a-` and a character or a name.
fn angle(inner: &str) -> Option<Key> {
    let (modifier, base) = match inner.split_at_checked(2) {
        Some((m @ ("c-" | "s-" | "a-"), base)) if !base.is_empty() => (Some(m), base),
        _ => (None, inner),
    };
    if base.starts_with("m-") {
        return None;
    }
    let mut mods: Vec<&'static str> = modifier
        .map(|m| match m {
            "c-" => "ctrl",
            "s-" => "shift",
            _ => "alt",
        })
        .into_iter()
        .collect();
    let mut chars = base.chars();
    let (sym, is_char) = match (chars.next(), chars.next(), modifier) {
        (Some(c), None, Some(_)) => (char_sym(c, &mut mods)?, true),
        _ => {
            let (extra, sym, is_char) = named(base)?;
            mods.extend(extra);
            (sym, is_char)
        }
    };
    Some(Key {
        end: 0,
        rune: is_char && matches!(modifier, None | Some("s-")),
        digit: false,
        // ui.go:1394 checks the key, whatever its modifiers.
        esc: base == "esc",
        chord: chord(&mods, &sym),
    })
}

/// Whether lf can reach `key` in normal or visual mode `t`: no leading count digit, no `<esc>` after the first key,
/// and no shorter bind that runs first (`findBinds`/`exact`, ui.go:1206-1218, 1406-1420).
fn reachable(t: &BTreeMap<String, String>, key: &str, ks: &[Key]) -> bool {
    let Some(first) = ks.first() else {
        return false;
    };
    !first.digit
        && !ks[1..].iter().any(|k| k.esc)
        && !ks[..ks.len() - 1]
            .iter()
            .any(|k| t.contains_key(&key[..k.end]))
}

fn category(text: &str) -> String {
    let b = text.as_bytes();
    if b.len() > 2 && matches!(b[0], b'$' | b'%' | b'!' | b'&') && &b[1..3] == b"{{" {
        "shell".into()
    } else if text.starts_with(":{{") {
        "list".into()
    } else {
        text.split_whitespace().next().unwrap_or("lf").to_string()
    }
}

/// The sheet from the tables, both paths' shared end, and the left-out count note (FR-019).
fn finish(t: &Table, mut notes: Vec<String>) -> (Sheet, Vec<String>) {
    let mut sheet = Sheet {
        title: "lf".into(),
        provider: "lf".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    let mut add = |ks: &[Key], text: &str, modes: &[&str]| {
        let chord = ks
            .iter()
            .map(|k| k.chord.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let names = modes
            .iter()
            .filter_map(|m| m.parse().ok())
            .collect::<Vec<_>>();
        let mut when = BTreeMap::new();
        if let (Ok(scope), Ok(v)) = ("mode".parse(), Values::try_from(names)) {
            when.insert(scope, v);
        }
        sheet.binds.entry(category(text)).or_default().push(Bind {
            key: chord,
            desc: text.to_string(),
            action: text.to_string(),
            when,
            run: Vec::new(),
        });
    };
    let nv: std::collections::BTreeSet<&String> = t.normal.keys().chain(t.visual.keys()).collect();
    for key in nv {
        let ks = keys(key);
        let ok = |t: &BTreeMap<String, String>| ks.as_ref().is_some_and(|ks| reachable(t, key, ks));
        let n = t.normal.get(key).filter(|_| ok(&t.normal));
        let v = t.visual.get(key).filter(|_| ok(&t.visual));
        // Unreachable in a mode it is bound in: counted once per bind (one, when both modes bind the same text).
        let bound = [t.normal.get(key), t.visual.get(key)];
        let Some(ks) = ks.as_deref().filter(|_| n.is_some() || v.is_some()) else {
            let same = bound[0].is_some() && bound[0] == bound[1];
            left_out += if same {
                1
            } else {
                bound.iter().flatten().count()
            };
            continue;
        };
        match (n, v) {
            (Some(a), Some(b)) if a == b => add(ks, a, &["normal", "visual"]),
            _ => {
                if let Some(a) = n {
                    add(ks, a, &["normal"]);
                }
                if let Some(b) = v {
                    add(ks, b, &["visual"]);
                }
                let same = bound[0].is_some() && bound[0] == bound[1];
                let missing = usize::from(bound[0].is_some() && n.is_none())
                    + usize::from(bound[1].is_some() && v.is_none());
                left_out += if same { missing.min(1) } else { missing };
            }
        }
    }
    for (key, text) in &t.command {
        match keys(key) {
            Some(ks) if ks.len() == 1 && !ks[0].rune => add(&ks, text, &["command-line"]),
            _ => left_out += 1,
        }
    }
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) lf never reads from the keyboard or runs (a pointer event, a count digit, a \
             key after a shorter bind or <esc>, a typed character on the command line) or keybind-find can't send"
        ));
    }
    (sheet, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/lf")
                .join(name),
        )
        .unwrap()
    }

    const E2E_LFRC: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/e2e/home/.config/lf/lfrc"
    ));

    /// The tables from lfrc `texts` in order (no defaults unless given), and the notes.
    fn eval(texts: &[&str]) -> (Table, Vec<String>) {
        let mut notes = Vec::new();
        let mut ev = Eval {
            table: Table::default(),
            home: Some("/home/u".into()),
            cwd: None,
            loads: 0,
            notes: &mut notes,
        };
        for t in texts {
            ev.run(t, "test", 0);
        }
        let table = ev.table;
        (table, notes)
    }

    fn sheet_of(texts: &[&str]) -> (Sheet, Vec<String>) {
        let (t, notes) = eval(texts);
        finish(&t, notes)
    }

    fn modes(b: &Bind) -> String {
        b.when["mode"]
            .iter()
            .map(|n| n.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The binds for `key` in `modes` (`normal,visual`, `command-line`), with their category.
    fn find<'a>(s: &'a Sheet, key: &str, mode: &str) -> Vec<(&'a str, &'a Bind)> {
        s.binds
            .iter()
            .flat_map(|(c, bs)| bs.iter().map(move |b| (c.as_str(), b)))
            .filter(|(_, b)| b.key == key && modes(b) == mode)
            .collect()
    }

    fn desc<'a>(s: &'a Sheet, key: &str, mode: &str) -> &'a str {
        let found = find(s, key, mode);
        assert_eq!(found.len(), 1, "{key} {mode}: {found:?}");
        &found[0].1.desc
    }

    fn count(s: &Sheet) -> usize {
        s.binds.values().map(Vec::len).sum()
    }

    /// The left-out count from the notes (0 when there's no such note).
    fn left_out(notes: &[String]) -> usize {
        notes
            .iter()
            .find_map(|n| {
                n.strip_prefix("leaving out ")?
                    .split(' ')
                    .next()?
                    .parse()
                    .ok()
            })
            .unwrap_or(0)
    }

    #[test]
    fn the_pinned_defaults() {
        let (t, notes) = eval(&[DEFAULTS]);
        assert!(notes.is_empty(), "{notes:?}");
        // opts.go: 71 shared keys, 3 normal-only, 3 visual-only, 34 command-line; setDefaults: 4 in both.
        assert_eq!(
            (t.normal.len(), t.visual.len(), t.command.len()),
            (78, 78, 34)
        );
        let (s, notes) = finish(&t, Vec::new());
        assert_eq!(desc(&s, "space", "normal"), ":{{ toggle; down; }}");
        assert_eq!(desc(&s, "shift+v", "normal"), "visual");
        assert_eq!(desc(&s, "shift+v", "visual"), "visual-accept");
        assert_eq!(desc(&s, "z h", "normal,visual"), "set hidden!");
        assert_eq!(desc(&s, "z a", "normal,visual"), "set info size:time");
        assert_eq!(
            desc(&s, "s n", "normal,visual"),
            ":{{ set sortby natural; set info; }}"
        );
        assert_eq!(desc(&s, "colon", "normal,visual"), "read");
        assert_eq!(desc(&s, "quotedbl", "normal,visual"), "mark-remove");
        assert_eq!(desc(&s, "apostrophe", "normal,visual"), "mark-load");
        assert_eq!(desc(&s, "dollar", "normal,visual"), "shell");
        assert_eq!(desc(&s, "e", "normal,visual"), "${{ $EDITOR \"$f\" }}");
        assert_eq!(desc(&s, "F1", "normal,visual"), "help");
        assert_eq!(desc(&s, "g h", "normal,visual"), "cd ~");
        assert_eq!(desc(&s, "Escape", "visual"), "visual-discard");
        assert_eq!(desc(&s, "ctrl+u", "command-line"), "cmd-delete-home");
        assert_eq!(
            desc(&s, "alt+BackSpace", "command-line"),
            "cmd-delete-word-back"
        );
        assert_eq!(find(&s, "e", "normal,visual")[0].0, "shell");
        assert_eq!(find(&s, "s n", "normal,visual")[0].0, "list");
        assert_eq!(find(&s, "z h", "normal,visual")[0].0, "set");
        // Left out: the four pointer keys (<m-up> <m-down> <c-m-up> <c-m-down>) and the command line's <space>.
        assert_eq!(left_out(&notes), 5, "{notes:?}");
        // 71 shared keys (75 less the 4 pointer keys), V twice, v, <space>, <esc>, o; 33 command-line keys.
        assert_eq!(count(&s), 71 + 2 + 4 + 33);
    }

    #[test]
    fn lfrc_example() {
        let example = fixture("lfrc.example");
        let (s, notes) = sheet_of(&[DEFAULTS, &example]);
        assert_eq!(left_out(&notes), 5, "{notes:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        // The example adds <enter>, `, x, X, o, O, a; o was visual-only (visual-change), now shared.
        assert_eq!(count(&s), 110 + 6);
        assert_eq!(desc(&s, "Return", "normal,visual"), "shell");
        let x = find(&s, "x", "normal,visual");
        assert_eq!((x[0].0, x[0].1.desc.as_str()), ("shell", "${{ $f }}"));
        assert_eq!(desc(&s, "grave", "normal,visual"), "!{{ true }}");
        assert_eq!(
            desc(&s, "shift+o", "normal,visual"),
            "${{ mimeopen --ask \"$f\" }}"
        );
        assert_eq!(desc(&s, "o", "normal,visual"), "&{{ mimeopen \"$f\" }}");
        let a = find(&s, "a", "normal,visual");
        assert_eq!(
            (a[0].0, a[0].1.desc.as_str()),
            ("list", ":{{ push %mkdir<space>; }}")
        );
        assert_eq!(desc(&s, "g g", "normal,visual"), "top");
        assert_eq!(desc(&s, "ctrl+u", "command-line"), "cmd-delete-home");
        // `cmd open &{{ … }}` binds nothing, and nothing after it is lost.
        assert!(s
            .binds
            .values()
            .flatten()
            .all(|b| !b.desc.contains("mime-type")));
    }

    fn parse_one(text: &str) -> Expr {
        let mut p = Parser::new(text.as_bytes());
        let e = p.parse_expr().unwrap();
        assert!(p.err.is_none(), "{:?}", p.err);
        e
    }

    fn call(name: &str, args: &[&str]) -> Option<Box<Expr>> {
        Some(Box::new(Expr::Call {
            name: name.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }))
    }

    #[test]
    fn scanner_and_parser() {
        let (t, notes) = eval(&[concat!(
            "map ; find-next\n",
            "map \"<c-x>\" quit\n",
            "map x $echo a; echo b\n",
            "cmd a ${{\n  one\n  two\n}}\n",
            "map m ${{\n  first\n  second\n}}\n",
            ":{{ nmap a up; vmap b down; }}\n",
            "map c up # a comment\n",
            "map z down\n",
        )]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(t.normal[";"], "find-next");
        assert_eq!(t.normal["<c-x>"], "quit");
        // A shell command runs to the line end, `;` included.
        assert_eq!(t.normal["x"], "${{ echo a; echo b }}");
        assert_eq!(t.normal["m"], "${{ first ... }}");
        assert_eq!(
            (t.normal.get("a"), t.visual.get("a")),
            (Some(&"up".to_string()), None)
        );
        assert_eq!(
            (t.normal.get("b"), t.visual.get("b")),
            (None, Some(&"down".to_string()))
        );
        assert_eq!(t.normal["c"], "up");
        assert_eq!(t.visual["z"], "down");
        // `\ ` in a bare word, '…' raw, "\101" octal.
        assert_eq!(
            parse_one("map a echo foo\\ bar"),
            Expr::Map {
                kind: "map",
                keys: "a".into(),
                expr: call("echo", &["foo bar"])
            }
        );
        assert_eq!(
            parse_one("map b echo 'a\\nb'"),
            Expr::Map {
                kind: "map",
                keys: "b".into(),
                expr: call("echo", &["a\\nb"])
            }
        );
        assert_eq!(
            parse_one("map c echo \"\\101\\t\\\"\""),
            Expr::Map {
                kind: "map",
                keys: "c".into(),
                expr: call("echo", &["A\t\""])
            }
        );
        assert_eq!(octal(b"101"), b'A');
        // Without the last `;`, `down` takes `}}` as an argument and the list never ends: nothing after it is bound
        // (checked against lf r42 in the e2e image).
        let (t, notes) = eval(&[":{{ nmap a up; vmap b down }}\nmap z top\n"]);
        assert!(
            t.normal.is_empty() && t.visual.is_empty() && notes.is_empty(),
            "{t:?} {notes:?}"
        );
        assert_eq!((octal(b"9"), octal(b"777")), (0, 255));
    }

    #[test]
    fn deleting_and_clearing() {
        let base = "map d cut\ncmap <c-a> cmd-home\n";
        let (t, _) = eval(&[base, "map d"]);
        assert!(!t.normal.contains_key("d") && !t.visual.contains_key("d"));
        let (t, _) = eval(&[base, "nmap d"]);
        assert!(!t.normal.contains_key("d") && t.visual.contains_key("d"));
        let (t, _) = eval(&[DEFAULTS, "clearmaps\nmap q quit"]);
        assert_eq!(t.normal.len(), 2);
        assert_eq!(t.normal[":"], "read");
        assert_eq!(t.visual.keys().collect::<Vec<_>>(), [":", "q"]);
        assert_eq!(t.command.len(), 34);
    }

    #[test]
    fn a_parse_error_stops_the_file() {
        let (t, notes) = eval(&["map a up\n}}\nmap b down\n"]);
        assert_eq!(t.normal.keys().collect::<Vec<_>>(), ["a"]);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("test: unexpected token: \"}}\""),
            "{notes:?}"
        );
    }

    fn notation(s: &str) -> Option<String> {
        Some(
            keys(s)?
                .iter()
                .map(|k| k.chord.clone())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    #[test]
    fn notation_of_lf_keys() {
        for (lf, want) in [
            ("<c-x>d", "ctrl+x d"),
            ("G", "shift+g"),
            ("<a-X>", "alt+shift+x"),
            ("<a-f>", "alt+f"),
            ("<lt>", "less"),
            ("<gt>", "greater"),
            ("<backtab>", "shift+Tab"),
            ("<f-12>", "F12"),
            ("<f-35>", "F35"),
            ("<pgup><pgdn>", "Prior Next"),
            ("<c-up>", "ctrl+Up"),
            ("<a-backspace>", "alt+BackSpace"),
            ("<c-m>", "ctrl+m"),
            ("gg", "g g"),
        ] {
            assert_eq!(notation(lf).as_deref(), Some(want), "{lf}");
        }
        for lf in [
            "<f-40>",
            "<f-36>",
            "<upleft>",
            "<center>",
            "<foo>",
            "<m-1>",
            "<m-up>",
            "<c-m-down>",
            "<",
            "a>",
            "<a",
            "é",
            " ",
        ] {
            assert_eq!(notation(lf), None, "{lf}");
        }
    }

    #[test]
    fn keys_lf_never_reaches_are_left_out() {
        let (s, notes) = sheet_of(&[concat!(
            "map 1x up\n",
            "map g1 up\n",
            "map g<esc> up\n",
            "map Q quit\n",
            "map Qa top\n",
            "map <esc> up\n",
            "cmap <a-x> cmd-escape\n",
            "cmap x cmd-escape\n",
            "cmap <space> cmd-escape\n",
            "cmap <s-a> cmd-escape\n",
            "cmap <c-a>b cmd-escape\n",
            "cmap <c-a> cmd-home\n",
        )]);
        assert_eq!(desc(&s, "g 1", "normal,visual"), "up");
        assert_eq!(desc(&s, "shift+q", "normal,visual"), "quit");
        // `<esc>` first is a key like any other.
        assert_eq!(desc(&s, "Escape", "normal,visual"), "up");
        assert_eq!(desc(&s, "alt+x", "command-line"), "cmd-escape");
        assert_eq!(desc(&s, "ctrl+a", "command-line"), "cmd-home");
        assert_eq!(count(&s), 5);
        // 1x (a count), g<esc>, Qa (Q runs first); x, <space> and <s-a> are typed; <c-a>b is two keys.
        assert_eq!(left_out(&notes), 7, "{notes:?}");
        // A prefix bound in normal mode only leaves the longer key out of normal mode only.
        let (s, notes) = sheet_of(&["nmap Q quit\nmap Qa top\n"]);
        assert_eq!(desc(&s, "shift+q a", "visual"), "top");
        assert_eq!(left_out(&notes), 1);
        assert!(sheet_of(&["map a up"]).1.is_empty());
    }

    #[test]
    fn the_live_answer() {
        let maps = rows(&fixture("maps.txt")).unwrap().unwrap();
        assert!(maps
            .iter()
            .any(|r| r == &("n".into(), "Y".into(), "e2e-live".into())));
        // lf's own answer equals the fallback over the same config, plus what on-init binds.
        let (mut want, notes) = eval(&[DEFAULTS, E2E_LFRC]);
        assert!(notes.is_empty(), "{notes:?}");
        want.normal.insert("Y".into(), "e2e-live".into());
        let root = homes("live");
        let log = root.join("queries");
        let lf = fake_lf(
            "answer",
            &format!(
                "printf '%s %s\\n' \"$2\" \"$XDG_RUNTIME_DIR\" >> '{}'\ncase \"$2\" in 'query 4242 maps') cat '{}' ;; 'query 4242 cmaps') cat '{}' ;; *) exit 3 ;; esac",
                log.display(),
                fixture_path("maps.txt").display(),
                fixture_path("cmaps.txt").display(),
            ),
        );
        let lf = lf.display().to_string();
        let lf_env = Environ::from([("XDG_RUNTIME_DIR".into(), "/lf-run".into())]);
        let got = live(4242, &lf, Some(&lf_env)).unwrap().unwrap();
        assert_eq!(got, want);
        // Two queries and nothing else, with lf's own runtime directory (its socket's).
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "query 4242 maps /lf-run\nquery 4242 cmaps /lf-run\n"
        );
        // Through `sheet_with`: the answer is the sheet, and the home's lfrc (`map a home`) isn't read.
        let home = format!("HOME={}/home", root.display());
        let proc = fake_proc("live", &["lf"], Some(&[home.as_str()]), &root.join("cwd"));
        let (s, notes) = sheet_with(Some(4242), &proc, &lf, &root.join("etc/lfrc"), &|_| None);
        assert!(find(&s, "a", "normal,visual").is_empty());
        assert_eq!(left_out(&notes), 7, "{notes:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        let (s, notes) = finish(&got, Vec::new());
        assert_eq!(desc(&s, "shift+y", "normal"), "e2e-live");
        assert_eq!(desc(&s, "shift+x", "normal"), "quit");
        assert_eq!(desc(&s, "k", "normal,visual"), "up");
        assert_eq!(desc(&s, "alt+x", "command-line"), "cmd-escape");
        // A user `cmd`'s name is its category.
        assert_eq!(find(&s, "shift+y", "normal")[0].0, "e2e-live");
        // The pointer keys, <space>, 1 (a count) and Qa (after Q).
        assert_eq!(left_out(&notes), 7, "{notes:?}");
    }

    #[test]
    fn answers_that_fall_back() {
        assert_eq!(rows("").unwrap(), None);
        assert_eq!(rows("mode\tkey\tcommand\n").unwrap(), None);
        let err = rows("remote command: bad\n").unwrap_err();
        assert!(err.to_string().contains("remote command"), "{err}");
        let err = rows("listen: query: no such client id is connected\n").unwrap_err();
        assert!(err.to_string().contains("no such client"), "{err}");
        assert!(rows("n\tk\tup\n").is_err());
        assert!(rows("mode\tkey\tcommand\nn\tk\n").is_err());
        assert_eq!(
            rows("mode\t\tkey\tcommand\nnv\t\tk\t\tset\t\tinfo\n")
                .unwrap()
                .unwrap(),
            [("nv".into(), "k".into(), "set info".into())]
        );
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/lf")
            .join(name)
    }

    /// A fake `lf` running `body` (`$1` `-remote`, `$2` the query), written through `sh` (ETXTBSY: see `plugins.rs`'s
    /// `write_script`). It exits 7 unless run with `LC_ALL=C.UTF-8` and `MISE_NOT_FOUND_AUTO_INSTALL=0`.
    fn fake_lf(name: &str, body: &str) -> PathBuf {
        use std::io::Write;
        let dir = crate::test_tmp().join(format!("kbf-lf-fake-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = -remote ] && [ \"$LC_ALL\" = C.UTF-8 ] && [ \"$MISE_NOT_FOUND_AUTO_INSTALL\" = 0 ] || exit 7\n{body}\n"
        );
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(dir.join("lf"))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(sh.wait().unwrap().success());
        dir.join("lf")
    }

    /// A stand-in `/proc` with an lf at pid 4242 run with `cmdline`, its `environ` (none: unreadable) and `cwd`.
    fn fake_proc(name: &str, cmdline: &[&str], environ: Option<&[&str]>, cwd: &Path) -> PathBuf {
        let proc = crate::test_tmp().join(format!("kbf-lf-proc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proc);
        let dir = proc.join("4242");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("comm"), "lf\n").unwrap();
        std::fs::write(dir.join("cmdline"), cmdline.join("\0") + "\0").unwrap();
        if let Some(env) = environ {
            std::fs::write(dir.join("environ"), env.join("\0") + "\0").unwrap();
        }
        std::os::unix::fs::symlink(cwd, dir.join("cwd")).unwrap();
        proc
    }

    #[test]
    fn a_failing_lf_falls_back_with_one_note() {
        let empty = crate::test_tmp().join(format!("kbf-lf-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let none = |_: &str| None;
        for (name, body, says) in [
            ("exit", "exit 1", "asking lf"),
            ("hang", "exec sleep 5", "asking lf"),
            (
                "listen",
                "echo 'listen: query: no such client id is connected'",
                "no such client",
            ),
            (
                "header",
                "printf 'n\\tk\\tup\\n'",
                "no `mode key command` header",
            ),
        ] {
            let lf = fake_lf(name, body);
            let proc = fake_proc(name, &["lf"], Some(&[]), &empty);
            let started = std::time::Instant::now();
            let (s, notes) = sheet_with(
                Some(4242),
                &proc,
                &lf.display().to_string(),
                &empty.join("no"),
                &none,
            );
            assert!(started.elapsed() < Duration::from_secs(3), "{name}");
            assert_eq!(notes.len(), 2, "{name}: {notes:?}");
            assert!(
                notes[0].contains(says) && notes[0].ends_with("reading lfrc instead"),
                "{name}: {notes:?}"
            );
            assert_eq!(desc(&s, "k", "normal,visual"), "up", "{name}");
        }
        // An empty answer (lf hasn't run a shell command yet) falls back without a word; lf not on PATH says so.
        let lf = fake_lf("empty", "exit 0");
        let proc = fake_proc("empty", &["lf"], Some(&[]), &empty);
        let (_, notes) = sheet_with(
            Some(4242),
            &proc,
            &lf.display().to_string(),
            &empty.join("no"),
            &none,
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        let (_, notes) = sheet_with(
            Some(4242),
            &proc,
            "/nonexistent/lf",
            &empty.join("no"),
            &none,
        );
        assert!(notes[0].contains("asking lf"), "{notes:?}");
        // A pid that isn't lf's is never asked.
        std::fs::write(proc.join("4242/comm"), "zsh\n").unwrap();
        let (_, notes) = sheet_with(
            Some(4242),
            &proc,
            "/nonexistent/lf",
            &empty.join("no"),
            &none,
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
    }

    #[test]
    fn go_flags() {
        let a = |v: &[&str]| flags(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["-config", "x"]), (Some("x".into()), vec![]));
        assert_eq!(a(&["--config=x"]), (Some("x".into()), vec![]));
        assert_eq!(
            a(&["-command", "map a up", "--command=map b down"]),
            (None, vec!["map a up".into(), "map b down".into()])
        );
        assert_eq!(a(&["-single", "-config", "x"]), (Some("x".into()), vec![]));
        assert_eq!(
            a(&["-single=true", "-config", "x"]),
            (Some("x".into()), vec![])
        );
        assert_eq!(a(&["dir", "-config", "x"]), (None, vec![]));
        assert_eq!(a(&["--", "-config", "x"]), (None, vec![]));
        assert_eq!(a(&["-", "-config", "x"]), (None, vec![]));
        assert_eq!(
            a(&["-log", "/tmp/l", "-config", "x"]),
            (Some("x".into()), vec![])
        );
        assert_eq!(a(&["-config"]), (None, vec![]));
    }

    /// A home with `.config/lf/lfrc`, an `xdg` and an `lfc` config home, a `cwd` with `rel.lfrc` and `cfg`.
    fn homes(name: &str) -> PathBuf {
        let root = crate::test_tmp().join(format!("kbf-lf-homes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in [
            ("home/.config/lf/lfrc", "map a home"),
            ("xdg/lf/lfrc", "map a xdg"),
            ("lfc/lf/lfrc", "map a lfc"),
            ("etc/lfrc", "map a etc\nmap b etc"),
            ("cwd/rel.lfrc", "map r rel"),
            ("cwd/cfg", "map a cfg\nsource rel.lfrc"),
            ("home/src.lfrc", "map t tilde"),
        ] {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        root
    }

    fn discover(
        root: &Path,
        name: &str,
        cmdline: &[&str],
        environ: Option<&[String]>,
        own: Env,
    ) -> (Table, Vec<String>) {
        let environ: Option<Vec<&str>> = environ.map(|e| e.iter().map(String::as_str).collect());
        let proc = fake_proc(name, cmdline, environ.as_deref(), &root.join("cwd"));
        let dir = proc.join("4242");
        let mut notes = Vec::new();
        let t = fallback(
            Some(dir.clone()),
            super::environ(&dir).as_ref(),
            &root.join("etc/lfrc"),
            own,
            &mut notes,
        );
        (t, notes)
    }

    #[test]
    fn the_files_lf_loads() {
        let root = homes("files");
        let r = root.display();
        let none = |_: &str| None;
        let env = |extra: &[String]| -> Vec<String> {
            [format!("HOME={r}/home")]
                .into_iter()
                .chain(extra.iter().cloned())
                .collect()
        };
        let a = |t: &Table| t.normal["a"].clone();
        // No flags: /etc/lf/lfrc, then <config home>/lf/lfrc, the first non-empty of LF_CONFIG_HOME, XDG_CONFIG_HOME,
        // $HOME/.config.
        let (t, n) = discover(&root, "plain", &["lf"], Some(&env(&[])), &none);
        assert_eq!((a(&t), t.normal["b"].as_str()), ("home".into(), "etc"));
        assert!(n.is_empty(), "{n:?}");
        let (t, _) = discover(
            &root,
            "xdg",
            &["lf"],
            Some(&env(&[format!("XDG_CONFIG_HOME={r}/xdg")])),
            &none,
        );
        assert_eq!(a(&t), "xdg");
        let (t, _) = discover(
            &root,
            "lfc",
            &["lf"],
            Some(&env(&[
                format!("XDG_CONFIG_HOME={r}/xdg"),
                format!("LF_CONFIG_HOME={r}/lfc"),
            ])),
            &none,
        );
        assert_eq!(a(&t), "lfc");
        let (t, _) = discover(
            &root,
            "blank",
            &["lf"],
            Some(&env(&["LF_CONFIG_HOME=".into(), "XDG_CONFIG_HOME=".into()])),
            &none,
        );
        assert_eq!(a(&t), "home");
        // -config: only that file (relative to lf's directory), which may `source` relative to it too.
        let (t, n) = discover(
            &root,
            "config",
            &["lf", "-config", "cfg"],
            Some(&env(&[])),
            &none,
        );
        assert_eq!(
            (a(&t), t.normal["r"].as_str(), t.normal.get("b")),
            ("cfg".into(), "rel", None)
        );
        assert!(n.is_empty(), "{n:?}");
        let (t, _) = discover(
            &root,
            "config2",
            &["lf", "--config=cfg"],
            Some(&env(&[])),
            &none,
        );
        assert_eq!(a(&t), "cfg");
        let (t, _) = discover(
            &root,
            "single",
            &["lf", "-single", "-config", "cfg"],
            Some(&env(&[])),
            &none,
        );
        assert_eq!(a(&t), "cfg");
        // A non-flag ends the flags: no -config.
        let (t, _) = discover(
            &root,
            "nonflag",
            &["lf", "dir", "-config", "cfg"],
            Some(&env(&[])),
            &none,
        );
        assert_eq!(a(&t), "home");
        // -command after the files, in order; `source ~/…` from the home.
        let (t, _) = discover(
            &root,
            "command",
            &[
                "lf",
                "-command",
                "map a one",
                "-command",
                "map a two; source ~/src.lfrc",
            ],
            Some(&env(&[])),
            &none,
        );
        assert_eq!((a(&t), t.normal["t"].as_str()), ("two".into(), "tilde"));
        // An unreadable environ: keybind-find's own.
        let own_home = format!("{r}/home");
        let own = move |k: &str| (k == "XDG_CONFIG_HOME").then(|| format!("{own_home}/../xdg"));
        let (t, _) = discover(&root, "own", &["lf"], None, &own);
        assert_eq!(a(&t), "xdg");
        // A missing config file is skipped silently; -config naming one reads nothing more.
        let (t, n) = discover(
            &root,
            "missing",
            &["lf", "-config", "nope"],
            Some(&env(&[])),
            &none,
        );
        assert_eq!((t.normal.get("a"), n.len()), (None, 0));
    }

    #[test]
    fn sources_without_a_directory_or_without_end() {
        let root = homes("source");
        let (mut notes, loop_file) = (Vec::new(), root.join("loop.lfrc"));
        std::fs::write(
            &loop_file,
            format!("map l loop\nsource {}", loop_file.display()),
        )
        .unwrap();
        let mut ev = Eval {
            table: Table::default(),
            home: None,
            cwd: None,
            loads: 0,
            notes: &mut notes,
        };
        ev.run(
            &format!(
                "source rel.lfrc\nsource ~/x\nsource {}\nsource /no/such",
                loop_file.display()
            ),
            "t",
            0,
        );
        assert_eq!(ev.table.normal["l"], "loop");
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(notes[0].contains("lf's directory is unknown"), "{notes:?}");
        assert!(notes[1].contains("no home directory"), "{notes:?}");
        assert!(notes[2].contains("more than 8 deep"), "{notes:?}");
        assert!(notes[3].starts_with("source /no/such:"), "{notes:?}");
    }

    #[test]
    fn dump_with_no_lf_reads_the_users_lfrc() {
        let root = homes("dump");
        let home = format!("{}/home", root.display());
        let env = move |k: &str| (k == "HOME").then(|| home.clone());
        let (s, notes) = sheet_with(None, &root, "/nonexistent/lf", &root.join("etc/lfrc"), &env);
        assert_eq!(desc(&s, "a", "normal,visual"), "home");
        assert_eq!(left_out(&notes), 5, "{notes:?}");
    }

    #[test]
    fn every_bind_is_described_by_its_command() {
        let example = fixture("lfrc.example");
        let (s, _) = sheet_of(&[DEFAULTS, &example, "map b %echo b\n"]);
        assert!(s.binds.values().flatten().all(|b| b.action == b.desc));
        for (key, cat) in [
            ("grave", "shell"),
            ("o", "shell"),
            ("b", "shell"),
            ("x", "shell"),
            ("a", "list"),
        ] {
            assert_eq!(find(&s, key, "normal,visual")[0].0, cat, "{key}");
        }
        // Exactly the pointer keys and the command line's <space> are left out of the defaults.
        let (t, _) = eval(&[DEFAULTS]);
        let (s, _) = finish(&t, Vec::new());
        let listed: std::collections::BTreeSet<(String, String)> = s
            .binds
            .values()
            .flatten()
            .map(|b| (b.key.clone(), modes(b)))
            .collect();
        let mut out = Vec::new();
        for (mode, table) in [("n", &t.normal), ("v", &t.visual), ("c", &t.command)] {
            for k in table.keys() {
                let chord = notation(k);
                let shown = chord.as_ref().is_some_and(|c| {
                    listed.iter().any(|(key, m)| {
                        key == c
                            && m.contains(match mode {
                                "n" => "normal",
                                "v" => "visual",
                                _ => "command-line",
                            })
                    })
                });
                if !shown {
                    out.push(format!("{mode} {k}"));
                }
            }
        }
        assert_eq!(
            out,
            [
                "n <c-m-down>",
                "n <c-m-up>",
                "n <m-down>",
                "n <m-up>",
                "v <c-m-down>",
                "v <c-m-up>",
                "v <m-down>",
                "v <m-up>",
                "c <space>"
            ]
        );
    }

    #[test]
    fn more_lfrc_forms() {
        // A `:` list without braces runs to the line end.
        let (t, notes) = eval(&[": nmap a up; vmap b down\nmap z top\n"]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            (
                t.normal["a"].as_str(),
                t.visual["b"].as_str(),
                t.normal["z"].as_str()
            ),
            ("up", "down", "top")
        );
        // Every escape lf's double quotes read.
        assert_eq!(
            parse_one(r#"map c echo "\\\a\b\f\n\r\v\x""#),
            Expr::Map {
                kind: "map",
                keys: "c".into(),
                expr: call("echo", &["\\\x07\x08\x0c\n\r\x0b\\x"])
            }
        );
    }

    #[test]
    fn more_key_names() {
        for (lf, want) in [
            ("<tab>", "Tab"),
            ("<delete>", "Delete"),
            ("<insert>", "Insert"),
            ("<home>", "Home"),
            ("<end>", "End"),
            ("<clear>", "Clear"),
            ("<pause>", "Pause"),
            ("<print>", "Print"),
            ("<enter>", "Return"),
            ("<esc>", "Escape"),
            ("<space>", "space"),
            ("<s-tab>", "shift+Tab"),
            ("<c-space>", "ctrl+space"),
        ] {
            assert_eq!(notation(lf).as_deref(), Some(want), "{lf}");
        }
        // <esc> with a modifier after the first key cancels too (ui.go:1394 checks the key, not its modifiers).
        let (s, notes) = sheet_of(&["map g<a-esc> up\nmap h up\n"]);
        assert_eq!((count(&s), left_out(&notes)), (1, 1));
    }

    #[test]
    fn an_unknown_mode_letter_falls_back() {
        let lf = fake_lf("mode", "printf 'mode\\tkey\\tcommand\\nx\\tk\\tup\\n'");
        let err = live(4242, &lf.display().to_string(), None).unwrap_err();
        assert!(err.to_string().contains("unknown mode"), "{err}");
    }

    #[test]
    fn unreadable_files_and_odd_arguments() {
        let root = homes("odd");
        let r = root.display().to_string();
        let none = |_: &str| None;
        let home = vec![format!("HOME={r}/home")];
        // /etc/lf/lfrc that can't be read (a directory): one note, and the user's lfrc still counts.
        let proc = fake_proc(
            "unreadable",
            &["lf"],
            Some(&[home[0].as_str()]),
            &root.join("cwd"),
        );
        let dir = proc.join("4242");
        let mut notes = Vec::new();
        let t = fallback(
            Some(dir.clone()),
            super::environ(&dir).as_ref(),
            &root,
            &none,
            &mut notes,
        );
        assert_eq!(t.normal["a"], "home");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("can't read"), "{notes:?}");
        // An empty `-config` is none, and the argument after it is still a flag.
        let (t, n) = discover(
            &root,
            "emptycfg",
            &["lf", "-config", "", "-command", "map c x"],
            Some(&home),
            &none,
        );
        assert_eq!(
            (t.normal["a"].as_str(), t.normal["c"].as_str(), n.len()),
            ("home", "x", 0)
        );
        // A relative config home is relative to lf's directory.
        let env = vec![home[0].clone(), "XDG_CONFIG_HOME=../xdg".into()];
        let (t, _) = discover(&root, "relxdg", &["lf"], Some(&env), &none);
        assert_eq!(t.normal["a"], "xdg");
        // lf's environment unreadable: keybind-find's own, with a note.
        let (_, n) = discover(&root, "noenv", &["lf"], None, &none);
        assert!(
            n.iter().any(|n| n.contains("can't read lf's environment")),
            "{n:?}"
        );
    }

    #[test]
    fn at_most_64_sourced_files() {
        let root = homes("loads");
        let leaf = root.join("leaf.lfrc");
        std::fs::write(&leaf, "map l leaf").unwrap();
        let text = format!("source {}\n", leaf.display()).repeat(70);
        let mut notes = Vec::new();
        let mut ev = Eval {
            table: Table::default(),
            home: None,
            cwd: None,
            loads: 0,
            notes: &mut notes,
        };
        ev.run(&text, "t", 0);
        assert_eq!(ev.loads, 64);
        assert_eq!(notes.len(), 6, "{notes:?}");
        assert!(notes[0].contains("64 files"), "{notes:?}");
    }
}
