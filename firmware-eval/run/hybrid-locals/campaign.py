#!/usr/bin/env python3
"""Run the whole corpus, both conditions, one job at a time.

Sequential by design: these jobs peak in the tens of GB, and two of them sharing a
machine would contend for memory bandwidth and distort exactly the two numbers being
measured. Cheapest binary first, so a broken run shows up in minutes.

The two conditions for one binary run **back to back**, and their order alternates
binary by binary (even index: hybrid first; odd: control first). Adjacency keeps
machine drift -- thermal state, page cache, whatever else is on the box -- common to
both members of a pair; alternation keeps any residual first/second-run effect from
landing on one condition every time.

Resumable: a job with an existing `runs/results/<condition>/<label>.json` is skipped.
`--force` redoes them.

Usage: campaign.py [--force] [--outdir runs] [--only LABEL ...]
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main():
    argv = sys.argv[1:]
    force = "--force" in argv
    outdir = HERE / "runs"
    if "--outdir" in argv:
        outdir = Path(argv[argv.index("--outdir") + 1])
    only = []
    if "--only" in argv:
        only = argv[argv.index("--only") + 1 :]

    corpus = json.loads((HERE / "corpus.json").read_text())["corpus"]
    corpus.sort(key=lambda e: e["prior_go_wall_s"])
    if only:
        corpus = [e for e in corpus if e["label"] in only]

    outdir.mkdir(parents=True, exist_ok=True)
    logf = open(HERE / "campaign.log", "a")

    def say(msg):
        line = f"[{time.strftime('%H:%M:%S')}] {msg}"
        print(line, flush=True)
        logf.write(line + "\n")
        logf.flush()

    say(f"=== campaign start: {len(corpus)} binaries x 2 conditions -> {outdir}")
    t0 = time.time()
    for i, ent in enumerate(corpus):
        order = ["hybrid", "control"] if i % 2 == 0 else ["control", "hybrid"]
        for cond in order:
            res = outdir / "results" / cond / f"{ent['label']}.json"
            if res.exists() and not force:
                continue
            env = dict(os.environ)
            if force:
                env["FORCE"] = "1"
            p = subprocess.run(
                [sys.executable, str(HERE / "run_one.py"), cond, ent["label"], str(outdir)],
                env=env,
                capture_output=True,
                text=True,
            )
            out = (p.stdout or "").strip() or (p.stderr or "").strip()[-300:]
            say(f"{i + 1:3d}/{len(corpus)} {out}")
    say(f"=== campaign done in {(time.time() - t0) / 60:.1f} min")


if __name__ == "__main__":
    main()
