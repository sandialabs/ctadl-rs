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


def quantiles(xs):
    """min / q1 / median / q3 / max of a list, without numpy."""
    s = sorted(xs)
    if not s:
        return None

    def q(p):
        if len(s) == 1:
            return s[0]
        i = p * (len(s) - 1)
        lo = int(i)
        hi = min(lo + 1, len(s) - 1)
        return s[lo] + (s[hi] - s[lo]) * (i - lo)

    return {"min": s[0], "q1": q(0.25), "median": q(0.5), "q3": q(0.75), "max": s[-1]}


def summarize(rows):
    """Corpus-level summary. At n=50 this, not the per-binary table, is the result.

    Totals answer "what does analyzing this corpus cost / find", and per-binary
    ratio quantiles answer the question totals cannot: is the aggregate carried
    by one outlier binary, or does it hold binary by binary? Ratios are only
    computed where both engines completed and the denominator is nonzero -
    a ratio against zero is not a number, and the zero cases are counted
    separately instead.
    """
    ok = [r for r in rows if _done(r, "rs") and _done(r, "souffle")]
    out = {
        "binaries_in_corpus": len(rows),
        "binaries_both_engines_ok": len(ok),
        "incomplete": [
            {
                "label": r["label"],
                "rs": (r.get("rs") or {}).get("status", "not run"),
                "souffle": (r.get("souffle") or {}).get("status", "not run"),
            }
            for r in rows
            if not (_done(r, "rs") and _done(r, "souffle"))
        ],
        # An engine that cannot finish a binary is a result about that engine,
        # not a gap in the data. Counted per engine and per phase so a crash is
        # never quietly absorbed into "n = whatever completed".
        "failures": {
            engine: sorted(
                f"{r['label']}: {(r.get(engine) or {}).get('status', 'not run')}"
                for r in rows
                if not _done(r, engine)
            )
            for engine in ("souffle", "rs")
        },
        "totals": {},
        "ratios": {},
        "paths": {},
        "containment": {},
    }

    for engine in ("souffle", "rs"):
        out["totals"][engine] = {
            k: sum((r[engine].get(k) or 0) for r in ok)
            for k in ("import_bytes", "index_bytes", "summaries", "sarif_paths")
        }
        out["totals"][engine]["wall_s"] = round(
            sum(sum(v or 0 for v in r[engine]["wall_s"].values()) for r in ok), 1
        )
        out["paths"][engine] = {
            "binaries_with_a_path": sum(1 for r in ok if r[engine]["sarif_paths"] > 0),
            # binaries where the engine bound no sink at all, so zero paths is
            # a statement about the program, not about the analysis
            "binaries_with_no_endpoint": sum(1 for r in ok if r[engine]["no_endpoints"]),
            "sinks_reached": sorted({s for r in ok for s in r[engine]["sink_funcs"]}),
            "sources_used": sorted({s for r in ok for s in r[engine]["source_funcs"]}),
        }

    # old / new for cost (smaller is better), new / old for yield (bigger is better)
    for name, key, num, den in (
        ("import_old_over_new", "import_bytes", "souffle", "rs"),
        ("index_old_over_new", "index_bytes", "souffle", "rs"),
        ("summaries_new_over_old", "summaries", "rs", "souffle"),
    ):
        vals = [
            r[num][key] / r[den][key]
            for r in ok
            if r[num].get(key) and r[den].get(key)
        ]
        out["ratios"][name] = {"n": len(vals), **(quantiles(vals) or {})}

    # paths need their own treatment: zeros are common and a ratio would divide
    # by them, so the comparison is stated as wins rather than as a ratio
    out["ratios"]["paths_wins"] = {
        "new_more": sum(1 for r in ok if r["rs"]["sarif_paths"] > r["souffle"]["sarif_paths"]),
        "old_more": sum(1 for r in ok if r["souffle"]["sarif_paths"] > r["rs"]["sarif_paths"]),
        "tied": sum(1 for r in ok if r["souffle"]["sarif_paths"] == r["rs"]["sarif_paths"]),
        "tied_at_zero": sum(
            1 for r in ok
            if r["souffle"]["sarif_paths"] == r["rs"]["sarif_paths"] == 0
        ),
    }

    pc = [r["pair_comparison"] for r in ok if "pair_comparison" in r]
    out["containment"] = {
        "pairs_old_total": sum(len(p["old"]) for p in pc),
        "pairs_new_total": sum(len(p["new"]) for p in pc),
        "pairs_both_total": sum(len(p["both"]) for p in pc),
        "pairs_old_only_total": sum(len(p["old_only"]) for p in pc),
        "pairs_new_only_total": sum(len(p["new_only"]) for p in pc),
        "binaries_with_an_old_only_pair": sum(1 for p in pc if p["old_only"]),
        "old_only_pairs": sorted({q for p in pc for q in p["old_only"]}),
    }
    return out


