"""Reading the results: queue status, and regression/improvement detection.

A commit's value for a metric is from its unit's latest successful run. It is compared to the
median of the same metric over the previous `window` measured commits, and flagged when it
moves by more than the metric's threshold *and* by more than 3x the spread (median absolute
deviation) the baseline itself shows. Quality metrics (`q_*`) and output digests are flagged on
any change: those are deterministic, so any change is real.
"""

from __future__ import annotations

import statistics
import time
from collections import defaultdict

from . import db as dbm

SWEEP = "firmware-sweep"

# metric -> relative threshold. Lower is better for all of these.
PERF = {"instructions": 0.005, "wall_s": 0.05, "peak_footprint_b": 0.03, "user_s": 0.08}
# Quality metrics where the direction is known; any other q_* change is reported as a change.
HIGHER_BETTER = {"q_tp", "q_recall", "q_pass", "q_has_flow"}
LOWER_BETTER = {"q_fp"}
DIGESTS = ("paths_digest", "findings_digest")


def connect(cfg):
    return dbm.connect(cfg["paths"]["root"] / "bench.db")


def measured_commits(conn) -> list[dict]:
    """Commits with at least one finished pass unit, newest first."""
    return [dict(r) for r in conn.execute(
        "SELECT c.*, (SELECT count(*) FROM units u WHERE u.sha=c.sha AND u.suite!=? ) AS units, "
        "(SELECT count(*) FROM units u WHERE u.sha=c.sha AND u.suite!=? AND u.status='done') AS done, "
        "(SELECT count(*) FROM units u WHERE u.sha=c.sha AND u.suite!=? AND u.status IN ('failed','skipped')) AS bad "
        "FROM commits c ORDER BY c.committed_at DESC", (SWEEP, SWEEP, SWEEP))]


def values(conn, sha: str) -> dict[tuple, dict]:
    """(suite, bench, phase) -> {metric: value, '_outcome': ..., digests...} for one commit's
    pass, from each unit's latest finished run."""
    out: dict[tuple, dict] = {}
    rows = conn.execute(
        "SELECT u.suite, u.bench, u.phase, u.status, r.id AS run_id, r.outcome FROM units u "
        "LEFT JOIN runs r ON r.id = (SELECT max(id) FROM runs WHERE unit_id=u.id AND outcome IS NOT NULL "
        "                            AND outcome!='killed') "
        "WHERE u.sha=? AND u.suite!=?", (sha, SWEEP)).fetchall()
    for r in rows:
        key = (r["suite"], r["bench"], r["phase"])
        v = {"_status": r["status"], "_outcome": r["outcome"], "_run": r["run_id"]}
        if r["run_id"] is not None:
            for m in conn.execute("SELECT name, value FROM metrics WHERE run_id=?", (r["run_id"],)):
                v[m["name"]] = m["value"]
            for a in conn.execute("SELECT name, value FROM artifacts WHERE run_id=? AND name IN (?,?)",
                                  (r["run_id"], *DIGESTS)):
                v[a["name"]] = a["value"]
        out[key] = v
    return out


def compare(conn, sha: str, window: int = 5) -> list[dict]:
    commits = [c for c in measured_commits(conn) if c["done"]]
    shas = [c["sha"] for c in commits]
    if sha not in shas:
        return []
    prev = shas[shas.index(sha) + 1:][:window]
    cur = values(conn, sha)
    base = [values(conn, p) for p in prev]
    findings = []
    for key, v in sorted(cur.items()):
        history = [b[key] for b in base if key in b]
        if not history:
            continue
        # Outcome changes first: a benchmark that used to finish and now does not.
        was_ok = [h["_outcome"] == "ok" for h in history if h["_outcome"]]
        if v["_outcome"] and was_ok:
            if v["_outcome"] != "ok" and all(was_ok):
                findings.append(dict(key=key, metric="outcome", kind="regression",
                                     value=v["_outcome"], baseline="ok"))
            elif v["_outcome"] == "ok" and not any(was_ok):
                findings.append(dict(key=key, metric="outcome", kind="improvement",
                                     value="ok", baseline=history[0]["_outcome"]))
        for metric, val in v.items():
            if metric.startswith("_"):
                continue
            hist = [h[metric] for h in history if metric in h]
            if not hist:
                continue
            if metric in DIGESTS:
                if val != hist[0]:
                    findings.append(dict(key=key, metric=metric, kind="change", value=val, baseline=hist[0]))
                continue
            med = statistics.median(hist)
            if metric.startswith("q_"):
                if val != hist[0]:
                    kind = ("improvement" if (val > hist[0]) == (metric in HIGHER_BETTER) else "regression") \
                        if metric in HIGHER_BETTER | LOWER_BETTER else "change"
                    findings.append(dict(key=key, metric=metric, kind=kind, value=val, baseline=hist[0],
                                         rel=(val - hist[0]) / hist[0] if hist[0] else None))
                continue
            if metric not in PERF or med == 0:
                continue
            rel = (val - med) / med
            mad = statistics.median(abs(h - med) for h in hist) / med if len(hist) >= 3 else 0.0
            if abs(rel) > max(PERF[metric], 3 * mad):
                findings.append(dict(key=key, metric=metric, kind="regression" if rel > 0 else "improvement",
                                     value=val, baseline=med, rel=rel, n=len(hist)))
    return findings


