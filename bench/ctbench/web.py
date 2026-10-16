"""The results page: latest commit, coarse trends over commits, the firmware sweep, and what the
daemon is doing. Standard library only; reads the database read-only.

Run it on the efficiency cores (`taskpolicy -b`; the launchd plist does this), so looking at
results never competes with a measurement.
"""

from __future__ import annotations

import html
import json
import sqlite3
import time
from collections import defaultdict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from . import report, suites

SWEEP = report.SWEEP

# Per suite, the quality number worth a trend line: (metric, label, how to combine).
QUALITY = {
    "taintbench": ("q_tp", "ground-truth flows found"),
    "android-icc": ("q_pass", "cases passing"),
    "fdroid": ("q_paths", "reported paths"),
    "apps": ("q_paths", "reported paths"),
    "firmware7": ("q_findings", "cmdi findings"),
}

CSS = """
:root {
  color-scheme: light;
  --page: #f9f9f7; --surface: #fcfcfb; --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781;
  --grid: #e1e0d9; --axis: #c3c2b7; --border: rgba(11,11,11,0.10);
  --series-1: #2a78d6; --good: #0ca30c; --good-text: #006300; --critical: #d03b3b;
  --warning: #fab219;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;
    --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781;
    --grid: #2c2c2a; --axis: #383835; --border: rgba(255,255,255,0.10);
    --series-1: #3987e5; --good-text: #0ca30c;
  }
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --page: #0d0d0d; --surface: #1a1a19; --ink: #f0efec; --ink-2: #c3c2b7; --muted: #898781;
  --grid: #2c2c2a; --axis: #383835; --border: rgba(255,255,255,0.10);
  --series-1: #3987e5; --good-text: #0ca30c;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--page); color: var(--ink);
       font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 1180px; margin: 0 auto; padding: 20px 16px 48px; }
h1 { font-size: 20px; margin: 0 0 2px; }
h2 { font-size: 15px; margin: 28px 0 10px; }
a { color: var(--series-1); text-decoration: none; }
a:hover { text-decoration: underline; }
.sub { color: var(--ink-2); }
.muted { color: var(--muted); }
.card { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 14px 16px; }
.row { display: flex; flex-wrap: wrap; gap: 12px; }
.row > .card { flex: 1 1 220px; }
.k { color: var(--ink-2); font-size: 12px; }
.v { font-size: 22px; font-weight: 600; }
table { width: 100%; border-collapse: collapse; font-size: 13px; }
th { text-align: left; color: var(--ink-2); font-weight: 500; border-bottom: 1px solid var(--axis); padding: 6px 8px; }
td { border-bottom: 1px solid var(--grid); padding: 5px 8px; vertical-align: top; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; white-space: nowrap; }
.scroll { overflow-x: auto; }
code, .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; }
.tag { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; white-space: nowrap; }
.tag::before { content: ""; width: 8px; height: 8px; border-radius: 50%; background: var(--c); }
.reg { --c: var(--critical); } .imp { --c: var(--good); } .chg { --c: var(--muted); } .warn { --c: var(--warning); }
.bar { height: 10px; background: var(--grid); border-radius: 5px; overflow: hidden; }
.bar > div { height: 100%; background: var(--series-1); border-radius: 5px; }
.charts { display: grid; grid-template-columns: repeat(auto-fill, minmax(270px, 1fr)); gap: 12px; }
.chart { position: relative; }
.chart h3 { font-size: 13px; font-weight: 600; margin: 0 0 2px; }
.chart svg { display: block; width: 100%; height: auto; overflow: visible; }
.tip { position: absolute; pointer-events: none; background: var(--surface); border: 1px solid var(--border);
       border-radius: 6px; padding: 6px 8px; font-size: 12px; box-shadow: 0 2px 8px rgba(0,0,0,.12);
       display: none; z-index: 2; max-width: 260px; }
.tip strong { font-size: 14px; display: block; }
details summary { cursor: pointer; color: var(--ink-2); }
"""

