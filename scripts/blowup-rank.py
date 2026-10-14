#!/usr/bin/env python3
"""Rank rules and relations from `ctadl index` debug logs (the recipe in recipes.md).

Each log is one run at RUST_LOG=warn,ctadl=debug, usually one rung of scripts/blowup-ladder.ps1.
For every log this prints:
  - the run's outcome: fixpoint or timed out, and the memguard summary line if there is one;
  - each SCC's iterations and time;
  - the rules ranked by time, with the size of the relation each one derives into and the time
    per tuple of that relation (a rule that spends much and yields little ranks high there);
  - the relations ranked by bytes, each with its indices (e.g. `reach_0_2`) and their bytes.
    The BYODS stores (`locals`, `assign_like`, `locals_key`, `edge_split`, `ext_dst`) print n/a
    in ascent's index table and are taken from their own store estimates instead.
With several logs it first prints one summary row per log, for reading a timeout ladder.

usage: blowup-rank.py [--top N] LOG [LOG ...]
"""
import argparse
import re
import sys

BYODS = ("locals", "assign_like", "locals_key", "edge_split", "ext_dst")
UNITS = {"ns": 1e-9, "µs": 1e-6, "us": 1e-6, "ms": 1e-3, "s": 1.0}
DUR = re.compile(r"([\d.]+)(ns|µs|us|ms|s)\b")


def secs(text):
    m = DUR.search(text)
    return float(m.group(1)) * UNITS[m.group(2)] if m else 0.0


def body(line):
    """A log line without its `[timestamp LEVEL module]` prefix."""
    return re.sub(r"^\[[^\]]*\] ", "", line.rstrip("\n"))


def parse(path):
    run = {"path": path, "sccs": [], "rules": [], "sizes": {}, "rels": {}, "byods_mb": {},
           "memcp": [], "guard": None, "timed_out": None}
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = f.readlines()
    section = None
    scc = None
    for raw in lines:
        line = body(raw)
        if raw.startswith("[memguard] limit"):
            run["guard"] = raw.strip().replace("[memguard] ", "")
        elif "TIMED OUT after" in line:
            run["timed_out"] = line
        elif "[mem cp]" in line:
            run["memcp"].append(line.split("[mem cp] ", 1)[1])
        elif "index scc times:" in line:
            section = "scc"
        elif "[relsizes] index relation sizes:" in line:
            section = "relsizes"
        elif "[relsizes] byods-backed:" in line:
            section = "byods"
            m = re.search(r"assign_like size: (\d+)", line)
            if m:
                run["sizes"]["assign_like"] = int(m.group(1))
        elif "[idxsizes] index sizes:" in line:
            section = "idx"
        elif raw.startswith("["):
            section = None
            # BYODS store estimates: `locals store estimate: total X MB`, `assign_like store
            # estimate: trie X MB`, and `[byods] NAME: ... store estimate: total|trie X MB`.
            m = re.search(r"(?:\[byods\] (\w+): )?(\w+) store estimate: (?:total|trie) ([\d.]+) MB", line)
            if m:
                run["byods_mb"][m.group(1) or m.group(2)] = float(m.group(3))
        elif section == "scc":
            s = line.strip()
            m = re.match(r"scc (\d+): iterations: (\d+), time: (\S+)", s)
            if m:
                scc = int(m.group(1))
                run["sccs"].append((scc, int(m.group(2)), secs(m.group(3))))
            elif s.startswith("rule "):
                run["rules"].append({"scc": scc, "text": s[5:], "time": 0.0})
            elif s.startswith("time:") and run["rules"]:
                run["rules"][-1]["time"] = secs(s)
            elif s.startswith("derived:") and run["rules"]:
                # Written by the counters in vendor/ascent_macro (rows produced, rows new).
                m = re.match(r"derived: (\d+) inserted: (\d+)", s)
                if m:
                    run["rules"][-1]["derived"] = int(m.group(1))
                    run["rules"][-1]["inserted"] = int(m.group(2))
        elif section in ("relsizes", "byods"):
            m = re.match(r"\s*(\w+) size: (\d+)", line)
            if m and not (section == "relsizes" and m.group(1) in BYODS and m.group(1) in run["sizes"]):
                run["sizes"][m.group(1)] = int(m.group(2))
        elif section == "idx":
            m = re.match(r"\s*(rel|ind) (\w+) (?:keys=(\d+) entries=(\d+) bytes=(\d+)|n/a)", line)
            if not m:
                continue
            kind, name = m.group(1), m.group(2)
            nbytes = int(m.group(5)) if m.group(5) else None
            if kind == "rel":
                cur = run["rels"].setdefault(name, {"bytes": nbytes, "inds": []})
            else:
                cur["inds"].append((short_index(name), nbytes, int(m.group(3) or 0)))
    return run


def short_index(name):
    """`reach_indices_0_2` -> `reach_0_2`, the recipe's spelling."""
    return name.replace("_indices_", "_")


def heads(rule_text):
    return [h.strip() for h in rule_text.split(" <-- ", 1)[0].split(",")]


def rel_mb(run, name):
    if name in run["byods_mb"]:
        return run["byods_mb"][name]
    r = run["rels"].get(name)
    if not r or r["bytes"] is None:
        return 0.0
    return (r["bytes"] + sum(b or 0 for _, b, _ in r["inds"])) / 1e6


def outcome(run):
    if run["timed_out"]:
        return "TIMED OUT"
    if run["sccs"]:
        return "fixpoint"
    return "no profile (killed?)"


