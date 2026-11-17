//! KDL v1 documents (zellij's config and presets, niri's config) through the `kdl` crate: a parse whose error names
//! its line, and a node's line. Each provider walks the document itself.

use kdl::{KdlDocument, KdlNode};

/// Deepest `{ }` nesting parsed (zellij's files nest 4 deep): the parser recurses per level, so a deeper text is
/// refused before it could overflow the stack and abort. Providers run on 2 MiB threads, where a debug build's parser
/// overflowed between 128 and 256 levels (measured 2026-11-16): 64 leaves room.
const MAX_DEPTH: usize = 64;

/// `text` as a KDL v1 document; a failure is `line N: <message>`, from the first diagnostic.
pub(crate) fn parse(text: &str) -> anyhow::Result<KdlDocument> {
    if let Some(at) = too_deep(text) {
        anyhow::bail!(
            "line {}: nested more than {MAX_DEPTH} deep",
            line_at(text, at)
        );
    }
    KdlDocument::parse_v1(text).map_err(|e| match e.diagnostics.first() {
        Some(d) => anyhow::anyhow!(
            "line {}: {}",
            line_at(text, d.span.offset()),
            d.message.as_deref().unwrap_or("not KDL")
        ),
        None => anyhow::anyhow!("not KDL"),
    })
}

/// The byte offset of the first `{` nested deeper than `MAX_DEPTH`, if any. Braces in `"…"` strings (escapes skipped)
/// and `//` comments don't count. ponytail: a cheap scan, not KDL: braces in raw strings and `/* */` comments count, so
/// a file holding hundreds of unmatched `{` there would be refused; a real config holds a handful.
fn too_deep(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let (mut i, mut depth) = (0, 0usize);
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'{' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Some(i);
                }
            }
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
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
    fn too_deep_is_one_error() {
        assert_eq!(MAX_DEPTH, 64);
        // Deep enough to overflow the parser's stack: one error, no abort.
        let deep = format!("a\n{}", "b {".repeat(100_000));
        assert_eq!(err(&deep), "line 2: nested more than 64 deep");
        let at = |n: usize| format!("{}{}", "b {\n".repeat(n), "}\n".repeat(n));
        assert!(parse(&at(64)).is_ok());
        assert_eq!(err(&at(65)), "line 65: nested more than 64 deep");
        // Braces in strings (an escaped quote inside) and `//` comments don't count.
        let quoted = format!(
            "{}a \"{{\\\"{{\" // {{{{\n{}",
            "b {\n".repeat(64),
            "}\n".repeat(64)
        );
        assert!(parse(&quoted).is_ok(), "{:#}", parse(&quoted).unwrap_err());
        // Extra closing braces never take the depth below zero (and hide a deeper nesting after them).
        assert_eq!(too_deep(&format!("}}}}{}", "{".repeat(64))), None);
        assert_eq!(too_deep(&format!("}}}}{}", "{".repeat(65))), Some(66));
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
