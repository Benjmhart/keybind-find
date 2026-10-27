//! vim: the user's maps, from `:map`, `:map!`, `:tmap` and `:lmap` in a headless vim (`vim -es`) started with the
//! user's vimrc. vim's built-in keys are not listed: `:map` shows none, and they are vim's own documentation's.
//! Shown when vim or vi is the program in the focused terminal, or a gvim window is focused. A `vim` that is nvim
//! gives nvim's sheet.

use crate::context::{run_for, run_for_status, Context};
use crate::sheet::{Bind, Name, Sheet, Values};
use anyhow::{bail, Context as _};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// A slow vimrc (plugin managers) must not hang the picker; nvim's limit.
const TIMEOUT: Duration = Duration::from_secs(10);

/// `--version` answers at once; it mustn't add a second long wait to the run's.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// Marks where the maps start on stdout: anything a vimrc or plugin prints before it is ignored.
const SENTINEL: &str = "KEYBIND-FIND-VIM";

/// Marks where they end: a vim that stopped part way through isn't taken for one with fewer maps.
const END: &str = "KEYBIND-FIND-VIM-DONE";

type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn sheet(ctx: &Context) -> anyhow::Result<Sheet> {
    let (sheet, notes) = sheet_with(ctx, |_| {}, &|k| std::env::var(k).ok())?;
    for note in notes {
        eprintln!("keybind-find: vim: {note}");
    }
    Ok(sheet)
}

/// `sheet` with `setup` applied to both runs and `env` for `PATH`, the vimrc and `VIMINIT` (a test seam).
fn sheet_with(
    ctx: &Context,
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    // Debian's vim-tiny has a `vi` and no `vim`.
    let program = program(env).context("neither vim nor vi is on PATH")?;
    let mut version = Command::new(program);
    version
        .arg("--version")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
    setup(&mut version);
    let version = run_for(version, VERSION_TIMEOUT)
        .with_context(|| format!("running {program} --version"))?;
    if is_nvim(&version) {
        return Ok((super::nvim::sheet(ctx)?, Vec::new()));
    }
    if !version.trim_start().starts_with("VIM") {
        bail!(
            "{program} is not vim ({}); its keys aren't read",
            version.lines().next().unwrap_or_default().trim()
        );
    }
    // `:echo` and so `:redir`'s sentinels need +eval, which vim-tiny is built without.
    if version.split_whitespace().any(|w| w == "-eval") {
        bail!("{program} is built without +eval (vim-tiny), so its maps can't be read");
    }
    read(program, TIMEOUT, setup, env)
}

/// `vim`, else `vi`, whichever is on `PATH` first in that order.
fn program(env: Env) -> Option<&'static str> {
    let path = env("PATH").unwrap_or_default();
    ["vim", "vi"].into_iter().find(|p| {
        path.split(':')
            .filter(|d| !d.is_empty())
            .any(|d| Path::new(d).join(p).is_file())
    })
}

/// Whether `vim --version` is nvim's (a `vim` that is a symlink to or a wrapper of nvim).
fn is_nvim(version: &str) -> bool {
    version.trim_start().starts_with("NVIM")
}

