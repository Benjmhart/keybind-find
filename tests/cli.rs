//! The whole binary, run as a child with a clean environment: a fixture xmonad keymap, no display, and a `PATH` with
//! nothing on it, so no real espanso, xdotool or notify-send runs and no UI opens.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_keybind-find");

/// An empty directory under cargo's per-test tmp dir.
fn dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A cache dir holding the fixture keymap, as xmonad would have written it.
fn cache_with_keymap(name: &str) -> PathBuf {
    let cache = dir(name);
    std::fs::create_dir_all(cache.join("keybind-find")).unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/xmonad.keys"),
        cache.join("keybind-find/xmonad.keys"),
    )
    .unwrap();
    cache
}

/// The picker stand-in, by absolute path since `PATH` is empty.
fn tool(name: &str) -> String {
    ["/usr/bin", "/bin"]
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| Path::new(p).exists())
        .unwrap_or_else(|| panic!("{name} not found"))
}

fn run(cache: &Path, picker: &str, args: &[&str]) -> Output {
    // Created once and never wiped: wiping it from parallel tests races `create_dir_all` (EEXIST, then gone).
    let empty = Path::new(env!("CARGO_TARGET_TMPDIR")).join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    run_with_path(cache, picker, args, &empty)
}

/// The usage log goes to `<cache>/state`, never the real `~/.local/state`.
fn run_with_path(cache: &Path, picker: &str, args: &[&str], path: &Path) -> Output {
    run_with_env(cache, picker, args, path, &[])
}

fn run_with_env(
    cache: &Path,
    picker: &str,
    args: &[&str],
    path: &Path,
    env: &[(&str, &str)],
) -> Output {
    Command::new(BIN)
        .args(args)
        .env_clear()
        // The fixture keymap is xmonad's: name the session, so detection never falls to the host's processes (with no
        // DISPLAY and no xprop on PATH, the window manager's own name can't be read first).
        .env("DESKTOP_SESSION", "xmonad")
        .envs(env.iter().copied())
        .env("PATH", path)
        .env("HOME", cache)
        .env("XDG_CACHE_HOME", cache)
        .env("XDG_STATE_HOME", cache.join("state"))
        .env("KEYBIND_FIND_PICKER", picker)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn print_runs_the_whole_flow_without_ui() {
    let cache = cache_with_keymap("print");
    let out = run(&cache, &format!("{} -1", tool("head")), &["--print"]);
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(out.status.success(), "{stderr}");
    // No display, so no window to refocus: the keys of the first entry, an xmonad one, then the modifier release.
    assert!(
        stdout.starts_with("xdotool key --clearmodifiers super+"),
        "{stdout}"
    );
    assert_eq!(stdout.lines().count(), 2, "{stdout}");
    assert!(
        stdout.lines().nth(1).unwrap().starts_with("xdotool keyup "),
        "{stdout}"
    );
    // espanso isn't on PATH: one line, and the flow still ran.
    assert!(
        stderr.contains("keybind-find: espanso: not installed"),
        "{stderr}"
    );
}

#[test]
fn a_cancelled_pick_sends_nothing() {
    let cache = cache_with_keymap("cancel");
    let out = run(&cache, &tool("true"), &["--print"]);
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), "");
}

