#!/usr/bin/env bash
set -euo pipefail

# Optional first argument: path to a prebuilt ctadl-ascent install/prefix.
CTADL_ASCENT_INSTALL="${1:-}"

if [[ -n "$CTADL_ASCENT_INSTALL" ]]; then
  echo "=== Using prebuilt ctadl-ascent from ${CTADL_ASCENT_INSTALL} ==="
  export PATH="${CTADL_ASCENT_INSTALL}/bin:$PATH"
fi

# Ensure essential tools (ctadl, dex-reader) are in PATH
if ! command -v ctadl &> /dev/null; then
  echo "ERROR: 'ctadl' executable not found on PATH." >&2
  exit 1
fi
if ! command -v dex-reader &> /dev/null; then
  echo "ERROR: 'dex-reader' executable not found on PATH." >&2
  exit 1
fi

export PATH="$PWD/scripts:$PATH"

cleanup() {
  if [[ "${CTADL_CLEAN_OUTPUT:-0}" == "1" ]]; then
    echo "CTADL_CLEAN_OUTPUT=1: removing ./output"
    rm -rf ./output
  else
    echo "CTADL_CLEAN_OUTPUT not set: preserving ./output"
  fi
}
trap cleanup EXIT

# ctadl stores all results into XDG_STATE_HOME, so for these tests, override it
# to an easily observable directory.
export XDG_STATE_HOME=output/state
mkdir -p "$XDG_STATE_HOME"

set -x
# Generate source maps for APK
#ctadl-souffle import jadx "$CTADL_TAINTBENCH_APKS/backflash.apk" -o output/bench -f
#
#ctadl import "$CTADL_TAINTBENCH_APKS/backflash.apk" -n backflash
#ctadl index backflash
#ctadl query backflash ./tests/taintbench/backflash-query.json
#ctadl format backflash > output/results.sarif
#sarif_bytes_to_lines.py --in output/results.sarif --maps output/bench/sources/.maps --out output/results.lines.sarif
#count=$(cat output/results.lines.sarif | jq -r '.runs[].results[].locations[].physicalLocation.region.startLine // empty' | wc -l | tr -d '[:space:]')
# Not sure what to test yet
#(( count > 500 )) || { echo "Expected count > 500, got: $count" >&2; exit 1; }


# Run source-sink tests
echo "=== Running SourceSinkExample test ==="
sh ./test_source_sink.sh tests/java/SourceSinkExample.java tests/java/source-sink-example.json

echo "=== Running AnotherExample test ==="
sh ./test_source_sink.sh tests/java/AnotherExample.java tests/java/another-example.json

echo "=== Running BranchingFlow test ==="
sh ./test_source_sink.sh tests/java/BranchingFlow.java tests/java/branching-flow.json

echo "=== Running LoopFlow test ==="
sh ./test_source_sink.sh tests/java/LoopFlow.java tests/java/loop-flow.json

echo "=== Running FieldFlow test ==="
sh ./test_source_sink.sh tests/java/FieldFlow.java tests/java/field-flow.json

echo "=== Running ArrayFlow test ==="
sh ./test_source_sink.sh tests/java/ArrayFlow.java tests/java/array-flow.json

echo "=== Running ExceptionFlow test ==="
sh ./test_source_sink.sh tests/java/ExceptionFlow.java tests/java/exception-flow.json

echo "=== Running StaticFieldFlow test ==="
sh ./test_source_sink.sh tests/java/StaticFieldFlow.java tests/java/static-field-flow.json

echo "=== Running MethodCallFlow test ==="
sh ./test_source_sink.sh tests/java/MethodCallFlow.java tests/java/method-call-flow.json

echo "=== Running ArrayListFlow test ==="
sh ./test_source_sink.sh tests/java/ArrayListFlow.java tests/java/array-list-flow.json

echo "=== Running ArrayListIteratorFlow test ==="
sh ./test_source_sink.sh tests/java/ArrayListIteratorFlow.java tests/java/array-list-iterator-flow.json

echo "=== Running ObjectSensitivity test ==="
sh ./test_source_sink.sh tests/java/ObjectSensitivity.java tests/java/object-sensitivity.json

echo "=== Running CrossClassStaticFieldFlow test ==="
sh ./test_source_sink.sh tests/java/CrossClassStaticFieldFlow.java tests/java/cross-class-static-field-flow.json

echo "=== Running InstanceMethodFlow test ==="
sh ./test_source_sink.sh tests/java/InstanceMethodFlow.java tests/java/instance-method-flow.json

echo "=== Running FieldSensitivity test ==="
sh ./test_source_sink.sh tests/java/FieldSensitivity.java tests/java/field-sensitivity.json

echo "=== Running StringBuilderFlow test ==="
sh ./test_source_sink.sh tests/java/StringBuilderFlow.java tests/java/string-builder-flow.json

echo "=== Running Reassignment test ==="
sh ./test_source_sink.sh tests/java/Reassignment.java tests/java/reassignment.json

echo "=== Running Pcode tests ==="
sh ./pcode_tests.sh

echo "=== All tests completed successfully ==="
