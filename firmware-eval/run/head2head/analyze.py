#!/usr/bin/env python3
"""Aggregate the campaign into one comparison table + raw record dump - DO-NOT-MERGE.

    python3 analyze.py

Reads results/<label>/{rs,souffle}.json and the SARIF each job produced, and
writes:

    results/aggregate.json   every metric, per binary, per engine, plus the
                             per-path records the path counts are derived from
    results/TABLE.md         the same thing as a table

The four measured quantities are import size, index size, summary count, and
SARIF path count. Paths are counted as `C0001` results - ctadl-rs spells the rule
`C0001.tainted-path` and ctadl-souffle spells it `C0001`, but they are the same
result class: one source-to-sink taint path.

Each path is also recorded with its endpoints (sink function, source function,
taint label) so the two engines' findings can be compared, not just counted.
"""

import json
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"

# `<EXTERNAL>::system@00008c84` -> `system`; a bare name is left alone.
NAME_RE = re.compile(r"(?:.*::)?([A-Za-z_][A-Za-z0-9_]*)(?:@.*)?$")


def short_name(fn: str) -> str:
    m = NAME_RE.match(fn or "")
    return m.group(1) if m else (fn or "?")


def is_path_result(r):
    """True for a real source-to-sink path result.

    ctadl-rs spells the rule `C0001.tainted-path`, ctadl-souffle spells it
    `C0001`; same result class. But when ctadl-rs finds NO flow it still emits
    one `C0001` result, `kind: open` / `level: none`, saying so - CTADL does not
    prove absence, so it reports "open" rather than "pass". Counting that as a
    finding would report 1 path for a binary with none.
    """
    if str(r.get("ruleId", "")).split(".")[0] != "C0001":
        return False
    return r.get("kind") != "open" and r.get("level") != "none"


def parse_paths(sarif_path: Path, engine: str):
    """Normalize one SARIF into per-path records comparable across engines."""
    try:
        data = json.loads(sarif_path.read_text())
    except Exception:
        return []
    recs = []
    for run in data.get("runs", []):
        for r in run.get("results", []):
            if not is_path_result(r):
                continue
            props = r.get("properties", {})
            msg = r.get("message", {}).get("text", "")

            if engine == "rs":
                sinks = props.get("sinkFunctions") or (
                    [props["sinkCallee"]] if props.get("sinkCallee") else []
                )
                srcs = props.get("sourceFunctions") or (
                    [props["sourceCallee"]] if props.get("sourceCallee") else []
                )
                sink = short_name(sinks[0]) if sinks else "?"
                src = short_name(srcs[0]) if srcs else "?"
            else:
                # ctadl-souffle states both endpoints in the result message:
                #   "Path starts in function 'F' label 'L', ends at 'S'"
                m = re.search(r"ends at '([^']+)'", msg)
                sink = short_name(m.group(1)) if m else "?"
                m = re.search(r"starts in function '([^']+)'", msg)
                src = short_name(m.group(1)) if m else "?"

            recs.append({
                "sink_func": sink,
                "source_func": src,
                "labels": props.get("taintLabels") or [],
            })
    return recs


def load_job(label, engine):
    p = RESULTS / label / f"{engine}.json"
    if not p.exists():
        return None
    job = json.loads(p.read_text())
    sarif = RESULTS / label / engine / "results.sarif"
    job["paths"] = parse_paths(sarif, engine) if sarif.exists() else []
    return job


def human(n):
    if n is None:
        return "-"
    for unit, div in (("G", 1024**3), ("M", 1024**2), ("K", 1024)):
        if n >= div:
            return f"{n / div:.1f}{unit}"
    return f"{n}B"


