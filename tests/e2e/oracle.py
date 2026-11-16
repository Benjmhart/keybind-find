"""The program as oracle, for tests/e2e/harness.sh's `o` cases: compares a program's own report of its effective keys
with `keybind-find dump PROVIDER` on the same config.

usage: oracle.py PROVIDER REPORT DUMP.json     (or: oracle.py --self-test)
Prints `ok` with both counts, or `FAIL` naming the keys only one side has; exits 1 on a difference, a stale known
difference, an empty report or an unknown provider.

Per provider, three pieces: REPORTS[provider] turns the report into a set of keys in keybind-find's notation (scope
first where the program has modes: `keyboard_mode=MODE<TAB>KEY`), dump_keys builds the same set from the dump (shared:
every bind's key, after its `when` scopes), and KNOWN[provider] lists the known differences, each an fnmatch pattern
with its reason. A known difference that no longer differs fails the case, so the list can't rot.
"""
import fnmatch
import json
import re
import sys

# --- kitty ------------------------------------------------------------------------------------------------------------
# kitty's key names (glfw_get_key_name: kitty/glfw.c and key_names.py at v0.49.2) as X keysyms. Its own table, not
# keybind-find's: the oracle must not reuse the code it checks.
KITTY_CHARS = {
    " ": "space", "/": "slash", "-": "minus", "[": "bracketleft", "]": "bracketright", ",": "comma", ".": "period",
    ";": "semicolon", ":": "colon", "'": "apostrophe", '"': "quotedbl", "<": "less", ">": "greater", "=": "equal",
    "+": "plus", "_": "underscore", "?": "question", "!": "exclam", "@": "at", "#": "numbersign", "$": "dollar",
    "%": "percent", "^": "asciicircum", "&": "ampersand", "*": "asterisk", "(": "parenleft", ")": "parenright",
    "{": "braceleft", "}": "braceright", "|": "bar", "\\": "backslash", "`": "grave", "~": "asciitilde",
}
KITTY_NAMES = {
    "escape": "Escape", "enter": "Return", "tab": "Tab", "backspace": "BackSpace", "insert": "Insert",
    "delete": "Delete", "left": "Left", "right": "Right", "up": "Up", "down": "Down", "page_up": "Prior",
    "page_down": "Next", "home": "Home", "end": "End", "caps_lock": "Caps_Lock", "scroll_lock": "Scroll_Lock",
    "num_lock": "Num_Lock", "print_screen": "Print", "pause": "Pause", "menu": "Menu", "space": "space",
    "kp_page_up": "KP_Prior", "kp_page_down": "KP_Next",
    "media_play": "XF86AudioPlay", "media_play_pause": "XF86AudioPlay", "media_pause": "XF86AudioPause",
    "media_stop": "XF86AudioStop", "media_fast_forward": "XF86AudioForward", "media_rewind": "XF86AudioRewind",
    "media_track_next": "XF86AudioNext", "media_track_previous": "XF86AudioPrev", "media_record": "XF86AudioRecord",
    "lower_volume": "XF86AudioLowerVolume", "raise_volume": "XF86AudioRaiseVolume", "mute_volume": "XF86AudioMute",
    "iso_level3_shift": "ISO_Level3_Shift", "iso_level5_shift": "ISO_Level5_Shift",
}
# left_shift, right_control, …: X's Shift_L, Control_R, …
for side in ("left", "right"):
    for mod, x in (("shift", "Shift"), ("control", "Control"), ("alt", "Alt"), ("super", "Super"), ("hyper", "Hyper"),
                   ("meta", "Meta")):
        KITTY_NAMES[f"{side}_{mod}"] = f"{x}_{side[0].upper()}"
# ponytail: a native key (`map ctrl+XF86AudioPlay`) prints as its code (`0x1008ff14`, glfw isn't initialised under
# +runpy), so it shows as a difference; give the oracle a keysym table when a case maps one.
# keybind-find's chord order: these first, in this order; any other modifier after them, as written.
MOD_ORDER = ["super", "ctrl", "alt", "shift"]


