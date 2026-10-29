#!/usr/bin/env bash
#
# Check that two index runs derive the same set for every relation of the
# index engine. The runs may be two builds (a reference such as main, and a
# candidate) or two engines (serial, `-j N`, `--lsm`).
#
# Usage:
#   scripts/fact-dump-diff.sh record OUT -- CMD [ARG...]
#   scripts/fact-dump-diff.sh compare [-n N] [-k KEEP] REF -- CMD [ARG...]
#   scripts/fact-dump-diff.sh diff [-n N] A B
#
#   record   run CMD, a `ctadl index` command line, with CTADL_DUMP_RELATIONS
#            set, and save its relations to the new directory OUT, each one
#            sorted. The command line is saved in OUT/command.
#   compare  run CMD as `record` does, into a temporary directory (or into
#            KEEP with -k), and compare it with the saved dump REF.
#   diff     compare two saved dumps.
#
#   -n N     show the first N rows only on each side of a differing relation
#            (default 5).
#
# CMD picks the build and the engine: serial is the default, `-j N` (N > 1) is
# the parallel engine, `--lsm` is Storage::Lsm. For example:
#
#   scripts/fact-dump-diff.sh record ref-serial -- ctadl index app
#   scripts/fact-dump-diff.sh compare ref-serial -- target/release/ctadl index app
#   scripts/fact-dump-diff.sh compare ref-serial -- target/release/ctadl index app -j 8
#
# The dump is written by the index engine (ctadl-index/src/index_engine/dump.rs).
# Rows are compared as sets: each relation is sorted with `LC_ALL=C sort -u`,
# so row order and duplicates do not matter. Raw dumps and sort's temporary
# files go under $TMPDIR; point it at a disk with room for a few times the
# dump.
#
# Exits 0 if every relation is the same set on both sides, 1 if any differs
# (with a summary per differing relation), 2 on a usage or run error.

set -euo pipefail

usage() {
  sed -n '3,36p' "$0" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

die() {
  echo "fact-dump-diff: $*" >&2
  exit 2
}

tmp_root="${TMPDIR:-/tmp}"
cleanup_dirs=()
cleanup() {
  if [ "${#cleanup_dirs[@]}" -gt 0 ]; then
    rm -rf "${cleanup_dirs[@]}"
  fi
}
trap cleanup EXIT

# record OUT CMD...: run CMD with the dump on, then sort each relation into OUT.
record() {
  local out="$1"
  shift
  [ "$#" -gt 0 ] || die "no command to run"
  if [ -e "$out" ] && [ -n "$(ls -A "$out")" ]; then
    die "$out exists and is not empty"
  fi
  local raw
  raw="$(mktemp -d "$tmp_root/fact-dump.XXXXXX")"
  cleanup_dirs+=("$raw")
  CTADL_DUMP_RELATIONS="$raw/rel" "$@" || die "command failed: $*"
  shopt -s nullglob
  local files=("$raw"/rel/*.tsv)
  shopt -u nullglob
  [ "${#files[@]}" -gt 0 ] ||
    die "the command wrote no dump; is it a \`ctadl index\` run, by a build with CTADL_DUMP_RELATIONS?"
  mkdir -p "$out"
  local f
  for f in "${files[@]}"; do
    LC_ALL=C sort -u -T "$raw" -o "$out/${f##*/}" "$f"
    rm -f "$f"
  done
  printf '%q ' "$@" | sed 's/ $//' >"$out/command"
  echo >>"$out/command"
  echo "fact-dump-diff: recorded ${#files[@]} relations in $out" >&2
}

# side_only FILE OTHER N SAMPLE: the number of rows of FILE not in OTHER, with
# the first N of them written to SAMPLE. One streaming pass.
side_only() {
  LC_ALL=C comm -23 "$1" "$2" |
    awk -v n="$3" -v sample="$4" 'NR <= n { print > sample } END { print NR }'
}

# compare_dumps A B N: compare every relation of two recorded dumps.
compare_dumps() {
  local a="$1" b="$2" n="$3"
  [ -d "$a" ] || die "$a is not a directory"
  [ -d "$b" ] || die "$b is not a directory"
  local names
  names="$(ls -- "$a" "$b" | sed -n 's/\.tsv$//p' | LC_ALL=C sort -u)"
  [ -n "$names" ] || die "no relations in $a or $b"

  local work
  work="$(mktemp -d "$tmp_root/fact-dump-diff.XXXXXX")"
  cleanup_dirs+=("$work")

  local rel same=0 differ=0 total=0
  while IFS= read -r rel; do
    total=$((total + 1))
    local fa="$a/$rel.tsv" fb="$b/$rel.tsv"
    if [ ! -f "$fa" ] || [ ! -f "$fb" ]; then
      differ=$((differ + 1))
      if [ -f "$fa" ]; then
        printf '%s: only in %s (%d rows)\n' "$rel" "$a" "$(wc -l <"$fa")"
      else
        printf '%s: only in %s (%d rows)\n' "$rel" "$b" "$(wc -l <"$fb")"
      fi
      continue
    fi
    if cmp -s "$fa" "$fb"; then
      same=$((same + 1))
      continue
    fi
    differ=$((differ + 1))
    : >"$work/a" >"$work/b"
    local only_a only_b
    only_a="$(side_only "$fa" "$fb" "$n" "$work/a")"
    only_b="$(side_only "$fb" "$fa" "$n" "$work/b")"
    printf '%s: %d rows vs %d rows; %d only in %s, %d only in %s\n' \
      "$rel" "$(wc -l <"$fa")" "$(wc -l <"$fb")" "$only_a" "$a" "$only_b" "$b"
    sed 's/^/  < /' "$work/a"
    sed 's/^/  > /' "$work/b"
  done <<<"$names"

  if [ "$differ" -eq 0 ]; then
    echo "fact-dump-diff: all $total relations are the same set"
    return 0
  fi
  echo "fact-dump-diff: $differ of $total relations differ ($same the same)"
  return 1
}

[ "$#" -gt 0 ] || usage
mode="$1"
shift

rows=5
keep=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -n) [ "$#" -ge 2 ] || usage; rows="$2"; shift 2 ;;
    -k) [ "$#" -ge 2 ] || usage; keep="$2"; shift 2 ;;
    -h|--help) usage ;;
    --) break ;;
    -*) die "unknown option: $1" ;;
    *) break ;;
  esac
done
case "$rows" in
  ''|*[!0-9]*) die "-n takes a number, not '$rows'" ;;
esac

case "$mode" in
  record)
    [ "$#" -ge 3 ] && [ "$2" = "--" ] || usage
    out="$1"
    shift 2
    record "$out" "$@"
    ;;
  compare)
    [ "$#" -ge 3 ] && [ "$2" = "--" ] || usage
    ref="$1"
    shift 2
    [ -d "$ref" ] || die "$ref is not a directory"
    if [ -n "$keep" ]; then
      cand="$keep"
    else
      cand="$(mktemp -d "$tmp_root/fact-dump-cand.XXXXXX")"
      cleanup_dirs+=("$cand")
    fi
    record "$cand" "$@"
    compare_dumps "$ref" "$cand" "$rows"
    ;;
  diff)
    [ "$#" -eq 2 ] || usage
    compare_dumps "$1" "$2" "$rows"
    ;;
  -h|--help) usage ;;
  *) die "unknown mode: $mode" ;;
esac
