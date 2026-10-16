#!/usr/bin/env python3
"""Aggregate campaign results into a Table-6-shaped per-vendor findings comparison.

CTADL side is computed from campaign/results/*.json joined with the attribution map
(sha -> every firmware image shipping it), so shared binaries are counted per-image
exactly like Mango's Table 6. Mango side is the published Table 6.
Safe to run mid-campaign for a progress snapshot.
"""
import json, glob, sys, re
from pathlib import Path
from collections import defaultdict, Counter

def clean_sink(name):
    """Normalize '<EXTERNAL>::system@00408620' -> 'system'."""
    if not name:
        return name
    name = name.split("::")[-1]
    name = name.split("@")[0]
    return name

HERE = Path(__file__).parent
RESULTS = HERE / "campaign" / "results"
ATTRIB = json.loads((HERE / "pop.json.attrib.json").read_text())  # sha -> [[vendor,firm],...]
POP = json.loads((HERE / "pop.json").read_text())

# Published Operation Mango Table 6 (Large Scale Evaluation).
# vendor -> (samples, total_bins, trupocs, error, oom)
MANGO_T6 = {
    "netgear":  (305, 182600, 6716, 4240, 52),
    "asus":     (158, 104422,  635, 1174, 45),
    "belkin":   ( 62,  20018, 2102,  353, 12),
    "linksys":  ( 67,  46470,  211,  440,  8),
    "tplink":   (484, 239020,  166, 3339, 93),
    "trendnet": (178,  41878,   31, 5585,  3),
    "tenda":    (104,  29650,   56,  576,  9),
    "dlink":    (320,  95788,  907, 2626, 25),
    "ZyXEL":    ( 20,  10528,   10,  254,  2),
}

def main():
    res = {}
    for f in glob.glob(str(RESULTS / "*.json")):
        try:
            d = json.loads(Path(f).read_text())
        except Exception:
            continue
        res[d["sha256"]] = d

    # per-vendor firmware-x-binary rollup (attribute each analyzed sha to every image)
    V = defaultdict(lambda: {
        "fxb_analyzed": 0, "fxb_alerts": 0, "fxb_alertbins": 0,
        "uniq_analyzed": 0, "uniq_alertbins": 0,
        "status": Counter(), "sink": Counter(), "src": Counter(),
    })
    vendor_uniq_analyzed = defaultdict(set)
    total_sink_shas = len(ATTRIB)
    for sha, images in ATTRIB.items():
        r = res.get(sha)
        if r is None:
            continue  # not yet analyzed this session
        # vendor set for this sha
        for (vendor, firm) in images:
            V[vendor]["fxb_analyzed"] += 1
            V[vendor]["status"][r["status"]] += 1
            if r["nfind"] > 0:
                V[vendor]["fxb_alerts"] += r["nfind"]
                V[vendor]["fxb_alertbins"] += 1
            vendor_uniq_analyzed[vendor].add(sha)
        # per-unique sink/src breakdown (count once)
        for fd in r.get("findings", []):
            # attribute breakdown to each vendor shipping it
            pass
    # unique alert-bins per vendor
    vendor_uniq_alertbin = defaultdict(set)
    for sha, r in res.items():
        if r["nfind"] > 0:
            for (vendor, firm) in ATTRIB.get(sha, []):
                vendor_uniq_alertbin[vendor].add(sha)
    for v in V:
        V[v]["uniq_analyzed"] = len(vendor_uniq_analyzed[v])
        V[v]["uniq_alertbins"] = len(vendor_uniq_alertbin[v])

    analyzed = len(res)
    print(f"\n=== CAMPAIGN PROGRESS: {analyzed:,}/{total_sink_shas:,} unique sink-binaries "
          f"analyzed ({100*analyzed/total_sink_shas:.1f}%) ===\n")

    order = ["netgear","asus","belkin","linksys","tplink","trendnet","tenda","dlink","ZyXEL"]
    # findings-focused table
    hdr = (f"{'Vendor':9s} | {'imgs':>4s} {'uAnl':>6s} {'uAlrt':>5s} | "
           f"{'CTADL alerts':>12s} {'CTADLalrtBin':>12s} | {'Mango TruPoC':>12s} {'MangoErr':>8s} {'OOM':>5s} | "
           f"{'to':>4s} {'oom':>4s} {'crash':>5s}")
    print(hdr); print("-"*len(hdr))
    tot = defaultdict(int)
    for v in order:
        d = V.get(v, None)
        m = MANGO_T6[v]
        if d is None:
            print(f"{v:9s} | (no results yet)")
            continue
        st = d["status"]
        print(f"{v:9s} | {m[0]:>4d} {d['uniq_analyzed']:>6d} {d['uniq_alertbins']:>5d} | "
              f"{d['fxb_alerts']:>12,d} {d['fxb_alertbins']:>12,d} | "
              f"{m[2]:>12,d} {m[3]:>8,d} {m[4]:>5d} | "
              f"{st.get('timeout',0):>4d} {st.get('oom',0):>4d} {st.get('crash',0):>5d}")
        tot["alerts"] += d["fxb_alerts"]; tot["alertbins"] += d["fxb_alertbins"]
        tot["mtru"] += m[2]; tot["merr"] += m[3]; tot["moom"] += m[4]
        tot["to"] += st.get('timeout',0); tot["oom"] += st.get('oom',0); tot["crash"] += st.get('crash',0)
        tot["uanl"] += d['uniq_analyzed']; tot["ualrt"] += d['uniq_alertbins']
    print("-"*len(hdr))
    print(f"{'TOTAL':9s} | {sum(m[0] for m in MANGO_T6.values()):>4d} {tot['uanl']:>6d} {tot['ualrt']:>5d} | "
          f"{tot['alerts']:>12,d} {tot['alertbins']:>12,d} | "
          f"{tot['mtru']:>12,d} {tot['merr']:>8,d} {tot['moom']:>5d} | "
          f"{tot['to']:>4d} {tot['oom']:>4d} {tot['crash']:>5d}")

    # global status + sink/source breakdown
    gstat = Counter(); gsink = Counter(); gsrc = Counter(); gfind = 0
    for r in res.values():
        gstat[r["status"]] += 1
        gfind += r["nfind"]
        for fd in r.get("findings", []):
            gsink[clean_sink(fd.get("sink_func"))] += 1
            gsrc[fd.get("source_class")] += 1
    print(f"\n=== global (unique binaries) === status={dict(gstat)}")
    print(f"total unique findings={gfind:,}")
    print(f"by sink : {dict(gsink.most_common(12))}")
    print(f"by source: {dict(gsrc)}")

if __name__ == "__main__":
    main()