/// Runs `program` headless with `setup` applied (a test seam: `PATH`, `HOME`) and the vimrc found through `env`.
/// Also returns the lines for stderr.
fn read(
    program: &str,
    timeout: Duration,
    setup: impl Fn(&mut Command),
    env: Env,
) -> anyhow::Result<(Sheet, Vec<String>)> {
    if env("VIMINIT").is_some_and(|v| !v.is_empty()) {
        bail!("VIMINIT is set, and vim then reads it instead of a vimrc; it isn't read here");
    }
    let vimrc = vimrc(env);
    let mut cmd = Command::new(program);
    cmd.args(args(vimrc.as_deref()))
        // Through a mise shim, a pinned-but-missing vim must not start a download in the middle of a keypress.
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
        // English messages and UTF-8 keys, whatever the session's locale.
        .env("LC_ALL", "C.UTF-8")
        // A vimrc with `set exrc` would also source a `.vimrc` from the working directory: give it none of ours.
        .current_dir("/");
    if let Some(v) = &vimrc {
        // `-u` leaves `$MYVIMRC` unset, which vimrcs use to find their own directory.
        cmd.env("MYVIMRC", v);
    }
    setup(&mut cmd);
    // vim exits 1 after any error in a vimrc, with its maps printed all the same: the sentinels say it ran.
    let (status, stdout, stderr) = run_for_status(cmd, timeout).context("running vim headless")?;
    let marker = format!("\n{SENTINEL}\n");
    let Some(at) = stdout.rfind(&marker) else {
        let why = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        bail!(
            "vim printed no maps ({status}{}{why}); the vimrc or a VimEnter autocommand may have quit vim",
            if why.is_empty() { "" } else { ": " }
        );
    };
    let rest = &stdout[at + marker.len()..];
    let Some(end) = rest.rfind(&format!("\n{END}")) else {
        bail!("vim stopped before it had listed every map ({status})");
    };
    let (sheet, skipped) = parse(&rest[..end])?;
    let notes = (skipped > 0)
        .then(|| {
            let maps = if skipped == 1 { "map" } else { "maps" };
            format!("skipping {skipped} {maps} with no rhs or a key that has no keysym we know")
        })
        .into_iter()
        .collect();
    Ok((sheet, notes))
}

/// The vimrc vim reads, the first that exists of `~/.vimrc`, `~/.vim/vimrc` and `$XDG_CONFIG_HOME/vim/vimrc`
/// (`~/.config/vim/vimrc` when unset; none when set but empty or relative, as vim reads nothing there either).
/// `None`: there is none. vim puts the XDG one's directory in `runtimepath` itself when `-u` names it.
fn vimrc(env: Env) -> Option<PathBuf> {
    let home = env("HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    let xdg = match env("XDG_CONFIG_HOME") {
        None => home.as_ref().map(|h| h.join(".config")),
        Some(d) => Some(PathBuf::from(d)).filter(|d| d.is_absolute()),
    };
    [
        home.as_ref().map(|h| h.join(".vimrc")),
        home.as_ref().map(|h| h.join(".vim/vimrc")),
        xdg.map(|d| d.join("vim/vimrc")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file())
}

/// vim's arguments. `-es` reads no vimrc by itself, so `-u` names it; with none, `NORC` and `defaults.vim`, as a
/// plain vim start loads. `VimEnter` is not sent: a vimrc's VimEnter autocommands (vim-plug's install-on-start)
/// would run on every keypress and be killed part way. `:tmap` and `:lmap` are `silent!`: a vim without them still
/// lists the rest. At most ten `-c`, vim's limit.
fn args(vimrc: Option<&Path>) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-es", "-N", "-i", "NONE", "-u"].map(OsString::from).into();
    match vimrc {
        Some(path) => args.push(path.into()),
        None => args.extend(["NORC", "-c", "source $VIMRUNTIME/defaults.vim"].map(OsString::from)),
    }
    let echo = format!("echo \"{SENTINEL}\"");
    let end = format!("echo \"{END}\" | redir END | qa!");
    for c in [
        "redir! > /dev/stdout",
        &echo,
        "silent map",
        "silent map!",
        "silent! tmap",
        "silent! lmap",
        &end,
    ] {
        args.extend(["-c", c].map(OsString::from));
    }
    args
}

