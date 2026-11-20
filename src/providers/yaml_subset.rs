//! The small part of YAML that lazygit's `keybinding:` section is written in, read strictly: the document's top-level
//! entries are split at column 0 and only `keybinding` is parsed; inside it, block mappings by indentation, plain and
//! quoted scalars, one-line flow sequences and mappings, and block sequences of scalars. Anything else in it (anchors,
//! tags, block scalars, a flow collection over several lines, a duplicate key, …) is refused with its line, so a file
//! is never read differently from how yaml.v3 reads it.

use anyhow::bail;
use std::collections::HashSet;

/// A parsed value. A mapping keeps its keys in order, each with its line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Map(Vec<(String, Node, usize)>),
    Seq(Vec<Node>),
    Scalar(String),
    Null,
}

/// How deep mappings and sequences may nest (as `kdl_v1` caps its parser): no lazygit file nests past 3.
const DEPTH: usize = 64;

/// `keybinding` over a file's bytes: text that isn't UTF-8 is refused, as yaml.v3 refuses it.
pub(crate) fn keybinding_of(bytes: &[u8]) -> anyhow::Result<Option<Node>> {
    match std::str::from_utf8(bytes) {
        Ok(text) => keybinding(text),
        Err(e) => bail!("not UTF-8 text (byte {})", e.valid_up_to() + 1),
    }
}

/// The value of the document's top-level `keybinding` entry, `None` when it has none. A failure is `line N: …`.
pub(crate) fn keybinding(text: &str) -> anyhow::Result<Option<Node>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    check_chars(text)?;
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let blank = |l: &str| {
        let t = l.trim_start_matches([' ', '\t']);
        t.is_empty() || t.starts_with('#')
    };
    let marker = |l: &str, m: &str| {
        l.strip_prefix(m)
            .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t']))
    };
    // Before the root: directives as yaml.v3 takes them (parserc.go:1188-1213, scannerc.go:1672): `%YAML 1.1` once,
    // `%TAG` once per handle; then `---`.
    let (mut root, mut started, mut directive) = (false, false, false);
    let mut handles: Vec<&str> = Vec::new();
    let mut entry: Option<usize> = None;
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate() {
        if blank(l) {
            continue;
        }
        let n = i + 1;
        // The end of the first document: yaml.v3's Unmarshal reads no further (decode.go:156-168).
        if marker(l, "...") || (marker(l, "---") && (root || started)) {
            end = end.min(i);
            break;
        }
        if marker(l, "---") {
            if !blank(&l[3..]) {
                bail!("line {n}: text after `---`");
            }
            started = true;
            directive = false;
            continue;
        }
        if !root && !started && l.starts_with('%') {
            let words: Vec<&str> = l
                .split_whitespace()
                .take_while(|w| !w.starts_with('#'))
                .collect();
            let ok = match words[..] {
                ["%YAML", "1.1"] => !handles.contains(&"%YAML"),
                ["%TAG", h, _] => h.starts_with('!') && h.ends_with('!') && !handles.contains(&h),
                _ => false,
            };
            if !ok {
                bail!("line {n}: a directive yaml.v3 refuses (`%YAML 1.1` and `%TAG` are read, each once)");
            }
            handles.push(words[words.len() - 2]);
            directive = true;
            continue;
        }
        if directive {
            bail!("line {n}: a directive (`%`) not followed by `---`");
        }
        if !root {
            root = true;
            if l.starts_with([' ', '\t']) {
                bail!("line {n}: an indented document root");
            }
            if l.starts_with(['{', '[']) || marker(l, "-") {
                bail!("line {n}: a flow collection or a sequence as the whole document");
            }
        }
        let top = !l.starts_with([' ', '\t', '-']) || (l.starts_with('-') && !marker(l, "-"));
        if !top {
            continue;
        }
        if unclosed(l) {
            bail!("line {n}: a quote or bracket left open at the end of a top-level line");
        }
        if entry.is_some() && end == lines.len() {
            end = i;
        }
        if key_of(l).is_some_and(|k| k == "keybinding") {
            if entry.is_some() {
                bail!("line {n}: `keybinding` twice");
            }
            entry = Some(i);
            end = lines.len();
        }
    }
    let Some(start) = entry else {
        return Ok(None);
    };
    let n = start + 1;
    let (_, rest) = split_key(lines[start], n)?;
    let rest = rest.split(" #").next().unwrap_or_default().trim();
    if !matches!(rest, "" | "~" | "null" | "Null" | "NULL" | "{}") {
        bail!("line {n}: `keybinding:` must be followed by a block on the next lines");
    }
    let mut body = Vec::new();
    for (i, l) in lines.iter().enumerate().take(end).skip(start + 1) {
        if blank(l) || marker(l, "...") {
            continue;
        }
        let content = l.trim_start_matches([' ', '\t']);
        let indent = &l[..l.len() - content.len()];
        if indent.contains('\t') {
            bail!("line {}: a tab in the indentation", i + 1);
        }
        // `- - x`: an item holding a sequence on the same line, read as `-` then `- x` at the second dash's column.
        let (mut col, mut content) = (indent.len(), content);
        while let Some(inner) = content
            .strip_prefix('-')
            .map(|r| r.trim_start_matches(' '))
            .filter(|r| r.len() + 1 < content.len() && is_item(r))
        {
            body.push((i + 1, col, "-"));
            col += content.len() - inner.len();
            content = inner;
        }
        body.push((i + 1, col, content));
    }
    match (body.first(), rest) {
        (None, "{}") => return Ok(Some(Node::Map(Vec::new()))),
        (None, _) => return Ok(Some(Node::Null)),
        (Some((n, _, _)), r) if !r.is_empty() => bail!("line {n}: unexpected indentation"),
        _ => {}
    }
    let mut pos = 0;
    let node = block(&body, &mut pos, 0, 1)?;
    match body.get(pos) {
        Some((n, _, _)) => bail!("line {n}: unexpected indentation"),
        None => Ok(Some(node)),
    }
}

