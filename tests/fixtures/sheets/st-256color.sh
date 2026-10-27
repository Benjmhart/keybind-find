#!/bin/sh
# A custom parser, as one for st would be: print a DMS cheatsheet on stdout.
cat <<'JSON'
{"title": "st", "provider": "st", "binds": {"clipboard": [
  {"key": "ctrl+shift+c", "desc": "Copy", "action": ""},
  {"key": "ctrl+shift+v", "desc": "Paste", "action": ""}
]}}
JSON
