//! sway: reads `bindsym` lines from the sway config, following `include` and substituting `set $var`.

use crate::context::Context;
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{bail, Context as _};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    sheet_from(&|name: &str| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

fn sheet_from(env: Env) -> anyhow::Result<Sheet> {
    let paths = config_paths(env);
    let path = paths.iter().find(|p| p.is_file()).with_context(|| {
        let tried: Vec<_> = paths.iter().map(|p| p.display().to_string()).collect();
        format!("no sway config (looked for {})", tried.join(", "))
    })?;
    let mut lines = Vec::new();
    let mut notes = Vec::new();
    load(
        path,
        env,
        &mut Vec::new(),
        &mut HashSet::new(),
        &mut lines,
        &mut notes,
    )?;
    for note in notes {
        eprintln!("keybind-find: sway: {note}");
    }
    let sheet = parse(&lines.join("\n"))?;
    if sheet.binds.is_empty() {
        bail!("{} has no bindsym lines", path.display());
    }
    Ok(sheet)
}

/// Where sway looks for its config, in its order; the first that exists is used. `$XDG_CONFIG_HOME` counts only when
/// absolute (as the XDG spec says), else `$HOME/.config`. ponytail: `sway -c <file>` isn't seen.
fn config_paths(env: Env) -> Vec<PathBuf> {
    let home = env("HOME").map(PathBuf::from);
    let xdg = env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let mut paths = Vec::new();
    paths.extend(home.as_ref().map(|h| h.join(".sway/config")));
    paths.extend(xdg.as_ref().map(|x| x.join("sway/config")));
    paths.extend(home.as_ref().map(|h| h.join(".i3/config")));
    paths.extend(xdg.as_ref().map(|x| x.join("i3/config")));
    paths.extend(["/etc/sway/config", "/etc/i3/config"].map(PathBuf::from));
    paths
}

/// Appends `path`'s logical lines to `out`, with each `include` replaced by the file(s) it names. `set` variables seen so far
/// are substituted in an include's path before environment variables, as sway does. A file is read once (as sway
/// does), so includes can't loop. An include that can't be read is a note, not an error; `path` itself is.
fn load(
    path: &Path,
    env: Env,
    vars: &mut Vec<(String, String)>,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<String>,
    notes: &mut Vec<String>,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    // Sway reads bytes; don't let one odd byte (a Latin-1 comment) lose the whole file.
    let text = String::from_utf8_lossy(&bytes);
    seen.insert(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    for line in file_lines(&path.display().to_string(), &text, notes) {
        let Some(target) = line
            .strip_prefix("include")
            .filter(|r| r.starts_with([' ', '\t']))
        else {
            set_var(&line, vars);
            out.push(line);
            continue;
        };
        let target = unquote(substitute(target.trim(), vars).trim()).to_string();
        let expanded = match expand(&target, env) {
            Ok(p) => p,
            Err(var) => {
                notes.push(format!(
                    "skipping include {target}: ${var} is not set or empty"
                ));
                continue;
            }
        };
        // Relative to the including file's directory, as sway resolves it.
        let expanded = path.parent().unwrap_or(Path::new(".")).join(expanded);
        let files = glob(&expanded);
        if files.is_empty() {
            notes.push(format!(
                "include {target}: no file at {}",
                expanded.display()
            ));
        }
        for file in files {
            if seen.contains(&file.canonicalize().unwrap_or_else(|_| file.clone())) {
                continue;
            }
            if let Err(e) = load(&file, env, vars, seen, out, notes) {
                notes.push(format!("include {target}: {e:#}"));
            }
        }
    }
    Ok(())
}

/// One file's logical lines with its blocks balanced, for `parse` to read joined with other files' (`label` names the
/// file in notes). Blocks don't span files in sway: a stray `}` can't close an earlier file's block (dropped, with a
/// note), and an unclosed one ends with its file (closed, with a note).
pub(crate) fn file_lines(label: &str, text: &str, notes: &mut Vec<String>) -> Vec<String> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for line in logical_lines(text) {
        if line == "}" {
            if depth == 0 {
                notes.push(format!("{label}: a `}}` with no block to close"));
                continue;
            }
            depth -= 1;
        } else if line.ends_with('{') {
            depth += 1;
        }
        // `parse` reads these lines again: a trailing space keeps a `\` that ended a line followed by spaces (not a
        // continuation) from turning into one.
        out.push(match line.ends_with('\\') {
            true => format!("{line} "),
            false => line,
        });
    }
    if depth > 0 {
        notes.push(format!("{label}: {depth} block(s) not closed"));
        out.extend(std::iter::repeat_n("}".to_string(), depth));
    }
    out
}

/// The file(s) a path names: itself if it exists, or with a `*` in the last part, the matches in its directory,
/// sorted, leaving out dotfiles unless the pattern starts with `.` (as the shell does). ponytail: `*` in the file name
/// only, which covers `include /etc/sway/config.d/*`; sway's wordexp does more.
fn glob(path: &Path) -> Vec<PathBuf> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !name.contains('*') {
        return if path.is_file() {
            vec![path.to_path_buf()]
        } else {
            vec![]
        };
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                (name.starts_with('.') || !n.starts_with('.')) && wildcard(name, n)
            })
        })
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