def main():
    corpus = json.loads((HERE / "corpus.json").read_text())["corpus"]
    agg = {"corpus": [], "controls": []}

    ctl = RESULTS / "controls.json"
    if ctl.exists():
        agg["controls"] = json.loads(ctl.read_text())

    for entry in corpus:
        label = entry["label"]
        row = {k: entry[k] for k in ("label", "vendor", "device", "arch", "binary")}
        for engine in ("rs", "souffle"):
            job = load_job(label, engine)
            if job is None:
                row[engine] = None
                continue
            paths = job.get("paths", [])
            row[engine] = {
                "status": job["status"],
                "binary_bytes": job["binary_bytes"],
                "import_bytes": job.get("import_bytes"),
                "index_bytes": job.get("index_bytes"),
                "summaries": job.get("summaries"),
                # recounted from the SARIF here, not taken from the job file, so
                # the number always matches the path records printed alongside it
                "sarif_paths": len(paths),
                "sink_funcs": sorted({p["sink_func"] for p in paths}),
                "source_funcs": sorted({p["source_func"] for p in paths}),
                "labels": sorted({l for p in paths for l in p["labels"]}),
                "wall_s": {k: v.get("wall_s") for k, v in job["phases"].items()},
                "peak_fp_b": {k: v.get("peak_fp_b") for k, v in job["phases"].items()},
                "path_records": paths,
            }
        agg["corpus"].append(row)

    RESULTS.mkdir(exist_ok=True)

    # ---- markdown ---------------------------------------------------------
    # (the pair comparison below is computed as the table is built, and lands in
    # aggregate.json too, so the JSON is written after this)
    L = []
    L.append("# Head-to-head raw results - ctadl-souffle (old) vs ctadl-rs (new)")
    L.append("")
    L.append("Both engines: built-in defaults suppressed, same shared model set, same Ghidra.")
    L.append("")
    L.append("| binary | vendor / arch | size | engine | import | index | summaries | SARIF paths | wall |")
    L.append("|---|---|--:|---|--:|--:|--:|--:|--:|")
    for row in agg["corpus"]:
        for engine, name in (("souffle", "old (souffle)"), ("rs", "new (rs)")):
            r = row.get(engine)
            if r is None:
                L.append(
                    f"| {row['label']} | {row['vendor']} / {row['arch']} | - | {name} "
                    f"| not run | | | | |"
                )
                continue
            wall = sum(v for v in r["wall_s"].values() if v)
            L.append(
                f"| {row['label']} | {row['vendor']} / {row['arch']} | {human(r['binary_bytes'])} "
                f"| {name} | {human(r['import_bytes'])} | {human(r['index_bytes'])} "
                f"| {r['summaries']} | {r['sarif_paths']} | {wall:.0f}s |"
            )
    L.append("")
    L.append("## Do the two engines find the same paths?")
    L.append("")
    L.append(
        "Paths are compared as `source -> sink` endpoint pairs, the coarsest join "
        "that is meaningful across engines (they do not agree on how to name an "
        "intermediate vertex). `both` counts pairs reported by both engines."
    )
    L.append("")
    L.append("| binary | pairs old | pairs new | both | old only | new only |")
    L.append("|---|--:|--:|--:|--:|--:|")
    for row in agg["corpus"]:
        old, new = row.get("souffle"), row.get("rs")
        if not old or not new:
            continue
        po = {(p["source_func"], p["sink_func"]) for p in old["path_records"]}
        pn = {(p["source_func"], p["sink_func"]) for p in new["path_records"]}
        row["pair_comparison"] = {
            "old": sorted("->".join(p) for p in po),
            "new": sorted("->".join(p) for p in pn),
            "both": sorted("->".join(p) for p in po & pn),
            "old_only": sorted("->".join(p) for p in po - pn),
            "new_only": sorted("->".join(p) for p in pn - po),
        }
        L.append(
            f"| {row['label']} | {len(po)} | {len(pn)} | {len(po & pn)} "
            f"| {len(po - pn)} | {len(pn - po)} |"
        )
    L.append("")
    L.append("## Endpoints of the reported paths")
    L.append("")
    L.append("| binary | engine | sinks reached | sources | taint labels |")
    L.append("|---|---|---|---|---|")
    for row in agg["corpus"]:
        for engine, name in (("souffle", "old"), ("rs", "new")):
            r = row.get(engine)
            if r is None:
                continue
            L.append(
                f"| {row['label']} | {name} | {', '.join(r['sink_funcs']) or '-'} "
                f"| {', '.join(r['source_funcs']) or '-'} | {', '.join(r['labels']) or '-'} |"
            )
    if agg["controls"]:
        L.append("")
        L.append("## Configuration control (Operation Mango synthetic binaries)")
        L.append("")
        L.append("Confirms neither engine was handed a model set it cannot use.")
        L.append("")
        L.append("| binary | old (souffle) paths | new (rs) paths |")
        L.append("|---|--:|--:|")
        for c in agg["controls"]:
            L.append(f"| {c['binary']} | {c['souffle_paths']} | {c['rs_paths']} |")
    (RESULTS / "TABLE.md").write_text("\n".join(L) + "\n")
    (RESULTS / "aggregate.json").write_text(json.dumps(agg, indent=2) + "\n")

    print("\n".join(L))
    print(f"\nwrote {RESULTS / 'aggregate.json'} and {RESULTS / 'TABLE.md'}")


if __name__ == "__main__":
    main()
