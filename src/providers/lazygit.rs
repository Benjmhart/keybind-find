//! lazygit: the rows of its keybindings cheatsheet by panel, pinned per version (`data/lazygit/`), with the keys the
//! user's config files set (the global ones, then the repo's) applied as lazygit applies them. Shown when lazygit is
//! the program in the focused terminal. Custom commands aren't listed; nothing in a config is run.

use super::nvim::single;
use super::pinned::{self, parse_version};
use super::yaml_subset::{self, Node};
use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const REGISTRY: &[(&str, &str)] = &[(
    "0.66.2",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/lazygit/0.66.2.toml"
    )),
)];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
const GIT_TIMEOUT: Duration = Duration::from_secs(1);

/// A config file bigger than this isn't read.
const MAX_SIZE: u64 = 4 << 20;

/// `MergeLegacyAltKeybindings` (pkg/config/user_config.go:851-867): each legacy field's keys are added to its main
/// field's, all under `universal`.
const ALT: &[(&str, &str)] = &[
    ("quit", "quit-alt1"),
    ("prevItem", "prevItem-alt"),
    ("nextItem", "nextItem-alt"),
    ("gotoTop", "gotoTop-alt"),
    ("gotoBottom", "gotoBottom-alt"),
    ("prevBlock", "prevBlock-alt"),
    ("nextBlock", "nextBlock-alt"),
    ("prevBlock", "prevBlock-alt2"),
    ("nextBlock", "nextBlock-alt2"),
    ("confirmInEditor", "confirmInEditor-alt"),
    ("scrollUpMain", "scrollUpMain-alt1"),
    ("scrollUpMain", "scrollUpMain-alt2"),
    ("scrollDownMain", "scrollDownMain-alt1"),
    ("scrollDownMain", "scrollDownMain-alt2"),
    ("diffingMenu", "diffingMenu-alt"),
];

/// A list of key lists, read by lazygit but in no cheatsheet row: read and ignored.
const JUMP_TO_BLOCK: &str = "universal.jumpToBlock";

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Each keybinding field (`universal.quit`) and its key labels.
type Fields = BTreeMap<String, Vec<String>>;

#[derive(Deserialize)]
struct Pinned {
    defaults: Fields,
    row: Vec<Row>,
}

/// One cheatsheet row: the keys of `field`, or its own `keys` when no field sets them.
#[derive(Deserialize)]
struct Row {
    panel: String,
    action: String,
    field: Option<String>,
    keys: Option<Vec<String>>,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let (sheet, notes) = sheet_with(
        REGISTRY,
        |_| {},
        crate::context::editor_pid(),
        Path::new("/proc"),
        std::env::current_dir().ok().as_deref(),
        &|k| std::env::var(k).ok(),
    )?;
    for note in notes {
        eprintln!("keybind-find: lazygit: {note}");
    }
    Ok(sheet)
}

/// The sheet for the lazygit at `pid` (none: `dump lazygit` from a shell), its process read under `proc`; `setup` is
/// applied to `lazygit --version`, `here` and `env` are keybind-find's own cwd and environment (test seams). Also the
/// notes.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    pid: Option<u32>,
    proc: &Path,
    here: Option<&Path>,
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    // The editor pid context found is lazygit's only when lazygit leads the terminal; check its comm.
    let dir = pid
        .filter(|p| {
            std::fs::read_to_string(proc.join(format!("{p}/comm")))
                .is_ok_and(|c| c.trim_end_matches('\n') == "lazygit")
        })
        .map(|p| proc.join(p.to_string()));
    // The focused lazygit's own binary (`/proc/<pid>/exe`, which runs even when deleted or replaced since), else
    // `lazygit` from PATH (`dump lazygit`, or an exe that can't be read or run).
    let ask = |program: &Path| {
        let mut cmd = Command::new(program);
        cmd.arg("--version")
            .env("LC_ALL", "C")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
        setup(&mut cmd);
        run_for(cmd, VERSION_TIMEOUT)
    };
    let version = match dir.as_ref().map(|d| ask(&d.join("exe"))) {
        Some(Ok(v)) => v,
        _ => ask(Path::new("lazygit")).context("running lazygit --version")?,
    };
    let (v, text, note) = pinned::select("lazygit", registry, version_of(&version).as_deref())
        .context("no pinned lazygit keybindings")?;
    let pin: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/lazygit/{v}.toml"))?;
    let mut notes: Vec<String> = note.into_iter().collect();
    let environ = dir.as_deref().and_then(environ);
    if dir.is_some() && environ.is_none() {
        notes.push("can't read lazygit's environment; using keybind-find's".into());
    }
    let args: Vec<String> = dir
        .as_ref()
        .and_then(|d| std::fs::read(d.join("cmdline")).ok())
        .map(|b| nul_split(&b))
        .unwrap_or_default();
    let cwd = match &dir {
        Some(d) => {
            let cwd = std::fs::read_link(d.join("cwd")).ok();
            if cwd.is_none() {
                notes.push(
                    "can't read lazygit's working directory; skipping relative config paths and the repo's files"
                        .into(),
                );
            }
            cwd
        }
        None => here.map(Path::to_path_buf),
    };
    let getenv = |k: &str| match &environ {
        Some(e) => e.get(k).cloned(),
        None => env(k),
    };
    let mut files = global_files(&getenv, args.get(1..).unwrap_or_default(), cwd.as_deref());
    if let Some(cwd) = &cwd {
        files.extend(
            repo_files(cwd, environ.as_ref())
                .into_iter()
                .map(|f| (f, false)),
        );
    }
    let fields = merged(v, &pin.defaults, &files, &mut notes);
    let (sheet, left_out) = rows(&pin.row, &fields);
    if left_out > 0 {
        let keys = if left_out == 1 { "key" } else { "keys" };
        notes.push(format!(
            "leaving out {left_out} {keys} that can't be typed here (meta, the mouse, or no keysym we know)"
        ));
    }
    Ok((sheet, notes))
}

/// The `version=` field of `lazygit --version` (`commit=…, …, version=0.66.2, os=linux, …, git version=…`).
fn version_of(text: &str) -> Option<String> {
    text.trim()
        .split(", ")
        .find_map(|f| f.strip_prefix("version="))
        .filter(|v| parse_version(v).is_some())
        .map(str::to_string)
}

type Environ = BTreeMap<String, String>;

/// A process's environment (`/proc/<pid>/environ`), `None` when it can't be read or reads empty (a zombie's).
fn environ(dir: &Path) -> Option<Environ> {
    let b = std::fs::read(dir.join("environ"))
        .ok()
        .filter(|b| !b.is_empty())?;
    let mut env = BTreeMap::new();
    for kv in nul_split(&b) {
        if let Some((k, v)) = kv.split_once('=') {
            // Go's `os.Getenv` takes the first of a name given twice.
            env.entry(k.into()).or_insert_with(|| v.into());
        }
    }
    Some(env)
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

/// `--use-config-file`/`-ucf` and `--use-config-dir`/`-ucd` from lazygit's arguments (flaggy: one or two dashes, the
/// value after `=` or next; the last wins; nothing after `--`).
/// ponytail: other flags' values aren't skipped, so a `-p` path spelled `-ucf` would count; flaggy knows them.
fn flags(args: &[String]) -> (Option<String>, Option<String>) {
    let (mut ucf, mut ucd) = (None, None);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
        let Some(name) = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) else {
            continue;
        };
        let (name, value) = match name.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (name, None),
        };
        let slot = match name {
            "ucf" | "use-config-file" => &mut ucf,
            "ucd" | "use-config-dir" => &mut ucd,
            _ => continue,
        };
        match value.or_else(|| it.next().cloned()) {
            Some(v) => *slot = Some(v),
            None => break,
        }
    }
    (ucf, ucd)
}

