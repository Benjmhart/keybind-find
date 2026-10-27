//! nvim: the maps of the running nvim, asked over its server socket: its mode and the current buffer's filetype and
//! buffer-local maps (filetype and LSP maps) as well as the global ones. The mode is the one nvim was in when
//! Super+/ was pressed (keybind-find's picker doesn't change it). With no running nvim to ask, the maps of the user's
//! config, from `nvim --headless` and `nvim_get_keymap()`.

use crate::context::Context;
use crate::sheet::{chord, Bind, Name, Sheet};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    // No nvim found in the window (`dump` from a shell): nothing to ask, and nothing wrong.
    let Some(pid) = crate::context::nvim_pid() else {
        return parse(&keymap_json(None)?);
    };
    match live(pid, runtime_dir().as_deref()) {
        Ok((sheet, facts)) => {
            crate::context::set_facts(facts);
            Ok(sheet)
        }
        Err(e) => {
            eprintln!(
                "keybind-find: nvim: the running nvim: {e:#}; reading the config headless instead"
            );
            parse(&keymap_json(None)?)
        }
    }
}

/// The sheet and facts of the running nvim whose pid (TUI or server) is `pid`.
fn live(pid: u32, runtime: Option<&Path>) -> anyhow::Result<(Sheet, crate::scope::Facts)> {
    parse_live(&live_json(&socket(pid, runtime)?)?)
}

fn runtime_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|d| d.is_absolute())
}