def sweep_summary(conn) -> dict:
    sha = dbm.get_state(conn, "sweep.sha")
    if not sha:
        return {}
    by_status = defaultdict(int)
    for r in conn.execute(
            "SELECT r.outcome, count(*) n FROM units u JOIN runs r ON r.id = "
            "(SELECT max(id) FROM runs WHERE unit_id=u.id AND outcome!='killed') "
            "WHERE u.sha=? AND u.suite=? AND u.phase='query' GROUP BY r.outcome", (sha, SWEEP)):
        by_status[r["outcome"] or "?"] = r["n"]
    failed_early = conn.execute(
        "SELECT count(DISTINCT bench) FROM units WHERE sha=? AND suite=? AND status IN ('failed','skipped') "
        "AND phase!='query'", (sha, SWEEP)).fetchone()[0]
    findings = conn.execute(
        "SELECT coalesce(sum(m.value),0), count(*) FROM units u JOIN runs r ON r.unit_id=u.id "
        "JOIN metrics m ON m.run_id=r.id AND m.name='q_findings' WHERE u.sha=? AND u.suite=?",
        (sha, SWEEP)).fetchone()
    done = conn.execute("SELECT count(DISTINCT bench) FROM units WHERE sha=? AND suite=? AND "
                        "status NOT IN ('pending','running') AND phase='query'", (sha, SWEEP)).fetchone()[0]
    from . import suites  # noqa: E402
    return {"sha": sha, "started_at": int(dbm.get_state(conn, "sweep.started_at", "0")),
            "cursor": int(dbm.get_state(conn, "sweep.cursor", "0")), "finished": done,
            "query_outcomes": dict(by_status), "failed_before_query": failed_early,
            "findings": findings[0], "binaries_with_results": findings[1]}


def status(cfg) -> None:
    conn = connect(cfg)
    print("commits:")
    for c in measured_commits(conn)[:10]:
        print(f"  {c['sha'][:10]} build={c['build_status']:8s} units {c['done']}/{c['units']} "
              f"({c['bad']} failed/skipped)  {c['subject'][:60]}")
    print("running:")
    for r in conn.execute("SELECT r.id, r.start_tier, r.started_at, u.suite, u.bench, u.phase FROM runs r "
                          "JOIN units u ON u.id=r.unit_id WHERE r.outcome IS NULL"):
        print(f"  run {r['id']} {r['suite']}/{r['bench']}/{r['phase']} tier={r['start_tier']} "
              f"for {time.time() - r['started_at']:.0f}s")
    s = sweep_summary(conn)
    if s:
        print(f"sweep @ {s['sha'][:10]}: {s['finished']} binaries finished, queued up to {s['cursor']}; "
              f"query outcomes {s['query_outcomes']}; {s['findings']:.0f} findings")
    print("recent events:")
    for e in conn.execute("SELECT * FROM events ORDER BY at DESC LIMIT 15"):
        print(f"  {time.strftime('%m-%d %H:%M:%S', time.localtime(e['at']))} {e['kind']} {e['detail'][:120]}")


def report(cfg, sha: str | None, window: int) -> None:
    conn = connect(cfg)
    commits = [c for c in measured_commits(conn) if c["done"]]
    if not commits:
        print("nothing measured yet")
        return
    if sha is None:
        sha = commits[0]["sha"]
    else:
        sha = next((c["sha"] for c in commits if c["sha"].startswith(sha)), sha)
    found = compare(conn, sha, window)
    print(f"{sha[:10]}: {len(found)} finding(s) vs. up to {window} previous commits")
    for f in sorted(found, key=lambda f: (f["kind"], f["key"])):
        rel = f" ({f['rel']:+.1%})" if f.get("rel") is not None else ""
        print(f"  {f['kind']:11s} {'/'.join(f['key'])} {f['metric']}: {f['baseline']} -> {f['value']}{rel}")
