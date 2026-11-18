"""A fake xdg-desktop-portal RemoteDesktop for tests/e2e/harness.sh, and the check of what it was asked.

usage: fake_portal.py LOG              serve on the session bus as FAKE_PORTAL (a mode in MODES) says, appending each
                                       call to LOG as one JSON line; prints `ready` once it owns the name
       fake_portal.py --plan MODE      print the mode's number of picks and the token to store before them (`-`: none)
       fake_portal.py --check MODE LOG TOKEN_FILE STDERR...
                                       check the calls (one pick per STDERR file), each STDERR and TOKEN_FILE

Owns org.freedesktop.portal.Desktop and exports /org/freedesktop/portal/desktop with
org.freedesktop.portal.RemoteDesktop (version 2, AvailableDeviceTypes 7). Each Request is answered by a Response on
its predicted path (/org/freedesktop/portal/desktop/request/SENDER/TOKEN), sent to the caller. Start answers 0 with the
restore_token `fake-1`; when no restore_token was selected (or, in the stale mode, one it doesn't honour) it first
waits DIALOG seconds, as a person reading the dialog would (past the 5 s every other step gets). A session has
org.freedesktop.portal.Session.Close on its path.

The check: the calls of each pick in MODES' order, Start's parent window "", the session handle CreateSession returned
in SelectDevices, Start and each NotifyKeyboardKeysym, the keysyms pressed and released (the mode's, in order, with no
options), each SelectDevices' types 1, persist_mode 2 and restore_token (the mode's) with their D-Bus types, TOKEN_FILE
(the token, mode 0600, or no file), each pick's exit status (STDERR with .rc for .err: 0 when its keys were sent),
each STDERR's last line (a pick that sent has no `… yourself` line), and no other `keybind-find: portal` line than the
mode's. Prints `ok` or what differs; exits 1 when something does.
"""
import json
import os
import sys

