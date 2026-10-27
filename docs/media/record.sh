#!/usr/bin/env bash
# Records the README demo (story 5.3): a screen region with ffmpeg's x11grab while you act out the scenario, then
# docs/media/demo.gif (at most 20 s, 5 MB, 1000 px wide) and docs/media/screenshot.png.
#
# Scenario, on X11 with keybind-find bound to Super+/:
#   1. An alacritty window running nvim on a file, in normal mode, fills the region below; the terminal running
#      this script is elsewhere (another monitor or workspace). Pause desktop notifications.
#   2. Run this script; it waits 5 s (switch to the nvim window), then records for DURATION seconds (default 20).
#   3. Press Super+/: the list opens in key search.
#   4. Press a key: the list narrows to the bindings that start with it.
#   5. Press Escape, type a word from a binding that visibly changes nvim (e.g. "zen" for LazyVim's Space u z), press
#      Enter: the binding runs.
#
# The GIF ships in the public repo. Before committing, look at every frame for personal data: your own bindings
# (window manager, espanso, herdr, nvim), espanso replacements, window titles, paths, notifications. If anything
# shows, record again (a demo config, another workspace); never blur. Commit only the approved take.
#
# Usage: docs/media/record.sh [WxH+X+Y]   (default: 1280x800 centred on the X screen; with several monitors, pass the
# region of the one nvim is on, e.g. from `xrandr`)
# Env: DURATION=<seconds, at most 20> (default 20); SHOT_AT=<seconds into the recording> for the screenshot
# (default: half way).
set -euo pipefail
export LC_ALL=C

for tool in ffmpeg ffprobe xdotool; do
  command -v "$tool" >/dev/null || { echo "record.sh: $tool is not installed" >&2; exit 1; }
done
[ -n "${DISPLAY:-}" ] || { echo "record.sh: no DISPLAY (x11grab records an X11 screen)" >&2; exit 1; }
if [ -n "${WAYLAND_DISPLAY:-}" ] || [ "${XDG_SESSION_TYPE:-}" = wayland ]; then
  echo "record.sh: a Wayland session: x11grab would record only XWayland; record on X11" >&2; exit 1
fi

here=$(cd -P -- "$(dirname -- "$0")" >/dev/null && pwd)
if [ $# -ge 1 ]; then
  region=$1
else
  geometry=$(xdotool getdisplaygeometry) || { echo "record.sh: xdotool can't read the screen size" >&2; exit 1; }
  read -r sw sh <<<"$geometry"
  w=$((sw < 1280 ? sw : 1280)) h=$((sh < 800 ? sh : 800))
  region="${w}x${h}+$(((sw - w) / 2))+$(((sh - h) / 2))"
fi
[[ $region =~ ^([0-9]+)x([0-9]+)\+([0-9]+)\+([0-9]+)$ ]] || { echo "record.sh: region must be WxH+X+Y, not $region" >&2; exit 2; }
size="${BASH_REMATCH[1]}x${BASH_REMATCH[2]}" offset="${BASH_REMATCH[3]},${BASH_REMATCH[4]}"
[ "${BASH_REMATCH[1]}" -gt 0 ] && [ "${BASH_REMATCH[2]}" -gt 0 ] || { echo "record.sh: region $region is empty" >&2; exit 2; }

duration=${DURATION:-20}
[[ $duration =~ ^[0-9]+$ ]] && [ "$duration" -ge 1 ] && [ "$duration" -le 20 ] ||
  { echo "record.sh: DURATION must be 1-20 seconds, not $duration" >&2; exit 2; }
shot_at=${SHOT_AT:-}
if ! [[ -z $shot_at || $shot_at =~ ^[0-9]+([.][0-9]+)?$ ]] ||
  ! awk -v t="${shot_at:-0}" -v d="$duration" 'BEGIN { exit !(t < d) }'; then
  echo "record.sh: SHOT_AT must be a number of seconds under DURATION ($duration), not $shot_at" >&2; exit 2
fi

tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
echo "Recording $region in 5 s, for $duration s: switch to the nvim window now."
sleep 5
# ffv1: lossless and built into ffmpeg (no libx264 needed).
ffmpeg -nostdin -hide_banner -loglevel error -f x11grab -framerate 15 -video_size "$size" -i "$DISPLAY+$offset" \
  -t "$duration" -c:v ffv1 "$tmp/raw.mkv"

# One palette for the whole clip; 10 fps, at most 1000 px wide and redrawing only the changed rectangle keep it small.
filters="fps=10,scale='min(1000,iw)':-2:flags=lanczos"
ffmpeg -nostdin -hide_banner -loglevel error -y -i "$tmp/raw.mkv" \
  -filter_complex "$filters,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle" \
  "$tmp/demo.gif"
length=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$tmp/raw.mkv")
ffmpeg -nostdin -hide_banner -loglevel error -y -ss "${shot_at:-$(awk -v d="$length" 'BEGIN { print d / 2 }')}" -i "$tmp/raw.mkv" \
  -frames:v 1 -vf "scale='min(1000,iw)':-2:flags=lanczos" "$tmp/screenshot.png"
[ -s "$tmp/screenshot.png" ] || { echo "record.sh: no frame at ${shot_at:-half way} for the screenshot" >&2; exit 1; }

# Checked before anything in docs/media/ is replaced, so a failed take keeps the last good one.
bytes=$(stat -c %s "$tmp/demo.gif")
gif_length=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$tmp/demo.gif")
gif_width=$(ffprobe -v error -select_streams v:0 -show_entries stream=width -of csv=p=0 "$tmp/demo.gif")
echo "demo.gif: $((bytes / 1024)) KiB, ${gif_length} s, ${gif_width} px wide; screenshot.png at ${shot_at:-half way}"
fail=
[ "$bytes" -le $((5 * 1024 * 1024)) ] || { echo "record.sh: demo.gif is over 5 MB: record a smaller region or a shorter take" >&2; fail=1; }
if ! [[ $gif_length =~ ^[0-9]+([.][0-9]+)?$ ]] || ! awk -v d="$gif_length" 'BEGIN { exit !(d <= 20.5) }'; then
  echo "record.sh: demo.gif is over 20 s (or its length can't be read: $gif_length)" >&2; fail=1
fi
[ "$gif_width" -le 1000 ] || { echo "record.sh: demo.gif is over 1000 px wide" >&2; fail=1; }
[ -z "$fail" ] || exit 1
mv "$tmp/demo.gif" "$tmp/screenshot.png" "$here/"
echo "Now look at every frame for personal data before committing (e.g. open demo.gif in a viewer and step through)."
