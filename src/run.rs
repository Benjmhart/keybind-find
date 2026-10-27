//! Running a pick: give focus back to the window that had it, then press its keys (or type an espanso trigger).
//! Only keys are sent; nothing from a config is ever executed, and no shell is involved.

use crate::sheet::Entry;
use anyhow::{bail, Context as _};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// xdotool's commands (`xdotool help`, 4.2026). `xdotool key`/`type` read arguments "until a new xdotool command is
/// seen", so an argument spelled like one of these would start that command (`exec` runs a program). Keys and
/// triggers that match are refused.
const XDOTOOL_COMMANDS: &[&str] = &[
    "getactivewindow",
    "getwindowfocus",
    "getwindowname",
    "getwindowclassname",
    "getwindowpid",
    "getwindowgeometry",
    "getdisplaygeometry",
    "search",
    "selectwindow",
    "help",
    "version",
    "behave",
    "behave_screen_edge",
    "click",
    "getmouselocation",
    "key",
    "keydown",
    "keyup",
    "mousedown",
    "mousemove",
    "mousemove_relative",
    "mouseup",
    "set_window",
    "type",
    "windowactivate",
    "windowfocus",
    "windowkill",
    "windowclose",
    "windowquit",
    "windowmap",
    "windowminimize",
    "windowmove",
    "windowraise",
    "windowlower",
    "windowreparent",
    "windowsize",
    "windowstate",
    "windowunmap",
    "set_num_desktops",
    "get_num_desktops",
    "set_desktop",
    "get_desktop",
    "set_desktop_for_window",
    "get_desktop_for_window",
    "get_desktop_viewport",
    "set_desktop_viewport",
    "exec",
    "sleep",
];

/// A stuck X server or a window manager that never confirms the focus change must not hang the hotkey.
const TIMEOUT: Duration = Duration::from_secs(2);

/// The commands that run `e`, as argv lists. `window` is the X window id to re-focus first (X11 only).
pub fn commands(window: Option<&str>, e: &Entry, wayland: bool) -> Vec<Vec<String>> {
    let key = e.bind.key.as_str();
    if key.trim().is_empty() {
        return Vec::new();
    }
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // espanso expands what it sees typed, so its trigger is typed (exactly as written), not pressed. `--`: a trigger
    // may start with `-`.
    let typed = e.provider == "espanso";
    if wayland {
        // sway gives focus back on its own; wtype has no window targeting.
        let mut cmd = argv(&["wtype"]);
        if typed {
            cmd.extend(argv(&["--", key]));
        } else {
            for chord in key.split_whitespace() {
                cmd.extend(wtype_chord(chord));
            }
        }
        return vec![cmd];
    }
    let mut cmds = Vec::new();
    if let Some(id) = window {
        cmds.push(argv(&["xdotool", "windowactivate", "--sync", id]));
    }
    // --clearmodifiers: a modifier still held (Ctrl from fzf's ctrl-n, Super from the hotkey) would join the chord.
    if typed {
        cmds.push(argv(&["xdotool", "type", "--clearmodifiers", "--", key]));
    } else {
        let mut cmd = argv(&["xdotool", "key", "--clearmodifiers"]);
        cmd.extend(key.split_whitespace().map(xdotool_chord));
        cmds.push(cmd);
    }
    // xdotool then presses the cleared modifiers again on X's XTEST device, where letting go of the real key never
    // releases them: Ctrl stays down and every xmonad binding stops matching until re-login. Release them there.
    // ponytail: skipped when the send fails or times out; a send killed mid-chord can still strand a key.
    cmds.push(argv(&[
        "xdotool",
        "keyup",
        "Shift_L",
        "Shift_R",
        "Control_L",
        "Control_R",
        "Alt_L",
        "Alt_R",
        "Super_L",
        "Super_R",
    ]));
    cmds
}