def kitty_key(name):
    """One kitty key (`ctrl+shift+page_up`, `ctrl+shift++`) in keybind-find's notation."""
    if name.endswith("++") or name == "+":
        mods, key = name[:-2].split("+") if len(name) > 1 else [], "+"
    else:
        *mods, key = name.split("+")
    mods = [m for m in mods if m]
    if len(key) == 1:
        sym = key.lower() if key.isalnum() and key.isascii() else KITTY_CHARS.get(key, "?" + key)
    elif key.lower() in KITTY_NAMES:
        sym = KITTY_NAMES[key.lower()]
    elif key[:1] in "fF" and key[1:].isdigit():
        sym = "F" + key[1:]
    elif key.lower().startswith("kp_"):
        rest = key[3:]
        sym = "KP_" + (rest if rest.isdigit() else rest[:1].upper() + rest[1:].lower())
    elif key.startswith("XF86"):
        sym = key
    else:
        sym = "!" + key  # a name this table lacks: a difference no KNOWN pattern covers
    ordered = [m for m in MOD_ORDER if m in mods] + [m for m in dict.fromkeys(mods) if m not in MOD_ORDER]
    return "+".join(ordered + [sym])


def kitty_report(text):
    """oracle-kitty.py's lines (MODE<TAB>KEYS<TAB>FLAGS) as keys; a --when-focus-on map gets a `focus:` prefix, and
    a line that isn't one (stray output) is kept as `!line:…`, a difference no KNOWN pattern covers."""
    keys = set()
    for line in filter(None, text.splitlines()):
        if line.count("\t") != 2:
            keys.add("!line:" + line)
            continue
        mode, seq, flags = line.split("\t")
        key = " ".join(kitty_key(k) for k in seq.split(" "))
        key = ("focus:" if flags == "focus" else "") + key
        keys.add(f"keyboard_mode={mode}\t{key}" if mode else key)
    return keys


# --- wezterm ----------------------------------------------------------------------------------------------------------
# `wezterm show-keys --lua` (wezterm-gui/src/inputmap.rs dump_config and lua_key at 76b606ec5): `keys = {`, then
# `key_tables = {` with one `<name> = {` per table, and one `{ key = '<k>', mods = '<A|B>', action = … },` per bind.
# Key names as lua_key_code prints them (KeyCode's names, `phys:` + PhysKeyCode's), in its own table. That table
# mirrors wezterm.rs's, so a mapping mistake made in both places passes: this oracle checks keybind-find's reading of
# the text form, its sections and its dedupe, not the key-name table.
WEZTERM_NAMES = {
    "Enter": "Return", "Escape": "Escape", "Backspace": "BackSpace", "Space": "space", "Tab": "Tab",
    "PageUp": "Prior", "PageDown": "Next", "Home": "Home", "End": "End", "Insert": "Insert", "LeftArrow": "Left",
    "RightArrow": "Right", "UpArrow": "Up", "DownArrow": "Down", "Copy": "XF86Copy", "Cut": "XF86Cut",
    "Paste": "XF86Paste", "VolumeUp": "XF86AudioRaiseVolume", "VolumeDown": "XF86AudioLowerVolume",
    "VolumeMute": "XF86AudioMute",
}
WEZTERM_PHYS = {
    "Return": "Return", "Delete": "Delete", "Backslash": "backslash", "Comma": "comma", "Equal": "equal",
    "Grave": "grave", "LeftBracket": "bracketleft", "RightBracket": "bracketright", "Minus": "minus",
    "Period": "period", "Quote": "apostrophe", "Semicolon": "semicolon", "Slash": "slash",
}
# The lua form has no leader line: the leader of tests/e2e/home/.config/wezterm/wezterm.lua (`config.leader`).
WEZTERM_LEADER = "ctrl+a"


