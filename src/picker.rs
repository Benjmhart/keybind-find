//! The picker. On X11, fzf in a floating alacritty window, with aligned columns, a `ctrl-s` search scope, and a
//! search by pressing the keys that is on when it opens (Escape for text search, `ctrl-k` back); otherwise a
//! dmenu-style command: one line per binding on stdin, the chosen line on stdout. `[picker]` in the config chooses
//! one (and fzf's terminal), or a JSON frontend: the whole list as one JSON object on stdin, the chosen id on stdout.

use crate::capture::{self, End, Ends, Key};
use crate::config::{self, Keys};
use crate::sheet::{Bind, Entry};
use anyhow::Context as _;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const KEY_CAP: usize = 28;
const DESC_CAP: usize = 60;

/// Shows `entries` in the picker; the index of the chosen one and whether to print its keys rather than send them
/// (a JSON frontend's `print`), or `None` if the user cancelled. `window` is what a JSON frontend is told.
pub fn pick(
    entries: &[Entry],
    window: &keybind_find_sheet::Context,
) -> anyhow::Result<Option<(usize, bool)>> {
    if entries.is_empty() {
        return Ok(None);
    }
    let (cfg, refused) = config::picker();
    let picker = std::env::var("KEYBIND_FIND_PICKER").ok();
    let wayland = std::env::var("WAYLAND_DISPLAY").ok();
    let (choice, fallback) = choose(&cfg, picker.as_deref(), wayland.as_deref(), on_path);
    // fzf's header shows a refused `[picker]` (it is in `config::load`'s note); any other picker, stderr.
    if !matches!(choice, Choice::Fzf(_)) {
        if let Some(why) = &refused {
            eprintln!("keybind-find: {why}");
        }
    }
    if let Some(why) = fallback {
        eprintln!("keybind-find: {why}");
    }
    let plain = |i: Option<usize>| i.map(|i| (i, false));
    match choice {
        Choice::Fzf(terminal) => pick_fzf(entries, &terminal).map(plain),
        Choice::Dmenu(cmd) => pick_with(&cmd, entries).map(plain),
        Choice::Json(cmd) => pick_json(&cmd, entries, window),
    }
}

/// Which picker runs, with its argument list: fzf's terminal, or the dmenu-style or JSON command.
#[derive(Debug, PartialEq)]
enum Choice {
    Fzf(Vec<String>),
    Dmenu(Vec<String>),
    Json(Vec<String>),
}

/// The picker for `cfg`: `KEYBIND_FIND_PICKER` first (a dmenu-style command), then `[picker]`'s kind, else today's
/// order (fzf on X11 when it and the terminal are installed, else fuzzel on Wayland, else dmenu). A `kind = "fzf"`
/// whose tools are missing falls back to the dmenu-style picker, with the reason.
fn choose(
    cfg: &config::Picker,
    env_picker: Option<&str>,
    wayland: Option<&str>,
    on_path: impl Fn(&str) -> bool,
) -> (Choice, Option<String>) {
    let terminal = cfg.terminal.clone().unwrap_or_else(default_terminal);
    let have_fzf = || on_path("fzf") && terminal.first().is_some_and(|t| on_path(t));
    let dmenu = || {
        Choice::Dmenu(
            cfg.command
                .clone()
                .unwrap_or_else(|| cmd_for(None, wayland)),
        )
    };
    if env_picker.is_some_and(|p| !p.trim().is_empty()) {
        return (Choice::Dmenu(cmd_for(env_picker, wayland)), None);
    }
    match cfg.kind {
        Some(config::PickerKind::Json) => {
            (Choice::Json(cfg.command.clone().unwrap_or_default()), None)
        }
        Some(config::PickerKind::Dmenu) => (dmenu(), None),
        Some(config::PickerKind::Fzf) if have_fzf() => (Choice::Fzf(terminal), None),
        Some(config::PickerKind::Fzf) => (
            dmenu(),
            Some(format!(
                "[picker] kind \"fzf\": fzf or {} is not installed; using the dmenu-style picker",
                terminal.first().map_or("the terminal", String::as_str)
            )),
        ),
        None if use_fzf(None, wayland, have_fzf()) => (Choice::Fzf(terminal), None),
        None => (dmenu(), None),
    }
}

/// The terminal fzf runs in when `[picker]` names none: alacritty, floated by its class, sized for the columns.
fn default_terminal() -> Vec<String> {
    [
        "alacritty",
        "--class",
        "keybind-find,keybind-find",
        "--title",
        "keybind-find",
        // Wide enough for the provider column and a refused config's reason; xmonad floats it at this size.
        "-o",
        "window.dimensions.columns=140",
        "-o",
        "window.dimensions.lines=30",
        "-e",
    ]
    .map(str::to_string)
    .to_vec()
}

/// fzf in alacritty only on X11, with no `KEYBIND_FIND_PICKER` override, and when both are installed.
fn use_fzf(picker: Option<&str>, wayland: Option<&str>, have_tools: bool) -> bool {
    picker.is_none_or(|p| p.trim().is_empty()) && wayland.is_none_or(str::is_empty) && have_tools
}

/// An executable file `name` in some `PATH` directory, or at `name` when it is a path.
fn on_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        return executable(Path::new(name));
    }
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| executable(&d.join(name))))
}

/// Key, description and provider for each entry, the first two padded to the widest (by `chars()`), capped at
/// `KEY_CAP` / `DESC_CAP`; a longer value overflows its own row only. The description ends with the entry's scope
/// tags (`[mode=insert]`, see `scope::tags`), which count toward its width. Control characters (line breaks, tabs,
/// NUL) become spaces, so each entry stays one line and the columns stay put.
fn columns(entries: &[Entry]) -> Vec<[String; 3]> {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect()
    };
    let rows: Vec<[String; 3]> = entries
        .iter()
        .map(|e| {
            let desc = e.bind.desc.clone() + &crate::scope::tags(&e.bind.when);
            [clean(&e.bind.key), clean(&desc), clean(&e.provider)]
        })
        .collect();
    let width = |col: usize, cap: usize| {
        rows.iter()
            .map(|r| r[col].chars().count())
            .max()
            .unwrap_or(0)
            .min(cap)
    };
    let (kw, dw) = (width(0, KEY_CAP), width(1, DESC_CAP));
    let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())));
    rows.into_iter()
        .map(|[k, d, p]| [pad(&k, kw), pad(&d, dw), p])
        .collect()
}

/// Runs fzf in a new `terminal` window (alacritty unless `[picker]` names another): this binary again, as `__fzf <list> <out>` (see `run_fzf`), with the lines in
/// a file and the pick written to another, both in a fresh `0700` directory removed afterwards. keybind-find's own
/// keys are listed after `entries`, so their indices are `entries.len()` and up and picking one is no pick. They are
/// added here rather than in `__fzf` so `columns` lines them up with the rest; `__fzf` reads the config again, for
/// its keys and to show a refused config in the header, where it is seen (Super+/'s stderr is not).
fn pick_fzf(entries: &[Entry], terminal: &[String]) -> anyhow::Result<Option<usize>> {
    let dir = RunDir::new()?;
    let (list, out) = (dir.0.join("list"), dir.0.join("out"));
    let all: Vec<Entry> = entries
        .iter()
        .cloned()
        .chain(own_entries(&load_keys().0))
        .collect();
    std::fs::write(&list, fzf_input(&all)).context("writing the fzf list")?;
    let exe = std::env::current_exe().context("finding keybind-find's own path")?;
    let args = terminal_args(terminal, &exe, &list, &out);
    let (program, args) = args.split_first().context("[picker] terminal is empty")?;
    let result = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("starting {} for the fzf picker", program.to_string_lossy()))?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let stderr = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        anyhow::bail!(
            "{} failed ({}): {stderr}",
            program.to_string_lossy(),
            result.status
        );
    }
    // No file: the window was closed before fzf finished. That's a cancel.
    let text = std::fs::read_to_string(&out).unwrap_or_default();
    if let Some(msg) = text.strip_prefix("error: ") {
        anyhow::bail!("{}", msg.trim_end());
    }
    Ok(pick_index(&text, entries.len()))
}

/// The picked index, if it is one of the `n` real entries: one of keybind-find's own keys (`n` and up) is no pick.
fn pick_index(text: &str, n: usize) -> Option<usize> {
    parse_pick(text).filter(|&i| i < n)
}

/// `terminal`, then this binary as `__fzf <list> <out>`.
fn terminal_args(
    terminal: &[String],
    exe: &Path,
    list: &Path,
    out: &Path,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = terminal.iter().map(Into::into).collect();
    args.extend([exe.into(), "__fzf".into(), list.into(), out.into()]);
    args
}

/// `<index>\t<key> \t<desc> \t<provider>` per entry. fzf hides the index (`--with-nth 2..`) and prints only it back
/// (`--accept-nth 1`); `--tabstop 1` draws each tab as one column, so the padded fields line up.
fn fzf_input(entries: &[Entry]) -> String {
    columns(entries)
        .iter()
        .enumerate()
        .map(|(i, [k, d, p])| format!("{i}\t{k} \t{d} \t{p}\n"))
        .collect()
}

/// The keys in force, checked against the running layout when xmodmap can read it, and why the config was refused.
fn load_keys() -> (Keys, Option<String>) {
    let layout = capture::layout_keysyms().ok();
    let has = layout.map(|l| move |k: &str| l.contains(k));
    config::load(has.as_ref().map(|f| f as &dyn Fn(&str) -> bool))
}

