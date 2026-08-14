#!/usr/bin/env python3
"""Build the ONE shared model set both tools run with - DO-NOT-MERGE.

The head-to-head experiment compares two analysis *engines*. If each engine runs
with its own shipped default models, a difference in findings is partly a
difference in modeling, and the comparison says nothing. So both engines run with
their defaults suppressed and with the same models loaded, generated here from a
single source of truth.

The shared set is the UNION of three inputs:

  1. ctadl-rs's built-in native propagation models
     (`ctadl-ascent/src/models/defaults/native-index.jsonl`)
  2. ctadl-souffle's built-in pcode propagation models
     (`ctadl/models/pcode/default-index.json`)
  3. the command-injection model this benchmark already uses
     (`firmware-eval/models/cmdi-firmware.json5`) - sources, sinks, and its
     extra string-builder propagations.

Union, not intersection, for a mechanical reason: ctadl-souffle has no
`--no-default-models`, so `ctadl index --models F` *adds* F to its built-in
default-index.json. Its defaults are therefore unavoidable, and the only way both
engines can end up with an identical model set is for that set to contain them.
ctadl-rs is then run with `--no-default-models` and the generated file, so
neither engine contributes anything of its own.

Outputs (in this directory):

  shared-index.rs.json        propagation / library models, ctadl-rs syntax
  shared-query.rs.json        sources + sinks,              ctadl-rs syntax
  shared-index.souffle.json   propagation / library models, ctadl-souffle syntax
  shared-query.souffle.json   sources + sinks,              ctadl-souffle syntax
  MODELS.md                   what was generated, and the translation rules

The two syntaxes are not identical, and the differences are all in the access
paths. `where` clauses (`signature_match` + `names`, `name` + `pattern`) mean the
same thing in both: both match the function's *short* name, `name` as a regex.
See MODELS.md for the port rules.
"""

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]  # .../ct-head-to-head-firmware
RS_DEFAULTS = REPO / "ctadl-ascent/src/models/defaults/native-index.jsonl"
CMDI = REPO / "firmware-eval/models/cmdi-firmware.json5"
SOUFFLE_DEFAULTS_GLOB = "python3.13-ctadl-0.14.1/lib/python3.13/site-packages/ctadl/models/pcode/default-index.json"


# --------------------------------------------------------------------------
# JSON5 -> JSON
# --------------------------------------------------------------------------
def json5_to_json(text: str):
    """Parse the JSON5 subset `cmdi-firmware.json5` uses.

    That subset is: `//` line comments, bare identifier keys, and trailing
    commas. Strings are always double-quoted. The scan tracks string state so a
    `//` or a `{` inside a regex pattern is left alone.
    """
    out = []
    i, n = 0, len(text)
    in_str = False
    while i < n:
        c = text[i]
        if in_str:
            out.append(c)
            if c == "\\":  # copy the escaped char verbatim
                if i + 1 < n:
                    out.append(text[i + 1])
                    i += 2
                    continue
            elif c == '"':
                in_str = False
            i += 1
            continue
        if c == '"':
            in_str = True
            out.append(c)
            i += 1
            continue
        if text.startswith("//", i):  # line comment
            j = text.find("\n", i)
            i = n if j < 0 else j
            continue
        if text.startswith("/*", i):  # block comment
            j = text.find("*/", i)
            i = n if j < 0 else j + 2
            continue
        out.append(c)
        i += 1
    stripped = "".join(out)
    # bare identifier keys -> quoted keys
    stripped = re.sub(r"([{,]\s*)([A-Za-z_][A-Za-z0-9_]*)(\s*:)", r'\1"\2"\3', stripped)
    # trailing commas
    stripped = re.sub(r",(\s*[}\]])", r"\1", stripped)
    return json.loads(stripped)


# --------------------------------------------------------------------------
# loading the three inputs
# --------------------------------------------------------------------------
def load_jsonl_models(path: Path):
    gens = []
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("//"):
            continue
        gens.append(json.loads(line))
    return gens


def find_souffle_defaults():
    for p in Path("/nix/store").glob("*" + SOUFFLE_DEFAULTS_GLOB.replace("python3.13-ctadl", "python3.13-ctadl")):
        return p
    return None


def souffle_port_to_rs(port: str) -> str:
    """ctadl-souffle spells the pointee `.*`; ctadl-rs spells it `.deref`."""
    return port.replace(".*", ".deref")


def rs_port_to_souffle(port: str) -> str:
    return port.replace(".deref", ".*")


def souffle_gen_to_rs(g: dict) -> dict:
    g = json.loads(json.dumps(g))
    for p in g.get("model", {}).get("propagation", []):
        for k in ("input", "output"):
            if k in p:
                p[k] = souffle_port_to_rs(p[k])
    for side in ("sources", "sinks"):
        for e in g.get("model", {}).get(side, []):
            if "port" in e:
                e["port"] = souffle_port_to_rs(e["port"])
    return g


