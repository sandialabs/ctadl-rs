#!/usr/bin/env bash
#
# Reject scratch files and scratch markers in a range of commits.
#
# Usage:
#   scripts/no-scratch.sh [-l|--list] [BASE [HEAD]]
#
#   BASE, HEAD  commits bounding the range to check (BASE...HEAD). They may
#               also be passed in the environment as $BASE and $HEAD, which is
#               how the "No scratch" GitHub workflow invokes this script.
#               Defaults: BASE is the merge base of HEAD with origin/main (or
#               main), HEAD is the working tree's HEAD.
#
#   -l, --list  print only the offending file paths, one per line, and nothing
#               else. Handy for cleaning up a branch:
#
#                   scripts/no-scratch.sh --list | xargs -r git rm
#
# Exits 1 if anything was flagged, 0 otherwise.

set -uo pipefail

list_only=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -l|--list) list_only=1; shift ;;
    -h|--help)
      sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    --) shift; break ;;
    -*)
      echo "unknown option: $1" >&2
      exit 2
      ;;
    *) break ;;
  esac
done

BASE="${1-${BASE-}}"
HEAD_REF="${2-${HEAD-}}"

: "${HEAD_REF:=HEAD}"

if [ -z "$BASE" ]; then
  for upstream in origin/main main; do
    if BASE="$(git merge-base "$HEAD_REF" "$upstream" 2>/dev/null)"; then
      break
    fi
    BASE=""
  done
fi

if [ -z "$BASE" ]; then
  echo "could not determine a base commit; pass one explicitly" >&2
  exit 2
fi

# Split so this script does not trip its own check.
marker="DO-NOT""-MERGE"

# In GitHub Actions, emit workflow annotations; elsewhere, plain lines.
report() { # path, message
  if [ "$list_only" -eq 1 ]; then
    printf '%s\n' "$1"
  elif [ -n "${GITHUB_ACTIONS-}" ]; then
    echo "::error file=$1::$2"
  else
    echo "$1: $2"
  fi
}

fail=0
while IFS= read -r -d '' f; do
  case "$f" in
    scratch/*|*/scratch/*|*.scratch.*)
      report "$f" "scratch path must not be merged"
      fail=1
      continue
      ;;
  esac

  # Use grep -c (reads to EOF) rather than grep -q (exits early):
  # under pipefail, grep -q's early exit SIGPIPEs the upstream greps,
  # and that 141 becomes the pipeline status, silently masking a match.
  if [ "$(git diff "$BASE...$HEAD_REF" -- "$f" \
    | grep -v '^+++' \
    | grep -c "^+.*$marker")" -gt 0 ]
  then
    report "$f" "added line contains $marker"
    fail=1
  fi
done < <(git diff -z --name-only --diff-filter=AM "$BASE...$HEAD_REF")

if [ "$fail" -ne 0 ]; then
  if [ "$list_only" -eq 0 ]; then
    echo
    echo "Remove the scratch files/lines above, or drop the $marker comment if the code is meant to ship."
  fi
  exit 1
fi
