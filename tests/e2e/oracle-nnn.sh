#!/bin/sh
# nnn's own report of its keys for the nnn-oracle case: its help screen (`?`, show_help), which nnn writes to a temp
# file and opens with $PAGER. nnn needs a terminal, so it runs in a detached tmux on its own socket; PAGER copies the
# file out. Prints the help. Every wait is bounded; the tmux server is killed on exit.
D=$(mktemp -d)
trap 'tmux -L nnn-oracle kill-server 2>/dev/null; rm -rf "${D:?}"' EXIT
trap 'exit 1' INT TERM
printf '#!/bin/sh\ncp "$1" '"'%s/help'"' && touch '"'%s/done'"'\n' "$D" "$D" > "$D/pager"
chmod 755 "$D/pager"
tmux -L nnn-oracle kill-server 2>/dev/null
PAGER="$D/pager" tmux -L nnn-oracle -f /dev/null new-session -d -x 200 -y 50 nnn /tmp
up=""
for _ in $(seq 100); do
  tmux -L nnn-oracle capture-pane -p 2>/dev/null | grep -q '/tmp' && { up=1; break; }
  sleep 0.1
done
[ -n "$up" ] || { echo "nnn didn't start" >&2; exit 1; }
tmux -L nnn-oracle send-keys '?'
for _ in $(seq 100); do
  [ -e "$D/done" ] && { cat "$D/help"; exit 0; }
  sleep 0.1
done
echo "no help from nnn" >&2
exit 1
