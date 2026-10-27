# keybind-find-sheet

The types [keybind-find](https://github.com/Benjmhart/keybind-find) reads, for parser plugins written in Rust: the
sheet (`Sheet`, `Bind`), the plugin protocol (`Request`, `Response`, and the `Manifest` a plugin prints for
`--manifest`), the key notation checks, and a runner that does the stdin/stdout part.

```rust,no_run
use keybind_find_sheet::{Bind, Sheet};
use std::collections::BTreeMap;

/// Lines of `<key> <description>`.
fn parse(text: &str) -> anyhow::Result<Sheet> {
    let binds = text
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(key, desc)| Bind { key: key.into(), desc: desc.into(), action: String::new(), when: Default::default() })
        .collect();
    Ok(Sheet {
        title: "My app".into(),
        provider: "myapp".into(),
        binds: BTreeMap::from([("general".to_string(), binds)]),
    })
}

fn main() {
    keybind_find_sheet::run(parse)
}
```

`run` reads the request keybind-find sends on stdin, calls `parse` on each input's text, checks every key, and
prints the version-1 response. On an error (or a panic in `parse`) it prints one line to stderr and exits with
status 1. Stdout is the response, so print anything else with `eprintln!`. A plugin that reads
several inputs together, or none (one that asks a running program), uses `run_request` instead.

`keybind-find plugin add` and a stanza's defaults come from the plugin's answer to `--manifest`: a `Manifest`
(`version` 1, name, description, default `inputs` and `match`, `queries`, `homepage`) printed as JSON on stdout,
exit status 0. Answer it before calling `run`, which would otherwise wait for a request on stdin.

In tests, `testing::assert_fixture(parse, "tests/fixtures/input", "tests/fixtures/expected.json")` compares the
response for a sample input with the JSON you expect.

## Key notation

A key is one or more chords separated by spaces (`ctrl+b c`; any whitespace reads as one space). A chord is `+`-joined: modifiers first, then an X
keysym name (`Return`, `plus`, `k`, `F5`, `XF86AudioRaiseVolume`). The modifiers are a closed set, `Modifier`:
`super`, `ctrl`, `alt`, `shift`, `meta`, `fn`, `mod2`, `mod3`, `mod5`, with `win` and `mod4` read as `super` and
`option` and `mod1` as `alt`. Anything else is a parse error. A shifted letter is `shift+k`; `K` reads as that.

`"shift+ctrl+k".parse::<Key>()` gives a `Key` (chords of a modifier set and a `Keysym`) that prints canonically:
`ctrl+shift+k`. `run` parses every key a plugin returns this way and prints the canonical spelling, so a bad key is
an error and not a sheet keybind-find has to guess about. Keysyms are checked for their spelling only, not against
X's table.

License: MIT.
