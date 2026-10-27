//! Which providers apply to the focused window.

use anyhow::{bail, Context as _};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Context {
    pub window_id: Option<String>,
    pub class: Option<String>,
    /// X11 `WM_CLASS`'s first string, e.g. a Chromium app window's `crx_<id>`; `None` on native Wayland and under
    /// Hyprland (`hyprctl` doesn't report it).
    pub instance: Option<String>,
    pub title: Option<String>,
    pub providers: Vec<&'static str>,
}

/// Never fails: anything it can't read leaves that field `None`, the window-manager and espanso providers still
/// apply, and one warning goes to stderr. Under sway the window comes from sway's tree, under Hyprland from `hyprctl`,
/// under KDE on Wayland from `kdotool`, on X from X, and in any other Wayland session there is no focus source yet.
pub fn detect() -> Context {
    let wayland = set(env_var("WAYLAND_DISPLAY"));
    let session = current_session();
    let (mut ctx, program) = focus(session, wayland, read_focus, run);
    add_nested_nvim(&mut ctx.providers, run, std::path::Path::new("/proc"));
    let window = keybind_find_sheet::Context {
        wm: session.map(str::to_string),
        class: ctx.class.clone(),
        title: ctx.title.clone(),
        program,
        facts: Default::default(),
    };
    let _ = WINDOW.set(window.clone());
    crate::providers::plugins::add(&mut ctx.providers, window);
    ctx
}

/// An environment variable, a non-UTF-8 value read lossily (as `run` reads `WAYLAND_DISPLAY`), not as unset.
fn env_var(k: &str) -> Option<String> {
    std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
}

fn current_session() -> Option<&'static str> {
    session(env_var, wm_check_name, || {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self")
            .map(|m| user_comms(std::path::Path::new("/proc"), m.uid()))
            .unwrap_or_default()
    })
}

/// The context for `session`, with the focus read from sway's tree, Hyprland's or KWin's active window (each through `query`,
/// `run`'s shape), from X through `read_x`, or not at all under any other Wayland session: an XWayland window's stale
/// class would bring in a wrong sheet. Both readers are test seams. Also returns the program inside the window.
fn focus(
    session: Option<&'static str>,
    wayland: bool,
    read_x: impl FnOnce(&mut Context) -> anyhow::Result<Option<Foreground>>,
    query: impl FnOnce(&str, &[&str]) -> anyhow::Result<String>,
) -> (Context, Option<String>) {
    let wm = wm_provider(session);
    let mut ctx = Context {
        window_id: None,
        class: None,
        instance: None,
        title: None,
        providers: providers_for(wm, None, None, &[]),
    };
    let focus = match session {
        Some("sway") if wayland => {
            query("swaymsg", &["-t", "get_tree"]).and_then(|tree| read_sway_focus(&mut ctx, &tree))
        }
        Some("hyprland") if wayland => query("hyprctl", &["-j", "activewindow"])
            .and_then(|json| read_hyprland_focus(&mut ctx, &json)),
        Some("kde") if wayland => {
            query("kdotool", &KDOTOOL_FOCUS).and_then(|out| read_kde_focus(&mut ctx, &out))
        }
        _ if wayland => return (ctx, None),
        _ => read_x(&mut ctx),
    };
    let program = settle(&mut ctx, wm, focus);
    (ctx, program)
}

/// nvim inside herdr, tmux or screen, each asked through `run` only in its own context; screen also reads `proc`
/// (`/proc`). Both are test seams.
fn add_nested_nvim(
    providers: &mut Vec<&'static str>,
    run: impl Fn(&str, &[&str]) -> anyhow::Result<String> + Copy,
    proc: &std::path::Path,
) {
    add_nvim_in(providers, "herdr", || herdr_pane_runs_nvim(run));
    add_nvim_in(providers, "tmux", || tmux_pane_runs_nvim(run));
    add_nvim_in(providers, "screen", || screen_window_runs_nvim(run, proc));
}

/// In a context holding the multiplexer `outer` (herdr, tmux), adds nvim after it when its focused pane runs nvim.
/// Only that context calls `query`, so no other context pays for it; a failed query warns and keeps the plain
/// context.
fn add_nvim_in(
    providers: &mut Vec<&'static str>,
    outer: &str,
    query: impl FnOnce() -> anyhow::Result<bool>,
) {
    let Some(i) = providers.iter().position(|p| *p == outer) else {
        return;
    };
    match query() {
        Ok(true) => providers.insert(i + 1, "nvim"),
        Ok(false) => {}
        Err(e) => eprintln!(
            "keybind-find: {outer} pane: {e:#}; showing {outer}'s bindings without nvim's"
        ),
    }
}

/// The pid of the nvim found in the focused window (the terminal's foreground program, the program in tmux's or
/// herdr's active pane, or in screen's current window), for the nvim provider to ask the running nvim.
/// ponytail: a process-wide value set while `detect` runs, beside `Context` rather than in it, so `Context`'s literals
/// elsewhere don't change; move it into `Context` when a story owns those.
static NVIM_PID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

pub(crate) fn nvim_pid() -> Option<u32> {
    NVIM_PID.get().copied()
}

fn found_nvim(pid: u32) {
    let _ = NVIM_PID.set(pid);
}

/// The window `detect` found, as a plugin's request and a JSON picker's request carry it.
/// ponytail: beside `Context`, like `NVIM_PID`, so `Context`'s literals don't change.
static WINDOW: std::sync::OnceLock<keybind_find_sheet::Context> = std::sync::OnceLock::new();

/// The window this run's `detect` found (its `facts` empty: see `facts`); all unknown before `detect`.
pub fn window() -> keybind_find_sheet::Context {
    WINDOW.get().cloned().unwrap_or_default()
}

/// What a provider learned about where the user is (the running nvim's `mode` and `filetype`), for `scope::shown`.
/// ponytail: one provider sets it, once per run, beside its sheet rather than in it; carry facts in `Sheet` if a
/// second provider reports some.
static FACTS: std::sync::OnceLock<crate::scope::Facts> = std::sync::OnceLock::new();

pub(crate) fn set_facts(facts: crate::scope::Facts) {
    let _ = FACTS.set(facts);
}

/// The facts reported this run; none if no provider learned any.
pub fn facts() -> crate::scope::Facts {
    FACTS.get().cloned().unwrap_or_default()
}

/// Asks tmux (through `run`, a test seam) whether the active pane of its most recently active client runs nvim.
/// ponytail: with several tmux windows open, the most recent client is taken to be the X-focused one; match the
/// alacritty window's client by its tty (`#{client_tty}`) if that bites.
fn tmux_pane_runs_nvim(
    run: impl Fn(&str, &[&str]) -> anyhow::Result<String>,
) -> anyhow::Result<bool> {
    // `list-clients`, unlike `list-keys`, doesn't start a server when none is running.
    let runs = newest_client_runs_nvim(&run(
        "tmux",
        &[
            "list-clients",
            "-F",
            "#{client_activity} #{pane_current_command}",
        ],
    )?)?;
    if runs {
        // The pane's first process (its shell); nvim leads the pane's foreground group.
        let panes = run(
            "tmux",
            &["list-clients", "-F", "#{client_activity} #{pane_pid}"],
        );
        let leader = panes.ok().and_then(|p| {
            let pane: u32 = newest_client(&p).ok()??.parse().ok()?;
            tpgid(&std::fs::read_to_string(format!("/proc/{pane}/stat")).ok()?)
        });
        if let Some(pid) = leader {
            found_nvim(pid);
        }
    }
    Ok(runs)
}

/// Whether screen's current window runs nvim: the one attached session (`screen -ls`), its current window's number
/// (`screen -S <session> -Q number`), the server's child with `WINDOW=<number>` in its environment under `proc` (a
/// test seam for `/proc`), and that child's terminal's foreground group leader. Records nvim's pid.
/// ponytail: several attached sessions can't be told apart (screen has no client activity, unlike tmux): an error, not
/// a guess. Match the session to the client in the focused terminal if that bites. `WINDOW=<n>` is the number a
/// window was created with, so after `number` renumbers windows the lookup can miss or find another window; and the
/// session list, like `$SCREENDIR`, is this process's view, not the terminal's.
fn screen_window_runs_nvim(
    run: impl Fn(&str, &[&str]) -> anyhow::Result<String>,
    proc: &std::path::Path,
) -> anyhow::Result<bool> {
    let (pid, comm) = screen_window_program(run, proc)?;
    let runs = comm == "nvim";
    if runs {
        found_nvim(pid);
    }
    Ok(runs)
}

/// The pid and comm of the program leading screen's current window (see `screen_window_runs_nvim`).
fn screen_window_program(
    run: impl Fn(&str, &[&str]) -> anyhow::Result<String>,
    proc: &std::path::Path,
) -> anyhow::Result<(u32, String)> {
    let ls = run("screen", &["-ls"])?;
    let session = attached_session(&ls)?;
    let number = run("screen", &["-S", session, "-Q", "number"])?;
    let number: u32 = number
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("screen -Q number printed {number:?}"))?;
    let server: u32 = session
        .split('.')
        .next()
        .and_then(|p| p.parse().ok())
        .with_context(|| format!("no pid in the session name {session:?}"))?;
    let window = window_process(proc, server, number)
        .with_context(|| format!("no process of screen window {number} under pid {server}"))?;
    let stat =
        std::fs::read_to_string(proc.join(window.to_string()).join("stat")).unwrap_or_default();
    let leader = tpgid(&stat).unwrap_or(window);
    let comm =
        std::fs::read_to_string(proc.join(leader.to_string()).join("comm")).unwrap_or_default();
    Ok((leader, comm.trim_end().to_string()))
}

/// The one attached session's name (`<pid>.<name>`) in `screen -ls`: lines `\t<name>\t[date\t](<state>)`, attached
/// when the state is `(Attached)` or `(Multi, attached)`. None or several is an error.
fn attached_session(ls: &str) -> anyhow::Result<&str> {
    let attached: Vec<&str> = ls
        .lines()
        .filter(|l| l.starts_with('\t'))
        .filter(|l| {
            let state = l.rfind('(').map_or("", |i| l[i..].trim_end());
            matches!(state, "(Attached)" | "(Multi, attached)")
        })
        // The name runs to the next tab: `screen -S "my work"` has a space in it.
        .filter_map(|l| l[1..].split('\t').next())
        .collect();
    match attached.as_slice() {
        [one] => Ok(one),
        [] => bail!("no attached screen session"),
        many => bail!(
            "{} attached screen sessions; can't tell which is in this terminal",
            many.len()
        ),
    }
}

/// The child of screen's server `server` whose environment holds `WINDOW=<number>`.
fn window_process(proc: &std::path::Path, server: u32, number: u32) -> Option<u32> {
    let want = format!("WINDOW={number}");
    let tasks = std::fs::read_dir(proc.join(server.to_string()).join("task")).ok()?;
    tasks
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
        .flat_map(|c| c.split_whitespace().map(str::to_string).collect::<Vec<_>>())
        .filter_map(|c| c.parse::<u32>().ok())
        .find(|c| {
            std::fs::read(proc.join(c.to_string()).join("environ"))
                .is_ok_and(|e| e.split(|b| *b == 0).any(|v| v == want.as_bytes()))
        })
}

/// From `list-clients -F '#{client_activity} #{pane_current_command}'`: whether the client with the latest activity
/// (Unix seconds) has nvim in its active pane. No clients: no.
fn newest_client_runs_nvim(clients: &str) -> anyhow::Result<bool> {
    Ok(newest_client(clients)? == Some("nvim"))
}

/// From `list-clients -F '#{client_activity} <format>'`: what the client with the latest activity (Unix seconds)
/// printed after its activity; on a tie, the one listed last. No clients: `None`.
fn newest_client(clients: &str) -> anyhow::Result<Option<&str>> {
    let mut newest: Option<(u64, &str)> = None;
    for line in clients.lines().filter(|l| !l.trim().is_empty()) {
        let (activity, rest) = line
            .split_once(' ')
            .and_then(|(a, c)| Some((a.parse::<u64>().ok()?, c)))
            .with_context(|| {
                format!("tmux printed {line:?}, not a client's activity and command")
            })?;
        if newest.is_none_or(|(n, _)| activity >= n) {
            newest = Some((activity, rest));
        }
    }
    Ok(newest.map(|(_, rest)| rest))
}

/// Asks herdr (through `run`, a test seam) whether its focused pane runs nvim. `focused_pane_id` is the focus of
/// herdr's default server.
/// ponytail: with several herdr windows on one server, or a client started with `--session`/`--remote`, it may not
/// be the pane in the X-focused window; pass the client's session from its argv if that setup appears.
fn herdr_pane_runs_nvim(
    run: impl Fn(&str, &[&str]) -> anyhow::Result<String>,
) -> anyhow::Result<bool> {
    let pane = focused_pane_id(&run("herdr", &["api", "snapshot"])?)?;
    let info = run("herdr", &["pane", "process-info", "--pane", &pane])?;
    if let Some(pid) = nvim_pid_in(&info) {
        found_nvim(pid);
    }
    runs_nvim(&info)
}

/// The pid of the first of `foreground_processes` named `nvim` in `herdr pane process-info`.
fn nvim_pid_in(process_info_json: &str) -> Option<u32> {
    let v: serde_json::Value = serde_json::from_str(process_info_json).ok()?;
    v["result"]["process_info"]["foreground_processes"]
        .as_array()?
        .iter()
        .find(|p| p["name"].as_str() == Some("nvim"))
        .and_then(|p| u32::try_from(p["pid"].as_u64()?).ok())
}

/// `result.snapshot.focused_pane_id` from `herdr api snapshot`.
fn focused_pane_id(snapshot_json: &str) -> anyhow::Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(snapshot_json).context("parsing herdr's snapshot")?;
    v["result"]["snapshot"]["focused_pane_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .context("herdr's snapshot has no focused_pane_id")
}

/// Whether any of `result.process_info.foreground_processes` from `herdr pane process-info` is named `nvim` (by
/// `name`, not `argv[0]`; any process in the group, not just the leader, so nvim under a wrapper counts).
fn runs_nvim(process_info_json: &str) -> anyhow::Result<bool> {
    let v: serde_json::Value =
        serde_json::from_str(process_info_json).context("parsing herdr's pane process info")?;
    let procs = v["result"]["process_info"]["foreground_processes"]
        .as_array()
        .context("herdr's pane process info has no foreground_processes")?;
    Ok(procs.iter().any(|p| p["name"].as_str() == Some("nvim")))
}

