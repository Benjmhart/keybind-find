//! Hyprland: the running compositor's binds, from `hyprctl -j binds`. The config is never read: from Hyprland 0.56 it
//! is Lua, so only the compositor knows what it bound.

use crate::context::{run_for, Context};
use crate::sheet::{chord, Bind, Name, Sheet, Values};
use anyhow::Context as _;
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(1);

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let mut cmd = Command::new("hyprctl");
    cmd.args(["-j", "binds"]);
    read(cmd)
}

/// The sheet from `cmd` (`hyprctl -j binds`, with the environment the caller gives it).
fn read(cmd: Command) -> anyhow::Result<Sheet> {
    let json = run_for(cmd, TIMEOUT)?;
    let (sheet, skipped) = parse(&json)?;
    if skipped > 0 {
        eprintln!(
            "keybind-find: hyprland: leaving out {skipped} binds that sent keys can't reach (mouse, catch-all, keycode, long press, an odd key name or modmask, or a submap name we can't scope)"
        );
    }
    Ok(sheet)
}

/// Modifier bits of Hyprland's `modmask` (X's masks). Caps lock (2), Mod2 (16, num lock) and Mod3 (32) are left out:
/// they are lock states, not keys a pick presses.
const MODS: [(u64, &str); 5] = [
    (1, "shift"),
    (4, "ctrl"),
    (8, "alt"),
    (64, "super"),
    (128, "mod5"),
];

fn mods(mask: u64) -> Vec<&'static str> {
    MODS.iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, m)| *m)
        .collect()
}

/// The sheet, and how many binds were left out because no key we can send reaches them. An empty array (no binds)
/// is an empty sheet.
fn parse(json: &str) -> anyhow::Result<(Sheet, usize)> {
    let binds: Vec<serde_json::Value> =
        serde_json::from_str(json).context("parsing `hyprctl -j binds`")?;
    let mut sheet = Sheet {
        title: "hyprland".into(),
        provider: "hyprland".into(),
        binds: Default::default(),
    };
    let mut skipped = 0;
    let scope: Name = "submap".parse()?;
    for b in &binds {
        let text = |k: &str| b[k].as_str().unwrap_or_default();
        let flag = |k: &str| b[k].as_bool() == Some(true);
        // A keycode bind (`code:NN`) and a catch-all have an empty `key`; a long press can't be sent as a tap.
        let mask = b["modmask"].as_u64();
        let sym = (!flag("mouse") && !flag("catch_all") && !flag("longPress"))
            .then(|| keysym(text("key")))
            .flatten();
        let submap = match text("submap") {
            "" => Ok(None),
            s => s.parse::<Name>().map(Some),
        };
        let (Some(sym), Ok(submap), Some(mask)) = (sym, submap, mask) else {
            skipped += 1;
            continue;
        };
        let (dispatcher, arg) = dispatcher(text("dispatcher"), text("arg"));
        // An older Hyprland has no `has_description` and no `description`.
        let desc = Some(text("description"))
            .filter(|d| !d.is_empty() && b["has_description"].as_bool() != Some(false))
            .map_or_else(
                || [dispatcher, arg].join(" ").trim().to_string(),
                str::to_string,
            );
        let category = match dispatcher {
            "" | "lua function" => "binds",
            d => d,
        };
        sheet.binds.entry(category.into()).or_default().push(Bind {
            key: chord(&mods(mask), &sym),
            desc,
            action: dispatcher.into(),
            when: submap
                .map(|s| (scope.clone(), Values::from(s)))
                .into_iter()
                .collect(),
        });
    }
    Ok((sheet, skipped))
}

/// The dispatcher's name and argument as shown. Hyprland 0.56's Lua binds print `HL.Dispatcher(<name>)` with the
/// argument as a Lua reference number (meaningless to a reader), or `function: 0x…` for a plain Lua function.
fn dispatcher<'a>(dispatcher: &'a str, arg: &'a str) -> (&'a str, &'a str) {
    if let Some(name) = dispatcher
        .strip_prefix("HL.Dispatcher(")
        .and_then(|d| d.strip_suffix(')'))
        .filter(|d| !d.is_empty())
    {
        return (name, "");
    }
    if dispatcher.starts_with("function:") || dispatcher.starts_with("HL.Dispatcher(") {
        return ("lua function", "");
    }
    (dispatcher, arg)
}

