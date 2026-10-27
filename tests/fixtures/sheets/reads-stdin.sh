#!/bin/sh
# stdin must be /dev/null (closed), not whatever keybind-find was started with.
[ "$(readlink /proc/self/fd/0)" = /dev/null ] || { echo "stdin is $(readlink /proc/self/fd/0)" >&2; exit 1; }
cat >/dev/null
echo '{"title":"t","provider":"x","binds":{"misc":[{"key":"F1","desc":"Help"}]}}'
