#!/bin/sh
# lf's own report of its keys for the lf-oracle case: starts lf in an xterm, waits for its client to answer
# `query <id> maps` (filled once lf runs a shell command: the e2e lfrc's on-init runs `&true`), prints `maps` then
# `cmaps`, and quits that lf. Every wait is bounded.
# lf takes its home from passwd, not $HOME: point it at the e2e config, where the dump (keybind-find's own
# environment) looks.
LF_CONFIG_HOME="$HOME/.config" xterm -e lf &
x=$!
id=""
for _ in $(seq 100); do
  id=$(lf -remote list 2>/dev/null | head -n 1)
  [ -n "$id" ] && break
  sleep 0.1
done
[ -n "$id" ] || { echo "no lf client" >&2; kill "$x"; exit 1; }
maps=""
for _ in $(seq 100); do
  maps=$(lf -remote "query $id maps")
  [ -n "$maps" ] && break
  sleep 0.1
done
[ -n "$maps" ] || { echo "lf $id has no maps" >&2; kill "$x"; exit 1; }
printf '%s\n' "$maps"
lf -remote "query $id cmaps"
lf -remote "send $id quit"
for _ in $(seq 50); do
  kill -0 "$x" 2>/dev/null || exit 0
  sleep 0.1
done
kill "$x"
