//! Key capture (X11): grab the keyboard and report each key press as a chord in the crate's key notation, so the
//! keys xmonad would act on reach us instead.

use crate::sheet;
use anyhow::{bail, Context};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, GrabStatus, KeyButMask};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::CURRENT_TIME;

/// No key press for this long ends the session, so a forgotten capture can't hold the keyboard.
const IDLE: Duration = Duration::from_secs(30);
/// How long to keep asking for the keyboard while another client (dmenu) holds it.
const GRAB_RETRY: Duration = Duration::from_secs(1);

/// Keys that only modify another key: pressing one alone reports nothing.
pub const MODIFIER_KEYS: &[&str] = &[
    "Shift_L",
    "Shift_R",
    "Control_L",
    "Control_R",
    "Alt_L",
    "Alt_R",
    "Meta_L",
    "Meta_R",
    "Super_L",
    "Super_R",
    "Hyper_L",
    "Hyper_R",
    "Caps_Lock",
    "Shift_Lock",
    "Num_Lock",
    "ISO_Level3_Shift",
    "ISO_Level5_Shift",
    "Mode_switch",
    "ISO_Next_Group",
    "ISO_Prev_Group",
    "ISO_Group_Shift",
    "ISO_Level3_Latch",
    "ISO_Level3_Lock",
];

/// What a capture session reports.
pub enum Key<'a> {
    /// The keyboard is ours: capture has really started.
    Grabbed,
    /// A modifier key went down or up; the modifiers now held, as a chord without a key (`super+shift`, or empty).
    Held(&'a str),
    /// A non-modifier key press (`super+shift+Return`).
    Chord(&'a str),
    /// One of the `undo` chords: take back the last chord.
    Undo,
}

/// How a capture session ended.
#[derive(Debug, PartialEq)]
pub enum End {
    /// One of the `text_search` chords (a bare Escape for `capture`).
    TextSearch,
    /// One of the `run` chords.
    Run,
}

/// The chords that end a session or take back a key, in the crate's notation (as `chord_for` reports them).
pub struct Ends<'a> {
    pub text_search: &'a [String],
    pub run: &'a [String],
    pub undo: &'a [String],
}

/// Grabs the keyboard and calls `on_key` with each event until a bare Escape. The grab is released on every way out:
/// Escape, an error (from X or from `on_key`), the 30 s idle limit, a panic (the guard's drop), and failing all of
/// those, the connection closing when the process exits.
pub fn capture(on_key: impl FnMut(Key) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let escape = ["Escape".to_string()];
    let ends = Ends {
        text_search: &escape,
        run: &[],
        undo: &[],
    };
    capture_until(&ends, on_key).map(|_| ())
}

