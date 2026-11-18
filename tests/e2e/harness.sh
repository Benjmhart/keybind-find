#!/usr/bin/env bash
# Runs inside the e2e image (tests/e2e/run starts it): builds keybind-find from /src, then for each case in `cases`
# starts the program for real (X11: Xvfb + i3, then awesome; Wayland: headless sway), waits until it has focus, runs
# `keybind-find dump` and checks it with check.py. Prints a table; exits 1 if any case failed or was skipped.
# `harness.sh build` only builds (run gives that step the network; the cases run without one).
set -u
E=/src/tests/e2e
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
  export CARGO_HOME=/home/tester/.cargo CARGO_TARGET_DIR=/target
  if [ "${1:-}" = build ]; then
    echo "e2e: building keybind-find" >&2
    cd /src && exec cargo build --locked -q
  fi
  (cd /src && cargo build --locked --offline -q) || exit 2
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
# A recording's picker: fzf in alacritty in every phase (under Wayland the default is fuzzel, not in the image). The
# home is this run's copy, so normal runs never see it.
if [ -n "${RECORD:-}" ]; then
  mkdir -p "$HOME/.config/keybind-find"
  printf '[picker]\nkind = "fzf"\n' >"$HOME/.config/keybind-find/config.toml"
fi
OUT=/tmp/out
mkdir -p "$OUT"
printf 'int main(void) { return 0; }\n' > /tmp/e2e.c
printf 'fn main() {}\n' > /tmp/e2e.rs
RESULTS=()
FAILED=0
SEEN=" "
# oracle.py's own check, before any `o` case trusts it.
python3 -I "$E/oracle.py" --self-test >/dev/null 2>"$OUT/oracle-self-test.err" || {
  RESULTS+=("oracle-self-test	FAIL	$(tail -c 300 "$OUT/oracle-self-test.err" | tr '\n\t' '  ')"); FAILED=1
}

# Whether CASES (space-separated names; empty: all) selects this case. Every case name passes through here, so a
# name in CASES that never does is a typo (reported at the end).
wanted() {
  SEEN+="$1 "
  [ -z "${CASES:-}" ] || [[ " $CASES " == *" $1 "* ]]
}

record() { RESULTS+=("$1	$2"); }

# dump_and_check NAME PROVIDERS KEYCHECK [PROVIDER]: `keybind-find dump [PROVIDER]`, then check.py; again until it
# passes or 15 s have gone (a program inside a terminal can still be starting when its window has focus).
dump_and_check() {
  local name=$1 providers=$2 key=$3 line ok=1 deadline=$((SECONDS + 15))
  while :; do
    timeout 60 "$KBF" dump ${4:+"$4"} >"$OUT/$name.json" 2>"$OUT/$name.err"
    # `dump PROVIDER` prints one sheet, not an array.
    [ -n "${4:-}" ] && python3 -I -c 'import json,sys; s=json.load(open(sys.argv[1])); json.dump([s],open(sys.argv[1],"w"))' "$OUT/$name.json" 2>/dev/null
    line=$(python3 -I "$E/check.py" "$OUT/$name.json" "$providers" "$key" 2>&1) && { ok=0; break; }
    [ "$SECONDS" -ge "$deadline" ] && break
    sleep 1
  done
  if [ $ok -eq 0 ]; then
    record "$name" "ok	$line"
  else
    record "$name" "FAIL	$line	$(tr '\n\t' '  ' < "$OUT/$name.err" | cut -c1-300)"
    FAILED=1
  fi
  return $ok
}

