#!/usr/bin/env python3
"""Time/memory benchmark for the `locals` BYODS store, driven by generated Flowy programs.

Sweeps the one shape parameter `locals_trie` is designed around -- the number of leaves in a
`(F,V)` group -- from 1 to tens of thousands while holding the total row count roughly
constant, so time and bytes/row are comparable across the sweep. Programs come from
`scripts/gen-locals-bench.py`; each configuration is imported and indexed by the real `ctadl`
binary, so what is measured is the store as the index phase actually drives it.

What is reported, per configuration:

  * store bytes    -- `LocalsIndCommon::heap_report()` (logged at DEBUG by the index phase).
                      Cross-checked against a counting allocator by
                      `cargo bench -p ctadl-ascent --bench locals_trie`: accurate to <1% for
                      the whole store, so it is used here as the per-structure number that
                      process-level memory cannot provide.
  * fixpoint time  -- wall time of the SCC containing the `locals` rules, from Ascent's own
                      `#![measure_rule_times]` instrumentation. This excludes parsing, SSA
                      and codegen, which dominate the process at these sizes.
  * process memory -- physical footprint before/after the fixpoint (`[mem cp]` log lines),
                      plus the peak physical footprint and peak RSS of the whole index run.
                      Physical footprint, not RSS, is the real number on macOS (see
                      `.claude/skills/measure-process-memory`); RSS is kept only as a
                      cross-check.

Usage:
    scripts/locals-bench.py                        # default sweep
    scripts/locals-bench.py --rows 400000          # smaller/larger target row count
    scripts/locals-bench.py --group-sizes 1,64,65  # specific group sizes
    scripts/locals-bench.py --paths 4 --out results.tsv
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
GEN = REPO / "scripts" / "gen-locals-bench.py"

# Rows the generator produces per function: measured ~5K + 2 for group size K (K singleton
# formal groups, ~4 groups of K leaves, plus the return-port pair). Only used to pick
# `--funcs` so total rows stay near the target; the actual row count is reported per run.
def rows_per_func(group_size):
    return 5 * group_size + 2


def sh(cmd, env=None, cwd=None):
    """Run a command, returning (exit_code, combined_output, wall_secs, peak_fp_b, peak_rss_b).

    Peak physical footprint is sampled from `footprint(1)` while the child runs; peak RSS
    comes from the child's own rusage (`os.wait4`), so it needs no sampling at all.
    """
    peak_fp = 0
    stop = threading.Event()

    def poll(pid):
        nonlocal peak_fp
        while not stop.is_set():
            try:
                out = subprocess.run(
                    ["footprint", "-p", str(pid), "-f", "bytes"],
                    capture_output=True, text=True, timeout=10,
                ).stdout
            except (OSError, subprocess.SubprocessError):
                return
            for line in out.splitlines():
                if "phys_footprint:" in line:
                    fields = line.split()
                    # "phys_footprint: 1868136 B" -- the value is field 1, NOT the last field
                    # (which is the unit "B"). Reading $NF here silently yields 0.
                    if len(fields) >= 2 and fields[1].isdigit():
                        peak_fp = max(peak_fp, int(fields[1]))
                    break
            stop.wait(0.02)

    t0 = time.monotonic()
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, env=env, cwd=cwd)
    watcher = threading.Thread(target=poll, args=(proc.pid,), daemon=True)
    watcher.start()
    out = proc.stdout.read()
    _, status, rusage = os.wait4(proc.pid, 0)
    stop.set()
    watcher.join(timeout=1)
    wall = time.monotonic() - t0
    # macOS reports ru_maxrss in bytes (Linux: kilobytes).
    rss = rusage.ru_maxrss if sys.platform == "darwin" else rusage.ru_maxrss * 1024
    return os.waitstatus_to_exitcode(status), out, wall, peak_fp, rss


HEAP_RE = re.compile(
    r"locals store estimate: total ([\d.]+) MB over (\d+) rows \(([\d.]+) B/row\) \| "
    r"fwd ([\d.]+) MB \((\d+)%\): (\d+) \(F,V\) groups, (\d+) \(F,V,P\) entries, (\d+) leaves \| "
    r"fidx ([\d.]+) MB \((\d+)%\): (\d+) funcs, (\d+) V entries \| "
    r"groups: max (\d+), large (\d+), mean ([\d.]+), log2hist \[([^\]]*)\]"
)
# A build from before this branch (e.g. `main`) logs the same line without the `groups:` shape
# suffix, which `HeapReport` grew here. Parse it too, so `main` itself can be measured; note that
# its `hb_bytes` assumed hashbrown's minimum table was 8 buckets rather than 4, so it reports
# every table of <=3 elements at twice its real size.
LEGACY_HEAP_RE = re.compile(
    r"locals store estimate: total ([\d.]+) MB over (\d+) rows \(([\d.]+) B/row\) \| "
    r"fwd ([\d.]+) MB \((\d+)%\): (\d+) \(F,V\) groups, (\d+) \(F,V,P\) entries, (\d+) leaves \| "
    r"fidx ([\d.]+) MB \((\d+)%\): (\d+) funcs, (\d+) V entries"
)
INCREASE_RE = re.compile(r"relation increase: locals: (\d+),.*?reached \((\d+)/\d+\)")
SCC_RE = re.compile(r"scc (\d+): iterations: (\d+), time: ([\d.]+)(ns|µs|ms|s)\b")
MEMCP_BEFORE_RE = re.compile(r"about to enter ascent_run: ([\d.-]+) MB")
MEMCP_AFTER_RE = re.compile(r"ascent_run returned[^:]*: ([\d.-]+) MB")

UNIT = {"ns": 1e-9, "µs": 1e-6, "ms": 1e-3, "s": 1.0}


def parse_log(log):
    """Pull the store shape, the dominant SCC's time, and the fixpoint's memory delta.

    A build with the `#[ds(locals_trie)]` attribute removed -- the A/B baseline against Ascent's
    default relation storage -- logs no store estimate. Fall back to the row/variable counts
    from the stats line so the same harness measures both.
    """
    m = HEAP_RE.search(log)
    if m:
        g = m.groups()
        rec = {
            "store_mb": float(g[0]), "rows": int(g[1]), "b_per_row": float(g[2]),
            "fwd_mb": float(g[3]), "groups": int(g[5]), "p_entries": int(g[6]),
            "fidx_mb": float(g[8]), "funcs": int(g[10]),
            "max_group": int(g[12]), "large": int(g[13]), "mean_group": float(g[14]),
            "hist": g[15],
        }
    elif LEGACY_HEAP_RE.search(log):
        g = LEGACY_HEAP_RE.search(log).groups()
        rec = {
            "store_mb": float(g[0]), "rows": int(g[1]), "b_per_row": float(g[2]),
            "fwd_mb": float(g[3]), "groups": int(g[5]), "p_entries": int(g[6]),
            "fidx_mb": float(g[8]), "funcs": int(g[10]),
            "max_group": 0, "large": 0,
            "mean_group": int(g[7]) / int(g[5]) if int(g[5]) else 0.0, "hist": "",
        }
    else:
        m = INCREASE_RE.search(log)
        if not m:
            raise RuntimeError("no `locals store estimate` or `relation increase: locals` line "
                               "in index output (was the index run with the index_engine module at DEBUG?)")
        rows, groups = int(m.group(1)), int(m.group(2))
        rec = {
            "store_mb": 0.0, "rows": rows, "b_per_row": 0.0, "fwd_mb": 0.0,
            "groups": groups, "p_entries": 0, "fidx_mb": 0.0, "funcs": 0,
            "max_group": 0, "large": 0,
            "mean_group": rows / groups if groups else 0.0, "hist": "",
        }
    # The `locals` rules all live in one SCC; it is the dominant one by construction here, so
    # take the slowest SCC rather than hard-coding an index that shifts as rules change.
    sccs = [(float(t) * UNIT[u], int(it), int(n)) for n, it, t, u in SCC_RE.findall(log)]
    if sccs:
        secs, iters, scc = max(sccs)
        rec["fixpoint_s"], rec["iterations"], rec["scc"] = secs, iters, scc
        rec["all_scc_s"] = sum(s for s, _, _ in sccs)
    else:
        rec["fixpoint_s"] = rec["iterations"] = rec["scc"] = rec["all_scc_s"] = 0
    before = MEMCP_BEFORE_RE.search(log)
    after = MEMCP_AFTER_RE.search(log)
    rec["fp_before_mb"] = float(before.group(1)) if before else 0.0
    rec["fp_after_mb"] = float(after.group(1)) if after else 0.0
    rec["fixpoint_mb"] = rec["fp_after_mb"] - rec["fp_before_mb"]
    return rec


def run_config(ctadl, workdir, funcs, group_size, paths, keep):
    tnt = workdir / f"g{group_size}_p{paths}.tnt"
    store = workdir / f"store-g{group_size}-p{paths}"
    if store.exists():
        shutil.rmtree(store)
    subprocess.run([sys.executable, str(GEN), "--funcs", str(funcs),
                    "--group-size", str(group_size), "--paths", str(paths),
                    "-o", str(tnt)], check=True)

    env = dict(os.environ, RUST_LOG="error")
    rc, out, *_ = sh([ctadl, "--store", str(store), "import", str(tnt), "--name", "bench"],
                     env=env)
    if rc != 0:
        raise RuntimeError(f"import failed for group_size={group_size}:\n{out}")

    # The store estimate, the scc times and the `[mem cp]` lines all live in
    # `ctadl_ascent::index_engine` and moved from INFO to DEBUG in main `e27e1466`. Raise that
    # one module rather than the whole log, so no other module's DEBUG output lands in the
    # fixpoint's hot path and taxes the time being measured.
    env = dict(os.environ, RUST_LOG="info,ctadl_ascent::index_engine=debug")
    rc, out, wall, peak_fp, peak_rss = sh(
        [ctadl, "--store", str(store), "index", "bench"], env=env)
    if rc != 0:
        raise RuntimeError(f"index failed for group_size={group_size}:\n{out[-4000:]}")
    rec = parse_log(out)
    rec.update(target_group=group_size, gen_funcs=funcs, gen_paths=paths,
               index_wall_s=wall, peak_fp_mb=peak_fp / 2**20, peak_rss_mb=peak_rss / 2**20,
               tnt_bytes=tnt.stat().st_size)
    if not keep:
        shutil.rmtree(store, ignore_errors=True)
        tnt.unlink(missing_ok=True)
    return rec


COLS = [
    ("target_group", "group", "{}"),
    ("gen_funcs", "funcs", "{}"),
    ("rows", "rows", "{}"),
    ("groups", "groups", "{}"),
    ("max_group", "max", "{}"),
    ("mean_group", "mean", "{:.2f}"),
    ("large", "large", "{}"),
    ("p_entries", "(F,V,P)", "{}"),
    ("store_mb", "store MB", "{:.1f}"),
    ("b_per_row", "B/row", "{:.0f}"),
    ("fixpoint_s", "fixpoint s", "{:.3f}"),
    ("iterations", "iters", "{}"),
    ("fixpoint_mb", "fixpoint MB", "{:.1f}"),
    ("peak_fp_mb", "peak fp MB", "{:.1f}"),
    ("peak_rss_mb", "peak rss MB", "{:.1f}"),
    ("index_wall_s", "wall s", "{:.2f}"),
]


def print_table(recs):
    head = [h for _, h, _ in COLS]
    rows = [[f.format(r[k]) for k, _, f in COLS] for r in recs]
    width = [max(len(h), *(len(r[i]) for r in rows)) for i, h in enumerate(head)]
    print("| " + " | ".join(h.rjust(w) for h, w in zip(head, width)) + " |")
    print("|" + "|".join("-" * (w + 2) for w in width) + "|")
    for r in rows:
        print("| " + " | ".join(c.rjust(w) for c, w in zip(r, width)) + " |")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ctadl", default=str(REPO / "target" / "release" / "ctadl"),
                    help="ctadl binary to measure (build with: cargo build --release)")
    ap.add_argument("--rows", type=int, default=1_000_000,
                    help="approximate `locals` rows to hold constant across the sweep")
    ap.add_argument("--group-sizes", default="1,2,4,8,16,32,64,65,128,512,2048,8192",
                    help="comma-separated group sizes to sweep (65 straddles the "
                         "SMALL_THRESHOLD=64 promotion to a Swiss table)")
    ap.add_argument("--paths", type=int, default=1,
                    help="distinct access paths per group (secondary knob)")
    ap.add_argument("--workdir", default=None, help="where to put generated programs/stores")
    ap.add_argument("--keep", action="store_true", help="keep generated programs and stores")
    ap.add_argument("--out", default=None, help="also write results as TSV to this path")
    args = ap.parse_args()

    ctadl = args.ctadl
    if not os.access(ctadl, os.X_OK):
        sys.exit(f"{ctadl} is not executable; build it with `cargo build --release`")
    workdir = Path(args.workdir) if args.workdir else Path(
        os.environ.get("TMPDIR", "/tmp")) / "locals-bench"
    workdir.mkdir(parents=True, exist_ok=True)

    sizes = [int(s) for s in args.group_sizes.split(",") if s.strip()]
    recs = []
    for k in sizes:
        funcs = max(1, args.rows // rows_per_func(k))
        paths = min(args.paths, k)
        print(f"# group_size={k} funcs={funcs} paths={paths} ...", file=sys.stderr, flush=True)
        recs.append(run_config(ctadl, workdir, funcs, k, paths, args.keep))
        r = recs[-1]
        print(f"#   rows={r['rows']} max_group={r['max_group']} large={r['large']} "
              f"store={r['store_mb']:.1f}MB fixpoint={r['fixpoint_s']:.3f}s",
              file=sys.stderr, flush=True)

    print()
    print_table(recs)
    if args.out:
        keys = list(recs[0].keys())
        with open(args.out, "w") as f:
            f.write("\t".join(keys) + "\n")
            for r in recs:
                f.write("\t".join(str(r[k]) for k in keys) + "\n")
        print(f"\nwrote {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