/// The server socket of the nvim whose pid is `pid`: nvim 0.10+ runs its TUI as one process and the server as an
/// `--embed` child, so the pid and each child are tried, by a `--listen` path in the command line or nvim's default
/// `$XDG_RUNTIME_DIR/<appname>.<pid>.0` (`nvim`, or `NVIM_APPNAME`), whichever exists.
fn socket(pid: u32, runtime: Option<&Path>) -> anyhow::Result<std::path::PathBuf> {
    let children = |pid: u32| -> Vec<u32> {
        std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
            .flat_map(|c| {
                c.split_whitespace()
                    .filter_map(|p| p.parse().ok())
                    .collect::<Vec<u32>>()
            })
            .collect()
    };
    let cmdline = |pid: u32| -> Vec<String> {
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|b| {
                b.split(|&c| c == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let in_runtime: Vec<std::path::PathBuf> = runtime
        .and_then(|r| std::fs::read_dir(r).ok())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect();
    std::iter::once(pid)
        .chain(children(pid))
        .flat_map(|p| {
            let default = in_runtime.iter().find(|s| is_default_socket(s, p)).cloned();
            [listen_arg(&cmdline(p)), default]
        })
        .flatten()
        .find(|s| s.exists())
        .with_context(|| format!("no server socket for nvim (pid {pid})"))
}

/// The path after `--listen` in an nvim command line.
fn listen_arg(args: &[String]) -> Option<std::path::PathBuf> {
    let i = args.iter().position(|a| a == "--listen")?;
    args.get(i + 1).map(std::path::PathBuf::from)
}

/// Whether `path` is nvim's own socket for the server `pid` with no `--listen`: `<appname>.<pid>.0`, the app name
/// being `nvim` unless `NVIM_APPNAME` says otherwise.
fn is_default_socket(path: &Path, pid: u32) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(&format!(".{pid}.0")))
        .is_some_and(|app| !app.is_empty())
}

/// Asked of a running nvim with `--remote-expr`: its mode, the current buffer's filetype, and the global and
/// current-buffer maps of every mode keybind-find lists, lhs/rhs/desc only (a Lua callback can't be encoded). One
/// line of Vim script around one line of Lua, with no `"` inside.
const LIVE_EXPR: &str = "luaeval(\"(function() \
  local function maps(list) local r = {} for _, m in ipairs(list) do \
    table.insert(r, { lhs = m.lhs, rhs = m.rhs, desc = m.desc }) end return r end \
  local g, b = vim.empty_dict(), vim.empty_dict() \
  for _, mode in ipairs({ 'n', 'x', 'i', 'o', 'c', 't' }) do \
    local l = maps(vim.api.nvim_get_keymap(mode)) if #l > 0 then g[mode] = l end \
    local lb = maps(vim.api.nvim_buf_get_keymap(0, mode)) if #lb > 0 then b[mode] = lb end \
  end \
  return vim.json.encode({ mode = vim.api.nvim_get_mode().mode, filetype = vim.bo.filetype, global = g, buffer = b }) \
end)()\")";

/// Short, so a dead or busy nvim falls back to the headless read with time to spare.
const LIVE_TIMEOUT: Duration = Duration::from_secs(2);

fn live_json(socket: &Path) -> anyhow::Result<String> {
    let mut cmd = Command::new("nvim");
    cmd.arg("--server")
        .arg(socket)
        .args(["--remote-expr", LIVE_EXPR])
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    let (stdout, _) = capture(cmd, LIVE_TIMEOUT).context("asking it")?;
    Ok(stdout)
}

#[derive(Deserialize)]
struct Live {
    mode: String,
    #[serde(default)]
    filetype: String,
    global: BTreeMap<String, Vec<Mapping>>,
    buffer: BTreeMap<String, Vec<Mapping>>,
}

/// The sheet and facts from `LIVE_EXPR`'s answer. Every binding is scoped to its mode; the buffer's own also to
/// `buffer = current` and, when it has one, the buffer's filetype. A fact whose value can't be a `Name` is left out.
fn parse_live(json: &str) -> anyhow::Result<(Sheet, crate::scope::Facts)> {
    let live: Live =
        serde_json::from_str(json.trim()).context("parsing the running nvim's answer")?;
    let filetype = live.filetype.parse::<Name>().ok();
    let mut sheet = sheet_of(live.global, &[])?;
    let mut local = vec![("buffer".parse::<Name>()?, "current".parse::<Name>()?)];
    if let Some(f) = &filetype {
        local.push(("filetype".parse()?, f.clone()));
    }
    for (category, binds) in sheet_of(live.buffer, &local)?.binds {
        sheet.binds.entry(category).or_default().extend(binds);
    }
    let mut facts = crate::scope::Facts::new();
    // Only a mode the bindings are filed under: any other (`r`, a prompt) would hide every nvim binding.
    let mode = mode_name(&live.mode);
    if let (true, Ok(mode)) = (CATEGORIES.contains(&mode), mode.parse::<Name>()) {
        facts.insert("mode".parse()?, mode);
    }
    if let Some(f) = filetype {
        facts.insert("filetype".parse()?, f);
    }
    Ok((sheet, facts))
}

/// The mode categories the live query files bindings under.
const CATEGORIES: [&str; 6] = ["normal", "visual", "insert", "o", "c", "t"];

/// `nvim_get_mode().mode` as the category names the bindings use: `normal`, `visual`, `insert` (Replace too: insert
/// maps apply there), `o` for operator-pending, or the raw letter (select mode is `s`: the `x` maps listed don't
/// apply there, so it is no fact).
fn mode_name(mode: &str) -> &str {
    if mode.starts_with("no") {
        return "o";
    }
    match mode.chars().next() {
        Some('n') => "normal",
        Some('v' | 'V' | '\u{16}') => "visual",
        Some('i' | 'R') => "insert",
        _ => mode.get(..1).unwrap_or(""),
    }
}

/// Marks where the keymap starts on stdout, so whatever the user's config writes there first can't break it.
const SENTINEL: &str = "KEYBIND-FIND-KEYMAP";

/// lazy.nvim fires `VeryLazy`, where LazyVim and user `config/keymaps.lua` add their maps, only after `UIEnter`,
/// which headless nvim never sends: send it, then wait for lazy to finish (no wait without lazy.nvim).
const WAIT_FOR_LAZY: &str =
    "lua if package.loaded.lazy then vim.wait(2000, function() return vim.g.did_very_lazy end) end";

/// Copies only lhs/rhs/desc: Lua-callback mappings carry a function, which `vim.json.encode` can't encode. Empty
/// modes are left out (an empty Lua table may encode as `{}` or `[]`). A Lua error exits non-zero with the message
/// on stderr; nvim itself would exit 0 and print nothing on stdout.
const LUA: &str = r#"lua local ok, err = pcall(function()
  local out, any = {}, false
  for _, mode in ipairs({ "n", "x", "i" }) do
    local list = {}
    for _, m in ipairs(vim.api.nvim_get_keymap(mode)) do
      table.insert(list, { lhs = m.lhs, rhs = m.rhs, desc = m.desc })
    end
    if #list > 0 then out[mode] = list; any = true end
  end
  io.stdout:write("\n" .. "KEYBIND-FIND-KEYMAP" .. "\n" .. (any and vim.json.encode(out) or "{}"))
end)
if not ok then io.stderr:write(tostring(err) .. "\n"); vim.cmd("cquit 1") end"#;

