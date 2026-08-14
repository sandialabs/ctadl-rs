#!/usr/bin/env python3
"""Pick the 50-binary corpus for the head-to-head, reproducibly - DO-NOT-MERGE.

    python3 select_corpus.py [-o corpus.json]

Selection is mechanical so that "why these 50?" has an answer that is a program
rather than a taste. Four filters and one sample, applied blind to either
engine's output:

  1. ELF executables under each of the 7 device roots in
     operation-mango-public/firmware/7_firmware. Shared objects are dropped
     (`lib*`, `*.so*`): the experiment analyzes programs, and a library has no
     `main`.
  2. Deduplicated by content hash. The same busybox appears under a dozen
     names and in more than one device root; analyzing it a dozen times would
     weight the corpus by packaging accident. Within a device, one binary per
     *name* as well.
  3. Size in [8 K, 512 K]. The floor drops stubs with nothing to analyze. The
     ceiling is set by ctadl-souffle, not by ctadl-rs - Souffle indexes the
     whole program eagerly, and the megabyte-class `httpd`/`fbwifi` binaries in
     this corpus are out of its reach in any reasonable budget. Both engines
     get the same ceiling.
  4. Contains at least one command-execution sink symbol AND at least one taint
     source symbol from the shared query models (matched as NUL-delimited
     strings, i.e. `.dynstr`/`.strtab` entries). A binary with no `system`-like
     callee cannot have a command-injection path in *either* engine, so it
     measures nothing but Ghidra. This is the one filter that touches the
     research question, and it is applied identically to both engines and
     before either is run.

Then: per device, sort the survivors by size, cut into `quota` equal-count
bins, and take from each bin the binary with the best attack-surface tier
(web/CGI > daemon > utility), ties broken alphabetically. That gives spread in
size and in role within every device instead of 50 near-identical stubs.

The 5 binaries from the original 5-binary run are pinned in, so the expanded
numbers are a superset of the ones already reported.
"""

import argparse
import collections
import hashlib
import json
import re
from pathlib import Path

MANGO = Path("/Users/dbueno/proj/operation-mango-public/firmware/7_firmware")

# device root -> (vendor, device, arch)
DEVICES = {
    "D-Link/D-Link 878": ("D-Link", "DIR-878", "MIPS"),
    "NetGear/R7000": ("Netgear", "R7000", "ARM"),
    "NetGear/RV6400_v2": ("Netgear", "R6400v2", "ARM"),
    "NetGear/XR300": ("Netgear", "XR300", "ARM"),
    "Tenda/AC15": ("Tenda", "AC15", "ARM"),
    "Tenda/AC18": ("Tenda", "AC18", "ARM"),
    "Tenda/W20E": ("Tenda", "W20E", "ARM"),
}

MIN_BYTES, MAX_BYTES = 8 * 1024, 512 * 1024
TOTAL = 50

# Sink and source names, taken from models/shared-query.*.json. Kept as a flat
# list rather than parsed out of the model file because the model file spells
# them as one alternation regex per generator.
SINKS = """system twsystem execFormatCmd exec_cmd ___system bstar_system doSystemCmd doShell
CsteSystem cgi_deal_popen ExeCmd ExecShell exec_shell_popen exec_shell_popen_str popen execl
execlp execle execv execvp execvpe execve tp_systemEx exec_shell_async exec_shell_sync
exec_shell_sync2 SLIBCSystem SLIBCExec SLIBCExecv SLIBCPopen SLIBCExecl pegaSystem""".split()

SOURCES = """nvram_get nvram_safe_get bcm_nvram_get envram_get wlcsm_nvram_get dni_nvram_get
PTI_nvram_get acosNvramConfig_get acosNvramConfig_read GetValue getenv recv recvfrom
custom_param_parser read fread fgets main""".split()

# Attack-surface tiers. Lower is better. Tier 0 is where router command
# injection is actually reported: the web stack, the CGI handlers, the
# config/NVRAM daemons, the vendor service brokers.
TIER_RE = [
    re.compile(
        r"cgi|http|^web$|goahead|lighttpd|nginx|soap|upnp|netctrl|nvram|acos_service"
        r"|^rc$|business_proc|app_data_center|^remote$|xagent|funjsq|easyroaming|onetouch"
        r"|ucloud|dap_|haserl|portal|dxml",
        re.I,
    ),
    re.compile(
        r"dhcp|dns|telnet|ftp|smb|dlna|wps|l2tp|pptp|ppp|igmp|zebra|ripd|udev|dbus"
        r"|multiWAN|cfmd|gnway|phddns|inadyn|dropbear|sntp|ntp|afpd|cnid|daapd|klips"
        r"|starter|spi|omcproxy|stad|ated|ralink|timer|time_c|mkconfig|nl_server|protest"
        r"|fota|seama|zeroconf|hotplug|wan|net|heartbeat|email|check|monitor|detect|logd"
        r"|logserver|daemon|d$",
        re.I,
    ),
]

# The 5 from the original run, pinned so the expanded corpus is a superset.
PINNED = {
    "arp_check": "R7000",
    "nvram_daemon": "DIR-878",
    "rc": "R7000",
    "acos_service": "R6400v2",
    "netctrl": "AC15",
}


def tier(name):
    for i, rx in enumerate(TIER_RE):
        if rx.search(name):
            return i
    return len(TIER_RE)


