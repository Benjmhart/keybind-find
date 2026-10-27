#!/bin/sh
# Prints one sheet, ignoring the request; its key is in a non-canonical order and it carries an action.
cat >/dev/null
echo '{"version": 1, "title": "Echo", "provider": "anything", "binds": {"Tools": [{"key": "shift+ctrl+k", "desc": "do a thing", "action": "xdg-open https://example.com"}]}}'
