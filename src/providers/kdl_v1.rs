//! KDL v1 documents (zellij's config and presets) through the `kdl` crate: a parse whose error names
//! its line, and a node's line. Each provider walks the document itself.

use anyhow::Context as _;
use kdl::{KdlDocument, KdlNode};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

/// The parser (kdl's v1, nom) recurses once per `{ }` level, once per `*`, `/` or run of other text in a `/* */`
/// comment, and once per `/-` it is inside of (a slashdashed node's or entry's frames stay open across its `{ }`
/// block, through line continuations and terminators). A stack overflow aborts the whole process. So it runs on a
/// thread of its own with `STACK`, and `parse` first refuses a text whose deepest `{` level plus its `/-` in all come
/// to more than `MAX_FRAMES`, or with a comment of more than `MAX_COMMENT` parts. zellij's own parser (the same kdl 4)
/// runs on its 8 MiB main thread, so text nested or commented past these is text zellij can't load either.
/// Measured 2026-11-17 in a debug build (release takes ~7-30x more): on 2 MiB, ~143 levels, ~140 chained `/-` or ~684
/// comment parts. The worst texts within both bounds (512 frames as levels; as levels each slashdashed, by
/// `/-b {`, `a /- {`, `/-b \` + newline + `{`, or `/-a {} /-{`; as levels then a chain; each with a comment of 8,192
/// parts at the leaf, three comment shapes) all parse on 32 MiB and abort on 24 MiB, so `STACK` leaves twice what
/// they need; a bigger `MAX_FRAMES` would eat that margin.
/// zellij's files nest 4 deep; the whole 0.45 default config commented out is about 1,400 comment parts.
const STACK: usize = 64 << 20;
const MAX_FRAMES: usize = 512;
const MAX_COMMENT: usize = 8192;

/// How long the parser may take. Some short texts send kdl 4's parser into exponential backtracking (slashdashed
/// entries and children, re-parsed at each level); a real config parses in milliseconds. 2 s, as each zellij run
/// gets: the picker waits on this.
const TIMEOUT: Duration = Duration::from_secs(2);

/// `text` as a KDL v1 document; a failure is `line N: <message>`, from the first diagnostic.
pub(crate) fn parse(text: &str) -> anyhow::Result<KdlDocument> {
    parse_within(text, TIMEOUT)
}

/// `parse` with its own time limit (tests give real-sized texts a long one: a debug build on a loaded machine can
/// take more than 2 s on them).
fn parse_within(text: &str, timeout: Duration) -> anyhow::Result<KdlDocument> {
    if let Some((at, what)) = too_deep(text) {
        anyhow::bail!("line {}: {what}", line_at(text, at));
    }
    let (send, receive) = std::sync::mpsc::channel();
    let owned = text.to_string();
    // ponytail: a parse that times out is left running on its thread, burning a core until the process (one picker
    // run) exits; kdl's parser can't be stopped.
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            let _ = send.send(KdlDocument::parse_v1(&owned).map_err(
                |e| match e.diagnostics.first() {
                    Some(d) => anyhow::anyhow!(
                        "line {}: {}",
                        line_at(&owned, d.span.offset()),
                        d.message.as_deref().unwrap_or("not KDL")
                    ),
                    None => anyhow::anyhow!("not KDL"),
                },
            ));
        })
        .context("the KDL parser failed: can't start its thread")?;
    match receive.recv_timeout(timeout) {
        Ok(parsed) => parsed,
        Err(RecvTimeoutError::Timeout) => {
            anyhow::bail!("the file took longer than {}s to parse", timeout.as_secs())
        }
        // The thread ended without a result: a panic. The default panic hook has already printed its own line;
        // kdl 4.7.1's panics are believed unreachable from `parse_v1`.
        Err(RecvTimeoutError::Disconnected) => anyhow::bail!("the KDL parser failed (a panic)"),
    }
}

