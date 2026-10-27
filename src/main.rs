mod capture;
mod config;
mod context;
mod picker;
mod providers;
mod run;
mod scope;
mod sheet;
mod usage;

use context::Context;
use sheet::Sheet;
use std::io::{IsTerminal, Write};
use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "usage: keybind-find [--print] | keybind-find dump [<provider>] | keybind-find keys (Escape stops) \
     | keybind-find plugin list | keybind-find plugin add <name> \
     | keybind-find check-plugin <name|path> [--input FILE]... [--class C] [--program P] [--title T] [--json]";

fn main() -> ExitCode {
    let args: Result<Vec<String>, _> = std::env::args_os()
        .skip(1)
        .map(|a| a.into_string())
        .collect();
    let Ok(args) = args else {
        report(&format!("arguments must be UTF-8. {USAGE}"), true);
        return ExitCode::from(2);
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // `notify`: the forms a hotkey runs, where an error should also reach the screen. A mistyped binding counts.
    let (result, notify) = match args.as_slice() {
        ["-h" | "--help"] => (print_out(USAGE), false),
        [] => (pick_and_run(false), true),
        ["--print"] => (pick_and_run(true), true),
        ["dump"] => (dump_all(), false),
        ["dump", name] => (dump(name), false),
        // Diagnostic: print the chord for each key pressed, until Escape.
        ["keys"] => (keys(), false),
        ["plugin", "list"] => (
            providers::plugins::list().and_then(|t| print_out(&t)),
            false,
        ),
        ["plugin", "add", name] => (
            providers::plugins::add_to_config(name).and_then(|t| print_out(&t)),
            false,
        ),
        ["check-plugin", rest @ ..] => (check_plugin(rest), false),
        // Internal: the fzf half of the picker, run inside alacritty by `picker::pick`. Not in USAGE.
        ["__fzf", list, out] => (
            picker::run_fzf(std::path::Path::new(list), std::path::Path::new(out)),
            false,
        ),
        // Internal: run by fzf's `ctrl-k` binding to start a key search in the `__fzf` process. Not in USAGE.
        ["__capture-signal", socket] => (
            picker::signal_key_search(std::path::Path::new(socket)),
            false,
        ),
        _ => {
            report(USAGE, true);
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report(&format!("{e:#}"), notify);
            ExitCode::FAILURE
        }
    }
}

/// Prints the report; an error (exit 1) when it found problems, so exit 0 means none.
fn check_plugin(args: &[&str]) -> anyhow::Result<()> {
    let (report, ok) = providers::plugins::check(args)?;
    print_out(&report)?;
    anyhow::ensure!(ok, "check-plugin: the plugin has problems");
    Ok(())
}

/// Detect the context, show every binding that applies, and send the one picked (or print it with `dry_run`).
fn pick_and_run(dry_run: bool) -> anyhow::Result<()> {
    let ctx = context::detect();
    let sheets = collect(&ctx)?;
    // What the running nvim reported (its mode and filetype) hides what can't apply; other scopes show as tags.
    let mut entries = scope::shown(&sheets, &context::facts())?;
    usage::rank(&mut entries, &usage::load());
    // A JSON frontend is told the window, with what the running nvim reported.
    let mut window = context::window();
    window.facts = context::facts();
    let Some((i, print)) = picker::pick(&entries, &window)? else {
        return Ok(());
    };
    // A frontend's `print` is `--print` for this pick: keys printed, nothing sent or logged.
    let dry_run = dry_run || print;
    run::send(ctx.window_id.as_deref(), &entries[i], dry_run)?;
    if !dry_run {
        if let Err(e) = usage::record(&entries[i]) {
            eprintln!("keybind-find: usage log: {e:#}");
        }
    }
    Ok(())
}

/// Prints each chord as it is pressed. Unlike `print_out`, a closed stdout ends the capture, at the next key pressed:
/// the keyboard must not stay grabbed for a reader that has gone (`keys | head -1`). That end is not an error.
fn keys() -> anyhow::Result<()> {
    let print = |k: capture::Key| match k {
        capture::Key::Chord(c) => Ok(writeln!(std::io::stdout().lock(), "{c}")?),
        _ => Ok(()),
    };
    match capture::capture(print) {
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        r => r,
    }
}

fn dump(name: &str) -> anyhow::Result<()> {
    let ctx = context::detect();
    let sheet = providers::sheet(name, &ctx)?;
    print_out(&serde_json::to_string_pretty(&sheet)?)
}

fn dump_all() -> anyhow::Result<()> {
    let ctx = context::detect();
    print_out(&serde_json::to_string_pretty(&collect(&ctx)?)?)
}

fn collect(ctx: &Context) -> anyhow::Result<Vec<Sheet>> {
    collect_with(&ctx.providers, |name| providers::sheet(name, ctx))
}

/// Runs `sheet` for every provider at once and keeps the ones that worked, in `names` order. Each failure is one
/// stderr line; if all fail, that's an error.
fn collect_with(
    names: &[&str],
    sheet: impl Fn(&str) -> anyhow::Result<Sheet> + Sync,
) -> anyhow::Result<Vec<Sheet>> {
    let results: Vec<_> = std::thread::scope(|s| {
        let sheet = &sheet;
        let handles: Vec<_> = names
            .iter()
            .map(|&name| (name, s.spawn(move || sheet(name))))
            .collect();
        handles
            .into_iter()
            .map(|(name, h)| {
                let r = h.join().unwrap_or_else(|p| Err(panic_error(p.as_ref())));
                (name, r)
            })
            .collect()
    });
    let mut sheets = Vec::new();
    for (name, r) in results {
        match r {
            Ok(s) => sheets.push(s),
            Err(e) => eprintln!("keybind-find: {}", provider_error(name, &e)),
        }
    }
    if sheets.is_empty() {
        anyhow::bail!(
            "no bindings to show: every provider failed ({})",
            names.join(", ")
        );
    }
    Ok(sheets)
}

/// A provider thread's panic as an error carrying the panic message.
fn panic_error(payload: &(dyn std::any::Any + Send)) -> anyhow::Error {
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default();
    anyhow::anyhow!("panicked: {msg}")
}

/// `<provider>: <error>` on one line, without repeating the name when the error already starts with it.
fn provider_error(name: &str, e: &anyhow::Error) -> String {
    let msg = format!("{e:#}").replace('\n', " ");
    let prefix = format!("{name}: ");
    let msg = msg.strip_prefix(&prefix).unwrap_or(&msg);
    format!("{name}: {msg}")
}

/// One stderr line. Under a hotkey nobody sees stderr, so with `notify` and no terminal on stderr, also show it as a
/// desktop notification if `notify-send` exists. Not waited for: a stuck notification daemon mustn't hold us up.
fn report(msg: &str, notify: bool) {
    eprintln!("keybind-find: {msg}");
    if notify && !std::io::stderr().is_terminal() {
        let _ = Command::new("notify-send")
            // No `--`: libnotify 0.8.9's notify-send rejects it. A leading space keeps a body from reading as an option.
            .args(["keybind-find", &format!(" {}", markup_escape(msg))])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Notification bodies are markup: a `<` or `&` from a path or window title would show wrongly.
fn markup_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `println!` panics when the reader goes away (`dump | head`); a closed pipe is not an error here.
fn print_out(s: &str) -> anyhow::Result<()> {
    match writeln!(std::io::stdout().lock(), "{s}") {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn ok(provider: &str) -> anyhow::Result<Sheet> {
        Ok(Sheet {
            title: provider.into(),
            provider: provider.into(),
            binds: BTreeMap::new(),
        })
    }

    #[test]
    fn failed_providers_are_dropped_in_order() {
        let got = collect_with(&["a", "bad", "c"], |n| match n {
            "bad" => anyhow::bail!("bad: broken"),
            _ => ok(n),
        })
        .unwrap();
        let names: Vec<_> = got.iter().map(|s| s.provider.as_str()).collect();
        assert_eq!(names, ["a", "c"]);
    }

    #[test]
    fn all_failing_is_an_error() {
        let e = collect_with(&["a", "b"], |_| anyhow::bail!("no")).unwrap_err();
        assert!(e.to_string().contains("a, b"), "{e}");
        assert!(collect_with(&[], ok).is_err());
    }

    #[test]
    fn a_panicking_provider_is_one_failure() {
        let got = collect_with(&["a", "boom"], |n| {
            if n == "boom" {
                panic!("boom");
            }
            ok(n)
        })
        .unwrap();
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn providers_run_at_the_same_time() {
        let t = std::time::Instant::now();
        let got = collect_with(&["a", "b", "c"], |n| {
            std::thread::sleep(std::time::Duration::from_millis(300));
            ok(n)
        })
        .unwrap();
        assert_eq!(got.len(), 3);
        assert!(
            t.elapsed() < std::time::Duration::from_millis(800),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn panic_message_is_kept() {
        let p = std::thread::spawn(|| panic!("index {}", 3))
            .join()
            .unwrap_err();
        assert_eq!(panic_error(p.as_ref()).to_string(), "panicked: index 3");
        let p = std::thread::spawn(|| panic!("plain")).join().unwrap_err();
        assert_eq!(panic_error(p.as_ref()).to_string(), "panicked: plain");
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(markup_escape("a <b> & c"), "a &lt;b&gt; &amp; c");
    }

    #[test]
    fn check_plugin_fails_only_on_problems() {
        let fixture =
            |n: &str| format!("{}/tests/fixtures/plugins/{n}", env!("CARGO_MANIFEST_DIR"));
        let e = check_plugin(&[&fixture("bad-key.sh"), "--json"]).unwrap_err();
        assert!(e.to_string().contains("has problems"), "{e}");
        // A good plugin: a manifest, a match and a valid sheet.
        let good = std::env::temp_dir().join(format!("kbf-main-good-{}.sh", std::process::id()));
        let script = "#!/bin/sh\nif [ \"$1\" = --manifest ]; then echo '{\"version\": 1, \"name\": \"good\", \"match\": {\"class\": [\"Gimp\"]}}'; exit 0; fi\n\
             cat >/dev/null\necho '{\"version\": 1, \"title\": \"t\", \"provider\": \"p\", \"binds\": {\"A\": [{\"key\": \"ctrl+k\", \"desc\": \"d\"}]}}'\n";
        // Written by `sh`, not this process (see `plugins::tests::write_script`: ETXTBSY).
        let written = Command::new("sh")
            .args([
                "-c",
                "printf '%s' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(&good)
            .arg(script)
            .status()
            .unwrap();
        assert!(written.success());
        assert!(check_plugin(&[&good.display().to_string()]).is_ok());
        let _ = std::fs::remove_file(&good);
    }

    #[test]
    fn provider_error_line() {
        let e = anyhow::anyhow!("espanso: not installed");
        assert_eq!(provider_error("espanso", &e), "espanso: not installed");
        let e = anyhow::anyhow!("dump missing\nsecond line").context("reading");
        assert_eq!(
            provider_error("xmonad", &e),
            "xmonad: reading: dump missing second line"
        );
    }
}
