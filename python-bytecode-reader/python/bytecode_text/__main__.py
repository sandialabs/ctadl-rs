"""CLI: ``python -m bytecode_text <path> --format stable|json [--stdin]``.

Reads ``.py`` source (compiled with ``compile``) or ``.pyc`` (unmarshalled), then
writes the stable text or JSON oracle to **stdout**. Exits non-zero with a
diagnostic on stderr on any failure.
"""

import argparse
import marshal
import sys
import types

from .collect import collect_file
from .serialize import to_stable, to_json

# The .pyc header is 16 bytes since CPython 3.7 (magic + bit field + source
# mtime/size or hash), followed by the marshalled code object.
_PYC_HEADER_SIZE = 16


def _load_code(path, use_stdin):
    if use_stdin:
        source = sys.stdin.buffer.read()
        return compile(source, path or "<stdin>", "exec")
    if path.endswith(".pyc"):
        with open(path, "rb") as f:
            data = f.read()
        code = marshal.loads(data[_PYC_HEADER_SIZE:])
        if not isinstance(code, types.CodeType):
            raise ValueError("marshalled object is not a code object: %s" % path)
        return code
    with open(path, "rb") as f:
        source = f.read()
    return compile(source, path, "exec")


def main(argv=None):
    parser = argparse.ArgumentParser(prog="bytecode_text")
    parser.add_argument("path", nargs="?", help="path to a .py or .pyc file")
    parser.add_argument(
        "--format", choices=("stable", "json"), default="stable",
        help="output format (default: stable)",
    )
    parser.add_argument(
        "--stdin", action="store_true",
        help="read source from stdin instead of `path`",
    )
    args = parser.parse_args(argv)

    if not args.stdin and not args.path:
        parser.error("a path is required unless --stdin is given")

    try:
        code = _load_code(args.path, args.stdin)
    except (OSError, ValueError, SyntaxError) as e:
        print("error: failed to load %s: %s" % (args.path, e), file=sys.stderr)
        return 1

    try:
        bytecode_file = collect_file(code)
        if args.format == "stable":
            sys.stdout.write(to_stable(bytecode_file))
        else:
            sys.stdout.write(to_json(bytecode_file))
    except Exception as e:  # noqa: BLE001 - report any serialization failure cleanly
        print("error: failed to serialize %s: %s" % (args.path, e), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
