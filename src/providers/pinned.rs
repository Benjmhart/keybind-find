//! Defaults pinned per program version (`data/<program>/<version>.toml`): which file to use for the version installed.

/// The pinned file for `installed`: its own silently; else, with a note naming the file to add, the nearest of the
/// same major version (older first), else the newest older one, else the oldest; the newest when the version is
/// unknown. `program` names the data directory in the note.
pub(crate) fn select(
    program: &str,
    registry: &[(&'static str, &'static str)],
    installed: Option<&str>,
) -> Option<(&'static str, &'static str, Option<String>)> {
    let mut entries: Vec<(Vec<u64>, &'static str, &'static str)> = registry
        .iter()
        .filter_map(|&(v, t)| Some((parse_version(v)?, v, t)))
        .collect();
    entries.sort();
    let &(_, newest, newest_text) = entries.last()?;
    let Some(wanted) = installed.and_then(parse_version) else {
        let note = format!("{program}'s version is unknown: using the defaults of {newest}");
        return Some((newest, newest_text, Some(note)));
    };
    if let Some(&(_, v, t)) = entries.iter().find(|(p, _, _)| *p == wanted) {
        return Some((v, t, None));
    }
    let same = |p: &Vec<u64>| p.first() == wanted.first();
    let older = entries.iter().rev().find(|(p, _, _)| *p < wanted);
    let &(_, v, t) = older
        .filter(|(p, _, _)| same(p))
        .or_else(|| entries.iter().find(|(p, _, _)| same(p) && *p > wanted))
        .or(older)
        .or(entries.first())?;
    let i = installed.unwrap_or_default();
    Some((
        v,
        t,
        Some(format!(
            "{program} {i}: using the defaults of {v}; add data/{program}/{i}.toml"
        )),
    ))
}

/// `0.49.2` → [0, 49, 2]; `None` unless every part is a number.
pub(crate) fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|p| p.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY: &[(&str, &str)] = &[("7.2", "a"), ("8.4", "b"), ("9.2", "c")];

    #[test]
    fn picks_by_nanos_rules() {
        let pick = |v: Option<&str>| select("x", REGISTRY, v).unwrap();
        assert_eq!(pick(Some("8.4")), ("8.4", "b", None));
        // Same major, older first.
        let (v, _, note) = pick(Some("8.6"));
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
        let (v, _, note) = pick(None);
        assert_eq!(v, "9.2");
        assert_eq!(
            note.as_deref(),
            Some("x's version is unknown: using the defaults of 9.2")
        );
        // An unparsable version is unknown.
        assert_eq!(pick(Some("abc")).0, "9.2");
        assert!(select("x", &[], Some("8.4")).is_none());
        // Registry order doesn't matter, and a bad registry version is skipped.
        let shuffled = [("9.2", "c"), ("bad", "z"), ("7.2", "a")];
        assert_eq!(select("x", &shuffled, None).unwrap().0, "9.2");
        assert_eq!(select("x", &shuffled, Some("7.9")).unwrap().0, "7.2");
    }
}