/// Where `text` goes past `MAX_FRAMES` or `MAX_COMMENT`, and which: a scan of KDL v1's strings (`"…"` with escapes,
/// `r#"…"#` raw), `//` line comments (to any of v1's newlines) and nested `/* */` comments, adding up outside them the
/// parser frames open at once. Each is a `{` level or a `/-` (`slashdash_comment` and `node_slashdash` frames stay open
/// across a slashdashed block, and other `/-` follow them through line continuations and terminators), so the deepest
/// level plus every `/-` in the file bounds them, however the parser closes them. ponytail: a sound bound, not the
/// parser's: a config with more than ~500 `/-` in all is refused, though it would parse; a real one holds a handful.
fn too_deep(text: &str) -> Option<(usize, String)> {
    let b = text.as_bytes();
    let at = |i: usize, s: &[u8]| b[i..].starts_with(s);
    let newline = |i: usize| {
        [
            &b"\n"[..],
            b"\r",
            b"\x0c",
            b"\xc2\x85",
            b"\xe2\x80\xa8",
            b"\xe2\x80\xa9",
        ]
        .iter()
        .any(|n| at(i, n))
    };
    // The deepest `{` level so far, the level now, and every `/-` so far: their sum bounds the frames open at once,
    // whatever ends a node or a slashdash.
    let (mut i, mut deepest, mut level, mut slashdashes) = (0, 0usize, 0usize, 0usize);
    let refuse = |i: usize| {
        Some((
            i,
            format!("more than {MAX_FRAMES} `{{` levels and `/-` together"),
        ))
    };
    while i < b.len() {
        let hashes = match b[i] {
            b'r' => b[i + 1..].iter().take_while(|&&c| c == b'#').count(),
            _ => 0,
        };
        // ponytail: any `r"`/`r#"` counts, even ending a name (`bar"`): that text is no KDL, and the parser stops there.
        let raw = b[i] == b'r' && b.get(i + 1 + hashes) == Some(&b'"');
        if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
        } else if raw {
            // No escapes: up to a `"` and as many `#`.
            let close = [&b"\""[..], &vec![b'#'; hashes]].concat();
            i += 2 + hashes;
            while i < b.len() && !at(i, &close) {
                i += 1;
            }
        } else if at(i, b"//") {
            while i < b.len() && !newline(i) {
                i += 1;
            }
            continue;
        } else if at(i, b"/*") {
            // Each `*`, `/` and run of other text is a step of the parser's.
            let (start, mut open, mut parts, mut run) = (i, 0usize, 0usize, false);
            while i < b.len() && (open > 0 || i == start) {
                if at(i, b"/*") || at(i, b"*/") {
                    open = if b[i] == b'/' { open + 1 } else { open - 1 };
                    (parts, run) = (parts + 2, false);
                    i += 2;
                } else {
                    let mark = b[i] == b'*' || b[i] == b'/';
                    parts += usize::from(mark || !run);
                    run = !mark;
                    i += 1;
                }
                if parts > MAX_COMMENT {
                    return Some((
                        start,
                        format!("a /* */ comment of more than {MAX_COMMENT} parts (`*`, `/` and the text between)"),
                    ));
                }
            }
            continue;
        } else if at(i, b"/-") {
            slashdashes += 1;
            if deepest + slashdashes > MAX_FRAMES {
                return refuse(i);
            }
            i += 1;
        } else if b[i] == b'{' {
            level += 1;
            deepest = deepest.max(level);
            if deepest + slashdashes > MAX_FRAMES {
                return refuse(i);
            }
        } else if b[i] == b'}' {
            level = level.saturating_sub(1);
        }
        i += 1;
    }
    None
}

/// `node`'s 1-based line in `text`, the document it was parsed from.
pub(crate) fn line(text: &str, node: &KdlNode) -> usize {
    line_at(text, node.span().offset())
}

