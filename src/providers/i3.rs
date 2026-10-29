//! i3: the bindings of the config i3 has loaded, from `i3-msg -t get_config`, read with sway's parser. The config files
//! on disk are never read: i3 has already followed `include` and replaced its variables.

use super::sway::{file_lines, parse_as};
use crate::context::{run_for, Context};
use crate::sheet::Sheet;
use anyhow::{bail, Context as _};
use serde::Deserialize;
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(1);

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let (sheet, notes) = read(|cmd| run_for(cmd, TIMEOUT))?;
    for note in notes {
        eprintln!("keybind-find: i3: {note}");
    }
    Ok(sheet)
}

/// Asks i3 for its config through `run` (a test seam; `i3-msg` finds the socket from `I3SOCK` or the X root window,
/// so nothing is passed) and parses the reply. Also returns the lines for stderr.
fn read(run: impl Fn(Command) -> anyhow::Result<String>) -> anyhow::Result<(Sheet, Vec<String>)> {
    let mut cmd = Command::new("i3-msg");
    cmd.args(["-t", "get_config"]);
    let reply = run(cmd).context("running i3-msg -t get_config")?;
    from_reply(&reply)
}

/// A GET_CONFIG reply: `config` is the top-level file (all of it before i3 4.20, which has no `include`);
/// `included_configs` (4.20+) is every file i3 loaded, in order.
#[derive(Deserialize)]
struct Reply {
    config: Option<String>,
    included_configs: Option<Vec<Included>>,
}

#[derive(Deserialize)]
struct Included {
    path: Option<String>,
    variable_replaced_contents: String,
}