/// The sheet of `:map` output, and the maps skipped for a key with no keysym. Each line is the mode letters (three
/// columns), the lhs padded to column 15 (or one space after a longer one), two flag columns (`*` no remap, `&`
/// remaps only script-local maps, `@` buffer-local) and the rhs. `<Plug>` and `<SNR>` maps (a plugin's insides)
/// and buffer-local ones (a headless vim has no buffer of the user's) are left out, as nvim's are.
fn parse(text: &str) -> anyhow::Result<(Sheet, usize)> {
    let mut sheet = Sheet {
        title: "vim".into(),
        provider: "vim".into(),
        binds: Default::default(),
    };
    let mut skipped = 0;
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        // Blank lines between the commands' output, and "No mapping found" (no mode column).
        let Some(lhs) = chars
            .get(3..)
            .map(|r| r.iter().take_while(|c| **c != ' ').collect::<String>())
            .filter(|l| {
                !l.is_empty()
                    && line.starts_with([' ', 'n', 'v', 'x', 's', 'o', 'i', 'c', 'l', 't', '!'])
            })
        else {
            continue;
        };
        let flags_at = (3 + lhs.chars().count() + 1).max(15);
        let flags: String = chars.iter().skip(flags_at).take(2).collect();
        let rhs: String = chars.iter().skip(flags_at + 2).collect();
        if flags.contains('@')
            || [&lhs, &rhs]
                .iter()
                .any(|s| s.contains("<Plug>") || s.contains("<SNR>"))
        {
            continue;
        }
        let modes = modes(&chars[..3].iter().collect::<String>());
        let (Some(category), false) = (modes.first(), rhs.is_empty()) else {
            skipped += 1;
            continue;
        };
        let Some(key) = super::nvim::key(&lhs) else {
            skipped += 1;
            continue;
        };
        let names = modes
            .iter()
            .map(|m| m.parse::<Name>())
            .collect::<anyhow::Result<Vec<_>>>()?;
        sheet
            .binds
            .entry(category.to_string())
            .or_default()
            .push(Bind {
                key,
                desc: rhs.clone(),
                action: rhs,
                when: [("mode".parse()?, Values::try_from(names)?)]
                    .into_iter()
                    .collect(),
            });
    }
    Ok((sheet, skipped))
}

