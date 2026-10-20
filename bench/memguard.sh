#!/bin/zsh
# Run a command under a physical-footprint cap, on macOS.
#
#   memguard.sh <limit-GiB> <command> [args...]
#
# The PID comes from `$!` of the command this script launched — never from
# pgrep: a pattern that matches the binary also matches any shell whose command
# line mentions it, and polling the wrong process yields a small, valid integer
# that sails under any cap forever.
#
# Exits with the command's status, or 137 if the guard killed it. Peak
# footprint and wall time go to stderr so stdout stays the command's own.

if [ $# -lt 2 ]; then
  echo "usage: memguard.sh <limit-GiB> <command> [args...]" >&2
  exit 2
fi
LIMIT_GIB=$1; shift
LIMIT=$(( LIMIT_GIB * 1024 * 1024 * 1024 ))

"$@" &
PID=$!

is_uint() { case "$1" in ''|*[!0-9]*) return 1;; *) return 0;; esac; }

# `footprint -p` can block indefinitely -- while macOS waits for someone at the console to
# authorize developer-tools access to other processes -- which would leave the guard, and
# whoever waits on it, hung long after the command is done. Each reading gets FP_TIMEOUT
# seconds; once one runs out, the rest of the run reads the resident size from ps instead,
# which undercounts compressed memory but keeps the cap in force.
FP_TIMEOUT=30
FP_OUT=$(mktemp "${TMPDIR:-/tmp}/memguard.XXXXXX")
FP_BLIND="$FP_OUT.blind"  # a file, since readings are taken in subshells
trap 'rm -f "$FP_OUT" "$FP_BLIND"' EXIT
rssbytes() {
  local kb=$(ps -o rss= -p "$1" 2>/dev/null | tr -d ' ')
  is_uint "$kb" && echo $(( kb * 1024 ))
}
fpbytes() {
  if [ -e "$FP_BLIND" ]; then
    rssbytes "$1"
    return
  fi
  footprint -p "$1" -f bytes >"$FP_OUT" 2>/dev/null &
  local fp=$! waited=0
  while kill -0 $fp 2>/dev/null; do
    if (( waited >= FP_TIMEOUT * 10 )); then
      kill -9 $fp 2>/dev/null
      wait $fp 2>/dev/null
      touch "$FP_BLIND"
      echo "memguard: footprint gave no reading in ${FP_TIMEOUT}s; using ps rss from now on" >&2
      rssbytes "$1"
      return
    fi
    sleep 0.1
    waited=$(( waited + 1 ))
  done
  wait $fp 2>/dev/null
  awk '/phys_footprint:/{print $2; exit}' "$FP_OUT"
}

# The command may be a wrapper — `/usr/bin/time -l ./analysis`, a `sh -c`, a
# cargo runner — whose own footprint is a megabyte while the process doing the
# work is its child. Sum the tree, walking it by parent (`pgrep -P`) rather
# than by name, so nothing is matched by pattern.
tree_pids() {
  local frontier=("$1") pids=("$1") next c p
  while (( ${#frontier} )); do
    next=()
    for p in $frontier; do
      for c in $(pgrep -P $p 2>/dev/null); do next+=($c); pids+=($c); done
    done
    frontier=($next)
  done
  echo $pids
}

tree_footprint() {
  local total=0 b
  for p in $(tree_pids "$1"); do
    b=$(fpbytes $p)
    is_uint "$b" && total=$(( total + b ))
  done
  # Zero means every process in the tree vanished mid-sample, which is a
  # failed reading, not a measurement of zero.
  [ "$total" -gt 0 ] && echo "$total"
}

PEAK=0; START=$(date +%s); KILLED=0; SAMPLES=0
while kill -0 $PID 2>/dev/null; do
  B=$(tree_footprint $PID)
  if ! is_uint "$B"; then
    # An unreadable gauge while the process is alive means the guard is blind.
    # Kill rather than limp on: that is how a run sails past the cap.
    if kill -0 $PID 2>/dev/null; then
      echo "memguard: no footprint for the tree under pid $PID ('$B') — killing" >&2
      kill -9 $(tree_pids $PID) 2>/dev/null; KILLED=1
    fi
    break
  fi
  SAMPLES=$((SAMPLES + 1))
  [ "$B" -gt "$PEAK" ] && PEAK=$B
  if [ "$B" -gt "$LIMIT" ]; then
    printf 'memguard: pid %s at %.2f GiB > %s GiB cap — killing\n' \
        "$PID" "$(echo "scale=2;$B/1073741824" | bc)" "$LIMIT_GIB" >&2
    kill -9 $(tree_pids $PID) 2>/dev/null; KILLED=1
    break
  fi
  sleep 2
done
wait $PID 2>/dev/null
STATUS=$?

# A run that ended before the first successful sample was never guarded. Say
# so rather than reporting a peak of 0.00 GiB as if it were a measurement.
if [ "$SAMPLES" -eq 0 ]; then
  echo "memguard: no sample taken — process exited before the first poll; peak unknown" >&2
else
  printf 'memguard: peak %.2f GiB over %d samples, wall %ds\n' \
      "$(echo "scale=2;$PEAK/1073741824" | bc)" "$SAMPLES" "$(( $(date +%s) - START ))" >&2
fi
[ "$KILLED" -eq 1 ] && exit 137
exit $STATUS
