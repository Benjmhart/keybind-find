//! vim: the user's maps, from `:map`, `:map!`, `:tmap` and `:lmap`: asked of the running vim over its clientserver
//! when it has a server (with its buffer's maps, its mode and filetype), else from a headless vim (`vim -es`) started
//! with the user's vimrc. vim's built-in keys are not listed: `:map` shows none, and they are vim's own
//! documentation's. Shown when vim or vi is the program in the focused terminal, or a gvim window is focused. A `vim`
//! that is nvim gives nvim's sheet.

use crate::context::{run_for, run_for_status, Context};
use crate::scope::Facts;
use crate::sheet::{Bind, Name, Sheet, Values};
use anyhow::{bail, ensure, Context as _};
use serde::Deserialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// A slow vimrc (plugin managers) must not hang the picker; nvim's limit.
const TIMEOUT: Duration = Duration::from_secs(10);

/// `--version` answers at once; it mustn't add a second long wait to the run's.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Marks where the maps start on stdout: anything a vimrc or plugin prints before it is ignored.
const SENTINEL: &str = "KEYBIND-FIND-VIM";

/// Marks where they end: a vim that stopped part way through isn't taken for one with fewer maps.
const END: &str = "KEYBIND-FIND-VIM-DONE";

/// The running vim's answer to `--remote-expr` comes at once; a busy or gone one mustn't hold up the picker.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

/// `--serverlist` only lists a directory (or asks the X server).
const SERVERLIST_TIMEOUT: Duration = Duration::from_secs(1);

/// What the running vim is asked: its pid (to tell it from another vim of the same name), its mode, its buffer's
/// filetype and the maps, as `:map` prints them. A constant: no text of the user's goes in.
const LIVE_EXPR: &str = r#"json_encode({"pid": getpid(), "mode": mode(1), "filetype": &filetype, "maps": execute(["map", "map!", "silent! tmap", "silent! lmap"])})"#;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let (sheet, notes, facts) = sheet_with(
        ctx,
        crate::context::editor_pid(),
        Path::new("/proc"),
        |_| {},
        &|k| std::env::var(k).ok(),
    )?;
    if let Some(facts) = facts {
        crate::context::set_facts(facts);
    }
    for note in notes {
        eprintln!("keybind-find: vim: {note}");
    }
    Ok(sheet)
}

/// `sheet` for the vim at `pid` (none: `dump vim` from a shell), its process read under `proc`, with `setup`
/// applied to every run and `env` for `PATH`, the vimrc and `VIMINIT` (test seams). Also the lines for stderr and,
/// from a running vim, the facts.
fn sheet_with(
    ctx: &Context,
    pid: Option<u32>,
    proc: &Path,
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>, Option<Facts>)> {
    // Debian's vim-tiny has a `vi` and no `vim`.
    let program = program(env).context("neither vim nor vi is on PATH")?;
    let mut version = Command::new(program);
    version
        .arg("--version")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut version);
    let version = run_for(version, VERSION_TIMEOUT)
        .with_context(|| format!("running {program} --version"))?;
    if is_nvim(&version) {
        return Ok((super::nvim::sheet(ctx)?, Vec::new(), None));
    }
    if !version.trim_start().starts_with("VIM") {
        bail!(
            "{program} is not vim ({}); its keys aren't read",
            version.lines().next().unwrap_or_default().trim()
        );
    }
    // `:echo` and so `:redir`'s sentinels need +eval, which vim-tiny is built without.
    if version.split_whitespace().any(|w| w == "-eval") {
        bail!("{program} is built without +eval (vim-tiny), so its maps can't be read");
    }
    let mut notes = Vec::new();
    if let Some(pid) = pid {
        match live(pid, program, proc, &setup) {
            Ok(Some((sheet, facts, skipped))) => {
                notes.extend(skipped_note(skipped));
                return Ok((sheet, notes, Some(facts)));
            }
            // No server to ask: the common case, and nothing wrong.
            Ok(None) => {}
            Err(e) => notes.push(format!(
                "the running vim: {e:#}; reading the vimrc headless instead"
            )),
        }
    }
    let (sheet, more) = read(program, TIMEOUT, setup, env)?;
    notes.extend(more);
    Ok((sheet, notes, None))
}

/// The line for maps left out, when there are any.
fn skipped_note(skipped: usize) -> Option<String> {
    (skipped > 0).then(|| {
        let maps = if skipped == 1 { "map" } else { "maps" };
        format!("skipping {skipped} {maps} with no rhs or a key that has no keysym we know")
    })
}

