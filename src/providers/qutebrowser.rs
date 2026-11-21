//! qutebrowser: its bindings by mode, asked from the running browser when its window has focus (a script run with
//! `:debug-pyeval --file`, sent over qutebrowser's IPC socket: defaults, `config.py`, autoconfig and `:bind`s, exactly
//! what `qute://bindings` lists, and the mode it is in). Otherwise its defaults, pinned per version
//! (`data/qutebrowser/`, from configdata.yml), with `autoconfig.yml`'s `bindings.commands` laid over them, read by
//! qutebrowser's own Python. Commands are shown, never run.

use super::mpv::private_dir;
use super::nvim::single;
use super::pinned;
use crate::context::{run_for, Context};
use crate::scope::Facts;
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::{bail, ensure, Context as _};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const REGISTRY: &[(&str, &str)] = &[(
    "3.7.0",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/qutebrowser/3.7.0.toml"
    )),
)];

/// The wait for the running qutebrowser's answer, and the fallback's run of qutebrowser's Python.
const TIMEOUT: Duration = Duration::from_secs(2);

/// The most of the answer file read: more is an error (a whole config's bindings are a few hundred KB at most).
const ANSWER_LIMIT: u64 = 8 << 20;

/// The window classes: X11's `WM_CLASS`, and the Wayland app id (its desktop file name, `qutebrowser.py:77-80`).
const CLASSES: [&str; 2] = ["qutebrowser", "org.qutebrowser.qutebrowser"];

/// The modes that use the `register` bindings (`usertypes.py:243-262`).
const REGISTER_MODES: [&str; 4] = ["set_mark", "jump_mark", "record_macro", "run_macro"];

/// Run inside qutebrowser by `:debug-pyeval --file` (`misc/utilcmds.py:136-172`), `@DIR@` replaced by the private
/// directory as a JSON string literal. The bindings as `qute://bindings` lists them (`browser/qutescheme.py:459-473`):
/// `normal`, then the other modes of both settings, sorted; each mode's `get_bindings_for` (`config/config.py:146-154`).
/// The answer is written whole (`.part`, then renamed). It runs in `utilcmds`' globals: one function, deleted after.
const ASK: &str = r#"def _keybind_find_ask(d):
    import json, os, traceback
    try:
        from qutebrowser import __version__
        from qutebrowser.config import config
        from qutebrowser.keyinput import modeman
        modes = set(config.val.bindings.default).union(config.val.bindings.commands)
        modes.discard("normal")
        answer = {
            "version": __version__,
            "mode": modeman.instance("last-focused").mode.name,
            "bindings": [[m, str(k), c] for m in ["normal"] + sorted(modes)
                         for k, c in config.key_instance.get_bindings_for(m).items()],
        }
    except Exception:
        answer = {"error": traceback.format_exc()}
    path = os.path.join(d, "answer.json")
    with open(path + ".part", "x", encoding="utf-8") as f:
        json.dump(answer, f)
    os.replace(path + ".part", path)
try:
    _keybind_find_ask(@DIR@)
finally:
    del _keybind_find_ask
"#;

/// Run by qutebrowser's own interpreter (`-I -c FALLBACK <autoconfig.yml>`): its version and the autoconfig as
/// qutebrowser loads it (`utils.yaml_load`: `CSafeLoader`, else `SafeLoader`), `null` when there is no file. A file that
/// isn't a regular one, is over 4 MiB, doesn't parse, or nests deeper than 32 or holds over 100000 values (aliases
/// expanded) is an `error` instead.
const FALLBACK: &str = r#"import json, os, stat, sys
import qutebrowser
def load(path):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
    except FileNotFoundError:
        return None
    with open(fd, "rb") as f:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ValueError("not a regular file")
        data = f.read((4 << 20) + 1)
    if len(data) > 4 << 20:
        raise ValueError("over 4 MiB")
    import yaml
    return yaml.load(data.decode("utf-8"), Loader=getattr(yaml, "CSafeLoader", yaml.SafeLoader))
left = [100000]
def walk(v, depth):
    left[0] -= 1
    if left[0] < 0 or depth > 32:
        raise ValueError("too large or nested too deep")
    for x in (v if isinstance(v, list) else [y for kv in v.items() for y in kv] if isinstance(v, dict) else []):
        walk(x, depth + 1)
out = {"version": qutebrowser.__version__, "autoconfig": None}
try:
    ac = load(sys.argv[1])
    walk(ac, 0)
    text = json.dumps(dict(out, autoconfig=ac), default=str)
except Exception as e:
    text = json.dumps(dict(out, error=f"{type(e).__name__}: {e}"))
print(text)
"#;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Deserialize)]
struct Pinned {
    qutebrowser_version: String,
    /// `bindings.default`'s sections, in configdata order.
    modes: Vec<String>,
    bind: Vec<DefaultBind>,
    /// Qt's key names (lower case) as X keysyms.
    keys: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct DefaultBind {
    mode: String,
    key: String,
    command: String,
}

/// The running qutebrowser's answer: its version, its mode and its bindings, or the error the script caught.
#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    bindings: Vec<(String, String, String)>,
}

/// `FALLBACK`'s output.
#[derive(Deserialize)]
struct Fallback {
    version: String,
    #[serde(default)]
    autoconfig: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

/// A live answer: the version, the mode and the bindings (mode, key, command).
type Live = (String, String, Vec<(String, String, String)>);

/// A binding with its key in keybind-find's notation (`None`: left out, see `notation`).
type Row = (String, Option<String>, String);

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    // The pid context found is the window's own only when the focused window is qutebrowser's.
    let pid =
        crate::context::editor_pid().filter(|_| ctx.class.as_deref().is_some_and(is_qutebrowser));
    let (sheet, facts, notes) = read(
        REGISTRY,
        pid,
        Path::new("/proc"),
        &|k| std::env::var(k).ok(),
        TIMEOUT,
    )?;
    if let Some(facts) = facts {
        crate::context::set_facts(facts);
    }
    for note in notes {
        eprintln!("keybind-find: qutebrowser: {note}");
    }
    Ok(sheet)
}

