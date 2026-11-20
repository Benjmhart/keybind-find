"""The program as oracle, for tests/e2e/harness.sh's `o` cases: compares a program's own report of its effective keys
with `keybind-find dump PROVIDER` on the same config.

usage: oracle.py PROVIDER REPORT DUMP.json     (or: oracle.py --self-test)
Prints `ok` with both counts, or `FAIL` naming the keys only one side has; exits 1 on a difference, a stale known
difference, an empty report or an unknown provider.

Per provider, three pieces: REPORTS[provider] turns the report into a set of keys in keybind-find's notation (scope
first where the program has modes: `keyboard_mode=MODE<TAB>KEY`), dump_keys builds the same set from the dump (shared:
every bind's key, after its `when` scopes), and KNOWN[provider] lists the known differences, each an fnmatch pattern
with its reason (and "dump" for a key only keybind-find has). A known difference that no longer differs fails the case,
so the list can't rot.
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
# Key names as lua_key_code prints them (KeyCode's names, `phys:` + PhysKeyCode's), in its own table. The oracle
# compares keys and scopes only, not actions: it checks keybind-find's reading of the text form, its sections and its
# key notation. A bind deduped away with a distinct action on a key already listed, or a wrong action, isn't caught;
# and the name tables mirror wezterm.rs's, so a mapping mistake made in both places passes.
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


# --- zellij -----------------------------------------------------------------------------------------------------------
# `zellij setup --dump-keybinds PRESET` (0.46.0): the preset file itself, read here by bind lines and block depth (not
# KDL): `preset { defaults { primary "Ctrl" … } }` gives the placeholders' values, each `keybinds` child block names its
# modes (a mode, or `shared`/`shared_except`/`shared_among` over 0.46's InputMode list), each `bind "k1" "k2" { … }`
# line in one gives keys; `mousebinds` and `//` comment lines are skipped. Keys and modes only, not actions: it checks
# keybind-find's KDL reading, its shared* expansion and its key notation. A preset holds no user config, so the user's
# changes (tests/e2e/home/.config/zellij/config.kdl) are the KNOWN differences, each failing as stale if keybind-find
# stops applying it.
ZELLIJ_MODES = ["normal", "locked", "resize", "pane", "tab", "scroll", "entersearch", "search", "renametab",
                "renamepane", "session", "move", "prompt", "tmux", "renamesession"]
ZELLIJ_NAMES = {
    "pagedown": "Next", "pageup": "Prior", "left": "Left", "right": "Right", "up": "Up", "down": "Down",
    "home": "Home", "end": "End", "backspace": "BackSpace", "delete": "Delete", "del": "Delete", "insert": "Insert",
    "tab": "Tab", "esc": "Escape", "enter": "Return", "space": "space", "capslock": "Caps_Lock",
    "scrolllock": "Scroll_Lock", "numlock": "Num_Lock", "printscreen": "Print", "pause": "Pause", "menu": "Menu",
}
ZELLIJ_NAMES.update({f"f{n}": f"F{n}" for n in range(1, 13)})
KDL_STRING = re.compile(r'"((?:[^"\\]|\\.)*)"')


def zellij_key(text):
    """One zellij key (`Ctrl Alt h`, `H`, `PageDown`) in keybind-find's notation."""
    *mods, bare = text.split() or ["!empty"]
    mods = [m.lower() if m.lower() in MOD_ORDER else "!" + m for m in mods]
    if bare.lower() in ZELLIJ_NAMES:
        sym = ZELLIJ_NAMES[bare.lower()]
    elif len(bare) == 1 and bare.isascii() and bare.isalnum():
        sym = bare.lower()
        if bare.isupper():
            mods.append("shift")
    elif len(bare) == 1:
        sym = KITTY_CHARS.get(bare, "?" + bare)
    else:
        sym = "!" + bare
    ordered = [m for m in MOD_ORDER if m in mods] + [m for m in dict.fromkeys(mods) if m not in MOD_ORDER]
    return "+".join(ordered + [sym])


def zellij_modes(header):
    """The modes a `keybinds` child block's header line names."""
    name, args = header.split()[0], [a.lower() for a in KDL_STRING.findall(header)]
    if name == "shared":
        return list(ZELLIJ_MODES)
    if name == "shared_except":
        return [m for m in ZELLIJ_MODES if m not in args]
    if name == "shared_among":
        return [m for m in ZELLIJ_MODES if m in args]
    return [name.lower()]