/// `*` matches any run of characters; everything else matches itself.
fn wildcard(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, tail)) => {
            name.starts_with(head)
                && (0..=name.len() - head.len())
                    .filter(|&i| name.is_char_boundary(head.len() + i))
                    .any(|i| wildcard(tail, &name[head.len() + i..]))
        }
    }
}

/// A leading `~` becomes `$HOME`; `$NAME` and `${NAME}` become that environment variable. `Err(NAME)` when one is
/// unset or empty.
fn expand(path: &str, env: Env) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = path;
    if rest == "~" || rest.starts_with("~/") {
        out.push_str(&env("HOME").ok_or("HOME")?);
        rest = &rest[1..];
    }
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let (name, next) = match after.strip_prefix('{').and_then(|a| a.split_once('}')) {
            Some((name, next)) => (name, next),
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        if name.is_empty() {
            out.push('$');
        } else {
            out.push_str(&env(name).ok_or(name)?);
        }
        rest = next;
    }
    out.push_str(rest);
    Ok(out)
}

/// Trimmed lines with `\`-continued lines joined as they stand (only a `\` that ends the line, as sway reads it), and
/// a `{` alone on a line joined to the line before (sway allows the brace there), without blank lines or `#` comments.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut pending = String::new();
    for raw in text.lines() {
        let line = if pending.is_empty() {
            raw.trim_start()
        } else {
            raw
        };
        // A comment ends at its line, `\` or not.
        if pending.is_empty() && line.starts_with('#') {
            continue;
        }
        if let Some(head) = line.strip_suffix('\\') {
            pending.push_str(head);
            continue;
        }
        pending.push_str(line);
        let full = std::mem::take(&mut pending).trim().to_string();
        match lines.last_mut() {
            Some(last) if full == "{" => last.push_str(" {"),
            _ if full.is_empty() || full.starts_with('#') => {}
            _ => lines.push(full),
        }
    }
    if !pending.trim().is_empty() {
        lines.push(pending.trim().to_string());
    }
    lines
}

/// Binds from config text whose includes are already in place. `set $var value` is substituted in later lines (a
/// variable used before it is set stays literal). A block (`name { … }`) puts its header in front of each line inside,
/// as sway does, so `mode "resize" { bindsym … }` binds go in category `resize`, `bindsym { … }` lines are binds, and
/// a `bar { bindsym … }` is not a key binding. Top-level binds (and mode `default`'s) go in `sway`. A key bound twice
/// with the same flags keeps its last entry, and `unbindsym` removes one.
pub fn parse(text: &str) -> anyhow::Result<Sheet> {
    parse_as("sway", text)
}

/// `parse` for another window manager with sway's config language (i3): `provider` is the sheet's title, provider
/// and top-level category.
pub(crate) fn parse_as(provider: &str, text: &str) -> anyhow::Result<Sheet> {
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    // (category, flags, bind) in file order.
    let mut binds: Vec<(String, String, Bind)> = Vec::new();
    for line in logical_lines(text) {
        if line == "}" {
            blocks.pop();
            continue;
        }
        if set_var(&line, &mut vars) {
            continue;
        }
        let line = substitute(&line, &vars);
        if let Some(header) = line.strip_suffix('{') {
            blocks.push(header.trim_end().to_string());
            continue;
        }
        let full = blocks.iter().map(|b| format!("{b} ")).collect::<String>() + &line;
        let (category, command) = match mode(&full) {
            Some((name, rest)) if name != "default" => (name, rest),
            Some((_, rest)) => (provider.to_string(), rest),
            None => (provider.to_string(), full.as_str()),
        };
        let unbind = split_bind(command, "unbindsym");
        let bind = split_bind(command, "bindsym");
        let Some((flags, keys, action)) = unbind.as_ref().or(bind.as_ref()) else {
            continue;
        };
        let Some(key) = key(keys) else {
            continue;
        };
        // A bindsym with no command is refused by sway and changes nothing.
        if bind.is_some() && action.is_empty() {
            continue;
        }
        binds.retain(|(c, f, b)| !(*c == category && f == flags && b.key == key));
        if bind.is_some() {
            binds.push((
                category,
                flags.clone(),
                Bind {
                    key,
                    desc: action.to_string(),
                    action: action.to_string(),
                    when: Default::default(),
                },
            ));
        }
    }
    let mut by_category: BTreeMap<String, Vec<Bind>> = BTreeMap::new();
    for (c, _, b) in binds {
        by_category.entry(c).or_default().push(b);
    }
    Ok(Sheet {
        title: provider.into(),
        provider: provider.into(),
        binds: by_category,
    })
}