/// xdotool reaches keypad digits (`KP_7`) by pressing Num_Lock, which leaves NumLock toggled after every send. A
/// terminal sends the same character for either key, so press the plain digit (or period) instead.
fn xdotool_chord(chord: &str) -> String {
    let (mods, keysym) = chord.rsplit_once('+').unwrap_or(("", chord));
    let plain = match keysym {
        "KP_Decimal" => Some("period".to_string()),
        k => k
            .strip_prefix("KP_")
            .filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
            .map(str::to_string),
    };
    match (plain, mods.is_empty()) {
        (Some(p), true) => p,
        (Some(p), false) => format!("{mods}+{p}"),
        (None, _) => chord.to_string(),
    }
}

/// `super+shift+k` → `-M logo -M shift -k k -m shift -m logo`: press modifiers, tap the key, release in reverse.
fn wtype_chord(chord: &str) -> Vec<String> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let keysym = parts.pop().unwrap_or_default();
    let mut mods: Vec<&str> = parts
        .into_iter()
        .map(|m| wtype_modifier(m).unwrap_or(m))
        .collect();
    // wtype looks keysyms up case-insensitively (`-k K` sends `k`): an uppercase letter is shift + the letter.
    let lower = keysym.to_ascii_lowercase();
    let keysym = if keysym.len() == 1 && keysym.as_bytes()[0].is_ascii_uppercase() {
        if !mods.contains(&"shift") {
            mods.push("shift");
        }
        lower.as_str()
    } else {
        keysym
    };
    let mut args = Vec::new();
    for m in &mods {
        args.extend(["-M".to_string(), m.to_string()]);
    }
    args.extend(["-k".to_string(), keysym.to_string()]);
    for m in mods.iter().rev() {
        args.extend(["-m".to_string(), m.to_string()]);
    }
    args
}

/// Our modifier names (and xmonad's mod1..mod5) as wtype's ("shift", "capslock", "ctrl", "logo", "win", "alt",
/// "altgr" per `man wtype`).
fn wtype_modifier(m: &str) -> Option<&'static str> {
    Some(match m {
        "super" | "mod4" => "logo",
        "ctrl" => "ctrl",
        "alt" | "mod1" => "alt",
        "shift" => "shift",
        "mod5" => "altgr",
        _ => return None,
    })
}

/// Refuses keys that can't be sent safely: anything outside the key notation (`+`-joined `[A-Za-z0-9_]` parts), an
/// xdotool command name, or a modifier wtype doesn't have.
fn check(e: &Entry, wayland: bool) -> anyhow::Result<()> {
    let key = e.bind.key.as_str();
    if key.trim().is_empty() {
        bail!("{:?} has no key to send", e.bind.desc);
    }
    if e.provider == "espanso" {
        if XDOTOOL_COMMANDS.contains(&key) {
            bail!("refusing to type {key:?}: xdotool would read it as a command");
        }
        return Ok(());
    }
    for chord in key.split_whitespace() {
        let parts: Vec<&str> = chord.split('+').collect();
        let valid = parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if !valid || XDOTOOL_COMMANDS.contains(&chord) {
            bail!("refusing to send {key:?}: {chord:?} is not a key in keybind-find's notation");
        }
        let mods = &parts[..parts.len() - 1];
        if wayland {
            if let Some(m) = mods.iter().find(|m| wtype_modifier(m).is_none()) {
                bail!("refusing to send {key:?}: wtype has no modifier {m:?}");
            }
        } else if let Some(m) = mods
            .iter()
            .find(|m| !["super", "ctrl", "alt", "shift"].contains(m))
        {
            // xdotool silently drops a modifier it has no key for (`mod3`), so a different binding would fire.
            bail!("refusing to send {key:?}: xdotool can't press modifier {m:?}");
        }
    }
    Ok(())
}

/// Runs `e`'s commands in order, or, with `dry_run`, prints them (one argv per line) and runs nothing.
pub fn send(window: Option<&str>, e: &Entry, dry_run: bool) -> anyhow::Result<()> {
    let wayland = is_wayland(std::env::var_os("WAYLAND_DISPLAY").as_deref());
    send_with(
        window,
        e,
        dry_run,
        wayland,
        &mut std::io::stdout().lock(),
        &run_all,
    )
}

