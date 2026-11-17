//! awesome: the running window manager's hotkeys, asked over D-Bus the way `awesome-client` asks (awesome v4.3
//! `utils/awesome-client`). The rc.lua is never read or run here: it is Lua, and only awesome knows what it bound.
//!
//! `awful` requires `awful.remote` (`lib/awful/init.lua:52`), which owns `org.awesomewm.awful` on the session bus
//! (`lib/awful/dbus.lua:16`) and answers `org.awesomewm.awful.Remote.Eval` by running the chunk and returning each
//! result as a string, or `Error during execution: …` (`lib/awful/remote.lua:20-45`). The chunk reads
//! `awful.key.hotkeys`, the table `awful.key.new` fills (`lib/awful/key.lua:133-138`) and the hotkeys popup lists
//! (`lib/awful/hotkeys_popup/widget.lua:285`): global and client keys alike, with their descriptions and groups.

use crate::context::{run_for, Context};
use crate::sheet::{chord, Bind, Sheet};
use anyhow::{anyhow, bail};
use std::process::Command;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

/// Sent to awesome: a fixed, read-only chunk (nothing is interpolated into it). Its one result is `N` (entries left
/// out: a `key` that isn't a string, as awesome-git's key groups have, or modifiers that aren't a list of strings),
/// then `;MODS,KEY,DESCRIPTION,GROUP` per entry, each field hex-encoded (the modifiers joined by NUL first), so the
/// reply is `[0-9a-f,;]` only: dbus-send prints a string as it is between quotes (`tools/dbus-print-message.c:324-333`
/// at dbus 1.16.2), and a description with `"` or a newline would be ambiguous. `data.mod`, else awesome-git's
/// `data.modifiers`; `data.key`, else awesome-git's one-key `data.keys = {{key}}` (`lib/awful/key.lua:323-330` at master
/// `1a5163e`, read as its popup reads it, `widget.lua:542-544`; more than one is a key group, left out). A key given as
/// a number is its text, as awesome reads it (`lua_tolstring`, `objects/key.c:337`).
const CHUNK: &str = r#"local function h(s) return (s:gsub(".", function(c) return ("%02x"):format(c:byte()) end)) end
local function t(v) return type(v) == "string" and v or "" end
local o, n = {}, 0
for _, d in pairs(require("awful.key").hotkeys) do
  local m, k = type(d) == "table" and (d.mod or d.modifiers), type(d) == "table" and d.key
  if k == nil and type(d.keys) == "table" and #d.keys == 1 and type(d.keys[1]) == "table" then k = d.keys[1][1] end
  if type(k) == "number" then k = tostring(k) end
  local ok = type(m) == "table" and type(k) == "string"
  for _, x in ipairs(ok and m or {}) do ok = ok and type(x) == "string" end
  if ok then
    o[#o + 1] = h(table.concat(m, "\0")) .. "," .. h(k) .. "," .. h(t(d.description)) .. "," .. h(t(d.group))
  else
    n = n + 1
  end
end
return n .. (#o > 0 and ";" .. table.concat(o, ";") or "")"#;

pub fn sheet(_ctx: &Context) -> anyhow::Result<Sheet> {
    let (sheet, notes) = read(command())?;
    if !notes.is_empty() {
        eprintln!("keybind-find: awesome: {}", notes.join("; "));
    }
    Ok(sheet)
}

/// `awesome-client`'s call, made directly: no bash, no rlwrap, and a reply timeout.
fn command() -> Command {
    let mut cmd = Command::new("dbus-send");
    cmd.args([
        "--session",
        "--dest=org.awesomewm.awful",
        "--type=method_call",
        "--print-reply",
        "--reply-timeout=2000",
        "/",
        "org.awesomewm.awful.Remote.Eval",
        &format!("string:{CHUNK}"),
    ]);
    cmd
}

/// The sheet and the stderr notes from running `cmd` (the call, with the environment the caller gives it).
fn read(cmd: Command) -> anyhow::Result<(Sheet, Vec<String>)> {
    let out = run_for(cmd, TIMEOUT).map_err(explain)?;
    parse(&out)
}

/// One line naming why awesome couldn't be asked and what fixes it. dbus-send prints `Error NAME: MESSAGE` for an
/// error reply and `Failed to open connection to "session" message bus: …` without a bus
/// (`tools/dbus-send.c:466,708` at dbus 1.16.2).
fn explain(e: anyhow::Error) -> anyhow::Error {
    let io = e.root_cause().downcast_ref::<std::io::Error>();
    if io.is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) {
        return anyhow!("dbus-send is not installed: install D-Bus (awesome depends on it) to list awesome's keys");
    }
    let text = format!("{e:#}");
    if text.contains("Failed to open connection") {
        anyhow!("no D-Bus session bus to ask awesome on: start awesome inside a session bus (dbus-run-session, or the login manager's) ({text})")
    } else if text.contains("org.freedesktop.DBus.Error.ServiceUnknown")
        || text.contains("org.freedesktop.DBus.Error.NameHasNoOwner")
    {
        anyhow!("awesome doesn't answer on D-Bus (no one owns org.awesomewm.awful): its rc.lua must load awful (`require(\"awful\")`), and awesome must be built with D-Bus; or this isn't awesome's session bus (DBUS_SESSION_BUS_ADDRESS)")
    } else if text.contains("org.freedesktop.DBus.Error.NoReply") || text.contains("did not answer")
    {
        anyhow!("awesome did not answer within 2 s over D-Bus: it is busy or hung, or rc.lua starts keybind-find synchronously (os.execute, io.popen), which blocks awesome until it exits; start it with awful.spawn")
    } else {
        e.context("asking awesome over D-Bus")
    }
}

#[derive(Default)]
struct LeftOut {
    /// Entries that aren't one key (awesome-git key groups, odd modifiers), counted by the chunk.
    shapes: usize,
    mods: usize,
    /// Listed keys whose modifiers had a name awesome doesn't know (ignored, as awesome ignores it).
    ignored: usize,
    keycodes: usize,
    names: usize,
}

/// The sheet from dbus-send's `--print-reply` output for `CHUNK`, and the stderr notes (only when anything was left
/// out). An empty reply is an empty sheet.
fn parse(out: &str) -> anyhow::Result<(Sheet, Vec<String>)> {
    let payload = reply_string(out)?;
    let mut sheet = Sheet {
        title: "awesome".into(),
        provider: "awesome".into(),
        binds: Default::default(),
    };
    let mut left = LeftOut::default();
    // ponytail: the chunk always returns its count, so `""` means something other than this chunk answered; kept as an
    // empty sheet because the story's Testing section specifies it. Make it an unexpected reply if that changes.
    if payload.is_empty() {
        return Ok((sheet, Vec::new()));
    }
    let mut entries = payload.split(';');
    left.shapes = entries
        .next()
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| unexpected(payload, "no count of entries left out"))?;
    for entry in entries {
        let fields = entry
            .split(',')
            .map(unhex)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unexpected(payload, "a field isn't hex"))?;
        let [mods, key, desc, group] = fields.as_slice() else {
            return Err(unexpected(payload, "an entry hasn't four fields"));
        };
        // No modifiers is an empty field; an empty name among others is a name awesome doesn't know.
        let mods: Vec<&str> = match mods.as_str() {
            "" => Vec::new(),
            m => m.split('\0').collect(),
        };
        let key = match translate(&mods, key) {
            Ok((k, ignored)) => {
                left.ignored += usize::from(ignored);
                k
            }
            Err(why) => {
                *match why {
                    Why::Modifier => &mut left.mods,
                    Why::Keycode => &mut left.keycodes,
                    Why::Name => &mut left.names,
                } += 1;
                continue;
            }
        };
        let category = if group.is_empty() { "awesome" } else { group };
        let desc = if desc.is_empty() {
            "no description"
        } else {
            desc
        };
        // Duplicates are kept: `awful.key.hotkeys` holds every key made with `awful.key`, as the popup lists it.
        sheet.binds.entry(category.into()).or_default().push(Bind {
            key,
            desc: desc.into(),
            action: String::new(),
            when: Default::default(),
            run: Vec::new(),
        });
    }
    Ok((sheet, left.notes()))
}

