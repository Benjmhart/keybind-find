#!/bin/sh
# Runs each template's own test, then `keybind-find check-plugin` on it with its sample input.
#   templates/check.sh [path/to/keybind-find]   (default: keybind-find on PATH)
# A template whose interpreter isn't installed is skipped with a message; under CI (CI set) that is a failure,
# except Lua, which CI doesn't install. Exit status: 0 when every template that ran passed.
set -u
kbf=${1:-keybind-find}
here=$(cd -P -- "$(dirname -- "$0")" >/dev/null && pwd)
failed=0
# Logs outside the tree: an interrupted run leaves nothing behind in it.
log=$(mktemp)
trap 'rm -f "$log"' EXIT

has() { command -v "$1" >/dev/null 2>&1; }

skip() {
  if [ -n "${CI:-}" ] && [ "$2" != lua ]; then
    echo "FAIL $1: $2 is not installed (CI must run it)"
    failed=1
  else
    echo "skip $1: $2 is not installed"
  fi
}

# check <name> <plugin path> <sample> <test command...>
check() {
  name=$1 plugin=$2 sample=$3
  shift 3
  if ! (cd "$here/$name" && "$@") >"$log" 2>&1; then
    echo "FAIL $name: its test failed:"
    sed 's/^/    /' "$log"
    failed=1
  elif ! "$kbf" check-plugin "$plugin" --input "$sample" --program toy >"$log" 2>&1; then
    echo "FAIL $name: check-plugin found problems:"
    sed 's/^/    /' "$log"
    failed=1
  else
    echo "ok   $name"
  fi
}

# Its own target dir whatever CARGO_TARGET_DIR says, so the binary checked is the one just built. Its own lint
# too: it isn't in keybind-find's workspace. `--locked`: after a change to keybind-find-sheet's version or
# dependencies, refresh its lock with `cargo update -p keybind-find-sheet` in templates/rust.
if has cargo; then
  check rust "$here/rust/target/debug/keybind-find-toy" "$here/rust/tests/fixtures/keys.conf" \
    sh -c 'cargo fmt --check && cargo clippy --quiet --locked --all-targets --target-dir target -- -D warnings &&
      cargo test --quiet --locked --target-dir target && cargo build --quiet --locked --target-dir target'
else
  skip rust cargo
fi
if has python3; then
  check python "$here/python/keybind-find-toy" "$here/python/tests/keys.conf" python3 test_plugin.py
else
  skip python python3
fi
if has node; then
  check js "$here/js/keybind-find-toy" "$here/js/tests/keys.conf" node test.js
else
  skip js node
fi
# The plugin's `#!/usr/bin/env lua` needs a `lua` on PATH (Debian and Ubuntu name it lua5.4; see its README).
if has lua; then
  check lua "$here/lua/keybind-find-toy" "$here/lua/tests/keys.conf" lua test.lua
else
  skip lua lua
fi
exit $failed