/// keybind-find's own keys, one entry per key in force, under program `keybind-find`. Text search's run and close are
/// fzf's own Return and Escape.
fn own_entries(keys: &Keys) -> Vec<Entry> {
    let entry = |category: &str, key: &str, desc: &str| Entry {
        provider: "keybind-find".into(),
        category: category.into(),
        bind: Bind {
            key: key.into(),
            desc: format!("{category}: {desc}"),
            action: String::new(),
            when: Default::default(),
        },
    };
    let mut out = Vec::new();
    for (chords, desc) in [
        (&keys.run, "run the highlighted entry"),
        (&keys.text_search, "switch to text search"),
        (&keys.undo, "undo, take back the last key"),
    ] {
        out.extend(chords.iter().map(|c| entry("key search", c, desc)));
    }
    for (chords, desc) in [
        (&keys.key_search, "start key search"),
        (&keys.scope, "change the search scope"),
    ] {
        out.extend(chords.iter().map(|c| entry("text search", c, desc)));
    }
    out.push(entry("text search", "Return", "run the highlighted entry"));
    out.push(entry("text search", "Escape", "close"));
    out
}

/// What the headers say: the keys in force, and a refused config's reason in front of every header, so it stays
/// in view while key search rewrites the rest.
#[derive(Clone)]
struct Ui {
    keys: Keys,
    note: String,
}

impl Ui {
    fn new((keys, err): (Keys, Option<String>)) -> Self {
        // Red: fzf draws a header's ANSI colours even without `--ansi`.
        let note = err
            .map(|e| format!("\x1b[31m{e}\x1b[0m | "))
            .unwrap_or_default();
        Ui { keys, note }
    }

    /// Key search's keys as X chords (`Return/KP_Enter`).
    fn x(keys: &[String]) -> String {
        if keys.is_empty() {
            return "(no key)".to_string();
        }
        keys.join("/")
    }

    /// Text search's keys by fzf's names (`ctrl-k`).
    fn fzf(keys: &[String]) -> String {
        Self::x(&config::fzf_keys(keys))
    }

    /// The key search header: what was pressed and the keys that end it or take one back.
    fn key_search(&self, so_far: &str) -> String {
        format!(
            "{}key search: {so_far} | {}: text search | {}: run | {}: undo",
            self.note,
            Self::x(&self.keys.text_search),
            Self::x(&self.keys.run),
            Self::x(&self.keys.undo)
        )
    }

    /// The text search header after `what` (the keys pressed, or why key search stopped).
    fn text_search(&self, what: &str) -> String {
        format!(
            "{}{what} | text search | Escape: close | {}: press keys again | {}: search scope",
            self.note,
            Self::fzf(&self.keys.key_search),
            Self::fzf(&self.keys.scope)
        )
    }
}

/// `ctrl-s` steps the search scope all → keys → description → program → all, and names it in the prompt. The
/// script is this fixed text, run by `sh` (`--with-shell`); nothing from a config or an entry is in it.
const SCOPE_CYCLE: &str = "case $FZF_NTH in \
    1) echo 'change-nth(2)+change-prompt(description> )' ;; \
    2) echo 'change-nth(3)+change-prompt(program> )' ;; \
    3) echo 'change-nth(..)+change-prompt(all> )' ;; \
    *) echo 'change-nth(1)+change-prompt(keys> )' ;; esac";

fn fzf_args(ui: &Ui) -> Vec<String> {
    let header = format!(
        "--header={}{}: search scope | {}: search by pressing keys",
        ui.note,
        Ui::fzf(&ui.keys.scope),
        Ui::fzf(&ui.keys.key_search)
    );
    [
        "--delimiter=\t",
        "--with-nth=2..",
        "--accept-nth=1",
        "--nth=..",
        "--tabstop=1",
        "--tiebreak=index",
        "--layout=reverse",
        "--prompt=all> ",
        "--with-shell=sh -c",
    ]
    .map(str::to_string)
    .into_iter()
    .chain([header])
    .chain(
        config::fzf_keys(&ui.keys.scope)
            .into_iter()
            .map(|k| format!("--bind={k}:transform:{SCOPE_CYCLE}")),
    )
    .collect()
}

/// The inner half of the fzf picker, run inside alacritty as `keybind-find __fzf <list> <out>`: fzf reads `list`
/// and draws on the terminal; `out` gets the picked index, nothing on a cancel, or `error: <message>`.
pub fn run_fzf(list: &Path, out: &Path) -> anyhow::Result<()> {
    let text = match fzf(list) {
        Ok(Some(i)) => format!("{i}\n"),
        Ok(None) => String::new(),
        Err(e) => format!("error: {e:#}\n"),
    };
    std::fs::write(out, text).with_context(|| format!("writing {}", out.display()))
}

fn fzf(list: &Path) -> anyhow::Result<Option<String>> {
    let input = std::fs::File::open(list).with_context(|| format!("opening {}", list.display()))?;
    let ui = Ui::new(load_keys());
    let mut args = fzf_args(&ui);
    // Without key search the picker opens in text search, and the header and `ctrl-k` say why.
    match x11_only(std::env::var_os("WAYLAND_DISPLAY").as_deref())
        .and_then(|()| fzf_has_unix_listen())
        .and_then(|()| listen_for_key_search(list, &ui))
    {
        Ok(more) => args.extend(more),
        Err(e) => args.extend(no_key_search_args(&e, &ui)),
    }
    let out = Command::new("fzf")
        .args(args)
        // The user's defaults (`--tac`, `--multi`, `--print-query`, a `bind`) would reorder the list or change what
        // fzf prints back. An API key would make fzf refuse our own requests.
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .env_remove("FZF_API_KEY")
        .stdin(input)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("running fzf")?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        // 1: no match, 130: Escape or ctrl-c.
        Some(1 | 130) => Ok(None),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
            anyhow::bail!("fzf failed ({}): {stderr}", out.status)
        }
    }
}

/// Key search only outside Wayland: a native Wayland terminal's keys never reach an X grab, even with XWayland's
/// `DISPLAY` set. Named, since a `WAYLAND_DISPLAY` left over in an X session turns it off too.
fn x11_only(wayland_display: Option<&std::ffi::OsStr>) -> anyhow::Result<()> {
    match wayland_display.filter(|w| !w.is_empty()) {
        Some(_) => anyhow::bail!("key search is X11 only (WAYLAND_DISPLAY is set)"),
        None => Ok(()),
    }
}

/// The header and `ctrl-k` showing why key search is off. The `action:argument` form takes the rest of the option, `,`
/// and `:` included.
fn no_key_search_args(e: &anyhow::Error, ui: &Ui) -> Vec<String> {
    let why = one_line(e);
    let header = format!(
        "{}{why} | text search | {}: search scope",
        ui.note,
        Ui::fzf(&ui.keys.scope)
    );
    std::iter::once(format!("--header={header}"))
        .chain(
            config::fzf_keys(&ui.keys.key_search)
                .into_iter()
                .map(|k| format!("--bind={k}:change-header:{}{why}", ui.note)),
        )
        .collect()
}

/// `--listen` on a Unix socket needs fzf 0.66; an older fzf would refuse the option and the whole picker with it.
fn fzf_has_unix_listen() -> anyhow::Result<()> {
    // The user's defaults could make even `--version` fail, as they would not for the real run.
    let out = Command::new("fzf")
        .arg("--version")
        .env_remove("FZF_DEFAULT_OPTS")
        .env_remove("FZF_DEFAULT_OPTS_FILE")
        .stdin(Stdio::null())
        .output()
        .context("running fzf --version")?;
    let version = String::from_utf8_lossy(&out.stdout);
    match parse_fzf_version(&version) {
        Some(v) if v >= (0, 66) => Ok(()),
        Some(_) => anyhow::bail!(
            "key search needs fzf 0.66 or newer (this is {})",
            version.trim()
        ),
        None => anyhow::bail!(
            "key search is off: couldn't read fzf's version from {:?}",
            version.trim()
        ),
    }
}

/// `0.74.5 (c0a193e0)` → (0, 74).
fn parse_fzf_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split_whitespace().next()?.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// fzf's HTTP server (`--listen`; the name must end in `.sock`) and the socket `ctrl-k` pokes, both in the run dir.
const FZF_SOCKET: &str = "fzf.sock";
const SIGNAL_SOCKET: &str = "keys.sock";

/// Listens on the run dir's signal socket and returns the fzf options that wire fzf's start and `ctrl-k` to it. Each
/// connection starts a key search on a thread of its own, so fzf keeps drawing; the thread dies with this process.
fn listen_for_key_search(list: &Path, ui: &Ui) -> anyhow::Result<Vec<String>> {
    let dir = list.parent().context("the fzf list has no directory")?;
    let exe = std::env::current_exe().context("finding keybind-find's own path")?;
    let signal = dir.join(SIGNAL_SOCKET);
    let fzf_socket = dir.join(FZF_SOCKET);
    // Checked before binding: the run dir is in the `reload` command too.
    let args = key_search_args(&exe, &signal, &fzf_socket, ui)?;
    let listener = UnixListener::bind(&signal)
        .with_context(|| format!("listening on {}", signal.display()))?;
    let list = list.to_path_buf();
    let ui = ui.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if conn.is_err() {
                break;
            }
            let actions = key_search(&list, &fzf_socket, &ui)
                .unwrap_or_else(|e| format!("change-header:{}", fail_header(&e, &ui)));
            // fzf gone: nothing left to tell.
            let _ = post(&fzf_socket, &actions);
            // A `ctrl-k` pressed again (or repeating) before the grab landed would start a second search the moment
            // this one ended, which looks like Escape not working. Drop them.
            if listener.set_nonblocking(true).is_ok() {
                while listener.accept().is_ok() {}
                let _ = listener.set_nonblocking(false);
            }
        }
    });
    Ok(args)
}

