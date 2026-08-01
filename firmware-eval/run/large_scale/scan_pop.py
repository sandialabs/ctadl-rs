#!/usr/bin/env python3
"""Reproduce Mango FirmwareFinder selection over the large_dataset and count population.

Population per firmware = ELF, NOT shared object (ET_DYN excluded, approx of `file`),
not symlink, not busybox, deduped by sha256 within firmware.
We also compute the global unique-sha set (what CTADL actually has to *run*).
"""
import os, sys, hashlib, json
from pathlib import Path
from collections import defaultdict

ROOT = Path("/Users/dbueno/proj/operation-mango-public/firmware/large_dataset")
BANNED = {"busybox"}

# cmdi sink symbol names (for has_sinks gate, checked as a byte substring in the
# binary's symbol/string table — cheap proxy for Mango's import check)
SINKS = [b"system", b"twsystem", b"execFormatCmd", b"exec_cmd", b"___system",
    b"bstar_system", b"doSystemCmd", b"doShell", b"CsteSystem", b"cgi_deal_popen",
    b"ExeCmd", b"ExecShell", b"exec_shell_popen", b"popen", b"execl", b"execlp",
    b"execle", b"execv", b"execvp", b"execvpe", b"execve", b"tp_systemEx",
    b"exec_shell_async", b"exec_shell_sync", b"SLIBCSystem", b"SLIBCExecl",
    b"SLIBCExec", b"SLIBCExecv", b"SLIBCPopen", b"pegaSystem"]

import stat as _stat
def elf_kind(p):
    """Return ('exec'|'dyn'|None, is_elf). Reads header only. Regular files only."""
    try:
        st = os.lstat(p)
        if not _stat.S_ISREG(st.st_mode):
            return None  # skip fifo/device/socket/symlink -> would block on open
        if st.st_size < 20:
            return None
        # open non-blocking to be extra safe against fifos slipping through
        fd = os.open(p, os.O_RDONLY | os.O_NONBLOCK)
        try:
            hdr = os.read(fd, 20)
        finally:
            os.close(fd)
    except OSError:
        return None
    if len(hdr) < 20 or hdr[:4] != b"\x7fELF":
        return None
    ei_data = hdr[5]  # 1=LE, 2=BE
    if ei_data == 1:
        e_type = hdr[16] | (hdr[17] << 8)
    else:
        e_type = (hdr[16] << 8) | hdr[17]
    if e_type == 2:
        return "exec"
    if e_type == 3:
        return "dyn"
    return "other"  # ET_REL / ET_CORE

def has_sink(p):
    try:
        with open(p, "rb") as f:
            data = f.read()
    except OSError:
        return False
    return any(s in data for s in SINKS)

def main():
    vendors = sorted([d for d in ROOT.iterdir() if d.is_dir()])
    # per firmware-image: keyed (vendor, firmware_dir_name) -> {sha: path}
    per_firm = defaultdict(dict)
    global_sha = {}
    stats = defaultdict(lambda: defaultdict(int))
    n_scanned = 0
    for vendor in vendors:
        # each firmware image is a direct subdir of the vendor
        firms = sorted([d for d in vendor.iterdir() if d.is_dir()])
        for firm in firms:
            key = (vendor.name, firm.name)
            for dirpath, dirnames, filenames in os.walk(firm):
                # prune virtual/device dirs (blocking special files) and carvings
                dirnames[:] = [d for d in dirnames
                               if d not in ("proc", "sys", "dev")
                               and not d.endswith(".extracted")]
                # skip nested binwalk carvings to match a clean filesystem
                if ".extracted" in dirpath:
                    continue
                for fn in filenames:
                    n_scanned += 1
                    if n_scanned % 100000 == 0:
                        sys.stderr.write(f"  scanned {n_scanned} files...\n"); sys.stderr.flush()
                    p = os.path.join(dirpath, fn)
                    if os.path.islink(p):
                        continue
                    if fn in BANNED:
                        continue
                    kind = elf_kind(p)
                    if kind is None or kind == "other":
                        continue
                    stats[vendor.name]["elf_total"] += 1
                    if kind == "dyn":
                        stats[vendor.name]["shared_obj"] += 1
                        continue  # excluded like `file`->shared object (approx)
                    # ET_EXEC executable
                    stats[vendor.name]["exec"] += 1
                    try:
                        with open(p, "rb") as f:
                            sha = hashlib.file_digest(f, "sha256").hexdigest()
                    except OSError:
                        continue
                    per_firm[key][sha] = p
                    if sha not in global_sha:
                        global_sha[sha] = p
    # sink gate on the global unique set
    sink_shas = {}
    for sha, p in global_sha.items():
        if has_sink(p):
            sink_shas[sha] = p
    # per-vendor rollup
    out = {"vendors": {}, "totals": {}}
    total_images = 0; total_binbins = 0; total_uniq = 0
    vendor_uniq = defaultdict(set); vendor_uniq_sink = defaultdict(set)
    vendor_images = defaultdict(int); vendor_binbins = 0
    v_binbins = defaultdict(int); v_binbins_sink = defaultdict(int)
    for (vname, fname), shas in per_firm.items():
        vendor_images[vname] += 1
        for sha in shas:
            vendor_uniq[vname].add(sha)
            v_binbins[vname] += 1
            if sha in sink_shas:
                vendor_uniq_sink[vname].add(sha)
                v_binbins_sink[vname] += 1
    for vname in sorted(vendor_images):
        out["vendors"][vname] = {
            "images": vendor_images[vname],
            "firmxbin_exec": v_binbins[vname],
            "firmxbin_exec_sink": v_binbins_sink[vname],
            "uniq_exec": len(vendor_uniq[vname]),
            "uniq_exec_sink": len(vendor_uniq_sink[vname]),
            "shared_obj_skipped": stats[vname]["shared_obj"],
        }
    out["totals"] = {
        "images": sum(vendor_images.values()),
        "firmxbin_exec": sum(v_binbins.values()),
        "firmxbin_exec_sink": sum(v_binbins_sink.values()),
        "uniq_exec_global": len(global_sha),
        "uniq_exec_sink_global": len(sink_shas),
        "files_scanned": n_scanned,
    }
    outp = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("pop.json")
    outp.write_text(json.dumps(out, indent=2))
    # also dump the runnable worklist: unique sink binaries with a representative path
    wl = [{"sha256": sha, "binary": p} for sha, p in sink_shas.items()]
    Path(str(outp) + ".worklist.json").write_text(json.dumps(wl, indent=2))
    # attribution: for each sink sha, every (vendor, firmware) image that ships it
    attrib = {}
    for (vname, fname), shas in per_firm.items():
        for sha in shas:
            if sha in sink_shas:
                attrib.setdefault(sha, []).append([vname, fname])
    Path(str(outp) + ".attrib.json").write_text(json.dumps(attrib))
    print(json.dumps(out["totals"], indent=2))
    print("\nPer vendor:")
    for v, d in out["vendors"].items():
        print(f"  {v:10s} images={d['images']:4d}  uniq_exec={d['uniq_exec']:6d}  uniq_sink={d['uniq_exec_sink']:6d}  firmxbin_sink={d['firmxbin_exec_sink']:6d}")

if __name__ == "__main__":
    main()
