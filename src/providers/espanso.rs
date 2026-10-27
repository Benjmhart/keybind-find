//! espanso: the matches active for the focused app, from `espanso match list --json`. Each trigger is one bind whose
//! key is the trigger text, so picking it types the trigger and espanso expands it.

use crate::context::Context;
use crate::sheet::{Bind, Sheet};
use anyhow::Context as _;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DESC_CHARS: usize = 60;
const TIMEOUT: Duration = Duration::from_secs(2);

/// One entry of `match list --json`. Missing and `null` fields both mean empty.
#[derive(Deserialize)]
struct Match {
    #[serde(default)]
    triggers: Option<Vec<String>>,
    #[serde(default)]
    replace: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    list("espanso", &args(ctx), TIMEOUT)
}

/// Runs `program args`, at most `timeout`, and parses its stdout.
fn list(program: &str, args: &[String], timeout: Duration) -> anyhow::Result<Sheet> {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("espanso: not installed")
        }
        r => r.context("espanso: running `espanso match list`")?,
    };
    // Read both pipes from threads so a large listing can't fill a pipe while we wait.
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait().context("espanso: waiting")? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("espanso: `match list` did not answer within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // ponytail: the deadline covers espanso itself, not a process it leaves holding the pipes (espanso 2.4 starts
    // none for `match list`); join on a channel with `recv_timeout` if one ever does.
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        let first = stderr.lines().map(str::trim).find(|l| !l.is_empty());
        anyhow::bail!(
            "espanso: {}",
            first.map_or_else(|| format!("match list failed ({status})"), str::to_string)
        );
    }
    parse(&String::from_utf8_lossy(&stdout))
}

/// `match list --json`, plus `--class=C` / `--title=T` when known. The `=` form keeps a title that starts with `-`
/// (`-h`, `--json`) from being read as an espanso option.
fn args(ctx: &Context) -> Vec<String> {
    let mut a: Vec<String> = ["match", "list", "--json"].map(String::from).into();
    if let Some(c) = &ctx.class {
        a.push(format!("--class={c}"));
    }
    if let Some(t) = &ctx.title {
        a.push(format!("--title={t}"));
    }
    a
}

fn parse(json: &str) -> anyhow::Result<Sheet> {
    let matches: Vec<Match> =
        serde_json::from_str(json).context("espanso: reading `match list --json` output")?;
    let binds = matches
        .iter()
        .flat_map(|m| {
            let desc = desc(m);
            // A trigger with a line break or tab would split or garble its picker line, and can't be typed anyway.
            let triggers = m.triggers.iter().flatten();
            triggers
                .filter(|t| !t.chars().any(char::is_control))
                .map(move |t| Bind {
                    key: t.clone(),
                    desc: desc.clone(),
                    action: String::new(),
                    when: Default::default(),
                })
        })
        .collect();
    Ok(Sheet {
        title: "espanso".into(),
        provider: "espanso".into(),
        binds: BTreeMap::from([("expansions".to_string(), binds)]),
    })
}