/// `capture`, ending on `ends`' `text_search` or `run` chords instead, and reporting its `undo` chords as `Key::Undo`.
/// Says which ended it.
pub fn capture_until(
    ends: &Ends,
    mut on_key: impl FnMut(Key) -> anyhow::Result<()>,
) -> anyhow::Result<End> {
    check_display(
        std::env::var_os("DISPLAY").as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").as_deref(),
    )?;
    // Before the grab: if xmodmap is missing, the keyboard is never taken.
    let names = keysym_names()?;
    // Without a text_search key nothing could end the session but the idle limit (the config checks this too).
    let stop_keys: Vec<&str> = ends
        .text_search
        .iter()
        .map(|c| crate::config::key_of(c))
        .collect();
    if !names.values().any(|n| stop_keys.contains(&n.as_str())) {
        bail!(
            "the keyboard layout has no {} key (xmodmap -pke), so key capture could not be stopped",
            ends.text_search.join(" or ")
        );
    }
    let (conn, screen) = x11rb::connect(None).context("connecting to the X display")?;
    let root = conn
        .setup()
        .roots
        .get(screen)
        .context("the X server has no such screen")?
        .root;
    let _ungrab = grab(&conn, root)?;
    on_key(Key::Grabbed)?;
    let mut last = Instant::now();
    // X autorepeat sends a release and a press of the same key at the same time. A key already down when the grab
    // landed (the `k` of fzf's `ctrl-k`) repeats the same way, so its repeats are dropped too.
    let mut last_release = None;
    loop {
        match conn.poll_for_event().context("reading X events")? {
            Some(Event::KeyPress(e)) => {
                if last_release.take() == Some((e.detail, e.time)) {
                    continue;
                }
                let name = names.get(&e.detail);
                // A text_search key that didn't end the session because more modifiers are held (one stuck down by a
                // killed xdotool) doesn't hold it open either: pressing it again and again still lets the idle limit
                // fire.
                if name.is_none_or(|n| !stuck_stop(u16::from(e.state), n, ends)) {
                    last = Instant::now();
                }
                // A keycode with no keysym in the layout: nothing to report.
                let Some(name) = name else {
                    continue;
                };
                match chord_for(u16::from(e.state), name) {
                    Some(c) => match hit(&c, ends) {
                        Hit::End(end) => return Ok(end),
                        Hit::Undo => on_key(Key::Undo)?,
                        Hit::Chord => on_key(Key::Chord(&c))?,
                    },
                    None => {
                        if let Some(held) = held_after(u16::from(e.state), name, true) {
                            on_key(Key::Held(&held))?;
                        }
                    }
                }
            }
            Some(Event::KeyRelease(e)) => {
                last_release = Some((e.detail, e.time));
                if let Some(held) = names
                    .get(&e.detail)
                    .and_then(|n| held_after(u16::from(e.state), n, false))
                {
                    on_key(Key::Held(&held))?;
                }
            }
            Some(_) => {}
            None => {
                if last.elapsed() > IDLE {
                    bail!("no key pressed for {} s, stopped listening", IDLE.as_secs());
                }
                // ponytail: polls every 10 ms; a blocking wait would need its own timer thread for the idle limit. The
                // limit is checked only between events, so an `on_chord` that blocks (a full pipe nobody reads) holds
                // the grab until it returns; a watchdog thread that ungrabs would close that if it ever matters.
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// What a chord does in a session.
#[derive(Debug, PartialEq)]
enum Hit {
    End(End),
    Undo,
    Chord,
}

/// Only the chords in `ends` end a session or undo, compared whole: with the defaults `super+Return`, `shift+Escape`
/// and `ctrl+BackSpace` are chords like any other.
fn hit(chord: &str, ends: &Ends) -> Hit {
    let has = |list: &[String]| list.iter().any(|c| c == chord);
    if has(ends.text_search) {
        Hit::End(End::TextSearch)
    } else if has(ends.run) {
        Hit::End(End::Run)
    } else if has(ends.undo) {
        Hit::Undo
    } else {
        Hit::Chord
    }
}

/// A press of a `text_search` chord's key with at least its modifiers held (`super+Escape` for `Escape`): a stop key
/// that didn't stop, which mustn't count as activity. Other presses of that key (a bare `x` for `ctrl+x`) do, and so
/// do the `run` and `undo` chords (`shift+Escape` as undo).
// ponytail: a binding searched with the stop key plus more modifiers (`ctrl+shift+x` for `ctrl+x`) isn't activity
// either; telling it from a stuck modifier would need the modifier's own press events.
fn stuck_stop(state: u16, keysym: &str, ends: &Ends) -> bool {
    let held = mods_in(state);
    let chord = chord_for(state, keysym).unwrap_or_default();
    if ends.run.contains(&chord) || ends.undo.contains(&chord) {
        return false;
    }
    ends.text_search.iter().any(|t| {
        let mut parts: Vec<&str> = t.split('+').collect();
        parts.pop() == Some(keysym) && parts.iter().all(|m| held.contains(m))
    })
}

/// `keys` on Wayland or with no display gets one clear line instead of a connection error.
fn check_display(display: Option<&OsStr>, wayland: Option<&OsStr>) -> anyhow::Result<()> {
    let set = |v: Option<&OsStr>| v.is_some_and(|v| !v.is_empty());
    match (set(display), set(wayland)) {
        (true, _) => Ok(()),
        (false, true) => bail!("key capture needs X11"),
        (false, false) => bail!("key capture needs an X display, and DISPLAY is not set"),
    }
}

/// Takes the keyboard from the root window, which overrides xmonad's passive grabs while it lasts. Another client's
/// active grab (dmenu) makes the request fail, so ask again for up to `GRAB_RETRY`.
fn grab(conn: &RustConnection, root: u32) -> anyhow::Result<Ungrab<'_>> {
    let started = Instant::now();
    loop {
        let status = conn
            .grab_keyboard(false, root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
            .context("asking for the keyboard")?
            .reply()
            .context("grabbing the keyboard")?
            .status;
        if status == GrabStatus::SUCCESS {
            return Ok(Ungrab(conn));
        }
        if started.elapsed() > GRAB_RETRY {
            if status == GrabStatus::ALREADY_GRABBED {
                bail!(
                    "another program has the keyboard (dmenu?); gave up after {} s",
                    GRAB_RETRY.as_secs()
                );
            }
            bail!("the X server refused the keyboard grab ({status:?})");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Releases the keyboard when dropped, including while a panic unwinds (the crate keeps the default `panic = "unwind"`;
/// with `abort`, only the connection closing at exit would release it).
struct Ungrab<'a>(&'a RustConnection);

impl Drop for Ungrab<'_> {
    fn drop(&mut self) {
        // Errors ignored: a dead connection has already released the grab.
        let _ = self.0.ungrab_keyboard(CURRENT_TIME);
        let _ = self.0.flush();
    }
}

/// The modifier bits we report, with their names in the crate's notation.
fn mod_bits() -> [(u16, &'static str); 4] {
    [
        (u16::from(KeyButMask::MOD4), "super"),
        (u16::from(KeyButMask::CONTROL), "ctrl"),
        (u16::from(KeyButMask::MOD1), "alt"),
        (u16::from(KeyButMask::SHIFT), "shift"),
    ]
}

fn mods_in(state: u16) -> Vec<&'static str> {
    mod_bits()
        .into_iter()
        .filter(|&(bit, _)| state & bit != 0)
        .map(|(_, m)| m)
        .collect()
}

/// The chord for a key press: `state`'s Super, Control, Alt and Shift in the crate's order, then the key's base
/// keysym. NumLock, CapsLock and the other mod bits are ignored. `None` for a modifier key on its own.
pub fn chord_for(state: u16, keysym: &str) -> Option<String> {
    if MODIFIER_KEYS.contains(&keysym) {
        return None;
    }
    Some(sheet::chord(&mods_in(state), keysym))
}

/// After a modifier key goes down (`pressed`) or up, the modifiers held, joined with `+` (`super+shift`, or empty).
/// X reports the state from before the event, so the key's own modifier is added or removed here. `None` for any
/// other key, and for modifiers we don't report (CapsLock, AltGr).
// ponytail: releasing one Shift while the other is still down reports Shift as released, until the next event.
fn held_after(state: u16, keysym: &str, pressed: bool) -> Option<String> {
    let name = match keysym {
        "Super_L" | "Super_R" => "super",
        "Control_L" | "Control_R" => "ctrl",
        "Alt_L" | "Alt_R" | "Meta_L" | "Meta_R" => "alt",
        "Shift_L" | "Shift_R" => "shift",
        _ => return None,
    };
    let (bit, _) = mod_bits().into_iter().find(|&(_, m)| m == name)?;
    let state = if pressed { state | bit } else { state & !bit };
    Some(mods_in(state).join("+"))
}

/// The base keysyms the running layout has (`xmodmap -pke`), which `config::load` checks `text_search` against.
pub fn layout_keysyms() -> anyhow::Result<std::collections::HashSet<String>> {
    Ok(keysym_names()?.into_values().collect())
}

/// Keycode → base keysym name, from the running server's layout (`xmodmap -pke`).
fn keysym_names() -> anyhow::Result<HashMap<u8, String>> {
    Ok(parse_keymap(&xmodmap_pke()?))
}

/// Shifted keysym → base keysym for the running layout (`question` → `slash`), so a binding written with the shifted
/// symbol can be matched against a capture, which reports `shift+slash`.
pub fn shifted_keysyms() -> anyhow::Result<HashMap<String, String>> {
    Ok(parse_shifted(&xmodmap_pke()?))
}

fn xmodmap_pke() -> anyhow::Result<String> {
    let out = Command::new("xmodmap")
        .arg("-pke")
        .stdin(Stdio::null())
        .output()
        .context("running xmodmap (is it installed?)")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
        bail!("xmodmap -pke failed ({}): {stderr}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `keycode  61 = slash question` → `question` → `slash`: the second name against the first, when both are real
/// and differ. The first keycode to name a shifted keysym wins. A keysym that is some key's own base keysym (`less`
/// on the ISO key, though it is also Shift+comma) is left alone: written in a binding, it means that key.
fn parse_shifted(text: &str) -> HashMap<String, String> {
    let bases: std::collections::HashSet<String> = parse_keymap(text).into_values().collect();
    let mut map = HashMap::new();
    for line in text.lines() {
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        if lhs.trim().strip_prefix("keycode").is_none() {
            continue;
        }
        let mut names = rhs.split_whitespace();
        if let (Some(base), Some(shifted)) = (names.next(), names.next()) {
            if base != "NoSymbol"
                && shifted != "NoSymbol"
                && base != shifted
                && !bases.contains(shifted)
            {
                map.entry(shifted.to_string())
                    .or_insert_with(|| base.to_string());
            }
        }
    }
    map
}

/// `keycode  38 = a A a A` → 38 → `a`: the first name, which is the unshifted keysym. Keycodes with no names, or
/// whose first is `NoSymbol`, are left out.
fn parse_keymap(text: &str) -> HashMap<u8, String> {
    text.lines()
        .filter_map(|line| {
            let (lhs, rhs) = line.split_once('=')?;
            let code = lhs.trim().strip_prefix("keycode")?.trim().parse().ok()?;
            let name = rhs.split_whitespace().next().filter(|&n| n != "NoSymbol")?;
            Some((code, name.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIFT: u16 = 1;
    const LOCK: u16 = 2;
    const CONTROL: u16 = 4;
    const MOD1: u16 = 8;
    const MOD2: u16 = 16;
    const MOD4: u16 = 64;

    #[test]
    fn chords_follow_the_crate_notation() {
        assert_eq!(chord_for(0, "a").as_deref(), Some("a"));
        assert_eq!(chord_for(MOD4, "slash").as_deref(), Some("super+slash"));
        assert_eq!(
            chord_for(MOD4 | SHIFT, "Return").as_deref(),
            Some("super+shift+Return")
        );
        assert_eq!(chord_for(SHIFT, "slash").as_deref(), Some("shift+slash"));
        assert_eq!(chord_for(SHIFT, "a").as_deref(), Some("shift+a"));
        assert_eq!(
            chord_for(CONTROL | MOD1, "F13").as_deref(),
            Some("ctrl+alt+F13")
        );
        assert_eq!(
            chord_for(SHIFT | MOD1 | CONTROL | MOD4, "a").as_deref(),
            Some("super+ctrl+alt+shift+a")
        );
        assert_eq!(
            chord_for(0, "XF86AudioMute").as_deref(),
            Some("XF86AudioMute")
        );
    }

    #[test]
    fn numlock_and_capslock_are_ignored() {
        assert_eq!(chord_for(MOD2, "a").as_deref(), Some("a"));
        assert_eq!(chord_for(LOCK | MOD4, "a").as_deref(), Some("super+a"));
        assert_eq!(chord_for(MOD2 | LOCK, "Escape").as_deref(), Some("Escape"));
        // Only a bare Escape ends a session; with a modifier it is a chord like any other.
        assert_eq!(chord_for(SHIFT, "Escape").as_deref(), Some("shift+Escape"));
        assert_eq!(chord_for(MOD4, "Escape").as_deref(), Some("super+Escape"));
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_the_configured_chords_end_a_session() {
        let k = crate::config::Keys::default();
        let ends = Ends {
            text_search: &k.text_search,
            run: &k.run,
            undo: &k.undo,
        };
        let end = |state, key| hit(&chord_for(state, key).unwrap(), &ends);
        assert_eq!(end(0, "Escape"), Hit::End(End::TextSearch));
        assert_eq!(end(MOD2, "Return"), Hit::End(End::Run));
        assert_eq!(end(0, "KP_Enter"), Hit::End(End::Run));
        assert_eq!(end(MOD4, "Return"), Hit::Chord, "super+Return is a chord");
        assert_eq!(end(SHIFT, "KP_Enter"), Hit::Chord);
        assert_eq!(end(SHIFT, "Escape"), Hit::Chord);
        assert_eq!(end(0, "BackSpace"), Hit::Undo);
        assert_eq!(
            end(CONTROL, "BackSpace"),
            Hit::Chord,
            "ctrl+BackSpace is a chord"
        );
        assert_eq!(end(0, "a"), Hit::Chord);
        // `keys` (plain `capture`) still prints a bare Return.
        let escape = strings(&["Escape"]);
        let keys = Ends {
            text_search: &escape,
            run: &[],
            undo: &[],
        };
        assert_eq!(hit("Return", &keys), Hit::Chord);
        assert_eq!(hit("BackSpace", &keys), Hit::Chord);
        assert_eq!(hit("Escape", &keys), Hit::End(End::TextSearch));
        // Configured: F1 ends, a bare Escape is a chord.
        let (f1, run, undo) = (strings(&["F1"]), strings(&["ctrl+j"]), strings(&["ctrl+u"]));
        let moved = Ends {
            text_search: &f1,
            run: &run,
            undo: &undo,
        };
        assert_eq!(hit("F1", &moved), Hit::End(End::TextSearch));
        assert_eq!(hit("ctrl+j", &moved), Hit::End(End::Run));
        assert_eq!(hit("ctrl+u", &moved), Hit::Undo);
        assert_eq!(
            hit("BackSpace", &moved),
            Hit::Chord,
            "a bare BackSpace narrows"
        );
        assert_eq!(hit("Escape", &moved), Hit::Chord);
        assert_eq!(hit("Return", &moved), Hit::Chord);
    }

    #[test]
    fn only_a_stop_key_that_did_not_stop_is_not_activity() {
        let (esc, undo) = (strings(&["Escape"]), strings(&["shift+Escape"]));
        let ends = |text_search| Ends {
            text_search,
            run: &[],
            undo: &undo,
        };
        assert!(
            stuck_stop(MOD4, "Escape", &ends(&esc)),
            "super+Escape: Super stuck"
        );
        assert!(stuck_stop(0, "Escape", &ends(&esc)));
        assert!(!stuck_stop(0, "a", &ends(&esc)));
        assert!(
            !stuck_stop(SHIFT, "Escape", &ends(&esc)),
            "undo is activity"
        );
        let ctrl_x = strings(&["ctrl+x"]);
        assert!(
            !stuck_stop(0, "x", &ends(&ctrl_x)),
            "a bare x is an ordinary key"
        );
        assert!(stuck_stop(CONTROL | SHIFT, "x", &ends(&ctrl_x)));
        // NumLock is no modifier.
        assert!(!stuck_stop(MOD2, "x", &ends(&ctrl_x)));
    }

    #[test]
    fn a_modifier_alone_reports_nothing() {
        assert_eq!(chord_for(SHIFT, "Shift_L"), None);
        assert_eq!(chord_for(MOD4, "Super_L"), None);
        assert_eq!(chord_for(0, "Num_Lock"), None);
        assert_eq!(chord_for(MOD1 | SHIFT, "ISO_Next_Group"), None);
    }

    #[test]
    fn held_modifiers_follow_presses_and_releases() {
        assert_eq!(held_after(0, "Super_L", true).as_deref(), Some("super"));
        assert_eq!(
            held_after(MOD4, "Shift_R", true).as_deref(),
            Some("super+shift")
        );
        assert_eq!(
            held_after(MOD4 | SHIFT, "Super_L", false).as_deref(),
            Some("shift")
        );
        assert_eq!(held_after(SHIFT, "Shift_L", false).as_deref(), Some(""));
        // NumLock in the state is not a held modifier; CapsLock is not one we report.
        assert_eq!(held_after(MOD2, "Control_L", true).as_deref(), Some("ctrl"));
        assert_eq!(held_after(0, "Caps_Lock", true), None);
        assert_eq!(held_after(0, "a", true), None);
    }

    #[test]
    fn shifted_keysyms_map_to_their_base() {
        let text = "keycode   9 = Escape NoSymbol Escape\n\
                    keycode  10 = 1 exclam 1 exclam\n\
                    keycode  38 = a A a A\n\
                    keycode  61 = slash question slash question\n\
                    keycode  65 = space\n\
                    keycode  59 = comma less comma less\n\
                    keycode  87 = KP_End KP_1 KP_End KP_1\n\
                    keycode  94 = less greater less greater\n\
                    keycode 200 = 2 exclam\n";
        let m = parse_shifted(text);
        assert_eq!(m["question"], "slash");
        assert_eq!(m["exclam"], "1", "first keycode wins");
        assert!(!m.contains_key("less"), "less is the ISO key's own keysym");
        assert_eq!(m["greater"], "less");
        assert_eq!(m["A"], "a");
        assert_eq!(m["KP_1"], "KP_End");
        assert!(!m.contains_key("NoSymbol") && !m.contains_key("space"));
    }

    #[test]
    fn keymap_takes_the_first_name_per_keycode() {
        let text = "keycode   8 =\n\
                    keycode   9 = Escape NoSymbol Escape\n\
                    keycode  38 = a A a A\n\
                    keycode  61 = slash question slash question\n\
                    keycode  93 = NoSymbol Hangul\n\
                    keycode 121 = XF86AudioMute NoSymbol XF86AudioMute\n\
                    not a keycode line\n\
                    keycode 300 = a\n";
        let names = parse_keymap(text);
        assert_eq!(names.len(), 4, "{names:?}");
        assert_eq!(names[&9], "Escape");
        assert_eq!(names[&38], "a");
        assert_eq!(names[&61], "slash");
        assert_eq!(names[&121], "XF86AudioMute");
    }

    #[test]
    fn no_x_display_is_one_clear_error() {
        let os = |s: &'static str| Some(OsStr::new(s));
        assert!(check_display(os(":0"), None).is_ok());
        assert!(check_display(os(":0"), os("wayland-1")).is_ok());
        let wayland = check_display(None, os("wayland-1")).unwrap_err();
        assert_eq!(wayland.to_string(), "key capture needs X11");
        let none = check_display(os(""), None).unwrap_err();
        assert!(none.to_string().contains("DISPLAY"), "{none}");
    }
}