KEYS = "ctrl+shift+k"
TOKEN = "fake-1"
DIALOG = 7
HANDSHAKE = ["AvailableDeviceTypes", "CreateSession", "SelectDevices", "Start"]
CTRL, SHIFT, K = 0xffe3, 0xffe1, 0x6b
# KEYS as NotifyKeyboardKeysym's (keysym, state): modifiers pressed, the key pressed and released, modifiers released.
PRESSED = [(CTRL, 1), (SHIFT, 1), (K, 1), (K, 0), (SHIFT, 0), (CTRL, 0)]
NOTIFY = ["NotifyKeyboardKeysym"] * len(PRESSED)
ALL = HANDSHAKE + NOTIFY + ["Session.Close"]
SENT = None  # no `… yourself` line, exit 0
REFUSED = f"keybind-find: the desktop refused keyboard control: press {KEYS} yourself"
NO_KEYBOARD = f"keybind-find: portal AvailableDeviceTypes: the desktop offers no keyboard control (6): press {KEYS} yourself"
# MODE: (picks, token stored first, each pick's calls, each pick's restore_token, last stderr line (SENT: none),
# token file after, the one other `keybind-find: portal` line a pick prints, as a prefix, each pick's keysym events). A
# last line ending in `…` is a prefix, the rest of it the keys.
MODES = {
    # Approved with the dialog, then restored with its token.
    "send": (2, None, ALL, [None, TOKEN], SENT, TOKEN, None, PRESSED),
    # The dialog cancelled: nothing stored.
    "refused": (1, None, HANDSHAKE + ["Session.Close"], [None], REFUSED, None, None, []),
    # A stored token the portal no longer honours (revoked, used up): as xdg-desktop-portal, it drops it and shows the
    # dialog, so Start runs past the 5 s a restored one gets. The token is deleted: the next pick gets the dialog's time.
    "stale": (1, "stale_1", HANDSHAKE, ["stale_1"],
              f"keybind-find: portal Start: no answer within 5s: press {KEYS} yourself", None, None, []),
    # CreateSession fails before the portal sees the stored token: it is kept.
    "early-fail": (1, "early_1", ["AvailableDeviceTypes", "CreateSession"], [], "keybind-find: portal CreateSession: …",
                   "early_1", None, []),
    # Started with no restore_token (not remembered): the stored one is deleted.
    "no-token": (1, "old_1", ALL, ["old_1"], SENT, None, None, PRESSED),
    # SelectDevices refused: the session is closed all the same.
    "select-refused": (1, None, ["AvailableDeviceTypes", "CreateSession", "SelectDevices", "Session.Close"], [None],
                       REFUSED, None, None, []),
    # No keyboard among the device types (6: pointer and touchscreen).
    "no-keyboard": (1, None, ["AvailableDeviceTypes"], [], NO_KEYBOARD, None, None, []),
    # No AvailableDeviceTypes property at all: Get fails before the fake sees it.
    "no-property": (1, None, [], [], "keybind-find: portal AvailableDeviceTypes: …", None, None, []),
    # CreateSession's Request on another path than the predicted one: refused before any session.
    "other-path": (1, None, ["AvailableDeviceTypes", "CreateSession"], [],
                   "keybind-find: portal CreateSession: the request is …", None, None, []),
    # Another client on the bus answers Start first, with a refusal: ignored, the portal's own answer counts.
    "forged": (1, None, HANDSHAKE + ["forged"] + ALL[4:], [None], SENT, TOKEN, None, PRESSED),
    # Session.Close fails: its own line, and the pick still ends with the usual one.
    "close-fails": (1, None, ALL, [None], SENT, TOKEN, "keybind-find: portal Session.Close: ", PRESSED),
    # No portal on the bus at all, and none to start (the harness runs the pick on a bus with no services).
    "absent": (1, None, [], [], "keybind-find: portal AvailableDeviceTypes: …", None, None, []),
    # Start grants no keyboard (devices 2, the pointer): refused before any key, the session closed.
    "no-devices": (1, None, HANDSHAKE + ["Session.Close"], [None],
                   f"keybind-find: portal Start: the desktop granted no keyboard control (2): press {KEYS} yourself",
                   None, None, []),
    # The key's press fails: shift and ctrl, still down, are released (in reverse) before Close.
    "keysym-fails": (1, None, HANDSHAKE + NOTIFY[:5] + ["Session.Close"], [None],
                     "keybind-find: portal NotifyKeyboardKeysym: …", TOKEN, None, PRESSED[:3] + [(SHIFT, 0), (CTRL, 0)]),
}
# SelectDevices' options and their D-Bus types.
OPTION_TYPES = {"handle_token": "s", "types": "u", "persist_mode": "u", "restore_token": "s"}
FORGER = None  # the forged mode's second connection, kept open while the fake runs

XML = """
<node>
  <interface name="org.freedesktop.portal.RemoteDesktop">
    <method name="CreateSession">
      <arg type="a{sv}" name="options" direction="in"/><arg type="o" name="handle" direction="out"/>
    </method>
    <method name="SelectDevices">
      <arg type="o" name="session_handle" direction="in"/><arg type="a{sv}" name="options" direction="in"/>
      <arg type="o" name="handle" direction="out"/>
    </method>
    <method name="Start">
      <arg type="o" name="session_handle" direction="in"/><arg type="s" name="parent_window" direction="in"/>
      <arg type="a{sv}" name="options" direction="in"/><arg type="o" name="handle" direction="out"/>
    </method>
    <method name="NotifyKeyboardKeycode">
      <arg type="o" name="session_handle" direction="in"/><arg type="a{sv}" name="options" direction="in"/>
      <arg type="i" name="keycode" direction="in"/><arg type="u" name="state" direction="in"/>
    </method>
    <method name="NotifyKeyboardKeysym">
      <arg type="o" name="session_handle" direction="in"/><arg type="a{sv}" name="options" direction="in"/>
      <arg type="i" name="keysym" direction="in"/><arg type="u" name="state" direction="in"/>
    </method>
    PROPERTIES
  </interface>
  <interface name="org.freedesktop.portal.Session">
    <method name="Close"/>
  </interface>
</node>
"""
ROOT = "/org/freedesktop/portal/desktop"


