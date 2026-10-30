//! GNOME: the window manager's, the shell's, mutter's and the media keys' shortcuts, the custom shortcuts and the
//! overlay key, from `gsettings`. Commands are shown, never run.

use super::xfce::gtk_accel;
use crate::context::run_for;
use crate::sheet::{Bind, Sheet};
use anyhow::{anyhow, Context as _};
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

/// The schemas read, and their categories.
const SCHEMAS: [(&str, &str); 5] = [
    ("org.gnome.desktop.wm.keybindings", "window manager"),
    ("org.gnome.shell.keybindings", "shell"),
    ("org.gnome.mutter.keybindings", "mutter"),
    ("org.gnome.mutter.wayland.keybindings", "mutter"),
    ("org.gnome.settings-daemon.plugins.media-keys", "media keys"),
];
const MEDIA: &str = "org.gnome.settings-daemon.plugins.media-keys";
/// The relocatable schema each custom shortcut's path is read with.
const CUSTOM: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";

pub fn sheet() -> anyhow::Result<Sheet> {
    let (sheet, notes) = read(|_| {})?;
    if !notes.is_empty() {
        eprintln!("keybind-find: gnome: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `gsettings list-recursively <schema>`'s text, with `setup` applied (a test seam: `PATH`, `GSETTINGS_SCHEMA_DIR`).
/// A schema given as `<id>:<path>` is a relocatable one at that path.
pub(crate) fn list_recursively(
    schema: &str,
    setup: impl Fn(&mut Command),
) -> anyhow::Result<String> {
    gsettings(&["list-recursively", schema], setup)
}

fn gsettings(args: &[&str], setup: impl Fn(&mut Command)) -> anyhow::Result<String> {
    let mut cmd = Command::new("gsettings");
    cmd.args(args)
        .env("LC_ALL", "C.UTF-8")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    run_for(cmd, TIMEOUT).with_context(|| format!("running gsettings {}", args.join(" ")))
}

/// Each line of `list-recursively`'s text as (schema, key, raw value); a line without all three is skipped.
pub(crate) fn settings(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, ' ');
            let (schema, key, value) = (parts.next()?, parts.next()?, parts.next()?);
            Some((schema.into(), key.into(), value.into()))
        })
        .collect()
}

/// A GVariant string (`'<Super>a'`, or double-quoted, with backslash escapes) or string array (`['a', 'b']`,
/// `@as []`) as printed by `gsettings`, decoded; `None` for any other value.
pub(crate) fn strings(value: &str) -> Option<Vec<String>> {
    let mut v = value.trim();
    if let Some(rest) = v.strip_prefix('@') {
        let (ty, rest) = rest.split_once(' ')?;
        if ty != "as" && ty != "s" {
            return None;
        }
        v = rest;
    }
    let mut chars = v.chars().peekable();
    fn skip(c: &mut std::iter::Peekable<std::str::Chars<'_>>) {
        while c.next_if_eq(&' ').is_some() {}
    }
    let list = chars.next_if_eq(&'[').is_some();
    let mut out = Vec::new();
    skip(&mut chars);
    if !(list && chars.next_if_eq(&']').is_some()) {
        loop {
            skip(&mut chars);
            out.push(string(&mut chars)?);
            if !list {
                break;
            }
            skip(&mut chars);
            match chars.next()? {
                ',' => {}
                ']' => break,
                _ => return None,
            }
        }
    }
    skip(&mut chars);
    chars.next().is_none().then_some(out)
}

