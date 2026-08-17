#!/usr/bin/env python3
"""Aggregate the paired runs into `runs/aggregate.json` + `runs/TABLE.md`.

Pairing is per binary: a binary contributes to the comparison only if BOTH conditions
produced a record. Binaries where the two conditions ended differently (one ok, one
timeout/oom) are reported separately -- those are the outcome differences, and folding
them into a ratio would either drop them silently or compare a finished run against a
killed one.

Summary statistics:
  * **geometric** mean of the per-binary ratio -- ratios are multiplicative, and one
    30x outlier would own an arithmetic mean of them.
  * corpus **totals** (sum of wall, sum of peak) -- what the whole suite costs.
  * the ratio **distribution** (min / quartiles / max) and the win/loss counts.

Safe to run mid-campaign.
"""
import json
import math
import statistics
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUNS = HERE / "runs"
CONDS = ("hybrid", "control")


def load(cond):
    out = {}
    d = RUNS / "results" / cond
    if not d.exists():
        return out
    for f in d.glob("*.json"):
        try:
            r = json.load(open(f))
        except Exception:
            continue
        out[r["label"]] = r
    return out


def geomean(xs):
    xs = [x for x in xs if x > 0]
    return math.exp(sum(math.log(x) for x in xs) / len(xs)) if xs else float("nan")


def q(xs, p):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    i = (len(xs) - 1) * p
    lo, hi = int(math.floor(i)), int(math.ceil(i))
    return xs[lo] if lo == hi else xs[lo] + (xs[hi] - xs[lo]) * (i - lo)


