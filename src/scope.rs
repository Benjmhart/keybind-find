//! `when` scopes: hide the bindings a known fact rules out, and tag the scopes nothing can check.

use crate::sheet::{self, Entry, Name, Sheet, Values};
use std::collections::BTreeMap;

/// What keybind-find knows about where the user is, scope → value (`mode` → `normal`), as in `Context::facts`.
pub type Facts = BTreeMap<Name, Name>;

/// The sheets' entries that `facts` don't rule out (see `apply`); an error if that leaves none, which says how many
/// the facts hid.
pub fn shown(sheets: &[Sheet], facts: &Facts) -> anyhow::Result<Vec<Entry>> {
    let mut entries = sheet::entries(sheets);
    let all = entries.len();
    apply(&mut entries, facts);
    match all {
        0 => anyhow::bail!("no bindings to show for this window"),
        _ if entries.is_empty() => {
            let facts: Vec<String> = facts.iter().map(|(k, v)| format!("{k}={v}")).collect();
            anyhow::bail!(
                "no bindings to show for this window: what keybind-find knows ({}) rules out all {all}",
                facts.join(" ")
            )
        }
        _ => Ok(entries),
    }
}

/// Drops each entry whose `when` has a scope that is a fact with a value not among the scope's, and takes the
/// scopes the facts confirm out of the rest. What stays in an entry's `when` is the part no fact covers, which the
/// picker shows as tags (`tags`). A scope with no fact never hides anything.
pub fn apply(entries: &mut Vec<Entry>, facts: &Facts) {
    entries.retain_mut(|e| {
        let mut applies = true;
        e.bind.when.retain(|scope, values| match facts.get(scope) {
            None => true,
            Some(fact) => {
                applies &= values.contains(fact.as_str());
                false
            }
        });
        applies
    });
}

/// ` [filetype=rust,toml] [mode=insert]`, one tag per scope in name order, to follow the description; `""` for
/// none. `Name` has no `,`, `=`, `]` or spaces, so a tag reads only one way.
pub fn tags(when: &BTreeMap<Name, Values>) -> String {
    when.iter()
        .map(|(scope, values)| {
            let values: Vec<&str> = values.iter().map(Name::as_str).collect();
            format!(" [{scope}={}]", values.join(","))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Bind;

    fn name(s: &str) -> Name {
        s.parse().unwrap()
    }

    fn entry(key: &str, when: &[(&str, &[&str])]) -> Entry {
        Entry {
            provider: "nvim".into(),
            category: "c".into(),
            bind: Bind {
                key: key.into(),
                desc: "d".into(),
                action: String::new(),
                when: when
                    .iter()
                    .map(|(s, v)| {
                        let v: Vec<Name> = v.iter().map(|v| name(v)).collect();
                        (name(s), v.try_into().unwrap())
                    })
                    .collect(),
            },
        }
    }

    fn facts(f: &[(&str, &str)]) -> Facts {
        f.iter().map(|(k, v)| (name(k), name(v))).collect()
    }

    /// The keys kept, each with the tags it would show.
    fn kept(f: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut entries = vec![
            entry("none", &[]),
            entry("insert", &[("mode", &["insert"])]),
            entry("normal", &[("mode", &["normal"])]),
            entry("rust", &[("filetype", &["rust", "toml"])]),
            entry(
                "both",
                &[("mode", &["normal", "visual"]), ("filetype", &["lua"])],
            ),
            entry("later", &[("lsp", &["attached"])]),
        ];
        apply(&mut entries, &facts(f));
        entries
            .into_iter()
            .map(|e| (e.bind.key, tags(&e.bind.when)))
            .collect()
    }

    fn want(rows: &[(&str, &str)]) -> Vec<(String, String)> {
        rows.iter()
            .map(|(k, t)| (k.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn no_facts_keeps_everything_tagged() {
        assert_eq!(
            kept(&[]),
            want(&[
                ("none", ""),
                ("insert", " [mode=insert]"),
                ("normal", " [mode=normal]"),
                ("rust", " [filetype=rust,toml]"),
                ("both", " [filetype=lua] [mode=normal,visual]"),
                ("later", " [lsp=attached]"),
            ])
        );
    }

    #[test]
    fn a_fact_hides_what_it_contradicts_and_untags_what_it_matches() {
        assert_eq!(
            kept(&[("mode", "normal")]),
            want(&[
                ("none", ""),
                ("normal", ""),
                ("rust", " [filetype=rust,toml]"),
                ("both", " [filetype=lua]"),
                ("later", " [lsp=attached]"),
            ])
        );
    }

    #[test]
    fn list_values_match_any_and_every_scope_must_hold() {
        assert_eq!(
            kept(&[("mode", "visual"), ("filetype", "toml")]),
            want(&[("none", ""), ("rust", ""), ("later", " [lsp=attached]")])
        );
        assert_eq!(
            kept(&[("mode", "visual"), ("filetype", "lua")]),
            want(&[("none", ""), ("both", ""), ("later", " [lsp=attached]")])
        );
    }

    #[test]
    fn a_fact_no_binding_names_or_a_scope_no_fact_names_hides_nothing() {
        assert_eq!(kept(&[("buftype", "help")]), kept(&[]));
    }

    #[test]
    fn shown_filters_the_sheets_and_refuses_an_empty_list() {
        let sheet = Sheet {
            title: "nvim".into(),
            provider: "nvim".into(),
            binds: BTreeMap::from([(
                "insert".to_string(),
                vec![entry("i", &[("mode", &["insert"])]).bind],
            )]),
        };
        let sheets = [sheet];
        let tagged = shown(&sheets, &facts(&[])).unwrap();
        assert_eq!(tags(&tagged[0].bind.when), " [mode=insert]");
        assert!(shown(&sheets, &facts(&[("mode", "insert")])).unwrap()[0]
            .bind
            .when
            .is_empty());
        let e = shown(&sheets, &facts(&[("mode", "normal")])).unwrap_err();
        assert_eq!(
            e.to_string(),
            "no bindings to show for this window: what keybind-find knows (mode=normal) rules out all 1"
        );
        let e = shown(&[], &facts(&[])).unwrap_err();
        assert_eq!(e.to_string(), "no bindings to show for this window");
    }
}