/// The label if set, else the first line of the replacement, cut to 60 chars with `…`.
fn desc(m: &Match) -> String {
    if let Some(l) = m.label.as_deref().filter(|l| !l.is_empty()) {
        return l.to_string();
    }
    let first = m
        .replace
        .as_deref()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    let mut d: String = first.chars().take(DESC_CHARS).collect();
    if first.chars().nth(DESC_CHARS).is_some() {
        d.push('…');
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/espanso.json"
    ));

    fn binds(json: &str) -> Vec<Bind> {
        let s = parse(json).unwrap();
        assert_eq!(s.binds.keys().collect::<Vec<_>>(), ["expansions"]);
        s.binds["expansions"].clone()
    }

    fn desc_of(binds: &[Bind], key: &str) -> String {
        binds.iter().find(|b| b.key == key).unwrap().desc.clone()
    }

    fn m(replace: &str, label: Option<&str>) -> Match {
        Match {
            triggers: None,
            replace: Some(replace.into()),
            label: label.map(str::to_string),
        }
    }

    fn ctx(class: Option<&str>, title: Option<&str>) -> Context {
        Context {
            window_id: None,
            class: class.map(str::to_string),
            instance: None,
            title: title.map(str::to_string),
            providers: vec![],
        }
    }

    #[test]
    fn sheet_identity() {
        let s = parse(FIXTURE).unwrap();
        assert_eq!(s.title, "espanso");
        assert_eq!(s.provider, "espanso");
    }

    #[test]
    fn one_bind_per_trigger_in_order() {
        let b = binds(FIXTURE);
        let keys: Vec<_> = b.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, [":date", ":sig", ":signature", ":lorem", ":addr"]);
        assert!(b.iter().all(|b| b.action.is_empty()));
        assert_eq!(desc_of(&b, ":sig"), "Best regards,");
        assert_eq!(desc_of(&b, ":signature"), "Best regards,");
    }

    #[test]
    fn descriptions() {
        let b = binds(FIXTURE);
        assert_eq!(desc_of(&b, ":date"), "Today's date");
        assert_eq!(desc_of(&b, ":addr"), "1 Example Street, Nowhere");
        let lorem = desc_of(&b, ":lorem");
        assert_eq!(lorem.chars().count(), 61, "{lorem}");
        assert!(lorem.ends_with('…'), "{lorem}");
        assert!(lorem.starts_with("Lorem ipsum dolor sit amet"), "{lorem}");
    }

    #[test]
    fn desc_cuts_by_chars_not_bytes() {
        // Exactly 60 chars: nothing cut, no ellipsis.
        let sixty = "é".repeat(60);
        assert_eq!(desc(&m(&sixty, None)), sixty);
        // A multi-byte char right at the boundary.
        let long = format!("{}ü€tail", "a".repeat(59));
        assert_eq!(desc(&m(&long, None)), format!("{}ü…", "a".repeat(59)));
        // A leading newline gives an empty first line, which is acceptable.
        assert_eq!(desc(&m("\nsecond line", None)), "");
        assert_eq!(desc(&m("", None)), "");
        assert_eq!(desc(&m("a\r\nb", None)), "a");
        assert_eq!(desc(&m("x", Some(""))), "x");
        assert_eq!(desc(&m("x", Some("L"))), "L");
    }

    #[test]
    fn lenient_parsing() {
        // Missing fields, unknown fields and `null` fields: no error.
        let b = binds(
            r#"[{"triggers":[":a"]},{"replace":"no triggers"},{"triggers":[":b"],"replace":"r","label":null,"form":true}]"#,
        );
        let keys: Vec<_> = b.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, [":a", ":b"]);
        assert_eq!(desc_of(&b, ":a"), "");
        assert_eq!(binds("[]").len(), 0);
        assert!(parse("not json").is_err());
        assert!(parse(r#"{"triggers":[]}"#).is_err());
        let b = binds(
            r#"[{"triggers":null,"replace":null},{"triggers":[":n"],"replace":null,"label":null}]"#,
        );
        assert_eq!(b.len(), 1);
        assert_eq!(desc_of(&b, ":n"), "");
    }

    #[test]
    fn control_characters_in_triggers_are_skipped() {
        let b = binds(r#"[{"triggers":["a\nb","tab\there","bell\u0007",":ok"],"replace":"r"}]"#);
        let keys: Vec<_> = b.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, [":ok"]);
    }

    fn sh(script: &str) -> anyhow::Result<Sheet> {
        list("sh", &["-c".into(), script.into()], TIMEOUT)
    }

    fn err(r: anyhow::Result<Sheet>) -> String {
        format!("{:#}", r.unwrap_err())
    }

    #[test]
    fn runs_a_command_and_parses_its_output() {
        let s = sh(r#"printf '[{"triggers":[":x"],"replace":"y"}]'"#).unwrap();
        assert_eq!(s.binds["expansions"][0].key, ":x");
        // Output well past a pipe buffer, while stderr is written too.
        let big = sh(
            r#"echo warn >&2; printf '['; i=0; while [ $i -lt 3000 ]; do printf '{"triggers":[":t%d"],"replace":"%s"},' $i xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i+1)); done; printf '{"triggers":[":end"]}]'"#,
        );
        assert_eq!(big.unwrap().binds["expansions"].len(), 3001);
    }

    #[test]
    fn errors_are_one_line() {
        assert_eq!(
            err(list("/nonexistent/espanso", &[], TIMEOUT)),
            "espanso: not installed"
        );
        assert_eq!(
            err(sh(
                "printf '\\n  error: config broken  \\nmore\\n' >&2; exit 3"
            )),
            "espanso: error: config broken"
        );
        let e = err(sh("printf '  \\n' >&2; exit 3"));
        assert!(
            e.starts_with("espanso: match list failed (exit status: 3)"),
            "{e}"
        );
        assert!(!e.contains('\n'), "{e}");
        assert!(err(sh("echo 'not json'")).starts_with("espanso: reading"));
    }

    #[test]
    fn times_out() {
        let t = Instant::now();
        let e = err(list("sleep", &["10".into()], Duration::from_millis(200)));
        assert!(e.contains("did not answer within 200ms"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn trigger_kept_exactly() {
        let b = binds(r#"[{"triggers":[" :sp ", ":ü"],"replace":"r"}]"#);
        assert_eq!(b[0].key, " :sp ");
        assert_eq!(b[1].key, ":ü");
    }

    #[test]
    fn command_args() {
        assert_eq!(args(&ctx(None, None)), ["match", "list", "--json"]);
        assert_eq!(
            args(&ctx(Some("Vivaldi-stable"), Some("a \"b\"; $(c)"))),
            [
                "match",
                "list",
                "--json",
                "--class=Vivaldi-stable",
                "--title=a \"b\"; $(c)"
            ]
        );
        assert_eq!(
            args(&ctx(Some("A"), None)),
            ["match", "list", "--json", "--class=A"]
        );
        // The `--title` `T` form would break here: espanso reads `-h` as its own option (and on 2.4.0 even
        // `--class Alacritty` as two args printed non-JSON), so keep the `=` form.
        assert_eq!(
            args(&ctx(None, Some("-h"))),
            ["match", "list", "--json", "--title=-h"]
        );
    }
}