/// One quoted GVariant string, as `g_variant_print` writes it.
fn string(chars: &mut impl Iterator<Item = char>) -> Option<String> {
    let quote = chars.next().filter(|c| *c == '\'' || *c == '"')?;
    let mut out = String::new();
    loop {
        let c = chars.next()?;
        if c == quote {
            return Some(out);
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        let hex = |chars: &mut dyn Iterator<Item = char>, n| {
            let digits: String = chars.take(n).collect();
            (digits.len() == n)
                .then(|| u32::from_str_radix(&digits, 16).ok())
                .flatten()
                .and_then(char::from_u32)
        };
        out.push(match chars.next()? {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'a' => '\x07',
            'b' => '\x08',
            'f' => '\x0c',
            'v' => '\x0b',
            'u' => hex(chars, 4)?,
            'U' => hex(chars, 8)?,
            c => c,
        });
    }
}

/// A key's name as its description: `switch-to-workspace-left` → `switch to workspace left`.
pub(crate) fn describe(key: &str) -> String {
    key.replace('-', " ")
}

/// The sheet and the notes for stderr (schemas not installed or failing, shortcuts left out). An error when no
/// schema could be read (`gsettings` missing, or every schema failing).
fn read(setup: impl Fn(&mut Command)) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut sheet = Sheet {
        title: "gnome".into(),
        provider: "gnome".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    let (mut missing, mut failed) = (Vec::<String>::new(), Vec::new());
    let mut first_err = None;
    let mut read_any = false;
    let mut push = |category: &str, key, desc, action| {
        sheet.binds.entry(category.into()).or_default().push(Bind {
            key,
            desc,
            action,
            when: Default::default(),
        })
    };
    // Notes the failure; true when gsettings hung (a stuck dconf or D-Bus), which every later run would too.
    let mut fail = |schema: &str, e: anyhow::Error| {
        let why = format!("{e:#}");
        if why.contains("No such schema") {
            if !missing.iter().any(|m| m == schema) {
                missing.push(schema.to_string());
            }
            return false;
        }
        let hung = why.contains("did not answer within");
        failed.push(if hung {
            format!("{why} (stopped reading GNOME's settings there)")
        } else {
            why
        });
        first_err.get_or_insert(e);
        hung
    };
    let mut hung = false;
    'schemas: for (schema, category) in SCHEMAS {
        let text = match list_recursively(schema, &setup) {
            Ok(t) => t,
            Err(e) => {
                hung = fail(schema, e);
                if hung {
                    break;
                }
                continue;
            }
        };
        read_any = true;
        for (s, key, value) in settings(&text) {
            // A child schema's lines carry the child's id; custom paths are read below; `*-static` are hardware keys.
            if s != schema || key == "custom-keybindings" || key.ends_with("-static") {
                continue;
            }
            for accel in strings(&value).unwrap_or_default() {
                match accel_key(&accel) {
                    Some(Some(k)) => push(category, k, describe(&key), key.clone()),
                    Some(None) => {}
                    None => left_out += 1,
                }
            }
        }
        if schema != MEDIA {
            continue;
        }
        let paths = settings(&text)
            .into_iter()
            .find(|(s, k, _)| s == MEDIA && k == "custom-keybindings")
            .and_then(|(_, _, v)| strings(&v))
            .unwrap_or_default();
        for (i, path) in paths.iter().enumerate() {
            // A path listed twice is one shortcut.
            if paths[..i].contains(path) {
                continue;
            }
            // gsettings' own rule for a path: `/`-delimited, no empty segment.
            if !path.starts_with('/') || !path.ends_with('/') || path.contains("//") {
                left_out += 1;
                continue;
            }
            let text = match list_recursively(&format!("{CUSTOM}:{path}"), &setup) {
                Ok(t) => t,
                Err(e) => {
                    hung = fail(CUSTOM, e);
                    if hung {
                        break 'schemas;
                    }
                    continue;
                }
            };
            let get = |key: &str| {
                settings(&text)
                    .into_iter()
                    .find(|(s, k, _)| s == CUSTOM && k == key)
                    .and_then(|(_, _, v)| strings(&v))
                    .and_then(|v| v.into_iter().next())
                    .unwrap_or_default()
            };
            let (name, command) = (get("name"), get("command"));
            match accel_key(&get("binding")) {
                Some(Some(k)) => {
                    let desc = [&name, &command]
                        .into_iter()
                        .find(|d| !d.is_empty())
                        .map_or_else(|| "custom shortcut".into(), |d| d.clone());
                    push("custom", k, desc, command)
                }
                Some(None) => {}
                None => left_out += 1,
            }
        }
    }
    if !read_any {
        return Err(first_err.unwrap_or_else(|| {
            anyhow!(
                "gsettings: no GNOME keybinding schema is installed ({})",
                missing.join(", ")
            )
        }));
    }
    if !hung {
        match gsettings(&["get", "org.gnome.mutter", "overlay-key"], &setup) {
            Ok(v) => {
                let key = strings(&v)
                    .and_then(|v| v.into_iter().next())
                    .unwrap_or_default();
                // mutter takes `Super` for either Super key.
                let key = if key == "Super" {
                    "Super_L".into()
                } else {
                    key
                };
                match accel_key(&key) {
                    Some(Some(k)) => push(
                        "shell",
                        k,
                        "overlay key (activities)".into(),
                        "overlay-key".into(),
                    ),
                    Some(None) => {}
                    None => left_out += 1,
                }
            }
            Err(e) => {
                fail("org.gnome.mutter", e);
            }
        }
    }
    let mut notes = Vec::new();
    if !missing.is_empty() {
        notes.push(format!("schema not installed: {}", missing.join(", ")));
    }
    notes.extend(failed);
    if left_out > 0 {
        let shortcuts = if left_out == 1 {
            "shortcut"
        } else {
            "shortcuts"
        };
        notes.push(format!(
            "leaving out {left_out} {shortcuts} keybind-find can't send (mutter's Above_Tab, a modifier other than \
             ctrl, shift, alt and super, a key not shaped like a keysym name, or a custom shortcut path that isn't \
             one)"
        ));
    }
    Ok((sheet, notes))
}

