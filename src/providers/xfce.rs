//! Xfce: xfwm4's window shortcuts and the application shortcuts, from `xfconf-query`'s listing of the
//! `xfce4-keyboard-shortcuts` channel. Commands are shown, never run.

use super::nvim::single;
use crate::context::run_for;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

/// The two trees libxfce4kbd keeps shortcuts in, and their categories.
const TREES: [(&str, &str); 2] = [("xfwm4", "window manager"), ("commands", "commands")];

/// xfwm4's keys that act only while a window is moved or windows are cycled.
const MODAL: [&str; 5] = ["cancel_key", "left_key", "right_key", "up_key", "down_key"];

pub fn sheet() -> anyhow::Result<Sheet> {
    let (sheet, left_out) = read(TIMEOUT, |_| {})?;
    if left_out > 0 {
        let shortcuts = if left_out == 1 {
            "shortcut"
        } else {
            "shortcuts"
        };
        eprintln!(
            "keybind-find: xfce: leaving out {left_out} {shortcuts} keybind-find can't send (a modifier other than \
             ctrl, shift, alt and super, a keycode, or a key not shaped like a keysym name)"
        );
    }
    Ok(sheet)
}

/// Runs `xfconf-query` with `setup` applied (a test seam: `PATH`) and parses its listing.
fn read(timeout: Duration, setup: impl Fn(&mut Command)) -> anyhow::Result<(Sheet, usize)> {
    let mut cmd = Command::new("xfconf-query");
    cmd.args(["-c", "xfce4-keyboard-shortcuts", "-l", "-v"])
        .env("LC_ALL", "C.UTF-8")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let text =
        run_for(cmd, timeout).context("running xfconf-query -c xfce4-keyboard-shortcuts -l -v")?;
    // An empty channel is said on stdout, with exit status 0.
    if let Some(line) = text
        .lines()
        .find(|l| l.starts_with("Channel ") && l.contains("contains no properties"))
    {
        // Also what it says when xfconfd can't be reached (the reason goes to stderr, which exit 0 drops).
        bail!(
            "xfconf-query: {} (no shortcuts saved, or xfconfd couldn't be reached)",
            line.trim()
        );
    }
    Ok(parse(&text))
}