/// Hyprland's key name (an XKB keysym name, matched ignoring case: `Q`, `RETURN`, `SPACE`, `XF86AudioMute`) in the
/// notation's spelling. `None` when it is empty, no keysym name, or a mouse button or wheel (`mouse:272`,
/// `mouse_down`).
/// ponytail: only common keys are put in X's case; others keep Hyprland's (`KP_ENTER` stays `KP_ENTER`): sending
/// works, since names are looked up ignoring case, but a search for `KP_Enter` misses it. Add them here.
fn keysym(key: &str) -> Option<String> {
    let lower = key.to_ascii_lowercase();
    if key.is_empty()
        || lower.starts_with("mouse")
        || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    if key.len() == 1 {
        return Some(lower);
    }
    if let Some(n) = lower
        .strip_prefix('f')
        .filter(|n| n.parse::<u8>().is_ok_and(|n| (1..=35).contains(&n)))
    {
        return Some(format!("F{n}"));
    }
    let x = match lower.as_str() {
        "return" => "Return",
        "space" => "space",
        "tab" => "Tab",
        "escape" => "Escape",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "prior" | "page_up" => "Prior",
        "next" | "page_down" => "Next",
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "print" => "Print",
        "comma" => "comma",
        "period" => "period",
        "slash" => "slash",
        "minus" => "minus",
        "equal" => "equal",
        "plus" => "plus",
        "grave" => "grave",
        _ => key,
    };
    Some(x.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/hyprland/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn find<'a>(sheet: &'a Sheet, key: &str) -> Vec<&'a Bind> {
        sheet
            .binds
            .values()
            .flatten()
            .filter(|b| b.key == key)
            .collect()
    }

    #[test]
    fn modmask_bits_alone_and_combined() {
        for (mask, want) in [
            (1, "shift+k"),
            (2, "k"),
            (4, "ctrl+k"),
            (8, "alt+k"),
            (16, "k"),
            (32, "k"),
            (64, "super+k"),
            (128, "mod5+k"),
            (65, "super+shift+k"),
            (76, "super+ctrl+alt+k"),
            (64 | 2 | 16, "super+k"),
            (255, "super+ctrl+alt+shift+mod5+k"),
        ] {
            assert_eq!(chord(&mods(mask), "k"), want, "{mask}");
        }
    }

    #[test]
    fn binds_fixture() {
        let (sheet, skipped) = parse(&fixture("binds.json")).unwrap();
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str()),
            ("hyprland", "hyprland")
        );
        // bindm, catch_all, keycode, the submap named "my map", a wheel bind, a long press, no modmask.
        assert_eq!(skipped, 7);
        // Lock bits alone are ignored like any lock bits: a plain key.
        assert_eq!(find(&sheet, "k")[0].desc, "exec caps");
        let term = find(&sheet, "super+Return");
        assert_eq!(term.len(), 1);
        assert_eq!(term[0].desc, "Open a terminal");
        assert_eq!(term[0].action, "exec");
        assert!(term[0].when.is_empty());
        assert_eq!(sheet.binds["exec"].len(), 4);
        // No description: dispatcher and argument.
        assert_eq!(find(&sheet, "super+shift+q")[0].desc, "killactive");
        assert_eq!(find(&sheet, "super+ctrl+alt+Right")[0].desc, "movewindow r");
        assert_eq!(find(&sheet, "super+ctrl+alt+Right")[0].action, "movewindow");
        // Caps lock and num lock in the mask are ignored.
        assert_eq!(find(&sheet, "super+space")[0].desc, "togglefloating");
        assert_eq!(
            find(&sheet, "XF86AudioRaiseVolume")[0].desc,
            "exec wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+"
        );
        let grow = find(&sheet, "l");
        assert_eq!(grow.len(), 1);
        assert_eq!(
            serde_json::to_string(&grow[0].when).unwrap(),
            r#"{"submap":"resize"}"#
        );
        assert_eq!(grow[0].desc, "Grow");
        // Hyprland 0.56's Lua binds.
        assert_eq!(find(&sheet, "super+e")[0].desc, "exec_cmd");
        assert_eq!(sheet.binds["exec_cmd"].len(), 2);
        assert_eq!(find(&sheet, "super+F1")[0].desc, "Show the keys");
        assert_eq!(find(&sheet, "super+x")[0].desc, "lua function");
        // `has_description` false: a leftover description isn't used.
        assert_eq!(find(&sheet, "super+d")[0].desc, "exec fuzzel");
        assert_eq!(sheet.binds["binds"].len(), 1);
    }

    #[test]
    fn older_hyprland_without_descriptions() {
        let (sheet, skipped) = parse(&fixture("binds-old.json")).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(sheet.binds.values().flatten().count(), 3);
        assert_eq!(find(&sheet, "super+q")[0].desc, "exec kitty");
        assert_eq!(find(&sheet, "super+1")[0].desc, "workspace 1");
        assert_eq!(find(&sheet, "super+shift+f")[0].desc, "fullscreen 0");
    }

    #[test]
    fn empty_and_bad_output() {
        let (sheet, skipped) = parse("[]").unwrap();
        assert!(sheet.binds.is_empty());
        assert_eq!(skipped, 0);
        assert!(parse("{}").is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn key_names() {
        for (hypr, want) in [
            ("Q", Some("q")),
            ("q", Some("q")),
            ("RETURN", Some("Return")),
            ("SPACE", Some("space")),
            ("f12", Some("F12")),
            ("XF86AudioMute", Some("XF86AudioMute")),
            ("1", Some("1")),
            ("page_up", Some("Prior")),
            ("PAGE_DOWN", Some("Next")),
            ("f0", Some("f0")),
            ("mouse_down", None),
            ("code:28", None),
            ("a-b", None),
            ("", None),
            ("mouse:272", None),
        ] {
            assert_eq!(keysym(hypr).as_deref(), want, "{hypr}");
        }
    }

    /// A real headless Hyprland with its default config; skipped when Hyprland or hyprctl isn't installed.
    #[test]
    fn real_headless_hyprland() {
        use std::process::Stdio;
        let have = |p: &str| {
            Command::new(p)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if !have("Hyprland") || !have("hyprctl") {
            eprintln!("skipping: Hyprland or hyprctl not installed");
            return;
        }
        struct KillOnDrop(std::process::Child);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        // Short: the socket path (`<runtime>/hypr/<~60-byte signature>/.socket.sock`) must fit in 108 bytes.
        let dir = std::path::PathBuf::from(format!("/tmp/kbf-h{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let runtime = dir.join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        let mut hypr = KillOnDrop(
            Command::new("Hyprland")
                .env("HYPRLAND_HEADLESS_ONLY", "1")
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("XDG_CONFIG_HOME", dir.join("config"))
                .env("HOME", &dir)
                .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("DISPLAY")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let signature = loop {
            let found = std::fs::read_dir(runtime.join("hypr"))
                .into_iter()
                .flatten()
                .flatten()
                .find(|e| e.path().join(".socket.sock").exists());
            if let Some(e) = found {
                break e.file_name();
            }
            if let Ok(Some(status)) = hypr.0.try_wait() {
                eprintln!("skipping: Hyprland can't start headless here ({status})");
                let _ = std::fs::remove_dir_all(&dir);
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no Hyprland socket in 15 s"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        // The socket can come up before the config is loaded: ask until binds appear.
        let sheet = loop {
            let mut cmd = Command::new("hyprctl");
            cmd.args(["-j", "binds"])
                .env("XDG_RUNTIME_DIR", &runtime)
                .env("HYPRLAND_INSTANCE_SIGNATURE", &signature);
            let sheet = read(cmd);
            if sheet.as_ref().is_ok_and(|s| !s.binds.is_empty())
                || std::time::Instant::now() > deadline
            {
                break sheet.unwrap();
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(sheet.binds.values().flatten().count() > 0, "{sheet:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
