"""Turn a finished benchmark's output into quality metrics.

Metrics are named `q_*` so reports can tell them from performance numbers. Alongside counts,
each scorer stores the *set* of what was found as an artifact, so a change in which flows are
reported shows up even when the count holds still.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path

TAINTED_PATH = "C0001.tainted-path"
TAINT_SOURCE = "C0003.taint-source"
TAINT_SINK = "C0004.taint-sink"


def _sarif_results(sarif: dict) -> list[dict]:
    return [r for run in sarif.get("runs", []) for r in run.get("results", [])]


def _flow_fingerprints(sarif: dict) -> list[str]:
    """A stable identity for each reported path: its message and the locations at both ends."""
    out = []
    for r in _sarif_results(sarif):
        if r.get("ruleId") != TAINTED_PATH:
            continue
        locs = []
        for cf in r.get("codeFlows", []):
            for tf in cf.get("threadFlows", []):
                steps = tf.get("locations", [])
                for step in (steps[:1] + steps[-1:]):
                    loc = step.get("location", {})
                    pl = loc.get("physicalLocation", {})
                    # Source line for bytecode, instruction offset for pcode. Vertex ids in
                    # step messages are left out: they are numbering, not identity.
                    locs.append(json.dumps([pl.get("artifactLocation", {}).get("uri"),
                                            pl.get("region", {}).get("startLine"),
                                            pl.get("address", {}).get("relativeAddress")]))
        out.append(json.dumps([r.get("message", {}).get("text"), sorted(set(locs))], sort_keys=True))
    return sorted(out)


def _counts(sarif: dict) -> dict[str, float]:
    rs = _sarif_results(sarif)
    return {
        "q_paths": sum(1 for r in rs if r.get("ruleId") == TAINTED_PATH),
        "q_sources": sum(1 for r in rs if r.get("ruleId") == TAINT_SOURCE),
        "q_sinks": sum(1 for r in rs if r.get("ruleId") == TAINT_SINK),
    }


def _digest(items: list[str]) -> str:
    return hashlib.sha256("\n".join(items).encode()).hexdigest()[:16]


def _numeric_leaves(obj, prefix: str, out: dict, depth: int = 0) -> None:
    if depth > 3:
        return
    if isinstance(obj, bool):
        return
    if isinstance(obj, (int, float)):
        out[prefix] = float(obj)
    elif isinstance(obj, dict):
        for k, v in obj.items():
            _numeric_leaves(v, f"{prefix}.{k}" if prefix else k, out, depth + 1)


def score(spec: dict, work: Path, ctx) -> tuple[dict[str, float], dict[str, str]]:
    """Returns (metrics, artifacts) for the benchmark whose last phase just finished."""
    kind = spec["kind"]
    sarif_path = work / "results.sarif"
    sarif = json.loads(sarif_path.read_text())
    metrics: dict[str, float] = dict(_counts(sarif))
    artifacts: dict[str, str] = {}
    fps = _flow_fingerprints(sarif)
    artifacts["paths_digest"] = _digest(fps)
    (work / "paths.json").write_text(json.dumps(fps, indent=0))

    report = work / "report.json"
    if report.exists():
        leaves: dict[str, float] = {}
        _numeric_leaves(json.loads(report.read_text()), "", leaves)
        metrics.update({f"q_report.{k}": v for k, v in leaves.items()})

    if kind == "taintbench":
        out = work / "taintbench-score.json"
        log = work / "taintbench-score.log"
        with open(log, "wb") as f:
            subprocess.run([ctx.xtask, "taintbench-score", "--apps-dir", spec["apps_dir"],
                            "--app", spec["app"], "--sarif", str(sarif_path), "--json", str(out)],
                           stdout=f, stderr=subprocess.STDOUT, check=True)
        s = json.loads(out.read_text())
        metrics["q_tp"] = len(s["matched_positive"])
        metrics["q_fp"] = len(s["matched_negative"])
        metrics["q_recall"] = len(s["matched_positive"]) / max(1, s["total_positive"])
        metrics["q_pass"] = 1.0 if s["passed"] else 0.0
        artifacts["matched_positive"] = json.dumps(s["matched_positive"])
        artifacts["matched_negative"] = json.dumps(s["matched_negative"])
        artifacts["verdict"] = s["verdict"]

    elif kind == "flow-expected":
        has_flow = any(r.get("ruleId") == TAINTED_PATH and r.get("codeFlows")
                       for r in _sarif_results(sarif))
        metrics["q_has_flow"] = 1.0 if has_flow else 0.0
        metrics["q_pass"] = 1.0 if has_flow == spec["expect_flow"] else 0.0
        artifacts["status"] = spec["status"]

    elif kind == "firmware":
        sys.path.insert(0, str(ctx.bench_repo / "firmware-eval" / "harness"))
        import normalize_ctadl  # noqa: E402  (the firmware-eval harness's own SARIF reader)
        findings = normalize_ctadl.parse_sarif(sarif, spec["sha256"])
        rows = sorted(json.dumps([f.sink_func, f.sink_callsite, f.source_class])
                      for f in findings)
        metrics["q_findings"] = len(rows)
        for cls in sorted({f.source_class for f in findings}):
            metrics[f"q_findings.{cls}"] = sum(1 for f in findings if f.source_class == cls)
        artifacts["findings"] = json.dumps(rows)
        artifacts["findings_digest"] = _digest(rows)

    return metrics, artifacts
