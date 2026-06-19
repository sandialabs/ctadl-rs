"""Shared core for the firmware-eval harness: the normalized Finding, the
results DB, and source-class mapping. Stdlib only (sqlite3/json/dataclasses)
so it runs under the devShell's python311 with no extra deps.

A Finding is the single record both tools (and the external datasets) coerce
into. Two findings MATCH on (sha256, sink_func, sink_callsite); source_class is
deliberately NOT part of the match key -- a source disagreement on a matched
sink is itself a triage signal.
"""

from __future__ import annotations

import json
import sqlite3
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Any, Iterable, Optional

SCHEMA = Path(__file__).with_name("schema.sql")


# --- source-class normalization ---------------------------------------------
# Map both tools' source vocabularies onto one enum so a flow's source class is
# comparable. Keys are matched as substrings (case-insensitive), most-specific
# first. Mango uses function names / "ARGV"; CTADL uses our model's `kind`s.
_SOURCE_CLASS_RULES: list[tuple[str, str]] = [
    ("nvram", "NVRAM"),
    ("nvram_input", "NVRAM"),
    ("acosnvram", "NVRAM"),
    ("getvalue", "NVRAM"),
    ("getenv", "ENV"),
    ("env_input", "ENV"),
    ("recvfrom", "NETWORK"),
    ("recv", "NETWORK"),
    ("network_input", "NETWORK"),
    ("custom_param_parser", "NETWORK"),
    ("http", "NETWORK"),
    ("argv", "ARGV"),
    ("fgets", "FILE"),
    ("fread", "FILE"),
    ("read", "FILE"),
    ("file_input", "FILE"),
]


def classify_source(raw: Optional[str]) -> str:
    """Normalize a raw source token (function name or model kind) to a class."""
    if not raw:
        return "UNKNOWN"
    low = raw.lower()
    for needle, cls in _SOURCE_CLASS_RULES:
        if needle in low:
            return cls
    return "UNKNOWN"


def parse_addr(val: Any) -> Optional[int]:
    """Parse an address that may be hex string ('0x40143c'), decimal, or int."""
    if val is None:
        return None
    if isinstance(val, int):
        return val
    s = str(val).strip()
    try:
        return int(s, 16) if s.lower().startswith("0x") else int(s)
    except ValueError:
        return None


# --- the normalized record ---------------------------------------------------
@dataclass
class Finding:
    binary_sha256: str
    tool: str
    sink_func: Optional[str] = None
    sink_callsite: Optional[int] = None
    sink_site_kind: Optional[str] = None  # "address" | "line"
    sink_argpos: Optional[int] = None
    source_class: str = "UNKNOWN"
    source_sites: list = field(default_factory=list)
    reach_from_main: Optional[bool] = None
    sanitized: Optional[bool] = None
    confidence: Optional[float] = None
    raw_path: Any = None

    def match_key(self) -> tuple:
        return (self.binary_sha256, self.sink_func, self.sink_callsite)


@dataclass
class RunInfo:
    sha256: str
    tool: str
    analyzer_version: str
    status: str = "ok"  # ok|crash|timeout|oom|unsupported
    wall_s: Optional[float] = None
    peak_mem_mb: Optional[float] = None
    exit_code: Optional[int] = None
    cfg_time: Optional[float] = None
    taint_time: Optional[float] = None
    unsupported_reason: Optional[str] = None
    stderr_excerpt: Optional[str] = None
    started_at: Optional[str] = None


# --- DB layer ----------------------------------------------------------------
def connect(db_path: str | Path) -> sqlite3.Connection:
    con = sqlite3.connect(str(db_path))
    con.row_factory = sqlite3.Row
    con.executescript(SCHEMA.read_text())
    return con


def upsert_binary(con: sqlite3.Connection, sha256: str, **cols: Any) -> None:
    con.execute(
        "INSERT INTO binary(sha256) VALUES(?) ON CONFLICT(sha256) DO NOTHING",
        (sha256,),
    )
    for k, v in cols.items():
        if v is not None:
            con.execute(f"UPDATE binary SET {k}=? WHERE sha256=?", (v, sha256))


def run_exists(con: sqlite3.Connection, sha256: str, tool: str, version: str) -> bool:
    cur = con.execute(
        "SELECT 1 FROM run WHERE sha256=? AND tool=? AND analyzer_version=?",
        (sha256, tool, version),
    )
    return cur.fetchone() is not None