/// Sets the providers from what the focus read found. On an error it warns and goes by whatever was read: the
/// class alone still brings in alacritty when only the program inside it is unknown.
/// Returns the program inside the window, which plugins match on.
fn settle(
    ctx: &mut Context,
    wm: Option<&'static str>,
    focus: anyhow::Result<Option<Foreground>>,
) -> Option<String> {
    let child = focus.unwrap_or_else(|e| {
        eprintln!("keybind-find: focused window: {e:#}; showing what applies without it");
        None
    });
    let (comm, group) = child.map_or((None, Vec::new()), |f| (Some(f.comm), f.group));
    ctx.providers = providers_for(wm, ctx.class.as_deref(), comm.as_deref(), &group);
    comm
}

/// Known sessions and the names each is recognised by, lowercase: the window manager's EWMH name, an
/// `XDG_CURRENT_DESKTOP` entry, a `DESKTOP_SESSION` value or a process `comm`.
/// ponytail: names from the coverage survey, not checked against each live session; a missing one lets the lookup
/// go on to the next step, which usually ends in "no provider". Add the alias.
const SESSIONS: &[(&str, &[&str])] = &[
    ("xmonad", &["xmonad"]),
    ("i3", &["i3", "i3-with-shmlog"]),
    ("sway", &["sway"]),
    ("hyprland", &["hyprland"]),
    ("niri", &["niri"]),
    ("kde", &["kde", "kwin", "plasma", "kwin_x11", "plasmax11"]),
    (
        "gnome",
        &[
            "gnome",
            "gnome shell",
            "gnome-shell",
            "mutter",
            "ubuntu",
            "gnome-xorg",
        ],
    ),
    // X11 window managers with no provider yet, so they are named rather than unknown.
    ("xfce", &["xfce", "xfwm4"]),
    ("openbox", &["openbox"]),
    ("bspwm", &["bspwm"]),
    ("awesome", &["awesome"]),
    ("qtile", &["qtile"]),
    ("herbstluftwm", &["herbstluftwm"]),
    ("dwm", &["dwm"]),
];

/// Compositors that are never an X11 session's window manager, whatever a leftover variable says.
const WAYLAND_ONLY: &[&str] = &["sway", "hyprland", "niri"];

/// Sessions that can be Wayland: a leftover `XDG_CURRENT_DESKTOP=xmonad` beside `WAYLAND_DISPLAY` names none.
const WAYLAND: &[&str] = &["sway", "hyprland", "niri", "kde", "gnome"];

fn set(v: Option<String>) -> bool {
    v.is_some_and(|v| !v.is_empty())
}

/// The session `name` is an alias of, ignoring case.
fn known(name: &str) -> Option<&'static str> {
    let name = name.trim();
    SESSIONS
        .iter()
        .find(|(_, aliases)| aliases.iter().any(|a| a.eq_ignore_ascii_case(name)))
        .map(|(s, _)| *s)
}

/// The session a process `comm` belongs to: an alias exactly, or xmonad's compiled config. No general prefix rule:
/// `gnome-keyring-d` is no GNOME session and `xmonad-log` (a bar helper) no xmonad.
/// ponytail: a config built under another name (stack's `my-xmonad`) isn't found here; its EWMH name or
/// `DESKTOP_SESSION` still is.
fn known_comm(comm: &str) -> Option<&'static str> {
    if is_xmonad_config(comm) {
        return Some("xmonad");
    }
    known(comm)
}

/// xmonad's compiled config is `xmonad-<arch>-<os>`, its `comm` cut to 15 bytes: `xmonad-x86_64-l`, or with an
/// arch of 8 bytes or more no os at all (`xmonad-mips64el`). `xmonad-log-appl` (a panel applet) is not one.
fn is_xmonad_config(comm: &str) -> bool {
    let Some(rest) = comm.strip_prefix("xmonad-") else {
        return false;
    };
    match rest.rsplit_once('-') {
        // An empty os: the cut fell right after the arch's dash (`xmonad-aarch64-`).
        Some((arch, os)) => {
            !arch.is_empty()
                && (!os.is_empty() || rest.len() == 8)
                && ["linux", "freebsd", "openbsd", "netbsd", "darwin", "solaris"]
                    .iter()
                    .any(|o| o.starts_with(os))
        }
        None => rest.len() == 8,
    }
}

/// Which session this is (`None`: unknown), from `env` (an empty variable counts as unset), and on X11 also the EWMH
/// check window's name and the user's process names. Each source is a closure, asked only when reached.
fn session(
    env: impl Fn(&str) -> Option<String>,
    wm_name: impl FnOnce() -> Option<String>,
    comms: impl FnOnce() -> Vec<String>,
) -> Option<&'static str> {
    let env = |k: &str| env(k).filter(|v| !v.is_empty());
    let desktop = || {
        env("XDG_CURRENT_DESKTOP")
            .map(|d| d.split(':').map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    if env("WAYLAND_DISPLAY").is_some() {
        for (var, name) in [
            ("SWAYSOCK", "sway"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "hyprland"),
            ("NIRI_SOCKET", "niri"),
        ] {
            if env(var).is_some() {
                return Some(name);
            }
        }
        return desktop()
            .iter()
            .find_map(|d| known(d).filter(|s| WAYLAND.contains(s)));
    }
    let x11 = |s: Option<&'static str>| s.filter(|s| !WAYLAND_ONLY.contains(s));
    x11(wm_name().as_deref().and_then(known))
        .or_else(|| desktop().iter().find_map(|d| x11(known(d))))
        // Older display managers set a path (`/usr/share/xsessions/plasma`, maybe `.desktop`).
        .or_else(|| x11(env("DESKTOP_SESSION").and_then(|d| known(session_file_name(&d)))))
        .or_else(|| comms().iter().find_map(|c| x11(known_comm(c))))
}

/// `DESKTOP_SESSION`'s last path segment, without a `.desktop` suffix.
fn session_file_name(d: &str) -> &str {
    let name = d
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    name.strip_suffix(".desktop").unwrap_or(name)
}

/// The window-manager provider for a session: only sway, Hyprland, xmonad and KDE have one. Nothing falls back to
/// xmonad: a leftover `xmonad.keys` under i3 must not show.
fn wm_provider(session: Option<&'static str>) -> Option<&'static str> {
    session.filter(|s| matches!(*s, "sway" | "hyprland" | "xmonad" | "kde"))
}

/// The EWMH check window's `_NET_WM_NAME` (who the window manager says it is); `None` when anything is unreadable.
fn wm_check_name() -> Option<String> {
    let root = run("xprop", &["-root", "_NET_SUPPORTING_WM_CHECK"]).ok()?;
    let name = run("xprop", &["-id", check_window_id(&root)?, "_NET_WM_NAME"]).ok()?;
    quoted(&name).into_iter().next()
}

/// The window id in `_NET_SUPPORTING_WM_CHECK(WINDOW): window id # 0x200001`.
fn check_window_id(xprop: &str) -> Option<&str> {
    xprop.split_whitespace().last().filter(|id| {
        id.strip_prefix("0x")
            .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()))
    })
}

/// The `comm` of every process in the `/proc`-shaped directory `proc` owned by `uid`, by pid. Another user's window
/// manager on a shared machine must not name this session. Entries that aren't pids, or that vanish mid-read, are
/// skipped.
/// ponytail: any of the user's processes, not just this login's: a second X display of the same user's can name
/// this one. Only the last step reads it; scope it by `/proc/<pid>/sessionid` if that bites.
fn user_comms(proc: &std::path::Path, uid: u32) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(proc) else {
        return Vec::new();
    };
    let mut comms: Vec<(u32, String)> = entries
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<u32>().ok()?;
            if e.metadata().ok()?.uid() != uid {
                return None;
            }
            let comm = std::fs::read_to_string(e.path().join("comm")).ok()?;
            Some((pid, comm.trim_end_matches('\n').to_string()))
        })
        .collect();
    comms.sort();
    comms.into_iter().map(|(_, c)| c).collect()
}

/// Terminal window classes (X11 `WM_CLASS`, sway `app_id`) whose program inside is looked up, matched ignoring case.
/// ponytail: a fixed list; a terminal missing here just shows no in-terminal providers. Add its class.
const TERMINALS: &[&str] = &[
    "Alacritty",
    "kitty",
    "foot",
    "footclient",
    "org.wezfurlong.wezterm",
    // st's WM_CLASS is "st-256color", "St": the class (second) is read; the instance is for a one-string WM_CLASS.
    "st-256color",
    "St",
    "XTerm",
    "UXTerm",
    "com.mitchellh.ghostty",
    "Gnome-terminal",
    "org.gnome.Terminal",
    "konsole",
    "org.kde.konsole",
    "Xfce4-terminal",
    "URxvt",
    "Terminator",
];

fn is_terminal(class: &str) -> bool {
    TERMINALS.iter().any(|t| t.eq_ignore_ascii_case(class))
}

/// Terminals whose own keys a provider lists: window class (matched ignoring case) and provider.
const TERMINAL_KEYS: &[(&str, &str)] = &[
    ("Alacritty", "alacritty"),
    ("com.mitchellh.ghostty", "ghostty"),
];

/// `child` is the terminal's foreground program (its group's leader), `group` every program in that group.
fn providers_for(
    wm: Option<&'static str>,
    class: Option<&str>,
    child: Option<&str>,
    group: &[String],
) -> Vec<&'static str> {
    let mut providers: Vec<_> = wm.into_iter().chain(["espanso"]).collect();
    if class.is_some_and(is_terminal) {
        // The terminal's own keys.
        if let Some((_, p)) = TERMINAL_KEYS
            .iter()
            .find(|(c, _)| class.is_some_and(|class| class.eq_ignore_ascii_case(c)))
        {
            providers.push(p);
        }
        match child {
            Some("nvim") => providers.push("nvim"),
            Some("herdr") => providers.push("herdr"),
            // tmux renames its process: the client's comm is `tmux: client`.
            Some("tmux" | "tmux: client") => providers.push("tmux"),
            // An idle shell is the foreground program; a login shell's comm is still `bash`, not `-bash`.
            Some("bash") => providers.push("bash"),
            Some("zsh") => providers.push("zsh"),
            Some("fish") => providers.push("fish"),
            Some("vim" | "vi") => providers.push("vim"),
            Some("nano" | "rnano") => providers.push("nano"),
            // The client in the terminal; the server renames its argv (`SCREEN`), not its comm.
            Some("screen") => providers.push("screen"),
            _ => {}
        }
        // `man`, `git log` and `systemctl status` lead the pipeline that less runs in.
        if group.iter().any(|c| c == "less") {
            providers.push("less");
        }
    }
    // gvim is the window itself, as mpv is.
    if class.is_some_and(|c| c.eq_ignore_ascii_case("Gvim")) {
        providers.push("vim");
    }
    // mpv is the window itself, not a terminal's child: matched by class (`io.mpv.Mpv`: the Flatpak's app id).
    if class.is_some_and(|c| {
        ["mpv", "io.mpv.Mpv"]
            .iter()
            .any(|m| m.eq_ignore_ascii_case(c))
    }) {
        providers.push("mpv");
    }
    // Static sheets and custom parsers are matched by class inside the provider; with no class nothing can match,
    // and leaving it out keeps "every provider failed" reachable when the real providers all fail.
    if let Some(class) = class {
        // Vimium acts on the page, so a web app's sheet stays innermost (last).
        if crate::providers::is_browser(class) {
            providers.push("vimium");
        }
        providers.push("sheets");
    }
    providers
}

/// Fills id, class and title; returns the program running inside a terminal window.
fn read_focus(ctx: &mut Context) -> anyhow::Result<Option<Foreground>> {
    let id = run("xdotool", &["getactivewindow"])?.trim().to_string();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("xdotool printed {id:?}, not a window id");
    }
    read_window(ctx, id)
}

fn read_window(ctx: &mut Context, id: String) -> anyhow::Result<Option<Foreground>> {
    ctx.window_id = Some(id.clone());
    let props = parse_xprop(&run(
        "xprop",
        &[
            "-id",
            &id,
            "WM_CLASS",
            "_NET_WM_PID",
            "_NET_WM_NAME",
            "WM_NAME",
        ],
    )?);
    let child = program_in(&props, "_NET_WM_PID");
    ctx.class = props.class;
    ctx.instance = props.instance;
    ctx.title = props.title;
    child
}

/// The program inside a terminal window (`None` for any other window), found from the window's pid. `pid_source`
/// names where the pid should have come from, for the error when it is missing.
fn program_in(props: &Props, pid_source: &str) -> anyhow::Result<Option<Foreground>> {
    let Some(class) = props.class.as_deref().filter(|c| is_terminal(c)) else {
        return Ok(None);
    };
    let pid = props
        .pid
        .with_context(|| format!("{class} window has no {pid_source}"))?;
    foreground(pid).with_context(|| format!("finding the program inside {class}"))
}

/// Fills id, class and title from `swaymsg -t get_tree`'s output; returns the program running inside a terminal
/// window. An empty workspace has focus: no window, nothing to warn about.
fn read_sway_focus(ctx: &mut Context, tree: &str) -> anyhow::Result<Option<Foreground>> {
    let Some((id, props)) = parse_sway_tree(tree)? else {
        return Ok(None);
    };
    ctx.window_id = Some(id);
    let child = program_in(&props, "pid in sway's tree");
    ctx.class = props.class;
    ctx.instance = props.instance;
    ctx.title = props.title;
    child
}

