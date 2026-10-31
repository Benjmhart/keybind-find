"""Checks one `keybind-find dump` (a JSON array of sheets) for tests/e2e/harness.sh.

usage: check.py DUMP.json "PROVIDER ..." "PROVIDER|KEY|DESC-PART" ("-" for no key check)
Prints the providers with their bind counts, then `ok` or what is missing; exits 1 when something is.
"""
import json
import sys

dump, providers, key = sys.argv[1], sys.argv[2].split(), sys.argv[3]
try:
    sheets = json.load(open(dump))
except Exception as e:
    print(f"\tnot JSON: {e}")
    sys.exit(1)
counts = {s["provider"]: sum(len(b) for b in s["binds"].values()) for s in sheets}
print(" ".join(f"{p}:{n}" for p, n in counts.items()), end="\t")
missing = [p for p in providers if p not in counts]
if key != "-":
    prov, k, part = key.split("|", 2)
    binds = [b for s in sheets if s["provider"] == prov for bs in s["binds"].values() for b in bs]
    if not any(b["key"] == k and part in b["desc"] for b in binds):
        found = [b["desc"] for b in binds if b["key"] == k]
        missing.append(f"{prov} {k} '{part}' (that key: {found[:3]})")
print("ok" if not missing else "MISSING " + "; ".join(missing))
sys.exit(1 if missing else 0)