def wezterm_key(key, mods):
    """One lua bind's key (`X`, `phys:k`, `F5`) and mods (`SHIFT|CTRL`, `NONE`, `LEADER`) in keybind-find's notation."""
    names = [m.removeprefix("LEFT_").removeprefix("RIGHT_") for m in mods.split("|") if m and m != "NONE"]
    lead = "LEADER" in names
    mods = [{"SHIFT": "shift", "ALT": "alt", "CTRL": "ctrl", "SUPER": "super"}.get(m, "!" + m) for m in names
            if m != "LEADER"]
    if key.startswith("phys:"):
        k = key[5:]
        sym = k.lower() if len(k) == 1 and k.isascii() and k.isalnum() else WEZTERM_PHYS.get(k) or WEZTERM_NAMES.get(
            k) or (k if k[:1] == "F" and k[1:].isdigit() else "!" + key)
    elif len(key) == 1:
        if key.isascii() and key.isupper():
            sym, mods = key.lower(), mods + ["shift"]
        elif key.isascii() and key.isalnum():
            sym = key
        else:
            sym = KITTY_CHARS.get(key, "?" + key)
    elif key in WEZTERM_NAMES:
        sym = WEZTERM_NAMES[key]
    elif key[:1] == "F" and key[1:].isdigit():
        sym = key
    elif key.startswith("Numpad") and key[6:].isdigit():
        sym = "KP_" + key[6:]
    elif key.startswith("RawCode("):
        sym = "raw:" + key[8:-1]
    else:
        sym = "!" + key
    ordered = [m for m in MOD_ORDER if m in mods] + [m for m in dict.fromkeys(mods) if m not in MOD_ORDER]
    chord = "+".join(ordered + [sym])
    return f"{WEZTERM_LEADER} {chord}" if lead else chord


def debug_char(key):
    """A control character as lua_key_code writes it (Rust's escape_debug: `\\u{1}`, `\\n`, `\\0`) back to the
    character, which then has no keysym, as in wezterm.rs."""
    m = re.fullmatch(r"\\u\{([0-9a-f]+)\}", key)
    if m:
        return chr(int(m.group(1), 16))
    return {"\\n": "\n", "\\0": "\0"}.get(key, key)


WEZTERM_BIND = re.compile(r"\{ key = '((?:[^'\\]|\\.)*)', mods = '([^']*)', action = .*\},$")


def wezterm_report(text):
    """show-keys --lua's binds as keys, `key_table=NAME<TAB>` before a key table's; a line that isn't a table or a
    bind (stray output) is kept as `!line:…`."""
    keys, table = set(), None
    for line in text.splitlines():
        s = line.strip()
        bind = WEZTERM_BIND.match(s)
        if bind:
            key = debug_char(re.sub(r"\\(.)", r"\1", bind.group(1)))
            k = wezterm_key(key, bind.group(2))
            keys.add(f"key_table={table}\t{k}" if table else k)
        elif s in ("keys = {", "key_tables = {"):
            table = None
        elif s.endswith(" = {"):
            table = s[:-4]
        elif s not in ("", "}", "},", "return {", "local wezterm = require 'wezterm'", "local act = wezterm.action"):
            keys.add("!line:" + s)
    return keys


REPORTS = {"kitty": kitty_report, "wezterm": wezterm_report}

KNOWN = {
    "kitty": [
        ("*hyper+*", "kitty.rs leaves out a hyper modifier: keybind-find's notation can't show it"),
        ("*caps_lock+*", "kitty.rs leaves out a caps_lock modifier: keybind-find's notation can't show it"),
        ("*num_lock+*", "kitty.rs leaves out a num_lock modifier: keybind-find's notation can't show it"),
        ("*[?]*", "a non-ASCII character key has no X keysym: kitty.rs leaves it out"),
        ("*focus:*", "--when-focus-on maps apply only over some windows: kitty.rs leaves them out"),
    ],
    "wezterm": [
        ("*[?]*", "a non-ASCII or control character key has no X keysym: wezterm.rs leaves it out"),
        ("*XF86Audio*", "wezterm.rs translates only the named keys it lists (no media keys): it leaves them out"),
        ("*raw:*", "a raw key code has no X keysym: wezterm.rs leaves it out"),
        ("key_table=*[ ]*\t*", "a key table name with a space can't be a scope: wezterm.rs leaves its binds out"),
    ],
}