def zellij_report(text):
    """A preset's binds as `input_mode=MODE<TAB>KEY`; a line in a mode block that isn't a bind is kept as `!line:…`."""
    keys, leaders, stack, modes = set(), {}, [], []
    for line in text.splitlines():
        s = line.strip()
        if not s or s.startswith("//"):
            continue
        # Braces inside strings (`"{primary} g"`) don't count.
        bare = KDL_STRING.sub('""', s)
        net = bare.count("{") - bare.count("}")
        if stack == ["preset", "defaults"] and KDL_STRING.search(s):
            leaders[s.split()[0]] = KDL_STRING.search(s).group(1)
        elif stack == ["keybinds"] and s != "}":
            # A mode header is `NAME [args] {` alone on its line; anything else here (a block opened and closed on one
            # line, a name with its `{` on the next) isn't read: its binds go nowhere, and the line is a difference.
            if net == 1 and bare.endswith("{") and re.match(r"[A-Za-z_]", s):
                modes = zellij_modes(s)
            else:
                keys.add("!line:" + s)
                modes = []
        elif len(stack) == 2 and stack[0] == "keybinds":
            if s.startswith("bind "):
                for k in KDL_STRING.findall(bare_args(s)):
                    k = re.sub(r"\\(.)", r"\1", k)
                    for name, value in leaders.items():
                        k = k.replace("{" + name + "}", value)
                    keys.update(f"input_mode={m}\t{zellij_key(k)}" for m in modes)
            elif s != "}":
                keys.add("!line:" + s)
        if net > 0:
            stack += [s.split()[0]] * net
        elif net < 0:
            del stack[net:]
    return keys


def bare_args(s):
    """A bind line's part before its action block."""
    m = re.match(r'bind((?:\s+"(?:[^"\\]|\\.)*")+)', s)
    return m.group(1) if m else ""


# --- awesome ----------------------------------------------------------------------------------------------------------
# `cases`' report chunk, sent by dbus-send: one string (`method return …` then `   string "…"`), `;`-separated
# `MODS,KEY` entries, each field hex (the modifiers joined by NUL), from root.keys(): each key object's `modifiers`
# (X's names, objects/key.c:228-244 and common/xutil.c:56-76 at v4.3) and `key` (a keysym name, or `#N` for a keycode,
# objects/key.c:303-321). Translated as awesome.rs's AC says, in the oracle's own tables: Mod4 super, Mod1 alt,
# Control ctrl, Shift shift, any other modifier kept by its X name; `#10`…`#19` the number row (`1`…`0`), any other
# keycode kept as `#N`; an upper-case letter as shift; a name outside the notation `?name`; a key object with no key
# name (the chunk sends it empty) or `NoSymbol` `!nokey`, never known. root.keys() gives keysym names, never punctuation characters.
# ponytail: a key bound with Mod2 or Lock shows here without it (the chunk drops awful.key's variants), so it differs
# unexplained; e2e's rc.lua binds none.
AWESOME_MODS = {"Mod4": "super", "Mod1": "alt", "Control": "ctrl", "Shift": "shift"}
AWESOME_NUMBER_ROW = {f"#{10 + i}": str((i + 1) % 10) for i in range(10)}


def awesome_key(mods, key):
    """One root key's modifiers (X names) and key in keybind-find's notation."""
    mods = [AWESOME_MODS.get(m, m) for m in mods]
    if key in AWESOME_NUMBER_ROW:
        sym = AWESOME_NUMBER_ROW[key]
    elif key.startswith("#") and len(key) > 1:
        sym = key
    elif len(key) == 1 and key.isascii() and key.isalnum():
        sym = key.lower()
        if key.isupper():
            mods.append("shift")
    elif re.fullmatch(r"[A-Za-z0-9_]+", key):
        sym = key
    else:
        sym = "?" + key
    ordered = [m for m in MOD_ORDER if m in mods] + [m for m in dict.fromkeys(mods) if m not in MOD_ORDER]
    return "+".join(ordered + [sym])


