//! The RemoteDesktop portal, which GNOME and KDE Plasma offer on Wayland in place of the virtual-keyboard protocol
//! wtype needs: a session on the session bus that the desktop lets press keys once its user approves. The approval is
//! kept as a restore token in the state directory, so the dialog shows once, not on every pick.
//!
//! The handshake (xdg-desktop-portal's `org.freedesktop.portal.RemoteDesktop`, interface version 2) runs on one
//! connection, since a session belongs to the connection that created it: `AvailableDeviceTypes`, `CreateSession`,
//! `SelectDevices` (keyboard only, kept until revoked, with the stored token), `Start`, a `NotifyKeyboardKeysym` for
//! each key pressed or released, then `Session.Close`. Keys go as X keysyms (data/keysyms, from xorgproto's
//! keysymdef.h), which the desktop maps through its live keymap.

use crate::sheet::Entry;
use anyhow::{bail, Context as _};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use zbus::export::futures_core::Stream;
use zbus::{Connection, MatchRule, MessageStream, OwnedObjectPath, OwnedVariant, Variant};

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const REMOTE_DESKTOP: &str = "org.freedesktop.portal.RemoteDesktop";
/// `AvailableDeviceTypes` and `SelectDevices`' `types`: 1 KEYBOARD, 2 POINTER, 4 TOUCHSCREEN.
const KEYBOARD: u32 = 1;
/// `persist_mode` 2: until revoked.
const UNTIL_REVOKED: u32 = 2;
/// Every call and every answer but a dialog's.
const STEP: Duration = Duration::from_secs(5);
/// `Start` with no stored token shows a dialog, which a person reads.
const DIALOG: Duration = Duration::from_secs(120);

/// Sends `e`'s keys through the portal: its chords pressed, or its espanso trigger typed (`events`), on a session
/// opened for them and closed after. An entry with a key the table lacks is refused before the portal is asked.
pub fn send(e: &Entry) -> anyhow::Result<()> {
    let events = events(e)?;
    let token_file = token_path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"));
    let session = open(token_file.as_deref())?;
    // ponytail: no deadline for the whole send, only 5 s per NotifyKeyboardKeysym (xdg-desktop-portal answers each at
    // once and passes it on unwaited); a long espanso trigger on a stalled portal could take keys × 5 s.
    let sent = press(&events, |keysym, state| session.notify(keysym, state))
        .context("portal NotifyKeyboardKeysym");
    // A session that won't close ends with keybind-find anyway: said, not the pick's error.
    if let Err(e) = session.close() {
        eprintln!("keybind-find: {e:#}");
    }
    sent
}

/// xorgproto's keysyms: `name 0xvalue` (keysymdef.h, then XF86keysym.h), then `U+XXXX 0xvalue`, the named keysym
/// for a code point.
const KEYSYMS: &str = include_str!("../data/keysyms/xorgproto-2026.1.txt");
/// `NotifyKeyboardKeysym`'s state.
const PRESS: u32 = 1;
const RELEASE: u32 = 0;

/// The keysym named `name`: exactly, else ignoring case as xkbcommon does (`return`, `page_up`); unlike wtype an exact
/// name wins (`Agrave` is À, wtype gives à). Of several names that differ only in case, the one with the fewest
/// capitals (`agrave` for `AGRAVE`). A one-letter name always matches exactly: keysymdef.h has every ASCII letter in
/// both cases.
fn keysym(name: &str) -> Option<u32> {
    // ponytail: a scan of ~3300 lines per name, a few names per pick; a map if a pick ever has hundreds.
    let value = |line: &str, matches: &dyn Fn(&str) -> bool| {
        let (n, v) = line.split_once(' ')?;
        matches(n).then(|| u32::from_str_radix(v.strip_prefix("0x")?, 16).ok())?
    };
    if let Some(k) = KEYSYMS.lines().find_map(|l| value(l, &|n| n == name)) {
        return Some(k);
    }
    KEYSYMS
        .lines()
        .filter_map(|l| {
            let caps = l
                .split(' ')
                .next()?
                .chars()
                .filter(char::is_ascii_uppercase)
                .count();
            Some((caps, value(l, &|n| n.eq_ignore_ascii_case(name))?))
        })
        .min_by_key(|&(caps, _)| caps)
        .map(|(_, k)| k)
}

/// Our modifier names (and xmonad's mod1, mod4, mod5) as the keysyms pressed for them.
pub fn modifier(m: &str) -> Option<&'static str> {
    Some(match m {
        "super" | "mod4" => "Super_L",
        "ctrl" => "Control_L",
        "alt" | "mod1" => "Alt_L",
        "shift" => "Shift_L",
        "mod5" => "ISO_Level3_Shift",
        _ => return None,
    })
}