/// An accelerator as a key: `Some(Some(key))`, `Some(None)` for none set (`''`, `disabled`), `None` when it can't be
/// listed (mutter's `Above_Tab`, or anything `gtk_accel` refuses).
fn accel_key(accel: &str) -> Option<Option<String>> {
    if accel.is_empty() || accel == "disabled" {
        return Some(None);
    }
    gtk_accel(accel)
        .filter(|k| {
            !k.rsplit('+')
                .next()
                .is_some_and(|k| k.eq_ignore_ascii_case("Above_Tab"))
        })
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gnome")
            .join(name)
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

    /// A stand-in `gsettings` running `body`, in its own directory; returns the directory.
    fn fake(name: &str, body: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!("kbf-gnome-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process (ETXTBSY: see `xfce.rs`'s `fake`).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("gsettings"))
            .arg(body)
            .status()
            .unwrap();
        assert!(ok.success());
        bin
    }

    fn path(bin: &Path) -> impl Fn(&mut Command) {
        let bin = bin.to_path_buf();
        move |c: &mut Command| {
            c.env("PATH", &bin);
        }
    }

    /// The fake's answer for each schema: `cat` of a fixture, else what `case` lines `extra` give, else
    /// `No such schema`, exit 1.
    fn answers(name: &str, wm: &str, extra: &str) -> PathBuf {
        fake(
            name,
            &format!(
                "[ \"$LC_ALL\" = C.UTF-8 ] || exit 9\ncase \"$*\" in\n\
                 'list-recursively org.gnome.desktop.wm.keybindings') /bin/cat '{}';;\n{extra}\n\
                 *) echo \"No such schema “$2”\" >&2; exit 1;;\nesac",
                fixture(wm).display()
            ),
        )
    }

    #[test]
    fn upstream_wm_defaults() {
        let bin = answers("wm", "list-wm.txt", "");
        let (s, notes) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert_eq!(s.title, "gnome");
        let wm = rows(&s, "window manager");
        assert!(wm.contains(&("alt+F4", "close")));
        assert!(wm.contains(&("ctrl+alt+Left", "switch to workspace left")));
        assert!(wm.contains(&("super+Page_Up", "switch to workspace left")));
        assert!(wm.contains(&("alt+space", "activate window menu")));
        assert!(wm.iter().all(|(k, _)| !k.contains("Above_Tab")));
        // Every accelerator of the 84 keys is listed but the four Above_Tab ones; `@as []` and `['']` list nothing.
        let all: usize = settings(&std::fs::read_to_string(fixture("list-wm.txt")).unwrap())
            .iter()
            .map(|(_, _, v)| strings(v).unwrap().iter().filter(|a| !a.is_empty()).count())
            .sum();
        assert_eq!(wm.len(), all - 4);
        assert_eq!(s.binds.len(), 1);
        // The other schemas aren't installed: named, with the Above_Tab count, in one line's notes.
        assert_eq!(
            notes[0],
            "schema not installed: org.gnome.shell.keybindings, org.gnome.mutter.keybindings, \
             org.gnome.mutter.wayland.keybindings, org.gnome.settings-daemon.plugins.media-keys, org.gnome.mutter"
        );
        assert!(notes[1].starts_with("leaving out 4 shortcuts"), "{notes:?}");
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn mixed_values() {
        let bin = answers(
            "mixed",
            "list-wm.txt",
            &format!(
                "'list-recursively org.gnome.settings-daemon.plugins.media-keys') /bin/cat '{}';;",
                fixture("list-mixed.txt").display()
            ),
        );
        let (s, notes) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert_eq!(
            rows(&s, "media keys"),
            [
                ("super+F1", "help"),
                ("super+e", "home"),
                ("ctrl+shift+q", "logout"),
                ("super+apostrophe", "screensaver"),
                ("XF86AudioMute", "volume mute"),
            ]
        );
        // Above_Tab ×4 from wm, then mixed's Above_Tab and `<Hyper>`.
        assert!(notes.last().unwrap().starts_with("leaving out 6 shortcuts"));
    }

    #[test]
    fn custom_shortcuts_and_the_overlay_key() {
        let p = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings";
        let media = format!(
            "{MEDIA} custom-keybindings ['{p}/custom0/', '{p}/custom1/', 'bad', '{p}//x/', '{p}/gone/', \
             '{p}/custom0/', '{p}/hyper/', '{p}/unset/', '{p}/blank/']\n\
             {MEDIA} volume-up ['XF86AudioRaiseVolume']"
        );
        let custom = |name: &str, binding: &str, command: &str| {
            format!(
                "echo \"{CUSTOM} binding '{binding}'\"; echo \"{CUSTOM} command '{command}'\"; \
                 echo \"{CUSTOM} enable-in-lockscreen false\"; echo \"{CUSTOM} name '{name}'\";;"
            )
        };
        let bin = answers(
            "custom",
            "list-wm.txt",
            &format!(
                "'list-recursively {MEDIA}') printf '%s\\n' \"{media}\";;\n\
                 'list-recursively {CUSTOM}:{p}/custom0/') {}\n\
                 'list-recursively {CUSTOM}:{p}/custom1/') {}\n\
                 'list-recursively {CUSTOM}:{p}/gone/') echo 'dconf: boom' >&2; exit 1;;\n\
                 'list-recursively {CUSTOM}:{p}/hyper/') {}\n\
                 'list-recursively {CUSTOM}:{p}/unset/') {}\n\
                 'list-recursively {CUSTOM}:{p}/blank/') {}\n\
                 'get org.gnome.mutter overlay-key') echo \"'Super'\";;",
                custom("Terminal", "<Super>Return", "kgx"),
                custom("", "<Primary><Alt>b", "firefox --new-window"),
                custom("Hyper", "<Hyper>x", "x"),
                custom("Unset", "", "y"),
                custom("", "<Super>F9", ""),
            ),
        );
        let (s, notes) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert_eq!(
            rows(&s, "custom"),
            [
                ("super+Return", "Terminal"),
                ("ctrl+alt+b", "firefox --new-window"),
                ("super+F9", "custom shortcut"),
            ]
        );
        assert_eq!(s.binds["custom"][0].action, "kgx");
        assert_eq!(
            rows(&s, "media keys"),
            [("XF86AudioRaiseVolume", "volume up")]
        );
        assert_eq!(rows(&s, "shell"), [("Super_L", "overlay key (activities)")]);
        // The path gsettings refused is a note with its reason; Above_Tab ×4, `bad`, `//` and `<Hyper>` are counted;
        // the repeated path is listed once, and an empty binding lists nothing.
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert!(notes[1].contains("dconf: boom"), "{notes:?}");
        assert!(notes[2].starts_with("leaving out 7 shortcuts"), "{notes:?}");
        // The overlay key set to nothing lists nothing; another key is listed as itself.
        for (overlay, want) in [
            ("''", vec![]),
            ("'Super_R'", vec![("Super_R", "overlay key (activities)")]),
        ] {
            let bin = answers(
                "overlay",
                "list-wm.txt",
                &format!("'get org.gnome.mutter overlay-key') echo \"{overlay}\";;"),
            );
            let (s, _) = read(path(&bin)).unwrap();
            let _ = std::fs::remove_dir_all(&bin);
            assert_eq!(rows(&s, "shell"), want);
        }
    }

    #[test]
    fn failures() {
        // gsettings not installed: one error.
        let none = std::env::temp_dir().join(format!("kbf-gnome-none-{}", std::process::id()));
        let e = read(path(&none)).unwrap_err();
        assert!(format!("{e:#}").contains("running gsettings"), "{e:#}");
        // No schema installed: one error naming them.
        let bin = fake("noschema", "echo \"No such schema “$2”\" >&2; exit 1");
        let e = read(path(&bin)).unwrap_err();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(
            format!("{e:#}").contains("no GNOME keybinding schema is installed"),
            "{e:#}"
        );
        // Another failure is kept in the notes, the rest still listed.
        let bin = answers(
            "other",
            "list-wm.txt",
            "'list-recursively org.gnome.shell.keybindings') echo boom >&2; exit 1;;",
        );
        let (s, notes) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(!rows(&s, "window manager").is_empty());
        assert!(
            notes
                .iter()
                .any(|n| n.contains("org.gnome.shell.keybindings") && n.contains("boom")),
            "{notes:?}"
        );
        // A gsettings that hangs stops the reading: no later run waits its 2 s too.
        let bin = answers(
            "hang",
            "list-wm.txt",
            "'list-recursively org.gnome.shell.keybindings') exec /bin/sleep 10;;\n\
             'list-recursively org.gnome.mutter.keybindings') exec /bin/sleep 10;;",
        );
        let start = std::time::Instant::now();
        let (s, notes) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(start.elapsed() >= TIMEOUT && start.elapsed() < TIMEOUT * 2);
        assert!(!rows(&s, "window manager").is_empty());
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("did not answer within"), "{notes:?}");
        assert!(
            notes[0].ends_with("(stopped reading GNOME's settings there)"),
            "{notes:?}"
        );
        // A hang on a custom path stops it too: no wait for the next path or the overlay key.
        let p = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings";
        let bin = answers(
            "hangcustom",
            "list-wm.txt",
            &format!(
                "'list-recursively {MEDIA}') echo \"{MEDIA} custom-keybindings ['{p}/a/', '{p}/b/']\";;\n\
                 'list-recursively {CUSTOM}:'*) exec /bin/sleep 10;;\n\
                 'get org.gnome.mutter overlay-key') echo \"'Super'\";;"
            ),
        );
        let start = std::time::Instant::now();
        let (s, notes) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(start.elapsed() >= TIMEOUT && start.elapsed() < TIMEOUT * 2);
        assert!(rows(&s, "shell").is_empty(), "{notes:?}");
        // The relocatable schema missing for two paths: named once.
        let bin = answers(
            "nocustom",
            "list-wm.txt",
            &format!(
                "'list-recursively {MEDIA}') echo \"{MEDIA} custom-keybindings ['{p}/a/', '{p}/b/']\";;\n\
                 'list-recursively {CUSTOM}:'*) echo \"No such schema “{CUSTOM}”\" >&2; exit 1;;"
            ),
        );
        let (_, notes) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert_eq!(notes[0].matches(CUSTOM).count(), 1, "{notes:?}");
    }

    #[test]
    fn each_schema_has_its_category() {
        let line = |schema: &str, key: &str, value: &str| {
            format!("'list-recursively {schema}') echo \"{schema} {key} {value}\";;")
        };
        let bin = answers(
            "categories",
            "list-wm.txt",
            &[
                line(
                    "org.gnome.shell.keybindings",
                    "toggle-application-view",
                    "['<Super>a']",
                ),
                line(
                    "org.gnome.mutter.keybindings",
                    "toggle-tiled-left",
                    "['<Super>Left']",
                ),
                line(
                    "org.gnome.mutter.wayland.keybindings",
                    "restore-shortcuts",
                    "['<Super>Escape']",
                ),
            ]
            .join("\n"),
        );
        let (s, _) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert_eq!(rows(&s, "shell"), [("super+a", "toggle application view")]);
        assert_eq!(
            rows(&s, "mutter"),
            [
                ("super+Left", "toggle tiled left"),
                ("super+Escape", "restore shortcuts")
            ]
        );
        assert!(rows(&s, "window manager").contains(&("alt+F4", "close")));
    }

    #[test]
    fn gvariant_strings() {
        let s = |v: &str| strings(v).map(|v| v.join("|"));
        for (value, want) in [
            ("'<Super>a'", Some("<Super>a")),
            ("\"it's\"", Some("it's")),
            ("'it\\'s'", Some("it's")),
            ("'a\\\\b'", Some("a\\b")),
            ("'tab\\there'", Some("tab\there")),
            ("'\\u00e9'", Some("é")),
            ("\"a\\\"b\"", Some("a\"b")),
            ("\"it\\'s\"", Some("it's")),
            ("['<Super>a', '<Alt>F4']", Some("<Super>a|<Alt>F4")),
            ("['']", Some("")),
            ("@as []", Some("")),
            ("[]", Some("")),
            ("@s 'x'", Some("x")),
            ("''", Some("")),
            ("6", None),
            ("uint32 6", None),
            ("true", None),
            ("@au []", None),
            ("['a' 'b']", None),
            ("['a',", None),
            ("'a' x", None),
            ("'open", None),
            ("'\\u12'", None),
            ("", None),
        ] {
            assert_eq!(s(value).as_deref(), want, "{value}");
        }
        assert_eq!(strings("@as []"), Some(vec![]));
        assert_eq!(strings("['']"), Some(vec![String::new()]));
    }

    #[test]
    fn settings_and_describe() {
        assert_eq!(
            settings("a.b close ['<Alt>F4']\nshort line\n\na.b.c k 'x y'"),
            [
                ("a.b".into(), "close".into(), "['<Alt>F4']".into()),
                ("a.b.c".into(), "k".into(), "'x y'".into()),
            ]
        );
        assert_eq!(describe("switch-to-workspace-1"), "switch to workspace 1");
    }

    /// The real `gsettings` on trimmed upstream schemas compiled into a temp dir, with the memory backend (no session,
    /// no dconf). Skipped, with a message, without `gsettings` or `glib-compile-schemas`.
    #[test]
    fn real_gsettings() {
        let has = |p: &str, arg: &str| {
            Command::new(p)
                .arg(arg)
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !has("gsettings", "--version") || !has("glib-compile-schemas", "--version") {
            eprintln!("skipping: gsettings or glib-compile-schemas is not installed");
            return;
        }
        let tmp = std::env::temp_dir().join(format!("kbf-gnome-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let schemas = tmp.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        for f in std::fs::read_dir(fixture("schemas")).unwrap() {
            let f = f.unwrap().path();
            std::fs::copy(&f, schemas.join(f.file_name().unwrap())).unwrap();
        }
        let ok = Command::new("glib-compile-schemas")
            .args(["--strict"])
            .arg(&schemas)
            .status()
            .unwrap();
        assert!(ok.success());
        let env = |backend: &'static str| {
            let (schemas, tmp) = (schemas.clone(), tmp.clone());
            move |c: &mut Command| {
                c.env("GSETTINGS_SCHEMA_DIR", &schemas)
                    .env("GSETTINGS_BACKEND", backend)
                    .env("HOME", tmp.join("home"))
                    .env("XDG_CONFIG_HOME", tmp.join("config"))
                    .env("XDG_DATA_HOME", tmp.join("data"))
                    .env("XDG_DATA_DIRS", tmp.join("dirs"));
            }
        };
        let (s, notes) = read(env("memory")).unwrap();
        assert!(
            rows(&s, "window manager").contains(&("alt+F4", "close")),
            "{s:?}"
        );
        assert!(rows(&s, "shell").contains(&("Super_L", "overlay key (activities)")));
        for category in ["shell", "mutter", "media keys"] {
            assert!(
                rows(&s, category)
                    .iter()
                    .any(|(k, _)| k.starts_with("super+")),
                "{category}: {s:?}"
            );
        }
        // Only the Above_Tab defaults are left out.
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("leaving out 2 shortcuts"), "{notes:?}");
        // A custom shortcut, set through the keyfile backend (the memory one forgets between runs), read back.
        let p = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom0/";
        for args in [
            vec!["set", MEDIA, "custom-keybindings", &format!("['{p}']")],
            vec!["set", &format!("{CUSTOM}:{p}"), "binding", "<Super>Return"],
            vec!["set", &format!("{CUSTOM}:{p}"), "name", "it's"],
        ] {
            let mut cmd = Command::new("gsettings");
            cmd.args(&args);
            env("keyfile")(&mut cmd);
            assert!(cmd.status().unwrap().success(), "{args:?}");
        }
        let (s, _) = read(env("keyfile")).unwrap();
        assert_eq!(rows(&s, "custom"), [("super+Return", "it's")]);
        // A schema that isn't installed, as the real gsettings says it.
        std::fs::remove_file(schemas.join("org.gnome.shell.gschema.xml")).unwrap();
        let ok = Command::new("glib-compile-schemas")
            .arg(&schemas)
            .status()
            .unwrap();
        assert!(ok.success());
        let (_, notes) = read(env("memory")).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(
            notes[0],
            "schema not installed: org.gnome.shell.keybindings"
        );
    }
}
