//! tmux: the running server's effective keymap, from `tmux list-keys -N -a`. The prefix table comes with each key
//! written after the user's real prefix (`C-b c`); the root table (`bind -n`) with `-P ''`, since tmux otherwise puts
//! the prefix before root keys too. A key's note is its description, or its command when it has none.

use crate::context::{run_for, Context};
use crate::sheet::{chord, Bind, Sheet};
use anyhow::Context as _;
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(1);

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    // ponytail: run from a hotkey there's no `TMUX` variable, so this is the default socket; servers started with
    // `-L`/`-S` (or a different `TMUX_TMPDIR`) aren't found. Take the socket from the client's argv if that setup appears.
    read(|args| {
        let mut cmd = Command::new("tmux");
        cmd.args(args);
        run_for(cmd, TIMEOUT)
    })
}

/// Both tables through `list` (a test seam: tmux's arguments in, its stdout out). `list-keys` starts a server when
/// none is running, which would run the user's `~/.tmux.conf` (plugins included) just to list keys, so it first
/// checks with `has-session`, which doesn't.
fn read(list: impl Fn(&[&str]) -> anyhow::Result<String>) -> anyhow::Result<Sheet> {
    list(&["has-session"]).context("checking for a tmux server on the default socket")?;
    // tmux deletes a table once it has no keys (`unbind -a`), and then says so: that table is just empty.
    let table = |args: &[&str]| match list(args) {
        Err(e) if format!("{e:#}").contains("doesn't exist") => Ok(String::new()),
        r => r,
    };
    let prefix = table(&["list-keys", "-N", "-a", "-T", "prefix"])?;
    let root = table(&["list-keys", "-N", "-a", "-P", "", "-T", "root"])?;
    let (sheet, skipped) = parse(&prefix, &root);
    if skipped > 0 {
        eprintln!("keybind-find: tmux: skipping {skipped} keys with no keysym we know");
    }
    Ok(sheet)
}

/// The sheet, and how many keys had no translation. Every line is `<prefix> <key> <padding> <text>`, where the
/// prefix is empty for the root table (`-P ''`) and with `set -g prefix None`; the key column is padded, so the key is
/// the next word after the first space, and the text the rest. Prefix-table keys with no prefix are left out: no key
/// sequence we could send reaches them.
/// ponytail: keys under `prefix2` show under `prefix` only; read `show -gv prefix2` if someone relies on it.
fn parse(prefix: &str, root: &str) -> (Sheet, usize) {
    let mut sheet = Sheet {
        title: "tmux".into(),
        provider: "tmux".into(),
        binds: Default::default(),
    };
    let mut skipped = 0;
    for (category, out) in [("prefix", prefix), ("root", root)] {
        for line in out.lines() {
            let Some((first, rest)) = line.split_once(' ') else {
                continue;
            };
            let rest = rest.trim_start();
            let (k, desc) = rest.split_once(' ').unwrap_or((rest, ""));
            let keys: Vec<&str> = [first, k].into_iter().filter(|k| !k.is_empty()).collect();
            let unreachable = category == "prefix" && first.is_empty();
            if unreachable || keys.is_empty() || keys.iter().any(|k| is_mouse(k)) {
                continue;
            }
            match keys.iter().map(|k| key(k)).collect::<Option<Vec<_>>>() {
                Some(chords) => sheet.binds.entry(category.into()).or_default().push(Bind {
                    key: chords.join(" "),
                    desc: desc.trim().into(),
                    action: String::new(),
                    when: Default::default(),
                }),
                None => skipped += 1,
            }
        }
    }
    (sheet, skipped)
}

fn is_mouse(key: &str) -> bool {
    ["Mouse", "Wheel", "Click", "Drag"]
        .iter()
        .any(|m| key.contains(m))
}