/// A qutebrowser window, by class or app id, any case.
pub(crate) fn is_qutebrowser(class: &str) -> bool {
    CLASSES.iter().any(|c| c.eq_ignore_ascii_case(class))
}

/// The sheet, the facts (from a live answer) and the stderr notes (printable): the qutebrowser at `pid` asked over its
/// IPC socket (found under `proc`), else the fallback (`env`: keybind-find's environment). `wait` bounds the answer
/// and the fallback's run (test seams).
fn read(
    registry: &[(&'static str, &'static str)],
    pid: Option<u32>,
    proc: &Path,
    env: Env,
    wait: Duration,
) -> anyhow::Result<(Sheet, Option<Facts>, Vec<String>)> {
    let mut notes = Vec::new();
    let live = match pid.and_then(|p| ipc_socket(proc, p)) {
        None => None,
        Some(socket) => match ask(&socket, wait) {
            Ok(a) => Some(a),
            Err(e) => {
                notes.push(format!(
                    "the running qutebrowser: {e:#}; reading its config files instead"
                ));
                None
            }
        },
    };
    let (version, rows, facts) = match live {
        Some((version, mode, bindings)) => {
            // Only the key table is used: its note about the defaults' version doesn't apply.
            let (pin, _) = pin_for(registry, Some(&version))?;
            (version, translate(bindings, &pin.keys), mode_fact(&mode))
        }
        None => {
            let (version, rows) = fallback(registry, env, wait, &mut notes)?;
            (version, rows, None)
        }
    };
    let (sheet, left_out) = to_sheet(&version, rows);
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) keybind-find can't send: the keypad modifier (Num), a Qt key with no X \
             keysym, a key qutebrowser can't parse, or a character with no keysym"
        ));
    }
    Ok((sheet, facts, notes.iter().map(|n| printable(n)).collect()))
}

/// The pinned file for `version`, checked, and `pinned::select`'s note.
fn pin_for(
    registry: &[(&'static str, &'static str)],
    version: Option<&str>,
) -> anyhow::Result<(Pinned, Option<String>)> {
    let (v, text, note) = pinned::select("qutebrowser", registry, version)
        .context("no pinned qutebrowser defaults")?;
    let pin: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/qutebrowser/{v}.toml"))?;
    ensure!(
        pin.qutebrowser_version == v,
        "data/qutebrowser/{v}.toml names qutebrowser {}",
        pin.qutebrowser_version
    );
    Ok((pin, note))
}

/// The answer's mode as the fact `mode`; the mark and macro modes use the `register` bindings.
fn mode_fact(mode: &str) -> Option<Facts> {
    let mode = if REGISTER_MODES.contains(&mode) {
        "register"
    } else {
        mode
    };
    Some(Facts::from([("mode".parse().ok()?, mode.parse().ok()?)]))
}

// --- the live read -------------------------------------------------------------------------------------------------

/// qutebrowser's IPC socket: the path, in `<proc>/<pid>/net/unix`, of a unix socket that is one of the process's fds
/// and whose file name starts with `ipc-` (`misc/ipc.py:55-68`; any `--basedir`, the Flatpak's runtime dir).
fn ipc_socket(proc: &Path, pid: u32) -> Option<PathBuf> {
    let dir = proc.join(pid.to_string());
    let inodes: std::collections::HashSet<String> = std::fs::read_dir(dir.join("fd"))
        .ok()?
        .flatten()
        .take(1 << 16)
        .filter_map(|e| std::fs::read_link(e.path()).ok())
        .filter_map(|l| {
            let l = l.to_str()?;
            Some(l.strip_prefix("socket:[")?.strip_suffix(']')?.to_string())
        })
        .collect();
    let mut text = String::new();
    std::fs::File::open(dir.join("net/unix"))
        .ok()?
        .take(16 << 20)
        .read_to_string(&mut text)
        .ok()?;
    // `Num RefCount Protocol Flags Type St Inode Path`, the inode right-aligned.
    text.lines().skip(1).find_map(|line| {
        let mut rest = line.trim_start_matches(' ');
        let mut inode = "";
        for _ in 0..7 {
            let end = rest.find(' ')?;
            inode = &rest[..end];
            rest = rest[end..].trim_start_matches(' ');
        }
        let path = Path::new(rest);
        let ipc = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("ipc-"));
        (path.is_absolute() && ipc && inodes.contains(inode)).then(|| path.to_path_buf())
    })
}