/// Refuses the characters yaml.v3's reader refuses (readerc.go:368-380: controls but tab, LF and CR; DEL; C1 but
/// NEL; U+FFFE, U+FFFF) and the line breaks this reader doesn't split at (a lone CR, NEL, U+2028, U+2029).
fn check_chars(text: &str) -> anyhow::Result<()> {
    let mut line = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let bad = match c {
            '\n' => {
                line += 1;
                false
            }
            '\r' => chars.peek() != Some(&'\n'),
            '\t' => false,
            '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{fffe}' | '\u{ffff}' => true,
            c => c.is_control(),
        };
        if bad {
            bail!(
                "line {line}: the character {c:?}, which yaml.v3 refuses or reads as a line break"
            );
        }
    }
    Ok(())
}

/// Whether a top-level line leaves a quote, `[` or `{` open (its value then goes on, and later lines at column 0
/// aren't entries).
fn unclosed(line: &str) -> bool {
    let (mut depth, mut quote, mut prev) = (0usize, None::<char>, ' ');
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' => {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            Some('"') if c == '\\' => {
                chars.next();
            }
            Some('"') if c == '"' => quote = None,
            Some(_) => {}
            None => match c {
                '#' if prev == ' ' || prev == '\t' => break,
                '\'' | '"' if matches!(prev, ' ' | '\t' | '[' | '{' | ',' | ':') => quote = Some(c),
                '[' | '{' if matches!(prev, ' ' | '\t' | '[' | '{' | ',' | ':') || depth > 0 => {
                    depth += 1
                }
                ']' | '}' if depth > 0 => depth -= 1,
                _ => {}
            },
        }
        prev = c;
    }
    quote.is_some() || depth > 0
}

/// A top-level line's key, when it has one.
fn key_of(line: &str) -> Option<String> {
    split_key(line, 0).ok().map(|(k, _)| k)
}

/// One body line: (line number, indentation, the rest).
type Line<'a> = (usize, usize, &'a str);

fn is_item(c: &str) -> bool {
    c == "-" || c.starts_with("- ") || c.starts_with("-\t")
}

/// The block at `body[*pos]`, deeper than `parent`.
fn block(body: &[Line], pos: &mut usize, parent: usize, depth: usize) -> anyhow::Result<Node> {
    let Some(&(n, indent, content)) = body.get(*pos) else {
        return Ok(Node::Null);
    };
    if depth > DEPTH {
        bail!("line {n}: nested deeper than {DEPTH} levels");
    }
    if indent <= parent {
        bail!("line {n}: a block must be indented more than its key");
    }
    if is_item(content) {
        return seq(body, pos, indent, depth, false);
    }
    let mut entries: Vec<(String, Node, usize)> = Vec::new();
    let mut keys = HashSet::new();
    while let Some(&(n, i, c)) = body.get(*pos) {
        if i < indent {
            break;
        }
        if i > indent {
            bail!("line {n}: unexpected indentation");
        }
        if is_item(c) {
            bail!("line {n}: a sequence item where a key belongs");
        }
        let (key, rest) = split_key(c, n)?;
        if key == "<<" {
            bail!("line {n}: a merge key (`<<`)");
        }
        if !keys.insert(key.clone()) {
            bail!("line {n}: the key {key:?} twice");
        }
        *pos += 1;
        let value = match line_value(rest, n, depth)? {
            Some(v) => v,
            None if body.get(*pos).is_some_and(|&(_, i, _)| i > indent) => {
                block(body, pos, indent, depth + 1)?
            }
            // `key:` then `- a` at the key's own column: the key's list (YAML allows it).
            None if body
                .get(*pos)
                .is_some_and(|&(_, i, c)| i == indent && is_item(c)) =>
            {
                if depth + 1 > DEPTH {
                    bail!("line {n}: nested deeper than {DEPTH} levels");
                }
                seq(body, pos, indent, depth + 1, true)?
            }
            None => Node::Null,
        };
        entries.push((key, value, n));
    }
    Ok(Node::Map(entries))
}

