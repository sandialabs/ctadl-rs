#!/usr/bin/env bash
# Import the same two translation units two ways and compare the taint results.
#
#   big:  `ctadl import -l c <dir>`  -- the directory as one import (once one concatenated
#         buffer; now one program lowered from each file as its own translation unit)
#   tu:   `ctadl import -l c a.c` and `... b.c` as two imports, co-indexed as one project
#
# usage: tests/c/tu_test/run.sh [path/to/ctadl]
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
CTADL="${1:-$ROOT/target/release/ctadl}"
[ -x "$CTADL" ] || CTADL="$ROOT/target/debug/ctadl"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STORE="$WORK/store"

run() { echo "+ $*" >&2; "$@" >"$WORK/log" 2>&1 || { cat "$WORK/log" >&2; return 1; }; }

# big buffer
run "$CTADL" import -l c "$HERE" --name big --store "$STORE"
run "$CTADL" index big --store "$STORE"
run "$CTADL" query big --models "$HERE/model.json" --store "$STORE" -o "$WORK/big.sarif"

# one translation unit at a time
run "$CTADL" import -l c "$HERE/a.c" --name tu_a --store "$STORE"
run "$CTADL" import -l c "$HERE/b.c" --name tu_b --store "$STORE"
run "$CTADL" index tu tu_a tu_b --store "$STORE"
run "$CTADL" query tu --models "$HERE/model.json" --store "$STORE" -o "$WORK/tu.sarif"

python3 - "$WORK/big.sarif" "$WORK/tu.sarif" <<'PY'
import json, sys
def flows(path):
    out = set()
    for run in json.load(open(path)).get("runs", []):
        for r in run.get("results", []):
            if r.get("kind") != "fail":
                continue
            p = r.get("properties", {})
            for s in p.get("sourceFunctions", ["?"]):
                for k in p.get("sinkFunctions", ["?"]):
                    out.add((s, k))
    return out
big, tu = flows(sys.argv[1]), flows(sys.argv[2])
cases = ["sink_intra", "sink_forward", "sink_return", "sink_fp", "sink_global", "sink_reverse"]
print(f"{'case':<14}{'big buffer':<14}{'per-TU':<14}")
for c in cases:
    b = any(k == c for _, k in big); t = any(k == c for _, k in tu)
    print(f"{c:<14}{'found' if b else '-':<14}{'found' if t else '-':<14}")
extra = sorted((big | tu) - {(s, k) for s, k in (big | tu) if k in cases})
if extra:
    print("other results:", extra)
print("SAME" if big == tu else "DIFFERENT")
PY
