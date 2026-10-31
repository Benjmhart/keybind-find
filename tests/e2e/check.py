"""Checks one `keybind-find dump` (a JSON array of sheets) for tests/e2e/harness.sh.

usage: check.py DUMP.json "PROVIDER ..." "PROVIDER|KEY|DESC-PART[|SCOPE=VALUE,...]"
Prints the providers with their bind counts, then `ok` or what is missing; exits 1 when something is. Each provider
named must be there with at least one bind; the key must be listed by that provider with the description part, and
carry each scope given.
"""
import json
import sys


def main():
    dump, providers, key = sys.argv[1], sys.argv[2].split(), sys.argv[3]
    try:
        sheets = json.load(open(dump))
        counts = {s["provider"]: sum(len(b) for b in s["binds"].values()) for s in sheets}
    except Exception as e:
        print(f"\tunreadable dump: {e!r}")
        return 1
    print(" ".join(f"{p}:{n}" for p, n in counts.items()), end="\t")
    missing = [p if p not in counts else f"{p} (no binds)" for p in providers if counts.get(p, 0) == 0]
    prov, k, part, *scopes = key.split("|")
    want = dict(s.split("=", 1) for s in scopes[0].split(",")) if scopes else {}
    binds = [b for s in sheets if s["provider"] == prov for bs in s["binds"].values() for b in bs]

    def scoped(b):
        when = b.get("when") or {}
        return all(v == when.get(n) or [v] == when.get(n) for n, v in want.items())

    if not any(b.get("key") == k and part in (b.get("desc") or "") and scoped(b) for b in binds):
        found = [(b.get("desc"), b.get("when")) for b in binds if b.get("key") == k]
        missing.append(f"{prov} {k} '{part}'{' ' + str(want) if want else ''} (that key: {found[:3]})")
    print("ok" if not missing else "MISSING " + "; ".join(missing))
    return 1 if missing else 0


sys.exit(main())