/// The focused window in a `get_tree` dump: its `id`, and `app_id` (Xwayland: `window_properties.class`), `pid` and
/// `name` as class, pid and title. `None` when a workspace or output has focus (an empty workspace).
fn parse_sway_tree(json: &str) -> anyhow::Result<Option<(String, Props)>> {
    let tree: serde_json::Value = serde_json::from_str(json).context("parsing swaymsg's tree")?;
    let node = focused(&tree).context("no focused node in sway's tree")?;
    // A workspace, an output, or a split container (focus parent) has neither `app_id` nor `window_properties`.
    let window = node["app_id"].is_string() || node["window_properties"].is_object();
    if !matches!(node["type"].as_str(), Some("con" | "floating_con")) || !window {
        return Ok(None);
    }
    let id = node["id"].as_u64().context("focused node has no id")?;
    let text = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let props = Props {
        class: text(&node["app_id"]).or_else(|| text(&node["window_properties"]["class"])),
        instance: text(&node["window_properties"]["instance"]),
        pid: node["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
        title: text(&node["name"]),
    };
    Ok(Some((id.to_string(), props)))
}

/// Fills id, class and title from `hyprctl -j activewindow`; returns the program running inside a terminal window.
/// `{}` (no window has focus) is no window, nothing to warn about. The `xwayland` flag changes nothing: `class` is the
/// app id or the X class either way.
fn read_hyprland_focus(ctx: &mut Context, json: &str) -> anyhow::Result<Option<Foreground>> {
    let window: serde_json::Value =
        serde_json::from_str(json).context("parsing `hyprctl -j activewindow`")?;
    let text = |k: &str| {
        window[k]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let Some(id) = text("address") else {
        return Ok(None);
    };
    let props = Props {
        // An app that hasn't set its class yet still has the one it mapped with.
        class: text("class").or_else(|| text("initialClass")),
        instance: None,
        // An XWayland window without `_NET_WM_PID` has pid 0.
        pid: window["pid"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .filter(|&p| p > 0),
        title: text("title"),
    };
    ctx.window_id = Some(id);
    let child = program_in(&props, "pid from hyprctl");
    ctx.class = props.class;
    ctx.title = props.title;
    child
}

/// One `kdotool` run (it loads a KWin script per run) printing the active window's pid, class and caption.
const KDOTOOL_FOCUS: [&str; 4] = [
    "getactivewindow",
    "getwindowpid",
    "getwindowclassname",
    "getwindowname",
];

/// Fills class and title from `kdotool`'s lines (pid, class, then the caption, which may span lines); returns the
/// program running inside a terminal window. No output is no window, nothing to warn about. No window id: KWin's is
/// a UUID, not an X id, and only `xdotool` would use it.
fn read_kde_focus(ctx: &mut Context, out: &str) -> anyhow::Result<Option<Foreground>> {
    if out.trim().is_empty() {
        return Ok(None);
    }
    let mut lines = out.lines();
    let (Some(pid), Some(class)) = (lines.next(), lines.next()) else {
        bail!("kdotool printed {out:?}, not a pid and a class");
    };
    let title = lines.collect::<Vec<_>>().join("\n");
    let text = |s: &str| Some(s.to_string()).filter(|s| !s.is_empty());
    let props = Props {
        class: text(class.trim()),
        instance: None,
        pid: pid.trim().parse().ok().filter(|&p| p > 0),
        title: text(&title),
    };
    let child = program_in(&props, "pid from kdotool");
    ctx.class = props.class;
    ctx.title = props.title;
    child
}

fn focused(node: &serde_json::Value) -> Option<&serde_json::Value> {
    if node["focused"].as_bool() == Some(true) {
        return Some(node);
    }
    ["nodes", "floating_nodes"]
        .iter()
        .filter_map(|k| node[*k].as_array())
        .flatten()
        .find_map(focused)
}

/// Runs a short query (X, sway, Hyprland) with a 1 s limit, so a wedged display server can't hang the hotkey.
fn run(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let mut cmd = Command::new(program);
    // A UTF-8 locale keeps xprop from octal-escaping non-ASCII titles when launched with a bare environment.
    cmd.args(args).env("LC_ALL", "C.UTF-8");
    run_for(cmd, Duration::from_secs(1))
}

/// Most a program may print on either stream; more is cut off (a runaway program can't eat memory).
const OUTPUT_LIMIT: u64 = 8 << 20;

/// Runs `cmd` (no shell; stdin closed) and returns its stdout; non-zero exit is an error carrying its stderr. The
/// whole run, output included, must end within `timeout`. It runs in its own process group, killed whole on a
/// timeout or when the program exits leaving a child that holds its output open.
pub(crate) fn run_for(cmd: Command, timeout: Duration) -> anyhow::Result<String> {
    run_for_input(cmd, timeout, None)
}

/// `run_for` with `input` written to the program's stdin (then closed), from a thread: a program that never reads it
/// can't block us, and one that exits first just ends the write.
pub(crate) fn run_for_input(
    cmd: Command,
    timeout: Duration,
    input: Option<Vec<u8>>,
) -> anyhow::Result<String> {
    let name = std::path::Path::new(cmd.get_program())
        .display()
        .to_string();
    checked(&name, run_with(cmd, timeout, input, Group::Own, false)?)
}

/// `run_for` for an interactive shell: `group` says whether `cmd` is `setsid <shell>`, and when the shell exits
/// leaving a background job from an rc file holding its output open, that job is killed (if still in the group) and
/// what the shell printed is kept.
pub(crate) fn run_keeping_output(
    cmd: Command,
    timeout: Duration,
    group: Group,
) -> anyhow::Result<String> {
    let name = std::path::Path::new(cmd.get_program())
        .display()
        .to_string();
    checked(&name, run_with(cmd, timeout, None, group, true)?)
}

/// Stdout of a run that exited 0; otherwise an error with the reason it printed.
fn checked(
    name: &str,
    (status, stdout, stderr): (std::process::ExitStatus, Vec<u8>, Vec<u8>),
) -> anyhow::Result<String> {
    if !status.success() {
        // With a blank stderr the reason is on stdout, for every caller (herdr prints its errors as JSON there).
        let stderr = String::from_utf8_lossy(if stderr.iter().all(u8::is_ascii_whitespace) {
            &stdout
        } else {
            &stderr
        });
        let mut why = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        if let Some((cut, _)) = why.char_indices().nth(300) {
            why.truncate(cut);
            why.push('…');
        }
        bail!("{name} failed ({status}): {why}");
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// A picker frontend: `run_for_input` without its time limit (a person is choosing; ponytail: a day, not none) and
/// in keybind-find's own process group, so a frontend that draws on the terminal (fzf reading `/dev/tty`) isn't
/// stopped for reading from a background group. Its exit status, stdout and stderr: what a non-zero exit means is
/// the picker's to say.
pub(crate) fn run_frontend(
    cmd: Command,
    input: Vec<u8>,
) -> anyhow::Result<(std::process::ExitStatus, String, String)> {
    let (status, stdout, stderr) = run_with(
        cmd,
        Duration::from_secs(24 * 3600),
        Some(input),
        Group::Shared,
        false,
    )?;
    let text = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
    Ok((status, text(stdout), text(stderr)))
}

/// `run_for` for a program whose exit status alone says too little: vim exits 1 after any error in a vimrc, with its
/// output complete. Its exit status, stdout and stderr; the caller decides what a non-zero exit means (vim's
/// sentinels). A process it leaves holding the output open (a vimrc's background job) is killed and the output kept.
pub(crate) fn run_for_status(
    cmd: Command,
    timeout: Duration,
) -> anyhow::Result<(std::process::ExitStatus, String, String)> {
    let (status, stdout, stderr) = run_with(cmd, timeout, None, Group::Own, true)?;
    let text = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
    Ok((status, text(stdout), text(stderr)))
}

/// The process group `run_with` runs a program in, killed whole on a timeout unless `Shared`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Group {
    /// Its own.
    Own,
    /// `cmd` is `setsid <program>`, which leads a new session and group: started outside ours (not a group leader),
    /// setsid doesn't fork, so the group's id is its pid.
    Session,
    /// keybind-find's: only the program is killed.
    Shared,
}

/// The run behind `run_for_input` and `run_frontend`: exit status, stdout and stderr, each within `OUTPUT_LIMIT`.
/// `keep_output`: a process left holding the output open after the program exits is killed, and the output kept.
fn run_with(
    mut cmd: Command,
    timeout: Duration,
    input: Option<Vec<u8>>,
    group: Group,
    keep_output: bool,
) -> anyhow::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    let name = std::path::Path::new(cmd.get_program())
        .display()
        .to_string();
    if group == Group::Own {
        cmd.process_group(0);
    }
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {name}"))?;
    let kill_group = |child: &mut std::process::Child| {
        if group == Group::Shared {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        // std can only kill its own child, and a libc dependency isn't worth it: sh's `kill` builtin (procps' kill
        // isn't always installed) with a fixed script; the group id is an argument, never part of the script.
        // ponytail: a Ctrl-C to keybind-find in a terminal doesn't reach the group; it's run from a hotkey.
        // `-s KILL`, not `-KILL`: dash (Debian's sh) then reads `--` as the end of options.
        let _ = Command::new("/bin/sh")
            .args([
                "-c",
                "kill -s KILL -- \"-$1\"",
                "kill-group",
                &child.id().to_string(),
            ])
            .stderr(Stdio::null())
            .status();
        let _ = child.kill();
        let _ = child.wait();
    };
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(&input);
        });
    }
    // Read while waiting: sway's tree can outgrow the pipe buffer, and a writer blocked on a full pipe never exits.
    // Into shared buffers, so `keep_output` can take what was read from a pipe that never ends.
    let (tx, rx) = std::sync::mpsc::channel();
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        let (tx, buf) = (
            tx.clone(),
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        );
        let out = buf.clone();
        std::thread::spawn(move || {
            if let Some(p) = pipe {
                let (mut p, mut chunk) = (p.take(OUTPUT_LIMIT), [0; 8192]);
                while let Ok(n @ 1..) = p.read(&mut chunk) {
                    if let Ok(mut b) = buf.lock() {
                        b.extend_from_slice(&chunk[..n]);
                    }
                }
            }
            let _ = tx.send(());
        });
        out
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    // ponytail: polls every 5 ms.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_group(&mut child);
                return Err(e).with_context(|| format!("waiting for {name}"));
            }
        }
        if Instant::now() > deadline {
            kill_group(&mut child);
            bail!(
                "{name} did not answer within {:.1} s",
                timeout.as_secs_f32()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // Once it has exited, what it wrote is already in the pipes: a short grace, not the rest of the deadline, so a
    // helper it left holding the pipes costs 200 ms, not the whole limit.
    let grace = Instant::now() + Duration::from_millis(200);
    for _ in 0..2 {
        if rx
            .recv_timeout(grace.saturating_duration_since(Instant::now()))
            .is_err()
        {
            kill_group(&mut child);
            if !keep_output {
                bail!("{name} exited but left a process holding its output open");
            }
            break;
        }
    }
    let take = |b: &std::sync::Mutex<Vec<u8>>| {
        b.lock()
            .map(|mut b| std::mem::take(&mut *b))
            .unwrap_or_default()
    };
    let (stdout, stderr) = (take(&out), take(&err));
    if stdout.len() as u64 == OUTPUT_LIMIT || stderr.len() as u64 == OUTPUT_LIMIT {
        bail!("{name} printed more than {} MiB", OUTPUT_LIMIT >> 20);
    }
    Ok((status, stdout, stderr))
}

/// The program in a terminal, by `comm`.
#[derive(Debug, PartialEq)]
struct Foreground {
    /// The leader of the foreground process group: `man` under `man ls`.
    comm: String,
    /// Every process in that group: `man`, `less` and the rest of the pipeline.
    group: Vec<String>,
}

/// The program in the terminal's foreground: the leader of its child's foreground process group (`tpgid`), so
/// `nvim` typed at a zsh prompt counts as nvim, and an idle shell counts as the shell. `None` if there is no child.
fn foreground(pid: u32) -> anyhow::Result<Option<Foreground>> {
    // Children of every thread, not just the main one.
    let mut children = Vec::new();
    for task in std::fs::read_dir(format!("/proc/{pid}/task"))? {
        let path = task?.path().join("children");
        if let Ok(text) = std::fs::read_to_string(path) {
            children.extend(text.split_whitespace().map(str::to_string));
        }
    }
    // ponytail: one shell per terminal process. A terminal that serves several windows, tabs or splits from one
    // process (alacritty's daemon mode, `foot --server`, gnome-terminal-server, kitty's tabs) has several children,
    // and we can't tell which belongs to the focused one, so say so; the caller names the terminal.
    let child = match children.as_slice() {
        [] => return Ok(None),
        [child] => child,
        _ => bail!(
            "pid {pid} has {} children (one process serving several windows or tabs?)",
            children.len()
        ),
    };
    let stat = std::fs::read_to_string(format!("/proc/{child}/stat")).ok();
    let tpgid = stat.as_deref().and_then(tpgid);
    // Without a terminal (`tpgid` -1), the child's own group.
    let group = tpgid
        .or_else(|| stat.as_deref().and_then(|s| stat_field(s, 2)))
        .map(group_comms)
        .unwrap_or_default();
    // The group leader may have exited; fall back to the child itself. A child that exited between reads is no
    // program at all.
    for pid in tpgid.iter().map(u32::to_string).chain([child.clone()]) {
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
            let comm = comm.trim_end_matches('\n').to_string();
            if let (Ok(pid), "nvim") = (pid.parse(), comm.as_str()) {
                found_nvim(pid);
            }
            return Ok(Some(Foreground { comm, group }));
        }
    }
    Ok(None)
}

/// The `comm` of every process in process group `pgrp`, from `/proc/*/stat`. Debian's man runs less as `pager`
/// (an alternatives link), so a `pager` is named by the program it runs.
/// ponytail: reads every process's stat, a few ms; fine once per keypress.
fn group_comms(pgrp: u32) -> Vec<String> {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    procs
        .flatten()
        .filter_map(|p| {
            // Lossily: a comm that isn't UTF-8 is still in the group.
            let stat =
                String::from_utf8_lossy(&std::fs::read(p.path().join("stat")).ok()?).into_owned();
            (stat_field(&stat, 2)? == pgrp).then_some(())?;
            let comm = &stat[stat.find('(')? + 1..stat.rfind(')')?];
            if comm == "pager" {
                // Another user's `exe` (`sudo man`) can't be read; Debian's link then says where `pager` goes.
                let exe = std::fs::read_link(p.path().join("exe"))
                    .or_else(|_| std::fs::canonicalize("/usr/bin/pager"));
                if let Some(name) = exe.ok().as_deref().and_then(std::path::Path::file_name) {
                    // A binary replaced since it started (an upgrade) reads `less (deleted)`.
                    let name = name.to_string_lossy();
                    return Some(name.trim_end_matches(" (deleted)").to_string());
                }
            }
            Some(comm.to_string())
        })
        .collect()
}

/// Field 8 (`tpgid`) of `/proc/<pid>/stat`. `comm` (field 2) may hold spaces and `)`, so count from the last `)`.
/// `None` when the process has no controlling terminal (`-1`).
fn tpgid(stat: &str) -> Option<u32> {
    stat_field(stat, 5)
}

/// The `n`th field after `comm` in `/proc/<pid>/stat`, from 0: state ppid pgrp session tty_nr tpgid. `None` when it
/// isn't a number (`tpgid` -1).
fn stat_field(stat: &str, n: usize) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(n)?.parse().ok()
}

#[derive(Debug, Default, PartialEq)]
struct Props {
    class: Option<String>,
    /// X11 `WM_CLASS`'s first string (an XWayland window's under sway); `None` for a native Wayland window and under
    /// Hyprland.
    instance: Option<String>,
    pid: Option<u32>,
    title: Option<String>,
}