def insert_run(con: sqlite3.Connection, r: RunInfo, version: str) -> int:
    cur = con.execute(
        """INSERT INTO run(sha256,tool,analyzer_version,status,wall_s,peak_mem_mb,
                           exit_code,cfg_time,taint_time,unsupported_reason,
                           stderr_excerpt,started_at)
           VALUES(?,?,?,?,?,?,?,?,?,?,?,?)
           ON CONFLICT(sha256,tool,analyzer_version) DO UPDATE SET
             status=excluded.status, wall_s=excluded.wall_s,
             peak_mem_mb=excluded.peak_mem_mb, exit_code=excluded.exit_code,
             cfg_time=excluded.cfg_time, taint_time=excluded.taint_time,
             unsupported_reason=excluded.unsupported_reason,
             stderr_excerpt=excluded.stderr_excerpt, started_at=excluded.started_at""",
        (r.sha256, r.tool, version, r.status, r.wall_s, r.peak_mem_mb, r.exit_code,
         r.cfg_time, r.taint_time, r.unsupported_reason, r.stderr_excerpt, r.started_at),
    )
    if cur.lastrowid:
        return cur.lastrowid
    row = con.execute(
        "SELECT id FROM run WHERE sha256=? AND tool=? AND analyzer_version=?",
        (r.sha256, r.tool, version),
    ).fetchone()
    return row["id"]


def insert_findings(con: sqlite3.Connection, run_id: int, findings: Iterable[Finding]) -> int:
    n = 0
    for f in findings:
        con.execute(
            """INSERT INTO finding(run_id,sha256,sink_func,sink_callsite,sink_site_kind,
                                   sink_argpos,source_class,source_sites,reach_from_main,
                                   sanitized,confidence,raw_path)
               VALUES(?,?,?,?,?,?,?,?,?,?,?,?)""",
            (run_id, f.binary_sha256, f.sink_func, f.sink_callsite, f.sink_site_kind,
             f.sink_argpos, f.source_class, json.dumps(f.source_sites),
             None if f.reach_from_main is None else int(f.reach_from_main),
             None if f.sanitized is None else int(f.sanitized),
             f.confidence, json.dumps(f.raw_path)),
        )
        n += 1
    return n


def ingest(con: sqlite3.Connection, run: RunInfo, version: str,
           findings: Iterable[Finding], arch: Optional[str] = None,
           path_example: Optional[str] = None) -> int:
    """Upsert binary + run + findings in one transaction. Returns run_id."""
    upsert_binary(con, run.sha256, arch=arch, path_example=path_example)
    run_id = insert_run(con, run, version)
    # Replace findings for this run so re-ingest is idempotent.
    con.execute("DELETE FROM finding WHERE run_id=?", (run_id,))
    insert_findings(con, run_id, findings)
    con.commit()
    return run_id


# --- comparison --------------------------------------------------------------
def _match(a: sqlite3.Row, b: sqlite3.Row, addr_tolerance: int) -> bool:
    """Address-primary match within one binary. Both tools reference the same
    binary address space, so the sink call-site address is the discriminator;
    addr_tolerance absorbs angr<->ghidra base-offset / lifting jitter. sink_func
    is only a refinement -- CTADL's SARIF does not always carry the callee name
    (it references the sink statement, not its name), so we never *require* it,
    but if both sides have it and it disagrees, the addresses must still align."""
    ca, cb = a["sink_callsite"], b["sink_callsite"]
    fa, fb = a["sink_func"], b["sink_func"]
    if ca is not None and cb is not None:
        return abs(ca - cb) <= addr_tolerance
    # address missing on a side: fall back to func equality if both known
    if fa and fb:
        return fa == fb
    return False


# Public alias: ground-truth scoring matches GT rows against findings with the
# same address-primary rule. GT rows carry sink_func/sink_callsite too, so the
# same predicate applies unchanged.
match_rows = _match


def compare(con: sqlite3.Connection, version_a: str, version_b: str,
            tool_a: str = "cta", tool_b: str = "mango",
            addr_tolerance: int = 0) -> dict:
    """2x2 between two (tool,version) finding sets. Matching is address-primary
    within each binary (see _match). Set addr_tolerance to the known base delta
    between the two tools' address spaces."""

    def load(tool: str, version: str) -> list[sqlite3.Row]:
        return con.execute(
            """SELECT f.* FROM finding f JOIN run r ON f.run_id=r.id
               WHERE r.tool=? AND r.analyzer_version=?""",
            (tool, version),
        ).fetchall()

    a = load(tool_a, version_a)
    b = load(tool_b, version_b)

    by_sha: dict = {}
    for r in b:
        by_sha.setdefault(r["sha256"], []).append(r)

    matched, only_a = [], []
    matched_b_ids = set()
    for r in a:
        hit = None
        for c in by_sha.get(r["sha256"], []):
            if c["id"] in matched_b_ids:
                continue
            if _match(r, c, addr_tolerance):
                hit = c
                break
        if hit:
            matched.append((r, hit))
            matched_b_ids.add(hit["id"])
        else:
            only_a.append(r)
    only_b = [r for r in b if r["id"] not in matched_b_ids]
    return {"matched": matched, "only_a": only_a, "only_b": only_b}
