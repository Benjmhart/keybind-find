//! A keybind-find plugin for the toy format: one binding per line, `key = description`, optionally ending with
//! ` ; scope=value` (where the binding applies). Copy this, rename it `keybind-find-<name>`, and change `parse` and
//! the manifest.

use anyhow::Context as _;
use keybind_find_sheet::{Bind, Key, Manifest, Match, Name, Request, Sheet, VERSION};
use std::collections::BTreeMap;

/// The sheet for one input file. An error names the line; keybind-find shows it as one line and drops the plugin.
fn parse(text: &str) -> anyhow::Result<Sheet> {
    let mut binds = Vec::new();
    // A leading byte-order mark is not part of the first line.
    for (n, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, rest) = line
            .split_once(" = ")
            .with_context(|| format!("line {}: not `key = description`", n + 1))?;
        // Report every binding with its scope; never drop one by guessing where the user is.
        let (desc, scope) = match rest.split_once(" ; ") {
            Some((desc, scope)) => (desc, Some(scope)),
            None => (rest, None),
        };
        // Checked here, so a bad key names its line (the runner checks every key again, without one).
        let key: Key = key
            .trim()
            .parse()
            .with_context(|| format!("line {}", n + 1))?;
        if desc.trim().is_empty() || desc.starts_with("; ") {
            anyhow::bail!("line {}: no description", n + 1);
        }
        let mut when = BTreeMap::new();
        if let Some(scope) = scope {
            let (name, value) = scope
                .split_once('=')
                .with_context(|| format!("line {}: a scope is `name=value`", n + 1))?;
            let parse = |s: &str| {
                s.trim()
                    .parse::<Name>()
                    .with_context(|| format!("line {}", n + 1))
            };
            let (name, value) = (parse(name)?, parse(value)?);
            when.insert(name, value.into());
        }
        binds.push(Bind {
            key: key.to_string(),
            desc: desc.trim().to_string(),
            action: String::new(),
            when,
        });
    }
    Ok(sheet(binds))
}

fn sheet(binds: Vec<Bind>) -> Sheet {
    Sheet {
        title: "Toy".into(),
        provider: "toy".into(),
        binds: BTreeMap::from([("General".to_string(), binds)]),
    }
}

/// Every input's bindings in one sheet, each error naming its file. No inputs (the user has no key file yet) is an
/// empty sheet, not an error.
fn respond(request: &Request) -> anyhow::Result<Sheet> {
    let mut binds = Vec::new();
    for input in &request.inputs {
        let s = parse(&input.text).with_context(|| input.path.clone())?;
        binds.extend(s.binds.into_values().flatten());
    }
    Ok(sheet(binds))
}

/// What `keybind-find plugin add` writes into the user's config, and the defaults a stanza leaves out.
fn manifest() -> Manifest {
    Manifest {
        version: VERSION,
        name: "toy".into(),
        description: "The toy format's key file".into(),
        inputs: vec!["~/.config/toy/keys.conf".into()],
        matches: Some(Match {
            program: Some(vec!["toy".into()]),
            ..Match::default()
        }),
        queries: vec![],
        homepage: Some("https://example.com/keybind-find-toy".into()),
    }
}

fn main() {
    // Answer `--manifest` before `run`, which would wait for a request on stdin.
    if std::env::args().nth(1).as_deref() == Some("--manifest") {
        match serde_json::to_string(&manifest()) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1)
            }
        }
        return;
    }
    // `run_request` reads the request, checks its version and every key, prints the response, and turns an error
    // into one stderr line and exit status 1.
    keybind_find_sheet::run_request(respond)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_sample_gives_the_expected_sheet() {
        keybind_find_sheet::testing::assert_fixture(
            super::parse,
            "tests/fixtures/keys.conf",
            "tests/fixtures/expected.json",
        );
    }

    #[test]
    fn a_line_without_a_description_names_its_line() {
        let e = super::parse("ctrl+s = Save\nctrl+q\n").unwrap_err();
        assert!(e.to_string().starts_with("line 2:"), "{e}");
    }
}
