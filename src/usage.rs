//! The usage log: one `<unix seconds>\t<provider>\t<key>` line per pick, appended after a successful send, and counted
//! on the next run so the most-used bindings come first. It never holds descriptions or espanso replacements.

use crate::sheet::Entry;
use anyhow::Context as _;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Counts = HashMap<(String, String), usize>;

pub fn path() -> Option<PathBuf> {
    path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// `$XDG_STATE_HOME/keybind-find/usage.tsv`, or under `$HOME/.local/state` when it is unset or relative.
fn path_from(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let state = match xdg_state_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => dir,
        None => PathBuf::from(home.filter(|v| !v.is_empty())?).join(".local/state"),
    };
    Some(state.join("keybind-find/usage.tsv"))
}

/// The counts from the log. Missing is empty; an unreadable log is one stderr line and empty.
// ponytail: reads the whole log every run; rotate or compact it if it passes ~1 MB (about 30 000 picks).
pub fn load() -> Counts {
    let Some(path) = path() else {
        return Counts::new();
    };
    // Lossy: a bad byte spoils only its own line, which then matches nothing.
    match std::fs::read(&path) {
        Ok(log) => counts(&String::from_utf8_lossy(&log)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Counts::new(),
        Err(e) => {
            eprintln!("keybind-find: reading {}: {e}", path.display());
            Counts::new()
        }
    }
}

/// Picks per `(provider, key)`. Lines without exactly three tab-separated fields are skipped.
pub fn counts(log: &str) -> Counts {
    let mut counts = Counts::new();
    for line in log.lines() {
        if let [_, provider, key] = line.split('\t').collect::<Vec<_>>()[..] {
            *counts.entry((provider.into(), key.into())).or_default() += 1;
        }
    }
    counts
}

/// Most-used first, ties in their usual order. The unused ones then take turns by program.
pub fn rank(entries: &mut [Entry], counts: &Counts) {
    let count = |e: &Entry| {
        counts
            .get(&(e.provider.clone(), e.bind.key.clone()))
            .copied()
            .unwrap_or(0)
    };
    entries.sort_by_cached_key(|e| std::cmp::Reverse(count(e)));
    let used = entries.iter().take_while(|e| count(e) > 0).count();
    alternate(&mut entries[used..]);
}

/// One entry from each program in turn, each program keeping its order, so the first screen isn't all one program.
/// The turns go: the innermost program (the last in the usual order), then the window manager, then the rest from
/// inner to outer.
fn alternate(entries: &mut [Entry]) {
    let mut groups: Vec<Vec<Entry>> = Vec::new();
    for e in entries.iter() {
        match groups.iter_mut().find(|g| g[0].provider == e.provider) {
            Some(g) => g.push(e.clone()),
            None => groups.push(vec![e.clone()]),
        }
    }
    groups.reverse();
    let wm = groups.iter().position(|g| {
        matches!(
            g[0].provider.as_str(),
            "xmonad" | "sway" | "hyprland" | "kde" | "xfce" | "i3" | "gnome"
        )
    });
    if let Some(wm @ 1..) = wm {
        let g = groups.remove(wm);
        groups.insert(1, g);
    }
    let mut turns: Vec<_> = groups.into_iter().map(Vec::into_iter).collect();
    let mut out = Vec::with_capacity(entries.len());
    while out.len() < entries.len() {
        for t in &mut turns {
            out.extend(t.next());
        }
    }
    entries.clone_from_slice(&out);
}

/// Appends `e` to the log, creating its directory. A key that couldn't be read back (tab, newline) is skipped.
pub fn record(e: &Entry) -> anyhow::Result<()> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let Some(line) = line(e, secs) else {
        return Ok(());
    };
    let path = path().context("no state directory: set XDG_STATE_HOME or HOME")?;
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // One write_all of a short line with O_APPEND: two runs at once don't interleave.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?
            .write_all(line.as_bytes())
    };
    write().with_context(|| format!("writing {}", path.display()))
}

