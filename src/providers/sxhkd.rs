//! sxhkd: the hotkeys of the user's running sxhkd (bspwm's keys, or any X11 window manager's), read from the config
//! files each sxhkd process loaded, as sxhkd 0.6.3 reads them. Commands are shown, never run.
//! Ported from sxhkd at tag 0.6.3 (commit `2cc7aca`): `src/sxhkd.c` (options, config path), `src/parse.c`
//! (`load_config`, `process_hotkey`, `get_token`, `render_next`, `extract_chunks`, `parse_chain`), `src/parse.h`.

use crate::context::{own_uid, user_pids, Context};
use crate::sheet::{chord, Bind, Sheet};
use anyhow::bail;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// sxhkd's key names (`nks_dict`), sorted.
const KEY_NAMES: &[&str] = include!("sxhkd_keysyms.rs");

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let env = |k: &str| std::env::var_os(k);
    let (sheet, notes) = read(Path::new("/proc"), own_uid(), &env)?;
    // One line each: an unreadable file, the left-out count, no sxhkd running.
    for note in notes {
        eprintln!("keybind-find: sxhkd: {note}");
    }
    Ok(sheet)
}

/// The sheet from every config of `uid`'s sxhkd processes in `proc` (with none, the default path from `env`), and
/// the notes for stderr. An error only when no file at all could be read.
fn read(proc: &Path, uid: Option<u32>, env: Env) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut notes = Vec::new();
    let mut files = uid.map_or_else(Vec::new, |uid| config_files(proc, uid, env));
    if files.is_empty() {
        files = configs(&[], env);
        if let Some(path) = files.first() {
            notes.push(format!("no sxhkd running; reading {}", path.display()));
        }
    }
    let mut sheet = Sheet {
        title: "sxhkd".into(),
        provider: "sxhkd".into(),
        binds: Default::default(),
    };
    let (mut read_any, mut left_out) = (false, 0);
    for path in &files {
        match std::fs::read(path) {
            Ok(text) => {
                read_any = true;
                let (binds, n) = parse(&text);
                left_out += n;
                for (category, bind) in binds {
                    sheet.binds.entry(category).or_default().push(bind);
                }
            }
            Err(e) => notes.push(format!("can't read {}: {e}", path.display())),
        }
    }
    if !read_any {
        bail!("{}", notes.join("; "));
    }
    if left_out > 0 {
        notes.push(format!(
            "leaving out {left_out} hotkey(s): hyper, meta, mode_switch, lock, mod2, mod3, mod5 or any; a pointer \
             button; an upper-case letter; or a key name sxhkd doesn't know"
        ));
    }
    Ok((sheet, notes))
}

/// `uid`'s live `sxhkd` processes in `proc`, each with its `cmdline`. A zombie's (or an exiting one's) `cmdline` is
/// empty: skipped. So is a pid whose `comm` is no longer `sxhkd` once `cmdline` is read (the pid was reused).
pub(crate) fn running(proc: &Path, uid: u32) -> Vec<(u32, Vec<u8>)> {
    user_pids(proc, uid, "sxhkd")
        .into_iter()
        .filter_map(|pid| {
            let dir = proc.join(pid.to_string());
            let cmdline = std::fs::read(dir.join("cmdline"))
                .ok()
                .filter(|c| !c.is_empty())?;
            let comm = std::fs::read_to_string(dir.join("comm")).ok()?;
            (comm.trim_end_matches('\n') == "sxhkd").then_some((pid, cmdline))
        })
        .collect()
}

