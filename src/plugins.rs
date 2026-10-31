//! Parser plugins declared in `config.toml` (`[[plugin]]`, see `crate::config::Plugin`). A plugin applies when every
//! `match` key it gives matches the focused window; it is sent the protocol v1 `Request` (the window, and the inputs
//! read here) on stdin and prints a `Response`. It runs beside the providers, as a provider named after the plugin,
//! with the time limit and failure isolation of executable sheets: anything wrong is one stderr line naming it.

use crate::config::{Match, Plugin};
use crate::context::{run_for_input, Context};
use crate::sheet::Sheet;
use anyhow::{bail, Context as _};
use keybind_find_sheet::{Input, Request, Response, VERSION};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

/// The same limit as an executable sheet's (`sheets::PARSER_TIMEOUT`), per plugin: they run in parallel.
const TIMEOUT: Duration = Duration::from_secs(3);

/// An input bigger than this is an error, not something sent down a pipe.
const INPUT_LIMIT: u64 = 1 << 20;

/// The whole request, so all of one plugin's inputs together (a broad `*` could match hundreds of files).
/// ponytail: one fixed limit, the size of the output a program may print; raise it if a real plugin needs more.
const INPUTS_LIMIT: u64 = 8 << 20;

/// The plugins that apply to this run, and the window they are told about.
/// ponytail: kept here beside `Context` (which holds only provider names) rather than in it, so `Context`'s literals
/// elsewhere don't change; move it into `Context` when a story owns those.
static ACTIVE: OnceLock<(Vec<Plugin>, keybind_find_sheet::Context)> = OnceLock::new();

/// Adds the configured plugins that match `window` to `providers` (see `place`). Once per run: a second call adds
/// nothing.
pub fn add(providers: &mut Vec<&'static str>, window: keybind_find_sheet::Context) {
    add_from(providers, configured, window);
}

/// `add`, with the configured plugins from `all` (a test seam), asked only on the first call.
fn add_from(
    providers: &mut Vec<&'static str>,
    all: impl FnOnce() -> Vec<Plugin>,
    window: keybind_find_sheet::Context,
) {
    let mut fresh = false;
    let (plugins, _) = ACTIVE.get_or_init(|| {
        fresh = true;
        let all = all();
        for p in all.iter().filter(|p| p.matches.is_none()) {
            eprintln!(
                "keybind-find: {}: no [plugin.match], so it never runs",
                p.name
            );
        }
        let applying = all
            .into_iter()
            .filter(|p| p.matches.as_ref().is_some_and(|m| matches(m, &window)))
            .collect();
        (applying, window)
    });
    if fresh {
        place(providers, plugins);
    }
}

/// Whether every key `m` gives matches `window`, ignoring case: `class` and `program` whole, `window_title` as a
/// substring. A key given as an empty list matches nothing.
fn matches(m: &Match, window: &keybind_find_sheet::Context) -> bool {
    let any = |want: &Option<Vec<String>>, have: &Option<String>, part: bool| {
        want.as_ref().is_none_or(|want| {
            have.as_deref().is_some_and(|have| {
                let have = have.to_lowercase();
                want.iter().any(|w| {
                    let w = w.to_lowercase();
                    if part {
                        have.contains(&w)
                    } else {
                        have == w
                    }
                })
            })
        })
    };
    any(&m.class, &window.class, false)
        && any(&m.program, &window.program, false)
        && any(&m.window_title, &window.title, true)
}

/// Puts each plugin where the turn order (`usage::alternate`, innermost last) wants it: one matched by `program` among
/// the programs inside the terminal (before the class-matched `sheets`), any other beside `sheets`, after it.
fn place<'a>(providers: &mut Vec<&'a str>, plugins: &'a [Plugin]) {
    for p in plugins {
        let by_program = p.matches.as_ref().is_some_and(|m| m.program.is_some());
        match providers.iter().position(|n| *n == "sheets") {
            Some(i) if by_program => providers.insert(i, &p.name),
            _ => providers.push(&p.name),
        }
    }
}

/// The sheet of the plugin called `name` in this run, or `None` when the name isn't a configured plugin's. A
/// configured one that doesn't apply here is an error saying so (`dump <name>` from another window).
pub fn sheet(name: &str, _ctx: &Context) -> Option<anyhow::Result<Sheet>> {
    sheet_with(name, crate::config::plugins)
}

/// `sheet`, with the configured plugins from `configured` (a test seam), asked only for a name not in this run.
/// Names compare ignoring case, as the config keeps them apart.
fn sheet_with(
    name: &str,
    configured: impl FnOnce() -> Vec<Plugin>,
) -> Option<anyhow::Result<Sheet>> {
    let same = |p: &Plugin| p.name.to_lowercase() == name.to_lowercase();
    let applying = ACTIVE
        .get()
        .and_then(|(plugins, window)| Some((plugins.iter().find(|p| same(p))?, window)));
    match applying {
        Some((p, window)) => Some(run(p, window, &home(), TIMEOUT)),
        None => configured().iter().any(same).then(|| {
            Err(anyhow::anyhow!(
                "plugin {name} doesn't apply to the focused window (its match, or no match)"
            ))
        }),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Reads `p`'s inputs, sends the request and reads the response: the sheet, its keys checked and in canonical
/// spelling, named after the plugin, with every `action` dropped (a pick sends keys, as for every provider).
fn run(
    p: &Plugin,
    window: &keybind_find_sheet::Context,
    home: &Option<PathBuf>,
    timeout: Duration,
) -> anyhow::Result<Sheet> {
    let mut skipped = Vec::new();
    let request = request(&p.inputs, window, home, &mut skipped);
    for path in skipped {
        eprintln!("keybind-find: {}: input {path} not found; skipped", p.name);
    }
    let request = request?;
    let program = tilde(&p.program, home.as_deref());
    let out = run_for_input(Command::new(&program), timeout, Some(request))?;
    let mut sheet = response(&out)?;
    sheet.provider = p.name.clone();
    for binds in sheet.binds.values_mut() {
        for b in binds {
            b.action.clear();
        }
    }
    Ok(sheet)
}

/// The encoded request for `window` with the files `patterns` name. Each pattern or file that isn't there is added
/// to `skipped`, also when a later one is an error.
fn request(
    patterns: &[String],
    window: &keybind_find_sheet::Context,
    home: &Option<PathBuf>,
    skipped: &mut Vec<String>,
) -> anyhow::Result<Vec<u8>> {
    let mut inputs = Vec::new();
    for pattern in patterns {
        let paths = expand(pattern, home.as_deref())?;
        if paths.is_empty() {
            skipped.push(pattern.clone());
        }
        for path in paths {
            match read_input(&path) {
                Ok(Some(text)) => inputs.push(Input {
                    path: path.display().to_string(),
                    text,
                }),
                Ok(None) => skipped.push(path.display().to_string()),
                Err(e) => return Err(e).with_context(|| format!("input {}", path.display())),
            }
        }
    }
    let request = serde_json::to_vec(&Request {
        version: VERSION,
        context: window.clone(),
        inputs,
    })
    .context("encoding the request")?;
    // The encoded size, escapes included, is what goes down the pipe.
    if request.len() as u64 > INPUTS_LIMIT {
        bail!(
            "its request (the inputs together) is over {} MiB",
            INPUTS_LIMIT >> 20
        );
    }
    Ok(request)
}

/// The sheet in a plugin's output: version first, so a later version says so rather than failing to parse.
fn response(out: &str) -> anyhow::Result<Sheet> {
    #[derive(serde::Deserialize)]
    struct Versioned {
        version: u32,
    }
    let v: Versioned = serde_json::from_str(out).context("its output is not a response")?;
    if v.version != VERSION {
        bail!("response version {} is not {VERSION}", v.version);
    }
    let r: Response = serde_json::from_str(out).context("its output is not a response")?;
    let mut sheet = r.sheet;
    keybind_find_sheet::normalize_sheet(&mut sheet).context("a key is not in the key notation")?;
    Ok(sheet)
}

/// A file's text, `None` when it doesn't exist; over `INPUT_LIMIT`, or not a regular file, is an error. Checked
/// before opening: opening a FIFO waits for a writer, outside any time limit.
fn read_input(path: &Path) -> anyhow::Result<Option<String>> {
    use std::io::Read;
    match std::fs::metadata(path) {
        Ok(m) if !m.is_file() => bail!("not a regular file"),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(INPUT_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > INPUT_LIMIT {
        bail!("over {} MiB", INPUT_LIMIT >> 20);
    }
    Ok(Some(String::from_utf8(bytes).context("not UTF-8 text")?))
}

/// `~/x` under `home`; anything else as written. No home: as written.
fn tilde(path: &str, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if path == "~" => home.map_or_else(|| PathBuf::from(path), Path::to_path_buf),
        _ => PathBuf::from(path),
    }
}

/// The files `pattern` names, sorted: itself (whether or not it exists), or with a `*` in its last component (one at
/// most, as the config checks), the entries of that directory matching around it. A missing directory matches none.
/// ponytail: no `**` or `?`; add when a plugin needs them.
fn expand(pattern: &str, home: Option<&Path>) -> anyhow::Result<Vec<PathBuf>> {
    let path = tilde(pattern, home);
    let Some(last) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Ok(vec![path]);
    };
    let Some((prefix, suffix)) = last.split_once('*') else {
        return Ok(vec![path]);
    };
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let dir = dir.unwrap_or(Path::new("."));
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.len() >= prefix.len() + suffix.len() && n.starts_with(prefix) && n.ends_with(suffix)
        })
        .filter(|e| e.path().is_file())
        .map(|e| dir.join(e.file_name()))
        .collect();
    found.sort();
    Ok(found)
}

/// The configured plugins, each stanza that leaves out `inputs` or `match` given its plugin's manifest defaults
/// (cached, so a normal run asks no plugin for `--manifest`). A manifest that can't be had leaves the stanza as
/// written; it is one stderr line when the stanza has no `match` of its own, or when the plugin gave no answer (it
/// then costs a run its time limit, at most once an hour). A plugin that asks a running program needs no inputs and
/// may have no manifest, so a refusal is quiet when the stanza has a `match`.
fn configured() -> Vec<Plugin> {
    configured_in(&Env::real(), crate::config::plugins())
}

/// `configured` in `env`: the same cache `plugin list` and `plugin add` fill, under the same key.
fn configured_in(env: &Env, stanzas: Vec<Plugin>) -> Vec<Plugin> {
    let cache = env.cache_file();
    with_defaults(stanzas, |program| {
        let path = env.resolve(program).context("not found on PATH")?;
        manifest(&path, cache.as_deref(), ask_manifest)
    })
}

