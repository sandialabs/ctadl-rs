"""Freeze the firmware-7 benchmark list: the network-facing binaries of the seven Operation
Mango devices, from the dataset's FIRMWARE_MANIFEST.md, resolved to files and hashed.

Run once (and again only on purpose); the daemon reads the JSON it writes, so the benchmark set
does not drift when the dataset directory changes. A binary whose hash no longer matches is
reported by the daemon rather than silently measured.

    python3 gen_firmware7.py <7_firmware dir> > firmware7.json
"""

import glob
import hashlib
import json
import re
import sys
from pathlib import Path

CATEGORIES = {"Web server", "Web / CGI", "UPnP / WPS"}

# Manifest section heading -> (device directory, rootfs glob under it). The Tenda rootfs paths
# contain a literal \x02 byte, which the `*` absorbs.
DEVICES = {
    "D-Link DIR-878": ("D-Link/D-Link 878",
                       "binwalk_extracted/*/A0/decompressed.bin.extracted/8957DC/decompressed.bin.extracted/0"),
    "NetGear R7000": ("NetGear/R7000", "binwalk_extracted/*/3A/partition_1.bin.extracted/0/squashfs-root"),
    "NetGear R6400v2": ("NetGear/RV6400_v2", "binwalk_extracted/*/3A/partition_1.bin.extracted/0/squashfs-root"),
    "NetGear XR300": ("NetGear/XR300", "binwalk_extracted/*/3A/partition_1.bin.extracted/0/squashfs-root"),
    "Tenda AC15": ("Tenda/AC15", "binwalk_extracted/*/0/*.bin.extracted/0/partition_1.bin.extracted/0/squashfs-root"),
    "Tenda AC18": ("Tenda/AC18", "binwalk_extracted/*/0/*.bin.extracted/0/partition_1.bin.extracted/0/squashfs-root"),
    "Tenda W20E": ("Tenda/W20E", "binwalk_extracted/*/0/*.bin.extracted/0/partition_1.bin.extracted/0/squashfs-root"),
}


def main() -> None:
    root = Path(sys.argv[1])
    manifest = (root / "FIRMWARE_MANIFEST.md").read_text()
    out, seen = [], set()
    device = None
    for line in manifest.splitlines():
        if line.startswith("## "):
            device = next((k for k in DEVICES if line[3:].startswith(k)), None)
            continue
        if device is None or not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) < 5 or cells[0] not in CATEGORIES:
            continue
        category, binary, _size, arch = cells[0], cells[1].strip("`"), cells[2], cells[3]
        dev_dir, rootfs_glob = DEVICES[device]
        rootfs = glob.glob(str(root / dev_dir / rootfs_glob))
        if len(rootfs) != 1:
            sys.exit(f"{device}: rootfs glob matched {rootfs}")
        path = Path(rootfs[0] + binary).resolve()
        sha = hashlib.sha256(path.read_bytes()).hexdigest()
        if sha in seen:  # the same binary under a second name (prog.cgi / prog-cgi)
            continue
        seen.add(sha)
        slug = re.sub(r"[^A-Za-z0-9]+", "-", f"{dev_dir.split('/')[1]}-{binary}").strip("-")
        out.append({"bench": slug, "device": device, "category": category, "binary": binary,
                    "arch": arch, "path": str(path), "sha256": sha})
    json.dump(out, sys.stdout, indent=1)
    print()


main()