# Crosshair + tooltip for the trend charts: snaps to the nearest commit; text via textContent.
JS = """
document.querySelectorAll('.chart').forEach(function (box) {
  var svg = box.querySelector('svg'), tip = box.querySelector('.tip');
  var pts = JSON.parse(box.dataset.points), unit = box.dataset.unit;
  var line = svg.querySelector('.cross');
  function show(ev) {
    var r = svg.getBoundingClientRect(), vb = svg.viewBox.baseVal;
    var x = (ev.clientX - r.left) * vb.width / r.width;
    var best = null;
    pts.forEach(function (p) { if (!best || Math.abs(p.x - x) < Math.abs(best.x - x)) best = p; });
    if (!best) return;
    line.setAttribute('x1', best.x); line.setAttribute('x2', best.x); line.style.display = '';
    tip.textContent = '';
    var s = document.createElement('strong'); s.textContent = best.label + (unit ? ' ' + unit : '');
    var d = document.createElement('div'); d.textContent = best.sha + ' ' + best.subject;
    d.className = 'muted';
    tip.appendChild(s); tip.appendChild(d);
    tip.style.display = 'block';
    var left = best.x * r.width / vb.width + 8;
    tip.style.left = Math.min(left, r.width - 200) + 'px'; tip.style.top = '28px';
  }
  svg.addEventListener('pointermove', show);
  svg.addEventListener('pointerleave', function () { tip.style.display = 'none'; line.style.display = 'none'; });
});
"""


def esc(s) -> str:
    return html.escape(str(s), quote=True)


def fmt(v, metric: str = "") -> str:
    if v is None:
        return "–"
    if isinstance(v, str):
        return esc(v)
    if metric.endswith("_b"):
        return f"{v / 2**20:,.0f} MiB"
    if metric in ("instructions", "cycles"):
        return f"{v / 1e9:,.2f} G"
    if metric.endswith("_s"):
        return f"{v:,.1f} s"
    if float(v).is_integer():
        return f"{v:,.0f}"
    return f"{v:,.3f}"


def ro(cfg) -> sqlite3.Connection:
    conn = sqlite3.connect(f"file:{cfg['paths']['root'] / 'bench.db'}?mode=ro", uri=True, timeout=10)
    conn.row_factory = sqlite3.Row
    return conn


# --- data ------------------------------------------------------------------------------------


def suite_totals(conn, commits: list[dict]) -> dict[str, list[dict]]:
    """Per suite, one point per commit whose part of the pass is complete: total instructions,
    total clean wall time (when every unit has one), and the suite's quality number."""
    out: dict[str, list[dict]] = defaultdict(list)
    for c in reversed(commits):  # oldest first
        vals = report.values(conn, c["sha"])
        by_suite: dict[str, list] = defaultdict(list)
        for (suite, _bench, _phase), v in vals.items():
            by_suite[suite].append(v)
        for suite, vs in by_suite.items():
            if any(v["_status"] in ("pending", "running") for v in vs):
                continue
            q_metric = QUALITY.get(suite, (None,))[0]
            done = [v for v in vs if v["_outcome"] == "ok"]
            walls = [v.get("wall_s") for v in done]
            out[suite].append({
                "sha": c["sha"][:10], "subject": c["subject"][:70],
                "instructions": sum(v.get("instructions", 0) for v in done) / 1e9,
                "wall": sum(walls) if walls and all(w is not None for w in walls) else None,
                "quality": sum(v.get(q_metric, 0) for v in done) if q_metric else None,
                "failed": sum(1 for v in vs if v["_status"] in ("failed", "skipped")),
            })
    return out


