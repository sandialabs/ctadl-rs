"""Ground truth for the firmware-eval loop, and scoring ctadl against it.

Ground truth is just a flat set of *known cmdi bugs per binary* -- one row each.
Where it comes from doesn't matter:

  from-mango  -- snapshot the Mango findings already in the DB (tool='mango') as
                 known bugs. (Run normalize_mango first.)
  ingest      -- load known bugs from a dataset file (JSON / JSONL / CSV).

Both land in the same `ground_truth` table with an `origin` tag (mango, a
dataset name, manual...) that's informational only -- there's no tiering.

Then `score` answers the only question that matters for improving the tool:

  found   -- known bug ctadl also reported            (good)
  missed  -- known bug ctadl did NOT report  -> FN    (improve recall)
  extra   -- ctadl reported, not a known bug -> FP?   (improve precision,
                                                        or ground truth is incomplete)

Matching is address-primary within a binary (findings.match_rows), so the sink
call-site lines the two up; --addr-tolerance absorbs any base-offset delta.

Dataset record fields (object keys; CSV uses the same as the header row):
  sha256         required -- binary content hash (the join anchor)
  sink_func      callee name (system, popen, ...)         [>=1 of func/callsite]
  sink_callsite  call instruction address (hex '0x..' or decimal)
  source_class   NETWORK|NVRAM|ENV|ARGV|FILE|CONST|UNKNOWN  (optional)
  origin         where it came from (else --origin default)
  note           free text

Usage:
  # ground truth from a Mango run you already ingested
  python ground_truth.py from-mango --db results.db [--version mango@sha256:<d>]
  # ...or from a dataset file
  python ground_truth.py ingest --db results.db --origin mydataset known_bugs.json

  # how is ctadl doing?
  python ground_truth.py score --db results.db --version cta@<sha> --show 20
"""

from __future__ import annotations

import argparse
import csv
import json
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Optional

import findings as F


@dataclass
class GTRecord:
    sha256: str
    sink_func: Optional[str] = None
    sink_callsite: Optional[int] = None
    source_class: Optional[str] = None
    origin: str = "manual"
    note: Optional[str] = None

    @property
    def addr_known(self) -> int:
        return 1 if self.sink_callsite is not None else 0


# Field-name aliases so a transcribed dataset doesn't have to match exactly.
_ALIASES = {
    "sha256": ("sha256", "sha", "hash", "binary_sha256", "binary_hash"),
    "sink_func": ("sink_func", "sink", "sink_function", "function", "callee", "func"),
    "sink_callsite": ("sink_callsite", "sink_addr", "ins_addr", "address", "addr",
                      "callsite", "vaddr", "offset"),
    "source_class": ("source_class", "source", "src", "src_class", "input"),
    "origin": ("origin", "provenance", "tool", "oracle", "dataset"),
    "note": ("note", "notes", "comment", "description", "desc", "cve"),
}


def _pick(d: dict, keys: tuple[str, ...]) -> Any:
    for k in keys:
        if k in d and d[k] not in ("", None):
            return d[k]
    return None


def _to_record(d: dict, default_origin: str) -> Optional[GTRecord]:
    sha = _pick(d, _ALIASES["sha256"])
    if not sha:
        return None
    func = _pick(d, _ALIASES["sink_func"])
    site = F.parse_addr(_pick(d, _ALIASES["sink_callsite"]))
    if not func and site is None:
        return None  # need at least one way to locate the sink
    src = _pick(d, _ALIASES["source_class"])
    return GTRecord(
        sha256=str(sha).strip(),
        sink_func=str(func).strip() if func else None,
        sink_callsite=site,
        source_class=F.classify_source(src) if src else None,
        origin=str(_pick(d, _ALIASES["origin"]) or default_origin).strip(),
        note=_pick(d, _ALIASES["note"]),
    )


def _load_raw(path: Path) -> list[dict]:
    """Read JSON (list or {records:[...]}), JSONL, or CSV into a list of dicts."""
    if path.suffix.lower() == ".csv":
        with path.open() as fh:
            return list(csv.DictReader(fh))
    if path.suffix.lower() in (".jsonl", ".ndjson"):
        return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    raw = json.loads(path.read_text())
    if isinstance(raw, dict):
        for key in ("records", "results", "alerts", "findings", "data"):
            if isinstance(raw.get(key), list):
                return raw[key]
        return [raw]
    return raw


def load_dataset(path: Path, default_origin: str) -> list[GTRecord]:
    return [r for r in (_to_record(d, default_origin) for d in _load_raw(path)) if r]


def records_from_mango(con: sqlite3.Connection,
                       version: Optional[str] = None) -> list[GTRecord]:
    """Snapshot Mango findings already in the DB (tool='mango') as known bugs.
    Pass `version` to pin a specific image digest; omit to take every Mango run."""
    sql = ("SELECT f.* FROM finding f JOIN run r ON f.run_id=r.id WHERE r.tool='mango'")
    params: tuple = ()
    if version:
        sql += " AND r.analyzer_version=?"
        params = (version,)
    return [GTRecord(sha256=f["sha256"], sink_func=f["sink_func"],
                     sink_callsite=f["sink_callsite"], source_class=f["source_class"],
                     origin="mango", note=f"mango finding #{f['id']}")
            for f in con.execute(sql, params).fetchall()]