/// Records `set $name value` (the value substituted, one pair of outer quotes removed, as sway stores it). Like sway,
/// the name itself is never substituted. `false` if the line isn't a `set`.
fn set_var(line: &str, vars: &mut Vec<(String, String)>) -> bool {
    let Some(rest) = line
        .strip_prefix("set")
        .filter(|r| r.starts_with([' ', '\t']))
    else {
        return false;
    };
    if let Some((name, value)) = rest.trim().split_once(char::is_whitespace) {
        let value = unquote(substitute(value.trim(), vars).trim()).to_string();
        // sway adds the `$` a name is written without.
        let name = match name.starts_with('$') {
            true => name.to_string(),
            false => format!("${name}"),
        };
        vars.retain(|(n, _)| *n != name);
        vars.push((name, value));
        // Longest first, so `$mod` doesn't eat the start of `$modx`.
        vars.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
    }
    true
}

/// `"x"` or `'x'` → `x`; anything else as is.
fn unquote(s: &str) -> &str {
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner;
        }
    }
    s
}

/// Replaces every set variable in `line`. `vars` is sorted longest name first.
fn substitute(line: &str, vars: &[(String, String)]) -> String {
    if !line.contains('$') {
        return line.to_string();
    }
    let mut out = String::new();
    let mut rest = line;
    'outer: while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        for (name, value) in vars {
            if let Some(r) = rest.strip_prefix(name.as_str()) {
                out.push_str(value);
                rest = r;
                continue 'outer;
            }
        }
        out.push('$');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// `mode [--pango_markup] "<name>" <command>` (or `'<name>'`, or a bare word) → the name and the command.
fn mode(line: &str) -> Option<(String, &str)> {
    let mut rest = line.strip_prefix("mode")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    rest = rest.trim_start();
    if let Some(r) = rest.strip_prefix("--pango_markup") {
        rest = r.trim_start();
    }
    let (name, after) = match rest.chars().next() {
        Some(q @ ('"' | '\'')) => rest[1..].split_once(q)?,
        _ => rest.split_once(char::is_whitespace)?,
    };
    Some((name.to_string(), after.trim_start()))
}

/// The flags that make a binding a different one to sway; `--to-code`, `--no-repeat`, `--no-warn` don't.
const IDENTITY_FLAGS: &[&str] = &[
    "--release",
    "--locked",
    "--inhibited",
    "--border",
    "--whole-window",
    "--exclude-titlebar",
];