def sweep_history(conn) -> list[dict]:
    rows = []
    for s in conn.execute("SELECT DISTINCT u.sha, c.subject, c.committed_at FROM units u JOIN commits c "
                          "ON c.sha=u.sha WHERE u.suite=? ORDER BY c.committed_at DESC", (SWEEP,)):
        counts = defaultdict(int)
        for r in conn.execute(
                "SELECT u.bench, max(CASE u.status WHEN 'done' THEN 0 ELSE 1 END) AS bad, "
                "max(CASE WHEN u.status IN ('pending','running') THEN 1 ELSE 0 END) AS open, "
                "(SELECT r.outcome FROM runs r JOIN units v ON v.id=r.unit_id WHERE v.sha=u.sha AND "
                " v.suite=u.suite AND v.bench=u.bench AND r.outcome NOT IN ('ok','killed') LIMIT 1) AS why "
                "FROM units u WHERE u.sha=? AND u.suite=? GROUP BY u.bench", (s["sha"], SWEEP)):
            counts["open" if r["open"] else (r["why"] or "failed") if r["bad"] else "ok"] += 1
        findings = conn.execute(
            "SELECT coalesce(sum(m.value),0) FROM units u JOIN runs r ON r.unit_id=u.id JOIN metrics m "
            "ON m.run_id=r.id AND m.name='q_findings' WHERE u.sha=? AND u.suite=?", (s["sha"], SWEEP)).fetchone()[0]
        rows.append({"sha": s["sha"], "subject": s["subject"], "counts": dict(counts), "findings": findings})
    return rows


# --- rendering -------------------------------------------------------------------------------


def chart(title: str, points: list[dict], key: str, unit: str = "") -> str:
    pts = [p for p in points if p.get(key) is not None]
    if not pts:
        return ""
    W, H, L, R, T, B = 300, 120, 44, 8, 8, 20
    ys = [p[key] for p in pts]
    lo, hi = min(ys), max(ys)
    if lo == hi:
        lo, hi = lo - (abs(lo) * 0.05 or 1), hi + (abs(hi) * 0.05 or 1)
    pad = (hi - lo) * 0.1
    lo, hi = lo - pad, hi + pad
    n = len(pts)
    def X(i): return L + (W - L - R) * (i / (n - 1) if n > 1 else 0.5)
    def Y(v): return T + (H - T - B) * (1 - (v - lo) / (hi - lo))
    ticks = [lo + (hi - lo) * f for f in (0.1, 0.5, 0.9)]
    grid = "".join(f'<line x1="{L}" x2="{W - R}" y1="{Y(t):.1f}" y2="{Y(t):.1f}" stroke="var(--grid)" stroke-width="1"/>'
                   f'<text x="{L - 6}" y="{Y(t) + 4:.1f}" text-anchor="end" font-size="10" fill="var(--muted)">{t:,.3g}</text>'
                   for t in ticks)
    path = " ".join(f"{'M' if i == 0 else 'L'}{X(i):.1f},{Y(p[key]):.1f}" for i, p in enumerate(pts))
    dots = "".join(f'<circle cx="{X(i):.1f}" cy="{Y(p[key]):.1f}" r="4" fill="var(--series-1)" '
                   f'stroke="var(--surface)" stroke-width="2"/>' for i, p in enumerate(pts)) if n <= 40 else ""
    data = [{"x": round(X(i), 1), "label": fmt(p[key]), "sha": p["sha"], "subject": p["subject"]}
            for i, p in enumerate(pts)]
    first, last = pts[0]["sha"][:7], pts[-1]["sha"][:7]
    return (f'<div class="card chart" data-unit="{esc(unit)}" data-points="{esc(json.dumps(data))}">'
            f'<h3>{esc(title)}</h3><div class="tip"></div>'
            f'<svg viewBox="0 0 {W} {H}" role="img" aria-label="{esc(title)} over {n} commits">'
            f'{grid}<line x1="{L}" x2="{W - R}" y1="{H - B}" y2="{H - B}" stroke="var(--axis)"/>'
            f'<path d="{path}" fill="none" stroke="var(--series-1)" stroke-width="2" stroke-linejoin="round"/>'
            f'{dots}<line class="cross" y1="{T}" y2="{H - B}" stroke="var(--muted)" stroke-width="1" style="display:none"/>'
            f'<text x="{L}" y="{H - 4}" font-size="10" fill="var(--muted)">{esc(first)}</text>'
            f'<text x="{W - R}" y="{H - 4}" font-size="10" fill="var(--muted)" text-anchor="end">{esc(last)}</text>'
            f'</svg></div>')