def ingest(con: sqlite3.Connection, records: Iterable[GTRecord]) -> int:
    """Insert known-bug rows, skipping exact duplicates. Returns rows inserted."""
    n = 0
    for r in records:
        F.upsert_binary(con, r.sha256)
        dup = con.execute(
            """SELECT 1 FROM ground_truth WHERE sha256=? AND IFNULL(provenance,'')=?
               AND IFNULL(sink_func,'')=IFNULL(?,'')
               AND IFNULL(sink_callsite,-1)=IFNULL(?,-1)""",
            (r.sha256, r.origin, r.sink_func, r.sink_callsite),
        ).fetchone()
        if dup:
            continue
        con.execute(
            """INSERT INTO ground_truth(sha256,sink_func,sink_callsite,addr_known,
                                        source_class,provenance,note)
               VALUES(?,?,?,?,?,?,?)""",
            (r.sha256, r.sink_func, r.sink_callsite, r.addr_known, r.source_class,
             r.origin, r.note),
        )
        n += 1
    con.commit()
    return n


def score(con: sqlite3.Connection, tool: str, version: str,
          addr_tolerance: int = 0) -> dict:
    """How a (tool, version) run did against the known bugs.

    Returns found / missed (FN) / extra (FP candidates). `extra` only counts
    findings on binaries that *have* ground truth -- a binary with no known bugs
    can't tell us anything about precision."""
    gt = con.execute("SELECT * FROM ground_truth").fetchall()
    findings = con.execute(
        """SELECT f.* FROM finding f JOIN run r ON f.run_id=r.id
           WHERE r.tool=? AND r.analyzer_version=?""",
        (tool, version),
    ).fetchall()

    gt_by_sha: dict[str, list] = {}
    for g in gt:
        gt_by_sha.setdefault(g["sha256"], []).append(g)
    f_by_sha: dict[str, list] = {}
    for f in findings:
        f_by_sha.setdefault(f["sha256"], []).append(f)

    found, missed, matched_f = [], [], set()
    for g in gt:
        hit = next((f for f in f_by_sha.get(g["sha256"], [])
                    if f["id"] not in matched_f
                    and F.match_rows(g, f, addr_tolerance)), None)
        if hit:
            matched_f.add(hit["id"])
            found.append((g, hit))
        else:
            missed.append(g)

    # extra = a ctadl finding, on a binary that has ground truth, matching no GT row
    extra = [f for f in findings
             if f["sha256"] in gt_by_sha and f["id"] not in matched_f]
    return {"found": found, "missed": missed, "extra": extra,
            "n_gt": len(gt), "n_findings": len(findings)}


# --- CLI ---------------------------------------------------------------------
def cmd_from_mango(args) -> None:
    con = F.connect(args.db)
    recs = records_from_mango(con, args.version)
    n = ingest(con, recs)
    print(f"added {n} known bugs from mango ({len(recs)} findings, "
          f"version={args.version or 'all'})")


def cmd_ingest(args) -> None:
    con = F.connect(args.db)
    total = 0
    for p in args.paths:
        recs = load_dataset(Path(p), args.origin)
        total += ingest(con, recs)
        print(f"  {p}: {len(recs)} records")
    print(f"added {total} known bugs.")


def cmd_score(args) -> None:
    con = F.connect(args.db)
    print_score(score(con, args.tool, args.version, args.addr_tolerance),
                args.tool, args.version, args.show)


def print_score(res: dict, tool: str, version: str, show: int) -> None:
    nf, nm, ne = len(res["found"]), len(res["missed"]), len(res["extra"])
    recall = 100.0 * nf / (nf + nm) if (nf + nm) else 0.0
    print(f"{tool}={version} vs {res['n_gt']} known bugs\n")
    print(f"  found  (TP)   : {nf:5d}")
    print(f"  missed (FN)   : {nm:5d}   <- improve recall   (recall={recall:.1f}%)")
    print(f"  extra  (FP?)  : {ne:5d}   <- improve precision / GT incomplete")
    if show and res["missed"]:
        print("\n-- missed (FN): known bugs ctadl didn't report --")
        for g in res["missed"][:show]:
            print(f"  {g['sha256'][:12]} {g['sink_func']}@{g['sink_callsite']} "
                  f"src={g['source_class']} [{g['provenance']}]")
    if show and res["extra"]:
        print("\n-- extra (FP?): ctadl reported, not in ground truth --")
        for f in res["extra"][:show]:
            print(f"  finding#{f['id']:<6} {f['sha256'][:12]} "
                  f"{f['sink_func']}@{f['sink_callsite']} src={f['source_class']}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    m = sub.add_parser("from-mango", help="known bugs from an ingested Mango run")
    m.add_argument("--db", required=True)
    m.add_argument("--version", default=None, help="pin a Mango image digest (else all)")
    m.set_defaults(func=cmd_from_mango)

    i = sub.add_parser("ingest", help="known bugs from a dataset file (json/jsonl/csv)")
    i.add_argument("--db", required=True)
    i.add_argument("--origin", default="dataset", help="label for where these came from")
    i.add_argument("paths", nargs="+")
    i.set_defaults(func=cmd_ingest)

    s = sub.add_parser("score", help="ctadl found / missed / extra vs known bugs")
    s.add_argument("--db", required=True)
    s.add_argument("--tool", default="cta")
    s.add_argument("--version", required=True)
    s.add_argument("--addr-tolerance", type=int, default=0)
    s.add_argument("--show", type=int, default=0)
    s.set_defaults(func=cmd_score)

    args = ap.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