/// A block sequence at `indent`: of scalars, or of lists (`jumpToBlock`'s key lists), never of mappings. `compact`:
/// a mapping's value at its key's column, ended by the next key.
fn seq(
    body: &[Line],
    pos: &mut usize,
    indent: usize,
    depth: usize,
    compact: bool,
) -> anyhow::Result<Node> {
    let mut items = Vec::new();
    while let Some(&(n, i, c)) = body.get(*pos) {
        if i < indent || (compact && !is_item(c)) {
            break;
        }
        if i > indent {
            bail!("line {n}: unexpected indentation");
        }
        if !is_item(c) {
            bail!("line {n}: a key where a sequence item belongs");
        }
        let rest = &c[1..];
        if split_key(rest.trim_start(), n).is_ok() {
            bail!("line {n}: a sequence holding a mapping");
        }
        *pos += 1;
        let item = match line_value(rest, n, depth + 1)? {
            Some(Node::Map(_)) => bail!("line {n}: a sequence holding a mapping"),
            Some(v) => v,
            None if body.get(*pos).is_some_and(|&(_, i, _)| i > indent) => {
                match block(body, pos, indent, depth + 1)? {
                    Node::Map(_) => bail!("line {n}: a sequence holding a mapping"),
                    v => v,
                }
            }
            None => Node::Null,
        };
        items.push(item);
    }
    Ok(Node::Seq(items))
}

/// A line's `key:` and the text after the colon.
fn split_key(line: &str, n: usize) -> anyhow::Result<(String, &str)> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut cur = Cur {
        s: line,
        chars: &chars,
        i: 0,
        n,
    };
    let key = match cur.peek() {
        Some('\'' | '"') => {
            let k = cur.quoted()?;
            cur.spaces();
            k
        }
        _ => {
            cur.plain_start()?;
            let start = cur.i;
            loop {
                match cur.peek() {
                    None => bail!("line {n}: expected `key: value`"),
                    Some(':') if matches!(cur.at(1), None | Some(' ' | '\t')) => break,
                    Some(' ' | '\t') if cur.at(1) == Some('#') => {
                        bail!("line {n}: expected `key: value`")
                    }
                    _ => cur.i += 1,
                }
            }
            cur.text(start, cur.i).trim_end().to_string()
        }
    };
    if cur.peek() != Some(':') || !matches!(cur.at(1), None | Some(' ' | '\t')) {
        bail!("line {n}: expected `:` after the key");
    }
    let at = cur.offset(cur.i + 1);
    Ok((key, &line[at..]))
}

/// The value after a key's colon (or a `-`), `None` when the line holds none.
fn line_value(rest: &str, n: usize, depth: usize) -> anyhow::Result<Option<Node>> {
    let chars: Vec<(usize, char)> = rest.char_indices().collect();
    let mut cur = Cur {
        s: rest,
        chars: &chars,
        i: 0,
        n,
    };
    cur.spaces();
    if matches!(cur.peek(), None | Some('#')) {
        return Ok(None);
    }
    let v = cur.value(false, depth)?;
    cur.end()?;
    Ok(Some(v))
}

/// A cursor over one line.
struct Cur<'a> {
    s: &'a str,
    chars: &'a [(usize, char)],
    i: usize,
    n: usize,
}