impl LeftOut {
    fn notes(&self) -> Vec<String> {
        let parts: Vec<String> = [
            (self.mods, "with a Mod2, Mod3, Mod5, Lock or Any modifier"),
            (self.keycodes, "a keycode other than the number row's"),
            (self.names, "a key name keybind-find can't send"),
            (
                self.shapes,
                "not one key (awesome-git's key groups, or modifiers that aren't strings)",
            ),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, why)| format!("{n} {why}"))
        .collect();
        let mut notes = Vec::new();
        if !parts.is_empty() {
            notes.push(format!("leaving out hotkeys: {}", parts.join(", ")));
        }
        if self.ignored > 0 {
            notes.push(format!(
                "{} hotkeys with an unknown modifier name, ignored as awesome does",
                self.ignored
            ));
        }
        notes
    }
}

/// The text of the reply's one string: `method return …` then `   string "…"` (`tools/dbus-print-message.c:72-75,
/// 324-333,572-576,591`). awesome returns its errors as that string too.
fn reply_string(out: &str) -> anyhow::Result<&str> {
    let body = out
        .split_once('\n')
        .filter(|(head, _)| head.starts_with("method return"))
        .and_then(|(_, body)| body.trim_start().strip_prefix("string \""))
        .and_then(|s| s.trim_end_matches('\n').strip_suffix('"'));
    let Some(payload) = body else {
        return Err(unexpected(out, "not one string"));
    };
    // A run error, or a load error (`[string "…"]:1: …`, `lib/awful/remote.lua:22-25`).
    if payload.starts_with("Error during execution:") || payload.starts_with("[string \"") {
        bail!(
            "awesome's Lua failed: {} (keybind-find's query expects awesome 4.3 or awesome-git; please report it)",
            one_line(payload)
        );
    }
    Ok(payload)
}