fn line(e: &Entry, secs: u64) -> Option<String> {
    let bad = |s: &str| s.is_empty() || s.contains(['\t', '\n', '\r']);
    if bad(&e.provider) || bad(&e.bind.key) {
        return None;
    }
    Some(format!("{secs}\t{}\t{}\n", e.provider, e.bind.key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Bind;

    fn entry(provider: &str, key: &str) -> Entry {
        Entry {
            provider: provider.into(),
            category: "c".into(),
            bind: Bind {
                key: key.into(),
                desc: "secret description".into(),
                action: String::new(),
                when: Default::default(),
            },
        }
    }

    fn keys(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.bind.key.as_str()).collect()
    }

    #[test]
    fn counts_skip_bad_lines() {
        let log = "1\txmonad\tsuper+a\n\
                   2\txmonad\tsuper+a\n\
                   3\tespanso\t:date\n\
                   4\txmonad\n\
                   5\txmonad\tsuper+a\textra\n\
                   \n";
        let c = counts(log);
        assert_eq!(c.len(), 2);
        assert_eq!(c[&("xmonad".into(), "super+a".into())], 2);
        assert_eq!(c[&("espanso".into(), ":date".into())], 1);
        assert!(counts("").is_empty());
    }

    #[test]
    fn rank_is_stable() {
        let mut es = vec![
            entry("x", "a"),
            entry("x", "b"),
            entry("x", "c"),
            entry("x", "d"),
            entry("y", "d"),
        ];
        // d (x) most used; b and c tie; a and y's d unused, and they take turns, the inner y first.
        let c = counts("0\tx\td\n0\tx\td\n0\tx\tc\n0\tx\tb\n");
        rank(&mut es, &c);
        assert_eq!(keys(&es), ["d", "b", "c", "d", "a"]);
        assert_eq!(es[3].provider, "y");

        // Past insertion-sort sizes, where an unstable sort would reorder the unused ones.
        let mut es: Vec<_> = (0..60).map(|i| entry("x", &i.to_string())).collect();
        rank(&mut es, &counts("0\tx\t59\n0\tx\t30\n0\tx\t30\n"));
        let want: Vec<String> = [30, 59]
            .into_iter()
            .chain((0..60).filter(|i| ![59, 30].contains(i)))
            .map(|i: i32| i.to_string())
            .collect();
        assert_eq!(keys(&es), want);
    }

    /// Entries for `(provider, how many)` groups in the usual order, keyed `<provider><n>`.
    fn usual(groups: &[(&str, usize)]) -> Vec<Entry> {
        groups
            .iter()
            .flat_map(|&(p, n)| (0..n).map(move |i| entry(p, &format!("{p}{i}"))))
            .collect()
    }

    fn ranked(groups: &[(&str, usize)], log: &str) -> Vec<String> {
        let mut es = usual(groups);
        rank(&mut es, &counts(log));
        es.into_iter().map(|e| e.bind.key).collect()
    }

    #[test]
    fn unused_take_turns_innermost_then_wm() {
        // Uneven groups: a used-up group is skipped, and each keeps its order.
        assert_eq!(
            ranked(
                &[("xmonad", 1), ("espanso", 1), ("alacritty", 2), ("nvim", 3)],
                ""
            ),
            [
                "nvim0",
                "xmonad0",
                "alacritty0",
                "espanso0",
                "nvim1",
                "alacritty1",
                "nvim2"
            ]
        );
        assert_eq!(
            ranked(
                &[
                    ("xmonad", 2),
                    ("espanso", 1),
                    ("alacritty", 1),
                    ("herdr", 1),
                    ("nvim", 2)
                ],
                ""
            ),
            [
                "nvim0",
                "xmonad0",
                "herdr0",
                "alacritty0",
                "espanso0",
                "nvim1",
                "xmonad1"
            ]
        );
        // Over Vivaldi: its static sheet (provider `sheets`) is the innermost.
        assert_eq!(
            ranked(&[("xmonad", 2), ("espanso", 1), ("sheets", 2)], ""),
            ["sheets0", "xmonad0", "espanso0", "sheets1", "xmonad1"]
        );
        // sway and Hyprland are window managers too.
        assert_eq!(
            ranked(&[("sway", 1), ("espanso", 1), ("nvim", 1)], ""),
            ["nvim0", "sway0", "espanso0"]
        );
        assert_eq!(
            ranked(&[("hyprland", 1), ("espanso", 1), ("nvim", 1)], ""),
            ["nvim0", "hyprland0", "espanso0"]
        );
        assert_eq!(
            ranked(&[("kde", 1), ("espanso", 1), ("nvim", 1)], ""),
            ["nvim0", "kde0", "espanso0"]
        );
        assert_eq!(
            ranked(&[("xfce", 1), ("espanso", 1), ("nvim", 1)], ""),
            ["nvim0", "xfce0", "espanso0"]
        );
        assert_eq!(
            ranked(&[("i3", 1), ("espanso", 1), ("nvim", 1)], ""),
            ["nvim0", "i30", "espanso0"]
        );
        assert_eq!(
            ranked(&[("gnome", 1), ("espanso", 1), ("nvim", 1)], ""),
            ["nvim0", "gnome0", "espanso0"]
        );
    }

    #[test]
    fn unused_take_turns_edge_cases() {
        // No window manager: the reverse of the usual order.
        assert_eq!(
            ranked(&[("espanso", 2), ("alacritty", 1), ("nvim", 1)], ""),
            ["nvim0", "alacritty0", "espanso0", "espanso1"]
        );
        // The window manager alone, or innermost: first, once.
        assert_eq!(
            ranked(&[("xmonad", 3)], ""),
            ["xmonad0", "xmonad1", "xmonad2"]
        );
        assert_eq!(
            ranked(&[("espanso", 1), ("xmonad", 2)], ""),
            ["xmonad0", "espanso0", "xmonad1"]
        );
        assert!(ranked(&[], "").is_empty());
        // A window manager in the middle moves to second.
        assert_eq!(
            ranked(
                &[("espanso", 1), ("xmonad", 1), ("alacritty", 1), ("nvim", 1)],
                ""
            ),
            ["nvim0", "xmonad0", "alacritty0", "espanso0"]
        );
        // A provider split by another is one group, at its first place.
        let mut es = usual(&[("xmonad", 1), ("nvim", 1), ("espanso", 1)]);
        es.push(entry("xmonad", "xmonad1"));
        rank(&mut es, &Counts::new());
        let keys: Vec<_> = es.iter().map(|e| e.bind.key.as_str()).collect();
        assert_eq!(keys, ["espanso0", "xmonad0", "nvim0", "xmonad1"]);
    }

    #[test]
    fn used_stay_on_top_and_the_rest_take_turns() {
        let log = "0\tnvim\tnvim2\n0\txmonad\txmonad1\n0\txmonad\txmonad1\n";
        assert_eq!(
            ranked(&[("xmonad", 2), ("espanso", 1), ("nvim", 3)], log),
            ["xmonad1", "nvim2", "nvim0", "xmonad0", "espanso0", "nvim1"]
        );
        // Every entry used: count order only.
        assert_eq!(
            ranked(
                &[("xmonad", 1), ("nvim", 1)],
                "0\tnvim\tnvim0\n0\txmonad\txmonad0\n0\txmonad\txmonad0\n"
            ),
            ["xmonad0", "nvim0"]
        );
    }

    #[test]
    fn path_rules() {
        let p = |x: Option<&str>, h: Option<&str>| {
            path_from(x.map(Into::into), h.map(Into::into)).map(|p| p.display().to_string())
        };
        let home = Some("/h/.local/state/keybind-find/usage.tsv");
        assert_eq!(
            p(Some("/s"), Some("/h")).as_deref(),
            Some("/s/keybind-find/usage.tsv")
        );
        assert_eq!(p(None, Some("/h")).as_deref(), home);
        assert_eq!(p(Some("rel"), Some("/h")).as_deref(), home);
        assert_eq!(p(Some(""), Some("/h")).as_deref(), home);
        assert_eq!(p(None, None), None);
        assert_eq!(p(None, Some("")), None);
    }

    #[test]
    fn line_holds_only_time_provider_key() {
        assert_eq!(
            line(&entry("espanso", ":date"), 7).as_deref(),
            Some("7\tespanso\t:date\n")
        );
        assert_eq!(line(&entry("x", "super+a\tb"), 7), None);
        assert_eq!(line(&entry("x", "a\nb"), 7), None);
        assert_eq!(line(&entry("x", "a\rb"), 7), None);
        assert_eq!(line(&entry("x", ""), 7), None);
    }
}
