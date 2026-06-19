"""Normalize Operation Mango output (`cmdi_results.json`) into Finding rows.

Mango result schema (per binary):
  { sha256, name, path, cfg_time, vra_time, mango_time, has_sinks, sinks,
    error,                       # None | "timeout" | "OOMKILLED" | "early_termination" | ...
    closures: [ {
       trace:[{function,string,ins_addr}...],
       sink:{function,string,ins_addr},
       depth, rank, reachable_from_main, sanitized,
       inputs:{ likely:{<src>:[sites...]}, possibly:{<src>:[sites...]} }
    } ... ] }

Usage:
  python normalize_mango.py --db results.db --version <mango image digest> PATH...
  (PATH = a cmdi_results.json file, or a dir scanned recursively for them)
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Optional

import findings as F

# Mango `error` field  ->  run status enum
_ERROR_STATUS = {
    None: "ok",
    "": "ok",
    "timeout": "timeout",
    "potential_timeout": "timeout",
    "OOMKILLED": "oom",
    "early_termination": "crash",
}


def _pick_source(inputs: dict) -> tuple[str, list]:
    """From a closure's `inputs`, choose the strongest source class and collect
    all source sites. Prefer `likely` over `possibly`; prefer a specific class
    over UNKNOWN.

    Mango's `likely`/`possibly` buckets are a *list* of source tokens, e.g.
    `["ARGV", "stdin", "accept(fd: 3)@0x..._6_6", "/etc/passwd"]`; older/other
    builds emit a `{src_name: [sites]}` dict. Handle both."""
    sites: list = []
    best = "UNKNOWN"
    for bucket in ("likely", "possibly"):
        group = (inputs or {}).get(bucket)
        if isinstance(group, dict):
            tokens = list(group.keys())
            for v in group.values():
                sites.extend(v or [])
        elif isinstance(group, list):
            tokens = [str(t) for t in group]
            sites.extend(tokens)
        else:
            tokens = []
        for tok in tokens:
            cls = F.classify_source(tok)
            if best == "UNKNOWN" and cls != "UNKNOWN":
                best = cls
        if best != "UNKNOWN" and bucket == "likely":
            break
    return best, sites


def parse_result(obj: dict) -> tuple[F.RunInfo, list[F.Finding]]:
    sha = obj.get("sha256") or ""
    status = _ERROR_STATUS.get(obj.get("error"), "crash" if obj.get("error") else "ok")
    run = F.RunInfo(
        sha256=sha,
        tool="mango",
        analyzer_version="",  # filled in by caller
        status=status,
        cfg_time=obj.get("cfg_time"),
        taint_time=obj.get("mango_time"),
        unsupported_reason=obj.get("error") if status == "crash" else None,
    )

    out: list[F.Finding] = []
    for cl in obj.get("closures", []) or []:
        sink = cl.get("sink", {}) or {}
        src_class, src_sites = _pick_source(cl.get("inputs", {}))
        out.append(F.Finding(
            binary_sha256=sha,
            tool="mango",
            sink_func=sink.get("function"),
            sink_callsite=F.parse_addr(sink.get("ins_addr")),
            sink_site_kind="address",
            sink_argpos=0,  # cmdi: command is Mango param 1 -> 0-based arg 0
            source_class=src_class,
            source_sites=src_sites,
            reach_from_main=cl.get("reachable_from_main"),
            sanitized=cl.get("sanitized"),
            confidence=cl.get("rank"),
            raw_path={"trace": cl.get("trace"), "depth": cl.get("depth"),
                      "sink_string": sink.get("string")},
        ))
    return run, out


def iter_result_files(paths: list[str]):
    for p in paths:
        pp = Path(p)
        if pp.is_dir():
            yield from sorted(pp.rglob("cmdi_results.json"))
        else:
            yield pp


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", required=True)
    ap.add_argument("--version", required=True,
                    help="mango analyzer_version tag (use the pinned image digest)")
    ap.add_argument("--arch", default=None)
    ap.add_argument("paths", nargs="+", help="cmdi_results.json file(s) or dir(s)")
    args = ap.parse_args()

    con = F.connect(args.db)
    n_runs = n_find = 0
    for rf in iter_result_files(args.paths):
        try:
            obj = json.loads(Path(rf).read_text())
        except (json.JSONDecodeError, OSError) as e:
            print(f"skip {rf}: {e}")
            continue
        run, fs = parse_result(obj)
        if not run.sha256:
            print(f"skip {rf}: no sha256")
            continue
        F.ingest(con, run, args.version, fs,
                 arch=args.arch, path_example=obj.get("path"))
        n_runs += 1
        n_find += len(fs)
    print(f"ingested {n_runs} mango runs, {n_find} findings -> {args.db}")


if __name__ == "__main__":
    main()