fn is_wayland(wayland_display: Option<&std::ffi::OsStr>) -> bool {
    wayland_display.is_some_and(|v| !v.is_empty())
}

fn send_with(
    window: Option<&str>,
    e: &Entry,
    dry_run: bool,
    wayland: bool,
    out: &mut dyn Write,
    run: &dyn Fn(&[Vec<String>]) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    check(e, wayland)?;
    let cmds = commands(window, e, wayland);
    if dry_run {
        // A closed pipe (`--print | head`) is not an error.
        return match out.write_all(render(&cmds).as_bytes()) {
            Err(err) if err.kind() != std::io::ErrorKind::BrokenPipe => Err(err.into()),
            _ => Ok(()),
        };
    }
    run(&cmds)
}

/// One argv per line; arguments that a shell would split or interpret are single-quoted, so the line reads (and pastes)
/// as exactly those arguments.
fn render(cmds: &[Vec<String>]) -> String {
    let quote = |a: &String| {
        let plain = !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_+-:.,/=@%".contains(c));
        if plain {
            a.clone()
        } else {
            format!("'{}'", a.replace('\'', r"'\''"))
        }
    };
    cmds.iter()
        .map(|c| c.iter().map(quote).collect::<Vec<_>>().join(" ") + "\n")
        .collect()
}

/// Runs each argv in order and stops at the first one that can't start, exits non-zero or runs past `TIMEOUT`.
fn run_all(cmds: &[Vec<String>]) -> anyhow::Result<()> {
    for cmd in cmds {
        run_one(cmd, TIMEOUT)?;
    }
    Ok(())
}

