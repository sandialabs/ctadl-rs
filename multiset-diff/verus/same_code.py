"""Check that the Verus copy of the merge is the crate's merge.

Usage: same_code.py <lib.rs> <multiset_diff.rs>

Takes `fn next` from each file, removes from the Verus copy what Verus adds (proof blocks,
ghost lets, asserts, loop invariants and decreases clauses), drops comments and whitespace
from both, and fails unless what is left is the same.
"""

import re
import sys


def body(src, name):
    start = src.index("fn next(&mut self)")
    open_brace = src.index("{", start)
    depth = 0
    for k in range(open_brace, len(src)):
        if src[k] == "{":
            depth += 1
        elif src[k] == "}":
            depth -= 1
            if depth == 0:
                return src[open_brace : k + 1]
    sys.exit(f"{name}: unbalanced braces in fn next")


def drop_balanced(text, opener, open_ch, close_ch, through=""):
    """Removes each `opener ...` whose `open_ch` is balanced, plus a trailing `through`."""
    out = []
    k = 0
    while True:
        m = re.compile(opener).search(text, k)
        if not m:
            out.append(text[k:])
            return "".join(out)
        out.append(text[k : m.start()])
        depth = 0
        e = text.index(open_ch, m.start())
        while True:
            if text[e] == open_ch:
                depth += 1
            elif text[e] == close_ch:
                depth -= 1
                if depth == 0:
                    break
            e += 1
        e += 1
        if through and text[e:].lstrip().startswith(through):
            e = text.index(through, e) + len(through)
        k = e


def strip_comments(text):
    return re.sub(r"//[^\n]*", "", text)


def tokens(text):
    # rustfmt adds trailing commas that verusfmt does not.
    return re.findall(r"\w+|[^\s\w]", re.sub(r",(\s*[)}\]])", r"\1", text))


lib, verus = (open(p).read() for p in sys.argv[1:3])
plain = body(strip_comments(lib), sys.argv[1])
ghost = body(strip_comments(verus), sys.argv[2])
ghost = drop_balanced(ghost, r"\bproof\s*\{", "{", "}")
ghost = drop_balanced(ghost, r"\bassert\s*\(", "(", ")", through=";")
ghost = re.sub(r"\blet\s+ghost\b[^;]*;", "", ghost)
# A loop header's clauses run from `invariant` to the loop body's `{`.
ghost = re.sub(r"\binvariant\b[^{]*", "", ghost)

if tokens(plain) != tokens(ghost):
    a, b = tokens(plain), tokens(ghost)
    k = next((k for k in range(min(len(a), len(b))) if a[k] != b[k]), min(len(a), len(b)))
    sys.exit(
        "the Verus copy of `next` differs from the crate's at token "
        f"{k}:\n  crate: {' '.join(a[max(0, k - 8) : k + 8])}\n  verus: {' '.join(b[max(0, k - 8) : k + 8])}"
    )
print("the Verus copy of `next` is the crate's, less its proof")