def awesome_report(text):
    """The chunk's reply as keys; anything that isn't one hex `MODS,KEY` entry is kept as `!line:…`."""
    m = re.fullmatch(r'method return [^\n]*\n   string "([0-9a-f,;]*)"\n', text)
    if not m:
        return {"!line:" + text.strip()[:200]}
    keys = set()
    for entry in filter(None, m.group(1).split(";")):
        fields = entry.split(",")
        if len(fields) != 2:
            keys.add("!line:" + entry)
            continue
        try:
            mods, key = (bytes.fromhex(f).decode(errors="replace") for f in fields)
        except ValueError:
            keys.add("!line:" + entry)
            continue
        mods = [x for x in mods.split("\0") if x]
        keys.add(awesome_key(mods, key) if key not in ("", "NoSymbol") else "!nokey:" + "+".join(mods))
    return keys


# --- lf ---------------------------------------------------------------------------------------------------------------
# lf's key names (key.go:11-127 at r42, `gKeyVal`, and readKey's <lt> <gt> <space>) as X keysyms: its own table.
LF_NAMES = {
    "enter": "Return", "backspace": "BackSpace", "tab": "Tab", "backtab": "shift+Tab", "esc": "Escape",
    "delete": "Delete", "insert": "Insert", "up": "Up", "down": "Down", "left": "Left", "right": "Right",
    "home": "Home", "end": "End", "pgup": "Prior", "pgdn": "Next", "clear": "Clear", "pause": "Pause",
    "print": "Print", "space": "space", "lt": "less", "gt": "greater",
}
LF_NAMES.update({f"f-{n}": f"F{n}" for n in range(1, 36)})
LF_MODS = {"c": "ctrl", "s": "shift", "a": "alt"}
LF_MODES = {"n": "normal", "v": "visual", "c": "command-line"}


def lf_char(c, mods):
    """One character lf reads, as a keysym; an upper-case letter adds shift."""
    if c.isascii() and c.isupper():
        mods.append("shift")
        c = c.lower()
    return c if c.isascii() and c.isalnum() else KITTY_CHARS.get(c, "?" + c)


def lf_key(s):
    """One lf bind string (`<c-x>d`, `G`, `<a-X>`) in keybind-find's notation; a pointer event is `pointer:…`, a name
    lf has no keysym for `!name:…`."""
    chords = []
    for tok in re.findall(r"<[^>]*>|.", s):
        mods = []
        if len(tok) > 1:
            inner = tok[1:-1]
            m = re.fullmatch(r"([csa])-(.+)", inner)
            if m:
                mods.append(LF_MODS[m.group(1)])
                inner = m.group(2)
            if inner.startswith("m-"):
                return "pointer:" + s
            if m and len(inner) == 1:
                sym = lf_char(inner, mods)
            elif inner in LF_NAMES:
                *extra, sym = LF_NAMES[inner].split("+")
                mods += extra
            else:
                return "!name:" + s
        else:
            sym = lf_char(tok, mods)
        order = sorted(dict.fromkeys(mods), key=lambda x: MOD_ORDER.index(x))
        chords.append("+".join(order + [sym]))
    return " ".join(chords)


def lf_report(text):
    """`query <id> maps` and `cmaps` answers (tab-padded `mode key command` rows) as `mode=…<TAB>KEY`, every mode the
    row names; a line that isn't a row is kept as `!line:…`."""
    keys = set()
    for line in text.splitlines():
        f = [x for x in line.split("\t") if x]
        if not f or f == ["mode", "key", "command"]:
            continue
        if len(f) < 3 or not set(f[0]) <= set(LF_MODES):
            keys.add("!line:" + line)
            continue
        scopes = [f"mode={LF_MODES[m]}" for m in sorted(f[0], key=list(LF_MODES).index)]
        keys.add("\t".join(scopes + [lf_key(f[1])]))
    return keys


# --- nnn --------------------------------------------------------------------------------------------------------------
# nnn's help screen (show_help, nnn.c:6277-6400 at v5.3): its own token names. `2Esc` is Esc twice.
NNN_NAMES = {"Up": "Up", "Dn": "Down", "Lt": "Left", "Rt": "Right", "Ret": "Return", "PgUp": "Prior", "PgDn": "Next",
             "Esc": "Escape", "2Esc": "Escape", "Alt+Esc": "alt+Escape", "Alt": "alt", "Space": "space", "^/": "ctrl+slash",
             "^]": "ctrl+bracketright", "(,)": "comma", "m-m": "m"}
NNN_SECTIONS = ("NAVIGATION", "FILTER & PROMPT", "FILES", "MISC")


