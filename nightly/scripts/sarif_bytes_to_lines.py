#!/usr/bin/env python3
"""
sarif_bytes_to_lines.py

Convert SARIF results that point at a binary (byteOffset/byteLength) into SARIF
results that point at source files (startLine[/endLine]) using "source map" JSON
files like the provided Certificate.json.

Usage:
  python sarif_bytes_to_lines.py \
      --in backflash.sarif \
      --maps /path/to/maps_dir \
      --out backflash.lines.sarif

Notes/assumptions:
- The SARIF input uses locations[].physicalLocation.region.byteOffset/byteLength.
- The mapping JSON format matches the example: top-level {"mappings":[...]} where each
  mapping has "binary":[{"physicalLocation":{... region.byteOffset/byteLength ...}}]
  and "source":[{"physicalLocation":{... artifactLocation.uri ... region.startLine ...}}].
- We choose the "best" mapping as the one whose binary span overlaps the SARIF span
  with maximum overlap (ties broken by smaller mapped span).
- If no mapping is found, the location is left unchanged and annotated.
"""

from __future__ import annotations

import argparse
import json
import os
from dataclasses import dataclass
from typing import Any, Dict, List, Optional, Tuple
from collections import OrderedDict


@dataclass(frozen=True)
class BinarySpan:
    offset: int
    length: int

    @property
    def end(self) -> int:
        return self.offset + self.length  # half-open [offset, end)


@dataclass(frozen=True)
class SourceLoc:
    uri: str
    start_line: int
    start_column: Optional[int] = None
    end_line: Optional[int] = None
    end_column: Optional[int] = None


@dataclass(frozen=True)
class MappingEntry:
    binary_uri: Optional[str]          # e.g., "classes.dex" (may be absent)
    span: BinarySpan
    source: SourceLoc


def _overlap(a: BinarySpan, b: BinarySpan) -> int:
    lo = max(a.offset, b.offset)
    hi = min(a.end, b.end)
    return max(0, hi - lo)


def _result_key(res: dict) -> Optional[str]:
    """Return a unique key based on primary location's file & startLine."""
    locs = res.get("locations") or []
    if not locs:
        return None
    pl = (locs[0].get("physicalLocation") or {})
    art = pl.get("artifactLocation") or {}
    reg = pl.get("region") or {}
    uri = art.get("uri")
    line = reg.get("startLine")
    if isinstance(uri, str) and isinstance(line, int):
        return f"{uri}:{line}"
    return None

def _merge_results(results: List[Dict]) -> List[Dict]:
    """Deduplicate results so only one per source line remains.
    Merges messages and shallow merges properties where possible."""
    seen: "OrderedDict[str, dict]" = OrderedDict()
    for r in results:
        k = _result_key(r)
        if k is None:
            # Preserve results without deterministic keys using a unique placeholder.
            seen[f"__no_key_{id(r)}"] = r
            continue
        if k not in seen:
            seen[k] = r
        else:
            existing = seen[k]
            # Merge message text
            old_msg = (existing.get("message") or {}).get("text", "")
            new_msg = (r.get("message") or {}).get("text", "")
            merged_msg = "; ".join(filter(None, [old_msg, new_msg]))
            if merged_msg:
                existing.setdefault("message", {})["text"] = merged_msg
            # Shallow merge properties dictionaries
            old_props = existing.get("properties")
            new_props = r.get("properties")
            if isinstance(old_props, dict) and isinstance(new_props, dict):
                for pk, pv in new_props.items():
                    if pk not in old_props:
                        old_props[pk] = pv
                    else:
                        if old_props[pk] != pv:
                            combined = old_props[pk]
                            if not isinstance(combined, list):
                                combined = [combined]
                            if pv not in combined:
                                combined.append(pv)
                            old_props[pk] = combined
    return list(seen.values())

def load_mapping_file(path: str) -> List[MappingEntry]:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)

    out: List[MappingEntry] = []
    for m in data.get("mappings", []):
        binaries = m.get("binary", [])
        sources = m.get("source", [])
        if not binaries or not sources:
            continue

        # Many files appear to have single binary and single source entry.
        bpl = binaries[0].get("physicalLocation", {})
        spl = sources[0].get("physicalLocation", {})

        b_art = bpl.get("artifactLocation", {}) or {}
        b_reg = bpl.get("region", {}) or {}
        s_art = spl.get("artifactLocation", {}) or {}
        s_reg = spl.get("region", {}) or {}

        b_off = b_reg.get("byteOffset")
        b_len = b_reg.get("byteLength")
        s_line = s_reg.get("startLine")

        if b_off is None or b_len is None or s_line is None:
            continue

        binary_uri = b_art.get("uri")  # e.g., "classes.dex"
        source_uri = s_art.get("uri")  # e.g., "com/adobe/flashplayer/Certificate.java"

        # Optional: handle columns/endLine if present
        src = SourceLoc(
            uri=source_uri,
            start_line=int(s_line),
            start_column=(int(s_reg["startColumn"]) if "startColumn" in s_reg else None),
            end_line=(int(s_reg["endLine"]) if "endLine" in s_reg else None),
            end_column=(int(s_reg["endColumn"]) if "endColumn" in s_reg else None),
        )

        out.append(
            MappingEntry(
                binary_uri=binary_uri,
                span=BinarySpan(offset=int(b_off), length=int(b_len)),
                source=src,
            )
        )

    return out


