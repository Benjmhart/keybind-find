"""A fake xdg-desktop-portal RemoteDesktop for tests/e2e/harness.sh, and the check of what it was asked.

usage: fake_portal.py LOG            serve on the session bus, appending each call to LOG as one JSON line
       fake_portal.py --check LOG TOKEN_FILE STDERR...
                                     check the calls of one pick per STDERR file, each STDERR, and TOKEN_FILE

Owns org.freedesktop.portal.Desktop and exports /org/freedesktop/portal/desktop with
org.freedesktop.portal.RemoteDesktop (version 2, AvailableDeviceTypes 7). Each Request is answered by a Response on
its predicted path (/org/freedesktop/portal/desktop/request/SENDER/TOKEN), sent to the caller. Start answers
FAKE_PORTAL_START (default 0) and, on 0, the restore_token `fake-1`. A session has
org.freedesktop.portal.Session.Close on its path.

The check: with FAKE_PORTAL_START unset (two picks), each pick's calls are AvailableDeviceTypes, CreateSession,
SelectDevices, Start, Session.Close; the first SelectDevices has no restore_token, the second carries `fake-1`, both
types 1 and persist_mode 2; TOKEN_FILE holds `fake-1`, mode 0600; each STDERR ends with the line keybind-find ends a
pick with until it sends keys. With FAKE_PORTAL_START=1, the same calls and the refusal line. Prints `ok` or what
differs; exits 1 when something does.
"""
import json
import os
import sys

KEYS = "ctrl+shift+k"
TOKEN = "fake-1"
ORDER = ["AvailableDeviceTypes", "CreateSession", "SelectDevices", "Start", "Session.Close"]
SENT_LINE = (f"keybind-find: can't send keys under GNOME on Wayland (no virtual-keyboard protocol for wtype): "
             f"press {KEYS} yourself")
REFUSED_LINE = f"keybind-find: the desktop refused keyboard control: press {KEYS} yourself"

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
    <property name="AvailableDeviceTypes" type="u" access="read"/>
    <property name="version" type="u" access="read"/>
  </interface>
  <interface name="org.freedesktop.portal.Session">
    <method name="Close"/>
  </interface>
</node>
"""
ROOT = "/org/freedesktop/portal/desktop"


def serve(log_path):
    from gi.repository import Gio, GLib

    info = Gio.DBusNodeInfo.new_for_xml(XML)
    remote, session_iface = info.interfaces
    start_response = int(os.environ.get("FAKE_PORTAL_START", "0"))
    loop = GLib.MainLoop()

    def log(method, args):
        with open(log_path, "a") as f:
            f.write(json.dumps({"method": method, "args": args}) + "\n")

    def sender_part(sender):
        return sender.lstrip(":").replace(".", "_")

    def respond(conn, sender, token, code, results):
        path = f"{ROOT}/request/{sender_part(sender)}/{token}"
        # After the method's reply, as a portal answers once its dialog closes.
        GLib.idle_add(lambda: conn.emit_signal(sender, path, "org.freedesktop.portal.Request", "Response",
                                               GLib.Variant("(ua{sv})", (code, results))) and False)
        return path

    def on_session(conn, sender, path, iface, method, params, invocation):
        log(f"Session.{method}", {"path": path})
        invocation.return_value(None)

    def on_call(conn, sender, path, iface, method, params, invocation):
        args = params.unpack()
        log(method, args)
        if method.startswith("Notify"):
            invocation.return_value(None)
            return
        options = args[-1]
        token = options.get("handle_token", "t")
        code, results = 0, {}
        if method == "CreateSession":
            session = f"{ROOT}/session/{sender_part(sender)}/{options['session_handle_token']}"
            conn.register_object(session, session_iface, on_session, None, None)
            results = {"session_handle": GLib.Variant("s", session)}
        elif method == "Start":
            code = start_response
            if code == 0:
                results = {"devices": GLib.Variant("u", 1), "restore_token": GLib.Variant("s", TOKEN)}
        invocation.return_value(GLib.Variant("(o)", (respond(conn, sender, token, code, results),)))

    def get_property(conn, sender, path, iface, name):
        log(name, None)
        return GLib.Variant("u", {"AvailableDeviceTypes": 7, "version": 2}[name])

    def on_bus(conn, name):
        conn.register_object(ROOT, remote, on_call, get_property, None)

    Gio.bus_own_name(Gio.BusType.SESSION, "org.freedesktop.portal.Desktop", Gio.BusNameOwnerFlags.NONE,
                     on_bus, None, lambda *_: loop.quit())
    loop.run()


def check(log_path, token_file, stderrs):
    problems = []
    try:
        calls = [json.loads(line) for line in open(log_path)]
    except OSError as e:
        calls = []
        problems.append(f"no log: {e}")
    refused = os.environ.get("FAKE_PORTAL_START", "0") != "0"
    names = [c["method"] for c in calls]
    if names != ORDER * len(stderrs):
        problems.append(f"calls {names}")
    selects = [c["args"][1] for c in calls if c["method"] == "SelectDevices"]
    for i, options in enumerate(selects):
        want = {"types": 1, "persist_mode": 2, **({"restore_token": TOKEN} if i else {})}
        got = {k: v for k, v in options.items() if k != "handle_token"}
        if got != want:
            problems.append(f"SelectDevices {i + 1}: {got}")
    if not refused:
        try:
            st = os.stat(token_file)
            token = open(token_file).read()
            if token != TOKEN or st.st_mode & 0o777 != 0o600:
                problems.append(f"portal-token {token!r} mode {st.st_mode & 0o777:o}")
        except OSError as e:
            problems.append(f"portal-token: {e}")
    for path in stderrs:
        # The last line: providers that can't run here (GNOME's other schemas) say so first.
        err = (open(path).read().strip().splitlines() or [""])[-1]
        if err != (REFUSED_LINE if refused else SENT_LINE):
            problems.append(f"{os.path.basename(path)}: {err!r}")
    print("ok\t" + " ".join(names[:len(ORDER)]) if not problems else "FAIL\t" + "; ".join(problems))
    return 1 if problems else 0


if __name__ == "__main__":
    if sys.argv[1] == "--check":
        sys.exit(check(sys.argv[2], sys.argv[3], sys.argv[4:]))
    serve(sys.argv[1])
