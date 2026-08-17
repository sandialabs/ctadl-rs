#!/usr/bin/env python3
"""Pick the 100 firmware binaries for the hybrid-locals data-structure experiment.

Selection rule (mechanical; this docstring IS the rule):

  Population. The 32,311 unique, sink-bearing ELF executables of the Operation
  Mango `large_dataset` corpus, as enumerated by
  `../../../..//ct-head-to-head-firmware/firmware-eval/run/large_scale/scan_pop.py`
  and already profiled once by that campaign (one `ctadl go` per binary, 600 s /
  24 GB guards). Those per-binary profiles are what we sample from, so the corpus
  is chosen by *cost*, not by findings.

  Filters.
    * status in {ok, no_findings} -- the binary completes under the old guards, so
      both conditions have a fair chance of finishing here too.
    * peak_fp_mb >= 300 -- excludes runs that never got off the ground.
    * wall_s <= 300 outside the top stratum -- bounds the campaign. The top stratum
      takes anything that finished (<= 600 s), because that is where the heaviest
      Datalog phases live and they are the point of the experiment.
    * the binary path still exists on disk.
    * at most 2 binaries per basename per stratum, so the corpus is not 30 `pppd`s.

  Strata. Four bins on the previously measured `go` peak footprint, weighted toward
  the heavy end: 300M-1.2G (15), 1.2-2G (25), 2-5G (30), 5G+ (30).

  Why that weighting, and why peak footprint. A 15-binary pilot (`pilot_corpus.json`,
  `pilot/`) timed the *index phase alone* across the whole `go`-cost range and
  showed what the composite `go` number hides: for a small firmware binary, `ctadl
  go` is almost entirely the Ghidra lift. Below ~1.2 GB of `go` peak, the index
  phase runs in 0.01-0.1 s and peaks at a few MB -- there is no data structure
  question there. `go` peak footprint (not `go` wall, which is mostly lift, and not
  binary size) is the usable predictor of index work: `go` peak 1.3 GB -> ~4.6 s /
  227 MB of index; 4.7 GB -> ~14 s / 1.2 GB; 8.5 GB -> ~34 s / 2.3 GB.

  The light stratum is kept, at reduced weight, deliberately: "the majority of real
  firmware binaries have a sub-second index phase where this choice cannot matter"
  is a finding, and dropping the stratum would hide it.

  Sampling inside a stratum is deterministic: sort by sha256, take every
  len(bin)//25-th element. No RNG, no seed to remember.
"""
import json
import os
import glob
from pathlib import Path

HERE = Path(__file__).resolve().parent
PRIOR = Path(
    "/Users/dbueno/proj/ct-head-to-head-firmware/firmware-eval/run/large_scale/campaign/results"
)
ATTRIB = Path(
    "/Users/dbueno/proj/ct-head-to-head-firmware/firmware-eval/run/large_scale/pop.json.attrib.json"
)

# (low_mb, high_mb, n_binaries, max_go_wall_s)
BINS = [
    (300, 1200, 15, 300),
    (1200, 2000, 25, 300),
    (2000, 5000, 30, 300),
    (5000, float("inf"), 30, 600),
]
MAX_PER_NAME = 2


def main():
    rows = []
    for f in glob.glob(str(PRIOR / "*.json")):
        try:
            d = json.load(open(f))
        except Exception:
            continue
        if d.get("status") not in ("ok", "no_findings"):
            continue
        if d.get("peak_fp_mb", 0) < 300:
            continue
        if not os.path.exists(d["binary"]):
            continue
        rows.append(d)

    attrib = {}
    if ATTRIB.exists():
        try:
            attrib = json.load(open(ATTRIB))
        except Exception:
            attrib = {}

    corpus = []
    strata_stats = []
    for lo, hi, n_want, max_wall in BINS:
        pool = [
            r
            for r in rows
            if lo <= r["peak_fp_mb"] < hi and r.get("wall_s", 0) <= max_wall
        ]
        pool.sort(key=lambda r: r["sha256"])
        picked, seen_names = [], {}
        # deterministic stride so a stratum is sampled across its whole range
        stride = max(1, len(pool) // (n_want * 3))
        for start in range(stride):
            for r in pool[start::stride]:
                name = os.path.basename(r["binary"])
                if seen_names.get(name, 0) >= MAX_PER_NAME:
                    continue
                seen_names[name] = seen_names.get(name, 0) + 1
                picked.append(r)
                if len(picked) == n_want:
                    break
            if len(picked) == n_want:
                break
        strata_stats.append(
            {
                "bin_mb": [lo, hi if hi != float("inf") else None],
                "max_go_wall_s": max_wall,
                "pool": len(pool),
                "picked": len(picked),
            }
        )
        for r in picked:
            sha = r["sha256"]
            images = attrib.get(sha, [])
            vendor = ""
            if images:
                first = images[0] if isinstance(images[0], str) else str(images[0])
                vendor = first.split("/")[0]
            corpus.append(
                {
                    "sha256": sha,
                    "label": f"{os.path.basename(r['binary'])}_{sha[:8]}",
                    "name": os.path.basename(r["binary"]),
                    "vendor": vendor,
                    "binary": r["binary"],
                    "size": os.path.getsize(r["binary"]),
                    "bin_mb": [lo, hi if hi != float("inf") else None],
                    "prior_go_wall_s": r.get("wall_s"),
                    "prior_go_peak_fp_mb": r.get("peak_fp_mb"),
                    "prior_status": r.get("status"),
                    "prior_nfind": r.get("nfind"),
                }
            )

    out = {
        "_comment": [
            "The 100 firmware binaries for the hybrid-locals data-structure experiment.",
            "GENERATED by select_corpus.py; edit that, not this. Its docstring is the rule.",
        ],
        "strata": strata_stats,
        "corpus": corpus,
    }
    (HERE / "corpus.json").write_text(json.dumps(out, indent=1))
    print(f"wrote corpus.json: {len(corpus)} binaries")
    for s in strata_stats:
        print("  ", s)


if __name__ == "__main__":
    main()
