#!/usr/bin/env python3
"""Run CTADL on ONE binary, guarded by wall-timeout + macOS physical-footprint cap.

Writes <outdir>/results/<sha>.json and prints a one-line status.
Classifies: ok | no_findings | timeout | oom | crash  (mirrors Mango's OOM/Error cols).
"""
import os, sys, json, time, signal, subprocess, shutil, tempfile
from pathlib import Path

HARNESS = "/Users/dbueno/proj/ct-firmware-eval/firmware-eval/harness"
sys.path.insert(0, HARNESS)
import normalize_ctadl as NC  # noqa: E402

CTADL = "/Users/dbueno/proj/ct-firmware-eval/target/release/ctadl"
MODEL = "/Users/dbueno/proj/ct-firmware-eval/firmware-eval/models/cmdi-firmware.json5"
GHIDRA_HOME = "/nix/store/30m9yjgksz2971r3x1gmzjcigfj538bm-ghidra-12.0.4/lib/ghidra"

TIMEOUT_S = int(os.environ.get("JOB_TIMEOUT", "600"))
MEMCAP_B = int(os.environ.get("JOB_MEMCAP_GB", "24")) * 1024**3
POLL = 3.0

def group_footprint_bytes(pgid):
    """Sum phys_footprint over every live pid in the process group."""
    try:
        out = subprocess.run(["ps", "-Ao", "pid=,pgid="], capture_output=True,
                             text=True, timeout=10).stdout
    except Exception:
        return None
    pids = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1] == str(pgid):
            pids.append(parts[0])
    if not pids:
        return 0
    total = 0
    for pid in pids:
        try:
            fo = subprocess.run(["footprint", "-p", pid, "-f", "bytes"],
                                capture_output=True, text=True, timeout=10).stdout
            for ln in fo.splitlines():
                if "phys_footprint:" in ln:
                    val = ln.split()[1]  # $2, the integer (NOT $NF which is "B")
                    if val.isdigit():
                        total += int(val)
                    break
        except Exception:
            pass
    return total

def main():
    sha = sys.argv[1]; binpath = sys.argv[2]; outdir = Path(sys.argv[3])
    resdir = outdir / "results"; resdir.mkdir(parents=True, exist_ok=True)
    resfile = resdir / f"{sha}.json"
    if resfile.exists():
        print(f"SKIP {sha[:12]}"); return

    store = Path(tempfile.mkdtemp(prefix=f"cta_{sha[:8]}_", dir=os.environ.get("CAMPAIGN_TMP", "/private/tmp")))
    sarif = store / "results.sarif"
    env = dict(os.environ, GHIDRA_HOME=GHIDRA_HOME)
    cmd = [CTADL, "go", "-n", f"j{sha[:10]}", "-l", "pcode",
           "--store", str(store / "s"), "--models", MODEL,
           "-o", str(sarif), binpath]

    start = time.time()
    peak = 0
    status = None
    exit_code = None
    logf = open(store / "run.log", "wb")
    proc = subprocess.Popen(cmd, stdout=logf, stderr=subprocess.STDOUT,
                            start_new_session=True, env=env)
    pgid = proc.pid  # start_new_session -> pgid == pid
    try:
        while True:
            rc = proc.poll()
            if rc is not None:
                exit_code = rc
                break
            elapsed = time.time() - start
            if elapsed > TIMEOUT_S:
                status = "timeout"
                break
            fp = group_footprint_bytes(pgid)
            if fp is not None:
                if fp > peak:
                    peak = fp
                if fp > MEMCAP_B:
                    status = "oom"
                    break
            time.sleep(POLL)
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
    findings = []
    nfind = 0
    if status is None:  # process exited on its own
        if exit_code != 0:
            status = "crash"
        else:
            try:
                data = json.loads(sarif.read_text())
                fs = NC.parse_sarif(data, sha)
                nfind = len(fs)
                findings = [{"sink_func": f.sink_func, "source_class": f.source_class,
                             "sink_callsite": f.sink_callsite} for f in fs]
                status = "ok" if nfind > 0 else "no_findings"
            except Exception as e:
                status = "crash"
                exit_code = f"parse:{e}"[:80]

    result = {
        "sha256": sha, "binary": binpath, "status": status,
        "exit_code": exit_code, "wall_s": round(wall, 1),
        "peak_fp_mb": round(peak / 1024**2, 1), "nfind": nfind,
        "findings": findings,
    }
    # tail of log for crash triage
    if status == "crash":
        try:
            result["log_tail"] = (store / "run.log").read_text(errors="replace")[-1500:]
        except Exception:
            pass
    tmp = resfile.with_suffix(".tmp")
    tmp.write_text(json.dumps(result))
    tmp.rename(resfile)
    shutil.rmtree(store, ignore_errors=True)
    print(f"{status.upper():11s} {sha[:12]} nfind={nfind:<4d} wall={wall:6.1f}s "
          f"peak={peak/1024**3:5.2f}G {os.path.basename(binpath)}")

if __name__ == "__main__":
    main()
