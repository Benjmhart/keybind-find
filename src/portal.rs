//! The RemoteDesktop portal, which GNOME and KDE Plasma offer on Wayland in place of the virtual-keyboard protocol
//! wtype needs: a session on the session bus that the desktop lets press keys once its user approves. The approval is
//! kept as a restore token in the state directory, so the dialog shows once, not on every pick.
//!
//! The handshake (xdg-desktop-portal's `org.freedesktop.portal.RemoteDesktop`, interface version 2) runs on one
//! connection, since a session belongs to the connection that created it: `AvailableDeviceTypes`, `CreateSession`,
//! `SelectDevices` (keyboard only, kept until revoked, with the stored token), `Start`, then `Session.Close`.

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

/// Sends `e`'s keys through the portal. For now it opens the session and closes it again; the caller then says to press
/// the keys by hand.
pub fn send(_e: &Entry) -> anyhow::Result<()> {
    let token_file = token_path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"));
    let session = open(token_file.as_deref())?;
    // The keys go here, on `session`, before it closes.
    // A session that won't close ends with keybind-find anyway: said, not the pick's error.
    if let Err(e) = session.close() {
        eprintln!("keybind-find: {e:#}");
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
}

/// What the handshake thread reports: `Ok(None)` after each step but the last, the started session (with `Start`'s
/// restore token) after it, or the error that stopped it.
type Report<T> = anyhow::Result<Option<T>>;

/// Runs the handshake with the token stored in `token_file`, then keeps, replaces or deletes that token (`token_after`).
fn open(token_file: Option<&Path>) -> anyhow::Result<Session> {
    let token = token_file.and_then(read_token);
    let steps = steps(token.is_some());
    let sent = token.is_some();
    let (tx, rx) = mpsc::channel();
    // zbus has no timer of its own to offer (its runtime is crate-private), so the handshake runs on a thread and each
    // step is waited for here. On a timeout keybind-find stops; its exit drops the connection, and the portal closes a
    // session whose peer is gone.
    std::thread::spawn(move || {
        let report = zbus::block_on(handshake(token, &tx)).map(Some);
        let _ = tx.send(report);
    });
    let started = wait(&rx, &steps);
    let new_token = started.as_ref().ok().map(|(_, t)| t.as_deref());
    if let Some(file) = token_file {
        let saved = match token_after(sent, new_token) {
            TokenFile::Keep => Ok(()),
            TokenFile::Write(t) => save_token(file, Some(t)),
            TokenFile::Delete => save_token(file, None),
        };
        // A token that can't be kept costs only a dialog next time.
        if let Err(e) = saved {
            eprintln!("keybind-find: portal token {}: {e}", file.display());
        }
    }
    started.map(|(session, _)| session)
}

#[derive(Debug, PartialEq)]
enum TokenFile<'a> {
    Keep,
    Write(&'a str),
    Delete,
}

/// The token file after a handshake that `sent` the stored token and `started` (with `Start`'s restore token) or
/// failed (`None`). A token is kept only when it has a token's shape. A failure after sending one deletes it: the portal
/// may no longer honour it (revoked, used up, another app id), and then shows its dialog, which the 5 s a restored
/// `Start` gets can't cover; without the token the next pick gets the dialog's 2 minutes.
fn token_after(sent: bool, started: Option<Option<&str>>) -> TokenFile<'_> {
    match started {
        Some(Some(t)) if shaped(t) => TokenFile::Write(t),
        Some(_) => TokenFile::Delete,
        None if sent => TokenFile::Delete,
        None => TokenFile::Keep,
    }
}

