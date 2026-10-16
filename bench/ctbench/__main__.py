"""ctbench: continuous ctadl benchmarking.

    python3 -m ctbench daemon            run the scheduler (what launchd starts)
    python3 -m ctbench status            what is queued, running, and done
    python3 -m ctbench report [SHA]      regressions/improvements of a commit vs. its predecessors
    python3 -m ctbench enqueue SHA...    queue specific commits of the tracked repo
    python3 -m ctbench serve             the results web page

All take --config (default: bench.toml next to this package).
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent


def load_config(path: Path) -> dict:
    cfg = tomllib.loads(path.read_text())
    for k, v in cfg["paths"].items():
        cfg["paths"][k] = Path(v).expanduser()
    return cfg


def load_inputs(cfg: dict) -> dict:
    """Build (or reuse) the bench flake's pinned inputs and read where everything is."""
    out = cfg["paths"]["root"] / "inputs"
    bench_repo = cfg["paths"]["bench_repo"]
    subprocess.run(["nix", "build", f"git+file://{bench_repo}?dir=bench#inputs", "--out-link", str(out)],
                   check=True, stdout=subprocess.DEVNULL)
    return json.loads(out.read_text())


def main() -> None:
    ap = argparse.ArgumentParser(prog="ctbench")
    ap.add_argument("--config", type=Path, default=HERE / "bench.toml")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("daemon")
    sub.add_parser("status")
    rp = sub.add_parser("report")
    rp.add_argument("sha", nargs="?")
    rp.add_argument("--window", type=int, default=5)
    ep = sub.add_parser("enqueue")
    ep.add_argument("shas", nargs="+")
    sp = sub.add_parser("serve")
    sp.add_argument("--port", type=int, default=8787)
    sp.add_argument("--host", default="127.0.0.1")
    args = ap.parse_args()
    cfg = load_config(args.config)

    if args.cmd == "daemon":
        from .runner import Daemon
        Daemon(cfg, load_inputs(cfg)).run()
    elif args.cmd == "enqueue":
        from .runner import Daemon
        d = Daemon(cfg, load_inputs(cfg))
        for sha in args.shas:
            full = d.git_out("rev-parse", sha)
            print(full, "queued" if d.add_commit(full) else "already known")
    elif args.cmd == "status":
        from . import report
        report.status(cfg)
    elif args.cmd == "report":
        from . import report
        report.report(cfg, args.sha, args.window)
    elif args.cmd == "serve":
        from . import web
        web.serve(cfg, args.host, args.port)


if __name__ == "__main__":
    sys.exit(main())
