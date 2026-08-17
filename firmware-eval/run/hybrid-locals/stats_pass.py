#!/usr/bin/env python3
"""Record each benchmark's workload size: how many rows `locals` actually reaches.

`locals` is the relation under test but it is never persisted -- only its row count is,
and only at `debug` level. So this is a separate, UNMEASURED pass: re-index each binary
once with `RUST_LOG=ctadl_ascent::index_engine=debug` and keep the counts.

The counts are a property of the benchmark, not of the condition -- `analyze.py` has
already verified the two builds produce the same index -- so one build suffices, and
nothing here touches the timing numbers.

Writes `runs/stats/<label>.json`. Resumable. Usage: stats_pass.py [min_control_wall_s]
"""
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import run_one as R

HERE = Path(__file__).resolve().parent
RUNS = HERE / "runs"


def main():
    min_wall = float(sys.argv[1]) if len(sys.argv) > 1 else 1.0
    corpus = {e["label"]: e for e in json.loads((HERE / "corpus.json").read_text())["corpus"]}
    outdir = RUNS / "stats"
    outdir.mkdir(parents=True, exist_ok=True)

    todo = []
    for label in corpus:
        cf = RUNS / "results" / "control" / f"{label}.json"
        if not cf.exists():
            continue
        c = json.loads(cf.read_text())
        if c.get("status") != "ok" or c.get("wall_s", 0) < min_wall:
            continue
        if (outdir / f"{label}.json").exists():
            continue
        todo.append(label)
    print(f"{len(todo)} binaries need a stats run")

    env = dict(os.environ, GHIDRA_HOME=R.GHIDRA_HOME, RUST_LOG="ctadl_ascent::index_engine=debug")
    for i, label in enumerate(sorted(todo), 1):
        src = RUNS / "imports" / label / "s"
        work = Path(R.SCRATCH) / f"stats_{label}"
        if work.exists():
            shutil.rmtree(work, ignore_errors=True)
        shutil.copytree(src, work)
        logp = outdir / f"{label}.log"
        cmd = [R.CTADL["hybrid"], "--store", str(work), "index", label, "--models", R.MODEL]
        t0 = time.time()
        with open(logp, "wb") as fh:
            subprocess.run(cmd, stdout=fh, stderr=subprocess.STDOUT, env=env)
        st = R.parse_stats(logp)
        st["stats_run_wall_s"] = round(time.time() - t0, 2)
        (outdir / f"{label}.json").write_text(json.dumps(st, indent=1))
        shutil.rmtree(work, ignore_errors=True)
        print(f"{i:3d}/{len(todo)} {label:<40} locals={st.get('locals_rows')} assign_like={st.get('assign_like_rows')}")


if __name__ == "__main__":
    main()
