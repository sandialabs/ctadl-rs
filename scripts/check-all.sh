#!/usr/bin/env bash
#
# Runs the `test` workflow's checks and the nightly suite against the working tree, in the
# current environment: fmt, clippy, the workspace tests, the regression suite, the reader tests
# in release mode, the LSM property tests at nightly's case count, and the Verus proofs.
#
# Usage: scripts/check-all.sh
#
# Everything is built first, one step at a time, and only then run, in parallel, so no build
# replaces a binary that another step is running. The parallel steps prefix each line of their
# output with the step's name. Exits 1 if any step fails.
#
# The regression suite needs the toolchain the dev shell provides. `nix run .#check-all` runs
# this script with that toolchain on PATH, so it works outside the dev shell too.

set -uo pipefail

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel) || exit
cd "$root" || exit

do_fmt() { cargo fmt --all -- --check; }
do_clippy() { cargo clippy --workspace -- -Dwarnings; }
do_build_tests() { cargo test --workspace --no-run; }
do_build_release_tests() {
  cargo test --release -p dex-reader -p jvm-reader --no-run &&
    cargo test --release -p lsm --test mmap_props --no-run
}
# The ctadl build is the one `xtask regression` makes before its cases.
do_build_regression() { cargo build -p xtask && cargo build --workspace --bin ctadl; }

do_workspace_tests() { cargo test --workspace; }
do_regression() { cargo xtask regression; }
do_dex_reader_tests() { cargo test --release -p dex-reader; }
do_jvm_reader_tests() { cargo test --release -p jvm-reader; }
do_lsm_property_tests() { PROPTEST_CASES=4096 cargo test --release -p lsm --test mmap_props; }
do_verus() {
  local out status
  out=$(mktemp -d) || return
  scripts/verus.sh "$root" "$out"
  status=$?
  rm -rf "$out"
  return "$status"
}

build_steps=(fmt clippy build-tests build-release-tests build-regression)
run_steps=(workspace-tests regression dex-reader-tests jvm-reader-tests lsm-property-tests verus)

failed=()
summary=()

# Runs step $1: the function do_<step>, with `-` as `_`.
step() { "do_${1//-/_}" 2>&1; }

# Runs step $1 with each line of its output prefixed by `$1> `.
prefixed() {
  step "$1" | while IFS= read -r line; do printf '%s> %s\n' "$1" "$line"; done
}

# Records step $1's exit status $2.
record() {
  if [ "$2" -eq 0 ]; then
    summary+=("  PASS  $1")
  else
    summary+=("  FAIL  $1")
    failed+=("$1")
  fi
}

finish() {
  echo
  printf '%s\n' "${summary[@]}"
  if [ "${#failed[@]}" -gt 0 ]; then
    echo "${#failed[@]} step(s) failed: ${failed[*]}"
    exit 1
  fi
  echo "All checks passed."
}

for name in "${build_steps[@]}"; do
  echo "== $name"
  step "$name"
  record "$name" $?
done
if [ "${#failed[@]}" -gt 0 ]; then
  echo "Build failed; not running the suites."
  finish
fi

echo "== running: ${run_steps[*]}"
pids=()
for name in "${run_steps[@]}"; do
  prefixed "$name" &
  pids+=($!)
done
for i in "${!run_steps[@]}"; do
  wait "${pids[$i]}"
  record "${run_steps[$i]}" $?
done
finish
