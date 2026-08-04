#!/usr/bin/env python3
"""Generate synthetic Flowy (`.tnt`) programs that drive the `locals` store into a chosen shape.

`locals(FunctionId, FlowVariable, Path, FormalIndex, Path)` is stored by
`ctadl-ascent/src/index_engine/locals_trie.rs` as one group per `(F,V)` holding the
`(P, FormalIndex, Fp)` leaves. Group *size* is the shape parameter the whole module design
turns on (a linear-probing set under 64 leaves, a Swiss table above), so this generator
makes it a knob and scales it from 1 to tens of thousands while keeping the total row count
fixed -- which is what makes time/memory comparable across the sweep.

How the shape is produced (all through ordinary flow, no special casing in the engine):

  * The engine seeds `locals(f, ai, eps, i, eps)` for every formal `ai` of `f`, then
    propagates along assignments. So a variable that receives K distinct formals ends up
    with a `(F,V)` group of K leaves.
  * `--group-size K` therefore gives each function K parameters and funnels them into one
    variable: `hub = a0, a1, ..., a{K-1};` (Flowy's multi-source assign, one `assign_like`
    row per source).
  * `--paths D` spreads those K formals over D distinct *access paths* of one variable, by
    chunking them and storing each chunk into its own field: `obj.p0 = c0; obj.p1 = c1; ...`.
    The group for `obj` then holds K leaves over D distinct `P` values, which is what makes
    the `0_1_2` view's per-`P` filter non-trivial: a group is an unordered set, so that view
    scans the group rather than slicing a contiguous run.
  * `--funcs N` replicates the function N times to scale total rows independently of group
    size.

Each function contributes ~4 groups of size K (the hub, the object, the returned value and
the summary/return port) plus K singleton groups (one per formal, which reaches only
itself), so a run has both regimes present, with `max_group == K`.

Usage:
    scripts/gen-locals-bench.py --funcs 50 --group-size 8 --paths 2 -o bench.tnt
    scripts/gen-locals-bench.py --funcs 50 --group-size 8 --paths 2 --print-expected
"""

import argparse
import sys


def gen_function(name: str, group_size: int, paths: int) -> str:
    """One function whose `(F,V)` groups peak at `group_size` leaves over `paths` paths."""
    formals = [f"a{i}" for i in range(group_size)]
    lines = [f"def {name}({', '.join(formals)}) : 1 {{", "start:"]

    # Chunk the formals into `paths` field-sized groups. Chunk j funnels into `c{j}`, which is
    # stored at `obj.p{j}` -- so `obj`'s group carries `group_size` leaves over `paths` paths.
    chunks = [formals[j::paths] for j in range(paths)]
    for j, chunk in enumerate(chunks):
        if not chunk:
            continue
        lines.append(f"  c{j} = {', '.join(chunk)};")
        lines.append(f"  obj.p{j} = c{j};")

    # A whole-object copy, so the object's whole group propagates as one unit (this is the
    # `0_1` view's iteration path) and a second group of the same size exists.
    lines.append("  out = obj;")
    # Returning `out` exposes the group at the function's return port, which is what turns
    # into `summary` rows -- i.e. the group is also read by the summary rules, not just built.
    lines.append("  return out;")
    lines.append("}")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--funcs", type=int, default=1,
                    help="number of generated functions (scales total rows)")
    ap.add_argument("--group-size", type=int, default=8,
                    help="leaves in each function's largest (F,V) group")
    ap.add_argument("--paths", type=int, default=1,
                    help="distinct access paths (P values) those leaves spread over")
    ap.add_argument("-o", "--output", default="-", help="output .tnt file ('-' for stdout)")
    ap.add_argument("--print-expected", action="store_true",
                    help="print the expected store shape instead of the program")
    args = ap.parse_args()

    if args.group_size < 1 or args.funcs < 1 or args.paths < 1:
        ap.error("--funcs, --group-size and --paths must all be >= 1")
    if args.paths > args.group_size:
        ap.error("--paths must be <= --group-size (each path needs at least one formal)")

    if args.print_expected:
        # Per function: `group_size` singleton formal groups, plus the hub/obj/out/return-port
        # groups of `group_size` leaves each. Exact counts are reported by the run itself
        # (`locals store estimate: ... groups: max ...`); this is the design intent.
        print(f"funcs={args.funcs} group_size={args.group_size} paths={args.paths}")
        print(f"expected max_group={args.group_size}")
        print(f"expected promoted (Swiss) groups: "
              f"{'yes' if args.group_size > 64 else 'no'} (threshold 64)")
        return 0

    body = "\n\n".join(
        gen_function(f"bench{i}", args.group_size, args.paths) for i in range(args.funcs)
    )
    header = (
        f"// Generated by scripts/gen-locals-bench.py\n"
        f"// funcs={args.funcs} group_size={args.group_size} paths={args.paths}\n"
        f"// Drives the `locals` store to max_group={args.group_size} over {args.paths} path(s).\n\n"
    )
    text = header + body + "\n"
    if args.output == "-":
        sys.stdout.write(text)
    else:
        with open(args.output, "w") as f:
            f.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
