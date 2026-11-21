#!/usr/bin/env bash
#
# Runs the `test` workflow's checks and the nightly suite against the working tree, in the
# current environment: fmt, clippy, the workspace tests, the regression suite, the reader tests
# in release mode, the LSM property tests at nightly's case count, and the Verus proofs.
#
# Usage: scripts/check-all.sh [LOGDIR]
#
#   LOGDIR  where each step writes <step>.log; default $TMPDIR/check-all
#
# Everything is built first, one step at a time, and only then run, in parallel, so no build
# replaces a binary that another step is running. Exits 1 if any step fails.
#
# The regression suite needs the toolchain the dev shell provides. `nix run .#check-all` runs
# this script with that toolchain on PATH, so it works outside the dev shell too.

set -uo pipefail

root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel) || exit
cd "$root" || exit
logs=${1:-${TMPDIR:-/tmp}/check-all}
mkdir -p "$logs" || exit
rm -rf "$logs"/*.log "$logs/verus-out"

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
do_verus() { scripts/verus.sh "$root" "$logs/verus-out"; }

build_steps=(fmt clippy build-tests build-release-tests build-regression)
run_steps=(workspace-tests regression dex-reader-tests jvm-reader-tests lsm-property-tests verus)

failed=()

# Runs step $1, the function do_<step> with `-` as `_`, logging to $logs/$1.log.
step() { "do_${1//-/_}" >"$logs/$1.log" 2>&1; }

# Reports step $1 by its exit status $2.
report() {
  if [ "$2" -eq 0 ]; then
    printf '  PASS  %s\n' "$1"
  else
    printf '  FAIL  %s  (%s)\n' "$1" "$logs/$1.log"
    failed+=("$1")
  fi
}

echo "Logs in $logs"
echo "Building:"
for name in "${build_steps[@]}"; do
  step "$name"
  report "$name" $?
done
if [ "${#failed[@]}" -gt 0 ]; then
  echo "Build failed; not running the suites."
  exit 1
fi

echo "Running:"
pids=()
for name in "${run_steps[@]}"; do
  step "$name" &
  pids+=($!)
done
for i in "${!run_steps[@]}"; do
  wait "${pids[$i]}"
  report "${run_steps[$i]}" $?
done

if [ "${#failed[@]}" -gt 0 ]; then
  echo "${#failed[@]} step(s) failed: ${failed[*]}"
  exit 1
fi
echo "All checks passed."