/// `--listen` for fzf's server, and fzf's start and the `key_search` keys (`ctrl-k`) running
/// `<exe> __capture-signal <signal>`: the picker opens in key search, as if `ctrl-k` had been pressed. fzf's server is listening before its `start` event fires, so
/// the search's first post finds it. fzf runs the command through `sh -c` (`--with-shell`), so both paths are
/// single-quoted, and a path holding a `'` is refused.
fn key_search_args(
    exe: &Path,
    signal: &Path,
    fzf_socket: &Path,
    ui: &Ui,
) -> anyhow::Result<Vec<String>> {
    // fzf also replaces `{...}` placeholders in the command.
    for p in [exe, signal] {
        if p.to_string_lossy().contains(['\'', '{', '}']) {
            anyhow::bail!(
                "key search is off: {} contains a ', {{ or }} character",
                p.display()
            );
        }
    }
    // The `action:argument` form takes the rest of the string, so a `)` in a path can't end it early.
    let run = format!(
        "execute-silent:'{}' __capture-signal '{}'",
        exe.display(),
        signal.display()
    );
    let mut args = vec![
        format!("--listen={}", fzf_socket.display()),
        // Until the grab lands; the search's first post replaces it.
        format!(
            "--header={}key search starting | {}: start it | {}: search scope",
            ui.note,
            Ui::fzf(&ui.keys.key_search),
            Ui::fzf(&ui.keys.scope)
        ),
        format!("--bind=start:{run}"),
    ];
    args.extend(
        config::fzf_keys(&ui.keys.key_search)
            .into_iter()
            .map(|k| format!("--bind={k}:{run}")),
    );
    Ok(args)
}

/// `keybind-find __capture-signal <socket>`, run by fzf on `ctrl-k`: connecting is the whole message.
pub fn signal_key_search(socket: &Path) -> anyhow::Result<()> {
    UnixStream::connect(socket).with_context(|| format!("connecting to {}", socket.display()))?;
    Ok(())
}

/// One key search: grab the keyboard and, after each key, show only the bindings whose key starts with what was
/// pressed. Returns the fzf actions for how it ended (`end_actions`).
fn key_search(list: &Path, fzf_socket: &Path, ui: &Ui) -> anyhow::Result<String> {
    let text =
        std::fs::read_to_string(list).with_context(|| format!("reading {}", list.display()))?;
    let dir = list.parent().context("the fzf list has no directory")?;
    let (shown, next) = (dir.join("keys"), dir.join("keys.next"));
    // Without it only letters fold (`K` = `shift+k`); if xmodmap is missing, capture says so next.
    let shifted = capture::shifted_keysyms().unwrap_or_default();
    let mut pressed: Vec<String> = Vec::new();
    let mut held = String::new();
    let mut last = String::new();
    let ends = Ends {
        text_search: &ui.keys.text_search,
        run: &ui.keys.run,
        undo: &ui.keys.undo,
    };
    let end = capture::capture_until(&ends, |k| {
        let (want, actions) = step(&mut pressed, &mut held, k, &shown, ui);
        let want: Vec<&str> = want.iter().map(String::as_str).collect();
        last = narrow(&text, &want, &shifted);
        // Renamed into place: a `cat` still reading the previous list keeps reading that one.
        std::fs::write(&next, &last).context("writing the narrowed list")?;
        std::fs::rename(&next, &shown).context("writing the narrowed list")?;
        post(fzf_socket, &actions)
    })?;
    if end == End::Run {
        wait_for_list(fzf_socket, &last)?;
    }
    Ok(end_actions(&end, &pressed, ui))
}

/// fzf loads a `reload` and matches it in the background, `reload-sync` included, and an `accept` that arrives first
/// picks from the list still showing. So before an Enter's `accept`, wait (up to 2 s) until fzf shows `lines`, the
/// list last sent.
// ponytail: the grab is already released while this waits (a few ms in practice), so a key typed then reaches fzf;
// and each request has its own 2 s timeout, so a stalled fzf can stretch the wait to 4 s.
fn wait_for_list(fzf_socket: &Path, lines: &str) -> anyhow::Result<()> {
    let request = format!("GET /?limit={}", lines.lines().count());
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if shows(&ask(fzf_socket, &request, "")?, lines) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    anyhow::bail!("fzf didn't show the narrowed list in time, so Enter ran nothing")
}