/// The config files of `uid`'s `sxhkd` processes in `proc`, each in the order its process loads them; a file an
/// earlier process (or the same one) already has is read once. The process's own environment and cwd decide its
/// paths: a process whose `environ` can't be read gets `env`.
fn config_files(proc: &Path, uid: u32, env: Env) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    for (pid, cmdline) in running(proc, uid) {
        let dir = proc.join(pid.to_string());
        let environ = std::fs::read(dir.join("environ")).ok();
        let own = |k: &str| match &environ {
            Some(e) => e
                .split(|b| *b == 0)
                .find_map(|v| v.strip_prefix(k.as_bytes())?.strip_prefix(b"="))
                .map(|v| OsStr::from_bytes(v).to_os_string()),
            None => env(k),
        };
        // Through the link itself, not its target: a deleted cwd or another mount namespace still resolves.
        let cwd = dir.join("cwd");
        // argv[0] is the program; a trailing NUL ends the last argument.
        let args: Vec<&[u8]> = cmdline
            .strip_suffix(&[0u8])
            .unwrap_or(&cmdline)
            .split(|b| *b == 0)
            .skip(1)
            .collect();
        for path in configs(&args, &own).into_iter().map(|p| cwd.join(p)) {
            if !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
}

/// The files sxhkd loads for the arguments `args` (argv without argv[0]): the main config, then each extra
/// (`src/sxhkd.c:78-124, 147-150`). getopt's `"hvm:t:c:r:s:a:"`, as glibc's getopt reads it: options and non-options
/// in any order (it permutes), `--` ends the options, an option's argument joined (`-cFILE`) or the next argument.
/// ponytail: `POSIXLY_CORRECT` in sxhkd's environment (options stop at the first non-option) isn't read.
fn configs(args: &[&[u8]], env: Env) -> Vec<PathBuf> {
    let path = |b: &[u8]| PathBuf::from(OsStr::from_bytes(b));
    let (mut main, mut extras) = (None, Vec::new());
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        if *a == b"--" {
            extras.extend(rest.by_ref().map(|a| path(a)));
        } else if a.len() < 2 || a[0] != b'-' {
            extras.push(path(a));
        } else {
            for (j, o) in a.iter().enumerate().skip(1) {
                if b"mtcras".contains(o) {
                    // A missing argument is getopt's error, which sxhkd ignores.
                    let arg = if j + 1 < a.len() {
                        Some(&a[j + 1..])
                    } else {
                        rest.next().copied()
                    };
                    if *o == b'c' {
                        main = arg.map(path).or(main);
                    }
                    break;
                }
            }
        }
    }
    // `src/sxhkd.c:117-124`: `XDG_CONFIG_HOME` counts when set at all, even empty (`getenv` only tested for NULL);
    // `CONFIG_PATH` is `sxhkd/sxhkdrc` (`src/sxhkd.h:37`).
    let main = main.unwrap_or_else(|| {
        let mut p = match env("XDG_CONFIG_HOME") {
            Some(home) => home,
            None => {
                // ponytail: an unset HOME is read as empty (glibc's printf would write `(null)`).
                let mut h = env("HOME").unwrap_or_default();
                h.push("/.config");
                h
            }
        };
        p.push("/sxhkd/sxhkdrc");
        PathBuf::from(p)
    });
    std::iter::once(main).chain(extras).collect()
}

/// C's `isgraph` in the C locale (sxhkd never calls `setlocale`): a printable non-space ASCII byte.
fn graph(b: &u8) -> bool {
    b.is_ascii_graphic()
}

/// The bindings of one config file, each with its category, and how many hotkeys were left out. A port of
/// `load_config` (`src/parse.c:2377-2422`); the comments it skips give descriptions and categories (see `comment`).
/// ponytail: sxhkd's fixed buffers (`MAXLEN`) aren't modelled: a line or hotkey longer than they hold reads whole here.
fn parse(text: &[u8]) -> (Vec<(String, Bind)>, usize) {
    // sxhkd's `chain` and `command`: C strings written at `offset`, the bytes past a NUL kept (`snprintf`).
    let (mut chain, mut command): (Vec<u8>, Vec<u8>) = (Vec::new(), Vec::new());
    let mut offset = 0;
    let (mut block, mut desc, mut title): (Vec<String>, String, Option<String>) =
        (Vec::new(), String::new(), None);
    let (mut binds, mut left_out) = (Vec::new(), 0);
    for line in text.split_inclusive(|b| *b == b'\n') {
        // `fgets` then `strlen`: the line ends at a NUL.
        let line = &line[..line.iter().position(|b| *b == 0).unwrap_or(line.len())];
        let first = line.first().copied().unwrap_or(0);
        let span = line
            .iter()
            .position(graph)
            .zip(line.iter().rposition(graph));
        // `START_COMMENT` only at column 0 (`src/parse.h:32`).
        if first == b'#' {
            block.push(comment(&line[1..]));
            continue;
        }
        // `strlen(buf) < 2`: skipped by sxhkd (a lone newline, or a line cut short by a NUL), blank here.
        let Some((start, end)) = span.filter(|_| line.len() >= 2) else {
            // A blank line: the comment block above it is a section title.
            let t = join(&block);
            if !t.is_empty() {
                title = Some(t);
            }
            block.clear();
            continue;
        };
        if graph(&first) {
            // A new hotkey's description; a continued line keeps it.
            if offset == 0 {
                desc = join(&block);
            }
            write_at(&mut chain, offset, &line[start..=end]);
        } else {
            write_at(&mut command, offset, &line[start..=end]);
        }
        block.clear();
        // `PARTIAL_LINE`: the next line overwrites the backslash.
        if line[end] == b'\\' {
            offset += end - start;
            continue;
        }
        offset = 0;
        // C's `isspace`: space, \t, \n, \v, \f, \r.
        if matches!(first, b' ' | b'\t'..=b'\r') && cstr(&chain) != b"" && cstr(&command) != b"" {
            let pairs = expand(cstr(&chain), cstr(&command));
            let several = pairs.len() > 1;
            for (hotkey, cmd) in pairs {
                let cmd = String::from_utf8_lossy(&cmd).into_owned();
                let Some(key) = key(&hotkey) else {
                    left_out += 1;
                    continue;
                };
                let category = title.clone().unwrap_or_else(|| {
                    let mut words = cmd.split_whitespace();
                    match (words.next(), words.next()) {
                        (Some("bspc"), Some(sub)) => sub.to_string(),
                        _ => "sxhkd".into(),
                    }
                });
                let desc = match (desc.is_empty(), several) {
                    (true, _) => cmd.clone(),
                    (false, false) => desc.clone(),
                    (false, true) => format!("{desc}: {cmd}"),
                };
                binds.push((
                    category,
                    Bind {
                        key,
                        desc,
                        action: cmd,
                        when: Default::default(),
                        run: Vec::new(),
                    },
                ));
            }
            // `chain[0] = '\0'`: the bytes after it stay, as in C.
            for buf in [&mut chain, &mut command] {
                if let Some(b) = buf.first_mut() {
                    *b = 0;
                }
            }
        }
    }
    (binds, left_out)
}