/// `p` with its plugin's defaults wherever the stanza is silent: no `inputs` (an empty list counts as none: the
/// config can't tell them apart) or no `match`. The config wins wherever it sets a key.
fn with_defaults(
    plugins: Vec<Plugin>,
    mut manifest_of: impl FnMut(&str) -> anyhow::Result<keybind_find_sheet::Manifest>,
) -> Vec<Plugin> {
    plugins
        .into_iter()
        .map(|mut p| {
            if !p.inputs.is_empty() && p.matches.is_some() {
                return p;
            }
            match manifest_of(&p.program) {
                Ok(m) => {
                    if p.inputs.is_empty() {
                        p.inputs = m.inputs;
                    }
                    if p.matches.is_none() {
                        p.matches = m.matches.map(|m| Match {
                            class: m.class,
                            program: m.program,
                            window_title: m.window_title,
                        });
                    }
                }
                Err(e) if p.matches.is_none() || e.to_string().starts_with(NO_ANSWER) => {
                    eprintln!("keybind-find: {}: its manifest: {e:#}", p.name)
                }
                Err(_) => {}
            }
            p
        })
        .collect()
}

/// Asks the plugin at `path` for its manifest and checks it.
/// The outer error is no answer (it didn't start, or didn't finish in time), which may pass and isn't remembered;
/// the inner one is an answer without a manifest (it ran and refused, or printed something else), which is.
fn ask_manifest(path: &Path) -> anyhow::Result<Result<keybind_find_sheet::Manifest, String>> {
    let mut cmd = Command::new(path);
    cmd.arg("--manifest");
    let out = match run_for_input(cmd, TIMEOUT, None) {
        Ok(out) => out,
        // `run_for`'s error for a non-zero exit status: it ran and said no.
        Err(e) if took_refused(&e) => return Ok(Err(format!("{e:#}"))),
        Err(e) => return Err(e),
    };
    Ok(parse_manifest(&out).map_err(|e| format!("{e:#}")))
}

/// A manifest from `--manifest` output: the version first (a later one says so rather than failing to parse), then
/// the rest, checked. An empty `match` names no window, as an empty `[plugin.match]` does.
fn parse_manifest(out: &str) -> anyhow::Result<keybind_find_sheet::Manifest> {
    #[derive(serde::Deserialize)]
    struct Versioned {
        version: u32,
    }
    let v: Versioned =
        serde_json::from_str(out).context("its --manifest output is not a manifest")?;
    if v.version != VERSION {
        bail!("manifest version {} is not {VERSION}", v.version);
    }
    let mut m: keybind_find_sheet::Manifest =
        serde_json::from_str(out).context("its --manifest output is not a manifest")?;
    if m.matches == Some(keybind_find_sheet::Match::default()) {
        m.matches = None;
    }
    check_manifest(&m)?;
    Ok(m)
}

/// The rules a stanza is held to (`config::plugin`), so a manifest's defaults never write one the config skips.
fn check_manifest(m: &keybind_find_sheet::Manifest) -> anyhow::Result<()> {
    if m.version != VERSION {
        bail!("manifest version {} is not {VERSION}", m.version);
    }
    let reserved = |n: &str| {
        let n = n.to_lowercase();
        n == "keybind-find" || crate::providers::KNOWN.contains(&n.as_str())
    };
    if m.name.trim().is_empty() || m.name.trim() != m.name || m.name.chars().any(char::is_control) {
        bail!("manifest name {:?} is not a plugin name", m.name);
    }
    if reserved(&m.name) {
        bail!("manifest name {:?} is keybind-find's own", m.name);
    }
    if let Some(i) = m.inputs.iter().find(|i| {
        let (dir, last) = i.rsplit_once('/').unwrap_or(("", i));
        !(i.starts_with('/') || i.starts_with("~/"))
            || dir.contains('*')
            || last.matches('*').count() > 1
    }) {
        bail!("manifest input {i:?}: a path starting with / or ~/, with one `*` at most, in the last component");
    }
    if let Some(m) = &m.matches {
        for (key, values) in [
            ("class", &m.class),
            ("program", &m.program),
            ("window_title", &m.window_title),
        ] {
            if values.as_ref().is_some_and(|v| {
                v.is_empty() || v.iter().any(|s| s.trim().is_empty() || s.trim() != s)
            }) {
                bail!("manifest match {key} has an empty or space-padded value");
            }
        }
    }
    Ok(())
}

/// One manifest cache entry: the plugin file's modification time and size when it was asked, and what it said, or
/// why it had none (so a program without one isn't asked again on every run either). `retry_after` (Unix seconds)
/// marks a run that got no answer, asked again after it; `keybind_find` is the version that checked it, so an
/// upgrade with other rules asks again.
#[derive(serde::Serialize, serde::Deserialize)]
struct Cached {
    keybind_find: String,
    mtime: (u64, u32),
    size: u64,
    retry_after: Option<u64>,
    manifest: Result<keybind_find_sheet::Manifest, String>,
}

/// How a manifest error that was no answer starts.
const NO_ANSWER: &str = "no answer";

/// How long a plugin that gave no answer is left alone.
const RETRY: u64 = 3600;

/// The manifest of the plugin at `path`: from `cache` (a JSON object keyed by the file's real path, so a symlinked
/// profile that moves to a new store path counts as new) while its modification time and size are unchanged, else
/// from `ask`, then stored unless there was no answer. A cache that can't be read or written is just no cache.
fn manifest(
    path: &Path,
    cache: Option<&Path>,
    ask: impl FnOnce(&Path) -> anyhow::Result<Result<keybind_find_sheet::Manifest, String>>,
) -> anyhow::Result<keybind_find_sheet::Manifest> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or_default();
    let size = meta.len();
    let key = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    let mut entries: std::collections::BTreeMap<String, Cached> = cache
        .and_then(|c| std::fs::read_to_string(c).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let version = env!("CARGO_PKG_VERSION");
    if let Some(hit) = entries.get(&key).filter(|c| {
        c.keybind_find == version
            && c.mtime == mtime
            && c.size == size
            && c.retry_after.is_none_or(|t| now < t)
    }) {
        return hit.manifest.clone().map_err(|e| anyhow::anyhow!(e));
    }
    // No answer (it didn't start or finish) may pass: remembered for an hour only, so a plugin that hangs on
    // `--manifest` doesn't cost every run its time limit, and a slow first run doesn't lose its defaults for good.
    let (m, retry_after) = match ask(path) {
        Ok(m) => (m, None),
        Err(e) => (
            Err(format!("{NO_ANSWER}: {e:#}; asked again in an hour")),
            Some(now + RETRY),
        ),
    };
    if let Some(cache) = cache {
        entries.insert(
            key,
            Cached {
                keybind_find: version.to_string(),
                mtime,
                size,
                retry_after,
                manifest: m.clone(),
            },
        );
        // Written whole and renamed into place, so a run reading it at the same time never sees half of it.
        let tmp = cache.with_extension(format!("json.{}", std::process::id()));
        if let Ok(json) = serde_json::to_vec(&entries) {
            let written = cache
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&tmp, json))
                .and_then(|()| std::fs::rename(&tmp, cache));
            if written.is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
    m.map_err(|e| anyhow::anyhow!(e))
}

/// Where plugins are looked for and the config and cache live: the environment, as one value so tests can make
/// one up.
struct Env {
    path: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
    config_home: Option<PathBuf>,
    cache_home: Option<PathBuf>,
}

impl Env {
    fn real() -> Env {
        let dir = |v: &str| {
            std::env::var_os(v)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
        };
        Env {
            path: std::env::var_os("PATH"),
            home: home(),
            config_home: dir("XDG_CONFIG_HOME"),
            cache_home: dir("XDG_CACHE_HOME"),
        }
    }

    /// `$XDG_CONFIG_HOME/keybind-find` (absolute only, as the XDG spec and `config::path` say), else `~/.config`.
    fn config_dir(&self) -> Option<PathBuf> {
        let base = self
            .config_home
            .clone()
            .or_else(|| Some(self.home.clone()?.join(".config")))?;
        Some(base.join("keybind-find"))
    }

    fn cache_file(&self) -> Option<PathBuf> {
        let base = self
            .cache_home
            .clone()
            .or_else(|| Some(self.home.clone()?.join(".cache")))?;
        Some(base.join("keybind-find").join("manifests.json"))
    }

    /// The absolute directories on `PATH`: an empty or relative entry means the working directory, which a hotkey
    /// doesn't choose (and a stanza can't name a program by it).
    fn path_dirs(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.path
            .iter()
            .flat_map(std::env::split_paths)
            .filter(|d| d.is_absolute())
    }

    /// A stanza's `program` as a file: a path (`~/` expanded), or the first executable of that name on `PATH`.
    fn resolve(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            return Some(tilde(program, self.home.as_deref()));
        }
        self.path_dirs()
            .map(|d| d.join(program))
            .find(|p| is_executable(p))
    }

    /// Installed plugins: each `keybind-find-<name>` on `PATH` (the first of a name, as a shell would run it),
    /// then each executable in `<config dir>/plugins/`, in name order.
    fn discover(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = Vec::new();
        let sorted = |dir: &Path| -> Vec<PathBuf> {
            let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|e| e.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            v.sort();
            v
        };
        for dir in self.path_dirs() {
            for p in sorted(&dir) {
                let named = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.len() > PREFIX.len() && n.starts_with(PREFIX));
                if named
                    && is_executable(&p)
                    && !found.iter().any(|f| f.file_name() == p.file_name())
                {
                    found.push(p);
                }
            }
        }
        if let Some(dir) = self.config_dir() {
            found.extend(
                sorted(&dir.join("plugins"))
                    .into_iter()
                    .filter(|p| is_executable(p)),
            );
        }
        found
    }
}

/// The file name a plugin on `PATH` has.
const PREFIX: &str = "keybind-find-";

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// One installed plugin: where it is, the program a stanza names it by, and its manifest or why there is none.
struct Found {
    path: PathBuf,
    program: String,
    manifest: anyhow::Result<keybind_find_sheet::Manifest>,
}

impl Found {
    /// The manifest's name, else the file's (without `keybind-find-`).
    fn name(&self) -> String {
        match &self.manifest {
            Ok(m) => m.name.clone(),
            Err(_) => {
                let file = self
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                file.strip_prefix(PREFIX)
                    .map(str::to_string)
                    .unwrap_or(file)
            }
        }
    }
}

fn found(env: &Env) -> Vec<Found> {
    let cache = env.cache_file();
    env.discover()
        .into_iter()
        .map(|path| {
            // A path, not a bare name, even for one on PATH: Super+/ runs keybind-find with the window manager's
            // PATH, which often lacks `~/.cargo/bin`. Under home as `~/`, so a shared config still reads.
            let under_home = env.home.as_deref().and_then(|h| path.strip_prefix(h).ok());
            let program = match under_home {
                Some(rest) => format!("~/{}", rest.display()),
                None => path.display().to_string(),
            };
            let manifest = manifest(&path, cache.as_deref(), ask_manifest);
            Found {
                path,
                program,
                manifest,
            }
        })
        .collect()
}

/// `keybind-find plugin list`: each installed plugin, whether a `[[plugin]]` stanza runs it, and its manifest.
pub fn list() -> anyhow::Result<String> {
    let env = Env::real();
    Ok(list_text(&env, &crate::config::plugins()))
}