/// lazygit's global config files (`NewAppConfig`, `findConfigFile`), each with whether a missing one gets a note:
/// `LG_CONFIG_FILE`'s list, else `$CONFIG_DIR/config.yml`, else the first `jesseduffield/lazygit/config.yml` then the
/// first `lazygit/config.yml` in the config home and dirs (adrg/xdg). `env` is lazygit's environment; `-ucf` and
/// `-ucd` set those variables first.
fn global_files(env: Env, args: &[String], cwd: Option<&Path>) -> Vec<(PathBuf, bool)> {
    let (ucf, ucd) = flags(args);
    let get = |k: &str, flag: Option<String>| {
        flag.filter(|v| !v.is_empty())
            .or_else(|| env(k))
            .filter(|v| !v.is_empty())
    };
    // A relative path is lazygit's cwd's; none when that can't be read.
    let at_cwd = |p: &str| match cwd {
        _ if Path::new(p).is_absolute() => Some(PathBuf::from(p)),
        c => c.map(|c| c.join(p)),
    };
    if let Some(list) = get("LG_CONFIG_FILE", ucf) {
        return list
            .split(',')
            .filter_map(|p| Some((at_cwd(p)?, true)))
            .collect();
    }
    if let Some(dir) = get("CONFIG_DIR", ucd) {
        return at_cwd(&dir)
            .map(|d| (d.join("config.yml"), false))
            .into_iter()
            .collect();
    }
    let search = config_dirs(env);
    ["jesseduffield/lazygit/config.yml", "lazygit/config.yml"]
        .iter()
        .find_map(|rel| search.iter().map(|d| d.join(rel)).find(|f| f.exists()))
        .map(|f| vec![(f, false)])
        .unwrap_or_default()
}

/// adrg/xdg's config search path: the config home, then the config dirs, deduplicated. Its `ExpandHome`
/// (pathutil_unix.go): a leading `~` or `$HOME` is `$HOME` (`/` when unset); only an absolute result counts.
fn config_dirs(env: Env) -> Vec<PathBuf> {
    let home = env("HOME")
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "/".into());
    let expand = |p: &str| {
        let p = match p.strip_prefix('~').or_else(|| p.strip_prefix("$HOME")) {
            Some(rest) => Path::new(&home).join(rest.trim_start_matches('/')),
            None => PathBuf::from(p),
        };
        Some(p).filter(|p| p.is_absolute())
    };
    let config_home = env("XDG_CONFIG_HOME")
        .filter(|h| !h.is_empty())
        .and_then(|h| expand(&h))
        .or_else(|| expand(&format!("{home}/.config")));
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in env("XDG_CONFIG_DIRS").unwrap_or_default().split(':') {
        if let Some(d) = expand(d).filter(|d| !d.as_os_str().is_empty() && !dirs.contains(d)) {
            dirs.push(d);
        }
    }
    if dirs.is_empty() {
        dirs.push("/etc/xdg".into());
    }
    config_home.into_iter().chain(dirs).collect()
}

/// The repo's config files (`getPerRepoConfigFiles`, pkg/gui/gui.go:483-504), asked of git in `cwd`; none when git
/// fails (not a repo, a bare one, no git). No `GIT_*` of keybind-find's reaches git; lazygit's own `GIT_DIR` and
/// `GIT_WORK_TREE` (from `environ`) do when both are absolute and `cwd` is in that work tree: lazygit sets and clears
/// them as it switches repos (git.go:87, repos_helper.go:384-389), so others are stale.
fn repo_files(cwd: &Path, environ: Option<&Environ>) -> Vec<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.args([
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--absolute-git-dir",
        "--git-common-dir",
        "--show-superproject-working-tree",
    ])
    .current_dir(cwd)
    .env("LC_ALL", "C")
    .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    for (k, _) in std::env::vars_os() {
        if k.as_encoded_bytes().starts_with(b"GIT_") {
            cmd.env_remove(k);
        }
    }
    if let Some((d, w)) = environ.and_then(|e| Some((e.get("GIT_DIR")?, e.get("GIT_WORK_TREE")?))) {
        if Path::new(d).is_absolute() && Path::new(w).is_absolute() && cwd.starts_with(w) {
            cmd.env("GIT_DIR", d).env("GIT_WORK_TREE", w);
        }
    }
    run_for(cmd, GIT_TIMEOUT)
        .map(|out| per_repo(&out))
        .unwrap_or_default()
}

/// The files from `git rev-parse`'s answer (`repoPathsForDir`): the repo is the worktree for the main worktree or a
/// submodule (a fourth line), else the common git dir's parent; then `.lazygit.yml` in each of its ancestors, the
/// outermost first, and `<common git dir>/lazygit.yml`.
fn per_repo(out: &str) -> Vec<PathBuf> {
    let lines: Vec<&str> = out.trim().split('\n').collect();
    let [top, git_dir, common, ..] = lines[..] else {
        return Vec::new();
    };
    let repo = if git_dir == common || lines.len() == 4 {
        Path::new(top)
    } else {
        Path::new(common).parent().unwrap_or(Path::new(common))
    };
    let mut files: Vec<PathBuf> = repo
        .ancestors()
        .skip(1)
        .map(|d| d.join(".lazygit.yml"))
        .collect();
    files.reverse();
    files.push(Path::new(common).join("lazygit.yml"));
    files
}

/// A file's bytes; `None` when it doesn't exist; an error when it can't be read, isn't a regular file or is over
/// `MAX_SIZE`. Opened non-blocking and checked on the handle, so a FIFO put there can't hang us.
fn read_file(p: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    // ponytail: Linux's O_NONBLOCK on x86 and arm (libc isn't a dependency); alpha, mips, parisc and sparc differ.
    const O_NONBLOCK: i32 = 0o4000;
    let f = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(p)
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
    Ok(Some(bytes))
}