def kind_tag(kind: str) -> str:
    cls = {"regression": "reg", "improvement": "imp"}.get(kind, "chg")
    return f'<span class="tag {cls}">{esc(kind)}</span>'


def findings_table(found: list[dict], limit: int | None = None) -> str:
    order = {"regression": 0, "improvement": 1, "change": 2}
    found = sorted(found, key=lambda f: (order.get(f["kind"], 3), -abs(f.get("rel") or 0)))
    rows = []
    for f in found[:limit]:
        rel = f" ({f['rel']:+.1%})" if f.get("rel") is not None else ""
        rows.append(f"<tr><td>{kind_tag(f['kind'])}</td><td>{esc('/'.join(f['key']))}</td>"
                    f"<td class=mono>{esc(f['metric'])}</td>"
                    f"<td class=num>{fmt(f['baseline'], f['metric'])}</td>"
                    f"<td class=num>{fmt(f['value'], f['metric'])}{esc(rel)}</td></tr>")
    more = f'<p class="muted">{len(found) - limit} more on the commit page.</p>' if limit and len(found) > limit else ""
    if not rows:
        return '<p class="muted">No changes against the previous commits.</p>'
    return ('<div class="scroll"><table><tr><th></th><th>benchmark</th><th>metric</th>'
            '<th class=num>baseline</th><th class=num>this commit</th></tr>' + "".join(rows) + "</table></div>" + more)


def page(title: str, body: str) -> bytes:
    return (f'<!doctype html><html lang="en"><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width, initial-scale=1">'
            f'<meta http-equiv="refresh" content="120"><title>{esc(title)}</title>'
            f'<style>{CSS}</style></head><body><main>{body}</main><script>{JS}</script></body></html>').encode()


