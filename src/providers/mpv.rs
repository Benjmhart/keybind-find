//! mpv: the bindings of a headless mpv started with the user's config and scripts, from its `input-bindings`
//! property. Shown when an mpv window is focused (by window class: mpv is the window, not a terminal's child).

use crate::context::{run_keeping_output, Group};
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

/// Marks where the bindings start on stdout, so whatever the user's scripts print can't break the parse.
const SENTINEL: &str = "KEYBIND-FIND-MPV";

/// On mpv's `idle` event, waits for the bindings to settle, prints them on the line after the sentinel and quits, even
/// when the dump fails. `idle` comes once every script has called `mpv_wait_event`, but a Lua script does that before
/// its idle handler flushes the keys it bound (mpv 0.41 `defaults.lua`), so a script's key could still be missing
/// (1 run in 200). So it reads `input-bindings` every 50 ms until two reads match, for at most 1 s after `idle`.
/// `idle` is deprecated, but observing `idle-active` instead fires before other scripts finish loading. Its file name is
/// the script's name in mpv, so it is one no user script is likely to have.
const LUA: &str = r#"local function bindings()
  local ok, json = pcall(function()
    return require("mp.utils").format_json(mp.get_property_native("input-bindings"))
  end)
  return ok and json or nil
end
mp.register_event("idle", function()
  local last, ticks, timer = bindings(), 0, nil
  timer = mp.add_periodic_timer(0.05, function()
    local now = bindings()
    ticks = ticks + 1
    if now == last or ticks >= 20 then
      timer:kill()
      pcall(function()
        io.stdout:write("\nKEYBIND-FIND-MPV\n" .. now .. "\n")
        io.stdout:flush()
      end)
      mp.command("quit")
    end
    last = now
  end)
end)
"#;
const SCRIPT_NAME: &str = "keybind-find-dump.lua";

/// The Flatpak's own config directory, under `$HOME`.
const FLATPAK_DIR: &str = ".var/app/io.mpv.Mpv/config/mpv";

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let tmp = env("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute() && d.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let (sheet, notes) = read(TIMEOUT, |_| {}, &env, &tmp)?;
    for note in notes {
        eprintln!("keybind-find: mpv: {note}");
    }
    Ok(sheet)
}

/// Runs mpv headless with `setup` applied (a test seam: `PATH`, `--config-dir`), the dump script in a private
/// directory under `tmp` that is removed afterwards. Also returns the lines for stderr.
fn read(
    timeout: Duration,
    setup: impl Fn(&mut Command),
    env: Env,
    tmp: &Path,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    if !on_path(env) {
        bail!("mpv isn't on PATH (a Flatpak-only mpv isn't run)");
    }
    let dir = private_dir(tmp)?;
    let out = dump(timeout, &setup, env, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    parse(&out?)
}

fn dump(
    timeout: Duration,
    setup: &impl Fn(&mut Command),
    env: Env,
    dir: &Path,
) -> anyhow::Result<String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let script = dir.join(SCRIPT_NAME);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&script)
        .and_then(|mut f| f.write_all(LUA.as_bytes()))
        .with_context(|| format!("writing {}", script.display()))?;
    let mut cmd = Command::new("mpv");
    cmd.args([
        "--vo=null",
        "--ao=null",
        "--no-terminal",
        "--idle=once",
        "--msg-level=all=no",
        // The user's config may name the socket or log file of the mpv they are watching: never take those over.
        "--input-ipc-server=",
        "--log-file=",
        // A conditional profile is applied after the command line, so it could set those, or a real `vo`, again.
        "--load-auto-profiles=no",
    ])
    .arg(format!("--script={}", script.display()))
    // Through a mise shim, a pinned-but-missing mpv must not start a download in the middle of a keypress.
    .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
    // No display to open a window on, whatever the config says.
    .env_remove("DISPLAY")
    .env_remove("WAYLAND_DISPLAY")
    .env_remove("WAYLAND_SOCKET");
    if let Some(flatpak) = flatpak_config(env) {
        cmd.arg(format!("--config-dir={}", flatpak.display()));
    }
    setup(&mut cmd);
    // A process a user script started may hold stdout open after mpv quits: it is killed, and the dump kept.
    run_keeping_output(cmd, timeout, Group::Own).context("running mpv headless")
}

