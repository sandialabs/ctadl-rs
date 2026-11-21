#!/usr/bin/env bash
#
# Runs the `test` workflow's checks and the nightly suite against the working tree. Builds
# everything first, so no build replaces a binary a running suite uses, then runs the suites
# in parallel (with GNU parallel when it is on PATH), each output line prefixed with its name.
# Exits 1 if anything fails.
#
# `nix run .#check-all` runs it with the regression toolchain on PATH.

set -euo pipefail
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

cargo fmt --all -- --check
cargo clippy --workspace -- -Dwarnings
cargo test --workspace --no-run
cargo test --release -p dex-reader -p jvm-reader --no-run
cargo test --release -p lsm --test mmap_props --no-run
cargo build -p xtask
cargo build --workspace --bin ctadl

suites=(
  "workspace-tests|cargo test --workspace"
  "regression|cargo xtask regression"
  "dex-reader|cargo test --release -p dex-reader"
  "jvm-reader|cargo test --release -p jvm-reader"
  "lsm-props|env PROPTEST_CASES=4096 cargo test --release -p lsm --test mmap_props"
  "verus|scripts/verus.sh ."
)

if parallel --version 2>/dev/null | grep -q 'GNU parallel'; then
  printf '%s\n' "${suites[@]}" |
    parallel --will-cite --colsep '\|' --line-buffer --halt never --tagstring '{1}>' '{=2 uq() =}'
  exit
fi

pids=()
for suite in "${suites[@]}"; do
  name=${suite%%|*}
  (eval "${suite#*|}" 2>&1 | sed "s/^/$name> /") &
  pids+=($!)
done
status=0
for pid in "${pids[@]}"; do wait "$pid" || status=1; done
exit "$status"
