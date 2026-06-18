#!/bin/bash

# Script to test source-sink analysis with bytecode offset mapping
# This script compiles a Java file to DEX, runs ctadl analysis,
# and verifies that the specified lines are correctly identified
# Usage: ./test_source_sink.sh <java_file> <json_config>

set -e  # Exit on error

# Check arguments
if [ $# -ne 2 ]; then
    echo "Usage: $0 <java_file> <json_config>"
    echo "Example: $0 tests/java/SourceSinkExample.java tests/java/source-sink-example.json"
    exit 1
fi

JAVA_FILE_ARG="$1"
JSON_CONFIG_ARG="$2"

# Configuration - determine paths relative to this script's location
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_DIR="$SCRIPT_DIR/tests/java"

# Handle Java file path - if it's relative to SOURCE_DIR, use it as-is, otherwise make it relative
if [[ "$JAVA_FILE_ARG" = /* ]]; then
    # Absolute path
    JAVA_FILE="$JAVA_FILE_ARG"
else
    # Relative path - assume it's relative to the current directory or SOURCE_DIR
    if [[ "$JAVA_FILE_ARG" = */* ]]; then
        # Contains a slash, treat as relative to current directory
        JAVA_FILE="$SCRIPT_DIR/$JAVA_FILE_ARG"
    else
        # Just a filename, look in SOURCE_DIR
        JAVA_FILE="$SOURCE_DIR/$JAVA_FILE_ARG"
    fi
fi

# Handle JSON config path - same logic
if [[ "$JSON_CONFIG_ARG" = /* ]]; then
    # Absolute path
    JSON_CONFIG="$JSON_CONFIG_ARG"
else
    # Relative path
    if [[ "$JSON_CONFIG_ARG" = */* ]]; then
        # Contains a slash, treat as relative to current directory
        JSON_CONFIG="$SCRIPT_DIR/$JSON_CONFIG_ARG"
    else
        # Just a filename, look in SOURCE_DIR
        JSON_CONFIG="$SOURCE_DIR/$JSON_CONFIG_ARG"
    fi
fi

# Extract base name from Java file (without extension)
BASE_NAME=$(basename "$JAVA_FILE" .java)
CLASS_FILE="$SOURCE_DIR/$BASE_NAME.class"
DEX_FILE="$SOURCE_DIR/$BASE_NAME.dex"
LINEMAP_FILE="$SOURCE_DIR/${BASE_NAME}_linemap.json"
SARIF_FILE="$SOURCE_DIR/${BASE_NAME}_output.sarif"
PROJECT_NAME="${BASE_NAME}_test"

echo "Java file: $JAVA_FILE"
echo "JSON config: $JSON_CONFIG"
echo "Base name: $BASE_NAME"
echo "DEX file: $DEX_FILE"
echo "Linemap file: $LINEMAP_FILE"
echo "SARIF file: $SARIF_FILE"
echo "Project name: $PROJECT_NAME"

# Create a working directory
WORK_DIR="${TMPDIR:-/tmp}/ctadl_test_$BASE_NAME"
rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR"
cd "$WORK_DIR"

# Clean up previous runs
rm -f "$DEX_FILE" "$LINEMAP_FILE" "$SARIF_FILE"
rm -f "$CLASS_FILE"

echo "=== Compiling Java to DEX ==="
# Change to source directory and compile
cd "$SOURCE_DIR"
rm -f "$BASE_NAME.class" "$BASE_NAME.dex"
echo "Current directory: $(pwd)"
echo "Files before compilation: $(ls -la ${BASE_NAME}*)"

# First compile Java to class file with Java 8 compatibility
javac --release 8 "$BASE_NAME.java"
echo "Compiled $BASE_NAME.java to class file"
echo "Files after javac: $(ls -la ${BASE_NAME}*)"

# Then convert class file to DEX
# Collect all classes generated from this source file (e.g., inner classes or co-located classes)
# Since they might be named differently (e.g. MyInterface.class), we can just dx all classes
# or better yet, dx the whole directory, but we need to be careful not to include other tests.
# A safe approach is to just run `javac` in a clean directory, or use `dx --dex --output=X.dex *.class` 
# after cleaning other classes. Let's make a temporary build dir.

BUILD_DIR="${TMPDIR:-/tmp}/ctadl_build_$BASE_NAME"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
cp "$BASE_NAME.java" "$BUILD_DIR/"
cd "$BUILD_DIR"

javac --release 8 "$BASE_NAME.java"
dx --dex --output="$BASE_NAME.dex" *.class
cp "$BASE_NAME.dex" "$SOURCE_DIR/"
cd "$SOURCE_DIR"

echo "Compiled class file to $BASE_NAME.dex"
echo "Files after dx: $(ls -la ${BASE_NAME}*)"

# Copy DEX file to working directory
cp "$BASE_NAME.dex" "$WORK_DIR/"
cd "$WORK_DIR"

echo "=== Running ctadl analysis ==="
# Import, index, query, and format
ctadl import --name "$PROJECT_NAME" "$BASE_NAME.dex"
ctadl index "$PROJECT_NAME"
ctadl query "$PROJECT_NAME" -m "$JSON_CONFIG"
ctadl format "$PROJECT_NAME" -o "$SARIF_FILE"
echo "Generated SARIF output: $SARIF_FILE"

echo "=== Generating linemap ==="
dex-reader "$BASE_NAME.dex" --linemap-json "$LINEMAP_FILE"
echo "Generated linemap: $LINEMAP_FILE"

# Check if SARIF file was created and has content
echo "=== Checking SARIF file ==="
if [ ! -f "$SARIF_FILE" ]; then
    echo "SARIF file not found at $SARIF_FILE"
    echo "Current directory contents:"
    ls -la
    echo "Looking for SARIF files:"
    find . -name "*.sarif" -o -name "*sarif*"
    exit 1
fi

if [ ! -s "$SARIF_FILE" ]; then
    echo "SARIF file is empty"
    exit 1
fi

echo "SARIF file exists and has content"

# Read expected lines from JSON config first
echo "=== Reading expected lines from JSON config ==="
if [ ! -f "$JSON_CONFIG" ]; then
    echo "JSON config file not found: $JSON_CONFIG"
    exit 1
fi

# Check if expected_lines key exists in JSON config
if ! jq -e '.expected_lines' "$JSON_CONFIG" >/dev/null; then
    echo "No expected_lines key found in JSON config: $JSON_CONFIG"
    exit 1
fi

EXPECTED_LINES_JSON=$(jq -r '.expected_lines | join(",")' "$JSON_CONFIG")

EXPECTED_LINES=()
if [ -n "$EXPECTED_LINES_JSON" ] && [ "$EXPECTED_LINES_JSON" != "null" ]; then
    # Convert comma-separated string to array
    IFS=',' read -ra EXPECTED_LINES_ARRAY <<< "$EXPECTED_LINES_JSON"
    # Trim whitespace from each element
    for line in "${EXPECTED_LINES_ARRAY[@]}"; do
        EXPECTED_LINES+=("$(echo -e "${line}" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')")
    done
fi

echo "Expected lines: ${EXPECTED_LINES[*]}"

# Extract bytecode offsets from SARIF output
echo "=== Extracting bytecode offsets from SARIF ==="
# Look for bytecode offsets in the SARIF file (ctadl uses byteOffset, not bytecodeOffset)
OFFSETS=$(grep -o '"byteOffset": *[0-9]*' "$SARIF_FILE" | grep -o '[0-9]*' | sort -u)

if [ ${#EXPECTED_LINES[@]} -eq 0 ]; then
    if [ -z "$OFFSETS" ]; then
        echo "=== SUCCESS: No flows found, as expected for negative test ==="
        exit 0
    else
        echo "=== FAILURE: Found unexpected bytecode offsets in negative test: $OFFSETS ==="
        exit 1
    fi
else
    if [ -z "$OFFSETS" ]; then
        echo "No bytecode offsets found in SARIF output"
        exit 1
    fi
fi

echo "Found bytecode offsets: $OFFSETS"

# Convert OFFSETS to an array for proper iteration
OFFSET_ARRAY=($OFFSETS)

# Map bytecode offsets to source lines using linemap
echo "=== Mapping bytecode offsets to source lines ==="
MAPPED_LINES=()

# Sort linemap by dex_offset for binary search or just proper iteration
# Since the linemap is small, we can just iterate and find the best match
for OFFSET in "${OFFSET_ARRAY[@]}"; do
    # Find the largest dex_offset in linemap that is <= OFFSET
    # We'll use python to do this cleanly if available, or just bash
    BEST_MATCH=$(jq -r ".[] | select(.dex_offset <= $OFFSET) | .dex_offset" "$LINEMAP_FILE" | sort -n | tail -n 1)
    
    if [ -n "$BEST_MATCH" ]; then
        LINE_ENTRY=$(jq -c ".[] | select(.dex_offset == $BEST_MATCH)" "$LINEMAP_FILE" | head -n 1)
        LINE=$(echo "$LINE_ENTRY" | jq -r '.line')
        METHOD=$(echo "$LINE_ENTRY" | jq -r '.method')
        echo "Offset $OFFSET (mapped from $BEST_MATCH) -> Line $LINE in $METHOD"
        MAPPED_LINES+=("$LINE")
    else
        echo "No linemap entry found for offset $OFFSET"
    fi
done

echo "=== Verifying expected lines ==="
FOUND_EXPECTED=false

for EXPECTED_LINE in "${EXPECTED_LINES[@]}"; do
    if [[ " ${MAPPED_LINES[@]} " =~ " $EXPECTED_LINE " ]]; then
        echo "✓ Found expected line $EXPECTED_LINE"
        FOUND_EXPECTED=true
    else
        echo "✗ Missing expected line $EXPECTED_LINE"
    fi
done

if [ "$FOUND_EXPECTED" = true ]; then
    echo "=== SUCCESS: Expected lines found in analysis results ==="
    exit 0
else
    echo "=== FAILURE: Expected lines not found in analysis results ==="
    exit 1
fi