def main():
    H, C = load("hybrid"), load("control")
    corpus = {e["label"]: e for e in json.loads((HERE / "corpus.json").read_text())["corpus"]}
    # workload size from the separate unmeasured stats pass (see stats_pass.py)
    S = {}
    for f in (RUNS / "stats").glob("*.json"):
        try:
            S[f.stem] = json.load(open(f))
        except Exception:
            pass

    pairs, mismatch, incomplete = [], [], []
    for label in sorted(set(H) | set(C)):
        h, c = H.get(label), C.get(label)
        if not h or not c:
            incomplete.append({"label": label, "have": [k for k, v in (("hybrid", h), ("control", c)) if v]})
            continue
        rec = {
            "label": label,
            "name": corpus.get(label, {}).get("name", label),
            "size": corpus.get(label, {}).get("size"),
            "bin_mb": corpus.get(label, {}).get("bin_mb"),
            "import_wall_s": h.get("import", {}).get("wall_s"),
            "workload": S.get(label, {}),
            "hybrid": {k: h.get(k) for k in ("status", "wall_s", "peak_fp_mb", "stats", "index_store_bytes")},
            "control": {k: c.get(k) for k in ("status", "wall_s", "peak_fp_mb", "stats", "index_store_bytes")},
        }
        if h["status"] != "ok" or c["status"] != "ok":
            rec["kind"] = "outcome_differs" if h["status"] != c["status"] else "both_failed"
            mismatch.append(rec)
            continue
        rec["kind"] = "paired"
        rec["time_ratio"] = round(h["wall_s"] / c["wall_s"], 4) if c["wall_s"] > 0 else None
        rec["mem_ratio"] = round(h["peak_fp_mb"] / c["peak_fp_mb"], 4) if c["peak_fp_mb"] > 0 else None
        hs, cs = h.get("stats") or {}, c.get("stats") or {}
        rec["rows_agree"] = None
        if hs and cs:
            keys = [k for k in ("locals_rows", "assign_like_rows", "summary_rows") if k in hs and k in cs]
            rec["rows_agree"] = all(hs[k] == cs[k] for k in keys) if keys else None
            rec["row_deltas"] = {k: cs[k] - hs[k] for k in keys}

        # Equivalence: the two builds must produce the SAME index. Row counts come from
        # parquet metadata for every relation; the sorted-row sha is order-independent
        # and present for relations under the fingerprint row cap.
        hf, cf = h.get("fingerprint") or {}, c.get("fingerprint") or {}
        if hf and cf:
            rels = sorted(set(hf) | set(cf))
            bad = []
            n_sha = 0
            for r in rels:
                a, b = hf.get(r, {}), cf.get(r, {})
                if a.get("rows") != b.get("rows"):
                    bad.append(f"{r}: rows {a.get('rows')} vs {b.get('rows')}")
                elif "sha" in a and "sha" in b:
                    n_sha += 1
                    if a["sha"] != b["sha"]:
                        bad.append(f"{r}: content sha differs")
            rec["index_equivalent"] = not bad
            rec["index_relations_checked"] = len(rels)
            rec["index_relations_sha_checked"] = n_sha
            if bad:
                rec["index_diff"] = bad
        else:
            rec["index_equivalent"] = None
        pairs.append(rec)

    usable = [p for p in pairs if p["time_ratio"] and p["mem_ratio"]]
    tr = [p["time_ratio"] for p in usable]
    mr = [p["mem_ratio"] for p in usable]
    hw = sum(p["hybrid"]["wall_s"] for p in usable)
    cw = sum(p["control"]["wall_s"] for p in usable)
    hp = sum(p["hybrid"]["peak_fp_mb"] for p in usable)
    cp = sum(p["control"]["peak_fp_mb"] for p in usable)

    summ = {
        "n_paired_ok": len(usable),
        "n_outcome_differs": sum(1 for m in mismatch if m["kind"] == "outcome_differs"),
        "n_both_failed": sum(1 for m in mismatch if m["kind"] == "both_failed"),
        "n_incomplete": len(incomplete),
        "totals": {
            "hybrid_wall_s": round(hw, 1),
            "control_wall_s": round(cw, 1),
            "wall_ratio": round(hw / cw, 4) if cw else None,
            "hybrid_peak_sum_mb": round(hp, 1),
            "control_peak_sum_mb": round(cp, 1),
            "peak_sum_ratio": round(hp / cp, 4) if cp else None,
        },
        "time_ratio": {
            "geomean": round(geomean(tr), 4) if tr else None,
            "median": round(statistics.median(tr), 4) if tr else None,
            "min": round(min(tr), 4) if tr else None,
            "q25": round(q(tr, 0.25), 4) if tr else None,
            "q75": round(q(tr, 0.75), 4) if tr else None,
            "max": round(max(tr), 4) if tr else None,
            "hybrid_faster": sum(1 for x in tr if x < 1),
            "hybrid_slower": sum(1 for x in tr if x > 1),
        },
        "mem_ratio": {
            "geomean": round(geomean(mr), 4) if mr else None,
            "median": round(statistics.median(mr), 4) if mr else None,
            "min": round(min(mr), 4) if mr else None,
            "q25": round(q(mr, 0.25), 4) if mr else None,
            "q75": round(q(mr, 0.75), 4) if mr else None,
            "max": round(max(mr), 4) if mr else None,
            "hybrid_smaller": sum(1 for x in mr if x < 1),
            "hybrid_bigger": sum(1 for x in mr if x > 1),
        },
        "rows_agree_all": all(p.get("rows_agree") is not False for p in pairs),
        "rows_checked": sum(1 for p in pairs if p.get("rows_agree") is not None),
        "index_equivalent_all": all(p.get("index_equivalent") is not False for p in pairs),
        "index_equivalence_checked": sum(1 for p in pairs if p.get("index_equivalent") is not None),
        "index_equivalence_failed": [p["label"] for p in pairs if p.get("index_equivalent") is False],
    }

    # Ratios on a sub-second run are wakeup noise, not a measurement of anything. Report
    # the substantive subset alongside the whole corpus rather than silently dropping it.
    sub = [p for p in usable if p["control"]["wall_s"] >= 1.0]
    if sub:
        st = [p["time_ratio"] for p in sub]
        sm = [p["mem_ratio"] for p in sub]
        summ["substantive_only"] = {
            "threshold": "control index wall >= 1.0 s",
            "n": len(sub),
            "time_ratio_geomean": round(geomean(st), 4),
            "time_ratio_median": round(statistics.median(st), 4),
            "mem_ratio_geomean": round(geomean(sm), 4),
            "mem_ratio_median": round(statistics.median(sm), 4),
            "hybrid_faster": sum(1 for x in st if x < 1),
            "hybrid_smaller": sum(1 for x in sm if x < 1),
        }

    RUNS.mkdir(parents=True, exist_ok=True)
    (RUNS / "aggregate.json").write_text(
        json.dumps({"summary": summ, "pairs": pairs, "mismatch": mismatch, "incomplete": incomplete}, indent=1)
    )

    # ---- TABLE.md
    L = []
    L.append("# Hybrid locals data structure: raw results\n")
    L.append(f"Paired binaries (both conditions ok): **{summ['n_paired_ok']}**  ")
    L.append(f"Outcome differs: **{summ['n_outcome_differs']}**  ")
    L.append(f"Both failed: **{summ['n_both_failed']}**  ")
    L.append(f"Incomplete (one side missing): **{summ['n_incomplete']}**\n")
    L.append("Ratio = hybrid / control. <1 means the hybrid structure wins.\n")
    L.append(
        f"Index equivalence: **{'PASS' if summ['index_equivalent_all'] else 'FAIL'}** "
        f"({summ['index_equivalence_checked']} binaries checked relation-by-relation; "
        f"failures: {summ['index_equivalence_failed'] or 'none'})\n"
    )
    L.append("## Summary\n")
    L.append("| metric | geomean | median | q25 | q75 | min | max | hybrid wins |")
    L.append("|---|--:|--:|--:|--:|--:|--:|--:|")
    t, m = summ["time_ratio"], summ["mem_ratio"]
    L.append(
        f"| index wall | {t['geomean']} | {t['median']} | {t['q25']} | {t['q75']} | "
        f"{t['min']} | {t['max']} | {t['hybrid_faster']}/{summ['n_paired_ok']} |"
    )
    L.append(
        f"| peak footprint | {m['geomean']} | {m['median']} | {m['q25']} | {m['q75']} | "
        f"{m['min']} | {m['max']} | {m['hybrid_smaller']}/{summ['n_paired_ok']} |"
    )
    tot = summ["totals"]
    L.append("\n## Corpus totals\n")
    L.append("| | hybrid | control | ratio |")
    L.append("|---|--:|--:|--:|")
    L.append(f"| sum index wall (s) | {tot['hybrid_wall_s']} | {tot['control_wall_s']} | {tot['wall_ratio']} |")
    L.append(
        f"| sum peak footprint (MB) | {tot['hybrid_peak_sum_mb']} | {tot['control_peak_sum_mb']} | {tot['peak_sum_ratio']} |"
    )

    if mismatch:
        L.append("\n## Outcome differences\n")
        L.append("| binary | hybrid | control |")
        L.append("|---|---|---|")
        for r in mismatch:
            L.append(
                f"| `{r['label']}` | {r['hybrid']['status']} "
                f"({r['hybrid']['wall_s']:.0f}s / {r['hybrid']['peak_fp_mb']:.0f}MB) | "
                f"{r['control']['status']} ({r['control']['wall_s']:.0f}s / {r['control']['peak_fp_mb']:.0f}MB) |"
            )

    L.append("\n## Per binary\n")
    L.append(
        "| binary | size | hybrid wall (s) | control wall (s) | t-ratio | "
        "hybrid peak (MB) | control peak (MB) | m-ratio | locals rows |"
    )
    L.append("|---|--:|--:|--:|--:|--:|--:|--:|--:|")
    for p in sorted(usable, key=lambda r: -r["control"]["wall_s"]):
        rows = (p.get("workload") or {}).get("locals_rows", "")
        L.append(
            f"| `{p['label']}` | {p['size']} | {p['hybrid']['wall_s']:.2f} | {p['control']['wall_s']:.2f} | "
            f"{p['time_ratio']:.2f} | {p['hybrid']['peak_fp_mb']:.1f} | {p['control']['peak_fp_mb']:.1f} | "
            f"{p['mem_ratio']:.2f} | {rows} |"
        )
    (RUNS / "TABLE.md").write_text("\n".join(L) + "\n")

    # ---- raw.csv: one row per binary, flat, for a spreadsheet or a deck
    import csv

    with open(RUNS / "raw.csv", "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(
            [
                "label", "name", "binary_size_bytes", "stratum_go_peak_mb_lo", "stratum_go_peak_mb_hi",
                "import_wall_s", "hybrid_status", "control_status",
                "hybrid_index_wall_s", "control_index_wall_s", "time_ratio",
                "hybrid_peak_fp_mb", "control_peak_fp_mb", "mem_ratio",
                "index_equivalent", "locals_rows", "assign_like_rows", "formals", "num_functions",
            ]
        )
        for p in sorted(pairs + mismatch, key=lambda r: r["label"]):
            bm = p.get("bin_mb") or [None, None]
            w.writerow(
                [
                    p["label"], p.get("name"), p.get("size"), bm[0], bm[1],
                    p.get("import_wall_s"),
                    p["hybrid"]["status"], p["control"]["status"],
                    p["hybrid"]["wall_s"], p["control"]["wall_s"], p.get("time_ratio"),
                    p["hybrid"]["peak_fp_mb"], p["control"]["peak_fp_mb"], p.get("mem_ratio"),
                    p.get("index_equivalent"),
                    (p.get("workload") or {}).get("locals_rows"),
                    (p.get("workload") or {}).get("assign_like_rows"),
                    (p.get("workload") or {}).get("formals"),
                    (p.get("workload") or {}).get("num_functions"),
                ]
            )

    print(json.dumps(summ, indent=1))
    print(f"\nwrote {RUNS / 'aggregate.json'} and {RUNS / 'TABLE.md'}")


if __name__ == "__main__":
    main()