def serve(log_path, mode):
    from gi.repository import Gio, GLib

    props = '<property name="version" type="u" access="read"/>'
    if mode != "no-property":
        props += '<property name="AvailableDeviceTypes" type="u" access="read"/>'
    info = Gio.DBusNodeInfo.new_for_xml(XML.replace("PROPERTIES", props))
    remote, session_iface = info.interfaces
    restoring = {}  # session path: the restore_token SelectDevices got
    notified = []  # the Notify calls so far
    loop = GLib.MainLoop()

    def log(entry):
        with open(log_path, "a") as f:
            f.write(json.dumps(entry) + "\n")

    def sender_part(sender):
        return sender.lstrip(":").replace(".", "_")

    def respond(conn, sender, token, code, results, delay=0, forge=False):
        path = f"{ROOT}/request/{sender_part(sender)}/{token}"

        def emit(by, code):
            by.emit_signal(sender, path, "org.freedesktop.portal.Request", "Response",
                           GLib.Variant("(ua{sv})", (code, results if by is conn else {})))
            return False

        if forge:
            # A second connection: another client, not the portal. Its Response is on the bus before the portal's.
            global FORGER
            FORGER = FORGER or Gio.DBusConnection.new_for_address_sync(
                Gio.dbus_address_get_for_bus_sync(Gio.BusType.SESSION, None),
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
                None, None)
            emit(FORGER, 1)
            FORGER.flush_sync(None)
            log({"method": "forged"})
            delay_ms = 500
        else:
            delay_ms = delay * 1000
        # After the method's reply, as a portal answers once its dialog closes.
        GLib.timeout_add(delay_ms, emit, conn, code)
        return path

    def on_session(conn, sender, path, iface, method, params, invocation):
        log({"method": f"Session.{method}", "path": path})
        if mode == "close-fails":
            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "fake close failure")
            return
        invocation.return_value(None)

    def on_call(conn, sender, path, iface, method, params, invocation):
        args = params.unpack()
        entry = {"method": method, "args": args}
        if method.startswith("Notify"):
            log(entry)
            notified.append(method)
            if mode == "keysym-fails" and len(notified) == 3:
                invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "fake keysym failure")
                return
            invocation.return_value(None)
            return
        options = args[-1]
        token = options.get("handle_token", "t")
        code, results, delay = 0, {}, 0
        if method == "CreateSession" and mode == "early-fail":
            log(entry)
            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "fake failure")
            return
        if method == "CreateSession":
            if mode == "other-path":
                token = "not_the_predicted_one"
            session = f"{ROOT}/session/{sender_part(sender)}/{options['session_handle_token']}"
            conn.register_object(session, session_iface, on_session, None, None)
            results = {"session_handle": GLib.Variant("s", session)}
            entry["session"] = session
        elif method == "SelectDevices":
            restoring[args[0]] = options.get("restore_token")
            dict_ = params.get_child_value(1)
            entry["types"] = {dict_.get_child_value(i).get_child_value(0).get_string():
                              dict_.get_child_value(i).get_child_value(1).get_variant().get_type_string()
                              for i in range(dict_.n_children())}
            code = 2 if mode == "select-refused" else 0
        elif method == "Start":
            restored = restoring.get(args[0])
            if mode == "refused":
                code = 1
            elif mode == "no-token":
                results = {"devices": GLib.Variant("u", 1)}
            else:
                devices = 2 if mode == "no-devices" else 1
                results = {"devices": GLib.Variant("u", devices), "restore_token": GLib.Variant("s", TOKEN)}
            # The dialog: when nothing is restored, or the stored token isn't honoured.
            delay = 0 if (restored and mode != "stale") or code else DIALOG
        log(entry)
        forge = mode == "forged" and method == "Start"
        invocation.return_value(GLib.Variant("(o)", (respond(conn, sender, token, code, results, delay, forge),)))

    def get_property(conn, sender, path, iface, name):
        log({"method": name})
        types = 6 if mode == "no-keyboard" else 7
        return GLib.Variant("u", {"AvailableDeviceTypes": types, "version": 2}[name])

    def on_bus(conn, name):
        conn.register_object(ROOT, remote, on_call, get_property, None)

    def on_acquired(conn, name):
        print("ready", flush=True)

    # REPLACE takes the name from an owner that allows it; DO_NOT_QUEUE: with any other owner, quit (the name lost).
    Gio.bus_own_name(Gio.BusType.SESSION, "org.freedesktop.portal.Desktop",
                     Gio.BusNameOwnerFlags.REPLACE | Gio.BusNameOwnerFlags.DO_NOT_QUEUE,
                     on_bus, on_acquired, lambda *_: loop.quit())
    loop.run()


