#!/usr/bin/env bash
# Runs inside the e2e image (tests/e2e/run starts it): builds keybind-find from /src, then for each case in `cases`
# starts the program for real (X11: Xvfb + i3; Wayland: headless sway), waits until it has focus, runs
# `keybind-find dump` and checks it with check.py. Prints a table; exits 1 if any case failed.
set -u
E=/src/tests/e2e
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
  export CARGO_HOME=/home/tester/.cargo CARGO_TARGET_DIR=/target
  echo "e2e: building keybind-find" >&2
  (cd /src && cargo build --locked -q) || exit 2
  # A fresh home with one remap per program (tests/e2e/home); short runtime paths (a vim socket must fit 108
  # bytes). Set before the session bus starts: the services it starts on demand (dconf, GNOME Terminal's server)
  # take its environment.
  export HOME=/tmp/home LANG=C.UTF-8 TERM=xterm-256color
  rm -rf "$HOME" && cp -r "$E/home" "$HOME"
  export XDG_RUNTIME_DIR=/tmp/run SCREENDIR=/tmp/screens
  mkdir -m 700 -p "$XDG_RUNTIME_DIR" "$SCREENDIR"
  unset WAYLAND_DISPLAY SWAYSOCK XDG_CURRENT_DESKTOP DESKTOP_SESSION
  # A session bus: GNOME Terminal, xfconf and gsettings need one.
  exec dbus-run-session -- bash "$0" "$@"
fi
KBF=/target/debug/keybind-find
OUT=/tmp/out
mkdir -p "$OUT"
printf 'int main(void) { return 0; }\n' > /tmp/e2e.c
printf 'fn main() {}\n' > /tmp/e2e.rs
RESULTS=()
FAILED=0

# Whether CASES (space-separated names; empty: all) selects this case.
wanted() { [ -z "${CASES:-}" ] || [[ " $CASES " == *" $1 "* ]]; }

record() { RESULTS+=("$1	$2"); }

# dump_and_check NAME PROVIDERS KEYCHECK [PROVIDER]: `keybind-find dump [PROVIDER]`, then check.py.
dump_and_check() {
  local name=$1 providers=$2 key=$3
  timeout 60 "$KBF" dump ${4:+"$4"} >"$OUT/$name.json" 2>"$OUT/$name.err"
  local line
  # `dump PROVIDER` prints one sheet, not an array.
  [ -n "${4:-}" ] && python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])); json.dump([s],open(sys.argv[1],"w"))' "$OUT/$name.json" 2>/dev/null
  if line=$(python3 -I "$E/check.py" "$OUT/$name.json" "$providers" "$key"); then
    record "$name" "ok	$line"
  else
    record "$name" "FAIL	$line	$(tr '\n\t' '  ' < "$OUT/$name.err" | cut -c1-300)"
    FAILED=1
  fi
}

# x NAME "PROVIDERS" "PROVIDER|KEY|DESC" [SETTLE] -- COMMAND...: an X11 case under i3.
x() {
  local name=$1 providers=$2 key=$3 settle=2
  shift 3
  [ "$1" != -- ] && { settle=$1; shift; }
  shift
  wanted "$name" || return 0
  command -v "$1" >/dev/null || { record "$name" "skip	$1 is not installed"; return; }
  local before; before=$(xdotool getactivewindow 2>/dev/null)
  setsid "$@" >"$OUT/$name.app.log" 2>&1 &
  local pid=$! w=""
  for _ in $(seq 150); do
    w=$(xdotool getactivewindow 2>/dev/null)
    [ -n "$w" ] && [ "$w" != "$before" ] && break
    w=""; sleep 0.1
  done
  if [ -z "$w" ]; then
    record "$name" "FAIL	no window	$(tail -c 300 "$OUT/$name.app.log" | tr '\n\t' '  ')"
    FAILED=1
  else
    sleep "$settle"
    dump_and_check "$name" "$providers" "$key"
    i3-msg -q kill >/dev/null 2>&1
  fi
  sleep 0.3; kill -- -"$pid" 2>/dev/null; sleep 0.3
}

# focused_con: sway's focused container id.
focused_con() {
  swaymsg -t get_tree 2>/dev/null | python3 -I -c 'import json,sys
def walk(n):
    if n.get("focused"): print(n["id"]); return True
    return any(walk(c) for c in n.get("nodes", []) + n.get("floating_nodes", []))
walk(json.load(sys.stdin))'
}

# w NAME "PROVIDERS" "PROVIDER|KEY|DESC" -- COMMAND...: a Wayland case under headless sway.
w() {
  local name=$1 providers=$2 key=$3
  shift 4
  wanted "$name" || return 0
  local before; before=$(focused_con)
  swaymsg -q exec -- "$*"
  local now=""
  for _ in $(seq 150); do
    now=$(focused_con)
    [ -n "$now" ] && [ "$now" != "$before" ] && break
    now=""; sleep 0.1
  done
  if [ -z "$now" ]; then
    record "$name" "FAIL	no window"
    FAILED=1
  else
    sleep 2
    dump_and_check "$name" "$providers" "$key"
    swaymsg -q kill
  fi
  sleep 0.5
}

# d NAME PROVIDER "PROVIDER|KEY|DESC" -- SETUP...: one provider dumped directly (a desktop that can't run here),
# after SETUP.
d() {
  local name=$1 provider=$2 key=$3
  shift 4
  wanted "$name" || return 0
  "$@" >/dev/null 2>&1
  dump_and_check "$name" "$provider" "$key" "$provider"
}

# --- X11: Xvfb and i3 -------------------------------------------------------------------------------------------
export DISPLAY=:77
Xvfb :77 -screen 0 1600x1000x24 -nolisten tcp >"$OUT/xvfb.log" 2>&1 &
XVFB=$!
for _ in $(seq 100); do xprop -root >/dev/null 2>&1 && break; sleep 0.1; done
dbus-update-activation-environment DISPLAY
i3 -c "$HOME/.config/i3/config" >"$OUT/i3.log" 2>&1 &
I3=$!
for _ in $(seq 100); do i3-msg -q nop 2>/dev/null && break; sleep 0.1; done
PHASE=x11 . "$E/cases"
emacsclient -e '(kill-emacs)' >/dev/null 2>&1
kill "$I3" "$XVFB" 2>/dev/null
unset DISPLAY

# --- Wayland: headless sway ---------------------------------------------------------------------------------------
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway -c "$HOME/.config/sway/config" \
  >"$OUT/sway.log" 2>&1 &
SWAY=$!
for _ in $(seq 100); do
  SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null | head -1)
  [ -n "$SWAYSOCK" ] && break; sleep 0.1
done
export SWAYSOCK WAYLAND_DISPLAY
WAYLAND_DISPLAY=$(ls "$XDG_RUNTIME_DIR" | grep -E '^wayland-[0-9]+$' | head -1)
dbus-update-activation-environment WAYLAND_DISPLAY SWAYSOCK
PHASE=wayland . "$E/cases"
swaymsg -q exit 2>/dev/null; kill "$SWAY" 2>/dev/null

# --- The table ----------------------------------------------------------------------------------------------------
printf '%s\n' "${RESULTS[@]}" | column -t -s $'\t' | cut -c1-"${COLUMNS:-200}"
echo "e2e: $(printf '%s\n' "${RESULTS[@]}" | grep -c $'\tok\t') ok, $(printf '%s\n' "${RESULTS[@]}" | grep -c $'\tFAIL') failed, $(printf '%s\n' "${RESULTS[@]}" | grep -c $'\tskip\t') skipped (E2E_OUT=dir keeps each case's dump and stderr)"
exit "$FAILED"
