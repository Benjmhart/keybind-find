#!/usr/bin/env bash
# Runs inside the e2e image (tests/e2e/run starts it): builds keybind-find from /src, then for each case in `cases`
# starts the program for real (X11: Xvfb + i3; Wayland: headless sway), waits until it has focus, runs
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
# through grim frames; one palette pass to a GIF of about 8 fps, 960 px wide, at most 10 MB. It passes only when
# keybind-find logged a send (its usage log) of the case's KEY.
demo() {
  [ -n "${RECORD:-}" ] || return 0
  local name=$1 key query=${QUERY:-} raw="$OUT/$name.rec" gif="$OUT/$name.gif" cap kbf before now="" size t0
  local usage="$HOME/.local/state/keybind-find/usage.tsv" sent0 sent
  key=${2#*|}; key=${key%%|*}
  [ -n "$query" ] || { query=${2#*|*|}; query=${query%%|*}; }
  rm -rf "$raw" "$raw.mkv" "$gif"
  # wtype has no `--`: a leading `-` would be read as an option.
  [[ "$query" != -* ]] || { record "$name.gif" "FAIL	a query can't start with -"; FAILED=1; return; }
  sent0=$(cat "$usage" 2>/dev/null | wc -l)
  # Capture: an X display (the i3 phase, or niri nested on its Xvfb), else sway's output. At most 60 s; the stop
  # below ends it sooner.
  local xdisp=${DISPLAY:-}
  [ -n "${NIRI_SOCKET:-}" ] && xdisp=:78
  t0=$SECONDS
  if [ -n "$xdisp" ]; then
    ffmpeg -loglevel error -f x11grab -framerate 8 -i "$xdisp" -t 60 -c:v libx264rgb -crf 0 -preset ultrafast \
      "$raw.mkv" </dev/null >"$OUT/$name.rec.log" 2>&1 &
  else
    mkdir -p "$raw"
    # A frame gets its number only once grim has written it whole, so the sequence has no gaps.
    (i=0; while [ $((SECONDS - t0)) -lt 60 ]; do
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
    status=124; kill -- -"$kbf" 2>/dev/null; wait "$kbf" 2>/dev/null
  else
    wait "$kbf"; status=$?
  fi
  sleep 2
  local capped=""
  kill -0 "$cap" 2>/dev/null || capped=1
  # SIGTERM: ffmpeg finishes its file on it (a background job here ignores SIGINT); it ends the grim loop.
  kill "$cap" 2>/dev/null; wait "$cap" 2>/dev/null
  local in=(-i "$raw.mkv") n
  if [ -d "$raw" ]; then
    # The frames' real rate: grim's own time adds to the 0.15 s between them.
    n=$(ls "$raw"/*.png 2>/dev/null | wc -l)
    in=(-framerate "$(awk -v n="$n" -v t=$((SECONDS - t0)) 'BEGIN { printf "%.2f", (t > 0 ? n / t : 5) }')" \
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
  sent=$(tail -n +"$((sent0 + 1))" "$usage" 2>/dev/null | cut -f3 | tr '\n' ' ')
  # The GIF stays on a FAIL: it may show why.
  if [ "$size" -gt 10485760 ]; then
    record "$name.gif" "FAIL	$size bytes: over GitHub's 10 MB for a GIF"
    FAILED=1
  elif [ -n "$capped" ]; then
    record "$name.gif" "FAIL	the capture hit its 60 s limit before the end"
    FAILED=1
  elif [ "$status" -ne 0 ]; then
    record "$name.gif" "FAIL	keybind-find exited $status	$(tail -c 300 "$OUT/$name.kbf.log" | tr '\n\t' '  ')"
    FAILED=1
  elif [ "$sent" != "$key " ]; then
    record "$name.gif" "FAIL	sent '${sent% }', not $key (query: $query)"
    FAILED=1
  else
    record "$name.gif" "ok	$size bytes	query: $query, sent $key"
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

# x NAME "PROVIDERS" "PROVIDER|KEY|DESC" [SETTLE] -- COMMAND...: an X11 case under i3.
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
PHASE=x11 . "$E/cases"
# The daemons the cases left (outside their process groups), so none carries this display into the next phase.
emacsclient -e '(kill-emacs)' >/dev/null 2>&1
tmux kill-server 2>/dev/null
pkill -x screen 2>/dev/null; pkill -x gnome-terminal- 2>/dev/null
kill "$I3" "$XVFB" 2>/dev/null
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
if wanted niri-phase; then
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