# --- shared -----------------------------------------------------------------------------------------------------------
def dump_keys(sheet):
    """Every bind's key, after its `when` scopes (`name=value<TAB>`, sorted), from one `dump PROVIDER` sheet."""
    keys = set()
    for binds in sheet["binds"].values():
        for b in binds:
            scopes = []
            for name, v in sorted((b.get("when") or {}).items()):
                scopes += [f"{name}={x}" for x in (v if isinstance(v, list) else [v])]
            keys.add("\t".join(scopes + [b["key"]]))
    return keys


def compare(report, dump, known):
    """(only in the report, only in the dump, stale known patterns). A known difference is a key the program has and
    keybind-find leaves out, so KNOWN covers only report-side keys: a dump-only key always fails."""
    # `!…` (a name or line the oracle can't read) is never known.
    is_known = lambda k: not k.startswith("!") and any(fnmatch.fnmatchcase(k, p) for p, _ in known)
    only_r, only_d = report - dump, dump - report
    stale = [p for p, _ in known if not any(is_known(k) and fnmatch.fnmatchcase(k, p) for k in only_r)]
    return sorted(k for k in only_r if not is_known(k)), sorted(only_d), stale


def listed(keys):
    shown = ", ".join(k.replace("\t", " ") for k in keys[:10])
    return shown + (f" (+{len(keys) - 10} more)" if len(keys) > 10 else "")


def main(provider, report_path, dump_path):
    if provider not in REPORTS:
        print(f"FAIL\tno oracle for {provider} (tests/e2e/oracle.py REPORTS)")
        return 1
    try:
        report = REPORTS[provider](open(report_path).read())
        sheet = json.load(open(dump_path))
        dump = dump_keys(sheet[0] if isinstance(sheet, list) else sheet)
    except Exception as e:
        print(f"FAIL\tunreadable report or dump: {e!r}")
        return 1
    if not report:
        print(f"FAIL\t{provider}'s report is empty")
        return 1
    known = KNOWN.get(provider, [])
    only_r, only_d, stale = compare(report, dump, known)
    counts = f"{provider}:{len(report)} keybind-find:{len(dump)} known:{len(report - dump) - len(only_r)}"
    problems = []
    if only_r:
        problems.append(f"only in {provider}: {listed(only_r)}")
    if only_d:
        problems.append(f"only in keybind-find: {listed(only_d)}")
    if stale:
        problems.append(f"stale known difference: {', '.join(stale)}")
    print(("FAIL\t" + "; ".join(problems) if problems else "ok") + f"\t{counts}")
    return 1 if problems else 0


