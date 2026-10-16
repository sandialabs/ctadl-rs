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

# exec-family functions whose attacker-controlled data is an *argv element*
# handed to a spawned program (argument injection), not a shell-interpreted
# command string. Mango does NOT emit these as cmdi closures -- it reports them
# only in the sibling `execv.json` produced by its argument-resolution pass. We
# ingest that file too so execve/execlp argv-injection is part of ground truth.
EXEC_FUNCS = {
    "execl", "execlp", "execle", "execv", "execvp", "execvpe", "execve",
    "SLIBCExecl", "SLIBCExec", "SLIBCExecv",
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


def _exec_sink_func(cmdi_obj: dict) -> Optional[str]:
    """The exec-family callee Mango targeted, read from the sibling
    cmdi_results.json `sinks` map (e.g. `{"execlp": 1}`). This is how we recover
    the function name, since `execv.json` keys entries by the *spawned* binary,
    not the caller. Unambiguous in practice (one exec sink per handcrafted
    binary); if several, return None and let address matching carry it."""
    sinks = (cmdi_obj or {}).get("sinks") or {}
    execs = [name for name in sinks if name in EXEC_FUNCS]
    return execs[0] if len(execs) == 1 else None


def parse_execv(obj: dict, sha: str, sink_func: Optional[str]) -> list[F.Finding]:
    """Argv-injection findings from Mango's `execv.json`. Each resolved exec call
    with a non-empty `vulnerable_args` is one Finding: attacker input reaches an
    argument of a spawned program. `args` maps positional index -> resolved value
    tokens (e.g. `"2": ["<BV64 ARGV_1>"]`); `vulnerable_args` lists the tainted
    indices; `addr` is the exec call site."""
    out: list[F.Finding] = []
    execv = (obj or {}).get("execv") or {}
    for spawned, calls in execv.items():
        for call in calls or []:
            vuln = call.get("vulnerable_args") or []
            if not vuln:
                continue
            args = call.get("args") or {}
            sites: list = []
            best = "UNKNOWN"
            for idx in vuln:
                for tok in args.get(str(idx), []) or []:
                    sites.append(tok)
                    cls = F.classify_source(tok)
                    if best == "UNKNOWN" and cls != "UNKNOWN":
                        best = cls
            # argv-resolved values that aren't a recognized getter/recv/etc. are
            # the program's own argv -> ARGV (Mango renders these as `ARGV_n`).
            if best == "UNKNOWN":
                best = "ARGV"
            out.append(F.Finding(
                binary_sha256=sha,
                tool="mango",
                sink_func=sink_func,
                sink_callsite=F.parse_addr(call.get("addr")),
                sink_site_kind="address",
                sink_argpos=vuln[0],
                source_class=best,
                source_sites=sites,
                raw_path={"spawned": spawned, "vulnerable_args": vuln,
                          "args": args, "kind": "argv_injection"},
            ))
    return out


def sibling_execv_findings(cmdi_path: Path, sha: str, cmdi_obj: dict) -> list[F.Finding]:
    """Read the `execv.json` next to a `cmdi_results.json` and parse it into
    argv-injection Findings. Returns [] if absent/unreadable or sha mismatches."""
    ev_path = Path(cmdi_path).parent / "execv.json"
    if not ev_path.exists():
        return []
    try:
        ev = json.loads(ev_path.read_text())
    except (json.JSONDecodeError, OSError) as e:
        print(f"skip {ev_path}: {e}")
        return []
    ev_sha = ev.get("sha256")
    if ev_sha and sha and ev_sha != sha:
        return []  # paired files must describe the same binary
    return parse_execv(ev, sha, _exec_sink_func(cmdi_obj))


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
        fs = fs + sibling_execv_findings(Path(rf), run.sha256, obj)
        F.ingest(con, run, args.version, fs,
                 arch=args.arch, path_example=obj.get("path"))
        n_runs += 1
        n_find += len(fs)
    print(f"ingested {n_runs} mango runs, {n_find} findings -> {args.db}")


if __name__ == "__main__":
    main()