def load_all_mappings(maps_root: str) -> List[MappingEntry]:
    entries: List[MappingEntry] = []
    for dirpath, _, filenames in os.walk(maps_root):
        for fn in filenames:
            if not fn.lower().endswith(".json"):
                continue
            path = os.path.join(dirpath, fn)
            try:
                entries.extend(load_mapping_file(path))
            except Exception as e:
                # Skip malformed/unexpected files but keep going
                print(f"[warn] failed to load mapping file {path}: {e}")
    return entries


def find_best_mapping(
    mappings: List[MappingEntry],
    sarif_span: BinarySpan,
    *,
    preferred_binary_uri: Optional[str] = None,
) -> Optional[MappingEntry]:
    best: Optional[Tuple[int, int, MappingEntry]] = None
    for me in mappings:
        if preferred_binary_uri is not None and me.binary_uri is not None:
            # Only filter if mapping declares a binary_uri; otherwise keep it eligible.
            if me.binary_uri != preferred_binary_uri:
                continue

        ov = _overlap(me.span, sarif_span)
        if ov <= 0:
            continue

        # Rank: max overlap, then min mapped span length
        rank = (ov, -me.span.length)
        if best is None or rank > (best[0], best[1]):
            best = (rank[0], rank[1], me)

    return None if best is None else best[2]


def convert_sarif(in_path: str, maps_root: str, out_path: str) -> None:
    with open(in_path, "r", encoding="utf-8") as f:
        sarif = json.load(f)

    mappings = load_all_mappings(maps_root)
    if not mappings:
        raise RuntimeError(f"No mapping entries loaded from: {maps_root}")

    # Iterate runs/results/locations
    runs = sarif.get("runs", [])
    for run in runs:
        results = run.get("results", [])
        for res in results:
            locs = res.get("locations", []) or []
            for loc in locs:
                pl = (loc.get("physicalLocation") or {})
                reg = (pl.get("region") or {})
                art = (pl.get("artifactLocation") or {})

                b_off = reg.get("byteOffset")
                b_len = reg.get("byteLength")

                if b_off is None or b_len is None:
                    continue  # nothing to convert

                sarif_span = BinarySpan(offset=int(b_off), length=int(b_len))

                # If SARIF points at an .apk, we generally can't match that directly.
                # The mapping files often refer to "classes.dex". If your SARIF can
                # distinguish which DEX, pass it in via artifactLocation; otherwise
                # we do not filter by binary uri.
                preferred_binary_uri = None
                uri = art.get("uri")
                if isinstance(uri, str) and (uri.endswith("classes.dex") or uri == "classes.dex"):
                    preferred_binary_uri = "classes.dex"

                me = find_best_mapping(mappings, sarif_span, preferred_binary_uri=preferred_binary_uri)
                if me is None:
                    # annotate message so you can see what didn't map
                    msg = (res.get("message") or {}).get("text", "")
                    res["message"] = {"text": f"{msg} [unmapped byteOffset={b_off} byteLength={b_len}]".strip()}
                    continue

                # Rewrite location to source file/line-based region
                pl["artifactLocation"] = {"uri": me.source.uri}
                new_region: Dict[str, Any] = {"startLine": me.source.start_line}
                if me.source.start_column is not None:
                    new_region["startColumn"] = me.source.start_column
                if me.source.end_line is not None:
                    new_region["endLine"] = me.source.end_line
                if me.source.end_column is not None:
                    new_region["endColumn"] = me.source.end_column

                # Optionally preserve original bytes in properties
                pl.setdefault("properties", {})
                pl["properties"]["originalArtifact"] = art
                pl["properties"]["originalRegion"] = {"byteOffset": int(b_off), "byteLength": int(b_len)}

                pl["region"] = new_region
                loc["physicalLocation"] = pl

    # Deduplicate results per source line, merging messages and properties
    for run in runs:
        if "results" in run:
            run["results"] = _merge_results(run["results"]) 

    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(sarif, f, indent=2, sort_keys=False)
        f.write("\n")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="in_path", required=True, help="Input SARIF (e.g., backflash.sarif)")
    ap.add_argument("--maps", dest="maps_root", required=True, help="Directory containing source-map JSON files")
    ap.add_argument("--out", dest="out_path", required=True, help="Output SARIF with line-based regions")
    args = ap.parse_args()

    convert_sarif(args.in_path, args.maps_root, args.out_path)


if __name__ == "__main__":
    main()
