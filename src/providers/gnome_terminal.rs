//! gnome-terminal: its keyboard shortcuts as its preferences set them, from `gsettings` (the
//! `org.gnome.Terminal.Legacy.Keybindings` child of `Legacy.Settings`). Shown when a gnome-terminal window has focus.

use super::gnome::{describe, list_recursively, settings, strings};
use super::xfce::gtk_accel;
use crate::sheet::{Bind, Sheet};
use std::process::Command;

const SETTINGS: &str = "org.gnome.Terminal.Legacy.Settings";
const KEYBINDINGS: &str = "org.gnome.Terminal.Legacy.Keybindings";

pub fn sheet() -> anyhow::Result<Sheet> {
    let (sheet, notes) = read(|_| {})?;
    for note in notes {
        eprintln!("keybind-find: gnome-terminal: {note}");
    }
    Ok(sheet)
}

/// One `gsettings list-recursively` of `Legacy.Settings` (its `keybindings` child included), with `setup` applied (a
/// test seam). An error when gsettings fails or the schema isn't installed.
fn read(setup: impl Fn(&mut Command)) -> anyhow::Result<(Sheet, Vec<String>)> {
    Ok(parse(&list_recursively(SETTINGS, setup)?))
}

/// The sheet from `list-recursively`'s text, and the notes for stderr: empty, with a note, when `shortcuts-enabled`
/// is off; `'disabled'` and `''` list nothing; an accelerator `gtk_accel` refuses is counted.
fn parse(text: &str) -> (Sheet, Vec<String>) {
    let mut sheet = Sheet {
        title: "gnome-terminal".into(),
        provider: "gnome-terminal".into(),
        binds: Default::default(),
    };
    let lines = settings(text);
    if lines
        .iter()
        .any(|(s, k, v)| s == SETTINGS && k == "shortcuts-enabled" && v == "false")
    {
        let note = "its shortcuts are turned off (Preferences → Shortcuts, or shortcuts-enabled)";
        return (sheet, vec![note.into()]);
    }
    let mut left_out = 0;
    for (_, key, value) in lines.iter().filter(|(s, _, _)| s == KEYBINDINGS) {
        let Some(accels) = strings(value) else {
            left_out += 1;
            continue;
        };
        for accel in accels.iter().filter(|a| !a.is_empty() && *a != "disabled") {
            let Some(chord) = gtk_accel(accel) else {
                left_out += 1;
                continue;
            };
            sheet
                .binds
                .entry("keybindings".into())
                .or_default()
                .push(Bind {
                    key: chord,
                    desc: describe(key),
                    action: key.clone(),
                    when: Default::default(),
                });
        }
    }
    let mut notes = Vec::new();
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} shortcut(s) keybind-find can't name (a modifier other than ctrl, shift, alt \
             and super, or a key not shaped like a keysym name)"
        ));
    }
    (sheet, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gnome-terminal")
            .join(name)
    }

    fn rows(s: &Sheet) -> Vec<(&str, &str)> {
        s.binds
            .get("keybindings")
            .into_iter()
            .flatten()
            .map(|b| (b.key.as_str(), b.desc.as_str()))
            .collect()
    }

    /// A fake `gsettings` running `body`, in its own temp dir.
    fn fake(name: &str, body: &str) -> PathBuf {
        let bin =
            std::env::temp_dir().join(format!("kbf-gnome-terminal-{name}-{}", std::process::id()));
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

    #[test]
    fn upstream_defaults() {
        let text = std::fs::read_to_string(fixture("list-default.txt")).unwrap();
        let (s, notes) = parse(&text);
        assert_eq!(s.title, "gnome-terminal");
        assert!(notes.is_empty(), "{notes:?}");
        let rows = rows(&s);
        for want in [
            ("ctrl+shift+t", "new tab"),
            ("ctrl+Page_Up", "prev tab"),
            ("ctrl+shift+c", "copy"),
            ("ctrl+plus", "zoom in"),
            ("ctrl+shift+f", "find"),
        ] {
            assert!(rows.contains(&want), "{want:?}: {rows:?}");
        }
        // `'disabled'` defaults list nothing: 29 lines of 68 have a key (the switch is one of the others).
        assert!(!rows.iter().any(|(_, d)| *d == "copy html"));
        assert_eq!(rows.len(), 28);
    }

    #[test]
    fn user_changes_and_the_switch() {
        let text = std::fs::read_to_string(fixture("list-custom.txt")).unwrap();
        let (s, notes) = parse(&text);
        let rows = rows(&s);
        assert!(rows.contains(&("ctrl+t", "new tab")), "{rows:?}");
        assert!(!rows.contains(&("ctrl+shift+t", "new tab")));
        // `''` is cleared: nothing, silently; `<Hyper>s` is counted.
        assert!(!rows.iter().any(|(_, d)| *d == "copy"));
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("leaving out 1 shortcut(s)"),
            "{notes:?}"
        );
        // Shortcuts turned off: an empty sheet and the note.
        let off = text.replace("shortcuts-enabled true", "shortcuts-enabled false");
        let (s, notes) = parse(&off);
        assert!(s.binds.is_empty());
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].starts_with("its shortcuts are turned off"),
            "{notes:?}"
        );
        // A value that isn't a string is counted too.
        let (_, notes) = parse(&format!("{KEYBINDINGS} copy 6"));
        assert!(notes[0].starts_with("leaving out 1 "), "{notes:?}");
    }

    #[test]
    fn failures_are_one_error() {
        let bin = fake(
            "noschema",
            "echo \"No such schema “org.gnome.Terminal.Legacy.Settings”\" >&2; exit 1",
        );
        let e = read(path(&bin)).unwrap_err();
        let _ = std::fs::remove_dir_all(&bin);
        let e = format!("{e:#}");
        assert!(
            e.starts_with("running gsettings list-recursively org.gnome.Terminal.Legacy.Settings")
                && e.contains("No such schema"),
            "{e}"
        );
        let empty =
            std::env::temp_dir().join(format!("kbf-gnome-terminal-none-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let e = read(path(&empty)).unwrap_err();
        let _ = std::fs::remove_dir_all(&empty);
        assert!(format!("{e:#}").starts_with("running gsettings"), "{e:#}");
        // The fake's answer reaches the parser.
        let bin = fake(
            "ok",
            &format!(
                "[ \"$*\" = 'list-recursively {SETTINGS}' ] || exit 9\n/bin/cat '{}'",
                fixture("list-default.txt").display()
            ),
        );
        let (s, _) = read(path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert!(rows(&s).contains(&("ctrl+shift+t", "new tab")));
    }

    /// The real `gsettings` on the upstream `Legacy.Keybindings` schema (3.60.0) compiled into a temp dir, with the
    /// memory backend. Skipped, with a message, without `gsettings` or `glib-compile-schemas`.
    #[test]
    fn real_gsettings() {
        let has = |p: &str| {
            Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !has("gsettings") || !has("glib-compile-schemas") {
            eprintln!("skipping: gsettings or glib-compile-schemas is not installed");
            return;
        }
        let tmp =
            std::env::temp_dir().join(format!("kbf-gnome-terminal-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let schemas = tmp.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        let name = "org.gnome.Terminal.gschema.xml";
        std::fs::copy(fixture("schemas").join(name), schemas.join(name)).unwrap();
        let ok = Command::new("glib-compile-schemas")
            .arg("--strict")
            .arg(&schemas)
            .status()
            .unwrap();
        assert!(ok.success());
        let (s, notes) = read(|c: &mut Command| {
            c.env("GSETTINGS_SCHEMA_DIR", &schemas)
                .env("GSETTINGS_BACKEND", "memory")
                .env("HOME", tmp.join("home"))
                .env("XDG_CONFIG_HOME", tmp.join("config"))
                .env("XDG_DATA_HOME", tmp.join("data"))
                .env("XDG_DATA_DIRS", tmp.join("dirs"));
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(rows(&s).contains(&("ctrl+shift+t", "new tab")), "{s:?}");
    }
}
