//! Emacs: the keys active in the selected window's buffer of the running Emacs, asked over its server socket with
//! `emacsclient --eval`, with its major mode and evil state as facts. With no server to ask, the global map (and evil's
//! state maps) of the user's init file, from `emacs --batch`.

use crate::context::Context;
use crate::sheet::{Bind, Name, Sheet};
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let ask_it = crate::context::editor_pid().map(target);
    let (sheet, facts, notes) = read(ask_it, framework(&home, &config), |_| {})?;
    if let Some(facts) = facts {
        crate::context::set_facts(facts);
    }
    for note in notes {
        eprintln!("keybind-find: emacs: {note}");
    }
    Ok(sheet)
}

/// The sheet, the facts (from a live answer) and the notes for stderr. With `ask_it` (the sockets to try and the pid
/// that must answer), the running Emacs; no answer from it, silently, or a failure, with a note, falls back to the batch
/// read. `setup` (a test seam) is applied to each command.
fn read(
    ask_it: Option<(Vec<PathBuf>, Option<u32>)>,
    framework: Option<(&str, PathBuf)>,
    setup: impl Fn(&mut Command),
) -> anyhow::Result<(Sheet, Option<crate::scope::Facts>, Vec<String>)> {
    let mut notes = Vec::new();
    if let Some((sockets, want)) = ask_it {
        match ask(&sockets, want, &setup) {
            Ok(Some(answer)) => {
                let (sheet, facts, left_out) = parse(answer, true)?;
                notes.extend(left_out_note(left_out));
                return Ok((sheet, Some(facts), notes));
            }
            Ok(None) => {}
            Err(e) => notes.push(format!(
                "the running Emacs: {e:#}; reading the init file in --batch instead"
            )),
        }
    }
    let (sheet, more) = batch(framework, &setup)?;
    notes.extend(more);
    Ok((sheet, None, notes))
}

fn left_out_note(left_out: usize) -> Option<String> {
    (left_out > 0).then(|| {
        format!(
            "leaving out {left_out} key(s) keybind-find can't send (hyper, alt-as-A- or a key with no X keysym)"
        )
    })
}

/// The sockets to try for the Emacs whose pid (an `emacs`, or the `emacsclient` talking to one) is `pid`, and the pid
/// that must answer: an emacs must be the one; an emacsclient talks to some server, so any.
fn target(pid: u32) -> (Vec<PathBuf>, Option<u32>) {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
    let cmdline: Vec<String> = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|b| {
            b.split(|&c| c == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .unwrap_or_default();
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let uid = std::fs::metadata("/proc/self").ok().map(|m| m.uid());
    let dir = server_dir(env("XDG_RUNTIME_DIR"), env("TMPDIR"), uid);
    let client = comm.trim_end() == "emacsclient";
    // emacsclient with no `-s` talks to `$EMACS_SOCKET_NAME`, else `server`.
    let default = client.then(|| {
        std::fs::read(format!("/proc/{pid}/environ"))
            .ok()
            .and_then(|e| {
                e.split(|&b| b == 0)
                    .find_map(|v| v.strip_prefix(b"EMACS_SOCKET_NAME="))
                    .filter(|v| !v.is_empty())
                    .map(|v| String::from_utf8_lossy(v).into_owned())
            })
            .unwrap_or_else(|| "server".into())
    });
    let want = (!client).then_some(pid);
    (
        socket_candidates(&cmdline, dir.as_deref(), default.as_deref()),
        want,
    )
}

/// Emacs's server directory (`server-socket-dir`): `$XDG_RUNTIME_DIR/emacs`, else `${TMPDIR:-/tmp}/emacs<uid>`.
fn server_dir(
    runtime: Option<std::ffi::OsString>,
    tmpdir: Option<std::ffi::OsString>,
    uid: Option<u32>,
) -> Option<PathBuf> {
    if let Some(runtime) = runtime {
        return Some(PathBuf::from(runtime).join("emacs"));
    }
    let tmp = PathBuf::from(tmpdir.unwrap_or_else(|| "/tmp".into()));
    Some(tmp.join(format!("emacs{}", uid?)))
}

/// The sockets to try, in order: the one named in the command line (`--daemon=NAME`, `--fg-daemon=NAME`,
/// `--bg-daemon=NAME`, or emacsclient's `-s NAME`, `-sNAME`, `--socket-name=NAME`), else `default`; a NAME without `/`
/// is a file in `dir`. Then every socket in `dir`.
fn socket_candidates(
    cmdline: &[String],
    dir: Option<&Path>,
    default: Option<&str>,
) -> Vec<PathBuf> {
    let named = cmdline.iter().enumerate().find_map(|(i, a)| {
        [
            "--daemon=",
            "--fg-daemon=",
            "--bg-daemon=",
            "--socket-name=",
        ]
        .iter()
        .find_map(|p| a.strip_prefix(p))
        .or_else(|| {
            matches!(a.as_str(), "-s" | "--socket-name")
                .then(|| cmdline.get(i + 1).map(String::as_str))
                .flatten()
        })
        // getopt's attached form.
        .or_else(|| a.strip_prefix("-s").filter(|_| !a.starts_with("--")))
        .filter(|n| !n.is_empty())
    });
    let named = named.or(default).and_then(|n| match n.contains('/') {
        true => Some(PathBuf::from(n)),
        false => dir.map(|d| d.join(n)),
    });
    let mut in_dir: Vec<PathBuf> = dir
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_socket()))
        .map(|e| e.path())
        .collect();
    in_dir.sort();
    let mut all: Vec<PathBuf> = named.into_iter().collect();
    for s in in_dir {
        if !all.contains(&s) {
            all.push(s);
        }
    }
    all
}

/// Short, so a dead or busy Emacs falls back to the batch read with time to spare.
const LIVE_TIMEOUT: Duration = Duration::from_secs(2);