/// The defaults with each file's fields over them in order (`loadUserConfig`), then the legacy alt fields merged in.
fn merged(
    v: &str,
    defaults: &Fields,
    files: &[(PathBuf, bool)],
    notes: &mut Vec<String>,
) -> Fields {
    let mut fields = defaults.clone();
    for (path, note_missing) in files {
        let p = path.display();
        let text = match read_file(path) {
            Ok(Some(t)) => t,
            Ok(None) if *note_missing => {
                notes.push(format!("{p} doesn't exist; skipped"));
                continue;
            }
            Ok(None) => continue,
            Err(e) => {
                notes.push(format!("can't read {p}: {e}; skipped"));
                continue;
            }
        };
        match user_fields(&text, defaults) {
            Ok((set, unknown, refused)) => {
                if !unknown.is_empty() {
                    notes.push(format!(
                        "{p}: {} isn't a keybinding of lazygit {v}; ignored, as lazygit ignores it",
                        unknown.join(", ")
                    ));
                }
                if !refused.is_empty() {
                    notes.push(format!(
                        "{p}: lazygit refuses the key {} (and with it its whole config); this file's other keys are listed as written",
                        refused.join(", ")
                    ));
                }
                fields.extend(set);
            }
            Err(e) => notes.push(format!("{p}: {e:#}; its keybindings are ignored")),
        }
    }
    for (main, alt) in ALT {
        let extra = fields
            .get(&format!("universal.{alt}"))
            .cloned()
            .unwrap_or_default();
        let keys = fields.entry(format!("universal.{main}")).or_default();
        for k in extra {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    fields
}

/// What a file sets: each field it names with its keys; also the sections and fields lazygit doesn't have, and the
/// labels lazygit refuses. An error refuses the whole file, as yaml.v3 does.
type UserFields = (Vec<(String, Vec<String>)>, Vec<String>, Vec<String>);

fn user_fields(text: &[u8], defaults: &Fields) -> anyhow::Result<UserFields> {
    let (mut set, mut unknown, mut refused) = (Vec::new(), Vec::new(), Vec::new());
    let sections = match yaml_subset::keybinding_of(text)? {
        None => return Ok((set, unknown, refused)),
        // `keybinding:` left null: lazygit's migration walks into it and fails (computeMigratedConfig,
        // app_config.go:304-308).
        Some(Node::Null) => bail!("`keybinding` is empty (null), which lazygit refuses"),
        Some(Node::Map(m)) => m,
        Some(_) => bail!("`keybinding` isn't a mapping"),
    };
    for (section, value, line) in sections {
        let prefix = format!("{section}.");
        if !defaults.keys().any(|k| k.starts_with(&prefix)) {
            unknown.push(section);
            continue;
        }
        let entries = match value {
            // lazygit turns a null under `keybinding.` into `<disabled>` (`changeNullKeybindingsToDisabled`,
            // app_config.go:398-405), a string, which yaml.v3 can't decode into the section: it refuses the config.
            Node::Null => bail!("line {line}: `{section}` is empty (null), which lazygit refuses"),
            Node::Map(m) => m,
            _ => bail!("line {line}: `{section}` isn't a mapping"),
        };
        for (f, value, line) in entries {
            let name = format!("{prefix}{f}");
            // jumpToBlock (`[]Keybinding`, not on the sheet) is checked, not kept: a list whose items are key lists.
            let jump = name == JUMP_TO_BLOCK;
            if !jump && !defaults.contains_key(&name) {
                unknown.push(name);
                continue;
            }
            let labels = match value {
                Node::Scalar(s) if !jump => vec![s],
                // lazygit turns null into `<disabled>` (migrateUserConfig).
                Node::Null if !jump => vec![],
                Node::Seq(items) => {
                    let mut labels = Vec::new();
                    for i in items {
                        match i {
                            Node::Scalar(s) => labels.push(s),
                            Node::Null => {}
                            Node::Seq(inner) if jump => {
                                for x in inner {
                                    match x {
                                        Node::Scalar(s) => labels.push(s),
                                        Node::Null => {}
                                        _ => bail!("line {line}: a list nested too deep ({name})"),
                                    }
                                }
                            }
                            _ => bail!("line {line}: a list inside a key list ({name})"),
                        }
                    }
                    labels
                }
                Node::Map(_) => bail!("line {line}: a mapping where a key list belongs ({name})"),
                _ => bail!("line {line}: `{name}` isn't a list, which lazygit refuses"),
            };
            let labels: Vec<String> = labels
                .into_iter()
                .filter(|l| !l.is_empty() && l != "<disabled>")
                .collect();
            refused.extend(labels.iter().filter(|l| label(l).is_none()).cloned());
            if !jump {
                set.push((name, labels));
            }
        }
    }
    Ok((set, unknown, refused))
}

/// The sheet: each row's keys (its field's, or its own) under its panel, one bind per key, a key given twice once.
/// Also how many keys were left out.
fn rows(rows: &[Row], fields: &Fields) -> (Sheet, usize) {
    let mut sheet = Sheet {
        title: "lazygit".into(),
        provider: "lazygit".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for r in rows {
        let (labels, action) = match (&r.field, &r.keys) {
            (Some(f), _) => (fields.get(f).cloned().unwrap_or_default(), f.clone()),
            (None, Some(k)) => (k.clone(), String::new()),
            (None, None) => continue,
        };
        let mut seen: Vec<String> = Vec::new();
        for l in labels {
            let Some(key) = notation(&l) else {
                left_out += 1;
                continue;
            };
            if seen.contains(&key) {
                continue;
            }
            seen.push(key.clone());
            sheet.binds.entry(r.panel.clone()).or_default().push(Bind {
                key,
                desc: r.action.clone(),
                action: action.clone(),
                when: Default::default(),
                run: Vec::new(),
            });
        }
    }
    (sheet, left_out)
}

/// A key as lazygit reads a label: its modifiers and what is pressed.
#[derive(Debug, PartialEq)]
enum Base {
    /// A named key's keysym (`Page_Up`); `backtab` is shift and `Tab`.
    Name(&'static str),
    Rune(char),
    Mouse,
}

/// lazygit's `KeyFromLabel` (pkg/config/keynames.go:95-193); `None` when lazygit refuses the label.
fn label(label: &str) -> Option<(Vec<&'static str>, Base)> {
    let mut l = label
        .strip_prefix('<')
        .and_then(|l| l.strip_suffix('>'))
        .unwrap_or(label);
    let mut mods: Vec<&'static str> = Vec::new();
    loop {
        // A bare `-` or `+` is the character, with whatever modifiers came before.
        if let "-" | "+" = l {
            return Some((mods, Base::Rune(l.chars().next()?)));
        }
        let Some(i) = l.find(['-', '+']) else {
            break;
        };
        let m = match &l[..i] {
            "s" | "shift" => "shift",
            "c" | "ctrl" => "ctrl",
            "a" | "alt" => "alt",
            "m" | "meta" => "meta",
            _ => return None,
        };
        if mods.contains(&m) {
            return None;
        }
        mods.push(m);
        l = &l[i + 1..];
    }
    let shift_alone = mods == ["shift"];
    let base = match l {
        "space" => Base::Rune(' '),
        "minus" | "plus" if shift_alone => return None,
        "minus" => Base::Rune('-'),
        "plus" => Base::Rune('+'),
        "insert" => Base::Name("Insert"),
        "delete" => Base::Name("Delete"),
        "home" => Base::Name("Home"),
        "end" => Base::Name("End"),
        "pgup" => Base::Name("Page_Up"),
        "pgdown" => Base::Name("Page_Down"),
        "up" => Base::Name("Up"),
        "down" => Base::Name("Down"),
        "left" => Base::Name("Left"),
        "right" => Base::Name("Right"),
        "tab" => Base::Name("Tab"),
        "backtab" => Base::Name("backtab"),
        "enter" => Base::Name("Return"),
        "esc" => Base::Name("Escape"),
        "backspace" => Base::Name("BackSpace"),
        "mouse wheel up" | "mouse wheel down" => Base::Mouse,
        _ => {
            const F: [&str; 12] = [
                "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
            ];
            if let Some(f) = F
                .iter()
                .find(|f| f.eq_ignore_ascii_case(l) && l.starts_with('f'))
            {
                Base::Name(f)
            } else {
                let mut chars = l.chars();
                let (Some(c), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                // Shift alone on a character, or a modifier with an upper-case letter: lazygit refuses both.
                if shift_alone || (!mods.is_empty() && c.is_ascii_uppercase()) {
                    return None;
                }
                Base::Rune(c)
            }
        }
    };
    Some((mods, base))
}

/// A lazygit label in keybind-find's notation; `None` when it is left out: refused by lazygit, meta (the terminal
/// decides what it is), the mouse, or a character with no keysym.
fn notation(l: &str) -> Option<String> {
    let (mut mods, base) = label(l)?;
    if mods.contains(&"meta") {
        return None;
    }
    let key = match base {
        Base::Mouse => return None,
        Base::Name("backtab") => {
            mods.push("shift");
            "Tab"
        }
        Base::Name(k) => k,
        Base::Rune(c) if c.is_ascii_uppercase() => {
            mods.push("shift");
            single(c.to_ascii_lowercase())?
        }
        Base::Rune(c) => single(c)?,
    };
    Some(chord(&mods, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lazygit")
    }

    fn pin() -> Pinned {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-lazygit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The sheet with `texts` as the user's files, in order; also the notes and the left-out count.
    fn with_files(name: &str, texts: &[&str]) -> (Sheet, Vec<String>, usize) {
        let dir = tmp(name);
        let files: Vec<(PathBuf, bool)> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let f = dir.join(format!("{i}.yml"));
                std::fs::write(&f, t).unwrap();
                (f, true)
            })
            .collect();
        let pin = pin();
        let mut notes = Vec::new();
        let fields = merged("0.66.2", &pin.defaults, &files, &mut notes);
        let (s, left_out) = rows(&pin.row, &fields);
        (s, notes, left_out)
    }

    fn keys<'a>(s: &'a Sheet, panel: &str, desc: &str) -> Vec<&'a str> {
        s.binds
            .get(panel)
            .map(|b| {
                b.iter()
                    .filter(|b| b.desc == desc)
                    .map(|b| b.key.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn triples(s: &Sheet) -> Vec<(String, String, String)> {
        let mut t: Vec<_> = s
            .binds
            .iter()
            .flat_map(|(c, b)| {
                b.iter()
                    .map(move |b| (c.clone(), b.key.clone(), b.desc.clone()))
            })
            .collect();
        t.sort();
        t
    }

    #[test]
    fn the_default_sheet_is_the_cheatsheet() {
        let text = REGISTRY[0].1;
        assert!(
            text.contains(
                "tag v0.66.2 (commit 63ab89fff478854deb4a35ef8902676b3cc5ca72), MIT licensed"
            ),
            "{}",
            text.lines().nth(1).unwrap()
        );
        // The cheatsheet's rows, read here (not by the provider): section, keys less the macOS `(…)` hint, action.
        let md = std::fs::read_to_string(fixtures().join("Keybindings_en.md")).unwrap();
        let mut section = String::new();
        let mut want = Vec::new();
        let (mut row_count, mut left) = (0, 0);
        for line in md.lines() {
            if let Some(s) = line.strip_prefix("## ") {
                section = s.to_string();
                continue;
            }
            let Some(rest) = line.strip_prefix("| `` ") else {
                continue;
            };
            let (keys, rest) = rest.split_once(" `` | ").unwrap();
            let action = rest.replace("\\|", "\u{0}");
            let action = action.split(" | ").next().unwrap().replace('\u{0}', "|");
            let keys = keys.replace("\\|", "|");
            let keys = match keys.rfind(" (") {
                Some(i) if keys.ends_with(')') => &keys[..i],
                _ => &keys,
            };
            row_count += 1;
            for k in keys.split(", ") {
                match notation(k) {
                    Some(k) => want.push((section.clone(), k, action.clone())),
                    None => left += 1,
                }
            }
        }
        want.sort();
        let pin = pin();
        assert_eq!((row_count, pin.row.len()), (301, 301));
        assert_eq!(pin.row.iter().filter(|r| r.keys.is_some()).count(), 3);
        let (s, notes, left_out) = with_files("none", &[]);
        assert!(notes.is_empty());
        assert_eq!(triples(&s), want);
        // Pinned: every row's keys, less the two mouse rows.
        assert_eq!((want.len(), left_out, left), (317, 2, 2));
        assert_eq!(s.binds.len(), 22);
        let q = &s.binds["Global keybindings"]
            .iter()
            .find(|b| b.desc == "Quit")
            .unwrap();
        assert_eq!((q.key.as_str(), q.action.as_str()), ("q", "universal.quit"));
        assert!(q.when.is_empty());
        assert_eq!(keys(&s, "Global keybindings", "Quit"), ["q", "ctrl+c"]);
        let fixed = &s.binds["Input prompt"]
            .iter()
            .find(|b| b.desc == "Confirm")
            .unwrap();
        assert_eq!((fixed.key.as_str(), fixed.action.as_str()), ("Return", ""));
    }

    #[test]
    fn the_documented_defaults_change_nothing() {
        let text = std::fs::read_to_string(fixtures().join("config-defaults.yml")).unwrap();
        assert!(matches!(
            yaml_subset::keybinding(&text),
            Ok(Some(Node::Map(_)))
        ));
        let (s, notes, left_out) = with_files("documented", &[&text]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(left_out, 2);
        assert_eq!(triples(&s), triples(&with_files("plain", &[]).0));
    }

    #[test]
    fn version() {
        let text = std::fs::read_to_string(fixtures().join("version.txt")).unwrap();
        assert_eq!(version_of(&text).as_deref(), Some("0.66.2"));
        assert_eq!(
            version_of("commit=, build date=, build source=unknown, version=unversioned, os=linux, arch=amd64, git version=2.56.0"),
            None
        );
        assert_eq!(version_of("lazygit: command not found"), None);
        assert_eq!(version_of(""), None);
        assert_eq!(
            pinned::select("lazygit", REGISTRY, None)
                .unwrap()
                .2
                .as_deref(),
            Some("lazygit's version is unknown: using the defaults of 0.66.2")
        );
    }

    #[test]
    fn real_lazygit_version() {
        let mut cmd = Command::new("lazygit");
        cmd.arg("--version").env("LC_ALL", "C");
        let Ok(text) = run_for(cmd, VERSION_TIMEOUT) else {
            eprintln!("skipping: lazygit is not installed");
            return;
        };
        let v = version_of(&text).unwrap_or_else(|| panic!("no version in {text:?}"));
        assert!(pinned::select("lazygit", REGISTRY, Some(&v)).is_some());
    }

    #[test]
    fn merge_as_lazygit() {
        let (s, notes, _) = with_files("quit", &["keybinding:\n  universal:\n    quit: X\n"]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
        let (s, _, _) = with_files(
            "alt-off",
            &["keybinding:\n  universal:\n    quit: X\n    quit-alt1: <disabled>\n"],
        );
        assert_eq!(keys(&s, "Global keybindings", "Quit"), ["shift+x"]);
        let (s, _, _) = with_files(
            "next",
            &["keybinding:\n  universal:\n    nextItem: [<down>, j]\n"],
        );
        assert_eq!(keys(&s, "Main panel (merging)", "Next hunk"), ["Down", "j"]);
        // A field set in two files: the later wins.
        let (s, _, _) = with_files(
            "two",
            &[
                "keybinding:\n  universal:\n    quit: X\n",
                "keybinding:\n  universal:\n    quit: Y\n",
            ],
        );
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+y", "ctrl+c"]
        );
        // `edit` disabled or null: its rows go in every panel.
        let edit_rows = |s: &Sheet| {
            s.binds
                .values()
                .flatten()
                .filter(|b| b.action == "universal.edit")
                .count()
        };
        assert!(edit_rows(&with_files("plain", &[]).0) > 3);
        for value in ["<disabled>", "", "null", "~", "[]", "['', <disabled>]"] {
            let (s, notes, _) = with_files(
                "edit",
                &[&format!("keybinding:\n  universal:\n    edit: {value}\n")],
            );
            assert!(notes.is_empty(), "{notes:?}");
            assert_eq!(edit_rows(&s), 0, "{value}");
        }
        // Unknown sections and fields: one note per file, the rest applied.
        let (s, notes, _) = with_files(
            "unknown",
            &["keybinding:\n  nosuch:\n    x: y\n  universal:\n    executeCustomCommand: x\n    quit: X\n    jumpToBlock: ['1', '2']\n"],
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with("0.yml: nosuch, universal.executeCustomCommand isn't a keybinding of lazygit 0.66.2; ignored, as lazygit ignores it"),
            "{notes:?}"
        );
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
        // A file the reader refuses: its note, its keys ignored, the next file applied.
        let (s, notes, _) = with_files(
            "refused",
            &[
                "keybinding:\n  universal:\n    quit: X\n    edit: &a e\n",
                "keybinding:\n  universal:\n    return: Z\n",
            ],
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with("0.yml: line 4: an anchor (`&`); its keybindings are ignored"),
            "{notes:?}"
        );
        assert_eq!(keys(&s, "Global keybindings", "Quit"), ["q", "ctrl+c"]);
        assert_eq!(keys(&s, "Global keybindings", "Cancel"), ["shift+z"]);
        // A mapping where keys belong, a section that isn't a mapping.
        for (text, says) in [
            (
                "keybinding:\n  universal:\n    quit:\n      a: b\n",
                "line 3: a mapping where a key list belongs (universal.quit)",
            ),
            (
                "keybinding:\n  universal: q\n",
                "line 2: `universal` isn't a mapping",
            ),
            ("keybinding:\n  - a\n", "`keybinding` isn't a mapping"),
        ] {
            let (_, notes, _) = with_files("bad", &[text]);
            assert!(notes[0].contains(says), "{notes:?}");
        }
        // Other entries, a missing file.
        let (_, notes, _) = with_files("other", &["gui:\n  x: |\n    y\n"]);
        assert!(notes.is_empty(), "{notes:?}");
        let mut notes = Vec::new();
        let missing = [(PathBuf::from("/nonexistent-kbf/x.yml"), true)];
        merged("0.66.2", &pin().defaults, &missing, &mut notes);
        assert_eq!(notes, ["/nonexistent-kbf/x.yml doesn't exist; skipped"]);
    }

    #[test]
    fn keys_in_our_notation() {
        for (l, want) in [
            ("q", Some("q")),
            ("Q", Some("shift+q")),
            ("<c-r>", Some("ctrl+r")),
            ("<ctrl+r>", Some("ctrl+r")),
            ("<c-s-a>", Some("ctrl+shift+a")),
            ("<a-down>", Some("alt+Down")),
            ("<alt+shift+c>", Some("alt+shift+c")),
            ("<s-tab>", Some("shift+Tab")),
            ("<backtab>", Some("shift+Tab")),
            ("<pgup>", Some("Page_Up")),
            ("<pgdown>", Some("Page_Down")),
            ("<f12>", Some("F12")),
            ("<f1>", Some("F1")),
            ("<c-minus>", Some("ctrl+minus")),
            ("<c-->", Some("ctrl+minus")),
            ("-", Some("minus")),
            ("+", Some("plus")),
            ("`", Some("grave")),
            ("|", Some("bar")),
            ("<space>", Some("space")),
            ("<s-space>", Some("shift+space")),
            ("<enter>", Some("Return")),
            ("<esc>", Some("Escape")),
            ("<backspace>", Some("BackSpace")),
            ("<ctrl+backspace>", Some("ctrl+BackSpace")),
            ("<insert>", Some("Insert")),
            ("<delete>", Some("Delete")),
            ("<home>", Some("Home")),
            ("<end>", Some("End")),
            ("<up>", Some("Up")),
            ("<left>", Some("Left")),
            ("<right>", Some("Right")),
            ("<tab>", Some("Tab")),
            ("<", Some("less")),
            (">", Some("greater")),
            // Left out: meta, the mouse, no keysym.
            ("<m-j>", None),
            ("<meta+enter>", None),
            ("<mouse wheel up>", None),
            ("é", None),
            // Refused by lazygit.
            ("<c-c-x>", None),
            ("<s-a>", None),
            ("<c-A>", None),
            ("<shift+minus>", None),
            ("<foo>", None),
            ("<Tab>", None),
            ("<f13>", None),
            ("<x-a>", None),
            ("<>", None),
        ] {
            assert_eq!(notation(l).as_deref(), want, "{l}");
        }
        // Refused (lazygit refuses the whole config) apart from left out (read, but not typable here).
        for (l, reads) in [
            ("<m-j>", true),
            ("<mouse wheel down>", true),
            ("é", true),
            ("<c-c-x>", false),
            ("<s-a>", false),
            ("<c-A>", false),
            ("<shift+minus>", false),
            ("<foo>", false),
        ] {
            assert_eq!(label(l).is_some(), reads, "{l}");
        }
        let (s, notes, left_out) = with_files(
            "keys",
            &["keybinding:\n  universal:\n    quit: [<m-q>, <c-c-x>, X]\n"],
        );
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
        assert_eq!(left_out, 4);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].ends_with(
                "0.yml: lazygit refuses the key <c-c-x> (and with it its whole config); this file's other keys are listed as written"
            ),
            "{notes:?}"
        );
        // The same key twice in a row's list: once.
        let (s, _, _) = with_files(
            "twice",
            &["keybinding:\n  universal:\n    quit: [<c-r>, <ctrl+r>]\n"],
        );
        assert_eq!(keys(&s, "Global keybindings", "Quit"), ["ctrl+r", "ctrl+c"]);
    }

    /// An env with `vars` only.
    fn env_of(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
        move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
    }

    #[test]
    fn config_files() {
        let root = tmp("files");
        for f in [
            "cwd/a.yml",
            "abs/b.yml",
            "home/.config/lazygit/config.yml",
            "home/.config/jesseduffield/lazygit/config.yml",
            "xdg/lazygit/config.yml",
            "dirs/lazygit/config.yml",
            "cd/config.yml",
        ] {
            std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
            std::fs::write(root.join(f), "").unwrap();
        }
        let t = |p: &str| root.join(p).to_string_lossy().into_owned();
        let cwd = root.join("cwd");
        let files = |mut vars: Vec<(&'static str, String)>, args: &[&str]| {
            // Never the host's /etc/xdg.
            if !vars.iter().any(|(k, _)| *k == "XDG_CONFIG_DIRS") {
                vars.push(("XDG_CONFIG_DIRS", t("nowhere")));
            }
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            global_files(&env_of(vars), &args, Some(&cwd))
        };
        let home = ("HOME", t("home"));
        // LG_CONFIG_FILE: relative against the cwd, in order, each noted when missing.
        let list = format!("a.yml,{},missing.yml", t("abs/b.yml"));
        assert_eq!(
            files(vec![home.clone(), ("LG_CONFIG_FILE", list.clone())], &[]),
            [
                (cwd.join("a.yml"), true),
                (root.join("abs/b.yml"), true),
                (cwd.join("missing.yml"), true)
            ]
        );
        let mut notes = Vec::new();
        let read = files(vec![("LG_CONFIG_FILE", list)], &[]);
        merged("0.66.2", &pin().defaults, &read, &mut notes);
        assert_eq!(
            notes,
            [format!(
                "{} doesn't exist; skipped",
                cwd.join("missing.yml").display()
            )]
        );
        // The flags, one or two dashes, `=` or the next argument, the last one wins, none after `--`.
        let one = |p: PathBuf| vec![(p, true)];
        assert_eq!(
            files(vec![home.clone()], &["-ucf", "x.yml"]),
            one(cwd.join("x.yml"))
        );
        assert_eq!(
            files(
                vec![home.clone()],
                &["--use-config-file=x.yml", "-ucf=y.yml"]
            ),
            one(cwd.join("y.yml"))
        );
        assert_eq!(
            files(
                vec![home.clone(), ("LG_CONFIG_FILE", "e.yml".into())],
                &["-p", "/r", "--ucf", "x.yml"]
            ),
            one(cwd.join("x.yml"))
        );
        assert_eq!(
            files(vec![home.clone()], &["--", "-ucf", "x.yml"]),
            [(
                root.join("home/.config/jesseduffield/lazygit/config.yml"),
                false
            )]
        );
        assert_eq!(
            files(vec![home.clone()], &["-ucd", &t("cd")]),
            [(root.join("cd/config.yml"), false)]
        );
        assert_eq!(
            files(vec![home.clone(), ("CONFIG_DIR", t("cd"))], &[]),
            [(root.join("cd/config.yml"), false)]
        );
        // `-ucf` beats `-ucd`, as LG_CONFIG_FILE beats CONFIG_DIR.
        assert_eq!(
            files(
                vec![home.clone()],
                &["-ucd", "/d", "-use-config-file", "x.yml"]
            ),
            one(cwd.join("x.yml"))
        );
        // The legacy path wins over lazygit/config.yml, in the same directory.
        assert_eq!(
            files(vec![home.clone()], &[]),
            [(
                root.join("home/.config/jesseduffield/lazygit/config.yml"),
                false
            )]
        );
        // XDG_CONFIG_HOME: relative is ignored, `~` expanded.
        let only = |p: &str| [(root.join(p), false)];
        assert_eq!(
            files(
                vec![("HOME", t("nohome")), ("XDG_CONFIG_HOME", t("xdg"))],
                &[]
            ),
            only("xdg/lazygit/config.yml")
        );
        assert_eq!(
            files(
                vec![("HOME", t("home")), ("XDG_CONFIG_HOME", "rel".into())],
                &[]
            ),
            only("home/.config/jesseduffield/lazygit/config.yml")
        );
        assert_eq!(
            files(
                vec![("HOME", t("")), ("XDG_CONFIG_HOME", "~/xdg".into())],
                &[]
            ),
            only("xdg/lazygit/config.yml")
        );
        // A file only in an XDG_CONFIG_DIRS directory (relative entries dropped); none at all.
        let dirs = format!("rel:{}:{}", t("nowhere"), t("dirs"));
        assert_eq!(
            files(vec![("HOME", t("nohome")), ("XDG_CONFIG_DIRS", dirs)], &[]),
            only("dirs/lazygit/config.yml")
        );
        assert_eq!(files(vec![("HOME", t("nohome"))], &[]), []);
        // The legacy path in a config dir beats lazygit/config.yml in the config home.
        for f in [
            "dirs2/jesseduffield/lazygit/config.yml",
            "home2/.config/lazygit/config.yml",
        ] {
            std::fs::create_dir_all(root.join(f).parent().unwrap()).unwrap();
            std::fs::write(root.join(f), "").unwrap();
        }
        assert_eq!(
            files(
                vec![("HOME", t("home2")), ("XDG_CONFIG_DIRS", t("dirs2"))],
                &[]
            ),
            only("dirs2/jesseduffield/lazygit/config.yml")
        );
    }

    #[test]
    fn repo_files_from_rev_parse() {
        let main = "/home/u/src/r\n/home/u/src/r/.git\n/home/u/src/r/.git\n";
        assert_eq!(
            per_repo(main),
            [
                "/.lazygit.yml",
                "/home/.lazygit.yml",
                "/home/u/.lazygit.yml",
                "/home/u/src/.lazygit.yml",
                "/home/u/src/r/.git/lazygit.yml"
            ]
            .map(PathBuf::from)
        );
        // A linked worktree: the repo is the common git dir's parent.
        let linked = "/w/feature\n/home/u/src/r/.git/worktrees/feature\n/home/u/src/r/.git\n";
        assert_eq!(per_repo(linked), per_repo(main));
        // A submodule (a fourth line): its own worktree.
        // As git prints it: the submodule's git dir is its own common dir.
        let sub = "/home/u/src/r/m\n/home/u/src/r/.git/modules/m\n/home/u/src/r/.git/modules/m\n/home/u/src/r\n";
        assert_eq!(
            per_repo(sub).last(),
            Some(&PathBuf::from("/home/u/src/r/.git/modules/m/lazygit.yml"))
        );
        assert_eq!(
            per_repo(sub).first().map(PathBuf::as_path),
            Some(Path::new("/.lazygit.yml"))
        );
        assert_eq!(per_repo(sub).len(), 6);
        assert_eq!(
            per_repo(sub)[4],
            PathBuf::from("/home/u/src/r/.lazygit.yml")
        );
        // A worktree of a submodule (git dir ≠ common dir, a fourth line): still its own worktree.
        let sub_wt = per_repo("/w/m\n/r/.git/modules/m/worktrees/x\n/r/.git/modules/m\n/r\n");
        assert_eq!(sub_wt[sub_wt.len() - 2], PathBuf::from("/w/.lazygit.yml"));
        assert!(per_repo("").is_empty());
        assert!(per_repo("/a\n/b\n").is_empty());
    }

    #[test]
    fn a_real_repo() {
        let repo = tmp("repo");
        let ok = Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .env("LC_ALL", "C")
            .status();
        if !ok.is_ok_and(|s| s.success()) {
            eprintln!("skipping: git is not installed");
            return;
        }
        let repo = repo.canonicalize().unwrap();
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        // keybind-find's own GIT_* never reach git: this ceiling (only this repo's path) would hide the repo.
        std::env::set_var("GIT_CEILING_DIRECTORIES", &repo);
        let files = repo_files(&repo.join("sub"), None);
        std::env::remove_var("GIT_CEILING_DIRECTORIES");
        assert_eq!(files.last(), Some(&repo.join(".git/lazygit.yml")));
        assert_eq!(
            files[files.len() - 2],
            repo.parent().unwrap().join(".lazygit.yml")
        );
        // The repo's `.git/lazygit.yml` applies after the global files; missing ones are skipped without a word.
        std::fs::write(
            repo.join(".git/lazygit.yml"),
            "keybinding:\n  universal:\n    quit: Z\n",
        )
        .unwrap();
        let files: Vec<(PathBuf, bool)> = files.into_iter().map(|f| (f, false)).collect();
        let mut notes = Vec::new();
        let fields = merged("0.66.2", &pin().defaults, &files, &mut notes);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(fields["universal.quit"], ["Z", "<ctrl+c>"]);
        // A bare repo: none.
        let bare = tmp("bare");
        let ok = Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&bare)
            .status()
            .unwrap();
        assert!(ok.success());
        let bare = bare.canonicalize().unwrap();
        assert!(repo_files(&bare, None).is_empty());
        // lazygit's GIT_DIR and GIT_WORK_TREE count when its cwd is in that work tree: here they name the repo from
        // the bare one's directory; from a cwd outside the work tree they're stale and dropped (the bare repo: none).
        let e: Environ = [
            ("GIT_DIR", repo.join(".git")),
            ("GIT_WORK_TREE", repo.clone()),
        ]
        .map(|(k, v)| (k.to_string(), v.display().to_string()))
        .into();
        assert_eq!(
            repo_files(&repo.join("sub"), Some(&e)).last(),
            Some(&repo.join(".git/lazygit.yml"))
        );
        assert!(repo_files(&bare, Some(&e)).is_empty());
    }

    /// A stand-in `lazygit` printing `out`, in its own directory; returns the directory.
    fn fake_lazygit(name: &str, out: &str) -> PathBuf {
        let bin = tmp(&format!("bin-{name}"));
        // Written by `sh`, not by this process (ETXTBSY: see yazi's `fake_yazi`).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("lazygit"))
            .arg(format!(
                "[ \"$*|$LC_ALL|$MISE_NOT_FOUND_AUTO_INSTALL\" = '--version|C|0' ] || exit 9; printf '{out}'"
            ))
            .status()
            .unwrap();
        assert!(ok.success());
        bin
    }

    /// A stand-in `/proc` with a process at pid 4242 named `comm`, run with `cmdline`, its `environ` and `cwd`.
    fn fake_proc(
        name: &str,
        comm: &str,
        cmdline: &[&str],
        environ: &[&str],
        cwd: &Path,
    ) -> PathBuf {
        let proc = tmp(&format!("proc-{name}"));
        let dir = proc.join("4242");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(dir.join("cmdline"), cmdline.join("\0") + "\0").unwrap();
        std::fs::write(dir.join("environ"), environ.join("\0") + "\0").unwrap();
        std::os::unix::fs::symlink(cwd, dir.join("cwd")).unwrap();
        proc
    }

    #[test]
    fn whole_sheet_from_the_running_lazygit() {
        let path = |bin: &Path| {
            let bin = bin.to_path_buf();
            move |c: &mut Command| {
                c.env("PATH", &bin);
            }
        };
        let bin = fake_lazygit("ok", "commit=x, version=0.66.2, os=linux, git version=2\\n");
        let home = tmp("home");
        std::fs::create_dir_all(home.join(".config/lazygit")).unwrap();
        std::fs::write(
            home.join(".config/lazygit/config.yml"),
            "keybinding:\n  universal:\n    quit: X\n    return: <m-x>\n",
        )
        .unwrap();
        std::fs::write(
            home.join("own.yml"),
            "keybinding:\n  universal:\n    quit: Y\n",
        )
        .unwrap();
        let env_home = format!("HOME={}", home.display());
        // lazygit's own environment and its `-ucf`, relative to its cwd; keybind-find's env isn't used.
        let proc = fake_proc(
            "ucf",
            "lazygit",
            &["lazygit", "-ucf", "own.yml"],
            &[&env_home],
            &home,
        );
        let nothing = |_: &str| None;
        let (s, notes) = sheet_with(
            REGISTRY,
            path(&bin),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &nothing,
        )
        .unwrap();
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+y", "ctrl+c"]
        );
        // The two mouse rows, always.
        assert_eq!(
            notes,
            ["leaving out 2 keys that can't be typed here (meta, the mouse, or no keysym we know)"]
        );
        // Its environment's HOME: the global file; `return`'s meta key is left out in each of its 8 rows.
        let proc = fake_proc("home", "lazygit", &["lazygit"], &[&env_home], &home);
        let (s, notes) = sheet_with(
            REGISTRY,
            path(&bin),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &nothing,
        )
        .unwrap();
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
        assert_eq!(
            notes,
            ["leaving out 10 keys that can't be typed here (meta, the mouse, or no keysym we know)"]
        );
        // A pid that isn't lazygit's: keybind-find's own environment.
        let proc = fake_proc("other", "zsh", &["zsh"], &[&env_home], &home);
        let own = |k: &str| (k == "HOME").then(|| "/nonexistent-kbf".to_string());
        let (s, _) = sheet_with(
            REGISTRY,
            path(&bin),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &own,
        )
        .unwrap();
        assert_eq!(keys(&s, "Global keybindings", "Quit"), ["q", "ctrl+c"]);
        // An unknown version: the newest pin, with its note.
        let bin = fake_lazygit("unversioned", "commit=, version=unversioned, os=linux\\n");
        let (_, notes) = sheet_with(
            REGISTRY,
            path(&bin),
            None,
            &proc,
            Some(&crate::test_tmp()),
            &own,
        )
        .unwrap();
        assert_eq!(
            notes[0],
            "lazygit's version is unknown: using the defaults of 0.66.2"
        );
        // lazygit missing or failing: an error.
        let empty = tmp("nobin");
        let err = sheet_with(
            REGISTRY,
            path(&empty),
            None,
            &proc,
            Some(&crate::test_tmp()),
            &own,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("running lazygit --version"),
            "{err:#}"
        );
        let bin = fake_lazygit("fail", "x\\n'; exit 3; '");
        assert!(sheet_with(
            REGISTRY,
            path(&bin),
            None,
            &proc,
            Some(&crate::test_tmp()),
            &own
        )
        .is_err());
    }

    #[test]
    fn one_key_left_out() {
        const ONE: &str =
            "[defaults]\n[[row]]\npanel = \"P\"\naction = \"A\"\nkeys = [\"<m-x>\", \"x\"]\n";
        let bin = fake_lazygit("one", "version=0.66.2\\n");
        let path = move |c: &mut Command| {
            c.env("PATH", &bin);
        };
        let (_, notes) = sheet_with(
            &[("0.66.2", ONE)],
            path,
            None,
            Path::new("/nonexistent-kbf"),
            Some(&crate::test_tmp()),
            &|_| None,
        )
        .unwrap();
        assert_eq!(
            notes,
            ["leaving out 1 key that can't be typed here (meta, the mouse, or no keysym we know)"]
        );
    }

    #[test]
    fn version_from_the_focused_binary_and_the_repo_files() {
        let path = |bin: &Path| {
            let bin = bin.to_path_buf();
            move |c: &mut Command| {
                c.env("PATH", &bin);
            }
        };
        let on_path = fake_lazygit("path", "version=0.66.2\\n");
        let own = fake_lazygit("own", "version=0.66.9\\n");
        let home = tmp("home-exe");
        let env_home = format!("HOME={}", home.display());
        let nothing = |_: &str| None;
        // `/proc/<pid>/exe` is what runs; a broken one falls back to PATH's.
        let proc = fake_proc("exe", "lazygit", &["lazygit"], &[&env_home], &home);
        std::os::unix::fs::symlink(own.join("lazygit"), proc.join("4242/exe")).unwrap();
        let (_, notes) = sheet_with(
            REGISTRY,
            path(&on_path),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &nothing,
        )
        .unwrap();
        assert_eq!(
            notes[0],
            "lazygit 0.66.9: using the defaults of 0.66.2; add data/lazygit/0.66.9.toml"
        );
        let proc = fake_proc("exe-gone", "lazygit", &["lazygit"], &[&env_home], &home);
        std::os::unix::fs::symlink(home.join("gone"), proc.join("4242/exe")).unwrap();
        let (_, notes) = sheet_with(
            REGISTRY,
            path(&on_path),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &nothing,
        )
        .unwrap();
        assert!(notes[0].starts_with("leaving out"), "{notes:?}");
        // No environ: keybind-find's HOME, with a note.
        std::fs::create_dir_all(home.join(".config/lazygit")).unwrap();
        std::fs::write(
            home.join(".config/lazygit/config.yml"),
            "keybinding:\n  universal:\n    quit: X\n",
        )
        .unwrap();
        let proc = fake_proc("no-environ", "lazygit", &["lazygit"], &[], &home);
        std::fs::remove_file(proc.join("4242/environ")).unwrap();
        let own_home = home.display().to_string();
        let own_env = move |k: &str| (k == "HOME").then(|| own_home.clone());
        let (s, notes) = sheet_with(
            REGISTRY,
            path(&on_path),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &own_env,
        )
        .unwrap();
        assert_eq!(
            notes[0],
            "can't read lazygit's environment; using keybind-find's"
        );
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
        // The first of a name given twice.
        let proc = fake_proc(
            "twice",
            "lazygit",
            &["lazygit"],
            &["HOME=a", "HOME=b"],
            &home,
        );
        assert_eq!(environ(&proc.join("4242")).unwrap()["HOME"], "a");
        // An environ that reads empty (a zombie's) counts as unreadable.
        std::fs::write(proc.join("4242/environ"), "").unwrap();
        assert_eq!(environ(&proc.join("4242")), None);
        // An exe that fails or hangs: PATH's lazygit, or (none there, a hang) an error within the timeout.
        let failing = fake_lazygit("exe-fails", "x\\n'; exit 3; '");
        let hanging = fake_lazygit("hangs", "x\\n'; PATH=/usr/bin:/bin exec sleep 5; '");
        let proc = fake_proc("exe-fails", "lazygit", &["lazygit"], &[&env_home], &home);
        std::os::unix::fs::symlink(failing.join("lazygit"), proc.join("4242/exe")).unwrap();
        let (_, notes) = sheet_with(
            REGISTRY,
            path(&own),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &nothing,
        )
        .unwrap();
        assert!(notes[0].starts_with("lazygit 0.66.9"), "{notes:?}");
        let start = std::time::Instant::now();
        assert!(sheet_with(
            REGISTRY,
            path(&hanging),
            None,
            &proc,
            Some(&crate::test_tmp()),
            &nothing
        )
        .is_err());
        // The timeout (2 s) ends it, not the program.
        assert!(
            (VERSION_TIMEOUT..std::time::Duration::from_secs(4)).contains(&start.elapsed()),
            "{:?}",
            start.elapsed()
        );
        // A cwd link that can't be read: a note, and `-ucf own.yml` (relative) skipped.
        let proc = fake_proc(
            "no-cwd",
            "lazygit",
            &["lazygit", "-ucf", "own.yml"],
            &[&env_home],
            &home,
        );
        std::fs::remove_file(proc.join("4242/cwd")).unwrap();
        let (s, notes) = sheet_with(
            REGISTRY,
            path(&on_path),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &nothing,
        )
        .unwrap();
        assert_eq!(
            notes[0],
            "can't read lazygit's working directory; skipping relative config paths and the repo's files"
        );
        assert_eq!(keys(&s, "Global keybindings", "Quit"), ["q", "ctrl+c"]);
        // The repo's files after the global ones, from lazygit's cwd.
        let repo = tmp("repo-sheet");
        let ok = Command::new("git").args(["init", "-q"]).arg(&repo).status();
        if !ok.is_ok_and(|s| s.success()) {
            eprintln!("skipping the repo part: git is not installed");
            return;
        }
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        std::fs::write(
            repo.join(".git/lazygit.yml"),
            "keybinding:\n  universal:\n    quit: Z\n",
        )
        .unwrap();
        let proc = fake_proc(
            "repo",
            "lazygit",
            &["lazygit"],
            &[&env_home],
            &repo.join("sub"),
        );
        let (s, _) = sheet_with(
            REGISTRY,
            path(&on_path),
            Some(4242),
            &proc,
            Some(&crate::test_tmp()),
            &nothing,
        )
        .unwrap();
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+z", "ctrl+c"]
        );
    }

    #[test]
    fn search_path_and_flags() {
        let env = |vars: Vec<(&'static str, &'static str)>| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        // Duplicates dropped, `$HOME` expanded, relative entries dropped.
        assert_eq!(
            config_dirs(&env(vec![
                ("HOME", "/h"),
                ("XDG_CONFIG_DIRS", "/a:$HOME/b:/a:rel:~/b")
            ])),
            ["/h/.config", "/a", "/h/b"].map(PathBuf::from)
        );
        // HOME unset: `/`.
        assert_eq!(
            config_dirs(&env(vec![])),
            ["/.config", "/etc/xdg"].map(PathBuf::from)
        );
        // `-ucf` with no value after it: nothing (flaggy exits on it, subCommand.go:156-159, so lazygit never runs so);
        // a trailing one keeps the value before it.
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(flags(&args(&["-ucf"])), (None, None));
        assert_eq!(
            flags(&args(&["-ucf", "a", "-ucf"])),
            (Some("a".into()), None)
        );
        // An empty `-ucf=` doesn't override LG_CONFIG_FILE (entry_point.go:84-90 sets it only when non-empty); an
        // empty LG_CONFIG_FILE falls through to CONFIG_DIR, relative to lazygit's cwd.
        let lg = env(vec![
            ("LG_CONFIG_FILE", "/x.yml"),
            ("XDG_CONFIG_DIRS", "/nowhere-kbf"),
        ]);
        let cwd = Path::new("/w");
        assert_eq!(
            global_files(&lg, &args(&["-ucf="]), Some(cwd)),
            [(PathBuf::from("/x.yml"), true)]
        );
        let dir = env(vec![("LG_CONFIG_FILE", ""), ("CONFIG_DIR", "c")]);
        assert_eq!(
            global_files(&dir, &[], Some(cwd)),
            [(PathBuf::from("/w/c/config.yml"), false)]
        );
        // lazygit's cwd unknown: relative paths are skipped.
        assert!(global_files(&dir, &[], None).is_empty());
        assert_eq!(
            global_files(&lg, &args(&["-ucf", "a.yml,/b.yml"]), None),
            [(PathBuf::from("/b.yml"), true)]
        );
    }

    #[test]
    fn what_lazygit_refuses_in_a_file() {
        for (text, says) in [
            // A section left null (everything under it commented out).
            (
                "keybinding:\n  universal:\n    # quit: X\n  files:\n    commitChanges: C\n",
                "line 2: `universal` is empty (null)",
            ),
            // A list inside a key list (only jumpToBlock holds those).
            (
                "keybinding:\n  universal:\n    quit: [[q]]\n",
                "line 3: a list inside a key list (universal.quit)",
            ),
            (
                "keybinding:\n  universal:\n    quit:\n      - - q\n",
                "line 3: a list inside a key list",
            ),
            // `keybinding` itself null.
            ("keybinding: ~\n", "`keybinding` is empty (null)"),
            ("keybinding: null\n", "`keybinding` is empty (null)"),
            // jumpToBlock: a list of key lists, nothing else.
            (
                "keybinding:\n  universal:\n    jumpToBlock: x\n",
                "line 3: `universal.jumpToBlock` isn't a list",
            ),
            (
                "keybinding:\n  universal:\n    jumpToBlock:\n",
                "line 3: `universal.jumpToBlock` isn't a list",
            ),
            (
                "keybinding:\n  universal:\n    jumpToBlock: [[a, [b]]]\n",
                "line 3: a list nested too deep",
            ),
            (
                "keybinding:\n  universal:\n    jumpToBlock: {a: b}\n",
                "line 3: a mapping where a key list",
            ),
        ] {
            let (s, notes, _) = with_files("refuse", &[text]);
            assert!(notes[0].contains(says), "{notes:?}");
            assert!(
                notes[0].ends_with("its keybindings are ignored"),
                "{notes:?}"
            );
            assert_eq!(keys(&s, "Files", "Commit"), ["c"]);
        }
        // jumpToBlock as key lists is read and ignored; a null field is no key; `keybinding: {}` is none.
        let (s, notes, _) = with_files(
            "jump",
            &[
                "keybinding:\n  universal:\n    jumpToBlock: [['1', '!'], '2']\n    edit:\n    quit:\n    - X\n",
                "keybinding:\n  universal:\n    jumpToBlock:\n      - - '1'\n        - ~\n      - '2'\n",
                "keybinding: {}\n",
            ],
        );
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
        // A label lazygit refuses in jumpToBlock: the refused-key note, the file's other keys as written.
        let (s, notes, _) = with_files(
            "jump-label",
            &["keybinding:\n  universal:\n    jumpToBlock: ['<bogus>']\n    quit: X\n"],
        );
        assert!(notes[0].ends_with("0.yml: lazygit refuses the key <bogus> (and with it its whole config); this file's other keys are listed as written"), "{notes:?}");
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
        // A BOM is read; bytes that aren't UTF-8 refuse the file.
        let dir = tmp("bytes");
        let f = dir.join("c.yml");
        std::fs::write(&f, b"keybinding:\n  universal:\n    quit: \xff\n").unwrap();
        let mut notes = Vec::new();
        merged("0.66.2", &pin().defaults, &[(f, false)], &mut notes);
        assert!(notes[0].contains("not UTF-8"), "{notes:?}");
        let (s, notes, _) =
            with_files("bom", &["\u{feff}keybinding:\n  universal:\n    quit: X\n"]);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            keys(&s, "Global keybindings", "Quit"),
            ["shift+x", "ctrl+c"]
        );
    }

    #[test]
    fn unreadable_and_oversized_files_are_noted() {
        let dir = tmp("big");
        let big = dir.join("big.yml");
        std::fs::write(&big, vec![b'#'; (MAX_SIZE + 1) as usize]).unwrap();
        let mut notes = Vec::new();
        let files = [(big.clone(), false), (dir.clone(), false)];
        merged("0.66.2", &pin().defaults, &files, &mut notes);
        assert_eq!(
            notes,
            [
                format!("can't read {}: larger than 4 MiB; skipped", big.display()),
                format!("can't read {}: not a regular file; skipped", dir.display())
            ]
        );
        // A FIFO (no writer) is refused without blocking.
        let fifo = dir.join("fifo.yml");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert_eq!(read_file(&fifo), Err("not a regular file".into()));
    }
}