/// Waits for each of `steps` in turn, as long as it may take, and returns what the last one reported.
fn wait<T>(
    rx: &mpsc::Receiver<Report<T>>,
    steps: &[(&'static str, Duration)],
) -> anyhow::Result<T> {
    for (i, &(step, wait)) in steps.iter().enumerate() {
        let last = i + 1 == steps.len();
        match rx.recv_timeout(wait) {
            Ok(Ok(None)) if !last => {}
            Ok(Ok(Some(done))) if last => return Ok(done),
            Ok(Ok(_)) => bail!("portal {step}: the handshake is out of step"),
            Ok(Err(e)) => return Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                bail!("portal {step}: no answer within {wait:?}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("portal {step}: the handshake stopped")
            }
        }
    }
    bail!("portal: no steps")
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

async fn handshake(
    restore: Option<String>,
    tx: &mpsc::Sender<Report<(Session, Option<String>)>>,
) -> anyhow::Result<(Session, Option<String>)> {
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
    let session = Session { conn, handle };
    match start(&session, restore, tx).await {
        Ok(token) => Ok((session, token)),
        Err(e) => {
            let _ = session.close_async().await;
            Err(e)
        }
    }
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
    tx: &mpsc::Sender<Report<(Session, Option<String>)>>,
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
    let mut results = request(
        &session.conn,
        "Start",
        &token,
        &(handle, "", options([], &token)),
    )
    .await?;
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
        loop {
            let msg = std::future::poll_fn(|cx| std::pin::Pin::new(&mut stream).poll_next(cx))
                .await
                .context("the bus closed")?
                .map_err(flat)?;
            if portal.is_some() && sender(&msg) == portal {
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

/// A zbus error as its own text: its source (the D-Bus or I/O error inside) would say the same again.
fn flat(e: zbus::Error) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// `AvailableDeviceTypes` has the KEYBOARD bit.
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
    // Checked before opening: opening a FIFO would wait for a writer.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(4097).read_to_string(&mut text).ok()?;
    let token = text.trim();
    if text.len() > 4096 {
        return None;
    }
    shaped(token).then(|| token.to_string())
}

/// Replaces the stored token with `token` (mode 0600, through a new file of this process's renamed over it), or
/// deletes it for `None`.
fn save_token(path: &Path, token: Option<&str>) -> std::io::Result<()> {
    let Some(token) = token else {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        };
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let new = temp_path(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&new)?
        .write_all(token.as_bytes())?;
    std::fs::rename(&new, path)
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
        save_token(&file, None).unwrap();
        save_token(&file, Some("abc_DEF-1")).unwrap();
        assert_eq!(read_token(&file).as_deref(), Some("abc_DEF-1"));
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // A token file someone made world-readable is replaced by a private one.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_token(&file, Some("second")).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "second");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!temp_path(&file).exists());
        assert!(temp_path(&file)
            .to_string_lossy()
            .ends_with(&format!("portal-token.new.{}", std::process::id())));
        save_token(&file, None).unwrap();
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
            assert_eq!(token_after(sent, Some(Some("fake-1"))), Write("fake-1"));
            assert_eq!(token_after(sent, Some(None)), Delete);
            assert_eq!(token_after(sent, Some(Some(""))), Delete);
            assert_eq!(token_after(sent, Some(Some("a b"))), Delete);
        }
        // Failed: a token sent is one the portal may no longer honour; with none sent there is nothing to change.
        assert_eq!(token_after(true, None), Delete);
        assert_eq!(token_after(false, None), Keep);
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
        // A handshake that stops reporting after n steps times out on step n, with that step's wait.
        for (n, (step, wait)) in short.iter().enumerate() {
            let (tx, rx) = mpsc::channel::<Report<&str>>();
            for _ in 0..n {
                tx.send(Ok(None)).unwrap();
            }
            let err = super::wait(&rx, &short).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("portal {step}: no answer within {wait:?}")
            );
            drop(tx);
        }
        // One that ends early says which step it stopped at.
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        tx.send(Ok(None)).unwrap();
        tx.send(Ok(None)).unwrap();
        drop(tx);
        let err = super::wait(&rx, &steps(true)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "portal CreateSession: the handshake stopped"
        );
        // The last report is the result; an error is passed on as it is.
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        for _ in 0..4 {
            tx.send(Ok(None)).unwrap();
        }
        tx.send(Ok(Some("started"))).unwrap();
        assert_eq!(super::wait(&rx, &steps(false)).unwrap(), "started");
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        tx.send(Ok(None)).unwrap();
        tx.send(Err(anyhow::anyhow!("the desktop refused keyboard control")))
            .unwrap();
        assert_eq!(
            super::wait(&rx, &steps(false)).unwrap_err().to_string(),
            "the desktop refused keyboard control"
        );
        // Out of step: a result before Start, or a step report where the result belongs.
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        for _ in 0..3 {
            tx.send(Ok(None)).unwrap();
        }
        tx.send(Ok(Some("early"))).unwrap();
        assert_eq!(
            super::wait(&rx, &steps(false)).unwrap_err().to_string(),
            "portal SelectDevices: the handshake is out of step"
        );
        let (tx, rx) = mpsc::channel::<Report<&str>>();
        for _ in 0..5 {
            tx.send(Ok(None)).unwrap();
        }
        assert_eq!(
            super::wait(&rx, &steps(false)).unwrap_err().to_string(),
            "portal Start: the handshake is out of step"
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
}
