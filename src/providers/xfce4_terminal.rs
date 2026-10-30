//! Xfce Terminal: its menu accelerators, the defaults pinned per version (`data/xfce4_terminal/`) with the user's
//! `accels.scm` (a GTK accel map) on top. Shown when an Xfce Terminal window has focus.

use super::pinned;
use crate::context::run_for;
use crate::sheet::{Bind, Sheet};
use anyhow::Context as _;
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

const REGISTRY: &[(&str, &str)] = &[(
    "1.2.0",
    include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/data/xfce4_terminal/1.2.0.toml"
    )),
)];

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Where the accel paths of the window's actions start.
const PREFIX: &str = "<Actions>/terminal-window/";

#[derive(Deserialize)]
struct Pinned {
    action: Vec<Action>,
}

#[derive(Deserialize)]
struct Action {
    path: String,
    /// `""`: none.
    accel: String,
    label: String,
}

pub fn sheet() -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var(k).ok();
    let (sheet, notes) = sheet_with(REGISTRY, |_| {}, &env)?;
    if !notes.is_empty() {
        eprintln!("keybind-find: xfce4-terminal: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `sheet` over `registry`, with `setup` applied to `xfce4-terminal --version` (a test seam: `PATH`) and
/// `accels.scm` found through `env`. Without `xfce4-terminal`, the newest defaults and the note that says so.
fn sheet_with(
    registry: &[(&'static str, &'static str)],
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    // The display stays: `main` runs `gtk_init` (which exits without one) before it handles `--version`; it opens no
    // window and asks no running instance.
    let mut cmd = Command::new("xfce4-terminal");
    cmd.arg("--version")
        .env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let installed = run_for(cmd, VERSION_TIMEOUT)
        .ok()
        .and_then(|t| version_of(&t));
    let (v, text, note) = pinned::select("xfce4_terminal", registry, installed.as_deref())
        .context("no pinned xfce4-terminal accelerators")?;
    let pinned: Pinned =
        toml::from_str(text).with_context(|| format!("reading data/xfce4_terminal/{v}.toml"))?;
    let accels = accels_file(env)
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    let (sheet, left_out) = parse(&pinned, accels.as_deref());
    let mut notes: Vec<String> = note.into_iter().collect();
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} key(s) keybind-find can't send (<Hyper>, <Meta>, <Release> or another \
             modifier, or a key that isn't a keysym name)"
        ));
    }
    Ok((sheet, notes))
}

/// The version in `xfce4-terminal 1.2.0 (Xfce 4.20)`.
fn version_of(text: &str) -> Option<String> {
    let v = text
        .lines()
        .next()?
        .strip_prefix("xfce4-terminal ")?
        .split_whitespace()
        .next()?;
    pinned::parse_version(v).map(|_| v.to_string())
}

