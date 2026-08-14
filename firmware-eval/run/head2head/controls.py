#!/usr/bin/env python3
"""Control experiment: are both engines configured correctly? - DO-NOT-MERGE

Before reading anything into the firmware numbers, this rules out the boring
explanation for a low count - that one engine was handed a model set it cannot
use. It runs both engines, with the same shared models as the firmware campaign,
over Operation Mango's synthetic test binaries, whose flows are known and small
enough to check by hand.

The set is chosen to separate two patterns:

  direct        the tainted value reaches the sink with no library call in
                between (`system(argv[1])`)            - nested, simple
  via-builder   the tainted value reaches the sink through a string builder
                (`sprintf(buf, "...%s", tainted); system(buf)`), which is the
                pattern real firmware command injection almost always has
                                                       - heap, wrapper, off_shoot

Writes results/controls.json.
"""

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODELS = HERE / "models"
BINDIR = Path("/Users/dbueno/proj/operation-mango-public/package/tests/binaries")

CONTROLS = [
    ("nested", "direct"),
    ("simple", "direct"),
    ("heap", "via-builder"),
    ("wrapper", "via-builder"),
    ("off_shoot", "via-builder"),
]


def count_paths(sarif: Path):
    """Count real path results.

    ctadl-rs emits one `C0001` result with `kind: open` when it finds NO flow -
    it does not prove absence, so it says so rather than staying silent. That
    placeholder must not be counted as a finding.
    """
    try:
        data = json.loads(sarif.read_text())
    except Exception:
        return None
    return sum(
        1
        for run in data.get("runs", [])
        for r in run.get("results", [])
        if str(r.get("ruleId", "")).split(".")[0] == "C0001"
        and r.get("kind") != "open"
        and r.get("level") != "none"
    )


def quiet(cmd, **kw):
    return subprocess.run(
        [str(c) for c in cmd], capture_output=True, text=True, **kw
    ).returncode


def souffle_paths(name, binary, tmp: Path):
    ct = os.environ["CTADL_SOUFFLE"]
    imp = tmp / "sf"
    sarif = tmp / "sf.sarif"
    if quiet([ct, "import", "pcode", binary, "-o", imp, "-f"]):
        return None
    if quiet([ct, "--directory", imp, "index", "-j10", "-f",
              "--models", MODELS / "shared-index.souffle.json"]):
        return None
    if quiet([ct, "--directory", imp, "query",
              MODELS / "shared-query.souffle.json",
              "-j10", "--format", "sarif", "-o", sarif]):
        return None
    return count_paths(sarif)


def rs_paths(name, binary, tmp: Path):
    ct = os.environ["CTADL"]
    store = tmp / "rs"
    sarif = tmp / "rs.sarif"
    if quiet([ct, "--store", store, "import", "-n", name, "-l", "pcode", binary]):
        return None
    if quiet([ct, "--store", store, "index", "--no-default-models",
              "--models", MODELS / "shared-index.rs.json", name]):
        return None
    if quiet([ct, "--store", store, "query",
              "--models", MODELS / "shared-query.rs.json", "-o", sarif, name]):
        return None
    return count_paths(sarif)


def main():
    if "CTADL" not in os.environ or "CTADL_SOUFFLE" not in os.environ:
        sys.exit("source env.sh first")
    out = []
    for name, pattern in CONTROLS:
        binary = BINDIR / name / "program"
        tmp = Path("/tmp") / f"h2h_ctl_{name}"
        if tmp.exists():
            shutil.rmtree(tmp)
        tmp.mkdir(parents=True)
        sf = souffle_paths(name, binary, tmp)
        rs = rs_paths(name, binary, tmp)
        print(f"{name:<12} {pattern:<12} souffle={sf}  rs={rs}", flush=True)
        out.append(
            {"binary": name, "pattern": pattern, "souffle_paths": sf, "rs_paths": rs}
        )
        shutil.rmtree(tmp, ignore_errors=True)
    (HERE / "results").mkdir(exist_ok=True)
    (HERE / "results" / "controls.json").write_text(json.dumps(out, indent=2) + "\n")


if __name__ == "__main__":
    main()