fn unexpected(out: &str, why: &str) -> anyhow::Error {
    anyhow!(
        "unexpected reply from awesome ({why}): {} (keybind-find's query expects awesome 4.3 or awesome-git; please report it)",
        one_line(out)
    )
}

/// `text` on one line, cut to 200 characters.
fn one_line(text: &str) -> String {
    let mut s = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((cut, _)) = s.char_indices().nth(200) {
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// Lower-case hex pairs as text; `None` for anything else.
fn unhex(s: &str) -> Option<String> {
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    let bytes = s
        .as_bytes()
        .chunks(2)
        .map(|pair| match pair {
            [hi, lo] => Some(nibble(*hi)? << 4 | nibble(*lo)?),
            _ => None,
        })
        .collect::<Option<Vec<u8>>>()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[derive(Debug, PartialEq)]
enum Why {
    Modifier,
    Keycode,
    Name,
}

/// awesome's modifiers and key in the key notation. The modifier names are X's (`common/xutil.c:31-53` at v4.3, where
/// `Ctrl` is `Control` too). The key is a keysym name, one character (`objects/key.c:98-101`: its byte is the keysym),
/// or `#N`, a keycode (`objects/key.c:103-107`); `#10`…`#19` are the number row, which the stock rc binds tags to
/// (`awesomerc.lua:395-440`) and the hotkeys popup labels `1`…`0` (`lib/awful/hotkeys_popup/widget.lua:139,
/// 165-177`).
/// Also whether a modifier name awesome doesn't know was ignored.
fn translate(mods: &[&str], key: &str) -> Result<(String, bool), Why> {
    let mut out = Vec::with_capacity(mods.len() + 1);
    let mut ignored = false;
    for m in mods {
        out.push(match *m {
            "Mod4" => "super",
            "Mod1" => "alt",
            "Control" | "Ctrl" => "ctrl",
            "Shift" => "shift",
            // Mod2 (num lock), Mod3, Mod5, Lock (caps lock), Any.
            "Mod2" | "Mod3" | "Mod5" | "Lock" | "Any" => return Err(Why::Modifier),
            // `xutil_key_mask_fromstr` gives 0 (`""`, `"Super"`, `"mod4"`): the key fires without it, so it is
            // dropped as awesome drops it.
            _ => {
                ignored = true;
                continue;
            }
        });
    }
    let mut chars = key.chars();
    let sym = match (chars.next(), chars.next()) {
        (Some(c), None) => {
            let s = super::nvim::single(c).ok_or(Why::Name)?;
            // ponytail: awesome 4.3 matches a keysym in column 0 of the keymap only (event.c:769-770), so an upper-case
            // letter (or a shifted symbol, `#`) never fires; listed as `shift+<letter>` all the same, as the story
            // asks. Leave such keys out (or count them) if that changes.
            if c.is_ascii_uppercase() {
                out.push("shift");
                s.to_ascii_lowercase()
            } else {
                s.to_string()
            }
        }
        _ if key.starts_with('#') => match key[1..].parse::<u8>() {
            Ok(n @ 10..=18) => (n - 9).to_string(),
            Ok(19) => "0".into(),
            _ => return Err(Why::Keycode),
        },
        // `"10"`: no keysym (awesome never fires it), and xdotool would read it as keycode 10.
        _ if key.bytes().all(|b| b.is_ascii_digit()) => return Err(Why::Name),
        // ponytail: any `[A-Za-z0-9_]+` name is taken; one X has no keysym for (a typo, `escape`) is listed though
        // awesome gets NoSymbol for it and never fires it. Checking needs X's keysym name table.
        _ if !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') => {
            key.into()
        }
        _ => return Err(Why::Name),
    };
    Ok((chord(&out, &sym), ignored))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/awesome/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn hex(s: &str) -> String {
        s.bytes().map(|b| format!("{b:02x}")).collect()
    }

    /// dbus-send's output for one string reply.
    fn reply(payload: &str) -> String {
        format!("method return time=1763400000.000000 sender=:1.3 -> destination=:1.9 serial=40 reply_serial=2\n   string \"{payload}\"\n")
    }

    /// One entry in the chunk's form.
    fn entry(mods: &[&str], key: &str, desc: &str, group: &str) -> String {
        [hex(&mods.join("\0")), hex(key), hex(desc), hex(group)].join(",")
    }

    fn find<'a>(sheet: &'a Sheet, key: &str) -> Vec<(&'a str, &'a Bind)> {
        sheet
            .binds
            .iter()
            .flat_map(|(c, bs)| bs.iter().map(move |b| (c.as_str(), b)))
            .filter(|(_, b)| b.key == key)
            .collect()
    }

    fn one<'a>(sheet: &'a Sheet, key: &str) -> (&'a str, &'a str) {
        let found = find(sheet, key);
        assert_eq!(found.len(), 1, "{key}: {found:?}");
        (found[0].0, found[0].1.desc.as_str())
    }

    /// `stock.txt`: dbus-send's output for `CHUNK` from awesome 4.3-7 (the e2e image, Arch snapshot 2026/10/30) running
    /// its stock rc.lua (`/etc/xdg/awesome/rc.lua`: v4.3's `awesomerc.lua` without its `@DOC_…@` comment lines).
    #[test]
    fn stock_config() {
        let (sheet, notes) = parse(&fixture("stock.txt")).unwrap();
        assert_eq!(
            (sheet.title.as_str(), sheet.provider.as_str()),
            ("awesome", "awesome")
        );
        // 28 global keys, 36 from the tag loop (`#10`…`#18`), 10 client keys; none left out.
        assert_eq!(sheet.binds.values().flatten().count(), 74);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(one(&sheet, "super+s"), ("awesome", "show help"));
        assert_eq!(one(&sheet, "super+Return"), ("launcher", "open a terminal"));
        assert_eq!(one(&sheet, "super+1"), ("tag", "view tag #1"));
        assert_eq!(
            one(&sheet, "super+ctrl+shift+9"),
            ("tag", "toggle focused client on tag #9")
        );
        assert_eq!(one(&sheet, "super+f"), ("client", "toggle fullscreen"));
        assert!(find(&sheet, "super+0").is_empty());
        assert!(sheet.binds.values().flatten().all(|b| b.run.is_empty()));
    }

    #[test]
    fn edge_replies() {
        // `string ""`: an empty sheet, no note. Also the chunk's real reply for an empty table.
        let (sheet, notes) = parse(&reply("")).unwrap();
        assert!(sheet.binds.is_empty() && notes.is_empty());
        let (sheet, notes) = parse(&reply("0")).unwrap();
        assert!(sheet.binds.is_empty() && notes.is_empty());
        let payload = [
            "2".to_string(),
            entry(&["Mod4"], "a", "", ""),
            entry(&["Mod4", "Mod3"], "b", "mod3", "g"),
            entry(&["Mod4"], "#94", "keycode", "g"),
            entry(&["Mod4"], "é", "accent", "g"),
            entry(&["Mod4"], "a", "again", "g"),
            entry(&[], "XF86AudioMute", "mute", "media"),
            entry(&["Mod4", "Super"], "c", "unknown", "g"),
            entry(&["Mod4", ""], "d", "empty", "g"),
        ]
        .join(";");
        let (sheet, notes) = parse(&reply(&payload)).unwrap();
        // No description and no group.
        let a = &sheet.binds["awesome"];
        assert_eq!(
            (a.len(), a[0].key.as_str(), a[0].desc.as_str()),
            (1, "super+a", "no description")
        );
        // Duplicates are kept.
        assert_eq!(sheet.binds["g"][0].desc, "again");
        assert_eq!(sheet.binds.values().flatten().count(), 5);
        assert_eq!(one(&sheet, "super+c"), ("g", "unknown"));
        assert_eq!(one(&sheet, "super+d"), ("g", "empty"));
        // No modifiers.
        assert_eq!(one(&sheet, "XF86AudioMute"), ("media", "mute"));
        assert_eq!(
            notes,
            [
                "leaving out hotkeys: 1 with a Mod2, Mod3, Mod5, Lock or Any modifier, 1 a keycode other than the number row's, 1 a key name keybind-find can't send, 2 not one key (awesome-git's key groups, or modifiers that aren't strings)",
                "2 hotkeys with an unknown modifier name, ignored as awesome does"
            ]
        );
        // A field that isn't hex, a short entry, a missing count.
        for bad in [
            "0;4d6f6434,6g,,",
            "0;4d6f6434,61,",
            "x;",
            ";4d,61,,",
            "0;4D,61,,",
            "+0;",
            "+0",
        ] {
            let e = parse(&reply(bad)).unwrap_err().to_string();
            assert!(
                e.contains("unexpected reply") && e.contains("please report it"),
                "{bad}: {e}"
            );
        }
        assert!(parse(&reply("0;4d6f6434,6,,"))
            .unwrap_err()
            .to_string()
            .contains("isn't hex"));
        // awesome's own error, on more than one line.
        let e = parse(&reply(
            "Error during execution: [string \"...\"]:3: attempt to index a nil value\nstack",
        ))
        .unwrap_err()
        .to_string();
        assert_eq!(
            e,
            "awesome's Lua failed: Error during execution: [string \"...\"]:3: attempt to index a nil value stack (keybind-find's query expects awesome 4.3 or awesome-git; please report it)"
        );
        // A load error (the chunk didn't compile), and a long error cut to 200 characters.
        let e = parse(&reply(
            "[string \"local function h(s)...\"]:1: unexpected symbol",
        ))
        .unwrap_err()
        .to_string();
        assert!(e.starts_with("awesome's Lua failed: [string"), "{e}");
        let long = format!("Error during execution: {}", "x".repeat(500));
        let e = parse(&reply(&long)).unwrap_err().to_string();
        assert!(
            e.contains(&format!("{}…", "x".repeat(176))) && !e.contains(&"x".repeat(177)),
            "{e}"
        );
        // Not dbus-send's reply form.
        for out in [
            "",
            "garbage",
            "method return x\n   double 3\n",
            "signal x\n   string \"0\"\n",
        ] {
            let e = parse(out).unwrap_err().to_string();
            assert!(e.contains("not one string"), "{out}: {e}");
        }
        // Two results.
        assert!(parse(&format!("{}   string \"0\"\n", reply("0"))).is_err());
    }

    #[test]
    fn key_table() {
        for (mods, key, want) in [
            (&["Mod4", "Shift"][..], "Return", Ok("super+shift+Return")),
            (&["Control", "Mod1"], "Delete", Ok("ctrl+alt+Delete")),
            (&["Ctrl"], "x", Ok("ctrl+x")),
            (&["Mod4"], "#19", Ok("super+0")),
            (&["Mod4"], "#10", Ok("super+1")),
            (&["Mod4"], "#18", Ok("super+9")),
            (&[], "XF86AudioRaiseVolume", Ok("XF86AudioRaiseVolume")),
            (&["Mod4"], "J", Ok("super+shift+j")),
            (&["Mod4", "Shift"], "J", Ok("super+shift+j")),
            (&["Mod4"], "/", Ok("super+slash")),
            (&["Mod4"], " ", Ok("super+space")),
            (&["Mod4"], "1", Ok("super+1")),
            (&["Mod4"], "Page_Up", Ok("super+Page_Up")),
            (&["Mod4"], "10", Err(Why::Name)),
            (&["Mod4"], "#", Ok("super+numbersign")),
            (&["Mod2"], "a", Err(Why::Modifier)),
            (&["Mod3"], "a", Err(Why::Modifier)),
            (&["Mod5"], "a", Err(Why::Modifier)),
            (&["Lock"], "a", Err(Why::Modifier)),
            (&["Any"], "a", Err(Why::Modifier)),
            (&["Mod4"], "#9", Err(Why::Keycode)),
            (&["Mod4"], "#20", Err(Why::Keycode)),
            (&["Mod4"], "#94", Err(Why::Keycode)),
            (&["Mod4"], "#x", Err(Why::Keycode)),
            (&["Mod4"], "é", Err(Why::Name)),
            (&["Mod4"], "a-b", Err(Why::Name)),
            (&["Mod4"], "", Err(Why::Name)),
        ] {
            assert_eq!(
                translate(mods, key),
                want.map(|k| (k.to_string(), false)),
                "{mods:?} {key}"
            );
        }
        // A modifier name awesome doesn't know is dropped, as awesome drops it, and said so.
        for mods in [&["Mod4", "Super"][..], &["mod4", "Mod4"], &["Mod4", ""]] {
            assert_eq!(
                translate(mods, "c"),
                Ok(("super+c".to_string(), true)),
                "{mods:?}"
            );
        }
        assert_eq!(translate(&["Super"], "c"), Ok(("c".to_string(), true)));
    }

    #[test]
    fn unhex_only_takes_lower_case_pairs() {
        assert_eq!(unhex("").as_deref(), Some(""));
        assert_eq!(unhex("4d6f6434").as_deref(), Some("Mod4"));
        assert_eq!(unhex("c3a9").as_deref(), Some("é"));
        // Every digit and letter; a stray byte is a replacement character, not an error.
        assert_eq!(
            unhex("303132333435363738393a3b3c3d3e3f").as_deref(),
            Some("0123456789:;<=>?")
        );
        assert_eq!(unhex("09af").as_deref(), Some("\t\u{fffd}"));
        for bad in ["4", "4D", "+1", "zz", "4d6", "g0", "0g", "/0", ":0", "`0"] {
            assert_eq!(unhex(bad), None, "{bad}");
        }
    }

    /// dbus-send's failures, through `run_for` as the real call goes: one line each, naming the fix.
    #[test]
    fn errors_name_the_fix() {
        let fail = |stderr: &str| {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", "printf '%s\\n' \"$1\" >&2; exit 1", "sh", stderr]);
            let e = format!("{:#}", read(cmd).unwrap_err());
            assert!(!e.contains('\n'), "{e}");
            e
        };
        let e = fail("Error org.freedesktop.DBus.Error.ServiceUnknown: The name org.awesomewm.awful was not provided by any .service files");
        assert!(
            e.contains("rc.lua must load awful") && e.contains("DBUS_SESSION_BUS_ADDRESS"),
            "{e}"
        );
        let e = fail("Error org.freedesktop.DBus.Error.NameHasNoOwner: Could not get owner of name 'org.awesomewm.awful': no such name");
        assert!(e.contains("rc.lua must load awful"), "{e}");
        let e = fail("Error org.freedesktop.DBus.Error.NoReply: Did not receive a reply.");
        assert!(
            e.contains("did not answer within 2 s") && e.contains("awful.spawn"),
            "{e}"
        );
        // run_for's own limit: a dbus-send that never returns.
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 5"]);
        let start = std::time::Instant::now();
        let e = format!("{:#}", read(cmd).unwrap_err());
        assert!(e.contains("did not answer within 2 s"), "{e}");
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "{:?}",
            start.elapsed()
        );
        let e = fail("Failed to open connection to \"session\" message bus: Unable to autolaunch a dbus-daemon without a $DISPLAY for X11");
        assert!(e.contains("no D-Bus session bus"), "{e}");
        let e = fail("Error org.freedesktop.DBus.Error.AccessDenied: no");
        assert!(
            e.contains("asking awesome over D-Bus") && e.contains("AccessDenied"),
            "{e}"
        );
        let mut cmd = command();
        cmd.env("PATH", "/nonexistent");
        let e = read(cmd).unwrap_err().to_string();
        assert!(e.contains("dbus-send is not installed"), "{e}");
    }

    fn on_path(program: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(program).is_file()))
    }

    /// `CHUNK` itself, run by a plain Lua over a stub `awful.key` holding each shape the chunk reads; skipped without
    /// a Lua.
    #[test]
    fn the_chunk_in_lua() {
        let Some(lua) = ["lua", "lua5.4", "lua5.3", "luajit"]
            .into_iter()
            .find(|p| on_path(p))
        else {
            eprintln!("skipping: no lua, lua5.4, lua5.3 or luajit installed");
            return;
        };
        let program = format!(
            r##"package.preload["awful.key"] = function() return {{ hotkeys = {{
  {{ mod = {{ "Mod4" }}, key = "s", description = "help", group = "awesome" }},
  {{ mod = {{ "Mod4" }}, modifiers = {{ "Mod4" }}, keys = {{ {{ "y" }} }}, description = "git" }},
  {{ mod = {{ "Mod4" }}, key = 7, description = "numeric" }},
  {{ mod = {{ "Mod4" }}, keys = {{ {{ "#10", 1 }}, {{ "#11", 2 }} }}, keygroup = "numrow" }},
  {{ mod = {{ 1 }}, key = "z" }},
}} }} end
io.write((function()
{CHUNK}
end)())"##
        );
        let out = Command::new(lua).args(["-e", &program]).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let payload = String::from_utf8(out.stdout).unwrap();
        let (sheet, notes) = parse(&reply(&payload)).unwrap();
        assert_eq!(one(&sheet, "super+s"), ("awesome", "help"));
        assert_eq!(one(&sheet, "super+y"), ("awesome", "git"));
        assert_eq!(one(&sheet, "super+7"), ("awesome", "numeric"));
        assert_eq!(sheet.binds.values().flatten().count(), 3);
        assert_eq!(
            notes,
            ["leaving out hotkeys: 2 not one key (awesome-git's key groups, or modifiers that aren't strings)"]
        );
    }

    #[test]
    fn the_chunk_is_sent_as_one_string_argument() {
        assert_eq!(TIMEOUT, Duration::from_millis(2000));
        let cmd = command();
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args[..7],
            [
                "--session",
                "--dest=org.awesomewm.awful",
                "--type=method_call",
                "--print-reply",
                "--reply-timeout=2000",
                "/",
                "org.awesomewm.awful.Remote.Eval"
            ]
        );
        assert_eq!(args[7], format!("string:{CHUNK}"));
        assert_eq!(args.len(), 8);
    }

    /// A real awesome on its own Xvfb and session bus, with an rc.lua that adds one key; skipped unless awesome,
    /// Xvfb, dbus-daemon and dbus-send are installed.
    #[test]
    fn real_awesome() {
        use std::io::{BufRead, BufReader};
        use std::process::Stdio;
        if !["awesome", "Xvfb", "dbus-daemon", "dbus-send"]
            .iter()
            .all(|p| on_path(p))
        {
            eprintln!("skipping: awesome, Xvfb, dbus-daemon or dbus-send not installed");
            return;
        }
        struct KillOnDrop(std::process::Child);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let first_line = |child: &mut std::process::Child| {
            let mut line = String::new();
            let _ = BufReader::new(child.stdout.take().unwrap()).read_line(&mut line);
            line.trim().to_string()
        };
        let mut bus = KillOnDrop(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let address = first_line(&mut bus.0);
        let mut xvfb = KillOnDrop(
            Command::new("Xvfb")
                .args(["-displayfd", "1", "-nolisten", "tcp"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let display = format!(":{}", first_line(&mut xvfb.0));
        if address.is_empty() || display == ":" {
            eprintln!("skipping: no session bus or X display came up");
            return;
        }
        struct RemoveOnDrop(std::path::PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir =
            RemoveOnDrop(std::env::temp_dir().join(format!("kbf-awesome-{}", std::process::id())));
        let dir = &dir.0;
        std::fs::create_dir_all(dir).unwrap();
        let rc = dir.join("rc.lua");
        // One key, a numeric key, and two hand-made entries: awesome-git's shape, and a key group (left out).
        std::fs::write(
            &rc,
            r##"local awful = require("awful")
root.keys(awful.key({ "Mod4", "Shift" }, "x", function() end, { description = "real test", group = "test" }))
awful.key({ "Mod4" }, 7, function() end, { description = "numeric", group = "test" })
local hotkeys = require("awful.key").hotkeys
table.insert(hotkeys, { mod = { "Mod4" }, modifiers = { "Mod4" }, keys = { { "y" } }, description = "git" })
table.insert(hotkeys, { mod = { "Mod4" }, keys = { { "#10", 1 }, { "#11", 2 } }, keygroup = "numrow" })
"##,
        )
        .unwrap();
        let mut wm = KillOnDrop(
            Command::new("awesome")
                .arg("-c")
                .arg(&rc)
                .env("DISPLAY", &display)
                .env("DBUS_SESSION_BUS_ADDRESS", &address)
                .env("HOME", dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let got = loop {
            let mut cmd = command();
            cmd.env("DBUS_SESSION_BUS_ADDRESS", &address);
            let got = read(cmd);
            if got.is_ok() || std::time::Instant::now() > deadline {
                break got;
            }
            if let Ok(Some(status)) = wm.0.try_wait() {
                panic!("awesome exited: {status}");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (sheet, notes) = got.unwrap();
        assert_eq!(one(&sheet, "super+shift+x"), ("test", "real test"));
        assert_eq!(one(&sheet, "super+7"), ("test", "numeric"));
        assert_eq!(one(&sheet, "super+y"), ("awesome", "git"));
        assert_eq!(
            notes,
            ["leaving out hotkeys: 1 not one key (awesome-git's key groups, or modifiers that aren't strings)"]
        );
    }
}