/// A new directory under `tmp` only this user can enter; creating it fails rather than reuse one that exists.
fn private_dir(tmp: &Path) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let dir = tmp.join(format!("keybind-find-mpv-{}-{nanos}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn on_path(env: Env) -> bool {
    env("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .any(|d| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(Path::new(d).join("mpv"))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The Flatpak's config directory, when mpv's own search finds no directory and it exists: mpv reads `$MPV_HOME`, or
/// `$XDG_CONFIG_HOME/mpv` (else `~/.config/mpv`) and the old `~/.mpv`.
fn flatpak_config(env: Env) -> Option<PathBuf> {
    let set = |k| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    // `MPV_HOME` replaces mpv's search: the user chose a directory.
    if set("MPV_HOME").is_some() {
        return None;
    }
    let home = set("HOME")?;
    let config = set("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
    if [config.join("mpv"), home.join(".mpv")]
        .iter()
        .any(|d| d.is_dir())
    {
        return None;
    }
    Some(home.join(FLATPAK_DIR)).filter(|d| d.is_dir())
}

#[derive(Deserialize)]
struct Binding {
    key: String,
    cmd: String,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    section: String,
    #[serde(default)]
    priority: i64,
    #[serde(default)]
    is_weak: bool,
}

/// The sheet from mpv's output: the JSON on the line after the last sentinel line (a JSON string can't hold a raw
/// newline, so a binding's text can't fake one). Each key once, as mpv resolves it (a negative
/// priority is an inactive section; then the highest priority, a strong binding over a weak one); the `ignore`
/// command and mouse keys are left out, and keys with no keysym, each counted in a note.
fn parse(stdout: &str) -> anyhow::Result<(Sheet, Vec<String>)> {
    let marker = format!("\n{SENTINEL}\n");
    let at = stdout
        .rfind(&marker)
        .context("mpv printed no bindings (no sentinel)")?;
    let list: Vec<Binding> = serde_json::Deserializer::from_str(&stdout[at + marker.len()..])
        .into_iter()
        .next()
        .context("mpv printed nothing after the sentinel")?
        .context("parsing mpv's input-bindings")?;
    let mut resolved: BTreeMap<&str, &Binding> = BTreeMap::new();
    for b in list.iter().filter(|b| b.priority >= 0) {
        let rank = |b: &Binding| (b.priority, !b.is_weak);
        if resolved
            .get(b.key.as_str())
            .is_none_or(|r| rank(b) > rank(r))
        {
            resolved.insert(&b.key, b);
        }
    }
    let mut sheet = Sheet {
        title: "mpv".into(),
        provider: "mpv".into(),
        binds: Default::default(),
    };
    let (mut left_out, mut unknown) = (0, 0);
    // In the order mpv listed them, which follows input.conf.
    for b in list.iter().filter(|b| {
        resolved
            .get(b.key.as_str())
            .is_some_and(|r| std::ptr::eq(*r, *b))
    }) {
        if b.cmd.trim() == "ignore" || is_mouse(&b.key) {
            left_out += 1;
            continue;
        }
        let Some(key) = key(&b.key) else {
            unknown += 1;
            continue;
        };
        let category = match b.section.as_str() {
            "default" | "" => "mpv",
            s => s,
        };
        sheet.binds.entry(category.into()).or_default().push(Bind {
            key,
            desc: b
                .comment
                .clone()
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| b.cmd.clone()),
            action: b.cmd.clone(),
            when: Default::default(),
        });
    }
    let mut notes = Vec::new();
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} mouse, wheel and `ignore` bindings"
        ));
    }
    if unknown > 0 {
        let keys = if unknown == 1 { "key" } else { "keys" };
        notes.push(format!("skipping {unknown} {keys} with no keysym we know"));
    }
    Ok((sheet, notes))
}

fn is_mouse(key: &str) -> bool {
    ["MBTN_", "WHEEL_", "MOUSE_"]
        .iter()
        .any(|m| key.contains(m))
}