/// Asks each socket in turn (`setup`, a test seam, applied to each `emacsclient` command) and returns the first answer
/// from `want` (any answer when `None`). An answer from another Emacs is skipped; when none answers as `want` and one
/// failed, its error.
fn ask(
    sockets: &[PathBuf],
    want: Option<u32>,
    setup: impl Fn(&mut Command),
) -> anyhow::Result<Option<Answer>> {
    let mut failed = None;
    for socket in sockets {
        let mut cmd = Command::new("emacsclient");
        cmd.arg("-s").arg(socket).args(["--eval", LIVE_EXPR]);
        setup(&mut cmd);
        // With `ALTERNATE_EDITOR=""`, emacsclient starts `emacs --daemon` when no server answers.
        // `EMACS_SERVER_FILE` would send it to a TCP server when the socket fails.
        cmd.env_remove("ALTERNATE_EDITOR")
            .env_remove("EMACS_SERVER_FILE")
            .env("MISE_NOT_FOUND_AUTO_INSTALL", "0");
        let answer = crate::context::run_for(cmd, LIVE_TIMEOUT)
            .and_then(|out| unquote(&out))
            .and_then(|json| serde_json::from_str::<Answer>(&json).context("parsing its answer"));
        match answer {
            Ok(a) if want.is_none_or(|w| a.pid == Some(w)) => return Ok(Some(a)),
            Ok(_) => {}
            Err(e) => failed = Some(e.context(format!("asking {}", socket.display()))),
        }
    }
    failed.map_or(Ok(None), Err)
}

/// emacsclient prints an `--eval` result as Emacs prints it: a string comes as a literal (`"{\"g\":…}"` and a
/// newline). Its text, with `\"`, `\\` and `\n` undone; anything else after a backslash is an error.
fn unquote(printed: &str) -> anyhow::Result<String> {
    let Some(rest) = printed.trim().strip_prefix('"') else {
        bail!("Emacs printed {}, not a string", one_line(printed));
    };
    let mut out = String::new();
    let mut chars = rest.chars();
    loop {
        match chars.next() {
            Some('"') => break,
            Some('\\') => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('n') => out.push('\n'),
                Some(c) => {
                    bail!("Emacs printed a string with an escape keybind-find doesn't read: \\{c}")
                }
                None => bail!("Emacs printed an unterminated string"),
            },
            Some(c) => out.push(c),
            None => bail!("Emacs printed an unterminated string"),
        }
    }
    if !chars.as_str().trim().is_empty() {
        bail!("Emacs printed more than one string");
    }
    Ok(out)
}