fn list_text(env: &Env, stanzas: &[Plugin]) -> String {
    let found = found(env);
    if found.is_empty() {
        return format!(
            "no plugins found: none named {PREFIX}<name> on PATH, and none in {}",
            env.config_dir().map_or("the plugins folder".into(), |d| d
                .join("plugins")
                .display()
                .to_string())
        );
    }
    let mut out = String::new();
    for f in &found {
        let enabled = stanzas.iter().find(|s| {
            env.resolve(&s.program)
                .is_some_and(|p| same_file(&p, &f.path))
        });
        let no_match = |s: &Plugin| {
            s.matches.is_none() && f.manifest.as_ref().map_or(true, |m| m.matches.is_none())
        };
        let state = match enabled {
            Some(s) if no_match(s) => format!(
                "in the config as {:?}, but with no match it never runs",
                s.name
            ),
            Some(s) => format!("enabled as {:?}", s.name),
            None => "not in the config".to_string(),
        };
        out.push_str(&format!("{}\t{state}\t{}\n", f.name(), f.path.display()));
        match &f.manifest {
            Ok(m) => {
                let matching = m.matches.as_ref().map_or(String::new(), |mm| {
                    [
                        ("class", &mm.class),
                        ("program", &mm.program),
                        ("window_title", &mm.window_title),
                    ]
                    .iter()
                    .filter_map(|(k, v)| Some(format!("{k} {}", v.as_ref()?.join(" or "))))
                    .collect::<Vec<_>>()
                    .join(", ")
                });
                for (label, value) in [
                    ("", one_line(&m.description)),
                    ("inputs: ", one_line(&m.inputs.join(", "))),
                    ("match: ", one_line(&matching)),
                    ("asks: ", one_line(&m.queries.join(", "))),
                    (
                        "homepage: ",
                        one_line(&m.homepage.clone().unwrap_or_default()),
                    ),
                ] {
                    if !value.is_empty() {
                        out.push_str(&format!("    {label}{value}\n"));
                    }
                }
            }
            Err(e) => out.push_str(&format!(
                "    no manifest: {}\n",
                one_line(&format!("{e:#}"))
            )),
        }
    }
    out.trim_end().to_string()
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `keybind-find plugin add <name>`: appends a stanza for the installed plugin `name` to `config.toml` (created if
/// missing) as text, so what is there stays as written, and returns what it added. Refuses a name a stanza already
/// has, or a config that isn't TOML (a duplicate couldn't be ruled out).
pub fn add_to_config(name: &str) -> anyhow::Result<String> {
    let env = Env::real();
    let config = env
        .config_dir()
        .context("no config folder: set XDG_CONFIG_HOME or HOME")?
        .join("config.toml");
    add_stanza(&env, name, &config)
}

fn add_stanza(env: &Env, name: &str, config: &Path) -> anyhow::Result<String> {
    let found = found(env);
    let f = found
        .iter()
        .find(|f| f.name().to_lowercase() == name.to_lowercase())
        .with_context(|| {
            format!("no installed plugin named {name:?} (see keybind-find plugin list)")
        })?;
    let m = match &f.manifest {
        Ok(m) => m,
        Err(e) => bail!("{}: no manifest: {e:#}", f.path.display()),
    };
    let text = match std::fs::read_to_string(config) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", config.display())),
    };
    let doc: toml::Value = toml::from_str(&text)
        .with_context(|| format!("{} is not valid TOML; fix it first", config.display()))?;
    let stanzas: Vec<&toml::Value> = doc
        .get("plugin")
        .and_then(|p| p.as_array())
        .into_iter()
        .flatten()
        .collect();
    let field = |s: &&toml::Value, k: &str| s.get(k).and_then(|v| v.as_str()).map(str::to_string);
    if stanzas
        .iter()
        .filter_map(|s| field(s, "name"))
        .any(|n| n.to_lowercase() == m.name.to_lowercase())
    {
        bail!(
            "{} already has a [[plugin]] named {:?}",
            config.display(),
            m.name
        );
    }
    if let Some(other) = stanzas.iter().find(|s| {
        field(s, "program")
            .and_then(|p| env.resolve(&p))
            .is_some_and(|p| same_file(&p, &f.path))
    }) {
        bail!(
            "{} already runs {} as {:?}",
            config.display(),
            f.path.display(),
            field(other, "name").unwrap_or_default()
        );
    }
    let stanza = stanza(m, &f.program);
    let sep = if text.is_empty() || text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    // The file as it will be must still read, with the new stanza last: a `plugin = [...]` array or a `[plugin]`
    // table already there would make the appended `[[plugin]]` break the whole config.
    let after: Option<toml::Value> = toml::from_str(&format!("{text}{sep}{stanza}")).ok();
    let last = after.as_ref().and_then(|d| {
        d.get("plugin")?
            .as_array()?
            .last()?
            .get("name")?
            .as_str()
            .map(str::to_string)
    });
    if last.as_deref() != Some(m.name.as_str()) {
        bail!(
            "{}: can't append a [[plugin]] stanza after its `plugin` key; write each plugin as a [[plugin]] stanza",
            config.display()
        );
    }
    if let Some(dir) = config.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config)
        .and_then(|mut f| f.write_all(format!("{sep}{stanza}").as_bytes()))
        .with_context(|| format!("writing {}", config.display()))?;
    Ok(format!(
        "added to {}:\n\n{}",
        config.display(),
        stanza.trim_end()
    ))
}

/// The stanza text for a plugin run as `program`, with its manifest's defaults written out to edit.
fn stanza(m: &keybind_find_sheet::Manifest, program: &str) -> String {
    let s = |v: &str| toml::Value::String(v.to_string()).to_string();
    let list = |v: &[String]| {
        toml::Value::Array(v.iter().map(|x| toml::Value::String(x.clone())).collect()).to_string()
    };
    let mut out = format!("# {}", m.name);
    if !m.description.is_empty() {
        out.push_str(&format!(": {}", one_line(&m.description)));
    }
    out.push('\n');
    if let Some(h) = &m.homepage {
        out.push_str(&format!("# {}\n", one_line(h)));
    }
    out.push_str(
        "# Added by `keybind-find plugin add`, with the plugin's defaults: edit them freely.\n",
    );
    out.push_str(&format!(
        "[[plugin]]\nname = {}\nprogram = {}\n",
        s(&m.name),
        s(program)
    ));
    out.push_str(&format!("inputs = {}\n", list(&m.inputs)));
    match &m.matches {
        Some(mm) => {
            out.push_str("[plugin.match]\n");
            for (key, values) in [("class", &mm.class), ("program", &mm.program), ("window_title", &mm.window_title)] {
                if let Some(v) = values {
                    out.push_str(&format!("{key} = {}\n", list(v)));
                }
            }
        }
        None => out.push_str("# No default match: add a [plugin.match] (class, program or window_title), or it never runs.\n"),
    }
    out
}

/// One problem `check-plugin` found: the part it is in (`manifest`, `request`, `run` or `response`), where in that
/// part (empty for the whole of it), and what is wrong.
#[derive(serde::Serialize, Debug)]
struct Problem {
    part: &'static str,
    at: String,
    message: String,
}

/// What `check-plugin` found: the plugin's file, its run times against the limit, notes that are not problems, and
/// the problems.
#[derive(serde::Serialize, Debug)]
struct Report {
    plugin: String,
    ok: bool,
    limit_ms: u128,
    manifest_ms: Option<u128>,
    run_ms: Option<u128>,
    notes: Vec<String>,
    problems: Vec<Problem>,
}

impl Report {
    fn new(path: &Path, limit: Duration) -> Report {
        Report {
            plugin: path.display().to_string(),
            ok: false,
            limit_ms: limit.as_millis(),
            manifest_ms: None,
            run_ms: None,
            notes: Vec::new(),
            problems: Vec::new(),
        }
    }

    fn add(&mut self, part: &'static str, at: impl Into<String>, message: impl std::fmt::Display) {
        self.problems.push(Problem {
            part,
            at: one_line(&at.into()),
            message: one_line(&format!("{message:#}")),
        });
    }

    /// One line per note and problem, then a summary.
    fn text(&self) -> String {
        let mut out = format!("checked {}\n", self.plugin);
        for n in &self.notes {
            out.push_str(&format!("note: {n}\n"));
        }
        for p in &self.problems {
            match p.at.as_str() {
                "" => out.push_str(&format!("{}: {}\n", p.part, p.message)),
                at => out.push_str(&format!("{}: {at}: {}\n", p.part, p.message)),
            }
        }
        match self.problems.len() {
            0 => out.push_str("ok: no problems"),
            1 => out.push_str("1 problem"),
            n => out.push_str(&format!("{n} problems")),
        }
        out
    }
}

const CHECK_USAGE: &str =
    "usage: keybind-find check-plugin <name|path> [--input FILE]... [--class C] [--program P] \
     [--title T] [--json]";

/// `keybind-find check-plugin`: runs the plugin as Super+/ would and returns the report (text, or JSON with
/// `--json`), and whether it found no problems.
pub fn check(args: &[&str]) -> anyhow::Result<(String, bool)> {
    let (mut target, mut inputs, mut json) = (None, Vec::new(), false);
    let mut window = keybind_find_sheet::Context::default();
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        let mut value = || {
            it.next()
                .map(|v| v.to_string())
                .with_context(|| format!("{a} needs a value. {CHECK_USAGE}"))
        };
        match a {
            // Absolute, as Super+/ sends every input.
            "--input" => inputs.push(
                std::path::absolute(tilde(&value()?, home().as_deref()))
                    .context("--input")?
                    .display()
                    .to_string(),
            ),
            "--class" => window.class = Some(value()?),
            "--program" => window.program = Some(value()?),
            "--title" => window.title = Some(value()?),
            "--json" => json = true,
            _ if a.starts_with('-') => bail!("unknown option {a}. {CHECK_USAGE}"),
            _ if target.is_none() => target = Some(a),
            _ => bail!("one plugin at a time. {CHECK_USAGE}"),
        }
    }
    let target = target.with_context(|| format!("which plugin? {CHECK_USAGE}"))?;
    let env = Env::real();
    let (path, stanza) = locate(&env, target, crate::config::plugins)?;
    let mut r = check_at(&path, stanza.as_ref(), &inputs, window, &env.home, TIMEOUT);
    // A config that can't be read, or a stanza it skips, would leave Super+/ running something else.
    if let Some(n) = crate::config::load(None).1 {
        r.notes.insert(0, one_line(&format!("config: {n}")));
    }
    let out = if json {
        serde_json::to_string_pretty(&r).context("encoding the report")?
    } else {
        r.text()
    };
    Ok((out, r.ok))
}

/// The plugin file `target` names, and the stanza that runs it: a path (with a `/`), else a configured stanza's
/// name (what Super+/ runs), else an installed plugin's name (as `plugin list` shows it). A path or installed plugin
/// is matched to a stanza by its program, as `plugin list` does.
fn locate(
    env: &Env,
    target: &str,
    configured: impl FnOnce() -> Vec<Plugin>,
) -> anyhow::Result<(PathBuf, Option<Plugin>)> {
    let stanzas = configured();
    let same = |n: &str| n.to_lowercase() == target.to_lowercase();
    let path = if target.contains('/') {
        tilde(target, env.home.as_deref())
    } else if let Some(p) = stanzas.iter().find(|p| same(&p.name)) {
        let path = env
            .resolve(&p.program)
            .with_context(|| format!("plugin {}: {} not found on PATH", p.name, p.program))?;
        return Ok((path, Some(p.clone())));
    } else {
        found(env)
            .into_iter()
            .find(|f| same(&f.name()))
            .map(|f| f.path)
            .with_context(|| {
                format!(
                    "no plugin named {target:?}: give its path (./{target} for one here), or see keybind-find plugin list"
                )
            })?
    };
    let stanza = stanzas.into_iter().find(|s| {
        env.resolve(&s.program)
            .is_some_and(|p| same_file(&p, &path))
    });
    Ok((path, stanza))
}

