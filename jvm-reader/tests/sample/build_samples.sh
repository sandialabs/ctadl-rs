#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TESTS_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUT_DIR="${SCRIPT_DIR}/out"
JAR_DIR="${TESTS_DIR}/jar"
CLASS_DIR="${TESTS_DIR}/class"

mkdir -p "${OUT_DIR}" "${JAR_DIR}" "${CLASS_DIR}"

if ! command -v javac >/dev/null 2>&1; then
  echo "error: javac not found in PATH" >&2
  exit 1
fi
if ! command -v jar >/dev/null 2>&1; then
  echo "error: jar not found in PATH" >&2
  exit 1
fi

shopt -s nullglob
java_files=("${SCRIPT_DIR}"/*.java)
if [ "${#java_files[@]}" -eq 0 ]; then
  echo "error: no .java files found in ${SCRIPT_DIR}" >&2
  exit 1
fi

for java_file in "${java_files[@]}"; do
  base_name="$(basename "${java_file}" .java)"
  sample_out="${OUT_DIR}/${base_name}"
  rm -rf "${sample_out}"
  mkdir -p "${sample_out}"

  echo "Compiling ${base_name}.java"
  javac -d "${sample_out}" "${java_file}"

  jar_path="${JAR_DIR}/${base_name}.jar"
  class_path="${CLASS_DIR}/${base_name}.class"

  echo "Creating ${jar_path}"
  jar cf "${jar_path}" -C "${sample_out}" .

  compiled_class="$(find "${sample_out}" -type f -name "${base_name}.class" | head -n 1 || true)"
  if [ -z "${compiled_class}" ]; then
    echo "error: could not locate ${base_name}.class under ${sample_out}" >&2
    exit 1
  fi
  echo "Copying ${compiled_class} -> ${class_path}"
  cp -f "${compiled_class}" "${class_path}"
done

echo "Done. Built ${#java_files[@]} sample(s)."