/// The modes of `:map`'s mode column, by nvim's names, once each in the order normal, visual, select, operator,
/// insert, command, terminal, language (the first is the bind's category): blank (`:map`) is normal, visual and
/// operator; `!` is insert and command; `v` is visual and select.
fn modes(column: &str) -> Vec<&'static str> {
    const ORDER: [&str; 8] = [
        "normal", "visual", "select", "operator", "insert", "command", "terminal", "language",
    ];
    if column.trim().is_empty() {
        return vec!["normal", "visual", "operator"];
    }
    let mut found: Vec<&str> = Vec::new();
    for c in column.chars() {
        found.extend_from_slice(match c {
            'n' => &["normal"],
            'v' => &["visual", "select"],
            'x' => &["visual"],
            's' => &["select"],
            'o' => &["operator"],
            'i' => &["insert"],
            'c' => &["command"],
            '!' => &["insert", "command"],
            'l' => &["language"],
            't' => &["terminal"],
            _ => &[],
        });
    }
    ORDER.into_iter().filter(|m| found.contains(m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vim")
    }

    fn find<'a>(s: &'a Sheet, category: &str, key: &str) -> Option<&'a Bind> {
        s.binds.get(category)?.iter().find(|b| b.key == key)
    }

    fn modes_of(b: &Bind) -> Vec<String> {
        b.when["mode"]
            .iter()
            .map(|m| m.as_str().to_string())
            .collect()
    }

    #[test]
    fn parses_captured_map_output() {
        let text = std::fs::read_to_string(fixtures().join("map.txt")).unwrap();
        let at = text.find(SENTINEL).unwrap() + SENTINEL.len();
        let (s, skipped) = parse(&text[at..]).unwrap();
        let q = find(&s, "normal", "space q").unwrap();
        assert_eq!((q.desc.as_str(), q.action.as_str()), (":q<CR>", ":q<CR>"));
        assert_eq!(modes_of(q), ["normal"]);
        // A ctrl chord, a long rhs with spaces, a long lhs, an rhs that is `*` (in the flag-free columns).
        assert_eq!(find(&s, "normal", "ctrl+w x").unwrap().desc, "<C-W>c");
        assert_eq!(
            find(&s, "normal", "space l").unwrap().desc,
            r#":echo "a long rhs with spaces"<CR>"#
        );
        assert!(find(&s, "normal", "space a b c d e f g h i j k l").is_some());
        assert_eq!(find(&s, "normal", "space s").unwrap().desc, "*");
        assert_eq!(find(&s, "normal", "space n").unwrap().desc, "<Nop>");
        // Modes: blank, `nox`, `v`, `x`, `s`, `o`, `!`, `i`, `c`, `t`, `l`.
        assert_eq!(
            modes_of(find(&s, "normal", "g h").unwrap()),
            ["normal", "visual", "operator"]
        );
        assert_eq!(
            modes_of(find(&s, "normal", "Q").unwrap()),
            ["normal", "visual", "operator"]
        );
        assert_eq!(
            modes_of(find(&s, "visual", "space v").unwrap()),
            ["visual", "select"]
        );
        assert_eq!(modes_of(find(&s, "visual", "space y").unwrap()), ["visual"]);
        assert_eq!(modes_of(find(&s, "select", "ctrl+s").unwrap()), ["select"]);
        assert_eq!(modes_of(find(&s, "operator", "i p").unwrap()), ["operator"]);
        assert_eq!(
            modes_of(find(&s, "insert", "ctrl+a").unwrap()),
            ["insert", "command"]
        );
        assert_eq!(modes_of(find(&s, "insert", "j k").unwrap()), ["insert"]);
        assert_eq!(
            modes_of(find(&s, "command", "ctrl+e").unwrap()),
            ["command"]
        );
        assert_eq!(
            find(&s, "terminal", "Escape Escape").unwrap().desc,
            r"<C-\><C-N>"
        );
        assert_eq!(modes_of(find(&s, "language", "a a").unwrap()), ["language"]);
        // Left out: `<Plug>` on either side, `<SNR>`, buffer-local; the `é` map (no keysym `nvim::key` knows: it
        // reads ASCII keys only) is skipped and counted.
        let all: Vec<&Bind> = s.binds.values().flatten().collect();
        for gone in ["space p", "space h", "space b"] {
            assert!(all.iter().all(|b| b.key != gone), "{gone}");
        }
        assert!(all.iter().all(|b| !b.desc.contains("<Plug>")));
        assert_eq!(skipped, 1);
        assert_eq!(all.len(), 17, "{all:#?}");
    }

    #[test]
    fn columns_at_the_lhs_width_boundary_and_odd_flags() {
        // From real vim 9.2: an lhs of 10, 11 and 12 characters; `&` (remaps script-local maps only); an empty rhs;
        // `ov` puts visual first.
        let text = "n  abcdefghij  * x\nn  abcdefghijk * y\nn  abcdefghijkl * z\nn  m1          & q\nn  m2          * \nov m3            w\n";
        let (s, skipped) = parse(text).unwrap();
        let desc = |k: &str| find(&s, "normal", k).map(|b| b.desc.clone());
        assert_eq!(desc("a b c d e f g h i j").as_deref(), Some("x"));
        assert_eq!(desc("a b c d e f g h i j k").as_deref(), Some("y"));
        assert_eq!(desc("a b c d e f g h i j k l").as_deref(), Some("z"));
        assert_eq!(desc("m 1").as_deref(), Some("q"));
        assert_eq!(skipped, 1, "the empty rhs");
        assert_eq!(
            modes_of(find(&s, "visual", "m 3").unwrap()),
            ["visual", "select", "operator"]
        );
    }

    #[test]
    fn empty_or_no_maps() {
        assert!(parse("").unwrap().0.binds.is_empty());
        let (s, skipped) = parse("\nNo mapping found\n\nNo mapping found\n").unwrap();
        assert!(s.binds.is_empty() && skipped == 0);
    }

    #[test]
    fn vimrc_search_order() {
        let base = std::env::temp_dir().join(format!("kbf-vimrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (home, xdg) = (base.join("home"), base.join("xdg"));
        let (h, x) = (home.display().to_string(), xdg.display().to_string());
        let found = |xdg_value: Option<&str>| {
            let e = |k: &str| match k {
                "HOME" => Some(h.clone()),
                "XDG_CONFIG_HOME" => xdg_value.map(str::to_string),
                _ => None,
            };
            vimrc(&e)
        };
        assert_eq!(found(Some(&x)), None);
        let write = |p: &Path| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        };
        // Each later one first, so each earlier one is shown to win.
        write(&home.join(".config/vim/vimrc"));
        assert_eq!(found(None), Some(home.join(".config/vim/vimrc")));
        assert_eq!(found(Some(&x)), None, "XDG_CONFIG_HOME replaces ~/.config");
        // Set but empty, or relative: vim reads no XDG vimrc either.
        assert_eq!(found(Some("")), None);
        assert_eq!(found(Some("rel")), None);
        write(&xdg.join("vim/vimrc"));
        assert_eq!(found(Some(&x)), Some(xdg.join("vim/vimrc")));
        write(&home.join(".vim/vimrc"));
        assert_eq!(found(Some(&x)), Some(home.join(".vim/vimrc")));
        write(&home.join(".vimrc"));
        assert_eq!(found(Some(&x)), Some(home.join(".vimrc")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn argument_lists() {
        let tail = [
            "-c",
            "redir! > /dev/stdout",
            "-c",
            "echo \"KEYBIND-FIND-VIM\"",
            "-c",
            "silent map",
            "-c",
            "silent map!",
            "-c",
            "silent! tmap",
            "-c",
            "silent! lmap",
            "-c",
            "echo \"KEYBIND-FIND-VIM-DONE\" | redir END | qa!",
        ];
        let with = args(Some(Path::new("/h/.vimrc")));
        let want: Vec<&str> = ["-es", "-N", "-i", "NONE", "-u", "/h/.vimrc"]
            .into_iter()
            .chain(tail)
            .collect();
        assert_eq!(with, want);
        let without = args(None);
        let want: Vec<&str> = [
            "-es",
            "-N",
            "-i",
            "NONE",
            "-u",
            "NORC",
            "-c",
            "source $VIMRUNTIME/defaults.vim",
        ]
        .into_iter()
        .chain(tail)
        .collect();
        assert_eq!(without, want);
        // vim takes at most ten `-c`.
        assert!(without.iter().filter(|a| *a == "-c").count() <= 10);
    }

    #[test]
    fn viminit_is_not_read() {
        let err = read("vim", TIMEOUT, |_| {}, &|k| {
            (k == "VIMINIT").then(|| "set nocp".into())
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("VIMINIT"), "{err}");
    }

    #[test]
    fn nvim_or_vim() {
        assert!(is_nvim("NVIM v0.12.6\nBuild type: Release\n"));
        assert!(!is_nvim(
            "VIM - Vi IMproved 9.2 (2026 Feb 14, compiled Oct 12 2026 22:01:04)\n"
        ));
        assert!(!is_nvim(""));
    }

    #[test]
    fn no_vim_on_path() {
        let err = read(
            "vim",
            TIMEOUT,
            |c| {
                c.env("PATH", "/nonexistent-kbf");
            },
            &|_| None,
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains("running vim headless") && err.contains("No such file"),
            "{err}"
        );
        assert!(!err.contains('\n'), "one line: {err}");
        // Which program: vim, else vi (Debian's vim-tiny), else none.
        let dir = std::env::temp_dir().join(format!("kbf-vim-which-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = format!("/nonexistent-kbf::{}", dir.display());
        let env = |k: &str| (k == "PATH").then(|| path.clone());
        assert_eq!(program(&env), None);
        std::fs::write(dir.join("vi"), "").unwrap();
        assert_eq!(program(&env), Some("vi"));
        std::fs::write(dir.join("vim"), "").unwrap();
        assert_eq!(program(&env), Some("vim"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake `vim` printing `body`'s output, in a new directory, written through `sh` (ETXTBSY: see `plugins.rs`'s
    /// `write_script`). `--version` prints `$KBF_VERSION`; the headless run fails (exit 7) unless run from `/`, with
    /// `LC_ALL=C.UTF-8`, `MISE_NOT_FOUND_AUTO_INSTALL=0` and closed stdin.
    fn fake_vim(name: &str, body: &str) -> PathBuf {
        use std::io::Write;
        let bin = std::env::temp_dir().join(format!("kbf-vim-fake-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = --version ] && {{ printf '%s\\n' \"$KBF_VERSION\"; exit 0; }}\n[ \"$PWD\" = / ] && [ \"$LC_ALL\" = C.UTF-8 ] && [ \"$MISE_NOT_FOUND_AUTO_INSTALL\" = 0 ] && ! read -r _ || exit 7\n{body}\n"
        );
        let mut sh = Command::new("sh")
            .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
            .arg(bin.join("vim"))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        sh.stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        assert!(sh.wait().unwrap().success());
        bin
    }

    fn run_fake(bin: &Path) -> anyhow::Result<(Sheet, Vec<String>)> {
        let path = format!("{}:/usr/bin:/bin", bin.display());
        read(
            "vim",
            TIMEOUT,
            |c| {
                c.env("PATH", &path);
            },
            &|_| None,
        )
    }

    #[test]
    fn sentinels() {
        // Map-shaped lines a vimrc printed before the sentinel are ignored; the exit status isn't what counts.
        let ok = fake_vim(
            "ok",
            r#"printf 'n  zz          * printed by a vimrc\n\nKEYBIND-FIND-VIM\n\nn  gh          * ^\n\nKEYBIND-FIND-VIM-DONE'; exit 1"#,
        );
        let (s, notes) = run_fake(&ok).unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(s.binds.values().flatten().count(), 1, "{s:?}");
        assert_eq!(find(&s, "normal", "g h").unwrap().desc, "^");
        // No sentinel (a vimrc that quit, or a vim without +eval: E319): one line with what vim said.
        let tiny = fake_vim(
            "tiny",
            "echo 'E319: Sorry, the command is not available' >&2; exit 1",
        );
        let err = format!("{:#}", run_fake(&tiny).unwrap_err());
        assert!(
            err.contains("printed no maps") && err.contains("E319"),
            "{err}"
        );
        assert!(!err.contains('\n'), "{err}");
        // Started but never ended (killed part way): an error, not a short list.
        let cut = fake_vim(
            "cut",
            r#"printf '\nKEYBIND-FIND-VIM\n\nn  gh          * ^\n'"#,
        );
        let err = format!("{:#}", run_fake(&cut).unwrap_err());
        assert!(err.contains("before it had listed every map"), "{err}");
        for b in [ok, tiny, cut] {
            let _ = std::fs::remove_dir_all(b);
        }
    }

    /// `sheet_with` over `bin` (its `vim` answering `--version` with `version`) as the whole `PATH`.
    fn run_sheet(bin: &Path, version: &str) -> anyhow::Result<(Sheet, Vec<String>)> {
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let ctx = Context {
            window_id: None,
            class: None,
            instance: None,
            title: None,
            providers: vec![],
        };
        let only = bin.display().to_string();
        sheet_with(
            &ctx,
            |c| {
                c.env("PATH", &path).env("KBF_VERSION", version);
            },
            &|k| (k == "PATH").then(|| only.clone()),
        )
    }

    #[test]
    fn which_vim() {
        let bin = fake_vim(
            "which",
            r#"printf '\nKEYBIND-FIND-VIM\n\nn  gh          * ^\n\nKEYBIND-FIND-VIM-DONE'"#,
        );
        // A real vim's banner: the headless run.
        let (s, _) = run_sheet(&bin, "VIM - Vi IMproved 9.2 (2026 Feb 14) +eval").unwrap();
        assert_eq!(s.provider, "vim");
        assert_eq!(find(&s, "normal", "g h").unwrap().desc, "^");
        // vim-tiny: one line naming +eval, before any headless run.
        let err = format!(
            "{:#}",
            run_sheet(&bin, "VIM - Vi IMproved 9.1 -eval -terminal").unwrap_err()
        );
        assert!(err.contains("+eval") && !err.contains('\n'), "{err}");
        // Another editor answering as `vim`.
        let err = format!("{:#}", run_sheet(&bin, "nvi 1.81.6").unwrap_err());
        assert!(err.contains("is not vim"), "{err}");
        // A `vim` that is nvim: nvim's sheet (from the nvim on the machine's PATH).
        if Command::new("nvim").arg("--version").output().is_ok() {
            let (s, _) = run_sheet(&bin, "NVIM v0.12.6").unwrap();
            assert_eq!(s.provider, "nvim");
        }
        // No vim at all: one line naming both.
        let none = std::env::temp_dir().join(format!("kbf-vim-none-{}", std::process::id()));
        std::fs::create_dir_all(&none).unwrap();
        let err = format!("{:#}", run_sheet(&none, "").unwrap_err());
        assert_eq!(err, "neither vim nor vi is on PATH");
        for b in [bin, none] {
            let _ = std::fs::remove_dir_all(b);
        }
    }

    /// A temp HOME holding `vimrc` (if any) as `~/.vimrc`.
    fn home(name: &str, vimrc: Option<&str>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kbf-vim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(text) = vimrc {
            std::fs::write(dir.join(".vimrc"), text).unwrap();
        }
        dir
    }

    /// The real vim with `home` as HOME; `None` and a message when there is no real vim (or `vim` is nvim), except
    /// in CI (`CI` set), where there must be one.
    fn real(home: &Path, timeout: Duration) -> Option<anyhow::Result<(Sheet, Vec<String>)>> {
        let real = Command::new("vim")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .is_some_and(|o| !is_nvim(&String::from_utf8_lossy(&o.stdout)));
        if !real {
            assert!(
                std::env::var_os("CI").is_none(),
                "a real vim is required in CI"
            );
            eprintln!("skipping: vim is not installed (or is nvim)");
            return None;
        }
        let h = home.display().to_string();
        Some(read(
            "vim",
            timeout,
            |c| {
                c.env("HOME", &h)
                    .env_remove("VIMINIT")
                    .env_remove("XDG_CONFIG_HOME");
            },
            &|k| (k == "HOME").then(|| h.clone()),
        ))
    }

    /// A cold vim with plugins on a loaded test machine may need more than the provider's limit.
    const REAL_TIMEOUT: Duration = Duration::from_secs(20);

    #[test]
    fn real_vim_xdg_vimrc_and_myvimrc() {
        let dir = home("xdg", None);
        let vimdir = dir.join(".config/vim");
        std::fs::create_dir_all(&vimdir).unwrap();
        std::fs::write(
            vimdir.join("vimrc"),
            "exe 'source ' . fnamemodify($MYVIMRC, ':h') . '/maps.vim'\n",
        )
        .unwrap();
        std::fs::write(vimdir.join("maps.vim"), "nnoremap zm :echo<CR>\n").unwrap();
        let Some(r) = real(&dir, REAL_TIMEOUT) else {
            return;
        };
        let (s, _) = r.unwrap();
        // Read through `$MYVIMRC`, which `-u` alone leaves unset. (The XDG directory's plugin/ is vim's to load: 9.1.0327
        // and later put it in runtimepath, older vims don't read this vimrc at all.)
        assert!(find(&s, "normal", "z m").is_some(), "{s:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_vim_reads_the_vimrc() {
        let dir = home(
            "maps",
            Some("echo \"a vimrc that prints\"\nlet mapleader = \",\"\nnnoremap <leader>q :q<CR>\ninoremap jk <Esc>\nthisisnotacommand\n"),
        );
        let Some(r) = real(&dir, REAL_TIMEOUT) else {
            return;
        };
        // An error in the vimrc (vim then exits 1) doesn't lose the maps.
        let (s, _) = r.unwrap();
        assert_eq!(find(&s, "normal", "comma q").unwrap().desc, ":q<CR>");
        assert_eq!(find(&s, "insert", "j k").unwrap().desc, "<Esc>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_vim_with_no_vimrc_and_a_slow_one() {
        let empty = home("empty", None);
        let Some(r) = real(&empty, REAL_TIMEOUT) else {
            return;
        };
        // No vimrc: defaults.vim's maps (`Q`), no user maps, no error.
        let (s, notes) = r.unwrap();
        assert!(notes.is_empty(), "{notes:?}");
        assert!(s.binds.values().flatten().any(|b| b.key == "Q"), "{s:?}");
        let slow = home("slow", Some("sleep 5\n"));
        let err = format!(
            "{:#}",
            real(&slow, Duration::from_millis(500))
                .unwrap()
                .unwrap_err()
        );
        assert!(err.contains("did not answer within"), "{err}");
        for d in [empty, slow] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