/// `e`'s key as (keysym, state) events, in order. Each chord (space-separated) presses its modifiers, presses and
/// releases its key, then releases the modifiers in reverse; a single uppercase letter is shift + the letter, as
/// wtype is given it. An espanso trigger is typed: each character pressed and released as its keysym.
pub fn events(e: &Entry) -> anyhow::Result<Vec<(u32, u32)>> {
    let key = e.bind.key.as_str();
    if e.provider == "espanso" {
        return Ok(key
            .chars()
            .flat_map(|c| [(char_keysym(c), PRESS), (char_keysym(c), RELEASE)])
            .collect());
    }
    let sym = |name: &str| {
        keysym(name).with_context(|| format!("refusing to send {key:?}: no keysym {name:?}"))
    };
    let mut events = Vec::new();
    for chord in key.split_whitespace() {
        let mut parts: Vec<&str> = chord.split('+').collect();
        let name = parts.pop().unwrap_or_default();
        if modifier(name).is_some() {
            bail!("refusing to send {key:?}: {chord:?} is a modifier alone");
        }
        // Each modifier once, where it first comes (`super+mod4+a` presses Super_L once).
        let mut mods = Vec::new();
        for m in parts {
            let m = modifier(m)
                .with_context(|| format!("refusing to send {key:?}: no modifier {m:?}"))?;
            if !mods.contains(&m) {
                mods.push(m);
            }
        }
        let lower = name.to_ascii_lowercase();
        let name = if name.len() == 1 && name.as_bytes()[0].is_ascii_uppercase() {
            if !mods.contains(&"Shift_L") {
                mods.push("Shift_L");
            }
            lower.as_str()
        } else {
            name
        };
        let mut mods = mods
            .into_iter()
            .map(sym)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let k = sym(name)?;
        // A key that is one of its modifiers (`super+Super_L`) is pressed once, as the key.
        mods.retain(|&m| m != k);
        events.extend(mods.iter().map(|&m| (m, PRESS)));
        events.extend([(k, PRESS), (k, RELEASE)]);
        events.extend(mods.iter().rev().map(|&m| (m, RELEASE)));
    }
    Ok(events)
}

/// A character's keysym, as xkbcommon's `xkb_utf32_to_keysym` gives it: Latin-1's printable characters are their own
/// code, any other is the named keysym keysymdef.h gives for it (`Cyrillic_zhe`, which layouts carry and Mutter
/// matches), else 0x01000000 + its code point. A newline stays 0x0100000a (not Linefeed): planning's choice.
fn char_keysym(c: char) -> u32 {
    let cp = u32::from(c);
    if matches!(cp, 0x20..=0x7e | 0xa0..=0xff) {
        return cp;
    }
    let prefix = format!("U+{cp:04X} 0x");
    KEYSYMS
        .lines()
        .find_map(|l| u32::from_str_radix(l.strip_prefix(&prefix)?, 16).ok())
        .filter(|_| c != '\n')
        .unwrap_or(0x0100_0000 + cp)
}

/// Sends `events` through `notify` in order. When one fails, the keys still down are released, last pressed first,
/// so no modifier stays held, and that failure is the result. A press that failed (or timed out) may still have reached
/// the desktop, so its key is released too: releasing a key that is up does nothing.
fn press(
    events: &[(u32, u32)],
    mut notify: impl FnMut(u32, u32) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let mut down = Vec::new();
    for &(keysym, state) in events {
        if let Err(e) = notify(keysym, state) {
            if state == PRESS {
                down.push(keysym);
            }
            for &k in down.iter().rev() {
                let _ = notify(k, RELEASE);
            }
            return Err(e);
        }
        if state == PRESS {
            down.push(keysym);
        } else if let Some(i) = down.iter().rposition(|&k| k == keysym) {
            down.remove(i);
        }
    }
    Ok(())
}

/// An open RemoteDesktop session, alive as long as its connection: the portal closes it when keybind-find exits.
pub struct Session {
    conn: Connection,
    handle: OwnedObjectPath,
}

impl Session {
    pub fn close(self) -> anyhow::Result<()> {
        zbus::block_on(self.close_async())
    }

    async fn close_async(&self) -> anyhow::Result<()> {
        self.conn
            .call_method(
                Some(DESKTOP),
                &self.handle,
                Some("org.freedesktop.portal.Session"),
                "Close",
                &(),
            )
            .await
            .map_err(flat)
            .context("portal Session.Close")?;
        Ok(())
    }

    /// One `NotifyKeyboardKeysym` (`(o a{sv} i u)`): `keysym` pressed (state 1) or released (0).
    fn notify(&self, keysym: u32, state: u32) -> anyhow::Result<()> {
        let keysym = i32::try_from(keysym).with_context(|| format!("keysym {keysym:#x}"))?;
        let no_options = HashMap::<&str, Variant>::new();
        zbus::block_on(self.conn.call_method(
            Some(DESKTOP),
            DESKTOP_PATH,
            Some(REMOTE_DESKTOP),
            "NotifyKeyboardKeysym",
            &(self.handle.as_ref(), no_options, keysym, state),
        ))
        .map_err(flat)?;
        Ok(())
    }
}

/// What the handshake thread reports: `Ok(None)` after each step but the last, the started session (with `Start`'s
/// restore token) after it, or the error that stopped it.
type Report<T> = anyhow::Result<Option<T>>;

/// Runs the handshake with the token stored in `token_file`, then keeps, replaces or deletes that token (`token_after`).
fn open(token_file: Option<&Path>) -> anyhow::Result<Session> {
    let token = token_file.and_then(read_token);
    let steps = steps(token.is_some());
    let sent = token.clone();
    let (tx, rx) = mpsc::channel();
    // zbus has no timer of its own to offer (its runtime is crate-private), so the handshake runs on a thread and each
    // step is waited for here. On a timeout keybind-find stops; its exit drops the connection, and the portal closes a
    // session whose peer is gone.
    std::thread::spawn(move || zbus::block_on(handshake(token, &tx)));
    let started = wait(&rx, &steps);
    let outcome = match &started {
        Ok((_, t)) => Ok(t.as_deref()),
        Err((step, _)) => Err(*step),
    };
    if let Some(file) = token_file {
        let saved = match token_after(sent.is_some(), outcome) {
            TokenFile::Keep => Ok(()),
            TokenFile::Write(t) => save_token(file, t),
            TokenFile::Delete => remove_token(file, sent.as_deref()),
        };
        // A token that can't be kept costs only a dialog next time.
        if let Err(e) = saved {
            eprintln!("keybind-find: portal token {}: {e}", file.display());
        }
    }
    started.map(|(session, _)| session).map_err(|(_, e)| e)
}

#[derive(Debug, PartialEq)]
enum TokenFile<'a> {
    Keep,
    Write(&'a str),
    Delete,
}

