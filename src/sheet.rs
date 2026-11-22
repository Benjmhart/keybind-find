//! The DMS cheatsheet model and the key notation, from the `keybind-find-sheet` crate (shared with Rust plugins),
//! and the flattened entries the picker and the runner use.

pub use keybind_find_sheet::{chord, Bind, Name, Sheet, Values};

/// Where a provider's keys came from: the built-in defaults of version `defaults`, and the version installed.
/// `installed` is only ever a version `pinned::parse_version` accepts; `mismatch` is "the version pick wrote its note".
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Source {
    pub defaults: String,
    pub installed: Option<String>,
    pub checked: bool,
    pub mismatch: bool,
}

impl Source {
    /// Defaults of `defaults`, the installed version not probed.
    pub fn unchecked(defaults: &str) -> Source {
        Source {
            defaults: defaults.into(),
            installed: None,
            checked: false,
            mismatch: false,
        }
    }

    /// The picker's header line for `provider`, without colour.
    pub fn line(&self, provider: &str) -> String {
        let installed = match (&self.installed, self.checked) {
            (Some(i), _) => format!("installed {i}"),
            (None, true) => "installed version unknown".into(),
            (None, false) => "installed version not checked".into(),
        };
        format!(
            "{provider}: {} defaults built in ({installed})",
            self.defaults
        )
    }
}

/// A sheet as `dump` prints it: the sheet's own keys, then `source` when the provider returned one.
#[derive(serde::Serialize)]
pub struct Dumped<'a> {
    #[serde(flatten)]
    pub sheet: &'a Sheet,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'a Source>,
}

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
            run: Vec::new(),
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
                run: Vec::new(),
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
    fn source_lines() {
        let mut s = Source::unchecked("1.143.1");
        assert_eq!(
            s.line("vscode"),
            "vscode: 1.143.1 defaults built in (installed version not checked)"
        );
        s.checked = true;
        s.mismatch = true;
        assert_eq!(
            s.line("vscode"),
            "vscode: 1.143.1 defaults built in (installed version unknown)"
        );
        s.installed = Some("1.140.0".into());
        assert_eq!(
            s.line("vscode"),
            "vscode: 1.143.1 defaults built in (installed 1.140.0)"
        );
        s.installed = Some("1.143.1".into());
        s.mismatch = false;
        assert_eq!(
            s.line("vscode"),
            "vscode: 1.143.1 defaults built in (installed 1.143.1)"
        );
    }

    #[test]
    fn dumped_adds_source_last() {
        let s = sheet("x", &[("c", &["k"])]);
        let plain = serde_json::to_string(&Dumped {
            sheet: &s,
            source: None,
        })
        .unwrap();
        assert_eq!(plain, serde_json::to_string(&s).unwrap());
        let mut src = Source::unchecked("1.0");
        let json = |src: &Source| {
            serde_json::to_string(&Dumped {
                sheet: &s,
                source: Some(src),
            })
            .unwrap()
        };
        let base = plain.strip_suffix('}').unwrap();
        assert_eq!(
            json(&src),
            format!(
                r#"{base},"source":{{"defaults":"1.0","installed":null,"checked":false,"mismatch":false}}}}"#
            )
        );
        src.installed = Some("1.1".into());
        src.checked = true;
        src.mismatch = true;
        assert!(json(&src).ends_with(
            r#","source":{"defaults":"1.0","installed":"1.1","checked":true,"mismatch":true}}"#
        ));
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