#[test]
fn nothing_to_show_is_an_error() {
    let out = run(
        &dir("no-keymap"),
        &format!("{} -1", tool("head")),
        &["--print"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert!(
        text(&out.stderr).contains("every provider failed (xmonad, espanso)"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn dump_all_and_named() {
    let cache = cache_with_keymap("dump");
    let out = run(&cache, "unused", &["dump"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let providers: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["provider"].clone())
        .collect();
    assert_eq!(providers, ["xmonad"]);

    let out = run(&cache, "unused", &["dump", "xmonad"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["provider"], "xmonad");

    let out = run(&cache, "unused", &["dump", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("xmonad, espanso, nvim, herdr, sway, alacritty, sheets"));

    let out = run(&cache, "unused", &["dump", "sheets"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["provider"], "sheets");

    // A broken sheet is one stderr line naming it, and doesn't stop the dump.
    let sheets = cache.join(".config/keybind-find/sheets");
    std::fs::create_dir_all(&sheets).unwrap();
    std::fs::write(sheets.join("broken.json"), "{").unwrap();
    let out = run(&cache, "unused", &["dump", "sheets"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let lines: Vec<_> = text(&out.stderr)
        .lines()
        .filter(|l| l.starts_with("keybind-find: sheets: "))
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("broken.json"), "{}", lines[0]);
}

#[test]
fn bad_arguments_exit_2() {
    let cache = dir("args");
    for args in [&["--prnt"][..], &["dump", "a", "b"], &["--print", "x"]] {
        let out = run(&cache, "unused", args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(text(&out.stderr).contains("usage:"), "{args:?}");
    }
}

/// A `PATH` holding only an `xdotool` that exits 0, so a real (non-`--print`) pick "sends" without pressing keys.
/// A script, not a symlink to `true`: a multicall coreutils (uutils, busybox) dispatches on the name. It is exec'd
/// by the binary's child well after the write, so the write-then-exec ETXTBSY race doesn't apply.
fn fake_xdotool(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let d = dir(name);
    let x = d.join("xdotool");
    std::fs::write(&x, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&x, std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

fn usage_log(cache: &Path) -> PathBuf {
    cache.join("state/keybind-find/usage.tsv")
}

#[test]
fn a_pick_is_logged_and_print_or_cancel_is_not() {
    let cache = cache_with_keymap("usage-log");
    let path = fake_xdotool("usage-log-path");
    let head = format!("{} -1", tool("head"));

    let out = run_with_path(&cache, &head, &["--print"], &path);
    assert!(out.status.success());
    let out = run_with_path(&cache, &tool("true"), &[], &path);
    assert!(out.status.success());
    assert!(!usage_log(&cache).exists());

    // Not the first row, so logging the wrong entry shows. `--print` with the same picker names the expected key;
    // ask again before each pick, since the first one re-ranks the list.
    let third = format!("{} -n 3p", tool("sed"));
    for n in 1..=2 {
        let printed = text(&run_with_path(&cache, &third, &["--print"], &path).stdout);
        let want = printed
            .lines()
            .next()
            .unwrap()
            .rsplit(' ')
            .next()
            .unwrap()
            .to_string();
        let out = run_with_path(&cache, &third, &[], &path);
        assert!(out.status.success(), "{}", text(&out.stderr));
        let log = std::fs::read_to_string(usage_log(&cache)).unwrap();
        assert_eq!(log.lines().count(), n, "appended, not overwritten: {log}");
        let fields: Vec<_> = log.lines().last().unwrap().split('\t').collect();
        assert_eq!(fields.len(), 3, "{log}");
        assert!(fields[0].parse::<u64>().is_ok(), "{log}");
        assert_eq!(fields[1], "xmonad");
        assert_eq!(fields[2], want, "{log}");
    }
}

#[test]
fn the_most_used_binding_comes_first() {
    let cache = cache_with_keymap("usage-rank");
    let first = run(&cache, &format!("{} -1", tool("head")), &["--print"]);
    let first = text(&first.stdout);
    // A binding further down, used twice.
    std::fs::create_dir_all(usage_log(&cache).parent().unwrap()).unwrap();
    // A bad line and a non-UTF-8 byte spoil only their own lines.
    std::fs::write(
        usage_log(&cache),
        b"1\txmonad\tsuper+shift+Return\n2\txmonad\tsuper+shift+Return\nbad line\n\xff\txmonad\tx\n",
    )
    .unwrap();
    let out = run(&cache, &format!("{} -1", tool("head")), &["--print"]);
    let stdout = text(&out.stdout);
    assert_ne!(stdout, first);
    assert_eq!(
        stdout,
        "xdotool key --clearmodifiers super+shift+Return\n\
         xdotool keyup Shift_L Shift_R Control_L Control_R Alt_L Alt_R Super_L Super_R\n",
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn an_unwritable_log_is_one_stderr_line() {
    use std::os::unix::fs::PermissionsExt;
    let cache = cache_with_keymap("usage-ro");
    let state = cache.join("state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();
    let out = run_with_path(
        &cache,
        &format!("{} -1", tool("head")),
        &[],
        &fake_xdotool("usage-ro-path"),
    );
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(out.status.success());
    let stderr = text(&out.stderr);
    let lines: Vec<_> = stderr
        .lines()
        .filter(|l| l.contains("usage log:"))
        .collect();
    assert_eq!(lines.len(), 1, "{stderr}");
}

#[test]
fn a_failed_send_is_not_logged() {
    // Empty PATH: no xdotool, so the send fails.
    let cache = cache_with_keymap("usage-failed-send");
    let out = run(&cache, &format!("{} -1", tool("head")), &[]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(!usage_log(&cache).exists());
}

#[test]
fn an_unreadable_log_is_one_stderr_line_and_the_flow_runs() {
    let cache = cache_with_keymap("usage-unreadable");
    std::fs::create_dir_all(usage_log(&cache)).unwrap(); // a directory where the file should be
    let out = run(&cache, &format!("{} -1", tool("head")), &["--print"]);
    assert!(out.status.success());
    assert!(text(&out.stdout).starts_with("xdotool key"));
    let stderr = text(&out.stderr);
    let lines: Vec<_> = stderr.lines().filter(|l| l.contains("usage.tsv")).collect();
    assert_eq!(lines.len(), 1, "{stderr}");
}

#[test]
fn the_fallback_picker_gets_aligned_columns() {
    let cache = cache_with_keymap("columns");
    let lines_file = cache.join("lines");
    let picker = format!("{} {}", tool("tee"), lines_file.display());
    let out = run(&cache, &picker, &["--print"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let lines = std::fs::read_to_string(&lines_file).unwrap();
    // The fixture's keys and descriptions are under the caps, so every `[provider]` starts in the same column.
    let cols: std::collections::BTreeSet<_> = lines
        .lines()
        .map(|l| l.chars().count() - "[xmonad]".chars().count())
        .collect();
    assert_eq!(cols.len(), 1, "{lines}");
    assert!(lines.lines().all(|l| l.ends_with("  [xmonad]")), "{lines}");
    // And the description column starts in the same place on every line (xmonad keys have no spaces).
    let desc_starts: std::collections::BTreeSet<_> = lines
        .lines()
        .map(|l| {
            let key = l.split(' ').next().unwrap().len();
            key + l[key..].chars().take_while(|&c| c == ' ').count()
        })
        .collect();
    assert_eq!(desc_starts.len(), 1, "{lines}");
}

/// A `PATH` with stand-ins for alacritty (runs its `-e` command in place), fzf (`fzf_body`, a `sh` script reading the
/// list on stdin) and xdotool. With `KEYBIND_FIND_PICKER` empty and no `WAYLAND_DISPLAY`, the fzf path runs headless.
fn fake_fzf(name: &str, alacritty_body: &str, fzf_body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let d = fake_xdotool(name);
    for (tool, body) in [("alacritty", alacritty_body), ("fzf", fzf_body)] {
        let p = d.join(tool);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    d
}

/// `alacritty --class … -e <exe> __fzf <list> <out>`: drop everything up to `-e`, run the rest.
const RUN_E: &str = "while [ \"$1\" != -e ]; do shift; done; shift; exec \"$@\"";

#[test]
fn the_fzf_path_maps_the_hidden_index_back() {
    let cache = cache_with_keymap("fzf-pick");
    // The third line's first (hidden) field, as `--accept-nth=1` would print it.
    let sed = tool("sed");
    // It fails unless it gets keybind-find's options and none of the user's defaults.
    let check = "case \" $* \" in *' --accept-nth=1 '*' --tiebreak=index '*) ;; *) echo \"args: $*\" >&2; exit 2 ;; esac
[ -z \"$FZF_DEFAULT_OPTS$FZF_DEFAULT_OPTS_FILE\" ] || { echo 'user defaults' >&2; exit 2; }";
    let path = fake_fzf(
        "fzf-pick-path",
        RUN_E,
        &format!("{check}\nexec {sed} -n '3s/\t.*//p'"),
    );
    let out = run_with_env(
        &cache,
        "",
        &["--print"],
        &path,
        &[
            ("FZF_DEFAULT_OPTS", "--tac"),
            ("FZF_DEFAULT_OPTS_FILE", "/x"),
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let want = run(&cache, &format!("{sed} -n 3p"), &["--print"]);
    assert!(!text(&out.stdout).is_empty());
    assert_eq!(text(&out.stdout), text(&want.stdout));
}

#[test]
fn fzf_escape_and_out_of_range_are_a_cancel() {
    let cache = cache_with_keymap("fzf-cancel");
    for (name, alacritty, fzf) in [
        ("esc", RUN_E, "exit 130"),
        ("nomatch", RUN_E, "exit 1"),
        ("range", RUN_E, "echo 99999"),
        // The window closed before fzf wrote anything.
        ("closed", "exit 0", "exit 0"),
    ] {
        let path = fake_fzf(&format!("fzf-cancel-{name}"), alacritty, fzf);
        let out = run_with_path(&cache, "", &["--print"], &path);
        assert!(out.status.success(), "{name}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "", "{name}");
    }
}

#[test]
fn fzf_and_alacritty_failures_are_errors_naming_them() {
    let cache = cache_with_keymap("fzf-fail");
    let path = fake_fzf("fzf-fail-fzf", RUN_E, "echo 'unknown option' >&2; exit 2");
    let out = run_with_path(&cache, "", &["--print"], &path);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("fzf failed") && stderr.contains("unknown option"),
        "{stderr}"
    );

    let path = fake_fzf(
        "fzf-fail-alacritty",
        "echo 'no display' >&2; exit 1",
        "exit 0",
    );
    let out = run_with_path(&cache, "", &["--print"], &path);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("alacritty failed") && stderr.contains("no display"),
        "{stderr}"
    );
}

#[test]
fn without_alacritty_the_dmenu_path_runs() {
    use std::os::unix::fs::PermissionsExt;
    let cache = cache_with_keymap("fzf-no-alacritty");
    let head = format!("{} -1", tool("head"));
    let want = text(&run(&cache, &head, &["--print"]).stdout);
    assert!(!want.is_empty());
    // Not executable, then missing: either way the stand-in dmenu (first line) is used.
    let path = fake_fzf("fzf-no-alacritty-path", "exit 3", "exit 3");
    let dmenu = path.join("dmenu");
    std::fs::write(&dmenu, format!("#!/bin/sh\nexec {head}\n")).unwrap();
    std::fs::set_permissions(&dmenu, std::fs::Permissions::from_mode(0o755)).unwrap();
    let alacritty = path.join("alacritty");
    std::fs::set_permissions(&alacritty, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = run_with_path(&cache, "", &["--print"], &path);
    assert_eq!(text(&out.stdout), want, "{}", text(&out.stderr));
    std::fs::remove_file(&alacritty).unwrap();
    let out = run_with_path(&cache, "", &["--print"], &path);
    assert_eq!(text(&out.stdout), want, "{}", text(&out.stderr));
}