def label_for(device, name):
    dev = device.lower().replace("-", "").replace("v2", "").replace("_", "")
    nm = re.sub(r"[^0-9a-zA-Z]+", "_", name).strip("_")
    return f"{dev}_{nm}"


def scan(path: Path):
    """Sink and source symbol names present as NUL-delimited strings."""
    blob = path.read_bytes()
    hit = set()
    for n in SINKS + SOURCES:
        if b"\x00" + n.encode() + b"\x00" in blob:
            hit.add(n)
    return sorted(hit & set(SINKS)), sorted(hit & set(SOURCES))


def candidates():
    by_sha, out = {}, []
    for rel, (vendor, device, arch) in DEVICES.items():
        for p in sorted((MANGO / rel).rglob("*")):
            if p.is_symlink() or not p.is_file():
                continue
            name = p.name
            if name.startswith("lib") or name.endswith(".so") or ".so." in name:
                continue
            try:
                size = p.stat().st_size
                if not (MIN_BYTES <= size <= MAX_BYTES):
                    continue
                with open(p, "rb") as f:
                    if f.read(4) != b"\x7fELF":
                        continue
                sha = hashlib.sha256(p.read_bytes()).hexdigest()[:16]
            except OSError:
                continue
            if sha in by_sha:  # same bytes, already have it
                continue
            by_sha[sha] = True
            sinks, sources = scan(p)
            if not sinks or not sources:
                continue
            out.append(
                {
                    "label": label_for(device, name),
                    "vendor": vendor,
                    "device": device,
                    "arch": arch,
                    "name": name,
                    "size": size,
                    "sha256_16": sha,
                    "tier": tier(name),
                    "sinks": sinks,
                    "sources": sources,
                    "binary": str(p),
                }
            )
    # one per (device, name): keep the shallowest path, then the largest
    best = {}
    for c in out:
        k = (c["device"], c["name"])
        cur = best.get(k)
        if cur is None or (c["binary"].count("/"), -c["size"]) < (
            cur["binary"].count("/"),
            -cur["size"],
        ):
            best[k] = c
    return sorted(best.values(), key=lambda c: (c["device"], c["size"], c["name"]))


def quotas(pool):
    """Split TOTAL across devices, proportional to pool size, >=1 each."""
    devs = sorted(pool, key=lambda d: (-len(pool[d]), d))
    n = sum(len(v) for v in pool.values())
    q = {d: min(len(pool[d]), max(1, round(TOTAL * len(pool[d]) / n))) for d in devs}
    while sum(q.values()) != TOTAL:
        step = 1 if sum(q.values()) < TOTAL else -1
        moved = False
        for d in devs if step > 0 else devs[::-1]:
            if 1 <= q[d] + step <= len(pool[d]):
                q[d] += step
                moved = True
                break
        if not moved:
            break
    return q


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-o", "--out", default=str(Path(__file__).parent / "corpus.json"))
    args = ap.parse_args()

    cands = candidates()
    pool = collections.defaultdict(list)
    for c in cands:
        pool[c["device"]].append(c)
    q = quotas(pool)

    picked = []
    for device in sorted(pool):
        items = sorted(pool[device], key=lambda c: (c["size"], c["name"]))
        k = q[device]
        # the pinned originals come first and count against the quota
        chosen = {c["label"]: c for c in items if PINNED.get(c["name"]) == device}
        # then size-stratified: k equal-count bins, best attack-surface tier in each
        for i in range(k):
            if len(chosen) >= k:
                break
            lo, hi = i * len(items) // k, (i + 1) * len(items) // k
            binned = [c for c in items[lo:hi] if c["label"] not in chosen]
            if not binned:
                continue
            pick = min(binned, key=lambda c: (c["tier"], c["name"]))
            chosen[pick["label"]] = pick
        # a bin that was already spoken for leaves the device short; backfill
        for c in items:
            if len(chosen) >= k:
                break
            chosen.setdefault(c["label"], c)
        picked.extend(sorted(chosen.values(), key=lambda c: c["size"]))

    picked.sort(key=lambda c: (c["vendor"], c["device"], c["size"]))
    doc = {
        "_comment": [
            f"The {len(picked)} firmware binaries for the ctadl-rs vs ctadl-souffle "
            "head-to-head - DO-NOT-MERGE.",
            "GENERATED by select_corpus.py; edit that, not this. Its docstring is the "
            "selection rule: ELF executables from the 7-device SaTC corpus behind "
            "Operation Mango's paper, deduplicated by content, 8K..512K, required to "
            "contain both a command-execution sink symbol and a taint source symbol, "
            "then sampled per device across size bins preferring the web/CGI/config-"
            "daemon attack surface.",
            "The 5 binaries of the original 5-binary run are pinned in, so these "
            "numbers are a superset of the ones already reported.",
        ],
        "selection": {
            "candidates_after_filters": len(cands),
            "per_device_quota": q,
            "min_bytes": MIN_BYTES,
            "max_bytes": MAX_BYTES,
        },
        "corpus": [
            {
                k: c[k]
                for k in (
                    "label", "vendor", "device", "arch", "name", "size",
                    "sha256_16", "tier", "sinks", "sources", "binary",
                )
            }
            for c in picked
        ],
    }
    Path(args.out).write_text(json.dumps(doc, indent=2) + "\n")
    print(f"{len(cands)} candidates -> {len(picked)} picked; quotas {q}")
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