/// One tmux key (`C-b`, `C-M-x`, `S-Up`, `BSpace`, `%`) as a chord in the crate's notation, `None` if it has no
/// keysym we know (keypad keys, `User0`, ...).
fn key(tmux: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut rest = tmux;
    // `M--` is alt+minus: a modifier needs something after it.
    while rest.len() > 2 {
        match rest.get(..2) {
            Some("C-") => mods.push("ctrl"),
            Some("M-") => mods.push("alt"),
            Some("S-") => mods.push("shift"),
            _ => break,
        }
        rest = &rest[2..];
    }
    let sym = match rest {
        "Up" | "Down" | "Left" | "Right" | "Home" | "End" | "Tab" | "Escape" => rest,
        // The X keysym is lower-case `space`, as nvim's sheet writes it.
        "Space" => "space",
        "Enter" => "Return",
        "BSpace" => "BackSpace",
        "DC" => "Delete",
        "IC" => "Insert",
        "PPage" | "PageUp" => "Prior",
        "NPage" | "PageDown" => "Next",
        "BTab" => {
            mods.push("shift");
            "Tab"
        }
        f if f
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=12).contains(&n)) =>
        {
            f
        }
        _ => {
            let mut chars = rest.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => super::nvim::single(c)?,
                _ => return None,
            }
        }
    };
    Some(chord(&mods, sym))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/tmux")
                .join(name),
        )
        .unwrap()
    }

    fn find<'a>(s: &'a Sheet, category: &str, key: &str) -> Option<&'a Bind> {
        s.binds.get(category)?.iter().find(|b| b.key == key)
    }

    #[test]
    fn reads_both_tables_without_mouse_keys() {
        let (s, skipped) = parse(&fixture("prefix.txt"), &fixture("root.txt"));
        assert_eq!(skipped, 0);
        assert_eq!(
            find(&s, "prefix", "ctrl+b c").unwrap().desc,
            "Create a new window"
        );
        assert_eq!(
            find(&s, "prefix", "ctrl+b quotedbl").unwrap().desc,
            "Split window vertically"
        );
        assert_eq!(
            find(&s, "prefix", "ctrl+b ctrl+b").unwrap().desc,
            "Send the prefix key"
        );
        assert_eq!(
            find(&s, "prefix", "ctrl+b semicolon").unwrap().desc,
            "Move to the previously active pane"
        );
        // A user binding with no note shows its command.
        assert_eq!(find(&s, "root", "alt+h").unwrap().desc, "select-pane -L");
        assert_eq!(s.binds["root"].len(), 1, "{:?}", s.binds["root"]);
        // Every default prefix key translates: one bind per line.
        assert_eq!(
            s.binds["prefix"].len(),
            fixture("prefix.txt").lines().count()
        );
        assert_eq!(s.provider, "tmux");
    }

    #[test]
    fn the_users_prefix_is_kept() {
        let prefix = fixture("prefix.txt").replace("C-b ", "C-a ");
        let (s, _) = parse(&prefix, "");
        assert!(find(&s, "prefix", "ctrl+a c").is_some());
        assert!(find(&s, "prefix", "ctrl+b c").is_none());
    }

    #[test]
    fn key_translation() {
        for (tmux, want) in [
            ("C-b", "ctrl+b"),
            ("C-M-x", "ctrl+alt+x"),
            ("M-C-x", "ctrl+alt+x"),
            ("S-Up", "shift+Up"),
            ("M-Up", "alt+Up"),
            ("C-S-Left", "ctrl+shift+Left"),
            ("BSpace", "BackSpace"),
            ("Enter", "Return"),
            ("Space", "space"),
            ("DC", "Delete"),
            ("IC", "Insert"),
            ("PPage", "Prior"),
            ("NPage", "Next"),
            ("BTab", "shift+Tab"),
            ("F5", "F5"),
            ("F12", "F12"),
            ("%", "percent"),
            ("\"", "quotedbl"),
            ("\\", "backslash"),
            ("C", "C"),
            ("M-1", "alt+1"),
            ("M--", "alt+minus"),
            ("-", "minus"),
            ("Home", "Home"),
            ("End", "End"),
            ("Escape", "Escape"),
            ("Tab", "Tab"),
            ("PageUp", "Prior"),
            ("PageDown", "Next"),
            ("S-BTab", "shift+Tab"),
        ] {
            assert_eq!(key(tmux).as_deref(), Some(want), "{tmux}");
        }
        for tmux in ["KP0", "User0", "F13", "F0", "Foo", "C-", "", "ö"] {
            assert_eq!(key(tmux), None, "{tmux}");
        }
    }

    #[test]
    fn prefix_none_and_mouse_prefix_keys_are_left_out() {
        // `set -g prefix None` (tmux 3.8): an empty prefix column. Pressing `c` alone doesn't reach these.
        let (s, skipped) = parse(
            " c       Create a new window\n Tab     Switch to a window\nC-b MouseDown1Pane  select-pane\n",
            "",
        );
        assert_eq!(skipped, 0);
        assert!(s.binds.is_empty(), "{:?}", s.binds);
        // A description whose first word is a key is still a description.
        let (s, _) = parse("C-b x       End the pane\n", "");
        assert_eq!(find(&s, "prefix", "ctrl+b x").unwrap().desc, "End the pane");
    }

    #[test]
    fn a_deleted_table_is_empty() {
        // After `unbind -a` and only `bind -n` keys, tmux has no prefix table.
        let s = read(|args| {
            if args.contains(&"prefix") {
                anyhow::bail!("tmux failed (exit status: 1): table prefix doesn't exist")
            }
            Ok(if args.contains(&"root") {
                " M-h   select-pane -L\n".into()
            } else {
                String::new()
            })
        })
        .unwrap();
        assert_eq!(find(&s, "root", "alt+h").unwrap().desc, "select-pane -L");
        // Any other failure still fails the provider.
        assert!(read(|args| if args.contains(&"prefix") {
            anyhow::bail!("tmux did not answer within 1.0 s")
        } else {
            Ok(String::new())
        })
        .is_err());
    }

    #[test]
    fn untranslatable_keys_are_skipped_and_counted() {
        let root =
            " KP0                       next-window\n F5                        next-window\n";
        let (s, skipped) = parse("C-b User0   select-pane\n", root);
        assert_eq!(skipped, 2);
        assert_eq!(s.binds["root"].len(), 1);
        assert!(!s.binds.contains_key("prefix"));
        assert!(parse("", "").0.binds.is_empty());
    }

    #[test]
    fn asks_tmux_for_both_tables_and_reports_its_failure() {
        let calls = std::cell::RefCell::new(Vec::new());
        let s = read(|args| {
            calls.borrow_mut().push(args.join(" "));
            Ok(if args.contains(&"prefix") {
                "C-b c       Create a new window\n".into()
            } else if args.contains(&"root") {
                " M-h   select-pane -L\n".into()
            } else {
                String::new()
            })
        })
        .unwrap();
        assert_eq!(
            *calls.borrow(),
            [
                "has-session",
                "list-keys -N -a -T prefix",
                "list-keys -N -a -P  -T root"
            ]
        );
        assert_eq!(s.binds.values().map(Vec::len).sum::<usize>(), 2);
        // No server: `list-keys` is never run, since it would start one.
        let calls = std::cell::RefCell::new(Vec::new());
        let err = read(|args| {
            calls.borrow_mut().push(args.join(" "));
            anyhow::bail!(
                "tmux failed (exit status: 1): error connecting to /tmp/tmux-1000/default"
            )
        })
        .unwrap_err();
        assert_eq!(*calls.borrow(), ["has-session"]);
        let err = format!("{err:#}");
        assert!(
            err.contains("checking for a tmux server") && err.contains("error connecting"),
            "{err}"
        );
    }
}
