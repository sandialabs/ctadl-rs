#!/usr/bin/env python3
"""One binary, one condition: time and measure `ctadl index`.

Two phases:

  import (shared, NOT measured)  Ghidra lift of the binary into a pristine store
      under `<outdir>/imports/<label>`. Done once and reused by both conditions, so
      the lift -- which is the same bytes either way and dominates `ctadl go` on
      small binaries -- cannot leak into the comparison.

  index (measured)               the store template is copied to a scratch dir and
      `ctadl index` runs on the copy. This is the Ascent Datalog fixpoint, the only
      thing the `#[ds(...)]` directive touches.

Memory is macOS **physical footprint**, never RSS (macOS compresses cold pages, so
RSS undercounts badly). Peak comes from `/usr/bin/time -l`'s `peak memory footprint`
line, which is the kernel's own high-water mark; a 1 s poll of `footprint -p` runs
alongside only to enforce the cap and to record the trajectory (and supplies the peak
if the job is killed before `time` can report).

Wall time likewise comes from `/usr/bin/time -l`'s `real` line, not from the polling
loop: the loop's wakeup granularity would quantize any run shorter than a poll
interval (an 0.01 s index would be recorded as a 1 s one). The polled wall is the
fallback for killed jobs.

Usage: run_one.py <condition> <label> <outdir>
Env:   CTADL_HYBRID, CTADL_CONTROL, MODEL, GHIDRA_HOME,
       JOB_TIMEOUT (s), JOB_MEMCAP_GB, SCRATCH
"""
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent

CTADL = {
    "hybrid": os.environ.get("CTADL_HYBRID", str(HERE / "bin/ctadl-hybrid")),
    "control": os.environ.get("CTADL_CONTROL", str(HERE / "bin/ctadl-control")),
}
MODEL = os.environ.get(
    "MODEL",
    "/Users/dbueno/proj/ct-head-to-head-hybrid-locals/firmware-eval/models/cmdi-firmware.json5",
)
GHIDRA_HOME = os.environ.get(
    "GHIDRA_HOME",
    "/nix/store/30m9yjgksz2971r3x1gmzjcigfj538bm-ghidra-12.0.4/lib/ghidra",
)
IMPORT_TIMEOUT_S = int(os.environ.get("IMPORT_TIMEOUT", "1200"))
TIMEOUT_S = int(os.environ.get("JOB_TIMEOUT", "900"))
MEMCAP_B = int(os.environ.get("JOB_MEMCAP_GB", "64")) * 1024**3
POLL = float(os.environ.get("POLL", "1.0"))
SCRATCH = os.environ.get("SCRATCH", "/private/tmp/hybrid-locals-scratch")


