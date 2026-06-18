#!/usr/bin/env python3
"""
Print Parquet schemas (column name + datatype) for each Parquet file in a directory.

Requires: pyarrow
  pip install pyarrow
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import pyarrow.parquet as pq


def iter_parquet_files(root: Path, recursive: bool) -> list[Path]:
    if recursive:
        return sorted(p for p in root.rglob("*.parquet") if p.is_file())
    return sorted(p for p in root.glob("*.parquet") if p.is_file())


def print_schema(parquet_path: Path) -> None:
    schema = pq.read_schema(parquet_path)  # reads metadata only (no table load)
    print(f"\n== {parquet_path.name} ==")
    print(f"Path: {parquet_path}")
    for field in schema:
        # field.type is a pyarrow DataType; str(field.type) is a readable representation
        print(f"{field.name}: {field.type}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Print schema (column name + datatype) for each Parquet file in a directory."
    )
    parser.add_argument("directory", type=Path, help="Directory containing *.parquet files")
    parser.add_argument(
        "-r", "--recursive", action="store_true", help="Search for Parquet files recursively"
    )
    args = parser.parse_args(argv)

    root = args.directory
    if not root.exists() or not root.is_dir():
        print(f"Error: '{root}' is not a directory.", file=sys.stderr)
        return 2

    files = iter_parquet_files(root, args.recursive)
    if not files:
        print(f"No .parquet files found in: {root}", file=sys.stderr)
        return 1

    failures = 0
    for p in files:
        try:
            print_schema(p)
        except Exception as e:
            failures += 1
            print(f"\n== {p.name} ==\nPath: {p}\nERROR: {e}", file=sys.stderr)

    return 0 if failures == 0 else 3


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
