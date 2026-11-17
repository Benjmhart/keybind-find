//! KDL v1 documents (zellij's config and presets) through the `kdl` crate: a parse whose error names
//! its line, and a node's line. Each provider walks the document itself.

use kdl::{KdlDocument, KdlNode};

/// The parser (kdl's v1, nom) recurses once per `{ }` level and once per `*`, `/` or run of other text in a `/* */`
/// comment, and a stack overflow aborts the whole process. So it runs on a thread of its own with `STACK`, and
/// `parse` first refuses a text deeper than `MAX_DEPTH` or with a comment holding more than `MAX_COMMENT` `*` and `/`.
/// Measured 2026-11-16: the worst text within both bounds parses on a 32 MiB stack in a debug build; on 64 MiB the
/// parser took ~4,600 levels or ~22k comment tokens in debug, ~297k tokens in release (2 MiB, the providers' threads:
/// 143 levels and ~670 tokens in debug, ~9k tokens in release). zellij's files nest 4 deep; the whole 0.45 default
/// config commented out holds about 700 `*` and `/`.
const STACK: usize = 64 << 20;
const MAX_DEPTH: usize = 256;
const MAX_COMMENT: usize = 8192;

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

/// Where `text` goes past `MAX_DEPTH` or `MAX_COMMENT`, and which: a scan of KDL v1's strings (`"…"` with escapes,
/// `r#"…"#` raw), `//` line comments and nested `/* */` comments, counting `{` levels outside them.
fn too_deep(text: &str) -> Option<(usize, String)> {
    let b = text.as_bytes();
    let at = |i: usize, s: &[u8]| b[i..].starts_with(s);
    let (mut i, mut depth) = (0, 0usize);
    while i < b.len() {
        let hashes = b[i + 1..].iter().take_while(|&&c| c == b'#').count();
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
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if at(i, b"/*") {
            let (start, mut open, mut marks) = (i, 0usize, 0usize);
            while i < b.len() && (open > 0 || i == start) {
                if at(i, b"/*") || at(i, b"*/") {
                    open = if b[i] == b'/' { open + 1 } else { open - 1 };
                    marks += 2;
                    i += 2;
                } else {
                    marks += usize::from(b[i] == b'*' || b[i] == b'/');
                    i += 1;
                }
                if marks > MAX_COMMENT {
                    return Some((
                        start,
                        format!("a /* */ comment with more than {MAX_COMMENT} `*` and `/`"),
                    ));
                }
            }
            continue;
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
        assert!(parse(&format!("/*\n{}\n*/\n", d.repeat(10))).is_ok());
        let nested = |n: usize| format!("{}{}", "b {\n".repeat(n), "}\n".repeat(n));
        assert!(parse(&nested(256)).is_ok());
        // Past the bounds: one error each.
        assert_eq!(err(&nested(257)), "line 257: nested more than 256 deep");
        assert_eq!(
            err(&format!("a\n{}", "b {".repeat(100_000))),
            "line 2: nested more than 256 deep"
        );
        let e = "line 2: a /* */ comment with more than 8192 `*` and `/`";
        assert_eq!(err(&format!("a\n/* {} */\n", "* ".repeat(20_000))), e);
        assert_eq!(err(&format!("a\n/* {} */\n", "/".repeat(8191))), e);
        assert!(parse(&format!("a\n/* {} */\n", "/".repeat(8188))).is_ok());
        assert_eq!(err(&format!("a\n{}", "/*".repeat(5000))), e);
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
        // The worst texts within both bounds: 255 levels around a comment of 8,190 `*` and `/` in each shape.
        let nest = "b {\n".repeat(255);
        for body in [
            "*a".repeat(4095),
            "/*".repeat(2047) + &"*/".repeat(2047),
            "/a".repeat(4095),
            "*".repeat(8186),
        ] {
            let t = format!("{nest}/* {body} */ c\n{}", "}\n".repeat(255));
            assert!(too_deep(&t).is_none());
            assert!(parse(&t).is_ok());
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