/// The running vim at `pid` asked over its clientserver: the sheet (with its buffer's maps), the facts (`mode`,
/// `filetype`) and the maps left out. `Ok(None)` when it can't be asked: a vim without `+clientserver`, or one that
/// runs no server. The client is that vim's own executable (`proc/<pid>/exe`, else `program`): socket servers before
/// patch 9.2.511 don't understand later ones. Each server name is tried in turn (`server_names`), and an answer from
/// another vim (another pid) is passed over. `proc/<pid>/exe` is run itself, so a vim whose file an upgrade replaced
/// still answers through its own client.
fn live(
    pid: u32,
    program: &str,
    proc: &Path,
    setup: &impl Fn(&mut Command),
) -> anyhow::Result<Option<(Sheet, Facts, usize)>> {
    let dir = proc.join(pid.to_string());
    // Only a vim's own executable is run (a window's `_NET_WM_PID` is the client's word).
    let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
    let comm = comm.trim_end();
    if !(comm == "vi" || comm.contains("vim")) {
        return Ok(None);
    }
    let exe = match std::fs::read_link(dir.join("exe")) {
        Ok(_) => dir.join("exe"),
        Err(_) => PathBuf::from(program),
    };
    let nul_list = |name: &str| -> Vec<String> {
        std::fs::read(dir.join(name))
            .map(|b| {
                b.split(|&c| c == 0)
                    .filter(|a| !a.is_empty())
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    let (cmdline, environ) = (nul_list("cmdline"), nul_list("environ"));
    // The backend for the client's requests (not for `--version`).
    let client = |args: &[&str]| {
        let mut cmd = Command::new(&exe);
        // vim reads its name for its mode: `exe` would start Ex mode.
        std::os::unix::process::CommandExt::arg0(&mut cmd, "vim");
        if let (Some(b), false) = (backend(&cmdline, &environ), args == ["--version"]) {
            cmd.args(["--clientserver", &b]);
        }
        cmd.args(args)
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
            .env("LC_ALL", "C.UTF-8")
            .current_dir("/");
        // Where the server is: its own runtime and temp directories (socket) and X display, not ours.
        for k in ["XDG_RUNTIME_DIR", "TMPDIR", "DISPLAY", "XAUTHORITY"] {
            match environ
                .iter()
                .find_map(|e| e.strip_prefix(&format!("{k}=")))
            {
                Some(v) => cmd.env(k, v),
                None => cmd.env_remove(k),
            };
        }
        setup(&mut cmd);
        cmd
    };
    // Another vi (nvi, busybox), or a vim that can't be asked.
    let version = run_for(client(&["--version"]), VERSION_TIMEOUT).unwrap_or_default();
    if !version.trim_start().starts_with("VIM")
        || !version.split_whitespace().any(|w| w == "+clientserver")
    {
        return Ok(None);
    }
    let listed = run_for(client(&["--serverlist"]), SERVERLIST_TIMEOUT).unwrap_or_default();
    let cwd = std::fs::read_link(dir.join("cwd")).ok();
    let names = server_names(&cmdline, &listed, cwd.as_deref());
    if names.is_empty() {
        return Ok(None);
    }
    // One time limit for every name together: a stopped vim (Ctrl-Z) holds its socket but never answers.
    let deadline = std::time::Instant::now() + QUERY_TIMEOUT;
    let mut why = Vec::new();
    for name in &names {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            why.push(format!("{name}: no time left"));
            break;
        }
        let answer = run_for(
            client(&["--servername", name, "--remote-expr", LIVE_EXPR]),
            left,
        )
        .and_then(|json| parse_live(&json, pid));
        match answer {
            Ok(Some(found)) => return Ok(Some(found)),
            // Another vim's server: this one runs none of those asked.
            Ok(None) => {}
            Err(e) => why.push(format!("{name}: {e:#}")),
        }
    }
    // Only other vims answered: this one runs no server, the common case.
    if why.is_empty() {
        return Ok(None);
    }
    bail!("{}", why.join("; "))
}

/// The value after the last `flag` among vim's options (before `--`, after which come file names).
fn option(cmdline: &[String], flag: &str) -> Option<String> {
    let options = cmdline.get(1..).unwrap_or_default();
    let options = &options[..options
        .iter()
        .position(|a| a == "--")
        .unwrap_or(options.len())];
    options
        .windows(2)
        .rev()
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

/// The server names to try for a vim with this command line, in order: its `--servername` (a relative socket path
/// made absolute against the vim's working directory `cwd`), then each name `--serverlist` printed; each once.
fn server_names(cmdline: &[String], serverlist: &str, cwd: Option<&Path>) -> Vec<String> {
    let given = option(cmdline, "--servername")
        .filter(|n| !n.is_empty())
        .map(|n| match cwd {
            Some(cwd) if n.starts_with("./") || n.starts_with("../") => {
                cwd.join(&n).to_string_lossy().into_owned()
            }
            _ => n,
        });
    let mut names: Vec<String> = Vec::new();
    for n in given
        .into_iter()
        .chain(serverlist.lines().map(|l| l.trim().to_string()))
    {
        if !n.is_empty() && !names.contains(&n) {
            names.push(n);
        }
    }
    names
}

/// The clientserver backend the vim chose, for its client to use the same: its `--clientserver` argument, else
/// `VIM_CLIENTSERVER` from its environment.
fn backend(cmdline: &[String], environ: &[String]) -> Option<String> {
    option(cmdline, "--clientserver")
        .or_else(|| {
            environ
                .iter()
                .find_map(|e| e.strip_prefix("VIM_CLIENTSERVER="))
                .map(str::to_string)
        })
        .filter(|b| !b.is_empty())
}

#[derive(Deserialize)]
struct Live {
    pid: u32,
    mode: String,
    filetype: String,
    maps: String,
}

/// The sheet, facts and maps left out from `LIVE_EXPR`'s answer; `None` when another vim answered (not `pid`). vim
/// exits 0 for a server it can't reach (E573 on stderr), so an empty or non-JSON answer is a failed query.
fn parse_live(json: &str, pid: u32) -> anyhow::Result<Option<(Sheet, Facts, usize)>> {
    ensure!(!json.trim().is_empty(), "no answer");
    let live: Live =
        serde_json::from_str(json.trim()).context("parsing the running vim's answer")?;
    if live.pid != pid {
        return Ok(None);
    }
    let filetype = live.filetype.parse::<Name>().ok();
    let mut local = vec![("buffer".parse::<Name>()?, "current".parse::<Name>()?)];
    if let Some(f) = &filetype {
        local.push(("filetype".parse()?, f.clone()));
    }
    let (sheet, skipped) = parse(&live.maps, Some(&local))?;
    let mut facts = Facts::new();
    if let Some(mode) = mode_name(&live.mode) {
        facts.insert("mode".parse()?, mode.parse()?);
    }
    if let Some(f) = filetype {
        facts.insert("filetype".parse()?, f);
    }
    Ok(Some((sheet, facts, skipped)))
}

/// `mode(1)` as the category names vim's maps are filed under; `None` for a mode with no maps of its own (`r`, `!`),
/// which would hide them all.
fn mode_name(mode: &str) -> Option<&'static str> {
    if mode.starts_with("no") {
        return Some("operator");
    }
    Some(match mode.chars().next()? {
        'n' => "normal",
        'v' | 'V' | '\u{16}' => "visual",
        's' | 'S' | '\u{13}' => "select",
        'i' | 'R' => "insert",
        'c' => "command",
        't' => "terminal",
        _ => return None,
    })
}

/// `vim`, else `vi`, whichever is on `PATH` first in that order.
fn program(env: Env) -> Option<&'static str> {
    let path = env("PATH").unwrap_or_default();
    ["vim", "vi"].into_iter().find(|p| {
        path.split(':')
            .filter(|d| !d.is_empty())
            .any(|d| Path::new(d).join(p).is_file())
    })
}

/// Whether `vim --version` is nvim's (a `vim` that is a symlink to or a wrapper of nvim).
fn is_nvim(version: &str) -> bool {
    version.trim_start().starts_with("NVIM")
}

/// Runs `program` headless with `setup` applied (a test seam: `PATH`, `HOME`) and the vimrc found through `env`.
/// Also returns the lines for stderr.
fn read(
    program: &str,
    timeout: Duration,
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    if env("VIMINIT").is_some_and(|v| !v.is_empty()) {
        bail!("VIMINIT is set, and vim then reads it instead of a vimrc; it isn't read here");
    }
    let vimrc = vimrc(env);
    let mut cmd = Command::new(program);
    cmd.args(args(vimrc.as_deref()))
        // Through a mise shim, a pinned-but-missing vim must not start a download in the middle of a keypress.
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
        // English messages and UTF-8 keys, whatever the session's locale.
        .env("LC_ALL", "C.UTF-8")
        // A vimrc with `set exrc` would also source a `.vimrc` from the working directory: give it none of ours.
        .current_dir("/");
    if let Some(v) = &vimrc {
        // `-u` leaves `$MYVIMRC` unset, which vimrcs use to find their own directory.
        cmd.env("MYVIMRC", v);
    }
    setup(&mut cmd);
    // vim exits 1 after any error in a vimrc, with its maps printed all the same: the sentinels say it ran.
    let (status, stdout, stderr) = run_for_status(cmd, timeout).context("running vim headless")?;
    let marker = format!("\n{SENTINEL}\n");
    let Some(at) = stdout.rfind(&marker) else {
        let why = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        bail!(
            "vim printed no maps ({status}{}{why}); the vimrc or a VimEnter autocommand may have quit vim",
            if why.is_empty() { "" } else { ": " }
        );
    };
    let rest = &stdout[at + marker.len()..];
    let Some(end) = rest.rfind(&format!("\n{END}")) else {
        bail!("vim stopped before it had listed every map ({status})");
    };
    let (sheet, skipped) = parse(&rest[..end], None)?;
    Ok((sheet, skipped_note(skipped).into_iter().collect()))
}

/// The vimrc vim reads, the first that exists of `~/.vimrc`, `~/.vim/vimrc` and `$XDG_CONFIG_HOME/vim/vimrc`
/// (`~/.config/vim/vimrc` when unset; none when set but empty or relative, as vim reads nothing there either).
/// `None`: there is none. vim puts the XDG one's directory in `runtimepath` itself when `-u` names it.
fn vimrc(env: Env) -> Option<PathBuf> {
    let home = env("HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    let xdg = match env("XDG_CONFIG_HOME") {
        None => home.as_ref().map(|h| h.join(".config")),
        Some(d) => Some(PathBuf::from(d)).filter(|d| d.is_absolute()),
    };
    [
        home.as_ref().map(|h| h.join(".vimrc")),
        home.as_ref().map(|h| h.join(".vim/vimrc")),
        xdg.map(|d| d.join("vim/vimrc")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file())
}

/// vim's arguments. `-es` reads no vimrc by itself, so `-u` names it; with none, `NORC` and `defaults.vim`, as a
/// plain vim start loads. `VimEnter` is not sent: a vimrc's VimEnter autocommands (vim-plug's install-on-start)
/// would run on every keypress and be killed part way. `:tmap` and `:lmap` are `silent!`: a vim without them still
/// lists the rest. At most ten `-c`, vim's limit.
fn args(vimrc: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-es", "-N", "-i", "NONE", "-u"].map(OsString::from).into();
    match vimrc {
        Some(path) => args.push(path.into()),
        None => args.extend(["NORC", "-c", "source $VIMRUNTIME/defaults.vim"].map(OsString::from)),
    }
    let echo = format!("echo \"{SENTINEL}\"");
    let end = format!("echo \"{END}\" | redir END | qa!");
    for c in [
        "redir! > /dev/stdout",
        &echo,
        "silent map",
        "silent map!",
        "silent! tmap",
        "silent! lmap",
        &end,
    ] {
        args.extend(["-c", c].map(OsString::from));
    }
    args
}

/// The sheet of `:map` output, and the maps skipped for a key with no keysym. Each line is the mode letters (three
/// columns), the lhs padded to column 15 (or one space after a longer one), two flag columns (`*` no remap, `&`
/// remaps only script-local maps, `@` buffer-local) and the rhs. `<Plug>` and `<SNR>` maps (a plugin's insides) are
/// left out, as nvim's are. Buffer-local maps are kept, with `local`'s scopes added, only when `local` is given (a
/// running vim's buffer); a headless vim has no buffer of the user's.
fn parse(text: &str, local: Option<&[(Name, Name)]>) -> anyhow::Result<(Sheet, usize)> {
    let mut sheet = Sheet {
        title: "vim".into(),
        provider: "vim".into(),
        binds: Default::default(),
    };
    let mut skipped = 0;
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        // Blank lines between the commands' output, and "No mapping found" (no mode column).
        let Some(lhs) = chars
            .get(3..)
            .map(|r| r.iter().take_while(|c| **c != ' ').collect::<String>())
            .filter(|l| {
                !l.is_empty()
                    && line.starts_with([' ', 'n', 'v', 'x', 's', 'o', 'i', 'c', 'l', 't', '!'])
            })
        else {
            continue;
        };
        let flags_at = (3 + lhs.chars().count() + 1).max(15);
        let flags: String = chars.iter().skip(flags_at).take(2).collect();
        let rhs: String = chars.iter().skip(flags_at + 2).collect();
        let buffer = flags.contains('@');
        if (buffer && local.is_none())
            || [&lhs, &rhs]
                .iter()
                .any(|s| s.contains("<Plug>") || s.contains("<SNR>"))
        {
            continue;
        }
        let modes = modes(&chars[..3].iter().collect::<String>());
        let (Some(category), false) = (modes.first(), rhs.is_empty()) else {
            skipped += 1;
            continue;
        };
        let Some(key) = super::nvim::key(&lhs) else {
            skipped += 1;
            continue;
        };
        let names = modes
            .iter()
            .map(|m| m.parse::<Name>())
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut when: std::collections::BTreeMap<Name, Values> =
            [("mode".parse()?, Values::try_from(names)?)]
                .into_iter()
                .collect();
        if buffer {
            for (scope, value) in local.unwrap_or_default() {
                when.insert(scope.clone(), Values::from(value.clone()));
            }
        }
        sheet
            .binds
            .entry(category.to_string())
            .or_default()
            .push(Bind {
                key,
                desc: rhs.clone(),
                action: rhs,
                when,
            });
    }
    Ok((sheet, skipped))
}

/// The modes of `:map`'s mode column, by nvim's names, once each in the order normal, visual, select, operator,
/// insert, command, terminal, language (the first is the bind's category): blank (`:map`) is normal, visual, select
/// and operator (`:help map-overview`); `!` is insert and command; `v` is visual and select.
fn modes(column: &str) -> Vec<&'static str> {
    const ORDER: [&str; 8] = [
        "normal", "visual", "select", "operator", "insert", "command", "terminal", "language",
    ];
    if column.trim().is_empty() {
        return vec!["normal", "visual", "select", "operator"];
    }
    let mut found: Vec<&str> = Vec::new();
    for c in column.chars() {
        found.extend_from_slice(match c {
            'n' => &["normal"],
            'v' => &["visual", "select"],
            'x' => &["visual"],
            's' => &["select"],
            'o' => &["operator"],
            'i' => &["insert"],
            'c' => &["command"],
            '!' => &["insert", "command"],
            // `:lmap` applies in insert and command-line mode too (`:help language-mapping`).
            'l' => &["insert", "command", "language"],
            't' => &["terminal"],
            _ => &[],
        });
    }
    ORDER.into_iter().filter(|m| found.contains(m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vim")
    }

    fn find<'a>(s: &'a Sheet, category: &str, key: &str) -> Option<&'a Bind> {
        s.binds.get(category)?.iter().find(|b| b.key == key)
    }

    fn modes_of(b: &Bind) -> Vec<String> {
        b.when["mode"]
            .iter()
            .map(|m| m.as_str().to_string())
            .collect()
    }

    #[test]
    fn parses_captured_map_output() {
        let text = std::fs::read_to_string(fixtures().join("map.txt")).unwrap();
        let at = text.find(SENTINEL).unwrap() + SENTINEL.len();
        let (s, skipped) = parse(&text[at..], None).unwrap();
        let q = find(&s, "normal", "space q").unwrap();
        assert_eq!((q.desc.as_str(), q.action.as_str()), (":q<CR>", ":q<CR>"));
        assert_eq!(modes_of(q), ["normal"]);
        // A ctrl chord, a long rhs with spaces, a long lhs, an rhs that is `*` (in the flag-free columns).
        assert_eq!(find(&s, "normal", "ctrl+w x").unwrap().desc, "<C-W>c");
        assert_eq!(
            find(&s, "normal", "space l").unwrap().desc,
            r#":echo "a long rhs with spaces"<CR>"#
        );
        assert!(find(&s, "normal", "space a b c d e f g h i j k l").is_some());
        assert_eq!(find(&s, "normal", "space s").unwrap().desc, "*");
        assert_eq!(find(&s, "normal", "space n").unwrap().desc, "<Nop>");
        // Modes: blank, `nox`, `v`, `x`, `s`, `o`, `!`, `i`, `c`, `t`, `l`.
        assert_eq!(
            modes_of(find(&s, "normal", "g h").unwrap()),
            ["normal", "visual", "select", "operator"]
        );
        assert_eq!(
            modes_of(find(&s, "normal", "Q").unwrap()),
            ["normal", "visual", "operator"]
        );
        assert_eq!(
            modes_of(find(&s, "visual", "space v").unwrap()),
            ["visual", "select"]
        );
        assert_eq!(modes_of(find(&s, "visual", "space y").unwrap()), ["visual"]);
        assert_eq!(modes_of(find(&s, "select", "ctrl+s").unwrap()), ["select"]);
        assert_eq!(modes_of(find(&s, "operator", "i p").unwrap()), ["operator"]);
        assert_eq!(
            modes_of(find(&s, "insert", "ctrl+a").unwrap()),
            ["insert", "command"]
        );
        assert_eq!(modes_of(find(&s, "insert", "j k").unwrap()), ["insert"]);
        assert_eq!(
            modes_of(find(&s, "command", "ctrl+e").unwrap()),
            ["command"]
        );
        assert_eq!(
            find(&s, "terminal", "Escape Escape").unwrap().desc,
            r"<C-\><C-N>"
        );
        // `:lmap` applies in insert and command-line mode too, so a mode fact doesn't hide it.
        assert_eq!(
            modes_of(find(&s, "insert", "a a").unwrap()),
            ["insert", "command", "language"]
        );
        // Left out: `<Plug>` on either side, `<SNR>`, buffer-local; the `é` map (no keysym `nvim::key` knows: it
        // reads ASCII keys only) is skipped and counted.
        let all: Vec<&Bind> = s.binds.values().flatten().collect();
        for gone in ["space p", "space h", "space b"] {
            assert!(all.iter().all(|b| b.key != gone), "{gone}");
        }
        assert!(all.iter().all(|b| !b.desc.contains("<Plug>")));
        assert_eq!(skipped, 1);
        assert_eq!(all.len(), 17, "{all:#?}");
    }

    #[test]
    fn columns_at_the_lhs_width_boundary_and_odd_flags() {
        // From real vim 9.2: an lhs of 10, 11 and 12 characters; `&` (remaps script-local maps only); an empty rhs;
        // `ov` puts visual first.
        let text = "n  abcdefghij  * x\nn  abcdefghijk * y\nn  abcdefghijkl * z\nn  m1          & q\nn  m2          * \nov m3            w\n";
        let (s, skipped) = parse(text, None).unwrap();
        let desc = |k: &str| find(&s, "normal", k).map(|b| b.desc.clone());
        assert_eq!(desc("a b c d e f g h i j").as_deref(), Some("x"));
        assert_eq!(desc("a b c d e f g h i j k").as_deref(), Some("y"));
        assert_eq!(desc("a b c d e f g h i j k l").as_deref(), Some("z"));
        assert_eq!(desc("m 1").as_deref(), Some("q"));
        assert_eq!(skipped, 1, "the empty rhs");
        assert_eq!(
            modes_of(find(&s, "visual", "m 3").unwrap()),
            ["visual", "select", "operator"]
        );
    }

    #[test]
    fn empty_or_no_maps() {
        assert!(parse("", None).unwrap().0.binds.is_empty());
        let (s, skipped) = parse("\nNo mapping found\n\nNo mapping found\n", None).unwrap();
        assert!(s.binds.is_empty() && skipped == 0);
    }

    #[test]
    fn vimrc_search_order() {
        let base = std::env::temp_dir().join(format!("kbf-vimrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (home, xdg) = (base.join("home"), base.join("xdg"));
        let (h, x) = (home.display().to_string(), xdg.display().to_string());
        let found = |xdg_value: Option<&str>| {
            let e = |k: &str| match k {
                "HOME" => Some(h.clone()),
                "XDG_CONFIG_HOME" => xdg_value.map(str::to_string),
                _ => None,
            };
            vimrc(&e)
        };
        assert_eq!(found(Some(&x)), None);
        let write = |p: &Path| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        };
        // Each later one first, so each earlier one is shown to win.
        write(&home.join(".config/vim/vimrc"));
        assert_eq!(found(None), Some(home.join(".config/vim/vimrc")));
        assert_eq!(found(Some(&x)), None, "XDG_CONFIG_HOME replaces ~/.config");
        // Set but empty, or relative: vim reads no XDG vimrc either.
        assert_eq!(found(Some("")), None);
        assert_eq!(found(Some("rel")), None);
        write(&xdg.join("vim/vimrc"));
        assert_eq!(found(Some(&x)), Some(xdg.join("vim/vimrc")));
        write(&home.join(".vim/vimrc"));
        assert_eq!(found(Some(&x)), Some(home.join(".vim/vimrc")));
        write(&home.join(".vimrc"));
        assert_eq!(found(Some(&x)), Some(home.join(".vimrc")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn argument_lists() {
        let tail = [
            "-c",
            "redir! > /dev/stdout",
            "-c",
            "echo \"KEYBIND-FIND-VIM\"",
            "-c",
            "silent map",
            "-c",
            "silent map!",
            "-c",
            "silent! tmap",
            "-c",
            "silent! lmap",
            "-c",
            "echo \"KEYBIND-FIND-VIM-DONE\" | redir END | qa!",
        ];
        let with = args(Some(Path::new("/h/.vimrc")));
        let want: Vec<&str> = ["-es", "-N", "-i", "NONE", "-u", "/h/.vimrc"]
            .into_iter()
            .chain(tail)
            .collect();
        assert_eq!(with, want);
        let without = args(None);
        let want: Vec<&str> = [
            "-es",
            "-N",
            "-i",
            "NONE",
            "-u",
            "NORC",
            "-c",
            "source $VIMRUNTIME/defaults.vim",
        ]
        .into_iter()
        .chain(tail)
        .collect();
        assert_eq!(without, want);
        // vim takes at most ten `-c`.
        assert!(without.iter().filter(|a| *a == "-c").count() <= 10);
    }

    #[test]
    fn viminit_is_not_read() {
        let err = read("vim", TIMEOUT, |_| {}, &|k| {
            (k == "VIMINIT").then(|| "set nocp".into())
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("VIMINIT"), "{err}");
    }

    #[test]
    fn nvim_or_vim() {
        assert!(is_nvim("NVIM v0.12.6\nBuild type: Release\n"));
        assert!(!is_nvim(
            "VIM - Vi IMproved 9.2 (2026 Feb 14, compiled Oct 12 2026 22:01:04)\n"
        ));
        assert!(!is_nvim(""));
    }

    #[test]
    fn no_vim_on_path() {
        let err = read(
            "vim",
            TIMEOUT,
            |c| {
                c.env("PATH", "/nonexistent-kbf");
            },
            &|_| None,
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains("running vim headless") && err.contains("No such file"),
            "{err}"
        );
        assert!(!err.contains('\n'), "one line: {err}");
        // Which program: vim, else vi (Debian's vim-tiny), else none.
        let dir = std::env::temp_dir().join(format!("kbf-vim-which-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = format!("/nonexistent-kbf::{}", dir.display());
        let env = |k: &str| (k == "PATH").then(|| path.clone());
        assert_eq!(program(&env), None);
        std::fs::write(dir.join("vi"), "").unwrap();
        assert_eq!(program(&env), Some("vi"));
        std::fs::write(dir.join("vim"), "").unwrap();
        assert_eq!(program(&env), Some("vim"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake `vim` printing `body`'s output, in a new directory, written through `sh` (ETXTBSY: see `plugins.rs`'s
    /// `write_script`). `--version` prints `$KBF_VERSION`; the headless run fails (exit 7) unless run from `/`, with
    /// `LC_ALL=C.UTF-8`, `MISE_NOT_FOUND_AUTO_INSTALL=0` and closed stdin.
    fn fake_vim(name: &str, body: &str) -> PathBuf {
        use std::io::Write;
        let bin = std::env::temp_dir().join(format!("kbf-vim-fake-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = --version ] && {{ printf '%s\\n' \"$KBF_VERSION\"; exit 0; }}\n[ \"$PWD\" = / ] && [ \"$LC_ALL\" = C.UTF-8 ] && [ \"$MISE_NOT_FOUND_AUTO_INSTALL\" = 0 ] && ! read -r _ || exit 7\n{body}\n"
        );
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(bin.join("vim"))
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

    fn run_fake(bin: &Path) -> anyhow::Result<(Sheet, Vec<String>)> {
        let path = format!("{}:/usr/bin:/bin", bin.display());
        read(
            "vim",
            TIMEOUT,
            |c| {
                c.env("PATH", &path);
            },
            &|_| None,
        )
    }

    #[test]
    fn sentinels() {
        // Map-shaped lines a vimrc printed before the sentinel are ignored; the exit status isn't what counts.
        let ok = fake_vim(
            "ok",
            r#"printf 'n  zz          * printed by a vimrc\n\nKEYBIND-FIND-VIM\n\nn  gh          * ^\n\nKEYBIND-FIND-VIM-DONE'; exit 1"#,
        );
        let (s, notes) = run_fake(&ok).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.binds.values().flatten().count(), 1, "{s:?}");
        assert_eq!(find(&s, "normal", "g h").unwrap().desc, "^");
        // No sentinel (a vimrc that quit, or a vim without +eval: E319): one line with what vim said.
        let tiny = fake_vim(
            "tiny",
            "echo 'E319: Sorry, the command is not available' >&2; exit 1",
        );
        let err = format!("{:#}", run_fake(&tiny).unwrap_err());
        assert!(
            err.contains("printed no maps") && err.contains("E319"),
            "{err}"
        );
        assert!(!err.contains('\n'), "{err}");
        // Started but never ended (killed part way): an error, not a short list.
        let cut = fake_vim(
            "cut",
            r#"printf '\nKEYBIND-FIND-VIM\n\nn  gh          * ^\n'"#,
        );
        let err = format!("{:#}", run_fake(&cut).unwrap_err());
        assert!(err.contains("before it had listed every map"), "{err}");
        for b in [ok, tiny, cut] {
            let _ = std::fs::remove_dir_all(b);
        }
    }

    /// `sheet_with` over `bin` (its `vim` answering `--version` with `version`) as the whole `PATH`.
    fn run_sheet(bin: &Path, version: &str) -> anyhow::Result<(Sheet, Vec<String>)> {
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let ctx = Context {
            window_id: None,
            class: None,
            instance: None,
            title: None,
            providers: vec![],
        };
        let only = bin.display().to_string();
        sheet_with(
            &ctx,
            None,
            Path::new("/proc"),
            |c| {
                c.env("PATH", &path).env("KBF_VERSION", version);
            },
            &|k| (k == "PATH").then(|| only.clone()),
        )
        .map(|(s, notes, _)| (s, notes))
    }

    #[test]
    fn which_vim() {
        let bin = fake_vim(
            "which",
            r#"printf '\nKEYBIND-FIND-VIM\n\nn  gh          * ^\n\nKEYBIND-FIND-VIM-DONE'"#,
        );
        // A real vim's banner: the headless run.
        let (s, _) = run_sheet(&bin, "VIM - Vi IMproved 9.2 (2026 Feb 14) +eval").unwrap();
        assert_eq!(s.provider, "vim");
        assert_eq!(find(&s, "normal", "g h").unwrap().desc, "^");
        // vim-tiny: one line naming +eval, before any headless run.
        let err = format!(
            "{:#}",
            run_sheet(&bin, "VIM - Vi IMproved 9.1 -eval -terminal").unwrap_err()
        );
        assert!(err.contains("+eval") && !err.contains('\n'), "{err}");
        // Another editor answering as `vim`.
        let err = format!("{:#}", run_sheet(&bin, "nvi 1.81.6").unwrap_err());
        assert!(err.contains("is not vim"), "{err}");
        // A `vim` that is nvim: nvim's sheet (from the nvim on the machine's PATH).
        if Command::new("nvim").arg("--version").output().is_ok() {
            let (s, _) = run_sheet(&bin, "NVIM v0.12.6").unwrap();
            assert_eq!(s.provider, "nvim");
        }
        // No vim at all: one line naming both.
        let none = std::env::temp_dir().join(format!("kbf-vim-none-{}", std::process::id()));
        std::fs::create_dir_all(&none).unwrap();
        let err = format!("{:#}", run_sheet(&none, "").unwrap_err());
        assert_eq!(err, "neither vim nor vi is on PATH");
        for b in [bin, none] {
            let _ = std::fs::remove_dir_all(b);
        }
    }

    /// A temp HOME holding `vimrc` (if any) as `~/.vimrc`.
    fn home(name: &str, vimrc: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kbf-vim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(text) = vimrc {
            std::fs::write(dir.join(".vimrc"), text).unwrap();
        }
        dir
    }

    /// The real vim with `home` as HOME; `None` and a message when there is no real vim (or `vim` is nvim), except
    /// in CI (`CI` set), where there must be one.
    fn real(home: &Path, timeout: Duration) -> Option<anyhow::Result<(Sheet, Vec<String>)>> {
        let real = Command::new("vim")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .is_some_and(|o| !is_nvim(&String::from_utf8_lossy(&o.stdout)));
        if !real {
            assert!(
                std::env::var_os("CI").is_none(),
                "a real vim is required in CI"
            );
            eprintln!("skipping: vim is not installed (or is nvim)");
            return None;
        }
        let h = home.display().to_string();
        Some(read(
            "vim",
            timeout,
            |c| {
                c.env("HOME", &h)
                    .env_remove("VIMINIT")
                    .env_remove("XDG_CONFIG_HOME");
            },
            &|k| (k == "HOME").then(|| h.clone()),
        ))
    }

    /// A cold vim with plugins on a loaded test machine may need more than the provider's limit.
    const REAL_TIMEOUT: Duration = Duration::from_secs(20);

    #[test]
    fn real_vim_xdg_vimrc_and_myvimrc() {
        let dir = home("xdg", None);
        let vimdir = dir.join(".config/vim");
        std::fs::create_dir_all(&vimdir).unwrap();
        std::fs::write(
            vimdir.join("vimrc"),
            "exe 'source ' . fnamemodify($MYVIMRC, ':h') . '/maps.vim'\n",
        )
        .unwrap();
        std::fs::write(vimdir.join("maps.vim"), "nnoremap zm :echo<CR>\n").unwrap();
        let Some(r) = real(&dir, REAL_TIMEOUT) else {
            return;
        };
        let (s, _) = r.unwrap();
        // Read through `$MYVIMRC`, which `-u` alone leaves unset. (The XDG directory's plugin/ is vim's to load: 9.1.0327
        // and later put it in runtimepath, older vims don't read this vimrc at all.)
        assert!(find(&s, "normal", "z m").is_some(), "{s:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_vim_reads_the_vimrc() {
        let dir = home(
            "maps",
            Some("echo \"a vimrc that prints\"\nlet mapleader = \",\"\nnnoremap <leader>q :q<CR>\ninoremap jk <Esc>\nthisisnotacommand\n"),
        );
        let Some(r) = real(&dir, REAL_TIMEOUT) else {
            return;
        };
        // An error in the vimrc (vim then exits 1) doesn't lose the maps.
        let (s, _) = r.unwrap();
        assert_eq!(find(&s, "normal", "comma q").unwrap().desc, ":q<CR>");
        assert_eq!(find(&s, "insert", "j k").unwrap().desc, "<Esc>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_vim_with_no_vimrc_and_a_slow_one() {
        let empty = home("empty", None);
        let Some(r) = real(&empty, REAL_TIMEOUT) else {
            return;
        };
        // No vimrc: defaults.vim's maps (`Q`), no user maps, no error.
        let (s, notes) = r.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert!(s.binds.values().flatten().any(|b| b.key == "Q"), "{s:?}");
        let slow = home("slow", Some("sleep 5\n"));
        let err = format!(
            "{:#}",
            real(&slow, Duration::from_millis(500))
                .unwrap()
                .unwrap_err()
        );
        assert!(err.contains("did not answer within"), "{err}");
        for d in [empty, slow] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    fn live_fixture() -> String {
        std::fs::read_to_string(fixtures().join("live-9.2.json")).unwrap()
    }

    fn when_of(b: &Bind) -> Vec<(String, Vec<String>)> {
        b.when
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.iter().map(|n| n.as_str().to_string()).collect(),
                )
            })
            .collect()
    }

    /// AC: the running vim's answer (captured from vim 9.2.1178 with `live.vimrc`, `--noplugin`).
    #[test]
    fn the_running_vims_answer() {
        let json = live_fixture();
        let pid: u32 = serde_json::from_str::<serde_json::Value>(&json).unwrap()["pid"]
            .as_u64()
            .unwrap() as u32;
        let (s, facts, skipped) = parse_live(&json, pid).unwrap().unwrap();
        assert_eq!(skipped, 0);
        // The buffer's map, scoped to it and its filetype; the global ones as the headless read gives them.
        let b = find(&s, "normal", "space b").unwrap();
        assert_eq!(b.desc, r#":echo "buffer"<CR>"#);
        assert_eq!(
            when_of(b),
            [
                ("buffer".to_string(), vec!["current".to_string()]),
                ("filetype".into(), vec!["c".into()]),
                ("mode".into(), vec!["normal".into()]),
            ]
        );
        assert_eq!(when_of(find(&s, "normal", "space w").unwrap()).len(), 1);
        assert_eq!(find(&s, "insert", "j k").unwrap().desc, "<Esc>");
        // `<Plug>` maps, and maps onto one, are a plugin's insides.
        assert!(s
            .binds
            .values()
            .flatten()
            .all(|b| !b.action.contains("<Plug>")));
        assert_eq!(s.binds.values().flatten().count(), 3, "{s:?}");
        let facts: Vec<(&str, &str)> = facts
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(facts, [("filetype", "c"), ("mode", "normal")]);
        // Another vim's answer is none; no answer, and vim's E573 (exit 0, nothing on stdout), are failures.
        assert!(parse_live(&json, pid + 1).unwrap().is_none());
        assert!(format!("{:#}", parse_live("", pid).unwrap_err()).contains("no answer"));
        assert!(parse_live("E573: Invalid server id used: X", pid).is_err());
        // No filetype: no `filetype` scope or fact.
        let (s, facts, _) = parse_live(
            &format!(
                r#"{{"pid": {pid}, "mode": "i", "filetype": "", "maps": "n  x           *@y"}}"#
            ),
            pid,
        )
        .unwrap()
        .unwrap();
        assert_eq!(when_of(find(&s, "normal", "x").unwrap()).len(), 2);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts.values().next().unwrap().as_str(), "insert");
    }

    #[test]
    fn mode_table() {
        for (mode, name) in [
            ("n", Some("normal")),
            ("niI", Some("normal")),
            ("nt", Some("normal")),
            ("no", Some("operator")),
            ("nov", Some("operator")),
            ("v", Some("visual")),
            ("V", Some("visual")),
            ("\u{16}", Some("visual")),
            ("s", Some("select")),
            ("S", Some("select")),
            ("\u{13}", Some("select")),
            ("i", Some("insert")),
            ("ic", Some("insert")),
            ("R", Some("insert")),
            ("Rv", Some("insert")),
            ("c", Some("command")),
            ("cv", Some("command")),
            ("t", Some("terminal")),
            ("r", None),
            ("!", None),
            ("", None),
        ] {
            assert_eq!(mode_name(mode), name, "{mode:?}");
        }
    }

    #[test]
    fn server_names_and_backend() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // `--servername` first, then `--serverlist`'s lines, each once.
        assert_eq!(
            server_names(&args(&["vim", "--servername", "X", "f"]), "Y\nX\n\n", None),
            ["X", "Y"]
        );
        assert_eq!(server_names(&args(&["vim"]), "A\nB\n", None), ["A", "B"]);
        // vim takes the last `--servername`; after `--` come file names.
        assert_eq!(
            server_names(
                &args(&[
                    "vim",
                    "--servername",
                    "X",
                    "--servername",
                    "Z",
                    "--",
                    "--servername",
                    "F"
                ]),
                "",
                None
            ),
            ["Z"]
        );
        assert!(server_names(&args(&["vim", "--", "--servername", "F"]), "", None).is_empty());
        assert!(server_names(&args(&["vim", "--servername", ""]), "", None).is_empty());
        // A relative socket path, against the vim's working directory.
        assert_eq!(
            server_names(
                &args(&["vim", "--servername", "./s"]),
                "",
                Some(Path::new("/w"))
            ),
            ["/w/./s"]
        );
        assert_eq!(
            backend(
                &args(&["vim", "--clientserver", "socket"]),
                &args(&["VIM_CLIENTSERVER=x11"])
            ),
            Some("socket".to_string())
        );
        assert_eq!(
            backend(&args(&["vim"]), &args(&["A=b", "VIM_CLIENTSERVER=x11"])),
            Some("x11".to_string())
        );
        assert_eq!(backend(&args(&["vim"]), &args(&["A=b"])), None);
    }

    /// A stand-in `/proc` for a vim at pid 4242 whose `exe` is `bin`'s fake vim, run with `cmdline`.
    fn fake_proc(name: &str, bin: &Path, cmdline: &[&str]) -> PathBuf {
        let proc = std::env::temp_dir().join(format!("kbf-vim-proc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&proc);
        let dir = proc.join("4242");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(bin.join("vim"), dir.join("exe")).unwrap();
        std::fs::write(dir.join("comm"), "vim\n").unwrap();
        std::fs::write(dir.join("cmdline"), cmdline.join("\0")).unwrap();
        std::fs::write(
            dir.join("environ"),
            "VIM_CLIENTSERVER=socket\0XDG_RUNTIME_DIR=/kbf-run\0",
        )
        .unwrap();
        proc
    }

    /// The fake vim for the live read: `--serverlist` prints `$KBF_SERVERS`, `--remote-expr` runs `$KBF_ANSWER`
    /// (after checking it was given the backend from the environ), anything else is the headless run.
    fn live_vim(name: &str) -> PathBuf {
        fake_vim(
            name,
            r#"case "$*" in
  *--serverlist*) printf '%s\n' "$KBF_SERVERS" ;;
  "--clientserver socket --servername "*" --remote-expr "*) eval "$KBF_ANSWER" ;;
  *--remote-expr*) exit 9 ;;
  *) printf '\nKEYBIND-FIND-VIM\n\nn  gh          * ^\n\nKEYBIND-FIND-VIM-DONE' ;;
esac"#,
        )
    }

    fn run_live_sheet(
        bin: &Path,
        proc: &Path,
        version: &str,
        servers: &str,
        answer: &str,
    ) -> anyhow::Result<(Sheet, Vec<String>, Option<Facts>)> {
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let ctx = Context {
            window_id: None,
            class: None,
            instance: None,
            title: None,
            providers: vec![],
        };
        let only = bin.display().to_string();
        sheet_with(
            &ctx,
            Some(4242),
            proc,
            |c| {
                c.env("PATH", &path)
                    .env("KBF_VERSION", version)
                    .env("KBF_SERVERS", servers)
                    .env("KBF_ANSWER", answer);
            },
            &|k| (k == "PATH").then(|| only.clone()),
        )
    }

    /// AC: no server to ask is the headless read, silently; a server that fails is one line, then the headless read.
    #[test]
    fn the_live_read_falls_back() {
        let bin = live_vim("live");
        let proc = fake_proc("live", &bin, &["vim", "file"]);
        let cs = "VIM - Vi IMproved 9.2 +eval +clientserver +socketserver";
        let headless = |r: anyhow::Result<(Sheet, Vec<String>, Option<Facts>)>| {
            let (s, notes, facts) = r.unwrap();
            assert!(facts.is_none());
            assert!(find(&s, "normal", "g h").is_some(), "{s:?}");
            notes
        };
        // No +clientserver, or no server name: silent.
        let notes = headless(run_live_sheet(
            &bin,
            &proc,
            "VIM - Vi IMproved 9.2 +eval -clientserver",
            "S",
            "exit 9",
        ));
        assert!(notes.is_empty(), "{notes:?}");
        let notes = headless(run_live_sheet(&bin, &proc, cs, "", "exit 9"));
        assert!(notes.is_empty(), "{notes:?}");
        // Only other vims' servers answered: this vim runs none (Ben, 2026-10-30): silent.
        let other = r#"printf '{"pid": 1, "mode": "n", "filetype": "", "maps": ""}'"#;
        let notes = headless(run_live_sheet(&bin, &proc, cs, "S\nT", other));
        assert!(notes.is_empty(), "{notes:?}");
        // E573 (exit 0, nothing on stdout), a hang past the limit: one line naming the server.
        for (answer, why) in [
            ("echo 'E573: Invalid server id used: S' >&2", "no answer"),
            ("sleep 3", "did not answer within 2.0 s"),
        ] {
            let notes = headless(run_live_sheet(&bin, &proc, cs, "S", answer));
            assert_eq!(notes.len(), 1, "{answer}: {notes:?}");
            assert!(
                notes[0].starts_with("the running vim: S: ")
                    && notes[0].contains(why)
                    && notes[0].ends_with("; reading the vimrc headless instead"),
                "{answer}: {notes:?}"
            );
        }
        // One time limit for all names: a hanging first server leaves none for the next.
        let notes = headless(run_live_sheet(&bin, &proc, cs, "S\nT", "sleep 3"));
        assert!(
            notes[0].contains("S: ") && notes[0].contains("T: no time left"),
            "{notes:?}"
        );
        // A failure among other vims' answers still gets the line.
        let mixed = r#"[ "$4" = S ] && exit 0; printf '{"pid": 1, "mode": "n", "filetype": "", "maps": ""}'"#;
        let notes = headless(run_live_sheet(&bin, &proc, cs, "S\nT", mixed));
        assert!(notes[0].contains("S: no answer"), "{notes:?}");
        // A pid that isn't a vim's (a window's `_NET_WM_PID` is the client's word): not run, silent.
        std::fs::write(proc.join("4242/comm"), "bash\n").unwrap();
        let notes = headless(run_live_sheet(&bin, &proc, cs, "S", "exit 9"));
        assert!(notes.is_empty(), "{notes:?}");
        let _ = std::fs::remove_dir_all(&proc);
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// AC: the `--servername` on the command line first, then `--serverlist`'s; an answer from another vim is passed
    /// over silently when a later name answers. The server's own `XDG_RUNTIME_DIR` reaches the client, and a `TMPDIR`
    /// it doesn't have isn't ours. The live sheet and its facts.
    #[test]
    fn the_live_read() {
        let bin = live_vim("live-ok");
        let proc = fake_proc("live-ok", &bin, &["vim", "--servername", "MINE", "file"]);
        let answer = r#"[ "$XDG_RUNTIME_DIR" = /kbf-run ] && [ -z "${TMPDIR+x}" ] || exit 4
case "$4" in
  MINE) printf '{"pid": 1, "mode": "n", "filetype": "", "maps": ""}' ;;
  OTHER) printf '{"pid": 4242, "mode": "v", "filetype": "rust", "maps": "n  zz          * :w<CR>"}' ;;
  *) exit 3 ;;
esac"#;
        let got = run_live_sheet(&bin, &proc, "VIM 9.2 +eval +clientserver", "OTHER", answer);
        let _ = std::fs::remove_dir_all(&proc);
        let _ = std::fs::remove_dir_all(&bin);
        let (s, notes, facts) = got.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(find(&s, "normal", "z z").unwrap().desc, ":w<CR>");
        assert!(find(&s, "normal", "g h").is_none(), "not the headless read");
        let facts: Vec<(String, String)> = facts
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str().into(), v.as_str().into()))
            .collect();
        assert_eq!(
            facts,
            [
                ("filetype".into(), "rust".into()),
                ("mode".into(), "visual".into())
            ]
        );
    }

    /// AC: a real vim server, over the socket backend. Skipped (with a message) unless `vim --version` has
    /// `+clientserver` and `+socketserver` and `script` (util-linux) is there; not asserted under `CI`.
    #[test]
    fn real_vim_server() {
        let version = Command::new("vim").arg("--version").output().ok();
        let version = version
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let has = |f: &str| version.split_whitespace().any(|w| w == f);
        if !version.starts_with("VIM") || !has("+clientserver") || !has("+socketserver") {
            eprintln!("skipping: no vim with +clientserver and +socketserver");
            return;
        }
        if Command::new("script").arg("--version").output().is_err() {
            eprintln!("skipping: no script (util-linux)");
            return;
        }
        // A short directory: a socket path must fit in 108 bytes.
        let dir = PathBuf::from(format!("/tmp/kbf-v{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s");
        let vimrc = fixtures().join("live.vimrc");
        let mut child = Command::new("script")
            .args(["-qfc"])
            .arg(format!(
                "vim -N --noplugin -u '{}' --servername '{}'",
                vimrc.display(),
                socket.display()
            ))
            .arg("/dev/null")
            .env("XDG_RUNTIME_DIR", &dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let ask = |expr: &str| {
            let mut cmd = Command::new("vim");
            cmd.args(["--servername"])
                .arg(&socket)
                .args(["--remote-expr", expr]);
            run_for(cmd, QUERY_TIMEOUT)
                .ok()
                .map(|o| o.trim().to_string())
        };
        let pid = ask("getpid()").and_then(|p| p.parse::<u32>().ok());
        let got = pid.map(|pid| live(pid, "vim", Path::new("/proc"), &|_: &mut Command| {}));
        let _ = Command::new("vim")
            .args(["--servername"])
            .arg(&socket)
            .args(["--remote-send", "<C-\\><C-N>:qa!<CR>"])
            .output();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        if std::env::var_os("CI").is_some() {
            eprintln!("not asserted under CI: {got:?}");
            return;
        }
        let (s, facts, _) = got
            .expect("the vim server answered its pid")
            .unwrap()
            .unwrap();
        assert_eq!(find(&s, "normal", "space w").unwrap().desc, ":w<CR>");
        assert!(find(&s, "normal", "space b").is_some());
        let facts: Vec<(&str, &str)> = facts
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(facts, [("filetype", "c"), ("mode", "normal")]);
    }
}