/// The token file after a handshake that `sent` the stored token and started (with `Start`'s restore token) or failed
/// at a step. A token is kept only when it has a token's shape. A failure at `SelectDevices` or `Start` after sending
/// one deletes it: the portal used it up there (xdg-desktop-portal consumes it on SelectDevices), may no longer honour
/// it (revoked, another app id) and then shows its dialog, which the 5 s a restored `Start` gets can't cover; without
/// it the next pick gets the dialog's 2 minutes. A failure before SelectDevices leaves it: the portal never saw it.
fn token_after<'a>(sent: bool, outcome: Result<Option<&'a str>, &str>) -> TokenFile<'a> {
    match outcome {
        Ok(Some(t)) if shaped(t) => TokenFile::Write(t),
        Ok(_) => TokenFile::Delete,
        Err("SelectDevices" | "Start") if sent => TokenFile::Delete,
        Err(_) => TokenFile::Keep,
    }
}

/// Waits for each of `steps` in turn, as long as it may take, and returns what the last one reported, or the step it
/// failed at with the error. After an error the handshake reports, it gets up to 5 s more to close its session.
fn wait<T>(
    rx: &mpsc::Receiver<Report<T>>,
    steps: &[(&'static str, Duration)],
) -> Result<T, (&'static str, anyhow::Error)> {
    for (i, &(step, wait)) in steps.iter().enumerate() {
        let last = i + 1 == steps.len();
        let error = match rx.recv_timeout(wait) {
            Ok(Ok(None)) if !last => continue,
            Ok(Ok(Some(done))) if last => return Ok(done),
            Ok(Ok(_)) => anyhow::anyhow!("portal {step}: the handshake is out of step"),
            Ok(Err(e)) => {
                // The thread ends (disconnecting) once its Close is done.
                let _ = rx.recv_timeout(STEP);
                e
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                anyhow::anyhow!("portal {step}: no answer within {wait:?}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::anyhow!("portal {step}: the handshake stopped")
            }
        };
        return Err((step, error));
    }
    Err(("connect", anyhow::anyhow!("portal: no steps")))
}

/// The handshake's steps, each with how long it may take: 5 s, but up to 120 s for a `Start` that shows the dialog (no
/// token to restore).
fn steps(restoring: bool) -> [(&'static str, Duration); 5] {
    [
        ("connect", STEP),
        ("AvailableDeviceTypes", STEP),
        ("CreateSession", STEP),
        ("SelectDevices", STEP),
        ("Start", if restoring { STEP } else { DIALOG }),
    ]
}

type Started = (Session, Option<String>);

/// The whole handshake, reported step by step on `tx`. An error after CreateSession is reported before the session is
/// closed, so a slow Close can't turn it into a timeout.
async fn handshake(restore: Option<String>, tx: &mpsc::Sender<Report<Started>>) {
    let session = match create(tx).await {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };
    match start(&session, restore, tx).await {
        Ok(token) => {
            let _ = tx.send(Ok(Some((session, token))));
        }
        Err(e) => {
            let _ = tx.send(Err(e));
            let _ = session.close_async().await;
        }
    }
}

/// Connects, checks `AvailableDeviceTypes` and creates the session.
async fn create(tx: &mpsc::Sender<Report<Started>>) -> anyhow::Result<Session> {
    let conn = zbus::connection::Builder::session()
        .method_timeout(STEP)
        .build()
        .await
        .map_err(flat)
        .context("portal connect")?;
    let _ = tx.send(Ok(None));
    let types = conn
        .call_method(
            Some(DESKTOP),
            DESKTOP_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(REMOTE_DESKTOP, "AvailableDeviceTypes"),
        )
        .await
        .and_then(|reply| u32::try_from(reply.body().deserialize::<OwnedVariant>()?))
        .map_err(flat)
        .context("portal AvailableDeviceTypes")?;
    if !offers_keyboard(types) {
        bail!("portal AvailableDeviceTypes: the desktop offers no keyboard control ({types})");
    }
    let _ = tx.send(Ok(None));
    let token = handle_token("session");
    let mut created = request(
        &conn,
        "CreateSession",
        &token,
        &(options(
            [("session_handle_token", Variant::from(token.as_str()))],
            &token,
        ),),
    )
    .await?;
    // A handle that is missing or isn't a path leaves nothing to close here: the portal closes the session when this
    // connection goes, at keybind-find's exit.
    let handle = created
        .remove("session_handle")
        .and_then(session_handle)
        .context("portal CreateSession: no session_handle")?;
    let _ = tx.send(Ok(None));
    Ok(Session { conn, handle })
}

/// CreateSession's `session_handle`: a string "for backwards compatibility", or an object path.
fn session_handle(v: OwnedVariant) -> Option<OwnedObjectPath> {
    match Variant::from(v) {
        Variant::Str(s) => OwnedObjectPath::try_from(s.as_str().to_string()).ok(),
        Variant::ObjectPath(p) => Some(p.into()),
        _ => None,
    }
}

/// `SelectDevices` and `Start` on `session`; returns `Start`'s restore token.
async fn start(
    session: &Session,
    restore: Option<String>,
    tx: &mpsc::Sender<Report<Started>>,
) -> anyhow::Result<Option<String>> {
    let token = handle_token("select");
    let mut select = vec![
        ("types", Variant::from(KEYBOARD)),
        ("persist_mode", Variant::from(UNTIL_REVOKED)),
    ];
    if let Some(r) = &restore {
        select.push(("restore_token", Variant::from(r.as_str())));
    }
    let handle = session.handle.as_ref();
    request(
        &session.conn,
        "SelectDevices",
        &token,
        &(handle.clone(), options(select, &token)),
    )
    .await?;
    let _ = tx.send(Ok(None));
    let token = handle_token("start");
    let results = request(
        &session.conn,
        "Start",
        &token,
        &(handle, "", options([], &token)),
    )
    .await?;
    started(results)
}

/// `Start`'s restore token, once its `devices` include the keyboard: the portal refuses key events otherwise.
fn started(mut results: HashMap<String, OwnedVariant>) -> anyhow::Result<Option<String>> {
    let devices = results
        .remove("devices")
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    if !offers_keyboard(devices) {
        bail!("portal Start: the desktop granted no keyboard control ({devices})");
    }
    Ok(results
        .remove("restore_token")
        .and_then(|v| String::try_from(v).ok()))
}

/// An `a{sv}` of `pairs` with `handle_token`, which names the Request object the answer comes on.
fn options<'a>(
    pairs: impl IntoIterator<Item = (&'static str, Variant<'a>)>,
    token: &'a str,
) -> HashMap<&'static str, Variant<'a>> {
    let mut map: HashMap<_, _> = pairs.into_iter().collect();
    map.insert("handle_token", Variant::from(token));
    map
}

/// A token for a Request or the session: unique within this process (each run is its own connection).
fn handle_token(what: &str) -> String {
    format!("keybind_find_{what}_{}", std::process::id())
}

/// Calls `method` on the portal and waits for its Request's `Response`, subscribed before the call so an answer can't
/// come first. The Request must be the predicted one, and only a Response from the portal that answered the call
/// counts (another client on the bus could send one too). Returns the results of a response 0; 1 (cancelled) and 2
/// (other) are the desktop's refusal.
async fn request<B>(
    conn: &Connection,
    method: &str,
    token: &str,
    body: &B,
) -> anyhow::Result<HashMap<String, OwnedVariant>>
where
    B: zbus::export::serde::Serialize + zbus::DynamicType,
{
    let answer = async {
        let unique = conn.unique_name().context("no unique name on the bus")?;
        let path = request_path(unique.as_str(), token);
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.portal.Request")
            .member("Response")
            .path(path.as_str())
            .build()
            .map_err(flat)?;
        let mut stream = MessageStream::for_match_rule(rule, conn, None)
            .await
            .map_err(flat)?;
        let reply = conn
            .call_method(
                Some(DESKTOP),
                DESKTOP_PATH,
                Some(REMOTE_DESKTOP),
                method,
                body,
            )
            .await
            .map_err(flat)?;
        // Every xdg-desktop-portal since 0.9 answers on the predicted path.
        let handle = reply
            .body()
            .deserialize::<OwnedObjectPath>()
            .map_err(flat)?;
        if handle.as_str() != path {
            bail!("the request is {}, not {path}", handle.as_str());
        }
        let portal = sender(&reply);
        // ponytail: more Responses forged on this path than the stream queues (64) can stall zbus's reader until the
        // method timeout, and a portal restarted mid-request is waited out to the step's end; track
        // NameOwnerChanged and size the queue if either matters.
        loop {
            let msg = std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx))
                .await
                .context("the bus closed")?
                .map_err(flat)?;
            if from_portal(portal.as_deref(), sender(&msg).as_deref())? {
                return msg
                    .body()
                    .deserialize::<(u32, HashMap<String, OwnedVariant>)>()
                    .map_err(flat);
            }
        }
    }
    .await
    .with_context(|| format!("portal {method}"))?;
    answered(method, answer.0, answer.1)
}

fn sender(msg: &zbus::Message) -> Option<String> {
    msg.header().sender().map(|s| s.to_string())
}

/// Whether a Response sent by `response` comes from `portal`, the sender of the call's reply. A reply has a sender on
/// any bus; one without is an error, not a wait for a Response that can never match.
fn from_portal(portal: Option<&str>, response: Option<&str>) -> anyhow::Result<bool> {
    let portal = portal.context("the reply has no sender")?;
    Ok(response == Some(portal))
}

/// A zbus error as its own text: its source (the D-Bus or I/O error inside) would say the same again.
fn flat(e: zbus::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// `AvailableDeviceTypes` (or `Start`'s `devices`) has the KEYBOARD bit.
fn offers_keyboard(types: u32) -> bool {
    types & KEYBOARD != 0
}

/// A Response's results when its code is 0 (success); 1 (cancelled) and 2 (other) are the desktop's refusal.
fn answered<T>(method: &str, code: u32, results: T) -> anyhow::Result<T> {
    match code {
        0 => Ok(results),
        1 | 2 => bail!("the desktop refused keyboard control"),
        n => bail!("portal {method}: response {n}"),
    }
}

/// `/org/freedesktop/portal/desktop/request/SENDER/TOKEN`, SENDER being the unique name without its `:` and with `_`
/// for `.` (org.freedesktop.portal.Request).
fn request_path(unique_name: &str, token: &str) -> String {
    let sender = unique_name.trim_start_matches(':').replace('.', "_");
    format!("{DESKTOP_PATH}/request/{sender}/{token}")
}

/// `portal-token` in the state directory, as `usage` finds its log.
fn token_path_from(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    Some(crate::usage::state_dir_from(xdg_state_home, home)?.join("portal-token"))
}

/// Whether `token` has a token's shape. xdg-desktop-portal rejects a malformed one (INVALID_ARGUMENT) instead of
/// ignoring it. Its tokens are `[A-Za-z0-9_]` (or a UUID, from older versions): letters, digits, `_` and `-`.
fn shaped(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The stored token: a regular file of at most 4 KiB with a token's shape; anything else is sent as no token.
fn read_token(path: &Path) -> Option<String> {
    use std::io::Read as _;
    // O_NONBLOCK: opening a FIFO doesn't wait for a writer; the type is then checked on what was opened.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0o4000)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(4097).read_to_string(&mut text).ok()?;
    let token = text.trim();
    if text.len() > 4096 {
        return None;
    }
    shaped(token).then(|| token.to_string())
}

/// Replaces the stored token with `token` (mode 0600, through a new file of this process's renamed over it).
fn save_token(path: &Path, token: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let new = temp_path(path);
    // The name has this process's pid: one left there is a dead run's of the same pid.
    let _ = std::fs::remove_file(&new);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&new)
        .and_then(|mut f| f.write_all(token.as_bytes()))
        .and_then(|()| std::fs::rename(&new, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&new);
    }
    written
}

/// Deletes the stored token if it is still `sent` (no token for `None`): another run may have stored a new one since.
// ponytail: compare, then remove; a run storing between the two loses its token, which costs one dialog.
fn remove_token(path: &Path, sent: Option<&str>) -> std::io::Result<()> {
    if read_token(path).as_deref() != sent {
        return Ok(());
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// `<file>.new.<pid>`: two runs at once each write their own.
fn temp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".new.{}", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn request_path_from_the_unique_name() {
        assert_eq!(
            request_path(":1.42", "keybind_find_start_7"),
            "/org/freedesktop/portal/desktop/request/1_42/keybind_find_start_7"
        );
        assert_eq!(
            request_path(":1.2.3", "t"),
            "/org/freedesktop/portal/desktop/request/1_2_3/t"
        );
        assert!(handle_token("select").starts_with("keybind_find_select_"));
        assert_ne!(handle_token("select"), handle_token("start"));
    }

    #[test]
    fn keyboard_bit_and_response_codes() {
        for (types, keyboard) in [
            (7, true),
            (1, true),
            (3, true),
            (0, false),
            (2, false),
            (6, false),
        ] {
            assert_eq!(offers_keyboard(types), keyboard, "{types}");
        }
        assert_eq!(answered("Start", 0, 5).unwrap(), 5);
        for code in [1, 2] {
            assert_eq!(
                format!("{:#}", answered("Start", code, ()).unwrap_err()),
                "the desktop refused keyboard control"
            );
        }
        assert_eq!(
            format!("{:#}", answered("SelectDevices", 3, ()).unwrap_err()),
            "portal SelectDevices: response 3"
        );
    }

    #[test]
    fn a_dialog_gets_two_minutes() {
        let wait = |restoring| -> Vec<(&str, u64)> {
            steps(restoring)
                .iter()
                .map(|(s, d)| (*s, d.as_secs()))
                .collect()
        };
        let mut want = vec![
            ("connect", 5),
            ("AvailableDeviceTypes", 5),
            ("CreateSession", 5),
            ("SelectDevices", 5),
            ("Start", 120),
        ];
        assert_eq!(wait(false), want);
        want[4].1 = 5;
        assert_eq!(wait(true), want);
    }

    #[test]
    fn token_path_rules() {
        let p = |x: Option<&str>, h: Option<&str>| {
            token_path_from(x.map(Into::into), h.map(Into::into)).map(|p| p.display().to_string())
        };
        let home = Some("/h/.local/state/keybind-find/portal-token");
        assert_eq!(
            p(Some("/s"), Some("/h")).as_deref(),
            Some("/s/keybind-find/portal-token")
        );
        assert_eq!(p(None, Some("/h")).as_deref(), home);
        assert_eq!(p(Some("rel"), Some("/h")).as_deref(), home);
        assert_eq!(p(None, None), None);
    }

    #[test]
    fn token_file_write_replace_delete() {
        let dir = crate::test_tmp().join(format!("kbf-portal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = token_path_from(Some(dir.clone().into()), None).unwrap();
        assert_eq!(read_token(&file), None);
        // Deleting a token that isn't there is fine.
        remove_token(&file, None).unwrap();
        save_token(&file, "abc_DEF-1").unwrap();
        assert_eq!(read_token(&file).as_deref(), Some("abc_DEF-1"));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // A token file someone made world-readable is replaced by a private one.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_token(&file, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!temp_path(&file).exists());
        assert!(temp_path(&file)
            .to_string_lossy()
            .ends_with(&format!("portal-token.new.{}", std::process::id())));
        // A temp file of this pid left by a dead run doesn't stop a write.
        std::fs::write(temp_path(&file), "left").unwrap();
        save_token(&file, "third").unwrap();
        assert_eq!(read_token(&file).as_deref(), Some("third"));
        assert!(!temp_path(&file).exists());
        // A write that fails leaves no temp file: here the rename, over a directory.
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("inside")).unwrap();
        assert!(save_token(&blocked, "x").is_err());
        assert!(!temp_path(&blocked).exists());
        // Deleted only while it is still the token that was sent: another run's newer one stays.
        remove_token(&file, Some("other")).unwrap();
        assert!(file.exists());
        remove_token(&file, Some("third")).unwrap();
        assert!(!file.exists());
        std::fs::write(&file, "two words").unwrap();
        save_token(&file, "fourth").unwrap();
        remove_token(&file, None).unwrap();
        assert!(file.exists());
        std::fs::write(&file, "two words").unwrap();
        remove_token(&file, None).unwrap();
        assert!(!file.exists());
        // A garbage file is no token.
        for garbage in ["", "  \n", "two words", "a/b", "tok\nen", "\u{fffd}x"] {
            std::fs::write(&file, garbage).unwrap();
            assert_eq!(read_token(&file), None, "{garbage:?}");
        }
        std::fs::write(&file, [0xff, 0xfe]).unwrap();
        assert_eq!(read_token(&file), None);
        // A trailing newline (an editor's) is not part of it; a UUID from an older portal is a token.
        std::fs::write(&file, "fake-1\n").unwrap();
        assert_eq!(read_token(&file).as_deref(), Some("fake-1"));
        std::fs::write(&file, "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").unwrap();
        assert!(read_token(&file).is_some());
        // At most 4 KiB, and only a regular file.
        std::fs::write(&file, "a".repeat(4096)).unwrap();
        assert!(read_token(&file).is_some());
        std::fs::write(&file, "a".repeat(4097)).unwrap();
        assert_eq!(read_token(&file), None);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert_eq!(read_token(&file), None);
        std::fs::remove_dir(&file).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&file)
            .status()
            .unwrap();
        assert!(made.success());
        assert_eq!(read_token(&file), None);
    }

    #[test]
    fn the_token_after_a_handshake() {
        use TokenFile::*;
        // Started: Start's token replaces the stored one; none (or an empty or misshapen one) deletes it.
        for sent in [false, true] {
            assert_eq!(token_after(sent, Ok(Some("fake-1"))), Write("fake-1"));
            assert_eq!(token_after(sent, Ok(None)), Delete);
            assert_eq!(token_after(sent, Ok(Some(""))), Delete);
            assert_eq!(token_after(sent, Ok(Some("a b"))), Delete);
        }
        // Failed: a token sent is deleted once SelectDevices had it; before that the portal never saw it, and with
        // none sent there is nothing to change.
        for (step, sent_deletes) in [
            ("connect", false),
            ("AvailableDeviceTypes", false),
            ("CreateSession", false),
            ("SelectDevices", true),
            ("Start", true),
        ] {
            let want = if sent_deletes { Delete } else { Keep };
            assert_eq!(token_after(true, Err(step)), want, "{step}");
            assert_eq!(token_after(false, Err(step)), Keep, "{step}");
        }
    }

    #[test]
    fn each_step_is_waited_for_by_name() {
        let ms = Duration::from_millis;
        let short = [
            ("connect", ms(50)),
            ("AvailableDeviceTypes", ms(50)),
            ("CreateSession", ms(50)),
            ("SelectDevices", ms(50)),
            ("Start", ms(80)),
        ];
        let failed = |r: Result<&str, (&str, anyhow::Error)>| {
            let (step, e) = r.unwrap_err();
            (step.to_string(), e.to_string())
        };
        // A handshake that stops reporting after n steps times out on step n, with that step's wait.
        for (n, (step, wait)) in short.iter().enumerate() {
            let (tx, rx) = mpsc::channel::<Report<&str>>();
            for _ in 0..n {
                tx.send(Ok(None)).unwrap();
            }
            assert_eq!(
                failed(super::wait(&rx, &short)),
                (
                    step.to_string(),
                    format!("portal {step}: no answer within {wait:?}")
                )
            );
            drop(tx);
        }
        // One that ends early says which step it stopped at.
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        tx.send(Ok(None)).unwrap();
        tx.send(Ok(None)).unwrap();
        drop(tx);
        assert_eq!(
            failed(super::wait(&rx, &steps(true))),
            (
                "CreateSession".into(),
                "portal CreateSession: the handshake stopped".into()
            )
        );
        // The last report is the result; an error is passed on as it is, at the step it came in.
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        for _ in 0..4 {
            tx.send(Ok(None)).unwrap();
        }
        tx.send(Ok(Some("started"))).unwrap();
        assert_eq!(super::wait(&rx, &steps(false)).ok(), Some("started"));
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        for _ in 0..3 {
            tx.send(Ok(None)).unwrap();
        }
        tx.send(Err(anyhow::anyhow!("the desktop refused keyboard control")))
            .unwrap();
        drop(tx);
        assert_eq!(
            failed(super::wait(&rx, &steps(false))),
            (
                "SelectDevices".into(),
                "the desktop refused keyboard control".into()
            )
        );
        // After a reported error the handshake's Close gets its time, but not forever.
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        tx.send(Err(anyhow::anyhow!("refused"))).unwrap();
        let closing = std::thread::spawn(move || {
            std::thread::sleep(ms(100));
            drop(tx);
        });
        let started = std::time::Instant::now();
        assert_eq!(failed(super::wait(&rx, &steps(false))).0, "connect");
        assert!(started.elapsed() >= ms(100));
        closing.join().unwrap();
        // Out of step: a result before Start, or a step report where the result belongs.
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        for _ in 0..3 {
            tx.send(Ok(None)).unwrap();
        }
        tx.send(Ok(Some("early"))).unwrap();
        assert_eq!(
            failed(super::wait(&rx, &steps(false))).1,
            "portal SelectDevices: the handshake is out of step"
        );
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        for _ in 0..5 {
            tx.send(Ok(None)).unwrap();
        }
        assert_eq!(
            failed(super::wait(&rx, &steps(false))).1,
            "portal Start: the handshake is out of step"
        );
    }

    #[test]
    fn only_the_portal_answers() {
        assert!(from_portal(Some(":1.5"), Some(":1.5")).unwrap());
        assert!(!from_portal(Some(":1.5"), Some(":1.9")).unwrap());
        assert!(!from_portal(Some(":1.5"), None).unwrap());
        assert_eq!(
            from_portal(None, Some(":1.5")).unwrap_err().to_string(),
            "the reply has no sender"
        );
    }

    #[test]
    fn session_handle_as_string_or_path() {
        let path = "/org/freedesktop/portal/desktop/session/1_4/t";
        let s = OwnedVariant::try_from(Variant::from(path)).unwrap();
        assert_eq!(session_handle(s).unwrap().as_str(), path);
        let o = OwnedVariant::try_from(Variant::from(zbus::ObjectPath::try_from(path).unwrap()))
            .unwrap();
        assert_eq!(session_handle(o).unwrap().as_str(), path);
        let bad = OwnedVariant::try_from(Variant::from("not a path")).unwrap();
        assert!(session_handle(bad).is_none());
        let num = OwnedVariant::try_from(Variant::from(7u32)).unwrap();
        assert!(session_handle(num).is_none());
    }

    fn entry(provider: &str, key: &str) -> Entry {
        Entry {
            provider: provider.into(),
            category: "c".into(),
            bind: crate::sheet::Bind {
                key: key.into(),
                desc: "d".into(),
                action: String::new(),
                when: Default::default(),
                run: Vec::new(),
            },
        }
    }

    fn evs(provider: &str, key: &str) -> Result<Vec<(u32, u32)>, String> {
        events(&entry(provider, key)).map_err(|e| e.to_string())
    }

    const CTRL: u32 = 0xffe3;
    const SHIFT: u32 = 0xffe1;
    const SUPER: u32 = 0xffeb;
    const ALT: u32 = 0xffe9;
    const LEVEL3: u32 = 0xfe03;

    #[test]
    fn the_keysym_table() {
        let count = |prefix: &str| {
            KEYSYMS
                .lines()
                .filter(|l| !l.starts_with('#') && l.starts_with(prefix))
                .count()
        };
        // keysymdef.h's 2114 names, XF86keysym.h's 398, and 729 code points with a named keysym.
        assert_eq!(count(""), 2114 + 398 + 729);
        assert_eq!(count("XF86"), 398);
        assert_eq!(count("U+"), 729);
        for (name, value) in [
            ("k", 0x6b),
            ("K", 0x4b),
            ("Return", 0xff0d),
            ("KP_7", 0xffb7),
            ("Super_L", SUPER),
            ("ISO_Level3_Shift", LEVEL3),
            ("EuroSign", 0x20ac),
            ("XF86AudioRaiseVolume", 0x1008ff13),
            // XF86keysym.h's _EVDEVK(0x0a4): 0x10081000 + 0x0a4.
            ("XF86MediaPlayPause", 0x100810a4),
            // Not exact: ignoring case, as wtype and xkbcommon find it.
            ("return", 0xff0d),
            ("page_up", 0xff55),
            ("xf86audioraisevolume", 0x1008ff13),
            // Several names that differ only in case: the one with the fewest capitals.
            ("AGRAVE", 0xe0),
            ("CYRILLIC_ZHE", 0x6d6),
            // Exact first.
            ("Agrave", 0xc0),
        ] {
            assert_eq!(keysym(name), Some(value), "{name}");
        }
        // Case is folded for ASCII only; the header is
        // not names (and the code-point section can't be asked for: a chord's parts have no `+`).
        for name in ["", "#", "kk", "Foo", "XK_k", "XF86XK_AudioMute", "Ж"] {
            assert_eq!(keysym(name), None, "{name}");
        }
    }

    #[test]
    fn chords_as_keysym_events() {
        // Modifiers in order, the key pressed and released, the modifiers released in reverse.
        assert_eq!(
            evs("x", "ctrl+shift+k"),
            Ok(vec![
                (CTRL, 1),
                (SHIFT, 1),
                (0x6b, 1),
                (0x6b, 0),
                (SHIFT, 0),
                (CTRL, 0)
            ])
        );
        // Every modifier name, xmonad's included.
        for (m, sym) in [
            ("super", SUPER),
            ("mod4", SUPER),
            ("ctrl", CTRL),
            ("alt", ALT),
            ("mod1", ALT),
            ("shift", SHIFT),
            ("mod5", LEVEL3),
        ] {
            assert_eq!(
                evs("x", &format!("{m}+Return")),
                Ok(vec![(sym, 1), (0xff0d, 1), (0xff0d, 0), (sym, 0)]),
                "{m}"
            );
        }
        // An uppercase letter is shift + the letter, after the chord's own modifiers; never shift twice.
        assert_eq!(
            evs("x", "K"),
            Ok(vec![(SHIFT, 1), (0x6b, 1), (0x6b, 0), (SHIFT, 0)])
        );
        assert_eq!(evs("x", "ctrl+K"), evs("x", "ctrl+shift+k"),);
        assert_eq!(
            evs("x", "shift+ctrl+K"),
            Ok(vec![
                (SHIFT, 1),
                (CTRL, 1),
                (0x6b, 1),
                (0x6b, 0),
                (CTRL, 0),
                (SHIFT, 0)
            ])
        );
        // Not a single letter: the name as it is, or ignoring case.
        assert_eq!(evs("x", "F5"), Ok(vec![(0xffc2, 1), (0xffc2, 0)]));
        assert_eq!(
            evs("x", "super+return XF86AudioRaiseVolume"),
            Ok(vec![
                (SUPER, 1),
                (0xff0d, 1),
                (0xff0d, 0),
                (SUPER, 0),
                (0x1008ff13, 1),
                (0x1008ff13, 0)
            ])
        );
        // Each modifier once, where it first comes.
        assert_eq!(
            evs("x", "super+ctrl+mod4+ctrl+a"),
            Ok(vec![
                (SUPER, 1),
                (CTRL, 1),
                (0x61, 1),
                (0x61, 0),
                (CTRL, 0),
                (SUPER, 0)
            ])
        );
        assert_eq!(evs("x", "alt+mod1+shift+A"), evs("x", "alt+shift+a"));
        // A key that is one of its own modifiers is pressed once, as the key.
        assert_eq!(evs("x", "super+Super_L"), Ok(vec![(SUPER, 1), (SUPER, 0)]));
        assert_eq!(
            evs("x", "ctrl+shift+Shift_L"),
            Ok(vec![(CTRL, 1), (SHIFT, 1), (SHIFT, 0), (CTRL, 0)])
        );
        // A key of three chords, one after another (extra spaces between them don't count).
        assert_eq!(
            evs("x", "ctrl+b  c super+mod5+x"),
            Ok(vec![
                (CTRL, 1),
                (0x62, 1),
                (0x62, 0),
                (CTRL, 0),
                (0x63, 1),
                (0x63, 0),
                (SUPER, 1),
                (LEVEL3, 1),
                (0x78, 1),
                (0x78, 0),
                (LEVEL3, 0),
                (SUPER, 0)
            ])
        );
        // A name the table lacks is refused, whichever chord it is in.
        assert_eq!(
            evs("x", "a ctrl+Foo"),
            Err(r#"refusing to send "a ctrl+Foo": no keysym "Foo""#.into())
        );
        assert_eq!(
            evs("x", "mod3+a"),
            Err(r#"refusing to send "mod3+a": no modifier "mod3""#.into())
        );
        // A chord of modifiers only.
        for (key, chord) in [
            ("ctrl+shift", "ctrl+shift"),
            ("a super", "super"),
            ("mod1+mod5", "mod1+mod5"),
        ] {
            assert_eq!(
                evs("x", key),
                Err(format!(
                    "refusing to send {key:?}: {chord:?} is a modifier alone"
                ))
            );
        }
        // A modifier's keysym is a key like any other.
        assert_eq!(evs("x", "Super_L"), Ok(vec![(SUPER, 1), (SUPER, 0)]));
    }

    #[test]
    fn espanso_text_as_keysym_events() {
        let typed = |text: &str| -> Vec<u32> {
            let e = evs("espanso", text).unwrap();
            // Each character pressed then released.
            for pair in e.chunks(2) {
                assert_eq!(
                    (pair[0].0, pair[0].1, pair[1].1),
                    (pair[1].0, 1, 0),
                    "{text}"
                );
            }
            e.iter().step_by(2).map(|&(k, _)| k).collect()
        };
        // ASCII is its own code, space and `'` included; the ends of the ranges too.
        assert_eq!(
            typed(":it's K~"),
            [0x3a, 0x69, 0x74, 0x27, 0x73, 0x20, 0x4b, 0x7e]
        );
        // Latin-1: U+00A0..U+00FF are their own code.
        assert_eq!(typed("\u{a0}é\u{ff}"), [0xa0, 0xe9, 0xff]);
        // Anything else: the named keysym keysymdef.h gives for it (Cyrillic_zhe, EuroSign, Lstroke, Tab, Delete) …
        assert_eq!(typed("ж€Ł\t\u{7f}"), [0x6d6, 0x20ac, 0x1a3, 0xff09, 0xffff]);
        assert_eq!(typed("∂√"), [0x8ef, 0x8d6]);
        // … else 0x01000000 + the code point (a C1 control, an emoji, a letter with no name); a newline too, not
        // Linefeed.
        assert_eq!(
            typed("\u{9f}🙂\u{1e9e}\n"),
            [0x0100_009f, 0x0101_f642, 0x0100_1e9e, 0x0100_000a]
        );
        // Typed as written: no chord or table lookup.
        assert_eq!(
            typed("ctrl+Foo"),
            "ctrl+Foo".bytes().map(u32::from).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_failed_event_releases_what_is_down() {
        let run = |fail_at: usize| {
            let sent = RefCell::new(Vec::new());
            let result = press(&evs("x", "ctrl+shift+k a").unwrap(), |k, s| {
                sent.borrow_mut().push((k, s));
                if sent.borrow().len() == fail_at {
                    bail!("refused");
                }
                Ok(())
            });
            (result.map_err(|e| e.to_string()), sent.into_inner())
        };
        // All sent.
        let (result, sent) = run(0);
        assert_eq!(result, Ok(()));
        assert_eq!(sent, evs("x", "ctrl+shift+k a").unwrap());
        // The key's press fails: it may have gone down all the same, so it is released, then shift, then ctrl.
        let (result, sent) = run(3);
        assert_eq!(result, Err("refused".into()));
        assert_eq!(
            sent,
            [
                (CTRL, 1),
                (SHIFT, 1),
                (0x6b, 1),
                (0x6b, 0),
                (SHIFT, 0),
                (CTRL, 0)
            ]
        );
        // The key's release fails: it is still down, released first.
        let (_, sent) = run(4);
        assert_eq!(sent[4..], [(0x6b, 0), (SHIFT, 0), (CTRL, 0)]);
        // Shift already released: only ctrl is left.
        let (_, sent) = run(6);
        assert_eq!(sent[6..], [(CTRL, 0)]);
        // The first chord all released: only the next key, whose press failed, is released.
        let (_, sent) = run(7);
        assert_eq!(sent[6..], [(0x61, 1), (0x61, 0)]);
        // The first press fails: only that key is released.
        let (_, sent) = run(1);
        assert_eq!(sent, [(CTRL, 1), (CTRL, 0)]);
        // The last release fails: that key is still down, released once more.
        let (_, sent) = run(8);
        assert_eq!(sent[6..], [(0x61, 1), (0x61, 0), (0x61, 0)]);
    }

    #[test]
    fn start_must_grant_the_keyboard() {
        let results = |devices: Option<u32>, token: Option<&str>| {
            let mut r = HashMap::new();
            if let Some(d) = devices {
                r.insert(
                    "devices".to_string(),
                    OwnedVariant::try_from(Variant::from(d)).unwrap(),
                );
            }
            if let Some(t) = token {
                r.insert(
                    "restore_token".to_string(),
                    OwnedVariant::try_from(Variant::from(t)).unwrap(),
                );
            }
            started(r).map_err(|e| e.to_string())
        };
        assert_eq!(results(Some(1), Some("t1")), Ok(Some("t1".into())));
        assert_eq!(results(Some(7), None), Ok(None));
        for (devices, shown) in [(Some(2), 2), (Some(6), 6), (None, 0)] {
            assert_eq!(
                results(devices, Some("t1")),
                Err(format!(
                    "portal Start: the desktop granted no keyboard control ({shown})"
                ))
            );
        }
    }
}
