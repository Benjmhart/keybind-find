//! KDL v1 documents (zellij's config and presets) through the `kdl` crate: a parse whose error names
//! its line, and a node's line. Each provider walks the document itself.

use kdl::{KdlDocument, KdlNode};

/// The parser (kdl's v1, nom) recurses once per `{ }` level, once per `*`, `/` or run of other text in a `/* */`
/// comment, and once per `/-` in a chain of them (one after another, with only whitespace and comments between), and
/// a stack overflow aborts the whole process. So it runs on a thread of its own with `STACK`, and `parse` first
/// refuses a text past `MAX_DEPTH` levels, `MAX_COMMENT` comment parts or `MAX_CHAIN` chained `/-`. zellij's own
/// parser (the same kdl 4) runs on its 8 MiB main thread, so a file past these is one zellij can't load either.
/// Measured 2026-11-16 in a debug build on 2 MiB (release takes ~7-30x more): ~143 levels, ~684 comment parts, ~140
/// chained `/-`; the worst text within all three bounds (`the_bounds_fit_the_stack`) needs between 24 and 32 MiB in
/// debug, so `STACK` leaves twice that.
/// zellij's files nest 4 deep; the whole 0.45 default config commented out is about 1,400 comment parts.
const STACK: usize = 64 << 20;
const MAX_DEPTH: usize = 256;
const MAX_COMMENT: usize = 8192;
const MAX_CHAIN: usize = 256;

/// `text` as a KDL v1 document; a failure is `line N: <message>`, from the first diagnostic.
pub(crate) fn parse(text: &str) -> anyhow::Result<KdlDocument> {
    if let Some((at, what)) = too_deep(text) {
        anyhow::bail!("line {}: {what}", line_at(text, at));
    }
    let parsed = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(s, || KdlDocument::parse_v1(text))
            .map(|t| t.join())
    });
    let parsed = match parsed {
        Ok(Ok(parsed)) => parsed,
        // The default panic hook has already printed its own line; kdl 4.7.1's panics are believed unreachable from
        // `parse_v1`.
        Ok(Err(_)) => anyhow::bail!("the KDL parser failed (a panic)"),
        Err(e) => anyhow::bail!("the KDL parser failed: can't start its thread: {e}"),
    };
    parsed.map_err(|e| match e.diagnostics.first() {
        Some(d) => anyhow::anyhow!(
            "line {}: {}",
            line_at(text, d.span.offset()),
            d.message.as_deref().unwrap_or("not KDL")
        ),
        None => anyhow::anyhow!("not KDL"),
    })
}