def fp_bytes(pid):
    """phys_footprint of one pid, in bytes. `$2` -- `$NF` is the literal 'B'."""
    try:
        out = subprocess.run(
            ["footprint", "-p", str(pid), "-f", "bytes"],
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout
    except Exception:
        return None
    for ln in out.splitlines():
        if "phys_footprint:" in ln:
            v = ln.split()[1]
            return int(v) if v.isdigit() else None
    return None


def group_fp_bytes(pgid):
    """Sum phys_footprint over the whole process group (Ghidra forks children)."""
    try:
        out = subprocess.run(
            ["ps", "-Ao", "pid=,pgid="], capture_output=True, text=True, timeout=10
        ).stdout
    except Exception:
        return None
    total, any_ok = 0, False
    for line in out.splitlines():
        p = line.split()
        if len(p) == 2 and p[1] == str(pgid):
            b = fp_bytes(p[0])
            if b is not None:
                total += b
                any_ok = True
    return total if any_ok else 0


TIME_PEAK_RE = re.compile(r"^\s*(\d+)\s+peak memory footprint", re.M)
TIME_REAL_RE = re.compile(r"^\s*([\d.]+)\s+real\s", re.M)


def guarded_run(cmd, logpath, timeout_s, memcap_b, env, trace=False):
    """Run cmd under /usr/bin/time -l with a wall + footprint guard.

    Returns (status, wall_s, peak_bytes, exit_code, trajectory)
    status: done | timeout | oom
    """
    full = ["/usr/bin/time", "-l"] + cmd
    logf = open(logpath, "wb")
    start = time.time()
    proc = subprocess.Popen(
        full, stdout=logf, stderr=subprocess.STDOUT, start_new_session=True, env=env
    )
    pgid = proc.pid
    polled_peak = 0
    status = "done"
    traj = []
    try:
        while True:
            rc = proc.poll()
            if rc is not None:
                break
            el = time.time() - start
            if el > timeout_s:
                status = "timeout"
                break
            b = group_fp_bytes(pgid)
            if b:
                polled_peak = max(polled_peak, b)
                if trace:
                    traj.append([round(el, 1), b])
                if b > memcap_b:
                    status = "oom"
                    break
            time.sleep(POLL)
    finally:
        if status != "done":
            try:
                os.killpg(pgid, signal.SIGKILL)
            except Exception:
                pass
            try:
                proc.wait(timeout=20)
            except Exception:
                pass
        logf.close()
    wall = time.time() - start
    exit_code = proc.returncode

    peak = polled_peak
    peak_src = "polled"
    if status == "done":
        try:
            txt = Path(logpath).read_text(errors="replace")
            m = TIME_PEAK_RE.search(txt)
            if m:
                peak = max(int(m.group(1)), polled_peak)
                peak_src = "time-l"
            m = TIME_REAL_RE.search(txt)
            if m:
                wall = float(m.group(1))
        except Exception:
            pass
    return status, wall, peak, exit_code, peak_src, traj


def ensure_import(label, binary, outdir, env):
    """Lift the binary once into a pristine store template. Returns (ok, info)."""
    imp = outdir / "imports" / label
    marker = imp / ".import-ok.json"
    if marker.exists():
        return True, json.loads(marker.read_text())
    if imp.exists():
        shutil.rmtree(imp, ignore_errors=True)
    imp.mkdir(parents=True, exist_ok=True)
    store = imp / "s"
    logp = imp / "import.log"
    cmd = [
        CTADL["hybrid"],  # import is frontend-only; the two builds share it byte-for-byte
        "--store",
        str(store),
        "import",
        "-n",
        label,
        "-l",
        "pcode",
        binary,
    ]
    st, wall, peak, rc, psrc, _ = guarded_run(
        cmd, logp, IMPORT_TIMEOUT_S, MEMCAP_B, env
    )
    ok = st == "done" and rc == 0
    info = {
        "status": st if not ok else "ok",
        "exit_code": rc,
        "wall_s": round(wall, 2),
        "peak_fp_mb": round(peak / 1024**2, 1),
        "store_bytes": dir_size(store) if ok else 0,
    }
    if ok:
        marker.write_text(json.dumps(info))
    else:
        (imp / ".import-failed.json").write_text(json.dumps(info))
    return ok, info


def dir_size(p):
    total = 0
    for root, _dirs, files in os.walk(p):
        for f in files:
            try:
                total += os.path.getsize(os.path.join(root, f))
            except OSError:
                pass
    return total


STAT_RES = {
    "locals_rows": re.compile(r"relation increase: locals: (\d+), (\d+) formals"),
    "assign_like": re.compile(r"relation increase: assign_like: [\d.]+ \((\d+)/(\d+)\)"),
    "summary": re.compile(r"relation increase: summary: [\d.]+ \((\d+)/(\d+)\)"),
}


FINGERPRINT_MAX_ROWS = int(os.environ.get("FINGERPRINT_MAX_ROWS", "3000000"))


def fingerprint_index(work, label):
    """Canonical fingerprint of the index this run produced, for equivalence checking.

    The two builds serialize the same relations in different physical orders, so the
    files are not byte-identical even when the analysis result is. Per relation we
    record the row count (parquet metadata only -- no data read) and, below a row
    threshold, a sha256 over the sorted rows, which IS order-independent.

    Runs after the measured region, so its cost never lands in the numbers.
    """
    idx = Path(work) / "projects" / label / "index"
    out = {}
    if not idx.exists():
        return out
    try:
        import pyarrow.parquet as pq
    except Exception:
        pq = None
    import hashlib

    for f in sorted(idx.glob("*.parquet")):
        rec = {}
        try:
            if pq is None:
                raise RuntimeError("no pyarrow")
            nrows = pq.ParquetFile(f).metadata.num_rows
            rec["rows"] = nrows
            if nrows <= FINGERPRINT_MAX_ROWS:
                t = pq.read_table(f)
                cols = [c.to_pylist() for c in t.columns]
                rows = sorted(map(str, zip(*cols))) if cols else []
                rec["sha"] = hashlib.sha256("\n".join(rows).encode()).hexdigest()[:16]
        except Exception as e:
            rec["error"] = str(e)[:120]
        out[f.name] = rec
    return out


def parse_stats(logpath):
    out = {}
    try:
        txt = Path(logpath).read_text(errors="replace")
    except Exception:
        return out
    m = STAT_RES["locals_rows"].search(txt)
    if m:
        out["locals_rows"] = int(m.group(1))
        out["formals"] = int(m.group(2))
    m = STAT_RES["assign_like"].search(txt)
    if m:
        out["assign_like_rows"] = int(m.group(1))
        out["initial_assign"] = int(m.group(2))
    m = STAT_RES["summary"].search(txt)
    if m:
        out["summary_rows"] = int(m.group(1))
        out["num_functions"] = int(m.group(2))
    return out


def main():
    condition, label, outdir = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    trace = os.environ.get("TRACE") == "1"
    stats_log = os.environ.get("STATS_LOG") == "1"
    resdir = outdir / "results" / condition
    resdir.mkdir(parents=True, exist_ok=True)
    resfile = resdir / f"{label}.json"
    if resfile.exists() and os.environ.get("FORCE") != "1":
        print(f"SKIP {condition} {label}")
        return

    corpus_path = Path(os.environ.get("CORPUS", HERE / "corpus.json"))
    corpus = json.loads(corpus_path.read_text())["corpus"]
    ent = next(e for e in corpus if e["label"] == label)

    env = dict(os.environ, GHIDRA_HOME=GHIDRA_HOME)
    env.pop("RUST_LOG", None)
    ok, imp_info = ensure_import(label, ent["binary"], outdir, env)
    if not ok:
        resfile.write_text(
            json.dumps(
                {
                    "label": label,
                    "condition": condition,
                    "status": "import_failed",
                    "import": imp_info,
                },
                indent=1,
            )
        )
        print(f"IMPORT_FAIL {label} {imp_info['status']}")
        return

    # measured phase: index a fresh copy of the pristine store
    os.makedirs(SCRATCH, exist_ok=True)
    work = Path(SCRATCH) / f"{condition}_{label}"
    if work.exists():
        shutil.rmtree(work, ignore_errors=True)
    shutil.copytree(outdir / "imports" / label / "s", work)

    logp = resdir / f"{label}.index.log"
    ienv = dict(env)
    # RUST_LOG is set identically for both conditions when stats are wanted, so the
    # logging cost is common-mode; the timing campaign runs without it.
    if stats_log:
        ienv["RUST_LOG"] = "ctadl_ascent::index_engine=debug"
    cmd = [
        CTADL[condition],
        "--store",
        str(work),
        "index",
        label,
        "--models",
        MODEL,
    ]
    st, wall, peak, rc, psrc, traj = guarded_run(
        cmd, logp, TIMEOUT_S, MEMCAP_B, ienv, trace=trace
    )
    status = st if st != "done" else ("ok" if rc == 0 else "crash")

    rec = {
        "label": label,
        "condition": condition,
        "ctadl": CTADL[condition],
        "binary": ent["binary"],
        "sha256": ent["sha256"],
        "size": ent["size"],
        "bin_mb": ent["bin_mb"],
        "status": status,
        "exit_code": rc,
        "wall_s": round(wall, 3),
        "peak_fp_bytes": peak,
        "peak_fp_mb": round(peak / 1024**2, 1),
        "peak_source": psrc,
        "index_store_bytes": dir_size(work) if status == "ok" else None,
        "import": imp_info,
        "stats": parse_stats(logp),
        "fingerprint": fingerprint_index(work, label) if status == "ok" else {},
    }
    if trace:
        rec["trajectory"] = traj
    resfile.write_text(json.dumps(rec, indent=1))
    shutil.rmtree(work, ignore_errors=True)
    print(
        f"{status.upper():>9} {condition:<7} {label:<38} "
        f"{rec['wall_s']:8.1f}s {rec['peak_fp_mb']:9.1f}MB"
    )


if __name__ == "__main__":
    main()