/// Asks the qutebrowser listening on `socket` for its bindings through a private directory made beside the socket (in
/// qutebrowser's runtime directory, which only we can enter), removed afterwards. Its version, mode and bindings.
fn ask(socket: &Path, wait: Duration) -> anyhow::Result<Live> {
    use std::hash::{BuildHasher, Hasher};
    use std::os::unix::fs::MetadataExt;
    let base = socket.parent().context("the socket has no directory")?;
    let private = std::fs::metadata(base).is_ok_and(|m| {
        m.is_dir() && Some(m.uid()) == crate::context::own_uid() && m.mode() & 0o077 == 0
    });
    ensure!(
        private,
        "{} isn't a directory only we can enter",
        base.display()
    );
    // Random keys from the OS (std's `RandomState`), so the name can't be guessed.
    let random = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    let dir = private_dir(base, &format!("keybind-find-qute-{random:016x}"))?;
    let out = ask_in(socket, &dir, wait);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn ask_in(socket: &Path, dir: &Path, wait: Duration) -> anyhow::Result<Live> {
    use std::os::unix::fs::OpenOptionsExt;
    let start = Instant::now();
    let path = dir
        .to_str()
        .context("the private directory's path isn't UTF-8")?;
    let script = ASK.replace("@DIR@", &serde_json::to_string(path)?);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join("ask.py"))
        .and_then(|mut f| f.write_all(script.as_bytes()))
        .context("writing the script")?;
    let arg = serde_json::to_string(&format!(":debug-pyeval --file {path}/ask.py"))?;
    let line = format!("{{\"args\": [{arg}], \"target_arg\": null, \"protocol_version\": 1}}\n");
    let mut stream = std::os::unix::net::UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream
        .write_all(line.as_bytes())
        .with_context(|| format!("writing to {}", socket.display()))?;
    // The connection stays open until the answer is in: qutebrowser reads the line before it sees us go.
    let answer = dir.join("answer.json");
    while !answer.exists() {
        if start.elapsed() >= wait {
            bail!("no answer within {} s", wait.as_secs_f32());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(stream);
    let mut text = String::new();
    std::fs::File::open(&answer)
        .context("opening the answer")?
        .take(ANSWER_LIMIT + 1)
        .read_to_string(&mut text)
        .context("reading the answer")?;
    ensure!(
        text.len() as u64 <= ANSWER_LIMIT,
        "its answer is over {ANSWER_LIMIT} bytes"
    );
    parse_answer(&text)
}

/// The answer file: the script's error (its last line) is an error.
fn parse_answer(text: &str) -> anyhow::Result<Live> {
    let a: Answer = serde_json::from_str(text).context("its answer isn't the script's JSON")?;
    if let Some(e) = a.error {
        let last = e.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!("{}", last.chars().take(300).collect::<String>());
    }
    match (a.version, a.mode) {
        (Some(v), Some(m)) => Ok((v, m, a.bindings)),
        _ => bail!("its answer has no version or mode"),
    }
}

// --- the fallback --------------------------------------------------------------------------------------------------

/// The pinned defaults for the installed qutebrowser, with `autoconfig.yml`'s `bindings.commands` over them, unless a
/// `config.py` decides what qutebrowser reads (`config/configinit.py:46-62`). Its version and rows.
fn fallback(
    registry: &[(&'static str, &'static str)],
    env: Env,
    wait: Duration,
    notes: &mut Vec<String>,
) -> anyhow::Result<(String, Vec<Row>)> {
    let dir = config_dir(env).context("no HOME to find qutebrowser's config in")?;
    let autoconfig = dir.join("autoconfig.yml");
    let out = run_fallback(env, &autoconfig, wait)?;
    let (pin, note) = pin_for(registry, Some(&out.version))?;
    notes.extend(note);
    let mut rows = translate(
        pin.bind
            .iter()
            .map(|d| (d.mode.clone(), d.key.clone(), d.command.clone())),
        &pin.keys,
    );
    if dir.join("config.py").exists() {
        notes.push(
            "config.py is Python: its bindings are read only from the running qutebrowser (Super+/ over its window)"
                .into(),
        );
    } else if let Some(e) = out.error {
        notes.push(format!(
            "reading {}: {e}; listing the defaults only",
            autoconfig.display()
        ));
    } else if let Some(ac) = out.autoconfig.filter(|a| !a.is_null()) {
        match commands(&ac, &pin.modes) {
            Ok(changes) => overlay(&mut rows, changes, &pin.keys),
            Err(why) => notes.push(format!(
                "{}: {why}; listing the defaults only",
                autoconfig.display()
            )),
        }
    }
    Ok((out.version, rows))
}

/// qutebrowser's config directory: Qt's `ConfigLocation` (`$XDG_CONFIG_HOME` when it starts with `/`, else
/// `~/.config`: qstandardpaths_unix.cpp:247-252) + `qutebrowser` (`utils/standarddir.py:65-94`).
fn config_dir(env: Env) -> Option<PathBuf> {
    let base = match env("XDG_CONFIG_HOME").filter(|v| v.starts_with('/')) {
        Some(c) => PathBuf::from(c),
        None => PathBuf::from(env("HOME").filter(|h| !h.is_empty())?).join(".config"),
    };
    Some(base.join("qutebrowser"))
}

/// Runs `FALLBACK` with the interpreter on the `#!` line of the `qutebrowser` on `PATH`.
fn run_fallback(env: Env, autoconfig: &Path, wait: Duration) -> anyhow::Result<Fallback> {
    let script = on_path(env).context("qutebrowser isn't on PATH")?;
    let (program, arg) = shebang(&script)?;
    let mut cmd = Command::new(&program);
    cmd.args(arg)
        .args(["-I", "-c", FALLBACK])
        .arg(autoconfig)
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    let out = run_for(cmd, wait).with_context(|| {
        format!(
            "running {program} (the interpreter of {}) for its version and autoconfig.yml",
            script.display()
        )
    })?;
    serde_json::from_str(out.trim()).context("reading what qutebrowser's Python printed")
}

/// The first executable regular file named `qutebrowser` in `PATH`.
fn on_path(env: Env) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    env("PATH")?
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join("qutebrowser"))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// A script's interpreter as Linux reads the `#!` line (its first 256 bytes): the program, and everything after it
/// (trimmed) as one optional argument.
fn shebang(script: &Path) -> anyhow::Result<(String, Option<String>)> {
    let mut head = Vec::new();
    std::fs::File::open(script)
        .and_then(|f| f.take(256).read_to_end(&mut head))
        .with_context(|| format!("reading {}", script.display()))?;
    let head = String::from_utf8_lossy(&head);
    let line = head
        .strip_prefix("#!")
        .and_then(|h| h.split('\n').next().filter(|_| h.contains('\n')))
        .with_context(|| format!("{} has no #! line", script.display()))?;
    let line = line.trim_matches([' ', '\t']);
    let (program, arg) = match line.split_once([' ', '\t']) {
        Some((p, a)) => (p, Some(a.trim_matches([' ', '\t']).to_string())),
        None => (line, None),
    };
    ensure!(
        !program.is_empty(),
        "{}'s #! line names no program",
        script.display()
    );
    Ok((program.to_string(), arg.filter(|a| !a.is_empty())))
}

/// autoconfig.yml's `settings` → `bindings.commands` → `global`, as (mode, key, command; `None` to unbind), checked for
/// the shape qutebrowser writes (`config/configfiles.py:326-415`, `VERSION = 2`); `modes`: the modes it may name
/// (`fixed_keys`). An error says what isn't so.
fn commands(
    ac: &serde_json::Value,
    modes: &[String],
) -> Result<Vec<(String, String, Option<String>)>, String> {
    let top = ac.as_object().ok_or("not a mapping")?;
    match top.get("config_version") {
        Some(v) if v.as_i64() == Some(2) => {}
        v => {
            let v = v.map_or("missing".into(), |v| v.to_string());
            return Err(format!("config_version is {v}, not 2"));
        }
    }
    let settings = top
        .get("settings")
        .and_then(|s| s.as_object())
        .ok_or("no settings mapping")?;
    let Some(commands) = settings.get("bindings.commands") else {
        return Ok(Vec::new());
    };
    let shape = "bindings.commands isn't {global: {mode: {key: command or null}}}";
    let commands = commands.as_object().ok_or(shape)?;
    if commands.keys().any(|k| k != "global") {
        return Err(shape.into());
    }
    let Some(global) = commands.get("global") else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (mode, keys) in global.as_object().ok_or(shape)? {
        if !modes.contains(mode) {
            return Err(format!("bindings.commands names no mode {mode:?}"));
        }
        let Some(keys) = keys.as_object() else {
            if keys.is_null() {
                continue;
            }
            return Err(shape.into());
        };
        for (key, command) in keys {
            let command = match command {
                serde_json::Value::Null => None,
                serde_json::Value::String(c) => Some(c.clone()),
                _ => return Err(shape.into()),
            };
            out.push((mode.clone(), key.clone(), command));
        }
    }
    Ok(out)
}

/// Lays `changes` over `rows` as `get_bindings_for` does (`config/config.py:146-154`): an empty or `None` command
/// removes its mode's key, any other replaces it in place or is appended. Keys are compared in keybind-find's notation.
fn overlay(
    rows: &mut Vec<Row>,
    changes: Vec<(String, String, Option<String>)>,
    keys: &BTreeMap<String, String>,
) {
    for (mode, key, command) in changes {
        let key = notation(&key, keys);
        let command = command.filter(|c| !c.is_empty());
        let at = key.as_ref().and_then(|k| {
            rows.iter()
                .position(|r| r.0 == mode && r.1.as_ref() == Some(k))
        });
        match (command, at) {
            (None, Some(i)) => {
                rows.remove(i);
            }
            (None, None) => {}
            (Some(c), Some(i)) => rows[i].2 = c,
            (Some(c), None) => rows.push((mode, key, c)),
        }
    }
}

// --- keys ----------------------------------------------------------------------------------------------------------

fn translate(
    rows: impl IntoIterator<Item = (String, String, String)>,
    keys: &BTreeMap<String, String>,
) -> Vec<Row> {
    rows.into_iter()
        .map(|(mode, key, command)| {
            let key = notation(&key, keys);
            (mode, key, command)
        })
        .collect()
}

/// A qutebrowser key string (`gt`, `<Ctrl-x>`, `<Ctrl+Shift+Tab>`) in keybind-find's notation, as `_parse_keystring`
/// splits it (`keyinput/keyutils.py:270-291`); `None` when a key in it can't be sent (see `special`, `single_key`) or
/// the notation refuses the result.
fn notation(s: &str, keys: &BTreeMap<String, String>) -> Option<String> {
    let mut out = Vec::new();
    let mut key = String::new();
    let mut special = false;
    for c in s.chars() {
        match c {
            '>' if special => {
                out.push(special_key(&key, keys)?);
                key.clear();
                special = false;
            }
            '>' => out.push(single_key('>')?),
            // As qutebrowser: a second `<` doesn't drop what the first one started.
            '<' => special = true,
            c if special => key.push(c),
            c => out.push(single_key(c)?),
        }
    }
    if special {
        out.push(single_key('<')?);
        for c in key.chars() {
            out.push(single_key(c)?);
        }
    }
    let notation = out.join(" ");
    (!notation.is_empty() && notation.parse::<keybind_find_sheet::Key>().is_ok())
        .then_some(notation)
}

/// One character typed as a key (`_parse_single_key`, keyutils.py:323-325): an upper-case one (Python's `isupper`) is
/// shift + the key; the X keysym is `nvim::single`'s.
fn single_key(c: char) -> Option<String> {
    if c.is_uppercase() {
        Some(chord(&["shift"], single(c.to_ascii_lowercase())?))
    } else {
        Some(single(c)?.to_string())
    }
}

/// The inside of `<…>`: `_parse_special_key`'s rewriting (keyutils.py:294-320), then Qt's `decodeString`
/// (qkeysequence.cpp:1039-1200): leading `ctrl+ shift+ alt+ meta+ num+` are modifiers, a lone `+` at the end is the
/// key, anything else before the last `+` is invalid; the key a character, `f1`-`f35`, or a Qt key name (`keys`).
/// `None` for the keypad modifier, an invalid key, or a key with no X keysym.
/// ponytail: Qt's spaces around a modifier (`ctrl +x`) aren't trimmed; such a key is left out.
fn special_key(s: &str, keys: &BTreeMap<String, String>) -> Option<String> {
    let mut s = s.to_lowercase();
    for (from, to) in [
        ("control", "ctrl"),
        ("windows", "meta"),
        ("mod4", "meta"),
        ("command", "meta"),
        ("cmd", "meta"),
        ("super", "meta"),
        ("mod1", "alt"),
        ("less", "<"),
        ("greater", ">"),
    ] {
        s = s.replace(from, to);
    }
    for m in ["ctrl", "meta", "alt", "shift", "num"] {
        s = s.replace(&format!("{m}-"), &format!("{m}+"));
    }
    let mut mods = Vec::new();
    let mut rest = s.as_str();
    // qutebrowser's Meta is the Windows/Super key; Num (the keypad) has no place in the notation.
    while let Some((prefix, m)) = [
        ("ctrl+", Some("ctrl")),
        ("shift+", Some("shift")),
        ("alt+", Some("alt")),
        ("meta+", Some("super")),
        ("num+", None),
    ]
    .into_iter()
    .find(|(p, _)| rest.starts_with(p))
    {
        mods.push(m?);
        // The prefix is ASCII.
        rest = &rest[prefix.len()..];
    }
    // Qt trims spaces around the name, but keeps a lone one.
    let name = match rest.trim_matches(' ') {
        "" if !rest.is_empty() => " ",
        t => t,
    };
    let keysym = match name.chars().count() {
        0 => return None,
        1 => single(name.chars().next()?)?.to_string(),
        _ if name.contains('+') => return None,
        _ => match name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
            Some(n) if (1..=35).contains(&n) => format!("F{n}"),
            _ => keys.get(name)?.clone(),
        },
    };
    Some(chord(&mods, &keysym))
}

