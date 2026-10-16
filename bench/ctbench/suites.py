"""What a pass runs: each suite expands into benchmarks, each benchmark into ctadl phases.

A phase's `spec` is plain JSON stored on its unit, so a unit can be re-run exactly as queued
even after the configuration changes. argv entries may contain `{ctadl}`, `{store}` and
`{work}`, filled in at launch: the commit's binary, the benchmark's store and its work dir.
After the last phase, `score` names how to turn the output into quality metrics (see score.py).
"""

from __future__ import annotations

import json
import re
from pathlib import Path

from . import db as dbm

# The SARIF profile every suite queries with: source and sink endpoints plus connected paths,
# which is what the scorers read.
PROFILE = ["--sarif-profile", "agent"]


def _phase(name, argv, timeout, after=None, env=None):
    return {"phase": name, "argv": argv, "after": after, "timeout_s": timeout, "env": env or {}}


def _apk_phases(t, apk, *, model=None, import_flags=(), report=False):
    """import -> [report] -> index -> query for one APK or XAPK."""
    m = ["-m", model] if model else []
    phases = [_phase("import", ["{ctadl}", "--store", "{store}", "import", *import_flags,
                                "-n", "app", apk], t["import"])]
    if report:
        phases.append(_phase("report", ["{ctadl}", "--store", "{store}", "report", "app",
                                        "--format", "json", "-o", "{work}/report.json"],
                             t["report"], after="import"))
    phases.append(_phase("index", ["{ctadl}", "--store", "{store}", "index", "app", *m],
                         t["index"], after="import"))
    phases.append(_phase("query", ["{ctadl}", "--store", "{store}", "query", "app", *m,
                                   "-o", "{work}/results.sarif", *PROFILE],
                         t["query"], after="index"))
    return phases


def _pcode_phases(t, binary, model, ghidra_home):
    env = {"GHIDRA_HOME": ghidra_home}
    return [
        _phase("import", ["{ctadl}", "--store", "{store}", "import", "-l", "pcode", "-n", "bin",
                          binary], t["import"], env=env),
        _phase("index", ["{ctadl}", "--store", "{store}", "index", "bin", "-m", model],
               t["index"], after="import", env=env),
        _phase("query", ["{ctadl}", "--store", "{store}", "query", "bin", "-m", model,
                         "-o", "{work}/results.sarif", *PROFILE], t["query"], after="index",
               env=env),
    ]


def _json5_lite(text: str):
    """Enough JSON5 for the android-icc specs: comments, bare keys, trailing commas."""
    text = re.sub(r"^\s*//.*$", "", text, flags=re.M)
    text = re.sub(r"([{,]\s*)([A-Za-z_][A-Za-z0-9_]*)\s*:", r'\1"\2":', text)
    text = re.sub(r",(\s*[}\]])", r"\1", text)
    return json.loads(text)


def benchmarks(cfg, inputs, conn, sha: str):
    """Yield (suite, bench, order, phases, score) for one commit's pass."""
    bench_repo = cfg["paths"]["bench_repo"]
    for suite, s in sorted(cfg["suites"].items(), key=lambda kv: kv[1]["order"]):
        if not s.get("enabled", True):
            continue
        t = {**cfg["defaults"]["timeouts"], **s.get("timeouts", {})}
        mem = s.get("mem_cap_gib", cfg["defaults"]["mem_cap_gib"])

        if suite == "taintbench":
            apps_dir = bench_repo / "taintbench" / "apps"
            for app in sorted(p.name for p in apps_dir.iterdir() if (p / "app.json").exists()):
                meta = json.loads((apps_dir / app / "app.json").read_text())
                if "excluded" in meta:
                    continue
                apk = str(Path(inputs["taintbench_apks"]) / f"{app}.apk")
                model = str(apps_dir / app / "model.json")
                yield (suite, app, s["order"], mem,
                       _apk_phases(t, apk, model=model, import_flags=("-l", "apk")),
                       {"kind": "taintbench", "app": app, "apps_dir": str(apps_dir)})

        elif suite == "android-icc":
            for spec_path in sorted((bench_repo / s["specs"]).glob("*.json5")):
                spec = _json5_lite(spec_path.read_text())
                apk = str(Path(inputs["android_icc_apks"]) / spec["apk"])
                model = str(spec_path.parent / spec["model"])
                phases = _apk_phases(t, apk, import_flags=("-l", "apk", "--no-native-libs"))
                # As xtask's runner does: the model goes to query only.
                phases[-1]["argv"][5:5] = ["-m", model]
                yield (suite, spec_path.stem, s["order"], mem, phases,
                       {"kind": "flow-expected", "expect_flow": spec.get("expect_flow", True),
                        "status": spec.get("status", "pass")})

        elif suite in ("fdroid", "apps"):
            for app in json.loads((bench_repo / s["list"]).read_text()):
                yield (suite, app["bench"], s["order"], mem,
                       _apk_phases(t, app["path"], report=True),
                       {"kind": "flows", "input_sha256": app["sha256"]})

        elif suite == "firmware7":
            model = str(bench_repo / s["model"])
            for b in json.loads((bench_repo / s["list"]).read_text()):
                yield (suite, b["bench"], s["order"], mem,
                       _pcode_phases(t, b["path"], model, inputs["ghidra_home"]),
                       {"kind": "firmware", "sha256": b["sha256"]})



def sweep_batch(cfg, inputs, start: int, n: int):
    """Benchmarks start..start+n of the firmware sweep, in the shape benchmarks() yields."""
    s = cfg["sweep"]
    bench_repo = cfg["paths"]["bench_repo"]
    t = {**cfg["defaults"]["timeouts"], **s.get("timeouts", {})}
    model = str(bench_repo / s["model"])
    for b in sweep_list(cfg)[start:start + n]:
        yield ("firmware-sweep", b["sha256"][:16], 100, s["mem_cap_gib"],
               _pcode_phases(t, b["path"], model, inputs["ghidra_home"]),
               {"kind": "firmware", "sha256": b["sha256"]})


_sweep_cache: dict = {}


def sweep_list(cfg) -> list[dict]:
    path = cfg["paths"]["bench_repo"] / cfg["sweep"]["list"]
    if path not in _sweep_cache:
        _sweep_cache[path] = json.loads(path.read_text())
    return _sweep_cache[path]


def enqueue_pass(cfg, inputs, conn, sha: str) -> int:
    """Create the units for one commit's pass. Returns how many were added."""
    return enqueue(conn, sha, benchmarks(cfg, inputs, conn, sha))


def enqueue(conn, sha: str, benches) -> int:
    added = 0
    seq = conn.execute("SELECT coalesce(max(seq), 0) FROM units WHERE sha=?", (sha,)).fetchone()[0]
    conn.execute("BEGIN")
    try:
        for suite, bench, _order, mem, phases, score in benches:
            ids = {}
            for ph in phases:
                seq += 1
                spec = {**ph, "mem_cap_gib": mem}
                if ph is phases[-1]:
                    spec["score"] = score
                cur = conn.execute(
                    "INSERT OR IGNORE INTO units(sha, suite, bench, phase, seq, after, spec) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?)",
                    (sha, suite, bench, ph["phase"], seq, ids.get(ph["after"]), json.dumps(spec)),
                )
                added += cur.rowcount
                ids[ph["phase"]] = conn.execute(
                    "SELECT id FROM units WHERE sha=? AND suite=? AND bench=? AND phase=?",
                    (sha, suite, bench, ph["phase"]),
                ).fetchone()["id"]
        conn.execute("COMMIT")
    except BaseException:
        conn.execute("ROLLBACK")
        raise
    return added