/// The sheet from `xfconf-query -l -v`'s lines (a property path, whitespace, the value), and how many shortcuts were
/// left out for `gtk_accel`. Each tree is read from `custom` when `/<tree>/custom/override` is true, else `default`,
/// as libxfce4kbd does.
fn parse(text: &str) -> (Sheet, usize) {
    let props: Vec<(&str, &str)> = text
        .lines()
        .map(|l| {
            let l = l.trim();
            l.split_once(char::is_whitespace)
                .map_or((l, ""), |(p, v)| (p, v.trim()))
        })
        .collect();
    let mut sheet = Sheet {
        title: "xfce".into(),
        provider: "xfce".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for (tree, category) in TREES {
        let custom = props.iter().any(|(p, v)| {
            *p == format!("/{tree}/custom/override") && v.eq_ignore_ascii_case("true")
        });
        let base = format!("/{tree}/{}/", if custom { "custom" } else { "default" });
        for (p, value) in &props {
            // Sub-properties (`<Alt>F2/startup-notify`) and the override flag aren't shortcuts; nor is an empty
            // value (no action).
            let Some(accel) = p.strip_prefix(&base) else {
                continue;
            };
            if accel.contains('/') || accel == "override" || value.is_empty() {
                continue;
            }
            let desc = if tree == "xfwm4" {
                if MODAL.contains(value) {
                    continue;
                }
                value
                    .strip_suffix("_key")
                    .unwrap_or(value)
                    .replace('_', " ")
            } else {
                value.to_string()
            };
            let Some(key) = gtk_accel(accel) else {
                left_out += 1;
                continue;
            };
            sheet.binds.entry(category.into()).or_default().push(Bind {
                key,
                desc,
                action: value.to_string(),
                when: Default::default(),
            });
        }
    }
    (sheet, left_out)
}

/// A GTK accelerator (`<Primary><Alt>Delete`) in keybind-find's notation (`ctrl+alt+Delete`); `None` for `<Meta>`,
/// `<Hyper>`, `<Release>`, a `<ModN>` other than 1 and 4, a keycode, or a key not shaped like a keysym name. Modifier
/// names in any case, as GTK's `gtk_accelerator_parse`; a single letter is lower case.
// ponytail: no keysym table; any well-formed name (a letter, then letters, digits and `_`) passes as a keysym.
pub(crate) fn gtk_accel(accel: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut rest = accel;
    while let Some(r) = rest.strip_prefix('<') {
        let (name, after) = r.split_once('>')?;
        mods.push(match name.to_ascii_lowercase().as_str() {
            "primary" | "control" | "ctrl" | "ctl" => "ctrl",
            "shift" | "shft" => "shift",
            "alt" | "mod1" => "alt",
            "super" | "mod4" => "super",
            _ => return None,
        });
        rest = after;
    }
    let mut chars = rest.chars();
    let key = match (chars.next(), chars.next()) {
        (None, _) => return None,
        (Some(c), None) if c.is_ascii() => single(c.to_ascii_lowercase())?.to_string(),
        (Some(c), _)
            if c.is_ascii_alphabetic()
                && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
        {
            rest.to_string()
        }
        _ => return None,
    };
    Some(chord(&mods, &key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// `xfce4-keyboard-shortcuts.xml` is libxfce4ui's `libxfce4kbd-private/xfce4-keyboard-shortcuts.xml` at
    /// `488b306`; `list-default.txt` is `xfconf-query -l -v` of it (padded as `xfconf_query_list_contents` pads).
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/xfce")
                .join(name),
        )
        .unwrap()
    }

    fn rows<'a>(s: &'a Sheet, category: &str) -> Vec<(&'a str, &'a str)> {
        s.binds
            .get(category)
            .map(|b| {
                b.iter()
                    .map(|b| (b.key.as_str(), b.desc.as_str()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn upstream_defaults() {
        let text = fixture("list-default.txt");
        let (s, left_out) = parse(&text);
        assert_eq!(left_out, 0);
        assert_eq!(s.title, "xfce");
        assert_eq!(
            s.binds.keys().collect::<Vec<_>>(),
            ["commands", "window manager"]
        );
        // Every default is listed under its tree, except the startup-notify lines and the modal keys.
        let shortcuts = |tree: &str| {
            text.lines()
                .filter(|l| {
                    l.starts_with(&format!("/{tree}/default/")) && !l.contains("startup-notify")
                })
                .count()
        };
        let commands = rows(&s, "commands");
        let wm = rows(&s, "window manager");
        assert_eq!(commands.len(), shortcuts("commands"));
        assert_eq!(wm.len(), shortcuts("xfwm4") - MODAL.len());
        assert_eq!((commands.len(), wm.len()), (21, 55));
        assert!(commands.contains(&("alt+F2", "xfce4-appfinder --collapsed")));
        assert!(commands.contains(&("ctrl+alt+Delete", "xfce4-session-logout")));
        assert!(commands.contains(&("super+l", "xflock4")));
        assert!(commands.contains(&("XF86Display", "xfce4-display-settings --minimal")));
        assert!(commands.contains(&("Print", "xfce4-screenshooter")));
        assert!(wm.contains(&("alt+F4", "close window")));
        assert!(wm.contains(&("ctrl+alt+KP_1", "move window workspace 1")));
        assert!(wm.contains(&("super+KP_Next", "tile down right")));
        assert!(wm
            .iter()
            .all(|(k, _)| !["Escape", "Left", "Right", "Up", "Down"].contains(k)));
        let close = &s.binds["window manager"]
            .iter()
            .find(|b| b.key == "alt+F4")
            .unwrap();
        assert_eq!(close.action, "close_window_key");
    }

    #[test]
    fn custom_over_default_per_tree() {
        let (s, left_out) = parse(&fixture("list-custom.txt"));
        // Commands from custom (override true): the removed default (<Super>l) is gone, <Hyper> left out.
        assert_eq!(
            rows(&s, "commands"),
            [
                ("alt+F2", "xfce4-appfinder --collapsed"),
                ("ctrl+alt+Delete", "xfce4-session-logout"),
                ("super+t", "sh -c 'kitty -- tmux new -A -s main'"),
            ]
        );
        // xfwm4 from default (override false), though custom has entries: <Release> left out, Escape modal.
        assert_eq!(
            rows(&s, "window manager"),
            [
                ("alt+F4", "close window"),
                ("ctrl+alt+KP_1", "move window workspace 1"),
            ]
        );
        assert_eq!(left_out, 2);
        // `/providers` and anything outside the trees: nothing.
        assert_eq!(s.binds.len(), 2);
        assert_eq!(
            parse("/providers  [commands,xfwm4]\n/other/default/a  x\n\n")
                .0
                .binds
                .len(),
            0
        );
        // override in any case; true with no custom entries is an empty set, as in Xfce.
        let commands = |t: &str| rows(&parse(t).0, "commands").len();
        assert_eq!(
            commands("/commands/custom/<Super>x  a\n/commands/custom/override  TRUE\n"),
            1
        );
        assert_eq!(
            commands("/commands/custom/override  true\n/commands/default/<Super>x  a\n"),
            0
        );
        // An empty value is no action: nothing.
        assert_eq!(parse("/commands/default/<Super>x\n").0.binds.len(), 0);
    }

    #[test]
    fn gtk_accelerators() {
        for (accel, want) in [
            ("<Primary>a", Some("ctrl+a")),
            ("<Control>a", Some("ctrl+a")),
            ("<Ctrl>a", Some("ctrl+a")),
            ("<Ctl>a", Some("ctrl+a")),
            ("<PRIMARY>a", Some("ctrl+a")),
            ("<shift>a", Some("shift+a")),
            ("<Shft>a", Some("shift+a")),
            ("<Alt>a", Some("alt+a")),
            ("<Mod1>a", Some("alt+a")),
            ("<super>a", Some("super+a")),
            ("<Mod4>a", Some("super+a")),
            ("<Primary><Shift>a", Some("ctrl+shift+a")),
            ("<Shift><Primary>a", Some("ctrl+shift+a")),
            ("<Super>A", Some("super+a")),
            ("XF86Mail", Some("XF86Mail")),
            ("<Primary><Alt>KP_1", Some("ctrl+alt+KP_1")),
            ("<Alt>space", Some("alt+space")),
            ("<Super>1", Some("super+1")),
            // A single character is named as a keysym; GTK writes `slash`, a hand edit `/`.
            ("<Super>/", Some("super+slash")),
            ("<Control><Primary>a", Some("ctrl+a")),
            // The shape rule (AC #3): a letter, then letters, digits and `_`; real digit-first keysyms are refused.
            ("<Super>Foo-Bar", None),
            ("<Super>a$", None),
            ("3270_Enter", None),
            ("<Meta>a", None),
            ("<Hyper>a", None),
            ("<Release>a", None),
            ("<Mod2>a", None),
            ("<Super>", None),
            ("<Foo>x", None),
            ("0x26", None),
            ("<Super>é", None),
            ("<Super", None),
            ("", None),
        ] {
            assert_eq!(gtk_accel(accel).as_deref(), want, "{accel}");
        }
    }

    /// A stand-in `xfconf-query` running `body`, in its own directory; returns the directory.
    fn fake(name: &str, body: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!("kbf-xfce-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process: a write handle another test thread's fork inherited would make
        // running the script fail with ETXTBSY (as `plugins.rs`'s `write_script` says).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("xfconf-query"))
            .arg(body)
            .status()
            .unwrap();
        assert!(ok.success());
        bin
    }

    #[test]
    fn failures_are_one_error() {
        let path = |bin: &Path| {
            let bin = bin.to_path_buf();
            move |c: &mut Command| {
                c.env("PATH", &bin);
            }
        };
        let empty = r#"echo 'Channel "xfce4-keyboard-shortcuts" contains no properties.'"#;
        // Exit 1 with the message on stderr, or exit 0 with it on stdout (as xfconf-query does): an error either way.
        for (name, body) in [
            ("fail", format!("{empty} >&2; exit 1")),
            ("empty", empty.to_string()),
        ] {
            let bin = fake(name, &body);
            let err = read(TIMEOUT, path(&bin)).unwrap_err();
            assert!(
                format!("{err:#}").contains("contains no properties"),
                "{err:#}"
            );
            let _ = std::fs::remove_dir_all(&bin);
        }
        // PATH holds only the fake: `sleep` by its path.
        let bin = fake("hang", "exec /bin/sleep 10");
        let start = std::time::Instant::now();
        assert!(read(Duration::from_millis(300), path(&bin)).is_err());
        // It waited for the timeout, not for a script that failed to start.
        assert!(start.elapsed() >= Duration::from_millis(300));
        assert!(start.elapsed() < Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(&bin);
        // Not installed.
        let none = std::env::temp_dir().join(format!("kbf-xfce-none-{}", std::process::id()));
        assert!(read(TIMEOUT, path(&none)).is_err());
        // A listing is read through the seam.
        let bin = fake(
            "ok",
            // PATH holds only the fake: `cat` by its path.
            &format!(
                "[ \"$*|$LC_ALL\" = '-c xfce4-keyboard-shortcuts -l -v|C.UTF-8' ] || exit 9; /bin/cat '{}'",
                fixture_path("list-custom.txt").display()
            ),
        );
        let (s, left_out) = read(TIMEOUT, path(&bin)).unwrap();
        assert_eq!((rows(&s, "commands").len(), left_out), (3, 2));
        let _ = std::fs::remove_dir_all(&bin);
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xfce")
            .join(name)
    }

    /// The real `xfconf-query` on a private D-Bus session, with the upstream defaults as the system channel file
    /// (xfconfd reads `$XDG_CONFIG_DIRS/xfce4/xfconf/xfce-perchannel-xml/`, through libxfce4util); if that finds no
    /// channel, the same file as the user's. Skipped, with a message, without `xfconf-query` or `dbus-run-session`,
    /// or when xfconfd can't start; an empty channel either way fails.
    #[test]
    fn real_xfconf_query() {
        let has = |p: &str, arg: &str| {
            Command::new(p)
                .arg(arg)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !has("xfconf-query", "--version") || !has("dbus-run-session", "--version") {
            eprintln!("skipping: xfconf-query or dbus-run-session is not installed");
            return;
        }
        let tmp = std::env::temp_dir().join(format!("kbf-xfce-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let list = |under: &str| {
            let channels = tmp.join(under).join("xfce4/xfconf/xfce-perchannel-xml");
            std::fs::create_dir_all(&channels).unwrap();
            std::fs::create_dir_all(tmp.join("home")).unwrap();
            std::fs::copy(
                fixture_path("xfce4-keyboard-shortcuts.xml"),
                channels.join("xfce4-keyboard-shortcuts.xml"),
            )
            .unwrap();
            let mut cmd = Command::new("dbus-run-session");
            cmd.args([
                "--",
                "xfconf-query",
                "-c",
                "xfce4-keyboard-shortcuts",
                "-l",
                "-v",
            ])
            .env("HOME", tmp.join("home"))
            .env("XDG_CONFIG_HOME", tmp.join("config"))
            .env("XDG_CONFIG_DIRS", tmp.join("dirs"))
            .env("LC_ALL", "C.UTF-8");
            run_for(cmd, Duration::from_secs(10))
        };
        let mut text = match list("dirs") {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skipping: xfconfd didn't start on a private bus: {e:#}");
                let _ = std::fs::remove_dir_all(&tmp);
                return;
            }
        };
        if text.contains("contains no properties") {
            eprintln!("xfconfd didn't read XDG_CONFIG_DIRS; trying XDG_CONFIG_HOME");
            std::fs::remove_dir_all(tmp.join("dirs")).unwrap();
            text = list("config").unwrap();
        }
        let (s, _) = parse(&text);
        assert!(
            rows(&s, "window manager").contains(&("alt+F4", "close window")),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
