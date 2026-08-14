#!/usr/bin/env python3
"""Run both engines over the whole corpus, one job at a time - DO-NOT-MERGE.

    source env.sh && python3 campaign.py [--force]

Sequential on purpose: wall time and peak memory are recorded per phase, and
running jobs concurrently on a shared machine would make both meaningless.
Resumable - a job whose results/<label>/<engine>.json already exists is skipped
unless --force is passed.
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
    corpus = json.loads((HERE / "corpus.json").read_text())["corpus"]
    jobs = [(e, c) for c in corpus for e in ("rs", "souffle")]
    t0 = time.time()
    for i, (engine, entry) in enumerate(jobs, 1):
        label = entry["label"]
        done = HERE / "results" / label / f"{engine}.json"
        if done.exists() and not force:
            print(f"[{i}/{len(jobs)}] SKIP {label}/{engine} (done)", flush=True)
            continue
        print(f"[{i}/{len(jobs)}] {label}/{engine}  ({time.time() - t0:.0f}s elapsed)", flush=True)
        subprocess.run(
            [sys.executable, str(HERE / "run_one.py"), engine, label, entry["binary"]],
            check=False,
        )
    print(f"campaign done in {(time.time() - t0) / 60:.1f} min", flush=True)


if __name__ == "__main__":
    main()
