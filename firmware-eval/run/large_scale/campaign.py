#!/usr/bin/env python3
"""Resumable, parallel controller: run CTADL over the whole sink-binary worklist.

Skips binaries already done (results/<sha>.json present) -> fully resumable.
Bounded worker pool; each job self-guards on time+memory via run_one.py.
"""
import os, sys, json, time, subprocess
from pathlib import Path
from collections import Counter

HERE = Path(__file__).parent
WORKLIST = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE / "pop.json.worklist.json"
OUTDIR = Path(sys.argv[2]) if len(sys.argv) > 2 else HERE / "campaign"
NWORK = int(os.environ.get("NWORK", "12"))

def done_set(outdir):
    d = outdir / "results"
    if not d.exists():
        return set()
    return {p.stem for p in d.glob("*.json")}

def main():
    OUTDIR.mkdir(parents=True, exist_ok=True)
    wl = json.loads(WORKLIST.read_text())
    done = done_set(OUTDIR)
    todo = [w for w in wl if w["sha256"] not in done]
    total = len(wl)
    print(f"[campaign] worklist={total} done={len(done)} todo={len(todo)} workers={NWORK}", flush=True)

    procs = {}  # popen -> sha
    it = iter(todo)
    launched = 0
    finished = 0
    last_report = 0
    t0 = time.time()

    def launch(w):
        nonlocal launched
        cmd = [sys.executable, str(HERE / "run_one.py"), w["sha256"], w["binary"], str(OUTDIR)]
        p = subprocess.Popen(cmd)
        procs[p] = w["sha256"]
        launched += 1

    # prime the pool
    for _ in range(NWORK):
        w = next(it, None)
        if w is None:
            break
        launch(w)

    while procs:
        time.sleep(1.0)
        for p in list(procs):
            if p.poll() is not None:
                del procs[p]
                finished += 1
                w = next(it, None)
                if w is not None:
                    launch(w)
        if finished >= last_report + 25:
            last_report = finished
            el = time.time() - t0
            rate = finished / el if el else 0
            rem = (len(todo) - finished) / rate if rate else 0
            print(f"[campaign] {finished}/{len(todo)} this-run  "
                  f"elapsed={el/60:.1f}m rate={rate*60:.1f}/min  eta~{rem/3600:.1f}h", flush=True)

    print(f"[campaign] complete: {finished} run this session, total done={len(done_set(OUTDIR))}", flush=True)

if __name__ == "__main__":
    main()
