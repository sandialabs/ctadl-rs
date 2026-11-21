#!/usr/bin/env bash
#
# Runs the Verus proofs of the verified crates: order-laws, multiset-diff, index-canon and
# sarif-canon. A crate another one uses has its specs exported and its library compiled, so the
# other can use its theorems.
#
# Usage: scripts/verus.sh ROOT [OUT]
#
#   ROOT  a directory holding <crate>/src for each of the four crates (the repo root will do)
#   OUT   where the exported specs and compiled libraries go; created if missing. Without it,
#         they go in a temporary directory that is removed on exit.

set -euo pipefail

root=$1
if [ $# -ge 2 ]; then
  out=$2
  mkdir -p "$out"
else
  out=$(mktemp -d)
  trap 'rm -rf "$out"' EXIT
fi

v() { verus --crate-type=lib --edition=2024 --no-report-long-running "$@"; }
ol=(-L "dependency=$out" --import "order_laws=$out/ol.vir" --extern "order_laws=$out/liborder_laws.rlib")

v --crate-name order_laws --export "$out/ol.vir" --compile -o "$out/liborder_laws.rlib" \
  "$root/order-laws/src/lib.rs"
v "${ol[@]}" --crate-name multiset_diff --export "$out/md.vir" --compile -o "$out/libmultiset_diff.rlib" \
  "$root/multiset-diff/src/lib.rs"
v "${ol[@]}" "$root/index-canon/src/lib.rs"
v "${ol[@]}" --crate-name sarif_canon \
  --import "multiset_diff=$out/md.vir" --extern "multiset_diff=$out/libmultiset_diff.rlib" \
  "$root/sarif-canon/src/lib.rs"