def ladder_row(run):
    big = max(run["sccs"], key=lambda s: s[2]) if run["sccs"] else None
    top = sorted(run["byods_mb"].items(), key=lambda kv: -kv[1])[:3]
    return "{:<34} {:<20} {:>14} {:>9}  {}  {}".format(
        run["path"][-34:], outcome(run),
        "scc {} x{}".format(big[0], big[1]) if big else "-",
        "{:.1f}s".format(big[2]) if big else "-",
        (run["guard"] or "no guard line").replace("limit ", "lim ").split("  exit")[0],
        ", ".join("{} {:.0f}MB".format(k, v) for k, v in top))


def report(run, top):
    print("=" * 100)
    print(run["path"])
    print("  outcome:", outcome(run), "|", run["guard"] or "no memguard line")
    if run["timed_out"]:
        print("  ", run["timed_out"])
    census = [c for c in run["memcp"] if "relation census" in c or "ascent_run returned" in c]
    for c in census:
        print("  mem:", c)
    if not run["sccs"]:
        print("  (no scc times: the run died before ascent returned; lower the timeout below the guard)")
        return

    print("\n  SCCs (iterations, time):")
    for scc, iters, t in sorted(run["sccs"], key=lambda s: -s[2]):
        if t >= 0.01:
            print("    scc {:>3}: {:>6} iters  {:>9.3f}s".format(scc, iters, t))

    print("\n  Rules by time (head size = final size of the derived relation; derived and new are the")
    print("  rows the rule produced and the rows that were new, when the binary counts them):")
    print("    {:>9} {:>4} {:>11} {:>10} {:>10} {:>10} {:>5}  rule".format(
        "time", "scc", "head size", "us/tuple", "derived", "new", "new%"))
    for r in sorted(run["rules"], key=lambda r: -r["time"])[:top]:
        size = max((run["sizes"].get(h, 0) for h in heads(r["text"])), default=0)
        per = "{:.3f}".format(r["time"] / size * 1e6) if size else "inf"
        text = r["text"] if len(r["text"]) <= 90 else r["text"][:87] + "..."
        d, n = r.get("derived"), r.get("inserted")
        print("    {:>8.3f}s {:>4} {:>11} {:>10} {:>10} {:>10} {:>5}  {}".format(
            r["time"], r["scc"], size, per, "-" if d is None else d, "-" if n is None else n,
            "{:.0f}".format(100 * n / d) if d else "-", text))
    counted = [r for r in run["rules"] if "derived" in r]
    if counted:
        d = sum(r["derived"] for r in counted)
        n = sum(r["inserted"] for r in counted)
        print("    all rules: {:.1f} M rows derived, {:.1f} M new ({:.0f}%)".format(
            d / 1e6, n / 1e6, 100 * n / d if d else 0))
        print("\n  Redundant work (rules deriving >= 100k rows, most rows that were not new first):")
        for r in sorted((r for r in counted if r["derived"] >= 100000),
                        key=lambda r: -(r["derived"] - r["inserted"]))[:top]:
            text = r["text"] if len(r["text"]) <= 90 else r["text"][:87] + "..."
            print("    {:>10} of {:>10} not new ({:>3.0f}% new) {:>8.3f}s  {}".format(
                r["derived"] - r["inserted"], r["derived"], 100 * r["inserted"] / r["derived"],
                r["time"], text))

    print("\n  Rules by time per head tuple (only those taking >= 1% of the total):")
    total = sum(r["time"] for r in run["rules"]) or 1.0
    costly = []
    for r in run["rules"]:
        size = max((run["sizes"].get(h, 0) for h in heads(r["text"])), default=0)
        if r["time"] >= 0.01 * total:
            costly.append((r["time"] / size if size else float("inf"), r, size))
    for per, r, size in sorted(costly, key=lambda x: -x[0])[:top]:
        text = r["text"] if len(r["text"]) <= 110 else r["text"][:107] + "..."
        print("    {:>10} us/tuple {:>8.3f}s  {}".format(
            "inf" if per == float("inf") else "{:.3f}".format(per * 1e6), r["time"], text))

    print("\n  Relations by memory (rows, MB; indices in key-column spelling):")
    names = set(run["rels"]) | set(run["byods_mb"])
    for name in sorted(names, key=lambda n: -rel_mb(run, n))[:top]:
        print("    {:<34} {:>11} rows {:>9.1f} MB{}".format(
            name, run["sizes"].get(name, 0), rel_mb(run, name),
            "  (byods store)" if name in run["byods_mb"] else ""))
        r = run["rels"].get(name)
        if r and name not in run["byods_mb"]:
            for ind, b, keys in sorted(r["inds"], key=lambda i: -(i[1] or 0)):
                print("      {:<40} keys={:<10} {:>9.1f} MB".format(ind, keys, (b or 0) / 1e6))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--top", type=int, default=12)
    ap.add_argument("logs", nargs="+")
    a = ap.parse_args()
    # Ascent's rule text uses `⋯`, which the Windows console code page cannot encode.
    sys.stdout.reconfigure(encoding="utf-8")
    runs = [parse(p) for p in a.logs]
    if len(runs) > 1:
        print("Ladder (largest scc by time; top byods stores):")
        for r in runs:
            print("  " + ladder_row(r))
    for r in runs:
        report(r, a.top)


if __name__ == "__main__":
    sys.exit(main())
