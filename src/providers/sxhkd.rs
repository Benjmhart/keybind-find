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
        files = configs(&[], env)
            .into_iter()
            .map(|p| (p.clone(), p))
            .collect();
        if let Some((path, _)) = files.first() {
            notes.push(format!("no sxhkd running; reading {}", path.display()));
        }
    }
    let mut sheet = Sheet {
        title: "sxhkd".into(),
        provider: "sxhkd".into(),
        binds: Default::default(),
    };
    let (mut read_any, mut left_out) = (false, 0);
    // ponytail: a hotkey in two files (or two sxhkd) is listed twice, though only the first grab fires; drop later
    // repeats of a key if that confuses.
    for (path, shown) in &files {
        match std::fs::read(path) {
            Ok(text) => {
                read_any = true;
                let (binds, n) = parse(&text);
                left_out += n;
                for (category, bind) in binds {
                    sheet.binds.entry(category).or_default().push(bind);
                }
            }
            Err(e) => notes.push(format!("can't read {}: {e}", shown.display())),
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
/// empty: skipped. So is a pid whose `comm` is no longer `sxhkd` once `cmdline` is read (the pid was reused), and a
/// script named `sxhkd` run by a shell: the program (`exe`'s file name, else argv[0]'s) must be `sxhkd` too.
/// ponytail: the second `comm` read narrows pid reuse without closing it, and the uid isn't read again; compare the
/// process's start time (`stat` field 22) before and after if that bites.
pub(crate) fn running(proc: &Path, uid: u32) -> Vec<(u32, Vec<u8>)> {
    user_pids(proc, uid, "sxhkd")
        .into_iter()
        .filter_map(|pid| {
            let dir = proc.join(pid.to_string());
            let cmdline = std::fs::read(dir.join("cmdline"))
                .ok()
                .filter(|c| !c.is_empty())?;
            // A binary replaced by an upgrade reads as `/usr/bin/sxhkd (deleted)`.
            let program = match std::fs::read_link(dir.join("exe")) {
                Ok(exe) => exe.as_os_str().as_bytes().to_vec(),
                Err(_) => cmdline
                    .split(|b| *b == 0)
                    .next()
                    .unwrap_or_default()
                    .to_vec(),
            };
            let program = program.strip_suffix(b" (deleted)").unwrap_or(&program);
            let named = program.rsplit(|b| *b == b'/').next() == Some(b"sxhkd".as_slice());
            let comm = std::fs::read_to_string(dir.join("comm")).ok()?;
            (named && comm.trim_end_matches('\n') == "sxhkd").then_some((pid, cmdline))
        })
        .collect()
}

/// The config files of `uid`'s `sxhkd` processes in `proc`, each in the order its process loads them, as (the path to
/// open, the path to show); a file an earlier process (or the same one) already has, by device and inode, is read
/// once. The process's own environment and cwd decide its paths: a process whose `environ` can't be read gets `env`.
fn config_files(proc: &Path, uid: u32, env: Env) -> Vec<(PathBuf, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    let mut files = Vec::new();
    // `(dev, ino)`, or the path when it can't be read (its error is noted when read).
    let mut seen: Vec<Result<(u64, u64), PathBuf>> = Vec::new();
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
        // Opened through the link itself, not its target: a deleted cwd or another mount namespace still resolves.
        // Shown through its target when it reads.
        let cwd = dir.join("cwd");
        let shown_cwd = std::fs::read_link(&cwd).unwrap_or_else(|_| cwd.clone());
        // argv[0] is the program; a trailing NUL ends the last argument.
        let args: Vec<&[u8]> = cmdline
            .strip_suffix(&[0u8])
            .unwrap_or(&cmdline)
            .split(|b| *b == 0)
            .skip(1)
            .collect();
        for rel in configs(&args, &own) {
            let path = cwd.join(&rel);
            let id = std::fs::metadata(&path)
                .map(|m| (m.dev(), m.ino()))
                .map_err(|_| path.clone());
            if !seen.contains(&id) {
                seen.push(id);
                files.push((path, shown_cwd.join(rel)));
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
                // ponytail: a keysym on no key of the current layout is listed, though sxhkd drops it (`make_chord`,
                // `src/types.c:141-177`); matching that needs the X keymap.
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

/// sxhkd 0.6.3's key names (`nks_dict`, `src/parse.c:38-2375` at tag 0.6.3, commit 2cc7aca), the groups
/// `src/locales.h` enables (not XK_3270, XK_TECHNICAL, XK_SPECIAL, XK_PUBLISHING, XK_APL), sorted for
/// `binary_search`. sxhkd is BSD-2-Clause, Copyright (c) 2013, Bastien Dejean. Generated from that file.
const KEY_NAMES: &[&str] = &[
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "A",
    "AE",
    "Aacute",
    "Abelowdot",
    "Abreve",
    "Abreveacute",
    "Abrevebelowdot",
    "Abrevegrave",
    "Abrevehook",
    "Abrevetilde",
    "AccessX_Enable",
    "AccessX_Feedback_Enable",
    "Acircumflex",
    "Acircumflexacute",
    "Acircumflexbelowdot",
    "Acircumflexgrave",
    "Acircumflexhook",
    "Acircumflextilde",
    "Adiaeresis",
    "Agrave",
    "Ahook",
    "Alt_L",
    "Alt_R",
    "Amacron",
    "Aogonek",
    "Arabic_0",
    "Arabic_1",
    "Arabic_2",
    "Arabic_3",
    "Arabic_4",
    "Arabic_5",
    "Arabic_6",
    "Arabic_7",
    "Arabic_8",
    "Arabic_9",
    "Arabic_ain",
    "Arabic_alef",
    "Arabic_alefmaksura",
    "Arabic_beh",
    "Arabic_comma",
    "Arabic_dad",
    "Arabic_dal",
    "Arabic_damma",
    "Arabic_dammatan",
    "Arabic_ddal",
    "Arabic_farsi_yeh",
    "Arabic_fatha",
    "Arabic_fathatan",
    "Arabic_feh",
    "Arabic_fullstop",
    "Arabic_gaf",
    "Arabic_ghain",
    "Arabic_ha",
    "Arabic_hah",
    "Arabic_hamza",
    "Arabic_hamza_above",
    "Arabic_hamza_below",
    "Arabic_hamzaonalef",
    "Arabic_hamzaonwaw",
    "Arabic_hamzaonyeh",
    "Arabic_hamzaunderalef",
    "Arabic_heh",
    "Arabic_heh_doachashmee",
    "Arabic_heh_goal",
    "Arabic_jeem",
    "Arabic_jeh",
    "Arabic_kaf",
    "Arabic_kasra",
    "Arabic_kasratan",
    "Arabic_keheh",
    "Arabic_khah",
    "Arabic_lam",
    "Arabic_madda_above",
    "Arabic_maddaonalef",
    "Arabic_meem",
    "Arabic_noon",
    "Arabic_noon_ghunna",
    "Arabic_peh",
    "Arabic_percent",
    "Arabic_qaf",
    "Arabic_question_mark",
    "Arabic_ra",
    "Arabic_rreh",
    "Arabic_sad",
    "Arabic_seen",
    "Arabic_semicolon",
    "Arabic_shadda",
    "Arabic_sheen",
    "Arabic_sukun",
    "Arabic_superscript_alef",
    "Arabic_switch",
    "Arabic_tah",
    "Arabic_tatweel",
    "Arabic_tcheh",
    "Arabic_teh",
    "Arabic_tehmarbuta",
    "Arabic_thal",
    "Arabic_theh",
    "Arabic_tteh",
    "Arabic_veh",
    "Arabic_waw",
    "Arabic_yeh",
    "Arabic_yeh_baree",
    "Arabic_zah",
    "Arabic_zain",
    "Aring",
    "Armenian_AT",
    "Armenian_AYB",
    "Armenian_BEN",
    "Armenian_CHA",
    "Armenian_DA",
    "Armenian_DZA",
    "Armenian_E",
    "Armenian_FE",
    "Armenian_GHAT",
    "Armenian_GIM",
    "Armenian_HI",
    "Armenian_HO",
    "Armenian_INI",
    "Armenian_JE",
    "Armenian_KE",
    "Armenian_KEN",
    "Armenian_KHE",
    "Armenian_LYUN",
    "Armenian_MEN",
    "Armenian_NU",
    "Armenian_O",
    "Armenian_PE",
    "Armenian_PYUR",
    "Armenian_RA",
    "Armenian_RE",
    "Armenian_SE",
    "Armenian_SHA",
    "Armenian_TCHE",
    "Armenian_TO",
    "Armenian_TSA",
    "Armenian_TSO",
    "Armenian_TYUN",
    "Armenian_VEV",
    "Armenian_VO",
    "Armenian_VYUN",
    "Armenian_YECH",
    "Armenian_ZA",
    "Armenian_ZHE",
    "Armenian_accent",
    "Armenian_amanak",
    "Armenian_apostrophe",
    "Armenian_at",
    "Armenian_ayb",
    "Armenian_ben",
    "Armenian_but",
    "Armenian_cha",
    "Armenian_da",
    "Armenian_dza",
    "Armenian_e",
    "Armenian_exclam",
    "Armenian_fe",
    "Armenian_full_stop",
    "Armenian_ghat",
    "Armenian_gim",
    "Armenian_hi",
    "Armenian_ho",
    "Armenian_hyphen",
    "Armenian_ini",
    "Armenian_je",
    "Armenian_ke",
    "Armenian_ken",
    "Armenian_khe",
    "Armenian_ligature_ew",
    "Armenian_lyun",
    "Armenian_men",
    "Armenian_nu",
    "Armenian_o",
    "Armenian_paruyk",
    "Armenian_pe",
    "Armenian_pyur",
    "Armenian_question",
    "Armenian_ra",
    "Armenian_re",
    "Armenian_se",
    "Armenian_separation_mark",
    "Armenian_sha",
    "Armenian_shesht",
    "Armenian_tche",
    "Armenian_to",
    "Armenian_tsa",
    "Armenian_tso",
    "Armenian_tyun",
    "Armenian_verjaket",
    "Armenian_vev",
    "Armenian_vo",
    "Armenian_vyun",
    "Armenian_yech",
    "Armenian_yentamna",
    "Armenian_za",
    "Armenian_zhe",
    "Atilde",
    "AudibleBell_Enable",
    "B",
    "Babovedot",
    "BackSpace",
    "Begin",
    "BounceKeys_Enable",
    "Break",
    "Byelorussian_SHORTU",
    "Byelorussian_shortu",
    "C",
    "CH",
    "C_H",
    "C_h",
    "Cabovedot",
    "Cacute",
    "Cancel",
    "Caps_Lock",
    "Ccaron",
    "Ccedilla",
    "Ccircumflex",
    "Ch",
    "Clear",
    "Codeinput",
    "ColonSign",
    "Control_L",
    "Control_R",
    "CruzeiroSign",
    "Cyrillic_A",
    "Cyrillic_BE",
    "Cyrillic_CHE",
    "Cyrillic_CHE_descender",
    "Cyrillic_CHE_vertstroke",
    "Cyrillic_DE",
    "Cyrillic_DZHE",
    "Cyrillic_E",
    "Cyrillic_EF",
    "Cyrillic_EL",
    "Cyrillic_EM",
    "Cyrillic_EN",
    "Cyrillic_EN_descender",
    "Cyrillic_ER",
    "Cyrillic_ES",
    "Cyrillic_GHE",
    "Cyrillic_GHE_bar",
    "Cyrillic_HA",
    "Cyrillic_HARDSIGN",
    "Cyrillic_HA_descender",
    "Cyrillic_I",
    "Cyrillic_IE",
    "Cyrillic_IO",
    "Cyrillic_I_macron",
    "Cyrillic_JE",
    "Cyrillic_KA",
    "Cyrillic_KA_descender",
    "Cyrillic_KA_vertstroke",
    "Cyrillic_LJE",
    "Cyrillic_NJE",
    "Cyrillic_O",
    "Cyrillic_O_bar",
    "Cyrillic_PE",
    "Cyrillic_SCHWA",
    "Cyrillic_SHA",
    "Cyrillic_SHCHA",
    "Cyrillic_SHHA",
    "Cyrillic_SHORTI",
    "Cyrillic_SOFTSIGN",
    "Cyrillic_TE",
    "Cyrillic_TSE",
    "Cyrillic_U",
    "Cyrillic_U_macron",
    "Cyrillic_U_straight",
    "Cyrillic_U_straight_bar",
    "Cyrillic_VE",
    "Cyrillic_YA",
    "Cyrillic_YERU",
    "Cyrillic_YU",
    "Cyrillic_ZE",
    "Cyrillic_ZHE",
    "Cyrillic_ZHE_descender",
    "Cyrillic_a",
    "Cyrillic_be",
    "Cyrillic_che",
    "Cyrillic_che_descender",
    "Cyrillic_che_vertstroke",
    "Cyrillic_de",
    "Cyrillic_dzhe",
    "Cyrillic_e",
    "Cyrillic_ef",
    "Cyrillic_el",
    "Cyrillic_em",
    "Cyrillic_en",
    "Cyrillic_en_descender",
    "Cyrillic_er",
    "Cyrillic_es",
    "Cyrillic_ghe",
    "Cyrillic_ghe_bar",
    "Cyrillic_ha",
    "Cyrillic_ha_descender",
    "Cyrillic_hardsign",
    "Cyrillic_i",
    "Cyrillic_i_macron",
    "Cyrillic_ie",
    "Cyrillic_io",
    "Cyrillic_je",
    "Cyrillic_ka",
    "Cyrillic_ka_descender",
    "Cyrillic_ka_vertstroke",
    "Cyrillic_lje",
    "Cyrillic_nje",
    "Cyrillic_o",
    "Cyrillic_o_bar",
    "Cyrillic_pe",
    "Cyrillic_schwa",
    "Cyrillic_sha",
    "Cyrillic_shcha",
    "Cyrillic_shha",
    "Cyrillic_shorti",
    "Cyrillic_softsign",
    "Cyrillic_te",
    "Cyrillic_tse",
    "Cyrillic_u",
    "Cyrillic_u_macron",
    "Cyrillic_u_straight",
    "Cyrillic_u_straight_bar",
    "Cyrillic_ve",
    "Cyrillic_ya",
    "Cyrillic_yeru",
    "Cyrillic_yu",
    "Cyrillic_ze",
    "Cyrillic_zhe",
    "Cyrillic_zhe_descender",
    "D",
    "Dabovedot",
    "Dcaron",
    "Delete",
    "DongSign",
    "Down",
    "Dstroke",
    "E",
    "ENG",
    "ETH",
    "EZH",
    "Eabovedot",
    "Eacute",
    "Ebelowdot",
    "Ecaron",
    "Ecircumflex",
    "Ecircumflexacute",
    "Ecircumflexbelowdot",
    "Ecircumflexgrave",
    "Ecircumflexhook",
    "Ecircumflextilde",
    "EcuSign",
    "Ediaeresis",
    "Egrave",
    "Ehook",
    "Eisu_Shift",
    "Eisu_toggle",
    "Emacron",
    "End",
    "Eogonek",
    "Escape",
    "Eth",
    "Etilde",
    "EuroSign",
    "Execute",
    "F",
    "F1",
    "F10",
    "F11",
    "F12",
    "F13",
    "F14",
    "F15",
    "F16",
    "F17",
    "F18",
    "F19",
    "F2",
    "F20",
    "F21",
    "F22",
    "F23",
    "F24",
    "F25",
    "F26",
    "F27",
    "F28",
    "F29",
    "F3",
    "F30",
    "F31",
    "F32",
    "F33",
    "F34",
    "F35",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "FFrancSign",
    "Fabovedot",
    "Farsi_0",
    "Farsi_1",
    "Farsi_2",
    "Farsi_3",
    "Farsi_4",
    "Farsi_5",
    "Farsi_6",
    "Farsi_7",
    "Farsi_8",
    "Farsi_9",
    "Farsi_yeh",
    "Find",
    "First_Virtual_Screen",
    "G",
    "Gabovedot",
    "Gbreve",
    "Gcaron",
    "Gcedilla",
    "Gcircumflex",
    "Georgian_an",
    "Georgian_ban",
    "Georgian_can",
    "Georgian_char",
    "Georgian_chin",
    "Georgian_cil",
    "Georgian_don",
    "Georgian_en",
    "Georgian_fi",
    "Georgian_gan",
    "Georgian_ghan",
    "Georgian_hae",
    "Georgian_har",
    "Georgian_he",
    "Georgian_hie",
    "Georgian_hoe",
    "Georgian_in",
    "Georgian_jhan",
    "Georgian_jil",
    "Georgian_kan",
    "Georgian_khar",
    "Georgian_las",
    "Georgian_man",
    "Georgian_nar",
    "Georgian_on",
    "Georgian_par",
    "Georgian_phar",
    "Georgian_qar",
    "Georgian_rae",
    "Georgian_san",
    "Georgian_shin",
    "Georgian_tan",
    "Georgian_tar",
    "Georgian_un",
    "Georgian_vin",
    "Georgian_we",
    "Georgian_xan",
    "Georgian_zen",
    "Georgian_zhar",
    "Greek_ALPHA",
    "Greek_ALPHAaccent",
    "Greek_BETA",
    "Greek_CHI",
    "Greek_DELTA",
    "Greek_EPSILON",
    "Greek_EPSILONaccent",
    "Greek_ETA",
    "Greek_ETAaccent",
    "Greek_GAMMA",
    "Greek_IOTA",
    "Greek_IOTAaccent",
    "Greek_IOTAdiaeresis",
    "Greek_IOTAdieresis",
    "Greek_KAPPA",
    "Greek_LAMBDA",
    "Greek_LAMDA",
    "Greek_MU",
    "Greek_NU",
    "Greek_OMEGA",
    "Greek_OMEGAaccent",
    "Greek_OMICRON",
    "Greek_OMICRONaccent",
    "Greek_PHI",
    "Greek_PI",
    "Greek_PSI",
    "Greek_RHO",
    "Greek_SIGMA",
    "Greek_TAU",
    "Greek_THETA",
    "Greek_UPSILON",
    "Greek_UPSILONaccent",
    "Greek_UPSILONdieresis",
    "Greek_XI",
    "Greek_ZETA",
    "Greek_accentdieresis",
    "Greek_alpha",
    "Greek_alphaaccent",
    "Greek_beta",
    "Greek_chi",
    "Greek_delta",
    "Greek_epsilon",
    "Greek_epsilonaccent",
    "Greek_eta",
    "Greek_etaaccent",
    "Greek_finalsmallsigma",
    "Greek_gamma",
    "Greek_horizbar",
    "Greek_iota",
    "Greek_iotaaccent",
    "Greek_iotaaccentdieresis",
    "Greek_iotadieresis",
    "Greek_kappa",
    "Greek_lambda",
    "Greek_lamda",
    "Greek_mu",
    "Greek_nu",
    "Greek_omega",
    "Greek_omegaaccent",
    "Greek_omicron",
    "Greek_omicronaccent",
    "Greek_phi",
    "Greek_pi",
    "Greek_psi",
    "Greek_rho",
    "Greek_sigma",
    "Greek_switch",
    "Greek_tau",
    "Greek_theta",
    "Greek_upsilon",
    "Greek_upsilonaccent",
    "Greek_upsilonaccentdieresis",
    "Greek_upsilondieresis",
    "Greek_xi",
    "Greek_zeta",
    "H",
    "Hangul",
    "Hangul_A",
    "Hangul_AE",
    "Hangul_AraeA",
    "Hangul_AraeAE",
    "Hangul_Banja",
    "Hangul_Cieuc",
    "Hangul_Codeinput",
    "Hangul_Dikeud",
    "Hangul_E",
    "Hangul_EO",
    "Hangul_EU",
    "Hangul_End",
    "Hangul_Hanja",
    "Hangul_Hieuh",
    "Hangul_I",
    "Hangul_Ieung",
    "Hangul_J_Cieuc",
    "Hangul_J_Dikeud",
    "Hangul_J_Hieuh",
    "Hangul_J_Ieung",
    "Hangul_J_Jieuj",
    "Hangul_J_Khieuq",
    "Hangul_J_Kiyeog",
    "Hangul_J_KiyeogSios",
    "Hangul_J_KkogjiDalrinIeung",
    "Hangul_J_Mieum",
    "Hangul_J_Nieun",
    "Hangul_J_NieunHieuh",
    "Hangul_J_NieunJieuj",
    "Hangul_J_PanSios",
    "Hangul_J_Phieuf",
    "Hangul_J_Pieub",
    "Hangul_J_PieubSios",
    "Hangul_J_Rieul",
    "Hangul_J_RieulHieuh",
    "Hangul_J_RieulKiyeog",
    "Hangul_J_RieulMieum",
    "Hangul_J_RieulPhieuf",
    "Hangul_J_RieulPieub",
    "Hangul_J_RieulSios",
    "Hangul_J_RieulTieut",
    "Hangul_J_Sios",
    "Hangul_J_SsangKiyeog",
    "Hangul_J_SsangSios",
    "Hangul_J_Tieut",
    "Hangul_J_YeorinHieuh",
    "Hangul_Jamo",
    "Hangul_Jeonja",
    "Hangul_Jieuj",
    "Hangul_Khieuq",
    "Hangul_Kiyeog",
    "Hangul_KiyeogSios",
    "Hangul_KkogjiDalrinIeung",
    "Hangul_Mieum",
    "Hangul_MultipleCandidate",
    "Hangul_Nieun",
    "Hangul_NieunHieuh",
    "Hangul_NieunJieuj",
    "Hangul_O",
    "Hangul_OE",
    "Hangul_PanSios",
    "Hangul_Phieuf",
    "Hangul_Pieub",
    "Hangul_PieubSios",
    "Hangul_PostHanja",
    "Hangul_PreHanja",
    "Hangul_PreviousCandidate",
    "Hangul_Rieul",
    "Hangul_RieulHieuh",
    "Hangul_RieulKiyeog",
    "Hangul_RieulMieum",
    "Hangul_RieulPhieuf",
    "Hangul_RieulPieub",
    "Hangul_RieulSios",
    "Hangul_RieulTieut",
    "Hangul_RieulYeorinHieuh",
    "Hangul_Romaja",
    "Hangul_SingleCandidate",
    "Hangul_Sios",
    "Hangul_Special",
    "Hangul_SsangDikeud",
    "Hangul_SsangJieuj",
    "Hangul_SsangKiyeog",
    "Hangul_SsangPieub",
    "Hangul_SsangSios",
    "Hangul_Start",
    "Hangul_SunkyeongeumMieum",
    "Hangul_SunkyeongeumPhieuf",
    "Hangul_SunkyeongeumPieub",
    "Hangul_Tieut",
    "Hangul_U",
    "Hangul_WA",
    "Hangul_WAE",
    "Hangul_WE",
    "Hangul_WEO",
    "Hangul_WI",
    "Hangul_YA",
    "Hangul_YAE",
    "Hangul_YE",
    "Hangul_YEO",
    "Hangul_YI",
    "Hangul_YO",
    "Hangul_YU",
    "Hangul_YeorinHieuh",
    "Hangul_switch",
    "Hankaku",
    "Hcircumflex",
    "Hebrew_switch",
    "Help",
    "Henkan",
    "Henkan_Mode",
    "Hiragana",
    "Hiragana_Katakana",
    "Home",
    "Hstroke",
    "Hyper_L",
    "Hyper_R",
    "I",
    "ISO_Center_Object",
    "ISO_Continuous_Underline",
    "ISO_Discontinuous_Underline",
    "ISO_Emphasize",
    "ISO_Enter",
    "ISO_Fast_Cursor_Down",
    "ISO_Fast_Cursor_Left",
    "ISO_Fast_Cursor_Right",
    "ISO_Fast_Cursor_Up",
    "ISO_First_Group",
    "ISO_First_Group_Lock",
    "ISO_Group_Latch",
    "ISO_Group_Lock",
    "ISO_Group_Shift",
    "ISO_Last_Group",
    "ISO_Last_Group_Lock",
    "ISO_Left_Tab",
    "ISO_Level2_Latch",
    "ISO_Level3_Latch",
    "ISO_Level3_Lock",
    "ISO_Level3_Shift",
    "ISO_Level5_Latch",
    "ISO_Level5_Lock",
    "ISO_Level5_Shift",
    "ISO_Lock",
    "ISO_Move_Line_Down",
    "ISO_Move_Line_Up",
    "ISO_Next_Group",
    "ISO_Next_Group_Lock",
    "ISO_Partial_Line_Down",
    "ISO_Partial_Line_Up",
    "ISO_Partial_Space_Left",
    "ISO_Partial_Space_Right",
    "ISO_Prev_Group",
    "ISO_Prev_Group_Lock",
    "ISO_Release_Both_Margins",
    "ISO_Release_Margin_Left",
    "ISO_Release_Margin_Right",
    "ISO_Set_Margin_Left",
    "ISO_Set_Margin_Right",
    "Iabovedot",
    "Iacute",
    "Ibelowdot",
    "Ibreve",
    "Icircumflex",
    "Idiaeresis",
    "Igrave",
    "Ihook",
    "Imacron",
    "Insert",
    "Iogonek",
    "Itilde",
    "J",
    "Jcircumflex",
    "K",
    "KP_0",
    "KP_1",
    "KP_2",
    "KP_3",
    "KP_4",
    "KP_5",
    "KP_6",
    "KP_7",
    "KP_8",
    "KP_9",
    "KP_Add",
    "KP_Begin",
    "KP_Decimal",
    "KP_Delete",
    "KP_Divide",
    "KP_Down",
    "KP_End",
    "KP_Enter",
    "KP_Equal",
    "KP_F1",
    "KP_F2",
    "KP_F3",
    "KP_F4",
    "KP_Home",
    "KP_Insert",
    "KP_Left",
    "KP_Multiply",
    "KP_Next",
    "KP_Page_Down",
    "KP_Page_Up",
    "KP_Prior",
    "KP_Right",
    "KP_Separator",
    "KP_Space",
    "KP_Subtract",
    "KP_Tab",
    "KP_Up",
    "Kana_Lock",
    "Kana_Shift",
    "Kanji",
    "Kanji_Bangou",
    "Katakana",
    "Kcedilla",
    "Korean_Won",
    "L",
    "L1",
    "L10",
    "L2",
    "L3",
    "L4",
    "L5",
    "L6",
    "L7",
    "L8",
    "L9",
    "Lacute",
    "Last_Virtual_Screen",
    "Lbelowdot",
    "Lcaron",
    "Lcedilla",
    "Left",
    "Linefeed",
    "LiraSign",
    "Lstroke",
    "M",
    "Mabovedot",
    "Macedonia_DSE",
    "Macedonia_GJE",
    "Macedonia_KJE",
    "Macedonia_dse",
    "Macedonia_gje",
    "Macedonia_kje",
    "Mae_Koho",
    "Massyo",
    "Menu",
    "Meta_L",
    "Meta_R",
    "MillSign",
    "Mode_switch",
    "MouseKeys_Accel_Enable",
    "MouseKeys_Enable",
    "Muhenkan",
    "Multi_key",
    "MultipleCandidate",
    "N",
    "Nacute",
    "NairaSign",
    "Ncaron",
    "Ncedilla",
    "NewSheqelSign",
    "Next",
    "Next_Virtual_Screen",
    "Ntilde",
    "Num_Lock",
    "O",
    "OE",
    "Oacute",
    "Obarred",
    "Obelowdot",
    "Ocaron",
    "Ocircumflex",
    "Ocircumflexacute",
    "Ocircumflexbelowdot",
    "Ocircumflexgrave",
    "Ocircumflexhook",
    "Ocircumflextilde",
    "Odiaeresis",
    "Odoubleacute",
    "Ograve",
    "Ohook",
    "Ohorn",
    "Ohornacute",
    "Ohornbelowdot",
    "Ohorngrave",
    "Ohornhook",
    "Ohorntilde",
    "Omacron",
    "Ooblique",
    "Oslash",
    "Otilde",
    "Overlay1_Enable",
    "Overlay2_Enable",
    "P",
    "Pabovedot",
    "Page_Down",
    "Page_Up",
    "Pause",
    "PesetaSign",
    "Pointer_Accelerate",
    "Pointer_Button1",
    "Pointer_Button2",
    "Pointer_Button3",
    "Pointer_Button4",
    "Pointer_Button5",
    "Pointer_Button_Dflt",
    "Pointer_DblClick1",
    "Pointer_DblClick2",
    "Pointer_DblClick3",
    "Pointer_DblClick4",
    "Pointer_DblClick5",
    "Pointer_DblClick_Dflt",
    "Pointer_DfltBtnNext",
    "Pointer_DfltBtnPrev",
    "Pointer_Down",
    "Pointer_DownLeft",
    "Pointer_DownRight",
    "Pointer_Drag1",
    "Pointer_Drag2",
    "Pointer_Drag3",
    "Pointer_Drag4",
    "Pointer_Drag5",
    "Pointer_Drag_Dflt",
    "Pointer_EnableKeys",
    "Pointer_Left",
    "Pointer_Right",
    "Pointer_Up",
    "Pointer_UpLeft",
    "Pointer_UpRight",
    "Prev_Virtual_Screen",
    "PreviousCandidate",
    "Print",
    "Prior",
    "Q",
    "R",
    "R1",
    "R10",
    "R11",
    "R12",
    "R13",
    "R14",
    "R15",
    "R2",
    "R3",
    "R4",
    "R5",
    "R6",
    "R7",
    "R8",
    "R9",
    "Racute",
    "Rcaron",
    "Rcedilla",
    "Redo",
    "RepeatKeys_Enable",
    "Return",
    "Right",
    "Romaji",
    "RupeeSign",
    "S",
    "SCHWA",
    "Sabovedot",
    "Sacute",
    "Scaron",
    "Scedilla",
    "Scircumflex",
    "Scroll_Lock",
    "Select",
    "Serbian_DJE",
    "Serbian_DZE",
    "Serbian_JE",
    "Serbian_LJE",
    "Serbian_NJE",
    "Serbian_TSHE",
    "Serbian_dje",
    "Serbian_dze",
    "Serbian_je",
    "Serbian_lje",
    "Serbian_nje",
    "Serbian_tshe",
    "Shift_L",
    "Shift_Lock",
    "Shift_R",
    "SingleCandidate",
    "Sinh_a",
    "Sinh_aa",
    "Sinh_aa2",
    "Sinh_ae",
    "Sinh_ae2",
    "Sinh_aee",
    "Sinh_aee2",
    "Sinh_ai",
    "Sinh_ai2",
    "Sinh_al",
    "Sinh_au",
    "Sinh_au2",
    "Sinh_ba",
    "Sinh_bha",
    "Sinh_ca",
    "Sinh_cha",
    "Sinh_dda",
    "Sinh_ddha",
    "Sinh_dha",
    "Sinh_dhha",
    "Sinh_e",
    "Sinh_e2",
    "Sinh_ee",
    "Sinh_ee2",
    "Sinh_fa",
    "Sinh_ga",
    "Sinh_gha",
    "Sinh_h2",
    "Sinh_ha",
    "Sinh_i",
    "Sinh_i2",
    "Sinh_ii",
    "Sinh_ii2",
    "Sinh_ja",
    "Sinh_jha",
    "Sinh_jnya",
    "Sinh_ka",
    "Sinh_kha",
    "Sinh_kunddaliya",
    "Sinh_la",
    "Sinh_lla",
    "Sinh_lu",
    "Sinh_lu2",
    "Sinh_luu",
    "Sinh_luu2",
    "Sinh_ma",
    "Sinh_mba",
    "Sinh_na",
    "Sinh_ndda",
    "Sinh_ndha",
    "Sinh_ng",
    "Sinh_ng2",
    "Sinh_nga",
    "Sinh_nja",
    "Sinh_nna",
    "Sinh_nya",
    "Sinh_o",
    "Sinh_o2",
    "Sinh_oo",
    "Sinh_oo2",
    "Sinh_pa",
    "Sinh_pha",
    "Sinh_ra",
    "Sinh_ri",
    "Sinh_rii",
    "Sinh_ru2",
    "Sinh_ruu2",
    "Sinh_sa",
    "Sinh_sha",
    "Sinh_ssha",
    "Sinh_tha",
    "Sinh_thha",
    "Sinh_tta",
    "Sinh_ttha",
    "Sinh_u",
    "Sinh_u2",
    "Sinh_uu",
    "Sinh_uu2",
    "Sinh_va",
    "Sinh_ya",
    "SlowKeys_Enable",
    "StickyKeys_Enable",
    "Super_L",
    "Super_R",
    "Sys_Req",
    "T",
    "THORN",
    "Tab",
    "Tabovedot",
    "Tcaron",
    "Tcedilla",
    "Terminate_Server",
    "Thai_baht",
    "Thai_bobaimai",
    "Thai_chochan",
    "Thai_chochang",
    "Thai_choching",
    "Thai_chochoe",
    "Thai_dochada",
    "Thai_dodek",
    "Thai_fofa",
    "Thai_fofan",
    "Thai_hohip",
    "Thai_honokhuk",
    "Thai_khokhai",
    "Thai_khokhon",
    "Thai_khokhuat",
    "Thai_khokhwai",
    "Thai_khorakhang",
    "Thai_kokai",
    "Thai_lakkhangyao",
    "Thai_lekchet",
    "Thai_lekha",
    "Thai_lekhok",
    "Thai_lekkao",
    "Thai_leknung",
    "Thai_lekpaet",
    "Thai_leksam",
    "Thai_leksi",
    "Thai_leksong",
    "Thai_leksun",
    "Thai_lochula",
    "Thai_loling",
    "Thai_lu",
    "Thai_maichattawa",
    "Thai_maiek",
    "Thai_maihanakat",
    "Thai_maihanakat_maitho",
    "Thai_maitaikhu",
    "Thai_maitho",
    "Thai_maitri",
    "Thai_maiyamok",
    "Thai_moma",
    "Thai_ngongu",
    "Thai_nikhahit",
    "Thai_nonen",
    "Thai_nonu",
    "Thai_oang",
    "Thai_paiyannoi",
    "Thai_phinthu",
    "Thai_phophan",
    "Thai_phophung",
    "Thai_phosamphao",
    "Thai_popla",
    "Thai_rorua",
    "Thai_ru",
    "Thai_saraa",
    "Thai_saraaa",
    "Thai_saraae",
    "Thai_saraaimaimalai",
    "Thai_saraaimaimuan",
    "Thai_saraam",
    "Thai_sarae",
    "Thai_sarai",
    "Thai_saraii",
    "Thai_sarao",
    "Thai_sarau",
    "Thai_saraue",
    "Thai_sarauee",
    "Thai_sarauu",
    "Thai_sorusi",
    "Thai_sosala",
    "Thai_soso",
    "Thai_sosua",
    "Thai_thanthakhat",
    "Thai_thonangmontho",
    "Thai_thophuthao",
    "Thai_thothahan",
    "Thai_thothan",
    "Thai_thothong",
    "Thai_thothung",
    "Thai_topatak",
    "Thai_totao",
    "Thai_wowaen",
    "Thai_yoyak",
    "Thai_yoying",
    "Thorn",
    "Touroku",
    "Tslash",
    "U",
    "Uacute",
    "Ubelowdot",
    "Ubreve",
    "Ucircumflex",
    "Udiaeresis",
    "Udoubleacute",
    "Ugrave",
    "Uhook",
    "Uhorn",
    "Uhornacute",
    "Uhornbelowdot",
    "Uhorngrave",
    "Uhornhook",
    "Uhorntilde",
    "Ukrainian_GHE_WITH_UPTURN",
    "Ukrainian_I",
    "Ukrainian_IE",
    "Ukrainian_YI",
    "Ukrainian_ghe_with_upturn",
    "Ukrainian_i",
    "Ukrainian_ie",
    "Ukrainian_yi",
    "Ukranian_I",
    "Ukranian_JE",
    "Ukranian_YI",
    "Ukranian_i",
    "Ukranian_je",
    "Ukranian_yi",
    "Umacron",
    "Undo",
    "Uogonek",
    "Up",
    "Uring",
    "Utilde",
    "V",
    "VoidSymbol",
    "W",
    "Wacute",
    "Wcircumflex",
    "Wdiaeresis",
    "Wgrave",
    "WonSign",
    "X",
    "XF86AddFavorite",
    "XF86ApplicationLeft",
    "XF86ApplicationRight",
    "XF86AudioCycleTrack",
    "XF86AudioForward",
    "XF86AudioLowerVolume",
    "XF86AudioMedia",
    "XF86AudioMicMute",
    "XF86AudioMute",
    "XF86AudioNext",
    "XF86AudioPause",
    "XF86AudioPlay",
    "XF86AudioPreset",
    "XF86AudioPrev",
    "XF86AudioRaiseVolume",
    "XF86AudioRandomPlay",
    "XF86AudioRecord",
    "XF86AudioRepeat",
    "XF86AudioRewind",
    "XF86AudioStop",
    "XF86Away",
    "XF86Back",
    "XF86BackForward",
    "XF86Battery",
    "XF86Blue",
    "XF86Bluetooth",
    "XF86Book",
    "XF86BrightnessAdjust",
    "XF86CD",
    "XF86Calculater",
    "XF86Calculator",
    "XF86Calendar",
    "XF86Clear",
    "XF86ClearGrab",
    "XF86Close",
    "XF86Community",
    "XF86ContrastAdjust",
    "XF86Copy",
    "XF86Cut",
    "XF86CycleAngle",
    "XF86DOS",
    "XF86Display",
    "XF86Documents",
    "XF86Eject",
    "XF86Excel",
    "XF86Explorer",
    "XF86Favorites",
    "XF86Finance",
    "XF86Forward",
    "XF86FrameBack",
    "XF86FrameForward",
    "XF86FullScreen",
    "XF86Game",
    "XF86Go",
    "XF86Green",
    "XF86Hibernate",
    "XF86History",
    "XF86HomePage",
    "XF86HotLinks",
    "XF86KbdBrightnessDown",
    "XF86KbdBrightnessUp",
    "XF86KbdLightOnOff",
    "XF86Keyboard",
    "XF86Launch0",
    "XF86Launch1",
    "XF86Launch2",
    "XF86Launch3",
    "XF86Launch4",
    "XF86Launch5",
    "XF86Launch6",
    "XF86Launch7",
    "XF86Launch8",
    "XF86Launch9",
    "XF86LaunchA",
    "XF86LaunchB",
    "XF86LaunchC",
    "XF86LaunchD",
    "XF86LaunchE",
    "XF86LaunchF",
    "XF86LightBulb",
    "XF86LogGrabInfo",
    "XF86LogOff",
    "XF86LogWindowTree",
    "XF86Mail",
    "XF86MailForward",
    "XF86Market",
    "XF86Meeting",
    "XF86Memo",
    "XF86MenuKB",
    "XF86MenuPB",
    "XF86Messenger",
    "XF86ModeLock",
    "XF86MonBrightnessDown",
    "XF86MonBrightnessUp",
    "XF86Music",
    "XF86MyComputer",
    "XF86MySites",
    "XF86New",
    "XF86News",
    "XF86Next_VMode",
    "XF86OfficeHome",
    "XF86Open",
    "XF86OpenURL",
    "XF86Option",
    "XF86Paste",
    "XF86Phone",
    "XF86Pictures",
    "XF86PowerDown",
    "XF86PowerOff",
    "XF86Prev_VMode",
    "XF86Q",
    "XF86RFKill",
    "XF86Red",
    "XF86Refresh",
    "XF86Reload",
    "XF86Reply",
    "XF86RockerDown",
    "XF86RockerEnter",
    "XF86RockerUp",
    "XF86RotateWindows",
    "XF86RotationKB",
    "XF86RotationLockToggle",
    "XF86RotationPB",
    "XF86Save",
    "XF86ScreenSaver",
    "XF86ScrollClick",
    "XF86ScrollDown",
    "XF86ScrollUp",
    "XF86Search",
    "XF86Select",
    "XF86Send",
    "XF86Shop",
    "XF86Sleep",
    "XF86Spell",
    "XF86SplitScreen",
    "XF86Standby",
    "XF86Start",
    "XF86Stop",
    "XF86Subtitle",
    "XF86Support",
    "XF86Suspend",
    "XF86Switch_VT_1",
    "XF86Switch_VT_10",
    "XF86Switch_VT_11",
    "XF86Switch_VT_12",
    "XF86Switch_VT_2",
    "XF86Switch_VT_3",
    "XF86Switch_VT_4",
    "XF86Switch_VT_5",
    "XF86Switch_VT_6",
    "XF86Switch_VT_7",
    "XF86Switch_VT_8",
    "XF86Switch_VT_9",
    "XF86TaskPane",
    "XF86Terminal",
    "XF86Time",
    "XF86ToDoList",
    "XF86Tools",
    "XF86TopMenu",
    "XF86TouchpadOff",
    "XF86TouchpadOn",
    "XF86TouchpadToggle",
    "XF86Travel",
    "XF86UWB",
    "XF86Ungrab",
    "XF86User1KB",
    "XF86User2KB",
    "XF86UserPB",
    "XF86VendorHome",
    "XF86Video",
    "XF86View",
    "XF86WLAN",
    "XF86WWAN",
    "XF86WWW",
    "XF86WakeUp",
    "XF86WebCam",
    "XF86WheelButton",
    "XF86Word",
    "XF86Xfer",
    "XF86Yellow",
    "XF86ZoomIn",
    "XF86ZoomOut",
    "XF86iTouch",
    "Xabovedot",
    "Y",
    "Yacute",
    "Ybelowdot",
    "Ycircumflex",
    "Ydiaeresis",
    "Ygrave",
    "Yhook",
    "Ytilde",
    "Z",
    "Zabovedot",
    "Zacute",
    "Zcaron",
    "Zen_Koho",
    "Zenkaku",
    "Zenkaku_Hankaku",
    "Zstroke",
    "a",
    "aacute",
    "abelowdot",
    "abovedot",
    "abreve",
    "abreveacute",
    "abrevebelowdot",
    "abrevegrave",
    "abrevehook",
    "abrevetilde",
    "acircumflex",
    "acircumflexacute",
    "acircumflexbelowdot",
    "acircumflexgrave",
    "acircumflexhook",
    "acircumflextilde",
    "acute",
    "adiaeresis",
    "ae",
    "agrave",
    "ahook",
    "amacron",
    "ampersand",
    "aogonek",
    "apostrophe",
    "approxeq",
    "aring",
    "asciicircum",
    "asciitilde",
    "asterisk",
    "at",
    "atilde",
    "b",
    "babovedot",
    "backslash",
    "bar",
    "because",
    "braceleft",
    "braceright",
    "bracketleft",
    "bracketright",
    "braille_blank",
    "braille_dot_1",
    "braille_dot_10",
    "braille_dot_2",
    "braille_dot_3",
    "braille_dot_4",
    "braille_dot_5",
    "braille_dot_6",
    "braille_dot_7",
    "braille_dot_8",
    "braille_dot_9",
    "braille_dots_1",
    "braille_dots_12",
    "braille_dots_123",
    "braille_dots_1234",
    "braille_dots_12345",
    "braille_dots_123456",
    "braille_dots_1234567",
    "braille_dots_12345678",
    "braille_dots_1234568",
    "braille_dots_123457",
    "braille_dots_1234578",
    "braille_dots_123458",
    "braille_dots_12346",
    "braille_dots_123467",
    "braille_dots_1234678",
    "braille_dots_123468",
    "braille_dots_12347",
    "braille_dots_123478",
    "braille_dots_12348",
    "braille_dots_1235",
    "braille_dots_12356",
    "braille_dots_123567",
    "braille_dots_1235678",
    "braille_dots_123568",
    "braille_dots_12357",
    "braille_dots_123578",
    "braille_dots_12358",
    "braille_dots_1236",
    "braille_dots_12367",
    "braille_dots_123678",
    "braille_dots_12368",
    "braille_dots_1237",
    "braille_dots_12378",
    "braille_dots_1238",
    "braille_dots_124",
    "braille_dots_1245",
    "braille_dots_12456",
    "braille_dots_124567",
    "braille_dots_1245678",
    "braille_dots_124568",
    "braille_dots_12457",
    "braille_dots_124578",
    "braille_dots_12458",
    "braille_dots_1246",
    "braille_dots_12467",
    "braille_dots_124678",
    "braille_dots_12468",
    "braille_dots_1247",
    "braille_dots_12478",
    "braille_dots_1248",
    "braille_dots_125",
    "braille_dots_1256",
    "braille_dots_12567",
    "braille_dots_125678",
    "braille_dots_12568",
    "braille_dots_1257",
    "braille_dots_12578",
    "braille_dots_1258",
    "braille_dots_126",
    "braille_dots_1267",
    "braille_dots_12678",
    "braille_dots_1268",
    "braille_dots_127",
    "braille_dots_1278",
    "braille_dots_128",
    "braille_dots_13",
    "braille_dots_134",
    "braille_dots_1345",
    "braille_dots_13456",
    "braille_dots_134567",
    "braille_dots_1345678",
    "braille_dots_134568",
    "braille_dots_13457",
    "braille_dots_134578",
    "braille_dots_13458",
    "braille_dots_1346",
    "braille_dots_13467",
    "braille_dots_134678",
    "braille_dots_13468",
    "braille_dots_1347",
    "braille_dots_13478",
    "braille_dots_1348",
    "braille_dots_135",
    "braille_dots_1356",
    "braille_dots_13567",
    "braille_dots_135678",
    "braille_dots_13568",
    "braille_dots_1357",
    "braille_dots_13578",
    "braille_dots_1358",
    "braille_dots_136",
    "braille_dots_1367",
    "braille_dots_13678",
    "braille_dots_1368",
    "braille_dots_137",
    "braille_dots_1378",
    "braille_dots_138",
    "braille_dots_14",
    "braille_dots_145",
    "braille_dots_1456",
    "braille_dots_14567",
    "braille_dots_145678",
    "braille_dots_14568",
    "braille_dots_1457",
    "braille_dots_14578",
    "braille_dots_1458",
    "braille_dots_146",
    "braille_dots_1467",
    "braille_dots_14678",
    "braille_dots_1468",
    "braille_dots_147",
    "braille_dots_1478",
    "braille_dots_148",
    "braille_dots_15",
    "braille_dots_156",
    "braille_dots_1567",
    "braille_dots_15678",
    "braille_dots_1568",
    "braille_dots_157",
    "braille_dots_1578",
    "braille_dots_158",
    "braille_dots_16",
    "braille_dots_167",
    "braille_dots_1678",
    "braille_dots_168",
    "braille_dots_17",
    "braille_dots_178",
    "braille_dots_18",
    "braille_dots_2",
    "braille_dots_23",
    "braille_dots_234",
    "braille_dots_2345",
    "braille_dots_23456",
    "braille_dots_234567",
    "braille_dots_2345678",
    "braille_dots_234568",
    "braille_dots_23457",
    "braille_dots_234578",
    "braille_dots_23458",
    "braille_dots_2346",
    "braille_dots_23467",
    "braille_dots_234678",
    "braille_dots_23468",
    "braille_dots_2347",
    "braille_dots_23478",
    "braille_dots_2348",
    "braille_dots_235",
    "braille_dots_2356",
    "braille_dots_23567",
    "braille_dots_235678",
    "braille_dots_23568",
    "braille_dots_2357",
    "braille_dots_23578",
    "braille_dots_2358",
    "braille_dots_236",
    "braille_dots_2367",
    "braille_dots_23678",
    "braille_dots_2368",
    "braille_dots_237",
    "braille_dots_2378",
    "braille_dots_238",
    "braille_dots_24",
    "braille_dots_245",
    "braille_dots_2456",
    "braille_dots_24567",
    "braille_dots_245678",
    "braille_dots_24568",
    "braille_dots_2457",
    "braille_dots_24578",
    "braille_dots_2458",
    "braille_dots_246",
    "braille_dots_2467",
    "braille_dots_24678",
    "braille_dots_2468",
    "braille_dots_247",
    "braille_dots_2478",
    "braille_dots_248",
    "braille_dots_25",
    "braille_dots_256",
    "braille_dots_2567",
    "braille_dots_25678",
    "braille_dots_2568",
    "braille_dots_257",
    "braille_dots_2578",
    "braille_dots_258",
    "braille_dots_26",
    "braille_dots_267",
    "braille_dots_2678",
    "braille_dots_268",
    "braille_dots_27",
    "braille_dots_278",
    "braille_dots_28",
    "braille_dots_3",
    "braille_dots_34",
    "braille_dots_345",
    "braille_dots_3456",
    "braille_dots_34567",
    "braille_dots_345678",
    "braille_dots_34568",
    "braille_dots_3457",
    "braille_dots_34578",
    "braille_dots_3458",
    "braille_dots_346",
    "braille_dots_3467",
    "braille_dots_34678",
    "braille_dots_3468",
    "braille_dots_347",
    "braille_dots_3478",
    "braille_dots_348",
    "braille_dots_35",
    "braille_dots_356",
    "braille_dots_3567",
    "braille_dots_35678",
    "braille_dots_3568",
    "braille_dots_357",
    "braille_dots_3578",
    "braille_dots_358",
    "braille_dots_36",
    "braille_dots_367",
    "braille_dots_3678",
    "braille_dots_368",
    "braille_dots_37",
    "braille_dots_378",
    "braille_dots_38",
    "braille_dots_4",
    "braille_dots_45",
    "braille_dots_456",
    "braille_dots_4567",
    "braille_dots_45678",
    "braille_dots_4568",
    "braille_dots_457",
    "braille_dots_4578",
    "braille_dots_458",
    "braille_dots_46",
    "braille_dots_467",
    "braille_dots_4678",
    "braille_dots_468",
    "braille_dots_47",
    "braille_dots_478",
    "braille_dots_48",
    "braille_dots_5",
    "braille_dots_56",
    "braille_dots_567",
    "braille_dots_5678",
    "braille_dots_568",
    "braille_dots_57",
    "braille_dots_578",
    "braille_dots_58",
    "braille_dots_6",
    "braille_dots_67",
    "braille_dots_678",
    "braille_dots_68",
    "braille_dots_7",
    "braille_dots_78",
    "braille_dots_8",
    "breve",
    "brokenbar",
    "c",
    "c_h",
    "cabovedot",
    "cacute",
    "caron",
    "ccaron",
    "ccedilla",
    "ccircumflex",
    "cedilla",
    "cent",
    "ch",
    "colon",
    "comma",
    "containsas",
    "copyright",
    "cuberoot",
    "currency",
    "d",
    "dabovedot",
    "dcaron",
    "dead_A",
    "dead_E",
    "dead_I",
    "dead_O",
    "dead_U",
    "dead_a",
    "dead_abovecomma",
    "dead_abovedot",
    "dead_abovereversedcomma",
    "dead_abovering",
    "dead_acute",
    "dead_belowbreve",
    "dead_belowcircumflex",
    "dead_belowcomma",
    "dead_belowdiaeresis",
    "dead_belowdot",
    "dead_belowmacron",
    "dead_belowring",
    "dead_belowtilde",
    "dead_breve",
    "dead_capital_schwa",
    "dead_caron",
    "dead_cedilla",
    "dead_circumflex",
    "dead_currency",
    "dead_dasia",
    "dead_diaeresis",
    "dead_doubleacute",
    "dead_doublegrave",
    "dead_e",
    "dead_grave",
    "dead_greek",
    "dead_hook",
    "dead_horn",
    "dead_i",
    "dead_invertedbreve",
    "dead_iota",
    "dead_macron",
    "dead_o",
    "dead_ogonek",
    "dead_perispomeni",
    "dead_psili",
    "dead_semivoiced_sound",
    "dead_small_schwa",
    "dead_stroke",
    "dead_tilde",
    "dead_u",
    "dead_voiced_sound",
    "degree",
    "diaeresis",
    "dintegral",
    "division",
    "dollar",
    "doubleacute",
    "dstroke",
    "e",
    "eabovedot",
    "eacute",
    "ebelowdot",
    "ecaron",
    "ecircumflex",
    "ecircumflexacute",
    "ecircumflexbelowdot",
    "ecircumflexgrave",
    "ecircumflexhook",
    "ecircumflextilde",
    "ediaeresis",
    "egrave",
    "ehook",
    "eightsubscript",
    "eightsuperior",
    "elementof",
    "emacron",
    "emptyset",
    "eng",
    "eogonek",
    "equal",
    "eth",
    "etilde",
    "exclam",
    "exclamdown",
    "ezh",
    "f",
    "fabovedot",
    "fivesubscript",
    "fivesuperior",
    "foursubscript",
    "foursuperior",
    "fourthroot",
    "g",
    "gabovedot",
    "gbreve",
    "gcaron",
    "gcedilla",
    "gcircumflex",
    "grave",
    "greater",
    "guillemotleft",
    "guillemotright",
    "h",
    "hcircumflex",
    "hebrew_aleph",
    "hebrew_ayin",
    "hebrew_bet",
    "hebrew_beth",
    "hebrew_chet",
    "hebrew_dalet",
    "hebrew_daleth",
    "hebrew_doublelowline",
    "hebrew_finalkaph",
    "hebrew_finalmem",
    "hebrew_finalnun",
    "hebrew_finalpe",
    "hebrew_finalzade",
    "hebrew_finalzadi",
    "hebrew_gimel",
    "hebrew_gimmel",
    "hebrew_he",
    "hebrew_het",
    "hebrew_kaph",
    "hebrew_kuf",
    "hebrew_lamed",
    "hebrew_mem",
    "hebrew_nun",
    "hebrew_pe",
    "hebrew_qoph",
    "hebrew_resh",
    "hebrew_samech",
    "hebrew_samekh",
    "hebrew_shin",
    "hebrew_taf",
    "hebrew_taw",
    "hebrew_tet",
    "hebrew_teth",
    "hebrew_waw",
    "hebrew_yod",
    "hebrew_zade",
    "hebrew_zadi",
    "hebrew_zain",
    "hebrew_zayin",
    "hstroke",
    "hyphen",
    "i",
    "iacute",
    "ibelowdot",
    "ibreve",
    "icircumflex",
    "idiaeresis",
    "idotless",
    "igrave",
    "ihook",
    "imacron",
    "iogonek",
    "itilde",
    "j",
    "jcircumflex",
    "k",
    "kana_A",
    "kana_CHI",
    "kana_E",
    "kana_FU",
    "kana_HA",
    "kana_HE",
    "kana_HI",
    "kana_HO",
    "kana_HU",
    "kana_I",
    "kana_KA",
    "kana_KE",
    "kana_KI",
    "kana_KO",
    "kana_KU",
    "kana_MA",
    "kana_ME",
    "kana_MI",
    "kana_MO",
    "kana_MU",
    "kana_N",
    "kana_NA",
    "kana_NE",
    "kana_NI",
    "kana_NO",
    "kana_NU",
    "kana_O",
    "kana_RA",
    "kana_RE",
    "kana_RI",
    "kana_RO",
    "kana_RU",
    "kana_SA",
    "kana_SE",
    "kana_SHI",
    "kana_SO",
    "kana_SU",
    "kana_TA",
    "kana_TE",
    "kana_TI",
    "kana_TO",
    "kana_TSU",
    "kana_TU",
    "kana_U",
    "kana_WA",
    "kana_WO",
    "kana_YA",
    "kana_YO",
    "kana_YU",
    "kana_a",
    "kana_closingbracket",
    "kana_comma",
    "kana_conjunctive",
    "kana_e",
    "kana_fullstop",
    "kana_i",
    "kana_middledot",
    "kana_o",
    "kana_openingbracket",
    "kana_switch",
    "kana_tsu",
    "kana_tu",
    "kana_u",
    "kana_ya",
    "kana_yo",
    "kana_yu",
    "kappa",
    "kcedilla",
    "kra",
    "l",
    "lacute",
    "lbelowdot",
    "lcaron",
    "lcedilla",
    "less",
    "lstroke",
    "m",
    "mabovedot",
    "macron",
    "masculine",
    "minus",
    "mu",
    "multiply",
    "n",
    "nacute",
    "ncaron",
    "ncedilla",
    "ninesubscript",
    "ninesuperior",
    "nobreakspace",
    "notapproxeq",
    "notelementof",
    "notidentical",
    "notsign",
    "ntilde",
    "numbersign",
    "numerosign",
    "o",
    "oacute",
    "obarred",
    "obelowdot",
    "ocaron",
    "ocircumflex",
    "ocircumflexacute",
    "ocircumflexbelowdot",
    "ocircumflexgrave",
    "ocircumflexhook",
    "ocircumflextilde",
    "odiaeresis",
    "odoubleacute",
    "oe",
    "ogonek",
    "ograve",
    "ohook",
    "ohorn",
    "ohornacute",
    "ohornbelowdot",
    "ohorngrave",
    "ohornhook",
    "ohorntilde",
    "omacron",
    "onehalf",
    "onequarter",
    "onesubscript",
    "onesuperior",
    "ooblique",
    "ordfeminine",
    "oslash",
    "otilde",
    "overline",
    "p",
    "pabovedot",
    "paragraph",
    "parenleft",
    "parenright",
    "partdifferential",
    "percent",
    "period",
    "periodcentered",
    "plus",
    "plusminus",
    "prolongedsound",
    "q",
    "question",
    "questiondown",
    "quotedbl",
    "quoteleft",
    "quoteright",
    "r",
    "racute",
    "rcaron",
    "rcedilla",
    "registered",
    "s",
    "sabovedot",
    "sacute",
    "scaron",
    "scedilla",
    "schwa",
    "scircumflex",
    "script_switch",
    "section",
    "semicolon",
    "semivoicedsound",
    "sevensubscript",
    "sevensuperior",
    "sixsubscript",
    "sixsuperior",
    "slash",
    "space",
    "squareroot",
    "ssharp",
    "sterling",
    "stricteq",
    "t",
    "tabovedot",
    "tcaron",
    "tcedilla",
    "thorn",
    "threequarters",
    "threesubscript",
    "threesuperior",
    "tintegral",
    "tslash",
    "twosubscript",
    "twosuperior",
    "u",
    "uacute",
    "ubelowdot",
    "ubreve",
    "ucircumflex",
    "udiaeresis",
    "udoubleacute",
    "ugrave",
    "uhook",
    "uhorn",
    "uhornacute",
    "uhornbelowdot",
    "uhorngrave",
    "uhornhook",
    "uhorntilde",
    "umacron",
    "underscore",
    "uogonek",
    "uring",
    "utilde",
    "v",
    "voicedsound",
    "w",
    "wacute",
    "wcircumflex",
    "wdiaeresis",
    "wgrave",
    "x",
    "xabovedot",
    "y",
    "yacute",
    "ybelowdot",
    "ycircumflex",
    "ydiaeresis",
    "yen",
    "ygrave",
    "yhook",
    "ytilde",
    "z",
    "zabovedot",
    "zacute",
    "zcaron",
    "zerosubscript",
    "zerosuperior",
    "zstroke",
];

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
        // `\v` is `isspace`: a command line.
        assert_eq!(rows("a\n\x0bcmd\n").0, [row("sxhkd", "a", "cmd", "cmd")]);
        // A comment between a hotkey and its command, then a blank line: no title.
        assert_eq!(
            rows("a\n# c\n\tx\n\nb\n\ty\n").0,
            [row("sxhkd", "a", "x", "x"), row("sxhkd", "b", "y", "y")]
        );
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

    /// The paths shown.
    fn paths(files: &[(PathBuf, PathBuf)]) -> Vec<String> {
        files.iter().map(|(_, p)| p.display().to_string()).collect()
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
                // A shell running a script named sxhkd (argv[0] set below); one whose exe is a shell; one whose
                // binary was replaced since it started.
                (23, &["-c", "/script"], Some(&[]), "/w"),
                (24, &["-c", "/shell"], Some(&[]), "/w"),
                (25, &["-c", "/s/d"], Some(&[]), "/w"),
                // Another variable starting with `HOME`.
                (26, &[], Some(&["HOMEBREW_PREFIX=/x", "HOME=/h3"]), "/w"),
            ],
        );
        std::fs::write(dir.join("20/cmdline"), "").unwrap();
        std::fs::write(dir.join("23/cmdline"), "/bin/sh\0/x/sxhkd\0-c\0/script\0").unwrap();
        std::os::unix::fs::symlink("/bin/sh", dir.join("24/exe")).unwrap();
        std::os::unix::fs::symlink("/usr/bin/sxhkd (deleted)", dir.join("25/exe")).unwrap();
        let all = config_files(&dir, uid(&dir), &own_env);
        let files = paths(&all);
        let running = running(&dir, uid(&dir));
        let theirs = config_files(&dir, uid(&dir).wrapping_add(1), &own_env);
        // A relative path is opened through the process's own `cwd` link, and shown through its target.
        let opened = all[3].0.display().to_string();
        let cwd = |_: u32, p: &str| format!("/w/{p}");
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
            "/s/d".into(),
            "/h3/.config/sxhkd/sxhkdrc".into(),
        ];
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(files, want);
        assert_eq!(opened, format!("{}/13/cwd/rel/path", dir.display()));
        assert!(!running.iter().any(|(pid, _)| *pid == 20));
        // Another user's sxhkd is ignored.
        assert!(theirs.is_empty());
    }

    #[test]
    fn one_file_read_once_however_named() {
        let root = crate::test_tmp().join(format!("kbf-sxhkd-conf-{}", std::process::id()));
        let conf = root.join("conf");
        std::fs::create_dir_all(&conf).unwrap();
        std::fs::write(conf.join("rc"), "a\n\tx\n").unwrap();
        std::os::unix::fs::symlink(&conf, root.join("link")).unwrap();
        let (c, r) = (conf.display().to_string(), root.display().to_string());
        let abs = format!("{c}/rc");
        // The same `-c rc` from the same directory twice, an absolute spelling, and one through a symlinked dir.
        let dir = fake_proc(
            "same",
            &[
                (30, &["-c", "rc"], Some(&[]), &c),
                (31, &["-c", "rc"], Some(&[]), &c),
                (32, &["-c", &abs], Some(&[]), "/w"),
                (33, &["-c", "link/rc"], Some(&[]), &r),
            ],
        );
        let files = paths(&config_files(&dir, uid(&dir), &own_env));
        let (sheet, _) = read(&dir, Some(uid(&dir)), &own_env).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(files, [abs]);
        assert_eq!(sheet.binds["sxhkd"].len(), 1);
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
        let missing = format!("can't read {cwd}/missing");
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