/// Checks the plugin at `path`: its `--manifest`, then a run with the request for `window` and the inputs (`inputs`,
/// else the stanza's, else the manifest's defaults), each timed against `limit`.
fn check_at(
    path: &Path,
    stanza: Option<&Plugin>,
    inputs: &[String],
    window: keybind_find_sheet::Context,
    home: &Option<PathBuf>,
    limit: Duration,
) -> Report {
    let mut r = Report::new(path, limit);
    if !is_executable(path) {
        r.add("plugin", "", "not an executable file");
        return r;
    }
    // Twice the limit, so a plugin a little too slow is measured and reported, not only killed.
    let timed = |r: &mut Report, part, cmd, input| {
        let start = std::time::Instant::now();
        let out = run_for_input(cmd, limit * 2, input);
        let took = start.elapsed();
        let over = format!("over the {:.1} s limit", limit.as_secs_f32());
        let out = match out {
            Ok(out) if took > limit => {
                r.add(
                    part,
                    "",
                    format!("took {:.1} s, {over}", took.as_secs_f32()),
                );
                Ok(out)
            }
            Err(e) if took > limit => Err(e.context(over)),
            out => out,
        };
        (out, took.as_millis())
    };
    let mut cmd = Command::new(path);
    cmd.arg("--manifest");
    let (out, ms) = timed(&mut r, "manifest", cmd, None);
    r.manifest_ms = Some(ms);
    let manifest = match out {
        // A refusal (it ran and exited non-zero) is allowed, as for a configured plugin (see `with_defaults`).
        Err(e) if took_refused(&e) => {
            r.notes.push(one_line(&format!(
                "no manifest ({e:#}): a stanza then needs its own match and inputs"
            )));
            None
        }
        out => match out.and_then(|o| parse_manifest(&o)) {
            Ok(m) => Some(m),
            Err(e) => {
                r.add("manifest", "", e);
                None
            }
        },
    };
    let matching = stanza.and_then(|p| p.matches.clone()).or_else(|| {
        let m = manifest.as_ref()?.matches.clone()?;
        Some(Match {
            class: m.class,
            program: m.program,
            window_title: m.window_title,
        })
    });
    if stanza.is_none() {
        r.notes.push(
            "no [[plugin]] stanza runs it, so Super+/ doesn't (keybind-find plugin add <name> writes one)".into(),
        );
    }
    match matching {
        None => r
            .notes
            .push("no match: Super+/ runs it only with a [plugin.match] in its stanza".into()),
        Some(m)
            if [&window.class, &window.program, &window.title]
                .iter()
                .any(|v| v.is_some())
                && !matches(&m, &window) =>
        {
            r.notes.push(
                "its match doesn't take this window as given (a field it matches that isn't given never \
                 matches): Super+/ wouldn't run it there"
                    .into(),
            )
        }
        Some(_) => {}
    }
    let (patterns, given) = if !inputs.is_empty() {
        (inputs.to_vec(), true)
    } else if let Some(p) = stanza.filter(|p| !p.inputs.is_empty()) {
        (p.inputs.clone(), false)
    } else {
        (manifest.map(|m| m.inputs).unwrap_or_default(), false)
    };
    let mut skipped = Vec::new();
    let request = request(&patterns, &window, home, &mut skipped);
    for s in skipped {
        if given {
            r.add("request", format!("input {s}"), "not found");
        } else {
            r.notes
                .push(one_line(&format!("input {s} not found here; not sent")));
        }
    }
    match request {
        Ok(request) => {
            let (out, ms) = timed(&mut r, "run", Command::new(path), Some(request));
            r.run_ms = Some(ms);
            match out {
                Ok(out) => {
                    r.notes.push(format!("ran in {ms} ms, exit status 0"));
                    check_response(&out, &mut r);
                }
                Err(e) => r.add("run", "", e),
            }
        }
        Err(e) => r.add("request", "", e),
    }
    r.ok = r.problems.is_empty();
    r
}

/// Whether `run_for_input`'s error is a non-zero exit status: the program ran and said no (as in `ask_manifest`).
fn took_refused(e: &anyhow::Error) -> bool {
    e.to_string().contains(" failed (")
}