/// An mpv key (`Ctrl+LEFT`, `Alt++`, the sequence `g-S`) in the key notation (`ctrl+Left`, `alt+plus`,
/// `g shift+s`). `None` when a part has no keysym we know (media keys: `PLAYPAUSE`).
fn key(s: &str) -> Option<String> {
    // A `-` separates a sequence's keys unless it is a key itself: first in a part, or after a modifier's `+`
    // (`Alt+-`; in `+-a` the `+` is a key).
    let mut parts = vec![String::new()];
    for c in s.chars() {
        let last = parts.last_mut()?;
        let after_modifier = last
            .strip_suffix('+')
            .and_then(|m| m.rsplit('+').next())
            .is_some_and(|m| {
                ["shift", "ctrl", "alt", "meta"].contains(&m.to_ascii_lowercase().as_str())
            });
        if c == '-' && !last.is_empty() && !after_modifier {
            parts.push(String::new());
        } else {
            last.push(c);
        }
    }
    let chords: Option<Vec<String>> = parts.iter().map(|p| part(p)).collect();
    Some(chords?.join(" "))
}

/// One chord: modifiers `Shift+`, `Ctrl+`, `Alt+`, `Meta+` (any case; Meta is the Super key), then the key.
fn part(s: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.get(1..).and_then(|r| r.find('+')).map(|i| i + 1) {
        let m = match rest[..i].to_ascii_lowercase().as_str() {
            "shift" => "shift",
            "ctrl" => "ctrl",
            "alt" => "alt",
            "meta" => "super",
            _ => break,
        };
        mods.push(m);
        rest = &rest[i + 1..];
    }
    let named = match rest {
        "SPACE" => "space",
        "ENTER" => "Return",
        "ESC" => "Escape",
        "TAB" => "Tab",
        "BS" => "BackSpace",
        "DEL" => "Delete",
        "INS" => "Insert",
        "HOME" => "Home",
        "END" => "End",
        "PGUP" => "Prior",
        "PGDWN" => "Next",
        "LEFT" => "Left",
        "RIGHT" => "Right",
        "UP" => "Up",
        "DOWN" => "Down",
        "SHARP" => "numbersign",
        "PRINT" => "Print",
        "MENU" => "Menu",
        "PAUSE" => "Pause",
        "CANCEL" => "Cancel",
        "KP_ENTER" => "KP_Enter",
        "KP_DEC" => "KP_Decimal",
        "KP_ADD" => "KP_Add",
        "KP_SUBTRACT" => "KP_Subtract",
        "KP_MULTIPLY" => "KP_Multiply",
        "KP_DIVIDE" => "KP_Divide",
        "KP_DEL" => "KP_Delete",
        "KP_INS" => "KP_Insert",
        "KP_HOME" => "KP_Home",
        "KP_END" => "KP_End",
        "KP_PGUP" => "KP_Prior",
        "KP_PGDWN" => "KP_Next",
        "KP_LEFT" => "KP_Left",
        "KP_RIGHT" => "KP_Right",
        "KP_UP" => "KP_Up",
        "KP_DOWN" => "KP_Down",
        "KP_BEGIN" => "KP_Begin",
        _ => "",
    };
    let keysym = if !named.is_empty() {
        named.to_string()
    } else if let Some(n) = rest
        .strip_prefix("KP")
        .filter(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
    {
        format!("KP_{n}")
    } else if let Some(n) = rest
        .strip_prefix('F')
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|n| (1..=35).contains(n))
    {
        format!("F{n}")
    } else {
        let mut chars = rest.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        // A capital is the shifted letter: `Q` is shift+q.
        if c.is_ascii_uppercase() {
            mods.push("shift");
        }
        super::nvim::single(c.to_ascii_lowercase())?.to_string()
    };
    Some(chord(&mods, &keysym))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mpv")
    }

    fn dumped(json: &str) -> String {
        format!("noise from a script\n{SENTINEL}\n{json}\n")
    }

    fn find<'a>(s: &'a Sheet, category: &str, key: &str) -> Vec<&'a Bind> {
        s.binds
            .get(category)
            .map(|b| b.iter().filter(|b| b.key == key).collect())
            .unwrap_or_default()
    }

    #[test]
    fn captured_dump_parses() {
        let json = std::fs::read_to_string(fixtures().join("input-bindings-0.41.0.json")).unwrap();
        let (s, notes) = parse(&dumped(&json)).unwrap();
        let space = find(&s, "mpv", "space");
        assert_eq!(space.len(), 1);
        assert_eq!(space[0].desc, "toggle pause/playback mode");
        assert_eq!(space[0].action, "cycle pause");
        // No comment: the command.
        assert_eq!(find(&s, "mpv", "q")[0].desc, "quit");
        assert_eq!(find(&s, "mpv", "shift+q")[0].action, "quit-watch-later");
        assert_eq!(find(&s, "mpv", "ctrl+Left")[0].action, "no-osd sub-seek -1");
        assert_eq!(find(&s, "mpv", "g shift+s").len(), 1, "a sequence");
        // `encode`'s `q` is an inactive section (priority -1): only the default `q` is listed.
        assert!(!s.binds.contains_key("encode"));
        assert!(s.binds.values().flatten().all(|b| !b.key.contains("MBTN")));
        assert!(
            notes.iter().any(|n| n.starts_with("leaving out ")),
            "{notes:?}"
        );
        assert!(notes.iter().any(|n| n.contains("no keysym")), "{notes:?}");
    }

    #[test]
    fn resolves_ignores_and_counts() {
        let json = r#"[
          {"section":"default","key":"SPACE","cmd":"cycle pause","comment":"toggle pause","priority":0,"is_weak":true},
          {"section":"default","key":"SPACE","cmd":"cycle pause","comment":"my pause","priority":15,"is_weak":false},
          {"section":"default","key":"x","cmd":"add sub-delay +0.1","priority":0,"is_weak":true},
          {"section":"default","key":"x","cmd":"ignore","priority":15,"is_weak":false},
          {"section":"default","key":"MBTN_LEFT","cmd":"cycle pause","priority":0,"is_weak":true},
          {"section":"default","key":"WHEEL_UP","cmd":"add volume 2","priority":0,"is_weak":true},
          {"section":"default","key":"PLAYPAUSE","cmd":"cycle pause","priority":0,"is_weak":true},
          {"section":"default","key":"z","cmd":"add sub-delay -0.1","comment":"","priority":0,"is_weak":true},
          {"section":"input_kbf","key":"Ctrl+k","cmd":"script-binding kbf/kbf-test","priority":4,"is_weak":true},
          {"section":"encode","key":"e","cmd":"quit 4","priority":-1,"is_weak":true},
          {"section":"default","key":"w","cmd":"weak","priority":5,"is_weak":true},
          {"section":"other","key":"w","cmd":"strong","priority":5,"is_weak":false},
          {"section":"default","key":"v","cmd":"first","priority":5,"is_weak":false},
          {"section":"default","key":"v","cmd":"second","priority":5,"is_weak":false}
        ]"#;
        let (s, notes) = parse(&dumped(json)).unwrap();
        let space = find(&s, "mpv", "space");
        assert_eq!(space.len(), 1);
        assert_eq!(space[0].desc, "my pause");
        assert!(find(&s, "mpv", "x").is_empty(), "rebound to ignore");
        assert_eq!(find(&s, "mpv", "z")[0].desc, "add sub-delay -0.1");
        assert_eq!(find(&s, "input_kbf", "ctrl+k").len(), 1);
        assert!(!s.binds.contains_key("encode"), "an inactive section");
        assert!(find(&s, "mpv", "w").is_empty());
        assert_eq!(
            find(&s, "other", "w")[0].action,
            "strong",
            "strong over weak"
        );
        // mpv lists a key once per section, so equal ranks don't happen; if they did, one bind, the first.
        assert_eq!(find(&s, "mpv", "v").len(), 1);
        assert_eq!(
            notes,
            [
                "leaving out 3 mouse, wheel and `ignore` bindings",
                "skipping 1 key with no keysym we know"
            ]
        );
    }

    #[test]
    fn bad_output_is_an_error_and_empty_is_empty() {
        assert!(parse("no sentinel here").is_err());
        assert!(parse(&dumped("{ not json")).is_err());
        assert!(parse(SENTINEL).is_err());
        // The sentinel inside a binding's text, a sentinel printed earlier, and text after the JSON.
        let json = format!(r#"[{{"key":"q","cmd":"quit","comment":"x {SENTINEL} y"}}]"#);
        let out = format!("\n{SENTINEL}\nnot json\n{}after\n", dumped(&json));
        let (s, _) = parse(&out).unwrap();
        assert_eq!(find(&s, "mpv", "q")[0].desc, format!("x {SENTINEL} y"));
        assert!(
            LUA.contains(&format!("\\n{SENTINEL}\\n")),
            "LUA writes SENTINEL"
        );
        let (s, notes) = parse(&dumped("[]")).unwrap();
        assert!(s.binds.is_empty() && notes.is_empty());
    }

    #[test]
    fn key_names() {
        for (mpv, want) in [
            ("ctrl+LEFT", Some("ctrl+Left")),
            ("Ctrl+LEFT", Some("ctrl+Left")),
            ("Shift+RIGHT", Some("shift+Right")),
            ("SPACE", Some("space")),
            ("ENTER", Some("Return")),
            ("ESC", Some("Escape")),
            ("PGUP", Some("Prior")),
            ("PGDWN", Some("Next")),
            (">", Some("greater")),
            ("q", Some("q")),
            ("Q", Some("shift+q")),
            ("Shift+q", Some("shift+q")),
            ("KP_ENTER", Some("KP_Enter")),
            ("KP1", Some("KP_1")),
            ("F1", Some("F1")),
            ("Alt++", Some("alt+plus")),
            ("Ctrl+-", Some("ctrl+minus")),
            ("-", Some("minus")),
            ("SHARP", Some("numbersign")),
            ("PRINT", Some("Print")),
            ("Meta+a", Some("super+a")),
            ("g-S", Some("g shift+s")),
            ("a--", Some("a minus")),
            ("+-a", Some("plus a")),
            ("a-+-b", Some("a plus b")),
            ("Shift+Ctrl+BS", Some("ctrl+shift+BackSpace")),
            ("PLAYPAUSE", None),
            ("g-PLAYPAUSE", None),
            ("F99", None),
        ] {
            assert_eq!(key(mpv).as_deref(), want, "{mpv}");
        }
    }

    #[test]
    fn config_directory() {
        let base = std::env::temp_dir().join(format!("kbf-mpv-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (home, xdg) = (base.join("home"), base.join("xdg"));
        let flatpak = home.join(FLATPAK_DIR);
        std::fs::create_dir_all(&flatpak).unwrap();
        std::fs::create_dir_all(&xdg).unwrap();
        let env_with = |xdg_set: bool| {
            let (home, xdg) = (home.clone(), xdg.clone());
            move |k: &str| match k {
                "HOME" => Some(home.display().to_string()),
                "XDG_CONFIG_HOME" if xdg_set => Some(xdg.display().to_string()),
                _ => None,
            }
        };
        // Neither host directory: the Flatpak's.
        assert_eq!(flatpak_config(&env_with(true)), Some(flatpak.clone()));
        assert_eq!(flatpak_config(&env_with(false)), Some(flatpak.clone()));
        // `MPV_HOME`, or the old `~/.mpv`, is the user's directory.
        let home_str = home.display().to_string();
        let mpv_home = |k: &str| match k {
            "HOME" => Some(home_str.clone()),
            "MPV_HOME" => Some("/somewhere".to_string()),
            _ => None,
        };
        assert_eq!(flatpak_config(&mpv_home), None);
        std::fs::create_dir_all(home.join(".mpv")).unwrap();
        assert_eq!(flatpak_config(&env_with(false)), None);
        std::fs::remove_dir(home.join(".mpv")).unwrap();
        // `~/.config/mpv` is not read when XDG_CONFIG_HOME is set: XDG_CONFIG_HOME's directory beats it.
        std::fs::create_dir_all(home.join(".config/mpv")).unwrap();
        assert_eq!(flatpak_config(&env_with(true)), Some(flatpak.clone()));
        assert_eq!(flatpak_config(&env_with(false)), None, "host dir wins");
        std::fs::create_dir_all(xdg.join("mpv")).unwrap();
        assert_eq!(flatpak_config(&env_with(true)), None, "host dir wins");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn no_mpv_on_path() {
        let tmp = std::env::temp_dir();
        let err = read(
            TIMEOUT,
            |_| {},
            &|k| (k == "PATH").then(|| "/nonexistent".into()),
            &tmp,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(err, "mpv isn't on PATH (a Flatpak-only mpv isn't run)");
    }

    /// A fake `mpv` in a new directory, written through `sh` (ETXTBSY: see `plugins.rs`'s `write_script`). It fails
    /// unless it has the headless flags, no display, closed stdin, and the dump script with mode 0600 in a 0700
    /// directory.
    fn fake_mpv(name: &str, body: &str) -> PathBuf {
        use std::io::Write;
        let bin = std::env::temp_dir().join(format!("kbf-mpv-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(bin.join("tmp")).unwrap();
        let script = format!(
            "#!/bin/sh\n\
             case \" $* \" in *' --vo=null --ao=null --no-terminal --idle=once '*' --input-ipc-server= --log-file= --load-auto-profiles=no '*) ;; *) exit 6 ;; esac\n\
             [ -z \"$KBF_WANT\" ] || case \" $* \" in *\" $KBF_WANT \"*) ;; *) exit 9 ;; esac\n\
             case \" $* \" in *--force-window*) exit 6 ;; esac\n\
             [ -z \"$DISPLAY$WAYLAND_DISPLAY\" ] && [ ! -t 0 ] && ! read -r _ || exit 8\n\
             for a; do case $a in --script=*) f=${{a#--script=}}; [ \"$(stat -c %a \"$f\")\" = 600 ] && [ \"$(stat -c %a \"${{f%/*}}\")\" = 700 ] || exit 7;; esac; done\n\
             {body}\n"
        );
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(bin.join("mpv"))
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

    fn run_fake(bin: &Path, timeout: Duration) -> anyhow::Result<(Sheet, Vec<String>)> {
        run_fake_in(bin, timeout, None, "")
    }

    /// `run_fake` with `HOME` set to `home`; the fake fails unless `want` is one of its arguments.
    fn run_fake_in(
        bin: &Path,
        timeout: Duration,
        home: Option<&Path>,
        want: &str,
    ) -> anyhow::Result<(Sheet, Vec<String>)> {
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let home = home.map(|h| h.display().to_string());
        let env = |k: &str| match k {
            "PATH" => Some(path.clone()),
            "HOME" => home.clone(),
            _ => None,
        };
        read(
            timeout,
            |c| {
                c.env("PATH", &path).env("KBF_WANT", want);
            },
            &env,
            &bin.join("tmp"),
        )
    }

    #[test]
    fn flatpak_config_dir_reaches_mpv() {
        let bin = fake_mpv("flatpak", &format!("printf '\\n%s\\n[]\\n' {SENTINEL}"));
        let home = bin.join("home");
        std::fs::create_dir_all(home.join(FLATPAK_DIR)).unwrap();
        let want = format!("--config-dir={}", home.join(FLATPAK_DIR).display());
        assert!(run_fake_in(&bin, TIMEOUT, Some(&home), &want).is_ok());
        // The check bites: a host directory means no `--config-dir`.
        std::fs::create_dir_all(home.join(".mpv")).unwrap();
        assert!(run_fake_in(&bin, TIMEOUT, Some(&home), &want).is_err());
        let _ = std::fs::remove_dir_all(&bin);
    }

    fn empty(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().next().is_none()
    }

    #[test]
    fn temp_script_removed_after_success_failure_and_timeout() {
        let ok = fake_mpv(
            "ok",
            &format!("printf '%s\\n' junk {SENTINEL} '[{{\"key\":\"q\",\"cmd\":\"quit\"}}]'"),
        );
        let (s, _) = run_fake(&ok, TIMEOUT).unwrap();
        assert_eq!(find(&s, "mpv", "q")[0].action, "quit");
        assert!(empty(&ok.join("tmp")));

        // A process left holding stdout (started by a user script) is killed; the dump is kept.
        let left = fake_mpv(
            "left",
            &format!("printf '\\n%s\\n[]\\n' {SENTINEL}; sleep 30 &"),
        );
        assert!(run_fake(&left, TIMEOUT).is_ok());
        assert!(empty(&left.join("tmp")));

        let fail = fake_mpv("fail", "echo broken >&2; exit 1");
        let err = format!("{:#}", run_fake(&fail, TIMEOUT).unwrap_err());
        assert!(err.contains("mpv") && err.contains("broken"), "{err}");
        assert!(empty(&fail.join("tmp")));

        let hang = fake_mpv("hang", "sleep 30");
        let err = format!(
            "{:#}",
            run_fake(&hang, Duration::from_millis(300)).unwrap_err()
        );
        assert!(err.contains("mpv did not answer within"), "{err}");
        assert!(empty(&hang.join("tmp")));
        for b in [ok, left, fail, hang] {
            let _ = std::fs::remove_dir_all(b);
        }
    }

    /// The real mpv, with no display, against a config directory: `None` (and a message) when mpv isn't installed;
    /// under `CI` that panics instead (`has_mpv`).
    fn real(config: &Path, timeout: Duration) -> Option<anyhow::Result<(Sheet, Vec<String>)>> {
        if !has_mpv() {
            return None;
        }
        let tmp = std::env::temp_dir();
        // Only PATH: no HOME, so no Flatpak `--config-dir` of the host's beside the test's.
        Some(read(
            timeout,
            |c| {
                c.arg(format!("--config-dir={}", config.display()));
            },
            &|k| (k == "PATH").then(|| std::env::var(k).ok()).flatten(),
            &tmp,
        ))
    }

    /// A cold mpv on a loaded test machine may need more than the provider's 3 s.
    const REAL_TIMEOUT: Duration = Duration::from_secs(10);

    /// Whether a working mpv is installed. When not, prints the skip message (the caller skips), except in CI (`CI`
    /// set), where it must be there.
    fn has_mpv() -> bool {
        let has = Command::new("mpv")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !has {
            assert!(
                std::env::var_os("CI").is_none(),
                "mpv is required in CI (set up in .github/workflows/ci.yml)"
            );
            eprintln!("skipping: mpv is not installed");
        }
        has
    }

    #[test]
    fn real_mpv_reads_input_conf_and_scripts() {
        let Some(r) = real(&fixtures().join("config"), REAL_TIMEOUT) else {
            return;
        };
        let (s, _) = r.unwrap();
        assert_eq!(find(&s, "mpv", "space")[0].desc, "pause from the fixture");
        assert!(find(&s, "mpv", "x").is_empty());
        assert_eq!(find(&s, "mpv", "q")[0].action, "quit");
        assert!(
            s.binds
                .values()
                .flatten()
                .any(|b| b.key == "ctrl+k" && b.action.contains("kbf-test")),
            "the script's key"
        );
    }

    #[test]
    fn real_mpv_waits_for_a_slow_scripts_key() {
        let Some(r) = real(&fixtures().join("slow-config"), REAL_TIMEOUT) else {
            return;
        };
        let (s, _) = r.unwrap();
        assert!(
            s.binds
                .values()
                .flatten()
                .any(|b| b.key == "ctrl+j" && b.action.contains("kbf-slow")),
            "the slow script's key"
        );
    }

    #[test]
    fn real_mpv_hung_script_times_out_and_junk_is_skipped() {
        if !has_mpv() {
            return;
        }
        let base = std::env::temp_dir().join(format!("kbf-mpv-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for (name, lua) in [
            ("hang", "while true do end"),
            ("junk", "io.stdout:write('junk { [ KEYBIND\\n')"),
        ] {
            std::fs::create_dir_all(base.join(name).join("scripts")).unwrap();
            std::fs::write(base.join(name).join("scripts/s.lua"), lua).unwrap();
        }
        let Some(hang) = real(&base.join("hang"), Duration::from_secs(1)) else {
            return;
        };
        let err = format!("{:#}", hang.unwrap_err());
        assert!(err.contains("mpv did not answer within"), "{err}");
        let (s, _) = real(&base.join("junk"), REAL_TIMEOUT).unwrap().unwrap();
        assert_eq!(find(&s, "mpv", "q")[0].action, "quit");
        let _ = std::fs::remove_dir_all(&base);
    }
}
