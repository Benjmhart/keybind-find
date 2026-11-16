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


REPORTS = {"kitty": kitty_report}

KNOWN = {
    "kitty": [
        ("*hyper+*", "kitty.rs leaves out a hyper modifier: keybind-find's notation can't show it"),
        ("*caps_lock+*", "kitty.rs leaves out a caps_lock modifier: keybind-find's notation can't show it"),
        ("*num_lock+*", "kitty.rs leaves out a num_lock modifier: keybind-find's notation can't show it"),
        ("*[?]*", "a non-ASCII character key has no X keysym: kitty.rs leaves it out"),
        ("*focus:*", "--when-focus-on maps apply only over some windows: kitty.rs leaves them out"),
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
    sheet = {"binds": {"k": [{"key": "Escape", "when": {"keyboard_mode": ["mv"]}}, {"key": "ctrl+a"}]}}
    assert dump_keys(sheet) == {"keyboard_mode=mv\tEscape", "ctrl+a"}
    print("oracle.py: self-test ok")
    return 0


if __name__ == "__main__":
    sys.exit(self_test() if sys.argv[1:] == ["--self-test"] else main(*sys.argv[1:4]))