/// One comment line's text (after its `#`), trimmed.
fn comment(text: &[u8]) -> String {
    String::from_utf8_lossy(text).trim().to_string()
}

/// A comment block's non-empty lines joined with a space, one trailing `:` removed.
fn join(block: &[String]) -> String {
    let text = block
        .iter()
        .filter(|l| !l.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    text.strip_suffix(':').map(str::to_string).unwrap_or(text)
}

/// `snprintf(buf + offset, …, "%s", s)`: `s` and a NUL at `offset`, the bytes after them kept; a gap left by an
/// offset past the end is zeros, as sxhkd's zeroed buffers hold.
fn write_at(buf: &mut Vec<u8>, offset: usize, s: &[u8]) {
    if buf.len() < offset + s.len() + 1 {
        buf.resize(offset + s.len() + 1, 0);
    }
    buf[offset..offset + s.len()].copy_from_slice(s);
    buf[offset + s.len()] = 0;
}

/// A buffer as C reads it: up to its first NUL.
fn cstr(buf: &[u8]) -> &[u8] {
    &buf[..buf.iter().position(|b| *b == 0).unwrap_or(buf.len())]
}

/// `chunk_t` (`src/parse.h:43-51`): `advance` is an index into `text` (C's pointer; `None` for NULL), the range
/// bounds C's (signed) `char`s.
struct Chunk {
    text: Vec<u8>,
    item: Vec<u8>,
    advance: Option<usize>,
    sequence: bool,
    range_cur: i8,
    range_max: i8,
}

impl Chunk {
    /// `make_chunk` (`src/parse.c:2641-2650`).
    fn new() -> Chunk {
        Chunk {
            text: Vec::new(),
            item: Vec::new(),
            advance: None,
            sequence: false,
            range_cur: 1,
            range_max: 0,
        }
    }
}

/// The (hotkey, command) pairs one config entry expands to, in sxhkd's order: a port of `process_hotkey`'s loop
/// (`src/parse.c:2451-2511`). A side that is one plain chunk is fixed (`CHECKCHUNK`); the loop runs while each other
/// side still renders something. Identical consecutive hotkeys are each returned (sxhkd cycles their commands).
fn expand(hotkey: &[u8], command: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let fixed = |mut chunks: Vec<Chunk>| match chunks.as_slice() {
        [c] if !c.sequence => (None, chunks.remove(0).text),
        _ => (Some(chunks), Vec::new()),
    };
    let (mut hk, mut hotkey) = fixed(extract_chunks(hotkey));
    let (mut cm, mut command) = fixed(extract_chunks(command));
    let render = |chunks: &mut Option<Vec<Chunk>>, dest: &mut Vec<u8>| {
        if let Some(c) = chunks {
            *dest = render_next(c);
        }
    };
    render(&mut hk, &mut hotkey);
    render(&mut cm, &mut command);
    let mut pairs = Vec::new();
    while (hk.is_none() || !hotkey.is_empty()) && (cm.is_none() || !command.is_empty()) {
        pairs.push((hotkey.clone(), command.clone()));
        if hk.is_none() && cm.is_none() {
            break;
        }
        render(&mut hk, &mut hotkey);
        render(&mut cm, &mut command);
    }
    pairs
}

/// `get_token` (`src/parse.c:2513-2545`): the first token of `src` split on the bytes of `sep` (leading separators
/// skipped) and how many bytes were read (C's `ign`, the separators after it, is unused here). `\` escapes the next
/// byte; it is kept unless that byte is `\`, a separator, or the end.
fn get_token(src: &[u8], sep: &[u8]) -> (Vec<u8>, usize) {
    let mut dst = Vec::new();
    let (mut i, mut inhibit, mut found) = (0, false, false);
    while i < src.len() && !found {
        let b = src[i];
        if inhibit {
            dst.push(b);
            inhibit = false;
        } else if b == b'\\' {
            inhibit = true;
            // `strchr(sep, '\0')` finds the terminator: at the end the backslash is dropped too.
            match src.get(i + 1) {
                Some(n) if *n != b'\\' && !sep.contains(n) => dst.push(b),
                _ => {}
            }
        } else if sep.contains(&b) {
            found = !dst.is_empty();
            while i < src.len() && sep.contains(&src[i]) {
                i += 1;
            }
            i -= 1;
        } else {
            dst.push(b);
        }
        i += 1;
    }
    (dst, i)
}

/// `render_next` (`src/parse.c:2547-2592`): the next rendering of `chunks`, advancing the first sequence that can
/// advance and carrying into the next (the first sequence moves fastest). Empty once every sequence has wrapped.
fn render_next(chunks: &mut [Chunk]) -> Vec<u8> {
    let mut dest = Vec::new();
    let mut incr = false;
    for c in chunks {
        if !c.sequence {
            dest.extend_from_slice(&c.text);
            continue;
        }
        if !incr {
            if c.range_cur < c.range_max {
                c.range_cur += 1;
                incr = true;
            } else {
                (c.range_cur, c.range_max) = (1, 0);
            }
        }
        match c.advance {
            None => {
                incr = true;
                let (item, n) = get_token(&c.text, b",");
                (c.item, c.advance) = (item, Some(n));
            }
            Some(a) if !incr && c.range_cur > c.range_max => {
                if a >= c.text.len() {
                    // Wrapped: back to the first item, the carry left for the next sequence.
                    let (item, n) = get_token(&c.text, b",");
                    (c.item, c.advance) = (item, Some(n));
                } else {
                    let (item, n) = get_token(&c.text[a..], b",");
                    (c.item, c.advance) = (item, Some(a + n));
                    incr = true;
                }
            }
            Some(_) => {}
        }
        // `sscanf(item, "%c-%c", …)`: the first byte is stored even when the `-` doesn't match.
        if c.range_cur > c.range_max && c.item.len() == 3 {
            c.range_cur = c.item[0] as i8;
            if c.item[1] == b'-' {
                c.range_max = c.item[2] as i8;
            }
        }
        if c.range_cur <= c.range_max {
            dest.push(c.range_cur as u8);
        } else if c.item != b"_" {
            // `SEQ_NONE` alone renders nothing.
            dest.extend_from_slice(&c.item);
        }
    }
    if !incr {
        dest.clear();
    }
    // A C string: a 0 rendered ends it.
    dest.truncate(cstr(&dest).len());
    dest
}

/// `extract_chunks` (`src/parse.c:2594-2639`): `s` cut into plain text and `{…}` sequences. A `\` before `{` or `}`
/// is dropped (the brace is plain text), one before `\` outside a sequence too; any other is kept for `get_token`.
fn extract_chunks(s: &[u8]) -> Vec<Chunk> {
    let mut chunks = vec![Chunk::new()];
    let mut inhibit = false;
    for (i, &b) in s.iter().enumerate() {
        let next = s.get(i + 1).copied().unwrap_or(0);
        let Some(c) = chunks.last_mut() else { break };
        if inhibit {
            c.text.push(b);
            inhibit = false;
        } else if b == b'\\' {
            inhibit = true;
            if (next != b'\\' || c.sequence) && next != b'{' && next != b'}' {
                c.text.push(b);
            }
        } else if b == b'{' || b == b'}' {
            if !c.text.is_empty() {
                chunks.push(Chunk::new());
            }
            if let Some(c) = chunks.last_mut() {
                c.sequence = b == b'{';
            }
        } else {
            c.text.push(b);
        }
    }
    chunks
}

/// One expanded hotkey in the key notation, as `parse_chain` (`src/parse.c:2662-2712`) splits it: chords on `;` or
/// `:` (`LNK_SEP`; a `:` link locks the chain, still the same sequence), names on `+` and space (`SYM_SEP`), a
/// leading `~` (replay) then `@` (release) stripped. `None`: left out (a modifier xdotool can't press, a pointer
/// button, an upper-case letter, a key name sxhkd doesn't know).
fn key(hotkey: &[u8]) -> Option<String> {
    let mut chords = Vec::new();
    let mut rest = hotkey;
    loop {
        let (chord_text, n) = get_token(rest, b";:");
        rest = &rest[n..];
        if chord_text.is_empty() {
            break;
        }
        let (mut mods, mut keysym) = (Vec::new(), None);
        let mut names = chord_text.as_slice();
        loop {
            let (name, n) = get_token(names, b"+ ");
            names = &names[n..];
            if name.is_empty() {
                break;
            }
            let name = name.strip_prefix(b"~").unwrap_or(&name);
            let name = name.strip_prefix(b"@").unwrap_or(name);
            // `parse_modifier` (`src/parse.c:2732-2778`), then a keysym, then `button%hhu` (`parse_button`).
            match name {
                b"super" | b"mod4" => mods.push("super"),
                b"alt" | b"mod1" => mods.push("alt"),
                b"ctrl" | b"control" => mods.push("ctrl"),
                b"shift" => mods.push("shift"),
                b"hyper" | b"meta" | b"mode_switch" | b"lock" | b"mod2" | b"mod3" | b"mod5"
                | b"any" => return None,
                _ if name
                    .strip_prefix(b"button")
                    .is_some_and(|d| d.first().is_some_and(u8::is_ascii_digit)) =>
                {
                    return None
                }
                // Each name must be one of sxhkd's key names, matched case-sensitively (`parse_keysym`,
                // `src/parse.c:2714-2724`: an unknown one drops the hotkey, 2688-2691), and a keysym in the notation;
                // the last wins, as `parse_keysym` overwrites it. The notation refuses an upper-case letter, so it
                // is left out.
                // ponytail: sxhkd binds `A` as shift + the key holding it (`make_chord`, `src/types.c:135-175`); list
                // it as `shift+a` if that's wanted (the layout decides which key that is).
                _ => {
                    let name = std::str::from_utf8(name).ok()?;
                    KEY_NAMES.binary_search(&name).ok()?;
                    name.parse::<keybind_find_sheet::Keysym>().ok()?;
                    keysym = Some(name.to_string());
                }
            }
        }
        chords.push(chord(&mods, &keysym?));
    }
    (!chords.is_empty()).then(|| chords.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    fn pairs(hotkey: &str, command: &str) -> Vec<(String, String)> {
        expand(hotkey.as_bytes(), command.as_bytes())
            .into_iter()
            .map(|(h, c)| (s(&h), s(&c)))
            .collect()
    }

    fn p(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(h, c)| (h.to_string(), c.to_string()))
            .collect()
    }

    /// `(category, key, desc, action)` of each bind of `text`, and the left-out count.
    fn rows(text: &str) -> (Vec<(String, String, String, String)>, usize) {
        let (binds, n) = parse(text.as_bytes());
        let rows = binds
            .into_iter()
            .map(|(c, b)| (c, b.key, b.desc, b.action))
            .collect();
        (rows, n)
    }

    fn row(c: &str, k: &str, d: &str, a: &str) -> (String, String, String, String) {
        (c.into(), k.into(), d.into(), a.into())
    }

    #[test]
    fn expansion_table() {
        // A hotkey sequence, no command sequence: the command repeats.
        assert_eq!(
            pairs("super + {a,b}", "cmd"),
            p(&[("super + a", "cmd"), ("super + b", "cmd")])
        );
        // A fixed hotkey with a command sequence: the same key twice, x then y (sxhkd cycles them).
        assert_eq!(
            pairs("super + a", "echo {x,y}"),
            p(&[("super + a", "echo x"), ("super + a", "echo y")])
        );
        // Unequal lengths stop at the shorter side.
        assert_eq!(pairs("{a,b,c}", "{x,y}"), p(&[("a", "x"), ("b", "y")]));
        assert_eq!(pairs("{a,b}", "{x,y,z}"), p(&[("a", "x"), ("b", "y")]));
        // `_` alone is empty.
        assert_eq!(
            pairs("super + {_,shift + }w", "bspc node -{c,k}"),
            p(&[
                ("super + w", "bspc node -c"),
                ("super + shift + w", "bspc node -k")
            ])
        );
        // A range.
        assert_eq!(
            pairs("{a-c}", "x"),
            p(&[("a", "x"), ("b", "x"), ("c", "x")])
        );
        // A range inside a list, the first sequence fastest.
        let all = pairs(
            "super + {_,shift + }{1-9,0}",
            "bspc {desktop -f,node -d} '^{1-9,10}'",
        );
        assert_eq!(all.len(), 20);
        assert_eq!(all[0], p(&[("super + 1", "bspc desktop -f '^1'")])[0]);
        assert_eq!(all[1], p(&[("super + shift + 1", "bspc node -d '^1'")])[0]);
        assert_eq!(all[2], p(&[("super + 2", "bspc desktop -f '^2'")])[0]);
        assert_eq!(all[18], p(&[("super + 0", "bspc desktop -f '^10'")])[0]);
        assert_eq!(
            all[19],
            p(&[("super + shift + 0", "bspc node -d '^10'")])[0]
        );
        // Escapes: `\{` is a plain brace; `\,` inside a sequence is a comma in an item.
        assert_eq!(pairs(r"a", r"echo \{x\}"), p(&[("a", "echo {x}")]));
        assert_eq!(
            pairs("{a,b}", r"echo {1\,2,3}"),
            p(&[("a", "echo 1,2"), ("b", "echo 3")])
        );
        // `\\` inside a sequence is one backslash.
        assert_eq!(pairs("{a}", r"echo {\\}"), p(&[("a", r"echo \")]));
        // Nested-looking text: the second `{` starts a new sequence, the first never closed.
        assert_eq!(pairs("{a{b}", "x"), p(&[("ab", "x")]));
        assert_eq!(pairs("k", "{a{b,c}"), p(&[("k", "ab"), ("k", "ac")]));
        // An item of three characters without a dash is itself.
        assert_eq!(pairs("{abc,d}", "x"), p(&[("abc", "x"), ("d", "x")]));
        // `{}` is no sequence: the hotkey is the empty text, once.
        assert_eq!(pairs("{}", "x"), p(&[("", "x")]));
        // C's `sscanf("%c-%c")` stores the first byte even without the dash, as a signed char: a three-byte item
        // starting with a byte over 0x7f (`€`, E2 82 AC) reads as a range from -30 up to the 0 that ends the string,
        // 30 renderings, as sxhkd 0.6.3 binds them.
        assert_eq!(expand(b"k", "{\u{20ac}}".as_bytes()).len(), 30);
    }

    #[test]
    fn lines() {
        // `#` only at column 0: an indented one under a hotkey is its command.
        assert_eq!(rows("a\n  # x\n").0, [row("sxhkd", "a", "# x", "# x")]);
        // A blank-line-separated block is a title; one right above a hotkey its description.
        assert_eq!(
            rows("# Title\n\n# desc\nsuper + a\n\tbspc node -c\nsuper + b\n\tbspc wm -r\n").0,
            [
                row("Title", "super+a", "desc", "bspc node -c"),
                row("Title", "super+b", "bspc wm -r", "bspc wm -r"),
            ]
        );
        // With no title: the bspc subcommand, else sxhkd.
        assert_eq!(
            rows("super + a\n\tbspc node -c\nsuper + b\n\turxvt\n").0,
            [
                row("node", "super+a", "bspc node -c", "bspc node -c"),
                row("sxhkd", "super+b", "urxvt", "urxvt"),
            ]
        );
        // A continued hotkey line, each part sorted by its own first byte.
        assert_eq!(
            rows("super + \\\nshift + a\n\techo hi\n").0,
            [row("sxhkd", "super+shift+a", "echo hi", "echo hi")]
        );
        // Two hotkey lines in a row: the second wins; a trailing hotkey with no command is dropped.
        assert_eq!(
            rows("a\nb\n\techo b\nc\n").0,
            [row("sxhkd", "b", "echo b", "echo b")]
        );
        // CRLF: `\r` is trailing whitespace.
        assert_eq!(
            rows("# d\r\nsuper + a\r\n\techo a\r\n").0,
            [row("sxhkd", "super+a", "d", "echo a")]
        );
        // A continued hotkey keeps its description.
        assert_eq!(
            rows("# d\nsuper + \\\na\n\techo\n").0,
            [row("sxhkd", "super+a", "d", "echo")]
        );
        // Only a line starting with whitespace completes an entry: one starting with another byte (not `isgraph`, not
        // `isspace`) is written to the command and waits.
        assert_eq!(
            rows("a\n\u{e9}x\n\tcmd\n").0,
            [row("sxhkd", "a", "cmd", "cmd")]
        );
        // A processed entry empties its hotkey: a second command line alone is nothing.
        assert_eq!(rows("a\n\tx\n\ty\n").0, [row("sxhkd", "a", "x", "x")]);
        // An empty hotkey (`{}` renders nothing) is left out, counted.
        assert_eq!(rows("{}\n\tx\n"), (vec![], 1));
        // A NUL ends a line (`strlen`): `a` alone is under 2 bytes, skipped, so the command has no hotkey.
        assert_eq!(rows("a\0junk\n\tcmd\n"), (vec![], 0));
        // One trailing `:` is removed, not two.
        assert_eq!(rows("# a::\nk\n\tx\n").0, [row("sxhkd", "k", "a:", "x")]);
        // "Several" counts every expansion, left-out ones too: the one listed shows its command.
        assert_eq!(
            rows("# d\nsuper + {A,b}\n\tx\n"),
            (vec![row("sxhkd", "super+b", "d: x", "x")], 1)
        );
    }

    #[test]
    fn keys() {
        let k = |h: &str| key(h.as_bytes());
        assert_eq!(
            rows("super + alt + {q,r}\n\tbspc {quit,wm -r}\n").0,
            [
                row("quit", "super+alt+q", "bspc quit", "bspc quit"),
                row("wm", "super+alt+r", "bspc wm -r", "bspc wm -r"),
            ]
        );
        assert_eq!(k("ctrl + a ; c").as_deref(), Some("ctrl+a c"));
        assert_eq!(k("super + w : h").as_deref(), Some("super+w h"));
        assert_eq!(
            k("mod4 + mod1 + Return").as_deref(),
            Some("super+alt+Return")
        );
        assert_eq!(k("control + ~@space").as_deref(), Some("ctrl+space"));
        // The last keysym named wins.
        assert_eq!(k("a + b").as_deref(), Some("b"));
        // sxhkd's key names are case-sensitive: `return` is none (sxhkd drops the hotkey).
        assert_eq!(k("super + Return").as_deref(), Some("super+Return"));
        assert_eq!(k("super + return"), None);
        assert_eq!(k("esc"), None);
        assert_eq!(k("Retrun"), None);
        // `lock` and `any` aren't the notation's modifiers: without the rule they would read as keysyms.
        for left_out in [
            "~button1",
            "hyper + x",
            "super + A",
            "mode_switch + a",
            "lock + a",
            "any + a",
            "Super + a",
            "super",
        ] {
            assert_eq!(k(left_out), None, "{left_out}");
        }
        // Counted, not listed.
        let (rows, n) = rows("~button1\n\tx\nhyper + x\n\tx\nsuper + {A,b}\n\tx\n");
        assert_eq!((rows.len(), n), (1, 3));
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/sxhkd")
                .join(name),
        )
        .unwrap()
    }

    /// `bspwm-sxhkdrc`: bspwm 0.9.12's `examples/sxhkdrc` under a header block.
    #[test]
    fn bspwm_example() {
        let (binds, left_out) = parse(&fixture("bspwm-sxhkdrc"));
        assert_eq!((binds.len(), left_out), (BSPWM_BINDS, 0));
        let find = |k: &str| {
            binds
                .iter()
                .find(|(_, b)| b.key == k)
                .map(|(c, b)| (c.as_str(), b.desc.as_str(), b.action.as_str()))
                .unwrap_or_else(|| panic!("{k}"))
        };
        assert_eq!(
            find("super+Return"),
            ("wm independent hotkeys", "terminal emulator", "urxvt")
        );
        assert_eq!(find("super+space").2, "dmenu_run");
        assert_eq!(
            find("super+Escape").1,
            "make sxhkd reload its configuration files"
        );
        assert_eq!(find("super+shift+w").1, "close and kill: bspc node -k");
        assert_eq!(find("super+shift+w").0, "bspwm hotkeys");
        assert_eq!(find("super+1").2, "bspc desktop -f '^1'");
        assert_eq!(find("super+shift+0").2, "bspc node -d '^10'");
        assert_eq!(find("super+bracketleft").2, "bspc desktop -f prev.local");
        assert_eq!(
            find("super+o").2,
            "bspc wm -h off; bspc node older -f; bspc wm -h on"
        );
        assert_eq!(
            find("super+ctrl+1"),
            (
                "preselect",
                "preselect the ratio: bspc node -o 0.1",
                "bspc node -o 0.1"
            )
        );
        assert_eq!(find("super+ctrl+9").2, "bspc node -o 0.9");
        assert_eq!(find("super+Right").2, "bspc node -v 20 0");
    }

    const BSPWM_BINDS: usize = 85;

    #[test]
    fn key_names_are_sorted() {
        // `binary_search` needs them sorted, each once: 2072 names in the groups `locales.h` enables.
        assert!(KEY_NAMES.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(KEY_NAMES.len(), 2072);
    }

    /// A fake process: pid, cmdline arguments, environ (`None`: unreadable), cwd.
    type Proc<'a> = (u32, &'a [&'a str], Option<&'a [&'a str]>, &'a str);

    /// A fake `/proc` of `procs`, each an `sxhkd` owned by this user.
    fn fake_proc(name: &str, procs: &[Proc]) -> PathBuf {
        let dir = crate::test_tmp().join(format!("kbf-sxhkd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (pid, args, environ, cwd) in procs {
            let d = dir.join(pid.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("comm"), "sxhkd\n").unwrap();
            let cmdline: String = std::iter::once("sxhkd")
                .chain(args.iter().copied())
                .map(|a| format!("{a}\0"))
                .collect();
            std::fs::write(d.join("cmdline"), cmdline).unwrap();
            if let Some(env) = environ {
                let e: String = env.iter().map(|v| format!("{v}\0")).collect();
                std::fs::write(d.join("environ"), e).unwrap();
            }
            std::os::unix::fs::symlink(cwd, d.join("cwd")).unwrap();
        }
        dir
    }

    fn uid(dir: &Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir).unwrap().uid()
    }

    fn own_env(k: &str) -> Option<OsString> {
        match k {
            "HOME" => Some("/own".into()),
            _ => None,
        }
    }

    fn paths(files: Vec<PathBuf>) -> Vec<String> {
        files.into_iter().map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn config_paths() {
        let dir = fake_proc(
            "paths",
            &[
                (10, &[], Some(&["XDG_CONFIG_HOME=/xdg", "HOME=/h"]), "/w"),
                (11, &[], Some(&["XDG_CONFIG_HOME=", "HOME=/h"]), "/w"),
                (12, &[], Some(&["HOME=/h"]), "/w"),
                (13, &["-c", "rel/path"], Some(&[]), "/w"),
                // The last `-c` wins.
                (14, &["-cnot", "-cfile"], Some(&[]), "/w"),
                (
                    15,
                    &["-m", "1", "-c", "x", "extra1", "extra2"],
                    Some(&[]),
                    "/w",
                ),
                // The same list as 12: read once.
                (16, &[], Some(&["HOME=/h"]), "/w"),
                // No environ: keybind-find's own.
                (17, &[], None, "/w"),
                // glibc's getopt permutes: an extra before the options; `--` ends them.
                (18, &["e", "-tc", "-c", "y", "--", "-c"], Some(&[]), "/w"),
                // `-r -s -a` take an argument; a lone `-` is no option; a `-c` with none is ignored.
                (
                    19,
                    &["-r", "x", "-s", "y", "-a", "z", "-", "-c"],
                    Some(&["HOME=/h2"]),
                    "/w",
                ),
                // A zombie: comm still sxhkd, its cmdline (set below) empty.
                (20, &[], Some(&["HOME=/zombie"]), "/w"),
                // A file another sxhkd loads too is read once.
                (21, &["-c", "/s/a", "/s/b"], Some(&[]), "/w"),
                (22, &["-c", "/s/b", "/s/c"], Some(&[]), "/w"),
            ],
        );
        std::fs::write(dir.join("20/cmdline"), "").unwrap();
        let files = paths(config_files(&dir, uid(&dir), &own_env));
        let running = running(&dir, uid(&dir));
        let theirs = config_files(&dir, uid(&dir).wrapping_add(1), &own_env);
        // Relative paths through each process's own `cwd` link.
        let cwd = |pid: u32, p: &str| format!("{}/{pid}/cwd/{p}", dir.display());
        let want = [
            "/xdg/sxhkd/sxhkdrc".into(),
            "/sxhkd/sxhkdrc".into(),
            "/h/.config/sxhkd/sxhkdrc".into(),
            cwd(13, "rel/path"),
            cwd(14, "file"),
            cwd(15, "x"),
            cwd(15, "extra1"),
            cwd(15, "extra2"),
            "/own/.config/sxhkd/sxhkdrc".into(),
            cwd(18, "y"),
            cwd(18, "e"),
            cwd(18, "-c"),
            "/h2/.config/sxhkd/sxhkdrc".into(),
            cwd(19, "-"),
            "/s/a".into(),
            "/s/b".into(),
            "/s/c".into(),
        ];
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(files, want);
        assert!(!running.iter().any(|(pid, _)| *pid == 20));
        // Another user's sxhkd is ignored.
        assert!(theirs.is_empty());
    }

    #[test]
    fn reads_every_file_and_notes_the_rest() {
        let files = crate::test_tmp().join(format!("kbf-sxhkd-files-{}", std::process::id()));
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("main"), "super + a\n\techo a\n").unwrap();
        std::fs::write(files.join("extra"), "super + b\n\techo b\nhyper + c\n\tx\n").unwrap();
        let cwd = files.display().to_string();
        let dir = fake_proc(
            "read",
            &[(20, &["-c", "main", "missing", "extra"], Some(&[]), &cwd)],
        );
        let (sheet, notes) = read(&dir, Some(uid(&dir)), &own_env).unwrap();
        let keys: Vec<&str> = sheet.binds["sxhkd"]
            .iter()
            .map(|b| b.key.as_str())
            .collect();
        assert_eq!(keys, ["super+a", "super+b"]);
        assert_eq!(notes.len(), 2, "{notes:?}");
        let missing = format!("can't read {}/20/cwd/missing", dir.display());
        assert!(notes[0].starts_with(&missing), "{notes:?}");
        assert!(notes[1].starts_with("leaving out 1 hotkey(s)"), "{notes:?}");
        // No sxhkd of this user: the default path from keybind-find's own environment, said so.
        let env = |k: &str| (k == "XDG_CONFIG_HOME").then(|| OsString::from(&cwd));
        std::fs::create_dir_all(files.join("sxhkd")).unwrap();
        std::fs::write(files.join("sxhkd/sxhkdrc"), "super + c\n\techo c\n").unwrap();
        let (sheet, notes) = read(&dir, Some(uid(&dir).wrapping_add(1)), &env).unwrap();
        assert_eq!(sheet.binds["sxhkd"][0].key, "super+c");
        assert_eq!(
            notes,
            [format!("no sxhkd running; reading {cwd}/sxhkd/sxhkdrc")]
        );
        // Nothing readable at all: an error naming the file.
        let err = read(&dir, None, &own_env).unwrap_err().to_string();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&files);
        assert!(
            err.contains("can't read /own/.config/sxhkd/sxhkdrc"),
            "{err}"
        );
    }
}