/// `<verb> [--flags…] <keys> [<command>]` → the flags that set the binding apart (sorted, space-joined; see
/// `IDENTITY_FLAGS`, plus `--input-device=`), the keys and the command (maybe empty). `None` if the command isn't that
/// verb or has no keys.
fn split_bind<'a>(command: &'a str, verb: &str) -> Option<(String, &'a str, &'a str)> {
    let mut rest = command.strip_prefix(verb)?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut flags = Vec::new();
    loop {
        rest = rest.trim_start();
        let (word, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if word.is_empty() {
            return None;
        }
        rest = after;
        if !word.starts_with("--") {
            flags.sort();
            return Some((flags.join(" "), word, rest.trim()));
        }
        if IDENTITY_FLAGS.contains(&word) || word.starts_with("--input-device=") {
            flags.push(word);
        }
    }
}

/// Sway's modifier names (case-insensitive, as sway reads them) in the key notation.
const MODS: &[(&str, &str)] = &[
    ("mod4", "super"),
    ("super", "super"),
    ("control", "ctrl"),
    ("ctrl", "ctrl"),
    ("mod1", "alt"),
    ("alt", "alt"),
    ("shift", "shift"),
    ("mod2", "mod2"),
    ("mod3", "mod3"),
    ("mod5", "mod5"),
];

/// `Mod4+Shift+q` → `super+shift+q`. The last part is the key in XKB keysym spelling; sway reads a letter in either
/// case as the same key, so a letter is lowercased. An unknown modifier, or a mouse button (`button3`, `BTN_LEFT`),
/// → `None`. ponytail: other keysyms keep sway's spelling (`return` isn't turned into `Return`).
fn key(sway: &str) -> Option<String> {
    let parts: Vec<&str> = sway.split('+').collect();
    let (keysym, mods) = parts.split_last()?;
    let lower = keysym.to_ascii_lowercase();
    let button = lower
        .strip_prefix("button")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if keysym.is_empty() || button || keysym.starts_with("BTN_") {
        return None;
    }
    let keysym = if keysym.len() == 1 && keysym.chars().all(|c| c.is_ascii_alphabetic()) {
        lower.as_str()
    } else {
        keysym
    };
    let mods = mods
        .iter()
        .map(|m| {
            let m = m.to_ascii_lowercase();
            MODS.iter().find(|(name, _)| *name == m).map(|(_, n)| *n)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(chord(&mods, keysym))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_env(name: &str) -> Option<String> {
        match name {
            "HOME" => Some("/home/test".into()),
            "KBF_HOST" => Some("host".into()),
            _ => None,
        }
    }

    fn fixture() -> (Sheet, Vec<String>) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sway.config");
        let (mut lines, mut notes) = (Vec::new(), Vec::new());
        load(
            &path,
            &fixture_env,
            &mut Vec::new(),
            &mut HashSet::new(),
            &mut lines,
            &mut notes,
        )
        .unwrap();
        (parse(&lines.join("\n")).unwrap(), notes)
    }

    #[test]
    fn parse_is_parse_as_sway() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sway.config");
        let (mut lines, mut notes) = (Vec::new(), Vec::new());
        load(
            &path,
            &fixture_env,
            &mut Vec::new(),
            &mut HashSet::new(),
            &mut lines,
            &mut notes,
        )
        .unwrap();
        let text = lines.join("\n");
        let json = |s: Sheet| serde_json::to_string(&s).unwrap();
        assert_eq!(
            json(parse(&text).unwrap()),
            json(parse_as("sway", &text).unwrap())
        );
        // Another provider names the sheet and the top-level category, nothing else.
        let i3 = parse_as("i3", &text).unwrap();
        assert_eq!((i3.title.as_str(), i3.provider.as_str()), ("i3", "i3"));
        assert!(i3.binds.contains_key("i3") && !i3.binds.contains_key("sway"));
        assert_eq!(i3.binds["i3"].len(), fixture().0.binds["sway"].len());
    }

    fn find<'a>(sheet: &'a Sheet, category: &str, key: &str) -> Option<&'a Bind> {
        sheet.binds.get(category)?.iter().find(|b| b.key == key)
    }

    #[test]
    fn fixture_binds() {
        let (sheet, _) = fixture();
        let b = find(&sheet, "sway", "super+Return").unwrap();
        assert_eq!(b.desc, "exec alacritty");
        assert_eq!(b.action, b.desc);
        assert_eq!(find(&sheet, "sway", "super+shift+q").unwrap().desc, "kill");
        // --release skipped; the criteria stay in the command.
        assert_eq!(find(&sheet, "sway", "Print").unwrap().desc, "exec grim");
        assert_eq!(
            find(&sheet, "sway", "ctrl+space").unwrap().desc,
            "[urgent=latest] focus"
        );
        // Continued line, joined as it stands.
        assert_eq!(
            find(&sheet, "sway", "super+d").unwrap().desc,
            "exec fuzzel     --width 60"
        );
        // A var used before it is set stays literal.
        assert_eq!(
            find(&sheet, "sway", "super+b").unwrap().desc,
            "exec $browser"
        );
        // Mode block.
        let resize = &sheet.binds["resize"];
        assert_eq!(resize.len(), 3, "{resize:?}");
        assert_eq!(
            find(&sheet, "resize", "Left").unwrap().desc,
            "resize shrink width 10px"
        );
        assert_eq!(
            find(&sheet, "resize", "Escape").unwrap().desc,
            "mode \"default\""
        );
        // Includes: a relative one, one through an environment variable, and a glob.
        assert_eq!(find(&sheet, "sway", "super+e").unwrap().desc, "exec thunar");
        assert_eq!(
            find(&sheet, "sway", "super+h").unwrap().desc,
            "exec host-thing"
        );
        // Glob matches are read in order, so glob-b.conf's rebind wins.
        assert_eq!(find(&sheet, "sway", "super+g").unwrap().desc, "exec glob-b");
        // A bar block's key binding is the bar's, not a key bind.
        assert!(find(&sheet, "sway", "super+p").is_none());
        // An include through a set variable, a quoted one, and a glob that leaves out dotfiles.
        assert_eq!(
            find(&sheet, "sway", "super+v").unwrap().desc,
            "exec via-set-var"
        );
        assert_eq!(find(&sheet, "sway", "super+u").unwrap().desc, "exec quoted");
        assert_eq!(find(&sheet, "sway", "super+y").unwrap().desc, "exec y-dot");
        assert!(find(&sheet, "sway", "super+x").is_none());
        // Comments, indented or not, are not binds.
        assert!(find(&sheet, "sway", "super+z").is_none());
        // The bar's mouse binding and bindcode lines are not key binds.
        assert!(sheet
            .binds
            .values()
            .flatten()
            .all(|b| !b.key.contains("button")));
        assert!(sheet.binds.values().flatten().all(|b| b.key != "super+38"));
        assert_eq!(sheet.binds.len(), 2, "{:?}", sheet.binds.keys());
    }

    #[test]
    fn fixture_notes_missing_includes() {
        let (_, notes) = fixture();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("$KBF_UNSET is not set"), "{notes:?}");
        assert!(notes[1].contains("no file at"), "{notes:?}");
    }

    #[test]
    fn keys_and_modifiers() {
        assert_eq!(key("Mod4+Shift+q").as_deref(), Some("super+shift+q"));
        assert_eq!(key("Shift+Mod4+q").as_deref(), Some("super+shift+q"));
        assert_eq!(
            key("Control+Mod1+Delete").as_deref(),
            Some("ctrl+alt+Delete")
        );
        assert_eq!(key("ctrl+alt+t").as_deref(), Some("ctrl+alt+t"));
        assert_eq!(key("Print").as_deref(), Some("Print"));
        assert_eq!(key("Hyper+x"), None);
        assert_eq!(key("Mod4+"), None);
    }

    #[test]
    fn keys_fold_letter_case_and_skip_mouse_buttons() {
        assert_eq!(key("Mod4+Q").as_deref(), Some("super+q"));
        assert_eq!(key("Mod4+Shift+Q").as_deref(), Some("super+shift+q"));
        assert_eq!(key("F1").as_deref(), Some("F1"));
        assert_eq!(key("Mod4+button3"), None);
        assert_eq!(key("button4"), None);
        assert_eq!(key("BTN_LEFT"), None);
        assert_eq!(key("Mod4+buttons").as_deref(), Some("super+buttons"));
    }

    #[test]
    fn bind_lines() {
        assert_eq!(
            split_bind(
                "bindsym --to-code --release --no-repeat --locked Mod4+x exec foo  bar",
                "bindsym"
            ),
            Some(("--locked --release".into(), "Mod4+x", "exec foo  bar"))
        );
        assert_eq!(
            split_bind("unbindsym --release Mod4+x", "unbindsym"),
            Some(("--release".into(), "Mod4+x", ""))
        );
        assert_eq!(split_bind("bindsym --release", "bindsym"), None);
        assert_eq!(split_bind("bindsymx a b", "bindsym"), None);
        assert_eq!(split_bind("bindcode 38 exec x", "bindsym"), None);
        // No command: not a bind.
        assert!(parse("bindsym Mod4+x\n").unwrap().binds.is_empty());
    }

    #[test]
    fn set_vars() {
        let s =
            parse("set $mod Mod4\nset $modx Mod1\nbindsym $modx+a x\nbindsym $mod+a y\n").unwrap();
        assert_eq!(find(&s, "sway", "alt+a").unwrap().desc, "x");
        assert_eq!(find(&s, "sway", "super+a").unwrap().desc, "y");
        // A value built from an earlier variable; a redefinition applies from there on; a tab after `set`.
        let s = parse(
            "set $mod Mod4\nset $left $mod+h\nbindsym $left x\nset\t$mod Mod1\nbindsym $mod+a y\n",
        )
        .unwrap();
        assert_eq!(find(&s, "sway", "super+h").unwrap().desc, "x");
        assert_eq!(find(&s, "sway", "alt+a").unwrap().desc, "y");
    }

    #[test]
    fn quoted_set_value_names_a_mode() {
        // The i3/sway "system mode" idiom: the quotes belong to the set, not the name.
        let s = parse(
            "set $mode_system \"System (l)ock, (e)xit\"\nmode \"$mode_system\" {\n  bindsym l exec swaylock\n}\n",
        )
        .unwrap();
        assert_eq!(
            find(&s, "System (l)ock, (e)xit", "l").unwrap().desc,
            "exec swaylock"
        );
    }

    #[test]
    fn modes_with_spaces_and_markup() {
        let s = parse("mode --pango_markup \"<b>move</b> it\" {\n  bindsym h move left\n}\nmode \"x\" bindsym j y\n")
            .unwrap();
        assert_eq!(find(&s, "<b>move</b> it", "h").unwrap().desc, "move left");
        assert_eq!(find(&s, "x", "j").unwrap().desc, "y");
        let s = parse("mode resize {\nbindsym Left x\n}\nmode \"default\" {\nbindsym k top\n}\n")
            .unwrap();
        assert_eq!(find(&s, "resize", "Left").unwrap().desc, "x");
        // Mode `default` is the top level.
        assert_eq!(find(&s, "sway", "k").unwrap().desc, "top");
    }

    #[test]
    fn bindsym_block() {
        let s = parse("bindsym {\n  Mod4+a x\n}\nbindsym --release {\n  Mod4+b y\n}\n").unwrap();
        assert_eq!(find(&s, "sway", "super+a").unwrap().desc, "x");
        assert_eq!(find(&s, "sway", "super+b").unwrap().desc, "y");
    }

    #[test]
    fn last_bind_wins_per_category_and_flags() {
        let s = parse("bindsym a one\nbindsym a two\n").unwrap();
        assert_eq!(s.binds["sway"].len(), 1);
        assert_eq!(s.binds["sway"][0].desc, "two");
        // A mode's Escape doesn't replace the top-level one.
        let s = parse("bindsym Escape a\nmode \"m\" {\nbindsym Escape b\n}\n").unwrap();
        assert_eq!(find(&s, "sway", "Escape").unwrap().desc, "a");
        assert_eq!(find(&s, "m", "Escape").unwrap().desc, "b");
        // Different flags are different bindings to sway.
        let s = parse("bindsym Print exec grim\nbindsym --release Print exec slurp\n").unwrap();
        assert_eq!(s.binds["sway"].len(), 2);
    }

    #[test]
    fn unbindsym_removes_a_bind() {
        let s = parse(
            "set $mod Mod4\nbindsym $mod+q kill\nbindsym --release $mod+q other\nbindsym $mod+w keep\nunbindsym $mod+Q\n",
        )
        .unwrap();
        assert!(find(&s, "sway", "super+w").is_some());
        let q: Vec<_> = s.binds["sway"]
            .iter()
            .filter(|b| b.key == "super+q")
            .collect();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].desc, "other");
    }

    #[test]
    fn mouse_binds_are_not_listed() {
        let s =
            parse("bindsym --whole-window Mod4+button2 kill\nbindsym BTN_LEFT x\nbindsym a b\n")
                .unwrap();
        assert_eq!(s.binds["sway"].len(), 1);
    }

    #[test]
    fn continuation_only_at_the_very_end() {
        // A `\` on the last line, and a `\` followed by a space (no continuation).
        let s = parse("bindsym a x \\").unwrap();
        assert_eq!(find(&s, "sway", "a").unwrap().desc, "x");
        let s = parse("bindsym a x \\ \nbindsym b y\n").unwrap();
        assert_eq!(find(&s, "sway", "a").unwrap().desc, "x \\");
        assert_eq!(find(&s, "sway", "b").unwrap().desc, "y");
    }

    #[test]
    fn expands_paths() {
        assert_eq!(
            expand("~/x/$KBF_HOST.conf", &fixture_env).unwrap(),
            "/home/test/x/host.conf"
        );
        assert_eq!(expand("/a/${KBF_HOST}b", &fixture_env).unwrap(), "/a/hostb");
        assert_eq!(
            expand("/a/$KBF_UNSET.conf", &fixture_env).unwrap_err(),
            "KBF_UNSET"
        );
        assert_eq!(expand("/a/$/b", &fixture_env).unwrap(), "/a/$/b");
        assert_eq!(expand("a~b", &fixture_env).unwrap(), "a~b");
    }

    #[test]
    fn unquotes() {
        assert_eq!(unquote("\"a b\""), "a b");
        assert_eq!(unquote("'a b'"), "a b");
        assert_eq!(unquote("\"a'"), "\"a'");
        assert_eq!(unquote("a"), "a");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*", "anything"));
        assert!(wildcard("*.conf", "a.conf"));
        assert!(!wildcard("*.conf", "a.conff"));
        assert!(wildcard("a*b*c", "aXbYc"));
        assert!(!wildcard("a*", "ba"));
        assert!(wildcard("é*", "éx"));
    }

    #[test]
    fn config_paths_in_sways_order() {
        let env = |n: &str| match n {
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        let want: Vec<PathBuf> = [
            "/h/.sway/config",
            "/xdg/sway/config",
            "/h/.i3/config",
            "/xdg/i3/config",
            "/etc/sway/config",
            "/etc/i3/config",
        ]
        .map(PathBuf::from)
        .to_vec();
        assert_eq!(config_paths(&env), want);
        let rel = |n: &str| match n {
            "XDG_CONFIG_HOME" => Some("rel".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(config_paths(&rel)[1], Path::new("/h/.config/sway/config"));
        assert_eq!(
            config_paths(&|_| None),
            [
                PathBuf::from("/etc/sway/config"),
                PathBuf::from("/etc/i3/config")
            ]
        );
    }

    /// A config dir under the temp dir, with `files` written into `<dir>/sway/`.
    fn config_home(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kbf-sway-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sway")).unwrap();
        for (f, text) in files {
            std::fs::write(dir.join("sway").join(f), text).unwrap();
        }
        dir
    }

    #[test]
    fn sheet_from_reads_the_config_and_absolute_includes() {
        let dir = config_home("abs", &[]);
        let extra = dir.join("sway/abs.conf");
        std::fs::write(&extra, "bindsym Mod4+a abs\n").unwrap();
        std::fs::write(
            dir.join("sway/config"),
            format!("include {}\nbindsym Mod4+b top\n", extra.display()),
        )
        .unwrap();
        let xdg = dir.display().to_string();
        let env = move |n: &str| (n == "XDG_CONFIG_HOME").then(|| xdg.clone());
        let s = sheet_from(&env).unwrap();
        assert_eq!(find(&s, "sway", "super+a").unwrap().desc, "abs");
        assert_eq!(find(&s, "sway", "super+b").unwrap().desc, "top");
    }

    #[test]
    fn sheet_from_without_binds_is_an_error() {
        let dir = config_home("empty", &[("config", "# nothing\nset $mod Mod4\n")]);
        let xdg = dir.display().to_string();
        let env = move |n: &str| (n == "XDG_CONFIG_HOME").then(|| xdg.clone());
        let e = sheet_from(&env).unwrap_err().to_string();
        assert!(e.contains("no bindsym lines"), "{e}");
    }

    #[test]
    fn sway_syntax_corners() {
        // A brace on the next line; a single-quoted mode name; nested blocks.
        let s = parse("mode \"resize\"\n{\nbindsym Left x\n}\nmode 'my mode' {\nbindsym a b\n}\n")
            .unwrap();
        assert_eq!(find(&s, "resize", "Left").unwrap().desc, "x");
        assert_eq!(find(&s, "my mode", "a").unwrap().desc, "b");
        let s =
            parse("mode \"m\" {\n bindsym {\n  a x\n }\n bindsym b y\n}\nbindsym c z\n").unwrap();
        assert_eq!(find(&s, "m", "a").unwrap().desc, "x");
        assert_eq!(find(&s, "m", "b").unwrap().desc, "y");
        assert_eq!(find(&s, "sway", "c").unwrap().desc, "z");
        // `set` without the `$`, as sway allows.
        let s = parse("set mod Mod4\nbindsym $mod+a x\n").unwrap();
        assert_eq!(find(&s, "sway", "super+a").unwrap().desc, "x");
        // A comment ending in `\` doesn't swallow the next line.
        let s = parse("# old \\\nbindsym a b\n").unwrap();
        assert_eq!(find(&s, "sway", "a").unwrap().desc, "b");
        // A bindsym with no command changes nothing.
        let s = parse("bindsym a one\nbindsym a\n").unwrap();
        assert_eq!(find(&s, "sway", "a").unwrap().desc, "one");
    }

    #[test]
    fn only_identity_flags_set_a_binding_apart() {
        // --no-warn is the override idiom: same binding to sway.
        let s = parse("bindsym Mod4+q y\nbindsym --no-warn Mod4+q x\n").unwrap();
        assert_eq!(s.binds["sway"].len(), 1);
        assert_eq!(s.binds["sway"][0].desc, "x");
        let s = parse("bindsym --no-repeat --to-code Mod4+q kill\nunbindsym Mod4+q\n").unwrap();
        assert!(s.binds.is_empty());
        let s = parse("bindsym --locked a x\nbindsym --input-device=1:1:kbd a y\nbindsym a z\n")
            .unwrap();
        assert_eq!(s.binds["sway"].len(), 3);
    }

    #[test]
    fn more_keys_and_paths() {
        assert_eq!(key("Super+x").as_deref(), Some("super+x"));
        assert_eq!(key("Mod2+KP_End").as_deref(), Some("mod2+KP_End"));
        assert_eq!(expand("~", &fixture_env).unwrap(), "/home/test");
    }

    #[test]
    fn glob_reads_dotfiles_only_for_a_dot_pattern() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sway.config.d");
        assert_eq!(glob(&d.join(".*.dot")), [d.join(".x.dot")]);
        assert_eq!(glob(&d.join("*.dot")), [d.join("y.dot")]);
        assert_eq!(
            glob(&d.join("glob-*.conf")),
            [d.join("glob-a.conf"), d.join("glob-b.conf")]
        );
    }

    fn sheet_in(name: &str, files: &[(&str, &[u8])]) -> anyhow::Result<Sheet> {
        let dir = config_home(name, &[]);
        for (f, bytes) in files {
            std::fs::write(dir.join("sway").join(f), bytes).unwrap();
        }
        let xdg = dir.display().to_string();
        sheet_from(&move |n: &str| (n == "XDG_CONFIG_HOME").then(|| xdg.clone()))
    }

    #[test]
    fn load_then_parse_keeps_lines_apart() {
        // A `\` followed by a space ends no line, even after load has trimmed it.
        let s = sheet_in("cont", &[("config", b"bindsym a x \\ \nbindsym b y\n")]).unwrap();
        assert_eq!(find(&s, "sway", "a").unwrap().desc, "x \\");
        assert_eq!(find(&s, "sway", "b").unwrap().desc, "y");
    }

    #[test]
    fn blocks_end_at_the_end_of_their_file() {
        let s = sheet_in(
            "blocks",
            &[
                ("open.conf", b"bar {\n position top\n"),
                ("stray.conf", b"}\n"),
                (
                    "config",
                    b"mode \"m\" {\ninclude stray.conf\nbindsym a x\n}\ninclude open.conf\nbindsym b y\n",
                ),
            ],
        )
        .unwrap();
        assert_eq!(find(&s, "m", "a").unwrap().desc, "x");
        assert_eq!(find(&s, "sway", "b").unwrap().desc, "y");
    }

    #[test]
    fn odd_bytes_and_tabs() {
        // Invalid UTF-8 (a Latin-1 comment) in the config and an include after a tab.
        let s = sheet_in(
            "bytes",
            &[
                ("inc.conf", b"bindsym i x\n"),
                ("config", b"# caf\xe9\ninclude\tinc.conf\nbindsym a b\n"),
            ],
        )
        .unwrap();
        assert_eq!(find(&s, "sway", "i").unwrap().desc, "x");
        assert_eq!(find(&s, "sway", "a").unwrap().desc, "b");
    }

    #[test]
    fn an_unreadable_include_is_a_note() {
        use std::os::unix::fs::PermissionsExt;
        let dir = config_home(
            "unreadable",
            &[
                ("config", "include sub.conf\nbindsym a b\n"),
                ("sub.conf", "bindsym s t\n"),
            ],
        );
        let sub = dir.join("sway/sub.conf");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&sub).is_ok() {
            return; // root reads it anyway
        }
        let (mut lines, mut notes) = (Vec::new(), Vec::new());
        load(
            &dir.join("sway/config"),
            &fixture_env,
            &mut Vec::new(),
            &mut HashSet::new(),
            &mut lines,
            &mut notes,
        )
        .unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("reading"), "{notes:?}");
        assert_eq!(lines, ["bindsym a b"]);
    }

    #[test]
    fn missing_config_is_an_error() {
        let mut notes = Vec::new();
        let e = load(
            Path::new("/nonexistent-kbf/sway/config"),
            &fixture_env,
            &mut Vec::new(),
            &mut HashSet::new(),
            &mut Vec::new(),
            &mut notes,
        )
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("/nonexistent-kbf/sway/config"),
            "{e:#}"
        );
    }
}