/// A hung nvim (a plugin waiting on something) must not hang the picker.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Runs nvim headless with the user's config, or with `init` and no plugins, and returns the keymap JSON.
/// `-i NONE` keeps it from reading or writing shada. The user's config and plugins still run as they do on any start
/// (lazy.nvim's own caches and logs); this only reads the mappings.
fn keymap_json(init: Option<&Path>) -> anyhow::Result<String> {
    let mut cmd = Command::new("nvim");
    cmd.arg("--headless");
    if let Some(init) = init {
        cmd.arg("-u").arg(init).arg("--noplugin");
    }
    cmd.args(["-i", "NONE", "-c", "doautocmd UIEnter", "-c", WAIT_FOR_LAZY])
        .args(["-c", LUA, "-c", "qa!"])
        // Through a mise shim, a pinned-but-missing nvim must not start a download in the middle of a keypress.
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    let (stdout, stderr) = capture(cmd, TIMEOUT).context("nvim")?;
    match stdout.rfind(SENTINEL) {
        Some(at) => Ok(stdout[at + SENTINEL.len()..].trim().to_string()),
        None if stderr.trim().is_empty() => bail!("nvim printed no keymap"),
        None => bail!("nvim printed no keymap: {}", one_line(&stderr)),
    }
}

/// Runs `cmd` with null stdin; returns (stdout, stderr), decoded leniently. Fails, with stderr on one line, if it
/// can't start, exits non-zero, or it or anything holding its pipes runs past `timeout`.
// ponytail: close to context::run (1 s, no reader threads); merge them if a third caller appears.
fn capture(mut cmd: Command, timeout: Duration) -> anyhow::Result<(String, String)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("starting")?;
    // Read both pipes on threads so a large keymap can't fill a pipe while we wait.
    let (tx, rx) = mpsc::channel();
    let pipes: [(usize, Box<dyn Read + Send>); 2] = [
        (0, Box::new(child.stdout.take().context("no stdout pipe")?)),
        (1, Box::new(child.stderr.take().context("no stderr pipe")?)),
    ];
    for (i, mut pipe) in pipes {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send((i, buf));
        });
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        let waited = child.try_wait();
        match waited {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() <= deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                waited?;
                bail!("did not finish within {}s", timeout.as_secs());
            }
        }
    };
    // A background process started by the config can keep the pipes open after nvim exits.
    let mut out = [String::new(), String::new()];
    for _ in 0..2 {
        let left = deadline.saturating_duration_since(Instant::now());
        let (i, buf) = rx.recv_timeout(left).map_err(|_| {
            anyhow::anyhow!(
                "output stayed open past {}s (a process it started?)",
                timeout.as_secs()
            )
        })?;
        out[i] = String::from_utf8_lossy(&buf).into_owned();
    }
    let [stdout, stderr] = out;
    if !status.success() {
        bail!("failed ({status}): {}", one_line(&stderr));
    }
    Ok((stdout, stderr))
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Deserialize)]
struct Mapping {
    lhs: String,
    #[serde(default)]
    rhs: Option<String>,
    #[serde(default)]
    desc: Option<String>,
}

fn parse(json: &str) -> anyhow::Result<Sheet> {
    let modes: BTreeMap<String, Vec<Mapping>> =
        serde_json::from_str(json).context("parsing nvim's keymap JSON")?;
    sheet_of(modes, &[])
}

/// The sheet of `modes` (mode letter → maps), each binding scoped to its mode and to the `extra` scopes.
fn sheet_of(
    modes: BTreeMap<String, Vec<Mapping>>,
    extra: &[(Name, Name)],
) -> anyhow::Result<Sheet> {
    let mut binds: BTreeMap<String, Vec<Bind>> = BTreeMap::new();
    for (mode, mappings) in modes {
        let category = match mode.as_str() {
            "n" => "normal",
            "x" => "visual",
            "i" => "insert",
            other => other,
        };
        for m in mappings {
            let rhs = m.rhs.unwrap_or_default();
            if [&m.lhs, &rhs]
                .iter()
                .any(|s| s.contains("<Plug>") || s.contains("<SNR>"))
            {
                continue;
            }
            let desc = m
                .desc
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| rhs.clone());
            if desc.is_empty() {
                continue;
            }
            let Some(key) = key(&m.lhs) else {
                continue;
            };
            binds.entry(category.to_string()).or_default().push(Bind {
                key,
                desc,
                action: rhs,
                when: [(
                    "mode".parse()?,
                    category
                        .parse::<Name>()
                        .with_context(|| format!("nvim mode {mode:?}"))?,
                )]
                .into_iter()
                .chain(extra.iter().cloned())
                .map(|(k, v)| (k, v.into()))
                .collect(),
            });
        }
    }
    Ok(Sheet {
        title: "nvim".into(),
        provider: "nvim".into(),
        binds,
    })
}