/// Where `text` goes past `MAX_DEPTH`, `MAX_COMMENT` or `MAX_CHAIN`, and which: a scan of KDL v1's strings (`"…"`
/// with escapes, `r#"…"#` raw), `//` line comments (to any of v1's newlines) and nested `/* */` comments, counting
/// `{` levels and chained `/-` outside them.
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
    let (mut i, mut depth, mut chain) = (0, 0usize, 0usize);
    while i < b.len() {
        let hashes = match b[i] {
            b'r' => b[i + 1..].iter().take_while(|&&c| c == b'#').count(),
            _ => 0,
        };
        // ponytail: any `r"`/`r#"` counts, even ending a name (`bar"`): that text is no KDL, and the parser stops there.
        let raw = b[i] == b'r' && b.get(i + 1 + hashes) == Some(&b'"');
        // Whitespace and comments keep a `/-` chain going; anything else ends it.
        if !(b[i].is_ascii_whitespace() || newline(i) || at(i, b"/")) {
            chain = 0;
        }
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
            chain += 1;
            if chain > MAX_CHAIN {
                return Some((i, format!("more than {MAX_CHAIN} `/-` in a row")));
            }
            i += 1;
        } else if b[i] == b'{' {
            depth += 1;
            if depth > MAX_DEPTH {
                return Some((i, format!("nested more than {MAX_DEPTH} deep")));
            }
        } else if b[i] == b'}' {
            depth = depth.saturating_sub(1);
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
        assert!(parse(&nested(256)).is_ok());
        // Past the bounds: one error each.
        assert_eq!(err(&nested(257)), "line 257: nested more than 256 deep");
        assert_eq!(
            err(&format!("a\n{}", "b {".repeat(100_000))),
            "line 2: nested more than 256 deep"
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
        // A chain of `/-`, of nodes or of entries, whitespace and comments between: one error past 256.
        let chain = "line 1: more than 256 `/-` in a row";
        assert_eq!(err(&format!("{}a\n", "/-".repeat(30_000))), chain);
        assert_eq!(err(&format!("a {}1\n", "/- ".repeat(50_000))), chain);
        assert_eq!(
            err(&format!("a {}1\n", "/- /* c */ // c\n".repeat(300)))
                .split(':')
                .nth(1),
            Some(" more than 256 `/-` in a row")
        );
        assert!(
            parse(&format!("{}a\n", "/-".repeat(256))).is_ok()
                || !err(&format!("{}a\n", "/-".repeat(256))).contains("in a row")
        );
        // Separate slashdashed binds aren't a chain.
        let binds = format!(
            "keybinds {{\n  normal {{\n{}  }}\n}}\n",
            "    /-bind \"a\" { Quit; }\n    bind \"b\" { Quit; }\n".repeat(300)
        );
        assert!(parse(&binds).is_ok(), "{:#}", parse(&binds).unwrap_err());
        // A `//` comment ends at any of KDL v1's newlines, and what follows counts.
        for nl in ["\r", "\r\n", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}"] {
            let t = format!("// x{nl}{}", "b {".repeat(100_000));
            assert!(err(&t).ends_with("nested more than 256 deep"), "{nl:?}");
        }
        assert!(parse(&format!("// x\ra \"\n{}\"\n", "{".repeat(300))).is_ok());
        // Many `#` (no `r` before them) scan in linear time.
        let started = std::time::Instant::now();
        assert!(too_deep(&"#".repeat(400_000)).is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        // A raw string ends at its quote and hashes, with no escapes: the braces after it count.
        assert_eq!(
            err(&format!("a r\"\\\"\n{}", "b {".repeat(3000))),
            "line 2: nested more than 256 deep"
        );
        assert_eq!(
            err(&format!("a r#\"\"\"#\n{}", "b {".repeat(3000))),
            "line 2: nested more than 256 deep"
        );
        // Braces in strings (an escaped quote inside, a raw one) and comments (nested) don't count.
        let quoted = format!(
            "{}a \"{{\\\"{{\" r##\"{{\"#{{\"## // {{{{\n/* {{ /* {{ */ {{ */ c\n{}",
            "b {\n".repeat(256),
            "}\n".repeat(256)
        );
        assert!(parse(&quoted).is_ok(), "{:#}", parse(&quoted).unwrap_err());
        // A raw string ends only at its quote with as many `#`: a `"` alone inside doesn't end it.
        let raw = format!(
            "{}a r#\"x\"{}\"#\n{}",
            "b {\n".repeat(254),
            "{".repeat(10),
            "}\n".repeat(254)
        );
        assert!(parse(&raw).is_ok(), "{:#}", parse(&raw).unwrap_err());
        // Extra closing braces never take the depth below zero.
        assert_eq!(too_deep(&format!("}}}}{}", "{".repeat(256))), None);
        assert!(too_deep(&format!("}}}}{}", "{".repeat(257))).is_some());
    }

    #[test]
    fn the_bounds_fit_the_stack() {
        // The worst texts within all bounds: 255 levels, a chain of 256 `/-`, then a comment of 8,192 parts in each
        // shape (marks alone, marks and text, nested comments).
        let nest = "b {\n".repeat(255);
        let close = "}\n".repeat(255);
        for body in [
            "*".repeat(8188),
            "*a".repeat(4094),
            "/*".repeat(2047) + &"*/".repeat(2047),
        ] {
            // A chain of node slashdashes is an error at the end (a `/-` is no node), after the parser went through it all.
            let nodes = format!("{nest}{}/*{body}*/c\n{close}", "/-".repeat(256));
            assert!(too_deep(&nodes).is_none());
            assert!(err(&nodes).starts_with("line 257: "));
            let entries = format!("{nest}a {}/*{body}*/ 1\n{close}", "/- ".repeat(256));
            assert!(too_deep(&entries).is_none());
            // So is a chain of entry slashdashes (a `/-` is no value); both return, no abort.
            assert!(err(&entries).starts_with("line 256: "));
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
            let doc = parse(&text).unwrap_or_else(|e| panic!("{f}: {e:#}"));
            assert!(
                doc.nodes().iter().any(|n| n.name().value() == "keybinds"),
                "{f}"
            );
        }
    }
}