def self_test():
    assert kitty_key("ctrl+shift+page_up") == "ctrl+shift+Prior"
    assert kitty_key("shift+ctrl++") == "ctrl+shift+plus"
    assert kitty_key("ctrl+shift+/") == "ctrl+shift+slash"
    assert kitty_key("ctrl+shift+alt+t") == "ctrl+alt+shift+t"
    assert kitty_key("hyper+ctrl+kp_add") == "ctrl+hyper+KP_Add"
    assert kitty_key("ctrl+shift+f11") == "ctrl+shift+F11"
    assert kitty_key("ctrl+bogus_name") == "ctrl+!bogus_name"
    assert kitty_key("ctrl+é") == "ctrl+?é"
    assert kitty_key("ctrl+left_shift") == "ctrl+Shift_L" and kitty_key("media_play") == "XF86AudioPlay"
    assert kitty_report("\tctrl+shift+p f\t\nmv\tescape\t\n\n") == {"ctrl+shift+p f", "keyboard_mode=mv\tEscape"}
    assert kitty_report("warning: x\n") == {"!line:warning: x"}
    assert compare({"!line:hyper+x ?"}, set(), [("*hyper+*", "r")]) == (["!line:hyper+x ?"], [], ["*hyper+*"])
    known = [("*hyper+*", "r"), ("stale*", "r")]
    report = {"ctrl+a", "ctrl+b", "hyper+x"}
    assert compare(report, {"ctrl+a", "ctrl+c"}, known) == (["ctrl+b"], ["ctrl+c"], ["stale*"])
    # A known pattern never hides a key only keybind-find has, nor counts it as still differing.
    assert compare({"ctrl+a"}, {"ctrl+a", "hyper+y"}, known[:1]) == ([], ["hyper+y"], ["*hyper+*"])
    assert compare(report, {"ctrl+a", "ctrl+b"}, known[:1]) == ([], [], [])
    # Every kitty KNOWN pattern is literal enough not to swallow an ordinary key (fnmatch's `?` is any character).
    for k in ("keyboard_mode=m\tctrl+shift+Down", "ctrl+!media_play"):
        assert not [p for p, _ in KNOWN["kitty"] if fnmatch.fnmatchcase(k, p)], k
    assert wezterm_key("X", "CTRL") == "ctrl+shift+x" and wezterm_key("x", "SHIFT|CTRL") == "ctrl+shift+x"
    assert wezterm_key("c", "LEADER") == "ctrl+a c" and wezterm_key("R", "LEADER") == "ctrl+a shift+r"
    assert wezterm_key("phys:K", "ALT|CTRL") == "ctrl+alt+k" and wezterm_key("phys:Space", "NONE") == "space"
    assert wezterm_key("'", "SHIFT|ALT|CTRL") == "ctrl+alt+shift+apostrophe" and wezterm_key("Numpad5", "NONE") == "KP_5"
    assert wezterm_key("LeftArrow", "SUPER") == "super+Left" and wezterm_key("RawCode(12)", "CTRL") == "ctrl+raw:12"
    assert wezterm_key("é", "NONE") == "?é" and wezterm_key("Hyper", "NONE") == "!Hyper"
    assert wezterm_key("x", "ENHANCED_KEY") == "!ENHANCED_KEY+x"
    assert wezterm_key("x", "LEFT_ALT|RIGHT_CTRL") == "ctrl+alt+x"
    assert debug_char("\\u{1}") == "\x01" and debug_char("\\n") == "\n" and debug_char("\\") == "\\"
    assert wezterm_key(debug_char("\\u{1}"), "CTRL") == "ctrl+?\x01"
    assert wezterm_report("    { key = '\\\\u{1}', mods = 'CTRL', action = act.Nop },\n") == {"ctrl+?\x01"}
    assert wezterm_report("    { key = '\\\\', mods = 'CTRL', action = act.Nop },\n") == {"ctrl+backslash"}
    lua = ("return {\n  keys = {\n    { key = '\\'', mods = 'CTRL', action = act.Nop },\n  },\n  key_tables = {\n"
           "    e2e wide = {\n      { key = 'Escape', mods = 'NONE', action = act.PopKeyTable },\n    },\n  }\n}\nstray\n")
    assert wezterm_report(lua) == {"ctrl+apostrophe", "key_table=e2e wide\tEscape", "!line:stray"}
    for k in ("key_table=e2e\tctrl+a c", "ctrl+a shift+r", "XF86Copy", "key_table=copy_mode\tshift+dollar"):
        assert not [p for p, _ in KNOWN["wezterm"] if fnmatch.fnmatchcase(k, p)], k
    sheet = {"binds": {"k": [{"key": "Escape", "when": {"keyboard_mode": ["mv"]}}, {"key": "ctrl+a"}]}}
    assert dump_keys(sheet) == {"keyboard_mode=mv\tEscape", "ctrl+a"}
    print("oracle.py: self-test ok")
    return 0


if __name__ == "__main__":
    sys.exit(self_test() if sys.argv[1:] == ["--self-test"] else main(*sys.argv[1:4]))