def index(cfg) -> bytes:
    conn = ro(cfg)
    commits = report.measured_commits(conn)
    now = time.time()
    gate = conn.execute("SELECT * FROM events WHERE kind='gate' ORDER BY at DESC LIMIT 1").fetchone()
    last = conn.execute("SELECT max(at) FROM events").fetchone()[0]
    running = conn.execute(
        "SELECT r.id, r.start_tier, r.started_at, r.demoted, r.promoted, u.suite, u.bench, u.phase, u.sha "
        "FROM runs r JOIN units u ON u.id=r.unit_id WHERE r.outcome IS NULL ORDER BY r.started_at").fetchall()
    building = conn.execute("SELECT sha FROM commits WHERE build_status='building'").fetchall()

    b = [f'<h1>ctadl benchmarks</h1><div class="sub">{esc(cfg["track"]["remote"])}/{esc(cfg["track"]["branch"])} '
         f'of <code>{esc(cfg["paths"]["repo"])}</code></div>']

    # Status tiles.
    state = gate["detail"].split(":")[0] if gate else "unknown"
    heartbeat = f"{now - last:.0f}s ago" if last else "never"
    measured = [c for c in commits if c["done"]]
    sw = report.sweep_summary(conn)
    total = len(suites.sweep_list(cfg)) if cfg.get("sweep", {}).get("enabled") else 0
    b.append('<div class="row" style="margin-top:14px">')
    b.append(f'<div class="card"><div class="k">machine</div><div class="v">{esc(state)}</div>'
             f'<div class="muted">{esc(gate["detail"].split(";", 1)[-1][:90] if gate else "")}</div></div>')
    b.append(f'<div class="card"><div class="k">daemon heartbeat</div><div class="v">{esc(heartbeat)}</div>'
             f'<div class="muted">{len(running)} run(s) active{", building" if building else ""}</div></div>')
    b.append(f'<div class="card"><div class="k">commits measured</div><div class="v">{len(measured)}</div>'
             f'<div class="muted">{len(commits)} seen</div></div>')
    if sw:
        pct = sw["finished"] / total if total else 0
        b.append(f'<div class="card"><div class="k">firmware sweep @ <code>{esc(sw["sha"][:10])}</code></div>'
                 f'<div class="v">{sw["finished"]:,} / {total:,}</div>'
                 f'<div class="bar" style="margin-top:6px"><div style="width:{pct * 100:.1f}%"></div></div></div>')
    b.append("</div>")

    # Latest commit.
    if measured:
        c = measured[0]
        found = report.compare(conn, c["sha"])
        nreg = sum(1 for f in found if f["kind"] == "regression")
        nimp = sum(1 for f in found if f["kind"] == "improvement")
        b.append(f'<h2>Latest: <a href="/c/{esc(c["sha"])}"><code>{esc(c["sha"][:10])}</code></a> '
                 f'<span class="sub">{esc(c["subject"])}</span></h2>')
        bad = f', {c["bad"]} failed or skipped' if c["bad"] else ""
        b.append(f'<div class="card"><p style="margin-top:0">{c["done"]:,} of {c["units"]:,} units done{bad} · '
                 f'<span class="tag reg">{nreg} regressions</span> · <span class="tag imp">{nimp} improvements</span></p>'
                 f'{findings_table(found, 25)}</div>')

    # Trends.
    totals = suite_totals(conn, commits[:60])
    charts = []
    for suite in sorted(totals, key=lambda s: cfg["suites"].get(s, {}).get("order", 99)):
        pts = totals[suite]
        charts.append(chart(f"{suite}: instructions (G)", pts, "instructions"))
        if suite in QUALITY:
            charts.append(chart(f"{suite}: {QUALITY[suite][1]}", pts, "quality"))
        charts.append(chart(f"{suite}: clean wall time (s)", pts, "wall", "s"))
    charts = [c for c in charts if c]
    if charts:
        b.append('<h2>Over time</h2><p class="muted" style="margin-top:-6px">One point per commit whose '
                 'suite finished. Instructions and wall time are summed over the suite; lower is better.</p>'
                 f'<div class="charts">{"".join(charts)}</div>')

    # Running now.
    if running or building:
        b.append("<h2>Running now</h2><div class=\"card scroll\"><table><tr><th>what</th><th>tier</th>"
                 "<th class=num>for</th></tr>")
        for s in building:
            b.append(f"<tr><td>build <code>{esc(s['sha'][:10])}</code></td><td></td><td></td></tr>")
        for r in running:
            tier = r["start_tier"] + (" → e" if r["demoted"] else " → p" if r["promoted"] else "")
            b.append(f"<tr><td>{esc(r['suite'])}/{esc(r['bench'])}/{esc(r['phase'])} "
                     f"<span class=muted>{esc(r['sha'][:7])}</span></td><td>{esc(tier)}</td>"
                     f"<td class=num>{now - r['started_at']:,.0f} s</td></tr>")
        b.append("</table></div>")

    # Sweeps.
    hist = sweep_history(conn)
    if hist:
        b.append('<h2>Firmware sweeps</h2><div class="card scroll"><table><tr><th>commit</th>'
                 '<th class=num>ok</th><th class=num>timeout</th><th class=num>oom</th><th class=num>failed</th>'
                 '<th class=num>open</th><th class=num>findings</th></tr>')
        for h in hist:
            k = h["counts"]
            b.append(f"<tr><td><code>{esc(h['sha'][:10])}</code> <span class=sub>{esc(h['subject'][:50])}</span></td>"
                     f"<td class=num>{k.get('ok', 0):,}</td><td class=num>{k.get('timeout', 0):,}</td>"
                     f"<td class=num>{k.get('oom', 0):,}</td><td class=num>{k.get('failed', 0):,}</td>"
                     f"<td class=num>{k.get('open', 0):,}</td><td class=num>{h['findings']:,.0f}</td></tr>")
        b.append("</table></div>")

    # Commits.
    b.append('<h2>Commits</h2><div class="card scroll"><table><tr><th>commit</th><th>build</th>'
             '<th class=num>units</th><th class=num>regressions</th><th class=num>improvements</th></tr>')
    for c in commits[:40]:
        found = report.compare(conn, c["sha"]) if c["done"] else []
        b.append(f'<tr><td><a href="/c/{esc(c["sha"])}"><code>{esc(c["sha"][:10])}</code></a> '
                 f'<span class=sub>{esc(c["subject"][:70])}</span></td><td>{esc(c["build_status"])}</td>'
                 f'<td class=num>{c["done"]:,}/{c["units"]:,}</td>'
                 f'<td class=num>{sum(1 for f in found if f["kind"] == "regression")}</td>'
                 f'<td class=num>{sum(1 for f in found if f["kind"] == "improvement")}</td></tr>')
    b.append("</table></div>")

    # Events.
    b.append('<h2>Recent events</h2><div class="card scroll"><table>')
    for e in conn.execute("SELECT * FROM events WHERE kind NOT IN ('gate') ORDER BY at DESC LIMIT 25"):
        b.append(f"<tr><td class=num>{time.strftime('%m-%d %H:%M', time.localtime(e['at']))}</td>"
                 f"<td>{esc(e['kind'])}</td><td class=mono>{esc(e['detail'][:160])}</td></tr>")
    b.append("</table></div>")
    return page("ctadl benchmarks", "".join(b))