/// The first `xfce4/terminal/accels.scm` in Xfce's config resource dirs (`xfce_resource_lookup`):
/// `$XDG_CONFIG_HOME` (else `~/.config`), then each `$XDG_CONFIG_DIRS` entry (else `/etc/xdg`), then `/etc/xdg`
/// (libxfce4util always adds its `SYSCONFDIR/xdg`).
fn accels_file(env: Env) -> Option<PathBuf> {
    let set = |k: &str| env(k).filter(|v| !v.is_empty());
    let home = set("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| set("HOME").map(|h| PathBuf::from(h).join(".config")));
    let dirs = set("XDG_CONFIG_DIRS").unwrap_or_else(|| "/etc/xdg".into());
    let found = home
        .into_iter()
        .chain(
            dirs.split(':')
                .chain(["/etc/xdg"])
                .map(PathBuf::from)
                .filter(|d| d.is_absolute()),
        )
        .map(|d| d.join("xfce4/terminal/accels.scm"))
        .find(|p| p.is_file());
    found
}

/// The sheet from the pinned actions with the accel map `accels` applied, and how many keys were left out.
fn parse(pinned: &Pinned, accels: Option<&str>) -> (Sheet, usize) {
    // (action name, accel, label), in the pinned order; an action the user's map names first goes after them.
    let mut actions: Vec<(String, String, String)> = pinned
        .action
        .iter()
        .filter_map(|a| {
            let name = a.path.strip_prefix(PREFIX)?;
            Some((name.to_string(), a.accel.clone(), a.label.replace('_', "")))
        })
        .collect();
    for (path, accel) in accel_map(accels.unwrap_or("")) {
        let Some(name) = path.strip_prefix(PREFIX) else {
            continue;
        };
        match actions.iter_mut().find(|(n, _, _)| n == name) {
            Some((_, a, _)) => *a = accel,
            None => actions.push((name.to_string(), accel, name.to_string())),
        }
    }
    let mut sheet = Sheet {
        title: "xfce4-terminal".into(),
        provider: "xfce4_terminal".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    for (name, accel, label) in actions {
        if accel.is_empty() {
            continue;
        }
        let Some(key) = notation(&accel) else {
            left_out += 1;
            continue;
        };
        sheet
            .binds
            .entry("xfce4-terminal".into())
            .or_default()
            .push(Bind {
                key,
                desc: label,
                action: name,
                when: Default::default(),
            });
    }
    (sheet, left_out)
}

/// A GTK accelerator in key notation (`super::xfce::gtk_accel`), with GDK's `Page_Up`/`Page_Down` as the X keysyms
/// `Prior`/`Next`.
fn notation(accel: &str) -> Option<String> {
    let key = super::xfce::gtk_accel(accel)?;
    let (mods, keysym) = key
        .rsplit_once('+')
        .map_or(("", key.as_str()), |(m, k)| (m, k));
    let keysym = match keysym {
        "Page_Up" => "Prior",
        "Page_Down" => "Next",
        k => k,
    };
    Some(if mods.is_empty() {
        keysym.to_string()
    } else {
        format!("{mods}+{keysym}")
    })
}

/// The `(gtk_accel_path "<path>" "<accel>")` statements of a GTK accel map, in order, read as GTK's scanner reads
/// them: tokens, so a statement may span lines; `;` starts a comment to the end of the line outside a string (GTK
/// writes the unchanged defaults commented out). Anything else is skipped.
fn accel_map(text: &str) -> Vec<(String, String)> {
    #[derive(PartialEq)]
    enum Token {
        Open,
        Close,
        Word(String),
        Str(String),
    }
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ';' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => s.extend(chars.next()),
                        c => s.push(c),
                    }
                }
                tokens.push(Token::Str(s));
            }
            c if c.is_whitespace() => {}
            c => {
                let mut w = c.to_string();
                while let Some(&n) = chars.peek() {
                    if n.is_whitespace() || "()\";".contains(n) {
                        break;
                    }
                    w.push(n);
                    chars.next();
                }
                tokens.push(Token::Word(w));
            }
        }
    }
    tokens
        .windows(5)
        .filter_map(|w| match w {
            [Token::Open, Token::Word(name), Token::Str(path), Token::Str(accel), Token::Close]
                if name == "gtk_accel_path" =>
            {
                Some((path.clone(), accel.clone()))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xfce4_terminal")
            .join(name)
    }

    fn pinned() -> Pinned {
        toml::from_str(REGISTRY[0].1).unwrap()
    }

    fn get<'a>(s: &'a Sheet, action: &str) -> Vec<(&'a str, &'a str)> {
        s.binds
            .values()
            .flatten()
            .filter(|b| b.action == action)
            .map(|b| (b.key.as_str(), b.desc.as_str()))
            .collect()
    }

    #[test]
    fn pinned_defaults() {
        let text = REGISTRY[0].1;
        assert!(
            text.contains("tag xfce4-terminal-1.2.0")
                && text.contains("bcc92696a1526b85875f03c21b265bd376bce00d")
        );
        assert!(text.contains("terminal/terminal-window.c"));
        let p = pinned();
        // 61 entries in `action_entries[]`, 33 with a default accelerator; every one of them translates.
        assert_eq!(p.action.len(), 61);
        assert_eq!(p.action.iter().filter(|a| !a.accel.is_empty()).count(), 33);
        let (s, left_out) = parse(&p, None);
        assert_eq!((s.binds["xfce4-terminal"].len(), left_out), (33, 0));
        assert_eq!(get(&s, "new-tab"), [("ctrl+shift+t", "Open Tab")]);
        assert_eq!(get(&s, "prev-tab"), [("ctrl+Prior", "Previous Tab")]);
        assert_eq!(get(&s, "next-tab"), [("ctrl+Next", "Next Tab")]);
        assert_eq!(
            get(&s, "move-tab-right"),
            [("ctrl+shift+Next", "Move Tab Right")]
        );
        assert_eq!(get(&s, "zoom-in"), [("ctrl+plus", "Zoom In")]);
        assert_eq!(get(&s, "goto-tab-3"), [("alt+3", "Go to Tab 3")]);
        assert_eq!(get(&s, "contents"), [("F1", "Contents")]);
        for b in s.binds.values().flatten() {
            assert!(
                b.key.parse::<keybind_find_sheet::Key>().is_ok(),
                "{}",
                b.key
            );
            assert!(b.when.is_empty());
        }
        assert_eq!(
            (s.title.as_str(), s.provider.as_str()),
            ("xfce4-terminal", "xfce4_terminal")
        );
    }

    #[test]
    fn the_users_accel_map() {
        let accels = std::fs::read_to_string(fixture("accels.scm")).unwrap();
        let (s, left_out) = parse(&pinned(), Some(&accels));
        // Commented defaults stand as they were; a changed key, a removed one.
        assert_eq!(get(&s, "copy"), [("ctrl+shift+c", "Copy")]);
        assert_eq!(get(&s, "new-tab"), [("ctrl+shift+n", "Open Tab")]);
        assert!(get(&s, "close-tab").is_empty());
        assert_eq!(get(&s, "goto-tab-1"), [("alt+1", "Go to Tab 1")]);
        assert_eq!(get(&s, "prev-tab"), [("ctrl+Prior", "Previous Tab")]);
        // A modifier name in any case; a capital letter is the letter.
        assert_eq!(get(&s, "detach-tab"), [("ctrl+shift+d", "Detach Tab")]);
        // An action the defaults don't have: its path's last part.
        assert_eq!(get(&s, "some-new-action"), [("super+k", "some-new-action")]);
        // Outside terminal-window, junk and an unfinished statement: nothing. `<Hyper>`: left out, counted.
        assert!(get(&s, "whatever").is_empty());
        assert_eq!(get(&s, "zoom-in"), [("ctrl+plus", "Zoom In")]);
        assert!(get(&s, "fullscreen").is_empty());
        assert_eq!(left_out, 1);
    }

    #[test]
    fn accel_map_lines() {
        let m = accel_map(
            "  (gtk_accel_path \"<Actions>/terminal-window/a\" \"q\")\n(gtk_accel_path \"p\\\"x\" \"<Alt>\\\\\") ; c\n; (gtk_accel_path \"b\" \"x\")\n(gtk_accel_path \"c\")\n",
        );
        assert_eq!(
            m,
            [
                ("<Actions>/terminal-window/a".to_string(), "q".to_string()),
                ("p\"x".to_string(), "<Alt>\\".to_string())
            ]
        );
    }

    #[test]
    fn a_statement_across_lines() {
        let m = accel_map(
            "(gtk_accel_path\n  \"<Actions>/terminal-window/copy\" ; why\n  \"<Alt>c\")\n",
        );
        assert_eq!(
            m,
            [(
                "<Actions>/terminal-window/copy".to_string(),
                "<Alt>c".to_string()
            )]
        );
    }

    /// A fake `xfce4-terminal` printing `version` for `--version`, in its own directory.
    fn fake(name: &str, version: &str) -> PathBuf {
        let bin = std::env::temp_dir().join(format!(
            "kbf-xfce4-terminal-bin-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&bin).unwrap();
        // Written by `sh`, not by this process (ETXTBSY: see `xfce.rs`'s `fake`).
        let ok = Command::new("sh")
            .args([
                "-c",
                "printf '#!/bin/sh\\necho \"%s\"\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(bin.join("xfce4-terminal"))
            .arg(version)
            .status()
            .unwrap();
        assert!(ok.success());
        bin
    }

    #[test]
    fn the_whole_read() {
        let home =
            std::env::temp_dir().join(format!("kbf-xfce4-terminal-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/xfce4/terminal")).unwrap();
        std::fs::copy(
            fixture("accels.scm"),
            home.join(".config/xfce4/terminal/accels.scm"),
        )
        .unwrap();
        let h = home.display().to_string();
        let env = |k: &str| (k == "HOME").then(|| h.clone());
        for (version, note) in [
            ("xfce4-terminal 1.2.0 (Xfce 4.20)", None),
            (
                "xfce4-terminal 1.1.4 (Xfce 4.20)",
                Some("add data/xfce4_terminal/1.1.4.toml"),
            ),
        ] {
            let bin = fake(&version[15..20], version);
            let (s, notes) = sheet_with(
                REGISTRY,
                |c: &mut Command| {
                    c.env("PATH", &bin);
                },
                &env,
            )
            .unwrap();
            let _ = std::fs::remove_dir_all(&bin);
            // The user's file reached the sheet; `<Hyper>f` counted in the notes.
            assert_eq!(get(&s, "new-tab"), [("ctrl+shift+n", "Open Tab")]);
            let left = "leaving out 1 key(s) keybind-find can't send";
            match note {
                None => assert!(notes.len() == 1 && notes[0].starts_with(left), "{notes:?}"),
                Some(n) => assert!(
                    notes.len() == 2 && notes[0].contains(n) && notes[1].starts_with(left),
                    "{notes:?}"
                ),
            }
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_config_file() {
        let dir = std::env::temp_dir().join(format!("kbf-xfce4-terminal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (home, xdg, sys1, sys2) = (
            dir.join("home"),
            dir.join("xdg"),
            dir.join("sys1"),
            dir.join("sys2"),
        );
        for d in [&home, &xdg, &sys1, &sys2] {
            std::fs::create_dir_all(d.join("xfce4/terminal")).unwrap();
        }
        std::fs::create_dir_all(home.join(".config/xfce4/terminal")).unwrap();
        let env_of = |pairs: Vec<(&'static str, String)>| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone())
        };
        let file = |d: &Path| d.join("xfce4/terminal/accels.scm");
        let dirs = format!("{}:{}", sys1.display(), sys2.display());
        let h = home.display().to_string();
        // None anywhere: none.
        assert_eq!(
            accels_file(&env_of(vec![
                ("HOME", h.clone()),
                ("XDG_CONFIG_DIRS", dirs.clone())
            ])),
            None
        );
        // XDG_CONFIG_DIRS in order.
        std::fs::write(file(&sys2), "").unwrap();
        assert_eq!(
            accels_file(&env_of(vec![
                ("HOME", h.clone()),
                ("XDG_CONFIG_DIRS", dirs.clone())
            ])),
            Some(file(&sys2))
        );
        std::fs::write(file(&sys1), "").unwrap();
        assert_eq!(
            accels_file(&env_of(vec![
                ("HOME", h.clone()),
                ("XDG_CONFIG_DIRS", dirs.clone())
            ])),
            Some(file(&sys1))
        );
        // ~/.config before them; XDG_CONFIG_HOME instead of ~/.config.
        std::fs::write(home.join(".config/xfce4/terminal/accels.scm"), "").unwrap();
        assert_eq!(
            accels_file(&env_of(vec![
                ("HOME", h.clone()),
                ("XDG_CONFIG_DIRS", dirs.clone())
            ])),
            Some(home.join(".config/xfce4/terminal/accels.scm"))
        );
        std::fs::write(file(&xdg), "").unwrap();
        assert_eq!(
            accels_file(&env_of(vec![
                ("HOME", h.clone()),
                ("XDG_CONFIG_HOME", xdg.display().to_string()),
                ("XDG_CONFIG_DIRS", dirs.clone())
            ])),
            Some(file(&xdg))
        );
        // XDG_CONFIG_HOME set: ~/.config isn't looked in, even with nothing there; relative or empty: unset.
        let empty = dir.join("empty");
        assert_eq!(
            accels_file(&env_of(vec![
                ("HOME", h.clone()),
                ("XDG_CONFIG_HOME", empty.display().to_string())
            ])),
            None
        );
        for rel in ["rel", ""] {
            assert_eq!(
                accels_file(&env_of(vec![
                    ("HOME", h.clone()),
                    ("XDG_CONFIG_HOME", rel.to_string())
                ])),
                Some(home.join(".config/xfce4/terminal/accels.scm"))
            );
        }
        // A relative XDG_CONFIG_DIRS entry is skipped.
        std::fs::remove_file(home.join(".config/xfce4/terminal/accels.scm")).unwrap();
        assert_eq!(
            accels_file(&env_of(vec![
                ("HOME", h.clone()),
                ("XDG_CONFIG_DIRS", format!("rel:{}", sys2.display()))
            ])),
            Some(file(&sys2))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_version() {
        assert_eq!(
            version_of("xfce4-terminal 1.2.0 (Xfce 4.20)\n\nCopyright…").as_deref(),
            Some("1.2.0")
        );
        for junk in ["", "xfce4-terminal", "foo 1.2.0", "xfce4-terminal x.y"] {
            assert_eq!(version_of(junk), None, "{junk}");
        }
        // No xfce4-terminal: the newest defaults and the note.
        let none = |_: &str| None;
        let (s, notes) = sheet_with(
            REGISTRY,
            |c: &mut Command| {
                c.env("PATH", "/nonexistent");
            },
            &none,
        )
        .unwrap();
        assert_eq!(s.binds["xfce4-terminal"].len(), 33);
        assert_eq!(
            notes,
            ["xfce4_terminal's version is unknown: using the defaults of 1.2.0"]
        );
    }

    /// The real `xfce4-terminal --version`, when it is installed; skipped with a message otherwise.
    #[test]
    fn real_xfce4_terminal() {
        let installed = Command::new("xfce4-terminal")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !installed {
            eprintln!(
                "skipping: xfce4-terminal is not installed (or has no display to start GTK on)"
            );
            return;
        }
        let mut cmd = Command::new("xfce4-terminal");
        cmd.arg("--version").env("LC_ALL", "C");
        let out = run_for(cmd, VERSION_TIMEOUT).unwrap();
        assert!(version_of(&out).is_some(), "{out}");
    }
}