# --------------------------------------------------------------------------
# rs -> souffle translation
# --------------------------------------------------------------------------
def rs_gen_to_souffle(g: dict) -> dict:
    """Translate one ctadl-rs generator into ctadl-souffle's schema.

    Rules (all of them; there is nothing else):

      port `.deref`      -> `.*`      both spell "the bytes at this pointer";
                                      souffle's star field is what its pcode
                                      frontend actually emits for a dereference,
                                      and `--match-star-fields` (on by default)
                                      makes it match concrete fields.
      source `saturating`-> `all_fields: true`
                                      rs saturates the access-path subtree under
                                      the seeded vertex; souffle's `all_fields`
                                      sensitizes the port to every field it saw.
                                      Same intent: "all of this is attacker-
                                      controlled, however the callee indexes in."
      sink `wildcard`    -> dropped, EXCEPT on a sink whose port carries no field
                                      at all (`Argument(1)` for the execv family,
                                      which relies on rs's default wildcard to
                                      reach `Argument(1).[k].deref`). Those get
                                      `all_fields: true`, souffle's nearest
                                      equivalent. A sink port that already ends in
                                      `.*` needs nothing: star matching covers the
                                      extensions rs's wildcard covers.

    `where` clauses and `modes` pass through unchanged - they mean the same thing
    in both engines.
    """
    g = json.loads(json.dumps(g))
    model = g.get("model", {})
    for p in model.get("propagation", []):
        for k in ("input", "output"):
            if k in p:
                p[k] = rs_port_to_souffle(p[k])
    for e in model.get("sources", []):
        saturating = e.pop("saturating", False)
        if "port" in e:
            e["port"] = rs_port_to_souffle(e["port"])
        if saturating:
            e["all_fields"] = True
    for e in model.get("sinks", []):
        wildcard = e.pop("wildcard", True)  # rs default for sinks is wildcard
        port = e.get("port", "")
        e["port"] = rs_port_to_souffle(port)
        if wildcard and "." not in port:
            # bare port: rs matches every extension of it, souffle needs to be
            # told to look at the fields.
            e["all_fields"] = True
    return g


# --------------------------------------------------------------------------
# de-duplication
# --------------------------------------------------------------------------
def dedupe(gens):
    seen, out = set(), []
    for g in gens:
        key = json.dumps(g, sort_keys=True)
        if key in seen:
            continue
        seen.add(key)
        out.append(g)
    return out


def split_endpoints(gens):
    """Split generators into (propagation-ish, endpoint-ish).

    A generator can carry both; the ones here do not, but the split is done per
    model key so a mixed generator would land correctly on both sides.
    """
    index_gens, query_gens = [], []
    for g in gens:
        model = g.get("model", {})
        idx = {k: v for k, v in model.items() if k in ("propagation", "modes")}
        qry = {k: v for k, v in model.items() if k in ("sources", "sinks")}
        if idx:
            index_gens.append({**g, "model": idx})
        if qry:
            query_gens.append({**g, "model": qry})
    return index_gens, query_gens


def write(path: Path, gens, header: str):
    path.write_text(
        json.dumps({"model_generators": gens}, indent=2) + "\n"
    )
    print(f"  {path.name:<28} {len(gens):3d} generators   ({header})")


def main():
    souffle_defaults = find_souffle_defaults()
    if souffle_defaults is None:
        sys.exit("could not find ctadl-souffle's pcode default-index.json in /nix/store")

    rs_defaults = load_jsonl_models(RS_DEFAULTS)
    sf_defaults = [
        souffle_gen_to_rs(g)
        for g in json.loads(souffle_defaults.read_text())["model_generators"]
    ]
    cmdi = json5_to_json(CMDI.read_text())["model_generators"]

    print(f"inputs:")
    print(f"  ctadl-rs   native-index.jsonl : {len(rs_defaults):3d} generators")
    print(f"  souffle    default-index.json : {len(sf_defaults):3d} generators  ({souffle_defaults})")
    print(f"  cmdi-firmware.json5           : {len(cmdi):3d} generators")

    cmdi_index, cmdi_query = split_endpoints(cmdi)
    index_rs = dedupe(rs_defaults + sf_defaults + cmdi_index)
    query_rs = dedupe(cmdi_query)

    print("outputs:")
    write(HERE / "shared-index.rs.json", index_rs, "ctadl-rs, propagation")
    write(HERE / "shared-query.rs.json", query_rs, "ctadl-rs, sources+sinks")
    write(
        HERE / "shared-index.souffle.json",
        [rs_gen_to_souffle(g) for g in index_rs],
        "ctadl-souffle, propagation",
    )
    write(
        HERE / "shared-query.souffle.json",
        [rs_gen_to_souffle(g) for g in query_rs],
        "ctadl-souffle, sources+sinks",
    )


if __name__ == "__main__":
    main()