/// From fzf's state (`GET /`): done reading, and matching exactly `lines`, in order (the query is empty during a key
/// search). A count alone can't tell two lists apart: releasing a held modifier widens the list again.
fn shows(state: &str, lines: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(state) else {
        return false;
    };
    let want: Vec<&str> = lines.lines().collect();
    let matches = v["matches"].as_array().map(|m| {
        m.iter()
            .map(|e| e["text"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
    });
    v["reading"] == false
        && v["matchCount"].as_u64() == u64::try_from(want.len()).ok()
        && matches.is_some_and(|m| m == want)
}

/// `run` (Enter) runs the highlighted entry, as Enter in text search does (with nothing listed, fzf exits with no
/// pick). `text_search` (Escape) leaves the narrowed list in text search.
fn end_actions(end: &End, pressed: &[String], ui: &Ui) -> String {
    match end {
        End::Run => "accept".to_string(),
        End::TextSearch => format!("change-header:{}", end_header(pressed, ui)),
    }
}

/// The header when a key search fails or stops (the 30 s idle limit): why, in text search.
fn fail_header(e: &anyhow::Error, ui: &Ui) -> String {
    ui.text_search(&one_line(e))
}

/// One capture event: records it in `pressed` / `held` and returns the keys to match (the chords pressed, then the
/// modifiers still held) and the fzf actions that show them, reloading the list from `shown`.
fn step(
    pressed: &mut Vec<String>,
    held: &mut String,
    k: Key,
    shown: &Path,
    ui: &Ui,
) -> (Vec<String>, String) {
    let start = matches!(k, Key::Grabbed);
    match k {
        Key::Grabbed => {}
        Key::Held(h) => *held = h.to_string(),
        Key::Chord(c) => {
            pressed.push(c.to_string());
            held.clear();
        }
        // A modifier still held stays: its release reports it.
        Key::Undo => {
            pressed.pop();
        }
    }
    let mut want = pressed.clone();
    if !held.is_empty() {
        want.push(held.clone());
    }
    let so_far = if want.is_empty() {
        "press the keys".to_string()
    } else {
        want.join(" ")
    };
    // A new search starts from the whole list and an empty query, so the order is fzf's own (`--tiebreak=index`).
    let actions = format!(
        "{}{}+reload:cat '{}'",
        if start { "change-query()+" } else { "" },
        bracketed("change-header", &ui.key_search(&so_far)),
        shown.display()
    );
    (want, actions)
}

/// `action(arg)`, in the first of fzf's bracket pairs that `arg` doesn't hold, so a refused config's text (a path,
/// a TOML error) can't end it early.
fn bracketed(action: &str, arg: &str) -> String {
    let pairs = [
        ('(', ')'),
        ('[', ']'),
        ('{', '}'),
        ('<', '>'),
        ('~', '~'),
        ('!', '!'),
        ('@', '@'),
        ('#', '#'),
        ('%', '%'),
        ('^', '^'),
        ('|', '|'),
    ];
    match pairs.iter().find(|&&(_, close)| !arg.contains(close)) {
        Some((open, close)) => format!("{action}{open}{arg}{close}"),
        // ponytail: every closer in the text: drop the `)`s rather than fail the key.
        None => format!("{action}({})", arg.replace(')', "")),
    }
}

/// The header once `text_search` (Escape) ends a search: what was pressed, that this is text search now, and the keys
/// from there.
fn end_header(pressed: &[String], ui: &Ui) -> String {
    let pressed = if pressed.is_empty() {
        "none".to_string()
    } else {
        pressed.join(" ")
    };
    ui.text_search(&format!("keys: {pressed}"))
}

/// The fzf lines (`<index>\t<key> \t...`) whose key starts with `pressed`.
fn narrow(list: &str, pressed: &[&str], shifted: &HashMap<String, String>) -> String {
    list.lines()
        .filter(|l| {
            l.split('\t')
                .nth(1)
                .is_some_and(|key| starts_with_keys(key, pressed, shifted))
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Whether `key` (`ctrl+b t`) starts with the chords `pressed`, compared whole: `super+s` doesn't match
/// `super+shift+Return`. The last pressed chord may be modifiers only, still held (`super+shift`): it matches a chord
/// with at least those modifiers. `shift+k` and `K` are the same chord, and so are `shift+slash` and `question` when
/// `shifted` (the layout's shifted keysym → base keysym) says so.
fn starts_with_keys(key: &str, pressed: &[&str], shifted: &HashMap<String, String>) -> bool {
    let have: Vec<&str> = key.split_whitespace().collect();
    pressed.len() <= have.len()
        && pressed.iter().zip(&have).enumerate().all(|(i, (p, h))| {
            // A capture already reports base keysyms; only the binding's text can hold a shifted symbol.
            let ((p_mods, p_key), (h_mods, h_key)) =
                (parse_chord(p, &HashMap::new()), parse_chord(h, shifted));
            match p_key {
                Some(_) => p_mods == h_mods && p_key == h_key,
                None => {
                    i + 1 == pressed.len()
                        && h_key.is_some()
                        && p_mods.iter().all(|m| h_mods.contains(m))
                }
            }
        })
}

/// `super+shift+k` → (`[shift, super]`, `k`); `K` → (`[shift]`, `k`); `super+shift` → (`[shift, super]`, none);
/// with `question` → `slash` in `shifted`, `question` → (`[shift]`, `slash`).
fn parse_chord(chord: &str, shifted: &HashMap<String, String>) -> (Vec<String>, Option<String>) {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let last = parts.pop().unwrap_or_default();
    let mut mods: Vec<String> = parts.iter().map(|m| m.to_string()).collect();
    let key = if ["super", "ctrl", "alt", "shift"].contains(&last) {
        mods.push(last.to_string());
        None
    } else if let Some(base) = shifted.get(last) {
        mods.push("shift".to_string());
        Some(base.clone())
    } else if last.len() == 1 && last.as_bytes()[0].is_ascii_uppercase() {
        mods.push("shift".to_string());
        Some(last.to_ascii_lowercase())
    } else {
        Some(last.to_string())
    };
    mods.sort();
    mods.dedup();
    (mods, key)
}

/// Sends fzf `actions` over its `--listen` socket (an HTTP POST); fzf answers 200, or 400 naming what it rejected.
fn post(fzf_socket: &Path, actions: &str) -> anyhow::Result<()> {
    ask(fzf_socket, "POST /", actions).map(|_| ())
}

/// One HTTP request to fzf's server (`POST /`, `GET /?limit=0`); the body of its 200 answer.
fn ask(fzf_socket: &Path, request_line: &str, actions: &str) -> anyhow::Result<String> {
    let mut s = UnixStream::connect(fzf_socket).context("connecting to fzf")?;
    s.set_read_timeout(Some(Duration::from_secs(2)))
        .and_then(|()| s.set_write_timeout(Some(Duration::from_secs(2))))
        .context("setting a timeout on fzf's socket")?;
    // One write: the request arrives whole, not in the pieces `write!` would send.
    // fzf answers a `Content-Length: 0` with 400 "invalid content length", so a GET has none.
    let length = if actions.is_empty() {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", actions.len())
    };
    let request = format!(
        "{request_line} HTTP/1.1\r\nHost: localhost\r\n{length}Connection: close\r\n\r\n{actions}"
    );
    s.write_all(request.as_bytes()).context("talking to fzf")?;
    let mut reply = String::new();
    s.read_to_string(&mut reply)
        .context("reading fzf's answer")?;
    let status = reply.lines().next().unwrap_or_default();
    let body = reply.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    if !status.contains(" 200 ") {
        anyhow::bail!("fzf refused {actions:?}: {status} {}", body.trim());
    }
    Ok(body.to_string())
}

/// An error as one header line.
fn one_line(e: &anyhow::Error) -> String {
    format!("{e:#}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The out file's index; anything else (empty, garbage) is a cancel.
fn parse_pick(text: &str) -> Option<usize> {
    text.trim().parse().ok()
}

/// A fresh `0700` directory under `$XDG_RUNTIME_DIR` (else the temp dir), removed when dropped.
struct RunDir(PathBuf);

impl RunDir {
    fn new() -> anyhow::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute() && p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("keybind-find-{}-{nanos}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self(dir))
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn pick_with(cmd: &[String], entries: &[Entry]) -> anyhow::Result<Option<usize>> {
    if entries.is_empty() {
        return Ok(None);
    }
    let (program, args) = cmd.split_first().context("empty picker command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting picker {:?}", cmd.join(" ")))?;
    let lines = dmenu_lines(entries);
    let mut input = lines.join("\n");
    input.push('\n');
    let mut stdin = child.stdin.take().context("picker stdin")?;
    // Fed from a thread, so a picker that prints while it reads can't deadlock against us; dropping stdin is EOF.
    let writer = std::thread::spawn(move || match stdin.write_all(input.as_bytes()) {
        // A picker that exits without reading everything (`head -1`) closes the pipe; that's not an error.
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e),
        _ => Ok(()),
    });
    let out = child.wait_with_output().context("waiting for the picker")?;
    let written = writer.join();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stderr = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    let chosen = String::from_utf8_lossy(&out.stdout);
    // dmenu's Ctrl+Return can print several picks; the first is the one.
    let chosen = chosen.lines().next().unwrap_or("").trim_end_matches('\r');
    if chosen.is_empty() {
        // Escape exits non-zero and says nothing. A picker that couldn't run (no display, a bad option) says why on
        // stderr, or was killed: that must not look like a cancel.
        if out.status.code().is_none() || !out.status.success() && !stderr.is_empty() {
            anyhow::bail!(
                "picker {:?} failed ({}): {stderr}",
                cmd.join(" "),
                out.status
            );
        }
        return Ok(None);
    }
    match written {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e).context("writing to the picker"),
        Err(_) => anyhow::bail!("writing to the picker panicked"),
    }
    // Typed text that matches no line is a cancel too.
    Ok(lines.iter().position(|l| l == chosen))
}

/// `<key>  <desc>  [<provider>]`, padded by `columns` so they line up in a monospace font.
fn dmenu_lines(entries: &[Entry]) -> Vec<String> {
    columns(entries)
        .iter()
        .map(|[k, d, p]| format!("{k}  {d}  [{p}]"))
        .collect()
}

/// The JSON protocol's request (version 1): the window, and the entries in ranked order, each with its id.
#[derive(serde::Serialize)]
struct JsonRequest<'a> {
    version: u32,
    context: &'a keybind_find_sheet::Context,
    entries: Vec<JsonEntry<'a>>,
}

#[derive(serde::Serialize)]
struct JsonEntry<'a> {
    id: usize,
    key: &'a str,
    desc: &'a str,
    program: &'a str,
    when: &'a std::collections::BTreeMap<crate::sheet::Name, crate::sheet::Values>,
    /// The tags the fzf picker shows after the description (`[mode=insert]`, see `scope::tags`).
    tags: Vec<String>,
}

/// The JSON picker protocol's version: its own, not the sheet protocol's, so either can change alone.
const PICKER_PROTOCOL: u32 = 1;

/// Runs the JSON frontend `cmd` with the request on stdin and reads its pick: `<id>`, or `<id> print`. Empty output
/// or a non-zero exit is a cancel; anything else is one stderr line and a cancel. A cancel with something on stderr,
/// or by a signal, also says so in one stderr line: a frontend that crashed must not look like Escape.
fn pick_json(
    cmd: &[String],
    entries: &[Entry],
    window: &keybind_find_sheet::Context,
) -> anyhow::Result<Option<(usize, bool)>> {
    let (program, args) = cmd.split_first().context("[picker] command is empty")?;
    let request = json_request(entries, window)?;
    let mut command = Command::new(program);
    command.args(args);
    let (status, out, stderr) = crate::context::run_frontend(command, request)
        .with_context(|| format!("picker {:?}", cmd.join(" ")))?;
    if !status.success() {
        if let Some(line) = crash_line(&cmd.join(" "), status, &stderr) {
            eprintln!("keybind-find: {line}");
        }
        return Ok(None);
    }
    let pick = parse_json_pick(&out, entries.len());
    if let Err(line) = &pick {
        eprintln!(
            "keybind-find: picker {:?} printed {line:?}, not an id from its list; nothing sent",
            cmd.join(" ")
        );
    }
    Ok(pick.unwrap_or(None))
}

/// The line a cancelled JSON frontend gets: none for a quiet non-zero exit (Escape), else its status and stderr
/// (one line, at most 300 characters), so a crash doesn't pass for Escape.
fn crash_line(cmd: &str, status: std::process::ExitStatus, stderr: &str) -> Option<String> {
    let mut why = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    if status.code().is_some() && why.is_empty() {
        return None;
    }
    if let Some((cut, _)) = why.char_indices().nth(300) {
        why.truncate(cut);
        why.push('…');
    }
    Some(format!(
        "picker {cmd:?} failed ({status}): {why}; nothing sent"
    ))
}

fn json_request(
    entries: &[Entry],
    window: &keybind_find_sheet::Context,
) -> anyhow::Result<Vec<u8>> {
    let entries = entries
        .iter()
        .enumerate()
        .map(|(id, e)| JsonEntry {
            id,
            key: &e.bind.key,
            desc: &e.bind.desc,
            program: &e.provider,
            when: &e.bind.when,
            // A tag has no spaces (`Name` allows none), so this splits only between tags.
            tags: crate::scope::tags(&e.bind.when)
                .split_whitespace()
                .map(str::to_string)
                .collect(),
        })
        .collect();
    serde_json::to_vec(&JsonRequest {
        version: PICKER_PROTOCOL,
        context: window,
        entries,
    })
    .context("encoding the picker request")
}

/// A JSON frontend's output: its first non-blank line, `<id>` or `<id> print`, for one of `n` entries. Nothing is a
/// cancel (`Ok(None)`); anything else is that line as the error.
fn parse_json_pick(out: &str, n: usize) -> Result<Option<(usize, bool)>, String> {
    let line = out
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.is_empty() {
        return Ok(None);
    }
    let (id, print) = match line.split_once(' ') {
        Some((id, rest)) if rest.trim() == "print" => (id, true),
        Some(_) => return Err(line.to_string()),
        None => (line, false),
    };
    match id.parse::<usize>() {
        Ok(i) if i < n => Ok(Some((i, print))),
        _ => Err(line.to_string()),
    }
}

/// `KEYBIND_FIND_PICKER` if set, else fuzzel on Wayland, else dmenu (fuzzel can't run under X).
// ponytail: whitespace split, no quoted arguments; use shell-words parsing if a picker ever needs one with spaces.
fn cmd_for(picker: Option<&str>, wayland: Option<&str>) -> Vec<String> {
    let words = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
    match (
        picker.filter(|p| !p.trim().is_empty()),
        wayland.filter(|w| !w.is_empty()),
    ) {
        (Some(p), _) => words(p),
        (None, Some(_)) => words("fuzzel --dmenu"),
        (None, None) => words("dmenu -i -l 25"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Bind;

    fn entry(key: &str, desc: &str, provider: &str) -> Entry {
        Entry {
            provider: provider.into(),
            category: "c".into(),
            bind: Bind {
                key: key.into(),
                desc: desc.into(),
                action: String::new(),
                when: Default::default(),
            },
        }
    }

    fn ui() -> Ui {
        Ui::new((Keys::default(), None))
    }

    fn three() -> Vec<Entry> {
        vec![
            entry("super+b", "Launch the browser (firefox)", "xmonad"),
            entry("space f f", "Find  files [root]", "nvim"),
            entry("ctrl+b c", "create a new tab", "herdr"),
        ]
    }

    #[test]
    fn dmenu_line_format() {
        assert_eq!(
            dmenu_lines(&three()),
            [
                "super+b    Launch the browser (firefox)  [xmonad]",
                "space f f  Find  files [root]            [nvim]",
                "ctrl+b c   create a new tab              [herdr]",
            ]
        );
        assert_eq!(
            dmenu_lines(&[
                entry("k", "two\nlines", "p"),
                entry("k\t", "a\rb\tc\0d", "p\t")
            ]),
            ["k   two lines  [p]", "k   a b c d    [p ]"]
        );
    }

    #[test]
    fn scope_tags_follow_the_description_and_count_toward_its_width() {
        let mut tagged = entry("i", "Insert", "nvim");
        let name = |s: &str| s.parse::<crate::sheet::Name>().unwrap();
        tagged.bind.when = std::collections::BTreeMap::from([
            (name("mode"), name("insert").into()),
            (
                name("filetype"),
                vec![name("rust"), name("toml")].try_into().unwrap(),
            ),
        ]);
        let entries = [tagged, entry("super+b", "Browser", "xmonad")];
        assert_eq!(
            dmenu_lines(&entries),
            [
                "i        Insert [filetype=rust,toml] [mode=insert]  [nvim]",
                "super+b  Browser                                    [xmonad]",
            ]
        );
        assert_eq!(
            fzf_input(&entries),
            "0\ti       \tInsert [filetype=rust,toml] [mode=insert] \tnvim\n\
             1\tsuper+b \tBrowser                                   \txmonad\n"
        );
    }

    #[test]
    fn column_widths_and_caps() {
        let long_key = "k".repeat(40);
        let long_desc = "d".repeat(100);
        let rows = columns(&[
            entry(&long_key, "short", "a"),
            entry("x", &long_desc, "b"),
            entry("ü→", "ünïcode", "c"),
        ]);
        // Capped widths: 28 and 60. Longer values overflow only their own row.
        assert_eq!(rows[0][0], long_key);
        assert_eq!(rows[1][1], long_desc);
        assert_eq!(rows[1][0], format!("x{}", " ".repeat(27)));
        assert_eq!(rows[0][1], format!("short{}", " ".repeat(55)));
        // Width counts chars, not bytes.
        assert_eq!(rows[2][0].chars().count(), 28);
        assert_eq!(rows[2][1].chars().count(), 60);
        assert_eq!(rows[2][2], "c");
        // Under the caps, the widest value sets the width.
        let rows = columns(&[entry("ab", "d", "p"), entry("ü", "dd", "p")]);
        assert_eq!(rows[1][0], "ü ");
        assert_eq!(rows[0][1], "d ");
        assert!(columns(&[]).is_empty());
    }

    #[test]
    fn fzf_lines_carry_the_index() {
        assert_eq!(
            fzf_input(&three()),
            "0\tsuper+b   \tLaunch the browser (firefox) \txmonad\n\
             1\tspace f f \tFind  files [root]           \tnvim\n\
             2\tctrl+b c  \tcreate a new tab             \therdr\n"
        );
    }

    #[test]
    fn fzf_argv() {
        let args = fzf_args(&ui());
        for want in [
            "--delimiter=\t",
            "--with-nth=2..",
            "--accept-nth=1",
            "--nth=..",
            "--tabstop=1",
            "--tiebreak=index",
            "--prompt=all> ",
            "--with-shell=sh -c",
            "--layout=reverse",
            "--header=ctrl-s: search scope | ctrl-k: search by pressing keys",
        ] {
            assert!(args.iter().any(|a| a == want), "{want} in {args:?}");
        }
        let bind = args
            .iter()
            .find(|a| a.starts_with("--bind=ctrl-s:transform:"));
        let bind = bind.expect("ctrl-s binding");
        for step in [
            "change-nth(1)",
            "change-nth(2)",
            "change-nth(3)",
            "change-nth(..)",
        ] {
            assert!(bind.contains(step), "{bind}");
        }
        // Order is the caller's: nothing re-sorts or reverses the input.
        assert!(!args
            .iter()
            .any(|a| a == "--tac" || a.starts_with("--no-sort")));
    }

    #[test]
    fn ctrl_s_cycles_all_keys_description_program() {
        // `$FZF_NTH` is empty at start (`--nth=..`) and `..` after a full cycle.
        let step = |nth: &str| {
            let out = Command::new("sh")
                .args(["-c", SCOPE_CYCLE])
                .env("FZF_NTH", nth)
                .output()
                .unwrap();
            String::from_utf8(out.stdout)
                .unwrap()
                .trim_end()
                .to_string()
        };
        assert_eq!(step(""), "change-nth(1)+change-prompt(keys> )");
        assert_eq!(step(".."), "change-nth(1)+change-prompt(keys> )");
        assert_eq!(step("1"), "change-nth(2)+change-prompt(description> )");
        assert_eq!(step("2"), "change-nth(3)+change-prompt(program> )");
        assert_eq!(step("3"), "change-nth(..)+change-prompt(all> )");
    }

    #[test]
    fn alacritty_runs_this_binary_in_fzf_mode() {
        let args = terminal_args(
            &default_terminal(),
            Path::new("/bin/kf"),
            Path::new("/r/list"),
            Path::new("/r/out"),
        );
        let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        assert_eq!(
            args,
            [
                "alacritty",
                "--class",
                "keybind-find,keybind-find",
                "--title",
                "keybind-find",
                "-o",
                "window.dimensions.columns=140",
                "-o",
                "window.dimensions.lines=30",
                "-e",
                "/bin/kf",
                "__fzf",
                "/r/list",
                "/r/out"
            ]
        );
        // `[picker] terminal`: that list, then the same command.
        let kitty: Vec<String> = ["kitty", "--class", "keybind-find", "-e"]
            .map(String::from)
            .to_vec();
        let args = terminal_args(
            &kitty,
            Path::new("/bin/kf"),
            Path::new("/r/list"),
            Path::new("/r/out"),
        );
        let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        assert_eq!(
            args,
            [
                "kitty",
                "--class",
                "keybind-find",
                "-e",
                "/bin/kf",
                "__fzf",
                "/r/list",
                "/r/out"
            ]
        );
    }

    fn strings(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_picker_config_and_the_environment_choose_the_picker() {
        use config::{Picker, PickerKind};
        let all = |_: &str| true;
        let none = |_: &str| false;
        let kitty = strings(&["kitty", "-e"]);
        let rofi = strings(&["rofi", "-dmenu"]);
        let fzf = |t: Vec<String>| (Choice::Fzf(t), None);
        let dmenu = |c: &[&str]| (Choice::Dmenu(strings(c)), None);
        let cfg = |kind, command: Option<Vec<String>>, terminal: Option<Vec<String>>| Picker {
            kind,
            command,
            terminal,
        };
        let default = Picker::default();
        // No [picker]: today's order.
        assert_eq!(choose(&default, None, None, all), fzf(default_terminal()));
        assert_eq!(
            choose(&default, None, Some("wayland-1"), all),
            dmenu(&["fuzzel", "--dmenu"])
        );
        assert_eq!(
            choose(&default, None, None, none),
            dmenu(&["dmenu", "-i", "-l", "25"])
        );
        // The terminal is what fzf needs installed besides itself.
        let only_fzf = |p: &str| p == "fzf";
        assert_eq!(
            choose(&default, None, None, only_fzf),
            dmenu(&["dmenu", "-i", "-l", "25"])
        );
        let with_kitty = cfg(None, None, Some(kitty.clone()));
        assert_eq!(
            choose(&with_kitty, None, None, |p| p == "fzf" || p == "kitty"),
            fzf(kitty.clone())
        );
        // Each kind.
        let json = cfg(
            Some(PickerKind::Json),
            Some(strings(&["front", "--x"])),
            None,
        );
        assert_eq!(
            choose(&json, None, None, all),
            (Choice::Json(strings(&["front", "--x"])), None)
        );
        let dm = cfg(Some(PickerKind::Dmenu), Some(rofi.clone()), None);
        assert_eq!(
            choose(&dm, None, None, all),
            (Choice::Dmenu(rofi.clone()), None)
        );
        let dm_default = cfg(Some(PickerKind::Dmenu), None, None);
        assert_eq!(
            choose(&dm_default, None, Some("w"), all),
            dmenu(&["fuzzel", "--dmenu"])
        );
        // `kind = "fzf"` on Wayland too (kitty runs there); without its tools, the dmenu-style picker and why.
        let f = cfg(Some(PickerKind::Fzf), None, Some(kitty.clone()));
        assert_eq!(choose(&f, None, Some("w"), all), fzf(kitty.clone()));
        let (choice, why) = choose(&f, None, None, none);
        assert_eq!(choice, Choice::Dmenu(strings(&["dmenu", "-i", "-l", "25"])));
        assert!(why.unwrap().contains("fzf or kitty is not installed"));
        // KEYBIND_FIND_PICKER overrides every kind, as a dmenu-style command; blank is unset.
        for c in [&default, &json, &dm, &f] {
            assert_eq!(
                choose(c, Some("bemenu -i"), None, all),
                dmenu(&["bemenu", "-i"])
            );
        }
        assert_eq!(
            choose(&json, Some("  "), None, all).0,
            Choice::Json(strings(&["front", "--x"]))
        );
        // A dmenu-style command given without a kind is the fallback's, in today's order.
        let plain = cfg(None, Some(rofi.clone()), None);
        assert_eq!(choose(&plain, None, None, all), fzf(default_terminal()));
        assert_eq!(
            choose(&plain, None, Some("w"), all),
            (Choice::Dmenu(rofi), None)
        );
    }

    fn window() -> keybind_find_sheet::Context {
        keybind_find_sheet::Context {
            wm: Some("xmonad".into()),
            class: Some("Alacritty".into()),
            title: Some("nvim main.rs".into()),
            program: Some("nvim".into()),
            facts: Default::default(),
        }
    }

    #[test]
    fn the_json_request_carries_the_window_and_the_ranked_entries() {
        let mut entries = three();
        entries[1].bind.when.insert(
            "mode".parse().unwrap(),
            crate::sheet::Values::try_from(vec![
                "normal".parse().unwrap(),
                "visual".parse().unwrap(),
            ])
            .unwrap(),
        );
        entries[1].bind.when.insert(
            "filetype".parse().unwrap(),
            "rust".parse::<crate::sheet::Name>().unwrap().into(),
        );
        let v: serde_json::Value =
            serde_json::from_slice(&json_request(&entries, &window()).unwrap()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["context"]["wm"], "xmonad");
        assert_eq!(v["context"]["program"], "nvim");
        assert_eq!(v["entries"].as_array().unwrap().len(), 3);
        assert_eq!(
            v["entries"][1],
            serde_json::json!({
                "id": 1, "key": "space f f", "desc": "Find  files [root]", "program": "nvim",
                "when": {"filetype": "rust", "mode": ["normal", "visual"]},
                "tags": ["[filetype=rust]", "[mode=normal,visual]"]
            })
        );
        assert_eq!(v["entries"][0]["when"], serde_json::json!({}));
        assert_eq!(v["entries"][0]["tags"], serde_json::json!([]));
    }

    #[test]
    fn a_crashed_frontend_says_so_and_escape_is_quiet() {
        use std::os::unix::process::ExitStatusExt;
        let exit = |code: i32| std::process::ExitStatus::from_raw(code << 8);
        assert_eq!(crash_line("f", exit(1), " \n"), None);
        assert_eq!(
            crash_line("f", exit(3), "Traceback\n  boom\n").unwrap(),
            "picker \"f\" failed (exit status: 3): Traceback boom; nothing sent"
        );
        let killed = crash_line("f", std::process::ExitStatus::from_raw(9), "").unwrap();
        assert!(killed.contains("signal: 9"), "{killed}");
        let long = crash_line("f", exit(1), &"x".repeat(500)).unwrap();
        assert!(long.contains(&format!("{}…;", "x".repeat(300))), "{long}");
        // Key search is off on Wayland, and says which variable did it.
        assert!(x11_only(None).is_ok());
        assert!(x11_only(Some("".as_ref())).is_ok());
        let e = x11_only(Some("wayland-1".as_ref()))
            .unwrap_err()
            .to_string();
        assert!(e.contains("WAYLAND_DISPLAY"), "{e}");
    }

    #[test]
    fn on_path_takes_a_path_too() {
        assert!(on_path("sh"));
        assert!(on_path("/bin/sh"));
        assert!(!on_path("/nonexistent/kitty"));
        assert!(!on_path("/etc/passwd"), "not executable");
        assert!(!on_path("kbf-no-such-program"));
    }

    #[test]
    fn a_json_pick_is_an_id_and_maybe_print() {
        assert_eq!(parse_json_pick("2\n", 3), Ok(Some((2, false))));
        assert_eq!(parse_json_pick("0 print\nignored", 3), Ok(Some((0, true))));
        assert_eq!(parse_json_pick("  1  \n", 3), Ok(Some((1, false))));
        assert_eq!(parse_json_pick("", 3), Ok(None));
        assert_eq!(parse_json_pick("\n\n", 3), Ok(None));
        assert_eq!(parse_json_pick("\n  \n2 print\n", 3), Ok(Some((2, true))));
        for bad in ["3", "-1", "x", "1 send", "1 print now", "1,2"] {
            assert_eq!(parse_json_pick(bad, 3), Err(bad.to_string()), "{bad}");
        }
    }

    /// Writes an executable `sh` script (by `sh`, so no write handle of ours can be inherited: ETXTBSY).
    fn script(path: &Path, body: &str) {
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(path)
            .arg(format!("#!/bin/sh\n{body}"))
            .status()
            .unwrap();
        assert!(ok.success());
    }

    /// The rofi template's own test (stub `rofi` on `PATH`), so CI runs it.
    #[test]
    fn the_rofi_template_test_passes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/frontends/rofi");
        let out = match Command::new("python3")
            .arg(dir.join("test_rofi.py"))
            .output()
        {
            Ok(out) => out,
            // Skipped without python3, except under CI, where it must run.
            Err(e) if std::env::var_os("CI").is_none() => {
                return eprintln!("skipped: no python3 ({e})")
            }
            Err(e) => panic!("python3 is needed for this test under CI: {e}"),
        };
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn a_json_frontend_round_trip() {
        let dir = std::env::temp_dir().join(format!("kbf-picker-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let front = dir.join("front");
        let seen = dir.join("seen.json");
        let run = |reply: &str| {
            // Saves its request and prints `reply`, or exits as `reply` says.
            script(
                &front,
                &format!(
                    "cat > '{}'\necho \"$@\" > '{}.args'\n{reply}",
                    seen.display(),
                    seen.display()
                ),
            );
            pick_json(
                &[front.display().to_string(), "--arg".into()],
                &three(),
                &window(),
            )
        };
        assert_eq!(run("echo 2").unwrap(), Some((2, false)));
        let req: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
        assert_eq!(req["entries"][2]["key"], "ctrl+b c");
        assert_eq!(req["context"]["class"], "Alacritty");
        let args = std::fs::read_to_string(format!("{}.args", seen.display())).unwrap();
        assert_eq!(args.trim(), "--arg");
        assert_eq!(run("echo '1 print'").unwrap(), Some((1, true)));
        // Cancels: nothing printed, a non-zero exit (even with a pick printed), an unknown id (one stderr line).
        assert_eq!(run("true").unwrap(), None);
        assert_eq!(run("echo 1; exit 1").unwrap(), None);
        // A crash (stderr, or a signal) is a cancel too, said on stderr: nothing sent.
        assert_eq!(run("echo oops >&2; exit 3").unwrap(), None);
        assert_eq!(run("kill -9 $$").unwrap(), None);
        assert_eq!(run("echo 7").unwrap(), None);
        assert_eq!(run("echo nope").unwrap(), None);
        // A frontend that can't start is an error, not a cancel.
        let e = pick_json(
            &[dir.join("missing").display().to_string()],
            &three(),
            &window(),
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("picker"), "{e:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_match_by_whole_chord_prefix() {
        let none = HashMap::new();
        let m = |key: &str, pressed: &[&str]| starts_with_keys(key, pressed, &none);
        // Modifiers still held: any chord with at least those modifiers.
        assert!(m("super+shift+Return", &["super+shift"]));
        assert!(m("super+shift+Return", &["super"]));
        assert!(!m("super+Return", &["super+shift"]));
        assert!(m("super+shift+Return", &["super+shift+Return"]));
        // A whole chord, never part of one.
        assert!(!m("super+shift+Return", &["super+s"]));
        assert!(!m("super+shift+Return", &["super+Return"]));
        // Sequences: a prefix of the chords.
        assert!(m("ctrl+b t", &["ctrl+b"]));
        assert!(m("ctrl+b t", &["ctrl+b", "t"]));
        assert!(!m("ctrl+b t", &["ctrl+b", "x"]));
        assert!(!m("ctrl+b", &["ctrl+b", "t"]));
        assert!(m("ctrl+b ctrl+n", &["ctrl+b", "ctrl"]));
        assert!(!m("ctrl+b t", &["ctrl+b", "ctrl"]));
        assert!(!m("ctrl+b t", &["ctrl", "t"]), "held modifiers only last");
        // shift+letter and the uppercase letter are one chord, both ways.
        assert!(m("K", &["shift+k"]));
        assert!(m("shift+k", &["K"]));
        assert!(m("super+K", &["super+shift+k"]));
        assert!(!m("k", &["shift+k"]));
        // Modifier order doesn't matter; padding from the columns doesn't either.
        assert!(m("shift+super+Return", &["super+shift+Return"]));
        assert!(m("space f f   ", &["space", "f"]));
        // Nothing pressed yet: everything.
        assert!(m("anything", &[]));
    }

    #[test]
    fn shifted_symbols_match_shift_and_the_base_key() {
        let shifted: HashMap<String, String> = [("question", "slash"), ("colon", "semicolon")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into();
        let m = |key: &str, pressed: &[&str]| starts_with_keys(key, pressed, &shifted);
        assert!(m("question", &["shift+slash"]));
        assert!(m("colon w", &["shift+semicolon"]));
        assert!(m("super+question", &["super+shift"]));
        assert!(!m("slash", &["shift+slash"]));
        assert!(!m("question", &["slash"]));
        // A pressed key is a base keysym already: no shift is added to it.
        assert!(!m("shift+slash", &["question"]));
    }

    #[test]
    fn narrowing_keeps_lines_whole_and_in_order() {
        let list = "0\tsuper+b \tBrowser \txmonad\n\
                    1\tctrl+b c \tnew tab \therdr\n\
                    2\tctrl+b n \tnext tab \therdr\n\
                    3\tK \thover \tnvim\n";
        let none = HashMap::new();
        assert_eq!(
            narrow(list, &["ctrl+b"], &none),
            "1\tctrl+b c \tnew tab \therdr\n2\tctrl+b n \tnext tab \therdr\n"
        );
        assert_eq!(narrow(list, &["shift+k"], &none), "3\tK \thover \tnvim\n");
        assert_eq!(narrow(list, &[], &none), list);
        assert_eq!(narrow(list, &["F1"], &none), "");
        // The lines fzf really gets: the key column is padded, the index survives.
        let real = fzf_input(&three());
        let kept = narrow(&real, &["ctrl+b"], &none);
        assert_eq!(kept.lines().count(), 1, "{kept}");
        assert!(kept.starts_with("2\tctrl+b c"), "{kept}");
    }

    #[test]
    fn each_key_updates_the_sequence_and_fzf() {
        let shown = Path::new("/run/x/keys");
        let (mut pressed, mut held) = (Vec::new(), String::new());
        let ui = ui();
        let mut step = |k| step(&mut pressed, &mut held, k, shown, &ui);
        // The grab: a fresh search over the whole list, with an empty query.
        let (want, actions) = step(Key::Grabbed);
        assert!(want.is_empty());
        assert_eq!(
            actions,
            "change-query()+change-header(key search: press the keys | Escape: text search | \
             Return/KP_Enter: run | BackSpace: undo)+reload:cat '/run/x/keys'"
        );
        // Held modifiers come last, and a chord replaces them.
        let (want, actions) = step(Key::Held("super+shift"));
        assert_eq!(want, ["super+shift"]);
        assert!(
            actions.starts_with("change-header(key search: super+shift |"),
            "{actions}"
        );
        let (want, actions) = step(Key::Chord("super+shift+Return"));
        assert_eq!(want, ["super+shift+Return"]);
        assert!(!actions.contains("change-query"), "{actions}");
        let (want, _) = step(Key::Held("ctrl"));
        assert_eq!(want, ["super+shift+Return", "ctrl"]);
        let (want, _) = step(Key::Held(""));
        assert_eq!(want, ["super+shift+Return"]);
        assert_eq!(
            end_header(&pressed, &ui),
            "keys: super+shift+Return | text search | Escape: close | ctrl-k: press keys again | \
             ctrl-s: search scope"
        );
        assert_eq!(
            end_header(&[], &ui),
            "keys: none | text search | Escape: close | ctrl-k: press keys again | ctrl-s: search scope"
        );
    }

    #[test]
    fn undo_takes_back_the_last_chord() {
        let shown = Path::new("/run/x/keys");
        let (mut pressed, mut held) = (Vec::new(), String::new());
        let ui = ui();
        let mut step = |k| step(&mut pressed, &mut held, k, shown, &ui);
        step(Key::Grabbed);
        // Nothing pressed: unchanged.
        let (want, actions) = step(Key::Undo);
        assert!(want.is_empty());
        assert!(
            actions.contains("key search: press the keys |"),
            "{actions}"
        );
        step(Key::Chord("ctrl+b"));
        step(Key::Chord("t"));
        let (want, actions) = step(Key::Undo);
        assert_eq!(want, ["ctrl+b"]);
        assert!(actions.contains("key search: ctrl+b |"), "{actions}");
        // A held modifier stays until it is released.
        step(Key::Held("ctrl"));
        let (want, _) = step(Key::Undo);
        assert_eq!(want, ["ctrl"]);
        let (want, _) = step(Key::Held(""));
        assert!(want.is_empty());
    }

    #[test]
    fn undo_and_the_ends_follow_the_config() {
        use crate::capture::chord_for;
        // ctrl+BackSpace is a chord, a bare BackSpace undoes (the default).
        let d = Keys::default();
        assert!(!d.undo.contains(&chord_for(4, "BackSpace").unwrap()));
        assert!(d.undo.contains(&chord_for(0, "BackSpace").unwrap()));
        // `undo = "ctrl+u"`: ctrl+u undoes and a bare BackSpace is a key like any other.
        let moved = Keys {
            undo: vec!["ctrl+u".into()],
            ..Keys::default()
        };
        assert!(moved.undo.contains(&chord_for(4, "u").unwrap()));
        assert!(!moved.undo.contains(&chord_for(0, "BackSpace").unwrap()));
        let ui = Ui::new((moved, None));
        assert!(
            ui.key_search("x").ends_with("| ctrl+u: undo"),
            "{}",
            ui.key_search("x")
        );
    }

    #[test]
    fn own_keys_are_listed_after_the_rest_and_pick_nothing() {
        let own = own_entries(&Keys::default());
        let keys: Vec<(&str, &str)> = own
            .iter()
            .map(|e| (e.category.as_str(), e.bind.key.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("key search", "Return"),
                ("key search", "KP_Enter"),
                ("key search", "Escape"),
                ("key search", "BackSpace"),
                ("text search", "ctrl+k"),
                ("text search", "ctrl+s"),
                ("text search", "Return"),
                ("text search", "Escape"),
            ]
        );
        assert!(own.iter().all(|e| e.provider == "keybind-find"));
        // Found by text too: `undo` names the BackSpace entry.
        let undo: Vec<&Entry> = own
            .iter()
            .filter(|e| e.bind.desc.contains("undo"))
            .collect();
        assert_eq!(undo.len(), 1);
        assert_eq!(undo[0].bind.key, "BackSpace");
        // The keys in force, not the defaults.
        let moved = own_entries(&Keys {
            undo: vec!["ctrl+u".into()],
            run: vec!["ctrl+j".into()],
            ..Keys::default()
        });
        assert_eq!(moved.len(), 7);
        assert!(moved.iter().any(|e| e.bind.key == "ctrl+u"));
        assert!(!moved.iter().any(|e| e.bind.key == "BackSpace"));
        // After the real entries, with indices from entries.len() up; such a pick is no pick (as `pick_fzf` filters).
        let entries = three();
        let all: Vec<Entry> = entries.iter().cloned().chain(own).collect();
        let lines = fzf_input(&all);
        let line = lines.lines().nth(entries.len()).unwrap();
        assert!(
            line.starts_with("3\tReturn") && line.ends_with("\tkeybind-find"),
            "{line}"
        );
        assert_eq!(pick_index("3\n", entries.len()), None);
        assert_eq!(pick_index("10\n", entries.len()), None);
        assert_eq!(pick_index("2\n", entries.len()), Some(2));
    }

    #[test]
    fn text_search_keys_come_from_the_config() {
        let ui = Ui::new((
            Keys {
                key_search: vec!["F2".into(), "alt+k".into()],
                scope: vec!["ctrl+o".into()],
                ..Keys::default()
            },
            None,
        ));
        let args = fzf_args(&ui);
        assert!(
            args.iter()
                .any(|a| a.starts_with("--bind=ctrl-o:transform:")),
            "{args:?}"
        );
        assert!(
            !args.iter().any(|a| a.starts_with("--bind=ctrl-s:")),
            "{args:?}"
        );
        let ks = key_search_args(
            Path::new("/bin/kbf"),
            Path::new("/r/keys.sock"),
            Path::new("/r/fzf.sock"),
            &ui,
        )
        .unwrap();
        assert!(
            ks.iter()
                .any(|a| a.starts_with("--bind=f2:execute-silent:")),
            "{ks:?}"
        );
        assert!(
            ks.iter()
                .any(|a| a.starts_with("--bind=alt-k:execute-silent:")),
            "{ks:?}"
        );
        assert!(
            !ks.iter().any(|a| a.starts_with("--bind=ctrl-k:")),
            "{ks:?}"
        );
        assert_eq!(
            end_header(&[], &ui),
            "keys: none | text search | Escape: close | f2/alt-k: press keys again | ctrl-o: search scope"
        );
        let off = no_key_search_args(&anyhow::anyhow!("no X"), &ui);
        assert!(
            off.iter().any(|a| a == "--bind=alt-k:change-header:no X"),
            "{off:?}"
        );
    }

    #[test]
    fn a_refused_config_heads_every_header() {
        let ui = Ui::new((
            Keys::default(),
            Some("/c/config.toml: conflict: run and text_search both use Escape (x)".into()),
        ));
        let note =
            "\x1b[31m/c/config.toml: conflict: run and text_search both use Escape (x)\x1b[0m | ";
        assert!(fzf_args(&ui)
            .iter()
            .any(|a| a.starts_with(&format!("--header={note}"))));
        assert!(end_header(&[], &ui).starts_with(note));
        assert!(fail_header(&anyhow::anyhow!("idle"), &ui).starts_with(note));
        // Its `)` can't end the bracketed header early.
        let (_, actions) = step(
            &mut Vec::new(),
            &mut String::new(),
            Key::Grabbed,
            Path::new("/k"),
            &ui,
        );
        assert!(
            actions.contains(&format!("change-header[{note}key search:")),
            "{actions}"
        );
        assert_eq!(bracketed("a", "x)y"), "a[x)y]");
        assert_eq!(bracketed("a", "xy"), "a(xy)");
        assert_eq!(bracketed("a", ")]}>~!@#%^|"), "a(]}>~!@#%^|)");
        // With key search off too.
        let off = no_key_search_args(&anyhow::anyhow!("no X"), &ui);
        assert!(
            off[0].starts_with(&format!("--header={note}no X | text search")),
            "{off:?}"
        );
        assert!(off
            .iter()
            .any(|a| *a == format!("--bind=ctrl-k:change-header:{note}no X")));
    }

    /// A stand-in for fzf's server: answers each connection with the next of `replies`, and returns the requests.
    fn fake_fzf(sock: &Path, replies: Vec<String>) -> std::thread::JoinHandle<Vec<String>> {
        let listener = UnixListener::bind(sock).unwrap();
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            for reply in replies {
                let (mut c, _) = listener.accept().unwrap();
                // Read until the body is in, however the request was split.
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = c.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&got);
                    // No Content-Length (a GET): the headers are the whole request.
                    let done = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                        head.lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|n| n.parse::<usize>().ok())
                            .is_none_or(|n| body.len() >= n)
                    });
                    if n == 0 || done {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&got).into_owned());
                c.write_all(reply.as_bytes()).unwrap();
            }
            requests
        })
    }

    /// fzf's `GET /` answer, in the shape fzf 0.74 sends, matching `texts`.
    fn state(reading: bool, texts: &[&str]) -> String {
        let matches: Vec<String> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| format!(r#"{{"index":{i},"text":{}}}"#, serde_json::json!(t)))
            .collect();
        format!(
            r#"{{"reading":{reading},"progress":100,"query":"","position":0,"totalCount":{n},"matchCount":{n},"matches":[{}],"selected":[]}}"#,
            matches.join(","),
            n = texts.len()
        )
    }

    #[test]
    fn enter_waits_for_the_narrowed_list() {
        let lines = "1\tctrl+b c \tnew tab \therdr\n2\tctrl+b n \tnext tab \therdr\n";
        let want = [
            "1\tctrl+b c \tnew tab \therdr",
            "2\tctrl+b n \tnext tab \therdr",
        ];
        assert!(shows(&state(false, &want), lines));
        assert!(!shows(&state(true, &want), lines), "still reading");
        // A list of the same length that isn't this one (a held modifier was released).
        assert!(!shows(&state(false, &[want[0], "3\tctrl+b x"]), lines));
        assert!(!shows(&state(false, &want[..1]), lines));
        assert!(shows(&state(false, &[]), ""));
        assert!(!shows("not json", ""));

        // Through the socket: polls until fzf shows the list, asking for that many lines.
        let dir = RunDir::new().unwrap();
        let sock = dir.0.join("f.sock");
        let ok = |body: String| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
        };
        let server = fake_fzf(&sock, vec![ok(state(true, &[])), ok(state(false, &want))]);
        assert!(wait_for_list(&sock, lines).is_ok());
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2);
        // No Content-Length: real fzf refuses `Content-Length: 0`.
        assert_eq!(
            requests[0],
            "GET /?limit=2 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn how_a_key_search_ends() {
        let pressed = ["ctrl+b".to_string()];
        // Enter: fzf's own accept, which prints the highlighted line's index (`--accept-nth=1`).
        let ui = ui();
        assert_eq!(end_actions(&End::Run, &pressed, &ui), "accept");
        assert_eq!(
            end_actions(&End::TextSearch, &pressed, &ui),
            format!("change-header:{}", end_header(&pressed, &ui))
        );
        // The idle limit and a failed start: why, that this is text search, and that ctrl-k starts again.
        let idle = anyhow::anyhow!("no key pressed for 30 s, stopped listening");
        assert_eq!(
            fail_header(&idle, &ui),
            "no key pressed for 30 s, stopped listening | text search | Escape: close | ctrl-k: press keys again | \
             ctrl-s: search scope"
        );
    }

    #[test]
    fn fzf_versions() {
        assert_eq!(parse_fzf_version("0.74.5 (c0a193e0)\n"), Some((0, 74)));
        assert_eq!(parse_fzf_version("0.66.0"), Some((0, 66)));
        assert!(parse_fzf_version("0.65.2 (brew)").is_some_and(|v| v < (0, 66)));
        assert_eq!(parse_fzf_version(""), None);
        assert_eq!(parse_fzf_version("fzf"), None);
    }

    #[test]
    fn key_search_off_keeps_the_whole_reason() {
        let e = anyhow::anyhow!("listening on /run/x/keys.sock: a, b: c");
        // Off: no search on start, text search with the reason in the header. fzf takes the last `--header`, and
        // these come after `fzf_args`'s.
        let args = no_key_search_args(&e, &ui());
        assert!(!args.iter().any(|a| a.starts_with("--bind=start:")));
        let all: Vec<String> = fzf_args(&ui()).into_iter().chain(args.clone()).collect();
        let header = all.iter().rfind(|a| a.starts_with("--header="));
        assert!(
            header.is_some_and(|h| h.contains("a, b: c | text search")),
            "{all:?}"
        );
        assert_eq!(
            args,
            [
                "--header=listening on /run/x/keys.sock: a, b: c | text search | ctrl-s: search scope",
                "--bind=ctrl-k:change-header:listening on /run/x/keys.sock: a, b: c",
            ]
        );
    }

    #[test]
    fn post_reads_fzfs_answer() {
        let dir = RunDir::new().unwrap();
        let sock = dir.0.join("f.sock");
        let server = fake_fzf(
            &sock,
            vec![
                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into(),
                "HTTP/1.1 400 Bad Request\r\nContent-Length: 15\r\n\r\nunknown action\n".into(),
            ],
        );
        assert!(post(&sock, "change-header(x)+reload:cat 'y'").is_ok());
        let err = post(&sock, "bogus").unwrap_err().to_string();
        assert!(
            err.contains("400") && err.contains("unknown action"),
            "{err}"
        );
        let requests = server.join().unwrap();
        assert_eq!(
            requests[0],
            "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 31\r\nConnection: close\r\n\r\n\
             change-header(x)+reload:cat 'y'"
        );
    }

    #[test]
    fn ctrl_k_binding_quotes_paths_and_refuses_a_quote() {
        let args = key_search_args(
            Path::new("/opt/my tools/keybind-find"),
            Path::new("/run/user/1000/keybind-find-1-2/keys.sock"),
            Path::new("/run/user/1000/keybind-find-1-2/fzf.sock"),
            &ui(),
        )
        .unwrap();
        let run = "execute-silent:'/opt/my tools/keybind-find' __capture-signal \
                   '/run/user/1000/keybind-find-1-2/keys.sock'";
        // Key search starts on open, and again on `ctrl-k`.
        assert_eq!(
            args,
            [
                "--listen=/run/user/1000/keybind-find-1-2/fzf.sock".to_string(),
                "--header=key search starting | ctrl-k: start it | ctrl-s: search scope"
                    .to_string(),
                format!("--bind=start:{run}"),
                format!("--bind=ctrl-k:{run}"),
            ]
        );
        let sock = Path::new("/run/x/keys.sock");
        let fzf = Path::new("/run/x/fzf.sock");
        let err = key_search_args(Path::new("/opt/it's/kbf"), sock, fzf, &ui()).unwrap_err();
        assert!(err.to_string().contains("'"), "{err}");
        assert!(key_search_args(Path::new("/opt/{q}/kbf"), sock, fzf, &ui()).is_err());
        assert!(key_search_args(
            Path::new("/bin/kbf"),
            Path::new("/run/a'b/keys.sock"),
            fzf,
            &ui()
        )
        .is_err());
    }

    #[test]
    fn errors_become_one_header_line() {
        let e = anyhow::anyhow!("line one\nline two").context("key capture");
        assert_eq!(one_line(&e), "key capture: line one line two");
    }

    #[test]
    fn out_file_parsing() {
        assert_eq!(parse_pick("3\n"), Some(3));
        assert_eq!(parse_pick(""), None);
        assert_eq!(parse_pick("garbage\n"), None);
        assert_eq!(parse_pick("-1"), None);
    }

    #[test]
    fn when_fzf_is_used() {
        assert!(use_fzf(None, None, true));
        assert!(use_fzf(Some(" "), Some(""), true));
        assert!(!use_fzf(None, None, false));
        assert!(!use_fzf(None, Some("wayland-0"), true));
        assert!(!use_fzf(Some("head -1"), None, true));
    }

    #[test]
    fn run_dir_is_private_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = RunDir::new().unwrap();
        let path = dir.0.clone();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        std::fs::write(path.join("list"), "x").unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn picker_command() {
        let v = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            cmd_for(Some("rofi -dmenu -i"), Some("wayland-0")),
            v("rofi -dmenu -i")
        );
        assert_eq!(cmd_for(None, Some("wayland-0")), v("fuzzel --dmenu"));
        assert_eq!(cmd_for(None, None), v("dmenu -i -l 25"));
        assert_eq!(cmd_for(Some(""), None), v("dmenu -i -l 25"));
        assert_eq!(cmd_for(Some("  "), Some("")), v("dmenu -i -l 25"));
    }

    fn with(picker: &str) -> anyhow::Result<Option<usize>> {
        pick_with(&cmd_for(Some(picker), None), &three())
    }

    #[test]
    fn picks_through_a_real_command() {
        assert_eq!(with("head -1").unwrap(), Some(0));
        assert_eq!(with("tail -1").unwrap(), Some(2));
        // The line with `[` and two spaces in its description round-trips.
        assert_eq!(with("sed -n 2p").unwrap(), Some(1));
    }

    #[test]
    fn cancel_and_nonmatching_text() {
        assert_eq!(with("true").unwrap(), None);
        assert_eq!(with("false").unwrap(), None);
        assert_eq!(with("echo typed something else").unwrap(), None);
    }

    #[test]
    fn missing_picker_names_the_command() {
        let e = format!("{:#}", with("/nonexistent/picker -x").unwrap_err());
        assert!(e.contains("/nonexistent/picker"), "{e}");
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    #[test]
    fn no_entries_opens_nothing() {
        // This picker would fail if it ran; with nothing to show, nothing runs.
        assert_eq!(pick_with(&sh("echo ran >&2; exit 2"), &[]).unwrap(), None);
    }

    #[test]
    fn a_failing_picker_is_an_error_not_a_cancel() {
        let e = format!(
            "{:#}",
            pick_with(&sh("echo 'cannot open display' >&2; exit 1"), &three()).unwrap_err()
        );
        assert!(e.contains("cannot open display"), "{e}");
        assert!(pick_with(&sh("kill -9 $$"), &three()).is_err());
        // Escape: non-zero and silent.
        assert_eq!(
            pick_with(&sh("cat >/dev/null; exit 1"), &three()).unwrap(),
            None
        );
    }

    #[test]
    fn several_picks_take_the_first() {
        assert_eq!(with("head -2").unwrap(), Some(0));
        assert_eq!(
            pick_with(&sh("sed -n 3p | sed 's/$/\r/'"), &three()).unwrap(),
            Some(2)
        );
    }

    #[test]
    fn large_input_to_a_picker_that_exits_early() {
        // Well past the 64 KiB pipe buffer: the write hits a closed pipe, which is fine.
        let many: Vec<Entry> = (0..4000)
            .map(|i| entry(&format!("k{i}"), &"x".repeat(40), "p"))
            .collect();
        assert_eq!(
            pick_with(&cmd_for(Some("true"), None), &many).unwrap(),
            None
        );
        assert_eq!(
            pick_with(&cmd_for(Some("head -1"), None), &many).unwrap(),
            Some(0)
        );
        // A filter that echoes everything while reading must not deadlock.
        assert_eq!(
            pick_with(&cmd_for(Some("cat"), None), &many).unwrap(),
            Some(0)
        );
    }
}