fn from_reply(reply: &str) -> anyhow::Result<(Sheet, Vec<String>)> {
    let reply: Reply = serde_json::from_str(reply).context("reading i3-msg's reply")?;
    // Each file on its own, as i3 parses them: an unclosed block ends with its file.
    let files: Vec<(String, String)> = match (reply.included_configs, reply.config) {
        (Some(included), _) if !included.is_empty() => included
            .into_iter()
            .map(|f| {
                let path = f.path.unwrap_or_else(|| "an included config".into());
                (path, f.variable_replaced_contents)
            })
            .collect(),
        (_, Some(config)) => vec![("i3's config".into(), config)],
        _ => bail!("i3-msg -t get_config replied with neither `included_configs` nor `config`"),
    };
    let mut notes = Vec::new();
    let mut lines = Vec::new();
    for (label, text) in &files {
        lines.extend(file_lines(label, text, &mut notes));
    }
    // Keycodes have no key name to show or send.
    let codes = lines
        .iter()
        .filter(|l| l.split_whitespace().next() == Some("bindcode"))
        .count();
    if codes > 0 {
        let lines = if codes == 1 { "line" } else { "lines" };
        notes.push(format!(
            "leaving out {codes} bindcode {lines} (keycodes have no key name)"
        ));
    }
    let sheet = parse_as("i3", &lines.join("\n"))?;
    if sheet.binds.is_empty() {
        bail!("i3's config has no bindsym lines");
    }
    Ok((sheet, notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::Bind;
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/i3")
                .join(name),
        )
        .unwrap()
    }

    fn find<'a>(s: &'a Sheet, category: &str, key: &str) -> Option<&'a Bind> {
        s.binds.get(category)?.iter().find(|b| b.key == key)
    }

    /// `get_config-4.25.1.json`: its first file is i3's `etc/config` at tag 4.25.1 (commit `254879d`) plus an
    /// `include`, with variables replaced as i3 replaces them (`set` lines too); the second a synthetic include.
    #[test]
    fn reply_with_included_configs() {
        let (s, notes) = from_reply(&fixture("get_config-4.25.1.json")).unwrap();
        assert_eq!((s.title.as_str(), s.provider.as_str()), ("i3", "i3"));
        assert_eq!(
            find(&s, "i3", "alt+Return").unwrap().desc,
            "exec i3-sensible-terminal"
        );
        // `$ws1` replaced by i3, `$left` (j) too.
        assert_eq!(
            find(&s, "i3", "alt+1").unwrap().desc,
            "workspace number \"1\""
        );
        assert_eq!(find(&s, "i3", "alt+j").unwrap().desc, "focus left");
        assert_eq!(
            find(&s, "i3", "XF86AudioRaiseVolume").unwrap().desc,
            "exec --no-startup-id pactl set-sink-volume @DEFAULT_SINK@ +10% && killall -SIGUSR1 i3status"
        );
        assert_eq!(
            find(&s, "resize", "j").unwrap().desc,
            "resize shrink width 10 px or 10 ppt"
        );
        assert_eq!(
            find(&s, "resize", "Escape").unwrap().desc,
            "mode \"default\""
        );
        // The second file: its top-level bind in `i3`, its unclosed mode's in `launch`, closed at its end.
        assert_eq!(find(&s, "i3", "alt+F2").unwrap().desc, "exec i3-input");
        assert_eq!(find(&s, "launch", "f").unwrap().desc, "exec firefox");
        assert_eq!(
            s.binds.keys().collect::<Vec<_>>(),
            ["i3", "launch", "resize"]
        );
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(
            notes[0],
            "/home/user/.config/i3/extra.conf: 1 block(s) not closed"
        );
        assert_eq!(
            notes[1],
            "leaving out 1 bindcode line (keycodes have no key name)"
        );
    }

    #[test]
    fn an_unclosed_block_ends_with_its_file() {
        let reply = serde_json::json!({"included_configs": [
            {"path": "a", "variable_replaced_contents": "mode \"m\" {\nbindsym x exec a\n"},
            {"path": "b", "variable_replaced_contents": "}\nbindsym y exec b\n"},
        ]});
        let (s, notes) = from_reply(&reply.to_string()).unwrap();
        assert_eq!(find(&s, "m", "x").unwrap().desc, "exec a");
        assert_eq!(find(&s, "i3", "y").unwrap().desc, "exec b");
        assert_eq!(
            notes,
            [
                "a: 1 block(s) not closed",
                "b: a `}` with no block to close"
            ]
        );
    }

    #[test]
    fn reply_before_4_20() {
        let (s, notes) = from_reply(&fixture("get_config-old.json")).unwrap();
        // Raw `set $mod`, substituted by the parser.
        assert_eq!(
            find(&s, "i3", "alt+Return").unwrap().desc,
            "exec i3-sensible-terminal"
        );
        assert_eq!(find(&s, "i3", "alt+d").unwrap().desc, "exec dmenu_run");
        assert_eq!(s.binds["i3"].len(), 2);
        assert_eq!(
            notes,
            ["leaving out 1 bindcode line (keycodes have no key name)"]
        );
        // An empty `included_configs` is read as no such field.
        let reply = serde_json::json!({"config": "bindsym a exec x", "included_configs": []});
        assert!(find(&from_reply(&reply.to_string()).unwrap().0, "i3", "a").is_some());
    }

    #[test]
    fn malformed_replies_are_errors() {
        for (reply, want) in [
            ("not json", "reading i3-msg's reply"),
            ("{}", "neither `included_configs` nor `config`"),
            (
                r#"{"config": "bar {\n}\nbindcode 24 kill\n"}"#,
                "has no bindsym lines",
            ),
            (
                r#"{"included_configs": [{"path": "a"}]}"#,
                "reading i3-msg's reply",
            ),
        ] {
            let err = from_reply(reply).unwrap_err();
            assert!(format!("{err:#}").contains(want), "{reply}: {err:#}");
        }
    }

    #[test]
    fn runs_i3_msg_get_config() {
        let seen = std::cell::RefCell::new(None);
        let (s, _) = read(|cmd| {
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            *seen.borrow_mut() = Some((cmd.get_program().to_string_lossy().into_owned(), args));
            Ok(r#"{"config": "bindsym a exec x"}"#.into())
        })
        .unwrap();
        assert_eq!(
            seen.into_inner(),
            Some(("i3-msg".into(), vec!["-t".into(), "get_config".into()]))
        );
        assert!(find(&s, "i3", "a").is_some());
        let err = read(|_| bail!("no socket")).unwrap_err();
        assert!(
            format!("{err:#}").contains("running i3-msg -t get_config: no socket"),
            "{err:#}"
        );
    }

    /// Kills (and reaps) a test's child when dropped, so a failed assert leaves nothing running.
    struct KillOnDrop(std::process::Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A real i3 on Xvfb with a temp config that `include`s a second file; skipped, with a message, without `i3`,
    /// `i3-msg` or `Xvfb`.
    #[test]
    fn real_i3_lists_an_included_bind() {
        use std::io::BufRead;
        use std::process::Stdio;
        let have = |tool: &str| {
            Command::new("sh")
                .args(["-c", "command -v \"$1\"", "sh", tool])
                .stdout(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if let Some(missing) = ["i3", "i3-msg", "Xvfb"].into_iter().find(|t| !have(t)) {
            eprintln!("skipping real_i3_lists_an_included_bind: no {missing}");
            return;
        }
        // Short: a socket path must fit in 108 bytes.
        let tmp = PathBuf::from(format!("/tmp/kbf-i3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let sock = tmp.join("i3.sock");
        std::fs::write(
            tmp.join("extra.conf"),
            "bindsym Mod1+F9 exec included-bind\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("config"),
            format!(
                "ipc-socket {}\ninclude {}\nbindsym Mod1+Return exec true\n",
                sock.display(),
                tmp.join("extra.conf").display()
            ),
        )
        .unwrap();
        let mut xvfb = KillOnDrop(
            Command::new("Xvfb")
                .args(["-displayfd", "1", "-nolisten", "tcp"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut number = String::new();
        std::io::BufReader::new(xvfb.0.stdout.take().unwrap())
            .read_line(&mut number)
            .unwrap();
        let display = format!(":{}", number.trim());
        assert!(display.len() > 1, "Xvfb printed no display number");
        let _i3 = KillOnDrop(
            Command::new("i3")
                .arg("-c")
                .arg(tmp.join("config"))
                .env("DISPLAY", &display)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut got = None;
        for _ in 0..100 {
            if sock.exists() {
                got = Some(read(|mut cmd| {
                    cmd.env("I3SOCK", &sock);
                    run_for(cmd, Duration::from_secs(5))
                }));
                if got.as_ref().is_some_and(|r| r.is_ok()) {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let (s, _) = got.expect("i3 never opened its socket").unwrap();
        assert_eq!(find(&s, "i3", "alt+F9").unwrap().desc, "exec included-bind");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