/// Each problem in a response, located by category, index and key: walked as JSON rather than parsed, so one
/// mistake doesn't hide the rest. The walk follows `Sheet` and `Bind`; whatever it lets through still goes through
/// `response`, so a schema change it misses can't pass a response Super+/ refuses.
fn check_response(out: &str, r: &mut Report) {
    use serde_json::Value;
    let before = r.problems.len();
    let v: Value = match serde_json::from_str(out) {
        Ok(v) => v,
        Err(e) => return r.add("response", "", format!("not JSON: {e}")),
    };
    let Some(top) = v.as_object() else {
        return r.add("response", "", "not a JSON object");
    };
    match top.get("version") {
        Some(n) if n.as_u64() == Some(VERSION.into()) => {}
        Some(n) => r.add("response", "version", format!("{n} is not {VERSION}")),
        None => r.add("response", "version", "missing"),
    }
    for field in ["title", "provider"] {
        if !top.get(field).is_some_and(Value::is_string) {
            r.add("response", field, "missing or not a string");
        }
    }
    let Some(binds) = top.get("binds").and_then(Value::as_object) else {
        return r.add("response", "binds", "missing or not an object");
    };
    for (category, list) in binds {
        let Some(list) = list.as_array() else {
            r.add(
                "response",
                format!("category {category:?}"),
                "not a list of bindings",
            );
            continue;
        };
        // A key twice in one category, in the same scope: canonical key and `when` → its first index.
        let mut seen = std::collections::HashMap::new();
        for (i, b) in list.iter().enumerate() {
            let Some(b) = b.as_object() else {
                r.add(
                    "response",
                    format!("category {category:?}, #{i}"),
                    "not an object",
                );
                continue;
            };
            let key = b.get("key").and_then(Value::as_str);
            let at = match key {
                Some(k) => format!("category {category:?}, #{i}, key {k:?}"),
                None => format!("category {category:?}, #{i}"),
            };
            let canonical = match key.map(str::parse::<keybind_find_sheet::Key>) {
                None => {
                    r.add("response", &at, "key: missing or not a string");
                    None
                }
                Some(Err(e)) => {
                    r.add("response", &at, format!("key: {e:#}"));
                    None
                }
                Some(Ok(k)) => Some(k.to_string()),
            };
            match b.get("desc") {
                Some(Value::String(d)) if d.trim().is_empty() => {
                    r.add("response", &at, "desc is empty")
                }
                Some(Value::String(_)) => {}
                _ => r.add("response", &at, "desc: missing or not a string"),
            }
            if b.get("action").is_some_and(|a| !a.is_string()) {
                r.add("response", &at, "action: not a string");
            }
            let when = b.get("when").map(|w| {
                serde_json::from_value::<keybind_find_sheet::Bind>(
                    serde_json::json!({"key": "", "desc": "", "when": w}),
                )
            });
            let scope = match when {
                None => Some("[]".to_string()),
                // Values are a set: sorted, so their order doesn't hide a duplicate.
                Some(Ok(bind)) => Some(format!(
                    "{:?}",
                    bind.when
                        .iter()
                        .map(|(k, v)| {
                            let mut v: Vec<&str> =
                                v.iter().map(keybind_find_sheet::Name::as_str).collect();
                            v.sort();
                            (k.as_str(), v)
                        })
                        .collect::<Vec<_>>()
                )),
                Some(Err(e)) => {
                    r.add("response", &at, format!("when: {e}"));
                    None
                }
            };
            if let (Some(k), Some(scope)) = (canonical, scope) {
                match seen.get(&(k.clone(), scope.clone())) {
                    Some(first) => r.add(
                        "response",
                        &at,
                        format!("the same key as #{first}, in the same scope"),
                    ),
                    None => {
                        seen.insert((k, scope), i);
                    }
                }
            }
        }
    }
    // What a walk of the parsed value can't see (a scope given twice in one `when`): the response as Super+/ reads it.
    if r.problems.len() == before {
        if let Err(e) = response(out) {
            r.add("response", "", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(
        class: Option<&str>,
        program: Option<&str>,
        title: Option<&str>,
    ) -> keybind_find_sheet::Context {
        keybind_find_sheet::Context {
            wm: Some("xmonad".into()),
            class: class.map(Into::into),
            title: title.map(Into::into),
            program: program.map(Into::into),
            facts: Default::default(),
        }
    }

    fn m(class: &[&str], program: &[&str], title: &[&str]) -> Match {
        let v = |l: &[&str]| (!l.is_empty()).then(|| l.iter().map(|s| s.to_string()).collect());
        Match {
            class: v(class),
            program: v(program),
            window_title: v(title),
        }
    }

    #[test]
    fn match_by_class_program_title_and_all() {
        let w = window(
            Some("Alacritty"),
            Some("btop"),
            Some("host: btop — 4 cores"),
        );
        assert!(matches(&m(&["alacritty"], &[], &[]), &w));
        assert!(matches(&m(&["kitty", "ALACRITTY"], &[], &[]), &w));
        assert!(!matches(&m(&["Alacritty-x"], &[], &[]), &w));
        assert!(matches(&m(&[], &["BTOP"], &[]), &w));
        assert!(!matches(&m(&[], &["bto"], &[]), &w));
        assert!(matches(&m(&[], &[], &["4 CORES"]), &w));
        assert!(!matches(&m(&[], &[], &["8 cores"]), &w));
        // Every key given must match.
        assert!(matches(&m(&["alacritty"], &["btop"], &["btop"]), &w));
        assert!(!matches(&m(&["alacritty"], &["nvim"], &["btop"]), &w));
        assert!(!matches(&m(&["kitty"], &["btop"], &[]), &w));
        // Nothing known about the window: a key given can't match.
        let none = window(None, None, None);
        assert!(!matches(&m(&["alacritty"], &[], &[]), &none));
        assert!(!matches(&m(&[], &["btop"], &[]), &none));
        // An empty list matches nothing.
        let empty = Match {
            class: Some(vec![]),
            ..Match::default()
        };
        assert!(!matches(&empty, &w));
    }

    fn plugin(name: &str, matches: Match) -> Plugin {
        Plugin {
            name: name.into(),
            program: "true".into(),
            inputs: vec![],
            matches: Some(matches),
        }
    }

    #[test]
    fn program_plugins_sit_with_the_inner_programs_others_after_sheets() {
        let plugins = [
            plugin("btop-keys", m(&[], &["btop"], &[])),
            plugin("gimp-keys", m(&["Gimp"], &[], &[])),
        ];
        let mut p = vec!["xmonad", "espanso", "alacritty", "sheets"];
        place(&mut p, &plugins);
        assert_eq!(
            p,
            [
                "xmonad",
                "espanso",
                "alacritty",
                "btop-keys",
                "sheets",
                "gimp-keys"
            ]
        );
        // No class, so no `sheets`: both go last, in config order.
        let mut p = vec!["xmonad", "espanso"];
        place(&mut p, &plugins);
        assert_eq!(p, ["xmonad", "espanso", "btop-keys", "gimp-keys"]);
    }

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/plugins")
            .join(name)
    }

    fn run_fixture(script: &str, inputs: Vec<String>) -> anyhow::Result<Sheet> {
        let p = Plugin {
            name: "test-plugin".into(),
            program: fixture(script).display().to_string(),
            inputs,
            matches: None,
        };
        // A short limit only for the fixture that hangs: the others mustn't flake on a busy runner.
        let timeout = if script == "hang.sh" {
            Duration::from_millis(300)
        } else {
            TIMEOUT
        };
        run(
            &p,
            &window(Some("Gimp"), Some("btop"), Some("a title")),
            &None,
            timeout,
        )
    }

    #[test]
    fn a_plugin_sheet_is_named_after_the_plugin_with_keys_checked() {
        let s = run_fixture("echo-sheet.sh", vec![]).unwrap();
        assert_eq!(s.provider, "test-plugin");
        let binds = &s.binds["Tools"];
        // `shift+ctrl+k` in the output, canonical here; the action is dropped.
        assert_eq!(binds[0].key, "ctrl+shift+k");
        assert_eq!(binds[0].desc, "do a thing");
        assert_eq!(binds[0].action, "");
    }

    #[test]
    fn the_request_carries_the_window_and_the_inputs() {
        let dir = scratch("request");
        std::fs::write(dir.join("b.conf"), "second").unwrap();
        std::fs::write(dir.join("a.conf"), "first").unwrap();
        std::fs::write(dir.join("a.txt"), "not matched").unwrap();
        let glob = format!("{}/*.conf", dir.display());
        let s = run_fixture("echo-request.sh", vec![glob]).unwrap();
        let descs: Vec<&str> = s.binds["Request"].iter().map(|b| b.desc.as_str()).collect();
        // The version, each input's path and text in sorted order, and the window.
        let (a, b) = (dir.join("a.conf"), dir.join("b.conf"));
        let (a, b) = (a.display().to_string(), b.display().to_string());
        assert_eq!(
            descs,
            [
                "version 1",
                &a,
                "first",
                &b,
                "second",
                "xmonad",
                "Gimp",
                "a title",
                "btop"
            ]
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kbf-plugins-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_failing_plugin_is_an_error_naming_what_went_wrong() {
        for (script, want) in [
            ("crash.sh", "exit status: 3"),
            ("hang.sh", "did not answer within"),
            ("wrong-version.sh", "response version 2 is not 1"),
            ("bad-key.sh", "a key is not in the key notation"),
            ("not-json.sh", "its output is not a response"),
        ] {
            let e = format!("{:#}", run_fixture(script, vec![]).unwrap_err());
            assert!(e.contains(want), "{script}: {e}");
        }
        let missing = Plugin {
            name: "x".into(),
            program: "/nonexistent/kbf-plugin".into(),
            inputs: vec![],
            matches: None,
        };
        assert!(run(&missing, &window(None, None, None), &None, TIMEOUT).is_err());
    }

    #[test]
    fn inputs_missing_are_skipped_and_too_big_is_an_error() {
        let dir = scratch("inputs");
        // A missing file and a glob that matches nothing: skipped, the plugin still runs.
        let missing = vec![
            format!("{}/nope.conf", dir.display()),
            format!("{}/none-*.conf", dir.display()),
            format!("{}/no-such-dir/*.conf", dir.display()),
        ];
        let s = run_fixture("echo-request.sh", missing).unwrap();
        // Only the version and the window's four fields: no input reached it.
        assert_eq!(s.binds["Request"].len(), 5);
        let big = dir.join("big.conf");
        std::fs::write(&big, vec![b'x'; (INPUT_LIMIT + 1) as usize]).unwrap();
        let e = format!(
            "{:#}",
            run_fixture("echo-sheet.sh", vec![big.display().to_string()]).unwrap_err()
        );
        assert!(e.contains("over 1 MiB"), "{e}");
        // Exactly the limit is fine.
        std::fs::write(&big, vec![b'x'; INPUT_LIMIT as usize]).unwrap();
        assert!(run_fixture("echo-sheet.sh", vec![big.display().to_string()]).is_ok());
        // Nine of them are over the limit for all inputs together.
        for i in 0..8 {
            std::fs::copy(&big, dir.join(format!("big{i}.conf"))).unwrap();
        }
        let all = format!("{}/big*.conf", dir.display());
        let e = format!("{:#}", run_fixture("echo-sheet.sh", vec![all]).unwrap_err());
        assert!(e.contains("is over 8 MiB"), "{e}");
        // A directory, or a FIFO (opening it would wait for a writer), is an error, not a wait.
        let one = |p: &Path| run_fixture("echo-sheet.sh", vec![p.display().to_string()]);
        let e = format!("{:#}", one(&dir).unwrap_err());
        assert!(e.contains("not a regular file"), "{e}");
        let fifo = dir.join("fifo");
        if Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .is_ok_and(|s| s.success())
        {
            let e = format!("{:#}", one(&fifo).unwrap_err());
            assert!(e.contains("not a regular file"), "{e}");
        }
    }

    /// The only test that sets `ACTIVE` (it is once per process), besides the ignored live check `live_window`:
    /// don't run that one with this.
    #[test]
    fn a_matching_plugin_is_a_provider_the_registry_runs() {
        let plugin = |name: &str, script: &str, matches| Plugin {
            name: name.into(),
            program: fixture(script).display().to_string(),
            inputs: vec![],
            matches,
        };
        let all = vec![
            plugin("gimp-echo", "echo-sheet.sh", Some(m(&["gimp"], &[], &[]))),
            plugin("kitty-echo", "echo-sheet.sh", Some(m(&["kitty"], &[], &[]))),
            plugin("never", "crash.sh", None),
        ];
        let all_again = all.clone();
        let mut providers = vec!["xmonad", "espanso", "sheets"];
        add_from(&mut providers, || all, window(Some("Gimp"), None, None));
        assert_eq!(providers, ["xmonad", "espanso", "sheets", "gimp-echo"]);
        let ctx = Context {
            window_id: None,
            class: Some("Gimp".into()),
            instance: None,
            title: None,
            providers: providers.clone(),
        };
        let sheet = crate::providers::sheet("gimp-echo", &ctx).unwrap();
        assert_eq!(sheet.provider, "gimp-echo");
        // Names compare ignoring case.
        assert!(sheet_with("GIMP-echo", || panic!("config read"))
            .unwrap()
            .is_ok());
        // Configured but not matched here (`dump <name>` from another window): it says so.
        let configured = || all_again.clone();
        for name in ["kitty-echo", "never"] {
            let e = sheet_with(name, configured)
                .unwrap()
                .unwrap_err()
                .to_string();
            assert!(e.contains("doesn't apply to the focused window"), "{e}");
        }
        // Not configured at all: not a plugin's name.
        assert!(sheet_with("nosuch", configured).is_none());
        // A second call adds nothing and doesn't read the config.
        let mut again = vec!["sheets"];
        add_from(
            &mut again,
            || panic!("config read twice"),
            window(None, None, None),
        );
        assert_eq!(again, ["sheets"]);
    }

    #[test]
    fn tilde_and_star_expand() {
        let home = Path::new("/home/u");
        assert_eq!(tilde("~/x/y", Some(home)), PathBuf::from("/home/u/x/y"));
        assert_eq!(tilde("~", Some(home)), PathBuf::from("/home/u"));
        assert_eq!(tilde("~/x", None), PathBuf::from("~/x"));
        assert_eq!(tilde("/a/~/b", Some(home)), PathBuf::from("/a/~/b"));
        let dir = scratch("glob");
        for f in ["c.lua", "a.lua", "b.vim", "init.lua"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        std::fs::create_dir(dir.join("dir.lua")).unwrap();
        let names = |p: &str| -> Vec<String> {
            expand(p, None)
                .unwrap()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        let d = dir.display();
        // Sorted, files only, prefix and suffix around the star.
        assert_eq!(names(&format!("{d}/*.lua")), ["a.lua", "c.lua", "init.lua"]);
        assert_eq!(names(&format!("{d}/i*")), ["init.lua"]);
        assert_eq!(
            names(&format!("{d}/*")),
            ["a.lua", "b.vim", "c.lua", "init.lua"]
        );
        // `a.lua*a.lua` needs the name to hold both parts, not overlap them.
        assert!(names(&format!("{d}/a.lua*a.lua")).is_empty());
        // No star: the path itself, existing or not.
        assert_eq!(names(&format!("{d}/zzz")), ["zzz"]);
    }

    /// A fake environment under a fresh directory: `bin` (on PATH), `bin2` (later on PATH), `home`, config and
    /// cache homes.
    fn env_in(name: &str) -> (Env, PathBuf) {
        let d = scratch(name);
        for sub in [
            "bin",
            "bin2",
            "home",
            "config/keybind-find/plugins",
            "cache",
        ] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        let env = Env {
            path: Some(std::env::join_paths([d.join("bin"), d.join("bin2")]).unwrap()),
            home: Some(d.join("home")),
            config_home: Some(d.join("config")),
            cache_home: Some(d.join("cache")),
        };
        (env, d)
    }

    /// Writes an executable `sh` script that prints `manifest` for `--manifest` (counting each ask in
    /// `<path>.asked`), else an empty sheet.
    fn plugin_script(path: &Path, manifest: &str) {
        write_script(
            path,
            &format!(
                "if [ \"$1\" = --manifest ]; then echo x >> \"$0.asked\"; cat <<'EOF'\n{manifest}\nEOF\nexit 0; fi\n\
                 cat >/dev/null\necho '{{\"version\": 1, \"title\": \"t\", \"provider\": \"p\", \"binds\": {{}}}}'"
            ),
        );
    }

    /// Writes an executable `sh` script with `body`.
    fn write_script(path: &Path, body: &str) {
        use std::io::Write;
        let script = format!("#!/bin/sh\n{body}\n");
        // Written by `sh`, not by this process: a file this process held open for writing could be inherited by a
        // child another test thread forks at that moment, and running the script would then fail with ETXTBSY.
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(sh.wait().unwrap().success());
    }

    const KAKOUNE: &str = r#"{"version": 1, "name": "kakoune", "description": "Kakoune's keymap", "inputs":
        ["~/.config/kak/kakrc"], "match": {"program": ["kak"]}, "queries": [],
        "homepage": "https://example.com/kakoune"}"#;

    fn asked(path: &Path) -> usize {
        std::fs::read_to_string(format!("{}.asked", path.display()))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn a_manifest_is_asked_once_per_file_version() {
        let (env, d) = env_in("cache");
        let p = d.join("bin/keybind-find-kakoune");
        plugin_script(&p, KAKOUNE);
        let cache = env.cache_file().unwrap();
        let m = manifest(&p, Some(&cache), ask_manifest).unwrap();
        assert_eq!((m.name.as_str(), m.inputs.len()), ("kakoune", 1));
        assert_eq!(m.matches.unwrap().program, Some(vec!["kak".to_string()]));
        // A hit: the plugin isn't run again.
        manifest(&p, Some(&cache), |_| panic!("asked on a cache hit")).unwrap();
        assert_eq!(asked(&p), 1);
        // A new modification time (the plugin was reinstalled): asked again.
        // By `touch`, not a write handle here (see `plugin_script`).
        let touched = Command::new("touch")
            .args(["-m", "-d", "@2000000000"])
            .arg(&p)
            .status();
        assert!(touched.unwrap().success());
        manifest(&p, Some(&cache), ask_manifest).unwrap();
        assert_eq!(asked(&p), 2);
        // A program with no manifest is remembered too, and still an error.
        let q = d.join("bin/other");
        plugin_script(&q, "not json");
        assert!(manifest(&q, Some(&cache), ask_manifest).is_err());
        let e = manifest(&q, Some(&cache), |_| panic!("asked on a cache hit")).unwrap_err();
        assert!(format!("{e:#}").contains("not a manifest"), "{e:#}");
        // No answer (a timeout, a failed start) is remembered for an hour only.
        let r = d.join("bin/slow");
        plugin_script(&r, KAKOUNE);
        let e = manifest(&r, Some(&cache), |_| anyhow::bail!("did not answer")).unwrap_err();
        assert!(
            e.to_string().starts_with("no answer: did not answer"),
            "{e}"
        );
        assert!(manifest(&r, Some(&cache), |_| panic!("asked within the hour")).is_err());
        let mut entries: std::collections::BTreeMap<String, Cached> =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        for c in entries.values_mut().filter(|c| c.retry_after.is_some()) {
            c.retry_after = Some(1);
        }
        std::fs::write(&cache, serde_json::to_vec(&entries).unwrap()).unwrap();
        assert_eq!(
            manifest(&r, Some(&cache), ask_manifest).unwrap().name,
            "kakoune"
        );
        // An entry written by another keybind-find version is asked again.
        for c in entries.values_mut() {
            c.keybind_find = "0.0.0".into();
        }
        std::fs::write(&cache, serde_json::to_vec(&entries).unwrap()).unwrap();
        let before = asked(&r);
        manifest(&r, Some(&cache), ask_manifest).unwrap();
        assert_eq!(asked(&r), before + 1);
        // A file of the same mtime but another size (copied with its time kept) is new.
        let touched = Command::new("sh")
            .args([
                "-c",
                "echo '# more' >> \"$1\" && touch -m -d @2000000000 \"$1\"",
                "sh",
            ])
            .arg(&p)
            .status();
        assert!(touched.unwrap().success());
        manifest(&p, Some(&cache), ask_manifest).unwrap();
        assert_eq!(asked(&p), 3);
        // No cache (or an unreadable one): just asked.
        manifest(&p, None, ask_manifest).unwrap();
        std::fs::write(&cache, "garbage").unwrap();
        manifest(&p, Some(&cache), ask_manifest).unwrap();
        assert_eq!(asked(&p), 5);
    }

    #[test]
    fn a_bad_manifest_is_refused() {
        let (_, d) = env_in("bad-manifest");
        let p = d.join("bin/keybind-find-x");
        for (manifest, want) in [
            (r#"{"version": 2, "name": "x"}"#, "manifest version 2"),
            (r#"{"version": 1, "name": "nvim"}"#, "keybind-find's own"),
            (r#"{"version": 1, "name": " x"}"#, "not a plugin name"),
            (
                r#"{"version": 1, "name": "x", "inputs": ["rel/conf"]}"#,
                "manifest input",
            ),
            (
                r#"{"version": 1, "name": "x", "match": {"class": []}}"#,
                "manifest match class",
            ),
            (r#"{"name": "x"}"#, "not a manifest"),
            // A later version's shape is named as a later version, not as junk.
            (r#"{"version": 2, "name": ["x"]}"#, "manifest version 2"),
        ] {
            plugin_script(&p, manifest);
            let e = ask_manifest(&p).unwrap().unwrap_err();
            assert!(e.contains(want), "{manifest}: {e}");
        }
        // An empty match is no match (it would otherwise match every window).
        plugin_script(&p, r#"{"version": 1, "name": "x", "match": {}}"#);
        assert_eq!(ask_manifest(&p).unwrap().unwrap().matches, None);
        // A non-zero exit is an answer (a plugin built on the crate's `run` exits 1 for `--manifest`); a program that
        // can't start is not.
        let refuses = d.join("bin/refuses");
        let script = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\necho no >&2\\nexit 1\\n' > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(&refuses)
            .status();
        assert!(script.unwrap().success());
        let e = ask_manifest(&refuses).unwrap().unwrap_err();
        assert!(e.contains("exit status: 1"), "{e}");
        let e = ask_manifest(&d.join("bin/nosuch")).unwrap_err();
        assert!(format!("{e:#}").contains("running"), "{e:#}");
    }

    #[test]
    fn a_stanza_takes_the_defaults_it_leaves_out() {
        let manifest =
            || -> anyhow::Result<keybind_find_sheet::Manifest> { Ok(serde_json::from_str(KAKOUNE)?) };
        let stanza = |inputs: &[&str], matches: Option<Match>| Plugin {
            name: "kakoune".into(),
            program: "keybind-find-kakoune".into(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            matches,
        };
        let mine = m(&["kitty"], &[], &[]);
        let got = with_defaults(
            vec![
                stanza(&[], None),
                stanza(&["~/mine.toml"], None),
                stanza(&[], Some(mine.clone())),
                stanza(&["~/mine.toml"], Some(mine.clone())),
            ],
            |_| manifest(),
        );
        let kak = Some(m(&[], &["kak"], &[]));
        let theirs = vec!["~/.config/kak/kakrc".to_string()];
        assert_eq!(
            (got[0].inputs.clone(), got[0].matches.clone()),
            (theirs.clone(), kak.clone())
        );
        assert_eq!(
            (got[1].inputs.clone(), got[1].matches.clone()),
            (vec!["~/mine.toml".into()], kak)
        );
        assert_eq!(
            (got[2].inputs.clone(), got[2].matches.clone()),
            (theirs, Some(mine.clone()))
        );
        assert_eq!(got[3], stanza(&["~/mine.toml"], Some(mine.clone())));
        // A full stanza never asks; one with no manifest stays as written.
        let full = with_defaults(vec![stanza(&["~/a"], Some(mine.clone()))], |_| {
            panic!("asked")
        });
        assert_eq!(full.len(), 1);
        let none = with_defaults(vec![stanza(&[], None)], |_| anyhow::bail!("no manifest"));
        assert_eq!(none[0], stanza(&[], None));
    }

    #[test]
    fn a_normal_run_reuses_the_manifests_plugin_list_read() {
        let (env, d) = env_in("normal-run");
        let p = d.join("bin/keybind-find-kakoune");
        plugin_script(&p, KAKOUNE);
        // `plugin list` (or `add`) reads it once...
        assert_eq!(found(&env).len(), 1);
        assert_eq!(asked(&p), 1);
        // ...and a run with a stanza that leaves both keys out gets the defaults without asking again.
        let stanza = Plugin {
            name: "kakoune".into(),
            program: "keybind-find-kakoune".into(),
            inputs: vec![],
            matches: None,
        };
        let got = configured_in(&env, vec![stanza]);
        assert_eq!(got[0].inputs, ["~/.config/kak/kakrc"]);
        assert_eq!(got[0].matches, Some(m(&[], &["kak"], &[])));
        assert_eq!(asked(&p), 1);
    }

    #[test]
    fn plugins_are_found_on_path_and_in_the_plugins_folder() {
        use std::os::unix::fs::PermissionsExt;
        let (env, d) = env_in("discover");
        plugin_script(&d.join("bin/keybind-find-kakoune"), KAKOUNE);
        plugin_script(&d.join("bin2/keybind-find-kakoune"), KAKOUNE);
        plugin_script(&d.join("bin2/keybind-find-btop"), KAKOUNE);
        plugin_script(&d.join("bin/other"), KAKOUNE);
        plugin_script(&d.join("config/keybind-find/plugins/mine"), KAKOUNE);
        // Not executable, or just the prefix: not plugins.
        std::fs::write(d.join("bin/keybind-find-notes"), "").unwrap();
        plugin_script(&d.join("bin/keybind-find-"), KAKOUNE);
        std::fs::set_permissions(
            d.join("bin/keybind-find-notes"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let rel = |p: PathBuf| p.strip_prefix(&d).unwrap().display().to_string();
        let found: Vec<String> = env.discover().into_iter().map(rel).collect();
        // PATH order, the first of a name only, then the folder.
        assert_eq!(
            found,
            [
                "bin/keybind-find-kakoune",
                "bin2/keybind-find-btop",
                "config/keybind-find/plugins/mine"
            ]
        );
        assert_eq!(
            env.resolve("keybind-find-btop"),
            Some(d.join("bin2/keybind-find-btop"))
        );
        assert_eq!(env.resolve("~/x"), Some(d.join("home/x")));
        assert_eq!(env.resolve("nosuch"), None);
        // An empty environment finds nothing and doesn't fail.
        let empty = Env {
            path: None,
            home: None,
            config_home: None,
            cache_home: None,
        };
        assert!(empty.discover().is_empty());
    }

    #[test]
    fn plugin_list_says_what_is_enabled() {
        let (env, d) = env_in("list");
        plugin_script(&d.join("bin/keybind-find-kakoune"), KAKOUNE);
        plugin_script(&d.join("bin/keybind-find-broken"), "oops");
        let stanza = Plugin {
            name: "kak".into(),
            program: "keybind-find-kakoune".into(),
            inputs: vec![],
            matches: None,
        };
        let text = list_text(&env, &[stanza]);
        let bin = d.join("bin");
        assert_eq!(
            text,
            format!(
                "broken\tnot in the config\t{b}/keybind-find-broken\n    no manifest: its --manifest output is not a \
                 manifest: expected value at line 1 column 1\n\
                 kakoune\tenabled as \"kak\"\t{b}/keybind-find-kakoune\n    Kakoune's keymap\n    inputs: \
                 ~/.config/kak/kakrc\n    match: program kak\n    homepage: https://example.com/kakoune",
                b = bin.display()
            )
        );
        let (empty, _) = env_in("list-empty");
        assert!(list_text(&empty, &[]).starts_with("no plugins found"));
    }

    #[test]
    fn plugin_add_appends_a_stanza_and_keeps_what_is_there() {
        let (env, d) = env_in("add");
        plugin_script(&d.join("bin/keybind-find-kakoune"), KAKOUNE);
        plugin_script(
            &d.join("config/keybind-find/plugins/mine"),
            r#"{"version": 1, "name": "mine", "description": "line one\nline two"}"#,
        );
        // A missing config (and folder) is created.
        let config = d.join("fresh/keybind-find/config.toml");
        let out = add_stanza(&env, "kakoune", &config).unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        // The program is a path (Super+/'s PATH may not hold it), here outside the fake home so absolute.
        assert_eq!(
            text,
            format!(
                "# kakoune: Kakoune's keymap\n# https://example.com/kakoune\n# Added by `keybind-find plugin add`, with the \
                 plugin's defaults: edit them freely.\n[[plugin]]\nname = \"kakoune\"\nprogram = \"{}\"\n\
                 inputs = [\"~/.config/kak/kakrc\"]\n[plugin.match]\nprogram = [\"kak\"]\n",
                d.join("bin/keybind-find-kakoune").display()
            )
        );
        assert!(
            out.starts_with(&format!("added to {}:\n\n# kakoune", config.display())),
            "{out}"
        );
        // A second add of the same name is refused and changes nothing (ignoring case, as the config does).
        let e = add_stanza(&env, "KAKOUNE", &config).unwrap_err().to_string();
        assert!(
            e.contains("already has a [[plugin]] named \"kakoune\""),
            "{e}"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        // An existing config keeps every byte, comments and order included; the folder plugin is named by path and
        // has no default match.
        let existing = "# my keys\n[keys]\nundo = \"Delete\"  # mine";
        std::fs::write(&config, existing).unwrap();
        add_stanza(&env, "mine", &config).unwrap();
        let after = std::fs::read_to_string(&config).unwrap();
        assert!(
            after.starts_with(&format!("{existing}\n\n# mine: line one line two\n")),
            "{after}"
        );
        let doc: toml::Value = toml::from_str(&after).unwrap();
        let p = &doc["plugin"][0];
        let path = d
            .join("config/keybind-find/plugins/mine")
            .display()
            .to_string();
        assert_eq!(p["program"].as_str(), Some(path.as_str()));
        assert_eq!(p["inputs"].as_array().map(Vec::len), Some(0));
        assert!(p.get("match").is_none());
        assert!(after.contains("# No default match"));
        assert_eq!(doc["keys"]["undo"].as_str(), Some("Delete"));
        // The same program under another name is refused too: its binds would show twice.
        std::fs::write(
            &config,
            "[[plugin]]\nname = \"kak\"\nprogram = \"keybind-find-kakoune\"\n",
        )
        .unwrap();
        let e = add_stanza(&env, "kakoune", &config).unwrap_err().to_string();
        assert!(e.contains("already runs") && e.contains("as \"kak\""), "{e}");
        // A `plugin` that isn't [[plugin]] stanzas: appending would break the whole config, so nothing is written.
        for text in [
            "plugin = []\n",
            "plugin = [{ name = \"a\", program = \"b\" }]\n",
            "[plugin]\nname = \"a\"\n",
        ] {
            std::fs::write(&config, text).unwrap();
            let e = add_stanza(&env, "kakoune", &config).unwrap_err().to_string();
            assert!(e.contains("can't append"), "{text}: {e}");
            assert_eq!(std::fs::read_to_string(&config).unwrap(), text);
        }
        // Under home, the program is written as `~/`.
        std::fs::remove_file(&config).unwrap();
        let homed = Env {
            path: Some(d.join("bin").into_os_string()),
            home: Some(d.clone()),
            config_home: None,
            cache_home: None,
        };
        add_stanza(&homed, "kakoune", &config).unwrap();
        let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            doc["plugin"][0]["program"].as_str(),
            Some("~/bin/keybind-find-kakoune")
        );
        // Unknown names, and a config that isn't TOML, are refused.
        assert!(add_stanza(&env, "nosuch", &config)
            .unwrap_err()
            .to_string()
            .contains("no installed plugin"));
        std::fs::write(&config, "[keys\n").unwrap();
        let e = add_stanza(&env, "kakoune", &config).unwrap_err().to_string();
        assert!(e.contains("not valid TOML"), "{e}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "[keys\n");
    }

    /// `check_at` on `path` with no stanza or window, as report lines.
    fn check_lines(path: &Path, inputs: &[String], limit: Duration) -> (Report, Vec<String>) {
        let r = check_at(path, None, inputs, Default::default(), &None, limit);
        let lines = r.text().lines().map(str::to_string).collect();
        (r, lines)
    }

    /// The report's problem lines and summary, without the first line and the notes.
    fn problem_lines(lines: &[String]) -> Vec<&String> {
        lines[1..]
            .iter()
            .filter(|l| !l.starts_with("note: "))
            .collect()
    }

    #[test]
    fn check_plugin_reports_each_fixtures_problems() {
        let no_manifest = "manifest: its --manifest output is not a manifest";
        let empty =
            |i: usize| format!(r#"response: category "Request", #{i}, key "c": desc is empty"#);
        let twice = |i: usize| {
            format!(
                r#"response: category "Request", #{i}, key "c": the same key as #1, in the same scope"#
            )
        };
        for (script, want) in [
            (
                "bad-key.sh",
                vec![
                    no_manifest.to_string(),
                    r#"response: category "A", #0, key "hyper+k": key: "hyper+k": "hyper" is not a modifier"#.into(),
                    "2 problems".into(),
                ],
            ),
            // `--manifest` refused (a note, see below), then the run fails.
            ("crash.sh", vec!["run: ".into(), "1 problem".into()]),
            ("echo-sheet.sh", vec![no_manifest.into(), "1 problem".into()]),
            (
                "not-json.sh",
                vec![
                    no_manifest.into(),
                    "response: not JSON: expected value at line 1 column 1".into(),
                    "2 problems".into(),
                ],
            ),
            (
                "wrong-version.sh",
                vec![
                    "manifest: manifest version 2 is not 1".into(),
                    "response: version: 2 is not 1".into(),
                    "2 problems".into(),
                ],
            ),
            // No window: the version and the wm, class, title and program (all empty) under one key.
            (
                "echo-request.sh",
                vec![
                    no_manifest.into(),
                    empty(1),
                    empty(2),
                    twice(2),
                    empty(3),
                    twice(3),
                    empty(4),
                    twice(4),
                    "8 problems".into(),
                ],
            ),
        ] {
            let (r, lines) = check_lines(&fixture(script), &[], TIMEOUT);
            assert!(!r.ok, "{script}");
            assert_eq!(lines[0], format!("checked {}", fixture(script).display()));
            let problems = problem_lines(&lines);
            assert_eq!(problems.len(), want.len(), "{script}: {lines:#?}");
            for (got, w) in problems.iter().zip(&want) {
                assert!(got.starts_with(w.as_str()), "{script}: {lines:#?}");
            }
        }
        // Exit status and stderr of a failed run; a refused `--manifest` is a note.
        let (_, lines) = check_lines(&fixture("crash.sh"), &[], TIMEOUT);
        assert!(lines[1].starts_with("note: no manifest ("), "{lines:#?}");
        let run = problem_lines(&lines)[0];
        assert!(
            run.ends_with("failed (exit status: 3): parse error"),
            "{lines:#?}"
        );
        // Run time against the limit: a hang is killed at twice the limit, and says which limit it broke.
        let (r, lines) = check_lines(&fixture("hang.sh"), &[], Duration::from_millis(100));
        assert_eq!(r.problems.len(), 2, "{lines:#?}");
        let problems = problem_lines(&lines);
        assert!(
            problems[0].starts_with("manifest: over the 0.1 s limit: ")
                && problems[0].ends_with("did not answer within 0.2 s"),
            "{lines:#?}"
        );
        assert!(
            problems[1].starts_with("run: over the 0.1 s limit"),
            "{lines:#?}"
        );
        // Not an executable file: one problem, nothing run.
        let (r, lines) = check_lines(Path::new("/nonexistent/kbf-plugin"), &[], TIMEOUT);
        assert_eq!(problem_lines(&lines)[0], "plugin: not an executable file");
        assert_eq!((r.problems.len(), r.manifest_ms), (1, None));
        // A version 1 manifest without a required field.
        let (_, d) = env_in("check-fields");
        let p = d.join("bin/nameless");
        plugin_script(&p, r#"{"version": 1}"#);
        let (_, lines) = check_lines(&p, &[], TIMEOUT);
        assert!(
            problem_lines(&lines)[0].contains("missing field `name`"),
            "{lines:#?}"
        );
    }

    #[test]
    fn check_plugin_passes_a_good_plugin_and_checks_its_inputs() {
        let (_, d) = env_in("check-good");
        let p = d.join("bin/keybind-find-kakoune");
        plugin_script(&p, KAKOUNE);
        // The default input isn't there (no home): a note, not a problem.
        let (r, lines) = check_lines(&p, &[], TIMEOUT);
        assert!(r.ok, "{lines:#?}");
        assert_eq!(lines.last().unwrap(), "ok: no problems");
        assert!(
            lines.contains(
                &"note: input ~/.config/kak/kakrc not found here; not sent".into()
            ),
            "{lines:#?}"
        );
        assert!(r.manifest_ms.is_some() && r.run_ms.is_some());
        // An input given that isn't there is a problem; one that is, is sent.
        let missing = d.join("nope.toml").display().to_string();
        let (r, lines) = check_lines(&p, std::slice::from_ref(&missing), TIMEOUT);
        assert!(!r.ok);
        assert!(
            lines.contains(&format!("request: input {missing}: not found")),
            "{lines:#?}"
        );
        let conf = d.join("config.toml");
        std::fs::write(&conf, "x").unwrap();
        assert!(check_lines(&p, &[conf.display().to_string()], TIMEOUT).0.ok);
        // An answer that comes, but late, is measured and reported. Killed only at twice the limit, so a busy
        // runner has most of a second to spare.
        let slow = d.join("bin/slow");
        write_script(&slow, &format!("sleep 1.2\ncat <<'EOF'\n{KAKOUNE}\nEOF"));
        let (r, lines) = check_lines(&slow, &[], Duration::from_secs(1));
        assert!(
            r.problems.iter().any(|p| p.part == "manifest"
                && p.message.starts_with("took ")
                && p.message.ends_with("over the 1.0 s limit")),
            "{lines:#?}"
        );
    }

    /// A plugin with `manifest` that echoes its request (`echo-request.sh`): what it was sent, as descriptions.
    fn echo_plugin(path: &Path, manifest: &str) {
        write_script(
            path,
            &format!(
                "if [ \"$1\" = --manifest ]; then cat <<'EOF'\n{manifest}\nEOF\nexit 0; fi\nexec {}",
                fixture("echo-request.sh").display()
            ),
        );
    }

    fn sent(r: &Report) -> Vec<String> {
        r.problems
            .iter()
            .filter(|p| p.message == "desc is empty")
            .map(|p| p.at.clone())
            .collect()
    }

    #[test]
    fn check_plugin_sends_the_window_and_the_inputs() {
        let (_, d) = env_in("check-sent");
        let home = d.join("home");
        std::fs::write(home.join("a.conf"), "first").unwrap();
        std::fs::write(home.join("b.conf"), "second").unwrap();
        let p = d.join("bin/echo");
        echo_plugin(
            &p,
            r#"{"version": 1, "name": "echo", "inputs": ["~/a.conf"], "match": {"class": ["Gimp"]}}"#,
        );
        let window = keybind_find_sheet::Context {
            class: Some("Gimp".into()),
            program: Some("btop".into()),
            title: Some("a title".into()),
            ..Default::default()
        };
        // The window given: only the wm (not settable) is still empty, at #3 after the default input's two.
        let r = check_at(&p, None, &[], window.clone(), &Some(home.clone()), TIMEOUT);
        assert_eq!(sent(&r), [r#"category "Request", #3, key "c""#], "{r:#?}");
        let echoed = |r: &Report| {
            r.problems
                .iter()
                .filter(|p| p.message.contains("the same key"))
                .count()
        };
        // version, a.conf's path and text, then the four window fields: duplicates of `c` from #4 on.
        assert_eq!(echoed(&r), 3, "{r:#?}");
        // Two inputs given: both sent (two more `p`/`t` pairs), not the default.
        let two = [home.join("a.conf"), home.join("b.conf")].map(|p| p.display().to_string());
        let r = check_at(&p, None, &two, window.clone(), &Some(home.clone()), TIMEOUT);
        assert_eq!(sent(&r), [r#"category "Request", #5, key "c""#], "{r:#?}");
        // A stanza's inputs win over the manifest's; `--input` wins over both.
        let stanza = Plugin {
            name: "echo".into(),
            program: p.display().to_string(),
            inputs: vec!["~/b.conf".into(), "~/a.conf".into()],
            matches: None,
        };
        let r = check_at(
            &p,
            Some(&stanza),
            &[],
            window.clone(),
            &Some(home.clone()),
            TIMEOUT,
        );
        assert_eq!(sent(&r), [r#"category "Request", #5, key "c""#], "{r:#?}");
        // A window the match doesn't take: a note.
        let kitty = keybind_find_sheet::Context {
            class: Some("kitty".into()),
            ..Default::default()
        };
        let r = check_at(&p, None, &[], kitty, &Some(home), TIMEOUT);
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("doesn't take this window")),
            "{r:#?}"
        );
    }

    #[test]
    fn check_plugin_locates_each_problem_in_a_response() {
        let problems = |out: &str| {
            let mut r = Report::new(Path::new("/x"), TIMEOUT);
            check_response(out, &mut r);
            r.problems
                .iter()
                .map(|p| format!("{} {}", p.at, p.message))
                .collect::<Vec<_>>()
        };
        assert_eq!(problems("[]"), [" not a JSON object"]);
        assert_eq!(
            problems(r#"{"version": "1", "provider": 3, "binds": []}"#),
            [
                "version \"1\" is not 1",
                "title missing or not a string",
                "provider missing or not a string",
                "binds missing or not an object"
            ]
        );
        let got = problems(
            r#"{"version": 1, "title": "t", "provider": "p", "binds": {
                "A": {},
                "B": [
                    "ctrl+k",
                    {"key": "shift+ctrl+k", "desc": "one"},
                    {"key": "ctrl+shift+k", "desc": "same key, other spelling"},
                    {"key": "ctrl+shift+k", "desc": "other scope", "when": {"mode": "insert"}},
                    {"key": "ctrl+shift+k", "desc": "same scope", "when": {"mode": ["insert"]}},
                    {"desc": "no key"},
                    {"key": "x", "desc": " ", "action": 1},
                    {"key": "y", "desc": "bad when", "when": {"mode": 3}},
                    {"key": "z", "desc": "two modes", "when": {"mode": ["insert", "normal"]}},
                    {"key": "z", "desc": "the same, other order", "when": {"mode": ["normal", "insert"]}},
                    {"key": "q", "desc": "no when"},
                    {"key": "q", "desc": "an empty when is none", "when": {}}
                ]}}"#,
        );
        assert_eq!(got.len(), 10, "{got:#?}");
        assert_eq!(got[0], r#"category "A" not a list of bindings"#);
        assert_eq!(got[1], r#"category "B", #0 not an object"#);
        assert_eq!(
            got[2],
            r#"category "B", #2, key "ctrl+shift+k" the same key as #1, in the same scope"#
        );
        assert_eq!(
            got[3],
            r#"category "B", #4, key "ctrl+shift+k" the same key as #3, in the same scope"#
        );
        assert_eq!(got[4], r#"category "B", #5 key: missing or not a string"#);
        assert_eq!(got[5], r#"category "B", #6, key "x" desc is empty"#);
        assert_eq!(got[6], r#"category "B", #6, key "x" action: not a string"#);
        assert!(
            got[7].starts_with(r#"category "B", #7, key "y" when: "#),
            "{got:#?}"
        );
        assert_eq!(
            got[8],
            r#"category "B", #9, key "z" the same key as #8, in the same scope"#
        );
        assert_eq!(
            got[9],
            r#"category "B", #11, key "q" the same key as #10, in the same scope"#
        );
        // A scope given twice is lost in a parsed value: the response as Super+/ reads it catches it.
        let twice = problems(
            r#"{"version": 1, "title": "t", "provider": "p", "binds": {"A": [
                {"key": "x", "desc": "d", "when": {"mode": "insert", "mode": "normal"}}]}}"#,
        );
        assert_eq!(twice.len(), 1);
        assert!(twice[0].contains("given twice"), "{twice:#?}");
    }

    #[test]
    fn check_plugin_arguments_json_and_names() {
        for (args, want) in [
            (&["--class"][..], "--class needs a value"),
            (&["x", "--bogus"], "unknown option --bogus"),
            (&["a/x", "b/y"], "one plugin at a time"),
            (&["--json"], "which plugin?"),
        ] {
            let e = check(args).unwrap_err().to_string();
            assert!(e.contains(want) && e.contains("usage:"), "{e}");
        }
        let (_, d) = env_in("check-json");
        let p = d.join("keybind-find-quiet");
        plugin_script(&p, r#"{"version": 1, "name": "quiet"}"#);
        let path = p.display().to_string();
        let (out, ok) = check(&[&path, "--json", "--class", "Gimp"]).unwrap();
        assert!(ok);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            (v["ok"].as_bool(), v["limit_ms"].as_u64()),
            (Some(true), Some(3000))
        );
        assert_eq!(v["problems"], serde_json::json!([]));
        // With problems: each one's part, location and message.
        let bad = fixture("bad-key.sh").display().to_string();
        let (out, ok) = check(&[&bad, "--json"]).unwrap();
        assert!(!ok);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        let key = &v["problems"][1];
        assert_eq!(
            (&key["part"], &key["at"]),
            (
                &"response".into(),
                &r#"category "A", #0, key "hyper+k""#.into()
            )
        );
        assert!(
            key["message"].as_str().unwrap().contains("not a modifier"),
            "{v:#}"
        );
        // By name: a configured stanza's first (what Super+/ runs), else an installed plugin's.
        let (env, d) = env_in("check-locate");
        let kakoune = d.join("bin/keybind-find-kakoune");
        plugin_script(&kakoune, KAKOUNE);
        assert_eq!(
            locate(&env, "KAKOUNE", Vec::new).unwrap(),
            (kakoune.clone(), None)
        );
        let mine = d.join("bin2/mine");
        plugin_script(&mine, "no manifest");
        let stanza = Plugin {
            name: "Kakoune".into(),
            program: "mine".into(),
            inputs: vec!["~/x".into()],
            matches: None,
        };
        let (path, found) = locate(&env, "kakoune", || vec![stanza.clone()]).unwrap();
        assert_eq!((path, found), (mine.clone(), Some(stanza.clone())));
        let e = locate(&env, "nosuch", Vec::new).unwrap_err().to_string();
        assert!(
            e.contains("no plugin named \"nosuch\": give its path (./nosuch"),
            "{e}"
        );
        // A path: `~/` is under home.
        let (path, _) = locate(&env, "~/p", Vec::new).unwrap();
        assert_eq!(path, d.join("home/p"));
        // A path, or an installed name, gets the stanza that runs that file.
        let (_, found) =
            locate(&env, &mine.display().to_string(), || vec![stanza.clone()]).unwrap();
        assert_eq!(found, Some(stanza.clone()));
        let runs_kakoune = Plugin {
            name: "kak".into(),
            program: kakoune.display().to_string(),
            ..stanza.clone()
        };
        let (_, found) = locate(&env, "kakoune", || vec![runs_kakoune.clone()]).unwrap();
        assert_eq!(found, Some(runs_kakoune));
        // A stanza whose program isn't there.
        let gone = Plugin {
            program: "nosuch-program".into(),
            ..stanza
        };
        let e = locate(&env, "kakoune", || vec![gone])
            .unwrap_err()
            .to_string();
        assert!(e.contains("nosuch-program not found on PATH"), "{e}");
    }

    #[test]
    fn check_plugin_takes_the_stanzas_match_and_relative_inputs() {
        let (_, d) = env_in("check-stanza");
        let p = d.join("bin/echo");
        echo_plugin(
            &p,
            r#"{"version": 1, "name": "echo", "match": {"class": ["Gimp"]}}"#,
        );
        let gimp = keybind_find_sheet::Context {
            class: Some("Gimp".into()),
            ..Default::default()
        };
        let stanza = Plugin {
            name: "echo".into(),
            program: p.display().to_string(),
            inputs: vec![],
            matches: Some(m(&["kitty"], &[], &[])),
        };
        let note = |r: &Report| {
            r.notes
                .iter()
                .any(|n| n.contains("doesn't take this window"))
        };
        // The manifest's match takes Gimp; the stanza's (kitty) wins over it.
        assert!(!note(&check_at(
            &p,
            None,
            &[],
            gimp.clone(),
            &None,
            TIMEOUT
        )));
        let r = check_at(&p, Some(&stanza), &[], gimp, &None, TIMEOUT);
        assert!(note(&r), "{r:#?}");
        assert!(
            !r.notes.iter().any(|n| n.contains("no [[plugin]] stanza")),
            "{r:#?}"
        );
        // `--program` and `--title` are sent: only wm and class are empty.
        let path = p.display().to_string();
        let (out, _) = check(&[&path, "--json", "--program", "btop", "--title", "t"]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let empty: Vec<&str> = v["problems"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["message"] == "desc is empty")
            .filter_map(|p| p["at"].as_str())
            .collect();
        assert_eq!(
            empty,
            [
                r#"category "Request", #1, key "c""#,
                r#"category "Request", #2, key "c""#
            ],
            "{v:#}"
        );
        // A relative `--input` is sent absolute, as Super+/ sends every input: this plugin fails on any other.
        let abs_only = d.join("bin/abs-only");
        write_script(
            &abs_only,
            "if [ \"$1\" = --manifest ]; then exit 1; fi\n\
             grep -q '\"path\":\"/' || { echo relative >&2; exit 1; }\n\
             echo '{\"version\": 1, \"title\": \"t\", \"provider\": \"p\", \"binds\": {}}'",
        );
        let rel = format!("kbf-check-rel-{}.conf", std::process::id());
        std::fs::write(&rel, "x").unwrap();
        let out = check(&[&abs_only.display().to_string(), "--input", &rel]);
        let _ = std::fs::remove_file(&rel);
        let (out, ok) = out.unwrap();
        assert!(ok, "{out}");
    }
}
