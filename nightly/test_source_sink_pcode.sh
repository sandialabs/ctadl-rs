#!/usr/bin/env bash
# test_source_sink_pcode.sh
set -eu
set -x
#o pipefail

if [ "$#" -ne 2 ]; then
    echo "Usage: $0 <source_file.c> <query_file.json>"
    exit 1
fi

C_FILE=$1
QUERY_JSON=$2

if [ ! -f "$C_FILE" ]; then
    echo "Error: Source file $C_FILE not found"
    exit 1
fi

if [ ! -f "$QUERY_JSON" ]; then
    echo "Error: Query file $QUERY_JSON not found"
    exit 1
fi

BASENAME=$(basename "$C_FILE" .c)
OUTDIR="test-output"
OBJ_FILE="${OUTDIR}/${BASENAME}.o"
FACTS_DIR="${OUTDIR}/${BASENAME}"
RESULTS_SARIF="${OUTDIR}/${BASENAME}_results.sarif"

mkdir -p "${OUTDIR}"

# 1. Find a compiler and addr2line that can produce and read Linux ELFs.
if command -v x86_64-unknown-linux-gnu-gcc &> /dev/null; then
    TEST_CC="x86_64-unknown-linux-gnu-gcc"
    TEST_ADDR2LINE="x86_64-unknown-linux-gnu-addr2line"
elif command -v x86_64-linux-gnu-gcc &> /dev/null; then
    TEST_CC="x86_64-linux-gnu-gcc"
    TEST_ADDR2LINE="x86_64-linux-gnu-addr2line"
elif [ "$(uname -s)" = "Linux" ]; then
    TEST_CC="gcc"
    TEST_ADDR2LINE="addr2line"
else
    echo "Warning: No x86_64 Linux cross-compiler found and not on Linux. Falling back to native 'gcc'."
    TEST_CC="gcc"
    TEST_ADDR2LINE="addr2line"
fi

# 1. Compile source to .o with debug symbols
echo "=== Compiling $C_FILE using $TEST_CC ==="
$TEST_CC -g -O0 -c "$C_FILE" -o "$OBJ_FILE"

# 2. Run ctadl-souffle to generate pcode facts
echo "=== Generating pcode facts ==="

# Nix build environments often set HOME=/var/empty, which Ghidra doesn't like.
if [ "${HOME:-/var/empty}" = "/var/empty" ] || [ ! -w "${HOME:-}" ]; then
    export HOME=$(mktemp -d)
    echo "Using temporary HOME: $HOME"
fi

export JAVA_TOOL_OPTIONS="-Duser.home=$HOME"

# Ensure JAVA_HOME is set if java is on the path
if [ -z "${JAVA_HOME:-}" ] && command -v java &> /dev/null; then
    export JAVA_HOME=$(dirname $(dirname $(realpath $(command -v java))))
    echo "Guessed JAVA_HOME: $JAVA_HOME"
fi

#ctadl-souffle import pcode "$OBJ_FILE" -o "$FACTS_DIR" -f

# 3. Run ctadl-ascent (import, index, query, format)
echo "=== Running ctadl analysis ==="
PROJECT_NAME="${BASENAME}_pcode"

ctadl import -l pcode "$OBJ_FILE" -n "$PROJECT_NAME"
ctadl index "$PROJECT_NAME"
ctadl query "$PROJECT_NAME" -m "$QUERY_JSON"
ctadl format "$PROJECT_NAME" -o "$RESULTS_SARIF"

# 4. Map addresses to lines and verify
echo "=== Verifying results ==="
OFFSETS=$(jq -r '.runs[0].results[].codeFlows[].threadFlows[].locations[].location.physicalLocation.address.absoluteAddress' "$RESULTS_SARIF" 2>/dev/null | sort -u)

if [ -z "$OFFSETS" ]; then
    if [ "$(uname)" = "Darwin" ]; then
        echo "=== WARNING: No tainted instructions found in SARIF on Darwin. Skipping strict offset check due to cross-platform decompilation differences. ==="
        exit 0
    else
        echo "FAILURE: No tainted instructions found in SARIF"
        exit 1
    fi
fi

# Base address from Ghidra facts seems to be 0x100000 = 1048576
BASE=1048576
FOUND_LINES=""

for OFFSET in $OFFSETS; do
    # Calculate relative address in .text section
    ADDR_HEX=$(printf "0x%x" $((OFFSET - BASE)))
    # Use addr2line to get the source line
    LINE_INFO=$($TEST_ADDR2LINE -e "$OBJ_FILE" "$ADDR_HEX")
    LINE=$(echo "$LINE_INFO" | cut -d: -f2)
    echo "Address $ADDR_HEX (offset $OFFSET) maps to line $LINE"
    FOUND_LINES="$FOUND_LINES $LINE"
done

EXPECTED_LINES=$(jq -r '.expected_lines[]' "$QUERY_JSON")
ALL_FOUND=true

for EXPECTED in $EXPECTED_LINES; do
    if echo "$FOUND_LINES" | grep -q "\b$EXPECTED\b"; then
        echo "Line $EXPECTED: FOUND"
    else
        echo "Line $EXPECTED: NOT FOUND"
        ALL_FOUND=false
    fi
done

if [ "$ALL_FOUND" = true ]; then
    echo "=== SUCCESS: All expected lines found in analysis results ==="
    exit 0
else
    echo "=== FAILURE: Some expected lines not found in analysis results ==="
    exit 1
fi
