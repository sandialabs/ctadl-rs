#!/usr/bin/env python3
"""Run ONE engine over ONE firmware binary, three phases, and record the metrics.

    python3 run_one.py <engine> <label> <binary>      engine = rs | souffle

Both engines run the same three phases (import -> index -> query) with the same
shared model set and their own defaults suppressed, so the numbers below differ
only by engine:

    import_bytes    on-disk size of the imported program
    index_bytes     on-disk size of the index, measured AFTER index and BEFORE
                    query (ctadl-souffle writes query results back into
                    ctadlir.db, which would otherwise inflate it)
    summaries       rows in the function-summary relation - the compositional
                    formal->formal flows that are CTADL's core data structure.
                    ctadl-rs: `summary.parquet`; ctadl-souffle: `SummaryFlow`.
                    Same shape (function, dst formal+path, src formal+path).
    sarif_paths     `C0001` tainted-path results in the SARIF

Each phase is guarded by a wall timeout and a physical-footprint cap. On macOS
`ps rss` undercounts compressed memory, so peak memory comes from `footprint(1)`
summed over the job's process group - the same guard the large-scale campaign
uses.

Writes results/<label>/<engine>.json and prints one status line.
"""

import json
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODELS = HERE / "models"

IMPORT_TIMEOUT_S = int(os.environ.get("IMPORT_TIMEOUT_S", 3600))
INDEX_TIMEOUT_S = int(os.environ.get("INDEX_TIMEOUT_S", 14400))
QUERY_TIMEOUT_S = int(os.environ.get("QUERY_TIMEOUT_S", 7200))
MEMCAP_B = int(os.environ.get("MEMCAP_GB", "64")) * 1024**3
KEEP_ARTIFACTS = os.environ.get("KEEP_ARTIFACTS") == "1"
SOUFFLE_JOBS = os.environ.get("SOUFFLE_JOBS", "10")
POLL_S = 5.0


# --------------------------------------------------------------------------
# guarded execution
# --------------------------------------------------------------------------
def group_footprint_bytes(pgid):
    """Sum phys_footprint over every live pid in the process group."""
    try:
        out = subprocess.run(
            ["ps", "-Ao", "pid=,pgid="], capture_output=True, text=True, timeout=10
        ).stdout
    except Exception:
        return None
    pids = [
        p.split()[0]
        for p in out.splitlines()
        if len(p.split()) == 2 and p.split()[1] == str(pgid)
    ]
    total = 0
    for pid in pids:
        try:
            fo = subprocess.run(
                ["footprint", "-p", pid, "-f", "bytes"],
                capture_output=True,
                text=True,
                timeout=10,
            ).stdout
            for ln in fo.splitlines():
                if "phys_footprint:" in ln:
                    val = ln.split()[1]
                    if val.isdigit():
                        total += int(val)
                    break
        except Exception:
            pass
    return total


def run_phase(name, cmd, logpath, timeout_s, env=None, cwd=None):
    """Run one phase under the time + memory guard. Returns a dict of results."""
    print(f"    [{name}] {' '.join(str(c) for c in cmd)}", flush=True)
    logf = open(logpath, "wb")
    start = time.time()
    proc = subprocess.Popen(
        [str(c) for c in cmd],
        stdout=logf,
        stderr=subprocess.STDOUT,
        start_new_session=True,
        env=env or os.environ.copy(),
        cwd=cwd,
    )
    pgid = proc.pid
    peak = 0
    status = None
    exit_code = None
    try:
        while True:
            rc = proc.poll()
            if rc is not None:
                exit_code = rc
                break
            if time.time() - start > timeout_s:
                status = "timeout"
                break
            fp = group_footprint_bytes(pgid)
            if fp is not None:
                peak = max(peak, fp)
                if fp > MEMCAP_B:
                    status = "oom"
                    break
            time.sleep(POLL_S)
    finally:
        if status in ("timeout", "oom"):
            try:
                os.killpg(pgid, signal.SIGKILL)
            except Exception:
                pass
            try:
                proc.wait(timeout=15)
            except Exception:
                pass
        logf.close()
    wall = time.time() - start
    if status is None:
        status = "ok" if exit_code == 0 else "crash"
    print(f"    [{name}] {status} {wall:.1f}s peak={peak / 1024**3:.2f}G", flush=True)
    return {
        "status": status,
        "exit_code": exit_code,
        "wall_s": round(wall, 1),
        "peak_fp_b": peak,
    }


def dir_bytes(path: Path, exclude_suffixes=(".log",)):
    """Bytes on disk under `path`, skipping console logs (not analysis data)."""
    if path.is_file():
        return path.stat().st_size
    total = 0
    for p in path.rglob("*"):
        if p.is_file() and not any(p.name.endswith(s) for s in exclude_suffixes):
            total += p.stat().st_size
    return total


# --------------------------------------------------------------------------
# metric extraction
# --------------------------------------------------------------------------
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


def count_sarif_paths(sarif_path: Path):
    try:
        data = json.loads(sarif_path.read_text())
    except Exception:
        return None
    return sum(
        1
        for run in data.get("runs", [])
        for r in run.get("results", [])
        if is_path_result(r)
    )


def count_rs_summaries(index_dir: Path):
    try:
        import pyarrow.parquet as pq

        return pq.read_table(index_dir / "summary.parquet").num_rows
    except Exception as e:
        print(f"    ! summary count failed: {e}", file=sys.stderr)
        return None


