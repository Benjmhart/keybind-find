//! Defaults pinned per program version (`data/<program>/<version>.toml`): which file to use for the version installed.

use crate::context::run_for;
use crate::sheet::Source;
use anyhow::Context as _;
use std::process::Command;
use std::time::Duration;

/// How long a version probe may run.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// The pinned file for `installed`: its own silently; else, with a note naming the file to add, the nearest of the
/// same major version (older first), else the newest older one, else the oldest; the newest when the version is
/// unknown. `program` names the data directory in the note. The `Source` says which version's file and why.
pub(crate) fn select(
    program: &str,
    registry: &[(&'static str, &'static str)],
    installed: Option<&str>,
) -> Option<(&'static str, &'static str, Option<String>, Source)> {
    let mut entries: Vec<(Vec<u64>, &'static str, &'static str)> = registry
        .iter()
        .filter_map(|&(v, t)| Some((parse_version(v)?, v, t)))
        .collect();
    entries.sort();
    let &(_, newest, newest_text) = entries.last()?;
    let Some(wanted) = installed.and_then(parse_version) else {
        let note = format!("{program}'s version is unknown: using the defaults of {newest}");
        return Some((newest, newest_text, Some(note), source(newest, None, true)));
    };
    let i = installed.unwrap_or_default();
    if let Some(&(_, v, t)) = entries.iter().find(|(p, _, _)| *p == wanted) {
        return Some((v, t, None, source(v, Some(i), false)));
    }
    let same = |p: &Vec<u64>| p.first() == wanted.first();
    let older = entries.iter().rev().find(|(p, _, _)| *p < wanted);
    let &(_, v, t) = older
        .filter(|(p, _, _)| same(p))
        .or_else(|| entries.iter().find(|(p, _, _)| same(p) && *p > wanted))
        .or(older)
        .or(entries.first())?;
    Some((
        v,
        t,
        Some(format!(
            "{program} {i}: using the defaults of {v}; add data/{program}/{i}.toml"
        )),
        source(v, Some(i), true),
    ))
}

/// A checked `Source`: `installed` kept only when `parse_version` accepts it, so nothing else a program printed
/// reaches the picker's header. `mismatch`: the pick wrote its note.
pub(crate) fn source(defaults: &str, installed: Option<&str>, mismatch: bool) -> Source {
    Source {
        defaults: defaults.into(),
        installed: installed
            .filter(|v| parse_version(v).is_some())
            .map(Into::into),
        checked: true,
        mismatch,
    }
}

/// `select` for a provider with one pin, `pin`, by a version probe: `cmd` (the program from `PATH` and its version
/// flag) run with `LC_ALL=C`, no mise auto-install, then `setup` (a test seam), for `VERSION_TIMEOUT`; its stdout read
/// by `version_of`. A probe that fails in any way is an unknown version. The note and the `Source`.
pub(crate) fn probe(
    program: &str,
    pin: &'static str,
    mut cmd: Command,
    setup: impl Fn(&mut Command),
    version_of: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<(Option<String>, Source)> {
    cmd.env("LC_ALL", "C")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut cmd);
    let installed = run_for(cmd, VERSION_TIMEOUT)
        .ok()
        .and_then(|t| version_of(&t));
    let (_, _, note, source) = select(program, &[(pin, "")], installed.as_deref())
        .with_context(|| format!("{program}'s pin {pin} is not a version"))?;
    Ok((note, source))
}

/// The version after `prefix` on `line` (`konsole 26.08.2`, `konsole ` → `26.08.2`), when `parse_version` accepts it.
pub(crate) fn version_after(line: Option<&str>, prefix: &str) -> Option<String> {
    let v = line?.trim().strip_prefix(prefix)?;
    parse_version(v).map(|_| v.to_string())
}

/// `0.49.2` → [0, 49, 2]; `None` unless every part is a number.
pub(crate) fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|p| p.parse().ok()).collect()
}

/// A directory holding `name`, a `sh` script running `body` (a version probe's stand-in), for a `PATH`.
#[cfg(test)]
pub(crate) fn fake_bin(name: &str, body: &str) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = crate::test_tmp().join(format!("kbf-probe-{name}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    // Written by `sh`, not by this process (ETXTBSY: see `xfce.rs`'s `fake`).
    let ok = Command::new("sh")
        .args([
            "-c",
            "printf '#!/bin/sh\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
            "sh",
        ])
        .arg(dir.join(name))
        .arg(body)
        .status()
        .unwrap();
    assert!(ok.success());
    dir
}

/// A `setup` giving the probe only `dir` as its `PATH`.
#[cfg(test)]
pub(crate) fn path_of(dir: &std::path::Path) -> impl Fn(&mut Command) {
    let dir = dir.to_path_buf();
    move |c: &mut Command| {
        c.env("PATH", &dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY: &[(&str, &str)] = &[("7.2", "a"), ("8.4", "b"), ("9.2", "c")];

    #[test]
    fn picks_by_nanos_rules() {
        let pick = |v: Option<&str>| select("x", REGISTRY, v).unwrap();
        let src = |d: &str, i: Option<&str>, mismatch| Source {
            defaults: d.into(),
            installed: i.map(Into::into),
            checked: true,
            mismatch,
        };
        assert_eq!(
            pick(Some("8.4")),
            ("8.4", "b", None, src("8.4", Some("8.4"), false))
        );
        // Same major, older first.
        let (v, _, note, s) = pick(Some("8.6"));
        assert_eq!(s, src("8.4", Some("8.6"), true));
        assert_eq!(v, "8.4");
        assert_eq!(
            note.as_deref(),
            Some("x 8.6: using the defaults of 8.4; add data/x/8.6.toml")
        );
        // Same major, newer when no older one shares it.
        assert_eq!(pick(Some("8.1")).0, "8.4");
        // Nearest older of another major; the oldest when all are newer.
        assert_eq!(pick(Some("10.0")).0, "9.2");
        assert_eq!(pick(Some("5.4")).0, "7.2");
        let (v, _, note, s) = pick(None);
        assert_eq!(v, "9.2");
        assert_eq!(s, src("9.2", None, true));
        assert_eq!(
            note.as_deref(),
            Some("x's version is unknown: using the defaults of 9.2")
        );
        // An unparsable version is unknown.
        assert_eq!(pick(Some("abc")).0, "9.2");
        assert_eq!(pick(Some("abc")).3, src("9.2", None, true));
        assert!(select("x", &[], Some("8.4")).is_none());
        // Registry order doesn't matter, and a bad registry version is skipped.
        let shuffled = [("9.2", "c"), ("bad", "z"), ("7.2", "a")];
        assert_eq!(select("x", &shuffled, None).unwrap().0, "9.2");
        assert_eq!(select("x", &shuffled, Some("7.9")).unwrap().0, "7.2");
    }
}