def nnn_keys(tok):
    """One help-screen key token as keybind-find's keys (a list: `1-8` and `(Sh)Tab` are several); `!token` if not
    one the table knows."""
    if tok in NNN_NAMES:
        return [NNN_NAMES[tok]]
    if tok == "1-8":
        return [str(n) for n in range(1, 9)]
    if tok == "(Sh)Tab":
        return ["Tab", "shift+Tab"]
    if len(tok) == 2 and tok[0] == "^" and tok[1].isascii() and tok[1].isalpha():
        return ["ctrl+" + tok[1].lower()]
    if len(tok) == 1:
        mods = []
        sym = lf_char(tok, mods)
        if not sym.startswith("?"):
            return ["+".join(mods + [sym])]
    return ["!" + tok]


def nnn_report(text):
    """The help screen's keys: each section's `KEYS  description` pairs (split on runs of two or more spaces, the keys
    on single spaces), then BOOKMARKS as `b KEY` and PLUGIN KEYS as `semicolon KEY` from ` KEY: value` lines; each
    section ends at a blank line. The art, CONTEXTS, VOLUME, the variables and the version are skipped."""
    keys, section = set(), None
    for line in text.splitlines():
        if not line.strip():
            section = None
        elif line.strip() in NNN_SECTIONS + ("BOOKMARKS", "PLUGIN KEYS"):
            section = line.strip()
        elif section in ("BOOKMARKS", "PLUGIN KEYS"):
            m = re.fullmatch(r" (.): .*", line)
            lead = "b" if section == "BOOKMARKS" else "semicolon"
            keys.add(f"{lead} {nnn_keys(m.group(1))[0]}" if m else "!line:" + line)
        elif section:
            parts = re.split(r" {2,}", line.strip())
            if len(parts) % 2:
                keys.add("!line:" + line)
                continue
            for ks in parts[::2]:
                for tok in ks.split(" "):
                    keys.update(nnn_keys(tok))
    return keys


REPORTS = {"kitty": kitty_report, "wezterm": wezterm_report, "zellij": zellij_report, "awesome": awesome_report,
           "lf": lf_report, "nnn": nnn_report}

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
    "zellij": [
        ("input_mode=*\tctrl+q", "config.kdl's top-level `unbind \"Ctrl q\"`: unbound in every mode, as zellij does"),
        ("input_mode=normal\talt+y", "config.kdl's `bind \"Alt y\" { NewTab; }` in normal: the preset can't show it",
         "dump"),
    ],
    # The stock rc.lua's clientkeys (awesomerc.lua:347-392 at v4.3) are set on each client by its rules and live in
    # each client's keys, never in root.keys(), which is all awesome's report reads.
    "awesome": [(k, "a stock client key (clientkeys): on each client, not in root.keys()", "dump") for k in (
        "super+f", "super+shift+c", "super+ctrl+space", "super+ctrl+Return", "super+o", "super+t", "super+n",
        "super+m", "super+ctrl+m", "super+shift+m")] + [
        ("*+Mod3+*", "awesome.rs leaves out a Mod3 modifier: keybind-find's notation has no name for it"),
        ("*+Mod5+*", "awesome.rs leaves out a Mod5 modifier: keybind-find's notation has no name for it"),
        ("Any+*", "awesome.rs leaves out the Any modifier: no one chord to send"),
        ("*#[0-9]*", "awesome.rs leaves out a keycode other than the number row's: it has no key name"),
        # awesome turns the character into its keysym (objects/key.c:110-160); awesome.rs reads the character, which
        # has no keysym in its table.
        ("super+eacute", "awesome.rs leaves out a non-ASCII character key (`é` in rc.lua)"),
    ],
    # lf's own answer comes from the running lf; the dump reads its lfrc over the pinned defaults (lf is gone by then).
    "lf": [
        ("*\tpointer:*", "lf's pointer events (<m-up>, <c-m-down>, …): not keys, lf.rs leaves them out"),
        ("mode=command-line\tspace", "lf's command line types a space itself (readCmdEvent): lf.rs leaves it out"),
        ("mode=normal\tmode=visual\t1", "a leading digit is a count (readNormalEvent): lf.rs leaves `map 1` out"),
        ("mode=normal\tmode=visual\tshift+q a", "Q runs before Qa can be typed (findBinds): lf.rs leaves Qa out"),
        ("mode=normal\tshift+y", "bound by on-init in the running lf; the dump's lfrc read runs no hooks"),
    ],
    # nextsel (nnn.c:3578-3690) reads Esc outside bindings[]: Esc then a key is `;` + that key (Alt+key), Esc Esc is
    # `q`, a lone Esc writes NNN_FIFO and a second quits; Alt arrives as Esc only if the terminal sends it so, and the
    # lone Esc depends on timing: nnn.rs lists none of them.
    "nnn": [
        ("Escape", "the help's Esc rows (quit, exit prompt, FIFO): nextsel's Esc handling, not bindings[]"),
        ("alt+Escape", "the help's `q Alt+Esc`: Esc Esc is `q` in nextsel, not a bindings[] key"),
        ("alt", "the help's `Alt ;`: Alt+key runs a plugin through nextsel's Esc handling"),
        ("Home", "KEY_HOME is in bindings[]; the help lists only `g ^A`", "dump"),
        ("End", "KEY_END is in bindings[]; the help lists only `G ^E`", "dump"),
    ],
}

