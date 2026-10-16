"""Normalize CTADL SARIF output (`results.sarif`) into Finding rows.

Verified CTADL SARIF shape for a taint flow:
  result.ruleId            == "C0001.tainted-path"
  result.message.text      == "Taint flow labelled '<source-kind>'"
  result.properties.taintLabels   == ["<source-kind>", ...]
  result.properties.taintVertices == { "<sink-kind>": [...], "<source-kind>": [...] }
  result.locations[0].physicalLocation.region   -> sink-side location
  result.codeFlows[].threadFlows[].locations[]  -> the step-by-step path
  run.logicalLocations[i].fullyQualifiedName     -> containing function names
  run.properties.parquet_dir -> source-info parquet (precise addr resolution)

CTADL has no per-finding confidence/rank, so confidence is left NULL.

Open item (finalize on first real pcode SARIF): how the *instruction address*
of the sink call surfaces in physicalLocation. On the .tnt/C frontends it's
line/col; pcode encodes the address (likely region.startLine or the uri). We
capture whatever is there and tag sink_site_kind so the matcher knows; the
parquet_dir is recorded for exact stmt->address resolution if needed.

Usage:
  python normalize_ctadl.py --db results.db --version <ctadl git sha> \
      --sha256 <binary sha> [--arch mipsel] [--status ok] results.sarif
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Optional

import findings as F

# Mango/angr load every one of these test binaries at this base, while Ghidra
# loads PIE ELFs at 0x100000 and non-PIE at 0x400000. The SARIF `address` object
# carries a base-independent `relativeAddress` (RVA); rebasing it onto the angr
# base puts CTADL sink addresses in the SAME space as the Mango ground truth, so
# they join address-primary regardless of PIE/non-PIE. (A small --addr-tolerance
# still absorbs the call-vs-arg-setup instruction jitter between the two lifters.)
ANGR_LOAD_BASE = 0x400000

# The cmdi sink callee names (mirror of the model / Mango COMMAND_INJECTION_SINKS).
# Used to recognize which sink a flow ends in, by scanning the SARIF text.
CMDI_SINKS = {
    "system", "twsystem", "execFormatCmd", "exec_cmd", "___system", "bstar_system",
    "doSystemCmd", "doShell", "CsteSystem", "cgi_deal_popen", "ExeCmd", "ExecShell",
    "exec_shell_popen", "exec_shell_popen_str", "popen", "execl", "execlp", "execle",
    "execv", "execvp", "execvpe", "execve", "tp_systemEx", "exec_shell_async",
    "exec_shell_sync", "exec_shell_sync2", "SLIBCSystem", "SLIBCExec", "SLIBCExecv",
    "SLIBCPopen", "pegaSystem", "SLIBCExecl",
}
_SINK_RE = re.compile("|".join(rf"\b{re.escape(s)}\b" for s in sorted(CMDI_SINKS, key=len, reverse=True)))


def _logical_names(run: dict) -> list[str]:
    return [ll.get("fullyQualifiedName") or ll.get("name")
            for ll in run.get("logicalLocations", [])]


def _resolve_logical(run_names: list[str], loc: dict) -> Optional[str]:
    for ll in loc.get("logicalLocations", []) or []:
        idx = ll.get("index")
        if isinstance(idx, int) and 0 <= idx < len(run_names):
            return run_names[idx]
        if ll.get("fullyQualifiedName"):
            return ll["fullyQualifiedName"]
    return None


def _sink_callee(result: dict) -> Optional[str]:
    """The sink callee name. CTADL emits it directly as `properties.sinkCallee`
    (see formatter.rs C0001 path results); fall back to scanning the result text
    for a known sink name on older SARIF that predates that property."""
    props = result.get("properties") or {}
    if props.get("sinkCallee"):
        return props["sinkCallee"]
    funcs = props.get("sinkFunctions")
    if funcs:
        return funcs[0]
    m = _SINK_RE.search(json.dumps(result))
    return m.group(0) if m else None


def _sink_site(result: dict) -> tuple[Optional[int], Optional[str]]:
    """Best-effort sink call-site location. Returns (value, kind) where kind is
    'address' or 'line'.

    Current pcode SARIF (post "Anchor query endpoints at callsites", #31) encodes
    the sink-call instruction address under
    `locations[0].physicalLocation.address.absoluteAddress` -- a real VA in the
    same space Mango reports, so it joins address-primary against the Mango GT.
    Older fallbacks (a property bag or `region.startLine`) are kept for SARIF that
    predates the `address` object."""
    locs = result.get("locations") or []
    if not locs:
        return None, None
    phys = (locs[0].get("physicalLocation") or {})
    # Preferred: the SARIF `address` object on the sink physicalLocation. Rebase
    # the (base-independent) relativeAddress onto angr's load base so it lands in
    # the Mango ground-truth address space; fall back to absoluteAddress as-is.
    addr = phys.get("address") or {}
    rel = F.parse_addr(addr.get("relativeAddress"))
    if rel is not None:
        return rel + ANGR_LOAD_BASE, "address"
    a = F.parse_addr(addr.get("absoluteAddress"))
    if a is not None:
        return a, "address"
    # pcode frontends may carry the address as a property or in the uri.
    props = locs[0].get("properties") or {}
    for key in ("address", "ins_addr", "vaddr"):
        a = F.parse_addr(props.get(key))
        if a is not None:
            return a, "address"
    region = phys.get("region") or {}
    line = region.get("startLine")
    if line is not None:
        # On pcode the "line" is conventionally the instruction address; the
        # matcher treats sink_site_kind to decide address vs source-line.
        return int(line), "line"
    return None, None


def _rebased_addr(addr: dict) -> Optional[int]:
    """A codeFlow/location `address` object -> a VA in Mango's (angr) space.
    Prefer the base-independent relativeAddress (RVA) rebased onto the angr load
    base; fall back to absoluteAddress. Mirrors `_sink_site`'s rebasing so PIE and
    non-PIE binaries land in the same address space as the Mango ground truth."""
    if not isinstance(addr, dict):
        return None
    rel = F.parse_addr(addr.get("relativeAddress"))
    if rel is not None:
        return rel + ANGR_LOAD_BASE
    return F.parse_addr(addr.get("absoluteAddress"))


def _callsite_addrs(result: dict) -> list[int]:
    """The sink *call-site* instruction address(es) for this flow.

    CTADL anchors the result's top-level `location` at the sink callee's entry
    (its PLT thunk on these ELFs), NOT at the call instruction -- so that address
    is ~hundreds of bytes off from Mango, which reports the `call` site. The real
    call site is in the codeFlow: a step whose message contains `call-arg(...)` is
    a call that passes tainted data forward. Per threadFlow we take the LAST such
    step (the sink call itself, past any wrapper-call hops); one result can carry
    several codeFlows -> several distinct call sites, which Mango counts as
    separate bugs. Returns the distinct rebased addresses, preserving order.

    Message format note: newer CTADL SARIF prefixes the step verb and names the
    callee -- `call call-arg(160, 3) in snprintf`, `sink call-arg(197, 0).deref
    in system` -- so `call-arg(` appears mid-string, not at the start (older SARIF
    emitted a bare `call-arg(...)`). We match the substring to handle both.

    PLT-thunk-seed de-duplication. The new frontend emits, for one bug, both the
    real flow (terminal `sink call-arg(...) in <sink>` @ the true call site) and a
    degenerate twin whose terminal `sink` step re-anchors at the sink's PLT thunk
    (the callee entry) -- e.g. heap's real `system(command2)` @ 0x1225 plus a twin
    ending at 0x10a4, 4 bytes into `system@plt`. Taking the last call-arg step
    blindly picks the thunk on the twin, emitting a spurious low-address duplicate
    of a real finding. The twin, however, still contains the true call site as a
    `call call-arg(...) in <sink>` step (verb `call`, i.e. a real forwarding call
    into the sink), whereas its thunk endpoint is a `sink call-arg(...)` step at a
    different address. So per threadFlow we prefer the last `call ... in <sink>`
    forwarding-call site when one is present, and fall back to the last call-arg
    step otherwise (covers the clean flow, whose only in-<sink> step is the `sink`
    line at the real call, and the old bare-`call-arg` format). This collapses
    each thunk twin onto its real TP instead of reporting it as an extra."""
    sink = (result.get("properties") or {}).get("sinkCallee")
    seen: dict[int, None] = {}
    for cf in result.get("codeFlows") or []:
        for tf in cf.get("threadFlows") or []:
            last: Optional[int] = None        # last call-arg step (any)
            call_into_sink: Optional[int] = None  # last `call ... in <sink>` site
            for step in tf.get("locations") or []:
                loc = step.get("location") or {}
                msg = (loc.get("message") or {}).get("text", "") or ""
                if "call-arg(" in msg:
                    a = _rebased_addr((loc.get("physicalLocation") or {}).get("address"))
                    if a is not None:
                        last = a
                        if sink and msg.startswith("call ") and msg.rstrip().endswith("in " + sink):
                            call_into_sink = a
            addr = call_into_sink if call_into_sink is not None else last
            if addr is not None:
                seen.setdefault(addr, None)
    return list(seen)


def parse_sarif(sarif: dict, sha256: str) -> list[F.Finding]:
    out: list[F.Finding] = []
    seen: set = set()
    for run in sarif.get("runs", []):
        run_names = _logical_names(run)
        parquet = (run.get("properties") or {}).get("parquet_dir")
        for res in run.get("results", []):
            rid = res.get("ruleId", "")
            if not rid.startswith("C0001"):  # tainted-path only
                continue
            vtx = (res.get("properties") or {}).get("taintVertices") or {}
            if "command_injection" not in vtx:
                continue  # not a cmdi flow
            props = res.get("properties") or {}
            labels = props.get("taintLabels") or []
            src_raw = labels[0] if labels else None
            # CTADL now also emits the source *function* names (sourceFunctions);
            # classify from those if the kind label is ambiguous, and record them.
            src_funcs = props.get("sourceFunctions") or []
            if F.classify_source(src_raw) == "UNKNOWN" and src_funcs:
                src_raw = src_funcs[0]
            sink_func = _sink_callee(res)
            source_class = F.classify_source(src_raw)
            source_sites = src_funcs or labels
            locs = res.get("locations") or [{}]
            container = _resolve_logical(run_names, locs[0]) if locs else None

            # One finding per real call site (from the codeFlow). Fall back to the
            # result's anchored location when no call-arg step is present.
            sites = [(a, "address") for a in _callsite_addrs(res)] or [_sink_site(res)]
            for site, kind in sites:
                dedup = (sink_func, site, source_class, tuple(source_sites))
                if dedup in seen:
                    continue
                seen.add(dedup)
                out.append(F.Finding(
                    binary_sha256=sha256,
                    tool="cta",
                    sink_func=sink_func,
                    sink_callsite=site,
                    sink_site_kind=kind,
                    sink_argpos=0,
                    source_class=source_class,
                    source_sites=source_sites,
                    reach_from_main=None,
                    sanitized=None,
                    confidence=None,
                    raw_path={"container": container,
                              "codeFlows": res.get("codeFlows"),
                              "parquet_dir": parquet},
                ))
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", required=True)
    ap.add_argument("--version", required=True, help="ctadl analyzer_version (git sha)")
    ap.add_argument("--sha256", required=True, help="sha256 of the analyzed binary")
    ap.add_argument("--arch", default=None)
    ap.add_argument("--status", default="ok", help="ok|crash|timeout|oom|unsupported")
    ap.add_argument("--path-example", default=None)
    ap.add_argument("sarif")
    args = ap.parse_args()

    con = F.connect(args.db)
    sarif = json.loads(Path(args.sarif).read_text())
    fs = parse_sarif(sarif, args.sha256)
    run = F.RunInfo(sha256=args.sha256, tool="cta", analyzer_version="", status=args.status)
    F.ingest(con, run, args.version, fs, arch=args.arch, path_example=args.path_example)
    print(f"ingested CTADL run for {args.sha256[:12]} ({args.status}): {len(fs)} cmdi findings")


if __name__ == "__main__":
    main()
