#!/bin/sh
# Echoes what the request carries, one description each: the version, each input's path and text, then the window's
# wm, class, title and program. The request is one line of compact JSON; the values used here hold no quotes.
req=$(cat)
field() { printf '%s\n' "$req" | grep -o "\"$1\":\"[^\"]*\"" | sed "s/\"$1\":\"\(.*\)\"/\1/"; }
binds=$(printf '%s\n' "$req" | grep -o '"version":[0-9]*' | sed 's/"version":\(.*\)/{"key": "v", "desc": "version \1"},/')
binds="$binds$(printf '%s\n' "$req" | grep -o '"path":"[^"]*","text":"[^"]*"' |
  sed 's/"path":"\(.*\)","text":"\(.*\)"/{"key": "p", "desc": "\1"}, {"key": "t", "desc": "\2"},/')"
for f in wm class title program; do
  binds="$binds {\"key\": \"c\", \"desc\": \"$(field $f)\"},"
done
echo "{\"version\": 1, \"title\": \"Request\", \"provider\": \"x\", \"binds\": {\"Request\": [${binds%,}]}}"