def commit_page(cfg, sha: str) -> bytes:
    conn = ro(cfg)
    c = conn.execute("SELECT * FROM commits WHERE sha LIKE ?", (sha + "%",)).fetchone()
    if c is None:
        return page("not found", "<p>No such commit.</p>")
    found = report.compare(conn, c["sha"])
    vals = report.values(conn, c["sha"])
    b = [f'<p><a href="/">← all results</a></p><h1><code>{esc(c["sha"][:10])}</code> {esc(c["subject"])}</h1>',
         f'<div class="sub">build {esc(c["build_status"])}</div>',
         "<h2>Changes vs. previous commits</h2>", f'<div class="card">{findings_table(found)}</div>',
         '<h2>Every unit</h2><div class="card scroll"><table><tr><th>benchmark</th><th>phase</th><th>outcome</th>'
         '<th class=num>wall</th><th class=num>instructions</th><th class=num>peak</th><th>quality</th></tr>']
    for (suite, bench, phase), v in sorted(vals.items()):
        q = ", ".join(f"{k[2:]}={fmt(x)}" for k, x in sorted(v.items())
                      if k.startswith("q_") and not k.startswith("q_report."))
        b.append(f"<tr><td>{esc(suite)}/{esc(bench)}</td><td>{esc(phase)}</td>"
                 f"<td>{esc(v['_outcome'] or v['_status'])}</td>"
                 f"<td class=num>{fmt(v.get('wall_s'), 'wall_s')}</td>"
                 f"<td class=num>{fmt(v.get('instructions'), 'instructions')}</td>"
                 f"<td class=num>{fmt(v.get('peak_footprint_b'), 'peak_footprint_b')}</td>"
                 f"<td class=mono>{esc(q)}</td></tr>")
    b.append("</table></div>")
    return page(f"{c['sha'][:10]} · ctadl benchmarks", "".join(b))


def serve(cfg, host: str, port: int) -> None:
    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            url = urlparse(self.path)
            try:
                if url.path == "/":
                    body, status = index(cfg), 200
                elif url.path.startswith("/c/"):
                    body, status = commit_page(cfg, url.path[3:]), 200
                elif url.path == "/api/latest":
                    conn = ro(cfg)
                    commits = [c for c in report.measured_commits(conn) if c["done"]]
                    data = {"commit": commits[0] if commits else None,
                            "findings": report.compare(conn, commits[0]["sha"]) if commits else [],
                            "sweep": report.sweep_summary(conn)}
                    body, status = json.dumps(data, default=str).encode(), 200
                    self.send_response(status)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write(body)
                    return
                else:
                    body, status = page("not found", "<p>Not found.</p>"), 404
            except sqlite3.OperationalError as e:
                body, status = page("no data", f"<p>No results database yet ({esc(e)}).</p>"), 503
            self.send_response(status)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    print(f"serving on http://{host}:{port}/", flush=True)
    ThreadingHTTPServer((host, port), Handler).serve_forever()
