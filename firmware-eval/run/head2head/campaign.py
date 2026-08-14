#!/usr/bin/env python3
"""Run both engines over the whole corpus, one job at a time - DO-NOT-MERGE.

    source env.sh && python3 campaign.py [--force] [--engine rs|souffle]

Sequential on purpose: wall time and peak memory are recorded per phase, and
running jobs concurrently on a shared machine would make both meaningless.
Resumable - a job whose results/<label>/<engine>.json already exists is skipped
unless --force is passed.

Jobs run smallest binary first. At 50 binaries the tail is where a run can
stall, and going up the size curve means the corpus is mostly measured before
anything expensive is attempted; a partial campaign is still a usable result.
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main():
    force = "--force" in sys.argv
    only = None
    if "--engine" in sys.argv:
        only = sys.argv[sys.argv.index("--engine") + 1]

    corpus = json.loads((HERE / "corpus.json").read_text())["corpus"]
    corpus.sort(key=lambda c: c.get("size") or os.path.getsize(c["binary"]))
    engines = [only] if only else ["rs", "souffle"]
    jobs = [(e, c) for c in corpus for e in engines]

    t0 = time.time()
    ran = 0
    for i, (engine, entry) in enumerate(jobs, 1):
        label = entry["label"]
        done = HERE / "results" / label / f"{engine}.json"
        if done.exists() and not force:
            print(f"[{i}/{len(jobs)}] SKIP {label}/{engine} (done)", flush=True)
            continue
        el = time.time() - t0
        eta = f", eta {(el / ran) * (len(jobs) - i + 1) / 60:.0f} min" if ran else ""
        print(
            f"[{i}/{len(jobs)}] {label}/{engine}  "
            f"({entry.get('size', 0) // 1024}K, {el / 60:.0f} min elapsed{eta})",
            flush=True,
        )
        subprocess.run(
            [sys.executable, str(HERE / "run_one.py"), engine, label, entry["binary"]],
            check=False,
        )
        ran += 1
    print(f"campaign done in {(time.time() - t0) / 60:.1f} min", flush=True)


if __name__ == "__main__":
    main()