// --- the sheet -----------------------------------------------------------------------------------------------------

/// One category per mode, each bind tagged with its mode; the command is the description and the action. Also how
/// many keys were left out.
fn to_sheet(version: &str, rows: Vec<Row>) -> (Sheet, usize) {
    let mut sheet = Sheet {
        title: format!("qutebrowser {version}"),
        provider: "qutebrowser".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for (mode, key, command) in rows {
        let (Some(key), Ok(scope), Ok(value)) = (key, "mode".parse::<Name>(), mode.parse::<Name>())
        else {
            left_out += 1;
            continue;
        };
        sheet.binds.entry(mode).or_default().push(Bind {
            key,
            desc: command.clone(),
            action: command,
            when: BTreeMap::from([(scope, Values::from(value))]),
            run: Vec::new(),
        });
    }
    (sheet, left_out)
}

/// `s` with control and bidirectional-format characters escaped: a note can quote the browser's answer or a file.
/// ponytail: a copy of `openbox::printable`; share it when a story owns both.
fn printable(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => c.escape_unicode().to_string(),
            c if c.is_control() => c.escape_debug().to_string(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn pin() -> Pinned {
        pin_for(REGISTRY, Some("3.7.0")).unwrap().0
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/qutebrowser")
                .join(name),
        )
        .unwrap()
    }

    /// A fresh directory for one test, 0700 (it stands for qutebrowser's runtime directory too).
    fn fresh(name: &str) -> PathBuf {
        let d = crate::test_tmp().join(format!("kbf-qute-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    /// An executable script at `path`, written by a child process (a file this process held open for writing would
    /// fail another test's exec with ETXTBSY).
    fn script(path: &Path, text: &str) {
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        assert!(sh.wait().unwrap().success());
    }

    /// (mode, key, desc, mode tag) of every bind, in sheet order.
    fn binds(s: &Sheet) -> Vec<(String, String, String, String)> {
        s.binds
            .iter()
            .flat_map(|(c, bs)| {
                bs.iter().map(move |b| {
                    let tag = b
                        .when
                        .get("mode")
                        .map(|v| v.iter().next().unwrap().to_string());
                    (
                        c.clone(),
                        b.key.clone(),
                        b.desc.clone(),
                        tag.unwrap_or_default(),
                    )
                })
            })
            .collect()
    }

    fn find<'a>(s: &'a Sheet, mode: &str, key: &str) -> Vec<&'a str> {
        s.binds[mode]
            .iter()
            .filter(|b| b.key == key)
            .map(|b| b.desc.as_str())
            .collect()
    }

    #[test]
    fn keys_as_qutebrowser_parses_them() {
        let keys = pin().keys;
        for (key, want) in [
            ("gt", "g t"),
            ("O", "shift+o"),
            ("?", "question"),
            ("<Ctrl-x>", "ctrl+x"),
            ("<ctrl-X>", "ctrl+x"),
            ("<Control+x>", "ctrl+x"),
            ("<Ctrl+Shift+Tab>", "ctrl+shift+Tab"),
            ("<Ctrl-PgDown>", "ctrl+Next"),
            ("<Escape>", "Escape"),
            ("<Esc>", "Escape"),
            ("<Space>", "space"),
            ("<Enter>", "KP_Enter"),
            ("<Back>", "XF86Back"),
            ("<Mod1-x>", "alt+x"),
            ("<Windows-x>", "super+x"),
            ("<Super-x>", "super+x"),
            ("<Mod4-x>", "super+x"),
            ("<Meta-x>", "super+x"),
            ("<Ctrl++>", "ctrl+plus"),
            ("<Ctrl-->", "ctrl+minus"),
            ("<less>", "less"),
            ("<F35>", "F35"),
            ("<ab", "less a b"),
            ("a>", "a greater"),
            (",a", "comma a"),
        ] {
            assert_eq!(notation(key, &keys).as_deref(), Some(want), "{key}");
        }
        for key in [
            "<F36>", "<Num+5>", "<Guide>", "<Foo-x>", "É", "<Ctrl+>", "<a+b>", "<>", "",
        ] {
            assert_eq!(notation(key, &keys), None, "{key}");
        }
    }

    /// AC #6: the pinned defaults.
    #[test]
    fn the_pinned_defaults() {
        let pin = pin();
        assert_eq!(pin.qutebrowser_version, "3.7.0");
        assert_eq!(pin.bind.len(), 295);
        assert_eq!(pin.modes.len(), 9);
        assert!(pin.bind.iter().all(|b| pin.modes.contains(&b.mode)));
        assert!(pin
            .keys
            .values()
            .all(|k| k.parse::<keybind_find_sheet::Key>().is_ok()));
        let rows = translate(
            pin.bind
                .iter()
                .map(|d| (d.mode.clone(), d.key.clone(), d.command.clone())),
            &pin.keys,
        );
        let (s, left_out) = to_sheet("3.7.0", rows);
        assert_eq!(left_out, 0);
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 295);
        assert_eq!(s.binds["normal"].len(), 189);
        for (mode, key, desc) in [
            ("normal", "o", "cmd-set-text -s :open"),
            ("normal", "shift+o", "cmd-set-text -s :open -t"),
            ("normal", "g t", "cmd-set-text -s :tab-select"),
            ("normal", "ctrl+Next", "tab-next"),
            (
                "normal",
                "Escape",
                "clear-keychain ;; search ;; fullscreen --leave",
            ),
            ("insert", "ctrl+e", "edit-text"),
            ("caret", "ctrl+space", "selection-drop"),
        ] {
            assert_eq!(find(&s, mode, key), [desc], "{mode} {key}");
        }
        assert_eq!(s.title, "qutebrowser 3.7.0");
    }

    /// AC #2, #5: a live answer as the sheet, its mode as the fact.
    #[test]
    fn the_answer_as_the_sheet() {
        let (version, mode, bindings) = parse_answer(&fixture("answer.json")).unwrap();
        assert_eq!((version.as_str(), mode.as_str()), ("3.7.0", "insert"));
        let (s, left_out) = to_sheet(&version, translate(bindings, &pin().keys));
        assert_eq!(left_out, 2, "<Num+5> and <Guide>");
        let t = |c: &str, k: &str, d: &str| (c.into(), k.into(), d.into(), c.into());
        assert_eq!(
            binds(&s),
            [
                t("caret", "v", "selection-toggle"),
                t("caret", "ctrl+space", "selection-drop"),
                t("insert", "ctrl+e", "edit-text"),
                t("insert", "Escape", "mode-leave"),
                t("normal", "g t", "tab-focus"),
                t("normal", "shift+o", "cmd-set-text -s :open -t"),
                t("normal", "ctrl+shift+Tab", "tab-prev"),
                t("normal", "ctrl+plus", "zoom-in"),
                t("normal", "comma l", "message-info e2e live"),
                t("register", "Escape", "mode-leave"),
            ]
        );
        assert!(s
            .binds
            .values()
            .flatten()
            .all(|b| b.action == b.desc && b.run.is_empty()));
        let fact = |m: &str| mode_fact(m).unwrap()["mode"].to_string();
        assert_eq!(fact("insert"), "insert");
        for m in REGISTER_MODES {
            assert_eq!(fact(m), "register");
        }
        let err = parse_answer(&fixture("answer-error.json"))
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "AttributeError: module 'qutebrowser.keyinput.modeman' has no attribute 'instance'"
        );
        assert!(parse_answer("{}").is_err() && parse_answer("[").is_err());
        // With the fact, scope::apply hides the other modes and takes the tag off insert's.
        let mut entries = crate::sheet::entries(&[s]);
        crate::scope::apply(&mut entries, &mode_fact("insert").unwrap());
        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .all(|e| e.category == "insert" && e.bind.when.is_empty()));
    }

    /// A stand-in `/proc` for pid 4242 with fds `fds` (link targets) and `net/unix` rows (inode, path).
    fn fake_proc(d: &Path, fds: &[&str], rows: &[(&str, &Path)]) -> PathBuf {
        let proc = d.join("proc");
        let dir = proc.join("4242");
        std::fs::create_dir_all(dir.join("fd")).unwrap();
        std::fs::create_dir_all(dir.join("net")).unwrap();
        for (i, target) in fds.iter().enumerate() {
            std::os::unix::fs::symlink(target, dir.join("fd").join(i.to_string())).unwrap();
        }
        let mut unix = "Num       RefCount Protocol Flags    Type St Inode Path\n".to_string();
        unix += "0000000000000000: 00000003 00000000 00000000 0001 03  9999\n";
        for (inode, path) in rows {
            unix += &format!(
                "0000000000000000: 00000002 00000000 00010000 0001 01 {inode:>5} {}\n",
                path.display()
            );
        }
        std::fs::write(dir.join("net/unix"), unix).unwrap();
        proc
    }

    #[test]
    fn the_socket_from_proc() {
        let d = fresh("proc");
        let ipc = d.join("run/ipc-48feb98");
        let other = d.join("run/ipc-other");
        let proc = fake_proc(
            &d,
            &["socket:[111]", "/dev/null", "socket:[9999]"],
            &[
                ("222", &other),
                ("111", &ipc),
                ("111", Path::new("@abstract")),
            ],
        );
        assert_eq!(ipc_socket(&proc, 4242), Some(ipc.clone()));
        // A socket of this process that isn't an `ipc-` one, or an `ipc-` one of another process: none.
        let proc = fake_proc(
            &fresh("proc2"),
            &["socket:[111]"],
            &[("222", &ipc), ("111", &d.join("x"))],
        );
        assert_eq!(ipc_socket(&proc, 4242), None);
        assert_eq!(ipc_socket(&proc, 1), None);
    }

    /// A fake `qutebrowser` in `d/bin` whose `#!` line runs a fake interpreter that checks its arguments and prints
    /// `d/out.json`; the environment for it (HOME `d/home`).
    fn fake_qutebrowser(d: &Path, out: &str) -> BTreeMap<String, String> {
        std::fs::create_dir_all(d.join("bin")).unwrap();
        std::fs::write(d.join("out.json"), out).unwrap();
        let home = d.join("home");
        std::fs::create_dir_all(home.join(".config/qutebrowser")).unwrap();
        script(
            &d.join("python"),
            &format!(
                "#!/bin/sh\n[ \"$MISE_NOT_FOUND_AUTO_INSTALL\" = 0 ] || exit 7\n\
                 [ \"$1 $2\" = '-I -c' ] && [ \"$4\" = '{ac}' ] && [ $# = 4 ] || exit 8\n\
                 printf '%s\\n' \"$@\" > '{d}/args'\ncat '{d}/out.json'\n",
                ac = home.join(".config/qutebrowser/autoconfig.yml").display(),
                d = d.display()
            ),
        );
        script(
            &d.join("bin/qutebrowser"),
            &format!("#!{}\nimport sys\n", d.join("python").display()),
        );
        BTreeMap::from([
            (
                "PATH".into(),
                format!("/nonexistent:{}", d.join("bin").display()),
            ),
            ("HOME".into(), home.display().to_string()),
        ])
    }

    fn run(
        pid: Option<u32>,
        proc: &Path,
        env: &BTreeMap<String, String>,
        wait: Duration,
    ) -> anyhow::Result<(Sheet, Option<Facts>, Vec<String>)> {
        read(REGISTRY, pid, proc, &|k| env.get(k).cloned(), wait)
    }

    /// AC #3: the fallback over the pinned defaults.
    #[test]
    fn the_fallback() {
        let d = fresh("fallback");
        let env = fake_qutebrowser(&d, &fixture("fallback.json"));
        let (s, facts, notes) = run(None, &d, &env, TIMEOUT).unwrap();
        assert!(facts.is_none() && notes.is_empty(), "{notes:?}");
        let args = std::fs::read_to_string(d.join("args")).unwrap();
        assert!(
            args.starts_with("-I\n-c\nimport json, os, stat, sys\n"),
            "{args}"
        );
        let normal = &s.binds["normal"];
        assert_eq!(normal.last().unwrap().key, "comma a");
        assert_eq!(normal.last().unwrap().desc, "message-info e2e autoconfig");
        assert_eq!(normal.len(), 189, "one added, one gone");
        assert!(find(&s, "normal", "d").is_empty());
        assert!(find(&s, "insert", "ctrl+e").is_empty());
        assert_eq!(find(&s, "caret", "ctrl+shift+e"), ["selection-drop"]);
        assert_eq!(find(&s, "command", "ctrl+e"), ["rl-end-of-line"]);

        // Not config_version 2: a note and the defaults.
        let v1 = fixture("fallback.json").replace("\"config_version\": 2", "\"config_version\": 1");
        std::fs::write(d.join("out.json"), v1).unwrap();
        let (s, _, notes) = run(None, &d, &env, TIMEOUT).unwrap();
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 295);
        assert_eq!(notes.len(), 1);
        assert!(notes[0]
            .ends_with("autoconfig.yml: config_version is 1, not 2; listing the defaults only"));
        // A shape qutebrowser doesn't write, and the script's error: the same.
        for out in [
            r#"{"version": "3.7.0", "autoconfig": {"config_version": 2, "settings": {"bindings.commands": {"global": {"normal": {"x": 1}}}}}}"#,
            r#"{"version": "3.7.0", "autoconfig": {"config_version": 2, "settings": {"bindings.commands": {"global": {"nosuch": {}}}}}}"#,
            r#"{"version": "3.7.0", "autoconfig": [], "error": "ValueError: not a regular file"}"#,
        ] {
            std::fs::write(d.join("out.json"), out).unwrap();
            let (s, _, notes) = run(None, &d, &env, TIMEOUT).unwrap();
            assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 295);
            assert!(
                notes.len() == 1 && notes[0].ends_with("listing the defaults only"),
                "{notes:?}"
            );
        }
        // No autoconfig.yml: the defaults, silently.
        std::fs::write(
            d.join("out.json"),
            r#"{"version": "3.7.0", "autoconfig": null}"#,
        )
        .unwrap();
        let (_, _, notes) = run(None, &d, &env, TIMEOUT).unwrap();
        assert!(notes.is_empty());

        // config.py decides: the defaults and the note.
        std::fs::write(d.join("out.json"), fixture("fallback.json")).unwrap();
        std::fs::write(d.join("home/.config/qutebrowser/config.py"), "").unwrap();
        let (s, _, notes) = run(None, &d, &env, TIMEOUT).unwrap();
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 295);
        assert_eq!(
            notes,
            ["config.py is Python: its bindings are read only from the running qutebrowser (Super+/ over its window)"]
        );
        // XDG_CONFIG_HOME, only when absolute.
        let mut xdg = env.clone();
        xdg.insert("XDG_CONFIG_HOME".into(), "relative".into());
        assert_eq!(
            config_dir(&|k| xdg.get(k).cloned()),
            Some(d.join("home/.config/qutebrowser"))
        );
        xdg.insert("XDG_CONFIG_HOME".into(), "/x".into());
        assert_eq!(
            config_dir(&|k| xdg.get(k).cloned()),
            Some(PathBuf::from("/x/qutebrowser"))
        );
    }

    #[test]
    fn the_interpreter() {
        let d = fresh("shebang");
        let env = fake_qutebrowser(&d, &fixture("fallback.json"));
        // `#!/usr/bin/env PROGRAM`: env and its one argument.
        script(
            &d.join("bin/qutebrowser"),
            &format!("#! /usr/bin/env  {} \n", d.join("python").display()),
        );
        assert_eq!(
            shebang(&d.join("bin/qutebrowser")).unwrap(),
            (
                "/usr/bin/env".into(),
                Some(d.join("python").display().to_string())
            )
        );
        let (s, _, notes) = run(None, &d, &env, TIMEOUT).unwrap();
        assert!(
            notes.is_empty() && s.binds["normal"].len() == 189,
            "{notes:?}"
        );
        // No #! line; no qutebrowser on PATH: errors.
        script(&d.join("bin/qutebrowser"), "import sys\n");
        assert!(run(None, &d, &env, TIMEOUT).is_err());
        let none = BTreeMap::from([
            ("PATH".into(), "/nonexistent".into()),
            ("HOME".into(), "/x".into()),
        ]);
        let err = run(None, &d, &none, TIMEOUT).unwrap_err();
        assert_eq!(err.to_string(), "qutebrowser isn't on PATH");
    }

    /// qutebrowser's side of the IPC socket at `socket`: reads the line, checks it, and answers `answer` (none: never).
    /// The private directory it was told about.
    fn fake_browser(socket: &Path, answer: Option<String>) -> std::thread::JoinHandle<PathBuf> {
        use std::io::BufRead;
        let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap();
            assert!(
                line.ends_with("}\n") && line.matches('\n').count() == 1,
                "{line:?}"
            );
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["protocol_version"], 1);
            assert!(v["target_arg"].is_null());
            let arg = v["args"][0].as_str().unwrap();
            assert_eq!(v["args"].as_array().unwrap().len(), 1);
            let file = arg.strip_prefix(":debug-pyeval --file ").unwrap();
            let dir = Path::new(file.strip_suffix("/ask.py").unwrap()).to_path_buf();
            let meta = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!((meta(&dir), meta(Path::new(file))), (0o700, 0o600));
            let ask = std::fs::read_to_string(file).unwrap();
            assert!(ask.contains(&format!(
                "_keybind_find_ask({})",
                serde_json::to_string(dir.to_str().unwrap()).unwrap()
            )));
            match answer {
                Some(a) => std::fs::write(dir.join("answer.json"), a).unwrap(),
                // Holds the connection until the other side gives up.
                None => {
                    let _ = std::io::BufReader::new(&stream).read_line(&mut String::new());
                }
            }
            dir
        })
    }

    /// AC #2: the live read against a stand-in qutebrowser, and its fallback.
    #[test]
    fn the_live_read() {
        let d = fresh("live");
        let env = fake_qutebrowser(&d, &fixture("fallback.json"));
        let run_dir = d.join("run");
        std::fs::create_dir(&run_dir).unwrap();
        std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = run_dir.join("ipc-0123");
        let proc = fake_proc(&d, &["socket:[111]"], &[("111", &socket)]);
        let browser = fake_browser(&socket, Some(fixture("answer.json")));
        let (s, facts, notes) = run(Some(4242), &proc, &env, TIMEOUT).unwrap();
        let dir = browser.join().unwrap();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].starts_with("leaving out 2 key(s)"), "{notes:?}");
        assert_eq!(facts.unwrap()["mode"].to_string(), "insert");
        assert_eq!(find(&s, "normal", "comma l"), ["message-info e2e live"]);
        assert!(!dir.exists(), "the private directory is removed");
        assert!(dir.starts_with(&run_dir));

        // An error answer: the note, then the fallback.
        std::fs::remove_file(&socket).unwrap();
        let browser = fake_browser(&socket, Some(fixture("answer-error.json")));
        let (s, facts, notes) = run(Some(4242), &proc, &env, TIMEOUT).unwrap();
        let dir = browser.join().unwrap();
        assert!(facts.is_none() && !dir.exists());
        assert_eq!(
            notes,
            ["the running qutebrowser: AttributeError: module 'qutebrowser.keyinput.modeman' has no attribute \
              'instance'; reading its config files instead"]
        );
        assert_eq!(
            find(&s, "normal", "comma a"),
            ["message-info e2e autoconfig"]
        );

        // No answer in time: the note, the fallback, and nothing left behind.
        std::fs::remove_file(&socket).unwrap();
        let browser = fake_browser(&socket, None);
        let (_, facts, notes) = run(Some(4242), &proc, &env, Duration::from_millis(200)).unwrap();
        let dir = browser.join().unwrap();
        assert!(facts.is_none() && !dir.exists());
        assert_eq!(
            notes,
            ["the running qutebrowser: no answer within 0.2 s; reading its config files instead"]
        );

        // Nobody listening: the connect fails; a socket in a directory others can enter isn't used.
        std::fs::remove_file(&socket).unwrap();
        let (_, _, notes) = run(Some(4242), &proc, &env, TIMEOUT).unwrap();
        assert!(
            notes[0].starts_with("the running qutebrowser: connecting to "),
            "{notes:?}"
        );
        std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (_, _, notes) = run(Some(4242), &proc, &env, TIMEOUT).unwrap();
        assert!(
            notes[0].contains("isn't a directory only we can enter"),
            "{notes:?}"
        );
        assert_eq!(std::fs::read_dir(&run_dir).unwrap().count(), 0);
        // No socket: no live read, silently.
        let (_, _, notes) = run(Some(1), &proc, &env, TIMEOUT).unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn notes_escape_control_and_bidi_characters() {
        assert_eq!(
            printable("a\u{1b}[2J\u{202e}b\n"),
            "a\\u{1b}[2J\\u{202e}b\\n"
        );
    }

    #[test]
    fn by_window_class() {
        for c in ["qutebrowser", "QUTEBROWSER", "org.qutebrowser.qutebrowser"] {
            assert!(is_qutebrowser(c), "{c}");
        }
        assert!(!is_qutebrowser("qutebrowser2") && !is_qutebrowser("firefox"));
    }
}