impl Cur<'_> {
    fn peek(&self) -> Option<char> {
        self.at(0)
    }

    fn at(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).map(|&(_, c)| c)
    }

    fn offset(&self, i: usize) -> usize {
        self.chars.get(i).map_or(self.s.len(), |&(o, _)| o)
    }

    fn text(&self, from: usize, to: usize) -> &str {
        &self.s[self.offset(from)..self.offset(to)]
    }

    fn spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
    }

    /// Only whitespace and a comment may follow a value.
    fn end(&mut self) -> anyhow::Result<()> {
        let before = self.i;
        self.spaces();
        match self.peek() {
            None => Ok(()),
            Some('#') if self.i > before => Ok(()),
            Some(c) => bail!("line {}: unexpected {c:?} after the value", self.n),
        }
    }

    fn value(&mut self, flow: bool, depth: usize) -> anyhow::Result<Node> {
        let n = self.n;
        if depth > DEPTH {
            bail!("line {n}: nested deeper than {DEPTH} levels");
        }
        match self.peek() {
            Some('[') => self.flow_seq(depth),
            Some('{') => self.flow_map(depth),
            Some('\'' | '"') => Ok(Node::Scalar(self.quoted()?)),
            // In a flow collection `:` and `?` are always indicators (scannerc.go:795-801): no value starts there.
            Some(c @ (':' | '?')) if flow => {
                bail!("line {n}: {c:?} starts no value in a flow collection; quote it")
            }
            _ => {
                self.plain_start()?;
                let start = self.i;
                while let Some(c) = self.peek() {
                    let next = self.at(1);
                    let blank_next = matches!(next, None | Some(' ' | '\t'));
                    // yaml.v3's plain scalar (scannerc.go:2726-2733): `:` ends it before a blank; in a flow
                    // collection so do `,` `?` `[` `]` `{` `}`, and what follows must then be a separator.
                    match c {
                        ' ' | '\t' if next == Some('#') => break,
                        ',' | '?' | '[' | ']' | '{' | '}' if flow => break,
                        ':' if blank_next => bail!("line {n}: a mapping where a value belongs"),
                        _ => self.i += 1,
                    }
                }
                let text = self.text(start, self.i).trim_end();
                // yaml.v3's core-schema nulls (resolve.go:54); the empty value is the caller's.
                Ok(match text {
                    "~" | "null" | "Null" | "NULL" => Node::Null,
                    t => Node::Scalar(t.to_string()),
                })
            }
        }
    }

    /// Refuses what YAML would read as something other than a plain scalar, or doesn't allow to start one.
    fn plain_start(&self) -> anyhow::Result<()> {
        let n = self.n;
        let blank_next = matches!(self.at(1), None | Some(' ' | '\t'));
        match self.peek() {
            Some('&') => bail!("line {n}: an anchor (`&`)"),
            Some('*') => bail!("line {n}: an alias (`*`)"),
            Some('!') => bail!("line {n}: a tag (`!`)"),
            Some('|' | '>') => bail!("line {n}: a block scalar (`|`, `>`)"),
            Some(c @ ('[' | ']' | '{' | '}' | ',' | '#' | '\'' | '"' | '%' | '@' | '`')) => {
                bail!("line {n}: a plain value can't start with {c:?}; quote it")
            }
            Some(c @ ('-' | '?' | ':')) if blank_next => {
                bail!("line {n}: {c:?} followed by a space starts no value here; quote it")
            }
            Some(_) => Ok(()),
            None => bail!("line {n}: a value is missing"),
        }
    }

    fn quoted(&mut self) -> anyhow::Result<String> {
        let n = self.n;
        let Some(q) = self.peek() else {
            bail!("line {n}: a value is missing");
        };
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                bail!("line {n}: a quoted value not closed on its line");
            };
            self.i += 1;
            match (q, c) {
                ('\'', '\'') if self.peek() == Some('\'') => {
                    self.i += 1;
                    out.push('\'');
                }
                ('\'', '\'') | ('"', '"') => return Ok(out),
                ('"', '\\') => {
                    let Some(e) = self.peek() else {
                        bail!("line {n}: a quoted value not closed on its line");
                    };
                    self.i += 1;
                    match e {
                        '\\' | '"' | '/' => out.push(e),
                        't' => out.push('\t'),
                        'n' => out.push('\n'),
                        'x' | 'u' => {
                            let len = if e == 'x' { 2 } else { 4 };
                            let hex: String = (0..len).filter_map(|k| self.at(k)).collect();
                            let hex_ok =
                                hex.len() == len && hex.chars().all(|c| c.is_ascii_hexdigit());
                            let c = hex_ok
                                .then(|| u32::from_str_radix(&hex, 16).ok())
                                .flatten()
                                .and_then(char::from_u32);
                            let Some(c) = c else {
                                bail!("line {n}: a bad \\{e} escape");
                            };
                            self.i += len;
                            out.push(c);
                        }
                        _ => bail!("line {n}: the escape \\{e} isn't read here"),
                    }
                }
                _ => out.push(c),
            }
        }
    }

    fn flow_end(&mut self, close: char) -> anyhow::Result<bool> {
        self.spaces();
        match self.peek() {
            Some(',') => {
                self.i += 1;
                Ok(false)
            }
            Some(c) if c == close => {
                self.i += 1;
                Ok(true)
            }
            None | Some('#') => bail!("line {}: a flow collection over several lines", self.n),
            Some(c) => bail!("line {}: unexpected {c:?} in a flow collection", self.n),
        }
    }

    fn flow_seq(&mut self, depth: usize) -> anyhow::Result<Node> {
        let n = self.n;
        self.i += 1;
        let mut items = Vec::new();
        loop {
            self.spaces();
            match self.peek() {
                Some(']') => {
                    self.i += 1;
                    return Ok(Node::Seq(items));
                }
                Some('{') => bail!("line {n}: a flow sequence holding a mapping"),
                None | Some('#') => bail!("line {n}: a flow collection over several lines"),
                _ => {}
            }
            items.push(self.value(true, depth + 1)?);
            if self.flow_end(']')? {
                return Ok(Node::Seq(items));
            }
        }
    }

    fn flow_map(&mut self, depth: usize) -> anyhow::Result<Node> {
        let n = self.n;
        self.i += 1;
        let mut entries: Vec<(String, Node, usize)> = Vec::new();
        let mut keys = HashSet::new();
        loop {
            self.spaces();
            let key = match self.peek() {
                Some('}') => {
                    self.i += 1;
                    return Ok(Node::Map(entries));
                }
                None | Some('#') => bail!("line {n}: a flow collection over several lines"),
                Some('\'' | '"') => self.quoted()?,
                Some(c @ (':' | '?')) => {
                    bail!("line {n}: {c:?} starts no key in a flow mapping; quote it")
                }
                _ => {
                    self.plain_start()?;
                    let start = self.i;
                    // As a flow plain scalar ends (above): `{a:b}` is the key `a:b`.
                    loop {
                        match self.peek() {
                            None | Some(',' | '?' | '[' | ']' | '{' | '}') => break,
                            Some(':') if matches!(self.at(1), None | Some(' ' | '\t')) => break,
                            Some(' ' | '\t') if self.at(1) == Some('#') => break,
                            _ => self.i += 1,
                        }
                    }
                    self.text(start, self.i).trim_end().to_string()
                }
            };
            self.spaces();
            if key == "<<" {
                bail!("line {n}: a merge key (`<<`)");
            }
            if !keys.insert(key.clone()) {
                bail!("line {n}: the key {key:?} twice");
            }
            let value = match self.peek() {
                // A key with no `:` has a null value.
                Some(',' | '}') => Node::Null,
                Some(':') => {
                    self.i += 1;
                    self.spaces();
                    match self.peek() {
                        Some(',' | '}') => Node::Null,
                        _ => self.value(true, depth + 1)?,
                    }
                }
                None | Some('#') => bail!("line {n}: a flow collection over several lines"),
                Some(c) => bail!("line {n}: unexpected {c:?} in a flow mapping"),
            };
            entries.push((key, value, n));
            if self.flow_end('}')? {
                return Ok(Node::Map(entries));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Node {
        Node::Scalar(v.into())
    }

    /// The `keybinding` entry of `body` (indented under it).
    fn kb(body: &str) -> anyhow::Result<Option<Node>> {
        keybinding(&format!("keybinding:\n{body}"))
    }

    /// The value of `universal.<field>` in a one-field document.
    fn field(value: &str) -> anyhow::Result<Node> {
        let node = kb(&format!("  universal:\n    f: {value}\n"))?;
        let Some(Node::Map(top)) = node else {
            panic!("{node:?}")
        };
        let Node::Map(u) = &top[0].1 else {
            panic!("{top:?}")
        };
        Ok(u[0].1.clone())
    }

    fn err(text: &str) -> String {
        format!("{:#}", keybinding(text).unwrap_err())
    }

    #[test]
    fn accepted_forms() {
        for (value, want) in [
            ("q", s("q")),
            ("<ctrl+c>", s("<ctrl+c>")),
            ("'?'", s("?")),
            ("'it''s'", s("it's")),
            (r#""n""#, s("n")),
            (r#""a\\b\"c\/\t\n\x41\u00e9""#, s("a\\b\"c/\t\nAé")),
            ("a#b", s("a#b")),
            ("'#'", s("#")),
            ("q # a comment", s("q")),
            ("[<up>, k]", Node::Seq(vec![s("<up>"), s("k")])),
            ("['>', <end>]", Node::Seq(vec![s(">"), s("<end>")])),
            ("[]", Node::Seq(vec![])),
            ("[a, b,]", Node::Seq(vec![s("a"), s("b")])),
            (
                "{ quit: q, x: { y: [a] }, z: }",
                Node::Map(vec![
                    ("quit".into(), s("q"), 3),
                    (
                        "x".into(),
                        Node::Map(vec![("y".into(), Node::Seq(vec![s("a")]), 3)]),
                        3,
                    ),
                    ("z".into(), Node::Null, 3),
                ]),
            ),
            ("~", Node::Null),
            ("Null", Node::Null),
            ("'null'", s("null")),
            ("", Node::Null),
            ("# only a comment", Node::Null),
            ("<disabled>", s("<disabled>")),
            ("y", s("y")),
            ("-x", s("-x")),
        ] {
            assert_eq!(field(value).unwrap(), want, "{value}");
        }
        // A block list, an empty value then a deeper block, comments between, CRLF.
        let text = "# top\r\n---\r\nother: 1\r\nkeybinding:\r\n  # c\r\n  universal:\r\n\r\n    nextItem:\r\n      - <down>\r\n      - \"j\"\r\n      -\r\n    quit: q\r\n  'files' : { a: b }\r\nlast: x\r\n";
        assert_eq!(
            keybinding(text).unwrap(),
            Some(Node::Map(vec![
                (
                    "universal".into(),
                    Node::Map(vec![
                        (
                            "nextItem".into(),
                            Node::Seq(vec![s("<down>"), s("j"), Node::Null]),
                            8
                        ),
                        ("quit".into(), s("q"), 12),
                    ]),
                    6
                ),
                (
                    "files".into(),
                    Node::Map(vec![("a".into(), s("b"), 13)]),
                    13
                ),
            ]))
        );
        // A section as a one-line flow mapping (only `keybinding:` itself must be a block).
        assert_eq!(
            kb("  universal: { quit: q }\n").unwrap(),
            Some(Node::Map(vec![(
                "universal".into(),
                Node::Map(vec![("quit".into(), s("q"), 2)]),
                2
            )]))
        );
        // No keybinding entry; an empty one.
        assert_eq!(keybinding("gui:\n  x: 1\n").unwrap(), None);
        assert_eq!(keybinding("").unwrap(), None);
        assert_eq!(keybinding("keybinding:\n# c\n").unwrap(), Some(Node::Null));
        assert_eq!(
            keybinding("\"keybinding\":\n  a: b\n").unwrap(),
            Some(Node::Map(vec![("a".into(), s("b"), 2)]))
        );
    }

    #[test]
    fn other_entries_are_skipped_unread() {
        let text = "\
customCommands:
- key: x
  command: |
    echo &a *b !!str {
  prompts: [
    a,
  ]
anchors: &a
  x: *a
update: { method: never }
keybinding:
  universal:
    quit: X
gui: |
  [not { read
";
        assert_eq!(
            keybinding(text).unwrap(),
            Some(Node::Map(vec![(
                "universal".into(),
                Node::Map(vec![("quit".into(), s("X"), 13)]),
                12
            )]))
        );
    }

    #[test]
    fn refusals_name_their_line() {
        for (text, line, what) in [
            ("keybinding:\n  universal:\n\tquit: q\n", 3, "tab"),
            ("keybinding:\n  universal: &a\n    quit: q\n", 2, "anchor"),
            ("keybinding:\n  universal:\n    quit: *a\n", 3, "alias"),
            ("keybinding:\n  universal:\n    <<: x\n", 3, "merge key"),
            ("keybinding:\n  universal:\n    quit: !!str q\n", 3, "tag"),
            (
                "keybinding:\n  universal:\n    quit: |\n      q\n",
                3,
                "block scalar",
            ),
            (
                "keybinding:\n  universal:\n    quit: >\n",
                3,
                "block scalar",
            ),
            (
                "keybinding:\n  universal:\n    quit: [q,\n      x]\n",
                3,
                "several lines",
            ),
            ("keybinding:\n  u:\n    quit: { a: b\n", 3, "several lines"),
            ("keybinding:\n  u:\n    quit: q\n    quit: x\n", 4, "twice"),
            ("keybinding:\n  u:\n    q: { a: 1, a: 2 }\n", 3, "twice"),
            ("keybinding:\n  u:\n    quit: @\n", 3, "quote it"),
            ("keybinding:\n  u:\n    quit: `\n", 3, "quote it"),
            ("keybinding:\n  u:\n    quit: - q\n", 3, "quote it"),
            (
                "keybinding:\n  u:\n    quit:\n      - a: b\n",
                4,
                "holding a mapping",
            ),
            (
                "keybinding:\n  u:\n    quit:\n      - { a: b }\n",
                4,
                "holding a mapping",
            ),
            (
                "keybinding:\n  u:\n    quit:\n      -\n        a: b\n",
                4,
                "holding a mapping",
            ),
            (
                "keybinding:\n  u:\n    quit: [[a], {a: b}]\n",
                3,
                "holding a mapping",
            ),
            (
                "keybinding:\n  u:\n    quit: a: b\n",
                3,
                "mapping where a value",
            ),
            ("keybinding:\n  u:\n    quit: \"\\q\"\n", 3, "escape"),
            ("keybinding:\n  u:\n    quit: 'q\n", 3, "not closed"),
            ("keybinding:\n  u:\n    quit: 'q'x\n", 3, "after the value"),
            (
                "keybinding:\n  u:\n    quit: q\n   x: y\n",
                4,
                "indentation",
            ),
            (
                "keybinding:\n  u:\n    quit: q\n      x: y\n",
                4,
                "indentation",
            ),
            ("keybinding:\n  u: q\n    x: y\n", 3, "indentation"),
            ("keybinding:\n  u\n", 2, "key: value"),
            ("keybinding:\n- a\n", 2, "indented more"),
            (
                "keybinding: { universal: { quit: q } }\n",
                1,
                "block on the next",
            ),
            ("keybinding:\n  a: 1\nkeybinding:\n  b: 2\n", 3, "twice"),
            ("{ keybinding: {} }\n", 1, "whole document"),
            ("# c\n[a]\n", 2, "whole document"),
            ("--- a: 1\n", 1, "text after `---`"),
            ("---\n{ keybinding: {} }\n", 2, "whole document"),
            ("- a\n", 1, "whole document"),
            ("  keybinding:\n    u: {}\n", 1, "indented document root"),
            // A top-level value going on to later lines: their column-0 text isn't an entry.
            ("a: \"x\nkeybinding:\n  u:\n    q: x\n\"\n", 1, "left open"),
            ("a: [x,\n  y]\n", 1, "left open"),
            ("a: {x: 'y\n", 1, "left open"),
            // Directives: `%YAML 1.1` and `%TAG`, each once.
            ("%YAML 1.2\n---\n", 1, "directive"),
            ("%FOO bar\n---\n", 1, "directive"),
            ("%YAML 1.1\n%YAML 1.1\n---\n", 2, "directive"),
            ("%TAG ! a:\n%TAG ! b:\n---\n", 2, "directive"),
            ("%YAML 1.1\nkeybinding:\n", 2, "not followed by `---`"),
            // What yaml.v3's reader refuses, and line breaks this reader doesn't split at.
            ("keybinding:\n  u:\n    q: a\u{7f}\n", 3, "character"),
            ("a: \u{1}\n", 1, "character"),
            ("a: \u{9f}\n", 1, "character"),
            ("a: \u{fffe}\n", 1, "character"),
            ("keybinding:\n  u:\n    q: a\r    w: b\n", 3, "character"),
            ("a: \u{85}\n", 1, "character"),
            ("a: 1\u{2028}b: 2\n", 1, "character"),
            ("a: 1\n\u{2029}\n", 2, "character"),
            // `{:a}`: a value with no key, which yaml.v3 refuses.
            ("keybinding:\n  u: {:a}\n", 2, "starts no key"),
            ("keybinding:\n  u: {: a}\n", 2, "starts no key"),
            ("keybinding:\n  u:\n    q: [:a]\n", 3, "starts no value"),
        ] {
            let e = err(text);
            assert!(e.starts_with(&format!("line {line}: ")), "{text:?}: {e}");
            assert!(e.contains(what), "{text:?}: {e}");
        }
    }

    #[test]
    fn yaml_v3_corners() {
        // A list at its key's column is the key's value; the next key ends it.
        let text = "keybinding:\n  u:\n    quit:\n    - a\n    - b\n    edit: e\n";
        assert_eq!(
            keybinding(text).unwrap(),
            Some(Node::Map(vec![(
                "u".into(),
                Node::Map(vec![
                    ("quit".into(), Node::Seq(vec![s("a"), s("b")]), 3),
                    ("edit".into(), s("e"), 6),
                ]),
                2
            )]))
        );
        // Lists of lists (jumpToBlock): flow in flow, flow items, block items.
        let ab = || Node::Seq(vec![s("1"), s("!")]);
        assert_eq!(
            field("[['1', '!'], '2']").unwrap(),
            Node::Seq(vec![ab(), s("2")])
        );
        let text = "keybinding:\n  u:\n    j:\n      - ['1', '!']\n      -\n        - '1'\n        - '!'\n      - '2'\n";
        let Some(Node::Map(top)) = keybinding(text).unwrap() else {
            panic!()
        };
        let Node::Map(u) = &top[0].1 else { panic!() };
        assert_eq!(u[0].1, Node::Seq(vec![ab(), ab(), s("2")]));
        // A BOM; text that isn't UTF-8.
        assert!(keybinding_of(b"\xef\xbb\xbfkeybinding:\n  u:\n    q: x\n")
            .unwrap()
            .is_some());
        let e = keybinding_of(b"keybinding:\n  u:\n    q: \xff\n").unwrap_err();
        assert!(format!("{e}").contains("not UTF-8"), "{e}");
        // Flow keys end at `:` before a blank or at a flow indicator: `{a:b}` is a key with no value.
        assert_eq!(
            field("{a:b, c: d, e}").unwrap(),
            Node::Map(vec![
                ("a:b".into(), Node::Null, 3),
                ("c".into(), s("d"), 3),
                ("e".into(), Node::Null, 3)
            ])
        );
        assert_eq!(field("[a:b]").unwrap(), Node::Seq(vec![s("a:b")]));
        // A `[` or `{` inside a flow plain scalar ends it, and nothing may follow.
        for v in ["[a[b]", "[a{b]", "{a{b: c}", "[a?b]"] {
            assert!(field(v).is_err(), "{v}");
        }
        // Escapes take hex digits only.
        for v in [r#""\x+4""#, r#""\u+041""#, r#""\x4g""#] {
            assert!(
                format!("{:#}", field(v).unwrap_err()).contains("escape"),
                "{v}"
            );
        }
        assert_eq!(field(r#""\u0041\x42""#).unwrap(), s("AB"));
        // Directives before the first `---`.
        assert!(keybinding(
            "%YAML 1.1 # c\n%TAG ! tag:x,2000:\n%TAG !! y:\n---\nkeybinding:\n  u:\n    q: x\n"
        )
        .unwrap()
        .is_some());
        // The first document only: what follows `...` or a second `---` isn't read (decode.go:156-168).
        for end in ["...", "---"] {
            let text = format!("keybinding:\n  u:\n    q: x\n{end}\nfoo: [\nkeybinding:\n");
            assert!(keybinding(&text).unwrap().is_some(), "{end}");
        }
        assert_eq!(keybinding("a: 1\n---\nkeybinding:\n").unwrap(), None);
        // A BOM starts the text only: one later is the start of a key (here ending `keybinding`'s block).
        assert_eq!(
            keybinding("keybinding:\n\u{feff}  u:\n    q: x\n").unwrap(),
            Some(Node::Null)
        );
        // `keybinding` with no value or a null one, or an empty flow mapping; a tab and CRLF are fine.
        for (v, want) in [
            ("~", Node::Null),
            ("null # c", Node::Null),
            ("{} # c", Node::Map(vec![])),
        ] {
            assert_eq!(
                keybinding(&format!("keybinding: {v}\n")).unwrap(),
                Some(want),
                "{v}"
            );
        }
        assert!(err("keybinding: {}\n  u: {}\n").starts_with("line 2: unexpected indentation"));
        assert!(keybinding("a: 'x'\t\r\n").is_ok());
        // `- - x`: a list item holding a list on its line.
        let text =
            "keybinding:\n  u:\n    j:\n      - - '1'\n        - '!'\n      - - - x\n      - -\n";
        let Some(Node::Map(top)) = keybinding(text).unwrap() else {
            panic!()
        };
        let Node::Map(u) = &top[0].1 else { panic!() };
        assert_eq!(
            u[0].1,
            Node::Seq(vec![
                ab(),
                Node::Seq(vec![Node::Seq(vec![s("x")])]),
                Node::Seq(vec![Node::Null])
            ])
        );
        // yaml.v3's nulls only.
        for v in ["~", "null", "Null", "NULL"] {
            assert_eq!(field(v).unwrap(), Node::Null, "{v}");
        }
        for v in ["nULL", "nuLL", "NUll"] {
            assert_eq!(field(v).unwrap(), s(v), "{v}");
        }
        // A `...` document end is skipped.
        assert!(keybinding("keybinding:\n  u:\n    q: x\n...\n")
            .unwrap()
            .is_some());
        // Indicators that can't start a plain value.
        for v in ["? x", ": x", "%x", "!x", ",x", "]x"] {
            assert!(field(v).is_err(), "{v}");
        }
    }

    #[test]
    fn deep_and_broken_input_never_panics() {
        let deep: String = (0..100)
            .map(|d| format!("{}k{d}:\n", " ".repeat(d + 1)))
            .collect();
        assert!(err(&format!("keybinding:\n{deep}")).contains("deeper than 64"));
        let flow = format!(
            "keybinding:\n  u: {}{}\n",
            "{a: ".repeat(100),
            "}".repeat(100)
        );
        assert!(err(&flow).contains("deeper than 64"));
        let full =
            "keybinding:\n  u:\n    quit: [\"a\\u00e9\", 'b''c', { x: y }]\n    'q': \"\\x4\"\n";
        let chars: Vec<char> = full.chars().collect();
        for cut in 0..=chars.len() {
            let text: String = chars[..cut].iter().collect();
            let _ = keybinding(&text);
            let _ = keybinding(&format!("{text}é\"'"));
        }
        for junk in [
            "\"",
            "'",
            ":",
            "- ",
            "-",
            "[",
            "{",
            "keybinding",
            "keybinding:\n  :",
            "keybinding:\n  ''",
        ] {
            let _ = keybinding(junk);
        }
    }
}