def count_souffle_summaries(db_path: Path):
    try:
        con = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        n = con.execute("select count(*) from SummaryFlow").fetchone()[0]
        con.close()
        return n
    except Exception as e:
        print(f"    ! SummaryFlow count failed: {e}", file=sys.stderr)
        return None


# --------------------------------------------------------------------------
# the two engines
# --------------------------------------------------------------------------
def run_rs(label, binary, outdir):
    ctadl = os.environ["CTADL"]
    store = outdir / "store"
    sarif = outdir / "results.sarif"
    res = {}

    res["import"] = run_phase(
        "rs/import",
        [ctadl, "--store", store, "import", "-n", label, "-l", "pcode", binary],
        outdir / "import.log",
        IMPORT_TIMEOUT_S,
    )
    if res["import"]["status"] != "ok":
        return res, {}
    import_dir = store / "imports" / label
    sizes = {"import_bytes": dir_bytes(import_dir)}

    res["index"] = run_phase(
        "rs/index",
        [
            ctadl, "--store", store, "index",
            "--no-default-models",
            "--models", MODELS / "shared-index.rs.json",
            label,
        ],
        outdir / "index.log",
        INDEX_TIMEOUT_S,
    )
    if res["index"]["status"] != "ok":
        return res, sizes
    index_dir = store / "projects" / label / "index"
    sizes["index_bytes"] = dir_bytes(index_dir)
    sizes["summaries"] = count_rs_summaries(index_dir)

    res["query"] = run_phase(
        "rs/query",
        [
            ctadl, "--store", store, "query",
            "--models", MODELS / "shared-query.rs.json",
            "-o", sarif,
            label,
        ],
        outdir / "query.log",
        QUERY_TIMEOUT_S,
    )
    if res["query"]["status"] == "ok":
        sizes["sarif_paths"] = count_sarif_paths(sarif)
    return res, sizes


def run_souffle(label, binary, outdir):
    ctadl = os.environ["CTADL_SOUFFLE"]
    imp = outdir / "import"
    sarif = outdir / "results.sarif"
    res = {}

    res["import"] = run_phase(
        "sf/import",
        [ctadl, "import", "pcode", binary, "-o", imp, "-f"],
        outdir / "import.log",
        IMPORT_TIMEOUT_S,
    )
    if res["import"]["status"] != "ok":
        return res, {}
    sizes = {"import_bytes": dir_bytes(imp)}

    res["index"] = run_phase(
        "sf/index",
        [
            ctadl, "--directory", imp, "index",
            "-j", SOUFFLE_JOBS, "-f",
            "--models", MODELS / "shared-index.souffle.json",
        ],
        outdir / "index.log",
        INDEX_TIMEOUT_S,
    )
    db = imp / "ctadlir.db"
    if res["index"]["status"] != "ok":
        return res, sizes
    # measured before query: ctadl-souffle writes query results back into this db
    sizes["index_bytes"] = dir_bytes(db)
    sizes["summaries"] = count_souffle_summaries(db)

    res["query"] = run_phase(
        "sf/query",
        [
            ctadl, "--directory", imp, "query",
            str(MODELS / "shared-query.souffle.json"),
            "-j", SOUFFLE_JOBS,
            "--format", "sarif",
            "-o", sarif,
        ],
        outdir / "query.log",
        QUERY_TIMEOUT_S,
    )
    if res["query"]["status"] == "ok":
        sizes["sarif_paths"] = count_sarif_paths(sarif)
    return res, sizes


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    engine, label, binary = sys.argv[1], sys.argv[2], sys.argv[3]
    if "CTADL" not in os.environ or "CTADL_SOUFFLE" not in os.environ:
        sys.exit("source env.sh first (CTADL / CTADL_SOUFFLE / GHIDRA_HOME)")

    outdir = HERE / "results" / label / engine
    if outdir.exists():
        shutil.rmtree(outdir)
    outdir.mkdir(parents=True)

    print(f"[{label}] {engine}: {binary}", flush=True)
    t0 = time.time()
    runner = run_rs if engine == "rs" else run_souffle
    phases, sizes = runner(label, binary, outdir)

    status = "ok"
    for name in ("import", "index", "query"):
        st = phases.get(name, {}).get("status")
        if st is None:
            status = f"missing:{name}"
            break
        if st != "ok":
            status = f"{st}:{name}"
            break

    if not KEEP_ARTIFACTS:
        # The store/import trees are the bulk (a souffle ctadlir.db for one
        # firmware binary is ~200 MB). Their sizes are the measurement and are
        # already recorded; what has to survive is the SARIF, the logs, and this
        # file. Set KEEP_ARTIFACTS=1 to keep them for a post-mortem.
        for d in (outdir / "store", outdir / "import"):
            shutil.rmtree(d, ignore_errors=True)

    result = {
        "engine": engine,
        "label": label,
        "binary": binary,
        "binary_bytes": os.path.getsize(binary),
        "status": status,
        "total_wall_s": round(time.time() - t0, 1),
        "phases": phases,
        **sizes,
    }
    (HERE / "results" / label / f"{engine}.json").write_text(
        json.dumps(result, indent=2) + "\n"
    )
    print(
        f"[{label}] {engine}: {status} "
        f"import={sizes.get('import_bytes')} index={sizes.get('index_bytes')} "
        f"summaries={sizes.get('summaries')} paths={sizes.get('sarif_paths')} "
        f"total={result['total_wall_s']}s",
        flush=True,
    )


if __name__ == "__main__":
    main()