fn run_one(cmd: &[String], timeout: Duration) -> anyhow::Result<()> {
    let Some((program, args)) = cmd.split_first() else {
        return Ok(());
    };
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {program} (is it installed?)"))?;
    let deadline = Instant::now() + timeout;
    // ponytail: polls every 10 ms; stderr is read after exit, fine for the few bytes these tools print.
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} did not finish within {}s",
                cmd.join(" "),
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = child.wait_with_output()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    // xdotool skips a key name it doesn't know, still sends the modifiers, and exits 0.
    if status.success() && stderr.contains("No such key name") {
        let stderr = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        bail!("{} sent no key: {stderr}", cmd.join(" "));
    }
    if !status.success() {
        let stderr = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        if args.first().map(String::as_str) == Some("windowactivate") {
            bail!("the window to send keys to is gone or can't be focused: {stderr}");
        }
        bail!("{} failed ({status}): {stderr}", cmd.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Bind;
    use std::cell::RefCell;

    fn entry(provider: &str, key: &str) -> Entry {
        Entry {
            provider: provider.into(),
            category: "c".into(),
            bind: Bind {
                key: key.into(),
                desc: "d".into(),
                action: String::new(),
                when: Default::default(),
            },
        }
    }

    fn release() -> Vec<String> {
        v(&[
            "xdotool",
            "keyup",
            "Shift_L",
            "Shift_R",
            "Control_L",
            "Control_R",
            "Alt_L",
            "Alt_R",
            "Super_L",
            "Super_R",
        ])
    }

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn x11_chords_and_sequences() {
        assert_eq!(
            commands(Some("123"), &entry("xmonad", "super+b"), false),
            [
                v(&["xdotool", "windowactivate", "--sync", "123"]),
                v(&["xdotool", "key", "--clearmodifiers", "super+b"]),
                release(),
            ]
        );
        assert_eq!(
            commands(Some("123"), &entry("herdr", "ctrl+b c"), false)[1],
            v(&["xdotool", "key", "--clearmodifiers", "ctrl+b", "c"])
        );
        assert_eq!(
            commands(None, &entry("xmonad", "Print"), false),
            [
                v(&["xdotool", "key", "--clearmodifiers", "Print"]),
                release()
            ]
        );
        assert_eq!(
            commands(None, &entry("nvim", "space  f f "), false)[..1],
            [v(&[
                "xdotool",
                "key",
                "--clearmodifiers",
                "space",
                "f",
                "f"
            ])]
        );
    }

    #[test]
    fn espanso_is_typed_verbatim() {
        assert_eq!(
            commands(Some("123"), &entry("espanso", ":date"), false)[1],
            v(&["xdotool", "type", "--clearmodifiers", "--", ":date"])
        );
        assert_eq!(
            commands(None, &entry("espanso", "-x"), true),
            [v(&["wtype", "--", "-x"])]
        );
        assert_eq!(
            commands(None, &entry("espanso", "btw "), false)[0][4],
            "btw "
        );
    }

    #[test]
    fn wayland_uses_wtype() {
        assert_eq!(
            commands(Some("123"), &entry("sway", "super+shift+k"), true),
            [v(&[
                "wtype", "-M", "logo", "-M", "shift", "-k", "k", "-m", "shift", "-m", "logo"
            ])]
        );
        assert_eq!(
            commands(None, &entry("herdr", "ctrl+b c"), true),
            [v(&[
                "wtype", "-M", "ctrl", "-k", "b", "-m", "ctrl", "-k", "c"
            ])]
        );
        assert_eq!(
            commands(None, &entry("xmonad", "mod1+mod5+x"), true),
            [v(&[
                "wtype", "-M", "alt", "-M", "altgr", "-k", "x", "-m", "altgr", "-m", "alt"
            ])]
        );
    }

    #[test]
    fn metacharacters_stay_verbatim() {
        let nasty = "a;rm -rf ~ $(x) 'q' \"d\"";
        let typed = commands(None, &entry("espanso", nasty), false);
        assert_eq!(typed[0].iter().filter(|a| *a == nasty).count(), 1);
        let pressed = commands(None, &entry("x", "a;b $(x)"), false);
        assert_eq!(pressed[0][3..], v(&["a;b", "$(x)"]));
    }

    #[test]
    fn unsafe_keys_are_refused() {
        // xdotool would start a new command at `exec`/`windowkill`.
        for key in [
            "a exec touch x",
            "windowkill",
            "a;b",
            "$(x)",
            "-w x",
            "--window 1",
            "ctrl+",
            "+",
            "ctrl++a",
            "ö",
        ] {
            assert!(check(&entry("nvim", key), false).is_err(), "{key}");
        }
        assert!(check(&entry("espanso", "exec"), false).is_err());
        assert!(check(&entry("espanso", "exec this"), false).is_ok());
        for key in [
            "super+shift+Return",
            "ctrl+b c",
            "space f f",
            "XF86AudioRaiseVolume",
            "KP_7",
        ] {
            assert!(check(&entry("x", key), false).is_ok(), "{key}");
        }
        // xdotool silently drops mod2..mod5; wtype has no mod3.
        assert!(check(&entry("x", "mod3+a"), false).is_err());
        assert!(check(&entry("x", "mod5+a"), false).is_err());
        assert!(check(&entry("x", "mod3+a"), true).is_err());
        assert!(check(&entry("x", "mod5+a"), true).is_ok());
    }

    #[test]
    fn keypad_digits_and_uppercase() {
        assert_eq!(
            commands(None, &entry("nvim", "KP_7 ctrl+KP_Decimal KP_Add"), false)[0][3..],
            v(&["7", "ctrl+period", "KP_Add"])
        );
        assert_eq!(
            commands(None, &entry("nvim", "K"), true),
            [v(&["wtype", "-M", "shift", "-k", "k", "-m", "shift"])]
        );
        assert_eq!(
            commands(None, &entry("nvim", "shift+K"), true),
            [v(&["wtype", "-M", "shift", "-k", "k", "-m", "shift"])]
        );
        // xdotool adds the shift for `K` itself.
        assert_eq!(commands(None, &entry("nvim", "K"), false)[0][3], "K");
    }

    #[test]
    fn unknown_key_names_are_errors() {
        let e = format!(
            "{:#}",
            run_one(
                &v(&[
                    "sh",
                    "-c",
                    "echo \"(symbol) No such key name 'ö'. Ignoring it.\" >&2"
                ]),
                TIMEOUT
            )
            .unwrap_err()
        );
        assert!(e.contains("sent no key"), "{e}");
    }

    #[test]
    fn empty_key_sends_nothing() {
        assert!(commands(Some("1"), &entry("x", "  "), false).is_empty());
        assert!(send(None, &entry("x", ""), true).is_err());
    }

    fn never_run(_: &[Vec<String>]) -> anyhow::Result<()> {
        panic!("a dry run must not run anything")
    }

    #[test]
    fn dry_run_prints_and_runs_nothing() {
        let mut out = Vec::new();
        send_with(
            Some("123"),
            &entry("espanso", ":date"),
            true,
            false,
            &mut out,
            &never_run,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "xdotool windowactivate --sync 123\nxdotool type --clearmodifiers -- :date\n\
             xdotool keyup Shift_L Shift_R Control_L Control_R Alt_L Alt_R Super_L Super_R\n"
        );
        let mut out = Vec::new();
        send_with(
            Some("123"),
            &entry("x", "super+b"),
            true,
            true,
            &mut out,
            &never_run,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "wtype -M logo -k b -m logo\n"
        );
        // Arguments a shell would split are quoted.
        assert_eq!(
            render(&[v(&["xdotool", "type", "--", "it's a b"])]),
            "xdotool type -- 'it'\\''s a b'\n"
        );
    }

    struct Broken(std::io::ErrorKind);
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn dry_run_into_a_closed_pipe() {
        let e = entry("x", "a");
        let mut closed = Broken(std::io::ErrorKind::BrokenPipe);
        assert!(send_with(None, &e, true, false, &mut closed, &never_run).is_ok());
        let mut full = Broken(std::io::ErrorKind::StorageFull);
        assert!(send_with(None, &e, true, false, &mut full, &never_run).is_err());
    }

    #[test]
    fn a_real_send_runs_the_commands() {
        let ran = RefCell::new(Vec::new());
        let record = |c: &[Vec<String>]| {
            ran.borrow_mut().extend_from_slice(c);
            Ok(())
        };
        send_with(
            Some("9"),
            &entry("x", "super+b"),
            false,
            false,
            &mut Vec::new(),
            &record,
        )
        .unwrap();
        assert_eq!(
            *ran.borrow(),
            commands(Some("9"), &entry("x", "super+b"), false)
        );
        // A refused key never reaches the runner.
        assert!(send_with(
            None,
            &entry("x", "a exec x"),
            false,
            false,
            &mut Vec::new(),
            &never_run
        )
        .is_err());
    }

    #[test]
    fn wayland_detection() {
        assert!(!is_wayland(None));
        assert!(!is_wayland(Some("".as_ref())));
        assert!(is_wayland(Some("wayland-1".as_ref())));
    }

    #[test]
    fn failures_name_the_tool() {
        let e = format!(
            "{:#}",
            run_all(&[v(&["/nonexistent/xdotool", "key", "x"])]).unwrap_err()
        );
        assert!(e.contains("xdotool"), "{e}");
        let e = format!(
            "{:#}",
            run_all(&[v(&["sh", "-c", "echo bad window >&2; exit 1"])]).unwrap_err()
        );
        assert!(e.contains("bad window") && e.contains("sh -c"), "{e}");
        // Stops at the first failure.
        let marker = std::env::temp_dir().join(format!("kbf-run-{}-stop", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let touch = v(&["touch", marker.to_str().unwrap()]);
        assert!(run_all(&[v(&["false"]), touch]).is_err());
        assert!(!marker.exists());
        assert!(run_all(&[]).is_ok());
    }

    #[test]
    fn a_hung_command_times_out() {
        let started = Instant::now();
        let e = format!(
            "{:#}",
            run_one(&v(&["sleep", "5"]), Duration::from_millis(200)).unwrap_err()
        );
        assert!(e.contains("did not finish"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // A failed refocus says what it means.
        let e = format!(
            "{:#}",
            run_one(&v(&["sh", "windowactivate"]), TIMEOUT).unwrap_err()
        );
        assert!(e.contains("window to send keys to is gone"), "{e}");
    }
}