/// nvim's lhs (`<C-S>`, ` ff`, `]q`, `<C-W><C-D>`) as space-separated chords. `None` if any key has no X keysym
/// we know (mouse, `<Plug>`, `<Cmd>`), which skips the mapping.
pub(crate) fn key(lhs: &str) -> Option<String> {
    let mut chords = Vec::new();
    let mut rest = lhs;
    while let Some(c) = rest.chars().next() {
        let special = rest
            .strip_prefix('<')
            .and_then(|r| {
                let end = r.find('>')?;
                // `<C->>` is ctrl+`>`: the first `>` is the key, not the end.
                let end = if r[..end].ends_with('-') && r[end + 1..].starts_with('>') {
                    end + 1
                } else {
                    end
                };
                Some(&r[..end])
            })
            .filter(|inner| !inner.is_empty());
        match special {
            Some(inner) => {
                chords.push(bracketed(inner)?);
                rest = &rest[inner.len() + 2..];
            }
            // A lone `<` (no closing `>`) is the key itself.
            None => {
                chords.push(single(c)?.to_string());
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    (!chords.is_empty()).then(|| chords.join(" "))
}

/// `C-S` → `ctrl+s`, `M-j` → `alt+j`, `S-Tab` → `shift+Tab`, `CR` → `Return`.
fn bracketed(inner: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut base = inner;
    while base.len() > 2 && base.as_bytes()[1] == b'-' {
        mods.push(match base.as_bytes()[0].to_ascii_lowercase() {
            b'c' => "ctrl",
            b'm' | b'a' => "alt",
            b's' => "shift",
            b'd' => "super",
            _ => return None,
        });
        base = &base[2..];
    }
    let keysym = match base.to_ascii_lowercase().as_str() {
        "cr" | "enter" | "return" => "Return".to_string(),
        "esc" => "Escape".into(),
        "tab" => "Tab".into(),
        "bs" => "BackSpace".into(),
        "space" => "space".into(),
        "up" => "Up".into(),
        "down" => "Down".into(),
        "left" => "Left".into(),
        "right" => "Right".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "Prior".into(),
        "pagedown" => "Next".into(),
        "del" => "Delete".into(),
        "lt" => "less".into(),
        "bslash" => "backslash".into(),
        "bar" => "bar".into(),
        "insert" | "ins" => "Insert".into(),
        "help" => "Help".into(),
        "undo" => "Undo".into(),
        // <Nul> is what a terminal sends for ctrl+@.
        "nul" => return Some(chord(&[&mods[..], &["ctrl"]].concat(), "at")),
        "kenter" => "KP_Enter".into(),
        "kplus" => "KP_Add".into(),
        "kminus" => "KP_Subtract".into(),
        "kmultiply" => "KP_Multiply".into(),
        "kdivide" => "KP_Divide".into(),
        "kpoint" => "KP_Decimal".into(),
        k if k.len() == 2 && k.starts_with('k') && k.as_bytes()[1].is_ascii_digit() => {
            format!("KP_{}", &k[1..])
        }
        f if f.len() > 1
            && f.starts_with('f')
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=35).contains(&n)) =>
        {
            format!("F{}", &f[1..])
        }
        _ => {
            let mut chars = base.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            // nvim writes ctrl chords uppercase (`<C-S>`); a terminal can't tell ctrl+S from ctrl+s.
            let c = if mods.contains(&"ctrl") {
                c.to_ascii_lowercase()
            } else {
                c
            };
            single(c)?.to_string()
        }
    };
    Some(chord(&mods, &keysym))
}

/// One typed character as an X keysym name: letters and digits as they are, punctuation by name.
pub(crate) fn single(c: char) -> Option<&'static str> {
    const ALNUM: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    if let Some(i) = ALNUM.find(c) {
        return Some(&ALNUM[i..i + 1]);
    }
    Some(match c {
        ' ' => "space",
        '/' => "slash",
        '-' => "minus",
        '[' => "bracketleft",
        ']' => "bracketright",
        ',' => "comma",
        '.' => "period",
        ';' => "semicolon",
        ':' => "colon",
        '\'' => "apostrophe",
        '"' => "quotedbl",
        '<' => "less",
        '>' => "greater",
        '=' => "equal",
        '+' => "plus",
        '_' => "underscore",
        '?' => "question",
        '!' => "exclam",
        '@' => "at",
        '#' => "numbersign",
        '$' => "dollar",
        '%' => "percent",
        '^' => "asciicircum",
        '&' => "ampersand",
        '*' => "asterisk",
        '(' => "parenleft",
        ')' => "parenright",
        '{' => "braceleft",
        '}' => "braceright",
        '|' => "bar",
        '\\' => "backslash",
        '`' => "grave",
        '~' => "asciitilde",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nvim/init.lua")
    }

    fn get<'a>(s: &'a Sheet, category: &str, key: &str) -> Option<&'a Bind> {
        s.binds.get(category)?.iter().find(|b| b.key == key)
    }

    #[test]
    fn key_translation() {
        for (lhs, want) in [
            ("<C-s>", "ctrl+s"),
            ("<C-S>", "ctrl+s"),
            ("<M-j>", "alt+j"),
            ("<A-j>", "alt+j"),
            ("<CR>", "Return"),
            ("<Esc>", "Escape"),
            ("<S-Tab>", "shift+Tab"),
            ("gd", "g d"),
            ("]q", "bracketright q"),
            ("K", "K"),
            (" ff", "space f f"),
            ("<Space>ff", "space f f"),
            ("<C-W><C-D>", "ctrl+w ctrl+d"),
            ("<C-W> ", "ctrl+w space"),
            ("<C-Space>", "ctrl+space"),
            ("[<C-T>", "bracketleft ctrl+t"),
            ("<lt>", "less"),
            ("<", "less"),
            ("<a", "less a"),
            ("<F5>", "F5"),
            ("<C-->", "ctrl+minus"),
            ("<PageDown>", "Next"),
            ("<C->>", "ctrl+greater"),
            ("<M->>x", "alt+greater x"),
            ("<Insert>", "Insert"),
            ("<Nul>", "ctrl+at"),
            ("<kPlus>", "KP_Add"),
            ("<k7>", "KP_7"),
            ("<F13>", "F13"),
        ] {
            assert_eq!(key(lhs).as_deref(), Some(want), "{lhs:?}");
        }
        for lhs in [
            "<LeftMouse>",
            "<ScrollWheelUp>",
            "<Plug>(x)",
            "<Cmd>",
            "<F36>",
            "<T-x>",
            "",
            "é",
        ] {
            assert_eq!(key(lhs), None, "{lhs:?}");
        }
    }

    #[test]
    fn parse_skips_and_falls_back() {
        let json = r#"{
            "n": [
                {"lhs": "gd", "rhs": "", "desc": "Go to definition"},
                {"lhs": "<Plug>(x)", "rhs": ":echo<CR>"},
                {"lhs": "a", "rhs": "<Plug>(y)"},
                {"lhs": "b", "rhs": "<SNR>12_f()"},
                {"lhs": "c"},
                {"lhs": "<LeftMouse>", "rhs": "x", "desc": "mouse"},
                {"lhs": "Y", "rhs": "y$"}
            ],
            "x": [],
            "i": [{"lhs": " cb", "desc": "Callback"}]
        }"#;
        let s = parse(json).unwrap();
        assert_eq!((s.title.as_str(), s.provider.as_str()), ("nvim", "nvim"));
        let normal: Vec<(&str, &str, &str)> = s.binds["normal"]
            .iter()
            .map(|b| (b.key.as_str(), b.desc.as_str(), b.action.as_str()))
            .collect();
        assert_eq!(normal, [("g d", "Go to definition", ""), ("Y", "y$", "y$")]);
        assert!(!s.binds.contains_key("visual"));
        let cb = get(&s, "insert", "space c b").unwrap();
        assert_eq!((cb.desc.as_str(), cb.action.as_str()), ("Callback", ""));
        // Each binding is scoped to its mode, and keeps its mode category.
        fn mode(b: &Bind) -> (Vec<&str>, usize) {
            let v = b.when["mode"].iter().map(Name::as_str).collect();
            (v, b.when.len())
        }
        assert_eq!(mode(cb), (vec!["insert"], 1));
        assert!(s.binds["normal"]
            .iter()
            .all(|b| mode(b) == (vec!["normal"], 1)));
        let x = parse(r#"{"x": [{"lhs": "gc", "desc": "Comment"}]}"#).unwrap();
        assert_eq!(mode(&x.binds["visual"][0]), (vec!["visual"], 1));
        // A mode the dump doesn't ask for keeps nvim's own letter as both.
        let o = parse(r#"{"o": [{"lhs": "a", "desc": "d"}]}"#).unwrap();
        assert_eq!(mode(&o.binds["o"][0]), (vec!["o"], 1));
        assert!(parse("not json").is_err());
    }

    #[test]
    fn fixture_runs_in_real_nvim() {
        let s = parse(&keymap_json(Some(&fixture())).unwrap()).unwrap();
        let find = get(&s, "normal", "space f f").expect("space f f");
        assert_eq!(find.desc, "Find files");
        assert_eq!(get(&s, "normal", "ctrl+s").unwrap().desc, "Save");
        assert_eq!(get(&s, "visual", "alt+j").unwrap().desc, "Move down");
        let jk = get(&s, "insert", "j k").unwrap();
        assert_eq!((jk.desc.as_str(), jk.action.as_str()), ("<Esc>", "<Esc>"));
        let cb = get(&s, "normal", "space x").unwrap();
        assert_eq!((cb.desc.as_str(), cb.action.as_str()), ("Callback map", ""));
        // Added on VeryLazy, after UIEnter: only there if the provider sends UIEnter and waits.
        assert_eq!(get(&s, "normal", "space w").unwrap().desc, "Late map");
        // The fixture's `<Plug>(kbf-test)` did load, and was left out.
        let raw = keymap_json(Some(&fixture())).unwrap();
        assert!(raw.contains("<Plug>(kbf-test)"));
        assert!(s.binds.values().flatten().all(|b| b.desc != ":echo 2<CR>"));
    }

    #[test]
    fn socket_paths() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            listen_arg(&args(&["nvim", "--listen", "/tmp/n.sock", "f.rs"])),
            Some("/tmp/n.sock".into())
        );
        assert_eq!(listen_arg(&args(&["nvim", "f.rs"])), None);
        assert_eq!(listen_arg(&args(&["nvim", "--listen"])), None);
        assert!(is_default_socket(
            Path::new("/run/user/1000/nvim.4242.0"),
            4242
        ));
        // NVIM_APPNAME names it.
        assert!(is_default_socket(
            Path::new("/run/user/1000/lazyvim.4242.0"),
            4242
        ));
        assert!(!is_default_socket(
            Path::new("/run/user/1000/nvim.42420.0"),
            4242
        ));
        assert!(!is_default_socket(
            Path::new("/run/user/1000/nvim.4242.1"),
            4242
        ));
        assert!(!is_default_socket(
            Path::new("/run/user/1000/.4242.0"),
            4242
        ));
        // A pid with no process and no socket: an error naming it, for the fallback line.
        let e = socket(u32::MAX, Some(Path::new("/nonexistent"))).unwrap_err();
        assert!(e.to_string().contains("no server socket"), "{e}");
    }

    #[test]
    fn live_answer_scopes_buffer_maps_and_reports_facts() {
        let json = r#"{"mode": "n", "filetype": "rust",
            "global": {"n": [{"lhs": "gd", "desc": "Definition"}], "i": [{"lhs": "<C-S>", "rhs": "<Esc>:w<CR>"}],
                       "t": [{"lhs": "<Esc><Esc>", "desc": "Leave terminal"}]},
            "buffer": {"n": [{"lhs": " ca", "desc": "Code action"}]}}"#;
        let (s, facts) = parse_live(json).unwrap();
        let when = |b: &Bind| -> Vec<String> {
            b.when
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        v.iter().map(Name::as_str).collect::<Vec<_>>().join(",")
                    )
                })
                .collect()
        };
        assert_eq!(when(get(&s, "normal", "g d").unwrap()), ["mode=normal"]);
        assert_eq!(when(get(&s, "insert", "ctrl+s").unwrap()), ["mode=insert"]);
        assert_eq!(when(get(&s, "t", "Escape Escape").unwrap()), ["mode=t"]);
        assert_eq!(
            when(get(&s, "normal", "space c a").unwrap()),
            ["buffer=current", "filetype=rust", "mode=normal"]
        );
        let facts: Vec<String> = facts.iter().map(|(k, v)| format!("{k}={v}")).collect();
        assert_eq!(facts, ["filetype=rust", "mode=normal"]);
        // In normal mode the insert map is hidden and the buffer's map shown, tagged only as the buffer's.
        let (s, facts) = parse_live(json).unwrap();
        let entries = crate::scope::shown(&[s], &facts).unwrap();
        let keys: Vec<(&str, String)> = entries
            .iter()
            .map(|e| (e.bind.key.as_str(), crate::scope::tags(&e.bind.when)))
            .collect();
        assert_eq!(
            keys,
            [
                ("g d", String::new()),
                ("space c a", " [buffer=current]".to_string())
            ]
        );
        // No filetype: no filetype fact or scope; an odd one (spaces) is left out rather than failing.
        let (s, facts) = parse_live(r#"{"mode": "i", "filetype": "", "global": {}, "buffer": {"i": [{"lhs": "jk", "desc": "x"}]}}"#).unwrap();
        assert_eq!(
            when(get(&s, "insert", "j k").unwrap()),
            ["buffer=current", "mode=insert"]
        );
        assert_eq!(facts.len(), 1);
        assert!(
            parse_live(r#"{"mode": "n", "filetype": "a b", "global": {}, "buffer": {}}"#).is_ok()
        );
        // A mode no binding is filed under (a hit-enter prompt) is no fact: it would hide every binding.
        let (_, facts) =
            parse_live(r#"{"mode": "r", "filetype": "", "global": {}, "buffer": {}}"#).unwrap();
        assert!(facts.is_empty());
        assert!(parse_live("not json").is_err());
    }

    #[test]
    fn mode_names() {
        for (mode, want) in [
            ("n", "normal"),
            ("no", "o"),
            ("nov", "o"),
            ("niI", "normal"),
            ("v", "visual"),
            ("V", "visual"),
            ("\u{16}", "visual"),
            ("i", "insert"),
            ("ic", "insert"),
            ("c", "c"),
            ("t", "t"),
            ("R", "insert"),
            ("Rv", "insert"),
            ("s", "s"),
            ("r?", "r"),
            ("!", "!"),
            ("", ""),
        ] {
            assert_eq!(mode_name(mode), want, "{mode:?}");
        }
    }

    /// AC: a real `nvim --headless --listen` with a buffer-local map, asked live; and the fallback when its socket
    /// is gone.
    #[test]
    fn a_running_nvim_is_asked_for_its_buffer_maps() {
        struct KillOnDrop(std::process::Child);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let dir = std::env::temp_dir().join(format!("kbf-nvim-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("nvim.sock");
        let _nvim = KillOnDrop(
            Command::new("nvim")
                .args(["--headless", "-u", "NONE", "-i", "NONE", "--listen"])
                .arg(&sock)
                .args([
                    "-c",
                    "set filetype=rust",
                    "-c",
                    "nnoremap <buffer> <Space>ca <Cmd>echo<CR>",
                ])
                .args([
                    "-c",
                    "lua vim.keymap.set('i', 'jk', '<Esc>', { desc = 'Leave insert' })",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut answer = Err(anyhow::anyhow!("never asked"));
        for _ in 0..100 {
            if sock.exists() {
                answer = live_json(&sock);
                if answer.is_ok() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let (s, facts) = parse_live(&answer.unwrap()).unwrap();
        // Found from the pid alone, by the `--listen` in its command line.
        assert_eq!(socket(_nvim.0.id(), None).unwrap(), sock);
        assert!(live(_nvim.0.id(), None).is_ok());
        let ca = get(&s, "normal", "space c a").unwrap();
        assert_eq!(ca.desc, "<Cmd>echo<CR>");
        assert!(ca.when.contains_key("buffer") && ca.when.contains_key("filetype"));
        assert_eq!(get(&s, "insert", "j k").unwrap().desc, "Leave insert");
        let facts: Vec<String> = facts.iter().map(|(k, v)| format!("{k}={v}")).collect();
        assert_eq!(facts, ["filetype=rust", "mode=normal"]);
        // The socket gone (nvim quit): asking fails, which is the fallback's cue.
        drop(_nvim);
        let _ = std::fs::remove_file(&sock);
        assert!(live_json(&sock).is_err());
        // With no `--listen`, nvim's default socket in the runtime dir, found from the pid.
        let runtime = dir.join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        let plain = KillOnDrop(
            Command::new("nvim")
                .args(["--headless", "-u", "NONE", "-i", "NONE"])
                .env("XDG_RUNTIME_DIR", &runtime)
                .env_remove("NVIM_APPNAME")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut found = Err(anyhow::anyhow!("never looked"));
        for _ in 0..100 {
            found = socket(plain.0.id(), Some(&runtime));
            if found.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            found.unwrap(),
            runtime.join(format!("nvim.{}.0", plain.0.id()))
        );
        drop(plain);
        // A TUI's server is its `--embed` child: the socket is found through the children of the pid given. A shell
        // stands in for the TUI here.
        let parent = KillOnDrop(
            Command::new("sh")
                .args(["-c", "nvim --headless -u NONE -i NONE & wait"])
                .env("XDG_RUNTIME_DIR", &runtime)
                .env_remove("NVIM_APPNAME")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut found = Err(anyhow::anyhow!("never looked"));
        for _ in 0..100 {
            found = socket(parent.0.id(), Some(&runtime));
            if found.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let found = found.unwrap();
        let name = found.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("nvim.") && !name.contains(&parent.0.id().to_string()),
            "{name}"
        );
        // Killing the shell would leave its nvim: kill the group of the shell's children by pid in the name.
        let child: u32 = name
            .trim_start_matches("nvim.")
            .trim_end_matches(".0")
            .parse()
            .unwrap();
        let _ = Command::new("kill").arg(child.to_string()).status();
        drop(parent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs the provider's nvim command with `init_lua` as the config.
    fn with_init(name: &str, init_lua: &str) -> anyhow::Result<String> {
        let dir = std::env::temp_dir().join(format!("kbf-nvim-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("init.lua");
        std::fs::write(&path, init_lua).unwrap();
        let got = keymap_json(Some(&path));
        let _ = std::fs::remove_dir_all(&dir);
        got
    }

    #[test]
    fn stdout_written_by_the_config_is_ignored() {
        let json = with_init("junk", "io.stdout:write('junk from a plugin\\n{')").unwrap();
        assert!(parse(&json).is_ok(), "{json}");
    }

    #[test]
    fn invalid_utf8_in_a_desc_keeps_the_sheet() {
        let init = r#"vim.keymap.set("n", "Q", "q", { desc = "bad \255 byte" })"#;
        let s = parse(&with_init("utf8", init).unwrap()).unwrap();
        assert!(get(&s, "normal", "Q").unwrap().desc.starts_with("bad "));
    }

    #[test]
    fn errors_carry_nvims_stderr() {
        let e = format!(
            "{:#}",
            with_init(
                "cquit",
                "io.stderr:write('kbf-boom\\n'); vim.cmd('cquit 3')"
            )
            .unwrap_err()
        );
        assert!(e.contains("kbf-boom"), "{e}");
        // Exits 0 before the snippet runs: no keymap, and the message says so with stderr.
        let e = format!(
            "{:#}",
            with_init("early", "io.stderr:write('kbf-early\\n'); vim.cmd('qa!')").unwrap_err()
        );
        assert!(
            e.contains("printed no keymap") && e.contains("kbf-early"),
            "{e}"
        );
    }

    #[test]
    fn capture_reports_failures() {
        let mut fail = Command::new("sh");
        fail.args(["-c", "echo first >&2; echo second >&2; exit 3"]);
        let e = format!("{:#}", capture(fail, TIMEOUT).unwrap_err());
        assert!(e.contains("first second"), "{e}");
        let e = format!(
            "{:#}",
            capture(Command::new("/nonexistent/nvim"), TIMEOUT).unwrap_err()
        );
        assert!(e.contains("starting"), "{e}");
        let mut slow = Command::new("sleep");
        slow.arg("5");
        let e = format!(
            "{:#}",
            capture(slow, Duration::from_millis(200)).unwrap_err()
        );
        assert!(e.contains("did not finish"), "{e}");
        let mut big = Command::new("sh");
        // More than a pipe buffer (64 KiB) on stdout must not deadlock.
        big.args(["-c", "head -c 200000 /dev/zero | tr '\\0' x"]);
        assert_eq!(capture(big, TIMEOUT).unwrap().0.len(), 200_000);
        // A background child holding stdout open past the deadline is an error, not a hang.
        let mut holder = Command::new("sh");
        holder.args(["-c", "sleep 3 & echo started"]);
        let started = Instant::now();
        let e = format!(
            "{:#}",
            capture(holder, Duration::from_millis(300)).unwrap_err()
        );
        assert!(e.contains("stayed open"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
