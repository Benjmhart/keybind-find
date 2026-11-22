#!/bin/sh
# GIMP's own report of its keys for the gimp-oracle case: the shortcutsrc it writes on a clean exit (every action with
# its keys after its defaults and the user's file; an action still on its defaults commented). GIMP runs in batch mode
# on the harness's display over a copy of the e2e home's GIMP/3.2 in a fresh directory under DIR (the argument: the
# harness's $OUT), so the home's own file, which `dump gimp` reads, stays untouched; it quits at once (about 10 s).
# usage: oracle-gimp.sh DIR
d=$(mktemp -d "${1:-/tmp}/gimp-oracle.XXXXXX") || exit 1
cp -r "$HOME/.config/GIMP/3.2/." "$d/"
GIMP3_DIRECTORY=$d timeout 50 gimp --no-splash --batch-interpreter=plug-in-script-fu-eval -b '(gimp-quit 0)' \
  >"$d.log" 2>&1 </dev/null || { tail -c 300 "$d.log" >&2; exit 1; }
cat "$d/shortcutsrc"