def check(mode, log_path, token_file, stderrs):
    _, _, order, restore, last, token_after, extra, keys = MODES[mode]
    problems = []
    try:
        calls = [json.loads(line) for line in open(log_path)]
    except OSError:
        calls = []
    names = [c["method"] for c in calls]
    if names != order * len(stderrs):
        problems.append(f"calls {names}")
    session = None
    selects = []
    pressed = []  # each pick's keysym events, a pick starting at its CreateSession
    for c in calls:
        if c["method"] == "CreateSession":
            session = c.get("session")
            pressed.append([])
        elif c["method"] in ("SelectDevices", "Start", "NotifyKeyboardKeysym") and c["args"][0] != session:
            problems.append(f"{c['method']} on {c['args'][0]}, not {session}")
        if c["method"] == "SelectDevices":
            selects.append(c["args"][1])
            wrong = {k: t for k, t in c["types"].items() if OPTION_TYPES.get(k) != t}
            if wrong:
                problems.append(f"SelectDevices option types {wrong}")
        if c["method"] == "Start" and c["args"][1] != "":
            problems.append(f"Start's parent window {c['args'][1]!r}")
        if c["method"] == "NotifyKeyboardKeysym":
            if c["args"][1] != {}:
                problems.append(f"NotifyKeyboardKeysym options {c['args'][1]}")
            if pressed:
                pressed[-1].append(tuple(c["args"][2:]))
    for i, got in enumerate(pressed):
        if got != keys:
            problems.append(f"pick {i + 1}'s keysyms {[(hex(k), s) for k, s in got]}")
    for i, options in enumerate(selects):
        want = {"types": 1, "persist_mode": 2}
        if i < len(restore) and restore[i]:
            want["restore_token"] = restore[i]
        got = {k: v for k, v in options.items() if k != "handle_token"}
        if got != want:
            problems.append(f"SelectDevices {i + 1}: {got}")
    if token_after is None:
        if os.path.lexists(token_file):
            problems.append(f"portal-token is still there: {open(token_file).read()!r}")
    else:
        try:
            st = os.stat(token_file)
            token = open(token_file).read()
            if token != token_after or st.st_mode & 0o777 != 0o600:
                problems.append(f"portal-token {token!r} mode {st.st_mode & 0o777:o}")
        except OSError as e:
            problems.append(f"portal-token: {e}")
    for path in stderrs:
        # The last line: providers that can't run here (GNOME's other schemas) say so first.
        lines = open(path).read().strip().splitlines() or [""]
        end = lines[-1]
        try:
            rc = open(path[:-len(".err")] + ".rc").read().strip()
        except OSError as e:
            rc = str(e)
        if rc != ("0" if last is SENT else "1"):
            problems.append(f"{os.path.basename(path)}: exit {rc}")
        if last is SENT:
            ok = not any(line.endswith(" yourself") for line in lines)
            end = None
        elif last.endswith("…"):
            ok = end.startswith(last[:-1]) and end.endswith(f": press {KEYS} yourself")
        else:
            ok = end == last
        if not ok:
            problems.append(f"{os.path.basename(path)}: {end or lines!r}")
        portal = [line for line in lines[:-1 if end else None] if line.startswith("keybind-find: portal")]
        if not (portal == [] if extra is None else len(portal) == 1 and portal[0].startswith(extra)):
            problems.append(f"{os.path.basename(path)}: {portal!r}")
    print("ok\t" + " ".join(names[:len(order)]) if not problems else "FAIL\t" + "; ".join(problems))
    return 1 if problems else 0


if __name__ == "__main__":
    if sys.argv[1] == "--plan":
        picks, token = MODES[sys.argv[2]][:2]
        print(picks, token or "-")
    elif sys.argv[1] == "--check":
        sys.exit(check(sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5:]))
    else:
        serve(sys.argv[1], os.environ.get("FAKE_PORTAL", "send"))