fn one_line(s: &str) -> String {
    let mut s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((cut, _)) = s.char_indices().nth(200) {
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// Walks the keymaps in `kbf-maps` (`(keymap . source)`, in lookup order) into `[key-description command source]`
/// vectors, breadth first, so a keymap's own prefix comes before its parent's. A key's first binding wins, as Emacs's
/// lookup does; a prefix in one map hides a key of the same name in a
/// later one, and the reverse. Dropped: char ranges, mouse, menu/tool/tab-bar, `remap` and other non-key events,
/// `undefined`, `ignore`, `self-insert-command` and bindings that aren't named commands (keyboard macros, lambdas).
/// A command remapped (`[remap …]`) in the buffer's maps is listed as the command that runs.
/// An autoloaded keymap isn't loaded. It runs the same under dynamic and lexical binding.
macro_rules! walk {
    () => {
        r#"(let ((seen (make-hash-table :test 'equal)) (prefixes (make-hash-table :test 'equal)) (out nil) (stack nil) (source nil) (prefix nil) (depth 0) (path nil) (case-fold-search nil))
  (dolist (m kbf-maps)
    (setq source (cdr m) stack (list (list [] (car m) 0 nil)))
    (while stack
      (let ((top (pop stack)))
        (setq prefix (nth 0 top) depth (nth 2 top) path (cons (indirect-function (nth 1 top)) (nth 3 top)))
        (map-keymap
         (lambda (ev def)
           (unless (or (consp ev) (eq ev t)
                       (and (symbolp ev)
                            (string-match-p "mouse\\|wheel\\|-bar\\|-line\\|fringe\\|margin\\|remap\\|touch\\|pinch\\|drag\\|frame\\|focus\\|select-window\\|sigusr\\|dbus\\|notify\\|help-echo\\|language-change\\|paste\\|compose\\|conversion\\|config-changed\\|ns-"
                                           (symbol-name (event-basic-type ev)))))
             (while (and (consp def) (or (eq (car def) 'menu-item) (stringp (car def))))
               (setq def (if (eq (car def) 'menu-item) (nth 2 def) (cdr def))))
             (let* ((key (vconcat prefix (vector ev))) (desc (key-description key)) (f (and (symbolp def) (indirect-function def))))
               (cond ((null def))
                     ((and (eq (car-safe f) 'autoload) (eq (nth 4 f) 'keymap)) (puthash desc t prefixes))
                     ((keymapp def)
                      (unless (or (gethash desc seen) (> depth 6) (memq (indirect-function def) path))
                        (puthash desc t prefixes)
                        (setq stack (nconc stack (list (list key def (1+ depth) path))))))
                     ((or (gethash desc seen) (gethash desc prefixes)))
                     (t (puthash desc t seen)
                        (let ((cmd (or (and (symbolp def) (command-remapping def)) def)))
                          (when (and (symbolp cmd) (commandp cmd) (not (memq cmd '(undefined ignore self-insert-command))))
                            (push (vector desc (symbol-name cmd) source) out))))))))
         (nth 1 top)))))
  (nreverse out))"#
    };
}

/// Asked of the running Emacs, in the selected window's buffer: its pid, major mode, evil state (when evil is on in
/// that buffer) and `(current-active-maps t)` walked, each bind's source `global`, `major` (`current-local-map`) or
/// `other`. A constant: nothing the user wrote goes into it.
const LIVE_EXPR: &str = concat!(
    r#"(with-current-buffer (window-buffer (selected-window))
  (require 'json)
  (let ((kbf-maps (mapcar (lambda (m) (cons m (cond ((eq m (current-global-map)) "global") ((eq m (current-local-map)) "major") (t "other")))) (current-active-maps t (window-point)))))
    (json-encode (list (cons 'pid (emacs-pid)) (cons 'major (symbol-name major-mode))
                       (cons 'evil (and (bound-and-true-p evil-local-mode) (boundp 'evil-state) (symbol-name evil-state)))
                       (cons 'binds (vconcat
"#,
    walk!(),
    "))))))"
);

const START: &str = "KEYBIND-FIND-EMACS";
const END: &str = "KEYBIND-FIND-EMACS-END";

/// Run by `emacs --batch -q`, after `(let ((kbf-load t|nil))`: with `kbf-load`, `early-init.el`, the installed packages
/// and the init file (`~/.emacs`, as `load` finds it, else `init` in `user-emacs-directory`), an error kept as `error`;
/// then the global map and, with evil loaded, its normal, insert, visual and motion state maps (source: the state),
/// between `START` and `END`. `server-start` does nothing (an advice, which outlives the init loading `server.el`) and `kill-emacs-hook` is cleared, so the init can't touch a
/// running Emacs or write its state files.
const BATCH_BODY: &str = concat!(
    r#"(let ((kbf-error nil) (dir user-emacs-directory))
  (advice-add 'server-start :override #'ignore)
  (when kbf-load
    (condition-case err
        (progn
          (load (expand-file-name "early-init" dir) t t)
          (when (and (fboundp 'package-activate-all) (or (not (boundp 'package-enable-at-startup)) package-enable-at-startup))
            (package-activate-all))
          (let ((init (or (locate-file "~/.emacs" nil (append (get-load-suffixes) '("")))
                          (locate-file (expand-file-name "init" dir) nil (get-load-suffixes)))))
            (when init (setq user-init-file init) (load init nil t t))))
      (error (setq kbf-error (error-message-string err)))))
  (setq kill-emacs-hook nil)
  (require 'json)
  (princ (concat "\nKEYBIND-FIND-EMACS\n"
                 (json-encode (list (cons 'error kbf-error) (cons 'binds
                   (apply #'vconcat
                     (mapcar (lambda (kbf-m) (let ((kbf-maps (list kbf-m))) (vconcat
"#,
    walk!(),
    r#")))
                       (cons (cons (current-global-map) "global")
                             (and (featurep 'evil)
                                  (mapcar (lambda (s) (cons (symbol-value (intern (format "evil-%s-state-map" s))) s))
                                          '("normal" "insert" "visual" "motion")))))))))
                 "\nKEYBIND-FIND-EMACS-END\n"))
  (kill-emacs 0))"#
);

/// A hung init (a package waiting on the network) must not hang the picker.
const BATCH_TIMEOUT: Duration = Duration::from_secs(10);

/// A framework whose init needs its own startup, found in `user-emacs-directory` (`~/.emacs.d` if it exists, else
/// `<config>/emacs`): its name and that directory.
fn framework(home: &Path, config: &Path) -> Option<(&'static str, PathBuf)> {
    let dot = home.join(".emacs.d");
    let dir = if dot.is_dir() {
        dot
    } else {
        config.join("emacs")
    };
    [
        ("Doom Emacs", "lisp/doom.el"),
        ("Spacemacs", "core/core-spacemacs.el"),
    ]
    .into_iter()
    .find(|(_, f)| dir.join(f).is_file())
    .map(|(name, _)| (name, dir))
}

/// The global map (and evil's) from `emacs --batch` (`setup`, a test seam, applied to the command), and the notes for
/// stderr. With a `framework`, its init isn't loaded: Emacs's own global map, with a note.
fn batch(
    framework: Option<(&str, PathBuf)>,
    setup: impl Fn(&mut Command),
) -> anyhow::Result<(Sheet, Vec<String>)> {
    let load = if framework.is_some() { "nil" } else { "t" };
    let expr = format!("(let ((kbf-load {load})) {BATCH_BODY})");
    let mut cmd = Command::new("emacs");
    setup(&mut cmd);
    cmd.args(["--batch", "-q", "--eval", &expr])
        .current_dir("/")
        .env("LC_ALL", "C.UTF-8")
        .env("MISE_NOT_FOUND_AUTO_INSTALL", "0")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("WAYLAND_SOCKET");
    let (_, stdout, stderr) = crate::context::run_for_status(cmd, BATCH_TIMEOUT)?;
    let json = between_sentinels(&stdout).with_context(|| match one_line(&stderr) {
        e if e.is_empty() => "emacs --batch printed no keymap".to_string(),
        e => format!("emacs --batch printed no keymap: {e}"),
    })?;
    let answer: Answer = serde_json::from_str(json).context("parsing emacs --batch's keymap")?;
    let mut notes = Vec::new();
    if let Some((name, dir)) = framework {
        notes.push(format!(
            "{name} found in {}: its init needs its own startup, so only Emacs's own global map is listed",
            dir.display()
        ));
    }
    if let Some(e) = &answer.error {
        notes.push(format!(
            "the init file failed ({}); listing the maps it set up before that",
            one_line(e)
        ));
    }
    let (sheet, _, left_out) = parse(answer, false)?;
    notes.extend(left_out_note(left_out));
    Ok((sheet, notes))
}

/// The text between the last start sentinel and the end sentinel after it (the init may print first).
fn between_sentinels(stdout: &str) -> Option<&str> {
    let at = stdout.rfind(&format!("{START}\n"))? + START.len();
    let rest = &stdout[at..];
    Some(rest[..rest.find(&format!("\n{END}"))?].trim())
}

/// `LIVE_EXPR`'s or the batch read's answer.
#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    major: Option<String>,
    #[serde(default)]
    evil: Option<String>,
    #[serde(default)]
    error: Option<String>,
    binds: Vec<(String, String, String)>,
}

/// The sheet, the facts (`live` only) and how many keys were left out. Categories: `global` (no scope), the major mode
/// (`major-mode = <mode>`), `buffer` (any other active map: `buffer = current`) and, from the batch read, evil's states
/// (`mode = <state>`). Live with evil on, every non-global bind also gets `mode = <state>`. A fact or scope value that
/// isn't a `Name` is left out.
fn parse(answer: Answer, live: bool) -> anyhow::Result<(Sheet, crate::scope::Facts, usize)> {
    let name = |s: &Option<String>| s.as_deref().and_then(|s| s.parse::<Name>().ok());
    let (major, evil) = (name(&answer.major), name(&answer.evil).filter(|_| live));
    let mut sheet = Sheet {
        title: "emacs".into(),
        provider: "emacs".into(),
        binds: Default::default(),
    };
    let mut left_out = 0;
    let mut seen = std::collections::HashSet::new();
    for (desc, command, source) in answer.binds {
        let Some(key) = key(&desc) else {
            left_out += 1;
            continue;
        };
        // `ESC <left>` and `M-<left>` are both alt+Left: the first, as the walk's order is Emacs's lookup order. The
        // batch read's maps are separate (evil's states), so a key is once per map there.
        if !seen.insert((
            if live { String::new() } else { source.clone() },
            key.clone(),
        )) {
            continue;
        }
        let mut when: Vec<(Name, Name)> = Vec::new();
        let category = match source.as_str() {
            "global" => "global".to_string(),
            "major" => {
                if let Some(m) = &major {
                    when.push(("major-mode".parse()?, m.clone()));
                }
                answer.major.clone().unwrap_or_else(|| "major mode".into())
            }
            "other" => {
                when.push(("buffer".parse()?, "current".parse()?));
                "buffer".to_string()
            }
            state => {
                if let Ok(s) = state.parse::<Name>() {
                    when.push(("mode".parse()?, s));
                }
                state.to_string()
            }
        };
        if let (true, Some(e)) = (source != "global", &evil) {
            when.push(("mode".parse()?, e.clone()));
        }
        sheet.binds.entry(category).or_default().push(Bind {
            key,
            desc: command.clone(),
            action: command,
            when: when.into_iter().map(|(k, v)| (k, v.into())).collect(),
        });
    }
    let mut facts = crate::scope::Facts::new();
    if live {
        if let Some(m) = major {
            facts.insert("major-mode".parse()?, m);
        }
        if let Some(e) = evil {
            facts.insert("mode".parse()?, e);
        }
    }
    Ok((sheet, facts, left_out))
}

/// A `key-description` (`C-x C-f`, `M-x`, `C-M-<return>`, `ESC x`) in the key notation; `None` when a key has a
/// hyper (`H-`) or `A-` modifier or a name with no X keysym. A leading `ESC` before another key is alt on that key.
fn key(desc: &str) -> Option<String> {
    let mut chords = Vec::new();
    let mut alt = false;
    let tokens: Vec<&str> = desc.split(' ').collect();
    for (i, token) in tokens.iter().enumerate() {
        if *token == "ESC" && i + 1 < tokens.len() && !alt {
            alt = true;
            continue;
        }
        let mut mods = Vec::new();
        if std::mem::take(&mut alt) {
            mods.push("alt");
        }
        let mut base = *token;
        while base.len() > 2 && base.as_bytes()[1] == b'-' {
            mods.push(match base.as_bytes()[0] {
                b'C' => "ctrl",
                b'M' => "alt",
                b's' => "super",
                b'S' => "shift",
                _ => return None,
            });
            base = &base[2..];
        }
        let keysym = match base {
            "RET" => "Return".to_string(),
            "TAB" => "Tab".into(),
            "SPC" => "space".into(),
            "ESC" => "Escape".into(),
            "DEL" => "BackSpace".into(),
            _ => match base.strip_prefix('<').and_then(|b| b.strip_suffix('>')) {
                Some(name) => named(name)?,
                None => {
                    let mut chars = base.chars();
                    let (Some(c), None) = (chars.next(), chars.next()) else {
                        return None;
                    };
                    if c.is_ascii_uppercase() {
                        mods.push("shift");
                    }
                    super::nvim::single(c.to_ascii_lowercase())?.to_string()
                }
            },
        };
        chords.push(crate::sheet::chord(&mods, &keysym));
    }
    (!chords.is_empty()).then(|| chords.join(" "))
}

/// An Emacs function-key name (`return`, `f5`, `prior`, `kp-add`, `XF86Back`) as an X keysym.
fn named(name: &str) -> Option<String> {
    let keysym = match name {
        "return" => "Return",
        "tab" => "Tab",
        // shift+Tab, as X sends it.
        "backtab" | "iso-lefttab" => "ISO_Left_Tab",
        "backspace" => "BackSpace",
        // `deletechar` is a terminal's Delete key.
        "delete" | "deletechar" => "Delete",
        "escape" => "Escape",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "prior" => "Prior",
        "Scroll_Lock" => "Scroll_Lock",
        "copy" => "XF86Copy",
        "cut" => "XF86Cut",
        "paste" => "XF86Paste",
        "open" => "XF86Open",
        "next" => "Next",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "begin" => "Begin",
        "help" => "Help",
        "menu" => "Menu",
        "undo" => "Undo",
        "redo" => "Redo",
        "find" => "Find",
        "execute" => "Execute",
        "print" => "Print",
        "pause" => "Pause",
        "kp-enter" => "KP_Enter",
        "kp-add" => "KP_Add",
        "kp-subtract" => "KP_Subtract",
        "kp-multiply" => "KP_Multiply",
        "kp-divide" => "KP_Divide",
        "kp-decimal" => "KP_Decimal",
        // Emacs names keys it has no name for by their X keysym.
        x if x.starts_with("XF86") && x.bytes().all(|b| b.is_ascii_alphanumeric()) => x,
        k => {
            if let Some(d) = k
                .strip_prefix("kp-")
                .filter(|d| d.len() == 1 && d.as_bytes()[0].is_ascii_digit())
            {
                return Some(format!("KP_{d}"));
            }
            let n = k
                .strip_prefix('f')?
                .parse::<u8>()
                .ok()
                .filter(|n| (1..=35).contains(n))?;
            return Some(format!("F{n}"));
        }
    };
    Some(keysym.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/emacs")
            .join(name)
    }

    fn answer(name: &str) -> Answer {
        let printed = std::fs::read_to_string(fixture(name)).unwrap();
        serde_json::from_str(&unquote(&printed).unwrap()).unwrap()
    }

    fn get<'a>(s: &'a Sheet, category: &str, key: &str) -> Option<&'a Bind> {
        s.binds.get(category)?.iter().find(|b| b.key == key)
    }

    /// `k=v` for each scope, in order, space-separated.
    fn scopes(b: &Bind) -> String {
        b.when
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}={}",
                    v.iter()
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A temp dir of this test's own.
    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kbf-emacs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A fake `program` running `body` in `dir` (written by `sh`, not by this process: ETXTBSY, see `xfce.rs`).
    fn fake(dir: &Path, program: &str, body: &str) {
        let ok = Command::new("sh")
            .args([
                "-c",
                // The tools it uses, with `PATH` set to the fake's own directory.
                "printf '#!/bin/sh\\nPATH=$PATH:/usr/bin:/bin\\n%s\\n' \"$2\" > \"$1\" && chmod 755 \"$1\"",
                "sh",
            ])
            .arg(dir.join(program))
            .arg(body)
            .status()
            .unwrap();
        assert!(ok.success());
    }

    fn path(dir: &Path) -> impl Fn(&mut Command) {
        let dir = dir.to_path_buf();
        move |c: &mut Command| {
            c.env("PATH", &dir);
        }
    }

    #[test]
    fn live_answer_from_emacs_31() {
        let a = answer("live-31.1.txt");
        assert_eq!(a.pid, Some(571));
        let (s, facts, left_out) = parse(a, true).unwrap();
        assert_eq!((s.title.as_str(), s.provider.as_str()), ("emacs", "emacs"));
        let fact = |k: &str| facts.get(k).map(|v| v.to_string());
        assert_eq!(fact("major-mode").as_deref(), Some("emacs-lisp-mode"));
        assert_eq!(fact("mode"), None);
        // The fixture init binds `C-c k` globally and in a minor mode: the minor mode's map comes first and wins.
        let k = get(&s, "buffer", "ctrl+c k").unwrap();
        assert_eq!(
            (k.desc.as_str(), k.action.as_str()),
            ("kill-line", "kill-line")
        );
        assert_eq!(scopes(k), "buffer=current");
        assert!(get(&s, "global", "ctrl+c k").is_none());
        assert_eq!(
            scopes(get(&s, "buffer", "ctrl+c m").unwrap()),
            "buffer=current"
        );
        let e = get(&s, "emacs-lisp-mode", "ctrl+c e").unwrap();
        assert_eq!(
            (e.desc.as_str(), scopes(e).as_str()),
            ("eval-buffer", "major-mode=emacs-lisp-mode")
        );
        // Global binds have no scope. `ESC <left>` (backward-word) and `M-<left>` (left-word) are both alt+Left: the
        // first in the walk, `M-<left>`, as in Emacs.
        assert_eq!(get(&s, "global", "alt+Left").unwrap().desc, "left-word");
        // A keymap's own prefix before its parent's: emacs-lisp-mode's `C-M-q`, not prog-mode's.
        assert_eq!(
            get(&s, "emacs-lisp-mode", "ctrl+alt+q").unwrap().desc,
            "indent-pp-sexp"
        );
        // The fixture init remaps upcase-word: listed as the command that runs.
        assert_eq!(get(&s, "global", "alt+u").unwrap().desc, "capitalize-word");
        assert_eq!(
            get(&s, "buffer", "ctrl+c m").unwrap().desc,
            "capitalize-word"
        );
        for (key, command) in [
            ("ctrl+x ctrl+f", "find-file"),
            ("alt+x", "execute-extended-command"),
            ("ctrl+x 4 f", "find-file-other-window"),
            ("F3", "kmacro-start-macro-or-insert-counter"),
            ("ctrl+at", "set-mark-command"),
            ("alt+Escape Escape", "keyboard-escape-quit"),
            ("Scroll_Lock", "scroll-lock-mode"),
            ("XF86Copy", "kill-ring-save"),
        ] {
            let b = get(&s, "global", key).unwrap_or_else(|| panic!("{key}"));
            assert_eq!(
                (b.desc.as_str(), scopes(b).as_str()),
                (command, ""),
                "{key}"
            );
        }
        // Each key once: the first map's binding wins.
        let all: Vec<&str> = s.binds.values().flatten().map(|b| b.key.as_str()).collect();
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(all.len(), unique.len());
        // No X keysym: `<again>`, `<deleteline>`, `<insertline>` and `<insertchar>` (plain, `C-`, `S-`).
        assert_eq!(left_out, 6);
    }

    #[test]
    fn live_answer_with_evil() {
        let (s, facts, _) = parse(answer("live-evil.txt"), true).unwrap();
        let fact = |k: &str| facts.get(k).map(|v| v.to_string());
        assert_eq!(fact("mode").as_deref(), Some("normal"));
        assert_eq!(fact("major-mode").as_deref(), Some("emacs-lisp-mode"));
        let j = get(&s, "buffer", "j").unwrap();
        assert_eq!(j.desc, "evil-next-line");
        assert_eq!(scopes(j), "buffer=current mode=normal");
        let e = get(&s, "emacs-lisp-mode", "ctrl+c e").unwrap();
        assert_eq!(scopes(e), "major-mode=emacs-lisp-mode mode=normal");
        assert_eq!(scopes(get(&s, "global", "ctrl+x ctrl+f").unwrap()), "");
        // A value that isn't a `Name` is no fact and no scope.
        let a = Answer {
            pid: None,
            major: Some("weird mode".into()),
            evil: Some(String::new()),
            error: None,
            binds: vec![("C-c a".into(), "x".into(), "major".into())],
        };
        let (s, facts, _) = parse(a, true).unwrap();
        assert!(facts.is_empty());
        assert_eq!(scopes(get(&s, "weird mode", "ctrl+c a").unwrap()), "");
    }

    #[test]
    fn unquoting() {
        assert_eq!(unquote("\"a\\\"b\\\\c\\nd\"\n").unwrap(), "a\"b\\c\nd");
        assert_eq!(unquote("\"\"").unwrap(), "");
        for bad in ["\"a\\tb\"", "\"abc", "\"abc\\", "abc", "nil", "\"a\" \"b\""] {
            assert!(unquote(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keys() {
        for (desc, want) in [
            ("RET", "Return"),
            ("TAB", "Tab"),
            ("SPC", "space"),
            ("ESC", "Escape"),
            ("DEL", "BackSpace"),
            ("<return>", "Return"),
            ("<tab>", "Tab"),
            ("<backspace>", "BackSpace"),
            ("<delete>", "Delete"),
            ("<deletechar>", "Delete"),
            ("<escape>", "Escape"),
            ("<f1>", "F1"),
            ("<f35>", "F35"),
            ("<left>", "Left"),
            ("<right>", "Right"),
            ("<up>", "Up"),
            ("<down>", "Down"),
            ("<prior>", "Prior"),
            ("<next>", "Next"),
            ("<home>", "Home"),
            ("<end>", "End"),
            ("<insert>", "Insert"),
            ("<kp-add>", "KP_Add"),
            ("<kp-7>", "KP_7"),
            ("<XF86Back>", "XF86Back"),
            ("<Scroll_Lock>", "Scroll_Lock"),
            ("<copy>", "XF86Copy"),
            ("C-x C-f", "ctrl+x ctrl+f"),
            ("M-x", "alt+x"),
            ("ESC x", "alt+x"),
            ("ESC ESC ESC", "alt+Escape Escape"),
            ("C-M-<return>", "ctrl+alt+Return"),
            ("s-<left>", "super+Left"),
            ("S-<return>", "shift+Return"),
            ("C-x 4 f", "ctrl+x 4 f"),
            ("C-M-%", "ctrl+alt+percent"),
            ("C--", "ctrl+minus"),
            ("C-@", "ctrl+at"),
            ("A", "shift+a"),
            ("C-S-a", "ctrl+shift+a"),
            ("M-<", "alt+less"),
        ] {
            assert_eq!(key(desc).as_deref(), Some(want), "{desc}");
        }
        for desc in [
            "H-a",
            "A-x",
            "C-x H-a",
            "<f36>",
            "<again>",
            "<mouse-1>",
            "é",
            "",
        ] {
            assert_eq!(key(desc), None, "{desc}");
        }
    }

    #[test]
    fn sockets_to_try() {
        let dir = temp("sockets");
        let _a = std::os::unix::net::UnixListener::bind(dir.join("server")).unwrap();
        let _b = std::os::unix::net::UnixListener::bind(dir.join("work")).unwrap();
        // Not a socket: left out.
        std::fs::write(dir.join("notes"), "").unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (server, work) = (dir.join("server"), dir.join("work"));
        let (server, work) = (server.as_path(), work.as_path());
        assert_eq!(
            socket_candidates(&args(&["emacs"]), Some(&dir), None),
            [server, work]
        );
        assert_eq!(
            socket_candidates(&args(&["emacs", "--daemon=work"]), Some(&dir), None),
            [work, server]
        );
        assert_eq!(
            socket_candidates(&args(&["emacs", "--fg-daemon=/abs/s"]), Some(&dir), None),
            [Path::new("/abs/s"), server, work]
        );
        for client in [
            &["emacsclient", "-t", "-s", "work"][..],
            &["emacsclient", "--socket-name=work", "-t"],
            &["emacsclient", "--socket-name", "work"],
            &["emacsclient", "-swork"],
            &["emacs", "--bg-daemon=work"],
        ] {
            assert_eq!(
                socket_candidates(&args(client), Some(&dir), Some("server")),
                [work, server],
                "{client:?}"
            );
        }
        // emacsclient with no `-s`: its default (`server`, or `$EMACS_SOCKET_NAME`) before the rest; `-t` is no name.
        assert_eq!(
            socket_candidates(&args(&["emacsclient", "-t"]), Some(&dir), Some("work")),
            [work, server]
        );
        assert_eq!(
            socket_candidates(&args(&["emacsclient", "-t"]), Some(&dir), Some("server")),
            [server, work]
        );
        // No directory, no name: nothing.
        assert!(socket_candidates(&args(&["emacs"]), None, None).is_empty());
        assert!(socket_candidates(&args(&["emacs"]), Some(&dir.join("none")), None).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_server_directory() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            server_dir(os("/run/user/1000"), os("/var/tmp"), Some(1000)),
            Some(PathBuf::from("/run/user/1000/emacs"))
        );
        assert_eq!(
            server_dir(None, os("/var/tmp"), Some(1000)),
            Some(PathBuf::from("/var/tmp/emacs1000"))
        );
        assert_eq!(
            server_dir(None, None, Some(1000)),
            Some(PathBuf::from("/tmp/emacs1000"))
        );
        assert_eq!(server_dir(None, None, None), None);
    }

    const ANSWER_7: &str = r#"printf '%s\n' '"{\"pid\":7,\"major\":\"text-mode\",\"evil\":null,\"binds\":[[\"C-c a\",\"seven\",\"global\"]]}"'"#;
    const ANSWER_9: &str = r#"printf '%s\n' '"{\"pid\":9,\"major\":\"text-mode\",\"evil\":null,\"binds\":[[\"C-c a\",\"nine\",\"global\"]]}"'"#;

    /// A fake emacsclient answering per socket name (`$2`), recording its environment.
    fn fake_client(name: &str) -> PathBuf {
        let bin = temp(name);
        fake(
            &bin,
            "emacsclient",
            &format!(
                "env > \"$(dirname \"$0\")/env\"\ncase \"$2\" in\n*/seven) {ANSWER_7} ;;\n*/nine) {ANSWER_9} ;;\n\
                 */refuses) echo \"can't connect\" >&2; exit 1 ;;\n*/slow) sleep 5 ;;\n*/bad) echo 'nil' ;;\nesac"
            ),
        );
        bin
    }

    #[test]
    fn the_answer_of_the_right_emacs() {
        let bin = fake_client("ask");
        let s = |n: &[&str]| n.iter().map(|n| bin.join(n)).collect::<Vec<_>>();
        let command = |a: Option<Answer>| a.map(|a| a.binds[0].1.clone());
        // An emacs must answer itself: another Emacs's answer is skipped.
        let got = ask(&s(&["seven", "nine"]), Some(9), path(&bin)).unwrap();
        assert_eq!(command(got).as_deref(), Some("nine"));
        // An emacsclient: the first answer.
        let got = ask(&s(&["seven", "nine"]), None, path(&bin)).unwrap();
        assert_eq!(command(got).as_deref(), Some("seven"));
        // No socket is its: nothing, silently.
        assert!(ask(&s(&["seven"]), Some(9), path(&bin)).unwrap().is_none());
        assert!(ask(&[], Some(9), path(&bin)).unwrap().is_none());
        // One that refuses is passed over when another answers; alone it is the error.
        let got = ask(&s(&["refuses", "nine"]), Some(9), path(&bin)).unwrap();
        assert_eq!(command(got).as_deref(), Some("nine"));
        let e = format!(
            "{:#}",
            ask(&s(&["refuses"]), Some(9), path(&bin)).unwrap_err()
        );
        assert!(e.contains("refuses") && e.contains("can't connect"), "{e}");
        let e = format!("{:#}", ask(&s(&["bad"]), Some(9), path(&bin)).unwrap_err());
        assert!(e.contains("not a string"), "{e}");
        // ALTERNATE_EDITOR never reaches emacsclient, even set to start a daemon.
        ask(&s(&["seven"]), None, |c: &mut Command| {
            c.env("PATH", &bin).env("ALTERNATE_EDITOR", "");
        })
        .unwrap();
        let env = std::fs::read_to_string(bin.join("env")).unwrap();
        assert!(!env.lines().any(|l| l.starts_with("ALTERNATE_EDITOR=")));
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn a_server_that_hangs_is_an_error() {
        let bin = fake_client("slow");
        let start = std::time::Instant::now();
        let e = ask(&[bin.join("slow")], Some(9), path(&bin)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(4));
        assert!(
            format!("{e:#}").contains("did not answer within 2.0 s"),
            "{e:#}"
        );
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// `sheet`'s path: the running Emacs, else the batch read, with one note only when asking failed.
    #[test]
    fn the_live_read_falls_back_to_batch() {
        let bin = fake_client("read");
        fake(
            &bin,
            "emacs",
            &format!("/bin/cat '{}'", fixture("batch-31.1.txt").display()),
        );
        let s = |n: &[&str]| n.iter().map(|n| bin.join(n)).collect::<Vec<_>>();
        // An answer: its sheet and facts, no batch read.
        let (sheet, facts, notes) = read(Some((s(&["seven"]), Some(7))), None, path(&bin)).unwrap();
        assert_eq!(get(&sheet, "global", "ctrl+c a").unwrap().desc, "seven");
        assert_eq!(
            facts
                .unwrap()
                .get("major-mode")
                .map(|m| m.to_string())
                .as_deref(),
            Some("text-mode")
        );
        assert!(notes.is_empty(), "{notes:?}");
        // No socket, no Emacs found, or only other Emacsen: the batch read, silently (bar its left-out count).
        for ask_it in [Some((vec![], Some(7))), None, Some((s(&["nine"]), Some(7)))] {
            let (sheet, facts, notes) = read(ask_it, None, path(&bin)).unwrap();
            assert!(get(&sheet, "global", "ctrl+c k").is_some());
            assert!(facts.is_none());
            assert!(
                notes.iter().all(|n| n.starts_with("leaving out ")),
                "{notes:?}"
            );
        }
        // A server that refuses or hangs: one note, then the batch read.
        for socket in ["refuses", "slow"] {
            let (sheet, facts, notes) =
                read(Some((s(&[socket]), Some(7))), None, path(&bin)).unwrap();
            assert!(get(&sheet, "global", "ctrl+c k").is_some());
            assert!(facts.is_none());
            assert!(
                notes[0].starts_with("the running Emacs: asking "),
                "{notes:?}"
            );
            assert!(
                notes[0].ends_with("; reading the init file in --batch instead"),
                "{notes:?}"
            );
            assert!(
                notes[1..].iter().all(|n| n.starts_with("leaving out ")),
                "{notes:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn batch_answer_from_emacs_31() {
        let stdout = std::fs::read_to_string(fixture("batch-31.1.txt")).unwrap();
        // The init printed before the start sentinel.
        assert!(stdout.starts_with("noise before"));
        let a: Answer = serde_json::from_str(between_sentinels(&stdout).unwrap()).unwrap();
        assert!(a.error.is_none());
        let (s, facts, left_out) = parse(a, false).unwrap();
        assert!(facts.is_empty());
        assert!(left_out > 0);
        let k = get(&s, "global", "ctrl+c k").unwrap();
        assert_eq!(
            (k.desc.as_str(), scopes(k).as_str()),
            ("kill-whole-line", "")
        );
        assert!(get(&s, "global", "ctrl+x ctrl+f").is_some());
        // No end sentinel: nothing read.
        let cut = &stdout[..stdout.rfind(END).unwrap()];
        assert!(between_sentinels(cut).is_none());
        // evil's state maps are filed under their state, scoped by it; no facts from the batch read.
        let a = Answer {
            pid: None,
            major: None,
            evil: None,
            error: None,
            binds: vec![
                ("j".into(), "evil-next-line".into(), "normal".into()),
                (
                    "C-o".into(),
                    "evil-execute-in-normal-state".into(),
                    "insert".into(),
                ),
            ],
        };
        let (s, facts, _) = parse(a, false).unwrap();
        assert!(facts.is_empty());
        assert_eq!(scopes(get(&s, "normal", "j").unwrap()), "mode=normal");
        assert_eq!(scopes(get(&s, "insert", "ctrl+o").unwrap()), "mode=insert");
    }

    #[test]
    fn batch_runs_emacs_safely() {
        let bin = temp("batch");
        // Records its arguments, environment and directory; prints the fixture.
        fake(
            &bin,
            "emacs",
            &format!(
                "d=$(dirname \"$0\")\nfor a in \"$@\"; do printf '%s\\n--\\n' \"$a\"; done > \"$d/args\"\n\
                 env > \"$d/env\"\npwd > \"$d/pwd\"\n/bin/cat '{}'",
                fixture("batch-31.1.txt").display()
            ),
        );
        let (s, notes) = batch(None, |c: &mut Command| {
            c.env("PATH", &bin)
                .env("DISPLAY", ":9")
                .env("WAYLAND_DISPLAY", "wayland-9")
                .env("WAYLAND_SOCKET", "9");
        })
        .unwrap();
        assert!(get(&s, "global", "ctrl+c k").is_some());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("leaving out "), "{notes:?}");
        let args = std::fs::read_to_string(bin.join("args")).unwrap();
        assert!(
            args.starts_with("--batch\n--\n-q\n--\n--eval\n--\n(let ((kbf-load t)) "),
            "{args}"
        );
        let env = std::fs::read_to_string(bin.join("env")).unwrap();
        let has = |v: &str| env.lines().any(|l| l.starts_with(v));
        assert!(!has("DISPLAY=") && !has("WAYLAND_DISPLAY=") && !has("WAYLAND_SOCKET="));
        assert!(has("LC_ALL=C.UTF-8"));
        assert_eq!(
            std::fs::read_to_string(bin.join("pwd")).unwrap().trim(),
            "/"
        );
        // Doom: its init isn't loaded, said once.
        let (_, notes) =
            batch(Some(("Doom Emacs", "/h/.config/emacs".into())), path(&bin)).unwrap();
        assert!(
            notes[0].starts_with("Doom Emacs found in /h/.config/emacs: "),
            "{notes:?}"
        );
        let args = std::fs::read_to_string(bin.join("args")).unwrap();
        assert!(args.contains("(let ((kbf-load nil)) "), "{args}");
        // An init error is a note; the maps are read all the same.
        let errored = std::fs::read_to_string(fixture("batch-31.1.txt"))
            .unwrap()
            .replace(
                "{\"error\":null",
                "{\"error\":\"Symbol’s value as variable is void: x\"",
            );
        std::fs::write(bin.join("errored.txt"), errored).unwrap();
        fake(
            &bin,
            "emacs",
            &format!("/bin/cat '{}'", bin.join("errored.txt").display()),
        );
        let (s, notes) = batch(None, path(&bin)).unwrap();
        assert!(get(&s, "global", "ctrl+c k").is_some());
        assert!(
            notes[0].starts_with("the init file failed (Symbol’s value"),
            "{notes:?}"
        );
        // No end sentinel, with stderr: one error carrying it; a non-zero exit with both sentinels still reads.
        fake(
            &bin,
            "emacs",
            "echo KEYBIND-FIND-EMACS; echo 'Cannot open load file' >&2; exit 255",
        );
        let e = format!("{:#}", batch(None, path(&bin)).unwrap_err());
        assert!(
            e.contains("printed no keymap: Cannot open load file"),
            "{e}"
        );
        fake(
            &bin,
            "emacs",
            &format!("/bin/cat '{}'; exit 1", fixture("batch-31.1.txt").display()),
        );
        assert!(batch(None, path(&bin)).is_ok());
        let _ = std::fs::remove_dir_all(&bin);
    }

    /// Captured from Emacs 31.1 with evil 1.15.0 and an init that loads evil, then errors: evil's state maps, filed
    /// and scoped by state, and the error as a note.
    #[test]
    fn batch_answer_with_evil_and_an_init_error() {
        let bin = temp("batch-evil");
        fake(
            &bin,
            "emacs",
            &format!("/bin/cat '{}'", fixture("batch-evil-31.1.txt").display()),
        );
        let (s, notes) = batch(None, path(&bin)).unwrap();
        let _ = std::fs::remove_dir_all(&bin);
        assert_eq!(
            notes[0],
            "the init file failed (boom here); listing the maps it set up before that"
        );
        let j = get(&s, "motion", "j").unwrap();
        assert_eq!(
            (j.desc.as_str(), scopes(j).as_str()),
            ("evil-next-line", "mode=motion")
        );
        // One key, one bind per state map, and the global one.
        let o = |c| get(&s, c, "ctrl+o").map(|b| (b.desc.as_str(), scopes(b)));
        assert_eq!(
            o("insert"),
            Some(("evil-execute-in-normal-state", "mode=insert".into()))
        );
        assert_eq!(
            o("motion"),
            Some(("evil-jump-backward", "mode=motion".into()))
        );
        assert_eq!(o("global"), Some(("open-line", String::new())));
        assert!(s.binds.contains_key("normal") && s.binds.contains_key("visual"));
    }

    #[test]
    fn doom_and_spacemacs_are_found() {
        let home = temp("frameworks");
        let config = home.join(".config");
        assert!(framework(&home, &config).is_none());
        let xdg = config.join("emacs");
        std::fs::create_dir_all(xdg.join("lisp")).unwrap();
        std::fs::write(xdg.join("lisp/doom.el"), "").unwrap();
        assert_eq!(framework(&home, &config), Some(("Doom Emacs", xdg.clone())));
        // `~/.emacs.d`, when it exists, is the directory instead.
        let dot = home.join(".emacs.d");
        std::fs::create_dir_all(dot.join("core")).unwrap();
        assert!(framework(&home, &config).is_none());
        std::fs::write(dot.join("core/core-spacemacs.el"), "").unwrap();
        assert_eq!(framework(&home, &config), Some(("Spacemacs", dot)));
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A real Emacs: a daemon with the fixture init answers the live query, and the batch read loads the same init.
    /// Skipped, with a message, without `emacs` and `emacsclient`; not asserted under `CI`, which has no Emacs.
    #[test]
    fn real_emacs() {
        let works = |p: &str| {
            Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !works("emacs") || !works("emacsclient") {
            eprintln!("skipping: emacs or emacsclient is not installed");
            return;
        }
        struct KillOnDrop(std::process::Child);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        // Short: a socket path has a 108-byte limit.
        let tmp = PathBuf::from(format!("/tmp/kbf-e{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join(".emacs.d")).unwrap();
        // Emacs refuses a socket directory others can read (`server-ensure-safe-dir`).
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        // The batch read's init also starts a server, which must stay off: no socket appears.
        let init = std::fs::read_to_string(fixture("init.el")).unwrap();
        std::fs::write(
            home.join(".emacs.d/init.el"),
            format!("{init}\n(require 'server)\n(unless (server-running-p) (server-start))\n"),
        )
        .unwrap();
        let socket = tmp.join("s");
        let env = |c: &mut Command| {
            c.env("HOME", &home)
                .env("XDG_RUNTIME_DIR", &tmp)
                .env("XDG_CONFIG_HOME", tmp.join("config"))
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY");
        };
        let mut daemon = Command::new("emacs");
        daemon
            .arg(format!("--fg-daemon={}", socket.display()))
            .args(["-Q", "-l"])
            .arg(fixture("init.el"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        env(&mut daemon);
        let daemon = KillOnDrop(daemon.spawn().unwrap());
        let start = std::time::Instant::now();
        while !socket.exists() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let live = ask(std::slice::from_ref(&socket), Some(daemon.0.id()), env);
        let batch = batch(None, env);
        let started = tmp.join("emacs/server").exists();
        drop(daemon);
        let _ = std::fs::remove_dir_all(&tmp);
        let checks = || -> anyhow::Result<()> {
            let answer = live?.context("no answer from the daemon")?;
            anyhow::ensure!(
                answer
                    .binds
                    .iter()
                    .all(|(k, _, _)| !["mouse", "menu-bar", "tool-bar", "remap"]
                        .iter()
                        .any(|w| k.contains(w))),
                "a mouse, menu or remap entry reached Rust"
            );
            let (s, facts, _) = parse(answer, true)?;
            anyhow::ensure!(get(&s, "global", "ctrl+x ctrl+f").is_some(), "no C-x C-f");
            // The fixture init's minor-mode `C-c k` wins over its global one; `C-c e` is the major mode's.
            anyhow::ensure!(
                get(&s, "buffer", "ctrl+c k").is_some_and(|b| b.desc == "kill-line")
                    && get(&s, "global", "ctrl+c k").is_none(),
                "the minor mode's C-c k didn't win"
            );
            anyhow::ensure!(
                get(&s, "emacs-lisp-mode", "ctrl+c e").is_some_and(|b| b.desc == "eval-buffer"),
                "no major-mode C-c e"
            );
            // Dropped in the elisp: no self-insert, mouse, menu-bar or `ignore` entries reach Rust.
            let all: Vec<&Bind> = s.binds.values().flatten().collect();
            anyhow::ensure!(
                all.iter()
                    .all(|b| !["self-insert-command", "ignore", "undefined"]
                        .contains(&b.desc.as_str())),
                "a dropped command was listed"
            );
            anyhow::ensure!(
                facts.get("major-mode").map(|m| m.to_string()).as_deref()
                    == Some("emacs-lisp-mode"),
                "{facts:?}"
            );
            anyhow::ensure!(!started, "the batch read started a server");
            let (s, _) = batch?;
            anyhow::ensure!(
                get(&s, "global", "ctrl+c k").is_some_and(|b| b.desc == "kill-whole-line"),
                "the batch read missed the init's key"
            );
            Ok(())
        };
        match checks() {
            Ok(()) => {}
            Err(e) if std::env::var_os("CI").is_some() => eprintln!("real emacs under CI: {e:#}"),
            Err(e) => panic!("{e:#}"),
        }
    }
}