/// The 1-based line of byte `offset` (spans are in bytes); past the end, the last line.
fn line_at(text: &str, offset: usize) -> usize {
    let end = text.floor_char_boundary(offset.min(text.len()));
    text[..end].matches('\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEEP: &str = "more than 512 `{` levels and `/-` together";

    /// The parse the tests use: a long limit, so a slow debug build under load fails nothing (`slow_texts_time_out`
    /// tests the real one).
    fn parse(text: &str) -> anyhow::Result<KdlDocument> {
        parse_within(text, Duration::from_secs(120))
    }

    fn err(text: &str) -> String {
        format!("{:#}", parse(text).unwrap_err())
    }

    #[test]
    fn errors_name_the_line() {
        // An unclosed string runs to the end of the text, where kdl reports it: the last line.
        let e = err("a 1\nb 2\nc \"unclosed");
        assert!(e.starts_with("line 3: "), "{e}");
        assert!(err("a\nb\nc 1.\nd\n").starts_with("line 3: Expected valid value"));
        // The offset is in bytes: twenty `é`s (two bytes each) on line 1 would move a char offset back two lines.
        let e = err(&format!("a \"{}\"\nb\nc 1.\nd\n", "é".repeat(20)));
        assert!(e.starts_with("line 3: "), "{e}");
        assert!(err("x }\n").starts_with("line 1: "));
    }

    #[test]
    fn deep_texts_are_one_error() {
        // Each of these overflowed the parser's stack on a 2 MiB thread (this test's own) and aborted.
        let commented = format!("/* {} */\na 1\n", "* ".repeat(4000));
        assert!(parse(&commented).is_ok());
        let d = std::fs::read_to_string(format!(
            "{}/tests/fixtures/zellij/0.45.1/default.kdl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        assert!(parse(&format!("/*\n{d}\n*/\n").repeat(10)).is_ok());
        // Ten in one comment are over the bound: one error.
        assert!(err(&format!("/*\n{}\n*/\n", d.repeat(10))).contains("comment of more than"));
        let nested = |n: usize| format!("{}{}", "b {\n".repeat(n), "}\n".repeat(n));
        assert!(parse(&nested(512)).is_ok());
        // Past the bounds: one error each.
        assert_eq!(err(&nested(513)), format!("line 513: {DEEP}"));
        assert_eq!(
            err(&format!("a\n{}", "b {".repeat(100_000))),
            format!("line 2: {DEEP}")
        );
        let e = "line 2: a /* */ comment of more than 8192 parts (`*`, `/` and the text between)";
        assert_eq!(err(&format!("a\n/* {} */\n", "* ".repeat(20_000))), e);
        // `/*`, then 8,189 parts (a space, the `/`s), then `*/`: 8,193.
        assert_eq!(err(&format!("a\n/* {} */\n", "/".repeat(8187))), e);
        assert!(parse(&format!("a\n/* {} */\n", "/".repeat(8186))).is_ok());
        // Text between marks counts too: `*a` is two parts.
        assert_eq!(err(&format!("a\n/*{}*/\n", "*a".repeat(4095))), e);
        assert!(parse(&format!("a\n/*{}*/\n", "*a".repeat(4094))).is_ok());
        assert_eq!(err(&format!("a\n{}", "/*".repeat(5000))), e);
        // `/-` count with the levels: a chain, of nodes or of entries, whitespace and comments between.
        let chain = format!("line 1: {DEEP}");
        assert_eq!(err(&format!("{}a\n", "/-".repeat(30_000))), chain);
        assert_eq!(err(&format!("a {}1\n", "/- ".repeat(50_000))), chain);
        assert!(err(&format!("a {}1\n", "/- /* c */ // c\n".repeat(600))).ends_with(DEEP));
        assert!(!err(&format!("{}a\n", "/-".repeat(512))).ends_with(DEEP));
        // Also through `\` line continuations and Unicode spaces.
        for t in [
            format!("a {}1\n", "/- \\\n".repeat(100_000)),
            format!("{}a\n", "/-\u{3000}".repeat(100_000)),
            format!("a {}1\n", "/-\u{a0}".repeat(100_000)),
            format!("{}a\n", "/-\u{feff}".repeat(1_000_000)),
        ] {
            assert!(err(&t).ends_with(DEEP), "{}", &t[..12]);
        }
        // Slashdashes before each `{` stay open through its block, also past a line continuation or a terminator's
        // own `/-{…}`: they add up with the levels.
        let n = 256;
        for t in [
            format!("a {}{{\n", "/- ".repeat(n)).repeat(255) + "c\n" + &"}\n".repeat(255),
            format!("{}b {{\n", "/-".repeat(n)).repeat(255) + "c\n" + &"}\n".repeat(255),
            format!("a {}{{\n", "/- \\\n".repeat(n)).repeat(255) + "c\n" + &"}\n".repeat(255),
            format!("{}b \\\n{{\n", "/-".repeat(n)).repeat(255) + "c\n" + &"}\n".repeat(255),
            format!("{}a {{}} /-{{\n", "/-".repeat(254)).repeat(128) + "c\n" + &"}\n".repeat(128),
        ] {
            assert!(err(&t).ends_with(DEEP), "{}", &t[..12]);
        }
        // A chain inside deep levels, with no `{` after it.
        let inside = format!(
            "{}a {}1\n{}",
            "b {\n".repeat(300),
            "/- ".repeat(300),
            "}\n".repeat(300)
        );
        assert_eq!(err(&inside), format!("line 301: {DEEP}"));
        // The deepest level counts, even closed before the `/-`; levels side by side don't add up.
        let after = format!("{}x {{ }}\na {}1\n", nested(300), "/- ".repeat(300));
        assert!(err(&after).ends_with(DEEP));
        assert!(parse(&"a { }\n".repeat(600)).is_ok());
        // Up to ~500 `/-` in all, wherever they are, parse.
        let lines =
            "/-a 1\nb /-2\n".repeat(100) + &"c { /-d; }\n".repeat(100) + &"e /-1; ".repeat(100);
        assert!(parse(&lines).is_ok(), "{:#}", parse(&lines).unwrap_err());
        // 300 separate slashdashed binds.
        let binds = format!(
            "keybinds {{\n  normal {{\n{}  }}\n}}\n",
            "    bind \"a\" { Quit; }\n    /-bind \"c\" {\n      Quit\n    }\n".repeat(300)
        );
        assert!(parse(&binds).is_ok(), "{:#}", parse(&binds).unwrap_err());
        // A `//` comment ends at any of KDL v1's newlines, and what follows counts.
        for nl in ["\r", "\r\n", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}"] {
            let t = format!("// x{nl}{}", "b {".repeat(100_000));
            assert!(err(&t).ends_with(DEEP), "{nl:?}");
        }
        assert!(parse(&format!("// x\ra \"\n{}\"\n", "{".repeat(300))).is_ok());
        // Many `#` (no `r` before them) scan in linear time.
        let started = std::time::Instant::now();
        assert!(too_deep(&"#".repeat(400_000)).is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // A raw string ends at its quote and hashes, with no escapes: the braces after it count.
        assert_eq!(
            err(&format!("a r\"\\\"\n{}", "b {".repeat(3000))),
            format!("line 2: {DEEP}")
        );
        assert_eq!(
            err(&format!("a r#\"\"\"#\n{}", "b {".repeat(3000))),
            format!("line 2: {DEEP}")
        );
        // Braces in strings (an escaped quote inside, a raw one) and comments (nested) don't count.
        let quoted = format!(
            "{}a \"{{\\\"{{\" r##\"{{\"#{{\"## // {{{{\n/* {{ /* {{ */ {{ */ c\n{}",
            "b {\n".repeat(512),
            "}\n".repeat(512)
        );
        assert!(parse(&quoted).is_ok(), "{:#}", parse(&quoted).unwrap_err());
        // A raw string ends only at its quote with as many `#`: a `"` alone inside doesn't end it.
        let raw = format!(
            "{}a r#\"x\"{}\"#\n{}",
            "b {\n".repeat(510),
            "{".repeat(10),
            "}\n".repeat(510)
        );
        assert!(parse(&raw).is_ok(), "{:#}", parse(&raw).unwrap_err());
        // Extra closing braces never take the depth below zero.
        assert_eq!(too_deep(&format!("}}}}{}", "{".repeat(512))), None);
        assert!(too_deep(&format!("}}}}{}", "{".repeat(513))).is_some());
    }

    #[test]
    fn the_bounds_fit_the_stack() {
        // The worst texts within all bounds: 512 frames as levels, or as levels then a chain, then a comment of 8,192
        // parts in each shape (marks alone, marks and text, nested comments). A text ending in a chain is a parse
        // error at its end; every one returns, no abort. (Levels each with a `/-` before their `{` backtrack past the
        // timeout; they were measured outside the tests: see `STACK`.)
        let levels = |n: usize, open: &str| (open.repeat(n), "}\n".repeat(n));
        for body in [
            "*".repeat(8188),
            "*a".repeat(4094),
            "/*".repeat(2047) + &"*/".repeat(2047),
        ] {
            for (open, close, chain) in [
                (levels(511, "b {\n"), "", 0),
                (levels(255, "b {\n"), "a ", 256),
            ]
            .map(|((o, c), x, n)| (o + x, c, n))
            {
                let t = format!("{open}{}/*{body}*/ c\n{close}", "/- ".repeat(chain));
                assert!(too_deep(&t).is_none(), "{}", &open[..6]);
                let _ = parse(&t);
            }
        }
    }

    #[test]
    fn slow_texts_time_out() {
        // kdl 4's parser backtracks exponentially on these (each doubles or triples per `n`): one error at the
        // timeout, the parse left running.
        let n = 24;
        for t in [
            format!("a {}{}\n", "/- ".repeat(n), "{} ".repeat(n)),
            "/-b {\n".repeat(n) + &"}\n".repeat(n),
            format!(
                "{}/*{}*/\n{}",
                "a /-{\n".repeat(255),
                "*".repeat(8188),
                "}\n".repeat(255)
            ),
        ] {
            assert!(too_deep(&t).is_none());
            let started = std::time::Instant::now();
            assert_eq!(
                format!("{:#}", super::parse(&t).unwrap_err()),
                "the file took longer than 2s to parse"
            );
            let took = started.elapsed();
            assert!(took >= TIMEOUT && took < Duration::from_secs(3), "{took:?}");
        }
    }

    #[test]
    fn node_lines() {
        let text = "a\n// c\nb { x; }\n\"é\" 1\nd\n";
        let doc = parse(text).unwrap();
        let lines: Vec<usize> = doc.nodes().iter().map(|n| line(text, n)).collect();
        assert_eq!(lines, [1, 3, 4, 5]);
        // A child node's line too.
        let child = &doc.nodes()[1].children().unwrap().nodes()[0];
        assert_eq!(line(text, child), 3);
        assert_eq!(line_at("a\nb", 99), 2);
        assert_eq!(line_at("é", 1), 1);
    }

    #[test]
    fn v1_that_v2_rejects() {
        let text = "keybinds clear-defaults=true {\n  bind r#\"x\"# { Write null; }\n}\n";
        assert!(KdlDocument::parse_v2(text).is_err());
        let doc = parse(text).unwrap();
        let keybinds = &doc.nodes()[0];
        assert_eq!(
            keybinds.get("clear-defaults").and_then(|v| v.as_bool()),
            Some(true)
        );
        let bind = &keybinds.children().unwrap().nodes()[0];
        assert_eq!(bind.entries()[0].value().as_string(), Some("x"));
        let write = &bind.children().unwrap().nodes()[0];
        assert_eq!(write.entries()[0].value(), &kdl::KdlValue::Null);
    }

    #[test]
    fn slashdash_drops() {
        let doc = parse("/-a 1\nb /-{ c; }\nd\n").unwrap();
        let names: Vec<&str> = doc.nodes().iter().map(|n| n.name().value()).collect();
        assert_eq!(names, ["b", "d"]);
        assert!(doc.nodes()[0].children().is_none());
    }

    #[test]
    fn zellij_files_parse() {
        for f in [
            "0.46.0/default.kdl",
            "0.46.0/unlock-first.kdl",
            "0.45.1/default.kdl",
            "config.kdl",
        ] {
            let path = format!("{}/tests/fixtures/zellij/{f}", env!("CARGO_MANIFEST_DIR"));
            let text = std::fs::read_to_string(&path).unwrap();
            let started = std::time::Instant::now();
            let doc = parse(&text).unwrap_or_else(|e| panic!("{f}: {e:#}"));
            // Far inside the timeout, in a debug build too.
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "{f}: {:?}",
                started.elapsed()
            );
            assert!(
                doc.nodes().iter().any(|n| n.name().value() == "keybinds"),
                "{f}"
            );
        }
    }
}