/// Parses `xprop -id <id> WM_CLASS _NET_WM_PID _NET_WM_NAME WM_NAME`. Missing properties print `NAME:  not found.`
/// or `NAME:  no such atom on any window.` and stay `None`. The title is `_NET_WM_NAME`, else the legacy `WM_NAME`
/// (xterm and other plain-Xlib clients set only that).
fn parse_xprop(text: &str) -> Props {
    let mut props = Props::default();
    let (mut net_name, mut wm_name) = (None, None);
    for line in text.lines() {
        let Some((name, value)) = line.split_once(" = ") else {
            continue;
        };
        match name.split('(').next() {
            // WM_CLASS is "instance", "class"; the class is the second. A malformed one string is taken as the class.
            Some("WM_CLASS") => {
                let mut strings = quoted(value);
                props.class = strings.pop();
                props.instance = strings.into_iter().next().filter(|i| !i.is_empty());
            }
            Some("_NET_WM_PID") => props.pid = value.trim().parse().ok(),
            Some("_NET_WM_NAME") => net_name = quoted(value).into_iter().next(),
            Some("WM_NAME") => wm_name = quoted(value).into_iter().next(),
            _ => {}
        }
    }
    props.title = net_name.or(wm_name);
    props
}

/// The double-quoted strings in an xprop value, with xprop's `\"` and `\\` escapes undone.
fn quoted(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = value.chars();
    while chars.by_ref().any(|c| c == '"') {
        let mut s = String::new();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => s.extend(chars.next()),
                c => s.push(c),
            }
        }
        out.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_table() {
        let base = vec!["xmonad", "espanso", "sheets"];
        let x = |c, p| providers_for(Some("xmonad"), c, p, &[]);
        assert_eq!(
            x(Some("Alacritty"), Some("nvim")),
            ["xmonad", "espanso", "alacritty", "nvim", "sheets"]
        );
        assert_eq!(
            x(Some("Alacritty"), Some("herdr")),
            ["xmonad", "espanso", "alacritty", "herdr", "sheets"]
        );
        assert_eq!(
            x(Some("Alacritty"), Some("tmux")),
            ["xmonad", "espanso", "alacritty", "tmux", "sheets"]
        );
        assert_eq!(
            x(Some("Alacritty"), Some("tmux: client")),
            ["xmonad", "espanso", "alacritty", "tmux", "sheets"]
        );
        assert_eq!(
            x(Some("Alacritty"), Some("zsh")),
            ["xmonad", "espanso", "alacritty", "zsh", "sheets"]
        );
        assert_eq!(
            providers_for(Some("sway"), Some("Alacritty"), Some("bash"), &[]),
            ["sway", "espanso", "alacritty", "bash", "sheets"]
        );
        assert_eq!(
            providers_for(Some("kde"), Some("org.kde.konsole"), Some("zsh"), &[]),
            ["kde", "espanso", "zsh", "sheets"]
        );
        // ghostty's own keys, in any case of its class, then the program inside it.
        for class in [
            "com.mitchellh.ghostty",
            "COM.MITCHELLH.GHOSTTY",
            "Com.Mitchellh.Ghostty",
        ] {
            assert_eq!(
                x(Some(class), Some("zsh")),
                ["xmonad", "espanso", "ghostty", "zsh", "sheets"]
            );
        }
        assert_eq!(
            x(Some("Alacritty"), None),
            ["xmonad", "espanso", "alacritty", "sheets"]
        );
        // Every row's provider is a known one, and its class a terminal.
        for (class, p) in TERMINAL_KEYS {
            assert!(crate::providers::KNOWN.contains(p), "{p}");
            assert!(is_terminal(class), "{class}");
        }
        assert_eq!(
            x(Some("kitty"), Some("fish")),
            ["xmonad", "espanso", "fish", "sheets"]
        );
        // Programs that use readline but aren't these shells, a shell in a browser or an unlisted class: none.
        assert_eq!(
            x(Some("Alacritty"), Some("python3")),
            ["xmonad", "espanso", "alacritty", "sheets"]
        );
        assert_eq!(
            x(Some("firefox"), Some("bash")),
            ["xmonad", "espanso", "vimium", "sheets"]
        );
        assert_eq!(x(Some("Some-terminal"), Some("zsh")), base);
        assert_eq!(
            x(Some("Alacritty"), None),
            ["xmonad", "espanso", "alacritty", "sheets"]
        );
        assert_eq!(x(Some("Gimp"), None), base);
        let browser = ["xmonad", "espanso", "vimium", "sheets"];
        assert_eq!(x(Some("Vivaldi"), None), browser);
        assert_eq!(x(Some("Vivaldi-stable"), Some("nvim")), browser);
        // No class: nothing for `sheets` to match.
        assert_eq!(x(None, None), ["xmonad", "espanso"]);
        // Under sway, sway replaces xmonad and nothing else changes.
        let sway = |c, p| providers_for(Some("sway"), c, p, &[]);
        assert_eq!(
            sway(Some("Alacritty"), Some("nvim")),
            ["sway", "espanso", "alacritty", "nvim", "sheets"]
        );
        assert_eq!(
            sway(Some("Alacritty"), Some("herdr")),
            ["sway", "espanso", "alacritty", "herdr", "sheets"]
        );
        assert_eq!(
            sway(Some("Alacritty"), Some("tmux: client")),
            ["sway", "espanso", "alacritty", "tmux", "sheets"]
        );
        assert_eq!(
            sway(Some("firefox"), Some("nvim")),
            ["sway", "espanso", "vimium", "sheets"]
        );
        assert_eq!(sway(None, None), ["sway", "espanso"]);
        // Hyprland takes sway's place; nothing else changes.
        let hypr = |c, p| providers_for(wm_provider(Some("hyprland")), c, p, &[]);
        assert_eq!(
            hypr(Some("kitty"), Some("nvim")),
            ["hyprland", "espanso", "nvim", "sheets"]
        );
        assert_eq!(
            hypr(Some("Alacritty"), Some("tmux: client")),
            ["hyprland", "espanso", "alacritty", "tmux", "sheets"]
        );
        assert_eq!(hypr(None, None), ["hyprland", "espanso"]);
        // No window-manager provider: the list starts with espanso.
        assert_eq!(
            providers_for(None, Some("Alacritty"), Some("nvim"), &[]),
            ["espanso", "alacritty", "nvim", "sheets"]
        );
        assert_eq!(providers_for(None, None, None, &[]), ["espanso"]);
    }

    #[test]
    fn less_anywhere_in_the_group_adds_less() {
        let group = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        let man = group(&["man", "less"]);
        // `man ls`: man leads the group; less comes after the terminal's own providers.
        assert_eq!(
            providers_for(Some("xmonad"), Some("Alacritty"), Some("man"), &man),
            ["xmonad", "espanso", "alacritty", "less", "sheets"]
        );
        // `git log` from zsh is git's group, not the shell's; a script piping into less shows both.
        assert_eq!(
            providers_for(
                Some("sway"),
                Some("kitty"),
                Some("git"),
                &group(&["git", "less"])
            ),
            ["sway", "espanso", "less", "sheets"]
        );
        assert_eq!(
            providers_for(None, Some("foot"), Some("bash"), &group(&["bash", "less"])),
            ["espanso", "bash", "less", "sheets"]
        );
        // The leader check is unchanged: nvim leads, less elsewhere doesn't hide it.
        assert_eq!(
            providers_for(None, Some("foot"), Some("nvim"), &group(&["nvim"])),
            ["espanso", "nvim", "sheets"]
        );
        // Not a terminal, or an unlisted class: no less.
        assert_eq!(
            providers_for(None, Some("Gimp"), Some("man"), &man),
            ["espanso", "sheets"]
        );
        assert_eq!(
            providers_for(None, Some("Some-terminal"), Some("man"), &man),
            ["espanso", "sheets"]
        );
    }

    #[test]
    fn vim_in_a_terminal_or_gvim() {
        for wm in ["xmonad", "sway"] {
            for child in ["vim", "vi"] {
                assert_eq!(
                    providers_for(Some(wm), Some("Alacritty"), Some(child), &[]),
                    [wm, "espanso", "alacritty", "vim", "sheets"],
                    "{child}"
                );
            }
            for class in ["Gvim", "gvim"] {
                assert_eq!(
                    providers_for(Some(wm), Some(class), None, &[]),
                    [wm, "espanso", "vim", "sheets"],
                    "{class}"
                );
            }
            // nvim and a shell don't add it; nor does vim outside a terminal.
            for (class, child) in [
                ("Alacritty", Some("nvim")),
                ("kitty", Some("zsh")),
                ("Gimp", Some("vim")),
            ] {
                assert!(
                    !providers_for(Some(wm), Some(class), child, &[]).contains(&"vim"),
                    "{class} {child:?}"
                );
            }
        }
    }

    #[test]
    fn mpv_by_window_class() {
        for wm in ["xmonad", "sway"] {
            for class in ["mpv", "MPV", "io.mpv.Mpv"] {
                assert_eq!(
                    providers_for(Some(wm), Some(class), None, &[]),
                    [wm, "espanso", "mpv", "sheets"],
                    "{class}"
                );
            }
        }
        // mpv inside a terminal (`--vo=tct`) is a terminal's child, not an mpv window; nor are look-alike classes.
        for (class, child) in [
            ("Alacritty", Some("mpv")),
            ("mpvtube", None),
            ("mpv-something", None),
        ] {
            for wm in ["xmonad", "sway"] {
                assert!(
                    !providers_for(Some(wm), Some(class), child, &[]).contains(&"mpv"),
                    "{class} under {wm}"
                );
            }
        }
    }

    #[test]
    fn every_terminal_gets_the_programs_inside() {
        for class in TERMINALS.iter().copied().chain([
            "KITTY",
            "xterm",
            "Foot",
            "alacritty",
            "COM.MITCHELLH.GHOSTTY",
        ]) {
            let own: Vec<&str> = TERMINAL_KEYS
                .iter()
                .filter(|(c, _)| c.eq_ignore_ascii_case(class))
                .map(|(_, p)| *p)
                .collect();
            for (child, inside) in [
                (Some("nvim"), vec!["nvim"]),
                (Some("herdr"), vec!["herdr"]),
                (Some("tmux: client"), vec!["tmux"]),
                (Some("zsh"), vec!["zsh"]),
                (Some("bash"), vec!["bash"]),
                (Some("fish"), vec!["fish"]),
                (Some("vim"), vec!["vim"]),
                (Some("vi"), vec!["vim"]),
                (Some("nano"), vec!["nano"]),
                (Some("rnano"), vec!["nano"]),
                (Some("screen"), vec!["screen"]),
                (Some("python3"), vec![]),
                (None, vec![]),
            ] {
                for wm in [Some("xmonad"), Some("sway"), None] {
                    let want: Vec<&str> = [
                        wm.into_iter().chain(["espanso"]).collect(),
                        own.clone(),
                        inside.clone(),
                        vec!["sheets"],
                    ]
                    .concat();
                    assert_eq!(
                        providers_for(wm, Some(class), child, &[]),
                        want,
                        "{wm:?} {class} {child:?}"
                    );
                }
            }
        }
        // alacritty's own provider stays alacritty's; an unknown class gets nothing from inside it.
        assert_eq!(
            providers_for(Some("xmonad"), Some("kitty"), Some("nvim"), &[]),
            ["xmonad", "espanso", "nvim", "sheets"]
        );
        assert_eq!(
            providers_for(Some("xmonad"), Some("Some-terminal"), Some("nvim"), &[]),
            ["xmonad", "espanso", "sheets"]
        );
    }

    const SNAPSHOT: &str = r#"{"id": "1", "type": "response", "result": {"type": "snapshot", "snapshot": {
        "focused_pane_id": "w1:p2", "focused_tab_id": "w1:t1", "focused_workspace_id": "w1",
        "panes": [{"pane_id": "w1:p2", "focused": true, "agent": null, "terminal_title": "t", "cwd": "/x"}],
        "tabs": [], "workspaces": [], "layouts": [], "agents": [], "protocol": 1, "version": "0.9.3"}}}"#;

    fn process_info(processes: &str) -> String {
        format!(
            r#"{{"id": "1", "type": "response", "result": {{"type": "process_info", "process_info": {{
            "pane_id": "w1:p2", "shell_pid": 10, "foreground_process_group_id": 20,
            "foreground_processes": [{processes}]}}}}}}"#
        )
    }

    fn proc(pid: u32, name: &str, argv0: &str) -> String {
        format!(
            r#"{{"pid": {pid}, "name": "{name}", "argv": ["{argv0}"], "cmdline": "{argv0}", "cwd": "/x"}}"#
        )
    }

    #[test]
    fn herdr_pane_parsers() {
        assert_eq!(focused_pane_id(SNAPSHOT).unwrap(), "w1:p2");
        let nvim = proc(20, "nvim", "nvim");
        assert!(runs_nvim(&process_info(&nvim)).unwrap());
        assert!(!runs_nvim(&process_info(&proc(20, "zsh", "-zsh"))).unwrap());
        assert!(!runs_nvim(&process_info("")).unwrap());
        // nvim as a non-leader under a wrapper, and `name` winning over `argv[0]`.
        let wrapped = format!(
            "{}, {}",
            proc(20, "direnv", "direnv"),
            proc(21, "nvim", "/usr/bin/vi")
        );
        assert!(runs_nvim(&process_info(&wrapped)).unwrap());
        assert!(!runs_nvim(&process_info(&proc(20, "nvim-qt", "nvim"))).unwrap());
    }

    #[test]
    fn herdr_pane_parsers_reject_bad_json() {
        let e = focused_pane_id(r#"{"result": {"snapshot": {"panes": []}}}"#).unwrap_err();
        assert!(e.to_string().contains("no focused_pane_id"), "{e}");
        assert!(focused_pane_id(r#"{"result": {"snapshot": {"focused_pane_id": ""}}}"#).is_err());
        assert!(focused_pane_id("not json").is_err());
        assert!(runs_nvim("not json").is_err());
        assert!(runs_nvim(r#"{"result": {"process_info": {}}}"#).is_err());
    }

    #[test]
    fn nvim_is_added_only_in_a_herdr_context() {
        let herdr = || providers_for(Some("xmonad"), Some("Alacritty"), Some("herdr"), &[]);
        let mut p = herdr();
        add_nvim_in(&mut p, "herdr", || Ok(true));
        assert_eq!(
            p,
            ["xmonad", "espanso", "alacritty", "herdr", "nvim", "sheets"]
        );
        let mut p = herdr();
        add_nvim_in(&mut p, "herdr", || Ok(false));
        assert_eq!(p, herdr());
        // herdr can't be queried: the plain herdr context.
        let mut p = herdr();
        add_nvim_in(&mut p, "herdr", || anyhow::bail!("running herdr"));
        assert_eq!(p, herdr());
        // Any other context never queries herdr.
        for child in [Some("zsh"), Some("nvim"), Some("tmux"), None] {
            let mut p = providers_for(Some("xmonad"), Some("Alacritty"), child, &[]);
            let before = p.clone();
            add_nvim_in(&mut p, "herdr", || panic!("herdr queried for {child:?}"));
            assert_eq!(p, before);
        }
    }

    #[test]
    fn nvim_pid_from_herdr_and_the_newest_tmux_client() {
        let wrapped = format!(
            "{}, {}",
            proc(20, "direnv", "direnv"),
            proc(21, "nvim", "nvim")
        );
        assert_eq!(nvim_pid_in(&process_info(&wrapped)), Some(21));
        assert_eq!(nvim_pid_in(&process_info(&proc(20, "zsh", "-zsh"))), None);
        assert_eq!(nvim_pid_in("not json"), None);
        assert_eq!(newest_client("100 111\n200 222\n").unwrap(), Some("222"));
        assert_eq!(newest_client("").unwrap(), None);
    }

    #[test]
    fn tmux_newest_client_parser() {
        assert!(newest_client_runs_nvim("1792873778 nvim\n").unwrap());
        assert!(!newest_client_runs_nvim("1792873778 zsh\n").unwrap());
        // nvim only in an older client.
        assert!(!newest_client_runs_nvim("100 nvim\n200 zsh\n").unwrap());
        assert!(newest_client_runs_nvim("200 nvim\n100 zsh\n").unwrap());
        // No clients.
        assert!(!newest_client_runs_nvim("").unwrap());
        // Activity is in whole seconds: on a tie, the client listed last wins.
        assert!(newest_client_runs_nvim("100 zsh\n100 nvim\n").unwrap());
        assert!(!newest_client_runs_nvim("100 nvim\n100 zsh\n").unwrap());
        // Malformed lines are an error, not a guess.
        for bad in ["nvim\n", "soon nvim\n", "100\n"] {
            assert!(newest_client_runs_nvim(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn nvim_is_added_only_in_a_tmux_context() {
        let tmux = || providers_for(Some("xmonad"), Some("Alacritty"), Some("tmux: client"), &[]);
        let mut p = tmux();
        add_nvim_in(&mut p, "tmux", || Ok(true));
        assert_eq!(
            p,
            ["xmonad", "espanso", "alacritty", "tmux", "nvim", "sheets"]
        );
        let mut p = tmux();
        add_nvim_in(&mut p, "tmux", || Ok(false));
        assert_eq!(p, tmux());
        let mut p = tmux();
        add_nvim_in(&mut p, "tmux", || anyhow::bail!("running tmux"));
        assert_eq!(p, tmux());
        for child in [Some("zsh"), Some("nvim"), Some("herdr"), None] {
            let mut p = providers_for(Some("xmonad"), Some("Alacritty"), child, &[]);
            let before = p.clone();
            add_nvim_in(&mut p, "tmux", || panic!("tmux queried for {child:?}"));
            assert_eq!(p, before);
        }
    }

    #[test]
    fn screen_sessions_from_ls() {
        let ls = |lines: &[&str]| {
            format!(
                "There are screens on:\n{}\n2 Sockets in /run/screen/S-u.\n",
                lines.join("\n")
            )
        };
        assert_eq!(
            attached_session(&ls(&["\t100.pts-1.host\t(Attached)"])).unwrap(),
            "100.pts-1.host"
        );
        assert_eq!(
            attached_session(&ls(&["\t100.a\t(Detached)", "\t200.b\t(Attached)"])).unwrap(),
            "200.b"
        );
        // Some builds print a date before the state.
        assert_eq!(
            attached_session(&ls(&["\t300.c\t(10/27/2026 10:00:00 AM)\t(Attached)"])).unwrap(),
            "300.c"
        );
        assert_eq!(
            attached_session(&ls(&["\t400.d\t(Multi, attached)"])).unwrap(),
            "400.d"
        );
        let two = ls(&["\t1.a\t(Attached)", "\t2.b\t(Attached)"]);
        assert!(format!("{:#}", attached_session(&two).unwrap_err()).contains("2 attached"));
        assert!(attached_session(&ls(&["\t1.a\t(Detached)"])).is_err());
        assert!(attached_session("No Sockets found in /run/screen/S-u.\n").is_err());
        // A name with a space runs to the tab; `Multi, detached` isn't attached.
        assert_eq!(
            attached_session(&ls(&[
                "\t500.my work\t(Attached)",
                "\t6.x\t(Multi, detached)"
            ]))
            .unwrap(),
            "500.my work"
        );
    }

    #[test]
    fn screen_window_program_from_proc() {
        let dir = std::env::temp_dir().join(format!("kbf-screen-proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let write = |path: &str, text: &[u8]| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        // The server (100) and two windows: 0 runs bash, 3 a zsh whose foreground group is led by nvim (310).
        write("100/task/100/children", b"200 300 ");
        write("200/environ", b"HOME=/h\0WINDOW=0\0");
        write("200/stat", b"200 (bash) S 100 200 200 34816 200 0");
        write("200/comm", b"bash\n");
        write("300/environ", b"WINDOW=3\0TERM=screen\0");
        write("300/stat", b"300 (zsh) S 100 300 300 34817 310 0");
        write("310/comm", b"nvim\n");
        let calls = std::cell::RefCell::new(Vec::new());
        let window = std::cell::Cell::new("3 (zsh)");
        let fake = |program: &str, args: &[&str]| {
            calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            Ok(match args[0] {
                "-ls" => "There is a screen on:\n\t100.pts-1.host\t(Attached)\n".to_string(),
                _ => window.get().to_string(),
            })
        };
        // Through `add_nested_nvim`: nvim right after screen.
        let screen = || providers_for(Some("xmonad"), Some("Alacritty"), Some("screen"), &[]);
        let mut p = screen();
        add_nested_nvim(&mut p, fake, &dir);
        assert_eq!(
            p,
            ["xmonad", "espanso", "alacritty", "screen", "nvim", "sheets"]
        );
        calls.borrow_mut().clear();
        let nvim = screen_window_program(fake, &dir);
        window.set("0 (bash)");
        let bash = screen_window_program(fake, &dir);
        let mut p = screen();
        add_nested_nvim(&mut p, fake, &dir);
        assert_eq!(p, screen());
        // A window without a readable stat: its own process is the program.
        write("400/environ", b"WINDOW=4\0");
        write("400/comm", b"htop\n");
        write("100/task/100/children", b"200 300 400");
        window.set("4 (htop)");
        let no_stat = screen_window_program(fake, &dir);
        window.set("7 (gone)");
        let gone = screen_window_program(fake, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(nvim.unwrap(), (310, "nvim".to_string()));
        assert_eq!(bash.unwrap(), (200, "bash".to_string()));
        assert_eq!(no_stat.unwrap(), (400, "htop".to_string()));
        assert!(format!("{:#}", gone.unwrap_err()).contains("screen window 7"));
        assert_eq!(
            calls.borrow()[..2],
            ["screen -ls", "screen -S 100.pts-1.host -Q number"]
        );
    }

    #[test]
    fn nvim_is_added_only_in_a_screen_context() {
        let screen = || providers_for(Some("xmonad"), Some("Alacritty"), Some("screen"), &[]);
        assert_eq!(
            screen(),
            ["xmonad", "espanso", "alacritty", "screen", "sheets"]
        );
        let mut p = screen();
        add_nvim_in(&mut p, "screen", || Ok(true));
        assert_eq!(
            p,
            ["xmonad", "espanso", "alacritty", "screen", "nvim", "sheets"]
        );
        // A failed query (no session here) keeps the plain context, and only a screen context asks.
        let calls = std::cell::RefCell::new(Vec::new());
        let fake = |program: &str, args: &[&str]| {
            calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            anyhow::bail!("No Sockets found")
        };
        let mut p = screen();
        add_nested_nvim(&mut p, fake, std::path::Path::new("/nonexistent"));
        assert_eq!(p, screen());
        assert_eq!(*calls.borrow(), ["screen -ls"]);
        calls.borrow_mut().clear();
        let mut p = providers_for(Some("xmonad"), Some("Alacritty"), Some("zsh"), &[]);
        add_nested_nvim(&mut p, fake, std::path::Path::new("/nonexistent"));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn each_multiplexer_is_asked_only_in_its_own_context() {
        let calls = std::cell::RefCell::new(Vec::new());
        let fake = |program: &str, args: &[&str]| {
            calls.borrow_mut().push(program.to_string());
            Ok(match (program, args[0]) {
                ("tmux", _) => "5 nvim\n".to_string(),
                (_, "api") => SNAPSHOT.to_string(),
                _ => process_info(&proc(20, "nvim", "nvim")),
            })
        };
        let mut p = providers_for(Some("xmonad"), Some("Alacritty"), Some("tmux: client"), &[]);
        add_nested_nvim(&mut p, fake, std::path::Path::new("/nonexistent"));
        assert_eq!(
            p,
            ["xmonad", "espanso", "alacritty", "tmux", "nvim", "sheets"]
        );
        // The second call asks for the pane's pid, for the nvim in it.
        assert_eq!(*calls.borrow(), ["tmux", "tmux"]);
        calls.borrow_mut().clear();
        let mut p = providers_for(Some("xmonad"), Some("Alacritty"), Some("herdr"), &[]);
        add_nested_nvim(&mut p, fake, std::path::Path::new("/nonexistent"));
        assert_eq!(
            p,
            ["xmonad", "espanso", "alacritty", "herdr", "nvim", "sheets"]
        );
        assert_eq!(*calls.borrow(), ["herdr", "herdr"]);
        calls.borrow_mut().clear();
        let mut p = providers_for(Some("xmonad"), Some("Alacritty"), Some("zsh"), &[]);
        add_nested_nvim(&mut p, fake, std::path::Path::new("/nonexistent"));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn tmux_is_asked_about_its_clients() {
        let calls = std::cell::RefCell::new(Vec::new());
        let fake = |program: &str, args: &[&str]| {
            calls
                .borrow_mut()
                .push(format!("{program} {}", args.join("|")));
            Ok("5 nvim\n".to_string())
        };
        assert!(tmux_pane_runs_nvim(fake).unwrap());
        // nvim in the pane: its pid is asked for too.
        assert_eq!(
            *calls.borrow(),
            [
                "tmux list-clients|-F|#{client_activity} #{pane_current_command}",
                "tmux list-clients|-F|#{client_activity} #{pane_pid}"
            ]
        );
        let failing =
            |_: &str, _: &[&str]| anyhow::bail!("tmux failed (exit status: 1): no server running");
        assert!(tmux_pane_runs_nvim(failing).is_err());
    }

    #[test]
    fn herdr_is_asked_about_its_focused_pane() {
        let calls = std::cell::RefCell::new(Vec::new());
        let fake = |program: &str, args: &[&str]| {
            calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            Ok(match args[0] {
                "api" => SNAPSHOT.to_string(),
                _ => process_info(&proc(20, "nvim", "nvim")),
            })
        };
        assert!(herdr_pane_runs_nvim(fake).unwrap());
        assert_eq!(
            *calls.borrow(),
            ["herdr api snapshot", "herdr pane process-info --pane w1:p2"]
        );
        // A failed snapshot stops before the second call.
        calls.borrow_mut().clear();
        let failing = |program: &str, args: &[&str]| {
            calls
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            anyhow::bail!("herdr failed (exit status: 1): server_not_running")
        };
        assert!(herdr_pane_runs_nvim(failing).is_err());
        assert_eq!(calls.borrow().len(), 1);
    }

    fn empty_context() -> Context {
        Context {
            window_id: None,
            class: None,
            instance: None,
            title: None,
            providers: providers_for(Some("sway"), None, None, &[]),
        }
    }

    #[test]
    fn sway_focus_fills_the_context() {
        let mut ctx = empty_context();
        let focus = read_sway_focus(&mut ctx, &tree("xwayland"));
        settle(&mut ctx, Some("sway"), focus);
        assert_eq!(ctx.window_id.as_deref(), Some("9"));
        assert_eq!(ctx.class.as_deref(), Some("Gimp"));
        assert_eq!(ctx.instance.as_deref(), Some("gimp"));
        assert_eq!(ctx.title.as_deref(), Some("GIMP"));
        assert_eq!(ctx.providers, ["sway", "espanso", "sheets"]);
        // An empty workspace: nothing filled in, no error.
        let mut ctx = empty_context();
        assert_eq!(read_sway_focus(&mut ctx, &tree("ws")).unwrap(), None);
        assert_eq!(ctx.window_id, None);
        // A failed read keeps the global providers.
        let mut ctx = empty_context();
        let focus = read_sway_focus(&mut ctx, "not json");
        settle(&mut ctx, Some("sway"), focus);
        assert_eq!(ctx.providers, ["sway", "espanso"]);
        // An alacritty whose inner program can't be found still gets the alacritty provider.
        let mut ctx = empty_context();
        ctx.class = Some("Alacritty".into());
        settle(&mut ctx, Some("sway"), Err(anyhow::anyhow!("no pid")));
        assert_eq!(ctx.providers, ["sway", "espanso", "alacritty", "sheets"]);
        // Any terminal app_id is looked inside; with no pid the warning names it and the list goes on without it.
        let json = r#"{"id": 1, "type": "root", "nodes": [{"id": 5, "type": "con", "app_id": "foot",
            "name": "t", "focused": true}]}"#;
        let mut ctx = empty_context();
        let focus = read_sway_focus(&mut ctx, json);
        assert_eq!(
            format!("{:#}", focus.as_ref().unwrap_err()),
            "foot window has no pid in sway's tree"
        );
        settle(&mut ctx, Some("sway"), focus);
        assert_eq!(ctx.class.as_deref(), Some("foot"));
        assert_eq!(ctx.providers, ["sway", "espanso", "sheets"]);
    }

    fn hypr_window(class: &str, pid: u32, xwayland: bool) -> String {
        format!(
            r#"{{"address": "0x55d1c3a0b2c0", "mapped": true, "class": "{class}", "title": "a title",
                "initialClass": "{class}", "pid": {pid}, "xwayland": {xwayland}, "workspace": {{"id": 1}}}}"#
        )
    }

    #[test]
    fn hyprland_focus_fills_the_context() {
        let hypr = |ctx: &mut Context| {
            ctx.providers = providers_for(Some("hyprland"), None, None, &[]);
        };
        // A native Wayland terminal: the program inside comes from its pid. `cat` ends when `sh` is gone (its stdin
        // closes), so nothing outlives the test.
        let mut sh = KillOnDrop(
            Command::new("sh")
                .args(["-c", "cat; :"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ctx = empty_context();
        hypr(&mut ctx);
        let found = loop {
            let found =
                read_hyprland_focus(&mut ctx, &hypr_window("kitty", sh.0.id(), false)).unwrap();
            if found.is_some() || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(found.map(|f| f.comm).as_deref(), Some("cat"));
        assert_eq!(ctx.window_id.as_deref(), Some("0x55d1c3a0b2c0"));
        assert_eq!(ctx.class.as_deref(), Some("kitty"));
        assert_eq!(ctx.title.as_deref(), Some("a title"));
        let _ = sh.0.kill();
        drop(sh.0.stdin.take());
        // An XWayland window gives the same fields.
        let mut ctx = empty_context();
        let focus = read_hyprland_focus(&mut ctx, &hypr_window("Gimp", 1, true));
        settle(&mut ctx, Some("hyprland"), focus);
        assert_eq!(ctx.class.as_deref(), Some("Gimp"));
        assert_eq!(ctx.title.as_deref(), Some("a title"));
        assert_eq!(ctx.providers, ["hyprland", "espanso", "sheets"]);
        // No window: nothing filled in, no error.
        let mut ctx = empty_context();
        let focus = read_hyprland_focus(&mut ctx, "{}");
        assert!(matches!(focus, Ok(None)));
        settle(&mut ctx, Some("hyprland"), focus);
        assert_eq!((ctx.window_id, ctx.class, ctx.title), (None, None, None));
        assert_eq!(ctx.providers, ["hyprland", "espanso"]);
        // No class yet: the initial class; pid 0 (XWayland without `_NET_WM_PID`) is no pid.
        let json =
            r#"{"address": "0x1", "class": "", "initialClass": "foot", "title": "t", "pid": 0}"#;
        let mut ctx = empty_context();
        let focus = read_hyprland_focus(&mut ctx, json);
        assert_eq!(
            format!("{:#}", focus.unwrap_err()),
            "foot window has no pid from hyprctl"
        );
        assert_eq!(ctx.class.as_deref(), Some("foot"));
        // Bad JSON: one warning (from `settle`), and the list without the window.
        let mut ctx = empty_context();
        let focus = read_hyprland_focus(&mut ctx, "not json");
        assert!(focus.is_err());
        settle(&mut ctx, Some("hyprland"), focus);
        assert_eq!(ctx.providers, ["hyprland", "espanso"]);
    }

    #[test]
    fn hyprland_never_falls_back_to_x() {
        let asked = std::cell::RefCell::new(Vec::new());
        let (ctx, program) = focus(
            Some("hyprland"),
            true,
            |_| panic!("X read under Hyprland"),
            |p, args| {
                asked.borrow_mut().push(format!("{p} {}", args.join(" ")));
                Err(anyhow::anyhow!("HYPRLAND_INSTANCE_SIGNATURE not set"))
            },
        );
        assert_eq!(*asked.borrow(), ["hyprctl -j activewindow"]);
        assert_eq!(program, None);
        assert_eq!((ctx.window_id, ctx.class), (None, None));
        assert_eq!(ctx.providers, ["hyprland", "espanso"]);
        // A working hyprctl fills the window in.
        let (ctx, _) = focus(
            Some("hyprland"),
            true,
            |_| panic!("X read under Hyprland"),
            |_, _| Ok(hypr_window("org.gnome.Nautilus", 1, false)),
        );
        assert_eq!(ctx.class.as_deref(), Some("org.gnome.Nautilus"));
        assert_eq!(ctx.providers, ["hyprland", "espanso", "sheets"]);
    }

    #[test]
    fn kde_focus_fills_the_context() {
        let kde = |ctx: &mut Context| {
            ctx.providers = providers_for(Some("kde"), None, None, &[]);
        };
        // A konsole: the program inside comes from its pid (a stand-in shell here, as for Hyprland).
        let mut sh = KillOnDrop(
            Command::new("sh")
                .args(["-c", "cat; :"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let out = format!("{}\norg.kde.konsole\nTitle\n", sh.0.id());
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ctx = empty_context();
        kde(&mut ctx);
        let found = loop {
            let found = read_kde_focus(&mut ctx, &out).unwrap();
            if found.is_some() || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(found.map(|f| f.comm).as_deref(), Some("cat"));
        assert_eq!(ctx.window_id, None);
        assert_eq!(ctx.class.as_deref(), Some("org.kde.konsole"));
        assert_eq!(ctx.title.as_deref(), Some("Title"));
        let _ = sh.0.kill();
        drop(sh.0.stdin.take());
        // A caption with a newline keeps both lines; the class reaches the providers.
        let mut ctx = empty_context();
        let focus = read_kde_focus(&mut ctx, "42\nGimp\nline one\nline two\n");
        settle(&mut ctx, Some("kde"), focus);
        assert_eq!(ctx.title.as_deref(), Some("line one\nline two"));
        assert_eq!(ctx.class.as_deref(), Some("Gimp"));
        assert_eq!(ctx.providers, ["kde", "espanso", "sheets"]);
        // No window: nothing filled in, no error.
        let mut ctx = empty_context();
        let focus = read_kde_focus(&mut ctx, "");
        assert!(matches!(focus, Ok(None)));
        settle(&mut ctx, Some("kde"), focus);
        assert_eq!((ctx.class, ctx.title), (None, None));
        assert_eq!(ctx.providers, ["kde", "espanso"]);
        // Half an answer is an error (one warning from `settle`); a terminal without a pid names it.
        let mut ctx = empty_context();
        assert!(read_kde_focus(&mut ctx, "42\n").is_err());
        let mut ctx = empty_context();
        let focus = read_kde_focus(&mut ctx, "null\nfoot\nt\n");
        assert_eq!(
            format!("{:#}", focus.unwrap_err()),
            "foot window has no pid from kdotool"
        );
    }

    #[test]
    fn kde_wayland_asks_kdotool_never_x() {
        let asked = std::cell::RefCell::new(Vec::new());
        let (ctx, program) = focus(
            Some("kde"),
            true,
            |_| panic!("X read under KDE Wayland"),
            |p, args| {
                asked.borrow_mut().push(format!("{p} {}", args.join(" ")));
                Err(anyhow::anyhow!("running kdotool (is it installed?)"))
            },
        );
        assert_eq!(
            *asked.borrow(),
            ["kdotool getactivewindow getwindowpid getwindowclassname getwindowname"]
        );
        assert_eq!(program, None);
        assert_eq!((ctx.window_id, ctx.class), (None, None));
        assert_eq!(ctx.providers, ["kde", "espanso"]);
        // A working kdotool fills the window in.
        let (ctx, _) = focus(
            Some("kde"),
            true,
            |_| panic!("X read under KDE Wayland"),
            |_, _| Ok("7\norg.kde.dolphin\nHome\n".into()),
        );
        assert_eq!(ctx.class.as_deref(), Some("org.kde.dolphin"));
        assert_eq!(ctx.providers, ["kde", "espanso", "sheets"]);
        // Plasma on X11 keeps the X path.
        let (ctx, _) = focus(
            Some("kde"),
            false,
            |ctx| {
                ctx.window_id = Some("5".into());
                ctx.class = Some("Gimp".into());
                Ok(None)
            },
            |p, _| panic!("{p} asked on X11"),
        );
        assert_eq!(ctx.window_id.as_deref(), Some("5"));
        assert_eq!(ctx.providers, ["kde", "espanso", "sheets"]);
    }

    #[test]
    fn the_foreground_group_reaches_the_providers() {
        let mut ctx = empty_context();
        ctx.class = Some("Alacritty".into());
        let man = Foreground {
            comm: "man".into(),
            group: vec!["man".into(), "less".into()],
        };
        // Plugins match on the leader; the group brings in less.
        assert_eq!(
            settle(&mut ctx, Some("sway"), Ok(Some(man))).as_deref(),
            Some("man")
        );
        assert_eq!(
            ctx.providers,
            ["sway", "espanso", "alacritty", "less", "sheets"]
        );
    }

    #[test]
    fn sway_split_container_and_empty_app_id() {
        // A focused split container (`focus parent`) is no window.
        let json = r#"{"id": 1, "type": "root", "nodes": [{"id": 5, "type": "con", "name": null, "focused": true,
            "nodes": [{"id": 6, "type": "con", "app_id": "foot", "focused": false}]}]}"#;
        assert_eq!(parse_sway_tree(json).unwrap(), None);
        // An empty app_id falls back to the X class.
        let json = r#"{"id": 1, "type": "root", "nodes": [{"id": 5, "type": "con", "app_id": "", "pid": 7,
            "window_properties": {"class": "Steam"}, "name": "Steam", "focused": true}]}"#;
        let (_, p) = parse_sway_tree(json).unwrap().unwrap();
        assert_eq!(p.class.as_deref(), Some("Steam"));
    }

    /// An environment of `vars` (`NAME=value`).
    fn env<'a>(vars: &'a [&'a str]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find_map(|v| v.strip_prefix(k)?.strip_prefix('='))
                .map(str::to_string)
        }
    }

    /// The session under Wayland, where neither the X name nor the process list may be asked.
    fn wayland(vars: &[&str]) -> Option<&'static str> {
        session(
            env(vars),
            || panic!("X name read under Wayland"),
            || panic!("process list read under Wayland"),
        )
    }

    #[test]
    fn wayland_sessions() {
        let w = "WAYLAND_DISPLAY=wayland-1";
        assert_eq!(wayland(&[w, "SWAYSOCK=/run/sway.sock"]), Some("sway"));
        assert_eq!(
            wayland(&[w, "HYPRLAND_INSTANCE_SIGNATURE=abc"]),
            Some("hyprland")
        );
        assert_eq!(wayland(&[w, "NIRI_SOCKET=/run/niri.sock"]), Some("niri"));
        // Two at once: the order in AC #1.
        assert_eq!(
            wayland(&[w, "NIRI_SOCKET=/n", "HYPRLAND_INSTANCE_SIGNATURE=abc"]),
            Some("hyprland")
        );
        assert_eq!(wayland(&[w, "NIRI_SOCKET=/n", "SWAYSOCK=/s"]), Some("sway"));
        // The sockets missing (started outside the compositor): the desktop variable, a colon list, any case.
        for (desktop, want) in [
            ("Hyprland", Some("hyprland")),
            ("niri", Some("niri")),
            ("sway", Some("sway")),
            ("KDE", Some("kde")),
            ("GNOME", Some("gnome")),
            ("ubuntu:GNOME", Some("gnome")),
            ("GNOME-Classic:GNOME", Some("gnome")),
            ("X-Cinnamon", None),
            // X11-only names are no Wayland session, even left over in the environment.
            ("xmonad", None),
            ("i3:GNOME", Some("gnome")),
        ] {
            let d = format!("XDG_CURRENT_DESKTOP={desktop}");
            assert_eq!(wayland(&[w, &d]), want, "{desktop}");
        }
        // Empty variables count as unset.
        assert_eq!(
            wayland(&[w, "SWAYSOCK=", "XDG_CURRENT_DESKTOP=KDE"]),
            Some("kde")
        );
        assert_eq!(
            wayland(&[
                w,
                "SWAYSOCK=",
                "HYPRLAND_INSTANCE_SIGNATURE=",
                "NIRI_SOCKET=",
                "XDG_CURRENT_DESKTOP="
            ]),
            None
        );
        assert_eq!(wayland(&[w]), None);
    }

    /// The session on X11 with the check window named `name` (`None`: unreadable) and processes `comms`.
    fn x11(vars: &[&str], name: Option<&str>, comms: &[&str]) -> Option<&'static str> {
        session(
            env(vars),
            || name.map(str::to_string),
            || comms.iter().map(|c| c.to_string()).collect(),
        )
    }

    #[test]
    fn x11_sessions() {
        for no_wayland in [&[][..], &["WAYLAND_DISPLAY="][..]] {
            for (name, want) in [
                ("xmonad", "xmonad"),
                ("i3", "i3"),
                ("KWin", "kde"),
                ("GNOME Shell", "gnome"),
                ("Xfwm4", "xfce"),
            ] {
                assert_eq!(x11(no_wayland, Some(name), &[]), Some(want), "{name}");
            }
            // The previous behaviour: SWAYSOCK without WAYLAND_DISPLAY is X11, and so are the other sockets.
            let sockets = [
                no_wayland,
                &[
                    "SWAYSOCK=/s",
                    "HYPRLAND_INSTANCE_SIGNATURE=abc",
                    "NIRI_SOCKET=/n",
                ],
            ]
            .concat();
            assert_eq!(x11(&sockets, Some("xmonad"), &[]), Some("xmonad"));
            assert_eq!(x11(&sockets, None, &[]), None);
        }
        // xmonad's "LG3D" is no known name: the later steps decide.
        assert_eq!(
            x11(&["DESKTOP_SESSION=xmonad"], Some("LG3D"), &[]),
            Some("xmonad")
        );
        assert_eq!(x11(&[], Some("LG3D"), &["xmonad-x86_64-l"]), Some("xmonad"));
        // An unreadable name, then each later step in turn.
        assert_eq!(
            x11(&["XDG_CURRENT_DESKTOP=ubuntu:GNOME"], None, &[]),
            Some("gnome")
        );
        assert_eq!(x11(&["DESKTOP_SESSION=i3"], None, &[]), Some("i3"));
        assert_eq!(
            x11(&["DESKTOP_SESSION=/usr/share/xsessions/plasma"], None, &[]),
            Some("kde")
        );
        assert_eq!(
            x11(&["DESKTOP_SESSION=i3-with-shmlog"], None, &[]),
            Some("i3")
        );
        for config in [
            "xmonad-aarch64-",
            "xmonad-mips64el",
            "xmonad-arm-linux",
            "xmonad-x86_64-f",
        ] {
            assert_eq!(x11(&[], None, &[config]), Some("xmonad"), "{config}");
        }
        for path in [
            "/usr/share/xsessions/plasma/",
            "/usr/share/xsessions/plasma.desktop",
        ] {
            let d = format!("DESKTOP_SESSION={path}");
            assert_eq!(x11(&[&d], None, &[]), Some("kde"), "{path}");
        }
        assert_eq!(x11(&[], None, &["zsh", "i3", "nvim"]), Some("i3"));
        assert_eq!(x11(&[], None, &["zsh", "gnome-shell"]), Some("gnome"));
        // Earlier steps beat later ones; a recognised name beats a contradicting desktop variable.
        assert_eq!(
            x11(
                &["XDG_CURRENT_DESKTOP=KDE", "DESKTOP_SESSION=i3"],
                Some("xmonad"),
                &["openbox"]
            ),
            Some("xmonad")
        );
        assert_eq!(
            x11(
                &["XDG_CURRENT_DESKTOP=KDE", "DESKTOP_SESSION=i3"],
                None,
                &["openbox"]
            ),
            Some("kde")
        );
        assert_eq!(x11(&["DESKTOP_SESSION=i3"], None, &["openbox"]), Some("i3"));
        // A Wayland-only compositor is never an X11 window manager; the search goes on past it.
        assert_eq!(
            x11(
                &["XDG_CURRENT_DESKTOP=sway", "DESKTOP_SESSION=hyprland"],
                None,
                &["niri", "xmonad"]
            ),
            Some("xmonad")
        );
        // A process name matches whole, or as xmonad's compiled config; helpers named after a desktop don't.
        assert_eq!(
            x11(
                &[],
                None,
                &[
                    "i3status",
                    "dwmblocks",
                    "xmonadx",
                    "gnome-keyring-d",
                    "i3-msg",
                    "kwalletd6",
                    "xmonad-log",
                    "xmonad-dbus",
                    "xmonad-log-appl",
                    "xmonad-contrib"
                ]
            ),
            None
        );
        // A Wayland-only name from the window manager itself is skipped too.
        assert_eq!(x11(&[], Some("sway"), &["xmonad"]), Some("xmonad"));
        // Nothing matching anywhere.
        assert_eq!(
            x11(&["XDG_CURRENT_DESKTOP=X-Cinnamon"], Some("LG3D"), &["zsh"]),
            None
        );
    }

    #[test]
    fn check_window_id_from_xprop() {
        assert_eq!(
            check_window_id("_NET_SUPPORTING_WM_CHECK(WINDOW): window id # 0x200001\n"),
            Some("0x200001")
        );
        assert_eq!(
            check_window_id("_NET_SUPPORTING_WM_CHECK:  not found.\n"),
            None
        );
        assert_eq!(check_window_id(""), None);
        assert_eq!(check_window_id("window id # 0x"), None);
        assert_eq!(check_window_id("window id # 0xzz"), None);
    }

    #[test]
    fn user_comms_reads_a_proc_tree() {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("kbf-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (entry, comm) in [
            ("12", Some("xmonad-x86_64-l")),
            ("3", Some("zsh")),
            ("self", Some("no")),
            ("40", None),
        ] {
            std::fs::create_dir_all(dir.join(entry)).unwrap();
            if let Some(comm) = comm {
                std::fs::write(dir.join(entry).join("comm"), format!("{comm}\n")).unwrap();
            }
        }
        // A file named like a pid is no process either.
        std::fs::write(dir.join("77"), "").unwrap();
        let uid = std::fs::metadata(&dir).unwrap().uid();
        let mine = user_comms(&dir, uid);
        // Another user's processes: a uid that owns no entry.
        let theirs = user_comms(&dir, uid.wrapping_add(1));
        let missing = user_comms(&dir.join("missing"), uid);
        let _ = std::fs::remove_dir_all(&dir);
        // By pid, without the newline; `self` (not a pid) and 40 (vanished: no comm) skipped.
        assert_eq!(mine, ["zsh", "xmonad-x86_64-l"]);
        assert!(theirs.is_empty());
        assert!(missing.is_empty());
    }

    #[test]
    fn which_sessions_have_a_provider() {
        // Each session with a window-manager provider names itself; a new one adds a row.
        let with = ["sway", "hyprland", "xmonad", "kde"];
        assert_eq!(wm_provider(None), None);
        for (s, _) in SESSIONS {
            let want = with.contains(s).then_some(*s);
            assert_eq!(wm_provider(Some(s)), want, "{s}");
        }
        for (s, _) in SESSIONS {
            if let Some(p) = wm_provider(Some(s)) {
                assert!(crate::providers::KNOWN.contains(&p), "{p}");
            }
        }
    }

    #[test]
    fn i3_with_a_leftover_xmonad_dump_lists_no_xmonad() {
        let list = |name| {
            let s = x11(&[], Some(name), &[]);
            providers_for(wm_provider(s), Some("Alacritty"), Some("nvim"), &[])
        };
        assert_eq!(
            list("xmonad"),
            ["xmonad", "espanso", "alacritty", "nvim", "sheets"]
        );
        assert_eq!(list("i3"), ["espanso", "alacritty", "nvim", "sheets"]);
    }

    #[test]
    fn wayland_without_a_focus_source_reads_no_window() {
        for session in [Some("niri"), Some("gnome"), None] {
            let (ctx, _) = focus(
                session,
                true,
                |_| panic!("X read under Wayland"),
                |p, _| panic!("{p} asked outside sway, Hyprland and KDE"),
            );
            assert_eq!((ctx.window_id, ctx.class, ctx.title), (None, None, None));
            assert_eq!(ctx.providers, ["espanso"]);
        }
        // On X11 every window manager, known or not, reads the focus from X.
        for session in [Some("i3"), Some("xmonad"), None] {
            let (ctx, _) = focus(
                session,
                false,
                |ctx| {
                    ctx.class = Some("Gimp".into());
                    Ok(None)
                },
                |p, _| panic!("{p} asked on X11"),
            );
            let want = [
                wm_provider(session).into_iter().collect(),
                vec!["espanso", "sheets"],
            ]
            .concat();
            assert_eq!(ctx.providers, want, "{session:?}");
        }
        // The program inside the window comes back for plugins to match on, from X and from sway.
        let gimp = |ctx: &mut Context| {
            ctx.class = Some("Gimp".into());
            Ok(Some(Foreground {
                comm: "btop".into(),
                group: vec!["btop".into()],
            }))
        };
        let (_, program) = focus(Some("xmonad"), false, gimp, |p, _| {
            panic!("{p} asked on X11")
        });
        assert_eq!(program.as_deref(), Some("btop"));
        let (_, program) = focus(
            Some("i3"),
            false,
            |_| Err(anyhow::anyhow!("no xdotool")),
            |_, _| panic!(),
        );
        assert_eq!(program, None);
        let (_, program) = focus(
            Some("niri"),
            true,
            |_| panic!("X under Wayland"),
            |_, _| panic!(),
        );
        assert_eq!(program, None);
        // Sway reads its tree, never X.
        let (ctx, _) = focus(
            Some("sway"),
            true,
            |_| panic!("X read under sway"),
            |p, args| {
                assert_eq!((p, args), ("swaymsg", &["-t", "get_tree"][..]));
                Ok(tree("xwayland"))
            },
        );
        assert_eq!(ctx.class.as_deref(), Some("Gimp"));
        assert_eq!(ctx.window_id.as_deref(), Some("9"));
        assert_eq!(ctx.providers, ["sway", "espanso", "sheets"]);
    }

    #[test]
    fn only_terminals_are_looked_inside() {
        let props = |class: &str, pid| Props {
            class: Some(class.into()),
            instance: None,
            pid,
            title: None,
        };
        // Not a terminal: no lookup, even with a pid that has no process.
        assert_eq!(
            program_in(&props("Gimp", Some(u32::MAX)), "pid").unwrap(),
            None
        );
        assert_eq!(program_in(&Props::default(), "pid").unwrap(), None);
        let e = program_in(&props("Alacritty", None), "pid in sway's tree").unwrap_err();
        assert!(e.to_string().contains("no pid in sway's tree"), "{e}");
        assert!(program_in(&props("Alacritty", Some(u32::MAX)), "pid").is_err());
        // Any terminal is looked inside, and the error names it.
        let e = program_in(&props("kitty", None), "_NET_WM_PID").unwrap_err();
        assert_eq!(e.to_string(), "kitty window has no _NET_WM_PID");
        let e = program_in(&props("foot", Some(u32::MAX)), "pid").unwrap_err();
        assert!(e.to_string().contains("inside foot"), "{e}");
    }

    // The shape of `swaymsg -t get_tree` (sway 1.12), trimmed: an output, a workspace, a tiled Wayland alacritty, a
    // floating Xwayland window.
    fn tree(focused: &str) -> String {
        tree_with(focused, "\"Alacritty\"")
    }

    /// `alacritty_app_id`: the JSON for the tiled window's `app_id`.
    fn tree_with(focused: &str, alacritty_app_id: &str) -> String {
        format!(
            r#"{{"id": 1, "type": "root", "focused": false, "nodes": [
  {{"id": 3, "type": "output", "name": "DP-1", "focused": false, "nodes": [
    {{"id": 4, "type": "workspace", "name": "1", "focused": {ws}, "nodes": [
      {{"id": 7, "type": "con", "name": "host: nvim", "app_id": {app_id}, "pid": 4745,
        "window_properties": {{"class": "XClass"}},
        "focused": {a}, "nodes": [], "floating_nodes": []}}
    ], "floating_nodes": [
      {{"id": 9, "type": "floating_con", "name": "GIMP", "app_id": null, "pid": 5150,
        "window_properties": {{"class": "Gimp", "instance": "gimp"}}, "focused": {x}, "nodes": [], "floating_nodes": []}}
    ]}}
  ]}}
], "floating_nodes": []}}"#,
            ws = focused == "ws",
            a = focused == "alacritty",
            x = focused == "xwayland",
            app_id = alacritty_app_id,
        )
    }

    #[test]
    fn sway_tree_focused_window() {
        assert_eq!(
            parse_sway_tree(&tree("alacritty")).unwrap().unwrap(),
            (
                "7".to_string(),
                Props {
                    class: Some("Alacritty".into()),
                    instance: None,
                    pid: Some(4745),
                    title: Some("host: nvim".into()),
                }
            )
        );
        // Xwayland: no app_id, so the X class.
        assert_eq!(
            parse_sway_tree(&tree("xwayland")).unwrap().unwrap(),
            (
                "9".to_string(),
                Props {
                    class: Some("Gimp".into()),
                    instance: Some("gimp".into()),
                    pid: Some(5150),
                    title: Some("GIMP".into()),
                }
            )
        );
    }

    #[test]
    fn sway_tree_app_id_wins_over_the_x_class() {
        let (_, p) = parse_sway_tree(&tree("alacritty")).unwrap().unwrap();
        assert_eq!(p.class.as_deref(), Some("Alacritty"));
        // A native Wayland window has no instance; an XWayland one has `window_properties.instance`.
        assert_eq!(p.instance, None);
        let (_, p) = parse_sway_tree(&tree("xwayland")).unwrap().unwrap();
        assert_eq!(p.instance.as_deref(), Some("gimp"));
        // With app_id null, the X class.
        let (_, p) = parse_sway_tree(&tree_with("alacritty", "null"))
            .unwrap()
            .unwrap();
        assert_eq!(p.class.as_deref(), Some("XClass"));
    }

    #[test]
    fn sway_tree_without_a_focused_window() {
        // An empty workspace: no window, and no error to warn about.
        assert_eq!(parse_sway_tree(&tree("ws")).unwrap(), None);
        let e = parse_sway_tree(&tree("none")).unwrap_err().to_string();
        assert!(e.contains("no focused node"), "{e}");
        assert!(parse_sway_tree("not json").is_err());
    }

    // Real xprop output (2026-10-21), with the title made neutral.
    const ALACRITTY: &str = r#"WM_CLASS(STRING) = "Alacritty", "Alacritty"
_NET_WM_PID(CARDINAL) = 4745
_NET_WM_NAME(UTF8_STRING) = "host: some-project"
"#;

    #[test]
    fn parses_alacritty() {
        assert_eq!(
            parse_xprop(ALACRITTY),
            Props {
                class: Some("Alacritty".into()),
                instance: Some("Alacritty".into()),
                pid: Some(4745),
                title: Some("host: some-project".into()),
            }
        );
    }

    #[test]
    fn parses_non_terminal_with_escapes_and_unicode() {
        // xprop escapes `\` and `"` (checked: a title of a\b"c é prints as "a\\b\"c é").
        let text = r#"WM_CLASS(STRING) = "vivaldi-stable", "Vivaldi-stable"
_NET_WM_PID(CARDINAL) = 1234
_NET_WM_NAME(UTF8_STRING) = "a\\b\"c é — \"quoted\", too"
"#;
        let p = parse_xprop(text);
        assert_eq!(p.class.as_deref(), Some("Vivaldi-stable"));
        assert_eq!(p.title.as_deref(), Some(r#"a\b"c é — "quoted", too"#));
    }

    #[test]
    fn missing_properties_are_none() {
        let text = "WM_CLASS:  not found.\n_NET_WM_PID:  not found.\n_NET_WM_NAME:  no such atom on any window.\n";
        assert_eq!(parse_xprop(text), Props::default());
        assert_eq!(parse_xprop(""), Props::default());
        assert_eq!(
            parse_xprop("_NET_WM_PID(CARDINAL) = notanumber\n").pid,
            None
        );
    }

    #[test]
    fn single_string_class_is_used() {
        assert_eq!(
            parse_xprop("WM_CLASS(STRING) = \"Only\"\n")
                .class
                .as_deref(),
            Some("Only")
        );
    }

    /// Manual check on a live X session (AC #3): `cargo test live_ -- --ignored --nocapture`, optionally with
    /// `KBF_WINDOW=<decimal id>` to inspect a window other than the focused one.
    #[test]
    #[ignore]
    fn live_window() {
        let mut ctx = detect();
        if let Ok(id) = std::env::var("KBF_WINDOW") {
            let child = read_window(&mut ctx, id);
            settle(&mut ctx, wm_provider(current_session()), child);
        }
        println!(
            "window_id={:?} class={:?} instance={:?} title={:?} providers={:?}",
            ctx.window_id, ctx.class, ctx.instance, ctx.title, ctx.providers
        );
    }

    #[test]
    fn wm_class_instance() {
        let app = "abcdefghijklmnopabcdefghijklmnop";
        let class_of = |line: &str| {
            let p = parse_xprop(line);
            (p.class, p.instance)
        };
        assert_eq!(
            class_of(&format!(
                r#"WM_CLASS(STRING) = "crx_{app}", "Google-chrome""#
            )),
            (Some("Google-chrome".into()), Some(format!("crx_{app}")))
        );
        assert_eq!(
            class_of(r#"WM_CLASS(STRING) = "st-256color", "St""#),
            (Some("St".into()), Some("st-256color".into()))
        );
        assert_eq!(
            class_of(r#"WM_CLASS(STRING) = "lonely""#),
            (Some("lonely".into()), None)
        );
        assert_eq!(class_of("WM_CLASS:  not found."), (None, None));
        assert_eq!(
            class_of(r#"WM_CLASS(STRING) = "", "Foo""#),
            (Some("Foo".into()), None)
        );
    }

    #[test]
    fn title_falls_back_to_wm_name() {
        let text = "WM_CLASS(STRING) = \"xterm\", \"XTerm\"\n_NET_WM_NAME:  not found.\nWM_NAME(STRING) = \"user@host: ~\"\n";
        assert_eq!(parse_xprop(text).title.as_deref(), Some("user@host: ~"));
        let both = "_NET_WM_NAME(UTF8_STRING) = \"net\"\nWM_NAME(STRING) = \"legacy\"\n";
        assert_eq!(parse_xprop(both).title.as_deref(), Some("net"));
    }

    #[test]
    fn tpgid_from_stat() {
        // Real shape; field 8 is tpgid. A comm can contain spaces and `)`.
        let zsh = "4946 (zsh) S 4813 4946 4946 34817 1191887 4194560 3 0 0";
        assert_eq!(tpgid(zsh), Some(1191887));
        assert_eq!(tpgid("7 (a) b (c)) S 1 7 7 34817 99 0"), Some(99));
        assert_eq!(tpgid("8 (daemon) S 1 8 8 0 -1 0"), None);
        assert_eq!(tpgid("garbage"), None);
    }

    #[test]
    fn run_reports_failure_and_output() {
        assert!(run("false", &[]).is_err());
        assert!(run("/nonexistent-kbf", &[]).is_err());
        assert_eq!(run("sh", &["-c", "echo x"]).unwrap(), "x\n");
        let err = run("sh", &["-c", "echo one >&2; echo two >&2; exit 3"]).unwrap_err();
        assert!(err.to_string().ends_with("one two"), "{err}");
        // An empty stderr: the reason is on stdout.
        let err = run(
            "sh",
            &["-c", "echo '{\"error\": \"pane not found\"}'; exit 1"],
        )
        .unwrap_err();
        assert!(
            err.to_string().ends_with(r#"{"error": "pane not found"}"#),
            "{err}"
        );
    }

    #[test]
    fn run_reads_output_bigger_than_a_pipe() {
        let out = run("head", &["-c", "300000", "/dev/zero"]).unwrap();
        assert_eq!(out.len(), 300000);
    }

    #[test]
    fn run_times_out() {
        let started = Instant::now();
        let err = run("sleep", &["5"]).unwrap_err();
        assert!(err.to_string().contains("within 1.0 s"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    /// A `sh` with no controlling terminal (via `setsid`, which may fork) that has forked one `sleep`. Returns the
    /// shell's own pid, as it reports it; kill it with `kill_pid` when done.
    fn shell_with_sleep() -> u32 {
        use std::io::BufRead;
        let mut setsid = Command::new("setsid")
            .args(["sh", "-c", "sleep 5 & echo $$; wait"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(setsid.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        // setsid exits once the shell is running (or is the shell); reap it either way after the shell is killed.
        let pid = line.trim().parse().unwrap();
        std::thread::spawn(move || setsid.wait());
        pid
    }

    /// The leader's comm, which most tests look at.
    fn foreground_comm(pid: u32) -> anyhow::Result<Option<String>> {
        foreground(pid).map(|f| f.map(|f| f.comm))
    }

    fn kill_pid(pid: u32) {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }

    #[test]
    fn foreground_of_a_process_without_a_terminal_is_its_child() {
        // No controlling tty, so tpgid is -1 and the child's own name is used.
        let sh = shell_with_sleep();
        // The forked child is named `sh` until it execs `sleep`; give it up to 1 s.
        let mut got = foreground_comm(sh);
        for _ in 0..100 {
            if got.as_ref().ok().and_then(|c| c.as_deref()) == Some("sleep") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            got = foreground_comm(sh);
        }
        kill_pid(sh);
        assert_eq!(got.unwrap().as_deref(), Some("sleep"));
    }

    /// Polls `program_in` for a window of `class` whose pid is `pid` until it finds `want` (a forked child is named
    /// after its parent until it execs), for up to 2 s; returns the last result.
    fn poll_program_in(class: &str, pid: u32, want: &str) -> anyhow::Result<Option<String>> {
        let props = Props {
            class: Some(class.into()),
            instance: None,
            pid: Some(pid),
            title: None,
        };
        let comm = || program_in(&props, "pid").map(|f| f.map(|f| f.comm));
        let mut got = comm();
        for _ in 0..200 {
            if got.as_ref().ok().and_then(|c| c.as_deref()) == Some(want) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            got = comm();
        }
        got
    }

    #[test]
    fn program_inside_a_stand_in_terminal_is_found() {
        // The shell stands in for a kitty process: its own session, one child.
        let sh = shell_with_sleep();
        let got = poll_program_in("kitty", sh, "sleep");
        kill_pid(sh);
        assert_eq!(got.unwrap().as_deref(), Some("sleep"));
    }

    /// Kills (and reaps) a test's child when dropped, so a failed assert leaves nothing running.
    struct KillOnDrop(std::process::Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn a_terminal_serving_several_windows_is_named_in_the_warning() {
        use std::io::BufRead;
        let mut setsid = KillOnDrop(
            Command::new("setsid")
                .args(["sh", "-c", "sleep 5 & sleep 5 & echo $$; wait"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        std::io::BufReader::new(setsid.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let sh: u32 = line.trim().parse().unwrap();
        let props = Props {
            class: Some("foot".into()),
            instance: None,
            pid: Some(sh),
            title: None,
        };
        let got = program_in(&props, "pid");
        // The shell leads its own session and process group: kill the group, sleeps included.
        let _ = Command::new("kill")
            .args(["--", &format!("-{sh}")])
            .status();
        let e = format!("{:#}", got.unwrap_err());
        assert!(e.starts_with("finding the program inside foot: "), "{e}");
        assert!(
            e.contains("has 2 children (one process serving several windows or tabs?)"),
            "{e}"
        );
    }

    /// AC: a real xterm under Xvfb. Skipped (with a message, seen with `--nocapture`) when `xterm`, `Xvfb`, `xdotool`
    /// or `xprop` is missing.
    #[test]
    fn program_inside_a_real_xterm_is_found() {
        use std::io::BufRead;
        let have = |tool: &str| {
            Command::new("sh")
                .args(["-c", "command -v \"$1\"", "sh", tool])
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if let Some(missing) = ["xterm", "Xvfb", "xdotool", "xprop"]
            .into_iter()
            .find(|t| !have(t))
        {
            eprintln!("skipping program_inside_a_real_xterm_is_found: no {missing}");
            return;
        }
        // Xvfb picks a free display itself and prints its number once it is ready.
        let mut xvfb = KillOnDrop(
            Command::new("Xvfb")
                .args(["-displayfd", "1", "-nolisten", "tcp"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut number = String::new();
        std::io::BufReader::new(xvfb.0.stdout.take().unwrap())
            .read_line(&mut number)
            .unwrap();
        let display = format!(":{}", number.trim());
        assert!(display.len() > 1, "Xvfb printed no display number");
        let x = |program: &str, args: &[&str]| {
            Command::new(program)
                .args(args)
                .env("DISPLAY", &display)
                .stderr(Stdio::null())
                .output()
        };
        let xterm = KillOnDrop(
            Command::new("xterm")
                // +ut: no utmp entry, which a killed xterm would leave behind.
                .args(["+ut", "-e", "sleep", "30"])
                .env("DISPLAY", &display)
                .spawn()
                .unwrap(),
        );
        let pid = xterm.0.id().to_string();
        let mut found = None;
        for _ in 0..100 {
            let ids = x("xdotool", &["search", "--pid", &pid])
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            if let Some(id) = ids
                .ok()
                .and_then(|ids| ids.split_whitespace().last().map(str::to_string))
            {
                let props = x(
                    "xprop",
                    &[
                        "-id",
                        &id,
                        "WM_CLASS",
                        "_NET_WM_PID",
                        "_NET_WM_NAME",
                        "WM_NAME",
                    ],
                )
                .map(|o| parse_xprop(&String::from_utf8_lossy(&o.stdout)));
                if let Ok(props) = props {
                    if let Ok(Some(Foreground { comm, .. })) = program_in(&props, "_NET_WM_PID") {
                        let done = comm == "sleep";
                        found = Some((props.class, comm));
                        if done {
                            break;
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let (class, comm) = found.expect("found no program inside the xterm window");
        assert_eq!(class.as_deref(), Some("XTerm"));
        assert_eq!(comm, "sleep");
    }

    /// A stand-in terminal and what runs in it: `pid` is the terminal's, `bin` a PATH dir whose `less`, `pager` and
    /// `decoy` are symlinks to `sleep` (a symlinked exe's comm is the link's name). Dropping it kills every process
    /// in the stand-in's session and removes `bin`, so a failed assert leaves nothing running.
    struct StandIn {
        pid: u32,
        /// Whether `pid` leads its own session (setsid). `script` doesn't: it runs in the test's session, so only
        /// its child's session is killed.
        own_session: bool,
        bin: std::path::PathBuf,
        _child: KillOnDrop,
    }

    impl Drop for StandIn {
        fn drop(&mut self) {
            let sid = |pid: u32| {
                std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .ok()
                    .and_then(|s| stat_field(&s, 3))
            };
            // The stand-in's own session (setsid), or its child's (script's shell).
            let mut sids: Vec<u32> = sid(self.pid)
                .filter(|s| self.own_session && *s == self.pid)
                .into_iter()
                .collect();
            if let Ok(c) =
                std::fs::read_to_string(format!("/proc/{}/task/{}/children", self.pid, self.pid))
            {
                sids.extend(c.split_whitespace().filter_map(|p| sid(p.parse().ok()?)));
            }
            for p in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
                let Some(pid) = p.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                    continue;
                };
                // Never the test runner's own session; with that unknown, nothing.
                let Some(ours) = sid(std::process::id()) else {
                    break;
                };
                if sid(pid).is_some_and(|s| sids.contains(&s) && s != ours) {
                    kill_pid_now(pid);
                }
            }
            let _ = std::fs::remove_dir_all(&self.bin);
        }
    }

    fn kill_pid_now(pid: u32) {
        let _ = Command::new("kill")
            .args(["-s", "KILL", &pid.to_string()])
            .status();
    }

    /// A PATH dir holding `less`, `pager` and `decoy` as symlinks to `sleep`, unique per call.
    fn sleep_links() -> (std::path::PathBuf, String) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bin = std::env::temp_dir().join(format!("kbf-less-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        let out = Command::new("sh")
            .args(["-c", "command -v sleep"])
            .output()
            .unwrap();
        let sleep = String::from_utf8(out.stdout).unwrap();
        for name in ["less", "pager", "decoy"] {
            std::os::unix::fs::symlink(sleep.trim(), bin.join(name)).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        (bin, path)
    }

    /// A `sh` in its own session (no tty, so `tpgid` -1) whose one child runs `pipeline`.
    fn terminal_running(pipeline: &str) -> StandIn {
        use std::io::BufRead;
        let (bin, path) = sleep_links();
        let mut setsid = KillOnDrop(
            Command::new("setsid")
                .args(["sh", "-c", "sh -c \"$1\" & echo $$; wait", "sh", pipeline])
                .env("PATH", path)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut line = String::new();
        std::io::BufReader::new(setsid.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        StandIn {
            pid: line.trim().parse().unwrap(),
            own_session: true,
            bin,
            _child: setsid,
        }
    }

    /// Polls `foreground` until `done` holds for its group (forked children take a moment to exec), for up to 2 s.
    fn group_until(pid: u32, done: impl Fn(&[String]) -> bool) -> Option<Foreground> {
        let mut fg = None;
        for _ in 0..200 {
            fg = foreground(pid).ok().flatten();
            if fg.as_ref().is_some_and(|f| done(&f.group)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        fg
    }

    fn count(group: &[String], name: &str) -> usize {
        group.iter().filter(|c| *c == name).count()
    }

    fn have(tool: &str) -> bool {
        let found = Command::new("sh")
            .args(["-c", "command -v \"$1\"", "sh", tool])
            .output()
            .is_ok_and(|o| o.status.success());
        if !found {
            eprintln!("skipped: no {tool}");
        }
        found
    }

    #[test]
    fn less_is_found_anywhere_in_the_foreground_group() {
        if !have("setsid") {
            return;
        }
        // `man` and `git log` shape: less is one stage, not the leader. No tty: the child's own group.
        let t = terminal_running("less 30 | sleep 30");
        let fg = group_until(t.pid, |g| count(g, "less") == 1 && count(g, "sleep") == 1);
        let group = fg.map(|f| f.group).unwrap_or_default();
        assert_eq!(
            (count(&group, "less"), count(&group, "sleep")),
            (1, 1),
            "{group:?}"
        );
        drop(t);
        // A group without less: both stages exec'd, neither is less.
        let t = terminal_running("sleep 30 | sleep 31");
        let group = group_until(t.pid, |g| count(g, "sleep") == 2)
            .map(|f| f.group)
            .unwrap_or_default();
        assert_eq!(
            (count(&group, "sleep"), count(&group, "less")),
            (2, 0),
            "{group:?}"
        );
        drop(t);
        // Debian's man runs less as `pager`, a link to it: named by the program it runs (`sleep` here).
        let t = terminal_running("pager 30 | sleep 31");
        let group = group_until(t.pid, |g| count(g, "sleep") == 2)
            .map(|f| f.group)
            .unwrap_or_default();
        assert_eq!(
            (count(&group, "sleep"), count(&group, "pager")),
            (2, 0),
            "{group:?}"
        );
    }

    #[test]
    fn the_terminals_foreground_job_is_the_group_read() {
        if !have("script") || !have("bash") {
            return;
        }
        // A real tty (script's pty), job control on: the background `decoy` gets its own group, and `tpgid` names
        // the foreground pipeline's, which less leads. Neither the shell nor the decoy is in it.
        let (bin, path) = sleep_links();
        let child = KillOnDrop(
            Command::new("script")
                .args(["-qec", "set -m; decoy 33 & less 30 | sleep 30", "/dev/null"])
                .env("SHELL", "bash")
                .env("PATH", path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let t = StandIn {
            pid: child.0.id(),
            own_session: false,
            bin,
            _child: child,
        };
        let fg = group_until(t.pid, |g| count(g, "less") == 1 && count(g, "sleep") == 1);
        let fg = fg.expect("no foreground program inside script");
        assert_eq!(fg.comm, "less");
        let mut group = fg.group;
        group.sort();
        assert_eq!(group, ["less", "sleep"]);
    }

    #[test]
    fn process_with_no_children_has_no_program() {
        let mut sleep = Command::new("sleep").arg("30").spawn().unwrap();
        let got = foreground_comm(sleep.id());
        let _ = sleep.kill();
        let _ = sleep.wait();
        assert_eq!(got.unwrap(), None);
    }
}