# demo NAME "PROVIDER|KEY|DESC": with RECORD set (tests/e2e/record), records NAME.gif of the program the case started:
# keybind-find with no arguments (what a Super+/ binding runs), its picker, QUERY (default: DESC) typed, Return
# picking the top entry so keybind-find sends its key, then 2 s of the program. X11 displays through x11grab, sway
# through grim frames; one palette pass to a GIF of about 8 fps, 960 px wide, at most 10 MB, from at most 20 s. It
# passes only when keybind-find logged one send (its usage log): of the case's own PROVIDER and KEY with the default
# query, of any key with a QUERY given.
demo() {
  [ -n "${RECORD:-}" ] || return 0
  local name=$1 want query=${QUERY:-} raw="$OUT/$name.rec" gif="$OUT/$name.gif" cap kbf before now="" size t0 t1
  local usage="$HOME/.local/state/keybind-find/usage.tsv" sent0 sent
  # The send to expect, as the usage log has it (PROVIDER<TAB>KEY); none for a QUERY of the user's.
  want=""
  [ -n "$query" ] || { want=${2%%|*}$'\t'${2#*|}; want=${want%%|*}; query=${2#*|*|}; query=${query%%|*}; }
  rm -rf "$raw" "$raw.mkv" "$gif"
  # wtype has no `--`: a leading `-` would be read as an option.
  [[ "$query" != -* ]] || { record "$name.gif" "FAIL	a query can't start with -"; FAILED=1; return; }
  sent0=$(cat "$usage" 2>/dev/null | wc -l)
  # Capture: an X display (the i3 and awesome phases, or niri nested on its Xvfb), else sway's output. At most 20 s;
  # the stop below ends it sooner.
  local xdisp=${DISPLAY:-}
  [ -n "${NIRI_SOCKET:-}" ] && xdisp=:78
  t0=$(date +%s.%N)
  if [ -n "$xdisp" ]; then
    ffmpeg -loglevel error -f x11grab -framerate 8 -i "$xdisp" -t 20 -c:v libx264rgb -crf 0 -preset ultrafast \
      "$raw.mkv" </dev/null >"$OUT/$name.rec.log" 2>&1 &
  else
    mkdir -p "$raw"
    # A frame gets its number only once grim has written it whole, so the sequence has no gaps.
    (i=0; end=$((SECONDS + 20)); while [ "$SECONDS" -lt "$end" ]; do
       f=$raw/$(printf %05d "$i")
       grim "$f.tmp" 2>>"$OUT/$name.rec.log" && mv "$f.tmp" "$f.png" && i=$((i + 1))
       sleep 0.15
     done) &
  fi
  cap=$!
  sleep 1
  before=$(focused)
  setsid "$KBF" >"$OUT/$name.kbf.log" 2>&1 &
  kbf=$!
  for _ in $(seq 100); do
    now=$(focused)
    [ -n "$now" ] && [ "$now" != "$before" ] && break
    now=""; sleep 0.1
  done
  if [ -z "$now" ]; then
    record "$name.gif" "FAIL	the picker didn't appear	$(tail -c 300 "$OUT/$name.kbf.log" | tr '\n\t' '  ')"
    FAILED=1
    kill -- -"$kbf" "$cap" 2>/dev/null; wait "$kbf" "$cap" 2>/dev/null
    rm -rf "$raw" "$raw.mkv"
    return
  fi
  sleep 1.5
  if [ -n "${DISPLAY:-}" ]; then
    # fzf opens in key search on X11 (it reads pressed keys): Escape switches to text search.
    xdotool key Escape; sleep 0.5
    xdotool type --delay 120 -- "$query"; sleep 1.5
    xdotool key Return
  else
    # One wtype: each run uploads a keymap and sway drops a key sent right after it; -s waits that out.
    wtype -s 500 -d 120 "$query" -s 1500 -k Return
  fi
  local status=0
  for _ in $(seq 100); do kill -0 "$kbf" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$kbf" 2>/dev/null; then
    status=still; kill -- -"$kbf" 2>/dev/null; wait "$kbf" 2>/dev/null
  else
    wait "$kbf"; status=$?
  fi
  sleep 2
  t1=$(date +%s.%N)
  local capped
  capped=$(awk -v a="$t0" -v b="$t1" 'BEGIN { if (b - a >= 20) print 1 }')
  # SIGTERM: ffmpeg finishes its file on it (a background job here ignores SIGINT); it ends the grim loop, after its
  # grim (a child) is gone.
  pkill -P "$cap" 2>/dev/null; kill "$cap" 2>/dev/null; wait "$cap" 2>/dev/null
  local in=(-i "$raw.mkv") n
  if [ -d "$raw" ]; then
    # The frames' real rate: grim's own time adds to the 0.15 s between them.
    n=$(ls "$raw"/*.png 2>/dev/null | wc -l)
    in=(-framerate "$(awk -v n="$n" -v a="$t0" -v b="$t1" 'BEGIN { t = b - a; printf "%.2f", (t > 0 ? n / t : 5) }')" \
      -i "$raw/%05d.png")
  fi
  if ! ffmpeg -loglevel error "${in[@]}" \
       -vf "fps=8,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse" -y "$gif" \
       </dev/null >>"$OUT/$name.rec.log" 2>&1; then
    record "$name.gif" "FAIL	no GIF	$(tail -c 300 "$OUT/$name.rec.log" | tr '\n\t' '  ')"
    FAILED=1; rm -rf "$raw" "$raw.mkv"; return
  fi
  rm -rf "$raw" "$raw.mkv"
  size=$(stat -c %s "$gif")
  sent=$(tail -n +"$((sent0 + 1))" "$usage" 2>/dev/null | cut -f2,3)
  # The GIF stays on a FAIL: it may show why.
  if [ "$size" -gt 10485760 ]; then
    record "$name.gif" "FAIL	$size bytes: over GitHub's 10 MB for a GIF"
    FAILED=1
  elif [ -n "$capped" ]; then
    record "$name.gif" "FAIL	the capture hit its 20 s limit before the end"
    FAILED=1
  elif [ "${want%%$'\t'*}" = niri ] && [ -z "$sent" ] && [ "$status" != still ] &&
    grep -qF "press ${want#*$'\t'} yourself" "$OUT/$name.kbf.log"; then
    # A niri case's own key (the default query) is refused by design: wtype's keys reach the window, not niri. The
    # refusal is the demo's end; a QUERY for the program's key shows a send.
    record "$name.gif" "ok	$size bytes	query: $query, niri's own key refused (as designed)"
  elif [ "$status" != 0 ]; then
    [ "$status" = still ] && status="was still running after 10 s" || status="exited $status"
    record "$name.gif" "FAIL	keybind-find $status	$(tail -c 300 "$OUT/$name.kbf.log" | tr '\n\t' '  ')"
    FAILED=1
  elif [ -z "$sent" ] || [[ "$sent" == *$'\n'* ]] || { [ -n "$want" ] && [ "$sent" != "$want" ]; }; then
    sent=$(printf '%s' "$sent" | tr '\n\t' '; ')
    record "$name.gif" "FAIL	sent '$sent'${want:+, not ${want/$'\t'/ }} (query: $query)	$(tail -c 200 \
      "$OUT/$name.kbf.log" | tr '\n\t' '  ')"
    FAILED=1
  else
    record "$name.gif" "ok	$size bytes	query: $query, sent $(printf '%s' "$sent" | tr '\t' ' ')"
  fi
}

# focused: the focused window's id in this phase (X11: xdotool; niri: its IPC; sway: focused_con).
focused() {
  if [ -n "${DISPLAY:-}" ]; then
    xdotool getactivewindow 2>/dev/null
  elif [ -n "${NIRI_SOCKET:-}" ]; then
    niri msg -j focused-window 2>/dev/null |
      python3 -I -c 'import json,sys; w=json.load(sys.stdin); print(w["id"] if w else "")' 2>/dev/null
  else
    focused_con
  fi
}

# x NAME "PROVIDERS" "PROVIDER|KEY|DESC" [SETTLE] -- COMMAND...: an X11 case under i3 (or awesome in its phase, where
# `i3-msg kill` fails harmlessly and the process-group kill closes the window).
x() {
  local name=$1 providers=$2 key=$3 settle=1
  shift 3
  [ "$1" != -- ] && { settle=$1; shift; }
  shift
  wanted "$name" || return 0
  # Everything comes from the image: a missing program means the image is wrong, so a skip fails the run.
  command -v "$1" >/dev/null || { record "$name" "skip	$1 is not installed"; FAILED=1; return; }
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
    dump_and_check "$name" "$providers" "$key" && demo "$name" "$key"
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
  command -v "$1" >/dev/null || { record "$name" "skip	$1 is not installed"; FAILED=1; return; }
  [ -n "${SWAYSOCK:-}" ] || { record "$name" "FAIL	sway didn't start	$(tail -c 300 "$OUT/sway.log" | tr '\n\t' '  ')"; FAILED=1; return; }
  local before; before=$(focused_con)
  # Through sway, so the program is sway's child; the command is a constant from `cases` (no quoting needed).
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
    # A late window would take the next case's focus.
    pkill -x "$1" 2>/dev/null
  else
    sleep 1
    dump_and_check "$name" "$providers" "$key" && demo "$name" "$key"
    swaymsg -q kill
  fi
  sleep 0.5
}

# p NAME RESPONSE: a send through the RemoteDesktop portal under a faked GNOME session on sway's display (no SWAYSOCK,
# XDG_CURRENT_DESKTOP=GNOME and a process named gnome-shell: a copy of sleep), the portal being tests/e2e/fake_portal.py
# on this session bus with Start answering RESPONSE. The pick is GNOME's `minimize`, set to ctrl+shift+k (under GNOME on
# Wayland no window is read, so a sheet can't apply), picked by a dmenu-style grep: two picks when RESPONSE is 0, one
# when it is a refusal. fake_portal.py --check checks the calls, the token file and each pick's last stderr line.
p() {
  local name=$1 response=$2 state=/tmp/$1-state picks=2 i line errs=()
  wanted "$name" || return 0
  [ "$response" = 0 ] || picks=1
  rm -rf "$state" "$OUT/$name.calls"
  mkdir -p /tmp/portal-bin && cp "$(command -v sleep)" /tmp/portal-bin/gnome-shell
  PATH=/tmp/portal-bin:$PATH gnome-shell 300 &
  local shell=$!
  gsettings set org.gnome.desktop.wm.keybindings minimize "['<Control><Shift>k']"
  FAKE_PORTAL_START=$response python3 -I "$E/fake_portal.py" "$OUT/$name.calls" >"$OUT/$name.fake.log" 2>&1 &
  local fake=$!
  if ! gdbus wait --session --timeout 10 org.freedesktop.portal.Desktop >/dev/null 2>&1; then
    record "$name" "FAIL	the fake portal didn't start	$(tail -c 300 "$OUT/$name.fake.log" | tr '\n\t' '  ')"
    FAILED=1
  else
    for i in $(seq "$picks"); do
      env -u SWAYSOCK XDG_CURRENT_DESKTOP=GNOME XDG_STATE_HOME="$state" KEYBIND_FIND_PICKER='grep -m1 minimize' \
        timeout 30 "$KBF" >/dev/null 2>"$OUT/$name.$i.err"
      errs+=("$OUT/$name.$i.err")
    done
    if line=$(FAKE_PORTAL_START=$response python3 -I "$E/fake_portal.py" --check "$OUT/$name.calls" \
         "$state/keybind-find/portal-token" "${errs[@]}" 2>&1); then
      record "$name" "$line"
    else
      record "$name" "FAIL	${line#FAIL	}"
      FAILED=1
    fi
  fi
  gsettings reset org.gnome.desktop.wm.keybindings minimize
  kill "$fake" "$shell" 2>/dev/null; wait "$fake" "$shell" 2>/dev/null
}

# n NAME "PROVIDERS" "PROVIDER|KEY|DESC" -- COMMAND...: a case under niri (its phase), the program started through
# niri (`niri msg action spawn`), its focus read through niri's IPC as keybind-find reads it.
# niri double-forks what it spawns (src/utils/spawning.rs:178, and :275 on its systemd path, at v26.04), so the
# program isn't niri's child: a window
# that never appears is cleaned up by killing only the PROGRAM processes that weren't there before.
n() {
  local name=$1 providers=$2 key=$3
  shift 4
  wanted "$name" || return 0
  command -v "$1" >/dev/null || { record "$name" "skip	$1 is not installed"; FAILED=1; return; }
  local before old p; before=$(focused); old=" $(pgrep -x "$1" | tr '\n' ' ')"
  niri msg action spawn -- "$@" >/dev/null 2>&1
  local now=""
  for _ in $(seq 150); do
    now=$(focused)
    [ -n "$now" ] && [ "$now" != "$before" ] && break
    now=""; sleep 0.1
  done
  if [ -z "$now" ]; then
    record "$name" "FAIL	no window	$(tail -c 300 "$OUT/niri.log" | tr '\n\t' '  ')"
    FAILED=1
    for p in $(pgrep -x "$1"); do [[ "$old" == *" $p "* ]] || kill "$p" 2>/dev/null; done
  else
    sleep 1
    dump_and_check "$name" "$providers" "$key" && demo "$name" "$key"
    niri msg action close-window --id "$now" >/dev/null 2>&1
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

# o NAME PROVIDER -- COMMAND...: the program as oracle. COMMAND prints the program's own effective keys; oracle.py
# compares them with `keybind-find dump PROVIDER` on the same config.
o() {
  local name=$1 provider=$2 line
  shift 3
  wanted "$name" || return 0
  command -v "$1" >/dev/null || { record "$name" "skip	$1 is not installed"; FAILED=1; return; }
  timeout 60 "$@" >"$OUT/$name.report" 2>"$OUT/$name.report.err" || {
    record "$name" "FAIL	report failed	$(tail -c 300 "$OUT/$name.report.err" | tr '\n\t' '  ')"
    FAILED=1; return
  }
  timeout 60 "$KBF" dump "$provider" >"$OUT/$name.json" 2>"$OUT/$name.err" || {
    record "$name" "FAIL	dump failed	$(tr '\n\t' '  ' < "$OUT/$name.err" | cut -c1-300)"
    FAILED=1; return
  }
  if line=$(python3 -I "$E/oracle.py" "$provider" "$OUT/$name.report" "$OUT/$name.json" 2>"$OUT/$name.oracle.err"); then
    record "$name" "$line"
  else
    # A crash in oracle.py (no FAIL of its own) is a FAIL too; the program's stderr may say why a key differs.
    line=$(printf '%s ' "${line#FAIL	}" "$(tail -c 300 "$OUT/$name.oracle.err")" | tr '\n' ' ')
    record "$name" "FAIL	$line	$(tail -c 300 "$OUT/$name.report.err" | tr '\n\t' '  ')"
    FAILED=1
  fi
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
# The daemons the cases left (outside their process groups), so none carries this display into the next phase.
stop_daemons() {
  emacsclient -e '(kill-emacs)' >/dev/null 2>&1
  tmux kill-server 2>/dev/null
  pkill -x screen 2>/dev/null; pkill -x gnome-terminal- 2>/dev/null
}
PHASE=x11 . "$E/cases"
stop_daemons
kill "$I3" 2>/dev/null; wait "$I3" 2>/dev/null

# --- X11: awesome on the same display ---------------------------------------------------------------------------
# i3 gone first (one window manager per display). awesome answers on this session bus once its rc.lua has loaded awful,
# which owns org.awesomewm.awful (lib/awful/dbus.lua:16 at v4.3): the cases wait for that name. Only when the phase
# or an awesome case (every case name starting `awesome`) is wanted, as niri's phase is only when named.
if wanted awesome-phase || [[ " ${CASES:-} " == *" awesome"* ]]; then
  awesome -c "$HOME/.config/awesome/rc.lua" >"$OUT/awesome.log" 2>&1 &
  AWESOME=$!
  owned=""
  for _ in $(seq 100); do
    dbus-send --session --dest=org.freedesktop.DBus --type=method_call --print-reply / \
      org.freedesktop.DBus.NameHasOwner string:org.awesomewm.awful 2>/dev/null | grep -q 'boolean true' &&
      { owned=1; break; }
    kill -0 "$AWESOME" 2>/dev/null || break
    sleep 0.1
  done
  if [ -n "$owned" ]; then
    PHASE=awesome . "$E/cases"
  else
    record awesome-phase "FAIL	awesome didn't start	$(tail -c 300 "$OUT/awesome.log" | tr '\n\t' '  ')"
    FAILED=1
  fi
  # Stopped before anything else runs: up to 5 s for it to exit, then killed.
  kill "$AWESOME" 2>/dev/null
  timeout 5 tail --pid="$AWESOME" -f /dev/null || kill -9 "$AWESOME" 2>/dev/null
  wait "$AWESOME" 2>/dev/null
  stop_daemons
fi
kill "$XVFB" 2>/dev/null
unset DISPLAY

# --- Wayland: headless sway ---------------------------------------------------------------------------------------
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway -c "$HOME/.config/sway/config" \
  >"$OUT/sway.log" 2>&1 &
SWAY=$!
SWAYSOCK="" WAYLAND_DISPLAY=""
for _ in $(seq 100); do
  SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null | head -1)
  WAYLAND_DISPLAY=$(ls "$XDG_RUNTIME_DIR" 2>/dev/null | grep -E '^wayland-[0-9]+$' | head -1)
  [ -n "$SWAYSOCK" ] && [ -n "$WAYLAND_DISPLAY" ] && break
  SWAYSOCK=""; sleep 0.1
done
export SWAYSOCK WAYLAND_DISPLAY
dbus-update-activation-environment WAYLAND_DISPLAY SWAYSOCK
PHASE=wayland . "$E/cases"
swaymsg -q exit 2>/dev/null; kill "$SWAY" 2>/dev/null
unset SWAYSOCK WAYLAND_DISPLAY

# --- niri: nested (winit) on its own Xvfb ------------------------------------------------------------------------
# niri's binary has no headless mode (its headless backend is for its tests only); with DISPLAY set and no
# WAYLAND_DISPLAY it runs as an X11 window, rendering through mesa's llvmpipe. The cases get niri's Wayland display,
# not the Xvfb one. A named phase (niri-phase), so a run naming only other cases skips it.
# niri falls back to its built-in config when the file doesn't load: check the pinned one first. The cases and
# keybind-find must read that file too, whatever the image's environment says.
unset XDG_CONFIG_HOME NIRI_CONFIG
if wanted niri-phase && ! command -v niri >/dev/null; then
  record niri-phase "FAIL	niri is not installed"
  FAILED=1
elif wanted niri-phase && ! niri validate -c "$HOME/.config/niri/config.kdl" >"$OUT/niri-validate.log" 2>&1; then
  record niri-phase "FAIL	niri rejects tests/e2e/home/.config/niri/config.kdl	$(tail -c 300 "$OUT/niri-validate.log" | tr '\n\t' '  ')"
  FAILED=1
elif wanted niri-phase; then
  Xvfb :78 -screen 0 1600x1000x24 -nolisten tcp >"$OUT/xvfb-niri.log" 2>&1 &
  XVFB=$!
  for _ in $(seq 100); do DISPLAY=:78 xprop -root >/dev/null 2>&1 && break; sleep 0.1; done
  DISPLAY=:78 NO_COLOR=1 niri >"$OUT/niri.log" 2>&1 &
  NIRI=$!
  NIRI_SOCKET=""
  # Up to 30 s: a cold start renders its first frame through llvmpipe.
  for _ in $(seq 300); do
    NIRI_SOCKET=$(ls "$XDG_RUNTIME_DIR"/niri.*."$NIRI".sock 2>/dev/null | head -1)
    [ -n "$NIRI_SOCKET" ] && NIRI_SOCKET=$NIRI_SOCKET niri msg version >/dev/null 2>&1 && break
    kill -0 "$NIRI" 2>/dev/null || break
    NIRI_SOCKET=""; sleep 0.1
  done
  if [ -n "$NIRI_SOCKET" ]; then
    # niri.<wayland socket>.<pid>.sock
    WAYLAND_DISPLAY=${NIRI_SOCKET##*/niri.}; WAYLAND_DISPLAY=${WAYLAND_DISPLAY%."$NIRI".sock}
    export NIRI_SOCKET WAYLAND_DISPLAY
    dbus-update-activation-environment WAYLAND_DISPLAY NIRI_SOCKET
    record niri-phase "ok	$(niri msg version 2>&1 | head -1) on $WAYLAND_DISPLAY"
    PHASE=niri . "$E/cases"
    niri msg action quit --skip-confirmation >/dev/null 2>&1
  else
    record niri-phase "FAIL	niri didn't start	$(tail -c 300 "$OUT/niri.log" | tr '\n\t' '  ') $(tail -c 200 "$OUT/xvfb-niri.log" | tr '\n\t' '  ')"
    FAILED=1
  fi
  kill "$NIRI" "$XVFB" 2>/dev/null
  unset NIRI_SOCKET WAYLAND_DISPLAY
fi

# --- Names asked for that no case has ---------------------------------------------------------------------------
for n in ${CASES:-}; do
  [[ "$SEEN" == *" $n "* ]] || { record "$n" "FAIL	no such case (see tests/e2e/cases)"; FAILED=1; }
done

# --- The table ----------------------------------------------------------------------------------------------------
printf '%s\n' "${RESULTS[@]}" | column -t -s $'\t' | cut -c1-"${COLUMNS:-200}"
echo "e2e: $(printf '%s\n' "${RESULTS[@]}" | grep -c $'\tok\t') ok, $(printf '%s\n' "${RESULTS[@]}" | grep -c $'\tFAIL') failed, $(printf '%s\n' "${RESULTS[@]}" | grep -c $'\tskip\t') skipped (E2E_OUT=dir keeps each case's dump and stderr)"
exit "$FAILED"
