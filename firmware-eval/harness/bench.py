"""firmware-eval orchestrator.

Runs CTADL (the analyzer under test) over a work-list of binaries, classifies
each run, normalizes the SARIF into the results DB, and diffs against Mango.

Subcommands
-----------
  run      run CTADL on each artifact, classify, ingest findings
  compare  print the cta-vs-mango 2x2 (matched / cta-only / mango-only)
  stats    run-status counts and finding totals per tool+version

Work-list for `run`: a JSON manifest -- a list of entries
  [{"binary": "/path/in/fs/httpd", "artifact": "/exports/httpd.pcode",
    "arch": "mipsel"}, ...]
`binary` is hashed for the content address; `artifact` is what CTADL imports
(e.g. a Ghidra-pcode export). If `artifact` is omitted, `binary` is imported
directly (works for frontends CTADL autodetects).

Resource caps are applied by the caller/wrapper (cgroup or the
measure-process-memory skill on macOS); `run` enforces only a wall timeout.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

import findings as F
import ground_truth as GT
import normalize_ctadl as NC
import normalize_mango as NM

HARNESS_DIR = Path(__file__).resolve().parent
REPO_ROOT = HARNESS_DIR.parent.parent  # ct-firmware-eval/
DEFAULT_MODEL = HARNESS_DIR.parent / "models" / "cmdi-firmware.json5"
DEFAULT_CTADL = REPO_ROOT / "target" / "release" / "ctadl"

# stderr substrings that mean "CTADL could not handle this", not a crash.
_UNSUPPORTED_MARKERS = (
    "unrecognized filename extension", "no filename extension",
    "unsupported", "not yet implemented", "unimplemented",
)


def sha256_file(path: str | Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git_version(repo: Path) -> str:
    try:
        out = subprocess.check_output(
            ["git", "-C", str(repo), "rev-parse", "--short", "HEAD"],
            stderr=subprocess.DEVNULL,
        )
        return "cta@" + out.decode().strip()
    except Exception:
        return "cta@unknown"


def classify(returncode: Optional[int], timed_out: bool, stderr: str) -> tuple[str, Optional[str]]:
    if timed_out:
        return "timeout", None
    low = stderr.lower()
    for m in _UNSUPPORTED_MARKERS:
        if m in low:
            return "unsupported", m
    if returncode and returncode != 0:
        return "crash", None
    return "ok", None


def run_one(ctadl: Path, model: Path, entry: dict, timeout_s: int) -> tuple[F.RunInfo, list[F.Finding], Optional[str], Optional[str]]:
    """Run CTADL on one entry. Returns (RunInfo, findings, arch, path_example)."""
    binary = entry["binary"]
    artifact = entry.get("artifact", binary)
    arch = entry.get("arch")
    # Frontend/IR family for `ctadl go`. A raw firmware ELF goes through the
    # Ghidra pcode lifter (`-l pcode`); set entry["language"] to override (e.g.
    # "jvm"/"dex"/"auto" for non-binary artifacts, or "" to let ctadl sniff it).
    language = entry.get("language", "pcode")
    sha = sha256_file(binary)
    project = "cta_" + sha[:12]

    with tempfile.TemporaryDirectory() as td:
        cmd = [str(ctadl), "go", project]
        if language:
            cmd += ["-l", language]
        cmd += ["--models", str(model), str(artifact)]
        t0 = time.monotonic()
        timed_out = False
        stderr = ""
        rc: Optional[int] = None
        try:
            proc = subprocess.run(cmd, cwd=td, capture_output=True,
                                  text=True, timeout=timeout_s)
            rc, stderr = proc.returncode, proc.stderr
        except subprocess.TimeoutExpired as e:
            timed_out = True
            stderr = (e.stderr or b"").decode(errors="replace") if isinstance(e.stderr, bytes) else (e.stderr or "")
        wall = time.monotonic() - t0

        status, reason = classify(rc, timed_out, stderr)
        run = F.RunInfo(
            sha256=sha, tool="cta", analyzer_version="", status=status,
            wall_s=round(wall, 3), exit_code=rc, unsupported_reason=reason,
            stderr_excerpt=stderr[-2000:] or None,
            started_at=datetime.now(timezone.utc).isoformat(timespec="seconds"),
        )

        fs: list[F.Finding] = []
        sarif = Path(td) / "results.sarif"
        if status == "ok" and sarif.exists():
            try:
                fs = NC.parse_sarif(json.loads(sarif.read_text()), sha)
            except (json.JSONDecodeError, OSError) as e:
                run.status = "crash"
                run.stderr_excerpt = f"sarif parse: {e}"
        return run, fs, arch, binary


def _entries(args) -> list[dict]:
    entries = json.loads(Path(args.manifest).read_text()) if args.manifest else []
    for b in args.binary or []:
        entries.append({"binary": b})
    return entries


def _run_worklist(con, ctadl: Path, model: Path, version: str,
                  entries: list[dict], timeout: int, force: bool) -> tuple[int, int]:
    n = skipped = 0
    for entry in entries:
        try:
            sha = sha256_file(entry["binary"])
        except OSError as e:
            print(f"skip {entry.get('binary')}: {e}")
            continue
        if not force and F.run_exists(con, sha, "cta", version):
            skipped += 1
            continue
        run, fs, arch, pe = run_one(ctadl, model, entry, timeout)
        F.ingest(con, run, version, fs, arch=arch, path_example=pe)
        print(f"[{run.status:11}] {sha[:12]} {Path(entry['binary']).name:24} "
              f"findings={len(fs)} wall={run.wall_s}s")
        n += 1
    return n, skipped


def cmd_run(args) -> None:
    con = F.connect(args.db)
    version = args.version or git_version(REPO_ROOT)
    n, skipped = _run_worklist(con, Path(args.ctadl), Path(args.model), version,
                               _entries(args), args.timeout, args.force)
    print(f"\n{n} run, {skipped} cached. version={version} db={args.db}")


def _load_mango_gt(con, mango_out: str, mango_version: str) -> int:
    """Normalize a Mango results dir into the DB and snapshot it as known bugs."""
    for rf in NM.iter_result_files([mango_out]):
        try:
            obj = json.loads(Path(rf).read_text())
        except (json.JSONDecodeError, OSError) as e:
            print(f"skip {rf}: {e}")
            continue
        run, fs = NM.parse_result(obj)
        if run.sha256:
            fs = fs + NM.sibling_execv_findings(Path(rf), run.sha256, obj)
            F.ingest(con, run, mango_version, fs, path_example=obj.get("path"))
    return GT.ingest(con, GT.records_from_mango(con, mango_version))


def cmd_eval(args) -> None:
    """One shot: run ctadl over the work-list, (optionally) load ground truth,
    and score found/missed/extra."""
    con = F.connect(args.db)
    version = args.version or git_version(REPO_ROOT)

    n, skipped = _run_worklist(con, Path(args.ctadl), Path(args.model), version,
                               _entries(args), args.timeout, args.force)
    print(f"\n{n} run, {skipped} cached. version={version}\n")

    if args.mango_out:
        added = _load_mango_gt(con, args.mango_out, args.mango_version)
        print(f"ground truth: +{added} known bugs from mango ({args.mango_out})")
    if args.gt:
        added = GT.ingest(con, GT.load_dataset(Path(args.gt), args.gt_origin))
        print(f"ground truth: +{added} known bugs from {args.gt}")
    print()

    res = GT.score(con, "cta", version, args.addr_tolerance)
    if res["n_gt"] == 0:
        print("no ground truth loaded -- pass --mango-out DIR or --gt FILE "
              "(or load it once, then `bench.py score`).")
        return
    GT.print_score(res, "cta", version, args.show)


def cmd_compare(args) -> None:
    con = F.connect(args.db)
    res = F.compare(con, args.cta_version, args.mango_version,
                    tool_a="cta", tool_b="mango", addr_tolerance=args.addr_tolerance)
    m, oa, ob = res["matched"], res["only_a"], res["only_b"]
    print(f"cta={args.cta_version}  mango={args.mango_version}  (addr_tol={args.addr_tolerance})\n")
    print(f"  matched (both)        : {len(m):5d}   -> TP candidates")
    print(f"  cta-only              : {len(oa):5d}   -> FP candidates / cta_advantage")
    print(f"  mango-only            : {len(ob):5d}   -> FN candidates")
    if args.show:
        print("\n-- mango-only (FN candidates: triage by Mango's trace) --")
        for r in ob[:args.show]:
            print(f"  {r['sha256'][:12]} {r['sink_func']}@{r['sink_callsite']} "
                  f"src={r['source_class']} rank={r['confidence']}")
        print("\n-- cta-only (FP / advantage) --")
        for r in oa[:args.show]:
            print(f"  {r['sha256'][:12]} {r['sink_func']}@{r['sink_callsite']} "
                  f"src={r['source_class']}")


def cmd_score(args) -> None:
    con = F.connect(args.db)
    res = GT.score(con, args.tool, args.version, args.addr_tolerance)
    GT.print_score(res, args.tool, args.version, args.show)


def _now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def _label_exists(con, finding_id, gt_id, label, version) -> bool:
    return con.execute(
        """SELECT 1 FROM label WHERE label=? AND IFNULL(analyzer_version,'')=IFNULL(?,'')
           AND IFNULL(finding_id,-1)=IFNULL(?,-1) AND IFNULL(gt_id,-1)=IFNULL(?,-1)""",
        (label, version, finding_id, gt_id),
    ).fetchone() is not None


def _insert_label(con, *, finding_id, gt_id, label, analyst, evidence, version) -> bool:
    if _label_exists(con, finding_id, gt_id, label, version):
        return False
    con.execute(
        """INSERT INTO label(finding_id,gt_id,label,analyst,evidence,analyzer_version,ts)
           VALUES(?,?,?,?,?,?,?)""",
        (finding_id, gt_id, label, analyst, evidence, version, _now()),
    )
    return True


def cmd_triage(args) -> None:
    """Optional bookkeeping: persist a verdict so you don't re-investigate the
    same finding. `score` already shows the FN/FP lists -- use this only if you
    want to record what you concluded about one."""
    con = F.connect(args.db)

    if args.action == "fn-seed":
        # Record an FN label for every known bug ctadl missed. Idempotent.
        res = GT.score(con, args.tool, args.version, args.addr_tolerance)
        seeded = 0
        for g in res["missed"]:
            if _insert_label(con, finding_id=None, gt_id=g["id"], label="FN",
                             analyst=args.analyst, version=args.version,
                             evidence=f"missed {g['sink_func']}@{g['sink_callsite']} "
                                      f"[{g['provenance']}]"):
                seeded += 1
        con.commit()
        print(f"seeded {seeded} FN label(s) for {args.tool}={args.version}")
        return

    if args.action == "set":
        if args.finding_id is None and args.gt_id is None:
            sys.exit("triage set: need --finding-id and/or --gt-id")
        ok = _insert_label(con, finding_id=args.finding_id, gt_id=args.gt_id,
                           label=args.label, analyst=args.analyst,
                           evidence=args.evidence, version=args.version)
        con.commit()
        print("labeled" if ok else "duplicate label, skipped")
        return


def cmd_stats(args) -> None:
    con = F.connect(args.db)
    print("== run status ==")
    for row in con.execute(
        "SELECT tool, analyzer_version, status, COUNT(*) n FROM run "
        "GROUP BY tool, analyzer_version, status ORDER BY tool, analyzer_version, status"):
        print(f"  {row['tool']:6} {row['analyzer_version']:22} {row['status']:11} {row['n']}")
    print("\n== findings ==")
    for row in con.execute(
        "SELECT r.tool, r.analyzer_version, COUNT(*) n FROM finding f "
        "JOIN run r ON f.run_id=r.id GROUP BY r.tool, r.analyzer_version "
        "ORDER BY r.tool, r.analyzer_version"):
        print(f"  {row['tool']:6} {row['analyzer_version']:22} {row['n']} findings")
    print("\n== source classes (cta) ==")
    for row in con.execute(
        "SELECT source_class, COUNT(*) n FROM finding f JOIN run r ON f.run_id=r.id "
        "WHERE r.tool='cta' GROUP BY source_class ORDER BY n DESC"):
        print(f"  {row['source_class']:10} {row['n']}")
    gt = con.execute("SELECT provenance, COUNT(*) n FROM ground_truth "
                     "GROUP BY provenance ORDER BY n DESC").fetchall()
    if gt:
        print("\n== ground truth (known bugs) ==")
        for row in gt:
            print(f"  {row['provenance'] or '?':12} {row['n']}")
    lab = con.execute("SELECT label, COUNT(*) n FROM label GROUP BY label "
                      "ORDER BY n DESC").fetchall()
    if lab:
        print("\n== labels ==")
        for row in lab:
            print(f"  {row['label']:16} {row['n']}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run", help="run CTADL over a work-list")
    r.add_argument("--db", required=True)
    r.add_argument("--manifest", help="JSON manifest of {binary,artifact,arch} entries")
    r.add_argument("--binary", action="append", help="extra binary/artifact path(s)")
    r.add_argument("--model", default=str(DEFAULT_MODEL))
    r.add_argument("--ctadl", default=str(DEFAULT_CTADL))
    r.add_argument("--version", default=None, help="override analyzer_version")
    r.add_argument("--timeout", type=int, default=3 * 60 * 60)
    r.add_argument("--force", action="store_true", help="re-run even if cached")
    r.set_defaults(func=cmd_run)

    e = sub.add_parser("eval", help="one shot: run ctadl, load ground truth, score")
    e.add_argument("--db", required=True)
    e.add_argument("--manifest", help="JSON manifest of {binary,artifact,arch} entries")
    e.add_argument("--binary", action="append", help="extra binary/artifact path(s)")
    e.add_argument("--model", default=str(DEFAULT_MODEL))
    e.add_argument("--ctadl", default=str(DEFAULT_CTADL))
    e.add_argument("--version", default=None, help="override analyzer_version")
    e.add_argument("--timeout", type=int, default=3 * 60 * 60)
    e.add_argument("--force", action="store_true", help="re-run even if cached")
    e.add_argument("--mango-out", default=None,
                   help="Mango results dir to load as ground truth")
    e.add_argument("--mango-version", default="mango@local",
                   help="version tag for the loaded Mango run")
    e.add_argument("--gt", default=None, help="ground-truth dataset file (json/jsonl/csv)")
    e.add_argument("--gt-origin", default="dataset", help="origin label for --gt rows")
    # Default tolerance absorbs the Ghidra(CTADL)<->angr(Mango) instruction-
    # attribution jitter: CTADL anchors the sink at the tainted-arg setup insn,
    # Mango at the `call` itself, typically <=~22 bytes apart. (Image-base deltas
    # are already removed -- normalize_ctadl rebases onto angr's load base.) 32 is
    # below the inter-sink spacing in the dense multi-sink fixtures, so it does not
    # cross-match adjacent sinks.
    e.add_argument("--addr-tolerance", type=int, default=32)
    e.add_argument("--show", type=int, default=20, help="rows per FN/FP list")
    e.set_defaults(func=cmd_eval)

    c = sub.add_parser("compare", help="cta-vs-mango 2x2")
    c.add_argument("--db", required=True)
    c.add_argument("--cta-version", required=True)
    c.add_argument("--mango-version", required=True)
    c.add_argument("--addr-tolerance", type=int, default=32,
                   help="abs address window for a match (absorbs lifter attribution jitter; "
                        "base deltas are already removed by normalize_ctadl's rebasing)")
    c.add_argument("--show", type=int, default=0, help="print up to N rows per bucket")
    c.set_defaults(func=cmd_compare)

    s = sub.add_parser("stats", help="status + finding counts")
    s.add_argument("--db", required=True)
    s.set_defaults(func=cmd_stats)

    sc = sub.add_parser("score", help="recall of a run vs ground truth")
    sc.add_argument("--db", required=True)
    sc.add_argument("--tool", default="cta")
    sc.add_argument("--version", required=True)
    sc.add_argument("--addr-tolerance", type=int, default=32)
    sc.add_argument("--show", type=int, default=0)
    sc.set_defaults(func=cmd_score)

    t = sub.add_parser("triage", help="write label rows (FN-seed / manual verdicts)")
    tsub = t.add_subparsers(dest="action", required=True)

    tf = tsub.add_parser("fn-seed", help="record an FN label for each missed known bug")
    tf.add_argument("--db", required=True)
    tf.add_argument("--tool", default="cta")
    tf.add_argument("--version", required=True)
    tf.add_argument("--analyst", default="auto")
    tf.add_argument("--addr-tolerance", type=int, default=32)
    tf.set_defaults(func=cmd_triage)

    ts = tsub.add_parser("set", help="record a manual verdict on a finding / GT row")
    ts.add_argument("--db", required=True)
    ts.add_argument("--finding-id", type=int, default=None)
    ts.add_argument("--gt-id", type=int, default=None)
    ts.add_argument("--label", required=True,
                    help="TP|FP|FN|unknown|cta_advantage|path_divergence")
    ts.add_argument("--analyst", default=None)
    ts.add_argument("--evidence", default=None)
    ts.add_argument("--version", default=None, help="analyzer_version this verdict is about")
    ts.set_defaults(func=cmd_triage)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