# Keys the e2e config unbinds in every mode: none may stay in the dump. A KNOWN pattern alone can't tell: a key keybind-
# find kept in one mode is on both sides there, so no difference, while the other modes still match the pattern.
UNBOUND = {
    "zellij": [("input_mode=*\tctrl+q", "config.kdl's top-level `unbind \"Ctrl q\"`")],
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
    keybind-find leaves out, so KNOWN covers report-side keys; an entry marked "dump" (a third item) instead covers a
    key only keybind-find has, for a report that can't show the user's config (zellij's preset). Any other key only one
    side has fails."""
    # `!…` (a name or line the oracle can't read) is never known.
    def known_in(keys, side):
        pats = [k[0] for k in known if (k[2:] == ("dump",)) == (side == "dump")]
        hit = lambda k: not k.startswith("!") and any(fnmatch.fnmatchcase(k, p) for p in pats)
        stale = [p for p in pats if not any(hit(k) and fnmatch.fnmatchcase(k, p) for k in keys)]
        return sorted(k for k in keys if not hit(k)), stale
    only_r, stale_r = known_in(report - dump, "report")
    only_d, stale_d = known_in(dump - report, "dump")
    return only_r, only_d, stale_r + stale_d


def listed(keys):
    shown = ", ".join(k.replace("\t", " ") for k in keys[:10])
    return shown + (f" (+{len(keys) - 10} more)" if len(keys) > 10 else "")


def check(provider, report, dump):
    """(the problems, the counts) of one comparison: keys only one side has and no KNOWN entry covers, stale KNOWN
    entries, and a dump key that UNBOUND says must be gone."""
    only_r, only_d, stale = compare(report, dump, KNOWN.get(provider, []))
    problems = []
    if only_r:
        problems.append(f"only in {provider}: {listed(only_r)}")
    if only_d:
        problems.append(f"only in keybind-find: {listed(only_d)}")
    if stale:
        problems.append(f"stale known difference: {', '.join(stale)}")
    still = sorted(k for k in dump if any(fnmatch.fnmatchcase(k, p) for p, _ in UNBOUND.get(provider, [])))
    if still:
        problems.append(f"unbound in the config, still in keybind-find: {listed(still)}")
    return problems, counts(provider, report, dump, only_r, only_d)


def counts(provider, report, dump, only_r, only_d):
    """The row's counts: each side's keys, and how many differences KNOWN covers (on either side)."""
    known = len(report ^ dump) - len(only_r) - len(only_d)
    return f"{provider}:{len(report)} keybind-find:{len(dump)} known:{known}"


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
    problems, counts = check(provider, report, dump)
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
    assert wezterm_key(debug_char("\\u{1}"), "CTRL") == "ctrl+?\x01" and debug_char("\\0") == "\0"
    assert wezterm_key("F5", "CTRL") == "ctrl+F5" and wezterm_key("phys:F5", "NONE") == "F5"
    # Each known pattern matches its own example, in the form the report gives it.
    examples = {"*[?]*": wezterm_key(debug_char("\\u{1}"), "CTRL"), "*raw:*": wezterm_key("RawCode(123)", "CTRL"),
                "*XF86Audio*": wezterm_key("VolumeUp", "CTRL"), "key_table=*[ ]*\t*": "key_table=e2e wide\tEscape"}
    for p, _ in KNOWN["wezterm"]:
        assert fnmatch.fnmatchcase(examples[p], p), p
    assert wezterm_report("    { key = '\\\\u{1}', mods = 'CTRL', action = act.Nop },\n") == {"ctrl+?\x01"}
    assert wezterm_report("    { key = '\\\\', mods = 'CTRL', action = act.Nop },\n") == {"ctrl+backslash"}
    lua = ("return {\n  keys = {\n    { key = '\\'', mods = 'CTRL', action = act.Nop },\n  },\n  key_tables = {\n"
           "    e2e wide = {\n      { key = 'Escape', mods = 'NONE', action = act.PopKeyTable },\n    },\n  }\n}\nstray\n")
    assert wezterm_report(lua) == {"ctrl+apostrophe", "key_table=e2e wide\tEscape", "!line:stray"}
    for k in ("key_table=e2e\tctrl+a c", "ctrl+a shift+r", "XF86Copy", "key_table=copy_mode\tshift+dollar"):
        assert not [p for p, _ in KNOWN["wezterm"] if fnmatch.fnmatchcase(k, p)], k
    assert zellij_key("Ctrl Alt h") == "ctrl+alt+h" and zellij_key("alt ctrl  h") == "ctrl+alt+h"
    assert zellij_key("H") == "shift+h" and zellij_key("Alt Shift p") == "alt+shift+p"
    assert zellij_key("PageDown") == "Next" and zellij_key("esc") == "Escape" and zellij_key("Space") == "space"
    assert zellij_key('"') == "quotedbl" and zellij_key("%") == "percent" and zellij_key("F12") == "F12"
    assert zellij_key("Hyper x") == "!Hyper+x" and zellij_key("é") == "?é" and zellij_key("F13") == "!F13"
    preset = ('preset {\n  defaults {\n    primary "Ctrl"\n    secondary "Alt"\n  }\n}\nkeybinds {\n'
              '  // bind "x" { Nop; }\n  locked {\n    bind "{primary} g" { SwitchToMode "Normal"; }\n  }\n'
              '  shared_except "locked" "tmux" {\n    bind "{secondary} h" "{secondary} Left" { MoveFocus "Left"; }\n  }\n'
              '  tmux {\n    bind "\\"" { NewPane "Down"; }\n    bind "{" { Nop; }\n    bind "w" {\n      LaunchOrFocusPlugin "a" {\n'
              '        floating true\n      };\n      SwitchToMode "Normal"\n    }\n    stray\n  }\n'
              '  shared_among "pane" {\n    bind "Shift s" { Nop; }\n  }\n}\nmousebinds {\n  shared {\n'
              '    bind "Left" { Click; }\n  }\n}\n')
    r = zellij_report(preset)
    assert {k for k in r if k.startswith("input_mode=tmux")} == {
        "input_mode=tmux\tquotedbl", "input_mode=tmux\tbraceleft", "input_mode=tmux\tw"}, r
    assert "input_mode=locked\tctrl+g" in r and "input_mode=pane\tshift+s" in r and "!line:stray" in r
    assert {k for k in r if k.endswith("\talt+h")} == {f"input_mode={m}\talt+h" for m in ZELLIJ_MODES
                                                      if m not in ("locked", "tmux")}
    assert len(r) == 1 + 13 * 2 + 3 + 1 + 1, len(r)
    assert zellij_modes("shared {") == ZELLIJ_MODES and zellij_modes('shared_among "tab" "Pane" {') == ["pane", "tab"]
    assert zellij_modes("Normal {") == ["normal"]
    # Each zellij KNOWN pattern matches its own example and no ordinary key; the "dump" one covers only that side.
    zk = KNOWN["zellij"]
    assert fnmatch.fnmatchcase("input_mode=pane\tctrl+q", zk[0][0])
    assert fnmatch.fnmatchcase("input_mode=normal\talt+y", zk[1][0])
    for k in ("input_mode=pane\tctrl+alt+q", "input_mode=normal\tctrl+qx", "input_mode=pane\talt+y",
              "input_mode=normal\tshift+alt+y"):
        assert not [p for p, *_ in zk if fnmatch.fnmatchcase(k, p)], k
    both = {"input_mode=pane\tctrl+g"}
    assert compare(both | {"input_mode=pane\tctrl+q"}, both | {"input_mode=normal\talt+y"}, zk) == ([], [], [])
    # alt+y in the report instead: not known on that side, and the dump pattern goes stale.
    assert compare(both | {"input_mode=normal\talt+y"}, both, zk[1:]) == (
        ["input_mode=normal\talt+y"], [], ["input_mode=normal\talt+y"])
    # A dump-only key a report pattern would match still fails.
    assert compare(both, both | {"input_mode=pane\tctrl+q"}, zk[:1]) == ([], ["input_mode=pane\tctrl+q"],
                                                                         ["input_mode=*\tctrl+q"])
    # Mode headers the reader can't follow are differences, and their binds go to no mode.
    split = 'keybinds {\n  normal\n  {\n    bind "a" { X; }\n  }\n  pane { bind "b" { X; }; }\n  tab {\n    bind "c" { X; }\n  }\n}\n'
    assert zellij_report(split) == {"!line:normal", "!line:{", "!line:pane { bind \"b\" { X; }; }",
                                    "input_mode=tab\tc"}, zellij_report(split)
    # zellij's whole check: the partial unbind (ctrl+q kept in pane) fails, the full one passes with known:15.
    modes = [m for m in ZELLIJ_MODES if m != "locked"]
    report = {f"input_mode={m}\tctrl+q" for m in modes} | {"input_mode=locked\tctrl+g"}
    dump = {"input_mode=locked\tctrl+g", "input_mode=normal\talt+y"}
    assert check("zellij", report, dump) == ([], "zellij:15 keybind-find:2 known:15"), check("zellij", report, dump)
    problems, _ = check("zellij", report, dump | {"input_mode=pane\tctrl+q"})
    assert problems == ["unbound in the config, still in keybind-find: input_mode=pane ctrl+q"], problems
    assert counts("k", {"a", "b"}, {"a", "c"}, [], []) == "k:2 keybind-find:2 known:2"
    assert counts("k", {"a", "b"}, {"a", "c"}, ["b"], []) == "k:2 keybind-find:2 known:1"
    assert counts("k", {"a", "b"}, {"a", "c"}, [], ["c"]) == "k:2 keybind-find:2 known:1"
    sheet = {"binds": {"k": [{"key": "Escape", "when": {"keyboard_mode": ["mv"]}}, {"key": "ctrl+a"}]}}
    assert dump_keys(sheet) == {"keyboard_mode=mv\tEscape", "ctrl+a"}
    assert awesome_key(["Mod4", "Shift"], "Return") == "super+shift+Return"
    assert awesome_key(["Control", "Mod1"], "Delete") == "ctrl+alt+Delete"
    assert awesome_key(["Mod4"], "#10") == "super+1" and awesome_key(["Mod4"], "#19") == "super+0"
    assert awesome_key(["Mod4", "Control", "Shift"], "#18") == "super+ctrl+shift+9"
    assert awesome_key(["Mod4"], "#94") == "super+#94" and awesome_key(["Mod4"], "J") == "super+shift+j"
    assert awesome_key(["Mod4", "Mod3"], "x") == "super+Mod3+x" and awesome_key(["Any"], "F13") == "Any+F13"
    assert awesome_key(["Mod4"], "é") == "super+?é" and awesome_key([], "slash") == "slash"
    hexed = lambda mods, key: ",".join(s.encode().hex() for s in ("\0".join(mods), key))
    out = ("method return time=1.0 sender=:1.3 -> destination=:1.9 serial=4 reply_serial=2\n   string \""
           + ";".join([hexed(["Mod4"], "s"), hexed(["Mod4", "Control"], "#10"), hexed([], "XF86AudioMute")]) + "\"\n")
    assert awesome_report(out) == {"super+s", "super+ctrl+1", "XF86AudioMute"}, awesome_report(out)
    assert awesome_report("Error org.x: y\n") == {"!line:Error org.x: y"}
    head = "method return time=1.0 sender=:1.3 -> destination=:1.9 serial=4 reply_serial=2\n   string \""
    # Three fields is no entry; a key object with no key name is never known; stray bytes don't crash.
    assert awesome_report(head + "4d6f6434,73,73\"\n") == {"!line:4d6f6434,73,73"}
    assert awesome_report(head + hexed(["Mod4"], "") + "\"\n") == {"!nokey:Mod4"}
    assert awesome_report(head + "ff,73\"\n") == {"\ufffd+s"}
    assert awesome_report(head + hexed(["Mod4"], "NoSymbol") + "\"\n") == {"!nokey:Mod4"}
    assert awesome_report(head + "4d6,73\"\n") == {"!line:4d6,73"}
    ak = KNOWN["awesome"]
    examples = {"*+Mod3+*": "super+Mod3+x", "*+Mod5+*": "super+Mod5+x", "Any+*": "Any+F13", "*#[0-9]*": "super+#94",
                "super+eacute": "super+eacute"}
    for p, *_ in ak:
        assert fnmatch.fnmatchcase(examples.get(p, p), p), p
    for k in ("super+x", "super+shift+x", "super+1", "ctrl+alt+Delete", "super+numbersign", "super+mod5+x"):
        assert not [p for p, *_ in ak if fnmatch.fnmatchcase(k, p)], k
    for lf, want in (("<c-x>d", "ctrl+x d"), ("G", "shift+g"), ("<a-X>", "alt+shift+x"), ("<lt>", "less"),
                     ("<backtab>", "shift+Tab"), ("<f-12>", "F12"), ("gg", "g g"), ('"', "quotedbl"),
                     ("<a-backspace>", "alt+BackSpace"), ("<m-up>", "pointer:<m-up>"),
                     ("<c-m-down>", "pointer:<c-m-down>"), ("<f-40>", "!name:<f-40>"), ("<foo>", "!name:<foo>")):
        assert lf_key(lf) == want, (lf, lf_key(lf))
    answer = "mode\t\tkey\tcommand\nnv\t\tk\tup\nn\t\t<space>\t:{{ toggle; down; }}\nc\t<a-f>\tcmd-word\nzz\n"
    assert lf_report(answer) == {"mode=normal\tmode=visual\tk", "mode=normal\tspace", "mode=command-line\talt+f",
                                 "!line:zz"}, lf_report(answer)
    lk = KNOWN["lf"]
    examples = {"*\tpointer:*": "mode=normal\tmode=visual\tpointer:<m-up>"}
    for p, *_ in lk:
        assert fnmatch.fnmatchcase(examples.get(p, p), p), p
    for k in ("mode=normal\tmode=visual\tk", "mode=visual\tshift+y", "mode=command-line\talt+x"):
        assert not [p for p, *_ in lk if fnmatch.fnmatchcase(k, p)], k
    for tok, want in (("Up", ["Up"]), ("PgDn", ["Next"]), ("Ret", ["Return"]), ("^U", ["ctrl+u"]),
                      ("^y", ["ctrl+y"]), ("^/", ["ctrl+slash"]), ("^]", ["ctrl+bracketright"]),
                      ("1-8", [str(n) for n in range(1, 9)]), ("(Sh)Tab", ["Tab", "shift+Tab"]), ("(,)", ["comma"]),
                      ("m-m", ["m"]), ("Esc", ["Escape"]), ("2Esc", ["Escape"]), ("Alt+Esc", ["alt+Escape"]),
                      ("Alt", ["alt"]), ("Space", ["space"]), ("'", ["apostrophe"]), ("G", ["shift+g"]),
                      ("?", ["question"]), ("Fn", ["!Fn"]), ("é", ["!é"])):
        assert nnn_keys(tok) == want, (tok, nnn_keys(tok))
    help_text = ("  |V\\_\n (;^; ||\n\n NAVIGATION\n         Up k  Prev              PgUp ^U  Page up\n"
                 "    q Alt+Esc  Quit context            d  Detail mode toggle\n\n FILTER & PROMPT\n"
                 "            .  Toggle hidden\n\n MISC\n        Alt ;  Select plugin           =  Launch app\n"
                 "       (Sh)Tab  Cycle\n stray\n\nCONTEXTS\n 1* /tmp\n\nVOLUME: avail:1G used:1G size:2G\n\n"
                 "BOOKMARKS\n d: /tmp\n\nPLUGIN KEYS\n x: !echo e2e quit\n F: finder\n\nNNN_TRASH: 1\n\nv5.3\n")
    assert nnn_report(help_text) == {"Up", "k", "Prior", "ctrl+u", "q", "alt+Escape", "d", "period", "alt",
                                     "semicolon", "equal", "Tab", "shift+Tab", "!line: stray", "b d", "semicolon x",
                                     "semicolon shift+f"}, nnn_report(help_text)
    nk = KNOWN["nnn"]
    for p, *_ in nk:
        assert fnmatch.fnmatchcase(p, p), p
    for k in ("semicolon", "q", "ctrl+q", "Return", "semicolon x", "b d"):
        assert not [p for p, *_ in nk if fnmatch.fnmatchcase(k, p)], k
    print("oracle.py: self-test ok")
    return 0


if __name__ == "__main__":
    sys.exit(self_test() if sys.argv[1:] == ["--self-test"] else main(*sys.argv[1:4]))
