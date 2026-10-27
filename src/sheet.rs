//! The DMS cheatsheet model and the key notation, from the `keybind-find-sheet` crate (shared with Rust plugins),
//! and the flattened entries the picker and the runner use.

pub use keybind_find_sheet::{chord, Bind, Name, Sheet, Values};

/// One bind flattened out of its sheet, for the picker and the runner.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub provider: String,
    pub category: String,
    pub bind: Bind,
}

/// Sheet order, then category order (BTreeMap), then bind order.
pub fn entries(sheets: &[Sheet]) -> Vec<Entry> {
    sheets
        .iter()
        .flat_map(|s| {
            s.binds.iter().flat_map(move |(category, binds)| {
                binds.iter().map(move |b| Entry {
                    provider: s.provider.clone(),
                    category: category.clone(),
                    bind: b.clone(),
                })
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(key: &str) -> Bind {
        Bind {
            key: key.into(),
            desc: format!("{key} desc"),
            action: String::new(),
            when: Default::default(),
        }
    }

    fn sheet(provider: &str, cats: &[(&str, &[&str])]) -> Sheet {
        Sheet {
            // Differs from provider, so a test notices if one is used for the other.
            title: format!("{provider} title"),
            provider: provider.into(),
            binds: cats
                .iter()
                .map(|(c, keys)| (c.to_string(), keys.iter().map(|k| bind(k)).collect()))
                .collect(),
        }
    }

    #[test]
    fn dms_json_both_ways() {
        let json = r#"{"title":"xmonad title","provider":"xmonad","binds":{"general":[{"key":"super+b","desc":"Browser","action":""}]}}"#;
        let s: Sheet = serde_json::from_str(json).unwrap();
        let mut want = sheet("xmonad", &[]);
        want.binds.insert(
            "general".into(),
            vec![Bind {
                key: "super+b".into(),
                desc: "Browser".into(),
                action: String::new(),
                when: Default::default(),
            }],
        );
        assert_eq!(s, want);
        // Exactly DMS's fields, in DMS's shape, with an empty action kept.
        assert_eq!(serde_json::to_string(&s).unwrap(), json);
        let back: Sheet = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn empty_binds_is_an_empty_object() {
        let s = sheet("x", &[]);
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"title":"x title","provider":"x","binds":{}}"#
        );
    }

    #[test]
    fn chord_canonical_order() {
        assert_eq!(chord(&["shift", "super"], "k"), "super+shift+k");
        assert_eq!(chord(&["alt", "ctrl"], "Tab"), "ctrl+alt+Tab");
        assert_eq!(
            chord(&["shift", "alt", "ctrl", "super"], "Return"),
            "super+ctrl+alt+shift+Return"
        );
        assert_eq!(chord(&[], "Print"), "Print");
    }

    #[test]
    fn chord_unknown_mods_keep_order_and_repeats_collapse() {
        assert_eq!(
            chord(&["hyper", "shift", "meta"], "k"),
            "shift+hyper+meta+k"
        );
        assert_eq!(chord(&["shift", "super", "shift"], "k"), "super+shift+k");
    }

    #[test]
    fn reads_dms_without_action_and_ignores_extra_fields() {
        let json =
            r#"{"title":"t","provider":"p","extra":1,"binds":{"c":[{"key":"k","desc":"d"}]}}"#;
        let s: Sheet = serde_json::from_str(json).unwrap();
        assert_eq!(s.binds["c"][0].action, "");
        assert!(serde_json::to_string(&s)
            .unwrap()
            .contains(r#""action":"""#));
    }

    #[test]
    fn entries_order() {
        let a = sheet("a", &[("z", &["a1"]), ("m", &["a2", "a3"])]);
        let b = sheet("b", &[("b", &["b1"]), ("a", &["b2"])]);
        let got: Vec<(String, String, String)> = entries(&[a, b])
            .into_iter()
            .map(|e| (e.provider, e.category, e.bind.key))
            .collect();
        let want = [
            ("a", "m", "a2"),
            ("a", "m", "a3"),
            ("a", "z", "a1"),
            ("b", "a", "b2"),
            ("b", "b", "b1"),
        ];
        assert_eq!(
            got,
            want.map(|(p, c, k)| (p.to_string(), c.to_string(), k.to_string()))
        );
    }
}