def _done(row, engine):
    r = row.get(engine)
    return bool(r) and r.get("status") == "ok"


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
                # the query ran, wrote a SARIF, and reported that no configured
                # sink matched the program. ctadl-rs exits nonzero on this and
                # ctadl-souffle exits 0; run_one.py normalizes both to a valid
                # zero-path measurement and sets this flag. See run_one.settle_query.
                "no_endpoints": bool(
                    job["phases"].get("query", {}).get("no_endpoints")
                ),
                "sink_funcs": sorted({p["sink_func"] for p in paths}),
                "source_funcs": sorted({p["source_func"] for p in paths}),
                "labels": sorted({l for p in paths for l in p["labels"]}),
                "wall_s": {k: v.get("wall_s") for k, v in job["phases"].items()},
                "peak_fp_b": {k: v.get("peak_fp_b") for k, v in job["phases"].items()},
                "path_records": paths,
            }
        agg["corpus"].append(row)

    RESULTS.mkdir(exist_ok=True)

    # the endpoint-pair comparison, per binary; the summary counts over it
    for row in agg["corpus"]:
        old, new = row.get("souffle"), row.get("rs")
        if not (_done(row, "souffle") and _done(row, "rs")):
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
    agg["summary"] = summarize(agg["corpus"])
    s = agg["summary"]

    # ---- markdown ---------------------------------------------------------
    L = []
    L.append("# Head-to-head raw results - ctadl-souffle (old) vs ctadl-rs (new)")
    L.append("")
    L.append("Both engines: built-in defaults suppressed, same shared model set, same Ghidra.")
    L.append("")
    L.append(
        f"{s['binaries_both_engines_ok']} of {s['binaries_in_corpus']} binaries "
        "completed all three phases under both engines."
    )
    L.append("")
    L.append("## Corpus totals")
    L.append("")
    L.append("| | old (souffle) | new (rs) | old / new |")
    L.append("|---|--:|--:|--:|")
    to, tn = s["totals"]["souffle"], s["totals"]["rs"]
    for key, name, fmt in (
        ("import_bytes", "import size, total", human),
        ("index_bytes", "index size, total", human),
        ("summaries", "function summaries, total", lambda v: f"{v:,}"),
        ("sarif_paths", "SARIF taint paths, total", lambda v: f"{v:,}"),
        ("wall_s", "wall time, total", lambda v: f"{v / 60:.1f} min"),
    ):
        ratio = f"{to[key] / tn[key]:.2f}x" if tn[key] else "-"
        L.append(f"| {name} | {fmt(to[key])} | {fmt(tn[key])} | {ratio} |")
    L.append(
        f"| binaries with >=1 path | {s['paths']['souffle']['binaries_with_a_path']} "
        f"| {s['paths']['rs']['binaries_with_a_path']} | |"
    )
    L.append(
        f"| binaries where no sink bound | "
        f"{s['paths']['souffle']['binaries_with_no_endpoint']} | "
        f"{s['paths']['rs']['binaries_with_no_endpoint']} | |"
    )
    L.append("")
    L.append("## Per-binary spread")
    L.append("")
    L.append(
        "Totals can be carried by one large binary. These are the same "
        "comparisons computed per binary and then quantiled, so a claim that "
        "holds here holds binary by binary."
    )
    L.append("")
    L.append("| ratio | n | min | q1 | median | q3 | max |")
    L.append("|---|--:|--:|--:|--:|--:|--:|")
    for name, title in (
        ("import_old_over_new", "import size, old / new"),
        ("index_old_over_new", "index size, old / new"),
        ("summaries_new_over_old", "summaries, new / old"),
    ):
        q = s["ratios"][name]
        if not q.get("n"):
            continue
        L.append(
            f"| {title} | {q['n']} | {q['min']:.2f}x | {q['q1']:.2f}x "
            f"| **{q['median']:.2f}x** | {q['q3']:.2f}x | {q['max']:.2f}x |"
        )
    w = s["ratios"]["paths_wins"]
    L.append("")
    L.append(
        f"Paths, per binary: new reports more on **{w['new_more']}**, old reports "
        f"more on **{w['old_more']}**, tied on {w['tied']} "
        f"({w['tied_at_zero']} of those tied at zero). Stated as wins rather than "
        "a ratio because a path count of zero is common and cannot be a denominator."
    )
    c = s["containment"]
    L.append("")
    L.append(
        f"Endpoint pairs across the corpus: old {c['pairs_old_total']}, "
        f"new {c['pairs_new_total']}, reported by both {c['pairs_both_total']}, "
        f"**old only {c['pairs_old_only_total']}** (on "
        f"{c['binaries_with_an_old_only_pair']} binaries), new only "
        f"{c['pairs_new_only_total']}."
    )
    if s["incomplete"]:
        L.append("")
        L.append("## Jobs that did not complete")
        L.append("")
        L.append(
            f"Old engine failed on {len(s['failures']['souffle'])} of "
            f"{s['binaries_in_corpus']} binaries, new engine on "
            f"{len(s['failures']['rs'])}. A binary is only in the comparison "
            "above if BOTH engines finished it, so these are excluded from every "
            "number - which means the totals understate any gap a failure "
            "represents."
        )
        L.append("")
        L.append("| binary | old (souffle) | new (rs) |")
        L.append("|---|---|---|")
        for i in s["incomplete"]:
            L.append(f"| {i['label']} | {i['souffle']} | {i['rs']} |")
    L.append("")
    L.append("## Per binary")
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
        "intermediate vertex). `both` counts pairs reported by both engines. "
        "Binaries on which neither engine reported a path are omitted - the row "
        "would be all zeros; their count is in the summary above."
    )
    L.append("")
    L.append("| binary | pairs old | pairs new | both | old only | new only |")
    L.append("|---|--:|--:|--:|--:|--:|")
    for row in agg["corpus"]:
        pc = row.get("pair_comparison")
        if not pc:
            continue
        if not (pc["old"] or pc["new"]):
            continue  # neither engine reported anything; the row would be all zeros
        L.append(
            f"| {row['label']} | {len(pc['old'])} | {len(pc['new'])} "
            f"| {len(pc['both'])} | {len(pc['old_only'])} | {len(pc['new_only'])} |"
        )
    L.append("")
    L.append("## Endpoints of the reported paths")
    L.append("")
    L.append("Only engine/binary pairs that reported at least one path.")
    L.append("")
    L.append("| binary | engine | sinks reached | sources | taint labels |")
    L.append("|---|---|---|---|---|")
    for row in agg["corpus"]:
        for engine, name in (("souffle", "old"), ("rs", "new")):
            r = row.get(engine)
            if r is None or not r["sarif_paths"]:
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

    # the per-binary tables are 100+ rows; only the summary goes to the console
    print("\n".join(L[: L.index("## Per binary")]))
    print(f"\nwrote {RESULTS / 'aggregate.json'} and {RESULTS / 'TABLE.md'}")


if __name__ == "__main__":
    main()
