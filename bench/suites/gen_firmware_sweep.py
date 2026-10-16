"""Freeze the firmware sweep population: every sink-bearing executable in the Operation Mango
dataset -- the large_dataset worklist (Mango's FirmwareFinder selection, from scan_pop.py)
plus the same selection over the seven extracted devices in 7_firmware -- deduplicated by
content hash.

    python3 gen_firmware_sweep.py <operation-mango-public dir> > firmware-sweep.json
"""

import hashlib
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "firmware-eval" / "run" / "large_scale"))
sys.path.insert(0, str(HERE))
from scan_pop import BANNED, elf_kind, has_sink  # noqa: E402

OLD_ROOT = "/Users/dbueno/proj/operation-mango-public"


def seven_rootfs(root: Path):
    # gen_firmware7 runs its main() on import, so read its table without importing it.
    src = (HERE / "gen_firmware7.py").read_text()
    table = src[src.index("DEVICES = {"):src.index("}\n", src.index("DEVICES = {")) + 1]
    ns: dict = {}
    exec(table, ns)
    import glob
    for device, (dev_dir, rootfs_glob) in ns["DEVICES"].items():
        (rootfs,) = glob.glob(str(root / "firmware" / "7_firmware" / dev_dir / rootfs_glob))
        yield device, Path(rootfs)


def main() -> None:
    root = Path(sys.argv[1])
    out: dict[str, dict] = {}
    worklist = json.loads((REPO / "firmware-eval/run/large_scale/pop.json.worklist.json").read_text())
    for w in worklist:
        path = w["binary"].replace(OLD_ROOT, str(root), 1)
        out[w["sha256"]] = {"sha256": w["sha256"], "path": path, "origin": "large_dataset"}
    added = 0
    for device, rootfs in seven_rootfs(root):
        for dirpath, dirnames, filenames in os.walk(rootfs):
            dirnames[:] = [d for d in dirnames if d not in ("proc", "sys", "dev")]
            for fn in filenames:
                p = os.path.join(dirpath, fn)
                if os.path.islink(p) or fn in BANNED or elf_kind(p) != "exec" or not has_sink(p):
                    continue
                with open(p, "rb") as f:
                    sha = hashlib.file_digest(f, "sha256").hexdigest()
                if sha not in out:
                    out[sha] = {"sha256": sha, "path": p, "origin": f"7_firmware/{device}"}
                    added += 1
    missing = sum(1 for e in out.values() if not os.path.exists(e["path"]))
    print(f"{len(out)} binaries ({added} from 7_firmware); {missing} paths missing",
          file=sys.stderr)
    json.dump(sorted(out.values(), key=lambda e: e["sha256"]), sys.stdout, indent=0)
    print()


main()
